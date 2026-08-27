//! The human/manager staffing lifecycle decision core.
//!
//! This is the single dispatcher over bench / transfer /
//! offboard. The TypeScript lifecycle module it grew out of is deleted, so the
//! comments below name the behaviour rather than the old file.
//!
//! This module owns every *decision* the lifecycle makes and none of the
//! effects: the caller reads the manifest, activity ledger and supervision
//! ledger, asks [`plan_request`] and [`decide`] what to do, and then performs
//! the one transaction the answer names. Keeping the decision pure is what lets
//! the whole operation be tested against a manifest rather than against a live
//! runtime server.
//!
//! # What the two-phase transition is actually for
//!
//! A staffing move is prepared as a transition first and applied second. The
//! two phases are not a wait: nothing blocks between them. The machinery earns
//! its keep because an **applied** transition is what sheds launch intent and
//! drives the pane teardown. So the caller prepares the transition, *releases* it
//! ([`crate::store::activity::release`]) and applies the structural mutation in
//! one call — a finished person benches, transfers or offboards immediately.
//!
//! # TOMBSTONE (#751-P4): the release used to carry a fabricated handoff
//!
//! Until #751-P4 a transition could only reach `ready` by way of a five-field
//! "reflection" payload, so this module exported `synthetic_handoff`, which
//! manufactured that payload ("Auto-handoff for `<action>`: reflection fence
//! removed.") purely so the state machine would advance. Nothing ever read the
//! text. The payload is now deleted product-wide and a transition records only
//! that it was released, so the fabrication is gone with nothing in its place.
//!
//! `synthetic_handoff` also carried the *guard* on when the release may be
//! issued at all, and that guard is real. It now lives at the single call site
//! (`chiefd-api`'s staffing lifecycle) as `!transition.status.is_released() &&
//! transition.abandoned_at.is_none()`, for two independent reasons: a
//! `Ready`/`Applied` transition was already released, and re-releasing it would
//! be a no-op refusal; an **abandoned** transition is terminal, so a release
//! against it refuses `transition-terminal` — which would turn the idempotent
//! retry the abandoned-reuse rule exists to support into a hard error.
//!
//! # An abandoned handoff must still be recognized on retry
//!
//! [`matching_transition`] admits a `Cancelled` transition **only** when it
//! carries `abandoned_at`. An abandoned transition is a prepared operation
//! whose release was provably unreachable, so an idempotent retry has to find
//! it or [`assert_applicable`] throws "already belongs to" on the second call.
//! Ordinary cancellations — idle-park recycles,
//! staffing supersessions — must NOT be admitted, which is why this keys on
//! `abandoned_at` and never on the status alone.
//!
//! # The unattended offboard (#443)
//!
//! An unattended removal is [`LifecycleDecision::ApplyDirectly`]: offboard,
//! withdraw the launch intent explicitly (there is no applied transition to
//! shed it), and
//! reconcile. It fires only for a genuinely divergent person, so it never
//! weakens the handoff an ordinary offboard still records.

use crate::error::Refusal;
use crate::store::activity::{
    ActivityLedger, GracefulTransition, TransitionAction, TransitionStatus,
};
use crate::store::organization::{
    stopped_organization_unit_ancestor, EmploymentState, OrganizationManifest, PersonKind,
    PersonRecord,
};

/// Refusal code for a request naming a person the manifest does not have.
pub const UNKNOWN_PERSON: &str = "unknown-person";

/// Refusal code for a request naming a unit the manifest does not have.
pub const UNKNOWN_DEPARTMENT: &str = "unknown-department";

/// Refusal code for a required field that was blank.
pub const MISSING_FIELD: &str = "missing-field";

/// Refusal code for a move a head cannot make.
pub const HEAD_NOT_MOVABLE: &str = "head-not-movable";

/// Refusal code for a destination whose ancestry is paused.
pub const STOPPED_DESTINATION: &str = "stopped-destination";

/// Refusal code for a request that is already true.
pub const ALREADY_APPLIED: &str = "already-applied";

/// Refusal code for a person who left.
pub const PERSON_DEPARTED: &str = "person-departed";

/// The five lifecycle actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaffingLifecycleRequest {
    /// Retain the person, stop their compute.
    Bench {
        /// Whose.
        person_id: String,
        /// Optional operator note; a default is composed when blank.
        reason: Option<String>,
    },
    /// Move the person permanently.
    Transfer {
        /// Whose.
        person_id: String,
        /// Where to.
        to_department_id: String,
        /// Optional operator note; a default is composed when blank.
        reason: Option<String>,
    },
    /// Remove the person from the company.
    Offboard {
        /// Whose.
        person_id: String,
        /// Optional operator note; a default is composed when blank.
        reason: Option<String>,
    },
}

impl StaffingLifecycleRequest {
    /// Whose lifecycle this is.
    #[must_use]
    pub fn person_id(&self) -> &str {
        match self {
            Self::Bench { person_id, .. }
            | Self::Transfer { person_id, .. }
            | Self::Offboard { person_id, .. } => person_id.as_str(),
        }
    }

    /// The wire spelling of the action.
    #[must_use]
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Bench { .. } => "bench",
            Self::Transfer { .. } => "transfer",
            Self::Offboard { .. } => "offboard",
        }
    }
}

/// A request resolved against the manifest: the transition it needs, the reason
/// that will be recorded, and the destination it moves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaffingPlan {
    /// The normalized request.
    pub request: StaffingLifecycleRequest,
    /// The transition the operation prepares.
    pub transition_action: TransitionAction,
    /// The reason recorded on the transition, never blank.
    pub reason: String,
    /// The destination unit, when the action has one.
    pub to_department_id: Option<String>,
}

/// Resolve a request into a plan.
///
/// The manifest is unused TODAY and the parameter stays: `return` read the
/// person's home out of it to resolve its own destination, and that verb was
/// deleted with the loan concept on 2026-08-13. Every surviving request names
/// its destination or needs none. Kept because this is the shared entry point
/// for the whole family and the next verb that needs the manifest should not
/// have to change every caller to get it back.
///
/// # Errors
/// [`MISSING_FIELD`] for a blank required field.
pub fn plan_request(
    _manifest: &OrganizationManifest,
    request: &StaffingLifecycleRequest,
) -> Result<StaffingPlan, Refusal> {
    let person_id = required(Some(request.person_id()), "personId")?;
    match request {
        StaffingLifecycleRequest::Bench { reason, .. } => Ok(StaffingPlan {
            request: StaffingLifecycleRequest::Bench {
                person_id: person_id.clone(),
                reason: reason.clone(),
            },
            transition_action: TransitionAction::Park,
            reason: trimmed(reason.as_deref())
                .unwrap_or_else(|| format!("Bench '{person_id}' after a bounded handoff.")),
            to_department_id: None,
        }),
        StaffingLifecycleRequest::Offboard { reason, .. } => Ok(StaffingPlan {
            request: StaffingLifecycleRequest::Offboard {
                person_id: person_id.clone(),
                reason: reason.clone(),
            },
            transition_action: TransitionAction::Offboard,
            reason: trimmed(reason.as_deref())
                .unwrap_or_else(|| format!("Offboard '{person_id}' after a bounded handoff.")),
            to_department_id: None,
        }),
        StaffingLifecycleRequest::Transfer { to_department_id, reason, .. } => {
            let destination = required(Some(to_department_id), "transfer target department")?;
            Ok(StaffingPlan {
                request: StaffingLifecycleRequest::Transfer {
                    person_id: person_id.clone(),
                    to_department_id: to_department_id.clone(),
                    reason: reason.clone(),
                },
                transition_action: TransitionAction::Transfer,
                reason: trimmed(reason.as_deref()).unwrap_or_else(|| {
                    format!("Transfer '{person_id}' to '{destination}' after a bounded handoff.")
                }),
                to_department_id: Some(destination),
            })
        }
    }
}

/// Whether the manifest already reflects the plan's outcome.
///
/// This is the idempotency predicate: a retry whose mutation already landed
/// must reconcile the runtime and report success rather than refusing.
#[must_use]
pub fn operation_matches(
    manifest: &OrganizationManifest,
    plan: &StaffingPlan,
    transition: &GracefulTransition,
) -> bool {
    let Some(target) = manifest.people.get(plan.request.person_id()) else {
        return false;
    };
    // ONE comparison. This was a four-way idempotency table with a different
    // asymmetric predicate per request kind, because a bench and an offboard had
    // to hold BOTH placement columns still while a transfer only had to land the
    // assigned one. One column, one question: did the person move.
    let placement_unchanged = target.department_id == transition.placement_department_id;
    match &plan.request {
        StaffingLifecycleRequest::Bench { .. } => {
            target.employment_state == EmploymentState::Benched && placement_unchanged
        }
        StaffingLifecycleRequest::Offboard { .. } => {
            target.employment_state == EmploymentState::Departed && placement_unchanged
        }
        StaffingLifecycleRequest::Transfer { .. } => {
            let to = plan.to_department_id.as_deref();
            Some(target.department_id.as_str()) == to
        }
    }
}

/// Whether a recorded transition belongs to this plan.
#[must_use]
pub fn transition_matches(transition: &GracefulTransition, plan: &StaffingPlan) -> bool {
    transition.person_id == plan.request.person_id()
        && transition.action == plan.transition_action
        && transition.to_department_id.as_deref() == plan.to_department_id.as_deref()
}

/// The most recent transition this plan may reuse, if any.
///
/// Scans newest-first through `transition_order`. A `Cancelled` transition is
/// admitted **only** when it carries `abandoned_at` — see the module docs.
#[must_use]
pub fn matching_transition<'l>(
    ledger: &'l ActivityLedger,
    plan: &StaffingPlan,
) -> Option<&'l GracefulTransition> {
    ledger.transition_order.iter().rev().filter_map(|id| ledger.transitions.get(id)).find(
        |transition| {
            transition_matches(transition, plan)
                && (transition.status != TransitionStatus::Cancelled
                    || transition.abandoned_at.is_some())
        },
    )
}

/// Every precondition the action has, checked against the manifest.
///
/// # Errors
/// One of [`UNKNOWN_PERSON`], [`UNKNOWN_DEPARTMENT`], [`PERSON_DEPARTED`],
/// [`HEAD_NOT_MOVABLE`], [`STOPPED_DESTINATION`], or [`ALREADY_APPLIED`].
pub fn assert_applicable(
    manifest: &OrganizationManifest,
    plan: &StaffingPlan,
) -> Result<(), Refusal> {
    let person_id = plan.request.person_id();
    match &plan.request {
        StaffingLifecycleRequest::Bench { .. } => {
            let target = person(manifest, person_id)?;
            match target.employment_state {
                EmploymentState::Departed => Err(Refusal::new(
                    PERSON_DEPARTED,
                    format!("Cannot bench departed person '{person_id}'"),
                )),
                EmploymentState::Benched => Err(Refusal::new(
                    ALREADY_APPLIED,
                    format!("Person '{person_id}' is already benched"),
                )),
                EmploymentState::Active => Ok(()),
            }
        }
        StaffingLifecycleRequest::Transfer { .. } => {
            let to = destination(plan)?;
            active_destination(manifest, to, &format!("transfer '{person_id}'"))?;
            let target = movable_worker(manifest, person_id, "transfer")?;
            if target.department_id == to {
                return Err(Refusal::new(
                    ALREADY_APPLIED,
                    format!("Person '{person_id}' already belongs to '{to}'"),
                ));
            }
            Ok(())
        }
        StaffingLifecycleRequest::Offboard { .. } => {
            movable_worker(manifest, person_id, "offboard")?;
            Ok(())
        }
    }
}

/// Whether the person's placement still matches what the transition recorded.
///
/// # Errors
/// [`UNKNOWN_PERSON`], or [`crate::store::organization::MANIFEST_INVALID`] when
/// the placement moved after the transition was prepared. Never skipped, even
/// for an abandoned handoff: an unreachable release does not make a stale
/// destination safe.
pub fn assert_source_unchanged(
    manifest: &OrganizationManifest,
    transition: &GracefulTransition,
) -> Result<(), Refusal> {
    let target = person(manifest, &transition.person_id)?;
    if target.department_id != transition.placement_department_id {
        return Err(Refusal::new(
            crate::store::organization::MANIFEST_INVALID,
            format!(
                "Person '{}' placement changed after transition '{}' was prepared",
                transition.person_id, transition.id
            ),
        ));
    }
    Ok(())
}

/// How the handoff resolved, for the operator's benefit.
///
/// Operator output must never read identically for an apply whose transition
/// was released and one whose release was abandoned as unreachable: the second
/// is a divergence worth seeing, and collapsing the two hides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandoffOutcome {
    /// The transition reached its structural change through a release.
    Completed,
    /// The release was provably unreachable and the transition was dropped.
    Abandoned,
}

/// What the caller must do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleDecision {
    /// The mutation already landed. Reconcile the runtime and report success;
    /// change no structure.
    AlreadyApplied {
        /// The transition that recorded it.
        transition_id: String,
        /// How its handoff resolved.
        handoff: HandoffOutcome,
    },
    /// An idempotent retry of a completed offboard: the person is already gone.
    /// Do nothing at all.
    NoOp,
    /// Apply the structural verb directly and reconcile — no transition. An
    /// offboard additionally withdraws the launch intent, since there is no
    /// applied transition to shed it.
    ApplyDirectly {
        /// The reason to record on the structural mutation.
        reason: String,
    },
    /// Prepare a transition, release it, apply the structural
    /// mutation, and reconcile.
    PrepareAndApply {
        /// An existing transition to reuse, when one matched.
        reuse_transition_id: Option<String>,
        /// The reason to record on the transition.
        reason: String,
    },
}

/// Decide what a lifecycle call should do.
///
///
/// # Errors
/// Whatever [`assert_applicable`] refuses, for the paths that reach it.
pub fn decide(
    manifest: &OrganizationManifest,
    activity: &ActivityLedger,
    plan: &StaffingPlan,
) -> Result<LifecycleDecision, Refusal> {
    let person_id = plan.request.person_id();
    if matches!(plan.request, StaffingLifecycleRequest::Offboard { .. }) {
        let departed = manifest
            .people
            .get(person_id)
            .is_some_and(|p| p.employment_state == EmploymentState::Departed);
        if departed {
            return Ok(LifecycleDecision::NoOp);
        }
    }
    if let Some(transition) = matching_transition(activity, plan) {
        if operation_matches(manifest, plan, transition) {
            return Ok(LifecycleDecision::AlreadyApplied {
                transition_id: transition.id.clone(),
                handoff: handoff_outcome(transition),
            });
        }
        assert_applicable(manifest, plan)?;
        return Ok(LifecycleDecision::PrepareAndApply {
            reuse_transition_id: Some(transition.id.clone()),
            reason: plan.reason.clone(),
        });
    }

    assert_applicable(manifest, plan)?;
    Ok(LifecycleDecision::PrepareAndApply {
        reuse_transition_id: None,
        reason: plan.reason.clone(),
    })
}

/// How a transition's handoff resolved.
#[must_use]
pub fn handoff_outcome(transition: &GracefulTransition) -> HandoffOutcome {
    if transition.abandoned_at.is_some() {
        HandoffOutcome::Abandoned
    } else {
        HandoffOutcome::Completed
    }
}

// TOMBSTONE (#751-P4): `synthetic_handoff` lived here. It returned the
// fabricated `(summary, learning, handoff)` triple that a prepared transition
// was "satisfied" with so it would advance to `ready`, plus the guard on when to
// issue that call at all. The payload is deleted product-wide — a transition
// records only that it was released — so the triple has no destination and the
// function is gone. The guard survives at the single call site; see the module
// docs above for the two reasons it rejects a released or an abandoned
// transition. Do not resurrect a helper here that manufactures content: there is
// no longer any column for it to land in.

/// Whether the runtime should keep this person resident through the reconcile
/// that follows the mutation.
///
/// A transfer moves a live pane; a bench or offboard takes it
/// down.
#[must_use]
pub const fn keeps_person_active(plan: &StaffingPlan) -> bool {
    matches!(plan.request, StaffingLifecycleRequest::Transfer { .. })
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|v| !v.is_empty()).map(ToString::to_string)
}

/// Generic over `AsRef<str>` so both `&String` (the destructured request
/// fields) and `&str` (`StaffingLifecycleRequest::person_id`) reach it without
/// a conversion at each of the seven call sites.
fn required(value: Option<impl AsRef<str>>, label: &str) -> Result<String, Refusal> {
    trimmed(value.as_ref().map(AsRef::as_ref))
        .ok_or_else(|| Refusal::new(MISSING_FIELD, format!("{label} is required")))
}

fn destination(plan: &StaffingPlan) -> Result<&str, Refusal> {
    plan.to_department_id
        .as_deref()
        .ok_or_else(|| Refusal::new(MISSING_FIELD, "target department is required"))
}

fn person<'m>(
    manifest: &'m OrganizationManifest,
    person_id: &str,
) -> Result<&'m PersonRecord, Refusal> {
    manifest.people.get(person_id).ok_or_else(|| {
        Refusal::new(UNKNOWN_PERSON, format!("Unknown organization person '{person_id}'"))
    })
}

fn headed_department_id<'m>(
    manifest: &'m OrganizationManifest,
    person_id: &str,
) -> Option<&'m str> {
    manifest
        .department_order
        .iter()
        .filter_map(|id| manifest.departments.get(id))
        .find(|unit| unit.head_person_id == person_id)
        .map(|unit| unit.id.as_str())
}

/// A head permanently owns their unit, so no lifecycle action may move them —
/// the whole unit reparents instead.
fn movable_worker<'m>(
    manifest: &'m OrganizationManifest,
    person_id: &str,
    action: &str,
) -> Result<&'m PersonRecord, Refusal> {
    let target = person(manifest, person_id)?;
    let headed = headed_department_id(manifest, person_id);
    if target.kind != PersonKind::Worker || headed.is_some() {
        return Err(Refusal::new(
            HEAD_NOT_MOVABLE,
            format!(
                "Cannot {action} '{person_id}' while they head department '{}'",
                headed.unwrap_or(target.department_id.as_str())
            ),
        ));
    }
    if target.employment_state == EmploymentState::Departed {
        return Err(Refusal::new(
            PERSON_DEPARTED,
            format!("Cannot {action} departed person '{person_id}'"),
        ));
    }
    Ok(target)
}

fn active_destination(
    manifest: &OrganizationManifest,
    department_id: &str,
    action: &str,
) -> Result<(), Refusal> {
    if !manifest.departments.contains_key(department_id) {
        return Err(Refusal::new(
            UNKNOWN_DEPARTMENT,
            format!("Unknown department '{department_id}'"),
        ));
    }
    if let Some(stopped_by) = stopped_organization_unit_ancestor(manifest, department_id)? {
        return Err(Refusal::new(
            STOPPED_DESTINATION,
            format!(
                "Cannot {action} into stopped unit '{department_id}'; '{}' is paused",
                stopped_by.id
            ),
        ));
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

    fn ledger(manifest: &OrganizationManifest) -> ActivityLedger {
        ActivityLedger::initial(manifest, "2026-08-07T00:00:00.000Z")
    }

    fn transition(
        id: &str,
        person_id: &str,
        action: TransitionAction,
        to: Option<&str>,
        placement: &str,
    ) -> GracefulTransition {
        GracefulTransition {
            id: id.to_string(),
            person_id: person_id.to_string(),
            action,
            reason: "because".to_string(),
            intent_id: None,
            placement_department_id: placement.to_string(),
            to_department_id: to.map(ToString::to_string),
            status: TransitionStatus::AwaitingHandoff,
            requested_at: "2026-08-07T00:00:00.000Z".to_string(),
            handoff_deadline_at: "2026-08-07T00:02:00.000Z".to_string(),
            applied_at: None,
            cancelled_at: None,
            forced_at: None,
            abandoned_at: None,
        }
    }

    fn bench(person_id: &str) -> StaffingLifecycleRequest {
        StaffingLifecycleRequest::Bench { person_id: person_id.to_string(), reason: None }
    }

    #[test]
    fn a_blank_bench_reason_gets_a_composed_default() {
        let m = manifest();
        let plan = plan_request(&m, &bench("signal-researcher")).expect("plan");
        assert_eq!(plan.transition_action, TransitionAction::Park);
        assert_eq!(plan.reason, "Bench 'signal-researcher' after a bounded handoff.");
        assert!(plan.to_department_id.is_none());
    }

    /// NOBODY IS INTERROGATED BEFORE A FIRING OR A MOVE. A blank reason used
    /// to refuse the whole request; it now composes the same kind of default
    /// `bench` always has, and the transition still carries a legible line.
    #[test]
    fn a_blank_offboard_or_transfer_reason_gets_a_composed_default() {
        let m = manifest();
        let offboard = plan_request(
            &m,
            &StaffingLifecycleRequest::Offboard {
                person_id: "signal-researcher".to_string(),
                reason: Some("  ".to_string()),
            },
        )
        .expect("plan");
        assert_eq!(offboard.reason, "Offboard 'signal-researcher' after a bounded handoff.");
        let transfer = plan_request(
            &m,
            &StaffingLifecycleRequest::Transfer {
                person_id: "signal-researcher".to_string(),
                to_department_id: "it".to_string(),
                reason: None,
            },
        )
        .expect("plan");
        assert_eq!(
            transfer.reason,
            "Transfer 'signal-researcher' to 'it' after a bounded handoff."
        );
    }

    #[test]
    fn a_head_can_never_be_transferred() {
        let m = manifest();
        let request = StaffingLifecycleRequest::Transfer {
            person_id: "quant-head".to_string(),
            to_department_id: "it".to_string(),
            reason: Some("reorg".to_string()),
        };
        let plan = plan_request(&m, &request).expect("plan");
        let err = assert_applicable(&m, &plan).expect_err("refusal");
        assert_eq!(err.code, HEAD_NOT_MOVABLE);
    }

    #[test]
    fn a_stopped_destination_refuses_a_move() {
        let mut m = manifest();
        if let Some(unit) = m.departments.get_mut("it") {
            unit.state = UnitState::Paused;
        }
        let request = StaffingLifecycleRequest::Transfer {
            person_id: "signal-researcher".to_string(),
            to_department_id: "it".to_string(),
            reason: Some("surge".to_string()),
        };
        let plan = plan_request(&m, &request).expect("plan");
        let err = assert_applicable(&m, &plan).expect_err("refusal");
        assert_eq!(err.code, STOPPED_DESTINATION);
    }

    #[test]
    fn benching_a_benched_person_refuses_as_already_applied() {
        let mut m = manifest();
        if let Some(person) = m.people.get_mut("signal-researcher") {
            person.employment_state = EmploymentState::Benched;
        }
        let plan = plan_request(&m, &bench("signal-researcher")).expect("plan");
        let err = assert_applicable(&m, &plan).expect_err("refusal");
        assert_eq!(err.code, ALREADY_APPLIED);
    }

    #[test]
    fn a_landed_bench_is_recognized_as_already_applied() {
        let mut m = manifest();
        if let Some(person) = m.people.get_mut("signal-researcher") {
            person.employment_state = EmploymentState::Benched;
        }
        let plan = StaffingPlan {
            request: bench("signal-researcher"),
            transition_action: TransitionAction::Park,
            reason: "x".to_string(),
            to_department_id: None,
        };
        let t = transition("t1", "signal-researcher", TransitionAction::Park, None, "quant");
        assert!(operation_matches(&m, &plan, &t));
    }

    #[test]
    fn an_ordinary_cancellation_is_not_reusable_but_an_abandoned_one_is() {
        let m = manifest();
        let mut l = ledger(&m);
        let plan = StaffingPlan {
            request: bench("signal-researcher"),
            transition_action: TransitionAction::Park,
            reason: "x".to_string(),
            to_department_id: None,
        };
        let mut cancelled =
            transition("t1", "signal-researcher", TransitionAction::Park, None, "quant");
        cancelled.status = TransitionStatus::Cancelled;
        l.transition_order.push("t1".to_string());
        l.transitions.insert("t1".to_string(), cancelled.clone());
        assert!(matching_transition(&l, &plan).is_none());

        cancelled.abandoned_at = Some("2026-08-07T00:05:00.000Z".to_string());
        l.transitions.insert("t1".to_string(), cancelled);
        assert!(matching_transition(&l, &plan).is_some());
    }

    #[test]
    fn a_completed_offboard_retry_is_a_no_op() {
        let mut m = manifest();
        if let Some(person) = m.people.get_mut("signal-researcher") {
            person.employment_state = EmploymentState::Departed;
        }
        let l = ledger(&m);
        let request = StaffingLifecycleRequest::Offboard {
            person_id: "signal-researcher".to_string(),
            reason: Some("role ended".to_string()),
        };
        let plan = plan_request(&m, &request).expect("plan");
        assert_eq!(decide(&m, &l, &plan).expect("decision"), LifecycleDecision::NoOp);
    }

    #[test]
    fn a_fresh_bench_prepares_and_applies() {
        let m = manifest();
        let l = ledger(&m);
        let plan = plan_request(&m, &bench("signal-researcher")).expect("plan");
        assert_eq!(
            decide(&m, &l, &plan).expect("decision"),
            LifecycleDecision::PrepareAndApply {
                reuse_transition_id: None,
                reason: "Bench 'signal-researcher' after a bounded handoff.".to_string(),
            }
        );
    }

    #[test]
    fn a_moved_person_fails_the_source_check() {
        let mut m = manifest();
        if let Some(person) = m.people.get_mut("signal-researcher") {
            person.department_id = "it".to_string();
        }
        let t = transition("t1", "signal-researcher", TransitionAction::Park, None, "quant");
        let err = assert_source_unchanged(&m, &t).expect_err("refusal");
        assert!(err.message.contains("placement changed"));
    }

    #[test]
    fn an_abandoned_transition_reports_an_abandoned_outcome() {
        // `abandoned_at` is the ONLY thing that separates the two outcomes: it
        // records a transition whose release was provably unreachable, and the
        // operator has to be able to tell that apply apart from one that went
        // through a real release. Keying on the status instead would fold
        // ordinary supersessions in with it.
        let mut t = transition("t1", "signal-researcher", TransitionAction::Park, None, "quant");
        assert_eq!(handoff_outcome(&t), HandoffOutcome::Completed);
        t.status = TransitionStatus::Cancelled;
        assert_eq!(
            handoff_outcome(&t),
            HandoffOutcome::Completed,
            "an ordinary cancel is not an abandonment"
        );
        t.abandoned_at = Some("2026-08-07T00:05:00.000Z".to_string());
        assert_eq!(handoff_outcome(&t), HandoffOutcome::Abandoned);
    }

    #[test]
    fn only_moves_keep_the_person_resident_through_the_reconcile() {
        let m = manifest();
        let benched = plan_request(&m, &bench("signal-researcher")).expect("plan");
        assert!(!keeps_person_active(&benched));
        let moved = plan_request(
            &m,
            &StaffingLifecycleRequest::Transfer {
                person_id: "signal-researcher".to_string(),
                to_department_id: "it".to_string(),
                reason: Some("surge".to_string()),
            },
        )
        .expect("plan");
        assert!(keeps_person_active(&moved));
    }
}
