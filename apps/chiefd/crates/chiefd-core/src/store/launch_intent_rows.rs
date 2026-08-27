//! The `launch-intent` ROW implementation (org-data-normalization P0, N2
//! copy-pattern, B4 singleton sweep).
//!
//! A per-company set of authorized non-CEO person ids → child rows in the
//! `launch_intent` table (one row per person; presence == intent). Reconstruct
//! rebuilds the [`LaunchIntent`] doc from those rows; publish diffs the incoming
//! id set against them (delete-absent + insert-new), one `org_events` touch per
//! changed person. DERIVED, never stored: `version` = const `1`; `organization`
//! = company slug; `sessionName` = `org-<slug>`; `updatedAt` = MAX(org_events.at).
//!
//! chiefd is a launch-intent READER only (settle-ux moved its live fix to the
//! native activity ledger, 2026-07-25) — no concurrent legacy-blob writer.
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

/// One `launch_intent` row as read back from SQL: the fenced person id plus
/// its start attribution (initiator, reason, started-at).
type LaunchIntentRow = (String, Option<String>, Option<String>, Option<String>);

/// A `launch-intent` doc. Mirrors the TS `OrganizationLaunchIntent`; everything
/// but `personIds` is DERIVED on reconstruct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchIntent {
    /// Always `1`. Not stored.
    pub version: u32,
    /// The company slug — DERIVED, not stored.
    pub organization: String,
    /// Explicit non-CEO nodes authorized to run — the child rows.
    pub person_ids: Vec<String>,
    /// DERIVED = MAX(org_events.at); not stored.
    pub updated_at: String,
    /// Per-person durable answer to “why is this person up?”.
    #[serde(default)]
    pub attributions: BTreeMap<String, StartAttribution>,
    /// Any unmodeled key (item D).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Durable attribution attached to one explicit person-start decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartAttribution {
    /// Attested launcher person that requested the start; absent is operator anomaly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initiator_person_id: Option<String>,
    /// Human-readable triggering reason supplied to `org start-person`.
    pub reason: String,
    /// Wall-clock instant at which the start intent was recorded.
    pub started_at: String,
}

/// The `org_documents` store family this row set replaces.
pub const LAUNCH_INTENT_STORE: &str = "launch-intent";

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("launch-intent-rows", e)
}

fn derived_updated_at(tx: &Transaction<'_>, row_slug: &str) -> Result<String, ChiefdError> {
    let at: Option<String> = tx
        .query_row("SELECT MAX(at) FROM org_events WHERE slug = ?1", params![row_slug], |r| {
            r.get(0)
        })
        .map_err(store_failure)?;
    Ok(at.unwrap_or_default())
}

/// Reconstruct the launch-intent doc for `company_slug`. ALWAYS total: a company
/// with no `launch_intent` rows reconstructs to an empty `person_ids` set — an
/// authorized-nobody fence is a real state, not a "fence-shaped hole". Absence is
/// already represented by the empty set + no rows; there is no `Option` (a `None`
/// return would be a second, redundant representation of the same absence, the
/// dual-representation defect the fence-containment fix removes).
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<LaunchIntent, ChiefdError> {
    let mut stmt = tx
        .prepare("SELECT person_id, initiator_person_id, reason, started_at FROM launch_intent WHERE slug = ?1 ORDER BY person_id")
        .map_err(store_failure)?;
    let rows: Vec<LaunchIntentRow> = stmt
        .query_map(params![row_slug], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(store_failure)?
        .collect::<Result<_, _>>()
        .map_err(store_failure)?;
    let ids = rows.iter().map(|row| row.0.clone()).collect();
    let attributions = rows
        .into_iter()
        .filter_map(|(id, initiator_person_id, reason, started_at)| {
            Some((
                id,
                StartAttribution { initiator_person_id, reason: reason?, started_at: started_at? },
            ))
        })
        .collect();
    Ok(LaunchIntent {
        version: 1,
        organization: company_slug.to_string(),
        person_ids: ids,
        updated_at: derived_updated_at(tx, row_slug)?,
        attributions,
        extra: BTreeMap::new(),
    })
}

/// Drop ONE person's fence row inside the caller's own `BEGIN IMMEDIATE`,
/// returning the [`EventTouch`] to fold into that transaction's `org_events`
/// batch when a row was actually removed, or `None` when the person held no fence
/// (idempotent — dropping an absent fence is a no-op, not an error).
///
/// This is the typed door atomic org-ops compose (e.g. `shutdown_person`): the
/// op-family contract is that ONLY this module names the `launch_intent`
/// key/table in SQL — callers never write raw fence SQL, they call this accessor
/// and append the returned touch to their own [`apply_and_emit`] batch. The
/// `Option` here is a genuine did-something / nothing signal for exactly one
/// row, NOT a whole-fence presence flag.
///
/// # `reason` is required, and it is not decoration
///
/// A withdrawn launch intent is a person the operator asked for and is not
/// getting. Every withdrawal in `converge_apply::cycle` names itself
/// (`launch intent withdrawn (settled)` / `(not-operational)` / `(no-demand)`),
/// and the ones that did not were invisible: on `taperoom-inc`, 2026-08-20, 310
/// of 597 fence deletes had no line anywhere in the log naming the person or the
/// verb. An operator watching a rail row that never comes up has no way to ask
/// the next question.
///
/// So the verb is a PARAMETER rather than a convention, which makes it a compile
/// error to drop a fence without saying which decision dropped it. It is
/// `&'static str` deliberately: it names the CALLER, not a runtime value, so it
/// stays greppable and cannot become a formatted sentence that drifts.
///
/// # Errors
/// Propagates any `rusqlite` failure from the delete (store corruption at the
/// caller's mapping point).
pub fn delete_person_fence(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    reason: &'static str,
) -> rusqlite::Result<Option<EventTouch>> {
    let deleted = tx.execute(
        "DELETE FROM launch_intent WHERE slug = ?1 AND person_id = ?2",
        params![slug, person_id],
    )?;
    if deleted == 0 {
        return Ok(None);
    }
    tracing::info!(
        event = "launch-intent.withdrawn",
        person = person_id,
        reason,
        company = slug,
        "a launch intent was withdrawn; the person the operator asked for is no longer \
         authorized to run, and this is the decision that de-authorized them"
    );
    Ok(Some(EventTouch::new("launch-intent", person_id, "delete", "launch_intent", slug)))
}

/// Add exactly one launch fence inside an enclosing semantic operation.
/// A duplicate is deliberately a no-op: repeated pending-mail scans must not
/// manufacture audit churn while the original named work remains pending.
///
/// This said "non-CEO" while the root ran on an unconditional exemption in
/// `activity::reconcile`. `prepare_ceo_only` names the CEO here now, to give the
/// root a real start decision rather than an exemption. Nothing about the row is
/// special-cased for them.
pub fn insert_person_fence(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
) -> rusqlite::Result<Option<EventTouch>> {
    let inserted = tx.execute(
        "INSERT INTO launch_intent(slug, person_id) VALUES(?1, ?2) \
         ON CONFLICT(slug, person_id) DO NOTHING",
        params![slug, person_id],
    )?;
    Ok((inserted > 0)
        .then(|| EventTouch::new("launch-intent", person_id, "upsert", "launch_intent", slug)))
}

/// Publish the id set as a direct atomic current-state write. Deletes rows no
/// longer named and inserts newly-named ones; one `org_events` touch per changed
/// person.
///
/// # Errors
/// [`UNMODELED_KEYS`] refusal (422); SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming: &LaunchIntent,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    let at = incoming.updated_at.clone();
    // Current set (may be empty).
    let current: std::collections::BTreeSet<String> = {
        let mut stmt = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug = ?1")
            .map_err(store_failure)?;
        let set = stmt
            .query_map(params![row_slug], |r| r.get::<_, String>(0))
            .map_err(store_failure)?
            .collect::<Result<_, _>>()
            .map_err(store_failure)?;
        set
    };
    let incoming_set: std::collections::BTreeSet<String> =
        incoming.person_ids.iter().cloned().collect();

    // THE OPERATOR'S WAKE SURVIVES A WHOLE-DOCUMENT REPUBLISH.
    //
    // This function's set difference is the sharpest edge in the fence: it
    // deletes every committed row the incoming document omits, and a document
    // is a WHOLE-FENCE value that any caller can compute from a stale read. On
    // `taperoom-inc`, 2026-08-20, that is exactly what took `research-promoter`
    // down 2.165 seconds after the operator woke her — one delete, empty actor,
    // no note, inside a batch of 37 unrelated `person-activity` upserts.
    //
    // `launch_intent::add` no longer computes its union from a stale read
    // (that is the cause, and it is fixed at the cause). This is the DURABLE
    // gate underneath it: whichever caller reaches this row, and whatever it
    // believed the fence was when it started, a person inside the quiet lease
    // an operator's wake bought them is not withdrawn here. It also closes the
    // window between a caller's fresh read and its commit, which no
    // read-then-write fix can close on its own.
    //
    // Not a refusal: a narrowing must never fail, and telling a publisher its
    // whole document was rejected because of one row would be worse than the
    // defect. The row is RETAINED and the retention is named.
    let leased = crate::store::activity::rows::operator_wake_leased_people(
        tx,
        row_slug,
        crate::isotime::parse_iso_millis(&at).unwrap_or(i64::MIN),
    )
    .map_err(store_failure)?;
    apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        let mut touches = Vec::new();
        for id in current.difference(&incoming_set) {
            if leased.contains(id) {
                tracing::info!(
                    event = "launch-intent.wake-lease-held",
                    person = %id,
                    company = row_slug,
                    "a whole-document republish omitted this person, but an operator woke them \
                     inside the quiet lease, so their launch intent is retained; a wake buys the \
                     full settle window whether or not anything was sent to them"
                );
                continue;
            }
            tx.execute(
                "DELETE FROM launch_intent WHERE slug = ?1 AND person_id = ?2",
                params![row_slug, id],
            )?;
            // NAMED, like every other withdrawal. This one was the silent
            // majority: 310 of 597 fence deletes on `taperoom-inc` that day had
            // no line anywhere in the log, and every one of them came through
            // here.
            //
            // INFO, NOT WARN, and the distinction is the one `cycle.rs` already
            // draws about a stand-down: a line that fires when the product is
            // working correctly must not be logged as a fault, or an operator's
            // own healthy company becomes a fault log nobody reads. `remove` —
            // the converge shrink half, which IS a per-person decision — commits
            // through this same document path, so most of what passes here is
            // exactly that, and the pass that decided it prints its own reason
            // (`launch intent withdrawn (settled)`) beside this line. Measured on
            // the QA box: Jordan's withdrawal carried both, 16ms apart.
            //
            // What this line owes the operator is not alarm; it is that the
            // answer to "why is this person gone" exists at all and is greppable
            // by person. That is the whole of the defect it closes.
            tracing::info!(
                event = "launch-intent.withdrawn",
                person = %id,
                reason = "document-republish",
                company = row_slug,
                "a whole-document launch-intent publish withdrew this person's fence: the \
                 incoming document did not name them and the committed rows did. The pass that \
                 decided it prints its own reason beside this line; a delete with no such \
                 reason anywhere is a withdrawal nobody owns"
            );
            touches.push(EventTouch::new("launch-intent", id, "delete", "launch_intent", row_slug));
        }
        for id in &incoming_set {
            tx.execute(
                "INSERT INTO launch_intent(slug, person_id, initiator_person_id, reason, started_at) VALUES(?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(slug, person_id) DO UPDATE SET initiator_person_id=excluded.initiator_person_id, reason=excluded.reason, started_at=excluded.started_at",
                params![row_slug, id, incoming.attributions.get(id).and_then(|a| a.initiator_person_id.clone()), incoming.attributions.get(id).map(|a| a.reason.clone()), incoming.attributions.get(id).map(|a| a.started_at.clone())],
            )?;
            if !current.contains(id) { touches.push(EventTouch::new("launch-intent", id, "upsert", "launch_intent", row_slug)); }
        }
        Ok(touches)
    })
    .map_err(|RowsSqlError(e)| e)
}

/// Fence-free CLEAR of the launch-intent doc: delete EVERY `launch_intent` row
/// for `row_slug`, emitting one `"delete"` `org_events` touch per removed person.
/// This is a REAL delete (the doc becomes ABSENT, not an empty set), which is
/// what the launch-intent absent-vs-empty fence semantics require — distinct
/// from publishing an empty id list. It runs inside the writer's own
/// `BEGIN IMMEDIATE`, via [`apply_and_emit`].
///
/// # Errors
/// SQL failures as [`ChiefdError::StoreFailure`].
pub fn clear(tx: &Transaction<'_>, row_slug: &str, at: &str) -> Result<(), ChiefdError> {
    let ids: Vec<String> = {
        let mut stmt = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug = ?1 ORDER BY person_id")
            .map_err(store_failure)?;
        let collected = stmt
            .query_map(params![row_slug], |r| r.get::<_, String>(0))
            .map_err(store_failure)?
            .collect::<Result<Vec<String>, _>>()
            .map_err(store_failure)?;
        collected
    };
    apply_and_emit::<RowsSqlError, _>(tx, row_slug, at, "", |tx| {
        let mut touches = Vec::new();
        for id in &ids {
            tx.execute(
                "DELETE FROM launch_intent WHERE slug = ?1 AND person_id = ?2",
                params![row_slug, id],
            )?;
            // NAMED, like every other withdrawal. This one is the operator's own
            // "start from CEO-only" door — `company.boot` and CEO-only recovery —
            // and it is deliberately NOT gated by the wake lease: a boot reset is
            // a newer decision than any wake it supersedes. It still says who it
            // de-authorized, because a reset that clears somebody the operator
            // woke ten seconds ago is exactly the event they will come looking
            // for.
            tracing::info!(
                event = "launch-intent.withdrawn",
                person = %id,
                reason = "fence-cleared",
                company = row_slug,
                "the whole launch-intent fence was cleared, so this person is no longer \
                 authorized to run; the company is back to CEO-only"
            );
            touches.push(EventTouch::new(
                "launch-intent",
                id.clone(),
                "delete",
                "launch_intent",
                row_slug,
            ));
        }
        Ok(touches)
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

fn reject_unmodeled_keys(doc: &LaunchIntent) -> Result<(), ChiefdError> {
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
            "launch-intent carries unmodeled keys the row model cannot store: {}",
            paths.join(", ")
        ),
    )))
}

/// Backfill the blob into the rows via the live publish path.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes; the publish's
/// [`UNMODELED_KEYS`] refusal passes through.
pub fn backfill_launch_intent(
    tx: &Transaction<'_>,
    row_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let doc: LaunchIntent =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("launch-intent-blob", e))?;
    publish(tx, row_slug, &doc)
}

/// The `launch-intent` zero-loss verifier. Signature mirrors
/// `migration::shadow_diff_manifest`.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes, [`ChiefdError::StoreFailure`] on a SQL
/// failure; an unmodeled
/// key is recorded loud, not an error.
pub fn shadow_diff_launch_intent(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<ShadowReport, ChiefdError> {
    let mut report = ShadowReport::new(LAUNCH_INTENT_STORE);
    let original: LaunchIntent =
        serde_json::from_slice(blob).map_err(|e| corrupt_store("launch-intent-blob", e))?;
    match backfill_launch_intent(tx, row_slug, blob) {
        Ok(_) => {}
        Err(e) if e.code() == Some(UNMODELED_KEYS) => {
            report.record_loud(format!("UNMODELED KEYS rejected by publish: {e}"));
            return Ok(report);
        }
        Err(e) => return Err(e),
    }
    let recon = reconstruct(tx, row_slug, company_slug)?;
    report.row_count = recon.person_ids.len();
    report.record("version", Disposition::Derived { proof: "constant 1".into() });
    report.record("organization", Disposition::Derived { proof: "process company slug".into() });
    // `sessionName` is RETIRED from this model, so there is no `recon` side
    // left to compare. The zero-loss question is still answered, and answered
    // more honestly than before: the blob's value is checked against the
    // derivation it always was, rather than against a field this row copied
    // from that same derivation and then compared with itself.
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
    report.record("updatedAt", Disposition::Derived { proof: "MAX(org_events.at)".into() });
    // personIds: every blob id must be present as a row (set equality; order is a
    // deterministic non-positional dimension → ExpectedDropped).
    let recon_set: std::collections::BTreeSet<&String> = recon.person_ids.iter().collect();
    for id in &original.person_ids {
        report.record(
            format!("personIds.{id}"),
            if recon_set.contains(id) {
                Disposition::Matched
            } else {
                Disposition::Lost { blob_value: id.clone() }
            },
        );
    }
    report.record(
        "personIds.order",
        Disposition::ExpectedDropped {
            where_now: "set membership; order has no positional consumer".into(),
        },
    );
    Ok(report)
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

    fn doc(ids: &[&str]) -> LaunchIntent {
        LaunchIntent {
            version: 1,
            organization: "acme".into(),
            person_ids: ids.iter().map(|s| s.to_string()).collect(),
            updated_at: "2026-07-25T00:00:00.000Z".into(),
            attributions: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn empty_before_any_publish_is_an_empty_set_not_none() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        // De-Option'd: no rows reconstructs to an empty set (a real
        // authorized-nobody fence), never a "fence-shaped hole".
        assert!(reconstruct(&tx, "acme", "acme").unwrap().person_ids.is_empty());
    }

    #[test]
    fn publish_creates_one_row_and_event_per_person() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &doc(&["head", "worker"])).unwrap();
        let got = reconstruct(&tx, "acme", "acme").unwrap();
        assert_eq!(got.person_ids, vec!["head".to_string(), "worker".to_string()]);
        assert_eq!(event_count(&tx), 2);
    }

    #[test]
    fn publish_reconstructs_per_person_start_attribution() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut incoming = doc(&["worker"]);
        incoming.attributions.insert(
            "worker".into(),
            StartAttribution {
                initiator_person_id: Some("engineering-head".into()),
                reason: "Investigate the build failure.".into(),
                started_at: "2026-07-27T12:00:00.000Z".into(),
            },
        );
        publish(&tx, "acme", &incoming).unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().attributions, incoming.attributions);
    }

    #[test]
    fn operator_start_attribution_omits_absent_initiator_on_the_typescript_wire() {
        let value = serde_json::to_value(StartAttribution {
            initiator_person_id: None,
            reason: "Resume exactly this manager.".into(),
            started_at: "2026-07-27T12:00:00.000Z".into(),
        })
        .unwrap();
        assert!(value.get("initiatorPersonId").is_none());
        assert_eq!(value["reason"], "Resume exactly this manager.");
    }

    #[test]
    fn removing_a_person_deletes_its_row_and_emits_a_delete() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &doc(&["a", "b"])).unwrap();
        publish(&tx, "acme", &doc(&["a"])).unwrap();
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().person_ids, vec!["a".to_string()]);
        let op: String = tx
            .query_row("SELECT op FROM org_events WHERE slug='acme' AND seq=3", [], |r| r.get(0))
            .unwrap();
        assert_eq!(op, "delete");
    }

    #[test]
    fn delete_person_fence_returns_a_touch_only_when_a_row_existed() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &doc(&["a", "b"])).unwrap();
        // Dropping a held fence removes the row and yields its delete touch.
        let touch = delete_person_fence(&tx, "acme", "a", "test").unwrap();
        let touch = touch.expect("a held fence yields a touch");
        assert_eq!(touch.op, "delete");
        assert_eq!(touch.entity_id, "a");
        assert_eq!(touch.detail_ref.as_deref(), Some("launch_intent:acme/a"));
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().person_ids, vec!["b".to_string()]);
        // Dropping an absent fence is idempotent: no row, no touch, no error.
        assert!(delete_person_fence(&tx, "acme", "a", "test").unwrap().is_none());
    }

    /// A WHOLE-DOCUMENT REPUBLISH DOES NOT WITHDRAW A WOKEN PERSON.
    ///
    /// The durable last gate under the operator's ruling of 2026-08-20 ("if
    /// woken, it needs to wait the 2 mins"). This function's set difference is
    /// the sharpest edge in the fence — it deletes every committed row the
    /// incoming document omits — and a document is a whole-fence value any
    /// caller can compute from a stale read. On `taperoom-inc` that is exactly
    /// what took `research-promoter` down 2.165 seconds after the operator woke
    /// her: one delete, empty actor, no note.
    ///
    /// `launch_intent::add` no longer computes its union from a stale read, and
    /// that is the cause. This is the gate underneath it, and it is what closes
    /// the window between any caller's fresh read and its commit.
    #[test]
    fn a_republish_retains_the_fence_of_somebody_inside_their_operator_wake_lease() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &doc(&["woken", "ordinary"])).unwrap();
        // `doc`'s stamp IS the publish clock, so the wake is one second before
        // the republish below and the lease is very much alive.
        tx.execute(
            "INSERT INTO person_activity(slug, person_id, last_desired_active, updated_at, \
             operator_wake_at) VALUES('acme', 'woken', 1, ?1, ?1)",
            rusqlite::params!["2026-07-24T23:59:59.000Z"],
        )
        .unwrap();

        // A republish that names NEITHER of them: the ordinary person goes, the
        // woken person stays.
        publish(&tx, "acme", &doc(&[])).unwrap();
        assert_eq!(
            reconstruct(&tx, "acme", "acme").unwrap().person_ids,
            vec!["woken".to_string()],
            "a republish computed without the woken person must not undo the operator's own \
             click; the ordinary person is withdrawn in the same call, so this is the lease \
             holding and not the delete failing"
        );
    }

    /// AND THE LEASE IS A FLOOR, NOT A CEILING. Past it the republish withdraws
    /// exactly as it always did — a wake that pinned somebody permanently would
    /// be a different defect, not a fix.
    #[test]
    fn a_republish_withdraws_normally_once_the_wake_lease_has_expired() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &doc(&["woken"])).unwrap();
        // Woken a full lease and a second before `doc`'s stamp. The lease moved
        // 120s -> 300s on 2026-08-24, so this instant moved with it: the point
        // of the fixture is "just PAST the lease", which is a relationship and
        // not a timestamp.
        tx.execute(
            "INSERT INTO person_activity(slug, person_id, last_desired_active, updated_at, \
             operator_wake_at) VALUES('acme', 'woken', 1, ?1, ?1)",
            rusqlite::params!["2026-07-24T23:54:59.000Z"],
        )
        .unwrap();
        publish(&tx, "acme", &doc(&[])).unwrap();
        assert!(
            reconstruct(&tx, "acme", "acme").unwrap().person_ids.is_empty(),
            "past the lease the ordinary narrowing resumes exactly"
        );
    }

    #[test]
    fn unchanged_publish_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &doc(&["a"])).unwrap();
        let out = publish(&tx, "acme", &doc(&["a"])).unwrap();
        assert_eq!(out, 1);
    }

    #[test]
    fn rejects_unmodeled_keys() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut d = doc(&["a"]);
        d.extra.insert("q".into(), serde_json::json!(1));
        assert_eq!(publish(&tx, "acme", &d).unwrap_err().code(), Some(UNMODELED_KEYS));
    }

    #[test]
    fn clear_deletes_all_rows_and_emits_a_delete_per_person() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", &doc(&["a", "b"])).unwrap();
        clear(&tx, "acme", "2026-07-25T00:00:00.000Z").unwrap();
        assert!(reconstruct(&tx, "acme", "acme").unwrap().person_ids.is_empty()); // ABSENT == empty person_ids
        assert_eq!(event_count(&tx), 4); // 2 upserts + 2 deletes
        let op: String = tx
            .query_row("SELECT op FROM org_events WHERE slug='acme' AND seq=4", [], |r| r.get(0))
            .unwrap();
        assert_eq!(op, "delete");
    }

    #[test]
    fn clear_on_absent_doc_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        clear(&tx, "acme", "2026-07-25T00:00:00.000Z").unwrap();
        assert_eq!(event_count(&tx), 0);
    }

    #[test]
    fn shadow_diff_zero_loss_on_a_multi_person_blob() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let blob = br#"{"version":1,"organization":"acme","sessionName":"org-acme","personIds":["head","worker","analyst"],"updatedAt":"2026-07-25T00:00:00.000Z"}"#;
        let report = shadow_diff_launch_intent(&tx, "acme", "acme", blob).unwrap();
        assert!(report.zero_loss(), "loud: {:?}", report.loud_failures());
        assert_eq!(report.row_count, 3);
    }
}
