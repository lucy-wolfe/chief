//! The actuator's pane process, which does not stay dead.
//!
//! # The operator's requirement
//!
//! *"If chiefd is up, the actuator should never die. It's a critical path — if
//! it dies, we never know."*
//!
//! "Never dies" is not achievable: a process can be SIGKILLed, OOM-killed,
//! panicked, or refused by a bug not yet written, and enumerating causes is not
//! a design. **"Never STAYS dead"** is achievable, and it is what this module
//! implements.
//!
//! # The incident (2026-08-23)
//!
//! chiefd stayed up and healthy for two hours while `chief actuate` lay dead in
//! its pane — it had exited on a `403` that was really a seven-second identity
//! store stall (fixed server-side in #1204). The pane held a corpse because
//! `remain-on-exit` is ours; tmux noticed and nothing listened. Between two
//! `chief` invocations NOTHING watches the actuator: the complete list of
//! restart mechanisms was a human running `chief` again, and a deploy tool that
//! refuses a dead pane.
//!
//! # The shape
//!
//! `chief actuate` — the exact argv `attach` already spawns — becomes a tiny
//! supervisor. It re-spawns the same binary as a CHILD with
//! [`ATTEMPT_ENV`] set, inherits stdio so the pane shows exactly what it shows
//! today, waits, and on any exit it did not ask for prints one line and starts
//! another child on the operator's own crash-loop curve.
//!
//! ## Why a child process and not an in-process loop
//!
//! A loop inside `run_actuate` would catch the one exit we have measured — a
//! terminal refusal — and none of the ones we have not: a panic, an abort, an
//! OOM kill, a stack overflow. It would also have to tear down a tmux control
//! client, a brain, a `CrashLoop` and an `EverObserved` by hand to get a clean
//! slate. A process boundary does all of that for free.
//!
//! ## Why the supervisor is INERT
//!
//! It opens no tmux client, no HTTP client, no socket and no file. It spawns,
//! waits, sleeps, forwards signals and prints. That is what makes it the
//! process that does not itself die in a loop, and what makes it safe for a
//! version-N supervisor to run a version-N+1 child after an in-place install —
//! they share argv and the environment, and nothing else.
//!
//! ## NO OVERLAP, BY CONSTRUCTION — the property to protect
//!
//! There is no server-side single-actuator guard: the actuation lease was
//! deleted, and `resident.rs` says outright that there is no lease. Worse, the
//! session brain UNCONDITIONALLY unlinks the rail socket before binding, so a
//! second actuator steals every rail from the first, and two `CrashLoop`s hold
//! two disagreeing backoff states over the same panes.
//!
//! So this loop spawns the next child **only after [`std::process::Child::wait`]
//! has returned**, which means the predecessor has been reaped. There is no
//! timer, no speculative spawn, and no liveness guess anywhere in this file. A
//! future edit that adds one — a health probe that restarts a child that "looks
//! dead", a spawn on a tick — reintroduces the one failure this product cannot
//! survive. That is also why no tmux `pane-died` hook is used: it would race
//! `attach`'s `ReplaceExited`, which kill-sessions a dead actuator and recreates
//! it.
//!
//! ## Why every exit restarts, including a clean one
//!
//! The child's transient/terminal split is untouched and still correct FOR A
//! ROUND. It is simply not a verdict on the PROCESS. A genuinely permanent
//! refusal — a revoked identity, a company this daemon does not serve —
//! therefore becomes one pane line and one HTTP round trip every ten seconds,
//! for ever, beside chiefd's own `unattended` word. That is precisely the
//! operator's own 2026-08-19 ruling for people ("if it's crash looping, just do
//! a backoff … always keep retrying"), applied to the actuator, and it puts the
//! sentence where a human reads it and acts.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::crash_loop;

/// The marker that tells a `chief actuate` process it is the CHILD, carrying
/// the restart number so its banner can say so.
///
/// An environment variable rather than a verb: a child verb would be typeable
/// by an operator and would appear in `chief help`. This is invisible, and it
/// does not leak into Pi panes — they receive only the explicit `-e` list in
/// `tmux::PANE_ENVIRONMENT`.
pub const ATTEMPT_ENV: &str = "CHIEF_ACTUATOR_ATTEMPT";

/// How long a child must live before its death is treated as a fresh fault
/// rather than the next step of a crash loop.
///
/// A death after a day restarts in half a second; a child that dies at once
/// climbs to the ceiling in about twenty-five seconds and stays there.
pub const STABLE_RUN: Duration = Duration::from_secs(60);

/// How a child stopped, as far as the supervisor is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It returned this status. `0` is included and is not special: see the
    /// module docs on why a clean exit still restarts.
    Code(i32),
    /// The kernel killed it with this signal — SIGKILL, SIGABRT, SIGSEGV.
    Killed(i32),
    /// Neither a code nor a signal, which POSIX does not produce but the type
    /// permits. Treated exactly like any other death.
    Unknown,
}

/// What happened, handed to [`Policy::after`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The child stopped on its own.
    Exited {
        /// How it stopped.
        outcome: Outcome,
        /// How long it had been running.
        uptime: Duration,
    },
    /// A signal arrived AT THE SUPERVISOR — the only way supervision ends.
    Signal(i32),
}

/// What the supervisor does next. There is deliberately no variant meaning
/// "give up": the whole point of the module is that there is no such state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Start another child after this delay.
    Restart {
        /// How long to wait first.
        after: Duration,
        /// Which consecutive failure this is, for the screen line.
        failures: u32,
    },
    /// Stop supervising. Reached only from [`Event::Signal`].
    Stop,
}

/// The consecutive-failure count, and nothing else.
#[derive(Debug, Default, Clone, Copy)]
pub struct Policy {
    failures: u32,
}

impl Policy {
    /// A supervisor that has not yet lost a child.
    #[must_use]
    pub const fn new() -> Self {
        Self { failures: 0 }
    }

    /// The decision after one event.
    ///
    /// A child that lived at least [`STABLE_RUN`] resets the count to ONE, not
    /// to zero: this death still counts, but the history before it does not.
    pub fn after(&mut self, event: Event) -> Decision {
        match event {
            // A signal to the supervisor is the ONE way the loop ends. A
            // child's death — by any signal, including SIGKILL — is not.
            Event::Signal(_) => Decision::Stop,
            Event::Exited { uptime, .. } => {
                self.failures =
                    if uptime >= STABLE_RUN { 1 } else { self.failures.saturating_add(1) };
                Decision::Restart {
                    after: crash_loop::retry_delay(self.failures),
                    failures: self.failures,
                }
            }
        }
    }

    /// The consecutive-failure count, for the child's attempt number.
    #[must_use]
    pub const fn failures(&self) -> u32 {
        self.failures
    }
}

/// The signal's name where an operator would recognise it, its number where
/// they would not.
#[must_use]
pub fn signal_name(signal: i32) -> String {
    match signal {
        1 => "SIGHUP".to_owned(),
        2 => "SIGINT".to_owned(),
        6 => "SIGABRT".to_owned(),
        9 => "SIGKILL".to_owned(),
        11 => "SIGSEGV".to_owned(),
        13 => "SIGPIPE".to_owned(),
        15 => "SIGTERM".to_owned(),
        other => format!("signal {other}"),
    }
}

/// How a death reads on the pane.
#[must_use]
pub fn describe(outcome: Outcome) -> String {
    match outcome {
        Outcome::Code(code) => format!("status {code}"),
        Outcome::Killed(signal) => format!("killed by {}", signal_name(signal)),
        Outcome::Unknown => "an unknown outcome".to_owned(),
    }
}

/// The one line the pane gets per restart.
///
/// It names every fact a human needs to decide whether to look further: what
/// died, how, how long it had been up, which restart this is, and how long
/// until the next one. Read straight off the glass the operator is already
/// looking at, which is the whole point — the previous behaviour was silence.
#[must_use]
pub fn restart_line(
    company: &str,
    outcome: Outcome,
    uptime: Duration,
    failures: u32,
    after: Duration,
) -> String {
    format!(
        "chief: the actuator for {company} exited ({}) after {} — restart #{failures} in {}",
        describe(outcome),
        crash_loop::human_duration(uptime),
        crash_loop::human_duration(after),
    )
}

/// The waiting seam.
///
/// Production waits the real delay for ever. Tests scale it down and bound the
/// number of restarts, so the loop is finite and fast without pretending the
/// curve is different — `scale` divides the delay, it does not replace it.
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    /// Divisor applied to every delay. `1` in production.
    pub scale: u32,
    /// Stop after this many restarts. `None` in production, which is the
    /// behaviour the operator asked for: there is no attempt cap.
    pub stop_after: Option<u32>,
}

impl Schedule {
    /// The real curve, for ever.
    #[must_use]
    pub const fn production() -> Self {
        Self { scale: 1, stop_after: None }
    }

    /// The delay this schedule actually waits for a policy delay.
    #[must_use]
    pub fn scaled(self, delay: Duration) -> Duration {
        delay / self.scale.max(1)
    }
}

impl Default for Schedule {
    fn default() -> Self {
        Self::production()
    }
}

/// Wait for the next of these signals, or for ever if the stream could not be
/// registered.
///
/// `Option` rather than a hard failure: a supervisor that cannot register a
/// handler must still supervise. Losing the handler costs the forwarding, and
/// the default disposition then ends this process — which is exactly the
/// behaviour that exists today.
async fn next_signal(stream: &mut Option<tokio::signal::unix::Signal>) {
    match stream {
        Some(signals) => {
            signals.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Register one signal stream, or report why not.
fn watch(kind: tokio::signal::unix::SignalKind) -> Option<tokio::signal::unix::Signal> {
    match tokio::signal::unix::signal(kind) {
        Ok(stream) => Some(stream),
        Err(error) => {
            eprintln!("chief: could not watch for a signal ({error}); supervision will end on it");
            None
        }
    }
}

/// Pass the signal that ended supervision on to the child.
///
/// `nix::sys::signal::kill` is a SAFE function, which matters here: this crate
/// is `#![forbid(unsafe_code)]`, so installing a raw `sigaction` handler is not
/// available and `tokio::signal` — which does that work behind a safe API — is
/// what watches. Nothing in this file is `unsafe`.
fn forward(pid: i32, signal: nix::sys::signal::Signal, company: &str) {
    if pid <= 0 {
        return;
    }
    if let Err(error) = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), signal) {
        // ESRCH is the ordinary case: the child died between the signal
        // arriving and this call, and the wait below reaps it either way.
        if error != nix::errno::Errno::ESRCH {
            eprintln!("chief: could not pass {signal:?} to the actuator for {company}: {error}");
        }
    }
}

/// Wait, at the one site that is allowed to.
async fn wait_out(delay: Duration) {
    // os-liveness: the supervisor's whole job is to wait for a process and a
    // clock it does not own. There is nothing to wake on, no injected Clock
    // reaches this process — it is deliberately inert — and every wait is
    // bounded by `crash_loop::retry_delay`'s ladder. Narrow and at the call
    // site so the exemption stays greppable.
    #[allow(clippy::disallowed_methods)]
    tokio::time::sleep(delay).await;
}

/// How a backoff ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backoff {
    /// The delay ran out. Start the next child.
    Elapsed,
    /// A signal reached the supervisor first. Supervision is over.
    Signalled(nix::sys::signal::Signal),
}

/// Wait out the backoff, or stop early because a signal arrived.
///
/// # Why the sleep is INSIDE the select
///
/// Registering the signal streams replaces each signal's default disposition
/// for the whole process, so a SIGHUP or SIGTERM arriving while the supervisor
/// slept was not fatal — it was QUEUED. The supervisor finished the sleep (up
/// to the ten-second ceiling), spawned one child, and only then noticed. The
/// consequences were benign — the doomed child was signalled within
/// microseconds, long before it could bind a socket or converge, so there was
/// no overlap and no re-mint window — but a supervisor that ignores `chief
/// stop` for ten seconds reads as a bug to whoever meets it, and it is one.
///
/// There is NO CHILD during a backoff, so there is nothing to forward to and
/// nothing to reap: the supervisor simply leaves.
async fn backoff(
    delay: Duration,
    hangup: &mut Option<tokio::signal::unix::Signal>,
    terminate: &mut Option<tokio::signal::unix::Signal>,
    interrupt: &mut Option<tokio::signal::unix::Signal>,
) -> Backoff {
    tokio::select! {
        () = wait_out(delay) => Backoff::Elapsed,
        () = next_signal(hangup) => Backoff::Signalled(nix::sys::signal::Signal::SIGHUP),
        () = next_signal(terminate) => Backoff::Signalled(nix::sys::signal::Signal::SIGTERM),
        () = next_signal(interrupt) => Backoff::Signalled(nix::sys::signal::Signal::SIGINT),
    }
}

/// The exact command a child is spawned with.
///
/// Split out so a test can read the program, the argument and the marker
/// without spawning anything.
///
/// `program` is `argv[0]` — the path `attach` invoked, already absolute — and
/// deliberately NOT `current_exe()`, which on Linux resolves to
/// `<path> (deleted)` after an in-place binary replacement and would then fail
/// every spawn until somebody respawned the pane. A crash after an install
/// therefore comes back on the NEW binary, which is the deploy tool's intent.
#[must_use]
pub fn child_command(program: &Path, attempt: u32) -> Command {
    let mut command = Command::new(program);
    command.arg("actuate").env(ATTEMPT_ENV, attempt.to_string());
    command
}

/// Supervise `chief actuate` for ever.
///
/// Returns the exit code the supervisor should itself exit with, which is
/// reached ONLY when a signal arrives — the child's own code, or `128 + signal`
/// where the child was killed.
///
/// The loop is: spawn, wait, decide, wait out the delay, spawn. **The next
/// spawn is unreachable until the wait has returned**, which is the no-overlap
/// property the module docs describe. The child is waited for on a blocking
/// thread rather than with `tokio::process`, which this workspace does not
/// enable; the `select!` below is therefore between that thread finishing and a
/// signal arriving, and both paths end by awaiting the same waiter, so the
/// child is reaped exactly once on every route out.
pub async fn run(program: &Path, company: &str, schedule: Schedule) -> u8 {
    use tokio::signal::unix::SignalKind;

    let mut hangup = watch(SignalKind::hangup());
    let mut terminate = watch(SignalKind::terminate());
    let mut interrupt = watch(SignalKind::interrupt());

    let mut policy = Policy::new();
    let mut restarts = 0_u32;

    loop {
        let attempt = policy.failures().saturating_add(1);
        let started = Instant::now();
        let child = match child_command(program, attempt).spawn() {
            Ok(child) => child,
            Err(error) => {
                // A binary that is momentarily absent — mid-install — is a
                // failed attempt like any other, never a reason to stop.
                eprintln!("chief: could not start the actuator for {company}: {error}");
                let Decision::Restart { after, .. } = policy
                    .after(Event::Exited { outcome: Outcome::Unknown, uptime: started.elapsed() })
                else {
                    return 0;
                };
                if let Backoff::Signalled(signal) =
                    backoff(schedule.scaled(after), &mut hangup, &mut terminate, &mut interrupt)
                        .await
                {
                    return saturating_signal_code(signal as i32);
                }
                restarts = restarts.saturating_add(1);
                if schedule.stop_after.is_some_and(|limit| restarts >= limit) {
                    return 0;
                }
                continue;
            }
        };

        let pid = i32::try_from(child.id()).unwrap_or(0);
        let mut waiter = tokio::task::spawn_blocking(move || {
            let mut child = child;
            child.wait()
        });

        let signalled = tokio::select! {
            reaped = &mut waiter => {
                // The child stopped on its own. Nothing to forward.
                let outcome = reaped_outcome(reaped, company);
                let uptime = started.elapsed();
                match policy.after(Event::Exited { outcome, uptime }) {
                    Decision::Stop => return 0,
                    Decision::Restart { after, failures } => {
                        let delay = schedule.scaled(after);
                        println!("{}", restart_line(company, outcome, uptime, failures, delay));
                        if let Backoff::Signalled(signal) =
                            backoff(delay, &mut hangup, &mut terminate, &mut interrupt).await
                        {
                            return saturating_signal_code(signal as i32);
                        }
                        restarts = restarts.saturating_add(1);
                        if schedule.stop_after.is_some_and(|limit| restarts >= limit) {
                            return 0;
                        }
                        continue;
                    }
                }
            }
            () = next_signal(&mut hangup) => nix::sys::signal::Signal::SIGHUP,
            () = next_signal(&mut terminate) => nix::sys::signal::Signal::SIGTERM,
            () = next_signal(&mut interrupt) => nix::sys::signal::Signal::SIGINT,
        };

        // A signal reached the SUPERVISOR: supervision ends. Pass it on, reap
        // the child, and leave with its status.
        forward(pid, signalled, company);
        let reaped = waiter.await;
        return match reaped_outcome(reaped, company) {
            Outcome::Code(code) => u8::try_from(code).unwrap_or(1),
            Outcome::Killed(signal) => saturating_signal_code(signal),
            Outcome::Unknown => saturating_signal_code(signalled as i32),
        };
    }
}

/// What a joined waiter says about the child.
fn reaped_outcome(
    reaped: Result<std::io::Result<std::process::ExitStatus>, tokio::task::JoinError>,
    company: &str,
) -> Outcome {
    match reaped {
        Ok(Ok(status)) => outcome_of(status),
        Ok(Err(error)) => {
            eprintln!("chief: could not read how the actuator for {company} stopped: {error}");
            Outcome::Unknown
        }
        Err(error) => {
            eprintln!("chief: lost track of the actuator for {company}: {error}");
            Outcome::Unknown
        }
    }
}

/// How a finished child stopped.
fn outcome_of(status: std::process::ExitStatus) -> Outcome {
    use std::os::unix::process::ExitStatusExt as _;
    if let Some(code) = status.code() {
        return Outcome::Code(code);
    }
    if let Some(signal) = status.signal() {
        return Outcome::Killed(signal);
    }
    Outcome::Unknown
}

/// The shell convention: `128 + signal`.
fn saturating_signal_code(signal: i32) -> u8 {
    u8::try_from(128_i32.saturating_add(signal)).unwrap_or(128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curve_is_the_operators_crash_loop_curve() {
        // Not re-derived here: the same function the person crash loop uses,
        // so the actuator and a person back off identically by construction.
        let mut policy = Policy::new();
        let mut seen = Vec::new();
        for _ in 0..8 {
            match policy.after(Event::Exited { outcome: Outcome::Code(1), uptime: Duration::ZERO })
            {
                Decision::Restart { after, .. } => seen.push(after),
                Decision::Stop => panic!("a child's death must never stop supervision"),
            }
        }
        assert_eq!(
            seen,
            vec![
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(10),
                Duration::from_secs(10),
                Duration::from_secs(10),
            ]
        );
        for (failures, delay) in seen.iter().enumerate() {
            let expected = crash_loop::retry_delay(u32::try_from(failures).unwrap_or(0) + 1);
            assert_eq!(*delay, expected, "the curve must BE crash_loop's, not a copy of it");
        }
    }

    #[test]
    fn a_child_that_lived_long_enough_resets_the_count() {
        let mut policy = Policy::new();
        for _ in 0..5 {
            let _ =
                policy.after(Event::Exited { outcome: Outcome::Code(1), uptime: Duration::ZERO });
        }
        assert_eq!(policy.failures(), 5, "five quick deaths in a row is five failures");

        // A child that ran a whole minute resets to ONE, not zero: this death
        // still counts, the history before it does not.
        let decision =
            policy.after(Event::Exited { outcome: Outcome::Code(1), uptime: STABLE_RUN });
        assert_eq!(
            decision,
            Decision::Restart { after: Duration::from_millis(500), failures: 1 },
            "a death after a stable run restarts at the bottom of the curve"
        );

        // And one millisecond short of stable does NOT reset.
        let mut climbing = Policy::new();
        let _ = climbing.after(Event::Exited { outcome: Outcome::Code(1), uptime: Duration::ZERO });
        let decision = climbing.after(Event::Exited {
            outcome: Outcome::Code(1),
            uptime: STABLE_RUN - Duration::from_millis(1),
        });
        assert_eq!(decision, Decision::Restart { after: Duration::from_secs(1), failures: 2 });
    }

    #[test]
    fn every_exit_restarts_including_success() {
        // The child's transient/terminal split is a verdict about a ROUND and
        // never about the process. A clean exit is still a dead actuator.
        for outcome in [
            Outcome::Code(0),
            Outcome::Code(1),
            Outcome::Code(101),
            Outcome::Killed(9),
            Outcome::Killed(6),
            Outcome::Unknown,
        ] {
            let mut policy = Policy::new();
            let decision = policy.after(Event::Exited { outcome, uptime: Duration::ZERO });
            assert!(
                matches!(decision, Decision::Restart { .. }),
                "{outcome:?} must restart; there is no give-up state"
            );
        }
    }

    #[test]
    fn a_signal_to_the_supervisor_ends_it_and_a_childs_death_never_does() {
        let mut policy = Policy::new();
        for signal in [1, 2, 15] {
            assert_eq!(policy.after(Event::Signal(signal)), Decision::Stop);
        }
        // The distinction that matters: the child being KILLED by a signal is
        // not the supervisor RECEIVING one.
        assert!(matches!(
            policy.after(Event::Exited { outcome: Outcome::Killed(9), uptime: Duration::ZERO }),
            Decision::Restart { .. }
        ));
    }

    #[test]
    fn the_screen_line_names_the_facts() {
        let line = restart_line(
            "northstar",
            Outcome::Code(1),
            Duration::from_secs(3),
            4,
            Duration::from_secs(4),
        );
        assert!(line.contains("northstar"), "{line}");
        assert!(line.contains("status 1"), "{line}");
        assert!(line.contains("restart #4"), "{line}");
        assert!(line.contains("3s"), "the uptime is on screen: {line}");
        assert!(line.contains("4s"), "and so is the delay: {line}");

        let killed =
            restart_line("acme", Outcome::Killed(9), Duration::ZERO, 1, Duration::from_millis(500));
        assert!(killed.contains("killed by SIGKILL"), "{killed}");
        assert!(killed.contains("500ms"), "{killed}");
    }

    #[test]
    fn the_child_is_spawned_from_argv0_not_current_exe() {
        // A stand-in path: the point is that the supervisor re-runs the path it
        // was invoked with, so an in-place binary replacement is picked up and
        // a `(deleted)` `current_exe` never is.
        let program = Path::new("/opt/somewhere/chief");
        let command = child_command(program, 3);
        assert_eq!(command.get_program(), program.as_os_str());
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, vec!["actuate"], "the child's argv is exactly what attach spawns today");
        let marker = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new(ATTEMPT_ENV))
            .and_then(|(_, value)| value)
            .map(std::ffi::OsStr::to_os_string);
        assert_eq!(marker, Some(std::ffi::OsString::from("3")), "the attempt travels to the child");
    }

    #[test]
    fn the_schedule_scales_the_curve_without_replacing_it() {
        let fast = Schedule { scale: 100, stop_after: Some(3) };
        assert_eq!(fast.scaled(Duration::from_secs(10)), Duration::from_millis(100));
        assert_eq!(Schedule::production().scaled(Duration::from_secs(10)), Duration::from_secs(10));
        assert_eq!(Schedule::production().stop_after, None, "production has no attempt cap");
    }

    /// Serialises the tests that drive the REAL loop.
    ///
    /// Two process-global facts make this necessary, and neither is avoidable:
    /// a signal reaches the whole process, and a `tokio::signal` stream
    /// registered anywhere in it stays registered. So the test that raises
    /// SIGTERM at this process would otherwise be delivered into a CONCURRENT
    /// supervisor's `select!` and end it early, failing a test about something
    /// else entirely.
    ///
    /// A `tokio` mutex rather than a `std` one because these tests hold it
    /// across awaits, which is exactly what `await_holding_lock` exists to
    /// forbid for the std type.
    fn supervisor_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        &LOCK
    }

    /// A stand-in child: one line appended per start, then the given ending.
    fn stand_in_child(label: &str, ending: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("chief-supervise-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture directory");
        let script = dir.join("child.sh");
        let log = dir.join("starts");
        // fixture-write: a stand-in executable for a test that spawns a real
        // process. Nothing here is a product filesystem effect, so the host
        // executor seam does not apply; narrow and at the call site.
        #[allow(clippy::disallowed_methods)]
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho started >> {}\n{ending}\n", log.display()),
        )
        .expect("write the stand-in child");
        let mut permissions = std::fs::metadata(&script).expect("stat").permissions();
        {
            use std::os::unix::fs::PermissionsExt as _;
            permissions.set_mode(0o755);
        }
        std::fs::set_permissions(&script, permissions).expect("chmod");
        (script, log)
    }

    fn starts_recorded(log: &std::path::Path) -> usize {
        std::fs::read_to_string(log).map(|text| text.lines().count()).unwrap_or(0)
    }

    /// THE named integration test. It spawns `/bin/sh` only — no tmux, no
    /// chiefd, no network — and drives the real loop.
    ///
    /// The property is asserted against the CLOCK rather than against
    /// timestamps written by the child, because `date +%N` is GNU-only and this
    /// repo must behave identically on macOS. Elapsed time is also the more
    /// direct statement of the rule: four restarts cannot have happened sooner
    /// than the sum of the first four delays.
    #[tokio::test]
    async fn the_supervisor_never_restarts_faster_than_the_curve() {
        let _guard = supervisor_lock().lock().await;
        let (script, log) = stand_in_child("curve", "exit 1");

        let schedule = Schedule { scale: 100, stop_after: Some(4) };
        let floor: Duration =
            (1..=4).map(|failures| schedule.scaled(crash_loop::retry_delay(failures))).sum();

        let began = Instant::now();
        let code = run(&script, "fixture", schedule).await;
        let took = began.elapsed();

        assert_eq!(code, 0, "a bounded run ends by its bound, not by a signal");
        assert!(
            took >= floor,
            "four restarts must take at least the first four delays ({floor:?}), took {took:?}"
        );
        assert!(
            starts_recorded(&log) >= 3,
            "the child must have been restarted repeatedly, not once"
        );
        let _ = std::fs::remove_dir_all(script.parent().unwrap_or(&script));
    }

    /// A child the kernel kills is a death like any other — and the single most
    /// important thing about it is that the next child starts only after the
    /// previous one has been reaped, which is what `run`'s structure gives.
    #[tokio::test]
    async fn a_child_killed_by_signal_is_restarted() {
        let _guard = supervisor_lock().lock().await;
        let (script, log) = stand_in_child("killed", "kill -9 $$");

        let code = run(&script, "fixture", Schedule { scale: 100, stop_after: Some(2) }).await;

        assert_eq!(code, 0);
        assert!(starts_recorded(&log) >= 2, "SIGKILL is not a verdict either");
        let _ = std::fs::remove_dir_all(script.parent().unwrap_or(&script));
    }

    /// The forwarding mechanism itself, with nothing live.
    ///
    /// This does NOT close acceptance criterion 2: the routing — a signal
    /// reaching the supervisor's `select!` and being handed to this function —
    /// still cannot be exercised from inside libtest, because the process that
    /// would receive the signal IS the test runner. What it does close is the
    /// cheaper half: the mechanism is no longer entirely unproven, so a failure
    /// of the whole path can now be localised to the routing rather than
    /// leaving both halves suspect.
    #[test]
    fn forward_delivers_the_signal_and_tolerates_a_child_that_already_died() {
        use std::os::unix::process::ExitStatusExt as _;

        let mut child =
            Command::new("sh").arg("-c").arg("sleep 30").spawn().expect("spawn a stand-in child");
        let pid = i32::try_from(child.id()).unwrap_or(0);
        assert!(pid > 0, "the fixture must have a real pid");

        forward(pid, nix::sys::signal::Signal::SIGTERM, "fixture");

        let status = child.wait().expect("wait");
        assert_eq!(
            status.signal(),
            Some(15),
            "the signal the supervisor received must reach the child itself"
        );

        // ESRCH is the ORDINARY case, not an error: the child can die between
        // the signal arriving and this call, and the wait reaps it either way.
        // It must neither panic nor complain.
        forward(pid, nix::sys::signal::Signal::SIGTERM, "fixture");
        // And a pid that was never published — the window between spawning and
        // recording — is a no-op rather than a signal to process group 0, which
        // would reach every process in this one's group.
        forward(0, nix::sys::signal::Signal::SIGTERM, "fixture");
        forward(-1, nix::sys::signal::Signal::SIGTERM, "fixture");
    }

    /// A stop signal arriving DURING a backoff ends supervision then, not up to
    /// ten seconds later.
    ///
    /// The sleep used to sit outside the `select!`, so the signal was queued:
    /// the supervisor slept out the delay, spawned one doomed child and only
    /// then noticed. Benign — that child was signalled within microseconds and
    /// never bound anything — but a supervisor that ignores `chief stop` for
    /// ten seconds is a bug to whoever meets it.
    ///
    /// Raising a real signal at this process is safe here and only here: a
    /// stream is registered BEFORE the raise, which replaces the default
    /// disposition process-wide, so the signal is delivered to tokio rather
    /// than killing the test runner. The lock keeps it away from the other
    /// tests that drive the loop.
    #[tokio::test]
    async fn a_signal_during_backoff_stops_it_then_and_not_after_the_delay() {
        let _guard = supervisor_lock().lock().await;
        // Registered first, and HELD for the whole test: the disposition must
        // already be replaced when the raise below lands.
        let _disposition =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("register SIGTERM before raising it");

        let (script, log) = stand_in_child("backoff-signal", "exit 1");
        let program = script.clone();
        // The real curve and a bound far past anything this test should reach:
        // if the signal did NOT stop it, the run would take minutes and the
        // elapsed assertion below would catch it.
        let schedule = Schedule { scale: 1, stop_after: Some(200) };
        let began = Instant::now();
        let supervising = tokio::spawn(async move { run(&program, "fixture", schedule).await });

        // Wait until the first child has actually run, so the supervisor is
        // certainly past registration and into its first backoff.
        for _ in 0..200 {
            if starts_recorded(&log) >= 1 {
                break;
            }
            wait_out(Duration::from_millis(10)).await;
        }
        assert!(starts_recorded(&log) >= 1, "the first child must have run");

        nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::Signal::SIGTERM)
            .expect("raise SIGTERM at this process");

        let code = supervising.await.expect("the supervisor task");
        let took = began.elapsed();

        assert_eq!(code, 143, "128 + SIGTERM, whether or not a child was up");
        assert!(
            took < Duration::from_secs(5),
            "it must stop on the signal, not run out its bound: {took:?}"
        );
        let _ = std::fs::remove_dir_all(script.parent().unwrap_or(&script));
    }

    /// A program that is not there is a failed attempt, never a reason to stop
    /// — the mid-install case.
    #[tokio::test]
    async fn a_binary_that_is_not_there_is_still_retried() {
        let _guard = supervisor_lock().lock().await;
        let missing = std::env::temp_dir().join("chief-supervise-no-such-binary");
        let _ = std::fs::remove_dir_all(&missing);

        let code = run(&missing, "fixture", Schedule { scale: 100, stop_after: Some(2) }).await;

        assert_eq!(code, 0, "it kept trying and ended only at the test's bound");
    }
}
