//! The host-effect seam `chiefd-host` still owns, and the one it no longer does.
//!
//! #751/P8-P10: every PANE verb left this trait. `runtime()`, `spawn_pane`,
//! `pane_pid`, `pane_identity`, `audit_session` and `dead_pane_ids` — and the
//! `Socket` / `PaneId` / `PanePlan` / `PaneIdentity` / `RuntimeCmd` / `RuntimeOut`
//! vocabulary they spoke — belong to `chief-cli` now. They had to: their seam was
//! an EMULATION seam (one multiplexer's argv in, its exit codes and its literal
//! stderr out), so nothing that does not speak that one program could ever have
//! sat behind it, and a backend that keeps one is a backend with a display
//! hard-coded into it.
//!
//! What is left is the host effect that has nothing to do with a display, and
//! that eighteen call sites in this crate legitimately need: materialize a file
//! tree, spawn a detached worker,
//! read a process's kernel identity, walk process ancestry, attest the pinned pi
//! build. None of those knows what a pane is, and none can move to a client,
//! because they are effects the DAEMON has to perform.
//!
//! Two host seams in one workspace is therefore the correct shape, not a
//! duplication: they are different seams. The rule that separates them is the
//! one line the whole workstream exists to hold — **chiefd decides WHO runs; the
//! client decides WHERE it is displayed.** A pane verb reappearing on this trait
//! is the regression, and it is meant to look obviously wrong here.

// `HostErr`, `Pid` and `ProcIdentity` are NOT declared here any more. They are
// `host-primitives`', the leaf `chief-cli` links too, because this crate held
// one copy of each and the client held another — and by layer 2 they had
// already started to disagree (`HostErr::Untrusted`'s `reason` was
// `&'static str` on the client side). Re-exported rather than re-pathed so
// every `crate::executor::HostErr` in this crate still resolves.
pub use host_primitives::{HostErr, Pid, ProcIdentity};

/// The materialization vocabulary, owned by the leaf both actuators link.
///
/// These three were declared here AND in `chief_cli::actuate::host`,
/// identically, and they are re-exported rather than imported at each call
/// site so that every existing `crate::executor::MaterializePlan` path — and
/// `chiefd_host::executor::MaterializeFile`, which the crash test uses — still
/// resolves.
pub use host_primitives::materialize::{DriftReport, MaterializeFile, MaterializePlan};

/// The host effects chiefd performs for itself.
///
/// Object-safe on purpose: the store layer holds a `&dyn HostExecutor` so a
/// fake can be substituted without generics leaking through every signature.
///
/// #751/P8-P10: this used to be titled "chiefd's only door to the machine",
/// and it no longer is — the operator client has its own door, and every pane
/// verb went through it. What remains here is the half a daemon cannot
/// delegate.
pub trait HostExecutor: Send + Sync {
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
    /// The host hands a worker's argv (with `env` overrides layered over the
    /// inherited environment) here. The child is launched into a *new process
    /// group* with stdio discarded and is never waited on, so it outlives a
    /// chiefd restart exactly as a runtime pane does — the `unsafe`-free port
    /// of the TypeScript `spawn(execPath, argv, { detached: true, stdio:
    /// "ignore" })` followed by `child.unref()`.
    ///
    /// A default is provided so an executor that never dispatches workers (an
    /// auth-only or observation-only double) compiles unchanged; the real and
    /// fake executors both override it. The default **refuses** rather than
    /// silently succeeding — a "spawn" that launched nothing must never be
    /// mistaken for a lease handed to a running worker.
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

#[cfg(test)]
mod tests {
    use super::*;

    // The `HostErr::Untrusted` variant test moved with the type, to
    // `host_primitives::error`. It is not deleted, and it is not duplicated
    // here — one definition, one test.

    #[test]
    fn host_executor_is_object_safe() {
        fn assert_object_safe(_: Option<&dyn HostExecutor>) {}
        assert_object_safe(None);
    }
}
