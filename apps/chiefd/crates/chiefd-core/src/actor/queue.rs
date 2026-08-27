//! The writer queue and its class scheduler.
//!
//! This module is deliberately free of threads, channels, SQLite and time
//! sources: it is a state machine driven by an explicit `now`. That is what
//! makes the scheduler tests in [`super::writer`] deterministic rather than
//! repetitive — *"No race is ever validated by repetition"* (TESTING.md §1.2).
//! The same state machine is exercised directly here with no actor at all.
//!
//! # The two rules it encodes
//!
//! 1. **Priority.** [`MutationClass::Small`] rides its own queue. Goal
//!    publication, assignment fences, receipts and the DB half of
//!    `model.change` must never queue behind a multi-second reconcile
//!    (plan §5.2; the intent-spool replacement guarantee, §11-A2).
//! 2. **Aging.** Strict priority would let a hot 28-agent fleet starve
//!    reconcile in the other direction, so after `N` consecutive `Small` ops
//!    *or* `T` of waiting by the oldest lower-priority op, one lower-priority
//!    op is admitted ([`AgingPolicy`]).
//!
//! # Why the deadline lives here and not at the caller
//!
//! `Busy` is minted in exactly one place on this path: [`QueueState::reap`],
//! which runs on the writer as it looks for its next job. **A caller has no
//! timeout of its own** — `mutate` awaits a oneshot and nothing else. So a
//! `Busy` result is proof that the job sat in a real queue for the full
//! [`MUTATION_QUEUE_DEADLINE`](super::MUTATION_QUEUE_DEADLINE); there is no
//! code path, accidental or otherwise, by which a caller obtains a fail-fast
//! acquisition. This is the structural form of plan §5.2 item 2.

use std::collections::VecDeque;
use std::time::Duration;

use crate::actor::{AgingPolicy, BusyProof, MutationClass, MutationName};
use crate::clock::Monotonic;
use crate::error::ChiefdError;
use crate::ledger::Ledgers;

/// The site string carried by every `Busy` this module mints.
pub(crate) const QUEUE_BUSY_SITE: &str = "mutation-queue";

/// Runs the caller's closure against the working ledgers.
///
/// The error is the full [`ChiefdError`] rather than a [`Refusal`] because a
/// store op can legitimately lose a **fence** — an assignment's
/// `(person, generation)` pair, a maintenance
/// claim triple — and plan §1 spells that `Conflict{expected, actual}`, not
/// `Refused`. Narrowing it to `Refusal` (as the plan §5.2 sketch did) would
/// force every fenced op to flatten a conflict into a decline, and the
/// conformance corpus matches on `{type, code}`, so the flattening would fail a
/// fixture rather than pass quietly.
///
/// This does **not** widen who may mint `Busy`: [`BusyProof`]'s constructor is
/// crate-private and `busy_is_mintable_from_exactly_the_two_reviewed_wait_sites`
/// pins the two call sites by grep, independently of this signature.
pub(crate) type ApplyFn = Box<dyn FnOnce(&mut Ledgers) -> Result<(), ChiefdError> + Send>;

/// Refreshes an in-memory projection after its source rows have committed.
///
/// The row transaction prepares the projection before `COMMIT`; this hook only
/// makes that already-validated projection visible to synchronous actor
/// readers. It must never write durable state: normalized rows remain the
/// authority and a projection refresh must not recreate a document fallback.
pub(crate) type PostCommitFn = Box<dyn FnOnce(&mut Ledgers) + Send>;

/// Work a job does directly against its own `BEGIN IMMEDIATE`, before the
/// ledger closure runs.
///
/// Used by the relational `host_actions` 2PC steps, which are not a
/// `documents` row. Running inside the same transaction is the whole point —
/// a step checked *before* the transaction is one a racing writer can
/// invalidate before the commit lands.
///
/// It returns the full [`ChiefdError`] rather than a [`Refusal`] because the
/// failure it reports is a `Conflict`, not a decline.
pub(crate) type TxnFn =
    Box<dyn FnOnce(&rusqlite::Transaction<'_>) -> Result<(), ChiefdError> + Send>;

/// Delivers a job's outcome to the caller. Runs exactly once per job.
pub(crate) type FinishFn = Box<dyn FnOnce(Result<(), ChiefdError>) + Send>;

/// A queued actor operation.
///
/// `apply` runs the caller's closure against the working ledgers; `finish`
/// delivers the outcome to the caller. Exactly one of the two delivery paths
/// runs for every job that is ever enqueued: `finish(Ok)` after a commit,
/// `finish(Err)` on refusal, rejection or a durable failure. A job is never
/// dropped silently, which is why a caller can await a oneshot with no timeout
/// and still be certain of an answer.
pub(crate) struct Job {
    pub(crate) class: MutationClass,
    /// The mutation this job commits.
    ///
    /// `Option` only so `run_job` can report a job that reached the writer
    /// carrying no name at all. **This queue is the WRITE queue**: reads do not
    /// enter it, because a read runs on `CompanyDb`'s read-only connection
    /// pool. A read that queued here would be capped at one at a time and would
    /// wait behind every mutation ahead of it.
    pub(crate) name: Option<MutationName>,
    pub(crate) enqueued_at: Monotonic,
    /// Transaction-scoped step, run first and inside the same
    /// `BEGIN IMMEDIATE`. `None` for an ordinary `mutate`.
    pub(crate) txn: Option<TxnFn>,
    /// A prepared row-authoritative projection to install after a successful
    /// commit and before this job becomes visible to readers.
    pub(crate) post_commit: Option<PostCommitFn>,
    pub(crate) apply: ApplyFn,
    pub(crate) finish: FinishFn,
}

impl Job {
    /// How long this job has waited, as of `now`.
    pub(crate) fn waited(&self, now: Monotonic) -> Duration {
        Duration::from_millis(self.enqueued_at.millis_until(now))
    }
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("class", &self.class)
            .field("name", &self.name)
            .field("enqueued_at", &self.enqueued_at)
            .finish_non_exhaustive()
    }
}

/// Whether the actor is still taking work.
///
/// Closing admission is how `company.remove.plan` step 2 is expressed: *"the
/// company writer actor stops admitting, in-flight `mutate` futures resolve
/// `Unavailable` (never hang)"* (plan §2.1). It is **not** a lock a caller can
/// fail fast against — it is a door that is shut for the rest of this actor's
/// life, and the reason is reported verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Normal operation.
    Open,
    /// Closed, with the stable reason string callers receive.
    Closed(&'static str),
}

/// What the writer should do next.
pub(crate) enum Next {
    /// Run this job.
    Run(Job),
    /// Deliver these rejections (outside the queue lock), then look again.
    Reject(Vec<(Job, ChiefdError)>),
    /// Nothing to do and nothing will arrive: shut down.
    Stop,
    /// Nothing to do right now; park on the condvar.
    Idle,
}

/// The queue pair plus the scheduler's aging counters.
pub(crate) struct QueueState {
    small: VecDeque<Job>,
    lower: VecDeque<Job>,
    consecutive_small: u32,
    admission: Admission,
    stop_requested: bool,
}

impl QueueState {
    pub(crate) fn new() -> Self {
        Self {
            small: VecDeque::new(),
            lower: VecDeque::new(),
            consecutive_small: 0,
            admission: Admission::Open,
            stop_requested: false,
        }
    }

    pub(crate) fn admission(&self) -> Admission {
        self.admission
    }

    /// Shut the door. Idempotent; the first reason wins so a later
    /// `shutdown()` cannot relabel an in-progress removal.
    pub(crate) fn close(&mut self, reason: &'static str) {
        if self.admission == Admission::Open {
            self.admission = Admission::Closed(reason);
        }
    }

    /// Shut the door and take everything still queued, so the caller can answer
    /// it immediately.
    ///
    /// Draining here rather than waiting for the writer's next scheduling pass
    /// is what makes plan §2.1 step 2 true: queued `mutate` futures resolve
    /// `Unavailable` *now*, not after the multi-second reconcile that happens to
    /// be executing. `remove.plan` must not be able to hang behind one.
    pub(crate) fn close_and_drain(&mut self, reason: &'static str) -> Vec<(Job, ChiefdError)> {
        self.close(reason);
        let Admission::Closed(actual) = self.admission else { return Vec::new() };
        self.small
            .drain(..)
            .chain(self.lower.drain(..))
            .map(|job| (job, ChiefdError::Unavailable { reason: actual }))
            .collect()
    }

    /// Ask the writer to exit once the queue is empty.
    pub(crate) fn request_stop(&mut self) {
        self.stop_requested = true;
    }

    /// Enqueue, or report why the door is shut.
    ///
    /// A rejected job is dropped here rather than answered through its
    /// `finish`: the caller has not started awaiting yet, so
    /// [`CompanyDb::mutate`](crate::actor::CompanyDb::mutate) returns this
    /// error to it directly. Every job that is *accepted* is answered.
    ///
    /// # Errors
    /// `Unavailable` with the reason admission was closed.
    pub(crate) fn push(&mut self, job: Job) -> Result<(), ChiefdError> {
        if let Admission::Closed(reason) = self.admission {
            return Err(ChiefdError::Unavailable { reason });
        }
        if job.class.is_priority() {
            self.small.push_back(job);
        } else {
            self.lower.push_back(job);
        }
        Ok(())
    }

    pub(crate) fn depth(&self) -> usize {
        self.small.len() + self.lower.len()
    }

    /// How long the OLDEST still-queued job (across both classes) has waited,
    /// as of `now`. `0` when the queue is empty — diagnostics
    /// (`CompanyDb::queue_snapshot`, E8-S2, #824) never needs a wall-clock
    /// read of its own: this is a pure function of already-held state,
    /// consistent with the `Mutex<QueueState>` discipline everywhere else in
    /// this module (README §5.2 item 4 — no lock protocol, a data-structure
    /// guard read for a few statements and never across an `.await`).
    #[must_use]
    pub(crate) fn oldest_wait_ms(&self, now: Monotonic) -> u64 {
        [self.small.front(), self.lower.front()]
            .into_iter()
            .flatten()
            .map(|job| u64::try_from(job.waited(now).as_millis()).unwrap_or(u64::MAX))
            .max()
            .unwrap_or(0)
    }

    /// Jobs that must be answered without running: everything queued once
    /// admission closed, and anything that has waited out the queue deadline.
    ///
    /// The deadline sweep only needs to inspect each queue's front, because a
    /// queue is FIFO by enqueue time.
    fn reap(&mut self, now: Monotonic, deadline: Duration) -> Vec<(Job, ChiefdError)> {
        let mut out = Vec::new();
        if let Admission::Closed(reason) = self.admission {
            for job in self.small.drain(..).chain(self.lower.drain(..)) {
                out.push((job, ChiefdError::Unavailable { reason }));
            }
            return out;
        }
        for queue in [&mut self.small, &mut self.lower] {
            while queue.front().is_some_and(|job| job.waited(now) >= deadline) {
                let Some(job) = queue.pop_front() else { break };
                let waited = job.waited(now);
                out.push((
                    job,
                    ChiefdError::Busy(BusyProof::after_waiting(waited, QUEUE_BUSY_SITE)),
                ));
            }
        }
        out
    }

    /// Pick the next job per the priority + aging rules.
    fn take_next(&mut self, now: Monotonic, aging: AgingPolicy) -> Option<Job> {
        let admit_lower = match (self.small.front(), self.lower.front()) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(_), Some(head)) => {
                aging.should_admit_aged(self.consecutive_small, head.waited(now))
            }
        };
        if admit_lower {
            self.consecutive_small = 0;
            self.lower.pop_front()
        } else {
            let job = self.small.pop_front()?;
            self.consecutive_small = self.consecutive_small.saturating_add(1);
            Some(job)
        }
    }

    /// One scheduling decision. The writer calls this under the queue lock and
    /// acts on the result after releasing it.
    pub(crate) fn next(&mut self, now: Monotonic, aging: AgingPolicy, deadline: Duration) -> Next {
        let rejects = self.reap(now, deadline);
        if !rejects.is_empty() {
            return Next::Reject(rejects);
        }
        if let Some(job) = self.take_next(now, aging) {
            return Next::Run(job);
        }
        if self.stop_requested {
            return Next::Stop;
        }
        Next::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    const DEADLINE: Duration = Duration::from_secs(30);

    /// A job that records its name when `finish` runs, so a test can read the
    /// admission order off the scheduler with no threads involved.
    fn job(
        class: MutationClass,
        name: &'static str,
        at: Monotonic,
        log: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Job {
        Job {
            class,
            name: Some(MutationName(name)),
            enqueued_at: at,
            txn: None,

            post_commit: None,
            apply: Box::new(|_| Ok(())),
            finish: Box::new(move |outcome| {
                let tag = match outcome {
                    Ok(()) => name.to_string(),
                    Err(err) => format!("{name}:{}", err.kind()),
                };
                log.lock().unwrap_or_else(|e| e.into_inner()).push(tag);
            }),
        }
    }

    fn drain_order(
        state: &mut QueueState,
        now: Monotonic,
        log: &Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Vec<String> {
        loop {
            match state.next(now, AgingPolicy::default(), DEADLINE) {
                Next::Run(j) => (j.finish)(Ok(())),
                Next::Reject(rejects) => {
                    for (j, err) in rejects {
                        (j.finish)(Err(err));
                    }
                }
                Next::Stop | Next::Idle => break,
            }
        }
        let out = log.lock().unwrap_or_else(|e| e.into_inner()).clone();
        out
    }

    #[test]
    fn small_ops_are_admitted_ahead_of_a_queued_reconcile() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = QueueState::new();
        let now = Monotonic(0);
        // The reconcile is enqueued FIRST — priority, not arrival order, decides.
        state.push(job(MutationClass::Reconcile, "reconcile", now, log.clone())).ok();
        for i in 0..3 {
            let name: &'static str = Box::leak(format!("small-{i}").into_boxed_str());
            state.push(job(MutationClass::Small, name, now, log.clone())).ok();
        }
        loop {
            match state.next(now, AgingPolicy::default(), DEADLINE) {
                Next::Run(j) => (j.finish)(Ok(())),
                Next::Reject(r) => {
                    for (j, e) in r {
                        (j.finish)(Err(e));
                    }
                }
                Next::Stop | Next::Idle => break,
            }
        }
        let order = log.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(order, vec!["small-0", "small-1", "small-2", "reconcile"]);
    }

    #[test]
    fn aging_admits_the_reconcile_after_exactly_n_small_ops_with_no_time_passing() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = QueueState::new();
        let now = Monotonic(0);
        state.push(job(MutationClass::Reconcile, "reconcile", now, log.clone())).ok();
        // A saturating Small stream: 100 of them, none of which ever ages out.
        for i in 0..100 {
            let name: &'static str = Box::leak(format!("small-{i}").into_boxed_str());
            state.push(job(MutationClass::Small, name, now, log.clone())).ok();
        }
        let order = {
            loop {
                match state.next(now, AgingPolicy::default(), DEADLINE) {
                    Next::Run(j) => (j.finish)(Ok(())),
                    Next::Reject(r) => {
                        for (j, e) in r {
                            (j.finish)(Err(e));
                        }
                    }
                    Next::Stop | Next::Idle => break,
                }
            }
            log.lock().unwrap_or_else(|e| e.into_inner()).clone()
        };
        // N = 32: thirty-two Small ops, then the aged Reconcile, then the rest.
        assert_eq!(order.len(), 101);
        assert_eq!(order[31], "small-31");
        assert_eq!(order[32], "reconcile", "starvation bound is exactly N=32 Small ops");
        assert_eq!(order[33], "small-32");
    }

    #[test]
    fn aging_admits_the_reconcile_after_the_interval_even_with_zero_small_ops_run() {
        // One millisecond short of T=2s: priority still wins.
        let before = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = QueueState::new();
        state.push(job(MutationClass::Reconcile, "reconcile", Monotonic(0), before.clone())).ok();
        state.push(job(MutationClass::Small, "small", Monotonic(0), before.clone())).ok();
        assert_eq!(drain_order(&mut state, Monotonic(1_999), &before), vec!["small", "reconcile"]);

        // At T=2s the reconcile is admitted first, having run no Small ops at
        // all — the count bound is not the only way out of starvation.
        let after = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = QueueState::new();
        state.push(job(MutationClass::Reconcile, "reconcile", Monotonic(0), after.clone())).ok();
        state.push(job(MutationClass::Small, "small", Monotonic(0), after.clone())).ok();
        assert_eq!(drain_order(&mut state, Monotonic(2_000), &after), vec!["reconcile", "small"]);
    }

    #[test]
    fn a_job_one_millisecond_short_of_the_deadline_still_runs() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = QueueState::new();
        state.push(job(MutationClass::Small, "goal.set", Monotonic(0), log.clone())).ok();

        let almost = Monotonic(u64::try_from(DEADLINE.as_millis()).unwrap_or(u64::MAX) - 1);
        let Next::Run(j) = state.next(almost, AgingPolicy::default(), DEADLINE) else {
            panic!("the deadline is a floor, not a ceiling: 29.999s must still run");
        };
        (j.finish)(Ok(()));
        assert_eq!(log.lock().unwrap_or_else(|e| e.into_inner()).as_slice(), ["goal.set"]);
    }

    #[test]
    fn busy_is_only_ever_minted_after_the_full_queue_deadline() {
        let ran = Arc::new(AtomicU32::new(0));
        let ran_in_closure = ran.clone();
        let outcome: Arc<std::sync::Mutex<Option<Result<(), ChiefdError>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let sink = outcome.clone();
        let mut state = QueueState::new();
        state
            .push(Job {
                class: MutationClass::Small,
                name: Some(MutationName("goal.set")),
                enqueued_at: Monotonic(0),
                txn: None,

                post_commit: None,
                apply: Box::new(move |_| {
                    ran_in_closure.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
                finish: Box::new(move |res| {
                    *sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(res);
                }),
            })
            .ok();

        let expired = Monotonic(u64::try_from(DEADLINE.as_millis()).unwrap_or(u64::MAX));
        let Next::Reject(rejects) = state.next(expired, AgingPolicy::default(), DEADLINE) else {
            panic!("a job past the deadline must be reaped, not run");
        };
        assert_eq!(rejects.len(), 1);
        for (job, err) in rejects {
            match &err {
                ChiefdError::Busy(proof) => {
                    assert_eq!(proof.site(), QUEUE_BUSY_SITE);
                    assert!(
                        proof.waited() >= DEADLINE,
                        "Busy must prove it waited the full deadline, waited {:?}",
                        proof.waited()
                    );
                }
                other => panic!("expected Busy, got {other:?}"),
            }
            (job.finish)(Err(err));
        }
        assert_eq!(ran.load(Ordering::SeqCst), 0, "a Busy job's closure must never run");
        assert!(matches!(
            outcome.lock().unwrap_or_else(|e| e.into_inner()).as_ref(),
            Some(Err(ChiefdError::Busy(_)))
        ));
    }

    #[test]
    fn closed_admission_rejects_queued_and_new_jobs_as_unavailable_never_busy() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = QueueState::new();
        state.push(job(MutationClass::Small, "queued", Monotonic(0), log.clone())).ok();
        state.close("removing");

        // Already-queued work is answered, not hung (plan §2.1 step 2).
        let Next::Reject(rejects) = state.next(Monotonic(0), AgingPolicy::default(), DEADLINE)
        else {
            panic!("closing admission must reject the queue");
        };
        assert_eq!(rejects.len(), 1);
        assert!(matches!(rejects[0].1, ChiefdError::Unavailable { reason: "removing" }));
        for (j, e) in rejects {
            (j.finish)(Err(e));
        }

        // New work is refused at the door with the same reason.
        let rejected = state.push(job(MutationClass::Normal, "late", Monotonic(0), log));
        let Err(err) = rejected else { panic!("a closed actor must not admit work") };
        assert!(matches!(err, ChiefdError::Unavailable { reason: "removing" }));

        assert!(matches!(state.next(Monotonic(0), AgingPolicy::default(), DEADLINE), Next::Idle));
        state.request_stop();
        assert!(matches!(state.next(Monotonic(0), AgingPolicy::default(), DEADLINE), Next::Stop));
    }

    #[test]
    fn the_first_close_reason_wins_so_a_later_shutdown_cannot_relabel_a_removal() {
        let mut state = QueueState::new();
        state.close("removing");
        state.close("stopping");
        assert_eq!(state.admission(), Admission::Closed("removing"));
    }
}
