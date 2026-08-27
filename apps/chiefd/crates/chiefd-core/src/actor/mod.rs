//! The per-company writer actor and its scheduler.
//!
//! One OS thread per company owns the `rusqlite::Connection` for that
//! company's `chief.db`. The only public mutation API is `mutate` /
//! `read` (plan §5.2); the closure runs exactly once inside
//! `BEGIN IMMEDIATE … validate … COMMIT`.
//!
//! ```text
//! impl CompanyDb {
//!     pub async fn mutate<T>(&self, class: MutationClass, op: MutationName,
//!         f: impl FnOnce(&mut Ledgers) -> Result<T, Refusal>) -> Result<T, ChiefdError>;
//!     pub fn read<T>(&self, f: impl FnOnce(&LedgerSnapshot) -> T) -> T;    // never queues (§5.3)
//! }
//! ```
//!
//! **Built at M4:** [`CompanyDb`] (in [`writer`]) — the thread, the queue pair,
//! the `arc-swap` snapshot publication and the transaction wrapper — over the
//! scheduler state machine in [`queue`].
//!
//! # There is no lease here, twice over (Mandate 4)
//!
//! M8's `mutate_under`/lease typestate was deleted in arch Step 8: it shipped
//! with zero reachable callers — F4 — and §2.0(3) superseded its planned
//! consumer with admission-time single-writer enforcement — originally a local
//! owner-marker file (`chiefd_host::run_admission`), replaced by beacond's
//! `POST /v1/register` (E10-S3, #764; see `chiefd::beacon`).
//!
//! #739 P4 then re-created both under the name `runtime_writer_lease`, and they
//! were deleted again for the same reason plus a worse one. Nothing they
//! fenced was a race:
//!
//! * **Across processes** — `chiefd run` refuses to open a company's storage
//!   at all unless beacond admits it as that company's sole daemon
//!   (`chiefd::run`, "cannot prove single-writer admission"), judged by pid
//!   liveness rather than a TTL. That is the exact "two chiefd instances on
//!   one company" configuration the lease named as its reason to exist.
//! * **Within one process** — this actor is one thread and one connection per
//!   company, and every mutation is one `BEGIN IMMEDIATE`.
//! * **Across a runtime actuation** — which is not a transaction at all, so no
//!   lease taken around a `mutate` ever covered it. The converge cycle's
//!   durable single-flight claim (`converge_safety::begin_cycle`) is what
//!   serializes that, and every converge pass — attended or duty-driven —
//!   goes through it.
//!
//! It could not even refuse: the holder string was `pid-<this process>` at
//! every call site, and a same-holder re-claim is an extension, so within the
//! one admitted process the CAS always passed. Across processes it compared a
//! deadline minted against another process's `Instant` origin with its own,
//! which decides nothing. The 2026-07-31 ruling already settled that trade —
//! "a TTL is a guess; pid evidence is exact".
//!
//! This module keeps the vocabulary M7/M8 and the API crate compile against:
//! mutation classes, the scheduler's aging policy, the queue deadline constant,
//! and the unmintable [`BusyProof`].
//!
//! What this module deliberately does *not* offer: a `try_lock`, an `acquire`,
//! or any pre-retry busy error a caller could receive. Contention is queue
//! depth; waiting is what `.await` does (plan §5.2 item 1).

use std::fmt;
use std::time::Duration;

mod org_slice;
pub(crate) mod queue;
mod session_lifecycle;

pub use session_lifecycle::MaintenanceContext;
mod runtime_verbs;
mod writer;

pub use runtime_verbs::RUNTIME_VERB_NO_MANIFEST;

pub use writer::{ChangeFeedSink, CompanyDb, CurrentJob, OpenError, PublishBarrier, QueueSnapshot};

/// The single bounded wait in the whole system (plan §5.2 item 2).
///
/// A `mutate` whose turn does not come within this window resolves
/// `Busy`. There is exactly one such constant on purpose: a second one would
/// be a second policy nobody reviews.
///
/// # The client has to outlive this, and an instrument says so
///
/// This is a policy about how long chiefd may hold a mutation under contention,
/// and it is chiefd's to make. What is NOT negotiable is that whoever asked is
/// still listening when the answer arrives — and the answer here is either the
/// commit itself or the reaped `429 Busy`, the one status meaning "back off and
/// ask again".
///
/// `@chief/chiefing`'s `FetchTransport` used to abandon the request at 10s.
/// Nothing was reaped at 10s, so a mutation that waited 12s ran and committed
/// while its caller had already been told `kind: 'timeout'` — which
/// `isTransientChiefdError` deliberately treats as non-transient, so nothing
/// retried and nothing re-read. The caller believed the write did not happen;
/// the write happened.
///
/// `scripts/test/client-observable-wait.test.mjs` derives this constant from
/// `CompanyDb::open`'s own call site and fails if it comes within 2s of the
/// client's abort, in either direction. Change this number freely — the guard
/// will tell you what else has to move.
pub const MUTATION_QUEUE_DEADLINE: Duration = Duration::from_secs(30);

/// After this many consecutive `Small` ops, the scheduler admits one queued
/// `Reconcile`/`Normal` op (plan §5.2, "the scheduler ages").
pub const SMALL_OPS_BEFORE_AGING: u32 = 32;

/// …or after this much time, whichever comes first. Strict priority would let
/// a hot 28-agent fleet starve reconcile in the other direction.
pub const AGING_INTERVAL: Duration = Duration::from_secs(2);

/// Scheduling class of a queued mutation.
///
/// `Small` ops are the ones whose latency is load-bearing: goal publication
/// (the intent spool's replacement guarantee), assignment fences, receipts,
/// the DB half of `model.change`. They must never queue
/// behind a multi-second reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MutationClass {
    /// High-priority channel; bounded, short transactions only.
    Small,
    /// Ordinary mutations.
    Normal,
    /// Compute-then-apply: snapshot read, plan off-thread, short apply
    /// transaction that re-checks generations.
    Reconcile,
}

impl MutationClass {
    /// Whether this class rides the high-priority channel.
    #[must_use]
    pub fn is_priority(self) -> bool {
        matches!(self, Self::Small)
    }
}

/// The operation name recorded on the queue for tracing and for the
/// deterministic scheduler test. A `&'static str` because op names are a
/// closed set defined in source, never caller-supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationName(pub &'static str);

impl fmt::Display for MutationName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Proof that chiefd actually waited before reporting `Busy`.
///
/// The inner field is private and the constructor is crate-private, so no code
/// outside `chiefd-core` can build one — which makes
/// [`crate::ChiefdError::Busy`] unconstructible in handler crates.
///
/// Inside the crate the discipline is narrower still, and is pinned by
/// `busy_is_mintable_from_exactly_the_two_reviewed_wait_sites`: the only call
/// sites are the writer's queue-deadline sweep and the SQLite busy_timeout
/// classification — the two, and only two, places chiefd is allowed to
/// conclude "busy", both of them only after a wait that already happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusyProof {
    waited: Duration,
    site: &'static str,
}

impl BusyProof {
    /// Mint a proof. Crate-private on purpose: only the actor's queue deadline
    /// and the SQLite busy_timeout classification may conclude "busy", and
    /// only after waiting.
    pub(crate) fn after_waiting(waited: Duration, site: &'static str) -> Self {
        Self { waited, site }
    }

    /// How long chiefd waited before giving up.
    #[must_use]
    pub fn waited(&self) -> Duration {
        self.waited
    }

    /// Which wait produced it (`mutation-queue`, `sqlite-busy-timeout`).
    #[must_use]
    pub fn site(&self) -> &'static str {
        self.site
    }
}

impl fmt::Display for BusyProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} after waiting {} ms", self.site, self.waited.as_millis())
    }
}

/// The scheduler's anti-starvation rule, extracted so M4's deterministic
/// scheduler test can drive it on a virtual clock with no threads at all.
#[derive(Debug, Clone, Copy)]
pub struct AgingPolicy {
    /// Consecutive `Small` ops tolerated before a lower-priority op is let in.
    pub small_ops_before_aging: u32,
    /// Wall budget tolerated before a lower-priority op is let in.
    pub aging_interval: Duration,
}

impl Default for AgingPolicy {
    fn default() -> Self {
        Self { small_ops_before_aging: SMALL_OPS_BEFORE_AGING, aging_interval: AGING_INTERVAL }
    }
}

impl AgingPolicy {
    /// Given the number of `Small` ops admitted back-to-back and how long the
    /// oldest lower-priority op has been waiting, should the scheduler admit
    /// that lower-priority op next?
    #[must_use]
    pub fn should_admit_aged(self, consecutive_small: u32, waited: Duration) -> bool {
        consecutive_small >= self.small_ops_before_aging || waited >= self.aging_interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_is_the_only_priority_class() {
        assert!(MutationClass::Small.is_priority());
        assert!(!MutationClass::Normal.is_priority());
        assert!(!MutationClass::Reconcile.is_priority());
    }

    #[test]
    fn aging_admits_after_the_op_count_even_with_no_elapsed_time() {
        let policy = AgingPolicy::default();
        assert!(!policy.should_admit_aged(31, Duration::ZERO));
        assert!(policy.should_admit_aged(32, Duration::ZERO));
    }

    #[test]
    fn aging_admits_after_the_interval_even_under_a_slow_small_stream() {
        let policy = AgingPolicy::default();
        assert!(!policy.should_admit_aged(1, Duration::from_millis(1999)));
        assert!(policy.should_admit_aged(1, Duration::from_secs(2)));
    }

    /// The private-constructor rule, asserted against source text.
    ///
    /// TESTING.md §3.1 asks for a `trybuild` compile-fail test proving handler
    /// crates cannot construct `ChiefdError::Busy`. That is worth having, but it
    /// only covers *outside* the crate, and the three historical regressions
    /// were written by people editing the lock code itself. This guard is the
    /// stronger half and needs no new dependency: it pins that the constructor
    /// is `pub(crate)` and that the whole crate mints `Busy` from exactly the
    /// reviewed wait sites. A new `after_waiting(` call site fails this test and
    /// has to argue for itself in review.
    #[test]
    fn busy_is_mintable_from_exactly_the_two_reviewed_wait_sites() {
        // Test modules are cut off first: a test is *supposed* to name the
        // constructor, and this guard is about production code.
        fn production_half(text: &str) -> &str {
            text.split("#[cfg(test)]").next().unwrap_or(text)
        }
        let sources: Vec<(&str, &str)> = vec![
            ("actor/mod.rs", production_half(include_str!("mod.rs"))),
            ("actor/queue.rs", production_half(include_str!("queue.rs"))),
            ("actor/writer.rs", production_half(include_str!("writer.rs"))),
            ("error.rs", production_half(include_str!("../error.rs"))),
        ];

        let declaration = sources
            .iter()
            .filter(|(_, text)| text.contains("fn after_waiting("))
            .collect::<Vec<_>>();
        assert_eq!(declaration.len(), 1, "the Busy constructor is declared once");
        assert!(
            declaration[0].1.contains("pub(crate) fn after_waiting("),
            "the Busy constructor must stay crate-private: a `pub` one lets any \
             handler mint a fail-fast Busy, which is the exact shape that shipped \
             three times in the TS system"
        );

        // Count real call sites — the declaration itself and doc/comment
        // mentions do not count.
        let call_sites: Vec<(&str, usize)> = sources
            .iter()
            .map(|(name, text)| {
                let count = text
                    .lines()
                    .filter(|line| {
                        let trimmed = line.trim_start();
                        !trimmed.starts_with("//")
                            && !trimmed.starts_with("///")
                            && line.contains("BusyProof::after_waiting(")
                    })
                    .count();
                (*name, count)
            })
            .filter(|(_, count)| *count > 0)
            .collect();

        // M4 owns both: the queue-deadline sweep, and the SQLite busy_timeout
        // classification (which is also strictly post-wait). M8's third site
        // (the lease contention ladder) was deleted with `lease.rs` in arch
        // Step 8 (F4: zero reachable callers).
        assert_eq!(
            call_sites,
            vec![("actor/queue.rs", 1), ("actor/writer.rs", 1)],
            "Busy may only be minted where chiefd has provably already waited"
        );
    }

    #[test]
    fn busy_proof_records_the_wait_it_is_proof_of() {
        let proof = BusyProof::after_waiting(Duration::from_millis(4250), "mutation-queue");
        assert_eq!(proof.waited(), Duration::from_millis(4250));
        assert_eq!(proof.site(), "mutation-queue");
        assert_eq!(proof.to_string(), "mutation-queue after waiting 4250 ms");
    }
}
