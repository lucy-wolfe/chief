//! Effect delivery lifecycle — duty #7's core.
//!
//! Port of the ledger half of `dispatchPendingSupervision`
//! (`src/organization/org-supervision.ts`) and its helpers
//! `applyEffectDelivered`, `recordEffectDeliveryFailure`,
//! `markEffectsDelivered`, and `reopenFailedSupervisionEffects`.
//!
//! Before this module chiefd modelled `EffectStatus::Delivered` but **no
//! production code ever set it** — effects were enqueued and could only be
//! superseded; nothing marked delivery, recorded a failure, or ran the
//! breaker. That is the od-supervisor gap table's row #7, and it is
//! the exact shape of the nineteen-hour blackout: effects pile up `pending`
//! while every liveness signal looks healthy.
//!
//! # What "delivered" means
//!
//! Transport semantics survive verbatim (`org-supervision-transport.ts`):
//! **"delivered" is a durable mailbox publication — not delivery to the
//! person.** The wake that follows is best-effort *after* durable staging; a
//! failed wake never turns a delivered effect back to pending (that recovery is
//! duty #8's job). This is why marking delivered is a pure ledger transition
//! here: the host's durable publish is what precedes it.
//!
//! # The pass order (host loop, second pass)
//!
//! 1. [`dispatch_plan`] — a PURE selection of pending effects, split into a
//!    routine batch and an urgent batch (`manager_goal_stalled`) that stays its
//!    own reconcile boundary for immediate manager wake.
//! 2. The host publishes each, then [`mark_delivered`] on success (one commit,
//!    with the two type-specific armings) or [`record_delivery_failure`]
//!    on failure (a 3-attempt breaker with **no half-open state**).
//!
//! Every function that mutates runs inside one [`super::mutate`] commit, so a
//! refusal publishes nothing.

use crate::ledger::Ledgers;
use crate::store::organization::OrganizationManifest;
use crate::ChiefdError;

use super::{mutate, EffectStatus, SupervisionDraft, SupervisionLedger};

/// A dispatch attempt trips the breaker at this many failures. A correct
/// breaker with **no half-open state**: once `failed`, an effect never
/// re-closes on a timer — only an explicit operator reopen re-drives it. This is
/// the port of `SUPERVISION_EFFECT_DELIVERY_ATTEMPT_LIMIT`; re-driving a poison
/// effect forever is the failure it prevents.
pub const SUPERVISION_EFFECT_DELIVERY_ATTEMPT_LIMIT: u32 = 3;

/// A single operator recovery may re-drive one failed effect this many times
/// before it is left at rest for good. Port of `SUPERVISION_EFFECT_REOPEN_LIMIT`.
pub const SUPERVISION_EFFECT_REOPEN_LIMIT: u32 = 3;

/// Effect kinds whose dispatch is an urgent escalation and forms its own
/// reconcile boundary (immediate manager wake), never coalesced with routine
/// goal-watch traffic.
const URGENT_KINDS: [&str; 1] = ["manager_goal_stalled"];

/// What one pure dispatch pass would carry, in dispatch order. Selection only —
/// no mutation, no I/O — so it is safe to compute off the writer thread from a
/// snapshot. Bounded: ids and counts, never the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DispatchPlan {
    /// Pending envelopes cleared to dispatch, routine batch.
    pub routine: Vec<String>,
    /// Pending envelopes cleared to dispatch, urgent batch (its own boundary).
    pub urgent: Vec<String>,
}

/// Compute the pure dispatch plan for a hydrated ledger snapshot.
#[must_use]
pub fn dispatch_plan(ledger: &SupervisionLedger) -> DispatchPlan {
    let mut plan = DispatchPlan::default();
    for id in ledger.effect_order() {
        let Some(effect) = ledger.effect(id) else {
            continue;
        };
        if effect.status != EffectStatus::Pending {
            continue;
        }
        if URGENT_KINDS.contains(&effect.kind.as_str()) {
            plan.urgent.push(id.clone());
        } else {
            plan.routine.push(id.clone());
        }
    }
    plan
}

/// Mark a batch of effects delivered in one commit, applying each kind's
/// type-specific arming. Port of `markEffectsDelivered` / `applyEffectDelivered`.
/// Returns the ids that transitioned (a non-`pending` effect is skipped, so a
/// replay is idempotent).
///
/// # Errors
/// Whatever the store refuses on commit; a refusal delivers nothing.
pub fn mark_delivered(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    effect_ids: &[String],
) -> Result<Vec<String>, ChiefdError> {
    mutate(ledgers, manifest, |draft, at| {
        let mut delivered = Vec::new();
        for id in effect_ids {
            if apply_effect_delivered(draft, id, at) {
                delivered.push(id.clone());
            }
        }
        Ok(delivered)
    })
}

fn apply_effect_delivered(draft: &mut SupervisionDraft<'_>, id: &str, at: &str) -> bool {
    let Some(effect) = draft.ledger().effect(id).cloned() else {
        return false;
    };
    if effect.status != EffectStatus::Pending {
        return false;
    }
    if let Some(record) = draft.ledger.effects.get_mut(id) {
        record.status = EffectStatus::Delivered;
        record.delivered_at = Some(at.to_string());
    }
    draft.touch_effect(id);
    true
}

/// Record one dispatch failure and run the breaker. At
/// [`SUPERVISION_EFFECT_DELIVERY_ATTEMPT_LIMIT`] the effect trips to `failed`
/// and stays there — no half-open state. Port of `recordEffectDeliveryFailure`.
/// Returns the effect's resulting status. A non-`pending` effect is unchanged.
///
/// # Errors
/// A `Refused{code:"unknown-effect"}` if the id does not exist; whatever the
/// store refuses on commit otherwise.
pub fn record_delivery_failure(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    effect_id: &str,
) -> Result<EffectStatus, ChiefdError> {
    mutate(ledgers, manifest, |draft, at| {
        let Some(current) = draft.ledger().effect(effect_id).map(|effect| effect.status) else {
            return Err(ChiefdError::refused(
                "unknown-effect",
                format!("Supervision effect '{effect_id}' does not exist"),
            ));
        };
        if current != EffectStatus::Pending {
            return Ok(current);
        }
        let status = match draft.ledger.effects.get_mut(effect_id) {
            // Unreachable given the `pending` check above; fail-safe rather than
            // panic, since `expect` on the writer thread is a crate-banned foot-gun.
            None => return Ok(current),
            Some(effect) => {
                let count = effect.delivery_failure_count.unwrap_or(0).saturating_add(1);
                effect.delivery_failure_count = Some(count);
                effect.last_delivery_failure_at = Some(at.to_string());
                if count >= SUPERVISION_EFFECT_DELIVERY_ATTEMPT_LIMIT {
                    effect.status = EffectStatus::Failed;
                    effect.failed_at = Some(at.to_string());
                    EffectStatus::Failed
                } else {
                    EffectStatus::Pending
                }
            }
        };
        draft.touch_effect(effect_id);
        Ok(status)
    })
}

/// Re-close the breaker for failed effects — the ONLY thing that re-drives a
/// `failed` effect, and it is bounded by [`SUPERVISION_EFFECT_REOPEN_LIMIT`].
/// Port of `reopenFailedSupervisionEffects`. Deliberately an operator action:
/// it is never called from a restart or resume path, because re-driving a
/// genuinely poison effect on a timer forever is the failure the breaker exists
/// to prevent. Returns the reopened ids.
///
/// # Errors
/// Whatever the store refuses on commit; a refusal reopens nothing.
pub fn reopen_failed_effects(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
) -> Result<Vec<String>, ChiefdError> {
    mutate(ledgers, manifest, |draft, at| {
        let candidates: Vec<String> = draft
            .ledger()
            .effect_order()
            .iter()
            .filter(|id| {
                draft.ledger().effect(id).is_some_and(|effect| {
                    effect.status == EffectStatus::Failed
                        && effect.reopen_count.unwrap_or(0) < SUPERVISION_EFFECT_REOPEN_LIMIT
                })
            })
            .cloned()
            .collect();
        let mut reopened = Vec::new();
        for id in candidates {
            if let Some(effect) = draft.ledger.effects.get_mut(&id) {
                effect.status = EffectStatus::Pending;
                effect.reopen_count = Some(effect.reopen_count.unwrap_or(0).saturating_add(1));
                effect.last_reopened_at = Some(at.to_string());
                effect.delivery_failure_count = Some(0);
                effect.failed_at = None;
                effect.last_delivery_failure_at = None;
            }
            draft.touch_effect(&id);
            reopened.push(id);
        }
        Ok(reopened)
    })
}

#[cfg(test)]
mod tests;
