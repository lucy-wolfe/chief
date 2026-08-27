//! #739 P3: "a kill requires positive evidence, never absence."
//!
//! `ObservedTopology`/`ObservedPane` (chiefd-core's `runtime::reconcile_plan`)
//! describe what THIS pass saw — they say nothing about what was seen on a
//! PRIOR pass. A pane absent from `owned_panes` this pass is equally
//! consistent with "genuinely gone" and "our model of it is incomplete" —
//! design doc §3 P3's exact complaint. `EverObserved` is the missing fact:
//! per-person, has chiefd's host loop ever seen this person's pane alive on
//! this socket, at least once, since the process started.
//!
//! SCOPED TO THIS PROCESS'S LIFETIME, not durable across a chiefd restart —
//! the design doc's own text ("the column and its migration already exist")
//! was verified false: `ever_observed` does not exist anywhere in the tree
//! (grepped repo-wide, zero hits outside an unrelated test name). A durable
//! SQL column is chiefd-core schema work (`chiefd-core/src/schema.rs`) this
//! module does not attempt — chiefd-core's `actor/` is P4's surface (eng-2,
//! in parallel) and this module stays inside chiefd-host, per the
//! architect's file-coordination split. The in-process version is strictly
//! weaker than a durable one (a restart forgets everything it observed and
//! starts every person back at "never observed"), which is the conservative
//! direction: it can only make P3 refuse a kill it would otherwise allow
//! (fail toward NOT deleting a pane), never the reverse.
//!
//! UNEXERCISED: written and reasoned through, never compiled or run, per the
//! standing no-builds directive.

use std::collections::HashSet;
use std::sync::Mutex;

/// Per-company registry of persons whose pane this process has observed
/// alive at least once. One instance lives for the lifetime of a company's
/// converge loop (mirrors `CycleGate`'s own per-company scoping in
/// `safety.rs`).
#[derive(Debug, Default)]
pub struct EverObserved {
    seen: Mutex<HashSet<String>>,
}

impl EverObserved {
    /// An empty registry: nobody has been observed yet.
    ///
    /// Construct ONE per company and keep it for the whole converge loop —
    /// see this type's own doc above. A registry rebuilt per pass can never
    /// accumulate "ever", which is the only property it exists to provide.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `person_id`'s pane was genuinely observed alive THIS
    /// pass. Idempotent — calling it again for the same person is a no-op,
    /// never a reset. There is no corresponding "unmark": a person is
    /// "ever observed" for the rest of this process's life once true,
    /// matching the design doc's own tense ("we have seen it live at least
    /// once") — this is a monotonic fact, not a snapshot of the current
    /// pass.
    pub fn mark_observed(&self, person_id: &str) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.insert(person_id.to_owned());
        }
    }

    /// Has this person's pane EVER been observed alive by this process,
    /// on any prior pass (including this one, if `mark_observed` already
    /// ran for it this pass)? A poisoned lock (a prior panic while holding
    /// it) fails CLOSED — returns `false`, i.e. "not proven observed" —
    /// because this predicate gates a destructive action (P3's kill
    /// precondition) and a poisoned lock is exactly the kind of untrusted
    /// state that must never read as positive evidence.
    #[must_use]
    pub fn was_ever_observed(&self, person_id: &str) -> bool {
        self.seen.lock().map(|seen| seen.contains(person_id)).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::EverObserved;

    #[test]
    fn a_person_never_marked_is_not_ever_observed() {
        let registry = EverObserved::new();
        assert!(!registry.was_ever_observed("alice"));
    }

    #[test]
    fn marking_observed_is_visible_immediately_and_persists() {
        let registry = EverObserved::new();
        registry.mark_observed("alice");
        assert!(registry.was_ever_observed("alice"));
        // A second pass that does NOT re-observe alice must not un-mark her --
        // this is the whole point: absence this pass is not evidence.
        assert!(registry.was_ever_observed("alice"));
    }

    #[test]
    fn marking_one_person_does_not_mark_another() {
        let registry = EverObserved::new();
        registry.mark_observed("alice");
        assert!(!registry.was_ever_observed("bob"));
    }

    #[test]
    fn marking_the_same_person_twice_is_idempotent() {
        let registry = EverObserved::new();
        registry.mark_observed("alice");
        registry.mark_observed("alice");
        assert!(registry.was_ever_observed("alice"));
    }
}
