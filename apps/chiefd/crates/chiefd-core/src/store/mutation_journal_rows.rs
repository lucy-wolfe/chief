//! The `mutation-journal` ROW implementation (org-data-normalization P0, N7/N8
//! journals port; bucket1b).
//!
//! The per-company org-mutation journal — the in-flight/committed/abandoned
//! marker set that closes the SIGKILL-between-commits gap in `launchOrganizationUnit`
//! (see `org-mutation-journal.ts`). One row per mutation in the slug-less
//! (one-DB-per-company) `mutation_journal` table, keyed by `mutation_id`; the
//! per-row `seq` (allocated `MAX(seq)+1`, table-local) preserves append order ==
//! recency, so `reconstruct` returns entries in the exact order the TS array held
//! them. DERIVED, never stored: `version` = const `1`; `organization` = company
//! slug.
//!
//! Storage is a MUTABLE state machine (`in-flight -> committed | abandoned`) with
//! a fingerprint-adoption lookup, which is why it is its own table and NOT the
//! append-only `org_events` feed (N7 Fable ruling). Bounded retention (keep the
//! newest [`MUTATION_JOURNAL_COMMITTED_CAP`] committed; in-flight/abandoned never
//! dropped) is applied by the TS writer BEFORE publish, so this port stores
//! exactly the entries it is handed — the diff is a pure set/field reconcile.
//!
//! Item D: publish REJECTS any serde-flatten `extra` (doc- or record-level) with
//! [`UNMODELED_KEYS`].

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::corrupt_store;
use crate::error::Refusal;
use crate::store::organization_rows::{RowsSqlError, UNMODELED_KEYS};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::store::shadow_report::{Disposition, ShadowReport};
use crate::ChiefdError;

/// The committed-retention cap, byte-identical to
/// `MUTATION_JOURNAL_COMMITTED_CAP` in `org-mutation-journal.ts`. Enforced by the
/// TS writer; recorded here for parity documentation only.
pub const MUTATION_JOURNAL_COMMITTED_CAP: usize = 32;

/// One mutation record. Mirrors `OrganizationMutationRecord`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationRecord {
    /// The per-mutation id — the PK.
    pub mutation_id: String,
    /// The verb (`unit-launch`, `transfer`, …).
    pub verb: String,
    /// Canonical digest of the desired end state — the adoption key.
    pub fingerprint: String,
    /// `in-flight` | `committed` | `abandoned`.
    pub status: String,
    /// ISO-8601 start stamp.
    pub started_at: String,
    /// ISO-8601 last-transition stamp.
    pub updated_at: String,
    /// Optional change author.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Any unmodeled key (item D).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The whole `mutation-journal` doc. Mirrors `OrganizationMutationJournal`;
/// `version`/`organization` are DERIVED on reconstruct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationJournal {
    /// Always `1`. Not stored.
    pub version: u32,
    /// The company slug — DERIVED, not stored.
    pub organization: String,
    /// Entries in append order (== recency). One row each.
    pub entries: Vec<MutationRecord>,
    /// Any unmodeled document-level key (item D).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The `org_documents` store family this row set replaces.
pub const MUTATION_JOURNAL_STORE: &str = "mutation-journal";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("mutation-journal-rows", e)
}

/// The stored column tuple of one record (excludes the internal `seq`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordCols {
    verb: String,
    fingerprint: String,
    status: String,
    started_at: String,
    updated_at: String,
    actor: Option<String>,
}

impl RecordCols {
    fn of(r: &MutationRecord) -> Self {
        Self {
            verb: r.verb.clone(),
            fingerprint: r.fingerprint.clone(),
            status: r.status.clone(),
            started_at: r.started_at.clone(),
            updated_at: r.updated_at.clone(),
            actor: r.actor.clone(),
        }
    }
}

/// The current `(mutation_id -> (seq, cols))` set.
fn read_rows(
    tx: &Transaction<'_>,
    row_slug: &str,
) -> Result<BTreeMap<String, (i64, RecordCols)>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT mutation_id, seq, verb, fingerprint, status, started_at, updated_at, actor \
             FROM mutation_journal WHERE slug = ?1",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![row_slug], |r| {
            let id: String = r.get(0)?;
            let seq: i64 = r.get(1)?;
            Ok((
                id,
                seq,
                RecordCols {
                    verb: r.get(2)?,
                    fingerprint: r.get(3)?,
                    status: r.get(4)?,
                    started_at: r.get(5)?,
                    updated_at: r.get(6)?,
                    actor: r.get(7)?,
                },
            ))
        })
        .map_err(store_failure)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (id, seq, cols) = row.map_err(store_failure)?;
        map.insert(id, (seq, cols));
    }
    Ok(map)
}

/// Reconstruct the journal for the company, or `None` when the table is empty.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<Option<MutationJournal>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT mutation_id, verb, fingerprint, status, started_at, updated_at, actor \
             FROM mutation_journal WHERE slug = ?1 ORDER BY seq",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![row_slug], |r| {
            Ok(MutationRecord {
                mutation_id: r.get(0)?,
                verb: r.get(1)?,
                fingerprint: r.get(2)?,
                status: r.get(3)?,
                started_at: r.get(4)?,
                updated_at: r.get(5)?,
                actor: r.get(6)?,
                extra: BTreeMap::new(),
            })
        })
        .map_err(store_failure)?;
    let entries: Vec<MutationRecord> = rows.collect::<Result<_, _>>().map_err(store_failure)?;
    if entries.is_empty() {
        return Ok(None);
    }
    Ok(Some(MutationJournal {
        version: 1,
        organization: company_slug.to_string(),
        entries,
        extra: BTreeMap::new(),
    }))
}

/// Publish the journal as a direct atomic current-state write. Deletes rows no
/// longer present and upserts changed/new ones (new rows allocate a table-local
/// `MAX(seq)+1` in incoming order so append order is preserved). One immutable
/// `org_events` touch is emitted per changed entity.
///
/// # Errors
/// [`UNMODELED_KEYS`] refusal (422); SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming: &MutationJournal,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    let current = read_rows(tx, row_slug)?;
    let incoming_ids: BTreeSet<&String> = incoming.entries.iter().map(|e| &e.mutation_id).collect();
    // Event stamp: the newest entry updatedAt, else empty.
    let at = incoming.entries.iter().map(|e| e.updated_at.clone()).max().unwrap_or_default();
    let mut next_seq: i64 = current.values().map(|(s, _)| *s).max().unwrap_or(0);

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        let mut touches = Vec::new();
        // deletes
        for id in current.keys() {
            if !incoming_ids.contains(id) {
                tx.execute("DELETE FROM mutation_journal WHERE slug = ?1 AND mutation_id = ?2", params![row_slug, id])?;
                touches.push(EventTouch::new(
                    "mutation", id.clone(), "delete", "mutation_journal", row_slug,
                ));
            }
        }
        // upserts (incoming order)
        for entry in &incoming.entries {
            let cols = RecordCols::of(entry);
            if let Some((_seq, existing)) = current.get(&entry.mutation_id) {
                if *existing == cols {
                    continue; // unchanged
                }
                tx.execute(
                    "UPDATE mutation_journal SET verb=?3, fingerprint=?4, status=?5, \
                     started_at=?6, updated_at=?7, actor=?8 \
                     WHERE slug = ?1 AND mutation_id = ?2",
                    params![
                        row_slug, entry.mutation_id, cols.verb, cols.fingerprint, cols.status,
                        cols.started_at, cols.updated_at, cols.actor,
                    ],
                )?;
            } else {
                next_seq += 1;
                tx.execute(
                    "INSERT INTO mutation_journal(slug, mutation_id, seq, verb, fingerprint, status, \
                     started_at, updated_at, actor) \
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        row_slug, entry.mutation_id, next_seq, cols.verb, cols.fingerprint, cols.status,
                        cols.started_at, cols.updated_at, cols.actor,
                    ],
                )?;
            }
            touches.push(EventTouch::new(
                "mutation", entry.mutation_id.clone(), "upsert", "mutation_journal", row_slug,
            ));
        }
        Ok(touches)
    })
    .map_err(|RowsSqlError(e)| e)
}

fn reject_unmodeled_keys(doc: &MutationJournal) -> Result<(), ChiefdError> {
    let mut paths: Vec<String> = doc.extra.keys().map(|k| format!("extra.{k}")).collect();
    for (i, e) in doc.entries.iter().enumerate() {
        for k in e.extra.keys() {
            paths.push(format!("entries.{i}.extra.{k}"));
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!(
            "mutation-journal carries unmodeled keys the row model cannot store: {}",
            paths.join(", ")
        ),
    )))
}

/// Backfill the blob into the rows via the live publish path.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes; the publish's
/// [`UNMODELED_KEYS`] refusal passes through.
pub fn backfill_mutation_journal(
    tx: &Transaction<'_>,
    row_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let doc: MutationJournal =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("mutation-journal-blob", e))?;
    publish(tx, row_slug, &doc)
}

/// The `mutation-journal` zero-loss verifier.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes, [`ChiefdError::StoreFailure`] on a SQL
/// failure; an unmodeled
/// key is recorded loud, not an error.
pub fn shadow_diff_mutation_journal(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<ShadowReport, ChiefdError> {
    let mut report = ShadowReport::new(MUTATION_JOURNAL_STORE);
    let original: MutationJournal =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("mutation-journal-blob", e))?;
    match backfill_mutation_journal(tx, row_slug, blob) {
        Ok(_) => {}
        Err(e) if e.code() == Some(UNMODELED_KEYS) => {
            report.record_loud(format!("UNMODELED KEYS rejected by publish: {e}"));
            return Ok(report);
        }
        Err(e) => return Err(e),
    }
    let recon = reconstruct(tx, row_slug, company_slug)?.unwrap_or(MutationJournal {
        version: 1,
        organization: company_slug.to_string(),
        entries: Vec::new(),
        extra: BTreeMap::new(),
    });
    report.row_count = recon.entries.len();
    report.record("version", Disposition::Derived { proof: "constant 1".into() });
    report.record("organization", Disposition::Derived { proof: "process company slug".into() });
    // entries: reconstruct must equal the blob's entries in order and content.
    if recon.entries == original.entries {
        report.record("entries", Disposition::Matched);
    } else {
        report.record(
            "entries",
            Disposition::Lost { blob_value: format!("{} entries", original.entries.len()) },
        );
    }
    Ok(report)
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
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("schema");
        conn
    }

    fn rec(id: &str, status: &str) -> MutationRecord {
        MutationRecord {
            mutation_id: id.into(),
            verb: "unit-launch".into(),
            fingerprint: "fp-1".into(),
            status: status.into(),
            started_at: "2026-07-25T06:00:00.000Z".into(),
            updated_at: "2026-07-25T06:00:00.000Z".into(),
            actor: None,
            extra: BTreeMap::new(),
        }
    }

    fn journal(entries: Vec<MutationRecord>) -> MutationJournal {
        MutationJournal { version: 1, organization: "acme".into(), entries, extra: BTreeMap::new() }
    }

    #[test]
    fn empty_before_any_publish() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap(), None);
    }

    #[test]
    fn round_trips_and_derives_identity() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let d = journal(vec![rec("m1", "in-flight"), rec("m2", "committed")]);
        publish(&tx, "acme", &d).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got, d);
        assert_eq!(got.version, 1);
        assert_eq!(got.organization, "acme");
        assert_eq!(event_count(&tx), 2); // one touch per entry
    }

    #[test]
    fn preserves_append_order_across_publishes() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &journal(vec![rec("m1", "in-flight")])).unwrap();
        publish(&tx, "acme", &journal(vec![rec("m1", "in-flight"), rec("m2", "in-flight")]))
            .unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(
            got.entries.iter().map(|e| e.mutation_id.clone()).collect::<Vec<_>>(),
            vec!["m1", "m2"]
        );
    }

    #[test]
    fn status_transition_updates_in_place_without_reorder() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &journal(vec![rec("m1", "in-flight"), rec("m2", "in-flight")]))
            .unwrap();
        let mut m1 = rec("m1", "committed");
        m1.updated_at = "2026-07-25T06:05:00.000Z".into();
        let out = publish(&tx, "acme", &journal(vec![m1.clone(), rec("m2", "in-flight")])).unwrap();
        // only m1 changed -> one touch
        assert_eq!(out, 3);
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got.entries[0], m1);
        assert_eq!(got.entries[1].status, "in-flight");
    }

    #[test]
    fn retention_drop_deletes_the_oldest_row() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &journal(vec![rec("m1", "committed"), rec("m2", "committed")]))
            .unwrap();
        // TS retention dropped m1; incoming carries only m2.
        publish(&tx, "acme", &journal(vec![rec("m2", "committed")])).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(
            got.entries.iter().map(|e| e.mutation_id.clone()).collect::<Vec<_>>(),
            vec!["m2"]
        );
    }

    #[test]
    fn unchanged_publish_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &journal(vec![rec("m1", "in-flight")])).unwrap();
        let out = publish(&tx, "acme", &journal(vec![rec("m1", "in-flight")])).unwrap();
        assert_eq!(out, 1);
    }

    #[test]
    fn a_second_direct_publish_uses_current_rows() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &journal(vec![rec("m1", "in-flight")])).unwrap();
        let mut committed = rec("m1", "committed");
        committed.updated_at = "2026-07-25T06:05:00.000Z".into();
        let out = publish(&tx, "acme", &journal(vec![committed.clone()])).unwrap();
        assert_eq!(out, 2);
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().unwrap().entries, vec![committed]);
    }

    #[test]
    fn rejects_unmodeled_record_keys() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut r = rec("m1", "in-flight");
        r.extra.insert("bogus".into(), serde_json::json!(1));
        assert_eq!(
            publish(&tx, "acme", &journal(vec![r])).unwrap_err().code(),
            Some(UNMODELED_KEYS)
        );
    }

    #[test]
    fn slug_scoping_isolates_companies_in_a_shared_db() {
        // The shared org.sqlite hosts many companies; a slug-scoped table must
        // never let one company's journal leak into another's (the 78-row
        // cross-company leak that #32 fixed).
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &journal(vec![rec("a1", "in-flight")])).unwrap();
        publish(&tx, "beta", &journal(vec![rec("b1", "committed"), rec("b2", "committed")]))
            .unwrap();
        let acme = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        let beta = reconstruct(&tx, "beta", "beta").unwrap().unwrap();
        assert_eq!(
            acme.entries.iter().map(|e| e.mutation_id.clone()).collect::<Vec<_>>(),
            vec!["a1"]
        );
        assert_eq!(
            beta.entries.iter().map(|e| e.mutation_id.clone()).collect::<Vec<_>>(),
            vec!["b1", "b2"]
        );
        // Deleting acme's entry leaves beta's rows untouched.
        publish(&tx, "acme", &journal(vec![])).unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap(), None);
        assert_eq!(reconstruct(&tx, "beta", "beta").unwrap().unwrap().entries.len(), 2);
    }

    #[test]
    fn shadow_diff_zero_loss_on_a_full_blob() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let blob = br#"{"version":1,"organization":"acme","entries":[{"mutationId":"m1","verb":"unit-launch","fingerprint":"fp","status":"committed","startedAt":"2026-07-25T06:00:00.000Z","updatedAt":"2026-07-25T06:05:00.000Z","actor":"chief"},{"mutationId":"m2","verb":"transfer","fingerprint":"fp2","status":"in-flight","startedAt":"2026-07-25T06:06:00.000Z","updatedAt":"2026-07-25T06:06:00.000Z"}]}"#;
        let report = shadow_diff_mutation_journal(&tx, "acme", "acme", blob).unwrap();
        assert!(report.zero_loss(), "loud: {:?}", report.loud_failures());
        assert_eq!(report.row_count, 2);
    }
}
