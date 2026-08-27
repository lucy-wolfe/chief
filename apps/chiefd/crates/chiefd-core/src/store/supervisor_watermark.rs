//! The supervisor duty watermark and its startup self-audit — od-supervisor
//! gap-table duty #14, the fix for the 41-hour-blackout failure class.
//!
//! # The failure this exists to remove
//!
//! On 2026-07-19 a launcher-hosted poll duty died and the launcher's own
//! staleness detector did not fire for ~41 hours — because that detector was hosted
//! *inside the very supervisor process* whose stopped duty it should have
//! detected, and a dead process runs no detector. Same structural class as the
//! per-person session-maintenance detector that could never fire. A detector
//! may not be hosted solely by the thing it detects.
//!
//! # The contract this store provides
//!
//! 1. **An EXTERNAL liveness watermark.** Every launcher-hosted duty writes a
//!    durable `lastSuccessAt` per `(company, duty)` on each successful run. It
//!    is a plain document row in the company database, so an external process —
//!    another daemon, an operator's script, or a freshly-restarted chiefd —
//!    reads it through [`crate::store::open_company_db_readonly`] WITHOUT
//!    chiefd's cycle running. That external readability is the whole point: the
//!    answer to "is a duty alive?" cannot depend on the process whose liveness
//!    is in question.
//!
//! 2. **A startup self-audit that raises the RETROACTIVE backlog.** When chiefd
//!    starts (or any external auditor runs), [`self_audit`] compares each duty's
//!    `lastSuccessAt` against its own cadence and, for a duty that has been
//!    silent across many of its windows, raises ONE incident that reports the
//!    whole missed window — how long it was down and how many cadence windows it
//!    missed — not merely "it is stale right now". The old system swallowed the
//!    41-hour gap and reported nothing about the window itself; the self-audit
//!    is what makes an outage-while-down visible after recovery.
//!
//! # Storage choice, recorded
//!
//! This is a NEW store with no TypeScript counterpart (the self-detector is the
//! defect being fixed, so there is nothing to be byte-identical to). It lives in
//! chiefd-core's company database as a `documents` row under the key
//! [`SupervisorWatermarkStore::NAME`], `FailOpen` throughout (losing it degrades
//! observability, never safety — the same reasoning as `health`). It
//! is shaped as an ordinary document body so the Phase-B repoint that moves
//! every store onto the surviving `org_documents` document contract moves this
//! one unchanged; nothing about the shape assumes the current backing file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::isotime::{iso_millis, parse_iso_millis};
use crate::ledger::Ledgers;
use crate::polarity::{decode_fail_open, Decoded, FailOpen, StoreKind};
use crate::store::context::CompanyContext;
use crate::store::health::{self, CycleOutcome, IncidentCandidate, NeverResolves};

/// Schema version of the watermark document body.
pub const SUPERVISOR_WATERMARK_SCHEMA_VERSION: u32 = 1;

/// How many of a duty's own cadence windows may elapse with no success before
/// the self-audit raises it. Three: a single slow or skipped cycle is noise;
/// silence across three consecutive windows is a stalled duty.
pub const SUPERVISOR_DUTY_STALE_MULTIPLE: i64 = 3;

/// The incident kind the self-audit raises. Deliberately NOT one of
/// `health`'s confirmation-gated kinds: a startup self-audit runs once and must
/// raise on that first pass, not wait for a second sample fifteen seconds later
/// that a just-started process may never take.
pub const SUPERVISOR_DUTY_STALLED_KIND: &str = "supervisor_duty_stalled";

/// A launcher-hosted supervisor duty with a liveness cadence.
///
/// This is the **canonical** list of hosted duties — the single source of truth
/// od-live's observe mode enumerates and the self-audit iterates. Adding a duty
/// to chiefd's cycle means adding it here so its liveness is watched; a duty
/// with no watermark is a duty whose silence nobody would notice, which is the
/// exact bug this module removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Duty {
    /// The D9 reconcile / ownership probe.
    SupervisionReconcile,
    /// The passive health-monitor pass.
    HealthMonitor,
    /// The pending-mail wake scan.
    MailboxWake,
    /// The durable-reminder dispatch pass — fires every due reminder and
    /// re-arms it. Watched like every other duty: a reminder duty that silently
    /// stopped would look exactly like a company where nobody armed anything,
    /// which is the failure this whole module exists to make impossible.
    ReminderDispatch,
}

impl Duty {
    /// Every duty, in a stable order.
    pub const ALL: &'static [Duty] = &[
        Duty::SupervisionReconcile,
        Duty::HealthMonitor,
        Duty::MailboxWake,
        Duty::ReminderDispatch,
    ];

    /// The stable key this duty is stored and reported under.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SupervisionReconcile => "supervision_reconcile",
            Self::HealthMonitor => "health_monitor",
            Self::MailboxWake => "mailbox_wake",
            Self::ReminderDispatch => "reminder_dispatch",
        }
    }

    /// The cadence this duty is expected to run at, in milliseconds. Sourced
    /// from the TS supervisor's own constants where they exist:
    /// health is `ORGANIZATION_HEALTH_MONITOR_INTERVAL_MS` (5 min); the probe
    /// duties run on the ~30 s ownership-probe cadence
    /// (`ORGANIZATION_SUPERVISOR_WAKE_RETRY_MAX_MS`).
    #[must_use]
    pub const fn interval_ms(self) -> i64 {
        match self {
            // ReminderDispatch and HealthMonitor (E8-S0/E8-S2, #822/#824) both
            // declare the reactive floor, not a poll cadence: ReminderDispatch
            // sleeps until its earliest armed reminder
            // (`supervision::next_reminder_due_at`) and HealthMonitor sleeps
            // until its earliest armed staleness/confirmation deadline
            // (`health::next_confirmation_deadline`, `chiefd/src/run.rs`'s
            // `health_monitor_next_interval`), resting at the floor when
            // nothing is armed. This value is therefore the LIVENESS
            // EXPECTATION the startup self-audit measures silence against for
            // both, never a wake-up rate.
            Self::HealthMonitor | Self::ReminderDispatch => 5 * 60 * 1_000,
            Self::SupervisionReconcile | Self::MailboxWake => 30_000,
        }
    }
}

/// One duty's liveness watermark.
///
/// #825-prereq: `last_failure_*`/`consecutive_failures` are a BOUNDED
/// singleton, not a log — one row per duty carries at most the most recent
/// failure, a bounded singleton idiom rather than an append-only log. [`record_success`] clears them in the same write that
/// advances `last_success_at`, so "failing" never survives a subsequent
/// success, and writing any number of consecutive failures never grows the
/// row past these four fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DutyWatermark {
    /// The duty key ([`Duty::as_str`]).
    pub duty: String,
    /// The cadence the duty was expected to run at when last recorded.
    pub interval_ms: i64,
    /// ISO-8601 stamp of the last successful run. Empty when the duty has
    /// recorded a failure but has never yet succeeded (startup semantics).
    pub last_success_at: String,
    /// How many successful runs have been recorded.
    pub run_count: u64,
    /// ISO-8601 stamp of the most recent failure, or `None` when the duty has
    /// never failed or its last failure was cleared by a later success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<String>,
    /// A short, stable classification of the most recent failure (e.g.
    /// `"cycle_input_gather_failed"`, `"reconcile_refused"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_kind: Option<String>,
    /// The raw diagnostic for the most recent failure. Never pre-redacted —
    /// same contract as [`IncidentCandidate`]'s `detail`: the health fold is
    /// the one redaction site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_detail: Option<String>,
    /// How many failures have been recorded back-to-back since the last
    /// success. Reset to `0` on every success.
    #[serde(default)]
    pub consecutive_failures: u64,
}

impl DutyWatermark {
    /// Whether this duty's most recently recorded event was a failure that
    /// has not since been cleared by a success. This is the sole "failing"
    /// predicate the tri-state liveness reader consults.
    #[must_use]
    pub fn is_failing(&self) -> bool {
        self.last_failure_at.is_some()
    }
}

/// The watermark document body: one entry per duty that has ever run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorWatermarkState {
    /// Always [`SUPERVISOR_WATERMARK_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The company this watermark belongs to.
    pub organization: String,
    /// Watermarks by duty key. A duty absent here has never recorded a success,
    /// and the self-audit does not raise it — a brand-new company must not page
    /// for every duty it has simply not run yet.
    pub duties: BTreeMap<String, DutyWatermark>,
}

impl SupervisorWatermarkState {
    /// The empty watermark for a company with no recorded duty runs.
    #[must_use]
    pub fn empty(organization: impl Into<String>) -> Self {
        Self {
            schema_version: SUPERVISOR_WATERMARK_SCHEMA_VERSION,
            organization: organization.into(),
            duties: BTreeMap::new(),
        }
    }
}

/// The `org_documents` store key for the watermark row.
///
/// Exposed as a plain const — not only through [`SupervisorWatermarkStore`]'s
/// `StoreKind::NAME` — so the Phase-B `chiefctl` takeover writer can seed the row
/// WITHOUT naming the store type, which the M7 fence (`fence_containment`)
/// forbids outside this module. The key stays here; an off-module caller reaches
/// the row through the allowlisted legacy surface it must use anyway.
pub const SUPERVISOR_WATERMARK_STORE: &str = "supervisor-watermark";

/// The supervisor-watermark store.
pub struct SupervisorWatermarkStore;

impl StoreKind for SupervisorWatermarkStore {
    const NAME: &'static str = SUPERVISOR_WATERMARK_STORE;
    type Body = SupervisorWatermarkState;
}

impl FailOpen for SupervisorWatermarkStore {
    fn empty() -> Self::Body {
        // The organization is filled in by `read` from the caller's context, as
        // `health` does: a fail-open empty cannot invent a company name.
        SupervisorWatermarkState::empty(String::new())
    }
}

/// Read the watermark. Total: unreadable bytes are an empty watermark plus a
/// warning, so the self-audit still runs (it simply finds no duties to fault),
/// never refusing because it could not parse its own state.
#[must_use]
pub fn read(ledgers: &Ledgers, ctx: &CompanyContext) -> Decoded<SupervisorWatermarkState> {
    // Absence is not corruption: a company that has recorded no duty run simply
    // has no row. That is a clean empty — a self-audit reading it finds no
    // duties and faults none — not a fail-open reset worth warning about on
    // every fresh company's first pass.
    //
    // This read used to hand-roll `decode_fail_open` to say exactly that,
    // because the helper had one input and could not tell the two facts apart.
    // It can now: absence is answered here, bytes are judged there, and the
    // fork is gone.
    let Some(body) = ledgers.document_body(SupervisorWatermarkStore::NAME) else {
        // Absence value: the empty watermark — unchanged.
        return Decoded::absent(SupervisorWatermarkState::empty(ctx.slug()));
    };
    // Present but unreadable IS the fail-open case: reset to empty and warn, so
    // corruption never wedges the detector this store exists to keep running.
    // The decode error is the only evidence of WHAT was wrong with the bytes,
    // and the warning is where a reader looks.
    match decode_fail_open::<SupervisorWatermarkStore>(
        serde_json::from_str::<SupervisorWatermarkState>(body)
            .map_err(|error| format!("the body did not decode: {error}")),
    ) {
        // `FailOpen::empty()` cannot invent a company name, so the slug is
        // filled in from the caller's context here, as `health` does.
        Decoded::RecoveredEmpty { warning, .. } => {
            Decoded::RecoveredEmpty { body: SupervisorWatermarkState::empty(ctx.slug()), warning }
        }
        other => other,
    }
}

/// Persist the watermark. A serialization failure drops the write — the
/// fail-open answer, matching `health`.
fn write(ledgers: &mut Ledgers, state: &SupervisorWatermarkState) {
    if let Ok(encoded) = serde_json::to_string(state) {
        ledgers.put_document(SupervisorWatermarkStore::NAME, encoded);
    }
}

/// Drop the watermark entirely. Returns whether a row was present.
pub fn clear(ledgers: &mut Ledgers) -> bool {
    ledgers.remove_document(SupervisorWatermarkStore::NAME)
}

/// Record that `duty` ran successfully at `at_millis`.
///
/// Called from inside a duty's own commit, so the watermark advances in the
/// same transaction as the work it attests — a success is durable with, never
/// after, the thing it records. Upserts the duty's entry, bumps its run
/// count, and — #825-prereq — CLEARS any recorded failure: a success is the
/// only thing that clears `is_failing()`, so "failing" can never outlive the
/// success that resolved it.
pub fn record_success(ledgers: &mut Ledgers, ctx: &CompanyContext, duty: Duty, at_millis: i64) {
    let (mut state, _warning) = read(ledgers, ctx).into_parts();
    let at = iso_millis(at_millis);
    let entry = state.duties.entry(duty.as_str().to_string()).or_insert_with(|| DutyWatermark {
        duty: duty.as_str().to_string(),
        interval_ms: duty.interval_ms(),
        last_success_at: at.clone(),
        run_count: 0,
        last_failure_at: None,
        last_failure_kind: None,
        last_failure_detail: None,
        consecutive_failures: 0,
    });
    entry.duty = duty.as_str().to_string();
    entry.interval_ms = duty.interval_ms();
    entry.last_success_at = at;
    entry.run_count = entry.run_count.saturating_add(1);
    entry.last_failure_at = None;
    entry.last_failure_kind = None;
    entry.last_failure_detail = None;
    entry.consecutive_failures = 0;
    write(ledgers, &state);
}

/// Record that `duty` failed at `at_millis` with classification `kind` and
/// raw diagnostic `detail`.
///
/// #825-prereq: this is the chiefd-owned (Rust) failure producer the health
/// read path consumes — the counterpart to [`record_success`]. Upserts the
/// duty's entry (creating it with an EMPTY `last_success_at` when the duty
/// has never yet succeeded, so "failing before any success" and "failing
/// after a prior success" are both representable) and overwrites the bounded
/// last-failure fields — never appends, so the row never grows regardless of
/// how many consecutive failures are recorded. `run_count` and
/// `last_success_at` are left untouched: a failure never fabricates or erases
/// a prior success.
pub fn record_failure(
    ledgers: &mut Ledgers,
    ctx: &CompanyContext,
    duty: Duty,
    at_millis: i64,
    kind: &str,
    detail: &str,
) {
    let (mut state, _warning) = read(ledgers, ctx).into_parts();
    let at = iso_millis(at_millis);
    let entry = state.duties.entry(duty.as_str().to_string()).or_insert_with(|| DutyWatermark {
        duty: duty.as_str().to_string(),
        interval_ms: duty.interval_ms(),
        last_success_at: String::new(),
        run_count: 0,
        last_failure_at: None,
        last_failure_kind: None,
        last_failure_detail: None,
        consecutive_failures: 0,
    });
    entry.duty = duty.as_str().to_string();
    entry.interval_ms = duty.interval_ms();
    entry.last_failure_at = Some(at);
    entry.last_failure_kind = Some(kind.to_string());
    entry.last_failure_detail = Some(detail.to_string());
    entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
    write(ledgers, &state);
}

/// The pure self-audit: which duties are stalled, and by how much.
///
/// For each recorded duty, computes the gap from its last success to `now` and,
/// if that gap spans at least [`SUPERVISOR_DUTY_STALE_MULTIPLE`] of the duty's
/// own cadence windows, emits ONE [`IncidentCandidate`] carrying the RETROACTIVE
/// backlog: `observed_count` is the number of cadence windows missed across the
/// outage, `oldest_at` is when the duty last succeeded, and the detail names the
/// window. That is the distinction from "currently stale" — the incident reports
/// the whole silence, so an outage that ended before anyone looked is still
/// visible after recovery.
///
/// Pure and side-effect-free so an external auditor can call it over a
/// read-only snapshot; [`run_startup_self_audit`] is the wiring that folds the
/// result into the health store.
#[must_use]
pub fn self_audit(state: &SupervisorWatermarkState, now_millis: i64) -> Vec<IncidentCandidate> {
    let mut candidates = Vec::new();
    for duty in Duty::ALL {
        let Some(watermark) = state.duties.get(duty.as_str()) else {
            // Never recorded: nothing to be stale against. A fresh company does
            // not page for duties it simply has not run yet.
            continue;
        };
        let last = match parse_iso_millis(&watermark.last_success_at) {
            Some(last) => last,
            // An unparseable stamp is not evidence of an outage; skip rather
            // than fabricate a gap from a bad timestamp.
            None => continue,
        };
        let interval =
            if watermark.interval_ms > 0 { watermark.interval_ms } else { duty.interval_ms() };
        let gap = now_millis.saturating_sub(last);
        if gap < interval.saturating_mul(SUPERVISOR_DUTY_STALE_MULTIPLE) {
            continue;
        }
        let missed_windows = gap / interval;
        let mut candidate = IncidentCandidate::new(
            SUPERVISOR_DUTY_STALLED_KIND,
            format!(
                "supervisor duty '{}' last succeeded at {} — silent across {} of its {}ms windows",
                duty.as_str(),
                watermark.last_success_at,
                missed_windows,
                interval,
            ),
        );
        // The retroactive backlog magnitude, and the window it spans.
        candidate.observed_count = Some(u64::try_from(missed_windows).unwrap_or(u64::MAX));
        candidate.oldest_at = Some(watermark.last_success_at.clone());
        candidates.push(candidate);
    }
    candidates
}

/// The startup self-audit: run [`self_audit`] and fold its retroactive backlog
/// into the company's health incidents in one pass.
///
/// This is the operation a starting chiefd (or an external auditor with write
/// access) runs to make a while-down outage visible. It reads the watermark,
/// computes the stalled-duty candidates, and applies them to the health store
/// through `health`'s own typed cycle — so the incidents live and resolve
/// exactly like every other health incident, and clear on the next pass once
/// the duty records a fresh success. Returns what the health pass changed.
pub fn run_startup_self_audit(
    ledgers: &mut Ledgers,
    ctx: &CompanyContext,
    now_millis: i64,
) -> CycleOutcome {
    let (state, _warning) = read(ledgers, ctx).into_parts();
    let candidates = self_audit(&state, now_millis);
    let (mut health_state, _health_warning) = health::read(ledgers, ctx).into_parts();
    let outcome = health::apply_cycle(&mut health_state, &candidates, now_millis, &NeverResolves);
    health::write(ledgers, &health_state);
    outcome
}

#[cfg(test)]
mod tests;
