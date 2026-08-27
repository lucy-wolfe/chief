//! Projection coverage: enum mappings, the handoff-required override token, a
//! manifest round-trip through `desired::validate`, and the sweep projection
//! including the supervision-missing fallback.

use std::collections::BTreeMap;

use super::{
    project_activity, project_activity_from_ledger, project_manifest, project_sweep_input,
};
use crate::runtime::desired as plan;
use crate::runtime::pointer_sweep::{compute_pointer_sweep, ClearReason};
use crate::store::activity::{
    ActivityLedger, ActivityReason, ActivitySnapshot as StoreActivitySnapshot, GracefulTransition,
    PersonActivityDecision as StoreDecision, TransitionAction, TransitionStatus,
};
use crate::test_support::northstar_manifest;

const EPOCH: i64 = 1_784_116_800_000; // 2026-07-15T12:00:00.000Z

fn iso(millis: i64) -> String {
    crate::isotime::iso_millis(millis)
}

#[test]
fn a_projected_manifest_validates_through_the_desired_model() {
    let manifest = northstar_manifest(EPOCH);
    let projected = project_manifest(&manifest);

    assert_eq!(projected.slug, manifest.slug);
    assert_eq!(projected.department_order, manifest.department_order);
    assert_eq!(projected.people_order, manifest.people_order);
    assert_eq!(projected.departments.len(), manifest.departments.len());
    assert_eq!(projected.people.len(), manifest.people.len());

    // `validate` is the real structural contract: the order tables must agree
    // with the maps, and a snapshot must cover every person exactly once.
    let snapshot = all_active(&projected);
    plan::validate(&projected, &snapshot).expect("a faithfully projected manifest must validate");
}

/// A snapshot covering every projected person, active.
fn all_active(manifest: &plan::Manifest) -> plan::ActivitySnapshot {
    let people = manifest
        .people_order
        .iter()
        .map(|person_id| {
            (
                person_id.clone(),
                plan::PersonActivityDecision {
                    person_id: person_id.clone(),
                    active: true,
                    reasons: Vec::new(),
                },
            )
        })
        .collect();
    plan::ActivitySnapshot { organization: manifest.slug.clone(), people }
}

#[test]
fn department_and_employment_enums_map_faithfully() {
    let manifest = northstar_manifest(EPOCH);
    let projected = project_manifest(&manifest);
    for (id, department) in &manifest.departments {
        let want = match department.state {
            crate::store::organization::UnitState::Active => plan::DepartmentState::Active,
            crate::store::organization::UnitState::Paused => plan::DepartmentState::Paused,
        };
        assert_eq!(projected.departments[id].state, want);
    }
    for (id, person) in &manifest.people {
        let want = match person.employment_state {
            crate::store::organization::EmploymentState::Active => plan::EmploymentState::Active,
            crate::store::organization::EmploymentState::Benched => plan::EmploymentState::Benched,
            crate::store::organization::EmploymentState::Departed => {
                plan::EmploymentState::Departed
            }
        };
        assert_eq!(projected.people[id].employment_state, want);
    }
}

fn decision(person: &str, reasons: Vec<ActivityReason>) -> StoreDecision {
    StoreDecision { person_id: person.to_owned(), active: true, reasons, transition_id: None }
}

#[test]
fn activity_projection_maps_fields_and_the_handoff_required_token() {
    let manifest = northstar_manifest(EPOCH);
    let snapshot = StoreActivitySnapshot {
        people: BTreeMap::from([
            ("boss".to_owned(), decision("boss", vec![ActivityReason::OrganizationRoot])),
            ("hand".to_owned(), decision("hand", vec![ActivityReason::HandoffRequired])),
        ]),
    };
    let projected = project_activity(&manifest, &snapshot);

    assert_eq!(projected.organization, manifest.slug);
    assert!(projected.people["boss"].active);
    assert_eq!(projected.people["boss"].reasons, vec!["organization-root".to_owned()]);
    // The override token the desired-person filter keys on is preserved exactly.
    assert!(projected.people["hand"].reasons.contains(&plan::HANDOFF_REQUIRED_REASON.to_owned()));
}

fn transition(id: &str, person: &str, status: TransitionStatus) -> GracefulTransition {
    GracefulTransition {
        id: id.to_owned(),
        person_id: person.to_owned(),
        action: TransitionAction::Park,
        reason: "test".to_owned(),
        intent_id: None,
        placement_department_id: "root".to_owned(),
        to_department_id: None,
        status,
        requested_at: iso(EPOCH),
        handoff_deadline_at: iso(EPOCH),
        applied_at: None,
        cancelled_at: None,
        forced_at: None,
        abandoned_at: None,
    }
}

/// Seed an activity ledger, point one person at a transition, and register it.
fn ledger_with_pointer(pointer_status: TransitionStatus) -> (ActivityLedger, String) {
    let manifest = northstar_manifest(EPOCH);
    let mut ledger = ActivityLedger::initial(&manifest, &iso(EPOCH));
    let person = ledger.person_order.first().cloned().expect("northstar has people");
    let transition_id = format!("transition:1:{person}:park");
    ledger.people.get_mut(&person).expect("seeded person state").active_transition_id =
        Some(transition_id.clone());
    ledger
        .transitions
        .insert(transition_id.clone(), transition(&transition_id, &person, pointer_status));
    (ledger, person)
}

#[test]
fn sweep_projection_leaves_an_applied_transition_alone() {
    // An applied transition is still legitimately claimable, so its pointer is
    // never cleared.
    let (ledger, _person) = ledger_with_pointer(TransitionStatus::Applied);
    assert!(compute_pointer_sweep(&project_sweep_input(&ledger)).is_empty());
}

#[test]
fn sweep_projection_clears_a_cancelled_transition() {
    // A cancelled transition can never be released, so its pointer is
    // unconsumable and the sweep clears it.
    let (ledger, person) = ledger_with_pointer(TransitionStatus::Cancelled);
    let input = project_sweep_input(&ledger);
    let actions = compute_pointer_sweep(&input);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].person_id, person);
    assert_eq!(actions[0].reason, ClearReason::Cancelled);
}

// --- ledger-based projection (project_activity_from_ledger) -----------------

/// A fresh activity ledger with `person` marked desired-active in a valid pane
/// department (the seeded placement), everyone else left inactive.
fn ledger_with_one_active(
    person: &str,
) -> (crate::store::organization::OrganizationManifest, ActivityLedger) {
    let manifest = northstar_manifest(EPOCH);
    let mut ledger = ActivityLedger::initial(&manifest, &iso(EPOCH));
    ledger.people.get_mut(person).expect("seeded person").last_desired_active = true;
    (manifest, ledger)
}

#[test]
fn ledger_projection_marks_desired_people_active_and_round_trips() {
    let (manifest, ledger) = ledger_with_one_active("signal-researcher");
    let projected = project_activity_from_ledger(&manifest, &ledger);

    assert_eq!(projected.organization, manifest.slug);
    assert!(projected.people["signal-researcher"].active);
    // An untouched person stays inactive (initial ledger seeds desired-active false).
    assert!(!projected.people[manifest.chief_person_id().unwrap()].active);
    // Every manifest person is covered (`validate` requires exact coverage).
    assert_eq!(projected.people.len(), manifest.people.len());

    // The projection validates, and the desired-person filter agrees with the
    // ledger: only the active person is wanted.
    let projected_manifest = project_manifest(&manifest);
    plan::validate(&projected_manifest, &projected).expect("a ledger projection must validate");
    let desired: Vec<&str> = projected_manifest
        .people_order
        .iter()
        .filter(|id| {
            plan::is_desired_person(
                &projected_manifest,
                &projected_manifest.people[*id],
                &projected,
            )
        })
        .map(String::as_str)
        .collect();
    assert_eq!(desired, vec!["signal-researcher"], "only the active person is desired");
}

#[test]
fn ledger_projection_reconstructs_handoff_required_from_a_pending_transition() {
    let (manifest, mut ledger) = ledger_with_one_active("signal-researcher");
    // Strand a pending (awaiting-handoff) transition on the person.
    let transition_id = "transition:1:signal-researcher:park".to_owned();
    ledger.transitions.insert(
        transition_id.clone(),
        transition(&transition_id, "signal-researcher", TransitionStatus::AwaitingHandoff),
    );
    ledger.transition_order.push(transition_id.clone());
    ledger.people.get_mut("signal-researcher").unwrap().active_transition_id = Some(transition_id);

    let projected = project_activity_from_ledger(&manifest, &ledger);
    assert!(
        projected.people["signal-researcher"]
            .reasons
            .contains(&plan::HANDOFF_REQUIRED_REASON.to_owned()),
        "a pending transition makes the person handoff-required",
    );
}

#[test]
fn ledger_projection_omits_handoff_required_for_a_terminal_transition() {
    let (manifest, mut ledger) = ledger_with_one_active("signal-researcher");
    // An applied (terminal) transition is not pending — no handoff-required.
    let transition_id = "transition:1:signal-researcher:park".to_owned();
    ledger.transitions.insert(
        transition_id.clone(),
        transition(&transition_id, "signal-researcher", TransitionStatus::Applied),
    );
    ledger.transition_order.push(transition_id.clone());
    ledger.people.get_mut("signal-researcher").unwrap().active_transition_id = Some(transition_id);

    let projected = project_activity_from_ledger(&manifest, &ledger);
    assert!(projected.people["signal-researcher"].reasons.is_empty());
}
