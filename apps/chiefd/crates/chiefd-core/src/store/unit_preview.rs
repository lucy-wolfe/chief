//! Unit blast radius, removal preview, and the operator tree render.
//!
//! Ports the pure half of `apps/cli/src/legacy/organization/org-staffing.ts`
//! (`organizationUnitSubtree`, `describeUnitRemovalImpact`,
//! `previewOrganizationUnitRemoval`) and of
//! `apps/cli/src/legacy/organization/org-units.ts` (`organizationTreeLines`).
//! Both files are deleted by this change.
//!
//! # Why a preview is business logic and not a formatting concern
//!
//! Removing a unit fires everyone homed anywhere beneath it — offboards them,
//! exactly as `org_offboard` does, keeping every row and its audit trail.
//! Before that commits, an operator has to be told the exact set — and the
//! *same* set the commit will compute, derived by the same walk, or the
//! confirmation prompt is a lie. [`describe_unit_removal_impact`] and
//! [`preview_organization_unit_removal`] therefore live next to the transaction
//! that applies the removal (`store::org_ops::remove_department_tree`) rather
//! than in whatever surface happens to be asking.
//!
//! # The straddle refusals went with the loan concept, then with the column
//!
//! A person could once be loaned OUT of the removed subtree, or borrowed INTO
//! it, and either way they straddled the boundary: removing the unit would
//! strand them or confiscate somebody else's staff. Both cases required a
//! person's home and assigned units to disagree, and a loan was the only thing
//! that allowed that. The loan verbs went on 2026-08-13, and the second column
//! went with them (#1081) — so there is no longer a pair to compare. A person
//! is placed inside the removed subtree or outside it, and neither refusal has
//! a state left to describe.

use std::collections::BTreeSet;

use crate::error::Refusal;
use crate::store::organization::{
    organization_unit_kind, stopped_organization_unit_ancestor, validate_organization_manifest,
    DepartmentRecord, EmploymentState, OrganizationManifest, PersonKind, UnitKind,
    MANIFEST_INVALID, ROOT_DEPARTMENT_ID,
};

/// Refusal code for an operation aimed at a unit the manifest does not have.
pub const UNKNOWN_UNIT: &str = "unknown-unit";

/// Refusal code for removing the company's root unit.
pub const ROOT_UNIT_NOT_REMOVABLE: &str = "root-unit-not-removable";

/// Whether `candidate_id` is a strict descendant of `ancestor_id`.
///
/// Walks upward with a visited set, so a cycle terminates as "not a
/// descendant" rather than hanging.
fn is_descendant(manifest: &OrganizationManifest, candidate_id: &str, ancestor_id: &str) -> bool {
    let mut cursor = manifest.departments.get(candidate_id);
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    while let Some(unit) = cursor {
        let Some(parent) = unit.parent_department_id.as_deref() else {
            return false;
        };
        if !visited.insert(unit.id.as_str()) {
            return false;
        }
        if parent == ancestor_id {
            return true;
        }
        cursor = manifest.departments.get(parent);
    }
    false
}

/// The unit plus every descendant unit, in canonical `department_order`.
///
/// # Errors
/// [`UNKNOWN_UNIT`] when the manifest has no such unit.
pub fn organization_unit_subtree(
    manifest: &OrganizationManifest,
    unit_id: &str,
) -> Result<Vec<String>, Refusal> {
    if !manifest.departments.contains_key(unit_id) {
        return Err(Refusal::new(UNKNOWN_UNIT, format!("Unknown department '{unit_id}'")));
    }
    Ok(manifest
        .department_order
        .iter()
        .filter(|candidate| {
            candidate.as_str() == unit_id || is_descendant(manifest, candidate, unit_id)
        })
        .cloned()
        .collect())
}

/// Who a unit removal would fire.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitRemovalImpact {
    /// The unit's own head, if it has one — the person the delete primarily
    /// fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_person_id: Option<String>,
    /// Every OTHER person the removal fires (its staff plus every person homed
    /// in a descendant unit), in roster order.
    pub member_person_ids: Vec<String>,
    /// Display names for `member_person_ids`, in the same order.
    pub member_names: Vec<String>,
}

/// The exact blast radius of removing `unit_id`, without touching the database.
///
/// Deliberately tolerant of a unit id the manifest does not have: the answer is
/// "nobody", which is what a surface warning wants. The commit path refuses the
/// unknown unit on its own.
#[must_use]
pub fn describe_unit_removal_impact(
    manifest: &OrganizationManifest,
    unit_id: &str,
) -> UnitRemovalImpact {
    let mut removed: BTreeSet<&str> = BTreeSet::new();
    removed.insert(unit_id);
    let mut grew = true;
    while grew {
        grew = false;
        for (id, unit) in &manifest.departments {
            if removed.contains(id.as_str()) {
                continue;
            }
            if unit.parent_department_id.as_deref().is_some_and(|p| removed.contains(p)) {
                removed.insert(id.as_str());
                grew = true;
            }
        }
    }
    let head_person_id = manifest.departments.get(unit_id).map(|unit| unit.head_person_id.clone());
    let member_person_ids: Vec<String> = manifest
        .people_order
        .iter()
        .filter(|id| {
            manifest.people.get(*id).is_some_and(|person| {
                removed.contains(person.department_id.as_str())
                    && Some(*id) != head_person_id.as_ref()
            })
        })
        .cloned()
        .collect();
    let member_names = member_person_ids
        .iter()
        .map(|id| manifest.people.get(id).map_or_else(|| id.clone(), |p| p.name.clone()))
        .collect();
    UnitRemovalImpact { head_person_id, member_person_ids, member_names }
}

/// The result of a removal preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitRemovalPreview {
    /// The manifest the removal would produce, already validated.
    pub manifest: OrganizationManifest,
    /// Units removed, in `department_order`.
    pub removed_department_ids: Vec<String>,
    /// People the removal would OFFBOARD, in `people_order`. They stay in the
    /// draft manifest as departed members of the removed unit's parent — the
    /// removal deletes units, never people.
    pub departed_person_ids: Vec<String>,
}

/// Build and validate the exact recursive-removal result without writing.
///
/// # Errors
/// * [`ROOT_UNIT_NOT_REMOVABLE`] for the company root — remove the company.
/// * [`UNKNOWN_UNIT`] for a unit the manifest does not have.
///   either direction.
/// * [`MANIFEST_INVALID`] when the resulting manifest would not validate.
pub fn preview_organization_unit_removal(
    manifest: &OrganizationManifest,
    unit_id: &str,
    at: &str,
) -> Result<UnitRemovalPreview, Refusal> {
    if unit_id == ROOT_DEPARTMENT_ID {
        return Err(Refusal::new(
            ROOT_UNIT_NOT_REMOVABLE,
            "Remove the company instead of its root unit",
        ));
    }
    let removed_department_ids = organization_unit_subtree(manifest, unit_id)?;
    let removed: BTreeSet<&str> = removed_department_ids.iter().map(String::as_str).collect();

    let departed_person_ids: Vec<String> = manifest
        .people_order
        .iter()
        .filter(|id| {
            manifest.people.get(*id).is_some_and(|p| removed.contains(p.department_id.as_str()))
        })
        .cloned()
        .collect();

    // The draft must be the SAME answer `store::org_ops::remove_department_tree`
    // commits, or the confirmation prompt built from it is a lie. That
    // transaction offboards these people — departed, re-homed to the removed
    // unit's parent, a head of a deleted unit demoted — and deletes no person
    // row, so the draft does exactly that. `unit_id` is not the root (refused
    // above), so its parent exists and survives the removal.
    let parent_department_id = manifest
        .departments
        .get(unit_id)
        .and_then(|unit| unit.parent_department_id.clone())
        .ok_or_else(|| {
            Refusal::new(ROOT_UNIT_NOT_REMOVABLE, "Remove the company instead of its root unit")
        })?;

    let mut draft = manifest.clone();
    for person_id in &departed_person_ids {
        if let Some(person) = draft.people.get_mut(person_id) {
            person.employment_state = EmploymentState::Departed;
            person.department_id.clone_from(&parent_department_id);
            if person.kind == PersonKind::Head {
                person.kind = PersonKind::Worker;
            }
        }
    }
    for department_id in &removed_department_ids {
        draft.departments.remove(department_id);
    }
    draft.department_order.retain(|id| !removed.contains(id.as_str()));
    draft.updated_at = at.to_string();
    validate_organization_manifest(&draft)?;
    Ok(UnitRemovalPreview { manifest: draft, removed_department_ids, departed_person_ids })
}

/// The operator's ASCII organization tree, one line per unit.
///
/// Each line reports the unit's kind, whether it is stopped (and by which
/// ancestor when the pause is inherited), its live headcount, and — for a
/// contract unit — the engagement it exists to deliver.
///
/// # Errors
/// [`MANIFEST_INVALID`] when the root unit is missing or a unit's ancestry
/// cycles.
pub fn organization_tree_lines(manifest: &OrganizationManifest) -> Result<Vec<String>, Refusal> {
    let rank: std::collections::BTreeMap<&str, usize> = manifest
        .department_order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    let mut children: std::collections::BTreeMap<&str, Vec<&DepartmentRecord>> =
        std::collections::BTreeMap::new();
    for unit in manifest.departments.values() {
        if let Some(parent) = unit.parent_department_id.as_deref() {
            children.entry(parent).or_default().push(unit);
        }
    }
    for units in children.values_mut() {
        units.sort_by_key(|unit| rank.get(unit.id.as_str()).copied().unwrap_or(usize::MAX));
    }
    let root = manifest
        .departments
        .get(&manifest.root_department_id)
        .ok_or_else(|| Refusal::new(MANIFEST_INVALID, "Organization root department is missing"))?;
    let mut lines = Vec::new();
    render_unit(manifest, &children, root, "", true, true, &mut lines)?;
    Ok(lines)
}

/// One tree line plus its descendants. Recursion depth is bounded by the
/// manifest's unit depth, which `validate_organization_manifest` proves acyclic.
fn render_unit(
    manifest: &OrganizationManifest,
    children: &std::collections::BTreeMap<&str, Vec<&DepartmentRecord>>,
    unit: &DepartmentRecord,
    prefix: &str,
    last: bool,
    root_unit: bool,
    lines: &mut Vec<String>,
) -> Result<(), Refusal> {
    let kind = organization_unit_kind(manifest, unit)?;
    let branch = if root_unit {
        String::new()
    } else {
        format!("{prefix}{} ", if last { "└─" } else { "├─" })
    };
    let people = manifest
        .people
        .values()
        .filter(|person| {
            person.employment_state != EmploymentState::Departed && person.department_id == unit.id
        })
        .count();
    let state = match stopped_organization_unit_ancestor(manifest, &unit.id)? {
        Some(stopped_by) if stopped_by.id == unit.id => "stopped".to_string(),
        Some(stopped_by) => format!("stopped (ancestor {})", stopped_by.id),
        None => "active".to_string(),
    };
    let transient = match (kind, unit.transient.as_ref()) {
        (UnitKind::Contract, Some(contract)) => {
            format!(" · transient: {}", contract.engagement)
        }
        (UnitKind::Contract, None) => {
            return Err(Refusal::new(
                MANIFEST_INVALID,
                format!("Contract unit '{}' is missing its engagement metadata", unit.id),
            ))
        }
        _ => String::new(),
    };
    let kind_label = match kind {
        UnitKind::Company => "company",
        UnitKind::Department => "department",
        UnitKind::Contract => "contract",
    };
    let noun = if people == 1 { "person" } else { "people" };
    lines.push(format!(
        "{branch}{} ({}) [{kind_label}] {state} · {people} {noun}{transient}",
        unit.name, unit.id
    ));
    let descendants = children.get(unit.id.as_str()).cloned().unwrap_or_default();
    let child_prefix = if root_unit {
        String::new()
    } else {
        format!("{prefix}{}", if last { "   " } else { "│  " })
    };
    let total = descendants.len();
    for (index, child) in descendants.into_iter().enumerate() {
        render_unit(manifest, children, child, &child_prefix, index + 1 == total, false, lines)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::organization::UnitState;
    use crate::test_support::northstar_manifest;

    fn manifest() -> OrganizationManifest {
        northstar_manifest(1_700_000_000_000)
    }

    #[test]
    fn a_leaf_subtree_is_just_itself() {
        let m = manifest();
        assert_eq!(organization_unit_subtree(&m, "quant").expect("subtree"), vec!["quant"]);
    }

    #[test]
    fn the_root_subtree_is_every_unit_in_order() {
        let m = manifest();
        assert_eq!(
            organization_unit_subtree(&m, ROOT_DEPARTMENT_ID).expect("subtree"),
            m.department_order
        );
    }

    #[test]
    fn an_unknown_unit_subtree_refuses() {
        let m = manifest();
        let err = organization_unit_subtree(&m, "nope").expect_err("refusal");
        assert_eq!(err.code, UNKNOWN_UNIT);
    }

    #[test]
    fn the_impact_separates_the_head_from_everyone_else() {
        let m = manifest();
        let impact = describe_unit_removal_impact(&m, "quant");
        assert_eq!(impact.head_person_id.as_deref(), Some("quant-head"));
        assert_eq!(impact.member_person_ids, vec!["signal-researcher".to_string()]);
        assert_eq!(impact.member_names, vec!["Signal Researcher".to_string()]);
    }

    #[test]
    fn the_impact_of_an_unknown_unit_is_nobody() {
        let m = manifest();
        let impact = describe_unit_removal_impact(&m, "nope");
        assert!(impact.head_person_id.is_none());
        assert!(impact.member_person_ids.is_empty());
    }

    #[test]
    fn removing_a_unit_departs_its_people_into_the_parent_and_keeps_the_rest_valid() {
        let m = manifest();
        let preview = preview_organization_unit_removal(&m, "quant", "2026-08-07T00:00:00.000Z")
            .expect("preview");
        assert_eq!(preview.removed_department_ids, vec!["quant".to_string()]);
        assert_eq!(
            preview.departed_person_ids,
            vec!["quant-head".to_string(), "signal-researcher".to_string()]
        );
        assert!(!preview.manifest.departments.contains_key("quant"));
        // The preview must be the SAME answer the commit writes
        // (`org_ops::remove_department_tree`), or the confirmation prompt built
        // from it is a lie. The commit offboards; so does this.
        let head = preview.manifest.people.get("quant-head").expect("the head's record survives");
        assert_eq!(head.employment_state, EmploymentState::Departed);
        assert_eq!(head.department_id, ROOT_DEPARTMENT_ID);
        assert_eq!(head.kind, PersonKind::Worker, "a head of a removed unit is not a head");
        let worker =
            preview.manifest.people.get("signal-researcher").expect("the worker's record survives");
        assert_eq!(worker.employment_state, EmploymentState::Departed);
        assert_eq!(worker.department_id, ROOT_DEPARTMENT_ID);
        assert!(
            preview.manifest.people_order.contains(&"signal-researcher".to_string()),
            "a departed person keeps their place in the roster order"
        );
        assert_eq!(preview.manifest.updated_at, "2026-08-07T00:00:00.000Z");
    }

    #[test]
    fn the_root_unit_is_never_removable() {
        let m = manifest();
        let err =
            preview_organization_unit_removal(&m, ROOT_DEPARTMENT_ID, "2026-08-07T00:00:00.000Z")
                .expect_err("refusal");
        assert_eq!(err.code, ROOT_UNIT_NOT_REMOVABLE);
    }

    #[test]
    fn the_tree_renders_the_root_without_a_branch_and_children_with_one() {
        let m = manifest();
        let lines = organization_tree_lines(&m).expect("tree");
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].starts_with("Northstar Conformance (executive) [company] active · 1 person")
        );
        assert!(lines[1].starts_with("├─ Quant (quant) [department] active · 2 people"));
        assert!(lines[2].starts_with("└─ IT (it) [department] active · 1 person"));
    }

    #[test]
    fn a_paused_unit_names_the_ancestor_that_stopped_it() {
        let mut m = manifest();
        if let Some(root) = m.departments.get_mut(ROOT_DEPARTMENT_ID) {
            root.state = UnitState::Paused;
        }
        let lines = organization_tree_lines(&m).expect("tree");
        assert!(lines[0].contains("] stopped ·"));
        assert!(lines[1].contains("stopped (ancestor executive)"));
    }
}
