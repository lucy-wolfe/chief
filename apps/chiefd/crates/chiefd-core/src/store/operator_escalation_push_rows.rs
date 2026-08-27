//! The `operator-escalation-push` singleton ROW implementation (org-data-
//! normalization P0, N2 copy-pattern, B4 singleton sweep).
//!
//! Scalar singleton (`operator_escalation_push` table, `slug` PK). Holds the
//! human-doorbell state: `lastPushedAt?` plus at most one outstanding `pending`
//! doorbell. The pending sub-record is a COHERENT triple — the three
//! `pending_*` columns are all-set or all-NULL, mirroring the blob's present/
//! absent `pending` object. DERIVED: `schemaVersion` = const `1`.
//!
//! Item D: publish REJECTS any serde-flatten `extra` with [`UNMODELED_KEYS`].

use std::collections::BTreeMap;

use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::corrupt_store;
use crate::error::Refusal;
use crate::store::organization_rows::{RowsSqlError, UNMODELED_KEYS};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::store::shadow_report::{Disposition, ShadowReport};
use crate::ChiefdError;

/// The at-most-one outstanding doorbell (blob `pending`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDoorbell {
    /// The escalation text to push.
    pub text: String,
    /// The escalation fingerprint (dedup key).
    pub fingerprint: String,
    /// Push attempts so far.
    pub attempts: i64,
}

/// An `operator-escalation-push` singleton. Mirrors the TS
/// `OperatorEscalationPushDocument`; `schemaVersion` DERIVED.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorEscalationPush {
    /// Always `1`. Not stored.
    pub schema_version: u32,
    /// When the human doorbell last rang. Absent ⇒ never rung.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_pushed_at: Option<String>,
    /// At most one outstanding doorbell; the newest escalation wins the slot.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pending: Option<PendingDoorbell>,
    /// Any unmodeled key (item D).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The `org_documents` store family this row set replaces.
pub const OPERATOR_ESCALATION_PUSH_STORE: &str = "operator-escalation-push";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("operator-escalation-push-rows", e)
}

/// Reconstruct the push singleton for `company_slug`, or `None`.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure; [`ChiefdError::Corrupt`] on an
/// incoherent pending triple.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
) -> Result<Option<OperatorEscalationPush>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT last_pushed_at, pending_text, pending_fingerprint, pending_attempts \
             FROM operator_escalation_push WHERE slug = ?1",
        )
        .map_err(store_failure)?;
    let mut rows = stmt.query(params![row_slug]).map_err(store_failure)?;
    let Some(row) = rows.next().map_err(store_failure)? else {
        return Ok(None);
    };
    let last_pushed_at: Option<String> = row.get(0).map_err(store_failure)?;
    let text: Option<String> = row.get(1).map_err(store_failure)?;
    let fingerprint: Option<String> = row.get(2).map_err(store_failure)?;
    let attempts: Option<i64> = row.get(3).map_err(store_failure)?;
    let pending = match (text, fingerprint, attempts) {
        (Some(text), Some(fingerprint), Some(attempts)) => {
            Some(PendingDoorbell { text, fingerprint, attempts })
        }
        (None, None, None) => None,
        _ => {
            return Err(crate::error::corrupt_store_because(
                "operator-escalation-push-rows",
                "the stored pending-doorbell columns are partly present: a doorbell is \
                 text, fingerprint and attempts together or none of them",
            ));
        }
    };
    Ok(Some(OperatorEscalationPush {
        schema_version: 1,
        last_pushed_at,
        pending,
        extra: BTreeMap::new(),
    }))
}

/// Publish the singleton atomically from current SQLite state. One `org_events`
/// touch iff any stored field changed; its sequence is returned as immutable
/// audit evidence. `at` is `lastPushedAt` when present, else the pending
/// fingerprint is not a timestamp, so an empty stamp is used (the feed's `at` is
/// advisory; the row is truth).
///
/// # Errors
/// [`UNMODELED_KEYS`] refusal (422); SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    incoming: &OperatorEscalationPush,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    let current = reconstruct(tx, row_slug)?;
    let at = incoming.last_pushed_at.clone().unwrap_or_default();
    let (p_text, p_fp, p_attempts) = match &incoming.pending {
        Some(p) => (Some(p.text.clone()), Some(p.fingerprint.clone()), Some(p.attempts)),
        None => (None, None, None),
    };

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        let unchanged = current
            .as_ref()
            .map(|c| c.last_pushed_at == incoming.last_pushed_at && c.pending == incoming.pending)
            .unwrap_or(false);
        if unchanged {
            return Ok(Vec::new());
        }
        tx.execute(
            "INSERT INTO operator_escalation_push(slug, last_pushed_at, pending_text, \
             pending_fingerprint, pending_attempts) VALUES(?1,?2,?3,?4,?5) \
             ON CONFLICT(slug) DO UPDATE SET last_pushed_at=?2, pending_text=?3, \
             pending_fingerprint=?4, pending_attempts=?5",
            params![row_slug, incoming.last_pushed_at, p_text, p_fp, p_attempts],
        )?;
        Ok(vec![EventTouch::new(
            "operator-escalation-push",
            company_slug,
            "upsert",
            "operator_escalation_push",
            row_slug,
        )])
    })
    .map_err(|RowsSqlError(e)| e)
}

fn reject_unmodeled_keys(doc: &OperatorEscalationPush) -> Result<(), ChiefdError> {
    if doc.extra.is_empty() {
        return Ok(());
    }
    let mut paths: Vec<String> = doc.extra.keys().map(|k| format!("extra.{k}")).collect();
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!(
            "operator-escalation-push carries unmodeled keys the row model cannot store: {}",
            paths.join(", ")
        ),
    )))
}

/// Backfill the blob into the row via the live publish path.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes; the publish's
/// [`UNMODELED_KEYS`] refusal passes through.
pub fn backfill_operator_escalation_push(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let doc: OperatorEscalationPush = serde_json::from_slice(blob)
        .map_err(|e| corrupt_store("operator-escalation-push-blob", e))?;
    publish(tx, row_slug, company_slug, &doc)
}

// ---- shadow-diff (zero-loss verifier) ------------------------------------

/// The `operator-escalation-push` zero-loss verifier. Signature mirrors
/// `migration::shadow_diff_manifest`.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes, [`ChiefdError::StoreFailure`] on a SQL
/// failure; an unmodeled
/// key is recorded loud, not an error.
pub fn shadow_diff_operator_escalation_push(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<ShadowReport, ChiefdError> {
    let mut report = ShadowReport::new(OPERATOR_ESCALATION_PUSH_STORE);
    let original: OperatorEscalationPush = serde_json::from_slice(blob)
        .map_err(|e| corrupt_store("operator-escalation-push-blob", e))?;
    if let Err(e) = backfill_operator_escalation_push(tx, row_slug, company_slug, blob) {
        if e.code() == Some(UNMODELED_KEYS) {
            report.record_loud(format!("UNMODELED KEYS rejected by publish: {e}"));
            return Ok(report);
        }
        return Err(e);
    }
    let r = reconstruct(tx, row_slug)?.ok_or_else(|| {
        crate::error::store_failure_because(
            "operator-escalation-push-rows",
            "the push rows are missing immediately after their own publish",
        )
    })?;
    report.row_count = 1;
    report.record("schemaVersion", Disposition::Derived { proof: "constant 1".into() });
    report.record("lastPushedAt", opt_disposition(&original.last_pushed_at, &r.last_pushed_at));
    match (&original.pending, &r.pending) {
        (None, None) => report.record(
            "pending",
            Disposition::ExpectedDropped { where_now: "absent (NULL columns)".into() },
        ),
        (Some(o), Some(n)) => {
            let m = |eq: bool, v: &str| {
                if eq {
                    Disposition::Matched
                } else {
                    Disposition::Lost { blob_value: v.to_string() }
                }
            };
            report.record("pending.text", m(o.text == n.text, &o.text));
            report.record("pending.fingerprint", m(o.fingerprint == n.fingerprint, &o.fingerprint));
            report.record("pending.attempts", m(o.attempts == n.attempts, &o.attempts.to_string()));
        }
        (Some(o), None) => {
            report.record("pending", Disposition::Lost { blob_value: format!("{o:?}") })
        }
        (None, Some(_)) => report.record("pending", Disposition::Matched),
    }
    Ok(report)
}

/// Disposition for an optional scalar VALUE: absent-in-blob is an
/// ExpectedDropped NULL; present is Matched/Lost by equality.
fn opt_disposition(original: &Option<String>, recon: &Option<String>) -> Disposition {
    match original {
        None => Disposition::ExpectedDropped { where_now: "absent (NULL column)".into() },
        Some(v) if recon.as_deref() == Some(v.as_str()) => Disposition::Matched,
        Some(v) => Disposition::Lost { blob_value: v.clone() },
    }
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

    #[test]
    fn empty_doc_round_trips_with_no_pending() {
        // The live cobalt blob is exactly {"schemaVersion":1}.
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let d = OperatorEscalationPush {
            schema_version: 1,
            last_pushed_at: None,
            pending: None,
            extra: BTreeMap::new(),
        };
        publish(&tx, "acme", "acme", &d).unwrap();
        let got = reconstruct(&tx, "acme").unwrap().unwrap();
        assert_eq!(got, d);
    }

    #[test]
    fn pending_triple_round_trips_coherently() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let d = OperatorEscalationPush {
            schema_version: 1,
            last_pushed_at: Some("2026-07-25T00:00:00.000Z".into()),
            pending: Some(PendingDoorbell {
                text: "blocked".into(),
                fingerprint: "abc".into(),
                attempts: 2,
            }),
            extra: BTreeMap::new(),
        };
        publish(&tx, "acme", "acme", &d).unwrap();
        assert_eq!(reconstruct(&tx, "acme").unwrap().unwrap(), d);
    }

    #[test]
    fn unchanged_publish_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let d = OperatorEscalationPush {
            schema_version: 1,
            last_pushed_at: Some("t".into()),
            pending: None,
            extra: BTreeMap::new(),
        };
        publish(&tx, "acme", "acme", &d).unwrap();
        let seq = publish(&tx, "acme", "acme", &d).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(current_seq(&tx, "acme").unwrap(), 1);
    }

    #[test]
    fn clearing_pending_emits_an_event() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let armed = OperatorEscalationPush {
            schema_version: 1,
            last_pushed_at: None,
            pending: Some(PendingDoorbell {
                text: "b".into(),
                fingerprint: "f".into(),
                attempts: 1,
            }),
            extra: BTreeMap::new(),
        };
        publish(&tx, "acme", "acme", &armed).unwrap();
        let cleared = OperatorEscalationPush {
            schema_version: 1,
            last_pushed_at: Some("t".into()),
            pending: None,
            extra: BTreeMap::new(),
        };
        let seq = publish(&tx, "acme", "acme", &cleared).unwrap();
        assert_eq!(seq, 2);
        assert_eq!(reconstruct(&tx, "acme").unwrap().unwrap(), cleared);
        assert_eq!(current_seq(&tx, "acme").unwrap(), 2);
    }

    #[test]
    fn rejects_unmodeled_keys() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut d = OperatorEscalationPush {
            schema_version: 1,
            last_pushed_at: None,
            pending: None,
            extra: BTreeMap::new(),
        };
        d.extra.insert("junk".into(), serde_json::json!(1));
        assert_eq!(publish(&tx, "acme", "acme", &d).unwrap_err().code(), Some(UNMODELED_KEYS));
    }

    #[test]
    fn backfill_from_live_empty_blob() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        backfill_operator_escalation_push(&tx, "acme", "acme", br#"{"schemaVersion":1}"#).unwrap();
        let got = reconstruct(&tx, "acme").unwrap().unwrap();
        assert_eq!(got.pending, None);
        assert_eq!(got.last_pushed_at, None);
    }
}
