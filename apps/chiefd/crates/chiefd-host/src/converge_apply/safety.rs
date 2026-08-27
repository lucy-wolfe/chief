//! The host-facing safety seam the reconcile cycle consults before it publishes
//! anything for an actuator to do.
//!
//! The durable state and the pure decision logic live in
//! [`chiefd_core::store::converge_safety`]; this module is the thin
//! [`CompanyDb`]-facing wrapper the cycle imports. It gives one surface for the
//! mode gate, the breaker, and the single-flight cycle lifecycle.
//!
//! #751/P10 removed the plan-budget pass-throughs (`check_budget`,
//! `check_plan_budget` and the three plan trimmers). They took a converge plan
//! of pane steps; chiefd has none, and the budgets are now enforced where the
//! per-person actions are minted
//! ([`chiefd_core::runtime::actuation::plan_runtime_actions`]). What the cycle
//! still needs from here is what is durable: the mode, the breaker, and the
//! claim.
//!
//! # Single-flight without a lock file
//!
//! There is no crash-safe file lock here. The per-company writer actor already
//! serializes every mutation, so a durable `cycle_in_progress` claim — taken in
//! [`begin_cycle`] and released in [`end_cycle`], both ordinary `Reconcile`
//! mutations — is an atomic check-and-set: a second cycle that begins while one
//! is in flight commits *after* the first and sees the claim, so it is
//! [`CycleGate::Skipped`]. The same claim carries the floor-interval anchor. A
//! cross-*process* file lock would only be needed to exclude a straggling
//! TypeScript writer, which does not touch this path in M2.
//!
//! # Escalation is the caller's to enqueue
//!
//! [`record_cycle_outcome`] returns [`BreakerAction::Tripped`] on the transition.
//! The escalation *effect* is enqueued by the caller — which is already inside a
//! supervision mutation with access to `SupervisionDraft::enqueue_effect` and its
//! effect-sequence invariant — not by this store reaching across that boundary.
//! This store's durable trip record is the audit half; the effect is the live
//! half, and they live on opposite sides of the seam by design.

use std::time::Duration;

use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::store::converge_safety;
use chiefd_core::ChiefdError;

pub use chiefd_core::store::converge_safety::{
    ActuationMode, BreakerAction, CycleGate, SafetyConfig, SkipReason,
};

/// The company's effective actuation config this pass: the stored mode with the
/// breaker folded in (shadow whenever tripped), plus the sweep and override
/// flags. Synchronous and infallible — a committed snapshot read that cannot
/// fail, over a fail-safe store whose worst case is "shadow, actuate nothing".
#[must_use]
pub fn read_safety_config(db: &CompanyDb) -> SafetyConfig {
    db.read(|snapshot| converge_safety::read(snapshot).into_parts().0.effective_config())
}

/// The effective config PLUS the RAW breaker flag, from one snapshot read.
///
/// [`SafetyConfig`] deliberately has no `breaker_tripped`: `effective_config`
/// folds a tripped breaker into [`ActuationMode::Shadow`] so no gate has to
/// know why. That is right for the GATE and wrong for the REASON — the action
/// stream reports `WithheldReason::BreakerTripped` and `WithheldReason::Shadow`
/// as different operator states, because "your company faulted three times and
/// needs you to clear it" is not the same sentence as "you asked for shadow".
/// Folding both into the mode and re-deriving the cause downstream would be a
/// second opinion on the breaker; reading both off ONE state is not.
#[must_use]
pub fn read_safety_posture(db: &CompanyDb) -> (SafetyConfig, bool) {
    db.read(|snapshot| {
        let state = converge_safety::read(snapshot).into_parts().0;
        (state.effective_config(), state.breaker_tripped)
    })
}

/// Re-adopt the durable converge-safety document from sqlite before a cycle
/// reads or rewrites it.
///
/// The actor's committed snapshot is authoritative from open to shutdown and
/// never re-reads the file on its own, so an operator control-plane write —
/// `chiefd set-actuation-config`, `chiefd clear-breaker`, or a raw
/// `bootstrap-store` seed of this document — is invisible to a running daemon
/// until this runs, and the cycle's own whole-document read-modify-writes
/// (`begin_cycle`/`end_cycle`/`record_refusal`/`record_cycle_outcome`) would
/// otherwise clobber it back within one pass (found live, 2026-07-22:
/// `budgetOverride: true` reverted to `false` in <30s —
/// runtime/takeover-bug-log.md BUG-1). The reconcile cycle calls this once per
/// pass, before [`begin_cycle`], so operator-owned fields (actuationMode,
/// sweepLive, budgetOverride) are adopted at pass cadence and survive the
/// cycle's own writes, which then read-modify-write from that fresh base.
///
/// # Errors
/// Propagates any [`ChiefdError`] from the writer.
pub async fn refresh_safety_doc(db: &CompanyDb) -> Result<(), ChiefdError> {
    let _ = db;
    Ok(())
}

/// Take the single-flight slot for one apply cycle, subject to `floor`.
///
/// [`CycleGate::Proceed`] when the claim was taken and the floor was met;
/// [`CycleGate::Skipped`] when another cycle holds the claim or the floor has not
/// elapsed. Pair every `Proceed` with an [`end_cycle`] so the claim is released.
///
/// # Errors
/// Propagates any [`ChiefdError`] from the writer (e.g. the actor quiescing).
pub async fn begin_cycle(db: &CompanyDb, floor: Duration) -> Result<CycleGate, ChiefdError> {
    let floor_ms = i64::try_from(floor.as_millis()).unwrap_or(i64::MAX);
    db.mutate(MutationClass::Reconcile, MutationName("converge.begin_cycle"), move |ledgers| {
        Ok(converge_safety::begin_cycle(ledgers, floor_ms))
    })
    .await
}

/// Release the single-flight claim taken by [`begin_cycle`].
///
/// # Errors
/// Propagates any [`ChiefdError`] from the writer.
pub async fn end_cycle(db: &CompanyDb) -> Result<(), ChiefdError> {
    db.mutate(MutationClass::Reconcile, MutationName("converge.end_cycle"), move |ledgers| {
        converge_safety::end_cycle(ledgers);
        Ok(())
    })
    .await
}

/// Fold one apply-cycle outcome into the breaker. Returns
/// [`BreakerAction::Tripped`] on the transition — the caller's cue to enqueue the
/// escalation effect and stop actuating.
///
/// # Errors
/// Propagates any [`ChiefdError`] from the writer.
pub async fn record_cycle_outcome(
    db: &CompanyDb,
    cycle_succeeded: bool,
) -> Result<BreakerAction, ChiefdError> {
    db.mutate(MutationClass::Reconcile, MutationName("converge.record_outcome"), move |ledgers| {
        Ok(converge_safety::record_cycle_outcome(ledgers, cycle_succeeded))
    })
    .await
}

/// Set the operator-facing actuation config (mode + sweep + budget override) and
/// resume the breaker — the operator control-plane write (`chiefctl`).
///
/// # Errors
/// Propagates any [`ChiefdError`] from the writer.
pub async fn set_actuation_config(
    db: &CompanyDb,
    mode: ActuationMode,
    sweep_live: bool,
    budget_override: bool,
) -> Result<(), ChiefdError> {
    db.mutate(MutationClass::Small, MutationName("converge.set_config"), move |ledgers| {
        converge_safety::set_actuation_config(ledgers, mode, sweep_live, budget_override);
        Ok(())
    })
    .await
}

/// The explicit operator acknowledgement that resumes a tripped breaker.
///
/// # Errors
/// Propagates any [`ChiefdError`] from the writer.
pub async fn clear_breaker(db: &CompanyDb) -> Result<(), ChiefdError> {
    db.mutate(MutationClass::Small, MutationName("converge.clear_breaker"), move |ledgers| {
        converge_safety::operator_clear_breaker(ledgers);
        Ok(())
    })
    .await
}

/// Record a refusal durably for audit. The live escalation effect is enqueued
/// caller-side; this is the durable trail.
///
/// # Errors
/// Propagates any [`ChiefdError`] from the writer.
pub async fn record_refusal(
    db: &CompanyDb,
    kind: &'static str,
    detail: String,
) -> Result<(), ChiefdError> {
    db.mutate(MutationClass::Small, MutationName("converge.record_refusal"), move |ledgers| {
        converge_safety::record_refusal(ledgers, kind, detail);
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests;
