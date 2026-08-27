//! Company runtime LIFECYCLE orchestration — the half of `org-runtime.ts`
//! that is not a converge engine.
//!
//! # This module is not a second reconciler
//!
//! The runtime half of `org-runtime.ts` is ALREADY in Rust, in
//! [`crate::converge_apply`]. `runOrganizationRuntimeUnlocked`'s whole slow
//! path — activity projection under the launch-intent fence, mailbox demand,
//! the goal-delivery quiesce watermark, the idle-park settle/withdraw, the
//! desired-topology plan, the destructive-action budget, the ramp, the apply —
//! is [`converge_apply::reconcile_cycle`](crate::converge_apply::reconcile_cycle).
//! Nothing here re-plans, re-observes, or re-actuates. What is here is the
//! lifecycle wrapper the CLI used to own: ownership, the stop, and the two
//! bounded waits.
//!
//! # It is also the ONE assembler of the launcher asset roots
//!
//! [`LauncherAssets`] is where the checkout's own code lives, and a daemon has
//! no `src/foundation/paths.ts` to read it from. [`launcher_assets`] is the
//! single place it is built, from [`ActuatorConfig`] plus the durable
//! `org_settings.launcher_root` row. A route must never build its own: that
//! would be a second opinion on where the checkout is, and
//! `org_settings.launcher_root` exists precisely to make that fact singular (it
//! replaced the deleted `state/launcher.json`).
//!
//! # TypeScript → Rust, function by function
//!
//! Every deleted `org-runtime.ts` / `org-model-command.ts` export, and what
//! replaced it. Rows marked **existing** are NOT re-implemented here; the named
//! item is called.
//!
//! | Deleted TypeScript | Rust |
//! |---|---|
//! | `runOrganizationRuntimeUnlocked` (plan/actuate) | **existing** [`converge_apply::reconcile_cycle`](crate::converge_apply::reconcile_cycle) |
//! | `planOrganizationRuntime` / `reconcileOrganizationRuntime` | GONE from this crate (#751/P8-P10): chiefd publishes the desired roster and the per-person action stream; the walk and its actuation are `chief-cli`'s |
//! | `auditOrganizationRuntime` | GONE. The actuator observes its own runtime and tells nobody; there is no `runtime_actuation` for chiefd to read. |
//! | `compareOrganizationRuntimeProjection` | **existing** [`compare_runtime_projection`] |
//! | `latestOrganizationPersonSession` | [`latest_person_session`] — a wrapper over **existing** `resource_catalog::read_materialized_resources_for_launch`, whose `session` field IS the selection (epoch filter and mtime tie-break included) |
//! | `validateResources` | [`refuse_unlaunchable_person`] over **existing** [`build_launch_catalog`](crate::converge_apply::build_launch_catalog), which IS the fail-closed on-disk gate. `organizationPersonPiCommand`'s argv half has no counterpart here at all: after #751/P8 the operator client assembles every pane command, and chiefd publishes the catalog it assembles from. |
//! | `launcherRoot` (`foundation/paths.ts`) | [`launcher_assets`]. `buildCatalog` and `sourcePiHome` are **DELETED, not ported**: chief resolves no skill and reads no operator settings file |
//! | `writeRuntimeState` | **existing** `CompanyDb::runtime_publish` |
//! | `readOrganizationRuntimeDocument` | **existing** `CompanyDb::runtime_read` |
//! | `observeOrganizationRuntime` | GONE. chiefd holds the desired state and cannot answer what is RUNNING; see `observe_runtime`'s tombstone. |
//! | `launchOrganizationRuntime` / `launchSupervisedOrganizationRuntime` | [`launch_runtime`] |
//! | `launchCeoOnlyOrganizationRuntime` / `probeCeoProviderBeforeLaunch` / `discardProbeSessionResidue` / `waitForCeoPaneLiveness` / `ceoPaneLivenessError` / `ceoPaneStderrDiagnostic` / `releaseCeoLivenessPaneRetention` | **DELETED, not ported** (chief-home-is-cwd §4c): the daemon-side CEO boot, whole. The operator client owns every pane, so there is no first pane for chiefd to bring up, no lease to take while it does, and no liveness of its own to prove |
//! | `reconcileOrganizationRuntime` | [`reconcile_runtime`] |
//! | `resumeSupervisedOrganizationRuntime` | [`resume_supervised_runtime`] |
//! | `stopOrganizationRuntime` / `…Unlocked` / `stopSupervisedOrganizationRuntime` | [`stop_runtime`] / [`stop_supervised_runtime`] |
//! | `awaitOrganizationSessionAbsence` | [`await_session_absence`] (REACTIVE) |
//! | `closeTemporaryLauncherPane` | **DELETED, not ported** (#751/P8-P10): moving viewers between sessions and closing the pane they sat in is entirely a client-side handoff, and every one of its ten refusals was a proof about the CALLER's own terminal |
//! | `organizationMaterializationIsStale` / `refreshOrganizationMaterialization` / `ensureMaterializationCurrentBeforeTrustedBoot` | **DELETED, not ported** (chief-home-is-cwd §4d). Nothing re-projects a home, so nothing can be stale: `agent_home::ensure_agent_home` creates a missing home on the hire path and touches a present one never |
//! | `organizationExtensionDrift` / `describeExtensionDrift` | **DELETED, not ported**: the scan compared a person's COPIED extension files against the checkout, and there are no copies. A deploy still replaces every affected pane, through `materialize::extension_source_digest` moving the derived launch hash |
//! | `organizationRuntimeExtensionDrift` | GONE. The extension digest is an input to the derived launch hash, so a stale pane fails the actuator's diff instead of being scanned for. |
//! | `auditOrganizationRuntimeOwnership` (I/O half) | [`observe_prior_ownership`] |
//! | `auditOrganizationRuntimeOwnership` (decision) / `claim…` / `release…` | **existing** `CompanyDb::runtime_ownership_{read,claim,release}` over `chiefd_core::store::runtime_ownership` |
//! | `withOrganizationRuntimeLock` | **DELETED, not ported** — MANDATE 4 |
//! | `recordRuntimeLifecycle` (journal projection) | **existing** row journal; never a file |
//! | `timedPhase` | deleted: it measured the file lock's hold, which no longer exists |
//! | `coalescedStableRuntimeResult` / `compatibleStableRuntimeProjection` | **existing**: the converge cycle's own observe→plan diff already produces the empty plan a "coalesced" pass used to fake |
//!
//! # The mandates this module is built around
//!
//! * **MANDATE 1 — no polling.** The TypeScript liveness and session-absence
//!   waits looped with sleeps. Both are now ONE [`tokio::time::timeout`] around
//!   a `.await` on a [`RuntimeChangeSignal`] the change feed nudges. No sleep
//!   loop, no busy retry, no spin; a read failure inside a wait never rejects
//!   it, because the deadline is the only failure bound.
//! * **MANDATE 4 — no locks.** `withOrganizationRuntimeLock` and its
//!   `.org.lock` / `.runtime.lock` files are DELETED, not ported, and so is
//!   the `runtime_writer_lease` that briefly replaced them. Serialization is
//!   structural, at three layers none of which is a lock: beacond admits ONE
//!   chiefd per company before its storage opens, that daemon's writer actor
//!   is one thread and one `BEGIN IMMEDIATE` per mutation, and every converge
//!   pass — attended or duty-driven — runs under
//!   `converge_safety::begin_cycle`'s durable single-flight claim. Every
//!   durable fact one pass produces is published in ONE transaction for the
//!   same reason.
//!
//!   TOMBSTONE (chief-home-is-cwd §4c): one mutual-exclusion object used to
//!   survive this mandate — the CEO boot lease, which fenced an ATTENDED
//!   CEO-only boot's slow pre-converge phase against the reconcile duty, a
//!   window no transaction spans. That window no longer exists. The daemon
//!   boots no pane at all now, so the mandate is unqualified again and nothing
//!   replaces the lease.
//! * **MANDATE 5 — every runtime fact is a row.** No pid file, no
//!   `runtime.json`, no `location.json`, no JSON projection. The two files this
//!   module still read — the CEO probe's session residue and the stderr tail Pi
//!   itself wrote — went with the daemon-side CEO boot, so it now touches no
//!   file inside an agent's home at all.

use std::collections::{BTreeMap, BTreeSet};
// `std::path::Path` is imported inside `mod tests` rather than here: the CEO
// pane's stderr diagnostic was production's only borrowed-path reader, and it
// went with the daemon-side CEO boot (chief-home-is-cwd §4c).
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use chiefd_core::actor::{ChangeFeedSink, CompanyDb, MutationClass, MutationName};
use chiefd_core::runtime::duty_hooks::{ActuationMode, DutyError, ReconcileReport};
use chiefd_core::store::organization::OrganizationManifest;
use chiefd_core::store::runtime_rows::{RuntimeState, RUNTIME_STORE};
use chiefd_core::{ChiefdError, Refusal};

// #751/P8: `launch_command` and `PaneCommand` are GONE from this crate — argv
// assembly is the operator client's, and chiefd cannot build a pane command it
// could never run. What survives is [`build_launch_catalog`], because it is the
// fail-closed on-disk gate, and the gate is what this module ever wanted from
// it (see [`refuse_unlaunchable_person`]).
use crate::converge_apply::{build_launch_catalog, ActivityProjectionInput, ActuatorConfig};
use crate::executor::HostErr;
use crate::materialize::{LauncherAssets, MaterializeError};

// ---------------------------------------------------------------------------
// Carried-over bounds
// ---------------------------------------------------------------------------

/// A one-off runtime query may race a server restart; a projection mismatch is
/// actionable only after a second identical owned observation this far apart.
pub const RUNTIME_OBSERVATION_CONFIRMATION_MS: u64 = 15_000;

/// Bounded wait for chiefd's converge duty to tear down a stopped company's
/// session after the launcher commits the "stopped" runtime row. Generous
/// relative to the daemon's periodic fallback floor so a timeout is a real
/// "the duty is not running" signal, never a lost race.
pub const STOP_CONVERGENCE_TIMEOUT_MS: u64 = 30_000;

// TOMBSTONE (chief-home-is-cwd §4c): `CEO_PANE_LIVENESS_TIMEOUT_MS` (2.5s, the
// bound on chiefd confirming the CEO's pane in the committed runtime row) and
// the two `last-runtime.stderr.log{,.exit}` names the failure diagnostic read
// stood here. All three served the daemon-side CEO boot, which is deleted: the
// operator client launches the pane and is the one process that can see whether
// it came up.

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything a runtime-lifecycle operation can fail with.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeLifecycleError {
    /// The durable store refused, conflicted, or faulted.
    #[error(transparent)]
    Store(#[from] ChiefdError),
    /// A host tool (the runtime, pi, the filesystem) failed.
    #[error(transparent)]
    Host(#[from] HostErr),
    /// Materialization failed.
    #[error(transparent)]
    Materialize(#[from] MaterializeError),
    /// The converge cycle skipped or failed this pass.
    #[error("converge cycle failed: {0}")]
    Converge(#[from] DutyError),
    /// The committed stop never converged into an absent session.
    #[error("{0}")]
    StopDidNotConverge(String),
    /// A temporary launcher → company client handoff was refused.
    #[error("{0}")]
    HandoffRefused(String),
}

impl From<Refusal> for RuntimeLifecycleError {
    fn from(refusal: Refusal) -> Self {
        Self::Store(ChiefdError::Refused(refusal))
    }
}

/// Shorthand for this module's own refusals.
fn refuse(code: &'static str, message: impl Into<String>) -> RuntimeLifecycleError {
    RuntimeLifecycleError::Store(ChiefdError::refused(code, message))
}

// ---------------------------------------------------------------------------
// The reactive change signal (MANDATE 1)
// ---------------------------------------------------------------------------

/// The per-company reactive wake seam both bounded waits park on.
///
/// The DAEMON owns one per company, hands the routes a handle, and installs
/// [`Self::feed_sink`] on `CompanyDb::set_change_feed_sink`. A route that minted
/// its own would park on a signal nothing nudges — a wait that can only ever
/// time out is worse than no wait at all.
///
/// [`Notify::notify_waiters`] rather than `notify_one`, because a wait is an
/// OBSERVER, not a work queue: two callers waiting on the same convergence must
/// both learn about it. A signal that is never nudged costs exactly one timeout
/// — never a wrong answer — because every wait re-reads the committed row before
/// parking.
#[derive(Debug, Clone, Default)]
pub struct RuntimeChangeSignal {
    notify: Arc<Notify>,
}

impl RuntimeChangeSignal {
    /// A fresh signal. The daemon creates one per company.
    #[must_use]
    pub fn new() -> Self {
        Self { notify: Arc::new(Notify::new()) }
    }

    /// A change-feed sink that nudges THIS signal for every `runtime` commit.
    ///
    /// Commits to any other store are ignored, so an unrelated mailbox write
    /// does not wake a parked liveness wait.
    #[must_use]
    pub fn feed_sink(&self) -> Arc<ChangeFeedSink> {
        let notify = Arc::clone(&self.notify);
        Arc::new(move |_company: &str, store: &str, _key: &str, _at: &str, _removed| {
            if store == RUNTIME_STORE {
                notify.notify_waiters();
            }
        })
    }

    /// Wake every parked waiter now. Used by a writer that published a topology
    /// change through a path with no feed sink installed.
    pub fn nudge(&self) {
        self.notify.notify_waiters();
    }

    /// A future that resolves on the next nudge.
    ///
    /// Callers MUST `enable()` the pinned future before reading the row, so a
    /// commit landing between the read and the park is not lost. `Notified` is
    /// itself `#[must_use]`, so dropping one is already a warning here.
    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }
}

/// A bounded wait that reached its deadline, carrying the last row it saw so a
/// diagnostic is built from real evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitTimeout {
    /// The most recent committed `runtime` row this wait observed, if any.
    pub last_seen: Option<RuntimeState>,
}

/// Wait until `predicate` holds for the committed `runtime` row.
///
/// **MANDATE 1**: this is the ONLY wait primitive in the module and it does not
/// poll. Order is load-bearing and mirrors the deleted `awaitRuntimeConvergence`:
/// arm the notification FIRST, then read, then park. Reading first would lose a
/// change committed between the read and the park. A read failure never rejects
/// the wait — the deadline is the only failure bound.
///
/// # Errors
/// [`WaitTimeout`] when `timeout` elapses with the predicate still false.
pub async fn await_runtime_state<R, Fut, P>(
    signal: &RuntimeChangeSignal,
    timeout: Duration,
    mut read: R,
    predicate: P,
) -> Result<Option<RuntimeState>, WaitTimeout>
where
    R: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<RuntimeState>, ChiefdError>>,
    P: Fn(Option<&RuntimeState>) -> bool,
{
    // `Mutex`, not `RefCell`: this cell is borrowed inside a future that axum
    // requires to be `Send`, and `RefCell` is not `Sync`. The guard is taken
    // and dropped within a single statement below — it never spans an
    // `.await`, so this is a momentary in-memory cell, not a lock protocol
    // over durable state (Mandate 4's subject).
    let last_seen: std::sync::Mutex<Option<RuntimeState>> = std::sync::Mutex::new(None);
    let converged = tokio::time::timeout(timeout, async {
        loop {
            let notified = signal.notified();
            tokio::pin!(notified);
            // Subscribe before reading. `enable()` registers the waiter without
            // parking, which is exactly what closes the read/park race.
            notified.as_mut().enable();
            if let Ok(seen) = read().await {
                if predicate(seen.as_ref()) {
                    return seen;
                }
                if let Ok(mut slot) = last_seen.lock() {
                    *slot = seen;
                }
            }
            notified.await;
        }
    })
    .await;
    match converged {
        Ok(seen) => Ok(seen),
        // A poisoned cell means a panic while holding it; report no last-seen
        // rather than propagating the poison into a timeout error.
        Err(_elapsed) => Err(WaitTimeout { last_seen: last_seen.into_inner().unwrap_or_default() }),
    }
}

/// `db.runtime_read()` with the fence seq dropped — the shape
/// [`await_runtime_state`] consumes.
async fn runtime_state_of(db: &CompanyDb) -> Result<Option<RuntimeState>, ChiefdError> {
    Ok(db.runtime_read().await?.map(|(state, _seq)| state))
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

// TOMBSTONE: `RuntimeObservationReport`. See `observe_runtime`'s tombstone
// below for why the whole observation answer is gone rather than emptied.

/// The outcome of a launch / resume / reconcile pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeLaunchReport {
    /// The company slug.
    pub organization: String,
    /// The runtime server socket — the opaque actuator identity chiefd holds
    /// for this company, never a session name (AC6).
    pub socket_name: String,
    /// Whether the cycle actuated (false in shadow mode).
    pub applied: bool,
    /// How many people the pass DESIRES running. Wire key `desiredPeople`.
    pub desired_people: usize,
    /// A reactive request arrived inside the single-flight floor and must be
    /// replayed after it.
    pub retry_after_floor: bool,
    /// person → the actuator's process handle, read back from the committed
    /// runtime row the cycle published.
    pub process_handles: BTreeMap<String, String>,
    /// Contained per-person failures and drift notices for the operator.
    pub monitor_warnings: Vec<String>,
    /// The converge cycle's own notes.
    pub notes: Vec<String>,
}

/// Who actually tore the session down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopActor {
    /// The committed stopped fact was converged by chiefd's reconcile duty
    /// (arch Step 7) — the ordinary path.
    Daemon,
    /// A caller holding the CEO-boot suppression lease tore its own transition
    /// window down, inside the window that same lease makes the duty inert.
    LeaseHolder,
}

/// The outcome of an explicit company stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStopReport {
    /// The company slug.
    pub organization: String,
    /// Whether the stop found no owned session at all.
    pub already_stopped: bool,
    /// The people the last committed observation had running immediately
    /// before the teardown.
    ///
    /// Person ids, not pane ids: chiefd has none to report, and the actuator's
    /// record is keyed by person. `stopped_window_ids` went with them — a
    /// window is a display grouping the backend never sees.
    pub stopped_person_ids: Vec<String>,
}

/// The live observation an ownership claim's pure decision cannot make for
/// itself, gathered immediately before the transaction that consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorOwnershipObservation {
    /// Whether the PREVIOUSLY RECORDED socket still projects a live runtime.
    pub prior_projection_exists: bool,
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Everything a launch decides that the committed rows cannot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchOptions {
    /// ISO-8601 stamp for every record this pass writes.
    pub at: String,
    /// Explicit per-node launch intent. Recorded durably, so — and only so —
    /// those nodes may run. The CEO is always implicitly intended and is never
    /// recorded.
    pub requested_person_ids: Vec<String>,
    /// An execution lease is deliberately an IN-MEMORY projection input, not a
    /// durable start decision. Persisting it would leave a manager resident
    /// after one completed public tool call and break the minimum-fleet rule,
    /// so these ids are excluded from the durable intent above.
    pub execution_lease_person_ids: Vec<String>,
    /// Who is asking, for the durable audit trail.
    pub actor: String,
}

/// Identity of the caller of an explicit stop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StopOptions {
    /// ISO-8601 stamp for every record this pass writes.
    pub at: String,
    /// Whether this stop DELETES launch intent rather than merely narrowing it.
    ///
    /// True only for an attended stop, whose contract is "the next boot is
    /// CEO-only". It is committed in the SAME transaction as the stopped
    /// runtime projection (`CompanyDb::runtime_stop_publish`) — the two used to
    /// be separate transactions with a runtime kill and a session-absence wait
    /// between them, which is a window in which the company reports stopped
    /// while still holding authorization for its whole previous roster.
    pub clear_launch_intent: bool,
}

// ---------------------------------------------------------------------------
// Small shared helpers
// ---------------------------------------------------------------------------

/// The committed manifest, or a refusal naming the absence.
async fn manifest_of(db: &CompanyDb) -> Result<OrganizationManifest, RuntimeLifecycleError> {
    db.org_manifest_read()
        .await?
        .map(|(manifest, _seq)| manifest)
        .ok_or_else(|| refuse("no-committed-manifest", "this company has no committed manifest"))
}

// TOMBSTONE: `now_ms` and `now_iso`. `now_ms` derived `ActuatorPresence`
// against a lease clock, and there is no lease and no presence; `now_iso`
// stamped the materialization checkpoints and the launcher-root fact a
// completed in-place pass produced, and there are no passes. Every durable
// record this module still writes is stamped by its CALLER, from the `at` the
// route already carries.

// ---------------------------------------------------------------------------
// The ONE assembler of launcher asset roots
// ---------------------------------------------------------------------------

/// What to tell an operator when the launcher root cannot work.
///
/// A pure function so BOTH branches are pinned by test: the recorded root and
/// the configured one produce different advice, and the recorded case is the
/// one that cost a multi-cycle hunt because the message named a path without
/// naming where it came from.
/// `missing` is the path that was not there and `consequence` is what its
/// absence DOES, because the two failures this refusal covers have the same
/// cure and different symptoms: a root that is not a checkout at all, and a
/// checkout nobody built.
fn unusable_launcher_root_detail(
    launcher_root: &std::path::Path,
    source: &str,
    missing: &std::path::Path,
    consequence: &str,
) -> String {
    format!(
        "The resource root '{}' ({}) has no '{}', {}. A root RECORDED for the company outranks \
         every daemon setting, so changing ORG_LAUNCHER_ROOT or --launcher-root will not move \
         it — republish it with /v1/org/settings/publish. Otherwise reinstall chief, or point \
         chiefd at a checkout that has been built.",
        launcher_root.display(),
        source,
        missing.display(),
        consequence,
    )
}

/// The refusal for a recorded launcher root that is the install's own versioned
/// resource path (H.2.5) — a value that should never have been persisted.
fn recorded_install_root_detail(recorded: &std::path::Path) -> String {
    format!(
        "The launcher root RECORDED for this company, '{}', is an install path under \
         'versions/', not a checkout. A later 'chief upgrade' prunes that directory, so \
         materialization would break with no upgrade failure in sight. The override is for \
         pinning a CHECKOUT, never the install. Clear it (republish an empty root via \
         /v1/org/settings/publish) so the daemon resolves its resources fresh each boot, or \
         re-pin it at a real checkout.",
        recorded.display(),
    )
}

/// What the missing extension SOURCES do: nobody gets the organization tools.
const NO_EXTENSION_SOURCES: &str = "so no person could be given the organization tools \
     (org_hire, org_roster, org_send, …) and the company's CEO would come up unable to staff it";

/// What the missing BUILT extension runtime does, in the words of the pane that
/// measured it.
///
/// Every extension chief hands Pi imports `@chief/piing/extension-runtime`,
/// which that package's `exports` map resolves to
/// `dist/extensionruntime/index.js`. An unbuilt checkout has the `.ts` sources
/// and no `dist`, so Node reports the subpath as *not exported* — and Pi exits
/// status 1 before it draws anything.
const NO_BUILT_EXTENSION_RUNTIME: &str = "so every extension chief hands Pi fails to load with \
     \"Package subpath './extension-runtime' is not defined by exports\" and the pane exits \
     during start-up — the checkout has the extension sources and was never built";

/// The other half of the same failure, and it is NOT the one above.
///
/// The probe for the runtime FILE was added after the August incident and it
/// passes on a tree that cannot launch: a release shipped
/// `packages/piing/dist/extensionruntime/index.js` and no package identity, so
/// the file existed and `@chief/piing/extension-runtime` still resolved against
/// nothing. **Existence was never the question — RESOLUTION was**, and the two
/// differ exactly when `node_modules/@chief/<pkg>` is missing.
///
/// One path covers both roots, which is why this probe is worth having rather
/// than being a second answer: a CHECKOUT has `node_modules/@chief/piing` as a
/// workspace link `bun install` creates, and an INSTALL has it as the shim
/// `scripts/release-chiefd.ts` writes beside the packaged runtime.
/// The `@chief` packages the shipped extensions import a runtime from.
///
/// Two, and the second is the one a symmetry-blind reader drops: `piing`
/// carries the extensions themselves, `chiefing` carries the transport,
/// discovery and SSE surface that `team-ui.ts` and `founder-launch.ts` import.
/// A release that packages one identity and not the other launches nothing,
/// and says so only inside a pane that is destroyed on exit.
const ORGANIZATION_EXTENSION_PACKAGES: [&str; 2] = ["piing", "chiefing"];

const NO_PACKAGE_IDENTITY: &str = "so every extension chief hands Pi fails to load with \
     \"Cannot find module '@chief/<package>/extension-runtime'\" and the pane exits during \
     start-up — the runtime is packaged but nothing gives it the name the extensions \
     import it by; run `bun install` in a checkout, or reinstall a release built after \
     the packaging fix";

/// Resolve this company's launcher root and the extension sources under it.
///
/// The single place [`LauncherAssets`] is built. A route that constructed its
/// own would be a second opinion on where the checkout is, and a wrong
/// extensions root yields a confidently-wrong `extension_source_digest` —
/// which replaces every pane in the company or none of them.
///
/// The root recorded for the company (`org_settings.launcherRoot`) OUTRANKS
/// whatever this daemon resolved from `~/.chief/launcher-root`,
/// `ORG_LAUNCHER_ROOT` or `--launcher-root`; the winning source is carried
/// into any refusal so an operator is never left assuming the value came from
/// their own configuration.
///
/// # Errors
///
/// Returns [`RuntimeLifecycleError`] if the settings read fails, or if the
/// resolved launcher root is not a launcher checkout (no
/// `packages/piing/extensions`, the directory the whole `org_*` tool surface
/// lives in — a root that would silently yield a toolless CEO is refused at
/// launch instead).
pub async fn launcher_assets(
    db: &CompanyDb,
    config: &ActuatorConfig,
) -> Result<LauncherAssets, RuntimeLifecycleError> {
    let recorded = recorded_launcher_root(db)
        .await?
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from);
    // H.2.5: a RECORDED root under the install's `versions/<v>/resources` is
    // refused, not used. The override exists so a company can pin its own
    // CHECKOUT; an exe-derived install path wearing that hat is a trap — a
    // later `chief upgrade` prunes `versions/<v>`, and materialization, which
    // reads `resources/` by path, then breaks with no upgrade failure in sight.
    // The exe-derived root is resolved fresh on every boot (the `config` branch
    // below); it is never a thing to persist. This checks the RECORDED value
    // only — `config.launcher_root` is legitimately of this shape on an
    // installed box and must pass untouched.
    if let Some(recorded_root) = recorded.as_ref() {
        if host_primitives::install::is_installed_resource_root(recorded_root) {
            return Err(refuse(
                "launcher-root-recorded-install-path",
                recorded_install_root_detail(recorded_root),
            ));
        }
    }
    // WHICH source won, carried alongside the value.
    //
    // A recorded root OUTRANKS everything the daemon resolved — the pointer at
    // `~/.chief/launcher-root`, `ORG_LAUNCHER_ROOT`, `--launcher-root`, all of
    // it. That is defensible: a company should be able to pin its own launcher.
    // What is not defensible is a refusal that names a path and leaves an
    // operator to assume it came from their configuration. It cost a
    // multi-cycle hunt on a company created when the conventional path was
    // still correct: every environment fix resolved the right root, and every
    // one was discarded here in favour of the recorded value.
    let (launcher_root, source) = recorded.map_or_else(
        || (config.launcher_root.clone(), "resolved from this daemon's configuration"),
        |root| (root, "recorded for this company (org_settings.launcherRoot)"),
    );

    // REFUSE a launcher root that is not a launcher checkout.
    //
    // Every path below is built from this root without asking whether the root
    // exists, so a wrong one produced a person whose `pi-home/extensions/` was
    // simply EMPTY — and each per-person failure behind that is contained by
    // policy and never surfaced. The result was a company that reported
    // "✅ Company launched · CEO booted" over a CEO with no `org_*` tools at
    // all: no `org_hire`, no `org_roster`, no `org_send`. Asked to staff
    // the company it answered "this session doesn't have the org tools", and
    // no operator action could fix it because nothing anywhere reported a
    // problem.
    //
    // `packages/piing/extensions` is the right probe: it is the directory the
    // organization extensions — the whole `org_*` tool surface — are copied
    // from, so its absence is exactly the condition that yields a toolless
    // agent. A launch that cannot produce a working CEO must fail loudly at the
    // launch, not silently at the first instruction the CEO cannot carry out.
    let piing = launcher_root.join("packages").join("piing");
    let extensions_root = piing.join("extensions");
    if !extensions_root.is_dir() {
        return Err(refuse(
            "launcher-root-unusable",
            unusable_launcher_root_detail(
                &launcher_root,
                source,
                &extensions_root,
                NO_EXTENSION_SOURCES,
            ),
        ));
    }
    // AND THE BUILT RUNTIME THOSE SOURCES IMPORT.
    //
    // The probe above was the whole check, and it is satisfied by a checkout
    // that cannot run: `extensions/` is committed source, `dist/` is a build
    // product. Measured live on a QA box (2026-08-19), on a company whose CEO
    // died 16 times in 3m23s while the actuator could report only "the pane the
    // actuator started for them was gone by the next converge pass". The pane's
    // own stderr, once it was kept, said it in one line:
    //
    //     Failed to load extension ".../packages/piing/extensions/organization-intercom.ts":
    //     Package subpath './extension-runtime' is not defined by "exports" in
    //     .../packages/piing/package.json
    //
    // `packages/piing/dist` did not exist in that checkout. Dropping the built
    // `dist` in brought the SAME CEO up on the next converge pass with nothing
    // else changed. So the honest probe is the file the extensions IMPORT, not
    // the directory they live in: the same refusal, at launch, by name, instead
    // of a crash loop whose cause dies with the pane.
    let extension_runtime = piing.join("dist").join("extensionruntime").join("index.js");
    if !extension_runtime.is_file() {
        return Err(refuse(
            "launcher-root-unbuilt",
            unusable_launcher_root_detail(
                &launcher_root,
                source,
                &extension_runtime,
                NO_BUILT_EXTENSION_RUNTIME,
            ),
        ));
    }

    // AND THE NAME THOSE EXTENSIONS IMPORT IT BY.
    //
    // Measured live: a company whose every person crash-looped with a blank
    // cause, because the probe above was satisfied — the runtime file shipped —
    // while `node_modules/@chief/piing` did not, so Pi could not resolve the
    // specifier and exited 1 inside a pane that was destroyed on exit.
    //
    // Checked at `launcher_root` rather than beside the package, because that
    // is the one path that is correct for BOTH roots: a checkout's workspace
    // link and an install's packaged shim sit in the same place relative to it.
    // BOTH packages, because the extensions import both and a probe that
    // checked one would pass a tree that still dies at Pi. `team-ui.ts` and
    // `founder-launch.ts` import `@chief/chiefing/extension-runtime`, and the
    // outage's own pane capture named that specifier beside `@chief/piing`'s:
    // checking only the first would have reproduced the exact failure this
    // probe exists to make legible, one package over.
    for package in ORGANIZATION_EXTENSION_PACKAGES {
        let package_identity = launcher_root.join("node_modules").join("@chief").join(package);
        if !package_identity.is_dir() {
            return Err(refuse(
                "launcher-root-unresolvable",
                unusable_launcher_root_detail(
                    &launcher_root,
                    source,
                    &package_identity,
                    NO_PACKAGE_IDENTITY,
                ),
            ));
        }
    }

    Ok(LauncherAssets { launcher_root })
}

// TOMBSTONE: `catalog_of` and `installed_resource_catalog`.
//
// They walked four skill roots to depth 5 reading every `SKILL.md`, walked two
// extension roots twice, canonicalized every extension directory and parsed
// every npm `package.json`, to answer "which skills, extensions and packages
// may a person be hired with". Nobody may be hired with one: a person selects
// no resource, chief resolves none, and the skills an agent has are whatever is
// in `<dir>/.pi/skills` when Pi looks. `POST /v1/org/resource-catalog/read` and
// the hire preflight that consumed it went with them.

/// The recorded `org_settings.launcher_root`, `None` when unset.
///
/// `chiefd_core::store::org_settings::read` is the authority and is not
/// re-implemented; this only names the read so it has one mapping point.
async fn recorded_launcher_root(db: &CompanyDb) -> Result<Option<String>, ChiefdError> {
    let slug = db.label().to_owned();
    db.read_txn(move |tx| {
        Ok(chiefd_core::store::org_settings::read(tx, &slug)?
            .and_then(|settings| settings.launcher_root))
    })
    .await
}

// ---------------------------------------------------------------------------
// Paths and per-person launch inputs (thin wrappers over existing Rust)
// ---------------------------------------------------------------------------

// TOMBSTONE: `person_files`. It composed the four paths of a person's
// materialized home — `people/<id>/{,workspace,pi-home,pi-home/sessions}` — for
// routes that needed them as data. There is ONE path now,
// `crate::agent_home::agent_home(dir, person_id)`, and it is derived by the
// module that writes it rather than re-composed by each reader.

/// The transcript a person's pane resumes: their newest, unless it predates the
/// clean-session epoch.
///
/// A thin wrapper: the selection itself (including the epoch filter and the
/// mtime tie-break) is **existing**
/// `resource_catalog::read_materialized_resources_for_launch`, whose `session`
/// field IS this answer. `None` means the pane omits `--session`, so a
/// filtered-out transcript is left on disk untouched.
///
/// # Errors
/// [`RuntimeLifecycleError::Store`] for an unknown person, or when the
/// session-epoch row cannot be read.
pub async fn latest_person_session(
    db: &CompanyDb,
    config: &ActuatorConfig,
    manifest: &OrganizationManifest,
    person_id: &str,
) -> Result<Option<PathBuf>, RuntimeLifecycleError> {
    let person = manifest
        .people
        .get(person_id)
        .ok_or_else(|| refuse("unknown-person", format!("Unknown person '{person_id}'")))?;
    let epoch = session_epoch_system_time(db).await?;
    Ok(crate::converge_apply::resource_catalog::read_materialized_resources_for_launch(
        person,
        &crate::agent_home::agent_home(&config.dir, person_id),
        &config.root_pi_agent_dir,
        epoch,
        false,
    )
    .and_then(|resources| resources.session))
}

/// Refuse, by name, a person the launch gate would decline — or return `Ok`.
///
/// This replaces `person_pi_command`, and the replacement is the honest shape
/// rather than a reduction. That function built a whole pane argv and threw the
/// argv away; its one caller used it as a pre-launch VALIDATION, with the
/// comment *"Building the command IS the fail-closed resource validation"*.
/// After #751/P8 chiefd cannot assemble a pane command at all — `launch_command`
/// left with the interpreter — but it still owns the gate, and the gate is the
/// only half that was ever load-bearing here.
///
/// The refusal text comes from [`build_launch_catalog`]'s own
/// `LaunchCatalog::refusal`, so an operator gets the SAME sentence whether the
/// person was declined at a CEO-only boot or at an ordinary start — including
/// the re-derived on-disk cause ("required directory 'workspace' is missing")
/// rather than an interchangeable "does not validate".
///
/// # Errors
/// [`RuntimeLifecycleError::Store`] when the person's materialized home does not
/// validate.
pub fn refuse_unlaunchable_person(
    config: &ActuatorConfig,
    manifest: &OrganizationManifest,
    person_id: &str,
) -> Result<(), RuntimeLifecycleError> {
    match build_launch_catalog(manifest, config).refusal(person_id) {
        None => Ok(()),
        Some(reason) => {
            Err(refuse("person-not-materialized", format!("refusing to boot: {reason}")))
        }
    }
}

// ---------------------------------------------------------------------------
// Extension drift
// ---------------------------------------------------------------------------

// TOMBSTONE: `source_extension_drift`.
//
// It answered "which of a person's materialized extension FILES do not match
// the checkout that produced them", by walking every person's
// `pi-home/extensions` and comparing content. Nothing is copied into a person's
// home, so the scan has no left-hand side: the question is unaskable rather
// than merely unasked. `POST /v1/org/materialize/extension-drift` went with it.
//
// THE GUARANTEE SURVIVES BY CONSTRUCTION, which is why this is a deletion
// rather than a loss — the same argument the runtime half's tombstone below
// makes, one layer along. `materialize::extension_source_digest` hashes the
// CHECKOUT and is an input to `desired_launch_hash`, so a deploy moves every
// person's published hash, their pane's tag stops matching, and the actuator
// replaces them on its next pass.

// TOMBSTONE: `live_extension_drift`.
//
// It answered "which RUNNING people cannot have loaded the extensions currently
// on their own disk", by joining the actuator's committed observation against
// mtimes under the data root. Both halves of that join are gone: the
// observation is deleted, and `extension_drift`'s runtime half
// (`RuntimeDriftScan`, `runtime_extension_drift`, `observed_process_pids`,
// `Unobserved`, `deploy_drift_report`) went with it.
//
// THE GUARANTEE SURVIVES BY CONSTRUCTION, which is the whole reason this is a
// deletion rather than a loss. The extension source digest is an INPUT to
// `desired_launch_hash`, so a launcher deploy moves every affected person's
// published hash, their pane's tag stops matching, and the actuator replaces
// them on its next pass. The incident this scan existed for -- "a whole fleet
// came up running old code and reported success" -- is answered by a stale pane
// being unable to survive a converge pass, rather than by chiefd noticing and
// asking an operator to restart people by hand.
//
// [`source_extension_drift`] is UNTOUCHED and is not a substitute that was
// weakened into place: it asks a different question with a different subject --
// what is on DISK versus what should be -- which chiefd materialized itself and
// can therefore still see.

// ---------------------------------------------------------------------------
// Materialization
// ---------------------------------------------------------------------------
//
// TOMBSTONE: `CommittedAgentsGuide`, `committed_agents_guide`,
// `projected_agents_guide`, `stage_materialization`, `refresh_materialization`,
// `materialization_is_stale`, `person_kind_text` and
// `ensure_materialization_current_before_trusted_boot`.
//
// Together they were the freshness machine: build a PROPOSED manifest into
// `<dir>/.chief/.staging/<uuid>/` to prove it resolves, promote it after the
// commit, re-project every home on every pass, publish a durable checkpoint per
// person, and probe both the checkpoint and the disk before trusting a home.
// Every part of that existed because a home was a projection of SQL and could
// therefore fall behind it.
//
// A home is not a projection any more. `agent_home::ensure_agent_home` creates
// a missing one and touches a present one never, so "stale" has no meaning and
// there is nothing to stage, promote, checkpoint or probe. The hire path calls
// it directly (`chiefd-api`'s `ensure_committed_agent_homes`); the converge
// cycle stays read-only about homes and refuses, by name, a person who has
// none.
//
// The `materialization_checkpoints` table went with the checkpoints, and the
// `AgentsGuide` seam went with the projection: `ensure_agent_home` takes the
// contract as a `&str`, because a trait exists to defer a decision and there is
// no longer a decision to defer.

/// Bring an EXISTING company's skills to what this release ships, touching
/// nothing else.
///
/// The library, the Chief's own install, and one install per person who already
/// has a home. That is all: no home is created, no contract is derived, no
/// identity is provisioned. Deliberately — this runs at daemon boot, which is
/// the one event every company has on every upgrade, and boot is far too early
/// to be minting identities. Calling `ensure_agent_homes` here instead was
/// measured to wedge the CEO's own pane: it enrols people mid-boot, and the
/// company came up with a desired CEO, an empty launch plan and no pane at all.
///
/// A person with no home yet is SKIPPED rather than built. They are getting one
/// from `ensure_agent_homes` on the hire path, which installs their role skill
/// in the same call.
///
/// # Errors
/// Never fails the caller: every problem is returned as a warning string, since
/// a company whose skills are briefly stale still runs.
pub async fn reconcile_company_skills(
    db: &CompanyDb,
    dir: &std::path::Path,
    shipped_skills_root: &std::path::Path,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Err(error) = crate::project_skills::reconcile_project_skills(dir, shipped_skills_root) {
        warnings.push(format!(
            "the company skill library could not be reconciled ({error}); people keep the skills they have"
        ));
        return warnings;
    }
    let manifest = match manifest_of(db).await {
        Ok(manifest) => manifest,
        Err(error) => {
            warnings
                .push(format!("the roster could not be read ({error}), so no skill was installed"));
            return warnings;
        }
    };
    let chief_person_id = match manifest.chief_person_id() {
        Ok(id) => id.to_owned(),
        Err(error) => {
            warnings.push(format!("the roster names no chief ({})", error.message));
            return warnings;
        }
    };
    for person_id in &manifest.people_order {
        let Some(person) = manifest.people.get(person_id) else { continue };
        if person_id == &chief_person_id || !crate::materialize::is_employed(person) {
            continue;
        }
        let home = crate::agent_home::agent_home(dir, person_id);
        if !home.exists() {
            continue;
        }
        if let Err(error) = crate::agent_home::install_role_skill_for(
            &home,
            crate::agent_home::RoleSkill::of(person.kind),
        ) {
            warnings.push(format!("{person_id}'s skill could not be installed ({error})"));
        }
    }
    warnings
}

/// Give every committed person a home and an enrolled identity, creating
/// neither twice.
///
/// **A person row, a person home and a person identity come into existence
/// together, or the person is a ghost.** This is the ONE sequence, called from
/// the hire path (`chiefd-api`'s person-creating routes) and from
/// [`launch_runtime`]; two spellings of it is how one of them falls behind.
///
/// It takes the company DIRECTORY rather than an [`ActuatorConfig`], and that
/// is deliberate: `chiefd run --serve-only` mounts no actuator at all, and a
/// person hired on that mount is still a person whose home must not wait for a
/// convergence pass the mount will never run.
///
/// It no longer takes the operator's agent directory. It used to, because the
/// home linked three files into it; since chief stopped redirecting
/// `PI_CODING_AGENT_DIR` the home holds nothing of the operator's at all, and
/// Pi reaches the operator's own configuration by its own inheritance.
///
/// Order is load-bearing and is the reverse of what it was. The home is created
/// FIRST and the identity key is minted into it second, because
/// [`crate::agent_home::ensure_agent_home`] returns immediately when the folder
/// exists — so a key minted into an absent home would create the folder and
/// make the home writer skip a home it never wrote, leaving an agent with a key
/// and no `AGENTS.md` and no role skill.
///
/// Idempotent by construction: both halves are create-if-absent, so the steady
/// path costs one stat per person and this may be called on every roster
/// mutation and every launch.
///
/// A person whose home cannot be written is REPORTED and stepped over, never
/// allowed to deny the rest of the roster theirs — the containment rule the
/// deleted `PersonFailurePolicy::Contain` carried, kept as the only policy
/// because there is no longer a preflight that could refuse instead. The
/// affected person is then refused BY NAME at launch, with the real cause.
///
/// # Errors
/// [`RuntimeLifecycleError::Store`] when the manifest carries a person whose
/// operating contract cannot be derived — a corrupt manifest, not one person's
/// bad luck.
pub async fn ensure_agent_homes(
    db: &CompanyDb,
    dir: &std::path::Path,
    shipped_skills_root: Option<&std::path::Path>,
) -> Result<Vec<String>, RuntimeLifecycleError> {
    let manifest = manifest_of(db).await?;
    let mut warnings = Vec::new();

    // THE LIBRARY, BEFORE ANYTHING INSTALLS OUT OF IT.
    //
    // It lives HERE, in the same call as the installs, and not on the launch
    // path beside it. Measured, on a live company restart: the launch-path
    // version silently never ran, and every person stayed on the retired flat
    // link with all five deleted skills still readable. This function is the
    // one that runs on every launch AND on every roster mutation, and it is the
    // only writer of the links that point INTO the library — so keeping the two
    // in one call is what stops them getting out of step. A link written before
    // the library was reconciled points at a retired skill, or at nothing.
    //
    // REPORTED rather than fatal, like a home that cannot be written: a company
    // whose library is briefly stale still runs.
    if let Some(shipped) = shipped_skills_root {
        match crate::project_skills::reconcile_project_skills(dir, shipped) {
            Ok(crate::project_skills::ProjectSkillOutcome::Changed) => tracing::info!(
                event = "org.skills.library.reconciled",
                dir = %dir.display(),
                "the company skill library was brought to the shipped set"
            ),
            Ok(crate::project_skills::ProjectSkillOutcome::Converged) => {}
            Err(error) => warnings.push(format!(
                "the company skill library could not be reconciled ({error}); people keep the skills they have until the next pass"
            )),
        }
    }
    // DERIVED, not read back. The contract is a pure function of the manifest,
    // and at hire time the stored document may not carry the person yet — so
    // reading it would refuse `person-contract-absent` for exactly the person
    // this call exists to serve. The transaction that commits the roster
    // publishes the same derivation from the same function.
    let contracts =
        chiefd_core::store::person_contracts::build::build_organization_person_contracts(&manifest)
            .map_err(ChiefdError::Refused)?;

    let chief_person_id = manifest.chief_person_id().map_err(ChiefdError::Refused)?.to_owned();
    let accent_order = crate::accent::identity_accent_order(&manifest.people);
    // The Chief is the operator's own Pi. It needs a company-scoped identity,
    // but it must never get a materialized agent home.
    let mut provisionable: Vec<String> = vec![chief_person_id.clone()];
    for person_id in &manifest.people_order {
        let Some(person) = manifest.people.get(person_id) else { continue };
        // A departed person keeps their home — departed-retention is a durable
        // history invariant — but nobody writes them a new one.
        if !crate::materialize::is_employed(person) {
            continue;
        }
        if person_id == &chief_person_id {
            continue;
        }
        let Some(contract) = contracts.contracts.get(person_id) else {
            warnings.push(format!(
                "{person_id} has no derivable operating contract, so no home was written;                  they will be refused at start by name"
            ));
            continue;
        };
        // The Chief is `continue`d above, so this never allocates for the CEO;
        // it is passed anyway so the reservation holds on every path.
        let identity_color = crate::accent::organization_person_accent(
            &accent_order,
            Some(chief_person_id.as_str()),
            person_id,
        )?;
        match crate::agent_home::ensure_agent_home(
            dir,
            person_id,
            &identity_color,
            &contract.text,
            crate::agent_home::RoleSkill::of(person.kind),
        ) {
            Ok(_) => provisionable.push(person_id.clone()),
            Err(error) => warnings.push(format!(
                "{person_id}'s agent home could not be written ({error}); they will be refused                  at start by name until the next pass repairs it"
            )),
        }
    }

    // The REAL mint: `chiefd-host` carries the same `p256` crate `chiefd-api`
    // verifies challenges with, so a key minted here and a signature verified
    // there cannot disagree about the encoding. Only people whose home exists
    // are provisioned — a key written beside a home that failed would be an
    // identity for a person who cannot boot.
    let mint = crate::identity_key::host_identity_key_mint();
    crate::identity_enrolment::provision_people(db, dir, &chief_person_id, provisionable, &mint)
        .await;

    Ok(warnings)
}

// ---------------------------------------------------------------------------
// Observation
// ---------------------------------------------------------------------------

/// The recovery identity of one projection mismatch. Two observations describe
/// the same problem exactly when their fingerprints match.
#[must_use]
pub fn recovery_fingerprint(
    missing_durable_person_ids: &[String],
    unexpected_observed_person_ids: &[String],
) -> String {
    let missing: BTreeSet<&String> = missing_durable_person_ids.iter().collect();
    let unexpected: BTreeSet<&String> = unexpected_observed_person_ids.iter().collect();
    serde_json::to_string(&serde_json::json!({
        "missingDurablePersonIds": missing,
        "unexpectedObservedPersonIds": unexpected,
    }))
    .unwrap_or_default()
}

/// Whether a mismatch is CONFIRMED: the same fingerprint has now been observed
/// for at least [`RUNTIME_OBSERVATION_CONFIRMATION_MS`].
///
/// A one-off runtime query may race a runtime server restart, so a single mismatch is
/// never actionable. This is the pure decision half of
/// `observeOrganizationRuntimeUnlocked`'s confirmation rule.
#[must_use]
pub fn recovery_is_confirmed(observed_at_ms: i64, recovery_observed_at_ms: i64) -> bool {
    observed_at_ms.saturating_sub(recovery_observed_at_ms)
        >= i64::try_from(RUNTIME_OBSERVATION_CONFIRMATION_MS).unwrap_or(i64::MAX)
}

// TOMBSTONE: `observe_runtime` and `RuntimeObservationReport`.
//
// This refreshed the derived runtime observation from the actuator's committed
// report -- the answer to "what is actually running" that `chief ls`, the
// board and `POST /v1/org/runtime/observe` all read. Its input is deleted and
// the direction is barred, so the function has nothing to derive from.
//
// It is worth recording what this function got RIGHT, because the deletion must
// not be read as walking away from it. It REFUSED an unproven report rather
// than folding it into an empty observation: `Observation::Untrusted` and "no
// actuator has reported" both became an error, because either one silently
// becoming `process_handles: {}` would publish a recovery fingerprint accusing
// every live person of being missing. That is the same unreadable-versus-empty
// rule this whole change is built on, and it is honoured here in the strongest
// available form -- the state that could be misread no longer exists, so
// nothing can misread it.
//
// NAMED, ACCEPTED LOSS: chiefd can no longer answer what is running, and
// `chief ls` loses liveness. the design record records it. The
// actuator owns the operator's screen and is the only process that can see a
// pane, which is where that question is now answered.
//
// `recovery_fingerprint` and `recovery_is_confirmed` above are UNTOUCHED. They
// are pure functions over two id lists and belong to the desired-side
// projection (`runtime_projection.rs`), not to the observation -- the plan is
// explicit that `unexpected_observed_person_ids` is a separate identifier with
// a separate meaning and does not die by name-similarity.

// ---------------------------------------------------------------------------
// Ownership: the I/O half only
// ---------------------------------------------------------------------------

/// Gather the live observation an ownership claim's PURE decision cannot make
/// for itself.
///
/// The decision (`runtime_ownership::audit_ownership`) and the write share one
/// `BEGIN IMMEDIATE` inside `CompanyDb::runtime_ownership_claim`, and a
/// transaction may not do I/O — so the ownership read happens HERE, immediately
/// before it.
///
/// There was a second observation, a live-supervisor probe read from
/// `supervisor_process_state`. Its writer was the detached org-supervisor's
/// state module, retired by #825 and deleted by `5681617a4`, so it answered
/// `None` on every call and the two refusals it fed were unreachable. It is
/// gone; see `audit_ownership` for why it is not re-sourced. What remains is
/// `prior_projection_exists`, and it is now the whole gate.
///
/// # Errors
/// [`RuntimeLifecycleError::Store`] when the manifest or ownership rows cannot
/// be read.
pub async fn observe_prior_ownership(
    db: &CompanyDb,
    config: &ActuatorConfig,
    socket_name: &str,
) -> Result<PriorOwnershipObservation, RuntimeLifecycleError> {
    let _ = config;
    let recorded = db.runtime_ownership_read().await?;
    let prior_socket = recorded.socket_name.as_deref().filter(|previous| {
        recorded.status == chiefd_core::store::runtime_owner_rows::RuntimeOwnerStatus::Active
            && *previous != socket_name
    });
    // #751/P8-P10: chiefd cannot look at another daemon's runtime server, and
    // it never could have proven absence there anyway — the audit it used to
    // run resolved to `true` on every untrusted or failed answer, which is
    // every answer a foreign socket can give a process that does not own it.
    // The rule is unchanged and now stated directly: a DIFFERENT active socket
    // is recorded, therefore a prior projection may still exist, therefore this
    // daemon must not take the company from it. Absence is never proven by not
    // looking.
    let prior_projection_exists = prior_socket.is_some();
    Ok(PriorOwnershipObservation { prior_projection_exists })
}

/// Claim this company's runtime for the daemon's own socket, gathering the audit
/// inputs first.
async fn claim_ownership(
    db: &CompanyDb,
    config: &ActuatorConfig,
    at: &str,
) -> Result<(), RuntimeLifecycleError> {
    let socket_name = config.socket.clone();
    let observation = observe_prior_ownership(db, config, &socket_name).await?;
    db.runtime_ownership_claim(socket_name, observation.prior_projection_exists, at.to_owned())
        .await?;
    Ok(())
}

// TOMBSTONE (chief-home-is-cwd §4c): `refuse_foreign_ceo_boot_lease` stood
// here. `launch_runtime` and `reconcile_runtime` both called it first, to
// refuse projecting a company while a CEO-only boot on a DIFFERENT runtime
// socket held its lease. There is no CEO-only boot to contend with — the daemon
// launches no pane — so no lease is ever taken and the check could only pass.
// The cross-daemon question it half-asked is answered properly one line further
// down by `runtime_owner`, which is a durable ownership record rather than a
// five-minute window, and which is untouched.

// ---------------------------------------------------------------------------
// The one converge pass
// ---------------------------------------------------------------------------

/// Read the launch-intent fence for the converge cycle's activity projection.
///
/// An absent fence is `Fenced(∅)` — CEO-only, the strictest legal value, never
/// `Unfenced`. No caller may turn a missing row into an eager fleet start.
async fn activity_projection(
    db: &CompanyDb,
    execution_lease_person_ids: &[String],
) -> Result<ActivityProjectionInput, RuntimeLifecycleError> {
    let mut person_ids: BTreeSet<String> = db
        .launch_intent_read()
        .await?
        .map(|(intent, _seq)| intent.person_ids.into_iter().collect())
        .unwrap_or_default();
    // An attended managed command is already executing in an authenticated, live
    // pane. It must be represented in this ONE activity decision so a concurrent
    // duty cannot kill that pane between the durable mutation and Pi's persisted
    // tool result. It is deliberately NOT written to launch intent: the completed
    // projection records `lastDesiredActive`, and the next normal pass turns that
    // into the existing bounded quiet lease before it parks.
    person_ids.extend(execution_lease_person_ids.iter().cloned());
    let maintenance_person_ids = db
        .session_maintenance_read()
        .await?
        .into_iter()
        .flat_map(|(ledger, _seq)| {
            ledger
                .ordered_requests()
                .filter(|request| request.status.is_open())
                .map(|request| request.person_id.clone())
                .collect::<BTreeSet<_>>()
        })
        .collect();
    Ok(ActivityProjectionInput {
        fence: chiefd_core::store::activity::LaunchFence::Fenced(person_ids),
        // The SQL pending-mail fact union is empty: the mailbox is native rows
        // and `reconcile_cycle` reads them itself inside its own commit.
        pending_mail_facts: Vec::new(),
        maintenance_person_ids,
    })
}

/// Run exactly ONE converge cycle and assemble its report from the committed
/// runtime row the cycle itself published.
async fn one_converge_pass(
    db: &CompanyDb,
    config: &ActuatorConfig,
    manifest: &OrganizationManifest,
    execution_lease_person_ids: &[String],
    monitor_warnings: Vec<String>,
) -> Result<RuntimeLaunchReport, RuntimeLifecycleError> {
    let projection = activity_projection(db, execution_lease_person_ids).await?;
    let report: ReconcileReport =
        crate::converge_apply::reconcile_cycle(db, config, ActuationMode::Apply, Some(projection))
            .await?;
    let committed = runtime_state_of(db).await?;
    Ok(RuntimeLaunchReport {
        organization: manifest.slug.clone(),
        socket_name: config.socket.clone(),
        applied: report.applied,
        desired_people: report.desired_people,
        retry_after_floor: report.retry_after_floor,
        process_handles: committed
            .as_ref()
            .map(|state| state.process_handles.clone())
            .unwrap_or_default(),
        monitor_warnings,
        notes: report.notes,
    })
}

// ---------------------------------------------------------------------------
// Launch
// ---------------------------------------------------------------------------

/// Explicit operator/CEO launch of a company and specific nodes.
///
/// Any non-CEO `requested_person_ids` express explicit per-node launch intent and
/// are recorded durably so — and only so — those nodes may run. This is the sole
/// path that opens the fence; every automatic reconcile/wake path leaves it shut.
///
/// **MANDATE 4**: the TypeScript `withOrganizationRuntimeLock` file lock is
/// DELETED, not ported, and no lease replaced it. This pass is serialized by
/// beacond's single-daemon admission, the writer actor, and the converge
/// cycle's own single-flight claim — there is no lock file, no acquisition
/// ladder, and no stale-holder wedge.
///
/// # Errors
/// [`RuntimeLifecycleError`]: an ownership refusal, a materialization failure,
/// or a converge failure.
pub async fn launch_runtime(
    db: &CompanyDb,
    config: &ActuatorConfig,
    options: &LaunchOptions,
) -> Result<RuntimeLaunchReport, RuntimeLifecycleError> {
    let manifest = manifest_of(db).await?;
    claim_ownership(db, config, &options.at).await?;

    // An execution lease is never durable start intent (see `LaunchOptions`).
    let leased: BTreeSet<&String> = options.execution_lease_person_ids.iter().collect();
    for person_id in &options.requested_person_ids {
        if leased.contains(person_id) {
            continue;
        }
        db.start_person(person_id.clone(), options.at.clone(), options.actor.clone()).await?;
    }

    // ALWAYS, and there is no longer a way for a caller to say otherwise.
    //
    // This used to be skippable by a `materialization_ready: bool` the CLIENT
    // supplied on the launch body — "the caller has just materialized". A6 made
    // the auth gate unconditional, and that changed what the field MEANT: this
    // is a path that enrols a company's people into the trust table, so a
    // client asserting readiness launches panes whose people cannot
    // authenticate at all. That is the shape of every off switch this
    // workstream deleted, and it went in the same commit that would have armed
    // it.
    //
    // It costs a stat per person on the steady path: both halves are
    // create-if-absent, so a company whose homes are already there is not
    // touched.
    // The shipped skill root travels with the launch so `ensure_agent_homes`
    // can reconcile the library before it installs out of it. Resolved through
    // `launcher_assets`, the ONE authority on where the checkout is; a failure
    // is a named warning rather than a refused launch, exactly like a home that
    // cannot be written.
    //
    // This path was briefly suspected of wedging the CEO's pane and was
    // MEASURED CLEAR rather than reverted on suspicion: with it removed the
    // pane still failed, and the actuator's own retry line named the real
    // shape — "the pane the actuator started for them was gone by the next
    // converge pass". Left in place, where it belongs.
    let (shipped_skills_root, mut monitor_warnings) = match launcher_assets(db, config).await {
        Ok(assets) => {
            (Some(assets.launcher_root.join("packages").join("piing").join("skills")), Vec::new())
        }
        Err(error) => (
            None,
            vec![format!(
                "the launcher checkout could not be resolved ({error}), so the company skill library was not reconciled and people keep the skills they have"
            )],
        ),
    };
    monitor_warnings
        .extend(ensure_agent_homes(db, &config.dir, shipped_skills_root.as_deref()).await?);
    one_converge_pass(db, config, &manifest, &options.execution_lease_person_ids, monitor_warnings)
        .await
}

/// Re-evaluate durable work and monitor leases without pinning an otherwise idle
/// CEO.
///
/// A thin call into the converge cycle, and nothing else.
///
/// **MANDATE 4 — the advisory reconciliation marker is DELETED, not ported.**
/// This verb used to stamp an `in_progress` marker on the runtime row before the
/// pass, refuse a second caller that saw one, and treat a marker older than 60s
/// as stale and overwrite it. That is an advisory marker plus a repair pass —
/// two banned shapes — and it did not work as a fence either: the guard READ and
/// the marker WRITE were separate writer jobs, so it was a check-then-act with a
/// window between them and two callers could both see "no marker" and both
/// proceed.
///
/// The single-flight it was imitating already exists one call down and is
/// correct: [`crate::converge_apply::reconcile_cycle`] takes
/// `converge_safety::begin_cycle`'s durable claim as an atomic check-and-set
/// inside ONE mutation, so a second pass that begins while one is in flight
/// commits after it, sees the claim and is skipped. Its stale-reclaim window is
/// also 10 minutes rather than 60 seconds — deliberately past any legitimate
/// converge pass, where the marker's 60s would have declared a slow-but-live
/// pass dead and let a second caller in.
///
/// # Errors
/// [`RuntimeLifecycleError`]: a converge failure.
pub async fn reconcile_runtime(
    db: &CompanyDb,
    config: &ActuatorConfig,
) -> Result<RuntimeLaunchReport, RuntimeLifecycleError> {
    let manifest = manifest_of(db).await?;
    one_converge_pass(db, config, &manifest, &[], Vec::new()).await
}

/// `company resume`: converge the company and repair its stuck supervision
/// effects.
///
/// There is nothing to resume FROM — CEO-only is a boot guarantee that ends with
/// the command — so this is an idempotent repair rather than a gate. It must
/// NEVER touch launch intent, and does not: waking the fleet is not a thing any
/// command does; only the CEO naming a person or a unit does that. It must
/// likewise never touch the goal-delivery quiesce watermark in either direction —
/// a crash-resume RETAINS it, so stale pre-reset mail cannot restaff a roster the
/// resume was never asked to grow.
///
/// TOMBSTONE (chief-home-is-cwd §4c): this opened by refusing with
/// `ceo-boot-in-progress` when a CEO boot lease was held. Nothing takes that
/// lease now — the daemon boots no pane — so the refusal had no subject.
///
/// # Errors
/// [`RuntimeLifecycleError`]: [`launch_runtime`]'s errors.
pub async fn resume_supervised_runtime(
    db: &CompanyDb,
    config: &ActuatorConfig,
    options: &LaunchOptions,
) -> Result<RuntimeLaunchReport, RuntimeLifecycleError> {
    // A resume NEVER opens the fence, so the requested ids are dropped here
    // rather than forwarded.
    let report = launch_runtime(
        db,
        config,
        &LaunchOptions { requested_person_ids: Vec::new(), ..options.clone() },
    )
    .await?;
    // A resume is the operator saying "try again". Bounded per effect by
    // `SUPERVISION_EFFECT_REOPEN_LIMIT`, so this can never become a perpetual
    // retry of genuinely poison work.
    db.mutate(
        MutationClass::Normal,
        MutationName("runtime_lifecycle.reopen_failed_effects"),
        move |ledgers| {
            let manifest = chiefd_core::store::organization::read(ledgers)?;
            chiefd_core::store::supervision::reopen_failed_effects(ledgers, &manifest)
        },
    )
    .await?;
    Ok(report)
}

// ---------------------------------------------------------------------------
// TOMBSTONE (chief-home-is-cwd §4c): the daemon-side CEO boot
// ---------------------------------------------------------------------------
//
// `launch_ceo_only_runtime` stood here, with `ceo_only_under_lease`,
// `ceo_only_projection`, `wait_for_ceo_liveness`, `ceo_pane_liveness_error`,
// `ceo_pane_stderr_diagnostic`, `discard_probe_session_residue` and
// `session_entries`. Together they were the whole "the daemon brings the
// company's first pane up itself, and proves it came up" apparatus: take a
// five-minute exclusivity lease, refresh materialization, claim ownership,
// commit the CEO-only fence, tear the fleet down, stamp a clean-session epoch,
// converge once, then wait for the CEO's process to appear in the committed
// runtime row.
//
// It has no subject. The operator client owns every pane; chiefd launches
// nothing, so there is no first pane for it to bring up and no liveness of its
// own to prove. `POST /v1/org/runtime/launch-ceo-only` is deleted with it.
//
// # The exclusion, answered
//
// The lease was a real mutual-exclusion object, so its removal is a decision
// rather than a tidy-up. What it excluded was ONE thing: chiefd's own reconcile
// duty, for the span between this command starting its slow pre-converge work
// (provider preflight, materialization) and committing its projection — a
// window no single transaction covers. `launch_ceo_only_runtime` was the lease's
// only publisher anywhere in the tree, so with this function gone no lease can
// ever be held, and every reader of one was answering a question about an event
// that cannot occur.
//
// Nothing else needed it. Writes were never serialized here: one daemon per
// company (beacond's admission), one writer actor per daemon with a
// `BEGIN IMMEDIATE` per mutation, and `converge_safety::begin_cycle`'s durable
// single-flight claim over every converge pass — attended or duty-driven — are
// the three layers that serialize, and none of them is this lease. The one
// cross-daemon hazard it also spoke to (a second runtime server projecting the
// same company) is answered by the durable `runtime_owner` record, which
// survives untouched and is checked by `claim_ownership` on every projecting
// path.
//
// The activity fence and the never-sleeps rules are NOT affected: they read
// `manifest.chief_person_id()`, a derived accessor over the root department's
// head, and never touched this lease.

// ---------------------------------------------------------------------------
// Stop
// ---------------------------------------------------------------------------

/// Explicit company stop.
///
/// arch Step 7: a company stop is a COMMITTED FACT the daemon converges on, not a
/// runtime command the launcher issues. The runtime row flips to `stopped` FIRST;
/// chiefd's converge duty reads it, plans `StopSession` itself, and kills the
/// session. The publish wakes the duty reactively, so the teardown lands near the
/// commit rather than on the periodic floor.
///
/// Launch intent is withdrawn BEFORE runtime is touched — the same ordering as the
/// idle-park withdrawal — because a stop that left the committed record saying
/// "desired" forever is exactly how a stopped company came back up.
///
/// # Errors
/// [`RuntimeLifecycleError`]: an ownership refusal, or
/// [`RuntimeLifecycleError::StopDidNotConverge`] when the daemon-owned teardown
/// never happened.
pub async fn stop_runtime(
    db: &CompanyDb,
    config: &ActuatorConfig,
    signal: &RuntimeChangeSignal,
    options: &StopOptions,
) -> Result<RuntimeStopReport, RuntimeLifecycleError> {
    let manifest = manifest_of(db).await?;
    // Audit the recorded owner before touching the runtime, but do NOT turn a
    // never-launched company into an active owner merely to stop it.
    db.runtime_ownership_read().await?;

    // Withdrawal only ever NARROWS the set, so a missing or malformed fence is a
    // no-op. Deliberately a row PUBLISH, not the clear route: the publish is what
    // emits the change-feed hint the reconcile duty wakes on.
    // The route layer keeps `router.rs`'s `reconcile_wake` wired to
    // this publish, which is what made `removeOrganizationLaunchIntentPersonIds`
    // reactive rather than periodic.
    if let Some((mut intent, _seq)) = db.launch_intent_read().await? {
        if !intent.person_ids.is_empty() {
            intent.person_ids.clear();
            intent.attributions.clear();
            db.launch_intent_publish(intent).await?;
        }
    }

    // What was standing comes from the LAST COMMITTED OBSERVATION, not from a
    // look chiefd cannot take. `RuntimeState::process_handles` holds only people an
    // actuator vouched were alive, so its key set IS "who this stop is taking
    // down", and an empty one means the last actuator to report saw nobody.
    let committed = runtime_state_of(db).await?;
    let already_stopped = committed
        .as_ref()
        .is_none_or(|state| state.status == "stopped" || state.process_handles.is_empty());
    let stopped_person_ids: Vec<String> = if already_stopped {
        Vec::new()
    } else {
        committed
            .as_ref()
            .map(|state| state.process_handles.keys().cloned().collect())
            .unwrap_or_default()
    };

    if let Some(mut state) = committed {
        state.observed_at = options.at.clone();
        state.status = "stopped".to_owned();
        state.socket_name = config.socket.clone();
        state.process_handles = BTreeMap::new();
        state.reconciliation = None;
        // MANDATE 4: stopped-status and launch-intent absence are ONE commit.
        db.runtime_stop_publish(state, options.at.clone(), options.clear_launch_intent).await?;
    }

    if !already_stopped {
        await_session_absence(
            db,
            signal,
            &manifest,
            Duration::from_millis(STOP_CONVERGENCE_TIMEOUT_MS),
        )
        .await?;
    }
    db.runtime_ownership_release(config.socket.clone(), options.at.clone()).await?;

    Ok(RuntimeStopReport {
        organization: manifest.slug.clone(),
        already_stopped,
        stopped_person_ids,
    })
}

/// Explicit company stop that claims ownership first and leaves the company
/// holding no launch intent, so the next boot is CEO-only.
///
/// **MANDATE 4**: the intent deletion is NOT a follow-up transaction. It rides
/// the same commit as the stopped runtime projection, via
/// `StopOptions::clear_launch_intent`. It used to be `db.launch_intent_clear`
/// called after `stop_runtime` returned — five transactions, one runtime
/// `kill-session` and one bounded session-absence wait later — so the promise
/// in this function's first sentence had a multi-second window in which it was
/// simply false.
///
/// # Errors
/// [`stop_runtime`]'s errors, plus an ownership refusal.
pub async fn stop_supervised_runtime(
    db: &CompanyDb,
    config: &ActuatorConfig,
    signal: &RuntimeChangeSignal,
    options: &StopOptions,
) -> Result<RuntimeStopReport, RuntimeLifecycleError> {
    // Called for its refusal, not its value: a company with no committed
    // manifest is refused here rather than part-way through a teardown.
    manifest_of(db).await?;
    claim_ownership(db, config, &options.at).await?;
    stop_runtime(
        db,
        config,
        signal,
        &StopOptions { at: options.at.clone(), clear_launch_intent: true },
    )
    .await
}

/// Bounded OBSERVATION wait for the daemon-driven session teardown an attended
/// stop committed.
///
/// **MANDATE 1**: reactive, not a poll. The launcher never kills the session
/// itself on this path. A session that outlives the deadline means the duty that
/// owns teardown is not running (a serve-only or dead daemon); that is a loud
/// operator-facing failure, never a silent lingering fleet, and never a reason to
/// re-arm the launcher's kill.
///
/// # Errors
/// [`RuntimeLifecycleError::StopDidNotConverge`] at the deadline. The message
/// text is an operator-facing contract.
pub async fn await_session_absence(
    db: &CompanyDb,
    signal: &RuntimeChangeSignal,
    manifest: &OrganizationManifest,
    timeout: Duration,
) -> Result<(), RuntimeLifecycleError> {
    let converged = await_runtime_state(
        signal,
        timeout,
        || runtime_state_of(db),
        |state| {
            state.is_some_and(|state| state.status == "stopped" && state.process_handles.is_empty())
        },
    )
    .await;
    if converged.is_err() {
        return Err(RuntimeLifecycleError::StopDidNotConverge(format!(
            "Organization '{}' stop did not converge: its runtime is still present {}ms \
             after the stopped fact was committed. chiefd's converge duty owns teardown \
             (arch Step 7); a surviving runtime means that duty is not running on this company — \
             investigate the daemon instead of re-issuing the stop.",
            manifest.slug,
            timeout.as_millis(),
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Model / thinking / migration commands
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Temporary launcher → company handoff — DELETED (#751/P8-P10)
// ---------------------------------------------------------------------------
//
// `close_temporary_launcher_pane`, `CloseTemporaryLauncherPaneInput`,
// `CloseTemporaryLauncherPaneOutcome`, `is_runtime_pane_id` and `checked_runtime`
// are gone, and no route serves them any more.
//
// The whole operation was: read the target session's ownership option, read the
// caller's own pane's socket path / session / ownership tags, list the clients
// attached to that pane, switch each of them to the company session, and kill
// the pane. Every step is a command against a terminal multiplexer, and every
// one of its ten refusals was a proof about the CALLER'S OWN terminal — that
// the socket it named is the socket it is sitting on, that its pane is not
// already inside the managed session, that its pane carries no managed
// ownership tags. A backend cannot check any of that about a client, and a
// client does not need a backend's permission to move its own viewers.
//
// It is not "moved to `chief-cli`" as a port of this code: the client already
// owns the session it is switching to and the pane it is closing, so the
// handoff is a local operation there with no wire call in it at all.

/// The clean-session epoch as a [`std::time::SystemTime`], for the launch
/// catalog's session selection.
pub async fn session_epoch_system_time(
    db: &CompanyDb,
) -> Result<Option<std::time::SystemTime>, ChiefdError> {
    let Some((epoch, _seq)) = db.session_epoch_read().await? else {
        return Ok(None);
    };
    let Some(millis) = chiefd_core::isotime::parse_iso_millis(&epoch.epoch_at) else {
        return Ok(None);
    };
    let millis = u64::try_from(millis).unwrap_or(0);
    Ok(Some(std::time::UNIX_EPOCH + Duration::from_millis(millis)))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The refusal names WHICH source the root came from.
    ///
    /// Both branches, because they call for different actions. A company was
    /// created when the conventional path was still correct, recorded it in its
    /// own `org_settings`, and that value then outranked every daemon setting
    /// forever. Every environment fix resolved the RIGHT root and every one was
    /// discarded — and the message named a path without saying where it came
    /// from, so it read as a configuration problem. That cost a multi-cycle
    /// hunt; naming the source turns it into one query.
    #[test]
    fn an_unusable_launcher_root_names_the_source_that_won() {
        let root = std::path::Path::new("/root/.local/share/tribe-launcher");
        let extensions = root.join("packages/piing/extensions");

        let recorded = super::unusable_launcher_root_detail(
            root,
            "recorded for this company (org_settings.launcherRoot)",
            &extensions,
            super::NO_EXTENSION_SOURCES,
        );
        assert!(recorded.contains("recorded for this company"), "{recorded}");
        // The action that actually works for a recorded root, and the ones
        // that do not — an operator who edits the pointer file learns nothing
        // from a message that only suggests editing the pointer file.
        assert!(recorded.contains("/v1/org/settings/publish"), "{recorded}");
        assert!(recorded.contains("outranks every daemon setting"), "{recorded}");

        let configured = super::unusable_launcher_root_detail(
            root,
            "resolved from this daemon's configuration",
            &extensions,
            super::NO_EXTENSION_SOURCES,
        );
        assert!(configured.contains("resolved from this daemon"), "{configured}");
        // Both name the path and the directory that was missing.
        for detail in [&recorded, &configured] {
            assert!(detail.contains("/root/.local/share/tribe-launcher"), "{detail}");
            assert!(detail.contains("packages/piing/extensions"), "{detail}");
            assert!(detail.contains("org_hire"), "{detail}");
        }
    }

    use super::*;
    use chiefd_core::clock::SystemClock;
    use chiefd_core::store::organization::PersonKind;
    use chiefd_core::test_support::northstar_manifest;

    fn config(root: &Path) -> ActuatorConfig {
        // The operator's own Pi agent directory, with the credential a real one
        // holds: `ensure_agent_home` links every person's `auth.json` at it,
        // and the launch gate refuses a home whose provider links dangle.
        let registry = root.join("registry");
        std::fs::create_dir_all(&registry).expect("operator agent dir");
        crate::materialize::publish_text(&registry.join("auth.json"), "{}", 0o600)
            .expect("the operator's credential");
        ActuatorConfig {
            socket: "cobalt-sock".to_owned(),
            // "watching for ever": the epoch, so an inferred quiet instant is
            // clamped by nothing and every expectation here is the pre-clamp one.
            watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
            dir: root.to_path_buf(),
            home: root.join("home"),
            pi_binary: root.join("pi"),
            floor: Duration::from_secs(5),
            launcher_root: root.join("launcher"),
            root_pi_agent_dir: root.join("registry"),
        }
    }

    fn runtime_state(process_handles: &[(&str, &str)], status: &str) -> RuntimeState {
        RuntimeState {
            version: 1,
            organization: None,
            observed_at: "2026-08-07T00:00:00.000Z".to_owned(),
            session: None,
            socket_name: "cobalt-sock".to_owned(),
            status: status.to_owned(),
            startup_admission_until: None,
            recovery_fingerprint: None,
            recovery_observed_at: None,
            recovery_confirmed: None,
            recovery: None,
            reconciliation: None,
            process_handles: process_handles
                .iter()
                .map(|(person, pane)| ((*person).to_owned(), (*pane).to_owned()))
                .collect(),
            monitor_warnings: Vec::new(),
            missing_durable_person_ids: Vec::new(),
            unexpected_observed_person_ids: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    // -----------------------------------------------------------------------
    // MANDATE 1: the wait is reactive and bounded by exactly one timeout
    // -----------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_wait_whose_predicate_never_holds_times_out_and_keeps_the_last_row() {
        let signal = RuntimeChangeSignal::new();
        let seen = runtime_state(&[("chief", "%1")], "running");
        let reads = std::cell::Cell::new(0_usize);
        let timeout = await_runtime_state(
            &signal,
            Duration::from_millis(2_500),
            || {
                reads.set(reads.get() + 1);
                let seen = seen.clone();
                async move { Ok(Some(seen)) }
            },
            |state| {
                state.is_some_and(|state| {
                    state.process_handles.get("chief").map(String::as_str) == Some("%9")
                })
            },
        )
        .await
        .expect_err("the predicate never holds");
        assert_eq!(
            timeout.last_seen.as_ref().map(|state| state.status.as_str()),
            Some("running"),
            "the deadline carries the last row it saw, so a diagnostic uses real evidence",
        );
        // Reactive, not a poll: with no nudge the wait reads ONCE and parks until
        // the deadline. A polling implementation would read many times.
        assert_eq!(reads.get(), 1, "a wait with no change-feed frame reads exactly once");
    }

    #[tokio::test(start_paused = true)]
    async fn a_nudge_re_reads_and_resolves_without_any_sleep() {
        let signal = RuntimeChangeSignal::new();
        let nudger = signal.clone();
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&ready);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            nudger.nudge();
        });
        let ready_for_read = Arc::clone(&ready);
        let resolved = await_runtime_state(
            &signal,
            Duration::from_secs(30),
            || {
                let ready = Arc::clone(&ready_for_read);
                async move {
                    Ok(ready
                        .load(std::sync::atomic::Ordering::SeqCst)
                        .then(|| runtime_state(&[("chief", "%1")], "running")))
                }
            },
            |state| state.is_some(),
        )
        .await
        .expect("the nudge resolves the wait");
        assert_eq!(resolved.map(|state| state.status), Some("running".to_owned()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_failure_never_rejects_a_wait_only_the_deadline_does() {
        let signal = RuntimeChangeSignal::new();
        let timeout = await_runtime_state(
            &signal,
            Duration::from_millis(50),
            || async {
                Err(chiefd_core::error::store_failure_because(
                    "runtime-rows",
                    "injected by the test",
                ))
            },
            |_| false,
        )
        .await
        .expect_err("only the deadline ends the wait");
        assert_eq!(timeout.last_seen, None, "a failed read contributes no evidence");
    }

    #[tokio::test(start_paused = true)]
    async fn the_feed_sink_wakes_a_parked_wait_only_for_runtime_commits() {
        let signal = RuntimeChangeSignal::new();
        let sink = signal.feed_sink();
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&ready);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            // An unrelated store must not be the thing that wakes the wait.
            sink("northstar", "mailbox", "", "", false);
            tokio::task::yield_now().await;
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            sink("northstar", RUNTIME_STORE, "", "", false);
        });
        let ready_for_read = Arc::clone(&ready);
        let resolved = await_runtime_state(
            &signal,
            Duration::from_secs(30),
            || {
                let ready = Arc::clone(&ready_for_read);
                async move {
                    Ok(ready
                        .load(std::sync::atomic::Ordering::SeqCst)
                        .then(|| runtime_state(&[], "stopped")))
                }
            },
            |state| state.is_some_and(|state| state.status == "stopped"),
        )
        .await
        .expect("the runtime frame resolves the wait");
        assert_eq!(resolved.map(|state| state.status), Some("stopped".to_owned()));
    }

    // TOMBSTONE (chief-home-is-cwd §4c): three tests stood here —
    // `a_liveness_timeout_is_reported_with_the_contract_message`,
    // `a_missing_pane_is_reported_before_any_wait_is_attempted` and
    // `the_stderr_tail_joins_the_exit_code_and_the_last_three_lines`. They
    // pinned the operator-facing wording of a failure only the daemon-side CEO
    // boot could produce, and the bounded stderr tail it attached. Both the
    // boot and its `ceo-pane-not-live` refusal are deleted: chiefd launches no
    // pane, so it has no launch of its own to report the death of.

    // -----------------------------------------------------------------------
    // Recovery confirmation
    // -----------------------------------------------------------------------

    #[test]
    fn the_recovery_fingerprint_is_order_independent_but_content_sensitive() {
        let a = recovery_fingerprint(&["b".to_owned(), "a".to_owned()], &["z".to_owned()]);
        let b = recovery_fingerprint(&["a".to_owned(), "b".to_owned()], &["z".to_owned()]);
        assert_eq!(a, b, "the same problem in a different order is the same fingerprint");
        let different = recovery_fingerprint(&["a".to_owned()], &["z".to_owned()]);
        assert_ne!(a, different, "a different missing set is a different problem");
    }

    #[test]
    fn one_mismatch_is_never_actionable_and_a_persisted_one_is() {
        let first = 1_000_000_i64;
        let window = i64::try_from(RUNTIME_OBSERVATION_CONFIRMATION_MS).expect("fits");
        assert!(
            !recovery_is_confirmed(first, first),
            "a single observation may have raced a runtime server restart",
        );
        assert!(!recovery_is_confirmed(first + window - 1, first));
        assert!(recovery_is_confirmed(first + window, first));
    }

    // -----------------------------------------------------------------------
    // Paths, collaborators, and the handoff
    // -----------------------------------------------------------------------

    /// An agent home hangs off `<dir>/.chief/agent/<id>`, but the Chief is not
    /// an agent and never receives one.
    ///
    /// Asserted as the composed path rather than "somewhere under dir" because
    /// the two differ by exactly one segment: while `ActuatorConfig` stored the
    /// `.chief` root and the pane env stamp read that same field, every pane
    /// was told its company directory was `<dir>/.chief` and every reader that
    /// joins onto it looked one `.chief` too deep.
    #[test]
    fn an_agent_home_hangs_off_the_chief_folder_inside_the_company_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = config(dir.path());
        let home = crate::agent_home::agent_home(&config.dir, "quant-head");
        assert_eq!(home, dir.path().join(".chief").join("agent").join("quant-head"));
        assert_eq!(home.parent().and_then(Path::parent), Some(config.data_root().as_path()));
        assert_ne!(config.data_root(), config.dir, "the two are one segment apart");
    }

    // DELETED with the handoff itself (#751/P8-P10), and named here rather
    // than vanishing:
    //
    // * `a_handoff_refuses_a_malformed_pane_id_a_foreign_role_and_a_foreign_pane`
    // * `a_handoff_refuses_when_the_target_session_is_not_tagged_for_this_company`
    // * `pane_id_shape_is_exactly_percent_digits`
    //
    // All three pinned refusals inside `close_temporary_launcher_pane`, which
    // is gone: it read the CALLER's own pane id, socket path and ownership tags
    // out of a terminal chiefd can no longer see, to decide whether that caller
    // was allowed to close its own pane. The subject is not "moved" — the
    // operator client owns both the session and the pane, so there is no
    // refusal left for a backend to make and nothing here to re-express.

    // -----------------------------------------------------------------------
    // Person contracts are DERIVED at materialization, never demanded of the
    // caller.
    // -----------------------------------------------------------------------

    /// A company whose manifest is committed but whose person-contracts
    /// projection has never been built.
    ///
    /// This is the state a company is in after ANY manifest mutation — hire,
    /// transfer, restructure — because the contracts document is a projection
    /// of the manifest and nothing rebuilt it.
    fn company_with_manifest_but_no_contracts(
        dir: &Path,
        manifest: &OrganizationManifest,
    ) -> Arc<CompanyDb> {
        let path = dir.join(chiefd_core::store::COMPANY_DB_FILENAME);
        {
            let mut connection = chiefd_core::store::open_company_db(&path).expect("open seed db");
            let transaction = connection.transaction().expect("seed transaction");
            chiefd_core::store::organization_rows::genesis(&transaction, &manifest.slug, manifest)
                .expect("seed manifest");
            transaction.commit().expect("commit seed manifest");
        }
        Arc::new(
            CompanyDb::open(&manifest.slug, &path, Arc::new(SystemClock::default()))
                .expect("open company db"),
        )
    }

    /// The defect: materializing a company refused with `person-contract-absent`
    /// whenever the manifest named somebody the contracts projection did not.
    /// Hiring anybody produced exactly that state, so after one hire NOBODY's
    /// home could be written — not the new person's, and not the chief's
    /// either, because the refusal aborted the whole roster.
    ///
    /// [`ensure_agent_homes`] DERIVES the contract from the manifest for the
    /// same reason, and this pins it at the state a hire actually leaves: a
    /// committed manifest with no contracts projection at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn homes_are_written_from_a_derived_contract_rather_than_a_stored_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut manifest = northstar_manifest(0);
        manifest.slug = "cobalt".to_owned();
        let db = company_with_manifest_but_no_contracts(dir.path(), &manifest);

        // Precondition: the projection really is absent, so this cannot pass by
        // testing a company that was already fine.
        assert!(
            db.org_person_contracts_read().await.expect("contracts read").is_none(),
            "fixture must start with NO contracts projection"
        );

        let config = config(dir.path());
        let warnings = ensure_agent_homes(&db, &config.dir, None).await.expect("homes");
        assert_eq!(warnings, Vec::<String>::new(), "nothing to contain: {warnings:?}");

        let chief_person_id = manifest.chief_person_id().expect("chief");
        assert!(
            !crate::agent_home::agent_home(dir.path(), chief_person_id).exists(),
            "the Chief is the operator Pi and must not get an agent home"
        );

        for person_id in manifest.people_order.iter().filter(|id| id.as_str() != chief_person_id) {
            let guide = crate::agent_home::agent_home(dir.path(), person_id).join("AGENTS.md");
            let text = std::fs::read_to_string(&guide)
                .unwrap_or_else(|_| panic!("no AGENTS.md written for {person_id}"));
            assert!(!text.trim().is_empty(), "{person_id}'s contract must not be empty");
            assert!(
                text.contains(&manifest.people[person_id].title),
                "{person_id}'s AGENTS.md must be THEIR contract, not a shared one: {text}"
            );
        }
    }

    /// A GREEN WORKSPACE SUITE SAT ON TOP OF A COMPANY THAT WOULD NOT BOOT.
    ///
    /// `cargo test --workspace` passed with 0 failures while a live company's
    /// CEO died 16 times in 3m23s, because every assertion about the launcher
    /// root stopped at the same place the product did: `packages/piing/extensions`
    /// is a directory, therefore the root is usable. It is committed SOURCE.
    /// The extensions in it import `@chief/piing/extension-runtime`, which that
    /// package's `exports` map resolves to `dist/extensionruntime/index.js` — a
    /// BUILD PRODUCT — and an unbuilt checkout has one and not the other. Pi
    /// then refused both extensions and exited status 1 during start-up, and
    /// the only sentence any surface carried was the actuator's "the pane the
    /// actuator started for them was gone by the next converge pass".
    ///
    /// So the fixture is exactly that checkout: sources present, `dist` absent.
    /// The second half is the half that makes it a probe of the build rather
    /// than of the path — the same root, built, is accepted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unbuilt_launcher_checkout_is_refused_before_a_pane_is_started() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut manifest = northstar_manifest(0);
        manifest.slug = "cobalt".to_owned();
        let db = company_with_manifest_but_no_contracts(dir.path(), &manifest);
        let config = config(dir.path());

        // A real checkout as far as the old probe could tell.
        let piing = config.launcher_root.join("packages").join("piing");
        crate::materialize::publish_text(
            &piing.join("extensions").join("organization-intercom.ts"),
            "import { OrganizationTools } from \"@chief/piing/extension-runtime\";\n",
            0o644,
        )
        .expect("the extension that imports the runtime");
        crate::materialize::publish_text(
            &piing.join("package.json"),
            "{\"exports\":{\"./extension-runtime\":\"./dist/extensionruntime/index.js\"}}\n",
            0o644,
        )
        .expect("the exports map that cannot resolve");

        let error = launcher_assets(&db, &config).await.expect_err(
            "an unbuilt checkout must be refused AT THE LAUNCH; discovering it from a dying \
             pane's stderr is what cost a whole session",
        );
        let text = format!("{error}");
        // The path that was missing, so an operator does not have to guess
        // which half of the checkout is absent.
        assert!(text.contains("dist/extensionruntime/index.js"), "{text}");
        // The exact sentence Pi prints, so the refusal and the pane agree.
        assert!(text.contains("'./extension-runtime' is not defined by exports"), "{text}");
        // And what to DO: the checkout is fine, the build never ran.
        assert!(text.contains("never built"), "{text}");

        // BUILDING THE RUNTIME IS NOT ENOUGH, AND THIS IS THE SECOND HALF OF
        // THE SAME OUTAGE. With the file present the old probe is satisfied,
        // and the specifier still resolves against nothing — which is exactly
        // the tree a release shipped, and exactly why every person in a live
        // company crash-looped with a blank cause.
        crate::materialize::publish_text(
            &piing.join("dist").join("extensionruntime").join("index.js"),
            "export const organizationTools = [];\n",
            0o644,
        )
        .expect("the built extension runtime");
        let error = launcher_assets(&db, &config).await.expect_err(
            "a runtime with no package identity must be refused AT THE LAUNCH: the file \
             exists, the name the extensions import it by does not",
        );
        let text = format!("{error}");
        assert!(text.contains("node_modules"), "{text}");
        // The exact sentence Pi prints, so the refusal and the pane agree —
        // and it is NOT the exports-map sentence the first half asserts.
        assert!(text.contains("Cannot find module '@chief/<package>/extension-runtime'"), "{text}");
        assert!(!text.contains("is not defined by exports"), "{text}");

        // THE SAME ROOT, BUILT AND RESOLVABLE, IS USABLE. Without this the
        // probes could be refusing the checkout rather than what is missing.
        // BOTH, because the probe checks both -- and seeding only the first
        // here would leave this test asserting a pass that a real launch does
        // not get, which is the fixture lying in the product's favour.
        for package in ORGANIZATION_EXTENSION_PACKAGES {
            std::fs::create_dir_all(
                config.launcher_root.join("node_modules").join("@chief").join(package),
            )
            .expect("the package identity a checkout gets from `bun install`");
        }
        let assets = launcher_assets(&db, &config).await.expect("a built checkout is usable");
        assert_eq!(assets.launcher_root, config.launcher_root);
    }

    /// H.2.5: a RECORDED launcher root that is the install's own versioned
    /// resource path is refused — never used — because a later `chief upgrade`
    /// prunes `versions/<v>` and materialization reads `resources/` by path.
    /// The refusal must fire on the RECORDED value alone: `config.launcher_root`
    /// is legitimately of this shape on an installed box and is tested usable
    /// above, so this seeds ONLY the override.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recorded_install_versioned_launcher_root_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut manifest = northstar_manifest(0);
        manifest.slug = "cobalt".to_owned();
        let db = company_with_manifest_but_no_contracts(dir.path(), &manifest);
        // Seed the override directly to an exe-derived install path — the store
        // does not police this; the read side does.
        db.org_settings_publish_launcher_root(
            "2026-08-25T00:00:00.000Z".to_owned(),
            "/home/me/.chief/versions/2.0.7/resources".to_owned(),
        )
        .await
        .expect("seed the recorded override");

        let config = config(dir.path());
        let error = launcher_assets(&db, &config)
            .await
            .expect_err("a recorded install path must be refused, not materialized from");
        let text = format!("{error}");
        assert!(text.contains("versions/"), "the refusal must name the shape: {text}");
        assert!(text.contains("re-pin"), "the refusal must say how to recover: {text}");
    }

    // ── the takeover gate's one live observation ─────────────────────────────
    //
    // `observe_prior_ownership` had no test, which is how it went on returning
    // `supervisor: None` unconditionally — the row it probed had had no writer
    // since #825 retired the detached org-supervisor. With the probe gone,
    // `prior_projection_exists` IS the gate, so both of its directions are
    // pinned: a DIFFERENT active socket means a projection may still exist and
    // the company may not be taken, while our own socket and a never-claimed
    // company are both free.

    /// A company whose ownership row records a claim on `socket` in `status`.
    async fn company_owning(
        dir: &Path,
        manifest: &OrganizationManifest,
        socket: &str,
        status: chiefd_core::store::runtime_owner_rows::RuntimeOwnerStatus,
    ) -> Arc<CompanyDb> {
        use chiefd_core::store::runtime_owner_rows::RuntimeOwnerStatus;
        let db = company_with_manifest_but_no_contracts(dir, manifest);
        db.runtime_owner_publish(chiefd_core::store::runtime_owner_rows::RuntimeOwner {
            version: 1,
            organization: manifest.slug.clone(),
            status,
            socket_name: Some(socket.to_owned()),
            claimed_at: Some("2026-08-10T00:00:00.000Z".to_owned()),
            validated_at: Some("2026-08-10T00:00:00.000Z".to_owned()),
            released_at: (status == RuntimeOwnerStatus::Released)
                .then(|| "2026-08-10T00:00:01.000Z".to_owned()),
            extra: Default::default(),
        })
        .await
        .expect("ownership row publishes");
        db
    }

    fn cobalt() -> OrganizationManifest {
        let mut manifest = northstar_manifest(0);
        manifest.slug = "cobalt".to_owned();
        manifest
    }

    /// A foreign socket holds an ACTIVE claim. chiefd cannot look at another
    /// daemon's runtime server, so absence is not proven and the takeover is
    /// refused. This is the direction that actually protects a running company.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_foreign_active_claim_is_observed_as_still_projecting() {
        use chiefd_core::store::runtime_owner_rows::RuntimeOwnerStatus;
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = cobalt();
        let db = company_owning(
            dir.path(),
            &manifest,
            "held-by-someone-else",
            RuntimeOwnerStatus::Active,
        )
        .await;

        let observed =
            observe_prior_ownership(&db, &config(dir.path()), "ours").await.expect("observation");
        assert!(
            observed.prior_projection_exists,
            "another socket's active claim must block the takeover"
        );
    }

    /// The other direction: a RELEASED claim on a foreign socket is takeable.
    /// Without this the fix could degenerate into "never take anything over".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_released_foreign_claim_is_observed_as_gone() {
        use chiefd_core::store::runtime_owner_rows::RuntimeOwnerStatus;
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = cobalt();
        let db = company_owning(
            dir.path(),
            &manifest,
            "held-by-someone-else",
            RuntimeOwnerStatus::Released,
        )
        .await;

        let observed =
            observe_prior_ownership(&db, &config(dir.path()), "ours").await.expect("observation");
        assert!(
            !observed.prior_projection_exists,
            "a released claim is not a live projection — the gate must still permit a takeover"
        );
    }

    /// The socket asking is never its own prior owner: re-validating a claim
    /// this daemon already holds must not audit itself into a refusal.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn our_own_active_claim_is_not_a_prior_projection() {
        use chiefd_core::store::runtime_owner_rows::RuntimeOwnerStatus;
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = cobalt();
        let db = company_owning(dir.path(), &manifest, "ours", RuntimeOwnerStatus::Active).await;

        let observed =
            observe_prior_ownership(&db, &config(dir.path()), "ours").await.expect("observation");
        assert!(
            !observed.prior_projection_exists,
            "the socket asking is never its own prior owner"
        );
    }

    /// A company that never claimed a runtime has nothing to take over.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_company_that_never_claimed_has_no_prior_projection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = cobalt();
        let db = company_with_manifest_but_no_contracts(dir.path(), &manifest);

        let observed =
            observe_prior_ownership(&db, &config(dir.path()), "ours").await.expect("observation");
        assert!(!observed.prior_projection_exists, "absence of a claim is not a live projection");
    }

    // -----------------------------------------------------------------------
    // Create once, and never again
    // -----------------------------------------------------------------------
    //
    // TOMBSTONE: the whole `HiringCompany` family — `a_cold_company_at_genesis_
    // warns_about_nobody`, `hiring_people_who_have_never_started_warns_about_
    // nobody`, `a_person_behind_the_source_is_replaced_by_the_hash_and_warns_
    // nobody` and `a_genesis_or_a_hire_warns_about_nobody`. Each pinned what
    // the source-to-disk extension drift scan REPORTED across a refresh, and
    // both the scan and the refresh are deleted: nothing is copied into a
    // person's home, so no file there can be behind the checkout.
    //
    // What replaces them is the inverse property, and it is the one this whole
    // stage rests on: chief writes a home once and then does not touch it.

    /// Hire `person_id` into the executive root, exactly as `org_hire` does.
    async fn hire(db: &CompanyDb, person_id: &str) {
        let slug = db.label().to_owned();
        let person_id = person_id.to_owned();
        db.in_transaction(MutationClass::Normal, MutationName("test.hire"), move |tx| {
            let empty: Vec<String> = Vec::new();
            chiefd_core::store::org_ops::hire_person(
                tx,
                &slug,
                &person_id,
                "executive",
                &chiefd_core::store::organization_rows::NewPersonSeed {
                    name: &person_id,
                    title: "Analyst",
                    mandate: "Own assigned work.",
                    kind: PersonKind::Worker,
                    employment_state: chiefd_core::store::organization::EmploymentState::Active,
                    activation: "resident",
                    tools: &empty,
                    prompts: &empty,
                },
                "chief",
                "2026-08-10T00:00:00.000Z",
                "test",
            )
            .map_err(|error| ChiefdError::refused("fixture-hire-failed", error.to_string()))
            .map(|_| ())
        })
        .await
        .expect("hire");
    }

    /// A company with one person's home already written, so the SECOND pass is
    /// the subject.
    async fn company_with_homes(
        dir: &Path,
    ) -> (Arc<CompanyDb>, ActuatorConfig, OrganizationManifest) {
        let mut manifest = northstar_manifest(0);
        manifest.slug = "cobalt".to_owned();
        let db = company_with_manifest_but_no_contracts(dir, &manifest);
        let config = config(dir);
        ensure_agent_homes(&db, &config.dir, None).await.expect("the first pass");
        (db, config, manifest)
    }

    /// A shipped skill tree, as the launcher checkout carries it.
    fn shipped_skills(dir: &Path) -> PathBuf {
        let root = dir.join("shipped").join("packages").join("piing").join("skills");
        for skill in ["manager", "worker", "founder"] {
            let entry = root.join(skill);
            std::fs::create_dir_all(&entry).expect("shipped skill");
            crate::files::publish_atomically(
                &entry.join("SKILL.md"),
                &format!("---\nname: {skill}\n---\n"),
                0o644,
            )
            .expect("SKILL.md");
        }
        root
    }

    /// THE PASS THAT ACTUALLY RUNS RECONCILES THE LIBRARY.
    ///
    /// This test exists because the first version of this change put the
    /// reconcile on the LAUNCH path, beside this call rather than inside it —
    /// and on a live company restart it silently never ran. Every person stayed
    /// on the retired flat `skills` symlink, all five deleted skills stayed
    /// readable, and no surface said so, because the only thing that would have
    /// spoken was a failure warning and nothing failed.
    ///
    /// So the library and the links that point into it are written by ONE call,
    /// and this asserts the whole result of that call rather than the reconcile
    /// in isolation: a unit test of `reconcile_project_skills` alone was green
    /// throughout the live failure.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_home_pass_reconciles_the_library_and_installs_every_role() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shipped = shipped_skills(dir.path());
        let mut manifest = northstar_manifest(0);
        manifest.slug = "cobalt".to_owned();
        let db = company_with_manifest_but_no_contracts(dir.path(), &manifest);
        let config = config(dir.path());

        // A company created by the PREVIOUS release: the whole library sits at
        // `.pi/skills`, and every home is one flat symlink at it.
        let old_library = config.dir.join(".pi").join("skills");
        for retired in ["organization-management", "market-data", "project-status-reporting"] {
            std::fs::create_dir_all(old_library.join(retired)).expect("retired skill");
        }

        assert_eq!(
            ensure_agent_homes(&db, &config.dir, Some(shipped.as_path())).await.expect("the pass"),
            Vec::<String>::new(),
            "a reconcile that warns is a reconcile that did not happen"
        );

        // The library.
        let library = crate::project_skills::company_skill_library(&config.dir);
        let mut held: Vec<String> = std::fs::read_dir(&library)
            .expect("library")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        held.sort();
        assert_eq!(held, vec!["manager".to_string(), "worker".to_string()]);

        // The CEO's own install, and the retired skills gone from it.
        let chief_skills = crate::project_skills::chief_skills_root(&config.dir);
        let mut chief_held: Vec<String> = std::fs::read_dir(&chief_skills)
            .expect("chief skills")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        chief_held.sort();
        assert_eq!(chief_held, vec!["manager".to_string()], "the Chief is a manager");
        assert!(
            std::fs::read_to_string(chief_skills.join("manager/SKILL.md"))
                .expect("readable through the link")
                .contains("name: manager"),
            "the Chief's skill must resolve THROUGH the link"
        );

        // Every person, by kind, with the retired flat link replaced.
        let manifest = manifest_of(&db).await.expect("manifest");
        let chief_person_id = manifest.chief_person_id().expect("chief").to_owned();
        let mut seen_manager = false;
        let mut seen_worker = false;
        for person_id in &manifest.people_order {
            if person_id == &chief_person_id {
                continue;
            }
            let person = manifest.person(person_id).expect("person");
            if !crate::materialize::is_employed(person) {
                continue;
            }
            let skills = crate::agent_home::agent_home(&config.dir, person_id).join(".pi/skills");
            assert!(
                std::fs::symlink_metadata(&skills).expect("skills").is_dir(),
                "{person_id}: the retired flat symlink must be gone"
            );
            let held: Vec<String> = std::fs::read_dir(&skills)
                .expect("installs")
                .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
                .collect();
            let expected = crate::agent_home::RoleSkill::of(person.kind).directory_name();
            assert_eq!(held, vec![expected.to_string()], "{person_id} installs exactly its role");
            assert!(
                std::fs::read_to_string(skills.join(expected).join("SKILL.md"))
                    .expect("readable through the link")
                    .contains(&format!("name: {expected}")),
                "{person_id}'s skill must resolve THROUGH the link into the library"
            );
            match person.kind {
                PersonKind::Worker => seen_worker = true,
                _ => seen_manager = true,
            }
        }
        assert!(seen_manager && seen_worker, "the fixture must cover both roles");
    }

    /// The recursive `(path, mtime, contents-or-link-target)` listing of a
    /// directory, so "chief changed nothing" is proved by comparison rather
    /// than by a spot check on two files.
    ///
    /// A directory's own mtime is deliberately not read: it moves when a child
    /// is added or removed, which the PATH LIST already catches, and it cannot
    /// be restamped by [`backdate`] the way a file can.
    fn snapshot(root: &Path) -> Vec<(PathBuf, Option<std::time::SystemTime>, String)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(next) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                let meta = std::fs::symlink_metadata(&path).expect("stat");
                if meta.is_symlink() {
                    let target = std::fs::read_link(&path).expect("link");
                    out.push((path, None, target.display().to_string()));
                } else if meta.is_dir() {
                    stack.push(path.clone());
                    out.push((path, None, String::new()));
                } else {
                    let contents = std::fs::read_to_string(&path).unwrap_or_default();
                    out.push((path, Some(meta.modified().expect("mtime")), contents));
                }
            }
        }
        out.sort();
        out
    }

    /// Stamp every regular file under `root` at a fixed instant in the past.
    ///
    /// The alternative is sleeping past the filesystem's mtime granularity,
    /// which `clippy.toml` bans and which is a race rather than an assertion.
    /// Backdating turns "chief did not rewrite this" into an exact equality: a
    /// rewrite of even identical bytes stamps the file at NOW, and NOW is never
    /// 2023.
    fn backdate(root: &Path) {
        // Through `nix` rather than `std::fs::File`: `clippy.toml` keeps file
        // handles inside `chiefd_host::executor` (README §5.6), and that holds
        // in fixtures too.
        let stamp = nix::sys::time::TimeVal::new(1_700_000_000, 0);
        let mut stack = vec![root.to_path_buf()];
        while let Some(next) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                let meta = std::fs::symlink_metadata(&path).expect("stat");
                if meta.is_dir() {
                    stack.push(path);
                } else if !meta.is_symlink() {
                    nix::sys::stat::utimes(&path, &stamp, &stamp).expect("set mtime");
                }
            }
        }
    }

    /// THE LOAD-BEARING TEST, and it is the inversion of the deleted
    /// `a_second_no_op_pass_still_checkpoints_every_person`.
    ///
    /// A second pass — and a third, after a real roster mutation that changes
    /// what an agent's contract SAYS — must leave the tree byte-identical and
    /// mtime-identical. The old materializer failed both: even a no-op pass
    /// rewrote `AGENTS.md`, the role skill and the reload contract, deleted
    /// `auth.json`, and wiped and rebuilt `packages/`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_pass_and_a_roster_mutation_change_nothing_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, config, _manifest) = company_with_homes(dir.path()).await;
        let agent_home = crate::agent_home::agent_home(&config.dir, "signal-researcher");

        assert!(
            snapshot(&agent_home).iter().any(|(path, _, _)| path.ends_with("AGENTS.md")),
            "the fixture must have written something to compare"
        );
        // BACKDATED, not slept past. A rewrite of even identical bytes stamps
        // the file at NOW, and NOW is never 2023 — so the equality below is an
        // assertion rather than a race against mtime granularity.
        backdate(&agent_home);
        let before = snapshot(&agent_home);

        assert_eq!(
            ensure_agent_homes(&db, &config.dir, None).await.expect("second pass"),
            Vec::<String>::new()
        );
        assert_eq!(snapshot(&agent_home), before, "a no-op pass must touch nothing at all");
    }

    /// TRANSFER, the mutation the plan names as the one that goes stale on
    /// purpose.
    ///
    /// A contract prints `Department: **<name>**.`, so moving somebody changes
    /// what their contract WOULD say — and their `AGENTS.md` must not move,
    /// because it is the hire-time seed and the live contract is SQL. Asserted
    /// with the derived text in hand, so the test cannot pass by the mutation
    /// having been a no-op.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_transfer_leaves_the_hire_time_agents_md_exactly_as_it_was() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, config, _manifest) = company_with_homes(dir.path()).await;
        let home = crate::agent_home::agent_home(&config.dir, "signal-researcher");
        let seeded = std::fs::read_to_string(home.join("AGENTS.md")).expect("read");
        assert!(seeded.contains("Department: **Quant**."), "{seeded}");
        backdate(&home);
        let before = snapshot(&home);

        db.in_transaction(MutationClass::Normal, MutationName("test.transfer"), move |tx| {
            chiefd_core::store::org_ops::transfer_person(
                tx,
                "cobalt",
                "signal-researcher",
                "it",
                "test",
                "2026-08-10T00:00:00.000Z",
                "chief",
                None,
            )
            .map_err(|error| ChiefdError::refused("fixture-transfer-failed", error.to_string()))
            .map(|_| ())
        })
        .await
        .expect("transfer");

        assert_eq!(
            ensure_agent_homes(&db, &config.dir, None).await.expect("pass after the transfer"),
            Vec::<String>::new()
        );

        // The derived contract really did move.
        let manifest = manifest_of(&db).await.expect("manifest");
        let derived = chiefd_core::store::agent_contracts::person_agents_guide(
            &manifest,
            manifest.people.get("signal-researcher").expect("person"),
        )
        .expect("derive");
        assert_ne!(derived, seeded, "the fixture must make the derived text differ");

        assert_eq!(
            snapshot(&home),
            before,
            "AGENTS.md is the HIRE-TIME contract and goes stale ON PURPOSE; the live one is SQL, \
             and it reaches the agent through the daemon rather than through this file"
        );
    }

    /// Promotion changes authority SCOPE in SQL, not the process capability
    /// set. Every structural tool is available to every person and is fenced
    /// server-side, so changing `Worker` to `Head` must not invent a role grant
    /// or replace a live Pi process. This proof crosses the real organization
    /// writer, the launch catalog, the production launch-hash functions, and
    /// the create-once home pass.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn promotion_changes_scope_but_not_tools_launch_hash_or_the_create_once_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, config, before_manifest) = company_with_homes(dir.path()).await;
        let person_id = "signal-researcher";
        let home = crate::agent_home::agent_home(&config.dir, person_id);
        let before_person = before_manifest.people.get(person_id).expect("worker");
        assert_eq!(before_person.kind, PersonKind::Worker, "the subject must start as a worker");

        backdate(&home);
        let home_before = snapshot(&home);
        let catalog_before = build_launch_catalog(&before_manifest, &config);
        let tools_before = catalog_before.people[person_id].tools.clone();
        assert!(
            tools_before.iter().any(|tool| tool == "org_appoint_department_head"),
            "the worker must already have the structural tool; server-side scope is the gate"
        );
        let command_before = chiefd_core::runtime::launch_hash::launch_command_fingerprint(
            before_person,
            &config.pi_binary.display().to_string(),
        );
        let hash_before = chiefd_core::runtime::launch_hash::desired_launch_hash(
            &chiefd_core::runtime::launch_hash::LaunchInputs {
                organization: &before_manifest.slug,
                person_id,
                launch_command: &command_before,
                extension_digest: "fixed-extension-digest",
            },
        );

        let outcome = db
            .create_department(
                "signal-lab".to_owned(),
                "quant".to_owned(),
                "Signal Lab".to_owned(),
                "Research market signals.".to_owned(),
                chiefd_core::store::org_ops::HeadDecision::AppointExisting {
                    person_id: person_id.to_owned(),
                },
                Vec::new(),
                Some("chief".to_owned()),
                "Promote the signal researcher".to_owned(),
                "2026-08-16T00:00:00.000Z".to_owned(),
                "chief".to_owned(),
            )
            .await
            .expect("department creation");
        assert!(
            matches!(
                outcome,
                chiefd_core::store::org_ops::CreateDepartmentOutcome::Applied {
                    ref department_id
                } if department_id == "signal-lab"
            ),
            "the real SQL promotion must apply: {outcome:?}"
        );

        let after_manifest = manifest_of(&db).await.expect("manifest after promotion");
        let after_person = after_manifest.people.get(person_id).expect("promoted person");
        assert_eq!(after_person.kind, PersonKind::Head);
        assert_eq!(after_person.department_id, "signal-lab");
        assert_eq!(after_manifest.departments["signal-lab"].head_person_id, person_id);

        let catalog_after = build_launch_catalog(&after_manifest, &config);
        let tools_after = &catalog_after.people[person_id].tools;
        assert_eq!(
            tools_after, &tools_before,
            "promotion changes server-side scope, not the catalog's tool grant"
        );
        let command_after = chiefd_core::runtime::launch_hash::launch_command_fingerprint(
            after_person,
            &config.pi_binary.display().to_string(),
        );
        let hash_after = chiefd_core::runtime::launch_hash::desired_launch_hash(
            &chiefd_core::runtime::launch_hash::LaunchInputs {
                organization: &after_manifest.slug,
                person_id,
                launch_command: &command_after,
                extension_digest: "fixed-extension-digest",
            },
        );
        assert_eq!(
            hash_after, hash_before,
            "promotion alone must not force the actuator to relaunch a live pane"
        );

        assert_eq!(
            ensure_agent_homes(&db, &config.dir, None).await.expect("home pass after promotion"),
            Vec::<String>::new()
        );
        // THE ONE THING A PROMOTION MUST CHANGE, AND THE ONLY ONE.
        //
        // This assertion used to be a flat `snapshot(&home) == home_before`,
        // and it was right for a product in which role reached the home not at
        // all: every person linked the whole company skill tree, so a promotion
        // genuinely had nothing to write. Role IS the installed skill set now,
        // so a promotion MUST swap it — a head still holding `worker` is the
        // defect, not the invariant.
        //
        // The rest of the home is still asserted byte for byte, because
        // everything else about "a promotion does not rebuild a home" survives:
        // no relaunch, no new identity, no theme churn, no re-linked
        // credentials. Only the role skill and the role contract move with it.
        let home_after = snapshot(&home);
        let moved: Vec<&std::path::PathBuf> = home_after
            .iter()
            .zip(home_before.iter())
            .filter(|(after, before)| after != before)
            .map(|(after, _)| &after.0)
            .collect();
        let changed_or_added: std::collections::BTreeSet<String> = home_after
            .iter()
            .filter(|entry| !home_before.contains(entry))
            .map(|entry| entry.0.strip_prefix(&home).unwrap_or(&entry.0).display().to_string())
            .collect();
        assert_eq!(
            changed_or_added,
            ["AGENTS.md".to_string(), ".pi/skills/manager".to_string()].into_iter().collect(),
            "a promotion swaps the installed skill and republishes the contract, and nothing else; moved={moved:?}"
        );
        let removed: std::collections::BTreeSet<String> = home_before
            .iter()
            .filter(|entry| !home_after.contains(entry))
            .map(|entry| entry.0.strip_prefix(&home).unwrap_or(&entry.0).display().to_string())
            .collect();
        assert_eq!(
            removed,
            ["AGENTS.md".to_string(), ".pi/skills/worker".to_string()].into_iter().collect(),
            "the worker skill is UNINSTALLED, not left beside the manager one"
        );
        let contract = std::fs::read_to_string(home.join("AGENTS.md")).expect("contract");
        assert!(
            contract.starts_with("# Department head — "),
            "the promoted person's contract must be the manager one: {contract}"
        );
        assert!(contract.contains("You do not do the work"), "{contract}");
    }

    /// A person hired after the first pass gets a home on the next one, and
    /// nobody else's is touched. This is the defect
    /// `every_person_creating_route_materializes_the_roster_it_just_changed`
    /// guards from the route side: a person row with no home is a person the
    /// actuator refuses on every pass, for ever.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_person_hired_later_gets_a_home_of_their_own() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, config, _manifest) = company_with_homes(dir.path()).await;
        assert!(!crate::agent_home::agent_home(&config.dir, "rae").exists());

        hire(&db, "rae").await;
        assert_eq!(
            ensure_agent_homes(&db, &config.dir, None).await.expect("pass"),
            Vec::<String>::new()
        );

        let home = crate::agent_home::agent_home(&config.dir, "rae");
        assert!(
            home.join("AGENTS.md").is_file(),
            "the new hire must get a home, or the actuator refuses them for ever"
        );
        assert!(home.join("sessions").is_dir());
        assert!(
            crate::agent_home::identity_key_path(&config.dir, "rae").is_file(),
            "and an identity, or they can prove nothing"
        );
    }

    /// An agent identity key is minted INSIDE the home, and the home is written
    /// first. The Chief is the one explicit exception.
    ///
    /// The order is the whole point: `ensure_agent_home` returns immediately
    /// when the folder exists, so a key minted into an absent home would create
    /// the folder and make the home writer skip a home it never wrote — an
    /// agent with a credential, no `AGENTS.md`, no `skills` link and no
    /// `auth.json`. Asserted as "the key AND the tree are both there", because
    /// that is the state the wrong order cannot reach.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_identity_key_lands_inside_a_fully_built_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_db, config, _manifest) = company_with_homes(dir.path()).await;
        let home = crate::agent_home::agent_home(&config.dir, "signal-researcher");

        assert!(
            crate::agent_home::identity_key_path(&config.dir, "signal-researcher").is_file(),
            "the agent must be able to authenticate"
        );
        assert!(home.join("AGENTS.md").is_file(), "and the home around the key must be real");
        // `.pi/skills` is a real DIRECTORY holding the person's one role skill.
        // It sits in PROJECT scope because the home is the cwd and no longer a
        // Pi agent dir. The point of the assertion is unchanged: a key-first
        // order would have created the folder and made the home writer skip it,
        // leaving an agent with a key and no role at all.
        assert!(
            std::fs::symlink_metadata(home.join(".pi/skills")).is_ok_and(|m| m.is_dir()),
            "including the role skill a key-first order would have skipped"
        );
        assert!(
            std::fs::symlink_metadata(home.join(".pi/skills/worker")).is_ok_and(|m| m.is_symlink()),
            "a worker's home installs the worker skill"
        );
        assert!(
            !crate::agent_home::agent_home(&config.dir, "chief").exists(),
            "the Chief runs as the operator's Pi and has no materialized home"
        );
        assert!(
            crate::agent_home::chief_identity_key_path(&config.dir).is_file(),
            "the Chief's company credential belongs directly under .chief"
        );
    }
}
