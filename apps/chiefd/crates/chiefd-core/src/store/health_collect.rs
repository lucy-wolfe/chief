//! The health-monitor **collection** pass — duty #9's sampling layer.
//!
//! Port of the sampling half of the deleted `org-health-monitor.ts`'s
//! (formerly `apps/cli/src/legacy/organization/`, retired whole by
//! #825/E8-S3) `collectIncidents`: the pass that reads the company's durable and observed
//! state each cycle and emits [`IncidentCandidate`]s for the already-landed
//! [`health`](crate::store::health) fold ([`apply_cycle`](crate::store::health::apply_cycle)).
//! The fold owns dedup, confirmation-gating, truncation and terminal-resolution;
//! this module owns *what an observation means* — the thresholds and the
//! per-source incident kinds.
//!
//! # Pure over a gathered snapshot
//!
//! Every I/O the TypeScript performs inline (load the supervisor state, read the
//! runtime document, run a runtime audit, list dead processes, walk the
//! supervision effects) is here a field of
//! [`HealthCollectionSnapshot`]. The chiefd-host gatherer that runs runtime /
//! `/proc` / reads the log files and assembles this snapshot behind
//! `HostExecutor` is a thin, deferred host concern; the decision logic below is
//! pure and directly red/green testable, matching how the reconciler and the
//! other Half-2 duties split (pure core planner + host I/O).
//!
//! # Do not pre-redact
//!
//! Candidates carry **raw** `detail`; [`apply_cycle`](crate::store::health::apply_cycle)
//! redacts it once, up front, and the fingerprint hashes the redacted result.
//! That is the [`IncidentCandidate`] contract — a caller that pre-redacted would
//! double-redact and could disagree with the fold on the fingerprint. The
//! TypeScript's inline `redact(...)` calls are therefore intentionally absent
//! here; the fold is the one redaction site.
//!
//! # Two moving-detail rules ported exactly
//!
//! * A kind that interpolates a moving `${minutes}m ago` into its detail must
//!   supply a stable [`IncidentCandidate::identity`] — otherwise the
//!   fingerprint churns every pass and the CEO is re-alerted forever.
//!   `supervisor_stale` has that moving-minutes shape and deliberately gets
//!   **no** identity, matching the TypeScript authority.
//! * `supervision_delivery_stalled`/`supervision_delivery_failed` carry
//!   [`IncidentCandidate::impaired_mailbox_person_id`]: an alert routed to the
//!   very person whose mailbox is impaired needs an out-of-band copy later.
//!
//! # Producer parity
//!
//! [`LEGACY_HEALTH_PRODUCER_PARITY`] is the deletion contract for #825: every
//! legacy-only incident kind has a named production input in this collector.

use std::collections::BTreeSet;

use crate::store::health::HealthLogCursor;
use crate::store::health::IncidentCandidate;
use crate::store::organization::{OrganizationManifest, UnitState};

/// The passive health-monitor cadence (`ORGANIZATION_HEALTH_MONITOR_INTERVAL_MS`).
pub const HEALTH_MONITOR_INTERVAL_MS: i64 = 5 * 60 * 1_000;

/// A runtime reconciliation older than this has blown its recovery lease
/// (`ORGANIZATION_RUNTIME_RECONCILIATION_STALE_MS`).
pub const RUNTIME_RECONCILIATION_STALE_MS: i64 = 60_000;

/// Default staleness for a pending supervision effect
/// (`ORGANIZATION_SUPERVISION_EFFECT_STALE_MS`).
pub const SUPERVISION_EFFECT_STALE_MS: i64 = 15 * 60 * 1_000;

/// Default age after which an otherwise-valid pending envelope is stale.
pub const MAILBOX_STALE_MS: i64 = 5 * 60 * 1_000;

/// Default age after which a work-free idle pane awaiting its release is stuck.
pub const IDLE_TRANSITION_STALE_MS: i64 = 10 * 60 * 1_000;

/// Exhaustive legacy detector inventory consumed by #825's deletion gate.
pub const LEGACY_HEALTH_PRODUCER_PARITY: [&str; 8] = [
    "exception",
    "runtime_log_error",
    "idle_pane_awaiting_release",
    "idle_supervision_error",
    "mailbox_recipient_inactive",
    "mailbox_unit_inactive",
    "mailbox_invalid",
    "mailbox_delivery_stale",
];

/// One newly appended legacy runtime-log diagnostic, already bounded by the host.
#[derive(Debug, Clone)]
pub struct LogIncidentObservation {
    /// `exception` for exceptions.jsonl, otherwise `runtime_log_error`.
    pub kind: String,
    /// File/source label only; never a secret-bearing path.
    pub source: String,
    /// Raw diagnostic; the health fold performs the one redaction pass.
    pub detail: String,
    /// True only when `detail` came from the `error` property of one parsed
    /// exception record. Plaintext lines that happen to resemble the roster
    /// race must still page.
    pub structured_exception: bool,
}

/// One live idle transition whose tagged pane still exists.
///
/// # Why this is not `IdlePaneObservation` any more (#751)
///
/// It carries a person, a transition id, two timestamps and a work-lease
/// flag — and NO pane id. It cannot carry one: this is `chiefd-core`, a backend
/// crate, and the whole P10 boundary is that the backend does not know what a
/// pane is. The old name was the same defect the runtime row's `panes` map had,
/// on a different map: a name stating something its contents cannot hold.
///
/// The *condition* still concerns a pane — whether a work-free person is
/// holding one — but the half of that this type observes is the durable
/// transition, and the runtime half is applied by the gatherer, which filters
/// these against people the actuator actually reported. Naming it for the half
/// it holds is what makes the two halves visible as two.
///
/// **The incident id `idle_pane_awaiting_release` is deliberately unchanged.**
/// That string is durable and operator-facing: it appears in stored health
/// documents and in the unblock text an operator reads. #751-P4 already
/// migrated it once (from `idle_pane_awaiting_reflection`) as its own deliberate
/// change, and renaming an incident id is a data migration, not a refactor.
#[derive(Debug, Clone)]
pub struct IdleTransitionObservation {
    /// Person whose idle transition is still open.
    pub person_id: String,
    /// Exact durable transition id.
    pub transition_id: String,
    /// When the transition opened.
    pub requested_at: String,
    /// Operator who owns the unblock.
    pub responsible_person_id: String,
    /// Whether current durable work still leases this person. A stale idle
    /// transition is actionable only once the person is genuinely work-free.
    pub work_lease_active: bool,
}

/// Health facts about one recipient's ordinary pending mailbox rows.
#[derive(Debug, Clone)]
pub struct MailboxObservation {
    /// Mailbox owner.
    pub person_id: String,
    /// `active`, `benched`, `departed`, or `missing`.
    pub employment_state: String,
    /// Whether the unit and all ancestors are active.
    pub unit_active: bool,
    /// Ordinary pending row count (health-alert rows excluded).
    pub ordinary_count: u64,
    /// Valid, non-alert envelopes used by the stale detector.
    pub valid_count: u64,
    /// Oldest valid ordinary envelope.
    pub oldest_at: Option<String>,
    /// At least one pending row failed the durable-envelope invariant.
    pub invalid: bool,
    /// The narrow CEO-only/quiesce suppression is active for the oldest row.
    pub quiesce_suppressed: bool,
    /// Operator who owns the unblock.
    pub responsible_person_id: String,
}

/// A supervisor liveness sample.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SupervisorObservation {
    /// The `status` field; only `"running"` is healthy.
    pub status: String,
    /// The supervisor's own cadence, doubling into the stale threshold.
    pub interval_ms: i64,
    /// Last heartbeat (ISO-8601), if any.
    pub last_heartbeat_at: Option<String>,
    /// The last recorded supervisor error, raw.
    pub last_error: Option<String>,
}

/// The reconciliation marker off the runtime document.
#[derive(Debug, Clone)]
pub struct RuntimeReconciliationObservation {
    /// Only `"in_progress"` is a live reconciliation.
    pub phase: String,
    /// When it started (ISO-8601).
    pub started_at: Option<String>,
}

/// The launcher-owned runtime projection document, narrowed to what the
/// collection pass reads.
#[derive(Debug, Clone, Default)]
pub struct RuntimeDocObservation {
    /// Document schema version; must be `1` to match.
    pub version: Option<i64>,
    /// The runtime socket the projection names.
    pub socket_name: Option<String>,
    /// Person ids the runtime row published a process handle for.
    pub process_person_ids: Vec<String>,
    /// The in-progress reconciliation marker, if any.
    pub reconciliation: Option<RuntimeReconciliationObservation>,
}

/// The outcome of the host's runtime audit for this pass.
#[derive(Debug, Clone)]
pub enum RuntimeSample {
    /// The audit ran; carries whether the session exists and the people it observed.
    Audited {
        /// Whether a tagged session exists.
        exists: bool,
        /// Person ids the actuator reported alive.
        process_person_ids: Vec<String>,
    },
    /// The audit itself failed — the `runtime_ownership_conflict` branch. Carries the raw
    /// error message (redacted by the fold, not here).
    Failed {
        /// The raw failure message.
        message: String,
    },
    /// No audit was attempted this pass (e.g. no expectation to check).
    NotRun,
}

/// One supervision effect, narrowed to the fields the two effect detectors read.
#[derive(Debug, Clone)]
pub struct SupervisionEffectObservation {
    /// The effect id.
    pub id: String,
    /// The effect `type` (`assignment_result`, `manager_goal_stalled`, …).
    pub kind: String,
    /// The effect status (`"pending"` / `"failed"` are the ones that fault).
    pub status: String,
    /// ISO-8601 creation stamp.
    pub created_at: Option<String>,
    /// How many delivery attempts failed.
    pub delivery_failure_count: Option<i64>,
    /// When it exhausted its attempts.
    pub failed_at: Option<String>,
    /// When its last delivery attempt failed (the `oldest_at` fallback).
    pub last_delivery_failure_at: Option<String>,
    /// Present on manager-owned effects (`"managerPersonId" in effect`).
    pub manager_person_id: Option<String>,
    /// Present on assignee-owned effects.
    pub assignee_person_id: Option<String>,
    /// Present on `manager_goal_stalled` effects.
    pub escalation_person_id: Option<String>,
}

/// The converge cycle's own liveness, read from `converge_safety`.
///
/// Replaces the advisory runtime-reconciliation marker, which was deleted with
/// the runtime-lifecycle port: every producer now writes
/// `runtime.reconciliation = None`, so the incident that read it could never
/// see a real age again.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConvergeCycleObservation {
    /// `cycle_in_progress` — is a converge pass running right now?
    pub in_progress: bool,
    /// `cycle_started_at_ms` — when it began, if it has begun.
    pub started_at_ms: Option<i64>,
}

/// Everything one collection pass needs, already gathered by the host.
#[derive(Debug, Clone)]
pub struct HealthCollectionSnapshot {
    /// `Date.parse(checkedAt)` — the instant this pass runs at.
    pub now_millis: i64,
    /// The runtime socket the company runs on (`options.socketName`).
    pub socket_name: String,
    /// The supervisor state, or `None` when it is absent.
    pub supervisor: Option<SupervisorObservation>,
    /// Optional override for the supervisor stale threshold.
    pub supervisor_stale_ms: Option<i64>,
    /// Optional override for the supervision-effect stale threshold.
    pub supervision_effect_stale_ms: Option<i64>,
    /// The runtime projection document, or `None` when it is absent.
    pub runtime: Option<RuntimeDocObservation>,
    /// Who durable activity says should be running.
    pub expected_active_people: Vec<String>,
    /// The runtime audit outcome.
    pub runtime_audit: RuntimeSample,
    /// People the actuator reported a DEAD process for — it looked, and the
    /// process it expected to find is gone.
    ///
    /// Person ids, not process handles: chiefd has no display id to report and the
    /// actuator's report is keyed by person. Renamed with the field so an
    /// operator is never shown a `%3` this backend could not have produced.
    pub dead_processes: Vec<String>,
    /// Supervision effects, in `effectOrder`.
    pub supervision_effects: Vec<SupervisionEffectObservation>,
    /// Newly appended bounded log observations.
    pub log_incidents: Vec<LogIncidentObservation>,
    /// EOF cursors after this pass's bounded reads.
    pub log_cursors: std::collections::BTreeMap<String, HealthLogCursor>,
    /// Work-free idle panes whose transition has not been released.
    pub idle_transitions: Vec<IdleTransitionObservation>,
    /// Error while deriving idle supervision facts; redacted by the fold.
    ///
    /// A failure to read a person's Pi session state at all is a real host
    /// fault regardless of what the reading was going to be used for, so it
    /// stays a dedicated incident rather than costing the whole pass.
    pub idle_supervision_error: Option<String>,
    /// Per-recipient ordinary pending mailbox observations.
    pub mailboxes: Vec<MailboxObservation>,
    /// Optional test/config override for mailbox staleness.
    pub mailbox_stale_ms: Option<i64>,
    /// Optional test/config override for idle-pane staleness.
    pub idle_transition_stale_ms: Option<i64>,
    /// The converge cycle's liveness, or `None` when it could not be read.
    /// `None` is "no opinion" and raises NO incident — an unreadable cycle is
    /// not evidence of a stuck one.
    pub converge_cycle: Option<ConvergeCycleObservation>,
    /// How long it is since an actuator read this company's desired set
    /// ([`crate::runtime::attendance::ActuatorAttendance`]).
    ///
    /// A DURATION and not the timestamp itself, deliberately. The stamp and
    /// `now_millis` above must come from ONE clock, and the first cut of this
    /// change got that wrong — an HTTP handler reaching for `SystemTime::now()`
    /// against a duty reading the ledger's clock, which under a `ManualClock`
    /// sits decades away and reads as "attended for the next fifty years".
    /// Both are the company's own injected clock now
    /// (`SupervisionLiveSource::clock_now`), and passing only the difference
    /// across this boundary makes the two-clock subtraction unrepresentable
    /// here rather than merely absent.
    ///
    /// Not an `Option`. Nothing is converging a company nobody has asked about,
    /// so there is no reading of this fact that means "no opinion" — the
    /// daemon's boot instant is the seed, and silence from there is silence.
    pub actuator_silent_ms: i64,
}

/// `timestampAge`: the non-negative age of an ISO-8601 stamp, or `None` when it
/// is absent or unparseable.
#[must_use]
fn timestamp_age(at: Option<&str>, now: i64) -> Option<i64> {
    let parsed = crate::isotime::parse_iso_millis(at?)?;
    Some((now - parsed).max(0))
}

/// Whether a unit and every ancestor of it is active.
///
/// Ported from `activeDepartment`: an **unknown** department id is treated as
/// active (the TS `while (department)` never enters), so a person assigned to a
/// department the manifest does not contain is not spuriously "in an inactive
/// unit".
#[must_use]
pub fn active_department(manifest: &OrganizationManifest, department_id: &str) -> bool {
    let mut current = manifest.department(department_id);
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    while let Some(department) = current {
        if visited.contains(department.id.as_str()) || department.state != UnitState::Active {
            return false;
        }
        visited.insert(department.id.as_str());
        current = department
            .parent_department_id
            .as_deref()
            .and_then(|parent| manifest.department(parent));
    }
    true
}

/// Whether `person_id` is an operator whose mailbox is a valid escalation
/// target: employed, in an active unit, and (if they head a unit) heading an
/// active one. Ported from `operationalMailboxPerson`.
#[must_use]
fn operational_mailbox_person(manifest: &OrganizationManifest, person_id: &str) -> bool {
    let Some(person) = manifest.person(person_id) else { return false };
    if person.employment_state != crate::store::organization::EmploymentState::Active
        || !active_department(manifest, &person.department_id)
    {
        return false;
    }
    match manifest.headed_department(person_id) {
        Some(headed) => active_department(manifest, &headed.id),
        None => true,
    }
}

/// The person `person_id`'s mailbox manager: for a head, the head of its unit's
/// parent; otherwise the head of the unit they are assigned to (never
/// themselves). Ported from `directMailboxManager`.
#[must_use]
fn direct_mailbox_manager(manifest: &OrganizationManifest, person_id: &str) -> Option<String> {
    manifest.person(person_id)?;
    if let Some(headed) = manifest.headed_department(person_id) {
        let parent = headed.parent_department_id.as_deref()?;
        return manifest.department(parent).map(|unit| unit.head_person_id.clone());
    }
    let person = manifest.person(person_id)?;
    let manager = manifest.department(&person.department_id).map(|unit| &unit.head_person_id)?;
    if manager.as_str() != person_id {
        Some(manager.clone())
    } else {
        None
    }
}

/// The operator whose mailbox is responsible for `person_id` — the first
/// operational manager walking up the mailbox chain, else the CEO. Ported from
/// `responsibleMailboxOperator`; a broken or cyclic chain resolves to the CEO.
#[must_use]
pub fn responsible_mailbox_operator(manifest: &OrganizationManifest, person_id: &str) -> String {
    let ceo = manifest.chief_person_id().unwrap_or("").to_string();
    if manifest.person(person_id).is_none() || person_id == ceo.as_str() {
        return ceo;
    }
    let mut current = person_id.to_string();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    visited.insert(current.clone());
    while current != ceo {
        let Some(manager) = direct_mailbox_manager(manifest, &current) else { return ceo };
        if visited.contains(&manager) {
            return ceo;
        }
        if manager == ceo || operational_mailbox_person(manifest, &manager) {
            return manager;
        }
        visited.insert(manager.clone());
        current = manager;
    }
    ceo
}

/// The mailbox person an effect's alert would be routed to — the person whose
/// impaired mailbox needs an out-of-band copy. Ported from
/// `supervisionEffectMailbox`.
#[must_use]
fn supervision_effect_mailbox(effect: &SupervisionEffectObservation) -> Option<String> {
    match effect.kind.as_str() {
        "manager_goal_stalled" => effect.escalation_person_id.clone(),
        "assignment_result" | "assignment_failure" | "manager_goal_watch" => {
            effect.manager_person_id.clone()
        }
        _ => effect.assignee_person_id.clone(),
    }
}

/// The operator who owns an effect's unblock: the manager directly, else the
/// operator responsible for the assignee's mailbox.
#[must_use]
fn supervision_effect_owner(
    manifest: &OrganizationManifest,
    effect: &SupervisionEffectObservation,
) -> String {
    if let Some(manager) = &effect.manager_person_id {
        return manager.clone();
    }
    responsible_mailbox_operator(manifest, effect.assignee_person_id.as_deref().unwrap_or(""))
}

/// Run the collection pass: fold the gathered snapshot into incident candidates.
///
/// The returned candidates are fed unchanged to
/// [`apply_cycle`](crate::store::health::apply_cycle). Push order follows the
/// TypeScript; the fold is order-independent for distinct fingerprints, so the
/// order is preserved for readability rather than correctness.
#[must_use]
pub fn collect(
    manifest: &OrganizationManifest,
    snapshot: &HealthCollectionSnapshot,
) -> Vec<IncidentCandidate> {
    let now = snapshot.now_millis;
    let mut incidents: Vec<IncidentCandidate> = Vec::new();

    // --- supervisor state ---------------------------------------------------
    match &snapshot.supervisor {
        None => incidents.push(IncidentCandidate::new(
            "supervisor_not_running",
            "supervisor state is absent or stopped",
        )),
        Some(supervisor) if supervisor.status != "running" => {
            incidents.push(IncidentCandidate::new(
                "supervisor_not_running",
                "supervisor state is absent or stopped",
            ))
        }
        Some(supervisor) => {
            let stale_ms = snapshot.supervisor_stale_ms.unwrap_or_else(|| {
                (supervisor.interval_ms.saturating_mul(2)).max(HEALTH_MONITOR_INTERVAL_MS * 3)
            });
            let age = timestamp_age(supervisor.last_heartbeat_at.as_deref(), now);
            if age.is_none_or(|age| age > stale_ms) {
                let detail = match age {
                    None => "heartbeat is invalid".to_string(),
                    Some(age) => format!("heartbeat is {}m old", age / 60_000),
                };
                incidents.push(IncidentCandidate::new("supervisor_stale", detail));
            }
            if let Some(error) = &supervisor.last_error {
                incidents.push(IncidentCandidate::new("supervisor_error", error.clone()));
            }
        }
    }

    // --- is anybody converging this company at all? -------------------------
    //
    // THE INCIDENT THE 22:17:40 OUTAGE HAD NO NAME FOR. The tmux server went
    // away with every pane and every person in it, and chiefd reported a
    // healthy pass for forty minutes because it holds no fact about the
    // runtime — by construction, since #751/P8-P10 deleted the actuator's
    // reports. It does hold this one: the actuator reads the desired set on
    // every round of its own loop, so silence longer than three of that loop's
    // ceilings is nobody reading it.
    //
    // The detail deliberately does NOT interpolate how long the silence has
    // run. A moving detail mints a fresh fingerprint every pass, dedup never
    // matches a prior one, and the alert repeats for ever — the failure
    // `IncidentCandidate::identity` exists to describe. The duration belongs in
    // the daemon's log line, which is a report rather than an identity.
    if snapshot.actuator_silent_ms > crate::runtime::attendance::ACTUATOR_LAPSE_MS {
        incidents.push(IncidentCandidate::new(
            "runtime_unattended",
            "no actuator has read this company's desired set; nobody is converging it and \
             nothing chiefd wants running is being made to run",
        ));
    }

    // --- converge cycle liveness -------------------------------------------
    //
    // Read from `converge_safety` (`cycle_in_progress` / `cycle_started_at_ms`),
    // NOT from the advisory `runtime.reconciliation` marker this used to use.
    // That marker was deleted with the runtime-lifecycle port, so every
    // producer now writes `None` — and `runtime_matches` returns false the
    // moment the marker is absent, which made this incident UNREACHABLE. A
    // company whose converge cycle really was wedged raised nothing at all,
    // and the tests passed by constructing a `reconciliation: Some(..)` shape
    // production can no longer produce. A health signal that cannot fire is
    // indistinguishable from a healthy fleet.
    //
    // The direction that matters: an ABSENT observation raises no incident. A
    // fact nobody could read is not evidence of a fault, and turning it into
    // one is how a permanent false alarm gets built.
    let cycle = snapshot.converge_cycle.as_ref();
    let cycle_age = cycle
        .filter(|cycle| cycle.in_progress)
        .and_then(|cycle| cycle.started_at_ms)
        .map(|started| (now - started).max(0));
    let reconciliation_in_progress =
        cycle_age.is_some_and(|age| age < RUNTIME_RECONCILIATION_STALE_MS);
    if cycle_age.is_some_and(|age| age >= RUNTIME_RECONCILIATION_STALE_MS) {
        incidents.push(IncidentCandidate::new(
            "runtime_reconciliation_stalled",
            "converge cycle remained in progress beyond its one-minute recovery lease",
        ));
    }

    let mut expected = snapshot.expected_active_people.clone();
    expected.sort();
    let mut runtime_people = snapshot
        .runtime
        .as_ref()
        .map(|runtime| runtime.process_person_ids.clone())
        .unwrap_or_default();
    runtime_people.sort();
    if !reconciliation_in_progress && runtime_people != expected {
        incidents.push(IncidentCandidate::new(
            "runtime_activity_mismatch",
            format!(
                "runtime processes [{}] differ from desired activity [{}]",
                runtime_people.join(","),
                expected.join(","),
            ),
        ));
    }

    // --- runtime audit ---------------------------------------------------------
    match &snapshot.runtime_audit {
        RuntimeSample::Failed { message } => {
            incidents.push(IncidentCandidate::new("runtime_ownership_conflict", message.clone()));
        }
        RuntimeSample::NotRun => {}
        RuntimeSample::Audited { exists, process_person_ids } => {
            let mut observed = process_person_ids.clone();
            observed.sort();
            if !reconciliation_in_progress && *exists && observed != runtime_people {
                incidents.push(IncidentCandidate::new(
                    "runtime_projection_mismatch",
                    format!(
                        "observed processes [{}] differ from runtime [{}]",
                        observed.join(","),
                        runtime_people.join(","),
                    ),
                ));
            }
            if !reconciliation_in_progress && !*exists && !expected.is_empty() {
                incidents.push(IncidentCandidate::new(
                    "runtime_session_missing",
                    "expected active people have no tagged runtime session",
                ));
            }
            if *exists && !snapshot.dead_processes.is_empty() {
                let mut dead = snapshot.dead_processes.clone();
                dead.sort();
                incidents.push(IncidentCandidate::new(
                    "runtime_dead_processes",
                    format!("dead runtime processes [{}]", dead.join(",")),
                ));
            }
        }
    }

    // --- bounded runtime logs ---------------------------------------------
    for observation in &snapshot.log_incidents {
        incidents.push(IncidentCandidate::new(
            observation.kind.clone(),
            format!("{}: {}", observation.source, observation.detail),
        ));
    }

    // --- idle pane awaiting release ----------------------------------------
    //
    // The condition this detects is unchanged by #751-P4: a work-free person
    // still owns a tagged pane because their idle-park transition has sat
    // unfinished past `IDLE_TRANSITION_STALE_MS`. Only the NAME was reflection-shaped
    // (`idle_pane_awaiting_reflection`), because finishing the transition used
    // to mean recording a reflection and now means releasing it. `Forced`
    // rescues only plain non-intent-bound automatic parks, so an intent-bound
    // park or a stalled offboard can still wedge a pane indefinitely with
    // nothing else watching — this is that alarm.
    if let Some(error) = &snapshot.idle_supervision_error {
        incidents.push(IncidentCandidate::new("idle_supervision_error", error.clone()));
    }
    let idle_stale_ms = snapshot.idle_transition_stale_ms.unwrap_or(IDLE_TRANSITION_STALE_MS);
    for idle in &snapshot.idle_transitions {
        if idle.work_lease_active {
            continue;
        }
        let Some(age) = timestamp_age(Some(&idle.requested_at), now) else { continue };
        if age < idle_stale_ms {
            continue;
        }
        let mut candidate = IncidentCandidate::new(
            "idle_pane_awaiting_release",
            format!(
                "Work-free person '{}' still owns a tagged pane because idle transition '{}' has not been released beyond {}m",
                idle.person_id,
                idle.transition_id,
                idle_stale_ms / 60_000,
            ),
        );
        candidate.responsible_person_id = Some(idle.responsible_person_id.clone());
        candidate.observed_count = Some(1);
        candidate.oldest_at = Some(idle.requested_at.clone());
        candidate.unblock_action = Some(format!(
            "Idle transition '{}' for @{} has not been released. Normal reconciliation parks the pane once it is; do not kill the pane or open another transition.",
            idle.transition_id, idle.person_id,
        ));
        incidents.push(candidate);
    }

    // --- pending mailboxes -------------------------------------------------
    let mailbox_stale_ms = snapshot.mailbox_stale_ms.unwrap_or(MAILBOX_STALE_MS);
    for mailbox in &snapshot.mailboxes {
        if mailbox.ordinary_count == 0 {
            continue;
        }
        let common = |kind: &str, detail: String, action: &str| {
            let mut candidate = IncidentCandidate::new(kind, detail);
            candidate.responsible_person_id = Some(mailbox.responsible_person_id.clone());
            candidate.impaired_mailbox_person_id = Some(mailbox.person_id.clone());
            candidate.observed_count = Some(mailbox.ordinary_count);
            candidate.unblock_action = Some(action.to_string());
            candidate
        };
        if matches!(mailbox.employment_state.as_str(), "departed" | "missing") {
            incidents.push(common(
                "mailbox_recipient_inactive",
                format!("Pending mailbox delivery targets unavailable person '{}'", mailbox.person_id),
                "Reroute the durable work to an employed owner; do not resend the existing envelope.",
            ));
            continue;
        }
        if mailbox.employment_state != "active" {
            incidents.push(common(
                "mailbox_recipient_inactive",
                format!("Pending mailbox delivery targets benched person '{}'", mailbox.person_id),
                "Recall the person or reroute ownership; do not resend the existing envelope.",
            ));
            continue;
        }
        if !mailbox.unit_active {
            incidents.push(common(
                "mailbox_unit_inactive",
                format!(
                    "Pending mailbox delivery targets person '{}' in an inactive unit",
                    mailbox.person_id
                ),
                "Resume the unit or reroute ownership; do not resend the existing envelope.",
            ));
            continue;
        }
        if mailbox.invalid {
            incidents.push(common(
                "mailbox_invalid",
                format!("Pending mailbox for '{}' contains an invalid durable envelope", mailbox.person_id),
                "Inspect the quarantined envelope metadata and repair its producer; never copy message content into logs.",
            ));
        }
        let Some(oldest_at) = mailbox.oldest_at.as_deref() else { continue };
        if !mailbox.quiesce_suppressed
            && timestamp_age(Some(oldest_at), now).is_some_and(|age| age >= mailbox_stale_ms)
        {
            let mut candidate = common(
                "mailbox_delivery_stale",
                format!(
                    "Mailbox delivery to '{}' has remained unaccepted beyond {}m",
                    mailbox.person_id,
                    mailbox_stale_ms / 60_000,
                ),
                "Check the recipient in org_roster and unblock its runtime/provider; ChiefD will retry the existing durable envelope, so do not resend it.",
            );
            candidate.observed_count = Some(mailbox.valid_count);
            candidate.oldest_at = Some(oldest_at.to_string());
            incidents.push(candidate);
        }
    }

    // --- supervision effects: stalled (pending) -----------------------------
    let effect_stale_ms =
        snapshot.supervision_effect_stale_ms.unwrap_or(SUPERVISION_EFFECT_STALE_MS);
    for effect in &snapshot.supervision_effects {
        if effect.status != "pending" {
            continue;
        }
        let Some(age) = timestamp_age(effect.created_at.as_deref(), now) else { continue };
        if age < effect_stale_ms {
            continue;
        }
        let mut candidate = IncidentCandidate::new(
            "supervision_delivery_stalled",
            format!(
                "Supervision effect '{}' ({}) has been pending beyond {}m without being dispatched",
                effect.id,
                effect.kind,
                effect_stale_ms / 60_000,
            ),
        );
        candidate.responsible_person_id = Some(supervision_effect_owner(manifest, effect));
        candidate.impaired_mailbox_person_id = supervision_effect_mailbox(effect);
        candidate.observed_count =
            Some(u64::try_from(effect.delivery_failure_count.unwrap_or(0)).unwrap_or(0));
        candidate.oldest_at = effect.created_at.clone();
        candidate.unblock_action = Some(
            "Check whether an undelivered replacement fence for the recipient's generation is \
             withholding this effect, then resume the company to re-drive it; do not manually \
             duplicate its durable envelope."
                .to_string(),
        );
        incidents.push(candidate);
    }

    // --- supervision effects: failed ----------------------------------------
    for effect in &snapshot.supervision_effects {
        if effect.status != "failed" {
            continue;
        }
        let mut candidate = IncidentCandidate::new(
            "supervision_delivery_failed",
            format!(
                "Supervision effect '{}' ({}) exhausted its bounded delivery attempts",
                effect.id, effect.kind,
            ),
        );
        candidate.responsible_person_id = Some(supervision_effect_owner(manifest, effect));
        candidate.impaired_mailbox_person_id = supervision_effect_mailbox(effect);
        candidate.observed_count =
            effect.delivery_failure_count.map(|count| u64::try_from(count).unwrap_or(0));
        candidate.oldest_at =
            effect.failed_at.clone().or_else(|| effect.last_delivery_failure_at.clone());
        candidate.unblock_action = Some(
            "Inspect the effect and recipient runtime metadata, repair the transport or ownership \
             fault, then resume through the normal manager workflow; do not manually duplicate its \
             durable envelope."
                .to_string(),
        );
        incidents.push(candidate);
    }

    incidents
}

// The `reconciliationMatchesRuntime` predicate (`runtime_matches`) lived here
// until #984. Its only caller was the converge-liveness incident above, which
// was rewritten to read `converge_safety` after the runtime-lifecycle port
// deleted the advisory `runtime.reconciliation` marker the predicate keyed
// on; that left the function with no callers at all. Deleted rather than
// `#[allow(dead_code)]`-ed: an unreachable predicate for a marker no producer
// writes is not a helper waiting for a caller, it is the residue of a path
// that is already gone (forward-only, AGENTS.md).

#[cfg(test)]
mod tests;
