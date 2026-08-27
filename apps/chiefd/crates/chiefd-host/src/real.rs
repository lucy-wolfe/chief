//! [`RealHostExecutor`] — the production [`HostExecutor`].
//!
//! Composition only: every method delegates to the module that owns the effect
//! ([`crate::proc`], [`crate::files`], [`crate::spawn`]). Keeping
//! the composition free of logic is what makes "the fake and the real executor
//! disagree" a reviewable question rather than a discovery in production.
//!
//! #751/P8-P10 removed the type parameters. This executor used to be generic
//! over a multiplexer runner and a waiter (`RealHostExecutor<R: RuntimeRunner,
//! W: Waiter>`) purely so the e2e harness could pin a multiplexer binary and a
//! throwaway `-L` socket. No multiplexer is named here any more, so there is
//! nothing to parameterize:
//! the struct is one concrete type, which is the honest shape for a composition
//! of `/proc`, the filesystem and a pinned pi build. The parameterized executor
//! still exists — in `chief-cli`, which is where the multiplexer is.

use crate::executor::{DriftReport, HostErr, HostExecutor, MaterializePlan, Pid, ProcIdentity};
use crate::proc::ProcReader;

/// The production executor.
#[derive(Debug)]
pub struct RealHostExecutor {
    proc: ProcReader,
}

impl RealHostExecutor {
    /// The executor chiefd runs with: a real `/proc`, and the given pinned pi.
    #[must_use]
    pub fn production() -> Self {
        Self { proc: ProcReader::default() }
    }

    /// Compose an executor from explicit parts (the e2e harness does this).
    #[must_use]
    pub const fn new(proc: ProcReader) -> Self {
        Self { proc }
    }
}

impl HostExecutor for RealHostExecutor {
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
        // /proc is real here: chiefd can always identify itself.
        let me = Pid(std::process::id().try_into().expect("pid fits"));
        assert_eq!(as_dyn.proc_identity(me).expect("identity").pid, me);
    }

    #[test]
    fn materialization_through_the_trait_is_the_same_convergence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let executor = RealHostExecutor::production();
        let plan = MaterializePlan {
            root: dir.path().to_path_buf(),
            files: vec![crate::executor::MaterializeFile {
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
