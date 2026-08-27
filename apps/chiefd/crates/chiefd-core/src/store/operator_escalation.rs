//! The out-of-band operator escalation: recording it, and ringing the human.
//!
//! The structural root executive — the one person with no manager to escalate
//! to — raises a blocker only a human can clear. Two tiers, deliberately
//! orthogonal:
//!
//! * the **LOG** tier is a row in [`operator_escalation_log`], deduplicated by
//!   the escalation's own deterministic fingerprint. A distinct blocker is
//!   recorded exactly once and forever; an identical blocker re-raised against
//!   the same subject records nothing. It is written first and is never gated
//!   on the notification succeeding.
//! * the **PUSH** tier is the human doorbell: one org-wide pending slot plus a
//!   cooldown, so a burst of escalations logs in full but pings once.
//!
//! ## What this module deletes
//!
//! The TypeScript predecessor (`org-operator-escalation.ts`,
//! `org-operator-escalation-notify.ts`) appended `logs/operator-escalations.jsonl`
//! behind an `appendOrganizationJournalEventOnce` marker. That was two writes to
//! two stores for one fact, and the file half violated Mandate 5. Here the
//! fingerprint is the primary key of one row, so the "did this already land"
//! question and the durable record are the same object and the same commit —
//! there is no marker to get out of step with the log, and nothing on disk.
//!
//! Delivery itself is network I/O and does not belong on the writer thread. The
//! split is: [`plan_doorbell`] decides (a pure function of the durable push
//! state and the clock), the caller performs the send, and [`settle_doorbell`]
//! commits the outcome. No sleeping, no retry loop, no lock.

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::Refusal;
use crate::store::operator_escalation_push_rows::{OperatorEscalationPush, PendingDoorbell};
use crate::store::organization_rows::RowsSqlError;
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::ChiefdError;

/// The stable escalation kind for a manager-less root executive's own blocker.
pub const ROOT_EXECUTIVE_ESCALATION_KIND: &str = "root_executive_blocked";

/// Longest accepted blocker prose. Beyond this the caller is dumping context
/// into a doorbell, which is a summons rather than a place to write a report.
pub const OPERATOR_ESCALATION_BLOCKER_MAX: usize = 600;

/// Longest accepted operator-action prose.
pub const OPERATOR_ESCALATION_ACTION_MAX: usize = 300;

/// One human doorbell per hour at most — a burst logs in full but pings once.
pub const OPERATOR_ESCALATION_PUSH_COOLDOWN_MS: i64 = 60 * 60 * 1_000;

/// A doorbell that failed to deliver is retried on the very next pass, then
/// given up: the durable log row remains the authoritative record either way.
pub const OPERATOR_ESCALATION_PUSH_MAX_ATTEMPTS: i64 = 2;

/// The escalation payload failed validation.
pub const INVALID_ESCALATION: &str = "invalid-operator-escalation";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("operator-escalation-log-rows", e)
}

fn invalid(detail: impl Into<String>) -> ChiefdError {
    ChiefdError::from(Refusal::new(INVALID_ESCALATION, detail))
}

/// Collapse insignificant whitespace and case, so a re-typed identical blocker
/// fingerprints identically while a genuinely different one does not.
#[must_use]
pub fn normalize_blocker(blocker: &str) -> String {
    blocker.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The dedup key. The subject is the raising person, so the same blocker
/// re-raised by the same person never duplicates.
///
/// Byte-compatible with the fingerprint the Pi extension computes
/// (`packages/piing/extensions/organization-intercom.ts`), which cannot import
/// this crate: `sha256("<kind>\0<subject>\0<normalized blocker>")`, first 24
/// hex characters.
#[must_use]
pub fn fingerprint(person_id: &str, blocker: &str) -> String {
    let subject = format!("person:{person_id}");
    let canonical = format!(
        "{ROOT_EXECUTIVE_ESCALATION_KIND}\u{0}{subject}\u{0}{}",
        normalize_blocker(blocker)
    );
    let digest = Sha256::digest(canonical.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    hex.chars().take(24).collect()
}

/// One escalation as the log records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorEscalationRecord {
    /// The deterministic dedup key from [`fingerprint`].
    pub fingerprint: String,
    /// The escalation kind. Always [`ROOT_EXECUTIVE_ESCALATION_KIND`] today.
    pub kind: String,
    /// The structural root person raising the blocker.
    pub person_id: String,
    /// The blocker prose.
    pub blocker: String,
    /// What the human is being asked to do.
    pub operator_action: String,
    /// When the raising Pi queued it.
    pub queued_at: String,
    /// When the drain committed it to the log.
    pub recorded_at: String,
}

/// Validate the caller-supplied halves of an escalation and derive the rest.
///
/// The fingerprint is always recomputed from the fields, never trusted from the
/// wire: a tampered or stale key would otherwise dedup against the wrong
/// subject and silently swallow a distinct blocker.
///
/// # Errors
/// [`INVALID_ESCALATION`] when a required field is empty or a bounded field is
/// over length.
pub fn record_from_parts(
    person_id: &str,
    blocker: &str,
    operator_action: &str,
    queued_at: &str,
    recorded_at: &str,
) -> Result<OperatorEscalationRecord, ChiefdError> {
    let person_id = person_id.trim();
    let blocker = blocker.trim();
    let operator_action = operator_action.trim();
    if person_id.is_empty() {
        return Err(invalid("operator escalation personId is required"));
    }
    if blocker.is_empty() {
        return Err(invalid("operator escalation blocker is required"));
    }
    if operator_action.is_empty() {
        return Err(invalid("operator escalation operatorAction is required"));
    }
    if blocker.chars().count() > OPERATOR_ESCALATION_BLOCKER_MAX {
        return Err(invalid("operator escalation blocker is too long"));
    }
    if operator_action.chars().count() > OPERATOR_ESCALATION_ACTION_MAX {
        return Err(invalid("operator escalation action is too long"));
    }
    if crate::isotime::parse_iso_millis(queued_at).is_none() {
        return Err(invalid("operator escalation queuedAt is not a timestamp"));
    }
    Ok(OperatorEscalationRecord {
        fingerprint: fingerprint(person_id, blocker),
        kind: ROOT_EXECUTIVE_ESCALATION_KIND.to_string(),
        person_id: person_id.to_string(),
        blocker: blocker.to_string(),
        operator_action: operator_action.to_string(),
        queued_at: queued_at.to_string(),
        recorded_at: recorded_at.to_string(),
    })
}

/// The doorbell text for one recorded escalation.
#[must_use]
pub fn doorbell_text(slug: &str, record: &OperatorEscalationRecord) -> String {
    format!(
        "Operator attention needed in {slug}: {}\n\nBlocker: {}",
        record.operator_action, record.blocker
    )
}

/// Append one escalation to the durable log, exactly once.
///
/// Returns whether THIS call wrote a new row, so the caller rings the human
/// only for genuinely new content. A replay of the same fingerprint returns
/// `false` and touches nothing.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn record_once(
    tx: &Transaction<'_>,
    row_slug: &str,
    record: &OperatorEscalationRecord,
) -> Result<bool, ChiefdError> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT fingerprint FROM operator_escalation_log WHERE slug = ?1 AND fingerprint = ?2",
            params![row_slug, record.fingerprint],
            |r| r.get(0),
        )
        .optional()
        .map_err(store_failure)?;
    if existing.is_some() {
        return Ok(false);
    }
    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &record.recorded_at, "", |tx| {
        tx.execute(
            "INSERT INTO operator_escalation_log(slug, fingerprint, kind, person_id, \
             blocker, operator_action, queued_at, recorded_at) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                row_slug,
                record.fingerprint,
                record.kind,
                record.person_id,
                record.blocker,
                record.operator_action,
                record.queued_at,
                record.recorded_at,
            ],
        )?;
        Ok(vec![EventTouch::new(
            "operator-escalation-log",
            &record.fingerprint,
            "insert",
            "operator_escalation_log",
            row_slug,
        )])
    })
    .map_err(|RowsSqlError(e)| e)?;
    Ok(true)
}

/// Read the whole log, oldest → newest by `recorded_at`.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn read_log(
    tx: &Transaction<'_>,
    row_slug: &str,
) -> Result<Vec<OperatorEscalationRecord>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT fingerprint, kind, person_id, blocker, operator_action, queued_at, \
             recorded_at FROM operator_escalation_log WHERE slug = ?1 \
             ORDER BY recorded_at ASC, fingerprint ASC",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![row_slug], |r| {
            Ok(OperatorEscalationRecord {
                fingerprint: r.get(0)?,
                kind: r.get(1)?,
                person_id: r.get(2)?,
                blocker: r.get(3)?,
                operator_action: r.get(4)?,
                queued_at: r.get(5)?,
                recorded_at: r.get(6)?,
            })
        })
        .map_err(store_failure)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(store_failure)?);
    }
    Ok(out)
}

/// Read the push singleton, or its empty shape when no row exists.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn read_push(
    tx: &Transaction<'_>,
    row_slug: &str,
) -> Result<OperatorEscalationPush, ChiefdError> {
    Ok(crate::store::operator_escalation_push_rows::reconstruct(tx, row_slug)?.unwrap_or(
        OperatorEscalationPush {
            schema_version: 1,
            last_pushed_at: None,
            pending: None,
            extra: std::collections::BTreeMap::new(),
        },
    ))
}

/// Put a doorbell in the single pending slot; the newest escalation wins it.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure, or a publish refusal.
pub fn enqueue_doorbell(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    text: String,
    fingerprint: String,
) -> Result<i64, ChiefdError> {
    let mut push = read_push(tx, row_slug)?;
    push.pending = Some(PendingDoorbell { text, fingerprint, attempts: 0 });
    crate::store::operator_escalation_push_rows::publish(tx, row_slug, company_slug, &push)
}

/// What the caller should do about the pending doorbell right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoorbellPlan {
    /// The slot is empty.
    NothingPending,
    /// The cooldown window is still open. The slot has been cleared: the log
    /// already holds the content, so a suppressed doorbell is a suppressed
    /// ping, never a lost escalation.
    SuppressedByCooldown,
    /// Send this text, then report back through [`settle_doorbell`].
    Ring {
        /// The message to deliver.
        text: String,
        /// The escalation it came from (diagnostics only).
        fingerprint: String,
        /// Delivery attempts already spent on this doorbell.
        attempts: i64,
    },
}

/// Decide what to do with the pending doorbell.
///
/// Pure: it reads the durable state and the clock and nothing else, so the
/// decision is reproducible and testable without a network.
#[must_use]
pub fn plan_doorbell(push: &OperatorEscalationPush, now_ms: i64) -> DoorbellPlan {
    let Some(pending) = &push.pending else {
        return DoorbellPlan::NothingPending;
    };
    let last = push.last_pushed_at.as_deref().and_then(crate::isotime::parse_iso_millis);
    if let Some(last) = last {
        if now_ms.saturating_sub(last) < OPERATOR_ESCALATION_PUSH_COOLDOWN_MS {
            return DoorbellPlan::SuppressedByCooldown;
        }
    }
    DoorbellPlan::Ring {
        text: pending.text.clone(),
        fingerprint: pending.fingerprint.clone(),
        attempts: pending.attempts,
    }
}

/// What the caller's delivery attempt produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorbellOutcome {
    /// At least one authorized recipient accepted the message.
    Delivered,
    /// Nobody accepted it. Retried once, then abandoned.
    NotDelivered,
    /// There was no reachable, authorized human to ring. The slot is dropped so
    /// a later, reachable escalation is not blocked behind this one.
    Skipped,
}

/// How the doorbell state moved after [`settle_doorbell`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorbellSettlement {
    /// Rang, cooldown restarted, slot cleared.
    Delivered,
    /// Kept in the slot for exactly one more attempt.
    Deferred,
    /// Slot cleared without ringing.
    Dropped,
}

/// Commit the outcome of one delivery attempt.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure, or a publish refusal.
pub fn settle_doorbell(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    outcome: DoorbellOutcome,
    now_ms: i64,
) -> Result<DoorbellSettlement, ChiefdError> {
    let mut push = read_push(tx, row_slug)?;
    let Some(pending) = push.pending.clone() else {
        return Ok(DoorbellSettlement::Dropped);
    };
    let settlement = match outcome {
        DoorbellOutcome::Delivered => {
            push.last_pushed_at = Some(crate::isotime::iso_millis(now_ms));
            push.pending = None;
            DoorbellSettlement::Delivered
        }
        DoorbellOutcome::Skipped => {
            push.pending = None;
            DoorbellSettlement::Dropped
        }
        DoorbellOutcome::NotDelivered => {
            let attempts = pending.attempts.saturating_add(1);
            if attempts >= OPERATOR_ESCALATION_PUSH_MAX_ATTEMPTS {
                push.pending = None;
                DoorbellSettlement::Dropped
            } else {
                push.pending = Some(PendingDoorbell { attempts, ..pending });
                DoorbellSettlement::Deferred
            }
        }
    };
    crate::store::operator_escalation_push_rows::publish(tx, row_slug, company_slug, &push)?;
    Ok(settlement)
}

/// Clear the cooldown-suppressed slot without ringing.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure, or a publish refusal.
pub fn drop_pending_doorbell(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<(), ChiefdError> {
    let mut push = read_push(tx, row_slug)?;
    if push.pending.is_none() {
        return Ok(());
    }
    push.pending = None;
    crate::store::operator_escalation_push_rows::publish(tx, row_slug, company_slug, &push)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_with(last: Option<&str>, pending: Option<(&str, i64)>) -> OperatorEscalationPush {
        OperatorEscalationPush {
            schema_version: 1,
            last_pushed_at: last.map(ToString::to_string),
            pending: pending.map(|(text, attempts)| PendingDoorbell {
                text: text.to_string(),
                fingerprint: "fp".to_string(),
                attempts,
            }),
            extra: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn a_retyped_identical_blocker_fingerprints_identically() {
        let a = fingerprint("chief", "  The   BUILD is   red  ");
        let b = fingerprint("chief", "the build is red");
        assert_eq!(a, b);
    }

    #[test]
    fn the_person_is_the_subject_so_two_roots_raising_one_blocker_differ() {
        let a = fingerprint("chief", "blocked on legal");
        let b = fingerprint("founder", "blocked on legal");
        assert_ne!(a, b);
    }

    #[test]
    fn the_fingerprint_is_twenty_four_hex_characters() {
        let fp = fingerprint("chief", "x");
        assert_eq!(fp.len(), 24);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_record_recomputes_its_fingerprint_and_never_trusts_the_wire() {
        let record = record_from_parts(
            "chief",
            " Blocked ",
            " Call legal ",
            "2026-08-07T00:00:00.000Z",
            "2026-08-07T00:00:01.000Z",
        )
        .expect("valid");
        assert_eq!(record.fingerprint, fingerprint("chief", "Blocked"));
        assert_eq!(record.blocker, "Blocked");
        assert_eq!(record.operator_action, "Call legal");
        assert_eq!(record.kind, ROOT_EXECUTIVE_ESCALATION_KIND);
    }

    #[test]
    fn an_overlong_blocker_is_refused_rather_than_truncated() {
        let blocker = "x".repeat(OPERATOR_ESCALATION_BLOCKER_MAX + 1);
        let error = record_from_parts(
            "chief",
            &blocker,
            "act",
            "2026-08-07T00:00:00.000Z",
            "2026-08-07T00:00:00.000Z",
        )
        .expect_err("too long");
        assert_eq!(error.code(), Some(INVALID_ESCALATION));
    }

    #[test]
    fn an_unparseable_queued_at_is_refused() {
        let error = record_from_parts("chief", "b", "a", "not-a-time", "2026-08-07T00:00:00.000Z")
            .expect_err("bad timestamp");
        assert_eq!(error.code(), Some(INVALID_ESCALATION));
    }

    #[test]
    fn an_empty_slot_plans_nothing() {
        assert_eq!(plan_doorbell(&push_with(None, None), 0), DoorbellPlan::NothingPending);
    }

    #[test]
    fn a_never_rung_doorbell_rings_immediately() {
        let plan = plan_doorbell(&push_with(None, Some(("ring", 0))), 10);
        assert!(matches!(plan, DoorbellPlan::Ring { attempts: 0, .. }));
    }

    #[test]
    fn a_doorbell_inside_the_cooldown_is_suppressed() {
        let push = push_with(Some("2026-08-07T00:00:00.000Z"), Some(("ring", 0)));
        let now = crate::isotime::parse_iso_millis("2026-08-07T00:30:00.000Z").expect("parse");
        assert_eq!(plan_doorbell(&push, now), DoorbellPlan::SuppressedByCooldown);
    }

    #[test]
    fn a_doorbell_past_the_cooldown_rings_again() {
        let push = push_with(Some("2026-08-07T00:00:00.000Z"), Some(("ring", 0)));
        let now = crate::isotime::parse_iso_millis("2026-08-07T01:00:01.000Z").expect("parse");
        assert!(matches!(plan_doorbell(&push, now), DoorbellPlan::Ring { .. }));
    }

    #[test]
    fn an_unparseable_last_push_fails_open_and_rings() {
        let push = push_with(Some("garbage"), Some(("ring", 0)));
        assert!(matches!(plan_doorbell(&push, 10), DoorbellPlan::Ring { .. }));
    }

    #[test]
    fn the_doorbell_text_names_the_company_the_action_and_the_blocker() {
        let record = record_from_parts(
            "chief",
            "legal review is stuck",
            "approve the contract",
            "2026-08-07T00:00:00.000Z",
            "2026-08-07T00:00:00.000Z",
        )
        .expect("valid");
        let text = doorbell_text("cobalt", &record);
        assert!(text.contains("cobalt"));
        assert!(text.contains("approve the contract"));
        assert!(text.contains("legal review is stuck"));
    }
}
