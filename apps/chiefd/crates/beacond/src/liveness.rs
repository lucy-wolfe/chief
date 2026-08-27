//! The admission liveness judge.
//!
//! beacond may judge a pid's liveness with a bare `kill(pid, 0)`, and needs
//! no hostname or process-start-time comparison, because it binds loopback:
//! every registrant reaches beacond over `127.0.0.1`, so every pid it is
//! handed is a process on beacond's own machine. That reasoning breaks the
//! day beacond ever binds a routable address — this judge would need a host
//! comparison at that point, the same way `chiefd-host`'s `owner_is_live`
//! needs one today for a genuinely cross-host marker file.
//!
//! Portable by construction: `nix::sys::signal::kill` is POSIX and behaves
//! identically on both of the repo's build targets, so this module needs no
//! per-platform conditional compilation and reads no Linux-specific process
//! filesystem. The one accepted gap, stated rather than hidden: a recycled
//! pid can make a dead location read live. It is bounded in practice because
//! every real chiefd refreshes `lastSeenAt` on its liveness tick, so the
//! window is one boot wide.

/// This machine's name, via POSIX `gethostname(3)`.
///
/// # Why it lives in beacond
///
/// It is the value written into a registration's `hostname` column and read
/// back out of it, and the writer and the reader are two different programs:
/// `chiefd` REPORTS it at registration, and the `chiefd` operator client
/// COMPARES a registration against it to judge liveness. They used to be one
/// binary sharing one `pub(crate)` function; the P6 operator-client split
/// divided them, and the client links none of the daemon's crates. **This is the ONE definition**, and a second
/// implementation that disagreed by a suffix or a trim would make every
/// registration read as foreign — which is not a hypothetical, see below.
///
/// # Why not `/proc`
///
/// This read `/proc/sys/kernel/hostname` and fell back to the literal
/// `"unknown"` when that failed — "so no extra dependency or feature gate is
/// needed". `/proc` is LINUX-ONLY. On macOS the read always fails, so every
/// company a Mac registered was recorded on host `"unknown"`, and the liveness
/// judge reads that as an unnameable host and refuses to start it: a company
/// could be created once and never attached to again. It compiled fine on both
/// platforms, which is exactly the failure mode `CLAUDE.md`'s cross-platform
/// rule is about.
///
/// `gethostname` is the portable answer and is what `hostname(1)` itself
/// calls, so the value matches what an operator sees in their shell.
///
/// The [`crate::config::UNNAMEABLE_HOST`] sentinel survives for the genuinely
/// unnameable host, because a wrong name is worse than an admittedly absent one
/// — but it is now the rare case it was always meant to be.
#[must_use]
pub fn hostname() -> String {
    nix::unistd::gethostname()
        .ok()
        .and_then(|name| name.into_string().ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| crate::config::UNNAMEABLE_HOST.to_string())
}

/// What a `kill(pid, 0)` probe PROVES about a pid.
///
/// The probe has three genuinely different outcomes and only two of them are
/// answers. This judge used to spell the third one `Err(_) => true`: a failure
/// with no way left to say it came from a failure. The value it produced was
/// even the right one, which is what made it invisible — a frozen gauge still
/// reads a plausible number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillProbe {
    /// The process EXISTS. Either the null signal was accepted, or `EPERM`
    /// said it exists and merely belongs to another user.
    Exists,
    /// `ESRCH`. The only errno that proves a process is gone.
    Gone,
    /// An errno that proves neither presence nor absence, carried rather than
    /// discarded. `kill(2)` documents exactly `EINVAL`, `EPERM` and `ESRCH` on
    /// both of this repo's targets, and `EINVAL` cannot arise for the null
    /// signal — so this arm is unreachable in practice, which is precisely why
    /// it must name the errno instead of being folded into a `_`. If it ever
    /// fires, the errno is the whole diagnosis.
    Unproven(nix::errno::Errno),
}

/// Classify one `kill(pid, 0)` outcome.
///
/// Pure with respect to the process table, so every arm — including the one
/// the real kernel will not produce — is unit-testable without a fixture.
#[must_use]
pub fn classify_kill(outcome: Result<(), nix::errno::Errno>) -> KillProbe {
    match outcome {
        // `EPERM` proves the process exists. Reading it as death is the
        // polarity mistake that makes another user's live chiefd look
        // deregisterable.
        Ok(()) | Err(nix::errno::Errno::EPERM) => KillProbe::Exists,
        Err(nix::errno::Errno::ESRCH) => KillProbe::Gone,
        Err(errno) => KillProbe::Unproven(errno),
    }
}

/// Whether `pid` is a live process, judged by `kill(pid, 0)`.
///
/// This is the ONE `kill(pid, 0)` judge in the workspace. It was written three
/// times — here, in the operator client's `discovery`, and in this crate's own
/// `watchdog` — and the third copy read `EPERM` as DEATH, the exact polarity
/// the other two warn about in their doc comments. Callers on both sides of
/// the P6 crate split call this function.
///
/// A non-positive pid is never live: `kill(0, …)` addresses the caller's own
/// process group and `kill(-n, …)` another group entirely, so neither is a
/// liveness question about a pid.
///
/// An [`KillProbe::Unproven`] outcome is reported as live and LOGGED with its
/// errno. Live is the fail-closed direction — a caller that believes a pid is
/// dead may deregister or replace it — but the log is the part that matters:
/// it is the difference between a judgement and a guess wearing a judgement's
/// return type.
#[must_use]
pub fn pid_is_live(pid: i64) -> bool {
    if pid < 1 {
        return false;
    }
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    match classify_kill(nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None)) {
        KillProbe::Exists => true,
        KillProbe::Gone => false,
        KillProbe::Unproven(errno) => {
            tracing::warn!(
                pid,
                ?errno,
                "kill(pid, 0) answered with an errno that proves neither presence nor absence; \
                 reading the pid as live so that no caller deregisters or replaces a process that \
                 may still be running"
            );
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive() {
        let me = std::process::id() as i64;
        assert!(pid_is_live(me));
    }

    #[test]
    fn pid_zero_and_negative_are_never_alive() {
        assert!(!pid_is_live(0));
        assert!(!pid_is_live(-1));
    }

    #[test]
    fn a_kill_probe_names_what_each_outcome_proves() {
        use nix::errno::Errno;

        // The two real answers.
        assert_eq!(classify_kill(Ok(())), KillProbe::Exists);
        assert_eq!(classify_kill(Err(Errno::ESRCH)), KillProbe::Gone);
        // `EPERM` is EXISTENCE, not death. The polarity this whole judge is
        // about: the process is there, it is simply not ours to signal.
        assert_eq!(classify_kill(Err(Errno::EPERM)), KillProbe::Exists);
        // Anything else is not an answer, and keeps its errno so a caller (or
        // a log line) can say which failure it was.
        assert_eq!(classify_kill(Err(Errno::EINVAL)), KillProbe::Unproven(Errno::EINVAL));
        assert_eq!(classify_kill(Err(Errno::EFAULT)), KillProbe::Unproven(Errno::EFAULT));
    }

    #[test]
    fn an_unproven_outcome_carries_its_errno_rather_than_collapsing() {
        // Pins the property the old `Err(_) => true` could not express at all:
        // two different unknown errnos stay distinguishable. Under the old
        // arm both were the single value `true`.
        let a = classify_kill(Err(nix::errno::Errno::EINVAL));
        let b = classify_kill(Err(nix::errno::Errno::EFAULT));
        assert_ne!(a, b);
        assert!(matches!(a, KillProbe::Unproven(_)));
    }

    #[test]
    fn an_exited_child_is_not_alive() {
        let mut child = std::process::Command::new("true").spawn().expect("spawn `true`");
        let pid = i64::from(child.id());
        // `wait` calls `waitpid` under the hood, which reaps the zombie
        // synchronously and needs no retry loop of any kind.
        child.wait().expect("wait for child");
        assert!(!pid_is_live(pid));
    }
}
