//! #504: attribute the actor behind chiefd's graceful shutdown.
//!
//! chiefd on cobalt restarts tens of times per day (measured: 37 on 2026-07-24),
//! every one a deliberate, graceful SIGTERM — not a crash — and nothing recorded
//! WHO sent it. The missing control is the bug: something chooses to stop the
//! daemon dozens of times a day and there was no actor trail.
//!
//! tokio's signal API cannot expose the sender: it collapses a signal to a
//! bare "it happened" wakeup with no `siginfo_t`. So this installs an ADDITIONAL
//! SIGTERM handler through `signal-hook` (whose `WithOrigin` exfiltrator carries
//! `si_pid`), on a dedicated thread. Registration multiplexes through
//! `signal-hook-registry` — the SAME registry tokio's `signal(SIGTERM)` uses — so
//! BOTH handlers fire on one SIGTERM: tokio still drives the graceful drain
//! exactly as before, and this thread records the sender. The signal is never
//! blocked, so nothing about delivery changes.
//!
//! At shutdown the daemon resolves the recorded pid against `/proc` and logs the
//! sender's pid, exe, cwd and cmdline. A shutdown with NO recorded sender is
//! itself logged as an anomaly (acceptance #2), so "unattributable" becomes a
//! visible event rather than a silence.
//!
//! Linux-only (#841): this reads `/proc/<pid>/...` directly and depends on
//! signal-hook's `WithOrigin` exfiltrator, which requires the "extended-siginfo"
//! feature — a feature that is itself unavailable without a C-compiled build
//! step (D17: `apps/chiefd/Cargo.toml` drops it for the Darwin cross-target
//! check, since that build script rejects the Darwin-only cc flags on this
//! Linux fleet). The attribution was never functional on macOS regardless —
//! `/proc` does not exist there — so `#[cfg(target_os = "linux")]` states a
//! constraint this module already had rather than papering over the check.
//! The `not(linux)` stub below exists only so the crate type-checks on other
//! targets; the Linux implementation is unchanged. #841 tracks re-checking
//! this gate on every signal-hook upgrade, and a real Darwin implementation if
//! macOS ever becomes a runtime target rather than a compile-check target.
//!
//! Re-checked 2026-08-04 against signal-hook 0.4.4 (the current latest;
//! this crate still pins 0.3.18), reading the published `Cargo.toml`
//! directly rather than trusting a summary: `extended-siginfo = ["channel",
//! "iterator", "extended-siginfo-raw"]` and `extended-siginfo-raw = ["cc"]`
//! are unchanged across the 0.3 → 0.4 line — `WithOrigin` still has no path
//! that skips the C build. This gate remains necessary; bumping the
//! dependency would not remove it. Re-check again on the next signal-hook
//! upgrade, from the crate's own `Cargo.toml`, not a changelog summary.

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::time::Duration;

    use signal_hook::consts::signal::SIGTERM;
    use signal_hook::iterator::exfiltrator::WithOrigin;
    use signal_hook::iterator::SignalsInfo;

    /// The pid of the process that sent the most recent SIGTERM, or `0` when none
    /// has been observed. Written only by the attribution thread, read at shutdown.
    static SIGTERM_SENDER_PID: AtomicI32 = AtomicI32::new(0);

    /// Install the SIGTERM sender-attribution handler on a dedicated thread.
    ///
    /// Best-effort and non-fatal: if the handler cannot be installed the daemon
    /// still runs and shuts down exactly as before — the sender is simply recorded
    /// as unattributable, which [`log_shutdown_actor`] then reports as an anomaly.
    /// Idempotent enough for the daemon's single call; a second call would just add
    /// a second (harmless) recorder.
    pub fn install() {
        let mut signals = match SignalsInfo::<WithOrigin>::new([SIGTERM]) {
            Ok(signals) => signals,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not install SIGTERM attribution; a shutdown's actor will be unrecorded (#504)"
                );
                return;
            }
        };
        let spawned = std::thread::Builder::new()
            .name("chiefd-sigterm-attribution".to_string())
            .spawn(move || {
                // Blocks until a SIGTERM arrives (then again, though the daemon shuts
                // down on the first). `origin.process` is `None` only for a signal
                // with no `si_pid` (e.g. a kernel-synthesized one); a `kill(2)` from
                // another process always carries it.
                for origin in &mut signals {
                    let pid = origin.process.map_or(0, |process| process.pid);
                    SIGTERM_SENDER_PID.store(pid, Ordering::SeqCst);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not spawn the SIGTERM attribution thread (#504)");
        }
    }

    /// The recorded SIGTERM sender pid, or `None` when none was observed.
    #[must_use]
    pub fn recorded_sender_pid() -> Option<i32> {
        match SIGTERM_SENDER_PID.load(Ordering::SeqCst) {
            0 => None,
            pid => Some(pid),
        }
    }

    /// One `/proc/<pid>` field, or `None` when it cannot be read (the sender may
    /// already have exited by shutdown time — common for a `kill && exit` deployer).
    fn proc_link(pid: i32, field: &str) -> Option<String> {
        std::fs::read_link(format!("/proc/{pid}/{field}"))
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    }

    /// The sender's `/proc/<pid>/cmdline`, NUL-separated on disk, rendered with
    /// spaces and bounded so a pathological argv can never flood the log.
    fn proc_cmdline(pid: i32) -> Option<String> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        if raw.is_empty() {
            return None;
        }
        let mut text: String = raw
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        const MAX: usize = 512;
        if text.len() > MAX {
            text.truncate(MAX);
            text.push('…');
        }
        Some(text)
    }

    /// Log the shutdown's actor: the SIGTERM sender's pid + `/proc` identity, or a
    /// loud anomaly line when no sender was recorded (acceptance #1 and #2).
    ///
    /// The attribution thread and tokio's shutdown wakeup race off the SAME signal,
    /// so the recorded pid may land a hair after `wait_for_signal` returns; a short
    /// bounded poll closes that window without ever blocking meaningfully (the pid
    /// is stored microseconds after delivery). Kept async so the wait is a tokio
    /// sleep on the shutdown path, not a blocked thread.
    pub async fn log_shutdown_actor(company: &str) {
        let mut pid = recorded_sender_pid();
        for _ in 0..10 {
            if pid.is_some() {
                break;
            }
            // Sanctioned real sleep (not the injected Clock): this bounded poll
            // closes a race against actual OS SIGTERM delivery — the pid is stored
            // microseconds after the kernel hands us the signal, on wall-clock time
            // a mock Clock cannot model. See clippy.toml's `tokio::time::sleep` ban.
            #[allow(clippy::disallowed_methods)]
            tokio::time::sleep(Duration::from_millis(5)).await;
            pid = recorded_sender_pid();
        }
        let Some(pid) = pid else {
            tracing::warn!(
                company = %company,
                "shutdown by an UNRECORDED actor: SIGTERM carried no sender pid, or arrived before \
                 attribution was installed — an anomaly, not a normal restart (#504)"
            );
            return;
        };
        tracing::info!(
            company = %company,
            sender_pid = pid,
            sender_exe = proc_link(pid, "exe").as_deref().unwrap_or("<gone>"),
            sender_cwd = proc_link(pid, "cwd").as_deref().unwrap_or("<gone>"),
            sender_cmdline = proc_cmdline(pid).as_deref().unwrap_or("<gone>"),
            "shutdown actor attributed (#504): this SIGTERM was sent by the named process"
        );
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A self-sent SIGTERM is attributed to THIS process. Installing the handler
        /// first also stops the signal from taking its default (terminate) action,
        /// so the test process survives to read the recorded pid. Deterministic: the
        /// sender of a `kill(getpid(), SIGTERM)` is unambiguously our own pid.
        #[test]
        fn a_self_sent_sigterm_is_attributed_to_this_process() {
            install();
            // Give the attribution thread a moment to register its handler before we
            // raise the signal (registration happens inside `SignalsInfo::new`, which
            // has returned by the time `install` spawns the thread, but the thread
            // must be iterating to consume it).
            // Sanctioned real sleep: this waits on a real OS signal-handler thread
            // reaching its blocking read, not on injected-Clock time — a mock clock
            // cannot model kernel signal delivery. See clippy.toml's sleep ban.
            #[allow(clippy::disallowed_methods)]
            std::thread::sleep(Duration::from_millis(50));

            let me = std::process::id() as i32;
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(me),
                nix::sys::signal::Signal::SIGTERM,
            )
            .expect("send SIGTERM to self");

            let mut recorded = None;
            for _ in 0..200 {
                recorded = recorded_sender_pid();
                if recorded.is_some() {
                    break;
                }
                // Sanctioned real sleep: polls for the attribution thread to record
                // a genuine OS SIGTERM's sender pid — wall-clock delivery timing a
                // mock Clock cannot model. See clippy.toml's sleep ban.
                #[allow(clippy::disallowed_methods)]
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                recorded,
                Some(me),
                "a self-sent SIGTERM must be attributed to this process"
            );
        }
    }
}

/// Stub for every non-Linux compile target (#841): `/proc` does not exist
/// there, so this attribution was never functional off Linux. Exists only so
/// the crate type-checks under the Darwin cross-target check (D17); the
/// Linux implementation above is unchanged.
///
/// This `mod imp` deliberately mirrors the Linux one's surface — `install`,
/// `recorded_sender_pid`, `log_shutdown_actor` — so a future caller can never
/// compile against one target and fail against the other (#884). Kept even
/// though nothing calls `recorded_sender_pid` here today: the top-level
/// `pub use imp::{install, log_shutdown_actor}` doesn't re-export it, and
/// this stub's `log_shutdown_actor` doesn't call it internally the way the
/// Linux one does — but deleting it would make the two `imp` modules
/// structurally diverge, so the next widened `pub use` or the next caller
/// reaching for `imp::recorded_sender_pid` compiles on Linux and breaks only
/// on Darwin, in a packet that has nothing to do with either.
#[cfg(not(target_os = "linux"))]
mod imp {
    pub fn install() {}

    /// Always `None` off Linux: there is no `/proc` to have read a sender
    /// pid from. Kept for parity with the Linux `imp`, not currently called
    /// from this stub — see the module doc comment above.
    #[allow(dead_code)]
    #[must_use]
    pub fn recorded_sender_pid() -> Option<i32> {
        None
    }

    pub async fn log_shutdown_actor(_company: &str) {}
}

pub use imp::{install, log_shutdown_actor};
