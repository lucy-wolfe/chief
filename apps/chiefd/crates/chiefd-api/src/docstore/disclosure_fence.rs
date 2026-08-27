//! The read fence: how much of a company a caller may be TOLD about.
//!
//! Track B4 of the design record. The routes that publish the
//! organization's shape — the lifecycle board, both trees, the desired roster,
//! the unit subtree — answered every caller with the whole company. The
//! sharpest case was `/v1/org/lifecycle-status/read`, whose `scopeDepartmentId`
//! is an OPTIONAL, caller-supplied filter: omit it and one POST returns every
//! department and every person. That is a filter the caller CHOOSES, never a
//! fence the server APPLIES. This module derives the fence instead.
//!
//! # A read must never resolve a person
//!
//! This is the trap the module exists to avoid, and it is not hypothetical.
//! `authenticated_person_id` answers `None` for a `Service` identity, and the
//! resident actuator authenticates as a service and makes only READS. A read
//! armed with that helper would authenticate the actuator and then refuse it —
//! a fence that locks out the one caller it was never aimed at.
//!
//! So [`disclosure_fence`] asks a different question. It looks for a PERSON ROW
//! that the caller's principal names, and it fences only when it finds one.
//! A service, an operator, a channel that names nobody, and an absent caller
//! are all admitted unfenced; none of them is ever resolved to a person. This
//! is the same rule `org_ops::actor_names_a_person` already applies on the
//! mutation side (B1, #1093), stated once here for reads.
//!
//! The single exception is stated in [`disclosure_fence`]: a `Person`-kind
//! identity whose principal is NOT in the manifest is refused. A stale or
//! foreign person credential is the one case where a missing row must not be
//! read as a missing fence.
//!
//! # Why this is not `caller_person_to_authorize`
//!
//! The mutation side has a helper of the same family (`router.rs`), and the
//! two are deliberately NOT one function. That one keys on the identity KIND —
//! anything that is not `Person` is allowed through — and answers "which person
//! am I authorizing". This one keys on the PRINCIPAL — does it name a person
//! row — and answers "what is the widest subtree this caller may be told
//! about". Two consequences follow that the kind-keyed version cannot express,
//! and both are pinned by tests:
//!
//! * a `Channel` (an attested pi-pane) whose principal names a real person is
//!   FENCED as that person, so attesting a channel is not a way to widen a
//!   head into the whole company;
//! * a `Person` whose principal names nobody is REFUSED rather than allowed
//!   through, because on a read a missing row must not read as a missing
//!   fence.
//!
//! There is one funnel per side and every route in this packet goes through
//! this one. Three variants of one gate would drift; two gates that answer
//! two different questions do not.

use chiefd_core::store::control_authority::{department_is_within, disclosure_scope_department_id};
use chiefd_core::store::identities::{Identity, IdentityKind};
use chiefd_core::store::organization::OrganizationManifest;

use super::route_error::RouteError;

/// The stable refusal code every disclosure fence answers with.
///
/// One code across every route in the packet, so a client can recognise "you
/// asked past your subtree" without matching prose. The DETAIL names the
/// subtree — never a job title (2026-08-13 ruling): authority is the tree you
/// head, so a refusal that said "head-level" would be describing a gate that
/// does not exist.
pub(super) const CALLER_OUT_OF_SCOPE: &str = "caller-out-of-scope";

/// The widest department a caller may be told about, or `None` for no fence.
///
/// `None` is NOT "refused" — it is "this caller is not a person in this
/// company's manifest, so there is no subtree to fence it to". The service
/// identity the resident actuator uses lands here, and that is the point.
///
/// # Errors
///
/// Only for a `Person`-kind identity whose principal the manifest does not
/// have. Every other caller shape either fences or passes.
pub(super) fn disclosure_fence(
    identity: &Identity,
    manifest: &OrganizationManifest,
) -> Result<Option<String>, RouteError> {
    match disclosure_scope_department_id(manifest, &identity.principal) {
        Some(department_id) => Ok(Some(department_id.to_owned())),
        None if identity.kind == IdentityKind::Person => Err(RouteError::refused(
            CALLER_OUT_OF_SCOPE,
            format!(
                "caller '{}' has no department in this company, so no part of it can be disclosed",
                identity.principal
            ),
        )),
        // A service, an operator, or a channel that names nobody. Never
        // resolved to a person, never fenced.
        None => Ok(None),
    }
}

/// Refuse a caller that named a department outside its own fence.
///
/// The detail names both the requested unit and the subtree the caller does
/// hold, so the caller can tell a wrong ask from a missing one.
pub(super) fn out_of_scope(fence: &str, requested_department_id: &str) -> RouteError {
    RouteError::refused(
        CALLER_OUT_OF_SCOPE,
        format!(
            "department '{requested_department_id}' is outside the subtree '{fence}' this caller may be told about"
        ),
    )
}

/// Resolve the department a fenced read should answer for.
///
/// * no fence — the caller's own optional filter, exactly as before;
/// * a fence and no filter — the fence, so an omitted filter narrows to the
///   caller instead of widening to the company;
/// * a fence and a filter inside it — the filter, because a caller may always
///   narrow further than its own subtree;
/// * a fence and a filter outside it — [`out_of_scope`].
///
/// # Errors
///
/// [`CALLER_OUT_OF_SCOPE`] for the last case.
pub(super) fn fenced_department(
    fence: Option<String>,
    requested: Option<&str>,
    manifest: &OrganizationManifest,
) -> Result<Option<String>, RouteError> {
    let Some(fence) = fence else {
        return Ok(requested.map(str::to_owned));
    };
    match requested {
        None => Ok(Some(fence)),
        Some(requested) if department_is_within(manifest, &fence, requested) => {
            Ok(Some(requested.to_owned()))
        }
        Some(requested) => Err(out_of_scope(&fence, requested)),
    }
}

/// Admit or refuse a read whose TARGET is a named department.
///
/// The unit routes take their subject out of the body, so there is nothing to
/// narrow — the request either names a unit inside the caller's fence or it
/// does not.
///
/// # Errors
///
/// [`CALLER_OUT_OF_SCOPE`] when the caller has a fence and the unit is outside
/// it, or when the caller is a `Person` the manifest does not have.
pub(super) fn require_department(
    caller: &Identity,
    manifest: &OrganizationManifest,
    department_id: &str,
) -> Result<(), RouteError> {
    let Some(fence) = disclosure_fence(caller, manifest)? else {
        return Ok(());
    };
    if department_is_within(manifest, &fence, department_id) {
        return Ok(());
    }
    Err(out_of_scope(&fence, department_id))
}

/// The manifest as this caller may be told about it, or `None` for "unfenced,
/// use the real one".
///
/// # Why a NARROWED MANIFEST rather than a filtered response
///
/// The tree and roster projections already know how to render a manifest, and
/// they are the product contract — a second, fence-aware copy of each
/// projection would be two statements of one rule, free to drift. So the fence
/// is applied to the INPUT: the departments outside it are dropped, the fence
/// unit becomes the root and loses its parent, and the orders are filtered to
/// stay bijections with the maps they index. What comes back is a manifest of
/// the same shape, and every existing projection renders it unchanged.
///
/// A person is kept when their unit survived. There is ONE unit to ask about
/// since #1099 collapsed `home`/`assigned` into `department_id` — membership
/// and placement were one fact recorded twice — so the question "does this
/// person belong inside the fence" and the question "would a tree place them
/// inside it" can no longer disagree.
pub(super) fn narrowed_manifest(
    manifest: &OrganizationManifest,
    fence: Option<&str>,
) -> Option<OrganizationManifest> {
    let fence = fence?;
    let mut narrowed = manifest.clone();
    narrowed.departments.retain(|id, _| department_is_within(manifest, fence, id));
    if let Some(root) = narrowed.departments.get_mut(fence) {
        root.parent_department_id = None;
    }
    fence.clone_into(&mut narrowed.root_department_id);

    let kept_departments: std::collections::BTreeSet<String> =
        narrowed.departments.keys().cloned().collect();
    narrowed.department_order.retain(|id| kept_departments.contains(id));
    narrowed.people.retain(|_, person| kept_departments.contains(&person.department_id));
    let kept_people: std::collections::BTreeSet<String> = narrowed.people.keys().cloned().collect();
    narrowed.people_order.retain(|id| kept_people.contains(id));
    Some(narrowed)
}
