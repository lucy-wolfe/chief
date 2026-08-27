//! The one wire shape the actuator reads: what chiefd wants running.
//!
//! # Re-declared, never imported
//!
//! These are second declarations of `chiefd-core`'s
//! `runtime::actuation::{DesiredRuntime, DesiredPerson, HoldReason}`, written
//! against the JSON. That is the same rule [`crate::roster`] follows and the
//! same reason: this crate links no backend crate, and the wire is the
//! contract.
//!
//! # TOMBSTONE: `ObservedReport`, `ObservedPerson`, `UnknownProcess`,
//! # `RuntimeAction`, `RuntimeActionPlan`, `WithheldReason`, `ActuatorPresence`
//!
//! This file used to declare TWO shapes: what the actuator SAW, and what chiefd
//! wanted done about it. The first is deleted outright and the direction it
//! represented is barred — **the actuator never reports anything to chiefd.**
//! The second is deleted because a verb (`start`, `restart`, `stop`, `stopAll`)
//! is a statement about a TRANSITION, and a transition can only be computed by
//! something that knows the current state. Only this crate knows that.
//!
//! What replaced them is smaller than either: a set of people and, for each, a
//! hash of what they should be running. The diff is [`crate::actuate::plan`]'s,
//! computed here, from tmux read at the moment it is acted on.
//!
//! `ObservedReport` was careful about one thing above all, and that care is not
//! lost — it is made unnecessary. It was an enum so that *"untrusted, and here
//! are zero people"* was unrepresentable, because something downstream read
//! that as **nothing is running**, which is a mandate to spawn a whole company
//! a second time on top of one already up. Nothing downstream reads this
//! client's observation any more: an observation this client cannot make is a
//! pass it declines to act on, one function call away from where it was made,
//! and never a claim it sends anywhere. [`crate::actuate::trust`] still holds
//! the same line for this crate's own reading of tmux, and is untouched.

use serde::{Deserialize, Serialize};

/// One person chiefd wants running, and what they should be running.
///
/// There is no `desiredActive` flag: this list is exactly the people who should
/// be up. A person who should not be running is ABSENT, and absence is the
/// instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesiredPerson {
    /// Who.
    pub person_id: String,
    /// The derived hash of what this person's process must be built from.
    ///
    /// The actuator tags a pane with this at launch and compares on every pass.
    /// "A pane exists for this person" is not enough to adopt it — the tag must
    /// MATCH, or the process is stale and is replaced.
    pub launch_hash: String,
}

/// Why the actuator must not act on the desired set this pass.
///
/// Both variants are chiefd's OWN durable safety policy, which is why both
/// survive a change that deleted everything derived from a host fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HoldReason {
    /// The circuit breaker is tripped: only an explicit operator clear resumes.
    BreakerTripped,
    /// The company is in shadow mode. The set is still published IN FULL — an
    /// operator running a shadow diff needs to see what WOULD happen.
    Shadow,
}

impl HoldReason {
    /// The operator-facing sentence for this hold.
    #[must_use]
    pub const fn explain(self) -> &'static str {
        match self {
            Self::BreakerTripped => {
                "the circuit breaker is tripped; only an operator clear resumes actuation"
            }
            Self::Shadow => "shadow mode — nothing will be applied",
        }
    }
}

/// chiefd's complete answer to "what should be running right now".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesiredRuntime {
    /// The company this desired set is for.
    pub company: String,
    /// The effective actuation mode, with the breaker already folded in.
    pub actuation_mode: String,
    /// Exactly the people who should be running, in canonical person order.
    pub people: Vec<DesiredPerson>,
    /// Set when the actuator must not act on `people` this pass.
    #[serde(default)]
    pub hold: Option<HoldReason>,
}

impl DesiredRuntime {
    /// person → launch hash, the form [`crate::placement::desired_topology`]
    /// consumes.
    #[must_use]
    pub fn hashes(&self) -> std::collections::BTreeMap<String, String> {
        self.people
            .iter()
            .map(|person| (person.person_id.clone(), person.launch_hash.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> serde_json::Value {
        serde_json::json!({
            "company": "acme",
            "actuationMode": "apply",
            "people": [
                { "personId": "vera", "launchHash": "aaa" },
                { "personId": "chief", "launchHash": "bbb" },
            ],
        })
    }

    #[test]
    fn the_desired_set_decodes_from_the_body_chiefd_publishes() {
        let desired: DesiredRuntime = serde_json::from_value(body()).expect("decodes");
        assert_eq!(desired.company, "acme");
        assert_eq!(desired.people.len(), 2);
        assert_eq!(desired.people[0].launch_hash, "aaa");
        assert!(desired.hold.is_none(), "an unheld set omits the field rather than sending null");
    }

    /// A held set still carries EVERY person. A hold says "do not act", never
    /// "I have nothing to say" — an operator running a shadow diff needs to see
    /// exactly what would happen when it resumes.
    #[test]
    fn a_held_set_still_carries_the_whole_company() {
        let mut raw = body();
        raw["hold"] = serde_json::json!("shadow");
        let desired: DesiredRuntime = serde_json::from_value(raw).expect("decodes");
        assert_eq!(desired.hold, Some(HoldReason::Shadow));
        assert_eq!(desired.people.len(), 2, "a hold must not empty the set");
    }

    #[test]
    fn a_company_desiring_nobody_publishes_an_empty_set_and_that_is_an_instruction() {
        let mut raw = body();
        raw["people"] = serde_json::json!([]);
        let desired: DesiredRuntime = serde_json::from_value(raw).expect("decodes");
        assert!(desired.people.is_empty());
        assert!(desired.hashes().is_empty(), "nobody desired is a real answer, not a doubt");
    }

    #[test]
    fn the_hashes_map_is_what_placement_consumes() {
        let desired: DesiredRuntime = serde_json::from_value(body()).expect("decodes");
        let hashes = desired.hashes();
        assert_eq!(hashes.get("vera").map(String::as_str), Some("aaa"));
        assert_eq!(hashes.get("nobody"), None);
    }
}
