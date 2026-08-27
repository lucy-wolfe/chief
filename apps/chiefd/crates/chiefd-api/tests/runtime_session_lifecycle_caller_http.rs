//! B2: the runtime and session-lifecycle routes read their caller — proved WITH
//! one present.
//!
//! Four routes in `docstore/router.rs` carried an authenticated identity that
//! nothing consulted. Three of them write COMPANY-WIDE state — the launch
//! intent and the runtime row — and one names PERSON TARGETS. So they take the
//! two fences that match those subjects, and neither is a job title:
//!
//! chief-home-is-cwd §4c removed a fourth company-wide member,
//! `runtime/prepare-ceo-only`, with the daemon-side CEO boot.
//!
//! * `session-maintenance/reconcile-parked` → `person_is_in_scope` over every
//!   `parkedPersonIds` entry. It reconciles OTHER PEOPLE's parked maintenance,
//!   so self-identity — which the six execution verbs beside it take, because
//!   each of those names itself — would be the wrong fence here.
//! * `launch-intent/clear`, `runtime/clear`, `runtime/publish` →
//!   `department_is_in_scope` over the ROOT department. The subject is the
//!   company, so the department the write reaches is the root, and only
//!   somebody who heads the root passes.
//!
//! # Why this file exists
//!
//! These routes carried an identity nothing read, so every test of them ran
//! without a `CallerIdentity` and could only ever prove the change broke
//! nothing, never that it worked. These run the real router with an identity
//! layered on, which is the only shape that can tell the difference.
//!
//! Each route gets three: a refusal with a stable code, the positive case, and
//! the no-caller case. The positive is what keeps the refusal honest — a route
//! that refused everybody would satisfy the negative on its own.
//!
//! The no-caller case is no longer an ALLOWING arm. It was, while the rollout
//! stage that read absence as local trust was in place; that stage is deleted,
//! so absence is now `401 caller-unauthenticated` from the `Caller` extractor
//! and the tests below say so. Do not confuse it with the daemon-scoped case:
//! an Operator, Service or Channel credential is a real principal whose scope
//! is unconditional, and it still passes.

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

const SLUG: &str = "b2-runtime-caller";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/b2-runtime-caller";
const AT: &str = "2026-08-13T00:00:00.000Z";

/// The COMPOSITE document key the handlers compare against. A display slug fails
/// the label match, so a harness that used one would agree with itself and
/// disagree with the daemon.
fn wire_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new(COMPANY_DIR))
}

fn identity(kind: IdentityKind, principal: &str, company: Option<String>) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: format!("id-{principal}"),
        principal: principal.to_owned(),
        kind,
        company_slug: company,
        pubkey: Some("test-key".to_owned()),
        fingerprint: format!("fp-{principal}"),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

/// A PERSON identity for `principal`, scoped to this company by that same
/// composite key.
fn person(principal: &str) -> CallerIdentity {
    identity(IdentityKind::Person, principal, Some(wire_key()))
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
        Some(caller) => router.layer(Extension(caller)),
        None => router,
    };
    (router, dir)
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

fn code(body: &Value) -> Option<&str> {
    body.get("code").and_then(Value::as_str)
}

/// A minimal but real runtime state document.
fn runtime_doc() -> String {
    json!({
        "version": 1,
        "observedAt": AT,
        "socketName": "default",
        "status": "running",
        "processHandles": {}
    })
    .to_string()
}

// --- reconcile-parked: scope over the people it NAMES ----------------------

/// THE POSITIVE CASE. `quant-head` heads `quant` and `signal-researcher` lives
/// there, so the reconcile is inside its subtree and applies.
#[tokio::test]
async fn a_head_reconciles_parked_maintenance_for_its_own_report() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = post(
        &app,
        "/v1/org/session-maintenance/reconcile-parked",
        json!({ "slug": wire_key(), "parkedPersonIds": ["signal-researcher"] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

/// Nothing reaches sideways. `it-head` is a peer, not a report, so naming it is
/// refused with a stable code.
#[tokio::test]
async fn reconcile_parked_refuses_a_person_outside_the_callers_subtree() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = post(
        &app,
        "/v1/org/session-maintenance/reconcile-parked",
        json!({ "slug": wire_key(), "parkedPersonIds": ["it-head"] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(code(&body), Some("caller-out-of-scope"), "body: {body}");
}

/// ONE unreachable target refuses the WHOLE request. A fence that let the
/// reachable half through would leave the caller acting on somebody it does not
/// manage.
#[tokio::test]
async fn reconcile_parked_refuses_a_mixed_list_whole() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = post(
        &app,
        "/v1/org/session-maintenance/reconcile-parked",
        json!({ "slug": wire_key(), "parkedPersonIds": ["signal-researcher", "it-head"] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(code(&body), Some("caller-out-of-scope"), "body: {body}");
}

/// NO CALLER, NO RECONCILE. The rollout arm that made absence mean "local
/// trust" is deleted, so a request naming a parked person it has no standing
/// to touch is refused before the handler reads the list at all.
#[tokio::test]
async fn reconcile_parked_without_a_caller_is_401() {
    let (app, _dir) = app(None).await;
    let (status, body) = post(
        &app,
        "/v1/org/session-maintenance/reconcile-parked",
        json!({ "slug": wire_key(), "parkedPersonIds": ["it-head"] }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(code(&body), Some("caller-unauthenticated"), "body: {body}");
}

// --- the three company-wide writes -----------------------------------------

/// THE POSITIVE CASES. The CEO heads the root, so every company-wide write
/// applies. Without these three the refusals below would be satisfied by a
/// route that simply refused everybody.
#[tokio::test]
async fn the_ceo_may_make_every_company_wide_runtime_write() {
    let (app, _dir) = app(Some(person("chief"))).await;

    let (status, body) =
        post(&app, "/v1/org/launch-intent/clear", json!({ "slug": wire_key(), "at": AT })).await;
    assert_eq!(status, StatusCode::OK, "launch-intent/clear: {body}");

    let (status, body) =
        post(&app, "/v1/org/runtime/clear", json!({ "slug": wire_key(), "at": AT })).await;
    assert_eq!(status, StatusCode::OK, "runtime/clear: {body}");

    let (status, body) =
        post(&app, "/v1/org/runtime/publish", json!({ "slug": wire_key(), "doc": runtime_doc() }))
            .await;
    assert_eq!(status, StatusCode::OK, "runtime/publish: {body}");
    assert_eq!(body.get("applied").and_then(Value::as_bool), Some(true), "{body}");
}

/// A head reaches its own subtree and no further. `quant-head` heads `quant`,
/// never the root, so every company-wide write is refused with a stable code —
/// one assertion per ROUTE, because each carries its own fence call.
#[tokio::test]
async fn a_head_may_not_make_a_company_wide_runtime_write() {
    let (app, _dir) = app(Some(person("quant-head"))).await;

    for (path, body) in [
        ("/v1/org/launch-intent/clear", json!({ "slug": wire_key(), "at": AT })),
        ("/v1/org/runtime/clear", json!({ "slug": wire_key(), "at": AT })),
        ("/v1/org/runtime/publish", json!({ "slug": wire_key(), "doc": runtime_doc() })),
    ] {
        let (status, out) = post(&app, path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {out}");
        assert_eq!(code(&out), Some("caller-out-of-company-scope"), "{path}: {out}");
    }
}

/// A worker heads nothing, so the same three refuse — and this is the case that
/// shows the fence is the SUBTREE and not a title: `signal-researcher` is
/// refused for the same reason `quant-head` is, one level further out.
#[tokio::test]
async fn a_worker_may_not_make_a_company_wide_runtime_write() {
    let (app, _dir) = app(Some(person("signal-researcher"))).await;
    let (status, body) =
        post(&app, "/v1/org/runtime/clear", json!({ "slug": wire_key(), "at": AT })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(code(&body), Some("caller-out-of-company-scope"), "body: {body}");
}

/// A person identity is company-scoped. Even a CEO — of ANOTHER company — is
/// refused before any scope question is asked.
#[tokio::test]
async fn a_person_of_another_company_is_refused_before_the_scope_question() {
    let foreign = identity(IdentityKind::Person, "chief", Some("northstar@/data/orgs".to_owned()));
    let (app, _dir) = app(Some(foreign)).await;
    let (status, body) =
        post(&app, "/v1/org/runtime/clear", json!({ "slug": wire_key(), "at": AT })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(code(&body), Some("caller-company-mismatch"), "body: {body}");
}

/// THE FENCE KEYS ON THE PRINCIPAL, NOT ON THE KIND. A `channel` identity is
/// attested server-side and carries no key of its own, and `identities`' schema
/// comment says two identities may share one principal — `operator-pane` beside
/// the operator's keypair, and `pi-pane`, which is a PERSON's pane. If the fence
/// admitted on kind, attesting a pane channel for `quant-head` would hand a head
/// unconditional scope over the whole company: a route to widening, obtained by
/// getting a channel attested rather than by heading anything.
///
/// So a credential whose principal NAMES A PERSON ROW is fenced as that person,
/// whatever kind it is. `quant-head` heads `quant` and not the root, so the
/// company-wide write is refused exactly as the person credential would be.
#[tokio::test]
async fn a_channel_attested_as_a_person_is_fenced_as_that_person() {
    for kind in [IdentityKind::Channel, IdentityKind::Service, IdentityKind::Operator] {
        let (app, _dir) = app(Some(identity(kind, "quant-head", None))).await;
        let (status, body) =
            post(&app, "/v1/org/runtime/clear", json!({ "slug": wire_key(), "at": AT })).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind:?}: {body}");
        assert_eq!(code(&body), Some("caller-out-of-company-scope"), "{kind:?}: {body}");
    }
}

/// The same rule from the other side: a channel attested as the CEO reaches
/// what the CEO reaches, so keying on the principal neither widens nor narrows
/// anybody — it just stops the kind from being the answer.
#[tokio::test]
async fn a_channel_attested_as_the_ceo_reaches_what_the_ceo_reaches() {
    let (app, _dir) = app(Some(identity(IdentityKind::Channel, "chief", None))).await;
    let (status, body) =
        post(&app, "/v1/org/runtime/clear", json!({ "slug": wire_key(), "at": AT })).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

/// THE OPERATOR CLI AND THE ACTUATOR KEEP WORKING. `chief-cli` posts
/// `runtime/clear` and `launch-intent/clear` from
/// Rust on a daemon-scoped credential whose principal names NO person row,
/// which `control_authority` defines as unconditional scope. A fence that
/// refused these would break the front door.
#[tokio::test]
async fn a_daemon_scoped_identity_keeps_its_unconditional_scope() {
    for kind in [IdentityKind::Operator, IdentityKind::Service, IdentityKind::Channel] {
        let (app, _dir) = app(Some(identity(kind, "operator", None))).await;
        let (status, body) =
            post(&app, "/v1/org/runtime/clear", json!({ "slug": wire_key(), "at": AT })).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");

        let (status, body) = post(
            &app,
            "/v1/org/session-maintenance/reconcile-parked",
            json!({ "slug": wire_key(), "parkedPersonIds": ["it-head"] }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
    }
}

/// The person fence keys on the principal too. A channel attested as
/// `quant-head` reconciles its own report and not a peer — identical to the
/// person credential, which is the whole point of not asking the kind.
#[tokio::test]
async fn a_channel_attested_as_a_person_gets_that_persons_subtree_and_no_more() {
    let (app, _dir) = app(Some(identity(IdentityKind::Channel, "quant-head", None))).await;

    let (status, body) = post(
        &app,
        "/v1/org/session-maintenance/reconcile-parked",
        json!({ "slug": wire_key(), "parkedPersonIds": ["signal-researcher"] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "own report: {body}");

    let (status, body) = post(
        &app,
        "/v1/org/session-maintenance/reconcile-parked",
        json!({ "slug": wire_key(), "parkedPersonIds": ["it-head"] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "peer: {body}");
    assert_eq!(code(&body), Some("caller-out-of-scope"), "peer: {body}");
}

/// NO CALLER, NONE OF THE THREE. Each of these rewrites the whole company's
/// runtime state, so all three are checked rather than one standing in for the
/// rest: a single route that kept the deleted "absence is local trust" arm
/// would be a company-wide write available to anybody who could reach the
/// socket.
#[tokio::test]
async fn without_a_caller_every_company_wide_write_is_401() {
    let (app, _dir) = app(None).await;

    for (label, path, body) in [
        (
            "launch-intent/clear",
            "/v1/org/launch-intent/clear",
            json!({ "slug": wire_key(), "at": AT }),
        ),
        ("runtime/clear", "/v1/org/runtime/clear", json!({ "slug": wire_key(), "at": AT })),
        (
            "runtime/publish",
            "/v1/org/runtime/publish",
            json!({ "slug": wire_key(), "doc": runtime_doc() }),
        ),
    ] {
        let (status, body) = post(&app, path, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{label}: {body}");
        assert_eq!(code(&body), Some("caller-unauthenticated"), "{label}: {body}");
    }
}

/// The fence runs AFTER the own-company filter, so a foreign slug is still the
/// 404 it always was rather than becoming a 403 that leaks whether the company
/// exists.
#[tokio::test]
async fn a_foreign_slug_is_still_unknown_company_not_a_refusal() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) =
        post(&app, "/v1/org/runtime/clear", json!({ "slug": "foreign@company", "at": AT })).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(code(&body), Some("unknown-company"), "body: {body}");
}
