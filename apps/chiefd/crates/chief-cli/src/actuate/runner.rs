//! Running the `tmux` binary, and waiting between retries.
//!
//! Two seams, both narrow on purpose:
//!
//! * [`TmuxRunner`] — one invocation in, one captured result out. The real
//!   implementation spawns a process; the scripted one in
//!   [`crate::fake`](crate::fake) returns canned results, which is how the
//!   trust rules are tested without a tmux server anywhere near CI.
//! * [`Waiter`] — the retry sleep. chiefd's rule is that no waiting is
//!   hard-coded (`clippy.toml` disallows `std::thread::sleep` outside the
//!   owner module), so the 20 × 25 ms ladder is injectable and the
//!   retry tests are deterministic rather than slow.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::actuate::host::{HostErr, Socket, TmuxCmd, TmuxOut};
use crate::actuate::redact::redact;

/// One tmux invocation against one server socket.
pub trait TmuxRunner: Send + Sync {
    /// Run `tmux -L <socket> <argv…>` and capture the result.
    ///
    /// A non-zero exit is **not** an error: exit status is data the trust
    /// rules classify. Only failing to run tmux at all is an `Err`.
    ///
    /// # Errors
    /// [`HostErr::ToolUnavailable`] when the binary could not be executed.
    fn run(&self, socket: &Socket, cmd: &TmuxCmd) -> Result<TmuxOut, HostErr>;
}

/// The longest a single `tmux` invocation may run before the client is killed
/// and the call reported unavailable. Normally a command answers in tens of
/// milliseconds; the transient retry ladder above this runner assumes a slow
/// server costs at most hundreds. A tmux CLIENT can hang forever when its
/// server accepts the connection and never answers (a server dying mid-read,
/// or one so overloaded it never schedules the reply) — and the caller is a
/// duty task awaiting the pass SYNCHRONOUSLY, so one hung invocation wedged
/// that duty for the rest of the process's life: no cycle starts, no
/// refusals, no log lines, and `supervise` cannot help because the task
/// never finishes (the measured arch-impl duty wedge: a daemon-converged
/// company stop then times out loudly against a duty that looks healthy in
/// every durable gauge). Ten seconds is far past any real answer and far
/// inside the 30 s stop-convergence window, so a transiently stuck server
/// degrades to one loudly-refused pass and the duty keeps cycling.
const DEFAULT_INVOCATION_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the bounded wait polls the child. Short enough that a real
/// answer is picked up promptly; the sanctioned sleep seam (`ThreadWaiter`)
/// is what actually blocks.
const TIMEOUT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The real runner: spawns `tmux`, bounded by [`DEFAULT_INVOCATION_TIMEOUT`].
#[derive(Debug, Clone)]
pub struct SystemTmuxRunner {
    binary: PathBuf,
    timeout: Duration,
}

impl Default for SystemTmuxRunner {
    /// Deliberate PATH resolution, audited and left. Unlike `pi` and `bun` —
    /// where PATH picked the WRONG build or nothing at all — `tmux` is a host
    /// prerequisite the launcher already probes before delegating
    /// (`operator.rs`'s `LAUNCHER_PREREQUISITES`), and chiefd inherits the
    /// launcher's full environment, so this resolves today on both platforms
    /// (verified against the live daemon: `/opt/homebrew/bin` is on its PATH).
    /// It would become reachable-and-wrong if chiefd were ever started with a
    /// scrubbed PATH, or if two tmux builds had to be told apart; use
    /// [`SystemTmuxRunner::with_binary`] then, as the e2e harness already does.
    fn default() -> Self {
        Self { binary: PathBuf::from("tmux"), timeout: DEFAULT_INVOCATION_TIMEOUT }
    }
}

impl SystemTmuxRunner {
    /// A runner using a specific tmux binary. The e2e harness pins one.
    #[must_use]
    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        Self { binary: binary.into(), timeout: DEFAULT_INVOCATION_TIMEOUT }
    }

    /// A runner with a non-default per-invocation bound. Tests use this to
    /// exercise the kill path without waiting out the production bound.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl TmuxRunner for SystemTmuxRunner {
    fn run(&self, socket: &Socket, cmd: &TmuxCmd) -> Result<TmuxOut, HostErr> {
        let mut child = Command::new(&self.binary)
            .arg("-L")
            .arg(&socket.0)
            .args(&cmd.argv)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| HostErr::ToolUnavailable {
                tool: "tmux",
                detail: redact(&error.to_string()),
            })?;
        let started = std::time::Instant::now();
        let output = loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    break child.wait_with_output().map_err(|error| HostErr::ToolUnavailable {
                        tool: "tmux",
                        detail: redact(&error.to_string()),
                    })?;
                }
                Ok(None) => {
                    if started.elapsed() >= self.timeout {
                        // The client is hung past any real answer. Kill AND
                        // reap it (`wait`, not `drop` — a dropped child is a
                        // zombie, spawn.rs's #61 lesson), then report the
                        // invocation unavailable: the trust rules classify
                        // the pass closed, the duty logs the refusal, and its
                        // next wake retries — a bounded loud skip instead of
                        // a silent permanent wedge.
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(HostErr::ToolUnavailable {
                            tool: "tmux",
                            detail: redact(&format!(
                                "tmux did not answer within {}ms; the hung client was killed",
                                self.timeout.as_millis(),
                            )),
                        });
                    }
                    ThreadWaiter.wait(TIMEOUT_POLL_INTERVAL);
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(HostErr::ToolUnavailable {
                        tool: "tmux",
                        detail: redact(&error.to_string()),
                    });
                }
            }
        };
        Ok(TmuxOut {
            // A tmux killed by a signal has no exit code; `-1` is not a status
            // any trust rule accepts as authoritative, which is the point.
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: redact(&String::from_utf8_lossy(&output.stderr)),
        })
    }
}

/// The retry sleep, injectable so tests never really wait.
pub trait Waiter: Send + Sync {
    /// Block for `d`.
    fn wait(&self, d: Duration);
}

/// Production waiter: blocks the calling thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadWaiter;

impl Waiter for ThreadWaiter {
    fn wait(&self, d: Duration) {
        // The host executor is the one module allowed to block: it shells out
        // to tmux synchronously and this is the ported 25 ms transient ladder
        // (`org-runtime-ownership.ts:149`). Every other wait in chiefd goes
        // through the injected clock — see `clippy.toml`.
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(d);
    }
}

/// A waiter that records what it was asked to wait for and returns instantly.
///
/// Not test-gated: the e2e harness and the conformance runner both need it,
/// and it is inert (it cannot make a production build wait *less* than it
/// otherwise would — it is only reachable if wired in deliberately).
#[derive(Debug, Default)]
pub struct RecordingWaiter {
    waits: std::sync::Mutex<Vec<Duration>>,
}

impl RecordingWaiter {
    /// Every wait requested, in order.
    #[must_use]
    pub fn waits(&self) -> Vec<Duration> {
        self.waits.lock().map(|w| w.clone()).unwrap_or_default()
    }

    /// Total time this waiter was asked to sleep.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.waits().into_iter().sum()
    }
}

impl Waiter for RecordingWaiter {
    fn wait(&self, d: Duration) {
        if let Ok(mut waits) = self.waits.lock() {
            waits.push(d);
        }
    }
}

#[cfg(test)]
mod tests {
    // Fixture staging (the stub tmux script below) writes files in a tempdir;
    // `std::fs::write` is seam-disallowed in production but sanctioned in
    // tests (same allow as authn/boot.rs).
    #![allow(clippy::disallowed_methods)]
    use std::time::Duration;

    use crate::actuate::*;

    #[test]
    fn the_recording_waiter_returns_immediately_and_keeps_the_ladder() {
        let waiter = RecordingWaiter::default();
        let started = std::time::Instant::now();
        waiter.wait(Duration::from_secs(30));
        waiter.wait(Duration::from_millis(25));
        assert!(started.elapsed() < Duration::from_secs(1), "the recording waiter never sleeps");
        assert_eq!(waiter.waits(), vec![Duration::from_secs(30), Duration::from_millis(25)]);
        assert_eq!(waiter.total(), Duration::from_millis(30_025));
    }

    #[test]
    fn a_missing_tmux_binary_is_tool_unavailable_not_a_silent_absence() {
        let runner = SystemTmuxRunner::with_binary("/nonexistent/tmux-binary-for-tests");
        let error = runner
            .run(&Socket("chiefd-test".into()), &TmuxCmd { argv: vec!["has-session".into()] })
            .expect_err("no such binary");
        assert!(matches!(error, HostErr::ToolUnavailable { tool: "tmux", .. }));
    }

    #[test]
    fn a_nonzero_exit_is_data_not_an_error() {
        // `false` exits 1 and prints nothing: the runner must hand that to the
        // trust rules rather than deciding for them.
        let runner = SystemTmuxRunner::with_binary("false");
        let out = runner
            .run(&Socket("chiefd-test".into()), &TmuxCmd { argv: Vec::new() })
            .expect("`false` runs");
        assert_eq!(out.status, 1);
        assert_eq!(
            super::super::trust::classify(out.status, &out.stderr),
            super::super::trust::Trust::Untrusted,
            "an unexplained non-zero exit is untrusted"
        );
    }

    /// The duty-wedge regression (arch-impl/wedge): a tmux client that never
    /// answers must be killed and reported unavailable within the bound — the
    /// unbounded `Command::output()` this replaced let one hung invocation
    /// wedge its duty task for the process's life (no cycle starts, no
    /// refusals, no logs; `supervise` only respawns DEAD tasks). The call
    /// runs on its own thread with a bounded join so this test FAILS FAST
    /// against the unfixed runner instead of hanging the suite forever.
    #[test]
    fn an_invocation_that_never_answers_is_killed_and_reported_unavailable() {
        use std::os::unix::fs::PermissionsExt as _;

        // A stub "tmux" that answers nothing, forever, and records its pid so
        // the test can prove the hung client was reaped. `std::fs::write` is
        // workspace-banned outside crate::actuate::files; tests write fixtures
        // directly (same pattern as authn/boot.rs).
        let dir = tempfile::tempdir().expect("tempdir");
        let stub = dir.path().join("tmux-stub");
        let pidfile = dir.path().join("stub.pid");
        std::fs::write(
            &stub,
            format!("#!/bin/sh\necho $$ > {}\nexec sleep 30\n", pidfile.display()),
        )
        .expect("write stub");
        let mut permissions = std::fs::metadata(&stub).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&stub, permissions).expect("chmod stub");

        // ETXTBSY, retried rather than tolerated as a flake. This test writes an
        // executable and then execs it, and it runs inside a MULTI-THREADED test
        // binary that is spawning processes on other threads throughout. Linux
        // refuses to exec a file any process holds open for writing; between a
        // sibling thread's `fork` and its `exec`, the child holds an inherited
        // copy of every fd open at that instant, including this file's. The
        // window is microseconds and the suite hits it a few times a day —
        // which is exactly how it read as one of the "intermittent, passes in
        // isolation, fails in the full workspace run" reds that got attributed
        // to tmux socket contention in a full `/tmp`. It is neither tmux nor the
        // disk: the failure names itself, `Text file busy (os error 26)`.
        //
        // The retry is in the TEST because the defect is in the test's own
        // arrangement. `SystemTmuxRunner` execs an operator's installed tmux,
        // which nothing is concurrently writing; teaching production code to
        // retry an exec would be carrying a fixture's problem into the product.
        let mut attempt = 0;
        let (outcome, elapsed) = loop {
            let runner =
                SystemTmuxRunner::with_binary(&stub).with_timeout(Duration::from_millis(150));
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                let outcome = runner.run(
                    &Socket("chiefd-test".into()),
                    &TmuxCmd { argv: vec!["list-panes".into()] },
                );
                let _ = tx.send((outcome, started.elapsed()));
            });
            let answered = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the tmux invocation is bounded: the unfixed runner hangs here forever");
            let busy = answered
                .0
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("Text file busy"));
            attempt += 1;
            // Bounded, and it fails as ETXTBSY rather than as a timeout if the
            // diagnosis above is ever wrong — a retry loop that swallowed the
            // real error would be its own instrument-that-cannot-see-its-subject.
            if !busy || attempt >= 20 {
                break answered;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        assert!(elapsed < Duration::from_secs(10), "bounded, got {elapsed:?}");
        let error = outcome.expect_err("a hung invocation is reported, never answered");
        assert!(
            matches!(&error, HostErr::ToolUnavailable { tool, .. } if *tool == "tmux"),
            "unavailable, got {error}"
        );
        assert!(
            error.to_string().contains("did not answer within 150ms"),
            "names the bound, got {error}"
        );
        // The hung client was killed AND reaped: its pid must be gone from
        // /proc (a dropped child is a zombie or a leak — spawn.rs's #61
        // lesson).
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("stub recorded its pid")
            .trim()
            .parse()
            .expect("pid parses");
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "the hung tmux client (pid {pid}) was left running or unreaped"
        );
    }
}
