//! The `runtime-owner` singleton ROW implementation (org-data-normalization P0,
//! N2 copy-pattern, B4 singleton sweep).
//!
//! Scalar singleton (`runtime_owner` table, `slug` PK, delta #24 reshape). Holds
//! the runtime-ownership claim: `status` ('active'|'released') plus the optional
//! `socketName`/`claimedAt`/`validatedAt`/`releasedAt` lifecycle stamps.
//! DERIVED, not stored: `version` = const `1`; `organization` = company slug.
//!
//! # RETIRED (AC6): `sessionName` and its `session NOT NULL` column
//!
//! The column stored `org-<slug>`, written from the manifest and only ever
//! compared against another derivation from the same slug, so it distinguished
//! nothing and could not disagree with anything. It also put a tmux session
//! name on `/v1/org/runtime-owner/read`. The column is absent from
//! `schema::COMPANY_SCHEMA_SQL`, so no company database grows it; the field is
//! gone from the model, so a legacy
//! blob that still carries `sessionName` lands in `extra`. It is DROPPED
//! there, not refused: the captured legacy blob really does carry the key
//! (`shadow_diff_is_zero_loss_on_live_blob`), and refusing it would make every
//! historical `org_documents` row unbackfillable on upgrade. Nothing is lost —
//! the value was `org-<slug>` for the very slug this row is keyed by. Any
//! OTHER unmodeled key is still refused loudly.
//!
//! Item D: publish REJECTS any serde-flatten `extra` with [`UNMODELED_KEYS`].

use std::collections::BTreeMap;

use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::corrupt_store;
use crate::error::Refusal;
use crate::store::organization_rows::{RowsSqlError, UNMODELED_KEYS};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::store::shadow_report::{Disposition, ShadowReport};
use crate::ChiefdError;

/// A `runtime-owner` singleton. Mirrors the TS `OrganizationRuntimeOwnership`;
/// `version`/`organization` are DERIVED on reconstruct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeOwner {
    /// Always `1`. Not stored.
    pub version: u32,
    /// The company slug — DERIVED, not stored.
    pub organization: String,
    /// Claim status.
    pub status: RuntimeOwnerStatus,
    /// The runtime socket name, when claimed.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub socket_name: Option<String>,
    /// When the claim was taken.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub claimed_at: Option<String>,
    /// When the claim was last validated live.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub validated_at: Option<String>,
    /// When the claim was released.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub released_at: Option<String>,
    /// Any unmodeled key (item D).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The claim status enum (matches the `status` CHECK).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeOwnerStatus {
    /// The runtime is actively owned.
    Active,
    /// The claim has been released.
    Released,
}

impl RuntimeOwnerStatus {
    fn as_text(self) -> &'static str {
        match self {
            RuntimeOwnerStatus::Active => "active",
            RuntimeOwnerStatus::Released => "released",
        }
    }
    fn from_text(s: &str) -> Result<Self, ChiefdError> {
        match s {
            "active" => Ok(RuntimeOwnerStatus::Active),
            "released" => Ok(RuntimeOwnerStatus::Released),
            other => Err(crate::error::corrupt_store_because(
                "runtime-owner-rows",
                format!("stored runtime-owner status '{other}' is not 'active' or 'released'"),
            )),
        }
    }
}

/// The `org_documents` store family this row set replaces.
pub const RUNTIME_OWNER_STORE: &str = "runtime-owner";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("runtime-owner-rows", e)
}

/// Reconstruct the runtime-owner singleton for `company_slug`, or `None`.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure; [`ChiefdError::Corrupt`] on an unknown
/// status.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<Option<RuntimeOwner>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT status, socket, claimed_at, validated_at, released_at \
             FROM runtime_owner WHERE slug = ?1",
        )
        .map_err(store_failure)?;
    let mut rows = stmt.query(params![row_slug]).map_err(store_failure)?;
    let Some(row) = rows.next().map_err(store_failure)? else {
        return Ok(None);
    };
    let status: String = row.get(0).map_err(store_failure)?;
    Ok(Some(RuntimeOwner {
        version: 1,
        organization: company_slug.to_string(),
        status: RuntimeOwnerStatus::from_text(&status)?,
        socket_name: row.get(1).map_err(store_failure)?,
        claimed_at: row.get(2).map_err(store_failure)?,
        validated_at: row.get(3).map_err(store_failure)?,
        released_at: row.get(4).map_err(store_failure)?,
        extra: BTreeMap::new(),
    }))
}

/// Publish the singleton as a direct atomic current-state write. One
/// `org_events` touch is appended iff any stored field changed.
///
/// # Errors
/// [`UNMODELED_KEYS`] refusal (422); SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    incoming: &RuntimeOwner,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    let current = reconstruct(tx, row_slug, company_slug)?;
    // The feed stamp is the most recent lifecycle instant present.
    let at = incoming
        .released_at
        .clone()
        .or_else(|| incoming.validated_at.clone())
        .or_else(|| incoming.claimed_at.clone())
        .unwrap_or_default();

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        let unchanged = current.as_ref().map(|c| fields_equal(c, incoming)).unwrap_or(false);
        if unchanged {
            return Ok(Vec::new());
        }
        tx.execute(
            "INSERT INTO runtime_owner(slug, status, socket, claimed_at, validated_at, \
             released_at) VALUES(?1,?2,?3,?4,?5,?6) \
             ON CONFLICT(slug) DO UPDATE SET status=?2, socket=?3, claimed_at=?4, \
             validated_at=?5, released_at=?6",
            params![
                row_slug,
                incoming.status.as_text(),
                incoming.socket_name,
                incoming.claimed_at,
                incoming.validated_at,
                incoming.released_at,
            ],
        )?;
        Ok(vec![EventTouch::new(
            "runtime-owner",
            company_slug,
            "upsert",
            "runtime_owner",
            row_slug,
        )])
    })
    .map_err(|RowsSqlError(e)| e)
}

fn fields_equal(a: &RuntimeOwner, b: &RuntimeOwner) -> bool {
    a.status == b.status
        && a.socket_name == b.socket_name
        && a.claimed_at == b.claimed_at
        && a.validated_at == b.validated_at
        && a.released_at == b.released_at
}

/// Keys a historical blob carries that this model deliberately no longer
/// stores. Dropped on publish instead of refused; see the module docs.
const RETIRED_KEYS: [&str; 1] = ["sessionName"];

fn reject_unmodeled_keys(doc: &RuntimeOwner) -> Result<(), ChiefdError> {
    let mut paths: Vec<String> = doc
        .extra
        .keys()
        .filter(|key| !RETIRED_KEYS.contains(&key.as_str()))
        .map(|k| format!("extra.{k}"))
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!(
            "runtime-owner carries unmodeled keys the row model cannot store: {}",
            paths.join(", ")
        ),
    )))
}

/// Backfill the blob into the row via the live publish path.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes; the publish's
/// [`UNMODELED_KEYS`] refusal passes through.
pub fn backfill_runtime_owner(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let doc: RuntimeOwner =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("runtime-owner-blob", e))?;
    publish(tx, row_slug, company_slug, &doc)
}

/// The `runtime-owner` zero-loss verifier. Signature mirrors
/// `migration::shadow_diff_manifest`.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes, [`ChiefdError::StoreFailure`] on a SQL
/// failure; an unmodeled
/// key is recorded loud, not an error.
pub fn shadow_diff_runtime_owner(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<ShadowReport, ChiefdError> {
    let mut report = ShadowReport::new(RUNTIME_OWNER_STORE);
    let original: RuntimeOwner =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("runtime-owner-blob", e))?;
    match backfill_runtime_owner(tx, row_slug, company_slug, blob) {
        Ok(_) => {}
        Err(e) if e.code() == Some(UNMODELED_KEYS) => {
            report.record_loud(format!("UNMODELED KEYS rejected by publish: {e}"));
            return Ok(report);
        }
        Err(e) => return Err(e),
    }
    let r = reconstruct(tx, row_slug, company_slug)?.ok_or_else(|| {
        crate::error::store_failure_because(
            "runtime-owner-rows",
            "the runtime-owner rows are missing immediately after their own publish",
        )
    })?;
    report.row_count = 1;
    report.record("version", Disposition::Derived { proof: "constant 1".into() });
    report.record("organization", Disposition::Derived { proof: "process company slug".into() });
    let m = |eq: bool, v: String| {
        if eq {
            Disposition::Matched
        } else {
            Disposition::Lost { blob_value: v }
        }
    };
    report.record(
        "sessionName",
        Disposition::ExpectedDropped {
            where_now: "retired (AC6): was `org-<slug>` for this row's own slug".into(),
        },
    );
    report.record("status", m(r.status == original.status, original.status.as_text().to_string()));
    report.record("socketName", opt_disposition(&original.socket_name, &r.socket_name));
    report.record("claimedAt", opt_disposition(&original.claimed_at, &r.claimed_at));
    report.record("validatedAt", opt_disposition(&original.validated_at, &r.validated_at));
    report.record("releasedAt", opt_disposition(&original.released_at, &r.released_at));
    Ok(report)
}

fn opt_disposition(original: &Option<String>, recon: &Option<String>) -> Disposition {
    match original {
        None => Disposition::ExpectedDropped { where_now: "absent (NULL column)".into() },
        Some(v) if recon.as_deref() == Some(v.as_str()) => Disposition::Matched,
        Some(v) => Disposition::Lost { blob_value: v.clone() },
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
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("schema");
        conn
    }

    fn live() -> RuntimeOwner {
        RuntimeOwner {
            version: 1,
            organization: "acme".into(),
            status: RuntimeOwnerStatus::Active,
            socket_name: Some("default".into()),
            claimed_at: Some("2026-07-25T06:46:10.832Z".into()),
            validated_at: Some("2026-07-25T18:09:51.203Z".into()),
            released_at: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn round_trips_the_live_active_shape() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &live()).unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().unwrap(), live());
    }

    #[test]
    fn released_status_round_trips() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut d = live();
        d.status = RuntimeOwnerStatus::Released;
        d.released_at = Some("2026-07-25T19:00:00.000Z".into());
        publish(&tx, "acme", "acme", &d).unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().unwrap(), d);
    }

    #[test]
    fn unchanged_publish_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &live()).unwrap();
        let out = publish(&tx, "acme", "acme", &live()).unwrap();
        assert_eq!(out, 1);
        assert_eq!(event_count(&tx), 1);
    }

    #[test]
    fn rejects_unmodeled_keys() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut d = live();
        d.extra.insert("z".into(), serde_json::json!(1));
        assert_eq!(publish(&tx, "acme", "acme", &d).unwrap_err().code(), Some(UNMODELED_KEYS));
    }

    #[test]
    fn a_legacy_blob_still_carrying_the_retired_session_key_backfills_instead_of_refusing() {
        // The READ-PATH check for the deleted field. `sessionName` is present
        // in the captured live blob below, so a model that merely dropped the
        // field would have turned every historical `org_documents` row into an
        // `unmodeled-keys` refusal on upgrade — the store would have become
        // unbackfillable, silently, for every company. It is retired, not
        // unknown: the publish drops it and the reconstruct is unaffected.
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut d = live();
        d.extra.insert("sessionName".into(), serde_json::json!("org-acme"));
        publish(&tx, "acme", "acme", &d).expect("a retired key is dropped, never refused");
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().unwrap(), live());

        // ...and the tolerance is exactly one key wide.
        let mut other = live();
        other.extra.insert("socketPath".into(), serde_json::json!("/tmp/x"));
        assert_eq!(
            publish(&tx, "acme", "acme", &other).unwrap_err().code(),
            Some(UNMODELED_KEYS),
            "only the named retired key is tolerated"
        );
    }

    #[test]
    fn shadow_diff_is_zero_loss_on_live_blob() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let blob = br#"{"version":1,"organization":"acme","sessionName":"org-acme","status":"active","socketName":"default","claimedAt":"2026-07-25T06:46:10.832Z","validatedAt":"2026-07-25T18:09:51.203Z"}"#;
        let report = shadow_diff_runtime_owner(&tx, "acme", "acme", blob).unwrap();
        assert!(report.zero_loss(), "loud: {:?}", report.loud_failures());
        assert_eq!(report.row_count, 1);
    }
}
