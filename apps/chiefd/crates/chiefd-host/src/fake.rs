//! `FakeHostExecutor` — the unit-test seam for the host effects chiefd still owns.
//!
//! Gated behind the `test-support` cargo feature (never `#[cfg(test)]`, because
//! integration tests, the conformance runner and the e2e harness are separate
//! crates).
//!
//! #751/P8-P10: this file used to hold TWO fakes. `ScriptedRuntime` — a
//! scripted multiplexer runner that answered argv with canned exit codes and
//! stderr — moved to
//! `chief-cli` along with everything else that knows what a pane is, and so did
//! every pane setter and pane trait method that used to live on this one
//! (`set_pane_pid`, `set_pane_identity`, `set_session_audit`, `set_dead_panes`,
//! `set_audit_untrusted`, and the six `HostExecutor` verbs behind them).
//!
//! What is left fakes exactly the seam `chiefd-host` still has: materialize,
//! spawn a detached worker, read a proc identity, walk
//! ancestry, attest pi. The call log and the `failing_at` injection are the two
//! things every test here actually uses, and both survive unchanged.

use std::sync::Mutex;

use crate::executor::{DriftReport, HostErr, HostExecutor, MaterializePlan, Pid, ProcIdentity};

/// One recorded `spawn_detached`: the argv, and the env overrides layered over
/// the inherited environment.
///
/// A tuple rather than a struct because both halves are already named by the
/// trait method's own signature, and a test reads `call.0`/`call.1` next to
/// that signature.
pub type SpawnCall = (Vec<String>, Vec<(String, String)>);

/// The fake's whole mutable world. Every field is either a recording or an
/// injection; there is no scripted RESPONSE state left, because the six verbs
/// that needed scripting were pane verbs and they are `chief-cli`'s now.
#[derive(Debug, Default)]
struct FakeState {
    /// Every effect, in the order it happened.
    calls: Vec<String>,
    /// The 0-based call index that fails, if any.
    fail_at: Option<usize>,
    /// Every `spawn_detached` that actually launched.
    spawn_calls: Vec<SpawnCall>,
    /// Scripted `descends_from` verdict; `None` is "descends".
    ancestry: Option<bool>,
    /// Whether `materialize` publishes real files.
    real_filesystem: bool,
}

/// A recording, scriptable `HostExecutor`.
///
/// This is a STORE-LAYER fake. The whole warning that used to sit here is
/// obsolete, and worth recording as a tombstone rather than deleting silently:
/// it explained that this fake's blanket exit-0 answer to any multiplexer
/// command contradicted its own "provably absent" `audit_session` default,
/// which cost three tests and a misdirected investigation before the mismatch
/// was traced to the fake instead of the route under test. That whole class of
/// bug is gone, because a store-layer fake can no longer answer a display
/// question at all — there is no such verb on this trait to answer with. A fake
/// that cannot express a contradiction cannot hand one to a test.
#[derive(Debug, Default)]
pub struct FakeHostExecutor {
    state: Mutex<FakeState>,
}

impl FakeHostExecutor {
    /// A fake that succeeds at everything.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A fake whose `index`-th call (0-based) fails with
    /// [`HostErr::ToolFailed`]. Every other call succeeds, so a test can place
    /// the failure exactly where a crash or a partial publish would occur.
    #[must_use]
    pub fn failing_at(index: usize) -> Self {
        Self { state: Mutex::new(FakeState { fail_at: Some(index), ..FakeState::default() }) }
    }

    /// Make [`HostExecutor::materialize`] actually publish files, through the
    /// same [`crate::files::materialize`] the real executor uses, while every
    /// other method stays fake and every call stays recorded.
    ///
    /// Host transactions are *about* the DB↔filesystem boundary (plan §5.6):
    /// a fake whose materialize writes nothing cannot show that a crash
    /// mid-publish is rolled back, because there is nothing to roll back. This
    /// combines with [`FakeHostExecutor::failing_at`] so a test can publish
    /// file 0 for real and fail on file 1 — the torn state itself.
    #[must_use]
    pub fn with_real_filesystem(self) -> Self {
        self.lock().real_filesystem = true;
        self
    }

    /// Script whether the next ancestry walk succeeds. Unset means "yes",
    /// which is the shape most store-layer tests want.
    pub fn set_ancestry(&self, descends: bool) {
        self.lock().ancestry = Some(descends);
    }

    /// The recorded call log, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    /// Every `spawn_detached` call, in order, as its `(argv, env)` pair.
    ///
    /// The seam for the duty-#12 dispatch tests: the caller asserts the ported
    /// worker argv and the two env overrides without a real process ever being
    /// launched. A `spawn_detached` that was injected-failed (via
    /// [`FakeHostExecutor::failing_at`]) is absent here — it is still counted in
    /// [`FakeHostExecutor::calls`], but nothing was spawned.
    #[must_use]
    pub fn spawn_calls(&self) -> Vec<SpawnCall> {
        self.lock().spawn_calls.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn record(&self, call: impl Into<String>) -> Result<(), HostErr> {
        let mut state = self.lock();
        let index = state.calls.len();
        let call = call.into();
        state.calls.push(call.clone());
        if state.fail_at == Some(index) {
            return Err(HostErr::ToolFailed {
                tool: "fake",
                detail: format!("injected failure at call {index}: {call}"),
            });
        }
        Ok(())
    }
}

// Restored, for the third time in this workstream and for the same reason each
// time: stripping the six pane verbs off an `impl` block backs up over the
// verb's doc comment, and the FIRST verb's doc comment is preceded by the
// `impl` line itself. `chiefd-host`'s lib build never noticed, because this
// file is `#[cfg(any(test, feature = "test-support"))]` — only a test build
// parses it.
impl HostExecutor for FakeHostExecutor {
    fn proc_identity(&self, pid: Pid) -> Result<ProcIdentity, HostErr> {
        self.record(format!("proc_identity({pid})"))?;
        Ok(ProcIdentity { pid, start_time: 1 })
    }

    fn descends_from(&self, child: Pid, ancestor: Pid) -> Result<bool, HostErr> {
        self.record(format!("descends_from({child}, {ancestor})"))?;
        Ok(self.lock().ancestry.unwrap_or(true))
    }

    fn materialize(&self, plan: &MaterializePlan) -> Result<DriftReport, HostErr> {
        self.record(format!("materialize({})", plan.root.display()))?;
        if self.lock().real_filesystem {
            return crate::files::materialize(plan);
        }
        Ok(DriftReport::default())
    }

    fn spawn_detached(&self, argv: &[String], env: &[(String, String)]) -> Result<Pid, HostErr> {
        // `record` runs the fail-injection check first: an injected failure at
        // this call index returns before anything is recorded as spawned, which
        // is what the rollback test relies on (a failed spawn launched nothing).
        self.record(format!("spawn_detached({argv:?})"))?;
        let mut state = self.lock();
        state.spawn_calls.push((argv.to_vec(), env.to_vec()));
        // A synthetic-but-plausible pid, distinct per spawn.
        let ordinal = i32::try_from(state.spawn_calls.len()).unwrap_or(1);
        Ok(Pid(90_000 + ordinal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_effect_is_recorded_in_call_order() {
        let fake = FakeHostExecutor::new();
        fake.proc_identity(Pid(1)).expect("identity");
        fake.proc_identity(Pid(2)).expect("identity");
        let calls = fake.calls();
        assert_eq!(calls.len(), 2, "both effects are logged");
        assert!(calls[0].starts_with("proc_identity(1"), "in the order they happened");
        assert!(calls[1].starts_with("proc_identity(2"));
    }

    #[test]
    fn failing_at_fails_exactly_the_nth_call_and_no_other() {
        // The injection point matters more than the failure: a fake that fails
        // every call cannot distinguish "the second effect is not retried" from
        // "nothing is ever retried".
        let fake = FakeHostExecutor::failing_at(1);
        fake.proc_identity(Pid(1)).expect("the zeroth call succeeds");
        assert!(fake.proc_identity(Pid(2)).is_err(), "the first call is the injected failure");
        fake.proc_identity(Pid(3)).expect("the second call succeeds again");
    }

    #[test]
    fn the_fake_satisfies_the_object_safe_seam() {
        let fake = FakeHostExecutor::new();
        let as_dyn: &dyn HostExecutor = &fake;
        assert!(as_dyn.descends_from(Pid(1), Pid(2)).is_ok());
    }
}
