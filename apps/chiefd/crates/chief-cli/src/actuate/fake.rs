//! `FakeHostExecutor` and `ScriptedTmux` — the unit-test seam.
//!
//! Gated behind the `test-support` cargo feature (never `#[cfg(test)]`,
//! because integration tests, the conformance runner and the e2e harness are
//! separate crates). Two fakes with different jobs:
//!
//! * [`FakeHostExecutor`] — a whole `HostExecutor` for the *store* layer. It
//!   records every call in order and can be told to fail at a specific call
//!   index, which is how ordering invariants get asserted without any timing
//!   dependence — e.g. invariant 19's "bench readiness check runs AFTER
//!   `beginGracefulStaffingTransition`" is a recorded-call-order assertion,
//!   not a race (TESTING.md §3.2).
//! * [`ScriptedTmux`] — a [`TmuxRunner`](crate::actuate::TmuxRunner) that returns
//!   canned `(status, stdout, stderr)` triples. The trust rules
//!   (TESTING.md §3.4) are exercised through the **real** [`TmuxHost`] logic
//!   driven by this runner, so the tests are deterministic and no tmux server
//!   exists anywhere in CI.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::actuate::host::{
    DriftReport, HostErr, HostExecutor, MaterializePlan, PaneId, PaneIdentity, PanePlan, Pid,
    ProcIdentity, Socket, TmuxCmd, TmuxOut,
};
use crate::actuate::runner::TmuxRunner;
use crate::actuate::{SessionAudit, SessionPresence};

/// A canned tmux answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedReply {
    /// Exit status tmux "returned".
    pub status: i32,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

impl ScriptedReply {
    /// A successful reply with this stdout.
    #[must_use]
    pub fn ok(stdout: &str) -> Self {
        Self { status: 0, stdout: stdout.to_owned(), stderr: String::new() }
    }

    /// A failure with this stderr.
    #[must_use]
    pub fn failed(stderr: &str) -> Self {
        Self { status: 1, stdout: String::new(), stderr: stderr.to_owned() }
    }

    /// The transient condition the retry ladder exists for.
    #[must_use]
    pub fn server_exited() -> Self {
        Self::failed("tmux: server exited unexpectedly")
    }

    /// The no-tag response of a tmux that does not know an option.
    #[must_use]
    pub fn invalid_option(option: &str) -> Self {
        Self::failed(&format!("invalid option: {option}"))
    }

    /// tmux answering, authoritatively, that the session is not there.
    #[must_use]
    pub fn no_session(session: &str) -> Self {
        Self::failed(&format!("can't find session: {session}"))
    }
}

#[derive(Debug, Default)]
struct ScriptState {
    replies: VecDeque<ScriptedReply>,
    geometry_replies: VecDeque<ScriptedReply>,
    /// Used when the script runs dry — `None` means "running dry is a bug and
    /// must be visible", which is the default.
    fallback: Option<ScriptedReply>,
    calls: Vec<Vec<String>>,
    record_viewport_authority: bool,
}

/// A [`TmuxRunner`] that replays a script of canned answers.
#[derive(Debug, Default)]
pub struct ScriptedTmux {
    state: Mutex<ScriptState>,
}

impl ScriptedTmux {
    /// A runner that answers with `replies`, in order.
    #[must_use]
    pub fn new(replies: impl IntoIterator<Item = ScriptedReply>) -> Self {
        Self {
            state: Mutex::new(ScriptState {
                replies: replies.into_iter().collect(),
                geometry_replies: VecDeque::new(),
                fallback: None,
                calls: Vec::new(),
                record_viewport_authority: false,
            }),
        }
    }

    /// A runner that gives the same answer to everything, forever. Used for
    /// the ladder tests, where "every attempt hits the transient" is the case
    /// under test.
    #[must_use]
    pub fn always(reply: ScriptedReply) -> Self {
        Self {
            state: Mutex::new(ScriptState {
                replies: VecDeque::new(),
                geometry_replies: VecDeque::new(),
                fallback: Some(reply),
                calls: Vec::new(),
                record_viewport_authority: false,
            }),
        }
    }

    /// Script the listed replies, then repeat `fallback` forever.
    #[must_use]
    pub fn then_always(
        replies: impl IntoIterator<Item = ScriptedReply>,
        fallback: ScriptedReply,
    ) -> Self {
        Self {
            state: Mutex::new(ScriptState {
                replies: replies.into_iter().collect(),
                geometry_replies: VecDeque::new(),
                fallback: Some(fallback),
                calls: Vec::new(),
                record_viewport_authority: false,
            }),
        }
    }

    /// Give keyed answers to canonical-window geometry probes without shifting
    /// the command script that tests an actuation sequence.
    #[must_use]
    pub fn with_geometry(self, replies: impl IntoIterator<Item = ScriptedReply>) -> Self {
        self.lock().geometry_replies = replies.into_iter().collect();
        self
    }

    /// Record viewport fence calls while retaining their stable canned reply.
    #[must_use]
    pub fn recording_viewport_authority(self) -> Self {
        self.lock().record_viewport_authority = true;
        self
    }

    /// Every argv this runner was asked to run, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.lock().calls.clone()
    }

    /// How many invocations were made.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.lock().calls.len()
    }

    /// Whether any invocation's first argument was `verb`. The trust tests use
    /// this to assert that a *destructive* verb was never reached.
    #[must_use]
    pub fn ran_verb(&self, verb: &str) -> bool {
        self.calls().iter().any(|argv| argv.first().is_some_and(|first| first == verb))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ScriptState> {
        // A poisoned mutex means a test already failed; recovering keeps the
        // original assertion failure as the reported one.
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl TmuxRunner for ScriptedTmux {
    fn run(&self, _socket: &Socket, cmd: &TmuxCmd) -> Result<TmuxOut, HostErr> {
        let mut state = self.lock();
        // Viewport manifest authority is orthogonal bookkeeping around the
        // actuation command scripts. Keep legacy business-rule fixtures keyed
        // to their product mutation calls; dedicated viewport tests exercise
        // these exact commands with real tmux and focused builders.
        if cmd.argv.iter().any(|arg| arg == "@chief_viewport_topology_epoch")
            && cmd.argv.iter().any(|arg| arg == "display-message")
        {
            if state.record_viewport_authority {
                state.calls.push(cmd.argv.clone());
            }
            return Ok(TmuxOut { status: 0, stdout: "1".to_owned(), stderr: String::new() });
        }
        if cmd.argv.first().is_some_and(|arg| arg == "if-shell")
            && cmd.argv.iter().any(|arg| arg.contains("@chief_viewport_refresh_command"))
        {
            if state.record_viewport_authority {
                state.calls.push(cmd.argv.clone());
            }
            return Ok(TmuxOut { status: 0, stdout: String::new(), stderr: String::new() });
        }
        let geometry_probe = cmd.argv.iter().any(|arg| {
            arg.contains("#{window_width}\t#{window_height}\t#{@organization_window_id}")
                || arg.contains(
                    "#{window_index}\t#{window_width}\t#{window_height}\t#{@organization_window_id}",
                )
        });
        if geometry_probe {
            let Some(reply) = state.geometry_replies.pop_front() else {
                return Ok(TmuxOut { status: 0, stdout: String::new(), stderr: String::new() });
            };
            state.calls.push(cmd.argv.clone());
            return Ok(TmuxOut {
                status: reply.status,
                stdout: reply.stdout,
                stderr: reply.stderr,
            });
        }
        state.calls.push(cmd.argv.clone());
        let reply = state.replies.pop_front().or_else(|| state.fallback.clone());
        match reply {
            Some(reply) => {
                Ok(TmuxOut { status: reply.status, stdout: reply.stdout, stderr: reply.stderr })
            }
            // Running dry is reported as a tool failure rather than a panic so
            // it surfaces as a test failure at the assertion, with the argv
            // that went unanswered.
            None => Err(HostErr::ToolUnavailable {
                tool: "scripted-tmux",
                detail: format!("script exhausted at call {}: {:?}", state.calls.len(), cmd.argv),
            }),
        }
    }
}

/// One recorded [`HostExecutor::spawn_detached`] call: its `argv` and the env
/// overrides it was handed. The unit under assertion for the duty-#12 tests.
pub type SpawnCall = (Vec<String>, Vec<(String, String)>);

#[derive(Debug, Default)]
struct FakeState {
    calls: Vec<String>,
    fail_at: Option<usize>,
    real_filesystem: bool,
    pane_pids: std::collections::HashMap<String, Pid>,
    pane_identities: std::collections::HashMap<String, PaneIdentity>,
    ancestry: Option<bool>,
    next_pane: usize,
    /// The audit `audit_session` returns. `None` is a provably-absent, empty
    /// session — the honest "no session here" default, never a fabricated
    /// ownership.
    session_audit: Option<SessionAudit>,
    /// When set, `audit_session` returns [`HostErr::Untrusted`] with this
    /// reason — the "tmux did not answer" case a gatherer must fail closed on
    /// rather than treat as an observed-empty session.
    audit_untrusted: Option<&'static str>,
    /// The pane ids `dead_pane_ids` reports.
    dead_panes: Vec<PaneId>,
    spawn_calls: Vec<SpawnCall>,
}

/// A recording, scriptable `HostExecutor`.
///
/// This is a STORE-LAYER fake: its blanket [`HostExecutor::tmux`] answers
/// every command, including `has-session`, with exit 0 regardless of what
/// was asked. That is exactly wrong for driving `converge_apply`'s
/// `observe`/`reconcile_cycle` pipeline: `observe()` reads a blanket-0
/// `has-session` as "session present," while this fake's OWN unset-state
/// `audit_session` default reads as "provably absent" — a self-contradiction
/// `reconcile_cycle`'s `SessionOwnership` refusal correctly rejects (found
/// the hard way: three tests failed against exactly this combination before
/// the mismatch was traced here, not in the route under test). Use
/// `RealHostExecutor<ScriptedTmux, RecordingWaiter>` instead for anything
/// that calls `observe`/`plan_cycle`/`reconcile_cycle` — see
/// `runtime_waker.rs`'s and `converge_apply::cycle`'s own test modules for
/// the `scripted_host` construction pattern.
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

    /// Script the pid a pane reports.
    ///
    /// Calling this twice for the same pane models `respawn-pane` and the
    /// native fresh-session path: the pid *changes under a live pane*, which
    /// is the exact case a spawn-time pid record gets wrong (plan §6.2).
    pub fn set_pane_pid(&self, pane: &PaneId, pid: Pid) {
        self.lock().pane_pids.insert(pane.0.clone(), pid);
    }

    /// Script the whole authentication observation of a pane: pid *and*
    /// ownership tags.
    ///
    /// Calling this twice for the same pane with a different pid is the
    /// `respawn-pane` / native-fresh-session case — the pane is the same, its
    /// process is not.
    pub fn set_pane_identity(&self, identity: PaneIdentity) {
        let mut state = self.lock();
        state.pane_pids.insert(identity.pane.0.clone(), identity.pid);
        state.pane_identities.insert(identity.pane.0.clone(), identity);
    }

    /// Script whether the next ancestry walk succeeds. Unset means "yes",
    /// which is the shape most store-layer tests want.
    pub fn set_ancestry(&self, descends: bool) {
        self.lock().ancestry = Some(descends);
    }

    /// Script the ownership audit `audit_session` returns.
    pub fn set_session_audit(&self, audit: SessionAudit) {
        self.lock().session_audit = Some(audit);
    }

    /// Make `audit_session` answer [`HostErr::Untrusted`] — the transient tmux
    /// condition a gatherer must not read as an empty session.
    pub fn set_audit_untrusted(&self, reason: &'static str) {
        self.lock().audit_untrusted = Some(reason);
    }

    /// Script the pane ids `dead_pane_ids` reports.
    pub fn set_dead_panes(&self, dead: Vec<PaneId>) {
        self.lock().dead_panes = dead;
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

impl HostExecutor for FakeHostExecutor {
    fn tmux(&self, socket: &Socket, cmd: TmuxCmd) -> Result<TmuxOut, HostErr> {
        self.record(format!("tmux({}, {:?})", socket.0, cmd.argv))?;
        Ok(TmuxOut { status: 0, stdout: String::new(), stderr: String::new() })
    }

    fn wait(&self, duration: std::time::Duration) {
        self.lock().calls.push(format!("wait({}ms)", duration.as_millis()));
    }

    fn spawn_pane(&self, plan: &PanePlan) -> Result<PaneId, HostErr> {
        self.record(format!("spawn_pane({}:{})", plan.session, plan.window))?;
        let mut state = self.lock();
        state.next_pane += 1;
        Ok(PaneId(format!("%{}", state.next_pane)))
    }

    fn pane_pid(&self, socket: &Socket, pane: &PaneId) -> Result<Pid, HostErr> {
        self.record(format!("pane_pid({}, {})", socket.0, pane.0))?;
        Ok(self.lock().pane_pids.get(&pane.0).copied().unwrap_or(Pid(4242)))
    }

    fn pane_identity(&self, socket: &Socket, pane: &PaneId) -> Result<PaneIdentity, HostErr> {
        self.record(format!("pane_identity({}, {})", socket.0, pane.0))?;
        self.lock().pane_identities.get(&pane.0).cloned().ok_or_else(|| HostErr::ToolFailed {
            tool: "fake",
            detail: format!("pane {} has incomplete ChiefD ownership tags", pane.0),
        })
    }

    fn audit_session(
        &self,
        socket: &Socket,
        session: &str,
        organization: &str,
    ) -> Result<SessionAudit, HostErr> {
        self.record(format!("audit_session({}, {session}, {organization})", socket.0))?;
        let state = self.lock();
        if let Some(reason) = state.audit_untrusted {
            return Err(HostErr::Untrusted { reason: reason.into() });
        }
        Ok(state.session_audit.clone().unwrap_or_else(|| SessionAudit {
            presence: SessionPresence::ProvablyAbsent,
            windows: std::collections::BTreeMap::new(),
            panes: std::collections::BTreeMap::new(),
        }))
    }

    fn dead_pane_ids(&self, socket: &Socket, session: &str) -> Result<Vec<PaneId>, HostErr> {
        self.record(format!("dead_pane_ids({}, {session})", socket.0))?;
        Ok(self.lock().dead_panes.clone())
    }

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
    use crate::actuate::*;

    fn socket() -> Socket {
        Socket("chiefd-test".into())
    }

    #[test]
    fn calls_are_recorded_in_order() {
        let fake = FakeHostExecutor::new();
        fake.tmux(&socket(), TmuxCmd { argv: vec!["list-panes".into()] }).expect("tmux");
        fake.pane_pid(&socket(), &PaneId("%1".into())).expect("pane_pid");
        fake.proc_identity(Pid(1)).expect("proc_identity");
        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert!(calls[0].starts_with("tmux("));
        assert!(calls[1].starts_with("pane_pid("));
        assert_eq!(calls[2], "proc_identity(1)");
    }

    #[test]
    fn failure_injection_hits_exactly_the_requested_call_index() {
        let fake = FakeHostExecutor::failing_at(1);
        fake.proc_identity(Pid(1)).expect("call 0 succeeds");
        let err = fake.proc_identity(Pid(1)).expect_err("call 1 fails");
        assert!(err.to_string().contains("injected failure at call 1"));
        fake.proc_identity(Pid(1)).expect("call 2 succeeds again");
        assert_eq!(fake.calls().len(), 3, "the failing call is still recorded");
    }

    #[test]
    fn a_fake_with_no_injection_never_fails() {
        let fake = FakeHostExecutor::new();
        for _ in 0..10 {
            fake.proc_identity(Pid(1)).expect("no injection configured");
        }
        assert_eq!(fake.calls().len(), 10);
    }

    #[test]
    fn the_fake_satisfies_the_object_safe_trait_object_the_store_layer_holds() {
        let fake = FakeHostExecutor::new();
        let as_dyn: &dyn HostExecutor = &fake;
        as_dyn.proc_identity(Pid(1)).expect("callable through the seam");
        assert_eq!(fake.calls(), vec!["proc_identity(1)".to_string()]);
    }

    #[test]
    fn spawned_panes_get_distinct_ids() {
        let fake = FakeHostExecutor::new();
        let plan = PanePlan {
            socket: socket(),
            session: "cobalt".into(),
            window: "eng".into(),
            argv: vec!["pi".into()],
            tags: Vec::new(),
        };
        let first = fake.spawn_pane(&plan).expect("spawn");
        let second = fake.spawn_pane(&plan).expect("spawn");
        assert_ne!(first, second);
    }

    #[test]
    fn a_scripted_pane_pid_can_change_under_a_live_pane() {
        // The respawn case: the pid a pane reports is not stable, which is why
        // auth re-reads it (plan §6.2).
        let fake = FakeHostExecutor::new();
        let pane = PaneId("%7".into());
        fake.set_pane_pid(&pane, Pid(100));
        assert_eq!(fake.pane_pid(&socket(), &pane).expect("read"), Pid(100));
        fake.set_pane_pid(&pane, Pid(200));
        assert_eq!(fake.pane_pid(&socket(), &pane).expect("re-read"), Pid(200));
    }

    #[test]
    fn the_scripted_runner_replays_in_order_and_records_argv() {
        let runner = ScriptedTmux::new([ScriptedReply::ok("%1"), ScriptedReply::failed("nope")]);
        let first =
            runner.run(&socket(), &TmuxCmd { argv: vec!["new-window".into()] }).expect("scripted");
        assert_eq!(first.stdout, "%1");
        let second =
            runner.run(&socket(), &TmuxCmd { argv: vec!["kill-pane".into()] }).expect("scripted");
        assert_eq!(second.status, 1);
        assert_eq!(runner.calls(), vec![vec!["new-window".to_string()], vec!["kill-pane".into()]]);
        assert!(runner.ran_verb("kill-pane"));
        assert!(!runner.ran_verb("kill-session"));
    }

    #[test]
    fn an_exhausted_script_is_a_visible_failure_not_a_default_success() {
        let runner = ScriptedTmux::new([]);
        let error =
            runner.run(&socket(), &TmuxCmd { argv: vec!["has-session".into()] }).expect_err("dry");
        assert!(error.to_string().contains("script exhausted"));
    }
}
