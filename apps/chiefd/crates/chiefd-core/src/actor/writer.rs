//! `CompanyDb` — the per-company writer actor.
//!
//! One OS thread per company owns that company's `rusqlite::Connection` and is
//! the only thing in the process that may write it. Every mutation runs on that
//! thread, **exactly once**, inside one `BEGIN IMMEDIATE … validate … COMMIT`.
//!
//! # The property this exists to create
//!
//! Plan §0: *"chiefd is the sole writer to every store, so optimistic
//! caller-side CAS with re-invoked mutators disappears."* The TS system's #1
//! port hazard was CAS-mutator purity: a mutator could be re-invoked after a
//! lost compare-and-swap, so any side effect inside it happened twice, and any
//! decision it made from stale reads was silently re-derived. Here a mutation
//! closure is handed `&mut Ledgers` on the writer thread with no other writer
//! in existence; there is no CAS to lose and therefore no re-invocation. The
//! `exactly_once_*` tests below pin it, including in the case that used to be
//! the trap — the caller going away mid-flight.
//!
//! Note the deliberate scope limit (plan §0): one transaction covers **one
//! database**. It does not cover DB↔filesystem or registry↔company-DB pairs;
//! those get host transactions (§5.6) and lifecycle intents (§5.7) in M9/M16.
//! "One writer, one transaction" is never cited as covering anything else.
//!
//! # Waiting is structural
//!
//! There is no `try_lock`, no `acquire`, no `busy` a caller can receive before
//! chiefd has waited. [`CompanyDb::mutate`] enqueues and awaits a oneshot with
//! **no timeout of its own**; the only code that can conclude `Busy` is the
//! writer's own scheduler sweep, after a job has sat in the queue for the full
//! [`MUTATION_QUEUE_DEADLINE`]. Contention is queue depth; waiting is what
//! `.await` does (plan §5.2 item 1).

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use arc_swap::ArcSwap;
use rusqlite::{Connection, TransactionBehavior};
use tokio::sync::oneshot;

use crate::actor::queue::{
    Admission, FinishFn, Job, Next, PostCommitFn, QueueState, TxnFn, QUEUE_BUSY_SITE,
};
use crate::actor::{AgingPolicy, BusyProof, MutationClass, MutationName, MUTATION_QUEUE_DEADLINE};
use crate::clock::{SharedClock, WallMillis};
use crate::error::ChiefdError;
use crate::error::{corrupt_store, store_failure, store_failure_because};
use crate::host_action::{HostActionPhase, HostActionRecord};
use crate::ledger::{DocumentRecord, EffectRow, LedgerSnapshot, Ledgers, MailboxRow, Validate};
use crate::store::open_company_db;

/// The store name reported when the company database itself fails.
///
/// A durable write that neither committed nor produced a [`Refusal`] is not a
/// refusal, a conflict or a busy signal — it is the store being unusable, which
/// the closed taxonomy calls `StoreFailure` (plan §1, §7.2 per-company isolation).
const COMPANY_DB_STORE: &str = "company-db";
const AUTH_IDENTITIES_STORE: &str = "auth-identities";

/// Failure opening a company database.
///
/// Deliberately not a [`ChiefdError`]: opening happens during the startup
/// recovery pass, where the plan's answer is per-company isolation (§7.2) —
/// M16 decides whether a given failure becomes `StoreFailure{store}` for one company
/// or aborts the daemon. Encoding that policy here would hide it.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// SQLite could not open the file, apply the schema, or read the ledger.
    #[error("opening company database failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The writer thread could not be spawned.
    #[error("spawning the writer thread failed: {0}")]
    Spawn(#[source] std::io::Error),
    /// A durable journal row could not be read back.
    ///
    /// Separate from [`OpenError::Sqlite`] because it is not a storage
    /// failure: SQLite handed over a perfectly good row whose *content* is
    /// unintelligible. Journals are fail-closed (plan §5.5), so this refuses
    /// the open rather than dropping the row.
    #[error("company journal is unreadable: {detail}")]
    CorruptJournal {
        /// Which row, and what about it could not be read.
        detail: String,
    },
}

struct Shared {
    /// The company slug — duplicated off [`CompanyDb::label`] because
    /// `run_job` (which needs it to call `feed_sink`) only ever sees `Shared`,
    /// never the owning `CompanyDb`.
    label: String,
    queue: Mutex<QueueState>,
    wake: Condvar,
    snapshot: ArcSwap<LedgerSnapshot>,
    clock: SharedClock,
    aging: AgingPolicy,
    deadline: Duration,
    /// The mutation the writer is actively running right now, if any (E8-S2,
    /// #824 — `CompanyDb::queue_snapshot`'s "current" diagnostic). Set by
    /// `writer_loop` immediately before `run_job`, on the SAME writer thread
    /// that already owns every commit — a second small `Mutex` with the
    /// identical discipline `queue` already has (held for a few statements,
    /// never across an `.await`), not a new lock protocol.
    ///
    /// Cleared by `run_job` itself, inside its wrapped `finish`, immediately
    /// *before* the caller is notified of the job's outcome (#905) — not by
    /// `writer_loop` after `run_job` returns. `finish`'s oneshot `send` is a
    /// synchronizes-with edge; a clear placed anywhere *after* that send has
    /// already fired carries no ordering guarantee relative to it, so an
    /// awoken caller could observe "the job resolved" on one thread while
    /// `current` (and therefore `queue_snapshot`'s `depth`) still counted
    /// that same job on another. Clearing inside the same closure that calls
    /// `finish`, strictly before it, is what makes "the caller learned the
    /// outcome" and "the diagnostic no longer counts this job" the same
    /// observable fact rather than two racing ones.
    current: Mutex<Option<CurrentJob>>,
    /// #376: the change-feed publish hook, installed post-construction (see
    /// [`CompanyDb::set_change_feed_sink`]) because in production
    /// (`chiefd`'s `run_company`) the docstore/`ChangeFeed` surface binds
    /// *after* `CompanyDb::open`. `None` — the default, and every existing
    /// caller's behavior — means commits publish nothing, exactly as before
    /// this hook existed.
    ///
    /// A plain `Mutex` rather than `arc_swap`'s `ArcSwapOption` (used for
    /// `snapshot` above): `arc-swap`'s `RefCnt` impl for `Arc<T>` requires
    /// `T: Sized`, which a `dyn Fn` trait object never is. The lock is taken
    /// at most once per commit (never across an `.await`, never contended
    /// against another writer — this actor is the only writer), so it costs
    /// nothing the queue's own `Mutex<QueueState>` doesn't already pay.
    feed_sink: Mutex<Option<Arc<ChangeFeedSink>>>,
}

/// A hook `run_job` calls once per changed/removed `documents` row after a
/// successful commit: `(company_label, store, body, commit_id, updated_at,
/// removed)`. `body` is the store's full serialized JSON on a change, and an
/// empty string on a removal (there is no content to carry). The
/// `(company_label, store, commit_id, updated_at, removed)` tail mirrors
/// `chiefd-api::docstore::ChangeFeed::publish`'s own argument shape
/// byte-for-byte so a caller can close over a real `Arc<ChangeFeed>` with a
/// one-line adapter — see `set_change_feed_sink`'s doc comment for why the
/// type lives here rather than as a direct dependency on `ChangeFeed` itself.
/// `body` was added for #372: a change-feed-only sink cannot mirror content
/// into a second durable document copy, only announce that
/// something changed, so a sink that ALSO wants to keep that table fresh
/// needs the actual bytes, not just a hint.
///
/// `chiefd-core` cannot name `chiefd_api::docstore::ChangeFeed` directly:
/// `chiefd-api` already depends on `chiefd-core` (the wire layer sits above
/// the store layer), so the reverse dependency would be circular. This trait
/// object is the seam instead — the `chiefd` binary crate (the one crate
/// that depends on both) is where a real `Arc<ChangeFeed>` gets closed over
/// into one of these (see `chiefd`'s `run.rs::wire_change_feed`).
///
/// `commit_id` is the actor's immutable identity for this committed change. It
/// fills the feed's historical cursor slot without reintroducing a
/// mutable per-record version. `updated_at` is the caller-supplied ISO-8601 string — the exact shape
/// `ChangeFeed::publish` takes — so both emission points (the `run_job`
/// document fan-out AND the [`CompanyDb::publish_row_feed_hint`] row-write
/// path) hand the feed the same wire value. The `run_job` site converts its
/// `WallMillis` via `WallMillis::to_iso8601`; the row-write path already holds
/// the caller's ISO stamp. A removal carries `""` (no clock — see
/// `chiefd-api::docstore::feed`'s wire-shape contract).
pub type ChangeFeedSink = dyn Fn(&str, &str, &str, &str, bool) + Send + Sync;

/// Work that must run after a mutation is durable and before its change event
/// is published — the only window where the two can be made to agree.
///
/// Carries a boxed `FnOnce` because the sole use is promoting a staged
/// company build onto its final paths, which owns the staging directory.
/// [`PublishBarrier::none`] is the ordinary case: nothing to do.
pub struct PublishBarrier(Option<Box<dyn FnOnce() + Send>>);

impl PublishBarrier {
    /// No barrier — the mutation publishes as soon as it is durable.
    #[must_use]
    pub const fn none() -> Self {
        Self(None)
    }

    /// Run `action` in the post-commit, pre-publish window.
    #[must_use]
    pub fn new(action: Box<dyn FnOnce() + Send>) -> Self {
        Self(Some(action))
    }

    fn run(self) {
        if let Some(action) = self.0 {
            action();
        }
    }
}

impl std::fmt::Debug for PublishBarrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PublishBarrier")
            .field(&if self.0.is_some() { "pending" } else { "none" })
            .finish()
    }
}

/// The job the writer is running right now ([`CompanyDb::queue_snapshot`],
/// E8-S2, #824).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentJob {
    /// The mutation's stable name.
    pub name: MutationName,
    /// Its priority class.
    pub class: MutationClass,
    /// How long this job waited in the queue before it started running.
    pub enqueued_ms: u64,
}

/// A read-only view of the writer's queue ([`CompanyDb::queue_snapshot`],
/// E8-S2, #824): diagnostics only, never a mutation, never a lock a caller
/// can contend against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueSnapshot {
    /// Jobs accepted by the writer and not yet committed, including the
    /// actively-running [`Self::current`] job when there is one.
    pub depth: usize,
    /// Age of the oldest still-queued job, in milliseconds. `0` when idle.
    pub oldest_enqueued_ms: u64,
    /// The queue deadline this writer was opened with
    /// ([`crate::actor::MUTATION_QUEUE_DEADLINE`] in production).
    pub deadline_ms: u64,
    /// The job the writer is actively running right now, if any.
    pub current: Option<CurrentJob>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        // A poisoned queue means a previous job's `finish` panicked while the
        // lock was held. The queue itself is plain data and is still coherent,
        // so recovering the guard is strictly better than taking the whole
        // company down — and `unwrap` is denied in this workspace for exactly
        // this reason.
        self.queue.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

/// One company's durable state and the single thread that writes it.
///
/// Construct with [`CompanyDb::open`]; share by wrapping in an `Arc`. Dropping
/// it quiesces the actor, checkpoints the WAL and joins the thread.
pub struct CompanyDb {
    label: String,
    shared: Arc<Shared>,
    readers: Arc<ReaderPool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// How many company reads may execute at the same time.
///
/// # Why a number at all, and why this one
///
/// Reads used to be capped at ONE, because they ran on the writer thread. The
/// cap was never a decision — it was a side effect of borrowing the writer's
/// connection. But unbounded is not the answer either: an unbounded reader
/// count turns a client-side burst into an unbounded thread count, and SQLite
/// page-cache pressure grows with it.
///
/// Four is the number of readers that can actually make progress at once on
/// the operator's box, and a bounded pool is what converts a burst into a
/// bounded wait instead of an unbounded pile-up. The semaphore is therefore
/// admission control as much as it is a resource limit — the property this
/// module lost when `BEGIN IMMEDIATE` stopped throttling callers for it.
const COMPANY_READERS: usize = 8;

/// A bounded pool of READ-ONLY connections to one company's database.
///
/// # Why this type exists
///
/// `SQLITE_OPEN_READ_ONLY` is what makes this safe, and it is not a comment:
/// every write on these connections fails at the SQLite layer, so the pool
/// cannot become a second writer no matter what a future caller hands it.
/// Mandate 4's subject is who may WRITE, and that is still exactly one thread.
///
/// A reader is checked out under a permit and returned on every path.
struct ReaderPool {
    conns: Mutex<Vec<Connection>>,
    permits: tokio::sync::Semaphore,
}

impl ReaderPool {
    /// Open `size` read-only connections to an ALREADY-CREATED database.
    ///
    /// # Errors
    /// Propagates any `rusqlite` failure; a read-only open never creates, so a
    /// missing file is reported rather than conjured.
    fn open(path: &Path, size: usize) -> Result<Self, OpenError> {
        let mut conns = Vec::with_capacity(size);
        for _ in 0..size {
            conns.push(crate::store::open_company_db_readonly(path)?);
        }
        Ok(Self { conns: Mutex::new(conns), permits: tokio::sync::Semaphore::new(size) })
    }

    /// Run `f` against one pooled reader, on a BLOCKING thread.
    ///
    /// `spawn_blocking` is not optional here. A pooled read is synchronous
    /// SQLite work, and running it inline would park a tokio worker for its
    /// whole duration — four concurrent reads would then be four workers not
    /// serving HTTP, which is the starvation this pool exists to end, moved
    /// one layer up.
    ///
    /// The connection is MOVED to the blocking thread and moved back, so it is
    /// touched by exactly one thread at a time — the discipline
    /// `SQLITE_OPEN_NO_MUTEX` requires. It is returned to the pool on the
    /// refusal path too, so a read that fails does not shrink the pool.
    ///
    /// # Errors
    /// Whatever `f` refuses, plus [`ChiefdError::Unavailable`] if the pool has
    /// been closed or the blocking thread was cancelled.
    async fn checkout<T, F>(self: &Arc<Self>, f: F) -> Result<T, ChiefdError>
    where
        F: FnOnce(&Connection) -> Result<T, ChiefdError> + Send + 'static,
        T: Send + 'static,
    {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| ChiefdError::Unavailable { reason: "reader-pool-closed" })?;
        // A poisoned mutex means a reader panicked mid-checkout; the vector is
        // only pushed and popped, so recover the guard rather than taking the
        // company down with it.
        let taken = match self.conns.lock() {
            Ok(mut g) => g.pop(),
            Err(poisoned) => poisoned.into_inner().pop(),
        };
        let Some(conn) = taken else {
            // Unreachable while the permit count and the vector length agree;
            // reported rather than unwrapped so a future divergence is loud.
            return Err(ChiefdError::Unavailable { reason: "reader-pool-exhausted" });
        };
        let pool = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let out = f(&conn);
            match pool.conns.lock() {
                Ok(mut g) => g.push(conn),
                Err(poisoned) => poisoned.into_inner().push(conn),
            }
            out
        })
        .await
        .unwrap_or(Err(ChiefdError::Unavailable { reason: "reader-cancelled" }))
    }
}

impl std::fmt::Debug for CompanyDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompanyDb")
            .field("label", &self.label)
            .field("commit_seq", &self.shared.snapshot.load().commit_seq())
            .field("queue_depth", &self.shared.lock().depth())
            .finish()
    }
}

impl CompanyDb {
    /// Open `<dataRoot>/<slug>/chief.db`, load its ledger, and start the writer.
    ///
    /// `label` is the company slug; it names the thread and appears in traces.
    ///
    /// # Errors
    /// [`OpenError`] if SQLite cannot open or initialize the database, or if
    /// the writer thread cannot be spawned.
    pub fn open(label: &str, path: &Path, clock: SharedClock) -> Result<Self, OpenError> {
        Self::open_with(label, path, clock, AgingPolicy::default(), MUTATION_QUEUE_DEADLINE)
    }

    /// [`CompanyDb::open`] with an explicit scheduling policy.
    ///
    /// The constants are not per-company policy in production — plan §5.2 keeps
    /// them as one reviewed set. This entry point exists so the deterministic
    /// scheduler tests can drive the same code with a short deadline instead of
    /// waiting thirty seconds, and so a future operator override lands in one
    /// place rather than as scattered literals.
    ///
    /// # Errors
    /// As [`CompanyDb::open`].
    pub fn open_with(
        label: &str,
        path: &Path,
        clock: SharedClock,
        aging: AgingPolicy,
        deadline: Duration,
    ) -> Result<Self, OpenError> {
        let mut conn = open_company_db(path)?;
        // An external holder of the file lock (during Phases 1–4 there still is
        // one) makes SQLite report BUSY. We wait it out here rather than
        // surfacing it, so that even a `Busy` produced by SQLite itself is a
        // post-wait fact — see `sqlite_busy_is_reported_as_a_post_wait_busy`.
        conn.busy_timeout(deadline)?;
        let ledgers = load_ledgers(&conn, label, clock.wall())?;
        // #123: per-commit validation only re-parses the bodies a commit
        // changed, so an untouched-but-corrupt on-disk body would otherwise
        // never be re-parsed. Validate the whole ledger ONCE here at load, so
        // on-disk corruption is caught loudly at open (fail-closed, plan §5.5)
        // exactly as the first post-open mutation used to catch it.
        if let Err(refusal) = ledgers.validate() {
            return Err(OpenError::CorruptJournal {
                detail: format!("{}: {}", refusal.code, refusal.message),
            });
        }

        let shared = Arc::new(Shared {
            label: label.to_string(),
            queue: Mutex::new(QueueState::new()),
            wake: Condvar::new(),
            snapshot: ArcSwap::from_pointee(LedgerSnapshot::committed(ledgers, 0)),
            clock,
            aging,
            deadline,
            current: Mutex::new(None),
            feed_sink: Mutex::new(None),
        });

        // AFTER `open_company_db` above, which is what creates and migrates the
        // file: a read-only open never creates, so the order is load-bearing.
        let readers = Arc::new(ReaderPool::open(path, COMPANY_READERS)?);

        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name(format!("chiefd-writer-{label}"))
            .spawn(move || writer_loop(&thread_shared, &mut conn))
            .map_err(OpenError::Spawn)?;

        Ok(Self { label: label.to_string(), shared, readers, thread: Mutex::new(Some(thread)) })
    }

    /// The company slug this actor writes for.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The company's injected clock.
    ///
    /// Exposed so a caller outside a mutation can read the SAME time the
    /// ledger stamps — the HTTP surface has no clock of its own, and reaching
    /// for `SystemTime::now()` there would put it on a different timeline from
    /// every duty that later compares against it. Under a
    /// [`ManualClock`](crate::test_support::ManualClock) both ends move
    /// together, which is what lets a test drive a timing rule by advancing
    /// time instead of by waiting (TESTING.md §4.2).
    #[must_use]
    pub fn clock(&self) -> &crate::clock::SharedClock {
        &self.shared.clock
    }

    /// A read-only view of the writer's queue (E8-S2, #824) — the
    /// diagnostics `GET /v1/docs/queue` exposes as the "is something stuck?"
    /// break-glass that replaces `org lock list` once E8-S6 deletes the file
    /// locks it read. Never blocks, never enqueues a job, never touches
    /// SQLite: it reads `Shared`'s own `Mutex<QueueState>` and
    /// `Mutex<Option<CurrentJob>>`, released before returning.
    #[must_use]
    pub fn queue_snapshot(&self) -> QueueSnapshot {
        let now = self.shared.clock.monotonic();
        // Lock in the same order `writer_loop` uses when it moves a job from
        // queued to current. That makes the public diagnostic one coherent
        // observation: it never reports the actor idle in the hand-off gap
        // between taking a job and beginning its transaction.
        let (queued_depth, oldest_enqueued_ms, current) = {
            let queue = self.shared.lock();
            let current = *self.shared.current.lock().unwrap_or_else(|poison| poison.into_inner());
            (queue.depth(), queue.oldest_wait_ms(now), current)
        };
        QueueSnapshot {
            depth: queued_depth + if current.is_some() { 1 } else { 0 },
            oldest_enqueued_ms,
            deadline_ms: u64::try_from(self.shared.deadline.as_millis()).unwrap_or(u64::MAX),
            current,
        }
    }

    /// Install (or replace) the change-feed publish hook (#376).
    ///
    /// Called after every future `run_job` commit, once per changed
    /// `documents` row (see `run_job`'s doc comment for the exact call
    /// shape). Post-construction rather than an `open`/`open_with` parameter
    /// because production wiring (`chiefd`'s `run_company`) opens the
    /// docstore/`ChangeFeed` surface *after* `CompanyDb::open` — see
    /// `resolve_company_db_path`'s doc comment in `run.rs` for why the two
    /// opens are ordered that way. A company that never calls this (every
    /// existing test harness, `chiefd run --once`, or a boot with
    /// `CHIEFD_STORE_DB_PATH` unset) keeps today's behavior exactly: commits
    /// publish nothing.
    ///
    /// #368's reactive duty scheduler is meant to subscribe to the SAME
    /// `ChangeFeed` a caller closes over here, not stand up a second one —
    /// see the `ChangeFeedSink` doc comment.
    pub fn set_change_feed_sink(&self, sink: Arc<ChangeFeedSink>) {
        *self.shared.feed_sink.lock().unwrap_or_else(|p| p.into_inner()) = Some(sink);
    }

    /// Publish a `ChangeFeed` hint for a NORMALIZED-ROW write that bypasses the
    /// `Ledgers` snapshot (the [`CompanyDb::in_transaction`] path — mailbox and
    /// operator-escalation).
    ///
    /// The 19-hour-silent-mail-outage class (and
    /// CLAUDE.md's reactive-never-polling rule): `run_job`'s post-commit fan-out
    /// only publishes for stores whose bodies live in `Ledgers`
    /// (`changed_since`). A store flipped to `store::*_rows` writes its rows +
    /// an `org_events` row inside `in_transaction` and never touches `Ledgers`,
    /// so it emits NO `WatchEvent` — the TS `createOrganizationMailboxWakeWatcher`
    /// (`stores: ["*"]`) never fires and the wake degrades to poll-only. This
    /// closes that gap: a row write publishes the SAME `WatchEvent` shape a
    /// `documents` write would, so existing `/v1/docs/watch` subscribers keep
    /// matching byte-for-byte on `event.store`.
    ///
    /// Emitted on the CALLER's async task AFTER the write commits — the exact
    /// emission model `DocStore`'s own writes use (see
    /// `chiefd-api::docstore::feed`'s "Emission point"): `ChangeFeed::publish`
    /// serializes `seq`/ring/broadcast under its own lock, so concurrent
    /// row-writers stay strictly seq-ordered. Row writes are never store
    /// removals, so `removed` is always `false` (a `dropCompanyStore` still
    /// rides the `documents` path). A no-sink company is a single `Option`
    /// check, exactly as before.
    pub(crate) fn publish_row_feed_hint(&self, store: &str, updated_at_iso: &str) {
        let sink = self.shared.feed_sink.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(sink) = sink {
            sink(&self.shared.label, store, "", updated_at_iso, false);
        }
    }

    /// Run `f` against the last committed snapshot, synchronously.
    ///
    /// **Never queues** (plan §5.3): `org.roster`, `activity.status`,
    /// `health.status` and `lifecycle_status` do not stall behind a multi-second
    /// reconcile or a 4.4 MB serialize. The snapshot is committed state only, so
    /// a reader is at most one in-flight mutation stale — the same guarantee
    /// HTTP callers had against the file stores.
    pub fn read<T>(&self, f: impl FnOnce(&LedgerSnapshot) -> T) -> T {
        f(&self.shared.snapshot.load())
    }

    /// The current committed snapshot as an owned `Arc`.
    #[must_use]
    pub fn snapshot(&self) -> Arc<LedgerSnapshot> {
        self.shared.snapshot.load_full()
    }

    /// Run `f` on the writer thread inside one transaction.
    ///
    /// `f` runs **exactly once** — including when this future is dropped
    /// mid-await. Once enqueued, a mutation is the actor's obligation, not the
    /// caller's; a client that disconnects does not un-commit a transaction. It
    /// also does not double-apply one, which is what re-invoked CAS mutators
    /// did.
    ///
    /// Ordering guarantee: `f` sees the state left by every mutation that
    /// committed before it was admitted, never a partially applied one.
    ///
    /// # Errors
    /// - `Refused` — `f` declined, or `validate()` rejected the resulting state.
    ///   The transaction rolled back and the in-memory ledger is untouched.
    /// - `Busy` — the job waited [`MUTATION_QUEUE_DEADLINE`] in the queue
    ///   without being admitted, and was never run. Minted only by the writer,
    ///   only after that wait.
    /// - `Unavailable` — the actor is quiescing (removal) or has stopped.
    /// - `StoreFailure` — the database itself failed the write.
    pub async fn mutate<T, F>(
        &self,
        class: MutationClass,
        op: MutationName,
        f: F,
    ) -> Result<T, ChiefdError>
    where
        F: FnOnce(&mut Ledgers) -> Result<T, ChiefdError> + Send + 'static,
        T: Send + 'static,
    {
        self.enqueue(None, None, class, op, f).await
    }

    async fn enqueue<T, F>(
        &self,
        txn: Option<TxnFn>,
        post_commit: Option<PostCommitFn>,
        class: MutationClass,
        op: MutationName,
        f: F,
    ) -> Result<T, ChiefdError>
    where
        F: FnOnce(&mut Ledgers) -> Result<T, ChiefdError> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel::<Result<T, ChiefdError>>();

        // The closure's return value cannot travel through the type-erased job,
        // so it is parked here and collected by `finish` after the commit that
        // makes it true. On any non-commit path the slot is simply never read.
        let slot: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let apply_slot = Arc::clone(&slot);

        let job = Job {
            class,
            name: Some(op),
            enqueued_at: self.shared.clock.monotonic(),
            txn,
            post_commit,
            apply: Box::new(move |ledgers| {
                let value = f(ledgers)?;
                *apply_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(value);
                Ok(())
            }),
            finish: Box::new(move |outcome| {
                let answer = match outcome {
                    Ok(()) => slot
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .take()
                        .ok_or(ChiefdError::Unavailable { reason: "writer-lost-result" }),
                    Err(err) => Err(err),
                };
                // The receiver is gone when the caller dropped its future. The
                // mutation already committed; there is nobody left to tell.
                let _ = tx.send(answer);
            }),
        };

        self.shared.lock().push(job)?;
        self.shared.wake.notify_one();

        // No timeout here, deliberately. A caller cannot bound its own wait,
        // so it cannot obtain a fail-fast acquisition by accident.
        rx.await.unwrap_or_else(|_| Err(ChiefdError::Unavailable { reason: "writer-stopped" }))
    }

    /// Run `f` directly against one `BEGIN IMMEDIATE`, with no ledger involved.
    ///
    /// Crate-private, and deliberately so: `host_actions` is relational rather than a
    /// `documents` row, so the DB↔filesystem 2PC needs the
    /// transaction itself. Everything a *caller* can reach still goes through
    /// [`CompanyDb::mutate`], so this is not a second write path into company
    /// state — it is the same writer thread, the same queue, the same
    /// scheduling class and the same single transaction.
    /// Run `f` inside one transaction.
    ///
    /// `pub` rather than `pub(crate)`: `chiefd-host`'s runtime-lifecycle verbs
    /// are a separate crate and need the same single-transaction guarantee
    /// every in-crate store gets. Widening the visibility is the honest move —
    /// the alternative was a second transaction mechanism outside the actor,
    /// which is what Mandate 4 exists to prevent.
    pub async fn in_transaction<T, F>(
        &self,
        class: MutationClass,
        op: MutationName,
        f: F,
    ) -> Result<T, ChiefdError>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, ChiefdError> + Send + 'static,
        T: Send + 'static,
    {
        let slot: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let fill = Arc::clone(&slot);
        let step: TxnFn = Box::new(move |txn| {
            let value = f(txn)?;
            *fill.lock().unwrap_or_else(|p| p.into_inner()) = Some(value);
            Ok(())
        });
        self.enqueue(Some(step), None, class, op, move |_ledgers| {
            slot.lock().unwrap_or_else(|p| p.into_inner()).take().ok_or_else(|| {
                ChiefdError::refused("txn-step-lost-result", "transaction step produced no value")
            })
        })
        .await
    }

    /// Run a raw-SQL step and a ledger step in ONE commit.
    ///
    /// The session-lifecycle ports need both halves at once: a drain reads and
    /// deletes queue ROWS while folding their content into the supervision
    /// LEDGER, and a fresh-session handoff moves the supervision ledger and the
    /// maintenance ledger together. Splitting either across two commits would
    /// recreate exactly the half-state Mandate 4 exists to forbid — an intent
    /// committed but still queued, or a cursor advanced with no WAL record
    /// naming it.
    ///
    /// `sql` runs first, inside the same `BEGIN IMMEDIATE`; whatever it returns
    /// is handed to `ledger`. A refusal from either aborts the whole commit.
    pub(crate) async fn in_transaction_and_mutate<A, T, S, L>(
        &self,
        class: MutationClass,
        op: MutationName,
        sql: S,
        ledger: L,
    ) -> Result<T, ChiefdError>
    where
        S: FnOnce(&rusqlite::Transaction<'_>) -> Result<A, ChiefdError> + Send + 'static,
        L: FnOnce(A, &mut Ledgers) -> Result<T, ChiefdError> + Send + 'static,
        A: Send + 'static,
        T: Send + 'static,
    {
        let carried: Arc<Mutex<Option<A>>> = Arc::new(Mutex::new(None));
        let fill = Arc::clone(&carried);
        let step: TxnFn = Box::new(move |txn| {
            let value = sql(txn)?;
            *fill.lock().unwrap_or_else(|p| p.into_inner()) = Some(value);
            Ok(())
        });
        self.enqueue(Some(step), None, class, op, move |ledgers| {
            let carried =
                carried.lock().unwrap_or_else(|p| p.into_inner()).take().ok_or_else(|| {
                    ChiefdError::refused(
                        "txn-step-lost-result",
                        "transaction step produced no value",
                    )
                })?;
            ledger(carried, ledgers)
        })
        .await
    }

    /// Run one SQL read on a pooled READ-ONLY connection.
    ///
    /// # Why a read does not run on the writer thread
    ///
    /// It used to, and that is the defect this replaces. A read never took the
    /// writer's `BEGIN IMMEDIATE` after `read_txn` landed, but it still queued
    /// onto the writer's single thread and its single connection, so the server
    /// could serve exactly ONE read at a time however many were asked for.
    ///
    /// That cap was invisible while `in_transaction` was the read path, because
    /// `BEGIN IMMEDIATE` also throttled the CALLERS: a client could not get a
    /// second read in flight until the first released the reserved write lock.
    /// Measured on the operator's box, the old binary never exceeded THREE
    /// requests in flight. Making reads cheap removed the throttle but not the
    /// cap, and arrival concurrency went to 104 against a one-at-a-time server:
    /// median read latency doubled and p95 went from 576ms to 18,850ms. A
    /// single-server queue converts arrival concurrency straight into latency,
    /// so the faster reads got, the worse the box behaved.
    ///
    /// Reads run on their own read-only connections instead. SQLite in WAL mode
    /// serves any number of readers concurrently with the writer, so this is
    /// what the storage engine already supports and the actor was preventing.
    ///
    /// # Errors
    /// Whatever `f` refuses, plus [`ChiefdError::Unavailable`] if no reader can
    /// be obtained.
    async fn read_pooled<T, F>(&self, f: F) -> Result<T, ChiefdError>
    where
        F: FnOnce(&Connection) -> Result<T, ChiefdError> + Send + 'static,
        T: Send + 'static,
    {
        self.readers.checkout(f).await
    }

    /// Run `f` against one DEFERRED, rolled-back transaction on a pooled
    /// READ-ONLY connection. **The read path for anything that reads more than
    /// one row.**
    ///
    /// # Why this exists, and what it replaces
    ///
    /// Every `*_read` on this type used to go through [`Self::in_transaction`],
    /// which is the WRITE path: `BEGIN IMMEDIATE` — taking the database's
    /// reserved write lock — then a deep clone of the whole [`Ledgers`] (every
    /// mailbox envelope body and every effect row the company has ever
    /// accumulated), full-ledger validation, two structural diffs over those
    /// same rows, `persist`, a `COMMIT`, a changefeed publication and a new
    /// snapshot. A question that changes nothing paid all of it, and the cost
    /// was proportional to accumulated history rather than to what was asked.
    /// Measured on the operator's box: `/v1/org/runtime/desired` performs five
    /// such reads per request and ran at a 925ms median.
    ///
    /// A read takes a SHARED lock on its first statement and drops it on
    /// rollback. Nothing is written, so nothing is committed, so there is no
    /// WAL record, no fsync, no diff and no fan-out.
    ///
    /// # Why a transaction at all, rather than [`Self::read_pooled`]
    ///
    /// Because these reads are MULTI-STATEMENT — `organization_rows::
    /// reconstruct` alone issues `3 + 4N` — and a torn manifest is a worse
    /// answer than a slow one. The transaction is what makes the whole read one
    /// consistent observation, which is exactly the property `in_transaction`
    /// was being used for and the only part of it that was ever needed.
    ///
    /// It is rolled back explicitly rather than dropped, so a rollback failure
    /// is reported instead of swallowed.
    ///
    /// `pub` for the same reason [`Self::in_transaction`] is: `chiefd-host`'s
    /// runtime-lifecycle reads are a separate crate and must not open a
    /// connection of its own to get a consistent read.
    ///
    /// # Errors
    /// Whatever `f` refuses, plus [`ChiefdError::StoreFailure`] if the
    /// transaction cannot be opened or rolled back.
    pub async fn read_txn<T, F>(&self, f: F) -> Result<T, ChiefdError>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, ChiefdError> + Send + 'static,
        T: Send + 'static,
    {
        self.read_pooled(move |conn| {
            // `unchecked_transaction` is the `&Connection` form; a pooled reader
            // is checked out to one caller at a time, so the exclusivity
            // `transaction(&mut self)` exists to prove is already structural.
            let txn =
                conn.unchecked_transaction().map_err(|e| store_failure(COMPANY_DB_STORE, e))?;
            let value = f(&txn)?;
            txn.rollback().map_err(|e| store_failure(COMPANY_DB_STORE, e))?;
            Ok(value)
        })
        .await
    }

    /// Read one agent-auth identity on the pooled read path.
    ///
    /// `Ok(None)` is never enrolled; `Err` is a store this read could not look
    /// in. The auth runtime fails closed on both and REPORTS them differently
    /// — it used to flatten the fault into the absence, which answered every
    /// caller of every route `403 unknown identity` for the seven seconds one
    /// company's store was stalled (#1204).
    ///
    /// This one runs on EVERY authenticated request, so it is the read most
    /// damaged by a one-at-a-time read path: it put a whole extra actor visit
    /// in front of every route before the route's own reads even started.
    pub async fn identity_read(
        &self,
        identity_id: String,
    ) -> Result<Option<crate::store::identities::Identity>, ChiefdError> {
        self.read_pooled(move |conn| {
            crate::store::identities::get(conn, &identity_id)
                .map_err(|e| store_failure(AUTH_IDENTITIES_STORE, e))
        })
        .await
    }

    /// Enrol an identity in one actor-owned `BEGIN IMMEDIATE` transaction.
    ///
    /// A repeat carrying the same fingerprint is the bootstrap-safe no-op;
    /// the same id carrying a different fingerprint is an explicit conflict,
    /// never a silent re-key.
    pub async fn identity_enroll(
        &self,
        new: crate::store::identities::NewIdentity<'_>,
    ) -> Result<bool, ChiefdError> {
        let identity_id = new.identity_id.to_owned();
        let principal = new.principal.to_owned();
        let kind = new.kind;
        let company_slug = new.company_slug.map(str::to_owned);
        let pubkey = new.pubkey.map(str::to_owned);
        let fingerprint = new.fingerprint.to_owned();
        let enrolled_by = new.enrolled_by.map(str::to_owned);
        let now = self.shared.clock.wall().0;
        self.in_transaction(MutationClass::Small, MutationName("auth.identity.enroll"), move |tx| {
            let existing = crate::store::identities::get(tx, &identity_id)
                .map_err(|e| store_failure(AUTH_IDENTITIES_STORE, e))?;
            if let Some(existing) = existing {
                if existing.fingerprint == fingerprint {
                    return Ok(false);
                }
                return Err(ChiefdError::refused(
                    "auth-identity-fingerprint-conflict",
                    "identity already enrolled with a different key fingerprint",
                ));
            }
            let new = crate::store::identities::NewIdentity {
                identity_id: &identity_id,
                principal: &principal,
                kind,
                company_slug: company_slug.as_deref(),
                pubkey: pubkey.as_deref(),
                fingerprint: &fingerprint,
                enrolled_by: enrolled_by.as_deref(),
            };
            crate::store::identities::enroll(tx, &new, now)
                .map_err(|e| store_failure(AUTH_IDENTITIES_STORE, e))?;
            Ok(true)
        })
        .await
    }

    /// Rotate one enrolled identity's fingerprint in one actor-owned
    /// `BEGIN IMMEDIATE` transaction.
    pub async fn identity_rotate_fingerprint(
        &self,
        identity_id: String,
        fingerprint: String,
    ) -> Result<bool, ChiefdError> {
        self.in_transaction(
            MutationClass::Small,
            MutationName("auth.identity.rotate-fingerprint"),
            move |tx| {
                crate::store::identities::rotate_fingerprint(tx, &identity_id, &fingerprint)
                    .map(|changed| changed == 1)
                    .map_err(|e| store_failure(AUTH_IDENTITIES_STORE, e))
            },
        )
        .await
    }

    /// Revoke one enrolled identity in one actor-owned `BEGIN IMMEDIATE`
    /// transaction.
    pub async fn identity_revoke(&self, identity_id: String, at: i64) -> Result<bool, ChiefdError> {
        self.in_transaction(MutationClass::Small, MutationName("auth.identity.revoke"), move |tx| {
            crate::store::identities::revoke(tx, &identity_id, at)
                .map(|changed| changed == 1)
                .map_err(|e| store_failure(AUTH_IDENTITIES_STORE, e))
        })
        .await
    }

    /// Run one named organization mutation and refresh the live organization
    /// projection from the normalized rows before the next queued operation
    /// can observe it.
    ///
    /// The projection is reconstructed inside the mutation's own transaction,
    /// so an unreadable row aborts the whole write. It is installed only after
    /// that transaction commits, avoiding both a stale post-hire actor roster
    /// and a durable document/JSON fallback.
    async fn in_transaction_refreshing_org<T, F>(
        &self,
        class: MutationClass,
        op: MutationName,
        f: F,
    ) -> Result<T, ChiefdError>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, ChiefdError> + Send + 'static,
        T: Send + 'static,
    {
        self.in_transaction_refreshing_org_barriered(class, op, PublishBarrier::none(), f).await
    }

    /// [`Self::in_transaction_refreshing_org`] with work that must complete
    /// after the commit is durable and BEFORE any watcher can see it.
    ///
    /// The change-feed fan-out happens on the writer thread, after
    /// `txn.commit()` and before the caller's `await` resolves, so a route
    /// cannot get control back early enough to do this itself. A hire commits
    /// a person the `chief-cli` actuator immediately tries to converge — it
    /// parks on the `org-manifest` changefeed — so a person's home must be at
    /// its final path before that event exists, or the watcher races a launch
    /// against a directory that is not there yet.
    async fn in_transaction_refreshing_org_barriered<T, F>(
        &self,
        class: MutationClass,
        op: MutationName,
        barrier: PublishBarrier,
        f: F,
    ) -> Result<T, ChiefdError>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, ChiefdError> + Send + 'static,
        T: Send + 'static,
    {
        let slug = self.label().to_string();
        let projection: Arc<Mutex<Option<LiveOrganizationProjection>>> = Arc::new(Mutex::new(None));
        let projection_for_step = Arc::clone(&projection);
        let slot: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let fill = Arc::clone(&slot);
        let step: TxnFn = Box::new(move |txn| {
            let value = f(txn)?;
            let refreshed = LiveOrganizationProjection::reconstruct(txn, &slug)?;
            *projection_for_step.lock().unwrap_or_else(|p| p.into_inner()) = Some(refreshed);
            *fill.lock().unwrap_or_else(|p| p.into_inner()) = Some(value);
            Ok(())
        });
        let post_commit_projection = Arc::clone(&projection);
        let post_commit: PostCommitFn = Box::new(move |ledgers| {
            let projection =
                post_commit_projection.lock().unwrap_or_else(|p| p.into_inner()).take();
            // The mutation step always reconstructs the projection before the
            // transaction commits, so a missing value is unreachable; log
            // rather than panic (`panic` is denied workspace-wide).
            if let Some(projection) = projection {
                projection.install(ledgers);
            } else {
                tracing::error!("post-commit ran without a prepared organization projection");
            }
            // Durable, and still invisible: this is the only window in which
            // published state and on-disk state can be made to agree before
            // anybody is told either changed.
            barrier.run();
        });
        self.enqueue(Some(step), Some(post_commit), class, op, move |_ledgers| {
            slot.lock().unwrap_or_else(|p| p.into_inner()).take().ok_or_else(|| {
                ChiefdError::refused("txn-step-lost-result", "transaction step produced no value")
            })
        })
        .await
    }

    /// Republish the person operating contracts inside the transaction that
    /// just created a department, so the department and the contracts of the
    /// people it hired commit or roll back together.
    ///
    /// A department create hires a head and its initial workers, and
    /// the home writer needs a committed operating contract for `AGENTS.md`.
    /// Leaving the contracts
    /// to the post-commit rebuild meant a department could commit whole and its
    /// people still be unable to start, because the second transaction is a
    /// separate write that can fail on its own — the department exists, a
    /// derived fact it needs does not, and the caller was told `applied: true`.
    ///
    /// Company genesis already made this ruling for the identical situation one
    /// level up: `org_manifest_genesis` puts the contracts in its ONE
    /// transaction so a crash mid-genesis never leaves a company without its
    /// contracts. Creating a department is the same
    /// act at a lower level of the tree and now answers the same way.
    ///
    /// This is NOT a second writer of the same fact. It is the same
    /// [`crate::store::person_contracts::build::rebuild_person_contracts`] the
    /// boot path calls, and that function writes only when the text actually
    /// changed — so the post-commit rebuild that follows a create now finds
    /// nothing to do rather than writing the rows a second time.
    ///
    /// A refusal writes nothing, so nothing to republish.
    fn commit_person_contracts_with(
        outcome: &crate::store::org_ops::CreateDepartmentOutcome,
        tx: &rusqlite::Transaction<'_>,
        slug: &str,
        at: &str,
    ) -> Result<(), ChiefdError> {
        if !matches!(outcome, crate::store::org_ops::CreateDepartmentOutcome::Applied { .. }) {
            return Ok(());
        }
        crate::store::person_contracts::build::rebuild_person_contracts(tx, slug, at).map(|_| ())
    }

    /// Read the current normalized organization event fence without
    /// reconstructing any aggregate. Typed HTTP readers use this for
    /// `ifSeqNot`: an unchanged poll can omit a potentially multi-megabyte
    /// document and skip its row reconstruction/serialization entirely.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] when the event fence cannot be read.
    pub async fn org_current_seq(&self) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("org-events", e))
        })
        .await
    }

    /// Reconstruct the org manifest from the normalized rows, with the
    /// `org_events` seq fence the read observed (org-data-normalization P0, N2).
    /// `None` ⇒ the company has no manifest rows. The row-key IS the real slug
    /// here: `chief.db` is per-company, so no documentKey isolation is needed
    /// (that is a shared-docstore concern only).
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; [`ChiefdError::StoreFailure`] on a row that
    /// cannot map to the typed manifest.
    pub async fn org_manifest_read(
        &self,
    ) -> Result<Option<(crate::store::organization::OrganizationManifest, i64)>, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("org-manifest-rows", e))?;
            let manifest = crate::store::organization_rows::reconstruct(tx, &slug)?;
            Ok(manifest.map(|m| (m, seq)))
        })
        .await
    }

    /// Whole-company genesis: the manifest and its person operating contracts,
    /// all in the ONE SQLite transaction this
    /// job runs as (E7-S2, #815).
    ///
    /// Genesis is the first write a live daemon receives, so it must atomically
    /// establish the manifest and the scheduler-owned contract ledger in the actor
    /// snapshot: a daemon that was already running before the launcher creates
    /// its company must be able to react to its very first explicit launch
    /// intent without a restart or a separate seed race. Any refusal rolls
    /// every part back and a crash mid-genesis leaves nothing at all — never a
    /// company that exists with some of its documents missing.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; validation refusals are mapped by the
    /// docstore route and an existing company returns `AlreadyExists`.
    pub async fn org_manifest_genesis(
        &self,
        manifest: crate::store::organization::OrganizationManifest,
        at: String,
        person_contracts: crate::store::person_contracts::rows::OrganizationPersonContracts,
    ) -> Result<crate::store::organization_rows::ManifestGenesisOutcome, ChiefdError> {
        let slug = self.label().to_owned();
        let step_slug = slug.clone();
        let step_at = at.clone();
        // Genesis is the write that NAMES the company, so its own step runs
        // before `org_settings.display_slug` exists to be read. The incoming
        // manifest IS that name — and it is the name the two documents below
        // were built from, which is what their identity check compares against.
        let company = manifest.slug.clone();
        let step: TxnFn = Box::new(move |tx| {
            if crate::store::organization_rows::reconstruct(tx, &step_slug)?.is_none() {
                crate::store::person_contracts::rows::publish(
                    tx,
                    &step_slug,
                    &company,
                    &step_at,
                    &person_contracts,
                )?;
            }
            Ok(())
        });
        self.enqueue(
            Some(step),
            None,
            MutationClass::Normal,
            MutationName("org.manifest.genesis"),
            move |ledgers| {
                if crate::store::organization::exists(ledgers) {
                    return Ok(
                        crate::store::organization_rows::ManifestGenesisOutcome::AlreadyExists,
                    );
                }
                crate::store::organization::create(ledgers, &manifest)?;
                crate::store::supervision::seed(ledgers, &manifest)?;
                crate::store::activity::seed(ledgers, &manifest)?;
                Ok(crate::store::organization_rows::ManifestGenesisOutcome::Created)
            },
        )
        .await
    }

    /// Reconstruct the person-contracts document from the normalized rows.
    /// `None` ⇒ the company has no contract rows. The row-key IS the real slug
    /// (`chief.db` is per-company); no mutable revision crosses this boundary.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; [`ChiefdError::StoreFailure`] on a SQL
    /// failure.
    pub async fn org_person_contracts_read(
        &self,
    ) -> Result<
        Option<crate::store::person_contracts::rows::OrganizationPersonContracts>,
        ChiefdError,
    > {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            crate::store::person_contracts::rows::reconstruct(
                tx,
                &slug,
                &crate::store::org_settings::display_slug(tx, &slug)?,
            )
        })
        .await
    }

    /// Reconstruct the activity ledger from the normalized rows, with the
    /// `org_events` seq fence the read observed (org-data-normalization P0, N4).
    /// `None` ⇒ the company has no activity rows (never seeded / removed). The
    /// manifest rows are reconstructed first because the ledger's `person_order`
    /// and placement are anchored to the normalized organization rows.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; [`ChiefdError::StoreFailure`] on a row that
    /// cannot map to the typed ledger.
    pub async fn activity_read(
        &self,
    ) -> Result<Option<(crate::store::activity::ActivityLedger, i64)>, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("activity-rows", e))?;
            let Some(manifest) = crate::store::organization_rows::reconstruct(tx, &slug)? else {
                return Ok(None);
            };
            let ledger = crate::store::activity::rows::read_rows(tx, &slug, &manifest)
                .map_err(crate::store::activity::rows::activity_store_failed)?;
            Ok(ledger.map(|l| (l, seq)))
        })
        .await
    }

    /// The organization manifest AND the activity ledger, from ONE
    /// reconstruction and ONE actor visit. `None` ⇒ the company has no
    /// manifest rows; an inner `None` ledger ⇒ it has no activity rows.
    ///
    /// # Why this exists
    ///
    /// [`Self::org_manifest_read`] and [`Self::activity_read`] each rebuild the
    /// whole manifest, because the activity ledger's `person_order` and
    /// placement are anchored to the organization rows. A caller that wants
    /// both — `/v1/org/runtime/desired` is one, and it is the route the
    /// actuator polls — used to make two actor round trips and pay for the
    /// rebuild twice: `3 + 4N` statements each, including an unbounded
    /// `staffing_history` scan per person, for an answer that cannot differ
    /// between the two calls.
    ///
    /// One visit is also strictly MORE correct than two: the pair is now read
    /// under a single transaction, so a manifest and an activity ledger can no
    /// longer be assembled from either side of a commit.
    ///
    /// # Errors
    /// As [`Self::read_txn`]; [`ChiefdError::StoreFailure`] on a row that
    /// cannot map to the typed manifest or ledger.
    pub async fn org_manifest_and_activity_read(
        &self,
    ) -> Result<
        Option<(
            crate::store::organization::OrganizationManifest,
            Option<crate::store::activity::ActivityLedger>,
            i64,
        )>,
        ChiefdError,
    > {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("org-manifest-rows", e))?;
            let Some(manifest) = crate::store::organization_rows::reconstruct(tx, &slug)? else {
                return Ok(None);
            };
            let ledger = crate::store::activity::rows::read_rows(tx, &slug, &manifest)
                .map_err(crate::store::activity::rows::activity_store_failed)?;
            Ok(Some((manifest, ledger, seq)))
        })
        .await
    }

    /// Reconstruct the session-maintenance ledger from the normalized rows, with
    /// the `org_events` seq fence the read observed (org-data-normalization P0,
    /// N5). `None` ⇒ the company has never needed maintenance. 1:1 with
    /// [`CompanyDb::org_manifest_read`].
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; [`ChiefdError::StoreFailure`] on a row that
    /// cannot map to the typed ledger.
    pub async fn session_maintenance_read(
        &self,
    ) -> Result<
        Option<(crate::store::session_maintenance::SessionMaintenanceLedger, i64)>,
        ChiefdError,
    > {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("session-maintenance-rows", e))?;
            let ledger = crate::store::session_maintenance::rows::reconstruct(tx, &slug)
                .map_err(|e| store_failure("session-maintenance-rows", e))?;
            Ok(ledger.map(|l| (l, seq)))
        })
        .await
    }

    /// Reconstruct the supervision ledger from the normalized rows, with the
    /// `org_events` seq fence the read observed (org-data-normalization P0, N3).
    /// `None` ⇒ the company has no `supervision_meta` row (never seeded /
    /// removed). Mirror of [`CompanyDb::org_manifest_read`]: the row-key IS the
    /// real slug (`chief.db` is per-company, so no documentKey isolation).
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; [`ChiefdError::StoreFailure`] on a row that
    /// cannot map to the typed ledger.
    pub async fn supervision_read(
        &self,
    ) -> Result<Option<(crate::store::supervision::SupervisionLedger, i64)>, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("supervision-rows", e))?;
            let ledger = crate::store::supervision::rows::reconstruct(
                tx,
                &slug,
                &crate::store::org_settings::display_slug(tx, &slug)?,
            )?;
            Ok(ledger.map(|l| (l, seq)))
        })
        .await
    }

    /// Publish a whole person-contracts document into normalized rows from
    /// current SQLite state. `at` is the caller's ISO-8601 event stamp.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a `Refused` (`unmodeled-keys` or
    /// `person-contracts-invalid`) for a validation failure (route maps to 422).
    pub async fn org_person_contracts_publish(
        &self,
        at: String,
        doc: crate::store::person_contracts::rows::OrganizationPersonContracts,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        let seq = self
            .in_transaction(
                MutationClass::Normal,
                MutationName("org.person-contracts.publish"),
                move |tx| {
                    crate::store::person_contracts::rows::publish(
                        tx,
                        &slug,
                        &crate::store::org_settings::display_slug(tx, &slug)?,
                        &at,
                        &doc,
                    )
                },
            )
            .await?;
        Ok(seq)
    }

    /// Decide, for each requested person, whether their on-disk `AGENTS.md`
    /// needs to be rewritten from the stored contract (E7-S3): moves the MD5
    /// compare/repair DECISION out of TypeScript, which becomes a dumb
    /// actuator of the returned action. Pure read — writes nothing.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; `Refused(unknown-person-contract)`
    /// for any requested person with no stored row (maps to 422 — the
    /// caller's lazy-backfill publish is a separate, unchanged step).
    pub async fn org_person_contracts_projection_plan(
        &self,
        observed: Vec<crate::store::person_contracts::rows::ObservedContract>,
    ) -> Result<Vec<(String, crate::store::person_contracts::rows::ProjectionAction)>, ChiefdError>
    {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Small,
            MutationName("org.person-contracts.projection-plan"),
            move |tx| crate::store::person_contracts::rows::projection_plan(tx, &slug, &observed),
        )
        .await
    }

    /// Read the org-settings singleton, `launcher_root` included. `None` when
    /// the company has no `org_settings` row (genesis has not run).
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; [`ChiefdError::StoreFailure`] on a SQL
    /// failure.
    pub async fn org_settings_read(
        &self,
    ) -> Result<Option<crate::store::org_settings::OrgSettings>, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| crate::store::org_settings::read(tx, &slug)).await
    }

    /// Publish ONLY `org_settings.launcher_root` (E7-S3), replacing
    /// `state/launcher.json` — ONE `BEGIN IMMEDIATE` transaction (D19): the
    /// column update and its `org_events` audit row commit together or not at
    /// all. The four policy ints are untouched.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; `Refused(unknown-company)` when the
    /// company has no `org_settings` row yet (maps to 404).
    pub async fn org_settings_publish_launcher_root(
        &self,
        at: String,
        launcher_root: String,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.settings.publish-launcher-root"),
            move |tx| {
                crate::store::org_settings::publish_launcher_root(tx, &slug, &at, &launcher_root)
            },
        )
        .await
    }

    /// Atomically shut a person down — the first member of the `org_ops` atomic
    /// org-chart family (the operator: every org-chart action is ONE fast SQL txn, no
    /// queuing/cap/backoff). One `BEGIN IMMEDIATE`:
    /// CEO/exec-root guard → supersede any open transition → terminal `park`
    /// (`applied`) → `last_desired_active = 0` → drop the launch-intent fence →
    /// per-entity `org_events`. Actuation is NOT here: the converge reaps the
    /// pane reactively off `last_desired_active = 0` (no KillPane under lock).
    ///
    /// `at` is the caller's ISO-8601 clock; `actor` the change's author. The
    /// outcome is a value, not an error: `Refused{CeoExempt}` writes nothing
    /// and maps to 422. The writer serializes the decision and write, so this
    /// operation never exposes a caller retry fence.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    pub async fn shutdown_person(
        &self,
        person_id: String,
        kind: crate::store::org_ops::ShutdownKind,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::ShutdownOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.person.shutdown"),
            move |tx| {
                crate::store::org_ops::shutdown_person(tx, &slug, &person_id, &kind, &at, &actor)
                    .map_err(|e| store_failure("org-activity-rows", e))
            },
        )
        .await
    }

    /// Atomically appoint a new department head (H2) — org_ops family member 2.
    /// One `BEGIN IMMEDIATE` that re-points the head, flips kinds, strips bash,
    /// optionally R4-demotes the outgoing head, records staffing, and performs
    /// the supervision ownership transfer (goal ids stable, nothing cancelled).
    /// The outcome is a value: `Refused{code}` writes nothing and maps to 422.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    pub async fn appoint_department_head(
        &self,
        department_id: String,
        successor_person_id: String,
        demote_to_department_id: Option<String>,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::AppointOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.person.appoint-head"),
            move |tx| {
                crate::store::org_ops::appoint_department_head(
                    tx,
                    &slug,
                    &department_id,
                    &successor_person_id,
                    demote_to_department_id.as_deref(),
                    &at,
                    &actor,
                )
                .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically create a department — the P1-a member of the `org_ops` atomic
    /// org-chart family (the operator: every org-chart action is ONE fast SQL txn). One
    /// `BEGIN IMMEDIATE`: refusal guards (unknown-parent
    /// / parent-paused / duplicate-department-id / head-decision-required /
    /// exec-root-protected) → INSERT the department at its append-ordinal → the
    /// explicit head decision (appoint-existing re-points the head; hire-new
    /// inserts one) → the head's and every active staff seed's `launch_intent`
    /// fence row → per-entity `org_events`. NO pane is spawned inside the
    /// transaction; the live reconciler converges to the fence, and only the
    /// settle path stops anybody again.
    ///
    /// `at` is the caller's ISO-8601 clock; `actor` the change's author. The
    /// outcome is a value, not an error: `Refused{code}` writes nothing and
    /// maps to a 422 business refusal.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_department(
        &self,
        department_id: String,
        parent_id: String,
        name: String,
        purpose: String,
        head: crate::store::org_ops::HeadDecision,
        staff: Vec<crate::store::org_ops::DepartmentStaffSeed>,
        requester_person_id: Option<String>,
        audit_reason: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::CreateDepartmentOutcome, ChiefdError> {
        self.create_department_unit(
            department_id,
            parent_id,
            name,
            purpose,
            head,
            staff,
            crate::store::org_ops::DepartmentCreateUnit::Department,
            None,
            requester_person_id,
            audit_reason,
            at,
            actor,
            PublishBarrier::none(),
        )
        .await
    }

    /// Typed-unit extension of [`Self::create_department`].
    #[allow(clippy::too_many_arguments)]
    pub async fn create_department_unit(
        &self,
        department_id: String,
        parent_id: String,
        name: String,
        purpose: String,
        head: crate::store::org_ops::HeadDecision,
        staff: Vec<crate::store::org_ops::DepartmentStaffSeed>,
        unit: crate::store::org_ops::DepartmentCreateUnit,
        head_vacates: Option<crate::store::org_ops::HeadVacancy>,
        requester_person_id: Option<String>,
        audit_reason: String,
        at: String,
        actor: String,
        barrier: PublishBarrier,
    ) -> Result<crate::store::org_ops::CreateDepartmentOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org_barriered(
            MutationClass::Normal,
            MutationName("org.department.create"),
            barrier,
            move |tx| {
                let outcome = crate::store::org_ops::create_department_with_staff_unit(
                    tx,
                    &slug,
                    &department_id,
                    &parent_id,
                    &name,
                    &purpose,
                    &head,
                    &staff,
                    &unit,
                    head_vacates.as_ref(),
                    requester_person_id.as_deref(),
                    &audit_reason,
                    &at,
                    &actor,
                )
                .map_err(|e| store_failure("org-manifest-rows", e))?;
                Self::commit_person_contracts_with(&outcome, tx, &slug, &at)?;
                Ok(outcome)
            },
        )
        .await
    }

    /// Atomically reparent a department (org_ops family P1-d — the operator's reorg): ONE
    /// `BEGIN IMMEDIATE` txn that re-points `parent_id`, recomputes the whole-tree
    /// preorder ordinal bijection (H1), and emits per-department `org_events`.
    /// The company writer serializes current-row validation, so callers supply
    /// no sequence and never retry a stale structural snapshot. Pane placement
    /// follows via converge (no actuation under lock). `Refused{...}` is a
    /// policy value (route → 422) and writes nothing.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    pub async fn reparent_department(
        &self,
        department_id: String,
        new_parent_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::ReparentOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.department.reparent"),
            move |tx| {
                crate::store::org_ops::reparent_department(
                    tx,
                    &slug,
                    &department_id,
                    &new_parent_id,
                    &at,
                    &actor,
                )
                .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically transfer ONE person to a destination department — an `org_ops`
    /// H1 family member. One `BEGIN IMMEDIATE`: validate
    /// (unknown-person/destination, paused, departed, head-needs-
    /// successor, exec-root) → supersede any open transition → re-home the person
    /// → append the `transferred` staffing entry → restore the whole-company
    /// ordinal bijection (H1). Pane placement follows via converge (#448).
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    #[allow(clippy::too_many_arguments)]
    pub async fn transfer_person(
        &self,
        person_id: String,
        destination_id: String,
        intent: String,
        at: String,
        actor: String,
        head_vacates: Option<crate::store::org_ops::HeadVacancy>,
    ) -> Result<crate::store::org_ops::TransferOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.person.transfer"),
            move |tx| {
                crate::store::org_ops::transfer_person(
                    tx,
                    &slug,
                    &person_id,
                    &destination_id,
                    &intent,
                    &at,
                    &actor,
                    head_vacates.as_ref(),
                )
                .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically move a SET of members from a source department to a
    /// destination — an `org_ops` H1 family member (N transfers in ONE
    /// `BEGIN IMMEDIATE`, all-or-nothing). Same composition as
    /// [`CompanyDb::transfer_person`]; every listed person must be a member (home)
    /// of the source and individually movable, or the WHOLE batch is refused
    /// without touching a row. Restores the H1 ordinal bijection once.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    #[allow(clippy::too_many_arguments)]
    pub async fn move_department_members(
        &self,
        from_department_id: String,
        destination_id: String,
        person_ids: Vec<String>,
        intent: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::TransferOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.department.move-members"),
            move |tx| {
                crate::store::org_ops::move_department_members(
                    tx,
                    &slug,
                    &from_department_id,
                    &destination_id,
                    &person_ids,
                    &intent,
                    &at,
                    &actor,
                )
                .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically offboard (FIRE) a person (P2) — org_ops family member 3. One
    /// `BEGIN IMMEDIATE` that flips `employment_state → departed` (row retained),
    /// writes a terminal `offboard` transition, returns them home, records
    /// staffing and clears the fence. The outcome
    /// is a value: `Refused{code}` writes nothing and maps to 422.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    pub async fn offboard_person(
        &self,
        person_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::OffboardOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.person.offboard"),
            move |tx| {
                crate::store::org_ops::offboard_person(tx, &slug, &person_id, &at, &actor)
                    .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically hire a new person into a department (P2-f) — org_ops family
    /// member 4. One `BEGIN IMMEDIATE` that inserts the person row + tool grants
    /// (department, employment active/benched, next gapless
    /// ordinal),
    /// seeds `person_activity` desired-off (THE HARD RULE: hiring starts NO
    /// pane), appends `staffing_history 'hired'`, and re-asserts the ordinal
    /// bijection. The outcome is a value: `Refused{code}` writes nothing and
    /// maps to a 422 business refusal.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    #[allow(clippy::too_many_arguments)]
    pub async fn hire_person(
        &self,
        person_id: String,
        department_id: String,
        seed: crate::store::org_ops::OwnedNewPersonSeed,
        requester_person_id: Option<String>,
        at: String,
        actor: String,
        barrier: PublishBarrier,
    ) -> Result<crate::store::org_ops::HireOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org_barriered(
            MutationClass::Normal,
            MutationName("org.person.hire"),
            barrier,
            move |tx| {
                crate::store::org_ops::hire_person_authorized(
                    tx,
                    &slug,
                    &person_id,
                    &department_id,
                    &seed.as_ref(),
                    requester_person_id.as_deref(),
                    &at,
                    &actor,
                )
                .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically PAUSE a department (P2-h) — org_ops family member. One `BEGIN
    /// IMMEDIATE` that flips `departments.state → 'paused'` and emits the single
    /// department org_events touch. A paused dept refuses transfers into it
    /// (`destination-paused`). The outcome is a value: `Refused{code}` and
    /// no partial state is written when a policy refusal is returned.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    pub async fn pause_department(
        &self,
        department_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::PauseOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.department.pause"),
            move |tx| {
                crate::store::org_ops::pause_department(tx, &slug, &department_id, &at, &actor)
                    .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically RESUME a department (P2-h) — org_ops family member. One `BEGIN
    /// IMMEDIATE` that flips `departments.state → 'active'`. Resume restores STATE
    /// ONLY — nobody spawns (THE HARD RULE). The outcome is a value: `Refused
    /// {code}` writes nothing and maps to 422.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    pub async fn resume_department(
        &self,
        department_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::PauseOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.department.resume"),
            move |tx| {
                crate::store::org_ops::resume_department(tx, &slug, &department_id, &at, &actor)
                    .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically resume a set of departments. This is deliberately one writer
    /// transaction: a parent and child can become active together without a
    /// caller-managed revision ladder, and no person is started as a side effect.
    pub async fn resume_departments(
        &self,
        department_ids: Vec<String>,
        skip_active: bool,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::PauseOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.department.resume-many"),
            move |tx| {
                crate::store::org_ops::resume_departments(
                    tx,
                    &slug,
                    &department_ids,
                    skip_active,
                    &at,
                    &actor,
                )
                .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically BENCH a person (P2) — org_ops family member 4. One `BEGIN
    /// IMMEDIATE` that flips `employment_state → benched` (row + placement
    /// retained), writes a terminal `park` transition, sets desired-off so the
    /// converge reaps the pane, records staffing `'benched'`, and clears the
    /// fence. Bench does NOT move the person or renumber ordinals (H1 untouched).
    /// The outcome is a value: `Refused{code}` writes nothing and maps to 422.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a sqlite failure inside the op is
    /// `StoreFailure`.
    pub async fn bench_person(
        &self,
        person_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::BenchOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.person.bench"),
            move |tx| {
                crate::store::org_ops::bench_person(tx, &slug, &person_id, &at, &actor)
                    .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically run ChiefD's reflected bench lifecycle.
    pub async fn bench_person_lifecycle(
        &self,
        person_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::BenchLifecycleOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.person.bench-lifecycle"),
            move |tx| {
                crate::store::org_ops::bench_person_lifecycle(tx, &slug, &person_id, &at, &actor)
                    .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Recall one benched person without starting a runtime pane.
    pub async fn recall_person(
        &self,
        person_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::DirectOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.person.recall"),
            move |tx| {
                crate::store::org_ops::recall_person(tx, &slug, &person_id, &at, &actor)
                    .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Atomically appoint a successor and offboard the former department head.
    pub async fn replace_head_and_offboard(
        &self,
        head_person_id: String,
        successor_person_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::DirectOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.person.replace-head-and-offboard"),
            move |tx| {
                crate::store::org_ops::replace_head_and_offboard(
                    tx,
                    &slug,
                    &head_person_id,
                    &successor_person_id,
                    &at,
                    &actor,
                )
                .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Repair legacy-paused executive-root departments.  This restores state
    /// only and never starts a person.
    pub async fn reactivate_executive_root(
        &self,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::DirectOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.department.reactivate-executive-root"),
            move |tx| {
                crate::store::org_ops::reactivate_executive_root(tx, &slug, &at, &actor)
                    .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Remove one non-root department subtree atomically. The operation owns
    /// its current-row preconditions and returns immutable deleted identities,
    /// never a manifest revision or retry fence.
    pub async fn remove_department_tree(
        &self,
        department_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::RemoveDepartmentOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.department.remove-tree"),
            move |tx| {
                crate::store::org_ops::remove_department_tree(
                    tx,
                    &slug,
                    &department_id,
                    &at,
                    &actor,
                )
                .map_err(|e| store_failure("org-manifest-rows", e))
            },
        )
        .await
    }

    /// Publish a whole supervision ledger into normalized rows as a direct
    /// atomic current-state operation (org-data-normalization P0, N3). It
    /// writes only touched rows plus immutable `org_events`; the returned
    /// sequence is audit evidence, never a caller write precondition. Item D:
    /// publish is STRICT — an incoming ledger carrying a key the row model does
    /// not cover is refused `unmodeled-keys` (the route maps it to 422), never a
    /// silent drop.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a `Refused` (`unmodeled-keys` or
    /// `supervision-invalid`) for a validation failure (the route maps it to 422).
    pub async fn supervision_publish(&self, body: String) -> Result<i64, ChiefdError> {
        // #637: route the launcher-authored row write through the same actor
        // mutation that every live ChiefD duty uses. The prior raw transaction
        // committed the normalized rows but left this process's immutable
        // snapshot stale, so the deadline duty woke up unable to see the newly
        // pending report. `ingest_external_document` adopts the raw relational
        // half, writes the row authority atomically, refreshes the snapshot, and
        // lets `run_job` publish the post-commit supervision feed hint.
        self.mutate(
            MutationClass::Normal,
            MutationName("org.supervision.publish"),
            move |ledgers| {
                let manifest = crate::store::organization::read(ledgers)?;
                // The very first launcher publish is the supervision genesis
                // that follows manifest genesis. It has no incumbent ledger to
                // reconcile, but still has to become a committed actor snapshot
                // and emit the same post-commit watch hint as every later
                // publish. Subsequent writes take the strict reconcile path.
                if crate::store::supervision::create_if_absent(ledgers, &body)? {
                    crate::store::supervision::read(ledgers, &manifest)?;
                    return Ok(());
                }
                crate::store::supervision::ingest_external_document(ledgers, &manifest, &body)
            },
        )
        .await?;

        // The route's sequence is immutable audit evidence, never a caller
        // precondition. Read the current row cursor after the single writer
        // commit so the existing typed HTTP contract remains intact.
        self.org_current_seq().await
    }

    /// [`Self::supervision_publish`] — same write, gated on the caller's
    /// last-read `org_events` seq still being current (#950/#954's fifth and
    /// last CAS route).
    ///
    /// Unlike the three sibling `*_publish_cas` methods
    /// (`session_maintenance_publish_cas`,
    /// `operator_escalation_intents_publish_cas`), this
    /// does NOT call a `<store>::rows::publish` function directly inside a
    /// raw-SQL [`Self::in_transaction`] step. `supervision::rows::publish` is
    /// dead — established earlier, and correctly left dead, not revived here
    /// either. Supervision's real, live commit path is the coordinated pair
    /// `persist_dispatch::dispatch_persist` (→ `supervision_rows::publish_meta`
    /// for the meta row) plus `persist_relational_tail`'s
    /// `ledger::relational_diff` call (for the `effects`
    /// relational table) — both driven automatically by `run_job`'s
    /// `apply → validate → persist → commit` sequence whenever a real
    /// `Ledgers`-mutating closure runs. A hand-rolled raw-SQL equivalent
    /// inside a `txn_step` cannot reproduce `relational_diff`'s output
    /// without a real `previous`/`working` `Ledgers` pair to diff, so this
    /// wraps the live path instead of re-deriving it: the seq check runs as
    /// a real `txn_step`, composed via [`Self::enqueue`] directly (not
    /// [`Self::mutate`]) with [`Self::supervision_publish`]'s existing
    /// `apply` closure, verbatim and unmodified.
    ///
    /// This is the FIRST place anywhere in this crate that pairs a real
    /// `txn_step` with a real (non-no-op) `apply` — every existing
    /// `enqueue(Some(step), ...)` caller (`in_transaction`,
    /// `in_transaction_refreshing_org`) pairs its step with an `apply` that
    /// only returns a value the step already stashed. `run_job`'s ordering
    /// guarantee (`txn_step` runs first, inside the transaction `apply` will
    /// use; a `txn_step` error returns before `apply` is ever reached, so no
    /// `Ledgers` clone is even taken) is a property of `run_job`'s control
    /// flow, not of what `apply` does once reached, so it holds for this
    /// pairing for the same reason it holds for the no-op ones — but this
    /// combination had no prior exerciser before the tests below. Its
    /// coverage is exactly what those tests assert and nothing more.
    ///
    /// # Errors
    /// `ChiefdError::Conflict` (`seq-conflict`, route → 409) when
    /// `expected_seq` no longer matches the current `org_events` cursor;
    /// otherwise as [`Self::supervision_publish`].
    pub async fn supervision_publish_cas(
        &self,
        body: String,
        expected_seq: i64,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.enqueue(
            Some(Box::new(move |tx: &rusqlite::Transaction<'_>| {
                let current = crate::store::rows_txn::current_seq(tx, &slug)
                    .map_err(|e| store_failure("org-events", e))?;
                if current != expected_seq {
                    return Err(ChiefdError::conflict(
                        "seq-conflict",
                        expected_seq.to_string(),
                        current.to_string(),
                    ));
                }
                Ok(())
            })),
            None,
            MutationClass::Normal,
            MutationName("org.supervision.publish-cas"),
            move |ledgers| {
                // Verbatim `supervision_publish` apply body -- the live path,
                // unmodified, not reimplemented.
                let manifest = crate::store::organization::read(ledgers)?;
                if crate::store::supervision::create_if_absent(ledgers, &body)? {
                    crate::store::supervision::read(ledgers, &manifest)?;
                    return Ok(());
                }
                crate::store::supervision::ingest_external_document(ledgers, &manifest, &body)
            },
        )
        .await?;

        self.org_current_seq().await
    }

    /// Publish a whole activity ledger (serialized JSON) into normalized rows
    /// in one atomic current-state transaction (org-data-normalization P0, N4).
    /// The returned sequence is an immutable audit-event cursor, not a caller
    /// supplied write precondition. Item D (`unmodeled-keys`) and validation
    /// refusals map to 422 at the route.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a `Refused` for a validation failure.
    pub async fn activity_publish(&self, ledger: String) -> Result<i64, ChiefdError> {
        // Like supervision publication, activity must pass through the actor
        // mutation path. A raw rows transaction updated SQLite but left the
        // daemon's immutable snapshot stale, so a just-created company woke on
        // a launch intent and still read "activity never written" until a
        // process restart. Keep the missing-ledger creation path for a safe
        // migration of older companies, while ordinary writes reconcile the
        // existing authoritative ledger in this same actor commit.
        self.mutate(MutationClass::Normal, MutationName("org.activity.publish"), move |ledgers| {
            let manifest = crate::store::organization::read(ledgers)?;
            if crate::store::activity::create_if_absent(ledgers, &ledger)? {
                crate::store::activity::read(ledgers, &manifest)?;
                return Ok(());
            }
            crate::store::activity::ingest_external_document(ledgers, &manifest, &ledger)
        })
        .await?;
        self.org_current_seq().await
    }

    /// Reconcile an existing activity aggregate after the authoritative
    /// organization manifest changed shape. The caller supplies neither a
    /// ledger nor a retry token: both the manifest and clock are owned inside
    /// this one actor mutation.
    ///
    /// Returns whether the normalized activity rows changed, together with the
    /// current immutable event cursor. A no-op preserves that cursor.
    pub async fn activity_reconcile_structural(&self) -> Result<(bool, i64), ChiefdError> {
        let applied = self
            .mutate(
                MutationClass::Normal,
                MutationName("org.activity.reconcile-structural"),
                move |ledgers| {
                    let manifest = crate::store::organization::read(ledgers)?;
                    crate::store::activity::reconcile_structural(ledgers, &manifest)
                },
            )
            .await?;
        let seq = self.org_current_seq().await?;
        Ok((applied, seq))
    }

    /// Publish a whole session-maintenance ledger into the normalized rows,
    /// as one atomic current-state transaction (org-data-normalization P0, N5).
    /// Its returned sequence is immutable audit evidence, not caller-side CAS.
    /// It rejects unmodeled keys (item D) and validates INTERNAL invariants only
    /// — the authoritative people-membership reconcile stays TS-side, since
    /// pre-N9 the manifest rows are empty.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a `Refused` (`unmodeled-keys` or
    /// `session-maintenance-ledger-invalid`) the route maps to 422.
    pub async fn session_maintenance_publish(
        &self,
        ledger: crate::store::session_maintenance::SessionMaintenanceLedger,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("session-maintenance.publish"),
            move |tx| crate::store::session_maintenance::rows::publish(tx, &slug, &ledger),
        )
        .await
    }

    /// #954 (additive, no caller yet): compare-and-swap variant of
    /// [`Self::session_maintenance_publish`] — same write, gated on the
    /// caller's last-read `org_events` seq still being current. Restores the
    /// expected-seq CAS this store's own history names as retired
    /// (`org-session-maintenance.ts`'s own comment: "the server retired the
    /// caller-supplied expected-seq CAS field, so mutual exclusion ... comes
    /// from the SAME durable lock" — see the design record).
    /// The check and the write run inside the SAME `in_transaction` closure —
    /// i.e. the same `BEGIN IMMEDIATE`, the same single queued job. That is
    /// load-bearing, not a style choice: the single-writer queue guarantees no
    /// other mutation can be admitted between two SEPARATE queued jobs either,
    /// so a check-then-write split across two `in_transaction` calls would
    /// reopen exactly the race this method exists to close (a second publish
    /// landing in the gap between the check job's commit and the write job's
    /// admission). One job, one transaction, is what makes this a real CAS.
    ///
    /// # Errors
    /// `ChiefdError::Conflict` (route → 409) when `expected_seq` no longer
    /// matches the current `org_events` cursor; otherwise as
    /// [`Self::session_maintenance_publish`].
    pub async fn session_maintenance_publish_cas(
        &self,
        ledger: crate::store::session_maintenance::SessionMaintenanceLedger,
        expected_seq: i64,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("session-maintenance.publish-cas"),
            move |tx| {
                let current = crate::store::rows_txn::current_seq(tx, &slug)
                    .map_err(|e| store_failure("org-events", e))?;
                if current != expected_seq {
                    return Err(ChiefdError::conflict(
                        "seq-conflict",
                        expected_seq.to_string(),
                        current.to_string(),
                    ));
                }
                crate::store::session_maintenance::rows::publish(tx, &slug, &ledger)
            },
        )
        .await
    }

    // TOMBSTONE: `begin_fresh_session_launches` and
    // `complete_fresh_session_launches`, the actuator-owned claim and credit for
    // a fresh-session launch. They existed only for the deleted `fresh_session`
    // action, and the ledger methods under them are gone too.

    // ---- B4 singleton-sweep seam (org-data-normalization P0) --------------
    //
    // One read/publish pair per ported store, each mirroring
    // `org_manifest_read`: read reconstructs the typed document + the
    // `org_events` sequence over `in_transaction`. The row-key IS the real slug
    // (chief.db is per-company).

    /// Read the `session-epoch` singleton + its fence seq (`None` ⇒ no row).
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure.
    pub async fn session_epoch_read(
        &self,
    ) -> Result<Option<(crate::store::session_epoch_rows::SessionEpoch, i64)>, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("session-epoch-rows", e))?;
            Ok(crate::store::session_epoch_rows::reconstruct(
                tx,
                &slug,
                &crate::store::org_settings::display_slug(tx, &slug)?,
            )?
            .map(|d| (d, seq)))
        })
        .await
    }
    /// Publish the `session-epoch` singleton atomically from current SQLite state.
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn session_epoch_publish(
        &self,
        doc: crate::store::session_epoch_rows::SessionEpoch,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.session-epoch.publish"),
            move |tx| {
                crate::store::session_epoch_rows::publish(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                    &doc,
                )
            },
        )
        .await
    }

    /// Read this company's operator stand-down, or `None` when it is working
    /// normally.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure.
    pub async fn stand_down_read(
        &self,
    ) -> Result<Option<crate::store::stand_down::StandDown>, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| crate::store::stand_down::read(tx, &slug)).await
    }

    /// Stand this company down: record the operator's decision and empty the
    /// launch-intent fence, in one transaction. Idempotent.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn stand_down_set(&self, at: String, reason: String) -> Result<(), ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(MutationClass::Normal, MutationName("org.stand-down.set"), move |tx| {
            crate::store::stand_down::stand_down(tx, &slug, &at, &reason)
        })
        .await
    }

    /// Lift this company's stand-down. Idempotent.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn stand_down_clear(&self, at: String) -> Result<(), ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.stand-down.clear"),
            move |tx| crate::store::stand_down::resume(tx, &slug, &at),
        )
        .await
    }

    /// Read the `goal-delivery-quiesce` singleton + its fence seq.
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure.
    pub async fn goal_delivery_quiesce_read(
        &self,
    ) -> Result<
        Option<(crate::store::goal_delivery_quiesce_rows::GoalDeliveryQuiesce, i64)>,
        ChiefdError,
    > {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("goal-delivery-quiesce-rows", e))?;
            Ok(crate::store::goal_delivery_quiesce_rows::reconstruct(
                tx,
                &slug,
                &crate::store::org_settings::display_slug(tx, &slug)?,
            )?
            .map(|d| (d, seq)))
        })
        .await
    }
    /// Publish the `goal-delivery-quiesce` singleton as one writer-owned atomic
    /// current-state operation. The returned seq is immutable audit/cursor
    /// evidence.
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn goal_delivery_quiesce_publish(
        &self,
        doc: crate::store::goal_delivery_quiesce_rows::GoalDeliveryQuiesce,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.goal-delivery-quiesce.publish"),
            move |tx| {
                crate::store::goal_delivery_quiesce_rows::publish(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                    &doc,
                )
            },
        )
        .await
    }

    /// Clear the  rows (unconditional delete + one op="delete"
    /// org_events touch per removed entity). Fence-free — idempotent (absent =>
    /// no-op).  is the caller-supplied event stamp.
    /// # Errors
    /// [] on a SQL fault.
    pub async fn goal_delivery_quiesce_clear(&self, at: String) -> Result<(), ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.goal-delivery-quiesce.clear"),
            move |tx| {
                crate::store::goal_delivery_quiesce_rows::clear(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                    &at,
                )
            },
        )
        .await
    }

    /// Read the `operator-escalation-push` singleton + its fence seq.
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure.
    pub async fn operator_escalation_push_read(
        &self,
    ) -> Result<
        Option<(crate::store::operator_escalation_push_rows::OperatorEscalationPush, i64)>,
        ChiefdError,
    > {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("operator-escalation-push-rows", e))?;
            Ok(crate::store::operator_escalation_push_rows::reconstruct(tx, &slug)?
                .map(|d| (d, seq)))
        })
        .await
    }
    /// Publish the `operator-escalation-push` singleton atomically from current
    /// SQLite state, returning an immutable audit sequence.
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn operator_escalation_push_publish(
        &self,
        doc: crate::store::operator_escalation_push_rows::OperatorEscalationPush,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        let seq = self
            .in_transaction(
                MutationClass::Normal,
                MutationName("org.operator-escalation-push.publish"),
                move |tx| {
                    crate::store::operator_escalation_push_rows::publish(
                        tx,
                        &slug,
                        &crate::store::org_settings::display_slug(tx, &slug)?,
                        &doc,
                    )
                },
            )
            .await?;
        // The push singleton also lives on rows (bypasses Ledgers); emit its
        // WatchEvent for any `/v1/docs/watch` subscriber.
        self.publish_row_feed_hint(
            crate::store::operator_escalation_push_rows::OPERATOR_ESCALATION_PUSH_STORE,
            "",
        );
        Ok(seq)
    }

    /// Read the `runtime-owner` singleton + its fence seq.
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure.
    pub async fn runtime_owner_read(
        &self,
    ) -> Result<Option<(crate::store::runtime_owner_rows::RuntimeOwner, i64)>, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("runtime-owner-rows", e))?;
            Ok(crate::store::runtime_owner_rows::reconstruct(
                tx,
                &slug,
                &crate::store::org_settings::display_slug(tx, &slug)?,
            )?
            .map(|d| (d, seq)))
        })
        .await
    }
    /// Publish the `runtime-owner` singleton atomically from current SQLite state.
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn runtime_owner_publish(
        &self,
        doc: crate::store::runtime_owner_rows::RuntimeOwner,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.runtime-owner.publish"),
            move |tx| {
                crate::store::runtime_owner_rows::publish(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                    &doc,
                )
            },
        )
        .await
    }

    /// Read the `launch-intent` doc + its fence seq (`None` ⇒ empty set).
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure. Always `Some` (an empty
    /// authorized set is a real fence state, never absent) — de-Option'd per the
    /// fence-containment fix.
    pub async fn launch_intent_read(
        &self,
    ) -> Result<Option<(crate::store::launch_intent_rows::LaunchIntent, i64)>, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("launch-intent-rows", e))?;
            Ok(Some((
                crate::store::launch_intent_rows::reconstruct(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                )?,
                seq,
            )))
        })
        .await
    }
    /// Publish the `launch-intent` doc atomically from current SQLite state.
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn launch_intent_publish(
        &self,
        doc: crate::store::launch_intent_rows::LaunchIntent,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.launch-intent.publish"),
            move |tx| crate::store::launch_intent_rows::publish(tx, &slug, &doc),
        )
        .await
    }

    /// Clear the  rows (unconditional delete + one op="delete"
    /// org_events touch per removed entity). Fence-free — idempotent (absent =>
    /// no-op).  is the caller-supplied event stamp.
    /// # Errors
    /// [] on a SQL fault.
    pub async fn launch_intent_clear(&self, at: String) -> Result<(), ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.launch-intent.clear"),
            move |tx| crate::store::launch_intent_rows::clear(tx, &slug, &at),
        )
        .await
    }

    /// Atomically admit one active person: the narrow launch fence and their
    /// durable desired-active activity demand advance together.
    pub async fn start_person(
        &self,
        person_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::DirectOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.person.start"),
            move |tx| {
                crate::store::org_ops::start_person(tx, &slug, &person_id, &at, &actor)
                    .map_err(|e| store_failure("org-activity-rows", e))
            },
        )
        .await
    }

    /// Wake one parked person: grant their launch intent and release the
    /// lapsed routine idle park that would otherwise make the grant unreadable.
    ///
    /// See [`crate::store::org_ops::wake_person`] for why this is not
    /// [`Self::start_person`] — the short version is that a wake must NOT
    /// pre-set `last_desired_active`, because the fence suppresses the
    /// `Requested` reason for anyone who already carries it.
    ///
    /// # Errors
    /// A typed refusal, or `StoreFailure` on a row failure.
    pub async fn wake_person(
        &self,
        person_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::DirectOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction_refreshing_org(
            MutationClass::Normal,
            MutationName("org.person.wake"),
            move |tx| {
                crate::store::org_ops::wake_person(tx, &slug, &person_id, &at, &actor)
                    .map_err(|e| store_failure("org-activity-rows", e))
            },
        )
        .await
    }

    /// Atomically prepare the durable CEO-only projection: clear launch intent
    /// and retract every non-CEO desired-active activity row in one writer turn.
    ///
    /// # Errors
    /// A typed refusal for an absent/invalid company, or `StoreFailure` on row failure.
    pub async fn prepare_ceo_only(&self, at: String) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.runtime.prepare-ceo-only"),
            move |tx| crate::store::org_ops::prepare_ceo_only(tx, &slug, &at, "company-ceo"),
        )
        .await
    }

    /// Clear ALL supervision rows for this company — the meta row, every
    /// slug-scoped family, the slug-scoped `effects`, and the
    /// slug-named sequence counters — so a subsequent `supervision_read` returns
    /// `None`. The row-path teardown the blob `drop_company_store` never had
    /// (deleteDoc needs it). Fence-free, idempotent (absent ⇒ no-op). `at` is
    /// the caller-supplied event stamp.
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL fault.
    pub async fn supervision_clear(&self, at: String) -> Result<(), ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.supervision.clear"),
            move |tx| crate::store::supervision::rows::clear(tx, &slug, &at),
        )
        .await
    }

    /// Read the `mutation-journal` doc + its audit cursor (`None` ⇒ empty).
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure.
    pub async fn mutation_journal_read(
        &self,
    ) -> Result<Option<(crate::store::mutation_journal_rows::MutationJournal, i64)>, ChiefdError>
    {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("mutation-journal-rows", e))?;
            Ok(crate::store::mutation_journal_rows::reconstruct(
                tx,
                &slug,
                &crate::store::org_settings::display_slug(tx, &slug)?,
            )?
            .map(|d| (d, seq)))
        })
        .await
    }
    /// Publish the `mutation-journal` doc from current SQLite state.
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn mutation_journal_publish(
        &self,
        doc: crate::store::mutation_journal_rows::MutationJournal,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.mutation-journal.publish"),
            move |tx| crate::store::mutation_journal_rows::publish(tx, &slug, &doc),
        )
        .await
    }

    /// Read the `health-monitor` state doc + its fence seq (`None` ⇒ no state).
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure.
    pub async fn health_monitor_read(
        &self,
    ) -> Result<Option<(crate::store::health_monitor_rows::HealthMonitorState, i64)>, ChiefdError>
    {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("health-monitor-rows", e))?;
            Ok(crate::store::health_monitor_rows::reconstruct(
                tx,
                &slug,
                &crate::store::org_settings::display_slug(tx, &slug)?,
            )?
            .map(|d| (d, seq)))
        })
        .await
    }
    /// Publish the `health-monitor` state doc from current SQLite state.
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn health_monitor_publish(
        &self,
        doc: crate::store::health_monitor_rows::HealthMonitorState,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.health-monitor.publish"),
            move |tx| crate::store::health_monitor_rows::publish(tx, &slug, &doc),
        )
        .await
    }

    /// Read the `runtime` projection doc + its fence seq (`None` ⇒ no runtime row).
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure.
    pub async fn runtime_read(
        &self,
    ) -> Result<Option<(crate::store::runtime_rows::RuntimeState, i64)>, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("runtime-rows", e))?;
            Ok(crate::store::runtime_rows::reconstruct(tx, &slug)?.map(|d| (d, seq)))
        })
        .await
    }
    /// Publish the `runtime` projection doc as one direct atomic current-state
    /// operation. Its returned sequence is immutable audit/cursor evidence.
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn runtime_publish(
        &self,
        doc: crate::store::runtime_rows::RuntimeState,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        let seq = self
            .in_transaction(MutationClass::Normal, MutationName("org.runtime.publish"), move |tx| {
                crate::store::runtime_rows::publish(tx, &slug, &doc)
            })
            .await?;
        // The silent-row-store gap (#711): `runtime` bypasses `Ledgers`, so a commit
        // here has no fan-out unless this call emits the hint itself.
        // Genesis writes go through exactly this path, so an unhinted
        // publish here left every fresh org's first runtime doc unable to
        // ever wake a `/v1/docs/watch` subscriber.
        if seq > 0 {
            self.publish_row_feed_hint(crate::store::runtime_rows::RUNTIME_STORE, "");
        }
        Ok(seq)
    }

    /// Update the actuator-owned startup admission watermark while preserving
    /// every unrelated runtime projection field. `bootstrap` is used only for
    /// the genesis admission, before the launcher has published its first
    /// runtime observation; later writes always retain the existing projection.
    ///
    /// It took a fourth `ceo_admission_debt: Option<bool>` argument until
    /// chief-home-is-cwd §4c, which set the one-shot "the next non-CEO batch
    /// still owes an admission step" flag on the runtime row. That debt could
    /// only be incurred by the daemon admitting the CEO on its own boot, and
    /// the daemon boots nobody; the flag and its column are deleted.
    pub async fn runtime_set_startup_admission_until(
        &self,
        startup_admission_until: String,
        at: String,
        bootstrap: crate::store::runtime_rows::RuntimeState,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        let seq = self
            .in_transaction(
                MutationClass::Reconcile,
                MutationName("org.runtime.set-startup-admission-until"),
                move |tx| {
                    let mut runtime =
                        crate::store::runtime_rows::reconstruct(tx, &slug)?.unwrap_or(bootstrap);
                    if runtime.startup_admission_until.as_deref() == Some(&startup_admission_until)
                    {
                        return Ok(0);
                    }
                    runtime.startup_admission_until = Some(startup_admission_until);
                    runtime.observed_at = at;
                    crate::store::runtime_rows::publish(tx, &slug, &runtime)
                },
            )
            .await?;
        // See `runtime_publish` above (#711): row-store writes need an
        // explicit hint or `run_job`'s Ledgers-only fan-out never sees them.
        if seq > 0 {
            self.publish_row_feed_hint(crate::store::runtime_rows::RUNTIME_STORE, "");
        }
        Ok(seq)
    }

    // TOMBSTONE (chief-home-is-cwd §4c): `runtime_clear_startup_ceo_admission_debt`
    // stood here and consumed the one-shot CEO admission debt after the next
    // non-CEO batch applied. Deleted with the debt column: only a daemon-side
    // CEO boot could incur it, and the daemon boots no pane.

    /// Commit a company's STOP — the stopped `runtime` projection and, for an
    /// attended stop, the ABSENCE of launch intent — as ONE transaction
    /// (Mandate 4).
    ///
    /// These were two transactions five apart, with a runtime `kill-session` and a
    /// bounded session-absence wait in between. The window between them is not
    /// cosmetic: launch intent is the only thing that authorizes a non-CEO pane
    /// on the next converge pass, so a crash — or any writer that re-added
    /// intent — after the runtime row said `stopped` and before the intent was
    /// cleared left a company that reports stopped and then boots its whole
    /// previous roster. That is the exact guarantee
    /// `runtime_lifecycle::stop_supervised_runtime` states in its own doc
    /// comment ("leaves the company holding no launch intent, so the next boot
    /// is CEO-only"), and it was not true. One commit makes stopped-status and
    /// intent-absence the same fact.
    ///
    /// `clear_launch_intent` is false for a daemon-converged stop, which
    /// deliberately narrows intent rather than deleting it.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL fault.
    pub async fn runtime_stop_publish(
        &self,
        state: crate::store::runtime_rows::RuntimeState,
        at: String,
        clear_launch_intent: bool,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        let seq = self
            .in_transaction(MutationClass::Normal, MutationName("org.runtime.stop"), move |tx| {
                let seq = crate::store::runtime_rows::publish(tx, &slug, &state)?;
                if clear_launch_intent {
                    crate::store::launch_intent_rows::clear(tx, &slug, &at)?;
                }
                Ok(seq)
            })
            .await?;
        // See `runtime_publish` (#711): row-store writes need an explicit hint
        // or `run_job`'s Ledgers-only fan-out never sees them.
        if seq > 0 {
            self.publish_row_feed_hint(crate::store::runtime_rows::RUNTIME_STORE, "");
        }
        Ok(seq)
    }

    /// Clear the `runtime` projection (delete the row + all children).
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL fault.
    pub async fn runtime_clear(&self, at: String) -> Result<(), ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(MutationClass::Normal, MutationName("org.runtime.clear"), move |tx| {
            crate::store::runtime_rows::clear(tx, &slug, &at)
        })
        .await
    }

    // TOMBSTONE (chief-home-is-cwd §4c): `ceo_boot_lease_read`,
    // `ceo_boot_lease_publish` and `ceo_boot_lease_clear` stood here. They were
    // the whole write side of the CEO boot lease — an exclusivity window
    // `launch_ceo_only_runtime` took before its slow pre-converge phase so
    // chiefd's own reconcile duty could not project the fleet underneath it.
    // The daemon boots no pane now, so the ONE publisher is gone and nothing
    // can hold, contend for, or observe a lease. The mutual exclusion it
    // provided is not replaced because there is nothing left to exclude: no
    // attended command runs a multi-step projection outside a transaction, and
    // every write is already serialized by one daemon per company, one writer
    // actor, and `converge_safety::begin_cycle`'s single-flight claim.

    /// Read the `converge-safety` doc + its fence seq (`None` ⇒ absent, the
    /// ordinary default for an unconfigured company).
    ///
    /// Returns the STORED [`crate::store::converge_safety::ConvergeSafetyState`]
    /// verbatim (via [`crate::store::converge_safety_rows::reconstruct`]) — never
    /// the breaker-folded [`crate::store::converge_safety::SafetyConfig`]
    /// projection `effective_config()` produces. A consumer deciding whether a
    /// company is actuating needs the real, stored `actuation_mode`; an
    /// approximated or defaulted safety-relevant mode is a silent wrong answer
    /// (Mandate 0 / D0), not a convenience.
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure.
    pub async fn converge_safety_read(
        &self,
    ) -> Result<Option<(crate::store::converge_safety::ConvergeSafetyState, i64)>, ChiefdError>
    {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("converge-safety-rows", e))?;
            Ok(crate::store::converge_safety_rows::reconstruct(tx, &slug)?.map(|d| (d, seq)))
        })
        .await
    }
    /// Publish the `converge-safety` doc from current SQLite state.
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn converge_safety_publish(
        &self,
        doc: crate::store::converge_safety::ConvergeSafetyState,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.converge-safety.publish"),
            move |tx| crate::store::converge_safety_rows::publish(tx, &slug, &doc),
        )
        .await
    }

    /// Read the `operator-escalation-intents` queue + its fence seq. Always
    /// `Some` (empty-map doc when no rows).
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL/row-map failure.
    pub async fn operator_escalation_intents_read(
        &self,
    ) -> Result<
        Option<(crate::store::operator_escalation_intents_rows::OperatorEscalationIntents, i64)>,
        ChiefdError,
    > {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("operator-escalation-intents-rows", e))?;
            Ok(Some((
                crate::store::operator_escalation_intents_rows::reconstruct(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                )?,
                seq,
            )))
        })
        .await
    }
    /// Publish the `operator-escalation-intents` queue atomically from current
    /// SQLite state, returning an immutable audit sequence.
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn operator_escalation_intents_publish(
        &self,
        doc: crate::store::operator_escalation_intents_rows::OperatorEscalationIntents,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        let seq = self
            .in_transaction(
                MutationClass::Normal,
                MutationName("org.operator-escalation-intents.publish"),
                move |tx| {
                    crate::store::operator_escalation_intents_rows::publish(
                        tx,
                        &slug,
                        &crate::store::org_settings::display_slug(tx, &slug)?,
                        &doc,
                    )
                },
            )
            .await?;
        // #277 wake: the supervisor's `createOrganizationMailboxWakeWatcher`
        // fires `onEscalationChange` on `event.store === "operator-escalation-
        // intents"`. This row write bypasses the Ledgers snapshot, so emit the
        // WatchEvent `run_job` never would.
        self.publish_row_feed_hint(
            crate::store::operator_escalation_intents_rows::OPERATOR_ESCALATION_INTENTS_STORE,
            "",
        );
        Ok(seq)
    }

    /// #954 (additive, no caller yet): compare-and-swap variant of
    /// [`Self::operator_escalation_intents_publish`] — see
    /// `session_maintenance_publish_cas`'s doc comment for why the check and
    /// the write must run inside the SAME `in_transaction` closure, and
    /// the design record for why this exists (the
    /// `OPERATOR_ESCALATION_INTENTS_DRAIN_LOCK_SCOPE` TS-side lock this is
    /// meant to eventually replace). Emits the same `#277` post-commit feed
    /// hint as the non-CAS path, so a future caller sees identical wake
    /// behaviour either way.
    ///
    /// # Errors
    /// `ChiefdError::Conflict` (route → 409) when `expected_seq` no longer
    /// matches the current `org_events` cursor; otherwise as
    /// [`Self::operator_escalation_intents_publish`].
    pub async fn operator_escalation_intents_publish_cas(
        &self,
        doc: crate::store::operator_escalation_intents_rows::OperatorEscalationIntents,
        expected_seq: i64,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        let seq = self
            .in_transaction(
                MutationClass::Normal,
                MutationName("org.operator-escalation-intents.publish-cas"),
                move |tx| {
                    let current = crate::store::rows_txn::current_seq(tx, &slug)
                        .map_err(|e| store_failure("org-events", e))?;
                    if current != expected_seq {
                        return Err(ChiefdError::conflict(
                            "seq-conflict",
                            expected_seq.to_string(),
                            current.to_string(),
                        ));
                    }
                    crate::store::operator_escalation_intents_rows::publish(
                        tx,
                        &slug,
                        &crate::store::org_settings::display_slug(tx, &slug)?,
                        &doc,
                    )
                },
            )
            .await?;
        self.publish_row_feed_hint(
            crate::store::operator_escalation_intents_rows::OPERATOR_ESCALATION_INTENTS_STORE,
            "",
        );
        Ok(seq)
    }

    /// Insert one operator-escalation intent without replacing the queue.
    ///
    /// # Errors
    /// A `Refused` (route → 422) or [`ChiefdError::StoreFailure`].
    pub async fn operator_escalation_intents_insert(
        &self,
        intent: crate::store::operator_escalation_intents_rows::OperatorEscalationIntent,
    ) -> Result<
        crate::store::operator_escalation_intents_rows::InsertOperatorEscalationOutcome,
        ChiefdError,
    > {
        let slug = self.label().to_string();
        let outcome = self
            .in_transaction(
                MutationClass::Normal,
                MutationName("org.operator-escalation-intents.insert"),
                move |tx| {
                    crate::store::operator_escalation_intents_rows::insert_if_absent(
                        tx, &slug, &intent,
                    )
                },
            )
            .await?;
        if let crate::store::operator_escalation_intents_rows::InsertOperatorEscalationOutcome::Inserted { .. } = &outcome {
            self.publish_row_feed_hint(
                crate::store::operator_escalation_intents_rows::OPERATOR_ESCALATION_INTENTS_STORE,
                "",
            );
        }
        Ok(outcome)
    }

    /// Reconstruct the whole-company mailbox from the columnarized rows, with
    /// the `org_events` seq fence the read observed (org-data-normalization P0,
    /// N-mailbox). The mailbox is always present (possibly empty), so this
    /// returns a snapshot rather than an `Option`.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; [`ChiefdError::StoreFailure`] on an
    /// unreadable row.
    pub async fn mailbox_read(
        &self,
    ) -> Result<(crate::store::mailbox_rows::MailboxSnapshot, i64), ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("mailbox-rows", e))?;
            let snapshot = crate::store::mailbox_rows::reconstruct(tx, &slug)?;
            Ok((snapshot, seq))
        })
        .await
    }

    /// Publish a whole mailbox into the columnarized rows through one
    /// writer-owned atomic current-state operation. Diffs the incoming snapshot
    /// against the current rows and writes only the touched rows + one
    /// `org_events` row per touched `(envelope,recipient)`; the returned
    /// sequence is immutable audit/cursor evidence.
    ///
    /// # Errors
    /// As [`CompanyDb::in_transaction`]; a `Refused` (`unmodeled-keys` or
    /// `mailbox-invalid`) for a validation failure (the route maps it to 422).
    pub async fn mailbox_publish(
        &self,
        snapshot: crate::store::mailbox_rows::MailboxSnapshot,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        // Distinct persons in the incoming snapshot + the stamp, cloned out
        // before the closure moves `snapshot`, for the post-commit wake hint.
        let mut hint_persons: Vec<String> =
            snapshot.entries.iter().map(|e| e.person.clone()).collect();
        hint_persons.sort();
        hint_persons.dedup();
        let hint_at = snapshot
            .entries
            .iter()
            .map(|e| e.updated_at)
            .max()
            .map(|ms| ms.to_string())
            .unwrap_or_default();
        let outcome = self
            .in_transaction(MutationClass::Normal, MutationName("org.mailbox.publish"), move |tx| {
                crate::store::mailbox_rows::publish(tx, &slug, &snapshot)
            })
            .await?;
        for person in &hint_persons {
            self.publish_row_feed_hint(
                &crate::store::mailbox_rows::mailbox_store_name(person),
                &hint_at,
            );
        }
        Ok(outcome)
    }

    /// Read ONE person's mailbox from the columnarized rows (WHERE person=?),
    /// recipients completed across sibling rows, with the org_events seq fence.
    /// The per-person read the flipped caller API uses (O(person) not O(company)).
    /// # Errors
    /// As []; [] on a SQL fault.
    pub async fn mailbox_read_person(
        &self,
        person: String,
    ) -> Result<(crate::store::mailbox_rows::MailboxSnapshot, i64), ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| {
            let seq = crate::store::rows_txn::current_seq(tx, &slug)
                .map_err(|e| store_failure("mailbox-rows", e))?;
            let snapshot = crate::store::mailbox_rows::reconstruct_person(tx, &slug, &person)?;
            Ok((snapshot, seq))
        })
        .await
    }

    /// Apply a fence-FREE per-person mailbox DELTA (upsert/delete only the given
    /// envelopes) — the O(1)-append path, no whole-company snapshot. Returns the
    /// new max seq.
    /// `actor` is the AUTHENTICATED caller — who is asking, as opposed to
    /// `person`, which is whose mailbox. The two are deliberately separate
    /// arguments: the product calls this route in both directions, so they are
    /// equal on a consumption and different on a delivery. See
    /// `mailbox_rows::authorize_delta` for the rule.
    ///
    /// # Errors
    /// As []; a  (unmodeled-keys / mailbox-
    /// invalid / person-mismatch / foreign-delete / not-a-delivery) the route
    /// maps to 422.
    pub async fn mailbox_delta(
        &self,
        person: String,
        upserts: Vec<crate::store::mailbox_rows::MailboxEntry>,
        deletes: Vec<String>,
        at: String,
        actor: String,
    ) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        // Kept for the post-commit change-feed hint (below): the closure moves
        // its own copies, so the wake-relevant store key + stamp must be cloned
        // out here before `person`/`at` are consumed.
        let hint_store = crate::store::mailbox_rows::mailbox_store_name(&person);
        let hint_at = at.clone();
        let seq = self
            .in_transaction(MutationClass::Normal, MutationName("org.mailbox.delta"), move |tx| {
                crate::store::mailbox_rows::delta(
                    tx, &slug, &person, &upserts, &deletes, &at, &actor,
                )
            })
            .await?;
        // The write is durable — publish the `WatchEvent` the wake watcher
        // filters on (`event.store.startsWith("mailbox/")`). Bypassing the
        // Ledgers snapshot means `run_job` never did this for us.
        self.publish_row_feed_hint(&hint_store, &hint_at);
        Ok(seq)
    }

    /// DISTINCT person ids with at least one mailbox row (org-data-normalization
    /// P0, N8) — backs listMailboxPersonIds after the 3-family collapse.
    /// # Errors
    /// As CompanyDb::in_transaction; StoreFailure on a SQL fault.
    pub async fn mailbox_list_persons(&self) -> Result<Vec<String>, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Small,
            MutationName("org.mailbox.list-persons"),
            move |tx| crate::store::mailbox_rows::list_persons(tx, &slug),
        )
        .await
    }

    /// Stop admitting work, answering queued and future mutations with
    /// `Unavailable{reason}`.
    ///
    /// This is step 2 of `company.remove.plan` (plan §2.1): in-flight `mutate`
    /// futures resolve `Unavailable` and **never hang**. The job executing at
    /// the moment of the call runs to completion; nothing queued behind it
    /// does. Idempotent; the first reason wins.
    pub fn quiesce(&self, reason: &'static str) {
        let rejected = self.shared.lock().close_and_drain(reason);
        self.shared.wake.notify_all();
        // Answered on the calling thread, on purpose: waiting for the writer's
        // next scheduling pass would make this hang behind whatever long
        // reconcile is executing — the thing plan §2.1 says must never happen.
        for (job, err) in rejected {
            (job.finish)(Err(err));
        }
    }

    /// How many jobs are waiting. Diagnostics only: contention is queue depth,
    /// and this is how an operator sees it.
    #[must_use]
    pub fn queue_depth(&self) -> usize {
        self.shared.lock().depth()
    }

    /// Whether the actor is still admitting mutations.
    #[must_use]
    pub fn is_admitting(&self) -> bool {
        self.shared.lock().admission() == Admission::Open
    }

    /// Quiesce, drain, `wal_checkpoint(TRUNCATE)`, close the connection and
    /// join the thread — plan §2.1 step 3, in that order.
    ///
    /// Idempotent. After it returns, no file descriptor of this actor points
    /// into the company directory, which is the precondition for the quarantine
    /// rename in `company.remove.finalize`.
    pub fn shutdown(&self) {
        let rejected = {
            let mut queue = self.shared.lock();
            let rejected = queue.close_and_drain("stopping");
            queue.request_stop();
            rejected
        };
        self.shared.wake.notify_all();
        for (job, err) in rejected {
            (job.finish)(Err(err));
        }
        let handle = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(handle) = handle {
            if handle.join().is_err() {
                tracing::error!(company = %self.label, "writer thread panicked before exit");
            }
        }
    }
}

impl Drop for CompanyDb {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A row-authoritative organization projection prepared by a named staffing
/// mutation before its transaction commits.
///
/// `Ledgers` retains these serialized values only so existing in-process actor
/// readers can continue to use their stable APIs. The normalized organization
/// and activity rows are the sole durable authority; this type is deliberately
/// constructed from those rows and is never persisted through `documents`.
struct LiveOrganizationProjection {
    manifest: Option<String>,
    activity: Option<String>,
}

impl LiveOrganizationProjection {
    fn reconstruct(tx: &rusqlite::Transaction<'_>, slug: &str) -> Result<Self, ChiefdError> {
        let manifest = crate::store::organization_rows::reconstruct(tx, slug)?;
        let activity = match &manifest {
            Some(manifest) => crate::store::activity::rows::read_rows(tx, slug, manifest)
                .map_err(crate::store::activity::rows::activity_store_failed)?,
            None => None,
        };
        Ok(Self {
            manifest: manifest
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| store_failure("org-manifest-rows", e))?,
            activity: activity
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| store_failure("org-activity-rows", e))?,
        })
    }

    fn install(self, ledgers: &mut Ledgers) {
        let at = ledgers.now();
        match self.manifest {
            Some(body) => ledgers.load_document(
                crate::store::organization_rows::ORGANIZATION_MANIFEST_STORE.to_string(),
                DocumentRecord::from_row(body, at),
            ),
            None => {
                ledgers
                    .remove_document(crate::store::organization_rows::ORGANIZATION_MANIFEST_STORE);
            }
        }
        match self.activity {
            Some(body) => ledgers.load_document(
                crate::store::activity::rows::ACTIVITY_STORE.to_string(),
                DocumentRecord::from_row(body, at),
            ),
            None => {
                ledgers.remove_document(crate::store::activity::rows::ACTIVITY_STORE);
            }
        }
    }
}

fn load_ledgers(conn: &Connection, slug: &str, now: WallMillis) -> Result<Ledgers, OpenError> {
    let mut ledgers = Ledgers::empty(now);
    // `slug` is the company KEY — a directory hash — and every document
    // reconstructed below stamps the company's DISPLAY NAME into its derived
    // `organization`. That name is a stored fact, so read it once here.
    //
    // `None` is the ordinary pre-genesis state, not a failure: this writer boots
    // for a directory whose company is about to be created, and an unnamed
    // company has no rows in any of the stores below either.
    let company = {
        let tx = conn.unchecked_transaction()?;
        crate::store::org_settings::read_display_slug(&tx, slug).map_err(|e| {
            OpenError::CorruptJournal { detail: format!("org-settings rows unreadable: {e}") }
        })?
    };
    let company = company.as_deref();
    load_host_actions(conn, &mut ledgers)?;
    load_relational(conn, slug, company, &mut ledgers)?;
    // Health-monitor rows are the authority. Reconstruct a transient in-memory
    // projection before the writer starts, so existing readers see normalized
    // state without a persistent document copy or a version fence. Since the
    // F16 un-cross-wiring this is ONE store: the daemon's own health duty and
    // the TS launcher's health monitor both persist to `health_monitor_*`
    // (merge semantics, Step 3), so the duty's reconstructed view includes
    // everything TS committed and vice versa.
    //
    // Skipped entirely for an unnamed company: the document carries the
    // company's name, and there is none to carry. Nothing is lost — health rows
    // are written by a duty that reads the manifest first, so an unnamed
    // company has none.
    if let Some(company) = company {
        let tx = conn.unchecked_transaction()?;
        let health =
            crate::store::health_monitor_rows::reconstruct(&tx, slug, company).map_err(|e| {
                OpenError::CorruptJournal { detail: format!("health-monitor rows unreadable: {e}") }
            })?;
        if let Some(health) = health {
            if let Ok(body) = serde_json::to_string(&health) {
                ledgers.load_document(
                    crate::store::health_monitor_rows::HEALTH_MONITOR_STORE.to_string(),
                    DocumentRecord::from_row(body, now),
                );
            }
        }
    }
    // Organization rows are the authority. Reconstruct a transient in-memory
    // manifest projection before the writer starts; no document copy or
    // version fence is durable state. A short-lived read transaction is safe:
    // open() runs before this company's writer serves mutations.
    {
        let tx = conn.unchecked_transaction()?;
        let manifest = crate::store::organization_rows::reconstruct(&tx, slug).map_err(|e| {
            OpenError::CorruptJournal { detail: format!("org-manifest rows unreadable: {e}") }
        })?;
        if let Some(manifest) = &manifest {
            if let Ok(body) = serde_json::to_string(manifest) {
                ledgers.load_document(
                    crate::store::organization_rows::ORGANIZATION_MANIFEST_STORE.to_string(),
                    DocumentRecord::from_row(body, now),
                );
            }
        }
        // Activity rows are the authority. Reconstruct their transient
        // in-memory projection after the manifest because it references
        // people and departments; this still occurs before writer mutations.
        if let Some(manifest) = &manifest {
            let ledger =
                crate::store::activity::rows::read_rows(&tx, slug, manifest).map_err(|e| {
                    OpenError::CorruptJournal { detail: format!("activity rows unreadable: {e}") }
                })?;
            if let Some(ledger) = ledger {
                if let Ok(body) = serde_json::to_string(&ledger) {
                    ledgers.load_document(
                        crate::store::activity::rows::ACTIVITY_STORE.to_string(),
                        DocumentRecord::from_row(body, now),
                    );
                }
            }
        }
    }
    // Supervision meta rows are the authority. Reconstruct only their
    // transient in-memory projection: effects came from their
    // own relational tables in `load_relational`, and `serde(skip)` excludes
    // them from this representation. Named companies only — the ledger carries
    // the company's name, and supervision is seeded by genesis anyway.
    if let Some(company) = company {
        let tx = conn.unchecked_transaction()?;
        let supervision = crate::store::supervision::rows::reconstruct(&tx, slug, company)
            .map_err(|e| OpenError::CorruptJournal {
                detail: format!("supervision rows unreadable: {e}"),
            })?;
        if let Some(supervision) = supervision {
            if let Ok(body) = serde_json::to_string(&supervision) {
                ledgers.load_document(
                    crate::store::supervision::rows::SUPERVISION_STORE.to_string(),
                    DocumentRecord::from_row(body, now),
                );
            }
        }
    }
    // Converge-safety rows are the authority; this is only an in-memory
    // projection rebuilt before the writer begins serving mutations.
    {
        let tx = conn.unchecked_transaction()?;
        let state = crate::store::converge_safety_rows::reconstruct(&tx, slug).map_err(|e| {
            OpenError::CorruptJournal { detail: format!("converge-safety rows unreadable: {e}") }
        })?;
        if let Some(state) = state {
            if let Ok(body) = serde_json::to_string(&state) {
                ledgers.load_document(
                    crate::store::converge_safety_rows::CONVERGE_SAFETY_STORE.to_string(),
                    DocumentRecord::from_row(body, now),
                );
            }
        }
    }
    // Supervisor-watermark rows are likewise reconstructed into a transient
    // projection before this actor starts serving mutations. Named companies
    // only, for the same reason: the state carries the company's name, and
    // every duty that records a watermark reads the manifest first.
    if let Some(company) = company {
        let tx = conn.unchecked_transaction()?;
        let watermark = crate::store::supervisor_watermark_rows::reconstruct(&tx, slug, company)
            .map_err(|e| OpenError::CorruptJournal {
                detail: format!("supervisor-watermark rows unreadable: {e}"),
            })?;
        if let Some(watermark) = watermark {
            if let Ok(body) = serde_json::to_string(&watermark) {
                ledgers.load_document(
                    crate::store::supervisor_watermark_rows::SUPERVISOR_WATERMARK_STORE.to_string(),
                    DocumentRecord::from_row(body, now),
                );
            }
        }
    }
    // Session-maintenance rows are the authority. The domain mutators retain a
    // transient in-memory projection reconstructed from the normalized tables
    // at every boot, never a persistent document fallback.
    {
        let tx = conn.unchecked_transaction()?;
        let maintenance =
            crate::store::session_maintenance::rows::reconstruct(&tx, slug).map_err(|e| {
                OpenError::CorruptJournal {
                    detail: format!("session-maintenance rows unreadable: {e}"),
                }
            })?;
        if let Some(maintenance) = maintenance {
            if let Ok(body) = serde_json::to_string(&maintenance) {
                ledgers.load_document(
                    crate::store::session_maintenance::rows::SESSION_MAINTENANCE_STORE.to_string(),
                    DocumentRecord::from_row(body, now),
                );
            }
        }
    }
    Ok(ledgers)
}

/// Load the columnarized `mailbox` table into the ledgers, rebuilding each
/// envelope's DERIVED `recipients` (the sorted sibling set sharing the logical
/// `id`) and `organization` from the company's DISPLAY slug. The health
/// collector reads both values to distinguish malformed pending mail from a
/// stale delivery, so the actor snapshot must carry the same normalized shape as
/// the row port. schema Part B / Fable #7.
///
/// `company` is `None` only before genesis, when the company has no name — and
/// no people, so no mail either. Nothing to load and nothing to stamp.
fn load_mailbox_rows(
    conn: &Connection,
    slug: &str,
    company: Option<&str>,
    ledgers: &mut Ledgers,
) -> Result<(), OpenError> {
    let Some(company) = company else {
        return Ok(());
    };
    use crate::store::mailbox::{
        HealthIncidentRef, MailboxEnvelope, Urgency, MAILBOX_ENVELOPE_SCHEMA_VERSION,
    };
    struct Raw {
        envelope_id: String,
        id: String,
        person: String,
        from_person_id: String,
        to_person_id: String,
        message: String,
        urgency: String,
        reply_to: Option<String>,
        h_fp: Option<String>,
        h_kind: Option<String>,
        h_rcp: Option<String>,
        created_at: String,
        state: String,
        updated_at: i64,
    }
    let mut stmt = conn.prepare(
        "SELECT envelope_id, id, person, from_person_id, to_person_id, message, urgency, \
         reply_to, health_fingerprint, health_kind, health_recipient_person_id, \
         created_at, state, updated_at FROM mailbox WHERE slug = ?1",
    )?;
    let raws = stmt
        .query_map(rusqlite::params![slug], |r| {
            Ok(Raw {
                envelope_id: r.get(0)?,
                id: r.get(1)?,
                person: r.get(2)?,
                from_person_id: r.get(3)?,
                to_person_id: r.get(4)?,
                message: r.get(5)?,
                urgency: r.get(6)?,
                reply_to: r.get(7)?,
                h_fp: r.get(8)?,
                h_kind: r.get(9)?,
                h_rcp: r.get(10)?,
                created_at: r.get(11)?,
                state: r.get(12)?,
                updated_at: r.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let mut recipients: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for raw in &raws {
        recipients.entry(raw.id.clone()).or_default().push(raw.person.clone());
    }
    for people in recipients.values_mut() {
        people.sort();
        people.dedup();
    }

    for raw in raws {
        let health_incident = raw.h_fp.clone().map(|fingerprint| HealthIncidentRef {
            fingerprint,
            kind: raw.h_kind.clone().unwrap_or_default(),
            recipient_person_id: raw.h_rcp.clone().unwrap_or_default(),
        });
        let envelope = MailboxEnvelope {
            schema_version: MAILBOX_ENVELOPE_SCHEMA_VERSION,
            id: raw.id.clone(),
            organization: company.to_string(),
            from_person_id: raw.from_person_id,
            to: raw.to_person_id,
            recipients: recipients.get(&raw.id).cloned().unwrap_or_default(),
            body: raw.message,
            urgency: Urgency::parse(&raw.urgency).unwrap_or(Urgency::Normal),
            reply_to: raw.reply_to,
            health_incident,
            created_at: raw.created_at,
        };
        ledgers.load_mailbox(
            raw.envelope_id,
            MailboxRow {
                person: raw.person,
                envelope,
                state: raw.state,
                updated_at: raw.updated_at,
            },
        );
    }
    Ok(())
}

/// Persist one columnarized mailbox row. `envelope_id` is the `id@person`
/// composite PK; `recipients`/`organization`/`schemaVersion` are DERIVED and so
/// deliberately NOT written (schema Part B / Fable #7). Single-PK upsert.
fn write_mailbox_row(
    txn: &rusqlite::Transaction<'_>,
    slug: &str,
    envelope_id: &str,
    row: &crate::ledger::MailboxRow,
) -> rusqlite::Result<()> {
    let e = &row.envelope;
    let h = e.health_incident.as_ref();
    txn.execute(
        "INSERT INTO mailbox(slug, envelope_id, id, person, from_person_id, to_person_id, message, \
         urgency, reply_to, health_fingerprint, health_kind, \
         health_recipient_person_id, created_at, state, updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15) \
         ON CONFLICT(slug, envelope_id) DO UPDATE SET id=?3, person=?4, from_person_id=?5, \
         to_person_id=?6, message=?7, urgency=?8, reply_to=?9, health_fingerprint=?10, health_kind=?11, \
         health_recipient_person_id=?12, created_at=?13, state=?14, updated_at=?15",
        rusqlite::params![
            slug,
            envelope_id,
            e.id,
            row.person,
            e.from_person_id,
            e.to,
            e.body,
            e.urgency.as_str(),
            e.reply_to,
            h.map(|h| &h.fingerprint),
            h.map(|h| &h.kind),
            h.map(|h| &h.recipient_person_id),
            e.created_at,
            row.state,
            row.updated_at,
        ],
    )?;
    Ok(())
}

/// Load the M12 relational tables: effects, mailbox, counters.
///
/// Like the provider tables and unlike the host-action journal, every column is
/// a scalar the schema constrains, so there is no unreadable-phase hazard: a
/// row that disagrees with itself is caught by `validate()` on the first commit
/// rather than stopping the company from opening.
fn load_relational(
    conn: &Connection,
    slug: &str,
    company: Option<&str>,
    ledgers: &mut Ledgers,
) -> Result<(), OpenError> {
    let mut stmt = conn.prepare("SELECT id, seq, kind, status, created_at, delivered_at, superseded_at, delivery_failure_count, last_delivery_failure_at, failed_at, reopen_count, last_reopened_at FROM effects WHERE slug = ?1")?;
    let rows = stmt.query_map(rusqlite::params![slug], |row| {
        Ok((
            row.get::<_, String>(0)?,
            EffectRow { seq: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0), kind: row.get::<_, String>(2)?, body: serde_json::json!({"id":row.get::<_,String>(0)?,"sequence":row.get::<_,i64>(1)?,"type":row.get::<_,String>(2)?,"status":row.get::<_,String>(3)?,"createdAt":row.get::<_,String>(4)?,"deliveredAt":row.get::<_,Option<i64>>(5)?.map(crate::isotime::iso_millis),"supersededAt":row.get::<_,Option<String>>(6)?,"deliveryFailureCount":row.get::<_,Option<i64>>(7)?,"lastDeliveryFailureAt":row.get::<_,Option<String>>(8)?,"failedAt":row.get::<_,Option<String>>(9)?,"reopenCount":row.get::<_,Option<i64>>(10)?,"lastReopenedAt":row.get::<_,Option<String>>(11)?}).to_string(), delivered_at: row.get::<_, Option<i64>>(5)? },
        ))
    })?;
    for row in rows {
        let (id, mut effect) = row?;
        let payload = crate::store::supervision::rows::read_effect_payload(conn, slug, &id)
            .map_err(|error| match error {
                crate::store::supervision::rows::EffectPayloadError::Database(error) => {
                    OpenError::Sqlite(error)
                }
                crate::store::supervision::rows::EffectPayloadError::Invalid(detail) => {
                    OpenError::CorruptJournal {
                        detail: format!("effect '{id}' payload rows are unreadable: {detail}"),
                    }
                }
            })?;
        let mut object: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&effect.body).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
        for (field, value) in payload {
            if object.insert(field.clone(), value).is_some() {
                return Err(OpenError::CorruptJournal {
                    detail: format!(
                        "effect '{id}' payload field '{field}' collides with a core effect column"
                    ),
                });
            }
        }
        effect.body = serde_json::Value::Object(object).to_string();
        ledgers.load_effect(id, effect);
    }
    drop(stmt);

    load_mailbox_rows(conn, slug, company, ledgers)?;

    // delta #36: counters are per-company via slug-named rows (the D2 convention
    // the row path uses, `<name>:<slug>`), NOT a slug column. Load only THIS
    // company's counters and strip the `:<slug>` suffix back to the bare
    // in-memory key the M12 ledger reads (NEXT_EFFECT_SEQUENCE, etc.) — this is
    // the load-side of the cross-company leak the write path already closes.
    let mut stmt = conn.prepare("SELECT name, value FROM counters")?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
    let suffix = format!(":{slug}");
    for row in rows {
        let (name, value) = row?;
        if let Some(base) = name.strip_suffix(&suffix) {
            ledgers.load_counter(base.to_string(), value);
        }
    }
    Ok(())
}

/// Load the open host-transaction intents (plan §5.6).
///
/// A row whose phase is not one of the three durable spellings is **not**
/// skipped and **not** defaulted: journals are fail-closed (plan §5.5), and
/// either guess loses real work — "closed" abandons a filesystem rollback,
/// "pending" rolls back a publish that had already completed. The company fails
/// to open instead, which is a loud, isolated, recoverable state (§7.2).
fn load_host_actions(conn: &Connection, ledgers: &mut Ledgers) -> Result<(), OpenError> {
    let mut stmt = conn.prepare(
        "SELECT action_id, kind, payload_schema, plan_json, phase, created_at FROM host_actions",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    for row in rows {
        let (action_id, kind, payload_schema, plan_json, phase, created_at) = row?;
        let expected_schema = crate::host_action::payload_schema_for_kind(&kind);
        if payload_schema != expected_schema {
            return Err(OpenError::CorruptJournal {
                detail: format!(
                    "host action '{action_id}' kind '{kind}' has payload schema \
                     '{payload_schema}', expected '{expected_schema}'"
                ),
            });
        }
        if !matches!(
            serde_json::from_str::<serde_json::Value>(&plan_json),
            Ok(serde_json::Value::Object(_))
        ) {
            return Err(OpenError::CorruptJournal {
                detail: format!(
                    "host action '{action_id}' payload is not a schema-discriminated JSON object"
                ),
            });
        }
        let phase = HostActionPhase::parse(&phase).ok_or_else(|| OpenError::CorruptJournal {
            detail: format!("host action '{action_id}' has unreadable phase '{phase}'"),
        })?;
        ledgers.load_host_action(
            action_id,
            HostActionRecord::from_row(kind, plan_json, phase, WallMillis(created_at)),
        );
    }
    Ok(())
}

fn writer_loop(shared: &Arc<Shared>, conn: &mut Connection) {
    loop {
        let action = {
            let mut queue = shared.lock();
            loop {
                let now = shared.clock.monotonic();
                match queue.next(now, shared.aging, shared.deadline) {
                    Next::Idle => {
                        let (guard, _) = shared
                            .wake
                            .wait_timeout(queue, shared.deadline)
                            .unwrap_or_else(|poison| poison.into_inner());
                        queue = guard;
                    }
                    Next::Run(job) => {
                        // Publish `current` while the queue guard is still
                        // held. `queue_snapshot` takes these guards in this
                        // same order, so a request sees this job either still
                        // queued or already current — never falsely idle.
                        let started = shared.clock.monotonic();
                        // Actor-owned reads deliberately carry no mutation
                        // name, so they must not be surfaced as the current
                        // mutation in this diagnostic.
                        let current = job.name.map(|name| CurrentJob {
                            name,
                            class: job.class,
                            enqueued_ms: u64::try_from(job.waited(started).as_millis())
                                .unwrap_or(u64::MAX),
                        });
                        *shared.current.lock().unwrap_or_else(|poison| poison.into_inner()) =
                            current;
                        break Next::Run(job);
                    }
                    other => break other,
                }
            }
        };

        match action {
            Next::Run(job) => {
                // #905: `run_job` itself clears `shared.current` now, on
                // every exit path, immediately before it calls `finish` —
                // see the comment there. Clearing it again here would be
                // redundant at best; at worst a future job type that skips
                // `run_job`'s wrapped `finish` could rely on this line to
                // paper over a missing clear instead of the bug being
                // visible. Single source of truth: `run_job` alone owns
                // when `current` transitions back to `None`.
                run_job(shared, conn, job);
            }
            Next::Reject(rejects) => {
                for (job, err) in rejects {
                    (job.finish)(Err(err));
                }
            }
            Next::Stop => break,
            Next::Idle => {}
        }
    }

    // `TRUNCATE` rather than `PASSIVE`: the WAL must not be left behind for a
    // directory that may be about to be quarantined (plan §2.1 step 3).
    if let Err(err) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)") {
        tracing::warn!(error = %err, "wal checkpoint on writer shutdown failed");
    }
}

/// Run one job: `BEGIN IMMEDIATE` → closure → `validate` → persist → `COMMIT`.
///
/// The working set is a clone of the last committed snapshot, so every
/// non-commit path rolls back the in-memory ledger as totally as the dropped
/// transaction rolls back the disk.
fn run_job(shared: &Arc<Shared>, conn: &mut Connection, job: Job) {
    let Job { name, txn: txn_step, post_commit, apply, finish, .. } = job;
    // #905: `finish` delivers the outcome via a oneshot `send`, which is a
    // real synchronizes-with edge — the awaiting caller is guaranteed to see
    // everything that ran on this thread *before* the send. `writer_loop`
    // used to clear `shared.current` in a separate statement *after*
    // `run_job` returned, with no ordering relationship to a send that had
    // already fired; an awoken caller could then call `queue_snapshot` on a
    // different thread and still see `current` (and therefore `depth`)
    // counting a job it had just learned had committed. Wrapping `finish`
    // here, so the clear happens on every exit path immediately before the
    // caller is notified, closes that gap: whichever thread the caller
    // resumes on is guaranteed to see `current` already cleared.
    let finish: FinishFn = {
        let shared = Arc::clone(shared);
        Box::new(move |outcome| {
            *shared.current.lock().unwrap_or_else(|poison| poison.into_inner()) = None;
            finish(outcome);
        })
    };
    let Some(name) = name else {
        finish(Err(store_failure_because(
            COMPANY_DB_STORE,
            "a job was dispatched onto the writer with no MutationName; the writer runs \
             writes only, and a read belongs on CompanyDb's read-only pool",
        )));
        return;
    };
    let base = shared.snapshot.load_full();

    let txn = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(txn) => txn,
        Err(err) => {
            finish(Err(begin_failure(err, shared.deadline, name)));
            return;
        }
    };

    // The transaction-scoped step (if any) runs *first* and
    // *inside* this transaction. A lost fence therefore rolls back a
    // transaction that has written nothing at all: no ledger clone was even
    // taken, so there is no state a loser could publish.
    if let Some(step) = txn_step {
        if let Err(err) = step(&txn) {
            drop(txn);
            finish(Err(err));
            return;
        }
    }

    let mut working = base.ledgers().clone();
    working.set_now(shared.clock.wall());

    if let Err(error) = apply(&mut working) {
        // Dropping the transaction rolls back; dropping `working` discards the
        // partially-applied in-memory state the closure may have left behind.
        // A `Conflict` from a fenced store op takes exactly this path, so a
        // fence loser publishes nothing.
        drop(txn);
        finish(Err(error));
        return;
    }

    // Plan §5.1: validate runs after every mutation, before commit. #123: only
    // the document bodies this commit CHANGED are re-parsed — every unchanged
    // body was already validated before it was persisted (single writer), so
    // re-parsing the whole ~1 MB `documents` ledger on every commit was pure
    // waste. `open_with` runs a full `validate()` once at load so on-disk
    // corruption of an untouched body is still caught. Every non-document rule
    // still runs whole (cheap; row-count proportional).
    if let Err(refusal) = working.validate_since(base.ledgers()) {
        drop(txn);
        finish(Err(ChiefdError::Refused(refusal)));
        return;
    }

    if let Err(err) = persist(&txn, &shared.label, base.ledgers(), &working) {
        drop(txn);
        tracing::error!(op = %name, error = %err, "persisting a mutation failed");
        finish(Err(err));
        return;
    }

    if let Err(err) = txn.commit() {
        tracing::error!(op = %name, error = %err, "committing a mutation failed");
        finish(Err(write_failure(&err)));
        return;
    }

    // A named normalized organization operation prepares this projection while
    // it still has the transaction's read view. Now that commit made those rows
    // durable, install it before publishing the next actor snapshot. The next
    // queued supervision/mailbox/task operation therefore observes the new
    // roster without a daemon reload or a disk/JSON fallback.
    if let Some(refresh) = post_commit {
        refresh(&mut working);
    }

    // #376: publish a change-feed hint for every store this commit actually
    // touched, after it becomes durable. Computed against
    // `base.ledgers()`/`working` BEFORE
    // `working` is moved into the new snapshot below. A company with no sink
    // installed (`set_change_feed_sink` never called) publishes nothing.
    let sink = shared.feed_sink.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let commit_id = base.commit_seq().saturating_add(1);
    if let Some(sink) = sink {
        for (store, record) in working.changed_since(base.ledgers()) {
            sink(&shared.label, store, record.body(), &record.updated_at().to_iso8601(), false);
        }
        for store in working.removed_since(base.ledgers()) {
            sink(&shared.label, &store, "", "", true);
        }
    }

    let next = LedgerSnapshot::committed(working, commit_id);
    shared.snapshot.store(Arc::new(next));
    finish(Ok(()));
}

/// The company database is intact and the machine cannot take the write.
///
/// `Unavailable` rather than a store error because the two ask an operator for
/// opposite things: a store error says *the database would not serve this*,
/// and this says *the storage under it will not accept a write*. The bytes are fine, and the request becomes servable again the
/// moment the disk does — which is what 503 means and 500 does not.
const STORAGE_UNWRITABLE: &str = "storage-unwritable";

/// Classify a raw SQLite failure at the company-database boundary.
///
/// Every site here used to answer `Corrupt{company-db}` for anything that was
/// not `BUSY`, which put a full filesystem, a failing disk and a genuine
/// unreadable page under one word. The measured case is the first: on
/// 2026-08-10 a build host's `/tmp` tmpfs filled, and the tool-contract suite
/// watched chiefd answer `corrupt store: company-db` to thirteen consecutive
/// write routes whose database was untouched. Three agents then reported that
/// as a reproducible product defect on clean `main`; it was a full disk.
///
/// **The code an out-of-space SQLite actually returns is `SQLITE_IOERR`, not
/// `SQLITE_FULL`.** Measured, not assumed: an `sqlite3` insert against a
/// deliberately filled 8 MB ext4 loop mount answers `disk I/O error (10)`.
/// `SQLITE_FULL` is here too because it is the same condition reached by the
/// other route (a page-count ceiling, or a VFS that reports it), and because a
/// match arm that fires only on the code the filesystem does *not* send is a
/// fix that never runs.
///
/// Anything else falls through to [`write_failure`], which narrows the label
/// further; this classifier never widens into a fail-open.
fn storage_failure(err: &rusqlite::Error) -> Option<ChiefdError> {
    matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::SystemIoFailure | rusqlite::ErrorCode::DiskFull)
    )
    .then_some(ChiefdError::Unavailable { reason: STORAGE_UNWRITABLE })
}

/// [`storage_failure`], then the company-database label — and the label is
/// `Corrupt` for exactly two SQLite codes.
///
/// `SQLITE_CORRUPT` and `SQLITE_NOTADB` are SQLite saying the file it was
/// handed is not the value it must be: the pages are malformed, or the header
/// is not a database at all. That is a decode failure and an operator should
/// be told their bytes are damaged. A constraint violation, a schema change, a
/// type mismatch or any other code is a [`ChiefdError::StoreFailure`] — the
/// database is fine and something else went wrong, which is what this label
/// used to claim was corruption for every code it could not name.
fn write_failure(err: &rusqlite::Error) -> ChiefdError {
    storage_failure(err).unwrap_or_else(|| {
        if matches!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
        ) {
            corrupt_store(COMPANY_DB_STORE, err)
        } else {
            crate::error::store_failure(COMPANY_DB_STORE, err)
        }
    })
}

/// Classify a `BEGIN IMMEDIATE` failure.
fn begin_failure(err: rusqlite::Error, waited: Duration, op: MutationName) -> ChiefdError {
    let busy = matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    );
    if busy {
        tracing::warn!(op = %op, "sqlite reported BUSY after the full busy_timeout");
        return ChiefdError::Busy(BusyProof::after_waiting(waited, QUEUE_BUSY_SITE));
    }
    tracing::error!(op = %op, error = %err, "beginning a write transaction failed");
    write_failure(&err)
}

fn persist(
    txn: &rusqlite::Transaction<'_>,
    slug: &str,
    previous: &Ledgers,
    working: &Ledgers,
) -> Result<(), ChiefdError> {
    // Every ledger store must be normalized before this cutover. A missing
    // dispatch arm is a programmer error, so fail closed rather than recreating
    // a document blob fallback.
    let mut changed = working.changed_since(previous);
    changed.sort_by_key(|(store, _)| {
        (*store != crate::store::organization_rows::ORGANIZATION_MANIFEST_STORE, *store)
    });
    for (store, record) in changed {
        match crate::store::persist_dispatch::dispatch_persist(txn, slug, store, record.body()) {
            Some(Ok(())) => {}
            // A dispatch refusal (item-D unmodeled keys, a validation rule the
            // row port owns) is a CALLER error, not corruption: propagate it
            // verbatim so the route maps it to its 422 instead of mislabeling
            // the commit "corrupt store: company-db" (500). `dispatch_persist`'s
            // own contract reserves `Some(Err(_))` for exactly this.
            Some(Err(error)) => {
                tracing::error!(store, error = %error, "row-dispatch persist failed");
                return Err(error);
            }
            None => {
                tracing::error!(store, "unwired store rejected after documents DROP");
                return Err(store_failure_because(
                    COMPANY_DB_STORE,
                    format!("store '{store}' has no persist dispatch arm"),
                ));
            }
        }
    }
    let removal_at = working.now().to_iso8601();
    for store in working.removed_since(previous) {
        match crate::store::persist_dispatch::dispatch_clear(txn, slug, &store, &removal_at) {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                tracing::error!(store, error = %error, "row-dispatch clear failed");
                return Err(error);
            }
            None => {
                tracing::error!(store, "unwired store clear rejected after documents DROP");
                return Err(store_failure_because(
                    COMPANY_DB_STORE,
                    format!("store '{store}' has no clear dispatch arm"),
                ));
            }
        }
    }
    // A genuine SQL failure below this point is a fault at the company boundary
    // (delta #49's mislabeling class, kept): the caller cannot route on it, so
    // it takes a `StoreFailure{company-db}` label — and only the two codes that
    // mean "this file is not a database" still say `Corrupt`. See
    // [`storage_failure`] and [`write_failure`]: a filesystem with no room left
    // answered `corrupt store: company-db` here for every write on a full build
    // host, and a constraint violation said exactly the same thing.
    persist_relational_tail(txn, slug, previous, working).map_err(|err| write_failure(&err))
}

fn persist_relational_tail(
    txn: &rusqlite::Transaction<'_>,
    slug: &str,
    previous: &Ledgers,
    working: &Ledgers,
) -> rusqlite::Result<()> {
    // Host-transaction intents ride the *same* transaction as the documents.
    // That is the whole mechanism of plan §5.6 commit 2: the manifest advance
    // and the intent close are atomic with respect to each other, so a crash
    // can never leave a manifest that advanced with an intent still open (a
    // recovery pass would then replay a completed transaction) or an intent
    // closed with the manifest behind (the effect would be lost forever).
    let (changed, removed) = crate::ledger::host_action_diff(previous, working);
    for (action_id, record) in changed {
        txn.execute(
            "INSERT INTO host_actions(action_id, kind, payload_schema, plan_json, phase, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(action_id) DO UPDATE SET kind = ?2, payload_schema = ?3, \
             plan_json = ?4, phase = ?5, created_at = ?6",
            rusqlite::params![
                action_id,
                record.kind(),
                record.payload_schema(),
                record.plan_json(),
                record.phase().as_str(),
                record.created_at().0,
            ],
        )?;
    }
    for action_id in removed {
        txn.execute("DELETE FROM host_actions WHERE action_id = ?1", rusqlite::params![action_id])?;
    }
    // The M12 relational tables ride the same transaction as the documents,
    // so a reader can never observe an effect row without the document write
    // that produced it, or the reverse.
    let relational = crate::ledger::relational_diff(previous, working);
    for (id, row) in relational.effects {
        let effect: crate::store::supervision::Effect = serde_json::from_str(&row.body)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        txn.execute(
            "INSERT INTO effects(slug, id, seq, kind, status, created_at, delivered_at, superseded_at, delivery_failure_count, last_delivery_failure_at, failed_at, reopen_count, last_reopened_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
             ON CONFLICT(slug, id) DO UPDATE SET seq=?3, kind=?4, status=?5, created_at=?6, delivered_at=?7, superseded_at=?8, delivery_failure_count=?9, last_delivery_failure_at=?10, failed_at=?11, reopen_count=?12, last_reopened_at=?13",
            rusqlite::params![
                slug,
                id,
                i64::try_from(row.seq).unwrap_or(i64::MAX),
                row.kind,
                effect.status.as_str(),
                effect.created_at,
                effect.delivered_at.as_deref().and_then(crate::isotime::parse_iso_millis),
                effect.superseded_at,
                effect.delivery_failure_count.map(i64::from),
                effect.last_delivery_failure_at,
                effect.failed_at,
                effect.reopen_count.map(i64::from),
                effect.last_reopened_at,
            ],
        )?;
        crate::store::supervision::rows::replace_effect_payload(txn, slug, id, &effect.payload)
            .map_err(|error| match error {
                crate::store::supervision::rows::EffectPayloadError::Database(error) => error,
                crate::store::supervision::rows::EffectPayloadError::Invalid(detail) => {
                    rusqlite::Error::ToSqlConversionFailure(detail.into())
                }
            })?;
    }
    for id in relational.removed_effects {
        txn.execute(
            "DELETE FROM effects WHERE slug = ?1 AND id = ?2",
            rusqlite::params![slug, id],
        )?;
    }
    for (envelope_id, row) in relational.mailbox {
        write_mailbox_row(txn, slug, envelope_id, row)?;
    }
    for envelope_id in relational.removed_mailbox {
        txn.execute(
            "DELETE FROM mailbox WHERE slug = ?1 AND envelope_id = ?2",
            rusqlite::params![slug, envelope_id],
        )?;
    }
    for (name, value) in relational.counters {
        // delta #36: slug-name the counter (D2 `<name>:<slug>`) so persist and
        // the supervision row path agree on the same per-company counter row.
        txn.execute(
            "INSERT INTO counters(name, value) VALUES (?1, ?2) \
             ON CONFLICT(name) DO UPDATE SET value = ?2",
            rusqlite::params![format!("{name}:{slug}"), value],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::time::Instant;

    use crate::actor::AGING_INTERVAL;
    use crate::store::COMPANY_DB_FILENAME;
    use crate::test_support::ManualClock;

    /// A company actor on a temp directory with a clock the test owns.
    struct Harness {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
        clock: Arc<ManualClock>,
        db: Arc<CompanyDb>,
    }

    impl Harness {
        /// An actor for a directory whose company does NOT exist yet — the
        /// ordinary pre-genesis state, and the right fixture for any test whose
        /// subject is genesis itself.
        fn open() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join(COMPANY_DB_FILENAME);
            let clock = Arc::new(ManualClock::default());
            let db = CompanyDb::open("e2eco", &path, clock.clone()).expect("open");
            Self { _dir: dir, path, clock, db: Arc::new(db) }
        }

        /// An actor for a company that EXISTS: genesis has run and named it.
        ///
        /// The actor's label is the company KEY, and every normalized store
        /// stamps the company's DISPLAY name into a derived field — a name only
        /// `org_settings` carries, and only genesis writes. Persisting a store
        /// into a company genesis has never named is writing to a company that
        /// does not exist, which the dispatch now refuses; a test about writer
        /// scheduling, snapshots or reopen wants a real company underneath it.
        ///
        /// Genesis runs through `organization_rows::genesis` on a raw
        /// connection BEFORE the actor opens, so no commit is charged to the
        /// actor and `commit_seq` still starts at 0.
        fn open_named() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join(COMPANY_DB_FILENAME);
            {
                let mut conn = crate::store::open_company_db(&path).expect("create company db");
                let tx = conn.transaction().expect("genesis txn");
                let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
                manifest.slug = "e2eco".to_string();
                crate::store::organization_rows::genesis(&tx, "e2eco", &manifest)
                    .expect("genesis names the company");
                tx.commit().expect("commit genesis");
            }
            let clock = Arc::new(ManualClock::default());
            let db = CompanyDb::open("e2eco", &path, clock.clone()).expect("open");
            Self { _dir: dir, path, clock, db: Arc::new(db) }
        }
    }

    /// Wait for a condition another *thread* must make true.
    ///
    /// This is not the forbidden "loop N times hoping to hit a window"
    /// (TESTING.md §1.2): the condition is a state the writer thread is
    /// guaranteed to reach, and the loop is how an async test observes an OS
    /// thread. A test that reaches the bound fails loudly rather than passing
    /// flakily.
    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let started = std::time::Instant::now();
        while !cond() {
            assert!(started.elapsed() < Duration::from_secs(10), "timed out waiting for: {what}");
            tokio::task::yield_now().await;
        }
    }

    /// A mutation that parks the writer thread until the returned sender fires.
    ///
    /// This is the "named pause point" TESTING.md §1.2 asks for: the contender
    /// is held at a known point, and the test decides when it releases.
    struct Barrier {
        release: mpsc::Sender<()>,
        entered: Arc<AtomicBool>,
    }

    fn barrier(
        order: &Arc<Mutex<Vec<String>>>,
    ) -> (Barrier, impl FnOnce(&mut Ledgers) -> Result<(), ChiefdError> + Send + 'static) {
        let (release, gate) = mpsc::channel::<()>();
        let entered = Arc::new(AtomicBool::new(false));
        let entered_in_closure = Arc::clone(&entered);
        let order = Arc::clone(order);
        let body = move |ledgers: &mut Ledgers| {
            entered_in_closure.store(true, Ordering::SeqCst);
            let _ = gate.recv();
            order.lock().unwrap_or_else(|p| p.into_inner()).push("barrier".to_string());
            touch_normalized_store(ledgers);
            Ok(())
        };
        (Barrier { release, entered }, body)
    }

    fn recorder(
        order: &Arc<Mutex<Vec<String>>>,
        name: String,
    ) -> impl FnOnce(&mut Ledgers) -> Result<(), ChiefdError> + Send + 'static {
        let order = Arc::clone(order);
        move |ledgers: &mut Ledgers| {
            order.lock().unwrap_or_else(|p| p.into_inner()).push(name.clone());
            touch_normalized_store(ledgers);
            Ok(())
        }
    }

    fn order_log() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn snapshot_of(order: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        order.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Exercise the writer through a real normalized store. Writer scheduling
    /// tests do not need an invented ledger key; converge-safety is a
    /// self-contained row-native singleton whose update is a normal commit.
    fn touch_normalized_store(ledgers: &mut Ledgers) {
        crate::store::converge_safety::set_actuation_config(
            ledgers,
            crate::store::converge_safety::ActuationMode::Shadow,
            false,
            false,
        );
    }

    /// Build the `rusqlite::Error` SQLite hands back for a primary result code.
    fn sqlite_error(code: std::os::raw::c_int) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    }

    /// Real SQLite, out of room, through the real classifier.
    ///
    /// `PRAGMA max_page_count` is how a hermetic test reaches the same
    /// condition a full filesystem reaches, on both macOS and Linux, without a
    /// mount and without a file: SQLite refuses to grow past the ceiling and
    /// answers `SQLITE_FULL`. Before this, the writer answered `corrupt store:
    /// company-db` — the store being perfectly readable at the time.
    #[test]
    fn a_database_that_cannot_grow_is_unavailable_and_never_a_store_failure() {
        let conn = rusqlite::Connection::open_in_memory().expect("open");
        conn.execute_batch("CREATE TABLE a(x TEXT)").expect("schema");
        conn.pragma_update(None, "max_page_count", 2).expect("ceiling");

        let mut error = None;
        for _ in 0..64 {
            if let Err(err) = conn.execute("INSERT INTO a VALUES (hex(randomblob(4000)))", []) {
                error = Some(err);
                break;
            }
        }
        let error = error.expect("a database capped at two pages must refuse to grow");
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DiskFull),
            "the condition under test must really be SQLite running out of room"
        );
        assert!(
            matches!(
                write_failure(&error),
                ChiefdError::Unavailable { reason } if reason == STORAGE_UNWRITABLE
            ),
            "no room is not damaged bytes"
        );
    }

    /// The code a real ENOSPC actually sends.
    ///
    /// Measured on 2026-08-10 against a deliberately filled 8 MB ext4 loop
    /// mount: SQLite answers `disk I/O error (10)` — `SQLITE_IOERR` — and never
    /// `SQLITE_FULL`. This is the arm that would have saved thirteen tests and
    /// three agents' afternoons, and it is the one a fix written from the
    /// obvious guess would have missed.
    #[test]
    fn the_io_error_a_full_filesystem_really_sends_is_unavailable_not_corrupt() {
        let error = sqlite_error(rusqlite::ffi::SQLITE_IOERR);
        assert_eq!(error.sqlite_error_code(), Some(rusqlite::ErrorCode::SystemIoFailure));
        assert!(
            matches!(
                write_failure(&error),
                ChiefdError::Unavailable { reason } if reason == STORAGE_UNWRITABLE
            ),
            "an ENOSPC write must not accuse the store of corruption"
        );
    }

    /// The narrowing must not become a fail-open: everything the classifier
    /// cannot name is still the company database being unusable.
    #[test]
    fn only_a_malformed_database_reports_corruption_and_the_rest_report_a_store_failure() {
        for code in [rusqlite::ffi::SQLITE_CORRUPT, rusqlite::ffi::SQLITE_NOTADB] {
            assert!(
                matches!(
                    write_failure(&sqlite_error(code)),
                    ChiefdError::Corrupt { store, .. } if store == COMPANY_DB_STORE
                ),
                "code {code} is SQLite saying the file is not a database: it is corruption"
            );
        }
        for code in [
            rusqlite::ffi::SQLITE_CONSTRAINT,
            rusqlite::ffi::SQLITE_SCHEMA,
            rusqlite::ffi::SQLITE_MISMATCH,
        ] {
            assert!(
                matches!(
                    write_failure(&sqlite_error(code)),
                    ChiefdError::StoreFailure { store, .. } if store == COMPANY_DB_STORE
                ),
                "code {code} says nothing about damaged bytes and must not claim any"
            );
        }
    }

    /// `BEGIN` has three answers, not two. Contention is still `Busy` — the
    /// property the ladder depends on — and a full disk is no longer folded in
    /// with corruption on the way past it.
    #[test]
    fn beginning_a_transaction_tells_contention_from_no_room_from_corruption() {
        let op = MutationName("converge-safety");
        let waited = Duration::from_millis(250);
        assert!(matches!(
            begin_failure(sqlite_error(rusqlite::ffi::SQLITE_BUSY), waited, op),
            ChiefdError::Busy(_)
        ));
        assert!(matches!(
            begin_failure(sqlite_error(rusqlite::ffi::SQLITE_IOERR), waited, op),
            ChiefdError::Unavailable { reason } if reason == STORAGE_UNWRITABLE
        ));
        assert!(matches!(
            begin_failure(sqlite_error(rusqlite::ffi::SQLITE_NOTADB), waited, op),
            ChiefdError::Corrupt { store, .. } if store == COMPANY_DB_STORE
        ));
        assert!(matches!(
            begin_failure(sqlite_error(rusqlite::ffi::SQLITE_CONSTRAINT), waited, op),
            ChiefdError::StoreFailure { store, .. } if store == COMPANY_DB_STORE
        ));
    }

    fn person_identity<'a>(fingerprint: &'a str) -> crate::store::identities::NewIdentity<'a> {
        crate::store::identities::NewIdentity {
            identity_id: "person:alix",
            principal: "person:alix",
            kind: crate::store::identities::IdentityKind::Person,
            company_slug: Some("e2eco"),
            pubkey: Some("spki-alix"),
            fingerprint,
            enrolled_by: Some("operator"),
        }
    }

    #[tokio::test]
    async fn identity_actor_read_enrol_rotate_and_revoke_round_trip() {
        let h = Harness::open();
        assert_eq!(h.db.identity_read("person:alix".to_owned()).await.expect("read absent"), None);
        assert!(h.db.identity_enroll(person_identity("fp-1")).await.expect("enrol"));
        assert!(!h
            .db
            .identity_enroll(person_identity("fp-1"))
            .await
            .expect("same key is idempotent"));
        let conflict = h.db.identity_enroll(person_identity("fp-2")).await.expect_err("re-key");
        assert_eq!(conflict.code(), Some("auth-identity-fingerprint-conflict"));

        let enrolled =
            h.db.identity_read("person:alix".to_owned())
                .await
                .expect("read enrolled")
                .expect("identity present");
        assert_eq!(enrolled.fingerprint, "fp-1");
        assert!(enrolled.active);
        assert_eq!(enrolled.enrolled_by.as_deref(), Some("operator"));

        assert!(h
            .db
            .identity_rotate_fingerprint("person:alix".to_owned(), "fp-rotated".to_owned())
            .await
            .expect("rotate"));
        assert!(h
            .db
            .identity_revoke("person:alix".to_owned(), 1_700_000_000_123)
            .await
            .expect("revoke"));
        assert!(!h
            .db
            .identity_revoke("person:alix".to_owned(), 1_700_000_000_124)
            .await
            .expect("revoke is idempotent"));
        let revoked =
            h.db.identity_read("person:alix".to_owned())
                .await
                .expect("read revoked")
                .expect("identity remains a revocation anchor");
        assert_eq!(revoked.fingerprint, "fp-rotated");
        assert!(!revoked.active);
        assert_eq!(revoked.revoked_at, Some(1_700_000_000_123));
    }

    #[tokio::test]
    async fn identity_mutation_failures_leave_company_rows_unchanged() {
        let h = Harness::open();
        assert!(h.db.identity_enroll(person_identity("fp-1")).await.expect("seed"));
        let trigger_conn = open_company_db(&h.path).expect("test trigger connection");

        trigger_conn
            .execute_batch(
                "CREATE TRIGGER abort_identity_enrol BEFORE INSERT ON identities
                 BEGIN SELECT RAISE(ABORT, 'injected enrol failure'); END;",
            )
            .expect("install enrol trigger");
        let enrol = h.db.identity_enroll(crate::store::identities::NewIdentity {
            identity_id: "person:bea",
            principal: "person:bea",
            kind: crate::store::identities::IdentityKind::Person,
            company_slug: Some("e2eco"),
            pubkey: Some("spki-bea"),
            fingerprint: "fp-bea",
            enrolled_by: None,
        });
        assert!(enrol.await.is_err(), "the injected enrol failure reaches the caller");
        assert_eq!(
            h.db.identity_read("person:bea".to_owned()).await.expect("read after enrol failure"),
            None,
            "a failed enrol must not leave a partial row"
        );

        trigger_conn
            .execute_batch(
                "DROP TRIGGER abort_identity_enrol;
                 CREATE TRIGGER abort_identity_rotation BEFORE UPDATE OF fingerprint ON identities
                 BEGIN SELECT RAISE(ABORT, 'injected rotate failure'); END;",
            )
            .expect("install rotate trigger");
        assert!(h
            .db
            .identity_rotate_fingerprint("person:alix".to_owned(), "fp-rotated".to_owned())
            .await
            .is_err());
        let after_rotate_failure =
            h.db.identity_read("person:alix".to_owned())
                .await
                .expect("read after rotate failure")
                .expect("seed identity remains");
        assert_eq!(after_rotate_failure.fingerprint, "fp-1");
        assert!(after_rotate_failure.active);
        assert_eq!(after_rotate_failure.revoked_at, None);

        trigger_conn
            .execute_batch(
                "DROP TRIGGER abort_identity_rotation;
                 CREATE TRIGGER abort_identity_revoke BEFORE UPDATE OF active ON identities
                 BEGIN SELECT RAISE(ABORT, 'injected revoke failure'); END;",
            )
            .expect("install revoke trigger");
        assert!(h.db.identity_revoke("person:alix".to_owned(), 1_700_000_000_123).await.is_err());
        let after_revoke_failure =
            h.db.identity_read("person:alix".to_owned())
                .await
                .expect("read after revoke failure")
                .expect("seed identity remains");
        assert_eq!(after_revoke_failure.fingerprint, "fp-1");
        assert!(after_revoke_failure.active);
        assert_eq!(after_revoke_failure.revoked_at, None);
    }

    #[test]
    fn identity_read_is_not_registered_as_a_mutation() {
        let source = include_str!("writer.rs");
        let read_mutation_name = format!("{}{}", "auth.identity.", "read");
        assert!(
            !source.contains(&read_mutation_name),
            "identity reads must not acquire a MutationName"
        );
        assert_eq!(
            source.matches("MutationName(\"auth.identity.").count(),
            3,
            "only enrol, rotate-fingerprint, and revoke are auth mutations"
        );
    }

    /// The final DROP contract: inspect SQLite's catalog rather than querying
    /// the retired table (which would make this test suite depend on it).
    fn documents_table_is_absent(path: &Path) -> bool {
        let conn = open_company_db(path).expect("reopen for read-back");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'documents'",
                [],
                |row| row.get(0),
            )
            .expect("inspect schema");
        count == 0
    }

    fn hired_worker_seed() -> crate::store::org_ops::OwnedNewPersonSeed {
        use crate::store::organization::{EmploymentState, PersonKind};

        crate::store::org_ops::OwnedNewPersonSeed {
            name: "Quinn".to_string(),
            title: "Research Engineer".to_string(),
            mandate: "Own the assigned research work and return verified results.".to_string(),
            kind: PersonKind::Worker,
            employment_state: EmploymentState::Active,
            activation: "on-demand".to_string(),
            tools: vec!["read".to_string()],
            prompts: vec![],
        }
    }

    /// A READ COMMITS NOTHING. This is the rule the whole `desired` path rests
    /// on, and it had no test at all.
    ///
    /// Every `*_read` used to go through `in_transaction`, which is the WRITE
    /// path: `BEGIN IMMEDIATE`, a deep clone of the whole `Ledgers` — every
    /// mailbox body and effect row in company history — full-ledger validation,
    /// two structural diffs, `persist`, `COMMIT`, a changefeed publication and a
    /// new snapshot. `/v1/org/runtime/desired` did five of those per request and
    /// ran at a 925ms median on the operator's box.
    ///
    /// `commit_seq` is the honest witness: `run_job` advances it once per
    /// COMMIT, so it counts write transactions and nothing else. A read that
    /// moves it is a read paying write prices, whatever the queue label says.
    #[tokio::test]
    async fn reads_commit_nothing_and_publish_no_snapshot() {
        let h = Harness::open_named();
        // One real commit first, so the test is proving that reads leave a
        // MOVING counter alone rather than that nothing ever moves it.
        h.db.mutate(MutationClass::Small, MutationName("goal.set"), |l| {
            touch_normalized_store(l);
            Ok(())
        })
        .await
        .expect("mutation commits");
        let after_one_write = h.db.read(LedgerSnapshot::commit_seq);
        assert_eq!(after_one_write, 1, "the one write committed");

        // EVERY read on this type, not only the `desired` path's five. The rule
        // is about the CLASS of operation, so a test that named one route would
        // let the next read to be written go back onto the write path.
        for _ in 0..2 {
            let _ = h.db.org_manifest_read().await.expect("manifest read");
            let _ = h.db.activity_read().await.expect("activity read");
            let _ = h.db.runtime_read().await.expect("runtime read");
            let _ = h.db.converge_safety_read().await.expect("converge safety read");
            let _ = h.db.org_manifest_and_activity_read().await.expect("manifest + activity read");
            let _ = h.db.org_current_seq().await.expect("seq read");
            let _ = h.db.org_settings_read().await.expect("settings read");
            let _ = h.db.supervision_read().await.expect("supervision read");
            let _ = h.db.session_maintenance_read().await.expect("session maintenance read");
            let _ = h.db.session_epoch_read().await.expect("session epoch read");
            let _ = h.db.goal_delivery_quiesce_read().await.expect("quiesce read");
            let _ = h.db.runtime_owner_read().await.expect("runtime owner read");
            let _ = h.db.launch_intent_read().await.expect("launch intent read");
            let _ = h.db.mutation_journal_read().await.expect("mutation journal read");
            let _ = h.db.health_monitor_read().await.expect("health monitor read");
            let _ = h.db.mailbox_read().await.expect("mailbox read");
            let _ = h.db.org_person_contracts_read().await.expect("person contracts read");
            let _ = h.db.operator_escalation_push_read().await.expect("escalation push read");
            let _ = h.db.operator_escalation_intents_read().await.expect("escalation intents read");
            let _ = h.db.operator_escalation_log().await.expect("escalation log read");
            let _ = h.db.session_maintenance_ledger().await;
            let _ = h.db.runtime_ownership_read().await;
        }

        assert_eq!(
            h.db.read(LedgerSnapshot::commit_seq),
            after_one_write,
            "these reads committed {} write transactions — each one a BEGIN IMMEDIATE, a whole \
             ledger clone, a full validation, two structural diffs and a changefeed \
             publication, to answer a question that changes nothing",
            h.db.read(LedgerSnapshot::commit_seq) - after_one_write
        );
    }

    #[tokio::test]
    async fn a_mutation_commits_once_and_publishes_a_snapshot() {
        let h = Harness::open_named();
        assert_eq!(h.db.read(LedgerSnapshot::commit_seq), 0);

        h.db.mutate(MutationClass::Small, MutationName("goal.set"), |l| {
            touch_normalized_store(l);
            Ok(())
        })
        .await
        .expect("mutation commits");

        assert_eq!(h.db.read(LedgerSnapshot::commit_seq), 1, "every commit publishes a snapshot");
        assert_eq!(
            h.db.read(|s| crate::store::converge_safety::read(s.ledgers())
                .into_parts()
                .0
                .actuation_mode),
            crate::store::converge_safety::ActuationMode::Shadow,
        );
        let conn = open_company_db(&h.path).expect("open row database");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM converge_safety", [], |row| row.get(0))
            .expect("count rows");
        assert_eq!(rows, 1, "the normalized row was committed");
    }

    #[tokio::test]
    async fn a_post_boot_hire_refreshes_the_live_roster_and_stays_desired_off() {
        use crate::store::{activity, organization};

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        h.db.mutate(MutationClass::Normal, MutationName("seed-live-org"), move |ledgers| {
            organization::create(ledgers, &manifest)?;
            activity::seed(ledgers, &manifest)?;
            Ok(())
        })
        .await
        .expect("seed a live pre-hire actor snapshot");
        assert!(
            !h.db
                .read(|snapshot| organization::read(snapshot.ledgers()).expect("live manifest"))
                .people
                .contains_key("quinn"),
            "the pre-hire actor snapshot is the condition that previously went stale"
        );

        h.db.hire_person(
            "quinn".to_string(),
            "quant".to_string(),
            hired_worker_seed(),
            Some("quant-head".to_string()),
            "2026-07-28T00:00:00.000Z".to_string(),
            "quant-head".to_string(),
            PublishBarrier::none(),
        )
        .await
        .expect("public-style named hire succeeds");

        h.db.read(|snapshot| {
            let manifest = organization::read(snapshot.ledgers()).expect("live manifest refreshed");
            assert!(
                manifest.people.contains_key("quinn"),
                "the next live supervision/task operation must see the hire without restart"
            );
            let activity =
                activity::read(snapshot.ledgers(), &manifest).expect("live activity refreshed");
            assert!(
                !activity.people["quinn"].last_desired_active,
                "a durable hire refreshes the actor snapshot but never starts a pane"
            );
        });
    }

    type SinkCall = (String, String, String, String, bool);

    fn recording_sink() -> (Arc<ChangeFeedSink>, Arc<Mutex<Vec<SinkCall>>>) {
        let calls: Arc<Mutex<Vec<SinkCall>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let sink: Arc<ChangeFeedSink> =
            Arc::new(move |label: &str, store: &str, body: &str, updated_at: &str, removed| {
                recorded.lock().unwrap_or_else(|p| p.into_inner()).push((
                    label.to_string(),
                    store.to_string(),
                    body.to_string(),
                    updated_at.to_string(),
                    removed,
                ));
            });
        (sink, calls)
    }

    #[tokio::test]
    async fn a_commit_with_no_sink_installed_publishes_nothing_and_still_commits() {
        // Default behavior (every existing caller, pre-#376): a company that
        // never calls `set_change_feed_sink` must work exactly as before.
        let h = Harness::open_named();
        h.db.mutate(MutationClass::Small, MutationName("goal.set"), |l| {
            touch_normalized_store(l);
            Ok(())
        })
        .await
        .expect("mutation commits with no sink installed");
    }

    #[tokio::test]
    async fn a_commit_invokes_the_installed_sink_once_per_changed_store_after_commit() {
        let h = Harness::open_named();
        let (sink, calls) = recording_sink();
        h.db.set_change_feed_sink(sink);

        h.db.mutate(MutationClass::Small, MutationName("goal.set"), |l| {
            touch_normalized_store(l);
            Ok(())
        })
        .await
        .expect("commits");

        let mut seen = calls.lock().unwrap_or_else(|p| p.into_inner()).clone();
        seen.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(seen.len(), 1, "one publish for the real normalized store mutation");
        assert_eq!(seen[0].0, "e2eco", "the sink is called with the company's own label");
        assert_eq!(seen[0].1, "converge-safety");
        assert!(!seen[0].4, "an upsert is never `removed`");
    }

    #[tokio::test]
    async fn a_publish_barrier_runs_after_the_commit_and_before_the_change_event() {
        // THE ordering contract. The `chief-cli` actuator parks on the
        // `org-manifest` changefeed and converges tmux from it, so a hire's
        // staged home must be promoted onto its final path BEFORE that event
        // exists. The publish happens on the writer thread between
        // `txn.commit()` and the caller's `await` resolving, so a route cannot
        // do this itself after the call returns — the barrier is the only
        // window there is.
        use crate::store::{activity, organization};

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        h.db.mutate(MutationClass::Normal, MutationName("seed-live-org"), move |ledgers| {
            organization::create(ledgers, &manifest)?;
            activity::seed(ledgers, &manifest)?;
            Ok(())
        })
        .await
        .expect("seed a live pre-hire actor snapshot");

        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_order = Arc::clone(&order);
        h.db.set_change_feed_sink(Arc::new(
            move |_label: &str, store: &str, _body: &str, _at: &str, _removed| {
                sink_order.lock().unwrap_or_else(|p| p.into_inner()).push(format!("event:{store}"));
            },
        ));
        let barrier_order = Arc::clone(&order);
        h.db.hire_person(
            "quinn".to_string(),
            "quant".to_string(),
            hired_worker_seed(),
            Some("quant-head".to_string()),
            "2026-08-11T00:00:00.000Z".to_string(),
            "quant-head".to_string(),
            PublishBarrier::new(Box::new(move || {
                barrier_order.lock().unwrap_or_else(|p| p.into_inner()).push("promote".to_owned());
            })),
        )
        .await
        .expect("the hire commits");

        let seen = order.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(seen.first().map(String::as_str), Some("promote"), "seen: {seen:?}");
        assert!(
            seen.iter().any(|entry| entry
                == &format!(
                    "event:{}",
                    crate::store::organization_rows::ORGANIZATION_MANIFEST_STORE
                )),
            "the hire publishes the org-manifest change the actuator converges from: {seen:?}"
        );
        assert!(
            seen.iter().skip(1).all(|entry| entry.starts_with("event:")),
            "the promote is complete before the FIRST event, not merely before the last: {seen:?}"
        );
    }

    #[tokio::test]
    async fn a_rolled_back_mutation_never_invokes_the_sink() {
        // A refusal must publish NOTHING — the same "fence loser publishes
        // nothing" property `a_refusal_from_the_closure_rolls_the_transaction_back`
        // already pins for the snapshot; the change-feed hint must honor it too,
        // or a subscriber would learn about a write that never actually landed.
        let h = Harness::open();
        let (sink, calls) = recording_sink();
        h.db.set_change_feed_sink(sink);

        let err =
            h.db.mutate(MutationClass::Normal, MutationName("org.department.add"), |l| {
                touch_normalized_store(l);
                Err::<(), _>(ChiefdError::refused("test-refusal", "test-only rollback"))
            })
            .await
            .expect_err("a refusing closure must not commit");
        assert!(matches!(err, ChiefdError::Refused(_)));
        assert!(
            calls.lock().unwrap_or_else(|p| p.into_inner()).is_empty(),
            "a rolled-back mutation must not publish a change-feed hint"
        );
    }

    #[tokio::test]
    async fn clearing_a_normalized_store_invokes_the_sink_with_removed_true() {
        let h = Harness::open_named();
        h.db.mutate(MutationClass::Small, MutationName("seed"), |l| {
            touch_normalized_store(l);
            Ok(())
        })
        .await
        .expect("seed commits");

        let (sink, calls) = recording_sink();
        h.db.set_change_feed_sink(sink);
        h.db.mutate(MutationClass::Small, MutationName("org.remove"), |l| {
            crate::store::converge_safety::clear(l);
            Ok(())
        })
        .await
        .expect("remove commits");

        let seen = calls.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, "converge-safety");
        assert_eq!(seen[0].2, "", "a removal carries no body -- there is no content to mirror");
        assert!(seen[0].4, "a removal must be reported as removed");
    }

    #[tokio::test]
    async fn a_later_sink_replaces_the_earlier_one_rather_than_stacking() {
        let h = Harness::open_named();
        let (first_sink, first_calls) = recording_sink();
        let (second_sink, second_calls) = recording_sink();
        h.db.set_change_feed_sink(first_sink);
        h.db.set_change_feed_sink(second_sink);

        h.db.mutate(MutationClass::Small, MutationName("goal.set"), |l| {
            touch_normalized_store(l);
            Ok(())
        })
        .await
        .expect("commits");

        assert!(
            first_calls.lock().unwrap_or_else(|p| p.into_inner()).is_empty(),
            "the replaced sink must not still be called"
        );
        assert_eq!(second_calls.lock().unwrap_or_else(|p| p.into_inner()).len(), 1);
    }

    #[tokio::test]
    async fn every_journal_row_of_one_commit_carries_the_writers_single_wall_reading() {
        let h = Harness::open();
        h.clock.advance(Duration::from_secs(5));
        h.db.mutate(MutationClass::Normal, MutationName("org.hire"), |l| {
            l.put_host_action("one", HostActionRecord::pending("test", "{}", l.now()));
            l.put_host_action("two", HostActionRecord::pending("test", "{}", l.now()));
            Ok(())
        })
        .await
        .expect("commit");
        let stamps = h.db.read(|s| {
            (
                s.host_action("one").map(HostActionRecord::created_at),
                s.host_action("two").map(HostActionRecord::created_at),
            )
        });
        assert_eq!(stamps.0, stamps.1);
        assert_eq!(stamps.0, Some(WallMillis(1_700_000_005_000)));
    }

    /// The CAS-mutator hazard, closed: a mutation runs exactly once even when
    /// the caller vanishes mid-flight (TESTING.md §3.1, enqueue-then-cancel).
    #[tokio::test]
    async fn a_mutation_runs_exactly_once_even_when_the_caller_drops_its_future() {
        let h = Harness::open_named();
        let order = order_log();
        let (gate, blocking) = barrier(&order);

        let blocker_db = Arc::clone(&h.db);
        let blocker = tokio::spawn(async move {
            blocker_db.mutate(MutationClass::Small, MutationName("barrier"), blocking).await
        });
        wait_until("the barrier mutation to occupy the writer", || {
            gate.entered.load(Ordering::SeqCst)
        })
        .await;

        let runs = Arc::new(AtomicU32::new(0));
        let runs_in_closure = Arc::clone(&runs);
        let victim_db = Arc::clone(&h.db);
        let victim = tokio::spawn(async move {
            victim_db
                .mutate(MutationClass::Small, MutationName("goal.set"), move |l| {
                    runs_in_closure.fetch_add(1, Ordering::SeqCst);
                    touch_normalized_store(l);
                    Ok(())
                })
                .await
        });

        wait_until("the second mutation to be queued", || h.db.queue_depth() == 1).await;

        // The caller goes away while its mutation is queued.
        victim.abort();
        assert!(victim.await.is_err(), "the caller's future was dropped");

        gate.release.send(()).expect("release the barrier");
        blocker.await.expect("join").expect("barrier commits");

        wait_until("the orphaned mutation to commit", || {
            h.db.read(LedgerSnapshot::commit_seq) == 2
        })
        .await;

        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "exactly once — never re-invoked, never skipped"
        );
        assert_eq!(
            h.db.read(|s| crate::store::converge_safety::read(s.ledgers())
                .into_parts()
                .0
                .actuation_mode),
            crate::store::converge_safety::ActuationMode::Shadow,
            "a dropped connection does not un-commit a transaction"
        );
    }

    // --- E8-S2 (#824): queue_snapshot diagnostics -----------------------------

    /// An idle writer's queue snapshot is exactly the shape `GET
    /// /v1/docs/queue` answers on a quiet daemon: zero depth, zero age, no
    /// `current`. Never an error — diagnostics never block, never enqueue.
    #[tokio::test]
    async fn queue_snapshot_idle() {
        let h = Harness::open();
        let snapshot = h.db.queue_snapshot();
        assert_eq!(snapshot.depth, 0);
        assert_eq!(snapshot.oldest_enqueued_ms, 0);
        assert!(snapshot.current.is_none());
    }

    /// One job running, one job waiting: `current` names the running job (and
    /// how long IT waited before starting), `depth` counts BOTH jobs because
    /// neither has committed, and the oldest queued job's age grows while it
    /// waits.
    #[tokio::test]
    async fn queue_snapshot_reports_waiting_job() {
        let h = Harness::open_named();
        let order = order_log();
        let (gate, blocking) = barrier(&order);

        let blocker_db = Arc::clone(&h.db);
        let blocker = tokio::spawn(async move {
            blocker_db.mutate(MutationClass::Small, MutationName("running.job"), blocking).await
        });
        wait_until("the barrier mutation to occupy the writer", || {
            gate.entered.load(Ordering::SeqCst)
        })
        .await;

        // The running job must be visible as `current` while it holds the writer.
        wait_until("queue_snapshot to report the running job", || {
            h.db.queue_snapshot().current.is_some()
        })
        .await;
        let mid_flight = h.db.queue_snapshot();
        let current = mid_flight.current.expect("a job is running");
        assert_eq!(
            mid_flight.depth, 1,
            "the current job is accepted but not yet committed, so it counts toward depth"
        );
        assert_eq!(current.name.0, "running.job");
        assert_eq!(current.class, MutationClass::Small);

        let waiting_db = Arc::clone(&h.db);
        let waiting = tokio::spawn(async move {
            waiting_db
                .mutate(MutationClass::Normal, MutationName("waiting.job"), |l| {
                    touch_normalized_store(l);
                    Ok(())
                })
                .await
        });
        wait_until("the second job to be queued", || h.db.queue_snapshot().depth == 2).await;
        let queued = h.db.queue_snapshot();
        assert_eq!(
            queued.depth, 2,
            "both the running and waiting jobs are accepted but not yet committed"
        );
        assert_eq!(
            queued.current.expect("still running").name.0,
            "running.job",
            "current still names the job that is actually executing, not the one waiting"
        );

        gate.release.send(()).expect("release the barrier");
        blocker.await.expect("join").expect("barrier commits");
        waiting.await.expect("join").expect("waiting job commits");

        let after = h.db.queue_snapshot();
        assert_eq!(after.depth, 0, "both jobs have committed");
        assert!(after.current.is_none(), "the writer is idle again");
    }

    /// #905 regression: `current` (and therefore `depth`) must already be
    /// cleared by the moment a `mutate()` future resolves for its caller —
    /// not merely "usually" by then, as it was before `run_job` was made to
    /// clear it itself immediately before calling `finish`.
    ///
    /// This deliberately does NOT poll (`wait_until`) or iterate looking for
    /// a window: it asserts on the very next line after the single `.await`
    /// that delivers the outcome, with no other task or thread given a
    /// chance to run first. Before the #905 fix this was genuinely racy —
    /// `writer_loop` cleared `current` in a statement with no ordering
    /// relationship to the oneshot `send` inside `finish`, so an awoken
    /// caller could observe `current` still `Some` here on an unlucky
    /// scheduling. After the fix, `finish`'s wrapped clear happens-before its
    /// own `send`, so any thread the caller resumes on is guaranteed to see
    /// `current` already `None` — this test pins that guarantee directly
    /// rather than by re-running the flake until it happens to pass.
    #[tokio::test]
    async fn current_is_cleared_before_the_caller_is_notified() {
        let h = Harness::open_named();

        h.db.mutate(MutationClass::Small, MutationName("solo.job"), |l| {
            touch_normalized_store(l);
            Ok(())
        })
        .await
        .expect("solo job commits");

        // No `wait_until`, no yield, no sleep: the guarantee under test is
        // that this is already true on the very first observation after the
        // `.await` above returns.
        let snapshot = h.db.queue_snapshot();
        assert_eq!(
            snapshot.depth, 0,
            "current must already be cleared by the time mutate() resolves for its caller"
        );
        assert!(
            snapshot.current.is_none(),
            "current must already be cleared by the time mutate() resolves for its caller"
        );
    }

    /// `queue_snapshot` is read-only: calling it never enqueues a job and never
    /// advances the committed sequence — the D19 evidence this story adds no
    /// SQLite mutation at all.
    #[tokio::test]
    async fn queue_snapshot_never_mutates() {
        let h = Harness::open();
        let before = h.db.read(LedgerSnapshot::commit_seq);
        for _ in 0..5 {
            let _ = h.db.queue_snapshot();
        }
        let after = h.db.read(LedgerSnapshot::commit_seq);
        assert_eq!(before, after, "queue_snapshot never commits anything");
    }

    /// A read is not a mutation, so the E8-S2 diagnostic must never invent a
    /// mutation name for one — and now that reads run on their own connections,
    /// an in-flight read must not appear on the writer's queue at all.
    #[tokio::test]
    async fn queue_snapshot_does_not_label_a_read_as_mutation() {
        let h = Harness::open();
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let read_entered = Arc::clone(&entered);
        let read_release = Arc::clone(&release);
        let db = Arc::clone(&h.db);
        let read = tokio::spawn(async move {
            db.read_pooled(move |_| {
                read_entered.store(true, Ordering::SeqCst);
                while !read_release.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                Ok(())
            })
            .await
        });

        wait_until("the read to start", || entered.load(Ordering::SeqCst)).await;
        let snapshot = h.db.queue_snapshot();
        release.store(true, Ordering::SeqCst);
        read.await.expect("join read").expect("read succeeds");

        assert_eq!(snapshot.depth, 0, "a read is absent from mutation queue depth");
        assert!(snapshot.current.is_none(), "a read is not a current mutation");
    }

    /// **A READ NEVER WAITS FOR A WRITE.** This is the rule the whole read path
    /// now rests on, and its absence is what made the operator's box unusable.
    ///
    /// `read_txn` correctly stopped reads from taking `BEGIN IMMEDIATE`, but a
    /// read still queued onto the writer's single thread and single connection.
    /// So the daemon could serve exactly ONE read at a time, and every read
    /// waited behind whatever mutation was already running. While reads were
    /// themselves write transactions this was hidden, because `BEGIN IMMEDIATE`
    /// throttled the CALLERS too — the old binary never exceeded three requests
    /// in flight. Cheap reads removed the throttle and not the cap: arrival
    /// concurrency reached 104 against a one-at-a-time server, and p95 read
    /// latency went from 576ms to 18,850ms.
    ///
    /// This test states the rule as a fact about time: a write is parked, and a
    /// read must still answer. On the old code the read is behind the barrier in
    /// the writer's queue and cannot answer until the write is released, so the
    /// test hangs and fails on its own timeout. It is RED on every version of
    /// this file before reads got their own connections.
    #[tokio::test]
    async fn a_read_answers_while_a_write_is_parked() {
        let h = Harness::open_named();
        // Park the writer thread on a mutation that will not finish until we
        // say so. This is TESTING.md §1.2's named pause point, not a sleep.
        let order = order_log();
        let (gate, body) = barrier(&order);
        let db = Arc::clone(&h.db);
        let parked = tokio::spawn(async move {
            db.mutate(MutationClass::Normal, MutationName("parked.write"), body).await
        });
        wait_until("the write to occupy the writer thread", || gate.entered.load(Ordering::SeqCst))
            .await;

        // The writer is now occupied. A read must not care.
        let read =
            tokio::time::timeout(Duration::from_secs(10), h.db.org_manifest_read()).await.expect(
                "a read must answer while a write is parked; if this timed out the read is \
                 queued behind the writer again",
            );
        read.expect("the read itself succeeds");

        // Only now let the write finish, proving it really was parked for the
        // whole read rather than having completed before it.
        drop(gate);
        parked.await.expect("join parked write").expect("the parked write commits");
    }

    /// **A BURST OF READS MUST NOT DELAY THE OPERATOR'S WRITE.**
    ///
    /// The read-side rules above say a read does not wait for a write. This is
    /// the other direction, and it is the one the operator feels: their click —
    /// `POST /v1/org/person/wake`, a [`MutationClass::Normal`] job — must not be
    /// pushed down the queue by the company's own read traffic.
    ///
    /// # Why this rule needs a test of its own
    ///
    /// the design record states that the wake "waits ~2s **by
    /// scheduler design**", because [`AgingPolicy::should_admit_aged`] admits a
    /// lower-priority op past a `Small` stream only after 32 consecutive `Small`
    /// ops or a 2-second window. That was TRUE when it was written: every
    /// company read enqueued a job here, in class `Small` and with no name
    /// (`read_on_actor`, deleted by `70ec7d376`), so read traffic *was* the
    /// `Small` stream and the operator's write sat behind it.
    ///
    /// Reads moved to their own connection pool, which removed that stream as a
    /// side effect. A side effect is not a guarantee — routing any read back
    /// through this queue would silently restore a multi-second wait on the one
    /// write a human is watching — so the guarantee is stated here.
    ///
    /// # A CONTROLLED EXPERIMENT, BECAUSE A ONE-ARMED VERSION WOULD BE VACUOUS
    ///
    /// Asserting only "the probe was fast" would pass on the old path too: on a
    /// small fixture 32 queued reads are 32 cheap jobs, so the probe would clear
    /// them in milliseconds and the test would certify a defect it cannot see.
    ///
    /// So both arms run the IDENTICAL work — the same closure, the same count,
    /// the same cost — and differ in one thing only: which lane it travels. The
    /// writer is parked on a barrier first, so in both arms the probe is
    /// enqueued behind a known queue and the clock starts at the release. The
    /// second arm is a faithful reconstruction of the pre-`70ec7d376` read path,
    /// and its measured wait is what makes the first arm's number mean
    /// something.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_read_burst_does_not_delay_the_operators_write() {
        /// Reads in the burst. The operator's box was measured at a peak of 104
        /// requests in flight; 64 is inside that and above the 32-op aging bound
        /// with room to spare.
        const BURST: usize = 64;
        /// What one of them costs. Fixed rather than measured so both arms carry
        /// exactly the same work and the comparison is about the lane.
        const EACH: Duration = Duration::from_millis(20);

        /// Which lane the burst travels.
        enum Lane {
            /// Today: the read-only connection pool.
            Pool,
            /// Before `70ec7d376`: a `Small` job on this queue, per read.
            WriterQueue,
        }

        /// Park the writer, queue `BURST` units of work on `lane`, enqueue the
        /// operator's `Normal` probe behind them, then release — and report how
        /// long the probe waited from release to the start of its own job.
        async fn probe_wait_behind(path: &std::path::Path, lane: Lane) -> Duration {
            let db = Arc::new(
                CompanyDb::open("e2eco", path, Arc::new(crate::clock::SystemClock::default()))
                    .expect("open"),
            );
            // The writer is held here for the whole setup, so nothing in the
            // burst can start early and the two arms begin from the same state.
            let (release, gate) = mpsc::channel::<()>();
            let parked_entered = Arc::new(AtomicBool::new(false));
            let entered_in_closure = Arc::clone(&parked_entered);
            let parked = {
                let db = Arc::clone(&db);
                tokio::spawn(async move {
                    db.mutate(MutationClass::Normal, MutationName("parked.write"), move |_| {
                        entered_in_closure.store(true, Ordering::SeqCst);
                        let _ = gate.recv();
                        Ok(())
                    })
                    .await
                })
            };
            wait_until("the writer to park", || parked_entered.load(Ordering::SeqCst)).await;

            // The work itself: one fixed unit that touches no SQLite state, so
            // the only difference between the arms is which queue it occupies.
            //
            // `std::thread::sleep` is banned in this workspace because a test
            // must never sleep for a condition another thread makes true — this
            // is not that. It is the WORK under measurement, and it is a sleep
            // rather than a spin on purpose: a spin would put 64 CPU-burning
            // units against four worker threads, and the arms would then differ
            // in CPU contention as well as in lane, which is the one thing this
            // experiment must hold constant. Every real wait below is
            // `wait_until`.
            #[allow(clippy::disallowed_methods)]
            fn one_unit_of_work() {
                std::thread::sleep(EACH);
            }
            let queued = Arc::new(AtomicU32::new(0));
            let mut burst = Vec::with_capacity(BURST);
            for _ in 0..BURST {
                let db = Arc::clone(&db);
                let queued = Arc::clone(&queued);
                burst.push(match lane {
                    Lane::Pool => tokio::spawn(async move {
                        queued.fetch_add(1, Ordering::SeqCst);
                        db.read_pooled(move |_| {
                            one_unit_of_work();
                            Ok(())
                        })
                        .await
                    }),
                    Lane::WriterQueue => tokio::spawn(async move {
                        queued.fetch_add(1, Ordering::SeqCst);
                        db.mutate(MutationClass::Small, MutationName("read.on.actor"), move |_| {
                            one_unit_of_work();
                            Ok(())
                        })
                        .await
                    }),
                });
            }
            wait_until("the burst to be in flight", || {
                queued.load(Ordering::SeqCst) as usize == BURST
            })
            .await;
            // What the burst put on the WRITE queue, beside the parked job. This
            // is the difference between the arms, stated as a number: on the
            // pool the burst is invisible here, which is the whole point.
            let occupied = match lane {
                Lane::Pool => 1,
                Lane::WriterQueue => BURST + 1,
            };
            wait_until("the burst to reach its lane", || db.queue_snapshot().depth >= occupied)
                .await;

            // The operator's click, enqueued behind whatever the burst built.
            // The closure runs when the writer admits this job, so the instant it
            // records IS enqueue → start, measured from the release below rather
            // than from here — the setup in between is not the operator's wait.
            let started = Arc::new(Mutex::new(None::<Instant>));
            let started_in_closure = Arc::clone(&started);
            let probe = {
                let db = Arc::clone(&db);
                tokio::spawn(async move {
                    db.mutate(MutationClass::Normal, MutationName("org.person.wake"), move |_| {
                        *started_in_closure.lock().unwrap_or_else(|p| p.into_inner()) =
                            Some(Instant::now());
                        Ok(())
                    })
                    .await
                })
            };
            // The writer resumes only once the probe is genuinely queued behind
            // the burst, so neither arm can win by racing.
            wait_until("the probe to be queued", || db.queue_snapshot().depth > occupied).await;

            let released_at = Instant::now();
            drop(release);
            parked.await.expect("join parked").expect("parked write commits");
            probe.await.expect("join probe").expect("probe commits");
            for handle in burst {
                handle.await.expect("join burst").expect("burst unit succeeds");
            }
            let started_at = started.lock().unwrap_or_else(|p| p.into_inner()).take();
            started_at.expect("the probe recorded when its job started") - released_at
        }

        let dir = tempfile::tempdir().expect("tempdir");
        // A company each, so the second arm never inherits the first's WAL or
        // page cache.
        let through_the_pool = probe_wait_behind(&dir.path().join("pool.db"), Lane::Pool).await;
        let through_the_queue =
            probe_wait_behind(&dir.path().join("queue.db"), Lane::WriterQueue).await;

        // ARM 2 FIRST: if the reconstruction of the old path does not itself
        // reproduce the delay, this test proves nothing about arm 1 and must say
        // so rather than passing. 32 is the aging bound the scheduler enforces;
        // half of it is a floor no scheduling accident reaches.
        assert!(
            through_the_queue > EACH * 16,
            "the counterfactual arm did not reproduce the queue delay ({through_the_queue:?}), \
             so the fast arm below is not evidence of anything — this test would be certifying \
             a defect it cannot see"
        );
        // ARM 1: the rule.
        assert!(
            through_the_pool < AGING_INTERVAL / 4,
            "the operator's write waited {through_the_pool:?} behind a burst of {BURST} reads. \
             A read has re-entered the writer's queue, which puts the one write a human is \
             watching back into the {AGING_INTERVAL:?} aging window"
        );
        // And the two lanes must stay TELLABLE APART, which the absolute bound
        // above cannot check on its own: shrink `EACH` and a queued probe fits
        // inside the aging interval while still being queued. A quarter is wide
        // enough that a loaded CI box does not trip it and narrow enough that a
        // probe which actually waited behind the burst cannot pass.
        assert!(
            through_the_pool < through_the_queue / 4,
            "the operator's write waited {through_the_pool:?} behind reads on the pool against \
             {through_the_queue:?} behind the same work on this queue; the two lanes are no \
             longer distinguishable, so reads are back on the write path"
        );
    }

    /// Reads run AT THE SAME TIME as each other, not merely off the write path.
    ///
    /// A bounded pool is still a cap, so this pins that the cap is above one:
    /// two reads must overlap. Serialized reads cannot overlap however fast they
    /// are, so this is RED on the old actor-queued read path for a reason no
    /// amount of per-read speed can fix.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_reads_overlap_in_time() {
        let h = Harness::open();
        // Two counters and a DEADLINE, not a rendezvous that can only be
        // satisfied or hang: a serialized read path must fail this test, and a
        // test that deadlocks the harness instead of failing is not a test.
        let inside = Arc::new(AtomicU32::new(0));
        let observed_overlap = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let db = Arc::clone(&h.db);
            let inside = Arc::clone(&inside);
            let seen = Arc::clone(&observed_overlap);
            handles.push(tokio::spawn(async move {
                db.read_pooled(move |_| {
                    inside.fetch_add(1, Ordering::SeqCst);
                    // Wait, bounded, for the OTHER read to be inside too.
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    while std::time::Instant::now() < deadline {
                        if inside.load(Ordering::SeqCst) >= 2 {
                            seen.store(true, Ordering::SeqCst);
                            break;
                        }
                        std::thread::yield_now();
                    }
                    inside.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
            }));
        }
        for handle in handles {
            handle.await.expect("join read").expect("read succeeds");
        }
        assert!(
            observed_overlap.load(Ordering::SeqCst),
            "two reads never overlapped: the read path is still serialized, so concurrency \
             can only turn into queue latency"
        );
    }

    #[tokio::test]
    async fn a_refusal_from_the_closure_rolls_the_transaction_back() {
        let h = Harness::open_named();
        h.db.mutate(MutationClass::Normal, MutationName("seed"), |l| {
            touch_normalized_store(l);
            Ok(())
        })
        .await
        .expect("seed commits");

        let err =
            h.db.mutate(MutationClass::Normal, MutationName("org.department.add"), |l| {
                // The closure mutates and *then* declines — the shape that made
                // re-invoked CAS mutators dangerous.
                touch_normalized_store(l);
                Err::<(), _>(ChiefdError::refused("test-refusal", "test-only rollback"))
            })
            .await
            .expect_err("a refusing closure must not commit");

        match err {
            ChiefdError::Refused(refusal) => assert_eq!(refusal.code, "test-refusal"),
            other => panic!("expected Refused, got {other:?}"),
        }
        assert_eq!(h.db.read(LedgerSnapshot::commit_seq), 1, "a refusal publishes no snapshot");
        assert_eq!(
            h.db.read(|s| crate::store::converge_safety::read(s.ledgers())
                .into_parts()
                .0
                .actuation_mode),
            crate::store::converge_safety::ActuationMode::Shadow,
            "the normalized ledger rolled back too"
        );
        assert!(documents_table_is_absent(&h.path));
    }

    /// Plan §5.3: reads serve from the snapshot, so they never queue.
    ///
    /// Deterministic by construction — the read is issued while the writer is
    /// parked inside a barrier mutation and *must* return before the test
    /// releases it. A `read()` that queued would hang here, not flake.
    #[tokio::test]
    async fn reads_do_not_queue_behind_a_blocked_writer() {
        let h = Harness::open_named();
        h.db.mutate(MutationClass::Normal, MutationName("seed"), |l| {
            touch_normalized_store(l);
            Ok(())
        })
        .await
        .expect("seed commits");

        let order = order_log();
        let (gate, blocking) = barrier(&order);
        let blocker_db = Arc::clone(&h.db);
        let blocker = tokio::spawn(async move {
            blocker_db.mutate(MutationClass::Normal, MutationName("reconcile"), blocking).await
        });
        wait_until("the writer to be occupied", || gate.entered.load(Ordering::SeqCst)).await;

        // The writer thread is parked inside a transaction right now.
        let seen = h.db.read(LedgerSnapshot::commit_seq);
        assert_eq!(seen, 1, "reads serve committed state while the writer is busy");

        gate.release.send(()).expect("release");
        blocker.await.expect("join").expect("commit");
        assert_eq!(h.db.read(LedgerSnapshot::commit_seq), 2);
    }

    /// `writer-queue-blocked-by-reconcile-small-class-bypasses` (TESTING.md
    /// §4.1) at the actor level: goal publication does not queue behind a
    /// reconcile. This is the intent-spool replacement guarantee (§11-A2).
    #[tokio::test]
    async fn a_small_op_is_admitted_ahead_of_a_reconcile_queued_before_it() {
        let h = Harness::open_named();
        let order = order_log();
        let (gate, blocking) = barrier(&order);

        let blocker_db = Arc::clone(&h.db);
        let blocker = tokio::spawn(async move {
            blocker_db.mutate(MutationClass::Small, MutationName("barrier"), blocking).await
        });
        wait_until("the writer to be occupied", || gate.entered.load(Ordering::SeqCst)).await;

        // Reconcile is enqueued FIRST; the Small goal publication after it.
        let reconcile_db = Arc::clone(&h.db);
        let reconcile_body = recorder(&order, "reconcile".to_string());
        let reconcile = tokio::spawn(async move {
            reconcile_db
                .mutate(MutationClass::Reconcile, MutationName("supervision.cycle"), reconcile_body)
                .await
        });
        wait_until("the reconcile to be queued", || h.db.queue_depth() == 1).await;

        let goal_db = Arc::clone(&h.db);
        let goal_body = recorder(&order, "goal.set".to_string());
        let goal = tokio::spawn(async move {
            goal_db.mutate(MutationClass::Small, MutationName("goal.set"), goal_body).await
        });
        wait_until("the goal to be queued", || h.db.queue_depth() == 2).await;

        gate.release.send(()).expect("release");
        blocker.await.expect("join").expect("barrier commits");
        goal.await.expect("join").expect("goal commits");
        reconcile.await.expect("join").expect("reconcile commits");

        assert_eq!(
            snapshot_of(&order),
            vec!["barrier".to_string(), "goal.set".to_string(), "reconcile".to_string()],
            "a Small op must not wait behind a queued Reconcile"
        );
    }

    /// The other direction: a saturating Small stream cannot starve reconcile.
    /// Deterministic — the aging bound is a *count* (N=32), so no clock moves.
    #[tokio::test]
    async fn a_saturating_small_stream_cannot_starve_a_queued_reconcile() {
        let h = Harness::open_named();
        let order = order_log();
        let (gate, blocking) = barrier(&order);

        // The barrier is Normal, not Small: admitting a lower-priority op is
        // what resets the consecutive-Small counter, so the count below starts
        // from a known zero rather than from "one, because the barrier was a
        // Small op too".
        let blocker_db = Arc::clone(&h.db);
        let blocker = tokio::spawn(async move {
            blocker_db.mutate(MutationClass::Normal, MutationName("barrier"), blocking).await
        });
        wait_until("the writer to be occupied", || gate.entered.load(Ordering::SeqCst)).await;

        let reconcile_db = Arc::clone(&h.db);
        let reconcile_body = recorder(&order, "reconcile".to_string());
        let reconcile = tokio::spawn(async move {
            reconcile_db
                .mutate(MutationClass::Reconcile, MutationName("supervision.cycle"), reconcile_body)
                .await
        });
        wait_until("the reconcile to be queued", || h.db.queue_depth() == 1).await;

        let mut smalls = Vec::new();
        for i in 0..50u32 {
            let db = Arc::clone(&h.db);
            let body = recorder(&order, format!("small-{i}"));
            smalls.push(tokio::spawn(async move {
                db.mutate(MutationClass::Small, MutationName("goal.set"), body).await
            }));
            let expected = usize::try_from(i).unwrap_or(usize::MAX) + 2;
            wait_until("the small op to be queued", || h.db.queue_depth() == expected).await;
        }

        gate.release.send(()).expect("release");
        blocker.await.expect("join").expect("barrier commits");
        for small in smalls {
            small.await.expect("join").expect("small commits");
        }
        reconcile.await.expect("join").expect("reconcile commits");

        let observed = snapshot_of(&order);
        assert_eq!(observed[0], "barrier");
        assert_eq!(observed[32], "small-31", "N=32 Small ops are admitted first");
        assert_eq!(observed[33], "reconcile", "then the aged Reconcile, exactly at the bound");
        assert_eq!(observed[34], "small-32");
    }

    /// Aging by time rather than by count, on the manual clock.
    #[tokio::test]
    async fn a_reconcile_that_has_waited_the_aging_interval_goes_first() {
        let h = Harness::open_named();
        let order = order_log();
        let (gate, blocking) = barrier(&order);

        let blocker_db = Arc::clone(&h.db);
        let blocker = tokio::spawn(async move {
            blocker_db.mutate(MutationClass::Small, MutationName("barrier"), blocking).await
        });
        wait_until("the writer to be occupied", || gate.entered.load(Ordering::SeqCst)).await;

        let reconcile_db = Arc::clone(&h.db);
        let reconcile_body = recorder(&order, "reconcile".to_string());
        let reconcile = tokio::spawn(async move {
            reconcile_db
                .mutate(MutationClass::Reconcile, MutationName("supervision.cycle"), reconcile_body)
                .await
        });
        wait_until("the reconcile to be queued", || h.db.queue_depth() == 1).await;

        let goal_db = Arc::clone(&h.db);
        let goal_body = recorder(&order, "goal.set".to_string());
        let goal = tokio::spawn(async move {
            goal_db.mutate(MutationClass::Small, MutationName("goal.set"), goal_body).await
        });
        wait_until("the goal to be queued", || h.db.queue_depth() == 2).await;

        // T = 2 s of virtual time, with zero Small ops run.
        h.clock.advance(crate::actor::AGING_INTERVAL);

        gate.release.send(()).expect("release");
        blocker.await.expect("join").expect("barrier commits");
        goal.await.expect("join").expect("goal commits");
        reconcile.await.expect("join").expect("reconcile commits");

        assert_eq!(
            snapshot_of(&order),
            vec!["barrier".to_string(), "reconcile".to_string(), "goal.set".to_string()],
            "after T the aged Reconcile precedes even a Small op"
        );
    }

    /// The single most important property in the project, asserted end to end:
    /// `Busy` arrives only after the full documented wait, and the mutation it
    /// refers to never ran.
    #[tokio::test]
    async fn queue_deadline_busy_proves_the_wait_and_never_runs_the_closure() {
        let h = Harness::open_named();
        let order = order_log();
        let (gate, blocking) = barrier(&order);

        let blocker_db = Arc::clone(&h.db);
        let blocker = tokio::spawn(async move {
            blocker_db.mutate(MutationClass::Small, MutationName("barrier"), blocking).await
        });
        wait_until("the writer to be occupied", || gate.entered.load(Ordering::SeqCst)).await;

        let runs = Arc::new(AtomicU32::new(0));
        let runs_in_closure = Arc::clone(&runs);
        let victim_db = Arc::clone(&h.db);
        let victim = tokio::spawn(async move {
            victim_db
                .mutate(MutationClass::Small, MutationName("goal.set"), move |l| {
                    runs_in_closure.fetch_add(1, Ordering::SeqCst);
                    touch_normalized_store(l);
                    Ok(())
                })
                .await
        });
        wait_until("the victim to be queued", || h.db.queue_depth() == 1).await;

        // Virtual time only: no test sleeps to wait for a timeout.
        h.clock.advance(MUTATION_QUEUE_DEADLINE);
        gate.release.send(()).expect("release");

        blocker.await.expect("join").expect("barrier commits");
        let err = victim.await.expect("join").expect_err("the victim must be told Busy");
        match err {
            ChiefdError::Busy(proof) => {
                assert_eq!(proof.site(), "mutation-queue");
                assert!(
                    proof.waited() >= MUTATION_QUEUE_DEADLINE,
                    "Busy must carry proof of the full wait, got {:?}",
                    proof.waited()
                );
            }
            other => panic!("expected Busy, got {other:?}"),
        }
        assert_eq!(runs.load(Ordering::SeqCst), 0, "a Busy mutation never ran");
        assert_eq!(
            h.db.read(|s| crate::store::converge_safety::read(s.ledgers())
                .into_parts()
                .0
                .actuation_mode),
            crate::store::converge_safety::ActuationMode::Shadow
        );
    }

    /// Plan §2.1 step 2: quiescing resolves in-flight mutations `Unavailable`
    /// and never hangs. The timeout in `wait_until` makes a hang a failure, not
    /// a retry.
    #[tokio::test]
    async fn quiescing_resolves_queued_mutations_unavailable_and_never_hangs() {
        let h = Harness::open_named();
        let order = order_log();
        let (gate, blocking) = barrier(&order);

        let blocker_db = Arc::clone(&h.db);
        let blocker = tokio::spawn(async move {
            blocker_db.mutate(MutationClass::Normal, MutationName("barrier"), blocking).await
        });
        wait_until("the writer to be occupied", || gate.entered.load(Ordering::SeqCst)).await;

        let queued_db = Arc::clone(&h.db);
        let queued = tokio::spawn(async move {
            queued_db
                .mutate(MutationClass::Small, MutationName("goal.set"), |l| {
                    touch_normalized_store(l);
                    Ok(())
                })
                .await
        });
        wait_until("the mutation to be queued", || h.db.queue_depth() == 1).await;

        h.db.quiesce("removing");

        let err = queued.await.expect("join").expect_err("queued work is answered, not hung");
        assert!(matches!(err, ChiefdError::Unavailable { reason: "removing" }));
        assert_eq!(
            h.db.read(LedgerSnapshot::commit_seq),
            0,
            "the answer arrived while the in-flight job was still parked — quiescing \
             does not queue behind a multi-second reconcile"
        );

        // New work is refused at the door with the same reason — and this is a
        // shut door, not a lock a caller could retry into.
        let late =
            h.db.mutate(MutationClass::Small, MutationName("goal.set"), |l| {
                touch_normalized_store(l);
                Ok(())
            })
            .await
            .expect_err("a quiesced actor admits nothing");
        assert!(matches!(late, ChiefdError::Unavailable { reason: "removing" }));

        // The job that was already executing still completes.
        gate.release.send(()).expect("release");
        blocker.await.expect("join").expect("the in-flight job commits");
        assert_eq!(h.db.read(LedgerSnapshot::commit_seq), 1);
    }

    #[tokio::test]
    async fn normalized_state_survives_closing_and_reopening_the_actor() {
        let h = Harness::open_named();
        h.db.mutate(MutationClass::Normal, MutationName("seed"), |l| {
            Ok(crate::store::converge_safety::set_actuation_config(
                l,
                crate::store::converge_safety::ActuationMode::Apply,
                true,
                false,
            ))
        })
        .await
        .expect("commit");
        h.db.shutdown();

        let reopened = CompanyDb::open("e2eco", &h.path, h.clock.clone()).expect("reopen");
        assert_eq!(reopened.read(LedgerSnapshot::commit_seq), 0, "commit_seq is per-process");
        let state =
            reopened.read(|s| crate::store::converge_safety::read(s.ledgers()).into_parts().0);
        assert_eq!(state.actuation_mode, crate::store::converge_safety::ActuationMode::Apply);
        assert!(state.sweep_live);

        reopened
            .mutate(MutationClass::Normal, MutationName("bump"), |l| {
                touch_normalized_store(l);
                Ok(())
            })
            .await
            .expect("commit");
        assert_eq!(
            reopened.read(LedgerSnapshot::commit_seq),
            1,
            "the new actor emits its first commit identity"
        );
    }

    /// #585: a normal first health write is row-authoritative and survives an
    /// actor restart. This is deliberately a real actor restart, not a direct
    /// reconstruction test. Since the F16 un-cross-wiring the rows are the
    /// `health_monitor_*` set — the same store the TS launcher reads.
    #[tokio::test]
    async fn health_monitor_rows_survive_fresh_write_and_actor_restart() {
        use crate::store::health::{self, HealthMonitorState};

        let h = Harness::open_named();
        h.db.mutate(MutationClass::Normal, MutationName("seed-health"), |ledgers| {
            health::write(ledgers, &HealthMonitorState::empty("e2eco"));
            Ok(())
        })
        .await
        .expect("fresh health write commits");
        h.db.shutdown();

        let reopened = CompanyDb::open("e2eco", &h.path, h.clock.clone())
            .expect("fresh health rows reconstruct after restart");
        assert_eq!(
            reopened.read(|snapshot| {
                snapshot
                    .ledgers()
                    .document_body("health-monitor")
                    .expect("health-monitor body is reconstructed from normalized rows")
                    .to_string()
            }),
            serde_json::to_string(&HealthMonitorState::empty("e2eco"))
                .expect("serialize expected health"),
        );
    }

    /// A normalized health-monitor row remains readable even when it was
    /// inserted outside the named publish path; it needs no surrogate document
    /// version.
    #[test]
    fn health_monitor_rows_without_an_event_sequence_reconstruct_at_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        let mut conn = open_company_db(&path).expect("open schema");
        // Genesis first: the reconstructed document stamps the company's
        // DISPLAY name, which only genesis writes. The unfenced row this test
        // is about is inserted after it, still outside the named publish path.
        {
            let tx = conn.transaction().expect("genesis txn");
            let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
            manifest.slug = "e2eco".to_string();
            crate::store::organization_rows::genesis(&tx, "e2eco", &manifest).expect("genesis");
            tx.commit().expect("commit genesis");
        }
        conn.execute(
            "INSERT INTO health_monitor_meta(slug, last_run_at) VALUES('e2eco', NULL)",
            [],
        )
        .expect("seed deliberately unfenced row");
        drop(conn);

        let db = CompanyDb::open("e2eco", &path, Arc::new(ManualClock::default()))
            .expect("a normalized health row needs no document version");
        assert!(
            db.read(|snapshot| snapshot.ledgers().document_body("health-monitor").is_some()),
            "the health-monitor projection reconstructs from normalized rows",
        );
    }

    #[tokio::test]
    async fn org_manifest_reconstructs_from_rows_across_a_reopen() {
        use crate::store::organization;

        let h = Harness::open();
        let manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        let expected_name = manifest.name.clone();
        h.db.mutate(MutationClass::Normal, MutationName("seed-manifest"), move |l| {
            organization::create(l, &manifest)
        })
        .await
        .expect("commit");

        // BLOB-DEATH: the manifest write dispatched to organization_rows, never
        // to the `documents` blob table.
        assert!(
            documents_table_is_absent(&h.path),
            "org-manifest must never land in the documents table"
        );

        h.db.shutdown();

        // Reopen: `load_ledgers` must reconstruct the manifest from rows, not
        // find it absent because the (now-unused) documents blob has nothing.
        let reopened = CompanyDb::open("e2eco", &h.path, h.clock.clone()).expect("reopen");
        let read_back = reopened
            .read(|snapshot| organization::read(snapshot.ledgers()).map(|m| m.name))
            .expect("manifest reconstructed from rows at open");
        assert_eq!(read_back, expected_name);
    }

    #[tokio::test]
    async fn supervision_meta_and_effects_reconstruct_from_rows_across_a_reopen() {
        use crate::store::organization;
        use crate::store::supervision;

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        let manifest_for_org = manifest.clone();
        h.db.mutate(MutationClass::Normal, MutationName("seed-org"), move |l| {
            organization::create(l, &manifest_for_org)?;
            supervision::seed(l, &manifest_for_org)?;
            Ok(())
        })
        .await
        .expect("org + supervision seed commit");

        // An effect row -- the relational half `relational_diff` owns -- must
        // survive a reopen through `load_relational`.
        let manifest_for_effect = manifest.clone();
        h.db.mutate(MutationClass::Normal, MutationName("seed-effect"), move |l| {
            supervision::mutate(l, &manifest_for_effect, |draft, at| {
                draft.enqueue_effect_for_test("effect-1", "person_reminder", at)?;
                Ok(())
            })
        })
        .await
        .expect("effect commit");

        h.db.shutdown();
        let reopened = CompanyDb::open("e2eco", &h.path, h.clock.clone()).expect("reopen");
        let effect_present = reopened.read(|snapshot| {
            let manifest = organization::read(snapshot.ledgers()).expect("manifest");
            let ledger = supervision::read(snapshot.ledgers(), &manifest).expect("supervision");
            ledger.effect("effect-1").is_some()
        });
        assert!(effect_present, "effect must survive the reopen via load_relational");
    }

    #[tokio::test]
    async fn session_maintenance_publish_reconciles_at_the_current_transaction_cursor() {
        use crate::store::organization;

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        h.db.mutate(MutationClass::Normal, MutationName("seed-manifest"), move |l| {
            organization::create(l, &manifest)
        })
        .await
        .expect("manifest commit");
        let before = h.db.org_current_seq().await.expect("audit seq");
        assert!(before > 0, "the regression needs a non-zero prior cursor");
        let ledger = crate::store::session_maintenance::SessionMaintenanceLedger::initial(
            "e2eco",
            "2026-07-28T00:00:00.000Z",
        );

        let first =
            h.db.session_maintenance_publish(ledger.clone()).await.expect("first direct publish");
        assert!(first >= before, "the direct write observes the durable audit cursor");
        let second =
            h.db.session_maintenance_publish(ledger)
                .await
                .expect("direct republish must use its current transaction cursor");
        assert_eq!(second, first, "an unchanged ledger preserves its audit cursor");
    }

    #[tokio::test]
    async fn supervision_recovers_an_exact_dead_maintenance_claim_without_a_pi_startup() {
        use crate::store::organization;
        use crate::store::session_maintenance::{MaintenanceAction, MaintenanceStatus};
        use crate::store::session_maintenance_ops::{Claim, ExpectedIdentity, QueueInput};
        use crate::store::supervision;

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        let seeded = manifest.clone();
        h.db.mutate(MutationClass::Normal, MutationName("seed-maintenance-company"), move |l| {
            organization::create(l, &seeded)?;
            supervision::seed(l, &seeded)?;
            Ok(())
        })
        .await
        .expect("company seed");

        let queued =
            h.db.session_maintenance_queue(QueueInput {
                action: MaintenanceAction::Compact,
                person_id: "signal-researcher".to_string(),
                requested_by: "chief".to_string(),
                reason: "test dead claim".to_string(),
                automatic: false,
                force: None,
            })
            .await
            .expect("queue");
        let claim = Claim {
            process_id: 4242,
            session_id: "dead-session".to_string(),
            claim_token: "dead-token".to_string(),
        };
        h.db.session_maintenance_start(
            ExpectedIdentity { person_id: "signal-researcher".to_string() },
            MaintenanceAction::Compact,
            Some(queued.id.clone()),
            Some(claim),
            None,
        )
        .await
        .expect("start")
        .expect("claimed");

        let report =
            h.db.session_maintenance_recover_dead_claims(vec![(queued.id.clone(), 4242)])
                .await
                .expect("supervision recovery");
        assert_eq!(report.interrupted.len(), 1);
        assert_eq!(report.replacements.len(), 1);
        assert_eq!(report.interrupted[0].status, MaintenanceStatus::Failed);
        assert_eq!(report.replacements[0].status, MaintenanceStatus::Queued);
        assert_eq!(
            report.replacements[0].recovered_from_request_id.as_deref(),
            Some(queued.id.as_str())
        );

        let replay =
            h.db.session_maintenance_recover_dead_claims(vec![(queued.id, 4242)])
                .await
                .expect("stale observation is harmless");
        assert!(replay.interrupted.is_empty());
        assert!(replay.replacements.is_empty());
    }

    /// A running daemon may receive manifest genesis after its own boot. The
    /// first explicit launch intent must therefore find the scheduler's native
    /// ledgers already present in the actor snapshot; requiring a later TS
    /// publication or a process restart would leave that first wake inert.
    #[tokio::test]
    async fn manifest_genesis_seeds_live_supervision_and_activity() {
        use crate::store::{activity, organization, supervision};

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();

        let genesis_manifest = manifest.clone();
        assert!(matches!(
            h.db.org_manifest_genesis(
                manifest.clone(),
                "2026-01-01T00:00:00.000Z".to_owned(),
                crate::store::person_contracts::build::build_organization_person_contracts(
                    &genesis_manifest,
                )
                .expect("person contracts document"),
            )
            .await
            .expect("manifest genesis"),
            crate::store::organization_rows::ManifestGenesisOutcome::Created
        ));

        let (activity_organization, supervision_organization) = h.db.read(|snapshot| {
            let committed_manifest = organization::read(snapshot.ledgers()).expect("live manifest");
            let activity =
                activity::read(snapshot.ledgers(), &committed_manifest).expect("live activity");
            let supervision = supervision::read(snapshot.ledgers(), &committed_manifest)
                .expect("live supervision");
            (activity.organization, supervision.organization)
        });
        assert_eq!(activity_organization, "e2eco");
        assert_eq!(supervision_organization, "e2eco");
    }

    /// An explicit start is a new operator decision, so it starts a new idle
    /// lease even when the person's prior desired-active projection is still
    /// present. The old quiet clock must not park the person before the caller
    /// can send work, and the ordinary two-minute idle limit still applies to
    /// the new lease.
    #[tokio::test]
    async fn explicit_start_replaces_an_expired_idle_clock_with_a_fresh_two_minute_lease() {
        use crate::clock::Clock;
        use crate::store::activity::{
            self, LaunchFence, ReconcileInput, IDLE_AUTO_PARK_REASON,
            ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS,
        };
        use crate::store::{organization, supervision};

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        let seeded = manifest.clone();
        h.db.mutate(MutationClass::Normal, MutationName("test.seed-start-lease"), move |ledgers| {
            organization::create(ledgers, &seeded)?;
            supervision::seed(ledgers, &seeded)?;
            activity::seed(ledgers, &seeded)?;
            Ok(())
        })
        .await
        .expect("seed company");

        let person = "signal-researcher";
        let fence = LaunchFence::fenced([person.to_string()]);
        let first_manifest = manifest.clone();
        let first_fence = fence.clone();
        h.db.mutate(MutationClass::Reconcile, MutationName("test.start-lease-running"), move |l| {
            let supervision = supervision::read(l, &first_manifest)?;
            activity::reconcile(
                l,
                &first_manifest,
                &supervision,
                &ReconcileInput {
                    launch_intent: first_fence,
                    requested_person_ids: vec![person.to_string()],
                    watching_since: "1970-01-01T00:00:00.000Z".to_string(),
                },
            )?;
            Ok(())
        })
        .await
        .expect("project person running");
        h.db.org_activity_note_agent_state(person.to_string(), false)
            .await
            .expect("person reports quiet");

        let quiet_manifest = manifest.clone();
        let quiet_fence = fence.clone();
        h.db.mutate(
            MutationClass::Reconcile,
            MutationName("test.start-lease-old-clock"),
            move |l| {
                let supervision = supervision::read(l, &quiet_manifest)?;
                activity::reconcile(
                    l,
                    &quiet_manifest,
                    &supervision,
                    &ReconcileInput {
                        launch_intent: quiet_fence,
                        requested_person_ids: Vec::new(),
                        watching_since: "1970-01-01T00:00:00.000Z".to_string(),
                    },
                )?;
                Ok(())
            },
        )
        .await
        .expect("persist old quiet clock");
        h.clock.advance(std::time::Duration::from_millis(
            u64::try_from(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1).unwrap(),
        ));

        let routine_park_id = "transition:test:signal-researcher:park".to_string();
        let routine_park_for_write = routine_park_id.clone();
        let park_at = crate::isotime::iso_millis(h.clock.wall().0);
        h.db.in_transaction(
            MutationClass::Reconcile,
            MutationName("test.start-lease-real-idle-park"),
            move |tx| {
                activity::rows::insert_awaiting_handoff_transition(
                    tx,
                    "e2eco",
                    &routine_park_for_write,
                    person,
                    activity::TransitionAction::Park,
                    "research",
                    None,
                    IDLE_AUTO_PARK_REASON,
                    &park_at,
                    "9999-01-01T00:00:00.000Z",
                )
                .map_err(|error| store_failure("org-activity-rows", error))?;
                activity::rows::upsert_person_activity_desired(
                    tx,
                    "e2eco",
                    person,
                    true,
                    Some(&routine_park_for_write),
                    &park_at,
                )
                .map_err(|error| store_failure("org-activity-rows", error))?;
                Ok(())
            },
        )
        .await
        .expect("commit real routine idle park");
        let staged_activity =
            h.db.activity_read().await.expect("activity read").expect("activity exists").0;
        let routine_park =
            staged_activity.transitions.get(&routine_park_id).expect("routine idle park");
        assert_eq!(routine_park.action, activity::TransitionAction::Park);
        assert_eq!(routine_park.intent_id, None, "the scheduler park owns no lifecycle intent");
        assert_eq!(routine_park.reason, IDLE_AUTO_PARK_REASON);
        assert!(
            routine_park.status.is_pending(),
            "the regression must start from a real open scheduler park"
        );

        let started_at = crate::isotime::iso_millis(h.clock.wall().0);
        assert_eq!(
            h.db.start_person(person.to_string(), started_at.clone(), "chief".to_string())
                .await
                .expect("explicit start"),
            crate::store::org_ops::DirectOutcome::Applied,
        );
        let committed_activity =
            h.db.activity_read().await.expect("activity read").expect("activity exists").0;
        let person_state = &committed_activity.people[person];
        let released =
            committed_activity.transitions.get(&routine_park_id).expect("released park history");
        assert_eq!(released.status, activity::TransitionStatus::Cancelled);
        assert_eq!(
            released.reason,
            format!("superseded-by-start:{person}"),
            "the durable override fact names the explicit start, not a wake"
        );
        assert_eq!(
            person_state.active_transition_id, None,
            "the explicit start detaches the scheduler park"
        );
        assert!(person_state.last_desired_active, "the start leaves the person desired");
        assert_eq!(person_state.agent_quiet_at.as_deref(), Some(started_at.as_str()));
        assert_eq!(person_state.idle_since.as_deref(), Some(started_at.as_str()));
        assert_eq!(person_state.agent_active_at, None);
        let intent =
            h.db.launch_intent_read()
                .await
                .expect("launch intent read")
                .expect("launch intent exists")
                .0;
        assert!(intent.person_ids.iter().any(|id| id == person), "the launch fence commits too");
        h.clock.advance(std::time::Duration::from_secs(3));

        let early_manifest = manifest.clone();
        let early_fence = fence.clone();
        let early =
            h.db.mutate(
                MutationClass::Reconcile,
                MutationName("test.start-lease-three-seconds"),
                move |l| {
                    let supervision = supervision::read(l, &early_manifest)?;
                    activity::reconcile(
                        l,
                        &early_manifest,
                        &supervision,
                        &ReconcileInput {
                            launch_intent: early_fence,
                            requested_person_ids: Vec::new(),
                            watching_since: "1970-01-01T00:00:00.000Z".to_string(),
                        },
                    )
                },
            )
            .await
            .expect("three-second supervision");
        assert!(
            early.people[person].active,
            "an explicit start must not inherit the expired quiet clock"
        );
        let fresh_idle = h.db.read(|snapshot| {
            activity::read(snapshot.ledgers(), &manifest).expect("activity").people[person]
                .idle_since
                .clone()
        });
        assert_eq!(fresh_idle.as_deref(), Some(started_at.as_str()), "the lease begins at start");

        h.clock.advance(std::time::Duration::from_millis(
            u64::try_from(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS - 3_000 + 1).unwrap(),
        ));
        let expired_manifest = manifest.clone();
        let expired = h
            .db
            .mutate(MutationClass::Reconcile, MutationName("test.start-lease-expired"), move |l| {
                let supervision = supervision::read(l, &expired_manifest)?;
                activity::reconcile(
                    l,
                    &expired_manifest,
                    &supervision,
                    &ReconcileInput {
                        launch_intent: fence,
                        requested_person_ids: Vec::new(),
                        watching_since: "1970-01-01T00:00:00.000Z".to_string(),
                    },
                )
            })
            .await
            .expect("expired supervision");
        assert!(!expired.people[person].active, "the normal two-minute idle park remains");
        let parked = h.db.read(|snapshot| {
            activity::read(snapshot.ledgers(), &manifest)
                .expect("activity")
                .active_transition(person)
                .map(|transition| transition.reason.clone())
        });
        assert_eq!(parked.as_deref(), Some(IDLE_AUTO_PARK_REASON));
    }

    /// The whole-roster person operating-contracts document a real genesis
    /// caller would seed alongside the manifest.
    fn person_contracts_fixture(
        manifest: &crate::store::organization::OrganizationManifest,
    ) -> crate::store::person_contracts::rows::OrganizationPersonContracts {
        use crate::store::person_contracts::rows::{
            OrganizationPersonContracts, PersonContractEntry,
        };
        use std::collections::BTreeMap;
        let mut contracts = BTreeMap::new();
        for person_id in manifest.people.keys() {
            contracts.insert(
                person_id.clone(),
                PersonContractEntry {
                    text: format!("# {person_id}\n\nOperating contract."),
                    md5: format!("{:x}", md5_of(person_id)),
                    extra: BTreeMap::new(),
                },
            );
        }
        OrganizationPersonContracts {
            version: crate::store::person_contracts::rows::PERSON_CONTRACTS_VERSION,
            organization: manifest.slug.clone(),
            contracts,
            extra: BTreeMap::new(),
        }
    }

    /// A stand-in md5-shaped hex digest — the fixture only needs a value that
    /// round-trips, not a real hash.
    fn md5_of(seed: &str) -> u128 {
        let mut acc: u128 = 0x9E37_79B9_7F4A_7C15;
        for byte in seed.bytes() {
            acc = acc.wrapping_mul(0x100_0000_01B3).wrapping_add(u128::from(byte));
        }
        acc
    }

    /// A person-contracts document that fails `reject_unmodeled_keys`.
    fn person_contracts_fixture_with_unmodeled_key(
        manifest: &crate::store::organization::OrganizationManifest,
    ) -> crate::store::person_contracts::rows::OrganizationPersonContracts {
        let mut doc = person_contracts_fixture(manifest);
        doc.extra.insert("legacyField".to_owned(), serde_json::json!("not modeled"));
        doc
    }

    /// Row counts across every table the genesis transaction can touch, so a
    /// crash-safety assertion can compare "before" and "after" as one value
    /// instead of separate queries that could individually miss a leak.
    async fn genesis_table_counts(db: &CompanyDb) -> i64 {
        db.in_transaction(MutationClass::Small, MutationName("test.genesis-counts"), |tx| {
            let count = |sql: &str| -> Result<i64, ChiefdError> {
                tx.query_row(sql, [], |row| row.get::<_, i64>(0))
                    .map_err(|e| store_failure("genesis-test", e))
            };
            count("SELECT count(*) FROM person_contracts")
        })
        .await
        .expect("counts")
    }

    /// **The crash-safety test #815/#751 requires.** Genesis writes five
    /// documents (manifest, model catalog/default, person contracts,
    /// supervision, activity) inside a
    /// SINGLE `BEGIN IMMEDIATE … COMMIT`. Split, house style (#762), into one
    /// success proof and three separately-named crash tests, each asserting
    /// the SURVIVING STATE rather than merely that an error came back.
    ///
    /// Every crash test below was run against a deliberately non-atomic
    /// implementation (the contract `publish` call moved outside the
    /// `reconstruct(..).is_none()` guard, so they ran unconditionally on
    /// every call including a duplicate) and failed exactly as expected —
    /// `a_duplicate_genesis_writes_no_additional_row` caught it. See the story's
    /// verification report for the transcript.
    #[tokio::test]
    async fn genesis_writes_every_document_in_one_call() {
        use crate::store::{activity, organization, supervision};

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_owned();
        let at = "2026-08-04T00:00:00.000Z".to_owned();

        let contracts = person_contracts_fixture(&manifest);
        h.db.org_manifest_genesis(manifest.clone(), at.clone(), contracts.clone())
            .await
            .expect("genesis with all five documents");

        assert!(h.db.org_manifest_read().await.expect("read manifest").is_some());
        let read_contracts =
            h.db.org_person_contracts_read()
                .await
                .expect("read contracts")
                .expect("contracts present");
        assert_eq!(read_contracts, contracts, "person-contracts document round-trips");
        h.db.read(|snapshot| {
            let committed_manifest = organization::read(snapshot.ledgers()).expect("live manifest");
            activity::read(snapshot.ledgers(), &committed_manifest).expect("live activity");
            supervision::read(snapshot.ledgers(), &committed_manifest).expect("live supervision");
        });
    }

    /// Both genesis-rollback tests' end state: the company was never created,
    /// so it has no `org_settings` row, no display name, and therefore no
    /// document any store can reconstruct for it.
    async fn assert_no_such_company(h: &Harness) {
        let read = h.db.org_person_contracts_read().await.err();
        assert_eq!(
            read.expect("a company that does not exist has no documents").code(),
            Some(crate::store::org_settings::UNKNOWN_COMPANY),
        );
    }

    /// An unmodeled key on the contracts document refuses the whole genesis.
    #[tokio::test]
    async fn a_crash_before_the_person_contracts_publish_leaves_no_row() {
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_owned();
        let at = "2026-08-04T00:00:00.000Z".to_owned();

        let h = Harness::open();
        let before = genesis_table_counts(&h.db).await;
        let bad_doc = person_contracts_fixture_with_unmodeled_key(&manifest);
        let error =
            h.db.org_manifest_genesis(manifest.clone(), at.clone(), bad_doc)
                .await
                .err()
                .expect("unmodeled person-contracts key is refused");
        assert_eq!(error.code(), Some(crate::store::person_contracts::rows::UNMODELED_KEYS),);

        let after = genesis_table_counts(&h.db).await;
        assert_eq!(
            before, after,
            "a refused person-contracts publish leaves every table untouched"
        );
        assert!(h.db.org_manifest_read().await.expect("read").is_none());
        assert_no_such_company(&h).await;

        h.db.org_manifest_genesis(
            manifest.clone(),
            at.clone(),
            person_contracts_fixture(&manifest),
        )
        .await
        .expect("retry with a valid document succeeds");
    }

    /// Duplicate genesis writes NOTHING: the second call's person-contracts
    /// payload never lands, matching the pre-existing
    /// `AlreadyExists` guard's behavior for the manifest and model-catalog
    /// rows (the "ordering note" in the story's Contract — the guard lives in
    /// the apply closure while the two new publishes live in the `step`, so
    /// this is the assertion that proves the `step`'s own `is_none()` guard
    /// covers them too).
    #[tokio::test]
    async fn a_duplicate_genesis_writes_no_additional_row() {
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_owned();
        let at = "2026-08-04T00:00:00.000Z".to_owned();
        let contracts = person_contracts_fixture(&manifest);

        let h = Harness::open();
        h.db.org_manifest_genesis(manifest.clone(), at.clone(), contracts.clone())
            .await
            .expect("first genesis");
        let before = genesis_table_counts(&h.db).await;
        let before_contracts = h.db.org_person_contracts_read().await.expect("read");
        let outcome =
            h.db.org_manifest_genesis(
                manifest.clone(),
                "2099-01-01T00:00:00.000Z".to_owned(),
                contracts.clone(),
            )
            .await
            .expect("duplicate genesis does not error");
        assert!(matches!(
            outcome,
            crate::store::organization_rows::ManifestGenesisOutcome::AlreadyExists
        ));

        let after = genesis_table_counts(&h.db).await;
        assert_eq!(before, after, "a duplicate genesis writes no additional rows");
        let after_contracts = h.db.org_person_contracts_read().await.expect("read");
        assert_eq!(
            after_contracts, before_contracts,
            "duplicate genesis's person-contracts payload never landed — byte-identical to the first call's"
        );
    }

    /// A department head seed that is complete on its own.
    ///
    /// [`new_department_head_seed`] deliberately clears `task_class` because
    /// the model-batch path fills it from the fresh selection; the plain
    /// create-department path has no batch to fill it, so a cleared one is an
    /// `invalid-seed` refusal before the first write.
    fn complete_department_head_seed() -> crate::store::org_ops::OwnedNewPersonSeed {
        let mut seed = hired_worker_seed();
        seed.kind = crate::store::organization::PersonKind::Head;
        seed.title = "Platform Head".to_owned();
        seed
    }

    /// Read one company-scoped `count(*)`/scalar through the actor's own
    /// connection, so an atomicity assertion reads the same durable rows the
    /// mutation wrote (or did not write).
    async fn scalar(db: &Arc<CompanyDb>, op: MutationName, sql: &'static str) -> i64 {
        db.in_transaction(MutationClass::Small, op, move |tx| {
            tx.query_row(sql, [], |row| row.get::<_, i64>(0))
                .map_err(|e| store_failure("atomicity-test", e))
        })
        .await
        .expect("scalar read")
    }

    /// A department is created whole or not at all — proven by BREAKING the
    /// creation partway through, not by watching it succeed.
    ///
    /// Every other department-create test at this layer proves a *refusal*
    /// writes nothing, and a refusal is decided before the first write, so none
    /// of them can see a failure that lands *between* two writes. This one
    /// installs a trigger that aborts the second initial worker's `people`
    /// insert: by the time it fires, the department row, the hired head, that
    /// head's activity row and staffing history, and the first worker are all
    /// already written inside the transaction. If any part of the creation
    /// escaped the single `BEGIN IMMEDIATE` — or if a mid-write SQL failure
    /// were reported as a value the caller commits — a half-created department
    /// would survive here, which is a durable corruption of a company's
    /// hierarchy.
    ///
    /// It runs against a real `CompanyDb`, so `PRAGMA foreign_keys=ON` holds
    /// exactly as it does in production; the store-level `org_ops` tests turn
    /// foreign keys OFF and therefore cannot see an ordering violation at all.
    #[tokio::test]
    async fn a_department_create_that_fails_partway_through_leaves_the_store_exactly_as_it_was() {
        use crate::store::org_ops::{DepartmentStaffSeed, HeadDecision};

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_owned();
        let genesis_manifest = manifest.clone();
        h.db.org_manifest_genesis(
            manifest,
            "2026-01-01T00:00:00.000Z".to_owned(),
            crate::store::person_contracts::build::build_organization_person_contracts(
                &genesis_manifest,
            )
            .expect("person contracts document"),
        )
        .await
        .expect("seed company");

        let (before, before_seq) = h.db.org_manifest_read().await.expect("read").expect("company");
        let before_history = scalar(
            &h.db,
            MutationName("test.partial-create-history-before"),
            "SELECT count(*) FROM staffing_history WHERE slug = 'e2eco'",
        )
        .await;
        let before_activity = scalar(
            &h.db,
            MutationName("test.partial-create-activity-before"),
            "SELECT count(*) FROM person_activity WHERE slug = 'e2eco'",
        )
        .await;
        // The per-slug `org_events` counter (D2). A creation that allocated
        // sequence numbers outside the transaction would leave this advanced
        // even with every row rolled back.
        let before_counter = scalar(
            &h.db,
            MutationName("test.partial-create-counter-before"),
            "SELECT COALESCE((SELECT value FROM counters WHERE name = 'org-events:e2eco'), 0)",
        )
        .await;
        let before_contracts = scalar(
            &h.db,
            MutationName("test.partial-create-contracts-before"),
            "SELECT count(*) FROM person_contracts WHERE slug = 'e2eco'",
        )
        .await;

        h.db.in_transaction(
            MutationClass::Small,
            MutationName("test.partial-create-injection"),
            |tx| {
                tx.execute_batch(
                    "CREATE TRIGGER inject_mid_create_failure BEFORE INSERT ON people \
                     WHEN NEW.id = 'platform-second' \
                     BEGIN SELECT RAISE(ABORT, 'injected mid-create failure'); END;",
                )
                .map_err(|e| store_failure("atomicity-test", e))
            },
        )
        .await
        .expect("install the injected failure");

        let error =
            h.db.create_department(
                "quant-platform".to_owned(),
                "quant".to_owned(),
                "Platform".to_owned(),
                "Own the platform.".to_owned(),
                HeadDecision::HireNew {
                    person_id: "platform-head".to_owned(),
                    seed: Box::new(complete_department_head_seed()),
                },
                vec![
                    DepartmentStaffSeed {
                        person_id: "platform-first".to_owned(),
                        seed: hired_worker_seed(),
                    },
                    DepartmentStaffSeed {
                        person_id: "platform-second".to_owned(),
                        seed: hired_worker_seed(),
                    },
                ],
                Some("quant-head".to_owned()),
                "Create Platform".to_owned(),
                "2026-08-02T06:00:01.000Z".to_owned(),
                "quant-head".to_owned(),
            )
            .await
            .expect_err("a SQL failure mid-creation is a fault, never a committed value");
        // A fault, not a refusal: the request was fine and the store broke, so
        // the caller must not be told a product rule declined it.
        assert!(
            matches!(error, ChiefdError::StoreFailure { .. }),
            "a mid-write failure must surface as a fault, got {error:?}"
        );

        let (after, after_seq) = h.db.org_manifest_read().await.expect("read").expect("company");
        // No row: neither the department nor ANY of the four people it would
        // have created — including the ones written before the injected step.
        assert!(!after.departments.contains_key("quant-platform"));
        for person_id in ["platform-head", "platform-first", "platform-second"] {
            assert!(!after.people.contains_key(person_id), "{person_id} survived a failed create");
        }
        // No placement: the department order and the people order are byte-for-
        // byte what they were, so nothing was reordered around a unit that does
        // not exist.
        assert_eq!(after.department_order, before.department_order);
        assert_eq!(after.people_order, before.people_order);
        // No derived fact: no staffing history, no activity row, no audit
        // event, and no advanced sequence counter.
        assert_eq!(after_seq, before_seq, "a failed create advanced the audit fence");
        assert_eq!(
            scalar(
                &h.db,
                MutationName("test.partial-create-history-after"),
                "SELECT count(*) FROM staffing_history WHERE slug = 'e2eco'",
            )
            .await,
            before_history
        );
        assert_eq!(
            scalar(
                &h.db,
                MutationName("test.partial-create-activity-after"),
                "SELECT count(*) FROM person_activity WHERE slug = 'e2eco'",
            )
            .await,
            before_activity
        );
        assert_eq!(
            scalar(
                &h.db,
                MutationName("test.partial-create-counter-after"),
                "SELECT COALESCE((SELECT value FROM counters WHERE name = 'org-events:e2eco'), 0)",
            )
            .await,
            before_counter,
            "a failed create left the org_events counter advanced"
        );
        assert_eq!(
            scalar(
                &h.db,
                MutationName("test.partial-create-contracts-after"),
                "SELECT count(*) FROM person_contracts WHERE slug = 'e2eco'",
            )
            .await,
            before_contracts,
            "a failed create left an operating contract behind"
        );

        // The company still works: with the injection removed, the same create
        // applies whole. A store that merely *looks* untouched but can no
        // longer accept the write is not "exactly as it was".
        h.db.in_transaction(
            MutationClass::Small,
            MutationName("test.partial-create-injection-removed"),
            |tx| {
                tx.execute_batch("DROP TRIGGER inject_mid_create_failure;")
                    .map_err(|e| store_failure("atomicity-test", e))
            },
        )
        .await
        .expect("remove the injected failure");
        let outcome =
            h.db.create_department(
                "quant-platform".to_owned(),
                "quant".to_owned(),
                "Platform".to_owned(),
                "Own the platform.".to_owned(),
                HeadDecision::HireNew {
                    person_id: "platform-head".to_owned(),
                    seed: Box::new(complete_department_head_seed()),
                },
                vec![
                    DepartmentStaffSeed {
                        person_id: "platform-first".to_owned(),
                        seed: hired_worker_seed(),
                    },
                    DepartmentStaffSeed {
                        person_id: "platform-second".to_owned(),
                        seed: hired_worker_seed(),
                    },
                ],
                Some("quant-head".to_owned()),
                "Create Platform".to_owned(),
                "2026-08-02T06:00:02.000Z".to_owned(),
                "quant-head".to_owned(),
            )
            .await
            .expect("the retry after the failure applies");
        assert_eq!(
            outcome,
            crate::store::org_ops::CreateDepartmentOutcome::Applied {
                department_id: "quant-platform".to_owned()
            }
        );
        // ...and it applies WHOLE. The three new people have their operating
        // contracts in the very transaction that created them, so no committed
        // department can hold a person whose home has no contract text.
        // Company genesis has always committed this way; a
        // department is the same act one level down the tree.
        for person_id in ["platform-head", "platform-first", "platform-second"] {
            let rows =
                h.db.in_transaction(
                    MutationClass::Small,
                    MutationName("test.created-person-contract"),
                    move |tx| {
                        tx.query_row(
                            "SELECT count(*) FROM person_contracts \
                             WHERE slug = 'e2eco' AND person_id = ?1 AND length(text) > 0",
                            [person_id],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(|e| store_failure("atomicity-test", e))
                    },
                )
                .await
                .expect("contract count");
            assert_eq!(rows, 1, "{person_id} committed without an operating contract");
        }
        // And the post-commit rebuild the route still runs is now a NO-OP: the
        // contracts are already current, so it writes nothing and re-stamps no
        // `AGENTS.md`. One fact, one derivation, written once.
        let (published, _) =
            h.db.org_person_contracts_build("2026-08-02T06:00:03.000Z".to_owned())
                .await
                .expect("post-commit rebuild");
        assert!(!published, "the create left the post-commit rebuild something to write");
    }

    /// THE OPERATOR'S CASE, against a REAL store. A head who is the ONLY member
    /// of the department they head is transferred into another department, and
    /// their now-empty single-person department dissolves — in ONE transaction.
    ///
    /// It runs against a real `CompanyDb`, so `PRAGMA foreign_keys=ON` holds
    /// exactly as it does in production. The store-level `org_ops` tests turn
    /// foreign keys OFF (org_ops.rs `open`/`open_vacancy`), so they cannot see
    /// this ordering violation: before this fix, `transfer_person` DELETEd the
    /// dissolved department while the mover's `people.department_id` still
    /// referenced it, and SQLite raised extended code 787 (FOREIGN KEY
    /// constraint failed) — the production `store failure: org-manifest-rows`
    /// that failed every transfer-with-dissolve.
    #[tokio::test]
    async fn a_transfer_that_dissolves_the_movers_single_person_department_succeeds() {
        use crate::store::org_ops::{HeadVacancy, TransferOutcome};
        use crate::store::organization::PersonKind;

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_owned();
        let genesis_manifest = manifest.clone();
        h.db.org_manifest_genesis(
            manifest,
            "2026-01-01T00:00:00.000Z".to_owned(),
            crate::store::person_contracts::build::build_organization_person_contracts(
                &genesis_manifest,
            )
            .expect("person contracts document"),
        )
        .await
        .expect("seed company");

        // `it-head` is the head AND only member of `it`. Move them into `quant`
        // and dissolve `it` in the same change — the exact flatten-into-worker
        // move the operator ran on the real box.
        let outcome =
            h.db.transfer_person(
                "it-head".to_owned(),
                "quant".to_owned(),
                "flatten it-head into quant".to_owned(),
                "2026-08-02T06:00:01.000Z".to_owned(),
                "chief".to_owned(),
                Some(HeadVacancy::Dissolve),
            )
            .await
            .expect("a transfer-with-dissolve is a fault only if the store cannot apply it");
        assert_eq!(
            outcome,
            TransferOutcome::Applied { moved: vec!["it-head".to_owned()] },
            "the sole member moved and the emptied department dissolved"
        );

        let (after, _) = h.db.org_manifest_read().await.expect("read").expect("company");
        let moved = after.people.get("it-head").expect("it-head survives the move");
        assert_eq!(moved.department_id, "quant", "the mover lands in the destination");
        assert_eq!(
            moved.kind,
            PersonKind::Worker,
            "a transferred head heads nothing and becomes a worker"
        );
        assert!(
            !after.departments.contains_key("it"),
            "the emptied single-person department is gone, not headless"
        );
    }

    /// The SAME fault on the create path. Appointing an EXISTING person — who is
    /// the sole member and head of their own department — as the head of a NEW
    /// department, with `vacates: dissolve` on the department they leave, must
    /// dissolve that emptied department in the same transaction.
    ///
    /// Against a real `CompanyDb` (`PRAGMA foreign_keys=ON`), this shares the
    /// transfer path's shape: the create path DELETEd the dissolved department
    /// while the appointee still homed there, so SQLite raised extended code 787
    /// (FOREIGN KEY constraint failed). The store-level `org_ops` create tests
    /// run with foreign keys OFF, so this was invisible there too.
    #[tokio::test]
    async fn a_created_department_that_dissolves_its_new_heads_old_one_person_department_succeeds()
    {
        use crate::store::org_ops::{
            CreateDepartmentOutcome, DepartmentCreateUnit, HeadDecision, HeadVacancy,
        };
        use crate::store::organization::PersonKind;

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_owned();
        let genesis_manifest = manifest.clone();
        h.db.org_manifest_genesis(
            manifest,
            "2026-01-01T00:00:00.000Z".to_owned(),
            crate::store::person_contracts::build::build_organization_person_contracts(
                &genesis_manifest,
            )
            .expect("person contracts document"),
        )
        .await
        .expect("seed company");

        // `it-head` heads `it` and is its ONLY member. Appoint them to head a
        // NEW `platform` department under `quant`, and dissolve `it` in the same
        // change — the create-path twin of the operator's transfer-with-dissolve.
        let outcome =
            h.db.create_department_unit(
                "platform".to_owned(),
                "quant".to_owned(),
                "Platform".to_owned(),
                "Own the platform.".to_owned(),
                HeadDecision::AppointExisting { person_id: "it-head".to_owned() },
                vec![],
                DepartmentCreateUnit::Department,
                Some(HeadVacancy::Dissolve),
                Some("chief".to_owned()),
                "it-head takes platform".to_owned(),
                "2026-08-02T06:00:01.000Z".to_owned(),
                "chief".to_owned(),
                PublishBarrier::none(),
            )
            .await
            .expect("a create-with-dissolve is a fault only if the store cannot apply it");
        assert_eq!(
            outcome,
            CreateDepartmentOutcome::Applied { department_id: "platform".to_owned() },
            "the new department is created and the emptied one dissolved"
        );

        let (after, _) = h.db.org_manifest_read().await.expect("read").expect("company");
        let head = after.people.get("it-head").expect("it-head survives the appointment");
        assert_eq!(
            head.department_id, "platform",
            "the appointee lives in the department they head"
        );
        assert_eq!(head.kind, PersonKind::Head, "the appointee is a real head now");
        assert_eq!(
            after.departments.get("platform").map(|d| d.head_person_id.as_str()),
            Some("it-head"),
            "platform is headed by the appointee"
        );
        assert!(
            !after.departments.contains_key("it"),
            "the appointee's emptied single-person department is gone"
        );
    }

    /// THE OPERATOR'S WHOLE GESTURE, against a REAL store: a parent department
    /// with three sub-departments is FLATTENED into itself, in the order a
    /// manager actually runs it — move each sub-department's ordinary members up
    /// first, then transfer each now-sole head into the parent with
    /// `vacates: dissolve`.
    ///
    /// The two tests above pin ONE transfer and ONE create. This pins the
    /// SEQUENCE the operator ran on a live box and again on a fresh
    /// company (`fk-labs`, 2026-08-19, TEST_SUITE Case 40): five people, three
    /// departments, two of which were NOT one-person departments until the
    /// member move emptied them. Before the fix every one of the three head
    /// transfers failed with SQLite extended code 787 (FOREIGN KEY constraint
    /// failed) and the heads were left stranded over their own empty units —
    /// the shape a company accretes when nobody can be converted back into a
    /// worker.
    ///
    /// It runs against a real `CompanyDb`, so `PRAGMA foreign_keys=ON` holds
    /// exactly as it does in production; the store-level `org_ops` tests turn
    /// foreign keys OFF and cannot see this ordering violation at all.
    #[tokio::test]
    async fn flattening_a_subtree_of_departments_into_their_parent_succeeds() {
        use crate::store::org_ops::{
            CreateDepartmentOutcome, DepartmentCreateUnit, DepartmentStaffSeed, HeadDecision,
            HeadVacancy, TransferOutcome,
        };
        use crate::store::organization::PersonKind;

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_owned();
        let genesis_manifest = manifest.clone();
        h.db.org_manifest_genesis(
            manifest,
            "2026-01-01T00:00:00.000Z".to_owned(),
            crate::store::person_contracts::build::build_organization_person_contracts(
                &genesis_manifest,
            )
            .expect("person contracts document"),
        )
        .await
        .expect("seed company");

        // The shape the operator built: `quant` over three sub-departments —
        // one with a head and NOBODY else, two with a head and one worker.
        for (department_id, head_id, worker_id) in [
            ("commodities", "commodities-head", None),
            ("securities", "securities-head", Some("securities-worker")),
            ("crypto", "crypto-head", Some("crypto-worker")),
        ] {
            let staff = worker_id
                .map(|person_id| {
                    vec![DepartmentStaffSeed {
                        person_id: person_id.to_owned(),
                        seed: hired_worker_seed(),
                    }]
                })
                .unwrap_or_default();
            let outcome =
                h.db.create_department_unit(
                    department_id.to_owned(),
                    "quant".to_owned(),
                    department_id.to_owned(),
                    "Own a strategy.".to_owned(),
                    HeadDecision::HireNew {
                        person_id: head_id.to_owned(),
                        seed: Box::new(complete_department_head_seed()),
                    },
                    staff,
                    DepartmentCreateUnit::Department,
                    None,
                    Some("chief".to_owned()),
                    format!("create {department_id}"),
                    "2026-08-02T06:00:01.000Z".to_owned(),
                    "chief".to_owned(),
                    PublishBarrier::none(),
                )
                .await
                .expect("building the subtree is a fault only if the store cannot apply it");
            assert_eq!(
                outcome,
                CreateDepartmentOutcome::Applied { department_id: department_id.to_owned() },
                "the sub-department is created under quant"
            );
        }

        // Step one: the ordinary members move up. A head is never left headless
        // by this call, so it EMPTIES the two staffed units down to their head.
        for (department_id, worker_id) in
            [("securities", "securities-worker"), ("crypto", "crypto-worker")]
        {
            let outcome =
                h.db.move_department_members(
                    department_id.to_owned(),
                    "quant".to_owned(),
                    vec![worker_id.to_owned()],
                    "flatten the subtree".to_owned(),
                    "2026-08-02T06:00:02.000Z".to_owned(),
                    "chief".to_owned(),
                )
                .await
                .expect("moving a worker up is a fault only if the store cannot apply it");
            assert_eq!(
                outcome,
                TransferOutcome::Applied { moved: vec![worker_id.to_owned()] },
                "the worker lands in the parent department"
            );
        }

        // Step two: every head is now the LAST member of their unit, so each
        // transfer carries `Dissolve` — the call that raised 787 for all three.
        for head_id in ["commodities-head", "securities-head", "crypto-head"] {
            let outcome =
                h.db.transfer_person(
                    head_id.to_owned(),
                    "quant".to_owned(),
                    "flatten the subtree".to_owned(),
                    "2026-08-02T06:00:03.000Z".to_owned(),
                    "chief".to_owned(),
                    Some(HeadVacancy::Dissolve),
                )
                .await
                .expect("a transfer-with-dissolve is a fault only if the store cannot apply it");
            assert_eq!(
                outcome,
                TransferOutcome::Applied { moved: vec![head_id.to_owned()] },
                "the head moves into the parent and its emptied unit dissolves"
            );
        }

        let (after, _) = h.db.org_manifest_read().await.expect("read").expect("company");
        for department_id in ["commodities", "securities", "crypto"] {
            assert!(
                !after.departments.contains_key(department_id),
                "{department_id} is gone from the roster, not headless and not empty"
            );
        }
        for person_id in [
            "commodities-head",
            "securities-head",
            "crypto-head",
            "securities-worker",
            "crypto-worker",
        ] {
            let person = after.people.get(person_id).expect("everybody survives the flatten");
            assert_eq!(person.department_id, "quant", "{person_id} homes in the parent department");
            assert_eq!(
                person.kind,
                PersonKind::Worker,
                "{person_id} heads nothing now, so they are a worker"
            );
        }
        assert_eq!(
            after.departments.get("quant").map(|d| d.head_person_id.as_str()),
            Some("quant-head"),
            "the parent department keeps its own head"
        );
    }

    #[tokio::test]
    async fn activity_publish_reconciles_at_the_current_transaction_cursor() {
        use crate::store::activity;
        use crate::store::organization;

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        let manifest_for_mutation = manifest.clone();
        h.db.mutate(MutationClass::Normal, MutationName("seed-manifest"), move |l| {
            organization::create(l, &manifest_for_mutation)
        })
        .await
        .expect("manifest commit");
        h.db.mutate(MutationClass::Normal, MutationName("seed-activity"), move |l| {
            activity::seed(l, &manifest).map(|_| ())
        })
        .await
        .expect("activity commit");

        // Seed mutations already emitted durable events. The direct activity
        // contract must nevertheless publish against the cursor it reads
        // inside its own `BEGIN IMMEDIATE`, with no caller sequence to supply.
        let before = h.db.org_current_seq().await.expect("audit seq");
        assert!(before > 0, "the regression needs a non-zero prior cursor");
        let (ledger, observed) =
            h.db.activity_read().await.expect("activity read").expect("seeded activity");
        assert_eq!(observed, before);
        let returned =
            h.db.activity_publish(serde_json::to_string(&ledger).expect("serialize ledger"))
                .await
                .expect("direct atomic publish must not reject the prior audit cursor");
        assert_eq!(returned, before, "an unchanged diff preserves the audit cursor");
    }

    #[tokio::test]
    async fn activity_structural_reconcile_is_a_noop_at_the_current_transaction_cursor() {
        use crate::store::activity;
        use crate::store::organization;

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        h.db.mutate(MutationClass::Normal, MutationName("seed-activity-authority"), move |l| {
            organization::create(l, &manifest)?;
            activity::seed(l, &manifest).map(|_| ())
        })
        .await
        .expect("seeded authority commits");

        let before = h.db.org_current_seq().await.expect("audit cursor");
        let (applied, seq) =
            h.db.activity_reconcile_structural()
                .await
                .expect("valid activity has no structural work");
        assert!(!applied, "an unchanged aggregate must not emit a write");
        assert_eq!(seq, before, "a no-op preserves the immutable cursor");
    }

    /// arch-audit F17 — the RED control for the health-publish merge
    /// semantics, measured red at f29503d5 (delete-on-absence in
    /// `health_monitor_rows::publish`) and un-skipped as the acceptance test
    /// for the fix (arch-impl/step3).
    ///
    /// The defect: `publish` diffed every map against the caller's payload
    /// and `diff_map` unconditionally DELETEd any fingerprint present in
    /// committed rows but absent from the payload — "unmentioned" and
    /// "resolved" were indistinguishable. The fix: incidents and terminal
    /// resolutions MERGE — incoming entries are upserted; an incident row is
    /// deleted only on positive evidence (its fingerprint present in
    /// committed or incoming `terminal_resolutions`); absence never deletes.
    ///
    /// This test pins the regression directly (deterministic, no
    /// interleaving required — a single stale caller republishing its own
    /// earlier snapshot after a second commit is a positive demonstration of
    /// the defect class). It does not depend on F16 (the
    /// `health::write`/dispatch-gap question) because it drives BOTH
    /// commits through `health_monitor_publish` — the confirmed-durable
    /// TS-facing path.
    #[tokio::test]
    async fn stale_health_monitor_publish_deletes_an_incident_it_never_observed() {
        use crate::store::health_monitor_rows::{HealthMonitorIncident, HealthMonitorState};

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        let genesis_manifest = manifest.clone();
        assert!(matches!(
            h.db.org_manifest_genesis(
                manifest.clone(),
                "2026-01-01T00:00:00.000Z".to_owned(),
                crate::store::person_contracts::build::build_organization_person_contracts(
                    &genesis_manifest,
                )
                .expect("person contracts document"),
            )
            .await
            .expect("manifest genesis"),
            crate::store::organization_rows::ManifestGenesisOutcome::Created
        ));

        fn incident(fingerprint: &str, kind: &str) -> HealthMonitorIncident {
            HealthMonitorIncident {
                fingerprint: fingerprint.to_owned(),
                kind: kind.to_owned(),
                detail: "test incident".to_owned(),
                first_seen_at: "2026-07-31T00:00:00.000Z".to_owned(),
                last_seen_at: "2026-07-31T00:00:00.000Z".to_owned(),
                count: 1,
                responsible_person_id: None,
                unblock_action: None,
                observed_count: None,
                oldest_at: None,
                acknowledged_at: None,
                alert_recipient_person_id: None,
                impaired_mailbox_person_id: None,
                extra: Default::default(),
            }
        }
        fn doc(incidents: Vec<HealthMonitorIncident>) -> HealthMonitorState {
            HealthMonitorState {
                version: 1,
                organization: "e2eco".to_owned(),
                last_run_at: Some("2026-07-31T00:00:00.000Z".to_owned()),
                cursors: Default::default(),
                observations: Default::default(),
                incidents: incidents.into_iter().map(|i| (i.fingerprint.clone(), i)).collect(),
                terminal_resolutions: Default::default(),
                cleared_fingerprints: Vec::new(),
                extra: Default::default(),
            }
        }

        // STEP 1: an incident A is committed and observed by a caller (its
        // "stale snapshot" going forward).
        h.db.health_monitor_publish(doc(vec![incident("A", "runtime_ownership_conflict")]))
            .await
            .expect("publish A");
        let stale = doc(vec![incident("A", "runtime_ownership_conflict")]);

        // STEP 2: a SECOND, independent commit adds incident B -- e.g. a
        // different pass, a different actuator, chiefd's own duty if F16 is
        // ever fixed. The stale caller from step 1 never saw this.
        h.db.health_monitor_publish(doc(vec![
            incident("A", "runtime_ownership_conflict"),
            incident("B", "supervisor_error"),
        ]))
        .await
        .expect("publish A+B");
        let (after_b, _seq) = h.db.health_monitor_read().await.expect("read").expect("doc present");
        assert!(
            after_b.incidents.contains_key("B"),
            "control: incident B is committed and visible before the stale republish"
        );

        // STEP 3 / THE MEASUREMENT: the stale caller republishes its OWN
        // unchanged snapshot from step 1 -- it did nothing wrong, it simply
        // acted on the last state it observed, exactly like TS's
        // `publishHealthMonitor` round-tripping a ledger it read earlier.
        h.db.health_monitor_publish(stale).await.expect("stale republish");

        let (final_doc, _seq2) =
            h.db.health_monitor_read().await.expect("read").expect("doc present");
        assert!(
            final_doc.incidents.contains_key("B"),
            "REGRESSION IF THIS FAILS: a stale health-monitor republish DELETED incident \
             'B', which was committed by an independent pass AFTER the stale caller's \
             snapshot and which the stale caller never observed. This is `diff_map`'s \
             delete-on-absence (health_monitor_rows.rs:422-448) applied to `incidents` \
             (:377-395): 'unmentioned' and 'resolved' are indistinguishable to this \
             write path. Any two callers publishing from different snapshot ages -- \
             TS and Rust today if F16's dispatch gap is ever closed, or even two TS \
             passes racing each other -- can silently erase each other's incidents \
             with no conflict signal."
        );
    }

    /// arch-audit F16 — the red control, measured RED at 089dd493 (on a second
    /// box, via the merger), ported and retargeted for the Step-4 fix, and
    /// UN-SKIPPED as this step's acceptance test.
    ///
    /// The defect (mechanism KNOW, architect5): `HealthStore::NAME` and
    /// `daemon_health_rows::DAEMON_HEALTH_STORE` were both the literal string
    /// `"health"`, so `run_health_monitor`'s `health::write` commits were
    /// routed by `persist_dispatch` into the `daemon_health_*` tables — the
    /// commit succeeded but landed in the WRONG SUBSYSTEM's rows, invisible
    /// to `health_monitor_read` and every org-health reader. Health detection
    /// worked only while a TypeScript process was running.
    ///
    /// The fix this test accepts: `HealthStore::NAME` is now
    /// `"health-monitor"`, the store the TS launcher already addresses, and
    /// `persist_dispatch`/`load_ledgers` wire it to `health_monitor_rows`
    /// (merge semantics landed in Step 3, so the two writers are safe). The
    /// wrong-subsystem `daemon_health_*` table set is dropped.
    #[tokio::test]
    async fn chiefd_own_health_commit_is_visible_to_the_health_monitor_publish_route() {
        use crate::store::health;

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        let genesis_manifest = manifest.clone();
        assert!(matches!(
            h.db.org_manifest_genesis(
                manifest.clone(),
                "2026-01-01T00:00:00.000Z".to_owned(),
                crate::store::person_contracts::build::build_organization_person_contracts(
                    &genesis_manifest,
                )
                .expect("person contracts document"),
            )
            .await
            .expect("manifest genesis"),
            crate::store::organization_rows::ManifestGenesisOutcome::Created
        ));

        // STEP 1: chiefd's OWN duty commits a health state — mirrors
        // `run_health_monitor`'s own write, minus `apply_cycle`'s
        // candidate-collection machinery (irrelevant to the wiring question
        // this control checks). The commit itself succeeding is the control:
        // the F16 static prediction was a whole-commit refusal; the measured
        // behavior was a successful commit into the wrong tables.
        let mut own_state = health::HealthMonitorState::empty("e2eco");
        own_state.last_run_at = Some("2026-07-31T00:00:00.000Z".to_owned());
        h.db.mutate(MutationClass::Normal, MutationName("test.health-write"), move |ledgers| {
            health::write(ledgers, &own_state);
            Ok(())
        })
        .await
        .expect("chiefd's own health::write commit");

        // STEP 2 / THE ACCEPTANCE: the TS-facing read (the same read
        // `health_monitor_read` serves) sees exactly what chiefd committed.
        // RED at 089dd493: `read_back` was `None` — "not two writers racing
        // on one row, but two disconnected stores under one name".
        let (read_back, _seq) =
            h.db.health_monitor_read()
                .await
                .expect("health-monitor read")
                .expect("chiefd's own health commit is visible to health_monitor_read (F16 fixed)");
        assert_eq!(
            read_back.last_run_at.as_deref(),
            Some("2026-07-31T00:00:00.000Z"),
            "the visible state is chiefd's own commit, not a stale or foreign document"
        );
    }

    /// Step-4 companion to the control above: the un-cross-wiring must not
    /// resurrect the F17 clobber in the other direction. TS's route-side
    /// publish and chiefd's own duty write now share the `health_monitor_*`
    /// rows; an incident committed through the TS-facing route must survive
    /// chiefd's own full-state write that never observed it (merge, not
    /// replace), and must be deletable by explicit clear.
    #[tokio::test]
    async fn chiefd_own_health_write_merges_with_route_committed_incidents() {
        use crate::store::health;
        use crate::store::health_monitor_rows::{HealthMonitorIncident, HealthMonitorState};

        let h = Harness::open();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        let genesis_manifest = manifest.clone();
        assert!(matches!(
            h.db.org_manifest_genesis(
                manifest.clone(),
                "2026-01-01T00:00:00.000Z".to_owned(),
                crate::store::person_contracts::build::build_organization_person_contracts(
                    &genesis_manifest,
                )
                .expect("person contracts document"),
            )
            .await
            .expect("manifest genesis"),
            crate::store::organization_rows::ManifestGenesisOutcome::Created
        ));

        fn incident(fingerprint: &str) -> HealthMonitorIncident {
            HealthMonitorIncident {
                fingerprint: fingerprint.to_owned(),
                kind: "runtime_ownership_conflict".to_owned(),
                detail: "route-committed".to_owned(),
                first_seen_at: "2026-07-31T00:00:00.000Z".to_owned(),
                last_seen_at: "2026-07-31T00:00:00.000Z".to_owned(),
                count: 1,
                responsible_person_id: None,
                unblock_action: None,
                observed_count: None,
                oldest_at: None,
                acknowledged_at: None,
                alert_recipient_person_id: None,
                impaired_mailbox_person_id: None,
                extra: Default::default(),
            }
        }

        // An incident lands through the TS-facing route.
        let mut route_doc = HealthMonitorState {
            version: 1,
            organization: "e2eco".to_owned(),
            last_run_at: Some("2026-07-31T00:00:00.000Z".to_owned()),
            cursors: Default::default(),
            observations: Default::default(),
            incidents: [("R".to_owned(), incident("R"))].into_iter().collect(),
            terminal_resolutions: Default::default(),
            cleared_fingerprints: Vec::new(),
            extra: Default::default(),
        };
        h.db.health_monitor_publish(route_doc.clone()).await.expect("route publish");

        // chiefd's own duty writes a full state that never observed "R".
        let mut own_state = health::HealthMonitorState::empty("e2eco");
        own_state.last_run_at = Some("2026-07-31T00:05:00.000Z".to_owned());
        h.db.mutate(MutationClass::Normal, MutationName("test.health-write"), move |ledgers| {
            health::write(ledgers, &own_state);
            Ok(())
        })
        .await
        .expect("chiefd health write");
        let (merged, _seq) = h.db.health_monitor_read().await.expect("read").expect("doc present");
        assert!(
            merged.incidents.contains_key("R"),
            "chiefd's own full-state write erased a route-committed incident it never \
             observed — the F16 fix must not trade invisibility for a live clobber \
             (Step 3 merge semantics must cover the daemon writer too)"
        );

        // An explicit clear deletes it (positive evidence, not absence).
        route_doc.incidents.clear();
        route_doc.cleared_fingerprints = vec!["R".to_owned()];
        h.db.health_monitor_publish(route_doc).await.expect("clearing publish");
        let (cleared, _seq) = h.db.health_monitor_read().await.expect("read").expect("doc present");
        assert!(
            !cleared.incidents.contains_key("R"),
            "an explicit clearedFingerprints entry must delete the incident row"
        );

        // And a clear is NOT a terminal resolution: a recurrence re-appears.
        let mut recur = cleared.clone();
        recur.incidents.insert("R".to_owned(), incident("R"));
        h.db.health_monitor_publish(recur).await.expect("recurrence publish");
        let (recurred, _seq) =
            h.db.health_monitor_read().await.expect("read").expect("doc present");
        assert!(
            recurred.incidents.contains_key("R"),
            "a cleared (not terminally resolved) fingerprint must accept a later recurrence"
        );
    }

    #[tokio::test]
    async fn activity_reconstructs_from_rows_across_a_reopen_and_never_touches_documents() {
        use crate::store::activity;
        use crate::store::organization;

        let h = Harness::open();
        // activity::rows::backfill_activity validates the manifest's embedded
        // slug against the company-db row slug ("e2eco" here, via Harness::open);
        // the fixture defaults to "northstar-conformance".
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "e2eco".to_string();
        let manifest_for_mutation = manifest.clone();
        h.db.mutate(MutationClass::Normal, MutationName("seed-manifest"), move |l| {
            organization::create(l, &manifest_for_mutation)
        })
        .await
        .expect("manifest commit");
        h.db.mutate(MutationClass::Normal, MutationName("seed-activity"), move |l| {
            activity::seed(l, &manifest).map(|_| ())
        })
        .await
        .expect("activity commit");

        // BLOB-DEATH: neither the manifest NOR the activity write dispatched
        // to the `documents` blob table -- both are rows-authoritative, and
        // this is a change-only mutation, not a double-write.
        assert!(
            documents_table_is_absent(&h.path),
            "org-manifest and activity must never land in the documents table"
        );
        h.db.shutdown();

        // Reopen: `load_ledgers` must reconstruct the activity ledger from
        // rows (after reconstructing the manifest it depends on), not find it
        // absent because the (now-unused) documents blob has nothing.
        let reopened = CompanyDb::open("e2eco", &h.path, h.clock.clone()).expect("reopen");
        let person_order = reopened
            .read(|snapshot| {
                let manifest =
                    organization::read(snapshot.ledgers()).expect("manifest reconstructed");
                activity::read(snapshot.ledgers(), &manifest).map(|a| a.person_order)
            })
            .expect("activity reconstructed from rows at open");
        assert!(
            !person_order.is_empty(),
            "the seeded activity ledger's person_order must survive the reopen"
        );
    }

    #[tokio::test]
    async fn converge_safety_reconstructs_from_rows_across_a_reopen() {
        use crate::store::converge_safety::{self, ActuationMode};

        let h = Harness::open_named();
        h.db.mutate(MutationClass::Normal, MutationName("seed-converge-safety"), |l| {
            Ok(converge_safety::set_actuation_config(l, ActuationMode::Apply, true, false))
        })
        .await
        .expect("commit");

        // BLOB-DEATH: the converge-safety write dispatched to
        // converge_safety_rows, never to the `documents` blob table.
        assert!(
            documents_table_is_absent(&h.path),
            "converge-safety must never land in the documents table"
        );

        h.db.shutdown();

        // Reopen: `load_ledgers` must reconstruct the state from rows, not
        // find it absent because the (now-unused) documents blob has nothing.
        let reopened = CompanyDb::open("e2eco", &h.path, h.clock.clone()).expect("reopen");
        let read_back =
            reopened.read(|snapshot| converge_safety::read(snapshot.ledgers()).into_parts().0);
        assert_eq!(read_back.actuation_mode, ActuationMode::Apply);
        assert!(read_back.sweep_live);
    }

    #[tokio::test]
    async fn supervisor_watermark_reconstructs_from_rows_across_a_reopen() {
        use crate::store::context::CompanyContext;
        use crate::store::supervisor_watermark::{self, Duty};

        let h = Harness::open_named();
        let ctx = CompanyContext::new("e2eco", "chief", ["chief"].map(String::from));
        h.db.mutate(MutationClass::Normal, MutationName("seed-watermark"), move |l| {
            supervisor_watermark::record_success(l, &ctx, Duty::MailboxWake, 1_784_116_800_000);
            Ok(())
        })
        .await
        .expect("commit");

        // BLOB-DEATH: the watermark write dispatched to supervisor_watermark_rows,
        // never to the `documents` blob table.
        assert!(
            documents_table_is_absent(&h.path),
            "supervisor-watermark must never land in the documents table"
        );

        h.db.shutdown();

        // Reopen: `load_ledgers` must reconstruct the watermark from rows, not
        // find it absent because the (now-unused) documents blob has nothing.
        let reopened = CompanyDb::open("e2eco", &h.path, h.clock.clone()).expect("reopen");
        let ctx = CompanyContext::new("e2eco", "chief", ["chief"].map(String::from));
        let has_duty = reopened.read(|snapshot| {
            let (state, _warning) =
                supervisor_watermark::read(snapshot.ledgers(), &ctx).into_parts();
            state.duties.contains_key(Duty::MailboxWake.as_str())
        });
        assert!(has_duty, "mailbox-wake duty reconstructed from rows must survive the reopen");
    }

    #[tokio::test]
    async fn clearing_a_normalized_store_is_persisted() {
        let h = Harness::open_named();
        h.db.mutate(MutationClass::Normal, MutationName("seed"), |l| {
            touch_normalized_store(l);
            Ok(())
        })
        .await
        .expect("commit");
        h.db.mutate(MutationClass::Normal, MutationName("drop"), |l| {
            Ok(crate::store::converge_safety::clear(l))
        })
        .await
        .expect("commit");
        let conn = open_company_db(&h.path).expect("reopen for read-back");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM converge_safety", [], |row| row.get(0))
            .expect("count rows");
        assert_eq!(count, 0, "the real store's clear is persisted");
        assert!(documents_table_is_absent(&h.path));
    }

    fn mailbox_rows_on_disk(path: &Path) -> Vec<(String, String, String)> {
        let conn = open_company_db(path).expect("reopen for read-back");
        let mut stmt = conn
            .prepare("SELECT envelope_id, person, state FROM mailbox ORDER BY envelope_id")
            .expect("prepare");
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows");
        rows
    }

    #[tokio::test]
    async fn a_mailbox_row_survives_a_reopen_and_its_removal_is_persisted() {
        let h = Harness::open_named();
        h.db.mutate(MutationClass::Normal, MutationName("enqueue"), |l| {
            l.put_mailbox(
                "e1@bob",
                MailboxRow {
                    person: "bob".to_string(),
                    // Columnarized: a typed envelope, not an opaque body blob.
                    envelope: crate::store::mailbox::MailboxEnvelope {
                        schema_version: crate::store::mailbox::MAILBOX_ENVELOPE_SCHEMA_VERSION,
                        id: "e1".to_string(),
                        organization: String::new(),
                        from_person_id: "chief".to_string(),
                        to: "bob".to_string(),
                        recipients: vec!["bob".to_string()],
                        body: "hi".to_string(),
                        urgency: crate::store::mailbox::Urgency::Normal,
                        reply_to: None,
                        health_incident: None,
                        created_at: "2026-07-20T00:00:00.000Z".to_string(),
                    },
                    state: "pending".to_string(),
                    updated_at: 42,
                },
            );
            Ok(())
        })
        .await
        .expect("commit");
        // Release the writer before a second actor opens the same file.
        h.db.shutdown();

        // The upsert survives a full close + reopen: this is the durable-first
        // guarantee the whole delivery lane rests on — an enqueued envelope is
        // never lost by a restart.
        let reopened = CompanyDb::open("e2eco", &h.path, h.clock.clone()).expect("reopen");
        assert_eq!(
            reopened.read(|s| s.mailbox("e1@bob").map(|row| (
                row.person.clone(),
                row.envelope.organization.clone(),
                row.state.clone(),
                row.updated_at
            ))),
            Some(("bob".to_string(), "e2eco".to_string(), "pending".to_string(), 42)),
        );
        assert_eq!(
            mailbox_rows_on_disk(&h.path),
            vec![("e1@bob".to_string(), "bob".to_string(), "pending".to_string())],
        );

        // And a removal (an archived/drained envelope pruned) is persisted.
        reopened
            .mutate(
                MutationClass::Normal,
                MutationName("prune"),
                |l| Ok(l.remove_mailbox("e1@bob")),
            )
            .await
            .expect("commit");
        reopened.shutdown();
        assert!(mailbox_rows_on_disk(&h.path).is_empty(), "a pruned mailbox row leaves disk");
    }

    #[tokio::test]
    async fn a_mutation_after_shutdown_is_unavailable_rather_than_a_hang() {
        let h = Harness::open();
        h.db.shutdown();
        let err =
            h.db.mutate(MutationClass::Small, MutationName("goal.set"), |l| {
                touch_normalized_store(l);
                Ok(())
            })
            .await
            .expect_err("a stopped actor admits nothing");
        assert!(matches!(err, ChiefdError::Unavailable { reason: "stopping" }));
    }

    #[test]
    fn the_writer_thread_is_named_after_its_company() {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock = Arc::new(ManualClock::default());
        let db =
            CompanyDb::open("cobalt", &dir.path().join(COMPANY_DB_FILENAME), clock).expect("open");
        assert_eq!(db.label(), "cobalt");
        assert!(db.is_admitting());
        db.shutdown();
        assert!(!db.is_admitting());
    }
    // ---- runtime row writes ------------------------------------------------

    fn base_runtime_state() -> crate::store::runtime_rows::RuntimeState {
        let mut process_handles = std::collections::BTreeMap::new();
        process_handles.insert("chief".to_string(), "%1".to_string());
        crate::store::runtime_rows::RuntimeState {
            version: 1,
            organization: None,
            observed_at: "2026-08-04T00:00:00.000Z".into(),
            session: None,
            socket_name: "sock".into(),
            status: "running".into(),
            startup_admission_until: None,
            recovery_fingerprint: None,
            recovery_observed_at: None,
            recovery_confirmed: None,
            recovery: None,
            reconciliation: None,
            process_handles,
            monitor_warnings: vec![],
            missing_durable_person_ids: vec![],
            unexpected_observed_person_ids: vec![],
            extra: std::collections::BTreeMap::new(),
        }
    }

    // ---- runtime_stop_publish: stopped-status and intent-absence are ONE
    // commit (Mandate 4; the `stop_runtime` two-transaction pair) -------------

    fn intent_for(person_ids: &[&str]) -> crate::store::launch_intent_rows::LaunchIntent {
        crate::store::launch_intent_rows::LaunchIntent {
            version: 1,
            organization: "e2eco".into(),
            person_ids: person_ids.iter().map(|id| (*id).to_string()).collect(),
            updated_at: "2026-08-08T00:00:00.000Z".into(),
            attributions: std::collections::BTreeMap::new(),
            extra: std::collections::BTreeMap::new(),
        }
    }

    fn stopped_from(
        state: crate::store::runtime_rows::RuntimeState,
    ) -> crate::store::runtime_rows::RuntimeState {
        let mut stopped = state;
        stopped.observed_at = "2026-08-08T00:01:00.000Z".into();
        stopped.status = "stopped".into();
        stopped.process_handles = std::collections::BTreeMap::new();
        stopped
    }

    /// The regression this verb exists for: an attended stop's promise is "the
    /// next boot is CEO-only", and launch intent is the ONLY thing that
    /// authorizes a non-CEO pane on the next converge pass. When the stopped
    /// projection and the intent deletion were two transactions, every observer
    /// between them saw a company that reported `stopped` while still holding
    /// authority for its whole previous roster.
    #[tokio::test]
    async fn an_attended_stop_commits_stopped_status_and_intent_absence_together() {
        let h = Harness::open_named();
        h.db.runtime_publish(base_runtime_state()).await.expect("genesis publish");
        h.db.launch_intent_publish(intent_for(&["cfo", "cto"])).await.expect("publish intent");
        assert_eq!(
            h.db.launch_intent_read().await.expect("read").expect("present").0.person_ids,
            vec!["cfo".to_string(), "cto".to_string()],
            "precondition: the company holds explicit launch intent",
        );

        let stopped = stopped_from(base_runtime_state());
        h.db.runtime_stop_publish(stopped, "2026-08-08T00:01:00.000Z".into(), true)
            .await
            .expect("stop publish");

        let runtime = h.db.runtime_read().await.expect("runtime read").expect("row").0;
        assert_eq!(runtime.status, "stopped");
        assert!(
            h.db.launch_intent_read()
                .await
                .expect("intent read")
                .is_none_or(|(intent, _)| intent.person_ids.is_empty()),
            "an attended stop leaves NO launch intent, in the same commit that said stopped",
        );
    }

    /// The other half of the contract: a daemon-converged stop narrows intent
    /// elsewhere and must not delete it here, so the flag is a real branch
    /// rather than an always-true argument.
    #[tokio::test]
    async fn a_daemon_converged_stop_publishes_stopped_without_deleting_launch_intent() {
        let h = Harness::open_named();
        h.db.runtime_publish(base_runtime_state()).await.expect("genesis publish");
        h.db.launch_intent_publish(intent_for(&["cfo"])).await.expect("publish intent");

        let stopped = stopped_from(base_runtime_state());
        h.db.runtime_stop_publish(stopped, "2026-08-08T00:01:00.000Z".into(), false)
            .await
            .expect("stop publish");

        assert_eq!(h.db.runtime_read().await.expect("read").expect("row").0.status, "stopped");
        assert_eq!(
            h.db.launch_intent_read().await.expect("read").expect("present").0.person_ids,
            vec!["cfo".to_string()],
            "a daemon-converged stop leaves the fence for the narrowing path that owns it",
        );
    }

    // TOMBSTONE: `observation_emits_exactly_one_feed_call_for_a_changed_pass`
    // and `observation_emits_zero_feed_calls_for_an_unchanged_pass`. Both drove
    // `runtime_publish_observation`, which is deleted with the observation it
    // committed. The feed properties they pinned — one call per changed row,
    // none for an unchanged one — belong to `publish` and are asserted for the
    // writers that remain.

    #[tokio::test]
    async fn observation_failure_leaves_the_database_unchanged() {
        // D19: `org.runtime.publish` is one `BEGIN IMMEDIATE ...
        // COMMIT` transaction. Prove it with a genuine crash, not an injected
        // `Err`: open a SECOND raw connection to the SAME on-disk file,
        // BEGIN IMMEDIATE, rewrite a runtime_process_handles row, and drop the
        // connection WITHOUT committing — exactly what a SIGKILL leaves
        // behind. SQLite auto-rolls-back an unfinished transaction when the
        // connection that held it closes.
        let h = Harness::open_named();
        h.db.runtime_publish(base_runtime_state()).await.expect("genesis publish");
        let before = h.db.runtime_read().await.expect("read").expect("row exists");

        let (sink, calls) = recording_sink();
        h.db.set_change_feed_sink(sink);

        {
            #[allow(clippy::disallowed_methods)] // crash-simulation seam, see this test's doc
            let crashing = rusqlite::Connection::open(&h.path).expect("open for crash simulation");
            crashing.execute("BEGIN IMMEDIATE", []).expect("begin");
            crashing
                .execute(
                    "UPDATE runtime_process_handles SET process_handle = 'crashed' WHERE slug = 'e2eco' AND person = 'chief'",
                    [],
                )
                .expect("partial rewrite");
            // No COMMIT. `crashing` drops here, uncommitted.
        }

        let after = h.db.runtime_read().await.expect("read").expect("row exists");
        assert_eq!(
            after, before,
            "a crashed mid-write must leave the runtime row exactly as it was"
        );
        assert!(
            calls.lock().unwrap_or_else(|p| p.into_inner()).is_empty(),
            "a crash must emit zero feed events"
        );
    }
}

/// M9 (plan §5.6): the `host_actions` journal shares the writer's transaction.
///
/// These live beside the document tests deliberately. The whole claim of the
/// host-transaction design is that commit 2 — *manifest advance and intent
/// close* — is **one** transaction; that is a property of this actor, not of
/// `chiefd-host`, and it is asserted here where the transaction is.
#[cfg(test)]
mod host_action_tests {
    use super::*;
    use crate::store::{open_company_db, COMPANY_DB_FILENAME};
    use crate::test_support::ManualClock;

    fn open(path: &std::path::Path) -> Arc<CompanyDb> {
        let clock: SharedClock = Arc::new(ManualClock::default());
        Arc::new(CompanyDb::open("cobalt", path, clock).expect("open"))
    }

    #[tokio::test]
    async fn a_journalled_intent_survives_a_reopen() {
        // Commit 1 is worthless if it does not outlive the process.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        {
            let db = open(&path);
            db.mutate(MutationClass::Small, MutationName("host-txn.intent"), |ledgers| {
                ledgers.put_host_action(
                    "act-1",
                    HostActionRecord::pending("materialize", r#"{"files":[]}"#, ledgers.now()),
                );
                ledgers.advance_host_action("act-1", HostActionPhase::Published);
                Ok(())
            })
            .await
            .expect("journal");
        }

        let db = open(&path);
        let (phase, plan, kind) = db.read(|snapshot| {
            let record = snapshot.host_action("act-1").expect("row survives");
            (record.phase(), record.plan_json().to_owned(), record.kind().to_owned())
        });
        assert_eq!(phase, HostActionPhase::Published);
        assert_eq!(plan, r#"{"files":[]}"#);
        assert_eq!(kind, "materialize");
    }

    /// WIRE round-trip (regression for the #53 flip's 49/65 supervision failures):
    /// the launcher-authored relational half — effects,
    /// nextEffectSequence — is `#[serde(skip)]` on SupervisionLedger, so a plain
    /// deserialize of the HTTP body drops it. `supervision_publish` must re-adopt
    /// it from the RAW body. Publish a ledger whose JSON carries effects,
    /// assert the *running snapshot* sees them immediately, then read them back
    /// across a reopen.
    /// The chiefd-core rows tests miss this because they populate via setters,
    /// never a serde-deserialize of a wire body.
    #[tokio::test]
    async fn supervision_publish_preserves_wire_authored_effects() {
        use crate::store::{organization, supervision};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        let clock = Arc::new(ManualClock::default());
        let db = Arc::new(CompanyDb::open("cobalt", &path, clock.clone()).expect("open"));
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "cobalt".to_string();
        let genesis_manifest = manifest.clone();
        assert!(matches!(
            db.org_manifest_genesis(
                manifest.clone(),
                "2026-01-01T00:00:00.000Z".to_owned(),
                crate::store::person_contracts::build::build_organization_person_contracts(
                    &genesis_manifest,
                )
                .expect("person contracts document"),
            )
            .await
            .expect("manifest genesis"),
            crate::store::organization_rows::ManifestGenesisOutcome::Created
        ));
        let seed = supervision::SupervisionLedger::initial(&manifest, "2026-07-25T00:00:00.000Z");
        db.supervision_publish(
            supervision::to_launcher_json(&seed).expect("serialize seed ledger"),
        )
        .await
        .expect("first direct supervision publish seeds the live actor snapshot");
        // #637: the launcher write must use the actor mutation path so the
        // snapshot ChiefD's duties read and the watched feed advance together.
        /// One recorded change-feed call: label, store, body, updated-at
        /// stamp, removed flag.
        type FeedCall = (String, String, String, String, bool);
        let calls: Arc<Mutex<Vec<FeedCall>>> = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&calls);
        db.set_change_feed_sink(Arc::new(move |label, store, body, updated_at, removed| {
            observed.lock().expect("sink lock").push((
                label.to_string(),
                store.to_string(),
                body.to_string(),
                updated_at.to_string(),
                removed,
            ));
        }));
        let seed_body = db.read(|snapshot| {
            let manifest = organization::read(snapshot.ledgers()).expect("seed manifest");
            let ledger =
                supervision::read(snapshot.ledgers(), &manifest).expect("seed supervision");
            supervision::to_launcher_json(&ledger).expect("serialize seed supervision")
        });
        let mut body: serde_json::Value = serde_json::from_str(&seed_body).expect("seed wire body");
        let object = body.as_object_mut().expect("seed body object");
        object.insert("organization".to_string(), serde_json::json!("cobalt"));
        object.insert("updatedAt".to_string(), serde_json::json!("2026-07-25T00:00:00.000Z"));
        object.insert("effectOrder".to_string(), serde_json::json!(["e1"]));
        object.insert(
            "effects".to_string(),
            serde_json::json!({ "e1": {
                // The RELATIONAL HALF the struct #[serde(skip)]s -- must survive the wire.
                "id": "e1", "sequence": 1, "type": "person_reminder", "status": "pending",
                "createdAt": "2026-07-25T00:00:00.000Z", "reminderId": "r1",
                "scalarValue": "plain",
                "singletonArray": ["only"],
                "manyArray": [11, 22, 33],
                "emptyArray": []
            }}),
        );
        object.insert("nextEffectSequence".to_string(), serde_json::json!(2));
        let body = body.to_string();

        // The launcher body is now stale: a native effect commits between the
        // launcher's read and its publish. This is the exact Unit01
        // interleaving that formerly reused sequence 1 and surfaced SQLite's
        // unique constraint as `company-db`.
        db.mutate(MutationClass::Normal, MutationName("native-effect"), |ledgers| {
            let manifest = organization::read(ledgers)?;
            supervision::mutate(ledgers, &manifest, |draft, at| {
                draft.enqueue_effect_for_test("native-effect", "reconcile_escalation", at)?;
                Ok(())
            })
        })
        .await
        .expect("native effect commits after the launcher read");
        {
            let mut race_hints = calls.lock().expect("sink lock");
            assert_eq!(
                race_hints.len(),
                1,
                "the intentional native race mutation emits its own single watch hint"
            );
            assert_eq!(race_hints[0].1, crate::store::supervision::rows::SUPERVISION_STORE);
            // The assertions below measure the direct publish itself. Keeping
            // this earlier committed hint in the same observation window made
            // one hint per mutation look like two hints from one publish.
            race_hints.clear();
        }

        let first = db
            .supervision_publish(body.clone())
            .await
            .expect("stale direct publish merges instead of colliding in SQLite");
        assert!(first > 0, "first publish must return an immutable audit cursor");
        let first_hint = calls.lock().expect("sink lock").clone();
        assert_eq!(first_hint.len(), 1, "direct supervision publish emits exactly one watch hint");
        assert_eq!(first_hint[0].0, "cobalt");
        assert_eq!(first_hint[0].1, crate::store::supervision::rows::SUPERVISION_STORE);
        assert!(
            !first_hint[0].2.is_empty(),
            "the actor publishes its committed supervision change"
        );
        assert!(!first_hint[0].3.is_empty(), "watch hint carries a committed timestamp");
        assert!(!first_hint[0].4, "a direct publish is an update, never a removal");
        let live_sequences = db.read(|snapshot| {
            ["native-effect", "e1"].map(|id| {
                let effect: crate::store::supervision::Effect = serde_json::from_str(
                    &snapshot.effect(id).expect("effect survives the raced publish").body,
                )
                .expect("effect decodes");
                (id, effect.sequence)
            })
        });
        assert_eq!(live_sequences, [("native-effect", 1), ("e1", 2)]);
        clock.advance(std::time::Duration::from_millis(1));
        let second = db
            .supervision_publish(body)
            .await
            .expect("a direct republish must advance the current transaction cursor");
        assert!(second > first, "a direct republish appends its immutable audit events");
        assert_eq!(
            calls.lock().expect("sink lock").len(),
            2,
            "each committed direct publish gets one hint"
        );

        drop(db);
        let db = open(&path);
        let reopened_body = db.read(|snapshot| {
            snapshot.effect("e1").expect("effect loaded into actor snapshot on reopen").body.clone()
        });
        let reopened_effect: crate::store::supervision::Effect =
            serde_json::from_str(&reopened_body).expect("reopened effect body");
        assert_eq!(reopened_effect.payload["scalarValue"], serde_json::json!("plain"));
        assert_eq!(reopened_effect.payload["singletonArray"], serde_json::json!(["only"]));
        assert_eq!(reopened_effect.payload["manyArray"], serde_json::json!([11, 22, 33]));
        assert_eq!(reopened_effect.payload["emptyArray"], serde_json::json!([]));

        let (ledger, _seq) =
            db.supervision_read().await.expect("read").expect("ledger present after publish");
        assert_eq!(
            ledger.effect_order(),
            ["native-effect", "e1"],
            "native and wire-authored effects survived"
        );
        let effect = ledger.effect("e1").expect("effect row present");
        assert_eq!(effect.payload["scalarValue"], serde_json::json!("plain"));
        assert_eq!(effect.payload["singletonArray"], serde_json::json!(["only"]));
        assert_eq!(effect.payload["manyArray"], serde_json::json!([11, 22, 33]));
        assert_eq!(effect.payload["emptyArray"], serde_json::json!([]));
        assert_eq!(
            ledger.next_effect_sequence(),
            3,
            "counter advances beyond both native and launcher effects"
        );
    }

    /// #954: `supervision_publish_cas` on a seq MATCH must reach the same
    /// live pipeline `supervision_publish` does -- the positive half of the
    /// pair proving the real `persist`/`relational_diff` path ran, not a
    /// bypassed no-op. Companion to the seq-mismatch test below, which
    /// proves the negative.
    #[tokio::test]
    async fn supervision_publish_cas_commits_the_relational_tail_on_a_seq_match() {
        use crate::store::{organization, supervision};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        let clock: SharedClock = Arc::new(ManualClock::default());
        let db = Arc::new(CompanyDb::open("cobalt", &path, clock).expect("open"));
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "cobalt".to_string();
        let genesis_manifest = manifest.clone();
        db.org_manifest_genesis(
            manifest.clone(),
            "2026-01-01T00:00:00.000Z".to_owned(),
            crate::store::person_contracts::build::build_organization_person_contracts(
                &genesis_manifest,
            )
            .expect("person contracts document"),
        )
        .await
        .expect("manifest genesis");
        let seed = supervision::SupervisionLedger::initial(&manifest, "2026-07-25T00:00:00.000Z");
        db.supervision_publish(supervision::to_launcher_json(&seed).expect("serialize seed"))
            .await
            .expect("seed publish");

        let expected_seq = db.org_current_seq().await.expect("read seq before CAS publish");
        let seed_body = db.read(|snapshot| {
            let manifest = organization::read(snapshot.ledgers()).expect("seed manifest");
            let ledger =
                supervision::read(snapshot.ledgers(), &manifest).expect("seed supervision");
            supervision::to_launcher_json(&ledger).expect("serialize seed supervision")
        });
        let mut body: serde_json::Value = serde_json::from_str(&seed_body).expect("seed wire body");
        let object = body.as_object_mut().expect("seed body object");
        object.insert("organization".to_string(), serde_json::json!("cobalt"));
        object.insert("updatedAt".to_string(), serde_json::json!("2026-07-25T00:00:00.000Z"));
        object.insert("effectOrder".to_string(), serde_json::json!(["e1"]));
        object.insert(
            "effects".to_string(),
            serde_json::json!({ "e1": {
                "id": "e1", "sequence": 1, "type": "person_reminder", "status": "pending",
                "createdAt": "2026-07-25T00:00:00.000Z", "reminderId": "r1"
            }}),
        );
        object.insert("nextEffectSequence".to_string(), serde_json::json!(2));

        let seq = db
            .supervision_publish_cas(body.to_string(), expected_seq)
            .await
            .expect("seq-matching CAS publish commits");
        assert!(seq > expected_seq, "a commit must advance the audit cursor");
        let live_body = db.read(|snapshot| snapshot.effect("e1").map(|e| e.body.clone()));
        let live_effect: supervision::Effect = serde_json::from_str(&live_body.expect(
            "the effect must be visible in the running snapshot immediately -- \
                 this is the proof the real dispatch_persist/relational_diff pipeline ran, \
                 not a bypassed no-op apply",
        ))
        .expect("live effect decodes");
        assert_eq!(live_effect.id, "e1");
    }

    /// #954: the ordering exerciser architect required. This is the FIRST
    /// place anywhere in this crate that pairs a real `txn_step` with a real
    /// (non-no-op) `apply` -- every existing `enqueue(Some(step), ...)`
    /// caller pairs its step with an apply that only returns a pre-stashed
    /// value, so this combination had no prior coverage before this test. A
    /// conflict test that only asserts the error is returned would pass
    /// even if `apply` had run and been rolled back for some other reason,
    /// or if the ordering were wrong in a way that happened not to bite --
    /// so this reads the ledger back afterward and asserts the effect
    /// this call's body carried is PROVABLY ABSENT, the only assertion that
    /// distinguishes "the txn_step ran first" from "the txn_step ran at
    /// all."
    #[tokio::test]
    async fn supervision_publish_cas_rejects_a_stale_seq_and_the_relational_tail_never_lands() {
        use crate::store::{organization, supervision};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        let clock: SharedClock = Arc::new(ManualClock::default());
        let db = Arc::new(CompanyDb::open("cobalt", &path, clock).expect("open"));
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "cobalt".to_string();
        let genesis_manifest = manifest.clone();
        db.org_manifest_genesis(
            manifest.clone(),
            "2026-01-01T00:00:00.000Z".to_owned(),
            crate::store::person_contracts::build::build_organization_person_contracts(
                &genesis_manifest,
            )
            .expect("person contracts document"),
        )
        .await
        .expect("manifest genesis");
        let seed = supervision::SupervisionLedger::initial(&manifest, "2026-07-25T00:00:00.000Z");
        db.supervision_publish(supervision::to_launcher_json(&seed).expect("serialize seed"))
            .await
            .expect("seed publish");

        let current_seq = db.org_current_seq().await.expect("read current seq");
        let stale_seq = current_seq - 1;
        let seed_body = db.read(|snapshot| {
            let manifest = organization::read(snapshot.ledgers()).expect("seed manifest");
            let ledger =
                supervision::read(snapshot.ledgers(), &manifest).expect("seed supervision");
            supervision::to_launcher_json(&ledger).expect("serialize seed supervision")
        });
        let mut body: serde_json::Value = serde_json::from_str(&seed_body).expect("seed wire body");
        let object = body.as_object_mut().expect("seed body object");
        object.insert("organization".to_string(), serde_json::json!("cobalt"));
        object.insert("updatedAt".to_string(), serde_json::json!("2026-07-25T00:00:00.000Z"));
        object.insert("effectOrder".to_string(), serde_json::json!(["e1"]));
        object.insert(
            "effects".to_string(),
            serde_json::json!({ "e1": {
                "id": "e1", "sequence": 1, "type": "person_reminder", "status": "pending",
                "createdAt": "2026-07-25T00:00:00.000Z", "reminderId": "r1"
            }}),
        );
        object.insert("nextEffectSequence".to_string(), serde_json::json!(2));
        // This body is well-formed and WOULD be accepted on a seq match --
        // deliberately, so that if the ordering guarantee under test were
        // ever wrong, the failure this test would produce is a wrongly
        // PRESENT effect, not a parse error masking the real question.

        let error = db
            .supervision_publish_cas(body.to_string(), stale_seq)
            .await
            .expect_err("a stale expected_seq must be refused, not silently accepted");
        assert!(
            matches!(error, ChiefdError::Conflict { code: "seq-conflict", .. }),
            "the caller must be able to distinguish a CAS conflict from any other failure: got {error:?}"
        );

        let effect_present = db.read(|snapshot| snapshot.effect("e1").is_some());
        assert!(
            !effect_present,
            "the effect this rejected call's body carried must be PROVABLY ABSENT -- \
             a present effect here would mean `apply` ran despite the txn_step's \
             conflict, i.e. the ordering guarantee did not hold for this pairing",
        );
        assert_eq!(
            db.org_current_seq().await.expect("read seq after rejected CAS"),
            current_seq,
            "a rejected CAS must not advance the audit cursor either"
        );
    }

    #[tokio::test]
    async fn the_manifest_advance_and_the_intent_close_commit_or_roll_back_together() {
        // Commit 2's atomicity, tested by making it fail: the closure advances
        // the manifest *and* closes the intent, then `validate()` rejects the
        // result. Neither half may survive — a manifest that advanced with the
        // intent still open would be replayed by recovery, and an intent closed
        // with the manifest behind would lose the effect forever.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        let db = open(&path);
        db.mutate(MutationClass::Small, MutationName("seed"), |ledgers| {
            ledgers.put_host_action(
                "act-1",
                HostActionRecord::pending("materialize", "{}", ledgers.now()),
            );
            Ok(())
        })
        .await
        .expect("seed");

        let refusal = db
            .mutate(MutationClass::Small, MutationName("host-txn.commit"), |ledgers| {
                ledgers.close_host_action("act-1");
                // …and a second normalized row whose invalid plan validate() hates.
                ledgers.put_host_action(
                    "broken",
                    HostActionRecord::pending("materialize", "not json", ledgers.now()),
                );
                Ok(())
            })
            .await
            .expect_err("validate rejects");
        assert_eq!(refusal.kind(), "Refused");

        db.read(|snapshot| {
            assert!(snapshot.host_action("act-1").is_some(), "the intent is still open");
        });

        // Same again on disk, after a reopen: the rollback is durable, not just
        // in memory.
        drop(db);
        let db = open(&path);
        db.read(|snapshot| {
            assert!(snapshot.host_action("act-1").is_some());
        });
    }

    #[tokio::test]
    async fn closing_an_intent_deletes_its_row_rather_than_tombstoning_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        {
            let db = open(&path);
            db.mutate(MutationClass::Small, MutationName("seed"), |ledgers| {
                ledgers.put_host_action(
                    "act-1",
                    HostActionRecord::pending("materialize", "{}", ledgers.now()),
                );
                Ok(())
            })
            .await
            .expect("seed");
            db.mutate(MutationClass::Small, MutationName("host-txn.commit"), |ledgers| {
                assert!(ledgers.close_host_action("act-1"));
                Ok(())
            })
            .await
            .expect("close");
        }
        let conn = open_company_db(&path).expect("reopen");
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM host_actions", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 0, "a closed intent leaves no litter to scan at every startup");
    }

    #[tokio::test]
    async fn an_intent_whose_plan_is_not_json_is_refused_before_it_is_journalled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = open(&dir.path().join(COMPANY_DB_FILENAME));
        let error = db
            .mutate(MutationClass::Small, MutationName("host-txn.intent"), |ledgers| {
                ledgers.put_host_action(
                    "act-1",
                    HostActionRecord::pending("materialize", "not json", ledgers.now()),
                );
                Ok(())
            })
            .await
            .expect_err("validate rejects");
        match error {
            ChiefdError::Refused(refusal) => {
                assert_eq!(refusal.code, "host-action-plan-not-object");
                assert!(!refusal.legal_routes.is_empty());
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        db.read(|snapshot| assert!(snapshot.host_action("act-1").is_none()));
    }

    #[test]
    fn a_row_with_an_unreadable_phase_refuses_the_open_instead_of_guessing() {
        // Fail-closed (plan §5.5). Both guesses lose real work: "closed"
        // abandons a filesystem rollback, "pending" rolls back a publish that
        // had already completed.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        {
            let conn = open_company_db(&path).expect("open");
            conn.execute_batch("PRAGMA ignore_check_constraints=ON;")
                .expect("corruption fixture bypasses fresh-schema checks");
            conn.execute(
                "INSERT INTO host_actions(
                    action_id, kind, payload_schema, plan_json, phase, created_at
                 ) VALUES (
                    'act-1','materialize','host-txn-v1','{}','half-done',0
                 )",
                [],
            )
            .expect("insert");
            conn.execute_batch("PRAGMA ignore_check_constraints=OFF;").expect("restore checks");
        }
        let clock: SharedClock = Arc::new(ManualClock::default());
        let error = CompanyDb::open("cobalt", &path, clock).expect_err("refuses to open");
        assert!(matches!(error, OpenError::CorruptJournal { .. }), "got {error:?}");
        assert!(error.to_string().contains("half-done"));
    }

    #[tokio::test]
    async fn open_intents_are_ordered_by_creation_not_by_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = open(&dir.path().join(COMPANY_DB_FILENAME));
        db.mutate(MutationClass::Small, MutationName("seed"), |ledgers| {
            ledgers.put_host_action(
                "zzz",
                HostActionRecord::pending("materialize", "{}", WallMillis(10)),
            );
            ledgers.put_host_action(
                "aaa",
                HostActionRecord::pending("materialize", "{}", WallMillis(20)),
            );
            Ok(())
        })
        .await
        .expect("seed");
        let order: Vec<String> = db.read(|snapshot| {
            snapshot.open_host_actions().into_iter().map(|(id, _)| id.to_owned()).collect()
        });
        assert_eq!(order, vec!["zzz".to_string(), "aaa".to_string()]);
    }
}
