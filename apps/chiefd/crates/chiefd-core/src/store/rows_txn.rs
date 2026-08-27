//! Shared row-transaction scaffold (org-data-normalization P0, N2).
//!
//! The fan-out gate. Every normalized store port (N2 manifest, N3 supervision,
//! N4 activity, N5 session-maintenance, N6 memory, N7 journals) publishes the
//! same way: ONE `BEGIN IMMEDIATE` transaction that (1) fences on the company's
//! `org_events` seq, (2) diffs the incoming aggregate against the current rows
//! and writes ONLY the touched rows, (3) appends one `org_events` row PER
//! TOUCHED ENTITY (entity granularity, `detail_ref = 'table:pk'`), allocating
//! each seq from the D2 per-slug counter row, and (4) commits.
//!
//! This module owns steps (1), (3), (4) and the seq allocator — the generic
//! machinery — so a port supplies only its own typed diff (step 2). It shares
//! MACHINERY, never a route: each store keeps its own DTO + diff + validators
//! (the typed boundary is gate-3). The chiefd runs raw SQL only on a company's
//! dedicated writer thread; callers reach this through
//! `CompanyDb::in_transaction`, which hands the `&Transaction` these helpers
//! take.
//!
//! D2 (fable-arch, FROZEN): seq is a per-slug `counters` row bumped in the SAME
//! transaction — NEVER `MAX(seq)+1`, NEVER `AUTOINCREMENT`. The fence read is
//! `MAX(org_events.seq)`; allocation is the counter. The two stay in lockstep
//! by construction (every event uses one allocated seq), and SQLite's
//! single-writer lock makes commit order == seq order, so the feed is gap-free
//! and totally ordered per slug (kills the #286 seq-race at the source).

use rusqlite::{params, Transaction};

/// The `counters` key for a company's `org_events` seq (D2). One dedicated row
/// per slug; bumped once per touched entity inside the publish transaction.
#[must_use]
pub fn org_events_counter_key(slug: &str) -> String {
    format!("org-events:{slug}")
}

/// The `counters` key for a company's `staffing_history` seq (D2). The staffing
/// ledger allocates its `seq` the SAME way as `org_events`, from its own
/// dedicated counter row so the two feeds never share a sequence.
#[must_use]
pub fn staffing_counter_key(slug: &str) -> String {
    format!("staffing:{slug}")
}

/// Allocate the next value of a per-slug counter row, bumped in THIS
/// transaction. The upsert-then-select idiom is deliberate (never
/// `RETURNING`).
///
/// # Errors
/// Propagates any `rusqlite` failure from the upsert or the read-back.
pub fn allocate_seq(tx: &Transaction<'_>, counter_key: &str) -> rusqlite::Result<i64> {
    tx.execute(
        "INSERT INTO counters(name, value) VALUES(?1, 1) \
         ON CONFLICT(name) DO UPDATE SET value = value + 1",
        params![counter_key],
    )?;
    tx.query_row("SELECT value FROM counters WHERE name = ?1", params![counter_key], |row| {
        row.get(0)
    })
}

/// The current immutable audit cursor for a company: `MAX(org_events.seq)`, or
/// `0` when the feed is empty (a company created but never mutated). It is
/// observable history only and never a mutation precondition.
///
/// # Errors
/// Propagates any `rusqlite` failure from the query.
pub fn current_seq(tx: &Transaction<'_>, slug: &str) -> rusqlite::Result<i64> {
    tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM org_events WHERE slug = ?1",
        params![slug],
        |row| row.get(0),
    )
}

/// One entity touched by a publish — the unit of `org_events` granularity.
///
/// `entity`/`op` are controlled labels (a person id or a department id is the
/// `entity_id`); `detail_ref` is the `'table:pk'` back-reference to the row that
/// changed (e.g. `"people:acme/ada"`), NEVER inline structure — the detail is
/// the row it points at, read separately (delta #9).
#[derive(Debug, Clone)]
pub struct EventTouch {
    /// The entity family, e.g. `"department"`, `"person"`, `"org"`.
    pub entity: String,
    /// The touched entity's id.
    pub entity_id: String,
    /// What happened, e.g. `"upsert"`, `"delete"`, `"reorder"`.
    pub op: String,
    /// `'table:pk'` reference to the changed row, or `None` for a whole-company op.
    pub detail_ref: Option<String>,
}

impl EventTouch {
    /// A touch whose `detail_ref` is `'<table>:<slug>/<entity_id>'`.
    #[must_use]
    pub fn new(
        entity: impl Into<String>,
        entity_id: impl Into<String>,
        op: impl Into<String>,
        table: &str,
        slug: &str,
    ) -> Self {
        let entity_id = entity_id.into();
        Self {
            detail_ref: Some(format!("{table}:{slug}/{entity_id}")),
            entity: entity.into(),
            entity_id,
            op: op.into(),
        }
    }
}

/// The fence-FREE diff-write core shared by two entry points (org-data-
/// normalization P0): it runs the port's typed diff (`apply`), appends
/// one `org_events` row per touched entity (each with a freshly allocated
/// per-slug D2 seq from the counter row), and returns the new max seq (the
/// unchanged fence when the diff touched nothing).
///
/// # Errors
/// `E` from `apply` (a domain refusal), or any `rusqlite` failure lifted into
/// `E` via its `From<rusqlite::Error>` bound.
pub fn apply_and_emit<E, F>(
    tx: &Transaction<'_>,
    slug: &str,
    at: &str,
    actor: &str,
    apply: F,
) -> Result<i64, E>
where
    E: From<rusqlite::Error>,
    F: FnOnce(&Transaction<'_>) -> Result<Vec<EventTouch>, E>,
{
    let touches = apply(tx)?;
    let counter_key = org_events_counter_key(slug);
    let mut last = current_seq(tx, slug)?;
    for touch in touches {
        let seq = allocate_seq(tx, &counter_key)?;
        tx.execute(
            "INSERT INTO org_events(slug, seq, entity, entity_id, op, actor, at, detail_ref) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                slug,
                seq,
                touch.entity,
                touch.entity_id,
                touch.op,
                actor,
                at,
                touch.detail_ref,
            ],
        )?;
        last = seq;
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// The minimum DDL these helpers touch (subset of `COMPANY_SCHEMA_SQL`).
    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            "CREATE TABLE counters(name TEXT PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE org_events(slug TEXT NOT NULL, seq INTEGER NOT NULL, \
               entity TEXT NOT NULL, entity_id TEXT NOT NULL, op TEXT NOT NULL, \
               actor TEXT NOT NULL DEFAULT '', at TEXT NOT NULL, detail_ref TEXT, \
               PRIMARY KEY (slug, seq));",
        )
        .expect("ddl");
        conn
    }

    #[test]
    fn allocate_seq_starts_at_one_and_increments_per_slug() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let key = org_events_counter_key("acme");
        assert_eq!(allocate_seq(&tx, &key).unwrap(), 1);
        assert_eq!(allocate_seq(&tx, &key).unwrap(), 2);
        // A different slug's counter is independent.
        assert_eq!(allocate_seq(&tx, &org_events_counter_key("beta")).unwrap(), 1);
        assert_eq!(allocate_seq(&tx, &key).unwrap(), 3);
        tx.commit().unwrap();
    }

    #[test]
    fn apply_and_emit_advances_without_a_mutation_fence() {
        // The single-writer path emits one org_events row per touch and keeps
        // advancing across calls in the same transaction.
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let seq = apply_and_emit::<rusqlite::Error, _>(&tx, "acme", "t0", "tester", |_tx| {
            Ok(vec![
                EventTouch::new("person", "ada", "upsert", "people", "acme"),
                EventTouch::new("person", "bob", "upsert", "people", "acme"),
            ])
        })
        .unwrap();
        assert_eq!(seq, 2, "two touches allocate seq 1 and 2, returns the max");
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let seq2 = apply_and_emit::<rusqlite::Error, _>(&tx, "acme", "t1", "tester", |_tx| {
            Ok(vec![EventTouch::new("person", "cy", "upsert", "people", "acme")])
        })
        .unwrap();
        assert_eq!(seq2, 3, "keeps advancing with no fence check");
        // A touch-free apply returns the unchanged max seq.
        let seq3 =
            apply_and_emit::<rusqlite::Error, _>(&tx, "acme", "t2", "tester", |_tx| Ok(vec![]))
                .unwrap();
        assert_eq!(seq3, 3, "no touches -> unchanged fence");
        tx.commit().unwrap();
    }

    #[test]
    fn current_seq_is_zero_before_any_event() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(current_seq(&tx, "acme").unwrap(), 0);
        tx.commit().unwrap();
    }

    #[test]
    fn apply_writes_one_event_per_touched_entity_and_advances_the_audit_cursor() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = apply_and_emit::<rusqlite::Error, _>(
            &tx,
            "acme",
            "2026-07-25T00:00:00.000Z",
            "chief",
            |_tx| {
                Ok(vec![
                    EventTouch::new("person", "ada", "upsert", "people", "acme"),
                    EventTouch::new("department", "executive", "upsert", "departments", "acme"),
                ])
            },
        )
        .unwrap();
        assert_eq!(out, 2);
        assert_eq!(current_seq(&tx, "acme").unwrap(), 2);
        let (entity, detail): (String, String) = tx
            .query_row(
                "SELECT entity, detail_ref FROM org_events WHERE slug='acme' AND seq=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(entity, "person");
        assert_eq!(detail, "people:acme/ada");
        tx.commit().unwrap();
    }

    #[test]
    fn a_no_op_apply_keeps_the_audit_cursor() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out =
            apply_and_emit::<rusqlite::Error, _>(&tx, "acme", "t", "", |_| Ok(vec![])).unwrap();
        assert_eq!(out, 0);
        tx.commit().unwrap();
    }
}
