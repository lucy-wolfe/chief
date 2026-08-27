//! Is a running component the build that is INSTALLED right now?
//!
//! # The operator's ruling
//!
//! *"when I run `chief` and chiefd/beacond/actuator/etc are on old shit, it
//! needs to kill that shit if it's different."*
//!
//! The incident behind it: after a release, the running `chiefd`, `beacond`
//! and a resident actuator stayed on the OLD binaries. An actuator from 17:01
//! crash-looped to 164 failures while the fixed binary sat on disk, and
//! nothing said so. The H.6 skew guard REFUSES a mismatched daemon; it does
//! not replace one. The fix was installed and not in effect.
//!
//! # "Different" is not a version string, and not a path
//!
//! The rebuild that caused this was `0.5.0` -> `0.5.0`: same declared version,
//! same install path, different bytes. A version comparison answers "same,
//! leave it" and fails the exact case that motivated the rule. A path
//! comparison fails it too, because the path is the half that stayed the same.
//!
//! **The identity is `(device, inode)`** — see
//! [`host_primitives::rendezvous::BuildIdentity`] — because a re-release
//! removes `versions/<v>` and writes a new file there, so the inode is what
//! moves. `/proc/<pid>/exe` reading `(deleted)` is a TRUE and useful DETAIL
//! for the operator's line on Linux, and it is deliberately not the test: it
//! is a presentation convention of one kernel's procfs, and a check that keys
//! on the string is one cosmetic change away from silently passing everything.
//!
//! # The identity is REPORTED, never inferred
//!
//! Each component stats its own executable at start and publishes the answer
//! on the surface it already publishes — the daemon on its rendezvous, beside
//! its pid. `chief` compares that report against the file the install symlink
//! resolves to now.
//!
//! This is the same rule the pid already follows, and it is not merely tidier:
//! `/proc` does not exist on macOS, and the Darwin call that looks like its
//! equivalent answers with the process's cwd and root vnodes rather than its
//! executable (`proc_vnodepathinfo` is `pvi_cdir` + `pvi_rdir`). A `/proc`
//! design therefore has no honest macOS arm — it would either refuse there, in
//! which case the operator's own incident class is undetectable on their
//! platform, or it would compile against the wrong vnode and look implemented
//! while passing everything. A process can always stat its OWN executable, on
//! every platform, at a moment when the file certainly still exists. One rule,
//! both platforms, one test.

use std::path::Path;

use host_primitives::rendezvous::{BuildIdentity, ReportedBuild};

/// What is known about whether a running component is the installed build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuildCheck {
    /// The running component IS the installed build. Say nothing.
    Current,
    /// The running component is NOT the installed build.
    Stale {
        /// What the component reported it is running.
        running: BuildIdentity,
        /// What the install resolves to now.
        installed: BuildIdentity,
    },
    /// The question cannot be answered, for a reason worth saying out loud
    /// once. NEVER treated as "current": a check that guesses "same" when it
    /// does not know is how the silent-stale incident happened in the first
    /// place.
    Unknowable {
        /// The sentence the operator reads.
        reason: String,
    },
}

/// Compare what a component REPORTED against what is installed now.
///
/// `reported_exe` is the path the component was started from, and
/// `reported_build` its identity, both as the component published them.
/// `installed` is the path the install resolves to today.
pub(crate) fn check(
    component: &str,
    reported: Option<&ReportedBuild>,
    installed: &Path,
) -> BuildCheck {
    // THE BOOTSTRAP GENERATION. A component started by a build that predates
    // this field publishes no identity, so its first check after this ships
    // cannot be answered. It is said once, loudly, and it closes itself: the
    // next restart of that component publishes one and it is knowable for
    // ever. This is the unavoidable cost of adding a fact to a durable
    // surface, and it is not a hole — a hole would be calling it "current".
    let Some(ReportedBuild { exe, identity: running }) = reported else {
        return BuildCheck::Unknowable {
            reason: format!(
                "{component} was started by a build that predates build-identity reporting, so it \
                 cannot say which binary it is running; it becomes checkable the next time it \
                 restarts"
            ),
        };
    };
    // A COMPONENT RUNNING OUT OF A DEVELOPMENT TREE IS OUT OF SCOPE. Its
    // executable is not under the versioned install at all, so "is it the
    // installed build" is not a question about it — and restarting somebody's
    // `cargo run` onto the installed binary would be the rule doing real harm
    // in the one place a developer would least expect it.
    let Some(install_root) = versioned_install_root(installed) else {
        return BuildCheck::Unknowable {
            reason: format!(
                "this chief is itself a development build ({}), so it has no versioned install to \
                 compare {component} against",
                installed.display()
            ),
        };
    };
    if !exe.starts_with(&install_root) {
        return BuildCheck::Unknowable {
            reason: format!(
                "{component} is running from {}, which is not a versioned install; a development \
                 build is never restarted by this rule",
                exe.display()
            ),
        };
    }
    let Some(installed_build) = BuildIdentity::of_path(installed) else {
        return BuildCheck::Unknowable {
            reason: format!(
                "nothing readable is installed at {}, so there is no build to compare \
                 {component} against",
                installed.display()
            ),
        };
    };
    if *running == installed_build {
        BuildCheck::Current
    } else {
        BuildCheck::Stale { running: *running, installed: installed_build }
    }
}

/// `~/.chief/versions`, derived from an installed binary's real path.
///
/// Derived rather than passed so the check needs no `home` argument and cannot
/// be handed a different root than the one the binary it is judging came from.
/// `None` when the path holds no `versions/<v>` segment at all, which is how a
/// development build of `chief` itself is recognized.
fn versioned_install_root(installed: &Path) -> Option<std::path::PathBuf> {
    let real = std::fs::canonicalize(installed).ok()?;
    real.ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "versions"))
        .map(Path::to_path_buf)
}

/// The line the operator reads BEFORE the restart happens.
///
/// It names the component, both identities, and what is about to be done — and
/// on Linux it carries the `(deleted)` detail when procfs offers it, because
/// that is the sentence an operator recognizes from their own shell. The
/// detail is decoration on a decision already made by the identities.
pub(crate) fn stale_line(
    component: &str,
    running: BuildIdentity,
    installed: BuildIdentity,
    exe: &Path,
) -> String {
    format!(
        "{component} is running a replaced binary ({}, inode {running}); the installed build is \
         inode {installed}. Restarting {component} on the installed build.",
        exe.display()
    )
}

/// The refusal when a restart did not take, printed once and never looped on.
///
/// A rule that restarts on mismatch can restart for ever: a `bin/` symlink
/// pointing at a stale version directory, or two install roots, produce a
/// fresh component that reads stale again immediately. One attempt, then this,
/// which names both paths and asks the question that leads somewhere rather
/// than guessing an answer.
pub(crate) fn refusal_after_one_attempt(component: &str, exe: &Path, installed: &Path) -> String {
    format!(
        "{component} still reports a different build after one restart on the installed binary. \
         It is running {}; the install resolves to {}. Not restarting it again — a loop here \
         would never end. Your install may have two roots: check what {} points at.",
        exe.display(),
        installed.display(),
        installed.display()
    )
}

#[cfg(test)]
// THE FIXTURE WRITES REAL FILES, AND MUST. The seam rule that bans
// `std::fs::write`/`remove_file` is about production filesystem effects
// belonging to a host transaction; there is no host transaction in a unit
// test, and what is under test here is what `stat` answers about real inodes
// on a real filesystem. A mocked filesystem would assert the mock — and the
// defect this module's own test caught (a reused inode reading as "same
// build") is invisible to any mock, because it is the allocator's behaviour
// and not ours. Same allowance, same reason, as `daemon.rs`'s `publish`.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::{check, refusal_after_one_attempt, stale_line, BuildCheck};
    use host_primitives::rendezvous::{BuildIdentity, ReportedBuild};
    use std::path::{Path, PathBuf};

    /// What a component would report about an executable it was started from.
    fn reported(exe: &Path) -> ReportedBuild {
        ReportedBuild {
            exe: exe.to_path_buf(),
            identity: BuildIdentity::of_path(exe).expect("an identity"),
        }
    }

    /// A versioned install, as the installer lays it out: the real file under
    /// `versions/<v>/bin/`, and `bin/<name>` a symlink pointing at it.
    struct Install {
        home: tempfile::TempDir,
    }

    impl Install {
        fn new() -> Self {
            let home = tempfile::tempdir().expect("tempdir");
            std::fs::create_dir_all(home.path().join("versions/0.5.0/bin")).expect("versions");
            std::fs::create_dir_all(home.path().join("bin")).expect("bin");
            Self { home }
        }

        fn write_version(&self, version: &str, bytes: &[u8]) -> PathBuf {
            let dir = self.home.path().join("versions").join(version).join("bin");
            std::fs::create_dir_all(&dir).expect("version dir");
            let path = dir.join("chiefd");
            std::fs::write(&path, bytes).expect("write");
            path
        }

        /// Re-point `bin/chiefd` at a version, the way an upgrade does.
        fn link(&self, target: &Path) -> PathBuf {
            let link = self.home.path().join("bin/chiefd");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(target, &link).expect("symlink");
            link
        }
    }

    /// THE TEST THAT KILLS THE VERSION-STRING DESIGN.
    ///
    /// The operator's own incident: a `0.5.0` -> `0.5.0` rebuild. The version
    /// strings are EQUAL and the build is different, so any check that
    /// compares versions answers "same, leave it" and the stale process keeps
    /// running. The identities differ, so this one restarts it.
    #[test]
    fn a_same_version_rebuild_is_stale_even_though_the_version_did_not_move() {
        let install = Install::new();
        let real = install.write_version("0.5.0", b"the build that is running");
        let link = install.link(&real);
        let running = reported(&real);

        // The release: `versions/0.5.0` is removed and written again. Same
        // version, same path, same symlink — different file.
        std::fs::remove_file(&real).expect("remove");
        // A DIFFERENT LENGTH, deliberately. The first version of this check was
        // `(dev, ino)` alone and this exact test failed on CI with `Current`,
        // because the replacement was handed the inode the removed file had
        // just freed. That is what added size and mtime to the identity, and a
        // fixture whose two builds differ in length is what keeps this test
        // about the RULE rather than about the filesystem's allocator.
        let rebuilt = install.write_version("0.5.0", b"the build that is installed instead");
        assert_eq!(real, rebuilt, "the PATH is unchanged, which is the whole trap");

        let check = check("chiefd", Some(&running), &link);
        let BuildCheck::Stale { running: was_running, installed } = check else {
            panic!("a replaced binary is stale: {check:?}");
        };
        assert_eq!(was_running, running.identity);
        assert_ne!(was_running, installed, "same version, different identity");

        let line = stale_line("chiefd", was_running, installed, &real);
        assert!(line.contains("chiefd is running a replaced binary"), "{line}");
        assert!(line.contains(&installed.to_string()), "it names what IS installed: {line}");
        assert!(line.contains(&was_running.to_string()), "and what is running: {line}");
    }

    /// The ordinary case is silent: a component that IS the installed build.
    #[test]
    fn a_component_running_the_installed_build_is_current() {
        let install = Install::new();
        let real = install.write_version("0.5.0", b"one build");
        let link = install.link(&real);
        assert_eq!(check("chiefd", Some(&reported(&real)), &link), BuildCheck::Current);
    }

    /// A different VERSION DIRECTORY is stale too, even when the old file
    /// survives — the `bin/` symlink is what says which build is installed.
    #[test]
    fn a_component_from_an_older_version_directory_is_stale() {
        let install = Install::new();
        let old = install.write_version("0.4.0", b"last release");
        let new = install.write_version("0.5.0", b"this release");
        let link = install.link(&new);
        assert!(
            matches!(check("chiefd", Some(&reported(&old)), &link), BuildCheck::Stale { .. }),
            "the old inode still exists and is still not what is installed"
        );
    }

    /// THE BOOTSTRAP GENERATION. A component that reports no identity is
    /// unknowable, never "current" — and the sentence says it closes itself.
    #[test]
    fn a_component_that_reports_no_identity_is_unknowable_and_says_why() {
        let install = Install::new();
        let real = install.write_version("0.5.0", b"one build");
        let link = install.link(&real);
        let BuildCheck::Unknowable { reason } = check("chiefd", None, &link) else {
            panic!("nothing reported is nothing known");
        };
        assert!(reason.contains("predates build-identity reporting"), "{reason}");
        assert!(reason.contains("next time it restarts"), "and that it fixes itself: {reason}");
    }

    /// A DEVELOPMENT BUILD IS NEVER RESTARTED. Its executable is not under the
    /// versioned install, so the question is not about it.
    #[test]
    fn a_development_build_is_out_of_scope_and_is_said_out_loud() {
        let install = Install::new();
        let real = install.write_version("0.5.0", b"one build");
        let link = install.link(&real);
        let dev = install.home.path().join("target/debug/chiefd");
        std::fs::create_dir_all(dev.parent().expect("parent")).expect("target dir");
        std::fs::write(&dev, b"a cargo build").expect("write");
        let BuildCheck::Unknowable { reason } = check("chiefd", Some(&reported(&dev)), &link)
        else {
            panic!("a dev build is out of scope, not stale");
        };
        assert!(reason.contains("not a versioned install"), "{reason}");
        assert!(reason.contains("never restarted"), "{reason}");
    }

    /// Nothing installed is unknowable, not stale: there is no build to
    /// restart onto, and killing a live component for a binary that is not
    /// there would leave the operator with nothing at all.
    #[test]
    fn an_absent_install_is_unknowable_rather_than_stale() {
        let install = Install::new();
        let real = install.write_version("0.5.0", b"one build");
        let link = install.link(&real);
        let running = reported(&real);
        std::fs::remove_file(&real).expect("remove the file the link points at");
        let check = check("chiefd", Some(&running), &link);
        assert!(
            matches!(check, BuildCheck::Unknowable { .. }),
            "a broken install is not a reason to stop a working component: {check:?}"
        );
    }

    /// THE LOOP FLOOR'S WORDS. It names both paths and asks a question rather
    /// than guessing, and it says it is not trying again.
    #[test]
    fn the_refusal_names_both_paths_and_refuses_to_loop() {
        let refusal = refusal_after_one_attempt(
            "chiefd",
            Path::new("/home/op/.chief/versions/0.4.0/bin/chiefd"),
            Path::new("/home/op/.chief/bin/chiefd"),
        );
        assert!(refusal.contains("versions/0.4.0/bin/chiefd"), "{refusal}");
        assert!(refusal.contains("/home/op/.chief/bin/chiefd"), "{refusal}");
        assert!(refusal.contains("Not restarting it again"), "{refusal}");
        assert!(refusal.contains("two roots"), "the remedy question: {refusal}");
    }
}
