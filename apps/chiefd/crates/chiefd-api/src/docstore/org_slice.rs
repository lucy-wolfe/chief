//! HTTP surface for the activity / staffing / units / people port.
//!
//! Every handler here replaces a TypeScript function that used to make the same
//! decision in `apps/cli/src/legacy/organization/`. The TS is deleted; this is
//! where the decision lives now.
//!
//! Handlers follow the house shape exactly (see `router.rs`): POST + JSON in,
//! JSON out, `unknown-company` 404 for a foreign slug, a `Refused` mapped to
//! 422 with its machine code, everything else through `company_error`. A
//! mutation wakes the reconcile loop rather than performing runtime work inline —
//! the client never converges anything (Mandate 1).

use axum::extract::{Extension, Json};

use chiefd_core::store::activity::{BeginTransitionInput, ReleaseInput};
use chiefd_core::store::control_authority::{
    department_is_in_scope, person_is_in_scope, ControlActor,
};
use chiefd_core::store::lifecycle_status::{
    project_organization_lifecycle_status, LifecycleStatusInput,
};
use chiefd_core::store::staffing_lifecycle::{
    decide, handoff_outcome, keeps_person_active, plan_request, HandoffOutcome, LifecycleDecision,
    StaffingLifecycleRequest,
};
use chiefd_core::store::unit_preview::{
    describe_unit_removal_impact, organization_tree_lines, organization_unit_subtree,
    preview_organization_unit_removal,
};

use super::route_error::RouteError;
use super::router::{caller_actor, company_error, now_iso, wake_reconcile, SupervisionLiveSource};

/// The refusal body every 422 in this module answers with.
pub(super) type Refused = RouteError;

/// Resolve the live company for a request slug, or 404.
pub(super) fn live(
    source: Option<SupervisionLiveSource>,
    slug: &str,
) -> Result<SupervisionLiveSource, Refused> {
    source
        .filter(|s| slug == s.org_documents_slug)
        .ok_or_else(|| RouteError::not_found("unknown-company", "no live company for this slug"))
}

/// Bind a `callerPersonId` DECLARED in the body to the authenticated caller.
///
/// The activity family is the one place in this module where a request already
/// carries a principal: both `callerPersonId` fields are documented as "the
/// person the trusted adapter authenticated. Never from a Pi payload". No
/// adapter authenticated them — the field arrived from the same client that
/// chose its value, so the doc-comment described an intent nothing enforced.
/// This is the enforcement, and it is the SAME predicate the staffing routes
/// use, not a second one.
///
/// Distinct from the disclosure fence the read routes in this file apply: that
/// one narrows what a caller may SEE, this one refuses a WRITE whose body names
/// somebody other than the caller. B3 and B4 landed them at the same insertion
/// point; both belong. (`caller_of`, which used to unwrap the optional
/// extension for the read half, is gone — the `Caller` extractor hands each
/// handler the identity directly.)
fn bind_caller_to_declared_person(
    caller: &chiefd_core::store::identities::Identity,
    declared_person_id: &str,
    company_slug: &str,
) -> Result<(), Refused> {
    super::caller_auth::bind_requester_to_caller(caller, Some(declared_person_id), company_slug)
}

/// Map a store refusal onto the 422 body.
fn refused(refusal: &chiefd_core::error::Refusal) -> Refused {
    RouteError::refused(refusal.code, refusal.message.clone())
}

/// Map any `ChiefdError` onto its wire status.
///
/// One line since the taxonomy moved into `route_error.rs`: the special case
/// this used to carry — pull the refusal out first, because `company_error`
/// would otherwise turn it into a codeless 400 — is now what `company_error`
/// does for every caller.
pub(super) fn failed(error: &chiefd_core::error::ChiefdError) -> Refused {
    company_error(error)
}

// --- lifecycle status ------------------------------------------------------

/// `POST /v1/org/lifecycle-status/read`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LifecycleStatusRequest {
    /// The own-company documentKey.
    pub(crate) slug: String,
    /// A NARROWING request, not a fence.
    ///
    /// This used to be the only thing that bounded the answer, which meant an
    /// omitted value returned the whole company to anybody. The fence is now
    /// derived from the caller (`disclosure_fence`); this field may only
    /// narrow further inside it, and naming a unit outside it is refused.
    #[serde(default)]
    pub(crate) scope_department_id: Option<String>,
    /// Optional people bound.
    #[serde(default)]
    pub(crate) max_people: Option<usize>,
}

/// Project the read-only up/down control board.
///
/// Every durable source but the manifest degrades into a warning, so an
/// operator asking "who is up and why" during an incident is never handed an
/// error because one ledger is unreadable.
///
/// # The scope is DERIVED, not chosen (B4)
///
/// `scopeDepartmentId` was an optional, caller-supplied filter, so omitting it
/// returned every department and every person in the company to any caller
/// that reached the route. It now narrows inside a fence taken from the
/// caller's own place in the tree, and a unit outside that fence is refused
/// `caller-out-of-scope`. A caller that names no person row — the resident
/// actuator's SERVICE identity, an operator, or no credential at all — is
/// unfenced and unchanged; see `disclosure_fence` for why a read must never
/// resolve a person.
pub(crate) async fn org_lifecycle_status_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<LifecycleStatusRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let manifest = match source.company.org_manifest_read().await {
        Ok(Some((manifest, _))) => manifest,
        Ok(None) => {
            return Err(RouteError::not_found(
                "unknown-company",
                "company has no organization manifest",
            ))
        }
        Err(error) => return Err(failed(&error)),
    };
    // Derived BEFORE any other durable read, so a caller that may see nothing
    // is refused without the daemon doing work on its behalf.
    let fence = super::disclosure_fence::disclosure_fence(&caller, &manifest)?;
    let scope_department_id = super::disclosure_fence::fenced_department(
        fence,
        req.scope_department_id.as_deref(),
        &manifest,
    )?;

    let activity = source.company.activity_read().await;
    let launch_intent = source.company.launch_intent_read().await;

    let activity_ledger = match &activity {
        Ok(value) => Ok(value.as_ref().map(|(ledger, _)| ledger)),
        Err(error) => Err(error.to_string()),
    };
    let intent = launch_intent.ok().flatten().map(|(doc, _)| doc);
    let status = project_organization_lifecycle_status(&LifecycleStatusInput {
        manifest: &manifest,
        activity: activity_ledger,
        launch_intent: intent.as_ref(),
        // TOMBSTONE (chief-home-is-cwd §4c): a `ceo_boot_lease_held` input sat
        // here, read fail-open off the `boot_lease` row. The lease had one
        // writer — the daemon-side CEO boot — and that is deleted, so the
        // board's `ceoOnlyBootInFlight` column could only ever say `false`.
        scope_department_id: scope_department_id.as_deref(),
        max_people: req.max_people,
    })
    .map_err(|refusal| refused(&refusal))?;
    serde_json::to_value(&status)
        .map(Json)
        .map_err(|_| RouteError::fault("error", "lifecycle status could not be serialized"))
}

// --- units -----------------------------------------------------------------

/// `POST /v1/org/tree/read`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SlugRequest {
    /// The own-company documentKey.
    pub(crate) slug: String,
}

/// Render the operator's ASCII organization tree.
///
/// Rooted at the caller's fence, not at the company (B4). The projection is
/// unchanged: the FENCE is applied to the manifest it renders, so there is one
/// tree renderer and not a second fence-aware copy of it.
pub(crate) async fn org_tree_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<SlugRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let manifest = read_manifest(&source).await?;
    let fence = super::disclosure_fence::disclosure_fence(&caller, &manifest)?;
    let narrowed = super::disclosure_fence::narrowed_manifest(&manifest, fence.as_deref());
    let view = narrowed.as_ref().unwrap_or(&manifest);
    let lines = organization_tree_lines(view).map_err(|r| refused(&r))?;
    Ok(Json(serde_json::json!({"lines": lines})))
}

/// `POST /v1/org/tree/structured` — departments as a forest, each with its
/// people.
///
/// The sibling of `org_tree_read`, which answers the same question for a
/// terminal (ASCII lines). A browser needs the STRUCTURE, and building it
/// client-side is what `apps/api` did — a projection of chiefd's own manifest
/// living in a client, which is the duplication mandate 3 forbids and the
/// reason deleting `apps/api` left the web unable to render a company.
///
/// Placement and identity only: no runtime state. Who is running is observed
/// by the routes that watch processes, and a tree carrying a stale `running`
/// flag would be a snapshot pretending to be live.
///
/// Rooted at the caller's fence (B4), by the same manifest narrowing
/// `org_tree_read` uses — so the terminal and the browser agree about what a
/// given caller may be told, rather than each deciding for itself.
pub(crate) async fn org_tree_structured(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<SlugRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let full = read_manifest(&source).await?;
    let fence = super::disclosure_fence::disclosure_fence(&caller, &full)?;
    // BEFORE THE NARROWING, and this is not a stylistic ordering. Accents come
    // from the same allocator materialization uses, so the colour a pane is
    // painted and the colour the browser draws are one decision — and the CEO's
    // colour is FIXED rather than allocated, so the allocator has to be told
    // who the CEO is. `narrowed_manifest` REWRITES `root_department_id` to the
    // fence unit, so `chief_person_id()` on a narrowed manifest never fails and
    // never answers the real Chief: it answers the FENCE's head. Asking the
    // narrowed manifest would therefore paint a department head in the CEO's
    // reserved purple for every fenced caller, in the browser, while that same
    // person's pane border wore their allocated colour.
    let chief_person_id = full.chief_person_id().ok().map(std::borrow::ToOwned::to_owned);
    let narrowed = super::disclosure_fence::narrowed_manifest(&full, fence.as_deref());
    let manifest = narrowed.unwrap_or(full);
    let accents = slice_accents(&manifest, chief_person_id.as_deref());
    let tree = super::company_tree::build_company_tree(&req.slug, &manifest, &accents);
    serde_json::to_value(tree).map(Json).map_err(|error| {
        refused(&chiefd_core::error::Refusal::new("company-tree-unserializable", error.to_string()))
    })
}

/// `POST /v1/org/unit/{subtree,removal-impact}`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UnitRequest {
    /// The own-company documentKey.
    pub(crate) slug: String,
    /// The unit in question.
    pub(crate) unit_id: String,
}

/// The unit plus every descendant, in canonical order.
///
/// The subject is NAMED in the body, so there is nothing to narrow (B4): the
/// unit is either inside the caller's fence or the read is refused.
pub(crate) async fn org_unit_subtree(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<UnitRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let manifest = read_manifest(&source).await?;
    super::disclosure_fence::require_department(&caller, &manifest, &req.unit_id)?;
    let unit_ids = organization_unit_subtree(&manifest, &req.unit_id).map_err(|r| refused(&r))?;
    Ok(Json(serde_json::json!({"unitIds": unit_ids})))
}

/// Exactly who a unit removal would fire, without writing anything.
///
/// It names the PEOPLE a removal would offboard, so it discloses a roster as
/// well as a shape; same body-named subject, same fence.
pub(crate) async fn org_unit_removal_impact(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<UnitRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let manifest = read_manifest(&source).await?;
    super::disclosure_fence::require_department(&caller, &manifest, &req.unit_id)?;
    let impact = describe_unit_removal_impact(&manifest, &req.unit_id);
    serde_json::to_value(&impact).map(Json).map_err(|_| serialization_failed())
}

/// `POST /v1/org/unit/removal-preview`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UnitRemovalPreviewRequest {
    /// The own-company documentKey.
    pub(crate) slug: String,
    /// The unit to remove.
    pub(crate) unit_id: String,
    /// The ISO-8601 stamp the previewed manifest would carry.
    #[serde(default)]
    pub(crate) at: Option<String>,
}

/// Build and validate the exact removal result, without writing.
///
/// A refusal names what blocks the removal and the step that clears it — a
/// generic manifest-invalid would leave the operator with no next step.
pub(crate) async fn org_unit_removal_preview(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<UnitRemovalPreviewRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let manifest = read_manifest(&source).await?;
    let at = req.at.unwrap_or_else(now_iso);
    let preview =
        preview_organization_unit_removal(&manifest, &req.unit_id, &at).map_err(|r| refused(&r))?;
    Ok(Json(serde_json::json!({
        "removedDepartmentIds": preview.removed_department_ids,
        "departedPersonIds": preview.departed_person_ids,
    })))
}

// --- activity command ------------------------------------------------------

/// `POST /v1/org/activity/command-status`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ActivityStatusRequest {
    /// The own-company documentKey.
    pub(crate) slug: String,
    /// The person the trusted adapter authenticated. Never from a Pi payload.
    pub(crate) caller_person_id: String,
}

/// Every handoff the caller still owes, plus the exact pending authority.
pub(crate) async fn org_activity_command_status(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<ActivityStatusRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    // `callerPersonId`'s own doc-comment reads "the person the trusted adapter
    // authenticated. Never from a Pi payload" — and no adapter authenticated
    // anything, because nothing on this route ever compared that field to a
    // credential. It is a DECLARED requester, so it binds.
    bind_caller_to_declared_person(&caller, &req.caller_person_id, &req.slug)?;
    let source = live(supervision_live, &req.slug)?;
    let ledger = match source.company.activity_read().await {
        Ok(Some((ledger, _))) => ledger,
        Ok(None) => return Err(RouteError::not_found("absent", "company has no activity ledger")),
        Err(error) => return Err(failed(&error)),
    };
    let status = chiefd_core::store::activity_command::activity_command_status(
        &ledger,
        &req.caller_person_id,
    )
    .map_err(|r| refused(&r))?;
    // ABSENT, NOT NULL, and the difference is the whole of this fix.
    //
    // `activity_command.rs` declares the wire shape as
    // `{ personId, pendingTransitions, activeTransitionId? }` — an OPTIONAL
    // KEY. Written through `json!`, an `Option::None` serializes as JSON
    // `null`, which is a present key with a null value, so this route violated
    // the contract its own doc comment states. The client's parser
    // (`parseActivityCommandResult`) tests `!== undefined` and then demands a
    // non-empty string naming a pending transition, so `null` is a hard throw:
    // "Organization activity result has an invalid active transition fence".
    //
    // It was unreachable dead code until yesterday. `queueAutomaticParkCompaction`
    // is this route's only caller, and until #1223 it died at a 403 on a
    // company-wide verb BEFORE ever reading status. Fixing that boundary moved
    // the wall one step later: measured on a live box 2026-08-24,
    // 12 of these between 12:54 and 13:14 across five people and still
    // accruing, with `auto-compact` requests still at zero. And it fires in the
    // COMMON case — a person with no active transition is exactly the `None`.
    //
    // The parser is not loosened to accept `null`. The server owns the declared
    // shape, and the client's strictness is the only reason anybody found this.
    let mut body = serde_json::Map::new();
    body.insert("personId".to_owned(), serde_json::Value::String(status.person_id));
    body.insert(
        "pendingTransitions".to_owned(),
        serde_json::Value::Array(
            status
                .pending_transitions
                .iter()
                .map(|transition| serde_json::to_value(transition).unwrap_or_default())
                .collect(),
        ),
    );
    if let Some(active) = status.active_transition_id {
        body.insert("activeTransitionId".to_owned(), serde_json::Value::String(active));
    }
    Ok(Json(serde_json::Value::Object(body)))
}

/// `POST /v1/org/activity/agent-state`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ActivityAgentStateRequest {
    /// The own-company documentKey.
    pub(crate) slug: String,
    /// The person the trusted adapter authenticated. Never from a Pi payload.
    pub(crate) caller_person_id: String,
    /// `true` for any activity event the pane observed, `false` for
    /// `agent_settled`.
    pub(crate) working: bool,
}

/// Record whether the caller's agent is working or has settled.
///
/// The settle countdown is a fact about an IDLE agent; before this verb existed
/// chiefd could only see the supervision ledger's demand, which says nothing
/// about whether the process is mid-turn, and stamped the quiet lease under
/// agents that were visibly working.
///
/// A beat is cheap and frequent, so a no-op (the state did not change) does NOT
/// wake the reconcile loop -- only a beat that actually moved the ledger does.
pub(crate) async fn org_activity_agent_state(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<ActivityAgentStateRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    // The beat that decides whether somebody's agent looks busy. Unbound, any
    // caller could report `working: true` under another person's name and hold
    // their automatic-settle lease open forever, or report `working: false` and
    // have chiefd park an agent that is mid-turn. It is a DECLARED requester,
    // so it binds.
    bind_caller_to_declared_person(&caller, &req.caller_person_id, &req.slug)?;
    let source = live(supervision_live, &req.slug)?;
    let applied = source
        .company
        .org_activity_note_agent_state(req.caller_person_id, req.working)
        .await
        .map_err(|error| failed(&error))?;
    if applied {
        wake_reconcile(&source);
    }
    Ok(Json(serde_json::json!({ "applied": applied })))
}

// --- staffing lifecycle ----------------------------------------------------

/// `POST /v1/org/staffing/lifecycle`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StaffingLifecycleHttpRequest {
    /// The own-company documentKey.
    pub(crate) slug: String,
    /// `bench` | `transfer` | `offboard`.
    pub(crate) action: String,
    /// Whose lifecycle.
    pub(crate) person_id: String,
    /// Destination, for transfer.
    #[serde(default)]
    pub(crate) to_department_id: Option<String>,
    /// An optional operator note recorded on the transition. NEVER required:
    /// the staffing ledger's line is authored by the daemon from the act and
    /// the authenticated actor, and a blank one here composes a default the
    /// same way `bench` always has.
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

/// Run one staffing lifecycle action end to end.
///
/// The runtime is NOT converged inline: the mutation lands and the reconcile
/// loop is woken, which is what moves or tears down the pane. A client that
/// converged runtime itself would be doing chiefd's job and racing it.
pub(crate) async fn org_staffing_lifecycle(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<StaffingLifecycleHttpRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let request = match req.action.as_str() {
        "bench" => StaffingLifecycleRequest::Bench {
            person_id: req.person_id.clone(),
            reason: req.reason.clone(),
        },
        "transfer" => StaffingLifecycleRequest::Transfer {
            person_id: req.person_id.clone(),
            to_department_id: req.to_department_id.clone().unwrap_or_default(),
            reason: req.reason.clone(),
        },
        "offboard" => StaffingLifecycleRequest::Offboard {
            person_id: req.person_id.clone(),
            reason: req.reason.clone(),
        },
        other => {
            return Err(RouteError::malformed(
                "unknown-action",
                format!("action must be bench|transfer|offboard, got {other}"),
            ))
        }
    };

    let (manifest, activity) =
        source.company.org_staffing_lifecycle_facts().await.map_err(|error| failed(&error))?;
    let plan = plan_request(&manifest, &request).map_err(|r| refused(&r))?;
    let activity = activity
        .ok_or_else(|| RouteError::not_found("absent", "company has no activity ledger"))?;
    let decision = decide(&manifest, &activity, &plan).map_err(|r| refused(&r))?;

    let at = now_iso();
    // WHO RAN THE LIFECYCLE ACTION. This route is a third door onto bench,
    // transfer and offboard, and it recorded `String::new()` as the author of
    // all three — so the one path a manager's tool actually takes was the one
    // path with no actor at all, and the guards inside those core verbs had
    // nothing to judge. Taking the caller here is the whole fix: the scope
    // checks already live in `bench_person`, `transfer_person`
    // and `offboard_person`, and a second statement of them at this seam would
    // be two answers to one question.
    let actor = caller_actor(&caller);
    // Post-commit problems this route reports as warnings on a SUCCESS, never
    // as a failure: by the time they can happen the mutation is durable, and
    // answering `ok: false` for a request that landed is what sends a manager
    // into a retry that is then refused as already applied.
    let mut settle_warnings: Vec<String> = Vec::new();
    let (structural_changed, handoff, transition_id) = match decision {
        LifecycleDecision::NoOp => (false, HandoffOutcome::Abandoned, None),
        LifecycleDecision::AlreadyApplied { transition_id, handoff } => {
            (false, handoff, Some(transition_id))
        }
        LifecycleDecision::ApplyDirectly { .. } => {
            if matches!(plan.request, StaffingLifecycleRequest::Offboard { .. }) {
                // The unattended variant, NOT the plain offboard: it withdraws
                // the launch intent in the same transaction. The plain verb
                // leaves the fence up for the handoff window, and a person with
                // no runtime generation can never complete that handoff — the
                // fence would hold a departed person's pane open forever.
                let outcome = source
                    .company
                    .org_offboard_unattended(req.person_id.clone(), at.clone(), actor.clone())
                    .await
                    .map_err(|error| failed(&error))?;
                // The refusal is surfaced rather than discarded — see
                // `apply_structural` for why a swallowed core refusal is a
                // fence that reports success.
                if let chiefd_core::store::org_ops::OffboardOutcome::Refused { reason: refusal } =
                    outcome
                {
                    return Err(RouteError::refused(refusal.code(), refusal.detail()));
                }
            } else {
                // #751/P3: every other verb is a placement change, not a
                // departure. There is no launch intent to withdraw — the person
                // has never had a pane — so the structural mutation alone is
                // the whole operation, and chiefd's reconcile places them.
                apply_structural(&source, &plan, &req.person_id, &at, &actor).await?;
            }
            (true, HandoffOutcome::Abandoned, None)
        }
        LifecycleDecision::PrepareAndApply { reuse_transition_id, reason } => {
            let transition = match reuse_transition_id {
                Some(id) => activity.transitions.get(&id).cloned(),
                None => None,
            };
            let transition = match transition {
                Some(existing) => existing,
                None => source
                    .company
                    .org_activity_prepare(BeginTransitionInput {
                        person_id: req.person_id.clone(),
                        action: plan.transition_action,
                        reason: reason.clone(),
                        to_department_id: plan.to_department_id.clone(),
                        intent_id: None,
                    })
                    .await
                    .map_err(|error| failed(&error))?,
            };
            // RELEASE the transition so its structural change may apply.
            //
            // This is the load-bearing step, and it used to wear a costume: it
            // called `org_activity_reflect` with a FABRICATED handoff
            // ("Auto-handoff for <action>: reflection fence removed.") purely
            // to get the transition to `ready`, because an applied transition
            // is what sheds launch intent and drives the pane teardown. The
            // handoff was never read by anything. Now the operation is named
            // for what it does and carries no payload at all -- delete this
            // call and a fired person's pane stays open forever.
            if !transition.status.is_released() && transition.abandoned_at.is_none() {
                source
                    .company
                    .org_activity_release(ReleaseInput {
                        transition_id: transition.id.clone(),
                        person_id: req.person_id.clone(),
                    })
                    .await
                    .map_err(|error| failed(&error))?;
            }
            apply_structural(&source, &plan, &req.person_id, &at, &actor).await?;
            // The structural rows are committed. RECORD THAT, here, instead of
            // waiting for a reconcile pass to notice.
            //
            // A move immediately followed by a second move used to be refused
            // `Person '<id>' is already assigned to '<department>'`: the first
            // moved the manifest at once, but the placement fence in
            // `begin_transition` reads the activity ledger's
            // `last_department_id`, which is the RECONCILER's
            // observation of the move rather than the move itself. A manager
            // doing the obvious thing got a false refusal until a pass landed.
            //
            // Only for a move that keeps the person (transfer). A bench or
            // offboard is a REMOVAL, and for those the reconcile does more
            // than observe — it decides whether to defer the teardown while
            // live work drains, which needs the host's observations and
            // is not this route's call to make.
            //
            // A failure here is not the operation's failure: the mutation
            // above is durable either way and the reconcile still settles it
            // exactly as it did before, so this degrades to the old timing
            // rather than to an error on a committed write.
            if keeps_person_active(&plan) {
                if let Err(error) =
                    source.company.org_activity_settle_move(req.person_id.clone()).await
                {
                    settle_warnings.push(format!(
                        "the {} committed, but recording it in the activity ledger did not complete ({error}); chiefd's reconciler settles it on its next pass",
                        request.action()
                    ));
                }
            }
            (true, handoff_outcome(&transition), Some(transition.id))
        }
    };

    wake_reconcile(&source);
    Ok(Json(serde_json::json!({
        "organization": req.slug,
        // #751/P4: every route in the staffing/structure family answers
        // `{"applied": true, …}` on 2xx, and the one client of that family
        // (`chiefdStaffingApplied` in `organization-intercom.ts`) refuses any
        // 2xx body without it rather than guess. This route alone shipped only
        // the `status` STRING, so `org_bench` and `org_offboard` committed the
        // mutation server-side and then threw `returned an invalid outcome` at
        // the manager — the same shape as the reconcile defect this packet
        // exists to fix: the change happens and the agent is told it did not.
        // `status` stays because it names the TRANSITION; `applied` names
        // whether the mutation committed, and they are not the same question.
        "applied": true,
        "action": request.action(),
        "personId": req.person_id,
        "status": "applied",
        "handoff": match handoff {
            HandoffOutcome::Completed => "completed",
            HandoffOutcome::Abandoned => "abandoned",
        },
        "retryable": false,
        "transitionId": transition_id,
        "structuralChanged": structural_changed,
        "warnings": settle_warnings,
    })))
}

/// Dispatch the plan's structural mutation to its atomic verb.
async fn apply_structural(
    source: &SupervisionLiveSource,
    plan: &chiefd_core::store::staffing_lifecycle::StaffingPlan,
    person_id: &str,
    at: &str,
    actor: &str,
) -> Result<(), Refused> {
    use chiefd_core::store::org_ops::{BenchOutcome, OffboardOutcome, TransferOutcome};
    let destination = plan.to_department_id.clone().unwrap_or_default();
    // A CORE REFUSAL IS AN ANSWER, NOT A SHRUG. Every arm below used to end in
    // `.map(|_| ())`, which threw the outcome away: a refused bench, transfer
    // or offboard was reported to the manager as `applied: true` while nothing
    // had been written. That was survivable while the only refusals were
    // preconditions the caller had usually already checked; it is not
    // survivable now that one of them is authorization, because a silently
    // swallowed `actor-out-of-scope` is a fence that reports success.
    let refusal: Option<(&'static str, String)> = match &plan.request {
        StaffingLifecycleRequest::Bench { .. } => {
            match source
                .company
                .bench_person(person_id.to_string(), at.to_string(), actor.to_string())
                .await
                .map_err(|error| failed(&error))?
            {
                BenchOutcome::Applied => None,
                BenchOutcome::Refused { reason: refusal } => {
                    Some((refusal.code(), refusal.detail().to_string()))
                }
            }
        }
        StaffingLifecycleRequest::Transfer { .. } => {
            match source
                .company
                .transfer_person(
                    person_id.to_string(),
                    destination,
                    String::new(),
                    at.to_string(),
                    actor.to_string(),
                    // The lifecycle surface carries no vacancy decision, so a
                    // head reaching it is still refused — now naming the
                    // department and the successors, and pointing at the
                    // transfer route that does take one.
                    None,
                )
                .await
                .map_err(|error| failed(&error))?
            {
                TransferOutcome::Applied { .. } => None,
                TransferOutcome::Refused { reason: refusal } => {
                    Some((refusal.code(), refusal.detail()))
                }
            }
        }
        StaffingLifecycleRequest::Offboard { .. } => {
            match source
                .company
                .offboard_person(person_id.to_string(), at.to_string(), actor.to_string())
                .await
                .map_err(|error| failed(&error))?
            {
                OffboardOutcome::Applied => None,
                OffboardOutcome::Refused { reason: refusal } => {
                    Some((refusal.code(), refusal.detail().to_string()))
                }
            }
        }
    };
    match refusal {
        None => Ok(()),
        Some((code, detail)) => Err(RouteError::refused(code, detail)),
    }
}

// TOMBSTONE: `POST /v1/org/cold-start/clear` and its `org_cold_start_clear`
// handler. Same disposition as the caller/authorize tombstone below and on the
// same evidence: the publisher-route sweep found no caller of any kind —
// `OrgSliceClient.coldStartClear` was referenced nowhere outside its own
// definition, and no Rust, script or extension posted the path. The
// `CompanyDb::cold_start_clear` seam it fronted is untouched.

// TOMBSTONE: `POST /v1/org/caller/authorize`, its `CallerAuthorizeRequest`,
// `authenticated_person_id` and `auth_refused`. The route took a CLI command
// name and its arguments and answered `{"authorized": true}`, and its only
// decision beyond "you authenticated as a person of this company" was
// `command_requires_organization_manager` — the job-title model this packet
// deletes. With that gone the route is a rubber stamp for any authenticated
// person, and it had no client: `packages/chiefing`'s `authorizeCaller`
// appeared exactly once in the tree, at its own definition. A route that
// answers "authorized" without deciding anything reads as a guarantee and is
// not one, which is the whole reason this vocabulary is going.

// --- control authority -----------------------------------------------------

/// `POST /v1/org/control-authority/person-in-scope`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PersonInScopeRequest {
    /// The own-company documentKey.
    pub(crate) slug: String,
    /// `operator`, or a person id.
    #[serde(default)]
    pub(crate) actor_person_id: Option<String>,
    /// Who is being acted on.
    pub(crate) target_person_id: String,
}

/// Whether the actor may act on the target.
///
/// An absent `actorPersonId` is the human operator: full scope by construction,
/// earned from pane ownership before the request was ever made.
pub(crate) async fn org_control_authority_person_in_scope(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<PersonInScopeRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let manifest = read_manifest(&source).await?;
    let actor = req.actor_person_id.map_or(ControlActor::Operator, ControlActor::Person);
    let in_scope = person_is_in_scope(&manifest, &actor, &req.target_person_id);
    Ok(Json(serde_json::json!({"inScope": in_scope})))
}

/// `POST /v1/org/control-authority/department-in-scope`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DepartmentInScopeRequest {
    /// The own-company documentKey.
    pub(crate) slug: String,
    /// The acting person.
    pub(crate) actor_person_id: String,
    /// The unit being acted on.
    pub(crate) department_id: String,
}

/// Whether the actor manages the unit.
///
/// `actorPersonId` is required here, unlike the person-in-scope route beside
/// it: this route asks about a NAMED person, so it always builds a
/// [`ControlActor::Person`]. A caller that wants the operator's unconditional
/// answer is not asking a question — the operator manages every unit that
/// exists by construction.
pub(crate) async fn org_control_authority_department_in_scope(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<DepartmentInScopeRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let manifest = read_manifest(&source).await?;
    let actor = ControlActor::Person(req.actor_person_id);
    let in_scope = department_is_in_scope(&manifest, &actor, &req.department_id);
    Ok(Json(serde_json::json!({"inScope": in_scope})))
}

// --- person contracts ------------------------------------------------------

/// Rebuild and publish every person's operating contract.
///
/// `published: false` means nothing changed — and nothing was written, so no
/// `AGENTS.md` mtime is re-stamped and drift detection keeps its baseline.
pub(crate) async fn org_person_contracts_build(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<SlugRequest>,
) -> Result<Json<serde_json::Value>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let (published, seq) = source
        .company
        .org_person_contracts_build(now_iso())
        .await
        .map_err(|error| failed(&error))?;
    Ok(Json(serde_json::json!({"published": published, "seq": seq})))
}

// --- shared ----------------------------------------------------------------

/// Read the company's manifest, or 404 when it has none.
async fn read_manifest(
    source: &SupervisionLiveSource,
) -> Result<chiefd_core::store::organization::OrganizationManifest, Refused> {
    match source.company.org_manifest_read().await {
        Ok(Some((manifest, _))) => Ok(manifest),
        Ok(None) => {
            Err(RouteError::not_found("unknown-company", "company has no organization manifest"))
        }
        Err(error) => Err(failed(&error)),
    }
}

/// The 500 a response that cannot be serialized answers with.
fn serialization_failed() -> Refused {
    RouteError::fault("error", "response could not be serialized")
}

/// Every person in `manifest`, mapped to the identity accent the allocator
/// gives them.
///
/// EVERY person, the chief included. The standard-identity skip that used to
/// leave `operator`/`ceo` accentless existed only because those two carried no
/// generated theme; no one carries one now, so there is nothing left to be an
/// exception to. An absent accent still means "no allocated colour" on the
/// wire — it is now reachable only by an exhausted palette.
///
/// `chief_person_id` must be read from the UNNARROWED manifest. A narrowed one
/// names its fence unit as the root, so its `chief_person_id()` answers a
/// department head; pass that and the head wears the CEO's reserved purple.
/// When the real Chief is outside the fence, no person here matches and the
/// arm simply never fires — while the allocator still holds the purple in
/// reserve, which is exactly right.
fn slice_accents(
    manifest: &chiefd_core::store::organization::OrganizationManifest,
    chief_person_id: Option<&str>,
) -> std::collections::BTreeMap<String, String> {
    let accent_order = chiefd_host::accent::identity_accent_order(&manifest.people);
    let mut accents = std::collections::BTreeMap::new();
    for person_id in &accent_order {
        if let Ok(accent) = chiefd_host::accent::organization_person_accent(
            &accent_order,
            chief_person_id,
            person_id,
        ) {
            accents.insert(person_id.clone(), accent);
        }
    }
    accents
}

#[cfg(test)]
mod tests {
    use super::slice_accents;

    /// **A FENCED SLICE'S HEAD MUST NOT WEAR THE CEO'S PURPLE.**
    ///
    /// The CEO's accent is FIXED (`accent::CHIEF_EXECUTIVE_ACCENT`) rather than
    /// allocated, so this route has to tell the allocator who the CEO is. The
    /// trap, and the reason this test exists rather than a comment: a NARROWED
    /// manifest *looks* like an organization with a chief.
    /// `narrowed_manifest` retains the fence unit, clears its parent and
    /// REWRITES `root_department_id` to it — so `chief_person_id()` on a
    /// narrowed manifest never errs and never names the real Chief. It names
    /// the FENCE'S HEAD. Reading it there hands a department head the CEO's
    /// reserved identity purple in the browser while their own pane border
    /// wears their allocated colour, which breaks the ruling and the
    /// "one decision" property this route is written for.
    #[test]
    fn a_fenced_slice_paints_its_head_their_own_colour_and_never_the_chiefs_purple() {
        let spec = serde_json::json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Quant",
                "purpose": "Model.",
                "head": { "name": "Quinn" },
                "staff": [{ "name": "Sam" }]
            }]
        });
        let full = chiefd_core::store::organization_spec::normalize_organization_spec(
            &spec,
            "2026-08-24T00:00:00.000Z",
        )
        .expect("manifest");
        let chief = full.chief_person_id().expect("a ceo").to_owned();

        // The unfenced slice: the Chief, and only the Chief, wears the purple.
        let whole = slice_accents(&full, Some(chief.as_str()));
        assert_eq!(
            whole.get(&chief).map(String::as_str),
            Some(chiefd_host::accent::CHIEF_EXECUTIVE_ACCENT)
        );
        for (person_id, accent) in &whole {
            assert!(
                person_id == &chief || accent != chiefd_host::accent::CHIEF_EXECUTIVE_ACCENT,
                "{person_id} wears the CEO's colour"
            );
        }

        let narrowed = super::super::disclosure_fence::narrowed_manifest(&full, Some("quant"))
            .expect("a fenced manifest");
        // THE TRAP ITSELF, asserted so a reader cannot mistake it for a
        // theoretical worry: the narrowed manifest answers the fence's head.
        assert_eq!(narrowed.chief_person_id().expect("a head"), "quant-head");
        assert!(!narrowed.people.contains_key(&chief), "and the real Chief is outside the fence");

        let fenced = slice_accents(&narrowed, Some(chief.as_str()));
        assert!(
            !fenced.values().any(|accent| accent == chiefd_host::accent::CHIEF_EXECUTIVE_ACCENT),
            "nobody inside a fence wears the CEO's reserved purple: {fenced:?}"
        );
        // And they keep the colour the allocator gives them for their own
        // position in the fenced order, which is what the pane border wears.
        let order = chiefd_host::accent::identity_accent_order(&narrowed.people);
        for person_id in &order {
            assert_eq!(
                fenced.get(person_id).map(String::as_str),
                chiefd_host::accent::organization_person_accent(
                    &order,
                    Some(chief.as_str()),
                    person_id
                )
                .ok()
                .as_deref()
            );
        }
    }

    /// #751/P4. `chiefdStaffingApplied` (`organization-intercom.ts`) treats a
    /// 2xx without `applied: true` as a malformed answer and throws, so the
    /// verbs that reach `/v1/org/staffing/lifecycle` — bench and offboard
    /// among them — committed their mutation server-side and then failed at
    /// the manager's tool. Observed live against a running chiefd before the
    /// fix: the body was `{"status":"applied", …}` with no `applied` key.
    ///
    /// A source assertion rather than an HTTP one on purpose. Driving this
    /// route needs a live company WITH an activity ledger and a benchable
    /// person, which is a fixture an order of magnitude larger than the
    /// property being protected; and the property is not "the route works"
    /// (`bench_lifecycle_http.rs` covers that shape for its sibling) but "the
    /// success body still carries the one key its only client keys off".
    /// `crate::docstore::router`'s own production-source assertions use the
    /// same technique.
    #[test]
    fn the_staffing_lifecycle_success_body_still_carries_applied_true() {
        let source = include_str!("org_slice.rs");
        let handler = source
            .split("pub(crate) async fn org_staffing_lifecycle")
            .nth(1)
            .expect("org_staffing_lifecycle is defined in this file");
        let body = handler
            .split("wake_reconcile(&source);")
            .nth(1)
            .expect("org_staffing_lifecycle wakes the reconciler before it answers");
        assert!(
            body.contains("\"applied\": true"),
            "the staffing/structure family answers {{\"applied\": true, …}} on 2xx; without it \
             org_bench/org_offboard commit and then throw at the manager"
        );
    }

    /// B1: the staffing lifecycle records its CALLER, not the empty string.
    ///
    /// This route is a third door onto bench, transfer and offboard, and the
    /// one a manager's tool actually takes. It passed `String::new()` as the
    /// actor, so the scope guards inside those core verbs had nothing to judge
    /// on the busiest path and the ledger named nobody as the author.
    ///
    /// The assertion is on the handler's SOURCE for the same reason as the test
    /// above: an end-to-end HTTP exercise of this route needs a live company
    /// WITH an activity ledger, which `org_manifest_genesis` does not seed, and
    /// the RULE this wiring feeds is already pinned per verb in
    /// `org_ops`'s own tests. What can rot here is the wiring, and that is what
    /// this reads.
    #[test]
    fn the_staffing_lifecycle_actor_comes_from_the_caller_and_not_the_empty_string() {
        let source = include_str!("org_slice.rs");
        let handler = source
            .split("pub(crate) async fn org_staffing_lifecycle")
            .nth(1)
            .expect("org_staffing_lifecycle is defined in this file");
        let body = handler.split("\n}\n").next().unwrap_or(handler);
        // CODE ONLY, and DO NOT SIMPLIFY THIS BACK. A handler's comments
        // explain the shape the guard forbids, and they quote it to do so — so
        // a substring search over the raw body matches the explanation and the
        // guard goes red while the code is right. That is the failure mode
        // that teaches people to delete guards. Its sibling below learned this
        // from CI rather than from foresight.
        let code: String =
            body.lines().filter(|line| !line.trim_start().starts_with("//")).collect();
        assert!(
            code.contains("caller_actor(&caller)"),
            "the staffing lifecycle takes its actor from the verified caller"
        );
        assert!(
            !code.contains("let actor = String::new();"),
            "an empty actor makes every scope guard beneath this route unjudgeable"
        );
    }

    /// B1: a core refusal reaches the caller instead of being thrown away.
    ///
    /// `apply_structural` ended every arm in `.map(|_| ())`, so a refused
    /// bench, transfer or offboard answered `applied: true` while nothing had
    /// been written. That was survivable while the refusals were preconditions;
    /// it is not survivable now that one of them is AUTHORIZATION, because a
    /// silently swallowed `actor-out-of-scope` is a fence that reports success.
    #[test]
    fn a_refused_structural_mutation_is_not_reported_as_applied() {
        let source = include_str!("org_slice.rs");
        let helper = source
            .split("async fn apply_structural")
            .nth(1)
            .expect("apply_structural is defined in this file");
        let body = helper.split("\n}\n").next().unwrap_or(helper);
        // CODE ONLY, and DO NOT SIMPLIFY THIS BACK. The comment inside that
        // helper QUOTES the shape it exists to forbid, and a naive substring
        // search over the whole body matched its own explanation — the guard
        // went red while the code was right, which is the failure mode that
        // teaches people to delete guards. CI found this one; its sibling
        // above carries the same strip for the same reason.
        let code: String =
            body.lines().filter(|line| !line.trim_start().starts_with("//")).collect();
        assert!(
            !code.contains(".map(|_| ())"),
            "discarding the outcome turns a refusal into a success body"
        );
        assert!(
            body.contains("RouteError::refused"),
            "a core refusal must reach the caller as the 422 it is"
        );
    }
}
