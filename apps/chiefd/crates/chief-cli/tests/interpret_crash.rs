//! Crash-injection tests for the converge-apply interpreter's tag sequences
//! (TESTING.md §4.3; task #18 / P2 restartability control, task #23).
//!
//! **Status: the fix has landed in this commit.** `interpret.rs` writes a
//! `tags::MINTING` marker on a window/pane BEFORE its first identity tag and
//! clears it AFTER its last; `observe()` (`observe.rs`) reads that marker as
//! an extra trailing field on the SAME `list-windows`/`list-panes` calls it
//! already makes (no added tmux round-trip) and destroys anything still
//! carrying it, before the pass that would otherwise see it half-tagged. All
//! four tests below are green: `window_tag_crash_self_heals_within_a_bounded_
//! number_of_passes` and `pane_tag_crash_self_heals_without_a_permanent_
//! duplicate` are the acceptance controls (red before this commit, per the
//! earlier report); `window_tag_crash_no_longer_wedges_the_company` and
//! `pane_tag_crash_no_longer_mints_a_duplicate` are the characterization
//! controls, DELIBERATELY inverted in this same commit — each carries its old
//! (now-false) body in a comment so the inversion is legible, not silent.
//!
//! **These are real crashes against a real tmux server**, not a mocked
//! executor: `tag_window`/`tag_pane` (interpret.rs) each issue several
//! SEPARATE `set-option` round-trips, and the property under test —
//! "does the tmux SERVER (which outlives the killed chiefd process) end up
//! in a state the next pass can recover from" — cannot be answered by a fake
//! whose state dies with the process. The child that gets SIGKILLed is a real
//! `RealHostExecutor<SystemTmuxRunner>` talking to a real, dedicated `-L`
//! socket; the parent re-observes that same live socket after the kill via
//! the production `observe()` used by every real converge pass, then feeds
//! it to the production `compute_converge_plan` — the exact function the
//! daemon's own next pass would call.
//!
//! A same-process simulated crash (killing nothing, just re-reading state at
//! the top of a loop) would prove nothing here: the state these tests probe
//! (tmux window/pane options) lives OUTSIDE the chiefd process and is
//! unaffected by how gracefully or ungracefully that process ends — the only
//! thing that varies is which `set-option` calls got to run before the
//! SIGKILL, which is exactly what the named pause points fix at a
//! deterministic instant instead of a timing race (TESTING.md §1.2).
//!
//! **Retargeted after §2.0(2) ONE SHOT (F12, architecture-audit Step 2):**
//! `CreateSession` and `CreateWindowByMove` no longer HAVE a tag sequence —
//! identity rides the single creating tmux invocation (see
//! `tests/interpret_crash_session.rs`, the acceptance control for that fix) —
//! so the multi-call tag tear this suite controls now lives only on
//! `CreateWindowWithSpawn`/`SplitPane`. The crash child therefore sets up an
//! already-live session (theo in `eng`, one-shot, never crashes) and then
//! crashes inside a `CreateWindowWithSpawn` adding vera's `ops` window: the
//! same torn-mint shape, the same pause points, the same marker repair under
//! test.
//!
//! Dedicated throwaway `-L` sockets only — never the operator's tmux server,
//! matching the e2e harness's own per-world isolation. Each test creates its
//! own socket and kills only that socket's server at teardown.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;

use chief_cli::actuate::ever_observed::EverObserved;
use chief_cli::actuate::host::Socket;
use chief_cli::actuate::plan;
use chief_cli::actuate::runner::{SystemTmuxRunner, ThreadWaiter};
use chief_cli::actuate::TmuxHost;
use chief_cli::actuate::{apply_plan, observe, LaunchSpec};
use chief_cli::placement;
use chief_cli::proc::ProcReader;
use chief_cli::real::RealHostExecutor;

const CRASH_SOCKET: &str = "CHIEFD_INTERPRET_CRASH_SOCKET";
const CRASH_AT: &str = "CHIEFD_INTERPRET_CRASH_AT";

const ORG: &str = "cobalt";
const SESSION: &str = "cobalt-session";

fn launch_spec_for(person: &str) -> LaunchSpec {
    LaunchSpec {
        pi_binary: PathBuf::from("/opt/pi/bin/pi"),
        pi_home: PathBuf::from(format!("/data/cobalt/.chief/agent/{person}")),
        workspace: PathBuf::from(format!("/data/cobalt/people/{person}/workspace")),
        display_name: format!("Cobalt · {person}"),
        person_name: person.to_owned(),
        accent: Some("#3c7adf".into()),
        tools: vec!["read".into()],
        extensions: Vec::new(),
        session: None,
        pending_mail: false,
        env: vec![("ORG_LAUNCHER_ORGANIZATION".into(), ORG.into())],
    }
}

fn launch_spec() -> LaunchSpec {
    launch_spec_for("vera")
}

fn launches() -> BTreeMap<String, LaunchSpec> {
    BTreeMap::from([
        ("theo".to_owned(), launch_spec_for("theo")),
        ("vera".to_owned(), launch_spec()),
    ])
}

/// The setup topology: theo alone in `eng`, materialized by the (uncrashed)
/// one-shot `CreateSession`.
fn desired_setup() -> placement::Topology {
    placement::Topology {
        organization: ORG.into(),
        session: SESSION.into(),
        windows: vec![placement::Window {
            logical_id: "eng".into(),
            name: "engineering".into(),
            panes: vec![placement::Pane {
                person_id: "theo".into(),
                launch_hash: "hash-3".into(),
                order: 0,
            }],
        }],
        known_person_ids: Default::default(),
    }
}

/// The topology the crashed pass was converging toward: theo keeps `eng`,
/// vera's new `ops` window is what the crashing `CreateWindowWithSpawn` was
/// minting when the SIGKILL landed.
fn desired() -> placement::Topology {
    placement::Topology {
        organization: ORG.into(),
        session: SESSION.into(),
        windows: vec![
            placement::Window {
                logical_id: "eng".into(),
                name: "engineering".into(),
                panes: vec![placement::Pane {
                    person_id: "theo".into(),
                    launch_hash: "hash-3".into(),
                    order: 0,
                }],
            },
            placement::Window {
                logical_id: "ops".into(),
                name: "operations".into(),
                panes: vec![placement::Pane {
                    person_id: "vera".into(),
                    launch_hash: "hash-2".into(),
                    order: 0,
                }],
            },
        ],
        known_person_ids: Default::default(),
    }
}

fn executor() -> RealHostExecutor<SystemTmuxRunner, ThreadWaiter> {
    RealHostExecutor::new(
        TmuxHost::new(SystemTmuxRunner::default(), ThreadWaiter),
        ProcReader::default(),
    )
}

fn empty_observed() -> plan::ObservedTopology {
    plan::ObservedTopology {
        session_exists: false,
        session_organization: String::new(),
        windows: Vec::new(),
        panes: Vec::new(),
    }
}

fn create_session_plan() -> plan::ConvergePlan {
    plan::ConvergePlan {
        steps: vec![plan::Step::CreateSession {
            first: plan::SpawnSpec { person_id: "theo".into(), launch_hash: "hash-3".to_owned() },
        }],
        predicted_respawn_persons: Vec::new(),
        predicted_kill_panes: Vec::new(),
        warnings: Vec::new(),
        ..Default::default()
    }
}

/// The pass under crash: mint vera's `ops` window into the already-live
/// session. `CreateWindowWithSpawn` is one of the two paths that KEEP the
/// multi-call tag sequence and its #18 P2 minting-marker repair (the other is
/// `SplitPane`) — `CreateSession`/`CreateWindowByMove` went one-shot under
/// §2.0(2) (F12, architecture-audit Step 2), so the tear this suite controls
/// now lives here, exercised identically.
fn create_window_plan() -> plan::ConvergePlan {
    plan::ConvergePlan {
        steps: vec![plan::Step::CreateWindowWithSpawn {
            w: plan::WindowSym("ops".into()),
            name: "operations".into(),
            first: plan::SpawnSpec { person_id: "vera".into(), launch_hash: "hash-2".to_owned() },
        }],
        predicted_respawn_persons: Vec::new(),
        predicted_kill_panes: Vec::new(),
        warnings: Vec::new(),
        ..Default::default()
    }
}

// --- the child ------------------------------------------------------------

/// The crashing side. A no-op unless [`CRASH_SOCKET`]/[`CRASH_AT`] are set, so
/// it is harmless as an ordinary test.
#[test]
fn crash_child() {
    let (Ok(socket_name), Ok(crash_at)) = (std::env::var(CRASH_SOCKET), std::env::var(CRASH_AT))
    else {
        return;
    };

    let watch_for = crash_at.clone();
    chief_cli::pause::install(move |name| {
        if name == watch_for {
            // SIGKILL, not `abort()`/`exit()`: no unwinding, no `Drop`, exactly
            // what an OOM kill or `kill -9` on a crash-restarted daemon leaves.
            let _ = nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::SIGKILL);
        }
    });

    // The pane this test asks tmux to spawn names a `pi` binary that does not
    // exist on this box on purpose (no real agent needs to run for a tag-
    // sequence crash test). Left at tmux's default, the exec failure kills the
    // pane instantly, which kills the window, the session, and — since this
    // socket has nothing else on it — the SERVER, all before the tag calls
    // this test is trying to crash between ever run. `remain-on-exit`
    // decouples "the launched command exited" from "the tmux objects vanish",
    // which is exactly the real daemon's own default expectation (a dead pane
    // is diagnosed and respawned, never silently gone). Seeding a harmless
    // long-lived session first is what lets `set-option -g` succeed before
    // any session exists to attach the default to.
    seed_server(&socket_name);

    let exec = executor();
    let socket = Socket(socket_name);
    let launches = launches();
    // Phase 1 (never crashes: `CreateSession` is one-shot now and carries no
    // pause point this child arms): theo materializes in `eng`.
    let setup = apply_plan(
        &exec,
        &socket,
        &desired_setup(),
        &empty_observed(),
        &launches,
        &create_session_plan(),
    );
    assert!(setup.succeeded(), "the uncrashed setup apply must succeed: {:?}", setup.failure);
    // Phase 2: vera's new window, whose tag sequence is where the SIGKILL lands.
    let ever_observed = EverObserved::new();
    let observed = observe(&exec, &socket, SESSION, &ever_observed)
        .expect("observe the session the setup just created");
    let report =
        apply_plan(&exec, &socket, &desired(), &observed, &launches, &create_window_plan());
    panic!("the pause point {crash_at:?} was never reached; apply_plan returned {report:?}");
}

/// Run the child to its death at `point`, and assert it really was killed.
fn crash_at(socket_name: &str, point: &str) {
    let exe = std::env::current_exe().expect("current test binary");
    let status = Command::new(exe)
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CRASH_SOCKET, socket_name)
        .env(CRASH_AT, point)
        .status()
        .expect("spawn the crashing child");
    assert_eq!(
        status.signal(),
        Some(nix::sys::signal::SIGKILL as i32),
        "the child must die by SIGKILL at {point}, not exit ({status:?})"
    );
}

fn unique_socket(label: &str) -> String {
    format!("chiefd-interpret-crash-{label}-{}", std::process::id())
}

/// Bootstrap the throwaway socket's server with a harmless long-lived seed
/// session, then turn on `remain-on-exit` for it. See the comment at the
/// `crash_child` call site for why this is needed at all.
fn seed_server(socket_name: &str) {
    let seed = Command::new("tmux")
        .args(["-L", socket_name, "new-session", "-d", "-s", "__seed", "--", "sleep", "3600"])
        .status()
        .expect("seed the throwaway tmux server");
    assert!(seed.success(), "failed to seed tmux server on socket {socket_name:?}");
    let remain = Command::new("tmux")
        .args(["-L", socket_name, "set-option", "-g", "remain-on-exit", "on"])
        .status()
        .expect("set remain-on-exit");
    assert!(remain.success(), "failed to set remain-on-exit on socket {socket_name:?}");
}

/// Best-effort teardown of the throwaway socket THIS test created. Never
/// touches any socket this test did not itself name.
fn kill_server(socket_name: &str) {
    let _ = Command::new("tmux").args(["-L", socket_name, "kill-server"]).status();
}

/// #23 cross-language crash rig, part 1: real `SIGKILL` between `tag_window`'s
/// two `set-option` calls while adding a NEW window ("eng-probe") to an
/// ALREADY-LIVE session (`PROBE_SOCKET`/`PROBE_SESSION`/`PROBE_ORG`) that a
/// DIFFERENT process (the TypeScript reconciler, `tests/org-tmux.test.ts`)
/// created and is managing. This is the shape a live crash actually produces
/// — a department joining a running company — rather than a hand-built
/// partial tag. Used by `tests/org-tmux.test.ts`'s
/// "a torn window contains its blast radius" regression test to prove
/// `assertUnambiguousObservation` no longer fails the WHOLE organisation over
/// one torn window it did not create. Env-gated: a no-op unless the three
/// PROBE_* vars are set, so it is harmless as an ordinary test run.
#[test]
fn crash_new_window_in_existing_session() {
    let (Ok(socket_name), Ok(session), Ok(org)) =
        (std::env::var("PROBE_SOCKET"), std::env::var("PROBE_SESSION"), std::env::var("PROBE_ORG"))
    else {
        return;
    };
    let watch_for = "interpret:tag_window:after_organization".to_owned();
    chief_cli::pause::install(move |name| {
        if name == watch_for {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::SIGKILL);
        }
    });
    let exec = executor();
    let socket = Socket(socket_name);
    let desired = placement::Topology {
        organization: org.clone(),
        session,
        windows: vec![placement::Window {
            logical_id: "eng-probe".into(),
            name: "eng-probe".into(),
            panes: vec![placement::Pane {
                person_id: "vera-probe".into(),
                launch_hash: "hash-2".into(),
                order: 0,
            }],
        }],
        known_person_ids: Default::default(),
    };
    let observed = plan::ObservedTopology {
        session_exists: true,
        session_organization: org,
        windows: Vec::new(),
        panes: Vec::new(),
    };
    let mut launches = BTreeMap::new();
    launches.insert("vera-probe".to_owned(), launch_spec());
    let steps = vec![plan::Step::CreateWindowWithSpawn {
        w: plan::WindowSym("eng-probe".into()),
        name: "eng-probe".into(),
        first: plan::SpawnSpec { person_id: "vera-probe".into(), launch_hash: "hash-2".to_owned() },
    }];
    let converge_plan = plan::ConvergePlan {
        steps,
        predicted_respawn_persons: Vec::new(),
        predicted_kill_panes: Vec::new(),
        warnings: Vec::new(),
        ..Default::default()
    };
    let report = apply_plan(&exec, &socket, &desired, &observed, &launches, &converge_plan);
    panic!("the pause point was never reached; apply_plan returned {report:?}");
}

/// #23 cross-language crash rig, part 2: runs exactly the `observe()` call a
/// real chiefd converge pass makes first — no daemon/DB plumbing needed — and
/// reports what survived. Used to simulate "one chiefd converge pass ran"
/// between the crash above and the TypeScript reconciler's own next read,
/// without needing to stand up a whole daemon for the question of whether the
/// reap fired. Env-gated: a no-op unless PROBE_SOCKET/PROBE_SESSION are set.
#[test]
fn observe_probe() {
    let (Ok(socket_name), Ok(session)) =
        (std::env::var("PROBE_SOCKET"), std::env::var("PROBE_SESSION"))
    else {
        return;
    };
    let exec = executor();
    let socket = Socket(socket_name);
    let ever_observed = EverObserved::new();
    let observed = observe(&exec, &socket, &session, &ever_observed).expect("observe");
    println!(
        "PROBE session_exists={} windows={} panes={}",
        observed.session_exists,
        observed.windows.len(),
        observed.panes.len()
    );
}

/// Raw, direct tmux read of the current windows in `session`, bypassing
/// `observe()` entirely — used only to prove the crash left the SAME torn
/// state it always has, since `observe()` now reaps a torn object as a side
/// effect of the very first call and so can no longer be used to inspect the
/// pre-reap state.
fn raw_windows(socket_name: &str, session: &str) -> Vec<(String, String)> {
    let out = Command::new("tmux")
        .args([
            "-L",
            socket_name,
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_id}\t#{@organization_window_id}",
        ])
        .output()
        .expect("raw list-windows");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(id, logical)| (id.to_owned(), logical.to_owned()))
        .collect()
}

/// Raw, direct tmux read of the current panes in `session` — see `raw_windows`.
fn raw_panes(socket_name: &str, session: &str) -> Vec<(String, String)> {
    let out = Command::new("tmux")
        .args([
            "-L",
            socket_name,
            "list-panes",
            "-s",
            "-t",
            session,
            "-F",
            "#{pane_id}\t#{@organization_person_id}",
        ])
        .output()
        .expect("raw list-panes");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(id, person)| (id.to_owned(), person.to_owned()))
        .collect()
}

/// Kills its socket's server on drop, including on an unwind from a failed
/// `assert!` — several of the tests below are EXPECTED to panic (that is the
/// whole point of a red control), and a teardown that only runs on the happy
/// path would leak a real background tmux server process every time the
/// control does its job.
struct SocketGuard(String);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        kill_server(&self.0);
    }
}

// --- the tests --------------------------------------------------------

/// **THE RESTARTABILITY CONTROL (design doc §3 P2, acceptance criterion 3:
/// "a converge pass is restartable — killed mid-pass, the next pass converges
/// without manual repair").** This test was RED at HEAD before the fix (see
/// `window_tag_crash_no_longer_wedges_the_company` below for the one-pass
/// proof) and is GREEN now that `observe()` reaps a torn mint as part of its
/// own read, before returning the topology any pass plans from.
///
/// This one asserts the DESIRED end state: a pass killed between the two
/// window tags must not leave the company permanently unreconcilable. Run it
/// however many more (uncrashed) passes it takes; the assertion is that it
/// eventually stops erroring, not that a specific step count fixes it — in
/// practice it now heals on the FIRST subsequent pass, because reaping
/// happens before planning ever sees the torn object.
///
/// **What a wrong-but-plausible fix would do to this test:** a fix that
/// merely retried `compute_converge_plan` a few extra times without changing
/// what it reads would still fail this test, because the READ (a live tmux
/// window carrying only the ORGANIZATION tag) never changes between calls —
/// there is nothing to converge toward without a change to what gets checked
/// or repaired. Reaping the torn object before the read is what changes the
/// read.
#[test]
fn window_tag_crash_self_heals_within_a_bounded_number_of_passes() {
    let socket_name = unique_socket("window-heal");
    crash_at(&socket_name, "interpret:tag_window:after_organization");
    let _guard = SocketGuard(socket_name.clone());

    let socket = Socket(socket_name.clone());
    let exec = executor();

    const MAX_PASSES: u32 = 5;
    let mut last_err: Option<plan::PlanErr> = None;
    let mut healed = false;
    for _ in 0..MAX_PASSES {
        let ever_observed = EverObserved::new();
        let observed =
            observe(&exec, &socket, SESSION, &ever_observed).expect("observe the real tmux socket");
        match plan::compute_converge_plan(&desired(), &observed) {
            Ok(_) => {
                healed = true;
                break;
            }
            Err(err) => last_err = Some(err),
        }
    }
    assert!(
        healed,
        "a converge pass killed between the two window tags must self-heal within {MAX_PASSES} passes \
         instead of leaving the company permanently unreconcilable; last error: {last_err:?}"
    );
}

/// **INVALIDATED BY THE FIX, DELIBERATELY, IN THE SAME COMMIT AS THE FIX.**
/// This test used to be a characterization control pinning today's broken
/// mechanism: it asserted that `compute_converge_plan` returned
/// `Err(WindowNotFullyTagged)` on THIS pass and recurred identically on a
/// SECOND pass over the same unchanged observation — i.e. "permanently
/// wedged, no self-heal." That old body is preserved verbatim below in a
/// comment so a reader can see exactly what the fix inverted.
///
/// It now asserts the opposite, in ONE pass: `observe()` removes the torn
/// window (as a side effect of its own read — see the module doc) before
/// returning, so the very first pass after the crash — no retries needed —
/// observes zero windows and plans a clean, fully-tagged replacement. The
/// pre-reap torn state is confirmed with a RAW tmux read (`raw_windows`),
/// since `observe()` itself can no longer be used to see it — its very first
/// call already reaps.
///
/// ```text
/// // THE OLD BODY (would now fail if run against the fixed code):
/// let observed = observe(&exec, &socket, SESSION).expect(...);
/// assert_eq!(observed.windows.len(), 1, "exactly one window was minted before the crash");
/// assert!(observed.windows[0].logical_id.is_empty(), ...);
/// let result = plan::compute_converge_plan(&desired(), &observed);
/// let err = result.expect_err("expected HEAD to fail closed on a partially-tagged window");
/// assert!(matches!(err, plan::PlanErr::WindowNotFullyTagged { .. }));
/// let second = plan::compute_converge_plan(&desired(), &observed);
/// assert!(matches!(second, Err(plan::PlanErr::WindowNotFullyTagged { .. })), "must recur identically");
/// ```
#[test]
fn window_tag_crash_no_longer_wedges_the_company() {
    let socket_name = unique_socket("window");
    crash_at(&socket_name, "interpret:tag_window:after_organization");
    let _guard = SocketGuard(socket_name.clone());

    // Confirm the crash still lands exactly where it used to (the marker
    // change didn't move the pause point), read directly from tmux so
    // `observe()`'s own reap doesn't consume the evidence before we can see it.
    // The world now also contains the uncrashed setup (theo in `eng`); exactly
    // the crashed mint is torn.
    let windows = raw_windows(&socket_name, SESSION);
    assert_eq!(windows.len(), 2, "the setup window plus the crashed mint: {windows:?}");
    let torn: Vec<_> = windows.iter().filter(|(_, logical)| logical.is_empty()).collect();
    assert_eq!(
        torn.len(),
        1,
        "the crash must still land strictly between the two window tags: {windows:?}"
    );
    assert!(
        windows.iter().any(|(_, logical)| logical == "eng"),
        "the setup window is intact: {windows:?}"
    );

    // THE FIX, in one pass: `observe()` reaps before returning.
    let socket = Socket(socket_name.clone());
    let exec = executor();
    let ever_observed = EverObserved::new();
    let observed = observe(&exec, &socket, SESSION, &ever_observed)
        .expect("observe reaps the torn window as it reads");
    assert_eq!(
        observed.windows.len(),
        1,
        "the torn window must be gone after observe(), with the setup window surviving: {:?}",
        observed.windows
    );
    assert_eq!(observed.windows[0].logical_id, "eng");

    let plan = plan::compute_converge_plan(&desired(), &observed).expect(
        "with the torn window gone, planning must succeed on the very first post-crash pass",
    );
    assert!(
        plan.warnings.is_empty(),
        "a clean re-plan carries no quarantine warnings: {:?}",
        plan.warnings
    );
    let plans_a_fresh_window_for_vera = plan
        .steps
        .iter()
        .any(|step| matches!(step, plan::Step::CreateSession { first } | plan::Step::CreateWindowWithSpawn { first, .. } if first.person_id == "vera"));
    assert!(
        plans_a_fresh_window_for_vera,
        "expected a clean single mint for vera, got steps: {:?}",
        plan.steps
    );
}

/// **THE RESTARTABILITY CONTROL for the pane half.** Same acceptance bar as
/// the window control above, run to completion: apply whatever plan each
/// subsequent (uncrashed) pass computes, and assert the company converges to
/// EXACTLY the desired topology — one pane for `vera`, zero quarantine
/// warnings. **Red at HEAD**, because the torn first pane is quarantined
/// forever (it is still `person_id`-desired, so `reapable_orphan_pane` never
/// claims it) while a second, valid pane gets minted alongside it: the
/// company never stops warning and never converges to the desired one-pane
/// shape. This is worse than a cosmetic warning — it is a real orphaned tmux
/// pane consuming a slot in the window's layout forever.
///
/// **What a wrong-but-plausible fix does to this test:** a fix that merely
/// suppresses the WARNING STRING (rather than reaping or completing the torn
/// pane) would make `next.warnings` empty without changing `observed.panes`,
/// so it would still fail on `assert_eq!(vera_panes.len(), 1, ...)` below —
/// the pane count, not the warning text, is the real assertion.
#[test]
fn pane_tag_crash_self_heals_without_a_permanent_duplicate() {
    assert!(
        chief_cli::sidebar::rail_program().is_none(),
        "an integration-test executable does not implement `chief sidebar` and must never be \
         minted as a rail beside the crash fixture"
    );
    let socket_name = unique_socket("pane-heal");
    crash_at(&socket_name, "interpret:tag_pane:after_window");
    let _guard = SocketGuard(socket_name.clone());

    let socket = Socket(socket_name.clone());
    let exec = executor();
    let launches = launches();

    const MAX_PASSES: u32 = 5;
    let mut converged = false;
    let mut last_warnings: Vec<String> = Vec::new();
    let mut last_vera_panes = 0usize;
    let mut apply_failure: Option<String> = None;
    for _ in 0..MAX_PASSES {
        let ever_observed = EverObserved::new();
        let observed =
            observe(&exec, &socket, SESSION, &ever_observed).expect("observe the real tmux socket");
        let plan = plan::compute_converge_plan(&desired(), &observed)
            .expect("a torn pane must be quarantined, not fatal");
        last_warnings = plan.warnings.clone();
        last_vera_panes = observed
            .panes
            .iter()
            .filter(|p| p.person_id == "vera" || p.person_id.is_empty())
            .count();
        if plan.warnings.is_empty() && last_vera_panes == 1 {
            converged = true;
            break;
        }
        if plan.steps.is_empty() {
            // Nothing left to do and still not converged: no amount of
            // further passes will change this.
            break;
        }
        let report = apply_plan(&exec, &socket, &desired(), &observed, &launches, &plan);
        if !report.succeeded() {
            // A failed apply is itself non-convergence, not a test-harness
            // error: today the leaked torn pane doesn't just linger as a
            // cosmetic warning, it corrupts the NEXT actuation step
            // (`ApplyLayout` computes a layout for the desired pane count,
            // but the live window already carries an extra untagged pane, so
            // `select-layout` refuses with a pane-count mismatch). That is a
            // stronger failure to converge than a warning, so it counts here
            // exactly as "still not converged" and stops the loop.
            apply_failure = Some(format!("{:?}", report.failure));
            break;
        }
    }
    assert!(
        converged,
        "a converge pass killed between the WINDOW and PERSON pane tags must self-heal to exactly \
         one pane for vera and zero quarantine warnings within {MAX_PASSES} passes; last warnings: \
         {last_warnings:?}, panes touching vera/untagged: {last_vera_panes}, apply failure along the way: \
         {apply_failure:?}"
    );
}

/// **INVALIDATED BY THE FIX, DELIBERATELY, IN THE SAME COMMIT AS THE FIX.**
/// This test used to be a characterization control pinning today's broken
/// mechanism: `compute_converge_plan` correctly quarantined the torn pane
/// (that half was always right) but, because `vera` therefore still read as
/// un-materialized, minted a SECOND pane for the same person alongside the
/// permanently-quarantined first one — a real duplicate, produced by ONE
/// interpreter crashing mid-tag, no second actuator involved. That old body
/// is preserved verbatim below so a reader can see exactly what the fix
/// inverted.
///
/// It now asserts the opposite, in ONE pass: `observe()` removes the torn
/// pane (and, since it was the window's only pane, tmux cascades the removal
/// to the window and then the session — the same cascade the window test's
/// fix relies on) before returning, so the very first pass after the crash
/// plans a single clean mint for vera, no duplicate, no quarantine warning.
/// The pre-reap torn state is confirmed with raw tmux reads, for the same
/// reason as the window test above.
///
/// ```text
/// // THE OLD BODY (would now fail if run against the fixed code):
/// let observed = observe(&exec, &socket, SESSION).expect(...);
/// assert_eq!(observed.panes.len(), 1, "exactly one pane was minted before the crash");
/// assert!(observed.panes[0].person_id.is_empty(), ...);
/// let next = plan::compute_converge_plan(&desired(), &observed).expect(...);
/// assert!(next.warnings.iter().any(|w| w.contains("not fully ownership-tagged")));
/// let mints_another_pane_for_vera = next.steps.iter().any(|step| matches!(step,
///     plan::Step::CreateWindowWithSpawn { first, .. } | plan::Step::SplitPane { spec: first, .. }
///         if first.person_id == "vera"));
/// assert!(mints_another_pane_for_vera, "expected a SECOND pane for vera (the duplicate)");
/// ```
#[test]
fn pane_tag_crash_no_longer_mints_a_duplicate() {
    let socket_name = unique_socket("pane");
    crash_at(&socket_name, "interpret:tag_pane:after_window");
    let _guard = SocketGuard(socket_name.clone());

    // Confirm the crash still lands exactly where it used to, via a raw read.
    // The world also contains the uncrashed setup (theo in `eng`, fully
    // tagged); exactly the crashed mint's pane is torn.
    let windows = raw_windows(&socket_name, SESSION);
    assert!(
        windows.iter().any(|(_, logical)| logical == "ops"),
        "the crashed window finished tagging before the crash: {windows:?}"
    );
    let panes = raw_panes(&socket_name, SESSION);
    assert_eq!(panes.len(), 2, "the setup pane plus the crashed mint: {panes:?}");
    let torn: Vec<_> = panes.iter().filter(|(_, person)| person.is_empty()).collect();
    assert_eq!(
        torn.len(),
        1,
        "the crash must still land strictly before the PERSON tag: {panes:?}"
    );
    assert!(
        panes.iter().any(|(_, person)| person == "theo"),
        "the setup pane is intact: {panes:?}"
    );

    // THE FIX, in one pass: `observe()` reaps before returning.
    let socket = Socket(socket_name.clone());
    let exec = executor();
    let ever_observed = EverObserved::new();
    let observed = observe(&exec, &socket, SESSION, &ever_observed)
        .expect("observe reaps the torn pane as it reads");
    assert_eq!(
        observed.panes.len(),
        1,
        "the torn pane must be gone after observe(), with the setup pane surviving: {:?}",
        observed.panes
    );
    assert_eq!(observed.panes[0].person_id, "theo");

    let plan = plan::compute_converge_plan(&desired(), &observed)
        .expect("with the torn pane gone, planning must succeed on the very first post-crash pass");
    assert!(
        plan.warnings.is_empty(),
        "a clean re-plan carries no quarantine warnings: {:?}",
        plan.warnings
    );
    let mints_exactly_one_pane_for_vera = plan.steps.iter().any(|step| {
        matches!(
            step,
            plan::Step::CreateSession { first }
            | plan::Step::CreateWindowWithSpawn { first, .. }
            | plan::Step::SplitPane { spec: first, .. }
                if first.person_id == "vera"
        )
    });
    assert!(
        mints_exactly_one_pane_for_vera,
        "expected a clean single mint for vera, got steps: {:?}",
        plan.steps
    );
}
