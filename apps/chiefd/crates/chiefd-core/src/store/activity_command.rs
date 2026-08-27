//! The protected activity command surface: which transitions a person still
//! has open, and which one is the live lifecycle authority.
//!
//! This module is the whole of that surface. The TypeScript activity-command
//! module it grew out of is deleted, so the comments below name the behaviour
//! rather than the old file.
//!
//! # `prepare` and `release` are not re-implemented here
//!
//! That surface's `prepare` and `reflect` verbs were three-line delegations to
//! `beginGracefulTransition` / `recordReflectionHandoff`, which are already
//! [`crate::store::activity::begin_transition`] and
//! [`crate::store::activity::release`] in Rust. Wrapping them again would be a
//! second way to do one thing. What this module owns is the part that was
//! genuinely only in TypeScript: which transitions a person currently has open
//! ([`pending_transitions`]).
//!
//! # TOMBSTONE (#751-P4): the prompt is gone because the payload is gone
//!
//! Until #751-P4 this module also owned `graceful_reflection_prompt`, a
//! deterministic block of text that asked a pane for five bounded fields
//! (summary/learning/handoff/artifacts/openCommitments) and told it to call
//! `org_reflect`. The whole reflection payload has been deleted from the
//! product: a transition now records only that it was *released*, never what
//! was said. There is nothing left to prompt *for*, so the prompt, its soft-cap
//! guidance, and the `GracefulReflectionRequest` pairing of transition+prompt
//! all went with it. Do not reintroduce a prompt here — a release takes no
//! content, so any prompt would be asking for something the store cannot store.
//!
//! The status verb therefore reports the pending transitions themselves. A
//! caller that wants to tell a pane what to do renders that from the transition
//! (action, deadline), which is the only durable material there is.
//!
//! # The caller identity is injected, never taken from the payload
//!
//! Every function here takes the person id the API layer authenticated from
//! the caller's own credential. There is no parameter through which a Pi
//! payload could name somebody else, which is what stops one person inspecting
//! or releasing another person's transition.

use crate::error::Refusal;
use crate::store::activity::{
    ActivityLedger, GracefulTransition, TransitionStatus, UNKNOWN_PERSON,
};

/// What a `status` verb answers with.
///
/// The wire shape is `{ personId, pendingTransitions, activeTransitionId? }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityCommandStatus {
    /// Whose status.
    pub person_id: String,
    /// Every transition still open for this person, in creation order.
    pub pending_transitions: Vec<GracefulTransition>,
    /// The exact pending lifecycle authority for this person.
    ///
    /// Reported **only** when it is one of `pending_transitions`: a historical
    /// pending record is never interchangeable with the transition the person
    /// is actually being asked to release, and reporting a stale pointer would
    /// invite a release against the wrong transition.
    pub active_transition_id: Option<String>,
}

/// Every transition `person_id` still has open, in creation order.
///
/// "Open" is `AwaitingHandoff | Overdue`: the two statuses from which a release
/// is still the person's to give. `Overdue` is deliberately included — the
/// grace deadline having passed does not retract the request, it only lets the
/// projection force the change if the release never comes, so a pane that wakes
/// up late must still be able to see and release what it owes.
///
/// # Errors
/// [`UNKNOWN_PERSON`] when the ledger has no state for the caller — a person
/// who is not in the activity roster cannot have an open transition, and
/// answering "none" would hide a roster divergence.
pub fn pending_transitions(
    ledger: &ActivityLedger,
    person_id: &str,
) -> Result<Vec<GracefulTransition>, Refusal> {
    if !ledger.people.contains_key(person_id) {
        return Err(Refusal::new(
            UNKNOWN_PERSON,
            format!("Unknown organization person '{person_id}'"),
        ));
    }
    Ok(ledger
        .transition_order
        .iter()
        .filter_map(|id| ledger.transitions.get(id))
        .filter(|transition| {
            transition.person_id == person_id
                && matches!(
                    transition.status,
                    TransitionStatus::AwaitingHandoff | TransitionStatus::Overdue
                )
        })
        .cloned()
        .collect())
}

/// Answer the `status` verb for one authenticated caller.
///
/// # Errors
/// [`UNKNOWN_PERSON`], as [`pending_transitions`]. A blank caller id is refused
/// outright: an unauthenticated activity command has no person whose
/// transitions it could legitimately read.
pub fn activity_command_status(
    ledger: &ActivityLedger,
    caller_person_id: &str,
) -> Result<ActivityCommandStatus, Refusal> {
    let caller = caller_person_id.trim();
    if caller.is_empty() {
        return Err(Refusal::new(
            UNKNOWN_PERSON,
            "Activity command requires an authenticated organization person",
        ));
    }
    let pending = pending_transitions(ledger, caller)?;
    let active_transition_id = ledger
        .people
        .get(caller)
        .and_then(|state| state.active_transition_id.as_deref())
        .filter(|id| pending.iter().any(|transition| transition.id == *id))
        .map(ToString::to_string);
    Ok(ActivityCommandStatus {
        person_id: caller.to_string(),
        pending_transitions: pending,
        active_transition_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::activity::TransitionAction;
    use crate::store::organization::OrganizationManifest;
    use crate::test_support::northstar_manifest;

    fn manifest() -> OrganizationManifest {
        northstar_manifest(1_700_000_000_000)
    }

    fn transition(id: &str, person_id: &str, status: TransitionStatus) -> GracefulTransition {
        GracefulTransition {
            id: id.to_string(),
            person_id: person_id.to_string(),
            action: TransitionAction::Park,
            reason: "idle".to_string(),
            intent_id: None,
            placement_department_id: "quant".to_string(),
            to_department_id: None,
            status,
            requested_at: "2026-08-07T00:00:00.000Z".to_string(),
            handoff_deadline_at: "2026-08-07T00:02:00.000Z".to_string(),
            applied_at: None,
            cancelled_at: None,
            forced_at: None,
            abandoned_at: None,
        }
    }

    fn ledger_with(transitions: Vec<GracefulTransition>) -> ActivityLedger {
        let m = manifest();
        let mut ledger = ActivityLedger::initial(&m, "2026-08-07T00:00:00.000Z");
        for transition in transitions {
            ledger.transition_order.push(transition.id.clone());
            ledger.transitions.insert(transition.id.clone(), transition);
        }
        ledger
    }

    #[test]
    fn only_awaiting_and_overdue_transitions_are_pending() {
        let ledger = ledger_with(vec![
            transition("t1", "signal-researcher", TransitionStatus::AwaitingHandoff),
            transition("t2", "signal-researcher", TransitionStatus::Overdue),
            transition("t3", "signal-researcher", TransitionStatus::Applied),
            transition("t4", "signal-researcher", TransitionStatus::Cancelled),
        ]);
        let pending = pending_transitions(&ledger, "signal-researcher").expect("pending");
        let ids: Vec<&str> = pending.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["t1", "t2"]);
    }

    #[test]
    fn a_released_transition_is_no_longer_pending() {
        // `Ready` means the person already released it; the structural change
        // is now the projection's to apply, not the person's to act on. It must
        // not come back on the next `status` poll, or a well-behaved pane
        // releases the same transition forever.
        let ledger =
            ledger_with(vec![transition("t1", "signal-researcher", TransitionStatus::Ready)]);
        let pending = pending_transitions(&ledger, "signal-researcher").expect("pending");
        assert!(pending.is_empty());
    }

    #[test]
    fn a_pending_transition_carries_its_own_action_and_deadline() {
        // The prompt that used to be paired with each transition is gone
        // (#751-P4). Everything a caller needs to tell a pane what is being
        // asked of it now has to come off the transition itself, so the status
        // answer must carry the whole record and not a digest of it.
        let ledger = ledger_with(vec![transition(
            "transition:1:signal-researcher:park",
            "signal-researcher",
            TransitionStatus::AwaitingHandoff,
        )]);
        let pending = pending_transitions(&ledger, "signal-researcher").expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "transition:1:signal-researcher:park");
        assert_eq!(pending[0].action, TransitionAction::Park);
        assert_eq!(pending[0].handoff_deadline_at, "2026-08-07T00:02:00.000Z");
    }

    /// THE SILENT-FAILURE PIN (#751-P4).
    ///
    /// `queueAutomaticParkCompaction`
    /// (`packages/piing/extensions/organization-intercom.ts`) is now the ONLY
    /// reader of `activity status`, and it decides whether to spend a compact
    /// with exactly this predicate over the serialized wire body:
    ///
    /// ```text
    /// status.pendingTransitions.some((t) =>
    ///   t.action === "park")
    /// ```
    ///
    /// If `action` stops serializing as the bare string `"park"`, that
    /// predicate silently evaluates false
    /// forever: no error, no log, auto-compact-before-park simply never fires
    /// again and panes start parking with a full context window. A struct-level
    /// assertion cannot catch that — only the serialized shape can — so this
    /// pins the JSON the adapter actually puts on the wire.
    #[test]
    fn the_status_wire_body_keeps_the_key_automatic_park_compaction_filters_on() {
        let ledger = ledger_with(vec![transition(
            "transition:1:signal-researcher:park",
            "signal-researcher",
            TransitionStatus::AwaitingHandoff,
        )]);
        let status = activity_command_status(&ledger, "signal-researcher").expect("status");

        // Serialize exactly as `docstore::org_slice`'s status route does.
        let wire: Vec<serde_json::Value> = status
            .pending_transitions
            .iter()
            .map(|transition| serde_json::to_value(transition).expect("transition serializes"))
            .collect();
        assert_eq!(wire.len(), 1);

        assert_eq!(
            wire[0].get("action").and_then(serde_json::Value::as_str),
            Some("park"),
            "`action` must serialize as the bare string the TS predicate compares against"
        );
        // And no reflection payload rides along on the status wire.
        assert!(
            !serde_json::to_string(&wire[0]).expect("body").contains("reflection"),
            "no reflection key may appear on the status wire: {:?}",
            wire[0]
        );
    }

    #[test]
    fn another_persons_transition_is_never_returned() {
        let ledger =
            ledger_with(vec![transition("t1", "quant-head", TransitionStatus::AwaitingHandoff)]);
        let pending = pending_transitions(&ledger, "signal-researcher").expect("pending");
        assert!(pending.is_empty());
    }

    #[test]
    fn a_person_absent_from_the_roster_refuses() {
        let ledger = ledger_with(vec![]);
        let err = pending_transitions(&ledger, "nobody").expect_err("refusal");
        assert_eq!(err.code, UNKNOWN_PERSON);
    }

    #[test]
    fn a_blank_caller_refuses() {
        let ledger = ledger_with(vec![]);
        let err = activity_command_status(&ledger, "   ").expect_err("refusal");
        assert_eq!(err.code, UNKNOWN_PERSON);
    }

    #[test]
    fn the_active_pointer_is_reported_only_when_it_is_actually_pending() {
        let mut ledger = ledger_with(vec![
            transition("t1", "signal-researcher", TransitionStatus::AwaitingHandoff),
            transition("t2", "signal-researcher", TransitionStatus::Applied),
        ]);
        if let Some(state) = ledger.people.get_mut("signal-researcher") {
            state.active_transition_id = Some("t1".to_string());
        }
        let status = activity_command_status(&ledger, "signal-researcher").expect("status");
        assert_eq!(status.active_transition_id.as_deref(), Some("t1"));

        if let Some(state) = ledger.people.get_mut("signal-researcher") {
            state.active_transition_id = Some("t2".to_string());
        }
        let status = activity_command_status(&ledger, "signal-researcher").expect("status");
        assert_eq!(status.active_transition_id, None);
    }
}
