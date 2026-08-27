//! What survives of the pi-home MATERIALIZER: an error type, the atomic
//! publish primitives, and the launcher asset roots.
//!
//! # TOMBSTONE — `materialize_organization` and everything it drove
//!
//! This module reprojected every person's pi-home from SQL on EVERY launch,
//! hire, start and department-create: a five-directory tree per person, skill
//! copies behind a fail-closed safety scan, extension closures rewritten
//! file-by-file, `packages/` wiped and rebuilt, a write-once `settings.json`,
//! `trust.json`, a reload hard contract, and a durable per-person checkpoint
//! the launch gate then re-validated. Even a no-op pass rewrote `AGENTS.md`,
//! deleted `auth.json`, and advanced every mtime the drift detector read.
//!
//! All of it is replaced by [`crate::agent_home::ensure_agent_home`] —
//! create-if-absent, never modify — because nothing in an agent home is a
//! projection of SQL any more: skills are a symlink to the company's own
//! `.pi/skills`, credentials and defaults are symlinks into the operator's own
//! Pi agent dir, and `AGENTS.md` is the hire-time contract that goes stale ON
//! PURPOSE. With no projection there is nothing to keep in sync, so there is
//! nothing to re-run, nothing to stage, nothing to checkpoint and nothing to
//! probe for staleness. `StagedMaterialization`, `MaterializeReport`,
//! `PersonFailurePolicy`, `Catalog`, the skills/settings/extensions/reload
//! modules and the `POST /v1/org/materialize/*` route family all went with it.
//!
//! # Why this module still exists at all
//!
//! Three things outlived their subject, and each has a live consumer that is
//! not a materializer:
//!
//! * [`MaterializeError`] and [`publish_text`] — `agent_home` writes through
//!   them, so the one atomic-publish seam `clippy.toml` permits stays one seam.
//! * [`LauncherAssets`] — where the checkout's own code lives, resolved once by
//!   `runtime_lifecycle::launcher_assets`.
//! * [`extension_source_digest`] — the input that makes `desired_launch_hash`
//!   catch a launcher DEPLOY. It hashes the CHECKOUT, never a per-home copy,
//!   which is precisely why it survives a change that deletes every copy.

use std::path::{Path, PathBuf};

use chiefd_core::error::Refusal;
use chiefd_core::hexdigest::hex_digest;
use chiefd_core::store::organization::{EmploymentState, PersonRecord};

use crate::executor::HostErr;

pub mod plan;

// --- refusal codes ---------------------------------------------------------
//
// TOMBSTONE: eighteen codes stood here. Every one of them named a way a
// PROJECTION could be wrong — an unresolvable resource reference, an unsafe
// skill tree, a broken extension source, malformed root settings, an
// unroutable provider. Nothing is projected, so none of them has a subject
// left. What remains is the two that describe a failed WRITE, plus the accent
// allocator's own exhaustion, which is a derivation rather than a projection.

/// A person's identity accent could not be allocated.
pub const ACCENT_EXHAUSTED: &str = "accent-exhausted";
/// A filesystem step failed.
pub const MATERIALIZE_FILESYSTEM: &str = "materialize-filesystem";
/// A host step failed.
pub const MATERIALIZE_HOST: &str = "materialize-host";

/// Everything writing an agent home can refuse or fail on.
///
/// Deliberate policy declines carry a [`Refusal`] (the shape the store layer
/// already turns into a 4xx with `legalRoutes`); everything else is a typed
/// variant. Every variant answers [`MaterializeError::code`], so a caller can
/// branch on the machine code without string-matching a message.
#[derive(Debug, thiserror::Error)]
pub enum MaterializeError {
    /// A deliberate decline: the plan was legal to ask for and was refused.
    #[error("refused: {}: {}", .0.code, .0.message)]
    Refused(Refusal),
    /// A filesystem step failed (create, publish, remove, read).
    #[error("materialization filesystem step failed: {detail}")]
    Filesystem {
        /// What failed, with the path.
        detail: String,
    },
    /// A host primitive (atomic publish, managed removal) failed.
    #[error("materialization host step failed: {0}")]
    Host(#[from] HostErr),
}

impl MaterializeError {
    /// The stable machine code for this failure.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Refused(refusal) => refusal.code,
            Self::Filesystem { .. } => MATERIALIZE_FILESYSTEM,
            Self::Host(_) => MATERIALIZE_HOST,
        }
    }

    /// Build a deliberate refusal.
    pub(crate) fn refuse(code: &'static str, message: impl Into<String>) -> Self {
        Self::Refused(Refusal::new(code, message))
    }

    /// Build a filesystem failure.
    pub(crate) fn filesystem(detail: impl Into<String>) -> Self {
        Self::Filesystem { detail: detail.into() }
    }
}

// --- launcher assets -------------------------------------------------------

/// Where the launcher's own code lives.
///
/// A daemon has no module-scope ambient checkout, so these are an explicit
/// input — which also makes every test hermetic.
///
/// TOMBSTONE: `piing_skills_root`, `source_pi_home`, `workspace_imports` and
/// `LauncherAssets::skill` were fields and a method of this struct. Chief
/// copies no skill and rewrites no import: skills reach an agent through one
/// symlink to the company's own `.pi/skills`, and the operator's own Pi home
/// reaches it through three more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherAssets {
    /// The launcher checkout root (`launcherRoot`).
    pub launcher_root: PathBuf,
}

/// The shipped organization extensions every agent pane loads, in argv order.
///
/// The Founder has a different fixed set. Agent panes get only these four:
/// loading the whole directory would also load `founder-launch.ts`.
///
/// `company-stop.ts` is here and NOT in the Founder's set on purpose. It
/// registers `/stop`, which tears this company down; before genesis there is no
/// company to tear down, so offering the command to the Founder would be
/// offering a verb with no object.
pub const ORGANIZATION_EXTENSION_FILES: [&str; 4] =
    ["organization-intercom.ts", "team-ui.ts", "tribes-welcome.ts", "company-stop.ts"];

/// Resolve the exact shipped organization extension paths from the launcher
/// checkout root.
#[must_use]
pub fn organization_extension_paths(launcher_root: &Path) -> Vec<PathBuf> {
    let root = launcher_root.join("packages/piing/extensions");
    ORGANIZATION_EXTENSION_FILES.iter().map(|name| root.join(name)).collect()
}

/// Whether a person is employable at all — kept beside the launcher assets
/// because callers building a candidate roster need the same predicate the
/// manifest uses.
#[must_use]
pub fn is_employed(person: &PersonRecord) -> bool {
    !matches!(person.employment_state, EmploymentState::Departed)
}

// --- extension source digest -----------------------------------------------

/// A digest of the launcher extension SOURCE this daemon would launch panes
/// against.
///
/// # Why this exists, and what breaks without it
///
/// It is the input that makes `desired_launch_hash` catch a deploy. A launcher
/// deploy rewrites extension code on disk and changes no person row at all, so
/// a hash over rows misses it completely — and `runtime_lifecycle.rs` records
/// what that cost: *"a whole fleet came up running old code and reported
/// success."*
///
/// The old answer was a SCAN: chiefd joined the actuator's observation against
/// mtimes and told an operator who to restart. That scan is deleted with the
/// observation, and every tombstone for it rests on this function existing —
/// the digest moves the hash, the hash moves the pane tag, and the actuator
/// replaces the stale pane without anybody being told. A missing producer here
/// would make all of those tombstones false and reopen the incident silently,
/// which is exactly the failure mode this whole fence is built against.
///
/// # What is hashed, and why not the bytes
///
/// Every extension file that agent argv loads, with its PATH and
/// `(len, mtime_ms)`, in the same fixed order. Deliberately not the file
/// contents:
///
/// * this runs on the `desired` read path, which the actuator polls, so it must
///   not read the whole extension tree on every pass;
/// * a deploy replaces files, so length-or-mtime moves for any real change;
/// * the failure direction is safe. A change this misses leaves a pane running
///   code it already had — the pre-existing state — while any change it sees
///   replaces the pane. It cannot invent a restart for a file nobody touched,
///   because mtime only moves when something writes.
///
/// Sorted, so two daemons walking the same checkout agree byte for byte: an
/// unstable order would make the hash differ per-process and restart the whole
/// fleet on every pass.
///
/// # Unreadable is NOT empty
///
/// An unreadable tree returns `None`, never a digest over nothing. Hashing the
/// empty string as a positive answer would publish one shared digest for every
/// company whose checkout could not be read, and — worse — a digest that
/// differs from the real one, which replaces EVERY PANE IN THE COMPANY. The
/// caller refuses rather than defaulting; see `docstore::desired`.
#[must_use]
pub fn extension_source_digest(assets: &LauncherAssets) -> Option<String> {
    use sha2::{Digest, Sha256};

    let mut entries: Vec<(String, u64, i128)> = Vec::new();
    for path in organization_extension_paths(&assets.launcher_root) {
        let meta = std::fs::metadata(&path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let relative = path
            .strip_prefix(&assets.launcher_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        // UNREADABLE IS NOT ZERO. A file whose mtime cannot be read is
        // answered with `None` for the whole digest: defaulting it to the epoch
        // could adopt a stale pane as current.
        let modified = meta
            .modified()
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|since| i128::try_from(since.as_millis()).ok())?;
        entries.push((relative, meta.len(), modified));
    }

    let mut hasher = Sha256::new();
    // Length-prefixed framing, for the reason `desired_launch_hash` states: a
    // separator is unambiguous only until a filename contains it.
    let mut field = |value: &str| {
        hasher.update(value.len().to_le_bytes());
        hasher.update(value.as_bytes());
    };
    for (relative, len, modified) in &entries {
        field(relative);
        field(&len.to_string());
        field(&modified.to_string());
    }
    Some(hex_digest(hasher.finalize()))
}

// --- shared filesystem primitives -----------------------------------------
//
// `clippy.toml` bans `std::fs::write`, `std::fs::remove_file`,
// `std::fs::rename`, `std::fs::File` and `std::fs::OpenOptions` outside
// `chiefd_host`'s executor seam, so every publish here routes through
// [`crate::files`].
//
// TOMBSTONE: `stage_then_swap`, `stage_then_swap_symlink`, `copy_tree`,
// `rename_path`, `link_if_present`, `relative_link`, `promote_tree`,
// `remove_path`, `normalize_path`, `read_dir_names`, `base_name`, `file_stem`,
// `final_path` and the `.staging-<uuid>` sibling machinery. Most of them
// existed to publish a COPY over a directory a live agent was writing into —
// the #1189 race — and there is no copy left to publish. `ensure_agent_home`
// creates agent content once and later replaces only the two exact Chief-owned
// organization theme files. Each file uses this atomic primitive, so no live
// Pi can read a partial JSON document and none of the directory-swap machinery
// has a subject.

/// Publish one text file atomically at `mode`, creating parents as needed.
pub(crate) fn publish_text(path: &Path, contents: &str, mode: u32) -> Result<(), MaterializeError> {
    crate::files::publish_atomically(path, contents, mode).map_err(MaterializeError::from)
}

/// Create one text file at `mode`, once, never replacing an existing one.
/// Returns whether this call created it.
///
/// The [`publish_text`] sibling for anything whose value is that it was written
/// exactly once — an identity key, and nothing else so far. See
/// [`crate::files::create_exclusively`] for why the primitive links rather than
/// renames.
pub(crate) fn create_text_once(
    path: &Path,
    contents: &str,
    mode: u32,
) -> Result<bool, MaterializeError> {
    crate::files::create_exclusively(path, contents, mode).map_err(MaterializeError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stamp one file's modification time, rather than sleeping for one.
    ///
    /// Through `nix` rather than `std::fs::File`: `clippy.toml` keeps file
    /// handles inside `chiefd_host::executor` (README §5.6), and that holds in
    /// fixtures too. `std::thread::sleep` is banned for a separate reason —
    /// waiting flows through the injected clock — and a fixed instant is a
    /// stronger assertion than a race against mtime granularity anyway.
    fn set_modified(path: &Path, unix_seconds: i64) {
        let stamp = nix::sys::time::TimeVal::new(unix_seconds, 0);
        nix::sys::stat::utimes(path, &stamp, &stamp).expect("set mtime");
    }

    /// A publish is ATOMIC and creates its parents — the property
    /// `ensure_agent_home` leans on when it writes `AGENTS.md` into a directory
    /// it has just made, and the one `ensure_identity_key` leans on when it
    /// writes into a keys directory that does not exist yet.
    #[test]
    fn publish_text_creates_parents_and_sets_the_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a").join("b").join("AGENTS.md");
        publish_text(&path, "# Chief\n", 0o600).expect("publish");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "# Chief\n");
        assert_eq!(std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777, 0o600);
    }

    /// Every agent pane loads `company-stop.ts`, and the Founder does not.
    ///
    /// The list is asserted by NAME rather than by length: a count would pass
    /// for any four files, and the fact worth pinning is that the extension
    /// registering `/stop` is one of them. A pane that does not load it has a
    /// `/stop` that silently does nothing, which is indistinguishable from the
    /// command not existing and is exactly the defect it was added to fix.
    #[test]
    fn every_agent_pane_loads_the_company_stop_extension() {
        assert_eq!(
            ORGANIZATION_EXTENSION_FILES,
            ["organization-intercom.ts", "team-ui.ts", "tribes-welcome.ts", "company-stop.ts"],
            "the agent extension set is argv order and every name in it is load-bearing"
        );
        assert!(
            !ORGANIZATION_EXTENSION_FILES.contains(&"founder-launch.ts"),
            "the Founder's extension must never reach an agent pane"
        );
    }

    /// The resolved paths sit under the launcher checkout's extensions
    /// directory, in the declared order.
    #[test]
    fn the_company_stop_path_resolves_under_the_launcher_checkout() {
        let paths = organization_extension_paths(Path::new("/opt/chief"));
        assert_eq!(paths.len(), ORGANIZATION_EXTENSION_FILES.len());
        assert_eq!(
            paths.last().expect("a non-empty set"),
            Path::new("/opt/chief/packages/piing/extensions/company-stop.ts")
        );
    }

    /// The digest is over the CHECKOUT, and an unreadable checkout is `None`
    /// rather than a digest over nothing — the difference between "replace no
    /// pane" and "replace every pane in the company".
    #[test]
    fn the_extension_digest_moves_with_the_checkout_and_refuses_an_unreadable_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let extensions = dir.path().join("packages").join("piing").join("extensions");
        let assets = LauncherAssets { launcher_root: dir.path().to_path_buf() };
        assert_eq!(extension_source_digest(&assets), None, "an absent root is not a digest");

        for path in organization_extension_paths(dir.path()) {
            publish_text(&path, "export const a = 1\n", 0o644).expect("seed");
        }
        let entry = extensions.join("team-ui.ts");
        let first = extension_source_digest(&assets).expect("a readable checkout digests");
        assert_eq!(extension_source_digest(&assets), Some(first.clone()), "stable across calls");

        publish_text(&extensions.join("founder-launch.ts"), "not loaded by agents\n", 0o644)
            .expect("unrelated founder extension");
        assert_eq!(
            extension_source_digest(&assets),
            Some(first.clone()),
            "the deploy hash must cover exactly the extensions agent argv loads"
        );

        // A deploy that keeps the byte LENGTH still moves the digest, because
        // mtime is part of the field set — the case a length-only hash misses.
        // The stamp is SET rather than waited for: `clippy.toml` bans
        // `std::thread::sleep`, and a fixed instant is a stronger assertion
        // than a race against filesystem mtime granularity anyway.
        publish_text(&entry, "export const a = 2\n", 0o644).expect("deploy");
        set_modified(&entry, 1_700_000_000);
        let second = extension_source_digest(&assets).expect("digest");
        assert_ne!(
            second, first,
            "a deploy must move the digest, or a stale pane survives the converge pass"
        );
        // And mtime alone is enough: same bytes, different stamp.
        set_modified(&entry, 1_700_000_060);
        assert_ne!(
            extension_source_digest(&assets),
            Some(second),
            "an identical file with a newer mtime is a redeploy, and must move the digest"
        );
    }
}
