//! The destructive PERSON routes read their caller — proved WITH one present.
//!
//! `/v1/org/person/{shutdown,start,bench,bench-lifecycle,recall,transfer,
//! appoint-head,replace-head-and-offboard}` each took their target out of the
//! request body, handed core `String::new()` as the actor (or, for `transfer`,
//! an unbound body string), and asked nothing about who was calling. Any caller
//! that reached them could stop, bench, start, recall, move, promote or replace
//! anybody in the company, and the staffing ledger recorded the author as the
//! empty string.
//!
//! # Why this file exists beside the core tests
//!
//! `org_ops`'s own tests cover the RULE — the CEO reaches every subtree, a
//! sibling head reaches none of another's, an actor naming no person row is not
//! judged. What they cannot cover is the WIRING: core can only refuse an actor
//! the route actually gives it, and until now every one of these routes gave it
//! nothing. A test that exercises the route without a caller extension proves
//! the change breaks nothing, never that it works.
//!
//! Each route below therefore gets BOTH a refusal with a real `CallerIdentity`
//! present and the positive case, because a route that refused everybody would
//! satisfy the refusals on its own.
//!
//! The manifest is `northstar`: `ceo` heads the company root, `quant-head`
//! heads `quant` with `signal-researcher` under it, and `it-head` heads the
//! sibling `it`. So `it-head` acting on `signal-researcher` is the sideways
//! reach the tree forbids.

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

const SLUG: &str = "person-verbs-caller";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/person-verbs-caller";

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

/// The answer to a request that proved no identity at all. Distinct from
/// `assert_out_of_scope`: that one is a real principal reaching past its
/// subtree, this one never reached a handler.
fn assert_unauthenticated(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-unauthenticated"),
        "body: {body}"
    );
}

fn assert_applied(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("applied").and_then(Value::as_bool), Some(true), "body: {body}");
}

// ---- shutdown -------------------------------------------------------------

fn shutdown_body() -> Value {
    json!({ "slug": wire_key(), "personId": "signal-researcher", "kind": "settle" })
}

#[tokio::test]
async fn a_sibling_head_cannot_shut_down_somebody_elses_worker() {
    let h = app(Some(person_identity("it-head"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/shutdown", shutdown_body()).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_shuts_down_a_worker_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/shutdown", shutdown_body()).await;
    assert_applied(status, &body);
}

/// NO CALLER, NO SHUTDOWN. The rollout arm that made absence mean "local
/// trust" is deleted: the request is `401 caller-unauthenticated` and the
/// person is left running, proved by the CEO's identical call — which still
/// finds a worker to shut down — on the SAME store.
#[tokio::test]
async fn without_a_caller_shutdown_is_401_and_stops_nobody() {
    let h = app(None).await;
    let (status, body) = post(&h.router, "/v1/org/person/shutdown", shutdown_body()).await;
    assert_unauthenticated(status, &body);

    let ceo = h.router.clone().layer(Extension(person_identity("chief")));
    let (status, body) = post(&ceo, "/v1/org/person/shutdown", shutdown_body()).await;
    assert_applied(status, &body);
}

// ---- start ----------------------------------------------------------------

fn start_body() -> Value {
    json!({ "slug": wire_key(), "personId": "signal-researcher" })
}

#[tokio::test]
async fn a_sibling_head_cannot_start_somebody_elses_worker() {
    let h = app(Some(person_identity("it-head"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/start", start_body()).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_starts_a_worker_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/start", start_body()).await;
    assert_applied(status, &body);
}

// ---- wake -----------------------------------------------------------------
//
// The rail's click-to-wake. It is the FIRST write the operator's sidebar makes,
// so the fence is proved here from both sides rather than assumed from the
// routes beside it.

fn wake_body() -> Value {
    json!({ "slug": wire_key(), "personId": "signal-researcher" })
}

/// THE SUBTREE FENCE, at the route. `it-head` heads the sibling unit and
/// manages nobody in `quant`, so the wake never reaches the writer at all —
/// `403 caller-out-of-scope`, from `require_person_scope`, ahead of any durable
/// read done on that caller's behalf.
///
/// It is 403 rather than the 422 `actor-out-of-scope` the start verb answers
/// because this route asks the question BEFORE the transaction. Both are the
/// same subtree question; neither is a role gate.
#[tokio::test]
async fn a_sibling_head_cannot_wake_somebody_elses_worker() {
    let h = app(Some(person_identity("it-head"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/wake", wake_body()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-out-of-scope"),
        "body: {body}"
    );
}

/// And the positive case, or the refusal above would be satisfied by a route
/// that refused everybody.
#[tokio::test]
async fn the_ceo_wakes_a_worker_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/wake", wake_body()).await;
    assert_applied(status, &body);
}

/// The operator's own bearer is a NON-PERSON principal with unconditional
/// scope, which is what the rail presents. It passes, and it must: the operator
/// is not somebody's report.
#[tokio::test]
async fn the_operator_bearer_wakes_anybody_because_it_names_no_person_row() {
    let h = app(Some(daemon_identity(IdentityKind::Operator, "operator"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/wake", wake_body()).await;
    assert_applied(status, &body);
}

#[tokio::test]
async fn without_a_caller_a_wake_is_401_and_grants_nothing() {
    let h = app(None).await;
    let (status, body) = post(&h.router, "/v1/org/person/wake", wake_body()).await;
    assert_unauthenticated(status, &body);
}

/// `deny_unknown_fields` APPLIES, exactly like the start/recall/replace request
/// structs beside it. A field this daemon does not model is a caller believing
/// something about the verb that is not true, and accepting it silently is how
/// a newer client's option gets dropped with nobody finding out.
#[tokio::test]
async fn a_wake_carrying_a_field_the_daemon_does_not_model_is_refused() {
    let h = app(Some(person_identity("chief"))).await;
    let mut body = wake_body();
    body["force"] = json!(true);
    let (status, answer) = post(&h.router, "/v1/org/person/wake", body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {answer}");
}

// ---- bench ----------------------------------------------------------------

fn bench_body() -> Value {
    json!({ "slug": wire_key(), "personId": "signal-researcher" })
}

#[tokio::test]
async fn a_sibling_head_cannot_bench_somebody_elses_worker() {
    let h = app(Some(person_identity("it-head"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/bench", bench_body()).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_benches_a_worker_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/bench", bench_body()).await;
    assert_applied(status, &body);
}

// ---- bench-lifecycle ------------------------------------------------------

#[tokio::test]
async fn a_sibling_head_cannot_bench_lifecycle_somebody_elses_worker() {
    let h = app(Some(person_identity("it-head"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/bench-lifecycle", bench_body()).await;
    assert_out_of_scope(status, &body);
}

/// THE POSITIVE CASE for the reflected lifecycle, and it asserts what this
/// harness can honestly observe. The committed bench registers an in-memory
/// convergence wait, and this router carries no `bench_completion` registry, so
/// a SUCCESSFUL guard passage answers `503 bench-convergence-timeout` rather
/// than `200`. What matters here is that the CEO gets PAST the authorization
/// gate — anything else would let a route that refused everybody satisfy the
/// refusal above on its own.
#[tokio::test]
async fn the_ceo_passes_the_bench_lifecycle_gate() {
    let h = app(Some(person_identity("chief"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/bench-lifecycle", bench_body()).await;
    assert_ne!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_ne!(
        body.get("code").and_then(Value::as_str),
        Some("actor-out-of-scope"),
        "body: {body}"
    );
}

// ---- recall ---------------------------------------------------------------

/// Recall needs a benched person, and the bench is done through the company
/// directly with an actor that names nobody, so the only judged call in each
/// test is the recall under test.
async fn bench_directly(company: &CompanyDb) {
    let outcome = company
        .bench_person("signal-researcher".to_owned(), "t0".to_owned(), String::new())
        .await
        .expect("bench");
    assert!(
        matches!(outcome, chiefd_core::store::org_ops::BenchOutcome::Applied),
        "setup bench must apply: {outcome:?}"
    );
}

#[tokio::test]
async fn a_sibling_head_cannot_recall_somebody_elses_worker() {
    let h = app(Some(person_identity("it-head"))).await;
    bench_directly(&h.company).await;
    let body = json!({ "slug": wire_key(), "personId": "signal-researcher" });
    let (status, body) = post(&h.router, "/v1/org/person/recall", body).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_recalls_a_benched_worker_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    bench_directly(&h.company).await;
    let body = json!({ "slug": wire_key(), "personId": "signal-researcher" });
    let (status, body) = post(&h.router, "/v1/org/person/recall", body).await;
    assert_applied(status, &body);
}

// ---- transfer -------------------------------------------------------------

fn transfer_body() -> Value {
    json!({ "slug": wire_key(), "personId": "signal-researcher", "destinationId": "it" })
}

/// BOTH DEPARTMENTS ARE ASKED. `it-head` manages the DESTINATION and not the
/// source, so it may not pull a worker out of `quant`.
#[tokio::test]
async fn a_head_that_manages_only_the_destination_cannot_transfer() {
    let h = app(Some(person_identity("it-head"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/transfer", transfer_body()).await;
    assert_out_of_scope(status, &body);
}

/// The mirror half: `quant-head` manages the SOURCE and not the destination, so
/// it may not push its own people into a unit it has no authority over.
#[tokio::test]
async fn a_head_that_manages_only_the_source_cannot_transfer() {
    let h = app(Some(person_identity("quant-head"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/transfer", transfer_body()).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_transfers_a_worker_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/transfer", transfer_body()).await;
    assert_applied(status, &body);
}

/// `transfer` is the ONE route in this family whose body carries an `actor`,
/// and the caller's principal OVERWRITES it rather than being reconciled with
/// it. `bind_caller` is deliberately not used: `actor` is free-form audit prose
/// (`operator`, `op`, the empty string) and it is optional, so
/// `bind_requester_to_caller` would read an omitted field as a claim on the
/// operator route and refuse every ordinary person-authenticated transfer.
///
/// The guarantee is stronger without it. A declared actor with authority cannot
/// lend that authority to a caller who has none: `it-head` naming `ceo` is
/// still refused, because the principal core judges is the caller's.
#[tokio::test]
async fn a_declared_actor_cannot_lend_its_authority_to_the_caller() {
    let h = app(Some(person_identity("it-head"))).await;
    let mut body = transfer_body();
    body["actor"] = json!("chief");
    let (status, body) = post(&h.router, "/v1/org/person/transfer", body).await;
    assert_out_of_scope(status, &body);
}

/// THE MIRROR, AND THE STRONGER HALF: a declared actor is not a credential.
/// With no caller the body names `ceo` — the one principal that could transfer
/// anybody — and the request is still `401 caller-unauthenticated`, because the
/// `Caller` extractor refuses before the handler reads a single body field.
/// A body value that could stand in for an identity would make every fence in
/// this file self-declared.
#[tokio::test]
async fn a_declared_actor_is_not_a_credential() {
    let h = app(None).await;
    let mut body = transfer_body();
    body["actor"] = json!("chief");
    let (status, body) = post(&h.router, "/v1/org/person/transfer", body).await;
    assert_unauthenticated(status, &body);
}

// ---- appoint-head ---------------------------------------------------------

fn appoint_body() -> Value {
    json!({
        "slug": wire_key(),
        "departmentId": "quant",
        "successorPersonId": "signal-researcher",
    })
}

#[tokio::test]
async fn a_sibling_head_cannot_appoint_another_departments_head() {
    let h = app(Some(person_identity("it-head"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/appoint-head", appoint_body()).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_appoints_a_head_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    let (status, body) = post(&h.router, "/v1/org/person/appoint-head", appoint_body()).await;
    assert_applied(status, &body);
}

// ---- replace-head-and-offboard --------------------------------------------

fn replace_body() -> Value {
    json!({
        "slug": wire_key(),
        "headPersonId": "quant-head",
        "successorPersonId": "signal-researcher",
    })
}

#[tokio::test]
async fn a_sibling_head_cannot_replace_another_departments_head() {
    let h = app(Some(person_identity("it-head"))).await;
    let (status, body) =
        post(&h.router, "/v1/org/person/replace-head-and-offboard", replace_body()).await;
    assert_out_of_scope(status, &body);
}

#[tokio::test]
async fn the_ceo_replaces_a_head_and_it_applies() {
    let h = app(Some(person_identity("chief"))).await;
    let (status, body) =
        post(&h.router, "/v1/org/person/replace-head-and-offboard", replace_body()).await;
    assert_applied(status, &body);
}

// ---- daemon-scoped identities, and what happens after A6 -------------------

/// A DAEMON-SCOPED IDENTITY KEEPS ITS UNCONDITIONAL SCOPE, and this is the
/// answer to "what do these guards do when the caller is not a person".
///
/// The fence is `person_manages_department`, which resolves a PERSON, and it is
/// reached only through `actor_names_a_person`. An operator, a service and a
/// channel each carry a principal that names no person row — `operator`,
/// `service`, `channel`, none of them a company member — so the predicate
/// answers false and core does not judge the actor. Exactly the rule that lets
/// `op` and the empty string through, applied to the identities that will
/// actually exist.
///
/// This matters BEFORE anything exercises it. Today no caller presents a
/// credential, so the behaviour is invisible; after A6 the middleware always
/// inserts an identity and `chief-cli` presents an operator bearer. If these
/// fences resolved a person and found none, that merge would begin failing with
/// a dozen routes' worth of cause to bisect. They do not, and this is what
/// keeps it true — including `Channel`, the kind nobody thinks of.
#[tokio::test]
async fn a_daemon_scoped_identity_keeps_its_unconditional_scope() {
    for (kind, principal) in [
        (IdentityKind::Operator, "operator"),
        (IdentityKind::Service, "service"),
        (IdentityKind::Channel, "channel"),
    ] {
        let h = app(Some(daemon_identity(kind, principal))).await;
        let (status, body) = post(&h.router, "/v1/org/person/bench", bench_body()).await;
        assert_applied(status, &body);

        let h = app(Some(daemon_identity(kind, principal))).await;
        let (status, body) = post(&h.router, "/v1/org/person/shutdown", shutdown_body()).await;
        assert_applied(status, &body);

        // `/v1/org/person/start` is named EXPLICITLY because `chief` is
        // its caller, at a moment in a company's life when the operator is the
        // only principal that can possibly exist. A person-resolving fence here
        // would not be a hardening; it would be a company that cannot be
        // created.
        let h = app(Some(daemon_identity(kind, principal))).await;
        let (status, body) = post(&h.router, "/v1/org/person/start", start_body()).await;
        assert_applied(status, &body);
    }
}
