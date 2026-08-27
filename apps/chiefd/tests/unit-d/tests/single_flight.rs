//! Unit D / D5 — single-flight and floor interval under concurrent duty ticks.
//!
//! The scheduler can have two overdue SupervisionReconcile ticks in flight for
//! one company (a missed-window backlog plus the live tick). Unit C's durable
//! `cycle_in_progress` claim + the per-company writer-actor serialization must
//! ensure exactly one runs. This file drives that through the **host wrapper**
//! (`converge_apply::safety::{begin_cycle,end_cycle}`) against a real
//! `CompanyDb`, so it exercises the actor serialization, not just the pure
//! check-and-set that `converge_safety::tests` already covers.
//!
//! Every wait is a `ManualClock` advance — no wall sleeps (TESTING.md §4.2).
//!
//! ACTIVATION: move to `crates/chiefd-host/tests/single_flight.rs` once m2-safety
//! (`store::converge_safety` + the host `converge_apply::safety` wrapper) is on
//! the synced main. All D5 cases REAL. (Source already tracked on origin/main; no
//! gitignore blocker.)

// See crash_injection.rs's comment on this same allow: clippy's
// `allow-expect-in-tests` doesn't cover `open()`, a helper called only from
// `#[test]` functions but not itself `#[test]`-attributed.
#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SharedClock;
use chiefd_core::store::converge_safety::{CycleGate, SkipReason, CLAIM_STALE_MS};
use chiefd_core::store::COMPANY_DB_FILENAME;
use chiefd_core::test_support::ManualClock;
use chiefd_host::converge_apply::safety;

const SLUG: &str = "cobalt";
const FLOOR: Duration = Duration::from_millis(5_000);

async fn open(clock: SharedClock) -> (tempfile::TempDir, Arc<CompanyDb>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(COMPANY_DB_FILENAME);
    let db = Arc::new(CompanyDb::open(SLUG, &path, clock).expect("open company db"));
    (dir, db)
}

// --- D5.1: two overdue ticks race, exactly one runs --------------------------

#[tokio::test]
async fn d5_1_two_racing_ticks_yield_exactly_one_proceed() {
    let mc = Arc::new(ManualClock::default());
    let (_dir, db) = open(mc.clone()).await;

    // Two begin_cycle mutations race the single writer actor. Both are ordinary
    // `Reconcile` mutations, so the actor commits them in some order; the second
    // sees the first's claim.
    let a = safety::begin_cycle(&db, FLOOR);
    let b = safety::begin_cycle(&db, FLOOR);
    let (ra, rb) = tokio::join!(a, b);
    let ra = ra.expect("begin a");
    let rb = rb.expect("begin b");

    let proceeds = [ra, rb].iter().filter(|g| g.may_proceed()).count();
    assert_eq!(proceeds, 1, "exactly one of two racing ticks proceeds");
    let skipped_running =
        [ra, rb].iter().any(|g| matches!(g, CycleGate::Skipped(SkipReason::AlreadyRunning)));
    assert!(skipped_running, "the loser is skipped for AlreadyRunning, not silently run");
}

// --- D5.2: floor interval spacing --------------------------------------------

#[tokio::test]
async fn d5_2_floor_interval_gates_the_next_start() {
    let mc = Arc::new(ManualClock::default());
    let (_dir, db) = open(mc.clone()).await;

    assert!(safety::begin_cycle(&db, FLOOR).await.expect("first").may_proceed());
    safety::end_cycle(&db).await.expect("release");

    // Immediately after release, the floor has not elapsed.
    assert!(
        matches!(
            safety::begin_cycle(&db, FLOOR).await.expect("second"),
            CycleGate::Skipped(SkipReason::FloorNotElapsed)
        ),
        "a start inside the floor is skipped",
    );

    // Advance past the floor and it proceeds.
    mc.advance(FLOOR);
    assert!(
        safety::begin_cycle(&db, FLOOR).await.expect("third").may_proceed(),
        "once the floor elapses the next cycle proceeds",
    );
}

// --- D5.3: a crashed claim is reclaimed, never wedged ------------------------

#[tokio::test]
async fn d5_3_a_stale_claim_is_reclaimed_after_the_staleness_window() {
    let mc = Arc::new(ManualClock::default());
    let (_dir, db) = open(mc.clone()).await;

    // Take a claim and then "crash" — never call end_cycle.
    assert!(safety::begin_cycle(&db, FLOOR).await.expect("claim").may_proceed());

    // Before the staleness window, the held claim still blocks (single-flight).
    mc.advance(Duration::from_millis(u64::try_from(CLAIM_STALE_MS - 1).unwrap()));
    assert!(
        matches!(
            safety::begin_cycle(&db, FLOOR).await.expect("still held"),
            CycleGate::Skipped(SkipReason::AlreadyRunning)
        ),
        "a fresh held claim blocks a second cycle",
    );

    // Past the staleness window the crash residue is reclaimed — not wedged.
    mc.advance(Duration::from_millis(2));
    assert!(
        safety::begin_cycle(&db, FLOOR).await.expect("reclaim").may_proceed(),
        "a claim older than CLAIM_STALE_MS is reclaimed by the next cycle",
    );
}

// --- D5.4: a skipped tick is a no-op for the ledger --------------------------
// TODO (REAL once the daemon's `run_supervision_reconcile` calls begin_cycle):
// assert that a `Skipped` actuation half does not touch runtime and does not move
// the SupervisionReconcile watermark (the ledger half owns the watermark). This
// needs the scheduler wired to the safety gate, which the current
// `run_supervision_reconcile` does not yet do (it calls the actuator directly).
