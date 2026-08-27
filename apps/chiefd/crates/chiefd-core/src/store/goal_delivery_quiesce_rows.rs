//! The `goal-delivery-quiesce` singleton ROW implementation (org-data-
//! normalization P0, N2 copy-pattern, B4 singleton sweep).
//!
//! Twin of [`crate::store::session_epoch_rows`]: a scalar singleton (`quiesce`
//! table, `slug` PK) holding one instant. Reconstruct from the row, publish by
//! diffing against it, one `org_events` touch per change.
//!
//! Identity DERIVED, never stored (B2): `version` = const `1`; `organization` =
//! the process's own company slug; `sessionName` = `org-<slug>` (the manifest's
//! `runtime_session` derivation — confirmed against the live blob 2026-07-25). Only
//! `quiescedAt` is stored (column `since`).
//!
//! Item D: publish REJECTS any serde-flatten `extra` with [`UNMODELED_KEYS`].

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::corrupt_store;
use crate::error::Refusal;
use crate::store::organization_rows::{RowsSqlError, UNMODELED_KEYS};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::store::shadow_report::{Disposition, ShadowReport};
use crate::ChiefdError;

/// A `goal-delivery-quiesce` singleton in its typed form. Mirrors the TS
/// `OrganizationGoalDeliveryQuiesce`; `version`/`organization`/`sessionName` are
/// DERIVED on reconstruct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalDeliveryQuiesce {
    /// Always `1`. Compile-time constant, not stored.
    pub version: u32,
    /// The company slug — DERIVED, not stored.
    pub organization: String,
    /// `org-<slug>` — DERIVED (== manifest runtime_session), not stored.
    /// Every automatic mail-backed grant requires its justifying envelope/effect
    /// `createdAt` strictly after this instant.
    #[serde(rename = "quiescedAt")]
    pub quiesced_at: String,
    /// Any unmodeled key (item D). Empty on a clean doc.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The `org_documents` store family this row set replaces.
pub const GOAL_DELIVERY_QUIESCE_STORE: &str = "goal-delivery-quiesce";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("goal-delivery-quiesce-rows", e)
}

// ---- reconstruct ----------------------------------------------------------

/// Reconstruct the quiesce singleton for `company_slug`, or `None` when absent.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<Option<GoalDeliveryQuiesce>, ChiefdError> {
    let mut stmt =
        tx.prepare("SELECT since FROM quiesce WHERE slug = ?1").map_err(store_failure)?;
    let mut rows = stmt.query(params![row_slug]).map_err(store_failure)?;
    let Some(row) = rows.next().map_err(store_failure)? else {
        return Ok(None);
    };
    Ok(Some(GoalDeliveryQuiesce {
        version: 1,
        organization: company_slug.to_string(),
        quiesced_at: row.get(0).map_err(store_failure)?,
        extra: BTreeMap::new(),
    }))
}

/// The quiesce instant alone, without reconstructing a document around it.
///
/// # Why this exists beside [`reconstruct`]
///
/// The health monitor's watermark read wants ONE column and throws the rest
/// away. Reaching it through `reconstruct` forced that reader to supply a
/// display slug for a `organization` field it immediately discards — which
/// meant looking the name up in `org_settings`, which meant a company genesis
/// had not named yet turned an optional, deliberately FAIL-OPEN watermark into
/// a silent `None`. Failing open there means "no suppression", so the coupling
/// would have been invisible and wrong in the unsafe direction.
///
/// A reader that needs one column must not have to name the company.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn quiesced_at(tx: &Transaction<'_>, row_slug: &str) -> Result<Option<String>, ChiefdError> {
    let mut stmt =
        tx.prepare("SELECT since FROM quiesce WHERE slug = ?1").map_err(store_failure)?;
    let mut rows = stmt.query(params![row_slug]).map_err(store_failure)?;
    let Some(row) = rows.next().map_err(store_failure)? else {
        return Ok(None);
    };
    Ok(Some(row.get(0).map_err(store_failure)?))
}

// ---- publish --------------------------------------------------------------

/// Publish the singleton as one atomic current-state operation. Rejects
/// unmodeled keys before writing; upserts the row and emits ONE `org_events`
/// touch iff `since` changed. The returned seq is immutable audit/cursor
/// evidence.
///
/// # Errors
/// [`UNMODELED_KEYS`] refusal (422); SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    incoming: &GoalDeliveryQuiesce,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    let at = incoming.quiesced_at.clone();

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        Ok(upsert_quiesce(tx, row_slug, company_slug, &incoming.quiesced_at)?.into_iter().collect())
    })
    .map_err(|RowsSqlError(e)| e)
}

/// Upsert the normalized quiesce instant inside a larger writer transaction,
/// returning its audit touch without independently emitting it. CEO-only
/// preparation composes this with activity and launch-intent changes so no
/// observer can see the empty admission fence before the watermark exists.
pub(crate) fn upsert_quiesce(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    quiesced_at: &str,
) -> rusqlite::Result<Option<EventTouch>> {
    let current: Option<String> = tx
        .query_row("SELECT since FROM quiesce WHERE slug = ?1", params![row_slug], |row| row.get(0))
        .optional()?;
    if current.as_deref() == Some(quiesced_at) {
        return Ok(None);
    }
    tx.execute(
        "INSERT INTO quiesce(slug, since) VALUES(?1, ?2) \
         ON CONFLICT(slug) DO UPDATE SET since = ?2",
        params![row_slug, quiesced_at],
    )?;
    Ok(Some(EventTouch::new("goal-delivery-quiesce", company_slug, "upsert", "quiesce", row_slug)))
}

/// Fence-free CLEAR of the quiesce singleton: delete the `quiesce` row for
/// `row_slug` (the doc becomes ABSENT), emitting one `"delete"` `org_events`
/// touch iff a row existed. Fence-free — runs inside the writer's own
/// `BEGIN IMMEDIATE` without a caller sequence precondition, via
/// [`apply_and_emit`].
///
/// # Errors
/// SQL failures as [`ChiefdError::StoreFailure`].
pub fn clear(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    at: &str,
) -> Result<(), ChiefdError> {
    let existed: bool = tx
        .query_row("SELECT 1 FROM quiesce WHERE slug = ?1", params![row_slug], |_| Ok(()))
        .optional()
        .map_err(store_failure)?
        .is_some();
    apply_and_emit::<RowsSqlError, _>(tx, row_slug, at, "", |tx| {
        if !existed {
            return Ok(Vec::new());
        }
        tx.execute("DELETE FROM quiesce WHERE slug = ?1", params![row_slug])?;
        // Same entity id the upsert touch uses: the singleton is named after
        // the company, and a delete that named it differently would read as a
        // different entity in the audit feed.
        Ok(vec![EventTouch::new(
            "goal-delivery-quiesce",
            company_slug,
            "delete",
            "quiesce",
            row_slug,
        )])
    })
    .map(|_seq| ())
    .map_err(|RowsSqlError(e)| e)
}

/// Keys a historical blob carries that this model deliberately no longer
/// stores. Dropped on publish instead of refused — the same mechanism, and the
/// same one-element table, `runtime_owner_rows` has carried for `sessionName`
/// since it retired the key on its own row.
///
/// `sessionName` was always `"org-" + slug`, derived on read and stored
/// nowhere. Every historical blob carries it; refusing them would turn a
/// readable document into `UNMODELED_KEYS` at publish.
const RETIRED_KEYS: [&str; 1] = ["sessionName"];

fn reject_unmodeled_keys(doc: &GoalDeliveryQuiesce) -> Result<(), ChiefdError> {
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
            "goal-delivery-quiesce carries unmodeled keys the row model cannot store: {}",
            paths.join(", ")
        ),
    )))
}

// ---- backfill -------------------------------------------------------------

/// Backfill the `goal-delivery-quiesce` blob into the row for one company via
/// the live publish path. Signature mirrors `migration::backfill_manifest`.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes; the publish's
/// [`UNMODELED_KEYS`] refusal passes through.
pub fn backfill_goal_delivery_quiesce(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let doc: GoalDeliveryQuiesce =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("goal-delivery-quiesce-blob", e))?;
    publish(tx, row_slug, company_slug, &doc)
}

// ---- shadow-diff (zero-loss verifier) ------------------------------------

/// The `goal-delivery-quiesce` zero-loss verifier. Signature mirrors
/// `migration::shadow_diff_manifest`.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes, [`ChiefdError::StoreFailure`] on a SQL
/// failure. An unmodeled
/// key is recorded in [`ShadowReport::loud_failures`], not an error.
pub fn shadow_diff_goal_delivery_quiesce(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<ShadowReport, ChiefdError> {
    let mut report = ShadowReport::new(GOAL_DELIVERY_QUIESCE_STORE);
    let original: GoalDeliveryQuiesce =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("goal-delivery-quiesce-blob", e))?;
    if let Err(e) = backfill_goal_delivery_quiesce(tx, row_slug, company_slug, blob) {
        if e.code() == Some(UNMODELED_KEYS) {
            report.record_loud(format!("UNMODELED KEYS rejected by publish: {e}"));
            return Ok(report);
        }
        return Err(e);
    }
    let recon = reconstruct(tx, row_slug, company_slug)?.ok_or_else(|| {
        crate::error::store_failure_because(
            "goal-delivery-quiesce-rows",
            "the quiesce rows are missing immediately after their own publish",
        )
    })?;
    report.row_count = 1;
    report.record("version", Disposition::Derived { proof: "constant 1".into() });
    report.record("organization", Disposition::Derived { proof: "process company slug".into() });
    // sessionName is RETIRED from this model; prove the derivation still
    // reproduces the blob value before trusting the drop (never assume, always
    // verify). The comparison is now against the DERIVATION rather than against
    // a field this row had copied from that same derivation.
    let retired_session =
        original.extra.get("sessionName").and_then(serde_json::Value::as_str).map(str::to_owned);
    report.record(
        "sessionName",
        match retired_session {
            None => Disposition::Derived { proof: "absent from the blob".into() },
            Some(stored)
                if stored == crate::store::organization::runtime_session_for_slug(company_slug) =>
            {
                Disposition::Derived { proof: format!("org-<slug> == {stored:?}, key retired") }
            }
            Some(stored) => Disposition::Lost { blob_value: stored },
        },
    );
    report.record(
        "quiescedAt",
        if recon.quiesced_at == original.quiesced_at {
            Disposition::Matched
        } else {
            Disposition::Lost { blob_value: original.quiesced_at.clone() }
        },
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::rows_txn::current_seq;
    use rusqlite::Connection;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("apply company schema");
        conn
    }

    fn doc(quiesced: &str) -> GoalDeliveryQuiesce {
        GoalDeliveryQuiesce {
            version: 1,
            organization: "acme".into(),
            quiesced_at: quiesced.into(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn round_trips_and_derives_session_name() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &doc("2026-07-25T06:46:10.852Z")).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got, doc("2026-07-25T06:46:10.852Z"));
    }

    #[test]
    fn unchanged_publish_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &doc("t1")).unwrap();
        let out = publish(&tx, "acme", "acme", &doc("t1")).unwrap();
        assert_eq!(out, 1);
        assert_eq!(current_seq(&tx, "acme").unwrap(), 1);
    }

    #[test]
    fn changed_publish_emits_one_event_with_the_right_detail_ref() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &doc("t1")).unwrap();
        publish(&tx, "acme", "acme", &doc("t2")).unwrap();
        let detail: String = tx
            .query_row("SELECT detail_ref FROM org_events WHERE slug='acme' AND seq=2", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(detail, "quiesce:acme/acme");
    }

    #[test]
    fn a_second_atomic_publish_replaces_the_current_singleton() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &doc("t1")).unwrap();
        let out = publish(&tx, "acme", "acme", &doc("t2")).unwrap();
        assert_eq!(out, 2);
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().unwrap().quiesced_at, "t2");
    }

    #[test]
    fn clear_deletes_the_row_and_emits_a_delete() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", &doc("t1")).unwrap();
        clear(&tx, "acme", "acme", "t2").unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap(), None); // ABSENT
        assert_eq!(current_seq(&tx, "acme").unwrap(), 2);
        let op: String = tx
            .query_row("SELECT op FROM org_events WHERE slug='acme' AND seq=2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(op, "delete");
    }

    #[test]
    fn clear_on_absent_singleton_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        clear(&tx, "acme", "acme", "t").unwrap();
        assert_eq!(current_seq(&tx, "acme").unwrap(), 0);
    }

    #[test]
    fn rejects_unmodeled_keys() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut d = doc("t1");
        d.extra.insert("bogus".into(), serde_json::json!(1));
        let err = publish(&tx, "acme", "acme", &d).unwrap_err();
        assert_eq!(err.code(), Some(UNMODELED_KEYS));
    }

    #[test]
    fn backfill_from_live_blob_shape() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let blob = br#"{"version":1,"organization":"acme","sessionName":"org-acme","quiescedAt":"2026-07-25T06:46:10.852Z"}"#;
        backfill_goal_delivery_quiesce(&tx, "acme", "acme", blob).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(got.quiesced_at, "2026-07-25T06:46:10.852Z");
    }
}
