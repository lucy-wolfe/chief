//! Host transactions — the DB↔filesystem boundary gets its 2PC back.
//!
//! Plan §5.6, the single most serious finding of the adversarial reviews, and
//! the one that comes from a real incident. SQLite's atomicity covers **one
//! database**. It does not cover a DB↔filesystem pair. In the predecessor, one
//! logical operation wrote a file store and a SQL store in sequence with no
//! atomicity across them; a service blip advanced one and not the other and
//! left them divergent with nothing to reconcile. A host effect is therefore
//! never an unrecorded side effect of a DB transaction.
//!
//! Every op that pairs a DB mutation with executor filesystem work —
//! `materialize` (hire, department add/move, unit removal), the model-catalog
//! symlink swap, the provider-credential
//! scrub, `model.change`, runtime-preference publication — runs as:
//!
//! 1. **Commit 1** — a `host_actions` intent row (kind, full plan,
//!    `phase = pending`) commits. No manifest or desired-state change yet.
//! 2. **Executor phase** — take per-file backups, then publish by rename, one
//!    file at a time through [`HostExecutor::materialize`]. Plans are
//!    idempotent and replayable by construction. A durable `phase = published`
//!    ends the phase.
//! 3. **Commit 2** — manifest/desired-state advance *and* the intent row
//!    closed, in **one** transaction. That is literal, not aspirational:
//!    documents and `host_actions` rows live in the same
//!    [`Ledgers`](chiefd_core::ledger::Ledgers), and one
//!    [`CompanyDb::mutate`] closure is one SQL transaction.
//!
//! Invariant 8 is preserved exactly by that ordering: the durable manifest
//! never references files that are not yet published, because the manifest
//! commit is commit 2.
//!
//! # What a crash leaves, and what [`recover`] does with it
//!
//! | Crash point | Durable phase | Files | Recovery |
//! |---|---|---|---|
//! | after commit 1, before backups | `pending` | untouched | nothing to undo; the row is closed |
//! | mid-publish | `pending` | partly published | **roll back** from the backup set |
//! | after publish, before the mark | `pending` | fully published | **roll back** — indistinguishable from mid-publish, and safe because the manifest never advanced |
//! | after the mark, before commit 2 | `published` | fully published | **roll forward**: replay (a no-op) and commit 2 |
//! | after commit 2 | row gone | published | nothing |
//!
//! Rolling back a *fully* published plan is not a lost update: the manifest
//! never advanced, so no durable state ever referred to those files, and the
//! caller never received success. The operation simply did not happen — which
//! is the entire point of putting the desired-state advance in commit 2.
//!
//! # Why the plan is journalled in full
//!
//! The recovery pass runs at startup with no caller, no closure and no
//! in-memory context. It must be able to *finish* commit 2 — including the
//! manifest advance — from the row alone. A host transaction is therefore
//! declarative: [`HostTxnPlan`] carries both the filesystem work and the
//! document writes, and the live path and the recovery path apply it through
//! the same function, so "replay converges to the same state" is a property of
//! the code rather than of two implementations agreeing.

use std::path::{Path, PathBuf};

use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::error::ChiefdError;
use chiefd_core::host_action::{HostActionPhase, HostActionRecord};
use chiefd_core::store::organization::{self, OrganizationManifest};
use serde::{Deserialize, Serialize};

use crate::executor::{DriftReport, HostErr, HostExecutor, MaterializePlan};
use crate::files;
use crate::pause;

/// The phase of a journalled intent, under the name this crate has used for it
/// since M5. The type itself belongs to `chiefd-core`, which owns the row.
pub use chiefd_core::host_action::HostActionPhase as HostTxnPhase;

/// Pause point: commit 1 is durable; nothing on disk has been touched.
pub const AFTER_INTENT_COMMIT: &str = "host-txn:after-intent-commit";
/// Pause point: every backup is durable; publishing is about to start.
pub const AFTER_BACKUPS: &str = "host-txn:after-backups";
/// Pause point: the first file of the plan has been published and the rest
/// have not — the torn state the whole mechanism exists for.
pub const AFTER_FIRST_FILE: &str = "host-txn:after-first-file";
/// Pause point: publishing finished **and** `phase = published` is durable.
pub const AFTER_PUBLISH: &str = "host-txn:after-publish";
/// Pause point: commit 2 is durable; only backup cleanup remains.
pub const AFTER_COMMIT: &str = "host-txn:after-commit";

/// A complete host transaction: filesystem work, plus the desired-state
/// advance that becomes true only once that work is published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostTxnPlan {
    /// Files to publish, as an idempotent convergence.
    pub materialize: MaterializePlan,
    /// The structural state commit 2 advances. `None` is legal — a
    /// pure-filesystem transaction still wants the journal, because a crash
    /// mid-publish still needs the rollback. This is typed rather than a
    /// generic document write: the retired `documents` blob has no fallback
    /// path after the SQL-normalization DROP.
    pub manifest: Option<OrganizationManifest>,
}

/// What a completed host transaction did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTxnOutcome {
    /// The journalled intent's id.
    pub action_id: String,
    /// Per-file drift, accumulated across the plan.
    pub drift: DriftReport,
}

/// What the startup recovery pass converged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Intents whose files were restored from backups and whose rows were
    /// closed without advancing anything.
    pub rolled_back: Vec<String>,
    /// Intents replayed forward through commit 2.
    pub rolled_forward: Vec<String>,
    /// `phase = closed` tombstones pruned. Not an error: the normal path
    /// deletes the row as part of commit 2, so one of these is a leftover from
    /// an older build or a future explicit close.
    pub pruned: Vec<String>,
}

impl RecoveryReport {
    /// Whether the pass had anything to do. A clean startup reports `true`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rolled_back.is_empty() && self.rolled_forward.is_empty() && self.pruned.is_empty()
    }
}

/// Failure of a host transaction.
#[derive(Debug, thiserror::Error)]
pub enum HostTxnError {
    /// The company database refused, was busy, or is unusable.
    #[error("host transaction store step failed: {0}")]
    Store(#[from] ChiefdError),
    /// A filesystem or tool step failed. The filesystem has already been
    /// restored from the backup set by the time this is returned.
    #[error("host transaction executor step failed: {0}")]
    Host(#[from] HostErr),
    /// The plan asked to write outside its own root — reported by
    /// [`crate::files::materialize`] as a conflict. Fatal for a host
    /// transaction: a partially applied plan whose remainder can never apply is
    /// not a state to leave behind, so it rolls back.
    #[error("host transaction plan is out of bounds: {paths:?}")]
    OutOfBounds {
        /// The offending relative paths.
        paths: Vec<String>,
    },
    /// A journalled row or backup set could not be understood. Fail-closed: it
    /// is left in place, untouched, for an operator (plan §5.5 — journals are
    /// `FailClosed`).
    #[error("host transaction journal is unreadable: {detail}")]
    Journal {
        /// Which row, and what about it could not be read.
        detail: String,
    },
}

// --- the live path ------------------------------------------------------

/// Run `plan` as a host transaction against `db`.
///
/// `backups` is the directory the per-intent backup sets are written to; the
/// same directory must be handed to [`recover`] at startup.
///
/// # Errors
/// See [`HostTxnError`]. On every error path the filesystem has been restored
/// from the backup set and the intent row closed, so a failed host transaction
/// leaves nothing for the recovery pass to find.
#[tracing::instrument(name = "host.transaction", skip_all, fields(kind = %kind))]
pub async fn run(
    db: &CompanyDb,
    host: &dyn HostExecutor,
    backups: &Path,
    kind: &str,
    plan: &HostTxnPlan,
) -> Result<HostTxnOutcome, HostTxnError> {
    let action_id = uuid::Uuid::new_v4().to_string();
    // A host transaction is two database commits around a filesystem publish,
    // and each of the three can block. The span's exit line carries the whole
    // elapsed time; the `action_id` is what joins it to the journal row a
    // recovery pass will later read.
    tracing::info!(
        event = "host.transaction.start",
        action_id = %action_id,
        files = plan.materialize.files.len(),
        "a host transaction opened"
    );
    let plan_json = serde_json::to_string(plan)
        .map_err(|error| HostTxnError::Journal { detail: format!("plan {error}") })?;

    // --- commit 1 -------------------------------------------------------
    let kind_owned = kind.to_owned();
    let id_for_intent = action_id.clone();
    db.mutate(MutationClass::Small, MutationName("host-txn.intent"), move |ledgers| {
        let record = HostActionRecord::pending(kind_owned, plan_json, ledgers.now());
        ledgers.put_host_action(id_for_intent, record);
        Ok(())
    })
    .await?;
    pause::at(AFTER_INTENT_COMMIT);

    // --- executor phase -------------------------------------------------
    let drift = match publish(host, backups, &action_id, plan) {
        Ok(drift) => drift,
        Err(error) => {
            // Undo whatever was published, then close the intent. The manifest
            // was never advanced, so afterwards the world is exactly as it was
            // before commit 1.
            rollback(backups, &action_id)?;
            close(db, &action_id).await?;
            return Err(error);
        }
    };

    let id_for_mark = action_id.clone();
    db.mutate(MutationClass::Small, MutationName("host-txn.published"), move |ledgers| {
        if ledgers.advance_host_action(&id_for_mark, HostActionPhase::Published) {
            Ok(())
        } else {
            // Unreachable by construction: commit 1 committed before the
            // executor ran. A named refusal rather than a panic, because the
            // only route here is a durable-state bug worth reading about.
            Err(ChiefdError::refused(
                "host-action-missing",
                "the intent row vanished between commit 1 and the executor phase",
            ))
        }
    })
    .await?;
    pause::at(AFTER_PUBLISH);

    // --- commit 2 -------------------------------------------------------
    commit_two(db, &action_id, plan).await?;
    pause::at(AFTER_COMMIT);
    discard_backups(backups, &action_id)?;

    Ok(HostTxnOutcome { action_id, drift })
}

/// Back up everything, then publish one file at a time.
///
/// Per-file rather than one whole-plan call for two reasons: the executor seam
/// stays the thing doing the writing (so a fake can fail at any index —
/// TESTING.md §1.3), and a crash can be injected *between* two files, which is
/// the torn state no amount of single-database atomicity prevents.
fn publish(
    host: &dyn HostExecutor,
    backups: &Path,
    action_id: &str,
    plan: &HostTxnPlan,
) -> Result<DriftReport, HostTxnError> {
    // Every backup is taken and made durable *before* the first publish, and
    // the backup set is published atomically. Its presence therefore means
    // "the backup set is complete"; its absence means "nothing was published".
    // The recovery pass relies on exactly that implication.
    take_backups(backups, action_id, &plan.materialize)?;
    pause::at(AFTER_BACKUPS);

    let mut drift = DriftReport::default();
    for (index, file) in plan.materialize.files.iter().enumerate() {
        let single =
            MaterializePlan { root: plan.materialize.root.clone(), files: vec![file.clone()] };
        let report = host.materialize(&single)?;
        if !report.conflicts.is_empty() {
            return Err(HostTxnError::OutOfBounds { paths: report.conflicts });
        }
        drift.changed.extend(report.changed);
        drift.unchanged.extend(report.unchanged);
        if index == 0 {
            pause::at(AFTER_FIRST_FILE);
        }
    }
    Ok(drift)
}

async fn commit_two(
    db: &CompanyDb,
    action_id: &str,
    plan: &HostTxnPlan,
) -> Result<(), HostTxnError> {
    let manifest = plan.manifest.clone();
    let id = action_id.to_owned();
    db.mutate(MutationClass::Small, MutationName("host-txn.commit"), move |ledgers| {
        if let Some(manifest) = &manifest {
            // The host layer never names a ledger key or store type. It may
            // create the first authority only; later structural changes must
            // arrive through a named normalized organization operation.
            if organization::exists(ledgers) {
                return Err(ChiefdError::refused(
                    "host-txn-manifest-update-retired",
                    "host transactions cannot replace an existing organization manifest",
                ));
            } else {
                organization::create(ledgers, manifest)?;
            }
        }
        ledgers.close_host_action(&id);
        Ok(())
    })
    .await?;
    Ok(())
}

async fn close(db: &CompanyDb, action_id: &str) -> Result<(), HostTxnError> {
    let id = action_id.to_owned();
    db.mutate(MutationClass::Small, MutationName("host-txn.close"), move |ledgers| {
        ledgers.close_host_action(&id);
        Ok(())
    })
    .await?;
    Ok(())
}

// --- the startup recovery pass -----------------------------------------

/// Converge every open `host_actions` intent (plan §5.6, §7.2).
///
/// Runs before the company serves tier-1 traffic. Idempotent: running it twice
/// converges the same way, and running it on a clean database does nothing.
///
/// # Errors
/// [`HostTxnError::Journal`] if a row's plan cannot be parsed — the row is left
/// exactly as it was, because a journal chiefd cannot read is not a journal
/// chiefd may delete.
#[tracing::instrument(name = "host.recover", skip_all)]
pub async fn recover(
    db: &CompanyDb,
    host: &dyn HostExecutor,
    backups: &Path,
) -> Result<RecoveryReport, HostTxnError> {
    // The work list is fixed before any of it is applied, so a recovery step's
    // own commits cannot change what recovery believes it has left to do.
    let open: Vec<(String, HostActionPhase, String)> = db.read(|snapshot| {
        snapshot
            .open_host_actions()
            .into_iter()
            .map(|(id, record)| (id.to_owned(), record.phase(), record.plan_json().to_owned()))
            .collect()
    });

    let mut report = RecoveryReport::default();
    for (action_id, phase, plan_json) in open {
        match phase {
            HostActionPhase::Closed => {
                // Deliberately never parsed: a tombstone must be prunable even
                // if its plan is unreadable, or one bad row wedges startup.
                close(db, &action_id).await?;
                report.pruned.push(action_id);
            }
            HostActionPhase::Pending => {
                // The row cannot say how far publishing got, so undo it. Safe
                // in every case, because commit 2 never ran.
                rollback(backups, &action_id)?;
                close(db, &action_id).await?;
                report.rolled_back.push(action_id);
            }
            HostActionPhase::Published => {
                let plan = parse_plan(&action_id, &plan_json)?;
                // Replay is a convergence, not a redo: an already-published
                // plan reports every file `unchanged` and writes nothing.
                let replayed = host.materialize(&plan.materialize)?;
                if !replayed.conflicts.is_empty() {
                    return Err(HostTxnError::OutOfBounds { paths: replayed.conflicts });
                }
                commit_two(db, &action_id, &plan).await?;
                discard_backups(backups, &action_id)?;
                report.rolled_forward.push(action_id);
            }
        }
    }
    Ok(report)
}

fn parse_plan(action_id: &str, plan_json: &str) -> Result<HostTxnPlan, HostTxnError> {
    serde_json::from_str(plan_json).map_err(|error| HostTxnError::Journal {
        detail: format!("host action '{action_id}' has an unreadable plan: {error}"),
    })
}

// --- the backup set -----------------------------------------------------

/// The pre-transaction state of one planned file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupEntry {
    relative_path: String,
    /// `None` means the file did not exist, and rollback deletes it. That
    /// distinction is load-bearing: "restore nothing" and "delete" are
    /// different operations, and conflating them is how a rolled-back hire
    /// leaves an orphan pi-home behind.
    previous: Option<BackedUpFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackedUpFile {
    contents: String,
    mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupSet {
    root: PathBuf,
    entries: Vec<BackupEntry>,
}

/// Where one intent's backup set lives: a single file, published atomically,
/// so that "the backup set exists" is one durable fact rather than a directory
/// whose completeness has to be inferred.
fn backup_path(backups: &Path, action_id: &str) -> PathBuf {
    backups.join(format!("{action_id}.json"))
}

fn take_backups(
    backups: &Path,
    action_id: &str,
    plan: &MaterializePlan,
) -> Result<(), HostTxnError> {
    let mut entries = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        let target = plan.root.join(&file.relative_path);
        let previous = match std::fs::read_to_string(&target) {
            Ok(contents) => Some(BackedUpFile { contents, mode: mode_of(&target) }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                // Unreadable existing content means the file cannot be backed
                // up, which means a publish over it could not be undone.
                // Refusing before the first write is the only moment at which
                // that is still true.
                return Err(HostErr::Filesystem {
                    detail: format!("cannot back up {}: {error}", target.display()),
                }
                .into());
            }
        };
        entries.push(BackupEntry { relative_path: file.relative_path.clone(), previous });
    }
    let set = BackupSet { root: plan.root.clone(), entries };
    let body = serde_json::to_string(&set)
        .map_err(|error| HostTxnError::Journal { detail: format!("backup set {error}") })?;
    files::publish_atomically(&backup_path(backups, action_id), &body, 0o600)?;
    Ok(())
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path).map_or(0o600, |meta| meta.permissions().mode() & 0o777)
}

/// Restore every file of an intent's backup set, then drop the set.
///
/// A missing backup set is **not** an error: it means the crash happened
/// between commit 1 and the first backup, so nothing was published and there is
/// nothing to undo.
fn rollback(backups: &Path, action_id: &str) -> Result<(), HostTxnError> {
    let path = backup_path(backups, action_id);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(HostErr::Filesystem {
                detail: format!("cannot read backup set {}: {error}", path.display()),
            }
            .into())
        }
    };
    let set: BackupSet = serde_json::from_str(&raw).map_err(|error| HostTxnError::Journal {
        detail: format!("backup set for '{action_id}' is unreadable: {error}"),
    })?;
    for entry in &set.entries {
        let target = set.root.join(&entry.relative_path);
        match &entry.previous {
            Some(previous) => {
                files::publish_atomically(&target, &previous.contents, previous.mode)?;
            }
            None => remove_if_present(&target)?,
        }
    }
    discard_backups(backups, action_id)
}

fn remove_if_present(target: &Path) -> Result<(), HostTxnError> {
    // Seam exception (clippy.toml): filesystem effects belong to chiefd_host.
    // Deleting a file the transaction created is the rollback half of
    // publishing it, and lives at the same boundary.
    #[allow(clippy::disallowed_methods)]
    let removed = std::fs::remove_file(target);
    match removed {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(HostErr::Filesystem {
            detail: format!("cannot roll back {}: {error}", target.display()),
        }
        .into()),
    }
}

fn discard_backups(backups: &Path, action_id: &str) -> Result<(), HostTxnError> {
    remove_if_present(&backup_path(backups, action_id))
}

/// Everything a crash-injection test needs to rebuild the exact state a killed
/// process left behind, without re-deriving it from private internals.
///
/// Public because the crash tests are a separate crate (TESTING.md §4.3) and
/// because the e2e harness (M17) drives the same helpers.
pub mod testing {
    use super::{
        backup_path, take_backups, BackupSet, HostTxnError, HostTxnPlan, MaterializePlan, Path,
    };

    /// Whether an intent's backup set is still on disk.
    #[must_use]
    pub fn backup_set_exists(backups: &Path, action_id: &str) -> bool {
        backup_path(backups, action_id).is_file()
    }

    /// The relative paths an intent's backup set covers, in plan order.
    ///
    /// # Errors
    /// [`HostTxnError::Journal`] if the set cannot be read or parsed.
    pub fn backed_up_paths(backups: &Path, action_id: &str) -> Result<Vec<String>, HostTxnError> {
        let raw = std::fs::read_to_string(backup_path(backups, action_id)).map_err(|error| {
            HostTxnError::Journal { detail: format!("backup set for '{action_id}': {error}") }
        })?;
        let set: BackupSet = serde_json::from_str(&raw).map_err(|error| HostTxnError::Journal {
            detail: format!("backup set for '{action_id}': {error}"),
        })?;
        Ok(set.entries.into_iter().map(|entry| entry.relative_path).collect())
    }

    /// Take the backup set a live transaction would have taken.
    ///
    /// # Errors
    /// As the live path.
    pub fn seed_backups(
        backups: &Path,
        action_id: &str,
        plan: &MaterializePlan,
    ) -> Result<(), HostTxnError> {
        take_backups(backups, action_id, plan)
    }

    /// Serialize a plan exactly as commit 1 journals it.
    ///
    /// # Errors
    /// [`HostTxnError::Journal`] if the plan does not serialize.
    pub fn plan_json(plan: &HostTxnPlan) -> Result<String, HostTxnError> {
        serde_json::to_string(plan)
            .map_err(|error| HostTxnError::Journal { detail: format!("plan {error}") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::MaterializeFile;
    use crate::fake::FakeHostExecutor;
    use chiefd_core::clock::{SharedClock, WallMillis};
    use chiefd_core::store::organization::OrganizationManifest;
    use chiefd_core::store::COMPANY_DB_FILENAME;
    use chiefd_core::test_support::{northstar_manifest, ManualClock};
    use std::sync::{Arc, Mutex};

    struct Harness {
        dir: tempfile::TempDir,
        db: Arc<CompanyDb>,
        host: FakeHostExecutor,
    }

    impl Harness {
        fn with(host: FakeHostExecutor) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let clock: SharedClock = Arc::new(ManualClock::default());
            let db = Arc::new(
                CompanyDb::open("cobalt", &dir.path().join(COMPANY_DB_FILENAME), clock)
                    .expect("open"),
            );
            std::fs::create_dir_all(dir.path().join("backups")).expect("backups dir");
            std::fs::create_dir_all(dir.path().join("tree")).expect("tree dir");
            Self { dir, db, host }
        }

        fn new() -> Self {
            Self::with(FakeHostExecutor::new().with_real_filesystem())
        }

        fn backups(&self) -> PathBuf {
            self.dir.path().join("backups")
        }

        fn root(&self) -> PathBuf {
            self.dir.path().join("tree")
        }

        fn manifest(&self, purpose: &str) -> OrganizationManifest {
            let mut manifest = northstar_manifest(1_784_116_800_000);
            manifest.slug = "cobalt".to_owned();
            manifest.purpose = purpose.to_owned();
            manifest.name = purpose.to_owned();
            let root_id = manifest.root_department_id.clone();
            manifest.departments.get_mut(&root_id).expect("root department").name =
                purpose.to_owned();
            manifest
        }

        fn plan(
            &self,
            files: &[(&str, &str)],
            manifest: Option<OrganizationManifest>,
        ) -> HostTxnPlan {
            HostTxnPlan {
                materialize: MaterializePlan {
                    root: self.root(),
                    files: files
                        .iter()
                        .map(|(path, contents)| MaterializeFile {
                            relative_path: (*path).to_owned(),
                            contents: (*contents).to_owned(),
                            mode: 0o600,
                        })
                        .collect(),
                },
                manifest,
            }
        }

        fn file(&self, relative: &str) -> Option<String> {
            std::fs::read_to_string(self.root().join(relative)).ok()
        }

        fn open_intents(&self) -> Vec<(String, HostActionPhase)> {
            self.db.read(|snapshot| {
                snapshot
                    .open_host_actions()
                    .into_iter()
                    .map(|(id, record)| (id.to_owned(), record.phase()))
                    .collect()
            })
        }

        fn manifest_marker(&self) -> Option<String> {
            self.db.read(|snapshot| organization::read(snapshot).ok().map(|manifest| manifest.name))
        }

        fn leftover_backups(&self) -> Vec<String> {
            std::fs::read_dir(self.backups())
                .expect("readdir")
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        }
    }

    #[tokio::test]
    async fn a_successful_transaction_publishes_advances_and_leaves_no_journal_behind() {
        let h = Harness::new();
        let plan =
            h.plan(&[("people/p1/settings.json", "{\"a\":1}\n")], Some(h.manifest("published")));
        let outcome = run(&h.db, &h.host, &h.backups(), "materialize", &plan).await.expect("run");

        assert_eq!(outcome.drift.changed, vec!["people/p1/settings.json".to_string()]);
        assert_eq!(h.file("people/p1/settings.json").as_deref(), Some("{\"a\":1}\n"));
        assert_eq!(h.manifest_marker().as_deref(), Some("published"));
        assert!(h.open_intents().is_empty(), "commit 2 closes the intent");
        assert!(h.leftover_backups().is_empty(), "a completed transaction leaves no backup set");
    }

    #[tokio::test]
    async fn the_intent_row_exists_before_the_executor_runs_and_names_the_full_plan() {
        // The ordering guarantee of commit 1, asserted from *inside* the
        // executor phase — at the moment the first file has been published.
        // Asserting it afterwards would prove nothing about the ordering.
        let h = Harness::new();
        let plan = h.plan(&[("a.json", "1\n"), ("b.json", "2\n")], Some(h.manifest("pending")));

        /// One mid-publish observation: the open intent's id, phase and
        /// journalled plan, plus whether the manifest had advanced yet.
        type Observation = (String, HostActionPhase, String, bool);
        let observed: Arc<Mutex<Vec<Observation>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&observed);
        let db = Arc::clone(&h.db);
        let b_json = h.root().join("b.json");
        let unpublished = Arc::new(Mutex::new(false));
        let unpublished_sink = Arc::clone(&unpublished);
        pause::install(move |name: &str| {
            if name != AFTER_FIRST_FILE {
                return;
            }
            let rows = db.read(|snapshot| {
                snapshot
                    .open_host_actions()
                    .into_iter()
                    .map(|(id, record)| {
                        (
                            id.to_owned(),
                            record.phase(),
                            record.plan_json().to_owned(),
                            organization::exists(snapshot),
                        )
                    })
                    .collect::<Vec<Observation>>()
            });
            sink.lock().unwrap_or_else(|p| p.into_inner()).extend(rows);
            *unpublished_sink.lock().unwrap_or_else(|p| p.into_inner()) = !b_json.exists();
        });
        run(&h.db, &h.host, &h.backups(), "materialize", &plan).await.expect("run");
        pause::uninstall();

        // Exact duplicates are collapsed before counting. What invariant 8
        // claims is a property of the *durable state* at that instant — the
        // set of open host actions is exactly one — not of how many times the
        // observer was invoked. Counting invocations made this assertion fail
        // roughly one run in twenty when a sibling test's transaction fired a
        // process-global pause hook and drove a second, byte-identical read of
        // this test's own database. The seam is thread-scoped now so that
        // cannot happen; this states the invariant the way it is actually
        // meant, so a stray extra invocation can never resurrect the flake.
        //
        // It is not a weaker assertion. Two genuinely distinct open intents
        // still differ in id and still fail here, and a single intent observed
        // once with the manifest advanced and once without differs in the last
        // field and also still fails — which is exactly the tear invariant 8
        // forbids.
        let mut rows = observed.lock().unwrap_or_else(|p| p.into_inner()).clone();
        rows.dedup();
        rows.sort();
        rows.dedup();
        assert!(!rows.is_empty(), "the mid-publish observation must have happened at all");
        assert_eq!(rows.len(), 1, "exactly one intent is open mid-publish: {rows:?}");
        assert_eq!(rows[0].1, HostActionPhase::Pending, "still pending during the executor phase");
        let journalled: HostTxnPlan = serde_json::from_str(&rows[0].2).expect("plan round-trips");
        assert_eq!(journalled, plan, "the *full* plan is journalled, not a summary");
        assert!(
            !rows[0].3,
            "invariant 8: the manifest has not advanced while a file is unpublished"
        );
        assert!(
            *unpublished.lock().unwrap_or_else(|p| p.into_inner()),
            "…and b.json really was still unpublished at that instant"
        );
    }

    #[tokio::test]
    async fn the_observer_detects_a_second_open_intent_when_one_genuinely_exists() {
        // THE ANTI-VACUITY PROOF, and the reason it exists: the fix above
        // scopes the pause hook to the installing thread. Scoping a hook
        // wrongly does not make a test fail — it makes the hook stop firing,
        // so the test passes forever and checks nothing. A vacuous test is
        // strictly worse than a flaky one, because a flake is visible.
        //
        // So this is the positive control for
        // `the_intent_row_exists_before_the_executor_runs_and_names_the_full_plan`:
        // same observer, same pause point, but invariant 8 is genuinely
        // broken beforehand by planting a second open intent — exactly what a
        // previously crashed transaction leaves behind. If the observer is
        // alive, it sees two. If someone scopes the seam wrongly later, this
        // test goes quiet in the same way the other one would, and it says so
        // rather than passing.
        let h = Harness::new();
        let leftover = "leftover-from-a-crashed-transaction".to_string();
        let stale_plan =
            serde_json::to_string(&h.plan(&[("stale.json", "0\n")], None)).expect("json");
        let planted = leftover.clone();
        h.db.mutate(MutationClass::Small, MutationName("test.plant-intent"), move |ledgers| {
            let record =
                HostActionRecord::pending("materialize".to_string(), stale_plan, ledgers.now());
            ledgers.put_host_action(planted, record);
            Ok(())
        })
        .await
        .expect("plant a leftover intent");

        let plan = h.plan(&[("a.json", "1\n"), ("b.json", "2\n")], Some(h.manifest("pending")));
        let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&observed);
        let db = Arc::clone(&h.db);
        pause::install(move |name: &str| {
            if name != AFTER_FIRST_FILE {
                return;
            }
            let ids = db.read(|snapshot| {
                snapshot
                    .open_host_actions()
                    .into_iter()
                    .map(|(id, _)| id.to_owned())
                    .collect::<Vec<String>>()
            });
            sink.lock().unwrap_or_else(|p| p.into_inner()).extend(ids);
        });
        run(&h.db, &h.host, &h.backups(), "materialize", &plan).await.expect("run");
        pause::uninstall();

        // The seam fired on this thread at all — the specific failure mode a
        // wrongly-scoped hook produces.
        assert!(
            pause::installed_names().iter().any(|name| name == AFTER_FIRST_FILE),
            "the pause point was never reached: the hook is not firing, so every assertion              made through it is vacuous"
        );

        let mut ids = observed.lock().unwrap_or_else(|p| p.into_inner()).clone();
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            2,
            "the observer must SEE a genuine second open intent — this is what proves the              sibling test's `exactly one` assertion can still fail: {ids:?}"
        );
        assert!(ids.contains(&leftover), "including the planted one: {ids:?}");
    }

    #[tokio::test]
    async fn an_out_of_bounds_plan_rolls_back_and_publishes_nothing() {
        let h = Harness::new();
        let outside = h.dir.path().join("outside.json");
        let plan = h.plan(
            &[("ok.json", "written\n"), ("../outside.json", "escaped\n")],
            Some(h.manifest("published")),
        );
        let error =
            run(&h.db, &h.host, &h.backups(), "materialize", &plan).await.expect_err("refused");
        assert!(matches!(error, HostTxnError::OutOfBounds { .. }), "got {error:?}");

        assert!(!outside.exists(), "an escaping entry is never written");
        assert!(h.file("ok.json").is_none(), "the in-bounds file was rolled back");
        assert!(h.manifest_marker().is_none(), "the manifest never advanced");
        assert!(h.open_intents().is_empty(), "a failed transaction leaves no open intent");
        assert!(h.leftover_backups().is_empty());
    }

    #[tokio::test]
    async fn a_failing_executor_rolls_the_earlier_files_back_to_their_previous_bytes() {
        // Seed a pre-existing file so rollback must *restore*, not delete.
        let h = Harness::new();
        let seeded = h.plan(&[("a.json", "original\n")], None);
        run(&h.db, &h.host, &h.backups(), "seed", &seeded).await.expect("seed");

        // Call 0 publishes a.json for real; call 1 fails — the torn state.
        let h = Harness { host: FakeHostExecutor::failing_at(1).with_real_filesystem(), ..h };
        let plan = h.plan(&[("a.json", "new\n"), ("b.json", "new\n")], Some(h.manifest("pending")));
        let error = run(&h.db, &h.host, &h.backups(), "materialize", &plan)
            .await
            .expect_err("executor failed");
        assert!(matches!(error, HostTxnError::Host(_)), "got {error:?}");

        assert_eq!(h.file("a.json").as_deref(), Some("original\n"), "restored byte-for-byte");
        assert!(h.file("b.json").is_none(), "a file the plan created is deleted, not left empty");
        assert!(h.manifest_marker().is_none());
        assert!(h.open_intents().is_empty());
        assert!(h.leftover_backups().is_empty());
    }

    #[tokio::test]
    async fn recovery_on_a_clean_database_does_nothing() {
        let h = Harness::new();
        let report = recover(&h.db, &h.host, &h.backups()).await.expect("recover");
        assert!(report.is_empty());
    }

    #[tokio::test]
    async fn recovery_is_idempotent_after_a_roll_forward() {
        let h = Harness::new();
        let plan = h.plan(&[("a.json", "published\n")], Some(h.manifest("published")));
        // The state a process killed after `AFTER_PUBLISH` leaves behind. (The
        // real SIGKILL version is `tests/host_txn_crash.rs`; this pins the
        // convergence itself.)
        h.host.materialize(&plan.materialize).expect("publish");
        let plan_json = serde_json::to_string(&plan).expect("json");
        h.db.mutate(MutationClass::Small, MutationName("test.seed"), move |ledgers| {
            ledgers.put_host_action(
                "act-1",
                HostActionRecord::pending("materialize", plan_json, ledgers.now()),
            );
            ledgers.advance_host_action("act-1", HostActionPhase::Published);
            Ok(())
        })
        .await
        .expect("seed");

        let first = recover(&h.db, &h.host, &h.backups()).await.expect("first");
        assert_eq!(first.rolled_forward, vec!["act-1".to_string()]);
        assert_eq!(h.manifest_marker().as_deref(), Some("published"));
        let manifest = h.db.read(|s| organization::read(s).expect("manifest"));

        let second = recover(&h.db, &h.host, &h.backups()).await.expect("second");
        assert!(second.is_empty(), "a converged database has nothing left to recover");
        assert_eq!(
            h.db.read(|s| organization::read(s).expect("manifest")),
            manifest,
            "replay must not alter the normalized organization rows"
        );
    }

    #[tokio::test]
    async fn an_unreadable_plan_fails_closed_and_leaves_the_row_in_place() {
        let h = Harness::new();
        h.db.mutate(MutationClass::Small, MutationName("test.seed"), |ledgers| {
            ledgers.put_host_action(
                "act-bad",
                HostActionRecord::pending("materialize", "{}", ledgers.now()),
            );
            ledgers.advance_host_action("act-bad", HostActionPhase::Published);
            Ok(())
        })
        .await
        .expect("seed");

        let error = recover(&h.db, &h.host, &h.backups()).await.expect_err("fails closed");
        assert!(matches!(error, HostTxnError::Journal { .. }), "got {error:?}");
        assert_eq!(
            h.open_intents(),
            vec![("act-bad".to_string(), HostActionPhase::Published)],
            "a journal chiefd cannot read is not a journal chiefd may delete"
        );
    }

    #[tokio::test]
    async fn a_closed_tombstone_is_pruned_rather_than_replayed() {
        let h = Harness::new();
        h.db.mutate(MutationClass::Small, MutationName("test.seed"), |ledgers| {
            ledgers.put_host_action(
                "act-old",
                // Deliberately unparseable: pruning must not depend on the
                // plan, or one bad tombstone wedges startup forever.
                HostActionRecord::pending("materialize", "{\"nonsense\":true}", ledgers.now()),
            );
            ledgers.advance_host_action("act-old", HostActionPhase::Closed);
            Ok(())
        })
        .await
        .expect("seed");

        let report = recover(&h.db, &h.host, &h.backups()).await.expect("prune");
        assert_eq!(report.pruned, vec!["act-old".to_string()]);
        assert!(h.open_intents().is_empty());
    }

    #[tokio::test]
    async fn open_intents_recover_in_the_order_they_were_created() {
        let h = Harness::new();
        let root = h.root();
        h.db.mutate(MutationClass::Small, MutationName("test.seed"), move |ledgers| {
            let empty = serde_json::json!({
                "materialize": { "root": root, "files": [] },
                "manifest": null
            })
            .to_string();
            for (id, created) in [("z-first", 10_i64), ("a-second", 20)] {
                ledgers.put_host_action(
                    id,
                    HostActionRecord::pending("materialize", empty.clone(), WallMillis(created)),
                );
                ledgers.advance_host_action(id, HostActionPhase::Published);
            }
            Ok(())
        })
        .await
        .expect("seed");

        let report = recover(&h.db, &h.host, &h.backups()).await.expect("recover");
        assert_eq!(
            report.rolled_forward,
            vec!["z-first".to_string(), "a-second".to_string()],
            "creation order, not id order"
        );
    }

    #[test]
    fn a_missing_backup_set_is_not_an_error_because_nothing_was_published() {
        let dir = tempfile::tempdir().expect("tempdir");
        rollback(dir.path(), "never-started").expect("nothing to undo");
    }

    #[test]
    fn a_corrupt_backup_set_fails_closed_rather_than_being_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        files::publish_atomically(&backup_path(dir.path(), "act-1"), "not json", 0o600)
            .expect("seed");
        let error = rollback(dir.path(), "act-1").expect_err("fails closed");
        assert!(matches!(error, HostTxnError::Journal { .. }), "got {error:?}");
        assert!(backup_path(dir.path(), "act-1").exists(), "the evidence is kept");
    }

    #[test]
    fn a_backup_set_records_absence_distinctly_from_emptiness() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("tree");
        files::publish_atomically(&root.join("present.json"), "", 0o600).expect("seed empty file");
        let plan = MaterializePlan {
            root: root.clone(),
            files: ["present.json", "absent.json"]
                .into_iter()
                .map(|path| MaterializeFile {
                    relative_path: path.to_owned(),
                    contents: "x".into(),
                    mode: 0o600,
                })
                .collect(),
        };
        take_backups(dir.path(), "act-1", &plan).expect("backups");
        let raw =
            std::fs::read_to_string(backup_path(dir.path(), "act-1")).expect("read backup set");
        let set: BackupSet = serde_json::from_str(&raw).expect("parse");
        assert_eq!(
            set.entries[0].previous,
            Some(BackedUpFile { contents: String::new(), mode: 0o600 })
        );
        assert_eq!(set.entries[1].previous, None, "absent is not the empty string");

        // And rollback acts on the difference.
        files::publish_atomically(&root.join("present.json"), "x", 0o600).expect("publish");
        files::publish_atomically(&root.join("absent.json"), "x", 0o600).expect("publish");
        rollback(dir.path(), "act-1").expect("rollback");
        assert_eq!(std::fs::read_to_string(root.join("present.json")).expect("restored"), "");
        assert!(!root.join("absent.json").exists(), "a created file is deleted");
    }
}
