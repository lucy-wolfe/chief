//! Converge-safety scaffold tests: the budget threshold math (including the
//! `max(2, 25%)` edge cases), the breaker's 3-strike-then-recover behaviour, the
//! shadow/apply read/write round-trip, the single-flight claim + floor spacing,
//! and the `FailSafeValue` polarity edges (absent is the shadow default; corrupt
//! is restrictive).
//!
//! #751/P10 removed the plan-trimming cases along with the functions they
//! pinned. The behaviour they asserted — an over-budget pass drains up to the
//! limit instead of refusing (#369), a start backlog drains incrementally
//! (#431), a stop is never throttled — is now asserted against the surviving
//! mechanism in `runtime::actuation::tests`, which is where the budgets are
//! enforced. What is left here is the arithmetic itself.

use super::{
    begin_cycle, clear, end_cycle, operator_clear_breaker, read, record_cycle_outcome,
    record_refusal, set_actuation_config, ActuationMode, BreakerAction, ConvergeSafetyState,
    ConvergeSafetyStore, CycleGate, SkipReason, BREAKER_TRIP_THRESHOLD, CLAIM_STALE_MS,
};
use crate::clock::WallMillis;
use crate::ledger::Ledgers;
use crate::polarity::{FailSafeValue, StoreKind};

const EPOCH: i64 = 1_784_116_800_000; // 2026-07-15T12:00:00.000Z

fn ledgers_at(now_ms: i64) -> Ledgers {
    Ledgers::empty(WallMillis(now_ms))
}

fn ledgers() -> Ledgers {
    ledgers_at(EPOCH)
}

// --- TOMBSTONE: 1. the destructive-action budget math ----------------------
//
// `the_budget_floor_is_two_even_for_a_tiny_or_empty_company` and
// `the_budget_is_twenty_five_percent_once_that_beats_the_floor` are DELETED
// with `destructive_budget` and `MIN_DESTRUCTIVE_BUDGET` themselves.
//
// The budget capped how many panes one pass was allowed to destroy, and it
// capped them because chiefd was ISSUING the kills. chiefd issues no kills:
// it publishes a desired set, and absence from that set is the whole
// instruction. There is no count of destructive actions for a budget to bound,
// so these tests are not unasserted -- their subject does not exist.
//
// What the budget was actually protecting is not lost. A pass that would tear
// down a whole company still cannot happen by accident, because the desired set
// is derived from the manifest and the activity ledger rather than from a
// diff against something that might have failed to load -- and the two
// operator safety valves that DO survive, the circuit breaker and shadow mode,
// are both tested below.

// --- 2. the 3-strike circuit breaker --------------------------------------

#[test]
fn two_failures_then_a_success_never_trips_and_resets_the_count() {
    let mut l = ledgers();
    set_actuation_config(&mut l, ActuationMode::Apply, false, false);

    assert_eq!(record_cycle_outcome(&mut l, false), BreakerAction::Continue);
    assert_eq!(record_cycle_outcome(&mut l, false), BreakerAction::Continue);
    let state = read(&l).into_parts().0;
    assert_eq!(state.consecutive_failures, 2, "two is not three");
    assert!(!state.breaker_tripped);
    assert_eq!(state.effective_config().actuation_mode, ActuationMode::Apply);

    assert_eq!(record_cycle_outcome(&mut l, true), BreakerAction::Continue);
    assert_eq!(read(&l).into_parts().0.consecutive_failures, 0, "any success resets the streak");
}

#[test]
fn the_third_consecutive_failure_trips_the_breaker_back_to_shadow() {
    let mut l = ledgers();
    set_actuation_config(&mut l, ActuationMode::Apply, false, false);

    assert_eq!(record_cycle_outcome(&mut l, false), BreakerAction::Continue);
    assert_eq!(record_cycle_outcome(&mut l, false), BreakerAction::Continue);
    assert_eq!(
        record_cycle_outcome(&mut l, false),
        BreakerAction::Tripped,
        "the third consecutive failure is the escalation signal"
    );

    let state = read(&l).into_parts().0;
    assert_eq!(state.effective_config().actuation_mode, ActuationMode::Shadow, "dropped to shadow");
    assert!(state.breaker_tripped);
    assert!(state.breaker_tripped_at.is_some(), "the trip is stamped for audit");
    assert!(
        state.last_refusal.is_some_and(|r| r.kind == "circuit-breaker"),
        "the trip is recorded for escalation"
    );
    assert_eq!(BREAKER_TRIP_THRESHOLD, 3);
}

#[test]
fn a_tripped_breaker_resumes_only_via_an_explicit_clear() {
    let mut l = ledgers();
    set_actuation_config(&mut l, ActuationMode::Apply, false, false);
    for _ in 0..BREAKER_TRIP_THRESHOLD {
        record_cycle_outcome(&mut l, false);
    }
    assert_eq!(read(&l).into_parts().0.effective_config().actuation_mode, ActuationMode::Shadow);

    let cleared = operator_clear_breaker(&mut l);
    assert!(!cleared.breaker_tripped);
    assert_eq!(cleared.consecutive_failures, 0);
    assert_eq!(
        cleared.effective_config().actuation_mode,
        ActuationMode::Apply,
        "an explicit clear resumes the mode the operator last chose"
    );
}

#[test]
fn a_config_change_also_resumes_a_tripped_breaker() {
    let mut l = ledgers();
    set_actuation_config(&mut l, ActuationMode::Apply, false, false);
    for _ in 0..BREAKER_TRIP_THRESHOLD {
        record_cycle_outcome(&mut l, false);
    }
    assert!(read(&l).into_parts().0.breaker_tripped);

    let after = set_actuation_config(&mut l, ActuationMode::Apply, false, false);
    assert!(!after.breaker_tripped, "a config change clears the trip");
    assert_eq!(after.effective_config().actuation_mode, ActuationMode::Apply);
}

// --- 3. the shadow/apply mode round-trip ----------------------------------

#[test]
fn a_fresh_company_defaults_to_shadow_without_a_warning() {
    let (state, warning) = read(&ledgers()).into_parts();
    assert_eq!(state, ConvergeSafetyState::default_shadow());
    assert!(warning.is_none(), "the ordinary default is not a recovery");

    let config = state.effective_config();
    assert_eq!(config.actuation_mode, ActuationMode::Shadow, "compute the plan, actuate nothing");
    assert!(!config.sweep_live);
    assert!(!config.budget_override_active);
}

#[test]
fn mode_and_sub_flags_survive_a_round_trip_through_the_ledger() {
    let mut l = ledgers();
    let written = set_actuation_config(&mut l, ActuationMode::Apply, true, true);
    assert_eq!(written.actuation_mode, ActuationMode::Apply);

    let (reread, warning) = read(&l).into_parts();
    assert!(warning.is_none(), "a row chiefd just wrote parses");
    let config = reread.effective_config();
    assert_eq!(config.actuation_mode, ActuationMode::Apply);
    assert!(config.sweep_live);
    assert!(config.budget_override_active);
}

#[test]
fn the_sweep_flag_can_apply_live_while_the_rest_stays_in_shadow() {
    let mut l = ledgers();
    set_actuation_config(&mut l, ActuationMode::Shadow, true, false);
    let config = read(&l).into_parts().0.effective_config();
    assert_eq!(config.actuation_mode, ActuationMode::Shadow, "the rest of actuation stays shadow");
    assert!(config.sweep_live, "the fence-proven sweep may still apply live");
}

#[test]
fn a_tripped_breaker_forces_full_shadow_including_the_sweep() {
    let mut l = ledgers();
    set_actuation_config(&mut l, ActuationMode::Apply, true, false);
    for _ in 0..BREAKER_TRIP_THRESHOLD {
        record_cycle_outcome(&mut l, false);
    }
    let config = read(&l).into_parts().0.effective_config();
    assert_eq!(config.actuation_mode, ActuationMode::Shadow);
    assert!(!config.sweep_live, "a company in trouble goes conservative everywhere");
}

#[test]
fn clearing_resets_to_the_shadow_default() {
    let mut l = ledgers();
    set_actuation_config(&mut l, ActuationMode::Apply, true, true);
    assert!(clear(&mut l), "a present row is cleared");
    assert!(!clear(&mut l), "clearing is idempotent");
    assert_eq!(
        read(&l).into_parts().0.effective_config().actuation_mode,
        ActuationMode::Shadow,
        "back to shadow"
    );
}

// --- 4. single-flight claim + floor interval ------------------------------

#[test]
fn a_first_cycle_proceeds_and_a_concurrent_second_is_skipped_as_already_running() {
    let mut l = ledgers();
    assert_eq!(begin_cycle(&mut l, 5_000), CycleGate::Proceed, "the first cycle takes the claim");
    assert_eq!(
        begin_cycle(&mut l, 5_000),
        CycleGate::Skipped(SkipReason::AlreadyRunning),
        "a second cycle while the claim is held is single-flighted out"
    );
}

#[test]
fn a_new_cycle_is_skipped_until_the_floor_interval_elapses_after_the_previous_end() {
    // Cycle one starts at EPOCH and ends.
    let mut l = ledgers_at(EPOCH);
    assert_eq!(begin_cycle(&mut l, 5_000), CycleGate::Proceed);
    end_cycle(&mut l);

    // 4.999s later the claim is free but the floor is not met.
    l.set_now_for_test(WallMillis(EPOCH + 4_999));
    assert_eq!(
        begin_cycle(&mut l, 5_000),
        CycleGate::Skipped(SkipReason::FloorNotElapsed),
        "too soon after the last start"
    );

    // At the floor it proceeds.
    l.set_now_for_test(WallMillis(EPOCH + 5_000));
    assert_eq!(begin_cycle(&mut l, 5_000), CycleGate::Proceed, "5s meets the floor");
}

#[test]
fn a_stale_claim_from_a_crashed_cycle_is_reclaimed() {
    let mut l = ledgers_at(EPOCH);
    assert_eq!(begin_cycle(&mut l, 5_000), CycleGate::Proceed);
    // The cycle never called end_cycle (it crashed). Far past the stale bound,
    // the claim is crash residue and a new cycle may reclaim it.
    l.set_now_for_test(WallMillis(EPOCH + CLAIM_STALE_MS + 1));
    assert_eq!(
        begin_cycle(&mut l, 5_000),
        CycleGate::Proceed,
        "a claim older than the stale bound must not wedge the company forever"
    );
}

// --- polarity edges: corrupt is restrictive -------------------------------

#[test]
fn a_present_but_unreadable_row_reads_as_the_restrictive_value_and_warns() {
    let mut l = ledgers();
    l.put_document(ConvergeSafetyStore::NAME, r#"{"schemaVersion":"nope"}"#);
    let (state, warning) = read(&l).into_parts();
    assert_eq!(state, ConvergeSafetyStore::restrictive());
    assert!(warning.is_some_and(|w| w.contains("restrictive")));
    assert_eq!(
        state.effective_config().actuation_mode,
        ActuationMode::Shadow,
        "corrupt bytes never actuate"
    );
}

#[test]
fn a_write_over_corrupt_bytes_cannot_inherit_an_unreadable_breaker_into_apply() {
    let mut l = ledgers();
    l.put_document(ConvergeSafetyStore::NAME, r#"{"schemaVersion":"nope"}"#);
    // A failure recorded over corrupt bytes reads the restrictive (tripped)
    // value first, so the company stays tripped rather than being reset.
    record_cycle_outcome(&mut l, false);
    let state = read(&l).into_parts().0;
    assert!(state.breaker_tripped, "corrupt bytes contribute a tripped breaker, never a clean one");
    assert_eq!(state.effective_config().actuation_mode, ActuationMode::Shadow);
}

#[test]
fn a_recorded_refusal_round_trips() {
    let mut l = ledgers();
    let state = record_refusal(&mut l, "destructive-budget", "would kill+respawn 40 of 20");
    let recorded = state.last_refusal.expect("present");
    assert_eq!(recorded.kind, "destructive-budget");
    assert!(recorded.detail.contains("40"));

    let reread = read(&l).into_parts().0;
    assert_eq!(reread.last_refusal.map(|r| r.kind), Some("destructive-budget".to_string()));
}
