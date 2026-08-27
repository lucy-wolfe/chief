//! beacond's SQLite store: the ONE sanctioned exception to "all state lives
//! in some company's chiefd" (rulings D3/D21).
//!
//! Why the exception exists, and why it is bounded. The list of companies
//! cannot live inside the companies — a table inside the database you are
//! trying to *locate* cannot tell you where it is, or that it exists at all
//! (the bootstrap paradox). So `~/.chief/beacond.sqlite` is state outside
//! every company's chiefd, granted by the architect's rulings D3/D21. The
//! bound is exactly this file: ONE table, `companies`, holding a company's
//! `dir`, `key`, `slug`, `registered_at`, and where its daemon currently is
//! (`url`/`port`/`pid`/`hostname`/`last_seen_at`). Nothing about a company's
//! *content* — no people, goals, tasks, settings, supervision, runtime.
//!
//! # The primary key is the DIRECTORY
//!
//! It was the slug, and a slug was never unique: one slug under two data
//! roots was two companies, which is why the wire identity had to become the
//! composite `slug@sha256(orgs_root)[..12]`. A company is the directory the
//! operator ran `chief` in, and a directory is unique by construction — so
//! `dir` is the primary key, `key` is the `sha256(dir)[..12]` the caller
//! minted, and `slug` is a DISPLAY column with no uniqueness at all. Two
//! directories holding companies with the same name is an ordinary, listable
//! state here; under the old key it could not be written down.
//!
//! Nothing in this file finds a company's own daemon any more. A command
//! standing in a directory reads `<dir>/.chief/run/daemon.json`
//! (`host_primitives::rendezvous`); this table answers the one question no
//! single directory can — what is running anywhere on this box — for
//! `chief ls` and `apps/web`.
//!
//! `Registry` owns exactly one `rusqlite::Connection` behind a `Mutex`. That
//! `Mutex` is in-process ownership of a single handle, not a lock protocol:
//! no caller can observe it, it has no timeout and no way to ask whether it
//! is free — the same "one writer owns the connection" shape as
//! `chiefd_core::actor::writer`. beacond is single-writer over HTTP (one
//! process, one connection, every mutation a request that one writer
//! applies), so the registry file is never opened by a second process: there
//! is no `SQLITE_BUSY` for a caller to receive and so no SQLite pragma
//! governing a busy wait — that would be a wait hidden inside the database,
//! and beacond has nothing to wait for.
//!
//! Every mutation is ONE transaction (ruling D19). `create` and `delete` are
//! single-statement. `register`, `heartbeat` and `deregister` are
//! read-then-write: the admission decision and the pid fence need the
//! previous row, so that read lives INSIDE the same immediate transaction
//! as the write. Splitting either into a bare `SELECT` followed by a
//! bare `UPDATE` is the exact interleaving the fences exist to prevent.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::liveness::pid_is_live;
use crate::wire::{Company, Location};

/// A registry mutation or read failed.
///
/// There is deliberately no "already taken" variant. Its predecessor,
/// `SlugTaken`, refused a slug that existed under a different orgs root —
/// a collision the directory key makes impossible, because the colliding
/// pair is now two rows with two directories and one display word.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// `register`/`heartbeat` named a directory with no row at all. A daemon
    /// may not bring a company into existence by binding.
    #[error("unknown company")]
    UnknownCompany,
    /// `heartbeat`/`deregister` named a pid that does not match the one
    /// currently recorded — a stale caller, fenced off from a live daemon's
    /// location.
    #[error("pid mismatch: recorded pid is {recorded_pid}")]
    PidMismatch {
        /// The pid actually on record.
        recorded_pid: i64,
    },
    /// The underlying SQLite operation failed.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// The registry's parent directory could not be created.
    #[error("could not create the registry's parent directory: {0}")]
    Io(#[from] std::io::Error),
}

/// What `register` decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// The location columns now name the caller: it owns this company.
    Admitted(Company),
    /// A DIFFERENT, verifiably live pid holds the location. The caller must
    /// log this and exit; nothing was changed.
    Occupied {
        /// The incumbent's pid.
        pid: i64,
        /// The incumbent's recorded host, if any.
        hostname: Option<String>,
        /// The incumbent's last heartbeat, if any.
        last_seen_at: Option<String>,
    },
}

const PRAGMAS: &[(&str, &str)] = &[("journal_mode", "WAL"), ("synchronous", "FULL")];

const CREATE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS companies(
    dir           TEXT PRIMARY KEY,
    key           TEXT NOT NULL,
    slug          TEXT NOT NULL,
    registered_at TEXT NOT NULL,
    url           TEXT,
    port          INTEGER,
    pid           INTEGER,
    hostname      TEXT,
    last_seen_at  TEXT
)";

/// Every column, in declaration order — the one `SELECT` list the five reads
/// below share. It was written out five times and had to be edited five times
/// to add a column, which is how a read forgets a field.
const COMPANY_COLUMNS: &str =
    "dir, key, slug, registered_at, url, port, pid, hostname, last_seen_at";

/// The company registry: one SQLite connection, one table.
pub struct Registry {
    connection: Mutex<Connection>,
}

fn row_to_company(row: &rusqlite::Row<'_>) -> rusqlite::Result<Company> {
    Ok(Company {
        dir: row.get("dir")?,
        key: row.get("key")?,
        slug: row.get("slug")?,
        registered_at: row.get("registered_at")?,
        url: row.get("url")?,
        port: row.get::<_, Option<i64>>("port")?.map(|p| p as u16),
        pid: row.get("pid")?,
        hostname: row.get("hostname")?,
        last_seen_at: row.get("last_seen_at")?,
    })
}

impl Registry {
    /// Open (creating if absent) the registry at `path`. Creates the parent
    /// directory chain — SQLite will not.
    ///
    /// # Errors
    /// [`RegistryError::Io`] if the parent directory cannot be created;
    /// [`RegistryError::Sqlite`] if the file cannot be opened or the schema
    /// cannot be applied.
    pub fn open(path: &Path) -> Result<Self, RegistryError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        // beacond is the second sanctioned owner of a raw SQLite connection
        // (the first is `chiefd_core::store`, for a company's own database):
        // the seam clippy.toml defends is "no OTHER code opens a company
        // connection behind the writer actor's back", and beacond's registry
        // is not a company database at all — it is the ruling-D3/D21
        // exception this module's doc explains. Narrow and commented, per
        // clippy.toml's own convention for a legitimate exception.
        #[allow(clippy::disallowed_methods)]
        let connection = Connection::open(path)?;
        // `pragma_update` (rather than `execute`/`query_row`) is the correct
        // rusqlite call for a pragma setter: `journal_mode` returns the
        // resulting mode as a row while `synchronous` returns nothing, and
        // `pragma_update` handles both shapes uniformly.
        for (name, value) in PRAGMAS {
            connection.pragma_update(None, name, value)?;
        }
        connection.execute(CREATE_TABLE_SQL, [])?;
        Ok(Self { connection: Mutex::new(connection) })
    }

    /// Whether the schema has been established (used by `GET /v1/health`).
    ///
    /// # Errors
    /// [`RegistryError::Sqlite`] if the check itself fails.
    pub fn schema_ready(&self) -> Result<bool, RegistryError> {
        let connection = self.connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='companies')",
            [],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    /// One atomic upsert INSERT (ruling D24/F27): a single
    /// `INSERT … ON CONFLICT(dir) DO UPDATE … RETURNING` statement.
    /// `PRIMARY KEY(dir)` decides a duplicate inside the same statement that
    /// attempts it — no read, no pre-check, no separate transaction.
    ///
    /// **It cannot be refused.** The predecessor could: it compared the
    /// caller's `orgs_root` against the row's and rejected a mismatch,
    /// because one slug under two roots was two companies fighting over one
    /// key. A directory holds exactly one company, so there is no second
    /// claimant to arbitrate between and nothing to refuse. What the conflict
    /// arm does instead is follow the DISPLAY word: re-creating a directory
    /// under a new slug renames it, which is the only thing a caller can
    /// legitimately have changed. `registered_at` is never touched — it is
    /// the company's birthday.
    ///
    /// Returns the company row and whether THIS call is the one that created
    /// it, decided by SQLite's own `last_insert_rowid()`: it advances only
    /// for a genuine `INSERT`, never for the `ON CONFLICT DO UPDATE` arm
    /// (verified empirically), so comparing it before and after the
    /// statement distinguishes "inserted" from "matched an existing row"
    /// exactly — unlike comparing the returned `registered_at` against `now`,
    /// which is wrong whenever two calls land in the same millisecond (a
    /// real, measured false positive under fast back-to-back calls, not a
    /// hypothetical).
    ///
    /// # Errors
    /// [`RegistryError::Sqlite`] if the statement itself fails.
    pub fn create(
        &self,
        dir: &str,
        key: &str,
        slug: &str,
        now: &str,
    ) -> Result<(Company, bool), RegistryError> {
        let connection = self.connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        connection.execute("BEGIN IMMEDIATE", [])?;
        let rowid_before = connection.last_insert_rowid();
        let result = connection.query_row(
            &format!(
                "INSERT INTO companies(dir, key, slug, registered_at) VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(dir) DO UPDATE SET key = excluded.key, slug = excluded.slug
                 RETURNING {COMPANY_COLUMNS}"
            ),
            rusqlite::params![dir, key, slug, now],
            row_to_company,
        );

        let outcome = match result {
            Ok(company) => {
                let created = connection.last_insert_rowid() != rowid_before;
                Ok((company, created))
            }
            Err(error) => Err(RegistryError::Sqlite(error)),
        };

        match &outcome {
            Ok(_) => connection.execute("COMMIT", [])?,
            Err(_) => connection.execute("ROLLBACK", [])?,
        };
        outcome
    }

    /// One atomic DELETE, idempotent. Deletes regardless of the location
    /// columns — the caller has already stopped the daemon.
    ///
    /// # Errors
    /// [`RegistryError::Sqlite`] if the delete itself fails.
    pub fn delete(&self, dir: &str) -> Result<bool, RegistryError> {
        let connection = self.connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        connection.execute("BEGIN IMMEDIATE", [])?;
        let outcome = connection
            .execute("DELETE FROM companies WHERE dir = ?1", [dir])
            .map(|affected| affected > 0)
            .map_err(RegistryError::Sqlite);

        match &outcome {
            Ok(_) => connection.execute("COMMIT", [])?,
            Err(_) => connection.execute("ROLLBACK", [])?,
        };
        outcome
    }

    /// The admission arbiter. ONE conditional UPDATE of the location columns
    /// of an EXISTING row, inside one transaction.
    ///
    /// # Errors
    /// [`RegistryError::UnknownCompany`] if no row exists for `location.dir`.
    pub fn register(&self, location: &Location) -> Result<Admission, RegistryError> {
        // The registry is one connection behind one mutex, and admission is
        // the call every company daemon makes at boot. A slow lock here is a
        // slow launch there, and only this side can see it: the waiting client
        // observes an absent registration and cannot tell a daemon that has
        // not asked yet from one queued behind a write.
        let waiting_since = std::time::Instant::now();
        let connection = self.connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let lock_ms = chiefd_log::elapsed_ms(waiting_since);
        connection.execute("BEGIN IMMEDIATE", [])?;
        tracing::debug!(
            event = "beacond.register.transaction",
            company = %location.dir,
            lock_ms,
            "took the registry write lock"
        );
        let outcome = (|| -> Result<Admission, RegistryError> {
            let existing: Option<(Option<i64>, Option<String>, Option<String>)> = connection
                .query_row(
                    "SELECT pid, hostname, last_seen_at FROM companies WHERE dir = ?1",
                    [&location.dir],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .ok();

            let Some((recorded_pid, recorded_hostname, recorded_last_seen)) = existing else {
                return Err(RegistryError::UnknownCompany);
            };

            // vacant (no pid), the caller's own pid, or a verifiably dead pid
            // all admit (a crash-honest takeover); a different LIVE pid
            // occupies and nothing is written.
            match recorded_pid {
                Some(pid) if pid != location.pid && pid_is_live(pid) => {
                    return Ok(Admission::Occupied {
                        pid,
                        hostname: recorded_hostname,
                        last_seen_at: recorded_last_seen,
                    });
                }
                _ => {}
            }

            connection.execute(
                "UPDATE companies SET url = ?1, port = ?2, pid = ?3, hostname = ?4, last_seen_at = ?5 \
                 WHERE dir = ?6",
                rusqlite::params![
                    location.url,
                    location.port,
                    location.pid,
                    location.hostname,
                    crate::wire::now_iso_millis(),
                    location.dir,
                ],
            )?;
            let company = connection.query_row(
                &format!("SELECT {COMPANY_COLUMNS} FROM companies WHERE dir = ?1"),
                [&location.dir],
                row_to_company,
            )?;
            Ok(Admission::Admitted(company))
        })();

        match &outcome {
            Ok(_) => connection.execute("COMMIT", [])?,
            Err(_) => connection.execute("ROLLBACK", [])?,
        };
        outcome
    }

    /// Refresh `last_seen_at` only, pid-fenced.
    ///
    /// # Errors
    /// [`RegistryError::PidMismatch`] if a different process holds the
    /// location.
    pub fn heartbeat(
        &self,
        dir: &str,
        pid: i64,
        now: &str,
    ) -> Result<Option<Company>, RegistryError> {
        let connection = self.connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        connection.execute("BEGIN IMMEDIATE", [])?;
        let outcome = (|| -> Result<Option<Company>, RegistryError> {
            let recorded_pid: Option<Option<i64>> = connection
                .query_row("SELECT pid FROM companies WHERE dir = ?1", [dir], |row| row.get(0))
                .ok();

            let Some(recorded_pid) = recorded_pid else {
                return Ok(None);
            };

            if recorded_pid != Some(pid) {
                return Err(RegistryError::PidMismatch { recorded_pid: recorded_pid.unwrap_or(0) });
            }

            connection.execute(
                "UPDATE companies SET last_seen_at = ?1 WHERE dir = ?2",
                rusqlite::params![now, dir],
            )?;
            let company = connection.query_row(
                &format!("SELECT {COMPANY_COLUMNS} FROM companies WHERE dir = ?1"),
                [dir],
                row_to_company,
            )?;
            Ok(Some(company))
        })();

        match &outcome {
            Ok(_) => connection.execute("COMMIT", [])?,
            Err(_) => connection.execute("ROLLBACK", [])?,
        };
        outcome
    }

    /// CLEAR the location columns. **The row survives** — stopping a daemon
    /// is not deleting a company.
    ///
    /// # Errors
    /// [`RegistryError::PidMismatch`] if a different process holds the
    /// location.
    pub fn deregister(&self, dir: &str, pid: i64) -> Result<bool, RegistryError> {
        let connection = self.connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        connection.execute("BEGIN IMMEDIATE", [])?;
        let outcome = (|| -> Result<bool, RegistryError> {
            let recorded_pid: Option<Option<i64>> = connection
                .query_row("SELECT pid FROM companies WHERE dir = ?1", [dir], |row| row.get(0))
                .ok();

            let Some(recorded_pid) = recorded_pid else {
                // No row at all: nothing to clear, idempotent success.
                return Ok(false);
            };

            let Some(recorded_pid_value) = recorded_pid else {
                // Row exists but already vacant: nothing to clear.
                return Ok(false);
            };

            if recorded_pid_value != pid {
                return Err(RegistryError::PidMismatch { recorded_pid: recorded_pid_value });
            }

            connection.execute(
                "UPDATE companies SET url = NULL, port = NULL, pid = NULL, hostname = NULL, \
                 last_seen_at = NULL WHERE dir = ?1",
                [dir],
            )?;
            Ok(true)
        })();

        match &outcome {
            Ok(_) => connection.execute("COMMIT", [])?,
            Err(_) => connection.execute("ROLLBACK", [])?,
        };
        outcome
    }

    /// One company, by the directory it occupies.
    ///
    /// # Errors
    /// [`RegistryError::Sqlite`] if the read fails.
    pub fn lookup(&self, dir: &str) -> Result<Option<Company>, RegistryError> {
        let connection = self.connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let company = connection
            .query_row(
                &format!("SELECT {COMPANY_COLUMNS} FROM companies WHERE dir = ?1"),
                [dir],
                row_to_company,
            )
            .ok();
        Ok(company)
    }

    /// Every company on this box, ordered by display name and then by
    /// directory — the slug alone is not a total order, because two
    /// directories may carry the same one.
    ///
    /// # Errors
    /// [`RegistryError::Sqlite`] if the read fails.
    pub fn list(&self) -> Result<Vec<Company>, RegistryError> {
        let connection = self.connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut statement = connection
            .prepare(&format!("SELECT {COMPANY_COLUMNS} FROM companies ORDER BY slug, dir"))?;
        let companies =
            statement.query_map([], row_to_company)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(companies)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory and the key its caller minted for it. The key's real value
    /// is `sha256(dir)[..12]`, computed by the client; these are stand-ins of
    /// the right SHAPE, because beacond records a key and never derives one.
    const ANVILS: (&str, &str) = ("/work/anvils", "0123456789ab");
    const FORGE: (&str, &str) = ("/work/forge", "cafebabe0011");

    fn open_temp() -> (tempfile::TempDir, Registry) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("beacond.sqlite");
        let registry = Registry::open(&path).expect("open registry");
        (dir, registry)
    }

    fn location(dir: &str, pid: i64) -> Location {
        Location {
            dir: dir.to_string(),
            url: "http://127.0.0.1:8794".to_string(),
            port: 8794,
            pid,
            hostname: "box".to_string(),
        }
    }

    // ---- test 3: open is idempotent ----

    #[test]
    fn a_second_open_on_the_same_file_is_a_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("beacond.sqlite");
        let first = Registry::open(&path).expect("first open");
        first.create(ANVILS.0, ANVILS.1, "anvils", "t0").expect("create");
        drop(first);

        let second = Registry::open(&path).expect("second open");
        assert_eq!(second.list().expect("list").len(), 1);
    }

    // ---- THE KEY: the directory is the identity, the slug is a word ----

    /// **TWO DIRECTORIES MAY HOLD COMPANIES WITH THE SAME NAME.**
    ///
    /// The case the slug-keyed registry could not represent at all: the
    /// second `create` overwrote the first company's row, so one of the two
    /// directories silently lost its registration and every later lookup
    /// answered with the other one's daemon. Both rows exist here, both keep
    /// their own key, and both keep their own location.
    #[test]
    fn two_directories_hold_two_companies_even_when_they_share_a_slug() {
        let (_dir, registry) = open_temp();
        let (first, created_first) =
            registry.create(ANVILS.0, ANVILS.1, "acme", "t0").expect("create the first acme");
        let (second, created_second) =
            registry.create(FORGE.0, FORGE.1, "acme", "t1").expect("create the second acme");
        assert!(created_first && created_second, "both directories are new companies");
        assert_eq!(first.slug, second.slug, "the display word is genuinely the same");
        assert_ne!(first.dir, second.dir);
        assert_ne!(first.key, second.key);

        assert_eq!(registry.list().expect("list").len(), 2, "two rows, not one overwritten row");

        // And each keeps its OWN daemon: a location published for one
        // directory must not appear against the other.
        registry.register(&location(ANVILS.0, 111)).expect("register anvils");
        registry.register(&location(FORGE.0, 222)).expect("register forge");
        let anvils = registry.lookup(ANVILS.0).expect("lookup").expect("row exists");
        let forge = registry.lookup(FORGE.0).expect("lookup").expect("row exists");
        assert_eq!(anvils.pid, Some(111));
        assert_eq!(forge.pid, Some(222));
        assert_eq!(anvils.key, ANVILS.1);
        assert_eq!(forge.key, FORGE.1);
    }

    // ---- test 4: create is an upsert on the directory ----

    #[test]
    fn create_inserts_a_new_row_with_no_location() {
        let (_dir, registry) = open_temp();
        let (company, created) =
            registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        assert!(created);
        assert_eq!(company.dir, ANVILS.0);
        assert_eq!(company.key, ANVILS.1);
        assert_eq!(company.slug, "northstar");
        assert_eq!(company.registered_at, "t0");
        assert_eq!(company.url, None);
        assert_eq!(company.port, None);
        assert_eq!(company.pid, None);
        assert_eq!(company.hostname, None);
        assert_eq!(company.last_seen_at, None);
    }

    #[test]
    fn create_twice_on_one_directory_is_idempotent_and_preserves_the_first_timestamp() {
        let (_dir, registry) = open_temp();
        let (first, created_first) =
            registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        assert!(created_first);

        let (second, created_second) =
            registry.create(ANVILS.0, ANVILS.1, "northstar", "t1").expect("create again");
        assert!(!created_second);
        // THE assertion the re-runnable-create design rests on: registered_at
        // is still the FIRST create's timestamp, not the second call's.
        assert_eq!(second.registered_at, "t0");
        assert_eq!(second, first);
    }

    #[test]
    fn create_twice_with_the_same_timestamp_is_still_correctly_not_created() {
        // Regression: two calls landing in the same millisecond (routine
        // under fast back-to-back HTTP requests) must not be misread as
        // "created" just because the passed-in `now` happens to match the
        // row's `registered_at` by coincidence. The `created` flag is
        // decided by SQLite's own last_insert_rowid(), not by comparing
        // timestamps.
        let (_dir, registry) = open_temp();
        let (_first, created_first) =
            registry.create(ANVILS.0, ANVILS.1, "northstar", "same-instant").expect("create");
        assert!(created_first);

        let (_second, created_second) =
            registry.create(ANVILS.0, ANVILS.1, "northstar", "same-instant").expect("create again");
        assert!(
            !created_second,
            "an identical re-create must report created=false even with an identical timestamp"
        );
    }

    #[test]
    fn create_twice_after_a_register_preserves_the_location() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        registry.register(&location(ANVILS.0, 4242)).expect("register");

        let (company, created) =
            registry.create(ANVILS.0, ANVILS.1, "northstar", "t1").expect("re-create");
        assert!(!created);
        assert_eq!(company.registered_at, "t0");
        // Re-creating a running company must not unregister it.
        assert_eq!(company.pid, Some(4242));
        assert_eq!(company.url.as_deref(), Some("http://127.0.0.1:8794"));
    }

    /// A RENAME is a display change, not a conflict.
    ///
    /// Its predecessor refused this: a second create naming a different orgs
    /// root was `SlugTaken`, because two roots under one slug were two
    /// companies fighting over one key. A directory has exactly one company,
    /// so the only thing a re-create can have changed is the word the
    /// operator calls it — and the row follows it.
    #[test]
    fn re_creating_a_directory_under_a_new_slug_renames_it_and_keeps_its_birthday() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");

        let (renamed, created) =
            registry.create(ANVILS.0, ANVILS.1, "polaris", "t1").expect("rename");
        assert!(!created, "a rename is not a new company");
        assert_eq!(renamed.slug, "polaris");
        assert_eq!(renamed.registered_at, "t0");
        assert_eq!(registry.list().expect("list").len(), 1, "and it is still one row");
    }

    // ---- test 5: delete is idempotent ----

    #[test]
    fn delete_removes_an_existing_company_and_is_idempotent() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");

        assert!(registry.delete(ANVILS.0).expect("delete"));
        assert_eq!(registry.lookup(ANVILS.0).expect("lookup"), None);

        assert!(!registry.delete(ANVILS.0).expect("second delete"));
    }

    #[test]
    fn delete_succeeds_even_when_a_location_is_filled() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        registry.register(&location(ANVILS.0, 4242)).expect("register");

        assert!(registry.delete(ANVILS.0).expect("delete"));
        assert_eq!(registry.lookup(ANVILS.0).expect("lookup"), None);
    }

    // ---- test 6/7: register is admission ----

    #[test]
    fn register_on_an_unknown_directory_is_refused_and_creates_no_row() {
        let (_dir, registry) = open_temp();
        let result = registry.register(&location("/work/ghost", 1));
        assert!(matches!(result, Err(RegistryError::UnknownCompany)));
        assert_eq!(registry.list().expect("list"), Vec::new());
    }

    #[test]
    fn register_on_a_vacant_company_admits_and_fills_every_location_column() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");

        let admission = registry.register(&location(ANVILS.0, 4242)).expect("register");
        let Admission::Admitted(company) = admission else {
            panic!("expected Admitted");
        };
        assert_eq!(company.pid, Some(4242));
        assert_eq!(company.url.as_deref(), Some("http://127.0.0.1:8794"));
        assert_eq!(company.hostname.as_deref(), Some("box"));
        assert!(company.last_seen_at.is_some());
        // registered_at is the company's birthday, never touched by register.
        assert_eq!(company.registered_at, "t0");
        // and neither is any identity column.
        assert_eq!(company.dir, ANVILS.0);
        assert_eq!(company.key, ANVILS.1);
        assert_eq!(company.slug, "northstar");
    }

    #[test]
    fn register_with_the_same_pid_re_admits() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        registry.register(&location(ANVILS.0, 4242)).expect("first register");

        let admission = registry.register(&location(ANVILS.0, 4242)).expect("re-register");
        assert!(matches!(admission, Admission::Admitted(_)));
    }

    #[test]
    fn register_takes_over_a_dead_pid() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");

        // A pid that is certainly not alive: spawn-and-reap.
        let mut child = std::process::Command::new("true").spawn().expect("spawn");
        let dead_pid = i64::from(child.id());
        child.wait().expect("wait");

        registry
            .register(&Location {
                dir: ANVILS.0.to_string(),
                url: "http://127.0.0.1:9000".to_string(),
                port: 9000,
                pid: dead_pid,
                hostname: "box".to_string(),
            })
            .expect("first register (dead pid, but nobody held it yet)");

        let admission = registry.register(&location(ANVILS.0, 4242)).expect("takeover");
        let Admission::Admitted(company) = admission else {
            panic!("expected Admitted (takeover of a dead pid)");
        };
        assert_eq!(company.pid, Some(4242));
        assert_eq!(company.registered_at, "t0");
    }

    #[test]
    fn register_refuses_a_live_incumbent_and_changes_nothing() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        let me = i64::from(std::process::id());
        registry.register(&location(ANVILS.0, me)).expect("incumbent registers");
        let before = registry.lookup(ANVILS.0).expect("lookup").expect("row exists");

        let admission = registry
            .register(&location(ANVILS.0, me + 999_999))
            .expect("register call itself does not error");
        match admission {
            Admission::Occupied { pid, .. } => assert_eq!(pid, me),
            Admission::Admitted(_) => panic!("a live incumbent must not be evicted"),
        }

        let after = registry.lookup(ANVILS.0).expect("lookup").expect("row exists");
        assert_eq!(before, after, "a refused register must change nothing");
    }

    // ---- test 8: heartbeat ----

    #[test]
    fn heartbeat_on_an_unknown_directory_is_none() {
        let (_dir, registry) = open_temp();
        assert_eq!(registry.heartbeat("/work/ghost", 1, "t1").expect("heartbeat"), None);
    }

    #[test]
    fn heartbeat_with_a_mismatched_pid_is_refused() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        registry.register(&location(ANVILS.0, 4242)).expect("register");

        let result = registry.heartbeat(ANVILS.0, 9999, "t1");
        assert!(matches!(result, Err(RegistryError::PidMismatch { recorded_pid: 4242 })));
    }

    #[test]
    fn heartbeat_with_the_right_pid_only_changes_last_seen_at() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        let after_register = registry.register(&location(ANVILS.0, 4242)).expect("register");
        let Admission::Admitted(before) = after_register else {
            panic!("expected Admitted");
        };

        let company =
            registry.heartbeat(ANVILS.0, 4242, "t-later").expect("heartbeat").expect("row exists");
        assert_eq!(company.last_seen_at.as_deref(), Some("t-later"));
        assert_eq!(company.pid, before.pid);
        assert_eq!(company.url, before.url);
        assert_eq!(company.registered_at, before.registered_at);
    }

    // ---- test 9: deregister keeps the company (the story's own "single
    // most important assertion") ----

    #[test]
    fn deregister_clears_the_location_and_keeps_the_row() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        registry.register(&location(ANVILS.0, 4242)).expect("register");

        assert!(registry.deregister(ANVILS.0, 4242).expect("deregister"));

        let after = registry.lookup(ANVILS.0).expect("lookup").expect("row STILL exists");
        assert_eq!(after.dir, ANVILS.0);
        assert_eq!(after.key, ANVILS.1);
        assert_eq!(after.slug, "northstar");
        assert_eq!(after.registered_at, "t0");
        assert_eq!(after.url, None);
        assert_eq!(after.port, None);
        assert_eq!(after.pid, None);
        assert_eq!(after.hostname, None);
        assert_eq!(after.last_seen_at, None);
    }

    #[test]
    fn deregister_twice_is_idempotent() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        registry.register(&location(ANVILS.0, 4242)).expect("register");

        assert!(registry.deregister(ANVILS.0, 4242).expect("first deregister"));
        assert!(!registry.deregister(ANVILS.0, 4242).expect("second deregister"));
    }

    #[test]
    fn deregister_on_an_unknown_directory_is_false_not_an_error() {
        let (_dir, registry) = open_temp();
        assert!(!registry.deregister("/work/ghost", 1).expect("deregister unknown"));
    }

    #[test]
    fn deregister_with_a_mismatched_pid_is_refused_and_clears_nothing() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        registry.register(&location(ANVILS.0, 4242)).expect("register");

        let result = registry.deregister(ANVILS.0, 9999);
        assert!(matches!(result, Err(RegistryError::PidMismatch { recorded_pid: 4242 })));

        let after = registry.lookup(ANVILS.0).expect("lookup").expect("row exists");
        assert_eq!(after.pid, Some(4242), "a refused deregister must clear nothing");
    }

    #[test]
    fn deregister_leaves_other_directories_untouched() {
        let (_dir, registry) = open_temp();
        registry.create(ANVILS.0, ANVILS.1, "acme", "t0").expect("create acme");
        registry.create(FORGE.0, FORGE.1, "northstar", "t0").expect("create northstar");
        registry.register(&location(ANVILS.0, 111)).expect("register acme");
        registry.register(&location(FORGE.0, 222)).expect("register northstar");

        registry.deregister(ANVILS.0, 111).expect("deregister acme");

        let northstar = registry.lookup(FORGE.0).expect("lookup").expect("row exists");
        assert_eq!(northstar.pid, Some(222));
    }

    // ---- test 10: list ----

    #[test]
    fn list_is_empty_on_a_fresh_store() {
        let (_dir, registry) = open_temp();
        assert_eq!(registry.list().expect("list"), Vec::new());
    }

    #[test]
    fn list_is_ordered_by_slug_then_directory_and_includes_a_never_registered_company() {
        let (_dir, registry) = open_temp();
        registry.create("/work/zeta", "aaaaaaaaaaaa", "zeta", "t0").expect("create zeta");
        registry.create("/work/alpha", "bbbbbbbbbbbb", "alpha", "t0").expect("create alpha");
        // The same word as the first, so only the directory can break the tie.
        registry.create("/elsewhere/zeta", "cccccccccccc", "zeta", "t0").expect("create the twin");
        registry.register(&location("/work/zeta", 1)).expect("register zeta");
        // "alpha" is never registered — the D21 property this redesign exists for.

        let companies = registry.list().expect("list");
        let ordered: Vec<(&str, &str)> =
            companies.iter().map(|c| (c.slug.as_str(), c.dir.as_str())).collect();
        assert_eq!(
            ordered,
            vec![("alpha", "/work/alpha"), ("zeta", "/elsewhere/zeta"), ("zeta", "/work/zeta")]
        );
        assert_eq!(companies[0].pid, None);
    }

    // ---- test 14: the mandate-2 exception, asserted (rulings D3/D21) ----

    #[test]
    fn the_registry_owns_exactly_one_table_keyed_by_the_company_directory() {
        // The list of companies cannot live inside the companies, so this ONE
        // store is allowed to exist outside every company's chiefd — and
        // only at this size. The day someone adds a company-content column,
        // this test says no.
        let (_dir, registry) = open_temp();
        let connection =
            registry.connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

        let table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .expect("count tables");
        assert_eq!(table_count, 1);

        let table_name: String = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .expect("table name");
        assert_eq!(table_name, "companies");

        let mut statement = connection.prepare("PRAGMA table_info(companies)").expect("table_info");
        let columns: Vec<(String, i64, i64)> = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)?, row.get::<_, i64>(5)?))
            })
            .expect("query columns")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect columns");

        let names: Vec<&str> = columns.iter().map(|(name, _, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "dir",
                "key",
                "slug",
                "registered_at",
                "url",
                "port",
                "pid",
                "hostname",
                "last_seen_at"
            ]
        );

        // **THE PRIMARY KEY IS THE DIRECTORY** (`pk` is table_info's column
        // 5, a 1-based position within the key, 0 for a non-key column). It
        // used to be `slug`, and a slug that must be unique per box cannot
        // describe two directories holding companies with the same name.
        // `slug` reading 0 here is what makes that pair representable.
        let keys: Vec<(&str, i64)> =
            columns.iter().map(|(name, _, pk)| (name.as_str(), *pk)).collect();
        assert_eq!(
            keys,
            vec![
                ("dir", 1),
                ("key", 0),
                ("slug", 0),
                ("registered_at", 0),
                ("url", 0),
                ("port", 0),
                ("pid", 0),
                ("hostname", 0),
                ("last_seen_at", 0)
            ]
        );

        // notnull column (index 3 of table_info): `key`, `slug` and
        // `registered_at` are explicitly NOT NULL; `dir` is 0 because SQLite
        // does not imply NOT NULL from a non-INTEGER PRIMARY KEY alone — the
        // PRIMARY KEY constraint itself is the uniqueness guard. The five
        // location columns are nullable.
        let notnull: Vec<i64> = columns.iter().map(|(_, notnull, _)| *notnull).collect();
        assert_eq!(notnull, vec![0, 1, 1, 1, 0, 0, 0, 0, 0]);
    }

    // ---- test 15: D20 boot-and-inspect ----

    #[test]
    fn only_the_store_file_and_its_wal_shm_siblings_ever_exist_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("beacond.sqlite");
        let registry = Registry::open(&path).expect("open");

        registry.create(ANVILS.0, ANVILS.1, "northstar", "t0").expect("create");
        registry.register(&location(ANVILS.0, 4242)).expect("register");
        registry.heartbeat(ANVILS.0, 4242, "t1").expect("heartbeat");
        registry.deregister(ANVILS.0, 4242).expect("deregister");
        registry.delete(ANVILS.0).expect("delete");
        drop(registry);

        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        for entry in &entries {
            assert!(
                entry == "beacond.sqlite" || entry.starts_with("beacond.sqlite-"),
                "unexpected on-disk entry: {entry}"
            );
        }
    }

    // Tests 16/17 (D19 crash-mid-transaction) live in
    // ../tests/crash_mid_transaction.rs: they issue their OWN raw immediate-
    // transaction start to simulate a crashed connection, which would
    // otherwise inflate this file's count of that statement past the five
    // real ones the D19 acceptance grep counts. Keeping the crash-simulation
    // grep-noise out of this file is the whole reason they live in `tests/`
    // instead of here.
}
