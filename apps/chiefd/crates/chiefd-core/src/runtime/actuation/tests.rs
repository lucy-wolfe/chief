//! Both directions on every rule the desired-set publisher has.
//!
//! REPLACES the action-planner suite. That file's organizing question was "when
//! does chiefd tell somebody to start an agent?", and every withholding test
//! had a twin proving the gate was not simply wired shut — because a gate that
//! always withholds is a company that never boots, and it passes every
//! one-directional test.
//!
//! That question no longer has an answer here, because chiefd no longer tells
//! anybody to start anything. It states who should be running; the actuator
//! computes the transition. The twin-test discipline is kept for the rules that
//! remain: every hold has a twin proving the desired set is still published in
//! full underneath it.
//!
//! The whole class of test this file used to be dominated by — trusted vs
//! untrusted observations, cold boot vs proven-empty, the start cap, the
//! destructive budget, the admission ramp — is gone with the
//! inputs those rules read. Their absence is asserted structurally where it
//! matters (see `the_published_set_names_no_verb_and_no_host_fact`).

use super::*;
use crate::runtime::roster::{RosterCompany, RosterDepartment, RosterPerson};

fn person(id: &str, desired: bool) -> RosterPerson {
    RosterPerson {
        id: id.to_owned(),
        display_name: id.to_owned(),
        title: "Engineer".to_owned(),
        department_id: "root".to_owned(),
        is_head_of: None,
        display_order: 0,
        desired_active: desired,
        employment_state: "active".to_owned(),
    }
}

fn roster(people: Vec<RosterPerson>) -> DesiredRoster {
    DesiredRoster {
        company: RosterCompany { slug: "acme".to_owned(), display_name: "Acme".to_owned() },
        root_department_id: "root".to_owned(),
        departments: vec![RosterDepartment {
            id: "root".to_owned(),
            name: "Acme".to_owned(),
            parent_department_id: None,
            head_person_id: "chief".to_owned(),
            order: 0,
            state: "active".to_owned(),
        }],
        people,
    }
}

/// A stand-in for the real derived hash. These tests care that the hash is
/// carried per person and comes from the supplied deriver, not what it equals —
/// `launch_hash`'s own suite owns the derivation rules.
fn hash_of(person_id: &str) -> String {
    format!("hash-{person_id}")
}

fn publish(roster: &DesiredRoster, mode: ActuationMode, breaker: bool) -> DesiredRuntime {
    publish_desired_runtime(roster, mode, breaker, false, hash_of)
}

#[test]
fn the_desired_set_is_exactly_the_people_who_should_be_running() {
    let roster = roster(vec![person("chief", true), person("val", true), person("gone", false)]);
    let published = publish(&roster, ActuationMode::Apply, false);

    assert_eq!(
        published.people.iter().map(|p| p.person_id.as_str()).collect::<Vec<_>>(),
        ["chief", "val"],
        "a person who should not run is ABSENT; absence is the instruction"
    );
    assert_eq!(published.company, "acme");
    assert_eq!(published.hold, None);
}

/// The cold-boot case, which the old planner needed a whole rule for: a company
/// with nothing running. Here it is not a case at all — the desired set does
/// not depend on what is running, so there is no cold-boot branch to get wrong.
/// This is the twin of the hold tests: the gate is not wired shut.
#[test]
fn a_company_that_desires_everybody_publishes_everybody_whatever_is_running() {
    let roster = roster(vec![person("chief", true), person("val", true)]);
    let published = publish(&roster, ActuationMode::Apply, false);
    assert_eq!(published.people.len(), 2, "a cold boot publishes the full desired set");
}

/// The opposite pole, and the one the old planner expressed as a `StopAll`
/// action: a company that desires nobody. It is now simply an empty set, and
/// the actuator kills whatever it finds. No verb, no special case.
#[test]
fn a_company_that_desires_nobody_publishes_an_empty_set_rather_than_a_stop_verb() {
    let roster = roster(vec![person("chief", false), person("val", false)]);
    let published = publish(&roster, ActuationMode::Apply, false);
    assert!(published.people.is_empty());
    assert_eq!(published.hold, None, "desiring nobody is a state, not a refusal to answer");
}

#[test]
fn every_desired_person_carries_the_hash_of_what_they_must_be_running() {
    let roster = roster(vec![person("val", true)]);
    let published = publish(&roster, ActuationMode::Apply, false);
    assert_eq!(published.people[0].launch_hash, "hash-val");
}

/// Ordering is the manifest's canonical person order, so two reads of an
/// unchanged company are byte-identical and a client can diff them cheaply.
#[test]
fn the_published_order_is_the_companys_own_and_never_map_iteration_order() {
    let roster = roster(vec![person("zeta", true), person("alpha", true), person("mid", true)]);
    let published = publish(&roster, ActuationMode::Apply, false);
    assert_eq!(
        published.people.iter().map(|p| p.person_id.as_str()).collect::<Vec<_>>(),
        ["zeta", "alpha", "mid"],
        "the operator's ordering survives; a BTreeMap walk would re-sort it alphabetically"
    );
}

// --- holds, and their twins -------------------------------------------------

#[test]
fn shadow_mode_holds_the_actuator_but_still_publishes_the_whole_desired_set() {
    let roster = roster(vec![person("chief", true), person("val", true)]);
    let published = publish(&roster, ActuationMode::Shadow, false);

    assert_eq!(published.hold, Some(HoldReason::Shadow));
    assert_eq!(
        published.people.len(),
        2,
        "a hold says DO NOT ACT, never I HAVE NOTHING TO SAY: an operator running a \
         shadow diff needs to see exactly what would happen"
    );
}

#[test]
fn a_tripped_breaker_holds_the_actuator_but_still_publishes_the_whole_desired_set() {
    let roster = roster(vec![person("chief", true), person("val", true)]);
    let published = publish(&roster, ActuationMode::Apply, true);

    assert_eq!(published.hold, Some(HoldReason::BreakerTripped));
    assert_eq!(published.people.len(), 2, "the set an operator will resume onto stays visible");
}

/// The breaker is the stronger statement and must be the one reported: an
/// operator whose breaker has tripped needs to read that, not the mode they set
/// days ago.
#[test]
fn a_tripped_breaker_is_reported_ahead_of_shadow_when_both_apply() {
    let roster = roster(vec![person("chief", true)]);
    assert_eq!(
        publish(&roster, ActuationMode::Shadow, true).hold,
        Some(HoldReason::BreakerTripped)
    );
}

#[test]
fn apply_mode_with_a_clear_breaker_holds_nothing() {
    let roster = roster(vec![person("chief", true)]);
    assert_eq!(publish(&roster, ActuationMode::Apply, false).hold, None);
}

// --- the architectural assertions -------------------------------------------

/// THE HARD CONSTRAINT, as a test.
///
/// The published set must contain no VERB and no HOST FACT. A verb ("start",
/// "restart", "kill") is a statement about a transition, and a transition can
/// only be computed by something that knows the current state — which chiefd
/// deliberately no longer does. A host fact is the thing whose upward travel
/// this entire change exists to stop.
///
/// Asserted against the serialized JSON rather than the type, because the wire
/// is what an actuator actually reads, and a field could be reintroduced
/// through a `#[serde(flatten)]` without changing this module's own shape.
#[test]
fn the_published_set_names_no_verb_and_no_host_fact() {
    let roster = roster(vec![person("chief", true), person("val", false)]);
    let published = publish(&roster, ActuationMode::Apply, false);
    let wire = serde_json::to_string(&published).expect("serializes");

    for verb in ["start", "restart", "stop", "stopAll", "action", "delay"] {
        assert!(!wire.contains(verb), "the desired set must name no verb, found `{verb}`: {wire}");
    }
    for host_fact in
        ["observ", "pid", "alive", "actuator", "unknownProcess", "lease", "trusted", "presence"]
    {
        assert!(
            !wire.contains(host_fact),
            "no host fact may appear in what chiefd publishes, found `{host_fact}`: {wire}"
        );
    }
}

/// TOMBSTONE ASSERTION for the admission ramp, by operator ruling ("just boot
/// them all at the same time").
///
/// Twenty desired people produce twenty entries in one pass, with nothing
/// deferred and no delay carried. The ramp was a decision about a MACHINE's
/// capacity, and chiefd is not on that machine; publishing a partial desired
/// set to spread load would make chiefd's stated truth depend on how busy a box
/// is, which is strictly worse than the problem it solved.
#[test]
fn a_large_company_publishes_every_desired_person_at_once_with_no_ramp() {
    let people: Vec<_> = (0..20).map(|i| person(&format!("p{i}"), true)).collect();
    let published = publish(&roster(people), ActuationMode::Apply, false);

    assert_eq!(published.people.len(), 20, "all twenty, in one pass");
    let wire = serde_json::to_string(&published).expect("serializes");
    for ramp_field in ["deferred", "admission", "delayMs", "budget"] {
        assert!(
            !wire.contains(ramp_field),
            "the ramp is deleted; `{ramp_field}` must not survive on the wire: {wire}"
        );
    }
}
