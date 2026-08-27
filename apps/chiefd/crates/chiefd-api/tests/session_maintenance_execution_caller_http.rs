//! The five session-maintenance EXECUTION verbs bind their identity to the caller.
//!
//! `start`, `defer`, `interrupt`, `recover` and `finish`
//! carry `identity: { personId }` in the BODY, and core's
//! `ExpectedIdentity::assert_owns` compares it against another caller-supplied
//! value. That is an integrity check, not authentication: a caller that names
//! the victim in both fields passes it. These five are the running person's OWN
//! verbs — the intercom fills the identity from the pane's context, spread last
//! so a payload cannot forge it — so the fence is the strongest one available,
//! the authenticated caller must BE the person it names.
//!
//! # Why these tests construct a caller
//!
//! `bind_requester_to_caller` used to return early when there was no caller
//! extension, which is what let the binding land before credentials were
//! universal. A test that omitted the caller therefore proved only that the
//! binding does not BREAK anything, never that it works. That arm is now
//! deleted — the helper takes `&Identity` — so each case below layers a
//! `CallerIdentity` onto the real router, which is the only shape there is.
//!
//! The accept case is not decoration: two refusals alone would pass against a
//! route that refused everything, so one verb is driven all the way through the
//! binding to core's own answer.

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

const SLUG: &str = "maint-exec";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/maint-exec";

/// Every execution verb, with the body shape it accepts. `start` is the one
/// carrying an action; the rest take a request id and the same identity.
// TOMBSTONE: `complete-native` was a sixth verb here. Deleted with the
// company-scoped maintenance request it completed; its wire type
// `maint.complete_native` went with it.
const EXECUTION_VERBS: &[&str] = &["defer", "interrupt", "recover", "finish"];

/// The COMPOSITE document key the handlers compare against. Scoping a test
/// identity by the DISPLAY slug makes the harness agree with itself and
/// disagree with the daemon — `bind_requester_to_caller` reads
/// `caller.company_slug` against this value, and `provider_models_http` records
/// the same trap for its own wire key.
fn wire_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new(COMPANY_DIR))
}

fn person_identity(principal: &str, company: &str) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: format!("id-{principal}"),
        principal: principal.to_owned(),
        kind: IdentityKind::Person,
        company_slug: Some(company.to_owned()),
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

/// A body each verb DESERIALIZES, so the binding is what answers rather than a
/// 422. The shapes differ: `start` carries an action and an optional claim,
/// `recover` a claim and no request id, `finish` a status, and the claimed
/// verbs both.
fn body_for(verb: &str, person_id: &str) -> Value {
    let claim = json!({ "processId": 1, "sessionId": "session", "claimToken": "token" });
    let mut body = json!({
        "slug": wire_key(),
        "identity": { "personId": person_id }
    });
    match verb {
        "start" => {
            body["action"] = json!("compact");
        }
        "recover" => {
            body["claim"] = claim;
        }
        "finish" => {
            body["requestId"] = json!("request-under-test");
            body["status"] = json!("completed");
        }
        _ => {
            body["requestId"] = json!("request-under-test");
            body["claim"] = claim;
        }
    }
    body
}

#[tokio::test]
async fn every_execution_verb_refuses_a_caller_naming_somebody_else() {
    // The impersonation the body shape allows on its own: `identity` is
    // caller-supplied, so before the binding a pane could complete, defer or
    // interrupt ANOTHER person's maintenance by naming them in it.
    for verb in EXECUTION_VERBS.iter().chain(std::iter::once(&"start")) {
        let (app, _dir) = app(Some(person_identity("signal-researcher", &wire_key()))).await;
        let (status, body) = post(
            &app,
            &format!("/v1/org/session-maintenance/{verb}"),
            body_for(verb, "quant-head"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{verb} accepted a foreign identity: {body}");
        assert_eq!(
            body.get("code").and_then(Value::as_str),
            Some("requester-identity-mismatch"),
            "{verb} must name the binding that failed: {body}"
        );
    }
}

#[tokio::test]
async fn every_execution_verb_refuses_a_caller_from_another_company() {
    // A person identity is company-scoped. Naming yourself correctly is not
    // enough if you are somebody else's person.
    for verb in EXECUTION_VERBS.iter().chain(std::iter::once(&"start")) {
        let (app, _dir) = app(Some(person_identity("quant-head", "another-company"))).await;
        let (status, body) = post(
            &app,
            &format!("/v1/org/session-maintenance/{verb}"),
            body_for(verb, "quant-head"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{verb} accepted a foreign company: {body}");
        assert_eq!(
            body.get("code").and_then(Value::as_str),
            Some("requester-company-mismatch"),
            "{verb} must name the company boundary: {body}"
        );
    }
}

#[tokio::test]
async fn a_person_naming_itself_passes_the_binding_and_reaches_core() {
    // The case that keeps the refusals honest. This must NOT be
    // `requester-identity-mismatch` or `requester-company-mismatch`: the
    // binding admits it, and whatever answer comes back is core's own — here a
    // refusal about the maintenance state, because the fixture has no queued
    // request to defer. A route that refused everything would satisfy both
    // tests above and fail this one.
    let (app, _dir) = app(Some(person_identity("quant-head", &wire_key()))).await;
    let (status, body) =
        post(&app, "/v1/org/session-maintenance/defer", body_for("defer", "quant-head")).await;
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default();
    assert!(
        !["requester-identity-mismatch", "requester-company-mismatch"].contains(&code),
        "the binding must admit a person acting as itself; got {status} {body}"
    );
}
