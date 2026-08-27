//! The daemon's half of "how a command finds its own directory's daemon".
//!
//! `host_primitives::rendezvous` owns the SHAPE — the file name, the mode, and
//! [`DaemonRendezvous`] itself — because two programs that may not link each
//! other must decode the same four fields. This module owns the WRITE, and
//! `chief-cli` owns the read.
//!
//! # When it is published, and when it is removed
//!
//! [`publish`] is called at the `surface_bound` latch in `run.rs`: beacond has
//! admitted this daemon, the docstore listener is mounted on the exact address
//! beacond was told, and its schema is ensured. That is the first instant the
//! URL in this file is answerable, and publishing a URL that is not yet
//! answerable is what turns a pointer into a lie. There is deliberately no
//! second ordering point.
//!
//! [`remove`] is called beside the beacond deregistration on graceful
//! shutdown, for the same reason and in the same place: both are this
//! process's published locations, and a shutdown that cleared one and left the
//! other would leave the two disagreeing.
//!
//! # A SIGKILLed daemon leaves this file behind, and that is fine
//!
//! There is no lock, no heartbeat, and no sweeper. The file is a POINTER, not
//! authority: a reader must prove the pid is alive and the listener answers
//! before binding it, so a stale file costs one probe and is then overwritten
//! by the next daemon. Adding a lease to a hint is how a pid file becomes
//! durable state nobody meant to keep.

use std::path::Path;

use host_primitives::rendezvous::{rendezvous_path, DaemonRendezvous, RENDEZVOUS_MODE};

/// Publish this daemon's location for `dir`, atomically.
///
/// `chiefd_host::files::publish_atomically` is the workspace's one
/// publish-by-rename seam (`clippy.toml` bans `std::fs::write`/`rename`
/// everywhere else): the JSON is written to a sibling temp file and
/// `rename(2)`d over the target, so a reader never sees half a rendezvous and
/// an existing symlink at the target is replaced rather than written through.
/// It creates `<dir>/.chief/run/` on the way, which is correct — that folder
/// is disposable and any process may recreate it.
///
/// # Errors
/// A bounded message when the file cannot be serialized or published. The
/// caller must REFUSE TO SERVE on it: a daemon nobody can find is not a daemon
/// an operator can attach to, and failing loudly at boot beats every command
/// in that directory timing out later with nothing to name.
pub(crate) fn publish(dir: &Path, url: &str) -> Result<(), String> {
    let rendezvous = DaemonRendezvous {
        dir: dir.to_path_buf(),
        key: crate::company_dir::company_key(dir),
        url: url.to_owned(),
        pid: std::process::id(),
        // WHICH BUILD THIS DAEMON IS, measured by the daemon itself, here,
        // while its own executable certainly still exists on disk.
        //
        // This is the same rule as the pid beside it: a component REPORTS what
        // it is, and no reader infers it behind the process's back. `chief`
        // compares this against the file `~/.chief/bin/chiefd` resolves to
        // now — if a release replaced that file, the identities differ even
        // when the version string did not move, which is the operator's own
        // incident. `None` here means this platform would not answer, and a
        // reader treats that as unknowable rather than as "the same".
        build: host_primitives::rendezvous::ReportedBuild::of_running_process(),
    };
    let body = serde_json::to_string_pretty(&rendezvous)
        .map_err(|error| format!("cannot render the daemon rendezvous: {error}"))?;
    chiefd_host::files::publish_atomically(&rendezvous_path(dir), &body, RENDEZVOUS_MODE)
        .map_err(|error| error.to_string())
}

/// Remove this daemon's rendezvous on graceful shutdown.
///
/// A missing file is already the desired state, so this is idempotent. The
/// outcome is reported to the caller as a warning, never a failure: the
/// process is exiting either way, and the next daemon overwrites whatever is
/// left.
///
/// # Errors
/// A bounded message when an existing file could not be removed.
pub(crate) fn remove(dir: &Path) -> Result<(), String> {
    chiefd_host::files::remove_file_if_exists(&rendezvous_path(dir))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_primitives::rendezvous::RENDEZVOUS_FILENAME;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;

    /// `<dir>/.chief/run`, taken from the shared path rather than rebuilt: the
    /// folder is `host-primitives`' to name, and a literal here would be a
    /// second answer to where the rendezvous lives.
    fn run_folder(dir: &Path) -> PathBuf {
        rendezvous_path(dir).parent().expect("the rendezvous always has a parent").to_path_buf()
    }

    /// The four fields a client decodes, with the values only this process
    /// knows — and the directory it names is the one it was published for.
    #[test]
    fn a_published_rendezvous_names_this_directory_this_key_this_url_and_this_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        publish(dir.path(), "http://127.0.0.1:8793").expect("publish");

        let path = rendezvous_path(dir.path());
        assert_eq!(path.file_name().and_then(|name| name.to_str()), Some(RENDEZVOUS_FILENAME));
        let decoded: DaemonRendezvous =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("decode");
        assert_eq!(decoded.dir, dir.path());
        assert!(decoded.describes(dir.path()), "the file must describe the directory it is in");
        assert_eq!(decoded.key, crate::company_dir::company_key(dir.path()));
        assert_eq!(decoded.url, "http://127.0.0.1:8793");
        assert_eq!(decoded.pid, std::process::id());
    }

    /// It lives in the DISPOSABLE run folder, which publishing creates.
    ///
    /// `.chief/run/` is deleted freely (that is what disposable means), so a
    /// daemon that required it to exist first would refuse to publish after
    /// any cleanup. The directory is an output of this call, not an input.
    #[test]
    fn publishing_creates_the_disposable_run_folder_it_lives_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!run_folder(dir.path()).exists(), "fixture: nothing there yet");
        publish(dir.path(), "http://127.0.0.1:1").expect("publish");
        assert_eq!(
            rendezvous_path(dir.path()),
            run_folder(dir.path()).join("daemon.json"),
            "the rendezvous belongs to the run folder, not beside the store"
        );
        assert!(rendezvous_path(dir.path()).is_file());
    }

    /// PUBLISHED AT THE DOCUMENTED MODE, not at whatever the umask allows.
    ///
    /// The mode is a constant in `host-primitives` precisely so a future field
    /// that IS sensitive has to change that line rather than inherit a
    /// permissive default; a writer that ignored it would make the constant a
    /// comment.
    #[test]
    fn the_rendezvous_is_published_at_the_mode_the_shared_contract_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        publish(dir.path(), "http://127.0.0.1:1").expect("publish");
        let mode =
            std::fs::metadata(rendezvous_path(dir.path())).expect("stat").permissions().mode()
                & 0o777;
        assert_eq!(mode, RENDEZVOUS_MODE, "published mode must be the shared constant");
    }

    /// A RE-PUBLISH REPLACES, and never appends or half-replaces.
    ///
    /// The stale-file case is the ordinary one after a reboot, so the second
    /// daemon's write has to leave a file that decodes cleanly as ITS
    /// location. Written over a longer previous body on purpose: a naive
    /// truncate-and-write that failed mid-way would leave the tail of the old
    /// URL behind and still parse as JSON here.
    #[test]
    fn republishing_over_a_stale_rendezvous_leaves_only_the_new_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        publish(dir.path(), "http://127.0.0.1:65535/a-deliberately-long-stale-url").expect("first");
        publish(dir.path(), "http://127.0.0.1:1").expect("second");

        let body = std::fs::read_to_string(rendezvous_path(dir.path())).expect("read");
        let decoded: DaemonRendezvous = serde_json::from_str(&body).expect("decode");
        assert_eq!(decoded.url, "http://127.0.0.1:1");
        assert!(!body.contains("65535"), "no byte of the stale rendezvous may survive: {body}");
    }

    /// Publishing leaves NO temp file behind — a `.chiefd-*.tmp` sibling that
    /// outlived its rename would be exactly the "half-written rendezvous a
    /// reader might find" this discipline exists to prevent.
    #[test]
    fn publishing_leaves_no_temporary_sibling_in_the_run_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        publish(dir.path(), "http://127.0.0.1:1").expect("publish");
        let entries: Vec<String> = std::fs::read_dir(run_folder(dir.path()))
            .expect("read run dir")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec![RENDEZVOUS_FILENAME.to_owned()]);
    }

    /// Removal is idempotent: a graceful shutdown after a crash-cleaned run
    /// folder must not fail, and a second removal must not either.
    #[test]
    fn removing_the_rendezvous_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        remove(dir.path()).expect("removing an absent rendezvous is already the desired state");
        publish(dir.path(), "http://127.0.0.1:1").expect("publish");
        remove(dir.path()).expect("remove");
        assert!(!rendezvous_path(dir.path()).exists(), "a graceful shutdown leaves no pointer");
        remove(dir.path()).expect("remove again");
    }
}
