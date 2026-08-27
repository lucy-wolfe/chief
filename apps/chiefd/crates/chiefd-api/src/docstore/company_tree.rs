//! The structured company tree: departments as a forest, each with its people.
//!
//! # Why this is a chiefd route and not a client projection
//!
//! `apps/api` built this shape in TypeScript (`buildTree`) from a manifest it
//! fetched from chiefd. That is a projection of chiefd's own data living in a
//! client — the exact duplication mandate 3 forbids — and it is why deleting
//! `apps/api` left the web unable to render a company at all.
//!
//! chiefd already had `/v1/org/tree/read`, but that answers a different
//! question: it returns ASCII tree LINES for an operator's terminal. A browser
//! needs the structure, not a rendering of it.
//!
//! # What this is NOT
//!
//! Placement and identity only. No runtime state — not who is running, not a
//! provider or model or thinking level. Those belong to the routes that
//! actually observe them, and a tree that carried a stale `running` flag would
//! be a snapshot pretending to be live. The predecessor shape shipped six such
//! fields the route never served, and every company page failed validation on
//! them and rendered "Loading…" forever.
//!
//! `employmentState` is on the PLACEMENT side of that line, not the runtime
//! side: it is a durable manifest field that hire and offboard write, so it
//! cannot be stale between reads. Omitting it was not restraint — it silently
//! dropped the one fact that distinguishes somebody who works here from
//! somebody who left, from the only roster the browser is given.

use std::collections::BTreeMap;

use chiefd_core::store::organization::{
    DepartmentRecord, OrganizationManifest, PersonRecord, UnitState,
};
use serde::Serialize;

/// One person, as the tree carries them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TreePerson {
    /// Stable person id.
    pub(crate) id: String,
    /// Display name.
    pub(crate) name: String,
    /// Job title.
    pub(crate) title: String,
    /// Structural role, lowercased for the wire (`worker`/`head`/`executive`).
    pub(crate) kind: String,
    /// Whether they still work here, lowercased for the wire
    /// (`active`/`benched`/`departed`).
    ///
    /// This is PLACEMENT, not runtime state (see the module header): it is a
    /// durable manifest field, decided by hire and offboard, and it does not
    /// go stale between reads the way a `running` flag would.
    ///
    /// It is carried because this projection is the only roster the browser
    /// has. Without it a departed person rendered identically to an active
    /// one and was still offered Transfer and Offboard, so an operator could
    /// offboard somebody who had already left and had no way to see who was
    /// still employed. The offboard itself was always durable — the manifest
    /// records `departed` — and only this projection lost it.
    pub(crate) employment_state: String,
    /// Identity colour, allocated for EVERY person including the chief.
    ///
    /// Still optional, and absence still means "no allocated colour, use the
    /// neutral one" — but the case that produced it has changed. It used to be
    /// the two standard Pi identities, which carried no generated theme by
    /// design; no one carries one now, so the only way to be without a colour
    /// is an exhausted palette.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) accent: Option<String>,
}

/// A department and everything beneath it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DepartmentNode {
    /// Hierarchical department id.
    pub(crate) id: String,
    /// Display name.
    pub(crate) name: String,
    /// Who heads it.
    pub(crate) head_person_id: String,
    /// `active` or `paused`.
    pub(crate) state: String,
    /// People whose department this is, in canonical people order.
    pub(crate) people: Vec<TreePerson>,
    /// Child departments, in canonical department order.
    pub(crate) children: Vec<DepartmentNode>,
}

/// The whole tree. No envelope: the response IS the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompanyTree {
    /// The company slug as the caller asked for it.
    pub(crate) slug: String,
    /// The root department's id.
    pub(crate) root_department_id: String,
    /// The forest, rooted at `root_department_id`.
    pub(crate) departments: Vec<DepartmentNode>,
}

/// Project a committed manifest into the tree.
///
/// Ordering is the manifest's own (`department_order`, `people_order`), never
/// map iteration order: the canonical orders are what make two reads of an
/// unchanged company byte-identical, and a `BTreeMap` walk would silently
/// re-sort a company's departments alphabetically.
///
/// A person is placed by `department_id` — the one department they belong to
/// and work in, and the one the operator's pane shows them under. That used to
/// be a choice between two columns, with a row where they differed being
/// corrupt rather than merely unusual; there is one column, so there is no
/// choice left to explain.
pub(crate) fn build_company_tree(
    slug: &str,
    manifest: &OrganizationManifest,
    accents: &BTreeMap<String, String>,
) -> CompanyTree {
    let mut children_of: BTreeMap<&str, Vec<&DepartmentRecord>> = BTreeMap::new();
    for id in &manifest.department_order {
        let Some(unit) = manifest.departments.get(id) else { continue };
        let Some(parent) = unit.parent_department_id.as_deref() else { continue };
        children_of.entry(parent).or_default().push(unit);
    }

    let mut people_of: BTreeMap<&str, Vec<&PersonRecord>> = BTreeMap::new();
    for id in &manifest.people_order {
        let Some(person) = manifest.people.get(id) else { continue };
        people_of.entry(person.department_id.as_str()).or_default().push(person);
    }

    let departments = manifest
        .departments
        .get(&manifest.root_department_id)
        .map(|root| vec![node(root, &children_of, &people_of, accents)])
        .unwrap_or_default();

    CompanyTree {
        slug: slug.to_owned(),
        root_department_id: manifest.root_department_id.clone(),
        departments,
    }
}

fn node(
    unit: &DepartmentRecord,
    children_of: &BTreeMap<&str, Vec<&DepartmentRecord>>,
    people_of: &BTreeMap<&str, Vec<&PersonRecord>>,
    accents: &BTreeMap<String, String>,
) -> DepartmentNode {
    DepartmentNode {
        id: unit.id.clone(),
        name: unit.name.clone(),
        head_person_id: unit.head_person_id.clone(),
        state: match unit.state {
            UnitState::Active => "active".to_owned(),
            UnitState::Paused => "paused".to_owned(),
        },
        people: people_of
            .get(unit.id.as_str())
            .map(|people| {
                people
                    .iter()
                    .map(|person| TreePerson {
                        id: person.id.clone(),
                        name: person.name.clone(),
                        title: person.title.clone(),
                        kind: format!("{:?}", person.kind).to_lowercase(),
                        employment_state: format!("{:?}", person.employment_state).to_lowercase(),
                        accent: accents.get(&person.id).cloned(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        children: children_of
            .get(unit.id.as_str())
            .map(|units| {
                units.iter().map(|child| node(child, children_of, people_of, accents)).collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chiefd_core::test_support::northstar_manifest;

    fn tree(manifest: &OrganizationManifest) -> CompanyTree {
        build_company_tree("cobalt", manifest, &BTreeMap::new())
    }

    #[test]
    fn roots_at_the_manifests_own_root_department() {
        let manifest = northstar_manifest(0);
        let built = tree(&manifest);

        assert_eq!(built.slug, "cobalt");
        assert_eq!(built.root_department_id, manifest.root_department_id);
        assert_eq!(built.departments.len(), 1, "the forest has exactly one root");
        assert_eq!(built.departments[0].id, manifest.root_department_id);
    }

    /// Every department the manifest names appears exactly once. A projection
    /// that dropped or duplicated a unit would render a company the operator
    /// does not have.
    #[test]
    fn every_department_appears_exactly_once() {
        let manifest = northstar_manifest(0);
        let built = tree(&manifest);

        let mut seen = Vec::new();
        fn walk(node: &DepartmentNode, seen: &mut Vec<String>) {
            seen.push(node.id.clone());
            for child in &node.children {
                walk(child, seen);
            }
        }
        for root in &built.departments {
            walk(root, &mut seen);
        }
        seen.sort();
        let mut expected = manifest.department_order.clone();
        expected.sort();

        assert_eq!(seen, expected);
    }

    /// Every person lands under their own department, and under exactly one.
    ///
    /// This restores placement coverage that was deleted along with its
    /// subject: the removed test hand-built a person whose
    /// assigned department differed from their home one and asserted the tree
    /// followed the assignment. That divergence is UNREPRESENTABLE now, so it
    /// cannot be manufactured to prove anything — but "a person appears where
    /// they work" is a live property of this projection and was left with no
    /// test at all. Asserted over the whole manifest rather than one person.
    #[test]
    fn places_every_person_under_their_assigned_department() {
        let manifest = northstar_manifest(0);
        let built = tree(&manifest);

        fn collect(node: &DepartmentNode, seen: &mut Vec<(String, String)>) {
            for person in &node.people {
                seen.push((person.id.clone(), node.id.clone()));
            }
            for child in &node.children {
                collect(child, seen);
            }
        }
        let mut placed = Vec::new();
        for root in &built.departments {
            collect(root, &mut placed);
        }

        assert_eq!(
            placed.len(),
            manifest.people_order.len(),
            "every person is placed exactly once, got {placed:?}"
        );
        for (person_id, department_id) in &placed {
            let person = &manifest.people[person_id];
            assert_eq!(
                department_id, &person.department_id,
                "{person_id} must show where they WORK"
            );
        }
    }

    /// An absent accent is omitted from the wire, not serialized as null — the
    /// client reads absence as "use the neutral accent". The shape is what is
    /// pinned here, not who is absent: the standard-identity exemption that
    /// used to put the chief in this arm is deleted, and only an exhausted
    /// palette can now.
    #[test]
    fn omits_the_accent_for_a_person_without_one() {
        let manifest = northstar_manifest(0);
        let built = tree(&manifest);
        let json = serde_json::to_string(&built).expect("serialize");

        assert!(!json.contains("\"accent\":null"), "absent accent must be omitted, got {json}");
    }

    /// Every person the tree places, paired with the employment state it
    /// reports for them.
    fn employment_states(built: &CompanyTree) -> Vec<(String, String)> {
        fn walk(node: &DepartmentNode, out: &mut Vec<(String, String)>) {
            for person in &node.people {
                out.push((person.id.clone(), person.employment_state.clone()));
            }
            for child in &node.children {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        for node in &built.departments {
            walk(node, &mut out);
        }
        out
    }

    /// A departed person is still IN the tree — the manifest keeps them, and
    /// dropping them here would be this projection deciding history — but the
    /// tree must SAY so. Without this field a departed person was byte-for-byte
    /// indistinguishable from an active one, so the rail offered Transfer and
    /// Offboard on somebody who had already left.
    #[test]
    fn carries_employment_state_so_a_departed_person_is_distinguishable() {
        use chiefd_core::store::organization::EmploymentState;

        let mut manifest = northstar_manifest(0);
        let departing = manifest.people_order.first().cloned().expect("fixture must have somebody");
        manifest.people.get_mut(&departing).expect("person").employment_state =
            EmploymentState::Departed;

        let states = employment_states(&tree(&manifest));

        let departed = states
            .iter()
            .find(|(id, _)| id == &departing)
            .expect("a departed person is still PLACED in the tree");
        assert_eq!(departed.1, "departed", "the tree must carry the departure");
        assert!(
            states.iter().any(|(id, state)| id != &departing && state == "active"),
            "and must still say ACTIVE for everybody else, got {states:?}"
        );
    }

    /// Lowercased for the wire, exactly like `kind`, so the TypeScript union
    /// (`'active' | 'benched' | 'departed'`) matches without a second mapping.
    #[test]
    fn serializes_employment_state_lowercased_under_a_camel_case_key() {
        let manifest = northstar_manifest(0);
        let json = serde_json::to_string(&tree(&manifest)).expect("serialize");

        assert!(json.contains("\"employmentState\":\"active\""), "got {json}");
        assert!(!json.contains("\"employmentState\":\"Active\""), "variant casing leaked: {json}");
    }
}
