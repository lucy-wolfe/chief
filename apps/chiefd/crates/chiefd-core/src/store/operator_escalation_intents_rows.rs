//! The `operator-escalation-intents` ROW implementation (org-data-normalization
//! P0, N2 copy-pattern, B4 singleton sweep).
//!
//! The company operator-escalation queue: one `org_documents` row per company
//! holding a `fingerprint → OperatorEscalationIntent` map → one
//! `operator_escalation_intents` child row per intent, keyed by fingerprint (the
//! blob's map key + enqueue idempotency key). An absent row set == an empty
//! queue. DERIVED per intent, never stored: `schemaVersion` = const `1`;
//! `organization` = company slug; `fingerprint` = the map key.
//!
//! Item D: publish REJECTS any serde-flatten `extra` with [`UNMODELED_KEYS`].

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::corrupt_store;
use crate::error::Refusal;
use crate::store::organization_rows::{RowsSqlError, UNMODELED_KEYS};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::store::shadow_report::{Disposition, ShadowReport};
use crate::ChiefdError;

/// One pending operator-escalation intent. Mirrors the TS
/// `OperatorEscalationIntent`; `schemaVersion`/`organization`/`fingerprint` are
/// DERIVED on reconstruct (fingerprint == the map key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorEscalationIntent {
    /// Always `1`. Not stored.
    pub schema_version: u32,
    /// The fingerprint — DERIVED from the map key, not a separate column.
    pub fingerprint: String,
    /// The company slug — DERIVED, not stored.
    pub organization: String,
    /// The structural root person raising the blocker.
    pub person_id: String,
    /// The blocker prose (opaque VALUE).
    pub blocker: String,
    /// The requested operator action (opaque VALUE).
    pub operator_action: String,
    /// When the intent was enqueued.
    pub queued_at: String,
    /// Any unmodeled per-intent key (item D).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The whole company queue. Mirrors the TS `OperatorEscalationIntentsDocument`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorEscalationIntents {
    /// Always `1`. Not stored.
    pub schema_version: u32,
    /// Pending intents keyed by fingerprint.
    pub intents: BTreeMap<String, OperatorEscalationIntent>,
    /// Any unmodeled top-level key (item D).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The `org_documents` store family this row set replaces.
pub const OPERATOR_ESCALATION_INTENTS_STORE: &str = "operator-escalation-intents";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("operator-escalation-intents-rows", e)
}

/// The semantic result of attempting to insert one operator escalation intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOperatorEscalationOutcome {
    /// The fingerprint was absent and is now durable.
    Inserted {
        /// Immutable audit cursor after the insert.
        seq: i64,
    },
    /// The same fingerprint and stored payload were already durable.
    Duplicate {
        /// Current immutable audit cursor; a duplicate does not advance it.
        seq: i64,
    },
}

/// Whether this fingerprint is already durable — the idempotency question, and
/// the only one [`insert_if_absent`] asks.
///
/// It used to reconstruct the whole intent and the caller threw every field
/// away. That reconstruction stamped `organization` from the ROW KEY, which is
/// a directory hash and never a company's name, so the one caller's `.is_some()`
/// was the only thing keeping a wrong value out of the world. Answering the
/// asked question removes the field, and with it the chance of the wrong answer.
fn intent_exists(
    tx: &Transaction<'_>,
    row_slug: &str,
    fingerprint: &str,
) -> Result<bool, ChiefdError> {
    tx.query_row(
        "SELECT 1 FROM operator_escalation_intents WHERE slug = ?1 AND fingerprint = ?2",
        params![row_slug, fingerprint],
        |_| Ok(()),
    )
    .optional()
    .map_err(store_failure)
    .map(|found| found.is_some())
}

/// Insert one operator-escalation intent when its fingerprint is absent.
///
/// This direct operation does not replace the queue. The fingerprint is the
/// complete idempotency identity, so every replay of that fingerprint is a
/// duplicate and the first durable row remains authoritative.
///
/// # Errors
/// [`UNMODELED_KEYS`] refusal (422) or [`ChiefdError::StoreFailure`] on SQL failure.
pub fn insert_if_absent(
    tx: &Transaction<'_>,
    row_slug: &str,
    intent: &OperatorEscalationIntent,
) -> Result<InsertOperatorEscalationOutcome, ChiefdError> {
    let mut intents = BTreeMap::new();
    intents.insert(intent.fingerprint.clone(), intent.clone());
    reject_unmodeled_keys(&OperatorEscalationIntents {
        schema_version: 1,
        intents,
        extra: BTreeMap::new(),
    })?;

    if intent_exists(tx, row_slug, &intent.fingerprint)? {
        let seq = crate::store::rows_txn::current_seq(tx, row_slug).map_err(store_failure)?;
        return Ok(InsertOperatorEscalationOutcome::Duplicate { seq });
    }

    let fingerprint = intent.fingerprint.clone();
    let person_id = intent.person_id.clone();
    let blocker = intent.blocker.clone();
    let operator_action = intent.operator_action.clone();
    let queued_at = intent.queued_at.clone();
    let seq = apply_and_emit::<RowsSqlError, _>(tx, row_slug, &queued_at, "", |tx| {
        tx.execute(
            "INSERT INTO operator_escalation_intents(slug, fingerprint, person_id, blocker, \
             operator_action, queued_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![row_slug, fingerprint, person_id, blocker, operator_action, queued_at],
        )?;
        Ok(vec![EventTouch::new(
            "operator-escalation-intent",
            fingerprint,
            "insert",
            "operator_escalation_intents",
            row_slug,
        )])
    })
    .map_err(|RowsSqlError(e)| e)?;
    Ok(InsertOperatorEscalationOutcome::Inserted { seq })
}

/// Reconstruct the queue for `company_slug` (empty-map when no rows; never
/// `None`, so an empty queue is representable).
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<OperatorEscalationIntents, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT fingerprint, person_id, blocker, operator_action, queued_at \
             FROM operator_escalation_intents WHERE slug = ?1 ORDER BY fingerprint",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![row_slug], |r| {
            let fingerprint: String = r.get(0)?;
            Ok((
                fingerprint.clone(),
                OperatorEscalationIntent {
                    schema_version: 1,
                    fingerprint,
                    organization: company_slug.to_string(),
                    person_id: r.get(1)?,
                    blocker: r.get(2)?,
                    operator_action: r.get(3)?,
                    queued_at: r.get(4)?,
                    extra: BTreeMap::new(),
                },
            ))
        })
        .map_err(store_failure)?;
    let mut intents = BTreeMap::new();
    for row in rows {
        let (fp, intent) = row.map_err(store_failure)?;
        intents.insert(fp, intent);
    }
    Ok(OperatorEscalationIntents { schema_version: 1, intents, extra: BTreeMap::new() })
}

/// Publish the queue atomically from current SQLite state. Deletes intents no
/// longer present, upserts new/changed ones, and returns the immutable audit
/// sequence; one `org_events` touch per changed intent.
///
/// # Errors
/// [`UNMODELED_KEYS`] refusal (422); SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    incoming: &OperatorEscalationIntents,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    let current = reconstruct(tx, row_slug, company_slug)?;

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, "", "", |tx| {
        let mut touches = Vec::new();
        for fp in current.intents.keys() {
            if !incoming.intents.contains_key(fp) {
                tx.execute(
                    "DELETE FROM operator_escalation_intents WHERE slug = ?1 AND fingerprint = ?2",
                    params![row_slug, fp],
                )?;
                touches.push(EventTouch::new(
                    "operator-escalation-intent",
                    fp,
                    "delete",
                    "operator_escalation_intents",
                    row_slug,
                ));
            }
        }
        for (fp, i) in &incoming.intents {
            let unchanged = current
                .intents
                .get(fp)
                .map(|c| {
                    c.person_id == i.person_id
                        && c.blocker == i.blocker
                        && c.operator_action == i.operator_action
                        && c.queued_at == i.queued_at
                })
                .unwrap_or(false);
            if unchanged {
                continue;
            }
            tx.execute(
                "INSERT INTO operator_escalation_intents(slug, fingerprint, person_id, \
                 blocker, operator_action, queued_at) VALUES(?1,?2,?3,?4,?5,?6) \
                 ON CONFLICT(slug, fingerprint) DO UPDATE SET person_id=?3, \
                 blocker=?4, operator_action=?5, queued_at=?6",
                params![row_slug, fp, i.person_id, i.blocker, i.operator_action, i.queued_at],
            )?;
            touches.push(EventTouch::new(
                "operator-escalation-intent",
                fp,
                "upsert",
                "operator_escalation_intents",
                row_slug,
            ));
        }
        Ok(touches)
    })
    .map_err(|RowsSqlError(e)| e)
}

fn reject_unmodeled_keys(doc: &OperatorEscalationIntents) -> Result<(), ChiefdError> {
    let mut paths: Vec<String> = doc.extra.keys().map(|k| format!("extra.{k}")).collect();
    for (fp, i) in &doc.intents {
        for k in i.extra.keys() {
            paths.push(format!("intents.{fp}.extra.{k}"));
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!(
            "operator-escalation-intents carries unmodeled keys the row model cannot store: {}",
            paths.join(", ")
        ),
    )))
}

/// Backfill the blob into the rows via the live publish path.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes; the publish's
/// [`UNMODELED_KEYS`] refusal passes through.
pub fn backfill_operator_escalation_intents(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let doc: OperatorEscalationIntents = serde_json::from_slice(blob)
        .map_err(|e| corrupt_store("operator-escalation-intents-blob", e))?;
    publish(tx, row_slug, company_slug, &doc)
}

/// The `operator-escalation-intents` zero-loss verifier. Signature mirrors
/// `migration::shadow_diff_manifest`.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes, [`ChiefdError::StoreFailure`] on a SQL
/// failure; an unmodeled
/// key is recorded loud, not an error.
pub fn shadow_diff_operator_escalation_intents(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<ShadowReport, ChiefdError> {
    let mut report = ShadowReport::new(OPERATOR_ESCALATION_INTENTS_STORE);
    let original: OperatorEscalationIntents = serde_json::from_slice(blob)
        .map_err(|e| corrupt_store("operator-escalation-intents-blob", e))?;
    if let Err(e) = backfill_operator_escalation_intents(tx, row_slug, company_slug, blob) {
        if e.code() == Some(UNMODELED_KEYS) {
            report.record_loud(format!("UNMODELED KEYS rejected by publish: {e}"));
            return Ok(report);
        }
        return Err(e);
    }
    let recon = reconstruct(tx, row_slug, company_slug)?;
    report.row_count = recon.intents.len();
    report.record("schemaVersion", Disposition::Derived { proof: "constant 1".into() });
    for (fp, o) in &original.intents {
        let path = format!("intents.{fp}");
        match recon.intents.get(fp) {
            Some(n)
                if n.person_id == o.person_id
                    && n.blocker == o.blocker
                    && n.operator_action == o.operator_action
                    && n.queued_at == o.queued_at =>
            {
                report.record(path, Disposition::Matched);
            }
            Some(_) => report.record(
                path,
                Disposition::Lost { blob_value: "intent fields differ after round-trip".into() },
            ),
            None => report.record(
                path,
                Disposition::Lost { blob_value: "intent absent after round-trip".into() },
            ),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::rows_txn::current_seq;
    use rusqlite::Connection;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("schema");
        conn
    }

    fn intent(fp: &str) -> OperatorEscalationIntent {
        OperatorEscalationIntent {
            schema_version: 1,
            fingerprint: fp.into(),
            organization: "acme".into(),
            person_id: "chief".into(),
            blocker: "blocked on tools".into(),
            operator_action: "add bash".into(),
            queued_at: "2026-07-25T01:38:03.432Z".into(),
            extra: BTreeMap::new(),
        }
    }

    fn queue(items: &[OperatorEscalationIntent]) -> OperatorEscalationIntents {
        let mut intents = BTreeMap::new();
        for i in items {
            intents.insert(i.fingerprint.clone(), i.clone());
        }
        OperatorEscalationIntents { schema_version: 1, intents, extra: BTreeMap::new() }
    }

    #[test]
    fn round_trips_every_intent_field() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let q = queue(&[intent("fp1"), intent("fp2")]);
        publish(&tx, "acme", "acme", &q).unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap(), q);
    }

    #[test]
    fn draining_deletes_the_intent_row() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &queue(&[intent("fp1"), intent("fp2")])).unwrap();
        let seq = publish(&tx, "acme", "acme", &queue(&[intent("fp2")])).unwrap();
        assert_eq!(seq, 3);
        let got = reconstruct(&tx, "acme", "acme").unwrap();
        assert_eq!(got.intents.len(), 1);
        assert!(got.intents.contains_key("fp2"));
    }

    #[test]
    fn semantic_insert_is_idempotent_and_never_replaces_a_conflict() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let first = intent("fp-1");
        assert_eq!(
            insert_if_absent(&tx, "acme", &first).unwrap(),
            InsertOperatorEscalationOutcome::Inserted { seq: 1 }
        );
        assert_eq!(
            insert_if_absent(&tx, "acme", &intent("fp-2")).unwrap(),
            InsertOperatorEscalationOutcome::Inserted { seq: 2 }
        );
        assert_eq!(
            insert_if_absent(&tx, "acme", &first).unwrap(),
            InsertOperatorEscalationOutcome::Duplicate { seq: 2 }
        );
        let mut fresh_retry = first.clone();
        fresh_retry.queued_at = "2026-07-25T01:39:03.432Z".into();
        fresh_retry.operator_action = "a different requested action".into();
        assert_eq!(
            insert_if_absent(&tx, "acme", &fresh_retry).unwrap(),
            InsertOperatorEscalationOutcome::Duplicate { seq: 2 }
        );
        let stored = reconstruct(&tx, "acme", "acme").unwrap();
        assert_eq!(stored.intents.len(), 2);
        assert_eq!(stored.intents["fp-1"].blocker, "blocked on tools");
        assert_eq!(stored.intents["fp-1"].operator_action, "add bash");
        assert_eq!(current_seq(&tx, "acme").unwrap(), 2);
    }

    #[test]
    fn rejects_unmodeled_keys() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut q = queue(&[intent("fp1")]);
        q.intents.get_mut("fp1").unwrap().extra.insert("weird".into(), serde_json::json!(1));
        assert_eq!(publish(&tx, "acme", "acme", &q).unwrap_err().code(), Some(UNMODELED_KEYS));
    }

    #[test]
    fn shadow_diff_zero_loss_on_live_blob() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let blob = br#"{"schemaVersion":1,"intents":{"dfb5cd44":{"schemaVersion":1,"fingerprint":"dfb5cd44","organization":"acme","personId":"chief","blocker":"b","operatorAction":"a","queuedAt":"2026-07-25T01:38:03.432Z"}}}"#;
        let report = shadow_diff_operator_escalation_intents(&tx, "acme", "acme", blob).unwrap();
        assert!(report.zero_loss(), "loud: {:?}", report.loud_failures());
        assert_eq!(report.row_count, 1);
    }
}
