//! A company IS a directory, and this module says where everything in it sits.
//!
//! `chiefd run --dir <dir>` is told ONE thing. Every path below hangs off it,
//! and nothing is configurable:
//!
//! ```text
//! <dir>/.chief/db/chief.db     the store — every durable company fact
//! <dir>/.chief/keys            the daemon's operator/service identity keys
//! <dir>/.chief/log             the chiefd-log jsonl sinks
//! <dir>/.chief/run             DISPOSABLE runtime: daemon.json, rail.sock
//! ```
//!
//! This is the same layout `chief_cli::paths` states for the client half, and
//! it replaces `<orgs-root>/.<slug>.chief.db` — a slug-keyed dotfile beside a
//! company directory, under a `--data-root` whose name meant the ORGS root one
//! directory down.
//!
//! # The identity is the DIRECTORY, not the slug
//!
//! One slug under two data roots was two companies, so the wire identity had
//! to be the composite `slug@sha256(orgs_root)[..12]` and every program had to
//! rebuild it the same way. A directory needs no composite: it IS the
//! identity, and [`company_key`] hashes it once. That key is this company's
//! label on the writer actor, its `org_documents` scope, and the `key` field
//! of the rendezvous this daemon publishes.
//!
//! The SLUG survives as a display name only — never argv, never a path
//! segment, and never something this daemon has an opinion about before
//! genesis has committed one.
//!
//! # THE SLUG IS NOT DURABLE YET, AND STAGE 2 IS BLOCKED ON THAT
//!
//! [`display_slug`] reads it from the store, which is where it belongs. Today
//! the store does not hold it: `chiefd_core::store::organization_rows::
//! reconstruct` DERIVES `manifest.slug` from `CompanyDb::label()` (stripping a
//! `@…` suffix) rather than reading a column, because the label used to BE
//! `<slug>@<rootHash>` and carried the name in its own prefix. A directory
//! hash carries nothing, so with the label re-keyed the reconstructed manifest
//! reads back named after its own key — and the activity/supervision row
//! projections, which refuse any ledger whose `organization` disagrees with
//! that derived value, refuse the genesis transaction outright.
//!
//! `chiefd-core` has to store the slug (an `org_settings` column, written by
//! the manifest publish and read by `reconstruct`) before a company can be
//! called anything but its own key. Nothing in THIS crate can close it.

use std::path::{Path, PathBuf};

use chiefd_core::actor::{CompanyDb, OpenError};
use chiefd_core::clock::SharedClock;
pub(crate) use host_primitives::rendezvous::company_key;

/// Everything chief owns inside a company's directory, `<dir>/.chief`.
///
/// `<dir>` itself and `<dir>/.pi/` are the user's and Pi's; nothing here ever
/// writes outside this one folder, which is what makes `rm -rf <dir>` a
/// complete removal.
#[must_use]
pub(crate) fn chief_dir(dir: &Path) -> PathBuf {
    dir.join(".chief")
}

/// The company's SQLite store, `<dir>/.chief/db/chief.db`.
///
/// Unconditional and slugless. The dotfile-sibling shape this replaces existed
/// so `<orgs>/<slug>/` could be deleted independently of the database beside
/// it; a directory holds exactly one company, so there is no sibling to keep
/// apart and nothing caller-supplied reaching a path join.
#[must_use]
pub(crate) fn store_db_path(dir: &Path) -> PathBuf {
    chief_dir(dir).join("db").join("chief.db")
}

/// The daemon's own identity keys, `<dir>/.chief/keys`.
///
/// Named through [`identity_keys::keys_dir`] rather than by joining `"keys"`
/// here: the directory name is that crate's constant, and the daemon and the
/// operator client must not each spell it.
#[must_use]
pub(crate) fn keys_dir(dir: &Path) -> PathBuf {
    identity_keys::keys_dir(&chief_dir(dir))
}

// `<dir>/.chief/run` — DISPOSABLE runtime state — is deliberately NOT an
// accessor here. `host_primitives::rendezvous::rendezvous_path` already names
// it, because the client reads the file this daemon writes there and the two
// may not each spell the path. A second builder in this module would be the
// second answer.
//
// `<dir>/.chief/log` is not here either, and that one is a GAP rather than a
// choice: `chiefd_log::install` resolves its sink root from the environment
// (`SinkLayer::from_env`) and cannot be told a directory, so this daemon's own
// jsonl still lands under `$HOME`. See this crate's report — `chiefd-log` has
// to take the directory before the last file leaves the operator's home.

// The company key, `sha256(canonical <dir>)[..12]`, is NOT defined here. It is
// `host_primitives::rendezvous::company_key` — the one crate this daemon and
// `chief-cli` may both link, which is what stops the ten-copy drift its
// composite predecessor suffered. This module imports it and re-exports
// nothing; every caller in this crate spells the same path.

/// The company's DISPLAY name, read from the store, or `None` before genesis
/// has committed one.
///
/// The daemon holds no opinion about the slug. It is told a directory, boots,
/// and serves the genesis that names the company — so a slug it could state
/// before that write landed would be a guess, and a slug it cached at boot
/// would be a guess that outlived its own correction. Read fresh, every time,
/// or not at all.
///
/// **It answers with the company KEY until `chiefd-core` stores the slug** —
/// see this module's header. The call is right and the store is not, so this
/// stays as written and the log line it feeds makes the gap visible on every
/// boot rather than hiding it behind a value nobody prints.
#[must_use]
pub(crate) fn display_slug(company: &CompanyDb) -> Option<String> {
    company.read(|snapshot| {
        chiefd_core::store::organization::read(snapshot.ledgers())
            .ok()
            .map(|manifest| manifest.slug)
    })
}

/// Open this directory's store, creating the directories SQLite cannot.
///
/// The label is [`company_key`]: one identity, derived from the one input this
/// daemon was given.
///
/// # Errors
/// Whatever [`CompanyDb::open`] refuses. `create_dir_all` is best-effort — a
/// real failure (permissions, a path component that is a file) surfaces one
/// line later as a SQLite open error naming the file, so a second error path
/// here would only report the same fact twice.
pub(crate) fn open(dir: &Path, clock: SharedClock) -> Result<CompanyDb, OpenError> {
    let path = store_db_path(dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    CompanyDb::open(&company_key(dir), &path, clock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// EVERY byte this daemon writes for a company hangs off `<dir>/.chief`.
    ///
    /// The claim the whole move rests on, asserted as containment rather than
    /// as four path strings: a future accessor that escaped the folder would
    /// fail this test rather than merely disagree with a literal.
    #[test]
    fn every_daemon_owned_path_hangs_off_the_directorys_own_chief_folder() {
        let dir = Path::new("/work/anvils");
        let chief = chief_dir(dir);
        for path in
            [store_db_path(dir), keys_dir(dir), host_primitives::rendezvous::rendezvous_path(dir)]
        {
            assert!(
                path.starts_with(&chief),
                "{} escapes the company's own .chief folder",
                path.display()
            );
        }
        assert_eq!(store_db_path(dir), PathBuf::from("/work/anvils/.chief/db/chief.db"));
        assert_eq!(keys_dir(dir), PathBuf::from("/work/anvils/.chief/keys"));
    }

    /// THE STORE IS INSIDE THE DIRECTORY IT DESCRIBES.
    ///
    /// The retired layout put it deliberately OUTSIDE, as `<orgs>/.<slug>
    /// .chief.db`, so that `rm -rf <orgs>/<slug>/` could not destroy the
    /// database before beacond's row. That reason is gone with the tree it
    /// protected: removing a company is now `rm -rf <dir>/.chief`, one act,
    /// and a store that survived it would be the residue.
    #[test]
    fn the_store_lives_inside_the_company_directory_rather_than_beside_it() {
        let dir = Path::new("/work/anvils");
        assert!(store_db_path(dir).starts_with(dir), "the store is the directory's own file");
        assert_eq!(store_db_path(dir).parent(), Some(chief_dir(dir).join("db").as_path()));
    }

    /// TWO DIRECTORIES MAY HOLD COMPANIES WITH THE SAME NAME.
    ///
    /// The case the retired slug-keyed layout could not represent at all, and
    /// the reason the identity is the directory.
    #[test]
    fn the_company_key_separates_two_directories_and_is_stable_for_one() {
        let first = company_key(Path::new("/work/acme"));
        let second = company_key(Path::new("/elsewhere/acme"));
        assert_ne!(first, second, "same name, different directories, different companies");
        assert_eq!(first, company_key(Path::new("/work/acme")), "and it is a pure function");
        assert_eq!(first.len(), 12, "twelve hex characters");
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()), "hex only: {first}");
    }

    /// The label the writer actor carries IS the company key — one identity,
    /// not a bare slug beside a composite one.
    #[test]
    fn opening_a_directory_labels_the_writer_actor_with_the_company_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(chiefd_core::clock::SystemClock::default());
        let company = open(dir.path(), clock).expect("open the store");
        assert_eq!(company.label(), company_key(dir.path()));
        assert!(store_db_path(dir.path()).is_file(), "the store file is created inside .chief/db");
        drop(company);
    }

    /// A company with no manifest has NO slug, and the daemon says so rather
    /// than inventing one. This is the ordinary state of the create flow: the
    /// client starts the daemon and then posts genesis THROUGH it.
    #[test]
    fn a_company_that_genesis_has_not_named_yet_has_no_display_slug() {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: SharedClock = Arc::new(chiefd_core::clock::SystemClock::default());
        let company = open(dir.path(), clock).expect("open the store");
        assert_eq!(display_slug(&company), None);
        drop(company);
    }

    /// D19 schema-application atomicity, carried over unchanged in substance:
    /// every statement in `COMPANY_SCHEMA_SQL` is `CREATE TABLE IF NOT
    /// EXISTS`, so a boot that dies partway through applying it must reopen to
    /// a COMPLETE schema. Simulated directly — apply only a prefix (proving
    /// the crash is genuinely partial, not vacuous), close, then reopen
    /// through the real production path.
    #[test]
    fn a_boot_that_dies_mid_schema_application_reopens_to_a_complete_schema() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = store_db_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("db parent")).expect("create .chief/db");

        let split_point = chiefd_core::schema::COMPANY_SCHEMA_SQL
            .find("CREATE TABLE IF NOT EXISTS identities")
            .expect("fixture: COMPANY_SCHEMA_SQL must still declare `identities` after `counters`");
        let partial_schema = &chiefd_core::schema::COMPANY_SCHEMA_SQL[..split_point];

        {
            #[allow(clippy::disallowed_methods)]
            let conn =
                rusqlite::Connection::open(&path).expect("open the fixture connection directly");
            conn.execute_batch(partial_schema).expect("apply the partial schema prefix");

            let has_counters: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='counters'",
                    [],
                    |row| row.get(0),
                )
                .expect("query sqlite_master");
            let has_supervisor_watermarks: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='supervisor_watermarks'",
                    [],
                    |row| row.get(0),
                )
                .expect("query sqlite_master");
            assert!(has_counters, "the simulated partial application must have created `counters`");
            assert!(
                !has_supervisor_watermarks,
                "the simulated partial application must NOT have reached `supervisor_watermarks` — \
                 otherwise this test proves nothing (it would pass even against a fully-applied schema)"
            );
        } // fixture connection dropped/closed here

        let clock: SharedClock = Arc::new(chiefd_core::clock::SystemClock::default());
        let company = open(dir.path(), clock).expect(
            "re-opening a partially-schema'd file must succeed, not refuse or corrupt it further",
        );
        drop(company); // quiesce the writer thread before reading independently

        let readonly = chiefd_core::store::open_company_db_readonly(&path)
            .expect("reopen read-only to inspect the completed schema");
        let has_supervisor_watermarks_now: bool = readonly
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='supervisor_watermarks'",
                [],
                |row| row.get(0),
            )
            .expect("query sqlite_master");
        assert!(
            has_supervisor_watermarks_now,
            "the second (real) application must have completed the schema — `supervisor_watermarks` \
             (the LAST table COMPANY_SCHEMA_SQL declares) must now exist. A crash during first \
             open must not leave a half-built database that a later open silently accepts as done."
        );
    }
}
