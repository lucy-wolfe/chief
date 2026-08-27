//! Write-then-verify exports — **M11 (plan §8 Phase 2.2)**.
//!
//! During Phase 2 the old JSON file stops being authority and becomes an
//! *export*: chiefd rewrites it after each commit so a rollback has something
//! to roll back to. That inverts the danger. An authoritative file that
//! changes underneath you is a conflict; an **export** that changes underneath
//! you means something else still believes it owns the store — the exact
//! condition that produced the earlier split state, where two stores failed
//! differently and diverged.
//!
//! So the plan's rule is:
//!
//! > Exports are write-then-verify: dev/ino/mtime/content-hash recorded; a
//! > changed-underneath file is a **hard alarm and a stop**, never a blind
//! > overwrite.
//!
//! This module implements that as a two-call protocol:
//!
//! 1. [`ExportWitness::observe`] records what is on disk now.
//! 2. [`publish`] re-observes, compares against the witness, and only then
//!    publishes by rename — and re-reads the published bytes to prove they are
//!    the bytes intended.
//!
//! The witness is deliberately *four* facts, not one. A content hash alone
//! misses a same-content replacement by another writer (which still proves a
//! second owner exists); dev/ino alone miss an in-place rewrite; mtime alone
//! is coarse and forgeable. Together they answer "is this still the file I
//! last wrote, untouched?" — which is the question that matters.
//!
//! # What "stop" means
//!
//! [`ExportError::ChangedUnderneath`] is not retried, not logged-and-continued
//! and not resolved by overwriting. It is returned to the operator with every
//! observed difference named, because the correct response is to find the
//! other writer, not to try again.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What was on disk at the moment the exporter looked.
///
/// Serializable so a cutover run can persist the witness between invocations:
/// the trailing rollback window (plan §8 Phase 2.5) can last days, and "the
/// file I wrote last Tuesday" must still be checkable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportWitness {
    /// Absent means the export file did not exist — a legitimate first write.
    pub present: bool,
    /// `st_dev` of the observed file.
    pub dev: u64,
    /// `st_ino` of the observed file.
    pub ino: u64,
    /// `st_mtime_nsec`-resolution modification time, as nanoseconds since the
    /// epoch.
    pub mtime_nanos: i128,
    /// Size in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of the contents.
    pub sha256: String,
}

impl ExportWitness {
    /// Observe `path` right now.
    ///
    /// # Errors
    /// [`ExportError::Io`] when the file exists but cannot be read.
    pub fn observe(path: &Path) -> Result<Self, ExportError> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    present: false,
                    dev: 0,
                    ino: 0,
                    mtime_nanos: 0,
                    size: 0,
                    sha256: String::new(),
                });
            }
            Err(error) => return Err(io("lstat", path, error)),
        };
        if metadata.file_type().is_symlink() {
            // An export path that has become a symlink is itself proof of
            // interference: chiefd only ever publishes regular files there.
            return Err(ExportError::NotARegularFile { path: path.display().to_string() });
        }
        let bytes = fs::read(path).map_err(|error| io_err("read", path, error))?;
        Ok(Self {
            present: true,
            dev: metadata.dev(),
            ino: metadata.ino(),
            mtime_nanos: i128::from(metadata.mtime()) * 1_000_000_000
                + i128::from(metadata.mtime_nsec()),
            size: metadata.size(),
            sha256: hex_sha256(&bytes),
        })
    }

    /// Every way `other` differs from `self`, in operator-readable form.
    #[must_use]
    pub fn differences(&self, other: &Self) -> Vec<String> {
        let mut out = Vec::new();
        if self.present != other.present {
            out.push(format!("present {} -> {}", self.present, other.present));
        }
        if self.present && other.present {
            if self.dev != other.dev || self.ino != other.ino {
                out.push(format!(
                    "identity {}:{} -> {}:{} (the file was replaced, not edited)",
                    self.dev, self.ino, other.dev, other.ino
                ));
            }
            if self.mtime_nanos != other.mtime_nanos {
                out.push(format!("mtime {} -> {}", self.mtime_nanos, other.mtime_nanos));
            }
            if self.size != other.size {
                out.push(format!("size {} -> {}", self.size, other.size));
            }
            if self.sha256 != other.sha256 {
                out.push(format!("sha256 {} -> {}", short(&self.sha256), short(&other.sha256)));
            }
        }
        out
    }
}

fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

/// Why an export stopped.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// **The hard alarm.** Something other than chiefd wrote the export file
    /// between the witness and the publish.
    #[error(
        "STOP: export '{path}' changed underneath chiefd ({differences}). \
         Another writer still believes it owns this store. Do not re-run the export: \
         find the writer (a pre-deploy supervisor, a CLI subprocess on an old launcher.json, \
         or a loaded extension copy), stop it, and re-verify ownership \
         before continuing."
    )]
    ChangedUnderneath {
        /// The export path.
        path: String,
        /// Comma-joined differences.
        differences: String,
    },
    /// The published bytes are not the bytes that were requested.
    #[error("STOP: export '{path}' read back different bytes than were written (wrote {wrote}, read {read})")]
    ReadBackMismatch {
        /// The export path.
        path: String,
        /// Hash of the intended contents.
        wrote: String,
        /// Hash of what came back.
        read: String,
    },
    /// The export path is not a regular file.
    #[error("STOP: export '{path}' is not a regular file; chiefd only ever publishes regular files there")]
    NotARegularFile {
        /// The export path.
        path: String,
    },
    /// Filesystem failure.
    #[error("export {op} '{path}': {source}")]
    Io {
        /// What was being attempted.
        op: &'static str,
        /// Path involved.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

fn io(op: &'static str, path: &Path, source: std::io::Error) -> ExportError {
    ExportError::Io { op, path: path.display().to_string(), source }
}

fn io_err(op: &'static str, path: &Path, source: std::io::Error) -> ExportError {
    io(op, path, source)
}

/// A completed export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Published {
    /// Where it landed.
    pub path: PathBuf,
    /// Bytes written.
    pub bytes: usize,
    /// Hash of the published contents.
    pub sha256: String,
    /// Witness of the file as published — the input to the *next* export's
    /// change check.
    pub witness: ExportWitness,
}

/// Publish `contents` to `path`, refusing if the file changed since `witness`.
///
/// Sequence: re-observe → compare → stage in the same directory → fsync →
/// rename → re-read → compare. The staging file is a sibling so the rename is
/// same-filesystem and therefore atomic; a reader never sees a half-written
/// export.
///
/// # Errors
/// [`ExportError::ChangedUnderneath`] when the witness no longer matches (the
/// hard stop), [`ExportError::ReadBackMismatch`] when the published bytes
/// differ from the intended ones, [`ExportError::Io`] otherwise.
pub fn publish(
    path: &Path,
    contents: &str,
    mode: u32,
    witness: &ExportWitness,
) -> Result<Published, ExportError> {
    let observed = ExportWitness::observe(path)?;
    let differences = witness.differences(&observed);
    if !differences.is_empty() {
        return Err(ExportError::ChangedUnderneath {
            path: path.display().to_string(),
            differences: differences.join(", "),
        });
    }

    let directory = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory).map_err(|error| io("mkdir", directory, error))?;
    let staging = directory.join(format!(
        ".{}.export-{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("export"),
        uuid::Uuid::new_v4()
    ));

    let write = (|| -> Result<(), ExportError> {
        #[allow(clippy::disallowed_types)]
        let mut file = {
            #[allow(clippy::disallowed_types)]
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true).mode(mode);
            opts.open(&staging).map_err(|error| io("create-staging", &staging, error))?
        };
        file.write_all(contents.as_bytes())
            .map_err(|error| io("write-staging", &staging, error))?;
        file.sync_all().map_err(|error| io("fsync-staging", &staging, error))?;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|error| io("chmod-staging", &staging, error))
    })();
    if let Err(error) = write {
        remove(&staging);
        return Err(error);
    }

    // Publish-by-rename is a host step, never an ad-hoc call — this module is
    // the host crate, and the staging/rename pair is the step (README §5.6).
    #[allow(clippy::disallowed_methods)]
    if let Err(error) = fs::rename(&staging, path) {
        remove(&staging);
        return Err(io("rename", path, error));
    }

    let expected = hex_sha256(contents.as_bytes());
    let published = ExportWitness::observe(path)?;
    if published.sha256 != expected {
        return Err(ExportError::ReadBackMismatch {
            path: path.display().to_string(),
            wrote: expected,
            read: published.sha256,
        });
    }
    Ok(Published {
        path: path.to_path_buf(),
        bytes: contents.len(),
        sha256: expected,
        witness: published,
    })
}

fn remove(path: &Path) {
    #[allow(clippy::disallowed_methods)]
    let _ = fs::remove_file(path);
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
// These tests deliberately act as the OTHER writer — the one the seam exists
// to exclude. Simulating an interfering process is the only way to test a
// defence against one, so the seam lints are lifted for the test module alone.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn first_export_of_an_absent_file_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("health-monitor.json");
        let witness = ExportWitness::observe(&path).unwrap();
        assert!(!witness.present);

        let published = publish(&path, "{\"a\":1}\n", 0o600, &witness).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        assert_eq!(published.witness.sha256, published.sha256);
        assert_eq!(fs::symlink_metadata(&path).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_second_export_over_chiefds_own_previous_export_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let first =
            publish(&path, "one\n", 0o600, &ExportWitness::observe(&path).unwrap()).unwrap();
        // The witness chiefd carries forward is the one it just published.
        let second = publish(&path, "two\n", 0o600, &first.witness).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        assert_ne!(first.sha256, second.sha256);
    }

    #[test]
    fn a_file_changed_underneath_is_a_hard_stop_naming_the_differences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("supervision.json");
        let first =
            publish(&path, "chiefd\n", 0o600, &ExportWitness::observe(&path).unwrap()).unwrap();

        // A separate writer edits the export — the split-state precondition.
        fs::write(&path, "someone else\n").unwrap();

        let error = publish(&path, "chiefd again\n", 0o600, &first.witness)
            .expect_err("a changed export must stop the cutover");
        let message = error.to_string();
        assert!(message.starts_with("STOP:"), "{message}");
        assert!(message.contains("sha256"), "the diagnostic names what changed: {message}");
        assert!(message.contains("Another writer still believes it owns this store"), "{message}");
        // And it must NOT have overwritten the other writer's bytes.
        assert_eq!(fs::read_to_string(&path).unwrap(), "someone else\n");
    }

    #[test]
    fn same_content_replacement_by_another_writer_is_still_caught() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("health-monitor.json");
        let first =
            publish(&path, "same\n", 0o600, &ExportWitness::observe(&path).unwrap()).unwrap();

        // Byte-identical, but a different inode: something else is writing
        // here, which is the fact that matters even though content agrees.
        let sibling = dir.path().join("other");
        fs::write(&sibling, "same\n").unwrap();
        fs::rename(&sibling, &path).unwrap();

        let error = publish(&path, "next\n", 0o600, &first.witness).expect_err("identity change");
        assert!(error.to_string().contains("the file was replaced, not edited"), "{error}");
    }

    #[test]
    fn deletion_underneath_is_caught_rather_than_silently_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity.json");
        let first = publish(&path, "a\n", 0o600, &ExportWitness::observe(&path).unwrap()).unwrap();
        fs::remove_file(&path).unwrap();

        let error = publish(&path, "b\n", 0o600, &first.witness).expect_err("deletion is a change");
        assert!(error.to_string().contains("present true -> false"), "{error}");
    }

    #[test]
    fn a_symlinked_export_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere.json");
        fs::write(&target, "x\n").unwrap();
        let path = dir.path().join("artifact.json");
        std::os::unix::fs::symlink(&target, &path).unwrap();

        let error = ExportWitness::observe(&path).expect_err("symlinks are refused");
        assert!(matches!(error, ExportError::NotARegularFile { .. }), "{error}");
    }

    #[test]
    fn a_multi_megabyte_export_round_trips() {
        // Cobalt's supervision ledger is 4.4 MB; an export path that only
        // works for small documents is the same class of bug as the 2 MB body
        // cap that shipped in Phase 1.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("supervision.json");
        let big = "x".repeat(5 * 1024 * 1024);
        let published =
            publish(&path, &big, 0o600, &ExportWitness::observe(&path).unwrap()).unwrap();
        assert_eq!(published.bytes, big.len());
        assert_eq!(fs::read_to_string(&path).unwrap().len(), big.len());
    }

    #[test]
    fn no_staging_files_survive_a_successful_export() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        publish(&path, "a\n", 0o600, &ExportWitness::observe(&path).unwrap()).unwrap();
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".export-"))
            .collect();
        assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");
    }
}
