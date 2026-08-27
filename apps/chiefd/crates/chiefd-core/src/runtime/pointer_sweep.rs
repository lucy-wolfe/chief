//! The #29 continuous pointer sweep — pure planner half.
//!
//! A live refinement of `clearOrphanedTerminalTransitions`
//! (`org-activity.ts:629`). That TypeScript function is safe **only at cold
//! start**: it clears a person's `activeTransitionId` whenever the pointed-to
//! transition is terminal (`applied` **or** `cancelled`), because a stopped
//! company has nothing running that could still legitimately consume a retained
//! `applied` handoff. Run naively against a live company it would destroy real
//! pending handoffs — an `applied` intent-bound park is normally kept so a
//! same-intent structural reconcile can still claim it.
//!
//! This function is the **live-safe** rule the design settled on: it clears a
//! pointer iff the pointed-to transition is terminal **and provably
//! unconsumable**, and it leaves a legitimately-claimable `applied` handoff
//! alone. Like M1's planner it is pure data-in / data-out: the durable ledgers
//! are projected into [`SweepInput`], the resulting [`ClearPointerAction`]s are
//! re-verified and applied under the writer lock by the caller.
//!
//! # The rule (`compute_pointer_sweep`)
//!
//! For each person whose `active_transition_id` points at a transition `t`:
//!
//! 1. `t.status == cancelled` → **clear**. A cancelled transition can never be
//!    released, so nothing can ever consume it.
//! 2. `t.status == applied` → **leave alone**. Still legitimately claimable;
//!    clearing it would drop a real pending handoff.
//!
//! An in-flight transition (`awaiting_handoff`, `overdue`, `ready`) is always
//! left alone — it is either still waiting to be released or already released
//! with the structural change still to come. A pointer at a transition that is
//! not present at all is likewise left alone: the enumerated rule only ever
//! clears a terminal transition it can see, and a corrupt dangling pointer is a
//! validation concern, not this sweep's.

use std::collections::BTreeMap;

/// A transition's terminal/in-flight status, mirroring
/// `store::activity::TransitionStatus` without depending on the store module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepStatus {
    /// Waiting for the person to release it from their own pane.
    AwaitingHandoff,
    /// Past the grace deadline, still waiting.
    Overdue,
    /// Released; the structural change may proceed.
    Ready,
    /// The structural change happened.
    Applied,
    /// Superseded or abandoned.
    Cancelled,
    /// Force-parked without ever being released (#337): terminal, but — like an
    /// in-flight status — left alone by the live sweep, mirroring the TS
    /// `isUnconsumableTerminalTransition` (only `cancelled` and applied-diverged
    /// pointers clear; a forced pointer is cleared only by a later request).
    Forced,
}

/// One transition, projected to just the fields the sweep reasons about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepTransition {
    /// Stable transition id (the value a person's pointer holds).
    pub transition_id: String,
    /// The person the transition belongs to.
    pub person_id: String,
    /// Where the transition is in its life.
    pub status: SweepStatus,
}

/// One person, projected to their pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepPerson {
    /// Stable person id.
    pub person_id: String,
    /// The person's `active_transition_id` pointer, if any.
    pub active_transition_id: Option<String>,
}

/// The projected snapshot the sweep reasons over. Built from the durable
/// activity + supervision ledgers by the caller; carries no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepInput {
    /// People in ledger order; only those with a pointer can produce an action.
    pub people: Vec<SweepPerson>,
    /// Every transition, keyed by id.
    pub transitions: BTreeMap<String, SweepTransition>,
}

/// Why a pointer is being cleared. Recorded for the operator/audit log, and it
/// is the re-verify contract the apply step re-checks before clearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearReason {
    /// The transition is cancelled; it can never be released, so nothing can
    /// ever consume it.
    Cancelled,
}

/// One pointer this sweep proposes clearing. The apply step re-verifies each
/// field under the writer lock and drops the action silently on any miss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearPointerAction {
    /// Whose pointer to clear.
    pub person_id: String,
    /// The transition id the pointer must still equal at apply time.
    pub transition_id: String,
    /// Why it is unconsumable.
    pub reason: ClearReason,
}

/// Compute the pointer-clear actions for one company. Pure: no I/O, no locks.
///
/// The result is in `people` order and contains only provably-unconsumable
/// terminal pointers. A claimable `applied` handoff and every in-flight
/// transition are deliberately absent.
#[must_use]
pub fn compute_pointer_sweep(input: &SweepInput) -> Vec<ClearPointerAction> {
    let mut actions = Vec::new();
    for person in &input.people {
        let Some(transition_id) = person.active_transition_id.as_deref() else {
            continue;
        };
        let Some(transition) = input.transitions.get(transition_id) else {
            // A pointer at a transition we cannot see is a validation concern,
            // not this sweep's: the enumerated rule only clears terminal
            // transitions it can prove unconsumable.
            continue;
        };
        let reason = match transition.status {
            SweepStatus::Cancelled => ClearReason::Cancelled,
            // An applied handoff is still claimable, and in-flight statuses are
            // still live work. Both are left alone.
            // A forced park is terminal but unconsumable-by-nothing: it was
            // parked precisely because the release never arrived, and it is
            // never retried, so like the in-flight statuses the live sweep
            // leaves its pointer alone (a later request clears it), mirroring
            // the TS predicate.
            SweepStatus::Applied
            | SweepStatus::AwaitingHandoff
            | SweepStatus::Overdue
            | SweepStatus::Ready
            | SweepStatus::Forced => continue,
        };
        actions.push(ClearPointerAction {
            person_id: person.person_id.clone(),
            transition_id: transition_id.to_string(),
            reason,
        });
    }
    actions
}

/// The live transition state re-read at apply time for one planned clear, so the
/// fenced compare-and-clear can prove the condition that justified the clear
/// still holds before mutating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearRecheck {
    /// The person's `active_transition_id` pointer, re-read under the lock.
    pub active_transition_id: Option<String>,
    /// The pointed-to transition's status, re-read; `None` if it vanished.
    pub status: Option<SweepStatus>,
}

/// Re-verify a planned [`ClearPointerAction`] at apply time (design Q2). Returns
/// `true` iff the pointer should still be cleared.
///
/// This is the compare-and-clear guard the store applies **inside** the same
/// writer lock that all other transition mutations take: between plan time and
/// apply time a person could have been re-pointed or had their transition
/// resolved, and clearing on a stale plan would race a live mutation. Every
/// check that made the pointer provably-unconsumable must still hold:
///
/// * (a) the pointer still equals the planned transition id;
/// * (b) the status still matches the terminal status the plan classified.
///
/// Any miss returns `false` and the caller drops the action silently — the next
/// pass re-observes and re-plans from the current truth.
#[must_use]
pub fn reverify_clear(action: &ClearPointerAction, current: &ClearRecheck) -> bool {
    // (a) the pointer still points where the plan expected.
    if current.active_transition_id.as_deref() != Some(action.transition_id.as_str()) {
        return false;
    }
    match action.reason {
        // A cancelled transition can never be released; only the status must
        // still hold.
        ClearReason::Cancelled => current.status == Some(SweepStatus::Cancelled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transition(id: &str, person: &str, status: SweepStatus) -> SweepTransition {
        SweepTransition { transition_id: id.to_string(), person_id: person.to_string(), status }
    }

    fn person(id: &str, pointer: Option<&str>) -> SweepPerson {
        SweepPerson {
            person_id: id.to_string(),
            active_transition_id: pointer.map(ToString::to_string),
        }
    }

    fn input(people: Vec<SweepPerson>, transitions: Vec<SweepTransition>) -> SweepInput {
        SweepInput {
            people,
            transitions: transitions.into_iter().map(|t| (t.transition_id.clone(), t)).collect(),
        }
    }

    #[test]
    fn an_applied_transition_is_never_cleared() {
        // The critical negative: an applied transition is
        // a real pending handoff. The cold-start rule would wrongly clear it;
        // the live-safe rule must leave it alone.
        let sweep = input(
            vec![person("nadia", Some("t-nadia-1"))],
            vec![transition("t-nadia-1", "nadia", SweepStatus::Applied)],
        );
        assert!(compute_pointer_sweep(&sweep).is_empty());
    }

    #[test]
    fn cancelled_is_always_cleared() {
        let sweep = input(
            vec![person("omar", Some("t-omar-1"))],
            vec![transition("t-omar-1", "omar", SweepStatus::Cancelled)],
        );
        let actions = compute_pointer_sweep(&sweep);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].reason, ClearReason::Cancelled);
    }

    #[test]
    fn in_flight_transitions_are_left_alone() {
        for status in [SweepStatus::AwaitingHandoff, SweepStatus::Overdue, SweepStatus::Ready] {
            let sweep = input(vec![person("p", Some("t"))], vec![transition("t", "p", status)]);
            assert!(
                compute_pointer_sweep(&sweep).is_empty(),
                "in-flight status {status:?} must never be swept",
            );
        }
    }

    #[test]
    fn a_person_with_no_pointer_produces_nothing() {
        let sweep = input(vec![person("p", None)], vec![]);
        assert!(compute_pointer_sweep(&sweep).is_empty());
    }

    #[test]
    fn a_dangling_pointer_at_a_missing_transition_is_left_alone() {
        // Corrupt state is a validation concern; the enumerated rule only clears
        // terminal transitions it can prove unconsumable.
        let sweep = input(vec![person("p", Some("gone"))], vec![]);
        assert!(compute_pointer_sweep(&sweep).is_empty());
    }

    #[test]
    fn actions_are_in_people_order() {
        let sweep = input(
            vec![person("a", Some("ta")), person("b", None), person("c", Some("tc"))],
            vec![
                transition("ta", "a", SweepStatus::Cancelled),
                transition("tc", "c", SweepStatus::Cancelled),
            ],
        );
        let actions = compute_pointer_sweep(&sweep);
        assert_eq!(
            actions.iter().map(|a| a.person_id.as_str()).collect::<Vec<_>>(),
            vec!["a", "c"]
        );
    }

    // --- apply-time re-verify (reverify_clear) --------------------------------

    #[test]
    fn reverify_cancelled_holds_only_while_still_cancelled() {
        let action = ClearPointerAction {
            person_id: "omar".into(),
            transition_id: "t-omar-1".into(),
            reason: ClearReason::Cancelled,
        };
        let cancelled = ClearRecheck {
            active_transition_id: Some("t-omar-1".into()),
            status: Some(SweepStatus::Cancelled),
        };
        assert!(reverify_clear(&action, &cancelled));

        let vanished = ClearRecheck { status: None, ..cancelled };
        assert!(!reverify_clear(&action, &vanished));
    }
}
