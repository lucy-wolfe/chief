//! The `supervisor-watermark` ROW implementation (org-data-normalization P0 —
//! blob-only residual port before the documents DROP).
//!
//! The per-duty liveness watermarks ([`SupervisorWatermarkState`]) as slug-scoped
//! child rows in `supervisor_watermarks`, one per `(slug, duty)`. DERIVED, never
//! stored: `schemaVersion` = const [`SUPERVISOR_WATERMARK_SCHEMA_VERSION`];
//! `organization` = the company slug. `duty` is BOTH the map key and a stored
//! column. An absent doc == no duty rows == the empty default (a company that has
//! recorded no successes). Reuses the canonical body structs from
//! [`super::supervisor_watermark`] so `backfill` parses the exact blob bytes.
//!
//! Self-contained (reconstruct + diff + backfill + clear); N8 wires it into
//! `persist_dispatch` + `load_ledgers`.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Transaction};

use crate::error::corrupt_store;
use crate::store::organization_rows::RowsSqlError;
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::store::supervisor_watermark::{
    DutyWatermark, SupervisorWatermarkState, SUPERVISOR_WATERMARK_SCHEMA_VERSION,
};
use crate::ChiefdError;

/// The `documents` store family this row set replaces.
pub const SUPERVISOR_WATERMARK_STORE: &str = "supervisor-watermark";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("supervisor-watermark-rows", e)
}

/// The stored columns of one duty row (excludes `duty`, which is the key).
///
/// #825-prereq: `last_failure_*`/`consecutive_failures` mirror
/// `DutyWatermark`'s bounded singleton fields one-for-one — the row shape,
/// not just the in-memory struct, carries at most one failure per duty.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DutyCols {
    interval_ms: i64,
    last_success_at: String,
    run_count: i64,
    last_failure_at: Option<String>,
    last_failure_kind: Option<String>,
    last_failure_detail: Option<String>,
    consecutive_failures: i64,
}

impl DutyCols {
    fn of(d: &DutyWatermark) -> Self {
        Self {
            interval_ms: d.interval_ms,
            last_success_at: d.last_success_at.clone(),
            run_count: i64::try_from(d.run_count).unwrap_or(i64::MAX),
            last_failure_at: d.last_failure_at.clone(),
            last_failure_kind: d.last_failure_kind.clone(),
            last_failure_detail: d.last_failure_detail.clone(),
            consecutive_failures: i64::try_from(d.consecutive_failures).unwrap_or(i64::MAX),
        }
    }
}

fn read_rows(
    tx: &Transaction<'_>,
    row_slug: &str,
) -> Result<BTreeMap<String, DutyCols>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT duty, interval_ms, last_success_at, run_count, last_failure_at, \
             last_failure_kind, last_failure_detail, consecutive_failures \
             FROM supervisor_watermarks WHERE slug = ?1",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![row_slug], |r| {
            Ok((
                r.get::<_, String>(0)?,
                DutyCols {
                    interval_ms: r.get(1)?,
                    last_success_at: r.get(2)?,
                    run_count: r.get(3)?,
                    last_failure_at: r.get(4)?,
                    last_failure_kind: r.get(5)?,
                    last_failure_detail: r.get(6)?,
                    consecutive_failures: r.get(7)?,
                },
            ))
        })
        .map_err(store_failure)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (duty, cols) = row.map_err(store_failure)?;
        map.insert(duty, cols);
    }
    Ok(map)
}

/// Reconstruct the watermark state, or `None` when the company has no duty rows
/// (absent == the empty default — no success has been recorded yet).
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<Option<SupervisorWatermarkState>, ChiefdError> {
    let rows = read_rows(tx, row_slug)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let duties = rows
        .into_iter()
        .map(|(duty, c)| {
            (
                duty.clone(),
                DutyWatermark {
                    duty,
                    interval_ms: c.interval_ms,
                    last_success_at: c.last_success_at,
                    run_count: u64::try_from(c.run_count).unwrap_or(0),
                    last_failure_at: c.last_failure_at,
                    last_failure_kind: c.last_failure_kind,
                    last_failure_detail: c.last_failure_detail,
                    consecutive_failures: u64::try_from(c.consecutive_failures).unwrap_or(0),
                },
            )
        })
        .collect();
    Ok(Some(SupervisorWatermarkState {
        schema_version: SUPERVISOR_WATERMARK_SCHEMA_VERSION,
        organization: company_slug.to_string(),
        duties,
    }))
}

/// Publish the watermark state from current SQLite state. Deletes duty rows no
/// longer present and upserts changed/new ones; one `org_events` touch per
/// changed duty.
///
/// # Errors
/// SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming: &SupervisorWatermarkState,
) -> Result<i64, ChiefdError> {
    let current = read_rows(tx, row_slug)?;
    let incoming_cols: BTreeMap<String, DutyCols> =
        incoming.duties.iter().map(|(k, v)| (k.clone(), DutyCols::of(v))).collect();
    // Event stamp: the newest last_success_at across the incoming duties.
    let at = incoming_cols.values().map(|c| c.last_success_at.clone()).max().unwrap_or_default();

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        let mut touches = Vec::new();
        let cur_keys: BTreeSet<&String> = current.keys().collect();
        let inc_keys: BTreeSet<&String> = incoming_cols.keys().collect();
        for duty in cur_keys.difference(&inc_keys) {
            tx.execute(
                "DELETE FROM supervisor_watermarks WHERE slug = ?1 AND duty = ?2",
                params![row_slug, duty],
            )?;
            touches.push(EventTouch::new(
                "supervisor-watermark",
                (*duty).clone(),
                "delete",
                "supervisor_watermarks",
                row_slug,
            ));
        }
        for (duty, cols) in &incoming_cols {
            if current.get(duty) == Some(cols) {
                continue;
            }
            tx.execute(
                "INSERT INTO supervisor_watermarks(slug, duty, interval_ms, last_success_at, \
                 run_count, last_failure_at, last_failure_kind, last_failure_detail, \
                 consecutive_failures) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(slug, duty) DO UPDATE SET \
                 interval_ms=?3, last_success_at=?4, run_count=?5, last_failure_at=?6, \
                 last_failure_kind=?7, last_failure_detail=?8, consecutive_failures=?9",
                params![
                    row_slug,
                    duty,
                    cols.interval_ms,
                    cols.last_success_at,
                    cols.run_count,
                    cols.last_failure_at,
                    cols.last_failure_kind,
                    cols.last_failure_detail,
                    cols.consecutive_failures,
                ],
            )?;
            touches.push(EventTouch::new(
                "supervisor-watermark",
                duty.clone(),
                "upsert",
                "supervisor_watermarks",
                row_slug,
            ));
        }
        Ok(touches)
    })
    .map_err(|RowsSqlError(e)| e)
}

/// Fence-free CLEAR: delete every duty row for `row_slug`, one `delete` touch
/// per removed duty.
///
/// # Errors
/// SQL failures as [`ChiefdError::StoreFailure`].
pub fn clear(tx: &Transaction<'_>, row_slug: &str, at: &str) -> Result<(), ChiefdError> {
    let duties: Vec<String> = read_rows(tx, row_slug)?.into_keys().collect();
    apply_and_emit::<RowsSqlError, _>(tx, row_slug, at, "", |tx| {
        let mut touches = Vec::new();
        for duty in &duties {
            tx.execute(
                "DELETE FROM supervisor_watermarks WHERE slug = ?1 AND duty = ?2",
                params![row_slug, duty],
            )?;
            touches.push(EventTouch::new(
                "supervisor-watermark",
                duty.clone(),
                "delete",
                "supervisor_watermarks",
                row_slug,
            ));
        }
        Ok(touches)
    })
    .map(|_seq| ())
    .map_err(|RowsSqlError(e)| e)
}

/// Backfill the blob body into the rows via the live publish path.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes; SQL failures propagate.
pub fn backfill_supervisor_watermark(
    tx: &Transaction<'_>,
    row_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let state: SupervisorWatermarkState =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("supervisor-watermark-blob", e))?;
    publish(tx, row_slug, &state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("schema");
        conn
    }

    fn event_count(tx: &Transaction<'_>, slug: &str) -> i64 {
        tx.query_row("SELECT COUNT(*) FROM org_events WHERE slug = ?1", params![slug], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn duty(duty: &str, last: &str, runs: u64) -> DutyWatermark {
        DutyWatermark {
            duty: duty.into(),
            interval_ms: 900_000,
            last_success_at: last.into(),
            run_count: runs,
            last_failure_at: None,
            last_failure_kind: None,
            last_failure_detail: None,
            consecutive_failures: 0,
        }
    }

    fn failing_duty(duty_key: &str, last: &str, runs: u64, failures: u64) -> DutyWatermark {
        DutyWatermark {
            last_failure_at: Some("2026-07-25T06:10:00.000Z".into()),
            last_failure_kind: Some("reconcile_refused".into()),
            last_failure_detail: Some("ledger mutate refused".into()),
            consecutive_failures: failures,
            ..duty(duty_key, last, runs)
        }
    }

    fn state(duties: Vec<DutyWatermark>) -> SupervisorWatermarkState {
        SupervisorWatermarkState {
            schema_version: SUPERVISOR_WATERMARK_SCHEMA_VERSION,
            organization: "acme".into(),
            duties: duties.into_iter().map(|d| (d.duty.clone(), d)).collect(),
        }
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
        let s = state(vec![
            duty("supervision-reconcile", "2026-07-25T06:00:00.000Z", 3),
            duty("mailbox-wake", "2026-07-25T06:01:00.000Z", 7),
        ]);
        publish(&tx, "acme", &s).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got, s);
        assert_eq!(got.schema_version, SUPERVISOR_WATERMARK_SCHEMA_VERSION);
        assert_eq!(got.organization, "acme");
        assert_eq!(event_count(&tx, "acme"), 2);
    }

    #[test]
    fn advancing_one_duty_upserts_only_it() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state(vec![duty("a", "t0", 1), duty("b", "t0", 1)])).unwrap();
        let seq = event_count(&tx, "acme");
        let out =
            publish(&tx, "acme", &state(vec![duty("a", "t1", 2), duty("b", "t0", 1)])).unwrap();
        assert_eq!(out, seq + 1);
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got.duties["a"].run_count, 2);
        assert_eq!(got.duties["b"].run_count, 1);
    }

    #[test]
    fn removing_a_duty_deletes_its_row() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state(vec![duty("a", "t0", 1), duty("b", "t0", 1)])).unwrap();
        publish(&tx, "acme", &state(vec![duty("a", "t0", 1)])).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got.duties.keys().cloned().collect::<Vec<_>>(), vec!["a"]);
    }

    #[test]
    fn unchanged_publish_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state(vec![duty("a", "t0", 1)])).unwrap();
        let seq = event_count(&tx, "acme");
        let out = publish(&tx, "acme", &state(vec![duty("a", "t0", 1)])).unwrap();
        assert_eq!(out, seq);
    }

    #[test]
    fn a_second_direct_publish_uses_current_state() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state(vec![duty("a", "t0", 1)])).unwrap();
        let out = publish(&tx, "acme", &state(vec![duty("a", "t0", 1)])).unwrap();
        assert_eq!(out, 1);
    }

    #[test]
    fn clear_deletes_all_duty_rows() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state(vec![duty("a", "t0", 1), duty("b", "t0", 1)])).unwrap();
        clear(&tx, "acme", "2026-07-25T06:05:00.000Z").unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap(), None);
    }

    #[test]
    fn backfill_round_trips_a_blob_body() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let s = state(vec![duty("supervision-reconcile", "t0", 5)]);
        let blob = serde_json::to_vec(&s).unwrap();
        backfill_supervisor_watermark(&tx, "acme", &blob).unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().unwrap(), s);
    }

    #[test]
    fn round_trips_a_failure_record() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let s = state(vec![failing_duty("supervision_reconcile", "t0", 3, 2)]);
        publish(&tx, "acme", &s).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got, s);
        assert!(got.duties["supervision_reconcile"].is_failing());
    }

    #[test]
    fn clearing_a_failure_via_republish_drops_all_four_columns() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state(vec![failing_duty("a", "t0", 1, 5)])).unwrap();
        publish(&tx, "acme", &state(vec![duty("a", "t1", 2)])).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert!(!got.duties["a"].is_failing());
        assert_eq!(got.duties["a"].consecutive_failures, 0);
        assert_eq!(got.duties["a"].last_failure_kind, None);
    }

    #[test]
    fn failure_row_stays_one_row_regardless_of_how_many_failures_are_recorded() {
        // Boundedness at the row level: publishing 50 consecutive failures for
        // the same duty must still leave exactly one row for it.
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        for n in 1..=50u64 {
            publish(&tx, "acme", &state(vec![failing_duty("a", "", 0, n)])).unwrap();
        }
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM supervisor_watermarks WHERE slug='acme' AND duty='a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "one row per duty no matter how many failures were recorded");
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got.duties["a"].consecutive_failures, 50);
    }

    #[test]
    fn slug_scoping_isolates_companies() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state(vec![duty("a", "t0", 1)])).unwrap();
        publish(&tx, "beta", &state(vec![duty("b", "t0", 9)])).unwrap();
        assert_eq!(
            reconstruct(&tx, "acme", "acme")
                .unwrap()
                .unwrap()
                .duties
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["a"]
        );
        let beta = reconstruct(&tx, "beta", "beta").unwrap().unwrap();
        assert_eq!(beta.duties.keys().cloned().collect::<Vec<_>>(), vec!["b"]);
        assert_eq!(beta.organization, "beta");
    }
}
