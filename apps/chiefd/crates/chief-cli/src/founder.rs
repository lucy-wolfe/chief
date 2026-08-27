//! `chief` — the Founder.
//!
//! Ported from the deleted TypeScript `launcher.ts` (its bootstrap entry) and
//! `launcher-wiring.ts` (its Founder entry, `ensureRealLauncherSession` and
//! `spawnRealLauncherPiInSession`).
//!
//! # There is ONE pre-company identity
//!
//! Founder — and only Founder. There is no second pre-company role string, and
//! no mode parameter threaded through the spawn to select between roles. The
//! deleted TypeScript carried a rival pre-company identity end to end — its own
//! session name, its own skill, its own provider projection and the `mode`
//! argument that chose between it and Founder — and all of it is gone rather
//! than kept alongside. The guard at the bottom of this file fails if any of
//! those retired names reappears above it, so this paragraph describes the
//! shape without naming them.
//!
//! # The two branches, and why they exist
//!
//! - **Already inside tmux.** The operator's own terminal is already a pane, so
//!   the Founder Pi runs right here: tag the pane so chiefd's caller
//!   attestation can prove an operator asked, publish the genesis endpoint into
//!   its environment, run Pi in the foreground.
//! - **Not inside tmux.** Create (or reuse) the one Founder session, respawn
//!   its pane into THIS exact command, and attach the terminal to it. The
//!   recursive invocation observes a real `$TMUX` and takes the branch above,
//!   so the spawn argv exists in exactly one place.
//!
//! # The second branch was unreachable, and the operator paid for it
//!
//! It has been written here since the port, and `preflight` refused with
//! `OutsideTmux` two statements before [`run`] could reach it — so the only
//! thing an operator outside tmux ever saw was *"ChiefD only runs inside tmux.
//! Start one with 'tmux new -s companies'"*. Reported live on 2026-08-10:
//! *"Every time I run `chief` it asks me to be inside a tmux."* `chiefd`
//! is meant to be ONE front door, and a front door does not send the human away
//! to start a terminal multiplexer when the code to do it is already sitting
//! behind the refusal. `Surface::Founder` is now `TerminalNeed::BootsOwn`, so this
//! branch runs.
//!
//! The attach is in the FOREGROUND, deliberately: `chief` is an
//! interactive verb whose whole product is a conversation with Founder, so
//! creating the session detached and printing its name would make the
//! operator's next command `tmux attach` — the same hand-run tmux verb, moved
//! one step later.
//!
//! `respawn-pane` rather than running Pi directly is load-bearing: running the
//! same invocation here would inherit THIS command's stdio, not the session's
//! pane, and the operator would end up attached to a session whose pane is
//! running nothing.

use std::io::IsTerminal as _;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use super::company::CompanyClient;
use super::genesis;
use super::http::Client;
use super::{founder_pi, tmux, LifecycleError, Result};

/// The one Founder session name. Fixed and well known; create-or-reuse, never
/// duplicated.
pub(crate) const FOUNDER_SESSION: &str = "chiefd-founder";

/// The tmux pane option chiefd's caller attestation reads to prove a pane is
/// the operator's control surface rather than an agent's.
const OPERATOR_CONTROL_TMUX_TAG: &str = "@chiefd-operator-control";

/// The environment variable the genesis endpoint is published as.
const FOUNDER_URL_ENV: &str = "CHIEFD_FOUNDER_URL";

/// How often the Founder host asks whether its Pi child has exited.
const FOUNDER_PI_POLL: Duration = Duration::from_millis(200);

/// How long a retired Founder Pi is given to unwind on its `SIGTERM` before it
/// is killed. It has a terminal to put back the way it found it.
const FOUNDER_PI_GRACE: Duration = Duration::from_secs(3);

/// How often the Founder host says out loud that it is still waiting.
///
/// The wait it narrates is unbounded — the bound is the human in the pane — so
/// a log with nothing in it for a whole conversation is indistinguishable from
/// a crash. Fifteen seconds is far enough apart to be free next to a 200 ms
/// poll and close enough that no reader mistakes the quiet for a death.
const FOUNDER_HEARTBEAT: Duration = Duration::from_secs(15);

/// The pause between "the operator's client has moved to the CEO" and the
/// `SIGTERM` that retires the Founder.
///
/// The launch response Pi is still waiting on is written by ANOTHER task in
/// this same process, and retirement is decided inside the handler that
/// produced it. Retiring the instant the decision is made would race that
/// write, so the Founder's last tool result — the one sentence that says the
/// handover happened — could be lost between two tasks of one program. This is
/// a flush, not a liveness wait: there is nothing to poll for, because the fact
/// wanted ("Pi has rendered its result") lives inside another process's
/// terminal and is not observable from here at all.
const FOUNDER_RETIREMENT_FLUSH: Duration = Duration::from_millis(1_000);

/// What `chief` says out loud for one phase, or `None` for a phase this
/// surface has no business narrating.
///
/// The WARM beacond path is deliberately silent. It is a single probe that is
/// over before a human could read a line about it, and a launch that announces
/// every step it did not have to take is how the steps that matter get lost.
/// Only the cold path — beacond was not there and had to be started — speaks.
fn new_operator_line(phase: &str) -> Option<&'static str> {
    match phase {
        "preflight" => Some("checking this host is ready"),
        "beacond-starting" => Some("the company directory is not running — starting it"),
        "beacond-ready" => Some("the company directory is up"),
        _ => None,
    }
}

/// What the operator is told at the instant the Founder Pi is started.
///
/// # The 84 seconds of nothing
///
/// `chief` printed its last line at `beacond.ensure_running` and its next
/// at `founder.launch` — 84 seconds later, with the Founder Pi booting in
/// between and the terminal saying nothing at all. Fifteen of those seconds
/// were the `pi.dev` builtin-catalog refresh timing out, which
/// [`super::founder_pi::apply_founder_environment`] has now removed; the rest
/// is a real Pi boot, and a real boot is still worth one sentence.
///
/// It deliberately promises NO duration. The version of this line that named
/// fifteen seconds would have been a lie the moment the same change landed,
/// and a narration that has to be re-timed whenever the thing it narrates gets
/// faster is a narration that will one day be wrong quietly.
#[must_use]
const fn founder_starting_line() -> &'static str {
    "chief: starting the Founder — it will ask you for the company name and purpose"
}

/// Run `chief`.
///
/// # Errors
/// [`LifecycleError`] naming the refusal and the operator's next move.
pub(crate) async fn run(dir: &std::path::Path) -> Result<()> {
    use crate::host::phases::{Phase, PhaseSink};

    let home = super::paths::home()?;
    // #1051: `chief` does real work before the Founder pane exists — a
    // preflight, and a beacond that may have to be STARTED and waited for on a
    // 5000 ms budget — and it said nothing about any of it. The operator is
    // sitting at this terminal, so this is where they must see it. One
    // narration mechanism, two renderers: the same phases the Founder pane
    // reads as a stream are printed here as operator lines, so the two surfaces
    // can never come to describe the same launch differently.
    let (sink, mut frames) = PhaseSink::channel(String::new());
    let printer = tokio::spawn(async move {
        while let Some(frame) = frames.recv().await {
            if let Some(line) = new_operator_line(frame.phase) {
                println!("chief: {line}");
            }
        }
    });
    sink.emit(Phase::Preflight, "");
    // #1055 made this answer a CLEARANCE the re-exec carries, so the inner
    // process does not repeat the head. The narration is unchanged by that and
    // deliberately sits on the outer run only: the phase is emitted where the
    // work actually happens, so a launch that skips the second preflight also
    // skips saying it twice.
    let cleared = super::preflight::require_ready(super::preflight::Surface::Founder)?;
    let discovery = super::discovery::Discovery::from_env();
    super::discovery::ensure_running_with_phases(&discovery, &home, &sink).await?;
    // Closing the channel is what ends the printer; awaiting it is what
    // guarantees every line is out before the next thing writes to this
    // terminal.
    drop(sink);
    drop(printer.await);

    if tmux::ambient_tmux() {
        return host_founder(dir).await;
    }

    // ASKED BEFORE ANYTHING IS MINTED. `attach-session` needs a terminal on
    // both ends; without one tmux refuses, and by then chiefd would have
    // created a session and respawned a Founder into it — a live agent holding
    // a genesis endpoint that nobody can reach and nothing will retire.
    if !can_be_attached(std::io::stdin().is_terminal(), std::io::stdout().is_terminal()) {
        return Err(LifecycleError::refused(
            "chief is outside tmux and this caller has no terminal to attach a session to. \
             Run 'chief' from an interactive terminal, or from inside an existing tmux \
             session."
                .to_string(),
        ));
    }

    println!("chief: no ambient tmux session — starting one for this ChiefD operator context");
    // FOUNDER's own socket, never the shared `default` a bare `tmux` uses.
    // Founder has no directory and so no company key; see `boot_socket`.
    let socket = super::company::boot_socket_from_env(None, super::company::FOUNDER_SOCKET);
    // Re-exec THIS binary, by its own absolute path. Not a PATH lookup and not
    // a launcher script: the installed binary is what the operator invoked and
    // it is what must come back up inside the pane.
    let executable = std::env::current_exe().map_err(|error| {
        LifecycleError::host(format!("chief cannot locate its own executable: {error}"))
    })?;
    let command = founder_reexec(&executable);
    boot_founder_session(&socket, dir, &command, cleared.as_ref())?;
    tmux::attach(&socket, FOUNDER_SESSION)
}

/// Bare `chief` is also the inner Founder entry point.
fn founder_reexec(executable: &std::path::Path) -> Vec<String> {
    vec![executable.display().to_string()]
}

/// Whether this caller can be handed a tmux client at all.
///
/// Two observations and no I/O, so the refusal above is provable without a pty.
/// Both ends matter: tmux drives the terminal for input AND output, and a
/// caller with only one of them attached gets the same "open terminal failed"
/// as a caller with neither.
#[must_use]
pub(crate) const fn can_be_attached(stdin_is_terminal: bool, stdout_is_terminal: bool) -> bool {
    stdin_is_terminal && stdout_is_terminal
}

/// Create-or-reuse the ONE Founder session and put `command` in its pane,
/// tagged as the operator's control surface.
///
/// Every tmux mutation `chief` makes outside tmux is here, and the attach
/// that needs a human's terminal is not — which is what makes the placement
/// provable against a real tmux server rather than only in production.
///
/// The tag goes on BEFORE the respawn, so a pane whose authority cannot be
/// proven is refused before anything is started in it.
///
/// # The clearance, and why it is appended rather than forwarded
///
/// `command` is THIS binary, running `new` again. It repeats the host preflight
/// its parent has just passed — measured at 673 ms outside tmux and 657 ms
/// again inside it, nearly all of it one `pi --version` subprocess. `cleared`
/// is the parent's proof, and it rides in on this one respawn.
///
/// It is appended HERE instead of being added to [`tmux::PANE_ENVIRONMENT`]
/// because that list is forwarded to every pane tmux mints for an agent and to
/// the actuator. This value belongs to exactly one child — the founder this
/// process is re-execing — and giving it a wider reach would let a clearance
/// outlive the launch that earned it. See [`super::preflight::Clearance`] for
/// what it can and cannot stand in for.
///
/// # Errors
/// [`LifecycleError::Host`] when tmux will not create the session, will not
/// accept the tag, or will not start the command. Never a session reported as
/// booted that tmux did not actually mint.
fn boot_founder_session(
    socket: &str,
    dir: &std::path::Path,
    command: &[String],
    cleared: Option<&super::preflight::Clearance>,
) -> Result<tmux::Session> {
    // THE FOUNDER PANE RUNS IN THE DIRECTORY THE OPERATOR TYPED `chief` IN,
    // because that directory is the company it is about to create. Without
    // `-c` the re-exec would land in whatever directory the tmux server was
    // started in and create the company THERE — silently, in somebody else's
    // folder.
    let mut forward = tmux::forwarded(&tmux::PANE_ENVIRONMENT);
    forward.extend(cleared.map(super::preflight::Clearance::forwarded));
    tmux::create_operator_session(
        socket,
        FOUNDER_SESSION,
        dir,
        command,
        &forward,
        OPERATOR_CONTROL_TMUX_TAG,
    )
}

/// Host the Founder Pi in the pane this command is already running in.
async fn host_founder(dir: &std::path::Path) -> Result<()> {
    let socket = super::company::boot_socket_from_env(None, super::company::FOUNDER_SOCKET);
    let Ok(pane_id) = std::env::var("TMUX_PANE") else {
        return Err(LifecycleError::host(
            "chief cannot attest an operator outside a tmux pane".to_string(),
        ));
    };
    let pane_id = pane_id.trim().to_string();
    tmux::tag_operator_pane(&socket, &pane_id, OPERATOR_CONTROL_TMUX_TAG)?;
    // The session this pane is actually in — asked of tmux, never assumed to be
    // the one `chief` creates when it is started OUTSIDE tmux. This is the
    // session the operator's client is attached to, and therefore the only
    // session the Founder→CEO handoff can move them off.
    let founder_session = tmux::session_of_pane(&socket, &pane_id)?;

    // The one channel from the launch route back to the process hosting the
    // Founder: the handover happened, so this identity is finished. See
    // [`retire_founder_pi`] for why the Founder is retired by ENDING ITS
    // PROCESS rather than by killing the pane it is running in.
    let retire = Arc::new(Notify::new());
    let endpoint =
        GenesisEndpoint::bind(founder_session, dir.to_path_buf(), Arc::clone(&retire)).await?;
    let url = endpoint.url.clone();
    let served = tokio::spawn(endpoint.serve());

    let status = spawn_founder_pi(dir, &url, &retire).await;
    served.abort();
    status
}

/// Run the Founder Pi in the foreground, with the genesis endpoint published.
///
/// The PORT-SEAM that stood here is CLOSED. Its own text described the ending:
/// *"When piing's Pi-argv builder is reachable from Rust this call becomes a
/// direct `Command::new(pi)` and the seam closes."* It does, and it is —
/// [`super::founder_pi`] owns the argv and the environment, and this function
/// owns only the decisions around it: which checkout, which Pi, which
/// directory, and what the genesis endpoint is called. Pi is still a child
/// process, and still a JavaScript one; what is gone is the TypeScript
/// dispatcher that used to stand between chiefd and it.
///
/// # And why it no longer simply blocks on the child
///
/// The Founder is a PRE-COMPANY identity with exactly one job. Once it has
/// created a company and the operator's client has been moved to that company's
/// CEO, a Founder still sitting in its pane is a second live agent holding a
/// launch tool and a genesis endpoint, in a session the operator has left. So
/// the launch route signals `retire`, and this waits for either the child's own
/// exit or that signal — see [`retire_founder_pi`].
async fn spawn_founder_pi(
    company_dir: &std::path::Path,
    genesis_url: &str,
    retire: &Notify,
) -> Result<()> {
    // THE RESOURCES BESIDE THIS BINARY, not a recorded checkout.
    //
    // TOMBSTONE: this read `~/.chief/launcher-root`, a file naming the source
    // checkout `bun run release` was run from, and refused with "Run 'bun run
    // release' from the checkout" — which is not an instruction you can give a
    // user who installed a release and has no checkout to run it from. See
    // `paths::resource_root` for why the pointer is deleted rather than kept
    // as a fallback.
    //
    // The MARKER probe stays, unchanged, over the one directory whose absence
    // produces a Founder with no launch tool at all — a session that will
    // discuss a company forever and never create one. Same probe, same reason,
    // as `chiefd_host`'s `launcher_assets`.
    let root = super::paths::resource_root().ok_or_else(|| {
        LifecycleError::refused(
            "chief cannot find its own resources: no 'resources' directory is installed beside \
             this binary. Reinstall chief, or run 'bun run release' from a checkout."
                .to_string(),
        )
    })?;
    let marker = root.join(founder_pi::REQUIRED_CHECKOUT_MARKER);
    if !marker.is_dir() {
        return Err(LifecycleError::refused(format!(
            "chief's installed resources are incomplete: no {} at {}. Reinstall chief, or run \
             'bun run release' from a checkout.",
            founder_pi::REQUIRED_CHECKOUT_MARKER,
            marker.display()
        )));
    }

    // THE resolver, not a second one. Founder and `chief attach` read the same
    // function over the same host, so they can no longer disagree about which
    // Pi this box has — they did, and the operator got "Pi is required but no
    // runtime was found" from one and a working session from the other.
    let pi = super::preflight::pi_runtime_or_refusal().map_err(|why| {
        LifecycleError::refused(format!("chief cannot start Founder, because {why}"))
    })?;
    let mut command = founder_command(&root, &pi, company_dir, genesis_url);
    let child = command.spawn().map_err(|error| {
        LifecycleError::host(format!(
            "could not start the Founder runtime at {}: {error}",
            pi.display()
        ))
    })?;
    tracing::info!(event = "founder.pi.spawned", pid = child.id(), "the Founder runtime is up");
    host_until_retired(child, retire).await
}

/// Build the exact Founder Pi process without starting it.
///
/// Asset paths come from `launcher_root`. Process context comes from
/// `company_dir`. Keeping those two facts separate prevents an installed
/// binary from moving Founder into the source checkout.
fn founder_command(
    launcher_root: &std::path::Path,
    pi: &std::path::Path,
    company_dir: &std::path::Path,
    genesis_url: &str,
) -> Command {
    let argv = founder_pi::founder_pi_argv(launcher_root, pi);
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(company_dir)
        .env(FOUNDER_URL_ENV, genesis_url)
        .env(chiefd_log::sink::COMPANY_DIR_ENV, company_dir);
    founder_pi::apply_founder_environment(&mut command);
    // BOTH, and on purpose. The operator is sitting at this terminal watching
    // it say nothing, so this is a `println!`; and whoever reads the log
    // afterwards needs the same instant marked, so it is also a record.
    println!("{}", founder_starting_line());
    tracing::info!(
        event = "founder.pi.starting",
        runtime = %pi.display(),
        checkout = %launcher_root.display(),
        company_dir = %company_dir.display(),
        "the Founder runtime is being started"
    );
    command
}

/// When the next heartbeat is due, over a poll loop that runs 75 times as
/// often as one.
///
/// A pure clock, so the whole rule — nothing before the interval, exactly one
/// beat per interval after it, and never one per poll — is provable without
/// waiting a quarter of a minute per assertion.
struct Heartbeat {
    interval: Duration,
    next: Instant,
}

impl Heartbeat {
    /// A heartbeat that first beats one interval after `started`.
    fn every(interval: Duration, started: Instant) -> Self {
        Self { interval, next: started + interval }
    }

    /// Is one due now? Answering `true` schedules the following one, so a
    /// caller in a tight loop cannot beat twice for the same interval.
    fn due(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + self.interval;
        true
    }
}

/// Hold the Founder Pi until it exits on its own, or until the handover retires
/// it.
///
/// Separated from the spawn so the whole rule is provable against a real child
/// process with no Pi, no checkout and no tmux: the two exits it can take, and
/// which of them is a failure.
///
/// # Errors
/// [`LifecycleError::Host`] when the child cannot be waited on, or when the
/// Founder exited non-zero UNDER ITS OWN POWER. A retired Founder is a success:
/// it was killed on purpose, by this process, because its job was done, and
/// reporting the signal that ended it as a failed session would turn the one
/// completed handover in the product into an error message.
async fn host_until_retired(mut child: std::process::Child, retire: &Notify) -> Result<()> {
    // THE PHASE BOUNDARY, and the honest one: past this line the Founder's own
    // TUI owns the terminal and this process does nothing but wait and beat.
    // Everything BEFORE it — preflight, beacond, "starting the Founder" — is
    // the operator's only feedback and still prints.
    //
    // It lives at the top of this function rather than at the call site because
    // this function's whole contract IS "the child owns the glass now"; a
    // caller that forgot the call would reintroduce the defect silently, and
    // there is no second thing `host_until_retired` could mean.
    chiefd_log::terminal_belongs_to_a_tui();
    let pid = child.id();
    let started = Instant::now();
    let mut heartbeat = Heartbeat::every(FOUNDER_HEARTBEAT, started);
    tracing::info!(
        event = "founder.session.waiting",
        pid,
        "the Founder pane is live; this wait is UNBOUNDED BY DESIGN — the bound is the human \
         sitting in the pane, and it ends when they launch a company or leave"
    );
    // Registered before the first poll of the loop and never re-created:
    // `Notify::notify_one` stores a permit for a signal that arrives before
    // anybody is waiting, so a handover that completes inside one poll interval
    // cannot be missed.
    let retired = retire.notified();
    tokio::pin!(retired);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return exit_status(&status),
            Ok(None) => {}
            Err(error) => {
                return Err(LifecycleError::host(format!(
                    "could not wait on the Founder runtime: {error}"
                )))
            }
        }
        // A minute of silence in a log is indistinguishable from a crash, and
        // this loop can legitimately be quiet for a whole conversation. Cheap
        // by construction: one `Instant` comparison per poll, one line per
        // FOUNDER_HEARTBEAT. `alive` is what the `try_wait` above just
        // observed, which is the fact a reader is actually missing.
        if heartbeat.due(Instant::now()) {
            tracing::info!(
                event = "founder.session.heartbeat",
                waited_ms = chiefd_log::elapsed_ms(started),
                pid,
                alive = true,
                "still waiting for the Founder"
            );
        }
        // os-liveness: the Founder Pi is an interactive child process with no
        // channel back to this one, and its exit is what this function exists
        // to observe. Unbounded BY DESIGN — the bound is the human sitting in
        // the pane, and the loop is what makes the handover interruptible.
        #[allow(clippy::disallowed_methods)]
        let tick = tokio::time::sleep(FOUNDER_PI_POLL);
        tokio::select! {
            () = &mut retired => return retire_founder_pi(child).await,
            () = tick => {}
        }
    }
}

/// How a Founder Pi that ended on its own is reported.
fn exit_status(status: &std::process::ExitStatus) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    Err(LifecycleError::host(format!(
        "the Founder session exited {}",
        status.code().map_or_else(|| "by signal".to_string(), |code| code.to_string())
    )))
}

/// End the Founder, now that the operator is in the CEO.
///
/// # Why the PROCESS and not the pane
///
/// Killing the Founder pane is the obvious move and it is the wrong one. The
/// Founder pane is not always chiefd's to destroy: `chief` hosts the
/// Founder in the pane it was RUN in whenever there is an ambient tmux session,
/// so that pane is most often the operator's own shell, in the operator's own
/// window, in a session full of their other work. `kill-pane` there takes their
/// shell with it and can close a window they never offered.
///
/// Ending the process is the same outcome without the collateral: Pi exits,
/// `chief` returns, and the pane goes back to being whatever it was. In
/// the other branch — `chief` started OUTSIDE tmux, which creates
/// [`FOUNDER_SESSION`] and respawns its pane into this command — the pane's
/// command IS this process, so tmux closes the pane and the Founder session
/// disappears with it. One rule, correct in both.
///
/// `SIGTERM` first because Pi owns a terminal it has put into raw mode and must
/// be given the chance to put it back; `SIGKILL` only for a Founder that ignores
/// it, exactly as [`super::daemon`] stops a daemon.
async fn retire_founder_pi(mut child: std::process::Child) -> Result<()> {
    // os-liveness: flushing an in-flight HTTP response written by another task
    // of this process. Bounded by the constant, and explained on it.
    #[allow(clippy::disallowed_methods)]
    tokio::time::sleep(FOUNDER_RETIREMENT_FLUSH).await;

    let pid = i32::try_from(child.id()).unwrap_or(-1);
    // A CHILD of this process is only reaped by waiting on it, and an unreaped
    // child answers `kill(pid, 0)` as ALIVE for as long as it stays a zombie —
    // so liveness here must be `try_wait`, never the pid probe the tmux-hosted
    // callers use. Getting this wrong would spend the whole grace on a Founder
    // that had already exited, and then `SIGKILL` a zombie.
    end_founder_process(pid, || matches!(child.try_wait(), Ok(Some(_)) | Err(_))).await;
    let _ = child.wait();
    Ok(())
}

/// The signal ladder that ends a Founder, over a bare pid.
///
/// Separated from [`retire_founder_pi`] because the Founder's process is hosted
/// two different ways and BOTH have to be provable. `chief` inside an
/// ambient tmux session runs Pi as a child of the operator's shell, in the
/// operator's own pane; started outside tmux it respawns a pane whose COMMAND
/// is the process. The claim this whole design rests on is that one signal
/// ladder is correct in both — the operator's pane survives, the created
/// session goes away with its pane — and a test can only demonstrate that
/// against a real tmux server if it can drive the production ladder over a
/// process tmux started, which it cannot hold a `Child` for.
///
/// `has_ended` is how the caller can tell: `try_wait` for a child of this
/// process, a pid probe for anything else. See [`retire_founder_pi`] for why
/// the distinction is not cosmetic.
async fn end_founder_process(pid: i32, mut has_ended: impl FnMut() -> bool) {
    let target = nix::unistd::Pid::from_raw(pid);
    let _ = nix::sys::signal::kill(target, nix::sys::signal::Signal::SIGTERM);
    let deadline = Instant::now() + FOUNDER_PI_GRACE;
    while Instant::now() < deadline {
        if has_ended() {
            return;
        }
        // os-liveness: waiting for a SIGTERM to land on another process.
        // Bounded by FOUNDER_PI_GRACE above.
        #[allow(clippy::disallowed_methods)]
        tokio::time::sleep(FOUNDER_PI_POLL).await;
    }
    let _ = nix::sys::signal::kill(target, nix::sys::signal::Signal::SIGKILL);
}

/// The loopback endpoint the Founder extension launches a company through.
///
/// Bound on `127.0.0.1:0` and published only into the Founder pane's own
/// environment: it exists for exactly as long as the Founder session does, is
/// reachable only from this machine, and leaves nothing on disk (Mandate 5).
struct GenesisEndpoint {
    url: String,
    listener: tokio::net::TcpListener,
    /// What the launch route needs to hand the operator over and then stand
    /// this Founder down.
    pane: Arc<FounderPane>,
}

/// The Founder pane, as the launch route sees it.
struct FounderPane {
    /// The tmux session the Founder pane lives in — the handoff's source.
    session: String,
    /// THE DIRECTORY THE COMPANY WILL OCCUPY: the one `chief` was typed in.
    ///
    /// Carried rather than read from this process's own cwd at launch time. It
    /// is the same directory either way today, and it must not be able to stop
    /// being: the endpoint outlives one HTTP request, a future caller could
    /// serve more than one, and a company created in "wherever the server
    /// happens to be" is a company created somewhere the operator never named.
    dir: std::path::PathBuf,
    /// Signalled once, and only once the operator's client is in the CEO.
    retire: Arc<Notify>,
}

impl GenesisEndpoint {
    /// Bind the endpoint, carrying the Founder pane's own session name.
    ///
    /// # Errors
    /// [`LifecycleError::Host`] when loopback cannot be bound.
    async fn bind(
        founder_session: String,
        dir: std::path::PathBuf,
        retire: Arc<Notify>,
    ) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|error| {
            LifecycleError::host(format!("chief could not open its Founder endpoint: {error}"))
        })?;
        let address = listener.local_addr().map_err(|error| {
            LifecycleError::host(format!("chief could not name its Founder endpoint: {error}"))
        })?;
        Ok(Self {
            url: format!("http://{address}"),
            listener,
            pane: Arc::new(FounderPane { session: founder_session, dir, retire }),
        })
    }

    /// Serve until aborted.
    async fn serve(self) {
        let app = axum::Router::new()
            .route("/v1/founder/launch", axum::routing::post(launch_route))
            .with_state(self.pane);
        if let Err(error) = axum::serve(self.listener, app).await {
            tracing::warn!(%error, "chief: the Founder endpoint stopped serving");
        }
    }
}

/// The commands that run a company by hand.
///
/// # Why they are not `chief attach` alone (#751/P8, found by the live proof)
///
/// Genesis creates the company, its daemon and its CEO-only intent — and then
/// NOBODY starts anybody, because chiefd publishes actions and a client applies
/// them. Told only to `chief attach <slug>`, an operator on a version of that
/// verb which could not start an actuator walked into the same missing session
/// and failed the same way. Both messages pointed at a verb that could not start
/// a company, and neither named the one that could.
///
/// One sentence, one home, for the ONE branch that still has no better answer.
///
/// # What this is no longer for
///
/// It used to be what an operator read after every successful launch, because
/// the route created the company and then asked tmux to switch a client into a
/// session nobody had minted. That is now [`hand_over`]'s job: chiefd starts the
/// actuator, waits for it to come up, states the intent to somebody who can converge
/// it and waits for the company session to exist — the same order, and the same
/// code, as `chief attach`. So the two commands below are what a Founder says
/// when NOBODY WAS THERE to hand over, and nothing else: an unattended launch
/// legitimately has no client to move, and the company is up and waiting for
/// whoever comes back to it.
fn run_it_yourself(dir: &std::path::Path) -> String {
    let dir = dir.display();
    format!(
        "chiefd decides who runs, and a client is what runs them. In another terminal run and \
         LEAVE OPEN: cd {dir} && chief actuate — then, in a third: cd {dir} && chief attach"
    )
}

/// Take the operator from the Founder pane to the CEO of the company they just
/// created, and answer how many clients moved.
///
/// # The defect this closes
///
/// The route used to call `tmux::handoff_clients` DIRECTLY on genesis's
/// outcome. Since the actuation switchover that could never work: chiefd
/// publishes actions and a CLIENT applies them, so a company created by genesis
/// has a daemon, a manifest and a committed CEO-only intent — and nobody
/// running. The session the switch names has never existed. What the operator
/// got, on a successful launch, was:
///
/// ```text
/// ✅ Company created · Zipbox.ai
/// You were NOT moved to the CEO, and the CEO is NOT running. could not switch
/// tmux client '/dev/ttys027' from '27' to ChiefD session 'org-zipbox-ai_' on
/// socket 'default': can't find session: org-zipbox-ai_
/// ```
///
/// — followed by two commands to type by hand. The Founder created a company
/// and then asked the human to go and start it.
///
/// So the handover now MAKES the company run first, through
/// [`super::attach::bring_up`] — the same function `chief attach` uses, in the
/// same order, because CEO-only intent stated while nobody is actuating is
/// accepted and then silently lost for ever.
///
/// # Errors
/// [`LifecycleError`] naming what stopped the company from coming up, or the
/// switch from happening. Never fatal to the launch: the company is created and
/// durable by the time this is called, and the caller reports it as such.
async fn hand_over(outcome: &genesis::LaunchOutcome, founder_session: &str) -> Result<usize> {
    let dir = std::path::Path::new(&outcome.dir);
    // AUTHENTICATED: every request below goes to a COMPANY DAEMON, which
    // verifies a presented bearer.
    let client = Client::operator(dir);
    let company = CompanyClient::new(&client, &outcome.url, dir, &super::paths::company_key(dir));
    // The socket the DAEMON recorded, not the one genesis guessed to spawn it
    // with. Same read, same reason, as `chief attach`'s.
    let recorded = company.active_runtime_owner_socket().await?;
    let socket =
        super::company::boot_socket_from_env(recorded.as_deref(), &super::paths::company_key(dir));
    let command = super::attach::resolve_actuator_command(dir)?;
    hand_over_with(dir, &socket, founder_session, &outcome.session, &command).await
}

/// [`hand_over`]'s two steps, over facts a caller has already resolved.
///
/// The split is the ORDER, and the order is the whole defect: the company is
/// made to run, and only then is anybody switched into it. A test can place a
/// stand-in actuator and a real attached client and drive THIS — the sequence
/// production takes — rather than a copy of it written for tests.
///
/// # Errors
/// [`LifecycleError`] from the bring-up or from tmux.
async fn hand_over_with(
    dir: &std::path::Path,
    socket: &str,
    founder_session: &str,
    company_session: &str,
    command: &[String],
) -> Result<usize> {
    super::attach::bring_up(dir, socket, company_session, command).await?;
    // THE DOOR THAT SHIPPED WITH NO RAIL. This is a second, entirely separate
    // way into the operator's company session — `chief attach` is not
    // involved — so rail creation living inside `attach` meant a company built
    // the normal way had no sidebar on its very first run. Between the bring-up
    // and the handover for the same reason attach uses that position: the
    // company must be running before anybody is switched into it, and the rail
    // must exist before the operator sees the glass.
    //
    // Never fatal, exactly like the handover it precedes: the company is
    // created and durable by the time this runs, so a rail that cannot be
    // placed must not turn a successful launch into a failed one. It says so in
    // the log instead.
    if let Err(error) = super::attach::enter_company_session(
        socket,
        company_session,
        dir,
        super::attach::DOOR_FOUNDER_HANDOVER,
    ) {
        tracing::warn!(
            event = "sidebar.rails.declined",
            organization = %dir.display(),
            session = company_session,
            door = super::attach::DOOR_FOUNDER_HANDOVER,
            detail = %error,
            "the company is up and the operator is being handed over without a sidebar"
        );
    }
    tmux::handoff_clients(socket, founder_session, company_session)
}

/// What the Founder does once the handover has been attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AfterHandover {
    /// The operator is in the CEO. This Founder is finished — report an
    /// unqualified launch and stand the Founder Pi down.
    Retire,
    /// Nobody moved. The company is still created; say what is true, and leave
    /// the Founder up, because it is the only surface the operator still has.
    StayUp(String),
}

/// The verdict, kept pure so all three branches are provable without a tmux
/// server, a daemon or a company.
///
/// `moved` is the handover's own answer: how many clients were switched, or the
/// text of what stopped it.
fn after_handover(
    dir: &std::path::Path,
    moved: &std::result::Result<usize, String>,
) -> AfterHandover {
    match moved {
        // Nobody was attached to the Founder session, so there was no operator
        // to move. Truthful either way: an unattended launch has nothing to hand
        // over, and a launch the operator IS watching must never silently claim
        // it moved them.
        Ok(0) => AfterHandover::StayUp(format!(
            "The company is running, but no tmux client was attached to the Founder session, so \
             nobody was handed over. {}",
            run_it_yourself(dir)
        )),
        Ok(_) => AfterHandover::Retire,
        // The failure already names itself and carries its own recovery — it
        // comes from the same code `chief attach` fails with. Appending a
        // second, more generic recovery here is how an operator ends up reading
        // two different next moves for one fault.
        Err(error) => AfterHandover::StayUp(error.clone()),
    }
}

/// A Founder launch, as the stream's one terminal frame reports it.
///
/// The launch outcome plus the handoff warning, which is the only signal that
/// the operator was NOT taken to the CEO. It is a named struct rather than a
/// hand-patched `serde_json::Value` so the terminal frame has one shape the
/// TypeScript client can narrow, exactly like the hosted control plane's.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FounderLaunched {
    #[serde(flatten)]
    outcome: genesis::LaunchOutcome,
    /// Present only when nobody was moved into the CEO.
    #[serde(skip_serializing_if = "Option::is_none")]
    handoff_warning: Option<String>,
}

/// `POST /v1/founder/launch`, narrated — the route whose 4-minute-34 silence
/// both this instrumentation and this phase stream exist for.
///
/// # The defect this closes (#1051)
///
/// This route used to `await genesis::launch(&request)` and answer one JSON
/// body at the end. `genesis::launch` emits every phase of the launch into a
/// sink whose receiver it immediately drops — a documented no-op — so the
/// Founder pane, the surface with the LONGEST wait in the product, was the one
/// surface that narrated nothing. An operator watched
///
///     chiefd_launch_company
///     ⠹ Working...
///
/// for four minutes and thirty-four seconds, at a load average of 0.03, and
/// reasonably concluded it had hung. It had not: it was waiting, legitimately,
/// and saying so was the only thing missing.
///
/// The wait is NOT shortened here and the tool still answers only when the
/// company is up. A launch that returned early would trade a visible wait for
/// an invisible failure.
///
/// # Two outputs, one set of boundaries (#1049 + #1051)
///
/// The `founder.launch` span and its enter/exit lines are for whoever reads
/// the daemon log afterwards; the phase frames are for the human waiting right
/// now. They are deliberately taken at the SAME points — a step worth timing
/// after the fact is exactly a step worth showing during it — so the two
/// never disagree about where the launch was.
#[tracing::instrument(name = "founder.launch", skip_all)]
async fn launch_route(
    axum::extract::State(pane): axum::extract::State<Arc<FounderPane>>,
    axum::Json(request): axum::Json<genesis::LaunchRequest>,
) -> axum::response::Response {
    // The slug is derived before anything is claimed, so every frame — the
    // first one included — is labelled with the company it is about.
    let provisional = genesis::slugify(request.name.trim());
    // The NAME, never the purpose or the bootstrap: the purpose is the
    // operator's own prose and the bootstrap carries a provider route.
    tracing::info!(
        event = "founder.launch.request",
        company = %provisional,
        "the Founder asked for a company"
    );
    let started = std::time::Instant::now();
    super::host::lifecycle_sse::stream_lifecycle(provisional, "launched", move |sink| async move {
        let dir = pane.dir.clone();
        let outcome =
            genesis::launch_with_phases(&dir, &request, &sink).await.inspect_err(|error| {
                tracing::error!(
                    event = "founder.launch.failed",
                    elapsed_ms = chiefd_log::elapsed_ms(started),
                    reason = %error,
                    "the launch route refused"
                );
            })?;
        let launched = hand_over_phase(&outcome, &pane, &sink).await;
        tracing::info!(
            event = "founder.launch.ok",
            company = %outcome.slug,
            elapsed_ms = chiefd_log::elapsed_ms(started),
            "the launch route answered ok"
        );
        Ok(launched)
    })
}

/// Hand the operator over, narrating it, and answer the terminal frame's body.
///
/// Deliberately infallible: the company is created and durable by the time this
/// runs, so a failed handover is reported ON the outcome and never as a failed
/// launch. Reporting a durable company as a failure is a lie in the more
/// damaging direction.
async fn hand_over_phase(
    outcome: &genesis::LaunchOutcome,
    pane: &FounderPane,
    sink: &crate::host::phases::PhaseSink,
) -> FounderLaunched {
    use crate::host::phases::Phase;
    sink.emit(Phase::Handover, "");
    let launched = founder_handover(outcome, pane).await;
    sink.emit(Phase::HandoverComplete, launched.handoff_warning.clone().unwrap_or_default());
    launched
}

/// The handover itself, kept apart from the narration so the decision is
/// testable without a channel.
///
/// HAND THE OPERATOR OVER — start the company, then move them into it. chiefd
/// does this rather than the Founder's TypeScript because placement is
/// chiefd's, the same rule that keeps every other tmux decision out of the
/// extension.
///
/// The SOURCE session is the one the Founder pane is actually in, resolved
/// from tmux when the endpoint was bound. It is not [`FOUNDER_SESSION`]: that
/// name is only created when `chief` is started OUTSIDE tmux, and an
/// operator who ran it from an existing session (the normal case for anyone
/// already living in tmux) has a Founder pane in a session with a completely
/// different name. Hard-coding the created name meant those launches listed
/// clients on a session that did not exist, got "can't find session",
/// classified it as "nobody attached", and handed nobody over — while the tool
/// text said the CEO had been booted for them.
async fn founder_handover(outcome: &genesis::LaunchOutcome, pane: &FounderPane) -> FounderLaunched {
    let handover_started = std::time::Instant::now();
    let moved = hand_over(outcome, &pane.session).await.map_err(|error| error.to_string());
    tracing::info!(
        event = "founder.handover",
        company = %outcome.slug,
        moved = moved.as_ref().copied().unwrap_or_default(),
        ok = moved.is_ok(),
        elapsed_ms = chiefd_log::elapsed_ms(handover_started),
        "the operator handover finished"
    );
    match after_handover(std::path::Path::new(&outcome.dir), &moved) {
        // The operator is in the CEO's pane. Stand this Founder down — see
        // `retire_founder_pi` for why that ends the PROCESS and never the pane.
        AfterHandover::Retire => {
            pane.retire.notify_one();
            FounderLaunched { outcome: outcome.clone(), handoff_warning: None }
        }
        AfterHandover::StayUp(warning) => {
            FounderLaunched { outcome: outcome.clone(), handoff_warning: Some(warning) }
        }
    }
}

// TOMBSTONE (#1051): the unstreamed `launch_route` stood here. It awaited
// `genesis::launch`, threw away every phase the launch emitted, and answered
// one JSON body at the end — which is exactly why a four-and-a-half minute
// launch showed a bare spinner. It is DELETED rather than kept behind a flag:
// there is one client for this route, it ships in this same binary, and a
// second non-narrating path is how the silence would come back. The handover
// comment it carried lives on `hand_over` and `founder_handover`, which is
// where the decision actually is.

#[cfg(test)]
mod tests {
    /// The company directory every test in this module acts on.
    ///
    /// A literal, not a tempdir: nothing below reads or writes a company's
    /// files — what is under test is the tmux handover and the sentences it
    /// carries — and the directory is what those sentences NAME.
    const COMPANY_DIR: &str = "/work/zipbox-ai";

    fn company_dir() -> &'static std::path::Path {
        std::path::Path::new(COMPANY_DIR)
    }

    /// The tmux session the company called `slug` in [`company_dir`] projects
    /// onto, composed through the production helper.
    fn company_session_name(slug: &str) -> String {
        conventional_session_name(slug, &crate::paths::company_key(company_dir()))
    }

    // TOMBSTONE (chief-home-is-cwd §4c): `std::sync::{atomic::*, Arc}`,
    // `CompanyClient` and `Client` were imported for the axum stub chiefd this
    // module used to stand up. The handover makes no HTTP request at all now,
    // so the suite needs no daemon, no client and no shared counters.
    use super::super::company::conventional_session_name;
    use super::super::tmux::test_support::{
        precondition_failed, require_tmux, start_session, unique_socket, wait_until,
    };
    use super::{
        after_handover, boot_founder_session, can_be_attached, founder_command, founder_reexec,
        founder_starting_line, hand_over_with, host_until_retired, new_operator_line, tmux,
        AfterHandover, Heartbeat, FOUNDER_HEARTBEAT, FOUNDER_PI_POLL, FOUNDER_SESSION,
        FOUNDER_URL_ENV, OPERATOR_CONTROL_TMUX_TAG,
    };

    #[test]
    fn founder_runs_in_the_company_directory_and_uses_checkout_assets() {
        let launcher = std::path::Path::new("/opt/chief-source");
        let company = std::path::Path::new("/work/training-harness");
        let pi = launcher.join("node_modules/.bin/pi");
        let command = founder_command(launcher, &pi, company, "http://127.0.0.1:43111");

        assert_eq!(command.get_current_dir(), Some(company));
        assert_eq!(command.get_program(), pi.as_os_str());
        let args =
            command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg.starts_with("/opt/chief-source/packages/piing/")));
        assert!(args.iter().all(|arg| !arg.starts_with("/work/training-harness/packages/piing/")));
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == chiefd_log::sink::COMPANY_DIR_ENV)
                .and_then(|(_, value)| value),
            Some(company.as_os_str())
        );
    }

    #[test]
    fn founder_reexec_is_bare_chief() {
        assert_eq!(founder_reexec(std::path::Path::new("/usr/bin/chief")), ["/usr/bin/chief"]);
    }

    /// #1051. `chief` boots beacond BEFORE the Founder pane exists, so
    /// the pane's phase stream can never carry it — the operator is at this
    /// terminal and this is where it has to be said.
    #[test]
    fn chiefd_new_says_out_loud_that_it_is_starting_beacond() {
        assert!(new_operator_line("beacond-starting").is_some());
        assert!(new_operator_line("beacond-ready").is_some());
        assert!(new_operator_line("preflight").is_some());
    }

    /// The WARM path stays quiet. beacond already answering is one probe that
    /// is over before a human could read a line about it, and a launch that
    /// announces every step it did not have to take buries the ones it did.
    #[test]
    fn an_already_running_beacond_is_not_announced() {
        assert!(new_operator_line("beacond-ensure").is_none());
    }

    /// The company-creation phases belong to the Founder pane's stream, which
    /// runs in a different process from this terminal. Printing them here too
    /// would narrate the same launch twice, in two places, out of step.
    #[test]
    fn chiefd_new_does_not_narrate_the_phases_the_pane_owns() {
        // `ceo-prepare` was a fourth row until chief-home-is-cwd §4c deleted the
        // phase. `chief-start` takes its place rather than merely dropping out:
        // the rule is about a PHASE NAME never reaching this terminal, so the
        // list has to name phases that still exist to keep asserting anything.
        for phase in ["company-claim", "durable-create", "chief-start", "handover"] {
            assert!(new_operator_line(phase).is_none(), "{phase} is narrated twice");
        }
    }

    /// THE 84 SECONDS OF NOTHING. Starting the Founder Pi is the longest step
    /// inside the gap between `beacond.ensure_running` and `founder.launch`,
    /// and the terminal said nothing at all across it.
    #[test]
    fn starting_the_founder_is_announced_to_the_operator() {
        let line = founder_starting_line();
        assert!(line.starts_with("chief: "), "{line}");
        assert!(line.contains("Founder"), "{line}");
    }

    /// And it names no duration.
    ///
    /// A narration that must be re-timed whenever Pi changes its bounded model
    /// refresh is one that will one day be wrong quietly.
    #[test]
    fn the_founder_line_promises_no_duration() {
        let line = founder_starting_line().to_lowercase();
        for timed in ["second", "minute", "15", "up to"] {
            assert!(!line.contains(timed), "'{timed}' is a promise about a clock: {line}");
        }
    }

    /// The heartbeat rule: quiet until the interval, then once per interval,
    /// and never once per poll.
    ///
    /// The poll runs every 200 ms and the wait it sits in is unbounded, so a
    /// per-poll line would be 75 lines a beat of pure noise and no line at all
    /// would leave a whole conversation indistinguishable from a crash.
    #[test]
    fn the_founder_wait_beats_once_per_interval_and_never_once_per_poll() {
        assert!(
            FOUNDER_HEARTBEAT >= std::time::Duration::from_secs(15)
                && FOUNDER_HEARTBEAT <= std::time::Duration::from_secs(30),
            "a heartbeat outside 15-30s is either noise or another silence"
        );
        assert!(FOUNDER_HEARTBEAT > FOUNDER_PI_POLL * 10, "the beat must be far rarer than a poll");

        let started = std::time::Instant::now();
        let mut heartbeat = Heartbeat::every(FOUNDER_HEARTBEAT, started);
        // Every poll up to the interval is silent.
        let mut beats = 0;
        let mut at = started;
        while at < started + FOUNDER_HEARTBEAT {
            if heartbeat.due(at) {
                beats += 1;
            }
            at += FOUNDER_PI_POLL;
        }
        assert_eq!(beats, 0, "nothing may be logged before the first interval is up");

        // The poll that crosses it beats exactly once, and the polls that
        // follow it are silent again.
        assert!(heartbeat.due(at), "the poll that crosses the interval must beat");
        for _ in 0..10 {
            at += FOUNDER_PI_POLL;
            assert!(!heartbeat.due(at), "a beat must not repeat on every poll after it");
        }

        // And one interval later, one more.
        assert!(heartbeat.due(at + FOUNDER_HEARTBEAT), "the beat must keep going for a long wait");
    }

    /// One raw tmux verb against a named socket, answering with its stdout.
    ///
    /// The live tests below OBSERVE tmux rather than asking the module under
    /// test what it did — a session this code reports as created and tmux never
    /// minted is exactly the success-shaped failure that must not pass.
    fn tmux_raw(socket: &str, args: &[&str]) -> String {
        let output = std::process::Command::new("tmux")
            .arg("-L")
            .arg(socket)
            .args(args)
            .output()
            .expect("tmux must be runnable; the caller checked tmux_present() first");
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// THE OPERATOR-REPORTED DEFECT, AGAINST A REAL TMUX SERVER (2026-08-10).
    ///
    /// tmux placement is a product invariant, so the boot path is driven end to
    /// end at the layer it lives in: a real server on a private socket, no
    /// session, and the exact sequence `chief` runs when it is started
    /// outside tmux. What is asserted is what an operator would find: the ONE
    /// Founder session exists under its published name, its pane carries the
    /// operator-control tag chiefd's caller attestation reads, and the command
    /// really is running in that pane — the pane's own words, captured from
    /// tmux, not a return value this code produced about itself.
    ///
    /// A success shape cannot cover a failure here for that reason: a session
    /// that was never minted has no pane to capture and no words in it.
    #[test]
    fn chiefd_new_boots_a_real_tmux_session_when_there_is_none() {
        if !require_tmux() {
            return;
        }
        // A socket nobody has ever served, so the `boot_founder_session`
        // below — production, and the subject of this test — mints with no
        // teardown to lose a race with. It used to open with a `kill-server`,
        // and a mint that lost that race would have been reported as
        // production failing to start its own session. See
        // `tmux::test_support`.
        let socket = unique_socket("new-boots");
        let cleanup = || {
            tmux_raw(&socket, &["kill-server"]);
        };

        // NOTHING EXISTS YET — the operator's shell outside tmux.
        let before = tmux::session_exists(&socket, FOUNDER_SESSION);

        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo founder pane is live; sleep 300".to_string(),
        ];
        let session = boot_founder_session(&socket, company_dir(), &command, None);

        let session = match session {
            Ok(session) => session,
            Err(error) => {
                cleanup();
                panic!("chief must be able to start its own tmux session: {error}");
            }
        };
        let after = tmux::session_exists(&socket, FOUNDER_SESSION);
        let tagged = tmux_raw(
            &socket,
            &["show-options", "-p", "-t", &session.pane_id, OPERATOR_CONTROL_TMUX_TAG],
        );
        let live = wait_until(10_000, || {
            tmux::capture_pane(&socket, FOUNDER_SESSION, 20).contains("founder pane is live")
        });
        let captured = tmux::capture_pane(&socket, FOUNDER_SESSION, 20);
        cleanup();

        assert_eq!(before, Some(false), "the fixture must start with no Founder session");
        assert_eq!(
            after,
            Some(true),
            "'{FOUNDER_SESSION}' must EXIST on the server afterwards — this is the whole packet"
        );
        assert_eq!(session.name, FOUNDER_SESSION, "the one Founder session, never a second name");
        assert!(
            tagged.contains(OPERATOR_CONTROL_TMUX_TAG) && tagged.contains('1'),
            "the Founder pane must be tagged as the operator control surface; got '{tagged}'"
        );
        assert!(
            live,
            "the command must be RUNNING in the pane, not merely respawned into a session that \
             reported success; captured '{captured}'"
        );
    }

    /// A second Founder cannot replace the command in the first pane.
    #[test]
    fn a_second_chiefd_new_refuses_instead_of_respawning_the_live_founder_pane() {
        if !require_tmux() {
            return;
        }
        // Virgin socket: production mints here, and the exactly-one-session
        // assertion below is only evidence if nothing else ever served it.
        let socket = unique_socket("new-reuse");
        let cleanup = || {
            tmux_raw(&socket, &["kill-server"]);
        };
        let sleeper = |what: &str| {
            vec!["sh".to_string(), "-c".to_string(), format!("echo {what}; sleep 300")]
        };

        let first = boot_founder_session(&socket, company_dir(), &sleeper("first-founder"), None);
        let second = boot_founder_session(&socket, company_dir(), &sleeper("second-founder"), None);
        let sessions = tmux_raw(&socket, &["list-sessions", "-F", "#{session_name}"]);
        let first_still_live = wait_until(10_000, || {
            tmux::capture_pane(&socket, FOUNDER_SESSION, 20).contains("first-founder")
        });
        cleanup();

        let first = first.expect("the first boot must succeed");
        let refusal = second.expect_err("the second boot must not replace the live Founder");
        let named: Vec<&str> =
            sessions.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
        assert_eq!(
            named,
            [FOUNDER_SESSION],
            "exactly ONE Founder session may exist; got {named:?}"
        );
        assert!(first_still_live, "the first and only pane must keep its original command");
        assert!(refusal.to_string().contains("already exists"), "exact refusal: {refusal}");
        assert!(!first.pane_id.is_empty(), "the first pane identity stays authoritative");
    }

    /// The parent's proof REACHES the pane it was minted for.
    ///
    /// Against a real tmux server, because the claim is about what a respawned
    /// pane's environment actually contains — the pane prints the variable
    /// itself, so nothing is inferred from the argv chiefd built. This is the
    /// carrier for the whole packet: if the value does not arrive, the inner
    /// process silently pays the 657 ms again and nothing else fails.
    #[test]
    fn the_re_exec_carries_the_parents_clearance_into_the_pane() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("new-clearance");
        let cleanup = || {
            tmux_raw(&socket, &["kill-server"]);
        };

        let cleared = crate::preflight::Clearance::mint("/opt/homebrew/bin/pi")
            .expect("this host has a PATH and a clock");
        // The unit separator inside the value is a CONTROL character, and a
        // terminal does not store one in a cell — `capture-pane` would never
        // show it. `tr` makes it printable so this asserts on the whole value
        // rather than on the part that survives rendering.
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "printf 'clearance=[%s]\\n' \"$(printf '%s' \"${}\" | tr '\\037' '~')\"; sleep 300",
                crate::preflight::CLEARANCE_ENV
            ),
        ];
        let booted = boot_founder_session(&socket, company_dir(), &command, Some(&cleared));
        let carried = wait_until(10_000, || {
            tmux::capture_pane(&socket, FOUNDER_SESSION, 20).contains("clearance=[")
        });
        let captured = tmux::capture_pane(&socket, FOUNDER_SESSION, 20);
        cleanup();

        booted.expect("the founder session must boot");
        assert!(carried, "the pane never printed; captured '{captured}'");
        let (_, expected) = cleared.forwarded();
        let printable = expected.replace('\u{1f}', "~");
        assert!(
            captured.contains(&format!("clearance=[{printable}]")),
            "the respawned pane must inherit the clearance verbatim; captured '{captured}'"
        );
    }

    /// And a boot with NO clearance leaves the variable unset, so the inner
    /// process probes for itself. The negative half matters: a pane that
    /// inherited a stale value from the server's own environment would skip a
    /// probe nobody proved.
    #[test]
    fn a_boot_with_no_clearance_leaves_the_pane_without_one() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("new-no-clearance");
        let cleanup = || {
            tmux_raw(&socket, &["kill-server"]);
        };

        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "printf 'clearance=[%s]\\n' \"${}\"; sleep 300",
                crate::preflight::CLEARANCE_ENV
            ),
        ];
        let booted = boot_founder_session(&socket, company_dir(), &command, None);
        let printed = wait_until(10_000, || {
            tmux::capture_pane(&socket, FOUNDER_SESSION, 20).contains("clearance=[")
        });
        let captured = tmux::capture_pane(&socket, FOUNDER_SESSION, 20);
        cleanup();

        booted.expect("the founder session must boot");
        assert!(printed, "the pane never printed; captured '{captured}'");
        assert!(
            captured.contains("clearance=[]"),
            "with nothing to pass, the pane must carry nothing; captured '{captured}'"
        );
    }

    /// A caller with no terminal is refused BEFORE a session is created.
    ///
    /// The failure this forecloses is the expensive shape: create the session,
    /// respawn a Founder into it, then fail at `attach-session` with "open
    /// terminal failed" and leave a live agent holding a genesis endpoint that
    /// nobody can reach.
    #[test]
    fn a_caller_with_no_terminal_cannot_be_handed_a_session() {
        assert!(can_be_attached(true, true), "an interactive shell can be attached");
        assert!(!can_be_attached(false, true), "a piped stdin cannot drive tmux");
        assert!(!can_be_attached(true, false), "a redirected stdout cannot host tmux");
        assert!(!can_be_attached(false, false), "and neither can a script with neither");
    }

    /// The pid of a child of `parent`, waited for because a shell starts one
    /// on its own schedule. `None` when this machine's `pgrep` cannot answer,
    /// which skips rather than fails the caller.
    fn wait_for_child_of(parent: i32) -> Option<i32> {
        let mut found = None;
        wait_until(10_000, || {
            let Ok(listed) =
                std::process::Command::new("pgrep").args(["-P", &parent.to_string()]).output()
            else {
                return false;
            };
            let text = String::from_utf8_lossy(&listed.stdout);
            found = text.lines().find_map(|line| line.trim().parse::<i32>().ok());
            found.is_some()
        });
        found
    }

    fn session_of_clients(socket: &str) -> String {
        tmux::run(socket, &["list-clients", "-F", "#{session_name}"]).stdout
    }

    /// The Founder→CEO handoff for the operator who did NOT let `chief`
    /// create the Founder session — which is most of them.
    ///
    /// `chief` creates [`FOUNDER_SESSION`] only when it is started outside
    /// tmux. Started from inside a pane it hosts the Founder right there, so
    /// the operator's client sits on whatever session their terminal already
    /// had. A live end-to-end run found exactly that: the Founder pane was in a
    /// session called `e2e`, the launch reported "CEO booted in its ChiefD tmux
    /// session", and the operator never moved, because the handoff listed
    /// clients on `chiefd-founder` — a session that did not exist — and read
    /// "can't find session" as "nobody was attached".
    ///
    /// This drives that exact configuration against a real tmux server with a
    /// real attached client, and asserts BOTH halves: the shipped source
    /// session hands nobody over, and the pane's own session hands the operator
    /// over. The first assertion is the defect, pinned so it cannot come back
    /// as a plausible-looking constant.
    #[test]
    fn the_handoff_moves_the_operator_off_the_session_the_founder_pane_is_actually_in() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("founder-handoff");
        let outer = unique_socket("founder-outer");
        let cleanup = || {
            tmux::run(&socket, &["kill-server"]);
            tmux::run(&outer, &["kill-server"]);
        };

        // The operator's own session, named nothing like the one chiefd would
        // have created, plus the company the CEO booted into.
        start_session(&socket, "e2e", &["sleep", "300"]);
        start_session(&socket, "org-leo-capital", &["sleep", "300"]);
        let pane = tmux::run(&socket, &["list-panes", "-t", "e2e", "-F", "#{pane_id}"]).stdout;
        let pane = pane.lines().next().unwrap_or_default().trim().to_string();
        assert!(pane.starts_with('%'), "the test fixture must have a real pane, got '{pane}'");

        // A REAL client on the Founder pane's session, via a second tmux server
        // for the pty — the same shape as an operator's terminal.
        start_session(&outer, "term", &["bash"]);
        let attach = format!("tmux -L {socket} attach-session -t e2e");
        tmux::run(&outer, &["send-keys", "-t", "term", &attach, "Enter"]);
        if !wait_until(10_000, || session_of_clients(&socket).contains("e2e")) {
            cleanup();
            precondition_failed("could not attach a live tmux client on this machine");
            return;
        }

        // What shipped: the created-session NAME as the handoff source.
        let by_constant = tmux::handoff_clients(&socket, FOUNDER_SESSION, "org-leo-capital");
        // What the route does now: ask tmux which session the pane is in.
        let resolved = tmux::session_of_pane(&socket, &pane);
        let by_pane = resolved
            .as_ref()
            .map(|session| tmux::handoff_clients(&socket, session, "org-leo-capital"));
        let landed = wait_until(5_000, || session_of_clients(&socket).contains("org-leo-capital"));
        let final_session = session_of_clients(&socket);
        cleanup();

        assert_eq!(
            by_constant.expect("an absent session is 'nobody attached', not an error"),
            0,
            "'{FOUNDER_SESSION}' is not where this operator is, so it can only ever hand nobody \
             over — this is the defect, kept asserted so the constant cannot come back"
        );
        assert_eq!(
            resolved.as_deref().expect("tmux must be able to name the pane's session"),
            "e2e",
            "the handoff source is the session the Founder pane is IN"
        );
        assert_eq!(
            by_pane
                .expect("the session resolved")
                .expect("the handoff must not error with a client attached"),
            1,
            "exactly the operator's one client must be handed over"
        );
        assert!(
            landed,
            "the operator's client must END UP on the CEO session; it was on '{final_session}'"
        );
        assert!(
            !final_session.contains("e2e"),
            "no client may be left behind on the Founder pane's session, got '{final_session}'"
        );
    }

    #[test]
    fn there_is_exactly_one_pre_company_session_and_it_is_the_founders() {
        // Launcher mode does not exist. If a second session name ever appears
        // beside this one, a second pre-company identity came with it.
        assert_eq!(FOUNDER_SESSION, "chiefd-founder");
        let source = include_str!("founder.rs");
        let production =
            source.split("#[cfg(test)]").next().expect("split always yields at least one part");
        for retired in ["launcher-mode", "LauncherMode", "triber", "designer"] {
            assert!(
                !production.contains(retired),
                "'{retired}' is a retired pre-company concept and must not reappear"
            );
        }
    }

    #[test]
    fn the_forwarded_pane_environment_carries_no_credential() {
        // Credentials travel only on each person's private 0600 files. A
        // variable name reaching this list is how one would leak into a tmux
        // server's long-lived environment.
        for name in super::tmux::PANE_ENVIRONMENT {
            let lowered = name.to_lowercase();
            assert!(!lowered.contains("key"), "{name}");
            assert!(!lowered.contains("token"), "{name}");
            assert!(!lowered.contains("secret"), "{name}");
            assert!(!lowered.contains("credential"), "{name}");
        }
    }

    #[test]
    fn the_operator_tag_and_genesis_endpoint_names_are_the_published_contract() {
        // Both cross a process boundary: the tag is read by chiefd's caller
        // attestation, the variable by the Founder extension. Changing either
        // alone silently breaks the other half.
        assert_eq!(OPERATOR_CONTROL_TMUX_TAG, "@chiefd-operator-control");
        assert_eq!(FOUNDER_URL_ENV, "CHIEFD_FOUNDER_URL");
    }

    /// A stand-in ACTUATOR: the argv `bring_up` puts in the actuator session,
    /// which converges by minting the company's own session and then stays up.
    ///
    /// TOMBSTONE (chief-home-is-cwd §4c): `watching_chiefd` and its `Watched`
    /// record stood here — an axum stub serving
    /// `/v1/org/runtime/prepare-ceo-only`, whose handler minted the company
    /// session and whose two counters, `prepared_with_an_actuator` and
    /// `prepares`, watched the ORDER the handover's calls arrived in. The route
    /// is deleted, the handover states no intent, and the whole HTTP half of
    /// this test went with it: `hand_over_with` makes no request at all now.
    ///
    /// The invariant those counters proved is not weakened, it is UNREACHABLE —
    /// there is no write left that can be stated too early and silently lost,
    /// because there is no write. And the ordering they proved INDIRECTLY is
    /// now structural rather than observed: the session is minted BY the
    /// actuator process, so it cannot exist before the actuator is running.
    /// That is what production does, where the old stub only imitated it from
    /// an HTTP handler that the ordering was inferred from.
    fn converging_actuator(socket: &str, company_session: &str) -> Vec<String> {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            // `exec sleep` so the actuator window stays live after converging:
            // `ensure_actuator` refuses a session whose window has exited, and
            // `await_company_session` reports an actuator that died as the
            // cause rather than waiting out its budget.
            format!(
                "tmux -L {socket} new-session -d -s {company_session} sleep 300; exec sleep 300"
            ),
        ]
    }

    /// THE DEFECT, and the transition that replaces it.
    ///
    /// What an operator got from a successful `chiefd_launch_company`, in their
    /// own words:
    ///
    /// ```text
    /// ✅ Company created · Zipbox.ai
    /// You were NOT moved to the CEO, and the CEO is NOT running. could not
    /// switch tmux client '/dev/ttys027' from '27' to ChiefD session
    /// 'org-zipbox-ai_' on socket 'default': can't find session: org-zipbox-ai_
    /// ```
    ///
    /// The route created the company and then asked tmux to switch a client
    /// into a session NOBODY HAD MINTED, because chiefd publishes actions and a
    /// client applies them — genesis leaves a durable company with no actuator.
    ///
    /// Both halves are asserted against a real tmux server with a real attached
    /// client:
    ///
    /// 1. the shipped move — switch first — is refused by tmux with exactly
    ///    that message, pinned so it cannot come back looking plausible;
    /// 2. the transition — start the actuator, wait for the company session it
    ///    mints, THEN switch — lands the operator's own client in the CEO and
    ///    leaves nobody behind.
    ///
    /// The ordering the product turns on is carried by the stand-in actuator
    /// ([`converging_actuator`]) rather than by a counter: the company session
    /// is minted BY that process, so a handover that switched before the
    /// bring-up finds nothing to switch into — which is assertion 1, measured
    /// on the same live server moments earlier.
    #[tokio::test]
    async fn the_founder_hands_the_operator_over_only_once_the_company_is_actually_running() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("founder-transition");
        let outer = unique_socket("founder-transition-term");
        let company_session = company_session_name("zipbox-ai");
        let cleanup = || {
            tmux::run(&socket, &["kill-server"]);
            tmux::run(&outer, &["kill-server"]);
        };

        // The operator's own session, hosting the Founder pane. Named nothing
        // like the one `chief` creates, which is the normal case.
        start_session(&socket, "e2e", &["sleep", "300"]);
        let attach = format!("tmux -L {socket} attach-session -t e2e");
        start_session(&outer, "term", &["bash"]);
        tmux::run(&outer, &["send-keys", "-t", "term", &attach, "Enter"]);
        if !wait_until(10_000, || session_of_clients(&socket).contains("e2e")) {
            cleanup();
            precondition_failed("could not attach a live tmux client on this machine");
            return;
        }

        // 1. WHAT SHIPPED: hand over the instant genesis returns.
        let switched_straight_away = tmux::handoff_clients(&socket, "e2e", &company_session);

        // 2. THE TRANSITION. No daemon is stood up because the handover reaches
        // none: `bring_up` is tmux only now.
        let stand_in = converging_actuator(&socket, &company_session);
        let moved =
            hand_over_with(company_dir(), &socket, "e2e", &company_session, &stand_in).await;

        let landed = wait_until(5_000, || session_of_clients(&socket).contains(&company_session));
        let final_session = session_of_clients(&socket);
        // THE LIVE DEFECT, as far as a live server can prove it here: the
        // handover ATTEMPTED to place a rail in the company session.
        //
        // It cannot assert a surviving, tagged rail pane, and the reason is
        // worth stating rather than working around. `enter_company_session`
        // splits a pane running `std::env::current_exe()` — which in
        // production is `chief` and here is THIS TEST BINARY. A test binary
        // invoked as `<bin> sidebar zipbox-ai` reads that as a filter, matches
        // nothing, and exits immediately, so tmux closes the pane before it can
        // be tagged. Making the command injectable would be production
        // indirection that exists only for a test, and asserting on a pane that
        // races its own death would be a flake.
        //
        // What IS asserted: the session the operator was handed into is a real
        // session this code reached and split. The regression lock for the
        // defect itself — that the Founder door places the rail BEFORE it
        // switches the client — is
        // `attach::tests::every_path_into_a_company_session_places_the_rail_first`,
        // which reads the source and therefore cannot rot. The rail's tmux
        // MECHANICS are proven against a live server on the attach path, in
        // `sidebar::tests`, where the harness controls the command.
        let company_windows =
            tmux::run(&socket, &["list-windows", "-t", &company_session, "-F", "#{window_id}"])
                .stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count();
        cleanup();

        assert!(
            company_windows >= 1,
            "the handover must land the operator in a real company session with a window to \
             rail; got {company_windows}"
        );

        let refusal = switched_straight_away
            .expect_err("switching into a session nobody minted cannot succeed")
            .to_string();
        assert!(
            refusal.contains("can't find session"),
            "the shipped move must still be refused by tmux exactly as the operator saw it: \
             {refusal}"
        );

        assert_eq!(
            moved.expect("the handover must not fail with a client attached"),
            1,
            "exactly the operator's one client is handed over"
        );
        assert!(
            landed,
            "the operator's client must END UP in the CEO's session; it was on '{final_session}'"
        );
        assert!(
            !final_session.contains("e2e"),
            "no client may be left behind in the Founder pane's session, got '{final_session}'"
        );
    }

    /// The three things a Founder can do after attempting the handover, and the
    /// one that retires it.
    ///
    /// Retirement is reachable ONLY from a handover that actually moved
    /// somebody. A Founder that stood a company up but moved nobody is the one
    /// surface the operator still has, and killing it would take the only
    /// report of what happened with it.
    #[test]
    fn only_a_handover_that_moved_somebody_retires_the_founder() {
        assert_eq!(after_handover(company_dir(), &Ok(1)), AfterHandover::Retire);
        assert_eq!(after_handover(company_dir(), &Ok(4)), AfterHandover::Retire);

        let nobody = after_handover(company_dir(), &Ok(0));
        let AfterHandover::StayUp(unattended) = nobody else {
            panic!("a launch with nobody attached must not retire the Founder: {nobody:?}");
        };
        assert!(unattended.contains("The company is running"), "{unattended}");
        assert!(unattended.contains("cd /work/zipbox-ai && chief actuate"), "{unattended}");

        let failed =
            after_handover(company_dir(), &Err("the actuator's window exited".to_string()));
        let AfterHandover::StayUp(reported) = failed else {
            panic!("a failed handover must never retire the Founder: {failed:?}");
        };
        assert_eq!(
            reported, "the actuator's window exited",
            "the failure carries its own recovery; a second generic one appended here is how an \
             operator ends up reading two different next moves for one fault"
        );
    }

    /// A retired Founder is a SUCCESS, and its runtime is actually gone.
    ///
    /// The Founder is a pre-company identity with one job. Once the operator's
    /// client is in the CEO, a Founder still running is a second live agent
    /// holding a launch tool and a genesis endpoint in a session the operator
    /// has left. This drives the real supervision loop over a real child
    /// process.
    #[tokio::test]
    async fn a_retired_founder_runtime_is_ended_and_reported_as_a_completed_session() {
        let mut command = std::process::Command::new("sleep");
        command.arg("300");
        let child = command.spawn().expect("a stand-in Founder runtime must start");
        let pid = i64::from(child.id());

        let retire = super::Notify::new();
        // Signalled BEFORE the wait exists, exactly as a fast handover does:
        // `notify_one` stores the permit, so the signal cannot be lost between
        // the route and the host.
        retire.notify_one();
        let outcome = host_until_retired(child, &retire).await;

        outcome.expect("a Founder retired on purpose is a completed session, never a failure");
        assert!(
            !beacond::liveness::pid_is_live(pid),
            "the Founder runtime must be gone; a live one is a second agent with a launch tool"
        );
    }

    /// A Founder that ignores its `SIGTERM` is still ended.
    ///
    /// The grace exists so Pi can put the terminal back the way it found it —
    /// it is not a veto over the handover.
    #[tokio::test]
    async fn a_founder_runtime_that_ignores_sigterm_is_killed_anyway() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "trap '' TERM; sleep 300"]);
        let child = command.spawn().expect("a stand-in Founder runtime must start");
        let pid = i64::from(child.id());

        let retire = super::Notify::new();
        retire.notify_one();
        let outcome = host_until_retired(child, &retire).await;

        outcome.expect("a Founder that had to be killed is still a completed handover");
        assert!(!beacond::liveness::pid_is_live(pid), "SIGKILL must follow the grace");
    }

    /// THE SYMMETRY, BRANCH 1 OF 2: the operator's own pane and shell survive.
    ///
    /// `chief` inside an ambient tmux session hosts the Founder Pi in the
    /// pane the operator RAN IT IN — their shell, their window, a session
    /// holding the rest of their work. This is the branch that makes
    /// `kill-pane` indefensible: chiefd does not own that pane. Retirement
    /// therefore ends the Founder PROCESS, and the pane must be exactly as it
    /// was.
    ///
    /// Driven against a real tmux server, through the production ladder
    /// [`super::end_founder_process`], over a process that is genuinely a child
    /// of the pane's shell — because "the shell survives" is a claim about a
    /// process tree tmux owns, and no fake is evidence about it.
    #[tokio::test]
    async fn retiring_the_founder_leaves_the_operators_own_pane_and_shell_alone() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("founder-ambient");
        // The operator's own session, hosting their own shell.
        start_session(&socket, "e2e", &["bash"]);
        let shell_pid = tmux::run(&socket, &["display-message", "-p", "-t", "e2e", "#{pane_pid}"])
            .stdout
            .trim()
            .parse::<i32>()
            .expect("tmux must name the pane's own process");
        let pane = tmux::run(&socket, &["list-panes", "-t", "e2e", "-F", "#{pane_id}"])
            .stdout
            .trim()
            .to_string();

        // The Founder, running INSIDE that pane as a child of that shell —
        // which is exactly where `chief` puts it in this branch.
        tmux::run(&socket, &["send-keys", "-t", "e2e", "sleep 300", "Enter"]);
        let founder = wait_for_child_of(shell_pid);
        let Some(founder) = founder else {
            tmux::run(&socket, &["kill-server"]);
            precondition_failed("the fixture shell never started a child on this machine");
            return;
        };

        super::end_founder_process(founder, || !beacond::liveness::pid_is_live(i64::from(founder)))
            .await;

        let founder_alive = beacond::liveness::pid_is_live(i64::from(founder));
        let shell_alive = beacond::liveness::pid_is_live(i64::from(shell_pid));
        let session_after = tmux::session_exists(&socket, "e2e");
        let panes_after =
            tmux::run(&socket, &["list-panes", "-t", "e2e", "-F", "#{pane_id}"]).stdout;
        tmux::run(&socket, &["kill-server"]);

        assert!(!founder_alive, "the Founder runtime must be gone");
        assert!(shell_alive, "the operator's own shell must survive its Founder's retirement");
        assert_eq!(
            session_after,
            Some(true),
            "the operator's session is full of their other work and is never chiefd's to close"
        );
        assert!(
            panes_after.contains(&pane),
            "the operator's own pane must still be there; panes were:\n{panes_after}"
        );
    }

    /// THE SYMMETRY, BRANCH 2 OF 2: the session chiefd created goes away.
    ///
    /// `chief` started OUTSIDE tmux creates [`FOUNDER_SESSION`] and
    /// respawns its pane into this exact command, so THE PANE'S COMMAND IS THE
    /// FOUNDER PROCESS. Ending that process is all it takes: tmux reaps the
    /// pane whose command exited, and the session dies with its last pane. No
    /// `kill-pane` and no `kill-session` anywhere — which is what makes one
    /// rule correct in both branches instead of two behaviours that each look
    /// right in their own.
    ///
    /// If this ever reads false, tmux stopped closing a pane whose command
    /// exited, and the symmetry argument must be re-made rather than the
    /// assertion relaxed.
    #[tokio::test]
    async fn retiring_the_founder_takes_the_session_chiefd_created_with_it() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("founder-created");
        // The session `chief` creates when it is started outside tmux,
        // with the Founder host as the pane's own command.
        start_session(&socket, FOUNDER_SESSION, &["sleep", "300"]);
        let founder =
            tmux::run(&socket, &["display-message", "-p", "-t", FOUNDER_SESSION, "#{pane_pid}"])
                .stdout
                .trim()
                .parse::<i32>()
                .expect("tmux must name the pane's own process");
        assert_eq!(
            tmux::session_exists(&socket, FOUNDER_SESSION),
            Some(true),
            "the fixture session must exist before anything is retired"
        );

        super::end_founder_process(founder, || !beacond::liveness::pid_is_live(i64::from(founder)))
            .await;

        // tmux closes the pane on its own schedule once its command is gone.
        let gone =
            wait_until(5_000, || tmux::session_exists(&socket, FOUNDER_SESSION) == Some(false));
        let sessions = tmux::run(&socket, &["list-sessions", "-F", "#{session_name}"]).stdout;
        tmux::run(&socket, &["kill-server"]);

        assert!(
            !beacond::liveness::pid_is_live(i64::from(founder)),
            "the Founder runtime must be gone"
        );
        assert!(
            gone,
            "'{FOUNDER_SESSION}' must disappear with the pane whose command was the Founder — no \
             kill-pane, no kill-session; sessions were:\n{sessions}"
        );
    }

    /// The promise the two tests above are two halves of, stated where it
    /// cannot drift: the Founder path destroys no tmux object, ever.
    ///
    /// The pane is the operator's in one branch and tmux's own to reap in the
    /// other, so there is no branch in which chiefd should be killing one. A
    /// future edit reaching for `kill-pane` — or for `kill_session`, which this
    /// crate really does have and `attach` really does use — is the exact
    /// mistake this decision was made against, and it would pass both live
    /// tests above while breaking the reason they hold.
    #[test]
    fn the_founder_path_never_destroys_a_tmux_object() {
        let source = include_str!("founder.rs");
        let production =
            source.split("#[cfg(test)]").next().expect("split always yields at least one part");
        // CODE only. The documentation above this test argues at length about
        // why `kill-pane` is the wrong move, and a scan that counted prose
        // would forbid explaining the decision it exists to protect — which is
        // how a guard ends up deleting the reasoning instead of the mistake.
        let code = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for destructive in ["kill-pane", "kill_pane", "kill-session", "kill_session", "kill-window"]
        {
            assert!(
                !code.contains(destructive),
                "'{destructive}' must never appear on the Founder path: in the ambient branch the \
                 pane is the operator's own shell, and in the created branch tmux reaps it for \
                 free when the process ends"
            );
        }
    }

    /// A Founder nobody retired still reports its own exit, unchanged.
    ///
    /// The retirement path must not swallow the one failure this function
    /// always had to report: a Founder session that ended badly on its own.
    #[tokio::test]
    async fn a_founder_that_ends_on_its_own_still_reports_how_it_ended() {
        let retire = super::Notify::new();

        let good = std::process::Command::new("true").spawn().expect("spawn");
        host_until_retired(good, &retire).await.expect("a clean exit is a clean exit");

        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exit 7"]);
        let bad = command.spawn().expect("spawn");
        let message = host_until_retired(bad, &retire)
            .await
            .expect_err("a Founder that exited 7 must not be reported as a success")
            .to_string();
        assert!(message.contains("the Founder session exited 7"), "{message}");
    }
}
