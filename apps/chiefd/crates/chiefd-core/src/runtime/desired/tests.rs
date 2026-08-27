//! The desired-person filter and the fail-closed input check.
//!
//! Ported from the deleted `reconcile_plan` suite (#751/P10): every case below
//! previously reached the predicate through `desired_topology`, which grouped
//! the answer into windows and panes. The grouping moved to the client; the
//! predicate did not, so the cases are asserted against it directly.

use std::collections::BTreeMap;

use super::{
    active_department, is_desired_person, validate, ActivitySnapshot, Department, DepartmentState,
    DesiredError, EmploymentState, Manifest, Person, PersonActivityDecision,
    HANDOFF_REQUIRED_REASON,
};

// --- builders --------------------------------------------------------------

fn dep(
    id: &str,
    name: &str,
    parent: Option<&str>,
    head: &str,
    state: DepartmentState,
) -> (String, Department) {
    (
        id.to_string(),
        Department {
            id: id.to_string(),
            name: name.to_string(),
            parent_department_id: parent.map(ToString::to_string),
            head_person_id: head.to_string(),
            state,
        },
    )
}

fn per(id: &str, department: &str, employment: EmploymentState) -> (String, Person) {
    (
        id.to_string(),
        Person {
            id: id.to_string(),
            department_id: department.to_string(),
            employment_state: employment,
        },
    )
}

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

/// The `org-runtime.test.ts` fixture: a three-level tree with a benched and a
/// departed engineer, so the roster filters have something to exclude.
fn cobalt() -> Manifest {
    let active = DepartmentState::Active;
    let departments = BTreeMap::from([
        dep("executive", "Executive", None, "chief", active),
        dep("quant", "Quant", Some("executive"), "quant-head", active),
        dep("quant-data", "Data", Some("quant"), "quant-data-head", active),
        dep("it", "IT", Some("executive"), "it-head", active),
    ]);
    let people = BTreeMap::from([
        per("chief", "executive", EmploymentState::Active),
        per("quant-head", "quant", EmploymentState::Active),
        per("quant-active-quant", "quant", EmploymentState::Active),
        per("quant-benched-quant", "quant", EmploymentState::Benched),
        per("quant-data-head", "quant-data", EmploymentState::Active),
        per("quant-data-active-data-engineer", "quant-data", EmploymentState::Active),
        per("quant-data-departed-data-engineer", "quant-data", EmploymentState::Departed),
        per("it-head", "it", EmploymentState::Active),
    ]);
    Manifest {
        slug: "cobalt".to_string(),
        root_department_id: "executive".to_string(),
        departments,
        people,
        department_order: ids(&["executive", "quant", "quant-data", "it"]),
        people_order: ids(&[
            "chief",
            "quant-head",
            "quant-active-quant",
            "quant-benched-quant",
            "quant-data-head",
            "quant-data-active-data-engineer",
            "quant-data-departed-data-engineer",
            "it-head",
        ]),
    }
}

/// The snapshot a fresh roster with no runtime history produces: every person
/// active. This is the shape `project_activity_from_ledger`
/// yields for a converged company, and the input the filters run against.
fn all_active(manifest: &Manifest) -> ActivitySnapshot {
    let people = manifest
        .people_order
        .iter()
        .map(|person_id| {
            (
                person_id.clone(),
                PersonActivityDecision {
                    person_id: person_id.clone(),
                    active: true,
                    reasons: Vec::new(),
                },
            )
        })
        .collect();
    ActivitySnapshot { organization: manifest.slug.clone(), people }
}

/// Everyone the filter says should be running, in canonical person order.
fn desired(manifest: &Manifest, activity: &ActivitySnapshot) -> Vec<String> {
    manifest
        .people_order
        .iter()
        .filter(|id| is_desired_person(manifest, &manifest.people[*id], activity))
        .cloned()
        .collect()
}

// --- the desired-person filter ---------------------------------------------

#[test]
fn the_roster_filters_exclude_the_benched_and_the_departed() {
    let manifest = cobalt();
    assert_eq!(
        desired(&manifest, &all_active(&manifest)),
        ids(&[
            "chief",
            "quant-head",
            "quant-active-quant",
            "quant-data-head",
            "quant-data-active-data-engineer",
            "it-head",
        ]),
        "benched and departed people are never desired"
    );
}

#[test]
fn a_paused_department_takes_its_whole_subtree_and_its_own_head_with_it() {
    let mut manifest = cobalt();
    manifest.departments.get_mut("quant").unwrap().state = DepartmentState::Paused;
    // Pausing quant stops quant AND its child quant-data. `quant-head` is
    // dropped too, even though they are assigned to a department whose own
    // chain is walked from `quant` — the head-of-a-paused-department rule.
    assert_eq!(
        desired(&manifest, &all_active(&manifest)),
        ids(&["chief", "it-head"]),
        "the paused subtree desires nobody"
    );
    assert!(!active_department(&manifest, "quant-data"), "the walk climbs to the paused ancestor");
    assert!(active_department(&manifest, "it"), "a sibling subtree is untouched");
}

#[test]
fn a_handoff_required_decision_beats_roster_state_in_both_directions() {
    let manifest = cobalt();

    // Benched, but carrying a handoff lease: retained.
    let mut keep = all_active(&manifest);
    let benched = keep.people.get_mut("quant-benched-quant").unwrap();
    benched.active = true;
    benched.reasons = vec![HANDOFF_REQUIRED_REASON.to_string()];
    assert!(desired(&manifest, &keep).contains(&"quant-benched-quant".to_string()));

    // Roster-active, but a handoff decision says inactive: dropped.
    let mut drop = all_active(&manifest);
    let active = drop.people.get_mut("quant-active-quant").unwrap();
    active.active = false;
    active.reasons = vec![HANDOFF_REQUIRED_REASON.to_string()];
    assert!(!desired(&manifest, &drop).contains(&"quant-active-quant".to_string()));
}

#[test]
fn a_person_carrying_no_decision_at_all_is_desired_subject_to_the_roster() {
    // The "company has never converged" case is the same branch, not a second
    // rule: an absent decision defaults to desired, and the roster filters
    // still apply on top of it.
    let manifest = cobalt();
    let empty = ActivitySnapshot { organization: manifest.slug.clone(), people: BTreeMap::new() };
    assert_eq!(
        desired(&manifest, &empty),
        desired(&manifest, &all_active(&manifest)),
        "no decision and an active decision reach the same membership"
    );
}

#[test]
fn an_inactive_decision_parks_an_otherwise_desired_person() {
    let manifest = cobalt();
    let mut parked = all_active(&manifest);
    parked.people.get_mut("quant-active-quant").unwrap().active = false;
    assert!(!desired(&manifest, &parked).contains(&"quant-active-quant".to_string()));
}

// --- the fail-closed input check -------------------------------------------

#[test]
fn validate_accepts_a_consistent_manifest_and_snapshot() {
    let manifest = cobalt();
    assert_eq!(validate(&manifest, &all_active(&manifest)), Ok(()));
}

#[test]
fn validate_rejects_a_mismatched_incomplete_or_malformed_snapshot() {
    let manifest = cobalt();

    let mut wrong_org = all_active(&manifest);
    wrong_org.organization = "other".to_string();
    assert!(matches!(validate(&manifest, &wrong_org), Err(DesiredError::ActivityMismatch { .. })));

    let mut missing = all_active(&manifest);
    missing.people.remove("chief");
    assert_eq!(validate(&manifest, &missing), Err(DesiredError::ActivityIncomplete));

    let mut mislabelled = all_active(&manifest);
    mislabelled.people.get_mut("chief").unwrap().person_id = "somebody-else".to_string();
    assert!(matches!(
        validate(&manifest, &mislabelled),
        Err(DesiredError::ActivityDecision(person)) if person == "chief"
    ));
}

#[test]
fn validate_rejects_a_manifest_whose_order_disagrees_with_its_maps() {
    let manifest = cobalt();

    let mut short_departments = manifest.clone();
    short_departments.department_order.pop();
    assert!(matches!(
        validate(&short_departments, &all_active(&manifest)),
        Err(DesiredError::ManifestInvalid(_))
    ));

    let mut duplicate_person = manifest.clone();
    duplicate_person.people_order.push("chief".to_string());
    assert!(matches!(
        validate(&duplicate_person, &all_active(&manifest)),
        Err(DesiredError::ManifestInvalid(_))
    ));

    let mut unknown_person = manifest.clone();
    unknown_person.people_order.push("ghost".to_string());
    unknown_person.people.insert(
        "ghost".to_string(),
        Person {
            id: "ghost".to_string(),
            department_id: "executive".to_string(),
            employment_state: EmploymentState::Active,
        },
    );
    unknown_person.people.remove("chief");
    assert!(matches!(
        validate(&unknown_person, &all_active(&manifest)),
        Err(DesiredError::ManifestInvalid(_))
    ));
}
