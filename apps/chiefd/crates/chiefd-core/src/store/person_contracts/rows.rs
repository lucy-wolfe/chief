//! The person-contracts ROW implementation (org-data-normalization P0, N2-contracts).
//!
//! Reconstruct an [`OrganizationPersonContracts`] document from the normalized
//! `person_contracts` table, and publish a whole document by diffing it against
//! the current rows. Sibling of `store::organization_rows` (the manifest port):
//! same direct atomic writer scaffold and one-`org_events`-row-per-touched-
//! entity contract, with its OWN DTO + diff.
//!
//! IMPORTANT — this is NOT the manifest. `person-contracts` is the per-person
//! operating-contract TEXT (the `AGENTS.md` a pane reads), stored flat as one
//! row per (company, person): `text` and its `md5`. It shares NO tables with
//! the manifest port — it owns the
//! dedicated `person_contracts` table alone (delta #27), so there is no
//! row-ownership overlap with `departments`/`people`.
//!
//! Identity that is DERIVED, never stored: `version` is the compile-time
//! constant `1`; `organization` is the process's own company slug. Neither is a
//! column (mirrors the manifest's derived `schema_version`/`slug`).
//!
//! Item D (Fable #6): a normalized document carries NO unmodeled keys. Publish
//! REJECTS any `extra` — on the document or on any entry — with [`UNMODELED_KEYS`]
//! (+ the offending dotted paths). Read-tolerant/write-strict per #337: the
//! live blob (cobalt tribes-capital, 59 entries) has ZERO legacy keys, so there
//! is nothing to tolerate on read; the allowlist is exactly `{text, md5}`.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::Refusal;
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::ChiefdError;

/// The document schema version — a compile-time constant, never a stored column.
pub const PERSON_CONTRACTS_VERSION: u32 = 1;

/// A publish carried a key the row model does not represent (item D). The detail
/// lists the offending dotted paths so the caller fixes the exact field.
pub const UNMODELED_KEYS: &str = "unmodeled-keys";

/// A structurally invalid document (wrong `version`, or an `organization` that
/// is not this process's own company). Maps to 422 like [`UNMODELED_KEYS`].
pub const CONTRACTS_INVALID: &str = "person-contracts-invalid";

/// [`projection_plan`] was asked about a person with no stored contract row.
/// Maps to 422 — the caller's lazy-backfill publish is a separate step
/// (unchanged by E7-S3) that must run before the plan can be requested again.
pub const UNKNOWN_PERSON_CONTRACT: &str = "unknown-person-contract";

/// One person's stored contract: the text and its MD5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonContractEntry {
    /// The rendered `AGENTS.md` contract text.
    pub text: String,
    /// `md5(text)` — the value the TS boot path compares the on-disk file to.
    pub md5: String,
    /// Keys this port does not model, preserved verbatim so item D can reject
    /// them loudly (never silently dropped).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The durable `person-contracts` document: one entry per person for a company.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationPersonContracts {
    /// Always [`PERSON_CONTRACTS_VERSION`]; DERIVED on read, validated on write.
    pub version: u32,
    /// The company slug; DERIVED on read from the process's own slug.
    pub organization: String,
    /// Contract entries keyed by person id.
    pub contracts: BTreeMap<String, PersonContractEntry>,
    /// Unmodeled top-level keys (item D). Rejected on publish.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// A SQL failure reading/writing the normalized rows is a store failure, not a
/// caller error and not corruption. Greppable single mapping point for every
/// `.map_err`; the real `rusqlite::Error` travels inside the value.
fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("person-contracts-rows", e)
}

fn invalid(code: &'static str, message: impl Into<String>) -> ChiefdError {
    ChiefdError::from(Refusal::new(code, message))
}

/// Wrapper giving `ChiefdError` a `From<rusqlite::Error>` at the scaffold
/// boundary without a blanket impl (mirrors `organization_rows::RowsSqlError`).
/// Unwrapped immediately by [`publish`].
pub struct RowsSqlError(pub ChiefdError);
impl From<rusqlite::Error> for RowsSqlError {
    fn from(e: rusqlite::Error) -> Self {
        RowsSqlError(store_failure(e))
    }
}
impl From<ChiefdError> for RowsSqlError {
    fn from(e: ChiefdError) -> Self {
        RowsSqlError(e)
    }
}

// ---- reconstruct (read path) ---------------------------------------------

/// Reconstruct the contracts document for `company_slug` from the rows keyed by
/// `row_slug`, or `None` when the company has no contract rows at all (never
/// published). `version`/`organization` are DERIVED, never read from a column.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<Option<OrganizationPersonContracts>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT person_id, text, md5 \
             FROM person_contracts WHERE slug = ?1 ORDER BY person_id",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![row_slug], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(store_failure)?;
    let mut contracts = BTreeMap::new();
    for row in rows {
        let (person_id, text, md5) = row.map_err(store_failure)?;
        contracts.insert(person_id, PersonContractEntry { text, md5, extra: BTreeMap::new() });
    }
    if contracts.is_empty() {
        return Ok(None);
    }
    Ok(Some(OrganizationPersonContracts {
        version: PERSON_CONTRACTS_VERSION,
        organization: company_slug.to_string(),
        contracts,
        extra: BTreeMap::new(),
    }))
}

// ---- publish (diff/write path) -------------------------------------------

/// Publish a whole contracts document into the rows from current SQLite state.
///
/// Rejects unmodeled keys (item D) and validates identity BEFORE writing, then
/// diffs the incoming document against the current rows at ENTITY (per-person)
/// granularity: each added or changed contract is rewritten and emits one
/// `org_events` row; each removed person emits a delete event. `at` is the
/// ISO-8601 event stamp (caller clock authority). Returns the final immutable
/// audit event sequence (or the unchanged cursor for a no-op document).
///
/// # Errors
/// [`UNMODELED_KEYS`] / [`CONTRACTS_INVALID`] refusals (map to 422); SQL
/// failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    at: &str,
    incoming: &OrganizationPersonContracts,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    validate(incoming, company_slug)?;

    let current = reconstruct(tx, row_slug, company_slug)?;

    apply_and_emit::<RowsSqlError, _>(tx, row_slug, at, "", |tx| {
        let mut touches = Vec::new();
        // Removals: any current person id absent from the incoming set.
        if let Some(cur) = &current {
            for id in cur.contracts.keys() {
                if !incoming.contracts.contains_key(id) {
                    tx.execute(
                        "DELETE FROM person_contracts WHERE slug=?1 AND person_id=?2",
                        params![row_slug, id],
                    )?;
                    touches.push(EventTouch::new(
                        "person-contract",
                        id,
                        "delete",
                        "person_contracts",
                        row_slug,
                    ));
                }
            }
        }
        // Upserts: added or changed entries only (a no-op entry is skipped).
        for (id, entry) in &incoming.contracts {
            let unchanged = current
                .as_ref()
                .and_then(|c| c.contracts.get(id))
                .map(|prev| prev == entry)
                .unwrap_or(false);
            if unchanged {
                continue;
            }
            tx.execute(
                "INSERT INTO person_contracts(slug, person_id, text, md5) \
                 VALUES(?1,?2,?3,?4) \
                 ON CONFLICT(slug,person_id) DO UPDATE SET text=?3, md5=?4",
                params![row_slug, id, entry.text, entry.md5],
            )?;
            touches.push(EventTouch::new(
                "person-contract",
                id,
                "upsert",
                "person_contracts",
                row_slug,
            ));
        }
        Ok(touches)
    })
    .map_err(|RowsSqlError(e)| e)
}

// ---- projection plan (E7-S3) -----------------------------------------------
//
// Moves the "does workspace/AGENTS.md match the stored contract?" MD5
// comparison from TypeScript (`ensureOrganizationPersonContractFile`,
// org-person-contracts.ts:164-187) into Rust: the caller supplies what it
// observed on disk, this decides `write` (with the text to overwrite the file
// with) or `keep`, and never touches a row itself — a pure read, like
// `reconstruct`.

/// One person's on-disk observation for the projection-plan decision: the
/// caller's already-computed MD5 of `workspace/AGENTS.md`, or `None` when the
/// file is missing or unreadable.
#[derive(Debug, Clone)]
pub struct ObservedContract {
    /// The person whose `AGENTS.md` this observation is about.
    pub person_id: String,
    /// The MD5 of the on-disk file, or `None` when it is missing/unreadable.
    pub md5: Option<String>,
}

/// The Rust-selected action for one person's `AGENTS.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionAction {
    /// The on-disk file is missing or stale; overwrite it with this text.
    Write {
        /// The stored contract text to write to `workspace/AGENTS.md`.
        text: String,
    },
    /// The on-disk file already matches the stored contract.
    Keep,
}

/// Decide, for each requested person (in request order), whether their
/// on-disk `AGENTS.md` needs to be rewritten from the stored contract. Pure
/// read — writes nothing.
///
/// For each entry: look up the `person_contracts` row; when absent, REFUSE
/// [`UNKNOWN_PERSON_CONTRACT`] (the caller's lazy-backfill publish is a
/// separate, unchanged step); when present, `Write` iff `observed.md5` is
/// `None` or differs from the stored `md5`, else `Keep`.
///
/// # Errors
/// [`UNKNOWN_PERSON_CONTRACT`] `Refused` for any requested person with no
/// stored row; SQL failures as [`ChiefdError::StoreFailure`].
pub fn projection_plan(
    tx: &Transaction<'_>,
    row_slug: &str,
    observed: &[ObservedContract],
) -> Result<Vec<(String, ProjectionAction)>, ChiefdError> {
    let mut plan = Vec::with_capacity(observed.len());
    for item in observed {
        let stored: Option<(String, String)> = tx
            .query_row(
                "SELECT text, md5 FROM person_contracts WHERE slug=?1 AND person_id=?2",
                params![row_slug, item.person_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(store_failure)?;
        let Some((text, stored_md5)) = stored else {
            return Err(invalid(
                UNKNOWN_PERSON_CONTRACT,
                format!("no stored person-contract for '{}'", item.person_id),
            ));
        };
        let action = match &item.md5 {
            Some(observed_md5) if *observed_md5 == stored_md5 => ProjectionAction::Keep,
            _ => ProjectionAction::Write { text },
        };
        plan.push((item.person_id.clone(), action));
    }
    Ok(plan)
}

/// Reject any `extra` (serde-flatten) key on the document or an entry — a
/// normalized document carries none (item D). NEVER silently drops.
fn reject_unmodeled_keys(doc: &OrganizationPersonContracts) -> Result<(), ChiefdError> {
    let mut paths = Vec::new();
    for key in doc.extra.keys() {
        paths.push(format!("extra.{key}"));
    }
    for (id, entry) in &doc.contracts {
        for key in entry.extra.keys() {
            paths.push(format!("contracts.{id}.extra.{key}"));
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    paths.sort();
    Err(invalid(
        UNMODELED_KEYS,
        format!(
            "person-contracts carries unmodeled keys the row model cannot store: {}",
            paths.join(", ")
        ),
    ))
}

/// Validate the DERIVED identity: `version` must be the constant and
/// `organization` must be this process's own company.
fn validate(doc: &OrganizationPersonContracts, company_slug: &str) -> Result<(), ChiefdError> {
    if doc.version != PERSON_CONTRACTS_VERSION {
        return Err(invalid(
            CONTRACTS_INVALID,
            format!(
                "unsupported person-contracts version {} (expected {PERSON_CONTRACTS_VERSION})",
                doc.version
            ),
        ));
    }
    if doc.organization != company_slug {
        return Err(invalid(
            CONTRACTS_INVALID,
            format!(
                "person-contracts organization '{}' is not this company '{}'",
                doc.organization, company_slug
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;
    use rusqlite::Connection;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("company schema");
        conn
    }

    fn entry(text: &str) -> PersonContractEntry {
        PersonContractEntry {
            text: text.to_string(),
            md5: format!("md5-of-{text}"),
            extra: BTreeMap::new(),
        }
    }

    fn doc(slug: &str, entries: &[(&str, PersonContractEntry)]) -> OrganizationPersonContracts {
        OrganizationPersonContracts {
            version: PERSON_CONTRACTS_VERSION,
            organization: slug.to_string(),
            contracts: entries.iter().map(|(id, e)| (id.to_string(), e.clone())).collect(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn reconstruct_is_none_before_any_publish() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert!(reconstruct(&tx, "acme", "acme").unwrap().is_none());
    }

    #[test]
    fn publish_then_reconstruct_round_trips_and_derives_identity() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let incoming = doc("acme", &[("chief", entry("be the CEO")), ("ada", entry("build it"))]);
        let out = publish(&tx, "acme", "acme", "2026-07-25T00:00:00.000Z", &incoming).unwrap();
        assert_eq!(out, 2);

        let back = reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        // Identity is DERIVED, not stored — it comes back exactly.
        assert_eq!(back.version, PERSON_CONTRACTS_VERSION);
        assert_eq!(back.organization, "acme");
        assert_eq!(back, incoming);
    }

    #[test]
    fn one_org_event_per_touched_person_with_table_pk_detail_ref() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", "t", &doc("acme", &[("chief", entry("x"))])).unwrap();
        let (entity, op, detail): (String, String, String) = tx
            .query_row(
                "SELECT entity, op, detail_ref FROM org_events WHERE slug='acme' AND seq=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(entity, "person-contract");
        assert_eq!(op, "upsert");
        assert_eq!(detail, "person_contracts:acme/chief");
    }

    #[test]
    fn diff_touches_only_changed_added_and_removed_entries() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(
            &tx,
            "acme",
            "acme",
            "t",
            &doc("acme", &[("chief", entry("v1")), ("ada", entry("v1"))]),
        )
        .unwrap();
        // ceo unchanged, ada changed, bob added, (nobody removed here).
        let out = publish(
            &tx,
            "acme",
            "acme",
            "t",
            &doc("acme", &[("chief", entry("v1")), ("ada", entry("v2")), ("bob", entry("v1"))]),
        )
        .unwrap();
        // Two touches (ada upsert, bob upsert) => seq advances 2 -> 4.
        assert_eq!(out, 4);
        let touched: i64 = tx
            .query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme' AND seq > 2", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(touched, 2);

        // Now drop ada -> one delete event.
        let out = publish(
            &tx,
            "acme",
            "acme",
            "t",
            &doc("acme", &[("chief", entry("v1")), ("bob", entry("v1"))]),
        )
        .unwrap();
        assert_eq!(out, 5);
        let (op, id): (String, String) = tx
            .query_row(
                "SELECT op, entity_id FROM org_events WHERE slug='acme' AND seq=5",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(op, "delete");
        assert_eq!(id, "ada");
        assert!(!reconstruct(&tx, "acme", "acme").unwrap().unwrap().contracts.contains_key("ada"));
    }

    #[test]
    fn a_no_op_republish_keeps_the_audit_cursor() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let d = doc("acme", &[("chief", entry("x"))]);
        publish(&tx, "acme", "acme", "t", &d).unwrap();
        let out = publish(&tx, "acme", "acme", "t", &d).unwrap();
        assert_eq!(out, 1);
    }

    #[test]
    fn a_second_direct_publish_replaces_the_current_contract() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", "t", &doc("acme", &[("chief", entry("x"))])).unwrap();
        let out =
            publish(&tx, "acme", "acme", "t", &doc("acme", &[("chief", entry("y"))])).unwrap();
        assert_eq!(out, 2);
        assert_eq!(reconstruct(&tx, "acme", "acme").unwrap().unwrap().contracts["chief"].text, "y");
    }

    #[test]
    fn item_d_rejects_unmodeled_keys_on_document_and_entry() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut d = doc("acme", &[("chief", entry("x"))]);
        d.extra.insert("legacy".to_string(), serde_json::json!(true));
        d.contracts
            .get_mut("chief")
            .unwrap()
            .extra
            .insert("stale".to_string(), serde_json::json!(1));
        let err = publish(&tx, "acme", "acme", "t", &d).unwrap_err();
        match err {
            ChiefdError::Refused(r) => {
                assert_eq!(r.code, UNMODELED_KEYS);
                assert!(r.message.contains("contracts.chief.extra.stale"));
                assert!(r.message.contains("extra.legacy"));
            }
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    /// Live-.bak CONTROL (org-data-normalization P0). Reads the real cobalt
    /// tribes-capital `person-contracts` blob from $PERSON_CONTRACTS_LIVE_BLOB,
    /// deserializes it, and publishes it into fresh rows — proving 0
    /// unmodeled-keys / 0 Corrupt against production data. Skipped when the env
    /// var is unset (ordinary CI has no live blob).
    #[test]
    fn live_bak_blob_publishes_into_fresh_rows_with_no_unmodeled_keys() {
        let Ok(path) = std::env::var("PERSON_CONTRACTS_LIVE_BLOB") else { return };
        let raw = std::fs::read_to_string(&path).expect("read live blob");
        let doc: OrganizationPersonContracts =
            serde_json::from_str(&raw).expect("deserialize live blob");
        // No legacy keys anywhere (item-D allowlist holds against production).
        assert!(
            doc.extra.is_empty(),
            "top-level extra: {:?}",
            doc.extra.keys().collect::<Vec<_>>()
        );
        for (id, e) in &doc.contracts {
            assert!(
                e.extra.is_empty(),
                "entry {id} extra: {:?}",
                e.extra.keys().collect::<Vec<_>>()
            );
        }
        let n = doc.contracts.len();
        let slug = doc.organization.clone();
        eprintln!("[live-control] organization={slug} entries={n}");

        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = publish(&tx, &slug, &slug, "2026-07-25T00:00:00.000Z", &doc)
            .expect("publish live blob (no Corrupt/Refused)");
        assert_eq!(out, n as i64);
        let back = reconstruct(&tx, &slug, &slug).unwrap().unwrap();
        assert_eq!(back, doc, "live blob did not round-trip");
        eprintln!(
            "[live-control] PASS: {n} contracts published + round-tripped, 0 unmodeled, 0 Corrupt"
        );
    }

    #[test]
    fn projection_plan_keeps_a_matching_md5_with_no_text() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", "t", &doc("acme", &[("chief", entry("x"))])).unwrap();
        let plan = projection_plan(
            &tx,
            "acme",
            &[ObservedContract {
                person_id: "chief".to_string(),
                md5: Some("md5-of-x".to_string()),
            }],
        )
        .unwrap();
        assert_eq!(plan, vec![("chief".to_string(), ProjectionAction::Keep)]);
    }

    #[test]
    fn projection_plan_writes_the_stored_text_on_a_differing_md5() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", "t", &doc("acme", &[("chief", entry("x"))])).unwrap();
        let plan = projection_plan(
            &tx,
            "acme",
            &[ObservedContract {
                person_id: "chief".to_string(),
                md5: Some("stale-md5".to_string()),
            }],
        )
        .unwrap();
        assert_eq!(
            plan,
            vec![("chief".to_string(), ProjectionAction::Write { text: "x".to_string() })]
        );
    }

    #[test]
    fn projection_plan_writes_when_the_file_is_missing() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(&tx, "acme", "acme", "t", &doc("acme", &[("chief", entry("x"))])).unwrap();
        let plan = projection_plan(
            &tx,
            "acme",
            &[ObservedContract { person_id: "chief".to_string(), md5: None }],
        )
        .unwrap();
        assert_eq!(
            plan,
            vec![("chief".to_string(), ProjectionAction::Write { text: "x".to_string() })]
        );
    }

    #[test]
    fn projection_plan_refuses_an_unknown_person() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let err = projection_plan(
            &tx,
            "acme",
            &[ObservedContract { person_id: "ghost".to_string(), md5: None }],
        )
        .unwrap_err();
        match err {
            ChiefdError::Refused(r) => assert_eq!(r.code, UNKNOWN_PERSON_CONTRACT),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn projection_plan_returns_one_action_per_person_in_request_order() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        publish(
            &tx,
            "acme",
            "acme",
            "t",
            &doc("acme", &[("chief", entry("x")), ("ada", entry("y"))]),
        )
        .unwrap();
        let plan = projection_plan(
            &tx,
            "acme",
            &[
                ObservedContract {
                    person_id: "ada".to_string(),
                    md5: Some("md5-of-y".to_string()),
                },
                ObservedContract { person_id: "chief".to_string(), md5: None },
            ],
        )
        .unwrap();
        assert_eq!(
            plan,
            vec![
                ("ada".to_string(), ProjectionAction::Keep),
                ("chief".to_string(), ProjectionAction::Write { text: "x".to_string() }),
            ]
        );
    }

    #[test]
    fn wrong_version_and_foreign_org_are_rejected() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut bad_ver = doc("acme", &[("chief", entry("x"))]);
        bad_ver.version = 2;
        assert!(matches!(
            publish(&tx, "acme", "acme", "t", &bad_ver).unwrap_err(),
            ChiefdError::Refused(r) if r.code == CONTRACTS_INVALID
        ));
        let foreign = doc("other", &[("chief", entry("x"))]);
        assert!(matches!(
            publish(&tx, "acme", "acme", "t", &foreign).unwrap_err(),
            ChiefdError::Refused(r) if r.code == CONTRACTS_INVALID
        ));
    }
}
