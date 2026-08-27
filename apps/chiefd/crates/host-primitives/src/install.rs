//! Where an installed chief keeps the resources it hands to a Pi pane, and how
//! a running binary finds them without being told.
//!
//! # What this replaces, and why the thing it replaces had to die
//!
//! Until the open-source launch there was a POINTER: `bun run release` wrote
//! the absolute path of the SOURCE CHECKOUT into `~/.chief/launcher-root`, and
//! the daemon read that file to resolve every person's Pi extensions and
//! skills. It worked, and it made a clone-free install impossible: the
//! installed binaries were not self-contained, they were a front end for a git
//! working copy that had to stay on disk, at that path, at a compatible
//! revision. `chief upgrade` on a machine that never cloned anything could not
//! exist while that pointer did.
//!
//! It also had the failure mode a pointer always has. A missing or stale
//! pointer resolved to a path no checkout occupies, materialization produced a
//! person with an EMPTY `extensions/`, and the company's CEO came up with no
//! `org_*` tools at all — while genesis reported "✅ Company launched". The
//! whole incident is written up in `chiefd-daemon/src/run.rs`.
//!
//! # What replaces it: the binary's own location
//!
//! An install is versioned, and the resources sit BESIDE the binaries that
//! were built with them:
//!
//! ```text
//! ~/.chief/
//!   bin/chiefd -> ../versions/v2.0.7/bin/chiefd      (symlink, atomically re-pointed)
//!   versions/v2.0.7/
//!     bin/{chief,chiefd,beacond}
//!     resources/packages/piing/{extensions,skills,dist/extensionruntime}
//!     manifest.json
//! ```
//!
//! So [`resource_root_from_exe`] is `<this binary>/../../resources`, and there
//! is nothing to record, nothing to keep in step, and nothing to go stale.
//! Three properties fall out of that, and each one is a bug the pointer had:
//!
//!   * **It cannot point at the wrong version.** A daemon that has been running
//!     since before an upgrade keeps resolving ITS OWN version's resources,
//!     because its own executable is still that file — Unix keeps the inode
//!     alive while it is open, and `chief upgrade` re-points a symlink rather
//!     than overwriting a binary. The pointer had one global value and an
//!     upgrade changed it under every running daemon at once.
//!   * **It cannot be absent.** A binary always knows where it is.
//!   * **It needs no `$HOME`.** The pointer's readers all had a `$HOME`
//!     fallback, and every one of them was a different guess.
//!
//! # Why the subtree shape did NOT change
//!
//! `resources/` mirrors the checkout: `packages/piing/extensions`,
//! `packages/piing/skills`, `packages/piing/dist/extensionruntime`. That looks
//! redundant inside a directory called `resources`, and it is deliberate — it
//! is what makes the dev path and the installed path the SAME path expression.
//! `--launcher-root <checkout>` still works, unchanged, for the test harness
//! and for anyone running out of a working copy, because a checkout root and a
//! `resources/` root resolve every subpath identically. Renaming the subtree
//! would have bought tidiness and cost one code path per consumer.

use std::path::PathBuf;

/// The directory name, under a version directory, holding the Pi payload.
pub const RESOURCES_DIR: &str = "resources";

/// The resources installed beside THIS running binary, if this binary was
/// installed rather than run out of a build directory.
///
/// `None` when the executable cannot be resolved, when it has no grandparent
/// (a binary at the filesystem root), or when no `resources/` directory sits
/// there — which is the ordinary case for `cargo run` and for every test that
/// runs a freshly built binary out of `target/`. Those callers pass
/// `--launcher-root` explicitly, and `None` is what makes the refusal they get
/// otherwise say something true.
///
/// Deliberately checks that the directory EXISTS rather than returning a path
/// that might not. The pointer this replaces returned a path unconditionally,
/// and a path that resolves to nothing is exactly how a CEO came up with no
/// tools and nobody found out for a day.
pub fn resource_root_from_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    resource_root_beside(&exe)
}

/// [`resource_root_from_exe`], against a named executable path.
///
/// Split out so the rule is testable without a real install: a resolver that
/// reads `current_exe()` can only be tested on the machine the test runs on,
/// which is how the launcher-root tests around this used to pass or fail
/// according to whether the box happened to have chief installed.
pub fn resource_root_beside(executable: &std::path::Path) -> Option<PathBuf> {
    let candidate = executable.parent()?.parent()?.join(RESOURCES_DIR);
    candidate.is_dir().then_some(candidate)
}

/// Whether `path` has the shape of an INSTALLED, versioned resource root —
/// `…/versions/<v>/resources`, which is what [`resource_root_from_exe`]
/// produces from an installed binary.
///
/// # Why this shape is a fact worth recognising (H.2.5)
///
/// The per-company `org_settings.launcher_root` override, when set, outranks
/// the root the daemon resolved — a deliberate "pin your own checkout" feature.
/// But the exe-derived root is an INSTALL DETAIL, not a company's chosen
/// checkout: it lives under `~/.chief/versions/<v>/`, which a later `chief
/// upgrade` prunes. If that path were ever RECORDED as the override, a routine
/// upgrade would delete the very directory materialization reads by path, and
/// the company would break with no upgrade "failure" in sight. So a RECORDED
/// root of this shape is refused rather than used — resolve the exe-derived
/// root fresh on every boot instead of persisting it. Matched by shape, not by
/// an absolute `$HOME` comparison, so it holds under a relocated `CHIEF_HOME`
/// and in a fixture tree alike: last component `resources`, its grandparent
/// `versions`.
#[must_use]
pub fn is_installed_resource_root(path: &std::path::Path) -> bool {
    use std::path::Component;
    let ends_in_resources = path.file_name().and_then(|name| name.to_str()) == Some(RESOURCES_DIR);
    let grandparent_is_versions = path
        .parent()
        .and_then(|parent| parent.parent())
        .and_then(std::path::Path::file_name)
        .and_then(|name| name.to_str())
        == Some("versions");
    // The `Component` walk rejects a `resources` whose real grandparent is
    // reached only through `..` segments — a path built by joining rather than
    // canonicalising could otherwise spoof the shape.
    let no_parent_traversal =
        !path.components().any(|component| matches!(component, Component::ParentDir));
    ends_in_resources && grandparent_is_versions && no_parent_traversal
}

#[cfg(test)]
mod tests {
    // The fixtures write a file where a `resources/` directory should be, to
    // prove `resource_root_beside` refuses a non-directory. The write is the
    // fixture, not a product effect, so the seam's allow sits at the module
    // boundary (clippy.toml README §5.6).
    #![allow(clippy::disallowed_methods)]
    use super::*;

    #[test]
    fn resources_are_found_two_levels_above_the_binary() {
        let root = tempfile::tempdir().expect("tempdir");
        let bin = root.path().join("versions/v2.0.7/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let resources = root.path().join("versions/v2.0.7/resources");
        std::fs::create_dir_all(&resources).expect("resources");

        assert_eq!(resource_root_beside(&bin.join("chiefd")), Some(resources));
    }

    #[test]
    fn a_binary_with_no_resources_beside_it_resolves_to_nothing() {
        // The `cargo run` and `target/release` case. ABSENT, never a path that
        // does not exist: a path that resolves to nothing is what produced a
        // company whose CEO had no tools and a genesis that reported success.
        let root = tempfile::tempdir().expect("tempdir");
        let bin = root.path().join("apps/chiefd/target/release");
        std::fs::create_dir_all(&bin).expect("bin");

        assert_eq!(resource_root_beside(&bin.join("chiefd")), None);
    }

    #[test]
    fn a_path_with_no_grandparent_resolves_to_nothing_rather_than_panicking() {
        assert_eq!(resource_root_beside(std::path::Path::new("/chiefd")), None);
        assert_eq!(resource_root_beside(std::path::Path::new("chiefd")), None);
    }

    #[test]
    fn a_resources_entry_that_is_not_a_directory_resolves_to_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        let bin = root.path().join("versions/v2.0.7/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::write(root.path().join("versions/v2.0.7/resources"), b"not a directory")
            .expect("write");

        assert_eq!(resource_root_beside(&bin.join("chiefd")), None);
    }

    #[test]
    fn an_installed_versioned_resource_root_is_recognised_by_shape() {
        assert!(is_installed_resource_root(std::path::Path::new(
            "/home/me/.chief/versions/2.0.7/resources"
        )));
        assert!(is_installed_resource_root(std::path::Path::new(
            "/opt/chief-home/versions/v9.9.9/resources"
        )));
    }

    #[test]
    fn a_developer_checkout_is_not_an_installed_resource_root() {
        // A pinned checkout — the legitimate `org_settings.launcher_root`
        // override — must not be mistaken for the prunable install path.
        assert!(!is_installed_resource_root(std::path::Path::new("/root/wt-web")));
        assert!(!is_installed_resource_root(std::path::Path::new(
            "/home/me/src/chief/packages/piing"
        )));
        // Right leaf, wrong grandparent.
        assert!(!is_installed_resource_root(std::path::Path::new("/somewhere/else/resources")));
        // Right grandparent, wrong leaf.
        assert!(!is_installed_resource_root(std::path::Path::new("/x/versions/2.0.7/bin")));
    }

    #[test]
    fn a_parent_traversal_cannot_spoof_the_installed_shape() {
        assert!(!is_installed_resource_root(std::path::Path::new(
            "/checkout/../versions/2.0.7/resources"
        )));
    }
}
