//! The desired-person model: the narrow manifest + activity projection chiefd
//! plans from, and the ONE predicate that answers "should this person be
//! running right now".
//!
//! # What this module is, and what it deliberately is not
//!
//! This is the surviving half of what used to be `runtime::reconcile_plan` — a
//! file that carried two unrelated things under one name. The other half was a
//! topology planner: desired panes, desired windows, an observed runtime
//! topology, an ordered plan of pane steps, and the layout maths that sized
//! them. That half now lives in the operator client (`chief-cli`'s
//! `actuate::plan`), because **chiefd decides WHO runs and a client decides
//! WHERE it is shown** — and a backend that still knew what a pane was would be
//! a second copy of one rule, which is the failure shape this workstream exists
//! to delete.
//!
//! So: nothing here names a session, a window, a pane, a socket or a layout.
//! Every type below is a fact about the COMPANY — an id, a name, an ordering
//! the operator chose, a structural relationship, a roster standing — or a
//! decision chiefd genuinely made.
//!
//! # The rules encoded here
//!
//! * **Desired-person filter** ([`is_desired_person`]) — the department chain
//!   from a person up to the root must be active, `employment_state` must be
//!   `Active`, a person who HEADS a department needs that department active
//!   too, and the activity decision must say `active`; with one override, a
//!   decision carrying the [`HANDOFF_REQUIRED_REASON`] reason beats roster
//!   state entirely. A person carrying no decision at all defaults to desired,
//!   subject to the roster filters — that is the "company has never converged"
//!   case, and it is the same branch, not a second rule.
//! * **Fail closed on an inconsistent input** ([`validate`]) — the manifest's
//!   order tables must agree with its maps, and the activity snapshot must
//!   cover every person of the right company exactly once, at a generation of
//!   at least 1.
//!
//! [`is_desired_person`] is public because it is the ONE answer to "who should
//! be running", and every surface that needs it — [`super::roster`], the
//! API-host launch profile — calls it rather than re-reading the same columns.
//! A second implementation of this predicate, in TypeScript, against a field
//! name chiefd does not write, is why `apps/api` launched no agent at all while
//! every suite stayed green.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

/// The one activity reason that overrides roster state in the desired-person
/// filter: a person retained just long enough to write a required handoff stays
/// desired even after being benched or offboarded.
pub const HANDOFF_REQUIRED_REASON: &str = "handoff-required";

// ---------------------------------------------------------------------------
// Inputs: the manifest and the runtime activity snapshot.
// ---------------------------------------------------------------------------

/// Whether a department is running. A paused department (and everything under
/// it) desires nobody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepartmentState {
    /// The department is running.
    Active,
    /// The department is paused; its subtree is not desired.
    Paused,
}

/// A person's roster standing. Only [`EmploymentState::Active`] is ordinarily
/// desired (the handoff-required override aside).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmploymentState {
    /// On the roster and working.
    Active,
    /// Temporarily off; not desired.
    Benched,
    /// Left; not desired.
    Departed,
}

/// One department in the disk model. `id` mirrors the map key it is stored
/// under so a value found via `.values()` still knows its own identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Department {
    /// Stable identity, equal to this department's key in [`Manifest::departments`].
    pub id: String,
    /// Display name. A company fact the operator chose; this module never
    /// renders it.
    pub name: String,
    /// Parent department, or `None` for the organization root.
    pub parent_department_id: Option<String>,
    /// The person who heads this department.
    pub head_person_id: String,
    /// Whether the department is running.
    pub state: DepartmentState,
}

/// One person in the disk model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// Stable identity, equal to this person's key in [`Manifest::people`].
    pub id: String,
    /// The one department this person belongs to and works in.
    pub department_id: String,
    /// Roster standing.
    pub employment_state: EmploymentState,
}

/// The disk model the desired set is derived from. This is the subset of
/// `OrganizationManifest` the pure predicate needs; upstream validation of the
/// wider manifest (head rules) is not repeated here.
///
/// It deliberately carries **no session name**. The old planner stamped one
/// onto its output, which is how a terminal-display decision came to be a
/// backend value; a client mints `org-<slug>` from the slug itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Organization slug.
    pub slug: String,
    /// The root department's id.
    pub root_department_id: String,
    /// All departments, keyed by id.
    pub departments: BTreeMap<String, Department>,
    /// All people, keyed by id.
    pub people: BTreeMap<String, Person>,
    /// Departments in depth-first disk order.
    pub department_order: Vec<String>,
    /// People in disk order.
    pub people_order: Vec<String>,
}

/// One person's runtime activity decision — the explicit disk-derived runtime
/// input the controller consults instead of discovering state from the machine.
///
/// It deliberately carries **no placement**. The decision used to name the
/// department window a person's process should be displayed in, read back out
/// of a persisted `last_pane_department_id` column; that answer is derivable
/// from the manifest at read time (head-in-parent) and belongs to whoever is
/// drawing, so the durable copy — and the field that carried it here — are
/// gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonActivityDecision {
    /// Must equal the person's id.
    pub person_id: String,
    /// The per-person replacement fence. A change here — and only here — means
    /// the running incarnation is stale.
    /// Whether the person is runtime-active this pass.
    pub active: bool,
    /// Activity reasons; [`HANDOFF_REQUIRED_REASON`] overrides roster state.
    pub reasons: Vec<String>,
}

impl PersonActivityDecision {
    fn is_handoff_required(&self) -> bool {
        self.reasons.iter().any(|reason| reason == HANDOFF_REQUIRED_REASON)
    }
}

/// The complete runtime activity snapshot: exactly one decision per person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySnapshot {
    /// Must equal the manifest slug.
    pub organization: String,
    /// Decision per person, keyed by person id.
    pub people: BTreeMap<String, PersonActivityDecision>,
}

/// Errors that fail a desired-set projection closed before any answer is served.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DesiredError {
    /// The manifest's order/lookup tables are inconsistent.
    #[error("Organization manifest is invalid: {0}")]
    ManifestInvalid(String),
    /// The activity snapshot is for a different organization.
    #[error("Activity snapshot does not match organization '{organization}'")]
    ActivityMismatch {
        /// Expected organization slug.
        organization: String,
    },
    /// The activity snapshot does not cover every person exactly once.
    #[error("Activity snapshot must include every organization person")]
    ActivityIncomplete,
    /// A person's activity decision is malformed.
    #[error("Activity snapshot has an invalid decision for '{0}'")]
    ActivityDecision(String),
}

// ---------------------------------------------------------------------------
// The predicate.
// ---------------------------------------------------------------------------

/// True when every department from `department_id` up to the root is active.
#[must_use]
pub fn active_department(manifest: &Manifest, department_id: &str) -> bool {
    let mut cursor = manifest.departments.get(department_id);
    while let Some(department) = cursor {
        if department.state != DepartmentState::Active {
            return false;
        }
        cursor = match &department.parent_department_id {
            Some(parent) => manifest.departments.get(parent),
            None => None,
        };
    }
    true
}

/// Whether chiefd wants this person running: the dept-chain active walk, the
/// employment state, the head-of-a-paused-department rule, and the
/// `handoff-required` override that beats all of them.
#[must_use]
pub fn is_desired_person(
    manifest: &Manifest,
    person: &Person,
    activity: &ActivitySnapshot,
) -> bool {
    let decision = activity.people.get(&person.id);
    if let Some(decision) = decision {
        if decision.is_handoff_required() {
            return decision.active;
        }
    }
    if !active_department(manifest, &person.department_id) {
        return false;
    }
    if person.employment_state != EmploymentState::Active {
        return false;
    }
    if let Some(headed) = manifest.departments.values().find(|d| d.head_person_id == person.id) {
        if !active_department(manifest, &headed.id) {
            return false;
        }
    }
    match decision {
        Some(decision) => decision.active,
        None => true,
    }
}

/// The fail-closed consistency check on a manifest and an activity snapshot
/// taken against it.
///
/// Public because it is a guard, not an implementation detail: a caller about
/// to publish a desired set derived from these two values can refuse first,
/// rather than serve an answer computed from a manifest whose order table
/// disagrees with its own maps.
///
/// # Errors
/// [`DesiredError`] naming which of the four consistency rules failed.
pub fn validate(manifest: &Manifest, activity: &ActivitySnapshot) -> Result<(), DesiredError> {
    let departments = &manifest.departments;
    let people = &manifest.people;

    let unique_departments: BTreeSet<&String> = manifest.department_order.iter().collect();
    if unique_departments.len() != manifest.department_order.len()
        || manifest.department_order.iter().any(|id| !departments.contains_key(id))
        || manifest.department_order.len() != departments.len()
    {
        return Err(DesiredError::ManifestInvalid("department order is invalid".to_string()));
    }
    let unique_people: BTreeSet<&String> = manifest.people_order.iter().collect();
    if unique_people.len() != manifest.people_order.len()
        || manifest.people_order.iter().any(|id| !people.contains_key(id))
        || manifest.people_order.len() != people.len()
    {
        return Err(DesiredError::ManifestInvalid("people order is invalid".to_string()));
    }

    if activity.organization != manifest.slug {
        return Err(DesiredError::ActivityMismatch { organization: manifest.slug.clone() });
    }
    if activity.people.len() != manifest.people_order.len()
        || manifest.people_order.iter().any(|id| !activity.people.contains_key(id))
    {
        return Err(DesiredError::ActivityIncomplete);
    }
    for person_id in &manifest.people_order {
        let decision = &activity.people[person_id];
        if &decision.person_id != person_id {
            return Err(DesiredError::ActivityDecision(person_id.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
