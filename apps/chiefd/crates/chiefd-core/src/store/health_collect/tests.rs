//! Red/green coverage for the pure health-collection pass.

use super::*;
use crate::store::organization::OrganizationManifest;
use crate::test_support::northstar_manifest;

const EPOCH: i64 = 1_784_116_800_000; // 2026-07-15T12:00:00.000Z

fn manifest() -> OrganizationManifest {
    northstar_manifest(EPOCH)
}

fn iso(millis: i64) -> String {
    crate::isotime::iso_millis(millis)
}

/// A snapshot with a healthy running supervisor and nothing else observed — the
/// baseline that must produce no incidents.
fn base(now: i64) -> HealthCollectionSnapshot {
    HealthCollectionSnapshot {
        now_millis: now,
        // No converge observation by default: an unread cycle is no opinion,
        // and must raise no incident.
        converge_cycle: None,
        socket_name: "sock".to_string(),
        supervisor: Some(SupervisorObservation {
            status: "running".to_string(),
            interval_ms: 30_000,
            last_heartbeat_at: Some(iso(now)),
            last_error: None,
        }),
        supervisor_stale_ms: None,
        supervision_effect_stale_ms: None,
        runtime: None,
        expected_active_people: Vec::new(),
        runtime_audit: RuntimeSample::NotRun,
        dead_processes: Vec::new(),
        supervision_effects: Vec::new(),
        log_incidents: Vec::new(),
        log_cursors: std::collections::BTreeMap::new(),
        idle_transitions: Vec::new(),
        idle_supervision_error: None,
        mailboxes: Vec::new(),
        mailbox_stale_ms: None,
        idle_transition_stale_ms: None,
        // Attended: the baseline must raise nothing, so the desired set was
        // read at this very instant.
        actuator_silent_ms: 0,
    }
}

fn kinds(incidents: &[IncidentCandidate]) -> Vec<String> {
    incidents.iter().map(|incident| incident.kind.clone()).collect()
}

fn has(incidents: &[IncidentCandidate], kind: &str) -> bool {
    incidents.iter().any(|incident| incident.kind == kind)
}

fn find<'a>(incidents: &'a [IncidentCandidate], kind: &str) -> &'a IncidentCandidate {
    incidents.iter().find(|incident| incident.kind == kind).expect("kind present")
}

/// A matching runtime doc carrying an in-progress reconciliation; `started_at`
/// controls whether it is fresh or stalled.
/// A converge cycle that is RUNNING and began at `started_at`.
fn running_cycle(started_at: i64) -> Option<ConvergeCycleObservation> {
    Some(ConvergeCycleObservation { in_progress: true, started_at_ms: Some(started_at) })
}

fn matching_runtime(started_at: i64, process_handles: &[&str]) -> RuntimeDocObservation {
    RuntimeDocObservation {
        version: Some(1),
        socket_name: Some("sock".to_string()),
        process_person_ids: process_handles.iter().map(ToString::to_string).collect(),
        reconciliation: Some(RuntimeReconciliationObservation {
            phase: "in_progress".to_string(),
            started_at: Some(iso(started_at)),
        }),
    }
}

#[test]
fn a_healthy_snapshot_produces_no_incidents() {
    let incidents = collect(&manifest(), &base(EPOCH));
    assert!(incidents.is_empty(), "got {:?}", kinds(&incidents));
}

/// THE 22:17:40 OUTAGE, as the pure collector sees it.
///
/// The tmux server went away with every pane and every person; the actuator
/// died with it and stopped reading the desired set. Nothing else about the
/// company changed, and that is the point — every other observation in this
/// snapshot is the healthy baseline, so this incident is the ONLY thing
/// standing between chiefd and forty minutes of reporting a healthy pass over
/// a runtime that did not exist.
#[test]
fn an_actuator_that_stopped_reading_the_desired_set_is_reported_unattended() {
    let mut snapshot = base(EPOCH);
    snapshot.actuator_silent_ms = crate::runtime::attendance::ACTUATOR_LAPSE_MS + 1;
    let incidents = collect(&manifest(), &snapshot);
    assert!(has(&incidents, "runtime_unattended"), "got {:?}", kinds(&incidents));
}

/// The boundary belongs to the attended side: a company read exactly one lapse
/// window ago has not yet missed anything.
#[test]
fn silence_within_the_lapse_window_is_not_an_incident() {
    let mut snapshot = base(EPOCH);
    snapshot.actuator_silent_ms = crate::runtime::attendance::ACTUATOR_LAPSE_MS;
    assert!(!has(&collect(&manifest(), &snapshot), "runtime_unattended"));
}

/// The detail must not carry how long the silence has run.
///
/// A moving detail mints a fresh fingerprint on every pass, so dedup can never
/// match the prior sighting and the alert repeats for ever — the failure
/// `IncidentCandidate::identity` was added to describe. Two passes a minute
/// apart must produce the SAME candidate.
#[test]
fn an_unattended_company_reports_the_same_detail_however_long_it_lasts() {
    let mut first = base(EPOCH);
    first.actuator_silent_ms = crate::runtime::attendance::ACTUATOR_LAPSE_MS + 1;
    let mut later = base(EPOCH + 3_600_000);
    later.actuator_silent_ms = first.actuator_silent_ms + 3_600_000;
    assert_eq!(
        find(&collect(&manifest(), &first), "runtime_unattended").detail,
        find(&collect(&manifest(), &later), "runtime_unattended").detail,
    );
}

#[test]
fn an_absent_supervisor_is_not_running() {
    let mut snapshot = base(EPOCH);
    snapshot.supervisor = None;
    let incidents = collect(&manifest(), &snapshot);
    assert!(has(&incidents, "supervisor_not_running"));
}

#[test]
fn a_stopped_supervisor_is_not_running() {
    let mut snapshot = base(EPOCH);
    snapshot.supervisor.as_mut().unwrap().status = "stopped".to_string();
    assert!(has(&collect(&manifest(), &snapshot), "supervisor_not_running"));
}

#[test]
fn an_old_heartbeat_is_stale_and_a_last_error_is_reported() {
    let mut snapshot = base(EPOCH);
    let supervisor = snapshot.supervisor.as_mut().unwrap();
    // 20 minutes old, past the 15-minute floor.
    supervisor.last_heartbeat_at = Some(iso(EPOCH - 20 * 60_000));
    supervisor.last_error = Some("poll failed".to_string());
    let incidents = collect(&manifest(), &snapshot);
    assert!(has(&incidents, "supervisor_stale"));
    assert!(find(&incidents, "supervisor_stale").detail.contains("20m old"));
    assert!(has(&incidents, "supervisor_error"));
    // supervisor_stale has a moving-minutes detail but deliberately NO identity.
    assert!(find(&incidents, "supervisor_stale").identity.is_none());
}

#[test]
fn an_unparseable_heartbeat_is_stale_and_reads_invalid() {
    let mut snapshot = base(EPOCH);
    snapshot.supervisor.as_mut().unwrap().last_heartbeat_at = Some("not-a-date".to_string());
    let incidents = collect(&manifest(), &snapshot);
    assert_eq!(find(&incidents, "supervisor_stale").detail, "heartbeat is invalid");
}

#[test]
fn a_reconciliation_past_its_lease_is_stalled() {
    let mut snapshot = base(EPOCH);
    // A converge cycle that began 2 minutes ago — past the 60s recovery lease.
    snapshot.runtime = Some(matching_runtime(EPOCH - 120_000, &[]));
    snapshot.converge_cycle = running_cycle(EPOCH - 120_000);
    let incidents = collect(&manifest(), &snapshot);
    assert!(has(&incidents, "runtime_reconciliation_stalled"));
}

/// The production shape raises nothing on its own: an absent marker is not a
/// stalled cycle. Paired with `a_wedged_cycle_is_reported_on_the_production_
/// runtime_shape` below, which proves the signal is nonetheless ALIVE.
#[test]
fn the_deleted_runtime_marker_alone_never_raises_a_stall() {
    let mut snapshot = base(EPOCH);
    // Exactly what every producer writes today: a runtime doc whose
    // reconciliation marker is absent, and no converge observation.
    snapshot.runtime = Some(RuntimeDocObservation {
        version: Some(1),
        socket_name: Some("sock".to_string()),
        process_person_ids: Vec::new(),
        reconciliation: None,
    });

    let incidents = collect(&manifest(), &snapshot);

    assert!(
        !has(&incidents, "runtime_reconciliation_stalled"),
        "an absent marker is not a stalled cycle; got {:?}",
        kinds(&incidents)
    );
}

/// THE regression test. The incident used to read `runtime.reconciliation`, an
/// advisory marker the runtime-lifecycle port DELETED — and `runtime_matches`
/// returns false the moment that marker is absent, so with every producer now
/// writing `None` the incident became UNREACHABLE. A company whose converge
/// cycle was genuinely wedged raised nothing at all, while the old tests passed
/// by constructing a `reconciliation: Some(..)` shape production can no longer
/// produce. A health signal that cannot fire is indistinguishable from a
/// healthy fleet.
///
/// This drives the PRODUCTION runtime shape (no marker) with a converge cycle
/// that is genuinely stuck, and requires the incident. Against the old code it
/// is impossible to satisfy.
#[test]
fn a_wedged_cycle_is_reported_on_the_production_runtime_shape() {
    let mut snapshot = base(EPOCH);
    // Exactly what every producer writes: no reconciliation marker.
    snapshot.runtime = Some(RuntimeDocObservation {
        version: Some(1),
        socket_name: Some("sock".to_string()),
        process_person_ids: Vec::new(),
        reconciliation: None,
    });
    // …and a converge pass that began well past its recovery lease.
    snapshot.converge_cycle = running_cycle(EPOCH - 120_000);

    let incidents = collect(&manifest(), &snapshot);

    assert!(
        has(&incidents, "runtime_reconciliation_stalled"),
        "a wedged converge cycle must be reported; got {:?}",
        kinds(&incidents)
    );
}

/// An idle converge cycle is not a stalled one, however old its last start.
#[test]
fn a_cycle_that_is_not_running_never_raises_a_stall() {
    let mut snapshot = base(EPOCH);
    snapshot.converge_cycle = Some(ConvergeCycleObservation {
        in_progress: false,
        started_at_ms: Some(EPOCH - 3_600_000),
    });

    let incidents = collect(&manifest(), &snapshot);

    assert!(!has(&incidents, "runtime_reconciliation_stalled"), "got {:?}", kinds(&incidents));
}

#[test]
fn a_fresh_reconciliation_suppresses_the_activity_mismatch_it_is_resolving() {
    let mut snapshot = base(EPOCH);
    // Fresh (age 0 < 60s), so reconciliation-in-progress; processes differ from the
    // expected set, which without the in-progress gate would page.
    snapshot.runtime = Some(matching_runtime(EPOCH, &["chief"]));
    snapshot.converge_cycle = running_cycle(EPOCH);
    snapshot.expected_active_people = vec!["quant-head".to_string()];
    let incidents = collect(&manifest(), &snapshot);
    assert!(!has(&incidents, "runtime_activity_mismatch"), "got {:?}", kinds(&incidents));
    assert!(!has(&incidents, "runtime_reconciliation_stalled"));
}

#[test]
fn runtime_processes_that_differ_from_desired_activity_page_when_not_reconciling() {
    let mut snapshot = base(EPOCH);
    // No reconciliation marker → not in progress. Runtime shows a process the
    // activity ledger does not desire.
    snapshot.runtime = Some(RuntimeDocObservation {
        process_person_ids: vec!["chief".to_string()],
        ..RuntimeDocObservation::default()
    });
    snapshot.expected_active_people = vec!["quant-head".to_string()];
    let incidents = collect(&manifest(), &snapshot);
    let incident = find(&incidents, "runtime_activity_mismatch");
    assert!(incident.detail.contains("[chief]"));
    assert!(incident.detail.contains("[quant-head]"));
}

#[test]
fn a_runtime_audit_failure_is_a_runtime_ownership_conflict_incident_carrying_raw_detail() {
    let mut snapshot = base(EPOCH);
    snapshot.runtime_audit =
        RuntimeSample::Failed { message: "runtime server exited unexpectedly".to_string() };
    let incidents = collect(&manifest(), &snapshot);
    let incident = find(&incidents, "runtime_ownership_conflict");
    assert_eq!(incident.detail, "runtime server exited unexpectedly");
}

#[test]
fn observed_processes_that_differ_from_runtime_are_a_mismatch() {
    let mut snapshot = base(EPOCH);
    snapshot.runtime = Some(RuntimeDocObservation {
        process_person_ids: vec!["chief".to_string()],
        ..RuntimeDocObservation::default()
    });
    snapshot.expected_active_people = vec!["chief".to_string()];
    snapshot.runtime_audit =
        RuntimeSample::Audited { exists: true, process_person_ids: vec!["quant-head".to_string()] };
    assert!(has(&collect(&manifest(), &snapshot), "runtime_projection_mismatch"));
}

#[test]
fn an_absent_session_with_expected_people_is_reported_missing() {
    let mut snapshot = base(EPOCH);
    snapshot.expected_active_people = vec!["chief".to_string()];
    snapshot.runtime_audit =
        RuntimeSample::Audited { exists: false, process_person_ids: Vec::new() };
    assert!(has(&collect(&manifest(), &snapshot), "runtime_session_missing"));
}

#[test]
fn an_absent_session_with_no_expected_people_is_silent() {
    let mut snapshot = base(EPOCH);
    snapshot.runtime_audit =
        RuntimeSample::Audited { exists: false, process_person_ids: Vec::new() };
    assert!(!has(&collect(&manifest(), &snapshot), "runtime_session_missing"));
}

#[test]
fn dead_tagged_processes_are_reported() {
    let mut snapshot = base(EPOCH);
    snapshot.runtime_audit =
        RuntimeSample::Audited { exists: true, process_person_ids: Vec::new() };
    snapshot.dead_processes = vec!["zoe".to_string(), "ada".to_string()];
    let incidents = collect(&manifest(), &snapshot);
    let incident = find(&incidents, "runtime_dead_processes");
    assert_eq!(incident.detail, "dead runtime processes [ada,zoe]", "ids are sorted");
}

#[test]
fn a_pending_supervision_effect_beyond_the_threshold_is_stalled() {
    let mut snapshot = base(EPOCH);
    snapshot.supervision_effects = vec![SupervisionEffectObservation {
        id: "effect-1".to_string(),
        kind: "assignment_delivery".to_string(),
        status: "pending".to_string(),
        created_at: Some(iso(EPOCH - 20 * 60_000)), // 20m, past 15m
        delivery_failure_count: Some(2),
        failed_at: None,
        last_delivery_failure_at: None,
        manager_person_id: None,
        assignee_person_id: Some("signal-researcher".to_string()),
        escalation_person_id: None,
    }];
    let incidents = collect(&manifest(), &snapshot);
    let incident = find(&incidents, "supervision_delivery_stalled");
    // Assignee-owned effect: impaired mailbox is the assignee; the owner is the
    // operator responsible for that assignee's mailbox (its head, quant-head).
    assert_eq!(incident.impaired_mailbox_person_id.as_deref(), Some("signal-researcher"));
    assert_eq!(incident.responsible_person_id.as_deref(), Some("quant-head"));
    assert_eq!(incident.observed_count, Some(2));
    assert_eq!(incident.oldest_at.as_deref(), Some(iso(EPOCH - 20 * 60_000).as_str()));
}

#[test]
fn a_recent_pending_effect_is_not_yet_stalled() {
    let mut snapshot = base(EPOCH);
    snapshot.supervision_effects = vec![SupervisionEffectObservation {
        id: "effect-1".to_string(),
        kind: "assignment_delivery".to_string(),
        status: "pending".to_string(),
        created_at: Some(iso(EPOCH - 60_000)),
        delivery_failure_count: None,
        failed_at: None,
        last_delivery_failure_at: None,
        manager_person_id: None,
        assignee_person_id: Some("signal-researcher".to_string()),
        escalation_person_id: None,
    }];
    assert!(!has(&collect(&manifest(), &snapshot), "supervision_delivery_stalled"));
}

#[test]
fn a_failed_manager_effect_is_owned_and_impaired_by_the_manager() {
    let mut snapshot = base(EPOCH);
    snapshot.supervision_effects = vec![SupervisionEffectObservation {
        id: "effect-9".to_string(),
        kind: "manager_goal_watch".to_string(),
        status: "failed".to_string(),
        created_at: Some(iso(EPOCH - 60_000)),
        delivery_failure_count: Some(4),
        failed_at: Some(iso(EPOCH - 30_000)),
        last_delivery_failure_at: Some(iso(EPOCH - 45_000)),
        manager_person_id: Some("quant-head".to_string()),
        assignee_person_id: None,
        escalation_person_id: None,
    }];
    let incidents = collect(&manifest(), &snapshot);
    let incident = find(&incidents, "supervision_delivery_failed");
    assert_eq!(incident.responsible_person_id.as_deref(), Some("quant-head"));
    assert_eq!(incident.impaired_mailbox_person_id.as_deref(), Some("quant-head"));
    assert_eq!(incident.observed_count, Some(4));
    // failedAt wins over lastDeliveryFailureAt for oldest_at.
    assert_eq!(incident.oldest_at.as_deref(), Some(iso(EPOCH - 30_000).as_str()));
}

#[test]
fn a_failed_effect_falls_back_to_last_delivery_failure_at_for_oldest() {
    let mut snapshot = base(EPOCH);
    snapshot.supervision_effects = vec![SupervisionEffectObservation {
        id: "effect-9".to_string(),
        kind: "manager_goal_stalled".to_string(),
        status: "failed".to_string(),
        created_at: Some(iso(EPOCH - 60_000)),
        delivery_failure_count: Some(1),
        failed_at: None,
        last_delivery_failure_at: Some(iso(EPOCH - 45_000)),
        manager_person_id: None,
        assignee_person_id: None,
        escalation_person_id: Some("it-head".to_string()),
    }];
    let incidents = collect(&manifest(), &snapshot);
    let incident = find(&incidents, "supervision_delivery_failed");
    // manager_goal_stalled routes to the escalation person.
    assert_eq!(incident.impaired_mailbox_person_id.as_deref(), Some("it-head"));
    assert_eq!(incident.oldest_at.as_deref(), Some(iso(EPOCH - 45_000).as_str()));
}

#[test]
fn the_responsible_operator_of_an_unknown_person_is_the_ceo() {
    assert_eq!(responsible_mailbox_operator(&manifest(), "nobody"), "chief");
}

#[test]
fn the_responsible_operator_of_a_worker_is_its_operational_head() {
    assert_eq!(responsible_mailbox_operator(&manifest(), "signal-researcher"), "quant-head");
}

#[test]
fn legacy_producer_inventory_is_exact_and_non_vacuous() {
    assert_eq!(
        LEGACY_HEALTH_PRODUCER_PARITY,
        [
            "exception",
            "runtime_log_error",
            // Renamed from `idle_pane_awaiting_reflection` in #751-P4: the
            // condition it detects is unchanged, only "finishing the
            // transition" stopped meaning "recording a reflection".
            "idle_pane_awaiting_release",
            "idle_supervision_error",
            "mailbox_recipient_inactive",
            "mailbox_unit_inactive",
            "mailbox_invalid",
            "mailbox_delivery_stale",
        ]
    );
}

#[test]
fn bounded_exception_and_runtime_log_observations_produce_both_kinds_raw_for_one_fold_redaction() {
    let mut snapshot = base(EPOCH);
    snapshot.log_incidents = vec![
        LogIncidentObservation {
            kind: "exception".into(),
            source: "supervision".into(),
            detail: "failed token=secret".into(),
            structured_exception: true,
        },
        LogIncidentObservation {
            kind: "runtime_log_error".into(),
            source: "worker.log".into(),
            detail: "retrying provider key=secret".into(),
            structured_exception: false,
        },
    ];
    let incidents = collect(&manifest(), &snapshot);
    assert!(has(&incidents, "exception"));
    assert!(has(&incidents, "runtime_log_error"));
    assert!(find(&incidents, "exception").detail.contains("token=secret"));
}

/// A work-free person whose idle-park transition has not been released past the
/// threshold pages the responsible operator, and current durable work makes the
/// same transition non-actionable.
///
/// Renamed from `idle_pane_awaiting_reflection` in #751-P4. The condition is
/// unchanged — only "finishing the transition" stopped meaning "recording a
/// reflection" — and this is the only alarm that covers an intent-bound park or
/// a stalled offboard wedging a pane, which `Forced` does not rescue.
#[test]
fn a_stale_unreleased_idle_pane_pages_its_operator_unless_work_still_leases_the_person() {
    let mut snapshot = base(EPOCH);
    snapshot.idle_transitions.push(IdleTransitionObservation {
        person_id: "signal-researcher".into(),
        transition_id: "transition:7:signal-researcher:park".into(),
        requested_at: iso(EPOCH - IDLE_TRANSITION_STALE_MS),
        responsible_person_id: "quant-head".into(),
        work_lease_active: false,
    });
    let incidents = collect(&manifest(), &snapshot);
    let idle = find(&incidents, "idle_pane_awaiting_release");
    assert_eq!(idle.responsible_person_id.as_deref(), Some("quant-head"));
    assert_eq!(idle.observed_count, Some(1));

    snapshot.idle_transitions[0].work_lease_active = true;
    let leased = collect(&manifest(), &snapshot);
    assert!(
        !has(&leased, "idle_pane_awaiting_release"),
        "current durable work makes the old idle transition non-actionable"
    );

    snapshot.idle_transitions[0].work_lease_active = false;
    snapshot.idle_transitions.clear();
    assert!(!has(&collect(&manifest(), &snapshot), "idle_pane_awaiting_release"));
}

/// The idle sub-detector's other half: an unreadable Pi session directory is a
/// real host fault on its own and pages raw, clearing with its input.
#[test]
fn an_idle_supervision_read_failure_pages_raw_and_clears_with_its_input() {
    let mut snapshot = base(EPOCH);
    snapshot.idle_supervision_error = Some("could not read supervision token=secret".into());
    let incidents = collect(&manifest(), &snapshot);
    assert!(has(&incidents, "idle_supervision_error"));
    // Raw, not pre-redacted: `apply_cycle` is the single redaction site.
    assert!(find(&incidents, "idle_supervision_error").detail.contains("token=secret"));

    snapshot.idle_supervision_error = None;
    assert!(!has(&collect(&manifest(), &snapshot), "idle_supervision_error"));
}

fn mailbox(person: &str) -> MailboxObservation {
    MailboxObservation {
        person_id: person.into(),
        employment_state: "active".into(),
        unit_active: true,
        ordinary_count: 2,
        valid_count: 2,
        oldest_at: Some(iso(EPOCH - MAILBOX_STALE_MS)),
        invalid: false,
        quiesce_suppressed: false,
        responsible_person_id: "quant-head".into(),
    }
}

#[test]
fn mailbox_recipient_and_unit_lifecycle_kinds_match_the_legacy_precedence() {
    let mut snapshot = base(EPOCH);
    let mut departed = mailbox("departed-person");
    departed.employment_state = "departed".into();
    let mut benched = mailbox("benched-person");
    benched.employment_state = "benched".into();
    let mut paused = mailbox("signal-researcher");
    paused.unit_active = false;
    snapshot.mailboxes = vec![departed, benched, paused];
    let incidents = collect(&manifest(), &snapshot);
    assert_eq!(
        kinds(&incidents).iter().filter(|kind| *kind == "mailbox_recipient_inactive").count(),
        2
    );
    assert!(has(&incidents, "mailbox_unit_inactive"));
    assert!(!has(&incidents, "mailbox_delivery_stale"), "inactive precedence stops stale emission");
}

#[test]
fn invalid_and_stale_mailbox_incidents_preserve_counts_oldest_and_quiesce_suppression() {
    let mut snapshot = base(EPOCH);
    let mut observed = mailbox("signal-researcher");
    observed.invalid = true;
    snapshot.mailboxes = vec![observed.clone()];
    let incidents = collect(&manifest(), &snapshot);
    assert!(has(&incidents, "mailbox_invalid"));
    let stale = find(&incidents, "mailbox_delivery_stale");
    assert_eq!(stale.observed_count, Some(2));
    assert_eq!(stale.oldest_at, observed.oldest_at);

    observed.invalid = false;
    observed.quiesce_suppressed = true;
    snapshot.mailboxes = vec![observed];
    let suppressed = collect(&manifest(), &snapshot);
    assert!(!has(&suppressed, "mailbox_invalid"));
    assert!(!has(&suppressed, "mailbox_delivery_stale"));
}

#[test]
fn new_producers_use_the_existing_redaction_dedup_and_resolution_lifecycle() {
    let mut snapshot = base(EPOCH);
    snapshot.log_incidents.push(LogIncidentObservation {
        kind: "runtime_log_error".into(),
        source: "worker.log".into(),
        detail: "provider failed token=top-secret".into(),
        structured_exception: false,
    });
    let first_candidates = collect(&manifest(), &snapshot);
    let mut state = crate::store::health::HealthMonitorState::empty("northstar");
    let first = crate::store::health::apply_cycle(
        &mut state,
        &first_candidates,
        EPOCH,
        &crate::store::health::NeverResolves,
    );
    assert_eq!(first.new_incidents.len(), 1);
    assert!(!first.new_incidents[0].detail.contains("top-secret"));
    assert_eq!(first.new_incidents[0].count, 1);

    let repeated = crate::store::health::apply_cycle(
        &mut state,
        &first_candidates,
        EPOCH + 1,
        &crate::store::health::NeverResolves,
    );
    assert!(repeated.new_incidents.is_empty(), "same fingerprint deduplicates");
    assert_eq!(repeated.active[0].count, 2);

    let cleared = crate::store::health::apply_cycle(
        &mut state,
        &[],
        EPOCH + 2,
        &crate::store::health::NeverResolves,
    );
    assert_eq!(cleared.resolved_fingerprints.len(), 1);
    assert!(cleared.active.is_empty());
}
