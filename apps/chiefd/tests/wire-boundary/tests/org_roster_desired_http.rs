//! Live-route tests for `POST /v1/org/roster/desired` — the client-agnostic
//! roster facts (#751/P4).
//!
//! # The assertion this file exists for
//!
//! [`runtime_shaped_fields`] fails if the response body ever gains a runtime-shaped
//! field. The repo's backend-runtime boundary guard scans **file text** and cargo
//! dependency edges; by its author's own statement it cannot inspect a wire
//! shape, so a route could satisfy that guard completely while serving
//! `{"session": "org-cobalt"}`. This is the assertion that catches it, and
//! [`the_no_runtime_field_assertion_catches_a_planted_field`] proves the scanner
//! is not vacuous by planting one.
//!
//! The ban list is deliberately wider here than the file-text guard's `runtime`:
//! this response carries no Pi conversation session, no LLM context window and
//! no `.windows(2)` — the false-positive cases that make `session`/`window`
//! useless as a repo-wide grep — so banning them outright costs nothing and
//! catches the fields a operator client would most like to be handed.

#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{
    router_with_supervision_live, ChangeFeed, DocStore, SupervisionLiveSource,
};
use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SystemClock;
use chiefd_core::store::activity::ActivityLedger;
use chiefd_core::store::organization::{OrganizationManifest, PersonRecord};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const LABEL: &str = "cobalt";
const SLUG: &str = "cobalt@roster";
const PATH: &str = "/v1/org/roster/desired";
/// The root department id every company uses.
const ROOT: &str = chiefd_core::store::organization::ROOT_DEPARTMENT_ID;

// --- the fixture: a nested department with its own head --------------------

/// Northstar plus `quant-alpha` under `quant`, with its own head and worker.
///
/// Depth is what discriminates a placement rule: `alpha-head` must land in
/// `quant-alpha`'s window, the unit they head, and a company whose every
/// department hangs off the root cannot tell that from "every head sits at the
/// root" — which is what the retired head-in-parent rule did.
fn nested_manifest() -> OrganizationManifest {
    let mut manifest = chiefd_core::test_support::northstar_manifest(0);
    manifest.slug = LABEL.to_owned();

    let mut alpha = manifest.departments["quant"].clone();
    alpha.id = "quant-alpha".to_owned();
    alpha.name = "Alpha".to_owned();
    alpha.parent_department_id = Some("quant".to_owned());
    alpha.head_person_id = "alpha-head".to_owned();
    manifest.department_order.push("quant-alpha".to_owned());
    manifest.departments.insert("quant-alpha".to_owned(), alpha);

    for (id, name, title, template) in [
        ("alpha-head", "Alex", "Head of Alpha", "quant-head"),
        ("alpha-worker", "Ada", "Alpha Researcher", "signal-researcher"),
    ] {
        let mut person: PersonRecord = manifest.people[template].clone();
        person.id = id.to_owned();
        person.name = name.to_owned();
        person.title = title.to_owned();
        person.department_id = "quant-alpha".to_owned();
        manifest.people_order.push(id.to_owned());
        manifest.people.insert(id.to_owned(), person);
    }
    manifest
}

struct Harness {
    app: axum::Router,
    company: Arc<CompanyDb>,
    manifest: OrganizationManifest,
    // Keep last: the router and CompanyDb release their SQLite handles before
    // the fixture directory removes the backing files.
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    harness_with(nested_manifest()).await
}

async fn harness_with(manifest: OrganizationManifest) -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    {
        let mut connection = chiefd_core::store::open_company_db(&path).expect("open seed db");
        let transaction = connection.transaction().expect("seed transaction");
        chiefd_core::store::organization_rows::genesis(&transaction, LABEL, &manifest)
            .expect("seed manifest");
        transaction.commit().expect("commit seed manifest");
    }
    let company = Arc::new(
        CompanyDb::open(LABEL, &path, Arc::new(SystemClock::default())).expect("open company db"),
    );
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&path.display().to_string(), 2, Arc::clone(&feed))
            .expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let live = SupervisionLiveSource::new(Arc::clone(&company), SLUG.to_owned());
    let app =
        router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live))
            .layer(axum::extract::Extension(actuator_caller()));
    Harness { app, company, manifest, _dir: dir }
}

/// The credential every request in this file carries.
///
/// A SERVICE identity, daemon-scoped (`company_slug: None`), because that is
/// literally this route's caller: `/v1/org/roster/desired` is one of the
/// resident actuator's own calls (`chief-cli/src/actuate/client.rs`), and the
/// actuator authenticates as a service. A person credential would narrow the
/// roster to that person's subtree, which is correct behaviour and completely
/// wrong for this file — these tests walk the WHOLE response to prove no field
/// on it is tmux-shaped, so a narrowed roster would hide fields rather than
/// prove their absence. The narrowing itself is pinned by
/// `chiefd-api/tests/org_shape_disclosure_fence_http.rs`.
///
/// It is not optional: with the absent-caller arm deleted, a request with no
/// identity is `401 caller-unauthenticated`, and an empty refusal body would
/// satisfy every "carries no runtime-shaped field" assertion here vacuously.
fn actuator_caller() -> chiefd_api::authn::middleware::CallerIdentity {
    chiefd_api::authn::middleware::CallerIdentity(chiefd_core::store::identities::Identity {
        identity_id: "chiefd-actuator".to_owned(),
        principal: "chiefd-actuator".to_owned(),
        kind: chiefd_core::store::identities::IdentityKind::Service,
        company_slug: None,
        pubkey: Some("test-key".to_owned()),
        fingerprint: "fp-chiefd-actuator".to_owned(),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

impl Harness {
    /// Commit an activity ledger whose last reconcile wanted everybody running
    /// except `parked`. `ActivityLedger::initial` seeds `last_desired_active`
    /// FALSE (inv 20) — the "nobody has been launched yet" state — so a test
    /// about the steady state has to say so.
    async fn converge(&self, parked: &[&str]) {
        let mut ledger = ActivityLedger::initial(&self.manifest, "2026-01-01T00:00:00.000Z");
        for person_id in &self.manifest.people_order {
            let state = ledger.people.get_mut(person_id).expect("seeded person state");
            state.last_desired_active = !parked.contains(&person_id.as_str());
        }
        self.company
            .activity_publish(serde_json::to_string(&ledger).expect("serialize ledger"))
            .await
            .expect("publish activity");
    }
}

async fn post(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(PATH)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let payload = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, payload)
}

// --- the no-runtime-field assertion -------------------------------------------

/// Field-name fragments that only an operator client could want.
///
/// The fragment is `tmux`, NOT `runtime`: `tests/wire-boundary` is the one
/// crate the tmux boundary guard does not scan, precisely so it can name the
/// thing it is banning, and a `runtime` fragment would fire on legitimate
/// field names.
const RUNTIME_SHAPED: [&str; 6] = ["tmux", "pane", "window", "session", "socket", "layout"];

/// Every path in `body` whose key names a runtime concept, plus every path whose
/// STRING VALUE is the runtime session name this company would own.
///
/// The value half matters as much as the key half: a session name smuggled in
/// under an innocent key (`"id": "org-cobalt"`) is exactly as much of a
/// placement decision as one called `session`.
fn runtime_shaped_fields(body: &Value, session_name: &str) -> Vec<String> {
    fn walk(value: &Value, path: &str, session_name: &str, found: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let child_path = format!("{path}.{key}");
                    let lowered = key.to_lowercase();
                    if let Some(hit) = RUNTIME_SHAPED.iter().find(|shape| lowered.contains(**shape))
                    {
                        found.push(format!("{child_path} (key names '{hit}')"));
                    }
                    walk(child, &child_path, session_name, found);
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}[{index}]"), session_name, found);
                }
            }
            Value::String(text) if text == session_name => {
                found.push(format!("{path} (value is the runtime session name '{session_name}')"));
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(body, "$", session_name, &mut found);
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_roster_response_carries_no_runtime_shaped_field() {
    let harness = harness().await;
    harness.converge(&[]).await;

    let (status, body) = post(&harness.app, json!({"slug": SLUG})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let found = runtime_shaped_fields(&body, &format!("org-{LABEL}"));
    assert_eq!(
        found,
        Vec::<String>::new(),
        "chiefd must publish facts, not a runtime layout. Offending fields: {found:?}\nbody: {body}"
    );

    // And, named individually, the fields whose ABSENCE is the packet: the
    // stored placement decision above all. A regression that re-added
    // `paneDepartmentId` would ship a display decision as a fact, and a stale
    // one — the column is only rewritten when the activity ledger is.
    let text = body.to_string();
    for banned in ["paneDepartmentId", "pane_department_id", "runtimeSession", "socketName"] {
        assert!(!text.contains(banned), "the roster must not carry {banned}: {body}");
    }
}

#[test]
fn the_no_runtime_field_assertion_catches_a_planted_field() {
    // An assertion that cannot fail is worse than no assertion. Four plants,
    // one per way a runtime fact could arrive.
    let clean = json!({
        "company": {"slug": "cobalt", "displayName": "Cobalt"},
        "rootDepartmentId": ROOT,
        "departments": [{"id": ROOT, "name": "Cobalt", "parentDepartmentId": null,
                         "headPersonId": "chief", "order": 0, "state": "active"}],
        "people": [{"id": "chief", "displayName": "Avery", "title": "Chief",
                    "departmentId": ROOT, "isHeadOf": ROOT, "displayOrder": 0,
                    "desiredActive": true, "employmentState": "active"}]
    });
    assert!(runtime_shaped_fields(&clean, "org-cobalt").is_empty(), "the real shape is clean");

    for (label, plant) in [
        ("a session name", json!({"session": "org-cobalt"})),
        ("a per-person pane", json!({"paneId": "%7"})),
        ("the restored placement column", json!({"paneDepartmentId": ROOT})),
        ("a socket", json!({"socketName": "chiefd"})),
    ] {
        let mut doctored = clean.clone();
        let map = doctored.as_object_mut().expect("object");
        for (key, value) in plant.as_object().expect("plant") {
            map.insert(key.clone(), value.clone());
        }
        assert!(
            !runtime_shaped_fields(&doctored, "org-cobalt").is_empty(),
            "{label} must be caught: {doctored}"
        );
    }

    // The value half: a session name under a key that names nothing.
    let mut smuggled = clean.clone();
    smuggled.as_object_mut().expect("object").insert("id".to_owned(), json!("org-cobalt"));
    assert!(
        !runtime_shaped_fields(&smuggled, "org-cobalt").is_empty(),
        "a session name under an innocent key must be caught"
    );
}

// --- the wire snapshot ------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_roster_wire_shape_is_frozen() {
    let harness = harness().await;
    harness.converge(&["alpha-worker"]).await;

    let (status, body) = post(&harness.app, json!({"slug": SLUG})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(
        body,
        json!({
            "company": {"slug": "cobalt", "displayName": "Northstar Conformance"},
            "rootDepartmentId": ROOT,
            "departments": [
                {"id": ROOT, "name": "Northstar Conformance", "parentDepartmentId": null,
                 "headPersonId": "chief", "order": 0, "state": "active"},
                // Depth-first, as the store reconstructs it: Alpha follows its
                // own parent, not the order it was appended in.
                {"id": "quant", "name": "Quant", "parentDepartmentId": ROOT,
                 "headPersonId": "quant-head", "order": 1, "state": "active"},
                {"id": "quant-alpha", "name": "Alpha", "parentDepartmentId": "quant",
                 "headPersonId": "alpha-head", "order": 2, "state": "active"},
                {"id": "it", "name": "IT", "parentDepartmentId": ROOT,
                 "headPersonId": "it-head", "order": 3, "state": "active"}
            ],
            "people": [
                {"id": "chief", "displayName": "Avery", "title": "Chief", "departmentId": ROOT,
                 "isHeadOf": ROOT, "displayOrder": 0, "desiredActive": true,
                 "employmentState": "active"},
                {"id": "quant-head", "displayName": "Quinn", "title": "Head of Quant",
                 "departmentId": "quant", "isHeadOf": "quant", "displayOrder": 1,
                 "desiredActive": true, "employmentState": "active"},
                {"id": "signal-researcher", "displayName": "Signal Researcher",
                 "title": "Signal Researcher", "departmentId": "quant", "isHeadOf": null,
                 "displayOrder": 2, "desiredActive": true, "employmentState": "active"},
                {"id": "it-head", "displayName": "Ira", "title": "Head of IT",
                 "departmentId": "it", "isHeadOf": "it", "displayOrder": 3,
                 "desiredActive": true, "employmentState": "active"},
                {"id": "alpha-head", "displayName": "Alex", "title": "Head of Alpha",
                 "departmentId": "quant-alpha", "isHeadOf": "quant-alpha", "displayOrder": 4,
                 "desiredActive": true, "employmentState": "active"},
                {"id": "alpha-worker", "displayName": "Ada", "title": "Alpha Researcher",
                 "departmentId": "quant-alpha", "isHeadOf": null, "displayOrder": 5,
                 "desiredActive": false, "employmentState": "active"}
            ]
        }),
        "the roster wire shape changed"
    );
}

// --- P5's derivation, run against the REAL response body -------------------

/// The placement rules P5 ports into `chief-cli`, computed from the response
/// body and nothing else. Returns `(session, [(windowId, windowName,
/// [personId])])`.
fn client_topology(body: &Value) -> (String, Vec<(String, String, Vec<String>)>) {
    let slug = body["company"]["slug"].as_str().expect("slug");
    let departments = body["departments"].as_array().expect("departments");
    let people = body["people"].as_array().expect("people");

    let mut ordered: Vec<&Value> = people.iter().collect();
    ordered.sort_by_key(|person| person["displayOrder"].as_u64().expect("displayOrder"));

    let mut panes: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for person in ordered.into_iter().filter(|p| p["desiredActive"] == json!(true)) {
        // DERIVED, and one rule for everybody: a person's pane is in their own
        // department's window. Heads are not an exception because appointing a
        // head MOVES them into the unit they head — see `chief-cli`'s
        // `placement::pane_department_id`, which used to place them one level up.
        let window = person["departmentId"].as_str().expect("departmentId").to_owned();
        panes.entry(window).or_default().push(person["id"].as_str().expect("id").to_owned());
    }

    let mut units: Vec<&Value> = departments.iter().collect();
    units.sort_by_key(|unit| unit["order"].as_u64().expect("order"));
    let windows = units
        .into_iter()
        .filter_map(|unit| {
            let id = unit["id"].as_str().expect("id").to_owned();
            // The empty-department rule.
            let members = panes.remove(&id)?;
            (!members.is_empty())
                .then(|| (id, unit["name"].as_str().expect("name").to_owned(), members))
        })
        .collect();

    (format!("org-{slug}"), windows)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_derives_the_whole_placement_from_the_response() {
    let harness = harness().await;
    harness.converge(&[]).await;
    let (status, body) = post(&harness.app, json!({"slug": SLUG})).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (session, windows) = client_topology(&body);

    // session = company.
    assert_eq!(session, "org-cobalt");

    // window = department, window name = the department's name; an EMPTY
    // department gets no window — which no department in this fixture is now
    // that every head lives in the unit they head.
    let shape: Vec<(&str, &str, Vec<&str>)> = windows
        .iter()
        .map(|(id, name, people)| {
            (id.as_str(), name.as_str(), people.iter().map(String::as_str).collect())
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            // Every head is in the unit they head, at both depths: the CEO in
            // the root, `quant-head` in Quant, `it-head` in IT, and Alpha's head
            // in Alpha rather than in Quant.
            (ROOT, "Northstar Conformance", vec!["chief"]),
            ("quant", "Quant", vec!["quant-head", "signal-researcher"]),
            ("quant-alpha", "Alpha", vec!["alpha-head", "alpha-worker"]),
            ("it", "IT", vec!["it-head"]),
        ],
        "a head sits in the department they head, nested or top-level"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_department_takes_its_whole_subtree_out_of_the_desired_set() {
    let mut manifest = nested_manifest();
    manifest.departments.get_mut("quant").expect("quant").state =
        chiefd_core::store::organization::UnitState::Paused;
    let harness = harness_with(manifest).await;
    harness.converge(&[]).await;

    let (status, body) = post(&harness.app, json!({"slug": SLUG})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let desired: Vec<&str> = body["people"]
        .as_array()
        .expect("people")
        .iter()
        .filter(|person| person["desiredActive"] == json!(true))
        .map(|person| person["id"].as_str().expect("id"))
        .collect();

    assert_eq!(desired, vec!["chief", "it-head"], "the whole Quant subtree stops");
    // Membership is unchanged: a client still needs to know these people exist
    // to tell its OWN stopped person from a stranger's process.
    assert_eq!(body["people"].as_array().expect("people").len(), 6);
    // And the paused department is still published, with its state.
    let quant = body["departments"]
        .as_array()
        .expect("departments")
        .iter()
        .find(|unit| unit["id"] == json!("quant"))
        .expect("quant is still a department");
    assert_eq!(quant["state"], json!("paused"));
}

// --- refusals ---------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_roster_route_fences_a_foreign_company() {
    let harness = harness().await;
    let (status, body) = post(&harness.app, json!({"slug": "someone-else@other"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], json!("unknown-company"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_roster_route_refuses_a_malformed_or_unmodeled_request() {
    let harness = harness().await;
    let malformed = Request::builder()
        .method("POST")
        .uri(PATH)
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .expect("request");
    let response = harness.app.clone().oneshot(malformed).await.expect("route");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let (status, _) = post(&harness.app, json!({"slug": SLUG, "unexpected": "must refuse"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_company_that_has_never_converged_answers_with_the_no_decision_roster() {
    // No activity document at all. Every person carries no decision, which is
    // the planner's own "no decision" branch — desired subject to the roster
    // and paused-subtree filters, not a separate "everybody" rule.
    let harness = harness().await;
    let (status, body) = post(&harness.app, json!({"slug": SLUG})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["people"]
            .as_array()
            .expect("people")
            .iter()
            .all(|person| person["desiredActive"] == json!(true)),
        "{body}"
    );
    assert!(runtime_shaped_fields(&body, &format!("org-{LABEL}")).is_empty());
}
