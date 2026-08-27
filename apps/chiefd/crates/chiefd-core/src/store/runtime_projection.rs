//! Compare *what the runtime wants projected* against *what runtime actually
//! shows* — the pure half of `org-runtime-projection.ts`.
//!
//! # Why this is its own module
//!
//! The comparison is the single fact every reconciler decision hangs off:
//! "missing" drives a spawn, "unexpected" drives a kill. Both answers are
//! ORDER-SENSITIVE — the supervisor spawns and kills in the order these
//! vectors carry — so the ordering rule below is behaviour, not cosmetics,
//! and it is locked by tests rather than left to whichever map the caller
//! happened to build.
//!
//! # The ordering rule
//!
//! `desiredPersonIds` is the manifest's `peopleOrder` filtered to the people
//! the activity ledger last recorded as desired-active. `observedPersonIds` is
//! the process-handle map re-sorted the same way: every observed id that the
//! manifest knows, in **manifest order**, followed by every observed id it
//! does NOT know, in their own order. That second group exists because the
//! supervisor deliberately includes unknown launcher-tagged processes in its
//! map so normal reconciliation can remove them; a passive reader may pass
//! only current-manifest identities. Either way the caller decides what goes
//! into `process_handles`, and this function decides nothing about membership —
//! only about order and set difference.
//!
//! # No I/O
//!
//! `desired_active` is projected by the caller from the activity ledger (the
//! `lastDesiredActive` flag) and `process_handles` comes from an
//! ownership-audited runtime read. Nothing here reads a store, so the whole comparison runs inside
//! the caller's single `BEGIN IMMEDIATE` transaction (Mandate 4).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::store::organization::OrganizationManifest;

/// One ownership-audited process-handle map compared with the desired activity
/// set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeProjectionComparison {
    /// The people the runtime wants projected, in manifest `peopleOrder`.
    pub desired_person_ids: Vec<String>,
    /// The people runtime actually shows: manifest-known ids in manifest order,
    /// then ids the manifest does not know, in their own order.
    pub observed_person_ids: Vec<String>,
    /// Desired but not observed — these need a process.
    pub missing_desired_person_ids: Vec<String>,
    /// Observed but not desired — these processes must go.
    pub unexpected_observed_person_ids: Vec<String>,
    /// Whether the projection already matches the desire exactly.
    pub exact: bool,
}

/// Compare the desired activity set with an observed process-handle map.
///
/// `desired_active` is the set of person ids whose activity-ledger
/// `lastDesiredActive` is true; ids in it that the manifest does not know are
/// ignored, exactly as the TypeScript filter over `peopleOrder` ignored them.
/// `process_handles` maps person id to process handle; only its keys matter here.
#[must_use]
pub fn compare_runtime_projection(
    manifest: &OrganizationManifest,
    desired_active: &BTreeSet<String>,
    process_handles: &BTreeMap<String, String>,
) -> RuntimeProjectionComparison {
    let desired_person_ids: Vec<String> = manifest
        .people_order
        .iter()
        .filter(|person_id| desired_active.contains(*person_id))
        .cloned()
        .collect();

    // The TS `observed.delete(personId)` idiom: consume the manifest-known ids
    // in manifest order, then append whatever the manifest never mentioned.
    // A `BTreeSet` gives the leftovers a deterministic (lexicographic) order —
    // the JS object-key order the predecessor relied on has no Rust analogue,
    // and "whatever the hash landed on" is not an ordering a kill sequence may
    // depend on.
    let mut remaining: BTreeSet<&str> = process_handles.keys().map(String::as_str).collect();
    let mut observed_person_ids: Vec<String> = Vec::with_capacity(process_handles.len());
    for person_id in &manifest.people_order {
        if remaining.remove(person_id.as_str()) {
            observed_person_ids.push(person_id.clone());
        }
    }
    observed_person_ids.extend(remaining.into_iter().map(str::to_owned));

    let desired: BTreeSet<&str> = desired_person_ids.iter().map(String::as_str).collect();
    let observed: BTreeSet<&str> = observed_person_ids.iter().map(String::as_str).collect();
    let missing_desired_person_ids: Vec<String> = desired_person_ids
        .iter()
        .filter(|person_id| !observed.contains(person_id.as_str()))
        .cloned()
        .collect();
    let unexpected_observed_person_ids: Vec<String> = observed_person_ids
        .iter()
        .filter(|person_id| !desired.contains(person_id.as_str()))
        .cloned()
        .collect();

    RuntimeProjectionComparison {
        exact: missing_desired_person_ids.is_empty() && unexpected_observed_person_ids.is_empty(),
        desired_person_ids,
        observed_person_ids,
        missing_desired_person_ids,
        unexpected_observed_person_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::organization::{
        DepartmentRecord, EmploymentState, OrganizationPolicy, PersonKind, PersonRecord, UnitKind,
        UnitState, ORGANIZATION_SCHEMA_VERSION, ROOT_DEPARTMENT_ID,
    };

    const AT: &str = "2026-08-01T00:00:00.000Z";

    fn person(id: &str) -> PersonRecord {
        PersonRecord {
            id: id.to_string(),
            name: id.to_string(),
            title: "engineer".to_string(),
            mandate: "build".to_string(),
            kind: PersonKind::Worker,
            department_id: ROOT_DEPARTMENT_ID.to_string(),
            employment_state: EmploymentState::Active,
            activation: "resident".to_string(),
            tools: Vec::new(),
            prompts: Vec::new(),
            created_at: AT.to_string(),
            staffing_history: None,
            extra: Default::default(),
        }
    }

    fn manifest(order: &[&str]) -> OrganizationManifest {
        let mut people = BTreeMap::new();
        for id in order {
            people.insert((*id).to_string(), person(id));
        }
        let mut departments = BTreeMap::new();
        departments.insert(
            ROOT_DEPARTMENT_ID.to_string(),
            DepartmentRecord {
                id: ROOT_DEPARTMENT_ID.to_string(),
                name: "Executive".to_string(),
                purpose: "run the company".to_string(),
                kind: Some(UnitKind::Company),
                transient: None,
                parent_department_id: None,
                head_person_id: order.first().map_or(String::new(), |id| (*id).to_string()),
                state: UnitState::Active,
                created_at: AT.to_string(),
                extra: Default::default(),
            },
        );
        OrganizationManifest {
            schema_version: ORGANIZATION_SCHEMA_VERSION,
            kind: "organization".to_string(),
            slug: "acme".to_string(),
            name: "Acme".to_string(),
            purpose: "ship".to_string(),
            root_department_id: ROOT_DEPARTMENT_ID.to_string(),
            policy: OrganizationPolicy {
                supervision_interval_ms: 1_000,
                acknowledgement_timeout_ms: 1_000,
                acknowledgement_retry_limit: 1,
                replacement_limit: 1,
            },
            department_order: vec![ROOT_DEPARTMENT_ID.to_string()],
            people_order: order.iter().map(|id| (*id).to_string()).collect(),
            departments,
            people,
            created_at: AT.to_string(),
            updated_at: AT.to_string(),
            extra: Default::default(),
        }
    }

    // Pid-shaped values, never `%N`: this map holds what the ACTUATOR reported,
    // and a `%N` fixture here is the same fiction that kept the roster reader's
    // tmux-id validator looking correct. Only the keys are read, so the values
    // exist to keep the fixture honest about the payload.
    fn process_handles(ids: &[&str]) -> BTreeMap<String, String> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| ((*id).to_string(), (48_213 + i).to_string()))
            .collect()
    }

    fn desired(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn desired_follows_manifest_order_not_set_order() {
        // `zoe` sorts last but is first in the manifest: the comparison must
        // report manifest order, which is the spawn order.
        let manifest = manifest(&["zoe", "ada", "bob"]);
        let out =
            compare_runtime_projection(&manifest, &desired(&["ada", "zoe"]), &process_handles(&[]));
        assert_eq!(out.desired_person_ids, vec!["zoe".to_string(), "ada".to_string()]);
        assert_eq!(out.missing_desired_person_ids, vec!["zoe".to_string(), "ada".to_string()]);
        assert!(!out.exact);
    }

    #[test]
    fn observed_puts_manifest_known_ids_first_then_unknown_ones() {
        let manifest = manifest(&["zoe", "ada"]);
        let out = compare_runtime_projection(
            &manifest,
            &desired(&["zoe", "ada"]),
            &process_handles(&["ada", "ghost", "zoe", "alpha"]),
        );
        // manifest order first (zoe, ada), then the strangers in their own order.
        assert_eq!(
            out.observed_person_ids,
            vec!["zoe".to_string(), "ada".to_string(), "alpha".to_string(), "ghost".to_string()]
        );
        assert_eq!(
            out.unexpected_observed_person_ids,
            vec!["alpha".to_string(), "ghost".to_string()]
        );
        assert!(out.missing_desired_person_ids.is_empty());
        assert!(!out.exact);
    }

    #[test]
    fn an_exact_projection_reports_exact() {
        let manifest = manifest(&["ada", "bob"]);
        let out = compare_runtime_projection(
            &manifest,
            &desired(&["ada", "bob"]),
            &process_handles(&["bob", "ada"]),
        );
        assert!(out.exact);
        assert_eq!(out.desired_person_ids, out.observed_person_ids);
        assert!(out.missing_desired_person_ids.is_empty());
        assert!(out.unexpected_observed_person_ids.is_empty());
    }

    #[test]
    fn a_desired_id_the_manifest_does_not_know_is_ignored() {
        // The TS filtered `peopleOrder`, so an activity row for a departed
        // person could never become "desired". Same here.
        let manifest = manifest(&["ada"]);
        let out = compare_runtime_projection(
            &manifest,
            &desired(&["ada", "ghost"]),
            &process_handles(&["ada"]),
        );
        assert_eq!(out.desired_person_ids, vec!["ada".to_string()]);
        assert!(out.exact);
    }

    #[test]
    fn a_parked_person_with_a_live_process_is_unexpected_not_missing() {
        let manifest = manifest(&["ada", "bob"]);
        let out = compare_runtime_projection(
            &manifest,
            &desired(&["ada"]),
            &process_handles(&["ada", "bob"]),
        );
        assert_eq!(out.unexpected_observed_person_ids, vec!["bob".to_string()]);
        assert!(out.missing_desired_person_ids.is_empty());
        assert!(!out.exact);
    }

    #[test]
    fn an_empty_company_compares_exact() {
        let manifest = manifest(&[]);
        let out = compare_runtime_projection(&manifest, &desired(&[]), &process_handles(&[]));
        assert!(out.exact);
        assert!(out.desired_person_ids.is_empty());
        assert!(out.observed_person_ids.is_empty());
    }
}
