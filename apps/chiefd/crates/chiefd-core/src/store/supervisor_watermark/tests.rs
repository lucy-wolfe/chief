//! Supervisor-watermark tests. Real `Ledgers`, no mocks; time is an argument.

use super::*;
use crate::clock::WallMillis;

const EPOCH: i64 = 1_784_116_800_000; // 2026-07-15T12:00:00.000Z
const HOUR_MS: i64 = 60 * 60 * 1_000;

fn ctx() -> CompanyContext {
    CompanyContext::new("cobalt", "chief", ["chief"].map(String::from))
}

fn ledgers() -> Ledgers {
    Ledgers::empty(WallMillis(EPOCH))
}

// --- the durable watermark --------------------------------------------------

#[test]
fn a_fresh_company_has_an_empty_watermark_named_for_itself() {
    let l = ledgers();
    let (state, warning) = read(&l, &ctx()).into_parts();
    assert_eq!(state, SupervisorWatermarkState::empty("cobalt"));
    assert!(state.duties.is_empty());
    // Absence is not corruption: a company that has run no duty simply has no
    // row, and that is a clean empty, not a warning.
    assert_eq!(warning, None);
}

#[test]
fn record_success_upserts_the_duty_and_counts_runs() {
    let mut l = ledgers();
    record_success(&mut l, &ctx(), Duty::MailboxWake, EPOCH);
    record_success(&mut l, &ctx(), Duty::MailboxWake, EPOCH + 30_000);

    let (state, _) = read(&l, &ctx()).into_parts();
    let watermark = state.duties.get(Duty::MailboxWake.as_str()).expect("recorded");
    assert_eq!(watermark.duty, "mailbox_wake");
    assert_eq!(watermark.interval_ms, Duty::MailboxWake.interval_ms());
    assert_eq!(watermark.last_success_at, crate::isotime::iso_millis(EPOCH + 30_000));
    assert_eq!(watermark.run_count, 2);
}

#[test]
fn the_watermark_row_is_plain_json_an_external_reader_can_parse_without_our_types() {
    // Requirement 1: an EXTERNAL process reads the watermark WITHOUT chiefd's
    // loop. It opens the company DB read-only and parses the `documents` row.
    // This asserts the byte shape that read path depends on: the row under the
    // store key is self-describing JSON carrying the duty and its lastSuccessAt.
    let mut l = ledgers();
    record_success(&mut l, &ctx(), Duty::MailboxWake, EPOCH);

    let body =
        l.document_body(SupervisorWatermarkStore::NAME).expect("a row exists under the store key");
    let raw: serde_json::Value = serde_json::from_str(body).expect("plain JSON");
    assert_eq!(raw["organization"], "cobalt");
    assert_eq!(raw["duties"]["mailbox_wake"]["lastSuccessAt"], crate::isotime::iso_millis(EPOCH));
    assert_eq!(raw["duties"]["mailbox_wake"]["duty"], "mailbox_wake");
}

// --- the self-audit ---------------------------------------------------------

#[test]
fn a_recently_run_duty_is_not_stalled() {
    let mut l = ledgers();
    record_success(&mut l, &ctx(), Duty::MailboxWake, EPOCH);
    let (state, _) = read(&l, &ctx()).into_parts();

    // One interval later: well inside the grace multiple.
    let candidates = self_audit(&state, EPOCH + Duty::MailboxWake.interval_ms());
    assert!(candidates.is_empty(), "a duty inside its cadence is healthy");
}

#[test]
fn a_never_recorded_duty_is_never_raised() {
    // A brand-new company that has simply not run a duty yet must not page for
    // it — silence with no prior success is not an outage.
    let l = ledgers();
    let (state, _) = read(&l, &ctx()).into_parts();
    let candidates = self_audit(&state, EPOCH + 1000 * HOUR_MS);
    assert!(candidates.is_empty());
}

#[test]
fn the_41_hour_duty_blackout_raises_one_incident_carrying_the_retroactive_backlog() {
    // The motivating case: the duty last succeeded, then went silent for 41
    // hours. The self-audit must raise it — and report the WHOLE window, not
    // just "stale now".
    let mut l = ledgers();
    record_success(&mut l, &ctx(), Duty::MailboxWake, EPOCH);
    let (state, _) = read(&l, &ctx()).into_parts();

    let now = EPOCH + 41 * HOUR_MS;
    let candidates = self_audit(&state, now);

    assert_eq!(candidates.len(), 1, "one incident per stalled duty, not per window");
    let candidate = &candidates[0];
    assert_eq!(candidate.kind, SUPERVISOR_DUTY_STALLED_KIND);
    // The retroactive backlog: 41h / 30s windows missed, reported as a count.
    let expected_missed = (41 * HOUR_MS) / Duty::MailboxWake.interval_ms();
    assert_eq!(
        candidate.observed_count,
        Some(u64::try_from(expected_missed).unwrap()),
        "the incident reports how many cadence windows were missed across the outage"
    );
    // The window start is when it last succeeded — the incident spans the gap.
    assert_eq!(candidate.oldest_at.as_deref(), Some(crate::isotime::iso_millis(EPOCH).as_str()));
    assert!(candidate.detail.contains("mailbox_wake"));
}

#[test]
fn a_duty_stale_by_more_than_the_multiple_but_others_healthy_raises_only_the_stale_one() {
    let mut l = ledgers();
    record_success(&mut l, &ctx(), Duty::MailboxWake, EPOCH);
    record_success(&mut l, &ctx(), Duty::HealthMonitor, EPOCH);
    let (state, _) = read(&l, &ctx()).into_parts();

    // Advance just past mailbox-wake's 3-window threshold (3 * 30s = 90s) but
    // well inside health's (3 * 5min = 15min).
    let now = EPOCH + 4 * Duty::MailboxWake.interval_ms(); // 120s
    let candidates = self_audit(&state, now);

    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].detail.contains("mailbox_wake"));
}

// --- the startup self-audit into the health store ---------------------------

#[test]
fn the_startup_self_audit_raises_a_health_incident_and_it_resolves_on_recovery() {
    let mut l = ledgers();
    record_success(&mut l, &ctx(), Duty::MailboxWake, EPOCH);

    // Startup after a 41-hour outage: the retroactive incident is raised into
    // the health store on the FIRST pass (the kind is not confirmation-gated).
    let outage_now = EPOCH + 41 * HOUR_MS;
    let outcome = run_startup_self_audit(&mut l, &ctx(), outage_now);
    assert_eq!(outcome.new_incidents.len(), 1);
    assert_eq!(outcome.new_incidents[0].kind, SUPERVISOR_DUTY_STALLED_KIND);
    // It is durable in the health store an external reader already watches.
    let (health_state, _) = crate::store::health::read(&l, &ctx()).into_parts();
    assert_eq!(health_state.incidents.len(), 1);

    // Recovery: the duty runs again, then a fresh audit at that instant clears
    // the incident (it is no longer stale, so it resolves like any other).
    record_success(&mut l, &ctx(), Duty::MailboxWake, outage_now);
    let recovered = run_startup_self_audit(&mut l, &ctx(), outage_now);
    assert!(recovered.new_incidents.is_empty());
    assert_eq!(recovered.resolved_fingerprints.len(), 1, "the outage incident resolves");
    let (health_after, _) = crate::store::health::read(&l, &ctx()).into_parts();
    assert!(health_after.incidents.is_empty());
}

#[test]
fn clear_removes_the_watermark() {
    let mut l = ledgers();
    record_success(&mut l, &ctx(), Duty::MailboxWake, EPOCH);
    assert!(clear(&mut l), "a present row is removed");
    let (state, _) = read(&l, &ctx()).into_parts();
    assert!(state.duties.is_empty());
}
