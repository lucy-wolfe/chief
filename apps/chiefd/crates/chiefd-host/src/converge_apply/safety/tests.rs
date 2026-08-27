//! Host safety-seam tests: the config pass-through, and the single-flight +
//! floor-interval cycle lifecycle driven through a **real** per-company writer
//! actor — so the claim's atomic check-and-set and the floor spacing are proven
//! against the actual mutate path, not a mock.
//!
//! #751/P10: the budget cases used to assert through `check_budget` /
//! `check_plan_budget` against a `ConvergePlan`. Both are deleted with the pane
//! machine, so the operator-override cases below assert on the config the gate
//! actually reads — which is the BUG-1 invariant they existed for.

use std::sync::Arc;
use std::time::Duration;

use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SharedClock;
use chiefd_core::store::COMPANY_DB_FILENAME;
use chiefd_core::test_support::ManualClock;

use super::{
    begin_cycle, clear_breaker, end_cycle, read_safety_config, record_cycle_outcome,
    record_refusal, set_actuation_config, ActuationMode, BreakerAction, CycleGate, SkipReason,
};

const EPOCH: i64 = 1_784_116_800_000;

struct Harness {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    clock: Arc<ManualClock>,
    db: CompanyDb,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        let clock = Arc::new(ManualClock::starting_at(0, EPOCH));
        let shared: SharedClock = clock.clone();
        let db = CompanyDb::open("cobalt", &path, shared).expect("open company db");
        Self { _dir: dir, path, clock, db }
    }
}

#[tokio::test]
async fn the_gate_reads_the_durable_actuation_config() {
    let h = Harness::new();
    assert_eq!(
        read_safety_config(&h.db).actuation_mode,
        ActuationMode::Shadow,
        "shadow by default"
    );

    set_actuation_config(&h.db, ActuationMode::Apply, true, false).await.expect("set config");
    let config = read_safety_config(&h.db);
    assert_eq!(config.actuation_mode, ActuationMode::Apply, "apply once an operator opts in");
    assert!(config.sweep_live);
}

#[tokio::test]
async fn only_one_cycle_holds_the_claim_at_a_time_through_the_real_writer() {
    let h = Harness::new();

    assert_eq!(
        begin_cycle(&h.db, Duration::from_secs(5)).await.expect("begin"),
        CycleGate::Proceed,
        "the first cycle takes the claim"
    );
    assert_eq!(
        begin_cycle(&h.db, Duration::from_secs(5)).await.expect("begin"),
        CycleGate::Skipped(SkipReason::AlreadyRunning),
        "single-flight: a second cycle while the claim is held is skipped"
    );

    end_cycle(&h.db).await.expect("end");
    // The floor still applies after release; advance past it, then a new cycle
    // may take the claim.
    h.clock.advance(Duration::from_secs(5));
    assert_eq!(
        begin_cycle(&h.db, Duration::from_secs(5)).await.expect("begin"),
        CycleGate::Proceed,
        "the claim frees on end and the floor has elapsed"
    );
}

#[tokio::test]
async fn a_cycle_start_is_skipped_until_the_floor_elapses() {
    let h = Harness::new();
    assert_eq!(
        begin_cycle(&h.db, Duration::from_secs(5)).await.expect("begin"),
        CycleGate::Proceed
    );
    end_cycle(&h.db).await.expect("end");

    // No time has passed: the floor is not met even though the claim is free.
    assert_eq!(
        begin_cycle(&h.db, Duration::from_secs(5)).await.expect("begin"),
        CycleGate::Skipped(SkipReason::FloorNotElapsed),
    );

    h.clock.advance(Duration::from_secs(5));
    assert_eq!(
        begin_cycle(&h.db, Duration::from_secs(5)).await.expect("begin"),
        CycleGate::Proceed,
        "5s meets the floor",
    );
}

#[tokio::test]
async fn three_failed_cycles_trip_the_breaker_and_a_clear_resumes() {
    let h = Harness::new();
    set_actuation_config(&h.db, ActuationMode::Apply, false, false).await.expect("set config");

    assert_eq!(record_cycle_outcome(&h.db, false).await.expect("outcome"), BreakerAction::Continue);
    assert_eq!(record_cycle_outcome(&h.db, false).await.expect("outcome"), BreakerAction::Continue);
    assert_eq!(
        record_cycle_outcome(&h.db, false).await.expect("outcome"),
        BreakerAction::Tripped,
        "the third consecutive failure trips the breaker",
    );
    assert_eq!(
        read_safety_config(&h.db).actuation_mode,
        ActuationMode::Shadow,
        "a tripped company is dropped to shadow",
    );

    clear_breaker(&h.db).await.expect("clear");
    assert_eq!(
        read_safety_config(&h.db).actuation_mode,
        ActuationMode::Apply,
        "an operator clear resumes the mode they last chose",
    );
}

#[tokio::test]
async fn a_success_resets_the_failure_streak() {
    let h = Harness::new();
    set_actuation_config(&h.db, ActuationMode::Apply, false, false).await.expect("set config");

    record_cycle_outcome(&h.db, false).await.expect("outcome");
    record_cycle_outcome(&h.db, true).await.expect("outcome");
    // Two more failures would trip if the streak had not reset; it did, so the
    // company is still applying.
    record_cycle_outcome(&h.db, false).await.expect("outcome");
    record_cycle_outcome(&h.db, false).await.expect("outcome");
    assert_eq!(
        read_safety_config(&h.db).actuation_mode,
        ActuationMode::Apply,
        "the success reset the streak, so two later failures do not trip",
    );
}

/// The BUG-1 regression (runtime/takeover-bug-log.md, measured live twice
/// 2026-07-22): an externally-seeded `budgetOverride: true` was reverted to
/// `false` within one 30s reconcile pass, because the daemon never re-read the
/// document and the cycle's whole-document mutators rewrote it from stale
/// in-memory state.
///
/// Migrated for the converge-safety blob-death cutover: converge-safety is
/// now REPLACEMENT-wired (rows are the sole authority) and
/// `refresh_safety_doc` correctly no-ops post-cutover -- there is no more
/// out-of-band blob for it to adopt from. The operator's write path is now
/// the supported rows-native `set_actuation_config`, which applies directly
/// through the actor's mutate -> converge_safety -> persist_dispatch chain.
/// The BUG-1 invariant this test protects is unchanged: the operator's
/// override is readable by whoever budgets, and the cycle's own
/// read-modify-write of the safety config must not revert it.
#[tokio::test]
async fn an_externally_seeded_budget_override_is_adopted_honored_and_preserved() {
    let h = Harness::new();
    set_actuation_config(&h.db, ActuationMode::Apply, true, false).await.expect("set config");
    assert!(
        !read_safety_config(&h.db).budget_override_active,
        "no override yet: the budget is in force"
    );

    // The operator flips the override through the supported rows-native
    // write path -- post-cutover this is the ONLY writer of converge-safety;
    // out-of-band blob adoption via `refresh_safety_doc` was retired with it.
    set_actuation_config(&h.db, ActuationMode::Apply, true, true).await.expect("set override");
    assert!(read_safety_config(&h.db).budget_override_active, "the write is honored immediately");

    // The cycle's own whole-document mutators must now PRESERVE the
    // operator-owned fields -- they read-modify-write from the adopted base.
    assert_eq!(
        begin_cycle(&h.db, Duration::from_secs(5)).await.expect("begin"),
        CycleGate::Proceed
    );
    record_refusal(&h.db, "converge_budget", "1 destructive action vs limit 0".to_owned())
        .await
        .expect("record refusal");
    end_cycle(&h.db).await.expect("end");
    let config = read_safety_config(&h.db);
    assert_eq!(config.actuation_mode, ActuationMode::Apply, "mode survived the cycle's writes");
    assert!(config.sweep_live, "sweep flag survived the cycle's writes");
    assert!(config.budget_override_active, "the override survived the cycle's writes");

    // And it is durable, not just in memory: reopening the CompanyDb (the
    // next process, or a restart) still reads the operator's flag straight
    // from the converge-safety ROWS -- rows-authoritative durability, not the
    // now-permanently-empty documents blob.
    let reopened = CompanyDb::open("cobalt", &h.path, h.clock.clone()).expect("reopen");
    let reopened_config = read_safety_config(&reopened);
    assert!(reopened_config.budget_override_active, "durable across a reopen, via rows");
    assert_eq!(
        reopened_config.actuation_mode,
        ActuationMode::Apply,
        "mode durable across a reopen"
    );
}

/// The lever is not one-way sticky: an operator turning the override back
/// OFF via the same supported write path is honored just the same, and the
/// budget gate refuses again -- the lever works in both directions.
///
/// Migrated for the converge-safety blob-death cutover: the OFF-direction
/// write now goes through `set_actuation_config` (rows-native) instead of an
/// out-of-band blob edit + `refresh_safety_doc` adoption, which is retired
/// post-cutover. The BUG-1 invariant -- the override reacting correctly in both
/// directions -- is unchanged.
#[tokio::test]
async fn an_external_override_clear_is_adopted_too() {
    let h = Harness::new();
    set_actuation_config(&h.db, ActuationMode::Apply, true, true).await.expect("set config");
    assert!(read_safety_config(&h.db).budget_override_active, "override on");

    set_actuation_config(&h.db, ActuationMode::Apply, true, false).await.expect("clear override");
    assert!(
        !read_safety_config(&h.db).budget_override_active,
        "the cleared override is honored: the budget is in force again"
    );
}
