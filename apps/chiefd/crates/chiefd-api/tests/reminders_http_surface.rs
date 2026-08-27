//! The reminder arming surface — `/v1/reminders/arm|list|stop`.
//!
//! ═══ What this file is FOR ═══
//!
//! `arm_reminder` / `stop_reminder` / `list_reminders` have existed, and been
//! covered by 23 unit tests, since this branch opened. None of that made a
//! reminder something an agent could actually schedule: nothing outside
//! chiefd-core could call them. This file covers the routes that close that
//! gap, and it exists because "the engine is tested" and "the feature is
//! reachable" are independent properties — the lesson this repository has now
//! paid for in three separate features shipped, tested, and called by nothing.
//!
//! ═══ Why these routes are NOT modelled on the `DocStore` ═══
//!
//! A reminder lives in the supervision ledger inside `CompanyDb`, whose plan
//! states there is exactly ONE writer of a reminder.
//! Routing reminders through `DocStore` would create a second authority for
//! reminder state — rows written where the `ReminderDispatch` duty never looks,
//! i.e. armed forever and fired never. The precedent actually copied is `cas`'s
//! `supervision.launcher_cas` arm: a supervision-ledger mutation served over
//! HTTP through `SupervisionLiveSource` -> `CompanyDb::mutate`. The refusal
//! tests at the bottom are what hold that shape in place.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::authn::middleware::CallerIdentity;
use chiefd_api::docstore::{
    router_with_supervision_live, ChangeFeed, DocStore, SupervisionLiveSource,
};
use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::clock::SystemClock;
use chiefd_core::store::identities::{Identity, IdentityKind};
use chiefd_core::store::{organization, supervision};
use chiefd_core::test_support::northstar_manifest;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const EPOCH: i64 = 1_784_116_800_000;
/// A person from `northstar_manifest`. Reminders are addressed to a person, and
/// arming for someone the manifest does not know is a refusal, so these ids must
/// be real ones.
const WORKER: &str = "signal-researcher";
/// Heads `quant`, the unit `WORKER` lives in — so `HEAD` manages `WORKER` and
/// `WORKER` manages nobody. Every authority assertion below turns on that.
const HEAD: &str = "quant-head";
/// Heads `it` — a SIBLING unit, in scope of nothing `WORKER` owns.
const OTHER_HEAD: &str = "it-head";
/// One hour, comfortably above `MIN_REMINDER_INTERVAL_MS`.
const HOURLY_MS: i64 = 3_600_000;

struct World {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    company: Arc<CompanyDb>,
    slug: String,
}

async fn seeded_world(tag: &str) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let slug = format!("cobalt@{tag}");
    let manifest = northstar_manifest(EPOCH);
    let company = Arc::new(
        CompanyDb::open("cobalt", &path, Arc::new(SystemClock::default()))
            .expect("open company db"),
    );
    company
        .mutate(MutationClass::Normal, MutationName("company.create"), {
            let seed = manifest.clone();
            move |ledgers| {
                organization::create(ledgers, &seed)?;
                supervision::seed(ledgers, &seed)?;
                Ok(())
            }
        })
        .await
        .expect("company creation commits");
    World { _dir: dir, path, company, slug }
}

/// Build the router, optionally with the duty-wake trigger the production
/// assembly hands it.
async fn app_for(world: &World, trigger: Option<Arc<tokio::sync::Notify>>) -> axum::Router {
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&world.path.display().to_string(), 2, Arc::clone(&feed))
            .expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let mut live = SupervisionLiveSource::new(Arc::clone(&world.company), world.slug.clone());
    if let Some(trigger) = trigger {
        live = live.with_reminder_trigger(trigger);
    }
    router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live))
}

/// The resolved caller `authn::middleware::require_identity` would have
/// inserted after a bearer token verified. These tests mount the router
/// directly, without the auth layer, so the extension is injected per request —
/// the identity is the SUBJECT of the reminder routes' authority checks, not
/// plumbing around them.
fn caller(kind: IdentityKind, principal: &str) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: format!("id:{principal}"),
        principal: principal.to_string(),
        kind,
        company_slug: Some("cobalt".to_string()),
        pubkey: Some("spki".to_string()),
        fingerprint: "fp".to_string(),
        active: true,
        enrolled_at: EPOCH,
        enrolled_by: None,
        revoked_at: None,
    })
}

/// POST as `actor`, a proven PERSON of this company.
async fn post_as(app: &axum::Router, actor: &str, uri: &str, body: Value) -> (StatusCode, String) {
    post_with(app, Some(caller(IdentityKind::Person, actor)), uri, body).await
}

/// POST with whatever credential (or none) the caller presented.
async fn post_with(
    app: &axum::Router,
    identity: Option<CallerIdentity>,
    uri: &str,
    body: Value,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    if let Some(identity) = identity {
        request.extensions_mut().insert(identity);
    }
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("response is not JSON ({e}): {body}"))
}

/// THE REACHABILITY TEST: a reminder armed over HTTP is durable in the
/// supervision ledger, and it is the SAME row the duty's own store API returns.
///
/// The second half is the point. Asserting only on the 200 would prove the route
/// answered, not that it wrote anywhere the firing duty will ever look — which
/// is exactly how a produced-forever/delivered-never surface passes its own
/// tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arming_over_http_writes_the_ledger_the_dispatch_duty_reads() {
    let world = seeded_world("armdurable").await;
    let app = app_for(&world, None).await;

    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": WORKER,
            "prompt": "re-read the risk limits before the open",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "arming must succeed: {body}");
    let armed = parse(&body);
    let reminder_id = armed["reminder"]["id"].as_str().expect("id").to_string();
    assert_eq!(armed["reminder"]["personId"], WORKER);
    assert_eq!(armed["reminder"]["status"], "active");
    assert_eq!(
        armed["reminder"]["recurring"], true,
        "an un-annotated arm must be recurring — a silent one-shot is a reminder the operator \
         never sees again"
    );

    // The assertion that matters: read it back through the STORE API the
    // `ReminderDispatch` duty itself calls, not through the route that wrote it.
    let stored = world.company.read(|snapshot| {
        let ledgers = snapshot.ledgers();
        let manifest = organization::read(ledgers).expect("manifest");
        let ledger = supervision::read(ledgers, &manifest).expect("supervision");
        supervision::list_reminders(&ledger, WORKER)
    });
    assert_eq!(stored.len(), 1, "the duty's own view must contain the HTTP-armed reminder");
    assert_eq!(stored[0].id, reminder_id, "and it must be the SAME row, not a second copy");
    assert_eq!(stored[0].prompt, "re-read the risk limits before the open");
}

/// Arming must wake the dispatch duty, not leave it asleep on a stale alarm.
///
/// `ReminderDispatch` sits on the reactive fan-out, but that fan-out
/// re-broadcasts from ONE signal that only a mailbox/fence event nudges. An HTTP
/// arm is a different caller, so without the trigger seam the duty keeps
/// sleeping on the alarm it computed BEFORE this reminder existed — up to the
/// five-minute fallback floor. Durable, correct, and late; and late is the whole
/// product for a reminder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arming_over_http_wakes_the_dispatch_duty() {
    let world = seeded_world("armwakes").await;
    let trigger = Arc::new(tokio::sync::Notify::new());
    let app = app_for(&world, Some(Arc::clone(&trigger))).await;

    // Register the waiter BEFORE the request: `Notify::notified()` only counts
    // permits from the moment it is created, so awaiting after the fact would
    // race and could hang on a working implementation.
    let waiter = {
        let trigger = Arc::clone(&trigger);
        tokio::spawn(async move { trigger.notified().await })
    };
    tokio::task::yield_now().await;

    let (status, body) = post_as(
        &app,
        HEAD,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": WORKER,
            "prompt": "stand-up",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "arming must succeed: {body}");

    // A real bound, not a wall-clock guess: the nudge is synchronous with the
    // handler, so anything but "immediately" is the bug.
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("arming must nudge the dispatch duty, not leave it on a stale alarm")
        .expect("waiter task");
}

/// Stopping must wake it too. Stopping shortens nothing, but it can REMOVE the
/// earliest alarm, leaving the duty scheduled to wake at an instant when nothing
/// is due.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_over_http_wakes_the_dispatch_duty() {
    let world = seeded_world("stopwakes").await;
    let trigger = Arc::new(tokio::sync::Notify::new());
    let app = app_for(&world, Some(Arc::clone(&trigger))).await;

    let (_, armed) = post_as(
        &app,
        WORKER,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": WORKER,
            "prompt": "stand-up",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    let reminder_id = parse(&armed)["reminder"]["id"].as_str().expect("id").to_string();

    let waiter = {
        let trigger = Arc::clone(&trigger);
        tokio::spawn(async move { trigger.notified().await })
    };
    tokio::task::yield_now().await;

    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/stop",
        json!({ "slug": world.slug, "personId": WORKER, "reminderId": reminder_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stopping must succeed: {body}");

    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("stopping must nudge the dispatch duty to recompute its alarm")
        .expect("waiter task");
}

/// Stop RETAINS the row, deliberately — it does not delete it.
///
/// Recycling a reminder id would collide with the effect ids that reminder has
/// already published (`person-reminder:<id>:<dueMillis>`), which `enqueue_effect`
/// refuses as a content conflict; the reused id would then silently stop firing.
/// This test is what stops a future "tidy up stopped reminders" change from
/// reintroducing that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_reminder_is_retained_as_history_never_deleted() {
    let world = seeded_world("stopretains").await;
    let app = app_for(&world, None).await;

    let (_, armed) = post_as(
        &app,
        WORKER,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": WORKER,
            "prompt": "stand-up",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    let reminder_id = parse(&armed)["reminder"]["id"].as_str().expect("id").to_string();

    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/stop",
        json!({ "slug": world.slug, "personId": WORKER, "reminderId": reminder_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stopping must succeed: {body}");
    assert_eq!(parse(&body)["reminder"]["status"], "stopped");

    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/list",
        json!({ "slug": world.slug, "personId": WORKER }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let listed = parse(&body);
    let reminders = listed["reminders"].as_array().expect("reminders array");
    assert_eq!(reminders.len(), 1, "the stopped row must survive as history, not vanish");
    assert_eq!(reminders[0]["id"], reminder_id.as_str());
    assert_eq!(reminders[0]["status"], "stopped");
}

/// `list` is scoped to one person, and does not leak another person's reminders.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_returns_only_the_named_persons_reminders() {
    let world = seeded_world("listscoped").await;
    let app = app_for(&world, None).await;

    for (person, prompt) in [(WORKER, "worker prompt"), (HEAD, "head prompt")] {
        let (status, body) = post_as(
            &app,
            person,
            "/v1/reminders/arm",
            json!({
                "slug": world.slug,
                "personId": person,
                "prompt": prompt,
                "intervalMs": HOURLY_MS,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "arming for {person} must succeed: {body}");
    }

    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/list",
        json!({ "slug": world.slug, "personId": WORKER }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let reminders = parse(&body)["reminders"].as_array().expect("array").clone();
    assert_eq!(reminders.len(), 1, "one person's list must not carry another's reminders");
    assert_eq!(reminders[0]["prompt"], "worker prompt");
}

/// A malformed arm is refused by the ENGINE, and the refusal reaches the caller
/// as a 400 rather than being flattened into a 500.
///
/// The sub-minute interval is the one worth naming: a reminder armed at seconds
/// is a poller wearing a reminder's clothes, and the engine refuses it. The
/// route must carry that refusal through legibly, or the agent that asked for it
/// learns nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sub_minute_interval_is_refused_legibly() {
    let world = seeded_world("refusepoll").await;
    let app = app_for(&world, None).await;

    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": WORKER,
            "prompt": "spin",
            "intervalMs": 1_000,
        }),
    )
    .await;
    // #1004: a product rule is a 422, not a 400. The request was well formed
    // and chiefd declined it — nothing about the payload was malformed.
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a one-second cadence must be refused: {body}"
    );
    assert!(
        body.contains("at least"),
        "the refusal must say what the bound IS, not merely that the request was bad: {body}"
    );

    let stored = world.company.read(|snapshot| {
        let ledgers = snapshot.ledgers();
        let manifest = organization::read(ledgers).expect("manifest");
        let ledger = supervision::read(ledgers, &manifest).expect("supervision");
        supervision::list_reminders(&ledger, WORKER)
    });
    assert!(stored.is_empty(), "a refused arm must write NOTHING");
}

/// Arming for someone this company does not employ is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arming_for_an_unknown_person_is_refused() {
    let world = seeded_world("refuseghost").await;
    let app = app_for(&world, None).await;

    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": "nobody-at-all",
            "prompt": "haunt",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unknown person must be refused: {body}"
    );
}

/// Stopping someone ELSE's reminder is refused, and reported as unknown rather
/// than as "not yours" — the two answers together would let anyone enumerate
/// another person's reminder ids.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_another_persons_reminder_is_refused_without_confirming_it_exists() {
    let world = seeded_world("refusecross").await;
    let app = app_for(&world, None).await;

    let (_, armed) = post_as(
        &app,
        WORKER,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": WORKER,
            "prompt": "private",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    let reminder_id = parse(&armed)["reminder"]["id"].as_str().expect("id").to_string();

    let (status, body) = post_as(
        &app,
        HEAD,
        "/v1/reminders/stop",
        json!({ "slug": world.slug, "personId": HEAD, "reminderId": reminder_id }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a cross-person stop must be refused: {body}"
    );
    assert!(
        !body.contains("belongs to"),
        "the refusal must not confirm the id exists for someone else: {body}"
    );

    // And the reminder is untouched.
    let stored = world.company.read(|snapshot| {
        let ledgers = snapshot.ledgers();
        let manifest = organization::read(ledgers).expect("manifest");
        let ledger = supervision::read(ledgers, &manifest).expect("supervision");
        supervision::list_reminders(&ledger, WORKER)
    });
    assert_eq!(stored[0].status, "active", "a refused stop must not disarm anything");
}

/// THE SHAPE TEST — a foreign company is REFUSED, never silently written to
/// `org_documents`.
///
/// This is the one that keeps the routes honest. Reminders have exactly one
/// authority, and a fallback to `org_documents` here would write rows into a
/// store the `ReminderDispatch` duty never reads: the request would answer 200,
/// the operator would see a scheduled reminder, and it would never once fire.
/// A refusal is the correct, legible outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_company_is_refused_never_written_to_a_store_nothing_fires_from() {
    let world = seeded_world("foreignslug").await;
    let app = app_for(&world, None).await;

    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/arm",
        json!({
            "slug": "some-other-company@elsewhere",
            "personId": WORKER,
            "prompt": "not ours",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a foreign slug must be refused: {body}");
    assert!(
        body.contains("not the reminder authority"),
        "the refusal must say WHY, so the caller can route elsewhere: {body}"
    );
}

/// The standalone docstore surface (no live company at all) refuses too, rather
/// than accepting a reminder no daemon will ever fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_router_with_no_live_company_refuses_rather_than_accepting_a_dead_reminder() {
    let world = seeded_world("nolive").await;
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&world.path.display().to_string(), 2, Arc::clone(&feed))
            .expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let app = router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), None);

    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": WORKER,
            "prompt": "nobody will fire this",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no live company must refuse: {body}");
    // Assert the REFUSAL, not merely the status. An unregistered route also
    // answers 404, so a bare status check here passes just as happily when the
    // routes do not exist at all — measured: with the three routes commented
    // out, 9 of the 10 tests in this file went red and THIS one stayed green.
    // A test that passes without its subject is worse than no test.
    assert!(
        body.contains("no live company"),
        "the refusal must be this route's own, not axum's route-not-found: {body}"
    );
}

// ═══ WHO MAY REACH WHOSE REMINDERS ═══════════════════════════════════════════
//
// The three tools have always DESCRIBED `personId` as "only for a manager
// arming a reminder for someone they manage". Nothing enforced it: the deleted
// CLI passed the ids through, and `arm_reminder` checked only that both people
// EXISTED — so any worker could arm a durable, recurring wake-up on the CEO,
// list anybody's reminders, and stop them. These tests are the enforcement, and
// every one of them asserts BOTH directions: an allowed case alone stays green
// for ever if the gate is deleted again, which is precisely how this survived
// from the day the family shipped.

/// A caller who cannot say who it is has no reminders to reach. Absence is a
/// refusal, not local trust — the same rule `require_self_identity` applies to
/// the two personal runtime switches (#751/P7).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unauthenticated_caller_cannot_arm_a_reminder() {
    let world = seeded_world("authnone").await;
    let app = app_for(&world, None).await;

    let (status, body) = post_with(
        &app,
        None,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": WORKER,
            "prompt": "armed by nobody",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "an unproven caller must be refused: {body}");
    assert!(
        body.contains("enrolled identity key"),
        "the refusal must say what is missing and how to get it: {body}"
    );

    let stored = world.company.read(|snapshot| {
        let ledgers = snapshot.ledgers();
        let manifest = organization::read(ledgers).expect("manifest");
        let ledger = supervision::read(ledgers, &manifest).expect("supervision");
        supervision::list_reminders(&ledger, WORKER)
    });
    assert!(stored.is_empty(), "a refused arm must write NOTHING");
}

/// An operator/service/channel credential authenticates but is nobody's agent.
/// Reading one as a person is how a gateway token becomes a manager, so it is
/// refused rather than promoted to unconditional scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_scoped_credential_is_not_a_person() {
    let world = seeded_world("authdaemon").await;
    let app = app_for(&world, None).await;

    for kind in [IdentityKind::Operator, IdentityKind::Service, IdentityKind::Channel] {
        let (status, body) = post_with(
            &app,
            Some(caller(kind, "gateway")),
            "/v1/reminders/arm",
            json!({
                "slug": world.slug,
                "personId": WORKER,
                "prompt": "armed by a gateway",
                "intervalMs": HOURLY_MS,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "a {kind:?} credential is not a person: {body}");
        assert!(body.contains("not a person"), "the refusal must say why: {body}");
    }
}

/// BOTH DIRECTIONS on the arm: the head who manages the worker may, a worker
/// reaching UP may not, and a sibling head reaching ACROSS may not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reminder_is_armed_only_on_yourself_or_on_somebody_you_manage() {
    let world = seeded_world("armscope").await;
    let app = app_for(&world, None).await;

    // ---- allowed ----------------------------------------------------------
    let (status, body) = post_as(
        &app,
        HEAD,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": WORKER,
            "prompt": "their own head may",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a head arming on their own report must succeed: {body}");

    // ---- refused ----------------------------------------------------------
    for (actor, target) in [(WORKER, HEAD), (OTHER_HEAD, WORKER)] {
        let (status, body) = post_as(
            &app,
            actor,
            "/v1/reminders/arm",
            json!({
                "slug": world.slug,
                "personId": target,
                "prompt": "out of scope",
                "intervalMs": HOURLY_MS,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "'{actor}' must not arm on '{target}': {body}");
        assert!(
            body.contains("does not manage") && body.contains(actor) && body.contains(target),
            "the refusal must name both people and the rule: {body}"
        );
    }

    // Exactly one reminder exists company-wide: the allowed one.
    let (worker_rows, head_rows) = world.company.read(|snapshot| {
        let ledgers = snapshot.ledgers();
        let manifest = organization::read(ledgers).expect("manifest");
        let ledger = supervision::read(ledgers, &manifest).expect("supervision");
        (
            supervision::list_reminders(&ledger, WORKER).len(),
            supervision::list_reminders(&ledger, HEAD).len(),
        )
    });
    assert_eq!(worker_rows, 1, "only the in-scope arm may have written");
    assert_eq!(head_rows, 0, "a refused arm must write NOTHING");
}

/// The creator is the CREDENTIAL, never the body. `createdByPersonId` left the
/// wire, so a caller that still sends one must not be able to re-attribute its
/// own reminder — or the field that authorized the write would be the field the
/// caller chose.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_creator_is_the_verified_caller_and_a_body_field_cannot_move_it() {
    let world = seeded_world("armcredit").await;
    let app = app_for(&world, None).await;

    let (status, body) = post_as(
        &app,
        HEAD,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": WORKER,
            // Ignored: not a field of the request any more.
            "createdByPersonId": WORKER,
            "prompt": "credited to the key, not the payload",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "arming must succeed: {body}");
    assert_eq!(
        parse(&body)["reminder"]["createdByPersonId"],
        HEAD,
        "the reminder must be credited to the authenticated caller: {body}"
    );
}

/// Reading and stopping are fenced by the SAME predicate as arming. A read that
/// was not gated would let any worker enumerate the executive's reminders.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listing_and_stopping_are_fenced_by_the_same_rule() {
    let world = seeded_world("readscope").await;
    let app = app_for(&world, None).await;

    let (_, armed) = post_as(
        &app,
        HEAD,
        "/v1/reminders/arm",
        json!({
            "slug": world.slug,
            "personId": HEAD,
            "prompt": "the head's own",
            "intervalMs": HOURLY_MS,
        }),
    )
    .await;
    let reminder_id = parse(&armed)["reminder"]["id"].as_str().expect("id").to_string();

    // ---- refused ----------------------------------------------------------
    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/list",
        json!({ "slug": world.slug, "personId": HEAD }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a worker must not read upward: {body}");
    assert!(body.contains("does not manage"), "the refusal must name its cause: {body}");

    let (status, body) = post_as(
        &app,
        WORKER,
        "/v1/reminders/stop",
        json!({ "slug": world.slug, "personId": HEAD, "reminderId": reminder_id }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a worker must not stop the head's: {body}");

    // ---- allowed ----------------------------------------------------------
    let (status, body) =
        post_as(&app, HEAD, "/v1/reminders/list", json!({ "slug": world.slug, "personId": HEAD }))
            .await;
    assert_eq!(status, StatusCode::OK, "their own list must still work: {body}");
    assert_eq!(parse(&body)["reminders"].as_array().expect("array").len(), 1);

    let (status, body) = post_as(
        &app,
        HEAD,
        "/v1/reminders/stop",
        json!({ "slug": world.slug, "personId": HEAD, "reminderId": reminder_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stopping their own must still work: {body}");
    assert_eq!(parse(&body)["reminder"]["status"], "stopped");
}
