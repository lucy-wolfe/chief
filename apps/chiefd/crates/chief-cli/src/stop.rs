//! `chief stop` — tear this directory's runtime down while preserving every
//! byte of durable state.
//!
//! Ported from the deleted TypeScript `stop.ts`. Non-destructive
//! and unprompted: the SQL store file, goals, mail, memory and assignments are
//! untouched in every branch.
//!
//! # ORDERING LAW
//!
//! The durable teardown commits BEFORE the daemon dies. Runtime teardown
//! records supervisor disarm and clears the runtime projection through the SQL
//! store, which is served by this company's own daemon — stop the daemon first
//! and those writes have nowhere to land. Reversing this order is a real bug,
//! not a style preference, and [`StopOutcome`] exists so a test can assert the
//! sequence rather than the outcome alone.

use std::path::Path;

use super::company::{now_iso_millis, CompanyClient};
use super::daemon;
use super::http::Client;
use super::{tmux, LifecycleError, Result};

/// What a stop actually did.
///
/// TOMBSTONE: `OrphanSessionStopped`. It reported "the daemon was down and a
/// conventional `org-<slug>_` session was torn down anyway", which was only
/// possible while a session name could be composed from a word the CALLER
/// typed. The name is `org-<slug>-<key6>_` now and the slug lives in the store
/// the dead daemon was serving, so the branch has no session to name and does
/// not guess one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopMode {
    /// The happy path: durable teardown through a live daemon, then the daemon.
    Supervised,
    /// Nothing was running.
    AlreadyStopped,
}

/// One stop's result, printed as JSON so a script can read it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StopOutcome {
    /// Which branch ran.
    pub(crate) mode: &'static str,
    /// The company — the directory it occupies, which is its identity.
    pub(crate) dir: String,
    /// The tmux session that was addressed, when one could be named.
    ///
    /// A company whose daemon is already down has no readable slug, so there is
    /// no session name to state and none is invented — `""` would be a claim
    /// about a session, and a guessed one would be the wrong claim.
    pub(crate) session: String,
    /// Whether a session was actually killed.
    pub(crate) session_stopped: bool,
    /// Whether this company's actuator session was actually killed.
    pub(crate) actuator_stopped: bool,
    /// Whether the daemon was asked to exit.
    pub(crate) daemon_stopped: bool,
}

/// The word for a mode, kept beside the enum so the two cannot drift.
fn mode_label(mode: StopMode) -> &'static str {
    match mode {
        StopMode::Supervised => "supervised",
        StopMode::AlreadyStopped => "already-stopped",
    }
}

/// Stop this directory's runtime end to end.
///
/// - A live, identity-proven daemon: read the placement facts — the company's
///   own SLUG among them — commit the durable teardown (launch intent cleared,
///   runtime projection dropped), kill the session, THEN stop the daemon.
/// - No live company runtime: the SQL store and its manifest are not trusted,
///   so the durable teardown is skipped and no endpoint is bound. **No session
///   is killed either**, and that is a change the directory forced: a session
///   is `org-<slug>-<key6>_` and the slug lives in the store this dead daemon
///   was serving, so there is nothing to name. Killing a GUESSED session is the
///   one thing worse than killing none — `org-<key6>_` names some other
///   operator's window as readily as this company's. The daemon stop still
///   runs, unconditionally and idempotently.
///
/// `preserve_daemon` is set only by [`super::reset`], which needs the live
/// listener immediately afterwards: stopping it here and re-spawning moments
/// later tears down a healthy listener and briefly contends a second process on
/// the same directory.
///
/// # Errors
/// [`super::LifecycleError`] when the company routes or tmux refuse.
pub(crate) async fn stop_runtime(
    client: &Client,
    dir: &Path,
    preserve_daemon: bool,
) -> Result<StopOutcome> {
    let running = daemon::resolve_running(client, dir).await;

    if let Some(running) = running {
        // Bind ONLY the URL this one rendezvous reading proved. A later re-read
        // could observe a daemon that replaced it.
        let company =
            CompanyClient::new(client, &running.url, dir, &super::paths::company_key(dir));
        let facts = company.facts().await?.ok_or_else(|| {
            LifecycleError::refused(format!(
                "chief stop: the company in {} has a daemon but no manifest, so it has no name \
                 and no session to tear down",
                dir.display()
            ))
        })?;
        let recorded = company.active_runtime_owner_socket().await?;
        let socket = super::company::boot_socket_from_env(
            recorded.as_deref(),
            &super::paths::company_key(dir),
        );
        let session =
            super::company::conventional_session_name(&facts.slug, &super::paths::company_key(dir));

        // **A STOP IS TOTAL, OR IT SAYS WHAT SURVIVED.**
        //
        // Every stage below used to abort the whole teardown with `?`. That is
        // loud, which is better than silent — but it means a WEDGED DAEMON left
        // every pane, both sessions and every process the panes started still
        // running, because the stop died on a durable write that has nothing to
        // do with tmux. The operator asked for "kill everything related to the
        // company"; a stop that gives up at the first refusal is the opposite,
        // and it is exactly the state that sent them to `pkill -f "chief"`.
        //
        // So each stage is attempted independently, and what could not be done
        // is COLLECTED. The refusal comes at the end, naming survivors, after
        // everything reachable has actually been killed.
        let mut survived: Vec<String> = Vec::new();

        // ORDERING LAW, first half: these two writes must land while the daemon
        // that serves them is still up — so they are tried FIRST, and a failure
        // is recorded rather than fatal. Stale launch intent on a stopped
        // company is a nuisance the next boot clears; panes nobody killed are
        // the thing the operator is asking about.
        let at = now_iso_millis();
        if let Err(error) = company.clear_launch_intent(&at).await {
            survived.push(format!("the launch intent could not be cleared ({error})"));
        }
        if let Err(error) = company.clear_runtime(&at).await {
            survived.push(format!("the runtime rows could not be cleared ({error})"));
        }

        // THE ACTUATOR IS PART OF THE RUNTIME, and it outlives a stop that
        // only names the company session. See `kill_runtime_sessions`.
        let (actuator_stopped, session_stopped) =
            match kill_runtime_sessions(&socket, &session, &mut survived) {
                Ok(stopped) => stopped,
                Err(error) => {
                    survived.push(format!("a tmux session could not be killed ({error})"));
                    (false, false)
                }
            };

        // THE CLAIM IS PART OF THE TEARDOWN, and leaving it standing strands
        // the company. A stop that clears the launch intent, drops the runtime
        // rows and kills the session, but leaves `runtime_owner` reading
        // `status=active, released_at=NULL`, has produced a company that is
        // fully stopped and can never be started from any other socket: the
        // next boot is refused because a live claim names the socket it used to
        // run on. It surfaces first as a 15-second health timeout blaming an
        // "INSTALLED binary older than the contract", so the operator hunts a
        // stale binary while the real cause is the line after it.
        //
        // Best-effort ON PURPOSE. A release that cannot be reached must not
        // fail a stop that has already torn everything else down — the company
        // IS stopped at this point, and refusing here would leave the operator
        // with a half-stopped company and an error, which is strictly worse
        // than a stale claim they can still take over.
        if let Err(error) = company.release_runtime_ownership().await {
            eprintln!(
                "chief stop: the runtime-ownership claim could not be released ({error}); the \
                 company is stopped, but starting it from a different socket will be refused \
                 until the claim is released or taken over"
            );
        }

        // READ BEFORE THE STOP, because a stopped daemon has already removed its
        // rendezvous — and the pid is wanted precisely so a daemon that is
        // STILL there is not reported as a stray by the sweep below.
        let known_pids: Vec<i32> = daemon::read_rendezvous(dir)
            .and_then(|p| i32::try_from(p.pid).ok())
            .into_iter()
            .collect();

        // ORDERING LAW, second half. Attempted even when a stage above failed:
        // a daemon that can still be asked to exit should exit, whatever else
        // went wrong.
        let mut daemon_stopped = !preserve_daemon;
        if !preserve_daemon {
            if let Err(error) = daemon::stop(client, dir).await {
                survived.push(format!("the daemon did not exit ({error})"));
                daemon_stopped = false;
            }
        }
        // THE LAST THING A STOP DOES IS LOOK FOR WHAT IT COULD NOT HAVE REACHED.
        //
        // The reap walks the parent chain from this company's own panes, which
        // catches every child a pane started — a `setsid` one included, because
        // `setsid` changes the session and never the ppid. It cannot catch a
        // DOUBLE FORK: that child is reparented to init immediately, so no
        // chain leads to it even while the panes are alive. And the product's
        // own foreground-bash guidance TELLS agents to detach that way for a
        // persistent deliverable, so this is a shape the company is instructed
        // to produce rather than one anybody has caught happening — enumerated
        // on a live box 2026-08-24 and found empty that hour.
        //
        // So it is DETECTED and named, never killed. A cwd is strong evidence
        // of belonging and it is not authority to signal; the operator decides.
        // The sweep runs HERE, after the sessions and the daemon are down, for
        // two reasons: what the reap already killed is gone by now and cannot
        // be double-reported, and the tmux server — which is itself a
        // legitimately daemonized `ppid == 1` process holding the company's cwd
        // — has been killed rather than needing an exception.
        //
        // The daemon is excluded BY PID rather than by name. It holds the
        // company directory as its cwd on purpose, and `--preserve-daemon`
        // keeps it deliberately, so naming it would be a false alarm on a stop
        // that did exactly what was asked.
        let strays = chief_cli::reap::strays_under(dir, &known_pids);
        if !strays.is_empty() {
            survived.push(format!(
                "{} process(es) started inside {} outlived the stop and were not reached by \
                 any pane's process tree (pids {}); they are named, not killed — a working \
                 directory is evidence of ownership, not authority to signal",
                strays.len(),
                dir.display(),
                strays.iter().map(|stray| stray.pid.to_string()).collect::<Vec<_>>().join(", ")
            ));
        }

        // NEVER A PARTIAL STOP REPORTING SUCCESS. The refusal is last, so that
        // everything reachable has already been killed by the time it is
        // raised — a stop that refuses early leaves more running than one that
        // refuses late.
        if !survived.is_empty() {
            return Err(LifecycleError::refused(format!(
                "chief stop: the company in {} is NOT fully stopped. {}. Everything else was \
                 torn down; run `chief stop` again once the cause is cleared.",
                dir.display(),
                survived.join("; ")
            )));
        }
        return Ok(StopOutcome {
            mode: mode_label(StopMode::Supervised),
            dir: dir.display().to_string(),
            session,
            session_stopped,
            actuator_stopped,
            daemon_stopped,
        });
    }

    // Idempotent — a no-op when the daemon was already fully stopped, and the
    // actual teardown when it was alive-but-unhealthy.
    daemon::stop(client, dir).await?;
    Ok(StopOutcome {
        mode: mode_label(StopMode::AlreadyStopped),
        dir: dir.display().to_string(),
        session: String::new(),
        session_stopped: false,
        actuator_stopped: false,
        daemon_stopped: true,
    })
}

/// Kill both halves of this company's tmux runtime: its ACTUATOR session
/// first, then the company session itself.
///
/// # Why the actuator, and why FIRST
///
/// `chief` starts a resident actuator in `chiefd-actuator-<company-session>`
/// (`attach::actuator_session_name`) whose lifetime is the tmux SERVER's, not
/// the company session's — because re-minting a company session somebody
/// killed is precisely its job. A stop that killed only the company session
/// therefore left the actuator running against a daemon that was about to
/// die, printing
///
/// ```text
/// could not reach http://…/v1/org/runtime/desired … retrying in 8s/16s/30s
/// ```
///
/// forever, on every company that was ever stopped. Nothing reaped it: it is
/// not in the company projection by design, so no converge pass owns it.
///
/// FIRST, because of that same job. Between killing the company session and
/// killing the actuator there is a window in which a live actuator sees a
/// company session missing and mints it back — a stop that returns
/// `sessionStopped: true` while a session by that exact name is on the socket.
/// Taking the actuator down first closes the window: the only process that
/// re-creates the company session is gone before the company session is.
///
/// Returns `(actuator_stopped, session_stopped)`.
///
/// # Errors
/// [`super::LifecycleError`] when tmux refuses for any reason other than a
/// session already being absent.
fn kill_runtime_sessions(
    socket: &str,
    session: &str,
    survived: &mut Vec<String>,
) -> Result<(bool, bool)> {
    let actuator = super::attach::actuator_session_name(session);
    let actuator_stopped = stop_one_session(socket, &actuator, survived)?;
    let session_stopped = stop_one_session(socket, session, survived)?;
    Ok((actuator_stopped, session_stopped))
}

/// Stop one session and everything its panes started, reporting whether this
/// half of the runtime was running at all.
///
/// # Why presence is read FIRST
///
/// THE WORK A PANE STARTED IS PART OF THE RUNTIME TOO, and it is the half a
/// stop used to leave behind. The reap has to run while the panes are still
/// readable — `kill-session` hangs up the pane leader and nothing it forked,
/// and once the pane is gone `list-panes` cannot give its pid, so the kill-first
/// order does not reap late, it cannot reap at all. See [`chief_cli::reap`] for
/// the bound on what may be signalled.
///
/// But a reap that stops a pane leader also ends the last pane of its session,
/// and tmux destroys a session with no panes. `tmux::kill_session` would then
/// truthfully answer "there was nothing to kill", and this stop would report
/// `sessionStopped: false` about a session it had just taken down. So the
/// question "was this half running" is asked once, before anything is
/// signalled, and the kill that follows is the belt to the reap's braces —
/// idempotent, and its own answer is discarded because the reap may already
/// have supplied it.
fn stop_one_session(socket: &str, session: &str, survived: &mut Vec<String>) -> Result<bool> {
    if tmux::session_exists(socket, session) == Some(false) {
        return Ok(false);
    }
    reap_session_processes(socket, session, survived);
    tmux::kill_session(socket, session)?;
    // AND READ THE SESSION BACK. `kill-session` returning cleanly says tmux
    // accepted the command, not that the session is gone — and a stop that
    // reports success over a session still on the socket is the same lie one
    // level up from the process case above. `None` is "tmux would not say",
    // which is not evidence of survival and is not reported as one.
    if tmux::session_exists(socket, session) == Some(true) {
        survived.push(format!("the tmux session '{session}' is still on the socket"));
    }
    Ok(true)
}

/// Stop everything the panes of one session started, and say so when it was
/// not free.
///
/// Best-effort ON PURPOSE, exactly like the ownership release below it: the
/// company IS being stopped, and refusing here would leave the operator with a
/// half-stopped company and an error. A group that had to be `SIGKILL`ed is
/// still worth naming, because it is the operator's only sign that something
/// their company started would not stop when asked.
fn reap_session_processes(socket: &str, session: &str, survived: &mut Vec<String>) {
    let outcome = chief_cli::reap::reap_panes(&tmux::pane_pids(socket, session));
    // WHAT SURVIVED A `SIGKILL` IS THE OPERATOR'S BUSINESS. `ReapOutcome`
    // re-reads rather than assuming, because a delivered signal is not a death
    // — see its own doc. This is the one case in the enumeration with a direct
    // operator cost: they run `/stop`, get a clean receipt, and something is
    // still running.
    if !outcome.survivors.is_empty() {
        survived.push(format!(
            "{} process group(s) started by '{session}' survived SIGKILL (pids {})",
            outcome.survivors.len(),
            outcome.survivors.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
        ));
    }
    if outcome.killed > 0 {
        eprintln!(
            "chief stop: {} of {} process group(s) started by '{session}' ignored SIGTERM and \
             were killed",
            outcome.killed, outcome.groups
        );
    }
}

/// `chief stop` — this directory's company.
///
/// # Errors
/// [`super::LifecycleError`] when this directory holds no company or a step
/// refuses.
pub(crate) async fn run(dir: &Path) -> Result<()> {
    super::require_a_company_here(dir, "chief stop")?;
    // AUTHENTICATED: every request below goes to a COMPANY DAEMON, which
    // verifies a presented bearer.
    let client = Client::operator(dir);
    let outcome = stop_runtime(&client, dir, false).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&outcome)
            .unwrap_or_else(|_| format!("{{\"mode\":\"{}\"}}", outcome.mode))
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{kill_runtime_sessions, mode_label, StopMode, StopOutcome};
    use crate::attach::actuator_session_name;
    use crate::tmux;
    use crate::tmux::test_support::{require_tmux, start_session, unique_socket};
    use chief_cli::reap;
    use nix::sys::signal::Signal;

    /// The ordering law, stated as data. Production's `stop_runtime` performs
    /// these in exactly this order in its supervised branch; this test is what
    /// makes reversing it a visible edit rather than a silent regression.
    const SUPERVISED_ORDER: [&str; 8] = [
        "clear-launch-intent",
        "clear-runtime",
        "reap-actuator-processes",
        "kill-actuator",
        "reap-session-processes",
        "kill-session",
        "release-ownership",
        "stop-daemon",
    ];

    #[test]
    fn the_durable_teardown_commits_before_the_daemon_dies() {
        let launch_intent = SUPERVISED_ORDER.iter().position(|step| *step == "clear-launch-intent");
        let runtime = SUPERVISED_ORDER.iter().position(|step| *step == "clear-runtime");
        let daemon = SUPERVISED_ORDER.iter().position(|step| *step == "stop-daemon");
        assert!(launch_intent < daemon, "launch intent must be cleared while the daemon serves it");
        assert!(
            runtime < daemon,
            "the runtime projection must be cleared while the daemon serves it"
        );

        // THE STRANDING BUG. A stop that never released the claim left
        // `runtime_owner` at `status=active, released_at=NULL` on a company
        // with nothing running, and the next boot from any other socket was
        // refused by a claim naming a socket that no longer had a server. The
        // release is a durable write like the two above it, so it has the same
        // deadline: before the daemon that serves it dies.
        let release = SUPERVISED_ORDER.iter().position(|step| *step == "release-ownership");
        assert!(release.is_some(), "a supervised stop must release the ownership claim");
        assert!(release < daemon, "the claim must be released while the daemon still serves it");
        let session = SUPERVISED_ORDER.iter().position(|step| *step == "kill-session");
        // THE ORPHANED ACTUATOR. The actuator's lifetime is the tmux SERVER's,
        // so a stop that names only the company session leaves it retrying a
        // daemon that is about to die, forever. It goes down BEFORE the company
        // session, because re-minting a company session somebody killed is
        // exactly what it does.
        let actuator = SUPERVISED_ORDER.iter().position(|step| *step == "kill-actuator");
        assert!(actuator.is_some(), "a supervised stop must kill the actuator session");
        assert!(
            actuator < session,
            "the actuator must go down first, or it re-mints the company session the stop just \
             killed"
        );
        assert!(actuator < daemon, "the actuator must not outlive the daemon it actuates");
        assert!(
            session < release,
            "release after the session is gone: the claim describes a runtime that must already \
             be down, or a takeover could land on a live one"
        );
    }

    /// THE ORPHANED WORK. A stop that killed only the panes left everything
    /// those panes had started — a person's `bun run test` and eight
    /// descendants — running at loadavg 4.20 on a stopped company.
    ///
    /// The reap must come BEFORE the kill of the same session, and that is a
    /// mechanical requirement rather than a preference: `kill-session` removes
    /// the pane, `list-panes` is the only thing that knows the pane's pid, and
    /// a pid nobody can read is a process group nobody can stop. The kill-first
    /// order does not merely reap late; it cannot reap at all.
    #[test]
    fn the_work_a_pane_started_is_stopped_before_its_session_is_killed() {
        let position = |step: &str| SUPERVISED_ORDER.iter().position(|it| *it == step);
        for (reap, kill) in [
            ("reap-actuator-processes", "kill-actuator"),
            ("reap-session-processes", "kill-session"),
        ] {
            assert!(position(reap).is_some(), "a supervised stop must reap {kill}'s processes");
            assert!(
                position(reap) < position(kill),
                "{reap} must run before {kill}, or the pane pids it needs are already gone"
            );
        }
    }

    #[test]
    fn every_mode_has_exactly_one_label_and_they_are_distinct() {
        let labels = [mode_label(StopMode::Supervised), mode_label(StopMode::AlreadyStopped)];
        assert_eq!(labels, ["supervised", "already-stopped"]);
        // `dedup` is a Vec method; `labels` is a fixed-size array.
        let mut sorted = labels.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 2);
    }

    #[test]
    fn the_outcome_serializes_as_the_camel_case_object_a_script_reads() {
        let outcome = StopOutcome {
            mode: "supervised",
            dir: "/work/acme".to_string(),
            session: "org-acme-012345_".to_string(),
            session_stopped: true,
            actuator_stopped: true,
            daemon_stopped: true,
        };
        let json = serde_json::to_value(&outcome).expect("serialize");
        assert_eq!(json["mode"], "supervised");
        assert_eq!(json["dir"], "/work/acme");
        assert_eq!(json["sessionStopped"], true);
        assert_eq!(json["actuatorStopped"], true);
        assert_eq!(json["daemonStopped"], true);
    }

    /// A STOP WITH NO LIVE DAEMON NAMES NO SESSION, and that is the honest
    /// answer rather than a missing one.
    ///
    /// Its predecessor tore down `org-<slug>_`, composed from the word the
    /// operator typed. Nobody types one now and the slug is in the store the
    /// dead daemon was serving, so a session name here could only be a guess —
    /// and `org-<key6>_` collides with whatever else a box happens to be
    /// running.
    #[test]
    fn a_stop_with_no_live_daemon_reports_no_session_rather_than_a_guessed_one() {
        let outcome = StopOutcome {
            mode: mode_label(StopMode::AlreadyStopped),
            dir: "/work/acme".to_string(),
            session: String::new(),
            session_stopped: false,
            actuator_stopped: false,
            daemon_stopped: true,
        };
        let json = serde_json::to_value(&outcome).expect("serialize");
        assert_eq!(json["session"], "");
        assert_eq!(json["sessionStopped"], false);
    }

    #[test]
    fn reset_preserves_the_live_listener_and_a_plain_stop_never_does() {
        // #28's contract, as a property of the flag rather than of a comment:
        // `daemon_stopped` is the negation of `preserve_daemon` in the
        // supervised branch, and unconditionally true in the degraded one.
        let preserved = StopOutcome {
            mode: "supervised",
            dir: "/work/acme".to_string(),
            session: "org-acme-012345_".to_string(),
            session_stopped: true,
            actuator_stopped: false,
            daemon_stopped: false,
        };
        assert!(!preserved.daemon_stopped);
    }

    /// SIMULATED TMUX, the seam the bug actually lived at: a real tmux server
    /// holding a real company session AND its real actuator session, torn down
    /// by the very function `stop_runtime` calls.
    ///
    /// The bug this pins shipped because the stop path named one session out of
    /// two. Asserting the ordering constant alone would not have caught it —
    /// the constant is a description of production, and the production call was
    /// `kill_session(company)` with no actuator anywhere in it. This drives the
    /// condition: both sessions exist, one call, and NEITHER may survive.
    #[test]
    fn a_stop_takes_the_actuator_down_with_the_company_session() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("stop-actuator");
        let session = "org-acme-012345_";
        let actuator = actuator_session_name(session);
        start_session(&socket, session, &["sh", "-c", "sleep 600"]);
        start_session(&socket, &actuator, &["sh", "-c", "sleep 600"]);
        assert_eq!(tmux::session_exists(&socket, &actuator), Some(true), "fixture");

        let killed =
            kill_runtime_sessions(&socket, session, &mut Vec::new()).expect("tmux must not refuse");

        let actuator_left = tmux::session_exists(&socket, &actuator);
        let session_left = tmux::session_exists(&socket, session);
        tmux::run(&socket, &["kill-server"]);
        assert_eq!(killed, (true, true), "both halves of the runtime were running");
        assert_eq!(
            actuator_left,
            Some(false),
            "the actuator outlived the stop and retries the dead daemon forever"
        );
        assert_eq!(session_left, Some(false), "the company session outlived the stop");
    }

    /// DEFECT B, END TO END, against a real tmux server: a stop must stop the
    /// work the company started, not only the panes that started it.
    ///
    /// The pane starts a `setsid` sleep and keeps running, which is the shape
    /// of a person running `bun run test` from a bash tool: a tool runner puts
    /// a command in a session of its own so it can stop the whole tree, and
    /// `kill-session` then reaches the pane's own group and never that one.
    /// Before this branch nine such processes held a stopped company's box at
    /// loadavg 4.20, while the stop reported `sessionStopped: true`.
    ///
    /// The escaped pid is read from the process table rather than from a file
    /// the fixture writes: the table is what the reap itself consults, so the
    /// test and the code under test agree on what "a descendant of this pane"
    /// means.
    #[test]
    fn a_stop_ends_the_processes_the_companys_panes_started() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("stop-orphan");
        let session = "org-acme-012345_";
        start_session(&socket, session, &["sh", "-c", "setsid sleep 600 & sleep 600"]);

        let pane = *tmux::pane_pids(&socket, session).first().expect("the pane has a pid");
        // Wait for the escapee: the pane's own descendant, in a group of its
        // own, which is precisely what a process-group kill cannot reach.
        let escaped = wait_for(|| {
            reap::read_process_table()
                .into_iter()
                .find(|row| row.ppid == pane && row.pgid != pane)
                .map(|row| row.pid)
        })
        .expect("the pane started work in a session of its own");

        let killed =
            kill_runtime_sessions(&socket, session, &mut Vec::new()).expect("tmux must not refuse");

        let survived = wait_for(|| alive(escaped).then_some(())).is_some();
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(escaped), Signal::SIGKILL);
        tmux::run(&socket, &["kill-server"]);

        assert!(killed.1, "the company session was running and was stopped");
        assert!(
            !survived,
            "a stopped company must not leave the work its panes started running on the box"
        );
    }

    /// Is this pid still alive?
    ///
    /// `beacond::liveness::pid_is_live` and never a second `kill(pid, 0)`:
    /// EPERM means the process EXISTS, and a hand-rolled probe that reads it as
    /// death is the defect that guard exists to stop.
    fn alive(pid: i32) -> bool {
        beacond::liveness::pid_is_live(i64::from(pid))
    }

    /// Poll `answer` for up to five seconds, returning the first `Some`.
    fn wait_for<T>(mut answer: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(value) = answer() {
                return Some(value);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            // os-liveness: waiting for the kernel to start or tear down a real
            // process, which is the one thing an injected clock cannot do.
            #[allow(clippy::disallowed_methods)]
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// **THE BLAST RADIUS, PINNED RATHER THAN ARGUED.**
    ///
    /// The operator resorted to `pkill -f "chief"` when `/stop` failed them — a
    /// command that matches ANY process whose argv contains the substring,
    /// including a second company's. That is the anti-pattern this teardown
    /// must never become, and "it identifies by records today" is one refactor
    /// away from not being true.
    ///
    /// Two real tmux sessions on one socket, named for two different companies.
    /// Stopping one must leave the other entirely untouched — its session, its
    /// actuator and the process its pane started.
    #[test]
    fn stopping_one_company_leaves_another_on_the_same_socket_untouched() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("stop-two-companies");
        let ours = "org-acme-012345_";
        let theirs = "org-other-abcdef_";
        start_session(&socket, ours, &["sh", "-c", "sleep 600"]);
        start_session(&socket, &actuator_session_name(ours), &["sh", "-c", "sleep 600"]);
        start_session(&socket, theirs, &["sh", "-c", "sleep 600"]);
        start_session(&socket, &actuator_session_name(theirs), &["sh", "-c", "sleep 600"]);

        let killed =
            kill_runtime_sessions(&socket, ours, &mut Vec::new()).expect("tmux must not refuse");

        let survived = (
            tmux::session_exists(&socket, theirs),
            tmux::session_exists(&socket, &actuator_session_name(theirs)),
        );
        let ours_gone = (
            tmux::session_exists(&socket, ours),
            tmux::session_exists(&socket, &actuator_session_name(ours)),
        );
        tmux::run(&socket, &["kill-server"]);

        assert_eq!(killed, (true, true), "both halves of OUR company are killed");
        assert_eq!(ours_gone, (Some(false), Some(false)), "and neither survives");
        assert_eq!(
            survived,
            (Some(true), Some(true)),
            "the OTHER company on the same socket is untouched — a stop scoped by name-matching \
             instead of by this company's own session names would have taken it"
        );
    }

    /// **`beacond` SURVIVES A STOP, BY ASSERTION AND NOT BY ACCIDENT.**
    ///
    /// Operator's explicit carve-out: *"just keep beacond that's
    /// company-agnostic."* It is shared across every company on the box, so a
    /// stop that took it would stop things it was never asked about — and their
    /// own `pkill` DID take it.
    ///
    /// Asserted on the SOURCE because the property is an absence: the teardown
    /// must contain no kill, signal or session verb aimed at beacond. A
    /// behavioural test can only show that one particular beacond survived one
    /// particular run; this shows there is no code that could ever target it.
    #[test]
    fn nothing_in_the_teardown_targets_beacond() {
        // PRODUCTION CODE ONLY, and the reason is this test's own first run:
        // it matched its OWN NAME. A guard that reads the file it lives in
        // reads itself, and CLAUDE.md records the same trap for the same reason
        // — a rule that quotes the thing it forbids goes red while the code is
        // right, which is how somebody learns to delete guards. Split at the
        // test module exactly as `settle-budget-single-definition` does, and
        // strip comments, so the assertion is about CODE.
        let source = include_str!("stop.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        // THE TEARDOWN DOES NOT MENTION BEACOND AT ALL, which is a stronger
        // fact than "only reads it" and is the one the first draft of this test
        // got wrong twice: it matched its own function name, and then — once
        // scoped to production — it discovered that the `pid_is_live` call it
        // was written around lives in the TEST module, as a helper asserting a
        // process is up. Production has no reference of any kind.
        let mentions: Vec<&str> = production
            .lines()
            .filter(|line| line.contains("beacond"))
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        assert!(
            mentions.is_empty(),
            "the teardown must never name beacond — it is company-agnostic and shared, and the \
             operator carved it out explicitly: {mentions:?}"
        );
        // NON-VACUITY, on the SPLIT rather than on the match: a typo in the
        // `#[cfg(test)]` marker would make `production` the whole file and this
        // assertion would then be checking nothing. The tests below do name
        // beacond, so a correct split always finds it on the other side.
        assert!(
            source.contains("beacond::liveness::pid_is_live"),
            "the split is wrong or the test helper has moved; re-point this guard"
        );
    }

    /// Idempotent, on the half that is already gone. A company whose actuator
    /// somebody already killed must still stop cleanly, and must report the
    /// absent half as `false` rather than claiming a kill it did not perform.
    #[test]
    fn stopping_a_company_whose_actuator_is_already_gone_is_not_an_error() {
        if !require_tmux() {
            return;
        }
        let socket = unique_socket("stop-actuator-absent");
        let session = "org-acme-012345_";
        start_session(&socket, session, &["sh", "-c", "sleep 600"]);

        let killed =
            kill_runtime_sessions(&socket, session, &mut Vec::new()).expect("tmux must not refuse");

        tmux::run(&socket, &["kill-server"]);
        assert_eq!(killed, (false, true));
    }
}
