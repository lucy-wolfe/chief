//! The supervision ROW implementation (org-data-normalization P0, N3).
//!
//! Reconstruct a [`SupervisionLedger`] from the normalized tables, and publish a
//! whole ledger by diffing it against the current rows — the N3 half of the
//! repository seam, copying `store::organization_rows` (own DTO + own diff,
//! shared `apply_and_emit` scaffold). A CHILD module of `supervision` so it can
//! touch the ledger's private relational fields and name the store, exactly what
//! `fence_containment` fences shut for any OUTSIDE module.
//!
//! validate() here is INTERNAL-INVARIANTS-ONLY (Fable + N2/N5 rule): the table
//! CHECKs/FKs (status enums, goal coherence, fresh one-open positive-list,
//! priority_mode) plus the priority ENUM-LABEL check are the whole gate.
//! Manager-membership
//! / reconcile-supervision-for-read stays TS-side (pre-N9 the manifest rows are
//! empty, so a Rust reconstruct of the roster would spuriously reject) — this
//! module never reconstructs the manifest.
//!
//! Item D: a normalized ledger carries NO unmodeled keys (every `extra` map was
//! promoted to named columns, empty-control clean). Publish REJECTS any residual
//! `extra` with [`crate::store::organization_rows::UNMODELED_KEYS`] + the paths.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, Transaction};

use super::{Effect, Reminder, SupervisionLedger, SUPERVISION_SCHEMA_VERSION};
use crate::error::Refusal;
use crate::isotime::parse_iso_millis;
use crate::ledger::NEXT_EFFECT_SEQUENCE;
use crate::store::organization_rows::UNMODELED_KEYS;
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::ChiefdError;

const SUPERVISION_INVALID: &str = "supervision-invalid";

/// This store's own documents key (`SupervisionStore::NAME`, supervision.rs),
/// named HERE too so `persist_dispatch`/`load_ledgers` can dispatch/reconstruct
/// without reaching across module boundaries for it -- this module is a
/// registered co-owner of the supervision key (fence_containment.rs
/// `allowed_files`), the columnar persistence half of the same store.
pub const SUPERVISION_STORE: &str = "supervision";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("supervision-rows", e)
}
fn invalid(message: impl Into<String>) -> ChiefdError {
    ChiefdError::from(Refusal::new(SUPERVISION_INVALID, message))
}

/// The direct row-apply helper requires `E: From<rusqlite::Error>`;
/// `ChiefdError` has none (other crates rely on that), so the diff closure runs
/// in this wrapper and is unwrapped by [`publish`]. Mirror of
/// `organization_rows::RowsSqlError`.
struct SupSqlError(ChiefdError);
impl From<rusqlite::Error> for SupSqlError {
    fn from(e: rusqlite::Error) -> Self {
        SupSqlError(store_failure(e))
    }
}
impl From<ChiefdError> for SupSqlError {
    fn from(e: ChiefdError) -> Self {
        SupSqlError(e)
    }
}

// ---- reconstruct (read path) ---------------------------------------------

/// Reconstruct the ledger for `company_slug` from the rows keyed by `row_slug`,
/// or `None` when there is no `supervision_meta` row (never seeded / removed).
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure or a row that cannot map.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<Option<SupervisionLedger>, ChiefdError> {
    let Some(created_at) = read_meta(tx, row_slug)? else {
        return Ok(None);
    };

    let (reminder_order, reminders) = read_reminders(tx, row_slug)?;
    let (effect_order, effects) = read_effects(tx, row_slug)?;
    let next_effect_sequence =
        read_counter(tx, &slug_counter_name(NEXT_EFFECT_SEQUENCE, row_slug))?.max(1);

    let updated_at: Option<String> = tx
        .query_row("SELECT MAX(at) FROM org_events WHERE slug = ?1", params![row_slug], |r| {
            r.get(0)
        })
        .map_err(store_failure)?;

    let ledger = SupervisionLedger {
        schema_version: SUPERVISION_SCHEMA_VERSION,
        organization: company_slug.to_string(),
        reminder_order,
        reminders,
        created_at: created_at.clone(),
        updated_at: updated_at.unwrap_or(created_at),
        effects,
        effect_order,
        next_effect_sequence,
    };
    // Read-TOLERANCE / write-STRICT split (#337): the row read must never wedge
    // the org on a legacy-shaped ledger — reconstruct DROPS the explicitly
    // allowlisted legacy keys (see [`READ_TOLERATED_LEGACY_KEYS`]) rather than treating them as
    // corruption, while [`publish`] stays STRICT and refuses them so writers
    // converge onto the modeled shape. Reconstruct builds every family from named
    // columns, so this is a no-op safety net on live data (empty-control,
    // 2026-07-25) — it earns its keep the day a legacy row round-trips through.
    let mut ledger = ledger;
    enforce_read_tolerance(&mut ledger)?;
    Ok(Some(ledger))
}

/// Legacy keys the READ path tolerates (drops) rather than treating as
/// corruption — the ONLY unmodeled keys reconstruct will silently absorb.
///
/// These are columns DROPPED by earlier reshapes of this store: an old
/// `id`/`goalId` linkage and the pre-rename
/// `cadenceMs` (→ `intervalMs`). A ledger deserialized from an old blob surfaces
/// them in `extra` (serde-flatten); the row model no longer carries them, so read
/// discards them and write refuses them.
pub(super) const READ_TOLERATED_LEGACY_KEYS: &[&str] = &["id", "goalId", "cadenceMs"];

/// The read half of the read-TOLERANCE / write-STRICT split: strip the
/// allowlisted legacy keys from every entity's `extra`, and refuse anything else
/// as [`ChiefdError::Corrupt`] (an unmodeled key that is NOT a known-legacy drop
/// is genuine damage, not tolerable drift).
fn enforce_read_tolerance(l: &mut SupervisionLedger) -> Result<(), ChiefdError> {
    let tolerate = |extra: &mut BTreeMap<String, serde_json::Value>| -> Result<(), ChiefdError> {
        extra.retain(|k, _| !READ_TOLERATED_LEGACY_KEYS.contains(&k.as_str()));
        if extra.is_empty() {
            Ok(())
        } else {
            // Name the keys. "supervision-rows is corrupt" sends an operator to
            // a whole ledger; the key that is not on the allowlist sends them to
            // one field.
            let mut keys: Vec<&str> = extra.keys().map(String::as_str).collect();
            keys.sort_unstable();
            Err(crate::error::corrupt_store_because(
                "supervision-rows",
                format!("unmodeled key(s) outside the read allowlist: {}", keys.join(", ")),
            ))
        }
    };
    for r in l.reminders.values_mut() {
        tolerate(&mut r.extra)?;
    }
    Ok(())
}

fn read_meta(tx: &Transaction<'_>, slug: &str) -> Result<Option<String>, ChiefdError> {
    let mut stmt = tx
        .prepare("SELECT created_at FROM supervision_meta WHERE slug=?1")
        .map_err(store_failure)?;
    let mut rows = stmt.query(params![slug]).map_err(store_failure)?;
    let Some(row) = rows.next().map_err(store_failure)? else {
        return Ok(None);
    };
    Ok(Some(row.get::<_, String>(0).map_err(store_failure)?))
}

/// The per-company counter row name (delta #36): the `counters` table is name-PK
/// (no slug column), so the supervision sequence counters embed the slug in their
/// NAME — the same D2 per-slug pattern `org_events`/`staffing` seqs use — keeping
/// one company's NEXT_EFFECT_SEQUENCE from clobbering
/// another's on the shared slug-multiplexed org.sqlite.
fn slug_counter_name(base: &str, slug: &str) -> String {
    format!("{base}:{slug}")
}

fn read_counter(tx: &Transaction<'_>, name: &str) -> Result<u64, ChiefdError> {
    let value: Option<i64> = tx
        .query_row("SELECT value FROM counters WHERE name=?1", params![name], |r| r.get(0))
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => e,
            other => other,
        })
        .ok();
    Ok(u64::try_from(value.unwrap_or(1)).unwrap_or(1))
}

fn read_reminders(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<(Vec<String>, BTreeMap<String, Reminder>), ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT id, person_id, created_by_person_id, prompt, interval_ms, next_due_at, \
             status, recurring, fire_count, created_at, last_fired_at, expires_at, \
             stopped_reason, stopped_at FROM reminders WHERE slug=?1 ORDER BY created_at, id",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug], |r| {
            Ok(Reminder {
                id: r.get(0)?,
                person_id: r.get(1)?,
                created_by_person_id: r.get(2)?,
                prompt: r.get(3)?,
                interval_ms: r.get(4)?,
                next_due_at: r.get(5)?,
                status: r.get(6)?,
                recurring: r.get::<_, i64>(7)? != 0,
                fire_count: u64::try_from(r.get::<_, i64>(8)?).unwrap_or(0),
                created_at: r.get(9)?,
                last_fired_at: r.get(10)?,
                expires_at: r.get(11)?,
                stopped_reason: r.get(12)?,
                stopped_at: r.get(13)?,
                extra: BTreeMap::new(),
            })
        })
        .map_err(store_failure)?;
    let mut order = Vec::new();
    let mut map = BTreeMap::new();
    for row in rows {
        let r = row.map_err(store_failure)?;
        order.push(r.id.clone());
        map.insert(r.id.clone(), r);
    }
    Ok((order, map))
}

fn read_effects(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<(Vec<String>, BTreeMap<String, Effect>), ChiefdError> {
    let mut stmt = tx
        .prepare("SELECT id, seq, kind, status, created_at, delivered_at, superseded_at, delivery_failure_count, last_delivery_failure_at, failed_at, reopen_count, last_reopened_at FROM effects WHERE slug=?1 ORDER BY seq")
        .map_err(store_failure)?;
    let raw = stmt
        .query_map(params![slug], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })
        .map_err(store_failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_failure)?;
    let mut order = Vec::new();
    let mut map = BTreeMap::new();
    for (
        id,
        seq,
        kind,
        status,
        created_at,
        delivered_at,
        superseded_at,
        delivery_failure_count,
        last_delivery_failure_at,
        failed_at,
        reopen_count,
        last_reopened_at,
    ) in raw
    {
        // `json!` of an object literal is always an object; the let-else just
        // keeps the invariant honest without a denied `expect`.
        let serde_json::Value::Object(mut object) = serde_json::json!({"id":id,"sequence":seq,"type":kind,"status":status,"createdAt":created_at,"deliveredAt":delivered_at.map(crate::isotime::iso_millis),"supersededAt":superseded_at,"deliveryFailureCount":delivery_failure_count,"lastDeliveryFailureAt":last_delivery_failure_at,"failedAt":failed_at,"reopenCount":reopen_count,"lastReopenedAt":last_reopened_at})
        else {
            return Err(invalid(format!("effect '{id}' did not build an object")));
        };
        append_effect_payload(tx, slug, &id, &mut object)?;
        let e: Effect = serde_json::from_value(serde_json::Value::Object(object))
            .map_err(|err| invalid(format!("effect '{id}' does not decode: {err}")))?;
        order.push(id.clone());
        map.insert(id, e);
    }
    Ok((order, map))
}

/// A malformed normalized effect payload, distinguished from an SQLite fault
/// so a writer can refuse an unsupported wire value while a reader fails the
/// store closed as corrupt.
#[derive(Debug)]
pub(crate) enum EffectPayloadError {
    /// SQLite could not read or write the child relation.
    Database(rusqlite::Error),
    /// Rows or an incoming value did not match the lossless scalar/array model.
    Invalid(String),
}

impl From<rusqlite::Error> for EffectPayloadError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

fn payload_scalar(
    field: &str,
    text: Option<String>,
    integer: Option<i64>,
    boolean: Option<i64>,
) -> Result<serde_json::Value, EffectPayloadError> {
    match (text, integer, boolean) {
        (Some(value), None, None) => Ok(serde_json::Value::String(value)),
        (None, Some(value), None) => Ok(serde_json::Value::Number(value.into())),
        (None, None, Some(0)) => Ok(serde_json::Value::Bool(false)),
        (None, None, Some(1)) => Ok(serde_json::Value::Bool(true)),
        values => Err(EffectPayloadError::Invalid(format!(
            "effect payload field '{field}' has an invalid scalar tuple: {values:?}"
        ))),
    }
}

/// Read one effect's kind-specific payload from the normalized child relation.
/// `is_array` is authoritative even for a singleton array. Empty arrays use
/// the schema-constrained `(is_array=1, ordinal=-1, NULL values)` marker.
pub(crate) fn read_effect_payload(
    conn: &Connection,
    slug: &str,
    effect_id: &str,
) -> Result<BTreeMap<String, serde_json::Value>, EffectPayloadError> {
    let mut stmt = conn.prepare(
        "SELECT field, ordinal, is_array, value_text, value_integer, value_boolean \
         FROM effect_payloads \
         WHERE slug=?1 AND effect_id=?2 \
         ORDER BY field, ordinal",
    )?;
    let rows = stmt
        .query_map(params![slug, effect_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut payload = BTreeMap::new();
    let mut empty_array_markers = BTreeSet::new();
    for (field, ordinal, is_array, text, integer, boolean) in rows {
        match (is_array, ordinal) {
            (1, -1) => {
                if text.is_some() || integer.is_some() || boolean.is_some() {
                    return Err(EffectPayloadError::Invalid(format!(
                        "effect '{effect_id}' payload '{field}' has a valued empty-array marker"
                    )));
                }
                if payload.insert(field.clone(), serde_json::Value::Array(Vec::new())).is_some() {
                    return Err(EffectPayloadError::Invalid(format!(
                        "effect '{effect_id}' payload '{field}' has conflicting shape rows"
                    )));
                }
                empty_array_markers.insert(field);
            }
            (0, 0) => {
                let scalar = payload_scalar(&field, text, integer, boolean)?;
                if payload.insert(field.clone(), scalar).is_some() {
                    return Err(EffectPayloadError::Invalid(format!(
                        "effect '{effect_id}' payload '{field}' has duplicate scalar rows"
                    )));
                }
            }
            (1, ordinal) if ordinal >= 0 => {
                if empty_array_markers.contains(&field) {
                    return Err(EffectPayloadError::Invalid(format!(
                        "effect '{effect_id}' payload '{field}' mixes an empty marker with values"
                    )));
                }
                let scalar = payload_scalar(&field, text, integer, boolean)?;
                let entry = payload
                    .entry(field.clone())
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                let values = entry.as_array_mut().ok_or_else(|| {
                    EffectPayloadError::Invalid(format!(
                        "effect '{effect_id}' payload '{field}' mixes scalar and array rows"
                    ))
                })?;
                let expected = i64::try_from(values.len()).unwrap_or(i64::MAX);
                if ordinal != expected {
                    return Err(EffectPayloadError::Invalid(format!(
                        "effect '{effect_id}' payload '{field}' ordinal {ordinal} is not contiguous from {expected}"
                    )));
                }
                values.push(scalar);
            }
            _ => {
                return Err(EffectPayloadError::Invalid(format!(
                    "effect '{effect_id}' payload '{field}' has invalid shape is_array={is_array}, ordinal={ordinal}"
                )));
            }
        }
    }
    Ok(payload)
}

/// Replace one effect's normalized payload child rows without storing JSON.
/// Scalars, singleton arrays, multi-value arrays, and empty arrays each have a
/// distinct relational representation.
pub(crate) fn replace_effect_payload(
    conn: &Connection,
    slug: &str,
    effect_id: &str,
    payload: &BTreeMap<String, serde_json::Value>,
) -> Result<(), EffectPayloadError> {
    conn.execute(
        "DELETE FROM effect_payloads WHERE slug=?1 AND effect_id=?2",
        params![slug, effect_id],
    )?;
    for (field, value) in payload {
        let (is_array, values): (i64, Vec<&serde_json::Value>) = match value {
            serde_json::Value::Array(items) => (1, items.iter().collect()),
            _ => (0, vec![value]),
        };
        if is_array == 1 && values.is_empty() {
            conn.execute(
                "INSERT INTO effect_payloads(
                    slug, effect_id, field, ordinal, is_array,
                    value_text, value_integer, value_boolean
                 ) VALUES (?1, ?2, ?3, -1, 1, NULL, NULL, NULL)",
                params![slug, effect_id, field],
            )?;
            continue;
        }
        for (ordinal, scalar) in values.into_iter().enumerate() {
            let (text, integer, boolean) = match scalar {
                serde_json::Value::String(value) => (Some(value.as_str()), None, None),
                serde_json::Value::Number(value) => {
                    let Some(value) = value.as_i64() else {
                        return Err(EffectPayloadError::Invalid(format!(
                            "effect '{effect_id}' payload '{field}' number is not an integer"
                        )));
                    };
                    (None, Some(value), None)
                }
                serde_json::Value::Bool(value) => (None, None, Some(i64::from(*value))),
                _ => {
                    return Err(EffectPayloadError::Invalid(format!(
                        "effect '{effect_id}' payload '{field}' is not a scalar or scalar array"
                    )));
                }
            };
            conn.execute(
                "INSERT INTO effect_payloads(
                    slug, effect_id, field, ordinal, is_array,
                    value_text, value_integer, value_boolean
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    slug,
                    effect_id,
                    field,
                    i64::try_from(ordinal).unwrap_or(i64::MAX),
                    is_array,
                    text,
                    integer,
                    boolean
                ],
            )?;
        }
    }
    Ok(())
}

/// Reconstruct an effect's kind-specific scalar fields from their child rows.
/// JSON itself never crosses the SQL persistence boundary.
fn append_effect_payload(
    tx: &Transaction<'_>,
    slug: &str,
    effect_id: &str,
    object: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), ChiefdError> {
    let payload = read_effect_payload(tx, slug, effect_id).map_err(|error| match error {
        EffectPayloadError::Database(error) => store_failure(error),
        EffectPayloadError::Invalid(detail) => crate::error::corrupt_store_because(
            "supervision-rows",
            format!("effect '{effect_id}' has an invalid stored payload: {detail}"),
        ),
    })?;
    for (field, value) in payload {
        // Name the duplicate before `insert` takes ownership of the key.
        let duplicated = field.clone();
        if object.insert(field, value).is_some() {
            return Err(crate::error::corrupt_store_because(
                "supervision-rows",
                format!("effect '{effect_id}' stores field '{duplicated}' twice"),
            ));
        }
    }
    Ok(())
}

// ---- publish (diff/write path) -------------------------------------------

/// Publish a whole ledger into the rows as a direct current-state mutation.
/// Rejects unmodeled keys (item D), validates INTERNAL invariants (priority enum
/// label; the table CHECKs/FKs enforce the rest), then diffs each family at
/// entity granularity. `company_slug` is accepted for signature parity with the
/// manifest port but the ledger is self-describing; `row_slug` keys every table.
///
/// # Errors
/// [`UNMODELED_KEYS`] / `supervision-invalid` refusals (→ 422); SQL failures as
/// [`ChiefdError::Corrupt`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming: &SupervisionLedger,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    validate_internal(incoming)?;

    let at = incoming.updated_at.clone();
    apply_and_emit::<SupSqlError, _>(tx, row_slug, &at, "", |tx| {
        let mut touches = Vec::new();
        write_meta(tx, row_slug, incoming, &mut touches)?;
        diff_reminders(tx, row_slug, incoming, &mut touches)?;
        // The launcher-authored relational half: effects and the
        // NEXT_EFFECT_SEQUENCE counter. `reconstruct` reads these back, so the
        // write flip (task #53) DROPS them unless publish persists them here —
        // mirrors `supervision::ingest_external_document`'s flush_effect /
        // set_counter(NEXT_EFFECT_SEQUENCE) reference path.
        //
        // Leak-safe (delta #36): `effects` carries a `slug` column, and the
        // `NEXT_EFFECT_SEQUENCE` counter embeds the slug in its NAME (the
        // `counters` table stays name-PK, D2-style) — so one company's publish
        // never delete-absents another's rows on the shared org.sqlite.
        diff_effects(tx, row_slug, incoming, &mut touches)?;
        write_counter(
            tx,
            &slug_counter_name(NEXT_EFFECT_SEQUENCE, row_slug),
            incoming.next_effect_sequence,
        )?;
        Ok(touches)
    })
    .map_err(|SupSqlError(e)| e)
}

/// BLOB-DEATH (N8): meta-only publish for the actor's dispatch/commit path.
///
/// `SupervisionLedger` `#[serde(skip)]`s `effects` and
/// `next_effect_sequence` (supervision.rs) -- those families live in
/// SEPARATE relational tables written by the actor's own `relational_diff`
/// path (writer.rs), never by the documents-blob dispatch. A ledger decoded
/// from the blob therefore always has EMPTY effects, and calling
/// the full [`publish`] with it would DELETE every real effect row
/// (a diff against "incoming has none" absents everything present) -- exactly
/// the data-corruption hazard this function exists to avoid.
///
/// `publish_meta` writes ONLY the blob-derived meta tables: `write_meta` plus
/// the manager-goal / delegated-goal / reminder / check-in / goal-watch
/// families. It deliberately OMITS
/// `diff_effects` and `write_counter(NEXT_EFFECT_SEQUENCE)`
/// -- those stay solely owned by `relational_diff`, and the two halves are
/// disjoint (meta tables vs. the effect table), so there is no
/// double-write between this path and the actor's relational commit.
///
/// The full [`publish`] remains unchanged and is still used by the `/v1/org`
/// route, which receives a COMPLETE ledger (effects included) from
/// the caller and legitimately owns the whole diff.
///
/// # Errors
/// [`UNMODELED_KEYS`] / `supervision-invalid` refusals (→ 422); SQL failures as
/// [`ChiefdError::Corrupt`].
pub fn publish_meta(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming: &SupervisionLedger,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    validate_internal(incoming)?;

    let at = incoming.updated_at.clone();
    apply_and_emit::<SupSqlError, _>(tx, row_slug, &at, "", |tx| {
        let mut touches = Vec::new();
        write_meta(tx, row_slug, incoming, &mut touches)?;
        diff_reminders(tx, row_slug, incoming, &mut touches)?;
        Ok(touches)
    })
    .map_err(|SupSqlError(e)| e)
}

/// Remove EVERY supervision row for `row_slug` — the row-path teardown that
/// makes [`reconstruct`] return `None` (no `supervision_meta` row). This is the
/// counterpart the blob path always had (`drop_company_store` clears only the
/// `org_documents` blob); without it nothing makes `supervisionRowRead` return
/// absent, so the refuses-absent / deleteDoc contract cannot be set up.
///
/// Mirrors `launch_intent_rows::clear`: a REAL delete (the meta row becomes
/// ABSENT, not an empty ledger), fence-free — it runs inside the writer's own
/// `BEGIN IMMEDIATE` via [`apply_and_emit`], emitting one delete touch per row.
///
/// Everything is slug-scoped (delta #36): the slug-scoped families by their
/// `slug` column, `effects` by its `slug` column, and the
/// `NEXT_EFFECT_SEQUENCE` counter by its
/// slug-embedded NAME — so clearing one company never touches another's rows.
///
/// # Errors
/// SQL failures as [`ChiefdError::StoreFailure`].
pub fn clear(tx: &Transaction<'_>, row_slug: &str, at: &str) -> Result<(), ChiefdError> {
    apply_and_emit::<SupSqlError, _>(tx, row_slug, at, "", |tx| {
        let mut touches = Vec::new();
        // Slug-scoped families.
        for (table, entity, id_col) in [("reminders", "reminder", "id")] {
            let ids: Vec<String> = tx
                .prepare(&format!("SELECT {id_col} FROM {table} WHERE slug=?1"))?
                .query_map(params![row_slug], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for id in ids {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE slug=?1 AND {id_col}=?2"),
                    params![row_slug, id],
                )?;
                touches.push(EventTouch::new(entity, id, "delete", table, row_slug));
            }
        }
        // Relational half (slug-scoped, delta #36) + the slug-named counters.
        for (table, entity) in [("effects", "effect")] {
            let ids: Vec<String> = tx
                .prepare(&format!("SELECT id FROM {table} WHERE slug=?1"))?
                .query_map(params![row_slug], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for id in ids {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE slug=?1 AND id=?2"),
                    params![row_slug, id],
                )?;
                touches.push(EventTouch::new(entity, id, "delete", table, row_slug));
            }
        }
        tx.execute(
            "DELETE FROM counters WHERE name=?1",
            params![slug_counter_name(NEXT_EFFECT_SEQUENCE, row_slug)],
        )?;
        // Meta LAST: while it survives, a concurrent reconstruct still sees the
        // ledger; once gone, reconstruct returns None.
        tx.execute("DELETE FROM supervision_meta WHERE slug=?1", params![row_slug])?;
        touches.push(EventTouch::new(
            "supervision",
            row_slug,
            "delete",
            "supervision_meta",
            row_slug,
        ));
        Ok(touches)
    })
    .map(|_seq| ())
    .map_err(|SupSqlError(e)| e)
}

fn reject_unmodeled_keys(l: &SupervisionLedger) -> Result<(), ChiefdError> {
    let mut paths = Vec::new();
    for (id, r) in &l.reminders {
        for k in r.extra.keys() {
            paths.push(format!("reminders.{id}.{k}"));
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!("supervision ledger carries unmodeled keys: {}", paths.join(", ")),
    )))
}

/// GoalPriority is the SYMBOLIC enum LABEL — the stored authority (schema delta
/// #25; org-goal-priority.ts `GoalPriority`). It is NOT an integer: the earlier
/// INTEGER/200000 canonical was wrong (200000 is not even in the rank space) and
/// live data is 100% symbolic. Any numeric ORDERING is DERIVED from the rank map
/// (see [`priority_rank`]) at query time, never stored.
fn validate_internal(_l: &SupervisionLedger) -> Result<(), ChiefdError> {
    Ok(())
}

fn write_meta(
    tx: &Transaction<'_>,
    slug: &str,
    l: &SupervisionLedger,
    touches: &mut Vec<EventTouch>,
) -> Result<(), ChiefdError> {
    tx.execute(
        "INSERT INTO supervision_meta(slug, created_at) \
         VALUES(?1,?2) ON CONFLICT(slug) DO UPDATE SET created_at=?2",
        params![slug, l.created_at],
    )
    .map_err(store_failure)?;
    touches.push(EventTouch::new("supervision", slug, "upsert", "supervision_meta", slug));
    Ok(())
}

fn diff_reminders(
    tx: &Transaction<'_>,
    slug: &str,
    l: &SupervisionLedger,
    touches: &mut Vec<EventTouch>,
) -> Result<(), ChiefdError> {
    delete_absent(tx, slug, "reminders", l.reminders.keys(), "reminder", touches)?;
    for id in &l.reminder_order {
        let r = l
            .reminders
            .get(id)
            .ok_or_else(|| invalid(format!("reminder_order names unknown '{id}'")))?;
        tx.execute(
            "INSERT INTO reminders(slug, id, person_id, created_by_person_id, prompt, interval_ms, \
             next_due_at, status, recurring, fire_count, created_at, last_fired_at, expires_at, \
             stopped_reason, stopped_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15) \
             ON CONFLICT(slug,id) DO UPDATE SET person_id=?3, created_by_person_id=?4, prompt=?5, \
             interval_ms=?6, next_due_at=?7, status=?8, recurring=?9, fire_count=?10, \
             last_fired_at=?12, expires_at=?13, stopped_reason=?14, stopped_at=?15",
            params![
                slug, id, r.person_id, r.created_by_person_id, r.prompt, r.interval_ms,
                r.next_due_at, r.status, i64::from(r.recurring), r.fire_count as i64, r.created_at,
                r.last_fired_at, r.expires_at, r.stopped_reason, r.stopped_at,
            ],
        )
        .map_err(store_failure)?;
        touches.push(EventTouch::new("reminder", id, "upsert", "reminders", slug));
    }
    Ok(())
}

/// Upsert one `counters` row. Mirrors the M12 actor writer's counter persist
/// (`INSERT … ON CONFLICT(name) DO UPDATE SET value=?`).
fn write_counter(tx: &Transaction<'_>, name: &str, value: u64) -> Result<(), ChiefdError> {
    tx.execute(
        "INSERT INTO counters(name, value) VALUES(?1, ?2) \
         ON CONFLICT(name) DO UPDATE SET value=?2",
        params![name, i64::try_from(value).unwrap_or(i64::MAX)],
    )
    .map_err(store_failure)?;
    Ok(())
}

/// Diff + persist the launcher-authored `effects`. Mirrors
/// `supervision::flush_effect`: the [`Effect`] round-trips through `body` (the
/// column `reconstruct` reads), `seq` is the effect's own `sequence` (delta #36:
/// the per-company effect.sequence written under PK `(slug, seq)`, the global
/// AUTOINCREMENT dropped), and `delivered_at` is epoch-millis for the M12 read
/// path. Slug-scoped `WHERE slug=?` — one company's publish never touches another's.
fn diff_effects(
    tx: &Transaction<'_>,
    slug: &str,
    l: &SupervisionLedger,
    touches: &mut Vec<EventTouch>,
) -> Result<(), ChiefdError> {
    let current: Vec<String> = tx
        .prepare("SELECT id FROM effects WHERE slug=?1")
        .map_err(store_failure)?
        .query_map(params![slug], |r| r.get::<_, String>(0))
        .map_err(store_failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_failure)?;
    for id in current {
        if !l.effects.contains_key(&id) {
            tx.execute("DELETE FROM effects WHERE slug=?1 AND id=?2", params![slug, id])
                .map_err(store_failure)?;
            touches.push(EventTouch::new("effect", &id, "delete", "effects", slug));
        }
    }
    for id in &l.effect_order {
        let e = l
            .effects
            .get(id)
            .ok_or_else(|| invalid(format!("effect_order names unknown '{id}'")))?;
        let delivered_at = e.delivered_at.as_deref().and_then(parse_iso_millis);
        tx.execute(
            "INSERT INTO effects(slug,id,seq,kind,status,created_at,delivered_at,superseded_at,delivery_failure_count,last_delivery_failure_at,failed_at,reopen_count,last_reopened_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13) ON CONFLICT(slug,id) DO UPDATE SET seq=?3,kind=?4,status=?5,created_at=?6,delivered_at=?7,superseded_at=?8,delivery_failure_count=?9,last_delivery_failure_at=?10,failed_at=?11,reopen_count=?12,last_reopened_at=?13",
            params![
                slug,
                id,
                i64::try_from(e.sequence).unwrap_or(i64::MAX),
                e.kind,
                e.status.as_str(), e.created_at,
                delivered_at,
                e.superseded_at, e.delivery_failure_count.map(i64::from), e.last_delivery_failure_at,
                e.failed_at, e.reopen_count.map(i64::from), e.last_reopened_at,
            ],
        )
        .map_err(store_failure)?;
        replace_effect_payload(tx, slug, id, &e.payload).map_err(|error| match error {
            EffectPayloadError::Database(error) => store_failure(error),
            EffectPayloadError::Invalid(message) => invalid(message),
        })?;
        touches.push(EventTouch::new("effect", id, "upsert", "effects", slug));
    }
    Ok(())
}

/// Delete rows whose id (first PK component after slug) is absent from
/// `present`, emitting a delete touch each. Every remaining supervision table
/// keys on `(slug, id)`; the `manager_person_id` variant went with the manager
/// goal tables.
fn delete_absent<'a>(
    tx: &Transaction<'_>,
    slug: &str,
    table: &str,
    present: impl Iterator<Item = &'a String>,
    entity: &str,
    touches: &mut Vec<EventTouch>,
) -> Result<(), ChiefdError> {
    let id_col = "id";
    let keep: std::collections::BTreeSet<&str> = present.map(String::as_str).collect();
    let current: Vec<String> = tx
        .prepare(&format!("SELECT {id_col} FROM {table} WHERE slug=?1"))
        .map_err(store_failure)?
        .query_map(params![slug], |r| r.get::<_, String>(0))
        .map_err(store_failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_failure)?;
    for id in current {
        if !keep.contains(id.as_str()) {
            tx.execute(
                &format!("DELETE FROM {table} WHERE slug=?1 AND {id_col}=?2"),
                params![slug, id],
            )
            .map_err(store_failure)?;
            touches.push(EventTouch::new(entity, &id, "delete", table, slug));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Item-D conformance for the supervision row port: the read-TOLERANCE /
    //! write-STRICT split (#337). A ledger carrying an ALLOWLISTED legacy key
    //! (the dropped pre-normalization goal-watch shape) is TOLERATED on read
    //! (the key is dropped) but REFUSED on publish; a NON-allowlisted unmodeled
    //! key is refused on BOTH halves.

    use super::*;
    use crate::store::supervision::SupervisionLedger;
    use crate::store::{open_company_db, COMPANY_DB_FILENAME};

    /// A minimal empty-but-valid supervision ledger for `slug`, with the
    /// relational half (`#[serde(skip)]`, so JSON never carries it) left empty.
    fn empty_ledger(slug: &str) -> SupervisionLedger {
        let json = serde_json::json!({
            "schemaVersion": 2,
            "organization": slug,
            "reminderOrder": [],
            "reminders": {},
            "createdAt": "2026-07-25T00:00:00.000Z",
            "updatedAt": "2026-07-25T00:00:00.000Z",
        });
        serde_json::from_value(json).expect("empty ledger deserializes")
    }

    fn effect(id: &str, sequence: u64) -> Effect {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "sequence": sequence,
            "type": "person_reminder",
            "status": "pending",
            "createdAt": "2026-07-25T00:00:00.000Z",
            "reminderId": "r1",
        }))
        .expect("effect deserializes")
    }

    /// THE GAP this slice closes: publish must persist the launcher-authored
    /// effects + NEXT_EFFECT_SEQUENCE so reconstruct round-trips
    /// them. Before this, publish dropped all three → reconstruct returned empty
    /// effects + a reset counter → "effect sequence is invalid".
    #[test]
    fn effects_and_counter_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut conn =
            open_company_db(&dir.path().join(COMPANY_DB_FILENAME)).expect("open company db");
        let slug = "cobalt@rc";

        let mut incoming = empty_ledger(slug);
        incoming.effect_order = vec!["e1".to_string(), "e2".to_string()];
        incoming.effects.insert("e1".to_string(), effect("e1", 1));
        incoming.effects.insert("e2".to_string(), effect("e2", 2));
        incoming.next_effect_sequence = 3;

        let tx = conn.transaction().expect("txn");
        let outcome = publish(&tx, slug, &incoming).expect("publish");
        assert!(outcome > 0, "first publish must emit an audit cursor, got {outcome}");
        tx.commit().expect("commit");

        // Regression: both live supervision persistence paths must reconstruct
        // from normalized fields/child rows. Fresh DDL must not even expose an
        // opaque compatibility body column that a future writer could revive.
        // `effects` is the only table left with this rule to keep -- the goal
        // and assignment tables it also covered are deleted.
        let table = "effects";
        let body_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name='body'",
                params![table],
                |row| row.get(0),
            )
            .expect("query table columns");
        assert_eq!(body_columns, 0, "{table} must not expose a body column");
        let payload_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM effect_payloads WHERE slug=?1 AND field='reminderId'",
                params![slug],
                |row| row.get(0),
            )
            .expect("query normalized payload rows");
        assert_eq!(payload_rows, 2, "effect payloads persist as child rows");

        let tx = conn.transaction().expect("txn");
        let round = reconstruct(&tx, slug, slug).expect("reconstruct").expect("present");
        assert_eq!(round.effect_order, vec!["e1".to_string(), "e2".to_string()]);
        assert_eq!(round.effects, incoming.effects, "effects round-trip via normalized child rows");
        assert_eq!(
            round.next_effect_sequence, 3,
            "NEXT_EFFECT_SEQUENCE advances — without it validate throws 'effect sequence is invalid'"
        );
        drop(tx);

        // A second publish that REMOVES an effect must delete the
        // absent rows (diff, not append-only), preserving the kept identity.
        let mut shrunk = empty_ledger(slug);
        shrunk.effect_order = vec!["e1".to_string()];
        shrunk.effects.insert("e1".to_string(), effect("e1", 1));
        shrunk.next_effect_sequence = 3;
        let tx = conn.transaction().expect("txn");
        let outcome = publish(&tx, slug, &shrunk).expect("second publish");
        assert!(outcome > 0, "got audit cursor {outcome}");
        tx.commit().expect("commit");
        let tx = conn.transaction().expect("txn");
        let round = reconstruct(&tx, slug, slug).expect("reconstruct").expect("present");
        assert_eq!(round.effect_order, vec!["e1".to_string()], "e2 deleted");
    }

    /// Delta #36 leak-safety: with effects slug-scoped (PK on slug)
    /// and the NEXT_EFFECT_SEQUENCE counter slug-named, a second company's publish
    /// must NOT delete-absent the first company's effects - the
    /// destructive cross-company leak #53 caught, now CLOSED. Two companies in one
    /// shared org.sqlite stay fully isolated across every supervision family.
    #[test]
    fn a_second_companys_publish_never_delete_absents_the_firsts_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut conn =
            open_company_db(&dir.path().join(COMPANY_DB_FILENAME)).expect("open company db");
        let a = "alpha@rc";
        let b = "bravo@rc";

        let mut la = empty_ledger(a);
        la.effect_order = vec!["e1".to_string()];
        la.effects.insert("e1".to_string(), effect("e1", 1));
        la.next_effect_sequence = 2;
        let tx = conn.transaction().expect("txn");
        publish(&tx, a, &la).expect("publish A");
        tx.commit().expect("commit");

        // B publishes its OWN effect (none of A's) in the SAME db.
        let mut lb = empty_ledger(b);
        lb.effect_order = vec!["b-e1".to_string()];
        lb.effects.insert("b-e1".to_string(), effect("b-e1", 1));
        lb.next_effect_sequence = 2;
        let tx = conn.transaction().expect("txn");
        publish(&tx, b, &lb).expect("publish B");
        tx.commit().expect("commit");

        // A is FULLY intact: B's slug-scoped publish delete-absented nothing of A's.
        let tx = conn.transaction().expect("txn");
        let ra = reconstruct(&tx, a, a).expect("reconstruct A").expect("present");
        assert_eq!(ra.effect_order, vec!["e1".to_string()], "A effect isolated");
        assert_eq!(ra.next_effect_sequence, 2, "A counter isolated (slug-named)");
        // B sees ONLY its own rows.
        let rb = reconstruct(&tx, b, b).expect("reconstruct B").expect("present");
        assert_eq!(rb.effect_order, vec!["b-e1".to_string()], "B sees only its own");
    }

    /// The row-path teardown: after `clear`, reconstruct returns None (the
    /// `supervision_meta` row is gone), which is what makes supervisionRowRead
    /// return absent so the refuses-absent / deleteDoc contract can be set up.
    #[test]
    fn clear_makes_reconstruct_return_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut conn =
            open_company_db(&dir.path().join(COMPANY_DB_FILENAME)).expect("open company db");
        let slug = "cobalt@rc";

        let mut incoming = empty_ledger(slug);
        incoming.effect_order = vec!["e1".to_string()];
        incoming.effects.insert("e1".to_string(), effect("e1", 1));
        incoming.next_effect_sequence = 2;

        let tx = conn.transaction().expect("txn");
        publish(&tx, slug, &incoming).expect("publish");
        tx.commit().expect("commit");

        let tx = conn.transaction().expect("txn");
        assert!(
            reconstruct(&tx, slug, slug).expect("reconstruct").is_some(),
            "published ledger reconstructs before clear"
        );
        drop(tx);

        let tx = conn.transaction().expect("txn");
        clear(&tx, slug, "2026-07-25T01:00:00.000Z").expect("clear");
        tx.commit().expect("commit");

        let tx = conn.transaction().expect("txn");
        assert!(
            reconstruct(&tx, slug, slug).expect("reconstruct").is_none(),
            "after clear the meta row is gone → reconstruct returns None"
        );
        // The relational rows are gone too, not orphaned.
        let effects: i64 =
            tx.query_row("SELECT COUNT(*) FROM effects", [], |r| r.get(0)).expect("count effects");
        assert_eq!(effects, 0, "effects cleared");
    }

    #[test]
    fn clear_on_absent_is_a_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut conn =
            open_company_db(&dir.path().join(COMPANY_DB_FILENAME)).expect("open company db");
        let slug = "never@seeded";
        let tx = conn.transaction().expect("txn");
        clear(&tx, slug, "2026-07-25T01:00:00.000Z").expect("clear is a no-op");
        assert!(reconstruct(&tx, slug, slug).expect("reconstruct").is_none());
    }
}
