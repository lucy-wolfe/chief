//! The real-kernel regression test for od:idle-cpu #285: a spawned `chiefd`
//! daemon must exit when its PARENT dies, driven by `PR_SET_PDEATHSIG` — zero
//! polling, zero thread — instead of the former 1Hz `parent_id() == 1` spin.
//!
//! The mode spawned below used to be `chiefd docstore-only`. That mode is
//! deleted; the watchdogs it carried are not — they moved to
//! `src/watchdogs.rs` and `run.rs` installs BOTH as the first thing `run` does,
//! before it parses a single argument. So the behaviour these two tests pin is
//! unchanged and now covers every boot of the daemon rather than one retired
//! mode. `run --serve-only` is what they drive: it is the cheapest long-lived
//! `run` (a read-only snapshot reader that never contacts beacond), so the
//! process under test stays alive for the whole window on nothing but a temp
//! directory and an ephemeral loopback port.
//!
//! An integration test (not a unit test in `src/watchdogs.rs`) because it
//! needs `CARGO_BIN_EXE_chiefd`, which cargo only sets for targets that are
//! NOT the binary's own unit-test build.

// This test drives a real second process tree and measures genuine wall-clock
// waiting for a real kernel signal to arrive — exactly the "separate-process
// locktest budgets" exception `chiefd_core::clock`'s own docs carve out for
// the injected-Clock rule. There is no ledger, no `Ledgers::now()`, and
// nothing here could be driven by a `ManualClock`.
#![allow(clippy::disallowed_methods)]
// Both tests below are `#[cfg(target_os = "linux")]` (they drive real
// `/proc` entries and `PR_SET_PDEATHSIG`), but that attribute is per-item —
// it does not make the file itself conditional. Under the Darwin cross-target
// check (#884, D17's `scripts/cargo-check-macos.sh`) both test fns compile
// away entirely, leaving the shared helpers below with no caller in this file
// and triggering unused-item warnings. Gating the whole file keeps them scoped
// to exactly the target that uses it.
#![cfg(target_os = "linux")]

use std::io::BufRead;

/// The `run` invocation both tests background.
///
/// `--serve-only` is deliberate: it returns from `run` before beacond
/// admission, so no registry process has to exist for the daemon under test to
/// come up and stay up, and the watchdogs it is here to prove were already
/// installed several statements earlier — `run` arms them ahead of
/// `parse_config`, so they do not depend on which mode was asked for.
///
/// `>/dev/null 2>&1` is NOT tidiness. The backgrounded daemon would otherwise
/// inherit the test harness's own stdout and stderr, which under `cargo test`
/// are the pipe cargo reads its output from — and cargo's reader does not see
/// EOF until the LAST holder of that pipe closes it. A daemon that outlived its
/// test by even a few seconds would therefore stall the whole `cargo test
/// --workspace` run, and the stall would be attributed to whichever suite
/// happened to be last. These two tests spawn processes that are SUPPOSED to
/// outlive the statement that starts them, so they are exactly the tests that
/// must not hand out that file descriptor.
fn serve_only_command(bin: &str, dir: &std::path::Path) -> String {
    format!(
        "'{bin}' run --dir '{dir}' --launcher-root '{dir}' \
         --pi-binary /opt/pi/bin/pi --serve-only >/dev/null 2>&1",
        dir = dir.display()
    )
}

/// A spawned child that is killed and reaped even if the test unwinds.
///
/// Every assertion between spawning the watched placeholder and killing it is a
/// panic that would otherwise strand a live process: `Command` has no `Drop`
/// that kills, so an unwind simply forgets the child. Stranding it is not a
/// cosmetic leak — the placeholder sleeps for 300 seconds, and a stray process
/// holding an inherited descriptor is the class this whole file exists to test.
struct KillOnDrop(std::process::Child);

impl KillOnDrop {
    fn id(&self) -> u32 {
        self.0.id()
    }

    /// Kill and reap now, for the test's own deliberate kill.
    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Block until `pid` leaves `/proc`, or fail with `message` at the deadline.
fn wait_for_exit(child_pid: i32, timeout: std::time::Duration, message: &str) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if !std::path::Path::new(&format!("/proc/{child_pid}")).exists() {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{message}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Two process levels are needed because `PR_SET_PDEATHSIG` fires on
/// reparenting: an intermediate `sh` backgrounds the daemon as ITS child, then
/// this test SIGKILLs the intermediate (unsheddable, so it cannot forward any
/// signal of its own) — the grandchild is reparented to init and the kernel
/// delivers the armed `SIGTERM` itself. The grandchild must exit on its own
/// within a bounded window, with no assistance from this test beyond the kill.
#[test]
#[cfg(target_os = "linux")]
fn parent_death_via_pdeathsig_exits_the_reparented_grandchild() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_chiefd");

    // `exec` is NOT used for the backgrounded daemon: `sh -c '... &'` makes sh
    // itself its parent, which is exactly the process we are about to kill out
    // from under it.
    let script = format!("{} & echo $!; wait", serve_only_command(bin, dir.path()));
    let mut shell = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .env("CHIEFD_STORE_BIND", "127.0.0.1:0")
        .env("CHIEFD_STORE_EXIT_WITH_PARENT", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn intermediate shell");

    let stdout = shell.stdout.take().expect("piped stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let mut pid_line = String::new();
    reader.read_line(&mut pid_line).expect("read grandchild pid");
    let child_pid: i32 = pid_line.trim().parse().expect("a pid on the first line");

    // Give the daemon a moment to actually install the watchdog (the
    // reparent-race re-check happens once, immediately, at startup).
    std::thread::sleep(std::time::Duration::from_millis(300));

    // IT IS ALIVE BEFORE THE KILL. Without this the test passes vacuously for
    // any daemon that exited on its own — a bad argument, a failed bind — and
    // would report a dead PDEATHSIG as working.
    assert!(
        std::path::Path::new(&format!("/proc/{child_pid}")).exists(),
        "the chiefd daemon (pid {child_pid}) must still be running before its parent is killed, \
         or the exit below proves nothing about PR_SET_PDEATHSIG"
    );

    // SIGKILL the intermediate: unsheddable, so it cannot forward any signal
    // to its child before dying. Only the kernel-delivered PDEATHSIG this fix
    // installs can save the grandchild from becoming a silently-orphaned
    // process.
    let _ = std::process::Command::new("kill").arg("-9").arg(shell.id().to_string()).status();
    let _ = shell.wait();

    wait_for_exit(
        child_pid,
        std::time::Duration::from_secs(5),
        &format!(
            "the reparented chiefd daemon (pid {child_pid}) did not exit on parent death within \
             5s — PR_SET_PDEATHSIG did not fire"
        ),
    );
}

/// The real-kernel regression test for U16 (shard-2 write-service death
/// cascade): a daemon genuinely double-forked away from its spawner (so it is
/// NOT that spawner's OS child, and `PR_SET_PDEATHSIG` would never fire for
/// it) must still self-exit once the NAMED watched pid dies, proving
/// `CHIEFD_STORE_WATCH_PID` actually reaps rather than merely compiling. Per
/// the fix design: watch a throwaway process (not this test binary, which must
/// keep running), kill it directly, and confirm the daemon exits within the
/// poll interval's bound.
#[test]
#[cfg(target_os = "linux")]
fn watch_pid_self_exits_once_the_named_pid_dies_even_when_not_the_os_parent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_chiefd");

    // The watched pid: a plain `sleep`, unrelated to the daemon's own process
    // tree, standing in for "the original bun test process." Killing it must
    // reap the daemon even though it was never that daemon's OS parent.
    //
    // Its streams go to /dev/null for the reason spelled out on
    // `serve_only_command`: inherited, they are cargo's own output pipe, and
    // this placeholder is the process most likely to escape — every panic path
    // between here and the kill below leaves it running for its full 300s,
    // holding that pipe open and stalling the entire workspace run long after
    // the test that leaked it has been reported as passed.
    let mut watched = KillOnDrop(
        std::process::Command::new("sleep")
            .arg("300")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the watched placeholder process"),
    );
    let watched_pid = watched.id();

    // A genuine double-fork: the (non-interactive) shell backgrounds the
    // daemon as ITS child, prints its pid, and exits — a background job of a
    // non-interactive shell is not SIGHUP'd on the shell's own exit, so the
    // daemon is reparented to init the instant this shell process ends,
    // before this test ever observes it — exactly the U16 spawn shape. If
    // PR_SET_PDEATHSIG were armed here it would self-kill the daemon almost
    // instantly; `CHIEFD_STORE_EXIT_WITH_PARENT` is deliberately NOT set.
    let script = format!("{} & echo $!", serve_only_command(bin, dir.path()));
    let mut shell = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .env("CHIEFD_STORE_BIND", "127.0.0.1:0")
        .env("CHIEFD_STORE_WATCH_PID", watched_pid.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the double-fork wrapper");

    let stdout = shell.stdout.take().expect("piped stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let mut pid_line = String::new();
    reader.read_line(&mut pid_line).expect("read grandchild pid");
    let child_pid: i32 = pid_line.trim().parse().expect("a pid on the first line");
    let _ = shell.wait();

    // Give the daemon a moment to install the watchdog and confirm it does NOT
    // self-exit merely because it was reparented (the double-fork itself must
    // be survivable — this is the exact case PDEATHSIG could not handle).
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        std::path::Path::new(&format!("/proc/{child_pid}")).exists(),
        "the double-forked chiefd daemon (pid {child_pid}) must survive its own reparent to init \
         — it is not what CHIEFD_STORE_WATCH_PID watches"
    );

    // Kill through the handle rather than shelling out to `kill(1)`: the
    // shell-out's status was discarded, so a kill that never landed read
    // exactly like one that did, and the reap that follows it is what turns
    // the placeholder from a zombie into a gone process.
    watched.kill();

    wait_for_exit(
        child_pid,
        std::time::Duration::from_secs(3),
        &format!(
            "the double-forked chiefd daemon (pid {child_pid}) did not exit within 3s of its \
             named watch pid ({watched_pid}) dying — CHIEFD_STORE_WATCH_PID did not reap it"
        ),
    );
}
