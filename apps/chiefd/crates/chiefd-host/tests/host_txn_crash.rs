//! Crash-injection tests for host transactions (TESTING.md §4.3).
//!
//! **These are real crashes.** The process that runs the host transaction is a
//! genuinely separate child process, parked at a named pause point
//! ([`chiefd_host::pause`]) and then **SIGKILLed** — no unwinding, no `Drop`,
//! no WAL checkpoint, no cooperative shutdown of any kind. Then the parent
//! opens the same database, runs the startup recovery pass, and asserts the
//! world converged.
//!
//! Simulating the crash in-process would have proved much less: the states this
//! mechanism exists for are precisely the ones where a destructor did *not*
//! run. The project's rule that no race may be validated by repetition
//! (TESTING.md §1.2) is honoured too — the child is killed at a named,
//! deterministic instant, never on a timer.
//!
//! The child is this very test binary, re-invoked with `--exact crash_child`
//! and two environment variables. That keeps one build, one fixture definition
//! and one code path shared between parent and child.
//!
//! Three cases, matching the plan's three durable positions:
//!
//! | Test | Killed at | Durable phase | Expected convergence |
//! |---|---|---|---|
//! | `host_txn_crash_between_intent_and_publish` | after commit 1 | `pending` | roll back; nothing was ever published |
//! | `host_txn_crash_mid_publish_restores_previous_bytes` | between two files | `pending` | roll back the torn half from backups |
//! | `host_txn_crash_after_publish_before_close` | after the publish mark | `published` | roll forward to commit 2 |

// This whole file is test code, but its helpers are plain functions rather
// than `#[test]` items, and clippy's `allow-expect-in-tests` only reaches the
// latter. A failed `expect` here is a failed test, which is the point.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use rusqlite::OptionalExtension;

use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SharedClock;
use chiefd_core::host_action::HostActionPhase;
use chiefd_core::polarity::StoreKind;
use chiefd_core::store::organization::{OrganizationManifest, OrganizationStore};
use chiefd_core::store::{open_company_db_readonly, COMPANY_DB_FILENAME};
use chiefd_core::test_support::{northstar_manifest, ManualClock};
use chiefd_host::executor::{MaterializeFile, MaterializePlan};
use chiefd_host::fake::FakeHostExecutor;
use chiefd_host::host_txn::{
    self, HostTxnPlan, AFTER_FIRST_FILE, AFTER_INTENT_COMMIT, AFTER_PUBLISH,
};

const CRASH_AT: &str = "CHIEFD_CRASH_AT";
const CRASH_ROOT: &str = "CHIEFD_CRASH_ROOT";

/// The manifest commit 2 creates, and the purpose it carries.
///
/// Host transactions may create the FIRST manifest authority only — replacing
/// an existing manifest is retired (`host-txn-manifest-update-retired`,
/// host_txn.rs `commit_two`), so the seed deliberately leaves the company
/// without one. The pre-transaction state is then "no manifest"; the property
/// under test is unchanged: commit 2 either happened completely or not at all.
const MANIFEST_AFTER: &str = "after materialization";

const A_BEFORE: &str = "original a\n";
const A_AFTER: &str = "new a\n";
const B_AFTER: &str = "new b\n";

fn db_path(root: &Path) -> PathBuf {
    root.join(COMPANY_DB_FILENAME)
}

fn tree(root: &Path) -> PathBuf {
    root.join("tree")
}

fn backups(root: &Path) -> PathBuf {
    root.join("backups")
}

fn open_db(root: &Path) -> CompanyDb {
    let clock: SharedClock = Arc::new(ManualClock::default());
    CompanyDb::open("cobalt", &db_path(root), clock).expect("open company db")
}

fn fixture_manifest(purpose: &str) -> OrganizationManifest {
    let mut manifest = northstar_manifest(1_784_116_800_000);
    manifest.slug = "cobalt".to_owned();
    manifest.purpose = purpose.to_owned();
    manifest.name = purpose.to_owned();
    let root_id = manifest.root_department_id.clone();
    let root = manifest.departments.get_mut(&root_id).expect("root department");
    root.name = purpose.to_owned();
    manifest
}

/// The plan both the parent and the child use. Defined once, so "the child did
/// what the parent asserts about" is not a coincidence.
///
/// `a.json` exists beforehand (so rollback must *restore* it) and `b.json` does
/// not (so rollback must *delete* it) — the two rollback behaviours that get
/// conflated when a backup set records only content.
fn fixture_plan(root: &Path) -> HostTxnPlan {
    HostTxnPlan {
        materialize: MaterializePlan {
            root: tree(root),
            files: vec![
                MaterializeFile {
                    relative_path: "a.json".into(),
                    contents: A_AFTER.into(),
                    mode: 0o600,
                },
                MaterializeFile {
                    relative_path: "b.json".into(),
                    contents: B_AFTER.into(),
                    mode: 0o600,
                },
            ],
        },
        manifest: Some(fixture_manifest(MANIFEST_AFTER)),
    }
}

fn executor() -> FakeHostExecutor {
    // Real filesystem: a fake whose materialize writes nothing would leave
    // nothing to roll back, which is the entire subject here.
    FakeHostExecutor::new().with_real_filesystem()
}

// --- the child ----------------------------------------------------------

/// The crashing side. A no-op unless [`CRASH_ROOT`] is set, so it is harmless
/// as an ordinary test.
#[test]
fn crash_child() {
    let (Ok(root), Ok(crash_at)) = (std::env::var(CRASH_ROOT), std::env::var(CRASH_AT)) else {
        return;
    };
    let root = PathBuf::from(root);

    chiefd_host::pause::install(move |name| {
        if name == crash_at {
            // SIGKILL, not `abort()`/`exit()`: no unwinding, no `Drop`, no WAL
            // checkpoint. Exactly what a power loss or an OOM kill leaves.
            let _ = nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::SIGKILL);
        }
    });

    let runtime =
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    runtime.block_on(async {
        let db = open_db(&root);
        let plan = fixture_plan(&root);
        let outcome = host_txn::run(&db, &executor(), &backups(&root), "materialize", &plan).await;
        panic!("the pause point was never reached; transaction returned {outcome:?}");
    });
}

/// Run the child to its death at `crash_at`, and assert it really was killed.
///
/// A child that exits any other way — success, panic, non-zero status — is a
/// test failure, not something to retry: it means the pause point moved.
fn crash_at(root: &Path, point: &str) {
    use std::os::unix::process::ExitStatusExt;

    let exe = std::env::current_exe().expect("current test binary");
    let status = Command::new(exe)
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CRASH_ROOT, root)
        .env(CRASH_AT, point)
        .status()
        .expect("spawn the crashing child");

    assert_eq!(
        status.signal(),
        Some(nix::sys::signal::SIGKILL as i32),
        "the child must die by SIGKILL at {point}, not exit ({status:?})"
    );
}

// --- the parent's shared setup and assertions ---------------------------

/// Seed the pre-transaction world: `a.json` on disk, `b.json` absent, and NO
/// manifest — the host transaction under test creates the first authority in
/// its commit 2 (the only manifest write host transactions may still make).
fn seed(root: &Path) {
    std::fs::create_dir_all(tree(root)).expect("tree");
    std::fs::create_dir_all(backups(root)).expect("backups");
    chiefd_host::files::publish_atomically(&tree(root).join("a.json"), A_BEFORE, 0o600)
        .expect("seed a.json");
}

fn read_file(root: &Path, relative: &str) -> Option<String> {
    std::fs::read_to_string(tree(root).join(relative)).ok()
}

fn manifest(db: &CompanyDb) -> Option<String> {
    db.read(|snapshot| {
        snapshot
            .document_body(OrganizationStore::NAME)
            .and_then(|body| serde_json::from_str::<OrganizationManifest>(body).ok())
            .map(|manifest| manifest.name)
    })
}

fn intents(db: &CompanyDb) -> Vec<(String, HostActionPhase)> {
    db.read(|snapshot| {
        snapshot
            .open_host_actions()
            .into_iter()
            .map(|(id, record)| (id.to_owned(), record.phase()))
            .collect()
    })
}

fn leftover_backups(root: &Path) -> Vec<String> {
    std::fs::read_dir(backups(root))
        .expect("readdir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

/// The drift check the M9 acceptance criterion demands after every recovery
/// path: re-running the plan that describes the *converged* state must report
/// every file `unchanged` and nothing `changed`.
fn assert_drift_clean(root: &Path, expected: &[(&str, &str)]) {
    let plan = MaterializePlan {
        root: tree(root),
        files: expected
            .iter()
            .map(|(path, contents)| MaterializeFile {
                relative_path: (*path).to_owned(),
                contents: (*contents).to_owned(),
                mode: 0o600,
            })
            .collect(),
    };
    let report = chiefd_host::files::materialize(&plan).expect("drift check");
    assert!(report.changed.is_empty(), "drift after recovery: {:?}", report.changed);
    assert!(report.conflicts.is_empty(), "conflicts after recovery: {:?}", report.conflicts);
}

fn recover(root: &Path, db: &CompanyDb) -> host_txn::RecoveryReport {
    let runtime = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
    runtime.block_on(host_txn::recover(db, &executor(), &backups(root))).expect("recover")
}

// --- the tests ----------------------------------------------------------

#[test]
fn host_txn_crash_between_intent_and_publish() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    seed(root);

    crash_at(root, AFTER_INTENT_COMMIT);

    // What SIGKILL left behind: commit 1 survived, nothing else happened.
    let db = open_db(root);
    let open = intents(&db);
    assert_eq!(open.len(), 1, "commit 1 is durable across a SIGKILL");
    assert_eq!(open[0].1, HostActionPhase::Pending);
    assert_eq!(read_file(root, "a.json").as_deref(), Some(A_BEFORE));
    assert_eq!(read_file(root, "b.json"), None);
    assert_eq!(manifest(&db), None, "invariant 8: the manifest was never created");

    let report = recover(root, &db);
    assert_eq!(report.rolled_back.len(), 1, "an unfinished intent rolls back: {report:?}");
    assert!(report.rolled_forward.is_empty());

    assert!(intents(&db).is_empty(), "recovery closes the intent");
    assert_eq!(manifest(&db), None, "the manifest was never created");
    assert_eq!(read_file(root, "a.json").as_deref(), Some(A_BEFORE));
    assert_eq!(read_file(root, "b.json"), None);
    assert!(leftover_backups(root).is_empty(), "the backup set is dropped once it is spent");
    assert_drift_clean(root, &[("a.json", A_BEFORE)]);
}

#[test]
fn host_txn_crash_mid_publish_restores_previous_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    seed(root);

    crash_at(root, AFTER_FIRST_FILE);

    // The torn state: one file of the plan published, the other not. This is
    // the state a single-database transaction cannot prevent and cannot see.
    assert_eq!(read_file(root, "a.json").as_deref(), Some(A_AFTER), "a.json was published");
    assert_eq!(read_file(root, "b.json"), None, "b.json was not");

    let db = open_db(root);
    assert_eq!(intents(&db).len(), 1);
    assert_eq!(intents(&db)[0].1, HostActionPhase::Pending);
    assert_eq!(manifest(&db), None);

    let report = recover(root, &db);
    assert_eq!(report.rolled_back.len(), 1, "{report:?}");

    assert_eq!(
        read_file(root, "a.json").as_deref(),
        Some(A_BEFORE),
        "the published half is restored byte-for-byte from its backup"
    );
    assert_eq!(read_file(root, "b.json"), None, "the half that was never written stays absent");
    assert_eq!(manifest(&db), None);
    assert!(intents(&db).is_empty());
    assert!(leftover_backups(root).is_empty());
    assert_drift_clean(root, &[("a.json", A_BEFORE)]);
}

#[test]
fn host_txn_crash_after_publish_before_close() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    seed(root);

    // Invariant 8, asserted by an observer *outside* the writing process for
    // the whole duration: at no instant may the durable manifest name a file
    // that is not published. The observer opens the company DB read-only, so
    // it can never become a second writer.
    let stop = Arc::new(AtomicBool::new(false));
    let violations = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    // Readable by the main thread WHILE the observer runs. `outcome` is moved
    // into the thread and cannot be inspected until `join`, so without this the
    // only way to learn whether the observer ever read anything was to stop it
    // first and assert afterwards - which is what made a starved thread on a
    // loaded host indistinguishable from a broken one.
    let seen = Arc::new(AtomicU32::new(0));
    let observer = {
        let (stop, violations, root, seen) =
            (Arc::clone(&stop), Arc::clone(&violations), root.to_path_buf(), Arc::clone(&seen));
        std::thread::spawn(move || {
            let mut outcome = ObserverOutcome::default();
            while !stop.load(Ordering::SeqCst) {
                outcome.open_attempts += 1;
                let conn = match open_company_db_readonly(&db_path(&root)) {
                    Ok(conn) => conn,
                    Err(error) => {
                        outcome.record_open_failure(&error);
                        continue;
                    }
                };
                // #949: company genesis publishes in more than one step — the
                // database FILE and its schema (the `departments` table) are
                // not the same instant. A reader can win the race on the file
                // (`open_company_db_readonly` succeeds) while losing it on
                // the schema (the table doesn't exist yet), which used to
                // surface as a `no such table: departments` error from the
                // informative query below — a real, load-bearing race, not a
                // starvation artifact, and not fixable by waiting longer on
                // "can I open it" (open already succeeded in that case).
                // Folding the schema check into the SAME readiness gate as
                // the open check — "the manifest is readable AND its schema
                // is complete" as one condition — means the informative query
                // below only ever runs once schema is actually in place, so
                // it can no longer observe the intermediate state.
                let schema_ready: bool = match conn
                    .query_row(
                        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'departments'",
                        [],
                        |_row| Ok(()),
                    )
                    .optional()
                {
                    Ok(present) => present.is_some(),
                    Err(error) => {
                        outcome.record_open_failure(&error);
                        continue;
                    }
                };
                if !schema_ready {
                    outcome.open_not_yet_created += 1;
                    continue;
                }
                // `.optional()`, not `.ok()`: `QueryReturnedNoRows` (the
                // department row not seeded yet at this instant, expected and
                // harmless) must not be conflated with a REAL query error
                // (e.g. schema drift) — #876 found the previous `.ok()` here
                // discarded both identically.
                let body: Option<String> = match conn
                    .query_row(
                        "SELECT name FROM departments WHERE slug = ?1 AND id = ?2",
                        ["cobalt", "executive"],
                        |row| row.get(0),
                    )
                    .optional()
                {
                    Ok(body) => body,
                    Err(error) => {
                        outcome.record_query_failure(&error);
                        continue;
                    }
                };
                if let Some(body) = body {
                    outcome.observations += 1;
                    seen.fetch_add(1, Ordering::SeqCst);
                    if body == MANIFEST_AFTER {
                        for (name, expected) in [("a.json", A_AFTER), ("b.json", B_AFTER)] {
                            if std::fs::read_to_string(tree(&root).join(name)).ok().as_deref()
                                != Some(expected)
                            {
                                let mut seen =
                                    violations.lock().unwrap_or_else(|poison| poison.into_inner());
                                seen.push(format!("manifest named {name} before it was published"));
                            }
                        }
                    }
                }
            }
            outcome
        })
    };

    crash_at(root, AFTER_PUBLISH);

    // Both files are published and the publish is durably marked, but commit 2
    // never ran: the manifest is still behind.
    assert_eq!(read_file(root, "a.json").as_deref(), Some(A_AFTER));
    assert_eq!(read_file(root, "b.json").as_deref(), Some(B_AFTER));

    let db = open_db(root);
    let open = intents(&db);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].1, HostActionPhase::Published, "the publish mark survived the SIGKILL");
    assert_eq!(manifest(&db), None, "commit 2 did not run");

    let report = recover(root, &db);
    assert_eq!(report.rolled_forward.len(), 1, "a published intent replays forward: {report:?}");
    assert!(report.rolled_back.is_empty());

    assert_eq!(manifest(&db).as_deref(), Some(MANIFEST_AFTER), "commit 2 completed");
    assert!(intents(&db).is_empty());
    assert_eq!(read_file(root, "a.json").as_deref(), Some(A_AFTER));
    assert_eq!(read_file(root, "b.json").as_deref(), Some(B_AFTER));
    assert!(leftover_backups(root).is_empty());
    assert_drift_clean(root, &[("a.json", A_AFTER), ("b.json", B_AFTER)]);

    // Every assertion above has just proved the durable state is FINAL and
    // published, so a read here must succeed - the race window the observer
    // exists to police is over. Give it a bounded chance to take that read
    // before stopping it.
    //
    // The flake this removes: on a loaded host the observer is scheduled only a
    // handful of times across the whole window (one failing run managed 12
    // attempts and sampled every one of them at an instant the row did not yet
    // exist), so `observations == 0` fired for a reason that is about the host,
    // not the product. This is deliberately NOT a relaxed assertion - the assert
    // below is unchanged and still fails if the observer never reads anything.
    // What changed is that a starved thread is now given time rather than
    // reported as a finding.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while seen.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    stop.store(true, Ordering::SeqCst);
    let outcome = observer.join().expect("observer thread");
    // Visible on a PASSING run too, not just a failing one: #876 found 68
    // discarded opens inside a single green run with no trace anywhere. This
    // isn't asserted on (that would trade one flake risk for another — a
    // `CannotOpen` burst before `seed`'s writer creates the file is expected
    // near the start of every run) but it is no longer invisible.
    eprintln!("host_txn_crash_after_publish_before_close observer: {outcome}");
    let violations = violations.lock().unwrap_or_else(|poison| poison.into_inner()).clone();
    assert!(violations.is_empty(), "invariant 8 violated: {violations:?}");
    // Unweakened per #876: this is still the only guard against the test
    // passing while the observer silently read nothing the whole run. What
    // changed is the message on the side that used to be blind — a wrong
    // `db_path`/timing bug (every attempt races `CannotOpen`, harmless near
    // the start but a bug if it never clears) and a starved thread (the OS
    // simply never scheduled it) used to look byte-for-byte identical:
    // `observations == 0`, no other evidence. `outcome`'s breakdown names
    // which one happened.
    assert!(outcome.observations > 0, "the observer never managed to read the manifest: {outcome}");
}

/// What the invariant-8 observer thread learned across the whole race
/// window, not just "how many times did it see the manifest" — #876: the
/// previous version discarded every `open_company_db_readonly`/`query_row`
/// error identically (`let-else … continue` / `.ok()`), so a wrong path and
/// a starved thread were indistinguishable, and one passing run was found to
/// discard 68 opens with zero visibility into why. Classified by SQLite's
/// own error code rather than a string match, where available.
#[derive(Default)]
struct ObserverOutcome {
    observations: u32,
    open_attempts: u32,
    /// `CannotOpen` (SQLITE_CANTOPEN): the file doesn't exist at this
    /// instant yet — expected and harmless near the start of the race,
    /// before `seed`'s writer has created the database file at all. #949:
    /// also counts the file-exists-but-schema-not-yet-applied case (the
    /// `departments` table missing) — folded into this SAME bucket rather
    /// than kept as a separate `query_errors` case, because both mean
    /// identically "not ready yet, keep polling," and treating them
    /// differently is what let the informative query below observe — and
    /// error on — the intermediate state.
    open_not_yet_created: u32,
    /// `DatabaseBusy`/`DatabaseLocked`: a real reader/writer collision —
    /// exactly the contention a read-only, no-mutex connection is supposed
    /// to be immune to (see `open_company_db_readonly`'s own doc comment).
    /// Nonzero here is worth seeing even on a run that otherwise passes.
    open_busy_or_locked: u32,
    open_other: u32,
    open_other_sample: Option<String>,
    query_errors: u32,
    query_error_sample: Option<String>,
}

impl ObserverOutcome {
    fn record_open_failure(&mut self, error: &rusqlite::Error) {
        match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::CannotOpen) => self.open_not_yet_created += 1,
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                self.open_busy_or_locked += 1;
            }
            _ => {
                self.open_other += 1;
                self.open_other_sample.get_or_insert_with(|| error.to_string());
            }
        }
    }

    fn record_query_failure(&mut self, error: &rusqlite::Error) {
        self.query_errors += 1;
        self.query_error_sample.get_or_insert_with(|| error.to_string());
    }
}

impl std::fmt::Display for ObserverOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "open_attempts={} not_yet_created={} busy_or_locked={} open_other={}",
            self.open_attempts,
            self.open_not_yet_created,
            self.open_busy_or_locked,
            self.open_other,
        )?;
        if let Some(sample) = &self.open_other_sample {
            write!(f, " (e.g. {sample:?})")?;
        }
        write!(f, " query_errors={}", self.query_errors)?;
        if let Some(sample) = &self.query_error_sample {
            write!(f, " (e.g. {sample:?})")?;
        }
        Ok(())
    }
}

#[test]
fn a_second_recovery_pass_finds_nothing_left_to_do() {
    // Recovery is a convergence, not a one-shot: chiefd may restart again
    // during recovery, so running the pass twice must be indistinguishable
    // from running it once (invariant 40 — idempotent replay, never restart).
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    seed(root);
    crash_at(root, AFTER_PUBLISH);

    let db = open_db(root);
    assert_eq!(recover(root, &db).rolled_forward.len(), 1);
    let first_manifest = manifest(&db);

    let second = recover(root, &db);
    assert!(second.is_empty(), "{second:?}");
    assert_eq!(manifest(&db), first_manifest, "a repeated recovery must not alter the manifest");
    assert_drift_clean(root, &[("a.json", A_AFTER), ("b.json", B_AFTER)]);
}
