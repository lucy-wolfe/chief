//! `/v1/org/department/remove-tree` reads its caller — proved WITH one present.
//!
//! This route deleted a whole department subtree, offboarded everyone under it,
//! and asked nothing at all: no requester in the body, no caller from the
//! extractor, and `String::new()` handed to core as the actor. So any caller
//! that reached it could delete any department in any company, and the staffing
//! ledger recorded the author of the most destructive verb in the crate as the
//! empty string.
//!
//! # Why this file exists beside the core tests
//!
//! `org_ops`'s own tests cover the RULE — the CEO may remove, a sibling head
//! may not, and an actor naming no person row is not judged. What they cannot
//! cover is the WIRING: core can only refuse an actor the route actually gives
//! it, and until now the route gave it nothing. Every test that exercises this
//! route without a caller extension therefore proves the change breaks nothing,
//! never that it works.
//!
//! These tests run the real router with a `CallerIdentity` PRESENT, which is
//! the only shape that can tell the difference:
//!
//! * the CEO removes a department and it applies,
//! * a head removes its own department and it applies,
//! * a head removes a SIBLING's department and is refused,
//! * person identities from another company are refused before principal
//!   lookup, whether their principal is absent or collides with the CEO,
//! * a daemon operator retains company-wide scope,
//! * with no caller at all the route is `401 caller-unauthenticated` and the
//!   subtree is still there.
//!
//! The first is the one that keeps the second honest: a route that refused
//! everything would satisfy the negative on its own.

#![allow(clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
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

const SLUG: &str = "remove-tree-caller";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/remove-tree-caller";

/// The COMPOSITE document key the handler compares against — a display slug
/// fails the label match, and a harness that used one would agree with itself
/// and disagree with the daemon.
fn wire_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new(COMPANY_DIR))
}

fn identity(kind: IdentityKind, principal: &str, company_slug: Option<&str>) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: format!("id-{principal}"),
        principal: principal.to_owned(),
        kind,
        company_slug: company_slug.map(str::to_owned),
        pubkey: Some("test-key".to_owned()),
        fingerprint: format!("fp-{principal}"),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

/// A PERSON identity for `principal`, scoped to the company by that same
/// composite key.
fn person_identity(principal: &str) -> CallerIdentity {
    identity(IdentityKind::Person, principal, Some(&wire_key()))
}

async fn app(caller: Option<CallerIdentity>) -> (axum::Router, tempfile::TempDir, Arc<CompanyDb>) {
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
    (router, dir, company)
}

static ROUTE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn remove_unlocked(app: &axum::Router, department_id: &str) -> (StatusCode, Value) {
    let body = json!({ "slug": wire_key(), "departmentId": department_id });
    let request = Request::builder()
        .method("POST")
        .uri("/v1/org/department/remove-tree")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn remove(app: &axum::Router, department_id: &str) -> (StatusCode, Value) {
    let _guard = ROUTE_TEST_LOCK.lock().await;
    remove_unlocked(app, department_id).await
}

/// THE POSITIVE CASE. The CEO manages the whole company, so its removal
/// applies — and this is what stops the refusal below from being satisfied by
/// a route that simply refuses everybody.
#[tokio::test]
async fn the_ceo_removes_a_department_and_it_applies() {
    let (app, _dir, _company) = app(Some(person_identity("chief"))).await;
    let (status, body) = remove(&app, "it").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("applied").and_then(Value::as_bool), Some(true), "body: {body}");
}

/// Authority is the subtree, not a CEO role. A department head can remove the
/// department it heads, including its retained people.
#[tokio::test]
async fn a_head_removes_its_own_department_and_it_applies() {
    let (app, _dir, _company) = app(Some(person_identity("quant-head"))).await;
    let (status, body) = remove(&app, "quant").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("applied").and_then(Value::as_bool), Some(true), "body: {body}");
}

/// A head reaches its own subtree and nothing sideways. `quant-head` heads
/// `quant`; `it` is a sibling, so the deletion is refused and nothing is
/// written.
#[tokio::test]
async fn a_head_removing_a_siblings_department_is_refused() {
    let (app, _dir, _company) = app(Some(person_identity("quant-head"))).await;
    let (status, body) = remove(&app, "it").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-out-of-scope"),
        "body: {body}"
    );

    // AND THE DEPARTMENT SURVIVES. A refusal that returned the right code while
    // deleting the subtree anyway would pass the assertion above.
    let (status, body) = remove(&app, "it").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-out-of-scope"),
        "body: {body}"
    );
}

/// THE DEFECT. Core receives only a free-form actor string. Before the typed
/// route fence, a person from another company whose principal was absent from
/// this roster looked exactly like an operator and could remove any subtree.
#[tokio::test]
async fn a_person_from_another_company_with_an_unknown_principal_is_refused() {
    let outsider = identity(IdentityKind::Person, "outside-head", Some("another-company"));
    let (app, _dir, company) = app(Some(outsider)).await;
    let (status, body) = remove(&app, "it").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-company-mismatch"),
        "body: {body}"
    );

    let manifest =
        company.org_manifest_read().await.expect("manifest read").expect("manifest row").0;
    assert!(manifest.departments.contains_key("it"), "the refused call must write nothing");
}

/// A principal string is not an identity. An outside-company person named
/// `chief` must not inherit the target company's same-named CEO authority.
#[tokio::test]
async fn a_person_from_another_company_cannot_collide_with_the_target_ceo() {
    let outsider = identity(IdentityKind::Person, "chief", Some("another-company"));
    let (app, _dir, company) = app(Some(outsider)).await;
    let (status, body) = remove(&app, "it").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-company-mismatch"),
        "body: {body}"
    );
    let manifest =
        company.org_manifest_read().await.expect("manifest read").expect("manifest row").0;
    assert!(manifest.departments.contains_key("it"), "the name collision must write nothing");
}

/// Daemon-scoped principals name no person row and retain the unconditional arm
/// of the organization authority model.
#[tokio::test]
async fn daemon_scoped_principals_remove_a_department() {
    for (kind, principal) in [
        (IdentityKind::Operator, "operator"),
        (IdentityKind::Service, "actuator"),
        (IdentityKind::Channel, "operator-pane"),
    ] {
        let caller = identity(kind, principal, None);
        let (app, _dir, _company) = app(Some(caller)).await;
        let (status, body) = remove(&app, "it").await;
        assert_eq!(status, StatusCode::OK, "{principal}: {body}");
        assert_eq!(body.get("applied").and_then(Value::as_bool), Some(true), "{principal}: {body}");
    }
}

/// A missing target is a product-state refusal, not an identity denial. The
/// typed route fence leaves the established core answer intact.
#[tokio::test]
async fn an_unknown_department_keeps_its_product_refusal() {
    let (app, _dir, _company) = app(Some(person_identity("chief"))).await;
    let (status, body) = remove(&app, "missing").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("unknown-department"),
        "body: {body}"
    );
}

/// NO CALLER, NO ROUTE. The rollout arm that made absence mean "local trust"
/// is deleted: a request that proves no identity is refused `401
/// caller-unauthenticated` by the `Caller` extractor, and the subtree it asked
/// to delete is still there afterwards.
///
/// The second half is the load-bearing one. This route deletes a department
/// tree and offboards everyone under it, so a refusal that returned the right
/// status while doing the work anyway would satisfy the status assertion alone.
#[tokio::test]
async fn without_a_caller_the_route_is_401_and_removes_nothing() {
    let (app, _dir, _company) = app(None).await;
    let (status, body) = remove(&app, "it").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-unauthenticated"),
        "body: {body}"
    );

    // The department survives on THIS company database: the CEO, who may
    // remove it, still finds something to remove. Same router, same store —
    // a fresh fixture would prove nothing about what the refused call did.
    let ceo = app.clone().layer(Extension(person_identity("chief")));
    let (status, body) = remove(&ceo, "it").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("applied").and_then(Value::as_bool), Some(true), "body: {body}");
}

/// One minimal subscriber captures the route's structured fields without a
/// test-only logging dependency.
struct CapturingSubscriber(Arc<Mutex<Vec<String>>>);

struct FieldCapture(String);

impl tracing::field::Visit for FieldCapture {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.0, " {}={value:?}", field.name());
    }
}

impl tracing::Subscriber for CapturingSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut capture = FieldCapture(String::new());
        event.record(&mut capture);
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(capture.0);
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// Applied and refused removals name the authenticated caller, target and
/// outcome. The generic request log cannot answer those audit questions.
#[tokio::test(flavor = "current_thread")]
async fn removal_results_emit_structured_caller_and_target_audit_fields() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(CapturingSubscriber(Arc::clone(&log)));

    let _route_guard = ROUTE_TEST_LOCK.lock().await;
    tracing::callsite::rebuild_interest_cache();

    let (allowed, _allowed_dir, _allowed_company) = app(Some(person_identity("chief"))).await;
    let (status, body) = remove_unlocked(&allowed, "it").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (refused, _refused_dir, _refused_company) = app(Some(person_identity("quant-head"))).await;
    let (status, body) = remove_unlocked(&refused, "it").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");

    let lines = log.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).join("\n");
    assert!(lines.contains("event=\"org.department.remove_tree.applied\""), "{lines}");
    assert!(lines.contains("caller=chief"), "{lines}");
    assert!(lines.contains("department=it"), "{lines}");
    assert!(lines.contains("removed_departments=[\"it\"]"), "{lines}");
    assert!(lines.contains("event=\"org.department.remove_tree.refused\""), "{lines}");
    assert!(lines.contains("caller=quant-head"), "{lines}");
    assert!(lines.contains("code=\"caller-out-of-scope\""), "{lines}");
}
