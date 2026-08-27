//! Store-name → normalized-row dispatch for the writer's commit path
//! (org-data-normalization P0, BLOB-DEATH).
//!
//! `writer::persist` historically wrote every changed ledger store's body into
//! the `documents` blob. Under blob-death it instead dispatches each changed
//! store to that store's normalized row writer, via N8's fence-free
//! [`apply_and_emit`](crate::store::rows_txn::apply_and_emit) core. Persist owns
//! the `BEGIN IMMEDIATE` transaction on the company's single writer thread, so
//! each store diffs its current rows and emits audit identity directly.
//!
//! TWO SLUGS, and they are no longer the same string. `row_slug` is
//! `CompanyDb::label()` — the company KEY, `sha256(canonical <dir>)[..12]` — and
//! selects the SQL rows. `company_slug` is the company's DISPLAY name, read
//! from `org_settings.display_slug` by [`named`], and is what the row writers
//! stamp into their derived `organization` / `sessionName` fields. They were one
//! value while the label was the composite `<slug>@<rootHash>` and the name rode
//! inside the key; a directory hash carries no name, so the two parted.
//!
//! A store is wired for REPLACEMENT (rows written, `documents` skipped) only
//! when BOTH a persist entry (`backfill_*`) AND a removal entry (`clear`) exist,
//! so a dropped store can never orphan its rows. Every other store falls through
//! to the caller's `documents` path unchanged. This set grows one store at a
//! time as each gains a `clear` entry and its TS reader flips to rows.

use rusqlite::Transaction;

use crate::error::corrupt_store;
use crate::store::supervision::rows as supervision_rows;
use crate::store::{
    activity, converge_safety_rows, goal_delivery_quiesce_rows, health_monitor_rows,
    launch_intent_rows, operator_escalation_intents_rows, operator_escalation_push_rows,
    organization_rows, runtime_owner_rows, session_epoch_rows, session_maintenance,
    supervisor_watermark_rows,
};
use crate::ChiefdError;

/// Persist one changed store as normalized rows, or `None` when the store is not
/// wired (the caller then writes the `documents` blob unchanged).
///
/// `Some(Ok(()))` means the rows were written and the `documents` blob MUST be
/// skipped for this store. `Some(Err(_))` is a genuine row-write/validation
/// failure the caller must propagate (rolling back the whole commit).
#[must_use]
pub fn dispatch_persist(
    tx: &Transaction<'_>,
    slug: &str,
    store: &str,
    body: &str,
) -> Option<Result<(), ChiefdError>> {
    let blob = body.as_bytes();
    let result = match store {
        // ---- wired for REPLACEMENT (both persist + clear; dropCompanyStore can happen) ----
        launch_intent_rows::LAUNCH_INTENT_STORE => {
            launch_intent_rows::backfill_launch_intent(tx, slug, blob).map(|_| ())
        }
        goal_delivery_quiesce_rows::GOAL_DELIVERY_QUIESCE_STORE => named(tx, slug, |company| {
            goal_delivery_quiesce_rows::backfill_goal_delivery_quiesce(tx, slug, company, blob)
                .map(|_| ())
        }),
        converge_safety_rows::CONVERGE_SAFETY_STORE => {
            converge_safety_rows::backfill_converge_safety(tx, slug, blob).map(|_| ())
        }
        // The daemon's org-health duty store (F16 un-cross-wiring): its
        // `health::write` blob decodes as the duty's own state type and is
        // converted at this boundary into the rows type. This is the SAME
        // store the TS launcher publishes through the health-monitor route;
        // publish merges (Step 3), so the two writers are safe.
        health_monitor_rows::HEALTH_MONITOR_STORE => {
            health_monitor_rows::backfill_health_monitor(tx, slug, blob).map(|_| ())
        }
        session_maintenance::rows::SESSION_MAINTENANCE_STORE => {
            session_maintenance::rows::backfill_session_maintenance(tx, slug, blob).map(|_| ())
        }
        // ---- wired CHANGE-ONLY (never dropped → no clear needed; TS reader flipped) ----
        session_epoch_rows::SESSION_EPOCH_STORE => named(tx, slug, |company| {
            session_epoch_rows::backfill_session_epoch(tx, slug, company, blob).map(|_| ())
        }),
        runtime_owner_rows::RUNTIME_OWNER_STORE => named(tx, slug, |company| {
            runtime_owner_rows::backfill_runtime_owner(tx, slug, company, blob).map(|_| ())
        }),
        operator_escalation_intents_rows::OPERATOR_ESCALATION_INTENTS_STORE => {
            named(tx, slug, |company| {
                operator_escalation_intents_rows::backfill_operator_escalation_intents(
                    tx, slug, company, blob,
                )
                .map(|_| ())
            })
        }
        operator_escalation_push_rows::OPERATOR_ESCALATION_PUSH_STORE => {
            named(tx, slug, |company| {
                operator_escalation_push_rows::backfill_operator_escalation_push(
                    tx, slug, company, blob,
                )
                .map(|_| ())
            })
        }
        // An actor's initial in-memory organization seed is the one legacy
        // document-shaped write that reaches this boundary. Accept it only as
        // create-once genesis; every later aggregate snapshot is refused so it
        // cannot become a whole-manifest replacement path.
        organization_rows::ORGANIZATION_MANIFEST_STORE => (|| -> Result<(), ChiefdError> {
            let manifest: crate::store::organization::OrganizationManifest =
                serde_json::from_slice(blob).map_err(|e| corrupt_store("org-manifest-blob", e))?;
            match organization_rows::genesis(tx, slug, &manifest)? {
                organization_rows::ManifestGenesisOutcome::Created => Ok(()),
                organization_rows::ManifestGenesisOutcome::AlreadyExists => Err(ChiefdError::refused(
                    "manifest-write-retired",
                    "whole organization manifest writes are retired; use a named normalized operation",
                )),
            }
        })(),
        // BLOB-DEATH (N8): supervision is rows-authoritative for its META
        // families only. SupervisionLedger `#[serde(skip)]`s effects and
        // next_effect_sequence (supervision.rs) -- those families live in a
        // SEPARATE relational table written by the actor's own
        // `relational_diff` path (writer.rs ~persist), never here. Calling the
        // full `supervision_rows::publish` on a blob-decoded ledger would see
        // EMPTY effects and DELETE every real row, so this arm calls
        // `publish_meta` instead -- see its doc comment for the full
        // correctness argument. CHANGE-ONLY: supervision is removed only on
        // whole-company teardown, never by a single dispatched clear, so there
        // is no `dispatch_clear` arm here (wiring one would delete the effect
        // rows relational_diff owns).
        supervision_rows::SUPERVISION_STORE => (|| -> Result<(), ChiefdError> {
            let ledger: crate::store::supervision::SupervisionLedger =
                serde_json::from_slice(blob).map_err(|e| corrupt_store("supervision-blob", e))?;
            supervision_rows::publish_meta(tx, slug, &ledger).map(|_| ())
        })(),
        // BLOB-DEATH: activity is rows-authoritative (activity::rows). Its
        // backfill applies the typed diff directly inside this writer-owned
        // transaction; the returned event cursor is audit identity only. The
        // manifest rows MUST already exist for this company (a caller-ordering
        // INVALID_INPUT refusal surfaces loudly otherwise, never a panic).
        activity::rows::ACTIVITY_STORE => {
            activity::rows::backfill_activity(tx, slug, blob).map(|_| ())
        }
        // BLOB-DEATH (HAND C): supervisor-watermark is rows-authoritative
        // (supervisor_watermark_rows). Daemon-internal store, no TS reader, so
        // this is a straight REPLACEMENT wire like launch-intent/goal-delivery-
        // quiesce above -- its backfill directly diffs the current rows inside
        // the single-writer transaction and emits immutable audit events.
        supervisor_watermark_rows::SUPERVISOR_WATERMARK_STORE => {
            supervisor_watermark_rows::backfill_supervisor_watermark(tx, slug, blob).map(|_| ())
        }
        _ => return None,
    };
    Some(result)
}

/// Run one store's row writer with the company's DISPLAY slug resolved for it.
///
/// `slug` is the company KEY — `sha256(canonical <dir>)[..12]`, a hash that
/// carries no name — while every store wrapped here stamps the company's NAME
/// into a derived `organization` / `sessionName` field. The name is a stored
/// fact (`org_settings.display_slug`), so it is read, once, here.
///
/// Resolved per arm rather than once at the top of the dispatch, for two
/// reasons that are both correctness rather than economy: an UNWIRED store must
/// fall through to the caller's `documents` path without touching the database
/// at all, and the manifest arm IS the write that first names the company, so
/// it cannot be handed a name that does not exist yet. `writer::persist` sorts
/// the manifest store first precisely so every arm below runs after it.
fn named(
    tx: &Transaction<'_>,
    slug: &str,
    write: impl FnOnce(&str) -> Result<(), ChiefdError>,
) -> Result<(), ChiefdError> {
    write(&crate::store::org_settings::display_slug(tx, slug)?)
}

/// Clear one removed store's rows (a REAL delete — the doc becomes absent), or
/// `None` when the store is not wired (the caller then deletes the `documents`
/// row unchanged). `at` is the commit's ISO-8601 event stamp.
#[must_use]
pub fn dispatch_clear(
    tx: &Transaction<'_>,
    slug: &str,
    store: &str,
    at: &str,
) -> Option<Result<(), ChiefdError>> {
    let result = match store {
        launch_intent_rows::LAUNCH_INTENT_STORE => launch_intent_rows::clear(tx, slug, at),
        goal_delivery_quiesce_rows::GOAL_DELIVERY_QUIESCE_STORE => {
            named(tx, slug, |company| goal_delivery_quiesce_rows::clear(tx, slug, company, at))
        }
        converge_safety_rows::CONVERGE_SAFETY_STORE => converge_safety_rows::clear(tx, slug, at),
        supervisor_watermark_rows::SUPERVISOR_WATERMARK_STORE => {
            supervisor_watermark_rows::clear(tx, slug, at)
        }
        health_monitor_rows::HEALTH_MONITOR_STORE => health_monitor_rows::clear(tx, slug, at),
        session_maintenance::rows::SESSION_MAINTENANCE_STORE => {
            session_maintenance::rows::clear(tx, slug, at)
        }
        _ => return None,
    };
    Some(result)
}

/// Is `store` wired for row REPLACEMENT (both persist + clear entries)? Used by
/// the writer to decide whether to skip the `documents` write for it.
#[must_use]
pub fn is_wired(store: &str) -> bool {
    matches!(
        store,
        launch_intent_rows::LAUNCH_INTENT_STORE
            | goal_delivery_quiesce_rows::GOAL_DELIVERY_QUIESCE_STORE
            | converge_safety_rows::CONVERGE_SAFETY_STORE
            | session_epoch_rows::SESSION_EPOCH_STORE
            | runtime_owner_rows::RUNTIME_OWNER_STORE
            | operator_escalation_intents_rows::OPERATOR_ESCALATION_INTENTS_STORE
            | operator_escalation_push_rows::OPERATOR_ESCALATION_PUSH_STORE
            | organization_rows::ORGANIZATION_MANIFEST_STORE
            | supervision_rows::SUPERVISION_STORE
            | activity::rows::ACTIVITY_STORE
            | supervisor_watermark_rows::SUPERVISOR_WATERMARK_STORE
            | health_monitor_rows::HEALTH_MONITOR_STORE
            | session_maintenance::rows::SESSION_MAINTENANCE_STORE
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// An empty store — no company, not even a named one. Used by the tests
    /// whose subject IS genesis, and by the unwired-store test.
    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("schema");
        conn
    }

    /// A store where the row slug `acme` names a company.
    ///
    /// Every dispatched store below the manifest stamps the company's DISPLAY
    /// name into a derived field, and [`named`] reads that name from
    /// `org_settings`. A row slug with no `org_settings` row is a company that
    /// does not exist yet, which is exactly what the dispatch now refuses.
    fn open_named(display_slug: &str) -> Connection {
        let conn = open();
        conn.execute(
            "INSERT INTO org_settings(slug, display_slug, supervision_interval_ms, \
             acknowledgement_timeout_ms, acknowledgement_retry_limit, replacement_limit) \
             VALUES('acme', ?1, 900000, 60000, 3, 2)",
            rusqlite::params![display_slug],
        )
        .expect("seed org_settings");
        conn
    }

    fn assert_documents_table_is_absent(tx: &rusqlite::Transaction<'_>) {
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='documents'",
                [],
                |row| row.get(0),
            )
            .expect("inspect schema");
        assert_eq!(count, 0, "the documents blob table must not exist");
    }

    #[test]
    fn unwired_store_returns_none() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert!(dispatch_persist(&tx, "acme", "some-unported-store", "{}").is_none());
        assert!(dispatch_clear(&tx, "acme", "some-unported-store", "t").is_none());
        assert!(!is_wired("some-unported-store"));
    }

    #[test]
    fn wired_launch_intent_persists_rows_and_skips_documents() {
        let mut conn = open_named("acme");
        let tx = conn.transaction().unwrap();
        let body = r#"{"version":1,"organization":"acme","sessionName":"org-acme","personIds":["head","worker"],"updatedAt":"2026-07-25T00:00:00.000Z"}"#;
        assert!(is_wired("launch-intent"));
        dispatch_persist(&tx, "acme", "launch-intent", body).unwrap().unwrap();
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        // and no documents row was written by the dispatch path
        assert_documents_table_is_absent(&tx);
    }

    #[test]
    fn wired_session_maintenance_persists_and_clears_its_full_row_aggregate() {
        let mut conn = open_named("acme");
        let tx = conn.transaction().unwrap();
        let body = serde_json::to_string(
            &crate::store::session_maintenance::SessionMaintenanceLedger::initial(
                "acme",
                "2026-07-26T00:00:00.000Z",
            ),
        )
        .unwrap();

        assert!(is_wired(session_maintenance::rows::SESSION_MAINTENANCE_STORE));
        dispatch_persist(&tx, "acme", "session-maintenance", &body).unwrap().unwrap();
        let present: i64 = tx
            .query_row("SELECT COUNT(*) FROM maintenance_ledger WHERE slug = 'acme'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(present, 1);
        assert_documents_table_is_absent(&tx);

        dispatch_clear(&tx, "acme", "session-maintenance", "2026-07-26T00:00:01.000Z")
            .unwrap()
            .unwrap();
        let remaining: i64 =
            tx.query_row("SELECT COUNT(*) FROM maintenance_ledger", [], |row| row.get(0)).unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn wired_clear_removes_rows() {
        let mut conn = open_named("acme");
        let tx = conn.transaction().unwrap();
        let body = r#"{"version":1,"organization":"acme","sessionName":"org-acme","personIds":["head"],"updatedAt":"2026-07-25T00:00:00.000Z"}"#;
        dispatch_persist(&tx, "acme", "launch-intent", body).unwrap().unwrap();
        dispatch_clear(&tx, "acme", "launch-intent", "2026-07-25T00:00:01.000Z").unwrap().unwrap();
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn change_only_stores_are_wired() {
        for store in [
            "session-epoch",
            "runtime-owner",
            "operator-escalation-intents",
            "operator-escalation-push",
        ] {
            assert!(is_wired(store), "{store} must be wired");
        }
        assert!(!is_wired("materialization"), "the deleted checkpoint store must stay unwired");
        let mut conn = open_named("acme");
        let tx = conn.transaction().unwrap();
        assert!(dispatch_persist(&tx, "acme", "materialization", "{}").is_none());
    }

    #[test]
    fn wired_session_epoch_persists_rows_and_skips_documents() {
        let mut conn = open_named("acme");
        let tx = conn.transaction().unwrap();
        let body = r#"{"version":1,"organization":"acme","epochAt":"2026-07-25T06:46:10.852Z","reason":"boot"}"#;
        dispatch_persist(&tx, "acme", "session-epoch", body).unwrap().unwrap();
        let n: i64 = tx
            .query_row("SELECT COUNT(*) FROM session_epoch WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // change-only store: no clear entry (never dropped).
        assert!(dispatch_clear(&tx, "acme", "session-epoch", "t").is_none());
    }

    #[test]
    fn only_the_first_manifest_persist_becomes_atomic_genesis() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = "acme".to_string();
        let body = serde_json::to_string(&manifest).unwrap();
        dispatch_persist(&tx, "acme", "org-manifest", &body)
            .expect("manifest is intercepted")
            .expect("first manifest write is genesis");
        let people: i64 = tx
            .query_row("SELECT COUNT(*) FROM people WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert!(people > 0, "genesis writes the normalized organization rows");
        let error = dispatch_persist(&tx, "acme", "org-manifest", &body)
            .expect("manifest is intercepted")
            .expect_err("whole manifest replacement is retired");
        assert_eq!(error.code(), Some("manifest-write-retired"));
        assert_documents_table_is_absent(&tx);
    }

    /// Seed `northstar_manifest` once into rows so a dependent store (activity)
    /// has a manifest to reconstruct against.
    fn seed_manifest_rows(
        tx: &rusqlite::Transaction<'_>,
        slug: &str,
    ) -> crate::store::organization::OrganizationManifest {
        // The fixture defaults `.slug` to "northstar-conformance"; activity's
        // backfill validates the ledger's embedded org slug against the row
        // slug it is published under, so callers using a different row slug
        // (e.g. "acme") must correct the manifest first.
        let mut manifest = crate::test_support::northstar_manifest(1_784_116_800_000);
        manifest.slug = slug.to_string();
        let outcome = organization_rows::genesis(tx, slug, &manifest).expect("seed manifest");
        assert!(matches!(outcome, organization_rows::ManifestGenesisOutcome::Created));
        manifest
    }

    #[test]
    fn wired_activity_persists_rows_and_skips_documents() {
        use crate::clock::WallMillis;
        use crate::ledger::Ledgers;

        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let manifest = seed_manifest_rows(&tx, "acme");

        // Build a real, valid `ActivityLedger` body the way the daemon would
        // (seeded from the manifest), not a hand-rolled fixture.
        let mut ledgers = Ledgers::empty(WallMillis(1_784_116_800_000));
        crate::store::activity::seed(&mut ledgers, &manifest).expect("seed activity");
        let ledger = crate::store::activity::read(&ledgers, &manifest).expect("read activity");
        let body = serde_json::to_string(&ledger).unwrap();

        assert!(is_wired("activity"));
        dispatch_persist(&tx, "acme", "activity", &body).unwrap().unwrap();

        // rows are the authority: activity_meta landed.
        let n: i64 = tx
            .query_row("SELECT COUNT(*) FROM activity_meta WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "activity rows must be populated by the dispatch");

        // and no documents row was written by the dispatch path (BLOB-DEATH).
        assert_documents_table_is_absent(&tx);

        // change-only store: no clear entry (activity is never dropped in prod).
        assert!(dispatch_clear(&tx, "acme", "activity", "t").is_none());
    }

    #[test]
    fn wired_supervision_meta_persists_rows_skips_documents_and_leaves_effects_intact() {
        use crate::store::supervision::rows as supervision_rows;
        use crate::store::supervision::SupervisionLedger;

        let mut conn = open_named("acme");
        let tx = conn.transaction().unwrap();
        let manifest = crate::test_support::northstar_manifest(1_784_116_800_000);

        // Seed an effect row directly (the relational half `relational_diff`
        // owns in production) BEFORE any supervision meta dispatch, so we can
        // prove the meta-only dispatch below never touches it.
        tx.execute(
            "INSERT INTO effects(slug, id, seq, kind, status) \
             VALUES ('acme', 'effect-1', 1, 'person_reminder', 'pending')",
            [],
        )
        .unwrap();

        let ledger = SupervisionLedger::initial(&manifest, "2026-07-26T00:00:00.000Z");
        let body = serde_json::to_string(&ledger).unwrap();

        assert!(is_wired(supervision_rows::SUPERVISION_STORE));
        dispatch_persist(&tx, "acme", supervision_rows::SUPERVISION_STORE, &body).unwrap().unwrap();

        // meta rows landed.
        let meta: i64 = tx
            .query_row("SELECT COUNT(*) FROM supervision_meta WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(meta, 1, "supervision_meta row must be populated by the meta-only dispatch");

        // no documents row was written by the dispatch path (BLOB-DEATH).
        assert_documents_table_is_absent(&tx);

        // THE ASSERTION: the pre-existing effect row is untouched -- the
        // meta-only dispatch must never wipe effects (it does not call
        // diff_effects, unlike the full `publish`).
        let effects: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM effects WHERE slug='acme' AND id='effect-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(effects, 1, "a supervision meta write must never delete effect rows");

        // change-only store: no clear entry (supervision is removed only on
        // whole-company teardown, never a single dispatched clear).
        assert!(dispatch_clear(&tx, "acme", supervision_rows::SUPERVISION_STORE, "t").is_none());
    }

    #[test]
    fn wired_converge_safety_persists_rows_skips_documents_and_clears() {
        let mut conn = open_named("acme");
        let tx = conn.transaction().unwrap();
        let body = r#"{"schemaVersion":1,"actuationMode":"apply","sweepLive":true,"budgetOverride":false,"consecutiveFailures":2,"breakerTripped":true,"breakerTrippedAt":"2026-07-25T06:00:00.000Z","cycleInProgress":false,"cycleStartedAtMs":1784000000000,"lastRefusal":{"kind":"circuit-breaker","detail":"three consecutive failures","at":"2026-07-25T06:01:00.000Z"}}"#;
        assert!(is_wired("converge-safety"));
        dispatch_persist(&tx, "acme", "converge-safety", body).unwrap().unwrap();
        let n: i64 = tx
            .query_row("SELECT COUNT(*) FROM converge_safety WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // and no documents row was written by the dispatch path (BLOB-DEATH).
        assert_documents_table_is_absent(&tx);
        // REPLACEMENT store: clear removes the row.
        dispatch_clear(&tx, "acme", "converge-safety", "2026-07-25T06:05:00.000Z")
            .unwrap()
            .unwrap();
        let n2: i64 = tx
            .query_row("SELECT COUNT(*) FROM converge_safety WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n2, 0);
    }

    #[test]
    fn wired_supervisor_watermark_persists_rows_and_skips_documents() {
        let mut conn = open_named("acme");
        let tx = conn.transaction().unwrap();
        let body = r#"{"schemaVersion":1,"organization":"acme","duties":{"mailbox-wake":{"duty":"mailbox-wake","intervalMs":900000,"lastSuccessAt":"2026-07-26T00:00:00.000Z","runCount":3}}}"#;
        assert!(is_wired("supervisor-watermark"));
        dispatch_persist(&tx, "acme", "supervisor-watermark", body).unwrap().unwrap();
        // rows are the authority: the duty row landed.
        let n: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM supervisor_watermarks WHERE slug='acme' AND duty='mailbox-wake'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        // and no documents row was written by the dispatch path (BLOB-DEATH).
        assert_documents_table_is_absent(&tx);
        // REPLACEMENT store: clear removes the rows.
        dispatch_clear(&tx, "acme", "supervisor-watermark", "2026-07-26T00:00:01.000Z")
            .unwrap()
            .unwrap();
        let n: i64 = tx
            .query_row("SELECT COUNT(*) FROM supervisor_watermarks WHERE slug='acme'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn wired_goal_delivery_quiesce_round_trips() {
        let mut conn = open_named("acme");
        let tx = conn.transaction().unwrap();
        let body = r#"{"version":1,"organization":"acme","sessionName":"org-acme","quiescedAt":"2026-07-25T06:46:10.852Z"}"#;
        dispatch_persist(&tx, "acme", "goal-delivery-quiesce", body).unwrap().unwrap();
        let since: String =
            tx.query_row("SELECT since FROM quiesce WHERE slug='acme'", [], |r| r.get(0)).unwrap();
        assert_eq!(since, "2026-07-25T06:46:10.852Z");
    }
}
