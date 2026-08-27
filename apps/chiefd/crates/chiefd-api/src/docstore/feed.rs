//! The docstore change-feed: a bounded ring of [`WatchEvent`] hints plus a
//! [`tokio::sync::broadcast`] sender, published once per **committed**
//! `org_documents` mutation. This is the Rust foundation of
//! the design record (PR #255) — see the design record
//! (design doc, PR #258) for the decisions this module implements.
//!
//! # Emission point
//!
//! [`ChangeFeed`] is owned by [`super::store::DocStore`] and published from
//! the **store layer**, after the outcome check, never from
//! [`super::engine::DocEngine`]: a CAS loss and a CAS win are both successful
//! SQL statements, and only the store layer knows `rows_affected == 1` means
//! "this one actually applied" (`store.rs`'s `insert_if_absent`, `cas_update`,
//! `drop_company`, `drop_company_store`). Publication happens on the
//! **caller's** async task, after `await`ing the engine — concurrent HTTP
//! writers on a multi-thread runtime therefore race each other to publish,
//! and commit order (serialized by the engine's single writer thread) is
//! NOT publish order. To keep the wire strictly seq-monotonic regardless,
//! `seq` assignment, the ring push, and the broadcast send are one atomic
//! critical section guarded by a single lock (see [`ChangeFeed::publish`]):
//! whichever concurrent publisher gets the lock first is assigned the lower
//! seq AND is the one observed first by every live subscriber and in ring
//! order, so publish order always matches seq order even when it does not
//! match commit order.
//!
//! # Wire event shape
//!
//! `WatchEvent { seq, slug, store, updated_at, removed }`:
//!
//! - `seq` — one `u64` counter for the whole process (not per-doc), so
//!   Last-Event-ID replay and gap detection are a single comparison. `seq`
//!   order is publish order (see "Emission point" above), which — for
//!   concurrent writers, including two writes to the *same* doc — is not
//!   guaranteed to match commit order; a subscriber must treat `seq` as an
//!   ordering token for delivery/dedup only, never as a proxy for which
//!   write committed first.
//! - `removed` — `true` for `drop_company` / `drop_company_store`. A removal
//!   also carries `updated_at: ""`: there is no caller-supplied clock reading
//!   (`drop_company`/`drop_company_store` take no timestamp — clock authority
//!   stays with the caller everywhere else in this module, and there is no
//!   caller-supplied clock to thread through a delete without widening the
//!   wire request shape, which is out of scope for this slice). Subscribers
//!   must not read `updated_at` on a `removed` event.
//! - `store: "*"` is the wildcard a `drop_company` publishes: the DELETE is a
//!   single statement over every store row for the slug, so the store layer
//!   does not know which individual store names it removed. Subscribers
//!   filtering on a specific `(slug, store)` must treat `store: "*"` as
//!   matching every store for that slug and invalidate accordingly.
//! - Events are hints, never payloads — no `blob`. Docs stay the only truth
//!   (the reconcile-nudge best-effort contract,
//!   `chiefd-host/src/runtime_waker.rs:81-88`): a subscriber reacts to a hint
//!   by re-reading the document, never by trusting the event's fields as the
//!   new state.
//!
//! # Restart / epoch and gap detection
//!
//! No persistence: a process restart resets `seq` to a fresh epoch starting
//! at 1 and empties the ring. [`ChangeFeed::replay_from`] detects both a
//! ring-eviction gap and a restart-epoch gap with the same two comparisons —
//! see its doc comment — so a caller (SSE-B) gets one `Gap` signal to resync
//! from a full document read, without needing an explicit epoch id on the
//! wire.
//!
//! # Non-blocking, infallible
//!
//! [`ChangeFeed::publish`] never blocks and never fails the caller's write: a
//! full ring evicts its oldest entry, and a broadcast send with zero (or
//! lagging) receivers is not an error — same contract as
//! `chiefd-host/src/runtime_waker.rs`'s `ReconcileTrigger` ("a nudge that is
//! dropped only costs next-cycle latency").

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::Serialize;
use tokio::sync::broadcast;

/// Ring capacity and the paired broadcast channel's lag buffer. ~1024 hints
/// is generous relative to the fleet-wide 600ms poll this feed replaces —
///
const DEFAULT_CAPACITY: usize = 1024;

/// One committed-mutation hint on the docstore change-feed. See the module
/// doc for the full wire-shape contract.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WatchEvent {
    /// Monotonic within one chiefd process, across every slug/store.
    pub seq: u64,
    /// The company key.
    pub slug: String,
    /// The store name, or `"*"` for a whole-company `drop_company` (see
    /// module doc).
    pub store: String,
    /// The caller-supplied `updated_at` for an insert/CAS; `""` for a
    /// `removed` event (no clock is threaded through a delete — see module
    /// doc).
    pub updated_at: String,
    /// `true` for `drop_company` / `drop_company_store`.
    pub removed: bool,
}

/// The result of asking the feed to replay everything after `after_seq`.
#[derive(Debug, Clone, PartialEq)]
pub enum Replay {
    /// Every retained event with `seq > after_seq`, oldest first. Empty is a
    /// valid answer ("you are already caught up"), not a gap.
    Events(Vec<WatchEvent>),
    /// `after_seq` cannot be served from the ring: either it names a seq this
    /// process's counter has never reached (a stale Last-Event-ID from a
    /// prior epoch — the process restarted), or the event immediately
    /// following it (`after_seq + 1`) was evicted by the ring bound before
    /// this call. Note that `after_seq` itself being evicted is fine — the
    /// client already has that one; only a missing *successor* is a real
    /// gap. Either way the caller must resync from a full document read, not
    /// from the feed.
    Gap,
}

/// The seq counter and ring, guarded by one lock so a concurrent
/// [`ChangeFeed::publish`] assigns `seq`, pushes the ring, and broadcasts as
/// a single atomic step — see the module doc's "Emission point" section for
/// why this must not be three separately-lockable pieces.
struct FeedState {
    seq: u64,
    ring: VecDeque<WatchEvent>,
}

/// The bounded ring + broadcast sender + seq counter, owned by
/// [`super::store::DocStore`]. See the module doc for the emission-point and
/// wire-shape contract.
pub struct ChangeFeed {
    state: Mutex<FeedState>,
    capacity: usize,
    sender: broadcast::Sender<WatchEvent>,
}

impl Default for ChangeFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl ChangeFeed {
    /// A feed with the default ~1024 ring/broadcast capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// A feed with an explicit ring/broadcast capacity (`0` is treated as
    /// `1` — a feed that could never retain anything is not useful and
    /// `tokio::sync::broadcast::channel` panics on `0`).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _receiver) = broadcast::channel(capacity);
        Self {
            state: Mutex::new(FeedState { seq: 0, ring: VecDeque::with_capacity(capacity) }),
            capacity,
            sender,
        }
    }

    /// Publish one committed-mutation hint. Assigns the next `seq`, pushes
    /// it into the ring (evicting the oldest entry if full), and broadcasts
    /// it to live subscribers. Returns the published event (its `seq` in
    /// particular) so a caller that wants it — e.g. for a response header —
    /// does not need a second lookup.
    ///
    /// `seq` assignment, the ring push, and the broadcast send all happen
    /// while holding [`FeedState`]'s lock, as one critical section. This is
    /// load-bearing, not incidental: `publish` runs on the *caller's* async
    /// task (see the module doc), so concurrent HTTP writers on a
    /// multi-thread runtime genuinely race each other here. Splitting seq
    /// assignment (e.g. a separate `AtomicU64`) from the ring push/broadcast
    /// re-opens the race this function exists to close — two publishers
    /// could then get seq 5 and 6 but push/broadcast 6 before 5, and
    /// `router.rs`'s live dedup (`seq <= last_seq`) would permanently drop
    /// the seq=5 event for every subscriber once seq=6 has been seen. Taking
    /// the lock once and doing all three steps inside it guarantees whoever
    /// is assigned the lower seq is also observed first, by every
    /// subscriber and in the ring, every time.
    ///
    /// Never blocks and never fails: a broadcast send with no subscribers is
    /// dropped, not an error (`broadcast::Sender::send` is synchronous and
    /// does not await, so calling it under this lock cannot deadlock or hold
    /// the lock across a suspension point). Callers MUST only invoke this
    /// after confirming the mutation actually committed (`rows_affected ==
    /// 1` at the call site) — this function itself does no such check, by
    /// design, so it stays reusable for the task-mutation emission
    /// `tasks.rs` needs (SSE-A2), which commits through `exec_interactive`
    /// rather than the `rows_affected` shape this module's own callers use.
    pub fn publish(
        &self,
        slug: impl Into<String>,
        store: impl Into<String>,
        updated_at: impl Into<String>,
        removed: bool,
    ) -> WatchEvent {
        let mut state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.seq += 1;
        let event = WatchEvent {
            seq: state.seq,
            slug: slug.into(),
            store: store.into(),
            updated_at: updated_at.into(),
            removed,
        };
        if state.ring.len() >= self.capacity {
            state.ring.pop_front();
        }
        state.ring.push_back(event.clone());
        // A dropped send (no receivers, or a lagging one) is the designed
        // no-op: the poll floor covers it next cycle. Sent while still
        // holding `state`'s lock — see the doc comment above.
        let _ = self.sender.send(event.clone());
        event
    }

    /// Subscribe to live events from this point forward. Does not replay —
    /// pair with [`ChangeFeed::replay_from`] using a `Last-Event-ID` to
    /// backfill the gap between "reconnect" and "subscribed".
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<WatchEvent> {
        self.sender.subscribe()
    }

    /// Replay every retained event after `after_seq`, or report a [`Gap`]
    /// when `after_seq` cannot be served — see [`Replay`]'s doc for the two
    /// cases this distinguishes (ring eviction vs. a restarted process).
    ///
    /// `after_seq: 0` always returns `Events` (never a gap): `0` is not a
    /// real seq (the counter starts assigning at `1`), so it means "give me
    /// everything currently retained", which the ring can always answer.
    ///
    /// [`Gap`]: Replay::Gap
    #[must_use]
    pub fn replay_from(&self, after_seq: u64) -> Replay {
        let state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if after_seq > state.seq {
            // A Last-Event-ID ahead of what this process has ever assigned
            // can only be a prior epoch's seq — the process restarted.
            return Replay::Gap;
        }
        // `after_seq == 0` is "no Last-Event-ID" (never a real seq — the
        // counter starts assigning at 1), so it always means "give me
        // everything currently retained": the eviction-gap check below is
        // about whether the event immediately AFTER `after_seq` is still
        // present, which is meaningless when there is no such event to ask
        // for in the first place.
        if after_seq == 0 {
            return Replay::Events(state.ring.iter().cloned().collect());
        }
        match state.ring.front() {
            None => Replay::Events(Vec::new()),
            // Contiguous is fine: `oldest.seq == after_seq + 1` means the
            // very next event the client needs is present — the client's
            // own last-seen event (`after_seq`) is allowed to have been
            // evicted, since the client already has it. Only a STRICT gap —
            // something between `after_seq` and `oldest` missing — means
            // data was lost.
            Some(oldest) if after_seq + 1 < oldest.seq => Replay::Gap,
            Some(_) => {
                Replay::Events(state.ring.iter().filter(|e| e.seq > after_seq).cloned().collect())
            }
        }
    }

    /// The oldest `seq` still retained in the ring, or `None` when nothing
    /// has been published yet.
    #[must_use]
    pub fn oldest_seq(&self) -> Option<u64> {
        let state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.ring.front().map(|e| e.seq)
    }

    /// The most recently assigned `seq`, or `0` when nothing has been
    /// published yet in this process (the counter's start value).
    #[must_use]
    pub fn current_seq(&self) -> u64 {
        let state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_assigns_a_strictly_monotonic_seq_starting_at_one() {
        let feed = ChangeFeed::new();
        let a = feed.publish("co@abc", "activity", "t0", false);
        let b = feed.publish("co@abc", "activity", "t1", false);
        let c = feed.publish("co@xyz", "supervision", "t2", false);
        assert_eq!(a.seq, 1);
        assert_eq!(b.seq, 2);
        assert_eq!(c.seq, 3, "seq is one counter for the whole process, not per-doc");
    }

    #[test]
    fn ring_evicts_oldest_once_over_capacity() {
        let feed = ChangeFeed::with_capacity(3);
        feed.publish("co", "s", "t", false);
        feed.publish("co", "s", "t", false);
        feed.publish("co", "s", "t", false);
        feed.publish("co", "s", "t", false); // evicts seq=1
        match feed.replay_from(0) {
            Replay::Events(events) => {
                let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
                assert_eq!(seqs, vec![2, 3, 4], "the ring bound evicted seq=1");
            }
            Replay::Gap => panic!("full ring must not report a gap on after_seq=0"),
        }
    }

    #[test]
    fn replay_from_zero_returns_everything_retained_never_a_gap() {
        let feed = ChangeFeed::new();
        assert_eq!(feed.replay_from(0), Replay::Events(Vec::new()), "nothing published yet");
        feed.publish("co", "s", "t", false);
        match feed.replay_from(0) {
            Replay::Events(events) => assert_eq!(events.len(), 1),
            Replay::Gap => panic!("after_seq=0 must never report a gap"),
        }
    }

    #[test]
    fn replay_from_a_seq_still_in_the_ring_returns_only_the_newer_events() {
        let feed = ChangeFeed::new();
        feed.publish("co", "s", "t", false);
        feed.publish("co", "s", "t", false);
        feed.publish("co", "s", "t", false);
        match feed.replay_from(1) {
            Replay::Events(events) => {
                let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
                assert_eq!(seqs, vec![2, 3]);
            }
            Replay::Gap => panic!("seq=1 is still retained, must not be a gap"),
        }
    }

    #[test]
    fn replay_from_a_seq_already_caught_up_returns_empty_not_a_gap() {
        let feed = ChangeFeed::new();
        feed.publish("co", "s", "t", false);
        assert_eq!(
            feed.replay_from(1),
            Replay::Events(Vec::new()),
            "caught up to the latest seq is a valid empty answer, not a gap"
        );
    }

    #[test]
    fn replay_from_a_seq_whose_evicted_successor_is_gone_reports_a_gap() {
        // Evicting the client's OWN last-seen seq is not itself a gap (the
        // client already has that event); it's a gap only when the event
        // the client needs NEXT is also gone. Capacity 2, four publishes:
        // seq=1 and seq=2 are both evicted, leaving [3, 4].
        let feed = ChangeFeed::with_capacity(2);
        feed.publish("co", "s", "t", false);
        feed.publish("co", "s", "t", false);
        feed.publish("co", "s", "t", false);
        feed.publish("co", "s", "t", false);
        assert_eq!(feed.oldest_seq(), Some(3));
        assert_eq!(
            feed.replay_from(1),
            Replay::Gap,
            "the client needs seq=2 next, and seq=2 was evicted along with seq=1"
        );
    }

    #[test]
    fn replay_from_a_seq_whose_evicted_successor_is_still_present_is_not_a_gap() {
        // The client's own last-seen seq (1) was evicted, but the event it
        // needs NEXT (seq=2) is still retained — contiguous, no data lost.
        let feed = ChangeFeed::with_capacity(2);
        feed.publish("co", "s", "t", false);
        feed.publish("co", "s", "t", false);
        feed.publish("co", "s", "t", false); // evicts seq=1, ring=[2,3]
        assert_eq!(feed.oldest_seq(), Some(2));
        match feed.replay_from(1) {
            Replay::Events(events) => {
                let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
                assert_eq!(seqs, vec![2, 3]);
            }
            Replay::Gap => {
                panic!("seq=2 (what the client needs next) is still retained — must not be a gap")
            }
        }
    }

    #[test]
    fn replay_from_a_seq_never_reached_this_epoch_reports_a_gap() {
        // Simulates Last-Event-ID surviving a restart: the new process's
        // counter has not reached the client's remembered seq yet.
        let feed = ChangeFeed::new();
        feed.publish("co", "s", "t", false); // current_seq() == 1
        assert_eq!(
            feed.replay_from(500),
            Replay::Gap,
            "a Last-Event-ID ahead of this process's counter can only be a prior epoch"
        );
    }

    #[test]
    fn subscribe_receives_events_published_after_it_but_not_before() {
        let feed = ChangeFeed::new();
        feed.publish("co", "s", "t0", false); // before subscribe: must not arrive
        let mut rx = feed.subscribe();
        let published = feed.publish("co", "s", "t1", false);
        let received =
            rx.try_recv().expect("a live subscriber must receive the post-subscribe publish");
        assert_eq!(received, published);
        assert!(rx.try_recv().is_err(), "no more events queued");
    }

    #[test]
    fn publish_with_zero_subscribers_does_not_block_or_panic() {
        let feed = ChangeFeed::new();
        // No `subscribe()` call at all — the broadcast send has nowhere to
        // go and must be silently dropped, not fail the publish.
        let event = feed.publish("co", "s", "t", false);
        assert_eq!(event.seq, 1);
    }

    #[test]
    fn publish_with_a_lagging_subscriber_does_not_block_the_writer() {
        let feed = ChangeFeed::with_capacity(4);
        let mut lagging = feed.subscribe();
        // Publish far more than the broadcast channel's capacity while the
        // subscriber never drains — must not block.
        for _ in 1..=50 {
            feed.publish("co", "s", "t", false);
        }
        assert_eq!(feed.current_seq(), 50, "every publish completed despite the lagging receiver");
        // The lagging receiver observes a `Lagged` error rather than the
        // writer stalling for it.
        match lagging.try_recv() {
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            other => {
                panic!("expected a Lagged error for a receiver that never drained, got {other:?}")
            }
        }
    }

    // A `#[tokio::test]` with no `flavor` runs on the default CURRENT-THREAD
    // runtime: every `tokio::spawn`ed task still runs on the one OS thread
    // that drives the test, cooperatively, never actually in parallel. With
    // no `.await` inside the `for` loop below, each spawned task also runs
    // to completion the instant it is first polled, so the 8 "concurrent"
    // publishers in fact execute one after another, back to back — this
    // test could not have caught #286 (the out-of-order seq/ring/broadcast
    // race), because the race requires two publishers to genuinely overlap
    // inside `publish`. `flavor = "multi_thread"` puts each spawned task on
    // a real OS thread from tokio's pool, so the 8 publishers actually run
    // in parallel and contend for `ChangeFeed`'s internal lock the way
    // concurrent HTTP writers do in production.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_writers_never_duplicate_or_skip_a_seq() {
        use std::sync::Arc;
        let feed = Arc::new(ChangeFeed::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let feed = Arc::clone(&feed);
            handles.push(tokio::spawn(async move {
                for _ in 0..50 {
                    feed.publish("co", "s", "t", false);
                }
            }));
        }
        for h in handles {
            h.await.expect("join");
        }
        assert_eq!(feed.current_seq(), 400, "8 * 50 publishes, one seq each, none lost or doubled");
        // Draining `replay_from(0)` (bounded by ring capacity) must show
        // strictly increasing, non-duplicated seqs among what's retained.
        if let Replay::Events(events) = feed.replay_from(0) {
            let mut seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
            let before_dedup = seqs.len();
            seqs.dedup();
            assert_eq!(seqs.len(), before_dedup, "no duplicate seq made it into the ring");
            assert!(seqs.windows(2).all(|w| w[0] < w[1]), "ring order matches seq order");
        } else {
            panic!("after_seq=0 must never report a gap");
        }
    }

    /// The #286 regression test: reproduces the exact failure mode described
    /// in the ticket. `router.rs`'s live-event dedup drops any event whose
    /// `seq` is `<= last_seq` it has already observed — permanently, for a
    /// healthy channel, since there is no floor fallback while the channel
    /// stays healthy. That is only safe if `publish` guarantees a live
    /// subscriber never observes a lower seq after a higher one. This test
    /// hammers `publish` from many real OS threads at once (multi-thread
    /// runtime, tight loop, no cooperative yielding needed — genuine thread
    /// parallelism supplies the race) and asserts BOTH that a live
    /// subscriber's broadcast order is strictly seq-increasing AND that the
    /// ring's retained order is too. Before the #286 fix (seq assigned via a
    /// bare `AtomicU64::fetch_add` outside the ring lock), this reliably
    /// reproduces an inversion under real parallelism; after the fix, seq
    /// assignment + ring push + broadcast send are one critical section
    /// under a single lock, so no interleaving of concurrent publishers can
    /// invert the order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_publishers_never_broadcast_or_ring_out_of_seq_order() {
        use std::sync::Arc;

        const PUBLISHERS: u64 = 8;
        const PER_PUBLISHER: u64 = 300;
        const TOTAL: u64 = PUBLISHERS * PER_PUBLISHER;

        // Capacity comfortably above TOTAL so nothing is evicted from the
        // ring and the live subscriber never lags — this test is about
        // ORDER, not eviction/backpressure (those have their own tests
        // above).
        let feed = Arc::new(ChangeFeed::with_capacity(TOTAL as usize + 64));
        let mut live = feed.subscribe();

        let mut handles = Vec::new();
        for _ in 0..PUBLISHERS {
            let feed = Arc::clone(&feed);
            handles.push(tokio::spawn(async move {
                for _ in 0..PER_PUBLISHER {
                    feed.publish("co", "s", "t", false);
                }
            }));
        }
        for h in handles {
            h.await.expect("join");
        }

        let mut broadcast_seqs = Vec::with_capacity(TOTAL as usize);
        while let Ok(event) = live.try_recv() {
            broadcast_seqs.push(event.seq);
        }
        assert_eq!(
            broadcast_seqs.len(),
            TOTAL as usize,
            "every publish must reach the live subscriber — capacity exceeds TOTAL, so nothing should lag/evict"
        );
        assert!(
            broadcast_seqs.windows(2).all(|w| w[0] < w[1]),
            "a live subscriber must observe strictly increasing seq order even under concurrent publishers, \
             or router.rs's `seq <= last_seq` dedup permanently drops a live event (#286): {broadcast_seqs:?}"
        );

        match feed.replay_from(0) {
            Replay::Events(events) => {
                let ring_seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
                assert_eq!(
                    ring_seqs.len(),
                    TOTAL as usize,
                    "ring capacity exceeds TOTAL, nothing evicted"
                );
                assert!(
                    ring_seqs.windows(2).all(|w| w[0] < w[1]),
                    "ring order must be strictly increasing seq order even under concurrent publishers: {ring_seqs:?}"
                );
            }
            Replay::Gap => panic!("after_seq=0 must never report a gap"),
        }
    }
}
