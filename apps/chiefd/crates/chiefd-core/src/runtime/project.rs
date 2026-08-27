//! Project the durable ledgers into the pure runtime input types.
//!
//! The desired-person model ([`desired`](crate::runtime::desired)) and the #29
//! sweep ([`pointer_sweep`](crate::runtime::pointer_sweep)) each define a
//! deliberately narrow input type, distinct from the wider durable store
//! records. This module is the adapter between them: it takes the store's
//! [`OrganizationManifest`], the computed activity [`StoreActivitySnapshot`]
//! (the ledger-mutation half's output), and the activity ledger + a
//! activity ledger, and produces [`plan::Manifest`],
//! [`plan::ActivitySnapshot`], and [`sweep::SweepInput`].
//!
//! Everything here is pure field-mapping — no I/O, no locks, no decisions. The
//! activity *decision* (who is active, and why) is made
//! upstream and only re-shaped here; the projection never re-derives it.

use std::collections::BTreeMap;

use crate::runtime::desired as plan;
use crate::runtime::pointer_sweep as sweep;
use crate::store::activity::{
    ActivityLedger, ActivitySnapshot as StoreActivitySnapshot, TransitionStatus as StoreStatus,
};
use crate::store::organization::{EmploymentState, OrganizationManifest, UnitState};

const fn map_department_state(state: UnitState) -> plan::DepartmentState {
    match state {
        UnitState::Active => plan::DepartmentState::Active,
        UnitState::Paused => plan::DepartmentState::Paused,
    }
}

const fn map_employment(state: EmploymentState) -> plan::EmploymentState {
    match state {
        EmploymentState::Active => plan::EmploymentState::Active,
        EmploymentState::Benched => plan::EmploymentState::Benched,
        EmploymentState::Departed => plan::EmploymentState::Departed,
    }
}

const fn map_status(status: StoreStatus) -> sweep::SweepStatus {
    match status {
        StoreStatus::AwaitingHandoff => sweep::SweepStatus::AwaitingHandoff,
        StoreStatus::Overdue => sweep::SweepStatus::Overdue,
        StoreStatus::Ready => sweep::SweepStatus::Ready,
        StoreStatus::Applied => sweep::SweepStatus::Applied,
        StoreStatus::Cancelled => sweep::SweepStatus::Cancelled,
        StoreStatus::Forced => sweep::SweepStatus::Forced,
    }
}

/// Project the durable manifest into the structural [`plan::Manifest`].
///
/// Field-for-field: only department state and employment enums narrow. The
/// store manifest's `runtime_session` is deliberately NOT carried across — a
/// session name is a display decision a client mints from the slug (#751/P10).
/// Order/consistency is not re-checked here — [`plan::validate`] does that.
#[must_use]
pub fn project_manifest(manifest: &OrganizationManifest) -> plan::Manifest {
    let departments = manifest
        .departments
        .iter()
        .map(|(id, department)| {
            (
                id.clone(),
                plan::Department {
                    id: department.id.clone(),
                    name: department.name.clone(),
                    parent_department_id: department.parent_department_id.clone(),
                    head_person_id: department.head_person_id.clone(),
                    state: map_department_state(department.state),
                },
            )
        })
        .collect();
    let people = manifest
        .people
        .iter()
        .map(|(id, person)| {
            (
                id.clone(),
                plan::Person {
                    id: person.id.clone(),
                    department_id: person.department_id.clone(),
                    employment_state: map_employment(person.employment_state),
                },
            )
        })
        .collect();
    plan::Manifest {
        slug: manifest.slug.clone(),
        root_department_id: manifest.root_department_id.clone(),
        departments,
        people,
        department_order: manifest.department_order.clone(),
        people_order: manifest.people_order.clone(),
    }
}

/// Project the computed activity decisions into [`plan::ActivitySnapshot`].
///
/// The organization slug comes from the manifest (the store snapshot does not
/// carry it).
#[must_use]
pub fn project_activity(
    manifest: &OrganizationManifest,
    snapshot: &StoreActivitySnapshot,
) -> plan::ActivitySnapshot {
    let people = snapshot
        .people
        .iter()
        .map(|(id, decision)| {
            (
                id.clone(),
                plan::PersonActivityDecision {
                    person_id: decision.person_id.clone(),
                    active: decision.active,
                    reasons: decision
                        .reasons
                        .iter()
                        .map(|reason| reason.as_str().to_owned())
                        .collect(),
                },
            )
        })
        .collect();
    plan::ActivitySnapshot { organization: manifest.slug.clone(), people }
}

/// Project the activity ledger into the pure sweep's [`sweep::SweepInput`].
#[must_use]
pub fn project_sweep_input(activity: &ActivityLedger) -> sweep::SweepInput {
    let transitions: BTreeMap<String, sweep::SweepTransition> = activity
        .transitions
        .iter()
        .map(|(id, transition)| {
            (
                id.clone(),
                sweep::SweepTransition {
                    transition_id: transition.id.clone(),
                    person_id: transition.person_id.clone(),
                    status: map_status(transition.status),
                },
            )
        })
        .collect();

    let people = activity
        .person_order
        .iter()
        .filter_map(|person_id| {
            let state = activity.people.get(person_id)?;
            Some(sweep::SweepPerson {
                person_id: person_id.clone(),
                active_transition_id: state.active_transition_id.clone(),
            })
        })
        .collect();

    sweep::SweepInput { people, transitions }
}

/// Project the *committed* activity ledger into [`plan::ActivitySnapshot`].
///
/// This is the integration path the converge cycle uses: it runs after
/// `supervision::cycle` has committed this tick's decisions, and the committed
/// [`PersonActivityState`](crate::store::activity::PersonActivityState) mirrors
/// what that cycle decided — `last_desired_active` is the decision's `active` —
/// so the desired set is derivable from the ledger alone, with no re-run of
/// `reconcile`.
///
/// The persisted `last_pane_department_id` is deliberately NOT read: a display
/// placement is derivable from the manifest at read time (head-in-parent) and
/// belongs to whoever is drawing, so a stored answer here could only be a stale
/// second source of truth (#751/P10).
///
///
///
/// Only the `handoff-required` reason is reconstructed, because it is the sole
/// reason [`plan::is_desired_person`] reads (its roster-override): a person is
/// handoff-required when their active transition is still pending a reflection.
/// Every other activity reason is shutdown/scheduling bookkeeping.
#[must_use]
pub fn project_activity_from_ledger(
    manifest: &OrganizationManifest,
    activity: &ActivityLedger,
) -> plan::ActivitySnapshot {
    let people = activity
        .person_order
        .iter()
        .filter_map(|person_id| {
            let state = activity.people.get(person_id)?;
            let handoff_required = activity
                .active_transition(person_id)
                .is_some_and(|transition| transition.status.is_pending());
            let reasons = if handoff_required {
                vec![plan::HANDOFF_REQUIRED_REASON.to_owned()]
            } else {
                Vec::new()
            };
            Some((
                person_id.clone(),
                plan::PersonActivityDecision {
                    person_id: person_id.clone(),
                    active: state.last_desired_active,
                    reasons,
                },
            ))
        })
        .collect();
    plan::ActivitySnapshot { organization: manifest.slug.clone(), people }
}

#[cfg(test)]
mod tests;
