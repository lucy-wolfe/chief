//! The PURE projection of a staffing request onto a manifest.
//!
//! One definition of what "may this department be created" and "may this
//! person be hired" mean, expressed as a total function from the current
//! manifest plus the request to either the manifest AS IT WOULD BE or the
//! exact refusal. Nothing here reads or writes SQL, so a caller can ask the
//! question BEFORE committing anything.
//!
//! # Why this exists
//!
//! Materialization — the pass that builds a person's home on disk and is the
//! only place a bad resource selection is discovered — ran AFTER the durable
//! commit. A person whose seed could not materialize was therefore already
//! hired, un-buildable, parked, and holding their id, so the corrected retry
//! hit `duplicate-person-id` and the company was stuck. The fix is to
//! materialize a PROJECTED manifest into a staging directory first and commit
//! only if that build succeeds.
//!
//! # Why the writers call it too
//!
//! [`crate::store::org_ops::create_department_with_staff_unit`] and
//! [`crate::store::org_ops::hire_person_authorized`] obtain their eligibility
//! verdict from these functions rather than re-deriving it from rows. A second
//! hand-rolled copy of the rules in the writer is free to drift from the copy
//! the caller preflights against, which is exactly the failure this module
//! exists to remove. The row accessors these functions replace
//! (`person_placement`, `person_kind`, `department_headed_by_person`,
//! `person_manages_department`, `person_may_create_under_department`,
//! `department_state`, `executive_root_unit_ids`) were all READS, and
//! `organization_rows::reconstruct` builds the manifest from those same rows,
//! so the port is a change of source, not of meaning. The existing org_ops
//! test suite is the oracle for that claim.

use std::collections::{BTreeMap, HashSet};

use super::org_ops::{echo, valid_entity_id};
use super::org_ops::{
    validate_new_person_seed, CreateDepartmentRefusal, DepartmentCreateUnit, DepartmentStaffSeed,
    HeadDecision, HeadIneligibility, HeadVacancy, HeadVacancyRefusal, HireRefusal, NewPersonSeed,
    OwnedNewPersonSeed, VacancyRefusal, ENTITY_ID_RULE,
};
use super::organization::{
    DepartmentRecord, EmploymentState, OrganizationManifest, PersonKind, PersonRecord, UnitKind,
    UnitState,
};
use crate::isotime::parse_iso_millis;

// ---------------------------------------------------------------------------
// The eligibility VIEW
// ---------------------------------------------------------------------------

/// One unit, reduced to what an eligibility decision depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitView {
    /// The parent unit; absent only on the root.
    pub parent_department_id: Option<String>,
    /// Who heads it.
    pub head_person_id: String,
    /// Active or paused.
    pub state: UnitState,
}

/// One person, reduced to what an eligibility decision depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonView {
    /// Structural role.
    pub kind: PersonKind,
    /// Employment state.
    pub employment_state: EmploymentState,
    /// Where they belong, which is also where they work.
    pub department_id: String,
}

/// The company shape an eligibility decision reads, and NOTHING else.
///
/// Narrower than the manifest on purpose. It is what lets the writer inside a
/// transaction and the route holding a manifest ask the SAME function the same
/// question: the writer builds this from the structural row columns, the route
/// builds it from the manifest it already has, and neither needs a person's
/// resource lists, model fields, or audit history to decide.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OrgView {
    /// Units by id.
    pub departments: BTreeMap<String, UnitView>,
    /// People by id.
    pub people: BTreeMap<String, PersonView>,
}

impl OrgView {
    /// The view of a manifest the route already holds.
    #[must_use]
    pub fn from_manifest(manifest: &OrganizationManifest) -> Self {
        Self {
            departments: manifest
                .departments
                .iter()
                .map(|(id, department)| {
                    (
                        id.clone(),
                        UnitView {
                            parent_department_id: department.parent_department_id.clone(),
                            head_person_id: department.head_person_id.clone(),
                            state: department.state,
                        },
                    )
                })
                .collect(),
            people: manifest
                .people
                .iter()
                .map(|(id, person)| {
                    (
                        id.clone(),
                        PersonView {
                            kind: person.kind,
                            employment_state: person.employment_state,
                            department_id: person.department_id.clone(),
                        },
                    )
                })
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure view reads — the twins of the `organization_rows` accessors
// ---------------------------------------------------------------------------

/// `organization_rows::department_state`.
fn department_state(view: &OrgView, department_id: &str) -> Option<UnitState> {
    view.departments.get(department_id).map(|department| department.state)
}

/// `org_ops::department_or_ancestor_is_paused`. An unknown unit, a non-active
/// unit, and a cycle all answer true, exactly as the row walk does.
fn department_or_ancestor_is_paused(view: &OrgView, department_id: &str) -> bool {
    let mut cursor = Some(department_id.to_owned());
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) {
            return true;
        }
        let Some(department) = view.departments.get(&id) else {
            return true;
        };
        if department.state != UnitState::Active {
            return true;
        }
        cursor = department.parent_department_id.clone();
    }
    false
}

/// `organization_rows::department_headed_by_person`.
fn department_headed_by_person<'a>(view: &'a OrgView, person_id: &str) -> Option<&'a str> {
    view.departments
        .iter()
        .find(|(_, department)| department.head_person_id == person_id)
        .map(|(id, _)| id.as_str())
}

/// Active members of `department_id` other than `except`, in a stable order.
///
/// Home AND assigned are both tested. Today only a loan can make the two
/// disagree, and the loan concept is being deleted — but the pair is what makes
/// somebody a real member of a department rather than a visitor, so testing
/// both keeps this correct whichever way that lands.
///
/// This is the ONE definition of "who could take this department over". Both
/// vacancy call sites read it: the refusal that lists candidates, and the
/// check that validates the successor a caller named. A second copy is how a
/// refusal comes to advertise a successor the writer then rejects.
fn unit_members_other_than(view: &OrgView, department_id: &str, except: &str) -> Vec<String> {
    view.people
        .iter()
        .filter(|(person_id, person)| {
            person_id.as_str() != except
                && person.employment_state != EmploymentState::Departed
                && person.department_id == department_id
        })
        .map(|(person_id, _)| person_id.clone())
        .collect()
}

/// The child units of `department_id`, in a stable order.
fn child_department_ids(view: &OrgView, department_id: &str) -> Vec<String> {
    view.departments
        .iter()
        .filter(|(_, unit)| unit.parent_department_id.as_deref() == Some(department_id))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Whether this person may leave the department they head, given the decision
/// the caller supplied about it.
///
/// The ONE statement of the rule, shared by department create and person
/// transfer. Both verbs move a head out of the department they lead, both leave
/// it without one, and writing the answer twice is how two statements of a
/// single rule drift apart.
///
/// `heads_now` is `None` for the ordinary case — somebody who leads nothing
/// vacates nothing — and a decision supplied for such a person is itself
/// refused, because it means the caller has the wrong person in mind.
///
/// # Errors
/// The exact refusal, naming the department and the way through.
pub(crate) fn check_head_vacancy(
    view: &OrgView,
    heads_now: Option<&str>,
    person_id: &str,
    decision: Option<&HeadVacancy>,
) -> Result<(), HeadVacancyRefusal> {
    match (heads_now, decision) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(HeadVacancyRefusal::Invalid {
            department_id: String::new(),
            because: VacancyRefusal::HeadsNothing,
        }),
        (Some(vacated), None) => Err(HeadVacancyRefusal::Required {
            person_id: person_id.to_string(),
            department_id: vacated.to_owned(),
            eligible_successor_ids: unit_members_other_than(view, vacated, person_id),
        }),
        (Some(vacated), Some(HeadVacancy::HandOver { successor_person_id })) => {
            // One predicate covers exists, not-the-outgoing-head, not-departed
            // and really-a-member. Spelling those out separately here would be
            // a second copy of `unit_members_other_than`'s rule.
            if unit_members_other_than(view, vacated, person_id)
                .iter()
                .any(|candidate| candidate == successor_person_id)
            {
                return Ok(());
            }
            Err(HeadVacancyRefusal::Invalid {
                department_id: vacated.to_owned(),
                because: VacancyRefusal::SuccessorNotAMember {
                    successor_person_id: successor_person_id.clone(),
                },
            })
        }
        (Some(vacated), Some(HeadVacancy::Dissolve)) => {
            let members = unit_members_other_than(view, vacated, person_id);
            if !members.is_empty() {
                return Err(HeadVacancyRefusal::Invalid {
                    department_id: vacated.to_owned(),
                    because: VacancyRefusal::StillHasMembers { member_person_ids: members },
                });
            }
            let children = child_department_ids(view, vacated);
            if !children.is_empty() {
                return Err(HeadVacancyRefusal::Invalid {
                    department_id: vacated.to_owned(),
                    because: VacancyRefusal::StillHasChildren { child_department_ids: children },
                });
            }
            Ok(())
        }
    }
}

/// `organization_rows::person_manages_department`. Executives reach the whole
/// company; a head reaches the unit it heads and that unit's descendants;
/// workers, departed people, missing people, disconnected trees and cycles are
/// all out of scope.
fn person_manages_department(
    view: &OrgView,
    requester_person_id: &str,
    department_id: &str,
) -> bool {
    let Some(requester) = view.people.get(requester_person_id) else {
        return false;
    };
    if requester.employment_state == EmploymentState::Departed {
        return false;
    }
    if requester.kind == PersonKind::Executive {
        return true;
    }
    if requester.kind != PersonKind::Head {
        return false;
    }
    let Some(managed_root) = department_headed_by_person(view, requester_person_id) else {
        return false;
    };
    let mut cursor = Some(department_id.to_owned());
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(id) = cursor {
        if id == managed_root {
            return true;
        }
        if !seen.insert(id.clone()) {
            return false;
        }
        cursor = view.departments.get(&id).and_then(|d| d.parent_department_id.clone());
    }
    false
}

/// `organization_rows::person_may_create_under_department` — deliberately more
/// permissive than management scope, and only for creation: a leaf may grow a
/// unit beneath the unit it sits in, because nothing that already exists
/// changes hands.
fn person_may_create_under_department(
    view: &OrgView,
    requester_person_id: &str,
    parent_department_id: &str,
) -> bool {
    if person_manages_department(view, requester_person_id, parent_department_id) {
        return true;
    }
    let Some(requester) = view.people.get(requester_person_id) else {
        return false;
    };
    if requester.employment_state == EmploymentState::Departed {
        return false;
    }
    // A head's authority root is the unit it heads, and that case already
    // answered true above. What is left is the leaf case: the unit it sits in.
    if department_headed_by_person(view, requester_person_id).is_some() {
        return false;
    }
    requester.department_id == parent_department_id
}

/// `org_ops::is_ceo` — the head of the root department, and nobody else.
///
/// This used to be `is_executive_root_protected`, asking whether the person's
/// home OR assigned unit was anywhere in the executive-root set — a question
/// with one answer now, and the wrong question either way. That froze a
/// Chief of Staff hired into the root department: he could not be made the head
/// of a new unit beneath it, and the refusal said only that he was
/// "executive-root protected". Operator ruling, 2026-08-13 (`AGENTS.md`): the
/// CEO is the only immovable node, everyone else is fluid.
fn is_ceo(view: &OrgView, person_id: &str) -> bool {
    view.departments
        .values()
        .find(|department| department.parent_department_id.is_none())
        .is_some_and(|root| root.head_person_id == person_id)
}

// ---------------------------------------------------------------------------
// Proposals
// ---------------------------------------------------------------------------

/// Everything `create_department_with_staff_unit` decides from.
pub struct DepartmentCreateProposal<'a> {
    /// The new unit's id.
    pub department_id: &'a str,
    /// The unit it hangs beneath.
    pub parent_id: &'a str,
    /// Display name.
    pub name: &'a str,
    /// What it exists to do.
    pub purpose: &'a str,
    /// Appoint-existing or hire-new (R3).
    pub head: &'a HeadDecision,
    /// Initial workers.
    pub staff: &'a [DepartmentStaffSeed],
    /// The typed unit kind and its contract metadata.
    pub unit: &'a DepartmentCreateUnit,
    /// Who asked, when the caller attests one.
    pub requester_person_id: Option<&'a str>,
    /// The staffing-history line for this create. The route composes it; a
    /// caller is never asked to justify a structural change, so it is NOT
    /// checked for blankness here.
    pub audit_reason: &'a str,
    /// The caller's ISO-8601 clock stamp.
    pub at: &'a str,
    /// What becomes of the department an appoint-existing head already heads.
    /// `None` is the ordinary case AND a refusable one: absent when the
    /// appointee heads something is exactly what
    /// [`CreateDepartmentRefusal::VacancyDecisionRequired`] answers.
    pub head_vacates: Option<&'a HeadVacancy>,
}

/// Everything `hire_person_authorized` decides from.
pub struct HireProposal<'a> {
    /// The new person's id.
    pub person_id: &'a str,
    /// Where they land; home and assigned both.
    pub department_id: &'a str,
    /// Their seed.
    pub seed: &'a NewPersonSeed<'a>,
    /// Who asked, when the caller attests one.
    pub requester_person_id: Option<&'a str>,
    /// The caller's ISO-8601 clock stamp.
    pub at: &'a str,
}

// ---------------------------------------------------------------------------
// Projections
// ---------------------------------------------------------------------------

/// Whether this department create is eligible, or the exact refusal. PURE —
/// this is the ONE definition of the rules, called both by the writer inside
/// its transaction and by the route that preflights before committing.
///
/// The guard ORDER is contractual — it is what decides which of several
/// applicable refusals a caller sees — and is preserved exactly:
/// contract-unit metadata -> `unknown-parent` -> `parent-paused` ->
/// `duplicate-department-id` -> `requester-out-of-scope` -> blank fields ->
/// `departmentId` shape -> the head decision -> the initial roster.
///
/// # Errors
/// The refusal the caller must surface.
pub fn check_department_create(
    view: &OrgView,
    proposal: &DepartmentCreateProposal<'_>,
) -> Result<(), CreateDepartmentRefusal> {
    let invalid = |field: &str, detail: String| CreateDepartmentRefusal::InvalidSeed {
        field: field.to_owned(),
        detail,
    };

    if let DepartmentCreateUnit::Contract(transient) = proposal.unit {
        if transient.engagement.trim().is_empty() {
            return Err(invalid(
                "unit.transient.engagement",
                "a contract unit requires a non-blank engagement description".to_owned(),
            ));
        }
        let Some(launched_at) = parse_iso_millis(&transient.launched_at) else {
            return Err(invalid(
                "unit.transient.launchedAt",
                format!(
                    "launchedAt must be an ISO-8601 millisecond timestamp \
                     (YYYY-MM-DDTHH:MM:SS.sssZ); got {}",
                    echo(&transient.launched_at)
                ),
            ));
        };
        if transient.expires_at.as_deref().is_some_and(|expires_at| {
            parse_iso_millis(expires_at).is_none_or(|expires| expires <= launched_at)
        }) {
            return Err(invalid(
                "unit.transient.expiresAt",
                "expiresAt must be omitted, or an ISO-8601 millisecond timestamp \
                 strictly after launchedAt"
                    .to_owned(),
            ));
        }
    }
    // unknown-parent / parent-paused: the parent AND every ancestor must be
    // active. Naming an active child below a paused ancestor does not bypass
    // the subtree rule.
    match department_state(view, proposal.parent_id) {
        None => return Err(CreateDepartmentRefusal::UnknownParent),
        Some(UnitState::Paused) => return Err(CreateDepartmentRefusal::ParentPaused),
        Some(UnitState::Active) => {}
    }
    if department_or_ancestor_is_paused(view, proposal.parent_id) {
        return Err(CreateDepartmentRefusal::ParentPaused);
    }
    if department_state(view, proposal.department_id).is_some() {
        return Err(CreateDepartmentRefusal::DuplicateDepartmentId);
    }
    if let Some(requester_person_id) = proposal.requester_person_id {
        // CREATION authority, not management authority: every leaf may grow a
        // unit beneath itself, because nothing that exists changes hands.
        if !person_may_create_under_department(view, requester_person_id, proposal.parent_id) {
            return Err(CreateDepartmentRefusal::RequesterOutOfScope);
        }
    }
    for (value, field) in [
        (proposal.department_id, "departmentId"),
        (proposal.name, "name"),
        (proposal.purpose, "purpose"),
    ] {
        if value.trim().is_empty() {
            return Err(invalid(field, format!("{field} is required and must not be blank")));
        }
    }
    if !valid_entity_id(proposal.department_id) {
        return Err(invalid(
            "departmentId",
            format!("{ENTITY_ID_RULE}; got {}", echo(proposal.department_id)),
        ));
    }
    // The head decision (R3).
    match proposal.head {
        HeadDecision::AppointExisting { person_id } => {
            // The appointee must be a real person (else the decision is empty).
            let Some(appointee) = view.people.get(person_id.as_str()) else {
                return Err(CreateDepartmentRefusal::HeadDecisionRequired);
            };
            // ...and must not be the CEO: appointing an existing person as a
            // head MOVES them into the department they now head, and the CEO
            // never moves. ONE PERSON, not a region — this comment said "the
            // CEO / office-of-the-ceo staff ... out of the reserved root"
            // while the code below already asked `is_ceo` alone, which is the
            // retired model surviving in prose next to the correct code.
            // Anybody else homed in the executive root, chief of staff
            // included, may be appointed here (AGENTS.md, 2026-08-13).
            if is_ceo(view, person_id) {
                return Err(CreateDepartmentRefusal::ExecRootProtected {
                    person_id: person_id.to_string(),
                });
            }
            if let Some(requester_person_id) = proposal.requester_person_id {
                // A person always reaches THEMSELVES — the same rule
                // `person_is_in_scope` states. This is how a leaf heads the
                // unit it just created: it appoints itself, moving nobody else.
                let appointing_self = requester_person_id == person_id.as_str();
                if !appointing_self
                    && !person_manages_department(
                        view,
                        requester_person_id,
                        &appointee.department_id,
                    )
                {
                    return Err(CreateDepartmentRefusal::RequesterOutOfScope);
                }
            }
            // Departed first, and alone. The set was four clauses until
            // 2026-08-13, when two went for different reasons: the on-loan
            // clause asked whether home and assigned disagreed, and deleting
            // the loan concept removed the only thing that could make them; and
            // `AlreadyHeads` was replaced by the vacancy decision below, which
            // asks a question with two answers instead of refusing outright.
            // Departed stays here because it is not answerable by a vacancy
            // decision — asking for one would send the caller to the wrong fix.
            let ineligible = if appointee.employment_state == EmploymentState::Departed {
                Some(HeadIneligibility::Departed)
            } else {
                None
            };
            if let Some(because) = ineligible {
                return Err(CreateDepartmentRefusal::HeadNotEligible {
                    person_id: person_id.to_string(),
                    because,
                });
            }
            // THE VACANCY DECISION. A sitting head is no longer refused for
            // being one; they are asked what becomes of the department they
            // leave. `departments_one_head` is a UNIQUE index, so the old
            // headship has to end in the same transaction the new one begins,
            // and the caller is the only one who can say how.
            let heads_now = department_headed_by_person(view, person_id).map(str::to_owned);
            check_head_vacancy(view, heads_now.as_deref(), person_id, proposal.head_vacates)
                .map_err(CreateDepartmentRefusal::HeadVacancy)?;
            // `NotAWorker` now asks only about somebody who heads NOTHING. A
            // sitting head IS a non-worker, and used to be caught by the
            // `AlreadyHeads` clause that stood above this one; the vacancy
            // decision has taken that case, so testing kind again here would
            // refuse the very request the decision just answered.
            if heads_now.is_none() && appointee.kind != PersonKind::Worker {
                return Err(CreateDepartmentRefusal::HeadNotEligible {
                    person_id: person_id.to_string(),
                    because: HeadIneligibility::NotAWorker,
                });
            }
        }
        HeadDecision::HireNew { person_id, seed } => {
            if view.people.contains_key(person_id.as_str()) {
                return Err(CreateDepartmentRefusal::DuplicatePersonId);
            }
            if !valid_entity_id(person_id) {
                return Err(invalid(
                    "head.personId",
                    format!("{ENTITY_ID_RULE}; got {}", echo(person_id)),
                ));
            }
            if let Err(rejection) =
                validate_new_person_seed(&OwnedNewPersonSeed::as_ref(seed), PersonKind::Head)
            {
                let rejection = rejection.under("head.");
                return Err(invalid(&rejection.field, rejection.detail));
            }
        }
    }
    // Validate the COMPLETE initial roster before anything is applied, so a
    // duplicate or invalid later member refuses the whole request.
    let mut roster_ids: HashSet<String> = HashSet::new();
    roster_ids.insert(proposal.head.person_id().to_owned());
    for (index, member) in proposal.staff.iter().enumerate() {
        if !valid_entity_id(&member.person_id) {
            return Err(invalid(
                &format!("staff[{index}].personId"),
                format!("{ENTITY_ID_RULE}; got {}", echo(&member.person_id)),
            ));
        }
        if !roster_ids.insert(member.person_id.clone())
            || view.people.contains_key(&member.person_id)
        {
            return Err(CreateDepartmentRefusal::DuplicatePersonId);
        }
        if let Err(rejection) = validate_new_person_seed(&member.seed.as_ref(), PersonKind::Worker)
        {
            let rejection = rejection.under(&format!("staff[{index}]."));
            return Err(invalid(&rejection.field, rejection.detail));
        }
    }
    Ok(())
}

/// Whether this hire is eligible, or the exact refusal. PURE, and the ONE
/// definition of the rules — see [`check_department_create`].
///
/// The guard ORDER is contractual and preserved exactly: `personId` shape ->
/// seed validation -> `unknown-department` -> `destination-paused` (including
/// the ancestor chain) -> `requester-out-of-scope` -> `duplicate-person-id`.
///
/// # Errors
/// The refusal the caller must surface.
pub fn check_hire(view: &OrgView, proposal: &HireProposal<'_>) -> Result<(), HireRefusal> {
    if !valid_entity_id(proposal.person_id) {
        return Err(HireRefusal::InvalidSeed {
            field: "personId".to_owned(),
            detail: format!("{ENTITY_ID_RULE}; got {}", echo(proposal.person_id)),
        });
    }
    if let Err(rejection) = validate_new_person_seed(proposal.seed, PersonKind::Worker) {
        return Err(HireRefusal::InvalidSeed { field: rejection.field, detail: rejection.detail });
    }
    match department_state(view, proposal.department_id) {
        None => return Err(HireRefusal::UnknownDepartment),
        Some(UnitState::Paused) => return Err(HireRefusal::DestinationPaused),
        Some(UnitState::Active) => {}
    }
    if department_or_ancestor_is_paused(view, proposal.department_id) {
        return Err(HireRefusal::DestinationPaused);
    }
    if let Some(requester_person_id) = proposal.requester_person_id {
        if !person_manages_department(view, requester_person_id, proposal.department_id) {
            return Err(HireRefusal::RequesterOutOfScope);
        }
    }
    // A hire NEVER overwrites an existing person.
    if view.people.contains_key(proposal.person_id) {
        return Err(HireRefusal::DuplicatePersonId);
    }
    Ok(())
}

/// The manifest AS IT WOULD BE after this department create, or the exact
/// refusal. The eligibility half is [`check_department_create`] — this adds
/// only the shape of the result, so a caller can materialize the proposed
/// company before committing to it.
///
/// # Errors
/// The refusal the caller must surface. Nothing is written either way.
pub fn project_department_create(
    manifest: &OrganizationManifest,
    proposal: &DepartmentCreateProposal<'_>,
) -> Result<OrganizationManifest, CreateDepartmentRefusal> {
    check_department_create(&OrgView::from_manifest(manifest), proposal)?;

    let mut projected = manifest.clone();
    match proposal.head {
        HeadDecision::AppointExisting { person_id } => {
            // The VACATED department first, and the order is not cosmetic: the
            // route materializes this manifest and the writer commits the same
            // shape, where `departments_one_head` is a UNIQUE index. A
            // projection that added the new headship before ending the old one
            // would describe a manifest the transaction cannot write.
            if let Some(decision) = proposal.head_vacates {
                let vacated = department_headed_by_person(
                    &OrgView::from_manifest(manifest),
                    person_id.as_str(),
                )
                .map(str::to_owned);
                if let Some(vacated) = vacated {
                    apply_head_vacancy(&mut projected, &vacated, decision);
                }
            }
            // Appointment re-points home AND assigned into the new unit and
            // promotes the appointee to head.
            let Some(promoted) = projected.people.get_mut(person_id.as_str()) else {
                return Err(CreateDepartmentRefusal::HeadDecisionRequired);
            };
            promoted.kind = PersonKind::Head;
            promoted.department_id = proposal.department_id.to_owned();
        }
        HeadDecision::HireNew { person_id, seed } => insert_projected_person(
            &mut projected,
            person_id,
            proposal.department_id,
            &OwnedNewPersonSeed::as_ref(seed),
            proposal.at,
        ),
    }
    let (kind, transient) = match proposal.unit {
        DepartmentCreateUnit::Department => (UnitKind::Department, None),
        DepartmentCreateUnit::Contract(transient) => (UnitKind::Contract, Some(transient.clone())),
    };
    projected.departments.insert(
        proposal.department_id.to_owned(),
        DepartmentRecord {
            id: proposal.department_id.to_owned(),
            name: proposal.name.to_owned(),
            purpose: proposal.purpose.to_owned(),
            kind: Some(kind),
            transient,
            parent_department_id: Some(proposal.parent_id.to_owned()),
            head_person_id: proposal.head.person_id().to_owned(),
            state: UnitState::Active,
            created_at: proposal.at.to_owned(),
            extra: BTreeMap::new(),
        },
    );
    projected.department_order.push(proposal.department_id.to_owned());
    for member in proposal.staff {
        insert_projected_person(
            &mut projected,
            &member.person_id,
            proposal.department_id,
            &member.seed.as_ref(),
            proposal.at,
        );
    }
    projected.updated_at = proposal.at.to_owned();
    Ok(projected)
}

/// The manifest AS IT WOULD BE after this hire, or the exact refusal. The
/// eligibility half is [`check_hire`].
///
/// # Errors
/// The refusal the caller must surface. Nothing is written either way.
pub fn project_hire(
    manifest: &OrganizationManifest,
    proposal: &HireProposal<'_>,
) -> Result<OrganizationManifest, HireRefusal> {
    check_hire(&OrgView::from_manifest(manifest), proposal)?;

    let mut projected = manifest.clone();
    insert_projected_person(
        &mut projected,
        proposal.person_id,
        proposal.department_id,
        proposal.seed,
        proposal.at,
    );
    projected.updated_at = proposal.at.to_owned();
    Ok(projected)
}

/// End a headship in the projected manifest, the same way the writer ends it in
/// its transaction: promote the named successor, or delete the emptied unit.
///
/// Kept beside the writer's own step so the two can be read together — the
/// staged materialization is only worth anything if it is the manifest that
/// actually commits.
pub(crate) fn apply_head_vacancy(
    projected: &mut OrganizationManifest,
    vacated: &str,
    decision: &HeadVacancy,
) {
    match decision {
        HeadVacancy::HandOver { successor_person_id } => {
            if let Some(unit) = projected.departments.get_mut(vacated) {
                unit.head_person_id.clone_from(successor_person_id);
            }
            if let Some(successor) = projected.people.get_mut(successor_person_id.as_str()) {
                successor.kind = PersonKind::Head;
            }
        }
        HeadVacancy::Dissolve => {
            projected.departments.remove(vacated);
            projected.department_order.retain(|id| id != vacated);
        }
    }
}

/// Add the person a seed describes to the projected manifest, placed in the
/// hiring unit at the append ordinal — the same placement
/// `organization_rows::insert_person` writes.
fn insert_projected_person(
    projected: &mut OrganizationManifest,
    person_id: &str,
    department_id: &str,
    seed: &NewPersonSeed<'_>,
    at: &str,
) {
    projected.people.insert(
        person_id.to_owned(),
        PersonRecord {
            id: person_id.to_owned(),
            name: seed.name.to_owned(),
            title: seed.title.to_owned(),
            mandate: seed.mandate.to_owned(),
            kind: seed.kind,
            department_id: department_id.to_owned(),
            employment_state: seed.employment_state,
            activation: seed.activation.to_owned(),
            tools: seed.tools.to_vec(),
            prompts: seed.prompts.to_vec(),
            created_at: at.to_owned(),
            staffing_history: None,
            extra: BTreeMap::new(),
        },
    );
    projected.people_order.push(person_id.to_owned());
}

/// A manifest with no departments and no people, standing in for a company
/// whose `org_settings` row is absent (never created / already removed).
///
/// The projection then reaches its ordinary refusals in their ordinary order —
/// `unknown-parent` for a create, `unknown-department` for a hire — rather than
/// the caller short-circuiting to a guess about which refusal applies. That
/// matters because seed-shape refusals are ordered BEFORE the destination
/// lookup on the hire path.
#[must_use]
pub fn empty_manifest(slug: &str) -> OrganizationManifest {
    OrganizationManifest {
        schema_version: super::organization::ORGANIZATION_SCHEMA_VERSION,
        kind: "organization".to_owned(),
        slug: slug.to_owned(),
        name: String::new(),
        purpose: String::new(),
        root_department_id: super::organization::ROOT_DEPARTMENT_ID.to_owned(),
        policy: super::organization::OrganizationPolicy {
            supervision_interval_ms: 0,
            acknowledgement_timeout_ms: 0,
            acknowledgement_retry_limit: 0,
            replacement_limit: 0,
        },
        department_order: Vec::new(),
        people_order: Vec::new(),
        departments: BTreeMap::new(),
        people: BTreeMap::new(),
        created_at: String::new(),
        updated_at: String::new(),
        extra: BTreeMap::new(),
    }
}
