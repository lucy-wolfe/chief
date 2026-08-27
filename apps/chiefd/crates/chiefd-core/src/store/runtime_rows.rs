//! The `runtime` ROW implementation (org-data-normalization P0, bucket1b —
//! authoritative full-typed 5-table port).
//!
//! The runtime projection document (`org-runtime.ts`, store `runtime`) —
//! AUTHORITATIVE (schema delta #29: every key has a cross-actor reader; the
//! carry-forward recovery/recon fields are not recomputable) — as one slug-keyed
//! scalar row plus three child tables: `runtime_process_handles` (person→the
//! actuator's process handle), `runtime_monitor_warnings` (ordered),
//! `runtime_recovery_people` (ordered, `kind` = missing|unexpected). DERIVED,
//! never stored: none — every field the TS writes is modeled.
//!
//! The TS side treats the doc as an untyped `Record`; this struct pins its real
//! shape (StableRuntimeProjection + the writeRuntimeState wrapper + the optional
//! `reconciliation` sub-object + `startupAdmissionUntil`).
//!
//! Item D: publish REJECTS any serde-flatten `extra` (doc- or reconciliation-
//! level) with [`UNMODELED_KEYS`].

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::Refusal;
use crate::store::organization_rows::{RowsSqlError, UNMODELED_KEYS};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::ChiefdError;

/// The `org_documents` store family this row set replaces.
pub const RUNTIME_STORE: &str = "runtime";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("runtime-rows", e)
}

/// The in-progress reconciliation sub-object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeReconciliation {
    /// Reconciliation lifecycle phase.
    pub phase: String,
    /// ISO-8601 time the reconciliation began.
    pub started_at: String,
    #[serde(flatten)]
    /// Forward-compatible unmodeled fields retained while decoding.
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The whole `runtime` projection doc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeState {
    /// Serialized document schema version.
    pub version: u32,
    /// Not a real runtime field in production (the doc has no `organization`);
    /// accepted-and-ignored so a test/legacy fixture that carries it is not
    /// item-D rejected. Never stored, never reconstructed.
    #[serde(default, skip_serializing)]
    pub organization: Option<String>,
    /// ISO-8601 observation time.
    pub observed_at: String,
    /// RETIRED (AC6). The runtime session name — `org-<slug>` — was a real
    /// `session NOT NULL` column, and it stored the same string the company
    /// slug already implies. Its ONE reader compared it against
    /// `manifest.runtime_session`, i.e. against another derivation from the
    /// same slug, so the column could not disagree with anything.
    ///
    /// Kept on the struct as accepted-and-ignored, exactly like
    /// [`RuntimeState::organization`] above and for the same reason: a legacy
    /// or test fixture that still carries the key must not be item-D rejected.
    /// Never stored, never reconstructed, never serialized.
    #[serde(default, skip_serializing)]
    pub session: Option<String>,
    /// Runtime socket identity observed by the runtime.
    pub socket_name: String,
    /// Current runtime status string.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Startup admission deadline, if a bounded ramp is active.
    pub startup_admission_until: Option<String>,
    // TOMBSTONE (chief-home-is-cwd §4c): `startup_ceo_admission_debt:
    // Option<bool>` sat here — "one CEO ramp step is owed to the next non-CEO
    // admission batch", set when the DAEMON admitted the CEO on its own boot.
    // The daemon boots nobody now, so the debt can never be incurred and its
    // column is gone from the schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Digest identifying a recovery observation, if any.
    pub recovery_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// ISO-8601 time the recovery state was observed.
    pub recovery_observed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Whether the recovery observation is confirmed.
    pub recovery_confirmed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Human-legible recovery state, if reported.
    pub recovery: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// In-progress reconciliation metadata, if a reconciliation is active.
    pub reconciliation: Option<RuntimeReconciliation>,
    /// person → the actuator's process handle: the pid as a decimal string, or
    /// the EMPTY STRING when the actuator proved the person alive without
    /// reading a pid. The KEY SET is the load-bearing half — a key means
    /// "alive" — and the value is a diagnostic. NEVER a tmux pane id: chiefd
    /// has held none since #751, and this map was called `panes` while holding
    /// pids, which is exactly what a reader believed when it validated the
    /// values as `%\d+` and refused every real payload.
    ///
    /// # NOT CLOSED, and the reason is a product dependency rather than effort
    ///
    /// This is an OBSERVED fact and the direction that carries it is barred, so
    /// the door should be shut. It is not, because this field still has LIVE
    /// READERS that have
    /// nothing else to read: `organization-intercom.ts`'s `org_roster` runtime
    /// projection (person state, the rendered pid, and four throw-invariants
    /// keyed on this map) and `RuntimeWake.ts`'s wake gate, which degrades to
    /// waking everybody on every message without it. Refusing the field on
    /// decode would not merely close a hole; it would strand both, and neither
    /// can be repaired by any value chiefd is entitled to put here. They need
    /// RE-FOUNDING on the desired set, which is a separate ticket and a
    /// different size of change.
    ///
    /// What IS closed, so the gap is bounded rather than open-ended: the only
    /// Rust writer that could ever have filled this map
    /// (`runtime_publish_observation` / `publish_observation`) is deleted, and
    /// every writer that remains writes it EMPTY. So the map is already always
    /// `{}` in practice; what stays open is that a caller could still POST a
    /// non-empty one to `/v1/org/runtime/publish` and have it stored.
    #[serde(default)]
    pub process_handles: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Ordered monitor warnings emitted by the runtime.
    pub monitor_warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Durable people missing from the observed runtime.
    pub missing_durable_person_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Observed people absent from durable state.
    pub unexpected_observed_person_ids: Vec<String>,
    #[serde(flatten)]
    /// Forward-compatible unmodeled fields retained while decoding.
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The scalar columns of the runtime row (everything but the child tables).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScalarCols {
    version: i64,
    observed_at: String,
    socket_name: String,
    status: String,
    startup_admission_until: Option<String>,
    recovery_fingerprint: Option<String>,
    recovery_observed_at: Option<String>,
    recovery_confirmed: Option<bool>,
    recovery: Option<String>,
    recon_phase: Option<String>,
    recon_started_at: Option<String>,
}

impl ScalarCols {
    fn of(s: &RuntimeState) -> Self {
        Self {
            version: i64::from(s.version),
            observed_at: s.observed_at.clone(),
            socket_name: s.socket_name.clone(),
            status: s.status.clone(),
            startup_admission_until: s.startup_admission_until.clone(),
            recovery_fingerprint: s.recovery_fingerprint.clone(),
            recovery_observed_at: s.recovery_observed_at.clone(),
            recovery_confirmed: s.recovery_confirmed,
            recovery: s.recovery.clone(),
            recon_phase: s.reconciliation.as_ref().map(|r| r.phase.clone()),
            recon_started_at: s.reconciliation.as_ref().map(|r| r.started_at.clone()),
        }
    }
}

fn read_scalars(tx: &Transaction<'_>, slug: &str) -> Result<Option<ScalarCols>, ChiefdError> {
    tx.query_row(
        "SELECT version, observed_at, socket_name, status, \
         startup_admission_until, recovery_fingerprint, \
         recovery_observed_at, recovery_confirmed, recovery, recon_phase, \
         recon_started_at FROM runtime WHERE slug = ?1",
        params![slug],
        |r| {
            Ok(ScalarCols {
                version: r.get(0)?,
                observed_at: r.get(1)?,
                socket_name: r.get(2)?,
                status: r.get(3)?,
                startup_admission_until: r.get(4)?,
                recovery_fingerprint: r.get(5)?,
                recovery_observed_at: r.get(6)?,
                recovery_confirmed: r.get::<_, Option<i64>>(7)?.map(|v| v != 0),
                recovery: r.get(8)?,
                recon_phase: r.get(9)?,
                recon_started_at: r.get(10)?,
            })
        },
    )
    .optional()
    .map_err(store_failure)
}

fn read_map(
    tx: &Transaction<'_>,
    slug: &str,
    sql: &str,
) -> Result<BTreeMap<String, String>, ChiefdError> {
    let mut stmt = tx.prepare(sql).map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(store_failure)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (k, v) = row.map_err(store_failure)?;
        map.insert(k, v);
    }
    Ok(map)
}

fn read_process_handle_rows(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<BTreeMap<String, String>, ChiefdError> {
    let mut stmt = tx
        .prepare("SELECT person, process_handle FROM runtime_process_handles WHERE slug = ?1")
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(store_failure)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (person, process_handle) = row.map_err(store_failure)?;
        map.insert(person, process_handle);
    }
    Ok(map)
}

fn read_warnings(tx: &Transaction<'_>, slug: &str) -> Result<Vec<String>, ChiefdError> {
    let mut stmt = tx
        .prepare("SELECT warning FROM runtime_monitor_warnings WHERE slug = ?1 ORDER BY seq")
        .map_err(store_failure)?;
    let rows = stmt.query_map(params![slug], |r| r.get::<_, String>(0)).map_err(store_failure)?;
    rows.collect::<Result<_, _>>().map_err(store_failure)
}

fn read_recovery_people(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<(Vec<String>, Vec<String>), ChiefdError> {
    let mut stmt = tx
        .prepare("SELECT kind, person FROM runtime_recovery_people WHERE slug = ?1 ORDER BY seq")
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(store_failure)?;
    let mut missing = Vec::new();
    let mut unexpected = Vec::new();
    for row in rows {
        let (kind, person) = row.map_err(store_failure)?;
        if kind == "missing" {
            missing.push(person);
        } else {
            unexpected.push(person);
        }
    }
    Ok((missing, unexpected))
}

/// Reconstruct the runtime doc, or `None` when the company has no runtime row.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
) -> Result<Option<RuntimeState>, ChiefdError> {
    let Some(c) = read_scalars(tx, row_slug)? else {
        return Ok(None);
    };
    let process_handles = read_process_handle_rows(tx, row_slug)?;
    let monitor_warnings = read_warnings(tx, row_slug)?;
    let (missing_durable_person_ids, unexpected_observed_person_ids) =
        read_recovery_people(tx, row_slug)?;
    let reconciliation = c.recon_phase.clone().map(|phase| RuntimeReconciliation {
        phase,
        started_at: c.recon_started_at.clone().unwrap_or_default(),
        extra: BTreeMap::new(),
    });
    Ok(Some(RuntimeState {
        version: u32::try_from(c.version).unwrap_or(1),
        organization: None,
        observed_at: c.observed_at,
        session: None,
        socket_name: c.socket_name,
        status: c.status,
        startup_admission_until: c.startup_admission_until,
        recovery_fingerprint: c.recovery_fingerprint,
        recovery_observed_at: c.recovery_observed_at,
        recovery_confirmed: c.recovery_confirmed,
        recovery: c.recovery,
        reconciliation,
        process_handles,
        monitor_warnings,
        missing_durable_person_ids,
        unexpected_observed_person_ids,
        extra: BTreeMap::new(),
    }))
}

/// Publish the runtime doc from current SQLite state. Upserts the scalar row if
/// changed and diffs the child tables; one `org_events` touch per changed entity.
///
/// # Errors
/// [`UNMODELED_KEYS`] refusal (422); SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming: &RuntimeState,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    let at = incoming.observed_at.clone();
    let cur_scalars = read_scalars(tx, row_slug)?;
    let cur_process_handle_rows = read_process_handle_rows(tx, row_slug)?;
    let cur_warnings = read_warnings(tx, row_slug)?;
    let (cur_missing, cur_unexpected) = read_recovery_people(tx, row_slug)?;
    let incoming_scalars = ScalarCols::of(incoming);

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        let mut touches = Vec::new();

        if cur_scalars.as_ref() != Some(&incoming_scalars) {
            let s = &incoming_scalars;
            tx.execute(
                "INSERT INTO runtime(slug, version, observed_at, \
                 socket_name, status, startup_admission_until, \
                 recovery_fingerprint, recovery_observed_at, recovery_confirmed, recovery, recon_phase, \
                 recon_started_at) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) \
                 ON CONFLICT(slug) DO UPDATE SET version=?2, observed_at=?3, \
                 socket_name=?4, status=?5, startup_admission_until=?6, \
                 recovery_fingerprint=?7, \
                 recovery_observed_at=?8, recovery_confirmed=?9, recovery=?10, \
                 recon_phase=?11, recon_started_at=?12",
                params![
                    row_slug, s.version, s.observed_at, s.socket_name, s.status,
                    s.startup_admission_until,
                    s.recovery_fingerprint, s.recovery_observed_at, s.recovery_confirmed.map(i64::from),
                    s.recovery, s.recon_phase, s.recon_started_at,
                ],
            )?;
            touches.push(EventTouch::new("runtime", row_slug, "upsert", "runtime", row_slug));
        }

        // process handles (one row per person)
        diff_process_handle_rows(tx, row_slug, &cur_process_handle_rows, &incoming.process_handles, &mut touches)?;
        // monitor warnings (ordered)
        if cur_warnings != incoming.monitor_warnings {
            tx.execute("DELETE FROM runtime_monitor_warnings WHERE slug=?1", params![row_slug])?;
            for (i, w) in incoming.monitor_warnings.iter().enumerate() {
                tx.execute(
                    "INSERT INTO runtime_monitor_warnings(slug, seq, warning) VALUES(?1,?2,?3)",
                    params![row_slug, i as i64, w],
                )?;
            }
            touches.push(EventTouch::new("runtime-monitor-warnings", row_slug, "upsert", "runtime_monitor_warnings", row_slug));
        }

        // recovery people (ordered: missing then unexpected)
        if cur_missing != incoming.missing_durable_person_ids || cur_unexpected != incoming.unexpected_observed_person_ids {
            tx.execute("DELETE FROM runtime_recovery_people WHERE slug=?1", params![row_slug])?;
            let mut seq = 0i64;
            for p in &incoming.missing_durable_person_ids {
                tx.execute("INSERT INTO runtime_recovery_people(slug, seq, kind, person) VALUES(?1,?2,'missing',?3)", params![row_slug, seq, p])?;
                seq += 1;
            }
            for p in &incoming.unexpected_observed_person_ids {
                tx.execute("INSERT INTO runtime_recovery_people(slug, seq, kind, person) VALUES(?1,?2,'unexpected',?3)", params![row_slug, seq, p])?;
                seq += 1;
            }
            touches.push(EventTouch::new("runtime-recovery-people", row_slug, "upsert", "runtime_recovery_people", row_slug));
        }

        Ok(touches)
    })
    .map_err(|RowsSqlError(e)| e)
}

/// Diff `runtime_process_handles`, the ONLY string-keyed child map this row
/// still has.
///
/// It had a generic `diff_string_map` sibling while `runtime_windows` existed;
/// with that table deleted the generic one had no second caller and went with
/// it.
fn diff_process_handle_rows(
    tx: &Transaction<'_>,
    row_slug: &str,
    current: &BTreeMap<String, String>,
    incoming_process_handles: &BTreeMap<String, String>,
    touches: &mut Vec<EventTouch>,
) -> Result<(), RowsSqlError> {
    let cur_keys: BTreeSet<&String> = current.keys().collect();
    let inc_keys: BTreeSet<&String> = incoming_process_handles.keys().collect();
    for key in cur_keys.difference(&inc_keys) {
        tx.execute(
            "DELETE FROM runtime_process_handles WHERE slug=?1 AND person=?2",
            params![row_slug, key],
        )?;
        touches.push(EventTouch::new(
            "runtime-process-handle",
            (*key).clone(),
            "delete",
            "runtime_process_handles",
            row_slug,
        ));
    }
    for (person, process_handle) in incoming_process_handles {
        if current.get(person) == Some(process_handle) {
            continue;
        }
        tx.execute(
            "INSERT INTO runtime_process_handles(slug, person, process_handle) VALUES(?1,?2,?3) \
             ON CONFLICT(slug, person) DO UPDATE SET process_handle=?3",
            params![row_slug, person, process_handle],
        )?;
        touches.push(EventTouch::new(
            "runtime-process-handle",
            person.clone(),
            "upsert",
            "runtime_process_handles",
            row_slug,
        ));
    }
    Ok(())
}

/// Fence-free CLEAR: delete the runtime row and every child row for `row_slug`,
/// so the doc becomes ABSENT (`reconstruct` -> None). Mirrors the launch-intent
/// clear seam; runs inside the writer's own `BEGIN IMMEDIATE`.
///
/// # Errors
/// SQL failures as [`ChiefdError::StoreFailure`].
pub fn clear(tx: &Transaction<'_>, row_slug: &str, at: &str) -> Result<(), ChiefdError> {
    let had_scalar = read_scalars(tx, row_slug)?.is_some();
    let process_handles = read_map(
        tx,
        row_slug,
        "SELECT person, process_handle FROM runtime_process_handles WHERE slug = ?1",
    )?;
    let warnings = read_warnings(tx, row_slug)?;
    let (missing, unexpected) = read_recovery_people(tx, row_slug)?;
    apply_and_emit::<RowsSqlError, _>(tx, row_slug, at, "", |tx| {
        let mut touches = Vec::new();
        if had_scalar {
            tx.execute("DELETE FROM runtime WHERE slug=?1", params![row_slug])?;
            touches.push(EventTouch::new("runtime", row_slug, "delete", "runtime", row_slug));
        }
        for person in process_handles.keys() {
            tx.execute(
                "DELETE FROM runtime_process_handles WHERE slug=?1 AND person=?2",
                params![row_slug, person],
            )?;
            touches.push(EventTouch::new(
                "runtime-process-handle",
                person.clone(),
                "delete",
                "runtime_process_handles",
                row_slug,
            ));
        }
        if !warnings.is_empty() {
            tx.execute("DELETE FROM runtime_monitor_warnings WHERE slug=?1", params![row_slug])?;
            touches.push(EventTouch::new(
                "runtime-monitor-warnings",
                row_slug,
                "delete",
                "runtime_monitor_warnings",
                row_slug,
            ));
        }
        if !missing.is_empty() || !unexpected.is_empty() {
            tx.execute("DELETE FROM runtime_recovery_people WHERE slug=?1", params![row_slug])?;
            touches.push(EventTouch::new(
                "runtime-recovery-people",
                row_slug,
                "delete",
                "runtime_recovery_people",
                row_slug,
            ));
        }
        Ok(touches)
    })
    .map(|_seq| ())
    .map_err(|RowsSqlError(e)| e)
}

// TOMBSTONE: `RuntimeObservation` and `publish_observation` (#823/E8-S1).
//
// They were the write path for an observed runtime: the fields one converge
// pass produced when chiefd's own cycle looked at the host, folded into the
// committed row under a carry-forward rule and a no-op gate. The pass that
// produced them is gone (`converge_apply::cycle`'s own tombstone), which left
// this pair with no caller at all — the same latent hole `refuse_observed_map`
// closes on the wire, one layer in. Deleted rather than kept for a future
// caller, because the only caller it could ever have is one that looked at the
// host, and nothing in chiefd may.
//
// The carry-forward rule is not lost with them. It said that a publish owns
// only the fields it names and must write every other one back unchanged, and
// `publish` still enforces exactly that for the writers that remain.

fn reject_unmodeled_keys(doc: &RuntimeState) -> Result<(), ChiefdError> {
    let mut paths: Vec<String> = doc.extra.keys().map(|k| format!("extra.{k}")).collect();
    if let Some(recon) = &doc.reconciliation {
        for k in recon.extra.keys() {
            paths.push(format!("reconciliation.extra.{k}"));
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!("runtime carries unmodeled keys the row model cannot store: {}", paths.join(", ")),
    )))
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

    fn base() -> RuntimeState {
        let mut process_handles = BTreeMap::new();
        process_handles.insert("chief".to_string(), "%1".to_string());
        RuntimeState {
            version: 1,
            organization: None,
            observed_at: "2026-07-25T06:00:00.000Z".into(),
            session: None,
            socket_name: "sock".into(),
            status: "running".into(),
            startup_admission_until: None,
            recovery_fingerprint: None,
            recovery_observed_at: None,
            recovery_confirmed: None,
            recovery: None,
            reconciliation: None,
            process_handles,
            monitor_warnings: vec![],
            missing_durable_person_ids: vec![],
            unexpected_observed_person_ids: vec![],
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn empty_before_any_publish() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(reconstruct(&tx, "acme").unwrap(), None);
    }

    #[test]
    fn round_trips_base() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let s = base();
        publish(&tx, "acme", &s).unwrap();
        assert_eq!(reconstruct(&tx, "acme").unwrap().unwrap(), s);
        // runtime scalar + 1 process handle = 2 touches. It was 3 while a
        // `runtime_windows` row existed; that table is deleted.
        assert_eq!(current_seq(&tx, "acme").unwrap(), 2);
    }

    #[test]
    fn round_trips_recon_recovery_and_ordered_arrays() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut s = base();
        s.status = "recovering".into();
        s.recovery_fingerprint = Some("fp".into());
        s.recovery_observed_at = Some("2026-07-25T06:01:00.000Z".into());
        s.recovery_confirmed = Some(true);
        s.recovery = Some("healing".into());
        s.startup_admission_until = Some("2026-07-25T06:10:00.000Z".into());
        s.reconciliation = Some(RuntimeReconciliation {
            phase: "in_progress".into(),
            started_at: "2026-07-25T06:00:00.000Z".into(),
            extra: BTreeMap::new(),
        });
        s.monitor_warnings = vec!["w1".into(), "w2".into()];
        s.missing_durable_person_ids = vec!["a".into(), "b".into()];
        s.unexpected_observed_person_ids = vec!["c".into()];
        publish(&tx, "acme", &s).unwrap();
        assert_eq!(reconstruct(&tx, "acme").unwrap().unwrap(), s);
    }

    #[test]
    fn scalar_only_change_is_one_touch() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &base()).unwrap();
        let seq = current_seq(&tx, "acme").unwrap();
        let mut s = base();
        s.observed_at = "2026-07-25T06:05:00.000Z".into();
        s.status = "idle".into();
        let out = publish(&tx, "acme", &s).unwrap();
        assert_eq!(out, seq + 1);
        assert_eq!(reconstruct(&tx, "acme").unwrap().unwrap().status, "idle");
    }

    #[test]
    fn removing_a_process_handle_deletes_its_row() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &base()).unwrap();
        let mut s = base();
        s.process_handles.clear();
        publish(&tx, "acme", &s).unwrap();
        assert!(reconstruct(&tx, "acme").unwrap().unwrap().process_handles.is_empty());
    }

    #[test]
    fn unchanged_publish_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &base()).unwrap();
        let seq = current_seq(&tx, "acme").unwrap();
        let out = publish(&tx, "acme", &base()).unwrap();
        assert_eq!(out, seq);
    }

    #[test]
    fn a_second_direct_publish_uses_current_state_after_an_audit_cursor_advanced() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let first = publish(&tx, "acme", &base()).unwrap();
        let mut next = base();
        next.status = "idle".into();
        next.observed_at = "2026-07-25T06:06:00.000Z".into();
        let seq = publish(&tx, "acme", &next).unwrap();
        // Against the FIRST write's cursor, not a hardcoded 3. The literal was
        // the genesis touch count (scalar + process handle + window) and it moved to 2
        // when `runtime_windows` was deleted, so the assertion was about the
        // number of child tables rather than about the cursor advancing.
        assert!(seq > first, "the second direct write emits a later audit cursor");
        assert_eq!(reconstruct(&tx, "acme").unwrap().unwrap().status, "idle");
    }

    #[test]
    fn rejects_unmodeled_keys() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut s = base();
        s.extra.insert("bogus".into(), serde_json::json!(1));
        assert_eq!(publish(&tx, "acme", &s).unwrap_err().code(), Some(UNMODELED_KEYS));
    }

    #[test]
    fn slug_scoping_isolates_companies() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &base()).unwrap();
        let mut beta = base();
        beta.process_handles.clear();
        beta.process_handles.insert("beta-ceo".into(), "%9".into());
        publish(&tx, "beta", &beta).unwrap();
        assert_eq!(
            reconstruct(&tx, "acme")
                .unwrap()
                .unwrap()
                .process_handles
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["chief"]
        );
        assert_eq!(
            reconstruct(&tx, "beta")
                .unwrap()
                .unwrap()
                .process_handles
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["beta-ceo"]
        );
    }

    // ---- the direction bar, on the wire -----------------------------------

    /// THE HALF THAT IS STILL OPEN, asserted so it is a recorded state rather
    /// than a belief.
    ///
    /// `process_handles` is the same kind of fact and is NOT refused, because
    /// two live TypeScript readers (`org_roster`'s runtime projection and the
    /// wake gate) have nothing else to read and must be re-founded on the
    /// desired set first. This test fails the day that lands, which is exactly
    /// when somebody should be made to come back here.
    #[test]
    fn observed_process_handles_are_still_accepted_and_that_is_the_open_half() {
        let doc = serde_json::from_str::<RuntimeState>(
            r#"{"version":1,"observedAt":"2026-08-04T00:00:00.000Z","socketName":"sock",
                "status":"running","processHandles":{"chief":"4812"}}"#,
        )
        .expect("still accepted: see the field's doc for what must move first");
        assert_eq!(doc.process_handles.get("chief").map(String::as_str), Some("4812"));
    }

    /// An EMPTY map still decodes, and must: every surviving writer writes
    /// empty (the `starting` bootstrap and the stop publish), and refusing that
    /// would refuse chiefd's own bootstrap.
    #[test]
    fn an_empty_map_still_decodes_because_chiefds_own_writers_send_one() {
        let doc = serde_json::from_str::<RuntimeState>(
            r#"{"version":1,"observedAt":"2026-08-04T00:00:00.000Z","socketName":"sock",
                "status":"starting","processHandles":{}}"#,
        )
        .expect("chiefd's own empty publish must still be accepted");
        assert!(doc.process_handles.is_empty());
    }

    /// An ABSENT key is the ordinary case and is not a refusal either — a
    /// publisher that never mentions process handles is not claiming anything
    /// about them.
    #[test]
    fn an_absent_key_is_not_a_claim_and_is_not_refused() {
        let doc = serde_json::from_str::<RuntimeState>(
            r#"{"version":1,"observedAt":"2026-08-04T00:00:00.000Z","socketName":"sock",
                "status":"starting"}"#,
        )
        .expect("saying nothing is always allowed");
        assert!(doc.process_handles.is_empty());
    }
}
