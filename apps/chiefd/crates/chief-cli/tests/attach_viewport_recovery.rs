//! An unclean shutdown must never leave a company unstartable.
//!
//! REAL tmux, a throwaway `-L` socket, and the exact race the operator hit on
//! a live box after a hard reboot:
//!
//! ```text
//! root@host:~/workspace# chief
//! have 6 panes but need 5: 6a8a,225x47,0,0{26x47,0,0,29,198x47,27,0[...]}
//! ```
//!
//! That is tmux's own `select-layout` refusal — `cmd-select-layout.c` prints
//! `<cause>: <layout>` — and it reached the operator as the WHOLE output of
//! `chief`, which then exited without attaching them to anything.
//!
//! `chief attach` publishes one ABSOLUTE layout string per managed window,
//! computed from a `list-panes -s` census and applied in a LATER tmux
//! invocation. A layout string enumerates every pane the window holds, so it
//! is appliable only to the census it came from. The actuator is a separate
//! process; a cold start after a reboot is exactly when it is minting a pane
//! every few hundred milliseconds. This test makes that concrete: the executor
//! splits a real pane into the window the moment the census has been read,
//! which is the only thing the operator's box did that a healthy box does not.
//!
//! Not simulated: real tmux answers the real `select-layout`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::unimplemented)]

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use chief_cli::actuate::host::{
    DriftReport, HostErr, HostExecutor, MaterializePlan, PaneId, PaneIdentity, PanePlan, Pid,
    ProcIdentity, Socket, TmuxCmd, TmuxOut,
};
use chief_cli::actuate::{resize_session_viewport_for_attach, AttachViewportPublication};
use chief_cli::real::RealHostExecutor;

const ORG: &str = "cobalt";
const SESSION: &str = "org-cobalt_";
const NONCE: &str = "0123456789abcdef0123456789abcdef";

fn tmux(socket: &str, argv: &[&str]) -> String {
    let out = Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(argv)
        .output()
        .expect("tmux must be on PATH for this test");
    assert!(out.status.success(), "tmux {argv:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The production executor, plus the one thing a cold-start box does that a
/// quiet box does not: a pane arrives while the publication is being built.
struct PaneArrivesMidPublication {
    inner: RealHostExecutor,
    socket: String,
    window: String,
    fired: AtomicBool,
}

impl HostExecutor for PaneArrivesMidPublication {
    fn tmux(&self, socket: &Socket, cmd: TmuxCmd) -> Result<TmuxOut, HostErr> {
        let listing = cmd.argv.first().is_some_and(|verb| verb == "list-panes");
        let out = self.inner.tmux(socket, cmd)?;
        if listing && !self.fired.swap(true, Ordering::SeqCst) {
            // A real split, in a real window, after the census has been read
            // and before it is published. This is the actuator.
            tmux(&self.socket, &["split-window", "-d", "-t", &self.window, "sleep", "600"]);
        }
        Ok(out)
    }

    fn wait(&self, duration: std::time::Duration) {
        self.inner.wait(duration);
    }
    fn spawn_pane(&self, _plan: &PanePlan) -> Result<PaneId, HostErr> {
        unimplemented!("the viewport publication spawns nothing")
    }
    fn pane_pid(&self, _socket: &Socket, _pane: &PaneId) -> Result<Pid, HostErr> {
        unimplemented!("the viewport publication reads no pid")
    }
    fn pane_identity(&self, _socket: &Socket, _pane: &PaneId) -> Result<PaneIdentity, HostErr> {
        unimplemented!("the viewport publication reads no pane identity")
    }
    fn audit_session(
        &self,
        _socket: &Socket,
        _session: &str,
        _organization: &str,
    ) -> Result<chief_cli::actuate::SessionAudit, HostErr> {
        unimplemented!("the viewport publication audits nothing")
    }
    fn dead_pane_ids(&self, _socket: &Socket, _session: &str) -> Result<Vec<PaneId>, HostErr> {
        unimplemented!("the viewport publication reaps nothing")
    }
    fn proc_identity(&self, _pid: Pid) -> Result<ProcIdentity, HostErr> {
        unimplemented!("the viewport publication reads no process")
    }
    fn descends_from(&self, _child: Pid, _ancestor: Pid) -> Result<bool, HostErr> {
        unimplemented!("the viewport publication walks no process tree")
    }
    fn materialize(&self, _plan: &MaterializePlan) -> Result<DriftReport, HostErr> {
        unimplemented!("the viewport publication materializes nothing")
    }
}

struct Server(String);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux").arg("-L").arg(&self.0).arg("kill-server").output();
    }
}

/// Stand one managed company window up on a throwaway server: a tagged
/// session, a tagged window, a tagged rail and two body panes.
fn company(socket: &str) -> String {
    tmux(socket, &["new-session", "-d", "-s", SESSION, "-x", "225", "-y", "47", "sleep", "600"]);
    tmux(socket, &["set-option", "-t", SESSION, "@organization_id", ORG]);
    tmux(socket, &["set-option", "-t", SESSION, "@chief_viewport_topology_epoch", "7"]);
    tmux(socket, &["set-option", "-t", SESSION, "@chief_viewport_server_nonce", NONCE]);
    let window = tmux(socket, &["display-message", "-p", "-t", SESSION, "#{window_id}"]);
    tmux(socket, &["set-option", "-w", "-t", &window, "@organization_window_id", "engineering"]);
    tmux(socket, &["split-window", "-d", "-t", &window, "sleep", "600"]);
    let rail = tmux(
        socket,
        &[
            "split-window",
            "-h",
            "-b",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &window,
            "sleep",
            "600",
        ],
    );
    tmux(socket, &["set-option", "-p", "-t", &rail, "@organization_sidebar", "1"]);
    window
}

#[test]
fn a_pane_that_arrives_mid_publication_never_makes_chief_die_with_a_tmux_error() {
    let socket = format!("chief-viewport-race-{}", std::process::id());
    let _server = Server(socket.clone());
    let window = company(&socket);

    let executor = PaneArrivesMidPublication {
        inner: RealHostExecutor::production(),
        socket: socket.clone(),
        window: window.clone(),
        fired: AtomicBool::new(false),
    };

    let published = resize_session_viewport_for_attach(
        &executor,
        &Socket(socket.clone()),
        SESSION,
        ORG,
        7,
        NONCE,
        (225, 47),
    );

    // On the tree that shipped to the operator this is
    // `Err("have 4 panes but need 3: <layout>")` — tmux's own words, straight
    // out of `chief`, and the company never comes up.
    let published = published.unwrap_or_else(|error| {
        panic!("a pane arriving mid-publication must never refuse the attach: {error}")
    });
    assert!(
        matches!(published, AttachViewportPublication::Applied(_)),
        "the publication is still the current authority: {published:?}"
    );
    assert_eq!(
        tmux(&socket, &["display-message", "-p", "-t", &window, "#{window_panes}"]),
        "4",
        "the pane that arrived is still there — it is skipped, never killed",
    );
}
