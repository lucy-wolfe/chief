//! `org_settings.launcher_root` — the source checkout that last materialized
//! this company (E7-S3), replacing `state/launcher.json`. The four policy
//! ints (`supervision_interval_ms`, …) stay owned by the manifest
//! genesis/policy paths (`store::organization_rows::write_org_settings`);
//! [`publish_launcher_root`] touches ONLY the `launcher_root` column, so a
//! settings publish can never silently rewrite policy.
//!
//! Sibling of `store::person_contracts::rows`: same direct atomic writer
//! scaffold ([`rows_txn::apply_and_emit`]), one `org_events` row per publish,
//! `BEGIN IMMEDIATE` supplied by the caller (`CompanyDb::in_transaction`).

use rusqlite::{params, OptionalExtension, Transaction};

use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::ChiefdError;

/// A company has no `org_settings` row at all — genesis has not run for this
/// slug. [`publish_launcher_root`] refuses rather than inventing a row: the
/// four policy ints are the manifest genesis path's to seed, never this one's.
pub const UNKNOWN_COMPANY: &str = "unknown-company";

/// The org-settings singleton, launcher-root column included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgSettings {
    /// Absolute path of the source checkout that last materialized this
    /// company. `None` until a launch has ever published it.
    pub launcher_root: Option<String>,
    /// Milliseconds between supervision passes. Owned by the manifest
    /// genesis/policy paths, never by [`publish_launcher_root`].
    pub supervision_interval_ms: i64,
    /// Milliseconds before an unacknowledged goal escalates.
    pub acknowledgement_timeout_ms: i64,
    /// Retries before an unacknowledged goal is treated as failed.
    pub acknowledgement_retry_limit: i64,
    /// Concurrent replacement cap.
    pub replacement_limit: i64,
}

/// A SQL failure reading/writing `org_settings` is a store failure, not a
/// caller error. Greppable single mapping point for every `.map_err`.
fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("org-settings", e)
}

/// Wrapper giving `ChiefdError` a `From<rusqlite::Error>` at the scaffold
/// boundary without a blanket impl (mirrors `organization_rows::RowsSqlError`
/// / `person_contracts::rows::RowsSqlError`). Unwrapped immediately by
/// [`publish_launcher_root`].
struct RowsSqlError(ChiefdError);
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

/// Read the org-settings singleton, `launcher_root` included. `None` when the
/// company has no `org_settings` row (genesis has not run for this slug).
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn read(tx: &Transaction<'_>, slug: &str) -> Result<Option<OrgSettings>, ChiefdError> {
    tx.query_row(
        "SELECT launcher_root, supervision_interval_ms, acknowledgement_timeout_ms, \
         acknowledgement_retry_limit, replacement_limit FROM org_settings WHERE slug = ?1",
        params![slug],
        |row| {
            Ok(OrgSettings {
                launcher_root: row.get(0)?,
                supervision_interval_ms: row.get(1)?,
                acknowledgement_timeout_ms: row.get(2)?,
                acknowledgement_retry_limit: row.get(3)?,
                replacement_limit: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(store_failure)
}

/// The company's DISPLAY slug — the name genesis committed for the company
/// stored under `row_slug`.
///
/// Every derived `organization` / `sessionName` / `runtime_session` field means
/// THIS value, and `row_slug` is `sha256(canonical <dir>)[..12]` — a hash that
/// carries no name. So a caller holding only the row key has to ask the store,
/// and this is where it asks. Callers that already hold a manifest read
/// `manifest.slug` instead; it is the same fact from the same column
/// (`organization_rows::read_policy` returns it alongside the policy).
///
/// REFUSES rather than defaulting to `row_slug`: a company genesis has not
/// named yet has no display slug, and answering with the key would be a second
/// source of truth for a company's name — exactly the confusion that made a
/// correctly seeded person-contracts document fail its own identity check.
///
/// # Errors
/// [`UNKNOWN_COMPANY`] `Refused` when the company has no `org_settings` row
/// yet; SQL failures as [`ChiefdError::StoreFailure`].
pub fn display_slug(tx: &Transaction<'_>, row_slug: &str) -> Result<String, ChiefdError> {
    read_display_slug(tx, row_slug)?.ok_or_else(|| {
        ChiefdError::refused(
            UNKNOWN_COMPANY,
            format!("no org_settings row for company '{row_slug}', so it has no display slug yet"),
        )
    })
}

/// The display slug, or `None` when genesis has not named this company yet.
///
/// [`display_slug`] is what callers want: they are about to stamp a name into a
/// document, and a company with no name cannot have one stamped. This form
/// exists for the ONE caller that legitimately opens a company BEFORE genesis —
/// `actor::writer::load_ledgers`, which boots the writer for a directory whose
/// company is about to be created. For it, an unnamed company is the expected
/// starting state, not a refusal.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn read_display_slug(
    tx: &Transaction<'_>,
    row_slug: &str,
) -> Result<Option<String>, ChiefdError> {
    tx.query_row(
        "SELECT display_slug FROM org_settings WHERE slug = ?1",
        params![row_slug],
        |row| row.get(0),
    )
    .optional()
    .map_err(store_failure)
}

/// Publish ONLY the `launcher_root` column — ONE `BEGIN IMMEDIATE` transaction
/// (ruling D19/mandate 4): the column `UPDATE` and its `org_events` audit row
/// commit together or not at all, via the same [`rows_txn::apply_and_emit`]
/// scaffold every other row port uses. The four policy ints are never touched
/// here.
///
/// # Errors
/// [`UNKNOWN_COMPANY`] `Refused` when the company has no `org_settings` row
/// yet (genesis has not run); SQL failures as [`ChiefdError::StoreFailure`].
pub fn publish_launcher_root(
    tx: &Transaction<'_>,
    slug: &str,
    at: &str,
    launcher_root: &str,
) -> Result<i64, ChiefdError> {
    apply_and_emit::<RowsSqlError, _>(tx, slug, at, "", |tx| {
        let touched = tx
            .execute(
                "UPDATE org_settings SET launcher_root = ?1 WHERE slug = ?2",
                params![launcher_root, slug],
            )
            .map_err(RowsSqlError::from)?;
        if touched == 0 {
            return Err(RowsSqlError(ChiefdError::refused(
                UNKNOWN_COMPANY,
                format!("no org_settings row for company '{slug}'"),
            )));
        }
        Ok(vec![EventTouch::new("org", slug, "upsert", "org_settings", slug)])
    })
    .map_err(|RowsSqlError(e)| e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    const SLUG: &str = "acme";

    /// `launcher_root` is added by the guarded ALTER
    /// [`crate::store::open_company_db`] runs (mirrors `event_once_markers`'s
    /// ACK columns), NOT by `COMPANY_SCHEMA_SQL`'s `CREATE TABLE` — so tests
    /// must open through the real entrypoint, on a temp-file database, rather
    /// than replay the base schema into an in-memory one.
    fn open() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("chief.db");
        let conn = crate::store::open_company_db(&path).expect("open company db");
        (dir, conn)
    }

    fn seed_org_settings(conn: &Connection, slug: &str, launcher_root: Option<&str>) {
        conn.execute(
            "INSERT INTO org_settings(slug, display_slug, launcher_root, \
             supervision_interval_ms, acknowledgement_timeout_ms, \
             acknowledgement_retry_limit, replacement_limit) \
             VALUES(?1, ?1, ?2, 900000, 60000, 3, 2)",
            params![slug, launcher_root],
        )
        .expect("seed org_settings");
    }

    #[test]
    fn read_is_none_before_any_genesis() {
        let (_dir, mut conn) = open();
        let tx = conn.transaction().unwrap();
        assert!(read(&tx, SLUG).unwrap().is_none());
    }

    #[test]
    fn publish_then_read_round_trips_launcher_root() {
        let (_dir, mut conn) = open();
        seed_org_settings(&conn, SLUG, None);
        let tx = conn.transaction().unwrap();
        let seq = publish_launcher_root(&tx, SLUG, "2026-08-04T00:00:00.000Z", "/checkouts/main")
            .unwrap();
        assert_eq!(seq, 1);
        let settings = read(&tx, SLUG).unwrap().unwrap();
        assert_eq!(settings.launcher_root.as_deref(), Some("/checkouts/main"));
    }

    #[test]
    fn publish_never_touches_the_four_policy_ints() {
        let (_dir, mut conn) = open();
        seed_org_settings(&conn, SLUG, None);
        let tx = conn.transaction().unwrap();
        let before = read(&tx, SLUG).unwrap().unwrap();
        publish_launcher_root(&tx, SLUG, "t", "/checkouts/main").unwrap();
        let after = read(&tx, SLUG).unwrap().unwrap();
        assert_eq!(before.supervision_interval_ms, after.supervision_interval_ms);
        assert_eq!(before.acknowledgement_timeout_ms, after.acknowledgement_timeout_ms);
        assert_eq!(before.acknowledgement_retry_limit, after.acknowledgement_retry_limit);
        assert_eq!(before.replacement_limit, after.replacement_limit);
    }

    #[test]
    fn publish_emits_exactly_one_org_settings_upsert_event() {
        let (_dir, mut conn) = open();
        seed_org_settings(&conn, SLUG, None);
        let tx = conn.transaction().unwrap();
        publish_launcher_root(&tx, SLUG, "t", "/checkouts/main").unwrap();
        let (entity, op, detail): (String, String, String) = tx
            .query_row(
                "SELECT entity, op, detail_ref FROM org_events WHERE slug=?1 AND seq=1",
                params![SLUG],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(entity, "org");
        assert_eq!(op, "upsert");
        assert_eq!(detail, format!("org_settings:{SLUG}/{SLUG}"));
    }

    #[test]
    fn publish_against_an_unknown_company_is_refused_not_a_silent_insert() {
        let (_dir, mut conn) = open();
        // No org_settings row seeded — genesis never ran for this slug.
        let tx = conn.transaction().unwrap();
        let err = publish_launcher_root(&tx, SLUG, "t", "/checkouts/main").unwrap_err();
        match err {
            ChiefdError::Refused(r) => assert_eq!(r.code, UNKNOWN_COMPANY),
            other => panic!("expected Refused(unknown-company), got {other:?}"),
        }
        assert!(read(&tx, SLUG).unwrap().is_none(), "no row must have been invented");
    }

    /// Mandate 4 / D19: a mid-operation failure — here, the `org_events`
    /// append that `apply_and_emit` performs AFTER the `launcher_root`
    /// `UPDATE` this function issues — must leave the database exactly as it
    /// was, because both writes share ONE `BEGIN IMMEDIATE` transaction.
    ///
    /// This is deliberately built to catch a non-atomic implementation: an
    /// engineer who wrote `publish_launcher_root` as two independently
    /// committed statements (`UPDATE org_settings; COMMIT; INSERT INTO
    /// org_events; COMMIT;`) would pass every test above but fail this one —
    /// the `UPDATE` would already be durable on disk before the injected
    /// `org_events` failure, leaving `launcher_root` changed with no audit
    /// row. Verified by hand: temporarily splitting the call into two
    /// `conn.execute` + `conn.execute("COMMIT")` pairs outside a shared
    /// transaction makes this test fail (`launcher_root` ends up
    /// "/checkouts/main" instead of the original value); the single-
    /// transaction implementation below makes it pass.
    #[test]
    fn a_failure_between_the_column_write_and_the_event_row_leaves_the_row_unchanged() {
        let (_dir, mut conn) = open();
        seed_org_settings(&conn, SLUG, Some("/checkouts/before"));

        let tx = conn.transaction().unwrap();
        // Force the failure `apply_and_emit` hits AFTER the `launcher_root`
        // `UPDATE` (inside the same `apply` closure) but BEFORE the
        // transaction commits: it drives the `org_events` append this
        // function relies on to record the change.
        tx.execute_batch("DROP TABLE org_events;").expect("drop org_events to inject the failure");

        let err = publish_launcher_root(&tx, SLUG, "t", "/checkouts/after")
            .expect_err("the org_events append must fail once its table is gone");
        assert!(matches!(err, ChiefdError::StoreFailure { .. }));

        // The whole transaction — UPDATE included — rolls back on drop,
        // exactly like `run_job` rolling back `CompanyDb::in_transaction`'s
        // step on an `Err`. Never commit a transaction that already failed.
        drop(tx);

        let tx = conn.transaction().unwrap();
        let settings = read(&tx, SLUG).unwrap().expect("row still exists");
        assert_eq!(
            settings.launcher_root.as_deref(),
            Some("/checkouts/before"),
            "the column write must not survive without its event row"
        );
    }
}
