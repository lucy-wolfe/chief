//! The read-only up/down control board.
//!
//! It replaces the launcher's TypeScript status board, which is gone, and is
//! now the only place the board is composed.
//!
//! Pure aggregation: every durable fact arrives as an already read ledger, and
//! this module decides only what the board says. It never mutates, never
//! observes the runtime, and bounds its people list so the answer stays well under the
//! subprocess ceiling.
//!
//! # A missing source degrades the board, it does not fail the call
//!
//! An operator asking "who is up and why" during an incident is exactly the
//! caller who must not be handed an error because one of five ledgers is
//! unreadable. Each source is passed as an `Option`/`Result`-shaped input and a
//! missing one contributes a line to [`OrganizationLifecycleStatus::warnings`]
//! while every other column still renders.
//!
//! # TOMBSTONE (chief-home-is-cwd §4c): the CEO-boot-lease column
//!
//! `ceo_only_boot_in_flight: bool` was a column of this board, fed by a
//! `ceo_boot_lease_held` input the caller read off the `boot_lease` row, and a
//! section here explained why it failed OPEN. The daemon boots no pane, so the
//! lease has no writer and the column could only ever have reported `false`.
//! Both the column and its input are deleted rather than pinned to a constant.
//!
//! # `desired_active` is durable intent, never live observation
//!
//! It is the activity ledger's `last_desired_active`. A pane that died a second
//! ago still reads `true` here, and that is the point — the board reports what
//! chiefd decided, so a disagreement with reality is a visible reconcile
//! backlog rather than an invisible one.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::Refusal;
use crate::store::activity::ActivityLedger;
use crate::store::launch_intent_rows::{LaunchIntent, StartAttribution};
use crate::store::organization::{
    DepartmentRecord, EmploymentState, OrganizationManifest, UnitState, MANIFEST_INVALID,
};

/// How many people the board reports before truncating.
pub const DEFAULT_MAX_PEOPLE: usize = 200;

/// One unit's row on the board.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleDepartmentStatus {
    /// Unit id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The parent unit, absent only on the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_department_id: Option<String>,
    /// The unit's own state.
    pub state: UnitState,
    /// True only when this unit **and every ancestor** is active.
    pub effective_active: bool,
}

/// One person's row on the board.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecyclePersonStatus {
    /// Who.
    pub person_id: String,
    /// Display name.
    pub name: String,
    /// Structural role.
    pub kind: String,
    /// The one department they belong to.
    pub department_id: String,
    /// Employment state.
    pub employment_state: String,
    /// Durable desired up/down from the activity ledger — not live observation.
    pub desired_active: bool,
    /// First durable instant with no effective demand, if idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_since: Option<String>,
    /// The durable "why is this person up?" attribution, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_intent: Option<StartAttribution>,
}

/// The whole board.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationLifecycleStatus {
    /// The company slug.
    pub organization: String,
    /// Unit rows, in `department_order`.
    pub departments: Vec<LifecycleDepartmentStatus>,
    /// Person rows, in `people_order`, bounded by `max_people`.
    pub people: Vec<LifecyclePersonStatus>,
    /// Non-fatal observations — an unreadable source names itself here.
    pub warnings: Vec<String>,
    /// Whether the people list was cut short.
    pub truncated: bool,
}

/// Everything the projection reads, gathered by the caller.
///
/// Each durable source is `Result<Option<…>, String>` so the caller can hand
/// over the exact failure text without this module needing an error taxonomy of
/// its own: `Err(reason)` becomes a warning, `Ok(None)` becomes "absent, no
/// warning".
pub struct LifecycleStatusInput<'a> {
    /// The structural authority. Required — with no manifest there is no board.
    pub manifest: &'a OrganizationManifest,
    /// Activity, for durable desired up/down and idleness.
    pub activity: Result<Option<&'a ActivityLedger>, String>,
    /// Launch intent, for start attributions.
    pub launch_intent: Option<&'a LaunchIntent>,
    /// Optional subtree fence: only this unit and its descendants.
    pub scope_department_id: Option<&'a str>,
    /// Bound on the people list. `None` uses [`DEFAULT_MAX_PEOPLE`].
    pub max_people: Option<usize>,
}

/// Whether a unit and every one of its ancestors is active.
///
/// A cycle answers `false` rather than looping: a manifest that cannot prove
/// its ancestry cannot prove the unit is operational either.
fn ancestry_active(departments: &BTreeMap<String, DepartmentRecord>, department_id: &str) -> bool {
    let mut cursor = departments.get(department_id);
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    while let Some(unit) = cursor {
        if !seen.insert(unit.id.as_str()) || unit.state != UnitState::Active {
            return false;
        }
        cursor = unit.parent_department_id.as_deref().and_then(|id| departments.get(id));
    }
    !seen.is_empty()
}

/// The unit plus every descendant, for the optional subtree fence.
fn subtree_department_ids<'m>(
    departments: &'m BTreeMap<String, DepartmentRecord>,
    root_id: &str,
) -> Result<BTreeSet<&'m str>, Refusal> {
    let root = departments.get_key_value(root_id).ok_or_else(|| {
        Refusal::new(MANIFEST_INVALID, format!("Unknown organization department '{root_id}'"))
    })?;
    let mut result: BTreeSet<&str> = BTreeSet::new();
    result.insert(root.0.as_str());
    let mut grew = true;
    while grew {
        grew = false;
        for (id, unit) in departments {
            if result.contains(id.as_str()) {
                continue;
            }
            if unit.parent_department_id.as_deref().is_some_and(|p| result.contains(p)) {
                result.insert(id.as_str());
                grew = true;
            }
        }
    }
    Ok(result)
}

/// Project the control board.
///
/// # Errors
/// [`MANIFEST_INVALID`] only when `scope_department_id` names a unit the
/// manifest does not have. Every other source failure becomes a warning.
pub fn project_organization_lifecycle_status(
    input: &LifecycleStatusInput<'_>,
) -> Result<OrganizationLifecycleStatus, Refusal> {
    let manifest = input.manifest;
    let scope = match input.scope_department_id {
        Some(id) => Some(subtree_department_ids(&manifest.departments, id)?),
        None => None,
    };
    let include = |department_id: &str| -> bool {
        scope.as_ref().is_none_or(|set| set.contains(department_id))
    };
    let mut warnings: Vec<String> = Vec::new();

    // Map order, NOT `department_order`: the pinned conformance fixture
    // (`conformance/fixtures/tools/org-lifecycle-status-*.json`) records
    // executive/it/quant for a company whose `department_order` is
    // executive/quant/it, because the TypeScript twin iterated the departments
    // object and chiefd serializes that from a BTreeMap. The people list below
    // genuinely is `people_order` — the two differ on purpose, and swapping
    // either one silently breaks a recorded fixture.
    let departments: Vec<LifecycleDepartmentStatus> = manifest
        .departments
        .values()
        .filter(|unit| include(&unit.id))
        .map(|unit| LifecycleDepartmentStatus {
            id: unit.id.clone(),
            name: unit.name.clone(),
            parent_department_id: unit.parent_department_id.clone(),
            state: unit.state,
            effective_active: ancestry_active(&manifest.departments, &unit.id),
        })
        .collect();

    let mut desired_active: BTreeMap<&str, bool> = BTreeMap::new();
    let mut idle_since: BTreeMap<&str, &str> = BTreeMap::new();
    match &input.activity {
        Ok(Some(ledger)) => {
            for (person_id, state) in &ledger.people {
                desired_active.insert(person_id.as_str(), state.last_desired_active);
                if let Some(idle) = state.idle_since.as_deref() {
                    idle_since.insert(person_id.as_str(), idle);
                }
            }
        }
        Ok(None) => {}
        Err(reason) => warnings.push(format!("activity ledger unavailable: {reason}")),
    }

    /// The empty attribution map an absent launch-intent document borrows, so
    /// the person loop below has one shape rather than two.
    static NO_ATTRIBUTIONS: BTreeMap<String, StartAttribution> = BTreeMap::new();
    let attributions: &BTreeMap<String, StartAttribution> = match input.launch_intent {
        Some(intent) => &intent.attributions,
        None => &NO_ATTRIBUTIONS,
    };

    let max_people = input.max_people.unwrap_or(DEFAULT_MAX_PEOPLE);
    let mut people: Vec<LifecyclePersonStatus> = Vec::new();
    let mut truncated = false;
    for person_id in &manifest.people_order {
        let Some(person) = manifest.people.get(person_id) else { continue };
        if person.employment_state == EmploymentState::Departed {
            continue;
        }
        if !include(&person.department_id) {
            continue;
        }
        if people.len() >= max_people {
            truncated = true;
            break;
        }
        people.push(LifecyclePersonStatus {
            person_id: person_id.clone(),
            name: person.name.clone(),
            kind: person_kind_label(person.kind).to_string(),
            department_id: person.department_id.clone(),
            employment_state: employment_label(person.employment_state).to_string(),
            desired_active: desired_active.get(person_id.as_str()).copied().unwrap_or(false),
            idle_since: idle_since.get(person_id.as_str()).map(|s| (*s).to_string()),
            start_intent: attributions.get(person_id).cloned(),
        });
    }

    Ok(OrganizationLifecycleStatus {
        organization: manifest.slug.clone(),
        departments,
        people,
        warnings,
        truncated,
    })
}

/// The wire spelling of a structural role.
fn person_kind_label(kind: crate::store::organization::PersonKind) -> &'static str {
    use crate::store::organization::PersonKind;
    match kind {
        PersonKind::Executive => "executive",
        PersonKind::Head => "head",
        PersonKind::Worker => "worker",
    }
}

/// The wire spelling of an employment state.
fn employment_label(state: EmploymentState) -> &'static str {
    match state {
        EmploymentState::Active => "active",
        EmploymentState::Benched => "benched",
        EmploymentState::Departed => "departed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::activity::ActivityLedger;
    use crate::test_support::northstar_manifest;

    fn manifest() -> OrganizationManifest {
        northstar_manifest(1_700_000_000_000)
    }

    fn input<'a>(manifest: &'a OrganizationManifest) -> LifecycleStatusInput<'a> {
        LifecycleStatusInput {
            manifest,
            activity: Ok(None),
            launch_intent: None,
            scope_department_id: None,
            max_people: None,
        }
    }

    #[test]
    fn every_unit_and_person_is_reported_by_default() {
        let m = manifest();
        let status = project_organization_lifecycle_status(&input(&m)).expect("status");
        assert_eq!(status.organization, "northstar-conformance");
        assert_eq!(status.departments.len(), 3);
        assert_eq!(status.people.len(), 4);
        assert!(!status.truncated);
        assert!(status.warnings.is_empty());
    }

    #[test]
    fn a_paused_ancestor_makes_a_child_not_effectively_active() {
        let mut m = manifest();
        if let Some(root) = m.departments.get_mut("executive") {
            root.state = UnitState::Paused;
        }
        let status = project_organization_lifecycle_status(&input(&m)).expect("status");
        assert!(status.departments.iter().all(|d| !d.effective_active));
    }

    #[test]
    fn departments_come_back_in_map_order_and_people_in_roster_order() {
        // Pinned by conformance/fixtures/tools/org-lifecycle-status-*.json:
        // northstar's `department_order` is executive/quant/it, and the fixture
        // records executive/it/quant.
        let m = manifest();
        let status = project_organization_lifecycle_status(&input(&m)).expect("status");
        let units: Vec<&str> = status.departments.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(units, vec!["executive", "it", "quant"]);
        let people: Vec<&str> = status.people.iter().map(|p| p.person_id.as_str()).collect();
        assert_eq!(people, vec!["chief", "quant-head", "signal-researcher", "it-head"]);
    }

    #[test]
    fn a_subtree_scope_fences_both_units_and_people() {
        let m = manifest();
        let mut i = input(&m);
        i.scope_department_id = Some("quant");
        let status = project_organization_lifecycle_status(&i).expect("status");
        assert_eq!(status.departments.len(), 1);
        assert_eq!(status.people.len(), 2);
    }

    #[test]
    fn an_unknown_scope_refuses() {
        let m = manifest();
        let mut i = input(&m);
        i.scope_department_id = Some("nope");
        let err = project_organization_lifecycle_status(&i).expect_err("refusal");
        assert_eq!(err.code, MANIFEST_INVALID);
    }

    #[test]
    fn the_people_list_is_bounded_and_says_so() {
        let m = manifest();
        let mut i = input(&m);
        i.max_people = Some(2);
        let status = project_organization_lifecycle_status(&i).expect("status");
        assert_eq!(status.people.len(), 2);
        assert!(status.truncated);
    }

    #[test]
    fn a_departed_person_is_never_on_the_board() {
        let mut m = manifest();
        if let Some(person) = m.people.get_mut("signal-researcher") {
            person.employment_state = EmploymentState::Departed;
        }
        let status = project_organization_lifecycle_status(&input(&m)).expect("status");
        assert!(status.people.iter().all(|p| p.person_id != "signal-researcher"));
    }

    #[test]
    fn an_unreadable_source_warns_instead_of_failing() {
        let m = manifest();
        let mut i = input(&m);
        i.activity = Err("corrupt".to_string());
        let status = project_organization_lifecycle_status(&i).expect("status");
        assert_eq!(status.people.len(), 4);
        assert_eq!(status.warnings.len(), 1);
        assert!(status.warnings.iter().any(|w| w.starts_with("activity ledger unavailable")));
    }

    #[test]
    fn desired_active_comes_from_the_activity_ledger() {
        let m = manifest();
        let mut ledger = ActivityLedger::initial(&m, "2026-08-07T00:00:00.000Z");
        if let Some(state) = ledger.people.get_mut("chief") {
            state.last_desired_active = true;
            state.idle_since = Some("2026-08-07T00:01:00.000Z".to_string());
        }
        let mut i = input(&m);
        i.activity = Ok(Some(&ledger));
        let status = project_organization_lifecycle_status(&i).expect("status");
        let ceo = status.people.iter().find(|p| p.person_id == "chief").expect("ceo row");
        assert!(ceo.desired_active);
        assert_eq!(ceo.idle_since.as_deref(), Some("2026-08-07T00:01:00.000Z"));
    }
}
