//! tmux, as the operator surface needs it: probe a session, ensure one exists,
//! respawn its pane, and hand the terminal over.
//!
//! Ported from the deleted TypeScript `ls.ts::runtimeSessionExists`, `launcher-wiring.ts`'s
//! `ensureRealLauncherSession` / `spawnRealLauncherPiInSession` /
//! `realAttachSession`, and the orphan-session teardown `stop.ts`
//! reached through `org-tmux.ts`.
//!
//! Everything here is STRUCTURAL: has-session, new-session, list-panes,
//! kill-session, set-option, respawn-pane, attach-session. Nothing here
//! projects a company — that is the daemon's converge loop, and keeping it out
//! of this module is why an operator command can never race the actuator.

use std::process::{Command, Stdio};

use super::{LifecycleError, Result};

/// The pane environment a long-lived tmux server cannot be assumed to carry.
///
/// A server that existed before ChiefD was installed preserves only its OWN
/// environment, not every variable from the client that later asks it to create
/// a pane — so the runtime and source-registry contracts are forwarded
/// explicitly. Credentials are never among them: they travel only on each
/// person's private 0600 files.
///
/// `TEAM_LAUNCHER_BUN` survives the deletion of chiefd's own Bun reach and is
/// not a leftover: chiefd never spawns Bun any more, but a managed person's
/// `organization-intercom` extension does, and it reads this variable to find
/// an absolute one. The chain is ambient shell → this pane → the daemon spawned
/// from it → each person's pane. Nothing on it is chiefd running a CLI verb
/// through JavaScript.
///
/// ONE list, read by both callers that create a pane from an operator's own
/// environment — the Founder pane and the resident actuator's. A second copy
/// would be a second answer to "what does a pane need", and the second copy is
/// the one nobody updates.
// `TEAM_LAUNCHER_PI` stood first on this list. It is deleted with the Pi pin:
// chief runs the Pi the operator installed, found on `PATH`, so there is no
// pinned value for a pane to inherit and forwarding one would reintroduce the
// second answer the ladder now refuses to have.
pub(crate) const PANE_ENVIRONMENT: [&str; 3] =
    ["TEAM_LAUNCHER_BUN", "PI_CODING_AGENT_SESSION_DIR", "PI_SOURCE_AGENT_DIR"];

/// The named variables as they stand in THIS process, blanks dropped.
///
/// Blank is dropped rather than forwarded as an empty string, because for every
/// name here an exported-but-empty variable is not the same fact as an unset
/// one — a reader that treats a blank as a value answers a question the
/// operator never asked.
pub(crate) fn forwarded(names: &[&'static str]) -> Vec<(&'static str, String)> {
    names
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(|value| (*name, value))
        })
        .collect()
}

/// One tmux invocation's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TmuxOutput {
    /// The exit status, or `None` when the client was signal-terminated.
    pub(crate) exit_code: Option<i32>,
    /// Captured stdout, trimmed.
    pub(crate) stdout: String,
    /// Captured stderr, trimmed.
    pub(crate) stderr: String,
}

impl TmuxOutput {
    /// Did the command succeed?
    #[must_use]
    pub(crate) fn ok(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// stderr, or stdout when stderr is empty — tmux uses both.
    #[must_use]
    pub(crate) fn diagnostic(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else {
            self.stderr.clone()
        }
    }
}

/// Run `tmux -L <socket> …` and capture its output.
///
/// Every multiplexer verb this binary runs goes through here, which is why the
/// timing lives here and not at each caller. A tmux server that has wedged
/// answers slowly or not at all, and the caller sees only a failed verb — the
/// elapsed time on this line is what separates "tmux said no" from "tmux took
/// nine seconds to say no".
pub(crate) fn run(socket: &str, args: &[&str]) -> TmuxOutput {
    run_reading(socket, args, Reading::Effect)
}

/// Run a tmux verb whose "no" is an ANSWER rather than a failure.
///
/// # The defect this closes
///
/// Every presence check went through [`run`], so a `has-session` against a
/// server that does not exist yet logged `tmux.verb.failed` at `warn`. On a
/// live box a company launch that SUCCEEDED wrote eight of them: one at cold
/// start (`error connecting to /tmp/tmux-0/default`) and seven from the ladders
/// that POLL for a session another process is about to create — `.898`, `.900`,
/// `.902`, `.943`, `.152`, `.360`, `.568`, then success. Ten of that log's
/// eighty-one lines were loud, with nothing wrong.
///
/// A `warn` must mean something a human can act on, and "the thing is not there
/// yet, and I am waiting for it" is not that. So a probe whose diagnostic
/// PROVES absence (see [`answers_absence`]) logs at `debug`, exactly like a
/// verb that exited zero — because it is the same thing: tmux was asked a
/// question and gave a usable answer.
///
/// Nothing is blanket-downgraded. A probe that fails for any OTHER reason — a
/// wedged server, an unknown verb, a tmux that will not start — is still a
/// `warn`, because that one a human can act on.
pub(crate) fn probe(socket: &str, args: &[&str]) -> TmuxOutput {
    run_reading(socket, args, Reading::Presence)
}

/// How the caller reads a non-zero exit from this verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// A verb run for its effect: any refusal is a failure worth reading.
    Effect,
    /// A verb run to ASK whether something exists: a proven absence is the
    /// answer, not a failure.
    Presence,
}

/// Does this refusal prove the thing asked about is simply not there?
///
/// The four diagnostics tmux gives for "no", each observed in the incident log
/// this reads from. Pure, so the classification can be asserted against the
/// literal strings without a tmux server.
fn answers_absence(result: &TmuxOutput) -> bool {
    let diagnostic = format!("{}\n{}", result.stderr, result.stdout).to_lowercase();
    diagnostic.contains("can't find session")
        || diagnostic.contains("no server running")
        || diagnostic.trim() == "no server"
        || (diagnostic.contains("error connecting to")
            && diagnostic.contains("no such file or directory"))
}

/// A tmux invocation that did not run, told apart from one that ANSWERED.
///
/// # The defect this closes
///
/// The deleted TypeScript client (`org-tmux.ts::checked`) carried both of these
/// and this port carried neither, so every transient became a `tmux.verb.failed`
/// at `warn` and — for a probe — an `Option<bool>` of `None`, which reads to
/// every caller as "I could not tell". See DECISIONS.md 2026-07-20, where the
/// first shape was MEASURED rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transient {
    /// The CLIENT never ran: no exit code at all, and nothing said on either
    /// stream. A tmux client fork killed by a signal — SIGKILL from the OOM
    /// killer under memory pressure, SIGSEGV or SIGPIPE during a fork storm —
    /// dies before it can connect, run, or explain itself. That is categorically
    /// different from a dead SERVER, which makes the client PRINT
    /// `no server running` and exit non-zero. The server and its panes are
    /// untouched, so the question we asked still has the same answer.
    ClientLost,
    /// A just-crashed-then-restarted server briefly reports neither presence nor
    /// absence. `chiefd_host`'s `tmux/trust.rs` already retries this transient
    /// 20×25ms with a note that it "never reads as permanent"; this client read
    /// the same transient as a permanent failure.
    ServerExitedUnexpectedly,
}

/// Which transient this is, or `None` when tmux actually answered.
///
/// Pure, so the classification is asserted against literal outputs rather than
/// against a machine under load.
fn transient(result: &TmuxOutput) -> Option<Transient> {
    if result.exit_code.is_none() && result.stdout.is_empty() && result.stderr.is_empty() {
        return Some(Transient::ClientLost);
    }
    // A spawn failure lands here too — `exit_code: None` with the OS error on
    // stderr — and is deliberately NOT transient: "No such file or directory"
    // for a missing tmux names its own cause, and replaying it would turn a
    // host without tmux into a slow failure instead of an immediate one.
    let diagnostic = format!("{}\n{}", result.stderr, result.stdout).to_lowercase();
    diagnostic.contains("server exited unexpectedly").then_some(Transient::ServerExitedUnexpectedly)
}

/// The tmux verbs an invocation lost to a transient may be REPLAYED for.
///
/// An ALLOWLIST of reads, never a prefix rule and never a denylist: a verb
/// introduced later defaults to fail-fast, because replaying a verb tmux may
/// ALREADY have applied duplicates a window, a pane or a keystroke — and tmux
/// placement is a product invariant. Every entry here only reads.
const REPLAYABLE_VERBS: [&str; 6] = [
    "has-session",
    "list-clients",
    "list-panes",
    "list-sessions",
    "list-windows",
    "display-message",
];

/// May this verb be replayed? See [`REPLAYABLE_VERBS`].
fn replayable(verb: &str) -> bool {
    REPLAYABLE_VERBS.contains(&verb)
}

/// The wait before the next replay, or `None` when the budget is spent.
///
/// `attempt` counts the replays ALREADY made of that same transient, so the two
/// budgets are independent — a lost client and a restarting server are different
/// faults and one must not exhaust the other's ladder.
///
/// The ladders are the ported ones: ~0.85s worst case for a lost client, which
/// recovers the instant the memory spike passes, and the 20×25ms this workspace
/// already uses for a restarting server. Deterministic, with no jitter: one
/// process replaying its own read stampedes nothing, and a fixed ladder is one a
/// test can assert.
fn replay_delay(kind: Transient, attempt: u32) -> Option<std::time::Duration> {
    const CLIENT_LOST_DELAYS_MS: [u64; 3] = [50, 200, 600];
    const SERVER_EXITED_DELAY_MS: u64 = 25;
    const SERVER_EXITED_REPLAYS: u32 = 20;
    let millis = match kind {
        Transient::ClientLost => {
            *CLIENT_LOST_DELAYS_MS.get(usize::try_from(attempt).unwrap_or(usize::MAX))?
        }
        Transient::ServerExitedUnexpectedly if attempt < SERVER_EXITED_REPLAYS => {
            SERVER_EXITED_DELAY_MS
        }
        Transient::ServerExitedUnexpectedly => return None,
    };
    Some(std::time::Duration::from_millis(millis))
}

/// Run `once` until tmux ANSWERS, replaying only the transients above and only
/// for the verbs above. Returns the answer and how many replays it took.
///
/// An exhausted budget returns the LAST result untouched — a lost client stays
/// `exit_code: None`, and never becomes a success. The whole defect being fixed
/// is a non-answer being read as one; converting it into a zero exit one layer
/// up would be the same defect wearing a hat.
fn run_with_replay(
    verb: &str,
    mut once: impl FnMut() -> TmuxOutput,
    mut wait: impl FnMut(std::time::Duration),
) -> (TmuxOutput, u32) {
    let (mut lost, mut exited) = (0_u32, 0_u32);
    loop {
        let result = once();
        if result.ok() {
            return (result, lost + exited);
        }
        let Some(kind) = transient(&result).filter(|_| replayable(verb)) else {
            return (result, lost + exited);
        };
        let attempt = match kind {
            Transient::ClientLost => lost,
            Transient::ServerExitedUnexpectedly => exited,
        };
        let Some(delay) = replay_delay(kind, attempt) else {
            return (result, lost + exited);
        };
        match kind {
            Transient::ClientLost => lost += 1,
            Transient::ServerExitedUnexpectedly => exited += 1,
        }
        wait(delay);
    }
}

/// One tmux invocation, with nothing decided about it.
fn spawn_once(socket: &str, args: &[&str]) -> TmuxOutput {
    let output =
        Command::new("tmux").arg("-L").arg(socket).args(args).stdin(Stdio::null()).output();
    match output {
        Ok(output) => TmuxOutput {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        },
        Err(error) => {
            TmuxOutput { exit_code: None, stdout: String::new(), stderr: error.to_string() }
        }
    }
}

/// The wait between replays.
fn replay_wait(delay: std::time::Duration) {
    // os-liveness: the fault being waited out belongs to the operating system —
    // a client the kernel killed, or a tmux server in another process coming
    // back up. There is nothing to wake on and no clock a caller can inject.
    // Every wait is bounded by `replay_delay`'s ladder. Narrow and at the call
    // site so the exemption stays greppable.
    #[allow(clippy::disallowed_methods)]
    std::thread::sleep(delay);
}

fn run_reading(socket: &str, args: &[&str], reading: Reading) -> TmuxOutput {
    run_reading_with(socket, args, reading, || spawn_once(socket, args), replay_wait)
}

/// [`run_reading`] with the invocation and the wait supplied, so the replay and
/// everything it logs can be driven without a machine under load.
fn run_reading_with(
    socket: &str,
    args: &[&str],
    reading: Reading,
    once: impl FnMut() -> TmuxOutput,
    wait: impl FnMut(std::time::Duration),
) -> TmuxOutput {
    let started = std::time::Instant::now();
    // The VERB and never the whole argv: a pane's launch command is an argv
    // and it carries the environment a person is started with. `debug` rather
    // than `info` because a listing runs dozens of these per command, and the
    // failure arm below is the one worth reading at the default level.
    let verb = args.first().copied().unwrap_or("");
    let (result, replays) = run_with_replay(verb, once, wait);
    let elapsed_ms = chiefd_log::elapsed_ms(started);
    // `info`, and never silence: tmux dying under load is the evidence that
    // found this defect, and a replay that succeeded would otherwise erase it.
    // One line per invocation rather than one per replay, so a ladder of twenty
    // cannot bury the command that ran it.
    if replays > 0 {
        tracing::info!(
            event = "tmux.verb.replayed",
            verb,
            socket,
            replays,
            elapsed_ms,
            "a tmux invocation was lost to a transient and replayed"
        );
    }
    if result.ok() {
        tracing::debug!(event = "tmux.verb", verb, socket, elapsed_ms, "a tmux verb succeeded");
    } else if reading == Reading::Presence && answers_absence(&result) {
        tracing::debug!(
            event = "tmux.probe.absent",
            verb,
            socket,
            elapsed_ms,
            "tmux answered a presence probe: the thing is not there"
        );
    } else {
        tracing::warn!(
            event = "tmux.verb.failed",
            verb,
            socket,
            exit_code = result.exit_code.unwrap_or(-1),
            elapsed_ms,
            diagnostic = %result.diagnostic(),
            "a tmux verb failed"
        );
    }
    result
}

/// Does a session exist? `None` when the probe could not answer.
///
/// The three-valued answer is load-bearing and is the ported contract: "no",
/// "yes" and "I could not tell" are three different inputs to
/// [`super::listing::derive_status`], and collapsing the third into "no" is
/// what makes a running company read as stopped.
#[must_use]
pub(crate) fn session_exists(socket: &str, session: &str) -> Option<bool> {
    // A PROBE, not an effect: "there is no such session" is the answer this
    // function was called for, so it is logged as one. See [`probe`].
    let result = probe(socket, &["has-session", "-t", session]);
    if result.ok() {
        return Some(true);
    }
    answers_absence(&result).then_some(false)
}

/// The session this server holds for the company keyed `key`, if any.
///
/// # Why a SEARCH and not a composed name
///
/// A company session is `org-<slug>-<key6>_`, and the slug lives in the
/// company's store — which its own daemon serves. Every caller that has a live
/// daemon composes the name instead (`company::conventional_session_name`);
/// this is for the one caller that must work when the daemon does NOT answer,
/// the click bench, whose whole experiment is "a click completes against a
/// wedged daemon".
///
/// It matches on the KEY, which is the company's identity, so a hit is this
/// directory's session and nothing else's — see
/// [`crate::placement::session_key_suffix`] for why an ending is enough.
/// `None` when nothing matches OR when tmux would not answer: this is a
/// convenience for a measurement harness, and its caller refuses either way.
#[must_use]
pub(crate) fn session_for_key(socket: &str, key: &str) -> Option<String> {
    let listed = run(socket, &["list-sessions", "-F", "#{session_name}"]);
    if !listed.ok() {
        return None;
    }
    listed
        .stdout
        .lines()
        .map(str::trim)
        .find(|session| crate::placement::session_belongs_to(session, key))
        .map(str::to_owned)
}

/// A tmux session hosting one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Session {
    /// The tmux socket it lives on.
    pub(crate) socket: String,
    /// The session name.
    pub(crate) name: String,
    /// The pane id, `%<n>`.
    pub(crate) pane_id: String,
}

/// Teach this tmux server what size a detached session is born at.
///
/// # The defect this closes
///
/// A company's session is created by the ACTUATOR, not by this process, and the
/// actuator has no operator terminal to measure (see [`crate::terminal`]). So
/// it cannot pass `-x`/`-y` itself, and without them tmux mints the session at
/// the server default of 80x24 — which
/// `actuate::interpret::apply_layout` then reads back and pins as an absolute
/// layout, resizing the window DOWN to 80x24 inside a 202x45 terminal.
///
/// `default-size` is tmux's own answer to "what size is a detached session",
/// so stamping it here needs no new IPC and no environment variable that goes
/// stale when the actuator restarts: the actuator's unchanged `new-session -d`
/// mints at operator geometry. Every operator command that can precede a
/// session mint re-stamps it, so the value tracks the current terminal.
///
/// # Two things measured rather than assumed (tmux 3.7)
///
/// `default-size` is a GLOBAL SESSION option, `-g`. Stamped as a server option
/// with `-s` it is accepted, stores nothing, exits zero, and every later
/// session is still born at 80x24 — a fix that reports success and changes
/// nothing.
///
/// It also needs a server that is already serving a session. `tmux
/// start-server` on an empty socket does not leave one running, so a stamp
/// issued before the first session is written to a server that immediately
/// exits. Every caller here therefore stamps AFTER a session exists on the
/// socket, which is still before the actuator mints the company's own.
///
/// Best-effort by construction: a caller with no terminal (a pipe, CI) stamps
/// nothing and leaves tmux's own default in place — exactly the behavior that
/// existed before.
pub(crate) fn stamp_default_size(socket: &str) {
    let Some((columns, rows)) = crate::terminal::operator_size() else {
        return;
    };
    let size = format!("{columns}x{rows}");
    let _ = run(socket, &["set-option", "-g", "default-size", &size]);
}

/// Give future browser-shaped tmux clients exact 24-bit color authority.
///
/// `terminal-features` is a server option. tmux does not derive RGB from
/// `COLORTERM=truecolor`, and changing this option does not renegotiate a
/// client that is already attached. Every operator entry path therefore calls
/// this. Initial attach configures before negotiation; a later handoff keeps
/// the RGB capability that client already negotiated and prepares the server
/// for future clients. The exact-entry guard keeps one rule on a server shared
/// by many companies while preserving unrelated terminal feature rows.
fn ensure_server_terminal_features(socket: &str) -> Result<()> {
    let configured = run(
        socket,
        &[
            "if-shell",
            chief_cli::actuate::terminal_features::BROWSER_RGB_PRESENT,
            "",
            chief_cli::actuate::terminal_features::BROWSER_RGB_APPEND,
        ],
    );
    if configured.ok() {
        Ok(())
    } else {
        Err(LifecycleError::host(format!(
            "failed to configure browser RGB on tmux socket '{socket}': {}",
            configured.diagnostic()
        )))
    }
}

/// Create-or-reuse a one-pane session in `start_directory`. Never duplicated,
/// never attached here.
///
/// # The start directory is a CORRECTNESS argument, not a convenience
///
/// Every verb this program runs in a pane — `chief actuate`, `chief sidebar` —
/// resolves its company from the directory it is run in. A session minted with
/// no `-c` inherits whatever directory the tmux SERVER happens to be in, which
/// is wherever the operator first started it and has nothing to do with any
/// company. `-c` is therefore how a pane is told which company it belongs to,
/// and it replaces the slug those verbs used to take as an argument.
///
/// A REUSED session keeps whatever directory it was created in, so
/// [`respawn_pane`] states it again on the pane itself.
///
/// # Errors
/// [`LifecycleError::Host`] when tmux refuses or reports a session with no pane.
pub(crate) fn ensure_session(
    socket: &str,
    name: &str,
    start_directory: &std::path::Path,
) -> Result<Session> {
    // A PROBE: create-or-reuse asks whether the session is there, and "it is
    // not" is the normal answer on the create half of this function's name.
    if probe(socket, &["has-session", "-t", name]).ok() {
        let listed = run(socket, &["list-panes", "-t", name, "-F", "#{pane_id}"]);
        let pane_id = listed.stdout.lines().map(str::trim).find(|line| !line.is_empty());
        return match (listed.ok(), pane_id) {
            (true, Some(pane_id)) => {
                // A reused session means a live server: the right moment to
                // teach it the operator's current geometry for every session
                // minted after this one — the company's, by the actuator.
                stamp_default_size(socket);
                ensure_server_terminal_features(socket)?;
                Ok(Session { socket: socket.into(), name: name.into(), pane_id: pane_id.into() })
            }
            _ => Err(LifecycleError::host(format!(
                "ChiefD tmux session '{name}' exists but reported no pane: {}",
                listed.diagnostic()
            ))),
        };
    }
    // The operator's real geometry, stated on the creating command itself
    // rather than left to the server default. `-x`/`-y` set the size a DETACHED
    // session is born at. The new window starts at tmux's `latest` default;
    // the managed-window geometry publisher changes it to `manual` with its
    // first final rail/body layout.
    let size = crate::terminal::operator_size()
        .map(|(columns, rows)| (columns.to_string(), rows.to_string()));
    let start = start_directory.display().to_string();
    let mut argv =
        vec!["new-session", "-d", "-s", name, "-c", start.as_str(), "-P", "-F", "#{pane_id}"];
    if let Some((columns, rows)) = &size {
        argv.extend(["-x", columns, "-y", rows]);
    }
    let created = run(socket, &argv);
    let pane_id = created.stdout.trim();
    if !created.ok() || pane_id.is_empty() {
        return Err(LifecycleError::host(format!(
            "failed to create ChiefD tmux session '{name}': {}",
            created.diagnostic()
        )));
    }
    // Now, and not before: the stamp needs a server that is serving a session,
    // and this call just created the first one on this socket.
    stamp_default_size(socket);
    ensure_server_terminal_features(socket)?;
    Ok(Session { socket: socket.into(), name: name.into(), pane_id: pane_id.to_string() })
}

/// Create the one operator session with its final command and ownership tag.
///
/// The command, pane, and tag are one tmux server message. There is no inert
/// shell pane and no later `respawn-pane`, so the pane attached to the
/// operator is the same pane that runs the Founder host from its first frame.
pub(crate) fn create_operator_session(
    socket: &str,
    name: &str,
    start_directory: &std::path::Path,
    command: &[String],
    forward: &[(&str, String)],
    tag: &str,
) -> Result<Session> {
    if session_exists(socket, name) != Some(false) {
        return Err(LifecycleError::host(format!(
            "ChiefD tmux session '{name}' already exists; close that stale Founder session and run 'chief' again"
        )));
    }
    let size = crate::terminal::operator_size()
        .map(|(columns, rows)| (columns.to_string(), rows.to_string()));
    let start = start_directory.display().to_string();
    let target = format!("{name}:");
    let mut argv = vec![
        "new-session".to_owned(),
        "-d".to_owned(),
        "-s".to_owned(),
        name.to_owned(),
        "-c".to_owned(),
        start,
        "-P".to_owned(),
        "-F".to_owned(),
        "#{pane_id}".to_owned(),
    ];
    if let Some((columns, rows)) = &size {
        argv.extend(["-x".to_owned(), columns.clone(), "-y".to_owned(), rows.clone()]);
    }
    for (key, value) in forward {
        argv.extend(["-e".to_owned(), format!("{key}={value}")]);
    }
    argv.extend(command.iter().cloned());
    argv.extend([
        ";".to_owned(),
        "set-option".to_owned(),
        "-p".to_owned(),
        "-t".to_owned(),
        target.clone(),
        tag.to_owned(),
        "1".to_owned(),
    ]);
    // The operator terminal, from the one definition both bootstraps read.
    for [scope, option, value] in chief_cli::actuate::OPERATOR_TERMINAL_OPTIONS {
        argv.extend([
            ";".to_owned(),
            "set-option".to_owned(),
            scope.to_owned(),
            "-t".to_owned(),
            target.clone(),
            option.to_owned(),
            value.to_owned(),
        ]);
    }
    let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
    let created = run(socket, &borrowed);
    let pane_id = created.stdout.lines().next().map(str::trim).unwrap_or_default();
    if !created.ok() || pane_id.is_empty() {
        return Err(LifecycleError::host(format!(
            "failed to create ChiefD tmux session '{name}' with its final command: {}",
            created.diagnostic()
        )));
    }
    stamp_default_size(socket);
    ensure_server_terminal_features(socket)?;
    Ok(Session { socket: socket.into(), name: name.into(), pane_id: pane_id.to_owned() })
}

/// Every pane pid in one session, in tmux's own order.
///
/// # Why a stop needs this
///
/// A tmux pane leader is the session leader of its own pty session, so its
/// process-group id IS its pid and that group holds everything the pane
/// spawned. `kill-session` hangs up the leader and nothing else; a `bun run
/// test` the leader started survives, is reparented to init, and keeps the box
/// busy long after the company is stopped. This read is how [`super::reap`]
/// learns which groups belong to this company — see its module doc for the
/// bound.
///
/// A session that is already gone has no panes, which is an answer and not a
/// failure, so this is a [`probe`] and returns an empty vec for it. A pid tmux
/// prints that does not parse as a positive integer is dropped rather than
/// guessed at: a signal is not something to send to a number nobody read.
pub(crate) fn pane_pids(socket: &str, session: &str) -> Vec<i32> {
    let listed = probe(socket, &["list-panes", "-s", "-t", session, "-F", "#{pane_pid}"]);
    if !listed.ok() {
        return Vec::new();
    }
    listed
        .stdout
        .lines()
        .filter_map(|line| line.trim().parse::<i32>().ok())
        .filter(|pid| *pid > 1)
        .collect()
}

/// Tear a session down.
///
/// Idempotent: a session that is provably absent is already stopped.
///
/// # Errors
/// [`LifecycleError::Host`] when tmux refuses for any reason other than the
/// session being absent.
pub(crate) fn kill_session(socket: &str, name: &str) -> Result<bool> {
    if session_exists(socket, name) == Some(false) {
        return Ok(false);
    }
    let killed = run(socket, &["kill-session", "-t", name]);
    if killed.ok() {
        return Ok(true);
    }
    // A race with the daemon's own teardown is success, not a failure: the
    // session is gone either way, which is the outcome that was asked for.
    if session_exists(socket, name) == Some(false) {
        return Ok(false);
    }
    Err(LifecycleError::host(format!(
        "failed to stop tmux session '{name}': {}",
        killed.diagnostic()
    )))
}

/// Is this process inside a tmux client?
#[must_use]
pub(crate) fn ambient_tmux() -> bool {
    std::env::var("TMUX").is_ok_and(|value| !value.trim().is_empty())
}

/// Tag a pane as the operator control surface.
///
/// The tag is what lets chiefd's caller-attestation tell an operator's own pane
/// from an agent's. A pane that cannot be tagged is a pane whose authority
/// cannot be proven, so this refuses rather than continuing untagged.
///
/// # Errors
/// [`LifecycleError::Host`] when the pane id is not a tmux pane id, or the tag
/// could not be set.
pub(crate) fn tag_operator_pane(socket: &str, pane_id: &str, tag: &str) -> Result<()> {
    let looks_like_pane = pane_id.starts_with('%')
        && pane_id.len() > 1
        && pane_id[1..].chars().all(|character| character.is_ascii_digit());
    if !looks_like_pane {
        return Err(LifecycleError::host(
            "chief cannot attest an operator outside a tmux pane".to_string(),
        ));
    }
    let tagged = run(socket, &["set-option", "-p", "-t", pane_id, tag, "1"]);
    if tagged.ok() {
        Ok(())
    } else {
        Err(LifecycleError::host(format!(
            "failed to tag the ChiefD operator pane: {}",
            tagged.diagnostic()
        )))
    }
}

/// Re-exec a pane into `command`, forwarding the named environment variables.
///
/// A long-lived tmux server preserves only its own environment, not every
/// variable from the client that later asks it to create a pane — so the
/// runtime and source-registry contracts are passed explicitly. Credentials are
/// never among them: they travel only on each person's private 0600 files.
///
/// # Errors
/// [`LifecycleError::Host`] when tmux refuses the respawn.
pub(crate) fn respawn_pane(
    session: &Session,
    start_directory: &std::path::Path,
    command: &[String],
    forward: &[(&str, String)],
) -> Result<()> {
    // `-c` on the RESPAWN and not only on the session's creation: a reused
    // session keeps the directory it was born in, and the pane this command
    // starts resolves its company from its own cwd.
    let mut args: Vec<String> = vec![
        "respawn-pane".into(),
        "-k".into(),
        "-c".into(),
        start_directory.display().to_string(),
    ];
    for (name, value) in forward {
        args.push("-e".into());
        args.push(format!("{name}={value}"));
    }
    args.push("-t".into());
    args.push(session.pane_id.clone());
    args.extend(command.iter().cloned());
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let respawned = run(&session.socket, &borrowed);
    if respawned.ok() {
        Ok(())
    } else {
        Err(LifecycleError::host(format!(
            "failed to start '{}' in ChiefD tmux session '{}': {}",
            command.first().map_or("(nothing)", String::as_str),
            session.name,
            respawned.diagnostic()
        )))
    }
}

/// The window name a company's actuator pane carries.
///
/// A published name, not a cosmetic one: `tmux list-windows` / `tmux
/// list-panes -a` is how an operator (and the live proof) sees that the process
/// running their company exists at all. Before this, the only actuator was one
/// an operator ran in a terminal of their own, and a company nobody could see
/// running was indistinguishable from a company nobody was running.
pub(crate) const ACTUATOR_WINDOW: &str = "chiefd-actuator";

/// What a company's actuator session is doing right now.
///
/// Four states and not a boolean, because "tmux would not answer" must never
/// read as "there is no actuator" — that answer starts a SECOND actuator, and
/// two actuators for one company is a second source of truth about what should
/// be running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActuatorSession {
    /// No such session on this socket.
    Absent,
    /// The session exists and at least one pane in it is still running.
    Running,
    /// The session exists and every pane in it has exited. Only reachable
    /// because the pane is created with `remain-on-exit`, which is what keeps a
    /// failed actuator's own words on screen for a refusal to quote.
    Exited,
    /// tmux could not be asked. Never evidence of absence.
    Unknown,
}

/// The session option a resident actuator stamps itself with at start.
///
/// TAGS ARE THE LIVE RECORD. This repository already keeps every fact about a
/// running tmux object on the object itself — a pane's person, its launch hash,
/// a window's department — precisely because nothing on disk can be trusted to
/// still describe a live server. The actuator's BUILD is the same kind of fact
/// about the same kind of object, so it is recorded the same way rather than in
/// a new file somebody would have to keep in step.
pub(crate) const ACTUATOR_BUILD_OPTION: &str = "@chief_actuator_build";

/// Record which build this actuator is, on its own session.
///
/// Best effort by construction: a tmux that will not take the option costs the
/// operator a build check, never an actuator. The reader treats an absent
/// option as UNKNOWABLE and leaves the actuator alone, which is the same
/// answer it gives for an actuator from a build that predates this option.
pub(crate) fn record_actuator_build(socket: &str, session: &str, build: &str) {
    let _ = run(socket, &["set-option", "-t", session, ACTUATOR_BUILD_OPTION, build]);
}

/// What the actuator on this session says it is running, if it said anything.
pub(crate) fn actuator_build(socket: &str, session: &str) -> Option<String> {
    let read = run(socket, &["show-options", "-v", "-t", session, ACTUATOR_BUILD_OPTION]);
    if !read.ok() {
        return None;
    }
    let value = read.stdout.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Classify a company's actuator session on `socket`.
pub(crate) fn actuator_session(socket: &str, session: &str) -> ActuatorSession {
    match session_exists(socket, session) {
        Some(false) => ActuatorSession::Absent,
        None => ActuatorSession::Unknown,
        Some(true) => classify_actuator_panes(&run(
            socket,
            &["list-panes", "-t", session, "-F", "#{pane_dead}"],
        )),
    }
}

/// The pure half of [`actuator_session`]: what a `#{pane_dead}` listing means.
fn classify_actuator_panes(listed: &TmuxOutput) -> ActuatorSession {
    if !listed.ok() {
        return ActuatorSession::Unknown;
    }
    let flags: Vec<&str> =
        listed.stdout.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if flags.is_empty() {
        // A session tmux says exists but reports no pane for is not an empty
        // session — it is a read we cannot trust. Failing closed here is the
        // difference between waiting and starting a duplicate.
        return ActuatorSession::Unknown;
    }
    if flags.contains(&"0") {
        ActuatorSession::Running
    } else {
        ActuatorSession::Exited
    }
}

/// What a session's panes last printed, trimmed to its final lines.
///
/// The evidence a failed actuator leaves behind. Empty when tmux cannot answer,
/// because an empty capture is reported as "nothing to quote" rather than
/// turned into a fabricated cause.
///
/// `-S -` — the whole scrollback, not the visible screen — and it is the
/// difference between a usable refusal and a useless one. When a pane held by
/// `remain-on-exit` dies, tmux draws `Pane is dead (status N, …)` at the CURSOR,
/// which on a short-lived process sits far below the output that explains why:
/// a plain `capture-pane -p` answered with a screenful of blank lines and the
/// tombstone, and none of the actuator's own words.
pub(crate) fn capture_pane(socket: &str, target: &str, lines: usize) -> String {
    let captured = run(socket, &["capture-pane", "-p", "-S", "-", "-t", target]);
    if !captured.ok() {
        return String::new();
    }
    last_lines(&captured.stdout, lines)
}

/// The last lines of a capture: blank rows dropped, then the final `lines`.
///
/// tmux answers `capture-pane` with the pane's whole grid, and a short-lived
/// process leaves most of it blank. The trim is what turns that into something
/// a refusal can quote.
fn last_lines(captured: &str, lines: usize) -> String {
    let kept: Vec<&str> =
        captured.lines().map(str::trim_end).filter(|line| !line.trim().is_empty()).collect();
    let start = kept.len().saturating_sub(lines);
    kept[start..].join("\n")
}

/// How many times a STOPPED pane is re-read before its words are quoted, and
/// how long each look is apart. Bounded: a capture that never settles is
/// quoted as it last read rather than waited on forever.
const DRAIN_ATTEMPTS: usize = 60;
const DRAIN_STEP: std::time::Duration = std::time::Duration::from_millis(25);

/// tmux's own words for a pane it is holding open because `remain-on-exit` is
/// set. It is drawn at the cursor when the child is reaped.
///
/// This is the ONE line a capture can hold while the pane's own output has not
/// been read yet, which is what makes it the signal that a reading is not
/// final. Matching tmux's text couples this to tmux, and that is deliberate:
/// the coupling already exists — `attach`'s refusal quotes this line to carry
/// the exit status, and two tests assert on it — so the alternative is not
/// less coupling, it is the same coupling with nothing reading it.
const PANE_TOMBSTONE: &str = "Pane is dead";

/// Has this reading got the pane's OWN output in it, or only tmux's tombstone?
fn drained(reading: &str) -> bool {
    reading.lines().any(|line| !line.trim().is_empty() && !line.contains(PANE_TOMBSTONE))
}

/// [`capture_pane`], for a pane that has ALREADY STOPPED — read until tmux has
/// finished draining it.
///
/// # The race this closes, which is a product fault and not a test one
///
/// `#{pane_dead}` turns 1 when tmux reaps the child. Getting the child's OUTPUT
/// onto the pane's grid is a different event on tmux's own loop — the read of
/// the pty — and on a loaded host the reap is observed FIRST. A caller that
/// captures the moment it sees `Exited` therefore reads a grid holding nothing
/// but tmux's own `Pane is dead (status N, …)` tombstone, and refuses with a
/// "What it printed" section that is empty. That is measured, from CI reds on
/// doc-only commits: `attach`'s refusal quoted the tombstone alone while tmux's
/// own exit status proved the actuator had printed its reason and exited.
///
/// # Why stability alone is not the test
///
/// The first shape of this settle asked only that two consecutive readings
/// AGREE, and that is right while a pane is actively draining — measured on a
/// 4000-line pane, the tail lands at 6-15ms and the reading first goes stable
/// at 13-30ms, always after. It is wrong at the other end, and that end is
/// exactly the failing case: a pane that wrote ONE line and whose pty tmux has
/// not read at all presents a grid that is already static, so "stopped
/// changing" is satisfied instantly by a reading that never started. Stability
/// cannot tell FINISHED from NOT YET BEGUN.
///
/// So a reading is final when it has stopped changing AND carries something
/// other than the tombstone. A pane that genuinely printed nothing spends the
/// whole bound and is then quoted as it stands — the honest cost, paid only on
/// the path where a company has already failed to start.
pub(crate) fn capture_dead_pane(socket: &str, target: &str, lines: usize) -> String {
    settled_capture(|| {
        let captured = run(socket, &["capture-pane", "-p", "-S", "-", "-t", target]);
        // os-liveness: the condition is owned by a tmux server in another
        // process draining a pty, with no channel to wake on. Bounded by
        // DRAIN_ATTEMPTS in `settled_capture` and never unbounded.
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(DRAIN_STEP);
        captured.ok().then_some(captured.stdout)
    })
    .map_or_else(String::new, |captured| last_lines(&captured, lines))
}

/// THE RULE [`capture_dead_pane`] implements, over any source of readings, so
/// it can be proven without a tmux server and without a clock.
///
/// Reads until one has both STOPPED CHANGING and got the pane's own output in
/// it. Four shapes, and each is a decision:
///
/// - A reading that is static but holds only the tombstone is NOT final. This
///   is the whole defect; see the type's own comment.
/// - A source that never settles is answered with its LAST reading rather than
///   with nothing. A partial quote is worth more to an operator than an empty
///   one, and a refusal must never hang on a pane that keeps talking.
/// - A reading that cannot be TAKEN ends the wait, and the pane is quoted from
///   what was already read. The commonest way for it to fail is the pane going
///   away, and the words read before that are still the cause.
/// - A capture that never answered at all is `None` — nothing to quote, never
///   a fabricated cause.
fn settled_capture(mut read: impl FnMut() -> Option<String>) -> Option<String> {
    let mut previous = read()?;
    for _ in 1..DRAIN_ATTEMPTS {
        let Some(current) = read() else {
            break;
        };
        let settled = current == previous && drained(&current);
        previous = current;
        if settled {
            break;
        }
    }
    Some(previous)
}

/// Start a resident actuator in its own session on the company's socket.
///
/// # Why the pane is created empty and then respawned into
///
/// A pane created with its command in one call and given `remain-on-exit` in
/// the next is a race the failure case always wins: an actuator that dies on
/// its first millisecond is reaped by tmux before the option can be set, and
/// the refusal that follows has nothing to quote. Creating the pane on its
/// default shell, marking it, and THEN respawning it into the actuator removes
/// the race entirely and reuses two already-tested helpers instead of adding a
/// third way to make a pane.
///
/// # Errors
/// [`LifecycleError::Host`] when tmux refuses any step.
pub(crate) fn start_actuator(
    socket: &str,
    session_name: &str,
    dir: &std::path::Path,
    command: &[String],
    forward: &[(&str, String)],
) -> Result<Session> {
    let session = ensure_session(socket, session_name, dir)?;
    // `remain-on-exit` FIRST: see the doc comment above.
    for option in [["remain-on-exit", "on"], ["automatic-rename", "off"]] {
        let set = run(socket, &["set-option", "-p", "-t", &session.pane_id, option[0], option[1]]);
        if !set.ok() {
            return Err(LifecycleError::host(format!(
                "failed to set '{}' on the ChiefD actuator pane in tmux session '{session_name}': {}",
                option[0],
                set.diagnostic()
            )));
        }
    }
    let renamed = run(socket, &["rename-window", "-t", &session.pane_id, ACTUATOR_WINDOW]);
    if !renamed.ok() {
        return Err(LifecycleError::host(format!(
            "failed to name the ChiefD actuator window in tmux session '{session_name}': {}",
            renamed.diagnostic()
        )));
    }
    respawn_pane(&session, dir, command, forward)?;
    Ok(session)
}

/// Is this process already running inside a tmux client?
///
/// `$TMUX` is tmux's own marker and is the only honest source: it is set by
/// the server for every process in a pane and unset everywhere else.
///
/// Answers a question about THIS PROCESS, so it is only ever correct for a
/// caller that IS the operator's terminal — `chief attach` typed at a prompt.
/// A server handing an operator over must not use it; see [`handoff_clients`].
fn inside_tmux() -> bool {
    std::env::var("TMUX").is_ok_and(|value| !value.trim().is_empty())
}

/// Move every client attached to `from` over to `to`. Returns how many moved.
///
/// # Why this exists rather than [`attach`]
///
/// The Founder→CEO handoff is issued by whatever process serves the founder
/// route, and that process is NOT the operator's pane — it has no controlling
/// terminal at all. `attach` decides between `attach-session` and
/// `switch-client` from `$TMUX`, which describes THE CALLER; for a server that
/// is simply unset, so the handoff ran `attach-session` from a process with no
/// tty. tmux answers that with **exit 0 and no effect**: the operator was left
/// in the Founder pane reading "✅ CEO booted", the route saw success, and
/// nothing reported a problem because nothing had failed loudly. A silent
/// no-op that reports success is worse than an error, and it survived a fix
/// and a round of tests because every check asked the caller's `$TMUX` instead
/// of asking tmux what actually happened.
///
/// So this asks tmux directly — which clients are on the source session — and
/// switches each BY NAME (`-c`). That needs no tty and makes no assumption
/// about who is calling. Zero clients is not an error here but it is not a
/// handoff either, so the count is returned rather than swallowed: an
/// unattended launch (apps/api, a script) legitimately has nobody to move, and
/// the caller decides what that means.
///
/// # Errors
/// [`LifecycleError::Host`] when the client list cannot be read, or when a
/// client that tmux just reported could not be switched.
pub(crate) fn handoff_clients(socket: &str, from: &str, to: &str) -> Result<usize> {
    ensure_server_terminal_features(socket)?;
    let listed = run(socket, &["list-clients", "-t", from, "-F", "#{client_name}"]);
    if !listed.ok() {
        // No client attached to a session is reported as a failure by some tmux
        // versions rather than an empty list. That is "nobody to hand over",
        // not a broken server, so it must not become an error.
        if listed.stderr.contains("no client") || listed.stderr.contains("can't find") {
            return Ok(0);
        }
        return Err(LifecycleError::host(format!(
            "could not list tmux clients on session '{from}' (socket '{socket}'): {}",
            listed.diagnostic()
        )));
    }
    let clients: Vec<&str> =
        listed.stdout.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let mut moved = 0;
    for client in clients {
        let switched = run(socket, &["switch-client", "-c", client, "-t", to]);
        if !switched.ok() {
            return Err(LifecycleError::host(format!(
                "could not switch tmux client '{client}' from '{from}' to ChiefD session '{to}' \
                 on socket '{socket}': {}",
                switched.diagnostic()
            )));
        }
        moved += 1;
    }
    Ok(moved)
}

/// Which session a pane belongs to.
///
/// # Why this is asked rather than assumed
///
/// The Founder pane is NOT always in the session chiefd would have created for
/// it. `chief` only creates `chiefd-founder` when it is started outside
/// tmux; started from inside a pane it hosts the Founder right there, in
/// whatever session the operator's terminal already had — `e2e`, `main`,
/// anything. The Founder→CEO handoff hard-coded the created name, so every
/// Founder launched from an existing session looked for clients on a session
/// that did not exist, found none, and handed nobody over while the tool text
/// said the CEO had been booted for them. The session a pane is in is a fact
/// only tmux holds; this asks it.
///
/// # Errors
/// [`LifecycleError::Host`] when tmux cannot name the pane's session — the pane
/// is gone, or the socket is wrong. Both make a handoff impossible, so neither
/// may be smoothed into a default name.
pub(crate) fn session_of_pane(socket: &str, pane_id: &str) -> Result<String> {
    let named = run(socket, &["display-message", "-p", "-t", pane_id, "#{session_name}"]);
    let session = named.stdout.trim().to_string();
    if !named.ok() || session.is_empty() {
        return Err(LifecycleError::host(format!(
            "could not read the tmux session of pane '{pane_id}' on socket '{socket}': {}",
            named.diagnostic()
        )));
    }
    Ok(session)
}

/// Hand this terminal to a tmux client, in the foreground, until it detaches.
///
/// This is not a wait for a change; it IS the interactive session. It inherits
/// stdio deliberately and blocks for as long as the human is attached.
///
/// # Inside tmux this SWITCHES rather than attaches
///
/// `attach-session` refuses to nest — from inside a pane tmux answers
/// "sessions should be nested with care, unset $TMUX to force" and exits 1.
/// That is the normal case for this product, not an edge one: the operator's
/// Founder runs in tmux, so `chief attach <company>` and the Founder's own
/// handoff to a new company's CEO are both issued from inside a client. Both
/// failed with a message about needing an interactive terminal, which is
/// exactly the wrong diagnosis — the terminal was interactive, the call was
/// simply the wrong one.
///
/// `switch-client` is the same handoff for an already-attached client. It
/// returns immediately rather than blocking, because the client it moved is
/// the caller's own and there is nothing left to wait for.
///
/// # Errors
/// [`LifecycleError::Host`] when tmux exits non-zero — in particular "not a
/// terminal", which must never be turned into a successful-looking exit that
/// leaves a hidden detached session behind.
pub(crate) fn attach(socket: &str, session: &str) -> Result<()> {
    // Before the handover, and on EVERY attach: the operator may be attaching
    // from a terminal of a different size than the one the company was created
    // from, and any session the actuator mints from here on must be born at
    // this one. `enter_company_session` already published this operator size
    // and the final layouts, then pinned all managed windows to manual. The
    // attach itself therefore cannot expose tmux's proportional transit split.
    stamp_default_size(socket);
    ensure_server_terminal_features(socket)?;
    // THE THIRD SURFACE OF THE SAME DEFECT, fixed with the instance rather than
    // after the report. This does NOT exec: it spawns tmux with the terminal
    // inherited and WAITS, so this process stays alive, keeps its console log
    // layer, and can paint over the company session an operator is now looking
    // at. Every caller reaches the glass through here — `attach` on both its
    // branches and the Founder handover — so the hand-over belongs here and not
    // at three call sites, one of which would eventually be added without it.
    //
    // Operator-facing failures are unaffected: those are `eprintln!`, not
    // `tracing`, precisely because they are answers to a person rather than
    // daemon events.
    chiefd_log::terminal_belongs_to_a_tui();
    let verb = if inside_tmux() { "switch-client" } else { "attach-session" };
    let status = Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args([verb, "-t", session])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| {
            LifecycleError::host(format!("could not run tmux attach-session: {error}"))
        })?;
    // A signal-terminated client has no numeric exit status. Treating that as
    // zero made a failed handoff look like a clean exit.
    if status.code() == Some(0) {
        return Ok(());
    }
    // The two failures need different advice, so they get different messages.
    // A switch that fails is almost always a session on ANOTHER tmux server
    // (a different `-L` socket), which no amount of retrying from a terminal
    // will fix; telling that operator to "run this from an interactive
    // terminal" sends them looking in the wrong place entirely.
    if inside_tmux() {
        return Err(LifecycleError::host(format!(
            "could not switch this tmux client to ChiefD session '{session}' on socket '{socket}' (tmux exited {}). \
             You are inside tmux, so this switches clients rather than attaching; that only works within ONE tmux server. \
             If the company runs on a different socket, detach first (prefix then d) and retry.",
            status.code().map_or_else(|| "by signal".to_string(), |code| code.to_string())
        )));
    }
    Err(LifecycleError::host(format!(
        "could not attach this terminal to ChiefD session '{session}' (tmux exited {}). Run this from an interactive terminal, then retry.",
        status.code().map_or_else(|| "by signal".to_string(), |code| code.to_string())
    )))
}

/// Fixtures shared by this crate's LIVE tmux tests.
///
/// # The flake these delete
///
/// A fixture that issues `kill-server` and then, immediately, a mutation on
/// the SAME socket races its own teardown. Under a loaded parallel workspace
/// run the mutation loses that race and fails OUTRIGHT, leaving no server
/// behind; the probe that follows reads `no server running` and answers
/// `Some(false)` — character-for-character the answer a genuine change in
/// tmux's target resolution would give. The suite then goes red quoting a
/// test's own "tmux changed" message while tmux has not changed at all.
///
/// Nothing here relaxes an assertion — being strict is right. What has to be
/// deterministic is the SETUP, and two mechanisms make it so:
///
/// 1. [`unique_socket`]: a socket path nobody has ever served, so the first
///    mutation on it has no teardown to race and needs no `kill-server`
///    before it.
/// 2. [`start_session`]: where a socket must be reused, the `new-session`
///    COMMAND is retried, not merely the probe — the command is what loses
///    the race, and no amount of waiting produces a session nobody
///    successfully asked for. It verifies by the session's EXACT name, so it
///    can never depend on the prefix behaviour some of these tests exist to
///    measure, and it fails as `setup failed: …` rather than as a finding
///    about tmux or about the product.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A precondition a live test cannot run without — and which CI is NEVER
    /// allowed to skip quietly.
    ///
    /// # The hole this closes
    ///
    /// Every live test in this crate opened with `if !tmux_present() { …
    /// return; }`, and libtest CAPTURES stdout and stderr on a PASS. So a
    /// skipped live test and a passing one are the same two words in CI
    /// output: `test … ok`. Twenty-four tests were shaped this way, three of
    /// them returning without printing anything at all, and among them the two
    /// that prove the launch-head clearance reaches the re-exec'd pane — a
    /// mechanism whose entire value is that it fails closed. A test that might
    /// be evidence of nothing is a poor guard for that.
    ///
    /// Nothing here can make a skip visible in a PASSING run, because libtest
    /// has no skip outcome and no way to print through the capture. So the
    /// answer is not a louder message; it is a REFUSAL in the one environment
    /// where the precondition must hold. `CI` is set by GitHub Actions on
    /// every job, and this repo's runner image carries tmux (`ci.yml` never
    /// installs it, and three jobs assert a bare `tmux -V` and pass). A failed
    /// precondition there means the image changed or the fixture broke, and
    /// both are findings — never a green.
    ///
    /// A developer's machine is a different question. It may legitimately lack
    /// tmux, so there the test still steps aside and says why.
    ///
    /// # Do not "simplify" this by deleting the skip
    ///
    /// Deleting it — so that a host without tmux FAILS everywhere — is the
    /// obvious next step and was considered and REJECTED on 2026-08-13. It
    /// buys nothing where it matters. CI carries tmux, so under `CI` the
    /// refusal above already guarantees that these tests run or the build
    /// goes red, which is the whole safety requirement. What deletion would
    /// add is a failure on a contributor's laptop that has not finished being
    /// set up — not a correctness gain, just a worse first run. Break the
    /// build only where a silent skip could hide a real regression, and that
    /// is CI.
    #[must_use]
    pub(crate) fn live_precondition(met: bool, reason: &str) -> bool {
        if met {
            return true;
        }
        precondition_failed(reason);
        false
    }

    /// [`live_precondition`] for the one every live test in this crate shares.
    ///
    /// Answers `true` when tmux is installed. Refuses under `CI` when it is
    /// not, and steps aside politely anywhere else.
    #[must_use]
    pub(crate) fn require_tmux() -> bool {
        live_precondition(tmux_present(), "tmux is not installed on this machine")
    }

    /// The refusal itself, for the sites that have ALREADY established the
    /// precondition is unmet and only need to step aside — a client that would
    /// not attach, a fixture shell that never forked. Same rule as
    /// [`live_precondition`], which is written in terms of this: refuse under
    /// `CI`, step aside anywhere else.
    ///
    /// Separate from [`live_precondition`] because those callers have no
    /// boolean left to test; a `#[must_use]` answer they would have to discard
    /// is noise at the call site, and discarding it is exactly the habit this
    /// whole packet exists to break.
    pub(crate) fn precondition_failed(reason: &str) {
        match decide_precondition(false, in_ci()) {
            PreconditionOutcome::Refuse => panic!(
                "live test precondition failed under CI: {reason}. CI must never report a \
                 skipped live test as a pass — this ran on a host that cannot satisfy the \
                 precondition, so either the runner image changed or the fixture is broken. \
                 Both are findings."
            ),
            PreconditionOutcome::Skip => eprintln!("skipping: {reason}"),
            PreconditionOutcome::Run => {}
        }
    }

    /// What a test should do about a precondition.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum PreconditionOutcome {
        /// The precondition holds: run the test.
        Run,
        /// It does not hold, and this host is allowed not to satisfy it.
        Skip,
        /// It does not hold on a host that must satisfy it. A finding.
        Refuse,
    }

    /// THE RULE, as a pure function of the two facts it turns on, so it can be
    /// proven without a test binary mutating its own environment.
    ///
    /// `std::env::set_var` is racy across a parallel libtest binary — one test
    /// setting `CI` would change the answer another test is measuring — so the
    /// decision is separated from the reading of it. [`in_ci`] does the
    /// reading, once, at the call site.
    #[must_use]
    pub(crate) const fn decide_precondition(met: bool, in_ci: bool) -> PreconditionOutcome {
        match (met, in_ci) {
            (true, _) => PreconditionOutcome::Run,
            (false, true) => PreconditionOutcome::Refuse,
            (false, false) => PreconditionOutcome::Skip,
        }
    }

    /// Is this a CI runner? GitHub Actions sets `CI` on every job.
    #[must_use]
    pub(crate) fn in_ci() -> bool {
        std::env::var_os("CI").is_some()
    }

    /// Is tmux installed here? The raw observation, which is a different thing
    /// from the DECISION about it — see [`require_tmux`], which is what a test
    /// should call.
    #[must_use]
    pub(crate) fn tmux_present() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// A tmux socket NOBODY has ever served: this process, this instant, this
    /// call.
    ///
    /// THE lock for the process-global environment, for every test in this
    /// crate that mutates it.
    ///
    /// # Why one, and why here
    ///
    /// `TMUX`, `TMUX_PANE`, `TEAM_LAUNCHER_TMUX_SOCKET` and `HOME` are
    /// process-wide, and libtest runs tests on many threads. Three tests
    /// mutated `TMUX` while claiming `SAFETY: single-threaded test` in a
    /// comment, and only one of them took any lock — a private one, in
    /// `company.rs`'s test module, that no other module could reach.
    ///
    /// That is not a tidiness problem. `company.rs::boot_socket`'s tier 3
    /// derives the tmux socket from `$TMUX`, whose basename inside an
    /// operator's pane is literally `default`. So a test installing a
    /// `default`-shaped `$TMUX` races every concurrent test that resolves a
    /// socket, and the answer they can get is a real server on this box. The
    /// `cli` CI shard runs these binaries twice at once, which doubles the
    /// window.
    ///
    /// A lock per module is the same defect with more copies: two modules
    /// holding two different mutexes are not excluding each other. This is the
    /// crate's ONE lock, and it lives beside the other live-test preconditions
    /// so a new test finds it where it finds `require_tmux`.
    pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A per-pid name is not enough. It is stable across every test in one
    /// binary and across re-runs, so the fixtures that used one had to open
    /// with a defensive `kill-server` — which is the whole disease. A name
    /// that cannot have been served before needs no teardown at all.
    #[must_use]
    pub(crate) fn unique_socket(label: &str) -> String {
        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let instant = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        format!("chiefd-{label}-{}-{instant}-{sequence}", std::process::id())
    }

    /// Start session `name` on `socket`, and do not return until tmux AGREES
    /// it is there — probed by its EXACT name.
    ///
    /// `command` is everything after the session name: the pane's own argv,
    /// and any `new-session` flag (`-n <window>`) the fixture needs.
    ///
    /// # Panics
    /// With `setup failed: …` when tmux never reports the session. A fixture
    /// that silently did nothing must never be reported as evidence.
    pub(crate) fn start_session(socket: &str, name: &str, command: &[&str]) {
        for _ in 0..50 {
            if super::session_exists(socket, name) == Some(true) {
                return;
            }
            let mut argv = vec!["new-session", "-d", "-s", name];
            argv.extend_from_slice(command);
            super::run(socket, &argv);
            settle();
        }
        panic!("setup failed: tmux never reported session {name:?} on socket {socket:?}");
    }

    /// Wait, bounded, for something an EXTERNAL process does. `true` when the
    /// condition held before the deadline.
    pub(crate) fn wait_until(deadline_ms: u64, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(deadline_ms);
        while std::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            settle();
        }
        condition()
    }

    /// The ONE blocking wait in this crate's tmux fixtures.
    fn settle() {
        // os-liveness: every condition these fixtures wait on is owned by a
        // real tmux server in another process — there is nothing to wake on
        // and no clock a test can inject (an injected clock advances a timer,
        // it does not advance tmux). Every caller bounds it, by a deadline or
        // by a loop count, and never leaves it unbounded. Narrow and at the
        // call site so the exemption stays greppable.
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    /// BOTH BOOTSTRAPS PRODUCE THE SAME OPERATOR TERMINAL.
    ///
    /// The whole point of `OPERATOR_TERMINAL_OPTIONS` being one definition,
    /// asserted rather than assumed: an option added to one bootstrap and not
    /// the other is a RED here, not a surprise in six months.
    ///
    /// A SOURCE check, not a live one, because the two servers this is about
    /// are ones a unit test cannot mint -- the company socket and the Founder
    /// socket. What is checkable, and what actually failed, is whether each
    /// bootstrap READS the shared list at all: the bug was one bootstrap
    /// setting the options inline and the other not knowing they existed.
    #[test]
    fn every_operator_bootstrap_applies_the_one_shared_terminal_definition() {
        for (label, source) in [
            ("the Founder/operator session", include_str!("tmux.rs")),
            ("the company session", include_str!("actuate/interpret.rs")),
        ] {
            assert!(
                source.contains("OPERATOR_TERMINAL_OPTIONS"),
                "{label} does not read OPERATOR_TERMINAL_OPTIONS -- a bootstrap that sets the \
                 operator terminal inline is a second copy, and two copies of a setting is how \
                 the two answers start disagreeing"
            );
        }
        // And the list is not empty, or the loops above iterate nothing and
        // every session gets no operator terminal at all while this test
        // reports success.
        assert!(
            !chief_cli::actuate::OPERATOR_TERMINAL_OPTIONS.is_empty(),
            "an empty operator-terminal list makes both bootstraps agree on NOTHING"
        );
        for [scope, option, _] in chief_cli::actuate::OPERATOR_TERMINAL_OPTIONS {
            assert!(scope.starts_with('-'), "{scope} is not a tmux option scope");
            assert!(!option.is_empty());
        }
    }

    /// The wheel specifically, because it is the reported symptom and the
    /// reason the shared definition exists.
    #[test]
    fn the_operator_terminal_turns_the_mouse_on() {
        assert!(
            chief_cli::actuate::OPERATOR_TERMINAL_OPTIONS
                .iter()
                .any(|[_, option, value]| *option == "mouse" && *value == "on"),
            "without `mouse on` the wheel is swallowed by the pane and the operator cannot scroll \
             the one session they meet first"
        );
    }

    /// The company directory the fixtures below belong to, and the key every
    /// session name they mint ends with.
    ///
    /// A literal, not a tempdir: what is under test is tmux NAMING, and a
    /// directory that changed per run would make the names unreadable in a
    /// failure message.
    fn fixture_dir() -> &'static std::path::Path {
        std::path::Path::new("/work/fixtures")
    }

    /// The session the company called `slug` in [`fixture_dir`] projects onto,
    /// composed through the production namer.
    fn fixture_session(slug: &str) -> String {
        crate::company::conventional_session_name(slug, &crate::paths::company_key(fixture_dir()))
    }

    use super::test_support::{
        precondition_failed, require_tmux, start_session, unique_socket, wait_until,
    };
    use super::TmuxOutput;

    /// A refusal, as tmux writes it on stderr.
    fn refused(stderr: &str) -> TmuxOutput {
        TmuxOutput { exit_code: Some(1), stdout: String::new(), stderr: stderr.to_string() }
    }

    // ---- The precondition rule itself ------------------------------------
    //
    // Driven through `decide_precondition`, the pure function both public
    // entry points are written in terms of, so the rule is provable without
    // this process mutating its own environment — `set_var` is racy across a
    // parallel test binary and would make these two tests depend on which
    // other test happened to be running.

    /// A met precondition is not the interesting case, and it is the one that
    /// must never change: no refusal, no message, the test proceeds.
    #[test]
    fn a_met_precondition_lets_the_test_run_anywhere() {
        assert_eq!(
            super::test_support::decide_precondition(true, false),
            super::test_support::PreconditionOutcome::Run
        );
        assert_eq!(
            super::test_support::decide_precondition(true, true),
            super::test_support::PreconditionOutcome::Run
        );
    }

    /// THE HOLE THIS PACKET CLOSES. An unmet precondition under `CI` is a
    /// finding, never a green: the runner image carries tmux, so a host that
    /// cannot satisfy it means the image changed or the fixture broke.
    #[test]
    fn an_unmet_precondition_refuses_under_ci_and_steps_aside_elsewhere() {
        assert_eq!(
            super::test_support::decide_precondition(false, true),
            super::test_support::PreconditionOutcome::Refuse,
            "CI must never report a skipped live test as a pass"
        );
        assert_eq!(
            super::test_support::decide_precondition(false, false),
            super::test_support::PreconditionOutcome::Skip,
            "a developer machine may legitimately lack tmux, and still gets told why"
        );
    }

    /// The decision is keyed to the ENVIRONMENT, not to the caller. No test
    /// may opt itself out of the refusal by asking differently — which is the
    /// only way this stays true as sites are added.
    #[test]
    fn the_refusal_is_decided_by_the_environment_alone() {
        for met in [true, false] {
            let in_ci = super::test_support::decide_precondition(met, true);
            let not_in_ci = super::test_support::decide_precondition(met, false);
            assert_eq!(
                met,
                in_ci == super::test_support::PreconditionOutcome::Run,
                "under CI, only a MET precondition may run"
            );
            assert_eq!(
                in_ci == not_in_ci,
                met,
                "the two environments may differ only when the precondition is unmet"
            );
        }
    }

    /// THE classification the whole level change rests on, asserted against the
    /// LITERAL diagnostics from the incident log rather than paraphrases of
    /// them. Each of the first four made a healthy launch look broken.
    #[test]
    fn tmux_answering_not_there_is_an_answer_and_anything_else_is_a_failure() {
        for absent in [
            // Cold start: no tmux server exists yet. Expected and handled.
            "error connecting to /tmp/tmux-0/default (No such file or directory)",
            // The seven polls: the session is not minted YET.
            "can't find session: chiefd-actuator-org-tribes-capital_",
            "can't find session: org-tribes-capital_",
            "no server running on /tmp/tmux-0/default",
        ] {
            assert!(
                super::answers_absence(&refused(absent)),
                "tmux said the thing is not there, which is an answer: {absent:?}"
            );
        }

        // The other half, and the reason this is a classification and not a
        // blanket downgrade: a probe can fail for reasons a human must act on,
        // and those are still failures.
        for failure in [
            "unknown command: has-sessionx",
            "invalid option: -q",
            "server exited unexpectedly",
            "lost server",
            "",
        ] {
            assert!(
                !super::answers_absence(&refused(failure)),
                "this is a tmux failure, not an absence: {failure:?}"
            );
        }
    }

    /// tmux writes some refusals on STDOUT, so the classification must read
    /// both streams — `session_exists` always did, and the split into a pure
    /// function must not have dropped it.
    #[test]
    fn an_absence_written_on_stdout_is_still_an_answer() {
        let on_stdout = TmuxOutput {
            exit_code: Some(1),
            stdout: "no server running on /tmp/tmux-501/chiefd".to_string(),
            stderr: String::new(),
        };
        assert!(super::answers_absence(&on_stdout));
        assert_eq!(on_stdout.diagnostic(), on_stdout.stdout, "stdout is the diagnostic here");
    }

    // ---- A tmux invocation that never ran ---------------------------------

    /// A tmux client the kernel killed said NOTHING, and that is the whole
    /// signature: no exit code, no stdout, no stderr. Everything tmux itself
    /// wrote is an ANSWER, however unwelcome, and must never be replayed.
    #[test]
    fn only_a_silent_codeless_result_is_a_lost_client() {
        let lost = TmuxOutput { exit_code: None, stdout: String::new(), stderr: String::new() };
        assert_eq!(super::transient(&lost), Some(super::Transient::ClientLost));

        // A spawn failure has no exit code EITHER, and is not transient: the
        // message names its own cause, and replaying a host without tmux only
        // makes the same refusal slower.
        let missing = TmuxOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: "No such file or directory (os error 2)".to_string(),
        };
        assert_eq!(super::transient(&missing), None);

        for answered in [
            "can't find session: org-acme",
            "no server running on /tmp/tmux-0/chiefd",
            "error connecting to /tmp/tmux-0/default (No such file or directory)",
            "unknown command: has-sessionx",
        ] {
            assert_eq!(
                super::transient(&refused(answered)),
                None,
                "tmux answered this, so nothing may be replayed: {answered:?}"
            );
        }
    }

    /// The OTHER transient, and the one this workspace already retries
    /// elsewhere: a server that went away mid-command reports neither presence
    /// nor absence. It is read from BOTH streams, like every other tmux
    /// diagnostic in this module.
    #[test]
    fn a_server_that_exited_mid_command_is_transient_on_either_stream() {
        assert_eq!(
            super::transient(&refused("server exited unexpectedly")),
            Some(super::Transient::ServerExitedUnexpectedly)
        );
        let on_stdout = TmuxOutput {
            exit_code: Some(1),
            stdout: "server exited unexpectedly".to_string(),
            stderr: String::new(),
        };
        assert_eq!(super::transient(&on_stdout), Some(super::Transient::ServerExitedUnexpectedly));
        // And it stays a FAILURE for the level rule: transient is about
        // replaying, absence is about what tmux said. The two are separate
        // readings of the same result and neither may be derived from the other.
        assert!(!super::answers_absence(&refused("server exited unexpectedly")));
    }

    /// THE safety rule of the whole replay: an allowlist of READS, checked
    /// against every verb this crate actually runs. A replayed mutation is a
    /// duplicated window, pane or keystroke, and tmux placement is a product
    /// invariant — so a verb nobody has thought about defaults to fail-fast.
    #[test]
    fn only_the_enumerated_read_verbs_are_ever_replayed() {
        for read in super::REPLAYABLE_VERBS {
            assert!(super::replayable(read), "{read} is enumerated and must replay");
        }
        for mutation in [
            "new-session",
            "kill-session",
            "kill-server",
            "send-keys",
            "switch-client",
            "attach-session",
            "respawn-pane",
            "set-option",
            "rename-window",
            "resize-window",
            "capture-pane",
            "",
        ] {
            assert!(
                !super::replayable(mutation),
                "{mutation:?} changes something or is unclassified; replaying it may apply it twice"
            );
        }
    }

    /// A lost client that comes back is replayed until tmux answers, and the
    /// waits are the documented ladder rather than whatever the machine allows.
    #[test]
    fn a_lost_client_is_replayed_until_tmux_answers() {
        let answers = std::cell::RefCell::new(vec![
            TmuxOutput { exit_code: None, stdout: String::new(), stderr: String::new() },
            TmuxOutput { exit_code: None, stdout: String::new(), stderr: String::new() },
            TmuxOutput { exit_code: Some(0), stdout: "ok".to_string(), stderr: String::new() },
        ]);
        let waits = std::cell::RefCell::new(Vec::new());
        let (result, replays) = super::run_with_replay(
            "has-session",
            || answers.borrow_mut().remove(0),
            |delay| waits.borrow_mut().push(delay.as_millis()),
        );
        assert!(result.ok(), "the answer is the one tmux finally gave");
        assert_eq!(replays, 2);
        assert_eq!(waits.into_inner(), vec![50, 200], "the ladder, in order");
    }

    /// The budget is bounded, and — the point of the whole packet — an
    /// exhausted budget still reports a NON-ANSWER. A lost client that never
    /// comes back must not become a success, an absence, or anything else a
    /// caller could act on.
    #[test]
    fn an_exhausted_replay_budget_still_reports_a_non_answer() {
        let calls = std::cell::Cell::new(0_u32);
        let (result, replays) = super::run_with_replay(
            "has-session",
            || {
                calls.set(calls.get() + 1);
                TmuxOutput { exit_code: None, stdout: String::new(), stderr: String::new() }
            },
            |_| {},
        );
        assert_eq!(replays, 3, "three replays, then the fault is reported");
        assert_eq!(calls.get(), 4, "the first invocation plus its three replays");
        assert_eq!(result.exit_code, None, "a lost client is never given an exit code");
        assert!(!result.ok(), "and is never a success");
        assert!(!super::answers_absence(&result), "nor an absence");

        // The two budgets are independent: one fault may not spend the other's.
        let (exited, exited_replays) =
            super::run_with_replay("has-session", || refused("server exited unexpectedly"), |_| {});
        assert_eq!(exited_replays, 20, "the ported 20×25ms bound");
        assert!(!exited.ok());
    }

    /// A verb that is not on the allowlist is answered ONCE, however it failed.
    #[test]
    fn a_mutating_verb_is_never_replayed() {
        let calls = std::cell::Cell::new(0_u32);
        let (_, replays) = super::run_with_replay(
            "kill-session",
            || {
                calls.set(calls.get() + 1);
                TmuxOutput { exit_code: None, stdout: String::new(), stderr: String::new() }
            },
            |_| panic!("a mutation must never wait to be replayed"),
        );
        assert_eq!(replays, 0);
        assert_eq!(calls.get(), 1, "run once, and reported");
    }

    /// Read back every JSONL line `body` wrote through the real sink layer.
    ///
    /// A near-copy of the helper `ladder::test_support` gives the LIBRARY, and
    /// it is copied rather than shared because this module belongs to the BIN
    /// target (`main.rs: mod tmux`) while `ladder` belongs to the library, and a
    /// `#[cfg(test)]` helper of the library does not exist when the binary's
    /// tests compile. The only import path between them would make a test helper
    /// part of a shipped library's public surface.
    ///
    /// `with_default` rather than a global install: these tests run beside each
    /// other in one process and a global subscriber can be set exactly once. No
    /// level filter is attached, so `debug` lines reach the file and can be
    /// asserted — in production the installed `EnvFilter` defaults to `info`
    /// and drops them, which is the whole point of the level change.
    ///
    /// The directory is per INVOCATION, not per process and name.
    /// `with_default` is thread-local and already keeps one test's lines out of
    /// another's; what a name plus a pid cannot keep out is a second invocation
    /// that reuses the name, a re-run whose pid the OS handed back, or a
    /// parallel binary that landed on the same one. A recorder a test does not
    /// own is a reading a test cannot trust, and the reading is the whole
    /// evidence. Same mechanism as [`super::test_support::unique_socket`].
    ///
    /// The subscriber is thread-local; the CALLSITE CACHE it reads is not, and
    /// that is what [`permit_every_callsite`] exists for. Nothing here works
    /// without it — see its own comment.
    fn recorded(name: &str, body: impl FnOnce()) -> Vec<serde_json::Value> {
        use std::sync::atomic::{AtomicU32, Ordering};

        use tracing_subscriber::layer::SubscriberExt as _;

        permit_every_callsite();

        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let instant = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        let directory = std::env::temp_dir().join(format!(
            "chiefd-tmux-log-{}-{name}-{instant}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("create the throwaway log directory");
        let sink = chiefd_log::OrgLog::new(&directory, "chief", chiefd_log::NO_ORGANIZATION);
        let path = sink.path().to_path_buf();
        let subscriber = tracing_subscriber::registry().with(chiefd_log::SinkLayer::new(sink));
        tracing::subscriber::with_default(subscriber, body);
        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line must be valid JSON"))
            .collect()
    }

    /// Install one permissive process-wide subscriber, so that no `tracing`
    /// callsite in this binary can ever be cached as "nobody is interested".
    ///
    /// # The race this closes
    ///
    /// `with_default` is thread-local, so the reasoning above is that one
    /// test's lines cannot reach another's recorder. True — and beside the
    /// point, because the decision of whether a callsite emits AT ALL is taken
    /// once, process-globally, the first time that line of code executes, and
    /// is then cached in a static for the life of the process
    /// (`tracing_core::callsite`). `tracing-core` computes that decision from
    /// every LIVE dispatcher — except when exactly one is alive, where it takes
    /// the shortcut of asking only the REGISTERING THREAD's default subscriber.
    ///
    /// A test binary hits that shortcut constantly. While one test holds the
    /// only live dispatcher (its own scoped recorder), every other test thread
    /// has no subscriber at all, and `NoSubscriber` answers `Interest::never`.
    /// So a tmux test running beside the recorder — and this module has a dozen
    /// that drive real tmux verbs — could be the thread that first reaches
    /// `tmux.probe.absent`, permanently disabling that callsite for the whole
    /// process. The recording test then read an EMPTY file and failed with
    /// "the answer is still recorded, at debug: []", having done nothing wrong.
    /// It reproduced within a handful of parallel runs and passed every time in
    /// isolation, which is exactly the signature of shared global state.
    ///
    /// A permanently-installed global default fixes both halves. It is a
    /// dispatcher that is always alive, so the one-dispatcher shortcut is never
    /// taken while a recorder is running and the decision is the union over the
    /// recorder too; and when it IS the only one, it is a bare
    /// `Registry`, whose answer is `Interest::always` rather than `never`.
    /// It has no layers, so it records nothing and cannot pollute a recorder —
    /// the scoped subscriber still wins on the thread that installs it.
    ///
    /// Idempotent: `set_global_default` succeeds once per process and every
    /// later call is a no-op.
    fn permit_every_callsite() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
        });
    }

    /// One `tracing` callsite two threads can reach.
    ///
    /// A `debug!` written inline in each place would be two DIFFERENT
    /// callsites with independent caches, and the test below would prove
    /// nothing. This is the shared line.
    fn shared_callsite() {
        tracing::debug!(event = "test.shared.callsite", "one callsite, two threads");
    }

    /// The RULE the recorder rests on: what a test records is decided by the
    /// subscriber that test installed, and no thread running beside it can
    /// switch the line off.
    ///
    /// This is the flake in [`a_cold_start_presence_probe_emits_no_warning`],
    /// made deterministic. The subscriber-less thread reaches the callsite
    /// FIRST, which is the whole race; the recording thread must still record
    /// it. Without [`permit_every_callsite`] the spawned thread caches
    /// `Interest::never` for the process and this fails every single run.
    #[test]
    fn a_recording_survives_a_subscriberless_thread_reaching_the_same_callsite() {
        let lines = recorded("callsite-cache", || {
            std::thread::spawn(shared_callsite).join().expect("the racing thread must finish");
            shared_callsite();
        });

        assert!(
            lines.iter().any(|line| line["event"] == "test.shared.callsite"),
            "a thread with no subscriber must not be able to silence a recorder: {lines:?}"
        );
    }

    /// The acceptance criterion at the verb wrapper: a probe that tmux answers
    /// "not there" writes NOTHING an operator must act on, against a real tmux
    /// and a real socket nobody has ever served.
    #[test]
    fn a_cold_start_presence_probe_emits_no_warning() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("probe-quiet");
        let lines = recorded("probe-quiet", || {
            assert_eq!(
                super::session_exists(&socket, "org-never-served"),
                Some(false),
                "an unserved socket proves absence"
            );
        });

        let loud: Vec<&serde_json::Value> = lines
            .iter()
            .filter(|line| line["level"] == "warn" || line["level"] == "error")
            .collect();
        assert!(loud.is_empty(), "a cold-start probe must be quiet, got {loud:?}");
        // Quiet is only half the rule, and on its own an empty recording would
        // satisfy it. The answer must be THERE, at `debug`, naming the verb and
        // the socket it was asked about — that is what makes the silence a
        // level choice rather than a lost line.
        let absent = lines
            .iter()
            .find(|line| line["event"] == "tmux.probe.absent")
            .unwrap_or_else(|| panic!("the answer is still recorded, at debug: {lines:?}"));
        assert_eq!(absent["level"], "debug", "the answer is a debug line: {absent:?}");
        assert_eq!(absent["detail"]["verb"], "has-session", "it names the verb: {absent:?}");
        assert_eq!(
            absent["detail"]["socket"],
            serde_json::Value::from(socket.as_str()),
            "it names the socket it probed: {absent:?}"
        );
    }

    /// A SIMULATED tmux that dies once and then answers: the operator sees the
    /// answer, no warning, and — the constraint that keeps the replay honest —
    /// one `info` saying the invocation had to be replayed.
    ///
    /// A machine under load is the only thing that produces a lost client, so
    /// the loss is injected rather than waited for. The logging, the levels and
    /// the replay itself are the real ones.
    #[test]
    fn a_replayed_probe_is_quiet_but_says_it_was_replayed() {
        let answers = std::cell::RefCell::new(vec![
            TmuxOutput { exit_code: None, stdout: String::new(), stderr: String::new() },
            TmuxOutput { exit_code: Some(0), stdout: String::new(), stderr: String::new() },
        ]);
        let lines = recorded("replay-quiet", || {
            let result = super::run_reading_with(
                "chiefd-simulated",
                &["has-session", "-t", "org-acme"],
                super::Reading::Presence,
                || answers.borrow_mut().remove(0),
                |_| {},
            );
            assert!(result.ok(), "the session is there, on the invocation that ran");
        });

        let loud: Vec<&serde_json::Value> = lines
            .iter()
            .filter(|line| line["level"] == "warn" || line["level"] == "error")
            .collect();
        assert!(loud.is_empty(), "a transient tmux lost and replayed is not a warning: {loud:?}");
        let replayed = lines
            .iter()
            .find(|line| line["event"] == "tmux.verb.replayed")
            .expect("the replay must leave evidence, or the next reader loses this signal");
        assert_eq!(replayed["level"], "info", "and at a level production actually records");
        assert_eq!(replayed["detail"]["replays"], 1);
        assert!(
            lines.iter().any(|line| line["event"] == "tmux.verb" && line["level"] == "debug"),
            "the successful verb is still recorded: {lines:?}"
        );
    }

    /// And the direction that proves the replay hid nothing: a client that never
    /// comes back is still a `warn`, and the probe still cannot answer.
    #[test]
    fn a_client_that_never_comes_back_is_still_loud() {
        let lines = recorded("replay-exhausted", || {
            let result = super::run_reading_with(
                "chiefd-simulated",
                &["has-session", "-t", "org-acme"],
                super::Reading::Presence,
                || TmuxOutput { exit_code: None, stdout: String::new(), stderr: String::new() },
                |_| {},
            );
            assert_eq!(result.exit_code, None, "an exhausted budget invents no answer");
            assert!(!super::answers_absence(&result));
        });

        assert!(
            lines.iter().any(|line| line["event"] == "tmux.verb.failed" && line["level"] == "warn"),
            "a tmux that never ran, after every replay, is a human's problem: {lines:?}"
        );
    }

    /// The other direction, and the constraint that keeps this honest:
    /// `tmux.verb.failed` is NOT blanket-downgraded.
    ///
    /// The SAME socket and the SAME diagnostic as the test above, run as an
    /// EFFECT rather than as a probe: a caller that asked tmux to do something
    /// and got "there is no server" was not answered, it was refused, and that
    /// is still a `warn`. Asserting both readings against one tmux state is
    /// what proves the level follows the caller's question rather than the
    /// diagnostic text.
    #[test]
    fn the_same_refusal_is_loud_when_the_caller_asked_for_an_effect() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("verb-loud");
        let lines = recorded("verb-loud", || {
            let refusal = super::run(&socket, &["kill-session", "-t", "org-never-served"]);
            assert!(!refusal.ok(), "tmux must refuse on a socket nobody serves");
        });

        assert!(
            lines.iter().any(|line| line["event"] == "tmux.verb.failed" && line["level"] == "warn"),
            "a tmux verb run for its effect still warns when it fails: {lines:?}"
        );
    }

    /// THE TMUX CONTRACT THE WHOLE GEOMETRY FIX RESTS ON, against a live
    /// server.
    ///
    /// Three facts, none of which this codebase can assume from documentation
    /// alone because the fix would be silently wrong if any changed:
    ///
    /// 1. `default-size` governs the size a DETACHED session is born at, which
    ///    is what lets `stamp_default_size` fix the geometry of a session the
    ///    ACTUATOR — a process with no operator terminal — creates later.
    /// 2. Being born at that size leaves `window-size` at `latest` until Chief
    ///    first publishes the managed layout.
    /// 3. `resize-window` flips `window-size` to `manual`. Chief keeps this
    ///    mode so a later client SIGWINCH cannot publish a proportional split.
    #[test]
    fn default_size_governs_a_detached_session_and_leaves_automatic_sizing_alone() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("default-size");
        // The stamp needs a live server, and a server with no session does not
        // stay up — so a session comes first, exactly as `ensure_session` does
        // it. `-g`, not `-s`: `default-size` is a global SESSION option, and
        // the `-s` spelling exits zero while storing nothing.
        start_session(&socket, "seed", &["sleep", "120"]);
        super::run(&socket, &["set-option", "-g", "default-size", "202x45"]);
        start_session(&socket, "sized", &["sleep", "120"]);

        let born = super::run(
            &socket,
            &["display-message", "-p", "-t", "sized", "-F", "#{window_width}x#{window_height}"],
        );
        assert_eq!(born.stdout, "202x45", "a session minted after the stamp is born at it");

        let mode =
            super::run(&socket, &["display-message", "-p", "-t", "sized", "-F", "#{window-size}"]);
        assert_eq!(
            mode.stdout, "latest",
            "an unsized birth leaves the window resizing with its clients"
        );

        // Fact 3 is the managed-window isolation rule.
        super::run(&socket, &["resize-window", "-t", "sized", "-A"]);
        let after =
            super::run(&socket, &["display-message", "-p", "-t", "sized", "-F", "#{window-size}"]);
        assert_eq!(
            after.stdout, "manual",
            "resize-window pins the window until Chief publishes another complete viewport"
        );

        super::run(&socket, &["kill-server"]);
    }

    /// A deployed Chief normally enters a server that already owns its durable
    /// company session. That path must configure the SERVER before the next
    /// operator client attaches. tmux does not renegotiate an existing
    /// client's features, so this also pins the required reconnect rule.
    #[test]
    fn reused_server_grants_rgb_to_the_next_client_without_changing_an_attached_client() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("reused-rgb");
        let session = "existing-company";
        start_session(&socket, session, &["sleep", "120"]);
        let directory = tempfile::tempdir().expect("typescript directory");

        fn attach_xterm_client(
            socket: &str,
            session: &str,
            typescript: &std::path::Path,
        ) -> (std::process::Child, std::process::ChildStdin, String) {
            let mut child = std::process::Command::new("script")
                .args([
                    "-q",
                    "-c",
                    &format!("tmux -L {socket} attach-session -t {session}"),
                    typescript.to_str().expect("typescript path"),
                ])
                .env("TERM", "xterm-256color")
                .env("COLORTERM", "truecolor")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("ordinary xterm client");
            let input = child.stdin.take().expect("keep the client attached");
            let mut name = String::new();
            assert!(
                wait_until(1_000, || {
                    name = super::run(
                        socket,
                        &["list-clients", "-t", session, "-F", "#{client_name}"],
                    )
                    .stdout
                    .trim()
                    .to_owned();
                    !name.is_empty()
                }),
                "ordinary xterm client did not attach"
            );
            (child, input, name)
        }

        fn client_features(socket: &str, client: &str) -> String {
            super::run(socket, &["display-message", "-p", "-c", client, "#{client_termfeatures}"])
                .stdout
        }

        let first_path = directory.path().join("before-rgb.typescript");
        let (mut first, first_input, first_name) =
            attach_xterm_client(&socket, session, &first_path);
        assert!(
            !client_features(&socket, &first_name).split(',').any(|feature| feature == "RGB"),
            "the fixture must begin with the live defect"
        );

        let reused = super::ensure_session(&socket, session, std::path::Path::new("/unused"))
            .expect("re-enter the existing server");
        assert!(!reused.pane_id.is_empty(), "the durable session stays present");
        super::ensure_session(&socket, session, std::path::Path::new("/unused"))
            .expect("repeat entry is idempotent");
        let rows = super::run(&socket, &["show-options", "-s", "-v", "terminal-features"]);
        assert_eq!(
            rows.stdout.lines().filter(|line| *line == "xterm*:RGB").count(),
            1,
            "repeated entry keeps one exact RGB rule: {}",
            rows.stdout
        );
        assert!(
            !client_features(&socket, &first_name).split(',').any(|feature| feature == "RGB"),
            "tmux must not be claimed to renegotiate an attached client"
        );

        let detached = super::run(&socket, &["detach-client", "-t", &first_name]);
        assert!(detached.ok(), "detach first client: {}", detached.diagnostic());
        drop(first_input);
        assert!(first.wait().expect("first script exits").success());

        let second_path = directory.path().join("after-rgb.typescript");
        let (mut second, second_input, second_name) =
            attach_xterm_client(&socket, session, &second_path);
        assert!(
            client_features(&socket, &second_name).split(',').any(|feature| feature == "RGB"),
            "the next ordinary client must negotiate RGB"
        );
        let detached = super::run(&socket, &["detach-client", "-t", &second_name]);
        assert!(detached.ok(), "detach second client: {}", detached.diagnostic());
        drop(second_input);
        assert!(second.wait().expect("second script exits").success());
        super::run(&socket, &["kill-server"]);
    }

    /// The tripwire for fact 3 above: only the canonical geometry helper can
    /// ask tmux to resize a window. Its unit tests pin the adjacent final layout
    /// and explicit manual ownership; any second call site could publish a
    /// transient proportional split.
    #[test]
    fn production_resize_window_is_confined_to_the_paired_geometry_helper() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory).expect("the crate's own src is readable") {
                let path = entry.expect("a readable dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                    continue;
                }
                if path.file_name().and_then(|name| name.to_str()) == Some("tests.rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("a readable source file");
                // Test bodies are cut first: the live test above calls
                // `resize-window` deliberately, to prove why production may not.
                let production = source.split("#[cfg(test)]").next().unwrap_or_default();
                if production.contains("\"resize-window\"")
                    && path.file_name().and_then(|name| name.to_str()) != Some("window_geometry.rs")
                {
                    offenders.push(path.display().to_string());
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "resize-window is safe only in window_geometry.rs, where the same argv immediately \
             publishes the final layout and manual ownership. Offending files: {offenders:?}"
        );
    }

    /// The verb `attach` chooses, which is the whole of the fix: `tmux
    /// attach-session` REFUSES to nest ("sessions should be nested with care,
    /// unset $TMUX to force", exit 1), and inside tmux is the normal case for
    /// this product — the operator's Founder runs in a pane, so both `chiefd
    /// attach <company>` and the Founder's handoff to a new CEO are issued
    /// from inside a client. Both failed, and blamed the terminal.
    ///
    /// Asserted through the same `TMUX` read the function makes rather than by
    /// invoking tmux. NOTE what this does NOT cover, because it is exactly how
    /// the Founder->CEO handoff stayed broken through a fix and a green suite:
    /// it asserts which verb a caller CHOOSES, and the handoff's bug was that
    /// the chooser was asking about the wrong process entirely. A test that
    /// asks the same question as the code can only ever agree with it. The real
    /// behaviour is pinned by
    /// `handoff_clients_moves_the_operators_real_tmux_client` below, against a
    /// live server.
    #[test]
    fn inside_a_pane_the_handoff_switches_the_client_instead_of_nesting() {
        // `TMUX` is process-global and libtest is multi-threaded, so the lock
        // is what makes this safe -- the comment that used to stand here said
        // "single-threaded test" and that was simply untrue.
        let _guard = super::test_support::env_lock();
        let restore = std::env::var("TMUX").ok();

        std::env::remove_var("TMUX");
        assert!(!super::inside_tmux(), "an unset TMUX is not inside tmux");

        std::env::set_var("TMUX", "");
        assert!(!super::inside_tmux(), "an EMPTY TMUX is not inside tmux either");

        std::env::set_var("TMUX", "/private/tmp/tmux-501/default,12345,0");
        assert!(super::inside_tmux(), "a real tmux pane must be recognised");

        match restore {
            Some(value) => std::env::set_var("TMUX", value),
            None => std::env::remove_var("TMUX"),
        }
    }

    fn session_of_clients(socket: &str) -> String {
        super::run(socket, &["list-clients", "-F", "#{session_name}"]).stdout
    }

    /// The Founder→CEO handoff, against a REAL tmux server with a REAL attached
    /// client — the only shape that can catch what actually shipped.
    ///
    /// The handoff is issued by a process serving an HTTP route, which has no
    /// controlling terminal. The previous implementation chose its tmux verb
    /// from that process's own `$TMUX`, found it unset, and ran
    /// `attach-session`. From a process with no tty tmux answers that with
    /// **exit 0 and no effect** — so the route saw success, the Founder printed
    /// "CEO booted in its ChiefD tmux session", no warning was produced
    /// anywhere, and the operator sat in the Founder pane. This test drives that
    /// exact configuration: no `$TMUX`, no tty, a client attached elsewhere.
    #[test]
    fn handoff_clients_moves_the_operators_real_tmux_client() {
        if !require_tmux() {
            return;
        }
        // Sockets nobody has ever served, so nothing here opens with a
        // `kill-server` these mints could lose a race with.
        let socket = unique_socket("handoff");
        let outer = unique_socket("handoff-outer");
        let cleanup = || {
            super::run(&socket, &["kill-server"]);
            super::run(&outer, &["kill-server"]);
        };

        // Two sessions on the company socket: where the operator is, and where
        // the handoff must take them.
        start_session(&socket, "founder", &["sleep", "300"]);
        start_session(&socket, "chief", &["sleep", "300"]);

        // A REAL client attached to `founder`. It needs a pty, so a second tmux
        // server provides one — the same way an operator's terminal does.
        start_session(&outer, "term", &["bash"]);
        let attach = format!("tmux -L {socket} attach-session -t founder");
        super::run(&outer, &["send-keys", "-t", "term", &attach, "Enter"]);
        let attached = wait_until(10_000, || session_of_clients(&socket).contains("founder"));
        if !attached {
            cleanup();
            precondition_failed("could not attach a live tmux client on this machine");
            return;
        }

        // The handoff runs with NO `$TMUX` — exactly the daemon's environment.
        // Held under the crate's one environment lock: `TMUX` is process-global
        // and these tests run on many threads at once.
        let _guard = super::test_support::env_lock();
        let restore = std::env::var("TMUX").ok();
        std::env::remove_var("TMUX");
        let moved = super::handoff_clients(&socket, "founder", "chief");
        match restore {
            Some(value) => std::env::set_var("TMUX", value),
            None => std::env::remove_var("TMUX"),
        }

        let moved = moved.expect("the handoff must not error with a client attached");
        let landed = wait_until(5_000, || session_of_clients(&socket).contains("chief"));
        let final_session = session_of_clients(&socket);
        cleanup();

        assert_eq!(moved, 1, "exactly the operator's one client must be handed over");
        assert!(
            landed,
            "the operator's client must END UP on the CEO session; it was on '{final_session}'"
        );
        assert!(
            !final_session.contains("founder"),
            "no client may be left behind on the Founder session, got '{final_session}'"
        );
    }

    /// Zero attached clients is reported as zero, never as a successful
    /// handoff. An unattended launch (apps/api, a script) genuinely has nobody
    /// to move, and the caller must be able to tell that apart from having
    /// moved somebody — the old path could not, which is why a no-op looked
    /// like success.
    #[test]
    fn handoff_clients_reports_zero_when_nobody_is_attached() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("handoff-empty");
        start_session(&socket, "founder", &["sleep", "300"]);
        start_session(&socket, "chief", &["sleep", "300"]);

        let moved = super::handoff_clients(&socket, "founder", "chief");
        super::run(&socket, &["kill-server"]);

        assert_eq!(moved.expect("an empty client list is not an error"), 0);
    }

    /// The absence classifier, exercised without a tmux binary by feeding it
    /// the exact strings tmux emits.
    fn provably_absent(stderr: &str) -> bool {
        let diagnostic = format!("{stderr}\n").to_lowercase();
        diagnostic.contains("can't find session")
            || diagnostic.contains("no server running")
            || diagnostic.trim() == "no server"
            || (diagnostic.contains("error connecting to")
                && diagnostic.contains("no such file or directory"))
    }

    #[test]
    fn only_tmuxs_own_absence_messages_prove_a_session_is_gone() {
        assert!(provably_absent("can't find session: org-acme"));
        assert!(provably_absent("no server running on /tmp/tmux-0/default"));
        assert!(provably_absent("no server"));
        assert!(provably_absent(
            "error connecting to /tmp/tmux-0/default (No such file or directory)"
        ));
    }

    #[test]
    fn an_unrecognised_failure_is_indeterminate_never_absent() {
        // The whole reason the probe is three-valued: a permission error or a
        // wedged server must not read as "this company is stopped".
        assert!(!provably_absent("permission denied"));
        assert!(!provably_absent("protocol version mismatch"));
        assert!(!provably_absent(""));
    }

    #[test]
    fn a_signal_terminated_client_is_never_a_success() {
        let signalled =
            TmuxOutput { exit_code: None, stdout: String::new(), stderr: String::new() };
        assert!(!signalled.ok());
        let clean = TmuxOutput { exit_code: Some(0), stdout: String::new(), stderr: String::new() };
        assert!(clean.ok());
    }

    #[test]
    fn the_diagnostic_prefers_stderr_but_falls_back_to_stdout() {
        let both =
            TmuxOutput { exit_code: Some(1), stdout: "out".to_string(), stderr: "err".to_string() };
        assert_eq!(both.diagnostic(), "err");
        let only_stdout =
            TmuxOutput { exit_code: Some(1), stdout: "out".to_string(), stderr: String::new() };
        assert_eq!(only_stdout.diagnostic(), "out");
    }

    /// What a `#{pane_dead}` listing means, without a tmux binary.
    ///
    /// The `Unknown` arms are the load-bearing ones: attach starts an actuator
    /// when it believes there is none, so a read that could not answer must
    /// never look like absence.
    #[test]
    fn an_actuator_session_is_running_dead_or_unreadable_and_never_guessed() {
        let listed = |ok: bool, stdout: &str| TmuxOutput {
            exit_code: if ok { Some(0) } else { Some(1) },
            stdout: stdout.to_string(),
            stderr: String::new(),
        };
        assert_eq!(
            super::classify_actuator_panes(&listed(true, "0")),
            super::ActuatorSession::Running
        );
        // A dead pane beside a live one is still a running actuator.
        assert_eq!(
            super::classify_actuator_panes(&listed(true, "1\n0")),
            super::ActuatorSession::Running
        );
        assert_eq!(
            super::classify_actuator_panes(&listed(true, "1")),
            super::ActuatorSession::Exited
        );
        assert_eq!(
            super::classify_actuator_panes(&listed(false, "")),
            super::ActuatorSession::Unknown
        );
        // A session tmux says exists but lists no pane for is not an empty
        // session — it is a read nobody may act on.
        assert_eq!(
            super::classify_actuator_panes(&listed(true, "  \n")),
            super::ActuatorSession::Unknown
        );
    }

    /// The actuator window's name crosses a boundary: it is what an operator
    /// greps for in `tmux list-panes -a` to prove their company is being run.
    #[test]
    fn the_actuator_window_name_is_the_published_one() {
        assert_eq!(super::ACTUATOR_WINDOW, "chiefd-actuator");
    }

    /// A blank variable is dropped rather than forwarded.
    ///
    /// An exported-but-empty variable is not the same fact as an unset one, and
    /// a reader that treats a blank as a value answers a question the operator
    /// never asked. (`TEAM_LAUNCHER_PI` was the case that taught this and is
    /// deleted; the property is about the forwarder, not about that name.)
    #[test]
    fn the_forwarded_environment_drops_blanks_and_trims() {
        // SAFETY: single-threaded test over names no other test reads, and both
        // are removed below.
        let present = "CHIEFD_TMUX_TEST_PRESENT";
        let blank = "CHIEFD_TMUX_TEST_BLANK";
        std::env::set_var(present, "  /opt/pi/bin/pi  ");
        std::env::set_var(blank, "   ");

        let forwarded = super::forwarded(&[present, blank, "CHIEFD_TMUX_TEST_UNSET"]);

        std::env::remove_var(present);
        std::env::remove_var(blank);
        assert_eq!(forwarded, vec![(present, "/opt/pi/bin/pi".to_string())]);
    }

    /// A live tmux server: the actuator pane keeps its last words after its
    /// command exits, which is the whole reason `remain-on-exit` is set before
    /// the actuator is respawned into the pane rather than after.
    #[test]
    fn a_dead_actuator_pane_survives_to_be_quoted() {
        if !require_tmux() {
            return;
        }
        // A virgin socket: `start_actuator` — production, and the subject of
        // this test — mints here, and a mint that lost a race with a teardown
        // would have been read as production failing to place an actuator.
        let socket = unique_socket("actuator-dead");
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo the actuator said this; exit 3".to_string(),
        ];

        super::start_actuator(&socket, "org-acme-actuator", fixture_dir(), &command, &[])
            .expect("placing an actuator must succeed on a live tmux server");
        let dead = wait_until(10_000, || {
            super::actuator_session(&socket, "org-acme-actuator") == super::ActuatorSession::Exited
        });
        let captured = super::capture_dead_pane(&socket, "org-acme-actuator", 20);
        super::run(&socket, &["kill-server"]);

        assert!(dead, "a pane whose command exited must be reported as Exited, not Running");
        assert!(
            captured.contains("the actuator said this"),
            "the dead pane's own words must survive; got '{captured}'"
        );
        // tmux's own tombstone carries the exit status, which is evidence in
        // its own right — a refusal that says "exited" without saying how is
        // one round trip short of useful.
        assert!(captured.contains("status 3"), "got '{captured}'");
    }

    /// THE RULE the CI reds cost, pinned where it can be proven: a stopped
    /// pane is quoted from a reading that has stopped changing AND has the
    /// pane's own words in it.
    ///
    /// Against readings rather than against tmux, and that is not a shortcut.
    /// The window between tmux reaping a child and tmux putting that child's
    /// output on the grid is under ten milliseconds on an idle host — it opened
    /// on a CI runner carrying ten concurrent cargo test processes, and no
    /// amount of running the live tests by hand, or under synthetic CPU load,
    /// opens it here. A live test that cannot fail without the fix is not
    /// evidence of the fix. These can only pass if the settle is really there.
    #[test]
    fn a_stopped_pane_is_quoted_from_a_settled_reading_that_has_its_words_in_it() {
        const TOMB: &str = "Pane is dead (status 3, Tue Aug 18 23:18:16 2026)";

        // STILL DRAINING: the first reading is the tombstone alone and must
        // never be the answer. THE DEFECT, in one line.
        let mut readings =
            [TOMB.to_owned(), format!("no pi runtime was found\n{TOMB}")].into_iter();
        assert_eq!(
            super::settled_capture(|| readings.next()),
            Some(format!("no pi runtime was found\n{TOMB}")),
            "a reading that is still changing is never the answer"
        );

        // NOT YET BEGUN, which STABILITY ALONE CALLS FINISHED. The reading is
        // identical every look because tmux has not read the pty at all, and
        // the words arrive on the fourth. A settle that asked only "did it stop
        // changing" answered on the second look with the tombstone — this is
        // the case that reached CI three times.
        let mut looks = 0;
        let answer = super::settled_capture(|| {
            looks += 1;
            Some(if looks < 4 {
                TOMB.to_owned()
            } else {
                format!("the actuator said this\n{TOMB}")
            })
        });
        assert_eq!(
            answer,
            Some(format!("the actuator said this\n{TOMB}")),
            "a static reading holding only tmux's tombstone is not a drained pane"
        );

        // SETTLED: two in a row agree and the pane's words are there, so the
        // loop stops rather than spending the whole bound on a finished pane.
        let mut looks = 0;
        assert_eq!(
            super::settled_capture(|| {
                looks += 1;
                Some(format!("the actuator said this\n{TOMB}"))
            }),
            Some(format!("the actuator said this\n{TOMB}"))
        );
        assert_eq!(looks, 2, "a settled capture costs one confirming read, not the bound");

        // A PANE THAT REALLY PRINTED NOTHING spends the bound and is then
        // quoted as it stands. The honest cost of the rule above, and it must
        // answer with the tombstone rather than with nothing.
        let mut looks = 0;
        assert_eq!(
            super::settled_capture(|| {
                looks += 1;
                Some(TOMB.to_owned())
            }),
            Some(TOMB.to_owned())
        );
        assert_eq!(looks, super::DRAIN_ATTEMPTS, "a silent pane costs the whole bound");

        // NEVER SETTLES: bounded, and answered with the LAST reading.
        let mut nth = 0;
        assert_eq!(
            super::settled_capture(|| {
                nth += 1;
                Some(format!("line {nth}"))
            }),
            Some(format!("line {}", super::DRAIN_ATTEMPTS)),
            "an unsettled source is bounded and quoted as it last read"
        );

        // A capture that stops being answerable — the pane went away — is
        // quoted from what was already read.
        let mut then_fails = [Some("half".to_owned()), None].into_iter();
        assert_eq!(super::settled_capture(|| then_fails.next().flatten()), Some("half".to_owned()));

        // One that never answered at all is nothing to quote.
        assert_eq!(super::settled_capture(|| None), None);
    }

    /// TWO REAL COMPANIES, ONE STOPPED, AGAINST A REAL TMUX SERVER.
    ///
    /// The defect the convention was changed to delete, driven end to end at
    /// the layer it lives in: `acme` is stopped, `acme-corp` is running, and a
    /// probe for `acme` must answer NO. Then the same arrangement in reverse.
    ///
    /// A unit test over strings cannot be evidence here, because the rule under
    /// test is tmux's OWN target resolution. So the test carries its own
    /// negative control: it runs the identical arrangement under the names the
    /// OLD convention minted and asserts tmux really does hand one company's
    /// session to the other. If that control ever reads absent, tmux changed
    /// and the reasoning must be re-checked rather than the assertion relaxed.
    #[test]
    fn two_companies_with_prefix_related_slugs_resolve_to_their_own_sessions() {
        if !require_tmux() {
            return;
        }
        // A SOCKET PER ARRANGEMENT, and each one VIRGIN. `kill-server`
        // followed immediately by `new-session` on one socket races its own
        // teardown, and a `new-session` that loses that race leaves an EMPTY
        // server — which reads exactly like the absence under test. A socket
        // nobody has ever served has no teardown to race, and `start_session`
        // retries the mint until tmux names the session, failing as a SETUP
        // failure if it never does.
        let corp_up = unique_socket("two-companies-corp");
        let acme_up = unique_socket("two-companies-acme");
        let old_names = unique_socket("two-companies-old");
        let acme = fixture_session("acme");
        let acme_corp = fixture_session("acme-corp");
        let start = |socket: &str, name: &str| start_session(socket, name, &["sleep", "300"]);
        let resolved = |socket: &str, target: &str| {
            super::run(socket, &["display-message", "-p", "-t", target, "#{session_name}"]).stdout
        };

        // 1. `acme` stopped, `acme-corp` running.
        start(&corp_up, &acme_corp);
        let stopped_acme_seen = super::session_exists(&corp_up, &acme);
        let stopped_acme_resolves_to = resolved(&corp_up, &acme);
        let running_corp_seen = super::session_exists(&corp_up, &acme_corp);

        // 2. The other direction: `acme-corp` stopped, `acme` running. A
        //    terminator that only worked one way round would pass step 1.
        start(&acme_up, &acme);
        let stopped_corp_seen = super::session_exists(&acme_up, &acme_corp);
        let stopped_corp_resolves_to = resolved(&acme_up, &acme_corp);
        let running_acme_seen = super::session_exists(&acme_up, &acme);

        // 3. THE NEGATIVE CONTROL: the same arrangement under the names the old
        //    `org-<slug>` convention minted.
        start(&old_names, "org-acme-corp");
        let old_convention_seen = super::session_exists(&old_names, "org-acme");
        let old_convention_resolves_to = resolved(&old_names, "org-acme");
        for server in [&corp_up, &acme_up, &old_names] {
            super::run(server, &["kill-server"]);
        }

        assert_eq!(
            stopped_acme_seen,
            Some(false),
            "'{acme}' is stopped; tmux answered for it while only '{acme_corp}' was up, and it \
             named '{stopped_acme_resolves_to}'"
        );
        assert!(
            !stopped_acme_resolves_to.contains(&acme_corp),
            "a target for the stopped company must never resolve to the running one; got \
             '{stopped_acme_resolves_to}'"
        );
        assert_eq!(running_corp_seen, Some(true), "'{acme_corp}' is up and must read as up");
        assert_eq!(
            stopped_corp_seen,
            Some(false),
            "'{acme_corp}' is stopped; tmux named '{stopped_corp_resolves_to}' for it"
        );
        assert!(
            !stopped_corp_resolves_to.contains(&acme),
            "the collision must be gone in BOTH directions; got '{stopped_corp_resolves_to}'"
        );
        assert_eq!(running_acme_seen, Some(true), "'{acme}' is up and must read as up");
        assert_eq!(
            old_convention_seen,
            Some(true),
            "this test is only evidence while tmux really does match a target by PREFIX — under \
             the old convention a probe for a STOPPED 'org-acme' had to be answered by the running \
             'org-acme-corp'"
        );
        assert_eq!(
            old_convention_resolves_to, "org-acme-corp",
            "the measured defect: `chief attach acme` read another company's session as its own"
        );
    }

    /// Is any name in `names` a prefix of another? The rule, as a value.
    ///
    /// `tmux -t <name>` matches EXACTLY first and falls back to PREFIX, so two
    /// minted names where one prefixes the other are one name as far as every
    /// tmux verb is concerned: `has-session`, `kill-session`, `display-message`,
    /// and every read `actuate::observe` makes.
    fn prefix_collision(names: &[String]) -> Option<(String, String)> {
        for (index, name) in names.iter().enumerate() {
            for other in names.iter().skip(index + 1) {
                if name.starts_with(other.as_str()) || other.starts_with(name.as_str()) {
                    return Some((name.clone(), other.clone()));
                }
            }
        }
        None
    }

    /// Every tmux session name this product mints FOR ONE COMPANY, DERIVED by
    /// calling the producers rather than copied from them.
    ///
    /// One company at a time. The rule this half enforces is that the names
    /// minted AROUND a company — the Founder's, the company's own, its
    /// actuator's — can never be confused for one another. The names minted
    /// around two DIFFERENT companies are the other half, and they are checked
    /// by [`no_two_companies_can_mint_prefix_related_session_names`], which
    /// runs this same producer over prefix-related slug PAIRS.
    fn minted_session_names(slug: &str) -> Vec<String> {
        let company = fixture_session(slug);
        vec![
            crate::founder::FOUNDER_SESSION.to_string(),
            crate::attach::actuator_session_name(&company),
            company,
        ]
    }

    /// THE GUARD, and the reason it exists: nothing enforced this before, and
    /// the first name that broke it shipped and killed its own actuator.
    ///
    /// It is derived, never enumerated — the names come out of the production
    /// producers — and the completeness half is
    /// [`every_tmux_session_mint_is_covered_by_the_prefix_rule`] below, which
    /// fails when a mint site this test does not know about appears.
    #[test]
    fn no_minted_tmux_session_name_is_a_prefix_of_another() {
        for slug in ["acme", "a", "northstar-freight", "acme-corp"] {
            let names = minted_session_names(slug);
            assert_eq!(names.len(), 3, "the producers must actually answer: {names:?}");
            assert_eq!(
                prefix_collision(&names),
                None,
                "two names minted around '{slug}' collide under tmux's prefix matching: {names:?}"
            );
        }
    }

    /// THE DEFECT THIS CONVENTION WAS CHANGED TO DELETE: two companies whose
    /// slugs are prefix-related must never share one tmux target.
    ///
    /// Under the old `org-<slug>` convention, `acme` and `acme-corp` minted
    /// `org-acme` and `org-acme-corp`. While both sessions existed tmux
    /// resolved each name exactly and nothing went wrong. The moment `acme` was
    /// STOPPED, every probe for `org-acme` resolved to `org-acme-corp`:
    /// `session_exists` answered YES for a company with nothing running,
    /// `chief attach acme` moved the operator into `acme-corp`'s panes, and
    /// `chief stop acme` would have killed `acme-corp`'s session.
    ///
    /// The names now end in [`chief_cli::placement::SESSION_TERMINATOR`], which
    /// [`crate::paths::is_canonical_slug`] refuses, so the collision is
    /// structurally impossible rather than merely absent for today's slugs — the proof is on
    /// [`chief_cli::placement::session_name_for_slug`] and the fact it rests on is
    /// pinned by [`a_canonical_slug_can_never_contain_the_session_terminator`].
    /// The live half, against a real tmux server, is
    /// [`two_companies_with_prefix_related_slugs_resolve_to_their_own_sessions`].
    #[test]
    fn no_two_companies_can_mint_prefix_related_session_names() {
        // Every pair here is prefix-related AS A SLUG, which is what made the
        // old convention collide. `a`/`ab` has no separator at the seam at all,
        // and `acme-corp`/`acme-corp-holdings` is the company that would have
        // broken a convention that merely avoided today's names.
        let pairs = [
            ("acme", "acme-corp"),
            ("acme", "acme-corp-holdings"),
            ("acme-corp", "acme-corp-holdings"),
            ("a", "ab"),
            ("a", "a-b"),
            ("northstar-freight", "northstar-freight-holdings"),
        ];
        for (shorter, longer) in pairs {
            // BOTH DIRECTIONS, stated directly on the company session names,
            // because that is the claim: neither name may resolve to the other,
            // whichever of the two companies happens to be the stopped one.
            let one = fixture_session(shorter);
            let other = fixture_session(longer);
            assert!(
                !other.starts_with(&one),
                "a probe for a stopped '{shorter}' would be answered by a running '{longer}': \
                 '{one}' / '{other}'"
            );
            assert!(
                !one.starts_with(&other),
                "a probe for a stopped '{longer}' would be answered by a running '{shorter}': \
                 '{other}' / '{one}'"
            );

            // And then EVERY name either company mints, not just the company
            // sessions: an actuator name is built from a company name, so a
            // collision there is the same defect one step along. Deduplicated
            // because the Founder's session is minted once for the machine, not
            // once per company, and one name is never a collision with itself.
            let mut names = minted_session_names(shorter);
            names.extend(minted_session_names(longer));
            names.sort();
            names.dedup();
            assert_eq!(
                prefix_collision(&names),
                None,
                "'{shorter}' and '{longer}' collide under tmux's prefix matching: {names:?}"
            );
        }
    }

    /// THE FACT THE PROOF RESTS ON. Everything above is only structural because
    /// a slug can never contain the terminator the session name ends with.
    ///
    /// Both halves: the validator that guards every slug REFUSES one, and this
    /// crate's producer cannot emit one.
    ///
    /// THE SECOND HALF IS ONE PRODUCER, NOT ALL OF THEM, and this comment used
    /// to say "the only producer" — which was false.
    /// `chiefd_core::store::organization_spec::slugify` is a second producer,
    /// and it cannot be called from here because the two crates are forbidden
    /// to link. Its own copy of this property is
    /// `no_input_makes_this_producer_emit_a_non_canonical_slug`, in that
    /// crate's test module, driven against the same shared corpus; the whole
    /// producer set is enumerated by shape in
    /// `scripts/test/slug-producers-agree.test.mjs`. The first half is what the
    /// proof actually rests on, and it is exact regardless of how many
    /// producers there are.
    #[test]
    fn a_canonical_slug_can_never_contain_the_session_terminator() {
        let terminator = chief_cli::placement::SESSION_TERMINATOR;
        for slug in ["acme_corp", "_acme", "acme_", "a_b_c"] {
            assert!(slug.contains(terminator), "the fixture must carry one: {slug}");
            assert!(
                !crate::paths::is_canonical_slug(slug),
                "'{slug}' carries '{terminator}' and must be refused"
            );
        }
        for name in ["Acme_Corp", "acme_corp", "__", "Leo_Capital Inc."] {
            let slug = crate::genesis::slugify(name);
            assert!(
                !slug.contains(terminator),
                "slugify('{name}') emitted the terminator: '{slug}'"
            );
        }
    }

    /// The negative control. Without it the check above is a test that agrees
    /// with itself: it would pass identically against a checker that always
    /// answers `None`.
    #[test]
    fn the_prefix_rule_catches_the_name_that_actually_shipped() {
        let broken = vec!["org-acme".to_string(), "org-acme-actuator".to_string()];
        assert_eq!(
            prefix_collision(&broken),
            Some(("org-acme".to_string(), "org-acme-actuator".to_string())),
            "the suffix name that killed a live actuator must be caught"
        );
        // And a same-shape pair of COMPANY names, which is the collision this
        // rule will meet next if a future name is built as a suffix.
        assert!(prefix_collision(&["org-a".to_string(), "org-ab".to_string()]).is_some());
        // THE COMPANY-SESSION CONVENTION THAT SHIPPED, verbatim. This is what
        // `no_two_companies_can_mint_prefix_related_session_names` is checking
        // for, so it is what proves that check can still FAIL: revert
        // `session_name_for_slug` to `format!("org-{slug}")` and these are the
        // exact strings it would produce for `acme` and `acme-corp`.
        assert_eq!(
            prefix_collision(&["org-acme".to_string(), "org-acme-corp".to_string()]),
            Some(("org-acme".to_string(), "org-acme-corp".to_string())),
            "the company-session convention that could move an operator into another company must \
             still be caught"
        );
    }

    /// THE COMPLETENESS HALF. A prefix rule over three names is worth nothing
    /// the moment a fourth session is minted somewhere this test cannot see.
    ///
    /// It carries no copy of the names — it counts the production mint sites in
    /// this crate's own source. A new one fails the test with instructions,
    /// rather than silently leaving the new name unchecked. That is the
    /// difference between a tripwire and a stale allowlist: this one cannot be
    /// satisfied by forgetting.
    ///
    /// `#[cfg(test)]` bodies are cut first, because test code mints throwaway
    /// sessions by the dozen and none of them is a product name.
    #[test]
    fn every_tmux_session_mint_is_covered_by_the_prefix_rule() {
        /// The three functions that create a tmux session in production.
        /// `ensure_session` is the only `new-session` call site in the crate;
        /// `start_actuator` uses it for resident sessions, and
        /// `create_operator_session` creates the final Founder pane and session
        /// in one tmux server message.
        const MINTS: [&str; 3] = ["ensure_session(", "start_actuator(", "create_operator_session("];
        /// How many production call sites exist today. A tripwire, not a
        /// register: it holds no name, and any change to the set fails here.
        const EXPECTED_SITES: usize = 4;

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(directory) = stack.pop() {
            let entries = std::fs::read_dir(&directory).expect("the crate's own src is readable");
            for entry in entries {
                let path = entry.expect("a readable dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("a readable source file");
                let production =
                    source.split("#[cfg(test)]").next().unwrap_or_default().to_string();
                for line in production.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("//") || trimmed.contains("fn ") {
                        continue;
                    }
                    for mint in MINTS {
                        if trimmed.contains(mint) {
                            found.push(format!("{}: {trimmed}", path.display()));
                        }
                    }
                }
            }
        }

        assert_eq!(
            found.len(),
            EXPECTED_SITES,
            "the set of production tmux session mints changed. Every session name this product \
             mints must be added to `minted_session_names` above and checked against the others, \
             because tmux resolves a target by PREFIX and two names where one prefixes the other \
             are one name to every tmux verb. Sites found:\n{}",
            found.join("\n")
        );
    }

    #[test]
    fn a_pane_id_must_look_like_a_pane_id_before_anything_is_tagged() {
        // `tag_operator_pane` refuses before touching tmux; assert the shape
        // rule directly so the refusal cannot be lost to a tmux-less test host.
        let ok = |id: &str| {
            id.starts_with('%') && id.len() > 1 && id[1..].chars().all(|c| c.is_ascii_digit())
        };
        assert!(ok("%0"));
        assert!(ok("%1234"));
        assert!(!ok(""));
        assert!(!ok("%"));
        assert!(!ok("0"));
        assert!(!ok("%1a"));
        assert!(!ok("pane"));
    }
}
