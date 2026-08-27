//! `/v1/org/mailbox/delta` — WHOSE MAILBOX is not WHO IS ASKING.
//!
//! This is the route B3 was blocked on, and the wiring is the half core's own
//! tests cannot prove: `mailbox_rows`'s tests cover the RULE, but core can only
//! judge an actor the route actually gives it, and until now the route gave it
//! nothing at all. A test that posts without an identity therefore proves the
//! change breaks nothing, never that it works.
//!
//! `personId` names whose mailbox is being written, and the product calls this
//! route in BOTH directions:
//!
//! * `publishMailboxEnvelope` sends `personId = recipient` — somebody else's
//!   mailbox — with `fromPersonId = context.personId`, the sender.
//! * `settleMailboxEntry` / `settleMailboxBatch` send `personId =
//!   context.personId` — the caller's own mailbox — to move a row into a
//!   terminal state.
//!
//! So a person-binding on `personId` would refuse EVERY message the product
//! sends. The two tests below are those two directions, and they are the
//! load-bearing ones: without them a route that refused everybody would satisfy
//! the forgery test on its own.

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

const SLUG: &str = "mailbox-delta-caller";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/mailbox-delta-caller";

/// The COMPOSITE document key the handler compares against — a display slug
/// fails the label match, and a harness that used one would agree with itself
/// and disagree with the daemon.
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

/// One company, from which a router can be built for ANY caller. The delivery
/// direction and the drain direction are two different people acting on the
/// same mailbox, so a harness that could hold only one identity could not
/// express the product's actual sequence.
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

/// The router a named person drives.
fn app_as(world: &World, principal: &str) -> axum::Router {
    app_for(world, Some(person_identity(principal)))
}

/// One wire entry, exactly the shape `mailboxUpsertEntry` builds: the envelope
/// flattened onto the entry, plus `person`, `state` and `updatedAt`.
fn wire_entry(id: &str, from: &str, recipient: &str, state: &str) -> Value {
    json!({
        "schemaVersion": 1,
        "id": id,
        "organization": SLUG,
        "fromPersonId": from,
        "to": recipient,
        "recipients": [recipient],
        "body": "the message body",
        "urgency": "normal",
        "createdAt": "2026-08-13T00:00:00.000Z",
        "person": recipient,
        "state": state,
        "updatedAt": 1_785_000_000_000_i64,
    })
}

async fn delta(
    app: &axum::Router,
    person: &str,
    upserts: Vec<Value>,
    deletes: Vec<&str>,
) -> (StatusCode, Value) {
    let body = json!({
        "slug": wire_key(),
        "personId": person,
        "upserts": Value::Array(upserts).to_string(),
        "deletes": deletes,
        "at": "2026-08-13T00:00:01.000Z",
    });
    let request = Request::builder()
        .method("POST")
        .uri("/v1/org/mailbox/delta")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn read_person(app: &axum::Router, person: &str) -> Value {
    let body = json!({ "slug": wire_key(), "personId": person });
    let request = Request::builder()
        .method("POST")
        .uri("/v1/org/mailbox/read-person")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let wire: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mailbox = wire.get("mailbox").and_then(Value::as_str).unwrap_or("{\"entries\":[]}");
    serde_json::from_str(mailbox).expect("mailbox snapshot")
}

fn entry_count(snapshot: &Value) -> usize {
    snapshot.get("entries").and_then(Value::as_array).map_or(0, Vec::len)
}

fn code(body: &Value) -> Option<&str> {
    body.get("code").and_then(Value::as_str)
}

/// THE FIRST PRODUCT DIRECTION — a real delivery. `quant-head` messages `ceo`:
/// `personId` is the RECIPIENT and the envelope is from the caller. Every
/// `org_message` the product sends looks exactly like this, so a rule that
/// refused it would silence the intercom.
#[tokio::test]
async fn a_person_may_deliver_a_message_from_itself_into_another_mailbox() {
    let world = world().await;
    let app = app_as(&world, "quant-head");
    let (status, body) =
        delta(&app, "chief", vec![wire_entry("m-1", "quant-head", "chief", "pending")], vec![])
            .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("applied").and_then(Value::as_bool), Some(true), "body: {body}");
    assert_eq!(entry_count(&read_person(&app, "chief").await), 1);
}

/// THE SECOND PRODUCT DIRECTION — consumption, in the product's real sequence:
/// `quant-head` delivers, then `ceo` drains. `settleMailboxEntry` reads the
/// envelope back, changes only `state`, and posts it again, so the sender it
/// carries is the sender already stored. This is why `personId` cannot simply
/// be bound to the caller.
#[tokio::test]
async fn a_person_may_settle_and_delete_a_row_it_already_holds() {
    let world = world().await;
    let sender = app_as(&world, "quant-head");
    let (status, body) =
        delta(&sender, "chief", vec![wire_entry("m-1", "quant-head", "chief", "pending")], vec![])
            .await;
    assert_eq!(status, StatusCode::OK, "delivery body: {body}");

    let recipient = app_as(&world, "chief");
    let (status, body) = delta(
        &recipient,
        "chief",
        vec![wire_entry("m-1", "quant-head", "chief", "accepted")],
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settle body: {body}");

    let (status, body) = delta(&recipient, "chief", vec![], vec!["m-1@chief"]).await;
    assert_eq!(status, StatusCode::OK, "delete body: {body}");
    assert_eq!(entry_count(&read_person(&recipient, "chief").await), 0);
}

/// THE FORGERY. `quant-head` writes into the CEO's mailbox a message claiming
/// to be from `it-head`. The recipient renders `fromPersonId` as the author, so
/// unbound this puts words in one person's mouth inside a third person's inbox —
/// the quietest write in the product, and the reason this route is judged per
/// entry rather than per request.
#[tokio::test]
async fn a_person_may_not_deliver_a_message_it_did_not_send() {
    let world = world().await;
    let app = app_as(&world, "quant-head");
    let (status, body) =
        delta(&app, "chief", vec![wire_entry("forged", "it-head", "chief", "pending")], vec![])
            .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(code(&body), Some("mailbox-delta-not-a-delivery"), "body: {body}");
    // AND NOTHING LANDED.
    assert_eq!(entry_count(&read_person(&app, "chief").await), 0);
}

/// SELF-FORGERY IS NOT HARMLESS. `ceo` mints a NEW row in its OWN mailbox
/// attributed to `quant-head`. Nothing is delivered and nothing is drained: it
/// is manufactured evidence of a message that was never sent — and `apps/web`
/// forwards every envelope opaque to an operator's browser, which renders
/// `fromPersonId` as the sender. This is why consumption is "a row you already
/// hold" and not "your own mailbox".
#[tokio::test]
async fn a_person_may_not_mint_a_row_in_its_own_mailbox_attributed_to_somebody_else() {
    let world = world().await;
    let app = app_as(&world, "chief");
    let (status, body) = delta(
        &app,
        "chief",
        vec![wire_entry("invented", "quant-head", "chief", "pending")],
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(code(&body), Some("mailbox-delta-not-a-delivery"), "body: {body}");
    assert_eq!(entry_count(&read_person(&app, "chief").await), 0);
}

/// DELETES ARE CONSUMPTION. Having legitimately delivered a message does not buy
/// the right to unsend it: a delete destroys a durable record in somebody else's
/// mailbox, and there is no such thing as delivering a deletion.
#[tokio::test]
async fn a_person_may_not_delete_from_another_mailbox_even_one_it_filled() {
    let world = world().await;
    let app = app_as(&world, "quant-head");
    let (status, body) =
        delta(&app, "chief", vec![wire_entry("m-1", "quant-head", "chief", "pending")], vec![])
            .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, body) = delta(&app, "chief", vec![], vec!["m-1@chief"]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(code(&body), Some("mailbox-delta-foreign-delete"), "body: {body}");
    assert_eq!(entry_count(&read_person(&app, "chief").await), 1, "the message survives");
}

/// A MIXED BATCH FAILS WHOLE. One genuine delivery and one forgery in the same
/// request: the genuine one does not land either. The delta is one
/// `BEGIN IMMEDIATE` answering one `seq`, so a partial apply would need a
/// per-entry outcome shape no caller reads — and dropping an entry silently is
/// the failure mode this module refuses everywhere else.
#[tokio::test]
async fn a_mixed_batch_is_refused_whole() {
    let world = world().await;
    let app = app_as(&world, "quant-head");
    let (status, body) = delta(
        &app,
        "chief",
        vec![
            wire_entry("genuine", "quant-head", "chief", "pending"),
            wire_entry("forged", "it-head", "chief", "pending"),
        ],
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(code(&body), Some("mailbox-delta-not-a-delivery"), "body: {body}");
    let detail = body.get("detail").and_then(Value::as_str).unwrap_or_default();
    assert!(detail.contains("forged@chief"), "the refusal names the entry: {detail}");
    assert_eq!(
        entry_count(&read_person(&app, "chief").await),
        0,
        "the entry that WOULD have been allowed must not land either"
    );
}

/// A DAEMON-SCOPED PRINCIPAL IS REFUSED, and it is decided here rather than
/// inherited. Every other route in this packet treats operator/service/channel
/// as unconditionally in scope, because their question is "does this caller
/// manage this target" and a daemon-scoped credential does. THIS route's
/// question is different: both halves of its rule compare against a PERSON, so
/// a principal that is not one cannot satisfy either definition, and allowing it
/// would let any service token mint an entry in anybody's mailbox attributed to
/// anybody — the launcher forgery reopened by another door. Nothing needs the
/// allowance: chiefd's own delivery sink writes these rows in-process.
#[tokio::test]
async fn a_daemon_scoped_principal_may_not_write_a_mailbox_delta() {
    for kind in [IdentityKind::Operator, IdentityKind::Service, IdentityKind::Channel] {
        let world = world().await;
        let mut identity = person_identity("actuator");
        identity.0.kind = kind;
        identity.0.company_slug = None;
        let app = app_for(&world, Some(identity));

        let (status, body) =
            delta(&app, "chief", vec![wire_entry("m-1", "actuator", "chief", "pending")], vec![])
                .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind:?} body: {body}");
        assert_eq!(code(&body), Some("mailbox-delta-requires-a-person"), "{kind:?} body: {body}");
        assert_eq!(entry_count(&read_person(&app, "chief").await), 0, "{kind:?} wrote a row");
    }
}

/// NO CALLER, NO DELIVERY. The rollout stage that made absence mean "local
/// trust" is deleted, so the forgery that arm used to wave through is now `401
/// caller-unauthenticated` and writes no row.
///
/// This stays deliberately DISTINCT from the daemon-scoped case above, and the
/// two must not be flattened into one another: a present non-person credential
/// is a real principal, refused `mailbox-delta-requires-a-person` because the
/// rule has no meaning for it; no credential at all never reaches a handler.
#[tokio::test]
async fn without_a_caller_the_route_is_401_and_writes_no_row() {
    let world = world().await;
    let app = app_for(&world, None);
    let (status, body) =
        delta(&app, "chief", vec![wire_entry("forged", "it-head", "chief", "pending")], vec![])
            .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(code(&body), Some("caller-unauthenticated"), "body: {body}");
    assert_eq!(entry_count(&read_person(&app_as(&world, "chief"), "chief").await), 0);
}

/// The router a named person drives, over a live source that carries a
/// reconcile trigger the test can wait on.
fn app_with_trigger(
    world: &World,
    principal: &str,
    trigger: &Arc<tokio::sync::Notify>,
) -> axum::Router {
    router_with_supervision_live(
        Arc::clone(&world.store),
        1024 * 1024,
        Duration::from_secs(15),
        Some(
            SupervisionLiveSource::new(Arc::clone(&world.company), wire_key())
                .with_reconcile_trigger(Arc::clone(trigger)),
        ),
    )
    .layer(Extension(person_identity(principal)))
}

/// THE UPWARD WAKE. A delivery is a reconcile input, and this route is what
/// makes it durable, so this route is what must nudge the pass that reads it.
///
/// The live defect: every subordinate's reply to the CEO printed "The
/// recipient's immediate wake-up hit an issue". The mailbox row landed, and
/// then the intercom asked for the wake with `/v1/org/runtime/launch` — a
/// COMPANY-WIDE runtime write only the head of the root department may make —
/// so `403 caller-out-of-company-scope` came back for all eight non-executive
/// people. Narrowing THAT route would only have moved the refusal one layer
/// down: `org_ops::start_person` asks `actor_out_of_scope` about the person it
/// starts, and a subordinate does not manage the CEO.
///
/// The recipient-scoped write is the one already in hand.
/// `project_activity_fence` reads pending mailbox rows and grants launch intent
/// to exactly their recipients ("a genuine durable envelope addressed to a
/// specific person IS work arriving and is itself the explicit, per-node
/// decision that authorizes exactly them"), so the wake needs no authority
/// beyond the delivery the caller was already allowed to make.
#[tokio::test]
async fn a_delivery_upward_wakes_the_reconcile_without_any_company_wide_authority() {
    let world = world().await;
    let trigger = Arc::new(tokio::sync::Notify::new());
    // `quant-head` heads a unit far below the root and manages nobody above it.
    let app = app_with_trigger(&world, "quant-head", &trigger);
    let (status, body) =
        delta(&app, "chief", vec![wire_entry("m-1", "quant-head", "chief", "pending")], vec![])
            .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // `Notify` holds one permit, so a wake fired before anyone waited is taken
    // by the very next `notified()` — the drive loop's, or this one.
    tokio::time::timeout(Duration::from_millis(500), trigger.notified())
        .await
        .expect("a committed delivery must nudge the reconcile duty that reads its mailbox row");
}

/// AND A REFUSED DELTA WAKES NOTHING. `wake_reconcile`'s contract is the
/// committed-success path only: a forgery commits no row, so there is no new
/// reconcile input and a pass would be pure cost.
#[tokio::test]
async fn a_refused_delivery_does_not_wake() {
    let world = world().await;
    let trigger = Arc::new(tokio::sync::Notify::new());
    let app = app_with_trigger(&world, "quant-head", &trigger);
    let (status, body) =
        delta(&app, "chief", vec![wire_entry("forged", "it-head", "chief", "pending")], vec![])
            .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");

    tokio::time::timeout(Duration::from_millis(100), trigger.notified())
        .await
        .expect_err("a refusal committed nothing and must not wake");
}
