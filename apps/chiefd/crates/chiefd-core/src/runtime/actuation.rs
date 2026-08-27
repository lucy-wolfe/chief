//! The published DESIRED SET: who should be running, and what they should be
//! running.
//!
//! # The line
//!
//! **chiefd decides WHO runs and WHAT they run. The actuator decides how to
//! make that true, and never reports back.**
//!
//! This module used to publish a per-person action stream — `Start`, `Restart`,
//! `Stop`, `StopAll` — computed by diffing the desired roster against a host
//! observation the actuator had POSTed. That whole design is gone. chiefd has
//! no view of tmux, wants none, and is no longer able to acquire one: it states
//! the desired set, the actuator diffs that against the panes in front of it,
//! and the difference is the actuator's to close.
//!
//! Nothing here names a session, a window, a pane, a socket or a layout — that
//! was true before and stays true. What is new is that nothing here names a
//! VERB either. "Start", "restart" and "kill" are all statements about a
//! transition, and a transition can only be computed by something that knows
//! the current state. Only the actuator knows that.
//!
//! # Why the observation had to go rather than be repaired
//!
//! The immediate defect was a conflation: `cycle.rs` handed the reconcile
//! `Some(observed_person_ids)` unconditionally, so an untrusted report ("I
//! could not look") arrived as `Some(EMPTY)` ("I looked, nobody is there").
//! `Observation` was an enum precisely so that untrusted-with-a-roster is
//! unrepresentable, and the conflation reconstructed that state one call later.
//!
//! That was fixable. It was not worth fixing, because the shape that produced
//! it — a durable decision conditioned on a host fact — had already grown FOUR
//! separate forks (retention, arrival, transfers, planning), each an
//! independent opportunity for the same class of bug. Removing the input
//! removes the class.
//!
//! # What is still chiefd's, and why
//!
//! The circuit breaker and the shadow/apply mode stay here. Both are SAFETY
//! POLICY about whether the company should be actuated at all, which is a
//! durable operator decision rather than a host observation, so they survive
//! the deletion intact and ride on this stream as a [`HoldReason`].
//!
//! # TOMBSTONE: the admission ramp
//!
//! `MAX_STARTS_PER_PASS`, the `Admission` interval, `deferred_starts`,
//! `deferred_restarts` and `admission_ms` are DELETED, by operator ruling: "just
//! boot them all at the same time." The ramp existed because #431 watched 34
//! spawns in one pass drive load to ~25 on 6 cores — but a ramp is a decision
//! about a MACHINE'S capacity, and chiefd is not on that machine. If a boot
//! storm needs pacing, the pacing belongs where the processes are actually
//! spawned. Publishing a partial desired set to spread load would have been
//! strictly worse: it makes chiefd's stated truth depend on how busy a box is.

use serde::{Deserialize, Serialize};

use crate::runtime::roster::DesiredRoster;
use crate::store::converge_safety::ActuationMode;

/// One person chiefd wants running, and what they should be running.
///
/// There is no `desired_active: bool` here: this list contains exactly the
/// people who should be up. A person who should not be running is ABSENT, and
/// absence is the instruction — the actuator kills whatever it finds for
/// somebody not on this list. Publishing everyone with a flag would invite a
/// reader to filter it wrongly, and one such reader (`apps/api`, filtering on
/// the wrong field name) once concluded nobody was ever desired and launched
/// no agents for weeks while every suite stayed green.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesiredPerson {
    /// Who.
    pub person_id: String,
    /// The derived hash of what this person's process must be built from.
    ///
    /// The actuator tags a pane with this at launch and compares on every pass.
    /// "A pane exists for this person" is not enough to adopt it — the tag must
    /// MATCH, or the process is stale and is replaced. See
    /// [`crate::runtime::launch_hash`] for what is and is not an input; in
    /// particular `model`, `provider` and `thinking` are excluded because Pi
    /// applies them live, and including them would restart a person for
    /// changing their own model.
    pub launch_hash: String,
}

/// Why the actuator should not act on the desired set this pass.
///
/// Both variants are chiefd's OWN durable safety policy. Neither is a host
/// fact, which is why both survive the deletion of the observation.
///
/// TOMBSTONE: `NoActuator` and `ObservationUntrusted` are gone. Both were
/// answers to "what did the actuator tell me?", and the actuator no longer
/// tells chiefd anything. chiefd cannot know whether anybody is attached, and
/// deliberately does not try — see the accepted losses in
/// the design record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HoldReason {
    /// The circuit breaker is tripped: three consecutive failed apply cycles.
    /// Only an explicit operator clear resumes.
    BreakerTripped,
    /// The company's actuation mode is shadow. The desired set is still
    /// published in full — an operator running a shadow diff needs to see what
    /// WOULD happen — but the actuator must not act on it.
    Shadow,
}

/// The complete answer to "what does chiefd want running right now".
///
/// Note what is absent, all of it deliberately: no `actuator` presence, because
/// chiefd cannot know who is attached; no `actions`, because a transition
/// cannot be computed without knowing the current state; no `admission_ms` or
/// `deferred_*`, because the ramp is deleted; no `unknown_processes`, because
/// an unattributed pid is a host fact that no longer travels; and no `lease_ms`,
/// because there is no report to keep a lease alive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesiredRuntime {
    /// The company this desired set is for.
    pub company: String,
    /// The effective actuation mode, with the breaker already folded in.
    pub actuation_mode: ActuationMode,
    /// Exactly the people who should be running, in the company's canonical
    /// person order so two reads of an unchanged company are byte-identical.
    pub people: Vec<DesiredPerson>,
    /// Set when the actuator must not act on `people` this pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<HoldReason>,
}

/// Publish the desired set for one company.
///
/// TOTAL, and now total in a much stronger sense than the old planner managed:
/// there is no input that can make this wrong, because there is no input about
/// the host at all. The old contract had to spell out that an absent, lapsed or
/// untrusted observation must never read as "nothing is running" — the
/// dangerous direction, since that reads as a mandate to spawn the whole
/// company a second time on top of one already up. That hazard is now
/// structurally absent: this function cannot express "nothing is running"
/// because it never claims to know what IS running.
///
/// `stopped` is the operator's explicit company stop, read from the committed
/// runtime row, and it EMPTIES the desired set rather than holding it. The
/// distinction is the whole meaning of a stop: a hold says "do not act on this
/// set", so the actuator leaves the company running; a stop says chiefd desires
/// NOBODY, and absence is the instruction that takes them down. Publishing the
/// full set for a stopped company would have the actuator boot, on its very
/// next pass, exactly the company an operator had just switched off.
///
/// `hash_of` supplies each person's derived launch hash. It is a parameter
/// rather than computed here because one of its inputs — the extension source
/// digest — belongs to the crate that can see the launcher checkout, and a
/// second opinion about which code is on disk is exactly the defect the digest
/// exists to prevent.
#[must_use]
pub fn publish_desired_runtime(
    roster: &DesiredRoster,
    mode: ActuationMode,
    breaker_tripped: bool,
    stopped: bool,
    hash_of: impl Fn(&str) -> String,
) -> DesiredRuntime {
    // The desired set is published in FULL on every path, including the held
    // ones. A hold says "do not act", never "I have nothing to say": an
    // operator running a shadow diff, or looking at a tripped breaker, needs to
    // see exactly what would happen when it resumes. The old planner made the
    // opposite choice for its stranger list and had to be corrected for it —
    // dropping the payload on the withholding paths deleted the operator's only
    // evidence at the moment they most needed it.
    let people = roster
        .people
        .iter()
        .filter(|_| !stopped)
        .filter(|person| person.desired_active)
        .map(|person| DesiredPerson {
            person_id: person.id.clone(),
            launch_hash: hash_of(&person.id),
        })
        .collect();

    // Breaker first: it is the stronger statement, and an operator whose
    // breaker has tripped needs to read THAT rather than the mode they set days
    // ago.
    let hold = if breaker_tripped {
        Some(HoldReason::BreakerTripped)
    } else if mode == ActuationMode::Shadow {
        Some(HoldReason::Shadow)
    } else {
        None
    };

    DesiredRuntime { company: roster.company.slug.clone(), actuation_mode: mode, people, hold }
}

#[cfg(test)]
mod tests;
