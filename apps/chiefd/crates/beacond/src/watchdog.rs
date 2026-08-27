//! The child-side owner-death self-kill.
//!
//! # The exposure this closes (#987, #751)
//!
//! Every `beacond` in this repo that is not an operator's own long-lived
//! registry is spawned by a test fixture, detached and `unref`'d, and reaped
//! only by that fixture's `stop()`. A vitest worker that is SIGKILLed, crashes,
//! or is torn down by a timeout never reaches `stop()`, so the daemon survives
//! holding its port and its database handle. #987 measured the consequence
//! rather than predicting it: an orphaned `beacond` still running **eight to
//! twelve hours** later on a shared build host, found by reading `ps`.
//!
//! `chiefd`'s docstore-only mode has had the answer since #694 —
//! `CHIEFD_STORE_WATCH_PID` — and beacond had no equivalent, so the two
//! fixtures that spawn it were carried in the orphanable-spawner register as
//! named, unfixed exposures. This module is that equivalent.
//!
//! # Why a named pid rather than `PR_SET_PDEATHSIG`
//!
//! The same reason the docstore's version takes one: the kernel signal is armed
//! against *whoever my OS parent is at this instant*, and a fixture that
//! double-forks (or whose intermediate shell exits immediately, which is the
//! ordinary case for a detached spawn) has already lost that parent by the time
//! the daemon boots. Polling a NAMED pid decouples "who owns me" from "who
//! happens to be my parent right now": the watched process is the test runner,
//! however many forks removed, and the daemon exits the instant it is gone.
//!
//! It is also the portable choice, which matters here. `PR_SET_PDEATHSIG` is
//! Linux-only; `kill(pid, 0)` is POSIX and behaves identically on both of this
//! repo's build targets, so this module needs no per-platform conditional
//! compilation — the same argument [`crate::liveness`] already makes for
//! beacond's admission judge, and beacond already depends on `nix` for it.
//!
//! # Opt-in, and inert when unset
//!
//! An operator's own `beacond` has no owner to watch and must never self-exit,
//! so the watchdog arms only when [`WATCH_PID_ENV`] names a pid. An unset
//! variable is silence, not a default.

/// Env var naming the process whose death this `beacond` should not outlive.
///
/// Deliberately parallel to `chiefd`'s `CHIEFD_STORE_WATCH_PID`, in
/// beacond's own `BEACOND_*` config family: the two daemons watch different
/// owners and are configured by different callers, so one variable read by both
/// would be a single name for two facts. The orphanable-spawner scanner
/// verifies this exact token appears in any spawn site claiming the
/// `beacond-exit-with-owner` watchdog, so a fixture cannot claim the safety
/// without setting it.
pub const WATCH_PID_ENV: &str = "BEACOND_WATCH_PID";

/// How often the watched pid's liveness is probed.
///
/// Matches the docstore watchdog's 500 ms. The cost of a slower poll is orphan
/// lifetime, and the cost of a faster one is a syscall nobody needs; half a
/// second against an 8-hour observed orphan is not a tuning question.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// What [`install`] decided, so the decision is unit-testable without spawning
/// a process or exiting one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// [`WATCH_PID_ENV`] was unset: an operator's own registry, nothing to
    /// watch.
    Inert,
    /// [`WATCH_PID_ENV`] was set to something that is not a pid. Reported
    /// rather than obeyed — a caller that meant to arm the watchdog and
    /// mistyped the value must not get silence.
    Malformed,
    /// The named pid was already gone when the daemon started. This is a real
    /// race, not a hypothetical: the owner can die between spawning beacond and
    /// beacond reaching this line, and a daemon that started polling AFTER that
    /// would poll for a corpse for ever.
    AlreadyDead,
    /// Armed and polling.
    Watching(i32),
}

/// Decide what the watchdog should do, from an INJECTED environment lookup.
///
/// Pure with respect to the environment and the process table, so every arm is
/// testable under a parallel runner without setting a real variable — the same
/// shape [`crate::config::Config::from_env`] uses for the same reason.
pub fn decide(var: impl Fn(&str) -> Option<String>, is_live: impl Fn(i32) -> bool) -> Decision {
    let Some(raw) = var(WATCH_PID_ENV) else { return Decision::Inert };
    let Ok(pid) = raw.trim().parse::<i32>() else { return Decision::Malformed };
    // A non-positive pid is not a process. `kill(0, …)` addresses the caller's
    // whole process GROUP and `kill(-n, …)` another group entirely, so parsing
    // one of those into a liveness probe would arm the watchdog against
    // something that is not the owner at all — and `kill(0, None)` succeeds
    // trivially, which would look exactly like a healthy owner for ever.
    if pid <= 0 {
        return Decision::Malformed;
    }
    if is_live(pid) {
        Decision::Watching(pid)
    } else {
        Decision::AlreadyDead
    }
}

/// Install the watchdog, if [`WATCH_PID_ENV`] names a live pid.
///
/// Exits the process immediately when the named pid is already gone, and spawns
/// a plain OS thread that does the same the moment it dies.
pub fn install() {
    // The ONE liveness judge, not a third copy of it. This module carried its
    // own `kill(pid, None).is_ok()`, which reads `EPERM` — a process that
    // EXISTS and merely belongs to another user — as owner-death, and a
    // watchdog that believes the owner died exits the process. That is the
    // exact polarity `crate::liveness` and the operator client's `discovery`
    // both warn about in their doc comments, inverted in the one copy nobody
    // was reading.
    match decide(|key| std::env::var(key).ok(), |pid| crate::liveness::pid_is_live(i64::from(pid)))
    {
        Decision::Inert => {}
        Decision::Malformed => {
            let raw = std::env::var(WATCH_PID_ENV).unwrap_or_default();
            tracing::warn!(
                value = %raw,
                "BEACOND_WATCH_PID is not a valid pid; the owner-death watchdog is inert"
            );
        }
        Decision::AlreadyDead => {
            tracing::info!("the process named by BEACOND_WATCH_PID is already gone; exiting");
            std::process::exit(0);
        }
        Decision::Watching(pid) => {
            tracing::info!(watched_pid = pid, "beacond will exit when its owner does");
            std::thread::spawn(move || loop {
                // A raw OS watchdog thread, deliberately outside any injected
                // clock — the same exemption `docstore_only.rs` takes for the
                // same mechanism, and for the same reason: this thread must
                // keep working while the async runtime is wedged, and a wedged
                // runtime is one of the states that strands a daemon in the
                // first place. A clock this process could stall is no watchdog.
                #[allow(clippy::disallowed_methods)]
                std::thread::sleep(POLL_INTERVAL);
                if !crate::liveness::pid_is_live(i64::from(pid)) {
                    std::process::exit(0);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_owned())
    }

    #[test]
    fn an_unset_variable_leaves_the_watchdog_inert() {
        // An operator's own registry has no owner to outlive. Defaulting to a
        // watched pid here would make beacond exit on a process it was never
        // given.
        assert_eq!(decide(env(&[]), |_| true), Decision::Inert);
    }

    #[test]
    fn a_live_named_pid_arms_the_watch() {
        assert_eq!(
            decide(env(&[(WATCH_PID_ENV, "4242")]), |pid| pid == 4242),
            Decision::Watching(4242)
        );
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        // The value is written by a shell or a spawn env map, and a trailing
        // newline is the ordinary way one arrives.
        assert_eq!(decide(env(&[(WATCH_PID_ENV, " 4242\n")]), |_| true), Decision::Watching(4242));
    }

    #[test]
    fn a_dead_named_pid_is_reported_before_any_polling_starts() {
        // The startup race, closed. The owner can die between the spawn and
        // this check; a watchdog that only polled from here on would never
        // notice, because the death already happened.
        assert_eq!(decide(env(&[(WATCH_PID_ENV, "4242")]), |_| false), Decision::AlreadyDead);
    }

    #[test]
    fn a_non_numeric_value_is_malformed_rather_than_ignored() {
        assert_eq!(decide(env(&[(WATCH_PID_ENV, "not-a-pid")]), |_| true), Decision::Malformed);
    }

    #[test]
    fn zero_and_negative_pids_are_refused_because_they_address_process_groups() {
        // `kill(0, None)` addresses the caller's own process group and always
        // succeeds, so accepting it would arm a watchdog that can never fire —
        // a green instrument watching nothing, which is the exact shape this
        // module exists to remove.
        for value in ["0", "-1", "-4242"] {
            assert_eq!(
                decide(env(&[(WATCH_PID_ENV, value)]), |_| true),
                Decision::Malformed,
                "{value}"
            );
        }
    }

    #[test]
    fn the_liveness_probe_answers_for_a_real_process_and_an_impossible_one() {
        // Pins the shared judge against the real process table rather than a
        // stub, so the injected `is_live` in the tests above is known to model
        // something true. Our own pid is live by construction; the maximum pid
        // value cannot be allocated.
        assert!(crate::liveness::pid_is_live(i64::from(std::process::id() as i32)));
        assert!(!crate::liveness::pid_is_live(i64::from(i32::MAX)));
    }

    #[test]
    fn the_watchdog_reads_a_process_it_may_not_signal_as_alive_not_as_dead() {
        // The polarity the deleted local `.is_ok()` probe got backwards. An
        // owner that exists but is not ours to signal must arm the watchdog,
        // never trip the `AlreadyDead` exit — `install()` calls
        // `std::process::exit(0)` on that arm, so an inverted probe kills
        // beacond while its owner is still running.
        assert!(matches!(
            crate::liveness::classify_kill(Err(nix::errno::Errno::EPERM)),
            crate::liveness::KillProbe::Exists
        ));
        assert!(crate::liveness::pid_is_live(i64::from(std::process::id() as i32)));
    }
}
