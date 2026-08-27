//! [`ReconcileWaker`] — the production [`RuntimeWaker`] for chiefd-host.
//!
//! `chiefd-core` delivers mail by durably staging every envelope
//! ([`MailboxDeliverySink`](chiefd_core::runtime::delivery_sink)); its injected
//! wake seam ([`RuntimeWaker`]) only decides how a newly-pending recipient is
//! made to pick up that mail *promptly* rather than on its own next loop. This
//! module is the real seam — until now only test fakes implemented it.
//!
//! # What "waking" actually is (the ported TS mechanism)
//!
//! Contrary to the intuitive "type into the pane" model, the live TypeScript
//! launcher does **not** `send-keys` into a recipient's pane on new mail. Its
//! wake path is a **targeted runtime reconcile**:
//! `wakePendingOrganizationMailboxes` scans the durable mailboxes for pending
//! recipients and calls `reconcileOrganizationRuntime(slug, { requestedPersonIds })`
//! (`src/organization/org-supervision-transport.ts:1163-1165`); the strict
//! forced-projection path does the same reconcile with a forced projection and
//! rethrows on failure (`org-supervision-transport.ts:978-988`). "Waking" a
//! recipient is thus *ensuring that recipient's live pane exists* — the resident
//! pi agent drains its own durable mailbox on boot — which is exactly what the
//! [`RuntimeWaker`] trait documents (`chiefd_core::store::mailbox`). A recipient
//! already live needs nothing: it drains in-process.
//!
//! # The Rust port: nudge the reconcile duty, do not re-implement it
//!
//! The Rust equivalent of `reconcileOrganizationRuntime` already exists and is
//! the one thing allowed to spawn/respawn panes: the daemon's
//! `SupervisionReconcile` duty
//! ([`ConvergeActuator`](crate::converge_apply::ConvergeActuator) /
//! [`reconcile_cycle`](crate::converge_apply::reconcile_cycle)), which projects
//! the committed ledger into a desired topology and converges it *through the
//! Unit-C safety scaffold* (single-flight + floor, destructive-action budget,
//! circuit breaker). A newly-pending recipient who should be active is a
//! desired-but-absent pane; the cycle spawns it and stamps it with the
//! recipient's ownership identity (`@organization_person_id`, applied at
//! `runtime/exec.rs`). So this waker does **not** touch the runtime, re-derive the launch
//! catalog, or bypass the safety scaffold. It *nudges* the already-scheduled
//! reconcile duty to run promptly, and reports which recipients it requested.
//!
//! This is deliberately not a synchronous bridge into `reconcile_cycle` (which
//! is `async` and owns the writer). `RuntimeWaker` is a synchronous seam;
//! building a blocking async bridge here would duplicate the duty that already
//! converges idempotently. Correctness does not live in a synchronous wake call:
//!
//! * A mailbox envelope is already durable before any wake, so a wake that never
//!   arrives only costs next-cycle latency — never a lost or duplicated message.
//!   [`RuntimeWaker::wake`] is therefore infallible by contract.
//! * A forced projection is already committed in the delivery
//!   writer phase (the dispatch *is* the mutation); the reconcile duty actuates
//!   it and self-heals a failed respawn on its next pass. So
//!   the waker returns `Ok` — it hands the
//!   already-durable projection to the runtime; it does not itself block on a
//!   respawn and cannot honestly fail.
//!
//! # Tiers
//!
//! The nudge is an injected [`ReconcileTrigger`] so the same waker serves two
//! latencies:
//!
//! * **Tier 1 — [`DeferToInterval`]:** the trigger is a no-op; the interval
//!   `SupervisionReconcile` duty converges the recipient within at most one
//!   interval. The correct, dependency-free default before a prompt-trigger seam
//!   is wired into the scheduler.
//! * **Tier 2 — [`NotifyReconcileTrigger`]:** the trigger fires a
//!   [`tokio::sync::Notify`] whose `notified()` the scheduler awaits in its drive
//!   loop, so a wake runs the next reconcile pass near-immediately. The daemon
//!   constructs the `Notify`, wires it into the loop, and hands this waker a
//!   clone.
//!
//! Either tier drops into `MailboxDeliverySink::new(company, Arc::new(waker))`
//! as the sink's `W`; the tier is a construction-time choice, not a type change.

use std::sync::Arc;

use chiefd_core::store::mailbox::RuntimeWaker;
use tokio::sync::Notify;

/// The seam by which a wake asks the daemon's `SupervisionReconcile` duty to run
/// promptly instead of waiting for its next interval.
///
/// Best-effort and non-blocking by contract: a nudge that is dropped only costs
/// next-cycle latency (the durable mailbox is the
/// authority), so an implementation must never block, fail, or panic. It exists
/// so the same [`ReconcileWaker`] serves both the interval-only Tier 1 and the
/// notify-driven Tier 2 without a type change.
pub trait ReconcileTrigger: Send + Sync {
    /// Request that a reconcile pass run promptly. Non-blocking; infallible.
    fn request_reconcile(&self);
}

/// Tier 1: the interval-only trigger. `request_reconcile` is a no-op — the
/// already-scheduled `SupervisionReconcile` duty converges the newly-pending
/// recipient within at most one interval. The correct default before a
/// prompt-trigger seam is wired into the scheduler.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeferToInterval;

impl ReconcileTrigger for DeferToInterval {
    fn request_reconcile(&self) {
        // Nothing to do: the interval reconcile duty owns convergence. The wake
        // is honest as "requested"; latency is bounded by the duty's interval.
    }
}

/// Tier 2: a nudge trigger that fires a [`Notify`] the scheduler's drive loop
/// awaits (`notify.notified().await`) to run the next reconcile pass
/// near-immediately. The daemon owns the `Notify`, wires the wait into its loop,
/// and hands this waker a clone of the same handle.
///
/// [`Notify::notify_one`] coalesces a burst of wakes into a single pending
/// permit, which is exactly the recipient-union coalescing the TS scan performs
/// (`pending-mailbox-wake-coalesced`) and the reconcile engine's own
/// single-flight enforces.
#[derive(Debug, Clone)]
pub struct NotifyReconcileTrigger {
    notify: Arc<Notify>,
}

impl NotifyReconcileTrigger {
    /// Build a trigger over a `Notify` shared with the scheduler's drive loop.
    #[must_use]
    pub fn new(notify: Arc<Notify>) -> Self {
        Self { notify }
    }
}

impl ReconcileTrigger for NotifyReconcileTrigger {
    fn request_reconcile(&self) {
        // Store a single permit; a loop already parked on `notified()` wakes,
        // and a burst collapses to one pending reconcile (natural coalescing).
        self.notify.notify_one();
    }
}

/// The production [`RuntimeWaker`]: a reconcile nudge.
///
/// Holds an injected [`ReconcileTrigger`] (erased so the concrete waker type is
/// tier-independent) and drives it on both wake paths. It performs no runtime I/O
/// and touches no durable state — waking is deferred to the reconcile duty,
/// which is the only component allowed to spawn/respawn panes and is the one
/// that stamps a pane with its recipient's ownership identity.
#[derive(Clone)]
pub struct ReconcileWaker {
    trigger: Arc<dyn ReconcileTrigger>,
}

impl ReconcileWaker {
    /// Build a waker over any reconcile trigger.
    #[must_use]
    pub fn new(trigger: Arc<dyn ReconcileTrigger>) -> Self {
        Self { trigger }
    }

    /// The Tier-1 waker: defer to the interval `SupervisionReconcile` duty.
    #[must_use]
    pub fn deferred() -> Self {
        Self::new(Arc::new(DeferToInterval))
    }

    /// The Tier-2 waker: nudge the scheduler's reconcile loop through `notify`.
    #[must_use]
    pub fn with_notify(notify: Arc<Notify>) -> Self {
        Self::new(Arc::new(NotifyReconcileTrigger::new(notify)))
    }
}

impl RuntimeWaker for ReconcileWaker {
    fn wake(&self, recipients: &[String]) -> Vec<String> {
        // Best-effort: ask the reconcile duty to converge now. Every requested
        // recipient is reported as woken (its pane will be ensured by the pass);
        // a recipient the reconcile cannot reach this pass is simply re-driven by
        // the next one, never lost — the durable envelope is the authority.
        self.trigger.request_reconcile();
        recipients.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
    use chiefd_core::clock::SharedClock;
    use chiefd_core::runtime::delivery_sink::MailboxDeliverySink;
    use chiefd_core::runtime::duty_hooks::{
        ActuationMode, DeliverySink, DutyContext, EffectEnvelope,
    };
    use chiefd_core::store::activity::{self, LaunchFence, ReconcileInput};
    use chiefd_core::store::mailbox::{self, RuntimeWaker};
    use chiefd_core::store::organization::{self, OrganizationManifest};
    use chiefd_core::store::{supervision, COMPANY_DB_FILENAME};
    use chiefd_core::test_support::{northstar_manifest, ManualClock};
    use serde_json::json;
    use tokio::sync::Notify;

    use super::{DeferToInterval, ReconcileTrigger, ReconcileWaker};

    use crate::converge_apply::{reconcile_cycle, safety, ActivityProjectionInput, ActuatorConfig};

    /// A trigger that counts how many nudges it received — the unit under
    /// assertion for the wake/fence paths.
    #[derive(Default)]
    struct RecordingTrigger {
        nudges: AtomicUsize,
    }

    impl RecordingTrigger {
        fn count(&self) -> usize {
            self.nudges.load(Ordering::SeqCst)
        }
    }

    impl ReconcileTrigger for RecordingTrigger {
        fn request_reconcile(&self) {
            self.nudges.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn waker_over(trigger: Arc<dyn ReconcileTrigger>) -> ReconcileWaker {
        ReconcileWaker::new(trigger)
    }

    #[test]
    fn wake_nudges_the_reconcile_duty_and_reports_every_requested_recipient() {
        let trigger = Arc::new(RecordingTrigger::default());
        let waker = waker_over(trigger.clone());

        let recipients = vec!["quant-head".to_string(), "signal-researcher".to_string()];
        let woken = waker.wake(&recipients);

        assert_eq!(woken, recipients, "every requested recipient is reported woken");
        assert_eq!(trigger.count(), 1, "one reconcile nudge for the batch");
    }

    #[test]
    fn wake_addresses_recipients_by_person_identity_verbatim_never_an_index() {
        // The recipients are person-ids (tagged ownership identity), and the
        // waker propagates them verbatim — never a positional pane index. These
        // are exactly the ids the reconcile stamps as `@organization_person_id`
        // pane tags when it converges each pane (runtime/exec.rs spawn tagging).
        let trigger = Arc::new(RecordingTrigger::default());
        let waker = waker_over(trigger.clone());

        let woken = waker.wake(&["it-head".to_string(), "chief".to_string()]);
        assert_eq!(woken, vec!["it-head".to_string(), "chief".to_string()]);
    }

    #[test]
    fn wake_of_no_recipients_reports_none() {
        let trigger = Arc::new(RecordingTrigger::default());
        let waker = waker_over(trigger.clone());
        assert!(waker.wake(&[]).is_empty());
    }

    #[test]
    fn defer_to_interval_is_a_safe_no_op_trigger() {
        let waker = ReconcileWaker::deferred();
        // With no prompt trigger wired, wake still reports its recipients —
        // latency is bounded by the reconcile interval.
        assert_eq!(waker.wake(&["bob".to_string()]), vec!["bob".to_string()]);
        // Direct: the trigger itself is inert.
        DeferToInterval.request_reconcile();
    }

    #[tokio::test]
    async fn notify_reconcile_trigger_delivers_the_nudge_to_a_waiting_loop() {
        // Tier 2: a wake must actually reach a reconcile loop parked on the
        // shared Notify. The stored permit makes a subsequent `notified()`
        // resolve at once; a bounded timeout fails loudly if the nudge was lost.
        let notify = Arc::new(Notify::new());
        let waker = ReconcileWaker::with_notify(Arc::clone(&notify));

        let woken = waker.wake(&["signal-researcher".to_string()]);
        assert_eq!(woken, vec!["signal-researcher".to_string()]);

        tokio::time::timeout(Duration::from_secs(1), notify.notified())
            .await
            .expect("the wake nudge is delivered to a waiting reconcile loop");
    }

    // --- through the real MailboxDeliverySink: no mail lost or duplicated -----

    fn open_db(dir: &std::path::Path, slug: &str) -> Arc<CompanyDb> {
        let clock: SharedClock = Arc::new(ManualClock::default());
        Arc::new(CompanyDb::open(slug, &dir.join(COMPANY_DB_FILENAME), clock).expect("open"))
    }

    fn ctx(db: &CompanyDb) -> DutyContext {
        DutyContext { slug: "cobalt".to_string(), snapshot: db.snapshot() }
    }

    fn reminder(id: &str, recipient: &str) -> EffectEnvelope {
        EffectEnvelope {
            id: id.to_string(),
            kind: "person_reminder".to_string(),
            // `personId` is where `evaluate_reminders` addresses its recipient,
            // and `message` is where it puts the prose. An envelope carrying no
            // content is a per-effect render failure rather than a `[kind]`
            // placeholder (#76), so a fixture without it is not a shape any real
            // producer emits.
            payload: json!({
                "personId": recipient,
                "message": "[reminder]\n\nRebalance the book",
            }),
        }
    }

    #[tokio::test]
    async fn the_real_sink_with_this_waker_stages_durably_and_fires_one_wake() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = open_db(dir.path(), "cobalt");
        let trigger = Arc::new(RecordingTrigger::default());
        let waker = Arc::new(waker_over(trigger.clone()));
        let sink = MailboxDeliverySink::new(Arc::clone(&db), waker);

        let outcome = sink.deliver(&ctx(&db), vec![reminder("del-1", "bob")]).await;
        assert_eq!(outcome.delivered, vec!["del-1".to_string()]);
        assert!(outcome.failed.is_empty());

        let pending = db.read(|snapshot| mailbox::pending_for(snapshot, "bob").len());
        assert_eq!(pending, 1, "the envelope is durably staged");
        assert_eq!(trigger.count(), 1, "exactly one wake nudge fired after staging");
    }

    #[tokio::test]
    async fn a_failed_wake_through_the_real_sink_neither_loses_nor_duplicates_mail() {
        // The waker does no mailbox I/O, so even a nudge that reaches nobody (an
        // inert Tier-1 trigger stands in for "the reconcile could not run this
        // pass") cannot lose or duplicate the durable envelope: a crash-retry of
        // the same effect stays exactly one durable row and delivers each time.
        let dir = tempfile::tempdir().expect("tempdir");
        let clock = Arc::new(ManualClock::default());
        let db = Arc::new(
            CompanyDb::open("cobalt", &dir.path().join(COMPANY_DB_FILENAME), clock.clone())
                .expect("open"),
        );
        // Tier-1 inert trigger: the wake "fails" to promptly wake anyone.
        let sink = MailboxDeliverySink::new(Arc::clone(&db), Arc::new(ReconcileWaker::deferred()));

        let first = sink.deliver(&ctx(&db), vec![reminder("del-1", "bob")]).await;
        assert_eq!(first.delivered, vec!["del-1".to_string()]);
        clock.advance(Duration::from_secs(60)); // a later pass re-presents the effect
        let second = sink.deliver(&ctx(&db), vec![reminder("del-1", "bob")]).await;
        assert_eq!(second.delivered, vec!["del-1".to_string()], "re-presented ⇒ delivered again");

        let pending = db.read(|snapshot| mailbox::pending_for(snapshot, "bob").len());
        assert_eq!(pending, 1, "one durable row across the failed-wake crash-retry");
    }

    // --- the nudge defers to a real, runtime-backed reconcile of the recipient ----

    const EPOCH: i64 = 1_700_000_000_000;

    async fn seed_and_activate(db: &CompanyDb, manifest: &OrganizationManifest, person: &str) {
        let seed = manifest.clone();
        db.mutate(MutationClass::Normal, MutationName("test.seed"), move |ledgers| {
            organization::create(ledgers, &seed)?;
            supervision::seed(ledgers, &seed)?;
            activity::seed(ledgers, &seed)?;
            Ok(())
        })
        .await
        .expect("seed");

        let act = manifest.clone();
        let person = person.to_owned();
        db.mutate(MutationClass::Reconcile, MutationName("test.activate"), move |ledgers| {
            let supervision = supervision::read(ledgers, &act)?;
            activity::reconcile(
                ledgers,
                &act,
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

    // TOMBSTONE: `attach_idle_actuator`. It committed a TRUSTED, EMPTY
    // observation so the pass below had an attached actuator to plan against --
    // without one, `plan_runtime_actions` withheld every action and the
    // assertion passed while testing nothing. There is no observation to commit
    // and no presence to withhold on: the desired set is derived from the
    // manifest and the activity ledger, so a pass has a subject unconditionally
    // and the vacuity this helper existed to prevent is unreachable.

    fn actuator_config(dir: &std::path::Path, session: &str) -> ActuatorConfig {
        ActuatorConfig {
            socket: format!("{session}-sock"),
            // "watching for ever": the epoch, so an inferred quiet instant is
            // clamped by nothing and every expectation here is the pre-clamp one.
            watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
            dir: dir.to_path_buf(),
            home: dir.join("home"),
            pi_binary: std::path::PathBuf::from("/opt/pi/bin/pi"),
            floor: Duration::from_millis(0),
            launcher_root: std::path::PathBuf::from("/launcher"),
            root_pi_agent_dir: dir.join("pi-agent"),
        }
    }

    #[tokio::test]
    async fn the_nudge_defers_to_a_runtime_backed_reconcile_that_plans_the_recipient() {
        // Prove the wake reaches a reconcile loop AND that the pass it drives
        // targets the recipient. The pass reads no report and looks at nothing:
        // its desired set comes from the manifest and the activity ledger, so
        // the recipient appearing in it is evidence about the wake rather than
        // about whether somebody had vouched for the host first.
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = northstar_manifest(EPOCH);
        let db = open_db(dir.path(), &manifest.slug);
        seed_and_activate(&db, &manifest, "signal-researcher").await;
        // A company's stored actuation mode defaults to SHADOW, and shadow now
        // withholds the whole action stream (`WithheldReason::Shadow`,
        // `desired_people == 0`) rather than computing a plan it declines to
        // apply. This test's subject is the nudge -> reconcile -> planned start
        // path, which only has a plan to assert in Apply — so the mode is set
        // deliberately, not bolted on to preserve a number. The shadow
        // behaviour has its own test, which asserts zero steps and the withheld
        // note.
        safety::set_actuation_config(&db, safety::ActuationMode::Apply, false, false)
            .await
            .expect("set actuation config");

        let notify = Arc::new(Notify::new());
        let waker = ReconcileWaker::with_notify(Arc::clone(&notify));

        let woken = waker.wake(&["signal-researcher".to_string()]);
        assert_eq!(woken, vec!["signal-researcher".to_string()]);
        // The nudge is delivered to a parked reconcile loop.
        tokio::time::timeout(Duration::from_secs(1), notify.notified())
            .await
            .expect("nudge delivered to the reconcile loop");

        // The reconcile that loop runs reads the committed observation and
        // plans the active recipient's start.
        let report = reconcile_cycle(
            &db,
            &actuator_config(dir.path(), &manifest.slug),
            ActuationMode::Apply,
            // The fence must admit the woken recipient or the cycle's own
            // fence projection would park them right back.
            Some(ActivityProjectionInput {
                fence: LaunchFence::fenced(["signal-researcher".to_owned()]),
                pending_mail_facts: Vec::new(),
                maintenance_person_ids: Vec::new(),
            }),
        )
        .await
        .expect("reconcile cycle");

        // RE-RULED, because the assertion this replaces had become vacuous.
        //
        // It read `desired_people > 0` and claimed that count was "proof that
        // this one read a real, trusted, unlapsed report". That was true while
        // a pass with no committed observation withheld every action. There is
        // no observation to withhold on: the desired set is derived from the
        // manifest and the activity ledger, so ANY live company answers `> 0`
        // and the assertion could no longer fail — it stopped measuring the
        // wake and started measuring that the company exists.
        //
        // The subject is the WAKE, so the recipient is named. A pass that woke
        // for the wrong person, or that reached a reconcile which desired
        // somebody else, now fails here.
        assert!(
            report.notes.iter().any(|note| note.contains("signal-researcher")),
            "the nudge must reach a reconcile that desires the RECIPIENT: {:?}",
            report.notes
        );
    }
}
