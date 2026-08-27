//! `chiefd run <company>` — the real per-company daemon loop (one-daemon
//! migration). The counterpart to `observe`: where `observe` predicts and logs,
//! `run` actually drives every supervisor duty through to mutation.
//!
//! # What this is
//!
//! For one company it: (1) opens the company writer actor
//! ([`CompanyDb`](chiefd_core::actor::CompanyDb)), (2) mounts the typed HTTP
//! surface and spawns every duty task, each HELD at a readiness latch until the
//! company's organization manifest exists ([`crate::manifest_ready`] — genesis
//! starts this daemon and then writes the manifest THROUGH it, so a brand-new
//! company has none when its daemon boots), (3) runs the startup self-audit once,
//! behind that same latch, to raise any missed-window backlog from downtime,
//! then (4) drives each [`Duty`] at its own `interval_ms` cadence until a
//! shutdown signal. Each duty pass is two phases with a hard boundary:
//!
//! * **host phase** — the injected hook ([`duty_hooks`]) observes runtime /
//!   sends an envelope / spawns a worker. Off the writer thread; a
//!   failure is one skipped pass, logged, never a crash.
//! * **commit phase** — the pure core (`supervision::cycle`, `evaluate_due_work`,
//!   `health_collect::collect`, …) runs inside ONE `CompanyDb::mutate`, and
//!   `supervisor_watermark::record_success` is folded into that SAME closure, so
//!   a duty's liveness watermark advances in the very commit that carries its
//!   work — never as a second, untracked write.
//!
//! # Scheduling: one task per duty, not one central select
//!
//! Each `(company, duty)` gets its own `tokio` task running [`supervise`],
//! which in turn runs [`drive`] — a single generic timer loop reused for all
//! six duties (the registration table [`Daemon::duty_table`] maps `Duty` →
//! its pass) — on a nested task it watches. Justification:
//!
//! * A slow or hung host gatherer is contained to its own duty — a stuck health
//!   observation cannot delay deadline evaluation.
//! * Each task is a trivial `loop { select! { shutdown, clock.sleep, trigger } }`.
//!   There is no shared scheduling state, no central dispatcher, nothing clever.
//!   `trigger` is an optional per-duty accelerator (`SupervisionReconcile` only,
//!   today): a `tokio::sync::Notify` shared with the real
//!   `MailboxDeliverySink`'s waker (`chiefd_host::runtime_waker::ReconcileWaker`),
//!   so a mailbox wake runs that duty's very next pass
//!   immediately instead of waiting out its interval — this is the actual "wake
//!   the runtime pane now" seam, not just durable staging plus a ≤30 s wait.
//! * `supervise` is the resilience layer: `drive`'s task dying for ANY reason
//!   other than an observed shutdown (panic, cancellation) is logged loudly
//!   with the duty name and payload, then the duty is respawned immediately —
//!   a duty can no longer silently vanish for the rest of the process's life
//!   (#340).
//!
//! All waiting is through the injected [`Clock`](chiefd_core::clock::Clock)
//! (`clock.sleep`), never `tokio::time::sleep` (clippy-banned), so the whole
//! schedule is driven deterministically by a `ManualClock` in tests. The
//! trigger `Notify` is not a timed wait, so it is not part of that ban.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chiefd_api::docstore;
use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::clock::{SharedClock, SystemClock};
use chiefd_core::runtime::attendance::ActuatorAttendance;
use chiefd_core::runtime::delivery_sink::MailboxDeliverySink;
#[cfg(test)]
use chiefd_core::runtime::duty_hooks::DutyError;
use chiefd_core::runtime::duty_hooks::{
    ActuationMode, BoxFuture, CycleInputGatherer, DeliveryOutcome, DeliverySink, DutyContext,
    EffectEnvelope, HealthSnapshotGatherer, ReconcileActuator,
};
use chiefd_core::store::supervisor_watermark::{self, Duty};
use chiefd_core::store::{health, health_collect, organization, supervision};
use chiefd_core::ChiefdError;
use chiefd_host::converge_apply::{
    root_pi_agent_dir, safety, ActuatorConfig, ApiHostLaunchProfileConfig,
    ApiHostLaunchProfileSource, ConvergeActuator,
};
use chiefd_host::real::RealHostExecutor;
use chiefd_host::runtime_waker::ReconcileWaker;
use chiefd_host::HostExecutor;
use tokio::sync::{watch, Notify};
use tokio::task::JoinSet;

/// Every host-side hook the loop injects, one per duty that needs one.
///
/// Held as trait objects so the concrete implementations (built in parallel by
/// `od-delivery-mailbox`, `od-host-gatherers`, and M2 for
/// the actuator) drop in with no change to the loop.
#[derive(Clone)]
pub struct Hooks {
    /// SupervisionReconcile host observation (`od-host-gatherers`).
    pub cycle_input: Arc<dyn CycleInputGatherer>,
    /// SupervisionReconcile runtime actuation half (M2 `reconcile_cycle`).
    pub actuator: Arc<dyn ReconcileActuator>,
    /// HealthMonitor host observation (`od-host-gatherers`).
    pub health: Arc<dyn HealthSnapshotGatherer>,
    /// MailboxWake effect delivery (`od-delivery-mailbox`).
    pub delivery: Arc<dyn DeliverySink>,
}

/// One company's running daemon: its writer actor, its clock, its injected
/// hooks, and its actuation posture.
pub struct Daemon {
    /// This company's identity, `sha256(<dir>)[..12]`.
    ///
    /// NOT the slug. The slug is a column of the `organization` row and is a
    /// display name; a daemon that cached one at boot would be holding a value
    /// the genesis it is about to serve can change. The key is derived from
    /// the one input this process was given and cannot go stale.
    company_key: String,
    company: Arc<CompanyDb>,
    clock: SharedClock,
    hooks: Hooks,
    actuation_mode: ActuationMode,
    /// The Tier-2 accelerator for the `SupervisionReconcile` duty's drive loop
    /// (`runtime-waker`'s `NotifyReconcileTrigger` seam): when set, a mailbox
    /// wake nudge (`ReconcileWaker::with_notify`, same
    /// `Arc`) wakes this duty's `drive` loop immediately instead of waiting out
    /// its interval. `None` is the safe default (Tier-1: interval-only,
    /// matching every other duty) — set via [`Daemon::with_reconcile_trigger`].
    reconcile_trigger: Option<Arc<Notify>>,
    /// True only while one floor-delayed replay of a coalesced reactive
    /// reconcile request is armed. It prevents a burst inside the five-second
    /// safety floor from turning into a timer storm after the first legal pass.
    reconcile_floor_retry_armed: Arc<AtomicBool>,
    /// Post-commit bridge shared with the live bench endpoint. The daemon is
    /// the only component allowed to resolve it, after a fresh post-actuation
    /// gather proves the requested tagged pane absent.
    bench_completion: Option<Arc<docstore::BenchCompletionRegistry>>,
    /// Production-only fence for a changed runtime-owner socket.  Startup owns
    /// socket adoption; a daemon must never silently switch runtime servers after
    /// it has begun reconciling.  When the normalized runtime-owner reader is
    /// wired, seeing a foreign owner after a successful boot is therefore a
    /// fatal handoff, not an inert steady state (#469).
    foreign_identity_fatal_shutdown: bool,
    /// First fatal handoff reason, observed by [`Daemon::serve`] so it can
    /// drain the daemon and return a non-zero process status to its supervisor.
    fatal_shutdown: watch::Sender<Option<String>>,
    /// Whether anybody is converging this company — see
    /// [`ActuatorAttendance`]. Shared with the desired-set route that stamps
    /// it and with the health gatherer that raises `runtime_unattended` from
    /// it; `chiefd run` takes all three off one cell.
    ///
    /// A daemon assembled without the HTTP surface (`--once`, and every unit
    /// test) gets its own cell seeded at construction, so it reads attended
    /// for one lapse window and unattended after. That is the truth for such a
    /// daemon: nobody is coming for its desired set.
    attendance: ActuatorAttendance,
}

/// A duty's single-pass runner: capture-free at the call site, `'static` so it
/// can be moved onto its own task.
type DutyPass = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// A duty's optional per-iteration dynamic sleep override (od:idle-cpu #280):
/// called FRESH before every `drive` sleep instead of the fixed period, so a
/// duty can sleep exactly until its next real deadline. `None` keeps the fixed
/// period. Used by the alarm-clock duties (sleep-until-next-work).
type NextInterval = Arc<dyn Fn() -> Duration + Send + Sync>;

impl Daemon {
    /// Assemble a daemon for one already-open company writer.
    #[must_use]
    pub fn new(
        company_key: impl Into<String>,
        company: Arc<CompanyDb>,
        clock: SharedClock,
        hooks: Hooks,
        actuation_mode: ActuationMode,
    ) -> Self {
        // E8-S2 (#824): every duty must be reactive-primary or carry a
        // written justification, enforced at construction time (not just by
        // the build-time test `duty_cadence_conformance`, `run/tests.rs`,
        // which calls this same check) — a future duty added on a bare
        // fixed timer with neither fails loudly the moment a daemon boots.
        let violations = Self::duty_cadence_conformance_violations();
        assert!(violations.is_empty(), "duty cadence conformance violated: {violations:?}");
        let (fatal_shutdown, _fatal_shutdown_rx) = watch::channel(None);
        let attendance = ActuatorAttendance::new(clock.wall().0);
        Self {
            company_key: company_key.into(),
            company,
            clock,
            hooks,
            actuation_mode,
            reconcile_trigger: None,
            reconcile_floor_retry_armed: Arc::new(AtomicBool::new(false)),
            bench_completion: None,
            foreign_identity_fatal_shutdown: false,
            fatal_shutdown,
            attendance,
        }
    }

    /// Share the attendance cell the HTTP surface stamps.
    ///
    /// `chiefd run` calls this with the cell it took off its own
    /// `SupervisionLiveSource`, so the route that records a desired-set read
    /// and the duty that judges the silence are looking at one value.
    #[must_use]
    pub fn with_actuator_attendance(mut self, attendance: ActuatorAttendance) -> Self {
        self.attendance = attendance;
        self
    }

    /// Require a process restart rather than silently going inert if the
    /// runtime-owner row changes to another runtime socket after this daemon has
    /// started.  `run_company` enables this only when that row is actually
    /// observable; tests and migration mode retain their deliberate inert
    /// defaults.
    #[must_use]
    pub fn with_foreign_identity_fatal_shutdown(mut self) -> Self {
        self.foreign_identity_fatal_shutdown = true;
        self
    }

    fn request_foreign_identity_shutdown(&self, holder: &str) {
        let reason = format!(
            "runtime-owner runtime socket drift: chiefd started on its resolved socket, but the active owner now names '{holder}'; refusing mid-run adoption to avoid a shadow fleet"
        );
        let observed = self.fatal_shutdown.subscribe();
        if observed.borrow().is_none() {
            self.fatal_shutdown.send_replace(Some(reason.clone()));
            tracing::error!(company = %self.company_key, holder, "chiefd run: {reason}; draining and exiting so the supervisor can restart against the current owner socket");
        }
    }

    #[cfg(test)]
    fn fatal_shutdown_reason(&self) -> Option<String> {
        self.fatal_shutdown.subscribe().borrow().clone()
    }

    /// Wire the Tier-2 nudge accelerator onto the `SupervisionReconcile` duty:
    /// the SAME `Arc<Notify>` handed to `production_hooks`' `ReconcileWaker`, so
    /// a mailbox wake — staged durably by the real
    /// `MailboxDeliverySink` — wakes this duty's `drive` loop immediately rather
    /// than waiting out its interval. A builder rather than a `new` parameter so
    /// every existing test call site (which wants the safe interval-only
    /// default) is unaffected.
    #[must_use]
    pub fn with_reconcile_trigger(mut self, trigger: Arc<Notify>) -> Self {
        self.reconcile_trigger = Some(trigger);
        self
    }

    /// Share the live endpoint's exact bench-completion registry with the sole
    /// reconcile loop.
    #[must_use]
    pub fn with_bench_completion(
        mut self,
        completion: Arc<docstore::BenchCompletionRegistry>,
    ) -> Self {
        self.bench_completion = Some(completion);
        self
    }

    /// `HealthMonitor`'s dynamic per-iteration sleep (E8-S2, #824): sleep
    /// exactly until the earliest armed staleness deadline this company has
    /// — today that is
    /// [`health::next_confirmation_deadline`]'s confirmation window
    /// (plan §2.7, `HEALTH_OBSERVATION_CONFIRMATION_MS` — a provisional
    /// observation genuinely needs a second, later sample before it may page)
    /// — capped at the reactive fallback floor, with the SAME #437
    /// overdue-backoff guard the reminder duty's own interval
    /// carries: a future deadline sleeps exactly until it; an overdue one
    /// sleeps [`DEADLINE_EVALUATION_MIN_INTERVAL`] and is evaluated promptly;
    /// the SAME overdue deadline recurring backs off geometrically to the
    /// floor rather than spinning; and the backoff is capped at the next
    /// strictly-future deadline so a stuck observation can never sleep
    /// through a different, newer one arming. A healthy quiet company (no
    /// observation pending confirmation) rests at the floor and commits
    /// nothing — `HealthMonitor::interval_ms()` (5 min,
    /// `supervisor_watermark.rs`) is now the LIVENESS EXPECTATION the
    /// startup self-audit measures silence against, exactly like
    /// `ReminderDispatch`'s, not a wake rate.
    fn health_monitor_next_interval(self: &Arc<Self>) -> NextInterval {
        let daemon = Arc::clone(self);
        let backoff: Mutex<Option<(i64, Duration)>> = Mutex::new(None);
        Arc::new(move || {
            let floor = reactive_fallback_floor();
            // Live clock, not `ledgers.now()`: a read-only snapshot's `now` is
            // stamped at the last commit and does not advance between commits.
            let now = daemon.clock.wall().0;
            let (due, next_future) = daemon
                .company
                .read(|snapshot| {
                    let ledgers = snapshot.ledgers();
                    let manifest = organization::read(ledgers).ok()?;
                    let ctx = organization::company_context(&manifest).ok()?;
                    let (state, _warning) = health::read(ledgers, &ctx).into_parts();
                    Some((
                        health::next_confirmation_deadline(&state),
                        health::next_confirmation_deadline_after(&state, now),
                    ))
                })
                .unwrap_or((None, None));
            let mut state = backoff.lock().unwrap_or_else(PoisonError::into_inner);
            match due {
                None => {
                    *state = None;
                    floor
                }
                Some(due) if due > now => {
                    *state = None;
                    Duration::from_millis(u64::try_from(due - now).unwrap_or(u64::MAX)).min(floor)
                }
                Some(due) => {
                    let backed_off = match *state {
                        Some((previous, last)) if previous == due => {
                            last.saturating_mul(2).min(floor)
                        }
                        _ => DEADLINE_EVALUATION_MIN_INTERVAL,
                    };
                    *state = Some((due, backed_off));
                    match next_future {
                        Some(future) => backed_off.min(
                            Duration::from_millis(u64::try_from(future - now).unwrap_or(u64::MAX))
                                .max(DEADLINE_EVALUATION_MIN_INTERVAL),
                        ),
                        None => backed_off,
                    }
                }
            }
        })
    }

    /// The read-only context handed to a host hook this pass: the company's
    /// scope key plus the last committed snapshot.
    ///
    /// `DutyContext::slug` is chiefd-core's name for the company SCOPE — the
    /// value a mailbox row is written under and every `/v1/docs/*` caller
    /// filters by — and that scope is the directory-derived key now, not a
    /// name. One key, one scope; the pair `bare_slug`/composite existed only
    /// because the two used to be different strings.
    fn context(&self) -> DutyContext {
        DutyContext { slug: self.company_key.clone(), snapshot: self.company.snapshot() }
    }

    // --- the startup self-audit --------------------------------------------

    /// Run the retroactive missed-window backlog catch-up exactly once, before
    /// any interval task starts. Folds stalled-duty incidents into the health
    /// store in one commit; a failure is logged, never fatal — a daemon that
    /// refused to start because its own audit hiccuped would be worse than one
    /// that starts and reports nothing.
    pub async fn run_startup_self_audit(&self) {
        let result = self
            .company
            .mutate(
                MutationClass::Normal,
                MutationName("duty.startup_self_audit"),
                move |ledgers| {
                    let manifest = organization::read(ledgers)?;
                    let ctx = organization::company_context(&manifest)?;
                    let now = ledgers.now().0;
                    let outcome = supervisor_watermark::run_startup_self_audit(ledgers, &ctx, now);
                    Ok(outcome.new_incidents.len())
                },
            )
            .await;
        match result {
            Ok(raised) => {
                tracing::info!(
                    company = %self.company_key,
                    raised,
                    "startup self-audit complete (retroactive missed-window backlog folded into health)"
                )
            }
            Err(error) => tracing::warn!(
                company = %self.company_key,
                %error,
                "startup self-audit could not run; continuing to the interval loop"
            ),
        }
    }

    // --- the six duty bodies ------------------------------------------------

    /// Duty #1 — SupervisionReconcile: gather the host observation, commit the
    /// D9 ledger cycle + its watermark in one transaction, then actuate the runtime.
    ///
    /// The two halves are one duty. `supervision::cycle` (ledger) commits first,
    /// then `ReconcileActuator::reconcile` (the runtime) converges toward that
    /// just-committed desired state — never before it, or it would actuate stale
    /// intent. The watermark advances with the *ledger* half: the duty's
    /// essential work is the cycle; actuation is best-effort convergence whose
    /// failures surface through health, not the liveness watermark.
    /// #825-prereq: record a genuine `SupervisionReconcile` LEDGER failure
    /// into the chiefd-owned bounded watermark
    /// ([`supervisor_watermark::record_failure`]), so the health read path
    /// (`read_supervisor_liveness`) can see it. The TypeScript
    /// `supervisor-state` writer it replaced is deleted.
    ///
    /// A dedicated follow-up mutation, not folded into the failed attempt's
    /// own transaction: the whole point is that the transaction that would
    /// have carried the success ROLLED BACK (or, in the self-heal branch, was
    /// never attempted), so there is nothing to fold this into — same reason
    /// `record_success` rides inside `duty.supervision_reconcile`'s own
    /// commit while this rides in its own. Only the ledger half of the duty
    /// is tracked here — actuation (the runtime) failures surface through `health`
    /// directly, matching `record_success`'s own scope (see its doc comment
    /// above `run_supervision_reconcile`).
    async fn record_supervision_reconcile_failure(&self, kind: &'static str, detail: String) {
        let recorded = self
            .company
            .mutate(
                MutationClass::Normal,
                MutationName("duty.supervision_reconcile.record_failure"),
                move |ledgers| {
                    let manifest = organization::read(ledgers)?;
                    let ctx = organization::company_context(&manifest)?;
                    let now = ledgers.now().0;
                    supervisor_watermark::record_failure(
                        ledgers,
                        &ctx,
                        Duty::SupervisionReconcile,
                        now,
                        kind,
                        &detail,
                    );
                    Ok(())
                },
            )
            .await;
        if let Err(error) = recorded {
            tracing::warn!(
                company = %self.company_key,
                %error,
                "could not record supervision-reconcile failure watermark; liveness observability degraded"
            );
        }
    }

    /// Recover exact maintenance claims whose owning Pi process is gone.
    ///
    /// This check belongs to the external supervision duty, not to Pi startup:
    /// a dead process cannot run its own recovery hook. The actor checks the
    /// request id and pid again in the mutation, so pid observations that race
    /// a newer claim become no-ops.
    async fn recover_dead_maintenance_claims(&self) {
        let ledger = match self.company.session_maintenance_read().await {
            Ok(Some((ledger, _seq))) => ledger,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(company = %self.company_key, %error, "session-maintenance dead-claim read failed");
                return;
            }
        };
        let dead_claims: Vec<(String, i64)> = ledger
            .ordered_requests()
            .filter(|request| {
                request.status
                    == chiefd_core::store::session_maintenance::MaintenanceStatus::Running
            })
            .filter_map(|request| {
                request
                    .claimed_process_id
                    .filter(|pid| !beacond::liveness::pid_is_live(*pid))
                    .map(|pid| (request.id.clone(), pid))
            })
            .collect();
        if dead_claims.is_empty() {
            return;
        }
        match self.company.session_maintenance_recover_dead_claims(dead_claims).await {
            Ok(report) => tracing::warn!(
                company = %self.company_key,
                interrupted = report.interrupted.len(),
                replacements = report.replacements.len(),
                "supervision recovered dead session-maintenance claims"
            ),
            Err(error) => tracing::warn!(
                company = %self.company_key,
                %error,
                "session-maintenance dead-claim recovery refused"
            ),
        }
    }

    pub async fn run_supervision_reconcile(&self) {
        self.recover_dead_maintenance_claims().await;
        let ctx = self.context();
        let input = match self.hooks.cycle_input.gather_cycle_input(&ctx).await {
            Ok(input) => input,
            Err(error) => {
                // Self-heal a reconcilable-drift wedge. The gather reads the
                // supervision ledger through the STRICT validating reader
                // (`supervision::read`), which refuses a ledger that still names
                // a person the manifest no longer staffs — a departed manager, or
                // the goals/delegated-goals/assignments of a whole offboarded
                // department. That refusal surfaces as `corrupt store:
                // supervision` and skips the pass, but the very repair
                // (`reconcile_protected` + `shed_departed_supervision`, both run
                // inside `supervision::mutate`) is gated behind the read that
                // fails — so the pass skipped FOREVER (found live on
                // tribes-capital: the reconcile stopped, panes never parked).
                //
                // A no-op supervision mutation runs the reconcile-and-shed over
                // the read-for-mutation path (which does NOT validate first), then
                // its commit re-validates the now-consistent ledger. When the
                // ledger is already clean this publishes nothing (idle stays at
                // zero cost). We attempt it once and retry the gather; a still-
                // failing gather skips exactly as before.
                tracing::warn!(company = %self.company_key, %error, "cycle-input gather failed; attempting supervision self-heal");
                let healed = self
                    .company
                    .mutate(
                        MutationClass::Reconcile,
                        MutationName("duty.supervision_reconcile.self_heal"),
                        move |ledgers| {
                            let manifest = organization::read(ledgers)?;
                            supervision::mutate(ledgers, &manifest, |_draft, _at| Ok(()))
                        },
                    )
                    .await;
                if let Err(error) = healed {
                    tracing::warn!(company = %self.company_key, %error, "supervision self-heal refused; skipping supervision pass");
                    self.record_supervision_reconcile_failure(
                        "self_heal_refused",
                        error.to_string(),
                    )
                    .await;
                    return;
                }
                match self.hooks.cycle_input.gather_cycle_input(&ctx).await {
                    Ok(input) => input,
                    Err(error) => {
                        tracing::warn!(company = %self.company_key, %error, "cycle-input gather still failing after self-heal; skipping supervision pass");
                        self.record_supervision_reconcile_failure(
                            "cycle_input_gather_failed",
                            error.to_string(),
                        )
                        .await;
                        return;
                    }
                }
            }
        };
        if self.foreign_identity_fatal_shutdown {
            if let supervision::IdentityObservation::Foreign { holder } = &input.identity {
                self.request_foreign_identity_shutdown(holder);
                return;
            }
        }
        let committed = self
            .company
            .mutate(
                MutationClass::Reconcile,
                MutationName("duty.supervision_reconcile"),
                move |ledgers| {
                    let manifest = organization::read(ledgers)?;
                    let ctx = organization::company_context(&manifest)?;
                    let now = ledgers.now().0;
                    let report = supervision::cycle(ledgers, &manifest, &input)?;
                    supervisor_watermark::record_success(
                        ledgers,
                        &ctx,
                        Duty::SupervisionReconcile,
                        now,
                    );
                    Ok(report)
                },
            )
            .await;
        match committed {
            // od:idle-cpu #437: an INERT cycle (suppressed, or the company's
            // ownership claim names another chiefd) commits successfully and
            // writes nothing — nothing converges. It used to log the identical
            // INFO line as a healthy pass, so 4911 consecutive no-op passes
            // went unnoticed (#63/#64). It is now a WARN that names the gate
            // that is shut.
            Ok(report) if report.is_inert() => tracing::warn!(
                company = %self.company_key,
                stages = report.stages.len(),
                reason = %report.warnings.join("; "),
                "supervision cycle went INERT: it wrote nothing and converged nothing"
            ),
            // A COMMITTED CYCLE IS NOT A HEALTHY ONE WHEN NOBODY IS LISTENING.
            //
            // This arm logged one unconditional INFO, and on 2026-08-18 it
            // logged it every five seconds for forty minutes against a tmux
            // server that had ceased to exist — eleven panes and five people
            // gone at 22:17:40Z, and not one line in `daemon.log` or
            // `chiefd.jsonl` saying so. The cycle really had committed: the
            // ledger work is correct with the display gone, and that is exactly
            // why the line was so misleading. It reported the half chiefd can
            // see and said nothing about the half it cannot.
            //
            // So the pass now names its own reach. Attended: the desired set is
            // going to somebody, INFO as before. Unattended: WARN, with how
            // long the silence has run, because a desired set published to
            // nobody converges nothing however cleanly it commits.
            Ok(report) => {
                let silent_ms = self.attendance.silent_ms(self.clock.wall().0);
                if silent_ms > chiefd_core::runtime::attendance::ACTUATOR_LAPSE_MS {
                    tracing::warn!(
                        company = %self.company_key,
                        stages = report.stages.len(),
                        warnings = %report.warnings.join("; "),
                        actuator_silent_ms = silent_ms,
                        "supervision cycle committed but NOBODY IS CONVERGING THIS COMPANY: no \
                         actuator has read the desired set, so whatever chiefd wants running is \
                         not being made to run and chiefd cannot see what is"
                    );
                } else {
                    tracing::info!(
                        company = %self.company_key,
                        stages = report.stages.len(),
                        warnings = %report.warnings.join("; "),
                        "supervision cycle committed"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(company = %self.company_key, %error, "supervision cycle refused; watermark not advanced, skipping actuation");
                self.record_supervision_reconcile_failure("reconcile_refused", error.to_string())
                    .await;
                return;
            }
        }
        // Actuate against the fresh post-cycle snapshot.
        let ctx = self.context();
        match self.hooks.actuator.reconcile(&ctx, self.actuation_mode).await {
            Ok(report) => {
                // ONE ARM, at the level the rule chooses. It was two arms with
                // two different message strings and two different field sets,
                // which is how `notes` — the field carrying the wake grant and
                // the settle withdrawal — came to exist on only one of them.
                //
                // `desired`, and it is the same word as the field. It printed
                // `planned=` -- the field's name BEFORE the rename that made it
                // a desired count -- beside a permanently-zero `actuated=`, so
                // the line read `planned=8 actuated=0` on passes where eight
                // people came up. A rename that stops at the struct leaves the
                // log telling the old story.
                let notes = report.notes.join("; ");
                if actuation_pass_log_level(report.changed, report.actuation_record)
                    == tracing::Level::INFO
                {
                    tracing::info!(
                        company = %self.company_key,
                        applied = report.applied,
                        desired = report.desired_people,
                        notes = %notes,
                        "reconcile actuation pass"
                    );
                } else {
                    tracing::debug!(
                        company = %self.company_key,
                        applied = report.applied,
                        desired = report.desired_people,
                        notes = %notes,
                        "reconcile actuation pass"
                    );
                }
                // THE REPLAY BELONGS HERE TOO, and its absence was the whole of
                // the minute an operator's click used to take.
                //
                // MEASURED on a live company, two wakes sixteen seconds apart:
                //
                //   22:16:16.528  wake maya   -> launching: ceo, maya at .650
                //   22:16:32.753  wake rhea   -> nothing, for over a minute
                //
                // 122 milliseconds against a fallback interval, and the only
                // difference is WHEN they landed. Maya's wake arrived twelve
                // seconds after the previous pass — outside `RECONCILE_FLOOR` —
                // so it ran at once. Rhea's arrived 0.9 seconds after one, INSIDE
                // the floor, and was deferred to a replay that the no-op arm
                // never scheduled: the call lived only in the `changed` branch.
                //
                // A pass that changed nothing is precisely the state a quiet
                // company sits in, so it is the state almost every wake lands
                // next to. Arming the replay only after a pass that DID
                // something meant the retry existed for every case except the
                // common one. It is unconditional now because there is one arm;
                // `report.retry_after_floor` is still the whole guard, so a pass
                // with nothing pending schedules nothing, and the atomic still
                // collapses a burst into one timer.
                schedule_reconcile_floor_retry(
                    self.reconcile_trigger.clone(),
                    Arc::clone(&self.reconcile_floor_retry_armed),
                    report.retry_after_floor,
                    RECONCILE_FLOOR,
                    Arc::clone(&self.clock),
                );
            }
            Err(error) => tracing::warn!(
                company = %self.company_key,
                %error,
                "reconcile actuation failed (ledger cycle already committed)"
            ),
        }
        self.observe_bench_completions().await;
    }

    /// Re-run the existing host gatherer after actuation only while an HTTP
    /// bench request is waiting. This second observation is the proof boundary:
    /// the pre-actuation input can still contain the pane the actuator just
    /// removed, while a desired-state or actuator report is not real topology.
    async fn observe_bench_completions(&self) {
        let Some(completion) = self.bench_completion.as_ref() else {
            return;
        };
        if !completion.has_pending() {
            return;
        }

        let ctx = self.context();
        match self.hooks.cycle_input.gather_cycle_input(&ctx).await {
            Ok(input) => completion.observe(&input),
            Err(error) => tracing::warn!(
                company = %self.company_key,
                %error,
                "post-actuation bench completion gather failed; leaving HTTP wait pending"
            ),
        }
    }

    /// Duty #2 — HealthMonitor: gather the host snapshot, fold it into incident
    /// candidates and apply them, watermark in the same commit.
    pub async fn run_health_monitor(&self) {
        let ctx = self.context();
        let snapshot = match self.hooks.health.gather_health(&ctx).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(company = %self.company_key, %error, "health gather failed; skipping pass");
                return;
            }
        };
        let result = self
            .company
            .mutate(MutationClass::Normal, MutationName("duty.health_monitor"), move |ledgers| {
                let manifest = organization::read(ledgers)?;
                let ctx = organization::company_context(&manifest)?;
                let now = ledgers.now().0;
                let candidates = health_collect::collect(&manifest, &snapshot);
                let (mut state, _warning) = health::read(ledgers, &ctx).into_parts();
                state.cursors = snapshot.log_cursors.clone();
                let outcome =
                    health::apply_cycle(&mut state, &candidates, now, &health::NeverResolves);
                health::write(ledgers, &state);
                supervisor_watermark::record_success(ledgers, &ctx, Duty::HealthMonitor, now);
                Ok(outcome)
            })
            .await;
        // THE OUTCOME USED TO GO IN THE BIN, and that is the whole of the
        // second defect the 22:17 outage exposed. `apply_cycle` returns which
        // incidents were newly raised and which were resolved; this call site
        // discarded the value, so a health monitor that had raised
        // `supervisor_not_running` 707 consecutive times told nobody, anywhere,
        // ever. Every alarm this company can raise was written to a document
        // and to no reader.
        //
        // The daemon log is the surface, and it is the correct one rather than
        // the convenient one. `MailboxEnvelope` has carried a `health_incident`
        // field since the port with no producer, so the company mailbox LOOKS
        // like the intended destination — but a mailbox is read by Pi agents
        // inside the runtime, and the incidents that matter most are exactly
        // the ones where no agent is up to read anything. `runtime_unattended`
        // is unreadable by definition. A supervisor's alarms have to survive
        // their own subject.
        //
        // NEW incidents only, plus resolutions. `apply_cycle` already dedups a
        // repeat sighting into a count on the existing record, so a fault that
        // persists for a week is one line and one recovery line, not 707.
        match result {
            Ok(outcome) => {
                for incident in &outcome.new_incidents {
                    tracing::warn!(
                        company = %self.company_key,
                        kind = %incident.kind,
                        detail = %incident.detail,
                        fingerprint = %incident.fingerprint,
                        since = %incident.first_seen_at,
                        "health incident RAISED"
                    );
                }
                for fingerprint in &outcome.resolved_fingerprints {
                    tracing::info!(
                        company = %self.company_key,
                        fingerprint = %fingerprint,
                        "health incident resolved"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(company = %self.company_key, %error, "health monitor commit refused")
            }
        }
    }

    /// Duty #3 — MailboxWake: compute the pure dispatch plan from the committed
    /// snapshot, deliver the ordered envelopes off-thread, then record the
    /// per-effect outcome and the watermark in one commit.
    pub async fn run_mailbox_wake(&self) {
        let ctx = self.context();
        let envelopes = self.company.read(|snapshot| {
            let ledgers = snapshot.ledgers();
            let Ok(manifest) = organization::read(ledgers) else { return Vec::new() };
            let Ok(ledger) = supervision::read(ledgers, &manifest) else { return Vec::new() };
            let plan = supervision::dispatch_plan(&ledger);
            let mut ordered = Vec::new();
            ordered.extend(plan.urgent);
            ordered.extend(plan.routine);
            ordered
                .into_iter()
                .filter_map(|id| {
                    ledger.effect(&id).map(|effect| EffectEnvelope {
                        id: effect.id.clone(),
                        kind: effect.kind.clone(),
                        payload: serde_json::to_value(&effect.payload)
                            .unwrap_or(serde_json::Value::Null),
                    })
                })
                .collect::<Vec<_>>()
        });

        let outcome: DeliveryOutcome = self.hooks.delivery.deliver(&ctx, envelopes).await;
        // WHY a dispatch failed reaches an operator HERE — the pass boundary
        // that already reports — and not from the pure core that knows it.
        // Every one of these used to be an `Err(_)` with no field to travel in,
        // so a delivery failure was readable only as an id and a count that
        // climbed toward the breaker limit with nothing saying what was wrong.
        // The commit below is unchanged: it still keys on the effect id alone.
        for failure in &outcome.failed {
            tracing::warn!(
                company = %self.company_key,
                effect = %failure.effect_id,
                reason = %failure.reason,
                "supervision effect delivery failed"
            );
        }

        let result = self
            .company
            .mutate(MutationClass::Normal, MutationName("duty.mailbox_wake"), move |ledgers| {
                let manifest = organization::read(ledgers)?;
                let ctx = organization::company_context(&manifest)?;
                let now = ledgers.now().0;
                if !outcome.delivered.is_empty() {
                    supervision::mark_delivered(ledgers, &manifest, &outcome.delivered)?;
                }
                for failure in &outcome.failed {
                    // Best-effort: an unknown id must not roll back a real
                    // delivery recorded in the same pass. Still best-effort,
                    // and the control flow is unchanged — but the refusal is
                    // now READ before it is discarded: `let _ =` here meant a
                    // failure count that never advanced (an unknown effect id,
                    // a refused ledger write) looked exactly like one that did.
                    if let Err(error) =
                        supervision::record_delivery_failure(ledgers, &manifest, &failure.effect_id)
                    {
                        tracing::warn!(
                            effect = %failure.effect_id,
                            %error,
                            "the delivery failure of a supervision effect could not be recorded; \
                             the breaker only advances on a recorded failure"
                        );
                    }
                }
                supervisor_watermark::record_success(ledgers, &ctx, Duty::MailboxWake, now);
                Ok(())
            })
            .await;
        if let Err(error) = result {
            tracing::warn!(company = %self.company_key, %error, "mailbox wake commit refused");
        }
    }

    /// Duty #7 — ReminderDispatch: fire every due durable reminder and re-arm
    /// it. No host hook — `evaluate_reminders` is pure over the ledger and the
    /// injected clock; the fire, the re-arm and the watermark commit together.
    ///
    /// This duty is the ONLY caller of `evaluate_reminders`, and chiefd is the
    /// only writer of a reminder. The launcher reads and renders. Stating that
    /// here because the recurring failure in this system is a durable layer and
    /// a visible layer that are different systems where the durable one does not
    /// drive the visible one — the reminder is durable HERE, and every count the
    /// operator sees is downstream of this commit.
    pub async fn run_reminder_dispatch(&self) {
        // Cheap read-only guard BEFORE the writer. `mutate` is not free — it
        // re-reads the ledger, clones it, and runs the protected-schedule
        // reconcile — and this duty is on the reactive fan-out, so it is woken
        // by every reconcile/mailbox nudge in the company, not only by its own
        // alarm. Paying a full writer round-trip per nudge on a company with
        // nothing armed is exactly the "idle must trend to zero" violation this
        // repository keeps paying for (#122). One snapshot read answers it.
        //
        // Fails OPEN deliberately: an unreadable snapshot falls through to the
        // real pass rather than skipping it, because a reminder that silently
        // stops firing is the defect, and the writer will report its own error.
        let now = self.clock.wall().0;
        let due_now = self.company.read(|snapshot| {
            let ledgers = snapshot.ledgers();
            let manifest = organization::read(ledgers).ok()?;
            let ledger = supervision::read(ledgers, &manifest).ok()?;
            Some(supervision::next_reminder_due_at(&ledger, now).is_some_and(|due| due <= now))
        });
        if due_now == Some(false) {
            return;
        }

        let result = self
            .company
            .mutate(MutationClass::Normal, MutationName("duty.reminder_dispatch"), move |ledgers| {
                let manifest = organization::read(ledgers)?;
                let ctx = organization::company_context(&manifest)?;
                let now = ledgers.now().0;
                let report = supervision::evaluate_reminders(ledgers, &manifest)?;
                supervisor_watermark::record_success(ledgers, &ctx, Duty::ReminderDispatch, now);
                Ok((report.fired.len(), report.retired.len()))
            })
            .await;
        match result {
            Ok((fired, retired)) if fired > 0 || retired > 0 => {
                tracing::info!(
                    company = %self.company_key, fired, retired, "reminder dispatch committed changes"
                );
                // Enqueuing the effect is not delivery. `MailboxWake` is the
                // duty that turns a pending effect into a mailbox row, and with
                // a trigger wired its own period is the 5-minute reactive floor
                // — and nothing else nudges it on our behalf, because the
                // existing nudges come from reconcile and the delivery
                // sink's own waker, none of which a reminder passes through.
                // Without this the reminder is *durable and on time* and then
                // sits up to three minutes before the person can see it, which
                // for a reminder is the whole product failing quietly.
                //
                // Gated on `fired > 0` so a pass that only retired an expired
                // row does not wake the fleet for nothing, mirroring the
                // `report.applied` gate on the reconcile nudge above: a nudge
                // that cannot lead to delivery is a spin.
                if fired > 0 {
                    if let Some(trigger) = &self.reconcile_trigger {
                        trigger.notify_one();
                    }
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(company = %self.company_key, %error, "reminder dispatch refused")
            }
        }
    }

    /// ReminderDispatch's dynamic per-iteration sleep — the same alarm-clock
    /// shape as [`Self::health_monitor_next_interval`], for the same
    /// reason: a reminder duty on a fixed cadence would be a poll, and a poll is
    /// a defect in this repository whether or not anything is armed.
    ///
    /// Sleep exactly until the earliest armed reminder, capped at
    /// the reactive fallback floor. **A company with no reminders armed rests at
    /// the floor doing nothing at all** — `next_reminder_due_at` returns `None`
    /// and the pass finds nothing to commit, so idle costs one blocked task and
    /// zero writes. That is the whole idle-trends-to-zero requirement for this
    /// feature.
    ///
    /// Past-due reminders are deliberately included in the alarm (they are the
    /// work), but unlike deadlines they cannot spin: `evaluate_reminders`
    /// ALWAYS advances or retires a reminder it fires, in the same commit, so a
    /// due reminder is never still due on the next iteration. There is therefore
    /// no #437-style backoff ladder here — a stuck-overdue state is not
    /// reachable, and inventing a ladder for it would only delay real fires.
    fn reminder_dispatch_next_interval(self: &Arc<Self>) -> NextInterval {
        let daemon = Arc::clone(self);
        Arc::new(move || {
            let floor = reactive_fallback_floor();
            // Live clock, not `ledgers.now()`: a read-only snapshot's `now` is
            // stamped at the last commit and does not advance between commits,
            // which would make this sleep never shrink while idle.
            let now = daemon.clock.wall().0;
            let due = daemon.company.read(|snapshot| {
                let ledgers = snapshot.ledgers();
                let manifest = organization::read(ledgers).ok()?;
                let ledger = supervision::read(ledgers, &manifest).ok()?;
                supervision::next_reminder_due_at(&ledger, now)
            });
            match due {
                // Nothing armed — or the snapshot was unreadable, which is the
                // same answer for scheduling purposes and must REST rather than
                // spin: a duty that hot-loops on an unreadable store is how
                // #122's 125% CPU happened.
                None => floor,
                // Already due: run promptly. The pass will clear it.
                Some(due) if due <= now => DEADLINE_EVALUATION_MIN_INTERVAL,
                Some(due) => {
                    Duration::from_millis(u64::try_from(due - now).unwrap_or(u64::MAX)).min(floor)
                }
            }
        })
    }

    // --- the schedule -------------------------------------------------------

    /// The registration table: every duty paired with a captured single-pass
    /// runner. The one place a duty is bound to its body.
    fn duty_table(self: &Arc<Self>) -> Vec<(Duty, DutyPass)> {
        fn bind<F, Fut>(daemon: &Arc<Daemon>, run: F) -> DutyPass
        where
            F: Fn(Arc<Daemon>) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = ()> + Send + 'static,
        {
            let daemon = Arc::clone(daemon);
            Arc::new(move || -> BoxFuture<'static, ()> { Box::pin(run(Arc::clone(&daemon))) })
        }
        vec![
            (
                Duty::SupervisionReconcile,
                bind(self, |d| async move { d.run_supervision_reconcile().await }),
            ),
            (Duty::HealthMonitor, bind(self, |d| async move { d.run_health_monitor().await })),
            (Duty::MailboxWake, bind(self, |d| async move { d.run_mailbox_wake().await })),
            (
                Duty::ReminderDispatch,
                bind(self, |d| async move { d.run_reminder_dispatch().await }),
            ),
        ]
    }

    /// Spawn one interval task per duty. Does NOT run the startup self-audit;
    /// [`Daemon::serve`] runs that first so this can be reused by tests that
    /// drive a `ManualClock` directly.
    ///
    /// `ready` is the manifest-readiness latch (see [`crate::manifest_ready`]):
    /// every duty task is created immediately but holds its first pass until the
    /// company's organization manifest exists. A test that has already seeded a
    /// manifest passes a receiver that is `true` from the start.
    #[must_use]
    pub fn spawn_all(
        self: &Arc<Self>,
        shutdown: watch::Receiver<bool>,
        ready: watch::Receiver<bool>,
    ) -> JoinSet<()> {
        let mut set = JoinSet::new();

        // #368: the reactive change signal (a mailbox/fence wake) is fanned out
        // to EVERY duty whose real work it represents, not just
        // SupervisionReconcile — a stopped recipient's mail must wake the
        // delivery duty (`MailboxWake`) just as promptly as it converges panes. A
        // `Notify::notify_one` wakes a single waiter, so one shared handle cannot
        // fan out to three drive loops; instead each reactive duty gets its OWN
        // `Notify`, and a tiny event-driven fan-out task (no polling: it blocks on
        // `notified()`) re-broadcasts the one waker signal to all of them. With a
        // trigger wired, each of these duties demotes its periodic timer to the
        // slow fallback floor (see `drive`).
        let reactive_triggers = self.spawn_reactive_fanout(&mut set, &shutdown);

        for (duty, pass) in self.duty_table() {
            let clock = Arc::clone(&self.clock);
            let shutdown = shutdown.clone();
            // The reconcile fan-out (#368) wakes every REACTIVE_DUTY.
            let trigger = reactive_triggers.get(&duty).cloned();
            // ReminderDispatch and HealthMonitor sleep until their next real
            // deadline rather than a fixed period; every other duty keeps the
            // fixed period (`None`).
            let next_interval = match duty {
                // ReminderDispatch sleeps until its earliest ARMED reminder, so
                // a company with none armed rests at the floor and commits
                // nothing (od:idle-cpu, same shape as #280).
                Duty::ReminderDispatch => Some(self.reminder_dispatch_next_interval()),
                // HealthMonitor (E8-S2, #824) sleeps until its earliest armed
                // staleness/confirmation deadline, with the #437 overdue
                // guard — the same shape as ReminderDispatch.
                Duty::HealthMonitor => Some(self.health_monitor_next_interval()),
                _ => None,
            };
            set.spawn(supervise(
                duty,
                pass,
                clock,
                shutdown,
                ready.clone(),
                trigger,
                next_interval,
            ));
        }
        set
    }

    /// The duties a reconcile change signal drives reactively. Each is
    /// latency-sensitive to a mailbox/fence wake: `SupervisionReconcile`
    /// converges the pane, `MailboxWake` delivers the envelope into it,
    /// the reactive duties re-check timers the same event may have armed, and
    /// `ReminderDispatch` re-reads its alarm because the same event may have
    /// ARMED A NEARER REMINDER than the sleep it is currently in the middle of.
    ///
    /// Without that last one the alarm clock is only as fresh as the last wake:
    /// a reminder armed one minute out, while the duty sits in a five-minute
    /// floor sleep, would not be looked at until that sleep expired and would
    /// fire minutes late. `run_reminder_dispatch` opens with a read-only
    /// due-check precisely so this extra membership is affordable — a nudge for
    /// a company with nothing due costs one snapshot read and no writer round
    /// trip. `HealthMonitor` (E8-S2, #824) joins for the same reason: a row
    /// change can arm a nearer staleness/confirmation deadline than the sleep
    /// its `health_monitor_next_interval` is currently in the middle of.
    const REACTIVE_DUTIES: &'static [Duty] = &[
        Duty::SupervisionReconcile,
        Duty::MailboxWake,
        Duty::ReminderDispatch,
        Duty::HealthMonitor,
    ];

    /// Duties that are reactive-primary but NOT through the shared reconcile
    /// fan-out: each gets its own dedicated `Notify` wired directly in
    /// `spawn_all`'s `trigger` match, rather than a `REACTIVE_DUTIES` entry.
    ///
    /// Named here (E8-S2, #824) so `duty_cadence_conformance` can assert every
    /// [`Duty`] is accounted for by exactly one of: [`Self::REACTIVE_DUTIES`],
    /// this list, or [`Self::NON_REACTIVE_DUTY_JUSTIFICATIONS`] — the
    /// conformance test that fails the build if a future duty is added on a
    /// bare fixed timer with no trigger and no written justification.
    const SELF_TRIGGERED_DUTIES: &'static [Duty] = &[];

    /// A duty may run on a bare fixed timer with no trigger ONLY if it
    /// appears here with a written reason. `duty_cadence_conformance` fails
    /// the build for any [`Duty`] that is in neither this list,
    /// [`Self::REACTIVE_DUTIES`], nor [`Self::SELF_TRIGGERED_DUTIES`] — so
    /// adding a duty on a bare timer is a decision that must be written down,
    /// not one that lands silently (E8-S2, #824).
    /// Empty since the long-poll duty that was its only entry was retired
    /// with its channel: every surviving duty is reactive or self-triggered.
    const NON_REACTIVE_DUTY_JUSTIFICATIONS: &'static [(Duty, &'static str)] = &[];

    /// Every [`Duty`] not accounted for by exactly one of
    /// [`Self::REACTIVE_DUTIES`], [`Self::SELF_TRIGGERED_DUTIES`], or
    /// [`Self::NON_REACTIVE_DUTY_JUSTIFICATIONS`], each described by a
    /// message naming the duty and its observed membership count. Empty
    /// means every duty is accounted for exactly once. Shared by
    /// [`Self::new`] (a runtime assertion at daemon construction) and
    /// `duty_cadence_conformance` (the build-time test, `run/tests.rs`) —
    /// one classification, checked twice, never duplicated (E8-S2, #824).
    fn duty_cadence_conformance_violations() -> Vec<String> {
        Duty::ALL
            .iter()
            .filter_map(|&duty| {
                let reactive = Self::REACTIVE_DUTIES.contains(&duty);
                let self_triggered = Self::SELF_TRIGGERED_DUTIES.contains(&duty);
                let justified =
                    Self::NON_REACTIVE_DUTY_JUSTIFICATIONS.iter().any(|(justified, _)| *justified == duty);
                let memberships = usize::from(reactive) + usize::from(self_triggered) + usize::from(justified);
                (memberships != 1).then(|| {
                    format!(
                        "{duty:?} must be reactive-primary (REACTIVE_DUTIES or SELF_TRIGGERED_DUTIES) or \
                         carry exactly one written justification in NON_REACTIVE_DUTY_JUSTIFICATIONS — saw \
                         {memberships} (reactive={reactive}, self_triggered={self_triggered}, justified={justified})"
                    )
                })
            })
            .collect()
    }

    /// Build a per-duty trigger for each reactive duty and spawn the fan-out task
    /// that re-broadcasts the single waker signal to all of them. Returns the
    /// per-duty triggers (empty when no reconcile signal is wired — e.g. a Tier-1
    /// deferred waker or a bare test daemon — in which case every duty drives on
    /// its own interval, exactly as before).
    fn spawn_reactive_fanout(
        self: &Arc<Self>,
        set: &mut JoinSet<()>,
        shutdown: &watch::Receiver<bool>,
    ) -> std::collections::HashMap<Duty, Arc<Notify>> {
        let mut triggers = std::collections::HashMap::new();
        let Some(signal) = self.reconcile_trigger.clone() else {
            return triggers;
        };
        for &duty in Self::REACTIVE_DUTIES {
            triggers.insert(duty, Arc::new(Notify::new()));
        }
        let fanned: Vec<Arc<Notify>> = triggers.values().cloned().collect();
        let mut shutdown = shutdown.clone();
        set.spawn(async move {
            loop {
                if *shutdown.borrow() {
                    break;
                }
                tokio::select! {
                    biased;
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            break;
                        }
                    }
                    // Pure event wait — no timer. One waker nudge wakes every
                    // reactive duty's own trigger; a burst coalesces to one
                    // pending permit per duty (the reconcile engine's own
                    // single-flight absorbs the rest).
                    () = signal.notified() => {
                        for trigger in &fanned {
                            trigger.notify_one();
                        }
                    }
                }
            }
        });
        triggers
    }

    /// Run one pass of every duty (in schedule order) and return. The `--once`
    /// smoke path: prove the whole loop wires end to end without waiting on any
    /// cadence.
    pub async fn run_once_all(self: &Arc<Self>) {
        for (_duty, pass) in self.duty_table() {
            pass().await;
        }
    }

    /// A clone of the daemon's shutdown-request sender (E7-S3), for the
    /// docstore mount's `POST /v1/admin/shutdown` route — a fresh clone
    /// rather than a second `watch` channel, so an HTTP shutdown request
    /// flips the SAME sender `Daemon::serve`'s select already watches
    /// ([`Daemon::fatal_shutdown`]).
    #[must_use]
    pub fn shutdown_requester(&self) -> watch::Sender<Option<String>> {
        self.fatal_shutdown.clone()
    }

    /// The production entry: mount the (optional) typed HTTP/changefeed surface,
    /// spawn every duty task held at the manifest-readiness latch, then release
    /// them (after the startup self-audit) once the company exists, until a
    /// shutdown signal drains everything gracefully.
    ///
    /// The mount comes FIRST because genesis arrives over it — see the gate's
    /// own comment below and [`crate::manifest_ready`].
    ///
    /// `docstore` is bound by the caller before this runs (via `bind_walking`
    /// as of E10-S3/#764 — a taken `:8792` walks to the next free port
    /// rather than refusing; only an exhausted range or a non-contention
    /// bind error refuses the whole daemon) and mounted on its OWN task,
    /// under the SAME `watch<bool>` the duty tasks read, and drained after
    /// them — never a bolt-on that outlives the rest. `chiefd run` always
    /// resolves and binds a per-company docstore as of E10-S2 (#763), so its
    /// only caller always passes `Some`; the parameter stays `Option` so a
    /// duties-only caller remains representable without a second entry
    /// point.
    /// The startup gate, as one task: wait (bounded) for this company's
    /// organization manifest, run the startup self-audit now that the company
    /// actually exists, then release every duty task held on `ready`.
    ///
    /// Returns whether the latch was opened. `false` means shutdown was observed
    /// while waiting — no duty ever ran a pass, and the self-audit did not run
    /// either, because a company that is stopping before it started has nothing
    /// to audit.
    ///
    /// # Why the self-audit sits HERE
    ///
    /// It is the one startup duty the genesis race skipped PERMANENTLY. `serve`
    /// calls it exactly once per process and nothing retries it, so a manifest
    /// that arrived 229 ms too late meant it never ran at all — the other five
    /// refusals in that window self-heal on the next reactive pass, and this one
    /// does not. On a brand-new company it is empty (no missed-window backlog to
    /// raise, no orphan supervision effects to reap), so nothing was lost in
    /// practice; but "empty" is a property of a fresh company, not a licence to
    /// skip it, and a daemon restarting an OLD company has a real backlog to
    /// fold into health. Running it behind the gate is what makes it run at all.
    ///
    /// A budget that expires still opens the latch: the duties then run, refuse
    /// and self-heal exactly as they did before this gate existed. The
    /// self-audit runs in that case too, and refuses in its own words.
    async fn open_the_duty_gate(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        ready: &watch::Sender<bool>,
        budget: Duration,
    ) -> bool {
        let readiness = tokio::select! {
            biased;
            _ = shutdown.changed() => None,
            readiness = crate::manifest_ready::await_manifest(
                &self.company,
                &self.company_key,
                &self.clock,
                budget,
            ) => Some(readiness),
        };
        let Some(readiness) = readiness else {
            // Draining. Leave the latch shut: a duty that has not run a pass
            // must not start one into a daemon that is stopping.
            tracing::warn!(
                company = %self.company_key,
                "chiefd run: shutdown requested before the organization manifest was ready; no \
                 duty pass ever started"
            );
            return false;
        };
        // "Cleared" is only true when a manifest was actually observed. A budget
        // that expires releases the duties anyway — that fallback is what keeps
        // this change from inventing a new way to lose a company — but saying
        // "cleared" there would be the same species of false sentence this whole
        // change exists to remove.
        if readiness.is_ready() {
            tracing::info!(
                company = %self.company_key,
                ?readiness,
                "chiefd run: organization manifest readiness gate cleared; the startup duties may run"
            );
        } else {
            tracing::error!(
                company = %self.company_key,
                ?readiness,
                "chiefd run: releasing the startup duties WITHOUT a manifest because the readiness \
                 budget expired; every duty will refuse until a manifest is committed, and the \
                 reactive self-heal will pick it up when one is"
            );
        }
        self.run_startup_self_audit().await;
        let _ = ready.send(true);
        true
    }

    pub async fn serve(self: Arc<Self>, docstore: Option<docstore::Bound>) -> Result<(), String> {
        // Arm the shutdown signal FIRST — before the attribution handler, before
        // the self-audit, and before anything binds or answers. `SignalsInfo`
        // below replaces SIGTERM's default disposition (terminate) with a
        // handler, so from that instant on a SIGTERM that nothing else is
        // listening for is *swallowed*, not fatal: the daemon would then run on
        // forever and take its supervisor's SIGKILL. tokio's signal stream
        // latches deliveries from the moment it is constructed rather than from
        // the moment it is polled, so arming it here closes that window
        // completely — the `select!` far below still observes a signal that
        // landed during startup.
        let shutdown_signal = ArmedShutdownSignal::arm();
        // #504: record who sends the graceful SIGTERM, before anything can fire
        // one. Runs on its own thread and leaves tokio's shutdown path untouched.
        crate::shutdown_attribution::install();
        let (tx, rx) = watch::channel(false);
        let mut fatal_shutdown = self.fatal_shutdown.subscribe();

        // ORDERING: the typed HTTP surface is mounted FIRST — before the duty
        // tasks and before the readiness gate below — because genesis arrives
        // over it.
        //
        // `chief_cli::genesis` starts this daemon and then POSTs
        // `/v1/org/manifest/genesis-with-models` to this daemon's own URL: the
        // company's single writer is this process, so a company is created
        // THROUGH the daemon that serves it, never before it. A gate that ran
        // ahead of this mount would wait for a write that can never be
        // delivered. Mounted on its own task, under the same shutdown watch
        // (see [`spawn_docstore_mount`]).
        let docstore_task =
            docstore.map(|bound| spawn_docstore_mount(self.company_key.clone(), bound, rx.clone()));

        // The genesis race, closed. Every duty task is spawned now, exactly as
        // before, but each one holds its first pass on this latch until the
        // company's organization manifest exists (see `wait_for_company_ready`).
        // The daemon's shape therefore does not depend on whether genesis has
        // run: a shutdown drains the same set of tasks either way.
        let (ready_tx, ready_rx) = watch::channel(false);
        let mut set = self.spawn_all(rx.clone(), ready_rx);

        // The gate itself, plus the startup self-audit that must not run before
        // it either, on a task of their own inside the SAME `JoinSet` — so the
        // drain owns them like any duty and `serve` never blocks on a wait.
        //
        // This is a WAIT, not a silencer. A budget that expires releases the
        // latch anyway and says so at ERROR: the duties then run, refuse, and
        // self-heal exactly as they did before this gate existed.
        {
            let daemon = Arc::clone(&self);
            let mut shutdown = rx.clone();
            set.spawn(async move {
                daemon
                    .open_the_duty_gate(
                        &mut shutdown,
                        &ready_tx,
                        crate::manifest_ready::MANIFEST_READY_BUDGET,
                    )
                    .await;
            });
        }
        drop(rx);
        tracing::info!(
            company = %self.company_key,
            mode = ?self.actuation_mode,
            docstore_mounted = docstore_task.is_some(),
            "chiefd run: duty scheduler started"
        );

        let initial_fatal_reason = fatal_shutdown.borrow().clone();
        let fatal_reason = if let Some(reason) = initial_fatal_reason {
            Some(reason)
        } else {
            tokio::select! {
                () = shutdown_signal.wait() => None,
                changed = fatal_shutdown.changed() => {
                    if changed.is_err() {
                        None
                    } else {
                        fatal_shutdown.borrow().clone()
                    }
                }
            }
        };
        let started = std::time::Instant::now();
        let deadline = started + SHUTDOWN_BUDGET;
        if let Some(reason) = &fatal_reason {
            tracing::error!(company = %self.company_key, %reason, "fatal runtime-ownership handoff requested; draining in-flight duty passes");
        } else {
            tracing::info!(company = %self.company_key, "shutdown signal received; draining in-flight duty passes");
        }
        // #504: name the actor behind this shutdown (or log the anomaly of an
        // unrecorded one) — the drain below is unchanged.
        crate::shutdown_attribution::log_shutdown_actor(&self.company_key).await;
        // Tell every task to stop starting new passes; a pass already inside its
        // commit runs to completion (CompanyDb::mutate is exactly-once even if
        // dropped, so this is graceful, not merely best-effort).
        let _ = tx.send(true);

        // Phase 1 — duty passes, under a deadline of OUR own. `drive()` only
        // re-checks the shutdown flag *between* passes, so a pass already inside
        // a long await would otherwise hold this join for tens of seconds —
        // past any supervisor's grace. Cooperative first, abort second.
        let duties_drained =
            tokio::time::timeout(phase_budget(deadline, DUTY_DRAIN_BUDGET), async {
                while set.join_next().await.is_some() {}
            })
            .await
            .is_ok();
        if !duties_drained {
            tracing::warn!(
                company = %self.company_key,
                budget_ms = DUTY_DRAIN_BUDGET.as_millis() as u64,
                "chiefd run: duty drain exceeded its budget; aborting the remaining passes"
            );
            set.abort_all();
        }
        drop(set);

        // Phase 2 — the docstore listener respects the same flag, so the port is
        // released before the process exits. Bounded for a concrete reason:
        // `axum`'s graceful shutdown waits for every in-flight *connection*, and
        // `/v1/docs/watch` is the remaining normalized-changefeed compatibility
        // route and is an SSE stream by design. Its producer sees the same
        // shutdown watch and sends EOF before this graceful drain starts; the
        // timeout remains a safety net for a genuinely stuck connection.
        let mut docstore_drained = true;
        if let Some(mut task) = docstore_task {
            if tokio::time::timeout(phase_budget(deadline, DOCSTORE_DRAIN_BUDGET), &mut task)
                .await
                .is_err()
            {
                docstore_drained = false;
                tracing::warn!(
                    company = %self.company_key,
                    budget_ms = DOCSTORE_DRAIN_BUDGET.as_millis() as u64,
                    "chiefd run: docstore drain exceeded its budget (long-lived watch streams); \
                     aborting the listener"
                );
                task.abort();
            }
        }

        // Phase 3 — quiesce the writer: drain the queue, `wal_checkpoint`, join
        // the thread. It joins a real OS thread that may be mid-job, so it runs
        // on a blocking task and is bounded like the rest; the checkpoint is
        // worth waiting for, but not worth a SIGKILL.
        let company = Arc::clone(&self.company);
        let mut writer = tokio::task::spawn_blocking(move || company.shutdown());
        let writer_drained =
            tokio::time::timeout(phase_budget(deadline, WRITER_SHUTDOWN_BUDGET), &mut writer)
                .await
                .is_ok();
        if !writer_drained {
            tracing::warn!(
                company = %self.company_key,
                budget_ms = WRITER_SHUTDOWN_BUDGET.as_millis() as u64,
                "chiefd run: writer shutdown exceeded its budget; exiting without the final \
                 wal checkpoint (committed data is durable; the WAL is simply left for recovery)"
            );
        }

        tracing::info!(
            company = %self.company_key,
            elapsed_ms = started.elapsed().as_millis() as u64,
            duties_drained,
            docstore_drained,
            writer_drained,
            "chiefd run: stopped"
        );
        match fatal_reason {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }
}

/// Total wall-clock budget for the whole graceful drain, measured from the
/// instant SIGTERM/SIGINT is observed.
///
/// Deliberately well inside the supervisor's grace (`scripts/promote-chiefd.sh`
/// waits 10 s before escalating to SIGKILL): the drain owns a deadline of its
/// own rather than borrowing the caller's, because a drain whose only bound is
/// "whenever the killer gives up" is not a drain — it is the crash path with
/// extra logging. Every restart before this landed took that crash path.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(7);

/// Phase 1 ceiling: in-flight duty passes.
const DUTY_DRAIN_BUDGET: Duration = Duration::from_secs(4);

/// Phase 2 ceiling: the mounted typed listener (watchers are asked to EOF first).
const DOCSTORE_DRAIN_BUDGET: Duration = Duration::from_secs(2);

/// Phase 3 ceiling: writer quiesce + `wal_checkpoint(TRUNCATE)` + thread join.
const WRITER_SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

/// #370: how long the tokio runtime drop may wait for any still-outstanding
/// blocking task (the writer and docstore are already joined/aborted inside
/// `serve`). Kept short and well inside the supervisor's SIGKILL grace so
/// process exit is never held hostage to a blocking child that cannot be
/// cancelled.
const RUNTIME_SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// This phase's ceiling, clamped to what is left of the whole-drain budget, so
/// three phases that each individually fit can never add up past
/// [`SHUTDOWN_BUDGET`]. Never zero: a phase always gets one chance to finish.
fn phase_budget(deadline: std::time::Instant, cap: Duration) -> Duration {
    deadline
        .saturating_duration_since(std::time::Instant::now())
        .min(cap)
        .max(Duration::from_millis(50))
}

/// One duty's interval loop: on each `clock.sleep(interval)`, run one pass;
/// break when shutdown is signalled. Biased so a pending shutdown always wins
/// the race against a due tick, and the loop-top check makes an
/// already-signalled shutdown (set before the task was polled) stop it too. A
/// shutdown that arrives *during* a pass lets that pass finish — the loop only
/// re-checks at the top, and no new pass starts.
///
/// `trigger` is the optional Tier-2 accelerator (`runtime-waker`'s
/// `NotifyReconcileTrigger` seam, `chiefd_host::runtime_waker`): when set, a
/// `notify_one` races the interval sleep and a pass runs the instant it fires
/// — this is the "the real runtime wake" for a newly-pending recipient, not just
/// "wait up to one interval and hope". `None` (every duty but
/// `SupervisionReconcile` today) behaves exactly as before: interval-only.
/// `Notify::notify_one` coalesces a burst into one pending permit, so a flurry
/// of wakes drives at most one extra out-of-cadence pass, never a busy loop.
async fn drive(
    duty: Duty,
    pass: DutyPass,
    clock: SharedClock,
    mut shutdown: watch::Receiver<bool>,
    mut ready: watch::Receiver<bool>,
    trigger: Option<Arc<Notify>>,
    next_interval: Option<NextInterval>,
) {
    // THE GENESIS GATE. A duty task is created the moment the daemon serves, but
    // its first pass waits here until the company's organization manifest exists.
    // Genesis starts this daemon and then writes the manifest THROUGH it, so on
    // a brand-new company every duty used to run its first pass against a
    // company that was not there yet and refuse `unknown-company` — measured at
    // 229 ms, on every single launch. The task still EXISTS while it waits, so a
    // shutdown drains exactly the same set of tasks whether or not genesis has
    // run. See [`crate::manifest_ready`] for the bounded wait that lifts this.
    if !wait_for_company_ready(&mut ready, &mut shutdown).await {
        return;
    }
    // #368 reactive-primary: when a change signal is wired, the reactive channel
    // is the schedule and the periodic timer is demoted to a SLOW fallback floor
    // (suppressed as the fast path) — a converged company runs this duty only
    // once every few minutes at rest instead of on the 30 s ownership-probe
    // cadence, while a real change still fires a pass in <100 ms through the
    // trigger. A duty with no trigger keeps its own cadence unchanged.
    let period = match &trigger {
        Some(_) => {
            let interval =
                Duration::from_millis(u64::try_from(duty.interval_ms()).unwrap_or(u64::MAX));
            interval.max(reactive_fallback_floor())
        }
        None => Duration::from_millis(u64::try_from(duty.interval_ms()).unwrap_or(u64::MAX)),
    };
    // A reactive duty's normal wake comes from a successful mutation calling
    // `wake_reconcile` -- but the daemon's OWN first write after a fresh boot
    // can be exactly the write that fails (the company not yet visible to
    // this process, a boot-before-genesis race), so a wake can never arrive:
    // nothing succeeded to send it. Without this, such a duty's first pass
    // would wait out the full reactive-fallback floor (a minimum of 60s) no
    // matter how quickly the company actually becomes ready, and every
    // caller that boots a duty daemon already assumes its FIRST pass runs
    // promptly, not after a multi-minute floor. Run one pass immediately,
    // before ever entering the sleep-or-trigger race, for every duty that
    // has been demoted to that floor (`trigger.is_some()`) -- steady-state
    // behavior (the loop below) is unchanged; this affects only the first
    // iteration. Idempotent by construction: a pass on an already-converged
    // company is the same cheap no-op cycle it already is at rest.
    if trigger.is_some()
        && !*shutdown.borrow()
        && run_pass_until_shutdown(&pass, &mut shutdown).await
    {
        return;
    }
    loop {
        if *shutdown.borrow() {
            break;
        }
        // od:idle-cpu #280: a dynamic override (the alarm-clock duties'
        // sleep-until-next-deadline) is recomputed fresh each iteration; every
        // other duty falls back to the fixed `period` above.
        let interval = next_interval.as_ref().map_or(period, |f| f());
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                // Sender set `true` or was dropped: either way, stop.
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            () = clock.sleep(interval) => {
                if run_pass_until_shutdown(&pass, &mut shutdown).await {
                    break;
                }
            }
            () = wait_for_trigger(trigger.as_deref()) => {
                if run_pass_until_shutdown(&pass, &mut shutdown).await {
                    break;
                }
            }
        }
    }
}

/// #370: run one duty pass, but ABANDON it at its next await point the instant
/// shutdown flips — do not await it to completion. `drive` used to `pass().await`
/// unconditionally, so a pass already in flight ran to the end even after
/// SIGTERM, so a SIGTERM landing mid-pass waited it out by design. Cancelling
/// is safe by construction: `CompanyDb::mutate` is exactly-once even if the
/// caller future is dropped, and every duty commits its durable work last (an
/// aborted pass simply replays idempotently next boot). Returns `true` when
/// shutdown was observed (the caller stops the loop).
async fn run_pass_until_shutdown(pass: &DutyPass, shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        biased;
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
        () = pass() => false,
    }
}

/// Hold a duty task until the company's organization manifest exists.
///
/// Returns `true` when the company is ready and the duty may run, `false` when
/// shutdown was observed first — in which case the task simply returns, having
/// run nothing, and the drain joins it immediately.
///
/// The latch is checked before it is awaited, so a company whose manifest is
/// already durable (every restart) costs one `borrow()` and no wait at all. A
/// dropped latch sender is treated as shutdown for the same reason a dropped
/// shutdown sender is: the daemon that owned it is gone.
async fn wait_for_company_ready(
    ready: &mut watch::Receiver<bool>,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    if *ready.borrow() {
        return true;
    }
    tokio::select! {
        biased;
        _ = shutdown.changed() => false,
        latched = ready.wait_for(|flagged| *flagged) => latched.is_ok(),
    }
}

/// Await a wired trigger's nudge, or never resolve if none is wired — lets
/// `drive` race a single trigger-shaped branch regardless of whether this duty
/// has an accelerator, with no `Option`-shaped branching duplicated at the
/// `select!` call site.
async fn wait_for_trigger(trigger: Option<&Notify>) {
    match trigger {
        Some(notify) => notify.notified().await,
        None => std::future::pending::<()>().await,
    }
}

/// The per-duty supervisor: runs [`drive`] on its own `tokio` task and
/// watches it. `drive` only ever returns cleanly when it has observed
/// shutdown, so ANY other completion — panic or cancellation — is an
/// unexpected death. Those are logged LOUDLY (duty name + panic payload) and
/// the duty is immediately respawned, so a single bad pass degrades to one
/// skipped cycle instead of the duty vanishing for the rest of the process's
/// life (#340: the previous `JoinSet` drain silently discarded exactly this
/// signal). A death observed after shutdown has been signalled is logged but
/// NOT respawned — the daemon is already draining.
async fn supervise(
    duty: Duty,
    pass: DutyPass,
    clock: SharedClock,
    shutdown: watch::Receiver<bool>,
    ready: watch::Receiver<bool>,
    trigger: Option<Arc<Notify>>,
    next_interval: Option<NextInterval>,
) {
    loop {
        let handle = tokio::spawn(drive(
            duty,
            Arc::clone(&pass),
            Arc::clone(&clock),
            shutdown.clone(),
            ready.clone(),
            trigger.clone(),
            next_interval.clone(),
        ));
        match handle.await {
            Ok(()) => {
                tracing::info!(duty = ?duty, "duty task stopped");
                break;
            }
            Err(join_err) => {
                if join_err.is_panic() {
                    let message = panic_message(join_err.into_panic().as_ref());
                    tracing::error!(duty = ?duty, panic = %message, "duty task panicked; restarting");
                } else {
                    tracing::error!(duty = ?duty, "duty task was cancelled; restarting");
                }
                if *shutdown.borrow() {
                    // Already draining: don't spawn a fresh pass into a
                    // daemon that is trying to stop.
                    break;
                }
                // Loop: respawn `drive` for this duty and keep supervising.
            }
        }
    }
}

/// Best-effort human-readable text for a `JoinError`'s panic payload —
/// `std::panic!` payloads are almost always `&str` or `String`; anything else
/// is named rather than silently dropped.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Spawn the mounted `org_documents` docstore surface onto its own `tokio`
/// task. Its graceful-shutdown future resolves when `shutdown` flips `true` (or
/// the sender drops), so `axum` stops accepting and drains in-flight requests on
/// the identical signal the duty passes respect — the listener never outlives
/// the duty tasks it runs beside.
///
/// Factored out of [`Daemon::serve`] so the exact production mount (this
/// `serve_bound` + watch-driven shutdown composition) is what the tests drive,
/// not a parallel copy.
fn spawn_docstore_mount(
    slug: String,
    bound: docstore::Bound,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // The graceful listener shutdown and every mounted SSE watcher must see
        // the same state transition. The generic document routes are retired;
        // this is solely for the surviving normalized-changefeed watcher.
        let watcher_shutdown = shutdown.clone();
        let signal = async move {
            // `wait_for` needs `&mut self`; rebind mutably inside the future that
            // owns the receiver so the closure is `Send + 'static` for axum.
            let mut shutdown = shutdown;
            let _ = shutdown.wait_for(|flagged| *flagged).await;
        };
        match docstore::serve_bound_with_watch(bound, signal, Some(watcher_shutdown)).await {
            Ok(()) => {
                tracing::info!(company = %slug, "chiefd run: docstore org_documents surface stopped")
            }
            Err(error) => tracing::error!(
                company = %slug,
                %error,
                "chiefd run: docstore surface stopped with an error"
            ),
        }
    })
}

/// A shutdown signal that is *already listening* — separated from the await so
/// registration can happen before any startup work.
///
/// Constructing a `tokio` signal stream registers the handler and starts
/// latching deliveries immediately; a signal that lands before the first
/// `recv()` is remembered, not lost. That is the whole point of this type.
/// [`wait_for_signal`] arms and awaits in one step, which is correct only where
/// nothing has installed a competing SIGTERM handler beforehand: once
/// [`crate::shutdown_attribution::install`] runs, SIGTERM no longer terminates
/// the process by default, so any gap between that install and this
/// registration is a window in which a supervisor's SIGTERM is silently
/// discarded and the daemon runs until SIGKILL. [`Daemon::serve`] therefore
/// arms this first and awaits it last.
struct ArmedShutdownSignal {
    #[cfg(unix)]
    term: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    interrupt: Option<tokio::signal::unix::Signal>,
}

impl ArmedShutdownSignal {
    /// Register the handlers now. Failure is non-fatal and logged: the daemon
    /// keeps running and simply falls back to whichever stream did arm.
    fn arm() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let term = signal(SignalKind::terminate())
                .map_err(|error| {
                    tracing::warn!(%error, "cannot install SIGTERM handler; falling back to SIGINT only");
                })
                .ok();
            let interrupt = signal(SignalKind::interrupt())
                .map_err(|error| {
                    tracing::warn!(%error, "cannot install SIGINT handler; falling back to SIGTERM only");
                })
                .ok();
            Self { term, interrupt }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// Resolve on the first SIGTERM/SIGINT observed since [`Self::arm`] — including
    /// one that arrived before this is ever polled.
    async fn wait(mut self) {
        #[cfg(unix)]
        {
            match (self.term.as_mut(), self.interrupt.as_mut()) {
                (Some(term), Some(interrupt)) => {
                    tokio::select! {
                        _ = term.recv() => {}
                        _ = interrupt.recv() => {}
                    }
                }
                (Some(only), None) | (None, Some(only)) => {
                    only.recv().await;
                }
                // Neither handler could be installed: fall back to tokio's own
                // ctrl-c helper rather than never resolving at all.
                (None, None) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// Await SIGINT or, on unix, SIGTERM — whichever the supervisor sends first.
///
/// Arms at the await point, so use it only where no SIGTERM handler is already
/// installed; [`Daemon::serve`] uses [`ArmedShutdownSignal`] directly instead.
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(term) => term,
            Err(error) => {
                tracing::warn!(%error, "cannot install SIGTERM handler; falling back to SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

// --- CLI surface -----------------------------------------------------------

/// Parsed `chiefd run` configuration.
#[derive(Debug, Clone)]
struct Config {
    /// THE ONE INPUT. The company directory, canonical and absolute — every
    /// path this daemon touches hangs off it (`crate::company_dir`) and its
    /// hash is the company's identity on the wire.
    ///
    /// There is deliberately no slug beside it. The slug is a column of the
    /// `organization` row, so a daemon told one on argv would be holding a
    /// second answer to a question the store already answers — and holding it
    /// from before genesis wrote the first one.
    dir: PathBuf,
    /// The runtime server socket (`runtime -L <socket>`) the actuator observes and
    /// actuates against. Resolved by [`resolve_runtime_socket`] at startup: a
    /// DEMAND wins, otherwise the company's own recorded runtime owner names
    /// it, and only a company nobody claims falls back to this preference.
    ///
    /// Holds the PREFERENCE until resolution overwrites it: `ORG_LAUNCHER_RUNTIME_SOCKET`
    /// when the client set one, and the company key when it did not.
    runtime_socket: String,
    /// A socket an operator DEMANDED on argv (`--runtime-socket`).
    ///
    /// The demand and the preference above used to be one field, and the flag
    /// and the environment variable both wrote it. That is what made the
    /// upgrade to per-company sockets un-startable: `chief` has no way to read
    /// a company's claim before a daemon serves it, so the socket it passes at
    /// spawn is a GUESS, and a guess arriving as a demand turned the adoption
    /// tier below into a refusal for every company created before `cb63690a0`.
    /// A demand is what a human types; a preference is what the client guesses,
    /// and a live claim outranks a guess — which is exactly `boot_socket`'s own
    /// precedence, now stated the same way on both sides of the spawn.
    runtime_socket_demanded: Option<String>,
    /// The pinned pi binary panes are launched with.
    pi_binary: PathBuf,
    /// The launcher root every pane's `ORG_LAUNCHER_ROOT` is stamped from.
    launcher_root: PathBuf,
    once: bool,
    /// Mount the typed native reader without resolving runtime ownership, seeding
    /// rows, scheduling duties, or actuating. This is for an isolated snapshot
    /// reader (for example the two-ChiefD stale-footer E2E), never a live
    /// company supervisor.
    serve_only: bool,
}

/// The runtime socket env, sharing the name a launched pane already reads so
/// operator config is one variable. (The `chiefd_host::auth` constant this
/// used to cite went with the pane-ancestry authenticator in #751/P7.)
const RUNTIME_SOCKET_ENV: &str = "ORG_LAUNCHER_RUNTIME_SOCKET";
/// The pinned pi binary env (host executor + launch catalog).
const PI_BINARY_ENV: &str = "CHIEFD_PI_BINARY";
/// The launcher-root env.
const LAUNCHER_ROOT_ENV: &str = "ORG_LAUNCHER_ROOT";

// The resource root a pane's `ORG_LAUNCHER_ROOT` is stamped from, resolved
// from THIS BINARY'S OWN LOCATION.
//
// # TOMBSTONE: `~/.chief/launcher-root`, `LAUNCHER_ROOT_RELATIVE`, `default_launcher_root`
//
// This was a three-step ladder ending in a guess, and every step of it is
// deleted. It read a pointer FILE that `bun run release` wrote with the
// absolute path of the source CHECKOUT, and fell back to
// `$HOME/.local/share/tribe-launcher` — a path a checkout never occupies.
//
// Two separate incidents came out of that pair, and both are worth keeping
// because the replacement is designed against them:
//
//   * **Nothing read the pointer at all**, for a while. Resolution went
//     straight to the fallback, materialization resolved every person's
//     extension sources against a directory that did not exist, and each
//     person materialized with an EMPTY `pi-home/extensions/`. The company's
//     CEO came up with no `org_*` tools — no `org_hire`, no `org_roster` —
//     and answered "this session doesn't have the org tools" to every
//     instruction to staff the company, while genesis reported
//     "✅ Company launched · CEO booted". Every per-person materialization
//     failure behind it was contained by policy and never surfaced.
//   * **The fallback was a hardcoded `/root/...`**, which is right only on a
//     Linux box running as root. On macOS chiefd stamped a nonexistent
//     directory into every pane as `ORG_LAUNCHER_ROOT`, the intercom ran its
//     subprocesses with that as their `cwd`, and a nonexistent cwd makes Node
//     report `ENOENT` against the BINARY — so the operator saw
//     `spawn /Users/…/bun ENOENT` for a bun that was present and runnable.
//
// A pointer had to go for a product reason as well: it made the installed
// binaries a front end for a git working copy that had to stay on disk at a
// compatible revision, so a clone-free install and `chief upgrade` could not
// exist while it did. See `host_primitives::install` for the replacement and
// the three properties it gets for free.
//
// **There is no fallback now, deliberately.** A daemon that cannot resolve
// resources refuses at parse time and says which two ways it could have been
// told. The old fallback's whole contribution was to turn "I do not know" into
// a plausible-looking wrong answer that surfaced a day later as a CEO with no
// tools.
fn usage() -> &'static str {
    "usage: chiefd run --dir <company directory> --pi-binary <absolute path> \
     [--runtime-socket <name>] [--launcher-root <dir>] [--once] [--serve-only]\n\
     env: ORG_LAUNCHER_RUNTIME_SOCKET, CHIEFD_PI_BINARY, ORG_LAUNCHER_ROOT\n\
     chiefd actuates the live runtime directly; --serve-only is the non-actuating snapshot-reader mode \
     used by isolated E2E topology proofs"
}

fn parse_config(mut args: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut dir: Option<PathBuf> = None;
    let runtime_socket = std::env::var(RUNTIME_SOCKET_ENV).ok();
    let mut runtime_socket_demanded: Option<String> = None;
    let mut pi_binary = std::env::var(PI_BINARY_ENV).ok().map(PathBuf::from);
    let mut launcher_root = std::env::var(LAUNCHER_ROOT_ENV).ok().map(PathBuf::from);
    let mut once = false;
    let mut serve_only = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = Some(PathBuf::from(args.next().ok_or("--dir needs a path")?)),
            "--runtime-socket" => {
                runtime_socket_demanded = Some(args.next().ok_or("--runtime-socket needs a name")?)
            }
            "--pi-binary" => {
                pi_binary = Some(PathBuf::from(args.next().ok_or("--pi-binary needs a path")?))
            }
            "--launcher-root" => {
                launcher_root =
                    Some(PathBuf::from(args.next().ok_or("--launcher-root needs a path")?));
            }
            "--once" => once = true,
            "--serve-only" => serve_only = true,
            other => return Err(format!("unknown argument: {other}\n{}", usage())),
        }
    }

    // THE ONE INPUT, and it is canonicalized here rather than trusted as
    // typed. The company key is `sha256(<dir>)`, so `.`, a trailing slash or a
    // symlinked spelling would key one company two ways — the exact split the
    // composite key existed to paper over. A daemon that cannot resolve the
    // directory it was pointed at has nothing to serve and refuses.
    let dir = dir.ok_or_else(|| format!("a company directory is required\n{}", usage()))?;
    let dir = dir.canonicalize().map_err(|error| {
        format!(
            "chiefd cannot resolve the company directory {}: {error}\n{}",
            dir.display(),
            usage()
        )
    })?;
    // The bare fallback is the COMPANY KEY, not a name: two directories may
    // hold companies called the same thing, and a socket name they shared
    // would put one company's panes on the other's runtime server. It is only
    // ever REACHED for a company with no live runtime-ownership claim —
    // `resolve_runtime_socket` prefers the claim itself (#63/#64).
    let runtime_socket = runtime_socket
        .map(|socket| socket.trim().to_owned())
        .filter(|socket| !socket.is_empty())
        .unwrap_or_else(|| crate::company_dir::company_key(&dir));
    // NO DEFAULT, and never a bare name.
    //
    // This value is published in every person's launch-catalog entry and is
    // literally what their pane execs. It used to fall back to `pi`, which
    // nothing set, so every company that has ever run shipped a bare name to
    // its panes and let the tmux server's PATH decide whether anybody could
    // start. On a host where the operator had pinned Pi with
    // `TEAM_LAUNCHER_PI` — the variable chiefd's own preflight clears the host
    // on — the CEO pane still died at creation, tmux reaped the empty window,
    // and the actuator blamed window dimensions once a second forever.
    // `chiefd` resolves this absolutely and passes it; a daemon that cannot be
    // told must refuse rather than guess on behalf of a pane it cannot see.
    let pi_binary = pi_binary.ok_or_else(|| {
        format!(
            "a pi binary is required (--pi-binary or {PI_BINARY_ENV}): it is what every person's \
             pane execs, and a daemon that guesses it publishes the guess\n{}",
            usage()
        )
    })?;
    if !pi_binary.is_absolute() {
        return Err(format!(
            "the pi binary must be an absolute path, got '{}': a pane is launched by the client, \
             in an environment this process cannot see, so a bare name is a different lookup with \
             a different answer\n{}",
            pi_binary.display(),
            usage()
        ));
    }
    // Precedence, most explicit first: `--launcher-root`, `$ORG_LAUNCHER_ROOT`
    // (both already folded into `launcher_root`), then the `resources/`
    // directory installed beside this very binary. NO FOURTH TIER — see the
    // tombstone above `usage()` for the two incidents the fourth tier caused.
    let launcher_root = launcher_root
        .or_else(host_primitives::install::resource_root_from_exe)
        .ok_or_else(|| {
            format!(
                "chiefd cannot resolve its resource root: this binary has no `resources/` \
                 directory installed beside it, and neither --launcher-root nor \
                 {LAUNCHER_ROOT_ENV} was given. Install with the chief installer or \
                 `bun run release`, or pass --launcher-root <checkout> when running a binary \
                 straight out of a build directory.\n{}",
                usage()
            )
        })?;
    if once && serve_only {
        return Err(format!("--once and --serve-only cannot be combined\n{}", usage()));
    }
    Ok(Config {
        dir,
        runtime_socket,
        runtime_socket_demanded,
        pi_binary,
        launcher_root,
        once,
        serve_only,
    })
}

/// The single-flight floor between reconcile-cycle starts. Well under the
/// SupervisionReconcile cadence so a legitimate per-tick cycle is never skipped,
/// but non-zero so a burst (e.g. a self-audit catch-up) cannot double-fire.
/// The level one runtime-actuation pass is logged at.
///
/// # What this file lost, and how
///
/// A live company was given an explicit operator stand-down. The CEO obeyed it
/// exactly: it stopped and parked six people, removed a reminder, reported
/// `Stood down 6 people`, and then refused two inbound messages on principle.
/// Forty-five seconds later all six were back up with fresh panes, fresh
/// processes and brand-new contexts. For that whole window `daemon.log`
/// contained **nothing but `supervision cycle committed`**: there was no record
/// of who had been launched, or why.
///
/// The record existed. `ReconcileReport::notes` carried `mail wake granted
/// launch intent: <names>` — the line naming exactly what had happened — and
/// this function's caller wrote it at DEBUG, because the only question it asked
/// was [`ReconcileReport::changed`].
///
/// # Why `changed` alone is the wrong question
///
/// `changed` is an audit-identity question: does this pass's action-intent body
/// differ from the last committed one? That body is derived from the desired
/// SET plus two safety flags, so a pass can grant launch intent to somebody the
/// set already named, withdraw intent the projection had already settled, or
/// refuse a demand it refused last pass too — and leave the body identical.
/// Each of those is a decision about whether a person runs. None of them
/// changes `changed`.
///
/// So the pass is news when it recorded something new **or** when it made a
/// launch decision, and only then. #367's rule is otherwise untouched, and it
/// is worth restating because it is what keeps this file readable: a steady
/// company being desired-up is the ordinary state, it is true on every pass,
/// and it is never news. That pass still logs at DEBUG.
///
/// This is deliberately NOT the fix for `docstore.request`. "A routine request
/// succeeded quickly" is a different line with a different rule, it stays at
/// DEBUG, and the 653k-line log that motivated demoting it was a real disk
/// problem. See `chiefd_api::docstore::request_log_level`.
pub const fn actuation_pass_log_level(changed: bool, actuation_record: bool) -> tracing::Level {
    if changed || actuation_record {
        tracing::Level::INFO
    } else {
        tracing::Level::DEBUG
    }
}

const RECONCILE_FLOOR: Duration = Duration::from_secs(5);

/// Wake one coalesced reactive reconcile only after the actuator can legally
/// start its next pass. This covers both a bounded explicit-start backlog and a
/// durable change whose immediate wake landed inside the single-flight floor.
/// The delay is progress recovery, not idle polling.
///
/// `Notify` coalesces waiter permits, while `armed` coalesces timers. The cycle
/// re-observes the live runtime before it acts; a converged or shadow-only pass
/// schedules none.
fn schedule_reconcile_floor_retry(
    trigger: Option<Arc<Notify>>,
    armed: Arc<AtomicBool>,
    should_retry: bool,
    delay: Duration,
    clock: SharedClock,
) {
    if !should_retry {
        return;
    }
    let Some(trigger) = trigger else {
        return;
    };
    if armed.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async move {
        clock.sleep(delay).await;
        armed.store(false, Ordering::Release);
        trigger.notify_one();
    });
}

/// Production safety-net cadence for reactive duties: sixty seconds.
///
/// This is a BACKSTOP, and it is deliberately not deleted. Desired state has an
/// event source, because a human authors it -- every committed mutation wakes
/// this duty. OBSERVED state has none: a pane dies, the box reboots, tmux is
/// briefly unreadable, and no event anywhere describes any of it. The periodic
/// pass is level-triggered -- it re-derives from disk and converges whatever it
/// finds, with or without a signal -- so it is the only thing that repairs a
/// world the event stream never described. Watch for speed, resync for truth.
///
/// It was three minutes, chosen when the reactive path had gaps and a shorter
/// floor read as compensating for them. It is sixty seconds now for one
/// reason: three minutes is exactly long enough for a missed wake or a dead
/// pane to be invisible for MINUTES, and a safety net nobody notices failing is
/// not a safety net. The cost is bounded and known -- a converged company's
/// pass plans nothing, logs at debug and writes only its own watermark -- and
/// sixty seconds is already the cadence the E2E runner has been proving cheap.
const DEFAULT_REACTIVE_FALLBACK_FLOOR: Duration = Duration::from_secs(60);

/// The shortest floor an operator may configure. Equal to the default: the
/// environment seam can lengthen the safety net for a constrained host, never
/// shorten it into a poll that would displace the reactive path as the primary
/// scheduler.
const MIN_REACTIVE_FALLBACK_FLOOR: Duration = Duration::from_secs(60);
const REACTIVE_FALLBACK_FLOOR_ENV: &str = "CHIEFD_REACTIVE_FALLBACK_FLOOR_MS";

/// Resolve the fallback floor once per sleep decision. The environment is an
/// explicit test/operations seam, not a wake mechanism; invalid or sub-minute
/// values fail closed to the production default.
fn reactive_fallback_floor_from(raw: Option<&str>) -> Duration {
    let parsed = raw
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|millis| *millis >= MIN_REACTIVE_FALLBACK_FLOOR.as_millis() as u64);
    parsed.map(Duration::from_millis).unwrap_or(DEFAULT_REACTIVE_FALLBACK_FLOOR)
}

fn reactive_fallback_floor() -> Duration {
    reactive_fallback_floor_from(std::env::var(REACTIVE_FALLBACK_FLOOR_ENV).ok().as_deref())
}
/// The shortest sleep an alarm-clock duty will ever take (od:idle-cpu #437).
///
/// An overdue deadline must be evaluated PROMPTLY — this is the bound on how
/// prompt: one second, not zero. Zero was the live spin (~16 sweeps/second,
/// each a full ~45 ms durable commit); one second is imperceptible for a
/// supervision deadline and is the floor the geometric backoff doubles from
/// when the same overdue deadline survives its own sweep.
const DEADLINE_EVALUATION_MIN_INTERVAL: Duration = Duration::from_secs(1);
/// The production host executor: a real `/proc`.
fn real_host() -> Arc<dyn HostExecutor> {
    Arc::new(RealHostExecutor::production())
}

/// Build the real `chiefd run` host hooks from real dependencies — the
/// replacement for the former all-inert scaffold hook set. What is REAL vs
/// deliberately still inert, and precisely why, is the honest state of the port:
///
/// * **actuator** — the real [`ConvergeActuator`] over the SAME shared writer
///   actor (`Arc<CompanyDb>`) the daemon and every hook use, plus the real host.
///   Each cycle first re-projects the activity ledger through
///   `activity::reconcile` under the launch-intent fence read from the shared
///   company `org.sqlite` (when wired — see the `cycle_input` bullet), so the
///   desired topology reflects who is actually authorized to run rather than a
///   frozen bootstrap snapshot; without the facts store the projection is
///   skipped, never fabricated. It then actuates the live runtime directly (the
///   daemon runs it in Apply and `run_company`
///   sets the durable config to apply at boot). The safety limits are genuine, not
///   staged-rollout ceremony: `reconcile_cycle`'s destructive-action budget
///   refuses an oversized plan and its 3-strike circuit breaker drops a
///   repeatedly-failing company back to shadow.
/// * **delivery** — the real [`MailboxDeliverySink`]. Its writer phase durably
///   stages mailbox rows (real, safe, pure DB writes). Its host wake seam is the
///   Tier-2 [`ReconcileWaker`] (`::with_notify`): waking is a targeted
///   reconcile, so it nudges the `SupervisionReconcile` duty's `drive` loop
///   (real actuator) through a shared `Arc<Notify>` this function also returns —
///   the caller wires it onto the daemon
///   ([`Daemon::with_reconcile_trigger`]) so a wake runs the very next pass
///   immediately instead of waiting out the interval. Envelope mail and a
///   nudge is delivered because the reconcile actuates the committed
///   state; a dropped/never-parked nudge still self-heals within
///   one interval (Tier-1 behavior), so the accelerator is pure latency, never
///   correctness.
/// * **cycle_input / health — real, always wired (E10-S2, #763).**
///   [`chiefd_host::gather::HostCycleInputGatherer`] and
///   [`HostHealthSnapshotGatherer`](chiefd_host::gather::HostHealthSnapshotGatherer)
///   read chiefd's OWN committed ledger (activity/supervision) for the
///   facts chiefd itself now writes natively, and the SAME per-company
///   database `company_dir::open` resolved above for the handful of
///   facts that have no chiefd-native store yet: runtime-ownership
///   (`Owned`/`Foreign`), the CEO-boot-lease suppression gate, the
///   supervisor-liveness sample, and the runtime-projection document. This is
///   the identical file `docstore::Config::from_env_with_db_path` binds the
///   `org_documents` surface from below — deliberately: one resolved path
///   configures both.
///
///   Before E10-S2 this was opt-in on an environment-supplied shared-store
///   path, with an INERT fallback
///   (`cycle_input` reporting the
///   company `Foreign`, `health`'s store-read facts `None`). The resolved
///   path is now always present, so that degraded branch is gone — there is
///   no boot of `chiefd run` that leaves these gatherers inert.
fn production_hooks(
    company: &Arc<CompanyDb>,
    _host: Arc<dyn HostExecutor>,
    config: &Config,
    company_key: &str,
    db_path: &str,
    attendance: ActuatorAttendance,
) -> (Hooks, Arc<Notify>, std::sync::Arc<std::sync::OnceLock<()>>) {
    // Built before the actuator so the converge cycle can source its
    // activity-fence projection from the same shared facts store the
    // cycle_input/health gatherers use. Always wired as of E10-S2 (#763):
    // the resolved per-company path always exists, so there is no
    // "opt-in/INERT" branch left — the facts store reads the SAME file
    // `company_dir::open` above just opened.
    tracing::info!(
        company = %company_key,
        db = %db_path,
        "chiefd run: cycle_input/health/reconcile facts reader wired (per-company database)"
    );
    // The second argument is `_data_root` and is already discarded by the
    // constructor — it was the digest half of the composite key, and nothing
    // has read it since the store began taking a resolved path. Passed empty
    // rather than fed a directory that would only look load-bearing; the
    // parameter itself belongs in `chiefd-host`'s deletion list.
    let facts_store =
        Some(chiefd_host::gather::ReconcilerFactsStore::new(PathBuf::from(db_path), String::new()));

    let surface_bound: std::sync::Arc<std::sync::OnceLock<()>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    // #739 P3's positive-evidence registry is gone with the observation it
    // remembered (#751/P8-P10): it accumulated "this person was once seen
    // alive" across passes, and chiefd sees nobody. The operator client owns
    // it, next to the `observe()` whose answers it accumulates.
    let api_host_profile_config =
        api_host_launch_profile_config(config, std::sync::Arc::clone(&surface_bound));
    let actuator = ConvergeActuator::new(
        Arc::clone(company),
        ActuatorConfig {
            socket: config.runtime_socket.clone(),
            watching_since: watching_since(),
            dir: api_host_profile_config.dir.clone(),
            home: api_host_profile_config.home.clone(),
            pi_binary: config.pi_binary.clone(),
            floor: RECONCILE_FLOOR,
            launcher_root: api_host_profile_config.launcher_root.clone(),
            root_pi_agent_dir: api_host_profile_config.root_pi_agent_dir.clone(),
        },
    )
    .with_launch_intent_store(facts_store.clone());

    // Real durable mail staging; the wake is the Tier-2 `ReconcileWaker`, sharing
    // ONE `Notify` with the `SupervisionReconcile` duty's drive loop (wired by the
    // caller via `Daemon::with_reconcile_trigger`) — a mailbox wake
    // fence nudges that loop to run its very next pass immediately rather than
    // waiting out the interval. This is the the real runtime wake: durable staging is
    // already correctness-complete (a dropped nudge just self-heals next interval,
    // Tier-1 behavior), so the accelerator only removes latency, never risk.
    let reconcile_trigger = Arc::new(Notify::new());
    let delivery = MailboxDeliverySink::new(
        Arc::clone(company),
        Arc::new(ReconcileWaker::with_notify(Arc::clone(&reconcile_trigger))),
    );

    // Neither gatherer holds the company writer any more. Both used it for one
    // thing -- reading the actuator's committed observation -- and chiefd
    // receives no observation to read.
    let cycle_input = chiefd_host::gather::HostCycleInputGatherer::new(
        facts_store.clone(),
        config.runtime_socket.clone(),
        company_key,
    );
    let health = chiefd_host::gather::HostHealthSnapshotGatherer::new(
        config.runtime_socket.clone(),
        facts_store,
        config.dir.clone(),
        attendance,
    );

    // There is no boot-time skill-extractor check any more. It verified that a
    // TypeScript file existed at a checkout-relative path, because the
    // extractor was a `bun run` subprocess and a moved checkout silently
    // disabled it. The pipeline is compiled into this binary now, so its
    // presence is not a runtime fact anything can check or get wrong.

    tracing::warn!(
        company = %company_key,
        "chiefd run host hooks: actuator=REAL (live, budget+breaker-limited), delivery=REAL \
         (reconcile-nudge wake, Tier-2 notify-accelerated), cycle_input=REAL, \
         health=REAL (per-company facts wired)"
    );

    (
        Hooks {
            cycle_input: Arc::new(cycle_input),
            actuator: Arc::new(actuator),
            health: Arc::new(health),
            delivery: Arc::new(delivery),
        },
        reconcile_trigger,
        surface_bound,
    )
}

/// Resolve the launch-profile configuration once from the daemon's explicit
/// process configuration. Both the runtime actuator and the API-host projection
/// consume this helper so neither path can independently derive home, registry,
/// launcher-root, or the surface-bound latch.
fn api_host_launch_profile_config(
    config: &Config,
    surface_bound: std::sync::Arc<std::sync::OnceLock<()>>,
) -> ApiHostLaunchProfileConfig {
    ApiHostLaunchProfileConfig {
        // The company DIRECTORY itself. `chiefd-host` derives `<dir>/.chief`
        // from it and composes person homes beneath that, with NO `<slug>`
        // segment — one directory holds one company. Handing over the `.chief`
        // root instead is what put a pane's `ORG_LAUNCHER_ORG_DIR` one level
        // too deep, because that same field was also the pane env stamp.
        dir: config.dir.clone(),
        // Deliberate last resort, audited and left: reachable only when HOME is
        // unset or relative. It must remain identical for a pane and an API
        // RPC child; a service manager cannot silently give the two hosts
        // different registry roots.
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| PathBuf::from("/root")),
        root_pi_agent_dir: root_pi_agent_dir(),
        launcher_root: config.launcher_root.clone(),
        surface_bound,
    }
}

// TOMBSTONE: `actuator_ramp_from_environment`. It parsed the operator's
// configured admission ramp so `chiefd run`'s converge pass and the actions
// route could stagger by the same rule. Both the ramp and that route are
// deleted by operator ruling -- the actuator boots every missing pane in one
// pass -- so there is no stagger left to agree about.

/// When THIS daemon process started watching, ISO-8601.
///
/// Pinned once, at process entry, and handed to every [`ActuatorConfig`] this
/// process builds. It reaches exactly one decision:
/// `ReconcileInput::watching_since`, which clamps the inferred quiet instant so
/// a chiefd restart longer than `AGENT_ACTIVITY_LIVENESS_MS` does not settle
/// every person who was mid-turn when this process's predecessor stopped. A
/// heartbeat can only be MISSING relative to somebody listening for it, and
/// nobody was listening before this instant.
///
/// A `OnceLock` rather than a value threaded from `main`, and it holds a
/// PROCESS fact: there is one daemon process, its start instant is the same for
/// every company and every pass, and two callers computing it separately would
/// be two answers to a question with one. [`run`] touches it before anything
/// else so the recorded instant is process entry rather than whenever the first
/// company happened to be wired.
fn watching_since() -> String {
    static WATCHING_SINCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    WATCHING_SINCE
        .get_or_init(|| {
            chiefd_core::isotime::iso_millis(
                i64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(i64::MAX),
            )
        })
        .clone()
}

pub fn run(args: impl Iterator<Item = String>) -> ExitCode {
    // FIRST, before any wiring: the watch instant must be process entry, not
    // the moment some later step happened to ask for it.
    let _ = watching_since();
    // #131/#28: opt-in child-side parent-death watchdog, gated on
    // CHIEFD_STORE_EXIT_WITH_PARENT=1 (the SAME env `docstore-only` uses). A
    // `chiefd run` daemon spawned `detached: true` by the e2e test harness
    // would otherwise be reaped ONLY by a parent-side sweeper, which does not
    // survive the runner being SIGKILLed (the 771-orphan class). Arming
    // PR_SET_PDEATHSIG(SIGTERM) here makes the daemon self-terminate the instant
    // its spawner dies — `wait_for_signal` already treats SIGTERM as a graceful
    // shutdown. Production launches chiefd via `setsid` WITHOUT this env set, so
    // the watchdog stays inert there (a setsid child is already reparented to
    // pid 1 and must NOT self-kill); only a caller that explicitly opts in is
    // affected.
    crate::watchdogs::install_parent_death_watchdog();
    // U16b: the e2e harness's `startChiefdWorld` double-forks this daemon away
    // from the bun test process (so Bun's own dangling-process reaper, which
    // keys off direct-child ancestry, cannot see it) and, for that spawn
    // shape, sets CHIEFD_STORE_WATCH_PID instead of the env above -- PDEATHSIG
    // targets whichever process is this daemon's OS parent AT SPAWN TIME,
    // which for a double-fork is a wrapper shell gone within milliseconds, so
    // arming it here would self-kill the daemon before it ever bound its
    // port. See watchdogs.rs’s own doc comment on WATCH_PID_ENV for the
    // full reasoning; this call must accompany install_parent_death_watchdog
    // wherever a `chiefd` binary boots, not just in docstore-only mode.
    crate::watchdogs::install_watch_pid_watchdog();

    let config = match parse_config(args) {
        Ok(config) => config,
        Err(message) => {
            tracing::error!("{message}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "cannot start the tokio runtime");
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(run_company(config));
    // #370: `serve()` drains cooperatively and within its own budget, but a
    // `spawn_blocking` task cannot be force-cancelled, and a plain `Runtime`
    // drop BLOCKS on outstanding blocking tasks — which would hold process
    // exit for tens of seconds past the drain, the exact >10-30 s tail
    // #346/#370 measured. `shutdown_timeout` caps that wait: committed data is
    // already durable (WAL), so abandoning a leaked blocking thread is
    // crash-safe by construction.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_GRACE);
    code
}

/// ONE store, not two. The company directory's own store
/// (`company_dir::store_db_path`) names BOTH the duty
/// scheduler's own `CompanyDb` and the docstore surface's `org_documents`
/// tables — the SAME physical file, opened by two independent connections
/// (SQLite's WAL mode is built for exactly this). Before this story, an
/// operator-set shared-store path and the per-company default could
/// disagree, and a pre-existing company (one that ran under the TypeScript
/// supervisor its whole life, like tribes-capital) had its `chief.db`
/// bootstrapped once and then silently diverged forever from every write the
/// launcher/CEO/CLI made through the docstore (`ORG_DURABLE_TRANSPORT=chiefd`)
/// — found live, 2026-07-21: a human-delivered goal landed in the shared file
/// and was durably recorded, but the duty scheduler could never see it
/// (confirmed absent from `chief.db`), so nothing woke the CEO to act on it.
/// That was two stores with no sync, which the standing SQL-only/no-dual-reader
/// rule forbids — not something to patch with a bridge; the fix is to stop
/// having two files. E10-S2 finishes that fix: there is no longer an
/// environment variable that could redirect either connection away from the
/// resolved per-company file, for any company, ever.
/// Resolve the runtime socket this daemon will observe and actuate against
/// (#63/#64).
///
/// Live evidence, cobalt 2026-07-24: chiefd was started with neither
/// `--runtime-socket` nor `ORG_LAUNCHER_RUNTIME_SOCKET`, so it fell back to the slug
/// (`tribes-capital`) while the company's own durable `runtime-owner` claim
/// named socket `default`. Two things followed, both silent:
///
/// * `gather::cycle_input` compares that claim against OUR socket, so every
///   supervision pass observed [`IdentityObservation::Foreign`] and took the
///   early return — 4911 consecutive passes that wrote nothing while logging a
///   healthy "committed".
/// * The converge actuator, pointed at a runtime server the company does not live
///   on, created its own `org-tribes-capital` session there and booted a SECOND
///   full fleet into it — 16 panes shadowing the operator's 17.
///
/// A daemon must not invent placement. The claim is the company's own record of
/// where it runs, so it is the authority whenever it exists:
///
/// * **Demanded and agreeing, or demanded with nobody claiming** — use it.
/// * **Demanded but contradicting a LIVE claim** — refuse to start. Acting would
///   mean converging a company onto a server it does not run on; the operator
///   gets a legible error instead of a daemon that is silently foreign forever.
/// * **Not demanded, with a live claim** — adopt the claim's socket.
/// * **Not demanded, nobody claiming** — the client's PREFERENCE, which is
///   `ORG_LAUNCHER_RUNTIME_SOCKET` or, absent that, the company key
///   `parse_config` already resolved. Nothing is running, so there is nothing
///   to contradict, and a never-launched company (or a test harness) still
///   starts.
///
/// # A GUESS IS NOT A DEMAND, and treating it as one bricked every upgrade
///
/// `cb63690a0` moved `boot_socket`'s last tier off the shared string
/// `"default"` and onto the company's own key. Every company created before it
/// therefore holds a live claim naming `default` while its client now boots on
/// the company key — and because `chief` passed that key as an explicit socket,
/// the first branch fired and the daemon refused. The operator saw
/// `chiefd ... did not become healthy within 15s` and the real reason only in
/// `daemon.log`.
///
/// The refusal was right and is unchanged; the INPUT was wrong. `chief` cannot
/// read a company's claim before a daemon serves it, so the socket it passes at
/// spawn is a guess. It now arrives as a preference (the environment variable)
/// and only a human's `--runtime-socket` arrives as a demand, so the adoption
/// tier above — which no production boot could reach — is the ordinary path for
/// a company that already ran somewhere. The client obeys the same order:
/// `company::boot_socket` puts the recorded claim above its own fallback too.
///
/// Adoption alone would leave a pre-`cb63690a0` company on the shared server for
/// ever. Moving it off is the CLIENT's job and needs a proof no daemon can make
/// — see `chief-cli`'s `company::claim_move`.
fn resolve_runtime_socket(
    demanded: Option<&str>,
    owner_socket: Option<&str>,
    preferred: &str,
    company_key: &str,
) -> Result<(String, &'static str), String> {
    match (demanded, owner_socket) {
        (Some(demanded), Some(owner)) if demanded != owner => Err(format!(
            "refusing to run company '{company_key}' on runtime socket '{demanded}': its live \
             runtime-ownership claim names socket '{owner}'. Actuating here would converge a \
             second, shadow fleet onto a server the company does not run on. Either start with \
             --runtime-socket {owner} (or drop the flag and let the claim decide), or end the \
             claim: run `chief stop` in this company's directory, which releases it, and start \
             again."
        )),
        (Some(demanded), _) => Ok((demanded.to_string(), "demanded")),
        (None, Some(owner)) => Ok((owner.to_string(), "adopted-from-runtime-owner")),
        (None, None) => Ok((preferred.to_string(), "client-preference")),
    }
}

/// #376: install `company`'s change-feed publish hook, closing over `feed`.
///
/// This is the one-line adapter `CompanyDb::set_change_feed_sink`'s doc
/// comment names: `chiefd-core` cannot depend on `chiefd-api` (layering —
/// `chiefd-api` already depends on `chiefd-core`), so its `ChangeFeedSink`
/// hook is a plain `Fn`, not a `ChangeFeed` reference. Only this binary
/// crate depends on both, so only it can close the loop.
/// The sink hands over the caller-supplied ISO-8601 `updated_at` string
/// directly — the same shape `ChangeFeed::publish` and `DocStore`'s own write
/// methods use (`run_job` renders its `WallMillis` via `to_iso8601` before it
/// reaches the sink; the row-write hint path already holds the ISO stamp).
///
/// #372: this USED TO ALSO mirror every commit's content into `org_documents`
/// (a second, duplicate copy of chiefd's own native ledger) — retired the
/// same day it shipped. The mirror wrote `SupervisionLedger`'s own residual
/// `Serialize` output verbatim; that type deliberately `#[serde(skip)]`s
/// `assignments`/`assignmentOrder`/`effects`/`effectOrder`/
/// `nextEffectSequence` (they live in relational tables, never duplicated
/// into the committed JSON body) — so the moment the mirror actually ran,
/// it overwrote a stale-but-structurally-valid `org_documents` row with an
/// INCOMPLETE one missing exactly those fields, which is what
/// `org-supervision-state.ts`'s effect-sequence validator throws
/// "Supervision effect sequence is invalid" on. Fixed properly, not
/// papered over: `chiefd-api::docstore::router`'s `/v1/docs/read` handler
/// now special-cases `store == "supervision"` for this process's own
/// company and reads chiefd's LIVE ledger directly (see
/// `docstore::bind_with_feed_and_company`/`SupervisionLiveSource` — this
/// process's `run_company` wires it in below) — no second copy to ever go
/// stale or incomplete again.
pub(crate) fn wire_change_feed(
    company: &CompanyDb,
    feed: Arc<docstore::ChangeFeed>,
    company_key: String,
) {
    company.set_change_feed_sink(Arc::new(
        move |_company_label: &str, store: &str, _body: &str, updated_at: &str, removed: bool| {
            // Published under the COMPANY KEY, which is what every mounted
            // `/v1/docs/watch` client filters on. The label and the filter used
            // to be two different strings — a bare slug and the composite
            // `<slug>@<data-root-digest>` — so a normalized-row event was
            // stranded behind an exact filter even though its commit had
            // succeeded. One key, one filter, no gap.
            feed.publish(company_key.clone(), store.to_string(), updated_at.to_string(), removed);
        },
    ));
}

/// The boot actuation write: a NEVER-CONFIGURED company adopts apply, with the
/// #29 pointer-sweep live too. A company an operator has configured keeps the
/// mode the operator chose.
///
/// The operator's stored `budget_override` is PRESERVED, not forced off:
/// hardcoding `budget_override=false` here is what made the durable override
/// useless as an operator lever — a `budgetOverride: true` seeded externally
/// was reverted by the very next boot before any cycle could read it (found
/// live, 2026-07-22 — runtime/takeover-bug-log.md BUG-1). A fresh,
/// never-configured company still reads the default `false`, so the budget
/// stays a real safety limit until an operator deliberately overrides it
/// (`chiefd set-actuation-config --budget-override on`).
///
/// `actuation_mode` HAD EXACTLY THE SAME BUG, one field over, and it was the
/// worse one (#751/#13). Hardcoding `Apply` here meant
/// `chiefd set-actuation-config --company <slug> --mode shadow` wrote a durable
/// row that the very next `chiefd run` overwrote — so a company could never
/// actually be put in shadow, and therefore could never be API-hosted. The
/// refusal an operator got back (`company-not-api-hosted`) told them to set
/// shadow AND restart the daemon; the restart is what destroyed the setting, so
/// following the instructions exactly could not work, and the whole browser
/// seam sat unreachable behind advice that defeated itself. A boot has no
/// opinion about a mode an operator has already expressed.
///
/// A company with no row at all still adopts apply, so a fresh company actuates
/// its runtime fleet as before, and the write still clears the breaker on boot (a
/// reboot is a fresh start — the module comment above blesses that).
async fn enable_live_actuation(company: &CompanyDb) -> Result<(), ChiefdError> {
    let (configured, stored) = company.read(|snapshot| {
        (
            chiefd_core::store::converge_safety::configured(snapshot),
            chiefd_core::store::converge_safety::read(snapshot).into_parts().0,
        )
    });
    let mode = if configured { stored.actuation_mode } else { safety::ActuationMode::Apply };
    safety::set_actuation_config(company, mode, true, stored.budget_override).await
}

/// Serve one native CompanyDb snapshot without becoming its supervisor.
///
/// A stale-reader proof must be able to point a real Pi at an older ChiefD
/// database while another daemon continues to own the current runtime fleet. A
/// normal `run` process must refuse that foreign placement and, when it owns
/// the same placement, must reconcile it; either behavior is correct for a
/// supervisor but makes a read-only topology test dishonest. This narrow mode
/// mounts the exact typed HTTP routes (including live normalized supervision
/// reconstruction) and waits for shutdown, while deliberately doing none of
/// the mutable supervisor work: no runtime-owner resolution, seed, duty, host
/// hook, or actuation configuration write.
///
/// This mode is AUTHENTICATED, on exactly the terms the live surface is (A7).
/// It used to refuse to start whenever the universal gate was on, and the
/// deleted comment gave the whole reason: it "refuses this mode rather than
/// accidentally making a second unauthenticated surface available". That
/// premise is gone. Unlike `chiefd docstore-only`, which has no company actor
/// at mount and therefore nothing to authenticate against, this mode HOLDS the
/// company — so it builds the same auth runtime `run_company` builds, from the
/// same `<dir>/.chief/keys`, through the same helper. A refusal would now only
/// mean that the one harness able to exercise the real route surface could
/// never exercise it authenticated. The isolated E2E binds an ephemeral
/// loopback port through the existing harness.
async fn serve_only_snapshot(
    company: Arc<CompanyDb>,
    config: &Config,
    clock: &SharedClock,
    company_key: String,
    db_path: String,
) -> ExitCode {
    // Before anything is served, and refusing rather than degrading: a company
    // whose operator and actuator cannot prove who they are has no control
    // plane, and that is as true of a snapshot reader as of a supervisor.
    let clock_for_auth = Arc::clone(clock);
    let auth_runtime = match ensure_daemon_auth_runtime(
        Arc::clone(&company),
        &config.dir,
        &company_key,
        Arc::new(move || clock_for_auth.wall().0),
    )
    .await
    {
        Ok(runtime) => runtime,
        Err(()) => {
            company.shutdown();
            return ExitCode::FAILURE;
        }
    };

    let change_feed = Arc::new(docstore::ChangeFeed::new());
    wire_change_feed(&company, Arc::clone(&change_feed), company_key.clone());
    // The org directory, and NOT the runtime host: this mount deliberately has
    // no actuator, and person-identity provisioning deliberately does not need
    // one. Until it was wired here a person bearer was impossible by
    // construction on `--serve-only` — the company, its people and their rows
    // all existed, and `/v1/auth/challenge` answered 401 for every one of them,
    // because the only path to an enrolled person ran through a materialization
    // this mount cannot perform.
    reconcile_company_skill_library(&company, config).await;
    let supervision_live =
        docstore::SupervisionLiveSource::new(company.clone(), company_key.clone())
            .with_agent_home_root(agent_home_root(config));
    // ONE store, not two (E10-S2): the snapshot reader mounts on the SAME
    // per-company file `company` above just opened, never a value read from
    // the environment. `from_env_with_db_path` is infallible — there is no
    // "requires the typed docstore surface" refusal branch left, because the
    // path is always resolvable now.
    let store_config =
        docstore::Config::from_env_with_db_path(|key| std::env::var(key).ok(), db_path);
    let bound = match docstore::bind_with_feed_and_company(
        &store_config,
        change_feed,
        Some(supervision_live),
    )
    .await
    {
        Ok(bound) => bound
            .with_auth(Some(auth_runtime))
            .with_runtime_identity("snapshot-reader", Some(company_key.clone())),
        Err(error) => {
            tracing::error!(company = %company_key, %error, "chiefd run --serve-only could not bind the snapshot reader");
            company.shutdown();
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = bound.ensure_schema().await {
        tracing::error!(company = %company_key, %error, "chiefd run --serve-only could not prove the typed docstore schema");
        company.shutdown();
        return ExitCode::FAILURE;
    }
    tracing::info!(
        company = %company_key,
        dir = %config.dir.display(),
        db = %store_config.db_path,
        "chiefd run --serve-only: native snapshot reader mounted; duties and runtime actuation are disabled"
    );
    let served = docstore::serve_bound_with_watch(bound, wait_for_signal(), None).await;
    company.shutdown();
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(company = %company_key, %error, "chiefd run --serve-only snapshot reader stopped with an error");
            ExitCode::FAILURE
        }
    }
}

/// Mint the two daemon-scoped credential files if they are absent, then build
/// the company's auth runtime from them. ONE credential bootstrap for the whole
/// daemon (A7).
///
/// Both mounts call this: the supervisor (`run_company`) and the snapshot
/// reader (`serve_only_snapshot`). They used to be one implementation and one
/// refusal-to-exist — the reader simply declined to start whenever the gate was
/// on — and the moment the reader had to authenticate too, the choice was one
/// helper or two copies of a security bootstrap. Two copies of a trust-anchor
/// path is how the two halves of one rule come to disagree about one directory.
///
/// The keys live at `<dir>/.chief/keys` — inside the company, like everything
/// else durable about it. There is no root to resolve and nothing to get one
/// directory wrong by, which is what the `--data-root`-means-the-orgs-root
/// collision cost a full day on #13.
///
/// `Err(())` means the caller must REFUSE TO SERVE. Everything worth saying is
/// already logged here, naming the file and the principal; the caller adds only
/// whatever teardown its own mount owes (a beacond deregistration, a company
/// shutdown), which is exactly the part the two mounts do not share.
async fn ensure_daemon_auth_runtime(
    company: Arc<CompanyDb>,
    dir: &std::path::Path,
    company_key: &str,
    clock: chiefd_api::authn::runtime::Clock,
) -> Result<Arc<chiefd_api::authn::runtime::AuthRuntime>, ()> {
    // `<dir>/.chief/keys`, DERIVED — never an env var, which is what
    // `CHIEFD_OPERATOR_KEY_PATH` was and why the whole fleet ran with no
    // operator identity.
    let keys_dir = crate::company_dir::keys_dir(dir);
    // Create-once, 0600, preserved thereafter. chiefd-api READS these files
    // and never writes one — filesystem effects are chiefd_host's.
    //
    // TWO daemon-scoped principals, not one. The operator is deliberate
    // action; `service` is the resident actuator, whose four HTTP calls are
    // all reads and all fail closed once the universal gate is on. Keeping
    // them apart is what lets an audit record say which of the two acted.
    let mint = chiefd_host::identity_key::SystemIdentityKeyMint::new();
    for (principal, path) in [
        ("operator", identity_keys::operator_key_path(&keys_dir)),
        ("service", identity_keys::service_key_path(&keys_dir)),
    ] {
        if let Err(error) = chiefd_host::identity_key::ensure_identity_key(&path, &mint) {
            tracing::error!(
                company = %company_key,
                principal,
                key = %path.display(),
                %error,
                "chiefd run: a daemon identity key could not be created; refusing to serve"
            );
            return Err(());
        }
    }
    match chiefd_api::authn::boot::build_auth_runtime(company, &keys_dir, clock).await {
        Ok(runtime) => Ok(runtime),
        Err(error) => {
            // The trust root could not initialize. There is no degraded mode
            // left to fall back to.
            tracing::error!(
                company = %company_key,
                %error,
                "chiefd run: the agent-auth runtime could not be initialized; refusing to \
                 serve a company whose agents would have no identity"
            );
            Err(())
        }
    }
}

/// The directories used by the one-time agent-home and genesis-skill writes.
///
/// Both are facts this process holds from argv and its own environment onward,
/// and NEITHER is a capability: `chiefd run --serve-only` mounts no actuator
/// and still wires this, because a person hired there is still a person whose
/// home and credential must not wait for a convergence pass that mount will
/// never run.
fn agent_home_root(config: &Config) -> chiefd_api::docstore::AgentHomeRoot {
    chiefd_api::docstore::AgentHomeRoot {
        dir: config.dir.clone(),
        shipped_skills_root: config.launcher_root.join("packages/piing/skills"),
    }
}

/// Bring this company's people to the skills this release ships, once per
/// daemon boot.
///
/// **This is the call that reaches a company nobody is changing.** The same
/// reconcile runs inside `ensure_agent_homes`, which covers every hire and every
/// explicit launch — and neither of those happens when an operator simply
/// restarts a company that already exists. Measured twice on a live company
/// before this was added: `chief` in an existing directory boots the CEO,
/// mutates no roster and requests no launch, so every person kept the retired
/// flat skills link and every deleted skill stayed readable, silently, across
/// two upgrades.
///
/// Daemon boot is the one event every company has on every upgrade, which is
/// what makes it the right place for the guarantee. It is writeless when
/// converged, so the cost on an already-current company is a directory walk.
///
/// REPORTED and stepped over, never fatal: a company whose library is stale
/// still runs, and refusing to serve over it would be a worse answer.
async fn reconcile_company_skill_library(company: &Arc<CompanyDb>, config: &Config) {
    let shipped = config.launcher_root.join("packages/piing/skills");
    // `ensure_agent_homes` and NOT `reconcile_project_skills` alone, and the
    // difference is the whole defect this call exists for. The library reconcile
    // rewrites `<dir>/.chief/skills` and the CEO's `<dir>/.pi/skills`; the
    // PER-PERSON installs are written by `ensure_agent_homes`, and a library-only
    // call leaves every person on the retired flat symlink at `.pi/skills`.
    //
    // Measured live, and it is a trap worth naming because it LOOKS correct: with
    // the library converged, `ls` on a person's retired flat link resolves to the
    // CEO's one-entry directory, so every person — heads and workers alike —
    // listed `manager` and the tree read as if the role split had worked. It had
    // not. The check that catches it is comparing a head's install with a
    // worker's; the check that misses it is looking at either one alone.
    for warning in
        chiefd_host::runtime_lifecycle::reconcile_company_skills(company, &config.dir, &shipped)
            .await
    {
        tracing::warn!(event = "org.skills.unreconciled", %warning);
    }
    tracing::info!(
        event = "org.skills.library.reconciled",
        dir = %config.dir.display(),
        "the company skill library and every existing person's installed skill are current"
    );
}

// TOMBSTONE: `person_identity_root`. It answered `<dir>/.chief` — one segment
// too deep, and its own doc comment said so: `converge_apply` composed person
// homes one directory below what this minted, so an enrolled key and the home
// it belonged to were different paths. Stage 4 closes it by having neither side
// compose anything: `agent_home::agent_home(dir, person_id)` is the one
// derivation, and both the daemon and the enroller are handed the COMPANY
// DIRECTORY rather than a root either of them has to walk down from.

/// Undo the one successful beacond admission when startup fails before the
/// daemon reaches its normal serving lifecycle. This is cleanup, not a retry or
/// fallback: the reserved listener is dropped by its owner on the same return
/// path, and a later launcher attempt must make a fresh single admission call.
async fn deregister_after_admitted_startup_failure(beacon: &crate::beacon::Beacon, dir: &Path) {
    if let Err(error) = beacon.deregister().await {
        tracing::warn!(
            dir = %dir.display(),
            %error,
            "chiefd run: beacond_deregister_failed after admitted startup failure; the location will read stale until the next register"
        );
    }
}

#[tracing::instrument(name = "daemon.boot", skip_all, fields(dir = %config.dir.display()))]
async fn run_company(mut config: Config) -> ExitCode {
    // THE OTHER HALF OF THE MISSING 4½ MINUTES. Everything between this line
    // and `daemon.admitted` below happens before beacond has heard of this
    // process, which is exactly the window the operator client spends in
    // `daemon.registration.wait` with nothing to report but "not yet". Each
    // step now says when it started and how long it took, on both sides.
    let boot_started = std::time::Instant::now();
    // The one identity, derived from the one input. No slug: the company's
    // display name is a column of a row this daemon may not have received yet.
    let company_key = crate::company_dir::company_key(&config.dir);
    tracing::info!(
        event = "daemon.boot.start",
        company = %company_key,
        dir = %config.dir.display(),
        once = config.once,
        serve_only = config.serve_only,
        "the company daemon is starting"
    );
    // Single-writer admission (E10-S3, #764) is one beacond `register` call.
    // A normal persistent daemon first reserves a listener, publishes that
    // exact address to beacond, and only then opens either SQLite surface.
    // Losing admission drops the reservation and exits with no company DB,
    // docstore schema, WAL, seed, or actor write. `--serve-only` and `--once`
    // remain explicit non-admission modes: the former is a read-only snapshot
    // surface; the latter never owns a persistent listener.
    let clock: SharedClock = Arc::new(SystemClock::default());
    // PURE: naming the store creates and opens nothing, so it is safe before
    // beacond admission — and it is the path the admitted listener will mount.
    let db_path = crate::company_dir::store_db_path(&config.dir).to_string_lossy().to_string();
    if config.serve_only {
        let company = match crate::company_dir::open(&config.dir, Arc::clone(&clock)) {
            Ok(opened) => opened,
            Err(error) => {
                tracing::error!(company = %company_key, %error, "cannot open the company database");
                return ExitCode::FAILURE;
            }
        };
        let company = Arc::new(company);
        return serve_only_snapshot(company, &config, &clock, company_key, db_path).await;
    }

    let store_config =
        docstore::Config::from_env_with_db_path(|key| std::env::var(key).ok(), db_path.clone());

    // `--once` is deliberately not a persistent daemon and has never bound or
    // registered. Preserve that explicit smoke-run contract. Every normal
    // `run` holds this storage-free reservation through exactly one admission
    // call, then opens/migrates both SQLite users only after `Admitted`.
    let admission = if config.once {
        None
    } else {
        let port_walk = port_walk_from_env(|key| std::env::var(key).ok());
        let reservation = match docstore::reserve_listener_walking(&store_config, port_walk).await {
            Ok(reservation) => reservation,
            Err(error) => {
                tracing::error!(
                    company = %company_key,
                    %error,
                    "chiefd run: could not reserve any port in the walked range; refusing before company storage opens"
                );
                return ExitCode::FAILURE;
            }
        };
        let Some(bound_addr) = reservation.local_addr() else {
            tracing::error!(
                company = %company_key,
                "chiefd run: reserved listener could not report its own address; refusing before company storage opens"
            );
            return ExitCode::FAILURE;
        };
        let bound_url = format!("http://{bound_addr}");
        tracing::info!(
            event = "daemon.listener.reserved",
            url = %bound_url,
            elapsed_ms = chiefd_log::elapsed_ms(boot_started),
            "reserved a listener; asking beacond for admission"
        );
        let beacon = std::sync::Arc::new(crate::beacon::Beacon::from_env(&config.dir));
        let register_started = std::time::Instant::now();
        // ONE self-registration retry, at boot only. `UnknownCompany` used to
        // be a flat refusal here — "a daemon cannot create one by binding" —
        // and the operator repealed that rule after the registry's own store
        // was destroyed underneath a live beacond: the row was gone, nothing
        // could recreate it, and a real company was unstartable behind a
        // refusal that was correct about a rule that no longer served anyone.
        // The ruling, verbatim: "chiefd should always try to register to
        // beacond when it starts. if it exists, no-op. if it doesn't,
        // register." beacond's create is an upsert, so exists-and-unchanged
        // is the no-op half and the `created` flag names which half ran.
        //
        // THE GUARD IS PROOF OF COMPANY: `<dir>/.chief/db/chief.db` must
        // exist on disk before this daemon may claim the row. The directory
        // is the company and the database is the proof — a daemon started in
        // an empty directory still refuses, so binding alone still mints
        // nothing. A REMOVED company cannot come back through here either:
        // `chief rm` deletes `<dir>/.chief/` BEFORE the registry row
        // (`remove.rs`), so the proof is gone by the time the row is. And
        // none of this reaches MID-RUN behaviour: a heartbeat 404 still
        // means the operator deleted the company while we ran, and is still
        // never repaired by recreating state (D22/F13, `beacon.rs`).
        let mut self_registration_spent = false;
        loop {
            match beacon.register(&bound_url, bound_addr.port()).await {
                Ok(crate::beacon::Admission::Admitted) => {
                    // The line the waiting client is blocked on. It states the
                    // time this daemon took to reach it, so the two sides of the
                    // wait can be lined up from one file instead of inferred.
                    tracing::info!(
                        event = "daemon.admitted",
                        url = %bound_url,
                        pid = std::process::id(),
                        register_ms = chiefd_log::elapsed_ms(register_started),
                        boot_ms = chiefd_log::elapsed_ms(boot_started),
                        "beacond admitted this daemon as the single writer"
                    );
                    break Some((reservation, bound_addr, beacon));
                }
                Ok(crate::beacon::Admission::UnknownCompany) if !self_registration_spent => {
                    self_registration_spent = true;
                    let database = config.dir.join(".chief").join("db").join("chief.db");
                    if !database.is_file() {
                        tracing::error!(
                            company = %company_key,
                            beacond_url = %beacon.base_url(),
                            database = %database.display(),
                            "chiefd run: beacond has no company row for this directory and the directory holds no company database; refusing — a daemon in an empty directory must not mint a company by binding"
                        );
                        return ExitCode::FAILURE;
                    }
                    // The slug is a DISPLAY name with no uniqueness (see the
                    // ledger), and at this point storage is deliberately not
                    // yet open, so the directory's basename is the honest
                    // available value. It is only ever written on INSERT in
                    // practice: a row that already exists answers Admitted
                    // above and never reaches this arm.
                    let slug = config.dir.file_name().map_or_else(
                        || company_key.clone(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    match beacon.create_company(&company_key, &slug).await {
                        Ok(true) => {
                            tracing::warn!(
                                event = "beacond.company_row.restored",
                                company = %company_key,
                                dir = %config.dir.display(),
                                slug = %slug,
                                "beacond had no row for a directory that holds a company database; the row was restored from the directory's own proof. Seeing this on every boot means the registry is LOSING rows — that is a live fault, and this line is how it gets found."
                            );
                        }
                        Ok(false) => {
                            // Raced another writer between the register and
                            // the create. Harmless: the retry below decides.
                            tracing::info!(
                                company = %company_key,
                                "beacond answered created=false to boot self-registration; the row already exists"
                            );
                        }
                        Err(error) => {
                            tracing::error!(
                                company = %company_key,
                                %error,
                                "chiefd run: boot self-registration failed; refusing before company storage opens"
                            );
                            return ExitCode::FAILURE;
                        }
                    }
                    continue;
                }
                Ok(crate::beacon::Admission::UnknownCompany) => {
                    tracing::error!(
                        company = %company_key,
                        beacond_url = %beacon.base_url(),
                        "chiefd run: beacond still has no company row after boot self-registration; refusing before company storage opens"
                    );
                    return ExitCode::FAILURE;
                }
                Ok(crate::beacon::Admission::Occupied { pid, hostname, last_seen_at }) => {
                    tracing::error!(
                        company = %company_key,
                        incumbent_pid = pid,
                        incumbent_hostname = hostname.as_deref().unwrap_or("unknown"),
                        incumbent_last_seen_at = last_seen_at.as_deref().unwrap_or("unknown"),
                        "chiefd run: refusing to start a second daemon against one company before company storage opens (beacond names a live incumbent)"
                    );
                    return ExitCode::FAILURE;
                }
                Err(error) => {
                    tracing::error!(
                        company = %company_key,
                        %error,
                        "chiefd run: beacond_register_failed — cannot prove single-writer admission; refusing before company storage opens"
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
    };

    let company = match crate::company_dir::open(&config.dir, Arc::clone(&clock)) {
        Ok(opened) => opened,
        Err(error) => {
            tracing::error!(company = %company_key, %error, "cannot open the company database");
            if let Some((_, _, beacon)) = admission.as_ref() {
                deregister_after_admitted_startup_failure(beacon, &config.dir).await;
            }
            return ExitCode::FAILURE;
        }
    };
    let company = Arc::new(company);
    // The company's DISPLAY name, now that there is a store to read it from —
    // absent for a company being born, which is the ordinary state of the
    // create flow. Logged once, never held: the daemon's identity is the key
    // above, and a slug cached at boot would outlive the genesis that changed
    // it.
    //
    // The line reads `slug=<the key>` until `chiefd-core` stores the slug
    // instead of deriving it from this actor's label — see
    // `company_dir`'s header. It is logged rather than suppressed precisely so
    // that gap is visible on every boot.
    tracing::info!(
        event = "daemon.company.named",
        company = %company_key,
        dir = %config.dir.display(),
        slug = crate::company_dir::display_slug(&company).as_deref().unwrap_or("<pre-genesis>"),
        "the store is this company's only display name"
    );

    // ORDERING (#63/#64): placement is resolved before the boot-time normalized
    // ledger seeds below. The seeds are MUTATIONS —
    // they seed native ledgers into CompanyDb — and a daemon that is about to
    // refuse to run (a socket contradicting a live ownership claim) must not
    // write anything first. And resolution depends on nothing the adopts
    // produce: it reads the normalized `runtime_owner` row through
    // `ReconcilerFactsStore`, not through the ledgers being seeded.
    // #63/#64: resolve WHERE this daemon actuates before anything observes or
    // actuates. The company's own runtime-ownership claim is the authority; a
    // derived guess is only acceptable when nobody claims the company. See
    // `resolve_runtime_socket` for the live failure this prevents.
    // Always constructed as of E10-S2 (#763): the resolved per-company path
    // is always present, so the runtime-ownership claim is always
    // observable — there is no longer a "no facts store configured" path
    // that falls through to the explicit socket fallback unobserved.
    let owner_socket = {
        let store =
            chiefd_host::gather::ReconcilerFactsStore::new(PathBuf::from(&db_path), String::new());
        // Both arguments are the company key: the first IS the row key, and
        // the second is already `_organization` in the reader — the row is
        // scoped by its own key and declares no company of its own.
        match store.active_runtime_owner_socket(&company_key, &company_key) {
            Ok(socket) => socket,
            Err(error) => {
                // Fail closed: the row exists but could not be trusted, or
                // the store could not be opened. Guessing a socket here is
                // exactly how a shadow fleet gets born.
                tracing::error!(
                    company = %company_key,
                    %error,
                    "chiefd run: cannot read the runtime-ownership claim, so where this company runs \
                     is unknown; refusing to actuate rather than guessing a runtime socket"
                );
                if let Some((_, _, beacon)) = admission.as_ref() {
                    deregister_after_admitted_startup_failure(beacon, &config.dir).await;
                }
                return ExitCode::FAILURE;
            }
        }
    };
    match resolve_runtime_socket(
        config.runtime_socket_demanded.as_deref(),
        owner_socket.as_deref(),
        &config.runtime_socket,
        &company_key,
    ) {
        Ok((socket, provenance)) => {
            tracing::info!(
                company = %company_key,
                socket = %socket,
                provenance,
                "chiefd run: runtime socket resolved"
            );
            config.runtime_socket = socket;
        }
        Err(error) => {
            tracing::error!(company = %company_key, %error, "chiefd run: refusing to start");
            if let Some((_, _, beacon)) = admission.as_ref() {
                deregister_after_admitted_startup_failure(beacon, &config.dir).await;
            }
            return ExitCode::FAILURE;
        }
    }

    // A company whose manifest is durable may still not have emitted
    // supervision/activity rows. Seed their deterministic initial state from the
    // manifest once; there is no blob adoption path and therefore no second
    // authority. A company with no manifest at all is a company being born, and
    // is reported as that rather than as two failures — see
    // [`crate::bootstrap::seed_boot_ledgers`].
    crate::bootstrap::seed_boot_ledgers(&company, &company_key).await;

    // #376: one change-feed, shared between this company's writer actor and
    // the docstore surface bound further down — so a `CompanyDb::mutate`
    // commit (every supervision duty's write path) publishes onto the exact
    // same feed a `DocStore` write would, and `/v1/docs/watch` (the footer's
    // `SseWatcher`, packages/piing/extensions/team-ui.ts) is no longer structurally blind to
    // chiefd-core's own commits. Built here, before the docstore mount below
    // — #368's reactive duty scheduler is meant to subscribe to this SAME
    // instance regardless of mount ordering.
    let change_feed = Arc::new(docstore::ChangeFeed::new());
    // #372: the key every `/v1/docs/*` caller scopes by — used below to gate
    // the live-supervision read special case to exactly this process's own
    // company, never a foreign one.
    wire_change_feed(&company, Arc::clone(&change_feed), company_key.clone());
    // #372: this company's live-supervision source — `/v1/docs/read` reads
    // `store == "supervision"` straight off `company` instead of a mirrored
    // `org_documents` row, gated to exactly this process's own key (see
    // `wire_change_feed`'s doc comment for why the mirror this replaces was
    // retired). Built here, before `company` moves into the `Daemon` below.
    //
    // od:idle-cpu #437 follow-up: LOG the key this boot computed, beside the
    // directory it was computed from. The gate is `req.slug == company_key`,
    // and the key digests the directory PATH — so a client standing in a
    // symlinked or differently-spelled spelling of the same directory computes
    // a different key, never matches, and every supervision read silently
    // falls through to the `org_documents` row instead of CompanyDb. That is
    // the two-authority split (#440) that reappears with no error anywhere.
    // Both sides canonicalize before hashing; one INFO line naming both is
    // what makes a disagreement a single grep rather than an investigation.
    tracing::info!(
        company = %company_key,
        dir = %config.dir.display(),
        "chiefd run: supervision live-read/CAS is scoped to this company key"
    );
    let supervision_live =
        docstore::SupervisionLiveSource::new(Arc::clone(&company), company_key.clone());
    let bench_completion = Arc::new(docstore::BenchCompletionRegistry::default());

    // chiefd actuates the live runtime directly — there is no staged-rollout flag. The
    // real limits are the destructive-action budget (refuses an oversized plan)
    // and the 3-strike circuit breaker (drops a repeatedly-failing company to
    // shadow); both live inside `reconcile_cycle`. To turn actuation ON without
    // an operator opt-in, set the durable converge-safety config to apply here at
    // boot. NB: this also resets the breaker on every boot — accepted under
    // move-fast/fix-forward (a reboot is a fresh start), not a bug.
    if let Err(error) = enable_live_actuation(&company).await {
        // Non-fatal: a company that could not be flipped to apply simply runs its
        // duties and converges in shadow this boot, surfacing via the intent log.
        tracing::warn!(
            company = %company_key,
            %error,
            "chiefd run: could not set live actuation config; converging in shadow this boot"
        );
    }

    let host = real_host();
    // #739 P2: the synchronous `/v1/org/projection/reconcile` route needs its
    // own `Arc<dyn HostExecutor>` handle -- `host` itself is moved into
    // `production_hooks` on the next line and not retained there. Cloning the
    // Arc (not calling `real_host` a second time) keeps this ONE live
    // executor for the whole process, matching every other daemon-only
    // capability `SupervisionLiveSource` carries.
    let host_for_reconcile_route = Arc::clone(&host);
    // ONE CELL, THREE HOLDERS. The desired-set route stamps it, the health
    // gatherer raises `runtime_unattended` off it, and the supervision duty
    // decides whether its own committed pass is worth an INFO. Taken off the
    // source that mints it rather than minted here and injected three times:
    // three independent cells would each be individually correct and
    // collectively useless.
    let attendance = supervision_live.actuator_attendance().clone();
    let (hooks, reconcile_trigger, surface_bound) =
        production_hooks(&company, host, &config, &company_key, &db_path, attendance.clone());
    // The HTTP route receives the exact configuration family the actuator
    // used, including the same latch set once the listener binds. It never
    // opens another CompanyDb and never derives a company's daemon from
    // ambient process state.
    let api_host_profile_config_for_route =
        api_host_launch_profile_config(&config, Arc::clone(&surface_bound));
    let api_host_launch_profile = ApiHostLaunchProfileSource::new(
        Arc::clone(&company),
        api_host_profile_config_for_route.clone(),
    );
    // #739 P2: an independent second `ActuatorConfig`, reconstructed here the
    // same way `api_host_launch_profile_config` above is already re-derived a
    // second time for the launch-profile source rather than threaded out of
    // `production_hooks`. This is not a fresh pattern -- it is the existing
    // one, applied once more.
    let reconcile_actuator_config = ActuatorConfig {
        socket: config.runtime_socket.clone(),
        watching_since: watching_since(),
        dir: api_host_profile_config_for_route.dir,
        home: api_host_profile_config_for_route.home,
        pi_binary: config.pi_binary.clone(),
        floor: RECONCILE_FLOOR,
        launcher_root: api_host_profile_config_for_route.launcher_root,
        root_pi_agent_dir: api_host_profile_config_for_route.root_pi_agent_dir,
    };
    // #637: launcher-authored supervision writes reach the same normalized
    // authority as ChiefD's duties. Their committed feed event must also wake
    // the reactive fan-out: otherwise a reactive duty can be asleep on its old
    // five-minute fallback floor and miss newly armed work until that floor
    // expires. This subscribes to the in-process feed
    // even without an HTTP/docstore mount; the feed is the durable writer's
    // post-commit signal, not an HTTP implementation detail.
    let _supervision_schedule_wake = spawn_supervision_schedule_wake(
        change_feed.subscribe(),
        company_key.clone(),
        Arc::clone(&reconcile_trigger),
    );
    // The per-company database is always the durable authority for runtime
    // ownership as of E10-S2 (#763) — there is no longer an "opt-in
    // migration-mode inert default" to fall back to. Once boot has resolved
    // its socket from that authority, a later foreign observation is
    // unconditionally a handoff/drift event: exit so the service manager
    // restarts and performs the one safe adoption point again (#469).
    // The reminder routes' wake seam. `ReminderDispatch` is on the reactive
    // fan-out (2fe0c331), but that fan-out re-broadcasts from THIS one signal,
    // and the only thing that nudges it is the `ReconcileWaker` on a
    // mailbox/fence event. An `/v1/reminders/arm` request is a different caller
    // entirely: without handing the trigger to the router, the duty would sleep
    // out the alarm it computed BEFORE the reminder was armed, and a reminder
    // set one minute out would not be looked at for five (the fallback floor).
    let supervision_live = supervision_live
        .with_reminder_trigger(Arc::clone(&reconcile_trigger))
        .with_reconcile_trigger(Arc::clone(&reconcile_trigger))
        .with_bench_completion(Arc::clone(&bench_completion))
        .with_api_host_launch_profile(api_host_launch_profile)
        .with_host_executor(host_for_reconcile_route)
        // The COMPANY DIRECTORY. Every agent home hangs off
        // `<dir>/.chief/agent/<person_id>/`, derived by `agent_home::agent_home`
        // rather than composed here — the daemon and the enroller used to
        // compose it separately and landed one segment apart.
        .with_agent_home_root(agent_home_root(&config))
        .with_reconcile_actuator_config(reconcile_actuator_config);
    // Before anything installs out of it, and before any pane is launched.
    reconcile_company_skill_library(&company, &config).await;
    // agent-auth: keep a clock handle before `clock` is moved into the daemon —
    // the auth runtime (built below) stamps token `iat`/nonce TTLs from it.
    let clock_for_auth = Arc::clone(&clock);
    // Unconditional as of E10-S2 (#763): the runtime-ownership claim is
    // always observable now (see the comment above), so a foreign
    // observation is always fatal-shutdown-worthy, never silently ignored.
    let daemon =
        Daemon::new(company_key.clone(), Arc::clone(&company), clock, hooks, ActuationMode::Apply)
            .with_reconcile_trigger(reconcile_trigger)
            .with_bench_completion(bench_completion)
            .with_actuator_attendance(attendance)
            .with_foreign_identity_fatal_shutdown();

    let daemon = Arc::new(daemon);

    if config.once {
        // A one-shot smoke run proves the loop wires end to end without waiting
        // on any cadence; standing up a persistent HTTP listener for it would
        // outlive the single pass and contend for `:8792` with a real daemon, so
        // `--once` deliberately mounts NO docstore surface — the same way it
        // skips the interval loop.
        //
        // And therefore no manifest-readiness gate either: the gate waits for a
        // genesis that arrives over exactly the surface this path does not mount,
        // so waiting here could only ever burn the whole budget. `--once` is a
        // smoke run against a company that already exists.
        daemon.run_startup_self_audit().await;
        daemon.run_once_all().await;
        daemon.company.shutdown();
        return ExitCode::SUCCESS;
    }

    // This exact listener was reserved and admitted before `CompanyDb::open`.
    // Keep it held until `mount` moves it directly into the HTTP surface: no
    // second bind is possible, so the URL beacond owns is the URL that serves
    // the same per-company SQLite file as the duty scheduler.
    let Some((reservation, bound_addr, beacon_for_deregister)) = admission else {
        // `--once` returned above, so this is an internal lifecycle breach,
        // not a recoverable admission alternative. Fail closed rather than
        // panicking or opening storage without the admitted reservation.
        tracing::error!(
            company = %company_key,
            "chiefd run: no admitted listener remained after persistent startup selection; refusing to serve"
        );
        daemon.company.shutdown();
        return ExitCode::FAILURE;
    };

    // agent-auth (P0) + #751/P7: build the auth runtime from this company's
    // actor, resolve the HS256 secret, and bootstrap-enrol the operator BEFORE
    // serving. This is UNCONDITIONAL now. It used to be skipped whenever
    // CHIEFD_AUTH_ENABLED was unset, which was survivable only while agents
    // were authenticated by the terminal pane they descended from; that
    // authentication is deleted, so a company with no issuer is a company whose
    // agents cannot prove who they are. An init failure refuses to serve.
    //
    // A6: the runtime's PRESENCE is the whole decision. There is no longer a
    // rollout flag beside it — the variable that produced one was set by
    // nothing, so every company daemon in the fleet attached a runtime and then
    // served every route to a caller that presented nothing at all.
    let auth_runtime = {
        let clock_for_closure = Arc::clone(&clock_for_auth);
        match ensure_daemon_auth_runtime(
            Arc::clone(&daemon.company),
            &config.dir,
            &company_key,
            Arc::new(move || clock_for_closure.wall().0),
        )
        .await
        {
            Ok(runtime) => runtime,
            Err(()) => {
                daemon.company.shutdown();
                deregister_after_admitted_startup_failure(&beacon_for_deregister, &config.dir)
                    .await;
                return ExitCode::FAILURE;
            }
        }
    };

    let docstore = match reservation.mount(
        &store_config,
        Arc::clone(&change_feed),
        Some(supervision_live),
    ) {
        Ok(bound) => bound
            .with_auth(Some(Arc::clone(&auth_runtime)))
            .with_runtime_identity("company", Some(company_key.clone()))
            .with_shutdown_requester(daemon.shutdown_requester())
            .with_liveness_sink(std::sync::Arc::new(crate::beacon::HeartbeatSink::new(
                std::sync::Arc::clone(&beacon_for_deregister),
            )))
            // E8-S2 (#824): GET /v1/docs/queue, the writer-queue diagnostics
            // that replace `org lock list` once E8-S6 deletes the file locks
            // it read. Keep this additive to #764's admitted-listener and
            // liveness-sink wiring above.
            .with_queue_source(Arc::clone(&company)),
        Err(error) => {
            tracing::error!(
                company = %company_key,
                %error,
                "chiefd run: admitted listener could not mount the org_documents store; refusing to run"
            );
            daemon.company.shutdown();
            deregister_after_admitted_startup_failure(&beacon_for_deregister, &config.dir).await;
            return ExitCode::FAILURE;
        }
    };

    // Ensure the docstore schema only after beacond has admitted the daemon.
    // A refused process never reaches this point, so it cannot create a WAL or
    // mutate either schema. The health contract remains schema-present before
    // the launcher can see this listener as ready.
    if let Err(error) = docstore.ensure_schema().await {
        tracing::error!(
            company = %company_key,
            %error,
            "chiefd run: org_documents schema could not be ensured at startup; refusing to run \
             (health contract is schema-present — a daemon that cannot reach it must not serve)"
        );
        daemon.company.shutdown();
        deregister_after_admitted_startup_failure(&beacon_for_deregister, &config.dir).await;
        return ExitCode::FAILURE;
    }
    tracing::info!(
        company = %company_key,
        bind = %store_config.bind,
        bound = %bound_addr,
        db = %store_config.db_path,
        "chiefd run: admitted listener mounted on its registered address and schema ensured alongside the duty scheduler"
    );
    // THE ONE READY INSTANT, and everything that publishes a location hangs
    // off it. beacond has admitted this daemon, the listener is mounted on the
    // registered address, and the schema behind it is ensured — so the URL is
    // answerable, which is the whole precondition for telling anyone about it.
    //
    // The API host may now project children for this company. A child resolves
    // its own company through this process, so this latch — not an address —
    // is what it waits on.
    let _ = surface_bound.set(());
    // And the operator client may now find this daemon by standing in the
    // directory. `<dir>/.chief/run/daemon.json` replaces a beacond lookup by
    // slug: the client reads the file, checks it names THIS directory, and
    // proves the pid and the URL before binding either. Published HERE, at the
    // same latch, because publishing a URL that does not yet answer is what
    // turns a pointer into a lie — and there is deliberately no second
    // ordering point for it.
    //
    // A failure REFUSES TO SERVE. A daemon nobody in its own directory can
    // find is not something an operator can attach to, and every later command
    // there would time out with nothing to name.
    if let Err(error) = crate::rendezvous::publish(&config.dir, &format!("http://{bound_addr}")) {
        tracing::error!(
            company = %company_key,
            dir = %config.dir.display(),
            %error,
            "chiefd run: could not publish the daemon rendezvous; refusing to serve a company no \
             command in its own directory could find"
        );
        daemon.company.shutdown();
        deregister_after_admitted_startup_failure(&beacon_for_deregister, &config.dir).await;
        return ExitCode::FAILURE;
    }
    tracing::info!(
        event = "daemon.rendezvous.published",
        company = %company_key,
        path = %host_primitives::rendezvous::rendezvous_path(&config.dir).display(),
        url = %format!("http://{bound_addr}"),
        pid = std::process::id(),
        "a command run in this directory can now find this daemon"
    );
    let docstore = Some(docstore);

    let outcome = match daemon.serve(docstore).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(company = %company_key, %error, "chiefd run: fatal runtime-owner handoff; exiting non-zero for supervised restart");
            ExitCode::FAILURE
        }
    };
    // THE TWO PUBLISHED LOCATIONS COME DOWN TOGETHER. This process announced
    // itself in exactly two places — beacond's row and the directory's own
    // rendezvous — so a graceful shutdown that cleared one and left the other
    // would leave the two disagreeing about whether this daemon is alive.
    //
    // E10-S3 (#764): clearing beacond's location is one pid-fenced UPDATE and
    // does NOT delete the company. Bounded by `Beacon::deregister`'s own 500ms
    // budget so a wedged beacond cannot hold shutdown; a failed deregister is a
    // loud warn, never fatal (the daemon is already exiting either way, and a
    // SIGKILLed daemon never reaches this line at all — the NEXT `register`
    // reclaims the stale location by design, see `beacon.rs`'s module doc).
    if let Err(error) = beacon_for_deregister.deregister().await {
        tracing::warn!(
            company = %company_key,
            %error,
            "chiefd run: beacond_deregister_failed — the location will read stale until the next register"
        );
    }
    // The rendezvous is removed on the same terms and for the same reason: a
    // SIGKILLed daemon leaves it behind, and that is expected — it is a
    // POINTER, so the next reader proves the pid and the URL before trusting
    // it and the next daemon overwrites it. There is no lock and no heartbeat
    // to clean up, which is precisely why this is a one-line removal.
    if let Err(error) = crate::rendezvous::remove(&config.dir) {
        tracing::warn!(
            company = %company_key,
            %error,
            "chiefd run: the daemon rendezvous could not be removed — it will read stale until a \
             reader probes its pid or the next daemon overwrites it"
        );
    }
    outcome
}

/// Parse `CHIEFD_STORE_PORT_WALK` (default 64, per E10-S3/#764's Contract):
/// how many consecutive ports `bind_walking` tries, inclusive of the first.
/// `1` disables the walk (today's pre-#764 behaviour: exactly one attempt).
/// Values `< 1` or unparseable fall back to the default rather than
/// refusing to boot over a malformed env var.
pub(crate) fn port_walk_from_env(var: impl Fn(&str) -> Option<String>) -> u16 {
    const DEFAULT_PORT_WALK: u16 = 64;
    var("CHIEFD_STORE_PORT_WALK")
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|&walk| walk >= 1)
        .unwrap_or(DEFAULT_PORT_WALK)
}

/// Detached, like the other forwarders here: this is a parked broadcast
/// receiver with no polling or shutdown resource of its own. A lagged receiver
/// wakes once conservatively; the next duty pass is read-only when no deadline
/// is due, while suppressing the wake would make a real armed deadline late.
/// Which committed document stores are DESIRED STATE the reconcile cycle reads,
/// and therefore must wake it the moment they commit.
///
/// The supervision ledger was here from #637. The organization manifest --
/// the department tree and the roster, the authority the cycle diffs to decide
/// which panes should exist -- was not, and it is exactly what a human authors
/// when a department is created. Every org_ops ROUTE already calls
/// `wake_reconcile`, so on that path this is a harmless duplicate that `Notify`
/// coalesces away; it exists for the writes that do NOT come through a route.
/// A manifest write intercepted onto the typed store (#442) from the launcher's
/// own docstore surface published on this feed and woke nothing at all, which
/// left the single most operator-visible change in the product -- structure --
/// as the one desired-state edit with no event source.
///
/// This is a scheduling signal only: it marks the duty dirty. The pass that
/// follows re-derives everything from disk and is the sole authority for what
/// actually happens.
/// # The minute a wake used to take, and why
///
/// MEASURED on a live company: the operator clicked a sleeping person at
/// 18:48:52 and their pane appeared at 18:49:53 — **sixty-one seconds**, which
/// is not this trigger firing but [`reactive_fallback_floor`] timing out.
///
/// `org_ops::wake_person` writes exactly two things: a launch-intent FENCE row
/// and an idle-park RELEASE. Both are reconcile inputs of the first order —
/// `person_can_run` reads the fence to decide whether somebody may start at
/// all, and the park is what the settle put there. Neither is the supervision
/// ledger and neither is the organization manifest, so this predicate answered
/// `false`, no `notify_one` was issued, and the single most latency-sensitive
/// gesture in the product waited out a fallback interval designed for writes
/// nobody is watching.
///
/// The operator's report was the whole symptom: "I click on her and it says
/// loading but it stays at sleeping — why isn't it starting?" It was starting.
/// It was starting a minute later.
///
/// The list is DERIVED from what the pass reads, not from where the write came
/// from. Adding a store here can only cost a redundant pass that re-derives
/// everything from disk and emits an empty plan; omitting one costs a minute.
fn is_reconcile_input_store(store: &str) -> bool {
    chiefd_core::store::supervision::is_supervision_store(store)
        || chiefd_core::store::organization::is_organization_store(store)
        // The idle park and its release: what the settle withdrew and what a
        // wake gives back.
        || chiefd_core::store::activity::is_activity_store(store)
        // The per-person launch fence: the authority on who may run at all.
        || store == chiefd_core::store::launch_intent_rows::LAUNCH_INTENT_STORE
}

fn spawn_supervision_schedule_wake(
    mut changes: tokio::sync::broadcast::Receiver<docstore::WatchEvent>,
    company_key: String,
    trigger: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match changes.recv().await {
                Ok(event)
                    if event.slug == company_key
                        && !event.removed
                        && is_reconcile_input_store(&event.store) =>
                {
                    trigger.notify_one();
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => trigger.notify_one(),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

// --- inert-by-design + test hooks ------------------------------------------

/// The all-empty hook set the loop's own tests run against. Every production
/// hook is now really wired (see `production_hooks`).
/// `cycle_input`/`health` (od-host-gatherers-completion) moved to real wiring;
/// their former inert scaffolds (`InertCycleInput`/`EmptyHealth`) are
/// gone/test-only — see `production_hooks` for what replaced each.
pub mod stubs {
    #[cfg(test)]
    use super::{BoxFuture, DutyContext};
    #[cfg(test)]
    use chiefd_core::runtime::duty_hooks::DutyError;
    // Only `test_hooks` assembles a `Hooks` from `Arc`s now that production wiring
    // moved to `super::production_hooks`; the inert stubs are plain trait impls.
    #[cfg(test)]
    use std::sync::Arc;

    #[cfg(test)]
    use super::{CycleInputGatherer, HealthSnapshotGatherer};
    #[cfg(test)]
    use chiefd_core::store::health_collect::{HealthCollectionSnapshot, RuntimeSample};
    #[cfg(test)]
    use chiefd_core::store::supervision::CycleInput;

    /// An empty "nothing observed" health snapshot at `now`. Constructed field
    /// by field because `HealthCollectionSnapshot` intentionally has no
    /// `Default` — every host observation is a decision the gatherer makes.
    /// Test-only now that production wires the real `HostHealthSnapshotGatherer`
    /// (`chiefd_host::gather`).
    #[cfg(test)]
    #[must_use]
    fn empty_health_snapshot(now: i64) -> HealthCollectionSnapshot {
        HealthCollectionSnapshot {
            converge_cycle: None,
            now_millis: now,
            socket_name: String::new(),
            supervisor: None,
            supervisor_stale_ms: None,
            supervision_effect_stale_ms: None,
            runtime: None,
            expected_active_people: Vec::new(),
            runtime_audit: RuntimeSample::NotRun,
            dead_processes: Vec::new(),
            supervision_effects: Vec::new(),
            log_incidents: Vec::new(),
            log_cursors: std::collections::BTreeMap::new(),
            idle_transitions: Vec::new(),
            // Attended: this fixture asserts about other observations, so it
            // must not smuggle in an unattended-company incident.
            actuator_silent_ms: 0,
            idle_supervision_error: None,
            mailboxes: Vec::new(),
            mailbox_stale_ms: None,
            idle_transition_stale_ms: None,
        }
    }

    /// A cycle-input gatherer that reports an owned company with an empty
    /// observation — for tests that want the real cycle to run.
    #[cfg(test)]
    pub struct OwnedEmptyCycleInput;
    #[cfg(test)]
    impl CycleInputGatherer for OwnedEmptyCycleInput {
        fn gather_cycle_input(
            &self,
            _ctx: &DutyContext,
        ) -> BoxFuture<'_, Result<CycleInput, DutyError>> {
            Box::pin(async { Ok(CycleInput::default()) })
        }
    }

    /// An actuator that computes nothing and touches nothing. Test-only now that
    /// production wires the real `ConvergeActuator`.
    #[cfg(test)]
    pub struct NoopActuator;
    #[cfg(test)]
    impl super::ReconcileActuator for NoopActuator {
        fn reconcile(
            &self,
            _ctx: &DutyContext,
            _mode: super::ActuationMode,
        ) -> BoxFuture<'_, Result<chiefd_core::runtime::duty_hooks::ReconcileReport, DutyError>>
        {
            Box::pin(async { Ok(chiefd_core::runtime::duty_hooks::ReconcileReport::default()) })
        }
    }

    /// A health gatherer that returns an empty snapshot (no incidents).
    /// Test-only now that production wires the real `HostHealthSnapshotGatherer`.
    #[cfg(test)]
    pub struct EmptyHealth;
    #[cfg(test)]
    impl HealthSnapshotGatherer for EmptyHealth {
        fn gather_health(
            &self,
            ctx: &DutyContext,
        ) -> BoxFuture<'_, Result<HealthCollectionSnapshot, DutyError>> {
            let now = ctx.snapshot.ledgers().now().0;
            Box::pin(async move { Ok(empty_health_snapshot(now)) })
        }
    }

    /// A delivery sink that dispatches nothing and reports nothing delivered.
    /// Test-only now that production wires the real `MailboxDeliverySink`.
    #[cfg(test)]
    pub struct NoopDelivery;
    #[cfg(test)]
    impl super::DeliverySink for NoopDelivery {
        fn deliver(
            &self,
            _ctx: &DutyContext,
            _envelopes: Vec<super::EffectEnvelope>,
        ) -> BoxFuture<'_, super::DeliveryOutcome> {
            Box::pin(async { super::DeliveryOutcome::default() })
        }
    }

    /// The all-empty-success hook set for tests: the real cycle runs against an
    /// owned, empty observation so a seeded company mutates deterministically.
    #[cfg(test)]
    #[must_use]
    pub fn test_hooks() -> super::Hooks {
        super::Hooks {
            cycle_input: Arc::new(OwnedEmptyCycleInput),
            actuator: Arc::new(NoopActuator),
            health: Arc::new(EmptyHealth),
            delivery: Arc::new(NoopDelivery),
        }
    }
}

#[cfg(test)]
mod tests;
