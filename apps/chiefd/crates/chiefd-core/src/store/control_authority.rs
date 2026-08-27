//! Hierarchy scope — who may act on whom (#322).
//!
//! Pure predicates over a manifest; no I/O, no clock, no store. Every rule is
//! a product contract carried over from the launcher's TypeScript
//! control-authority module, which this replaced and which no longer exists.
//!
//! # The walk goes UP, never down
//!
//! A head's reach is "my headed unit and everything beneath it", and the only
//! safe way to decide that is to walk from the *target* upward looking for the
//! actor's headed unit. Walking down from the actor's unit and collecting a
//! set looks equivalent and is not: a damaged manifest with a cycle turns the
//! downward collection into an unbounded set, and a target whose ancestry was
//! reparented mid-read can appear in the collected set without actually being
//! beneath the actor. Upward with a visited-set terminates on every manifest
//! shape and can only ever answer "yes" by producing the actual chain.
//!
//! # A worker is in scope of nobody but themselves
//!
//! Workers head no unit, so [`department_is_in_scope`] is false for every
//! worker actor. That is the invariant behind "only a manager may act on
//! someone other than themselves" — it falls out of the structure rather than
//! being a separate rule that could drift.

use std::collections::BTreeSet;

use crate::store::organization::{OrganizationManifest, PersonKind};

/// Who is asking.
///
/// An operator has no manifest entry and full scope by construction — it is the
/// human at the launcher, authenticated by its own operator credential before
/// it ever reaches this predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlActor {
    /// The human operator: unconditional scope.
    Operator,
    /// A pane-attested person.
    Person(String),
}

/// The unit `manager_person_id` heads, if any.
///
/// Resolved through `department_order` so a damaged manifest with two units
/// naming the same head answers deterministically rather than by map order.
#[must_use]
pub fn headed_department_id<'m>(
    manifest: &'m OrganizationManifest,
    manager_person_id: &str,
) -> Option<&'m str> {
    manifest
        .department_order
        .iter()
        .filter_map(|id| manifest.departments.get(id))
        .find(|department| department.head_person_id == manager_person_id)
        .map(|department| department.id.as_str())
}

/// Whether `department_id` is `root_department_id` itself or a unit beneath it.
///
/// The containment half of [`department_is_in_scope`], with no actor in it.
/// [`department_is_in_scope`] resolves an actor to a root and then asks exactly
/// this question; the disclosure fence
/// ([`disclosure_scope_department_id`]) resolves a root a different way and
/// asks the same one, so the walk lives here once rather than being copied.
///
/// The walk goes UP from the target, for the reason in the module docs: a
/// downward collection is unbounded on a manifest with a cycle, and this one
/// terminates on every manifest shape because a visited-set stops it.
#[must_use]
pub fn department_is_within(
    manifest: &OrganizationManifest,
    root_department_id: &str,
    department_id: &str,
) -> bool {
    if !manifest.departments.contains_key(department_id) {
        return false;
    }
    let mut cursor = manifest.departments.get(department_id);
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    while let Some(unit) = cursor {
        if unit.id == root_department_id {
            return true;
        }
        if !seen.insert(unit.id.as_str()) {
            return false;
        }
        let Some(parent) = unit.parent_department_id.as_deref() else {
            return false;
        };
        cursor = manifest.departments.get(parent);
    }
    false
}

/// Whether `actor` manages `department_id`.
///
/// An operator manages every unit unconditionally. An executive (the CEO)
/// manages the whole company unconditionally. A head manages their own headed
/// unit and every unit beneath it, walked from the target up to the root —
/// never down, so a head can never claim scope over a sibling or a parent unit.
/// A worker heads no unit and so manages nothing.
///
/// # Why the actor is a [`ControlActor`] and never a bare person id
///
/// A daemon-scoped principal — the operator, the service, a channel of the
/// operator — has NO `people` row and can never acquire one:
/// `enroll_bootstrap_operator` enrols it with a NULL company slug, and `chiefd
/// new` creates a company before any person exists, which is why genesis
/// authenticates as the operator. Handed such a principal as a string, this
/// predicate looks it up in `manifest.people`, misses, and answers `false` —
/// refusing the operator for a reason no refusal names. A `&str` parameter
/// invites exactly that call, so the parameter is the actor kind and the
/// [`ControlActor::Operator`] arm answers before any lookup. The mapping a
/// caller owes this predicate is the one `bind_requester_to_caller` states in
/// prose: `IdentityKind::Person` is a [`ControlActor::Person`], and
/// `Operator`/`Service`/`Channel` are all [`ControlActor::Operator`], which may
/// act AS the operator and may never impersonate a person.
///
/// A department that does not exist is in scope of nobody, the operator
/// included — the same shape as [`person_is_in_scope`], which answers `false`
/// for a target the manifest does not have even when the actor is the operator.
///
/// # Its relative on the rows side
///
/// `store::org_ops::actor_names_a_person` asks the same subtree question of a
/// different corpus — free-form audit prose inside a SQL transaction rather
/// than an authenticated principal — and its caller `actor_out_of_scope`
/// composes it with the OPPOSITE default: an actor naming nobody is never out
/// of scope there, where an unresolvable person is `false` here.
///
/// Both defaults are correct for their corpus, and unifying them would mean
/// choosing which one survives for the other. Read that function's doc-comment
/// before treating the pair as an inconsistency: they are relatives kept apart
/// on purpose.
#[must_use]
pub fn department_is_in_scope(
    manifest: &OrganizationManifest,
    actor: &ControlActor,
    department_id: &str,
) -> bool {
    if !manifest.departments.contains_key(department_id) {
        return false;
    }
    let actor_person_id = match actor {
        ControlActor::Operator => return true,
        ControlActor::Person(id) => id.as_str(),
    };
    let Some(person) = manifest.people.get(actor_person_id) else {
        return false;
    };
    if person.kind == PersonKind::Executive {
        return true;
    }
    let Some(root) = headed_department_id(manifest, actor_person_id) else {
        return false;
    };
    department_is_within(manifest, root, department_id)
}

/// The unit a person may be TOLD ABOUT: the widest subtree a read may disclose
/// to them, or `None` when the manifest has no such person.
///
/// # Why this is not `headed_department_id`
///
/// Structural authority and disclosure are different questions and this is
/// deliberately the looser of the two. `department_is_in_scope` answers "may
/// this actor ACT on that unit", and a worker heads nothing, so it answers no
/// for every unit — correct for a mutation and useless as a read fence,
/// because it would leave a worker unable to see the department they work in.
///
/// So the disclosure root is the unit you HEAD, and the unit you LIVE IN when
/// you head none. The CEO is an executive and heads the root department, so its
/// disclosure root is the root department and a CEO-scoped read legitimately
/// covers the whole company — that is the model, not an exemption.
///
/// This asks nothing about a job title. It reads the tree, which is where
/// authority lives (2026-08-13 ruling).
#[must_use]
pub fn disclosure_scope_department_id<'m>(
    manifest: &'m OrganizationManifest,
    person_id: &str,
) -> Option<&'m str> {
    let person = manifest.people.get(person_id)?;
    if person.kind == PersonKind::Executive {
        return Some(manifest.root_department_id.as_str());
    }
    if let Some(headed) = headed_department_id(manifest, person_id) {
        return Some(headed);
    }
    manifest.departments.get(&person.department_id).map(|department| department.id.as_str())
}

/// Whether `actor` may act on `target_person_id`.
///
/// An operator may always. A person may always target themselves (self-service
/// verbs need no management scope) and otherwise needs
/// [`department_is_in_scope`] over the target's **home** unit — the permanent
/// membership, which is now the only kind there is, so nothing moves who
/// manages someone.
#[must_use]
pub fn person_is_in_scope(
    manifest: &OrganizationManifest,
    actor: &ControlActor,
    target_person_id: &str,
) -> bool {
    let Some(target) = manifest.people.get(target_person_id) else {
        return false;
    };
    match actor {
        // Answered here rather than left to `department_is_in_scope`, which
        // would additionally require the target's home unit to exist: the
        // operator reaches every person the manifest HAS, whatever shape the
        // rest of the manifest is in.
        ControlActor::Operator => return true,
        ControlActor::Person(actor_person_id) if actor_person_id == target_person_id => {
            // Self-service verbs need no management scope.
            return true;
        }
        ControlActor::Person(_) => {}
    }
    department_is_in_scope(manifest, actor, &target.department_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::northstar_manifest;

    fn manifest() -> OrganizationManifest {
        northstar_manifest(1_700_000_000_000)
    }

    fn person(person_id: &str) -> ControlActor {
        ControlActor::Person(person_id.to_string())
    }

    #[test]
    fn a_head_is_found_through_department_order() {
        let m = manifest();
        assert_eq!(headed_department_id(&m, "quant-head"), Some("quant"));
        assert_eq!(headed_department_id(&m, "signal-researcher"), None);
    }

    #[test]
    fn the_executive_manages_every_unit() {
        let m = manifest();
        assert!(department_is_in_scope(&m, &person("chief"), "quant"));
        assert!(department_is_in_scope(&m, &person("chief"), "it"));
        assert!(department_is_in_scope(&m, &person("chief"), &m.root_department_id));
    }

    #[test]
    fn a_head_manages_its_own_unit_and_not_a_sibling() {
        let m = manifest();
        assert!(department_is_in_scope(&m, &person("quant-head"), "quant"));
        assert!(!department_is_in_scope(&m, &person("quant-head"), "it"));
        assert!(!department_is_in_scope(&m, &person("quant-head"), &m.root_department_id));
    }

    #[test]
    fn a_worker_manages_nothing() {
        let m = manifest();
        assert!(!department_is_in_scope(&m, &person("signal-researcher"), "quant"));
    }

    #[test]
    fn an_unknown_unit_is_never_in_scope() {
        let m = manifest();
        assert!(!department_is_in_scope(&m, &person("chief"), "nope"));
    }

    /// THE REASON THIS PREDICATE TAKES A `ControlActor`. A daemon-scoped
    /// principal — the operator, the service, a channel — has no `people` row
    /// and can never have one, so it is in scope of every unit by construction
    /// rather than by lookup. Given a bare person id this arm could not exist:
    /// the operator would miss the `people` map and be refused silently.
    #[test]
    fn the_operator_manages_every_unit_without_a_person_row() {
        let m = manifest();
        assert!(!m.people.contains_key("operator"), "the operator is not a person and never is");
        assert!(department_is_in_scope(&m, &ControlActor::Operator, "quant"));
        assert!(department_is_in_scope(&m, &ControlActor::Operator, "it"));
        assert!(department_is_in_scope(&m, &ControlActor::Operator, &m.root_department_id));
    }

    /// The operator's scope is unconditional over units that EXIST. A unit the
    /// manifest does not have is in scope of nobody — the same shape
    /// `person_is_in_scope` has for a target the manifest does not have.
    #[test]
    fn even_the_operator_does_not_manage_a_unit_that_does_not_exist() {
        let m = manifest();
        assert!(!department_is_in_scope(&m, &ControlActor::Operator, "nope"));
    }

    /// The other half of the same rule: a PERSON actor still has to be a
    /// person. A stale or foreign person credential names nobody and manages
    /// nothing, and the operator arm must not have widened that.
    #[test]
    fn a_person_actor_the_manifest_does_not_have_manages_nothing() {
        let m = manifest();
        assert!(!department_is_in_scope(&m, &person("stranger"), "quant"));
        assert!(!department_is_in_scope(&m, &person("stranger"), &m.root_department_id));
    }

    #[test]
    fn the_operator_reaches_every_person_but_not_a_missing_one() {
        let m = manifest();
        assert!(person_is_in_scope(&m, &ControlActor::Operator, "signal-researcher"));
        assert!(!person_is_in_scope(&m, &ControlActor::Operator, "nobody"));
    }

    #[test]
    fn a_person_always_reaches_themselves() {
        let m = manifest();
        assert!(person_is_in_scope(&m, &person("signal-researcher"), "signal-researcher"));
    }

    #[test]
    fn a_worker_never_reaches_a_peer() {
        let m = manifest();
        assert!(!person_is_in_scope(&m, &person("signal-researcher"), "quant-head"));
    }

    #[test]
    fn a_head_reaches_its_own_report_only() {
        let m = manifest();
        assert!(person_is_in_scope(&m, &person("quant-head"), "signal-researcher"));
        assert!(!person_is_in_scope(&m, &person("quant-head"), "it-head"));
    }

    /// A stale person credential reaches nobody but is not thereby the
    /// operator: `person_is_in_scope` and `department_is_in_scope` agree.
    #[test]
    fn a_person_the_manifest_does_not_have_reaches_nobody() {
        let m = manifest();
        assert!(!person_is_in_scope(&m, &person("stranger"), "signal-researcher"));
    }

    // --- the disclosure fence (B4) -------------------------------------

    #[test]
    fn containment_is_reflexive_downward_and_never_sideways_or_upward() {
        let m = manifest();
        assert!(department_is_within(&m, "quant", "quant"), "a unit contains itself");
        assert!(department_is_within(&m, &m.root_department_id, "quant"), "the root contains all");
        assert!(!department_is_within(&m, "quant", "it"), "a sibling is never contained");
        assert!(!department_is_within(&m, "quant", &m.root_department_id), "never upward");
        assert!(!department_is_within(&m, "quant", "nope"), "an unknown unit is contained by none");
    }

    #[test]
    fn the_disclosure_root_of_the_ceo_is_the_root_department() {
        let m = manifest();
        assert_eq!(
            disclosure_scope_department_id(&m, "chief"),
            Some(m.root_department_id.as_str())
        );
    }

    #[test]
    fn the_disclosure_root_of_a_head_is_the_unit_it_heads() {
        let m = manifest();
        assert_eq!(disclosure_scope_department_id(&m, "quant-head"), Some("quant"));
        assert_eq!(disclosure_scope_department_id(&m, "it-head"), Some("it"));
    }

    /// THE CASE THAT SEPARATES DISCLOSURE FROM AUTHORITY. `department_is_in_scope`
    /// answers no for every unit here, because a worker heads none — correct for
    /// a mutation and useless as a read fence. A worker sees the unit they live
    /// in, and nothing above or beside it.
    #[test]
    fn the_disclosure_root_of_a_worker_is_the_unit_it_lives_in() {
        let m = manifest();
        assert!(!department_is_in_scope(&m, &person("signal-researcher"), "quant"));
        assert_eq!(disclosure_scope_department_id(&m, "signal-researcher"), Some("quant"));
        assert!(!department_is_within(&m, "quant", "it"), "a worker never sees a sibling unit");
        assert!(
            !department_is_within(&m, "quant", &m.root_department_id),
            "a worker never sees the whole company"
        );
    }

    /// A stale or foreign person credential has NO fence, and the caller of this
    /// predicate must read `None` as "refuse", never as "no filter".
    #[test]
    fn a_person_the_manifest_does_not_have_gets_no_disclosure_root() {
        let m = manifest();
        assert_eq!(disclosure_scope_department_id(&m, "stranger"), None);
    }
}
