//! Tests for the `chiefd run` duty scheduler.
//!
//! Every wait is driven by a [`ManualClock`]: no test sleeps on the wall clock,
//! and the schedule is advanced explicitly, so "fired once per interval" is a
//! decision the test makes, never a window it hopes to hit.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::clock::{Clock, SharedClock};
use chiefd_core::runtime::duty_hooks::{
    BoxFuture, CycleInputGatherer, ReconcileActuator, ReconcileReport,
};
use chiefd_core::store::org_ops::BenchCompletionKey;
use chiefd_core::store::supervision::{CycleInput, IdentityObservation, RuntimeAuditObservation};
use chiefd_core::store::supervisor_watermark::Duty;
use chiefd_core::store::{organization, supervision, supervisor_watermark};
use chiefd_core::test_support::{northstar_manifest, ManualClock};
use serde_json::json;
use tokio::sync::{watch, Notify};

use chiefd_api::docstore::ChangeFeed;

use super::{
    drive, reactive_fallback_floor_from, schedule_reconcile_floor_retry,
    spawn_supervision_schedule_wake, stubs, supervise, wire_change_feed, ActuationMode, Daemon,
    DutyContext, DutyError, DutyPass, DEFAULT_REACTIVE_FALLBACK_FLOOR, MIN_REACTIVE_FALLBACK_FLOOR,
};

// TOMBSTONE: `actuator_ramp_reads_bounded_operator_values_and_falls_back_closed`.
//
// It pinned that `chiefd run`'s admission ramp was the operator's configured
// one, bounds and fail-closed parse included. The ramp is deleted by operator
// ruling, so there is no value for this file to read and nothing for the parse
// to be bounded about. Not weakened: the behaviour and the type are both gone.

/// E8-S2 (#824): every [`Duty`] must be accounted for by exactly one of
/// `REACTIVE_DUTIES`, `SELF_TRIGGERED_DUTIES`, or
/// `NON_REACTIVE_DUTY_JUSTIFICATIONS` — never zero (a duty silently added on
/// a bare fixed timer with no trigger and no written reason) and never more
/// than one (a duty whose documentation contradicts itself about why it
/// runs). This is the conformance mechanism mandate 1 asks for: adding a
/// duty that fails to satisfy one of these three fails the build, not a
/// future code review. Calls the SAME classification [`Daemon::new`] asserts
/// at construction time, so the rule is checked twice, never duplicated.
#[test]
fn duty_cadence_conformance() {
    let violations = Daemon::duty_cadence_conformance_violations();
    assert!(violations.is_empty(), "{violations:#?}");
    for (duty, reason) in Daemon::NON_REACTIVE_DUTY_JUSTIFICATIONS {
        assert!(
            !reason.is_empty(),
            "{duty:?}'s justification must be a real reason, not an empty string"
        );
    }
}

/// The negative half: prove the check above can actually fail, so it is not
/// a vacuously-passing conformance test. Temporarily narrows
/// `REACTIVE_DUTIES` to a slice missing `HealthMonitor` (this test's own
/// local copy, never the real associated const) and confirms the SAME
/// membership arithmetic reports it as unaccounted-for.
#[test]
fn duty_cadence_conformance_actually_fails_when_a_duty_is_unaccounted_for() {
    let reactive_without_health: Vec<Duty> = Daemon::REACTIVE_DUTIES
        .iter()
        .copied()
        .filter(|&duty| duty != Duty::HealthMonitor)
        .collect();
    let violations: Vec<String> = Duty::ALL
        .iter()
        .filter_map(|&duty| {
            let reactive = reactive_without_health.contains(&duty);
            let self_triggered = Daemon::SELF_TRIGGERED_DUTIES.contains(&duty);
            let justified = Daemon::NON_REACTIVE_DUTY_JUSTIFICATIONS
                .iter()
                .any(|(justified, _)| *justified == duty);
            let memberships =
                usize::from(reactive) + usize::from(self_triggered) + usize::from(justified);
            (memberships != 1).then(|| format!("{duty:?}"))
        })
        .collect();
    assert_eq!(
        violations,
        vec!["HealthMonitor".to_string()],
        "the check itself can fail, and names the duty"
    );
}

const SLUG: &str = "northstar-conformance";

/// Open a company writer on a temp DB and seed its manifest + supervision
/// ledger, so the duty bodies have real durable state to mutate.
async fn seed(clock: SharedClock, now: i64) -> (tempfile::TempDir, Arc<CompanyDb>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let company = Arc::new(CompanyDb::open(SLUG, &path, clock).expect("open company db"));
    company
        .mutate(MutationClass::Normal, MutationName("test.seed"), move |ledgers| {
            let manifest = northstar_manifest(now);
            organization::create(ledgers, &manifest)?;
            supervision::seed(ledgers, &manifest)?;
            Ok(())
        })
        .await
        .expect("seed company");
    (dir, company)
}

fn daemon(company: Arc<CompanyDb>, clock: SharedClock) -> Arc<Daemon> {
    Arc::new(Daemon::new(SLUG, company, clock, stubs::test_hooks(), ActuationMode::Shadow))
}

/// A live runtime-owner row changed to another socket after this daemon booted.
/// Production observes this through `ReconcilerFactsStore`; the focused scheduler
/// regression needs only its resulting cycle-input fact.
struct SwitchingRuntimeOwner {
    drifted: Arc<AtomicBool>,
}

/// First gather sees the pre-actuation pane; only a second gather, after the
/// actuator marks itself complete, reports the pane absent.
struct PostActuationBenchGather {
    calls: Arc<AtomicUsize>,
    actuated: Arc<AtomicBool>,
}

impl CycleInputGatherer for PostActuationBenchGather {
    fn gather_cycle_input(
        &self,
        _ctx: &DutyContext,
    ) -> BoxFuture<'_, Result<CycleInput, DutyError>> {
        let call = self.calls.fetch_add(1, SeqCst);
        let actuated = Arc::clone(&self.actuated);
        Box::pin(async move {
            if call > 0 {
                assert!(actuated.load(SeqCst), "the completion audit must run after actuation");
            }
            Ok(CycleInput {
                audit: RuntimeAuditObservation {
                    live: if call == 0 {
                        ["signal-researcher".to_string()].into_iter().collect()
                    } else {
                        Default::default()
                    },
                    ..RuntimeAuditObservation::default()
                },
                ..CycleInput::default()
            })
        })
    }
}

struct MarkingBenchActuator {
    actuated: Arc<AtomicBool>,
}

impl ReconcileActuator for MarkingBenchActuator {
    fn reconcile(
        &self,
        _ctx: &DutyContext,
        _mode: ActuationMode,
    ) -> BoxFuture<'_, Result<ReconcileReport, DutyError>> {
        let actuated = Arc::clone(&self.actuated);
        Box::pin(async move {
            actuated.store(true, SeqCst);
            Ok(ReconcileReport { applied: true, ..ReconcileReport::default() })
        })
    }
}

impl CycleInputGatherer for SwitchingRuntimeOwner {
    fn gather_cycle_input(
        &self,
        _ctx: &DutyContext,
    ) -> BoxFuture<'_, Result<CycleInput, DutyError>> {
        let drifted = Arc::clone(&self.drifted);
        Box::pin(async move {
            if drifted.load(SeqCst) {
                Ok(CycleInput {
                    identity: IdentityObservation::Foreign {
                        holder: "new-owner-socket".to_string(),
                    },
                    ..CycleInput::default()
                })
            } else {
                Ok(CycleInput::default())
            }
        })
    }
}

/// Yield cooperatively until the manual clock has exactly `target` parked
/// waits, or fail — a bounded settle so a wiring bug cannot hang the suite.
async fn settle_sleeps(clock: &ManualClock, target: usize) {
    for _ in 0..1_000 {
        if clock.pending_sleeps() == target {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("clock never settled to {target} pending sleeps (saw {})", clock.pending_sleeps());
}

/// A manifest-readiness latch that is ALREADY open.
///
/// Every duty task holds its first pass until the company's organization
/// manifest exists (`wait_for_company_ready`, the genesis gate). These tests
/// seed the manifest before they start anything, so the company is ready by
/// construction and the latch must not be what they are measuring. The sender
/// is dropped immediately on purpose: the gate reads the value and returns
/// before it ever awaits, so a closed channel is never observed.
///
/// The gate's own behaviour is proved in `crate::manifest_ready`'s tests and in
/// `a_duty_holds_its_first_pass_until_the_manifest_exists` below, not here.
fn ready_now() -> watch::Receiver<bool> {
    watch::channel(true).1
}

/// Yield until `counter` reaches `target`, or fail.
async fn settle_count(counter: &AtomicUsize, target: usize) {
    for _ in 0..1_000 {
        if counter.load(SeqCst) >= target {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("counter never reached {target} (saw {})", counter.load(SeqCst));
}

/// Regression: a SIGTERM that lands during startup must still stop the daemon.
///
/// `Daemon::serve` installs the #504 SIGTERM attribution handler, which
/// replaces SIGTERM's default disposition (terminate) with a recorder. From
/// that instant until tokio's own SIGTERM stream is registered, a supervisor's
/// SIGTERM was swallowed outright — the daemon ran on and took the SIGKILL
/// escalation ten seconds later. The whole startup path (self-audit, duty
/// spawn, docstore bind + mount) sat inside that window, and the mounted
/// `/v1/docs/watch` surface answers requests from inside it, so a supervisor
/// that signals as soon as the surface is up hit it regularly.
///
/// The fix is `ArmedShutdownSignal`: registration is separated from the await
/// and happens first. This locks the property that makes that safe — a
/// delivery that predates the first poll is remembered, not lost.
///
/// `raise` is a real process-directed signal rather than an injected clock
/// tick, because "does the handler latch it" is a question only the kernel can
/// answer; it cannot terminate this test binary, since `arm()` installed the
/// handler on the line above.
#[tokio::test]
async fn an_armed_shutdown_signal_latches_a_sigterm_that_predates_its_first_poll() {
    let armed = super::ArmedShutdownSignal::arm();

    nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).expect("SIGTERM is raised");

    tokio::time::timeout(Duration::from_secs(5), armed.wait())
        .await
        .expect("an armed shutdown signal observes a SIGTERM delivered before it was awaited");
}

#[tokio::test]
async fn a_duty_records_success_in_one_commit() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let (_dir, company) = seed(clock.clone(), mc.wall().0).await;
    let daemon = daemon(company.clone(), clock);

    let before = company.snapshot().commit_seq();
    daemon.run_supervision_reconcile().await;
    let after = company.snapshot();

    // The decisive assertion: the duty's own work AND its watermark advanced in
    // EXACTLY ONE commit. A separate `record_success` mutate would make this
    // `before + 2`. (Carried by the supervision-reconcile duty now that the
    // deadline-evaluation duty this was written against is deleted; the
    // property is the one-commit rule, not the duty.)
    assert_eq!(after.commit_seq(), before + 1, "duty work and its watermark are one commit");

    // And the watermark actually names this duty with a recorded run.
    let body = company.read(|snapshot| {
        snapshot.ledgers().document_body("supervisor-watermark").unwrap_or_default().to_string()
    });
    assert!(body.contains("supervision_reconcile"), "watermark names the duty: {body}");
    assert!(body.contains("\"runCount\":1"), "watermark recorded exactly one run: {body}");
}

/// THE STARTUP SELF-AUDIT RUNS AFTER THE GATE — it is not skipped more quietly.
///
/// This is the structural half of the genesis race. The other five refusals in
/// that 229 ms window self-heal on the next reactive pass; this one does not.
/// `serve` calls `run_startup_self_audit` exactly once per process and nothing
/// retries it, so a manifest that arrived late meant the audit never ran at all.
///
/// On a brand-new company the audit is EMPTY — no missed-window backlog, no
/// orphan supervision effects — which is exactly why a test has to give it real
/// work: otherwise "it ran" and "it did nothing" are indistinguishable, and a
/// fix that merely skipped it more quietly would pass. So the company that
/// arrives at the gate here carries a `SupervisionReconcile` watermark twenty of
/// its windows stale, and the assertion is the durable health incident that only
/// a self-audit which actually RAN, and ran late enough to see the manifest,
/// could have folded in.
#[tokio::test]
async fn the_startup_self_audit_runs_after_the_gate_and_does_its_work() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let now = mc.wall().0;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let company = Arc::new(CompanyDb::open(SLUG, &path, clock.clone()).expect("open company db"));
    let daemon = daemon(company.clone(), clock);

    // The shutdown sender is held for the whole test: the gate must end because
    // the manifest arrived, never because the daemon was stopping.
    let (_tx, shutdown) = watch::channel(false);
    let (ready_tx, ready_rx) = watch::channel(false);
    let gate = {
        let daemon = Arc::clone(&daemon);
        let mut shutdown = shutdown.clone();
        tokio::spawn(async move {
            daemon
                .open_the_duty_gate(
                    &mut shutdown,
                    &ready_tx,
                    crate::manifest_ready::MANIFEST_READY_BUDGET,
                )
                .await
        })
    };
    settle_sleeps(&mc, 1).await;
    assert!(!*ready_rx.borrow(), "the latch is shut while the company does not exist");

    // Genesis commits — and this company has a REAL backlog waiting for the
    // audit: SupervisionReconcile last succeeded twenty of its windows ago,
    // well past the three-window stale multiple.
    let stale_at = now - 20 * Duty::SupervisionReconcile.interval_ms();
    company
        .mutate(MutationClass::Normal, MutationName("test.genesis"), move |ledgers| {
            let manifest = northstar_manifest(now);
            organization::create(ledgers, &manifest)?;
            supervision::seed(ledgers, &manifest)?;
            chiefd_core::store::activity::seed(ledgers, &manifest)?;
            let ctx = organization::company_context(&manifest)?;
            supervisor_watermark::record_success(
                ledgers,
                &ctx,
                Duty::SupervisionReconcile,
                stale_at,
            );
            Ok(())
        })
        .await
        .expect("genesis commits");
    mc.advance(Duration::from_secs(1));

    assert!(gate.await.expect("the gate task completes"), "the gate opened on the manifest");
    assert!(*ready_rx.borrow(), "and released every duty held behind it");

    let health = company.read(|snapshot| {
        snapshot.ledgers().document_body("health-monitor").unwrap_or_default().to_string()
    });
    assert!(
        health.contains("supervisor_duty_stalled"),
        "the self-audit RAN, after the manifest arrived, and folded the missed-window backlog \
         into health: {health}"
    );
}

/// THE STARTUP-RACE REGRESSION, at the scheduler's own level.
///
/// `chief_cli::genesis` starts a company's daemon and only THEN posts
/// `/v1/org/manifest/genesis-with-models` to it, because the daemon is the
/// company's single writer — genesis writes THROUGH the process that needs the
/// manifest. So the daemon's first duty pass used to run against a company that
/// did not exist yet, and refused `unknown-company`. Measured live on
/// `tribes-capital`: 229 ms, six refusals, on every single launch.
///
/// Both halves are asserted against ONE fixture, because the whole claim is a
/// difference: the same pass, on the same company, before and after the gate.
#[tokio::test]
async fn the_first_duty_pass_runs_after_the_readiness_gate_instead_of_refusing() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    // An EMPTY company — schema present, no manifest — which is exactly what
    // genesis spawns a daemon onto.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let company = Arc::new(CompanyDb::open(SLUG, &path, clock.clone()).expect("open company db"));
    let daemon = daemon(company.clone(), clock.clone());

    // What the daemon used to do: its first pass, before genesis. It refuses —
    // and the refusal is not even durably observable, because the failure
    // watermark's own write reads the same absent manifest.
    daemon.run_supervision_reconcile().await;
    assert!(
        company.read(|snapshot| snapshot
            .ledgers()
            .document_body(chiefd_core::store::supervisor_watermark::SUPERVISOR_WATERMARK_STORE)
            .is_none()),
        "a pre-genesis pass leaves nothing behind: not a cycle, not even a failure watermark"
    );

    // What it does now: hold at the readiness gate. The daemon reaches it FIRST,
    // as it always does.
    let waiting = {
        let company = Arc::clone(&company);
        let clock = clock.clone();
        tokio::spawn(async move {
            crate::manifest_ready::await_manifest(
                &company,
                SLUG,
                &clock,
                crate::manifest_ready::MANIFEST_READY_BUDGET,
            )
            .await
        })
    };
    settle_sleeps(&mc, 1).await;

    // Genesis commits, in the ONE transaction `org_manifest_genesis_with_models`
    // uses: the manifest and both scheduler ledgers together.
    let now = mc.wall().0;
    company
        .mutate(MutationClass::Normal, MutationName("test.genesis"), move |ledgers| {
            let manifest = northstar_manifest(now);
            organization::create(ledgers, &manifest)?;
            supervision::seed(ledgers, &manifest)?;
            chiefd_core::store::activity::seed(ledgers, &manifest)?;
            Ok(())
        })
        .await
        .expect("genesis commits");
    // Any advance past one poll releases the gate: the wait re-reads the
    // manifest BEFORE it consults its budget, so a late wake still reports the
    // manifest it can see rather than the budget it slept through.
    mc.advance(Duration::from_secs(1));
    assert!(
        waiting.await.expect("the gate completes").is_ready(),
        "the gate reports the manifest genesis just committed"
    );

    // And only now the first duty pass — which commits, instead of refusing.
    let before = company.snapshot().commit_seq();
    daemon.run_supervision_reconcile().await;
    assert!(
        company.snapshot().commit_seq() > before,
        "the first pass after the gate commits a real supervision cycle"
    );
    let body = company.read(|snapshot| {
        snapshot
            .ledgers()
            .document_body(chiefd_core::store::supervisor_watermark::SUPERVISOR_WATERMARK_STORE)
            .unwrap_or_default()
            .to_string()
    });
    assert!(body.contains("supervision_reconcile"), "the watermark names the duty: {body}");
    assert!(body.contains("\"runCount\":1"), "and records exactly one run: {body}");
    assert!(
        !body.contains("lastFailureKind"),
        "the gate means the very first pass has no `unknown-company` failure to record: {body}"
    );
}

/// #469: boot-time socket adoption is the only safe adoption point.  If the
/// durable runtime-owner row later names another socket, continuing as an inert
/// duty would freeze goals forever, while switching sockets could grow a shadow
/// fleet.  The production daemon must request a non-zero, supervised restart
/// before opening the ledger mutation or touching the runtime.
#[tokio::test]
async fn foreign_runtime_owner_after_boot_requests_fatal_restart_without_a_cycle_commit() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let (_dir, company) = seed(clock.clone(), mc.wall().0).await;
    let mut hooks = stubs::test_hooks();
    let drifted = Arc::new(AtomicBool::new(false));
    hooks.cycle_input = Arc::new(SwitchingRuntimeOwner { drifted: Arc::clone(&drifted) });
    let daemon = Daemon::new(SLUG, company.clone(), clock, hooks, ActuationMode::Apply)
        .with_foreign_identity_fatal_shutdown();

    let before = company.snapshot().commit_seq();
    daemon.run_supervision_reconcile().await;
    let after_healthy_cycle = company.snapshot().commit_seq();
    assert!(
        after_healthy_cycle > before,
        "the same daemon must complete a healthy owned cycle before the owner drifts"
    );

    drifted.store(true, SeqCst);
    daemon.run_supervision_reconcile().await;

    assert_eq!(
        company.snapshot().commit_seq(),
        after_healthy_cycle,
        "socket drift must not publish an inert supervision cycle or advance its watermark"
    );
    let reason = daemon.fatal_shutdown_reason().expect("drift must request process exit");
    assert!(reason.contains("new-owner-socket"), "reason identifies the new owner: {reason}");
    assert!(
        reason.contains("refusing mid-run adoption"),
        "reason proves it will not switch sockets: {reason}"
    );
}

#[tokio::test]
async fn bench_completion_is_acknowledged_only_by_a_post_actuation_gather() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let (_dir, company) = seed(clock.clone(), mc.wall().0).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let actuated = Arc::new(AtomicBool::new(false));
    let mut hooks = stubs::test_hooks();
    hooks.cycle_input = Arc::new(PostActuationBenchGather {
        calls: Arc::clone(&calls),
        actuated: Arc::clone(&actuated),
    });
    hooks.actuator = Arc::new(MarkingBenchActuator { actuated: Arc::clone(&actuated) });

    let completion = Arc::new(chiefd_api::docstore::BenchCompletionRegistry::default());
    let wait = completion.register(BenchCompletionKey {
        operation_id: "transition:1:signal-researcher:park".to_string(),
        person_id: "signal-researcher".to_string(),
    });
    let daemon = Daemon::new(SLUG, company, clock, hooks, ActuationMode::Apply)
        .with_bench_completion(completion);

    daemon.run_supervision_reconcile().await;

    tokio::time::timeout(Duration::from_secs(1), wait)
        .await
        .expect("post-actuation gather resolves the exact bench wait")
        .expect("registry sender remains live until acknowledgement");
    assert_eq!(calls.load(SeqCst), 2, "one pre-actuation and one completion gather");
    assert!(actuated.load(SeqCst));
}

/// #376: the exact production wiring (`run_company`'s `wire_change_feed`)
/// driving a REAL duty pass, not a hand-rolled `put_document`. Before #376
/// such a pass committed and nothing downstream of `CompanyDb` ever learned
/// about it; this pins that a wired company now publishes a change-feed hint
/// for every store the pass touches. Written against the deadline-evaluation
/// duty, carried by supervision-reconcile now that duty is deleted.
#[tokio::test]
async fn a_real_duty_pass_publishes_to_the_wired_change_feed() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let (_dir, company) = seed(clock.clone(), mc.wall().0).await;

    let feed = Arc::new(ChangeFeed::new());
    let composite_slug = format!("{SLUG}@test-root-digest");
    wire_change_feed(&company, Arc::clone(&feed), composite_slug.clone());
    let mut live = feed.subscribe();

    let daemon = daemon(company.clone(), clock);
    daemon.run_supervision_reconcile().await;

    // The pass commits `supervisor-watermark` at minimum (pinned by the
    // sibling test above); every store the ONE resulting commit touched must
    // have published a hint for this company's own slug.
    let mut seen_stores = Vec::new();
    while let Ok(event) = live.try_recv() {
        assert_eq!(
            event.slug, composite_slug,
            "the hint must use the mounted docstore composite key, never CompanyDb's bare label"
        );
        assert!(!event.removed, "an upsert commit must never publish as removed");
        seen_stores.push(event.store);
    }
    assert!(
        seen_stores.contains(&"supervisor-watermark".to_string()),
        "the real duty pass must publish a change-feed hint for its own \
         watermark commit, got: {seen_stores:?}"
    );
}

/// A normalized mailbox append bypasses the ordinary `Ledgers` snapshot, so it
/// relies on `CompanyDb::publish_row_feed_hint`.  The hint's old bare-company
/// label could never pass `/v1/docs/watch`'s exact composite-slug filter,
/// leaving live recipients parked until the fallback scan.  Pin both halves of
/// the wire identity here at the producer boundary.
#[tokio::test]
async fn normalized_mailbox_delta_publishes_the_composite_slug_and_person_store() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let (_dir, company) = seed(clock.clone(), mc.wall().0).await;
    let feed = Arc::new(ChangeFeed::new());
    let composite_slug = format!("{SLUG}@mailbox-feed-root-digest");
    wire_change_feed(&company, Arc::clone(&feed), composite_slug.clone());
    let mut live = feed.subscribe();
    let entry = serde_json::from_value(json!({
        "schemaVersion": 1,
        "id": "mailbox-feed-1",
        "organization": SLUG,
        "fromPersonId": "ceo",
        "to": "alex",
        "recipients": ["alex"],
        "body": "durable mail must wake Alex",
        "urgency": "normal",
        "createdAt": "2026-07-27T00:00:00.000Z",
        "person": "alex",
        "state": "pending",
        "updatedAt": 1_785_172_800_000_i64
    }))
    .expect("valid normalized mailbox row");

    company
        .mailbox_delta(
            "alex".to_string(),
            vec![entry],
            Vec::new(),
            "2026-07-27T00:00:00.000Z".to_string(),
            // Unauthenticated harness: an actor naming no person row is unjudged.
            String::new(),
        )
        .await
        .expect("mailbox delta commits");

    let event = tokio::time::timeout(Duration::from_secs(1), live.recv())
        .await
        .expect("row write publishes a feed hint")
        .expect("feed remains live");
    assert_eq!(event.slug, composite_slug);
    assert_eq!(event.store, "mailbox/alex");
    assert!(!event.removed);
}

#[tokio::test]
async fn startup_self_audit_runs_before_the_first_interval_tick() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let now = mc.wall().0;
    let (_dir, company) = seed(clock.clone(), now).await;

    // Pre-stamp SupervisionReconcile as long-silent: twenty of its 30 s windows
    // ago, well past the 3-window stale multiple, so the self-audit MUST raise
    // the retroactive backlog.
    let stale_at = now - 20 * Duty::SupervisionReconcile.interval_ms();
    company
        .mutate(MutationClass::Normal, MutationName("test.stale"), move |ledgers| {
            let manifest = organization::read(ledgers)?;
            let ctx = organization::company_context(&manifest)?;
            supervisor_watermark::record_success(
                ledgers,
                &ctx,
                Duty::SupervisionReconcile,
                stale_at,
            );
            Ok(())
        })
        .await
        .expect("stamp stale watermark");

    let daemon = daemon(company.clone(), clock);

    // serve() runs the audit first; run it explicitly here.
    daemon.run_startup_self_audit().await;

    // The backlog is now a durable health incident.
    let health = company.read(|snapshot| {
        snapshot.ledgers().document_body("health-monitor").unwrap_or_default().to_string()
    });
    assert!(
        health.contains("supervisor_duty_stalled"),
        "startup self-audit folded the missed-window backlog into health: {health}"
    );

    // Now bring up the interval tasks. With the clock un-advanced, every duty is
    // parked on its first sleep and NOT ONE has run — proof the audit completed
    // before the first tick.
    let (tx, rx) = watch::channel(false);
    let mut set = daemon.spawn_all(rx, ready_now());
    settle_sleeps(&mc, Duty::ALL.len()).await;
    assert_eq!(
        mc.pending_sleeps(),
        Duty::ALL.len(),
        "all duties parked on their first sleep before any tick fired"
    );

    let _ = tx.send(true);
    while set.join_next().await.is_some() {}
}

#[tokio::test]
async fn drive_fires_a_duty_once_per_interval() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();

    let counter = Arc::new(AtomicUsize::new(0));
    let pass: DutyPass = {
        let counter = counter.clone();
        Arc::new(move || {
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, SeqCst);
            }) as BoxFuture<'static, ()>
        })
    };

    let (tx, rx) = watch::channel(false);
    // MailboxWake's cadence is 30 s.
    let interval_ms = Duty::MailboxWake.interval_ms();
    let handle = tokio::spawn(drive(Duty::MailboxWake, pass, clock, rx, ready_now(), None, None));

    for _ in 0..3 {
        // Wait until the task is parked on its next sleep, then release exactly
        // one interval.
        settle_sleeps(&mc, 1).await;
        mc.advance(Duration::from_millis(u64::try_from(interval_ms).unwrap()));
    }
    settle_count(&counter, 3).await;
    assert_eq!(counter.load(SeqCst), 3, "one pass per interval, and no busy extra passes");

    let _ = tx.send(true);
    let _ = handle.await;
}

/// A counting duty pass: increments on every call and does nothing else.
fn counting_pass(counter: &Arc<AtomicUsize>) -> DutyPass {
    let counter = Arc::clone(counter);
    Arc::new(move || {
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            counter.fetch_add(1, SeqCst);
        }) as BoxFuture<'static, ()>
    })
}

/// THE GENESIS GATE, as a duty task experiences it.
///
/// `SupervisionReconcile` with a trigger wired runs one pass IMMEDIATELY on
/// entry — the reactive first-pass fix — and on a company being created that is
/// exactly the pass that refused `unknown-company`, 229 ms before genesis
/// committed the manifest it reads. The task must exist and run NOTHING until
/// the latch opens, then run that first pass with no clock advance at all.
#[tokio::test]
async fn a_duty_holds_its_first_pass_until_the_manifest_exists() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let counter = Arc::new(AtomicUsize::new(0));

    let (tx, rx) = watch::channel(false);
    let (ready_tx, ready_rx) = watch::channel(false);
    let trigger = Arc::new(Notify::new());
    let handle = tokio::spawn(drive(
        Duty::SupervisionReconcile,
        counting_pass(&counter),
        clock,
        rx,
        ready_rx,
        Some(Arc::clone(&trigger)),
        None,
    ));

    // Every chance to run a pass it must not run. Nothing parks on the clock
    // either: the task is on the latch, not on a timer.
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(counter.load(SeqCst), 0, "no duty pass runs before the manifest exists");
    assert_eq!(mc.pending_sleeps(), 0, "and the task is held at the gate, not on a timer");

    // Genesis commits; the latch opens; the immediate first pass fires with no
    // clock advance at all.
    ready_tx.send(true).expect("the latch opens");
    settle_count(&counter, 1).await;
    assert_eq!(counter.load(SeqCst), 1, "the held first pass runs the moment the company exists");

    let _ = tx.send(true);
    let _ = handle.await;
}

/// The property the graceful drain depends on: a duty still held at the gate
/// must stop PROMPTLY on shutdown and run nothing.
///
/// `Daemon::serve`'s phase-1 drain joins every duty task under a four-second
/// budget before aborting them, and `sigterm_grace`'s `duties_drained=true`
/// asserts the cooperative path was taken. A gate that ignored shutdown would
/// hold every task for the whole readiness budget and turn that clean drain
/// into an abort.
#[tokio::test]
async fn a_duty_held_at_the_gate_stops_on_shutdown_without_running_a_pass() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let counter = Arc::new(AtomicUsize::new(0));

    let (tx, rx) = watch::channel(false);
    // Held for the whole test: a DROPPED latch sender would end the wait for a
    // different reason than the one under test.
    let (_ready_tx, ready_rx) = watch::channel(false);
    let handle = tokio::spawn(drive(
        Duty::SupervisionReconcile,
        counting_pass(&counter),
        clock,
        rx,
        ready_rx,
        Some(Arc::new(Notify::new())),
        None,
    ));

    let _ = tx.send(true);
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("a duty held at the gate observes shutdown instead of waiting out the budget")
        .expect("the duty task does not panic");
    assert_eq!(counter.load(SeqCst), 0, "a duty that never passed the gate never ran a pass");
}

/// #370 regression: a pass ALREADY IN FLIGHT when shutdown is signalled must be
/// dropped at its next await point, not awaited to completion. Before this fix
/// `drive` did `pass().await` unconditionally, so a SIGTERM landing inside a
/// long-running duty pass waited the whole pass out. Here the pass
/// hangs forever (`pending()` — a stand-in for the wedged/long poll); the test
/// proves `drive` returns PROMPTLY once shutdown flips, and that the hung pass
/// never ran to completion. Without the fix, `drive` never returns and the
/// bounded `timeout` below fires — the exact hang this closes.
#[tokio::test]
async fn drive_cancels_an_in_flight_pass_the_instant_shutdown_is_signalled() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();

    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let pass: DutyPass = {
        let started = started.clone();
        let completed = completed.clone();
        Arc::new(move || {
            let started = started.clone();
            let completed = completed.clone();
            Box::pin(async move {
                started.fetch_add(1, SeqCst);
                // A pass that never returns on its own — the wedged/long-poll shape.
                std::future::pending::<()>().await;
                // Only reachable if a cancelled pass wrongly ran to completion.
                completed.fetch_add(1, SeqCst);
            }) as BoxFuture<'static, ()>
        })
    };

    let (tx, rx) = watch::channel(false);
    let trigger = Arc::new(Notify::new());
    let handle = tokio::spawn(drive(
        Duty::SupervisionReconcile,
        pass,
        clock,
        rx,
        ready_now(),
        Some(trigger.clone()),
        None,
    ));

    // Updated for the immediate-first-pass fix: a trigger-wired duty now
    // starts its pass immediately on entry, before ever reaching the
    // sleep-or-trigger select — so the pass is already in flight (and
    // already hung) without needing this test's own nudge to start it.
    settle_count(&started, 1).await;

    // SIGTERM equivalent: `drive` must abandon the in-flight pass and return.
    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("drive did not return after shutdown while a pass was in flight (#370 hang)")
        .expect("drive task panicked");
    assert_eq!(
        completed.load(SeqCst),
        0,
        "the in-flight pass was cancelled at its await point, never run to completion"
    );
}

/// The load-bearing proof for the "the real runtime wake" seam: a duty wired with a
/// [`Notify`] trigger runs a pass the instant the trigger fires, WITHOUT the
/// clock ever advancing — the wake is not "wait up to one interval and hope",
/// it is immediate. `SupervisionReconcile`'s cadence is used so this matches
/// exactly what `production_hooks`/`Daemon::with_reconcile_trigger` wire.
#[tokio::test]
async fn drive_runs_a_pass_immediately_when_its_trigger_fires_with_no_clock_advance() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();

    let counter = Arc::new(AtomicUsize::new(0));
    let pass: DutyPass = {
        let counter = counter.clone();
        Arc::new(move || {
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, SeqCst);
            }) as BoxFuture<'static, ()>
        })
    };

    let (tx, rx) = watch::channel(false);
    let trigger = Arc::new(Notify::new());
    let handle = tokio::spawn(drive(
        Duty::SupervisionReconcile,
        pass,
        clock,
        rx,
        ready_now(),
        Some(trigger.clone()),
        None,
    ));

    // Updated for the immediate-first-pass fix: `drive()` now runs one pass on
    // entry for any trigger-wired duty, before this test's own nudge. Wait for
    // that pass first (proves the immediate-pass fix), THEN prove the trigger
    // still wakes a SECOND pass with the clock never advanced (the original
    // claim this test was written for).
    settle_count(&counter, 1).await;
    assert_eq!(
        counter.load(SeqCst),
        1,
        "the immediate first pass ran before any nudge or clock advance"
    );

    // Let the task park on its first sleep-or-trigger race before nudging —
    // proves the nudge is what wakes pass #2, not a startup race.
    settle_sleeps(&mc, 1).await;
    assert_eq!(
        counter.load(SeqCst),
        1,
        "still just the one immediate pass; the interval has not elapsed and nothing nudged yet"
    );

    trigger.notify_one();
    settle_count(&counter, 2).await;
    assert_eq!(
        counter.load(SeqCst),
        2,
        "the trigger ran a second pass immediately, with the clock never advanced"
    );

    let _ = tx.send(true);
    let _ = handle.await;
}

/// A capped real start pass, or any durable change whose immediate wake lands
/// inside the floor, may not spend its permit early. The scheduler instead
/// arms one delayed nudge; the next pass re-observes live state. Use zero delay
/// here so this focused wiring test never sleeps on wall clock; production
/// passes `RECONCILE_FLOOR`.
#[tokio::test]
async fn a_floor_blocked_reconcile_request_schedules_one_delayed_nudge() {
    let trigger = Arc::new(Notify::new());
    let armed = Arc::new(AtomicBool::new(false));
    schedule_reconcile_floor_retry(
        Some(Arc::clone(&trigger)),
        Arc::clone(&armed),
        true,
        Duration::ZERO,
        Arc::new(chiefd_core::clock::SystemClock::default()),
    );

    tokio::time::timeout(Duration::from_secs(1), trigger.notified())
        .await
        .expect("a floor-blocked request schedules a follow-up wake");
    assert!(!armed.load(SeqCst), "the timer disarms after sending its one nudge");
}

#[tokio::test]
async fn many_floor_blocked_requests_arm_one_delayed_retry() {
    let trigger = Arc::new(Notify::new());
    let armed = Arc::new(AtomicBool::new(false));
    schedule_reconcile_floor_retry(
        Some(Arc::clone(&trigger)),
        Arc::clone(&armed),
        true,
        Duration::from_secs(60),
        Arc::new(chiefd_core::clock::SystemClock::default()),
    );
    schedule_reconcile_floor_retry(
        Some(trigger),
        Arc::clone(&armed),
        true,
        Duration::from_secs(60),
        Arc::new(chiefd_core::clock::SystemClock::default()),
    );

    assert!(armed.load(SeqCst), "one timer remains armed for the whole burst");
}

/// A duty with no trigger wired (`None`, the default for every duty but
/// `SupervisionReconcile`) is unaffected by the trigger branch at all — it
/// stays interval-only forever, proving the accelerator is additive.
#[tokio::test]
async fn drive_with_no_trigger_wired_never_runs_early() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();

    let counter = Arc::new(AtomicUsize::new(0));
    let pass: DutyPass = {
        let counter = counter.clone();
        Arc::new(move || {
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, SeqCst);
            }) as BoxFuture<'static, ()>
        })
    };

    let (tx, rx) = watch::channel(false);
    let interval_ms = Duty::SupervisionReconcile.interval_ms();
    let handle =
        tokio::spawn(drive(Duty::SupervisionReconcile, pass, clock, rx, ready_now(), None, None));

    settle_sleeps(&mc, 1).await;
    // Yield a while with the clock untouched: still zero passes.
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(counter.load(SeqCst), 0, "no trigger wired ⇒ no early pass");

    mc.advance(Duration::from_millis(u64::try_from(interval_ms).unwrap()));
    settle_count(&counter, 1).await;
    assert_eq!(counter.load(SeqCst), 1, "the interval alone still fires the pass");

    let _ = tx.send(true);
    let _ = handle.await;
}

/// #368: while its reactive channel is HEALTHY (a trigger is wired), a duty's
/// periodic timer is demoted to the slow fallback floor — the 30 s
/// ownership-probe cadence is SUPPRESSED. Advancing the old fast interval fires
/// nothing further; only crossing the slow floor self-heals with one more
/// fallback pass.
///
/// Updated for the immediate-first-pass fix: a trigger-wired duty now runs its
/// FIRST pass immediately on entry, before ever racing the clock or the
/// trigger (the reactive wake normally arrives via a successful mutation, but
/// a fresh boot's own first write can be exactly the one that fails, so a
/// duty relying only on the wake could never recover). This test now asserts
/// that immediate pass explicitly, then re-asserts the original claim this
/// test was written for — the fast interval stays suppressed in STEADY STATE,
/// only the slow floor fires again — one level up from pass #1.
#[tokio::test]
async fn a_wired_trigger_suppresses_the_fast_interval_until_the_slow_floor() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();

    let counter = Arc::new(AtomicUsize::new(0));
    let pass: DutyPass = {
        let counter = counter.clone();
        Arc::new(move || {
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, SeqCst);
            }) as BoxFuture<'static, ()>
        })
    };

    let (tx, rx) = watch::channel(false);
    let trigger = Arc::new(Notify::new());
    let handle = tokio::spawn(drive(
        Duty::SupervisionReconcile,
        pass,
        clock,
        rx,
        ready_now(),
        Some(trigger.clone()),
        None,
    ));

    // The immediate first pass fires before any clock advance at all — no
    // wake, no interval crossed, nothing but `drive()` starting.
    settle_count(&counter, 1).await;
    assert_eq!(
        counter.load(SeqCst),
        1,
        "a trigger-wired duty runs its first pass immediately, not after the floor"
    );

    settle_sleeps(&mc, 1).await;
    // Advance the WHOLE old fast cadence (30 s): the reactive channel is healthy,
    // so the periodic branch is parked on the slow floor and nothing further fires.
    let fast =
        Duration::from_millis(u64::try_from(Duty::SupervisionReconcile.interval_ms()).unwrap());
    mc.advance(fast);
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        counter.load(SeqCst),
        1,
        "the fast interval is suppressed while reactive is healthy (still just the immediate pass)"
    );

    // Advance the remainder up to the slow floor: exactly one MORE fallback pass.
    mc.advance(DEFAULT_REACTIVE_FALLBACK_FLOOR - fast);
    settle_count(&counter, 2).await;
    assert_eq!(counter.load(SeqCst), 2, "the slow fallback floor still self-heals a dropped nudge, on top of the immediate first pass");

    let _ = tx.send(true);
    let _ = handle.await;
}

/// #368: one waker signal must wake EVERY reactive duty, not just
/// `SupervisionReconcile`. A `notify_one` wakes a single waiter, so the fan-out
/// gives each reactive duty its own `Notify` and re-broadcasts the shared signal
/// to all of them — a stopped recipient's mail wakes the delivery duty and the
/// deadline duty as promptly as it converges panes.
#[tokio::test]
async fn a_single_reconcile_signal_fans_out_to_every_reactive_duty() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let (_dir, company) = seed(clock.clone(), 0).await;

    let signal = Arc::new(Notify::new());
    let daemon = Arc::new(
        Daemon::new(SLUG, company, clock, stubs::test_hooks(), ActuationMode::Shadow)
            .with_reconcile_trigger(signal.clone()),
    );

    let (tx, rx) = watch::channel(false);
    let mut set = tokio::task::JoinSet::new();
    let triggers = daemon.spawn_reactive_fanout(&mut set, &rx);
    // Assert the exact SET, not just its size: a bare count says "somebody
    // changed the number" while a named set says WHICH duty joined or left, and
    // membership here is a behavioural decision (a duty outside this set is only
    // as fresh as its own sleep). `ReminderDispatch` is a member because the
    // same event that arms a reminder may arm one NEARER than the sleep the duty
    // is already in the middle of; its read-only due-check keeps that affordable.
    let mut reactive: Vec<Duty> = triggers.keys().copied().collect();
    reactive.sort();
    let mut expected = Daemon::REACTIVE_DUTIES.to_vec();
    expected.sort();
    assert_eq!(reactive, expected, "every reactive duty gets its own trigger, and only those");
    assert_eq!(
        reactive,
        {
            let mut want = vec![
                Duty::SupervisionReconcile,
                Duty::MailboxWake,
                Duty::ReminderDispatch,
                Duty::HealthMonitor,
            ];
            want.sort();
            want
        },
        "reconcile + mailbox + reminder + health each get a trigger"
    );

    // Fire the ONE waker signal; every reactive duty's own trigger must wake.
    // `Notify` stores a permit for a not-yet-parked waiter, so awaiting after the
    // fan-out fires still returns — no lost wake, no ordering race.
    signal.notify_one();
    for &duty in Daemon::REACTIVE_DUTIES {
        let trigger = triggers.get(&duty).expect("reactive duty has a trigger");
        tokio::time::timeout(Duration::from_secs(1), trigger.notified())
            .await
            .unwrap_or_else(|_| panic!("{duty:?} was not woken by the fanned-out signal"));
    }

    let _ = tx.send(true);
    while set.join_next().await.is_some() {}
}

#[test]
fn reactive_fallback_defaults_to_one_minute_and_only_accepts_a_one_minute_or_longer_override() {
    // The backstop is sixty seconds, not three minutes: a missed wake or a dead
    // pane must not be invisible for minutes. The override may only LENGTHEN it.
    assert_eq!(reactive_fallback_floor_from(None), Duration::from_secs(60));
    assert_eq!(
        reactive_fallback_floor_from(Some("not-a-duration")),
        DEFAULT_REACTIVE_FALLBACK_FLOOR
    );
    assert_eq!(reactive_fallback_floor_from(Some("59999")), DEFAULT_REACTIVE_FALLBACK_FLOOR);
    assert_eq!(reactive_fallback_floor_from(Some("60000")), MIN_REACTIVE_FALLBACK_FLOOR);
    // A longer override is honoured verbatim -- pinned as a literal, not as
    // `DEFAULT_...`, so this stays a real assertion now that the default is the
    // minimum rather than three minutes.
    assert_eq!(reactive_fallback_floor_from(Some("180000")), Duration::from_secs(180));
    assert_eq!(DEFAULT_REACTIVE_FALLBACK_FLOOR, MIN_REACTIVE_FALLBACK_FLOOR);
}

// --- E8-S2 (#824): HealthMonitor becomes reactive and deadline-driven ------

/// Seed one pending (unconfirmed) health observation directly, the way a real
/// pass would after `apply_cycle` sees a `requires_confirmed_observation`
/// candidate for the first time — without needing a real host gather.
async fn seed_pending_observation(
    company: &Arc<CompanyDb>,
    fingerprint: &'static str,
    first_observed_at: i64,
) {
    company
        .mutate(MutationClass::Normal, MutationName("test.health_observation"), move |ledgers| {
            let manifest = organization::read(ledgers)?;
            let ctx = organization::company_context(&manifest)?;
            let (mut state, _warning) =
                chiefd_core::store::health::read(ledgers, &ctx).into_parts();
            state.observations.insert(
                fingerprint.to_string(),
                chiefd_core::store::health::HealthMonitorObservation {
                    first_observed_at: chiefd_core::isotime::iso_millis(first_observed_at),
                    last_observed_at: chiefd_core::isotime::iso_millis(first_observed_at),
                    count: 1,
                },
            );
            chiefd_core::store::health::write(ledgers, &state);
            Ok(())
        })
        .await
        .expect("seed pending observation");
}

/// A confirmation window closing 30s out is nearer than the floor: the
/// dynamic sleep is exactly that gap, not the 5-minute liveness expectation.
///
/// 30s, not the 90s this used to carry: the reactive floor moved 180s -> 60s,
/// so a 90s window is no longer nearer than the floor and the floor would
/// legitimately win. The property under test is unchanged - the fixture moved
/// inside the new floor so it can still observe it.
#[tokio::test]
async fn health_monitor_next_interval_sleeps_until_a_near_confirmation_deadline() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let now = mc.wall().0;
    let (_dir, company) = seed(clock.clone(), now).await;
    let first_observed_at =
        now - (chiefd_core::store::health::HEALTH_OBSERVATION_CONFIRMATION_MS - 30_000);
    seed_pending_observation(&company, "fp-near", first_observed_at).await;

    let daemon = daemon(company.clone(), clock);
    let next_interval = daemon.health_monitor_next_interval();
    assert_eq!(
        next_interval(),
        Duration::from_millis(30_000),
        "sleeps exactly until the confirmation window closes, not the reactive floor"
    );
}

/// With nothing pending confirmation, the dynamic sleep rests at the reactive
/// floor. Computing that idle sleep is a pure read of committed state: a quiet
/// company gains no synthetic deadline and no writer commit merely because the
/// scheduler asks when to wake next.
#[tokio::test]
async fn health_monitor_next_interval_rests_at_the_floor_when_nothing_is_pending() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let now = mc.wall().0;
    let (_dir, company) = seed(clock.clone(), now).await;

    let daemon = daemon(company.clone(), clock);
    let next_interval = daemon.health_monitor_next_interval();
    let before = company.snapshot().commit_seq();
    assert_eq!(
        next_interval(),
        super::reactive_fallback_floor(),
        "an idle health monitor sleeps at the reactive fallback floor"
    );
    assert_eq!(
        company.snapshot().commit_seq(),
        before,
        "reading an idle health deadline never enqueues or commits"
    );
}

/// #437 applied to HealthMonitor: a confirmation window already past must
/// still be evaluated promptly, but the sleep before that evaluation must
/// never be zero.
#[tokio::test]
async fn health_monitor_next_interval_never_sleeps_zero_on_an_overdue_deadline() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let now = mc.wall().0;
    let (_dir, company) = seed(clock.clone(), now).await;
    seed_pending_observation(&company, "fp-overdue", now).await;
    mc.advance(Duration::from_secs(60 * 60));

    let daemon = daemon(company.clone(), clock);
    let next_interval = daemon.health_monitor_next_interval();
    let first = next_interval();
    assert!(first > Duration::ZERO, "an overdue deadline must never produce a zero-delay sleep");
    assert_eq!(first, super::DEADLINE_EVALUATION_MIN_INTERVAL, "…but it IS evaluated promptly");
}

/// #437 applied to HealthMonitor: the SAME overdue confirmation window coming
/// back unswept (its observation is never re-gathered, so `apply_cycle` never
/// clears it) backs off geometrically to the floor rather than spinning; a
/// newly armed nearer deadline resets the backoff immediately.
#[tokio::test]
async fn health_monitor_next_interval_overdue_backs_off_to_the_floor() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let now = mc.wall().0;
    let (_dir, company) = seed(clock.clone(), now).await;
    seed_pending_observation(&company, "fp-stuck", now).await;
    mc.advance(Duration::from_secs(60 * 60));

    let daemon = daemon(company.clone(), clock);
    let next_interval = daemon.health_monitor_next_interval();
    assert_eq!(next_interval(), Duration::from_secs(1), "prompt first");
    assert_eq!(next_interval(), Duration::from_secs(2), "then back off");
    assert_eq!(next_interval(), Duration::from_secs(4));
    for _ in 0..20 {
        next_interval();
    }
    assert_eq!(
        next_interval(),
        DEFAULT_REACTIVE_FALLBACK_FLOOR,
        "settles at the shared idle floor"
    );

    // Control: the backoff is scoped to the ONE unclearable deadline. A newly
    // armed nearer deadline is honoured immediately and exactly.
    let later = mc.wall().0;
    seed_pending_observation(
        &company,
        "fp-newer",
        later - (chiefd_core::store::health::HEALTH_OBSERVATION_CONFIRMATION_MS - 45_000),
    )
    .await;
    assert_eq!(
        next_interval(),
        Duration::from_secs(45),
        "a changed earliest deadline resets the backoff and is slept to exactly"
    );
}

/// Rule 4 of the #437 guard: the overdue backoff is capped at the next
/// strictly-future deadline, so a stuck observation's backoff can never sleep
/// through a DIFFERENT, newer confirmation window arming.
#[tokio::test]
async fn health_monitor_next_interval_backoff_is_capped_by_the_next_future_deadline() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let now = mc.wall().0;
    let (_dir, company) = seed(clock.clone(), now).await;
    seed_pending_observation(&company, "fp-stuck", now).await;
    mc.advance(Duration::from_secs(60 * 60));
    let later = mc.wall().0;
    // A second confirmation window closing 5s AFTER the advanced clock —
    // nearer than the backoff would otherwise reach by its fourth overdue
    // pass (2s, 4s are still under 5s; 8s is not, so the cap must engage
    // there). Its `first_observed_at` is set so `+ HEALTH_OBSERVATION_CONFIRMATION_MS`
    // lands exactly at `later + 5s`.
    let confirmation_ms = chiefd_core::store::health::HEALTH_OBSERVATION_CONFIRMATION_MS;
    seed_pending_observation(&company, "fp-future", later + 5_000 - confirmation_ms).await;

    let daemon = daemon(company.clone(), clock);
    let next_interval = daemon.health_monitor_next_interval();
    assert_eq!(
        next_interval(),
        Duration::from_secs(1),
        "prompt first, capped by the 5s future deadline anyway"
    );
    assert_eq!(next_interval(), Duration::from_secs(2), "still under the 5s cap");
    assert_eq!(
        next_interval(),
        Duration::from_secs(4),
        "the raw backoff (4s) is still under the 5s cap — it is not clipped yet"
    );
    assert_eq!(
        next_interval(),
        Duration::from_secs(5),
        "the FOURTH pass's raw backoff (8s) exceeds the 5s future deadline, so it is capped there"
    );
}

/// A fan-out reconcile signal wakes `HealthMonitor` too, not just the four
/// pre-existing reactive duties — the row change that arms/clears a
/// confirmation window may need a nearer look than the sleep in progress.
#[tokio::test]
async fn health_monitor_wakes_on_reactive_signal() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let (_dir, company) = seed(clock.clone(), 0).await;

    let signal = Arc::new(Notify::new());
    let daemon = Arc::new(
        Daemon::new(SLUG, company, clock, stubs::test_hooks(), ActuationMode::Shadow)
            .with_reconcile_trigger(signal.clone()),
    );
    let (_tx, rx) = watch::channel(false);
    let mut set = tokio::task::JoinSet::new();
    let triggers = daemon.spawn_reactive_fanout(&mut set, &rx);
    let trigger = triggers.get(&Duty::HealthMonitor).expect("HealthMonitor has a reactive trigger");

    signal.notify_one();
    // 1s here is a deadlock detector, not a performance budget — deliberately
    // far above any realistic in-process notify latency, even under
    // contention; do not tune it down to just-barely-pass. The assertion is
    // "this eventually wakes at all," never "this wakes within N ms."
    tokio::time::timeout(Duration::from_secs(1), trigger.notified())
        .await
        .expect("HealthMonitor was not woken by the fanned-out reconcile signal");
}

/// #637: a direct normalized supervision write can arm an earlier deadline
/// while Duty #5 is parked on its fallback floor. Its post-commit event must
/// wake the shared reactive fan-out; unrelated stores or another company may
/// never do so.
#[tokio::test]
async fn a_supervision_write_wakes_reactive_duties_through_the_change_feed() {
    let feed = ChangeFeed::new();
    let changes = feed.subscribe();
    let trigger = Arc::new(Notify::new());
    let _task = spawn_supervision_schedule_wake(
        changes,
        "northstar@live".to_string(),
        Arc::clone(&trigger),
    );

    // See the sibling test: the negative control must be a store the reconcile
    // pass does not read, and `activity` stopped being one when a wake's own
    // idle-park release became a reconcile input.
    feed.publish("northstar@live", "launcher-catalog", "2026-07-28T00:00:00.000Z", false);
    feed.publish("other@live", "supervision", "2026-07-28T00:00:00.000Z", false);
    feed.publish("northstar@live", "supervision", "2026-07-28T00:00:00.000Z", true);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), trigger.notified()).await.is_err(),
        "unrelated, foreign, and removal events do not wake the scheduler"
    );

    feed.publish("northstar@live", "supervision", "2026-07-28T00:00:01.000Z", false);
    tokio::time::timeout(Duration::from_secs(1), trigger.notified())
        .await
        .expect("this company's committed supervision write wakes reactive duties");
}

/// THE PACKET'S ACTUAL BEHAVIOUR CHANGE, pinned.
///
/// The supervision ledger woke the reconcile fan-out from #637. The
/// ORGANIZATION MANIFEST -- the department tree and the roster, the authority
/// the cycle diffs to decide which panes should exist -- did not. That left
/// the single most operator-visible change in the product, structure, as the
/// one desired-state edit with no event source: create a department and the
/// head waited for the next periodic pass.
///
/// The negative controls are the load-bearing half. This must widen the feed
/// by exactly one store, not open it: an unrelated store, another company, and
/// a removal must all still be ignored, or the reconcile fan-out becomes a
/// wake-on-any-write and the backstop's cost argument stops holding.
#[tokio::test]
async fn a_manifest_write_wakes_reactive_duties_through_the_change_feed() {
    let feed = ChangeFeed::new();
    let changes = feed.subscribe();
    let trigger = Arc::new(Notify::new());
    let _task = spawn_supervision_schedule_wake(
        changes,
        "northstar@live".to_string(),
        Arc::clone(&trigger),
    );

    // The negative control is a store the reconcile pass genuinely does not
    // read. It was `activity` until the idle park became a reconcile input —
    // a wake writes one, and using it here would have pinned the very latency
    // that made a click take a minute.
    feed.publish("northstar@live", "launcher-catalog", "2026-08-11T00:00:00.000Z", false);
    feed.publish("other@live", "org-manifest", "2026-08-11T00:00:00.000Z", false);
    feed.publish("northstar@live", "org-manifest", "2026-08-11T00:00:00.000Z", true);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), trigger.notified()).await.is_err(),
        "an unrelated store, another company, and a removal still do not wake the scheduler"
    );

    feed.publish("northstar@live", "org-manifest", "2026-08-11T00:00:01.000Z", false);
    tokio::time::timeout(Duration::from_secs(1), trigger.notified())
        .await
        .expect("this company's committed manifest write wakes reactive duties");
}

/// THE REASON THE LOOP IS NOT DELETED, and the test that stops someone quietly
/// making the wake load-bearing later.
///
/// The wake is an accelerator; the periodic pass is the authority. Desired
/// state has an event source because a human authors it. OBSERVED state has
/// none -- a pane dies, the box reboots, tmux is briefly unreadable, and no
/// event anywhere describes any of it. So the pass must converge with the
/// signal wired and NEVER delivered, repeatedly, forever.
///
/// Asserted across THREE floors rather than one, because a single floor cannot
/// tell a level-triggered backstop from a one-shot catch-up: an implementation
/// that ran once after the first floor and then waited for a wake would satisfy
/// a single-floor assertion and still leave a company stuck the moment a wake
/// was dropped. This is also the assertion that fails if the backstop is ever
/// quietly lengthened back out -- the floor is now 60s precisely so a missed
/// wake is not invisible for minutes.
#[tokio::test]
async fn the_periodic_pass_converges_forever_with_a_wake_that_never_arrives() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();

    let counter = Arc::new(AtomicUsize::new(0));
    let pass: DutyPass = {
        let counter = counter.clone();
        Arc::new(move || {
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, SeqCst);
            }) as BoxFuture<'static, ()>
        })
    };

    let (tx, rx) = watch::channel(false);
    // Wired, and deliberately never notified for the whole test.
    let trigger = Arc::new(Notify::new());
    let handle = tokio::spawn(drive(
        Duty::SupervisionReconcile,
        pass,
        clock,
        rx,
        ready_now(),
        Some(Arc::clone(&trigger)),
        None,
    ));

    settle_count(&counter, 1).await;

    for floor in 1..=3 {
        settle_sleeps(&mc, 1).await;
        mc.advance(DEFAULT_REACTIVE_FALLBACK_FLOOR);
        settle_count(&counter, floor + 1).await;
        assert_eq!(
            counter.load(SeqCst),
            floor + 1,
            "floor {floor}: the backstop must converge on its own cadence, with no wake ever sent"
        );
    }

    let _ = tx.send(true);
    let _ = handle.await;
}

/// A minimal `tracing::Subscriber` that records every event's fields
/// (including its `message`) as one string per line, so a test can assert on
/// log content without pulling in a mocking crate. Spans are ignored — the
/// duty-supervision logging under test is all bare events.
struct CapturingSubscriber(Arc<Mutex<Vec<String>>>);

struct FieldCapture(String);

impl tracing::field::Visit for FieldCapture {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
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
        self.0.lock().unwrap_or_else(|p| p.into_inner()).push(capture.0);
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// #340 regression: `spawn_all` wires every duty through `supervise`, not
/// `drive` directly, precisely so a pass that panics is (1) logged loudly
/// with the duty name and the panic payload, and (2) does not end the duty —
/// it keeps firing on schedule afterward. Before this fix the panic unwound
/// the whole per-duty `tokio` task and nothing ever noticed: the duty was
/// gone for the rest of the process's life with zero trace.
#[tokio::test]
async fn a_panicking_pass_is_logged_and_the_duty_keeps_running() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();

    let attempts = Arc::new(AtomicUsize::new(0));
    let pass: DutyPass = {
        let attempts = attempts.clone();
        Arc::new(move || {
            let attempts = attempts.clone();
            Box::pin(async move {
                let attempt = attempts.fetch_add(1, SeqCst) + 1;
                if attempt == 1 {
                    panic!("boom-340-regression-test");
                }
            }) as BoxFuture<'static, ()>
        })
    };

    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let _tracing_guard = tracing::subscriber::set_default(CapturingSubscriber(log.clone()));

    let (tx, rx) = watch::channel(false);
    let interval_ms = Duty::MailboxWake.interval_ms();
    let handle =
        tokio::spawn(supervise(Duty::MailboxWake, pass, clock, rx, ready_now(), None, None));

    // First interval: the pass panics, killing `drive`'s task.
    settle_sleeps(&mc, 1).await;
    mc.advance(Duration::from_millis(u64::try_from(interval_ms).unwrap()));
    settle_count(&attempts, 1).await;

    // `supervise` must have caught the death and respawned `drive` — proven
    // by a FRESH pending sleep reappearing on the SAME outer task, and that
    // duty firing again on its very next interval.
    settle_sleeps(&mc, 1).await;
    mc.advance(Duration::from_millis(u64::try_from(interval_ms).unwrap()));
    settle_count(&attempts, 2).await;
    assert_eq!(attempts.load(SeqCst), 2, "the duty ran again after the panic instead of vanishing");

    let logged = log.lock().unwrap().join("\n");
    assert!(logged.contains("panicked"), "the panic must be logged loudly: {logged}");
    assert!(
        logged.contains("boom-340-regression-test"),
        "the panic payload must be captured in the log: {logged}"
    );
    assert!(logged.contains("MailboxWake"), "the duty name must be in the log: {logged}");

    let _ = tx.send(true);
    let _ = handle.await;
}

/// Serializes every test that mutates PROCESS environment.
///
/// `std::env::set_var` / `remove_var` write one table shared by the whole test
/// binary, and Rust runs these tests in parallel threads. Six tests here set or
/// clear `PI_BINARY_ENV` around a `parse_config` call, so one test's cleanup
/// could land between another's `set_var` and the read it was setting up for —
/// `parse_config_takes_the_pi_binary_from_the_environment` failed exactly that
/// way under a full `cargo test --workspace` while passing in isolation, which
/// is the signature of this bug and the reason it reads as a flake.
///
/// ANY test that touches `DATA_ROOT_ENV`, `RUNTIME_SOCKET_ENV`,
/// `PI_BINARY_ENV`, `LAUNCHER_ROOT_ENV` or `HOME` must hold this — a lock only
/// half the mutators take is not a lock, which is the failure mode being fixed
/// rather than a rule worth restating loosely. `PI_SOURCE_AGENT_DIR` is the one
/// exception and needs no change: it already has its own
/// `PI_SOURCE_AGENT_DIR_LOCK` with an RAII installer, and it names a variable
/// nothing here touches, so the two groups cannot collide.
///
/// Deliberately a plain `Mutex` and deliberately poison-tolerant — a panicking
/// test must not convert one failure into every later test failing to acquire,
/// which would bury the original cause under a wall of unrelated red.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`ENV_LOCK`], ignoring poisoning.
fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A real directory for `--dir`. `parse_config` canonicalizes its one input,
/// so a made-up path is refused before anything else is read.
fn company_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// THE ONE INPUT, and it must name a directory that is really there.
///
/// `--company <slug>` and `--data-root <dir>`/`CHIEFD_DATA_ROOT` are deleted:
/// a slug on argv was a second answer to a question the store's `organization`
/// row already answers, and the two roots that shared the name `data root`
/// one directory apart cost a full day on #13. What is left is a directory,
/// and the daemon refuses when it cannot resolve one.
#[test]
fn parse_config_requires_a_company_directory_that_exists() {
    let _env = env_guard();
    std::env::remove_var(super::PI_BINARY_ENV);

    let err = super::parse_config(["--pi-binary", "/opt/pi/bin/pi"].into_iter().map(String::from))
        .expect_err("a directory is required");
    assert!(err.contains("company directory is required"), "{err}");
    assert!(err.contains("--dir"), "the usage names the flag: {err}");

    let missing = company_dir().path().join("no-such-directory");
    let err = super::parse_config(
        ["--dir", missing.to_str().expect("utf8"), "--pi-binary", "/opt/pi/bin/pi"]
            .into_iter()
            .map(String::from),
    )
    .expect_err("a directory that is not there cannot be served");
    assert!(err.contains("cannot resolve the company directory"), "{err}");
    assert!(err.contains(missing.to_str().expect("utf8")), "the refusal names it: {err}");

    // And the retired flags are not merely ignored — an argument this daemon
    // does not answer is a refusal, so a caller still spelling the old surface
    // fails loudly instead of silently serving some other company.
    for retired in [vec!["--company", "cobalt"], vec!["--data-root", "/srv/orgs"]] {
        let dir = company_dir();
        let mut argv =
            vec!["--dir", dir.path().to_str().expect("utf8"), "--pi-binary", "/opt/pi/bin/pi"];
        argv.extend(retired.iter().copied());
        let err = super::parse_config(argv.into_iter().map(String::from))
            .expect_err("a retired flag must not be silently accepted");
        assert!(err.contains("unknown argument"), "{retired:?}: {err}");
    }
}

/// THE COMPANY KEY IS THE SOCKET FALLBACK, not a name.
///
/// It was the slug, and two directories may hold companies called the same
/// thing — so the fallback they shared would put one company's panes on the
/// other's runtime server. The key is `sha256(<dir>)[..12]`, which two
/// directories cannot share.
#[test]
fn parse_config_reads_the_runtime_socket_and_pi_binary() {
    let _env = env_guard();
    std::env::remove_var(super::RUNTIME_SOCKET_ENV);
    std::env::remove_var(super::PI_BINARY_ENV);
    let here = company_dir();
    let there = company_dir();
    // The pi binary never defaults — see
    // `parse_config_refuses_a_daemon_that_was_not_told_what_panes_exec`.
    let defaulted = super::parse_config(
        [
            "--dir",
            here.path().to_str().expect("utf8"),
            "--pi-binary",
            "/opt/pi/bin/pi",
            "--launcher-root",
            "/opt/launcher",
        ]
        .into_iter()
        .map(String::from),
    )
    .expect("parses");
    assert_eq!(
        defaulted.runtime_socket,
        crate::company_dir::company_key(&defaulted.dir),
        "the runtime socket falls back to the company key"
    );
    let elsewhere = super::parse_config(
        [
            "--dir",
            there.path().to_str().expect("utf8"),
            "--pi-binary",
            "/opt/pi/bin/pi",
            "--launcher-root",
            "/opt/launcher",
        ]
        .into_iter()
        .map(String::from),
    )
    .expect("parses");
    assert_ne!(
        defaulted.runtime_socket, elsewhere.runtime_socket,
        "two directories never share a fallback socket, however they are named"
    );
    assert_eq!(defaulted.pi_binary, std::path::PathBuf::from("/opt/pi/bin/pi"));
    assert!(!defaulted.serve_only, "ordinary chiefd run must retain its supervisory behavior");

    let overridden = super::parse_config(
        [
            "--dir",
            here.path().to_str().expect("utf8"),
            "--runtime-socket",
            "cobalt-live",
            "--pi-binary",
            "/opt/pi/bin/pi",
            "--launcher-root",
            "/opt/launcher",
        ]
        .into_iter()
        .map(String::from),
    )
    .expect("parses");
    assert_eq!(
        overridden.runtime_socket_demanded.as_deref(),
        Some("cobalt-live"),
        "a socket typed on argv is a DEMAND, and only a demand may contradict a live claim"
    );
    assert_eq!(overridden.pi_binary, std::path::PathBuf::from("/opt/pi/bin/pi"));
}

/// THE DIRECTORY IS CANONICALIZED BEFORE IT IS HASHED.
///
/// The company key digests the path, so `.`, a trailing component, or a
/// symlinked spelling of one directory would key one company two ways — and
/// the client, which canonicalizes its own cwd, would then read a rendezvous
/// whose key it disagrees with. That silent split is exactly what the
/// composite `slug@sha256(orgs_root)` existed to paper over.
#[test]
fn parse_config_canonicalizes_the_directory_so_one_company_has_one_key() {
    let _env = env_guard();
    std::env::remove_var(super::RUNTIME_SOCKET_ENV);
    let dir = company_dir();
    let real = dir.path().canonicalize().expect("canonical tempdir");
    std::fs::create_dir_all(real.join("child")).expect("child");
    let indirect = real.join("child").join("..");

    let config = super::parse_config(
        [
            "--dir",
            indirect.to_str().expect("utf8"),
            "--pi-binary",
            "/opt/pi/bin/pi",
            "--launcher-root",
            "/opt/launcher",
        ]
        .into_iter()
        .map(String::from),
    )
    .expect("parses");

    assert_eq!(config.dir, real, "one directory, one spelling, however the caller wrote it");
    assert_eq!(
        crate::company_dir::company_key(&config.dir),
        crate::company_dir::company_key(&real),
        "and therefore one key"
    );
}

/// THE COLD-ATTACH REGRESSION, daemon half.
///
/// `pi_binary` is published in every person's launch-catalog entry and is
/// literally the program their pane execs. It used to default to the bare name
/// `pi`, and NOTHING in the product ever set `CHIEFD_PI_BINARY` — so every
/// company that has ever run shipped a bare name to tmux and let the server's
/// PATH decide whether anybody could start. Reproduced from cold on a host
/// whose operator had pinned Pi with `TEAM_LAUNCHER_PI`: the preflight cleared
/// the host on that pin, the daemon never read it, the CEO pane died at
/// creation, and the actuator reported `unusable window dimensions "\t\n"`
/// once per second.
#[test]
fn parse_config_refuses_a_daemon_that_was_not_told_what_panes_exec() {
    let _env = env_guard();
    std::env::remove_var(super::PI_BINARY_ENV);

    let dir = company_dir();
    let error = super::parse_config(
        ["--dir", dir.path().to_str().expect("utf8")].into_iter().map(String::from),
    )
    .expect_err("a daemon that cannot be told what panes exec must not guess");

    assert!(error.contains("pi binary is required"), "{error}");
    // The recovery names both channels, because the operator may be starting
    // this daemon by hand.
    assert!(error.contains("--pi-binary"), "{error}");
    assert!(error.contains(super::PI_BINARY_ENV), "{error}");
}

/// A relative path is the same defect wearing a longer name: `Command` would
/// resolve `bin/pi` against the PANE's working directory, not chiefd's.
#[test]
fn parse_config_refuses_a_pi_binary_that_is_not_absolute() {
    let _env = env_guard();
    std::env::remove_var(super::PI_BINARY_ENV);

    let dir = company_dir();
    for relative in ["pi", "bin/pi", "./node_modules/.bin/pi"] {
        let error = super::parse_config(
            ["--dir", dir.path().to_str().expect("utf8"), "--pi-binary", relative]
                .into_iter()
                .map(String::from),
        )
        .expect_err("a relative pi binary must be refused");

        assert!(error.contains("must be an absolute path"), "{relative}: {error}");
        assert!(error.contains(relative), "the refusal must name the value: {error}");
    }
}

/// The other half of a two-crate agreement. `chief-cli` spawns this daemon and
/// writes the pi binary under this exact name; it links none of this crate, so
/// it asserts the same literal from its own side
/// (`attach::tests::the_pi_binary_environment_name_is_the_one_the_daemon_reads`).
/// A rename on either side leaves one of the pair red instead of producing a
/// daemon that silently never receives the value.
#[test]
fn the_pi_binary_environment_name_is_the_one_the_client_writes() {
    assert_eq!(super::PI_BINARY_ENV, "CHIEFD_PI_BINARY");
}

/// The environment channel is the one `chiefd` actually uses, so it gets its
/// own proof rather than riding on the flag's.
#[test]
fn parse_config_takes_the_pi_binary_from_the_environment() {
    let _env = env_guard();
    std::env::set_var(super::PI_BINARY_ENV, "/opt/harnesses/bin/pi");

    let dir = company_dir();
    let config = super::parse_config(
        ["--dir", dir.path().to_str().expect("utf8"), "--launcher-root", "/opt/launcher"]
            .into_iter()
            .map(String::from),
    )
    .expect("parses");

    assert_eq!(config.pi_binary, std::path::PathBuf::from("/opt/harnesses/bin/pi"));
    std::env::remove_var(super::PI_BINARY_ENV);
}

#[test]
fn parse_config_accepts_serve_only_and_refuses_combining_it_with_once() {
    let _env = env_guard();
    let dir = company_dir();
    let path = dir.path().to_str().expect("utf8");
    let reader = super::parse_config(
        [
            "--dir",
            path,
            "--pi-binary",
            "/opt/pi/bin/pi",
            "--serve-only",
            "--launcher-root",
            "/opt/launcher",
        ]
        .into_iter()
        .map(String::from),
    )
    .expect("serve-only reader parses");
    assert!(reader.serve_only);
    assert!(!reader.once);

    let error = super::parse_config(
        [
            "--dir",
            path,
            "--pi-binary",
            "/opt/pi/bin/pi",
            "--once",
            "--serve-only",
            "--launcher-root",
            "/opt/launcher",
        ]
        .into_iter()
        .map(String::from),
    )
    .expect_err("one-shot duty execution and a persistent reader are incompatible");
    assert!(error.contains("cannot be combined"), "{error}");
}

#[test]
fn parse_config_reads_the_launcher_root_and_refuses_rather_than_guessing() {
    let _env = env_guard();
    std::env::remove_var(super::LAUNCHER_ROOT_ENV);
    // THE RULE THIS PINS, and it is the whole point of deleting the pointer:
    // a daemon that cannot resolve resources REFUSES. It used to fall through
    // to `$HOME/.local/share/tribe-launcher`, which is a path a checkout never
    // occupies — so the daemon started, materialized every person with an
    // empty `extensions/`, and the CEO came up with no `org_*` tools while
    // genesis reported success. A refusal at parse time is louder by a day.
    //
    // The test binary lives in `target/…/deps/`, which has no `resources/`
    // beside it, so `resource_root_from_exe` is `None` here — which is exactly
    // the shape a developer running a freshly built binary gets, and why this
    // assertion is about THIS process rather than about the box.
    let dir = company_dir();
    let path = dir.path().to_str().expect("utf8");
    let refusal = super::parse_config(
        ["--dir", path, "--pi-binary", "/opt/pi/bin/pi"].into_iter().map(String::from),
    )
    .expect_err("an unresolvable resource root must refuse, never guess");
    assert!(refusal.contains("resource root"), "{refusal}");
    assert!(refusal.contains("--launcher-root"), "the refusal must name a way out: {refusal}");

    let overridden = super::parse_config(
        ["--dir", path, "--pi-binary", "/opt/pi/bin/pi", "--launcher-root", "/opt/launcher"]
            .into_iter()
            .map(String::from),
    )
    .expect("parses");
    assert_eq!(overridden.launcher_root, std::path::PathBuf::from("/opt/launcher"));

    std::env::set_var(super::LAUNCHER_ROOT_ENV, "/env/launcher");
    let from_env = super::parse_config(
        ["--dir", path, "--pi-binary", "/opt/pi/bin/pi"].into_iter().map(String::from),
    )
    .expect("parses");
    assert_eq!(from_env.launcher_root, std::path::PathBuf::from("/env/launcher"));
    std::env::remove_var(super::LAUNCHER_ROOT_ENV);

    // The flag outranks the environment, still.
    std::env::set_var(super::LAUNCHER_ROOT_ENV, "/env/launcher");
    let flag_wins = super::parse_config(
        ["--dir", path, "--pi-binary", "/opt/pi/bin/pi", "--launcher-root", "/opt/launcher"]
            .into_iter()
            .map(String::from),
    )
    .expect("parses");
    assert_eq!(flag_wins.launcher_root, std::path::PathBuf::from("/opt/launcher"));
    std::env::remove_var(super::LAUNCHER_ROOT_ENV);
}

/// The load-bearing proof: `production_hooks` wires the REAL `ConvergeActuator`
/// (not the no-op scaffold) and it ACTUATES a fake pane live — both when called
/// directly and end to end through the SupervisionReconcile duty. chiefd actuates
/// live by default; the destructive-action budget and the circuit breaker remain
/// as genuine safety limits (covered in `converge_apply/cycle/tests.rs`). No real
/// runtime is ever touched: the host is `chiefd-host`'s scripted the runtime.
mod production_wiring {
    use std::path::PathBuf;
    use std::sync::Arc;

    use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
    use chiefd_core::clock::SharedClock;
    use chiefd_core::runtime::duty_hooks::{ActuationMode, DutyContext, EffectEnvelope};
    use chiefd_core::store::activity::{self, LaunchFence, ReconcileInput};
    use chiefd_core::store::{organization, supervision, COMPANY_DB_FILENAME};
    use chiefd_core::test_support::{northstar_manifest, ManualClock};
    use chiefd_host::converge_apply::safety;
    use chiefd_host::proc::ProcReader;
    use chiefd_host::real::RealHostExecutor;

    use super::super::{production_hooks, Config, Daemon};

    const SLUG: &str = "northstar-conformance";
    const EPOCH: i64 = 1_784_116_800_000;
    const PERSON: &str = "signal-researcher";

    /// `production_hooks` wires `ActuatorConfig::root_pi_agent_dir` from
    /// `chiefd_host::converge_apply::cycle::root_pi_agent_dir()` —
    /// deliberately ambient (`$PI_SOURCE_AGENT_DIR` or `$HOME/.pi/agent`), not
    /// test-injectable, because it must resolve the same way a real daemon
    /// does. That is correct for production but leaves these two tests
    /// dependent on whatever real registry/auth files happen to exist on the
    /// box running them: measured on zipbox, `$HOME/.pi/agent/auth.json`
    /// exists but is empty (`{}`), so the CEO's provider has no matching
    /// credential and `build_launch_catalog` aborts with "no launch spec for
    /// person 'ceo'" before any runtime call happens — an environment-dependent
    /// false negative that `cargo test --workspace` (never previously run at
    /// head) surfaced for the first time tonight.
    ///
    /// Point `PI_SOURCE_AGENT_DIR` at an isolated tempdir carrying a minimal
    /// valid registry + credential for the guard's lifetime, mirroring
    /// `cycle::tests::operator_pi_agent_dir`'s fixture shape and
    /// `launcher_root_default::with_home`'s env-mutation-is-process-global
    /// discipline (same file, above). RAII rather than a closure-taking
    /// helper: the guarded tests are `#[tokio::test]` async fns whose
    /// `production_hooks`/`.await` calls span the whole body, and a `Drop`
    /// guard restores the environment at scope exit regardless of the
    /// `.await` points in between, without holding a lock across them.
    static PI_SOURCE_AGENT_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(super) struct IsolatedModelRegistry {
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
        // Held for the guard's whole lifetime (across this test's `.await`
        // points, which is safe: `cargo test` gives each test function its
        // own OS thread, and each `#[tokio::test]` here uses the default
        // current-thread flavor, so a contending test's thread just blocks
        // synchronously on this Mutex rather than deadlocking anything).
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl IsolatedModelRegistry {
        /// Serializes the env write against any other test in this binary
        /// that might touch `PI_SOURCE_AGENT_DIR`, for as long as the
        /// returned guard lives.
        pub(super) fn install() -> Self {
            let lock =
                PI_SOURCE_AGENT_DIR_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = tempfile::tempdir().expect("registry tempdir");
            chiefd_host::files::publish_atomically(
                &dir.path().join("models.json"),
                r#"{"providers":{}}"#,
                0o644,
            )
            .expect("write isolated registry");
            chiefd_host::files::publish_atomically(
                &dir.path().join("auth.json"),
                r#"{"openrouter":{"type":"api_key","key":"sk-production-wiring-fixture"}}"#,
                0o600,
            )
            .expect("write isolated auth registry");
            let previous = std::env::var_os("PI_SOURCE_AGENT_DIR");
            std::env::set_var("PI_SOURCE_AGENT_DIR", dir.path());
            Self { _dir: dir, previous, _lock: lock }
        }
    }

    impl Drop for IsolatedModelRegistry {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("PI_SOURCE_AGENT_DIR", value),
                None => std::env::remove_var("PI_SOURCE_AGENT_DIR"),
            }
        }
    }

    /// The production executor, unscripted. It fakes nothing because there is
    /// nothing left on this seam to fake: `HostExecutor` carries no pane verb,
    /// and the hooks these tests wire use it only for `/proc` reads and
    /// detached worker spawns.
    pub(super) fn plain_host() -> RealHostExecutor {
        RealHostExecutor::new(ProcReader::default())
    }

    pub(super) fn config(dir: &std::path::Path) -> Config {
        Config {
            dir: dir.to_path_buf(),
            runtime_socket: "northstar-sock".to_string(),
            runtime_socket_demanded: Some("northstar-sock".to_string()),
            pi_binary: PathBuf::from("/opt/pi/bin/pi"),
            launcher_root: dir.to_path_buf(),
            once: false,
            serve_only: false,
        }
    }

    /// Give one person the home `build_launch_catalog` gates on:
    /// `<dir>/.chief/agent/<personId>/`, with the real `sessions/` and the
    /// `skills` symlink `ensure_agent_home` writes. Without it the real
    /// actuator omits the person from its launch catalog and the CreateSession
    /// step aborts with "no launch spec" — the actuator is real here, so it
    /// needs the same on-disk inputs a live company has.
    ///
    /// The path is derived by `chiefd_host::agent_home::agent_home`, never
    /// composed here: a fixture that composes its own subject can look
    /// somewhere the writer does not write and then agree with itself for ever.
    /// The `skills` link is part of it on purpose — the gate used to refuse any
    /// symlink, so a fixture of plain directories would satisfy the OLD rule
    /// too and prove nothing about the change.
    fn materialize_person(dir: &std::path::Path, person_id: &str) {
        let home = chiefd_host::agent_home::agent_home(dir, person_id);
        std::fs::create_dir_all(home.join("sessions")).expect("sessions");
        let skills = dir.join(".pi").join("skills");
        std::fs::create_dir_all(&skills).expect("the company's own skills");
        std::os::unix::fs::symlink("../../../.pi/skills", home.join("skills")).expect("link");
    }

    // `session()` and `atomic_runtime_message_containing` are gone with the two
    // scripts and the argv assertions they served. The latter picked the
    // `start-server ; new-session` one-shot out of a recorded call log to prove
    // the daemon had actuated; chiefd issues no argv and records no call log,
    // so there is no message to find and nothing to find it in.

    // TOMBSTONE: `attach_idle_actuator`. It published a TRUSTED, EMPTY
    // actuation record so the wired actuator had a present, vouching actuator
    // to plan against -- without one, every action was withheld and the
    // assertions below passed while proving nothing. There is no record to
    // publish and no presence to withhold on: the desired set comes from the
    // manifest and the activity ledger, so a pass always has a subject and the
    // vacuity this helper defended against cannot occur.

    /// Turn actuation on exactly the way `run_company` does at boot — the real
    /// boot write, so these tests exercise what production runs (durable
    /// converge-safety config → apply, sweep live, stored budget override
    /// preserved).
    pub(super) async fn enable_live_actuation(company: &CompanyDb) {
        super::super::enable_live_actuation(company).await.expect("enable live actuation");
    }

    /// BUG-1's restart half: the boot actuation write must NOT reset a stored
    /// `budgetOverride` — pre-fix it hardcoded `budget_override=false`, so a
    /// daemon restart reverted the operator's flag before any cycle could read
    /// it (measured live, 2026-07-22).
    #[tokio::test]
    async fn boot_actuation_preserves_a_stored_budget_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(ManualClock::default());
        let company =
            CompanyDb::open(SLUG, &dir.path().join(COMPANY_DB_FILENAME), clock).expect("open");

        // A fresh, never-configured company still boots to apply with the
        // budget kept as a real limit.
        super::super::enable_live_actuation(&company).await.expect("boot write");
        let fresh = safety::read_safety_config(&company);
        assert!(matches!(fresh.actuation_mode, safety::ActuationMode::Apply));
        assert!(fresh.sweep_live);
        assert!(!fresh.budget_override_active, "the default keeps the budget real");

        // An operator-set override survives every later boot.
        safety::set_actuation_config(&company, safety::ActuationMode::Apply, true, true)
            .await
            .expect("operator override");
        super::super::enable_live_actuation(&company).await.expect("second boot write");
        let kept = safety::read_safety_config(&company);
        assert!(kept.budget_override_active, "the boot write preserves the stored override");
        assert!(matches!(kept.actuation_mode, safety::ActuationMode::Apply));
        assert!(kept.sweep_live);
    }

    /// BUG-1's twin, one field over, and the one that mattered (#751/#13): the
    /// boot actuation write must NOT reset a stored `actuationMode`.
    ///
    /// It hardcoded `Apply`, so `chiefd set-actuation-config --mode shadow`
    /// wrote a durable row that the next `chiefd run` overwrote. A company
    /// therefore could not be put in shadow at all, and since the web only
    /// hosts agents for a shadow company, no agent could ever answer through
    /// the browser. What made it cost a day rather than an hour is that the
    /// refusal an operator got back told them to set shadow AND restart the
    /// daemon — and the restart is what destroyed the setting, so doing
    /// exactly what the product asked could never work.
    ///
    /// Both directions are asserted. Only checking that shadow survives would
    /// pass just as well if the boot write had been deleted outright, which
    /// would stop every fresh company from ever actuating its runtime fleet.
    #[tokio::test]
    async fn boot_actuation_preserves_an_operator_set_shadow_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(ManualClock::default());
        let company =
            CompanyDb::open(SLUG, &dir.path().join(COMPANY_DB_FILENAME), clock).expect("open");

        // Never configured: the boot write still adopts apply, so a fresh
        // company actuates exactly as it always did.
        super::super::enable_live_actuation(&company).await.expect("first boot write");
        assert!(matches!(
            safety::read_safety_config(&company).actuation_mode,
            safety::ActuationMode::Apply
        ));

        // The operator chooses shadow — the API-hosted mode.
        safety::set_actuation_config(&company, safety::ActuationMode::Shadow, true, false)
            .await
            .expect("operator sets shadow");

        // ...and restarts the daemon, which is what the refusal message tells
        // them to do. Pre-fix this line is what silently undid the line above.
        super::super::enable_live_actuation(&company).await.expect("boot write after shadow");
        let kept = safety::read_safety_config(&company);
        assert!(
            matches!(kept.actuation_mode, safety::ActuationMode::Shadow),
            "a daemon restart must not overwrite the mode an operator configured"
        );
        assert!(kept.sweep_live, "the pointer sweep stays live regardless of mode");
    }

    // `create_session_script` and `duty_create_session_script` are gone. They
    // enumerated the exact reply sequence a terminal would give a converge
    // pass — two `has-session` observes, then the one-shot `new-session` parse
    // output — and chiefd issues none of it. Their fragility was itself the
    // evidence: both had to be re-tuned every time the observe path changed
    // shape, most recently when the cycle-input gather started consuming reply
    // #1. A backend that asks a display nothing has no reply sequence to keep
    // in step with.

    async fn open_seeded(dir: &std::path::Path, clock: SharedClock) -> Arc<CompanyDb> {
        let company =
            Arc::new(CompanyDb::open(SLUG, &dir.join(COMPANY_DB_FILENAME), clock).expect("open"));
        company
            .mutate(MutationClass::Normal, MutationName("test.seed"), move |ledgers| {
                let manifest = northstar_manifest(EPOCH);
                organization::create(ledgers, &manifest)?;
                supervision::seed(ledgers, &manifest)?;
                activity::seed(ledgers, &manifest)?;
                Ok(())
            })
            .await
            .expect("seed");
        company
    }

    /// Make one person desired-active through the real reconcile, so the actuator
    /// has a genuine spawn to plan.
    async fn activate(company: &CompanyDb, person: &str) {
        let person = person.to_string();
        company
            .mutate(MutationClass::Reconcile, MutationName("test.activate"), move |ledgers| {
                let manifest = organization::read(ledgers)?;
                let supervision = supervision::read(ledgers, &manifest)?;
                activity::reconcile(
                    ledgers,
                    &manifest,
                    &supervision,
                    &ReconcileInput {
                        launch_intent: LaunchFence::Unfenced,
                        requested_person_ids: vec![person.clone()],
                        watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
                    },
                )?;
                Ok(())
            })
            .await
            .expect("activate");
    }

    fn context(company: &Arc<CompanyDb>) -> DutyContext {
        DutyContext { slug: SLUG.to_string(), snapshot: company.snapshot() }
    }

    #[tokio::test]
    async fn the_wired_actuator_runs_a_live_apply_pass_against_the_committed_observation() {
        // Was `the_wired_actuator_actuates_a_fake_pane_live`, and the rename is
        // the finding: chiefd actuates nothing (#751/P8-P10). What this test
        // proved that still has a subject is the WIRING — `production_hooks`
        // builds an actuator bound to this company, and a reconcile through it
        // really runs in Apply and really plans the desired roster. What it can
        // no longer prove is an actuation count and the `start-server ;
        // new-session` argv, because the client applies the plan and reports
        // its own outcome.
        //
        // The one-shot argv assertions deleted with it pinned real rules that
        // did not disappear, only moved: that server configuration and session
        // creation ride ONE message, and that the SESSION itself carries the
        // `@organization_id` ownership tag. Both are `chief-cli`'s interpreter
        // to pin now, against the terminal it actually drives.
        let _registry = IsolatedModelRegistry::install();
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(ManualClock::default());
        let company = open_seeded(dir.path(), clock).await;
        // GENESIS ASKS FOR THE CEO, and this fixture models a created company,
        // so it has to ask too. `prepare_ceo_only` is the call genesis makes,
        // and it writes the root's start decision as a launch-intent entry.
        // (`chief attach` made it too, through `POST
        // /v1/org/runtime/prepare-ceo-only`, until chief-home-is-cwd §4c
        // deleted that route with the daemon-side CEO boot.)
        //
        // Nothing needed to say this before #1148: `activity::reconcile` gave
        // the root an unconditional `OrganizationRoot` lease, so every company
        // had at least one desired person for free and the assertion below was
        // satisfied whatever else happened. With the lease deleted, `active` is
        // derived purely from demand -- a company nobody asked for desires
        // nobody, and this test would pass an EMPTY roster through a wiring
        // check whose whole point is that the roster gets planned.
        company
            .prepare_ceo_only("1970-01-01T00:00:00.000Z".to_owned())
            .await
            .expect("the root's start decision, exactly as genesis records it");
        activate(&company, PERSON).await;
        enable_live_actuation(&company).await;
        // The CEO occupies the first admission slot, so the launch catalog
        // needs their materialization even though only PERSON was activated.
        materialize_person(dir.path(), "ceo");
        materialize_person(dir.path(), PERSON);

        let db_path = dir.path().join(COMPANY_DB_FILENAME).to_string_lossy().to_string();
        let (hooks, _reconcile_trigger, _surface_bound) = production_hooks(
            &company,
            Arc::new(plain_host()),
            &config(dir.path()),
            company.label(),
            &db_path,
            chiefd_core::runtime::attendance::ActuatorAttendance::new(company.clock().wall().0),
        );

        let report = hooks
            .actuator
            .reconcile(&context(&company), ActuationMode::Apply)
            .await
            .expect("reconcile");

        assert!(report.applied, "the wired actuator runs live");
        assert!(
            report.desired_people >= 1,
            "the desired roster is planned against the attached actuator: {report:?}"
        );
    }

    /// THE LINE AN OPERATOR READS, read back out of tracing itself.
    ///
    /// # What was measured
    ///
    /// On a live company every reconcile line said `planned=N actuated=0`, N
    /// from 1 to 8, across passes where people demonstrably came up and the
    /// round line never once said NOT converged. Both words were wrong. The
    /// count is `desired_people` — renamed from `planned_steps` when chiefd
    /// stopped emitting actions — and the log kept the pre-rename word;
    /// `actuated` was a field that could only ever be 0, because chiefd applies
    /// nothing. So the one line that judges a pass reported, once a second,
    /// that nothing was happening while everything was.
    ///
    /// # Why this test and not an assertion about the report
    ///
    /// There WAS an assertion about the report: `actuated_steps == 0`, in this
    /// very file, passing on every run. It pinned the value's SHAPE while the
    /// condition it described — what the operator is shown — was never driven.
    /// This drives it: a real company, the real production hooks, the daemon's
    /// own `run_supervision_reconcile`, and the actual emitted fields captured
    /// off the tracing subscriber.
    #[tokio::test]
    async fn the_reconcile_line_names_the_desired_count_and_no_actuation_count() {
        let _registry = IsolatedModelRegistry::install();
        let dir = tempfile::tempdir().expect("tempdir");
        let mc = Arc::new(ManualClock::default());
        let clock: SharedClock = mc.clone();
        let company = open_seeded(dir.path(), clock.clone()).await;
        company
            .prepare_ceo_only("1970-01-01T00:00:00.000Z".to_owned())
            .await
            .expect("the root's start decision");
        activate(&company, PERSON).await;
        enable_live_actuation(&company).await;
        materialize_person(dir.path(), "ceo");
        materialize_person(dir.path(), PERSON);

        let db_path = dir.path().join(COMPANY_DB_FILENAME).to_string_lossy().to_string();
        let (hooks, reconcile_trigger, _surface_bound) = production_hooks(
            &company,
            Arc::new(plain_host()),
            &config(dir.path()),
            company.label(),
            &db_path,
            chiefd_core::runtime::attendance::ActuatorAttendance::new(company.clock().wall().0),
        );
        let daemon = Arc::new(
            Daemon::new(SLUG, Arc::clone(&company), clock, hooks, ActuationMode::Apply)
                .with_reconcile_trigger(reconcile_trigger),
        );

        let log: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let _tracing_guard = tracing::subscriber::set_default(
                crate::run::tests::CapturingSubscriber(log.clone()),
            );
            daemon.run_supervision_reconcile().await;
        }

        let lines = log.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let pass = lines
            .iter()
            .find(|line| line.contains("reconcile actuation pass"))
            .unwrap_or_else(|| panic!("the pass must log its own line; captured: {lines:#?}"));
        assert!(
            !pass.contains("actuated"),
            "a count that can only ever be 0 must not be on the line an operator judges a pass \
             by: {pass}"
        );
        assert!(
            !pass.contains("planned"),
            "nothing is PLANNED here — chiefd publishes a desired set and emits no steps, and \
             `planned=` is the field's name from before that changed: {pass}"
        );
        assert!(pass.contains("desired="), "the line must name what the count actually is: {pass}");
    }

    #[tokio::test]
    async fn the_supervision_reconcile_duty_drives_the_real_actuator_end_to_end() {
        // The load-bearing e2e, with the half that still exists: the daemon's
        // SupervisionReconcile duty, wired with the real production hooks,
        // drives a real converge pass to completion. Its old proof — a
        // `new-session` in the recorded argv — is `chief-cli`'s now; the pass
        // completing, and recording that it completed, is the wiring fact this
        // daemon owns.
        //
        // Isolate the ambient model registry (see `IsolatedModelRegistry`) so
        // the outcome does not depend on this box's real `$HOME/.pi/agent`.
        let _registry = IsolatedModelRegistry::install();
        let dir = tempfile::tempdir().expect("tempdir");
        let mc = Arc::new(ManualClock::default());
        let clock: SharedClock = mc.clone();
        let company = open_seeded(dir.path(), clock.clone()).await;
        activate(&company, PERSON).await;
        enable_live_actuation(&company).await;
        materialize_person(dir.path(), "ceo");
        materialize_person(dir.path(), PERSON);

        let db_path = dir.path().join(COMPANY_DB_FILENAME).to_string_lossy().to_string();
        let (hooks, reconcile_trigger, _surface_bound) = production_hooks(
            &company,
            Arc::new(plain_host()),
            &config(dir.path()),
            company.label(),
            &db_path,
            chiefd_core::runtime::attendance::ActuatorAttendance::new(company.clock().wall().0),
        );
        let daemon = Arc::new(
            Daemon::new(SLUG, Arc::clone(&company), clock, hooks, ActuationMode::Apply)
                .with_reconcile_trigger(reconcile_trigger),
        );

        daemon.run_supervision_reconcile().await;

        // The duty's liveness watermark advanced (the pass completed).
        let watermark = company.read(|snapshot| {
            snapshot.ledgers().document_body("supervisor-watermark").unwrap_or_default().to_string()
        });
        assert!(
            watermark.contains("supervision_reconcile"),
            "the duty recorded a run: {watermark}"
        );
    }

    /// The other half of the wiring proof: `production_hooks`' delivery sink and
    /// its returned trigger are the SAME `Notify` — a real mailbox delivery
    /// (through `hooks.delivery`, exactly as `run_mailbox_wake` calls it) nudges
    /// the identical handle `Daemon::with_reconcile_trigger` wires onto the
    /// `SupervisionReconcile` duty's drive loop. Without this, "the real runtime wake"
    /// would be two disconnected `Notify`s and a mailbox wake would never
    /// actually accelerate anything.
    #[tokio::test]
    async fn a_mailbox_delivery_nudges_the_exact_trigger_the_scheduler_drive_loop_awaits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(ManualClock::default());
        let company = open_seeded(dir.path(), clock).await;

        let db_path = dir.path().join(COMPANY_DB_FILENAME).to_string_lossy().to_string();
        let (hooks, reconcile_trigger, _surface_bound) = production_hooks(
            &company,
            Arc::new(plain_host()),
            &config(dir.path()),
            company.label(),
            &db_path,
            chiefd_core::runtime::attendance::ActuatorAttendance::new(company.clock().wall().0),
        );

        let envelope = EffectEnvelope {
            id: "del-1".to_string(),
            kind: "person_reminder".to_string(),
            payload: serde_json::json!({
                "personId": PERSON,
                // The reminder's prose, under the key `evaluate_reminders`
                // actually writes. An envelope carrying no content fails
                // render (#76).
                "message": "[reminder]\n\nRebalance the book",
            }),
        };

        let outcome = hooks.delivery.deliver(&context(&company), vec![envelope]).await;
        assert_eq!(outcome.delivered, vec!["del-1".to_string()], "the mailbox row was staged");

        tokio::time::timeout(std::time::Duration::from_secs(1), reconcile_trigger.notified())
            .await
            .expect(
                "the delivery's wake nudged the SAME trigger the scheduler's drive loop awaits \
                 — production_hooks and Daemon::with_reconcile_trigger share one Notify",
            );
    }
}

// ---------------------------------------------------------------------------
// The mounted org_documents docstore surface.
//
// These drive the EXACT production mount — `super::spawn_docstore_mount`, the
// same `serve_bound` + watch-driven shutdown composition `Daemon::serve` uses —
// over a REAL bound socket with a REAL HTTP client (hyper), not a `oneshot`
// against the router. The proof is a genuine write→read across the wire and a
// port that no longer serves this test's rows once the shutdown signal drains
// the listener (a bare "connect must fail" would flake on ephemeral-port reuse
// — see `assert_docstore_port_released`).
// ---------------------------------------------------------------------------

use chiefd_api::docstore::{self, DocStore};
use http_body_util::BodyExt;

type TestClient = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    http_body_util::Full<hyper::body::Bytes>,
>;

fn http_client() -> TestClient {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build_http()
}

/// Send one request and collect the whole body — the docstore's responses are
/// small JSON, but the client streams frames so a large export would work too.
async fn send(
    client: &TestClient,
    method: &str,
    url: &str,
    body: &str,
    bearer: &str,
) -> (u16, String) {
    let request = hyper::Request::builder()
        .method(method)
        .uri(url)
        // Close after each response so no idle keep-alive connection lingers to
        // stall the server's graceful shutdown (which drains active connections).
        .header("connection", "close")
        .header("content-type", "application/json")
        // Every non-exempt route is behind the gate and there is no pass-through
        // arm left for a request that presents nothing. This is a REAL bearer
        // over a REAL socket, minted from the same runtime the mount verifies
        // against — the production shape, which is what this section claims to
        // drive.
        .header("authorization", format!("Bearer {bearer}"))
        .body(http_body_util::Full::new(hyper::body::Bytes::from(body.to_owned())))
        .expect("build request");
    let response = client.request(request).await.expect("send request");
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.expect("collect body").to_bytes();
    (status, String::from_utf8(bytes.to_vec()).expect("utf8 body"))
}

/// Assert the mounted docstore released `addr` once its mount task joined.
///
/// Joining the mount task orders the listener's `close(2)` before this check
/// (axum drops the listener before its graceful-shutdown future resolves), so
/// refusal is the expected path. But a freed ephemeral port is recyclable the
/// instant it closes: a parallel test — or another process on a shared host —
/// can bind `:0` and be LISTENING on the very same port a moment later, which
/// is not this mount leaking. So a successful connect convicts only if the wire
/// still serves `marker`, a row only THIS test's docstore could answer with: a
/// genuinely leaked listener fails, a recycled port cannot.
async fn assert_docstore_port_released(
    addr: std::net::SocketAddr,
    bearer: &str,
    slug: &str,
    store: &str,
    marker: &str,
) {
    if tokio::net::TcpStream::connect(addr).await.is_err() {
        return; // refused: the port is genuinely free
    }
    // Something answered. It is this mount's leaked listener only if it can
    // still read this test's own row back. A recycled port may belong to a
    // non-HTTP or black-hole listener, so bound the probe and acquit on error.
    let still_serving_this_store =
        tokio::time::timeout(Duration::from_secs(5), serves_row(addr, slug, store, marker, bearer))
            .await
            .unwrap_or(false);
    assert!(
        !still_serving_this_store,
        "the docstore port still serves this test's own row after graceful shutdown — the listener leaked"
    );
}

/// True iff the HTTP surface at `addr` still has an exactly-once event marker
/// for `slug` keyed on `marker` (the marker doubles as its own `keyDigest`, so
/// finding it back proves this exact surface answered, not a coincidence).
/// Any transport or HTTP failure is `false`: only a live chiefd surface
/// holding THIS test's file could produce the marker.
///
/// #830: was `/v1/locks/list`, deleted with the rest of the TTL lease.
/// `/v1/org/event-journal/read` is the same shape this helper needs — a
/// DocStore-direct route with no live-company gate (per its own doc comment,
/// "markers are a cross-producer primitive written before any company is
/// live"), same as the deleted locks routes were.
async fn serves_row(
    addr: std::net::SocketAddr,
    slug: &str,
    _store: &str,
    marker: &str,
    bearer: &str,
) -> bool {
    let request = hyper::Request::builder()
        .method("POST")
        .uri(format!("http://{addr}/v1/org/event-journal/read"))
        .header("connection", "close")
        .header("content-type", "application/json")
        // The bearer matters HERE most of all. Without it a genuinely leaked
        // listener would answer 401, this helper would read `false`, and the
        // port-release assertion would pass on exactly the leak it exists to
        // catch.
        .header("authorization", format!("Bearer {bearer}"))
        .body(http_body_util::Full::new(hyper::body::Bytes::from(
            serde_json::json!({ "slug": slug, "keyDigest": marker }).to_string(),
        )))
        .expect("build probe request");
    let Ok(response) = http_client().request(request).await else {
        return false;
    };
    if response.status() != hyper::StatusCode::OK {
        return false;
    }
    let Ok(collected) = response.into_body().collect().await else {
        return false;
    };
    String::from_utf8_lossy(&collected.to_bytes()).contains(marker)
}

/// A docstore config bound to an ephemeral port against `db_path`.
fn store_config(db_path: &str) -> docstore::Config {
    docstore::Config {
        bind: "127.0.0.1:0".to_string(),
        db_path: db_path.to_string(),
        read_pool: docstore::DEFAULT_READ_POOL,
        max_body_bytes: docstore::DEFAULT_MAX_BODY_BYTES,
    }
}

/// Bind the docstore against `db_path` AND attach an auth runtime, which is
/// what production does at every one of its serve sites.
///
/// Returns the `Bound` plus a bearer minted from that runtime's own secret for
/// the enrolled bootstrap operator.
///
/// The runtime is not decoration. `unauthenticated_mounts.rs` pins the fact
/// that this crate has two serve sites and both attach one, so a mount test
/// that binds WITHOUT a runtime is no longer driving the production mount it
/// claims to drive — every non-exempt route on it answers `401
/// caller-unauthenticated` and the round-trip below would prove nothing about
/// which file was opened. The operator identity is used because it is what
/// `authn::boot` self-enrols at daemon init: daemon-scoped, carrying no
/// company, and therefore able to reach the DocStore-direct event-journal
/// routes this section probes with.
async fn mounted_with_auth(
    dir: &std::path::Path,
    db_path: &str,
) -> (docstore::Bound, String, std::sync::Arc<chiefd_core::actor::CompanyDb>) {
    let company = std::sync::Arc::new(
        chiefd_core::actor::CompanyDb::open(
            "northstar",
            &dir.join(chiefd_core::store::COMPANY_DB_FILENAME),
            std::sync::Arc::new(chiefd_core::clock::SystemClock::default()),
        )
        .expect("open company for the mount's auth runtime"),
    );
    let secret = std::sync::Arc::new(b"mounted-docstore-test-secret".to_vec());
    let auth = std::sync::Arc::new(chiefd_api::authn::runtime::AuthRuntime::new(
        std::sync::Arc::clone(&company),
        std::sync::Arc::clone(&secret),
        60_000,
        8,
        std::sync::Arc::new(|| 1_000),
    ));
    auth.enroll_bootstrap_operator("operator", "c3BraQ==", "fp-operator")
        .await
        .expect("enrol the bootstrap operator");
    let identity = company
        .identity_read("operator".to_owned())
        .await
        .expect("read the enrolled operator")
        .expect("the bootstrap operator is enrolled");
    let bearer = chiefd_api::authn::issue_token_for(&secret, &identity, 1_000)
        .expect("mint the operator bearer");
    let bound =
        docstore::bind(&store_config(db_path)).await.expect("bind docstore").with_auth(Some(auth));
    (bound, bearer, company)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mounted_docstore_round_trips_and_is_the_single_source_of_truth() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("org.sqlite").display().to_string();

    // --- seed the operator's real file with a pre-existing typed row ---------
    // Written through a store that is then DROPPED (its writer thread exits), so
    // the mount below is the ONLY writer of this org.sqlite — the single-writer
    // invariant the whole cutover rests on.
    //
    // #830: was `seed.lock_acquire(...)`, deleted with the rest of the TTL
    // lease. `event_journal_rows::insert_if_absent` is the same
    // DocStore-direct, no-live-company-gate shape the deleted lock methods
    // had, called the same way `org_event_journal_insert`'s own handler calls
    // it — `DocStore::engine()`/`DocEngine::exec_interactive` are both public.
    {
        let seed = DocStore::open(&db_path, 4).expect("open seed store");
        seed.ensure_schema().await.expect("seed schema");
        let created = seed
            .engine()
            .exec_interactive(move |tx| {
                chiefd_core::store::event_journal_rows::insert_if_absent(
                    tx,
                    "northstar",
                    "seededBefore",
                    "seededBefore",
                    &serde_json::Map::new(),
                    0,
                )
                .map_err(|e| docstore::StoreError::Query(e.to_string()))
            })
            .await
            .expect("seed marker");
        assert!(created.created, "the seed marker must be newly written");
    }

    // --- mount exactly as `Daemon::serve` does -------------------------------
    let (bound, bearer, _company) = mounted_with_auth(dir.path(), &db_path).await;
    let addr = bound.local_addr().expect("bound addr");
    let (tx, rx) = watch::channel(false);
    let handle = super::spawn_docstore_mount("northstar".to_string(), bound, rx);
    let client = http_client();
    let base = format!("http://{addr}");

    // Health is 200 once the writer and typed schema are ready.
    let (status, body) = send(&client, "GET", &format!("{base}/v1/docs/health"), "", &bearer).await;
    assert_eq!(status, 200, "health after seed must be ok: {body}");
    assert!(body.contains("\"status\":\"ok\""), "health body: {body}");

    // The mount opened the OPERATOR'S file, not a fresh/empty one: the marker
    // seeded before the surface existed reads back through the surface.
    //
    // #830: the four calls below used to be `/v1/locks/list`/`/v1/locks/
    // acquire` round-trips; see `serves_row`'s doc comment for why
    // `/v1/org/event-journal/{read,insert-if-absent}` is the same shape.
    let (status, body) = send(
        &client,
        "POST",
        &format!("{base}/v1/org/event-journal/read"),
        &serde_json::json!({ "slug": "northstar", "keyDigest": "seededBefore" }).to_string(),
        &bearer,
    )
    .await;
    assert_eq!(status, 200, "read seeded marker: {body}");
    assert!(body.contains("seededBefore"), "seeded typed row must round-trip: {body}");

    // A genuine typed write→read round-trip across the wire through the mount.
    let (status, body) = send(
        &client,
        "POST",
        &format!("{base}/v1/org/event-journal/insert-if-absent"),
        &serde_json::json!({
            "slug": "northstar",
            "keyDigest": "writtenViaMount",
            "id": "writtenViaMount",
            "event": {},
            "createdAtMs": 0,
        })
        .to_string(),
        &bearer,
    )
    .await;
    assert_eq!(status, 200, "insert via mount: {body}");
    assert!(body.contains("\"created\":true"), "marker must be newly written: {body}");

    let (status, body) = send(
        &client,
        "POST",
        &format!("{base}/v1/org/event-journal/read"),
        &serde_json::json!({ "slug": "northstar", "keyDigest": "writtenViaMount" }).to_string(),
        &bearer,
    )
    .await;
    assert_eq!(status, 200, "read-back via mount: {body}");
    assert!(body.contains("writtenViaMount"), "the just-written typed row must read back: {body}");

    // --- shut the mount down on the daemon's signal --------------------------
    drop(client); // release any pooled connection before draining the listener
    let _ = tx.send(true);
    handle.await.expect("mount task joins on shutdown");

    // Single source of truth: the write made THROUGH the surface is durable in
    // the SAME file — a store freshly reopened on that path sees it. Reopened
    // only after shutdown so there is never a second concurrent writer.
    let reopened = DocStore::open(&db_path, 4).expect("reopen store");
    let marker = reopened
        .engine()
        .exec_interactive(move |tx| {
            chiefd_core::store::event_journal_rows::read_marker(tx, "northstar", "writtenViaMount")
                .map_err(|e| docstore::StoreError::Query(e.to_string()))
        })
        .await
        .expect("reopen read")
        .expect("the mount's write must be durable in the same org.sqlite");
    assert_eq!(
        marker.key_digest, "writtenViaMount",
        "reopened store must see the mount's write — one file, one source of truth"
    );

    // And the port is released once the listener has drained: a connect either
    // refuses outright or lands on a recycled ephemeral port that cannot serve
    // THIS test's row (see `assert_docstore_port_released`).
    assert_docstore_port_released(addr, &bearer, "northstar", "health", "writtenViaMount").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn docstore_mount_releases_its_port_on_the_shutdown_signal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("org.sqlite").display().to_string();
    {
        // Give it a schema so /health answers 200 while it is up, plus one
        // unique row so the post-shutdown port check can tell THIS mount's
        // listener from a recycled ephemeral port serving someone else's store.
        // #830: was `seed.lock_acquire(...)`; see `serves_row`'s doc comment.
        // The seeded `keyDigest` must equal `assert_docstore_port_released`'s
        // marker EXACTLY below (`serves_row` queries by exact keyDigest, not
        // substring containment the way the deleted lock-holder check did).
        let seed = DocStore::open(&db_path, 2).expect("open seed store");
        seed.ensure_schema().await.expect("seed schema");
        seed.engine()
            .exec_interactive(move |tx| {
                chiefd_core::store::event_journal_rows::insert_if_absent(
                    tx,
                    "northstar",
                    "portProbe",
                    "portProbe",
                    &serde_json::Map::new(),
                    0,
                )
                .map_err(|e| docstore::StoreError::Query(e.to_string()))
            })
            .await
            .expect("seed probe marker");
    }

    let (bound, bearer, _company) = mounted_with_auth(dir.path(), &db_path).await;
    let addr = bound.local_addr().expect("bound addr");
    let (tx, rx) = watch::channel(false);
    let handle = super::spawn_docstore_mount("northstar".to_string(), bound, rx);
    let client = http_client();

    // It is genuinely serving before shutdown.
    let (status, _body) =
        send(&client, "GET", &format!("http://{addr}/v1/docs/health"), "", &bearer).await;
    assert_eq!(status, 200, "surface must serve while mounted");

    // The same watch the duty tasks read stops the listener too.
    drop(client); // release any pooled connection before draining the listener
    let _ = tx.send(true);
    handle.await.expect("mount task joins on shutdown");

    // The port is released: refusal, or a recycled port that cannot serve THIS
    // test's probe row (see `assert_docstore_port_released`).
    assert_docstore_port_released(addr, &bearer, "northstar", "probe", "portProbe").await;
}

// --- one-store resolution (E10-S2/#763: every company owns its own file) ---
//
// TOMBSTONE: `one_store_resolution`. Its three tests pinned the resolver's
// dotfile-sibling shape (`<data_root>/.<slug>.chief.db`) and its composite
// `slug@sha256(data_root)` label. Both are deleted with the data root: a
// company is a directory and its store is unconditionally
// `<dir>/.chief/db/chief.db`, which `company_dir.rs`'s own tests pin — along
// with the invariant those three existed for, that two companies never share
// one file.

// --- P0: the normalized manifest is the only boot authority ----------------

/// Open a FRESH, empty company writer in a real directory — the store at
/// `<dir>/.chief/db/chief.db`, no manifest seeded — plus write a stale
/// on-disk artifact (`stale-artifact.json`) beside it when
/// `with_stale_artifact` is set. The file represents leftover junk from an old
/// build and must never be able to turn an empty SQL company into a known
/// company.
async fn empty_company_on_disk(
    clock: SharedClock,
    now: i64,
    with_stale_artifact: bool,
) -> (tempfile::TempDir, std::path::PathBuf, Arc<CompanyDb>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let company_root = dir.path().to_path_buf();
    if with_stale_artifact {
        let manifest = northstar_manifest(now);
        // `clippy.toml` bans `std::fs::write` outside the filesystem-effects
        // seam; use the crate's own atomic-publish primitive.
        chiefd_host::files::publish_atomically(
            &crate::company_dir::chief_dir(&company_root).join("stale-artifact.json"),
            &serde_json::to_string(&manifest).expect("serialize manifest"),
            0o644,
        )
        .expect("write stale-artifact.json");
    }
    let company =
        Arc::new(crate::company_dir::open(&company_root, clock).expect("open company db"));
    (dir, company_root, company)
}

/// A valid-looking stale on-disk artifact beside an empty CompanyDb is not
/// authority. Startup's only remaining seed step requires a typed SQL
/// manifest and a duty remains fail-closed; neither path imports the
/// projection.
#[tokio::test]
async fn boot_never_adopts_manifest_from_stale_artifact_projection() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let (_dir, company_root, company) =
        empty_company_on_disk(clock.clone(), mc.wall().0, true).await;

    assert!(
        crate::company_dir::chief_dir(&company_root).join("stale-artifact.json").is_file(),
        "negative control really has a valid derived projection"
    );
    assert!(
        !company.read(|snapshot| organization::exists(snapshot.ledgers())),
        "an empty company db starts with no manifest"
    );

    for store in chiefd_core::store::BOOT_ADOPTABLE_STORES {
        let error = crate::bootstrap::seed_store_if_absent(&company, store)
            .await
            .expect_err("a native ledger cannot be seeded without the SQL manifest");
        assert_eq!(
            error.code(),
            Some(organization::UNKNOWN_COMPANY),
            "{store} fails on the missing SQL authority"
        );
    }
    assert!(
        !company.read(|snapshot| organization::exists(snapshot.ledgers())),
        "the disk projection was never imported"
    );

    let daemon = daemon(company.clone(), clock);
    let before = company.snapshot().commit_seq();
    daemon.run_supervision_reconcile().await;
    assert_eq!(
        company.snapshot().commit_seq(),
        before,
        "a disk-only company stays unknown and the duty writes no commit"
    );
}

/// #105 second half: a genuinely FRESH company whose manifest is already in
/// normalized SQL is seeded at startup so its duties can actually run.
///
/// Measured before this: four duties refused `store never written` and the
/// daemon exited reporting success. The fix is a STARTUP step, not a read-time
/// fallback, which is why the read primitives in `store/activity.rs` and
/// `store/supervision.rs` still refuse on `Absent` (asserted by their own
/// tests): a ledger that vanishes from a LIVE company must stay a loud refusal
/// and must never be read as "empty", because for activity that is the input
/// that plans a kill for every staffed person.
#[tokio::test]
async fn boot_seeds_initial_native_ledgers_when_there_is_nothing_to_adopt() {
    let mc = Arc::new(ManualClock::default());
    let clock: SharedClock = mc.clone();
    let now = mc.wall().0;
    let (_dir, company_root, company) = empty_company_on_disk(clock.clone(), now, true).await;
    // THE MANIFEST'S SLUG MUST EQUAL THE WRITER ACTOR'S LABEL, which is now the
    // company key. This is not a fixture nicety — it is a hard coupling in
    // `chiefd-core`: `organization_rows::reconstruct` DERIVES the manifest's
    // `slug` from `CompanyDb::label()` rather than reading a stored column
    // (there is none), and the activity/supervision row projections then refuse
    // any ledger whose `organization` disagrees with that derived value. The
    // composite key this stage deletes carried the bare slug as its own prefix,
    // which is what made the derivation work; a directory hash carries nothing.
    // See this crate's report: `chiefd-core` has to store the slug before a
    // company can be named anything but its own key.
    let mut manifest = northstar_manifest(now);
    manifest.slug = crate::company_dir::company_key(&company_root);
    company
        .mutate(MutationClass::Normal, MutationName("test.seed-manifest"), move |ledgers| {
            organization::create(ledgers, &manifest)
        })
        .await
        .expect("typed SQL manifest seed succeeds");

    // Precondition: no ledger, and a read says so in the #105 vocabulary.
    let before = company.read(|snapshot| {
        let ledgers = snapshot.ledgers();
        let manifest = organization::read(ledgers).expect("SQL manifest present");
        supervision::read(ledgers, &manifest).err().map(|e| e.to_string())
    });
    assert_eq!(before.as_deref(), Some("store never written: supervision"));

    for store in chiefd_core::store::BOOT_ADOPTABLE_STORES {
        let seeded = crate::bootstrap::seed_store_if_absent(&company, store)
            .await
            .unwrap_or_else(|e| panic!("seeding {store} succeeds: {e}"));
        assert!(seeded, "{store} had nothing to adopt, so it is seeded");
    }

    // Both ledgers now READ — which is what lets the four duties run at all.
    company.read(|snapshot| {
        let ledgers = snapshot.ledgers();
        let manifest = organization::read(ledgers).expect("manifest");
        supervision::read(ledgers, &manifest).expect("supervision reads after seeding");
        chiefd_core::store::activity::read(ledgers, &manifest)
            .expect("activity reads after seeding");
    });

    // Idempotent: a second startup seeds nothing and commits nothing.
    let seq = company.snapshot().commit_seq();
    for store in chiefd_core::store::BOOT_ADOPTABLE_STORES {
        assert!(
            !crate::bootstrap::seed_store_if_absent(&company, store).await.expect("no-op"),
            "{store} is already present, so it is never re-seeded"
        );
    }
    assert_eq!(company.snapshot().commit_seq(), seq, "a present ledger is never clobbered");
}

// #63/#64: the live failure was a daemon that guessed its runtime socket from the
// slug while the company's own runtime-ownership claim named another server —
// silently foreign to its own company for 4911 passes, and a shadow fleet on the
// guessed server.
#[test]
fn an_unstated_socket_is_adopted_from_the_live_runtime_ownership_claim() {
    let (socket, provenance) =
        super::resolve_runtime_socket(None, Some("default"), "tribes-capital", "tribes-capital")
            .expect("adoptable");
    assert_eq!(socket, "default", "the company's own claim says where it runs");
    assert_eq!(provenance, "adopted-from-runtime-owner");
}

// THE UPGRADE. `cb63690a0` moved the client's last fallback tier off the shared
// string `"default"` and onto the company key, so every company created before
// it boots with a preference its live claim contradicts. That pair must ADOPT,
// not refuse: the client cannot read a claim before a daemon serves it, so the
// socket it names at spawn is a guess, and a guess must lose to the company's
// own record of where it runs. This test was the operator's real box.
#[test]
fn the_clients_preference_loses_to_a_live_claim_instead_of_refusing() {
    let (socket, provenance) =
        super::resolve_runtime_socket(None, Some("default"), "4cc439341aa9", "4cc439341aa9")
            .expect("a pre-cb63690a0 company must still start");
    assert_eq!(socket, "default", "the claim decides, and the boot proceeds");
    assert_eq!(provenance, "adopted-from-runtime-owner");
}

#[test]
fn a_demanded_socket_contradicting_a_live_claim_refuses_to_start() {
    let error = super::resolve_runtime_socket(
        Some("tribes-capital"),
        Some("default"),
        "tribes-capital",
        "tribes-capital",
    )
    .expect_err("a contradicted socket must not silently actuate");
    assert!(error.contains("'default'"), "the refusal names the real socket: {error}");
    assert!(error.contains("shadow fleet"), "and says what it prevents: {error}");
    // A refusal an operator cannot act on is half a refusal: this one used to
    // say "release the claim first" without saying how, so the only stated
    // recovery was a flag read out of a log file.
    assert!(error.contains("chief stop"), "and names the command that ends the claim: {error}");
}

#[test]
fn a_demanded_socket_that_agrees_is_simply_used() {
    let (socket, provenance) = super::resolve_runtime_socket(
        Some("default"),
        Some("default"),
        "tribes-capital",
        "tribes-capital",
    )
    .expect("agreeing");
    assert_eq!((socket.as_str(), provenance), ("default", "demanded"));
}

#[test]
fn nobody_claiming_leaves_the_previous_behaviour_untouched() {
    let _env = env_guard();
    // A never-launched company (or a test harness): nothing to contradict, so
    // a demanded socket stands and an unstated one still falls back — to the
    // client's preference, which `parse_config` already resolves to the company
    // key when the client named none. This is the case that must NOT start
    // refusing.
    let (demanded, _) =
        super::resolve_runtime_socket(Some("cobalt-live"), None, "0123456789ab", "0123456789ab")
            .expect("demanded, unclaimed");
    assert_eq!(demanded, "cobalt-live");
    let (fallback, provenance) =
        super::resolve_runtime_socket(None, None, "0123456789ab", "0123456789ab")
            .expect("unclaimed");
    assert_eq!((fallback.as_str(), provenance), ("0123456789ab", "client-preference"));
    // The e2e harness pins a throwaway socket through the environment; with no
    // claim to lose to, that preference is what the daemon runs on.
    let (preferred, provenance) =
        super::resolve_runtime_socket(None, None, "chiefd-test-7", "0123456789ab")
            .expect("unclaimed, preferred");
    assert_eq!((preferred.as_str(), provenance), ("chiefd-test-7", "client-preference"));
}

// ---------------------------------------------------------------------------
// TOMBSTONE: `mod launcher_root_default` and the three pointer tests
// ---------------------------------------------------------------------------
//
// Four tests lived here and all four are deleted with their subject rather
// than ported, because every one of them asserted a property of a mechanism
// that no longer exists:
//
//   * `mod launcher_root_default` pinned that the `$HOME`-derived fallback was
//     derived and not the hardcoded `/root/.local/share/tribe-launcher` — the
//     literal that made every macOS pane stamp a nonexistent directory. There
//     is no fallback now; `parse_config` refuses instead, and
//     `parse_config_reads_the_launcher_root_and_refuses_rather_than_guessing`
//     is where that is pinned.
//   * `the_installed_launcher_root_pointer_is_read`,
//     `an_absent_launcher_root_pointer_resolves_to_none` and
//     `a_blank_launcher_root_pointer_is_absent_not_an_empty_path` pinned the
//     read of `~/.chief/launcher-root`. The pointer is gone; resources are
//     resolved from the running binary's own location, and the equivalent
//     rules — absent when nothing is there, never an empty path — are pinned
//     in `host_primitives::install`'s own tests, hermetically, against a named
//     executable path rather than the real `current_exe()`.
//
// Deleting a feature means deleting its tests. Keeping these as "ported"
// assertions about a resolver that answers a different question would have
// been the worse half of both options.

// ---- A7: ONE credential bootstrap, called by both mounts ----

/// The whole reason `chiefd run --serve-only` may now serve under an enforced
/// gate: it builds the SAME auth runtime the supervisor builds, from the same
/// `<dir>/.chief/keys`, through this one helper. The refusal it replaces keyed
/// on the gate and said so — "rather than accidentally making a second
/// unauthenticated surface available" — and a snapshot reader that enrols the
/// operator is not that surface.
///
/// Two principals, minted 0600, INSIDE THE COMPANY DIRECTORY, and a live
/// runtime that resolves both. A test that only checked the return value would
/// pass on a helper that read the keys from anywhere at all — which is exactly
/// the split-brain `identity_keys` exists to prevent, and exactly what
/// `keys_dir_from_orgs_root`'s `<root>/../keys` derivation invited.
#[tokio::test]
async fn the_shared_credential_bootstrap_mints_both_principals_inside_the_company_directory() {
    use std::os::unix::fs::PermissionsExt as _;

    use chiefd_api::authn::middleware::IdentityLookup as _;

    let clock: SharedClock = Arc::new(ManualClock::starting_at(0, 1_000));
    let (dir, company) = seed(Arc::clone(&clock), 1_000).await;

    let runtime = super::ensure_daemon_auth_runtime(company, dir.path(), SLUG, Arc::new(|| 1_000))
        .await
        .expect("the snapshot reader and the supervisor share this bootstrap");

    let keys = crate::company_dir::keys_dir(dir.path());
    assert_eq!(keys, dir.path().join(".chief").join("keys"));
    for name in ["operator.key", "service.key"] {
        let path = keys.join(name);
        assert!(path.exists(), "{name} is minted inside the company's own .chief folder");
        assert!(path.starts_with(dir.path()), "{name} must never escape the company directory");
        assert_eq!(
            std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600,
            "{name} is owner-only from the first byte"
        );
    }
    assert!(runtime.get(identity_keys::OPERATOR_IDENTITY_ID).await.expect("readable").is_some());
    assert!(runtime.get(identity_keys::SERVICE_IDENTITY_ID).await.expect("readable").is_some());
}

/// A restart re-reads the same two files rather than re-minting them.
/// Regenerating either would orphan the public half already enrolled in this
/// company — and with two mounts now calling this, one directory can be
/// bootstrapped twice.
#[tokio::test]
async fn a_second_bootstrap_on_the_same_directory_preserves_both_keys() {
    let clock: SharedClock = Arc::new(ManualClock::starting_at(0, 1_000));
    let (dir, company) = seed(Arc::clone(&clock), 1_000).await;
    let operator = crate::company_dir::keys_dir(dir.path()).join("operator.key");

    super::ensure_daemon_auth_runtime(Arc::clone(&company), dir.path(), SLUG, Arc::new(|| 1_000))
        .await
        .expect("first mount");
    let minted = std::fs::read_to_string(&operator).expect("read the minted key");

    super::ensure_daemon_auth_runtime(company, dir.path(), SLUG, Arc::new(|| 2_000))
        .await
        .expect("second mount");
    assert_eq!(
        std::fs::read_to_string(&operator).expect("read again"),
        minted,
        "an existing trust anchor is preserved, never re-minted"
    );
}

// TOMBSTONE: `a_root_with_no_parent_refuses_rather_than_guessing_a_keys_
// directory`. It pinned `identity_keys::keys_dir_from_orgs_root`'s `None` for
// a root with no parent — the failure mode of deriving the keys directory by
// walking UP from the orgs root. The keys hang off the company directory now
// (`<dir>/.chief/keys`), so there is no parent to be missing and no branch
// left to refuse on.

/// THE MINUTE A WAKE USED TO TAKE.
///
/// Measured on a live company: the operator clicked a sleeping person at
/// 18:48:52 and their pane appeared at 18:49:53 — sixty-one seconds, which is
/// `reactive_fallback_floor` timing out, not this trigger firing.
///
/// `org_ops::wake_person` writes a launch-intent fence row and an idle-park
/// release. Both are first-order reconcile inputs — `person_can_run` reads the
/// fence to decide whether somebody may start at all — but neither is the
/// supervision ledger nor the organization manifest, so the predicate answered
/// false and the most latency-sensitive gesture in the product waited out an
/// interval meant for writes nobody is watching.
#[test]
fn a_wake_s_own_writes_are_reconcile_inputs_and_nudge_the_duty() {
    use chiefd_core::store::launch_intent_rows::LAUNCH_INTENT_STORE;

    assert!(
        super::is_reconcile_input_store(LAUNCH_INTENT_STORE),
        "the launch fence decides who may run; a wake writes it and must not wait a minute \
         to be noticed"
    );
    assert!(
        chiefd_core::store::activity::is_activity_store("activity")
            && super::is_reconcile_input_store("activity"),
        "the idle park is what the settle withdrew and what a wake gives back"
    );
    assert!(
        !super::is_reconcile_input_store("org-documents-unrelated"),
        "and a store the pass does not read still wakes nothing: this is a scheduling \
         signal, not a subscription to everything"
    );
}

/// The blindness the 22:17:40Z outage exposed, and the false alarm that was
/// firing beside it the whole time.
///
/// Two defects, one module, because they are two halves of one sentence: chiefd
/// reported a live company it could not see, and the one alarm it did raise
/// reached nobody.
mod runtime_blindness {
    use std::sync::{Arc, Mutex};

    use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
    use chiefd_core::clock::SharedClock;
    use chiefd_core::runtime::attendance::{ActuatorAttendance, ACTUATOR_LAPSE_MS};
    use chiefd_core::runtime::duty_hooks::ActuationMode;
    use chiefd_core::store::{activity, organization, supervision, COMPANY_DB_FILENAME};
    use chiefd_core::test_support::{northstar_manifest, ManualClock};

    use super::super::{production_hooks, Daemon};
    use super::production_wiring::{config, plain_host, IsolatedModelRegistry};
    use super::CapturingSubscriber;

    const EPOCH: i64 = 1_784_116_800_000;

    /// A company whose ROW KEY IS NOT ITS NAME — the shape every live company
    /// has and no fixture in this file had.
    ///
    /// `CompanyDb::label()` is the directory key (`sha256(<dir>)[..12]`) and is
    /// what every normalized row is written under; `manifest.slug` is the
    /// display name genesis was given, stored in `org_settings.display_slug`.
    /// Every other fixture here opens the writer under the same string it names
    /// the manifest with, which makes the two indistinguishable and is exactly
    /// why a reader that reached for the wrong one has been raising a false
    /// alarm since the port with a full green suite over it.
    const KEY: &str = "f7c6f2358be9";
    const NAME: &str = "foundboot-labs";

    async fn open_named_company(dir: &std::path::Path, clock: SharedClock) -> Arc<CompanyDb> {
        let company =
            Arc::new(CompanyDb::open(KEY, &dir.join(COMPANY_DB_FILENAME), clock).expect("open"));
        company
            .mutate(MutationClass::Normal, MutationName("test.seed"), move |ledgers| {
                let mut manifest = northstar_manifest(EPOCH);
                manifest.slug = NAME.to_string();
                organization::create(ledgers, &manifest)?;
                supervision::seed(ledgers, &manifest)?;
                activity::seed(ledgers, &manifest)?;
                Ok(())
            })
            .await
            .expect("seed");
        company
    }

    /// Assemble the real production hooks and a daemon over `company`, sharing
    /// one attendance cell whose last read was `silent_for` milliseconds ago.
    async fn daemon_with_attendance(
        dir: &std::path::Path,
        company: &Arc<CompanyDb>,
        clock: SharedClock,
        silent_for: i64,
    ) -> Arc<Daemon> {
        let db_path = dir.join(COMPANY_DB_FILENAME).to_string_lossy().to_string();
        // The COMPANY's clock, the same one the desired-set route stamps off and
        // the same one the health gatherer reads. A wall-clock reading here
        // would sit decades away from a `ManualClock` company's own time and the
        // silence would read as a negative age.
        let attendance = ActuatorAttendance::new(company.clock().wall().0 - silent_for);
        let (hooks, _trigger, _bound) = production_hooks(
            company,
            Arc::new(plain_host()),
            &config(dir),
            company.label(),
            &db_path,
            attendance.clone(),
        );
        Arc::new(
            Daemon::new(KEY, Arc::clone(company), clock, hooks, ActuationMode::Apply)
                .with_actuator_attendance(attendance),
        )
    }

    fn captured(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        log.lock().unwrap_or_else(|poison| poison.into_inner()).clone()
    }

    /// THE PRIMARY DEFECT. A company nobody is converging must say so on every
    /// supervision pass.
    ///
    /// On 2026-08-18 the whole tmux server went away with eleven panes and five
    /// people in it, and this line went on reading `supervision cycle
    /// committed` every five seconds for forty minutes. The cycle really was
    /// committing — the ledger work is correct with the display gone — so the
    /// only thing that could have told an operator was the pass naming its own
    /// reach.
    #[tokio::test]
    async fn a_supervision_pass_nobody_is_converging_says_so() {
        let _registry = IsolatedModelRegistry::install();
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(ManualClock::default());
        let company = open_named_company(dir.path(), clock.clone()).await;
        let daemon =
            daemon_with_attendance(dir.path(), &company, clock, ACTUATOR_LAPSE_MS + 1_000).await;

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let _guard = tracing::subscriber::set_default(CapturingSubscriber(Arc::clone(&log)));
            daemon.run_supervision_reconcile().await;
        }

        let lines = captured(&log);
        assert!(
            lines.iter().any(|line| line.contains("NOBODY IS CONVERGING THIS COMPANY")),
            "an unattended pass must name its own reach; captured: {lines:#?}"
        );
    }

    /// And an ATTENDED company stays quiet — otherwise the warning is noise and
    /// an operator learns to scroll past it.
    #[tokio::test]
    async fn a_supervision_pass_somebody_is_converging_does_not() {
        let _registry = IsolatedModelRegistry::install();
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(ManualClock::default());
        let company = open_named_company(dir.path(), clock.clone()).await;
        let daemon = daemon_with_attendance(dir.path(), &company, clock, 0).await;

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let _guard = tracing::subscriber::set_default(CapturingSubscriber(Arc::clone(&log)));
            daemon.run_supervision_reconcile().await;
        }

        let lines = captured(&log);
        assert!(
            !lines.iter().any(|line| line.contains("NOBODY IS CONVERGING")),
            "an attended company must not be warned about; captured: {lines:#?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("supervision cycle committed")),
            "the pass still reports itself; captured: {lines:#?}"
        );
    }

    /// THE SECOND DEFECT. A health incident must reach a reader.
    ///
    /// `run_health_monitor` computed `apply_cycle`'s outcome and dropped it, so
    /// a monitor that had raised the same incident 707 consecutive times told
    /// nobody, anywhere, ever.
    #[tokio::test]
    async fn a_raised_health_incident_reaches_the_daemon_log() {
        let _registry = IsolatedModelRegistry::install();
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(ManualClock::default());
        let company = open_named_company(dir.path(), clock.clone()).await;
        let daemon =
            daemon_with_attendance(dir.path(), &company, clock, ACTUATOR_LAPSE_MS + 1_000).await;

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let _guard = tracing::subscriber::set_default(CapturingSubscriber(Arc::clone(&log)));
            daemon.run_health_monitor().await;
        }

        let lines = captured(&log);
        let raised: Vec<&String> =
            lines.iter().filter(|line| line.contains("health incident RAISED")).collect();
        assert!(
            raised.iter().any(|line| line.contains("runtime_unattended")),
            "the unattended runtime must be reported to a reader, not only to a document; \
             captured: {lines:#?}"
        );
    }

    /// THE FALSE ALARM. A company whose supervision duty is succeeding must not
    /// accuse its own supervisor of being absent.
    ///
    /// `supervisor_not_running` fired 707 consecutive times on a live company —
    /// continuously, from its first second — while the duty behind it recorded
    /// a success every five seconds. The reader looked the watermark up under
    /// the DISPLAY name while the writer keys it by the ROW KEY, so the row was
    /// never going to be found for any company that has ever run.
    ///
    /// This test can only fail on the tree it was written for if `KEY != NAME`;
    /// that is the whole fixture, and it is why the assertion lives in its own
    /// module rather than borrowing `production_wiring`'s company.
    #[tokio::test]
    async fn a_company_whose_key_is_not_its_name_does_not_report_its_own_supervisor_absent() {
        let _registry = IsolatedModelRegistry::install();
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(ManualClock::default());
        let company = open_named_company(dir.path(), clock.clone()).await;
        let daemon = daemon_with_attendance(dir.path(), &company, clock, 0).await;

        // The duty records its own success watermark — the fact the health
        // reader is about to go looking for.
        daemon.run_supervision_reconcile().await;
        daemon.run_health_monitor().await;

        let health = company.read(|snapshot| {
            snapshot.ledgers().document_body("health-monitor").unwrap_or_default().to_string()
        });
        assert!(
            !health.contains("supervisor_not_running"),
            "a duty that just succeeded must not read as an absent supervisor; the health \
             document says otherwise: {health}"
        );
    }
}

/// THE ACTUATION RECORD, as a rule.
///
/// A live company relaunched six people forty-five seconds after an operator
/// stood it down, and `daemon.log` for that window held nothing but
/// `supervision cycle committed`. The line naming who was launched and why —
/// `mail wake granted launch intent: <names>` — was on the report and was
/// written at DEBUG, because the only question asked was `changed`.
mod actuation_pass_log_level {
    use crate::run::actuation_pass_log_level as level;
    use tracing::Level;

    /// THE DEFECT. A pass that granted, withdrew or refused launch intent is
    /// news even when the desired set is what it already was — which is
    /// precisely the state a quiet company sits in, and therefore the state
    /// almost every wake lands next to.
    #[test]
    fn a_launch_decision_is_visible_even_when_the_audit_body_did_not_change() {
        assert_eq!(
            level(false, true),
            Level::INFO,
            "a wake grant, a settle withdrawal or a refused mail demand must reach an operator \
             who is running at the default level"
        );
    }

    /// #367, untouched: a steady company being desired-up is the ordinary
    /// state, true on every pass, and never news.
    #[test]
    fn a_pass_that_recorded_nothing_and_decided_nothing_stays_silent() {
        assert_eq!(level(false, false), Level::DEBUG);
    }

    /// The original rule still stands on its own.
    #[test]
    fn a_pass_that_recorded_something_new_is_visible() {
        assert_eq!(level(true, false), Level::INFO);
        assert_eq!(level(true, true), Level::INFO);
    }
}
