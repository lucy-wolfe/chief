//! The `converge-safety` ROW implementation (org-data-normalization P0 —
//! blob-only residual port before the documents DROP).
//!
//! One slug-scoped singleton row in `converge_safety` reconstructs the
//! [`ConvergeSafetyState`] body the reconcile gate reads. DERIVED, never stored:
//! `schemaVersion` = const [`CONVERGE_SAFETY_SCHEMA_VERSION`]. The nested
//! [`RefusalRecord`] flattens to three nullable columns (all-null == no
//! refusal). Reuses the canonical body structs from [`super::converge_safety`]
//! so `backfill` parses the exact bytes the blob store wrote.
//!
//! Self-contained (reconstruct + diff + backfill + clear); N8 wires it into
//! `persist_dispatch` + `load_ledgers`.

use rusqlite::{params, OptionalExtension, Transaction};

use crate::error::{corrupt_store, corrupt_store_because};
use crate::store::converge_safety::{
    ActuationMode, ConvergeSafetyState, RefusalRecord, CONVERGE_SAFETY_SCHEMA_VERSION,
};
use crate::store::organization_rows::RowsSqlError;
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::ChiefdError;

/// The `documents` store family this row set replaces.
pub const CONVERGE_SAFETY_STORE: &str = "converge-safety";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("converge-safety-rows", e)
}

fn mode_text(mode: ActuationMode) -> &'static str {
    match mode {
        ActuationMode::Shadow => "shadow",
        ActuationMode::Apply => "apply",
    }
}

fn mode_from(raw: &str) -> Result<ActuationMode, ChiefdError> {
    match raw {
        "shadow" => Ok(ActuationMode::Shadow),
        "apply" => Ok(ActuationMode::Apply),
        other => Err(corrupt_store_because(
            "converge-safety-rows",
            format!("stored actuation mode '{other}' is not 'shadow' or 'apply'"),
        )),
    }
}

/// The stored column tuple (excludes the derived `schemaVersion`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cols {
    actuation_mode: String,
    sweep_live: bool,
    budget_override: bool,
    consecutive_failures: i64,
    breaker_tripped: bool,
    breaker_tripped_at: Option<String>,
    cycle_in_progress: bool,
    cycle_started_at_ms: Option<i64>,
    last_refusal_kind: Option<String>,
    last_refusal_detail: Option<String>,
    last_refusal_at: Option<String>,
}

impl Cols {
    fn of(s: &ConvergeSafetyState) -> Self {
        Self {
            actuation_mode: mode_text(s.actuation_mode).to_string(),
            sweep_live: s.sweep_live,
            budget_override: s.budget_override,
            consecutive_failures: i64::from(s.consecutive_failures),
            breaker_tripped: s.breaker_tripped,
            breaker_tripped_at: s.breaker_tripped_at.clone(),
            cycle_in_progress: s.cycle_in_progress,
            cycle_started_at_ms: s.cycle_started_at_ms,
            last_refusal_kind: s.last_refusal.as_ref().map(|r| r.kind.clone()),
            last_refusal_detail: s.last_refusal.as_ref().map(|r| r.detail.clone()),
            last_refusal_at: s.last_refusal.as_ref().map(|r| r.at.clone()),
        }
    }
}

fn read_cols(tx: &Transaction<'_>, row_slug: &str) -> Result<Option<Cols>, ChiefdError> {
    tx.query_row(
        "SELECT actuation_mode, sweep_live, budget_override, consecutive_failures, \
         breaker_tripped, breaker_tripped_at, cycle_in_progress, cycle_started_at_ms, \
         last_refusal_kind, last_refusal_detail, last_refusal_at \
         FROM converge_safety WHERE slug = ?1",
        params![row_slug],
        |r| {
            Ok(Cols {
                actuation_mode: r.get(0)?,
                sweep_live: r.get::<_, i64>(1)? != 0,
                budget_override: r.get::<_, i64>(2)? != 0,
                consecutive_failures: r.get(3)?,
                breaker_tripped: r.get::<_, i64>(4)? != 0,
                breaker_tripped_at: r.get(5)?,
                cycle_in_progress: r.get::<_, i64>(6)? != 0,
                cycle_started_at_ms: r.get(7)?,
                last_refusal_kind: r.get(8)?,
                last_refusal_detail: r.get(9)?,
                last_refusal_at: r.get(10)?,
            })
        },
    )
    .optional()
    .map_err(store_failure)
}

/// Reconstruct the converge-safety state, or `None` when the company has no row
/// (the ordinary default — an unconfigured company reads as `default_shadow`).
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure; [`ChiefdError::Corrupt`] on an
/// unreadable actuation mode.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
) -> Result<Option<ConvergeSafetyState>, ChiefdError> {
    let Some(c) = read_cols(tx, row_slug)? else {
        return Ok(None);
    };
    let last_refusal = match (&c.last_refusal_kind, &c.last_refusal_detail, &c.last_refusal_at) {
        (Some(kind), Some(detail), Some(at)) => {
            Some(RefusalRecord { kind: kind.clone(), detail: detail.clone(), at: at.clone() })
        }
        _ => None,
    };
    Ok(Some(ConvergeSafetyState {
        schema_version: CONVERGE_SAFETY_SCHEMA_VERSION,
        actuation_mode: mode_from(&c.actuation_mode)?,
        sweep_live: c.sweep_live,
        budget_override: c.budget_override,
        consecutive_failures: u32::try_from(c.consecutive_failures).unwrap_or(0),
        breaker_tripped: c.breaker_tripped,
        breaker_tripped_at: c.breaker_tripped_at,
        cycle_in_progress: c.cycle_in_progress,
        cycle_started_at_ms: c.cycle_started_at_ms,
        last_refusal,
    }))
}

/// Publish the state from current SQLite state. Upserts the singleton row iff a
/// stored column changed; one `org_events` touch on change.
///
/// # Errors
/// SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming: &ConvergeSafetyState,
) -> Result<i64, ChiefdError> {
    let current = read_cols(tx, row_slug)?;
    let cols = Cols::of(incoming);
    // Event stamp: prefer the last-refusal instant, else the breaker-trip instant.
    let at = cols
        .last_refusal_at
        .clone()
        .or_else(|| cols.breaker_tripped_at.clone())
        .unwrap_or_default();

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        if current.as_ref() == Some(&cols) {
            return Ok(Vec::new());
        }
        tx.execute(
            "INSERT INTO converge_safety(slug, actuation_mode, sweep_live, budget_override, \
             consecutive_failures, breaker_tripped, breaker_tripped_at, cycle_in_progress, \
             cycle_started_at_ms, last_refusal_kind, last_refusal_detail, last_refusal_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) \
             ON CONFLICT(slug) DO UPDATE SET actuation_mode=?2, sweep_live=?3, budget_override=?4, \
             consecutive_failures=?5, breaker_tripped=?6, breaker_tripped_at=?7, \
             cycle_in_progress=?8, cycle_started_at_ms=?9, last_refusal_kind=?10, \
             last_refusal_detail=?11, last_refusal_at=?12",
            params![
                row_slug,
                cols.actuation_mode,
                i64::from(cols.sweep_live),
                i64::from(cols.budget_override),
                cols.consecutive_failures,
                i64::from(cols.breaker_tripped),
                cols.breaker_tripped_at,
                i64::from(cols.cycle_in_progress),
                cols.cycle_started_at_ms,
                cols.last_refusal_kind,
                cols.last_refusal_detail,
                cols.last_refusal_at,
            ],
        )?;
        Ok(vec![EventTouch::new(
            "converge-safety",
            row_slug,
            "upsert",
            "converge_safety",
            row_slug,
        )])
    })
    .map_err(|RowsSqlError(e)| e)
}

/// Fence-free CLEAR: delete the singleton row so the doc becomes ABSENT. Emits
/// one `delete` touch iff a row existed.
///
/// # Errors
/// SQL failures as [`ChiefdError::StoreFailure`].
pub fn clear(tx: &Transaction<'_>, row_slug: &str, at: &str) -> Result<(), ChiefdError> {
    let existed = read_cols(tx, row_slug)?.is_some();
    apply_and_emit::<RowsSqlError, _>(tx, row_slug, at, "", |tx| {
        if !existed {
            return Ok(Vec::new());
        }
        tx.execute("DELETE FROM converge_safety WHERE slug = ?1", params![row_slug])?;
        Ok(vec![EventTouch::new(
            "converge-safety",
            row_slug,
            "delete",
            "converge_safety",
            row_slug,
        )])
    })
    .map(|_seq| ())
    .map_err(|RowsSqlError(e)| e)
}

/// Backfill the blob body into the rows via the live publish path.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes; SQL failures propagate.
pub fn backfill_converge_safety(
    tx: &Transaction<'_>,
    row_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let state: ConvergeSafetyState =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("converge-safety-blob", e))?;
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

    fn state() -> ConvergeSafetyState {
        ConvergeSafetyState {
            schema_version: CONVERGE_SAFETY_SCHEMA_VERSION,
            actuation_mode: ActuationMode::Apply,
            sweep_live: true,
            budget_override: false,
            consecutive_failures: 2,
            breaker_tripped: true,
            breaker_tripped_at: Some("2026-07-25T06:00:00.000Z".into()),
            cycle_in_progress: false,
            cycle_started_at_ms: Some(1_784_000_000_000),
            last_refusal: Some(RefusalRecord {
                kind: "circuit-breaker".into(),
                detail: "three consecutive failures".into(),
                at: "2026-07-25T06:01:00.000Z".into(),
            }),
        }
    }

    #[test]
    fn empty_before_any_publish() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(reconstruct(&tx, "acme").unwrap(), None);
    }

    #[test]
    fn round_trips_full_state_and_derives_schema_version() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let s = state();
        publish(&tx, "acme", &s).unwrap();
        let got = reconstruct(&tx, "acme").unwrap().unwrap();
        assert_eq!(got, s);
        assert_eq!(got.schema_version, CONVERGE_SAFETY_SCHEMA_VERSION);
        assert_eq!(event_count(&tx, "acme"), 1);
    }

    #[test]
    fn round_trips_shadow_default_without_refusal() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let s = ConvergeSafetyState {
            schema_version: CONVERGE_SAFETY_SCHEMA_VERSION,
            actuation_mode: ActuationMode::Shadow,
            sweep_live: false,
            budget_override: false,
            consecutive_failures: 0,
            breaker_tripped: false,
            breaker_tripped_at: None,
            cycle_in_progress: false,
            cycle_started_at_ms: None,
            last_refusal: None,
        };
        publish(&tx, "acme", &s).unwrap();
        let got = reconstruct(&tx, "acme").unwrap().unwrap();
        assert_eq!(got, s);
        assert!(got.last_refusal.is_none());
    }

    #[test]
    fn unchanged_publish_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let seq = event_count(&tx, "acme");
        let out = publish(&tx, "acme", &state()).unwrap();
        assert_eq!(out, seq);
    }

    #[test]
    fn a_second_direct_publish_uses_current_state() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let out = publish(&tx, "acme", &state()).unwrap();
        assert_eq!(out, 1);
    }

    #[test]
    fn clear_deletes_the_row() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        clear(&tx, "acme", "2026-07-25T06:05:00.000Z").unwrap();
        assert_eq!(reconstruct(&tx, "acme").unwrap(), None);
    }

    #[test]
    fn backfill_round_trips_a_blob_body() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let blob = serde_json::to_vec(&state()).unwrap();
        backfill_converge_safety(&tx, "acme", &blob).unwrap();
        assert_eq!(reconstruct(&tx, "acme").unwrap().unwrap(), state());
    }

    #[test]
    fn slug_scoping_isolates_companies() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let mut beta = state();
        beta.actuation_mode = ActuationMode::Shadow;
        beta.breaker_tripped = false;
        beta.breaker_tripped_at = None;
        publish(&tx, "beta", &beta).unwrap();
        assert_eq!(reconstruct(&tx, "acme").unwrap().unwrap().actuation_mode, ActuationMode::Apply);
        assert_eq!(
            reconstruct(&tx, "beta").unwrap().unwrap().actuation_mode,
            ActuationMode::Shadow
        );
    }
}
