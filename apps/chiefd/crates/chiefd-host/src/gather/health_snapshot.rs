//! Duty #9 — the host gatherer for the health-monitor collection pass.
//!
//! Assembles a real [`HealthCollectionSnapshot`] for
//! [`collect`](chiefd_core::store::health_collect::collect). The snapshot is
//! assembled from durable facts plus bounded local observations: the runtime
//! ownership/dead-pane reads, append-only runtime-log deltas, and a readability
//! probe over each person's Pi session directory. Native ledger mailbox and
//! supervision facts complete those host-derived inputs before the pure
//! collector folds them.
//!
//! # The idle-pane work lease
//!
//! This gatherer derives a per-person *work lease* — open manager goals, open
//! assignments, pending mailbox recipients, expanded up the operational mailbox
//! chain — so the pure collector can decide whether a work-free person still
//! holding a tagged pane with an unfinished idle-park transition should page as
//! `idle_pane_awaiting_release`. (#751-P4 renamed that incident from
//! `idle_pane_awaiting_reflection`: finishing the transition used to mean
//! recording a reflection and now means releasing it. The *condition* is
//! unchanged, and it is the only alarm covering an intent-bound park or a
//! stalled offboard wedging a pane — `Forced` rescues only plain
//! non-intent-bound automatic parks.)
//!
//! The same pass also probes a narrow fault it reports separately: a person's
//! `sessions` directory that cannot be listed or stat'd at all. That is a real host fault independent of any transition, and it becomes
//! `idle_supervision_error` rather than costing the whole health pass.
//!
//! # The runtime sample's three outcomes, deliberately mapped (#751/P8-P10)
//!
//! The monitor no longer looks at a display; it READS the observation the
//! attached operator client committed through `POST /v1/org/runtime/observed`,
//! and maps that record's three states onto [`RuntimeSample`].
//!
//! # Presence outranks trust, and the signature is what enforces it
//!
//! The mapping below is reached only for a report whose lease still holds. A
//! report is evidence about the moment it was made, and [`presence`] is the one
//! function that says whether that moment is still current; consulting the
//! observation without it reads a month-old report as news. Both directions of
//! that mistake are live faults, not hypotheticals: a stale `Trusted` report
//! asserts people alive on evidence nobody has refreshed, and a stale report
//! whose people all read dead raises `runtime_session_missing` — accusing a
//! company of losing a runtime when the truth is that nobody has looked.
//!
//! So a lapsed lease maps to [`RuntimeSample::NotRun`], exactly as no record at
//! all does, and for the same sentence: nobody is actuating this company, so
//! nobody looked. That is also why this gatherer takes the whole
//! [`RuntimeActuationRecord`] rather than a caller-extracted
//! [`Observation`] — the timestamp cannot be dropped on the way in, which makes
//! the defect unrepresentable instead of merely fixed. `RuntimeDriftScan::of`
//! derives its own coverage from the record for the same reason.
//!
//! * **TOMBSTONE.** These bullets mapped the actuation record's three states
//!   onto [`RuntimeSample`]. chiefd holds no observation, so there is no
//!   mapping left: the sample is always [`RuntimeSample::NotRun`], which
//!   asserts nothing about what is alive. It is deliberately NOT
//!   `Audited { exists: false }` -- "I have no sample" must never render as "I
//!   sampled and found nothing", which is the unreadable-becomes-empty
//!   conflation this change exists to remove.
//! * **no record at all, or a lapsed lease →** [`RuntimeSample::NotRun`], which
//!   raises nothing.
//!   Nobody is actuating this company, so nobody looked — that is literally
//!   "no audit was attempted this pass", and it is emphatically NOT
//!   [`RuntimeSample::Failed`]: that variant raises `runtime_ownership_conflict`,
//!   an accusation that another daemon holds the session, and an unattended
//!   company is not a contested one. The un-actuated state is not silence
//!   either — the converge pass names it on every report as
//!   `WithheldReason::NoActuator`, from the same record.
//!
//! There is no separate dead-process read left to fail on its own: the alive
//! and dead halves arrive in ONE record, so "observed empty" and "observation
//! failed" cannot drift apart between two calls the way two host reads could.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chiefd_core::runtime::attendance::ActuatorAttendance;
use chiefd_core::runtime::duty_hooks::{BoxFuture, DutyContext, DutyError, HealthSnapshotGatherer};
use chiefd_core::store::health::{self, HealthLogCursor};
use chiefd_core::store::health_collect::{
    HealthCollectionSnapshot, IdleTransitionObservation, LogIncidentObservation,
    MailboxObservation, RuntimeDocObservation, RuntimeSample, SupervisionEffectObservation,
    SupervisorObservation,
};
use chiefd_core::store::supervision::EffectStatus;
use chiefd_core::store::{activity, launch_intent, organization, supervision};

use crate::executor::HostErr;
use crate::gather::reconciler_facts::ReconcilerFactsStore;

/// The durable and host-derived facts one pure collection pass needs, plus the
/// identifiers its runtime reads are scoped to. The production gatherer derives
/// bounded log, mailbox, and activity inputs before constructing this
/// context; callers of [`gather_health_snapshot`] can provide those facts
/// directly through the same explicit seam.
#[derive(Debug, Clone)]
pub struct HealthGatherContext<'a> {
    /// `Date.parse(checkedAt)` — the instant this pass runs at.
    pub now_millis: i64,
    /// The converge cycle's liveness from `converge_safety`; `None` when it
    /// could not be read, which raises no incident.
    pub converge_cycle: Option<chiefd_core::store::health_collect::ConvergeCycleObservation>,
    /// The runtime server socket NAME; also the snapshot's `socket_name`.
    pub socket_name: &'a str,
    /// The supervisor liveness sample, derived from the `SupervisionReconcile`
    /// duty watermark.
    pub supervisor: Option<SupervisorObservation>,
    /// Optional override for the supervisor stale threshold.
    pub supervisor_stale_ms: Option<i64>,
    /// Optional override for the supervision-effect stale threshold.
    pub supervision_effect_stale_ms: Option<i64>,
    /// The runtime projection document, read from the runtime doc.
    pub runtime: Option<RuntimeDocObservation>,
    /// The activity ledger's desired-active set.
    pub expected_active_people: Vec<String>,
    /// Supervision effects, in `effectOrder`, read from the supervision ledger.
    pub supervision_effects: Vec<SupervisionEffectObservation>,
    /// Newly appended bounded runtime-log observations.
    pub log_incidents: Vec<LogIncidentObservation>,
    /// EOF cursors after the bounded reads.
    pub log_cursors: BTreeMap<String, HealthLogCursor>,
    /// Idle panes derived from the native activity/supervision ledgers.
    pub idle_transitions: Vec<IdleTransitionObservation>,
    /// Failure to read a person's Pi session state at all.
    pub idle_supervision_error: Option<String>,
    /// Native pending mailbox observations.
    pub mailboxes: Vec<MailboxObservation>,
    /// How long it is since an actuator read this company's desired set
    /// ([`chiefd_core::runtime::attendance::ActuatorAttendance`]) — whether
    /// anybody is converging this company at all.
    pub actuator_silent_ms: i64,
}

/// Gather a real [`HealthCollectionSnapshot`] by auditing runtime and scanning for
/// dead processes.
///
/// # Errors
/// [`HostErr::Untrusted`] / [`HostErr::ToolUnavailable`] from either runtime read
/// propagates: the gather fails closed rather than reporting an empty or
/// all-clear observation it could not actually make. An authoritative
/// [`HostErr::ToolFailed`] audit does *not* error — it becomes a
/// [`RuntimeSample::Failed`] the collector turns into an ownership incident.
pub fn gather_health_snapshot(
    ctx: HealthGatherContext<'_>,
) -> Result<HealthCollectionSnapshot, HostErr> {
    let (runtime_audit, dead_processes) = runtime_sample(ctx.now_millis)?;
    let idle_detector_ran = matches!(&runtime_audit, RuntimeSample::Audited { exists: true, .. });
    let observed_people: std::collections::BTreeSet<&str> = match &runtime_audit {
        RuntimeSample::Audited { process_person_ids, .. } => {
            process_person_ids.iter().map(String::as_str).collect()
        }
        RuntimeSample::Failed { .. } | RuntimeSample::NotRun => std::collections::BTreeSet::new(),
    };
    let idle_transitions = ctx
        .idle_transitions
        .into_iter()
        .filter(|idle| observed_people.contains(idle.person_id.as_str()))
        .collect();

    Ok(HealthCollectionSnapshot {
        now_millis: ctx.now_millis,
        socket_name: ctx.socket_name.to_owned(),
        supervisor: ctx.supervisor,
        supervisor_stale_ms: ctx.supervisor_stale_ms,
        supervision_effect_stale_ms: ctx.supervision_effect_stale_ms,
        runtime: ctx.runtime,
        expected_active_people: ctx.expected_active_people,
        runtime_audit,
        dead_processes,
        supervision_effects: ctx.supervision_effects,
        log_incidents: ctx.log_incidents,
        log_cursors: ctx.log_cursors,
        idle_transitions,
        // Idle work leases are derived only after an existing runtime audit, and
        // that gate is kept deliberately: a company with no tagged session has
        // no running people whose Pi home could matter, so an unreadable
        // session directory there is a dormant filesystem fact, not an
        // actionable fault. Reporting it anyway would page every stopped
        // company forever.
        idle_supervision_error: idle_detector_ran.then_some(ctx.idle_supervision_error).flatten(),
        mailboxes: ctx.mailboxes,
        mailbox_stale_ms: None,
        idle_transition_stale_ms: None,
        converge_cycle: ctx.converge_cycle,
        actuator_silent_ms: ctx.actuator_silent_ms,
    })
}

/// chiefd's runtime sample: it does not have one, and says so.
///
/// TOMBSTONE: this used to read the actuator's committed report and map its
/// three states onto [`RuntimeSample`] -- `Trusted` to `Audited` with the live
/// person ids, `Untrusted` to `HostErr::Untrusted`, and a lapsed lease or no
/// record to `NotRun`. Every one of those is a host fact, so the whole sample
/// goes.
///
/// [`RuntimeSample::NotRun`] is the honest answer and the correct fail-safe
/// one. It is what a lapsed lease already mapped to, it asserts nothing about
/// what is alive, and -- crucially -- it is NOT the empty-`process_person_ids`
/// path, which raises `runtime_session_missing`. "I have no sample" must never
/// render as "I sampled and found nothing"; that is the same unreadable-becomes-
/// empty conflation this whole change exists to remove, and it would have been
/// very easy to reintroduce here by returning `Audited { exists: false }`.
///
/// NAMED, ACCEPTED LOSS: `chief ls` no longer shows liveness. What is actually
/// running is the actuator's screen to own.
fn runtime_sample(_now_ms: i64) -> Result<(RuntimeSample, Vec<String>), HostErr> {
    Ok((RuntimeSample::NotRun, Vec::new()))
}

/// The lowercase wire string `EffectStatus` serializes to
/// (`#[serde(rename_all = "lowercase")]`), spelled out rather than routed
/// through `serde_json` for one enum with four variants.
const fn effect_status_str(status: EffectStatus) -> &'static str {
    match status {
        EffectStatus::Pending => "pending",
        EffectStatus::Delivered => "delivered",
        EffectStatus::Superseded => "superseded",
        EffectStatus::Failed => "failed",
    }
}

const MAX_HEALTH_LOG_FILES: usize = 16;
/// Legacy `MAX_MAILBOX_HEALTH_FILES_PER_PERSON`: cap validation/age work per
/// recipient without changing that mailbox's total pending observation count.
const MAX_MAILBOX_HEALTH_ROWS_PER_PERSON: usize = 64;

/// Matches the legacy monitor's `/\.(?:log|jsonl)$/i` generic-log admission.
///
/// Name-specific exclusions above this check intentionally remain exact, just
/// as `isOrgLogStreamFile` did in the retired TypeScript monitor.
fn has_monitored_log_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("log") || extension.eq_ignore_ascii_case("jsonl")
    })
}

fn monitored_log_paths(logs: &Path) -> Vec<PathBuf> {
    // `logs/supervisor.log` was seeded here alongside `exceptions.jsonl` and is
    // no longer: nothing in the product has written it since the detached
    // `org-supervisor` process was deleted, so the seed could only ever cost an
    // `openat` that returns NotFound and is skipped. It is not repointed at the
    // daemon log (`~/.chiefd/run/<slug>.log`) either — that file is diagnostics
    // and explicitly never authority, it is `tracing` output whose every
    // error-level line trips `generic_error_line` unconditionally, and it
    // carries the health monitor's OWN "health gather failed" and "health
    // monitor commit refused" warnings, so scanning it would make this duty
    // manufacture incidents out of its own reporting forever. The seed is also
    // redundant: the directory scan below admits any `.log` the excluded and
    // structured lists do not name, so a `supervisor.log` that ever did appear
    // would still be picked up.
    let mut paths = vec![logs.join("exceptions.jsonl")];
    if let Ok(entries) = std::fs::read_dir(logs) {
        let mut names: Vec<String> =
            entries.flatten().filter_map(|entry| entry.file_name().into_string().ok()).collect();
        names.sort();
        for name in names {
            let structured = [
                "supervisor.jsonl",
                "runtime.jsonl",
                "dispatch.jsonl",
                "mailbox.jsonl",
                "admission.jsonl",
                "health.jsonl",
                "chiefd.jsonl",
                "write-db.jsonl",
            ]
            .contains(&name.as_str());
            if name == "health-monitor.jsonl"
                || name == "operator-escalations.jsonl"
                || structured
                || !has_monitored_log_extension(&name)
            {
                continue;
            }
            paths.push(logs.join(name));
        }
    }
    paths.sort();
    paths.dedup();
    paths.truncate(MAX_HEALTH_LOG_FILES);
    paths
}

fn generic_error_line(line: &str) -> bool {
    // Exact port of `/\\b(error|exception|failed|failure|retrying|timeout|refus)/i`.
    // The legacy regexp accepts suffixes (`errors`, `failed_once`) after a
    // word boundary; splitting into whole words accidentally lost those lines.
    let lowercase = line.to_ascii_lowercase();
    ["error", "exception", "failed", "failure", "retrying", "timeout", "refus"].iter().any(
        |needle| {
            lowercase.match_indices(needle).any(|(offset, _)| {
                offset == 0
                    || !matches!(
                        lowercase.as_bytes()[offset - 1],
                        b'a'..=b'z' | b'0'..=b'9' | b'_'
                    )
            })
        },
    )
}

fn roster_transition_race(detail: &str) -> Option<(&str, &str)> {
    let rest = detail.strip_prefix("Runtime observation parked '")?;
    let (person_id, rest) = rest.split_once("' before active transition '")?;
    let transition_id = rest.strip_suffix("' completed")?;
    (!person_id.is_empty()
        && !transition_id.is_empty()
        && !person_id.contains('\'')
        && !transition_id.contains('\''))
    .then_some((person_id, transition_id))
}

fn is_resolved_roster_transition_race(
    incident: &LogIncidentObservation,
    is_terminal: impl FnOnce(&str, &str) -> bool,
) -> bool {
    incident.kind == "exception"
        && incident.structured_exception
        && roster_transition_race(&incident.detail)
            .is_some_and(|(person_id, transition_id)| is_terminal(person_id, transition_id))
}

fn collect_log_observations(
    logs: &Path,
    previous: &BTreeMap<String, HealthLogCursor>,
    baseline: bool,
) -> Result<(Vec<LogIncidentObservation>, BTreeMap<String, HealthLogCursor>), String> {
    let paths = monitored_log_paths(logs);
    let limit = health::per_log_read_limit(paths.len());
    let mut observations = Vec::new();
    let mut cursors = BTreeMap::new();
    for path in paths {
        let mut file = match crate::files::ObservedFile::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("health log open: {error}")),
        };
        let metadata = file.metadata();
        let key = path.to_string_lossy().to_string();
        let plan = health::plan_bounded_read(
            previous.get(&key),
            &metadata.device,
            &metadata.inode,
            metadata.size,
            limit,
        );
        cursors.insert(key, plan.cursor.clone());
        if baseline || plan.bytes == 0 {
            continue;
        }
        let bytes =
            usize::try_from(plan.bytes).map_err(|_| "health log read too large".to_string())?;
        let buffer = file
            .read_range(plan.read_start, bytes)
            .map_err(|error| format!("health log read: {error}"))?;
        let text = String::from_utf8_lossy(&buffer);
        let lines = health::bounded_read_lines(&text, &plan);
        let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("runtime.log");
        for line in lines {
            if file_name == "exceptions.jsonl" {
                let parsed = serde_json::from_str::<serde_json::Value>(&line).ok();
                let source = parsed
                    .as_ref()
                    .and_then(|value| value.get("source"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("exception-log");
                let parsed_error = parsed
                    .as_ref()
                    .and_then(|value| value.get("error"))
                    .and_then(serde_json::Value::as_str);
                let detail = parsed_error.unwrap_or(&line);
                if !detail.is_empty() {
                    observations.push(LogIncidentObservation {
                        kind: "exception".to_string(),
                        source: source.to_string(),
                        detail: detail.to_string(),
                        structured_exception: parsed_error.is_some(),
                    });
                }
            } else if generic_error_line(&line) {
                observations.push(LogIncidentObservation {
                    kind: "runtime_log_error".to_string(),
                    source: file_name.to_string(),
                    detail: line,
                    structured_exception: false,
                });
            }
        }
    }
    Ok((observations, cursors))
}

/// Probe every person's `sessions/` directory for readability.
///
/// The path is [`crate::agent_home::agent_home`]'s, never composed here: this
/// reader joined `people/<id>/pi-home/sessions` itself and would have gone on
/// finding nothing for every person after the home moved, silently, because
/// of the very `NotFound` arm below.
///
/// # A MISSING sessions directory is now an ERROR, and that is a change
///
/// It used to `continue` — "ordinary, most people have never run" — which was
/// true while `sessions/` was one directory of a five-directory projection
/// that a person might legitimately be waiting on. It is not true now:
/// `ensure_agent_home` creates the home and `sessions/` inside it in the same
/// call, at hire, so the directory is absent exactly when the home was never
/// built. That is the state the launch gate refuses a person for, and idle
/// supervision reporting a healthy company over it is the same silent-absence
/// shape this whole reader nearly shipped.
///
/// So absence becomes `idle_supervision_error`, like an unreadable directory:
/// a fact about the host that costs this observation and not the health pass.
/// A directory that cannot be listed, or whose session records cannot be
/// stat'd, is unchanged.
fn pi_session_directories_readable(
    dir: &Path,
    manifest: &chiefd_core::store::organization::OrganizationManifest,
) -> Result<(), String> {
    for person_id in &manifest.people_order {
        let sessions = crate::agent_home::agent_home(dir, person_id).join("sessions");
        let entries = match std::fs::read_dir(&sessions) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "idle supervision found no sessions directory for '{person_id}' at {}; \
                     their agent home was never written, so nothing can be observed about them",
                    sessions.display()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "idle supervision cannot list {}: {error}",
                    sessions.display()
                ));
            }
        };
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!("idle supervision reads {}: {error}", sessions.display())
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.ends_with(".jsonl") {
                continue;
            }
            entry.metadata().and_then(|metadata| metadata.modified()).map_err(|error| {
                format!("idle supervision stats {}: {error}", entry.path().display())
            })?;
        }
    }
    Ok(())
}

/// Expand direct work demand to the exact operational mailbox management
/// chain that the legacy idle detector treated as leased.
fn idle_work_leases(
    manifest: &chiefd_core::store::organization::OrganizationManifest,
    direct: impl IntoIterator<Item = String>,
) -> BTreeSet<String> {
    let mut leases: BTreeSet<String> = direct.into_iter().collect();
    let ceo = manifest.chief_person_id().unwrap_or("").to_string();
    leases.insert(ceo.clone());
    let direct_leases: Vec<String> = leases.iter().cloned().collect();
    for person_id in direct_leases {
        let mut current = person_id;
        let mut visited = BTreeSet::new();
        while visited.insert(current.clone()) {
            let manager = chiefd_core::store::health_collect::responsible_mailbox_operator(
                manifest, &current,
            );
            if manager.is_empty() || manager == current || leases.contains(&manager) {
                break;
            }
            leases.insert(manager.clone());
            current = manager;
        }
    }
    leases
}

fn idle_pane_observations(
    dir: &Path,
    manifest: &chiefd_core::store::organization::OrganizationManifest,
    activity_ledger: &chiefd_core::store::activity::ActivityLedger,
    pending_mailbox_people: &BTreeSet<String>,
) -> Result<Vec<IdleTransitionObservation>, String> {
    pi_session_directories_readable(dir, manifest)?;
    let mut direct_leases: BTreeSet<String> = BTreeSet::new();
    direct_leases.extend(pending_mailbox_people.iter().cloned());
    let work_leases = idle_work_leases(manifest, direct_leases);
    Ok(activity_ledger
        .person_order
        .iter()
        .filter_map(|person_id| {
            let state = activity_ledger.people.get(person_id)?;
            let transition = state
                .active_transition_id
                .as_deref()
                .and_then(|id| activity_ledger.transitions.get(id))?;
            (state.last_desired_active
                && chiefd_core::store::activity::is_routine_idle_park(transition)
                && transition.status.is_pending())
            .then(|| IdleTransitionObservation {
                person_id: person_id.clone(),
                transition_id: transition.id.clone(),
                requested_at: transition.requested_at.clone(),
                responsible_person_id:
                    chiefd_core::store::health_collect::responsible_mailbox_operator(
                        manifest, person_id,
                    ),
                work_lease_active: work_leases.contains(person_id),
            })
        })
        .collect())
}

fn collect_idle_health(
    dir: &Path,
    manifest: &chiefd_core::store::organization::OrganizationManifest,
    activity_ledger: &chiefd_core::store::activity::ActivityLedger,
    pending_mailbox_people: &BTreeSet<String>,
) -> (Vec<IdleTransitionObservation>, Option<String>) {
    match idle_pane_observations(dir, manifest, activity_ledger, pending_mailbox_people) {
        Ok(observations) => (observations, None),
        Err(error) => (Vec::new(), Some(error)),
    }
}

/// Group the company's ordinary pending mailbox rows into per-recipient health
/// facts.
///
/// It also returns the set of recipients with pending mail, one of the "this
/// person still has work" leases that decides whether an unfinished idle-park
/// transition is actionable for `idle_pane_awaiting_release`.
fn mailbox_observations(
    ledgers: &chiefd_core::ledger::Ledgers,
    manifest: &chiefd_core::store::organization::OrganizationManifest,
    quiesced_since_ms: Option<i64>,
    launch_intent_people: &BTreeSet<&str>,
) -> (Vec<MailboxObservation>, BTreeSet<String>) {
    let mut grouped = BTreeMap::<String, MailboxObservation>::new();
    let mut inspected_rows = BTreeMap::<String, usize>::new();
    let mut pending_mailbox_people = BTreeSet::new();
    for (_, row) in ledgers.mailbox_rows() {
        if row.state != "pending" || row.envelope.health_incident.is_some() {
            continue;
        }
        let person_id = row.person.clone();
        pending_mailbox_people.insert(person_id.clone());
        let person = manifest.person(&person_id);
        let observation = grouped.entry(person_id.clone()).or_insert_with(|| {
            let employment_state = match person.map(|record| record.employment_state) {
                Some(chiefd_core::store::organization::EmploymentState::Active) => "active",
                Some(chiefd_core::store::organization::EmploymentState::Benched) => "benched",
                Some(chiefd_core::store::organization::EmploymentState::Departed) => "departed",
                None => "missing",
            };
            MailboxObservation {
                person_id: person_id.clone(),
                employment_state: employment_state.to_string(),
                unit_active: person.is_none_or(|record| {
                    chiefd_core::store::health_collect::active_department(
                        manifest,
                        &record.department_id,
                    )
                }),
                ordinary_count: 0,
                valid_count: 0,
                oldest_at: None,
                invalid: false,
                quiesce_suppressed: false,
                responsible_person_id:
                    chiefd_core::store::health_collect::responsible_mailbox_operator(
                        manifest, &person_id,
                    ),
            }
        });
        observation.ordinary_count = observation.ordinary_count.saturating_add(1);
        let inspected = inspected_rows.entry(person_id.clone()).or_default();
        if *inspected >= MAX_MAILBOX_HEALTH_ROWS_PER_PERSON {
            continue;
        }
        *inspected += 1;
        let created = row.envelope.created_at.clone();
        // WHAT AN INVALID ENVELOPE ACTUALLY IS.
        //
        // This used to include `row.envelope.organization == manifest.slug`,
        // and those are two different identifiers for the company, not two
        // copies of one. `organization` is DERIVED at reconstruct from the
        // company's ROW SLUG (`mailbox_rows.rs`: `organization:
        // company_slug.to_string()`), which is the directory-keyed id;
        // `manifest.slug` is the company's display slug. `MailboxRow`'s own doc
        // says the in-memory copy of that field is "advisory only" — it is not
        // a fact about the envelope at all.
        //
        // On a company whose two names differ the comparison is false for EVERY
        // row, so every pending envelope was reported poisoned and
        // `mailbox_invalid` was raised on every pass with nothing an operator
        // could do to clear it. Measured on `taperoom-inc` (`4cc439341aa9`),
        // 2026-08-20: eleven different recipients flagged in one afternoon —
        // ordinary messages and person-reminders alike, each moments after the
        // mail arrived, none of them malformed.
        //
        // The scope question the comparison was reaching for is already
        // answered before this line: these rows come from THIS company's
        // ledger. What remains are the three facts that are genuinely about the
        // envelope — its schema, that it names the person whose mailbox holds
        // it, and that its instant parses.
        let valid = row.envelope.schema_version == 1
            && row.envelope.recipients.contains(&person_id)
            && chiefd_core::isotime::parse_iso_millis(&created).is_some();
        if !valid {
            observation.invalid = true;
        } else {
            observation.valid_count = observation.valid_count.saturating_add(1);
        }
        if valid
            && observation.oldest_at.as_ref().is_none_or(|oldest| {
                chiefd_core::isotime::parse_iso_millis(&created)
                    < chiefd_core::isotime::parse_iso_millis(oldest)
            })
        {
            observation.oldest_at = Some(created);
        }
    }
    let mailboxes = grouped
        .into_values()
        .map(|mut mailbox| {
            mailbox.quiesce_suppressed = quiesced_since_ms.is_some_and(|since| {
                !launch_intent_people.contains(mailbox.person_id.as_str())
                    && mailbox
                        .oldest_at
                        .as_deref()
                        .and_then(chiefd_core::isotime::parse_iso_millis)
                        .is_some_and(|oldest| oldest < since)
            });
            mailbox
        })
        .collect();
    (mailboxes, pending_mailbox_people)
}

/// The real [`HealthSnapshotGatherer`] — `chiefd run`'s production wiring
/// (od-host-gatherers-completion).
///
/// Four sources feed one [`HealthCollectionSnapshot`]:
///
/// * chiefd's OWN already-committed ledger (`ctx.snapshot`) supplies
///   `expected_active_people` (`activity::read`) and every supervision
///   effect (`supervision::read`'s `effect_order`/`effect`) — again, chiefd's
///   own native ledger, not a second-store document.
/// * the shared company `org.sqlite` (when `facts` is `Some`) supplies the
///   supervisor-liveness sample and runtime-projection document — the only two
///   health inputs with no committed-ledger equivalent yet — plus the optional
///   goal-delivery quiesce watermark that suppresses stale mail only. A
///   one-daemon deployment does not run a separate `org-supervisor.ts` process
///   to have a heartbeat, and chiefd's actuator recomputes its desired runtime
///   topology fresh every cycle rather than persisting a projection document.
/// * the company data root supplies bounded runtime-log deltas and the
///   readability of each person's Pi session directory. Together with the
///   committed ledger's mailbox facts, these derive the runtime-log, stale-mail
///   and unreadable-session observations without a second health watcher. (It
///   used to derive per-person work leases here too, for the idle-pane incident
///   deleted in #751-P4 — see the module tombstone.)
/// * the company's own `runtime_actuation` row supplies the observation the
///   attached operator client committed — the alive and dead halves together —
///   via the pure [`gather_health_snapshot`] above. Read through [`CompanyDb`]
///   rather than off `ctx.snapshot`: the row store is the authority for that
///   record, and the snapshot's document view is the retired copy.
///
/// When `facts` is `None`, `supervisor`/`runtime` are simply `None` — the
/// same fail-**open** default `read_supervisor_liveness`/`read_runtime_document`
/// give an absent row, matching `health`'s store-wide `FailOpen` polarity
/// (losing this observability never blocks safety, and the pure collector
/// already turns `None` into its own honest `supervisor_not_running` /
/// no-process-expected incidents rather than needing a fabricated substitute).
pub struct HostHealthSnapshotGatherer {
    // No `CompanyDb`. It was held to fetch the runtime sample from the
    // actuator's committed report; that sample is deleted.
    socket_name: String,
    facts: Option<ReconcilerFactsStore>,
    /// Whether anybody is converging this company — the daemon's one shared
    /// [`ActuatorAttendance`] cell, not a copy of its reading.
    attendance: ActuatorAttendance,
    /// The COMPANY DIRECTORY, `<dir>`. Agent homes hang off
    /// `<dir>/.chief/agent/<id>/` and are derived by `agent_home::agent_home`;
    /// the log sink's `<dir>/.chief/logs` is joined below. Storing the
    /// directory and JOINING is one-directional, where storing `.chief` and
    /// recovering `<dir>` from it means walking up — the reconstruction that
    /// made chiefd-log's deleted tier-2 wrong.
    dir: PathBuf,
}

impl HostHealthSnapshotGatherer {
    /// Build the gatherer. `facts` is `None` when the shared facts store is
    /// not configured for this boot (see the type docs).
    #[must_use]
    pub fn new(
        socket_name: impl Into<String>,
        facts: Option<ReconcilerFactsStore>,
        dir: PathBuf,
        attendance: ActuatorAttendance,
    ) -> Self {
        Self { socket_name: socket_name.into(), facts, dir, attendance }
    }
}

impl HealthSnapshotGatherer for HostHealthSnapshotGatherer {
    fn gather_health(
        &self,
        ctx: &DutyContext,
    ) -> BoxFuture<'_, Result<HealthCollectionSnapshot, DutyError>> {
        // Capture owned/cheaply-cloned data up front — see
        // `HostCycleInputGatherer::gather_cycle_input`'s identical note.
        let snapshot = Arc::clone(&ctx.snapshot);
        let socket_name = self.socket_name.clone();
        let facts = self.facts.clone();
        let dir = self.dir.clone();
        let attendance = self.attendance.clone();
        // THE ROW KEY, not the display name. `DutyContext::slug` is the
        // company's directory-derived key (`sha256(canonical <dir>)[..12]`),
        // which is the value every normalized row is written under
        // (`persist_dispatch`: "`row_slug` … selects the SQL rows;
        // `company_slug` is the company's DISPLAY name"). The two reads below
        // used `manifest.slug` — the display name — so they looked up a
        // watermark under a key nothing has ever written a row with, found
        // nothing, and `collect` turned the absence into
        // `supervisor_not_running` on the company's FIRST health pass and on
        // every pass after: 707 consecutive false alarms in one 21-minute
        // observation, from a duty that was succeeding every five seconds the
        // whole time. `gather/reconciler_facts.rs`'s own test warns about
        // exactly this ("a caller that reaches for the manifest slug finds no
        // row at all"), and this was the caller it warned about; the sibling
        // `cycle_input` gatherer already takes the key separately.
        let row_key = ctx.slug.clone();

        Box::pin(async move {
            let ledgers = snapshot.ledgers();
            let now_millis = ledgers.now().0;
            let manifest =
                organization::read(ledgers).map_err(|error| DutyError::new(error.to_string()))?;
            let activity_ledger = activity::read(ledgers, &manifest)
                .map_err(|error| DutyError::new(error.to_string()))?;
            let supervision_ledger = supervision::read_reconciled(ledgers, &manifest)
                .map_err(|error| DutyError::new(error.to_string()))?;
            let health_context = organization::company_context(&manifest)
                .map_err(|error| DutyError::new(format!("{}: {}", error.code, error.message)))?;
            let (health_state, _warning) = health::read(ledgers, &health_context).into_parts();
            let (mut log_incidents, log_cursors) = collect_log_observations(
                &dir.join(crate::converge_apply::cycle::CHIEF_DIR).join("logs"),
                &health_state.cursors,
                health_state.last_run_at.is_none(),
            )
            .map_err(DutyError::new)?;
            log_incidents.retain(|incident| {
                !is_resolved_roster_transition_race(incident, |person_id, transition_id| {
                    activity_ledger.transitions.get(transition_id).is_some_and(|transition| {
                        transition.person_id == person_id
                            && matches!(
                                transition.status,
                                chiefd_core::store::activity::TransitionStatus::Applied
                                    | chiefd_core::store::activity::TransitionStatus::Cancelled
                            )
                    })
                })
            });
            let (launch_intent, _warning) =
                launch_intent::read(ledgers, &health_context).into_parts();
            let launch_intent_people: std::collections::BTreeSet<&str> =
                launch_intent.person_ids().iter().map(String::as_str).collect();
            let quiesced_since_ms =
                facts.as_ref().and_then(|facts| facts.goal_delivery_quiesced_since(&row_key));

            let expected_active_people: Vec<String> = activity_ledger
                .people
                .iter()
                .filter(|(_, state)| state.last_desired_active)
                .map(|(person_id, _)| person_id.clone())
                .collect();

            let supervision_effects: Vec<SupervisionEffectObservation> = supervision_ledger
                .effect_order()
                .iter()
                .filter_map(|id| supervision_ledger.effect(id))
                .map(|effect| SupervisionEffectObservation {
                    id: effect.id.clone(),
                    kind: effect.kind.clone(),
                    status: effect_status_str(effect.status).to_string(),
                    created_at: Some(effect.created_at.clone()),
                    delivery_failure_count: effect.delivery_failure_count.map(i64::from),
                    failed_at: effect.failed_at.clone(),
                    last_delivery_failure_at: effect.last_delivery_failure_at.clone(),
                    manager_person_id: effect
                        .payload
                        .get("managerPersonId")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    assignee_person_id: effect
                        .payload
                        .get("assigneePersonId")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    escalation_person_id: effect
                        .payload
                        .get("escalationPersonId")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                })
                .collect();

            let (mailboxes, pending_mailbox_people) =
                mailbox_observations(ledgers, &manifest, quiesced_since_ms, &launch_intent_people);
            let (idle_transitions, idle_supervision_error) =
                collect_idle_health(&dir, &manifest, &activity_ledger, &pending_mailbox_people);

            let (supervisor, runtime) = match &facts {
                Some(facts) => facts.health_durable_facts(&row_key).map_err(DutyError::new)?,
                None => (None, None),
            };

            // The converge cycle's own liveness. Read from the durable
            // converge-safety document, which is the only thing that still
            // knows whether a pass is running — the advisory runtime marker
            // this incident used to read is gone.
            let converge_cycle = {
                // The RAW document, not `effective_config()`: the breaker-folded
                // projection drops the cycle fields entirely, and the question
                // here is literally "is a pass running, and since when".
                let doc = chiefd_core::store::converge_safety::read(&snapshot).into_parts().0;
                Some(chiefd_core::store::health_collect::ConvergeCycleObservation {
                    in_progress: doc.cycle_in_progress,
                    started_at_ms: doc.cycle_started_at_ms,
                })
            };

            let gather_ctx = HealthGatherContext {
                now_millis,
                converge_cycle,
                socket_name: &socket_name,
                supervisor,
                supervisor_stale_ms: None,
                supervision_effect_stale_ms: None,
                runtime,
                expected_active_people,
                supervision_effects,
                log_incidents,
                log_cursors,
                idle_transitions,
                idle_supervision_error,
                mailboxes,
                // Both halves of the subtraction, taken here rather than in the
                // pure fold: the fold receives a duration, not two timestamps,
                // so it can never be handed readings from two clocks. The stamp
                // is written by the desired-set route off this same company's
                // injected clock (`SupervisionLiveSource::clock_now`), which is
                // the clock `now_millis` above reads too.
                actuator_silent_ms: attendance.silent_ms(now_millis),
            };
            gather_health_snapshot(gather_ctx).map_err(|error| DutyError::new(error.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TOMBSTONE: this module's subject was the observation-to-sample mapping --
    // trusted reports, untrusted reasons, lapsed leases, dead processes. There
    // is no mapping, so those tests have no subject.
    //
    // The one property worth keeping is the fail-safe direction, and it is now
    // a single assertion.

    /// A GOOD ENVELOPE IN A COMPANY WHOSE ROW SLUG IS NOT ITS DISPLAY SLUG IS
    /// NOT POISONED.
    ///
    /// `mailbox_invalid` fired for eleven different people on the operator's
    /// company inside one afternoon — ordinary messages and person-reminders
    /// alike, each raised once, moments after the mail arrived. Nothing was
    /// wrong with any of them.
    ///
    /// The check tested `row.envelope.organization == manifest.slug`. Both
    /// halves are real strings and they are not the same identifier: the
    /// envelope's `organization` is DERIVED at reconstruct from the company's
    /// ROW SLUG (`mailbox_rows.rs`: `organization: company_slug.to_string()`,
    /// and `MailboxRow`'s own doc calls the in-memory copy "advisory only"),
    /// while `manifest.slug` is the company's display slug. On a company where
    /// those differ — `4cc439341aa9` against `taperoom-inc` — EVERY pending
    /// envelope reads as invalid, for ever, because nothing about the envelope
    /// can ever make the comparison true.
    #[test]
    fn a_pending_envelope_is_not_invalid_merely_because_the_slugs_are_two_names() {
        let mut manifest = chiefd_core::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "taperoom-inc".to_owned();
        let mut ledgers =
            chiefd_core::ledger::Ledgers::empty(chiefd_core::clock::WallMillis(1_784_116_800_000));
        let envelope_id = "msg-7d48d639@quant-head";
        ledgers.put_mailbox(
            envelope_id,
            chiefd_core::ledger::MailboxRow {
                person: "quant-head".to_owned(),
                envelope: chiefd_core::store::mailbox::MailboxEnvelope {
                    schema_version: 1,
                    id: "msg-7d48d639".to_owned(),
                    // The row slug, which is what `reconstruct` derives this
                    // from — NOT the manifest's display slug.
                    organization: "4cc439341aa9".to_owned(),
                    from_person_id: "chief".to_owned(),
                    to: "quant-head".to_owned(),
                    recipients: vec!["quant-head".to_owned()],
                    body: "please look at the trade journal".to_owned(),
                    urgency: chiefd_core::store::mailbox::Urgency::Normal,
                    reply_to: None,
                    health_incident: None,
                    created_at: "2026-08-20T17:23:38.000Z".to_owned(),
                },
                state: "pending".to_owned(),
                updated_at: 1_784_116_800_000,
            },
        );

        let (observations, pending) =
            mailbox_observations(&ledgers, &manifest, None, &BTreeSet::new());
        assert!(pending.contains("quant-head"), "the row is pending mail for them");
        let observation = observations
            .iter()
            .find(|o| o.person_id == "quant-head")
            .expect("one observation for the recipient");
        assert!(
            !observation.invalid,
            "a well-formed envelope was called poisoned because the company has two names \
             for itself; this raises `mailbox_invalid` on every pass and nothing can ever \
             clear it"
        );
        assert_eq!(observation.valid_count, 1, "and it counts as the valid mail it is");
    }

    /// AND A GENUINELY MALFORMED ENVELOPE IS STILL CAUGHT. The repair above
    /// removes one comparison; it must not remove the alarm. Each of the three
    /// surviving facts is about the envelope itself.
    #[test]
    fn a_malformed_envelope_is_still_reported_invalid() {
        let manifest = chiefd_core::test_support::northstar_manifest(1_784_116_800_000);
        let base = |id: &str| chiefd_core::store::mailbox::MailboxEnvelope {
            schema_version: 1,
            id: id.to_owned(),
            organization: "4cc439341aa9".to_owned(),
            from_person_id: "chief".to_owned(),
            to: "quant-head".to_owned(),
            recipients: vec!["quant-head".to_owned()],
            body: "look at this".to_owned(),
            urgency: chiefd_core::store::mailbox::Urgency::Normal,
            reply_to: None,
            health_incident: None,
            created_at: "2026-08-20T17:23:38.000Z".to_owned(),
        };
        let cases: Vec<(&str, chiefd_core::store::mailbox::MailboxEnvelope)> = vec![
            ("a schema this daemon does not model", {
                let mut e = base("msg-schema");
                e.schema_version = 2;
                e
            }),
            ("an envelope addressed to somebody else", {
                let mut e = base("msg-recipient");
                e.recipients = vec!["signal-researcher".to_owned()];
                e
            }),
            ("an instant that does not parse", {
                let mut e = base("msg-clock");
                e.created_at = "whenever".to_owned();
                e
            }),
        ];
        for (why, envelope) in cases {
            let mut ledgers = chiefd_core::ledger::Ledgers::empty(chiefd_core::clock::WallMillis(
                1_784_116_800_000,
            ));
            let envelope_id = format!("{}@quant-head", envelope.id);
            ledgers.put_mailbox(
                envelope_id,
                chiefd_core::ledger::MailboxRow {
                    person: "quant-head".to_owned(),
                    envelope,
                    state: "pending".to_owned(),
                    updated_at: 1_784_116_800_000,
                },
            );
            let (observations, _) =
                mailbox_observations(&ledgers, &manifest, None, &BTreeSet::new());
            let observation = observations
                .iter()
                .find(|o| o.person_id == "quant-head")
                .expect("one observation for the recipient");
            assert!(observation.invalid, "{why} must still raise mailbox_invalid");
        }
    }

    #[test]
    fn no_sample_is_not_run_and_never_an_empty_audit() {
        let (sample, dead) = runtime_sample(0).expect("chiefd always has an answer");
        assert!(
            matches!(sample, RuntimeSample::NotRun),
            "having no host facts is NotRun; `Audited {{ exists: false }}` would claim \
             chiefd looked and found nothing, which raises runtime_session_missing"
        );
        assert!(dead.is_empty());
    }
}
