//! The daemon claims its own file-descriptor budget at startup.
//!
//! # Why this exists
//!
//! chiefd reported `Too many open files (os error 24)` on macOS, from inside a
//! materialization step that had nothing to do with it. Materialization was
//! measured first and cleared: it holds at most TWO descriptors at a time and
//! leaks none — five whole-roster passes over a 27-person company (~1,350
//! `renameat` calls) left the lowest free descriptor exactly where it started.
//! The exhaustion was process-wide, and the process had never asked for a
//! budget at all.
//!
//! What chiefd actually needs is not small and not variable:
//!
//! * **~54 descriptors before it serves one request.** Two independent SQLite
//!   connection pools are opened on the SAME database file — `CompanyDb`'s
//!   writer plus eight eager readers, and the docstore `DocEngine`'s writer
//!   plus eight more — and both run in WAL mode, where every connection costs
//!   three descriptors (the database, its `-wal` and its `-shm`).
//! * **One live descriptor per SSE subscriber**, on a changefeed the client
//!   reconnects after every wake and that scales with open tmux WINDOWS rather
//!   than with people. A peer that vanishes without a clean FIN holds its
//!   descriptor until the 15-second heartbeat write fails.
//!
//! # Why it is a macOS bug and not a Linux one
//!
//! Nothing in that list is unreasonable, and on Linux nothing goes wrong: the
//! default soft `RLIMIT_NOFILE` is 1024. macOS ships **256**. The identical
//! daemon doing the identical work therefore fails on one platform and not the
//! other — which is the cross-platform rule in `AGENTS.md` broken by a limit
//! rather than by a type width. A daemon must not inherit whatever ceiling the
//! shell that spawned it happened to have.
//!
//! # Why a fixed target rather than the hard limit
//!
//! Raising the soft limit to the hard limit is the obvious move and it is wrong
//! on Darwin: the hard `RLIMIT_NOFILE` there is normally `RLIM_INFINITY`, and
//! Darwin's `setrlimit` REFUSES a soft `NOFILE` above `kern.maxfilesperproc`
//! with `EINVAL`. The fix would have silently done nothing on the one platform
//! that needed it. So the target is a stated number, clamped by whatever hard
//! limit the process really has, which needs no `sysctl` read and behaves
//! identically on both platforms.

use nix::sys::resource::{getrlimit, rlim_t, setrlimit, Resource};

/// The soft `RLIMIT_NOFILE` chiefd runs with.
///
/// Chosen against the two measured costs above, not picked round: ~150x the
/// constant SQLite baseline, and far above any SSE subscriber count a single
/// operator's tmux session can produce. It is also below 10240, the most
/// conservative `kern.maxfilesperproc` any supported macOS has shipped, so
/// Darwin accepts it without a `sysctl` probe.
pub const DESCRIPTOR_BUDGET: rlim_t = 8192;

/// The two bounds [`DESCRIPTOR_BUDGET`] is chosen between, checked when this
/// crate compiles rather than when a test runs: a number this load-bearing
/// should not be able to reach a build at all if somebody moves it out of
/// range.
const _: () = {
    // Two SQLite pools on one file, 18 connections, three descriptors each
    // under WAL. The budget must leave room for SSE subscribers on top of that
    // constant cost, not merely cover it.
    assert!(DESCRIPTOR_BUDGET >= 18 * 3 * 100);
    // Darwin refuses a soft `NOFILE` above `kern.maxfilesperproc`, whose most
    // conservative shipped value is 10240. A budget above it would make
    // `setrlimit` fail with EINVAL on the one platform this exists for.
    assert!(DESCRIPTOR_BUDGET < 10_240);
};

/// The soft limit [`claim`] will put in force, or `None` when the process
/// already has at least the budget.
///
/// The whole decision, kept pure so the platforms this exists for can be
/// asserted without a test mutating the descriptor limit of the process it runs
/// in. The hard limit is a CEILING and never a target: on Darwin it is normally
/// `RLIM_INFINITY`, which `setrlimit` refuses for `NOFILE`.
const fn target_soft_limit(soft: rlim_t, hard: rlim_t) -> Option<rlim_t> {
    let target = if DESCRIPTOR_BUDGET < hard { DESCRIPTOR_BUDGET } else { hard };
    if soft < target {
        Some(target)
    } else {
        None
    }
}

/// Raise the soft descriptor limit to [`DESCRIPTOR_BUDGET`], bounded by the
/// hard limit this process was given.
///
/// Returns the soft limit in force afterwards. A process that already has at
/// least the budget is left alone, and the hard limit is never touched:
/// lowering it is irreversible for the process's whole lifetime.
///
/// This never refuses to start the daemon. The limit is advisory, a daemon that
/// exited because it could not raise one would be strictly worse than one
/// running on the inherited ceiling, and the outcome is logged either way so a
/// later `EMFILE` can be read against the budget that was actually in force.
pub fn claim() -> rlim_t {
    let (soft, hard) = match getrlimit(Resource::RLIMIT_NOFILE) {
        Ok(limits) => limits,
        Err(error) => {
            tracing::warn!(
                event = "process.descriptor_budget.unreadable",
                error = %error,
                "cannot read the descriptor limit; running on the inherited ceiling"
            );
            return 0;
        }
    };
    let Some(target) = target_soft_limit(soft, hard) else {
        tracing::info!(
            event = "process.descriptor_budget",
            soft,
            hard,
            raised = false,
            "the inherited descriptor limit already covers the budget"
        );
        return soft;
    };
    match setrlimit(Resource::RLIMIT_NOFILE, target, hard) {
        Ok(()) => {
            tracing::info!(
                event = "process.descriptor_budget",
                inherited = soft,
                hard,
                soft = target,
                raised = true,
                "descriptor budget claimed"
            );
            target
        }
        Err(error) => {
            tracing::warn!(
                event = "process.descriptor_budget.refused",
                inherited = soft,
                hard,
                target,
                error = %error,
                "cannot raise the descriptor limit; running on the inherited ceiling"
            );
            soft
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS's default and Linux's default, side by side — the RULE this
    /// module exists for.
    ///
    /// chiefd hit `EMFILE` on macOS under a 256 soft limit it had inherited
    /// from the shell that spawned it and never questioned, while the identical
    /// daemon on Linux inherited 1024 and did not. Both must end at the same
    /// budget, or a descriptor bug stays a bug you can only reproduce on one
    /// person's laptop.
    #[test]
    fn the_same_budget_is_claimed_on_macos_and_on_linux() {
        // Darwin: soft 256, hard RLIM_INFINITY.
        assert_eq!(target_soft_limit(256, rlim_t::MAX), Some(DESCRIPTOR_BUDGET));
        // Linux: soft 1024, hard 524288.
        assert_eq!(target_soft_limit(1024, 524_288), Some(DESCRIPTOR_BUDGET));
    }

    #[test]
    fn the_hard_limit_is_a_ceiling_and_an_ample_process_is_left_alone() {
        assert_eq!(
            target_soft_limit(64, 512),
            Some(512),
            "a hard limit below the budget bounds it; asking above it would only fail"
        );
        assert_eq!(
            target_soft_limit(DESCRIPTOR_BUDGET, rlim_t::MAX),
            None,
            "a process that already has the budget is not touched"
        );
        assert_eq!(
            target_soft_limit(rlim_t::MAX, rlim_t::MAX),
            None,
            "and neither is one with more"
        );
    }

    /// The one assertion that runs the real syscalls. It only ever RAISES, so
    /// it cannot starve the rest of the suite of descriptors.
    #[test]
    fn claim_leaves_the_process_holding_at_least_the_budget() {
        let (_, hard) = getrlimit(Resource::RLIMIT_NOFILE).expect("read the limit");
        let claimed = claim();
        let (soft_after, hard_after) = getrlimit(Resource::RLIMIT_NOFILE).expect("read the limit");
        assert_eq!(soft_after, claimed, "claim reports the limit it actually put in force");
        assert_eq!(hard_after, hard, "the hard limit is never touched — lowering it is final");
        assert!(
            soft_after >= DESCRIPTOR_BUDGET.min(hard),
            "the daemon must not run on whatever ceiling its shell happened to have"
        );
    }
}
