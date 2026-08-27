//! The actuator never gives up on a person who will not stay up, and the wait
//! between attempts is bounded.
//!
//! **Against a real tmux server, with a real broken launch.** The condition
//! under test is a process that dies the instant it is spawned, and the only
//! evidence the actuator has of it is a pane that is gone — or is a different
//! pane — by the next observation. A fake executor cannot produce that: the
//! pane has to be really minted by a real server, the exec has to really fail,
//! and the server has to really reap it while the test is looking away.
//!
//! `vera`'s launch names a `pi` binary that does not exist, so tmux mints her
//! pane, the exec fails, and (with no `remain-on-exit`) the pane vanishes —
//! which is exactly what happened to `ivo` and `sasha` on the owner's box when
//! their Pi died inside extension bind during a chiefd outage. `theo`'s launch
//! names `/bin/cat`, which blocks on its tty forever, so the company session
//! survives every round and the test can tell "one person's boot keeps dying"
//! from "everything is broken".
//!
//! THE LIVE WEDGE THIS PINS. Five consecutive deaths used to make the actuator
//! stop trying, drop the person from placement, and publish that verdict to a
//! tmux session option so no replacement actuator would retry either. Nothing
//! released it: the release conditions all needed a live pane, and a person
//! dropped from placement never gets one. Four people sat at `starting` for an
//! hour and a half after the fault that caused it had cleared.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use chief_cli::actuate::crash_loop::{retry_delay, CrashLoop, MAX_RETRY_DELAY};
use chief_cli::actuate::ever_observed::EverObserved;
use chief_cli::actuate::host::Socket;
use chief_cli::actuate::interpret::{
    apply_plan_with_launch_roster, LaunchInputs, LaunchRosterDiagnostics, PassContext,
};
use chief_cli::actuate::runner::{SystemTmuxRunner, ThreadWaiter};
use chief_cli::actuate::{observe, plan, LaunchSpec, TmuxHost};
use chief_cli::placement;
use chief_cli::proc::ProcReader;
use chief_cli::real::RealHostExecutor;

const ORG: &str = "backoff";
const SESSION: &str = "backoff-session";
/// How long the broken stand-in lives before it dies.
///
/// NOT ZERO, and that is the realistic shape rather than a convenience. `ivo`'s
/// Pi on the owner's box reached `extensions-bind-begin` and died there — after
/// the pane existed and after the actuator had finished tagging it. A launch
/// that fails its `exec` outright dies INSIDE the spawning pass instead, which
/// fail-stops that pass, and a failed pass is deliberately not evidence about
/// anybody's process (`crash_loop`'s `previous_pass_failed`). Both are real;
/// this test drives the one the crash loop exists to count.
const DEATH_AFTER: &str = "0.5";
/// Long enough for the broken stand-in to have died before the next pass looks.
const BETWEEN_PASSES: Duration = Duration::from_millis(800);

/// Wait for the world outside this process to move.
///
/// The injected `Clock` cannot help here: what this test waits for is a real
/// child process exiting and a real tmux server reaping its pane, neither of
/// which this process schedules. The crash loop's OWN clock is still injected —
/// every `Instant` it is handed is computed, never read.
#[expect(
    clippy::disallowed_methods,
    reason = "waiting on a real tmux server reaping a real process, which no injected clock drives"
)]
fn settle() {
    std::thread::sleep(BETWEEN_PASSES);
}
/// A stand-in for `pi` that ignores whatever launch arguments it is handed and
/// then does exactly one thing.
#[expect(
    clippy::disallowed_methods,
    reason = "a real tmux pane needs a real executable on disk; this test IS the host effect"
)]
fn stand_in(label: &str, body: &str) -> PathBuf {
    // WRITTEN ONCE PER PROCESS, AND NEVER REWRITTEN WHILE IT MAY BE RUNNING.
    //
    // This keyed its path on the pid and `line!()`. `line!()` expands where it
    // is WRITTEN, not at the call site, so it was one constant for every
    // caller: the files left behind were `chiefd-backoff-<label>-<pid>-83`,
    // one per process rather than one per call. Both tests in this file build
    // their launches from `launches()`, and `launches()` is called on EVERY
    // pass, so this rewrote those two scripts hundreds of times per run —
    // while cargo ran the two tests in parallel threads of that same process.
    //
    // `fs::write` truncates before it fills. So one test could hand the other
    // test's tmux pane a zero-length script: the HEALTHY stand-in dies, the
    // actuator sees a dead pane and spawns that person a second time, and
    // `a_launch_that_can_never_work_is_still_being_attempted_long_past_the_old_limit`
    // fails on `theo_spawns` being 2 where it must be 1. That is the CI red of
    // 2026-08-21 — "the healthy person beside her was spawned once and left
    // alone", left 2, right 1.
    //
    // A unique path per CALL also removes the race, and was the first repair
    // here — but `launches()` runs per pass, so it leaked ~40 scripts per test
    // run (measured: 936 files after eighteen). Writing each script exactly
    // once is the honest shape: there is nothing to truncate, and the file
    // count is a constant.
    static WRITTEN: std::sync::Mutex<Option<BTreeMap<String, PathBuf>>> =
        std::sync::Mutex::new(None);
    let mut guard = WRITTEN.lock().expect("the stand-in registry is never poisoned");
    let written = guard.get_or_insert_with(BTreeMap::new);
    if let Some(path) = written.get(label) {
        return path.clone();
    }
    let path = std::env::temp_dir().join(format!("chiefd-backoff-{label}-{}", std::process::id()));
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the stand-in");
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("make the stand-in executable");
    written.insert(label.to_owned(), path.clone());
    path
}

/// TWO TESTS IN THIS FILE, ONE PROCESS, AND THE SCRIPTS THEY EXEC MUST NOT BE
/// REWRITTEN UNDER THEM.
///
/// Cargo runs these tests in parallel threads, and `launches()` — which builds
/// both stand-ins — is called on every pass. When `stand_in` rewrote a fixed
/// path, one test truncated the script the other test's tmux pane was about to
/// exec, the healthy stand-in died, and its person was spawned a second time.
/// That is the CI red this file has cost.
///
/// The rule is not "unique paths" — it is that a script, once written, is not
/// written again. This pins it by INODE and by modification time, which is the
/// only way to tell "handed back the same path" from "rewrote the same path".
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the subject is a real file on disk that a real tmux pane execs; stamping it \
              is how a rewrite is detected at all"
)]
fn a_stand_in_is_written_once_and_never_rewritten() {
    let first = healthy_binary();

    // A MARKER THE HELPER DOES NOT WRITE. A rewrite truncates, so the marker
    // is gone if and only if the file was written again.
    //
    // Neither `mtime()` nor `ino()` can see this: `fs::write` truncates IN
    // PLACE so the inode is unchanged, and `mtime()` is whole seconds while
    // the rewrites happen microseconds apart. A first version of this test
    // used exactly those two and passed against the broken helper — a test
    // that cannot fail is not evidence, so it is written this way instead.
    let marker = b"\n# written-once marker\n";
    let original = std::fs::read(&first).expect("the stand-in exists");
    let mut stamped = original.clone();
    stamped.extend_from_slice(marker);
    std::fs::write(&first, &stamped).expect("stamp the stand-in");

    // Every later call must hand back that same file, untouched — this is the
    // shape `launches()` produces on every one of the twenty passes.
    for _ in 0..5 {
        let again = healthy_binary();
        assert_eq!(again, first, "the same stand-in, not a new one to leak");
        let now = std::fs::read(&again).expect("the stand-in still exists");
        assert!(
            now.ends_with(marker),
            "the stand-in was REWRITTEN — that truncation is the window a parallel test's \
             tmux pane execs through, and it is what spawned the healthy person twice"
        );
    }

    assert_ne!(healthy_binary(), broken_binary(), "and the two kinds never collide");
}

/// Never exits, so the company session survives every round.
fn healthy_binary() -> PathBuf {
    stand_in("healthy", "exec sleep 3600")
}

/// Comes up, then dies — the shape a Pi that fails during start-up has.
fn broken_binary() -> PathBuf {
    stand_in("broken", &format!("sleep {DEATH_AFTER}; exit 3"))
}

fn spec(person: &str, binary: PathBuf) -> LaunchSpec {
    LaunchSpec {
        pi_binary: binary,
        pi_home: PathBuf::from("/tmp"),
        workspace: PathBuf::from("/tmp"),
        display_name: format!("Backoff · {person}"),
        person_name: person.to_owned(),
        accent: None,
        tools: Vec::new(),
        extensions: Vec::new(),
        session: None,
        pending_mail: false,
        env: vec![("ORG_LAUNCHER_ORGANIZATION".into(), ORG.into())],
    }
}

fn launches() -> BTreeMap<String, LaunchSpec> {
    BTreeMap::from([
        ("theo".to_owned(), spec("theo", healthy_binary())),
        ("vera".to_owned(), spec("vera", broken_binary())),
    ])
}

fn topology() -> placement::Topology {
    placement::Topology {
        organization: ORG.into(),
        session: SESSION.into(),
        windows: vec![placement::Window {
            logical_id: "eng".into(),
            name: "engineering".into(),
            panes: vec![
                placement::Pane {
                    person_id: "theo".into(),
                    launch_hash: "hash-theo".into(),
                    order: 0,
                },
                placement::Pane {
                    person_id: "vera".into(),
                    launch_hash: "hash-vera".into(),
                    order: 1,
                },
            ],
        }],
        known_person_ids: ["theo".to_owned(), "vera".to_owned()].into_iter().collect(),
    }
}

fn hashes() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("theo".to_owned(), "hash-theo".to_owned()),
        ("vera".to_owned(), "hash-vera".to_owned()),
    ])
}

fn executor() -> RealHostExecutor<SystemTmuxRunner, ThreadWaiter> {
    RealHostExecutor::new(
        TmuxHost::new(SystemTmuxRunner::default(), ThreadWaiter),
        ProcReader::default(),
    )
}

/// Who tmux is carrying right now, person id to pane id — the actuator's own
/// reading, and the only evidence a boot took.
fn person_panes(observed: &plan::ObservedTopology) -> BTreeMap<String, String> {
    observed
        .panes
        .iter()
        .filter(|pane| pane.organization_id == ORG)
        .filter(|pane| !pane.person_id.is_empty())
        .map(|pane| (pane.person_id.clone(), pane.tmux_id.clone()))
        .collect()
}

/// The people a plan's steps would spawn.
fn spawn_targets(steps: &[plan::Step]) -> Vec<String> {
    steps
        .iter()
        .filter_map(|step| match step {
            plan::Step::CreateSession { first }
            | plan::Step::CreateWindowWithSpawn { first, .. } => Some(first.person_id.clone()),
            plan::Step::SplitPane { spec, .. } | plan::Step::Respawn { spec, .. } => {
                Some(spec.person_id.clone())
            }
            _ => None,
        })
        .collect()
}

struct Round {
    /// Who this pass asked tmux to spawn.
    spawned: Vec<String>,
    /// Who this pass skipped because their backoff had not elapsed.
    deferred: BTreeSet<String>,
    /// Who tmux was carrying when this pass looked.
    live: BTreeMap<String, String>,
}

/// One converge pass, exactly as the resident actuator runs one: observe, judge
/// the crash loop, ask who is waiting, plan against chiefd's WHOLE desired set,
/// apply.
fn pass(
    exec: &RealHostExecutor<SystemTmuxRunner, ThreadWaiter>,
    socket: &Socket,
    ever: &EverObserved,
    registry: &mut CrashLoop,
    now: Instant,
) -> Round {
    let observed = observe(exec, socket, SESSION, ever).expect("observe the live session");
    let live = person_panes(&observed);
    registry.observed(&hashes(), &live, now);
    let waiting = registry.waiting(now);
    let converge =
        plan::compute_converge_plan(&topology(), &observed).expect("a plan over a live session");
    let launches = launches();
    let report = apply_plan_with_launch_roster(
        exec,
        socket,
        &topology(),
        &observed,
        LaunchInputs {
            catalog: &launches,
            diagnostics: LaunchRosterDiagnostics::default(),
            deferred: &waiting,
        },
        &converge,
        PassContext::default(),
    );
    let reached = converge.steps.get(..report.steps_reached).unwrap_or(&converge.steps);
    let spawned: Vec<String> = spawn_targets(reached)
        .into_iter()
        .filter(|person| !report.deferred.contains(person))
        .collect();
    registry.spawning(spawned.clone());
    registry.pass_failed(report.failure.is_some());
    Round { spawned, deferred: report.deferred, live }
}

fn unique_socket(label: &str) -> String {
    format!("chiefd-backoff-{label}-{}", std::process::id())
}

/// Keep the throwaway server alive independently of the company session, so a
/// pane that dies takes nothing else with it.
fn seed_server(socket_name: &str) {
    let seed = Command::new("tmux")
        .args(["-L", socket_name, "new-session", "-d", "-s", "__seed", "--", "sleep", "3600"])
        .status()
        .expect("seed the throwaway tmux server");
    assert!(seed.success(), "failed to seed tmux server on socket {socket_name:?}");
}

fn kill_server(socket_name: &str) {
    let _ = Command::new("tmux").args(["-L", socket_name, "kill-server"]).status();
}

/// THE WHOLE RULING, DRIVEN AGAINST A REAL TMUX.
///
/// Twenty passes with a launch that cannot possibly work. The old design gave
/// up at five. This asserts the person is spawned again and again all the way
/// to the twentieth, that the healthy person beside them was never disturbed,
/// and that the wait between attempts never passed the ceiling.
#[test]
fn a_launch_that_can_never_work_is_still_being_attempted_long_past_the_old_limit() {
    let socket_name = unique_socket("forever");
    seed_server(&socket_name);
    let exec = executor();
    let socket = Socket(socket_name.clone());
    let ever = EverObserved::new();
    let mut registry = CrashLoop::new();
    let base = Instant::now();
    let mut clock = Duration::ZERO;

    let mut vera_spawns = 0_u32;
    let mut theo_spawns = 0_u32;
    for _ in 0..20 {
        // Far enough apart that every attempt is due: this test is about
        // whether attempts KEEP HAPPENING, and the delay has its own test.
        clock += MAX_RETRY_DELAY;
        let outcome = pass(&exec, &socket, &ever, &mut registry, base + clock);
        if outcome.spawned.iter().any(|person| person == "vera") {
            vera_spawns += 1;
        }
        if outcome.spawned.iter().any(|person| person == "theo") {
            theo_spawns += 1;
        }
        assert!(
            outcome.deferred.iter().all(|person| person == "vera"),
            "only the person whose boot is dying ever waits: {:?}",
            outcome.deferred
        );
        // tmux needs a moment to reap a pane whose exec failed; without this the
        // observation races the server and the death is invisible.
        settle();
    }

    let report = registry.reports(base + clock);
    kill_server(&socket_name);

    assert!(
        vera_spawns >= 8,
        "a person whose launch can never work must keep being attempted; she was spawned \
         {vera_spawns} times in 20 passes, and the design this replaces stopped at 5"
    );
    assert_eq!(theo_spawns, 1, "and the healthy person beside her was spawned once and left alone");
    let vera = report.get("vera").expect("vera's crash report");
    assert!(vera.failures >= 5, "her failures were counted: {vera:?}");
    assert!(
        vera.retry_in <= MAX_RETRY_DELAY,
        "and the wait between attempts never passes the ceiling: {vera:?}"
    );
}

/// A person waiting out their backoff costs THEIR OWN STEP AND NOTHING ELSE —
/// and the wait is over as soon as it elapses.
///
/// The old design's equivalent was permanent and took the person's whole window
/// with them. This asserts the skip is one step, one pass, and that the very
/// next pass past the delay spawns them again.
#[test]
fn a_person_inside_their_backoff_is_skipped_for_one_pass_and_spawned_on_the_next() {
    let socket_name = unique_socket("deferral");
    seed_server(&socket_name);
    let exec = executor();
    let socket = Socket(socket_name.clone());
    let ever = EverObserved::new();
    let mut registry = CrashLoop::new();
    let base = Instant::now();
    let mut clock = Duration::ZERO;

    // Two passes: the first mints the session and both panes, the second finds
    // vera's gone and charges her one failure.
    for _ in 0..2 {
        clock += MAX_RETRY_DELAY;
        pass(&exec, &socket, &ever, &mut registry, base + clock);
        settle();
    }
    let failures = registry.reports(base + clock).get("vera").map(|report| report.failures);
    assert_eq!(failures, Some(1), "one dead pane is one failure");

    // A pass INSIDE her delay. She is skipped by name; theo is untouched and
    // his pane is still there, which is the "costs their own step" property.
    clock += retry_delay(1) / 2;
    let inside = pass(&exec, &socket, &ever, &mut registry, base + clock);
    assert_eq!(
        inside.deferred.into_iter().collect::<Vec<_>>(),
        vec!["vera".to_owned()],
        "the person inside their backoff is the only one skipped"
    );
    assert!(!inside.spawned.contains(&"vera".to_owned()), "and nothing was spawned for her");
    assert!(inside.live.contains_key("theo"), "while the healthy person kept his pane");

    // A pass PAST her delay. The wait is over and she is attempted again, with
    // no operator action of any kind.
    clock += retry_delay(1);
    let outside = pass(&exec, &socket, &ever, &mut registry, base + clock);
    kill_server(&socket_name);
    assert!(
        outside.deferred.is_empty(),
        "once the delay elapses nobody is waiting; a backoff is not a hold"
    );
    assert!(
        outside.spawned.contains(&"vera".to_owned()),
        "and she is spawned again on the very next pass"
    );
}
