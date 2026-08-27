//! Unit D / D4 — safety-gate behaviour where it meets the assembled cycle.
//!
//! Unit C's own `store::converge_safety::tests` already prove the breaker
//! arithmetic in isolation. This file proves the *decisions* that only matter
//! once the breaker, the budget, and the actuation loop are wired together:
//!
//!   D4.2  a success between failures resets the counter (no spurious trip).
//!   D4.3  a budget refusal is NOT a breaker strike — the settled decision. A
//!         pass bounded by a budget records a refusal and releases its claim but
//!         must NEVER call `record_cycle_outcome(false)`, so a company that keeps
//!         hitting its budget never trips the breaker on that basis alone.
//!   D4.5  an operator clear / config change resumes apply.
//!
//! #751/P8-P10 removed D4.1/D4.4. Both drove a whole `reconcile_cycle` against
//! a scripted runtime server through chiefd's own host executor, and asserted
//! that a tripped breaker suppressed the ACTUATION. chiefd actuates nothing, so
//! the question those cases asked is now "does a tripped breaker withhold the
//! action stream", and that is asserted directly against the planner in
//! `chiefd_core::runtime::actuation::tests`
//! (`a_tripped_breaker_withholds_and_reports_the_breaker_not_the_mode`), which
//! needs no runtime server at all.
//!
//! The pure cases are REAL against `converge_safety` over `Ledgers::empty` — the
//! same in-memory ledger the store's own tests use.
//!
//! ACTIVATION: move to `crates/chiefd-host/tests/safety_gate_integration.rs`
//! once m2-safety (`store::converge_safety`) is on the synced main. (Source
//! already tracked on origin/main; no gitignore blocker.)

use chiefd_core::clock::WallMillis;
use chiefd_core::ledger::Ledgers;
use chiefd_core::store::converge_safety::{
    self as safety, ActuationMode, BreakerAction, BREAKER_TRIP_THRESHOLD,
};

fn ledgers() -> Ledgers {
    Ledgers::empty(WallMillis(1_700_000_000_000))
}

// --- D4.2: success resets the consecutive-failure counter --------------------

#[test]
fn d4_2_a_success_between_failures_prevents_a_trip() {
    let mut l = ledgers();
    // fail, fail, success, fail — never three in a row, so never trips.
    assert_eq!(safety::record_cycle_outcome(&mut l, false), BreakerAction::Continue);
    assert_eq!(safety::record_cycle_outcome(&mut l, false), BreakerAction::Continue);
    assert_eq!(safety::record_cycle_outcome(&mut l, true), BreakerAction::Continue);
    assert_eq!(safety::record_cycle_outcome(&mut l, false), BreakerAction::Continue);
    assert!(
        !safety::read(&l).into_parts().0.breaker_tripped,
        "an interleaved success must reset the counter and prevent a trip",
    );
}

#[test]
fn d4_2b_three_consecutive_failures_trip_exactly_once() {
    let mut l = ledgers();
    for _ in 0..(BREAKER_TRIP_THRESHOLD - 1) {
        assert_eq!(safety::record_cycle_outcome(&mut l, false), BreakerAction::Continue);
    }
    // The Nth consecutive failure trips — and returns `Tripped` exactly once.
    assert_eq!(safety::record_cycle_outcome(&mut l, false), BreakerAction::Tripped);
    // A further failure does NOT re-fire `Tripped` (escalation happens once).
    assert_eq!(safety::record_cycle_outcome(&mut l, false), BreakerAction::Continue);
    let state = safety::read(&l).into_parts().0;
    assert!(state.breaker_tripped);
    assert_eq!(
        state.effective_config().actuation_mode,
        ActuationMode::Shadow,
        "a tripped breaker folds the effective mode to shadow",
    );
}

// --- D4.3: THE settled decision — a budget refusal is not a breaker strike ----
//
// #751/P10: the budget is no longer evaluated over a converge plan, so the
// precondition that built an over-budget plan is gone. The DECISION this case
// exists for is untouched and is about the two writers, not about the plan: a
// pass bounded by a budget calls `record_refusal`, never
// `record_cycle_outcome(false)`, so a company that keeps hitting its budget
// never trips the breaker on that basis alone.

#[test]
fn d4_3_a_budget_refusal_is_not_a_breaker_strike() {
    let mut l = ledgers();
    // The assembled cycle's contract on a budget refusal: record the refusal +
    // end the cycle, but DO NOT fold it into the breaker. We simulate that here
    // exactly — `record_refusal` (not `record_cycle_outcome`) is called.
    for _ in 0..10 {
        safety::record_refusal(&mut l, "destructive-budget", "over budget");
        // NOTE: `record_cycle_outcome(false)` is deliberately NOT called.
    }

    let state = safety::read(&l).into_parts().0;
    assert_eq!(
        state.consecutive_failures, 0,
        "ten budget refusals must leave the breaker counter at zero",
    );
    assert!(!state.breaker_tripped, "a budget refusal must never trip the breaker");
    assert!(state.last_refusal.is_some(), "but the refusal is still recorded for audit");
}

// --- D4.5: operator clear / config change resumes apply -----------------------

#[test]
fn d4_5_operator_clear_and_config_change_resume_apply() {
    let mut l = ledgers();
    for _ in 0..BREAKER_TRIP_THRESHOLD {
        safety::record_cycle_outcome(&mut l, false);
    }
    assert!(safety::read(&l).into_parts().0.breaker_tripped, "tripped");

    // Path 1: explicit operator clear.
    safety::operator_clear_breaker(&mut l);
    assert!(!safety::read(&l).into_parts().0.breaker_tripped, "operator clear resumes");

    // Trip again, then Path 2: a config change also resumes and sets the mode.
    for _ in 0..BREAKER_TRIP_THRESHOLD {
        safety::record_cycle_outcome(&mut l, false);
    }
    safety::set_actuation_config(&mut l, ActuationMode::Apply, false, false);
    let cfg = safety::read(&l).into_parts().0.effective_config();
    assert_eq!(cfg.actuation_mode, ActuationMode::Apply, "config change resumes into apply");
}
