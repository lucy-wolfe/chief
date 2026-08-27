//! The `session-epoch` singleton ROW implementation (org-data-normalization P0,
//! N2 copy-pattern, B4 singleton sweep).
//!
//! Reconstruct an [`SessionEpoch`] from the `session_epoch` table and publish
//! one by diffing it against the current row — the singleton analogue of
//! [`crate::store::organization_rows`]. A singleton is the simplest shape: at
//! most one row per company (`slug` PK), so the diff is "row present & equal?"
//! and a change is a single `org_events` touch (`detail_ref = 'table:pk'`).
//!
//! Identity that is DERIVED, never stored (B2): `version` is the compile-time
//! constant `1`; `organization` is the process's own company slug (the `chief.db`
//! is per-company, so it is redundant with the PK). Only `epochAt` and `reason`
//! are stored (columns `at`, `reason`).
//!
//! Item D (Fable #6): a normalized singleton carries NO unmodeled keys. Publish
//! REJECTS any serde-flatten `extra` with [`UNMODELED_KEYS`] — never drops.
//!
//! TS is a thin HTTP client over `/v1/org/session-epoch/{read,publish}`; no SQL
//! lives outside chiefd-core.

use std::collections::BTreeMap;

use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::corrupt_store;
use crate::error::Refusal;
use crate::store::organization_rows::{RowsSqlError, UNMODELED_KEYS};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::store::shadow_report::{Disposition, ShadowReport};
use crate::ChiefdError;

/// A `session-epoch` singleton in its typed form. Mirrors the TS
/// `OrganizationSessionEpoch` exactly; `version`/`organization` are DERIVED on
/// reconstruct (never read from a column). `extra` captures any unmodeled key so
/// publish can reject it (item D) instead of silently dropping it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEpoch {
    /// Always `1`. A compile-time constant, not a stored column.
    pub version: u32,
    /// The company slug — DERIVED from the process's own company, not stored.
    pub organization: String,
    /// Transcripts last modified before this instant are not resumed.
    #[serde(rename = "epochAt")]
    pub epoch_at: String,
    /// Opaque prose VALUE (D1 KEEP).
    pub reason: String,
    /// Any key the row model does not represent (item D). Empty on a clean doc.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The `org_documents` store family this row set replaces.
pub const SESSION_EPOCH_STORE: &str = "session-epoch";

/// A SQL failure reading/writing the row is a store failure, not a caller
/// error. Single greppable mapping point (mirrors organization_rows::store_failure).
fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("session-epoch-rows", e)
}

// ---- reconstruct (read path) ---------------------------------------------

/// Reconstruct the session-epoch singleton for `company_slug` from its row, or
/// `None` when the company has no `session_epoch` row.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<Option<SessionEpoch>, ChiefdError> {
    let mut stmt = tx
        .prepare("SELECT at, reason FROM session_epoch WHERE slug = ?1")
        .map_err(store_failure)?;
    let mut rows = stmt.query(params![row_slug]).map_err(store_failure)?;
    let Some(row) = rows.next().map_err(store_failure)? else {
        return Ok(None);
    };
    Ok(Some(SessionEpoch {
        version: 1,
        organization: company_slug.to_string(),
        epoch_at: row.get(0).map_err(store_failure)?,
        reason: row.get(1).map_err(store_failure)?,
        extra: BTreeMap::new(),
    }))
}

// ---- publish (diff/write path) -------------------------------------------

/// Publish the singleton into its row as a direct atomic current-state write.
/// Rejects unmodeled keys (item D) BEFORE writing, then upserts the single row
/// and emits ONE `org_events` touch iff the stored value changed.
///
/// # Errors
/// [`UNMODELED_KEYS`] refusal (maps to 422); SQL failures as
/// [`ChiefdError::Corrupt`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    incoming: &SessionEpoch,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    let current = reconstruct(tx, row_slug, company_slug)?;
    // The event stamp is the doc's own epochAt — the singleton has no separate
    // updated_at, and the epoch instant is the change's authoritative clock.
    let at = incoming.epoch_at.clone();

    // The direct writer transaction is the serialization boundary. The closure
    // uses the shared SQL wrapper because `ChiefdError` intentionally does not
    // implement `From<rusqlite::Error>`.
    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        // No-op iff the stored fields already equal the incoming ones.
        let unchanged = current
            .as_ref()
            .map(|c| c.epoch_at == incoming.epoch_at && c.reason == incoming.reason)
            .unwrap_or(false);
        if unchanged {
            return Ok(Vec::new());
        }
        tx.execute(
            "INSERT INTO session_epoch(slug, at, reason) VALUES(?1, ?2, ?3) \
             ON CONFLICT(slug) DO UPDATE SET at = ?2, reason = ?3",
            params![row_slug, incoming.epoch_at, incoming.reason],
        )?;
        Ok(vec![EventTouch::new(
            "session-epoch",
            company_slug,
            "upsert",
            "session_epoch",
            row_slug,
        )])
    })
    .map_err(|RowsSqlError(e)| e)
}

/// Reject any serde-flatten `extra` key — a normalized singleton carries none
/// (item D). NEVER silently drops.
fn reject_unmodeled_keys(doc: &SessionEpoch) -> Result<(), ChiefdError> {
    if doc.extra.is_empty() {
        return Ok(());
    }
    let mut paths: Vec<String> = doc.extra.keys().map(|k| format!("extra.{k}")).collect();
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!(
            "session-epoch carries unmodeled keys the row model cannot store: {}",
            paths.join(", ")
        ),
    )))
}

// ---- backfill (blob -> rows) ---------------------------------------------

/// Backfill the `session-epoch` blob into the row for one company, through the
/// live publish path (against current transaction state) so the seeded row and its
/// `org_events` entry are indistinguishable from a normal mutation's.
///
/// Signature mirrors `migration::backfill_manifest` so N9 registers it into the
/// migrate CLI's per-store dispatch unchanged.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes; the publish's
/// [`UNMODELED_KEYS`] refusal passes through.
pub fn backfill_session_epoch(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let doc: SessionEpoch =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("session-epoch-blob", e))?;
    publish(tx, row_slug, company_slug, &doc)
}

// ---- shadow-diff (zero-loss verifier) ------------------------------------

/// The `session-epoch` zero-loss verifier: blob → rows → reconstructed doc, then
/// a field-by-field disposition report. Signature mirrors
/// `migration::shadow_diff_manifest`; the caller rolls the txn back (dry-run) or
/// commits it (cutover).
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes, [`ChiefdError::StoreFailure`] on a SQL
/// failure. An unmodeled
/// key is NOT an error — it is recorded in [`ShadowReport::loud_failures`].
pub fn shadow_diff_session_epoch(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<ShadowReport, ChiefdError> {
    let mut report = ShadowReport::new(SESSION_EPOCH_STORE);
    let original: SessionEpoch =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("session-epoch-blob", e))?;
    match backfill_session_epoch(tx, row_slug, company_slug, blob) {
        Ok(_) => {}
        Err(e) if e.code() == Some(UNMODELED_KEYS) => {
            report.record_loud(format!("UNMODELED KEYS rejected by publish: {e}"));
            return Ok(report);
        }
        Err(e) => return Err(e),
    }
    let recon = reconstruct(tx, row_slug, company_slug)?.ok_or_else(|| {
        crate::error::store_failure_because(
            "session-epoch-rows",
            "the session-epoch rows are missing immediately after their own publish",
        )
    })?;
    report.row_count = 1;
    report.record("version", Disposition::Derived { proof: "constant 1".into() });
    report.record("organization", Disposition::Derived { proof: "process company slug".into() });
    report.record(
        "epochAt",
        matched_or_lost(recon.epoch_at == original.epoch_at, &original.epoch_at),
    );
    report.record("reason", matched_or_lost(recon.reason == original.reason, &original.reason));
    Ok(report)
}

fn matched_or_lost(equal: bool, blob_value: &str) -> Disposition {
    if equal {
        Disposition::Matched
    } else {
        Disposition::Lost { blob_value: blob_value.to_string() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn event_count(tx: &Transaction<'_>) -> i64 {
        tx.query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |r| r.get(0))
            .expect("count events")
    }

    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("apply company schema");
        conn
    }

    fn doc(epoch: &str, reason: &str) -> SessionEpoch {
        SessionEpoch {
            version: 1,
            organization: "acme".into(),
            epoch_at: epoch.into(),
            reason: reason.into(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn reconstruct_is_none_before_any_publish() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap(), None);
    }

    #[test]
    fn publish_then_reconstruct_round_trips_and_advances_the_feed() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = publish(&tx, "acme", "acme", &doc("2026-07-25T06:46:10.852Z", "CEO-only boot"))
            .unwrap();
        assert_eq!(out, 1);
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got, doc("2026-07-25T06:46:10.852Z", "CEO-only boot"));
        assert_eq!(event_count(&tx), 1);
    }

    #[test]
    fn an_unchanged_direct_publish_is_a_no_op_that_keeps_the_audit_sequence() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &doc("t1", "r1")).unwrap();
        let out = publish(&tx, "acme", "acme", &doc("t1", "r1")).unwrap();
        assert_eq!(out, 1);
        assert_eq!(event_count(&tx), 1);
    }

    #[test]
    fn a_changed_publish_emits_exactly_one_event() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &doc("t1", "r1")).unwrap();
        publish(&tx, "acme", "acme", &doc("t2", "r2")).unwrap();
        assert_eq!(event_count(&tx), 2);
        let (entity, detail): (String, String) = tx
            .query_row(
                "SELECT entity, detail_ref FROM org_events WHERE slug='acme' AND seq=2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(entity, "session-epoch");
        assert_eq!(detail, "session_epoch:acme/acme");
    }

    #[test]
    fn a_second_direct_publish_replaces_the_current_row() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &doc("t1", "r1")).unwrap();
        let out = publish(&tx, "acme", "acme", &doc("t2", "r2")).unwrap();
        assert_eq!(out, 2);
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().unwrap().epoch_at, "t2");
    }

    #[test]
    fn publish_rejects_unmodeled_keys_with_422_code() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut d = doc("t1", "r1");
        d.extra.insert("sneaky".into(), serde_json::json!("x"));
        let err = publish(&tx, "acme", "acme", &d).unwrap_err();
        assert_eq!(err.code(), Some(UNMODELED_KEYS));
        // Nothing was written.
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap(), None);
    }

    #[test]
    fn backfill_seeds_the_row_from_the_blob_and_derives_identity() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let blob = br#"{"version":1,"organization":"acme","epochAt":"2026-07-25T06:46:10.852Z","reason":"CEO-only boot on socket 'default'"}"#;
        let out = backfill_session_epoch(&tx, "acme", "acme", blob).unwrap();
        assert_eq!(out, 1);
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got.epoch_at, "2026-07-25T06:46:10.852Z");
        assert_eq!(got.reason, "CEO-only boot on socket 'default'");
        assert_eq!(got.organization, "acme"); // DERIVED from company slug
        assert_eq!(got.version, 1); // DERIVED constant
    }
}
