//! Detached worker spawn — the host half of a detached background worker.
//!
//! One free function, [`spawn_detached`], behind the host executor traits.
//! It is the `unsafe`-free port of the TypeScript `defaultRunner`:
//! `spawn(execPath, argv, { detached: true, stdio: "ignore" })` followed by
//! `child.unref()`.
//!
//! Two properties are load-bearing and each maps to one line below:
//!
//! * **Detached from chiefd's process group.** The child is put in a *new*
//!   process group ([`std::os::unix::process::CommandExt::process_group`] with
//!   `0`), so a signal delivered to the daemon's group never reaches the worker
//!   and the worker survives a chiefd exit — exactly like a runtime pane. This is
//!   the safe equivalent of the TypeScript `detached: true` (which calls
//!   `setsid`); `#![forbid(unsafe_code)]` rules out a `pre_exec(setsid)`.
//! * **Stdio discarded.** A detached worker must not inherit chiefd's terminal
//!   or keep a pipe open that would wedge the daemon — all three descriptors go
//!   to `/dev/null`.
//!
//! The handle is dropped, never waited on (the `unref()` port): the worker is
//! not a chiefd child in the reaping sense. Reaping a detached worker's
//! eventual exit status is the daemon's SIGCHLD concern, not this call's.

use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use crate::redact::redact;
use crate::{HostErr, Pid};

/// Spawn `argv` as a detached background process and return its pid.
///
/// `env` entries are layered *over* the inherited environment (the child keeps
/// chiefd's env and adds/overrides these), matching the TypeScript
/// `{ ...process.env, ...overrides }`.
///
/// # Errors
/// [`HostErr::ToolUnavailable`] when `argv` is empty, the program could not be
/// executed, or the spawned pid does not fit an `i32`.
pub fn spawn_detached(argv: &[String], env: &[(String, String)]) -> Result<Pid, HostErr> {
    let (program, rest) = argv.split_first().ok_or_else(|| HostErr::ToolUnavailable {
        tool: "spawn_detached",
        detail: "empty argv: a detached worker needs a program to run".to_string(),
    })?;

    let mut command = Command::new(program);
    command
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    for (key, value) in env {
        command.env(key, value);
    }

    let child = command.spawn().map_err(|error| HostErr::ToolUnavailable {
        tool: "spawn_detached",
        detail: redact(&error.to_string()),
    })?;
    let pid = i32::try_from(child.id()).map_err(|_| HostErr::ToolUnavailable {
        tool: "spawn_detached",
        detail: "spawned pid does not fit in i32".to_string(),
    })?;
    // #61: the caller must NOT block on the worker — its exit is observed
    // through the expiring durable lease, never through this handle — but
    // "don't block" was implemented as `drop(child)`, and dropping a `Child`
    // in Rust neither kills nor waits: it just abandons the pid. Nothing in a
    // Rust process reaps for you (unlike a Bun/Node runtime, which installs
    // its own SIGCHLD handling), so every worker that exited stayed a
    // `<defunct> chiefd-bin` under the daemon for the daemon's whole life —
    // measured live at ~11.4 per hour. That is also a correctness bug, not
    // just clutter: a zombie keeps its `/proc/<pid>` entry, so any
    // pid-liveness check reads a finished worker as still running.
    //
    // So the child is handed to a monitor thread that performs one blocking
    // `wait`, exactly as #49/#65 do for the extraction and review children.
    // The thread costs nothing while parked, ends when the worker does, and
    // the caller returns immediately — detached in the sense that matters.
    // The PROGRAM only. A detached worker's argv and environment carry the
    // credential material the caller staged for it.
    tracing::info!(
        event = "worker.spawned",
        program = %program,
        pid,
        "spawned a detached worker"
    );
    std::thread::spawn(move || {
        let mut child = child;
        match child.wait() {
            Ok(status) => tracing::info!(
                event = "worker.exited",
                pid,
                exit_code = status.code().unwrap_or(-1),
                "a detached worker exited and was reaped"
            ),
            Err(error) => tracing::warn!(%pid, %error, "could not reap a detached worker"),
        }
    });
    Ok(Pid(pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_argv_is_refused_rather_than_spawning_nothing() {
        let error = spawn_detached(&[], &[]).expect_err("empty argv must not spawn");
        assert!(error.to_string().contains("empty argv"), "{error}");
    }

    /// `/proc/<pid>/stat`'s state field, or `None` once the pid is fully
    /// reaped. The comm field can contain spaces and parens, so the state is
    /// read from after the LAST `)`.
    fn process_state(pid: i32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_comm = stat.rsplit_once(')')?.1;
        after_comm.split_whitespace().next()?.chars().next()
    }

    /// #61: the live leak — ~11.4 defunct `chiefd-bin` children per hour under
    /// the daemon. `drop(child)` abandons the pid without reaping it, and
    /// nothing in a Rust process reaps on its own, so a worker that exits stays
    /// a zombie forever. Fails (state `Z`, indefinitely) without the monitor
    /// thread.
    #[test]
    fn a_detached_worker_that_exits_is_reaped_rather_than_left_defunct() {
        let pid = spawn_detached(&["true".to_string()], &[]).expect("spawn true").0;
        // The child exits immediately; the monitor thread reaps it. Bounded
        // rather than unbounded so a regression fails instead of hanging.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match process_state(pid) {
                None => break, // reaped and gone: the fix works
                Some('Z') | Some(_) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "pid {pid} was never reaped: state {:?}",
                        process_state(pid)
                    );
                    std::thread::yield_now();
                }
            }
        }
    }

    /// The other half of the contract: reaping must not make the spawn block.
    /// A worker that outlives the call is still detached — the caller returns
    /// with its pid while it is very much alive.
    #[test]
    fn spawning_a_long_lived_worker_still_returns_immediately() {
        let started = std::time::Instant::now();
        let pid =
            spawn_detached(&["sleep".to_string(), "30".to_string()], &[]).expect("spawn sleep").0;
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "the spawn must not wait");
        assert!(process_state(pid).is_some(), "the detached worker is still running");
        // Leave nothing behind for the rest of the suite.
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }

    #[test]
    fn spawn_detached_launches_a_real_process_and_returns_a_plausible_pid() {
        // `true` is coreutils, always on PATH; it exits 0 immediately. We are
        // asserting the spawn mechanism (a real, detached child with a real
        // pid), not the worker's behaviour.
        let pid = spawn_detached(
            &["true".to_string()],
            &[("CHIEFD_SPAWN_TEST".to_string(), "1".to_string())],
        )
        .expect("spawn true");
        assert!(pid.0 > 0, "a spawned process has a positive pid, got {pid}");
    }

    #[test]
    fn a_missing_program_is_a_tool_unavailable_error_not_a_panic() {
        let error = spawn_detached(&["/nonexistent/chiefd-worker-xyz".to_string()], &[])
            .expect_err("a missing program cannot be spawned");
        assert!(matches!(error, HostErr::ToolUnavailable { .. }), "{error}");
    }
}
