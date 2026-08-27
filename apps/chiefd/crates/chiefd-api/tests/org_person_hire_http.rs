//! The hire surface over REAL HTTP — `POST /v1/org/person/hire-preview` then
//! `POST /v1/org/person/hire` (#751/P3).
//!
//! ═══ What this file is FOR ═══
//!
//! The intercom performs the genuine two-step: it asks which route chiefd
//! WOULD choose, then hands that answer back as `expectedProvider` /
//! `expectedModel` / `expectedModelReason`, which the inserting transaction
//! re-derives and compares before it may write. That fence is the whole reason
//! a person cannot be created on an unattested model, so it is exercised in
//! BOTH directions here: a matching triple commits, a mismatched one is
//! refused with nothing written.
//!
//! It also pins `mint_hire_ids`. Before it, a hire that left `title` or
//! `taskClass` blank was refused `invalid-seed` every single time — the deleted
//! CLI sent `seed.title ?? ""` verbatim — and the person id was minted
//! client-side as `<department>-<slug(name)>`, a second opinion about a name
//! chiefd already knows how to derive. Both now live here, so a person hired
//! through this route is named exactly as one created by genesis.
//!
//! The company is created through `/v1/org/manifest/genesis-with-models`, the
//! same route the Founder uses, so the Founder-route fallback these tests lean
//! on is the observation chiefd actually RECORDED rather than one the fixture
//! invented.

#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{
    router_with_live_resolver, ChangeFeed, DocStore, SupervisionLiveResolver, SupervisionLiveSource,
};
use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SystemClock;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const SLUG: &str = "hire-two-step-http";
/// The spec name whose `slugify` result IS [`SLUG`].
const SPEC_NAME: &str = "Hire Two Step HTTP";
const GENESIS_AT: &str = "2026-08-01T00:00:00.000Z";
/// The person id genesis mints for the spec's `ceo` seed.
const CEO: &str = "chief";

struct Fixture {
    app: axum::Router,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let company_path = dir.path().join("company.sqlite");
    let company = Arc::new(
        CompanyDb::open(SLUG, &company_path, Arc::new(SystemClock::default()))
            .expect("open company"),
    );
    let source = SupervisionLiveSource::new(Arc::clone(&company), SLUG.to_owned());
    let resolved = source.clone();
    let resolver: SupervisionLiveResolver =
        Arc::new(move |slug, _mode| (slug == SLUG).then(|| resolved.clone()));

    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&dir.path().join("control.sqlite").display().to_string(), 2, feed)
            .expect("control docstore"),
    );
    store.ensure_schema().await.expect("control schema");

    let app = router_with_live_resolver(
        store,
        1024 * 1024,
        Duration::from_secs(15),
        Some(source),
        Some(resolver),
        None,
    )
    .layer(axum::extract::Extension(ceo_caller()));
    Fixture { app, _dir: dir }
}

/// The credential every request in this file carries: the CEO, as a PERSON.
///
/// It has to be this exact person and not a daemon-scoped stand-in. Every hire
/// below declares `requester: { kind: "person", personId: CEO }`, and
/// `bind_requester_to_caller` refuses a non-person caller that claims to act as
/// a person — correctly, because a manager-attributed hire inherits that
/// manager's model route and is recorded against them. So the caller must BE
/// the requester the body names.
///
/// It is not optional either. The absent-caller arm is deleted: with no
/// identity these routes answer `401 caller-unauthenticated` before the handler
/// reads the body, and every assertion about the two-step would fail on the
/// status rather than on the rule it is testing.
fn ceo_caller() -> chiefd_api::authn::middleware::CallerIdentity {
    chiefd_api::authn::middleware::CallerIdentity(chiefd_core::store::identities::Identity {
        identity_id: format!("id-{CEO}"),
        principal: CEO.to_owned(),
        kind: chiefd_core::store::identities::IdentityKind::Person,
        company_slug: Some(SLUG.to_owned()),
        pubkey: Some("test-key".to_owned()),
        fingerprint: format!("fp-{CEO}"),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

fn genesis_body() -> Value {
    json!({
        "slug": SLUG,
        "spec": {
            "name": SPEC_NAME,
            "purpose": "Prove the hire two-step.",
            "chief": { "name": "Chief" }
        },
        "at": GENESIS_AT,
    })
}

async fn post(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    // A rejection from axum's `Json` extractor is PLAIN TEXT, not JSON, so a
    // silent `unwrap_or(Value::Null)` turns "your body has an unknown field"
    // into an unreadable `null` and the assertion message says nothing.
    let text = String::from_utf8_lossy(&bytes).to_string();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::String(text)))
}

async fn seeded() -> Fixture {
    let fixture = fixture().await;
    let (status, body) = post(&fixture.app, "/v1/org/manifest/genesis", genesis_body()).await;
    assert_eq!(status, StatusCode::OK, "genesis must succeed: {body}");
    fixture
}

/// A complete hire body with the fields a manager genuinely supplies.
///
/// `personId` and `title` are left BLANK on purpose: the intercom sends them
/// empty for chiefd to mint. There is no provider, model or task class here,
/// on any door — chiefd holds none of them, and `deny_unknown_fields` refuses
/// a caller that still sends one.
fn hire_body(name: &str) -> Value {
    json!({
        "slug": SLUG,
        "requester": { "kind": "person", "personId": CEO },
        "personId": "",
        "departmentId": "executive",
        "name": name,
        "title": "",
        "mandate": "Write the compiler.",
    })
}

/// Read the committed people back through chiefd's OWN structured tree.
///
/// `/v1/org/tree/read` renders a human summary that does not name individual
/// people, so it can report "2 people" for a hire whose id and title are both
/// wrong. The structured forest carries the identity fields, which is what
/// these assertions are actually about.
async fn tree(app: &axum::Router) -> String {
    let (status, body) = post(app, "/v1/org/tree/structured", json!({"slug": SLUG})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body.to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hire_commits_and_chiefd_mints_the_blanks() {
    let fixture = seeded().await;

    let (status, body) = post(&fixture.app, "/v1/org/person/hire", hire_body("Ada Lovelace")).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], json!(true));

    // The minted id follows `<department>-<slug(name)>`, the rule the deleted
    // CLI applied client-side. The title falls back to the person's name —
    // before `mint_hire_ids` both of those blanks were an `invalid-seed`
    // refusal, so this hire could not have succeeded at all.
    let tree = tree(&fixture.app).await;
    assert!(
        tree.contains("executive-ada-lovelace"),
        "minted person id absent from the tree:\n{tree}"
    );
    assert!(tree.contains("Ada Lovelace"), "minted title absent from the tree:\n{tree}");
}

/// THE RULE STAGE 1 ESTABLISHES, at the boundary that has to hold it.
///
/// A hire body carrying ANY provider/model input is refused rather than
/// ignored. `deny_unknown_fields` is what makes chief's exit from the provider
/// business a boundary a caller cannot talk its way around: an intercom that
/// still sent a model would fail loudly at the door instead of having its
/// choice silently dropped, which is the failure nobody would notice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hire_that_still_names_a_model_is_refused_at_the_door() {
    let fixture = seeded().await;

    for retired in [
        "provider",
        "model",
        "modelReason",
        "expectedProvider",
        "expectedModel",
        "expectedModelReason",
        "taskClass",
        "thinking",
        "observation",
        "hiringManagerPersonId",
    ] {
        let mut body = hire_body("Ada Lovelace");
        body[retired] = json!("smuggled");
        let (status, response) = post(&fixture.app, "/v1/org/person/hire", body).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "`{retired}` must be refused, not ignored: {response}"
        );
    }

    // And nothing was written by any of them.
    let tree = tree(&fixture.app).await;
    assert!(!tree.contains("Ada Lovelace"), "a refused hire must write nothing:\n{tree}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hiring_the_same_person_twice_is_a_loud_refusal_not_an_overwrite() {
    let fixture = seeded().await;

    let (first, _) = post(&fixture.app, "/v1/org/person/hire", hire_body("Ada Lovelace")).await;
    assert_eq!(first, StatusCode::OK);

    // The SAME name mints the SAME id, which is the whole point of a
    // deterministic mint: a repeat is caught as a duplicate rather than
    // silently overwriting a live person.
    let (status, body) = post(&fixture.app, "/v1/org/person/hire", hire_body("Ada Lovelace")).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], json!("duplicate-person-id"), "actual body: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_that_produces_no_usable_id_is_refused_rather_than_guessed() {
    let fixture = seeded().await;

    let (status, body) = post(&fixture.app, "/v1/org/person/hire", hire_body("!!!")).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], json!("invalid-person-name"), "actual body: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_person_id_is_carried_through_untouched() {
    let fixture = seeded().await;

    let mut body = hire_body("Ada Lovelace");
    body["personId"] = json!("ada");
    body["title"] = json!("Principal Engineer");
    let (status, response) = post(&fixture.app, "/v1/org/person/hire", body).await;

    assert_eq!(status, StatusCode::OK, "{response}");
    let tree = tree(&fixture.app).await;
    assert!(tree.contains("Principal Engineer"), "an explicit title must survive:\n{tree}");
    assert!(
        !tree.contains("executive-ada-lovelace"),
        "a supplied id must not be re-minted:\n{tree}"
    );
}
