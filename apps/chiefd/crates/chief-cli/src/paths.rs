//! Where a company lives, and where this box's binaries are installed.
//!
//! # The company is the DIRECTORY
//!
//! `cd somewhere && chief` makes that directory the company. Everything
//! durable about it lives under `<dir>/.chief/`, and the existence of
//! `<dir>/.chief/db/chief.db` is the entire first-run check — absent means
//! Founder mode, present means attach. There is no marker file, no company-id
//! record, no `~` data root and no global registry the CLI path needs: the
//! DB's own `organization` row carries the slug, the name and the purpose,
//! which is why the DB must live in the directory rather than beside it.
//!
//! This replaces `~/.chiefd/orgs/`, a global invisible tree behind a registry
//! keyed by slug. One slug under two roots was two companies, so the wire
//! identity had to be the composite `slug@sha256(orgs_root)[..12]` — recomputed
//! independently in nine places. A directory needs no composite: it IS the
//! identity, and [`company_key`] hashes it once.
//!
//! # What is left in `~`, and why
//!
//! `~/.chief/bin`, `~/.chief/versions` and `~/.chief/state` — INSTALL facts
//! about the box, not company data. A company must not carry its own copy of
//! "where is the chiefd binary", and an install must not live inside one
//! company's directory. There is no longer a `~/.chief/launcher-root`: see
//! [`resource_root`] for what replaced it and why it had to.
//!
//! # Ownership per path
//!
//! `<dir>` and `<dir>/.pi/` are the USER's (and Pi's). `.chief/db` and
//! `.chief/keys` are chiefd's, writer-actor only. `.chief/run`, `.chief/log`,
//! `.chief/logs` and `.chief/bus` are disposable — their owners may recreate
//! them.
//! `.chief/agent/<id>/` is written once at hire and never touched by chief
//! again.

use std::path::{Path, PathBuf};

/// The folder chief owns, inside a company directory and inside `$HOME` alike.
///
/// **ONE SPELLING, and it is not this crate's.** `chiefd-log` is the leaf both
/// this client and the daemon already link, so the name lives there and both
/// sides import it. That is not tidiness: `chiefd-daemon` read
/// `~/.chiefd/launcher-root` while this crate wrote `~/.chief/launcher-root`
/// for a while, and the consequence is silent by construction — a missing
/// launcher-root pointer is deliberately ABSENT rather than an error, so every
/// person materialized with an empty `extensions/` and the CEO came up with no
/// `org_*` tools while genesis still reported success. Two literals that agreed
/// yesterday are what a rename breaks; a shared constant is what makes the
/// divergence unspellable.
use chiefd_log::sink::CHIEF_DIR;

/// Everything chief owns inside a company's directory.
#[must_use]
pub(crate) fn chief_dir(dir: &Path) -> PathBuf {
    dir.join(CHIEF_DIR)
}

/// The company's SQLite store, `<dir>/.chief/db/chief.db`.
///
/// Unconditional and slugless — a directory holds exactly one company, so
/// there is nothing to key by and nothing to validate before joining. The old
/// `<orgsRoot>/.<slug>.chief.db` needed a canonical-slug guard precisely
/// because a caller-supplied word reached a path join; none does here.
#[must_use]
pub(crate) fn store_db_path(dir: &Path) -> PathBuf {
    chief_dir(dir).join("db").join("chief.db")
}

/// **THE FIRST-RUN CHECK.** Does this directory hold a company?
///
/// The store database's existence is the whole question and the only one. Not
/// a marker file, not a beacond row, not a `.chief/` directory that a crashed
/// genesis could have left half-built: every durable fact about a company is a
/// row in that file, so a directory without it has no company in any sense a
/// caller can act on.
#[must_use]
pub(crate) fn company_present(dir: &Path) -> bool {
    store_db_path(dir).is_file()
}

/// **THE company identity**, re-exported from the leaf both programs link.
///
/// Deliberately NOT defined here. `chiefd-daemon` needs the same answer and
/// the backend/client boundary guard forbids either crate from depending on
/// the other, so the one definition lives in `host-primitives` — see
/// [`host_primitives::rendezvous::company_key`] for why nine independent
/// derivations is the failure this replaces.
pub(crate) use host_primitives::rendezvous::company_key;

/// The canonical absolute path of a directory, for [`company_key`].
///
/// # Errors
/// Whatever `canonicalize` refuses — a directory that is not there, or that
/// this process cannot reach.
pub(crate) fn canonical_dir(dir: &Path) -> super::Result<PathBuf> {
    dir.canonicalize().map_err(|error| {
        super::LifecycleError::refused(format!("chief cannot resolve {} : {error}", dir.display()))
    })
}

/// The identity keys chiefd mints at genesis, `<dir>/.chief/keys`.
#[must_use]
pub(crate) fn keys_dir(dir: &Path) -> PathBuf {
    chief_dir(dir).join("keys")
}

/// DISPOSABLE runtime state, `<dir>/.chief/run`.
///
/// The daemon rendezvous, the daemon's own log, the rail socket and the
/// actuator's scratch. Nothing here is authority: any process may delete the
/// directory and the next command recreates what it needs.
#[must_use]
pub(crate) fn run_dir(dir: &Path) -> PathBuf {
    chief_dir(dir).join("run")
}

/// Where a live daemon publishes its URL and pid, `<dir>/.chief/run/daemon.json`.
///
/// This is the CLI's rendezvous with the daemon, and it replaces a beacond
/// lookup by slug: the client that wants this directory's daemon reads this
/// file and verifies the pid is alive, the same liveness ladder the old
/// discovery path ran after its registry hit. beacond survives as the
/// box-wide presence registry for `chief ls`; it is no longer on the path
/// between a command and its own company.
#[must_use]
pub(crate) fn daemon_rendezvous_path(dir: &Path) -> PathBuf {
    run_dir(dir).join("daemon.json")
}

/// Where a spawned daemon's stdout/stderr goes, `<dir>/.chief/run/daemon.log`.
///
/// Diagnostics, never state: nothing reads this file as authority, and the
/// only consumer is the start-failure message that quotes its last lines back
/// to the operator.
#[must_use]
pub(crate) fn daemon_log_path(dir: &Path) -> PathBuf {
    run_dir(dir).join("daemon.log")
}

/// The brain's rail socket, `<dir>/.chief/run/rail.sock`.
#[must_use]
pub(crate) fn rail_socket_path(dir: &Path) -> PathBuf {
    run_dir(dir).join("rail.sock")
}

// `<dir>/.chief/log`, `<dir>/.chief/logs`, `<dir>/.chief/bus`,
// `<dir>/.chief/agent/<id>` and `<dir>/.pi/skills` are
// deliberately NOT named here, and their absence is a boundary rather than an
// omission. This module is the OPERATOR CLIENT's path table and every entry in
// it has a reader in this crate; those three are written and read entirely by
// the backend:
//
// * the Chief-program jsonl sinks are `chiefd-log`'s, resolved from
//   `ORG_LAUNCHER_ORG_DIR` (`chiefd_log::sink`) because `install` runs before
//   argv is parsed; Pi owns its `.chief/logs` and `.chief/bus` sinks. This crate
//   STAMPS the company-directory variable and never joins those paths itself;
// * an agent's home is minted once at hire by `chiefd-host`, which is a crate
//   this one is forbidden to link;
// * `.pi/skills` becomes the USER's and Pi's after the backend seeds the
//   shipped company skills once at genesis. This client never reads or writes
//   it.
//
// A path constant with no reader is a second answer waiting to disagree with
// the first, which is the whole failure this stage exists to end.

// --- box-level install facts, the only `~` residents left -------------------

/// Where this box's chief install lives, `$HOME/.chief`.
///
/// The same [`CHIEF_DIR`] a company's own folder is named with, and the daemon
/// composes its `launcher-root` read from that constant too — see its doc for
/// the divergence that made a shared spelling worth a `pub`.
#[must_use]
pub(crate) fn install_home(home: &Path) -> PathBuf {
    home.join(CHIEF_DIR)
}

/// The daemon executable beside the client that is running now.
///
/// There is deliberately no `chief_binary` beside it. This crate IS the
/// installed `chief`, and the one place that needs its own path — the Founder
/// respawn — takes `std::env::current_exe()`, so a `chief` run out of a cargo
/// target directory re-execs itself and not whatever happens to be installed.
/// A constant naming this program's own install path would be a second answer
/// to "which chief is running", and the wrong one exactly when it mattered.
///
/// TWO binaries, not one. `chief` is this crate — the operator client, which
/// owns tmux and talks HTTP; `chiefd` is the backend, which owns a company's
/// durable state and knows nothing about a terminal. A company's daemon is
/// started by spawning that program, never by re-invoking this one.
///
/// The pair is one release unit. Resolving `chiefd` from `$HOME` here used to
/// let a Chief run from a fresh build start an older installed daemon. That
/// daemon could predate the rendezvous contract, stay alive, and make the
/// current client wait the complete startup budget for a file it could never
/// publish. The forwarder already used the sibling; company startup must use
/// the same one answer.
#[must_use]
pub(crate) fn chiefd_daemon_binary(client_executable: &Path) -> PathBuf {
    client_executable.with_file_name(super::DAEMON_PROGRAM)
}

/// The installed beacond executable, beside the installed chief.
#[must_use]
pub(crate) fn beacond_binary(home: &Path) -> PathBuf {
    install_bin(home).join("beacond")
}

/// `$HOME/.chief/bin`, the three symlinks an operator's `PATH` names.
///
/// SYMLINKS, not binaries. Each entry points into
/// `versions/<v>/bin/`, and an upgrade re-points them with `rename(2)` rather
/// than overwriting a file — which is what makes an upgrade safe while a daemon
/// is running: the old inode stays alive as long as that process holds it open.
#[must_use]
pub(crate) fn install_bin(home: &Path) -> PathBuf {
    install_home(home).join("bin")
}

/// `$HOME/.chief/versions`, one directory per installed version.
#[must_use]
pub(crate) fn versions_dir(home: &Path) -> PathBuf {
    install_home(home).join("versions")
}

/// One installed version's own directory: its binaries, its resources, and its
/// manifest, all of which were built together and are only correct together.
#[must_use]
pub(crate) fn version_dir(home: &Path, version: &str) -> PathBuf {
    versions_dir(home).join(version)
}

/// `$HOME/.chief/state`, the upgrade journal's home.
///
/// Not a company's state and not a cache: it is what `chief upgrade --rollback`
/// reads to know which version to go back to.
#[must_use]
pub(crate) fn install_state_dir(home: &Path) -> PathBuf {
    install_home(home).join("state")
}

/// The resources installed beside the binary that is running right now.
///
/// # TOMBSTONE: `launcher_root_record` and `~/.chief/launcher-root`
///
/// A pointer FILE holding the absolute path of the source CHECKOUT stood here,
/// and it was read for two things: Founder's Pi assets, and the preflight's
/// "was this host ever released" question. Both now read the same thing every
/// other consumer does — the `resources/` directory sitting beside this
/// executable — and the pointer is deleted rather than kept as a fallback.
///
/// It had to go for a product reason, not a tidiness one: while it existed the
/// installed binaries were a front end for a git working copy that had to stay
/// on disk, at that path, at a compatible revision. A user who never cloned
/// anything could not be served, so neither the curl installer nor
/// `chief upgrade` could exist. `host_primitives::install` carries the full
/// account, including the two incidents the pointer's silent absence caused.
#[must_use]
pub(crate) fn resource_root() -> Option<PathBuf> {
    host_primitives::install::resource_root_from_exe()
}

/// `$HOME` as a path.
///
/// # Errors
/// Refuses when `HOME` is unset or empty. There is no literal fallback: a
/// guessed home is how a Linux deployment path ended up baked into a macOS
/// host's company records.
pub(crate) fn home() -> super::Result<PathBuf> {
    match std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        Some(value) => Ok(PathBuf::from(value)),
        None => Err(super::LifecycleError::refused(
            "chief cannot resolve its home: HOME is unset. Run it from a normal login shell.",
        )),
    }
}

/// The directory this command was run in, canonicalized.
///
/// THE front door's one input. Every path above hangs off it.
///
/// # Errors
/// Refuses when the current directory cannot be read or resolved — a deleted
/// cwd, or one this process cannot reach.
pub(crate) fn current_dir() -> super::Result<PathBuf> {
    let cwd = std::env::current_dir().map_err(|error| {
        super::LifecycleError::refused(format!(
            "chief cannot resolve the current directory: {error}. Run it from a directory that \
             still exists."
        ))
    })?;
    canonical_dir(&cwd)
}

/// The canonical company-slug form, `^[a-z0-9]+(?:-[a-z0-9]+)*$`.
///
/// Still enforced, and no longer for path safety: a slug reaches no path join
/// now. It is the DISPLAY name and the tmux session word, and both want the
/// same closed vocabulary — `session_name_for_slug` composes it into a target
/// tmux itself parses.
///
/// Written as an explicit scan rather than a regex dependency: the rule is
/// "lowercase alphanumeric runs joined by single hyphens, no leading or
/// trailing hyphen", and a scan states exactly that.
#[must_use]
pub(crate) fn is_canonical_slug(slug: &str) -> bool {
    if slug.is_empty() {
        return false;
    }
    let mut previous_was_hyphen = true; // a leading hyphen is illegal
    for character in slug.chars() {
        match character {
            'a'..='z' | '0'..='9' => previous_was_hyphen = false,
            '-' if !previous_was_hyphen => previous_was_hyphen = true,
            _ => return false,
        }
    }
    !previous_was_hyphen // a trailing hyphen is illegal
}

#[cfg(test)]
mod tests {
    use super::{
        beacond_binary, canonical_dir, chiefd_daemon_binary, company_key, company_present,
        daemon_log_path, daemon_rendezvous_path, install_bin, install_home, install_state_dir,
        is_canonical_slug, keys_dir, rail_socket_path, run_dir, store_db_path, version_dir,
        versions_dir,
    };
    use std::path::{Path, PathBuf};

    /// EVERY durable byte hangs off `<dir>/.chief`, and nothing hangs off `~`.
    ///
    /// The claim the whole move rests on: `rm -rf <dir>` removes a company
    /// completely, and no company writes anything to the operator's home.
    #[test]
    fn every_company_path_hangs_off_the_directorys_own_chief_folder() {
        let dir = Path::new("/work/anvils");
        let chief = dir.join(".chief");
        for path in [
            store_db_path(dir),
            keys_dir(dir),
            run_dir(dir),
            daemon_rendezvous_path(dir),
            daemon_log_path(dir),
            rail_socket_path(dir),
        ] {
            assert!(
                path.starts_with(&chief),
                "{} escapes the company's own .chief folder",
                path.display()
            );
        }
        assert_eq!(store_db_path(dir), PathBuf::from("/work/anvils/.chief/db/chief.db"));
        assert_eq!(keys_dir(dir), PathBuf::from("/work/anvils/.chief/keys"));
    }

    /// `.pi/` is the USER's, so nothing this module names may reach it.
    ///
    /// The claim `chief rm` rests on, asserted at the source of every path
    /// rather than at the one verb that deletes: `remove.rs` removes
    /// `chief_dir` whole, so containment HERE is what makes "it never touches
    /// .pi/ or any user file" true by construction.
    #[test]
    fn nothing_this_module_names_reaches_the_users_own_files() {
        let dir = Path::new("/work/anvils");
        let users = [dir.join(".pi"), dir.join("README.md"), dir.join("src")];
        for path in [store_db_path(dir), keys_dir(dir), run_dir(dir), daemon_log_path(dir)] {
            for own in &users {
                assert!(
                    !path.starts_with(own),
                    "{} reaches the user's {}",
                    path.display(),
                    own.display()
                );
            }
        }
    }

    /// The only `~` residents are INSTALL facts about the box.
    #[test]
    fn the_home_directory_holds_binaries_and_nothing_about_any_company() {
        let home = Path::new("/home/op");
        assert_eq!(install_home(home), PathBuf::from("/home/op/.chief"));
        assert_eq!(
            chiefd_daemon_binary(Path::new("/home/op/.chief/bin/chief")),
            PathBuf::from("/home/op/.chief/bin/chiefd")
        );
        assert_eq!(beacond_binary(home), PathBuf::from("/home/op/.chief/bin/beacond"));
        assert_eq!(install_bin(home), PathBuf::from("/home/op/.chief/bin"));
        assert_eq!(versions_dir(home), PathBuf::from("/home/op/.chief/versions"));
        assert_eq!(version_dir(home, "v2.0.7"), PathBuf::from("/home/op/.chief/versions/v2.0.7"));
        assert_eq!(install_state_dir(home), PathBuf::from("/home/op/.chief/state"));
    }

    /// A Chief binary from a fresh build must start the daemon from that same
    /// build, not an older daemon left in the user's install directory.
    ///
    /// The live failure had exactly this shape: `/opt/chief/bin/chief` was the
    /// current client, `$HOME/.chief/bin/chiefd` still predated daemon
    /// rendezvous, and the client waited its full budget for a file that old
    /// process could never publish.
    #[test]
    fn a_running_chief_uses_the_chiefd_beside_it_not_an_old_home_install() {
        let running_chief = Path::new("/opt/chief/bin/chief");
        let stale_home_daemon = Path::new("/home/fresh/.chief/bin/chiefd");

        let selected = chiefd_daemon_binary(running_chief);
        assert_eq!(selected, running_chief.with_file_name("chiefd"));
        assert_ne!(selected, stale_home_daemon, "an old home install must not be selected");
    }

    /// THE FIRST-RUN CHECK, and its exact subject.
    ///
    /// A `.chief/` directory that exists without a database is NOT a company:
    /// a crashed genesis, or a `run/` a stale daemon recreated, must still
    /// land the operator in Founder mode rather than in a company that has no
    /// rows.
    #[test]
    fn a_directory_holds_a_company_only_when_its_store_database_is_there() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!company_present(dir.path()), "an empty directory holds no company");

        std::fs::create_dir_all(run_dir(dir.path())).expect("a stale run directory");
        assert!(!company_present(dir.path()), "a .chief folder alone is not a company");

        let db = store_db_path(dir.path());
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("db dir");
        assert!(!company_present(dir.path()), "an empty db directory is not a company");

        // Through this crate's own atomic-publish primitive, not `std::fs::write`:
        // `clippy.toml` bans the bare call everywhere, tests included, because the
        // filesystem-effects seam is the one place a write may happen. The
        // `allow-*-in-tests` switches beside it cover `unwrap`/`expect`/`panic`
        // only — a fixture write is still a write.
        chief_cli::files::publish_atomically(&db, "", 0o600).expect("the store");
        assert!(company_present(dir.path()), "the database IS the company");
    }

    /// TWO DIRECTORIES MAY HOLD COMPANIES WITH THE SAME NAME.
    ///
    /// The case the old global slug registry could not represent at all, and
    /// the reason the wire key is the directory rather than the slug.
    #[test]
    fn the_company_key_separates_two_directories_and_is_stable_for_one() {
        let first = company_key(Path::new("/work/acme"));
        let second = company_key(Path::new("/elsewhere/acme"));
        assert_ne!(first, second, "same name, different directories, different companies");
        assert_eq!(first, company_key(Path::new("/work/acme")), "and it is a pure function");
        assert_eq!(first.len(), 12, "twelve hex characters");
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()), "hex only: {first}");
    }

    /// A key is only as good as the path it hashes, so the path is
    /// canonicalized before it is hashed — `.` and a symlink must not key one
    /// company two ways.
    #[test]
    fn a_relative_or_indirect_path_canonicalizes_to_the_same_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().canonicalize().expect("canonical tempdir");
        let indirect = real.join("child").join("..");
        std::fs::create_dir_all(real.join("child")).expect("child");

        assert_eq!(
            company_key(&canonical_dir(&indirect).expect("canonicalize")),
            company_key(&real),
            "one directory, one key, however the caller spelled it"
        );
    }

    #[test]
    fn the_slug_rule_accepts_exactly_the_canonical_form() {
        for good in ["a", "acme", "acme-co", "a1-b2-c3"] {
            assert!(is_canonical_slug(good), "{good} is canonical");
        }
        for bad in ["", "-a", "a-", "a--b", "Acme", "acme_co", "acme co", "acme/co", "a.b"] {
            assert!(!is_canonical_slug(bad), "{bad} is not canonical");
        }
    }
}
