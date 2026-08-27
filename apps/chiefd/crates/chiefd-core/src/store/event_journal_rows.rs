//! The `event-journal` once-marker ROW implementation (org-data-normalization
//! P0, bucket1b — insert-if-absent multi-row store).
//!
//! The exactly-once event markers that replaced the O_EXCL+hardlink file marker
//! (`org-event-journal.ts`). One row per logical event in the slug-scoped
//! `event_once_markers` table, keyed by `(slug, key_digest)` with a
//! `UNIQUE(slug, id)`. The SQL `INSERT … ON CONFLICT DO NOTHING` is the native
//! form of the hardlink's "cannot replace a concurrent winner", so a process
//! death between the marker and the best-effort JSONL append can never duplicate
//! the logical event on retry.
//!
//! This is NOT the fenced read/publish (`org_row_route_pair!`) shape: a marker is
//! an independent atomic insert, not part of a diffed aggregate, so it emits NO
//! `org_events` touch (matching the "one winner" file semantic it replaced).
//!
//! Projection: only the closed fields required by read-back consumers persist:
//! the terminal-health-resolution slice (`thr_*`). Every other event field
//! remains existence-only and is intentionally
//! NOT stored (the full event may appear in best-effort `events.jsonl`).
//! `key_digest` is supplied by the TS client (`sha256(id)`), so chiefd needs no
//! hasher.

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ChiefdError;

/// The `org_documents` store family this row set replaces.
pub const EVENT_JOURNAL_STORE: &str = "event-journal";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("event-journal-rows", e)
}

/// One stored once-marker. Mirrors `OrganizationEventOnceMarker`; `event` is
/// reconstructed from the typed columns (id + event type + the permitted
/// `thr_*` slice).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventOnceMarker {
    /// Always `1`.
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// `sha256(id)` — the store suffix; supplied by the caller.
    #[serde(rename = "keyDigest")]
    pub key_digest: String,
    /// The reconstructed bounded event (id + type + its closed read-back fields).
    pub event: Map<String, Value>,
}

/// The typed `thr_*` projection extracted from an incoming event. Every field is
/// optional — populated only for the terminal-incident-resolution event type.
#[derive(Debug, Clone, Default)]
struct ThrCols {
    message_id: Option<String>,
    fingerprint: Option<String>,
    kind: Option<String>,
    incident_first_seen_at: Option<String>,
    recipient_person_id: Option<String>,
    accepted_at: Option<String>,
}

fn str_field(event: &Map<String, Value>, key: &str) -> Option<String> {
    event.get(key).and_then(Value::as_str).map(str::to_string)
}

fn thr_of(event: &Map<String, Value>) -> ThrCols {
    ThrCols {
        message_id: str_field(event, "messageId"),
        fingerprint: str_field(event, "fingerprint"),
        kind: str_field(event, "kind"),
        incident_first_seen_at: str_field(event, "incidentFirstSeenAt"),
        recipient_person_id: str_field(event, "recipientPersonId"),
        accepted_at: str_field(event, "acceptedAt"),
    }
}

/// Rebuild the read-back `event` object from the stored columns. Fields absent
/// from the row are absent from the object (exactly what the health-monitor
/// validator expects: it reads only these keys).
fn rebuild_event(id: &str, event_type: &str, thr: &ThrCols) -> Map<String, Value> {
    let mut event = Map::new();
    event.insert("id".into(), Value::String(id.to_string()));
    event.insert("event".into(), Value::String(event_type.to_string()));
    if let Some(v) = &thr.message_id {
        event.insert("messageId".into(), Value::String(v.clone()));
    }
    if let Some(v) = &thr.fingerprint {
        event.insert("fingerprint".into(), Value::String(v.clone()));
    }
    if let Some(v) = &thr.kind {
        event.insert("kind".into(), Value::String(v.clone()));
    }
    if let Some(v) = &thr.incident_first_seen_at {
        event.insert("incidentFirstSeenAt".into(), Value::String(v.clone()));
    }
    if let Some(v) = &thr.recipient_person_id {
        event.insert("recipientPersonId".into(), Value::String(v.clone()));
    }
    if let Some(v) = &thr.accepted_at {
        event.insert("acceptedAt".into(), Value::String(v.clone()));
    }
    event
}

/// Read one once-marker by its digest, or `None` when no marker exists.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn read_marker(
    tx: &Transaction<'_>,
    row_slug: &str,
    key_digest: &str,
) -> Result<Option<EventOnceMarker>, ChiefdError> {
    tx.query_row(
        "SELECT id, event_type, thr_message_id, thr_fingerprint, thr_kind, \
         thr_incident_first_seen_at, thr_recipient_person_id, thr_accepted_at \
         FROM event_once_markers \
         WHERE slug = ?1 AND key_digest = ?2",
        params![row_slug, key_digest],
        |r| {
            let id: String = r.get(0)?;
            let event_type: String = r.get(1)?;
            let thr = ThrCols {
                message_id: r.get(2)?,
                fingerprint: r.get(3)?,
                kind: r.get(4)?,
                incident_first_seen_at: r.get(5)?,
                recipient_person_id: r.get(6)?,
                accepted_at: r.get(7)?,
            };
            Ok(EventOnceMarker {
                schema_version: 1,
                key_digest: key_digest.to_string(),
                event: rebuild_event(&id, &event_type, &thr),
            })
        },
    )
    .optional()
    .map_err(store_failure)
}

/// The result of an insert-if-absent: whether THIS call created the marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsertOutcome {
    /// `true` only when this call wrote the row (the O_EXCL "one winner").
    pub created: bool,
}

/// Insert a once-marker if absent. Atomic `INSERT … ON CONFLICT DO NOTHING`; no
/// `org_events` touch (an independent exactly-once marker, not a diffed
/// aggregate). `event["event"]` is the required type; only its closed `thr_*`
/// slice is extracted for a read-back consumer.
/// `created_at_ms` is the caller-supplied row-write time (drives the 48h
/// reactive prune).
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn insert_if_absent(
    tx: &Transaction<'_>,
    row_slug: &str,
    key_digest: &str,
    id: &str,
    event: &Map<String, Value>,
    created_at_ms: i64,
) -> Result<InsertOutcome, ChiefdError> {
    let event_type = event.get("event").and_then(Value::as_str).unwrap_or("");
    let thr = thr_of(event);
    let changed = tx
        .execute(
            "INSERT INTO event_once_markers(slug, key_digest, id, schema_version, event_type, \
             created_at, thr_message_id, thr_fingerprint, thr_kind, thr_incident_first_seen_at, \
             thr_recipient_person_id, thr_accepted_at) \
             VALUES(?1,?2,?3,1,?4,?5,?6,?7,?8,?9,?10,?11) \
             ON CONFLICT(slug, key_digest) DO NOTHING",
            params![
                row_slug,
                key_digest,
                id,
                event_type,
                created_at_ms,
                thr.message_id,
                thr.fingerprint,
                thr.kind,
                thr.incident_first_seen_at,
                thr.recipient_person_id,
                thr.accepted_at,
            ],
        )
        .map_err(store_failure)?;
    Ok(InsertOutcome { created: changed > 0 })
}

/// Delete expired exactly-once markers for one company. The timestamp is the
/// typed `created_at` column, never a blob key prefix: retention remains a
/// bounded server-side operation after `org_documents` is retired.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn prune_older_than(
    tx: &Transaction<'_>,
    row_slug: &str,
    older_than_ms: i64,
) -> Result<usize, ChiefdError> {
    tx.execute(
        "DELETE FROM event_once_markers WHERE slug = ?1 AND created_at < ?2",
        params![row_slug, older_than_ms],
    )
    .map_err(store_failure)
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

    fn terminal_event(id: &str) -> Map<String, Value> {
        json!({
            "id": id,
            "event": "terminal-health-incident-resolved",
            "messageId": "msg-1",
            "fingerprint": "fp-1",
            "kind": "supervision.terminal",
            "incidentFirstSeenAt": "2026-07-25T06:00:00.000Z",
            "recipientPersonId": "chief",
            "acceptedAt": "2026-07-25T06:10:00.000Z",
            "recoveredFromAcceptedArchive": true
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn absent_marker_reads_none() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(read_marker(&tx, "acme", "digest-x").unwrap(), None);
    }

    #[test]
    fn insert_then_read_round_trips_the_thr_fields() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let ev = terminal_event("res-1");
        let out = insert_if_absent(&tx, "acme", "digest-1", "res-1", &ev, 1_000).unwrap();
        assert!(out.created);
        let marker = read_marker(&tx, "acme", "digest-1").unwrap().unwrap();
        assert_eq!(marker.schema_version, 1);
        assert_eq!(marker.key_digest, "digest-1");
        assert_eq!(marker.event.get("id").unwrap(), "res-1");
        assert_eq!(marker.event.get("event").unwrap(), "terminal-health-incident-resolved");
        assert_eq!(marker.event.get("messageId").unwrap(), "msg-1");
        assert_eq!(marker.event.get("fingerprint").unwrap(), "fp-1");
        assert_eq!(marker.event.get("acceptedAt").unwrap(), "2026-07-25T06:10:00.000Z");
        // Non-read-back fields are intentionally dropped.
        assert!(marker.event.get("recoveredFromAcceptedArchive").is_none());
    }

    #[test]
    fn insert_is_exactly_once() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let ev = terminal_event("res-1");
        assert!(insert_if_absent(&tx, "acme", "digest-1", "res-1", &ev, 1).unwrap().created);
        assert!(!insert_if_absent(&tx, "acme", "digest-1", "res-1", &ev, 2).unwrap().created);
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM event_once_markers WHERE slug='acme'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn non_terminal_event_stores_only_id_and_type() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let ev = json!({
            "id": "e-9", "event": "supervision-noticed", "detail": "whatever",
            "assignmentId": "must-drop", "assigneePersonId": "must-drop"
        })
        .as_object()
        .unwrap()
        .clone();
        insert_if_absent(&tx, "acme", "d9", "e-9", &ev, 1).unwrap();
        let marker = read_marker(&tx, "acme", "d9").unwrap().unwrap();
        assert_eq!(marker.event.get("event").unwrap(), "supervision-noticed");
        assert!(marker.event.get("messageId").is_none());
        assert!(marker.event.get("detail").is_none()); // existence-only; not stored
        assert!(marker.event.get("assignmentId").is_none());
        assert!(marker.event.get("assigneePersonId").is_none());
    }

    #[test]
    fn slug_scoping_isolates_companies_in_a_shared_db() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        // Same digest+id under two companies must not collide.
        assert!(insert_if_absent(&tx, "acme", "d", "e", &terminal_event("e"), 1).unwrap().created);
        assert!(insert_if_absent(&tx, "beta", "d", "e", &terminal_event("e"), 1).unwrap().created);
        assert!(read_marker(&tx, "acme", "d").unwrap().is_some());
        assert!(read_marker(&tx, "beta", "d").unwrap().is_some());
        assert!(read_marker(&tx, "gamma", "d").unwrap().is_none());
    }

    #[test]
    fn prune_removes_only_expired_markers_for_the_named_company() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let event = terminal_event("old");
        insert_if_absent(&tx, "acme", "old", "old", &event, 10).unwrap();
        insert_if_absent(&tx, "acme", "new", "new", &event, 100).unwrap();
        insert_if_absent(&tx, "beta", "old", "old", &event, 10).unwrap();
        assert_eq!(prune_older_than(&tx, "acme", 50).unwrap(), 1);
        assert!(read_marker(&tx, "acme", "old").unwrap().is_none());
        assert!(read_marker(&tx, "acme", "new").unwrap().is_some());
        assert!(read_marker(&tx, "beta", "old").unwrap().is_some());
    }
}
