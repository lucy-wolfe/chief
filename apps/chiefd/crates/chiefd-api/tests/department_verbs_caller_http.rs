//! The remaining destructive DEPARTMENT routes read their caller.
//!
//! `/v1/org/department/{move-members, pause, resume, resume-many,
//! reactivate-executive-root}` each took their target out of the request body,
//! handed core `String::new()` as the actor, and asked nothing about who was
//! calling. Any caller that reached them could empty one department into
//! another, or stop and start any part of the company — and the ledger
//! recorded the author as the empty string.
//!
//! `reparent` is NOT here: it landed separately on `main` (#1095) with its own
//! two-code refusal and its own HTTP file beside this one.
//!
//! `org_ops`'s own tests cover the RULE. This file covers the WIRING: core can
//! only refuse an actor the route actually gives it, and until now every one of
//! these routes gave it nothing.
//!
//! The manifest is `northstar`: `ceo` heads the company root `executive`,
//! `quant-head` heads `quant`, and `it-head` heads the sibling `it`. So a head
//! reaching across at the other's unit is the sideways move the tree forbids,
//! and the CEO's identical call is the positive case that stops a route which
//! refused everybody from satisfying the refusals on its own.

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

const SLUG: &str = "department-verbs-caller";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/department-verbs-caller";

/// The COMPOSITE document key the handler compares against — a display slug
/// fails the label match.
fn wire_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new(COMPANY_DIR))
}

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

/// A DAEMON-SCOPED identity of `kind`: no company slug, principal named after
/// the kind — the shape `enroll_bootstrap_operator` mints for `operator`, and
/// the shape A3 will mint for `service`.
fn daemon_identity(kind: IdentityKind, principal: &str) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: principal.to_owned(),
        principal: principal.to_owned(),
        kind,
        company_slug: None,
        pubkey: Some("test-key".to_owned()),
        fingerprint: format!("fp-{principal}"),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

struct Harness {
    router: axum::Router,
    company: Arc<CompanyDb>,
    _dir: tempfile::TempDir,
}

async fn app(caller: Option<CallerIdentity>) -> Harness {
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
        Some(SupervisionLiveSource::new(Arc::clone(&company), wire_key())),
    );
    let router = match caller {
        Some(identity) => router.layer(Extension(identity)),
        None => router,
    };
    Harness { router, company, _dir: dir }
}

async fn post(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn assert_out_of_scope(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("actor-out-of-scope"),
        "body: {body}"
    );
}

fn assert_applied(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("applied").and_then(Value::as_bool), Some(true), "body: {body}");
}

/// Pause a department through the company directly, with an actor that names
/// nobody, so the only judged call in a resume test is the resume itself.
async fn pause_directly(company: &CompanyDb, department_id: &str) {
    let outcome = company
        .pause_department(department_id.to_owned(), "t0".to_owned(), String::new())
        .await
        .expect("pause");
    assert!(
        matches!(outcome, chiefd_core::store::org_ops::PauseOutcome::Applied),
        "setup pause must apply: {outcome:?}"
    );
}

// ---- move-members ---------------------------------------------------------

fn move_members_body() -> Value {
    json!({ "slug": wire_key(), "fromDepartmentId": "quant", "destinationId": "it" })
}

#[tokio::test]
async fn a_head_that_manages_only_the_destination_cannot_move_members() {
    let h = app(Some(person_identity("it-head"))).await;
    let (status, body) =
        post(&h.router, "/v1/org/department/move-members", move_members_body()).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn a_head_that_manages_only_the_source_cannot_move_members() {
    let h = app(Some(person_identity("quant-head"))).await;
    let (status, body) =
        post(&h.router, "/v1/org/department/move-members", move_members_body()).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_moves_members_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    let (status, body) =
        post(&h.router, "/v1/org/department/move-members", move_members_body()).await;
    assert_applied(status, &body);
}

// ---- pause / resume / resume-many -----------------------------------------

fn quant_body() -> Value {
    json!({ "slug": wire_key(), "departmentId": "quant" })
}

#[tokio::test]
async fn a_sibling_head_cannot_pause_another_departments_unit() {
    let h = app(Some(person_identity("it-head"))).await;
    let (status, body) = post(&h.router, "/v1/org/department/pause", quant_body()).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_pauses_a_department_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    let (status, body) = post(&h.router, "/v1/org/department/pause", quant_body()).await;
    assert_applied(status, &body);
}

/// NO CALLER, NO PAUSE. The rollout arm that made absence mean "local trust"
/// is deleted: a request that proves no identity is refused `401
/// caller-unauthenticated` by the `Caller` extractor, and nothing is written.
///
/// The second half is the load-bearing one. `quant` is still RUNNING after the
/// refusal, so the CEO's identical call still has something to pause — a
/// refusal that returned the right status and paused the department anyway
/// would satisfy the status assertion on its own.
#[tokio::test]
async fn without_a_caller_pause_is_401_and_pauses_nothing() {
    let h = app(None).await;
    let (status, body) = post(&h.router, "/v1/org/department/pause", quant_body()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-unauthenticated"),
        "body: {body}"
    );

    // Same router, same store: a fresh fixture would prove nothing about what
    // the refused call did.
    let ceo = h.router.clone().layer(Extension(person_identity("chief")));
    let (status, body) = post(&ceo, "/v1/org/department/pause", quant_body()).await;
    assert_applied(status, &body);
}

#[tokio::test]
async fn a_sibling_head_cannot_resume_another_departments_unit() {
    let h = app(Some(person_identity("it-head"))).await;
    pause_directly(&h.company, "quant").await;
    let (status, body) = post(&h.router, "/v1/org/department/resume", quant_body()).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_resumes_a_department_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    pause_directly(&h.company, "quant").await;
    let (status, body) = post(&h.router, "/v1/org/department/resume", quant_body()).await;
    assert_applied(status, &body);
}

/// EVERY department in the batch is asked, not the first. A batch naming a unit
/// the actor does not manage is refused whole — otherwise the batch verb would
/// be a way round the single verb's guard.
#[tokio::test]
async fn resume_many_refuses_a_batch_holding_a_unit_outside_the_actors_subtree() {
    let h = app(Some(person_identity("it-head"))).await;
    pause_directly(&h.company, "it").await;
    pause_directly(&h.company, "quant").await;
    let body = json!({ "slug": wire_key(), "departmentIds": ["it", "quant"] });
    let (status, body) = post(&h.router, "/v1/org/department/resume-many", body).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_resumes_a_batch_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    pause_directly(&h.company, "it").await;
    pause_directly(&h.company, "quant").await;
    let body = json!({ "slug": wire_key(), "departmentIds": ["it", "quant"] });
    let (status, body) = post(&h.router, "/v1/org/department/resume-many", body).await;
    assert_applied(status, &body);
}

// ---- reactivate-executive-root --------------------------------------------

/// The target is the company ROOT, and the only person who manages the root is
/// the one who heads it. That falls out of the same subtree predicate every
/// other verb asks — it names no title and protects no region.
#[tokio::test]
async fn a_head_that_does_not_hold_the_root_cannot_reactivate_it() {
    let h = app(Some(person_identity("quant-head"))).await;
    let body = json!({ "slug": wire_key() });
    let (status, body) =
        post(&h.router, "/v1/org/department/reactivate-executive-root", body).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_reactivates_the_root_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    let body = json!({ "slug": wire_key() });
    let (status, body) =
        post(&h.router, "/v1/org/department/reactivate-executive-root", body).await;
    assert_applied(status, &body);
}

// ---- daemon-scoped identities, and what happens after A6 -------------------

/// A DAEMON-SCOPED IDENTITY KEEPS ITS UNCONDITIONAL SCOPE. The department
/// fences resolve a PERSON through `person_manages_department`, and they are
/// reached only through `actor_names_a_person`. An operator, a service and a
/// channel each carry a principal that names no person row, so the predicate
/// answers false and core does not judge the actor.
///
/// This is the distinction the absent-caller deletion must NOT flatten. "No
/// credential at all" is now impossible — the middleware answers 401 first —
/// but a NON-PERSON principal is a real, authenticated caller whose scope is
/// unconditional, and it stays that way. `Channel` is included because it is
/// the kind nobody thinks of.
#[tokio::test]
async fn a_daemon_scoped_identity_keeps_its_unconditional_scope() {
    for (kind, principal) in [
        (IdentityKind::Operator, "operator"),
        (IdentityKind::Service, "service"),
        (IdentityKind::Channel, "channel"),
    ] {
        let h = app(Some(daemon_identity(kind, principal))).await;
        let (status, body) = post(&h.router, "/v1/org/department/pause", quant_body()).await;
        assert_applied(status, &body);
    }
}
