//! Filesystem effects: publish-by-rename and materialization.
//!
//! `clippy.toml` bans `std::fs::write`, `std::fs::rename`, `std::fs::File` and
//! `std::fs::OpenOptions` everywhere except this crate's executor — the seam
//! that stops a handler from quietly re-creating the multi-writer world chiefd
//! exists to delete. The `#[allow]`s below are therefore the *only* ones in
//! the workspace, each at the exact call site, as `clippy.toml` prescribes.
//!
//! The file invariants ported here (plan §4 table):
//!
//! * **Never unlink first, never publish through an old symlink** (inv 30):
//!   every publish writes a sibling temp file and `rename(2)`s it over the
//!   target. The target is replaced atomically; an existing symlink at the
//!   target is *replaced*, never followed, so a stale link can never redirect
//!   a write to another company's file.
//! * **0600 for anything with a credential in it** (inv 32) — the mode is set
//!   at `open(2)` time, not chmod'ed afterwards, so the file is never briefly
//!   world-readable.
//! * **Idempotent and replayable** (plan §5.6): materialization compares
//!   content first and reports drift; running the same plan twice changes
//!   nothing the second time.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::executor::{DriftReport, HostErr, MaterializePlan};

/// Publish `contents` at `path` atomically, with `mode`.
///
/// # Errors
/// [`HostErr::Filesystem`] on any step; the target is left untouched unless
/// the final rename succeeded.
pub fn publish_atomically(path: &Path, contents: &str, mode: u32) -> Result<(), HostErr> {
    let parent = path.parent().ok_or_else(|| HostErr::Filesystem {
        detail: format!("{} has no parent directory", path.display()),
    })?;
    std::fs::create_dir_all(parent).map_err(|error| HostErr::Filesystem {
        detail: format!("cannot create {}: {error}", parent.display()),
    })?;
    let temp = parent.join(format!(".chiefd-{}.tmp", uuid::Uuid::new_v4()));
    write_new(&temp, contents, mode)?;
    // rename(2), not unlink-then-create: the target never blinks out of
    // existence, and an existing symlink is replaced rather than written
    // through (invariant 30).
    #[allow(clippy::disallowed_methods)]
    let renamed = std::fs::rename(&temp, path);
    renamed.map_err(|error| {
        #[allow(clippy::disallowed_methods)]
        let _ = std::fs::remove_file(&temp);
        HostErr::Filesystem { detail: format!("cannot publish {}: {error}", path.display()) }
    })
}

/// Create `path` holding `contents` at `mode`, ONCE, refusing to replace a file
/// that is already there. Returns whether THIS call created it.
///
/// The difference from [`publish_atomically`] is the whole point:
/// `rename(2)` replaces, and a replaced identity key orphans the public half
/// already enrolled in the trust table. `link(2)` fails `EEXIST` instead, and
/// it does so in ONE syscall — so two writers that both passed an existence
/// check still end with one file, and the loser can tell that it lost.
///
/// It goes through a sibling temp rather than opening the destination with
/// `O_EXCL` directly, so a concurrent reader never sees a half-written
/// credential: the name appears only once the bytes are on disk and synced.
///
/// `link(2)` and not `renameat2(RENAME_NOREPLACE)`: the latter is Linux-only
/// and this repo must build and behave identically on macOS.
///
/// # Errors
/// [`HostErr::Filesystem`] on any step except the `EEXIST` that means somebody
/// else won, which is `Ok(false)`.
pub fn create_exclusively(path: &Path, contents: &str, mode: u32) -> Result<bool, HostErr> {
    let parent = path.parent().ok_or_else(|| HostErr::Filesystem {
        detail: format!("{} has no parent directory", path.display()),
    })?;
    std::fs::create_dir_all(parent).map_err(|error| HostErr::Filesystem {
        detail: format!("cannot create {}: {error}", parent.display()),
    })?;
    let temp = parent.join(format!(".chiefd-{}.tmp", uuid::Uuid::new_v4()));
    write_new(&temp, contents, mode)?;
    let linked = std::fs::hard_link(&temp, path);
    // The temp name always goes; the link, if it was made, keeps the inode.
    #[allow(clippy::disallowed_methods)]
    let _ = std::fs::remove_file(&temp);
    match linked {
        Ok(()) => Ok(true),
        // Somebody else got there first. Their file stands, untouched.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(HostErr::Filesystem {
            detail: format!("cannot create {}: {error}", path.display()),
        }),
    }
}

/// Publish one file atomically through a descriptor-pinned existing directory.
///
/// Unlike [`publish_atomically`], this refuses a symlink at the parent path and
/// keeps the opened directory inode pinned through temp creation and rename.
/// A concurrent rename/symlink swap of the pathname therefore cannot redirect
/// a credential into another person's home. The target name itself is still
/// replaced atomically, so an existing target symlink is replaced rather than
/// followed.
///
/// # Errors
/// [`HostErr::Filesystem`] if the parent is missing/untrusted or any
/// open/write/sync/rename step fails.
pub fn publish_atomically_in_existing_directory(
    path: &Path,
    contents: &str,
    mode: u32,
) -> Result<(), HostErr> {
    publish_in_existing_directory(path, contents, mode, false)
}

/// Publish through a descriptor-pinned existing directory only when bytes or
/// permissions differ. The trusted-parent rules are identical to
/// [`publish_atomically_in_existing_directory`], including on a no-op pass.
///
/// # Errors
/// [`HostErr::Filesystem`] if the parent is missing/untrusted, the current
/// regular file cannot be inspected, or publication fails.
pub fn publish_atomically_if_changed_in_existing_directory(
    path: &Path,
    contents: &str,
    mode: u32,
) -> Result<(), HostErr> {
    publish_in_existing_directory(path, contents, mode, true)
}

fn publish_in_existing_directory(
    path: &Path,
    contents: &str,
    mode: u32,
    skip_unchanged: bool,
) -> Result<(), HostErr> {
    use nix::fcntl::{open, openat, renameat, OFlag};
    use nix::sys::stat::{mode_t, Mode};
    use nix::unistd::{unlinkat, UnlinkatFlags};
    use std::os::unix::fs::PermissionsExt;

    let parent = path.parent().ok_or_else(|| HostErr::Filesystem {
        detail: format!("{} has no parent directory", path.display()),
    })?;
    let name = path.file_name().ok_or_else(|| HostErr::Filesystem {
        detail: format!("{} has no file name", path.display()),
    })?;
    let directory = open(
        parent,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| HostErr::Filesystem {
        detail: format!("cannot open trusted directory {}: {error}", parent.display()),
    })?;
    if skip_unchanged {
        match openat(
            &directory,
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) {
            Ok(descriptor) => {
                #[allow(clippy::disallowed_types)]
                let mut current = std::fs::File::from(descriptor);
                let metadata = current.metadata().map_err(|error| HostErr::Filesystem {
                    detail: format!("cannot inspect {}: {error}", path.display()),
                })?;
                if metadata.is_file() {
                    let mut bytes = Vec::new();
                    current.read_to_end(&mut bytes).map_err(|error| HostErr::Filesystem {
                        detail: format!("cannot read {}: {error}", path.display()),
                    })?;
                    if bytes == contents.as_bytes() && metadata.permissions().mode() & 0o777 == mode
                    {
                        return Ok(());
                    }
                }
            }
            Err(nix::errno::Errno::ENOENT | nix::errno::Errno::ELOOP) => {}
            Err(error) => {
                return Err(HostErr::Filesystem {
                    detail: format!("cannot inspect {}: {error}", path.display()),
                });
            }
        }
    }
    let temporary = format!(".chiefd-{}.tmp", uuid::Uuid::new_v4());
    let descriptor = openat(
        &directory,
        temporary.as_str(),
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        // `mode_t` is `u16` on macOS and `u32` on Linux, so `Mode` (a
        // `libc_bitflags!` over `mode_t`) takes a different integer width per
        // target. The `mode` we carry is the `std::os::unix` `u32` form of the
        // permission bits; casting through `mode_t` keeps both targets happy
        // while truncating only the bits `Mode` never stores.
        Mode::from_bits_truncate(mode as mode_t),
    )
    .map_err(|error| HostErr::Filesystem {
        detail: format!("cannot create private temporary in {}: {error}", parent.display()),
    })?;
    #[allow(clippy::disallowed_types)]
    let mut file = std::fs::File::from(descriptor);
    let prepared = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(mode)))
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = prepared {
        let _ = unlinkat(&directory, temporary.as_str(), UnlinkatFlags::NoRemoveDir);
        return Err(HostErr::Filesystem {
            detail: format!("cannot prepare private file in {}: {error}", parent.display()),
        });
    }
    if let Err(error) = renameat(&directory, temporary.as_str(), &directory, name) {
        let _ = unlinkat(&directory, temporary.as_str(), UnlinkatFlags::NoRemoveDir);
        return Err(HostErr::Filesystem {
            detail: format!("cannot publish {}: {error}", path.display()),
        });
    }
    Ok(())
}

/// Remove one file through a descriptor-pinned existing directory.
///
/// This is the stale-secret counterpart to
/// [`publish_atomically_in_existing_directory`]: the parent must be a real
/// directory, and a concurrent pathname swap cannot redirect deletion into a
/// different person's home.
///
/// # Errors
/// [`HostErr::Filesystem`] if the parent is missing/untrusted or an existing
/// target cannot be removed.
pub fn remove_file_if_exists_in_existing_directory(path: &Path) -> Result<(), HostErr> {
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::Mode;
    use nix::unistd::{unlinkat, UnlinkatFlags};

    let parent = path.parent().ok_or_else(|| HostErr::Filesystem {
        detail: format!("{} has no parent directory", path.display()),
    })?;
    let name = path.file_name().ok_or_else(|| HostErr::Filesystem {
        detail: format!("{} has no file name", path.display()),
    })?;
    let directory = open(
        parent,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| HostErr::Filesystem {
        detail: format!("cannot open trusted directory {}: {error}", parent.display()),
    })?;
    match unlinkat(&directory, name, UnlinkatFlags::NoRemoveDir) {
        Ok(()) | Err(nix::errno::Errno::ENOENT) => Ok(()),
        Err(error) => Err(HostErr::Filesystem {
            detail: format!("cannot remove {}: {error}", path.display()),
        }),
    }
}

/// Remove one managed file if it exists.
///
/// This is deliberately non-recursive: a stale credential target may be a
/// symlink, in which case `remove_file` removes the link itself rather than
/// following it. A missing target is already the desired state.
///
/// # Errors
/// [`HostErr::Filesystem`] when an existing target cannot be removed.
pub fn remove_file_if_exists(path: &Path) -> Result<(), HostErr> {
    #[allow(clippy::disallowed_methods)]
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(HostErr::Filesystem {
            detail: format!("cannot remove {}: {error}", path.display()),
        }),
    }
}

/// Create a private directory (and any missing parents) with `mode`.
///
/// Mirrors [`write_new`]'s discipline for directories: creation applies
/// `mode` (subject to the process umask, exactly like `mkdir(2)`), then the
/// mode is pinned exactly by an explicit `set_permissions` so a freshly
/// created private directory's permissions are a property of the plan, never
/// of the caller's shell — the directory analogue of invariant 32.
///
/// # Errors
/// [`HostErr::Filesystem`] if any directory in the path cannot be created.
pub fn create_private_directory(path: &Path, mode: u32) -> Result<(), HostErr> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(mode);
    builder.create(path).map_err(|error| HostErr::Filesystem {
        detail: format!("cannot create directory {}: {error}", path.display()),
    })?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|error| {
        HostErr::Filesystem { detail: format!("cannot set mode on {}: {error}", path.display()) }
    })
}

/// Recursively remove a directory if it exists. Missing is already the
/// desired state — the directory analogue of [`remove_file_if_exists`].
///
/// # Errors
/// [`HostErr::Filesystem`] when an existing directory cannot be removed.
pub fn remove_directory_if_exists(path: &Path) -> Result<(), HostErr> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(HostErr::Filesystem {
            detail: format!("cannot remove directory {}: {error}", path.display()),
        }),
    }
}

/// The one `std::fs::rename` call site in this module's directory-replacing
/// primitive. Kept as a named wrapper so the seam's `#[allow]` stays on a
/// single greppable line even though [`rename_replacing`] renames up to three
/// times (clippy.toml: "every such site greppable and reviewable").
#[allow(clippy::disallowed_methods)]
fn rename_raw(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// Rename `from` to `to`, replacing any existing target.
///
/// The generic `rename(2)` primitive behind [`publish_atomically`]'s
/// temp-file swap; a caller that stages a whole prepared directory (rather
/// than a single file) uses this directly for the same atomic-replace
/// guarantee — the target is replaced, never unlinked first (invariant 30).
///
/// Two POSIX asymmetries have to be paid for here. `rename(2)` replaces an
/// existing FILE destination unconditionally, but replaces an existing
/// DIRECTORY destination only when that directory is EMPTY; a populated one
/// fails `ENOTEMPTY` (POSIX also permits `EEXIST`, and Linux spells it 39
/// while Darwin spells it 66 — both surface as the same `std::io::ErrorKind`,
/// which is why the check is on the kind, never on a raw errno number). And it
/// refuses outright to replace a target of the OTHER kind: a directory onto a
/// file is `ENOTDIR`, a file onto a directory is `EISDIR`. A bare rename
/// therefore cannot honour this function's contract for a staged tree, which is
/// exactly the caller it exists for.
///
/// So any destination `rename(2)` declines to overwrite takes the standard
/// swap-and-delete: displace the live target to a sibling temp name, rename the
/// staged tree into place, then drop the displaced target. Ordering is chosen
/// for the failure modes, not for elegance:
///
/// * The displacement is a SIBLING (same parent, therefore same filesystem),
///   so it is itself a rename and cannot fail for lack of space.
/// * The two renames are adjacent, so the window in which `to` does not exist
///   is two syscalls wide and contains no I/O.
/// * If the second rename fails, the displaced tree is renamed straight back:
///   the destination ends up valid and holding its ORIGINAL content, and the
///   error is reported. A failed replace never leaves a hole where the live
///   directory was.
/// * The displaced tree is removed only AFTER the destination is known good,
///   and that removal is best-effort — residue is recoverable, a missing
///   destination is not.
///
/// # Errors
/// [`HostErr::Filesystem`] if the rename fails.
pub fn rename_replacing(from: &Path, to: &Path) -> Result<(), HostErr> {
    let failed = |error: &std::io::Error| HostErr::Filesystem {
        detail: format!("cannot rename {} to {}: {error}", from.display(), to.display()),
    };
    let error = match rename_raw(from, to) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    use std::io::ErrorKind::{AlreadyExists, DirectoryNotEmpty, IsADirectory, NotADirectory};
    if !matches!(error.kind(), DirectoryNotEmpty | AlreadyExists | IsADirectory | NotADirectory) {
        return Err(failed(&error));
    }
    let (Some(parent), Some(name)) = (to.parent(), to.file_name()) else {
        return Err(failed(&error));
    };
    // The `<name>.superseded-<uuid>` shape is deliberate, not decorative: it is
    // the residue name `materialize::skills::reconcile_disk_skills` already
    // sweeps, so a crash between the two renames leaves a tree the next
    // reconcile reclaims instead of an orphan nothing owns.
    let name = name.to_string_lossy();
    let displaced = parent.join(format!("{name}.superseded-{}", uuid::Uuid::new_v4()));
    rename_raw(to, &displaced).map_err(|error| HostErr::Filesystem {
        detail: format!("cannot displace {} to {}: {error}", to.display(), displaced.display()),
    })?;
    match rename_raw(from, to) {
        Ok(()) => {
            // The destination is already correct; residue is best-effort. The
            // displaced target may be of either kind, because a type mismatch
            // is one of the reasons a rename declines to overwrite.
            let _ = remove_directory_if_exists(&displaced)
                .or_else(|_| remove_file_if_exists(&displaced));
            Ok(())
        }
        Err(error) => {
            // Put the original tree back so the destination is never a hole.
            let _ = rename_raw(&displaced, to);
            Err(failed(&error))
        }
    }
}

/// Append one line (`contents` plus a trailing `\n`) to `path`, creating the
/// file (and its parent directory) with `mode` if it does not exist yet.
///
/// Unlike [`publish_atomically`] this is **not** a rename-based replace: the
/// worker-family JSONL logs it exists for (`exceptions.jsonl`) are
/// accumulating logs,
/// not a single desired-state document, so there is no "whole content" to
/// publish atomically. The mode is still applied at `open(2)` time so a
/// pre-existing file is never widened and a fresh one is never briefly
/// world-readable (invariant 32).
///
/// # Errors
/// [`HostErr::Filesystem`] if the parent cannot be created or the write fails.
pub fn append_line(path: &Path, contents: &str, mode: u32) -> Result<(), HostErr> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or_else(|| HostErr::Filesystem {
        detail: format!("{} has no parent directory", path.display()),
    })?;
    std::fs::create_dir_all(parent).map_err(|error| HostErr::Filesystem {
        detail: format!("cannot create {}: {error}", parent.display()),
    })?;
    #[allow(clippy::disallowed_types)]
    let mut file =
        std::fs::OpenOptions::new().append(true).create(true).mode(mode).open(path).map_err(
            |error| HostErr::Filesystem {
                detail: format!("cannot open {} for append: {error}", path.display()),
            },
        )?;
    file.write_all(contents.as_bytes()).and_then(|()| file.write_all(b"\n")).map_err(|error| {
        HostErr::Filesystem { detail: format!("cannot append to {}: {error}", path.display()) }
    })
}

/// Append one already-serialized record with exactly one `O_APPEND` write.
///
/// The caller owns framing (including a trailing newline) and a bounded record
/// size. This is separate from [`append_line`], whose content/newline
/// convenience contract uses two writes for existing worker logs. A
/// cross-process observer needs one append operation so its records cannot be
/// interleaved with another writer's record.
///
/// # Errors
/// [`HostErr::Filesystem`] if opening, writing, or completing the one record
/// fails. A short write is reported rather than repaired with a second write.
pub fn append_record_once(path: &Path, record: &[u8]) -> Result<(), HostErr> {
    #[allow(clippy::disallowed_types)]
    let mut file =
        std::fs::OpenOptions::new().append(true).create(true).open(path).map_err(|error| {
            HostErr::Filesystem {
                detail: format!("cannot open {} for append: {error}", path.display()),
            }
        })?;
    let written = file.write(record).map_err(|error| HostErr::Filesystem {
        detail: format!("cannot append to {}: {error}", path.display()),
    })?;
    if written != record.len() {
        return Err(HostErr::Filesystem {
            detail: format!(
                "short append to {}: wrote {written} of {} bytes",
                path.display(),
                record.len()
            ),
        });
    }
    Ok(())
}

/// The identity fields that let a bounded passive reader detect log rotation.
///
/// They are strings deliberately: the native widths of `dev` and `ino` differ
/// between Linux and macOS, while the health cursor's durable wire format is
/// textual on both platforms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedFileMetadata {
    /// Device identity of the opened file.
    pub device: String,
    /// Inode identity of the opened file.
    pub inode: String,
    /// Byte size captured from the same open descriptor.
    pub size: u64,
    /// Permission and file-kind bits captured from the same descriptor.
    pub mode: u32,
    /// Last content modification time captured from the same descriptor.
    pub modified: std::time::SystemTime,
    /// Inode-change time seconds captured from the same descriptor.
    pub changed_seconds: i64,
    /// Inode-change time nanoseconds captured from the same descriptor.
    pub changed_nanoseconds: i64,
}

/// A descriptor-pinned, read-only filesystem observation.
///
/// The host gatherers need a bounded range reader to retain the log cursor's
/// rotation semantics. Keep the raw file handle inside this filesystem seam:
/// callers receive only its identity and an exact range read, never a general
/// purpose descriptor that could grow an ad-hoc filesystem authority.
pub struct ObservedFile {
    #[allow(clippy::disallowed_types)]
    file: std::fs::File,
    metadata: ObservedFileMetadata,
}

impl ObservedFile {
    /// Open one file for a passive bounded observation.
    ///
    /// # Errors
    /// Returns the underlying filesystem error, including `NotFound`, so a
    /// caller can distinguish an absent optional log from a failed observation.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;

        #[allow(clippy::disallowed_types)]
        let file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        Ok(Self {
            metadata: ObservedFileMetadata {
                device: metadata.dev().to_string(),
                inode: metadata.ino().to_string(),
                size: metadata.len(),
                mode: metadata.mode(),
                modified: metadata.modified()?,
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
            },
            file,
        })
    }

    /// Open a regular file below `root` through a descriptor-pinned directory
    /// walk. A symlink can select a contained target, but no pathname swap can
    /// redirect the final descriptor outside the opened root.
    pub fn open_contained(path: &Path, root: &Path) -> std::io::Result<Option<Self>> {
        use nix::fcntl::{open, openat, OFlag};
        use nix::sys::stat::Mode;
        use std::os::unix::fs::MetadataExt as _;
        use std::path::Component;

        let root = std::fs::canonicalize(root)?;
        let path = match std::fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let relative = path.strip_prefix(&root).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "file escapes trusted root")
        })?;
        let parts: Vec<_> = relative
            .components()
            .filter_map(|part| match part {
                Component::Normal(part) => Some(part),
                _ => None,
            })
            .collect();
        if parts.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "trusted file has no relative name",
            ));
        }
        let io_error = |error: nix::errno::Errno| std::io::Error::from_raw_os_error(error as i32);
        let mut directory = open(
            &root,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)?;
        for part in &parts[..parts.len() - 1] {
            directory = openat(
                &directory,
                *part,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(io_error)?;
        }
        let descriptor = openat(
            &directory,
            parts[parts.len() - 1],
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)?;
        #[allow(clippy::disallowed_types)]
        let file = std::fs::File::from(descriptor);
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "trusted source is not a regular file",
            ));
        }
        Ok(Some(Self {
            metadata: ObservedFileMetadata {
                device: metadata.dev().to_string(),
                inode: metadata.ino().to_string(),
                size: metadata.len(),
                mode: metadata.mode(),
                modified: metadata.modified()?,
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
            },
            file,
        }))
    }

    /// The identity sampled from the descriptor at open time.
    #[must_use]
    pub fn metadata(&self) -> &ObservedFileMetadata {
        &self.metadata
    }

    /// Read exactly `bytes` from `start` through the opened descriptor.
    ///
    /// # Errors
    /// The underlying seek/read failure. A short file is an error rather than
    /// a fabricated truncated observation.
    pub fn read_range(&mut self, start: u64, bytes: usize) -> std::io::Result<Vec<u8>> {
        use std::io::{Read as _, Seek as _, SeekFrom};

        self.file.seek(SeekFrom::Start(start))?;
        let mut contents = vec![0; bytes];
        self.file.read_exact(&mut contents)?;
        Ok(contents)
    }
}

/// Read exactly `N` bytes of kernel entropy.
///
/// `read_exact`, not `std::fs::read`: `/dev/urandom` never reaches EOF, so
/// reading it to the end is an unbounded read that fills memory until the
/// process is killed. Living in this module keeps the file handle on the owner
/// side of the seam.
///
/// # Errors
/// [`HostErr::Filesystem`] when the kernel entropy source cannot be read —
/// which no caller may paper over with a constant.
pub fn read_entropy<const N: usize>() -> Result<[u8; N], HostErr> {
    use std::io::Read as _;
    #[allow(clippy::disallowed_types)]
    let mut source = std::fs::File::open("/dev/urandom").map_err(|error| HostErr::Filesystem {
        detail: format!("cannot open /dev/urandom: {error}"),
    })?;
    let mut bytes = [0_u8; N];
    source.read_exact(&mut bytes).map_err(|error| HostErr::Filesystem {
        detail: format!("cannot read {N} bytes of entropy: {error}"),
    })?;
    Ok(bytes)
}

/// Create a brand-new file at `path` with `mode`, failing if one already
/// exists (`O_CREAT|O_EXCL`) — the primitive [`publish_atomically`] stages its
/// temp file with, and one a caller that owns a fresh, private staging
/// directory (guaranteed to have no pre-existing entries) can use directly
/// without a temp-file-plus-rename dance of its own.
///
/// # Errors
/// [`HostErr::Filesystem`] if the file already exists or the write/mode/sync
/// steps fail.
pub fn write_new(path: &Path, contents: &str, mode: u32) -> Result<(), HostErr> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    // The mode is applied by `open(2)`; a chmod after the fact leaves a window
    // in which a credential file is world-readable (invariant 32).
    #[allow(clippy::disallowed_types)]
    let mut file =
        std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(path).map_err(
            |error| HostErr::Filesystem {
                detail: format!("cannot create {}: {error}", path.display()),
            },
        )?;
    file.write_all(contents.as_bytes()).map_err(|error| HostErr::Filesystem {
        detail: format!("cannot write {}: {error}", path.display()),
    })?;
    // `open(2)` applies the umask, so the file so far is *at most* `mode` —
    // never wider, so no credential was ever exposed. Pinning it exactly makes
    // the published mode a property of the plan rather than of the operator's
    // shell.
    file.set_permissions(std::fs::Permissions::from_mode(mode)).map_err(|error| {
        HostErr::Filesystem { detail: format!("cannot set mode on {}: {error}", path.display()) }
    })?;
    file.sync_all().map_err(|error| HostErr::Filesystem {
        detail: format!("cannot sync {}: {error}", path.display()),
    })
}

/// Apply a materialization plan. Idempotent: identical content is not
/// rewritten and is not reported as changed.
///
/// A file whose relative path escapes the plan root is a **conflict**, not an
/// error and never a write: the plan is data, and a plan that tries to leave
/// its root must not be able to reach another company's tree.
///
/// # Errors
/// [`HostErr::Filesystem`] if a legitimate write fails.
pub fn materialize(plan: &MaterializePlan) -> Result<DriftReport, HostErr> {
    let mut report = DriftReport::default();
    for file in &plan.files {
        let Some(target) = contained_path(&plan.root, &file.relative_path) else {
            report.conflicts.push(file.relative_path.clone());
            continue;
        };
        if let Ok(existing) = std::fs::read_to_string(&target) {
            if existing == file.contents && mode_of(&target) == Some(file.mode) {
                report.unchanged.push(file.relative_path.clone());
                continue;
            }
        }
        publish_atomically(&target, &file.contents, file.mode)?;
        report.changed.push(file.relative_path.clone());
    }
    Ok(report)
}

fn mode_of(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path).ok().map(|meta| meta.permissions().mode() & 0o777)
}

/// `root` joined with `relative`, or `None` if the result would escape `root`.
///
/// THE containment rule for this crate. `pub(crate)` rather than private
/// because `materialize` needs exactly this question answered and kept a
/// second copy of the answer while it could not reach this one.
pub(crate) fn contained_path(root: &Path, relative: &str) -> Option<PathBuf> {
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return None;
    }
    let mut out = root.to_path_buf();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => out.push(part),
            // `.` is harmless but `..`, a root, or a prefix is an escape.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out == root {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::MaterializeFile;
    use std::os::unix::fs::PermissionsExt;

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("read")
    }

    #[test]
    fn observed_file_keeps_one_identity_and_reads_only_the_requested_range() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("observed.log");
        publish_atomically(&path, "abcdef", 0o600).expect("fixture");

        let mut observed = ObservedFile::open(&path).expect("open");
        let metadata = observed.metadata();
        assert_eq!(metadata.size, 6);
        assert!(!metadata.device.is_empty());
        assert!(!metadata.inode.is_empty());
        assert_eq!(observed.read_range(2, 3).expect("bounded read"), b"cde");
    }

    #[test]
    fn a_publish_replaces_a_symlink_instead_of_writing_through_it() {
        // Invariant 30: never publish through an old symlink. If the rename
        // followed the link, the *victim* file would be overwritten and the
        // target would still be a link — the failure mode that let one
        // company's catalog write into another's.
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("victim.json");
        publish_atomically(&victim, "victim contents", 0o644).expect("seed");
        let target = dir.path().join("catalog.json");
        std::os::unix::fs::symlink(&victim, &target).expect("symlink");

        publish_atomically(&target, "new contents", 0o644).expect("publish");

        assert_eq!(read(&victim), "victim contents", "the symlink target is untouched");
        assert_eq!(read(&target), "new contents");
        assert!(
            !std::fs::symlink_metadata(&target).expect("meta").file_type().is_symlink(),
            "the symlink itself was replaced"
        );
    }

    #[test]
    fn a_descriptor_pinned_publish_refuses_a_symlinked_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("other-person-pi-home");
        std::fs::create_dir(&victim).expect("victim directory");
        let redirected = dir.path().join("selected-person-pi-home");
        std::os::unix::fs::symlink(&victim, &redirected).expect("parent symlink");

        let target = redirected.join("auth.json");
        assert!(publish_atomically_in_existing_directory(&target, "secret", 0o600).is_err());
        assert!(
            !victim.join("auth.json").exists(),
            "a parent symlink cannot redirect the descriptor-pinned publish"
        );
    }

    #[test]
    fn a_descriptor_pinned_stale_remove_refuses_a_symlinked_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("other-person-pi-home");
        std::fs::create_dir(&victim).expect("victim directory");
        publish_atomically(&victim.join("auth.json"), "victim", 0o600).expect("victim auth");
        let redirected = dir.path().join("selected-person-pi-home");
        std::os::unix::fs::symlink(&victim, &redirected).expect("parent symlink");

        assert!(remove_file_if_exists_in_existing_directory(&redirected.join("auth.json")).is_err());
        assert_eq!(read(&victim.join("auth.json")), "victim");
    }

    #[test]
    fn a_descriptor_pinned_publish_replaces_the_target_atomically_at_0600() {
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("victim.json");
        publish_atomically(&victim, "victim", 0o600).expect("victim");
        let target = dir.path().join("auth.json");
        std::os::unix::fs::symlink(&victim, &target).expect("target symlink");

        publish_atomically_in_existing_directory(&target, "selected", 0o600).expect("publish");

        assert_eq!(read(&victim), "victim");
        assert_eq!(read(&target), "selected");
        assert!(!std::fs::symlink_metadata(&target).expect("meta").file_type().is_symlink());
        assert_eq!(std::fs::metadata(target).expect("meta").permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_publish_never_unlinks_the_target_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("catalog.json");
        publish_atomically(&target, "v1", 0o644).expect("first");
        let first_inode = inode(&target);
        publish_atomically(&target, "v2", 0o644).expect("second");
        assert_eq!(read(&target), "v2");
        assert_ne!(first_inode, inode(&target), "publish is a rename of a fresh inode");
    }

    fn inode(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).expect("meta").ino()
    }

    #[test]
    fn credential_files_are_created_0600_not_chmodded_afterwards() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("provider.env");
        publish_atomically(&target, "ANTHROPIC_API_KEY=sk-x\n", 0o600).expect("publish");
        let mode = std::fs::metadata(&target).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "invariant 32");
    }

    #[test]
    fn append_line_creates_the_file_and_parent_directory_with_the_given_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("nested").join("settled.jsonl");
        append_line(&target, "{\"a\":1}", 0o600).expect("append");
        assert_eq!(read(&target), "{\"a\":1}\n");
        let mode = std::fs::metadata(&target).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn append_line_adds_to_an_existing_file_rather_than_replacing_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("playbook.md");
        append_line(&target, "first", 0o644).expect("first append");
        append_line(&target, "second", 0o644).expect("second append");
        assert_eq!(read(&target), "first\nsecond\n");
    }

    #[test]
    fn append_record_once_preserves_each_preframed_record_exactly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("probe.jsonl");
        append_record_once(&target, b"{\"first\":1}\n").expect("first append");
        append_record_once(&target, b"{\"second\":2}\n").expect("second append");
        assert_eq!(std::fs::read(&target).expect("read"), b"{\"first\":1}\n{\"second\":2}\n");
    }

    #[test]
    fn no_temp_files_are_left_behind_after_a_successful_publish() {
        let dir = tempfile::tempdir().expect("tempdir");
        publish_atomically(&dir.path().join("a"), "x", 0o644).expect("publish");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".chiefd-"))
            .collect();
        assert!(leftovers.is_empty(), "left {leftovers:?}");
    }

    /// The primitive an identity anchor rests on, from both directions on one
    /// fixture. A create must produce the bytes at exactly the mode; a SECOND
    /// create must report that it did not create, and must leave the first
    /// file byte-for-byte — the property `publish_atomically` cannot have,
    /// because `rename(2)` replaces.
    #[test]
    fn create_exclusively_creates_once_and_never_replaces() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        // Nested: the first provision of a key runs before its parents exist.
        let target = dir.path().join("agent").join("quant-head").join("key.pem");

        assert!(create_exclusively(&target, "first", 0o600).expect("create"));
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "first");
        assert_eq!(
            std::fs::metadata(&target).expect("meta").permissions().mode() & 0o777,
            0o600,
            "a credential is owner-only from the first byte"
        );

        assert!(
            !create_exclusively(&target, "second", 0o600).expect("second create"),
            "the loser reports that it did not create the file"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read"),
            "first",
            "the anchor that already existed is untouched"
        );
    }

    /// A refused create leaves nothing behind either — a temp file per losing
    /// pass would accumulate one credential-shaped file per race, in the
    /// directory a person's Pi reads.
    #[test]
    fn create_exclusively_leaves_no_temp_file_whether_it_wins_or_loses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("key.pem");
        assert!(create_exclusively(&target, "first", 0o600).expect("create"));
        assert!(!create_exclusively(&target, "second", 0o600).expect("second"));

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".chiefd-"))
            .collect();
        assert!(leftovers.is_empty(), "left {leftovers:?}");
    }

    fn plan(root: &Path, files: Vec<MaterializeFile>) -> MaterializePlan {
        MaterializePlan { root: root.to_path_buf(), files }
    }

    fn file(path: &str, contents: &str) -> MaterializeFile {
        MaterializeFile {
            relative_path: path.to_owned(),
            contents: contents.to_owned(),
            mode: 0o644,
        }
    }

    #[test]
    fn materialize_is_idempotent_the_second_time_changes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = plan(dir.path(), vec![file("people/p1/settings.json", "{}\n")]);
        let first = materialize(&plan).expect("first");
        assert_eq!(first.changed, vec!["people/p1/settings.json".to_string()]);
        assert!(first.unchanged.is_empty());

        let second = materialize(&plan).expect("replay");
        assert!(second.changed.is_empty(), "a replay must not rewrite identical content");
        assert_eq!(second.unchanged, vec!["people/p1/settings.json".to_string()]);
        assert!(second.conflicts.is_empty());
    }

    #[test]
    fn materialize_reports_drift_when_content_changed_underneath() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = plan(dir.path(), vec![file("a.json", "expected\n")]);
        materialize(&plan).expect("first");
        publish_atomically(&dir.path().join("a.json"), "edited by hand\n", 0o644).expect("edit");
        let report = materialize(&plan).expect("second");
        assert_eq!(report.changed, vec!["a.json".to_string()]);
        assert_eq!(read(&dir.path().join("a.json")), "expected\n");
    }

    #[test]
    fn a_plan_entry_escaping_its_root_is_a_conflict_and_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = dir.path().join("outside.json");
        let root = dir.path().join("root");
        let plan = plan(
            &root,
            vec![
                file("../outside.json", "escaped"),
                file("/etc/passwd", "escaped"),
                file("ok.json", "fine"),
            ],
        );
        let report = materialize(&plan).expect("materialize");
        assert_eq!(
            report.conflicts,
            vec!["../outside.json".to_string(), "/etc/passwd".to_string()]
        );
        assert_eq!(report.changed, vec!["ok.json".to_string()]);
        assert!(!outside.exists(), "an escaping entry is never written");
    }

    // --- create_private_directory / remove_directory_if_exists / rename_replacing ---

    #[test]
    fn create_private_directory_creates_missing_parents_at_exactly_the_given_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("a").join("b").join("staging");
        create_private_directory(&target, 0o700).expect("create");
        assert!(target.is_dir());
        let mode = std::fs::metadata(&target).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "the mode is pinned exactly, not left to the umask");
    }

    #[test]
    fn remove_directory_if_exists_is_a_genuine_no_op_on_an_absent_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("never-existed");
        remove_directory_if_exists(&missing).expect("missing is already the desired state");
    }

    #[test]
    fn remove_directory_if_exists_removes_a_populated_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("populated");
        create_private_directory(&target, 0o700).expect("create");
        publish_atomically(&target.join("SKILL.md"), "content", 0o600).expect("seed file");
        remove_directory_if_exists(&target).expect("remove");
        assert!(!target.exists());
    }

    #[test]
    fn rename_replacing_atomically_replaces_an_existing_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("staged");
        create_private_directory(&from, 0o700).expect("create staged");
        publish_atomically(&from.join("SKILL.md"), "new", 0o600).expect("seed staged");
        let to = dir.path().join("live");
        create_private_directory(&to, 0o700).expect("create live");
        publish_atomically(&to.join("SKILL.md"), "old", 0o600).expect("seed live");

        rename_replacing(&from, &to).expect("rename");

        assert!(!from.exists(), "the staged source is gone after the rename");
        assert_eq!(
            read(&to.join("SKILL.md")),
            "new",
            "the live target now holds the staged content"
        );

        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read parent")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["live".to_string()], "the displaced tree is not left behind");
    }

    #[test]
    fn rename_replacing_leaves_the_destination_intact_when_the_source_is_absent() {
        // The swap must never be entered speculatively: only a genuine
        // "destination is a populated directory" failure displaces the live
        // tree. Any other rename failure returns with the destination
        // untouched, never as a hole where the live directory was.
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("never-staged");
        let to = dir.path().join("live");
        create_private_directory(&to, 0o700).expect("create live");
        publish_atomically(&to.join("SKILL.md"), "old", 0o600).expect("seed live");

        let error =
            rename_replacing(&missing, &to).expect_err("an absent source cannot be published");

        assert!(error.to_string().contains("cannot rename"), "{error}");
        assert!(to.is_dir(), "the destination is never left absent");
        assert_eq!(read(&to.join("SKILL.md")), "old", "the destination keeps its original content");
    }

    #[test]
    fn rename_replacing_displaces_a_target_of_the_other_kind_too() {
        // `rename(2)` refuses to overwrite across kinds — a directory onto a
        // file is ENOTDIR, a file onto a directory is EISDIR — as firmly as it
        // refuses a populated directory. All three are the same answer ("I will
        // not overwrite this"), so all three take the displace-aside swap;
        // otherwise a caller that dropped its own `remove_dir_all` would lose
        // the ability to publish a resource whose kind changed. Matched by
        // `io::ErrorKind`, never by errno number.
        let dir = tempfile::tempdir().expect("tempdir");

        let staged_file = dir.path().join("staged-file");
        publish_atomically(&staged_file, "new", 0o600).expect("seed");
        let live_directory = dir.path().join("live-directory");
        create_private_directory(&live_directory, 0o700).expect("create");
        publish_atomically(&live_directory.join("SKILL.md"), "old", 0o600).expect("seed");
        rename_replacing(&staged_file, &live_directory).expect("a file replaces a directory");
        assert_eq!(read(&live_directory), "new");

        let staged_directory = dir.path().join("staged-directory");
        create_private_directory(&staged_directory, 0o700).expect("create");
        publish_atomically(&staged_directory.join("SKILL.md"), "new", 0o600).expect("seed");
        let live_file = dir.path().join("live-file");
        publish_atomically(&live_file, "old", 0o600).expect("seed");
        rename_replacing(&staged_directory, &live_file).expect("a directory replaces a file");
        assert_eq!(read(&live_file.join("SKILL.md")), "new");

        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read parent")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec!["live-directory".to_string(), "live-file".to_string()],
            "a displaced target of EITHER kind is cleaned up, not left as residue"
        );
    }
}
