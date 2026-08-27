//! Crash-injection acceptance control for the §2.0(2) ONE SHOT creation fix
//! (architecture-audit F12 / migration Step 2, TESTING.md §4.3).
//!
//! **Status: this is the ACCEPTANCE test for the landed fix — it is green
//! because the tear no longer exists.** It replaces the eng-d5/audit-u4
//! characterization control (`d5a10a3e` + `6ce53c37` + header `175506dd`),
//! which measured the pre-fix wedge: `create_session` ran `new-session` (zero
//! tags) then `tag_session` → `tag_window` ×2 → `tag_pane` ×4 as separate
//! round-trips, so a SIGKILL in that window left an untagged session that
//! failed `PlanErr::SessionOwnership{found:"missing"}` identically across 5
//! passes with no repair path (F12, CONFIRMED on 3 boxes). The sibling
//! `create_window_by_move` tagged LAST — a crash left a zero-tag window
//! invisible to every torn-object detector, with a duplicate minted next pass
//! and a zombie bootstrap `sleep 3600` leaked.
//!
//! The fix: identity rides the CREATING tmux command. Both steps now issue
//! ONE tmux client invocation whose argv is a `;`-separated command list
//! (mint first, every `set-option` identity tag after it). The tmux client
//! transmits the whole argv as one message and the single-threaded server
//! executes the list end-to-end once received, so a SIGKILL of the chiefd
//! process lands either BEFORE the message was sent (nothing exists) or AFTER
//! it (the server completes the whole list: every minted object fully
//! identified). `create_window_by_move` additionally collapses to `break-pane`
//! — the move and the window mint are one server-side operation, so there is
//! no bootstrap pane to leak at all.
//!
//! The property under test, per the operator's acceptance: **a SIGKILL at any
//! point in creation leaves either nothing or a fully-identified object —
//! never a torn one.** The two pause points per step bracket the single
//! invocation, which is every point a crash CAN land: inside the message is
//! server-side and atomic from this process's perspective (a partial write is
//! discarded with the dead client's socket; a received message runs to
//! completion). Real tmux, real SIGKILL, a dedicated throwaway `-L` socket —
//! never the operator's server; each test kills only the server it created.

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

const CRASH_SOCKET: &str = "CHIEFD_INTERPRET_SESSION_CRASH_SOCKET";
const CRASH_AT: &str = "CHIEFD_INTERPRET_SESSION_CRASH_AT";

const SESSION_BEFORE_MINT: &str = "interpret:create_session:before_mint";
const SESSION_AFTER_MINT: &str = "interpret:create_session:after_mint";
const MOVE_BEFORE_MINT: &str = "interpret:create_window_by_move:before_mint";
const MOVE_AFTER_MINT: &str = "interpret:create_window_by_move:after_mint";

const ORG: &str = "cobalt";
const SESSION: &str = "cobalt-session";

fn launch_spec() -> LaunchSpec {
    LaunchSpec {
        pi_binary: PathBuf::from("/opt/pi/bin/pi"),
        pi_home: PathBuf::from("/data/cobalt/.chief/agent/vera"),
        workspace: PathBuf::from("/data/cobalt/people/vera/workspace"),
        display_name: "Cobalt · vera".into(),
        person_name: "vera".into(),
        accent: Some("#3c7adf".into()),
        tools: vec!["read".into()],
        extensions: Vec::new(),
        session: None,
        pending_mail: false,
        env: vec![("ORG_LAUNCHER_ORGANIZATION".into(), ORG.into())],
    }
}

fn launches() -> BTreeMap<String, LaunchSpec> {
    BTreeMap::from([("vera".to_owned(), launch_spec())])
}

fn desired() -> placement::Topology {
    placement::Topology {
        organization: ORG.into(),
        session: SESSION.into(),
        windows: vec![placement::Window {
            logical_id: "eng".into(),
            name: "engineering".into(),
            panes: vec![placement::Pane {
                person_id: "vera".into(),
                launch_hash: "hash-2".into(),
                order: 0,
            }],
        }],
        known_person_ids: Default::default(),
    }
}

/// The post-move desired topology: vera relocated from `eng` to a new `ops`
/// window (which is what makes the planner's `CreateWindowByMove` shape).
fn desired_after_move() -> placement::Topology {
    placement::Topology {
        organization: ORG.into(),
        session: SESSION.into(),
        windows: vec![
            placement::Window {
                logical_id: "eng".into(),
                name: "engineering".into(),
                panes: Vec::new(),
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
            first: plan::SpawnSpec { person_id: "vera".into(), launch_hash: "hash-2".to_owned() },
        }],
        predicted_respawn_persons: Vec::new(),
        predicted_kill_panes: Vec::new(),
        warnings: Vec::new(),
        ..Default::default()
    }
}

// --- the children ------------------------------------------------------

fn install_crash_hook(watch_for: &str) {
    let watch_for = watch_for.to_owned();
    chief_cli::pause::install(move |name| {
        if name == watch_for {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::SIGKILL);
        }
    });
}

/// The crashing side for the create_session points. A no-op unless the env
/// vars are set, so it is harmless as an ordinary test.
#[test]
fn crash_child() {
    let (Ok(socket_name), Ok(crash_at)) = (std::env::var(CRASH_SOCKET), std::env::var(CRASH_AT))
    else {
        return;
    };
    if crash_at != SESSION_BEFORE_MINT && crash_at != SESSION_AFTER_MINT {
        return;
    }
    install_crash_hook(&crash_at);

    // Same seeding rationale as tests/interpret_crash.rs: the spawned pane
    // names a nonexistent `pi` binary on purpose, so `remain-on-exit` is
    // required to stop the exec failure from tearing down the pane -> window
    // -> session -> (empty) server before the pause point is ever reached.
    seed_server(&socket_name);

    let exec = executor();
    let socket = Socket(socket_name);
    let report = apply_plan(
        &exec,
        &socket,
        &desired(),
        &empty_observed(),
        &launches(),
        &create_session_plan(),
    );
    panic!("the pause point {crash_at:?} was never reached; apply_plan returned {report:?}");
}

/// The crashing side for the create_window_by_move points: first materialize
/// vera in `eng` (uncrashed — the armed point names only the move step), then
/// run a hand-built `CreateWindowByMove` plan and die at the armed point.
#[test]
fn crash_move_child() {
    let (Ok(socket_name), Ok(crash_at)) = (std::env::var(CRASH_SOCKET), std::env::var(CRASH_AT))
    else {
        return;
    };
    if crash_at != MOVE_BEFORE_MINT && crash_at != MOVE_AFTER_MINT {
        return;
    }
    install_crash_hook(&crash_at);
    seed_server(&socket_name);

    let exec = executor();
    let socket = Socket(socket_name);
    let setup = apply_plan(
        &exec,
        &socket,
        &desired(),
        &empty_observed(),
        &launches(),
        &create_session_plan(),
    );
    assert!(setup.succeeded(), "the uncrashed setup apply must succeed: {:?}", setup.failure);

    let ever_observed = EverObserved::new();
    let observed = observe(&exec, &socket, SESSION, &ever_observed)
        .expect("observe the session the setup just created");
    let vera =
        observed.panes.iter().find(|pane| pane.person_id == "vera").expect("vera was materialized");
    let move_plan = plan::ConvergePlan {
        steps: vec![plan::Step::CreateWindowByMove {
            w: plan::WindowSym("ops".into()),
            name: "operations".into(),
            move_pane: plan::PaneId(vera.tmux_id.clone()),
        }],
        predicted_respawn_persons: Vec::new(),
        predicted_kill_panes: Vec::new(),
        warnings: Vec::new(),
        ..Default::default()
    };
    let report =
        apply_plan(&exec, &socket, &desired_after_move(), &observed, &launches(), &move_plan);
    panic!("the pause point {crash_at:?} was never reached; apply_plan returned {report:?}");
}

fn crash_at(child: &str, socket_name: &str, point: &str) {
    let exe = std::env::current_exe().expect("current test binary");
    let status = Command::new(exe)
        .args(["--exact", child, "--nocapture", "--test-threads=1"])
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
    format!("chiefd-interpret-session-crash-{label}-{}", std::process::id())
}

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

struct SocketGuard(String);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        kill_server(&self.0);
    }
}

/// Every object in the observation must be FULLY identified — the ONE SHOT
/// property: nothing may exist half-tagged, at any crash point.
fn assert_fully_identified(observed: &plan::ObservedTopology, context: &str) {
    assert!(observed.session_exists, "{context}: the session must exist after this crash point");
    assert_eq!(
        observed.session_organization, ORG,
        "{context}: the session must carry its organization"
    );
    for window in &observed.windows {
        assert_eq!(
            window.organization_id, ORG,
            "{context}: window {} must carry its organization",
            window.tmux_id
        );
        assert!(
            !window.logical_id.is_empty(),
            "{context}: window {} must carry its logical id",
            window.tmux_id
        );
    }
    for pane in &observed.panes {
        assert_eq!(
            pane.organization_id, ORG,
            "{context}: pane {} must carry its organization",
            pane.tmux_id
        );
        assert!(
            !pane.logical_window_id.is_empty(),
            "{context}: pane {} must carry its window",
            pane.tmux_id
        );
        assert!(
            !pane.person_id.is_empty(),
            "{context}: pane {} must carry its person",
            pane.tmux_id
        );
        assert!(
            !pane.launch_hash.is_empty(),
            "{context}: pane {} must carry its launch hash",
            pane.tmux_id
        );
    }
}

/// The wedge the pre-fix characterization measured: the planner must NEVER
/// fail closed on session ownership against a post-fix observation, pass
/// after pass (each re-observed from scratch, as a daemon restart would).
fn assert_never_wedges(
    exec: &RealHostExecutor<SystemTmuxRunner, ThreadWaiter>,
    socket: &Socket,
    desired_topology: &placement::Topology,
) {
    const MAX_PASSES: u32 = 5;
    for pass in 0..MAX_PASSES {
        // Fresh per pass on purpose: this helper's whole model is "each pass
        // re-observed from scratch, as a daemon restart would", and
        // `EverObserved` is explicitly process-scoped — a restart forgets it.
        // The assertion below reads `compute_converge_plan`, which never
        // consults the registry, so this models the restart without weakening
        // anything the helper checks.
        let ever_observed = EverObserved::new();
        let observed =
            observe(exec, socket, SESSION, &ever_observed).expect("observe the real tmux socket");
        let result = plan::compute_converge_plan(desired_topology, &observed);
        assert!(
            !matches!(&result, Err(plan::PlanErr::SessionOwnership { .. })),
            "pass {pass}: the pre-fix wedge (PlanErr::SessionOwnership) must be unreachable now, got {result:?}"
        );
    }
}

// --- the tests -----------------------------------------------------------

/// **"Nothing" arm.** SIGKILL lands BEFORE the one-shot message is sent: no
/// session, no window, no pane may exist afterwards — and a fresh (uncrashed)
/// pass then creates the whole topology fully tagged, proving the crash left
/// no debris the reconcile has to work around.
#[test]
fn crash_before_the_one_shot_leaves_nothing_then_creates_cleanly() {
    let socket_name = unique_socket("session-before");
    crash_at("crash_child", &socket_name, SESSION_BEFORE_MINT);
    let _guard = SocketGuard(socket_name.clone());

    let socket = Socket(socket_name.clone());
    let exec = executor();
    let ever_observed = EverObserved::new();
    let observed = observe(&exec, &socket, SESSION, &ever_observed)
        .expect("observe the real tmux socket after the crash");
    assert!(
        !observed.session_exists,
        "a kill before the one-shot message is sent must leave NOTHING: {observed:?}"
    );

    let report =
        apply_plan(&exec, &socket, &desired(), &observed, &launches(), &create_session_plan());
    assert!(report.succeeded(), "the recovery pass must succeed cleanly: {:?}", report.failure);
    let observed =
        observe(&exec, &socket, SESSION, &ever_observed).expect("observe after the recovery pass");
    assert_fully_identified(&observed, "recovery pass");
    assert_eq!(observed.windows.len(), 1);
    assert_eq!(observed.windows[0].logical_id, "eng");
    assert_eq!(observed.panes.len(), 1);
    assert_eq!(observed.panes[0].person_id, "vera");
    assert_eq!(observed.panes[0].launch_hash, "hash-2");
}

/// **"Fully-identified" arm.** SIGKILL lands AFTER the one-shot message was
/// sent: the server has executed the whole list, so the session, its window
/// and its pane all exist with EVERY identity tag — and the pre-fix wedge
/// (`PlanErr::SessionOwnership`, measured identical across 5 passes on 3
/// boxes) is unreachable, pass after pass.
#[test]
fn crash_after_the_one_shot_leaves_a_fully_identified_session_that_never_wedges() {
    let socket_name = unique_socket("session-after");
    crash_at("crash_child", &socket_name, SESSION_AFTER_MINT);
    let _guard = SocketGuard(socket_name.clone());

    let socket = Socket(socket_name.clone());
    let exec = executor();
    let ever_observed = EverObserved::new();
    let observed = observe(&exec, &socket, SESSION, &ever_observed)
        .expect("observe the real tmux socket after the crash");
    assert_fully_identified(&observed, "after-mint crash");
    assert_eq!(
        observed.windows.len(),
        1,
        "new-session -n mints the window atomically with the session"
    );
    assert_eq!(observed.windows[0].logical_id, "eng");
    assert_eq!(observed.panes.len(), 1);
    assert_eq!(observed.panes[0].person_id, "vera");
    assert_never_wedges(&exec, &socket, &desired());
}

/// **The move step, "nothing changed" arm.** SIGKILL lands before the
/// `break-pane` message: vera is still in her original, fully tagged `eng`
/// window — no bootstrap window, no moved pane, no debris of any kind.
#[test]
fn crash_before_the_move_one_shot_leaves_the_original_topology_intact() {
    let socket_name = unique_socket("move-before");
    crash_at("crash_move_child", &socket_name, MOVE_BEFORE_MINT);
    let _guard = SocketGuard(socket_name.clone());

    let socket = Socket(socket_name.clone());
    let exec = executor();
    let ever_observed = EverObserved::new();
    let observed = observe(&exec, &socket, SESSION, &ever_observed)
        .expect("observe the real tmux socket after the crash");
    assert_fully_identified(&observed, "before-move crash");
    assert_eq!(observed.windows.len(), 1, "nothing was minted: still exactly the setup window");
    assert_eq!(observed.windows[0].logical_id, "eng");
    assert_eq!(observed.panes.len(), 1);
    assert_eq!(observed.panes[0].person_id, "vera");
    assert_eq!(observed.panes[0].tmux_window_id, observed.windows[0].tmux_id);
}

/// **The move step, "fully-identified" arm.** SIGKILL lands after the
/// `break-pane` message: vera has moved into the freshly minted `ops` window,
/// which carries BOTH window identity tags (the tags rode the same message,
/// addressed via the moved pane's id) — the pre-fix failure (a zero-tag
/// window invisible to every torn-object detector, duplicate minted next
/// pass, zombie bootstrap process leaked) cannot exist. There is also no
/// bootstrap pane: `break-pane` needs none, so nothing leaks.
#[test]
fn crash_after_the_move_one_shot_leaves_a_fully_identified_window() {
    let socket_name = unique_socket("move-after");
    crash_at("crash_move_child", &socket_name, MOVE_AFTER_MINT);
    let _guard = SocketGuard(socket_name.clone());

    let socket = Socket(socket_name.clone());
    let exec = executor();
    let ever_observed = EverObserved::new();
    let observed = observe(&exec, &socket, SESSION, &ever_observed)
        .expect("observe the real tmux socket after the crash");
    assert_fully_identified(&observed, "after-move crash");
    assert_eq!(
        observed.windows.len(),
        1,
        "vera was eng's only pane: eng is gone, exactly one window remains"
    );
    assert_eq!(observed.windows[0].logical_id, "ops", "the minted window carries its logical id");
    assert_eq!(observed.panes.len(), 1, "no bootstrap pane exists to leak");
    assert_eq!(observed.panes[0].person_id, "vera", "the moved pane kept its identity");
    assert_eq!(observed.panes[0].tmux_window_id, observed.windows[0].tmux_id);
    assert_never_wedges(&exec, &socket, &desired_after_move());
}
