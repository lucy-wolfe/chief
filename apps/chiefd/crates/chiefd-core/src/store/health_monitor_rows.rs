//! The `health-monitor` ROW implementation (org-data-normalization P0, bucket1b
//! — 5-table single-doc port).
//!
//! The health-monitor state document (`org-health-monitor.ts`) — a PROJECTION
//! (never an authority) of log cursors, provisional observations, active
//! incidents, and terminal-incident resolutions — reconstructed from five
//! slug-scoped tables. Cursors/observations publish by diffing each map
//! against its rows; incidents/terminal-resolutions publish by MERGE (F17 —
//! deletion requires positive evidence, never absence from a payload). One
//! `org_events` touch per changed entity. DERIVED, never stored: `version` =
//! const `1`; `organization` = company slug.
//!
//! Tables (#32 slug-scoped): `health_monitor_meta` (singleton: `last_run_at`,
//! also the doc's existence marker), `health_monitor_cursors` (by path),
//! `health_monitor_observations` (by key), `health_monitor_incidents` (by
//! fingerprint), `health_monitor_terminal_resolutions` (by fingerprint).
//!
//! Item D: publish REJECTS any serde-flatten `extra` (doc- or entry-level) with
//! [`UNMODELED_KEYS`].

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::corrupt_store;
use crate::error::Refusal;
use crate::store::organization_rows::{RowsSqlError, UNMODELED_KEYS};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::ChiefdError;

/// The `org_documents` store family this row set replaces.
pub const HEALTH_MONITOR_STORE: &str = "health-monitor";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("health-monitor-rows", e)
}

// ---- entry types (mirror org-health-monitor.ts) ---------------------------

/// A bounded-log read cursor. The map key (path) is stored separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthLogCursor {
    /// Stable source-device identifier observed for the log path.
    pub device: String,
    /// Stable source inode identifier observed for the log path.
    pub inode: String,
    /// Byte position from which the next bounded read resumes.
    pub offset: i64,
}

/// A provisional runtime observation awaiting a later sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthMonitorObservation {
    /// ISO-8601 time at which this observation first appeared.
    pub first_observed_at: String,
    /// ISO-8601 time at which this observation was last sampled.
    pub last_observed_at: String,
    /// Number of samples contributing to this provisional observation.
    pub count: i64,
}

/// An active incident. `fingerprint` is BOTH the map key and a stored field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthMonitorIncident {
    /// Stable incident identity, also used as this map's key.
    pub fingerprint: String,
    /// Classified incident kind.
    pub kind: String,
    /// Human-legible incident detail.
    pub detail: String,
    /// ISO-8601 time at which the incident first appeared.
    pub first_seen_at: String,
    /// ISO-8601 time at which the incident was last seen.
    pub last_seen_at: String,
    /// Number of observations grouped into this incident.
    pub count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Person currently responsible for resolving the incident, if assigned.
    pub responsible_person_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Suggested action that can unblock the incident, if known.
    pub unblock_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Count measured when the incident was observed, if applicable.
    pub observed_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Oldest contributing timestamp, if the incident has time-ordered input.
    pub oldest_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// ISO-8601 acknowledgement time, if acknowledged.
    pub acknowledged_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Person selected to receive the alert, if any.
    pub alert_recipient_person_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Mailbox-impaired person implicated by the incident, if any.
    pub impaired_mailbox_person_id: Option<String>,
    #[serde(flatten)]
    /// Forward-compatible unmodeled fields retained while decoding.
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// A terminal-incident resolution. `fingerprint` is BOTH the map key and a
/// stored field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalHealthIncidentResolution {
    /// Stable incident identity, also used as this map's key.
    pub fingerprint: String,
    /// Classified incident kind.
    pub kind: String,
    /// ISO-8601 time at which the incident first appeared.
    pub first_seen_at: String,
    /// Person who accepted the resolution.
    pub recipient_person_id: String,
    /// ISO-8601 time at which the resolution was accepted.
    pub accepted_at: String,
}

/// The whole `health-monitor` state doc. Maps are keyed exactly as TS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthMonitorState {
    /// Serialized document schema version.
    pub version: u32,
    /// Company slug owning this health-monitor state.
    pub organization: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// ISO-8601 time of the most recent monitor pass, if it has run.
    pub last_run_at: Option<String>,
    // Maps default to empty on missing (the blob `stateFrom` tolerated absent
    // maps via `?? {}`; a partial state doc must not fail deserialization).
    #[serde(default)]
    /// Bounded-log cursors keyed by source path.
    pub cursors: BTreeMap<String, HealthLogCursor>,
    #[serde(default)]
    /// Provisional observations keyed by their derived identity.
    pub observations: BTreeMap<String, HealthMonitorObservation>,
    #[serde(default)]
    /// Active incidents keyed by fingerprint.
    pub incidents: BTreeMap<String, HealthMonitorIncident>,
    #[serde(default)]
    /// Terminal resolutions keyed by incident fingerprint.
    pub terminal_resolutions: BTreeMap<String, TerminalHealthIncidentResolution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Explicit clear signal (Step-4 follow-up to F17's merge semantics):
    /// incident fingerprints the publishing pass positively resolved — the
    /// scan-derived condition is gone. Deletion requires positive evidence;
    /// this is the evidence for resolutions that are NOT terminal
    /// acceptances. Unlike a terminal resolution a clear is NOT journaled:
    /// the fingerprint is released, so a later recurrence of the same
    /// condition re-appears as a new incident with a fresh `firstSeenAt`
    /// (a terminal resolution, being append-only, would suppress it forever).
    /// Write-only: reconstructions always serve it empty.
    pub cleared_fingerprints: Vec<String>,
    #[serde(flatten)]
    /// Forward-compatible unmodeled fields retained while decoding.
    pub extra: BTreeMap<String, serde_json::Value>,
}

// ---- reconstruct ----------------------------------------------------------

fn read_meta(tx: &Transaction<'_>, slug: &str) -> Result<Option<Option<String>>, ChiefdError> {
    tx.query_row(
        "SELECT last_run_at FROM health_monitor_meta WHERE slug = ?1",
        params![slug],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .map_err(store_failure)
}

fn read_cursors(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<BTreeMap<String, HealthLogCursor>, ChiefdError> {
    let mut stmt = tx
        .prepare("SELECT path, device, inode, offset FROM health_monitor_cursors WHERE slug = ?1")
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug], |r| {
            Ok((
                r.get::<_, String>(0)?,
                HealthLogCursor { device: r.get(1)?, inode: r.get(2)?, offset: r.get(3)? },
            ))
        })
        .map_err(store_failure)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (k, v) = row.map_err(store_failure)?;
        map.insert(k, v);
    }
    Ok(map)
}

fn read_observations(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<BTreeMap<String, HealthMonitorObservation>, ChiefdError> {
    let mut stmt = tx
        .prepare("SELECT key, first_observed_at, last_observed_at, count FROM health_monitor_observations WHERE slug = ?1")
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug], |r| {
            Ok((
                r.get::<_, String>(0)?,
                HealthMonitorObservation {
                    first_observed_at: r.get(1)?,
                    last_observed_at: r.get(2)?,
                    count: r.get(3)?,
                },
            ))
        })
        .map_err(store_failure)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (k, v) = row.map_err(store_failure)?;
        map.insert(k, v);
    }
    Ok(map)
}

fn read_incidents(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<BTreeMap<String, HealthMonitorIncident>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT fingerprint, kind, detail, first_seen_at, last_seen_at, count, \
             responsible_person_id, unblock_action, observed_count, oldest_at, acknowledged_at, \
             alert_recipient_person_id, impaired_mailbox_person_id \
             FROM health_monitor_incidents WHERE slug = ?1",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug], |r| {
            let fingerprint: String = r.get(0)?;
            Ok((
                fingerprint.clone(),
                HealthMonitorIncident {
                    fingerprint,
                    kind: r.get(1)?,
                    detail: r.get(2)?,
                    first_seen_at: r.get(3)?,
                    last_seen_at: r.get(4)?,
                    count: r.get(5)?,
                    responsible_person_id: r.get(6)?,
                    unblock_action: r.get(7)?,
                    observed_count: r.get(8)?,
                    oldest_at: r.get(9)?,
                    acknowledged_at: r.get(10)?,
                    alert_recipient_person_id: r.get(11)?,
                    impaired_mailbox_person_id: r.get(12)?,
                    extra: BTreeMap::new(),
                },
            ))
        })
        .map_err(store_failure)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (k, v) = row.map_err(store_failure)?;
        map.insert(k, v);
    }
    Ok(map)
}

fn read_resolutions(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<BTreeMap<String, TerminalHealthIncidentResolution>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT fingerprint, kind, first_seen_at, recipient_person_id, accepted_at \
             FROM health_monitor_terminal_resolutions WHERE slug = ?1",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug], |r| {
            let fingerprint: String = r.get(0)?;
            Ok((
                fingerprint.clone(),
                TerminalHealthIncidentResolution {
                    fingerprint,
                    kind: r.get(1)?,
                    first_seen_at: r.get(2)?,
                    recipient_person_id: r.get(3)?,
                    accepted_at: r.get(4)?,
                },
            ))
        })
        .map_err(store_failure)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (k, v) = row.map_err(store_failure)?;
        map.insert(k, v);
    }
    Ok(map)
}

/// Reconstruct the state doc, or `None` when the company has no health-monitor
/// state at all (no meta row and every table empty).
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<Option<HealthMonitorState>, ChiefdError> {
    let meta = read_meta(tx, row_slug)?;
    let cursors = read_cursors(tx, row_slug)?;
    let observations = read_observations(tx, row_slug)?;
    let incidents = read_incidents(tx, row_slug)?;
    let terminal_resolutions = read_resolutions(tx, row_slug)?;
    if meta.is_none()
        && cursors.is_empty()
        && observations.is_empty()
        && incidents.is_empty()
        && terminal_resolutions.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(HealthMonitorState {
        version: 1,
        organization: company_slug.to_string(),
        last_run_at: meta.flatten(),
        cursors,
        observations,
        incidents,
        terminal_resolutions,
        cleared_fingerprints: Vec::new(),
        extra: BTreeMap::new(),
    }))
}

// ---- publish (diff) -------------------------------------------------------

/// Publish the full state from current SQLite state. One `org_events` touch per
/// changed entity.
///
/// Map semantics differ by table (F17 — "merge semantics before second
/// writer"): `cursors` and `observations` are the monitor pass's own working
/// set and keep full-snapshot diffing (delete absent, upsert changed/new).
/// `incidents` and `terminal_resolutions` are FACTS that outlive a pass and may
/// carry other writers' commits, so they MERGE: incoming entries are upserted,
/// and an incident row is deleted ONLY on positive evidence of resolution —
/// its fingerprint appearing in `terminal_resolutions` (committed or incoming,
/// journaled) or in `cleared_fingerprints` (explicit, not journaled, so a
/// recurring condition re-appears fresh). Absence from the payload never
/// deletes; "unmentioned" and "resolved" are different words on this write
/// path. Terminal resolutions are append-only.
///
/// # Errors
/// [`UNMODELED_KEYS`] refusal (422); SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming: &HealthMonitorState,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    let at = incoming.last_run_at.clone().unwrap_or_default();

    let cur_meta = read_meta(tx, row_slug)?;
    let cur_cursors = read_cursors(tx, row_slug)?;
    let cur_obs = read_observations(tx, row_slug)?;
    let cur_incidents = read_incidents(tx, row_slug)?;
    let cur_res = read_resolutions(tx, row_slug)?;

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        let mut touches = Vec::new();

        // meta (singleton; also existence marker)
        if cur_meta != Some(incoming.last_run_at.clone()) {
            tx.execute(
                "INSERT INTO health_monitor_meta(slug, last_run_at) VALUES(?1, ?2) \
                 ON CONFLICT(slug) DO UPDATE SET last_run_at = ?2",
                params![row_slug, incoming.last_run_at],
            )?;
            touches.push(EventTouch::new("hm-meta", row_slug, "upsert", "health_monitor_meta", row_slug));
        }

        // cursors
        diff_map(
            tx, row_slug, &cur_cursors, &incoming.cursors, &mut touches,
            "hm-cursor", "health_monitor_cursors",
            |tx, slug, key| tx.execute("DELETE FROM health_monitor_cursors WHERE slug=?1 AND path=?2", params![slug, key]),
            |tx, slug, key, v| tx.execute(
                "INSERT INTO health_monitor_cursors(slug, path, device, inode, offset) VALUES(?1,?2,?3,?4,?5) \
                 ON CONFLICT(slug, path) DO UPDATE SET device=?3, inode=?4, offset=?5",
                params![slug, key, v.device, v.inode, v.offset],
            ),
        )?;

        // observations
        diff_map(
            tx, row_slug, &cur_obs, &incoming.observations, &mut touches,
            "hm-observation", "health_monitor_observations",
            |tx, slug, key| tx.execute("DELETE FROM health_monitor_observations WHERE slug=?1 AND key=?2", params![slug, key]),
            |tx, slug, key, v| tx.execute(
                "INSERT INTO health_monitor_observations(slug, key, first_observed_at, last_observed_at, count) \
                 VALUES(?1,?2,?3,?4,?5) ON CONFLICT(slug, key) DO UPDATE SET \
                 first_observed_at=?3, last_observed_at=?4, count=?5",
                params![slug, key, v.first_observed_at, v.last_observed_at, v.count],
            ),
        )?;

        // incidents (F17): merge — upsert mentioned incidents, delete ONLY on
        // positive evidence of resolution (fingerprint present in committed or
        // incoming terminal_resolutions). Absence from the payload deletes
        // nothing: a stale republish cannot erase an incident it never saw.
        let mut resolved: BTreeSet<&String> = cur_res.keys().collect();
        resolved.extend(incoming.terminal_resolutions.keys());
        for (key, value) in &incoming.incidents {
            if resolved.contains(key) || cur_incidents.get(key) == Some(value) {
                // A resolution outranks (re)publication of the same fingerprint.
                continue;
            }
            tx.execute(
                "INSERT INTO health_monitor_incidents(slug, fingerprint, kind, detail, first_seen_at, \
                 last_seen_at, count, responsible_person_id, unblock_action, observed_count, oldest_at, \
                 acknowledged_at, alert_recipient_person_id, \
                 impaired_mailbox_person_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) \
                 ON CONFLICT(slug, fingerprint) DO UPDATE SET kind=?3, detail=?4, first_seen_at=?5, \
                 last_seen_at=?6, count=?7, responsible_person_id=?8, unblock_action=?9, observed_count=?10, \
                 oldest_at=?11, acknowledged_at=?12, alert_recipient_person_id=?13, \
                 impaired_mailbox_person_id=?14",
                params![
                    row_slug, key, value.kind, value.detail, value.first_seen_at, value.last_seen_at, value.count,
                    value.responsible_person_id, value.unblock_action, value.observed_count, value.oldest_at,
                    value.acknowledged_at, value.alert_recipient_person_id,
                    value.impaired_mailbox_person_id,
                ],
            )?;
            touches.push(EventTouch::new("hm-incident", key.clone(), "upsert", "health_monitor_incidents", row_slug));
        }
        for key in cur_incidents.keys() {
            if resolved.contains(key) {
                tx.execute(
                    "DELETE FROM health_monitor_incidents WHERE slug=?1 AND fingerprint=?2",
                    params![row_slug, key],
                )?;
                touches.push(EventTouch::new("hm-incident", key.clone(), "delete", "health_monitor_incidents", row_slug));
            }
        }

        // terminal resolutions (F17): append-only journal — upsert incoming,
        // never delete on absence. Resolutions are the positive evidence the
        // incidents merge above resolves against, so losing one to a stale
        // republish would un-resolve an incident.
        for (key, value) in &incoming.terminal_resolutions {
            if cur_res.get(key) == Some(value) {
                continue;
            }
            tx.execute(
                "INSERT INTO health_monitor_terminal_resolutions(slug, fingerprint, kind, first_seen_at, \
                 recipient_person_id, accepted_at) \
                 VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(slug, fingerprint) DO UPDATE SET \
                 kind=?3, first_seen_at=?4, recipient_person_id=?5, accepted_at=?6",
                params![row_slug, key, value.kind, value.first_seen_at, value.recipient_person_id, value.accepted_at],
            )?;
            touches.push(EventTouch::new("hm-terminal-resolution", key.clone(), "upsert", "health_monitor_terminal_resolutions", row_slug));
        }

        // Explicit clears (Step-4 follow-up): a fingerprint in
        // `cleared_fingerprints` is positive evidence the publishing pass
        // resolved that incident without an operator terminal acceptance, so
        // the row is deleted WITHOUT a journal entry and a later recurrence
        // re-appears fresh. A clear outranks a same-payload (re)publication
        // of the same fingerprint; a journaled terminal resolution outranks
        // both (handled above, so it is skipped here).
        for key in &incoming.cleared_fingerprints {
            if resolved.contains(key) {
                continue;
            }
            if cur_incidents.contains_key(key) || incoming.incidents.contains_key(key) {
                tx.execute(
                    "DELETE FROM health_monitor_incidents WHERE slug=?1 AND fingerprint=?2",
                    params![row_slug, key],
                )?;
                touches.push(EventTouch::new("hm-incident", key.clone(), "delete", "health_monitor_incidents", row_slug));
            }
        }

        Ok(touches)
    })
    .map_err(|RowsSqlError(e)| e)
}

/// Decode a `health-monitor` documents blob — the daemon duty's
/// [`crate::store::health::HealthMonitorState`] serialization — and publish it
/// as rows (the F16 un-cross-wiring: the duty's commits used to be routed by
/// the colliding `"health"` store name into the `daemon_health_*` tables,
/// invisible to every org-health reader).
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes, [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn backfill_health_monitor(
    tx: &Transaction<'_>,
    row_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let state: crate::store::health::HealthMonitorState =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("health-monitor-blob", e))?;
    let state = from_daemon_state(&state);
    publish(tx, row_slug, &state)
}

/// Convert the daemon duty's state type (`store/health.rs`) into this module's
/// wire/storage type. The two types are field-compatible projections of the
/// same document; only numeric widths (`u64` → `i64`) differ.
#[must_use]
pub fn from_daemon_state(state: &crate::store::health::HealthMonitorState) -> HealthMonitorState {
    let terminal_resolutions = state
        .terminal_resolutions
        .iter()
        .map(|(key, r)| {
            (
                key.clone(),
                TerminalHealthIncidentResolution {
                    fingerprint: r.fingerprint.clone(),
                    kind: r.kind.clone(),
                    first_seen_at: r.first_seen_at.clone(),
                    recipient_person_id: r.recipient_person_id.clone(),
                    accepted_at: r.accepted_at.clone(),
                },
            )
        })
        .collect();
    HealthMonitorState {
        version: state.version,
        organization: state.organization.clone(),
        last_run_at: state.last_run_at.clone(),
        cursors: state
            .cursors
            .iter()
            .map(|(k, c)| {
                (
                    k.clone(),
                    HealthLogCursor {
                        device: c.device.clone(),
                        inode: c.inode.clone(),
                        offset: c.offset as i64,
                    },
                )
            })
            .collect(),
        observations: state
            .observations
            .iter()
            .map(|(k, o)| {
                (
                    k.clone(),
                    HealthMonitorObservation {
                        first_observed_at: o.first_observed_at.clone(),
                        last_observed_at: o.last_observed_at.clone(),
                        count: o.count as i64,
                    },
                )
            })
            .collect(),
        incidents: state
            .incidents
            .iter()
            .map(|(k, i)| {
                (
                    k.clone(),
                    HealthMonitorIncident {
                        fingerprint: i.fingerprint.clone(),
                        kind: i.kind.clone(),
                        detail: i.detail.clone(),
                        first_seen_at: i.first_seen_at.clone(),
                        last_seen_at: i.last_seen_at.clone(),
                        count: i.count as i64,
                        responsible_person_id: i.responsible_person_id.clone(),
                        unblock_action: i.unblock_action.clone(),
                        observed_count: i.observed_count.map(|c| c as i64),
                        oldest_at: i.oldest_at.clone(),
                        acknowledged_at: i.acknowledged_at.clone(),
                        alert_recipient_person_id: i.alert_recipient_person_id.clone(),
                        impaired_mailbox_person_id: i.impaired_mailbox_person_id.clone(),
                        extra: BTreeMap::new(),
                    },
                )
            })
            .collect(),
        terminal_resolutions,
        cleared_fingerprints: Vec::new(),
        extra: BTreeMap::new(),
    }
}

/// Delete every health-monitor row for `row_slug`, fence-free via
/// [`apply_and_emit`] (mirrors `launch_intent_rows::clear`).
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn clear(tx: &Transaction<'_>, row_slug: &str, at: &str) -> Result<(), ChiefdError> {
    let had_meta = read_meta(tx, row_slug)?.is_some();
    let cursors: Vec<String> = read_cursors(tx, row_slug)?.into_keys().collect();
    let observations: Vec<String> = read_observations(tx, row_slug)?.into_keys().collect();
    let incidents: Vec<String> = read_incidents(tx, row_slug)?.into_keys().collect();
    let resolutions: Vec<String> = read_resolutions(tx, row_slug)?.into_keys().collect();

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, at, "", |tx| {
        let mut touches = Vec::new();
        if had_meta {
            tx.execute("DELETE FROM health_monitor_meta WHERE slug = ?1", params![row_slug])?;
            touches.push(EventTouch::new(
                "hm-meta",
                row_slug,
                "delete",
                "health_monitor_meta",
                row_slug,
            ));
        }
        for key in &cursors {
            tx.execute(
                "DELETE FROM health_monitor_cursors WHERE slug=?1 AND path=?2",
                params![row_slug, key],
            )?;
            touches.push(EventTouch::new(
                "hm-cursor",
                key.clone(),
                "delete",
                "health_monitor_cursors",
                row_slug,
            ));
        }
        for key in &observations {
            tx.execute(
                "DELETE FROM health_monitor_observations WHERE slug=?1 AND key=?2",
                params![row_slug, key],
            )?;
            touches.push(EventTouch::new(
                "hm-observation",
                key.clone(),
                "delete",
                "health_monitor_observations",
                row_slug,
            ));
        }
        for key in &incidents {
            tx.execute(
                "DELETE FROM health_monitor_incidents WHERE slug=?1 AND fingerprint=?2",
                params![row_slug, key],
            )?;
            touches.push(EventTouch::new(
                "hm-incident",
                key.clone(),
                "delete",
                "health_monitor_incidents",
                row_slug,
            ));
        }
        for key in &resolutions {
            tx.execute(
                "DELETE FROM health_monitor_terminal_resolutions WHERE slug=?1 AND fingerprint=?2",
                params![row_slug, key],
            )?;
            touches.push(EventTouch::new(
                "hm-terminal-resolution",
                key.clone(),
                "delete",
                "health_monitor_terminal_resolutions",
                row_slug,
            ));
        }
        Ok(touches)
    })
    .map(|_seq| ())
    .map_err(|RowsSqlError(e)| e)
}

/// Generic map diff: delete keys no longer present, upsert changed/new ones,
/// pushing one `EventTouch` per change. `V: PartialEq` decides "changed".
#[allow(clippy::too_many_arguments)]
fn diff_map<V, D, U>(
    tx: &Transaction<'_>,
    row_slug: &str,
    current: &BTreeMap<String, V>,
    incoming: &BTreeMap<String, V>,
    touches: &mut Vec<EventTouch>,
    entity: &str,
    table: &str,
    del: D,
    up: U,
) -> Result<(), RowsSqlError>
where
    V: PartialEq,
    D: Fn(&Transaction<'_>, &str, &str) -> rusqlite::Result<usize>,
    U: Fn(&Transaction<'_>, &str, &str, &V) -> rusqlite::Result<usize>,
{
    let cur_keys: BTreeSet<&String> = current.keys().collect();
    let inc_keys: BTreeSet<&String> = incoming.keys().collect();
    for key in cur_keys.difference(&inc_keys) {
        del(tx, row_slug, key)?;
        touches.push(EventTouch::new(entity, (*key).clone(), "delete", table, row_slug));
    }
    for (key, value) in incoming {
        if current.get(key) == Some(value) {
            continue;
        }
        up(tx, row_slug, key, value)?;
        touches.push(EventTouch::new(entity, key.clone(), "upsert", table, row_slug));
    }
    Ok(())
}

fn reject_unmodeled_keys(doc: &HealthMonitorState) -> Result<(), ChiefdError> {
    let mut paths: Vec<String> = doc.extra.keys().map(|k| format!("extra.{k}")).collect();
    for (fp, incident) in &doc.incidents {
        for k in incident.extra.keys() {
            paths.push(format!("incidents.{fp}.extra.{k}"));
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!(
            "health-monitor carries unmodeled keys the row model cannot store: {}",
            paths.join(", ")
        ),
    )))
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

    fn incident(fp: &str, detail: &str) -> HealthMonitorIncident {
        HealthMonitorIncident {
            fingerprint: fp.into(),
            kind: "supervisor_stale".into(),
            detail: detail.into(),
            first_seen_at: "2026-07-25T06:00:00.000Z".into(),
            last_seen_at: "2026-07-25T06:05:00.000Z".into(),
            count: 1,
            responsible_person_id: Some("chief".into()),
            unblock_action: None,
            observed_count: None,
            oldest_at: None,
            acknowledged_at: None,
            alert_recipient_person_id: Some("chief".into()),
            impaired_mailbox_person_id: None,
            extra: BTreeMap::new(),
        }
    }

    fn state() -> HealthMonitorState {
        let mut cursors = BTreeMap::new();
        cursors.insert(
            "/log/a".to_string(),
            HealthLogCursor { device: "d".into(), inode: "i".into(), offset: 42 },
        );
        let mut observations = BTreeMap::new();
        observations.insert(
            "obs-1".to_string(),
            HealthMonitorObservation {
                first_observed_at: "a".into(),
                last_observed_at: "b".into(),
                count: 2,
            },
        );
        let mut incidents = BTreeMap::new();
        incidents.insert("fp-1".to_string(), incident("fp-1", "5m ago"));
        let mut terminal_resolutions = BTreeMap::new();
        terminal_resolutions.insert(
            "fp-0".to_string(),
            TerminalHealthIncidentResolution {
                fingerprint: "fp-0".into(),
                kind: "supervision.terminal".into(),
                first_seen_at: "a".into(),
                recipient_person_id: "chief".into(),
                accepted_at: "2026-07-25T06:04:00.000Z".into(),
            },
        );
        HealthMonitorState {
            version: 1,
            organization: "acme".into(),
            last_run_at: Some("2026-07-25T06:05:00.000Z".into()),
            cursors,
            observations,
            incidents,
            terminal_resolutions,
            cleared_fingerprints: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn empty_before_any_publish() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap(), None);
    }

    #[test]
    fn round_trips_full_state_and_derives_identity() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let s = state();
        publish(&tx, "acme", &s).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got, s);
        assert_eq!(got.version, 1);
        assert_eq!(got.organization, "acme");
        // meta + 1 cursor + 1 obs + 1 incident + 1 resolution = 5 touches
        assert_eq!(event_count(&tx, "acme"), 5);
    }

    #[test]
    fn empty_state_with_only_last_run_at_persists_via_meta() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let s = HealthMonitorState {
            version: 1,
            organization: "acme".into(),
            last_run_at: Some("t".into()),
            cursors: BTreeMap::new(),
            observations: BTreeMap::new(),
            incidents: BTreeMap::new(),
            terminal_resolutions: BTreeMap::new(),
            cleared_fingerprints: Vec::new(),
            extra: BTreeMap::new(),
        };
        publish(&tx, "acme", &s).unwrap();
        assert_eq!(
            reconstruct(&tx, "acme", "acme").unwrap().unwrap().last_run_at,
            Some("t".into())
        );
    }

    #[test]
    fn incident_detail_change_upserts_only_that_incident() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let seq = event_count(&tx, "acme");
        let mut s = state();
        s.incidents.get_mut("fp-1").unwrap().detail = "25m ago".into();
        s.last_run_at = Some("2026-07-25T06:25:00.000Z".into());
        publish(&tx, "acme", &s).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got.incidents.get("fp-1").unwrap().detail, "25m ago");
        // meta (last_run_at changed) + incident = 2 touches
        assert_eq!(event_count(&tx, "acme"), seq + 2);
    }

    #[test]
    fn an_incident_absent_from_the_payload_is_not_deleted_without_positive_evidence() {
        // F17 merge semantics: "unmentioned" is not "resolved". A caller
        // republishing a snapshot that predates (or simply omits) a committed
        // incident must not delete it.
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let mut s = state();
        s.incidents.clear();
        publish(&tx, "acme", &s).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert!(
            got.incidents.contains_key("fp-1"),
            "absence from the payload must not delete a committed incident"
        );
        assert_eq!(got.cursors.len(), 1); // others untouched
    }

    fn resolution(fp: &str) -> TerminalHealthIncidentResolution {
        TerminalHealthIncidentResolution {
            fingerprint: fp.into(),
            kind: "supervision.terminal".into(),
            first_seen_at: "a".into(),
            recipient_person_id: "chief".into(),
            accepted_at: "2026-07-25T06:04:00.000Z".into(),
        }
    }

    #[test]
    fn an_explicit_terminal_resolution_deletes_the_incident() {
        // Positive evidence in the SAME payload: the caller omits the incident
        // AND records its terminal resolution — that is a resolution, and the
        // row goes away.
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let mut s = state();
        s.incidents.clear();
        s.terminal_resolutions.insert("fp-1".to_string(), resolution("fp-1"));
        publish(&tx, "acme", &s).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert!(
            got.incidents.is_empty(),
            "an explicit terminal resolution is positive evidence: the incident row is deleted"
        );
        assert!(got.terminal_resolutions.contains_key("fp-1"));
    }

    #[test]
    fn a_resolution_committed_by_another_writer_deletes_the_incident() {
        // Positive evidence committed by an INDEPENDENT earlier publish: the
        // next publish — even one that never mentions the incident — resolves
        // against the committed journal, not just its own payload.
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let mut s = state();
        s.incidents.clear();
        s.terminal_resolutions.insert("fp-1".to_string(), resolution("fp-1"));
        publish(&tx, "acme", &s).unwrap();
        assert!(reconstruct(&tx, "acme", "acme").unwrap().unwrap().incidents.is_empty());
        // A later stale republish of the ORIGINAL snapshot (incident fp-1
        // present, resolution fp-1 unknown to it) must not resurrect fp-1:
        // a resolution outranks (re)publication of the same fingerprint.
        publish(&tx, "acme", &state()).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert!(
            !got.incidents.contains_key("fp-1"),
            "a resolved incident must not be recreated by a stale republication"
        );
        assert!(got.terminal_resolutions.contains_key("fp-1"));
    }

    #[test]
    fn terminal_resolutions_are_append_only() {
        // The resolution journal is the positive evidence the incident merge
        // resolves against; a stale republish must not un-resolve by omission.
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let mut s = state();
        s.terminal_resolutions.clear();
        publish(&tx, "acme", &s).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert!(
            got.terminal_resolutions.contains_key("fp-0"),
            "absence from the payload must not delete a committed resolution"
        );
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
        assert_eq!(out, 5);
    }

    #[test]
    fn rejects_unmodeled_incident_keys() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut s = state();
        s.incidents.get_mut("fp-1").unwrap().extra.insert("bogus".into(), serde_json::json!(1));
        assert_eq!(publish(&tx, "acme", &s).unwrap_err().code(), Some(UNMODELED_KEYS));
    }

    #[test]
    fn slug_scoping_isolates_companies_in_a_shared_db() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let mut beta = state();
        beta.organization = "beta".into();
        beta.incidents.clear();
        beta.incidents.insert("fp-beta".to_string(), incident("fp-beta", "beta-only"));
        publish(&tx, "beta", &beta).unwrap();
        assert_eq!(
            reconstruct(&tx, "acme", "acme")
                .unwrap()
                .unwrap()
                .incidents
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["fp-1"]
        );
        assert_eq!(
            reconstruct(&tx, "beta", "beta")
                .unwrap()
                .unwrap()
                .incidents
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["fp-beta"]
        );
    }

    #[test]
    fn a_cleared_fingerprint_deletes_the_incident_without_a_resolution() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let mut clearing = state();
        clearing.incidents.clear();
        clearing.cleared_fingerprints = vec!["fp-1".into()];
        publish(&tx, "acme", &clearing).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert!(!got.incidents.contains_key("fp-1"), "the explicit clear deletes the row");
        assert!(
            !got.terminal_resolutions.contains_key("fp-1"),
            "a clear is not journaled as a terminal resolution"
        );
    }

    #[test]
    fn a_clear_outranks_a_same_payload_republication() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let mut s = state();
        s.cleared_fingerprints = vec!["fp-1".into()]; // fp-1 is ALSO in incidents
        publish(&tx, "acme", &s).unwrap();
        assert!(!reconstruct(&tx, "acme", "acme").unwrap().unwrap().incidents.contains_key("fp-1"));
    }

    #[test]
    fn a_cleared_fingerprint_accepts_a_later_recurrence() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let mut clearing = state();
        clearing.incidents.clear();
        clearing.cleared_fingerprints = vec!["fp-1".into()];
        publish(&tx, "acme", &clearing).unwrap();
        // Recurrence: the same fingerprint re-published with no clear.
        publish(&tx, "acme", &state()).unwrap();
        assert!(
            reconstruct(&tx, "acme", "acme").unwrap().unwrap().incidents.contains_key("fp-1"),
            "unlike a terminal resolution, a clear must not suppress recurrence"
        );
    }

    #[test]
    fn a_clear_for_an_unknown_fingerprint_is_a_noop() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        let mut s = state();
        s.cleared_fingerprints = vec!["fp-never-existed".into()];
        publish(&tx, "acme", &s).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert!(got.incidents.contains_key("fp-1"));
    }

    #[test]
    fn clear_removes_every_health_monitor_row() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &state()).unwrap();
        clear(&tx, "acme", "2026-07-31T00:00:00.000Z").unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap(), None);
    }

    #[test]
    fn backfill_publishes_the_daemon_duty_blob() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut own = crate::store::health::HealthMonitorState::empty("acme");
        own.last_run_at = Some("2026-07-31T00:00:00.000Z".into());
        own.incidents.insert(
            "fp-daemon".into(),
            crate::store::health::HealthMonitorIncident {
                fingerprint: "fp-daemon".into(),
                kind: "supervisor_duty_stalled".into(),
                detail: "duty silent".into(),
                first_seen_at: "2026-07-31T00:00:00.000Z".into(),
                last_seen_at: "2026-07-31T00:00:00.000Z".into(),
                count: 2,
                responsible_person_id: None,
                unblock_action: None,
                observed_count: None,
                oldest_at: None,
                acknowledged_at: None,
                impaired_mailbox_person_id: None,
                alert_recipient_person_id: None,
            },
        );
        let blob = serde_json::to_vec(&own).unwrap();
        backfill_health_monitor(&tx, "acme", &blob).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got.last_run_at.as_deref(), Some("2026-07-31T00:00:00.000Z"));
        assert_eq!(got.incidents["fp-daemon"].kind, "supervisor_duty_stalled");
        assert_eq!(got.incidents["fp-daemon"].count, 2);
    }
}
