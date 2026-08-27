//! B4 singleton-sweep live-seam integration tests (org-data-normalization P0).
//!
//! Drives the REAL `/v1/org/<store>/read` routes over a real `CompanyDb` +
//! `SupervisionLiveSource`, proving the daemon reconstructs each ported store
//! byte-equivalent through chiefd, and that the own-company gate behaves like
//! the manifest reference.
//!
//! # Why most rows are SEEDED in-process rather than published over HTTP
//!
//! The publisher-route sweep deleted the publish half of every store in this
//! family except `runtime`: nothing called them, and the row that matters is
//! written in-process through `CompanyDb` inside the daemon's own
//! transactions. So these tests now write the row the way production writes
//! it and read it back the way production reads it, which is a stronger seam
//! test than the round trip it replaces — the write side is no longer a door
//! only the test used.
//!
//! `runtime` keeps its publish route (`packages/piing`'s tool-contract suite
//! seeds through it), so the macro's publish arm, its wake, and its rejection
//! of a retired `expectedSeq` are all still covered below.

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
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// The company's own directory. A company IS a directory, and its identity is
/// the hash of that path.
const COMPANY_DIR: &str = "/data/orgs/cobalt";
/// The company's DISPLAY slug — what genesis names it, and what the derived
/// `organization` on every document seeded below means. It is NOT the key.
const COMPANY_SLUG: &str = "cobalt";

/// The company KEY: the actor's label, the row scope, and the `slug` every
/// route matches against.
fn company_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new(COMPANY_DIR))
}

/// Another company's key entirely — the cross-tenant caller's scope.
fn foreign_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new("/data/orgs/someone-else"))
}

struct Seam {
    app: axum::Router,
    slug: String,
    company: Arc<CompanyDb>,
    _dir: tempfile::TempDir,
}

async fn seam_with_reconcile_trigger(reconcile_trigger: Option<Arc<tokio::sync::Notify>>) -> Seam {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let key = company_key();
    // Genesis first: every document below stamps the company's DISPLAY name
    // into a derived field, and only genesis writes that name.
    {
        let mut conn = chiefd_core::store::open_company_db(&path).expect("create company db");
        let tx = conn.transaction().expect("genesis txn");
        let mut manifest = chiefd_core::test_support::northstar_manifest(1_700_000_000_000);
        manifest.slug = COMPANY_SLUG.to_owned();
        chiefd_core::store::organization_rows::genesis(&tx, &key, &manifest)
            .expect("genesis names the company");
        tx.commit().expect("commit genesis");
    }
    let company = Arc::new(
        CompanyDb::open(&key, &path, Arc::new(SystemClock::default())).expect("open company db"),
    );
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&path.display().to_string(), 2, Arc::clone(&feed))
            .expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let mut live = SupervisionLiveSource::new(Arc::clone(&company), key.clone());
    if let Some(trigger) = reconcile_trigger {
        live = live.with_reconcile_trigger(trigger);
    }
    let app =
        router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live))
            .layer(axum::extract::Extension(seam_caller()));
    Seam { app, slug: key, company, _dir: dir }
}

/// The credential every request in this file carries.
///
/// A SERVICE identity, deliberately, and daemon-scoped (`company_slug: None`).
/// These are row-plumbing tests — they ask whether the daemon reconstructs each
/// ported store byte-equivalent — and a person credential would drag subtree
/// authority into every assertion, so a refusal about scope would read as a
/// serialization defect. A non-person principal has unconditional scope, which
/// is exactly the caller the resident actuator is; the authority rules
/// themselves are pinned by the `*_caller_http` files beside this one.
///
/// It is not optional. There is no absent-caller arm any more: the middleware
/// answers a credential-less request 401 before a handler runs, and the
/// `Caller` extractor says the same thing in the handler's own signature.
fn seam_caller() -> chiefd_api::authn::middleware::CallerIdentity {
    chiefd_api::authn::middleware::CallerIdentity(chiefd_core::store::identities::Identity {
        identity_id: "b4-seam-service".to_owned(),
        principal: "b4-seam-service".to_owned(),
        kind: chiefd_core::store::identities::IdentityKind::Service,
        company_slug: None,
        pubkey: Some("test-key".to_owned()),
        fingerprint: "fp-b4-seam-service".to_owned(),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

/// The temporary directory is HELD BY the returned [`Seam`] rather than
/// `Box::leak`ed. The leak was how it stayed alive for the router that borrows
/// its path, and it worked — at the cost of never running the destructor, so
/// every test left a ~1 MB `chief.db` in `TMPDIR` for ever. Handing it back
/// gives it the calling test's stack frame, which outlives the router by
/// construction.
async fn seam() -> Seam {
    seam_with_reconcile_trigger(None).await
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
    let payload: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, payload)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seeded_session_epoch_reads_back_over_the_route() {
    let seam = seam().await;
    let doc = json!({"version":1,"organization":"cobalt","epochAt":"2026-07-25T06:46:10.852Z","reason":"CEO-only boot"});
    let typed: chiefd_core::store::session_epoch_rows::SessionEpoch =
        serde_json::from_value(doc.clone()).expect("session epoch doc");
    seam.company.session_epoch_publish(typed).await.expect("seed the session epoch");

    let (status, out) =
        post(&seam.app, "/v1/org/session-epoch/read", json!({"slug": seam.slug})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["found"], json!(true));
    assert!(out["seq"].as_i64().expect("seq") > 0, "the publish committed a cursor");
    let got: Value = serde_json::from_str(out["doc"].as_str().expect("doc")).expect("doc json");
    assert_eq!(got, doc);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_slug_reads_not_found_and_publishes_404() {
    let seam = seam().await;
    let (status, out) =
        post(&seam.app, "/v1/org/session-epoch/read", json!({"slug": foreign_key()})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["found"], json!(false));

    // The own-company gate on the write side, proved on the one publish route
    // that still exists.
    let doc = json!({
        "version": 1,
        "observedAt": "2026-07-25T06:46:10.832Z",
        "socketName": "default",
        "status": "idle",
        "processHandles": {}
    });
    let (status, out) = post(
        &seam.app,
        "/v1/org/runtime/publish",
        json!({"slug": foreign_key(), "doc": doc.to_string()}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{out}");
    assert_eq!(out["code"], json!("unknown-company"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_queue_inserts_are_atomic_idempotent_and_conflict_safe() {
    let seam = seam().await;
    let slug = seam.slug.clone();
    let escalation = json!({
        "schemaVersion": 1,
        "fingerprint": "blocked-launch",
        "organization": "cobalt",
        "personId": "chief",
        "blocker": "Provider is unavailable.",
        "operatorAction": "Configure a provider.",
        "queuedAt": "2026-07-28T01:02:00.000Z"
    });
    let second = json!({
        "schemaVersion": 1,
        "fingerprint": "blocked-billing",
        "organization": "cobalt",
        "personId": "chief",
        "blocker": "Billing is unconfigured.",
        "operatorAction": "Add a payment method.",
        "queuedAt": "2026-07-28T01:03:00.000Z"
    });

    // Two distinct fingerprints inserted concurrently both land.
    let (first_out, second_out) = tokio::join!(
        post(
            &seam.app,
            "/v1/org/operator-escalation-intents/insert",
            json!({"slug": slug, "intent": escalation})
        ),
        post(
            &seam.app,
            "/v1/org/operator-escalation-intents/insert",
            json!({"slug": slug, "intent": second})
        ),
    );
    for (status, body) in [first_out, second_out] {
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], json!("inserted"));
    }

    // A byte-identical replay is a duplicate, not a second row.
    let escalation_body = json!({"slug": slug, "intent": escalation});
    let (status, body) =
        post(&seam.app, "/v1/org/operator-escalation-intents/insert", escalation_body.clone())
            .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], json!("duplicate"));

    // A retry that changes the payload under the same fingerprint never
    // replaces what is already queued.
    let mut fresh_escalation_retry = escalation_body;
    fresh_escalation_retry["intent"]["queuedAt"] = json!("2026-07-28T01:06:00.000Z");
    fresh_escalation_retry["intent"]["operatorAction"] = json!("A different requested action.");
    let (status, body) =
        post(&seam.app, "/v1/org/operator-escalation-intents/insert", fresh_escalation_retry).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], json!("duplicate"));

    // A stale CAS expectation is refused rather than applied.
    let (status, body) = post(
        &seam.app,
        "/v1/org/operator-escalation-intents/insert",
        json!({"slug": slug, "intent": escalation, "expectedSeq": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seeded_person_contracts_read_back_durably_without_a_revision() {
    let seam = seam().await;
    let contracts = json!({
        "version": 1,
        "organization": "cobalt",
        "contracts": {
            "chief": { "text": "# CEO\n\nLead the company.", "md5": "direct-contract" }
        }
    });
    let typed: chiefd_core::store::person_contracts::rows::OrganizationPersonContracts =
        serde_json::from_value(contracts.clone()).expect("contracts doc");
    seam.company
        .org_person_contracts_publish("2026-07-28T00:00:00.000Z".to_string(), typed)
        .await
        .expect("seed the person contracts");

    let (status, out) =
        post(&seam.app, "/v1/org/person-contracts/read", json!({"slug": seam.slug})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["found"], json!(true));
    assert!(out.get("seq").is_none(), "a direct person-contract read exposes no revision: {out}");
    let got: Value = serde_json::from_str(out["contracts"].as_str().expect("contracts"))
        .expect("contracts json");
    assert_eq!(got, contracts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seeded_launch_intent_collection_reads_back_over_the_route() {
    let seam = seam().await;
    let doc = json!({"version":1,"organization":"cobalt","sessionName":"org-cobalt","personIds":["head","worker"],"updatedAt":"2026-07-25T00:00:00.000Z"});
    let typed: chiefd_core::store::launch_intent_rows::LaunchIntent =
        serde_json::from_value(doc).expect("launch intent doc");
    seam.company.launch_intent_publish(typed).await.expect("seed the launch intent");

    let (status, out) =
        post(&seam.app, "/v1/org/launch-intent/read", json!({"slug": seam.slug})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["found"], json!(true));
    let got: Value = serde_json::from_str(out["doc"].as_str().expect("doc")).expect("doc json");
    assert_eq!(got["personIds"], json!(["head", "worker"]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seeded_runtime_owner_row_reads_back_without_its_retired_session_name() {
    let seam = seam().await;
    // AC6: the stored body still carries `sessionName`, because a historical
    // `org_documents` blob does. It is a RETIRED key — accepted and dropped,
    // never refused (a refusal would make every upgraded company's
    // runtime-owner row unbackfillable) — so this is deliberately asymmetric:
    // what goes in has it, what comes back does not.
    let doc = json!({
        "version": 1,
        "organization": "cobalt",
        "sessionName": "org-cobalt",
        "status": "active",
        "socketName": "default",
        "claimedAt": "2026-07-25T06:46:10.832Z",
        "validatedAt": "2026-07-25T18:09:51.203Z"
    });
    let typed: chiefd_core::store::runtime_owner_rows::RuntimeOwner =
        serde_json::from_value(doc.clone()).expect("a retired key must not refuse the write");
    seam.company.runtime_owner_publish(typed).await.expect("seed the runtime owner");

    let (status, out) =
        post(&seam.app, "/v1/org/runtime-owner/read", json!({"slug": seam.slug})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let got: Value = serde_json::from_str(out["doc"].as_str().expect("doc")).expect("doc json");
    let mut without_session = doc.clone();
    without_session.as_object_mut().expect("object").remove("sessionName");
    assert_eq!(got, without_session, "no session name may come back off this route (AC6)");
    assert!(got.get("sessionName").is_none());
    // The identity that DOES survive: the owner's socket, which chiefd stores
    // for the client and never parses.
    assert_eq!(got["socketName"], json!("default"));
}

// #861: the actuation-mode read seam. Proves the route returns the STORED
// mode verbatim (round-trips `"apply"` unchanged, not folded to `"shadow"`
// by any breaker/effective-config projection — the whole point of exposing
// `reconstruct()`'s raw doc rather than `effective_config()`) and that an
// unconfigured company reads real absence, not a defaulted doc.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn converge_safety_read_returns_the_stored_actuation_mode() {
    let seam = seam().await;
    let doc = json!({
        "schemaVersion": 1,
        "actuationMode": "apply",
        "sweepLive": true,
        "budgetOverride": false,
        "consecutiveFailures": 0,
        "breakerTripped": false,
        "cycleInProgress": false
    });
    let typed: chiefd_core::store::converge_safety::ConvergeSafetyState =
        serde_json::from_value(doc.clone()).expect("converge safety doc");
    seam.company.converge_safety_publish(typed).await.expect("seed the converge safety state");

    let (status, out) =
        post(&seam.app, "/v1/org/converge-safety/read", json!({"slug": seam.slug})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let got: Value = serde_json::from_str(out["doc"].as_str().expect("doc")).expect("doc json");
    assert_eq!(
        got["actuationMode"],
        json!("apply"),
        "the read must return the STORED mode, not a computed one"
    );
    assert_eq!(got, doc);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn converge_safety_read_of_an_unconfigured_company_is_real_absence_not_a_default() {
    let seam = seam().await;
    let (status, out) =
        post(&seam.app, "/v1/org/converge-safety/read", json!({"slug": seam.slug})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        out["found"],
        json!(false),
        "an unconfigured company must read as absent, never a defaulted shadow doc"
    );
    assert!(out.get("doc").is_none() || out["doc"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_second_publish_uses_current_state_without_a_caller_fence() {
    let seam = seam().await;
    let slug = seam.slug.clone();
    let first = json!({
        "version": 1,
        "observedAt": "2026-07-25T06:46:10.832Z",
        "session": "org-cobalt",
        "socketName": "default",
        "status": "running",
        "processHandles": {}
    });
    let (status, out) =
        post(&seam.app, "/v1/org/runtime/publish", json!({"slug": slug, "doc": first.to_string()}))
            .await;
    assert_eq!(status, StatusCode::OK, "{out}");

    // Same rule as the runtime-owner read above: `session` goes in (a
    // historical blob has it) and does not come back (AC6 retired the column
    // and the field).
    let second = json!({
        "version": 1,
        "observedAt": "2026-07-25T06:47:10.832Z",
        "session": "org-cobalt",
        "socketName": "default",
        "status": "idle",
        "processHandles": {}
    });
    let (status, out) = post(
        &seam.app,
        "/v1/org/runtime/publish",
        json!({"slug": slug, "doc": second.to_string()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(out["seq"].as_i64().unwrap_or_default() > 1, "{out}");

    let (status, out) = post(&seam.app, "/v1/org/runtime/read", json!({"slug": slug})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let got: Value = serde_json::from_str(out["doc"].as_str().expect("doc")).expect("doc json");
    let mut without_session = second.clone();
    without_session.as_object_mut().expect("object").remove("session");
    assert_eq!(got, without_session, "no session name may come back off this route (AC6)");
    assert!(got.get("session").is_none());
}

/// The macro's publish arm rejects a caller-supplied sequence outright rather
/// than silently reintroducing caller-side CAS. Only one route in this family
/// still has a publish half, so this pins the rule where it can still be
/// broken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_direct_atomic_singleton_publish_rejects_a_retired_expected_seq_payload() {
    let seam = seam().await;
    let doc = json!({
        "version": 1,
        "observedAt": "t",
        "session": "org-cobalt",
        "socketName": "default",
        "status": "idle"
    });
    let (status, out) = post(
        &seam.app,
        "/v1/org/runtime/publish",
        json!({"slug": seam.slug, "expectedSeq": 0, "doc": doc.to_string()}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{out}");
}

/// Arch-audit Step 6 (F1): the wake is no longer a launch-intent-only opt-in.
/// The macro's publish arm wakes the reconcile duty by default, and
/// `no_reconcile_wake` is the only opt-out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_macro_publish_arm_wakes_the_reconcile_duty() {
    let trigger = Arc::new(tokio::sync::Notify::new());
    let seam = seam_with_reconcile_trigger(Some(Arc::clone(&trigger))).await;
    let waiter = {
        let trigger = Arc::clone(&trigger);
        tokio::spawn(async move { trigger.notified().await })
    };
    tokio::task::yield_now().await;

    let doc = json!({
        "version": 1,
        "observedAt": "2026-07-25T06:46:10.832Z",
        "socketName": "default",
        "status": "running",
        "processHandles": {}
    });
    let (status, out) = post(
        &seam.app,
        "/v1/org/runtime/publish",
        json!({"slug": seam.slug, "doc": doc.to_string()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");

    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("a macro-row publish must wake the reconcile duty (wake-by-default)")
        .expect("reconcile wake waiter task");
}

/// Arch-audit Step 6 (F1 shape 2, the Gap 1 trap): the hand-written
/// `*_clear` handlers carried zero wakes — withdrawal of launch intent rode
/// the ~30s cadence. A clear must wake the reconcile duty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launch_intent_clear_wakes_the_reconcile_duty() {
    let trigger = Arc::new(tokio::sync::Notify::new());
    let seam = seam_with_reconcile_trigger(Some(Arc::clone(&trigger))).await;
    let doc = json!({
        "version": 1,
        "organization": "cobalt",
        "sessionName": "org-cobalt",
        "personIds": ["head"],
        "updatedAt": "2026-07-28T16:00:00.000Z"
    });
    let typed: chiefd_core::store::launch_intent_rows::LaunchIntent =
        serde_json::from_value(doc).expect("launch intent doc");
    seam.company.launch_intent_publish(typed).await.expect("seed the launch intent to clear");

    let waiter = {
        let trigger = Arc::clone(&trigger);
        tokio::spawn(async move { trigger.notified().await })
    };
    tokio::task::yield_now().await;

    let (status, out) = post(
        &seam.app,
        "/v1/org/launch-intent/clear",
        json!({"slug": seam.slug, "at": "2026-07-28T16:05:00.000Z"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");

    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("launch-intent clear must wake the reconcile duty (the Gap 1 trap)")
        .expect("reconcile wake waiter task");
}
