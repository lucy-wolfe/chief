//! The owner-death watchdog, proven against a REAL spawned `beacond`.
//!
//! `src/watchdog.rs`'s unit tests pin the decision — which env value arms the
//! watch, which is refused — against an injected environment and an injected
//! liveness probe. That is the right shape for the decision and it proves
//! nothing about the daemon: a `Decision::Watching` that no code path ever acts
//! on would satisfy every one of them. This file spawns the actual binary, kills
//! the actual owner it was told to watch, and waits for the actual process to
//! be gone.
//!
//! #987 is the reason it exists. A test-owned `beacond` was measured still
//! running **eight to twelve hours** after the vitest worker that spawned it
//! was SIGKILLed, because the only thing that reaped it was a `stop()` the
//! runner never reached. A watchdog nobody proved fires would leave exactly
//! that outcome in place while reading as fixed.

// The same file-level exemption `chiefd-daemon/tests/parent_death_watchdog.rs`
// takes for the sibling mechanism, and for the same reasons. A test that proves
// a real process dies must sleep on the real clock — an injected one would
// prove the test's own arithmetic — and must `expect` on its spawns, because a
// harness that cannot start its subject has nothing to report but a failure.
#![allow(clippy::disallowed_methods, clippy::expect_used)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long a correct watchdog may take: the 500 ms poll, plus generous room
/// for a loaded build host. Comfortably under any value that would let this
/// pass while the daemon is actually orphaned.
const MUST_EXIT_WITHIN: Duration = Duration::from_secs(10);

/// A process that stands in for the test runner that owns a `beacond`.
///
/// `sleep` is used rather than a second `beacond` because the owner's only
/// required property is that it is alive until killed, and this keeps the test
/// independent of beacond's own boot.
fn spawn_owner() -> Child {
    Command::new("sleep")
        .arg("300")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the stand-in owner process")
}

fn spawn_beacond(db_dir: &std::path::Path, watch_pid: Option<u32>) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_beacond"));
    command
        // Port 0 so concurrent runs never contend for one, and the registry in
        // a temp directory so this test shares no state with an operator's own.
        .env("BEACOND_BIND", "127.0.0.1:0")
        .env("BEACOND_DB_PATH", db_dir.join("beacond.sqlite"))
        .env("HOME", db_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match watch_pid {
        Some(pid) => command.env("BEACOND_WATCH_PID", pid.to_string()),
        // Explicitly cleared rather than merely unset: the test runner's own
        // environment must not be able to arm or disarm the subject.
        None => command.env_remove("BEACOND_WATCH_PID"),
    };
    command.spawn().expect("spawn beacond")
}

/// Poll until the child is gone, returning whether it exited in time.
fn exited_within(child: &mut Child, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match child.try_wait().expect("poll the child") {
            Some(_) => return true,
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    false
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a_beacond_watching_its_owner_exits_when_that_owner_is_sigkilled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut owner = spawn_owner();
    let mut daemon = spawn_beacond(dir.path(), Some(owner.id()));

    // Let it get past config, registry open and bind, so what is proven is a
    // running daemon exiting rather than one that never started.
    std::thread::sleep(Duration::from_millis(750));
    assert!(
        daemon.try_wait().expect("poll beacond").is_none(),
        "beacond exited before its owner died — this test would prove nothing"
    );

    // SIGKILL, not SIGTERM: the whole point is the death the owner cannot
    // handle, clean up after, or notify anybody about.
    kill_and_reap(&mut owner);

    let exited = exited_within(&mut daemon, MUST_EXIT_WITHIN);
    if !exited {
        kill_and_reap(&mut daemon);
    }
    assert!(
        exited,
        "beacond outlived the owner named by BEACOND_WATCH_PID — this is the #987 orphan, which was measured at 8-12 hours on a shared build host"
    );
}

#[test]
fn a_beacond_told_to_watch_an_already_dead_owner_exits_immediately() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut owner = spawn_owner();
    let dead_pid = owner.id();
    kill_and_reap(&mut owner);

    let mut daemon = spawn_beacond(dir.path(), Some(dead_pid));

    let exited = exited_within(&mut daemon, MUST_EXIT_WITHIN);
    if !exited {
        kill_and_reap(&mut daemon);
    }
    assert!(
        exited,
        "beacond kept running for an owner that was already gone when it started — the startup race the pre-poll check exists to close"
    );
}

#[test]
fn a_beacond_with_no_watched_owner_keeps_running() {
    // The other direction, and the one that matters to an operator: an
    // unset variable must leave the daemon alone. A watchdog that defaulted to
    // watching something would make a real registry exit on a process it was
    // never given.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut daemon = spawn_beacond(dir.path(), None);

    let exited = exited_within(&mut daemon, Duration::from_secs(2));
    kill_and_reap(&mut daemon);
    assert!(!exited, "an unwatched beacond exited on its own");
}
