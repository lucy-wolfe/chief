//! B3 sweep: the four ledger writes that carried a caller and never read one.
//!
//! the design record is the frame — authentication proves WHO IS
//! CALLING, and authorization is the ROUTE ASKING whether that caller may do
//! this. These four routes had the first and not the second:
//!
//! * `/v1/org/activity/agent-state` and `/v1/org/activity/command-status` take
//!   a `callerPersonId` whose own doc-comment reads "the person the trusted
//!   adapter authenticated. Never from a Pi payload" — and no adapter ever
//!   authenticated it. The field arrived from the same client that chose its
//!   value, so any caller could hold another person's automatic-settle lease
//!   open, or park an agent that was mid-turn, or read somebody else's pending
//!   handoffs.
//! * `/v1/org/event-journal/insert-if-absent` and `/v1/org/event-journal/prune`
//!   are DocStore-direct on the shared `org.sqlite` with NO live-company gate,
//!   which is deliberate — an exactly-once marker is a cross-producer primitive
//!   written before any company is "live". The consequence is that the body's
//!   `slug` alone chose whose journal was written or bulk-deleted.
//!
//! Every test here layers a REAL `CallerIdentity` onto the REAL router, because
//! that is the only shape that can tell a fixed route from a broken one: a test
//! that posts without an identity proves the change breaks nothing, never that
//! it works. Each route gets a positive and a refusal, and the positive is what
//! stops a route that refused everybody from satisfying the refusal alone.

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

const SLUG: &str = "ledger-write-caller";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/ledger-write-caller";

/// The COMPOSITE document key every route compares against. A display slug
/// fails the label match, and a harness that used one would agree with itself
/// and disagree with the daemon.
fn wire_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new(COMPANY_DIR))
}

/// The composite key of some OTHER company — what a cross-tenant caller's
/// identity is scoped to.
fn foreign_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new("/data/orgs/somebody-else"))
}

/// A PERSON identity for `principal`, scoped to `company` by composite key.
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

/// One company, from which a router can be built for ANY caller. Two callers
/// over ONE store is what a cross-tenant test needs: a harness that gave each
/// identity its own database would prove nothing about the rows the other one
/// can reach.
struct World {
    _dir: tempfile::TempDir,
    company: Arc<CompanyDb>,
    store: Arc<DocStore>,
}

async fn world() -> World {
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
    World { _dir: dir, company, store }
}

fn app_for(world: &World, caller: Option<CallerIdentity>) -> axum::Router {
    let router = router_with_supervision_live(
        Arc::clone(&world.store),
        1024 * 1024,
        Duration::from_secs(15),
        Some(SupervisionLiveSource::new(Arc::clone(&world.company), wire_key())),
    );
    match caller {
        Some(identity) => router.layer(Extension(identity)),
        None => router,
    }
}

/// The router a person of this company drives.
fn app_as(world: &World, principal: &str) -> axum::Router {
    app_for(world, Some(person_identity(principal, &wire_key())))
}

/// The router somebody from ANOTHER company drives, aimed at this one.
fn app_as_outsider(world: &World) -> axum::Router {
    app_for(world, Some(person_identity("outsider", &foreign_key())))
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

// --- /v1/org/activity/agent-state -----------------------------------------

async fn beat(app: &axum::Router, person: &str, working: bool) -> (StatusCode, Value) {
    post(
        app,
        "/v1/org/activity/agent-state",
        json!({ "slug": wire_key(), "callerPersonId": person, "working": working }),
    )
    .await
}

/// THE POSITIVE. A pane reports its own agent busy, which is the only call the
/// product makes on this route (`noteAgentActivityBeat` sends
/// `callerPersonId: context.personId`).
#[tokio::test]
async fn a_person_may_beat_its_own_agent_state() {
    let world = world().await;
    let (status, body) = beat(&app_as(&world, "chief"), "chief", true).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

/// THE DEFECT. `working: false` under somebody else's name is a request that
/// chiefd park an agent that may be mid-turn; `working: true` holds their
/// automatic-settle lease open forever. Both were free.
#[tokio::test]
async fn a_person_may_not_beat_another_persons_agent_state() {
    let world = world().await;
    let (status, body) = beat(&app_as(&world, "quant-head"), "chief", false).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(code(&body), Some("requester-identity-mismatch"), "body: {body}");
}

// --- /v1/org/activity/command-status ---------------------------------------

async fn command_status(app: &axum::Router, person: &str) -> (StatusCode, Value) {
    post(
        app,
        "/v1/org/activity/command-status",
        json!({ "slug": wire_key(), "callerPersonId": person }),
    )
    .await
}

/// THE POSITIVE. Reading your OWN pending handoffs is the whole verb, and it
/// still works.
#[tokio::test]
async fn a_person_may_read_its_own_command_status() {
    let world = world().await;
    let app = app_as(&world, "chief");
    // Move the activity ledger first: the read answers `absent` for a company
    // that has never recorded any activity at all, and that 404 would hide
    // whether the binding passed.
    let (status, body) = beat(&app, "chief", true).await;
    assert_eq!(status, StatusCode::OK, "beat body: {body}");

    let (status, body) = command_status(&app, "chief").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("personId").and_then(Value::as_str), Some("chief"), "body: {body}");
}

/// **AN OPTIONAL KEY IS ABSENT, NOT NULL — and the difference cost the
/// operator a working feature.**
///
/// `activity_command.rs` declares the wire shape as
/// `{ personId, pendingTransitions, activeTransitionId? }`. The route built
/// that body with `json!`, where an `Option::None` serializes as JSON `null`
/// — a PRESENT key with a null value. The one client
/// (`parseActivityCommandResult`) tests `!== undefined` and then demands a
/// non-empty string naming a pending transition, so `null` is a hard throw.
///
/// It was unreachable until 2026-08-24. `queueAutomaticParkCompaction` is this
/// route's only caller and it used to die at a 403 on a company-wide verb
/// BEFORE the status read; #1223 fixed that boundary and the wall simply moved
/// one step later — 12 `invalid active transition fence` rows across five
/// people in twenty minutes on a live box, with `auto-compact`
/// requests still at zero. And it fires in the COMMON case: a person with no
/// active transition is exactly the `None`.
///
/// This asserts KEY ABSENCE, not null-ness, because that is the contract —
/// `body.get("activeTransitionId")` returning `Some(Value::Null)` is the bug.
#[tokio::test]
async fn no_active_transition_omits_the_fence_key_entirely() {
    let world = world().await;
    let app = app_as(&world, "chief");
    let (status, body) = beat(&app, "chief", true).await;
    assert_eq!(status, StatusCode::OK, "beat body: {body}");

    let (status, body) = command_status(&app, "chief").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    // Non-vacuity: this person genuinely has no active transition, so the
    // absent key is the answer under test rather than an accident of setup.
    assert_eq!(
        body.get("pendingTransitions").and_then(Value::as_array).map(Vec::len),
        Some(0),
        "body: {body}"
    );
    assert!(
        body.get("activeTransitionId").is_none(),
        "an optional key with no value is ABSENT, never null: {body}"
    );
    // And the keys that are NOT optional are still there, so a fix that
    // dropped the wrong field would not pass this.
    assert_eq!(body.get("personId").and_then(Value::as_str), Some("chief"), "body: {body}");
}

/// Somebody else's pending transitions are somebody else's business.
#[tokio::test]
async fn a_person_may_not_read_another_persons_command_status() {
    let world = world().await;
    let (status, body) = command_status(&app_as(&world, "quant-head"), "chief").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(code(&body), Some("requester-identity-mismatch"), "body: {body}");
}

// --- /v1/org/event-journal/insert-if-absent --------------------------------

async fn insert_marker(app: &axum::Router, digest: &str) -> (StatusCode, Value) {
    post(
        app,
        "/v1/org/event-journal/insert-if-absent",
        json!({
            "slug": wire_key(),
            "keyDigest": digest,
            "id": "message-queued:1",
            "event": { "event": "message-queued" },
            "createdAtMs": 1_000,
        }),
    )
    .await
}

async fn read_marker(app: &axum::Router, digest: &str) -> Value {
    let (_status, body) =
        post(app, "/v1/org/event-journal/read", json!({ "slug": wire_key(), "keyDigest": digest }))
            .await;
    body
}

/// THE POSITIVE. Every organization event the intercom records passes through
/// this route (`RowStoresClient.insertEventOnceMarker`), so a gate that refused
/// a person writing its own company's journal would silence the product.
#[tokio::test]
async fn a_person_may_write_its_own_companys_event_marker() {
    let world = world().await;
    let (status, body) = insert_marker(&app_as(&world, "chief"), "digest-own").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("created").and_then(Value::as_bool), Some(true), "body: {body}");
}

/// THE DEFECT. There is no live-company gate on this route at all, so the
/// body's `slug` was the only thing that chose whose journal was written — and
/// a forged marker makes another company's exactly-once event silently
/// disappear, because the producer reads `created: false` as "already done".
#[tokio::test]
async fn a_person_may_not_write_another_companys_event_marker() {
    let world = world().await;
    let (status, body) = insert_marker(&app_as_outsider(&world), "digest-foreign").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(code(&body), Some("caller-company-mismatch"), "body: {body}");

    // AND NOTHING WAS WRITTEN. Read the same digest back through this company's
    // OWN caller: a refusal that returned the right code after inserting would
    // satisfy the assertion above.
    let found = read_marker(&app_as(&world, "chief"), "digest-foreign").await;
    assert_eq!(found.get("found").and_then(Value::as_bool), Some(false), "body: {found}");
}

// --- /v1/org/event-journal/prune -------------------------------------------

async fn prune(app: &axum::Router) -> (StatusCode, Value) {
    post(app, "/v1/org/event-journal/prune", json!({ "slug": wire_key(), "olderThanMs": 10_000 }))
        .await
}

/// THE POSITIVE, and it proves the delete really happens for the right caller:
/// the marker written first is counted as pruned.
#[tokio::test]
async fn a_person_may_prune_its_own_companys_markers() {
    let world = world().await;
    let app = app_as(&world, "chief");
    let (status, body) = insert_marker(&app, "digest-prune").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, body) = prune(&app).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("rowsAffected").and_then(Value::as_i64), Some(1), "body: {body}");
    let found = read_marker(&app, "digest-prune").await;
    assert_eq!(found.get("found").and_then(Value::as_bool), Some(false), "body: {found}");
}

/// The sharpest of the four. Prune DELETES in bulk, so a caller naming another
/// company erased that company's exactly-once history and let every one of its
/// events fire a second time.
#[tokio::test]
async fn a_person_may_not_prune_another_companys_markers() {
    let world = world().await;
    let owner = app_as(&world, "chief");
    let (status, body) = insert_marker(&owner, "digest-survives").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, body) = prune(&app_as_outsider(&world)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(code(&body), Some("caller-company-mismatch"), "body: {body}");

    // AND THE MARKER SURVIVES — the same store, read through the owner. This is
    // why the harness builds two routers over ONE company: separate databases
    // would make this assertion vacuous.
    let found = read_marker(&owner, "digest-survives").await;
    assert_eq!(found.get("found").and_then(Value::as_bool), Some(true), "body: {found}");
}

// --- no credential at all ---------------------------------------------------

/// NO CALLER, NO LEDGER WRITE — on all four routes, not just the one that is
/// convenient to check. The rollout stage that made absence mean "enforcement
/// is off" is deleted: every one of these routes takes a `Caller`, so a request
/// carrying no identity is answered `401 caller-unauthenticated` before the
/// handler body starts, and nothing reaches the ledger.
#[tokio::test]
async fn without_a_caller_all_four_routes_are_401_and_write_nothing() {
    let world = world().await;
    let app = app_for(&world, None);

    for (label, (status, body)) in [
        ("agent-state", beat(&app, "chief", true).await),
        ("command-status", command_status(&app, "chief").await),
        ("insert", insert_marker(&app, "digest-no-caller").await),
        ("prune", prune(&app).await),
    ] {
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{label} body: {body}");
        assert_eq!(code(&body), Some("caller-unauthenticated"), "{label} body: {body}");
    }

    // AND NOTHING LANDED. Read the marker back through a router that CAN read
    // it — the same store, so an insert that had gone through would be found.
    let owner = app_as(&world, "chief");
    let found = read_marker(&owner, "digest-no-caller").await;
    assert_eq!(found.get("found").and_then(Value::as_bool), Some(false), "body: {found}");
}
