//! [`RealHostExecutor`] — the production [`HostExecutor`].
//!
//! Composition only: every method delegates to the module that owns the
//! effect ([`crate::actuate::exec`], [`crate::proc`], [`crate::files`]).
//! Keeping the composition free of logic is what makes "the fake and the real
//! executor disagree" a reviewable question rather than a discovery in
//! production.
//!
//! The tmux runner and waiter are type parameters so the e2e harness can pin a
//! tmux binary and a throwaway `-L` socket without a second implementation of
//! the trait.

use crate::actuate::exec::TmuxHost;
use crate::actuate::host::{
    DriftReport, HostErr, HostExecutor, MaterializePlan, PaneId, PaneIdentity, PanePlan, Pid,
    ProcIdentity, Socket, TmuxCmd, TmuxOut,
};
use crate::actuate::runner::{SystemTmuxRunner, ThreadWaiter, TmuxRunner, Waiter};
use crate::actuate::SessionAudit;
use crate::control::ControlTransport;
use crate::proc::ProcReader;

/// The production executor.
#[derive(Debug)]
pub struct RealHostExecutor<R: TmuxRunner = SystemTmuxRunner, W: Waiter = ThreadWaiter> {
    tmux: TmuxHost<R, W>,
    proc: ProcReader,
}

impl RealHostExecutor<SystemTmuxRunner, ThreadWaiter> {
    /// The executor the client runs with: the `tmux` on `PATH` and a real
    /// `/proc`.
    #[must_use]
    pub fn production() -> Self {
        Self {
            tmux: TmuxHost::new(SystemTmuxRunner::default(), ThreadWaiter),
            proc: ProcReader::default(),
        }
    }
}

impl RealHostExecutor<ControlTransport, ThreadWaiter> {
    /// The executor the resident actuator runs with: one persistent tmux
    /// control-mode client instead of one process per command.
    ///
    /// The actuator is the heaviest tmux caller in the product — a pass is
    /// many invocations, each of which used to be a fork, an exec and a fresh
    /// socket connection at ~25 ms apiece. The transport keeps the client
    /// open, so the same pass pays that once.
    ///
    /// It needs the SESSION as well as the socket because control mode is an
    /// attached client and an attached client has a session. `session` is the
    /// company session the actuator already targets, so nothing new is
    /// created and no relative target resolves anywhere different.
    ///
    /// When control mode cannot be established this behaves exactly as
    /// [`RealHostExecutor::production`] does, command for command.
    #[must_use]
    pub fn control_mode(socket: Socket, session: String) -> Self {
        Self {
            tmux: TmuxHost::new(ControlTransport::new(socket, session), ThreadWaiter),
            proc: ProcReader::default(),
        }
    }
}

impl<R: TmuxRunner, W: Waiter> RealHostExecutor<R, W> {
    /// Compose an executor from explicit parts (the e2e harness does this).
    pub const fn new(tmux: TmuxHost<R, W>, proc: ProcReader) -> Self {
        Self { tmux, proc }
    }

    /// The tmux layer, for the ownership audit and presence checks that are
    /// richer than the trait's single `tmux()` method.
    pub const fn tmux_host(&self) -> &TmuxHost<R, W> {
        &self.tmux
    }
}

impl<R: TmuxRunner, W: Waiter> HostExecutor for RealHostExecutor<R, W> {
    fn tmux(&self, socket: &Socket, cmd: TmuxCmd) -> Result<TmuxOut, HostErr> {
        self.tmux.run_retrying_transients(socket, &cmd)
    }

    fn wait(&self, duration: std::time::Duration) {
        self.tmux.waiter().wait(duration);
    }

    fn spawn_pane(&self, plan: &PanePlan) -> Result<PaneId, HostErr> {
        self.tmux.spawn_pane(&plan.socket, &plan.session, &plan.window, &plan.argv, &plan.tags)
    }

    fn pane_pid(&self, socket: &Socket, pane: &PaneId) -> Result<Pid, HostErr> {
        self.tmux.pane_pid(socket, pane)
    }

    fn pane_identity(&self, socket: &Socket, pane: &PaneId) -> Result<PaneIdentity, HostErr> {
        self.tmux.pane_identity(socket, pane)
    }

    fn audit_session(
        &self,
        socket: &Socket,
        session: &str,
        organization: &str,
    ) -> Result<SessionAudit, HostErr> {
        self.tmux.audit_session(socket, session, organization)
    }

    fn dead_pane_ids(&self, socket: &Socket, session: &str) -> Result<Vec<PaneId>, HostErr> {
        self.tmux.dead_pane_ids(socket, session)
    }

    fn proc_identity(&self, pid: Pid) -> Result<ProcIdentity, HostErr> {
        self.proc.identity(pid)
    }

    fn descends_from(&self, child: Pid, ancestor: Pid) -> Result<bool, HostErr> {
        self.proc.descends_from(child, ancestor)
    }

    fn materialize(&self, plan: &MaterializePlan) -> Result<DriftReport, HostErr> {
        crate::files::materialize(plan)
    }

    fn spawn_detached(&self, argv: &[String], env: &[(String, String)]) -> Result<Pid, HostErr> {
        crate::spawn::spawn_detached(argv, env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_executor_satisfies_the_object_safe_seam() {
        let executor = RealHostExecutor::production();
        let as_dyn: &dyn HostExecutor = &executor;
        // /proc is real here: the client can always identify itself.
        let me = Pid(std::process::id().try_into().expect("pid fits"));
        assert_eq!(as_dyn.proc_identity(me).expect("identity").pid, me);
    }

    #[test]
    fn materialization_through_the_trait_is_the_same_convergence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let executor = RealHostExecutor::production();
        let plan = MaterializePlan {
            root: dir.path().to_path_buf(),
            files: vec![crate::actuate::host::MaterializeFile {
                relative_path: "settings.json".into(),
                contents: "{}\n".into(),
                mode: 0o600,
            }],
        };
        assert_eq!(executor.materialize(&plan).expect("first").changed.len(), 1);
        let replay = executor.materialize(&plan).expect("replay");
        assert!(replay.changed.is_empty());
        assert_eq!(replay.unchanged, vec!["settings.json".to_string()]);
    }
}
