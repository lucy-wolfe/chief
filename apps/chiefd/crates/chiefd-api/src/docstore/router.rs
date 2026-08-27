//! The typed HTTP surface for the `org_documents` contract.
//!
//! Every route is a semantic operation with a typed body, never a SQL string.
//! The route set maps one-to-one onto the public methods of
//! `SqliteDurableStore` / `DurableLock` in
//! `src/organization/org-durable-store.ts`, which is what makes the Phase-B
//! repoint minimal: only that file's private transport helpers change to call
//! these routes, and the public store API every one of the nineteen stores
//! depends on is untouched. See the design record → "od-store design
//! notes" for the exact repoint recipe.
//!
//! Field names are camelCase because the client is JSON from TypeScript. Bodies
//! can be megabytes (Cobalt's supervision ledger is 4.4 MB), so the body limit
//! is explicit and large, the same lesson `legacy_sql` and write-db already paid
//! for.

use super::route_error::RouteError;
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{DefaultBodyLimit, Extension, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chiefd_core::actor::{CompanyDb, MutationClass, MutationName, PublishBarrier};
use chiefd_core::runtime::attendance::ActuatorAttendance;
use chiefd_core::store::{organization, supervision};
use futures_util::future::BoxFuture;
use futures_util::stream::{self, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch as shutdown_watch};

use super::feed::{ChangeFeed, Replay, WatchEvent};
use super::store::{DocStore, StoreError};
use super::BenchCompletionRegistry;

/// How often `/v1/docs/watch` sends a `:hb` comment line during quiet state
/// (plan the design record §B). Tests that need to observe a
/// heartbeat use [`router_with_heartbeat_interval`] with a short interval
/// instead of waiting out the real 15s — see `tests/watch_http_surface.rs`.
pub(crate) const WATCH_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
/// How long `/v1/org/person/bench-lifecycle` holds its answer waiting for
/// chiefd's own convergence to acknowledge that the benched person's pane
/// really stopped.
///
/// # This bound belongs to the CLIENT, not to the reconciler
///
/// The route's contract on expiry is a deliberate, documented one: HTTP 503
/// `bench-convergence-timeout`, whose detail begins "bench committed" and which
/// `org_bench` answers by re-reading the roster and reporting the bench as the
/// success it is (the #141 shape — a manager told a bench failed that in fact
/// succeeded then retries, and is answered `already-benched`).
///
/// That contract is worth nothing if no caller is still listening when it
/// arrives. `@chief/chiefing`'s `FetchTransport` aborts at
/// `DEFAULT_TIMEOUT_MS` — then 10s — and the org intercom builds its transport
/// with no override, so a 30s wait here meant the CLIENT always gave up first —
/// 20 seconds early, every time. The abort raises `ChiefdUnavailableError` with
/// `kind: 'timeout'` and NO `status`, so the tool's own
/// `error.status === 503` recovery could never match: a committed bench was
/// reported to the manager as an outage, and the branch written to prevent
/// exactly that was dead code from the day it shipped. Found by
/// `OrganizationToolContract`'s staffing family, which drives `org_bench`
/// through its registered `execute` against a live daemon.
///
/// So this must stay comfortably INSIDE the client's patience, and
/// `scripts/test/client-observable-wait.test.mjs` derives both ends and fails
/// when they cross. The margin covers connect, the JSON round trip and the
/// reconcile wake. When the wait expires the manager is told the truth — the
/// bench committed and its teardown is still converging — which is strictly
/// better than the false failure a client-side abort produces.
///
/// The client's patience is now well past this bound (it has to be: the writer
/// actor can queue a mutation for `MUTATION_QUEUE_DEADLINE`), and that is not a
/// reason to grow this one. A ceiling is not a target. This route has an honest
/// answer available at 6s, so it owes the caller that answer at 6s.
const BENCH_COMPLETION_TIMEOUT: Duration = Duration::from_secs(6);

// --- graceful shutdown (E7-S3) ----------------------------------------------
//
// `POST /v1/admin/shutdown` asks the daemon to drain and exit through the
// shutdown path it already has (`Daemon.fatal_shutdown` / docstore-only's own
// channel) — a launcher never needs a pid to stop a daemon again (D7: who a
// daemon IS/where it lives stays beacond's question, not this route's; the
// response carries no pid). NOT part of the route table [`router`] builds:
// mounted conditionally by `docstore::mod`, next to the `RuntimeIdentity`
// mount, exactly when a sender was supplied — same optionality, same reason
// (a generic library caller with no daemon shutdown watch gets no route).

/// Carries the sender half of the daemon's shutdown watch so the handler can
/// flip it without reaching into daemon-specific state. `pub` because
/// `chiefd`'s `docstore_only`/`run` modules construct it when mounting the
/// route; `Clone` because axum's `Extension` layer clones per request.
#[derive(Clone)]
pub struct ShutdownRequest(pub tokio::sync::watch::Sender<Option<String>>);

#[derive(serde::Deserialize)]
pub(crate) struct AdminShutdownRequest {
    reason: String,
}

#[derive(serde::Serialize)]
pub(crate) struct AdminShutdownResponse {
    accepted: bool,
}

/// `POST /v1/admin/shutdown` handler. Never blocks on the drain: it flips the
/// watch and returns immediately — the caller observes exit by the socket
/// closing / the beacond registration going stale, not by this response. A
/// second call is harmless: `send_replace` on an already-`Some` sender simply
/// replaces the recorded reason; the daemon is already draining.
pub(crate) async fn admin_shutdown(
    Extension(ShutdownRequest(sender)): Extension<ShutdownRequest>,
    Json(req): Json<AdminShutdownRequest>,
) -> (StatusCode, Json<AdminShutdownResponse>) {
    tracing::info!(reason = %req.reason, "chiefd: shutdown requested over HTTP (/v1/admin/shutdown)");
    sender.send_replace(Some(req.reason));
    (StatusCode::ACCEPTED, Json(AdminShutdownResponse { accepted: true }))
}

/// #372: live source for the ONE special-cased read this router makes --
/// `store == "supervision"` for THIS chiefd process's own company reads
/// straight off the live `CompanyDb` instead of the (now-retired) mirrored
/// `org_documents` row. `None` everywhere except chiefd's own one-daemon
/// assembly (`chiefd run`'s `run_company`, via
/// [`super::bind_with_feed_and_company`]) -- the standalone/migration
/// entrypoints (`bind`/`bind_with_feed`/`serve`) have no `CompanyDb` at all
/// and are structurally unaffected by this.
///
/// `org_documents_slug` is the exact composite key
/// the company key (`host_primitives::rendezvous::company_key`) this process
/// computes for itself at boot -- a request naming any OTHER slug (a
/// foreign company) always falls through to the ordinary `org_documents`
/// path untouched, because this process has no live ledger for anyone but
/// its own company.
#[derive(Clone)]
pub struct SupervisionLiveSource {
    pub(super) company: Arc<CompanyDb>,
    pub(super) org_documents_slug: String,
    /// The daemon's reconcile trigger, when this router is mounted inside
    /// `chiefd run`'s one-daemon assembly. Nudged after a reminder is armed or
    /// stopped over HTTP -- see [`SupervisionLiveSource::with_reminder_trigger`]
    /// for why the route cannot simply rely on the mutation itself.
    pub(super) reminder_trigger: Option<Arc<tokio::sync::Notify>>,
    /// The daemon's reconcile trigger. A successful explicit launch-intent
    /// publish changes which people may receive panes, so the durable write
    /// must wake the live reconciler rather than wait for its fallback floor.
    pub(super) reconcile_trigger: Option<Arc<tokio::sync::Notify>>,
    pub(super) bench_completion: Option<Arc<BenchCompletionRegistry>>,
    /// The read-only API-host launch-profile source. Only `chiefd run` wires
    /// this from the same CompanyDb/configuration as its converge actuator;
    /// standalone and migration routers deliberately have no such authority.
    api_host_launch_profile: Option<Arc<chiefd_host::converge_apply::ApiHostLaunchProfileSource>>,
    /// #739 P2: the the live runtime executor `POST /v1/org/projection/reconcile`
    /// calls `reconcile_cycle` with. `None` everywhere except `chiefd run`
    /// -- same daemon-only-capability shape as every other field here.
    pub(super) host_executor: Option<Arc<dyn chiefd_host::HostExecutor>>,
    /// #739 P2: the `ActuatorConfig` the same route's `reconcile_cycle` call
    /// needs alongside [`Self::host_executor`], reconstructed by `chiefd
    /// run` the same way [`Self::api_host_launch_profile`]'s config is.
    pub(super) reconcile_actuator_config: Option<Arc<chiefd_host::converge_apply::ActuatorConfig>>,
    /// #751: the company's ONE runtime change signal, nudged by the daemon's
    /// change-feed sink on every `runtime` commit.
    ///
    /// The bounded waits in `runtime_lifecycle` (CEO pane liveness, session
    /// absence) park on it. It must be the daemon's own — a route that minted
    /// a fresh signal would park on something nothing nudges, and a wait that
    /// can only ever time out is worse than no wait at all. `None` everywhere
    /// except `chiefd run`, same daemon-only-capability shape as the two
    /// fields above.
    pub(super) runtime_change_signal:
        Option<Arc<chiefd_host::runtime_lifecycle::RuntimeChangeSignal>>,
    /// When an actuator last read this company's desired set — the one fact
    /// chiefd holds about whether ANYBODY is converging it.
    ///
    /// Not a daemon-only capability and deliberately not an `Option`: it is
    /// minted here, by `new`, so that the route which stamps it and the daemon
    /// which judges it cannot be wired to two different cells. `chiefd run`
    /// takes its own handle off [`Self::actuator_attendance`] rather than
    /// injecting one, which makes "forgot to wire it" unspellable.
    actuator_attendance: ActuatorAttendance,
    /// `<dir>` — the COMPANY DIRECTORY the operator ran `chief` in. Every agent
    /// home hangs off `<dir>/.chief/agent/<person_id>/`, and its identity key
    /// lives inside it. The Chief has no agent home and its key lives directly
    /// under `<dir>/.chief/`.
    ///
    /// It is the DIRECTORY and not the `.chief` root beneath it, so every
    /// reader derives the rest by joining rather than by walking up. The two
    /// used to be composed separately on either side of this field and landed
    /// one segment apart.
    ///
    /// BOTH daemon mounts wire it — the supervisor and `--serve-only` — because
    /// where the company lives is a fact, not a capability to actuate it. That
    /// is the whole reason it is not read off `reconcile_actuator_config`:
    /// `--serve-only` has none, and a person hired there is still a person
    /// whose home and credential must not wait for a convergence pass that
    /// mount will never run.
    pub(super) agent_home_root: Option<AgentHomeRoot>,
}

/// The directories needed for the one-time company filesystem writes.
#[derive(Debug, Clone)]
pub struct AgentHomeRoot {
    /// `<dir>` — the company directory the operator ran `chief` in.
    pub dir: std::path::PathBuf,
    /// `packages/piing/skills` in the pinned launcher checkout. Genesis copies
    /// its company skills to `<dir>/.pi/skills` only when that root is absent.
    pub shipped_skills_root: std::path::PathBuf,
}

/// One company clock reading, in epoch millis.
fn clock_now(company: &CompanyDb) -> i64 {
    company.clock().wall().0
}

impl SupervisionLiveSource {
    /// Wire a live-supervision source for `company`, matched against reads
    /// naming exactly `org_documents_slug` (this process's own composite
    /// `host_primitives::rendezvous::company_key(dir)`).
    #[must_use]
    pub fn new(company: Arc<CompanyDb>, org_documents_slug: String) -> Self {
        Self {
            actuator_attendance: ActuatorAttendance::new(clock_now(&company)),
            company,
            org_documents_slug,
            reminder_trigger: None,
            reconcile_trigger: None,
            bench_completion: None,
            api_host_launch_profile: None,
            host_executor: None,
            reconcile_actuator_config: None,
            runtime_change_signal: None,
            agent_home_root: None,
        }
    }

    /// This company's own clock reading, in epoch millis.
    ///
    /// The HTTP surface has no clock of its own. Reading the COMPANY's is what
    /// keeps an attendance stamp on the same timeline as the duty that later
    /// judges it — and what lets a test drive the rule by advancing a
    /// `ManualClock` rather than by waiting, which `clippy.toml` forbids
    /// outright.
    #[must_use]
    pub(super) fn clock_now(&self) -> i64 {
        clock_now(&self.company)
    }

    /// The shared attendance cell this source's desired-set route stamps.
    ///
    /// `chiefd run` clones this handle for its supervision duty and its health
    /// gatherer. One cell, three holders, and no builder that could be left off.
    #[must_use]
    pub fn actuator_attendance(&self) -> &ActuatorAttendance {
        &self.actuator_attendance
    }

    /// Carry the company filesystem roots, so genesis can seed project skills
    /// and each person can receive a home and identity when they become
    /// durable.
    #[must_use]
    pub fn with_agent_home_root(mut self, root: AgentHomeRoot) -> Self {
        self.agent_home_root = Some(root);
        self
    }

    /// Carry the daemon's runtime change signal so the bounded lifecycle waits
    /// park on the same signal the converge cycle nudges (#751).
    #[must_use]
    pub fn with_runtime_change_signal(
        mut self,
        signal: Arc<chiefd_host::runtime_lifecycle::RuntimeChangeSignal>,
    ) -> Self {
        self.runtime_change_signal = Some(signal);
        self
    }

    /// Carry the daemon's reconcile trigger so an HTTP arm/stop wakes the
    /// `ReminderDispatch` duty immediately.
    ///
    /// This is load-bearing, not a nicety, and it is the second time this
    /// branch has had to prove the wake rather than assume it. `2fe0c331` put
    /// `ReminderDispatch` on the reactive fan-out, but that fan-out re-broadcasts
    /// from ONE signal -- the daemon's `reconcile_trigger`, nudged by the
    /// `ReconcileWaker` on a mailbox/fence event. A `CompanyDb::mutate` arriving
    /// over HTTP is a DIFFERENT caller and nudges nothing: without this the duty
    /// keeps sleeping on the alarm it computed BEFORE the reminder existed, so a
    /// reminder armed one minute out is not looked at until the five-minute
    /// `REACTIVE_FALLBACK_FLOOR` sleep expires. Durable and correct, but late --
    /// and "late" is the whole product for a reminder.
    ///
    /// `None` everywhere except `chiefd run` (standalone/migration entrypoints
    /// have no daemon to wake), in which case arming still commits durably and
    /// the duty picks it up on its next pass.
    #[must_use]
    pub fn with_reminder_trigger(mut self, trigger: Arc<tokio::sync::Notify>) -> Self {
        self.reminder_trigger = Some(trigger);
        self
    }

    /// Carry the live reconciler wake used after a launch-intent publication.
    /// This is intentionally narrower than waking on every normalized row:
    /// launch intent is the explicit authority that changes desired panes.
    #[must_use]
    pub fn with_reconcile_trigger(mut self, trigger: Arc<tokio::sync::Notify>) -> Self {
        self.reconcile_trigger = Some(trigger);
        self
    }

    #[must_use]
    /// Carry the live daemon's post-commit bench acknowledgement registry.
    pub fn with_bench_completion(mut self, completion: Arc<BenchCompletionRegistry>) -> Self {
        self.bench_completion = Some(completion);
        self
    }

    /// Install the live, read-only API-host profile source. The source is
    /// intentionally injected rather than reconstructed in the route so the
    /// route cannot discover paths, resolve a daemon URL, or create a second
    /// CompanyDb authority.
    #[must_use]
    pub fn with_api_host_launch_profile(
        mut self,
        source: chiefd_host::converge_apply::ApiHostLaunchProfileSource,
    ) -> Self {
        self.api_host_launch_profile = Some(Arc::new(source));
        self
    }

    /// Carry the the live runtime executor `POST /v1/org/projection/reconcile`
    /// calls `reconcile_cycle` with. Injected, not constructed by the route,
    /// for the same reason [`Self::with_api_host_launch_profile`] is: the
    /// route must not be able to build a second, independent executor.
    #[must_use]
    pub fn with_host_executor(mut self, host: Arc<dyn chiefd_host::HostExecutor>) -> Self {
        self.host_executor = Some(host);
        self
    }

    /// Carry the `ActuatorConfig` `POST /v1/org/projection/reconcile` needs
    /// alongside [`Self::with_host_executor`].
    #[must_use]
    pub fn with_reconcile_actuator_config(
        mut self,
        config: chiefd_host::converge_apply::ActuatorConfig,
    ) -> Self {
        self.reconcile_actuator_config = Some(Arc::new(config));
        self
    }
}

/// Build the typed router over an already-open store, with the production
/// `/v1/docs/watch` heartbeat cadence. No live-supervision source -- every
/// `store == "supervision"` read serves `org_documents` exactly as every
/// other store does. This is the standalone/migration entrypoint's router;
/// `chiefd run`'s one-daemon assembly goes through
/// [`router_with_supervision_live`] instead (via `serve_bound`).
pub fn router(store: Arc<DocStore>, max_body_bytes: usize) -> Router {
    router_with_heartbeat_interval(store, max_body_bytes, WATCH_HEARTBEAT_INTERVAL)
}

/// [`router`], but with an overridable `/v1/docs/watch` heartbeat interval —
/// exists so tests can observe a heartbeat without a real 15s wait. The
/// production entry point ([`router`]) always uses
/// [`WATCH_HEARTBEAT_INTERVAL`]; this is `pub` (not `pub(crate)`) only so the
/// `tests/` integration crate, which compiles against this crate's public API
/// like any other client, can reach it.
pub fn router_with_heartbeat_interval(
    store: Arc<DocStore>,
    max_body_bytes: usize,
    watch_heartbeat_interval: Duration,
) -> Router {
    router_with_supervision_live(store, max_body_bytes, watch_heartbeat_interval, None)
}

/// [`router_with_heartbeat_interval`], additionally wired to a
/// [`SupervisionLiveSource`] -- the ONE production caller that needs this is
/// `chiefd run`'s one-daemon assembly (`serve_bound`, via
/// [`super::bind_with_feed_and_company`]). `supervision_live: None` behaves
/// byte-for-byte like [`router_with_heartbeat_interval`] (every route, the
/// `read` handler included, is unaffected — see `read`'s own doc comment).
/// Per-request resolver from a request `slug` (the composite org_documents key)
/// to a live source. `chiefd run` needs only its single company; the
/// multi-company `docstore-only` test surface passes a lazy registry so the
/// bun-test harness can exercise /v1/org routes for many companies from one
/// process (org-data-normalization P0, N8).
/// Why a per-request live company is being resolved. Ordinary routes never
/// open an absent company; genesis may open a new slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveResolutionMode {
    /// Normal reads and mutations require a live normalized manifest.
    ExistingOnly,
    /// Manifest genesis may open an absent company or swap a removal slot.
    Genesis,
}

/// Resolve the exact composite slug according to the route's explicit
/// admission mode.
pub type SupervisionLiveResolver =
    Arc<dyn Fn(&str, LiveResolutionMode) -> Option<SupervisionLiveSource> + Send + Sync>;

/// [`router_with_live_resolver`] with no per-request resolver — the production
/// entry, byte-for-byte the prior behaviour.
pub fn router_with_supervision_live(
    store: Arc<DocStore>,
    max_body_bytes: usize,
    watch_heartbeat_interval: Duration,
    supervision_live: Option<SupervisionLiveSource>,
) -> Router {
    // No auth, no resolver: the standalone/migration/test surface. `chiefd run`
    // uses [`router_with_live_resolver`] instead (via `serve_bound`).
    router_with_live_resolver(
        store,
        max_body_bytes,
        watch_heartbeat_interval,
        supervision_live,
        None,
        None,
    )
}

/// [`router_with_supervision_live`], plus the agent-auth verify-middleware and
/// `/v1/auth/*` handlers (agent-auth P0), with no per-request resolver. Kept as
/// a named public entry; delegates to [`router_with_live_resolver`]. `None`
/// auth behaves exactly like [`router_with_supervision_live`] (middleware passes
/// through, auth endpoints answer 501).
pub fn router_with_auth(
    store: Arc<DocStore>,
    max_body_bytes: usize,
    watch_heartbeat_interval: Duration,
    supervision_live: Option<SupervisionLiveSource>,
    auth: Option<Arc<crate::authn::runtime::AuthRuntime>>,
) -> Router {
    router_with_live_resolver(
        store,
        max_body_bytes,
        watch_heartbeat_interval,
        supervision_live,
        None,
        auth,
    )
}

/// [`router_with_supervision_live`], plus an optional per-request `resolver`
/// (the multi-company docstore-only test surface, org-data-normalization P0 N8)
/// AND the agent-auth verify-middleware + `/v1/auth/*` handlers (agent-auth P0).
/// The full builder every other entry delegates to. When `resolver` is present,
/// a body-peek middleware reads each request's `slug` and inserts the resolved
/// `Option<SupervisionLiveSource>` so the ordinary handlers serve /v1/org routes
/// per-company; it is INNER to the static `Extension(supervision_live)` so its
/// per-request value wins. The auth gate is the OUTERMOST layer, and it
/// enforces whenever a runtime is present — there is no longer a second
/// argument that could say otherwise (A6).
pub fn router_with_live_resolver(
    store: Arc<DocStore>,
    max_body_bytes: usize,
    watch_heartbeat_interval: Duration,
    supervision_live: Option<SupervisionLiveSource>,
    resolver: Option<SupervisionLiveResolver>,
    auth: Option<Arc<crate::authn::runtime::AuthRuntime>>,
) -> Router {
    router_with_live_resolver_and_shutdown(
        store,
        max_body_bytes,
        watch_heartbeat_interval,
        supervision_live,
        resolver,
        auth,
        None,
    )
}

/// [`router_with_live_resolver`], with an optional daemon shutdown watch for
/// watcher EOF. This is used only by the daemon mount: the public standalone
/// builders deliberately leave it unwired so they preserve their caller-owned
/// lifetime. Generic document reads and writes are retired; the lone
/// `/v1/docs/watch` route remains as the normalized changefeed compatibility
/// surface, which is why the mount still has a long-lived connection to close.
#[allow(clippy::too_many_arguments)]
pub fn router_with_live_resolver_and_shutdown(
    store: Arc<DocStore>,
    max_body_bytes: usize,
    watch_heartbeat_interval: Duration,
    supervision_live: Option<SupervisionLiveSource>,
    resolver: Option<SupervisionLiveResolver>,
    auth: Option<Arc<crate::authn::runtime::AuthRuntime>>,
    shutdown: Option<shutdown_watch::Receiver<bool>>,
) -> Router {
    apply_gate(
        ungated_routes(store, max_body_bytes, watch_heartbeat_interval, resolver, shutdown),
        supervision_live,
        auth,
        max_body_bytes,
    )
}

/// Every route this surface serves, with the per-request live-source resolver
/// wired, and NO outer layers — specifically not the verify-middleware.
///
/// Split out of [`router_with_live_resolver_and_shutdown`] for one reason, and
/// it is a defect rather than a taste: `Router::layer` wraps only the routes
/// registered BEFORE the call, so a route added to a router that already
/// carries the auth layer is OUTSIDE the gate. `serve_bound` adds three
/// (`/v1/docs/runtime`, `/v1/admin/shutdown`, `/v1/docs/queue`) and they were
/// exactly that — an unauthenticated remote shutdown of a company daemon among
/// them. Now every caller adds its routes HERE and calls [`apply_gate`] last,
/// so the gate cannot be outrun by registration order.
pub(crate) fn ungated_routes(
    store: Arc<DocStore>,
    max_body_bytes: usize,
    watch_heartbeat_interval: Duration,
    resolver: Option<SupervisionLiveResolver>,
    shutdown: Option<shutdown_watch::Receiver<bool>>,
) -> Router {
    let router = Router::new()
        .route("/v1/docs/health", get(health))
        .route("/v1/docs/ensure-schema", post(ensure_schema))
        .route(
            "/v1/docs/watch",
            get(
                move |State(store): State<Arc<DocStore>>,
                      Query(query): Query<WatchQuery>,
                      crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
                      headers: HeaderMap| {
                    watch(store, query, caller, headers, watch_heartbeat_interval, shutdown.clone())
                },
            ),
        )
        .route("/v1/reminders/arm", post(reminders_arm))
        .route("/v1/reminders/list", post(reminders_list))
        .route("/v1/reminders/stop", post(reminders_stop))
        // org-data-normalization P0, N2: the manifest on normalized rows. The
        // reference route shape N3-N7 copy — gate on own-company, dispatch into
        // CompanyDb (the rows live in chief.db), let chiefd-core own the txn.
        .route("/v1/org/manifest/read", post(org_manifest_read))
        .route("/v1/org/manifest/genesis", post(org_manifest_genesis))
        .route("/v1/org/person-contracts/read", post(org_person_contracts_read))
        .route(
            "/v1/org/person-contracts/projection-plan",
            post(org_person_contracts_projection_plan),
        )
        .route("/v1/org/api-host-launch-profile/read", post(org_api_host_launch_profile_read))
        .route("/v1/org/settings/read", post(org_settings_read))
        .route("/v1/org/settings/publish", post(org_settings_publish))
        .route("/v1/org/activity/read", post(org_activity_read))
        .route("/v1/org/projection/reconcile", post(org_projection_reconcile))
        // org-data-normalization P0, N5: the session-maintenance ledger on
        // normalized rows — same reference shape as the manifest pair.
        .route("/v1/org/session-maintenance/read", post(session_maintenance_read))
        .route("/v1/org/session-epoch/read", post(org_session_epoch_read))
        // The supervision & session-lifecycle verbs (the legacy-TypeScript
        // port). Each one is a decision chiefd makes, not a document a client
        // reads, edits and publishes back.
        .route("/v1/org/session-maintenance/ledger", post(org_session_maintenance_ledger))
        .route("/v1/org/session-maintenance/queue", post(org_session_maintenance_queue))
        .route("/v1/org/session-maintenance/start", post(org_session_maintenance_start))
        .route("/v1/org/session-maintenance/defer", post(org_session_maintenance_defer))
        .route("/v1/org/session-maintenance/interrupt", post(org_session_maintenance_interrupt))
        .route("/v1/org/session-maintenance/recover", post(org_session_maintenance_recover))
        .route("/v1/org/session-maintenance/finish", post(org_session_maintenance_finish))
        .route(
            "/v1/org/session-maintenance/reconcile-parked",
            post(org_session_maintenance_reconcile_parked),
        )
        .route("/v1/org/operator-escalation-intents/drain", post(org_operator_escalation_drain))
        .route("/v1/org/operator-escalation-log/read", post(org_operator_escalation_log))
        .route("/v1/org/operator-escalation-push/plan", post(org_operator_escalation_doorbell_plan))
        .route(
            "/v1/org/operator-escalation-push/settle",
            post(org_operator_escalation_doorbell_settle),
        )
        .route("/v1/org/session-epoch/stamp", post(org_session_epoch_stamp))
        .route("/v1/org/session-epoch/ms", post(org_session_epoch_ms))
        .route("/v1/org/operator-escalation-push/read", post(org_operator_escalation_push_read))
        .route("/v1/org/runtime-owner/read", post(org_runtime_owner_read))
        .route("/v1/org/launch-intent/read", post(org_launch_intent_read))
        .route("/v1/org/launch-intent/clear", post(org_launch_intent_clear))
        .route("/v1/org/stand-down", post(org_stand_down_set))
        .route("/v1/org/stand-down/clear", post(org_stand_down_clear))
        .route("/v1/org/stand-down/read", post(org_stand_down_read))
        .route("/v1/org/goal-delivery-quiesce/read", post(org_goal_delivery_quiesce_read))
        // TOMBSTONE (chief-home-is-cwd §4c): `/v1/org/runtime/prepare-ceo-only`
        // was registered here — the operator client's "I have arrived, ask for
        // the root" call on the daemon-side CEO boot path. The daemon boots no
        // pane, so the whole boot verb family is deleted. The STORE op it
        // called, `org_ops::prepare_ceo_only`, survives untouched: genesis
        // still makes exactly that call to record a new company's start
        // decision (see `org_manifest_genesis`).
        .route("/v1/org/event-journal/read", post(org_event_journal_read))
        .route("/v1/org/event-journal/insert-if-absent", post(org_event_journal_insert))
        .route("/v1/org/event-journal/prune", post(org_event_journal_prune))
        .route("/v1/org/mutation-journal/read", post(org_mutation_journal_read))
        .route("/v1/org/health-monitor/read", post(org_health_monitor_read))
        .route("/v1/org/runtime/read", post(org_runtime_read))
        .route("/v1/org/runtime/publish", post(org_runtime_publish))
        .route("/v1/org/runtime/clear", post(org_runtime_clear))
        // TOMBSTONE (chief-home-is-cwd §4c): `/v1/org/ceo-boot-lease/read` was
        // registered here. Its row store is deleted with the daemon-side CEO
        // boot that was the lease's only writer.
        .route("/v1/org/converge-safety/read", post(org_converge_safety_read))
        // The operator control-plane write `chiefd set-actuation-config` makes.
        // The raw whole-document publish that used to sit beside it is deleted
        // (it had no caller); this one stays because it is the only write that
        // can express "change the mode and leave everything else alone"
        // without the caller reimplementing the merge and the breaker reset.
        // See the handler.
        .route(
            "/v1/org/converge-safety/set-actuation-config",
            post(org_converge_safety_set_actuation_config),
        )
        // #954/#950: additive CAS sibling, no TS caller yet.
        // #954/#950: additive CAS sibling, no TS caller yet.
        .route(
            "/v1/org/operator-escalation-intents/read",
            post(org_operator_escalation_intents_read),
        )
        .route(
            "/v1/org/operator-escalation-intents/insert",
            post(org_operator_escalation_intents_insert),
        )
        // org-data-normalization P0, N3: the supervision ledger on normalized
        // rows. Only the READ survives — the publish/publish-cas/clear siblings
        // are deleted, because the TS `RowSupervisionRepository`
        // (org-durable-store.ts) that once posted them no longer exists and
        // nothing replaced it as a caller.
        .route("/v1/org/supervision/read", post(org_supervision_read))
        // #751/P4: the goals/assignments family. These eight replace
        // `org assignment <verb>`, the subprocess the Pi extension spawned to
        // reach the daemon it was already connected to.
        // org-data-normalization P0, N-mailbox: the mailbox on columnarized rows.
        .route("/v1/org/mailbox/read", post(org_mailbox_read))
        .route("/v1/org/mailbox/read-person", post(org_mailbox_read_person))
        .route("/v1/org/mailbox/delta", post(org_mailbox_delta))
        .route("/v1/org/mailbox/list-persons", post(org_mailbox_list_persons))
        // org_ops atomic family, member 1: shutdown_person. One BEGIN IMMEDIATE;
        // policy refusal maps to 422 and no caller retry fence exists.
        .route("/v1/org/person/shutdown", post(org_person_shutdown))
        .route("/v1/org/person/start", post(org_person_start))
        // The rail's click-to-wake. Narrower than `start`: it never recalls,
        // never rehires, and never pre-sets desired-active — see
        // `org_ops::wake_person`.
        .route("/v1/org/person/wake", post(org_person_wake))
        // org_ops atomic family, member 2: appoint_department_head (H2).
        .route("/v1/org/person/appoint-head", post(org_person_appoint_head))
        // org_ops atomic family, member P1-a: create_department (starts nobody).
        .route("/v1/org/department/create", post(org_department_create))
        // org_ops atomic family, member P1-d: reparent_department.
        .route("/v1/org/department/reparent", post(org_department_reparent))
        // org_ops atomic family, H1 (P1-c): transfer_person + move_department_members.
        .route("/v1/org/person/transfer", post(org_person_transfer))
        // org_ops atomic family, member 3: offboard_person (P2, fire).
        .route("/v1/org/person/offboard", post(org_person_offboard))
        .route("/v1/org/person/hire", post(org_person_hire))
        .route("/v1/org/department/pause", post(org_department_pause))
        .route("/v1/org/department/resume", post(org_department_resume))
        .route("/v1/org/department/resume-many", post(org_department_resume_many))
        .route("/v1/org/person/bench", post(org_person_bench))
        .route("/v1/org/person/bench-lifecycle", post(org_person_bench_lifecycle))
        .route("/v1/org/department/move-members", post(org_department_move_members))
        .route("/v1/org/person/recall", post(org_person_recall))
        .route(
            "/v1/org/person/replace-head-and-offboard",
            post(org_person_replace_head_and_offboard),
        )
        .route(
            "/v1/org/department/reactivate-executive-root",
            post(org_department_reactivate_executive_root),
        )
        .route("/v1/org/department/remove-tree", post(org_department_remove_tree))
        // --- activity/staffing/units/people port (the apps/cli/src/legacy/
        // organization/ slice). Handlers live in `docstore::org_slice`; every
        // mutating one wakes reconcile inside its own handler rather than here,
        // because the convergence they need is chiefd's, never the client's.
        .route(
            "/v1/org/lifecycle-status/read",
            post(crate::docstore::org_slice::org_lifecycle_status_read),
        )
        .route("/v1/org/tree/read", post(crate::docstore::org_slice::org_tree_read))
        .route("/v1/org/tree/structured", post(crate::docstore::org_slice::org_tree_structured))
        // #751/P4: the client-agnostic roster facts. Who exists and who should
        // be running — no session, window, pane, socket or layout. A runtime
        // client derives its own placement from this; so does a browser.
        .route("/v1/org/roster/desired", post(crate::docstore::roster::org_roster_desired))
        // #751/P8: the actuation contract. `observed` commits an actuator's
        // report and answers with the plan computed against it; `actions` is the
        // read-only re-read for everybody else; `launch-catalog` is the other
        // half of a start — the actions say WHO, the catalog says WITH WHAT.
        // The catalog stays a route rather than a moved function because its
        // derivation is a fail-closed gate over the daemon's OWN data root that
        // also stages the person's provider credential; a client that
        // re-implemented it would be a second answer to "may this person
        // launch".
        .route("/v1/org/runtime/desired", post(crate::docstore::desired::org_runtime_desired))
        .route(
            "/v1/org/runtime/launch-catalog",
            post(crate::docstore::desired::org_runtime_launch_catalog),
        )
        .route("/v1/org/unit/subtree", post(crate::docstore::org_slice::org_unit_subtree))
        .route(
            "/v1/org/unit/removal-impact",
            post(crate::docstore::org_slice::org_unit_removal_impact),
        )
        .route(
            "/v1/org/unit/removal-preview",
            post(crate::docstore::org_slice::org_unit_removal_preview),
        )
        .route(
            "/v1/org/activity/command-status",
            post(crate::docstore::org_slice::org_activity_command_status),
        )
        .route(
            "/v1/org/activity/agent-state",
            post(crate::docstore::org_slice::org_activity_agent_state),
        )
        .route(
            "/v1/org/staffing/lifecycle",
            post(crate::docstore::org_slice::org_staffing_lifecycle),
        )
        .route(
            "/v1/org/control-authority/person-in-scope",
            post(crate::docstore::org_slice::org_control_authority_person_in_scope),
        )
        .route(
            "/v1/org/control-authority/department-in-scope",
            post(crate::docstore::org_slice::org_control_authority_department_in_scope),
        )
        .route(
            "/v1/org/person-contracts/build",
            post(crate::docstore::org_slice::org_person_contracts_build),
        )
        // agent-auth (P0): the two routes the verify-middleware exempts, because
        // they mint the token every other route needs.
        .route("/v1/auth/challenge", post(crate::authn::routes::challenge))
        .route("/v1/auth/token", post(crate::authn::routes::token))
        // Enrolment is GATED (not exempt): the operator enrols new keypairs, and
        // proves it is the operator with a bearer of its own. It is deliberately
        // NOT a fifth exempt path — the operator identity is enrolled from disk
        // at boot (`authn::boot`), so there is no chicken-and-egg to solve.
        .route("/v1/auth/enroll", post(crate::authn::routes::enroll));
    // The runtime / runtime-placement / materialization family (#751). It lives in
    // its own module and merges in one line, so three parallel ports were not
    // all editing this 7500-line route table at once. Merged BEFORE the layers
    // below, so it gets the same live-source resolution, auth and body limit as
    // every other `/v1/org/*` route.
    let router = super::runtime_routes::merge(router);
    // Per-request live-source resolver for the multi-company docstore-only test
    // surface (norm-n8): innermost layer, so its value overrides the static
    // Extension below. Absent (production) ⇒ the router is unchanged.
    let router = match resolver {
        Some(resolver) => router.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let resolver = resolver.clone();
                async move { resolve_live_source(&resolver, max_body_bytes, req, next).await }
            },
        )),
        None => router,
    };
    router.with_state(store)
}

/// Wrap a fully-registered route set in the verify-middleware and the surface's
/// outer layers. **Call this LAST**, after every route the surface serves is
/// registered: `Router::layer` wraps only what precedes it, so a route added
/// after this call answers with no identity check at all.
pub(crate) fn apply_gate(
    router: Router,
    supervision_live: Option<SupervisionLiveSource>,
    auth: Option<Arc<crate::authn::runtime::AuthRuntime>>,
    max_body_bytes: usize,
) -> Router {
    router
        // The verify-middleware — ONE layer over the whole router, OUTER to the
        // resolver. It runs whenever a runtime exists, and then a bearer is
        // REQUIRED on every path but the exempt four: a presented credential is
        // verified and its identity reaches the handlers (#751/P7 — the
        // person-fenced routes depend on that), and a request that presents none
        // is refused before any handler runs. No 127.0.0.1 bypass ever (R5), and
        // since A6 no deploy-stage switch either — there is no argument here a
        // caller could set to serve an anonymous request.
        .layer(axum::middleware::from_fn_with_state(
            auth.as_ref().map(|runtime| {
                crate::authn::middleware::AuthState::new(
                    runtime.secret(),
                    std::sync::Arc::clone(runtime)
                        as std::sync::Arc<dyn crate::authn::middleware::IdentityLookup>,
                )
            }),
            crate::authn::middleware::require_identity,
        ))
        // The auth handlers read the runtime through this extension (present
        // whenever a runtime exists).
        .layer(Extension(auth))
        .layer(Extension(supervision_live))
        .layer(DefaultBodyLimit::max(max_body_bytes))
        // OUTERMOST, so it measures the whole request including every layer
        // above and the body limit below. Genesis is one of these calls, and a
        // launch that stalls in it used to be indistinguishable from a launch
        // that stalled reaching the daemon at all.
        .layer(axum::middleware::from_fn(timed_request))
}

/// A request that took at least this long is news whatever it answered.
///
/// The outermost timing layer exists because "a launch that stalls in it used
/// to be indistinguishable from a launch that stalled reaching the daemon at
/// all", and that only survives the demotion below if a slow request keeps a
/// level an operator reads. One second is far above the ordinary changefeed
/// poll and far below any wait a human would call a hang.
const SLOW_REQUEST_MS: u64 = 1_000;

/// The final path segments this daemon POLLS itself with, and the ONLY
/// requests whose success is demoted out of the operator's log.
///
/// Measured on a live company's 6,260-line log: `activity/read` 928,
/// `docs/watch` 552, `lifecycle-status/read` 513, `runtime/desired` 511,
/// `roster/desired` 510, `runtime/launch-catalog` 506, `mailbox/read-person`
/// 219, `session-maintenance/read` 150, `supervision/read` 136,
/// `manifest/read` 127, `activity/agent-state` 43. That is 98% of every
/// request line, and every one of them is a rail or an actuator asking what
/// the state is, several times a second, for ever.
///
/// # Why a list of what is SILENT rather than a list of what is loud
///
/// A blanket demotion took `POST /v1/org/person/wake` with it — one operator
/// click, the request the live test suite counts to prove that click produced
/// exactly one wake — and every other MUTATION with it. A new route added
/// tomorrow would have been silent too, which is the wrong default for a log:
/// a request that changes something is news, and this daemon's state changes
/// are rare. So silence is the named exception and INFO is the default, and
/// anything not on this list stays where an operator reads it.
const POLLING_READ_SEGMENTS: [&str; 7] =
    ["read", "read-person", "desired", "watch", "launch-catalog", "agent-state", "health"];

/// Is this path one of the reads this daemon polls itself with?
fn is_polling_read(path: &str) -> bool {
    let Some(last) = path.rsplit('/').next() else { return false };
    POLLING_READ_SEGMENTS.contains(&last)
}

/// The level one served request is logged at.
///
/// # Why this is not INFO for everything any more
///
/// It was, and `daemon.log` reached **126 MB in five hours** on a live company:
/// 670k lines, of which 653k were `event="docstore.request"`. On a 79 GB box at
/// 60% full that is a real disk risk with no rotation anywhere, and it is a
/// worse READING problem than a disk one — the refusals, holds and withheld
/// reasons an operator opens this file for were one line in forty.
///
/// The volume is not a busy company; it is the changefeed. A quiet company
/// polls its own daemon several times a second, for ever, and each poll is a
/// 200 in a millisecond. "A routine request succeeded quickly" is the one line
/// here that carries no information, and it is 97.5% of the file.
///
/// # Why demotion and not rotation or sampling
///
/// **Rotation** bounds the disk and fixes nothing else: the signal stays at one
/// line in forty, and the rotation that keeps the file small is the rotation
/// that deletes the refusal an operator came back for. **Sampling** produces a
/// log where a given request may or may not be there, so an absent line proves
/// nothing — a rule an operator cannot state is not a rule they can read
/// against. Demotion deletes only the lines that say nothing and keeps every
/// line that says something, at a level that names how much it matters.
///
/// A DEBUG line is not gone: the same run at `--log-level debug` prints exactly
/// what this printed before, which is what a request-by-request investigation
/// actually wants.
///
/// # What stays visible
///
/// * `>= 500` — ERROR, as before: the daemon failed.
/// * `>= 400` — WARN. A refusal is the single most operator-relevant thing
///   this surface produces (`caller-unauthenticated`, a fenced route, a body
///   that does not parse), and it was previously indistinguishable from a
///   successful poll at INFO.
/// * slow — INFO, whatever the status. See [`SLOW_REQUEST_MS`].
/// * a MUTATION, or any route not in [`POLLING_READ_SEGMENTS`] — INFO. A wake,
///   a start, a stop, a hire: rare, and the whole reason an operator opens
///   this file.
/// * a fast, successful POLL — DEBUG. That is the 98%.
pub fn request_log_level(status: u16, elapsed_ms: u64, path: &str) -> tracing::Level {
    if status >= 500 {
        return tracing::Level::ERROR;
    }
    if status >= 400 {
        return tracing::Level::WARN;
    }
    if elapsed_ms >= SLOW_REQUEST_MS {
        return tracing::Level::INFO;
    }
    if is_polling_read(path) {
        return tracing::Level::DEBUG;
    }
    tracing::Level::INFO
}

/// Log every request this daemon serves: method, path, status and elapsed.
///
/// The PATH and never the query string or the body. A docstore path names a
/// route; the body carries a company spec, a person's prose and, on the auth
/// routes, a bearer token.
///
/// The LEVEL is [`request_log_level`], and the reason it is not one level is
/// written there.
async fn timed_request(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let started = std::time::Instant::now();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    let elapsed_ms = chiefd_log::elapsed_ms(started);
    // One event, four levels. `tracing`'s level must be a literal at the
    // callsite, so the arms are spelled out rather than computed into the
    // macro; the RULE lives in one `const fn` above so it can be read and
    // proven in one place.
    match request_log_level(status, elapsed_ms, &path) {
        tracing::Level::ERROR => tracing::error!(
            event = "docstore.request",
            method = %method,
            path = %path,
            status,
            elapsed_ms,
            "a docstore request failed"
        ),
        tracing::Level::WARN => tracing::warn!(
            event = "docstore.request",
            method = %method,
            path = %path,
            status,
            elapsed_ms,
            "a docstore request was refused"
        ),
        // TWO REASONS REACH INFO — it took a second, or it was not a poll —
        // and the line must say which. It said "was slow" for both, so a wake
        // that answered in 10 ms was reported as slow on the very first live
        // run of this rule.
        tracing::Level::INFO if elapsed_ms >= SLOW_REQUEST_MS => tracing::info!(
            event = "docstore.request",
            method = %method,
            path = %path,
            status,
            elapsed_ms,
            "a docstore request was slow"
        ),
        tracing::Level::INFO => tracing::info!(
            event = "docstore.request",
            method = %method,
            path = %path,
            status,
            elapsed_ms,
            "a docstore request was served"
        ),
        _ => tracing::debug!(
            event = "docstore.request",
            method = %method,
            path = %path,
            status,
            elapsed_ms,
            "a docstore request was served"
        ),
    }
    response
}

/// Body-peek middleware for the multi-company `docstore-only` surface: read the
/// request `slug`, resolve it (lazily opening that company's `CompanyDb`), and
/// insert the resolved `Option<SupervisionLiveSource>` so the ordinary handlers
/// serve /v1/org routes per-company. Runs INNER to the static `Extension`
/// layer, so its per-request value wins. Bodies without a `slug` (health, the
/// watch GET) resolve to `None` and behave exactly as the standalone surface.
async fn resolve_live_source(
    resolver: &SupervisionLiveResolver,
    max_body_bytes: usize,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    #[derive(Deserialize)]
    struct SlugPeek {
        slug: String,
    }
    // Only the dedicated /v1/org/* row routes consume a live source. The
    // /v1/docs/* blob routes have their OWN supervision_live fast-paths (manifest
    // /activity/supervision live reads/CAS) that are correct ONLY for a single
    // live company (production chiefd run); activating them on this multi-company
    // test surface would hijack blob createOrganization. Leave every non-/v1/org
    // request untouched (static Extension = None, byte-for-byte the standalone
    // surface).
    if !req.uri().path().starts_with("/v1/org/") {
        return next.run(req).await;
    }
    // GENESIS: only the manifest publish may create an absent company; every other
    // /v1/org/* route (and all reads) must find it already live — reads never create.
    let resolution_mode = match req.uri().path() {
        "/v1/org/manifest/genesis" => LiveResolutionMode::Genesis,
        _ => LiveResolutionMode::ExistingOnly,
    };
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, max_body_bytes).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return match classify_body_read_failure(&error) {
                BodyReadFailure::TooLarge => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
                BodyReadFailure::Unreadable(error) => error.into_response(),
            }
        }
    };
    let source = serde_json::from_slice::<SlugPeek>(&bytes)
        .ok()
        .and_then(|peek| resolver(&peek.slug, resolution_mode));
    let mut req = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
    req.extensions_mut().insert::<Option<SupervisionLiveSource>>(source);
    next.run(req).await
}

/// Why `axum::body::to_bytes` gave up — the two answers it conflates.
///
/// `to_bytes` wraps the body in `http_body_util::Limited`, which fails on TWO
/// unrelated things: the declared length limit, and any error the underlying
/// body stream surfaces — a caller that went away mid-upload, a chunked body
/// that stopped short, a connection reset. Answering `413` for both told a
/// caller whose connection dropped that its payload was too large, and that
/// does not merely read wrong: it PRESCRIBES A WRONG ACTION TO A MACHINE. An
/// agent that reads 413 shrinks a payload that was never oversized and sends
/// it again, and the second attempt fails for exactly the same reason as the
/// first.
#[derive(Debug)]
enum BodyReadFailure {
    /// The body really was longer than the limit. The caller must send less —
    /// `413`, with the bare status it has always had.
    TooLarge,
    /// The body could not be read to its end. The caller must send the request
    /// again, intact; the size was never the problem.
    Unreadable(RouteError),
}

/// Split a `to_bytes` failure into [`BodyReadFailure`]'s two cases.
///
/// The limit case is identified by walking the error's source chain for
/// `http_body_util::LengthLimitError` — the check axum's own `to_bytes`
/// documentation prescribes. Everything else is a body chiefd could not read,
/// and it goes out through `route_error` so the caller gets the `{code,
/// detail}` shape and the cause BY NAME instead of a bare status about a size
/// that was never wrong.
fn classify_body_read_failure(error: &axum::Error) -> BodyReadFailure {
    let mut chain: Vec<String> = Vec::new();
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = cause {
        if current.is::<http_body_util::LengthLimitError>() {
            return BodyReadFailure::TooLarge;
        }
        let rendered = current.to_string();
        // `axum::Error` renders its inner error verbatim, so the head of the
        // chain repeats the link below it. Say each cause once.
        if chain.last() != Some(&rendered) {
            chain.push(rendered);
        }
        cause = current.source();
    }
    BodyReadFailure::Unreadable(RouteError::malformed(
        "request-body-unreadable",
        format!("the request body could not be read: {}", chain.join(": ")),
    ))
}

/// Map a typed-store failure onto the taxonomy. A lost CAS or a stale lock is
/// NOT an error — those are `false`/`0` in the success body, exactly as the SQL
/// rows_affected was — so only genuine faults reach here.
///
/// Every arm is a FAULT and says so. The SQL text stays in `detail` because it
/// is the only diagnostic there is, but the `code` is stable and the status no
/// longer claims the caller wrote a bad request: a `rusqlite` error is chiefd's
/// problem, not the caller's, and 400 sent a client hunting its own payload.
fn store_error(error: &StoreError) -> RouteError {
    match error {
        StoreError::Write(failure) => write_failure_error(failure),
        StoreError::Query(message) => RouteError::fault("store-query-failed", message.clone()),
        StoreError::RowShape(message) => RouteError::fault("store-row-shape", message.clone()),
    }
}

/// The write half of [`store_error`].
fn write_failure_error(failure: &super::engine::WriteFailure) -> RouteError {
    match failure {
        // SQLite contention is neither the caller's fault nor a fault at all:
        // it is chiefd holding a lock, and the taxonomy already has the word
        // for that. A caller told its request was malformed has the only real
        // instruction — come back — left in prose it must pattern-match.
        super::engine::WriteFailure::Sql(message) if is_store_contention(message) => {
            RouteError::busy("store-contended", message.clone())
        }
        super::engine::WriteFailure::Sql(message) => {
            RouteError::fault("store-sql-failed", message.clone())
        }
        super::engine::WriteFailure::WriterDown => {
            RouteError::unavailable("writer-down", "the store writer is down")
        }
        super::engine::WriteFailure::WriterDropped => {
            RouteError::fault("writer-dropped", "the store writer was dropped mid-request")
        }
    }
}

/// SQLite saying "come back", in the two spellings it uses.
///
/// Matched on the message because that is what `rusqlite` hands the engine
/// through `WriteFailure::Sql(String)` — the typed code is already gone by the
/// time it reaches here. Narrow on purpose: anything else stays a fault, so a
/// real schema or constraint error can never be mistaken for weather.
fn is_store_contention(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("database is locked") || lowered.contains("database table is locked")
}

/// Map a `CompanyDb` failure onto the taxonomy (#440: the supervision write
/// path applies into `CompanyDb`, not the typed `DocStore`, so it fails with
/// `ChiefdError`, not `StoreError`).
///
/// This is now one line, and that is the point. It used to be its own status
/// table, and the table was wrong in the way that matters: `Refused` — the
/// variant that MEANS "a product rule declined" — answered **400 with a bare
/// text body**, so the refusal's machine code never left the process and three
/// sibling mappers (`company_error`, `company_error`,
/// `company_error`, all deleted with this change) existed only to
/// hand-list codes that deserved a 422 back. See `route_error.rs` for the one
/// table that replaced it.
/// A response body chiefd built and then could not serialize. Always a fault:
/// nothing the caller did produced it, and no retry can change it.
fn encode_fault(error: impl std::fmt::Display) -> RouteError {
    RouteError::fault("encode-failed", error.to_string())
}

pub(crate) fn company_error(error: &chiefd_core::error::ChiefdError) -> RouteError {
    RouteError::from_chiefd(error)
}

#[cfg(test)]
mod taxonomy_tests {
    use super::*;

    /// The whole point of the taxonomy, at the mapper: a product rule is a
    /// refusal carrying its code, and a fault is a fault. Before this,
    /// `company_error` answered EVERY refusal 400 with a bare text body, so the
    /// code — the only thing a caller can branch on — never left the process.
    #[test]
    fn a_refused_chiefd_error_keeps_its_code_and_becomes_a_refusal() {
        let error = company_error(&chiefd_core::error::ChiefdError::refused(
            "model-command-unknown-model",
            "thinking change names a model this provider does not report",
        ));
        assert_eq!(error.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.code(), "model-command-unknown-model");
        assert!(error.is_refusal());
    }

    #[test]
    fn a_corrupt_store_is_a_fault_and_is_never_read_as_a_rule() {
        let error = company_error(&chiefd_core::error::store_failure_because("activity", "locked"));
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!error.is_refusal());
    }

    /// SQLite contention is chiefd holding a lock, not the caller's request
    /// being wrong. It used to be a 400 — "your request is malformed" — with
    /// the only real instruction left in prose.
    #[test]
    fn sqlite_contention_is_busy_and_a_real_sql_error_is_a_fault() {
        let contended = write_failure_error(&super::super::engine::WriteFailure::Sql(
            "database is locked".to_owned(),
        ));
        assert_eq!(contended.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(contended.code(), "store-contended");
        assert!(!contended.is_refusal(), "come back later is not a rule to act on");

        let broken = write_failure_error(&super::super::engine::WriteFailure::Sql(
            "no such column: assignee_id".to_owned(),
        ));
        assert_eq!(broken.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(broken.code(), "store-sql-failed");
    }

    #[test]
    fn store_contention_detection_is_narrow_on_purpose() {
        assert!(is_store_contention("database is locked"));
        assert!(is_store_contention("Database Table Is Locked"));
        assert!(!is_store_contention("no such table: org_documents"));
        assert!(!is_store_contention("UNIQUE constraint failed"));
    }
}

/// Nudge the reconcile duty after a COMMITTED mutation of a row it can read
/// (arch-audit F1/F9, Step 6): the reactive wake is the DEFAULT for every
/// reconcile-input publish, not an opt-in — an unwired mutation converges only
/// on the ~30s SupervisionReconcile cadence. Call this only on the success
/// path, after the store reports the write applied; a refusal committed
/// nothing and must not wake. The enumeration test
/// `every_reconcile_input_publish_wakes` pins the wired/opted-out route sets.
pub(crate) fn wake_reconcile(source: &SupervisionLiveSource) {
    if let Some(trigger) = &source.reconcile_trigger {
        trigger.notify_one();
    }
}

// TOMBSTONE: `provision_person_identities`. It minted and enrolled every
// committed person's key on its own, one layer above the home they live in.
// Splitting the two was how the key could be written into a folder that
// `ensure_agent_home` would then decline to build, so the pair is now ONE
// ordered call — `runtime_lifecycle::ensure_agent_homes` — which writes the
// home first and provisions into it second.

// TOMBSTONE: `committed_people`. It read the roster out of a snapshot so the
// route could hand a list of ids to the identity provisioner. The host call
// reads the manifest itself now, because it needs each person's RECORD — their
// employment state and their derived contract — not just their id.

/// Give every committed person a home and an enrolled identity.
///
/// **A person row, a person home and a person identity are created together, or
/// the person is a ghost.** Hiring wrote SQL and woke the reconciler, and the
/// converge cycle is deliberately read-only about homes — it reads
/// `read_materialized_resources_for_launch` and never writes one — so nothing
/// between the hire and the pane ever created one. The person appeared in the
/// roster and in the tree, and the actuator refused them on every pass, for
/// ever.
///
/// The sequence itself is `runtime_lifecycle::ensure_agent_homes`, in the crate
/// that owns filesystem effects; this route-side wrapper only publishes the
/// contracts document first, because that is a durable projection the ROUTE
/// owns and the home writer does not.
///
/// Idempotent by construction: both halves of the host call are
/// create-if-absent, so the common path costs a stat per person and this may
/// run on every roster mutation. A surface with no actuator config (every
/// `chiefd` mode except `chiefd run`) has no directory to write into and skips
/// it.
///
/// Returns per-person warnings; one person whose home cannot be written is
/// reported, never allowed to fail the whole call.
async fn ensure_committed_agent_homes(
    source: &SupervisionLiveSource,
    at: String,
) -> Result<Vec<String>, RouteError> {
    let Some(root) = source.agent_home_root.clone() else {
        return Ok(Vec::new());
    };
    // Contracts first, because the contracts DOCUMENT is what the intercom and
    // the browser read. The home writer derives its own copy from the manifest
    // rather than reading this back — at hire time the person is not in the
    // stored document yet, so a read would refuse `person-contract-absent` for
    // exactly the person this call exists to serve.
    source.company.org_person_contracts_build(at).await.map_err(|error| company_error(&error))?;
    chiefd_host::runtime_lifecycle::ensure_agent_homes(
        &source.company,
        &root.dir,
        Some(root.shipped_skills_root.as_path()),
    )
    .await
    .map_err(|error| RouteError::refused("agent-home-failed", error.to_string()))
}

/// The company's committed manifest, as the projection consumes it.
fn current_manifest(
    source: &SupervisionLiveSource,
) -> Result<chiefd_core::store::organization::OrganizationManifest, RouteError> {
    let snapshot = source.company.snapshot();
    chiefd_core::store::organization::read(&snapshot)
        .map_err(|error| RouteError::refused("error", error.to_string()))
}

// TOMBSTONE: `stage_projected_company`.
//
// It built a PROPOSED manifest into `<dir>/.chief/.staging/<uuid>/` before the
// commit, so a hire naming an unresolvable skill refused with the person id
// still free, and promoted the stage through a `PublishBarrier` after the
// commit was durable. Both halves are gone with their subject: there is no
// resource to resolve, so a proposed roster cannot fail to build, and there is
// nothing to promote because a home is written after the commit rather than
// before it. The barrier at each call site is `PublishBarrier::none()`.

/// [`ensure_committed_agent_homes`] for a caller whose row is ALREADY
/// committed, where a failure must never be reported as a failed call.
///
/// Hire and department-create write first and materialize second, so by the
/// time this runs the person exists and the answer is `applied: true` whatever
/// happens next. Returning an error there would tell a caller its request
/// failed when it half-succeeded — the caller then retries and is told the
/// department already exists, which is exactly the confusing shape a live CEO
/// hit. The problem is reported in `warnings` instead, and the person's home
/// gets a second chance on their next start, which refuses honestly with the
/// real cause if it is still broken.
async fn materialize_after_commit(source: &SupervisionLiveSource, at: String) -> Vec<String> {
    match ensure_committed_agent_homes(source, at).await {
        Ok(warnings) => warnings,
        Err(error) => {
            let detail = error.detail();
            vec![format!(
                "the roster committed, but one or more agent homes were not written ({detail}); \
                 affected people will be refused at start with the specific reason"
            )]
        }
    }
}

/// Why the actuator would refuse to give this person a pane, or `None` when it
/// would not.
///
/// The launch gate's own re-derivation, asked BEFORE a durable write instead of
/// after it. `start_person` committed `active`, a launch fence and durable
/// demand, answered `{"applied": true}` — which the CEO's tool renders as
/// `✅ Started @<id> · only this person was launched` — and only then did the
/// actuator discover it could not spawn. The roster showed four people
/// `active · recovering · no live pane observed` that never converged, and the
/// only place the real cause appeared was a chiefd log line nobody was reading.
///
/// A start the actuator will refuse must not answer success.
fn launch_refusal_for(source: &SupervisionLiveSource, person_id: &str) -> Option<String> {
    let actuator_config = source.reconcile_actuator_config.as_ref()?;
    let snapshot = source.company.snapshot();
    let manifest = chiefd_core::store::organization::read(&snapshot).ok()?;
    // A person the manifest does not know has no launch to refuse. The record
    // itself is no longer read: the only check that needed one was the
    // generated theme trio, keyed on the person's id, and that requirement is
    // deleted with the themes.
    manifest.people.get(person_id)?;
    // Derived by the module that WRITES the home, never composed here. While
    // this composed its own path it named a directory nothing writes, so the
    // explainer read every person's home as missing and answered a fabricated
    // refusal for a person the actuator would have launched.
    let home = chiefd_host::agent_home::agent_home(&actuator_config.dir, person_id);
    chiefd_host::converge_apply::resource_catalog::explain_launch_refusal(
        &home,
        &actuator_config.root_pi_agent_dir,
    )
}

/// Return the current normalized row fence when it matches the caller's cached
/// seq. This is the typed-row equivalent of the retired blob conditional read:
/// one cheap fence query, no aggregate reconstruction and no serialization.
async fn unchanged_org_seq(
    source: &SupervisionLiveSource,
    if_seq_not: Option<i64>,
) -> Result<Option<i64>, RouteError> {
    let Some(expected) = if_seq_not else {
        return Ok(None);
    };
    let current = source.company.org_current_seq().await.map_err(|e| company_error(&e))?;
    Ok((current == expected).then_some(current))
}

// --- org manifest rows (org-data-normalization P0, N2) --------------------
//
// The REFERENCE route pair N3-N7 copy for `/v1/org/<store>/*`. The normalized
// tables live in `chief.db`, so these dispatch into the live company's
// `CompanyDb` (never the typed `DocStore`), gate on own-company exactly like
// `cas`, and let chiefd-core own the single `BEGIN IMMEDIATE`. Fence is the
// `org_events` seq. Item D: a manifest with unmodeled keys is a 422
// `unmodeled-keys`, never a silent drop. Item A (activation): DROPPED — phantom
// field, not stored, not round-tripped.

#[derive(serde::Deserialize)]
struct OrgManifestReadRequest {
    /// The own-company documentKey; a foreign slug is served `found:false`.
    slug: String,
}

#[derive(serde::Serialize)]
struct OrgManifestReadResponse {
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<String>,
    seq: i64,
}

async fn org_manifest_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgManifestReadRequest>,
) -> Result<Json<OrgManifestReadResponse>, RouteError> {
    // Isolation: this process serves only its own company. A foreign/absent slug
    // gets `found:false` (there is no cross-company row path), mirroring `cas`'s
    // fall-through rather than erroring.
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Ok(Json(OrgManifestReadResponse { found: false, manifest: None, seq: 0 }));
    };
    match source.company.org_manifest_read().await.map_err(|e| company_error(&e))? {
        Some((manifest, seq)) => {
            let body = serde_json::to_string(&manifest).map_err(encode_fault)?;
            Ok(Json(OrgManifestReadResponse { found: true, manifest: Some(body), seq }))
        }
        None => Ok(Json(OrgManifestReadResponse { found: false, manifest: None, seq: 0 })),
    }
}

/// First-write-only organization creation with the exact refreshed Founder
/// provider/model snapshot. The bootstrap is required: this route has no
/// optional, static, or compatibility default path.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgManifestGenesisRequest {
    slug: String,
    /// The company SPEC — name, purpose, CEO, department tree.
    ///
    /// This used to be a pre-normalized `OrganizationManifest` string, which
    /// meant the launcher decided every id, default tool grant, employment
    /// state and unit relationship and chiefd merely stored the answer: the
    /// single most consequential decision in the product, what a company IS at
    /// birth, made outside chiefd. #751 moved normalization into
    /// `chiefd_core::store::organization_spec`, so the wire now carries the
    /// question rather than the answer.
    spec: serde_json::Value,
    /// ISO-8601 event stamp for the seeded documents (the caller's clock).
    at: String,
}

async fn org_manifest_genesis(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgManifestGenesisRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    // The two genesis artifacts are DERIVED here, from the spec, rather than
    // accepted from the caller. A caller that could hand chiefd a
    // manifest disagreeing with its own contracts could seed a company that
    // never validated as a whole.
    let manifest =
        chiefd_core::store::organization_spec::normalize_organization_spec(&req.spec, &req.at)
            .map_err(|refusal| RouteError::refused(refusal.code, refusal.message.clone()))?;
    let person_contracts =
        chiefd_core::store::person_contracts::build::build_organization_person_contracts(&manifest)
            .map_err(|refusal| RouteError::refused(refusal.code, refusal.message.clone()))?;
    // Kept for the CEO's start decision and the home write below: `req.at`
    // moves into the actor call.
    let genesis_at = req.at.clone();
    match source.company.org_manifest_genesis(manifest, req.at, person_contracts).await {
        Ok(chiefd_core::store::organization_rows::ManifestGenesisOutcome::Created) => {
            // The company skill library, installed at genesis so a brand-new
            // company's first people have something to link at before any
            // launch pass runs. It is RECONCILED, not seeded once: the launch
            // path calls the same function on every pass, which is how an
            // existing company receives a skill change.
            if let Some(root) = &source.agent_home_root {
                if let Err(error) = chiefd_host::project_skills::reconcile_project_skills(
                    &root.dir,
                    &root.shipped_skills_root,
                ) {
                    tracing::warn!(
                        company = %source.org_documents_slug,
                        %error,
                        "genesis: the project skill root was not reconciled"
                    );
                }
            }
            // The people now exist, so their HOMES and their identities do
            // too. This is the moment both belong to — NOT a later convergence
            // pass, which is what made a company answer 401 for its own CEO
            // between genesis and its first reconcile, and what would leave the
            // seeded staff of a brand-new company with no home at all.
            //
            // A genesis warning is reported the way a hire's is: the rows are
            // committed and correct either way, and a person whose home is
            // missing is refused BY NAME at their first start rather than
            // launched half-built.
            for warning in materialize_after_commit(&source, genesis_at.clone()).await {
                tracing::warn!(
                    company = %source.org_documents_slug,
                    %warning,
                    "genesis: an agent home was not written"
                );
            }
            // CREATING A COMPANY IS THE DECISION TO RUN ITS CEO, and this is
            // where that decision becomes durable.
            //
            // # The defect this closes
            //
            // A company created through `chiefd_launch_company` came up with
            // NOBODY running: the actuator reported `requested=0 applied=0`
            // round after round and no tmux session ever appeared, while the
            // Founder's card said the CEO had been booted. chiefd was not
            // failing to actuate — it was correctly actuating an empty desired
            // set, because nothing had ever asked for the CEO.
            //
            // Until #1148 the root had an unconditional
            // `ActivityReason::OrganizationRoot` lease, and that lease — not
            // any start decision — was the only thing that made a CEO run at
            // all. #1148 deleted it so the root settles like everybody else,
            // which is the operator's ruling and stands. Its own commit
            // message named the consequence and the required answer: whatever
            // brings the root back must SUPPLY DEMAND, not re-exempt the CEO.
            // #1149 supplied it for the operator's ARRIVAL, which posted the
            // now-deleted `prepare-ceo-only` route. Genesis had no such moment,
            // so a company created without an attach — exactly what the
            // Founder's launch tool does — was born with no demand for anybody.
            // This call is the store op's LAST caller, and it is the honest
            // one.
            //
            // This is that missing moment, and it is the honest one: the
            // creation of a company is an explicit, durable decision that its
            // CEO should be running.
            //
            // A refusal here is NOT allowed to fail genesis. The company is
            // created and committed at this point; answering an error would
            // tell the caller their company does not exist when it does, and
            // invite the retry that earns `already-exists`. The CEO simply
            // stays unrequested until an attach asks, which is precisely the
            // behaviour that existed before this line.
            if let Err(error) = source.company.prepare_ceo_only(genesis_at).await {
                tracing::warn!(
                    event = "genesis.ceo_start_decision_failed",
                    slug = %source.org_documents_slug,
                    error = %error,
                    "the company committed but its CEO was not asked for; it will \
                     have no runtime until an operator attaches"
                );
            }
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"created": true})))
        }
        Ok(chiefd_core::store::organization_rows::ManifestGenesisOutcome::AlreadyExists) => {
            Err(RouteError::conflict(
                "organization-exists",
                "normalized organization rows already exist for this company",
            ))
        }
        Err(chiefd_core::error::ChiefdError::Refused(refusal)) => {
            Err(RouteError::refused(refusal.code, refusal.message.clone()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// --- org person-contracts rows (org-data-normalization P0, N2-contracts) ---
//
// Sibling of the manifest route pair: the `person-contracts` document (per-
// person AGENTS.md contract text) on its own `person_contracts` table. Same
// own-company gate and same dispatch into `CompanyDb`; publication is a direct
// atomic row write and `org_events` stays an audit trail, not a read revision.
// Item D: a document with unmodeled keys is a 422 `unmodeled-keys`, replicating
// the manifest publish handler's INLINE 422 (company_error maps Refused->400,
// so the 422 mapping MUST live here, not there).

#[derive(serde::Deserialize)]
struct OrgPersonContractsReadRequest {
    /// The own-company documentKey; a foreign slug is served `found:false`.
    slug: String,
}

#[derive(serde::Serialize)]
struct OrgPersonContractsReadResponse {
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    contracts: Option<String>,
}

async fn org_person_contracts_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgPersonContractsReadRequest>,
) -> Result<Json<OrgPersonContractsReadResponse>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Ok(Json(OrgPersonContractsReadResponse { found: false, contracts: None }));
    };
    match source.company.org_person_contracts_read().await.map_err(|e| company_error(&e))? {
        Some(doc) => {
            let body = serde_json::to_string(&doc).map_err(encode_fault)?;
            Ok(Json(OrgPersonContractsReadResponse { found: true, contracts: Some(body) }))
        }
        None => Ok(Json(OrgPersonContractsReadResponse { found: false, contracts: None })),
    }
}

// --- AGENTS.md projection plan (E7-S3) -------------------------------------
//
// Moves the "does workspace/AGENTS.md match the stored contract?" MD5
// comparison from TypeScript into Rust: a pure read that decides `write`
// (with the text to overwrite the file with) or `keep`, per requested person.
// TS becomes a dumb actuator of the returned action and never compares
// hashes. Same own-company gate as the sibling read.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectionPlanObserved {
    person_id: String,
    #[serde(default)]
    md5: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgPersonContractsProjectionPlanRequest {
    slug: String,
    observed: Vec<ProjectionPlanObserved>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectionPlanActionResponse {
    person_id: String,
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(serde::Serialize)]
struct OrgPersonContractsProjectionPlanResponse {
    actions: Vec<ProjectionPlanActionResponse>,
}

async fn org_person_contracts_projection_plan(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgPersonContractsProjectionPlanRequest>,
) -> Result<Json<OrgPersonContractsProjectionPlanResponse>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let observed: Vec<chiefd_core::store::person_contracts::rows::ObservedContract> = req
        .observed
        .into_iter()
        .map(|o| chiefd_core::store::person_contracts::rows::ObservedContract {
            person_id: o.person_id,
            md5: o.md5,
        })
        .collect();
    match source.company.org_person_contracts_projection_plan(observed).await {
        Ok(plan) => Ok(Json(OrgPersonContractsProjectionPlanResponse {
            actions: plan
                .into_iter()
                .map(|(person_id, action)| match action {
                    chiefd_core::store::person_contracts::rows::ProjectionAction::Write {
                        text,
                    } => ProjectionPlanActionResponse {
                        person_id,
                        action: "write",
                        text: Some(text),
                    },
                    chiefd_core::store::person_contracts::rows::ProjectionAction::Keep => {
                        ProjectionPlanActionResponse { person_id, action: "keep", text: None }
                    }
                })
                .collect(),
        })),
        // unknown-person-contract is a 422 with the machine code + detail,
        // same shape as the sibling publish route's item-D refusal.
        Err(chiefd_core::error::ChiefdError::Refused(refusal)) => {
            Err(RouteError::refused(refusal.code, refusal.message.clone()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// --- API-host launch profile (E7-S8, #893) ---------------------------------
//
// This is deliberately a projection route rather than another variant of the
// runtime launch catalog. Rust supplies the resolved, non-secret API/RPC child
// facts; the API host later adds only its own process-local identity. The live
// source is absent from standalone/migration routers, where manufacturing a
// path or daemon URL would be a split-brain bug.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgApiHostLaunchProfileReadRequest {
    slug: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiHostLaunchProfileResponsePlan {
    person_id: String,
    cwd: String,
    env: std::collections::BTreeMap<String, String>,
    session_file: Option<String>,
    tools: Vec<String>,
    display_name: String,
}

/// Who is actuating this company — published as a fact, not enforced as a gate.
///
/// This read used to REFUSE outside shadow mode, which made the launch half of
/// the client-agnostic per-person contract unreadable by the one client that
/// needs it most: a operator client runs against a company in `apply` by
/// definition. Reading a fact is not actuating on it, and only the reader knows
/// whether it is about to. All three values the refusal carried are here, so no
/// caller lost information; what left is chiefd's sentence about what the
/// caller should do next.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiHostActuationFacts {
    effective_mode: String,
    configured_mode: String,
    breaker_tripped: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OrgApiHostLaunchProfileReadResponse {
    actuation: ApiHostActuationFacts,
    plans: Vec<ApiHostLaunchProfileResponsePlan>,
}

async fn org_api_host_launch_profile_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgApiHostLaunchProfileReadRequest>,
) -> Result<Json<OrgApiHostLaunchProfileReadResponse>, RouteError> {
    let Some(source) = supervision_live.filter(|source| req.slug == source.org_documents_slug)
    else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let Some(profile_source) = source.api_host_launch_profile else {
        return Err(RouteError::unavailable(
            "api-host-launch-profile-unavailable",
            "this ChiefD surface has no live API-host launch-profile source",
        ));
    };
    match profile_source.read().await {
        Ok(read) => Ok(Json(OrgApiHostLaunchProfileReadResponse {
            actuation: ApiHostActuationFacts {
                effective_mode: read.actuation.effective_mode,
                configured_mode: read.actuation.configured_mode,
                breaker_tripped: read.actuation.breaker_tripped,
            },
            plans: read
                .profiles
                .into_iter()
                .map(|plan| ApiHostLaunchProfileResponsePlan {
                    person_id: plan.person_id,
                    cwd: plan.cwd.display().to_string(),
                    env: plan.env,
                    session_file: plan.session_file.map(|path| path.display().to_string()),
                    tools: plan.tools,
                    display_name: plan.display_name,
                })
                .collect(),
        })),
        Err(error) => Err(match &error {
            chiefd_host::converge_apply::ApiHostLaunchProfileError::NotMaterialized { .. } => {
                RouteError::refused(error.code(), error.to_string())
            }
            chiefd_host::converge_apply::ApiHostLaunchProfileError::SurfaceNotBound => {
                RouteError::unavailable(error.code(), error.to_string())
            }
            // A manifest or session epoch that was never written is a company
            // that has not been through genesis — a product state the caller
            // acts on, not a fault. It was a 500.
            chiefd_host::converge_apply::ApiHostLaunchProfileError::Manifest(_)
            | chiefd_host::converge_apply::ApiHostLaunchProfileError::SessionEpoch(_) => {
                RouteError::not_found(error.code(), error.to_string())
            }
        }),
    }
}

// --- projection reconcile (#739 P2) -----------------------------------------
//
// The synchronous bridge into `reconcile_cycle` that `runtime_waker.rs`
// deliberately does not provide -- see
// the design record. This calls
// `reconcile_cycle` on the request task, reusing its existing `begin_cycle`/
// `end_cycle` single-flight claim rather than any new lock: the HTTP caller
// and the daemon's own interval/notify-driven duty loop contend for the SAME
// per-company claim. No caller-supplied fence/projection is accepted --
// `ConvergeActuator::reconcile`, the only production caller of
// `reconcile_cycle` today, never accepts one either, and a single-flight skip
// silently discarding one is exactly the defect that field would have risked
// (see the plan's "does a caller-supplied fence survive a skip?" section).
// `projection: None` here is the same already-exercised "plan from current
// durable state" input every non-legacy-store production pass already uses.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgProjectionReconcileRequest {
    slug: String,
    correlation_id: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OrgProjectionReconcileSkipped {
    reason: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OrgProjectionReconcileResponse {
    correlation_id: String,
    applied: bool,
    desired_people: usize,
    retry_after_floor: bool,
    skipped: Option<OrgProjectionReconcileSkipped>,
    notes: Vec<String>,
}

/// `begin_cycle`'s `CycleGate::Skipped` path is not surfaced to this router
/// as a typed variant -- `reconcile_cycle` folds it into `ReconcileReport`
/// (`applied: false`, all counts zero, one note of the exact shape
/// `format!("skipped: {reason:?}")`; see `converge_apply::cycle::reconcile_cycle`).
/// This reads that note back out rather than inventing a parallel signal.
///
/// PRODUCER: `apps/chiefd/crates/chiefd-host/src/converge_apply/cycle.rs`,
/// `reconcile_cycle`, the `CycleGate::Skipped(reason) => { ... }` arm --
/// currently `notes: vec![format!("skipped: {reason:?}")]`. This string
/// match is the ONLY thing coupling that literal to this parser; the
/// compiler enforces nothing here. If that note's wording ever changes,
/// `projection_reconcile_tests::two_concurrent_requests_for_the_same_company_serialize_to_one_skip`
/// (`chiefd-api/src/docstore/router.rs`) drives a REAL skip through a real
/// `reconcile_cycle` call and asserts this parser reads it back correctly --
/// that test, not the literal-fixture parser tests beside it, is what
/// reddens when producer and parser drift apart.
const SKIPPED_NOTE_PREFIX: &str = "skipped: ";

fn skipped_from_report(
    report: &chiefd_core::runtime::duty_hooks::ReconcileReport,
) -> Option<OrgProjectionReconcileSkipped> {
    report.notes.iter().find_map(|note| {
        note.strip_prefix(SKIPPED_NOTE_PREFIX)
            .map(|reason| OrgProjectionReconcileSkipped { reason: reason.to_string() })
    })
}

async fn org_projection_reconcile(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgProjectionReconcileRequest>,
) -> Result<Json<OrgProjectionReconcileResponse>, RouteError> {
    let Some(source) = supervision_live.filter(|source| req.slug == source.org_documents_slug)
    else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    // #751/P8: NO host executor. `reconcile_cycle` stopped taking one when
    // chiefd stopped actuating — a converge pass now publishes the desired set
    // and the safety policy and applies nothing, so there is no machine for a
    // route to hand it. The `ActuatorConfig` is still required, because the
    // pass still reads the company's socket, data root and ramp from it.
    let Some(actuator_config) = source.reconcile_actuator_config.clone() else {
        return Err(RouteError::unavailable(
            "projection-reconcile-unavailable",
            "this ChiefD surface has no actuator config",
        ));
    };

    let report = chiefd_host::converge_apply::reconcile_cycle(
        &source.company,
        &actuator_config,
        chiefd_core::runtime::duty_hooks::ActuationMode::Apply,
        None,
    )
    .await
    .map_err(|error| RouteError::fault("reconcile-failed", error.to_string()))?;

    // A pass this route could not run is a REQUEST, not an answer. The
    // reconcile engine keeps a five-second single-flight floor, and a
    // foreground converge fired straight after a durable write lands inside it
    // routinely -- the wake the committing route already sent starts the
    // daemon's own pass milliseconds earlier, and this one is refused. Until
    // now that refusal was reported to the caller and then dropped: nothing
    // re-ran it, so a change a caller had explicitly asked to converge waited
    // out the reactive fallback floor anyway.
    //
    // Re-arming is one `notify_one` on the SAME `reconcile_trigger` that every
    // committed mutation already uses -- no second mechanism, no actuation
    // here, no bypass of the diff. It cannot be lost and cannot storm:
    // `Notify` holds one permit, so a wake that arrives while a pass is in
    // flight is taken by the drive loop's very next `notified()` instead of
    // being swallowed; and the pass that permit schedules will, if IT is
    // floored too, arm exactly one delayed replay through the daemon's
    // `schedule_reconcile_floor_retry` (coalesced by an `AtomicBool`, so a
    // burst is one timer, not a storm). The chain therefore ends in a legal
    // pass after at most one floor, instead of at the next interval.
    //
    // Gated on `applied` rather than on `retry_after_floor` for the reason the
    // caller-side warning is: a pass skipped while another was in flight
    // reports `applied:false` with `retry_after_floor:false`, and it needs the
    // same re-arm. An applied pass converged and asks for nothing.
    if !report.applied {
        wake_reconcile(&source);
    }

    Ok(Json(OrgProjectionReconcileResponse {
        correlation_id: req.correlation_id,
        applied: report.applied,
        desired_people: report.desired_people,
        retry_after_floor: report.retry_after_floor,
        skipped: skipped_from_report(&report),
        notes: report.notes.clone(),
    }))
}

// --- org settings: launcher root (E7-S3) ------------------------------------
//
// Replaces `state/launcher.json`: the absolute path of the source checkout
// that last materialized this company, now a column on the existing
// `org_settings` singleton. `publish` writes ONLY `launcher_root` — the four
// policy ints stay owned by the manifest genesis/policy paths.

#[derive(serde::Deserialize)]
struct OrgSettingsReadRequest {
    /// The own-company documentKey; a foreign slug is served `found:false`.
    slug: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OrgSettingsDto {
    launcher_root: Option<String>,
    supervision_interval_ms: i64,
    acknowledgement_timeout_ms: i64,
    acknowledgement_retry_limit: i64,
    replacement_limit: i64,
}

#[derive(serde::Serialize)]
struct OrgSettingsReadResponse {
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<OrgSettingsDto>,
}

async fn org_settings_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgSettingsReadRequest>,
) -> Result<Json<OrgSettingsReadResponse>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Ok(Json(OrgSettingsReadResponse { found: false, settings: None }));
    };
    match source.company.org_settings_read().await.map_err(|e| company_error(&e))? {
        Some(settings) => Ok(Json(OrgSettingsReadResponse {
            found: true,
            settings: Some(OrgSettingsDto {
                launcher_root: settings.launcher_root,
                supervision_interval_ms: settings.supervision_interval_ms,
                acknowledgement_timeout_ms: settings.acknowledgement_timeout_ms,
                acknowledgement_retry_limit: settings.acknowledgement_retry_limit,
                replacement_limit: settings.replacement_limit,
            }),
        })),
        None => Ok(Json(OrgSettingsReadResponse { found: false, settings: None })),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgSettingsPublishRequest {
    slug: String,
    /// ISO-8601 event stamp for the `org_events` row (caller clock authority).
    #[serde(default)]
    at: String,
    launcher_root: String,
}

async fn org_settings_publish(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgSettingsPublishRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    // H.2.5: the override pins a CHECKOUT, never the install. An exe-derived
    // `…/versions/<v>/resources` root must never be persisted — a later `chief
    // upgrade` prunes that directory, and a company pinned to it would break at
    // its next materialization. An empty root (the documented way to CLEAR the
    // override) is not of this shape and passes.
    if host_primitives::install::is_installed_resource_root(std::path::Path::new(
        &req.launcher_root,
    )) {
        return Err(RouteError::refused(
            "launcher-root-install-path",
            format!(
                "'{}' is an install path under 'versions/', not a checkout — a later 'chief \
                 upgrade' prunes it. The launcher-root override pins a checkout; the install root \
                 is resolved fresh on every boot and is never persisted.",
                req.launcher_root
            ),
        ));
    }
    match source.company.org_settings_publish_launcher_root(req.at, req.launcher_root).await {
        Ok(seq) => Ok(Json(serde_json::json!({"applied": true, "seq": seq}))),
        // No `org_settings` row for a live company means genesis has not run
        // — the same "unknown company" shape the foreign-slug branch above
        // returns, not a 422 (this is not a caller validation mistake).
        Err(chiefd_core::error::ChiefdError::Refused(refusal))
            if refusal.code == chiefd_core::store::org_settings::UNKNOWN_COMPANY =>
        {
            Err(RouteError::not_found(refusal.code, refusal.message))
        }
        Err(chiefd_core::error::ChiefdError::Refused(refusal)) => {
            Err(RouteError::refused(refusal.code, refusal.message.clone()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// ---- activity rows (org-data-normalization P0, N4) -----------------------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgActivityReadRequest {
    /// The own-company documentKey; a foreign slug is served `found:false`.
    slug: String,
    /// Cached `org_events` seq. Equal ⇒ omit the aggregate.
    #[serde(default)]
    if_seq_not: Option<i64>,
}

#[derive(serde::Serialize)]
struct OrgActivityReadResponse {
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ledger: Option<String>,
    seq: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    unchanged: Option<bool>,
}

async fn org_activity_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgActivityReadRequest>,
) -> Result<Json<OrgActivityReadResponse>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Ok(Json(OrgActivityReadResponse {
            found: false,
            ledger: None,
            seq: 0,
            unchanged: None,
        }));
    };
    if let Some(seq) = unchanged_org_seq(&source, req.if_seq_not).await? {
        return Ok(Json(OrgActivityReadResponse {
            found: true,
            ledger: None,
            seq,
            unchanged: Some(true),
        }));
    }
    match source.company.activity_read().await.map_err(|e| company_error(&e))? {
        Some((ledger, seq)) => {
            let body = serde_json::to_string(&ledger).map_err(encode_fault)?;
            Ok(Json(OrgActivityReadResponse {
                found: true,
                ledger: Some(body),
                seq,
                unchanged: None,
            }))
        }
        None => Ok(Json(OrgActivityReadResponse {
            found: false,
            ledger: None,
            seq: 0,
            unchanged: None,
        })),
    }
}

// --- session-maintenance rows (org-data-normalization P0, N5) -------------
//
// Same reference shape as the manifest read: gate on own-company, dispatch into
// the live `CompanyDb` (rows live in chief.db), fence on the `org_events` seq.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionMaintenanceReadRequest {
    /// The own-company documentKey; a foreign slug is served `found:false`.
    slug: String,
    #[serde(default)]
    if_seq_not: Option<i64>,
}

#[derive(serde::Serialize)]
struct SessionMaintenanceReadResponse {
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ledger: Option<String>,
    seq: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    unchanged: Option<bool>,
}

async fn session_maintenance_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<SessionMaintenanceReadRequest>,
) -> Result<Json<SessionMaintenanceReadResponse>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Ok(Json(SessionMaintenanceReadResponse {
            found: false,
            ledger: None,
            seq: 0,
            unchanged: None,
        }));
    };
    if let Some(seq) = unchanged_org_seq(&source, req.if_seq_not).await? {
        return Ok(Json(SessionMaintenanceReadResponse {
            found: true,
            ledger: None,
            seq,
            unchanged: Some(true),
        }));
    }
    match source.company.session_maintenance_read().await.map_err(|e| company_error(&e))? {
        Some((ledger, seq)) => {
            let body = serde_json::to_string(&ledger).map_err(encode_fault)?;
            Ok(Json(SessionMaintenanceReadResponse {
                found: true,
                ledger: Some(body),
                seq,
                unchanged: None,
            }))
        }
        // #983: no session-maintenance ledger exists yet, but the seq this
        // route answers with is the COMPANY-WIDE `org_events` cursor, not a
        // per-document one -- the same cursor every other mutation type
        // (goals, delegates, sends...) advances. Hardcoding `seq: 0` here was
        // correct only for a company that has done literally nothing else yet,
        // i.e. never for a real company, so a caller comparing this seq
        // against a later read saw a false change on every first read. Fetch
        // the real current cursor unconditionally.
        None => {
            let seq = source.company.org_current_seq().await.map_err(|e| company_error(&e))?;
            Ok(Json(SessionMaintenanceReadResponse {
                found: false,
                ledger: None,
                seq,
                unchanged: None,
            }))
        }
    }
}

// --- B4 singleton-sweep row routes (org-data-normalization P0) ------------
//
// Uniform `/v1/org/<store>/read` routes over each ported store's `CompanyDb`
// seam, all shaped exactly like the manifest reference: own-company gate (a
// foreign/absent slug reads `found:false`) and a fence on the `org_events`
// seq. One store — runtime — still carries the publish half, because a caller
// exists for it; every other publish half was deleted with no caller.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgRowReadRequest {
    /// The own-company documentKey; a foreign slug is served `found:false`.
    slug: String,
    #[serde(default)]
    if_seq_not: Option<i64>,
}

#[derive(serde::Serialize)]
struct OrgRowReadResponse {
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    doc: Option<String>,
    seq: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    unchanged: Option<bool>,
}

/// Publish request shape for direct atomic singleton routes. The sequence
/// returned by a read is an audit cursor only, so it is deliberately not an
/// accepted input here.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectOrgRowPublishRequest {
    slug: String,
    /// A serialized store document.
    doc: String,
}

/// Generate the READ half of a direct-atomic singleton row route.
///
/// Most stores in this family have only this half: the publish siblings were
/// deleted once the sweep proved nobody called them, and a store's row is
/// written in-process through `CompanyDb` inside the daemon's own
/// transactions. `direct_org_row_route_pair!` below adds the publish half for
/// the one store that still has a caller.
macro_rules! direct_org_row_read_route {
    ($read_fn:ident, $read_method:ident) => {
        async fn $read_fn(
            Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
            Json(req): Json<OrgRowReadRequest>,
        ) -> Result<Json<OrgRowReadResponse>, RouteError> {
            let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
                return Ok(Json(OrgRowReadResponse {
                    found: false,
                    doc: None,
                    seq: 0,
                    unchanged: None,
                }));
            };
            if let Some(seq) = unchanged_org_seq(&source, req.if_seq_not).await? {
                return Ok(Json(OrgRowReadResponse {
                    found: true,
                    doc: None,
                    seq,
                    unchanged: Some(true),
                }));
            }
            match source.company.$read_method().await.map_err(|e| company_error(&e))? {
                Some((doc, seq)) => {
                    let body = serde_json::to_string(&doc).map_err(encode_fault)?;
                    Ok(Json(OrgRowReadResponse {
                        found: true,
                        doc: Some(body),
                        seq,
                        unchanged: None,
                    }))
                }
                // #984: as #983's session_maintenance_read/org_supervision_read
                // fix -- every $read_method generated by this macro fences its
                // seq on the company-wide org_events cursor
                // (rows_txn::current_seq), not a per-document one starting at
                // 0, so hardcoding seq: 0 here was correct only for a company
                // with zero prior activity, i.e. never for a real one. The
                // CAS siblings that made this an ACUTE defect are now deleted
                // along with every other caller-less publish, but "inert
                // because of who happens to call it today" was never a
                // property of the design, so the class stays fixed once here.
                None => {
                    let seq =
                        source.company.org_current_seq().await.map_err(|e| company_error(&e))?;
                    Ok(Json(OrgRowReadResponse { found: false, doc: None, seq, unchanged: None }))
                }
            }
        }
    };
}

/// Generate a direct-atomic singleton route pair: the read half above plus a
/// publish half. It intentionally does not share a caller-sequence request:
/// every generated endpoint rejects legacy `expectedSeq` rather than silently
/// reintroducing caller-side CAS.
///
/// # The publish half carries the whole-company fence (auth B2)
///
/// It is written HERE rather than in a wrapper around the generated function,
/// and that is a consequence of #1107 rather than a preference. While twelve
/// pairs shared this arm, a fence in it would have silently authorized eleven
/// routes belonging to other tracks, so B2 renamed the generated function and
/// wrapped it. #1107 deleted every caller-less publisher, leaving `runtime` as
/// THE ONE surviving pair — the shared arm is no longer shared, the wrapper's
/// whole reason is gone, and the fence belongs in the one place the publish
/// actually happens.
///
/// The row this generates for is the company's entire runtime state document,
/// so overwriting it reaches every person in the company; the department such a
/// write acts on is the ROOT department, which is what
/// [`require_company_wide_authority`] asks about. A future second pair must
/// decide its own fence before it is added, exactly as this one did — the macro
/// is now a single-row generator, not a shared surface.
///
/// `packages/piing`'s `OrganizationToolContract.test.ts` seeds runtime rows
/// through `/v1/org/runtime/publish` against a real daemon, and since A7
/// (#1114) that harness AUTHENTICATES — it reads the `operator.key` the daemon
/// mints under `<data-root>/keys` and presents a bearer on every call. It is
/// admitted here on the daemon-scoped arm rather than the rollout arm:
/// `operator` is one of the two principals `chiefd run` enrols and it names no
/// person row, so [`caller_scope_actor`] allows it. Nothing was added to the
/// harness for this fence; the reason it passes simply improved.
macro_rules! direct_org_row_route_pair {
    ($read_fn:ident, $publish_fn:ident, $read_method:ident, $publish_method:ident, $doc:ty $(, $no_wake:ident)? $(,)?) => {
        direct_org_row_read_route!($read_fn, $read_method);

        async fn $publish_fn(
            crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
            Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
            Json(req): Json<DirectOrgRowPublishRequest>,
        ) -> Result<Json<serde_json::Value>, RouteError> {
            let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
                return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
            };
            require_company_wide_authority(
                &caller,
                &source,
                "overwrite the runtime row",
            )
            .await?;
            let doc: $doc = serde_json::from_str(&req.doc).map_err(|e| {
                RouteError::malformed("malformed-doc", e.to_string())
            })?;
            match source.company.$publish_method(doc).await {
                Ok(seq) => {
                    // Wake-by-default (arch-audit F1, Step 6): every publish of
                    // a row the reconcile cycle can read nudges the trigger so
                    // the change converges reactively instead of riding the
                    // ~30s cadence. Opting OUT requires passing the trailing
                    // `no_reconcile_wake` ident and a stated reason in the
                    // enumeration test — silence wakes.
                    let wake_opted_out = false $(|| { let _ = stringify!($no_wake); true })?;
                    if !wake_opted_out {
                        wake_reconcile(&source);
                    }
                    Ok(Json(serde_json::json!({"applied": true, "seq": seq})))
                }
                Err(chiefd_core::error::ChiefdError::Refused(refusal)) => Err(RouteError::refused(refusal.code, refusal.message.clone())),
                Err(other) => Err(company_error(&other)),
            }
        }
    };
}

direct_org_row_read_route!(org_session_epoch_read, session_epoch_read);
direct_org_row_read_route!(org_goal_delivery_quiesce_read, goal_delivery_quiesce_read);
direct_org_row_read_route!(org_operator_escalation_push_read, operator_escalation_push_read);
direct_org_row_read_route!(org_runtime_owner_read, runtime_owner_read);
direct_org_row_read_route!(org_launch_intent_read, launch_intent_read);
direct_org_row_read_route!(org_mutation_journal_read, mutation_journal_read);
direct_org_row_read_route!(org_health_monitor_read, health_monitor_read);
// The one surviving pair. `packages/piing`'s tool-contract suite seeds runtime
// rows through `/v1/org/runtime/publish` against a real daemon, so this
// publish half has a caller and the others did not.
direct_org_row_route_pair!(
    org_runtime_read,
    org_runtime_publish,
    runtime_read,
    runtime_publish,
    chiefd_core::store::runtime_rows::RuntimeState
);

direct_org_row_read_route!(org_operator_escalation_intents_read, operator_escalation_intents_read);
// #861: the stored converge/apply actuation mode (shadow/apply) a company is
// configured for. Read returns the STORED ConvergeSafetyState verbatim — the
// same "return the doc, absence is real absence" shape every sibling route
// here already has — never the breaker-folded `effective_config()`
// projection. apps/api's planned `hosting()` verb (#800) is the first
// consumer, via packages/chiefing's `readConvergeSafety`. The write is
// `set-actuation-config`, not a whole-document publish.
direct_org_row_read_route!(org_converge_safety_read, converge_safety_read);

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperatorEscalationIntentInsertRequest {
    slug: String,
    intent: chiefd_core::store::operator_escalation_intents_rows::OperatorEscalationIntent,
}

fn semantic_insert_error(error: chiefd_core::ChiefdError) -> RouteError {
    match error {
        chiefd_core::error::ChiefdError::Refused(refusal) => {
            RouteError::refused(refusal.code, refusal.message.clone())
        }
        other => company_error(&other),
    }
}

async fn org_operator_escalation_intents_insert(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OperatorEscalationIntentInsertRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let outcome = source
        .company
        .operator_escalation_intents_insert(req.intent)
        .await
        .map_err(semantic_insert_error)?;
    let (status, seq) = match outcome {
        chiefd_core::store::operator_escalation_intents_rows::InsertOperatorEscalationOutcome::Inserted { seq } => {
            ("inserted", seq)
        }
        chiefd_core::store::operator_escalation_intents_rows::InsertOperatorEscalationOutcome::Duplicate { seq } => {
            ("duplicate", seq)
        }
    };
    wake_reconcile(&source);
    Ok(Json(serde_json::json!({"status": status, "seq": seq})))
}

// --- supervision ledger rows (org-data-normalization P0, N3) --------------
//
// A mechanical copy of the manifest read above, carrying the WHOLE
// SupervisionLedger. Same own-company gate, same dispatch into `CompanyDb` (the
// rows live in `chief.db`, never the typed `DocStore`). The client sends
// `{slug}` and reads back `{found, ledger?, seq?}`; `seq` is immutable audit
// output, never a request field. The publish, publish-cas and clear siblings
// are deleted — the TypeScript repository that posted them is gone.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgSupervisionReadRequest {
    /// The own-company documentKey; a foreign slug is served `found:false`.
    slug: String,
    #[serde(default)]
    if_seq_not: Option<i64>,
}

#[derive(serde::Serialize)]
struct OrgSupervisionReadResponse {
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ledger: Option<String>,
    seq: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    unchanged: Option<bool>,
}

async fn org_supervision_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgSupervisionReadRequest>,
) -> Result<Json<OrgSupervisionReadResponse>, RouteError> {
    // Isolation: this process serves only its own company. A foreign/absent slug
    // gets `found:false`, mirroring the manifest read's fall-through.
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Ok(Json(OrgSupervisionReadResponse {
            found: false,
            ledger: None,
            seq: 0,
            unchanged: None,
        }));
    };
    if let Some(seq) = unchanged_org_seq(&source, req.if_seq_not).await? {
        return Ok(Json(OrgSupervisionReadResponse {
            found: true,
            ledger: None,
            seq,
            unchanged: Some(true),
        }));
    }
    match source.company.supervision_read().await.map_err(|e| company_error(&e))? {
        Some((ledger, seq)) => {
            // The relational half is #[serde(skip)] on SupervisionLedger, so a
            // plain to_string would strip every assignment/effect from the read
            // RESPONSE. Serialize through the launcher-JSON helper that splices
            // the half back (the read-side twin of the publish-side adoption).
            let body = chiefd_core::store::supervision::to_launcher_json(&ledger)
                .map_err(|e| company_error(&e))?;
            Ok(Json(OrgSupervisionReadResponse {
                found: true,
                ledger: Some(body),
                seq,
                unchanged: None,
            }))
        }
        // #983: same shape as session_maintenance_read's identical fix --
        // the seq here is the company-wide `org_events` cursor (writer.rs's
        // `supervision_read`), not a per-document seq, so hardcoding `seq: 0`
        // on "no ledger yet" reported a cursor no caller could compare
        // against for any company with prior activity. Fetch the real current
        // cursor unconditionally.
        None => {
            let seq = source.company.org_current_seq().await.map_err(|e| company_error(&e))?;
            Ok(Json(OrgSupervisionReadResponse {
                found: false,
                ledger: None,
                seq,
                unchanged: None,
            }))
        }
    }
}

// --- org mailbox rows (org-data-normalization P0, N-mailbox) ---------------
//
// Mirrors the manifest reads for `/v1/org/mailbox/*`: own-company gate,
// dispatch into the live `CompanyDb` (rows live in `chief.db`), chiefd-core owns
// the single `BEGIN IMMEDIATE`. The whole-mailbox publish is deleted; the
// mailbox is written by the send/delivery verbs, never by a caller handing
// back a whole document.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgMailboxReadRequest {
    slug: String,
    #[serde(default)]
    if_seq_not: Option<i64>,
}

#[derive(serde::Serialize)]
struct OrgMailboxReadResponse {
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mailbox: Option<String>,
    seq: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    unchanged: Option<bool>,
}

async fn org_mailbox_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgMailboxReadRequest>,
) -> Result<Json<OrgMailboxReadResponse>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Ok(Json(OrgMailboxReadResponse {
            found: false,
            mailbox: None,
            seq: 0,
            unchanged: None,
        }));
    };
    if let Some(seq) = unchanged_org_seq(&source, req.if_seq_not).await? {
        return Ok(Json(OrgMailboxReadResponse {
            found: true,
            mailbox: None,
            seq,
            unchanged: Some(true),
        }));
    }
    let (snapshot, seq) = source.company.mailbox_read().await.map_err(|e| company_error(&e))?;
    let body = serde_json::to_string(&snapshot).map_err(encode_fault)?;
    Ok(Json(OrgMailboxReadResponse { found: true, mailbox: Some(body), seq, unchanged: None }))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgMailboxReadPersonRequest {
    slug: String,
    person_id: String,
    #[serde(default)]
    if_seq_not: Option<i64>,
}

async fn org_mailbox_read_person(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgMailboxReadPersonRequest>,
) -> Result<Json<OrgMailboxReadResponse>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Ok(Json(OrgMailboxReadResponse {
            found: false,
            mailbox: None,
            seq: 0,
            unchanged: None,
        }));
    };
    if let Some(seq) = unchanged_org_seq(&source, req.if_seq_not).await? {
        return Ok(Json(OrgMailboxReadResponse {
            found: true,
            mailbox: None,
            seq,
            unchanged: Some(true),
        }));
    }
    let (snapshot, seq) =
        source.company.mailbox_read_person(req.person_id).await.map_err(|e| company_error(&e))?;
    let body = serde_json::to_string(&snapshot).map_err(encode_fault)?;
    Ok(Json(OrgMailboxReadResponse { found: true, mailbox: Some(body), seq, unchanged: None }))
}

// Fence-free per-person delta: upserts (a JSON array of MailboxEntry) + deletes
// (envelope_ids). Always applies (no CAS) — returns the new seq.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgMailboxDeltaRequest {
    slug: String,
    person_id: String,
    upserts: String,
    deletes: Vec<String>,
    at: String,
}

async fn org_mailbox_delta(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgMailboxDeltaRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let upserts: Vec<chiefd_core::store::mailbox_rows::MailboxEntry> =
        serde_json::from_str(&req.upserts)
            .map_err(|e| RouteError::malformed("malformed-mailbox", e.to_string()))?;
    // NO `bind_caller` HERE, AND IT IS THE POINT OF THIS ROUTE.
    //
    // `personId` is WHOSE MAILBOX, not who is asking, and the intercom calls
    // this route both ways — `personId = recipient` when one person messages
    // another, `personId = context.personId` when a pane settles its own queue.
    // Binding the two would refuse every message the product sends, and it
    // would look right while doing it.
    //
    // So the caller travels down as an `actor` and CORE decides, per entry:
    // a delta is either consumption of your own mailbox or a delivery from you.
    // The rule lives beside the data because writing it is what defines a
    // delivery on the wire, and a handler that owned that definition would be
    // a second answer to it.
    //
    // A NON-PERSON PRINCIPAL IS REFUSED OUTRIGHT, and this is the one place in
    // the packet that departs from "a daemon-scoped identity is unconditionally
    // in scope". It is not an exception to that rule; the rule has nothing to
    // say here. Both halves of the mailbox rule compare against a PERSON —
    // consumption is `personId == caller` and delivery is
    // `fromPersonId == caller` — so an operator, service or channel credential
    // cannot satisfy either definition, and allowing it would let any service
    // token mint an entry in anybody's mailbox attributed to anybody. That is
    // the `from_person_id: "launcher"` forgery this route already refuses,
    // reopened by another door. Nothing needs the allowance: chiefd's own
    // delivery sink writes mailbox rows IN-PROCESS through `CompanyDb`, never
    // over HTTP.
    if caller.kind != chiefd_core::store::identities::IdentityKind::Person {
        return Err(RouteError::forbidden(
            "mailbox-delta-requires-a-person",
            format!(
                "caller '{}' is not a person identity; a mailbox delta is consumption of \
                 your own mail or a delivery from you, and neither is defined for a \
                 daemon-scoped principal",
                caller.principal
            ),
        ));
    }
    let actor = caller.principal.clone();
    // THE WAKE BELONGS TO THIS WRITE, AND ONLY THIS WRITE REACHES IT.
    //
    // A pending mailbox row is a reconcile input: `project_activity_fence`
    // reads the pending rows and grants launch intent to exactly their
    // recipients, because "a genuine durable envelope addressed to a specific
    // person IS work arriving and is itself the explicit, per-node decision
    // that authorizes exactly them". chiefd's own in-process delivery sink
    // already nudges the duty through `ReconcileWaker`; the HTTP delivery the
    // intercom performs had no such nudge, so the recipient waited out the
    // ~30s cadence and the intercom compensated by posting the COMPANY-WIDE
    // `/v1/org/runtime/launch` — which `require_company_wide_authority` grants
    // only to the head of the root department. Every upward reply to the CEO
    // was therefore `403 caller-out-of-company-scope`.
    //
    // The nudge is the whole repair, and it needs no authority at all: the
    // caller has already been judged, per entry, to be draining its own
    // mailbox or delivering from itself. A pass carries no caller and starts
    // nobody the durable ledger does not already name.
    match source.company.mailbox_delta(req.person_id, upserts, req.deletes, req.at, actor).await {
        Ok(seq) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"applied": true, "seq": seq})))
        }
        Err(chiefd_core::error::ChiefdError::Refused(refusal)) => {
            Err(RouteError::refused(refusal.code, refusal.message.clone()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

#[derive(serde::Deserialize)]
struct OrgMailboxListPersonsRequest {
    slug: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OrgMailboxListPersonsResponse {
    person_ids: Vec<String>,
}

async fn org_mailbox_list_persons(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgMailboxListPersonsRequest>,
) -> Result<Json<OrgMailboxListPersonsResponse>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Ok(Json(OrgMailboxListPersonsResponse { person_ids: Vec::new() }));
    };
    let person_ids = source.company.mailbox_list_persons().await.map_err(|e| company_error(&e))?;
    Ok(Json(OrgMailboxListPersonsResponse { person_ids }))
}

// --- org_ops atomic family: shutdown_person (member 1) --------------------
//
// Own-company slug gate, then dispatch into the `CompanyDb` wrapper which owns
// the single BEGIN IMMEDIATE. Family HTTP convention: 200 {applied:true,...} |
// 422 {code,detail} (policy/validation refusal) | 4xx/5xx transport faults.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrgPersonShutdownRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// The person to shut down.
    person_id: String,
    /// `"commanded"` (an operator/manager stop, requires `intentId`) or
    /// `"settle"` (an automatic idle settle, `intentId` omitted).
    kind: String,
    /// The originating `person-stop:<…>` id for a commanded stop.
    #[serde(default)]
    intent_id: Option<String>,
}

async fn org_person_shutdown(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonShutdownRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::{ShutdownKind, ShutdownOutcome};
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let kind = match req.kind.as_str() {
        "commanded" => match req.intent_id {
            Some(intent_id) => ShutdownKind::Commanded { intent_id },
            None => {
                return Err(RouteError::malformed(
                    "missing-intent-id",
                    "commanded shutdown requires intentId",
                ));
            }
        },
        "settle" => ShutdownKind::AutomaticSettle,
        other => {
            return Err(RouteError::malformed(
                "unknown-kind",
                format!("kind must be commanded|settle, got {other}"),
            ));
        }
    };
    match source
        .company
        .shutdown_person(req.person_id, kind, now_iso(), caller_actor(&caller))
        .await
    {
        Ok(ShutdownOutcome::Applied { transition_id }) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"applied": true, "transitionId": transition_id})))
        }
        // Policy refusal → 422 with the machine code (fable family convention:
        // a legitimate "no" is LOUD, never a quiet 200 body a retry loop misses).
        Ok(ShutdownOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// --- org_ops atomic family: appoint_department_head (member 2, H2) ----------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgPersonAppointHeadRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// The department whose head is being appointed.
    department_id: String,
    /// The person to appoint as the new head.
    successor_person_id: String,
    /// R4: move the outgoing head to this department; omit to leave in place.
    #[serde(default)]
    demote_to_department_id: Option<String>,
}

async fn org_person_appoint_head(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonAppointHeadRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::AppointOutcome;
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    match source
        .company
        .appoint_department_head(
            req.department_id,
            req.successor_person_id,
            req.demote_to_department_id,
            now_iso(),
            caller_actor(&caller),
        )
        .await
    {
        Ok(AppointOutcome::Applied) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"applied": true})))
        }

        // Policy refusal → 422 with the machine code (family convention).
        Ok(AppointOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// ---- org_ops family: create_department (P1-a) ----------------------------
//
// Own-company slug gate, dispatch into the `CompanyDb` wrapper (chiefd-core
// owns the single BEGIN IMMEDIATE). Business refusals are 422 values; there is
// no caller revision input or stale-sequence outcome: org_settings carries no
// revision counter, so this is a plain revisionless normalized write with no
// compatibility fence to advance.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgDepartmentCreateRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// Launcher-attested manager or explicit direct operator.
    requester: OrgStaffingRequester,
    /// The new department's id.
    department_id: String,
    /// The parent department id (a new department always has a parent).
    parent_id: String,
    /// The new department's name.
    name: String,
    /// The new department's purpose.
    #[serde(default)]
    purpose: String,
    /// Human/operator rationale retained in staffing history.
    #[serde(default = "default_department_create_reason")]
    reason: String,
    /// Optional typed child-unit metadata. Omitted requests remain ordinary
    /// department creates for wire compatibility.
    #[serde(default)]
    unit: Option<OrgDepartmentCreateUnit>,
    /// The explicit head decision (R3). `kind`: `"appoint-existing"` (requires
    /// `headPersonId`) or `"hire-new"` (requires the person seed fields).
    head: OrgDepartmentHead,
    /// What becomes of the department an appoint-existing head ALREADY heads.
    /// Absent is itself an answer the core refuses when they head something.
    #[serde(default)]
    vacates: Option<OrgHeadVacancy>,
    /// Optional initial workers. Every entry is a complete `"hire-new"` seed
    /// and commits with the department/head in the same SQL transaction.
    #[serde(default)]
    staff: Vec<OrgDepartmentHead>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum OrgDepartmentCreateUnit {
    /// An ordinary child department (the legacy omitted-request default).
    Department,
    /// A transient child contract with complete metadata.
    Contract { transient: OrgDepartmentContractMetadata },
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgDepartmentContractMetadata {
    engagement: String,
    launched_at: String,
    #[serde(default)]
    expires_at: Option<String>,
}

/// What becomes of the department the person being moved already heads.
///
/// One wire type for BOTH routes that can vacate a headship — department create
/// and person transfer — because it is one decision with one shape. Tagged and
/// kebab-cased, the same convention `head.kind` already uses.
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum OrgHeadVacancy {
    /// Promote a member of the vacated department to head it.
    #[serde(rename_all = "camelCase")]
    HandOver {
        /// A member of the department being left.
        successor_person_id: String,
    },
    /// The person is that department's last member; remove the emptied unit.
    Dissolve,
}

impl From<OrgHeadVacancy> for chiefd_core::store::org_ops::HeadVacancy {
    fn from(value: OrgHeadVacancy) -> Self {
        match value {
            OrgHeadVacancy::HandOver { successor_person_id } => {
                Self::HandOver { successor_person_id }
            }
            OrgHeadVacancy::Dissolve => Self::Dissolve,
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgDepartmentHead {
    /// `"appoint-existing"` | `"hire-new"`.
    kind: String,
    /// The head person's id (both decisions).
    person_id: String,
    /// hire-new only: display name.
    #[serde(default)]
    name: Option<String>,
    /// hire-new only: title.
    #[serde(default)]
    title: Option<String>,
    /// hire-new only: mandate.
    #[serde(default)]
    mandate: Option<String>,
    /// hire-new only: kind (`worker`|`head`|`executive`).
    #[serde(default)]
    person_kind: Option<String>,
    /// hire-new only: active / benched.
    #[serde(default)]
    employment_state: Option<String>,
    /// hire-new only: resident / on-demand.
    #[serde(default)]
    activation: Option<String>,
    /// hire-new only: tool and prompt child rows.
    ///
    /// TOMBSTONE (chief-home-is-cwd §4e): `skills`, `extensions` and `packages`
    /// stood beside `tools`. `deny_unknown_fields` above turns a caller that
    /// still sends one into a 400 rather than a silently dropped selection —
    /// which is the honest answer, because chief selects no Pi resource for
    /// anybody.
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    prompts: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgStaffingRequester {
    /// `"person"` | `"operator"`.
    kind: String,
    /// Required only for a launcher-attested person.
    #[serde(default)]
    person_id: Option<String>,
}

fn parse_staffing_requester(
    requester: OrgStaffingRequester,
) -> Result<(Option<String>, String), RouteError> {
    match requester.kind.as_str() {
        "person" => {
            let person_id =
                requester.person_id.filter(|value| !value.trim().is_empty()).ok_or_else(|| {
                    RouteError::malformed(
                        "invalid-requester",
                        "requester.personId is required when requester.kind is person",
                    )
                })?;
            Ok((Some(person_id.clone()), person_id))
        }
        "operator" if requester.person_id.is_none() => Ok((None, "operator".to_string())),
        "operator" => Err(RouteError::malformed(
            "invalid-requester",
            "requester.personId is forbidden when requester.kind is operator",
        )),
        other => Err(RouteError::malformed(
            "invalid-requester",
            format!("requester.kind must be person|operator, got {other}"),
        )),
    }
}

fn default_department_create_reason() -> String {
    "created department with explicit head decision".to_string()
}

fn parse_department_create_unit(
    raw: Option<OrgDepartmentCreateUnit>,
) -> Result<chiefd_core::store::org_ops::DepartmentCreateUnit, RouteError> {
    use chiefd_core::isotime::parse_iso_millis;
    use chiefd_core::store::org_ops::DepartmentCreateUnit;

    let Some(raw) = raw else { return Ok(DepartmentCreateUnit::Department) };
    let OrgDepartmentCreateUnit::Contract { transient } = raw else {
        return Ok(DepartmentCreateUnit::Department);
    };
    let Some(launched_at) = parse_iso_millis(&transient.launched_at) else {
        return Err(RouteError::malformed("invalid-unit", "contract unit requires non-empty transient engagement, ISO launchedAt, and a later ISO expiresAt when supplied"));
    };
    if transient.engagement.trim().is_empty()
        || transient.expires_at.as_deref().is_some_and(|expires_at| {
            parse_iso_millis(expires_at).is_none_or(|expires_at| expires_at <= launched_at)
        })
    {
        return Err(RouteError::malformed("invalid-unit", "contract unit requires non-empty transient engagement, ISO launchedAt, and a later ISO expiresAt when supplied"));
    }
    Ok(DepartmentCreateUnit::Contract(chiefd_core::store::organization::ContractMetadata {
        engagement: transient.engagement,
        launched_at: transient.launched_at,
        expires_at: transient.expires_at,
    }))
}

/// Fill in the identities and titles a caller may leave to chiefd (#751/R3).
///
/// The ids and the seed defaults used to be minted client-side, in TypeScript,
/// by `planDepartmentCreate` — so every caller carried a second opinion about
/// what a department's id is, and the rules drifted the moment chiefd changed
/// its own. They are chiefd's to decide, and they already exist here: this
/// applies `organization_spec`'s rules verbatim, so a department created
/// through this route is named exactly as one created by genesis.
///
/// A blank field means "you decide"; a supplied one is honoured untouched and
/// still validated downstream. Nothing here can rename an existing person: only
/// a `hire-new` seed is filled, and `appoint-existing` names somebody who is
/// already there, so a blank id is a caller error rather than an invitation.
fn mint_department_create_ids(req: &mut OrgDepartmentCreateRequest) -> Result<(), RouteError> {
    use chiefd_core::store::organization_spec::slugify;

    fn blank(value: &str) -> bool {
        value.trim().is_empty()
    }
    fn invalid(code: &'static str, detail: String) -> RouteError {
        RouteError::malformed(code, detail)
    }

    if blank(&req.department_id) {
        let local = slugify(&req.name);
        if local.is_empty() {
            return Err(invalid(
                "invalid-department-name",
                format!("name {:?} produces no usable id, and no departmentId was given", req.name),
            ));
        }
        // A nested unit's id is `<parent>-<local>` so ids stay globally unique
        // and readable without a lookup; the root's children keep the bare
        // local id. Same rule as `organization_spec`'s nested departments.
        req.department_id = if req.parent_id == chiefd_core::store::organization::ROOT_DEPARTMENT_ID
        {
            local
        } else {
            format!("{}-{local}", req.parent_id)
        };
    }

    if req.head.kind == "hire-new" {
        if blank(&req.head.person_id) {
            req.head.person_id = format!("{}-head", req.department_id);
        }
        if req.head.title.as_deref().is_none_or(blank) {
            req.head.title = Some(format!("Head of {}", req.name));
        }
    } else if blank(&req.head.person_id) {
        return Err(invalid(
            "invalid-head",
            "head.personId is required for appoint-existing: it names a person who already exists"
                .to_string(),
        ));
    }

    let department_id = req.department_id.clone();
    for (index, member) in req.staff.iter_mut().enumerate() {
        let name = member.name.clone().unwrap_or_default();
        if blank(&member.person_id) {
            let local = slugify(&name);
            if local.is_empty() {
                return Err(invalid(
                    "invalid-staff-name",
                    format!("staff[{index}] has no personId and no name to derive one from"),
                ));
            }
            member.person_id = format!("{department_id}-{local}");
        }
        if member.title.as_deref().is_none_or(blank) {
            member.title = Some(name);
        }
        if member.person_kind.is_none() {
            member.person_kind = Some("worker".to_string());
        }
    }
    if req.head.person_kind.is_none() && req.head.kind == "hire-new" {
        req.head.person_kind = Some("head".to_string());
    }
    Ok(())
}

/// Fill in the identity, title and task class a hire may leave to chiefd
/// (#751/P3) — the single-person twin of [`mint_department_create_ids`].
///
/// `hire_person_authorized` refuses a blank `personId`, `title` or `taskClass`
/// as `invalid-seed`, and nothing filled them: the deleted CLI sent
/// `seed.title ?? ""` and `seed.taskClass ?? ""` verbatim, so a hire that named
/// no title was refused every time, and it minted `<department>-<slug(name)>`
/// client-side — a second opinion about what a person is called, exactly what
/// R3 removed from department create. Both belong here, applied with
/// `organization_spec`'s own rules, so a person hired through this route is
/// named and classified exactly as one created by genesis.
///
/// A blank field means "you decide"; a supplied one is honoured untouched and
/// still validated downstream.
fn mint_hire_ids(req: &mut OrgPersonHireRequest) -> Result<(), RouteError> {
    use chiefd_core::store::organization_spec::slugify;

    fn blank(value: &str) -> bool {
        value.trim().is_empty()
    }

    if blank(&req.person_id) {
        let local = slugify(&req.name);
        if local.is_empty() {
            return Err(RouteError::malformed(
                "invalid-person-name",
                format!("name {:?} produces no usable id, and no personId was given", req.name),
            ));
        }
        req.person_id = format!("{}-{local}", req.department_id);
    }
    if blank(&req.title) {
        req.title = req.name.trim().to_string();
    }
    Ok(())
}

fn parse_department_new_person(
    raw: OrgDepartmentHead,
    label: &str,
) -> Result<(String, chiefd_core::store::org_ops::OwnedNewPersonSeed), RouteError> {
    use chiefd_core::store::org_ops::OwnedNewPersonSeed;
    use chiefd_core::store::organization::{EmploymentState, PersonKind};

    if raw.kind != "hire-new" {
        return Err(RouteError::malformed(
            "unknown-kind",
            format!("{label}.kind must be hire-new"),
        ));
    }
    let (Some(name), Some(title), Some(mandate), Some(person_kind)) =
        (raw.name, raw.title, raw.mandate, raw.person_kind)
    else {
        return Err(RouteError::malformed(
            "invalid-seed",
            format!("{label} requires the complete normalized person seed"),
        ));
    };
    let kind = match person_kind.as_str() {
        "worker" => PersonKind::Worker,
        "head" => PersonKind::Head,
        "executive" => PersonKind::Executive,
        other => {
            return Err(RouteError::malformed(
                "unknown-kind",
                format!("{label}.personKind must be worker|head|executive, got {other}"),
            ));
        }
    };
    let employment_state = match raw.employment_state.as_deref().unwrap_or("active") {
        "active" => EmploymentState::Active,
        "benched" => EmploymentState::Benched,
        other => {
            return Err(RouteError::malformed(
                "unknown-employment-state",
                format!("{label}.employmentState must be active|benched, got {other}"),
            ));
        }
    };
    Ok((
        raw.person_id,
        OwnedNewPersonSeed {
            name,
            title,
            mandate,
            kind,
            employment_state,
            activation: raw.activation.unwrap_or_else(|| "resident".to_string()),
            tools: raw.tools,
            prompts: raw.prompts,
        },
    ))
}

async fn org_department_create(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgDepartmentCreateRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::{CreateDepartmentOutcome, DepartmentStaffSeed, HeadDecision};
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let mut req = req;
    mint_department_create_ids(&mut req)?;
    let unit = parse_department_create_unit(req.unit)?;
    let (requester_person_id, actor) = parse_staffing_requester(req.requester)?;
    bind_caller(&caller, requester_person_id.as_deref(), &req.slug)?;
    let head_kind = req.head.kind.clone();
    // Read ONCE and shared by the preflight projection and the writer call
    // below. The staged materialization must describe the same manifest the
    // transaction commits, and a decision applied to only one of them stages a
    // build that does not match what lands.
    let head_vacates: Option<chiefd_core::store::org_ops::HeadVacancy> =
        req.vacates.take().map(Into::into);
    // A hire-new head did not exist a moment ago and heads nothing, so a
    // vacancy decision cannot mean anything. REFUSED rather than ignored: a
    // request carrying a decision the server drops silently is one the caller
    // believes it made.
    if head_kind == "hire-new" && head_vacates.is_some() {
        return Err(RouteError::malformed(
            "vacancy-decision-invalid",
            "vacates says what becomes of a department the new head already leads, so it belongs \
             only with an appoint-existing head; a newly hired head leads nothing yet",
        ));
    }
    let head = match head_kind.as_str() {
        "appoint-existing" => HeadDecision::AppointExisting { person_id: req.head.person_id },
        "hire-new" => {
            let (person_id, seed) = parse_department_new_person(req.head, "head")?;
            HeadDecision::HireNew { person_id, seed: Box::new(seed) }
        }
        other => {
            return Err(RouteError::malformed(
                "unknown-head-kind",
                format!("head.kind must be appoint-existing|hire-new, got {other}"),
            ));
        }
    };
    let mut staff = Vec::with_capacity(req.staff.len());
    for (index, raw) in req.staff.into_iter().enumerate() {
        let (person_id, seed) = parse_department_new_person(raw, &format!("staff[{index}]"))?;
        staff.push(DepartmentStaffSeed { person_id, seed });
    }
    // TOMBSTONE: `without_redundant_baseline_skills`. It stripped the manager
    //   skill from a seed's declared list because the build copied that skill
    //   unconditionally anyway. Nothing is copied, so nothing is redundant.
    // Project and refuse without committing anything. The barrier is empty:
    //   there is no staged tree to promote, because a home is written after the
    //   commit rather than built before it.
    let barrier = {
        let manifest = current_manifest(&source)?;
        let preflight_head = head.clone();
        let preflight_staff: Vec<DepartmentStaffSeed> = staff
            .iter()
            .map(|member| DepartmentStaffSeed {
                person_id: member.person_id.clone(),
                seed: member.seed.clone(),
            })
            .collect();
        let projected = chiefd_core::store::org_projection::project_department_create(
            &manifest,
            &chiefd_core::store::org_projection::DepartmentCreateProposal {
                department_id: &req.department_id,
                parent_id: &req.parent_id,
                name: &req.name,
                purpose: &req.purpose,
                head: &preflight_head,
                staff: &preflight_staff,
                unit: &unit,
                requester_person_id: requester_person_id.as_deref(),
                audit_reason: &req.reason,
                at: &now_iso(),
                head_vacates: head_vacates.as_ref(),
            },
        );
        match projected {
            Ok(_projected) => PublishBarrier::none(),
            // The writer re-derives this same refusal from the same function;
            // surfacing it here only means nothing was written to reach it.
            Err(reason) => return Err(RouteError::refused(reason.code(), reason.detail())),
        }
    };
    let outcome = {
        if !staff.is_empty() && head_kind != "hire-new" {
            return Err(RouteError::malformed(
                "invalid-staff",
                "appoint-existing department creation accepts no new staff",
            ));
        }
        source
            .company
            .create_department_unit(
                req.department_id,
                req.parent_id,
                req.name,
                req.purpose,
                head,
                staff,
                unit,
                head_vacates,
                requester_person_id,
                req.reason,
                now_iso(),
                actor,
                barrier,
            )
            .await
    };
    match outcome {
        Ok(CreateDepartmentOutcome::Applied { department_id }) => {
            // A department create hires its head and its staff, so it creates
            // people and owes them homes for the same reason a hire does. It
            // also owes the department itself a `shared/departments/<id>`,
            // which only `materialize_person` ever creates.
            let warnings = materialize_after_commit(&source, now_iso()).await;
            wake_reconcile(&source);
            Ok(Json(
                serde_json::json!({"applied": true, "departmentId": department_id, "warnings": warnings}),
            ))
        }
        Ok(CreateDepartmentOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// --- org_ops atomic family: reparent_department (P1-d, the operator's reorg) -------
//
// Same shape as shutdown: own-company slug gate, dispatch into the `CompanyDb`
// wrapper (chiefd-core owns the single BEGIN IMMEDIATE). It deliberately has
// no caller revision input or stale-sequence outcome: 200 {applied:true,...} |
// 422 {code,detail} (policy refusal, never retryable) | 4xx/5xx transport.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgDepartmentReparentRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// The department to move.
    department_id: String,
    /// The new parent department.
    new_parent_id: String,
}

async fn org_department_reparent(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgDepartmentReparentRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::ReparentOutcome;
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    // WHO REORGANIZED. Same shape as `remove-tree`: this request has never
    // carried a requester field of any kind, so there is nothing for
    // `bind_caller` to reconcile and the caller's principal is the first
    // principal the route has ever had. Absent caller yields an empty actor and
    // the behaviour is unchanged, which is what lets this land ahead of
    // universal credentials; core enforces only once the actor names a person.
    let actor = caller.principal.clone();
    match source
        .company
        .reparent_department(req.department_id, req.new_parent_id, now_iso(), actor)
        .await
    {
        Ok(ReparentOutcome::Applied { department_id }) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"applied": true, "departmentId": department_id})))
        }
        Ok(ReparentOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// --- org_ops atomic family: transfer_person / move_department_members (P1-c)
//
// The H1 verbs. Both dispatch into `CompanyDb` (chiefd-core owns the single
// BEGIN IMMEDIATE). Both public contracts are revisionless: validation and
// writes run against the current rows inside the serialized company writer.

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgPersonTransferRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// The person to transfer.
    person_id: String,
    /// The destination department.
    destination_id: String,
    /// The originating intent id, stamped on any superseded transition.
    #[serde(default)]
    intent: Option<String>,
    /// ACCEPTED AND IGNORED. The actor recorded on the org-event run is the
    /// AUTHENTICATED caller's principal now, never this claim — see
    /// `caller_actor`. The field stays in the contract because
    /// `deny_unknown_fields` would otherwise 400 every client still sending it,
    /// and the underscore is what tells `dead_code` the silence is deliberate.
    #[serde(default, rename = "actor")]
    _actor: Option<String>,
    /// What becomes of the department this person heads, when they head one.
    #[serde(default)]
    vacates: Option<OrgHeadVacancy>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgDepartmentMoveMembersRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// The source department every listed person must be a member (home) of.
    from_department_id: String,
    /// The destination department.
    destination_id: String,
    /// The explicit set of members to move together (all-or-nothing). OMIT (or
    /// send empty) to move every ORDINARY member of the source — everybody
    /// assigned to it who is not its head and has not departed. That set is
    /// derived inside the transaction rather than by the caller, so it can
    /// neither be stale nor carry a second opinion about who counts as a
    /// member (#751/P3, the `mint_department_create_ids` rule).
    #[serde(default)]
    person_ids: Vec<String>,
    /// The originating intent id, stamped on any superseded transition + the
    /// staffing entries.
    #[serde(default)]
    intent: Option<String>,
}

/// Map a direct batch transfer to its committed placement or a policy refusal.
fn batch_transfer_response(
    outcome: Result<chiefd_core::store::org_ops::TransferOutcome, chiefd_core::error::ChiefdError>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::TransferOutcome;
    match outcome {
        Ok(TransferOutcome::Applied { moved }) => {
            Ok(Json(serde_json::json!({"applied": true, "moved": moved})))
        }
        Ok(TransferOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

/// Map a direct transfer to either its committed placement or an explicit
/// policy refusal. The company writer serializes the decision with the rows;
/// no stale-retry wire contract exists.
fn direct_transfer_response(
    outcome: Result<chiefd_core::store::org_ops::TransferOutcome, chiefd_core::error::ChiefdError>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::TransferOutcome;
    match outcome {
        Ok(TransferOutcome::Applied { moved }) => {
            Ok(Json(serde_json::json!({"applied": true, "moved": moved})))
        }
        Ok(TransferOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

async fn org_person_transfer(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonTransferRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    // WHO IS MOVING WHOM. `actor` arrived in the body and was recorded as-is,
    // so a placement change could name anybody as its author.
    //
    // THE CALLER'S PRINCIPAL WINS, AND `bind_caller` IS NOT USED — which was
    // tried first and is wrong here. `actor` is FREE-FORM AUDIT PROSE, not a
    // declared staffing requester: this corpus writes `operator`, `op` and the
    // empty string into it, and the field is optional. `bind_requester_to_caller`
    // reads an ABSENT declaration as a claim on the operator route, so binding
    // it refused every person-authenticated caller who simply omitted the
    // field — `operator-requester-forbidden` on an ordinary transfer. That is a
    // category error, not a tuning problem: the field never named a principal.
    //
    // Overwriting is the whole guarantee instead. Once a caller is present its
    // principal is what core authorizes and what the ledger records, so the
    // body value cannot be believed even when it disagrees; with no caller the
    // behaviour is unchanged, which is what lets this land before credentials
    // are universal.
    // The caller's principal, always — the extractor cannot hand back an absent
    // one. `req.actor` used to survive when no credential was presented, which
    // is exactly the caller-asserted audit prose this workstream exists to stop
    // trusting.
    let actor = caller.principal.clone();
    let outcome = source
        .company
        .transfer_person(
            req.person_id,
            req.destination_id,
            req.intent.unwrap_or_default(),
            now_iso(),
            actor,
            req.vacates.map(Into::into),
        )
        .await;
    if matches!(outcome, Ok(chiefd_core::store::org_ops::TransferOutcome::Applied { .. })) {
        wake_reconcile(&source);
    }
    direct_transfer_response(outcome)
}

async fn org_department_move_members(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgDepartmentMoveMembersRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let outcome = source
        .company
        .move_department_members(
            req.from_department_id,
            req.destination_id,
            req.person_ids,
            req.intent.unwrap_or_default(),
            now_iso(),
            caller_actor(&caller),
        )
        .await;
    if matches!(outcome, Ok(chiefd_core::store::org_ops::TransferOutcome::Applied { .. })) {
        wake_reconcile(&source);
    }
    batch_transfer_response(outcome)
}

// --- org_ops atomic family: offboard_person (member 3, P2, fire) ------------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgPersonOffboardRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// The person to offboard (fire).
    person_id: String,
    /// Stable caller identity recorded in the contiguous org-event run.
    /// Mirrors `OrgPersonTransferRequest.actor`.
    #[serde(default)]
    actor: Option<String>,
}

async fn org_person_offboard(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonOffboardRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::OffboardOutcome;
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    // WHO FIRED WHOM. `actor` arrived in the body, defaulted to the empty
    // string, and was recorded as-is — so the staffing ledger's record of a
    // firing named nobody. `hire` was bound and `offboard` was not, which the
    // authz audit called its sharpest asymmetry.
    //
    // Two things happen here, and they are separable on purpose. The BINDING
    // refuses a declared actor that is not the authenticated caller; it is a
    // no-op while no daemon sets credentials, which is what lets track B1 land
    // ahead of track A. The RECORD prefers the caller's principal over the
    // body, so once credentials exist the ledger cannot be told a name its
    // author did not have.
    bind_caller(&caller, req.actor.as_deref(), &req.slug)?;
    // The caller's principal, always — the extractor cannot hand back an absent
    // one. `req.actor` used to survive when no credential was presented, which
    // is exactly the caller-asserted audit prose this workstream exists to stop
    // trusting.
    let actor = caller.principal.clone();
    match source.company.offboard_person(req.person_id, now_iso(), actor).await {
        Ok(OffboardOutcome::Applied) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"applied": true})))
        }

        // Policy refusal → 422 with the machine code (family convention).
        Ok(OffboardOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// --- org_ops atomic family: hire_person (member 4, P2-f) --------------------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgPersonHireRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// Launcher-attested manager or explicit direct operator.
    requester: OrgStaffingRequester,
    /// The id of the person to create.
    person_id: String,
    /// The department to hire them into.
    department_id: String,
    /// Display name.
    name: String,
    /// Role title.
    title: String,
    /// The person's mandate.
    mandate: String,
    /// `worker` (default) | `head` | `executive`.
    #[serde(default)]
    kind: Option<String>,
    /// active / benched.
    #[serde(default)]
    employment_state: Option<String>,
    /// `resident` (default) | `on-demand`.
    #[serde(default)]
    activation: Option<String>,
    /// Tool grants.
    ///
    /// TOMBSTONE (chief-home-is-cwd §4e): `skills`, `extensions` and `packages`
    /// stood here. A hire selects no Pi resource — the skills an agent has are
    /// whatever is in `<dir>/.pi/skills` when Pi looks — and
    /// `deny_unknown_fields` makes a caller that still sends one a 400 rather
    /// than a selection quietly dropped on the floor.
    #[serde(default)]
    tools: Vec<String>,
    /// Prompt template child rows.
    #[serde(default)]
    prompts: Vec<String>,
}

/// Hold an authenticated caller to the requester it declared.
///
/// Thin adapter over [`super::caller_auth::bind_requester_to_caller`] so the
/// four staffing handlers all bind the same way — the attestation this replaces
/// lived in ONE TypeScript client, which is exactly how it came to be
/// bypassable by every other caller of these routes.
fn bind_caller(
    caller: &chiefd_core::store::identities::Identity,
    declared_person_id: Option<&str>,
    company_slug: &str,
) -> Result<(), RouteError> {
    super::caller_auth::bind_requester_to_caller(caller, declared_person_id, company_slug)
}

/// The audit actor a route records, taken from the AUTHENTICATED CALLER.
///
/// The staffing and structure family recorded `String::new()`, so the ledger's
/// account of who shut somebody down, benched them, moved them or replaced
/// their head named nobody, and core had no principal to authorize against.
///
/// This deliberately reads the extractor and nothing else. Most of these
/// requests declare no requester at all, and adding a body field so there were
/// something to `bind_caller` would manufacture a second `requestedBy` — a
/// value that reads as bound and is supplied by the caller it claims to
/// authenticate. Where a body field ALREADY exists (`transfer`'s `actor`), the
/// route binds it with [`bind_caller`] and still prefers this value.
///
/// There is no absent caller: the extractor hands every handler a proven
/// identity, so the actor a route records always names the principal the
/// daemon authenticated.
pub(crate) fn caller_actor(caller: &chiefd_core::store::identities::Identity) -> String {
    caller.principal.clone()
}

/// [`super::caller_auth::bind_caller_to_company`] over the extractor's
/// extension — the binding for a route that names no requester and no person
/// target, only a company.
fn bind_caller_company(
    caller: &chiefd_core::store::identities::Identity,
    company_slug: &str,
) -> Result<(), RouteError> {
    super::caller_auth::bind_caller_to_company(caller, company_slug)
}

/// The person a control-authority decision must be made about, together with
/// the manifest to decide it against — or `None`, meaning "allow, there is no
/// scope question here".
///
/// # It keys on the PRINCIPAL, not on the kind
///
/// The obvious gate is "a non-person identity is daemon-scoped, allow it", and
/// it is WRONG in one specific way. `identities`' own schema comment says two
/// identities may share one principal, and the `channel` kind exists for
/// daemon-terminated inbound channels — `operator-pane`, and `pi-pane`, which is
/// a PERSON's pane. Every channel row in the tree today carries the operator's
/// principal, so the hole is not reachable yet; the moment a pi-pane channel is
/// attested for a person, a kind-keyed gate would hand that person unconditional
/// company-wide scope, and attesting a channel would become the way to widen a
/// head into the whole company. So the question asked here is "does this
/// principal NAME A PERSON ROW", never "what kind of credential is it".
///
/// The arms, in order:
///
/// * A PERSON identity of another company — refused outright. A person identity
///   is company-scoped and must never act on a company it does not belong to.
///   (Daemon-scoped kinds carry a NULL slug by schema CHECK, so this cannot
///   fire for them.)
/// * No committed manifest — the company has no hierarchy, so a person cannot
///   be proved to manage anything in it and is refused; a caller that names no
///   person keeps its unconditional scope, which is what lets the operator
///   client act on a company between creation and genesis.
/// * A principal naming NO person row, on a non-person credential — the
///   operator, the actuator, a channel of the operator. This is
///   [`ControlActor::Operator`](chiefd_core::store::control_authority::ControlActor::Operator),
///   which `control_authority` defines as unconditional scope, and it is what
///   keeps `chief-cli` (which posts `runtime/clear` and `launch-intent/clear`
///   from Rust) working.
/// * Anything else — a person credential, or a channel attested AS a person —
///   is handed back to be scope-checked as that person.
async fn caller_scope_actor<'a>(
    caller: &'a chiefd_core::store::identities::Identity,
    source: &SupervisionLiveSource,
) -> Result<Option<(&'a str, chiefd_core::store::organization::OrganizationManifest)>, RouteError> {
    use chiefd_core::store::identities::IdentityKind;
    let company_slug = &source.org_documents_slug;
    let is_person_credential = caller.kind == IdentityKind::Person;
    if is_person_credential
        && caller.company_slug.as_deref().is_some_and(|slug| slug != company_slug.as_str())
    {
        return Err(RouteError::forbidden(
            "caller-company-mismatch",
            format!(
                "caller '{}' belongs to a different company than '{company_slug}'",
                caller.principal
            ),
        ));
    }
    let manifest = source
        .company
        .org_manifest_read()
        .await
        .map_err(|error| company_error(&error))?
        .map(|(manifest, _seq)| manifest);
    let principal = caller.principal.as_str();
    let Some(manifest) = manifest else {
        return if is_person_credential {
            Err(RouteError::forbidden(
                "caller-out-of-company-scope",
                "this company has no committed organization manifest, so no person's control \
                 authority over it can be proved",
            ))
        } else {
            Ok(None)
        };
    };
    if !is_person_credential && !manifest.people.contains_key(principal) {
        return Ok(None);
    }
    Ok(Some((principal, manifest)))
}

/// Refuse a caller that does not control the WHOLE COMPANY.
///
/// The subject of these routes is the company: they clear its launch intent,
/// drop or overwrite its runtime row, or commit CEO-only boot intent for it.
/// There is no person and no department in the body to scope against, so the
/// department the request acts on is the ROOT department, and the question is
/// the ordinary subtree one — `department_is_in_scope(manifest, caller, root)`.
/// Only somebody who heads the root passes it, which today is the CEO.
///
/// This is the SUBTREE rule, not a role gate. Nothing here reads a job title,
/// asks whether the caller is a manager, or consults any protected region; it
/// asks the same question `/v1/org/control-authority/department-in-scope`
/// answers, about the department the write actually reaches.
///
/// There is deliberately NO `bind_caller`. None of these requests declares a
/// requester, and adding a body field so one could be bound would manufacture
/// exactly the caller-supplied-value-compared-against-a-caller-supplied-value
/// shape that `MaintenanceStartRequest.identity` is.
///
/// # Errors
/// `403 caller-company-mismatch` for a person of another company;
/// `403 caller-out-of-company-scope` for a person who does not head the root.
pub(super) async fn require_company_wide_authority(
    caller: &chiefd_core::store::identities::Identity,
    source: &SupervisionLiveSource,
    verb: &str,
) -> Result<(), RouteError> {
    use chiefd_core::store::control_authority::{department_is_in_scope, ControlActor};
    let Some((principal, manifest)) = caller_scope_actor(caller, source).await? else {
        return Ok(());
    };
    let root = manifest.root_department_id.clone();
    // `caller_scope_actor` has already returned `None` for every daemon-scoped
    // principal, which is the operator arm of `ControlActor` and passes this
    // fence unconditionally. What survives to here always names a person.
    let actor = ControlActor::Person(principal.to_owned());
    if department_is_in_scope(&manifest, &actor, &root) {
        return Ok(());
    }
    Err(RouteError::forbidden(
        "caller-out-of-company-scope",
        format!(
            "caller '{principal}' does not head '{root}', so it may not {verb} for the whole \
             company; this write reaches every person in it"
        ),
    ))
}

/// Refuse a caller that does not manage every person the request NAMES.
///
/// Used where the body carries person targets rather than acting company-wide.
/// The predicate is `person_is_in_scope`, which resolves each target to its home
/// department and asks the same subtree question — self always, otherwise the
/// caller must head a unit above the target's home.
///
/// # Errors
/// `403 caller-company-mismatch` for a person of another company;
/// `403 caller-out-of-scope`, naming the first target out of reach.
async fn require_person_scope(
    caller: &chiefd_core::store::identities::Identity,
    source: &SupervisionLiveSource,
    target_person_ids: &[String],
) -> Result<(), RouteError> {
    use chiefd_core::store::control_authority::{person_is_in_scope, ControlActor};
    if target_person_ids.is_empty() {
        return Ok(());
    }
    let Some((principal, manifest)) = caller_scope_actor(caller, source).await? else {
        return Ok(());
    };
    let actor = ControlActor::Person(principal.to_owned());
    for target in target_person_ids {
        if !person_is_in_scope(&manifest, &actor, target) {
            return Err(RouteError::forbidden(
                "caller-out-of-scope",
                format!("caller '{principal}' does not manage '{target}'"),
            ));
        }
    }
    Ok(())
}

/// Refuse a caller that does not control the department the request names.
///
/// This is the typed route fence for structural verbs whose target is a
/// department. It first applies the caller's company binding in
/// [`caller_scope_actor`], then asks the one organization rule:
/// [`department_is_in_scope`]. Daemon-scoped principals that name no person
/// retain operator authority. A person identity can never gain that authority
/// by using a principal absent from the target company's roster.
async fn require_department_scope(
    caller: &chiefd_core::store::identities::Identity,
    source: &SupervisionLiveSource,
    department_id: &str,
) -> Result<(), RouteError> {
    use chiefd_core::store::control_authority::{department_is_in_scope, ControlActor};
    let Some((principal, manifest)) = caller_scope_actor(caller, source).await? else {
        return Ok(());
    };
    // Existence is a product-state question, not an authorization result. Let
    // the transaction return its established `unknown-department` refusal so
    // a missing id never reads as an identity denial. Company binding has
    // already run above, so this does not let a cross-company caller probe it.
    if !manifest.departments.contains_key(department_id) {
        return Ok(());
    }
    let actor = ControlActor::Person(principal.to_owned());
    if department_is_in_scope(&manifest, &actor, department_id) {
        return Ok(());
    }
    Err(RouteError::forbidden(
        "caller-out-of-scope",
        format!("caller '{principal}' does not manage department '{department_id}'"),
    ))
}

async fn org_person_hire(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonHireRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::{HireOutcome, OwnedNewPersonSeed};
    use chiefd_core::store::organization::PersonKind;
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let mut req = req;
    mint_hire_ids(&mut req)?;
    let (requester_person_id, actor) = parse_staffing_requester(req.requester)?;
    bind_caller(&caller, requester_person_id.as_deref(), &req.slug)?;
    let kind = match req.kind.as_deref() {
        None | Some("worker") => PersonKind::Worker,
        Some("head") => PersonKind::Head,
        Some("executive") => PersonKind::Executive,
        Some(other) => {
            return Err(RouteError::malformed(
                "unknown-kind",
                format!("kind must be worker|head|executive, got {other}"),
            ));
        }
    };
    let employment_state = match req.employment_state.as_deref().unwrap_or("active") {
        "active" => chiefd_core::store::organization::EmploymentState::Active,
        "benched" => chiefd_core::store::organization::EmploymentState::Benched,
        other => {
            return Err(RouteError::malformed(
                "unknown-employment-state",
                format!("employmentState must be active|benched, got {other}"),
            ));
        }
    };
    let seed = OwnedNewPersonSeed {
        name: req.name,
        title: req.title,
        mandate: req.mandate,
        kind,
        employment_state,
        activation: req.activation.unwrap_or_else(|| "resident".to_string()),
        tools: req.tools,
        prompts: req.prompts,
    };
    // Project and refuse with NOTHING committed and the person id still free.
    // The barrier is empty: there is no staged tree to promote, because a home
    // is written after the commit rather than built before it.
    let barrier = {
        let manifest = current_manifest(&source)?;
        let projected = chiefd_core::store::org_projection::project_hire(
            &manifest,
            &chiefd_core::store::org_projection::HireProposal {
                person_id: &req.person_id,
                department_id: &req.department_id,
                seed: &seed.as_ref(),
                requester_person_id: requester_person_id.as_deref(),
                at: &now_iso(),
            },
        );
        match projected {
            Ok(_projected) => PublishBarrier::none(),
            Err(reason) => return Err(RouteError::refused(reason.code(), reason.detail())),
        }
    };
    match source
        .company
        .hire_person(
            req.person_id,
            req.department_id,
            seed,
            requester_person_id,
            now_iso(),
            actor,
            barrier,
        )
        .await
    {
        Ok(HireOutcome::Applied) => {
            // MATERIALIZE, then wake. A hire that only writes rows produces a
            // person the actuator can never spawn — see
            // [`ensure_committed_agent_homes`]. The row is already committed,
            // so this reports rather than fails ([`materialize_after_commit`]).
            let warnings = materialize_after_commit(&source, now_iso()).await;
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({ "applied": true, "warnings": warnings })))
        }

        // Policy refusal → 422 with the machine code (family convention).
        Ok(HireOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// --- org_ops atomic family: pause_department / resume_department (P2-h) ------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgDepartmentPauseRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// The department to pause / resume.
    department_id: String,
}

async fn org_department_pause(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgDepartmentPauseRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let outcome =
        source.company.pause_department(req.department_id, now_iso(), caller_actor(&caller)).await;
    if matches!(outcome, Ok(chiefd_core::store::org_ops::PauseOutcome::Applied)) {
        wake_reconcile(&source);
    }
    pause_response(outcome)
}

async fn org_department_resume(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgDepartmentPauseRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let outcome =
        source.company.resume_department(req.department_id, now_iso(), caller_actor(&caller)).await;
    if matches!(outcome, Ok(chiefd_core::store::org_ops::PauseOutcome::Applied)) {
        wake_reconcile(&source);
    }
    pause_response(outcome)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgDepartmentResumeManyRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// Departments to resume as one all-or-nothing direct operation.
    department_ids: Vec<String>,
    /// Treat an already-active unit as satisfied; used by convergence callers.
    #[serde(default)]
    skip_active: bool,
}

async fn org_department_resume_many(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgDepartmentResumeManyRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let outcome = source
        .company
        .resume_departments(req.department_ids, req.skip_active, now_iso(), caller_actor(&caller))
        .await;
    if matches!(outcome, Ok(chiefd_core::store::org_ops::PauseOutcome::Applied)) {
        wake_reconcile(&source);
    }
    pause_response(outcome)
}

/// Map a [`PauseOutcome`] onto the family HTTP convention (shared by pause and
/// resume): Applied → 200 `{applied:true}`, Refused → 422 `{code,detail}`
/// (kebab), transport → 5xx.
fn pause_response(
    outcome: Result<chiefd_core::store::org_ops::PauseOutcome, chiefd_core::error::ChiefdError>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::PauseOutcome;
    match outcome {
        Ok(PauseOutcome::Applied) => Ok(Json(serde_json::json!({"applied": true}))),
        // Policy refusal → 422 with the machine code (family convention).
        Ok(PauseOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// --- org_ops atomic family: bench_person (member 4, P2, durable idle) --------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgPersonBenchRequest {
    /// The own-company documentKey; a foreign slug is `unknown-company` (404).
    slug: String,
    /// The person to bench (durable idle; reversible via recall).
    person_id: String,
}

async fn org_person_bench(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonBenchRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::BenchOutcome;
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    // Malformed input → 400 (a blank person_id is not a policy refusal).
    if req.person_id.trim().is_empty() {
        return Err(RouteError::malformed(
            "missing-person-id",
            "bench requires a non-empty personId",
        ));
    }
    match source.company.bench_person(req.person_id, now_iso(), caller_actor(&caller)).await {
        Ok(BenchOutcome::Applied) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"applied": true})))
        }

        // Policy refusal → 422 with the machine code (family convention).
        Ok(BenchOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

/// Rust-owned reflected lifecycle for a manager's durable bench request.
async fn org_person_bench_lifecycle(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonBenchRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::BenchLifecycleOutcome;
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    if req.person_id.trim().is_empty() {
        return Err(RouteError::malformed(
            "missing-person-id",
            "bench requires a non-empty personId",
        ));
    }
    match source
        .company
        .bench_person_lifecycle(req.person_id, now_iso(), caller_actor(&caller))
        .await
    {
        // No completion key: no transition was written, so there is no pane
        // whose disappearance could be observed. The bench committed; waiting
        // on a proof that cannot arrive would report it as a convergence
        // timeout (#751/P3).
        Ok(BenchLifecycleOutcome::Applied { completion: None }) => {
            wake_reconcile(&source);
            Ok(Json(
                serde_json::json!({"applied": true, "structuralChanged": true, "handoff": "abandoned"}),
            ))
        }
        Ok(BenchLifecycleOutcome::Applied { completion: Some(key) }) => {
            let Some(registry) = source.bench_completion.as_ref() else {
                wake_reconcile(&source);
                return Err(RouteError::unavailable(
                    "bench-convergence-timeout",
                    "bench committed but no live Rust convergence acknowledgement is available",
                ));
            };
            // Registration is deliberately post-commit: the writer method has
            // returned and released its transaction before this in-memory wait
            // exists. The key stays internal; the HTTP success remains the
            // data-free structural response established by #717.
            let completion = registry.register(key.clone());
            wake_reconcile(&source);
            if !matches!(
                tokio::time::timeout(BENCH_COMPLETION_TIMEOUT, completion).await,
                Ok(Ok(()))
            ) {
                registry.cancel(&key);
                return Err(RouteError::unavailable(
                    "bench-convergence-timeout",
                    "bench committed but Rust convergence did not confirm the tagged pane stopped",
                ));
            }
            Ok(Json(
                serde_json::json!({"applied": true, "structuralChanged": true, "handoff": "completed"}),
            ))
        }
        Ok(BenchLifecycleOutcome::Refused { reason }) => {
            Err(RouteError::refused(reason.code(), reason.detail()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

// --- revisionless lifecycle / runtime-preference operations -----------------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgPersonRecallRequest {
    slug: String,
    person_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgPersonStartRequest {
    slug: String,
    person_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgPersonReplaceHeadAndOffboardRequest {
    slug: String,
    head_person_id: String,
    successor_person_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgDepartmentReactivateExecutiveRootRequest {
    slug: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgDepartmentRemoveTreeRequest {
    slug: String,
    department_id: String,
}

fn direct_org_response(
    outcome: Result<chiefd_core::store::org_ops::DirectOutcome, chiefd_core::error::ChiefdError>,
) -> Result<Json<serde_json::Value>, RouteError> {
    match outcome {
        Ok(chiefd_core::store::org_ops::DirectOutcome::Applied) => {
            Ok(Json(serde_json::json!({"applied": true})))
        }
        Ok(chiefd_core::store::org_ops::DirectOutcome::Refused { code, detail }) => {
            Err(RouteError::refused(code, detail))
        }
        Err(other) => Err(company_error(&other)),
    }
}

async fn org_person_recall(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonRecallRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let outcome =
        source.company.recall_person(req.person_id, now_iso(), caller_actor(&caller)).await;
    if matches!(outcome, Ok(chiefd_core::store::org_ops::DirectOutcome::Applied)) {
        wake_reconcile(&source);
    }
    direct_org_response(outcome)
}

async fn org_person_start(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonStartRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    // Repair first, THEN answer. Starting somebody is a promise that a pane is
    // coming, and this route used to make that promise without ever asking
    // whether it could be kept: it committed `active`, a launch fence and
    // durable demand, returned `{"applied": true}` — rendered to the CEO as
    // `✅ Started @<id> · only this person was launched` — and left the
    // actuator to discover on its next pass that the person had no home. The
    // roster then showed `active · recovering · no live pane observed` for
    // people who would never converge, and the real cause existed only in a
    // chiefd log line.
    //
    // Writing homes here also repairs a person hired before hire itself did so,
    // and one whose home the user deleted by hand — an already-broken company
    // heals on the next start rather than staying unbootable.
    let warnings = ensure_committed_agent_homes(&source, now_iso()).await?;
    if let Some(reason) = launch_refusal_for(&source, &req.person_id) {
        // The materialization warnings travel IN the detail rather than as a
        // third body field: the refusal body is closed at `{code, detail}`, and
        // a warning nobody could read was worth less than a sentence the agent
        // is actually shown.
        let context = if warnings.is_empty() {
            String::new()
        } else {
            format!(" Writing the agent homes also warned: {}.", warnings.join("; "))
        };
        return Err(RouteError::refused(
            "person-not-launchable",
            format!(
                "'{}' cannot be started: {reason}. Nothing was written; the roster is \
                 unchanged.{context}",
                req.person_id
            ),
        ));
    }
    let outcome =
        source.company.start_person(req.person_id, now_iso(), caller_actor(&caller)).await;
    if matches!(outcome, Ok(chiefd_core::store::org_ops::DirectOutcome::Applied)) {
        wake_reconcile(&source);
    }
    direct_org_response(outcome)
}

/// `POST /v1/org/person/wake`.
///
/// `deny_unknown_fields` APPLIES, like every other person-target request
/// struct in this family (`OrgPersonStartRequest`, `OrgPersonRecallRequest`,
/// `OrgPersonReplaceHeadAndOffboardRequest`). The rule those follow is the
/// wire contract's, not a per-route taste: a field this daemon does not model
/// is a caller believing something about the verb that is not true, and
/// accepting it silently is how a newer client's option gets dropped without
/// anybody finding out. The two request structs nearby that OMIT it —
/// `OrgClearRequest` and the mailbox list request — are the exceptions, and
/// neither carries a person.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgPersonWakeRequest {
    slug: String,
    person_id: String,
}

/// Wake one parked person, for the operator who pointed at them.
///
/// # The authority is the SUBTREE, and there is no role gate
///
/// `require_person_scope` asks the one legitimate question — does the caller
/// MANAGE this person — which resolves the target to their home department and
/// walks up. The operator's own bearer names no person row, so
/// `caller_scope_actor` hands it the unconditional `ControlActor::Operator`
/// scope and it passes; a head calling this reaches exactly their own subtree,
/// which is correct and is the same fence every structural verb in this file
/// takes. Nothing here reads a job title. `wake_person` asks the identical
/// question a second time inside the transaction (`actor_out_of_scope`),
/// exactly as `start_person` does, so the fence cannot be lost by a future
/// caller that reaches the op another way.
///
/// # Both directions are logged
///
/// A wake that worked and a wake that was refused are equally invisible from
/// the rail — the operator sees a pane appear, or they do not — so the daemon
/// says which happened and why. That matches the rail's own
/// `sidebar.wake.unavailable` / `sidebar.click` pattern on the other side of
/// the wire.
async fn org_person_wake(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonWakeRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let person_id = req.person_id.clone();
    if let Err(refusal) =
        require_person_scope(&caller, &source, std::slice::from_ref(&req.person_id)).await
    {
        tracing::info!(
            event = "org.person.wake.refused",
            company = %source.org_documents_slug,
            caller = %caller.principal,
            person = %person_id,
            reason = "caller-out-of-scope",
            "a wake was refused: the caller does not manage that person"
        );
        return Err(refusal);
    }
    let outcome = source.company.wake_person(req.person_id, now_iso(), caller_actor(&caller)).await;
    match &outcome {
        Ok(chiefd_core::store::org_ops::DirectOutcome::Applied) => {
            tracing::info!(
                event = "org.person.wake.applied",
                company = %source.org_documents_slug,
                caller = %caller.principal,
                person = %person_id,
                "launch intent granted and the lapsed idle park released; the next pass \
                 brings them up"
            );
        }
        Ok(chiefd_core::store::org_ops::DirectOutcome::Refused { code, .. }) => {
            tracing::info!(
                event = "org.person.wake.refused",
                company = %source.org_documents_slug,
                caller = %caller.principal,
                person = %person_id,
                reason = code,
                "a wake was refused"
            );
        }
        Err(error) => {
            tracing::warn!(
                event = "org.person.wake.failed",
                company = %source.org_documents_slug,
                person = %person_id,
                %error,
                "a wake could not be committed"
            );
        }
    }
    if matches!(outcome, Ok(chiefd_core::store::org_ops::DirectOutcome::Applied)) {
        wake_reconcile(&source);
    }
    direct_org_response(outcome)
}

async fn org_person_replace_head_and_offboard(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgPersonReplaceHeadAndOffboardRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let outcome = source
        .company
        .replace_head_and_offboard(
            req.head_person_id,
            req.successor_person_id,
            now_iso(),
            caller_actor(&caller),
        )
        .await;
    if matches!(outcome, Ok(chiefd_core::store::org_ops::DirectOutcome::Applied)) {
        wake_reconcile(&source);
    }
    direct_org_response(outcome)
}

async fn org_department_reactivate_executive_root(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgDepartmentReactivateExecutiveRootRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let outcome = source.company.reactivate_executive_root(now_iso(), caller_actor(&caller)).await;
    if matches!(outcome, Ok(chiefd_core::store::org_ops::DirectOutcome::Applied)) {
        wake_reconcile(&source);
    }
    direct_org_response(outcome)
}

async fn org_department_remove_tree(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<OrgDepartmentRemoveTreeRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::org_ops::RemoveDepartmentOutcome;
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    if let Err(refusal) = require_department_scope(&caller, &source, &req.department_id).await {
        tracing::warn!(
            event = "org.department.remove_tree.refused",
            company = %source.org_documents_slug,
            caller = %caller.principal,
            department = %req.department_id,
            code = refusal.code(),
            "a department-tree removal was refused by caller scope"
        );
        return Err(refusal);
    }
    // WHO DELETED THE SUBTREE. This route passed `String::new()`, so the most
    // destructive verb in the crate recorded its author as the empty string and
    // core had nothing to authorize against.
    //
    // There is NO `bind_caller` here and that is deliberate, not an omission.
    // `bind_caller` reconciles a requester DECLARED IN THE BODY with the
    // authenticated one, and this request has never had such a field — no
    // `actor`, no `requester`, no identity. So the caller's principal is the
    // first and only principal this route has ever carried, and adding a body
    // field to bind would manufacture exactly the second `requested_by` the
    // plan warns about: a value that reads as bound and is supplied by the
    // caller it claims to authenticate.
    //
    // The `Caller` extractor makes absence unrepresentable. The typed route
    // fence above authenticates company membership and subtree authority;
    // core receives the same authenticated principal and keeps its existing
    // actor fence as defense in depth.
    let actor = caller_actor(&caller);
    let department_id = req.department_id;
    match source.company.remove_department_tree(department_id.clone(), now_iso(), actor).await {
        Ok(RemoveDepartmentOutcome::Applied { removed_department_ids, departed_person_ids }) => {
            tracing::info!(
                event = "org.department.remove_tree.applied",
                company = %source.org_documents_slug,
                caller = %caller.principal,
                department = %department_id,
                removed_departments = ?removed_department_ids,
                departed_people = ?departed_person_ids,
                "a department tree was removed"
            );
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({
                "applied": true,
                "removedDepartmentIds": removed_department_ids,
                "departedPersonIds": departed_person_ids,
            })))
        }
        Ok(RemoveDepartmentOutcome::Refused { code, detail }) => {
            tracing::warn!(
                event = "org.department.remove_tree.refused",
                company = %source.org_documents_slug,
                caller = %caller.principal,
                department = %department_id,
                code,
                "a department-tree removal was refused by a product rule"
            );
            Err(RouteError::refused(code, detail))
        }
        Err(other) => {
            tracing::warn!(
                event = "org.department.remove_tree.failed",
                company = %source.org_documents_slug,
                caller = %caller.principal,
                department = %department_id,
                error = %other,
                "a department-tree removal could not be committed"
            );
            Err(company_error(&other))
        }
    }
}

// ---- health --------------------------------------------------------------

async fn health(
    State(store): State<Arc<DocStore>>,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let m = store.engine().metrics();
    // #1207: the one fact chiefd holds about whether ANYBODY is converging this
    // company, put on the body `chief ls` already fetches.
    //
    // chiefd still never spawns and never rebuilds — the 2026-08-18 ruling, and
    // a pane verb in chiefd is banned by name. It REPORTS, and the CLI is the
    // consumer: an actuator whose supervisor is alive over a child that cannot
    // get up is `Running` to tmux and silent to chiefd, and only the two facts
    // together name it. A DURATION and never a timestamp, because the client's
    // clock is not this one.
    //
    // The verdict is the SERVER's: `chief-cli` must not depend on
    // `chiefd-core`, so `ACTUATOR_LAPSE_MS` is applied here rather than shipped
    // for the client to re-derive.
    let attendance = supervision_live.as_ref().map(|source| {
        let now = source.clock_now();
        (source.actuator_attendance().silent_ms(now), source.actuator_attendance().attended(now))
    });
    let body = |status: &str| {
        let mut value = serde_json::json!({
            "status": status,
            // #H.6: the daemon's release version, so `chief upgrade` can name
            // the companies still running an older build after a swap, and a
            // client can refuse to drive a daemon whose major/minor differs.
            // Stamped by this crate's `build.rs` from the same
            // `CHIEF_RELEASE_VERSION` the daemon binary uses.
            "version": env!("CHIEF_VERSION"),
            "uptime_secs": store.engine().uptime_secs(),
            "writes_total": m.writes_total.load(Ordering::Relaxed),
            "queries_total": m.queries_total.load(Ordering::Relaxed),
            "busy_retries": m.busy_retries.load(Ordering::Relaxed),
        });
        if let Some((silent_ms, attended)) = attendance {
            value["actuatorSilentMs"] = silent_ms.into();
            value["actuatorAttended"] = attended.into();
        }
        value
    };
    match store.health_probe().await {
        Ok(()) => (super::route_error::HEALTH_SERVING, Json(body("ok"))),
        Err(cause) => (super::route_error::HEALTH_NOT_SERVING, Json(body(&cause))),
    }
}

#[derive(Serialize)]
struct OkBody {
    ok: bool,
}

async fn ensure_schema(State(store): State<Arc<DocStore>>) -> Result<Json<OkBody>, RouteError> {
    store.ensure_schema().await.map_err(|e| store_error(&e))?;
    Ok(Json(OkBody { ok: true }))
}

// ---- watch (#259, SSE-B; wire contract owned by this ticket per plan
// the design record §B and the design doc the design record) --
//
// `GET /v1/docs/watch?slug=<slug>&stores=<csv>[&after=<seq>]` — the documented
// wire contract every TS subscriber (SSE-C1/C2/D1/D2, #261-264) builds
// against:
//
// - Response is `text/event-stream`. Two event kinds:
//   - `event: doc-change`, `id: <seq>`, `data: <JSON WatchEvent>` — the SAME
//     shape `docstore::feed::WatchEvent` derives via `#[derive(Serialize)]`,
//     UNCHANGED field names (snake_case: `seq`, `slug`, `store`,
//     `updated_at`, `removed`) — this is a deliberate exception
//     to this router's usual camelCase convention (see the module doc):
//     the plan's own documented event data is `{slug, store,
//     updated_at}`, already snake_case, so leaving `WatchEvent`'s plain serde
//     names alone is what MATCHES the locked spec rather than departing from
//     it. `seq` is included in `data` (not only the `id:` line) so a
//     hand-rolled stream parser never needs to correlate the two. A
//     `removed: true` event (from `drop_company`/`drop_company_store`)
//     carries `updated_at: ""` — see `feed.rs`'s module
//     doc for why — and `store: "*"` on such an event means "every store for
//     this slug is gone" (from a whole-company `drop_company`); a client
//     filtering by `stores=` must treat that as matching every store it
//     subscribed to for the slug.
//   - `event: reorg`, `data: {}` (no `id:` — a reorg carries no seq) — sent
//     once whenever [`Replay::Gap`] fires: `after`/`Last-Event-ID` is either
//     from a prior process epoch (chiefd restarted) or names a seq whose
//     immediate successor was evicted from the change-feed's ring, OR a live
//     subscriber lagged the broadcast channel badly enough to lose buffered
//     events. Either way the client's own docs are stale in a way this feed
//     cannot repair — it MUST resync by re-reading every store it cares
//     about via the normal `/v1/docs/read` route, then keep consuming the
//     stream (the connection is not closed; live events keep flowing after a
//     `reorg`).
// - Filter: exact `slug` match plus the `stores` CSV (e.g.
//   `mailbox/alice,supervision`). A selector ending in `/` is a store-name
//   prefix (for example, `mailbox/` matches every dynamic mailbox person);
//   other selectors are exact. `stores=*` is a debug-only wildcard matching
//   every store for the slug. `stores` is REQUIRED (per the plan's
//   own URL template, only `after` is bracketed optional) — an axum `Query`
//   rejection (400) is the answer to an omitted one, same as every other
//   route's automatic body-shape rejection.
// - Replay: `Last-Event-ID` header takes priority over `after=` (a real
//   browser `EventSource` reconnect sends the header automatically; `after`
//   is for a first connect or a non-browser client). Absent both, `after` is
//   treated as `0`, which [`ChangeFeed::replay_from`] defines as "everything
//   currently retained" — never a gap — so a brand-new subscriber gets the
//   ring's current backlog for its slug/stores before going live, not just
//   the live tail.
// - Heartbeat: a `:hb` comment line every 15s in production
//   ([`WATCH_HEARTBEAT_INTERVAL`]) via axum's own `KeepAlive`, so a dead TCP
//   connection is distinguishable from quiet state.
// - Disconnect/lag safety: dropping the response stream (client disconnect)
//   drops the `broadcast::Receiver`, which unsubscribes automatically — no
//   explicit cleanup needed. A lagging receiver never blocks the writer
//   (`ChangeFeed::publish`'s contract) and is turned into a `reorg`, not a
//   silently-dropped run of events.

/// Query parameters for `GET /v1/docs/watch`. `stores` is a CSV of exact store
/// names and/or trailing-slash store-name prefixes, or the literal `*`
/// (debug-only wildcard, all stores for `slug`).
/// `after` is the CSV — er, the seq — after which to replay; superseded by a
/// `Last-Event-ID` header when the client (a real `EventSource`) sends one.
#[derive(Debug, Deserialize)]
struct WatchQuery {
    slug: String,
    stores: String,
    #[serde(default)]
    after: Option<u64>,
}

/// The parsed `stores` filter.
#[derive(Debug, Clone)]
enum StoreFilter {
    /// `stores=*` — every store for the slug (debug only).
    All,
    /// The CSV, parsed and trimmed. A selector ending in `/` intentionally
    /// names a dynamic store family, while every other selector stays exact.
    Selectors { exact: HashSet<String>, prefixes: HashSet<String> },
}

/// Parse the raw `stores` query value. A trailing slash selects a store-name
/// prefix; all other non-empty values remain exact selectors. An
/// empty/whitespace-only CSV (e.g.
/// `stores=` or `stores=,,`) degrades to [`StoreFilter::All`] rather than a
/// filter that can never match anything — a client is far more likely to
/// have meant "everything" than "nothing" with an empty list, and a filter
/// that matches nothing would silently look like a healthy-but-quiet
/// subscription forever.
fn parse_store_filter(raw: &str) -> StoreFilter {
    if raw.trim() == "*" {
        return StoreFilter::All;
    }
    let mut exact = HashSet::new();
    let mut prefixes = HashSet::new();
    for selector in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if selector.ends_with('/') {
            prefixes.insert(selector.to_string());
        } else {
            exact.insert(selector.to_string());
        }
    }
    if exact.is_empty() && prefixes.is_empty() {
        StoreFilter::All
    } else {
        StoreFilter::Selectors { exact, prefixes }
    }
}

/// Whether `event` should be forwarded to a subscriber watching `slug`
/// through `filter`. `event.store == "*"` (a whole-company `drop_company`)
/// always passes a named filter — it means every store for this slug,
/// including whichever ones the client asked for, is gone.
fn event_matches(filter: &StoreFilter, slug: &str, event: &WatchEvent) -> bool {
    if event.slug != slug {
        return false;
    }
    match filter {
        StoreFilter::All => true,
        StoreFilter::Selectors { exact, prefixes } => {
            event.store == "*"
                || exact.contains(&event.store)
                || prefixes.iter().any(|prefix| event.store.starts_with(prefix))
        }
    }
}

/// `Last-Event-ID` (a real reconnecting `EventSource`) wins over `after=`
/// (a first connect, or a client that sets its own query param); absent
/// both, `0` — which `replay_from` defines as "everything currently
/// retained," never a gap.
fn resolve_after_seq(headers: &HeaderMap, query_after: Option<u64>) -> u64 {
    headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .or(query_after)
        .unwrap_or(0)
}

/// The `reorg` control event. No `id:` (a reorg carries no seq of its own);
/// `data: {}` rather than truly empty, since an SSE event with a wholly
/// empty data buffer is defined to never dispatch at all (the spec drops
/// it), which would make `reorg` unobservable by exactly the class of client
/// that most needs it.
fn reorg_event() -> Event {
    Event::default()
        .event("reorg")
        .json_data(serde_json::json!({}))
        .unwrap_or_else(|_| Event::default().event("reorg").comment("reorg"))
}

/// One `doc-change` event: `id` is the seq (also duplicated into `data` —
/// see the module doc), `data` is `event` itself, JSON-encoded verbatim.
/// `json_data` on a plain `{u64, String, u64, String, bool}` struct cannot
/// practically fail (no NaN/Infinity floats, no non-string map keys), but
/// `unwrap`/`expect`/`panic` are denied crate-wide, so the theoretical error
/// path degrades to a harmless comment rather than being asserted away.
fn to_sse_event(event: &WatchEvent) -> Event {
    Event::default()
        .id(event.seq.to_string())
        .event("doc-change")
        .json_data(event)
        .unwrap_or_else(|_| Event::default().event("doc-change").comment("serialize-error"))
}

/// What one step of the watch stream decided to do — kept SEPARATE from the
/// SSE `Event` encoding on purpose: `axum::response::sse::Event`'s wire
/// bytes are produced by a private `finalize()` method (not visible outside
/// the `axum` crate), so a stream of `Event` is opaque to a test. A stream of
/// this plain enum is not — the filter/replay/dedup/gap DECISION is what
/// the design record and this ticket's acceptance criteria care
/// about; `to_sse_event`/`reorg_event` below are a thin, separately-obvious
/// final encoding step.
#[derive(Debug, Clone, PartialEq)]
enum WatchOutcome {
    DocChange(WatchEvent),
    Reorg,
}

/// Where a subscriber's stream currently is: draining the initial
/// replay batch (already computed, in order), or live on the broadcast
/// channel.
enum WatchPhase {
    Replay(std::vec::IntoIter<WatchOutcome>),
    Live,
}

/// One subscriber's state, threaded through [`stream::unfold`].
struct WatchState {
    phase: WatchPhase,
    rx: broadcast::Receiver<WatchEvent>,
    /// The highest seq already handled (replayed OR seen-and-dropped on the
    /// live channel), so a live event that the replay batch already covered
    /// — possible because `rx` subscribes BEFORE the replay snapshot is
    /// taken, see [`watch_outcomes`] — is not forwarded twice.
    last_seq: u64,
    slug: String,
    filter: StoreFilter,
}

/// The `stream::unfold` step: pulls the next already-computed replay
/// outcome, or blocks on the live broadcast channel and turns the next
/// matching, not-already-replayed [`WatchEvent`] into a
/// [`WatchOutcome::DocChange`] — looping past filtered-out or already-seen
/// events without yielding, and turning a lagging receiver into a single
/// [`WatchOutcome::Reorg`] per lag (the receiver auto-catches-up
/// internally; nothing else to do). Returns `None` (ending the stream) only
/// if the feed itself is gone.
async fn watch_step(mut state: WatchState) -> Option<(WatchOutcome, WatchState)> {
    loop {
        if let WatchPhase::Replay(iter) = &mut state.phase {
            if let Some(outcome) = iter.next() {
                return Some((outcome, state));
            }
            state.phase = WatchPhase::Live;
        }
        match state.rx.recv().await {
            Ok(watch_event) => {
                if watch_event.seq <= state.last_seq {
                    continue;
                }
                state.last_seq = watch_event.seq;
                if event_matches(&state.filter, &state.slug, &watch_event) {
                    return Some((WatchOutcome::DocChange(watch_event), state));
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return Some((WatchOutcome::Reorg, state));
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

/// Build the decision-level stream for one `/v1/docs/watch` subscriber:
/// subscribe to the feed FIRST, THEN take the replay snapshot — in that
/// order, so no event published between the two is lost (it lands in `rx`
/// instead, and `last_seq` de-duplicates it against the replay batch if the
/// ring also happened to retain it). Takes `&ChangeFeed` directly (not
/// `&Arc<DocStore>`) specifically so a test can drive it against a bare
/// `ChangeFeed::with_capacity(..)` — a real gap needs only a couple of
/// publishes against a tiny ring, not 1000+ writes through a real `DocStore`.
/// Separated from [`watch_stream`]'s SSE `Event` encoding so this is
/// directly unit-testable — see the module's `#[cfg(test)] mod watch_tests`.
fn watch_outcomes(
    feed: &ChangeFeed,
    query: &WatchQuery,
    headers: &HeaderMap,
) -> impl Stream<Item = WatchOutcome> {
    let rx = feed.subscribe();
    let after_seq = resolve_after_seq(headers, query.after);
    let filter = parse_store_filter(&query.stores);
    let (initial, last_seq): (Vec<WatchOutcome>, u64) = match feed.replay_from(after_seq) {
        Replay::Gap => (vec![WatchOutcome::Reorg], 0),
        Replay::Events(events) => {
            let last_seq = events.last().map(|e| e.seq).unwrap_or(after_seq);
            let initial: Vec<WatchOutcome> = events
                .into_iter()
                .filter(|event| event_matches(&filter, &query.slug, event))
                .map(WatchOutcome::DocChange)
                .collect();
            (initial, last_seq)
        }
    };
    let state = WatchState {
        phase: WatchPhase::Replay(initial.into_iter()),
        rx,
        last_seq,
        slug: query.slug.clone(),
        filter,
    };
    stream::unfold(state, watch_step)
}

fn to_sse_event_outcome(outcome: &WatchOutcome) -> Event {
    match outcome {
        WatchOutcome::DocChange(event) => to_sse_event(event),
        WatchOutcome::Reorg => reorg_event(),
    }
}

/// Build the wire-level SSE event stream for one subscriber — [`watch_outcomes`]
/// plus the final `Event` encoding.
fn watch_stream(
    store: &Arc<DocStore>,
    query: &WatchQuery,
    headers: &HeaderMap,
) -> impl Stream<Item = Result<Event, Infallible>> {
    watch_outcomes(store.feed(), query, headers).map(|outcome| Ok(to_sse_event_outcome(&outcome)))
}

/// The route's actual logic, called from a closure in [`router_with_heartbeat_interval`]
/// (not registered directly as a `Handler`) so `heartbeat_interval` can be
/// captured per-router rather than living in shared state.
/// # The fence here is the COMPANY, not a subtree (B4)
///
/// This route has no `SupervisionLiveSource` and therefore no manifest, so
/// there is no department in it to derive a subtree from — and it does not
/// want one, because the thing it discloses is a DOCUMENT changing, not a
/// person or a unit. The fence that IS derivable is the one the identity
/// already carries: `company_slug` is `Some` exactly for a `Person`, so a
/// person credential issued for company A cannot subscribe to company B's
/// document stream. Every daemon-scoped identity — operator, service, channel
/// — carries `None` and passes, which is what keeps the resident actuator's
/// own `GET /v1/docs/watch` working.
async fn watch(
    store: Arc<DocStore>,
    query: WatchQuery,
    caller: chiefd_core::store::identities::Identity,
    headers: HeaderMap,
    heartbeat_interval: Duration,
    shutdown: Option<shutdown_watch::Receiver<bool>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, RouteError> {
    // A person credential is company-scoped; a daemon-scoped one carries no
    // company and watches whatever it is pointed at. Unchanged by A6(c) — only
    // the shape it is read out of changed.
    if let Some(company_slug) = &caller.company_slug {
        if company_slug != &query.slug {
            return Err(RouteError::refused(
                super::disclosure_fence::CALLER_OUT_OF_SCOPE,
                format!(
                    "caller '{}' belongs to company '{company_slug}' and cannot watch '{}'",
                    caller.principal, query.slug
                ),
            ));
        }
    }
    let stream = watch_stream(&store, &query, &headers);
    // Axum graceful shutdown stops accepting connections but waits for in-flight
    // responses. A watcher is intentionally infinite, so the daemon's own
    // shutdown watch must end this producer first; otherwise the bounded drain
    // expires and aborts the listener. An unwired standalone router retains the
    // historical infinite stream until its client disconnects.
    let stop: BoxFuture<'static, ()> = match shutdown {
        Some(mut shutdown) => Box::pin(async move {
            let _ = shutdown.wait_for(|stopping| *stopping).await;
        }),
        None => Box::pin(std::future::pending()),
    };
    Ok(Sse::new(stream.take_until(stop))
        .keep_alive(KeepAlive::new().interval(heartbeat_interval).text("hb")))
}

// ===========================================================================
// Supervision & session lifecycle (the `apps/cli/src/legacy/organization/`
// port). Every route below is ONE writer transaction: the read, the decision
// and the write happen inside it, so no client holds a lease, retries a CAS,
// or sleeps between attempts. The TypeScript that used to make these decisions
// is deleted, not wrapped.
// ===========================================================================

/// The person chiefd injects into every worker-issued call. Never
/// model-supplied: it is the caller's authenticated identity.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WorkerIdentityBody {
    person_id: String,
}

impl From<WorkerIdentityBody> for chiefd_core::store::session_maintenance_ops::ExpectedIdentity {
    fn from(body: WorkerIdentityBody) -> Self {
        Self { person_id: body.person_id }
    }
}

/// The process/session/token triple that owns a maintenance request.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ClaimBody {
    process_id: i64,
    session_id: String,
    claim_token: String,
}

impl From<ClaimBody> for chiefd_core::store::session_maintenance_ops::Claim {
    fn from(body: ClaimBody) -> Self {
        Self {
            process_id: body.process_id,
            session_id: body.session_id,
            claim_token: body.claim_token,
        }
    }
}

/// Resolve the live company for a session-lifecycle route.
fn lifecycle_source<'a>(
    supervision_live: &'a Option<SupervisionLiveSource>,
    slug: &str,
) -> Result<&'a SupervisionLiveSource, RouteError> {
    match supervision_live {
        Some(source) if source.org_documents_slug == slug => Ok(source),
        _ => Err(RouteError::not_found("unknown-company", "no live company for this slug")),
    }
}

/// One error mapping for the whole family. Now literally the shared taxonomy:
/// the `Refused`-first special case it used to carry is what `company_error`
/// does for every caller.
fn lifecycle_error(error: &chiefd_core::error::ChiefdError) -> RouteError {
    company_error(error)
}

fn lifecycle_json<T: serde::Serialize>(value: &T) -> Result<Json<serde_json::Value>, RouteError> {
    serde_json::to_value(value)
        .map(Json)
        .map_err(|error| RouteError::fault("encode-failed", error.to_string()))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SlugOnlyRequest {
    slug: String,
}

async fn org_session_maintenance_ledger(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<SlugOnlyRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    let ledger =
        source.company.session_maintenance_ledger().await.map_err(|e| lifecycle_error(&e))?;
    lifecycle_json(&ledger)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MaintenanceQueueRequest {
    slug: String,
    action: chiefd_core::store::session_maintenance::MaintenanceAction,
    person_id: String,
    requested_by: String,
    /// An optional operator note. NEVER required: a caller that sends none is
    /// recorded with the line core authors from the action and the requester.
    #[serde(default)]
    reason: String,
    #[serde(default)]
    automatic: bool,
    // TOMBSTONE: `model` and `model_provider`, `set_model`'s wire fields.
    #[serde(default)]
    force: Option<bool>,
}

async fn org_session_maintenance_queue(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<MaintenanceQueueRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    // `requestedBy` ARRIVES IN THE BODY and, until track B1, was believed
    // without being bound to anything: a caller could name any person as the
    // requester and core only checked that the name existed. This is the same
    // binding `hire` uses — the declared requester must BE the authenticated
    // caller's principal, or the route refuses `requester-identity-mismatch`.
    //
    // NOT the `MaintenanceStartRequest.identity` shape beside it: that reads
    // `{ person_id }` from the BODY too, and comparing one caller-supplied
    // value against another is an integrity check rather than authentication.
    // Copying it here would have manufactured a second `requestedBy`.
    //
    // With enforcement off there is no caller extension and this is a no-op,
    // which is what lets B1 land before track A's credentials exist.
    bind_caller(&caller, Some(req.requested_by.as_str()), &req.slug)?;
    let input = chiefd_core::store::session_maintenance_ops::QueueInput {
        action: req.action,
        person_id: req.person_id,
        requested_by: req.requested_by,
        reason: req.reason,
        automatic: req.automatic,
        force: req.force,
    };
    let request =
        source.company.session_maintenance_queue(input).await.map_err(|e| lifecycle_error(&e))?;
    wake_reconcile(source);
    lifecycle_json(&request)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MaintenanceStartRequest {
    slug: String,
    identity: WorkerIdentityBody,
    action: chiefd_core::store::session_maintenance::MaintenanceAction,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    claim: Option<ClaimBody>,
    #[serde(default)]
    compact_session_id: Option<String>,
    #[serde(default)]
    compact_anchor_entry_id: Option<String>,
}

/// The six session-maintenance EXECUTION verbs bind their declared identity to
/// the authenticated caller.
///
/// `WorkerIdentityBody` is `{ person_id }` read from the BODY, and core's
/// `ExpectedIdentity::assert_owns` compares it against another caller-supplied
/// value — an integrity check, not authentication: a caller that names the
/// victim in both fields passes it. These six are the RUNNING PERSON's own
/// verbs (the intercom supplies the identity from the pane's context, spread
/// last so a payload cannot forge it), so the fence is the strongest one
/// available: the authenticated caller must BE the person it names.
///
/// `queue` is deliberately not in this family — it names a TARGET rather than
/// the caller, and B1 bound its `requestedBy` and added the scope check in
/// core.
async fn org_session_maintenance_start(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<MaintenanceStartRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    bind_caller(&caller, Some(req.identity.person_id.as_str()), &req.slug)?;
    let anchor = match (req.compact_session_id, req.compact_anchor_entry_id) {
        (Some(session_id), Some(entry_id)) => {
            Some(chiefd_core::store::session_maintenance_ops::CompactAnchor {
                session_id,
                entry_id,
            })
        }
        _ => None,
    };
    let claimed = source
        .company
        .session_maintenance_start(
            req.identity.into(),
            req.action,
            req.request_id,
            req.claim.map(Into::into),
            anchor,
        )
        .await
        .map_err(|e| lifecycle_error(&e))?;
    wake_reconcile(source);
    lifecycle_json(&serde_json::json!({ "request": claimed }))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MaintenanceClaimedRequest {
    slug: String,
    request_id: String,
    identity: WorkerIdentityBody,
    claim: ClaimBody,
}

async fn org_session_maintenance_defer(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<MaintenanceClaimedRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    bind_caller(&caller, Some(req.identity.person_id.as_str()), &req.slug)?;
    let request = source
        .company
        .session_maintenance_defer(req.request_id, req.claim.into(), req.identity.into())
        .await
        .map_err(|e| lifecycle_error(&e))?;
    wake_reconcile(source);
    lifecycle_json(&request)
}

async fn org_session_maintenance_interrupt(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<MaintenanceClaimedRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    bind_caller(&caller, Some(req.identity.person_id.as_str()), &req.slug)?;
    let request = source
        .company
        .session_maintenance_interrupt(req.request_id, req.claim.into(), req.identity.into())
        .await
        .map_err(|e| lifecycle_error(&e))?;
    wake_reconcile(source);
    lifecycle_json(&request)
}

// TOMBSTONE: `org_session_maintenance_complete_native`, the handler behind
// `/v1/org/session-maintenance/complete-native`. Deleted with the company
// native reset it credited.

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MaintenanceRecoverRequest {
    slug: String,
    identity: WorkerIdentityBody,
    claim: ClaimBody,
}

async fn org_session_maintenance_recover(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<MaintenanceRecoverRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    bind_caller(&caller, Some(req.identity.person_id.as_str()), &req.slug)?;
    let recovered = source
        .company
        .session_maintenance_recover(req.identity.into(), req.claim.into())
        .await
        .map_err(|e| lifecycle_error(&e))?;
    wake_reconcile(source);
    lifecycle_json(&serde_json::json!({
        "interrupted": recovered.interrupted,
        "replacements": recovered.replacements,
    }))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MaintenanceFinishRequest {
    slug: String,
    request_id: String,
    identity: WorkerIdentityBody,
    status: chiefd_core::store::session_maintenance::MaintenanceStatus,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    compact_entry_id: Option<String>,
}

async fn org_session_maintenance_finish(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<MaintenanceFinishRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    bind_caller(&caller, Some(req.identity.person_id.as_str()), &req.slug)?;
    let request = source
        .company
        .session_maintenance_finish(
            req.request_id,
            req.status,
            req.error,
            req.compact_entry_id,
            req.identity.into(),
        )
        .await
        .map_err(|e| lifecycle_error(&e))?;
    wake_reconcile(source);
    lifecycle_json(&request)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MaintenanceReconcileParkedRequest {
    slug: String,
    parked_person_ids: Vec<String>,
}

/// Reconcile OTHER PEOPLE's parked maintenance, so the fence is scope over the
/// people it names — not self-identity, which the six execution verbs beside it
/// take because each of those is the running person's own verb and names itself.
async fn org_session_maintenance_reconcile_parked(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<MaintenanceReconcileParkedRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    require_person_scope(&caller, source, &req.parked_person_ids).await?;
    let skipped = source
        .company
        .session_maintenance_reconcile_parked(req.parked_person_ids)
        .await
        .map_err(|e| lifecycle_error(&e))?;
    wake_reconcile(source);
    lifecycle_json(&serde_json::json!({ "skipped": skipped }))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AtRequest {
    slug: String,
    at: String,
}

async fn org_operator_escalation_drain(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<AtRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    let report =
        source.company.drain_operator_escalations(req.at).await.map_err(|e| lifecycle_error(&e))?;
    wake_reconcile(source);
    lifecycle_json(&serde_json::json!({
        "recordedFingerprints": report.recorded_fingerprints,
        "rejectedFingerprints": report.rejected_fingerprints,
        "doorbellArmed": report.doorbell_armed,
    }))
}

async fn org_operator_escalation_log(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<SlugOnlyRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    let records =
        source.company.operator_escalation_log().await.map_err(|e| lifecycle_error(&e))?;
    lifecycle_json(&serde_json::json!({ "records": records }))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DoorbellPlanRequest {
    slug: String,
    now_ms: i64,
}

async fn org_operator_escalation_doorbell_plan(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<DoorbellPlanRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::operator_escalation::DoorbellPlan;
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    let plan = source
        .company
        .operator_escalation_doorbell_plan(req.now_ms)
        .await
        .map_err(|e| lifecycle_error(&e))?;
    let body = match plan {
        DoorbellPlan::NothingPending => serde_json::json!({ "plan": "nothing-pending" }),
        DoorbellPlan::SuppressedByCooldown => {
            serde_json::json!({ "plan": "suppressed-by-cooldown" })
        }
        DoorbellPlan::Ring { text, fingerprint, attempts } => serde_json::json!({
            "plan": "ring",
            "text": text,
            "fingerprint": fingerprint,
            "attempts": attempts,
        }),
    };
    Ok(Json(body))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum DoorbellOutcomeBody {
    Delivered,
    NotDelivered,
    Skipped,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DoorbellSettleRequest {
    slug: String,
    outcome: DoorbellOutcomeBody,
    now_ms: i64,
}

async fn org_operator_escalation_doorbell_settle(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<DoorbellSettleRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    use chiefd_core::store::operator_escalation::{DoorbellOutcome, DoorbellSettlement};
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    let outcome = match req.outcome {
        DoorbellOutcomeBody::Delivered => DoorbellOutcome::Delivered,
        DoorbellOutcomeBody::NotDelivered => DoorbellOutcome::NotDelivered,
        DoorbellOutcomeBody::Skipped => DoorbellOutcome::Skipped,
    };
    let settlement = source
        .company
        .operator_escalation_doorbell_settle(outcome, req.now_ms)
        .await
        .map_err(|e| lifecycle_error(&e))?;
    let settled = match settlement {
        DoorbellSettlement::Delivered => "delivered",
        DoorbellSettlement::Deferred => "deferred",
        DoorbellSettlement::Dropped => "dropped",
    };
    Ok(Json(serde_json::json!({ "settled": settled })))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SessionEpochStampRequest {
    slug: String,
    epoch_at: String,
    reason: String,
}

async fn org_session_epoch_stamp(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<SessionEpochStampRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    let stamped = source
        .company
        .session_epoch_stamp(req.epoch_at, req.reason)
        .await
        .map_err(|e| lifecycle_error(&e))?;
    wake_reconcile(source);
    lifecycle_json(&stamped)
}

async fn org_session_epoch_ms(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<SlugOnlyRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let source = lifecycle_source(&supervision_live, &req.slug)?;
    let epoch_ms = source.company.session_epoch_ms().await.map_err(|e| lifecycle_error(&e))?;
    Ok(Json(serde_json::json!({ "epochMs": epoch_ms })))
}

#[cfg(test)]
mod watch_tests {
    use super::*;

    async fn fresh_store() -> (tempfile::TempDir, Arc<DocStore>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("org.sqlite").display().to_string();
        let store = Arc::new(DocStore::open(&path, 2).expect("open"));
        store.ensure_schema().await.expect("schema");
        (dir, store)
    }

    fn query(slug: &str, stores: &str, after: Option<u64>) -> WatchQuery {
        WatchQuery { slug: slug.to_string(), stores: stores.to_string(), after }
    }

    async fn collect_n(stream: impl Stream<Item = WatchOutcome>, n: usize) -> Vec<WatchOutcome> {
        tokio::pin!(stream);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(stream.next().await.expect("stream ended early"));
        }
        out
    }

    #[test]
    fn parse_store_filter_treats_star_and_empty_as_all() {
        assert!(matches!(parse_store_filter("*"), StoreFilter::All));
        assert!(matches!(parse_store_filter(""), StoreFilter::All));
        assert!(matches!(parse_store_filter(" , , "), StoreFilter::All));
    }

    #[test]
    fn parse_store_filter_trims_dedupes_and_separates_exact_from_prefix_selectors() {
        match parse_store_filter(" activity , mailbox/ , supervision ,activity, mailbox/") {
            StoreFilter::Selectors { exact, prefixes } => {
                assert_eq!(exact.len(), 2);
                assert!(exact.contains("activity"));
                assert!(exact.contains("supervision"));
                assert_eq!(prefixes.len(), 1);
                assert!(prefixes.contains("mailbox/"));
            }
            StoreFilter::All => panic!("must be a named filter"),
        }
    }

    #[test]
    fn event_matches_requires_the_exact_slug() {
        let filter = StoreFilter::All;
        let event = WatchEvent {
            seq: 1,
            slug: "co@abc".into(),
            store: "activity".into(),
            updated_at: "t".into(),
            removed: false,
        };
        assert!(event_matches(&filter, "co@abc", &event));
        assert!(!event_matches(&filter, "co@xyz", &event), "a different slug must never leak");
    }

    #[test]
    fn event_matches_named_filter_accepts_the_wildcard_removed_store() {
        let filter = parse_store_filter("activity,supervision");
        let ordinary = WatchEvent {
            seq: 1,
            slug: "co@abc".into(),
            store: "mailbox/alice".into(),
            updated_at: "t".into(),
            removed: false,
        };
        assert!(!event_matches(&filter, "co@abc", &ordinary), "unsubscribed store must not leak");

        let wildcard_removed = WatchEvent {
            seq: 2,
            slug: "co@abc".into(),
            store: "*".into(),
            updated_at: String::new(),
            removed: true,
        };
        assert!(
            event_matches(&filter, "co@abc", &wildcard_removed),
            "a whole-company drop must pass even a named filter — every store, including the ones subscribed to, is gone"
        );
    }

    #[test]
    fn event_matches_prefix_and_exact_selectors_without_leaking_similar_names() {
        let filter = parse_store_filter("mailbox/,operator-escalation-intents");
        let mailbox = WatchEvent {
            seq: 1,
            slug: "co@abc".into(),
            store: "mailbox/senior-engineer".into(),
            updated_at: "t".into(),
            removed: false,
        };
        let escalation = WatchEvent {
            seq: 2,
            slug: "co@abc".into(),
            store: "operator-escalation-intents".into(),
            updated_at: "t".into(),
            removed: false,
        };
        let similar = WatchEvent {
            seq: 3,
            slug: "co@abc".into(),
            store: "mailbox-archive/senior-engineer".into(),
            updated_at: "t".into(),
            removed: false,
        };
        assert!(event_matches(&filter, "co@abc", &mailbox));
        assert!(event_matches(&filter, "co@abc", &escalation));
        assert!(!event_matches(&filter, "co@abc", &similar));
    }

    #[tokio::test]
    async fn a_fresh_subscriber_with_no_after_replays_the_current_ring_for_its_slug_then_goes_live()
    {
        let (_dir, store) = fresh_store().await;
        store.feed().publish("co@abc", "activity", "t0", false);
        store.feed().publish("co@xyz", "activity", "t0", false);

        let headers = HeaderMap::new();
        let q = query("co@abc", "activity", None);
        let outcomes = watch_outcomes(store.feed(), &q, &headers);
        let batch = collect_n(outcomes, 1).await;
        assert_eq!(
            batch,
            vec![WatchOutcome::DocChange(WatchEvent {
                seq: 1,
                slug: "co@abc".into(),
                store: "activity".into(),
                updated_at: "t0".into(),
                removed: false,
            })],
            "only the subscribed slug's event replays, not the other company's"
        );
    }

    #[tokio::test]
    async fn unmatched_stores_are_skipped_never_yielded() {
        let (_dir, store) = fresh_store().await;
        store.feed().publish("co@abc", "activity", "t0", false);
        store.feed().publish("co@abc", "supervision", "t1", false);

        let headers = HeaderMap::new();
        let q = query("co@abc", "supervision", None);
        let outcomes = watch_outcomes(store.feed(), &q, &headers);
        // Exactly one outcome (supervision) should surface, even though two
        // events exist in the ring — collect_n(1) proves the NEXT item is
        // "supervision", not "activity" leaking through unfiltered.
        let batch = collect_n(outcomes, 1).await;
        match &batch[0] {
            WatchOutcome::DocChange(e) => assert_eq!(e.store, "supervision"),
            WatchOutcome::Reorg => panic!("must not be a reorg"),
        }
    }

    #[tokio::test]
    async fn mailbox_prefix_selector_filters_replay_and_live_events_server_side() {
        let (_dir, store) = fresh_store().await;
        store.feed().publish("co@abc", "mailbox/alice", "t0", false);
        store.feed().publish("co@abc", "activity", "t1", false);
        store.feed().publish("co@abc", "mailbox-archive/alice", "t2", false);

        let headers = HeaderMap::new();
        let q = query("co@abc", "mailbox/", None);
        let outcomes = watch_outcomes(store.feed(), &q, &headers);
        tokio::pin!(outcomes);
        match outcomes.next().await {
            Some(WatchOutcome::DocChange(event)) => assert_eq!(event.store, "mailbox/alice"),
            other => panic!("expected only the replayed mailbox family event, got {other:?}"),
        }

        store.feed().publish("co@abc", "supervision", "t3", false);
        store.feed().publish("co@abc", "mailbox/bob", "t4", false);
        match outcomes.next().await {
            Some(WatchOutcome::DocChange(event)) => assert_eq!(event.store, "mailbox/bob"),
            other => panic!("expected only the live mailbox family event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn after_a_seq_still_in_the_ring_replays_only_the_newer_matching_events() {
        let (_dir, store) = fresh_store().await;
        store.feed().publish("co@abc", "activity", "t0", false);
        store.feed().publish("co@abc", "activity", "t1", false);

        let headers = HeaderMap::new();
        let q = query("co@abc", "activity", Some(1));
        let outcomes = watch_outcomes(store.feed(), &q, &headers);
        let batch = collect_n(outcomes, 1).await;
        match &batch[0] {
            WatchOutcome::DocChange(e) => {
                assert_eq!(e.seq, 2, "seq=1 already delivered, only the CAS win replays")
            }
            WatchOutcome::Reorg => panic!("seq=1 is still retained — must not be a gap"),
        }
    }

    #[tokio::test]
    async fn after_an_evicted_seq_yields_reorg_then_resumes_live() {
        // A bare `ChangeFeed` with a tiny ring — no `DocStore`/SQLite needed,
        // and no 1000+ writes to overflow the module-level ~1024 default.
        let feed = ChangeFeed::with_capacity(2);
        feed.publish("co@abc", "activity", "t0", false);
        feed.publish("co@abc", "activity", "t1", false);
        feed.publish("co@abc", "activity", "t2", false);
        feed.publish("co@abc", "activity", "t3", false); // ring now [3, 4]

        // `after=1`: the event the client needs next (seq=2) was evicted —
        // a genuine gap, not a "the client's own last-seen was evicted"
        // false positive (see feed.rs's own tests for that distinction).
        let headers = HeaderMap::new();
        let q = query("co@abc", "activity", Some(1));
        let outcomes = watch_outcomes(&feed, &q, &headers);
        tokio::pin!(outcomes);
        assert_eq!(
            outcomes.next().await,
            Some(WatchOutcome::Reorg),
            "seq=2 was evicted along with seq=1 — a genuine gap"
        );

        // The connection is NOT closed by a reorg — a publish afterward must
        // still reach this same subscriber.
        feed.publish("co@abc", "activity", "t4", false);
        match outcomes.next().await {
            Some(WatchOutcome::DocChange(event)) => {
                assert_eq!(event.seq, 5, "resumed live after the reorg")
            }
            other => panic!("expected the live event after the reorg, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_removed_wildcard_event_is_forwarded_with_the_documented_removal_shape() {
        let (_dir, store) = fresh_store().await;
        store.feed().publish("co@abc", "activity", "t0", false);
        store.feed().publish("co@abc", "*", "", true);

        let headers = HeaderMap::new();
        let q = query("co@abc", "activity", Some(1));
        let outcomes = watch_outcomes(store.feed(), &q, &headers);
        let batch = collect_n(outcomes, 1).await;
        match &batch[0] {
            WatchOutcome::DocChange(e) => {
                assert_eq!(e.store, "*", "drop_company's wildcard passes a named filter");
                assert!(e.removed);
                assert_eq!(e.updated_at, "");
            }
            WatchOutcome::Reorg => panic!("seq=1 is still retained — must not be a gap"),
        }
    }

    #[test]
    fn resolve_after_seq_prefers_the_last_event_id_header_over_the_query_param() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", "42".parse().expect("header value"));
        assert_eq!(resolve_after_seq(&headers, Some(7)), 42);
    }

    #[test]
    fn resolve_after_seq_falls_back_to_the_query_param_then_zero() {
        let headers = HeaderMap::new();
        assert_eq!(resolve_after_seq(&headers, Some(7)), 7);
        assert_eq!(resolve_after_seq(&headers, None), 0);
    }

    #[test]
    fn resolve_after_seq_ignores_an_unparseable_header_and_falls_back() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", "not-a-number".parse().expect("header value"));
        assert_eq!(resolve_after_seq(&headers, Some(7)), 7);
    }

    #[tokio::test]
    async fn a_watcher_that_lags_the_broadcast_channel_mid_stream_gets_reorg_not_silence() {
        // Distinct from `after_an_evicted_seq_yields_reorg_then_resumes_live`
        // above: that one is a REPLAY gap (a stale `after` at connect time).
        // This is the LIVE path — a subscriber already past its replay batch
        // and blocked in `watch_step`'s `state.rx.recv().await`, whose
        // broadcast receiver falls behind the channel's own bound because
        // nothing ever polls the stream while a burst of publishes happens.
        // `RecvError::Lagged` must become a `Reorg`, never a silently
        // dropped run of events (the "lagging-consumer eviction" acceptance
        // criterion, #259).
        let feed = ChangeFeed::with_capacity(4);
        let headers = HeaderMap::new();
        let q = query("co@abc", "activity", None);
        // No data yet: the initial replay batch is empty, so the stream is
        // already sitting in `state.rx.recv().await` (the Live phase) the
        // instant this future is polled — never mind that we don't poll it
        // until after the burst below.
        let outcomes = watch_outcomes(&feed, &q, &headers);
        tokio::pin!(outcomes);

        // Publish far more than the broadcast channel's capacity (4) without
        // ever polling `outcomes` — the receiver falls behind.
        for _ in 1..=20 {
            feed.publish("co@abc", "activity", "t", false);
        }

        assert_eq!(
            outcomes.next().await,
            Some(WatchOutcome::Reorg),
            "a receiver that fell behind the broadcast channel must surface as reorg, not skip silently"
        );

        // Still live afterward. A lagged `broadcast::Receiver` can report a
        // FEW more catch-up `Lagged`s (or drain some already-stale events)
        // before settling — tokio's own internal recovery cadence, not
        // something this code controls — so prove liveness by publishing a
        // sentinel and draining until it appears, rather than asserting the
        // very next item is it.
        // `seq` is always auto-assigned by the feed's own monotonic counter,
        // so a sentinel VALUE has to ride in a field the caller controls. The
        // store name is filtered on, so the sentinel rides `updated_at`.
        const SENTINEL_AT: &str = "sentinel-9999";
        feed.publish("co@abc", "activity", SENTINEL_AT, false);
        let mut saw_sentinel = false;
        for i in 0..50 {
            let item = tokio::time::timeout(Duration::from_millis(500), outcomes.next())
                .await
                .unwrap_or_else(|_| panic!("iteration {i} timed out waiting on outcomes.next()"));
            match item {
                Some(WatchOutcome::DocChange(event)) if event.updated_at == SENTINEL_AT => {
                    saw_sentinel = true;
                    break;
                }
                Some(_) => continue, // a stale catch-up event or another reorg — keep draining
                None => break,
            }
        }
        assert!(
            saw_sentinel,
            "the subscriber must resume live and eventually observe a fresh publish"
        );
    }

    #[test]
    fn atomic_staffing_requests_reject_legacy_revision_fields() {
        let create = serde_json::json!({
            "slug": "co@root",
            "requester": { "kind": "operator" },
            "departmentId": "product",
            "parentId": "executive",
            "name": "Product",
            "head": { "kind": "appoint-existing", "personId": "bo" }
        });
        assert!(
            serde_json::from_value::<OrgDepartmentCreateRequest>(create.clone()).is_ok(),
            "baseline create body must be valid"
        );
        for retired in ["skills", "extensions", "packages"] {
            let mut smuggled = create.clone();
            smuggled["head"][retired] = serde_json::json!(["retired-selection"]);
            assert!(
                serde_json::from_value::<OrgDepartmentCreateRequest>(smuggled).is_err(),
                "department create must reject retired head field {retired} at the Rust boundary"
            );
        }
        let mut contract = create.clone();
        contract["unit"] = serde_json::json!({
            "kind": "contract",
            "transient": {
                "engagement": "Ship the launch site",
                "launchedAt": "2026-08-02T06:00:00.000Z",
                "expiresAt": "2026-09-01T00:00:00.000Z"
            }
        });
        assert!(
            serde_json::from_value::<OrgDepartmentCreateRequest>(contract.clone()).is_ok(),
            "complete contract metadata must be accepted"
        );
        let mut missing_transient = contract.clone();
        missing_transient["unit"] = serde_json::json!({"kind": "contract"});
        assert!(
            serde_json::from_value::<OrgDepartmentCreateRequest>(missing_transient).is_err(),
            "a contract must carry transient metadata"
        );
        let mut unknown_transient_field = contract.clone();
        unknown_transient_field["unit"]["transient"]["surplus"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<OrgDepartmentCreateRequest>(unknown_transient_field).is_err(),
            "unknown contract metadata must be rejected rather than dropped"
        );
        let mut malformed_unit = contract.clone();
        malformed_unit["unit"]["kind"] = serde_json::json!("company");
        assert!(
            serde_json::from_value::<OrgDepartmentCreateRequest>(malformed_unit).is_err(),
            "the root-only company kind must be rejected at the child-unit boundary"
        );
        let mut stale_create = create;
        stale_create["expectedSeq"] = serde_json::json!(7);
        assert!(
            serde_json::from_value::<OrgDepartmentCreateRequest>(stale_create).is_err(),
            "expectedSeq must be rejected, not silently ignored"
        );

        let hire = serde_json::json!({
            "slug": "co@root",
            "requester": { "kind": "operator" },
            "personId": "zoe",
            "departmentId": "engineering",
            "name": "Zoe",
            "title": "Engineer",
            "mandate": "Build"
        });
        assert!(
            serde_json::from_value::<OrgPersonHireRequest>(hire.clone()).is_ok(),
            "baseline hire body must be valid"
        );
        for retired in ["skills", "extensions", "packages"] {
            let mut smuggled = hire.clone();
            smuggled[retired] = serde_json::json!(["retired-selection"]);
            assert!(
                serde_json::from_value::<OrgPersonHireRequest>(smuggled).is_err(),
                "hire must reject retired field {retired} at the Rust boundary"
            );
        }
        let mut stale_hire = hire.clone();
        stale_hire["revision"] = serde_json::json!(7);
        assert!(
            serde_json::from_value::<OrgPersonHireRequest>(stale_hire).is_err(),
            "revision must be rejected, not silently ignored"
        );
        // Chief is out of the provider/model business: every model input a
        // caller could once send is now an unknown field, refused rather than
        // silently ignored. `deny_unknown_fields` is what makes that a
        // BOUNDARY rather than a convention.
        for retired in ["model", "provider", "modelApproval", "expectedModel", "taskClass"] {
            let mut smuggled = hire.clone();
            smuggled[retired] = serde_json::json!("smuggled");
            assert!(
                serde_json::from_value::<OrgPersonHireRequest>(smuggled).is_err(),
                "retired {retired} must be rejected by the Rust request boundary"
            );
        }

        let transfer = serde_json::json!({
            "slug": "co@root",
            "personId": "zoe",
            "destinationId": "engineering",
            "intent": "person-transfer:zoe",
            // NO `reason`: audit prose is not asked of a caller, and the field
            // left the contract with the requirement (the ledger line is
            // authored by `transfer_person`). `actor` stays because it is
            // accepted-and-ignored rather than deleted.
            "actor": "chief"
        });
        assert!(
            serde_json::from_value::<OrgPersonTransferRequest>(transfer.clone()).is_ok(),
            "baseline direct transfer body must be valid"
        );
        let mut stale_transfer = transfer;
        stale_transfer["expectedSeq"] = serde_json::json!(7);
        assert!(
            serde_json::from_value::<OrgPersonTransferRequest>(stale_transfer).is_err(),
            "direct transfer must reject expectedSeq, not preserve CAS retry semantics"
        );
    }

    #[test]
    fn manifest_genesis_rejects_legacy_revision_protocol_fields() {
        // The native first-write body: the company SPEC plus the Founder
        // route. #751 deleted the pre-normalized `manifest` string and the
        // caller-built `materialization`/`personContracts` documents. The
        // materialization document no longer exists, and chiefd derives person
        // contracts itself, so
        // those three keys are as legacy as the revision fields below, and
        // `deny_unknown_fields` refuses them all the same way.
        let request = serde_json::json!({
            "slug": "co@root",
            "spec": {
                "name": "Co",
                "purpose": "Freeze the genesis wire shape.",
                "chief": { "id": "chief", "name": "Avery" },
                "departments": []
            },
            "at": "2026-08-02T00:00:00Z"
        });
        assert!(
            serde_json::from_value::<OrgManifestGenesisRequest>(request.clone()).is_ok(),
            "the native first-write request has no revision input"
        );

        for legacy_field in ["expectedRevision", "currentRevision", "revision"] {
            let mut stale = request.clone();
            stale[legacy_field] = serde_json::json!(7);
            assert!(
                serde_json::from_value::<OrgManifestGenesisRequest>(stale).is_err(),
                "{legacy_field} must be rejected instead of reviving manifest CAS"
            );
        }

        // The retired pre-#751 genesis inputs are refused just as hard: a
        // caller may not hand chiefd a pre-normalized manifest, a deleted
        // materialization document, or person contracts chiefd derives.
        for retired_field in ["manifest", "materialization", "personContracts", "bootstrap"] {
            let mut built = request.clone();
            built[retired_field] = serde_json::json!({});
            assert!(
                serde_json::from_value::<OrgManifestGenesisRequest>(built).is_err(),
                "{retired_field} must be rejected instead of accepting a caller-built artifact"
            );
        }
    }

    #[test]
    fn singleton_row_publishes_reject_retired_sequence_inputs() {
        // The mailbox publish this used to assert on is deleted with its route
        // (the publisher-route sweep found no caller). The RULE survives on the
        // one singleton publish left: a caller may not hand chiefd a sequence.
        let runtime = serde_json::json!({"slug":"co@root","doc":"{}"});
        assert!(serde_json::from_value::<DirectOrgRowPublishRequest>(runtime.clone()).is_ok());

        let mut stale = runtime;
        stale["expectedSeq"] = serde_json::json!(7);
        assert!(
            serde_json::from_value::<DirectOrgRowPublishRequest>(stale).is_err(),
            "a singleton publish must reject expectedSeq rather than revive a caller fence"
        );
    }

    #[test]
    fn direct_atomic_staffing_is_attributed_to_operator() {
        let requester: OrgStaffingRequester =
            serde_json::from_value(serde_json::json!({ "kind": "operator" }))
                .expect("operator requester");
        assert_eq!(
            parse_staffing_requester(requester).expect("valid operator requester"),
            (None, "operator".to_string())
        );
    }
}

/// Synthesize the current instant once per request, chiefd-clock authority.
/// Every store call a single request makes is given the SAME string, so a
/// request that cascades several writes gets one consistent timestamp.
pub(crate) fn now_iso() -> String {
    let epoch_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    chiefd_core::isotime::iso_millis(epoch_millis)
}

// ---------------------------------------------------------------------------
// Durable reminders.
//
// These three routes are what make the reminder engine REACHABLE. `arm_reminder`
// / `stop_reminder` / `list_reminders` have existed and been tested in
// chiefd-core since this branch opened, and until now nothing outside that crate
// could call them -- an engine nobody can reach, which is the operator's actual
// ask ("agents could schedule loops to remind themselves") left unmet.
//
// A reminder lives in the supervision ledger inside `CompanyDb`, and the
// plan's ownership table
// states there is exactly ONE writer of a reminder: chiefd. Routing these
// through `DocStore` would plant a second authority for reminder state -- the
// dual-writer trap #81 catalogued four times in one day. The precedent copied
// instead is `cas`'s `supervision.launcher_cas` arm above: a supervision-ledger
// mutation served over HTTP through `SupervisionLiveSource` ->
// `CompanyDb::mutate`.
//
// Consequence, stated rather than left implicit: there is NO `org_documents`
// fallback here. A request naming a foreign slug, or arriving at a router with
// no live source (the standalone/migration entrypoints), is REFUSED. Falling
// through to `org_documents` would write reminder rows into a store the
// `ReminderDispatch` duty never reads -- armed forever, fired never, the exact
// produced-forever/delivered-never shape this branch already hit once in
// `dispatch::recipients_for`.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReminderResponse {
    reminder: supervision::Reminder,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListRemindersResponse {
    reminders: Vec<supervision::Reminder>,
}

/// `createdByPersonId` is deliberately NOT on this wire. Who armed a reminder
/// is the same fact as who was allowed to arm it, and since #751/P7 that fact
/// is the enrolled key the caller signed a challenge with — a body field would
/// be a claim the caller makes about itself, which is exactly what P7 deleted
/// everywhere else.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArmReminderRequest {
    slug: String,
    person_id: String,
    prompt: String,
    interval_ms: i64,
    #[serde(default = "default_recurring")]
    recurring: bool,
    #[serde(default)]
    expires_at: Option<String>,
}

/// A reminder is recurring unless the caller says otherwise: "remind me every
/// morning" is the request people actually make, and a one-shot is the special
/// case. Defaulting the other way would silently turn every un-annotated arm
/// into a single fire the operator never sees again.
fn default_recurring() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRemindersRequest {
    slug: String,
    person_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StopReminderRequest {
    slug: String,
    person_id: String,
    reminder_id: String,
}

/// Resolve the live supervision authority for `slug`, or refuse.
///
/// A fence-free CLEAR request for a row store (org-data-normalization P0, N8):
/// unconditionally deletes the store's rows and emits one op="delete"
/// org_events touch per removed entity. `at` is the event stamp. Shared shape
/// for launch-intent and runtime, the two droppable stores whose clear still
/// has a caller (`chief-cli`'s reset path posts both).
#[derive(Deserialize)]
struct OrgClearRequest {
    slug: String,
    at: String,
}

async fn org_launch_intent_clear(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgClearRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    require_company_wide_authority(&caller, &source, "clear the launch intent").await?;
    match source.company.launch_intent_clear(req.at).await {
        Ok(()) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"cleared": true})))
        }
        Err(other) => Err(company_error(&other)),
    }
}

/// `POST /v1/org/stand-down`'s body.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgStandDownRequest {
    slug: String,
    at: String,
    /// What the operator said about it. Optional, and free text.
    #[serde(default)]
    reason: String,
}

/// Stand a company down: record the operator's decision and empty the launch
/// intent, in one transaction.
///
/// # Why this is a company-wide authority and not a staffing verb
///
/// It is a decision about the whole company rather than about any person in it,
/// so it asks the same question `clear the launch intent` above asks. It is
/// deliberately NOT scoped to a subtree: a head standing down its own unit is a
/// department pause, which already exists.
async fn org_stand_down_set(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgStandDownRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    require_company_wide_authority(&caller, &source, "stand the company down").await?;
    match source.company.stand_down_set(req.at, req.reason).await {
        // WAKE THE RECONCILER. The stand-down has already emptied the fence;
        // the pass that observes it is what takes the panes down, and an
        // operator who asked their company to stop must not wait out a cadence
        // to see it happen.
        Ok(()) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"applied": true, "stoodDown": true})))
        }
        Err(other) => Err(company_error(&other)),
    }
}

/// Lift a company's stand-down. Held mail is still pending, so the ordinary
/// wake brings its recipients back on the next pass.
async fn org_stand_down_clear(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgClearRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    require_company_wide_authority(&caller, &source, "resume the company").await?;
    match source.company.stand_down_clear(req.at).await {
        Ok(()) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"applied": true, "stoodDown": false})))
        }
        Err(other) => Err(company_error(&other)),
    }
}

/// `POST /v1/org/stand-down/read`'s body — the slug alone.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgStandDownReadRequest {
    slug: String,
}

/// Is this company stood down, and since when?
///
/// `{"standDown": null}` for a company that is working, so an absent stand-down
/// is a value a caller reads rather than a shape they have to branch on.
async fn org_stand_down_read(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgStandDownReadRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    match source.company.stand_down_read().await {
        Ok(stand_down) => Ok(Json(serde_json::json!({"standDown": stand_down}))),
        Err(other) => Err(company_error(&other)),
    }
}

// TOMBSTONE (chief-home-is-cwd §4c): `OrgPrepareCeoOnlyRequest`,
// `OrgPrepareCeoOnlyResponse` and the `prepare_ceo_only_response_tests` module
// stood here — the body of `POST /v1/org/runtime/prepare-ceo-only`, and the
// test that held it byte-equal to the fixture `chief-cli` parses. The route is
// deleted with the daemon-side CEO boot, and the shared fixture
// `conformance/fixtures/wire/prepare-ceo-only-response.json` went with
// `chief-cli`'s own parser and its three verdict tests (the client half of
// §4c) — an `include_str!` was its last reader, so it could only leave once
// that reader did.

/// `POST /v1/org/converge-safety/set-actuation-config`'s body.
///
/// Only the mode. `sweepLive` and `budgetOverride` are deliberately absent:
/// this route exists so a caller can say "actuate, or do not" WITHOUT having to
/// know or restate the other two, and every field a caller can restate is a
/// field a caller can accidentally clear.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrgSetActuationConfigRequest {
    slug: String,
    actuation_mode: chiefd_core::store::converge_safety::ActuationMode,
}

/// Set a live company's durable actuation mode.
///
/// The HTTP twin of `chiefd set-actuation-config --mode …`, and the same
/// `chiefd_host::converge_apply::safety::set_actuation_config` call underneath
/// — not a second implementation of it. It exists because an api-hosted
/// company has to be pinned to `shadow` (its Pi children live in `apps/api`,
/// so chiefd must compute the plan and execute none of it) and the only way to
/// say that before was to spawn the CLI.
///
/// The merge is the reason this is not the raw `publish` route. It reads the
/// **stored** state — never `effective_config()`, which folds the breaker in and
/// would silently write `shadow` back as if the operator had chosen it — and
/// carries `sweep_live`/`budget_override` forward untouched. Setting the config
/// also resumes a tripped breaker, which is `set_actuation_config`'s own
/// documented behaviour: an operator restating what a company may actuate is
/// exactly the acknowledgement the breaker waits for.
async fn org_converge_safety_set_actuation_config(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgSetActuationConfigRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    let stored = source
        .company
        .read(|snapshot| chiefd_core::store::converge_safety::read(snapshot).into_parts().0);
    let mode = req.actuation_mode;
    match chiefd_host::converge_apply::safety::set_actuation_config(
        &source.company,
        mode,
        stored.sweep_live,
        stored.budget_override,
    )
    .await
    {
        Ok(()) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({
                "actuationMode": match mode {
                    chiefd_core::store::converge_safety::ActuationMode::Shadow => "shadow",
                    chiefd_core::store::converge_safety::ActuationMode::Apply => "apply",
                },
                "sweepLive": stored.sweep_live,
                "budgetOverride": stored.budget_override,
            })))
        }
        Err(chiefd_core::error::ChiefdError::Refused(refusal)) => {
            Err(RouteError::refused(refusal.code, refusal.message.clone()))
        }
        Err(other) => Err(company_error(&other)),
    }
}

async fn org_runtime_clear(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<OrgClearRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let Some(source) = supervision_live.filter(|s| req.slug == s.org_documents_slug) else {
        return Err(RouteError::not_found("unknown-company", "no live company for this slug"));
    };
    require_company_wide_authority(&caller, &source, "drop the runtime row").await?;
    match source.company.runtime_clear(req.at).await {
        Ok(()) => {
            wake_reconcile(&source);
            Ok(Json(serde_json::json!({"cleared": true})))
        }
        Err(other) => Err(company_error(&other)),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventJournalReadRequest {
    slug: String,
    key_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventJournalInsertRequest {
    slug: String,
    key_digest: String,
    id: String,
    event: serde_json::Value,
    created_at_ms: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventJournalPruneRequest {
    slug: String,
    older_than_ms: i64,
}

/// Read one exactly-once event marker by digest. DocStore-direct on the shared
/// org.sqlite (no live-company gate — parity with `/v1/docs/read`): markers are a
/// cross-producer primitive written before any company is "live". `found:false`
/// when no marker exists.
async fn org_event_journal_read(
    State(store): State<Arc<DocStore>>,
    Json(req): Json<EventJournalReadRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    let slug = req.slug.clone();
    let key_digest = req.key_digest.clone();
    let marker = store
        .engine()
        .exec_interactive(move |tx| {
            chiefd_core::store::event_journal_rows::read_marker(tx, &slug, &key_digest)
                .map_err(|e| StoreError::Query(e.to_string()))
        })
        .await
        .map_err(|e| store_error(&e))?;
    match marker {
        Some(marker) => {
            let body = serde_json::to_string(&marker).map_err(encode_fault)?;
            Ok(Json(serde_json::json!({"found": true, "marker": body})))
        }
        None => Ok(Json(serde_json::json!({"found": false}))),
    }
}

/// Insert an exactly-once event marker if absent. DocStore-direct (no live-company
/// gate, no org_events fence — an independent atomic marker). `created` is the
/// O_EXCL "one winner" — true only when this call wrote the row.
async fn org_event_journal_insert(
    State(store): State<Arc<DocStore>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<EventJournalInsertRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    // WHOSE JOURNAL. This route is DocStore-direct on the shared `org.sqlite`
    // with no live-company gate, so the body's `slug` alone decided which
    // company's exactly-once markers were written, and nothing compared it to
    // the caller. There is no requester in this request and none is added: the
    // company IS the fence, and a body field invented so it could be bound
    // would be a `requested_by` supplied by the caller it claims to
    // authenticate.
    bind_caller_company(&caller, &req.slug)?;
    let serde_json::Value::Object(event) = req.event else {
        return Err(RouteError::malformed("malformed-doc", "event must be a JSON object"));
    };
    let slug = req.slug.clone();
    let key_digest = req.key_digest.clone();
    let id = req.id.clone();
    let created_at_ms = req.created_at_ms;
    let out = store
        .engine()
        .exec_interactive(move |tx| {
            chiefd_core::store::event_journal_rows::insert_if_absent(
                tx,
                &slug,
                &key_digest,
                &id,
                &event,
                created_at_ms,
            )
            .map_err(|e| StoreError::Query(e.to_string()))
        })
        .await
        .map_err(|e| store_error(&e))?;
    Ok(Json(serde_json::json!({"created": out.created})))
}

/// Prune expired once-markers through their typed table, never through the
/// retired generic document-prefix API.
async fn org_event_journal_prune(
    State(store): State<Arc<DocStore>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<EventJournalPruneRequest>,
) -> Result<Json<serde_json::Value>, RouteError> {
    // The same fence as its sibling above, and for a sharper reason: this one
    // DELETES markers in bulk, so a caller naming another company's slug erased
    // that company's exactly-once history and let every one of its events fire
    // a second time.
    bind_caller_company(&caller, &req.slug)?;
    let slug = req.slug;
    let older_than_ms = req.older_than_ms;
    let rows_affected = store
        .engine()
        .exec_interactive(move |tx| {
            chiefd_core::store::event_journal_rows::prune_older_than(tx, &slug, older_than_ms)
                .map_err(|e| StoreError::Query(e.to_string()))
        })
        .await
        .map_err(|e| store_error(&e))?;
    Ok(Json(serde_json::json!({"rowsAffected": rows_affected})))
}

/// The refusal is a 404 rather than a 500 because nothing is broken: this
/// process simply is not the authority for that company. See the module note
/// above for why there is no `org_documents` fallback.
fn reminder_source<'a>(
    supervision_live: &'a Option<SupervisionLiveSource>,
    slug: &str,
) -> Result<&'a SupervisionLiveSource, RouteError> {
    match supervision_live {
        Some(source) if source.org_documents_slug == slug => Ok(source),
        Some(_) => Err(RouteError::not_found(
            "unknown-company",
            format!(
                "this chiefd is not the reminder authority for '{slug}' — reminders are served \
                 only for the company this process runs"
            ),
        )),
        None => Err(RouteError::not_found(
            "unknown-company",
            "this chiefd has no live company; reminders are unavailable on the standalone \
             docstore surface",
        )),
    }
}

/// The person a verified credential resolved to, or a refusal.
///
/// Modelled exactly on `runtime_routes::require_self_identity`, which #751/P7
/// landed for the two personal runtime switches, and refusing for the same
/// reason: a reminder is somebody's durable scheduled wake-up, so "who is
/// asking" has to be a proven fact rather than a body field. Absence is a
/// refusal, not local trust.
///
/// Only a `person` credential names a person. An operator, service or channel
/// credential authenticates but is nobody's agent — reading one as a person is
/// how a gateway token would become a manager (`caller_auth`'s own warning), so
/// it is refused rather than promoted to unconditional scope.
fn reminder_actor(caller: &chiefd_core::store::identities::Identity) -> Result<String, RouteError> {
    use chiefd_core::store::identities::IdentityKind;
    if caller.kind == IdentityKind::Person {
        return Ok(caller.principal.clone());
    }
    Err(RouteError::forbidden(
        "caller-not-a-person",
        format!(
            "'{}' is a daemon-scoped credential, not a person; reminders belong to people",
            caller.principal
        ),
    ))
}

/// As [`company_error`], but an out-of-scope caller answers 403.
///
/// Every other engine refusal keeps `from_chiefd`'s 422: the request was well
/// formed and a product rule declined. This one is the case `RouteError`'s own
/// taxonomy separates out — the bar is the caller's IDENTITY, not the state of
/// the thing asked about — and a caller told 422 would go looking for what is
/// wrong with a body that is fine.
fn reminder_error(error: &chiefd_core::error::ChiefdError) -> RouteError {
    match error {
        chiefd_core::error::ChiefdError::Refused(refusal)
            if refusal.code == supervision::REMINDER_NOT_IN_SCOPE =>
        {
            RouteError::forbidden(refusal.code, refusal.message.clone())
        }
        other => company_error(other),
    }
}

async fn reminders_arm(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<ArmReminderRequest>,
) -> Result<Json<ReminderResponse>, RouteError> {
    let source = reminder_source(&supervision_live, &req.slug)?;
    let actor = reminder_actor(&caller)?;
    let request = supervision::ArmRequest {
        person_id: req.person_id,
        created_by_person_id: actor,
        prompt: req.prompt,
        interval_ms: req.interval_ms,
        recurring: req.recurring,
        expires_at: req.expires_at,
    };
    let reminder = source
        .company
        .mutate(MutationClass::Normal, MutationName("supervision.reminder_arm"), move |ledgers| {
            let manifest = organization::read(ledgers)?;
            supervision::arm_reminder(ledgers, &manifest, &request)
        })
        .await
        .map_err(|e| reminder_error(&e))?;
    // Wake the duty so it recomputes its alarm against the reminder that now
    // exists, instead of sleeping out the alarm it computed before. See
    // `SupervisionLiveSource::with_reminder_trigger` for why the mutation alone
    // does not do this.
    if let Some(trigger) = &source.reminder_trigger {
        trigger.notify_one();
    }
    Ok(Json(ReminderResponse { reminder }))
}

async fn reminders_list(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<ListRemindersRequest>,
) -> Result<Json<ListRemindersResponse>, RouteError> {
    let source = reminder_source(&supervision_live, &req.slug)?;
    let actor = reminder_actor(&caller)?;
    // Read-only, off the writer: `CompanyDb::read` never queues behind a
    // mutation, so listing reminders cannot be blocked by an in-flight commit.
    // The scope check runs inside the SAME snapshot the rows are read from, so
    // the manifest that authorized the read is the manifest the read saw.
    let reminders = source
        .company
        .read(|snapshot| {
            let ledgers = snapshot.ledgers();
            let manifest = organization::read(ledgers)?;
            supervision::ensure_reminder_scope(&manifest, &actor, &req.person_id)?;
            let ledger = supervision::read(ledgers, &manifest)?;
            Ok::<_, chiefd_core::error::ChiefdError>(supervision::list_reminders(
                &ledger,
                &req.person_id,
            ))
        })
        .map_err(|e| reminder_error(&e))?;
    Ok(Json(ListRemindersResponse { reminders }))
}

async fn reminders_stop(
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    Json(req): Json<StopReminderRequest>,
) -> Result<Json<ReminderResponse>, RouteError> {
    let source = reminder_source(&supervision_live, &req.slug)?;
    let actor = reminder_actor(&caller)?;
    let person_id = req.person_id;
    let reminder_id = req.reminder_id;
    let reminder = source
        .company
        .mutate(MutationClass::Normal, MutationName("supervision.reminder_stop"), move |ledgers| {
            let manifest = organization::read(ledgers)?;
            supervision::stop_reminder(ledgers, &manifest, &actor, &person_id, &reminder_id)
        })
        .await
        .map_err(|e| reminder_error(&e))?;
    // Stopping shortens nothing, but it can REMOVE the earliest alarm, so the
    // duty must recompute rather than wake at an instant nothing is due at.
    if let Some(trigger) = &source.reminder_trigger {
        trigger.notify_one();
    }
    Ok(Json(ReminderResponse { reminder }))
}

#[cfg(test)]
mod projection_reconcile_tests {
    use super::*;
    use chiefd_core::clock::SharedClock;
    use chiefd_core::test_support::ManualClock;
    use chiefd_host::proc::ProcReader;
    use chiefd_host::real::RealHostExecutor;

    fn skipped_note(reason: &str) -> String {
        format!("{SKIPPED_NOTE_PREFIX}{reason}")
    }

    /// A real, non-scripted executor.
    ///
    /// The long note that used to sit here explained why a *scripted terminal*
    /// was required: `reconcile_cycle` drove `observe()` against a runner, and
    /// the store-layer `FakeHostExecutor` answered exit 0 to every command
    /// including `has-session`, which contradicted its own "provably absent"
    /// audit default. Neither hazard exists: `reconcile_cycle` takes no
    /// executor at all (#751/P8-P10) and reads the actuator's committed
    /// observation instead, so there is no terminal to script and no
    /// contradiction to script it out of. This executor is here only because
    /// `SupervisionLiveSource` still carries one for the routes that spawn
    /// workers and probe providers.
    fn plain_host() -> RealHostExecutor {
        RealHostExecutor::new(ProcReader::default())
    }

    #[test]
    fn skipped_from_report_reads_the_skip_note_back_out() {
        let report = chiefd_core::runtime::duty_hooks::ReconcileReport {
            notes: vec![skipped_note("FloorNotElapsed")],
            ..Default::default()
        };
        let skipped = skipped_from_report(&report).expect("skip note parsed");
        assert_eq!(skipped.reason, "FloorNotElapsed");
    }

    #[test]
    fn skipped_from_report_is_none_for_a_completed_pass() {
        let report = chiefd_core::runtime::duty_hooks::ReconcileReport {
            applied: true,
            desired_people: 2,
            notes: vec!["converged".to_string()],
            ..Default::default()
        };
        assert!(
            skipped_from_report(&report).is_none(),
            "a real pass must never be misread as a skip"
        );
    }

    /// `reconcile_cycle` refuses a company with no organization manifest
    /// (`unknown-company`) -- an empty `CompanyDb` is not "idle", it is
    /// unprovisioned, and the route correctly treats the two differently.
    /// Seeds the same `northstar_manifest` fixture `converge_apply::cycle`'s
    /// own tests use, UNMODIFIED: the fixture's `runtime_session` field
    /// ("org-northstar-conformance") is set independently of `slug`, not
    /// derived from it, so retargeting `slug` alone (an earlier version of
    /// this helper tried exactly that) leaves the two disagreeing and
    /// `reconcile_cycle`'s ownership-tag check refuses the mismatch. `slug`
    /// is therefore fixed at the fixture's own value, not a parameter.
    async fn fresh_company(dir: &tempfile::TempDir) -> Arc<CompanyDb> {
        let slug = "northstar-conformance";
        let company_path = dir.path().join(format!("{slug}.chief.db"));
        let clock: SharedClock = Arc::new(ManualClock::default());
        let company =
            Arc::new(CompanyDb::open(slug, &company_path, clock).expect("open company writer"));
        let manifest = chiefd_core::test_support::northstar_manifest(0);
        company
            .mutate(
                chiefd_core::actor::MutationClass::Normal,
                chiefd_core::actor::MutationName("test.seed"),
                move |ledgers| {
                    chiefd_core::store::organization::create(ledgers, &manifest)?;
                    chiefd_core::store::supervision::seed(ledgers, &manifest)?;
                    chiefd_core::store::activity::seed(ledgers, &manifest)?;
                    Ok(())
                },
            )
            .await
            .expect("seed manifest/supervision/activity");
        company
    }

    fn test_actuator_config(
        dir: &tempfile::TempDir,
    ) -> chiefd_host::converge_apply::ActuatorConfig {
        chiefd_host::converge_apply::ActuatorConfig {
            socket: "chiefd-test-socket".to_string(),
            // "watching for ever": the epoch, so an inferred quiet instant is
            // clamped by nothing and every expectation here is the pre-clamp one.
            watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
            dir: dir.path().to_path_buf(),
            home: dir.path().to_path_buf(),
            pi_binary: dir.path().join("pi"),
            // Zero, not RECONCILE_FLOOR's production 5s: the floor and the
            // single-flight claim are two SEPARATE gates in begin_cycle, and
            // this module's sequential test needs the second call to be
            // uncontended once the first releases its claim, not skipped by
            // an unrelated minimum-interval timer. Zero makes the floor
            // check trivially pass, leaving the claim -- the thing these
            // tests actually exercise -- as the only gate in effect. This is
            // a deterministic property of the input, not a sleep/retry/
            // longer-timeout workaround for a race.
            floor: std::time::Duration::ZERO,
            launcher_root: dir.path().to_path_buf(),
            root_pi_agent_dir: dir.path().join("pi-agent"),
        }
    }

    fn wired_source(
        company: Arc<CompanyDb>,
        slug: &str,
        dir: &tempfile::TempDir,
    ) -> SupervisionLiveSource {
        SupervisionLiveSource::new(company, slug.to_string())
            .with_host_executor(Arc::new(plain_host()))
            .with_reconcile_actuator_config(test_actuator_config(dir))
    }

    /// A LAUNCHER CHECKOUT with the extension SOURCE present but no built
    /// runtime, as `chiefd_host::files::publish_atomically` seeds it. This is
    /// the QA fixture in miniature: `packages/piing/extensions/*.ts` are real,
    /// readable files; `packages/piing/dist/extensionruntime/index.js` — the
    /// build product every extension imports — is absent.
    fn seed_extension_sources(launcher_root: &std::path::Path) {
        let exts = launcher_root.join("packages").join("piing").join("extensions");
        // Read from the one home for the list rather than repeating it: a
        // hardcoded copy here silently seeds an INCOMPLETE checkout the moment
        // an extension is added, and the failure lands as "this daemon could
        // not read its launcher extension source" — a refusal about the
        // product, from a defect in the fixture.
        for name in chiefd_host::materialize::ORGANIZATION_EXTENSION_FILES {
            chiefd_host::files::publish_atomically(
                &exts.join(name),
                "import { OrganizationTools } from \"@chief/piing/extension-runtime\";\n",
                0o644,
            )
            .expect("seed a readable extension source");
        }
        // AND THE NAME THOSE SOURCES IMPORT THE RUNTIME BY. A checkout gets
        // this from `bun install`'s workspace link and an install from the
        // release's packaged shim; a fixture that seeds neither is a checkout
        // whose extensions cannot load, which the launch probe now refuses --
        // correctly, and for a reason that is about the fixture rather than
        // the product.
        // BOTH packages: `team-ui.ts` imports `@chief/chiefing/extension-runtime`
        // and the launch probe checks both, so seeding one would make this
        // fixture a checkout the product refuses -- the fixture lying in its
        // own favour.
        for package in ["piing", "chiefing"] {
            std::fs::create_dir_all(
                launcher_root.join("node_modules").join("@chief").join(package),
            )
            .expect("seed the package identity the extensions import by");
        }
    }

    async fn desired_against(
        company: Arc<CompanyDb>,
        dir: &tempfile::TempDir,
        launcher_root: std::path::PathBuf,
    ) -> Result<
        axum::Json<chiefd_core::runtime::actuation::DesiredRuntime>,
        crate::docstore::org_slice::Refused,
    > {
        let mut cfg = test_actuator_config(dir);
        cfg.launcher_root = launcher_root;
        let source = SupervisionLiveSource::new(company, "northstar-conformance".to_string())
            .with_host_executor(Arc::new(plain_host()))
            .with_reconcile_actuator_config(cfg);
        crate::docstore::desired::org_runtime_desired(
            Extension(Some(source)),
            Json(crate::docstore::org_slice::SlugRequest {
                slug: "northstar-conformance".to_string(),
            }),
        )
        .await
    }

    /// THE REGRESSION. An UNBUILT launcher checkout must refuse the desired set
    /// by its REAL reason — the build never ran — and never by the fixed
    /// `extension-source-unreadable` label a `.map_err(|_| ...)` discard used to
    /// stamp over every launcher-assets failure.
    ///
    /// This is the test whose absence let `cargo test --workspace` pass 0
    /// failures while a fresh company's CEO would not boot: nothing drove
    /// `/v1/org/runtime/desired` all the way through `launcher_assets` against a
    /// checkout that is present and readable but not built. The extension source
    /// here is PLAINLY readable — three real `.ts` files — so a refusal that
    /// says the source could not be READ is a lie, and an operator reading it
    /// hunts the wrong thing (as one did, for a whole session).
    #[tokio::test]
    async fn desired_names_an_unbuilt_launcher_by_its_real_reason_not_extension_source_unreadable()
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = fresh_company(&dir).await;
        let launcher = dir.path().join("unbuilt-checkout");
        seed_extension_sources(&launcher);
        // Deliberately NO packages/piing/dist/extensionruntime/index.js.

        let error = desired_against(company, &dir, launcher)
            .await
            .expect_err("an unbuilt checkout cannot derive a launch hash and must refuse");

        assert_eq!(
            error.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "the fault is fixable — build the checkout — so it is 503, not 500"
        );
        assert_ne!(
            error.code(),
            "extension-source-unreadable",
            "the extension SOURCE was present and readable; labelling this 'unreadable' \
             discards the real cause and sends the operator hunting a file that is fine: {}",
            error.detail(),
        );
        assert_eq!(
            error.code(),
            "launcher-root-unbuilt",
            "the desired route must carry the launcher's own refusal: {}",
            error.detail(),
        );
        assert!(
            error.detail().contains("never built") || error.detail().contains("was never built"),
            "the refusal must tell the operator the checkout was not built: {}",
            error.detail(),
        );
    }

    /// A root that is not a launcher checkout at all surfaces `launcher-root-\
    /// unusable`, again NOT `extension-source-unreadable`. Same discard, same
    /// cure: the desired route carries the launcher's own code.
    #[tokio::test]
    async fn desired_names_a_non_checkout_root_as_unusable_not_unreadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = fresh_company(&dir).await;
        // A directory with no packages/piing/extensions under it at all.
        let launcher = dir.path().join("not-a-checkout");
        std::fs::create_dir_all(&launcher).expect("bare directory");

        let error = desired_against(company, &dir, launcher)
            .await
            .expect_err("a root that is not a checkout must refuse");

        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code(), "launcher-root-unusable", "detail: {}", error.detail(),);
    }

    /// The other half of the probe: the SAME checkout, BUILT, is accepted and
    /// derives a digest. Without this the refusal above could be refusing the
    /// checkout rather than the missing build.
    #[tokio::test]
    async fn desired_accepts_a_built_launcher_checkout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = fresh_company(&dir).await;
        let launcher = dir.path().join("built-checkout");
        seed_extension_sources(&launcher);
        chiefd_host::files::publish_atomically(
            &launcher
                .join("packages")
                .join("piing")
                .join("dist")
                .join("extensionruntime")
                .join("index.js"),
            "export const organizationTools = [];\n",
            0o644,
        )
        .expect("the built extension runtime");

        let desired = desired_against(company, &dir, launcher)
            .await
            .expect("a present, readable, built checkout derives a launch hash");
        assert_eq!(desired.0.company, "northstar-conformance");
    }

    fn request(slug: &str, correlation_id: &str) -> OrgProjectionReconcileRequest {
        OrgProjectionReconcileRequest {
            slug: slug.to_string(),
            correlation_id: correlation_id.to_string(),
        }
    }

    #[tokio::test]
    async fn a_request_naming_an_unknown_slug_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = fresh_company(&dir).await;
        let source = wired_source(company, "northstar-conformance", &dir);

        let result = org_projection_reconcile(
            Extension(Some(source)),
            Json(request("some-other-company", "corr-1")),
        )
        .await;

        let error = result.expect_err("foreign slug must not resolve to this live source");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_source_with_no_actuator_config_wired_is_service_unavailable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = fresh_company(&dir).await;
        // Deliberately NOT calling with_reconcile_actuator_config -- matches
        // every standalone/migration router in production, which has no
        // daemon-owned data root to converge against. (#751/P8 removed the host
        // executor from this route's requirements: a converge pass applies
        // nothing, so there is no machine for it to need.)
        let source = SupervisionLiveSource::new(company, "northstar-conformance".to_string());

        let result = org_projection_reconcile(
            Extension(Some(source)),
            Json(request("northstar-conformance", "corr-1")),
        )
        .await;

        let error = result.expect_err("no actuator config means the route cannot run");
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_single_request_against_an_idle_company_runs_and_is_never_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = fresh_company(&dir).await;
        let source = wired_source(company, "northstar-conformance", &dir);

        let response = org_projection_reconcile(
            Extension(Some(source)),
            Json(request("northstar-conformance", "corr-1")),
        )
        .await
        .expect("no in-flight cycle contends, so this must run");

        assert_eq!(response.correlation_id, "corr-1");
        assert!(response.skipped.is_none(), "an uncontended pass must not report a skip");
    }

    /// The core serialization guarantee this route relies on, exercised
    /// end-to-end through the route rather than only at `reconcile_cycle`'s
    /// own layer: two concurrent requests for the SAME company contend for
    /// `begin_cycle`'s existing single-flight claim, so exactly one of them
    /// must observe the other's in-flight pass as a skip. This is the same
    /// claim the async duty loop already depends on -- the route adds no new
    /// lock, so this test is really asserting `begin_cycle` still behaves
    /// the way the plan's design relies on when reached through HTTP-shaped
    /// concurrent callers instead of the duty loop's own interval ticks.
    ///
    /// This is ALSO the producer-to-parser coupling test named in the
    /// comment at `SKIPPED_NOTE_PREFIX`: `skipped_from_report`'s two unit
    /// tests above assert against a literal fixture, which pins the parser
    /// to itself, not to `reconcile_cycle`. Only a real skip, driven through
    /// a real `reconcile_cycle` call and read back by the SAME parser the
    /// route uses, can catch a drift between the note `reconcile_cycle`
    /// actually emits and the prefix/format this router expects -- so this
    /// test asserts on the parsed `reason` itself, not just
    /// `skipped.is_some()`.
    #[tokio::test]
    async fn two_concurrent_requests_for_the_same_company_serialize_to_one_skip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = fresh_company(&dir).await;
        let source_a = wired_source(Arc::clone(&company), "northstar-conformance", &dir);
        let source_b = wired_source(company, "northstar-conformance", &dir);

        let (result_a, result_b) = tokio::join!(
            org_projection_reconcile(
                Extension(Some(source_a)),
                Json(request("northstar-conformance", "corr-a")),
            ),
            org_projection_reconcile(
                Extension(Some(source_b)),
                Json(request("northstar-conformance", "corr-b")),
            ),
        );

        let response_a = result_a.expect("request A completes with a report, skipped or not").0;
        let response_b = result_b.expect("request B completes with a report, skipped or not").0;

        let skipped_responses: Vec<&OrgProjectionReconcileResponse> =
            [&response_a, &response_b].into_iter().filter(|r| r.skipped.is_some()).collect();
        assert_eq!(
            skipped_responses.len(),
            1,
            "exactly one concurrent request must observe the other's in-flight claim as a skip \
             (begin_cycle's single-flight guarantee, reused unmodified by this route): got A \
             skipped={:?}, B skipped={:?}",
            response_a.skipped,
            response_b.skipped
        );

        // The producer-to-parser assertion: neither company has run a prior
        // cycle yet, so the collision is caught by the single-flight claim
        // itself (`SkipReason::AlreadyRunning`), not the floor timer
        // (`FloorNotElapsed`, which needs a PRIOR cycle start to measure
        // from). A real `reconcile_cycle` call produced this string; if its
        // wording ever changes, this assertion -- not the literal-fixture
        // tests above -- is what catches it.
        let reason = &skipped_responses[0].skipped.as_ref().expect("checked above").reason;
        assert_eq!(
            reason, "AlreadyRunning",
            "the skipped response's reason must be the real SkipReason debug string \
             reconcile_cycle produced, parsed back out by skipped_from_report -- not an \
             empty/placeholder value"
        );
    }

    /// `correlation_id` is echoed, never used to dedupe or merge calls --
    /// two SEQUENTIAL requests (no contention: the first has fully released
    /// its claim before the second starts) with the identical id must both
    /// run their own independent pass rather than the second short-circuiting
    /// off the first's cached result.
    #[tokio::test]
    async fn same_correlation_id_on_two_sequential_calls_runs_independently_each_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let company = fresh_company(&dir).await;

        let first = org_projection_reconcile(
            Extension(Some(wired_source(Arc::clone(&company), "northstar-conformance", &dir))),
            Json(request("northstar-conformance", "same-id")),
        )
        .await
        .expect("first call runs uncontended");
        assert!(first.skipped.is_none());

        let second = org_projection_reconcile(
            Extension(Some(wired_source(company, "northstar-conformance", &dir))),
            Json(request("northstar-conformance", "same-id")),
        )
        .await
        .expect("second call, after the first released its claim, also runs uncontended");
        assert!(
            second.skipped.is_none(),
            "an identical correlation_id must not make the second call look like a duplicate \
             the router silently skips -- both are honest, independent passes"
        );
        assert_eq!(second.correlation_id, "same-id");
    }
}

#[cfg(test)]
mod department_create_minting_tests {
    //! #751/R3: the ids and seed defaults a department create needs are
    //! CHIEFD's to decide, not its caller's.
    //!
    //! They used to be minted in TypeScript by `planDepartmentCreate`, so every
    //! caller carried a second opinion about what a department is called and
    //! the two drifted the moment either changed. These pin that the route's
    //! minting reproduces `organization_spec`'s rules exactly — a department
    //! created through this route is named the same as one created at genesis.

    use super::{mint_department_create_ids, OrgDepartmentCreateRequest};

    fn request(json: serde_json::Value) -> OrgDepartmentCreateRequest {
        serde_json::from_value(json).expect("a valid create request")
    }

    fn hire_new_head(name: &str) -> serde_json::Value {
        serde_json::json!({
            "kind": "hire-new", "personId": "", "name": name,
            "mandate": "Own the department's delivery.",
        })
    }

    #[test]
    fn a_blank_department_id_is_minted_from_the_name_under_its_parent() {
        // A child of the root keeps the bare local id...
        let mut root_child = request(serde_json::json!({
            "slug": "acme@0", "requester": {"kind": "person", "personId": "chief"},
            "departmentId": "", "parentId": "executive", "name": "Growth Engineering",
            "head": hire_new_head("Dana"),
        }));
        mint_department_create_ids(&mut root_child).expect("mint");
        assert_eq!(root_child.department_id, "growth-engineering");

        // ...and a nested one is `<parent>-<local>`, so ids stay globally
        // unique and readable without a lookup.
        let mut nested = request(serde_json::json!({
            "slug": "acme@0", "requester": {"kind": "person", "personId": "chief"},
            "departmentId": "", "parentId": "growth-engineering", "name": "Platform",
            "head": hire_new_head("Dana"),
        }));
        mint_department_create_ids(&mut nested).expect("mint");
        assert_eq!(nested.department_id, "growth-engineering-platform");
    }

    #[test]
    fn a_supplied_department_id_is_never_rewritten() {
        let mut req = request(serde_json::json!({
            "slug": "acme@0", "requester": {"kind": "person", "personId": "chief"},
            "departmentId": "chosen-by-the-caller", "parentId": "executive", "name": "Growth",
            "head": hire_new_head("Dana"),
        }));
        mint_department_create_ids(&mut req).expect("mint");
        assert_eq!(req.department_id, "chosen-by-the-caller");
    }

    #[test]
    fn head_and_staff_identities_and_titles_are_minted_the_way_genesis_mints_them() {
        let mut req = request(serde_json::json!({
            "slug": "acme@0", "requester": {"kind": "person", "personId": "chief"},
            "departmentId": "", "parentId": "executive", "name": "Growth Engineering",
            "head": hire_new_head("Dana Rivers"),
            "staff": [
                {"kind": "hire-new", "personId": "", "name": "Sam Ito", "mandate": "Ship the funnel."},
                {"kind": "hire-new", "personId": "kept-as-given", "name": "Lee Park",
                 "title": "Staff Engineer", "mandate": "Own retention."}
            ],
        }));
        mint_department_create_ids(&mut req).expect("mint");
        assert_eq!(req.head.person_id, "growth-engineering-head");
        assert_eq!(req.head.title.as_deref(), Some("Head of Growth Engineering"));
        assert_eq!(req.head.person_kind.as_deref(), Some("head"));
        assert_eq!(req.staff[0].person_id, "growth-engineering-sam-ito");
        assert_eq!(req.staff[0].title.as_deref(), Some("Sam Ito"));
        assert_eq!(req.staff[0].person_kind.as_deref(), Some("worker"));
        // A caller who DID decide keeps every value it decided.
        assert_eq!(req.staff[1].person_id, "kept-as-given");
        assert_eq!(req.staff[1].title.as_deref(), Some("Staff Engineer"));
    }

    #[test]
    fn appoint_existing_still_requires_the_person_it_names() {
        // Minting an id here would invent a person rather than appoint one, so
        // a blank id is a caller error and must stay one.
        let mut req = request(serde_json::json!({
            "slug": "acme@0", "requester": {"kind": "person", "personId": "chief"},
            "departmentId": "", "parentId": "executive", "name": "Growth",
            "head": {"kind": "appoint-existing", "personId": ""},
        }));
        let error = mint_department_create_ids(&mut req).expect_err("a blank id must refuse");
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_name_that_produces_no_usable_id_is_refused_rather_than_guessed() {
        let mut req = request(serde_json::json!({
            "slug": "acme@0", "requester": {"kind": "person", "personId": "chief"},
            "departmentId": "", "parentId": "executive", "name": "---",
            "head": hire_new_head("Dana"),
        }));
        let error = mint_department_create_ids(&mut req).expect_err("no usable id must refuse");
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    }
}

#[cfg(test)]
mod wake_enumeration_tests {
    //! Arch-audit Step 6 (findings F1/F9): the reactive wake is the DEFAULT for
    //! every publish of a row the reconcile cycle can read — opt-out requires a
    //! stated reason below, not opt-in. Modeled on chiefd-core's
    //! `busy_is_mintable_from_exactly_the_two_reviewed_wait_sites`: a curated
    //! enumeration over the production half of this very file, so a new route
    //! that mutates reconcile inputs without a wake (or a silent removal of
    //! one) fails here and has to argue for itself in review.
    //!
    //! Three route-wiring shapes are pinned (F1 refinement):
    //!   1. `direct_org_row_route_pair!` macro rows — wake is the macro
    //!      default; `no_reconcile_wake` is the only opt-out and there are
    //!      currently ZERO approved users of it.
    //!   2. Hand-written `*_clear` handlers.
    //!   3. Hand-written org_ops verb handlers — the WHOLE family, not just
    //!      the reparent/transfer pair the audit sampled: shutdown, hire,
    //!      offboard, bench and the rest mutate the same manifest/supervision
    //!      rows the cycle reads.
    //!
    //! Plus the remaining hand-written row publishes/inserts on the
    //! supervision-live surface (manifest genesis, escalation-intents). The
    //! contracts, activity, supervision and mailbox publishes that used to sit
    //! beside them are deleted — the publisher-route sweep found no caller.

    /// The file under test with every `#[cfg(test)]` module cut out: a test is
    /// *supposed* to name wake internals; this guard is about production code.
    /// Test modules are not all at the end of this file (`watch_tests` sits in
    /// the middle), so each one is excised individually — from the attribute
    /// to the module's closing brace at column zero (its contents are always
    /// indented).
    fn production_source() -> String {
        // ALL THREE files, because the supervision-live handler surface spans
        // them. Scanning only `router.rs` would let a mutating handler in
        // `org_slice.rs` be silently exempt from
        // the wake guard -- a guard that passes because it cannot see its
        // subject, which is the single most expensive class of mistake in this
        // repo. A new module holding supervision-live handlers belongs in this
        // list in the same commit that creates it.
        let mut out = String::new();
        for full in [include_str!("router.rs"), include_str!("org_slice.rs")] {
            let mut rest = full;
            while let Some(start) = rest.find("#[cfg(test)]") {
                out.push_str(&rest[..start]);
                let tail = &rest[start..];
                let end = tail.find("\n}\n").map(|i| i + 3).unwrap_or(tail.len());
                rest = &tail[end..];
            }
            out.push_str(rest);
            out.push('\n');
        }
        out
    }

    #[test]
    fn durable_materialization_routes_are_absent() {
        let source = production_source();
        assert!(!source.contains("/v1/org/materialization/"));
        assert!(!source.contains("fn org_materialization_"));
    }

    /// Slice one hand-written handler body: from `async fn <name>(` to the
    /// first closing brace at column zero (inner blocks are always indented).
    fn handler_body<'a>(source: &'a str, name: &str) -> &'a str {
        let marker = format!("async fn {name}(");
        let start = source
            .find(&marker)
            .unwrap_or_else(|| panic!("handler `{name}` not found in router.rs or org_slice.rs"));
        let rest = &source[start..];
        let end = rest.find("\n}\n").map(|i| i + 3).unwrap_or(rest.len());
        &rest[..end]
    }

    /// GENESIS ASKS FOR THE CEO, and a company created without an attach is
    /// therefore born with somebody to run.
    ///
    /// # The failure this pins
    ///
    /// A company created through `chiefd_launch_company` came up EMPTY: the
    /// actuator logged `requested=0 applied=0` round after round, no tmux
    /// session ever appeared, and the Founder's card nonetheless reported the
    /// CEO booted. chiefd was actuating correctly — it was actuating an empty
    /// desired set, because nothing had asked for anybody.
    ///
    /// #1148 deleted the root's unconditional `ActivityReason::OrganizationRoot`
    /// lease so the CEO settles like everybody else, which is the operator's
    /// ruling and stands. That lease was also the only thing that made a CEO
    /// run at all, so its own commit message required that whatever brings the
    /// root back must SUPPLY DEMAND rather than re-exempt the CEO. #1149 gave
    /// the operator's ARRIVAL that meaning; genesis had no equivalent moment,
    /// and the Founder's launch tool never attaches.
    ///
    /// So creating a company is itself the durable decision that its CEO should
    /// be running. This guard is on the SOURCE because the alternative — a live
    /// genesis against a real daemon — is exactly the integration the launch
    /// tool already performs, and what regressed was not the mechanism but its
    /// ABSENCE. A deleted line is invisible to every test that exercises a line
    /// that is there.
    #[test]
    fn genesis_records_the_ceos_start_decision_so_a_created_company_has_demand() {
        let source = production_source();
        let body = handler_body(&source, "org_manifest_genesis");
        assert!(
            body.contains("prepare_ceo_only("),
            "genesis must ask for the CEO, or a company created without an \
             attach converges to an empty desired set forever"
        );
        // The second arm of this assertion held `org_runtime_prepare_ceo_only`
        // to the same call, so `attach` and genesis could not drift into two
        // definitions of "the CEO has been asked for". chief-home-is-cwd §4c
        // deleted that route with the rest of the daemon-side CEO boot, which
        // makes genesis the SOLE caller — there is nothing left to drift from.
    }

    /// And a refusal from that call must NOT fail genesis. The company is
    /// committed by the time it runs; answering an error would tell the caller
    /// their company does not exist when it does, and invite the retry that
    /// earns `already-exists`.
    #[test]
    fn a_refused_ceo_start_decision_does_not_fail_a_committed_genesis() {
        let source = production_source();
        let body = handler_body(&source, "org_manifest_genesis");
        let call = body.split_once("prepare_ceo_only(").expect("genesis asks for the CEO").0;
        assert!(
            call.trim_end().ends_with("if let Err(error) = source.company")
                || call.contains("if let Err(error) = source.company"),
            "the start decision must be attempted and its refusal logged, never \
             propagated with `?` out of a committed genesis"
        );
    }

    /// Hand-written handlers whose committed-success path MUST wake. A new
    /// mutating handler on the supervision-live surface belongs here; adding
    /// it anywhere else leaves the wake unwired.
    const WIRED_HANDLERS: &[&str] = &[
        // Whole-row publishes. The publisher-route sweep deleted every other
        // member of this group: person-contracts, activity, supervision and
        // mailbox all published a whole document for a caller that did not
        // exist, so there is no handler left to wake for.
        "org_manifest_genesis",
        // A mailbox delta is a reconcile input and was in NEITHER list, which
        // is how the upward-wake defect survived: `project_activity_fence`
        // reads pending mailbox rows and grants launch intent to exactly their
        // recipients, so a committed delivery changes what the next pass
        // decides — and nothing told the pass to run. The intercom papered over
        // it with a company-wide `/v1/org/runtime/launch`, which no subordinate
        // may post.
        "org_mailbox_delta",
        // Semantic inserts.
        "org_operator_escalation_intents_insert",
        // Shape 2: the *_clear handlers.
        "org_launch_intent_clear",
        "org_runtime_clear",
        // The activity/staffing/units/people port (#751): mutating handlers in
        // `docstore::org_slice`. They wake inside their own bodies rather than
        // at the route, so they are enumerated here exactly like the ones
        // above -- being in a sibling module is not an exemption.
        "org_staffing_lifecycle",
        // Shape 3: the org_ops verb family.
        "org_person_shutdown",
        "org_person_start",
        // The rail's click-to-wake. It commits the launch-intent grant AND
        // releases the lapsed idle park, which are both reconcile inputs, so a
        // wake that did not nudge the pass would leave the operator waiting out
        // the cadence for the pane they just asked for.
        "org_person_wake",
        "org_person_appoint_head",
        "org_department_create",
        "org_department_reparent",
        "org_person_transfer",
        "org_department_move_members",
        "org_person_offboard",
        "org_person_hire",
        "org_department_pause",
        "org_department_resume",
        "org_department_resume_many",
        "org_person_bench",
        "org_person_bench_lifecycle",
        "org_person_recall",
        "org_person_replace_head_and_offboard",
        "org_department_reactivate_executive_root",
        "org_department_remove_tree",
        // Shape 4: the supervision & session-lifecycle verbs. Every one of
        // these changes something the reconcile cycle reads — a maintenance
        // request's claim, a folded goal, the clean-session epoch — so every
        // one wakes.
        "org_session_maintenance_queue",
        "org_session_maintenance_start",
        "org_session_maintenance_defer",
        "org_session_maintenance_interrupt",
        "org_session_maintenance_recover",
        "org_session_maintenance_finish",
        "org_session_maintenance_reconcile_parked",
        "org_operator_escalation_drain",
        "org_session_epoch_stamp",
    ];

    /// Deliberate opt-outs, each with its stated reason. Adding a row here is
    /// the sanctioned way to skip the wake — the reason is the review surface.
    const OPTED_OUT: &[(&str, &str)] = &[
        // Exactly-once event markers: DocStore-direct with no live-company
        // gate, never read by the reconcile cycle.
        ("org_event_journal_insert", "exactly-once markers; not a reconcile input"),
        ("org_event_journal_prune", "exactly-once markers; not a reconcile input"),
        // The human doorbell. Both handlers write the operator-escalation-push
        // singleton, and that row is read by nothing in the reconcile cycle —
        // it is delivery bookkeeping for an operator notification, not an input to
        // any projection. Waking here would cost a pass per notification
        // attempt and change nothing.
        (
            "org_operator_escalation_doorbell_plan",
            "doorbell bookkeeping; not a reconcile input",
        ),
        (
            "org_operator_escalation_doorbell_settle",
            "doorbell bookkeeping; not a reconcile input",
        ),
        // The activity/staffing/units/people port (#751). Each of these
        // mutates rows, and none of them changes an input the reconcile cycle
        // reads to decide pane placement:
        (
            "org_person_contracts_build",
            "contract TEXT; the boot path projects it to AGENTS.md, the reconcile cycle never reads it",
        ),
    ];
    // reminders_arm / reminders_stop are in neither list on purpose: they wake
    // the SEPARATE `reminder_trigger` (their own duty's recompute signal) and
    // are not reconcile inputs.
    //
    // org_projection_reconcile (#739 P2) is in neither list for a different
    // reason than either bucket above: it is not a row publish waiting on the
    // async duty loop to notice and wake into a pass -- it calls
    // `reconcile_cycle` itself, synchronously, on the request task. There is
    // no separate wake to fire because the noticing already happened inline.

    #[test]
    fn macro_publish_arm_wakes_by_default() {
        let source = production_source();
        assert!(
            source.contains(
                "let wake_opted_out = false $(|| { let _ = stringify!($no_wake); true })?;"
            ),
            "the macro must wake by DEFAULT, with `no_reconcile_wake` as the only opt-out"
        );
        assert!(
            source.contains("fn wake_reconcile(source: &SupervisionLiveSource)"),
            "the wake_reconcile helper must exist"
        );
    }

    #[test]
    fn every_macro_row_wakes() {
        let source = production_source();
        let rows: Vec<&str> =
            source.lines().filter(|line| line.starts_with("direct_org_row_route_pair!(")).collect();
        // 17 since #751 deleted the removal-state crash journal: the
        // `org_removal_state_read`/`org_removal_state_publish` pair went with
        // the store it fronted (three tables, the row module, both CompanyDb
        // methods and the two routes). `remove_department_tree` is ONE
        // transaction in Rust, so the half-state that tombstone recorded is
        // unrepresentable and nothing writes the store — Mandate 0 residue,
        // removed whole rather than kept warm. Nothing wakes for it because
        // there is no longer a row to publish.
        //
        // 15 since the supervisor-state and supervisor-armed-intent pairs went
        // the same way: both described the detached org-supervisor PROCESS
        // (socket/token/pid/process_start), which #825 retired and 5681617a4
        // deleted the writer for. Same reasoning as removal-state — there is no
        // longer a row to publish, so there is nothing to wake for.
        //
        // 14 since the retired messaging channel's gateway pair went with it:
        // the store, its four tables, its row module and both CompanyDb
        // methods are deleted, so once again there is no row left to publish
        // and nothing to wake for.
        //
        // 13 since the acknowledgement-receipt queue went the same way: the
        // ack machinery is deleted, so there is no `acks` row to publish.
        //
        // 12 since the goal-intent queue went with the goal feature. Note what
        // did NOT go with it: `goal-delivery-quiesce` is named for a goal but
        // is the converge cycle's delivery-quiescence stamp, written by
        // `runtime_lifecycle.rs` and read by `converge_apply/cycle.rs`, and its
        // read route is still here.
        //
        // 1 since the publisher-route sweep. Eleven of the twelve rows had a
        // publish half nobody called: the row is written in-process through
        // `CompanyDb` inside the daemon's own transactions, and the HTTP door
        // beside it was a second entrance with nobody behind it. Those eleven
        // are now `direct_org_row_read_route!` rows, which have no publish arm
        // and therefore nothing to wake. `runtime` keeps its pair because
        // `packages/piing`'s tool-contract suite seeds through it.
        assert_eq!(
            rows.len(),
            1,
            "the macro-row inventory changed; review each row for the wake and update this pin: {rows:#?}"
        );
        for row in rows {
            assert!(
                !row.contains("no_reconcile_wake"),
                "macro row opted out of the reconcile wake with no reviewed reason: {row}"
            );
        }
    }

    #[test]
    fn every_reconcile_input_publish_wakes() {
        let source = production_source();
        for name in WIRED_HANDLERS {
            let body = handler_body(&source, name);
            // `org_goals_clear` binds `source` by reference, so clippy rewrites
            // its call to `wake_reconcile(source);` — both spellings wake.
            assert!(
                body.contains("wake_reconcile(&source);")
                    || body.contains("wake_reconcile(source);"),
                "{name} mutates rows the reconcile cycle reads but never wakes \
                 (arch-audit F1): add `wake_reconcile(&source);` on its \
                 committed-success path, or move it to OPTED_OUT with a reason"
            );
        }
    }

    #[test]
    fn opt_outs_stay_opted_out_with_reasons() {
        let source = production_source();
        for (name, reason) in OPTED_OUT {
            assert!(!reason.is_empty(), "{name} needs a stated reason");
            let body = handler_body(&source, name);
            assert!(
                !body.contains("wake_reconcile"),
                "{name} is listed as opted out but now wakes — move it to WIRED_HANDLERS"
            );
        }
    }

    /// Every hand-written handler that can bring a NEW person into the roster.
    ///
    /// A person row without an agent home is a person the actuator refuses on
    /// every pass forever, so creating one and writing its home are the same
    /// operation. `org_runtime_prepare_ceo_only` was a third member until
    /// chief-home-is-cwd §4c deleted the daemon-side CEO boot with its route;
    /// it is where this was fixed the first time, for the CEO only.
    const PERSON_CREATING_HANDLERS: &[&str] = &["org_person_hire", "org_department_create"];

    /// THE regression guard for a person the actuator refuses for ever.
    ///
    /// Hiring committed rows and woke the reconciler, and the converge cycle
    /// never writes a home — so a person hired after genesis had none and the
    /// actuator aborted step 0 on every pass. It is retargeted at
    /// `ensure_agent_home`'s route-side caller rather than at the deleted
    /// materializer: the defect is identical and only the name of the fix moved.
    #[test]
    fn every_person_creating_route_writes_the_home_for_the_person_it_just_added() {
        let source = production_source();
        for name in PERSON_CREATING_HANDLERS {
            let body = handler_body(&source, name);
            assert!(
                body.contains("ensure_committed_agent_homes(")
                    || body.contains("materialize_after_commit("),
                "{name} can add a person to the roster but never gives them a home. A person \
                 row with no agent home is a person the actuator refuses for ever (\"this \
                 person has no agent home\"): call \
                 `materialize_after_commit(&source, now_iso()).await` on its \
                 committed-success path, before the wake"
            );
        }
    }

    /// Starting somebody promises a pane. This route used to make that promise
    /// unconditionally — it committed `active`, a launch fence and durable
    /// demand and answered `{"applied": true}` for a person the actuator could
    /// not spawn, which the CEO's tool rendered as
    /// `✅ Started @<id> · only this person was launched` while the roster
    /// showed `active · recovering · no live pane observed` forever.
    #[test]
    fn starting_a_person_asks_whether_the_actuator_could_spawn_them_before_answering() {
        let source = production_source();
        let body = handler_body(&source, "org_person_start");
        assert!(
            body.contains("launch_refusal_for("),
            "org_person_start answers success without asking whether the actuator would \
             refuse: consult `launch_refusal_for` and return the real reason instead"
        );
        // And it must ask BEFORE it writes, so a refusal leaves the roster
        // untouched rather than stranding a person `active` with no pane.
        let asks = body.find("launch_refusal_for(").expect("asks");
        let writes = body.find("start_person(").expect("writes");
        assert!(
            asks < writes,
            "org_person_start writes durable demand before checking launchability; a refusal \
             must commit nothing"
        );
    }

    #[test]
    fn wired_and_opted_out_sets_are_disjoint_and_complete_against_clear_handlers() {
        assert!(
            WIRED_HANDLERS.iter().all(|name| !OPTED_OUT.iter().any(|(o, _)| o == name)),
            "a handler appears in both WIRED_HANDLERS and OPTED_OUT"
        );
        // Every *_clear handler on the supervision-live surface is enumerated
        // in one set or the other (F1's seven were the wake-less class).
        let source = production_source();
        for name in ["org_launch_intent_clear", "org_runtime_clear"] {
            assert!(
                WIRED_HANDLERS.contains(&name) || OPTED_OUT.iter().any(|(o, _)| *o == name),
                "{name} escaped both wake enumerations"
            );
            let _ = handler_body(&source, name);
        }
    }
}

#[cfg(test)]
mod body_read_tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use std::io::{Read, Write};

    /// A body that really is over the limit — the ONE case 413 was ever about.
    #[tokio::test]
    async fn an_oversized_body_is_the_only_thing_that_stays_a_413() {
        let error = axum::body::to_bytes(Body::from(vec![0_u8; 4096]), 1024)
            .await
            .expect_err("4096 bytes must not fit in a 1024-byte limit");
        assert!(
            matches!(classify_body_read_failure(&error), BodyReadFailure::TooLarge),
            "a genuine length-limit failure must classify as TooLarge"
        );
    }

    /// The same `to_bytes` call, failing for a completely different reason.
    #[tokio::test]
    async fn a_body_stream_error_is_named_and_is_not_a_size_complaint() {
        let body = Body::from_stream(stream::once(async {
            Err::<Bytes, std::io::Error>(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "peer went away mid-upload",
            ))
        }));
        let error = axum::body::to_bytes(body, 64 * 1024)
            .await
            .expect_err("an erroring stream must not collect");
        let BodyReadFailure::Unreadable(refusal) = classify_body_read_failure(&error) else {
            panic!("a stream error is not a length-limit failure");
        };
        assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);
        assert_eq!(refusal.code(), "request-body-unreadable");
        assert!(
            refusal.detail().contains("peer went away mid-upload"),
            "the underlying cause must be named, got {:?}",
            refusal.detail()
        );
    }

    /// Serve `resolve_live_source` over a real socket on a `/v1/org/*` path,
    /// with a resolver that never resolves anything — the middleware still
    /// reads the whole body to peek at the slug, which is the code under test.
    fn peek_router(max_body_bytes: usize) -> Router {
        let resolver: SupervisionLiveResolver = Arc::new(|_, _| None);
        Router::new().route("/v1/org/ping", post(|| async { "pong" })).layer(
            axum::middleware::from_fn(
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    let resolver = Arc::clone(&resolver);
                    async move { resolve_live_source(&resolver, max_body_bytes, req, next).await }
                },
            ),
        )
    }

    async fn serve_peek(max_body_bytes: usize) -> std::net::SocketAddr {
        let app = peek_router(max_body_bytes);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        addr
    }

    /// Drive one raw HTTP/1.1 request at `addr` and return its status line.
    /// `body` is written after the head, then the write half is closed — with
    /// a SHORT body that is exactly a caller disappearing mid-upload, and the
    /// read half stays open so chiefd's answer still arrives.
    ///
    /// Blocking `std` sockets on a blocking thread: the workspace `tokio` is
    /// built without `io-util`, and this proof needs nothing tokio provides.
    async fn one_request(
        addr: std::net::SocketAddr,
        declared: usize,
        body: &'static [u8],
    ) -> String {
        tokio::task::spawn_blocking(move || {
            let mut stream = std::net::TcpStream::connect(addr).expect("connect");
            let head = format!(
                "POST /v1/org/ping HTTP/1.1\r\nHost: chiefd\r\nContent-Length: {declared}\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).expect("write the head");
            stream.write_all(body).expect("write the body");
            stream.shutdown(std::net::Shutdown::Write).expect("close the write half");
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).expect("read the response");
            String::from_utf8_lossy(&raw).lines().next().unwrap_or_default().to_owned()
        })
        .await
        .expect("socket thread")
    }

    /// The defect, proved the way it happens in production: promise a body,
    /// send part of it, and go away. That is NOT a size problem and must not
    /// be answered with one.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_caller_that_disconnects_mid_upload_is_not_told_its_payload_was_too_large() {
        let addr = serve_peek(64 * 1024).await;
        let status = one_request(addr, 4096, b"{\"slug\":\"co").await;
        assert!(
            status.contains("400 Bad Request"),
            "a truncated upload must be answered as an unreadable request, got {status:?}"
        );
    }

    /// The other half of the split, driven through the SAME middleware in
    /// process. A real socket cannot observe this one reliably: hyper closes a
    /// connection whose request body it never drained, and that close carries
    /// unread bytes, so the peer sends RST and the client's kernel discards
    /// the 413 that was already on the wire. `oneshot` runs the identical
    /// middleware and reads the response the router actually produced.
    #[tokio::test]
    async fn an_oversized_body_still_gets_a_bare_413_through_the_middleware() {
        use tower::ServiceExt;

        let app = peek_router(1024);
        let response = app
            .oneshot(
                axum::http::Request::post("/v1/org/ping")
                    .body(Body::from(vec![b'x'; 4096]))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.expect("body");
        assert!(body.is_empty(), "413 keeps its bare status, got {body:?}");
    }

    /// And the unreadable body carries the taxonomy's `{code, detail}` shape
    /// out of the same middleware — not a bare status.
    #[tokio::test]
    async fn an_unreadable_body_answers_the_refusal_shape_through_the_middleware() {
        use tower::ServiceExt;

        let app = peek_router(64 * 1024);
        let body = Body::from_stream(stream::once(async {
            Err::<Bytes, std::io::Error>(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the upload stopped short",
            ))
        }));
        let response = app
            .oneshot(axum::http::Request::post("/v1/org/ping").body(body).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let raw = axum::body::to_bytes(response.into_body(), 64 * 1024).await.expect("body");
        let json: serde_json::Value = serde_json::from_slice(&raw).expect("refusal json");
        assert_eq!(json["code"], "request-body-unreadable");
        assert!(
            json["detail"].as_str().unwrap_or_default().contains("the upload stopped short"),
            "the cause must be named, got {json}"
        );
    }
}
