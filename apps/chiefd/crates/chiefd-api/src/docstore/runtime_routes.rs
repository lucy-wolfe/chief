//! The runtime / runtime-placement / materialization route family.
//!
//! # Why this is its own module
//!
//! `router.rs` is a single 7500-line file that three parallel ports were
//! editing at once. This module exposes [`merge`], so the whole family costs
//! exactly ONE line in the route table. The handlers, their request/response
//! shapes and their state extractors are otherwise identical to the ones in
//! `router.rs` — same `Extension<Option<SupervisionLiveSource>>` gate, same
//! own-company slug filter, same `company_error` mapping.
//!
//! # What it replaced
//!
//! Seventeen TypeScript modules under `apps/cli/src/legacy/organization`,
//! deleted with this branch: `org-runtime.ts`, `org-the runtime.ts`,
//! `org-materialize.ts`, `org-runtime-ownership.ts`,
//! `org-runtime-projection.ts`, `org-model-command.ts`,
//! `org-company-session-actions.ts`, `org-loop-control.ts`,
//! `org-monitor-reader.ts`, `org-extension-runtime-drift.ts` and their
//! siblings. Every decision they made is now Rust; TypeScript reaches this
//! surface through `@chief/chiefing`'s `RuntimeClient` and holds no logic of
//! its own (mandate 3).
//!
//! # The two capabilities a handler here may need
//!
//! `host_executor` and `reconcile_actuator_config` are `None` on every router
//! except the one `chiefd run` assembles. A handler that needs runtime or the
//! disk therefore refuses with `503` rather than constructing an executor of
//! its own — the same daemon-only-capability shape
//! `/v1/org/projection/reconcile` already uses.

use std::sync::Arc;

use axum::routing::post;
use axum::{Extension, Json, Router};

use super::route_error::RouteError;
use super::router::{company_error, require_company_wide_authority, SupervisionLiveSource};
use super::DocStore;

/// WHO DID IT, taken from the extractor rather than the body.
///
/// `LaunchRequest.actor` is caller-asserted audit prose — it is written into the
/// runtime record and read back by an operator, and nothing ever tied it to
/// whoever was calling. Every caller is proven now, so the principal always
/// replaces the claim and the body field decides nothing.
fn actor_of(caller: &chiefd_core::store::identities::Identity) -> String {
    caller.principal.clone()
}

/// Wall-clock now as the ISO-8601 millisecond stamp every store write records.
///
/// `isotime::iso_millis` takes epoch MILLIS (`i64`), not a `SystemTime`; the
/// nine call sites in this file were passing `SystemTime::now()` directly.
/// A saturating conversion keeps it total — a pre-1970 clock reads as 0 rather
/// than panicking a request handler.
fn now_iso() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX));
    chiefd_core::isotime::iso_millis(millis)
}

/// Add the runtime family to the route table.
///
/// One call site in `router.rs`, immediately after the base table is built and
/// before the layers are applied, so these routes get the same live-source
/// resolution, auth and body limit as every other `/v1/org/*` route.
// TOMBSTONE (chief-home-is-cwd §4d): the whole `POST /v1/org/materialize/*`
// family — `run`, `stale`, `ensure-current` and `extension-drift` — plus
// `POST /v1/org/resource-catalog/read`, were registered by this function.
//
// The first four asked a caller to decide when a home should be re-projected
// from SQL and reported how far behind it had fallen. Nothing is projected, so
// a home cannot be stale and cannot drift: `agent_home::ensure_agent_home`
// creates a missing one on the hire path and touches a present one never. The
// fifth listed the skills, extensions and packages a person could be hired
// with; nobody is hired with one, because the skills an agent has are whatever
// is in `<dir>/.pi/skills` when Pi looks.
//
// TOMBSTONE (2026-08-24): the five `/v1/org/company-session-action/*` routes —
// `queue`, `progress`, `unresolved`, `skip-parked` and `reconcile-claims`.
// Deleted with `org_maintain_session`; nothing in production could ever queue a
// company action.
//
// These sit ABOVE the function rather than trailing its last `.route(...)`,
// where they were: a comment at the end of a builder chain has no expression to
// attach to, and rustfmt reflows it into the enclosing block.
pub(super) fn merge(router: Router<Arc<DocStore>>) -> Router<Arc<DocStore>> {
    router
        // runtime lifecycle
        .route("/v1/org/runtime/launch", post(runtime_launch))
        // TOMBSTONE (chief-home-is-cwd §4c): `/v1/org/runtime/launch-ceo-only`
        // was registered here. It asked chiefd to bring the company's first
        // pane up itself, under an exclusivity lease, and prove it came up.
        // The operator client owns every pane, so the route had no work to do
        // and its handler is deleted with `launch_ceo_only_runtime`.
        .route("/v1/org/runtime/resume", post(runtime_resume))
        .route("/v1/org/runtime/stop", post(runtime_stop))
        .route("/v1/org/runtime/ownership/read", post(runtime_ownership_read))
        .route("/v1/org/runtime/ownership/claim", post(runtime_ownership_claim))
        .route("/v1/org/runtime/ownership/release", post(runtime_ownership_release))
}

/// The own-company gate every handler here starts with.
///
/// This process serves exactly one company. A request naming any other slug is
/// not an error to be reported, it is simply not ours — `404` with the same
/// `{code, detail}` body the rest of the `/v1/org/*` family uses.
fn live(
    supervision_live: Option<SupervisionLiveSource>,
    slug: &str,
) -> Result<SupervisionLiveSource, RouteError> {
    supervision_live
        .filter(|source| slug == source.org_documents_slug)
        .ok_or_else(|| RouteError::not_found("unknown-company", "no live company for this slug"))
}

/// The two halves of this daemon's runtime host capability, borrowed from the
/// live source that owns them.
type HostCapability<'a> =
    (&'a Arc<dyn chiefd_host::HostExecutor>, &'a Arc<chiefd_host::converge_apply::ActuatorConfig>);

/// The runtime executor plus its actuator configuration, or a `503`.
///
/// Absent everywhere but `chiefd run`. A handler must never build its own —
/// an executor constructed in a handler would not share the daemon's
/// `EverObserved`, and "have I ever seen this person's pane" is exactly the
/// fact that must not be reset per request.
fn host(source: &SupervisionLiveSource) -> Result<HostCapability<'_>, RouteError> {
    match (source.host_executor.as_ref(), source.reconcile_actuator_config.as_ref()) {
        (Some(executor), Some(config)) => Ok((executor, config)),
        _ => Err(RouteError::unavailable(
            "no-runtime-host-capability",
            "this chiefd has no runtime host capability",
        )),
    }
}

/// **The fix this module exists for.** Classify one runtime-lifecycle failure
/// instead of calling every one of them a server fault.
///
/// This replaced a local `internal()` that answered **500 + plain text** for
/// every outcome of every `chiefd_host::runtime_lifecycle::*` call. Read the
/// variants: `Store(ChiefdError::Refused(..))` is a stale runtime generation, a
/// foreign identity, an unjustified thinking elevation, an unusable launcher
/// root — every one of them a rule the caller can act on and none of them a
/// fault. `HandoffRefused` is ten authorization decisions in
/// `close_temporary_launcher_pane`. All of it was 500, so the client turned all
/// of it into `chiefd unavailable (http-error)`, and an agent retrying against
/// a generation fence that will never open retried forever.
///
/// The remaining variants really are faults or non-service, and are named as
/// such rather than lumped: a runtime tool that will not run is 503 (come back),
/// a runtime tool that ran and failed is 500 (an operator), and a convergence
/// window that expired is 503 with the operator-facing sentence intact.
fn lifecycle_error(error: &chiefd_host::runtime_lifecycle::RuntimeLifecycleError) -> RouteError {
    use chiefd_host::runtime_lifecycle::RuntimeLifecycleError as Lifecycle;
    match error {
        Lifecycle::Store(store) => RouteError::from_chiefd(store),
        Lifecycle::HandoffRefused(detail) => RouteError::refused("handoff-refused", detail),
        // TOMBSTONE (chief-home-is-cwd §4c): `Lifecycle::CeoPaneLiveness` mapped
        // to the 503 refusal `ceo-pane-not-live`. The only thing that could
        // produce it was the daemon waiting on a pane it had launched itself,
        // and the daemon launches none.
        Lifecycle::StopDidNotConverge(detail) => {
            RouteError::unavailable("stop-did-not-converge", detail)
        }
        Lifecycle::Materialize(chiefd_host::materialize::MaterializeError::Refused(refusal)) => {
            RouteError::refused(refusal.code, refusal.message.clone())
        }
        Lifecycle::Materialize(other) => {
            RouteError::fault("materialization-failed", other.to_string())
        }
        // #751/P8-P10: `Lifecycle::Observe` is gone with the observation it
        // wrapped. An unproven observation now arrives as
        // `HostErr::Untrusted` from the committed actuation record and takes
        // the same 503 exit it always did, one line up.
        Lifecycle::Host(host) => host_error(host),
        Lifecycle::Converge(duty) => RouteError::fault("converge-failed", duty.to_string()),
    }
}

/// A host-tool failure, split the one way that matters to a caller: a tool that
/// could not be RUN at all, or an observation the trust rules say to disbelieve,
/// is chiefd not currently serving (503). A tool that ran and reported failure,
/// or a filesystem step that broke, is a fault an operator looks at (500).
fn host_error(error: &chiefd_host::HostErr) -> RouteError {
    match error {
        chiefd_host::HostErr::ToolUnavailable { .. } | chiefd_host::HostErr::Untrusted { .. } => {
            RouteError::unavailable("host-tool-unavailable", error.to_string())
        }
        chiefd_host::HostErr::ToolFailed { .. } | chiefd_host::HostErr::Filesystem { .. } => {
            RouteError::fault("host-step-failed", error.to_string())
        }
    }
}

/// A response body chiefd built and then could not serialize. Always a fault:
/// nothing the caller did produced it.
fn encode_fault(error: impl std::fmt::Display) -> RouteError {
    RouteError::fault("encode-failed", error.to_string())
}

// ---- request shapes -----------------------------------------------------

/// Every route in this family names its company, and that is normally all it
/// needs: the daemon serves exactly one company and actuates on exactly one
/// terminal socket, which it already holds in its own
/// [`ActuatorConfig`](chiefd_host::converge_apply::ActuatorConfig).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SlugRequest {
    slug: String,
}

/// A launch decides two things the committed rows cannot: WHO was explicitly
/// asked for, and who merely holds an execution lease. The second set is
/// deliberately NOT durable start intent — persisting it would leave a manager
/// resident after one completed tool call and break the minimum-fleet rule.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchRequest {
    slug: String,
    #[serde(default)]
    requested_person_ids: Vec<String>,
    #[serde(default)]
    execution_lease_person_ids: Vec<String>,
    /// ACCEPTED AND IGNORED — the launch record names the authenticated
    /// caller's principal, never this claim. See [`actor_of`].
    #[serde(rename = "actor")]
    _actor: String,
}

impl LaunchRequest {
    /// `actor` is supplied by the CALLER of this function, not read off `self`
    /// — see [`actor_of`]. A launch that recorded the body's claim would be
    /// recording who the request SAID was launching, beside an identity the
    /// daemon had actually proven.
    fn options(self, actor: String) -> chiefd_host::runtime_lifecycle::LaunchOptions {
        chiefd_host::runtime_lifecycle::LaunchOptions {
            at: now_iso(),
            requested_person_ids: self.requested_person_ids,
            execution_lease_person_ids: self.execution_lease_person_ids,
            actor,
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StopRequest {
    slug: String,
    // `localTeardownReason` is gone (#751/P8-P10). It let a caller holding the
    // CEO-boot suppression lease ask chiefd to kill the session itself instead
    // of committing the stop and waiting for it to converge. chiefd cannot kill
    // a session from any path now, so the field could only ever have been
    // accepted and ignored — which is worse than refusing it.
}

/// The runtime change signal a bounded wait parks on.
///
/// One per company, owned by the daemon and nudged by its change-feed sink, so
/// a wait started by a route sees commits made by the converge cycle. A route
/// that minted its own would park on a signal nothing nudges — a wait that can
/// only ever time out is worse than no wait at all.
fn runtime_signal(
    source: &SupervisionLiveSource,
) -> Result<&chiefd_host::runtime_lifecycle::RuntimeChangeSignal, RouteError> {
    source.runtime_change_signal.as_deref().ok_or_else(|| {
        RouteError::unavailable(
            "no-runtime-change-signal",
            "this chiefd has no runtime change signal",
        )
    })
}

// TOMBSTONE: `manifest_of`. It read the committed manifest for the
// materialization handlers, which are the only routes in this family that ever
// needed one — every survivor works on the runtime rows and the ownership
// claim, neither of which names a person.

// ---- runtime lifecycle --------------------------------------------------

// TOMBSTONE: `POST /v1/org/runtime/observe` and its `runtime_observe` handler.
//
// This was chiefd answering "what is actually running", derived from the
// actuator's committed observation. The observation is deleted and the
// direction it represented is barred, so the route cannot be answered — not
// "is answered less well", but has no input at all. Deleted rather than left
// returning an empty or unknown shape: a route that answers "nobody is
// running" because it can no longer look is precisely the unreadable-becomes-
// empty conflation this whole change exists to remove, and it would be handing
// that conflation to every client instead of to one reconcile.
//
// `unexpectedObservedPersonIds` lived on its answer and is NOT deleted by
// name-similarity — it is a separate desired-side projection with a separate
// meaning (`runtime_projection.rs`), and the plan says so explicitly. Only the
// feed FROM the observation dies here.

/// # The fence, and why it runs before [`host`]
///
/// A launch projects panes for the WHOLE COMPANY. It names no person target to
/// scope against — `requestedPersonIds` widens the fleet rather than being the
/// subject — so the department it reaches is the ROOT department, and the check
/// is the ordinary subtree one over it. Only somebody who heads the root passes;
/// daemon-scoped identities keep the unconditional scope `control_authority`
/// already grants them, which is what keeps the operator client launching.
///
/// It runs BEFORE the host-capability check on purpose: a caller with no
/// authority must not be able to tell, from a 503 versus a 403, whether the
/// daemon it is talking to holds a runtime host.
async fn runtime_launch(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<LaunchRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = live(supervision_live, &req.slug)?;
    require_company_wide_authority(&caller, &source, "launch the runtime").await?;
    let actor = actor_of(&caller);
    let (_executor, config) = host(&source)?;
    let report = chiefd_host::runtime_lifecycle::launch_runtime(
        &source.company,
        config,
        &req.options(actor),
    )
    .await
    .map_err(|error| lifecycle_error(&error))?;
    // A launch changes who may hold a pane, so the live reconciler is woken
    // rather than left to its fallback floor.
    if let Some(trigger) = source.reconcile_trigger.as_ref() {
        trigger.notify_waiters();
    }
    serde_json::to_value(report).map(Json).map_err(encode_fault)
}

/// A resume never opens the launch fence, which is why it carries no requested
/// person ids even though it shares `LaunchRequest`'s shape.
async fn runtime_resume(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<LaunchRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = live(supervision_live, &req.slug)?;
    require_company_wide_authority(&caller, &source, "resume the runtime").await?;
    let actor = actor_of(&caller);
    let (_executor, config) = host(&source)?;
    let report = chiefd_host::runtime_lifecycle::resume_supervised_runtime(
        &source.company,
        config,
        &req.options(actor),
    )
    .await
    .map_err(|error| lifecycle_error(&error))?;
    serde_json::to_value(report).map(Json).map_err(encode_fault)
}

async fn runtime_stop(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<StopRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = live(supervision_live, &req.slug)?;
    require_company_wide_authority(&caller, &source, "stop the runtime").await?;
    let (_executor, config) = host(&source)?;
    let signal = runtime_signal(&source)?;
    let options = chiefd_host::runtime_lifecycle::StopOptions {
        at: now_iso(),
        // `stop_supervised_runtime` sets this itself — an attended stop always
        // leaves the company CEO-only — so the route does not get a say.
        clear_launch_intent: true,
    };
    let report = chiefd_host::runtime_lifecycle::stop_supervised_runtime(
        &source.company,
        config,
        signal,
        &options,
    )
    .await
    .map_err(|error| lifecycle_error(&error))?;
    serde_json::to_value(report).map(Json).map_err(encode_fault)
}

// The Founder/launcher → company handoff route is GONE (#751/P8-P10), and so is
// `POST /v1/org/runtime/close-temporary-pane`.
//
// Every field it took was something the CALLER observed about its own terminal
// — its own pane id, its own socket path, its own role — and every one of its
// ten refusals was chiefd proving those observations against a display it can
// no longer see. A client does not need a backend's permission to move its own
// viewers between its own sessions and close its own pane, and a backend that
// cannot see either one cannot audit the request anyway. The operator client
// performs the handoff locally, with no wire call in it.

/// Who owns this company's runtime.
///
/// This route exists so no client re-derives the answer. `CompanyDb::
/// runtime_ownership_read` applies `store::runtime_ownership::validate_ownership`
/// and derives the documented initial state ("released") for a company that has
/// never claimed one — a client reading the raw `runtime-owner` row instead
/// would have to re-implement BOTH, and a validator that disagreed with the
/// daemon's about (say) an active row missing its socket is how a CLI and its
/// daemon end up with different views of who holds the session.
async fn runtime_ownership_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<SlugRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = live(supervision_live, &req.slug)?;
    let owner =
        source.company.runtime_ownership_read().await.map_err(|error| company_error(&error))?;
    serde_json::to_value(owner).map(Json).map_err(encode_fault)
}

/// The claim's two audit inputs are observed HERE, immediately before the
/// transaction that consumes them, and never inside it: the decision must be
/// pure and the transaction must not do I/O.
///
/// # AC6: the owner is `config.socket`, never a caller-supplied one
///
/// This pair took a `socketName` on the wire, and #751-P9 left it there on the
/// grounds that the value is written into and compared against the durable
/// runtime-owner record, so moving who supplies it might move takeover
/// semantics. Worked through, it does not — and the caller-supplied version
/// was the unsafe half:
///
/// * `Takeover` is UNREACHABLE from this route and always was.
///   `observe_prior_ownership` defines `prior_projection_exists` as "an
///   ACTIVE record names a socket other than the requesting one", which is
///   verbatim the precondition `audit_ownership` requires before it will
///   return `Takeover` — and it refuses on that same fact one line earlier.
///   The only reachable outcomes are `Unchanged` and a refusal, whoever
///   supplies the name.
/// * The daemon's OWN claim and release (`runtime_lifecycle::claim_ownership`,
///   `stop_supervised_runtime`) already pass `config.socket`. A caller naming
///   anything else could therefore only ever produce a spurious
///   `runtime-ownership-projection-live` refusal, or — over a released
///   company — write a socket the daemon does not hold, after which the
///   daemon's own claim refuses and it is locked out of its own company.
///
/// What the record needs to identify an owner is a durable string that
/// distinguishes one actuator from another and that chiefd never parses.
/// `config.socket` is exactly that, and it is client-supplied where it should
/// be — once, at daemon start (`--runtime-socket`), by the operator client
/// that actually drives the display. It is not re-litigated per request by
/// whoever happens to be calling.
/// # This route has NO PRODUCTION CALLER, and that is a fact worth knowing
/// before reaching for it
///
/// Established 2026-08-11 by tracing every path, after a measurement harness
/// waited on the lease this route grants and deadlocked:
///
/// * `chiefing`'s `claimOwnership()` is a client method nothing calls;
/// * `chief-cli` never references `runtime_lifecycle` at all — the actuator
///   converges and the DAEMON launches;
/// * every REAL claim happens in-process inside
///   [`chiefd_host::runtime_lifecycle::claim_ownership`], which only
///   `launch_runtime` and `stop_supervised_runtime` call.
///
/// So the lease is claimed when the runtime PROJECTS OR TEARS DOWN a session,
/// never when an actuator merely starts, and a converged actuator with nothing
/// to run reads `released` correctly. This route is the only way to obtain the
/// lease WITHOUT doing either — which is exactly why a caller should say in
/// writing why it wants ownership that no launch is backing, rather than
/// discovering later that a lease exists which nothing else expects.
///
/// Deliberately NOT deleted: it is the honest standalone spelling of a verb the
/// launch paths use, and the audit it performs is the same one. Deliberately
/// not guarded by `two_implementation_stores.rs` either — that guard's subject
/// is chiefd WRITING a store it only reads, and runtime ownership is chiefd's
/// own; a guard that reddened when a public HTTP route gained a caller would be
/// the wrong shape for an API surface.
async fn runtime_ownership_claim(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<SlugRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = live(supervision_live, &req.slug)?;
    require_company_wide_authority(&caller, &source, "claim the runtime ownership lease").await?;
    let (_executor, config) = host(&source)?;
    let socket_name = config.socket.clone();
    let observation = chiefd_host::runtime_lifecycle::observe_prior_ownership(
        &source.company,
        config,
        &socket_name,
    )
    .await
    .map_err(|error| lifecycle_error(&error))?;
    let (owner, verdict) = source
        .company
        .runtime_ownership_claim(socket_name, observation.prior_projection_exists, now_iso())
        .await
        .map_err(|error| company_error(&error))?;
    let previous = match &verdict {
        chiefd_core::store::runtime_ownership::OwnershipVerdict::Takeover {
            previous_socket_name,
        } => Some(previous_socket_name.as_str()),
        chiefd_core::store::runtime_ownership::OwnershipVerdict::Unchanged => None,
    };
    Ok(Json(ownership_result_body(owner, verdict.is_takeover(), previous)))
}

/// One `RuntimeOwnershipResult` body, for BOTH ownership routes.
///
/// # OPTIONAL KEYS ARE ABSENT, NEVER `null`
///
/// `RuntimeOwnershipResult` declares `socketName?: string` and
/// `previousSocketName?: string`, and `RuntimeOwner` even carries
/// `#[serde(skip_serializing_if = "Option::is_none")]` on `socket_name` — but
/// both routes used to build their bodies with a hand-written `json!`, which
/// BYPASSES the struct's own attribute and turns `None` into a present,
/// null-valued key. That is the nastier half of this shape: a reader auditing
/// the TYPE concludes a safety the ROUTE does not deliver.
///
/// No client throws on it today — the only reader is a falsy check rather than
/// a validator — so this was the latent sibling of the
/// `activeTransitionId`/`null` fence, not a second live fault. It is written
/// once here so the two routes cannot drift, and so the rule is testable
/// without a runtime host: driving either route to a `200` needs a host
/// capability the caller-fence harness deliberately does not have, which is
/// why the fix shipped untested and this function exists.
fn ownership_result_body(
    owner: chiefd_core::store::runtime_owner_rows::RuntimeOwner,
    takeover: bool,
    previous_socket_name: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert("organization".to_owned(), serde_json::Value::String(owner.organization));
    body.insert("status".to_owned(), serde_json::to_value(owner.status).unwrap_or_default());
    if let Some(socket) = owner.socket_name {
        body.insert("socketName".to_owned(), serde_json::Value::String(socket));
    }
    body.insert("takeover".to_owned(), serde_json::Value::Bool(takeover));
    if let Some(previous) = previous_socket_name {
        body.insert(
            "previousSocketName".to_owned(),
            serde_json::Value::String(previous.to_owned()),
        );
    }
    serde_json::Value::Object(body)
}

/// Release this company's runtime from the socket the DAEMON holds.
///
/// Same rule as the claim above, and the same reason it is not a semantic
/// change: `stop_supervised_runtime` already releases with `config.socket`,
/// and `released_ownership` refuses a release from any other socket — so a
/// caller naming one could only ever get `runtime-ownership-release-foreign`
/// back. Taking the name off the wire deletes the refusal's only cause.
async fn runtime_ownership_release(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<SlugRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = live(supervision_live, &req.slug)?;
    require_company_wide_authority(&caller, &source, "release the runtime ownership lease").await?;
    let (_executor, config) = host(&source)?;
    let owner = source
        .company
        .runtime_ownership_release(config.socket.clone(), now_iso())
        .await
        .map_err(|error| company_error(&error))?;
    Ok(Json(ownership_result_body(owner, false, None)))
}

// ---- runtime drift, monitors, loops -------------------------------------

// TOMBSTONE: `runtime_drift_body`, `POST /v1/org/runtime/extension-drift` and
// `POST /v1/org/runtime/deploy-drift`.
//
// The scan asked "which RUNNING people are loading stale extension code?", and
// it answered by joining the actuator's committed observation against mtimes on
// disk. It was a good question and it is now the wrong shape of answer: it made
// chiefd read a host fact in order to tell an operator to go and restart people
// by hand.
//
// THE GUARANTEE IS PRESERVED BY CONSTRUCTION, which is why this is a deletion
// and not a loss. The extension source digest is an INPUT to
// `desired_launch_hash`, so a launcher deploy moves every affected person's
// published hash, their pane's `@organization_launch_hash` tag no longer
// matches, and the actuator replaces them on its next pass. Nobody has to be
// told, because nobody has to act: a stale pane cannot survive a converge pass.
// There is no longer a question to ask, and `deploy_drift_report`'s verdict --
// which existed so a deploy script could get an exit code -- is answered by the
// deploy itself having changed the hash.
//
// `POST /v1/org/materialize/extension-drift` asked the same question with a
// different subject — what is on DISK versus what should be — and it is
// deleted too, one stage later and for a stronger reason: chiefd copies nothing
// into a person's home, so the question has no left-hand side at all.

// ---- model / thinking ---------------------------------------------------

// TOMBSTONE: the five company-session-action handlers and their request
// bodies — queue, progress, unresolved, skip-parked and reconcile-claims —
// plus `company_action_runtime`, the liveness probe they shared.
//
// The chiefd half of #54's company-wide native reset and compact actions,
// deleted whole with the feature. Nothing in production could queue one: the
// only caller of the queue verb was chiefing's own client method, exercised by
// contract tests, and the historical queuer was the legacy CLI deleted in
// `ca2da9b57`.

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    /// The own-company gate is the first thing every handler does. A foreign
    /// slug is not an error to report — it is simply not this process's
    /// company — so it must be a 404 and never a 500 or a panic.
    #[test]
    fn a_foreign_slug_is_not_this_process_company() {
        let result = live(None, "someone-else");
        assert_eq!(result.err().map(|error| error.status()), Some(StatusCode::NOT_FOUND));
    }

    /// Is `path` registered on the router [`merge`] actually builds?
    ///
    /// axum exposes no route table, so this asks the router the only way it
    /// answers: registering the same method+path a second time PANICS
    /// (`Overlapping method route. Handler for POST /… already exists`). A
    /// panic therefore means the family already owns the path; a clean return
    /// means nothing there claimed it. The probe handler is never called — the
    /// registration itself is the whole question.
    ///
    /// The panics raised here are libtest-captured and printed only if this
    /// test fails, so the global panic hook is deliberately left alone: a
    /// suppressed hook would race any other test in this crate that panics
    /// while this one runs, and swallow its message.
    fn family_registers(path: &str) -> bool {
        let outcome = std::panic::catch_unwind(|| {
            let router: Router<Arc<DocStore>> = merge(Router::new());
            let _ = router.route(path, post(|| async {}));
        });
        outcome.is_err()
    }

    /// Every route in the family must be reachable. This pins the count so a
    /// route deleted by a bad merge is visible rather than silently absent —
    /// the failure mode where an instrument reports success because it cannot
    /// see its subject.
    ///
    /// # This guard used to be that failure mode
    ///
    /// Until #862 it read its own file with `include_str!("runtime_routes.rs")`
    /// and asserted each documented path appeared somewhere in the text as a
    /// quoted literal. The documented list is a literal array *in that same
    /// file*, so every entry matched itself: the assertion held for any input
    /// whatsoever, and deleting a real `.route(…)` line changed nothing it
    /// could see. It now builds the router and interrogates it, which is what
    /// its name always claimed.
    /// AC6: NO route in this family takes a terminal socket, and the last two
    /// that did were the ownership pair.
    ///
    /// Both directions matter. A body that names only the company must be
    /// accepted — that is the contract — and a body that still smuggles a
    /// `socketName` must be IGNORED rather than honoured, so an old client
    /// cannot steer the daemon at a socket it does not actuate. Every handler
    /// uses `config.socket`, which no request can reach.
    #[test]
    fn no_route_in_the_family_takes_a_terminal_socket() {
        let bare: SlugRequest =
            serde_json::from_value(serde_json::json!({ "slug": "cobalt@x" })).expect("slug only");
        assert_eq!(bare.slug, "cobalt@x");

        let smuggled: SlugRequest = serde_json::from_value(
            serde_json::json!({ "slug": "cobalt@x", "socketName": "someone-elses" }),
        )
        .expect("an unmodeled field is ignored, not honoured");
        assert_eq!(smuggled.slug, "cobalt@x");

        // The assertion that keeps the two above meaningful. `SocketRequest`
        // — the shape the ownership pair demanded, which would have REFUSED
        // the bare body — is deleted, so no request type in this module may
        // name a socket or a session at all. A text check is the right
        // instrument here precisely because the type it is looking for no
        // longer exists to be named.
        // Production source only — the scan must not read this test module,
        // which necessarily spells the banned words in order to ban them.
        let production = include_str!("runtime_routes.rs")
            .split("\n#[cfg(test)]\n")
            .next()
            .expect("the file has a production half");
        let requests = production
            .split("#[derive(serde::Deserialize)]")
            .skip(1)
            .map(|block| block.split("\n}\n").next().unwrap_or_default())
            .collect::<Vec<_>>();
        assert!(!requests.is_empty(), "the scan found no request shapes at all");
        for block in requests {
            for banned in ["socket", "session_name"] {
                assert!(
                    !block.contains(banned),
                    "a request shape in this family names `{banned}`:\n{block}"
                );
            }
        }
    }

    // TOMBSTONE: `the_runtime_drift_body_says_nobody_looked_instead_of_answering_null`
    // and `a_scan_that_ran_and_found_nobody_stale_still_answers_a_null_summary`.
    //
    // Both pinned a genuinely good property -- that an empty `drift` list must
    // not read the same whether the scan RAN and found nobody or nobody ever
    // looked. That is the same unreadable-versus-empty distinction this whole
    // change is about, and it is exactly why these tests are deleted rather
    // than weakened: chiefd cannot look at all now, so there is no scan with
    // two possible meanings to keep apart. The route, its body function and its
    // input are all gone.
    //
    // What the property protected survives as a construction rather than a
    // report: a launcher deploy moves each affected person's launch hash, so a
    // stale pane fails the actuator's diff and is replaced. There is no state
    // in which a reader could mistake "nobody looked" for "nobody is stale",
    // because nothing looks and nothing reports.

    #[test]
    fn the_family_registers_every_route_it_documents() {
        let paths = [
            "/v1/org/runtime/launch",
            "/v1/org/runtime/resume",
            "/v1/org/runtime/stop",
            "/v1/org/runtime/ownership/read",
            "/v1/org/runtime/ownership/claim",
            "/v1/org/runtime/ownership/release",
        ];
        // 6, down from 11: the five `/v1/org/company-session-action/*` routes
        // went with the feature — nothing in production could ever queue one.
        //
        // Before that, 11 down from 16. The five that went are the whole
        // `POST /v1/org/materialize/*` family and
        // `POST /v1/org/resource-catalog/read` (chief-home-is-cwd §4d): with no
        // projection there is nothing to run, nothing to probe for staleness,
        // nothing to compare for drift, and no catalog to list. Before that, 21
        // -- `/v1/org/materialize/model-selection`, `/v1/org/model/change`,
        // `/v1/org/thinking/change` and `/v1/org/model/migrate` went with
        // provider/model management, and `/v1/org/runtime/launch-ceo-only` with
        // the daemon-side CEO boot (§4c). The count is asserted so a route
        // silently dropped from this list is a failure rather than a shorter
        // loop.
        assert_eq!(paths.len(), 6);
        for path in paths {
            assert!(
                family_registers(path),
                "{path} is documented but `merge` does not register it as a route"
            );
        }
    }

    /// The other direction, and the reason the assertion above is worth
    /// anything: the probe must be able to say NO. A `family_registers` that
    /// answered `true` unconditionally — because the panic it keys off stopped
    /// happening, or because a wildcard fallback started swallowing every path
    /// — would keep the guard above green with every route deleted.
    #[test]
    fn a_path_the_family_does_not_register_is_reported_as_unregistered() {
        assert!(!family_registers("/v1/org/runtime/no-such-route"));
        // A near-miss too: prefix matching is not registration.
        assert!(!family_registers("/v1/org/runtime"));
        assert!(!family_registers("/v1/org/runtime/launch/deeper"));
    }

    // TOMBSTONE: the three `RuntimeLiveness` tests, the banner above them, and
    // the two helpers only they used — `live_company` (a `SupervisionLiveSource`
    // over a real seeded company database) and `publish_owner`.
    //
    // They pinned `company_action_runtime` in BOTH directions after it had
    // returned `Stopped` for every company on every call — its second conjunct
    // read a table whose writer had been deleted, and `Stopped` was the correct
    // answer for a released company, so only asserting the other direction
    // against a real database exposed the pin. That probe existed solely to
    // decide whether a company action could fan out, and it is deleted with the
    // family.
    //
    // The lesson survives its subject and is worth keeping: a one-directional
    // test would not have caught it, because the wrong answer was also a
    // legitimate answer.
    /// **AN OPTIONAL KEY IS ABSENT, NOT NULL — and nothing pinned it.**
    ///
    /// The two ownership routes were fixed in #1226 alongside the
    /// `activeTransitionId` fence and shipped with NO test, which is the
    /// failure this file's own subject warns about: a later tidy reverting
    /// these bodies to `json!` would regress both with every test green, and
    /// `json!` on an `Option::None` writes a present, null-valued key.
    ///
    /// Asserted as KEY ABSENCE (`contains_key`), never as `!= null`, because a
    /// present-and-null key is exactly the bug — a null-ness assertion passes
    /// on the broken code.
    ///
    /// It tests the shared BODY BUILDER rather than the routes, deliberately:
    /// driving either route to a `200` needs a runtime host capability the
    /// caller-fence harness does not have, so a route-level test could only
    /// have asserted the refusal. Both routes now funnel through this one
    /// function, so there is nothing left for them to disagree about.
    #[test]
    fn an_ownership_body_omits_the_optional_keys_it_has_no_value_for() {
        use chiefd_core::store::runtime_owner_rows::{RuntimeOwner, RuntimeOwnerStatus};
        let owner = |socket: Option<&str>| RuntimeOwner {
            version: 1,
            organization: "northstar-conformance".to_owned(),
            status: RuntimeOwnerStatus::Released,
            socket_name: socket.map(str::to_owned),
            claimed_at: None,
            validated_at: None,
            released_at: None,
            extra: Default::default(),
        };

        let released = super::ownership_result_body(owner(None), false, None);
        let released = released.as_object().expect("an object body");
        assert!(
            !released.contains_key("socketName"),
            "an owner with no socket has NO socketName key: {released:?}"
        );
        assert!(
            !released.contains_key("previousSocketName"),
            "a non-takeover has NO previousSocketName key: {released:?}"
        );
        // NON-VACUITY: the non-optional keys are still written, so a change
        // that dropped everything would not pass this.
        assert_eq!(
            released.get("organization").and_then(serde_json::Value::as_str),
            Some("northstar-conformance")
        );
        assert_eq!(released.get("takeover").and_then(serde_json::Value::as_bool), Some(false));

        // AND THE OTHER DIRECTION, so the fix cannot degenerate into "never
        // report a socket": present values are still reported, as strings.
        let taken = super::ownership_result_body(owner(Some("default")), true, Some("old-socket"));
        let taken = taken.as_object().expect("an object body");
        assert_eq!(taken.get("socketName").and_then(serde_json::Value::as_str), Some("default"));
        assert_eq!(
            taken.get("previousSocketName").and_then(serde_json::Value::as_str),
            Some("old-socket")
        );
        assert_eq!(taken.get("takeover").and_then(serde_json::Value::as_bool), Some(true));
    }
}
