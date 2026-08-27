//! Process watchdogs shared by every `chiefd` boot path.
//!
//! These lived in the deleted `docstore_only` module; the mode is gone but the
//! watchdogs are not — `run.rs` installs both on the real daemon. The env names
//! keep their `CHIEFD_STORE_*` spelling because the orphanable-spawner scanner
//! matches those exact tokens.

/// Env flag that opts a spawned docstore into the child-side parent-death
/// self-kill. Named into the `CHIEFD_STORE_*` config family; the
/// orphanable-spawner scanner verifies this exact token in any detached spawn
/// site that claims the `chiefd-store-exit-with-parent` watchdog.
const EXIT_WITH_PARENT_ENV: &str = "CHIEFD_STORE_EXIT_WITH_PARENT";

/// Env var naming an explicit pid to watch for the U16 double-fork spawn path
/// (`tests/setup-durable-store.ts`). `EXIT_WITH_PARENT_ENV` snapshots the
/// process's OS parent AT SPAWN TIME via `PR_SET_PDEATHSIG`; a genuinely
/// double-forked docstore (spawned so it is NOT a direct child of the bun
/// test process, specifically to escape Bun's test-runner dangling-process
/// reaper — od:u16-shard2-cascade) has an intermediate shell as that
/// snapshotted parent, and that shell exits within milliseconds by design,
/// so `PR_SET_PDEATHSIG` would fire almost immediately and self-kill the
/// daemon before it ever became reachable. This env decouples "who do I
/// consider my true owner" from "who is my OS parent right now": the daemon
/// polls THIS NAMED pid's liveness (the original bun test process, however
/// many forks removed) and self-exits the instant it is gone, regardless of
/// its own reparent history. Purely additive; `EXIT_WITH_PARENT_ENV` and its
/// PDEATHSIG path are untouched for callers that spawn a direct child.
const WATCH_PID_ENV: &str = "CHIEFD_STORE_WATCH_PID";

/// Install the child-side parent-death watchdog when opted in via
/// [`EXIT_WITH_PARENT_ENV`].
///
/// On Linux, `PR_SET_PDEATHSIG` (od:idle-cpu #285) asks the kernel to deliver
/// `SIGTERM` the instant this process's parent dies — no thread, no polling;
/// `beacond::shutdown::wait_for_signal`'s SIGTERM handler treats it exactly like an
/// operator's `kill`, so shutdown is graceful. Immediately re-checks
/// `getppid()` after arming: `PR_SET_PDEATHSIG` only fires for a FUTURE
/// reparent, so if the parent had ALREADY died before this call (the process
/// is already reparented to pid 1), no future signal will ever come — the
/// standard "close the race" idiom for this syscall (prctl(2)). Losing that
/// race self-terminates immediately rather than running orphaned.
///
/// Non-Linux unix has no `PDEATHSIG` equivalent; it keeps the former 1Hz poll
/// thread as a portable fallback (parent death is not latency-critical here).
#[cfg(target_os = "linux")]
pub(crate) fn install_parent_death_watchdog() {
    if std::env::var(EXIT_WITH_PARENT_ENV).as_deref() != Ok("1") {
        return;
    }
    if let Err(error) = nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGTERM) {
        tracing::warn!(%error, "PR_SET_PDEATHSIG failed; the parent-death watchdog is inert");
        return;
    }
    if nix::unistd::getppid().as_raw() == 1 {
        // The reparent race: the parent was already gone by the time we
        // armed the signal, so no SIGTERM is coming. Self-terminate now.
        std::process::exit(0);
    }
}

/// Portable fallback for non-Linux unix (no `PR_SET_PDEATHSIG`): keep the
/// former poll thread, slowed — parent death is not latency-critical here.
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn install_parent_death_watchdog() {
    if std::env::var(EXIT_WITH_PARENT_ENV).as_deref() != Ok("1") {
        return;
    }
    std::thread::spawn(|| loop {
        if std::os::unix::process::parent_id() == 1 {
            std::process::exit(0);
        }
        // A raw OS watchdog thread, deliberately OUTSIDE the injected Clock: it
        // guards process liveness, not application timing, so the Clock seam
        // (which exists so tests never sleep) does not apply here. This is the
        // sole sanctioned `std::thread::sleep` in the crate — the clippy.toml
        // "narrow, commented allow at the exact call site" escape hatch.
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(std::time::Duration::from_secs(5));
    });
}

/// On non-unix there is no parent-death signal to arm or poll for; the
/// watchdog is a no-op. The test harness that relies on it only runs on unix.
#[cfg(not(unix))]
pub(crate) fn install_parent_death_watchdog() {}

/// Install the [`WATCH_PID_ENV`] named-pid watchdog when opted in.
///
/// Unlike [`install_parent_death_watchdog`], this never arms a kernel signal
/// against "my current parent" — it polls a NAMED pid's liveness on a plain
/// thread and self-exits the instant that pid is gone, however many forks
/// removed the daemon itself now is. This is what makes it safe for the
/// double-forked spawn path: the watched pid (the original bun test process)
/// need not be this process's OS parent at all.
#[cfg(unix)]
pub(crate) fn install_watch_pid_watchdog() {
    let Ok(raw) = std::env::var(WATCH_PID_ENV) else { return };
    let Ok(watched) = raw.trim().parse::<i32>() else {
        tracing::warn!(value = %raw, "CHIEFD_STORE_WATCH_PID is not a valid pid; the watch-pid watchdog is inert");
        return;
    };
    // Race-closed the same way install_parent_death_watchdog is: check once
    // before ever starting to poll, so a caller that names an already-dead
    // pid self-terminates immediately instead of running orphaned until the
    // first poll tick.
    // The ONE liveness judge, not this crate's own copy of it. `.is_err()`
    // read `EPERM` — a process that EXISTS and merely belongs to another user
    // — as owner-death, and the reaction to owner-death here is
    // `std::process::exit(0)`: the daemon killed itself over a probe that had
    // proved the owner alive.
    if !beacond::liveness::pid_is_live(i64::from(watched)) {
        std::process::exit(0);
    }
    std::thread::spawn(move || loop {
        // A raw OS watchdog thread, deliberately OUTSIDE the injected Clock —
        // see the identical justification on the portable PDEATHSIG fallback
        // above. `beacond::liveness::pid_is_live` is the standard POSIX
        // liveness probe, in the one place the workspace spells it.
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(std::time::Duration::from_millis(500));
        if !beacond::liveness::pid_is_live(i64::from(watched)) {
            std::process::exit(0);
        }
    });
}

/// On non-unix there is no pid-liveness probe to poll; the watchdog is a
/// no-op. The double-fork spawn path that relies on it only runs on unix.
#[cfg(not(unix))]
pub(crate) fn install_watch_pid_watchdog() {}
