//! The client's own host vocabulary: the argv it runs, what came back, and how
//! a step failed (#751/P8).
//!
//! # Why these types are declared here rather than imported
//!
//! They were `chiefd_host::executor`'s. They are not any more, and the reason
//! is the whole packet: **this crate links none of `chiefd-core`,
//! `chiefd-host`, `chiefd-api` or `chiefd-daemon`**, enforced in both
//! directions by `scripts/test/backend-tmux-boundary.test.mjs`. A type that
//! names a tmux socket, a pane id and a tmux argv cannot live in a backend
//! crate under the mandate, and it cannot be imported from one either — so it
//! is re-declared, exactly as [`crate::roster`] re-declares the roster wire
//! shape rather than importing it.
//!
//! # Why the seam could not simply be abstracted
//!
//! [`TmuxRunner`](super::runner::TmuxRunner) is an **emulation** seam, not an
//! abstraction seam: it takes tmux argv, returns tmux exit codes and
//! `#{pane_id}` stdout, and [`trust`](super::trust) classifies tmux's literal
//! stderr strings (`server exited unexpectedly`, `can't find session`, `no
//! server running`). Nothing that does not speak tmux can sit behind it, so
//! there is no adapter that would have let these files stay where they were.
//! They had to move, and their vocabulary with them.

// `HostErr`, `Pid` and `ProcIdentity` are NOT declared here any more. They are
// `host-primitives`', the leaf `chiefd-host` links too, because this crate
// held one copy of each and the backend held another. They are re-exported
// rather than re-pathed, so every `crate::actuate::host::HostErr` and every
// name in `crate::actuate`'s re-export block still resolves.
//
// ONE THING CHANGED IN THE MOVE, and it is the only semantic change in the
// packet: `HostErr::Untrusted`'s `reason` was `&'static str` here and `String`
// in the backend, whose untrusted reasons arrive as prose on the wire and
// therefore cannot be compile-time strings. `String` is the shape that
// survived, so the literals this crate passes now convert at the construction
// site. Every rendered message is unchanged.
pub use host_primitives::{HostErr, Pid, ProcIdentity};

/// A tmux server socket name (`tmux -L <name>`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Socket(pub String);

/// A tmux pane identifier (`%12`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneId(pub String);

/// A tmux invocation, as an argv run verbatim after `tmux -L <socket>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxCmd {
    /// Arguments after `tmux -L <socket>`.
    pub argv: Vec<String>,
}

/// Captured result of a tmux invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxOut {
    /// Exit status.
    pub status: i32,
    /// Standard output.
    pub stdout: String,
    /// Standard error, already redacted.
    pub stderr: String,
}

/// Everything needed to spawn one pane: window, tags, command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanePlan {
    /// Target tmux server.
    pub socket: Socket,
    /// Session name.
    pub session: String,
    /// Window name.
    pub window: String,
    /// The command argv the pane runs.
    pub argv: Vec<String>,
    /// Pane tags the ownership audit later matches against.
    pub tags: Vec<(String, String)>,
}

/// Everything one `display-message` reports about a managed pane, read in a
/// single call so the pid and the ownership tags describe the *same*
/// observation — reading them separately admits a window in which a
/// `respawn-pane` lands between two reads.
///
/// Every field is required. A pane missing any tag is *not* a pane with an
/// empty tag — it is a pane whose ownership cannot be established, and the
/// executor reports an error rather than a partly-filled struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneIdentity {
    /// The pane observed.
    pub pane: PaneId,
    /// Its process id, as of this observation. Never a spawn-time record.
    pub pid: Pid,
    /// `#{session_name}`.
    pub session: String,
    /// `@organization_id` — the company slug the pane was tagged for.
    pub organization: String,
    /// `@organization_person_id`.
    pub person_id: String,
    /// `@organization_launch_hash` — the derived hash this pane was launched
    /// at. Compared for equality against chiefd's desired hash and never
    /// parsed.
    pub launch_hash: String,
}

/// The materialization vocabulary, owned by the leaf both actuators link.
///
/// These three were declared here AND in `chiefd_host::executor`, identically,
/// and they are re-exported rather than imported at each call site so that
/// every existing `crate::actuate::host::MaterializePlan` path still resolves.
pub use host_primitives::materialize::{DriftReport, MaterializeFile, MaterializePlan};

/// The actuator's only door to the machine.
///
/// Object-safe on purpose: the actuation layer holds a `&dyn HostExecutor` so a
/// fake can be substituted without generics leaking through every signature.
///
/// This is a second declaration of what was `chiefd_host::executor`'s trait,
/// trimmed to what the client actually calls. It is not imported, for the
/// reason in this module's own doc comment.
pub trait HostExecutor: Send + Sync {
    /// Run a tmux command against a specific server socket.
    ///
    /// # Errors
    /// See [`HostErr`]; transient tmux errors surface as [`HostErr::Untrusted`]
    /// and must never be read as takeover permission.
    fn tmux(&self, socket: &Socket, cmd: TmuxCmd) -> Result<TmuxOut, HostErr>;

    /// Wait between two bounded observations of live host state.
    ///
    /// The production executor sleeps. Test executors record the request and
    /// return at once, so readiness loops stay deterministic.
    fn wait(&self, duration: std::time::Duration);

    /// Spawn a pane. Via tmux, so the pane outlives the actuator.
    ///
    /// # Errors
    /// See [`HostErr`].
    fn spawn_pane(&self, plan: &PanePlan) -> Result<PaneId, HostErr>;

    /// Read a pane's pid *now* (`display-message -p -F '#{pane_pid}'`).
    ///
    /// # Errors
    /// See [`HostErr`].
    fn pane_pid(&self, socket: &Socket, pane: &PaneId) -> Result<Pid, HostErr>;

    /// Read a pane's pid **and** ownership tags in one observation.
    ///
    /// The pid it returns is the pid tmux reports *now*, and the tags describe
    /// that same observation — a `respawn-pane` cannot land between them and
    /// pair a fresh pid with stale tags.
    ///
    /// # Errors
    /// [`HostErr::ToolFailed`] when the pane answered with incomplete ownership
    /// tags; [`HostErr::Untrusted`] when tmux did not answer at all — which is
    /// never evidence about the pane.
    fn pane_identity(&self, socket: &Socket, pane: &PaneId) -> Result<PaneIdentity, HostErr>;

    /// Read-only ownership audit of a session: proven presence/absence plus the
    /// fully-tagged windows and panes this company owns.
    ///
    /// # Errors
    /// [`HostErr::Untrusted`] when presence could not be proven (a transient
    /// tmux condition is never read as absence); [`HostErr::ToolFailed`] when
    /// the session is owned by another company or an object inside it is only
    /// partly tagged. A caller must distinguish these from an *observed-empty*
    /// session (`Ok` with no panes) and never fabricate the latter from the
    /// former.
    fn audit_session(
        &self,
        socket: &Socket,
        session: &str,
        organization: &str,
    ) -> Result<super::SessionAudit, HostErr>;

    /// The tmux pane ids in a session that tmux reports `#{pane_dead}` for.
    ///
    /// A dead tagged pane is a distinct signal from a missing one: the pane
    /// still exists in the session but its process has exited.
    ///
    /// # Errors
    /// [`HostErr::ToolFailed`] when tmux answered no (e.g. the session is
    /// absent); [`HostErr::Untrusted`] when tmux did not answer. Never an empty
    /// vector standing in for an unanswered read.
    fn dead_pane_ids(&self, socket: &Socket, session: &str) -> Result<Vec<PaneId>, HostErr>;

    /// Read a process's kernel identity (pid + start time).
    ///
    /// # Errors
    /// See [`HostErr`].
    fn proc_identity(&self, pid: Pid) -> Result<ProcIdentity, HostErr>;

    /// Whether `child` descends from `ancestor` in the process tree.
    ///
    /// # Errors
    /// See [`HostErr`].
    fn descends_from(&self, child: Pid, ancestor: Pid) -> Result<bool, HostErr>;

    /// Apply an idempotent, replayable materialization plan.
    ///
    /// # Errors
    /// See [`HostErr`].
    fn materialize(&self, plan: &MaterializePlan) -> Result<DriftReport, HostErr>;

    /// Spawn a **detached** background worker and return its pid.
    ///
    /// A default is provided so an executor that never dispatches workers (an
    /// observation-only double) compiles unchanged. The default **refuses**
    /// rather than silently succeeding — a "spawn" that launched nothing must
    /// never be mistaken for a running worker.
    ///
    /// # Errors
    /// [`HostErr::ToolUnavailable`] when the program could not be executed.
    fn spawn_detached(&self, argv: &[String], env: &[(String, String)]) -> Result<Pid, HostErr> {
        let _ = (argv, env);
        Err(HostErr::ToolUnavailable {
            tool: "spawn_detached",
            detail: "this executor does not spawn detached workers".to_string(),
        })
    }
}

// TOMBSTONE: `ExecutorTmux`, the adapter that let the actuator issue the rail's
// own tmux verbs through its `HostExecutor`. It existed for exactly one caller
// — the actuator publishing `sidebar_options::COMPANY` into a session option
// and ringing every rail's doorbell — and Stage 3 of that work
// deleted both. The session brain lives in that same process now and holds its
// own `ControlTransport`, so there is nothing left to adapt.

#[cfg(test)]
mod tests {
    use super::*;

    // The `HostErr::Untrusted` variant test moved with the type, to
    // `host_primitives::error`. One definition, one test — this crate had the
    // second copy of both.

    #[test]
    fn host_executor_is_object_safe() {
        fn assert_object_safe(_: Option<&dyn HostExecutor>) {}
        assert_object_safe(None);
    }
}
