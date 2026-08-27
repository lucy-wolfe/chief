//! The exactly-once event marker's policy half — port of the policy in
//! `apps/cli/src/legacy/organization/org-event-journal.ts`. Storage is
//! [`crate::store::event_journal_rows`] (`read_marker`, `insert_if_absent`,
//! `prune_older_than`); this module owns the digest, the retention window,
//! and the reactive sweep throttle.
//!
//! ## Retention is reactive, not a timer
//!
//! A once-marker is an exactly-once receipt (incident/resolution ids,
//! acknowledgement receipts, one-shot migrations); a receipt older than
//! [`JOURNAL_MARKER_RETENTION_MS`] can never gate a re-fire that still
//! matters, so the table is a 48h rolling window rather than an unbounded
//! accumulator. The sweep is driven by marker CREATION — the only thing that
//! grows the table — and throttled to at most one bounded server-side
//! `DELETE` per [`JOURNAL_MARKER_SWEEP_THROTTLE_MS`] per company, so a
//! healthy company that writes markers prunes itself and a silent one costs
//! nothing. Mandate 1 forbids a timer sweep; this stays reactive.
//!
//! ## What changed in the port
//!
//! The TS original threw the throttle stamp in a process-local
//! `Map<slug, lastSweptMs>` — a Mandate 2 violation (all state belongs in
//! SQLite, not process memory: a restart or a second chiefd process would
//! have no idea a sweep just ran). Here the stamp is the
//! `event_journal_sweep` singleton row (one per company, see
//! `schema.rs`), read and written in the SAME transaction as the marker
//! insert and the prune it may trigger — stamped BEFORE the prune runs so a
//! persistently failing prune can never hot-loop the DELETE.
//!
//! **Not ported**: the TS `events.jsonl` best-effort append
//! (`appendBoundedLine`, its 128 MB cap, and its one-generation rotation).
//! That was a disk spill file living outside Pi's home, which Mandate 5
//! forbids outright. The marker row created by [`append_event_once`] is the
//! sole durable authority; there is no second, file-backed projection to keep
//! in sync with it.
//!
//! The TS client hashed `id` into the digest itself before calling chiefd;
//! here chiefd owns the hash, using the same `sha2` dependency
//! [`crate::store::model_change`] already uses for its own request digest, so
//! no caller can drift from the canonical `sha256(id)` key.

use crate::hexdigest::hex_digest;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::store::event_journal_rows;
use crate::ChiefdError;

/// A once-marker receipt older than this can never gate a re-fire that still
/// matters, byte-identical to the TS `JOURNAL_MARKER_RETENTION_MS`.
pub const JOURNAL_MARKER_RETENTION_MS: i64 = 48 * 60 * 60 * 1000;

/// At most one bounded sweep `DELETE` per company per this interval,
/// byte-identical to the TS `JOURNAL_MARKER_SWEEP_THROTTLE_MS`.
pub const JOURNAL_MARKER_SWEEP_THROTTLE_MS: i64 = 60 * 60 * 1000;

/// Whether [`append_event_once`] created the marker (the O_EXCL "one winner"
/// the SQL `INSERT ... ON CONFLICT DO NOTHING` implements).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventOnceOutcome {
    /// `true` only when THIS call wrote the row.
    pub created: bool,
}

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("event-journal-sweep", e)
}

/// The company's last-swept wall-clock stamp, or `None` when this company has
/// never swept. `None` — not `0` — is the "never swept" sentinel here; the TS
/// original used a `0` default read out of a `Map`, which this typed column
/// has no need to reproduce.
fn read_last_swept_ms(tx: &Transaction<'_>, slug: &str) -> Result<Option<i64>, ChiefdError> {
    tx.query_row(
        "SELECT last_swept_at_ms FROM event_journal_sweep WHERE slug = ?1",
        params![slug],
        |row| row.get(0),
    )
    .optional()
    .map_err(store_failure)
}

fn stamp_swept_ms(tx: &Transaction<'_>, slug: &str, now_ms: i64) -> Result<(), ChiefdError> {
    tx.execute(
        "INSERT INTO event_journal_sweep(slug, last_swept_at_ms) VALUES(?1, ?2) \
         ON CONFLICT(slug) DO UPDATE SET last_swept_at_ms = excluded.last_swept_at_ms",
        params![slug, now_ms],
    )
    .map_err(store_failure)?;
    Ok(())
}

/// Prune this company's expired markers, at most once per
/// [`JOURNAL_MARKER_SWEEP_THROTTLE_MS`]. The throttle stamp is written BEFORE
/// the prune runs (not after), so a prune that itself errors — which, inside
/// this same transaction, rolls the stamp write back with it — can never
/// leave the stamp advanced without the corresponding prune having at least
/// been attempted in the same atomic unit; there is no window where a
/// half-completed sweep persists as "done".
fn maybe_sweep(tx: &Transaction<'_>, slug: &str, now_ms: i64) -> Result<(), ChiefdError> {
    if let Some(last) = read_last_swept_ms(tx, slug)? {
        if now_ms.saturating_sub(last) < JOURNAL_MARKER_SWEEP_THROTTLE_MS {
            return Ok(());
        }
    }
    stamp_swept_ms(tx, slug, now_ms)?;
    event_journal_rows::prune_older_than(tx, slug, now_ms - JOURNAL_MARKER_RETENTION_MS)?;
    Ok(())
}

/// Record one exactly-once event marker. Hashes `id` with SHA-256 into the
/// row's `key_digest`, inserts it if absent, and — ONLY when this call
/// created the marker — runs the throttled retention sweep, all in the same
/// transaction. A duplicate `id` (marker already exists) never sweeps: sweep
/// activity is driven strictly by table growth, matching the TS reactive
/// contract.
///
/// # Errors
/// Any [`ChiefdError`] the underlying row insert or prune produces.
pub fn append_event_once(
    tx: &Transaction<'_>,
    slug: &str,
    id: &str,
    event: &Map<String, Value>,
    now_ms: i64,
) -> Result<EventOnceOutcome, ChiefdError> {
    let key_digest = hex_digest(Sha256::digest(id.as_bytes()));
    let outcome = event_journal_rows::insert_if_absent(tx, slug, &key_digest, id, event, now_ms)?;
    if outcome.created {
        maybe_sweep(tx, slug, now_ms)?;
    }
    Ok(EventOnceOutcome { created: outcome.created })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("schema");
        conn
    }

    fn event(id: &str) -> Map<String, Value> {
        json!({
            "id": id,
            "event": "terminal-health-incident-resolved",
            "messageId": "msg-1",
            "fingerprint": "fp-1",
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn last_swept(tx: &Transaction<'_>, slug: &str) -> Option<i64> {
        read_last_swept_ms(tx, slug).unwrap()
    }

    #[test]
    fn a_marker_is_created_exactly_once() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let first = append_event_once(&tx, "acme", "res-1", &event("res-1"), 1_000).unwrap();
        assert!(first.created);
        let second = append_event_once(&tx, "acme", "res-1", &event("res-1"), 2_000).unwrap();
        assert!(!second.created, "the same id must not create a second marker");
    }

    #[test]
    fn the_key_digest_is_sha256_of_the_id() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        append_event_once(&tx, "acme", "res-1", &event("res-1"), 1_000).unwrap();
        let expected_digest = hex_digest(Sha256::digest(b"res-1"));
        let marker = event_journal_rows::read_marker(&tx, "acme", &expected_digest)
            .unwrap()
            .expect("marker readable at the sha256(id) digest");
        assert_eq!(marker.event.get("id").unwrap(), "res-1");
    }

    #[test]
    fn the_sweep_runs_only_on_the_creating_call() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(last_swept(&tx, "acme"), None, "no sweep before any marker exists");
        append_event_once(&tx, "acme", "res-1", &event("res-1"), 1_000).unwrap();
        assert_eq!(last_swept(&tx, "acme"), Some(1_000), "creation sweeps and stamps");
        // A duplicate id does not create a marker, so it must not re-sweep even
        // though plenty of (simulated) time has passed.
        let far_future = 1_000 + JOURNAL_MARKER_SWEEP_THROTTLE_MS * 10;
        append_event_once(&tx, "acme", "res-1", &event("res-1"), far_future).unwrap();
        assert_eq!(last_swept(&tx, "acme"), Some(1_000), "a non-creating call never sweeps");
    }

    #[test]
    fn the_sweep_is_throttled_to_once_per_interval() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        append_event_once(&tx, "acme", "res-1", &event("res-1"), 0).unwrap();
        assert_eq!(last_swept(&tx, "acme"), Some(0));
        // A second, distinct marker inside the throttle window still creates
        // its own row, but must not advance the sweep stamp.
        let still_throttled = JOURNAL_MARKER_SWEEP_THROTTLE_MS - 1;
        let outcome =
            append_event_once(&tx, "acme", "res-2", &event("res-2"), still_throttled).unwrap();
        assert!(outcome.created);
        assert_eq!(last_swept(&tx, "acme"), Some(0), "still inside the throttle window");
        // Once the throttle interval has fully elapsed, the next creating call
        // sweeps again.
        let past_throttle = JOURNAL_MARKER_SWEEP_THROTTLE_MS;
        append_event_once(&tx, "acme", "res-3", &event("res-3"), past_throttle).unwrap();
        assert_eq!(last_swept(&tx, "acme"), Some(past_throttle));
    }

    #[test]
    fn a_throttled_sweep_still_prunes_expired_markers() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        // An old marker, now past the 48h retention window.
        append_event_once(&tx, "acme", "old", &event("old"), 0).unwrap();
        let expired_at = JOURNAL_MARKER_RETENTION_MS + JOURNAL_MARKER_SWEEP_THROTTLE_MS;
        append_event_once(&tx, "acme", "trigger", &event("trigger"), expired_at).unwrap();
        let old_digest = hex_digest(Sha256::digest(b"old"));
        assert!(
            event_journal_rows::read_marker(&tx, "acme", &old_digest).unwrap().is_none(),
            "the expired marker was pruned by the triggered sweep"
        );
        let trigger_digest = hex_digest(Sha256::digest(b"trigger"));
        assert!(
            event_journal_rows::read_marker(&tx, "acme", &trigger_digest).unwrap().is_some(),
            "the fresh marker that triggered the sweep survives its own sweep"
        );
    }

    #[test]
    fn the_sweep_throttle_is_scoped_per_company() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        append_event_once(&tx, "acme", "a1", &event("a1"), 500).unwrap();
        assert_eq!(last_swept(&tx, "acme"), Some(500));
        assert_eq!(last_swept(&tx, "beta"), None, "a sibling company's throttle is untouched");
        append_event_once(&tx, "beta", "b1", &event("b1"), 999_999).unwrap();
        assert_eq!(last_swept(&tx, "beta"), Some(999_999));
        assert_eq!(
            last_swept(&tx, "acme"),
            Some(500),
            "acme's stamp is unaffected by beta's sweep"
        );
    }
}
