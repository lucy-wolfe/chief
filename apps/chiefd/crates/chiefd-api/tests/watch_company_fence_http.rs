//! `GET /v1/docs/watch` is fenced to the caller's COMPANY (B4, route 7).
//!
//! # Why this route's fence is not a subtree
//!
//! Every other route in this packet narrows to the caller's part of the
//! organization tree. This one cannot and should not. Its handler is a closure
//! over `State(store)` alone — no `SupervisionLiveSource`, therefore no
//! manifest, therefore no departments and no tree to walk. And the thing it
//! discloses is not a person or a unit: it is a DOCUMENT changing, named by
//! `{store, key, seq}` and selected by a slug plus a store CSV.
//!
//! So the fence is the one the identity already carries. `company_slug` is
//! `Some` exactly for a `Person` identity, and a person credential issued for
//! company A must not be able to subscribe to company B's document stream —
//! which is a cross-company leak, the failure this route can actually have.
//! Every daemon-scoped identity (operator, service, channel) carries `None`
//! and passes, which is what keeps the resident actuator's own
//! `GET /v1/docs/watch` working; it is one of its four calls.
//!
//! Do not later "improve" this into a subtree check. There is no subtree here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Extension;
use axum::http::{Request, StatusCode};
use chiefd_api::authn::middleware::CallerIdentity;
use chiefd_api::docstore::{router_with_heartbeat_interval, DocStore};
use chiefd_core::store::identities::{Identity, IdentityKind};
use tower::ServiceExt;

const SLUG: &str = "watch-fence@abc123";
const OTHER_SLUG: &str = "somebody-else@def456";

fn identity(principal: &str, kind: IdentityKind, company: Option<&str>) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: format!("id-{principal}"),
        principal: principal.to_owned(),
        kind,
        company_slug: company.map(str::to_owned),
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
    let path = dir.path().join("org.sqlite").display().to_string();
    let store = Arc::new(DocStore::open(&path, 4).expect("open"));
    store.ensure_schema().await.expect("schema");
    let router = router_with_heartbeat_interval(store, 1024 * 1024, Duration::from_secs(15));
    let router = match caller {
        Some(identity) => router.layer(Extension(identity)),
        None => router,
    };
    (router, dir)
}

/// The response HEAD only. An SSE body is infinite by construction, so a test
/// that collected it would hang — the status is the whole assertion here.
async fn watch(app: &axum::Router, slug: &str) -> StatusCode {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/v1/docs/watch?slug={slug}&stores=organization"))
        .body(Body::empty())
        .expect("request");
    app.clone().oneshot(request).await.expect("route").status()
}

/// THE LEAK. A person credential minted for another company subscribing to
/// this one's document stream.
#[tokio::test]
async fn a_person_from_another_company_cannot_watch_this_ones_documents() {
    let (app, _dir) = app(Some(identity("stranger", IdentityKind::Person, Some(OTHER_SLUG)))).await;
    assert_eq!(watch(&app, SLUG).await, StatusCode::UNPROCESSABLE_ENTITY);
}

/// THE POSITIVE. Same person, own company — a fence that refused every person
/// would satisfy the test above on its own.
#[tokio::test]
async fn a_person_watches_its_own_companys_documents() {
    let (app, _dir) = app(Some(identity("quant-head", IdentityKind::Person, Some(SLUG)))).await;
    assert_eq!(watch(&app, SLUG).await, StatusCode::OK);
}

/// THE ACTUATOR. `GET /v1/docs/watch` is one of the resident actuator's four
/// HTTP calls and it authenticates as a SERVICE, which carries no
/// `company_slug`. Refusing it here would park the convergence loop forever.
#[tokio::test]
async fn the_actuators_service_identity_watches_any_company() {
    let (app, _dir) = app(Some(identity("chiefd-actuator", IdentityKind::Service, None))).await;
    assert_eq!(watch(&app, SLUG).await, StatusCode::OK);
}

#[tokio::test]
async fn an_operator_identity_watches_any_company() {
    let (app, _dir) = app(Some(identity("operator", IdentityKind::Operator, None))).await;
    assert_eq!(watch(&app, OTHER_SLUG).await, StatusCode::OK);
}

/// NO CALLER, NO STREAM. A change feed handed to an uncredentialed subscriber
/// is a live disclosure of everything the company writes, so the arm that used
/// to open it is deleted and the request is 401 instead.
#[tokio::test]
async fn without_a_caller_the_stream_is_401() {
    let (app, _dir) = app(None).await;
    assert_eq!(watch(&app, SLUG).await, StatusCode::UNAUTHORIZED);
}
