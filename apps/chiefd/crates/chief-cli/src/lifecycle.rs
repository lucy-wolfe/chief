//! `POST /v1/org/lifecycle-status/read`, as this client reads it.
//!
//! # Re-declared, never imported
//!
//! A second declaration of `chiefd_core::store::lifecycle_status`'s
//! `OrganizationLifecycleStatus`, written against the JSON — the rule
//! [`crate::roster`] and [`crate::actuate::desired`] already follow, for the
//! same reason: this crate links no backend crate, and the wire is the
//! contract.
//!
//! # Why the rail reads this at all
//!
//! It is the ONE published place carrying `idleSince`, and `idleSince` is what
//! separates a person who is WORKING from one who is IDLE. The distinction is
//! not a guess about business: `agent_quiet_since` has three states — never
//! beaten, BEATING, and went-quiet — and the settle clock is STOPPED BY the
//! beat, which fires on `message_update`, `message_end` and
//! `tool_execution_start`/`update`/`end`. So a running clock is a positive
//! report of quiet, and no clock on a live pane is a positive report that the
//! model is emitting or a tool is in flight.
//!
//! Only two fields of a large board are read. The rest — `startIntent`,
//! `warnings`, the department rows — are chiefd's answers to questions the rail
//! does not ask, and `serde` drops them.
//!
//! TOMBSTONE (chief-home-is-cwd §4c): `ceoOnlyBootInFlight` was named here as a
//! fourth unread field. The served body no longer carries it — it reported the
//! daemon-side CEO boot lease, and the daemon boots no pane — so listing it
//! would describe a field nothing sends. The RULE it illustrated is unchanged
//! and is the reason this doc names dropped fields at all: this struct is a
//! NARROWING of the wire, not a mirror of it, so a field appearing or
//! disappearing upstream is not a change here.

use serde::{Deserialize, Serialize};

/// One person's row on the lifecycle board, narrowed to what the rail reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecyclePerson {
    /// Who.
    pub person_id: String,
    /// The first durable instant with no effective demand.
    ///
    /// `Some` means the settle clock is RUNNING — the person reported quiet and
    /// is spending down the lease before they park. `None` means no clock, which
    /// on a live pane is the beat still landing.
    #[serde(default)]
    pub idle_since: Option<String>,
}

/// The lifecycle board, narrowed to what the rail reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleStatus {
    /// Person rows, in the company's canonical person order.
    pub people: Vec<LifecyclePerson>,
}

impl LifecycleStatus {
    /// Decode a `/v1/org/lifecycle-status/read` body.
    ///
    /// # Errors
    /// The serde error, verbatim: this is a peer service in the same workspace,
    /// so there is no tolerant second arm for a body it has never sent.
    pub fn from_json(body: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(body)
    }

    /// The people whose settle clock is RUNNING.
    ///
    /// Exactly the IDLE set, for a person who also has a live pane. A person
    /// with no pane is not idle — they are parked or starting, and the clock
    /// they may still carry is a leftover from before, which is why liveness is
    /// consulted first and this set only ever narrows the live ones.
    #[must_use]
    pub fn idle_person_ids(&self) -> std::collections::BTreeSet<String> {
        self.people
            .iter()
            .filter(|person| person.idle_since.is_some())
            .map(|person| person.person_id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::LifecycleStatus;

    /// The body chiefd actually serves, narrowed. Everything this client does
    /// not read must be tolerated rather than refused, because the board grows
    /// on chiefd's schedule and a rail that refused an unknown field would stop
    /// drawing the moment somebody added one.
    #[test]
    fn a_board_decodes_and_unread_fields_are_ignored() {
        let body = r#"{
            "organization": "acme",
            "ceoOnlyBootInFlight": false,
            "departments": [{"departmentId": "executive", "effectiveActive": true}],
            "people": [
                {"personId": "chief", "name": "Ada", "kind": "executive",
                 "departmentId": "executive", "employmentState": "active",
                 "desiredActive": true, "idleSince": "2026-08-13T22:00:00.000Z"},
                {"personId": "analyst", "name": "Bo", "kind": "worker",
                 "departmentId": "quant", "employmentState": "active",
                 "desiredActive": true}
            ],
            "warnings": [],
            "truncated": false
        }"#;
        let status = LifecycleStatus::from_json(body).expect("the board decodes");
        assert_eq!(status.people.len(), 2);
        assert_eq!(
            status.idle_person_ids().iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["chief"],
            "a running settle clock IS the idle set; a person with no clock is not in it"
        );
    }

    #[test]
    fn a_board_with_nobody_idle_yields_an_empty_set() {
        let body = r#"{"people": [{"personId": "chief"}]}"#;
        let status = LifecycleStatus::from_json(body).expect("decodes");
        assert!(status.idle_person_ids().is_empty(), "no clock is not an idle person");
    }
}
