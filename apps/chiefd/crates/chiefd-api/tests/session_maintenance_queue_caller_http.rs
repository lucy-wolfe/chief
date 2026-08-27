//! `/v1/org/session-maintenance/queue` reads its caller — proved WITH one present.
//!
//! This is the rule `VerbAuth` used to declare and never enforced: queueing
//! maintenance names a TARGET, so a worker who could queue it could queue a
//! fresh session for anyone. Track B1 made it real — the route binds
//! `requestedBy` to the authenticated caller, and core asks whether that
//! requester MANAGES the target — and B1's own tests cover the core half
//! (`session_maintenance_ops`:
//! `a_stranger_cannot_queue_maintenance_against_somebody_they_do_not_manage`
//! and its two siblings).
//!
//! # Why this file exists beside those
//!
//! `bind_requester_to_caller` used to return early when there was NO caller
//! extension, which is what let B1 land before credentials were universal.
//! Every test that exercised the route without one therefore proved only that
//! the binding did not break anything — never that it works. That arm is now
//! deleted (the helper takes `&Identity`, and a request proving no identity is
//! answered 401 before a handler runs), so these three run the real router with
//! a `CallerIdentity` PRESENT, which is the only shape that can tell the
//! difference:
//!
//! * a person naming somebody else as the requester is refused,
//! * a person from another company is refused,
//! * a person naming ITSELF passes the binding and reaches core.
//!
//! The third is the one that keeps the first two honest: a route that refused
//! everything would satisfy both negatives.

#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Extension;
use axum::http::{Request, StatusCode};
use chiefd_api::authn::middleware::CallerIdentity;
use chiefd_api::docstore::{
    router_with_supervision_live, ChangeFeed, DocStore, SupervisionLiveSource,
};
use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SystemClock;
use chiefd_core::store::identities::{Identity, IdentityKind};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const SLUG: &str = "queue-caller";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/queue-caller";

/// The COMPOSITE document key the handler compares against, exactly as
/// `provider_models_http` builds it — a display slug fails the label match.
fn wire_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new(COMPANY_DIR))
}

/// A PERSON identity for `principal`, scoped to `company`.
///
/// The scope is the company KEY, never the display slug: the binding compares
/// `caller.company_slug` against the `slug` on the request, and that field
/// carries `company_key(dir)` in production. A harness that scoped the identity
/// by the display name would agree with itself and disagree with the daemon —
/// the same trap `provider_models_http` records for its own wire key.
fn person_identity(principal: &str, company: Option<&str>) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: format!("id-{principal}"),
        principal: principal.to_owned(),
        kind: IdentityKind::Person,
        company_slug: company.map(ToOwned::to_owned),
        pubkey: Some("test-key".to_owned()),
        fingerprint: format!("fp-{principal}"),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

async fn app(caller: Option<CallerIdentity>) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let company = Arc::new(
        CompanyDb::open(&wire_key(), &path, Arc::new(SystemClock::default())).expect("company"),
    );
    let mut manifest = chiefd_core::test_support::northstar_manifest(0);
    manifest.slug = SLUG.to_owned();
    let genesis_manifest = manifest.clone();
    company
        .org_manifest_genesis(
            manifest,
            "2026-01-01T00:00:00.000Z".to_owned(),
            chiefd_core::store::person_contracts::build::build_organization_person_contracts(
                &genesis_manifest,
            )
            .expect("person contracts document"),
        )
        .await
        .expect("manifest genesis");
    let feed = Arc::new(ChangeFeed::new());
    let store =
        Arc::new(DocStore::open_with_feed(&path.display().to_string(), 2, feed).expect("docstore"));
    store.ensure_schema().await.expect("schema");
    let router = router_with_supervision_live(
        store,
        1024 * 1024,
        Duration::from_secs(15),
        Some(SupervisionLiveSource::new(company, wire_key())),
    );
    let router = match caller {
        Some(identity) => router.layer(Extension(identity)),
        None => router,
    };
    (router, dir)
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
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A compact request against the northstar fixture: `requested_by` is the
/// claim under test, `person_id` the target.
fn queue_body(requested_by: &str, person_id: &str) -> Value {
    json!({
        "slug": wire_key(),
        "action": "compact",
        "personId": person_id,
        "requestedBy": requested_by,
        "reason": "caller binding under test"
    })
}

#[tokio::test]
async fn a_person_naming_somebody_else_as_the_requester_is_refused() {
    // The impersonation the body always allowed: `requestedBy` is a plain
    // string, so before B1 a caller could attribute the request to anyone.
    let (app, _dir) = app(Some(person_identity("signal-researcher", Some(&wire_key())))).await;
    let (status, body) =
        post(&app, "/v1/org/session-maintenance/queue", queue_body("quant-head", "chief")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("requester-identity-mismatch"),
        "the refusal must name the binding that failed: {body}"
    );
}

#[tokio::test]
async fn a_person_from_another_company_is_refused() {
    // A person identity is company-scoped, and a person of one company must
    // never queue maintenance in another.
    let (app, _dir) = app(Some(person_identity("quant-head", Some("another-company")))).await;
    let (status, body) = post(
        &app,
        "/v1/org/session-maintenance/queue",
        queue_body("quant-head", "signal-researcher"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("requester-company-mismatch"),
        "the refusal must name the company boundary: {body}"
    );
}

#[tokio::test]
async fn a_person_naming_itself_passes_the_binding_and_reaches_core() {
    // The positive that keeps the two negatives honest. `quant-head` heads the
    // department `signal-researcher` sits in, so core's scope check admits it
    // and the request is accepted — the binding is a fence, not a wall.
    let (app, _dir) = app(Some(person_identity("quant-head", Some(&wire_key())))).await;
    let (status, body) = post(
        &app,
        "/v1/org/session-maintenance/queue",
        queue_body("quant-head", "signal-researcher"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a manager queueing for its own member: {body}");
    assert_eq!(
        body.get("requestedBy").and_then(Value::as_str),
        Some("quant-head"),
        "the accepted request records the bound requester: {body}"
    );
}

// TOMBSTONE: `a_manager_model_switch_reaches_the_same_server_side_subtree_fence`.
// It proved that `set_model` is fenced by the SAME server-side subtree question
// every other queue verb asks — a manager may switch a subordinate's model and
// the route binds `requestedBy` to the authenticated caller. `set_model` is
// deleted, so there is no second action left to prove the fence is general.
//
// The fence itself is unchanged and still covered: the tests above drive the
// same route with `compact`, including the manager-queueing-for-its-own-member
// case and the naming-somebody-else refusal.
