//! Folding the operator-escalation queue into the supervision ledger.
//!
//! A foreground Pi publishes an operator escalation as a tiny durable row and
//! returns immediately. This module is the other half: the reconcile cycle
//! folds every valid queued item into the ledger and drops it from the queue.
//!
//! ## What this module deletes
//!
//! The TypeScript predecessor (`drainOrganizationOperatorEscalationIntents`)
//! carried a single-flight drain lock, and when that lock was retired it was
//! replaced by a client-side CAS loop with a sleep ladder — a poll wearing a
//! different hat. Both are gone. The drain here is one pass on chiefd's writer
//! thread inside one transaction: the queue read, the ledger fold and the
//! queue delete commit together or not at all, so there is no window in which
//! an item is committed but still queued (or dequeued but not committed), and
//! therefore nothing to reconcile afterwards.
//!
//! Idempotency still does the work a lock used to: an escalation is keyed on
//! its fingerprint, so a replay changes nothing.

use rusqlite::Transaction;

use crate::store::operator_escalation::{
    doorbell_text, enqueue_doorbell, record_from_parts, record_once,
};
use crate::store::operator_escalation_intents_rows::OperatorEscalationIntent;
use crate::store::organization::{EmploymentState, OrganizationManifest};
use crate::ChiefdError;

/// What one operator-escalation drain pass recorded and dropped.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OperatorEscalationDrainReport {
    /// Fingerprints written to the durable log this pass (or already there).
    pub recorded_fingerprints: Vec<String>,
    /// Fingerprints dropped: an author that is no longer the structural root,
    /// or a payload that failed validation.
    pub rejected_fingerprints: Vec<String>,
    /// Whether this pass put a doorbell in the pending slot.
    pub doorbell_armed: bool,
}

/// A person with no manager to escalate to — the one node structurally
/// guaranteed to still be running, and the only one allowed to reach the human
/// operator directly.
#[must_use]
pub fn is_structural_root(manifest: &OrganizationManifest, person_id: &str) -> bool {
    if manifest.person(person_id).is_none() {
        return false;
    }
    // `manager_of` resolves the walk up the department tree. A person whose
    // manager resolves to nobody — or to themselves — is the structural root.
    match manifest.manager_of(person_id) {
        None => true,
        Some(manager) => manager == person_id,
    }
}

// --- operator escalation intents ----------------------------------------

/// Every operator-escalation intent currently queued for this company.
pub fn queued_operator_escalations(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<Vec<OperatorEscalationIntent>, ChiefdError> {
    Ok(crate::store::operator_escalation_intents_rows::reconstruct(tx, row_slug, company_slug)?
        .intents
        .into_values()
        .collect())
}

/// Record every valid queued escalation in the durable log, arm the doorbell
/// for genuinely new content, and drop the queue rows — one transaction.
///
/// The log write is always first and is never gated on the doorbell: a distinct
/// blocker is durably recorded even when every notification path is down.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure, or a publish refusal.
pub fn drain_operator_escalations(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    manifest: &OrganizationManifest,
    at: &str,
) -> Result<OperatorEscalationDrainReport, ChiefdError> {
    let mut report = OperatorEscalationDrainReport::default();
    let queued = queued_operator_escalations(tx, row_slug, company_slug)?;
    if queued.is_empty() {
        return Ok(report);
    }
    let mut newest_doorbell: Option<(String, String)> = None;
    for intent in &queued {
        let active = manifest
            .person(&intent.person_id)
            .is_some_and(|person| person.employment_state == EmploymentState::Active);
        if !active || !is_structural_root(manifest, &intent.person_id) {
            report.rejected_fingerprints.push(intent.fingerprint.clone());
            continue;
        }
        let record = match record_from_parts(
            &intent.person_id,
            &intent.blocker,
            &intent.operator_action,
            &intent.queued_at,
            at,
        ) {
            Ok(record) if record.fingerprint == intent.fingerprint => record,
            _ => {
                report.rejected_fingerprints.push(intent.fingerprint.clone());
                continue;
            }
        };
        let wrote = record_once(tx, row_slug, &record)?;
        report.recorded_fingerprints.push(record.fingerprint.clone());
        if wrote {
            newest_doorbell =
                Some((doorbell_text(company_slug, &record), record.fingerprint.clone()));
        }
    }
    if let Some((text, fingerprint)) = newest_doorbell {
        enqueue_doorbell(tx, row_slug, company_slug, text, fingerprint)?;
        report.doorbell_armed = true;
    }
    let processed: Vec<String> = report
        .recorded_fingerprints
        .iter()
        .chain(report.rejected_fingerprints.iter())
        .cloned()
        .collect();
    if !processed.is_empty() {
        {
            let mut queue = crate::store::operator_escalation_intents_rows::reconstruct(
                tx,
                row_slug,
                company_slug,
            )?;
            for fingerprint in &processed {
                queue.intents.remove(fingerprint);
            }
            crate::store::operator_escalation_intents_rows::publish(
                tx,
                row_slug,
                company_slug,
                &queue,
            )?;
        }
    }
    Ok(report)
}
