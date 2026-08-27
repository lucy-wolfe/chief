//! `/v1/org/department/reparent` reads its caller — proved WITH one present.
//!
//! The reorg keystone re-hung a department, everything beneath it, and every
//! person in it under a new parent, and asked nothing at all: no requester in
//! the body, no caller from the extractor, and `String::new()` handed to core
//! as the actor. Any caller that reached it could reorganize any company, and
//! the ledger recorded the author as the empty string.
//!
//! # Why this file exists beside the core tests
//!
//! `org_ops`'s own tests cover the RULE. What they cannot cover is the WIRING:
//! core can only judge an actor the route actually gives it, and until now the
//! route gave it nothing. Every existing test of this route runs without a
//! caller extension, so all of them prove the change breaks nothing and none of
//! them prove it works.
//!
//! # The half that is easy to miss
//!
//! A reparent is TWO questions. It detaches a subtree from one place and
//! attaches it somewhere else, so scope over the MOVED unit alone is not
//! enough — a head could hang their own department, and everyone in it, under a
//! parent they have no authority over. That case gets its own test and its own
//! code, because "you cannot move that" and "you cannot move it THERE" send the
//! caller to different fixes.

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

const SLUG: &str = "reparent-caller";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/reparent-caller";

/// The COMPOSITE document key the handler compares against — a display slug
/// fails the label match, and a harness that used one would agree with itself
/// and disagree with the daemon.
fn wire_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new(COMPANY_DIR))
}

/// A PERSON identity for `principal`, scoped to the company by that same
/// composite key.
fn person_identity(principal: &str) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: format!("id-{principal}"),
        principal: principal.to_owned(),
        kind: IdentityKind::Person,
        company_slug: Some(wire_key()),
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

async fn reparent(
    app: &axum::Router,
    department_id: &str,
    new_parent_id: &str,
) -> (StatusCode, Value) {
    let body = json!({
        "slug": wire_key(),
        "departmentId": department_id,
        "newParentId": new_parent_id,
    });
    let request = Request::builder()
        .method("POST")
        .uri("/v1/org/department/reparent")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// THE POSITIVE CASE. The CEO heads the root and therefore manages every
/// department, so an ordinary whole-company reorg still applies — and this is
/// what stops the refusals below from being satisfied by a route that simply
/// refuses everybody.
#[tokio::test]
async fn the_ceo_reparents_a_department_and_it_applies() {
    let (app, _dir) = app(Some(person_identity("chief"))).await;
    let (status, body) = reparent(&app, "it", "quant").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("applied").and_then(Value::as_bool), Some(true), "body: {body}");
}

/// HALF ONE: you cannot move a unit OUT of somebody else's subtree.
/// `quant-head` heads `quant`; `it` is a sibling, so taking it is refused.
#[tokio::test]
async fn a_head_reparenting_a_siblings_department_is_refused() {
    let (app, _dir) = app(Some(person_identity("quant-head"))).await;
    let (status, body) = reparent(&app, "it", "quant").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("actor-out-of-scope"),
        "body: {body}"
    );
}

/// HALF TWO — THE ESCALATION. The moved unit IS in the caller's scope:
/// `quant-head` heads `quant`. A guard that asked only about the moved unit
/// would let this through, and it would hang Quant and everyone in it beneath
/// `it`, a department `quant-head` has no authority over.
///
/// The distinct code is the point. Refusing this as `actor-out-of-scope` would
/// tell the caller they do not manage Quant, which is false and sends them
/// looking in the wrong place.
#[tokio::test]
async fn a_head_grafting_its_own_department_under_a_foreign_parent_is_refused() {
    let (app, _dir) = app(Some(person_identity("quant-head"))).await;
    let (status, body) = reparent(&app, "quant", "it").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("new-parent-out-of-scope"),
        "body: {body}"
    );
}

/// NO CALLER, NO REORG. The rollout arm that made absence mean "local trust"
/// is deleted: a request that proves no identity is refused `401
/// caller-unauthenticated` by the `Caller` extractor, and the tree is exactly
/// where it was.
///
/// The second half is the load-bearing one — a refusal that returned the right
/// status while re-hanging the subtree anyway would satisfy the status
/// assertion on its own. The CEO's reparent to `quant` can only apply if `it`
/// is still under its original parent.
#[tokio::test]
async fn without_a_caller_the_route_is_401_and_moves_nothing() {
    let (app, _dir) = app(None).await;
    let (status, body) = reparent(&app, "it", "quant").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-unauthenticated"),
        "body: {body}"
    );

    // Same router, same store: a fresh fixture would prove nothing about what
    // the refused call did.
    let ceo = app.clone().layer(Extension(person_identity("chief")));
    let (status, body) = reparent(&ceo, "it", "quant").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("applied").and_then(Value::as_bool), Some(true), "body: {body}");
}
