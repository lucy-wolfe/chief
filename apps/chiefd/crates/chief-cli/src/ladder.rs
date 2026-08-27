//! The one log shape every bounded wait in this client emits.
//!
//! # The defect this closes
//!
//! The daemon-level stream `chiefd-log` writes showed a company launch that
//! SUCCEEDED writing ten `warn`/`error` records out of eighty-one lines. Seven
//! of them were one ladder polling for a tmux session that another process was
//! about to create — `.898`, `.900`, `.902`, `.943`, `.152`, `.360`, `.568`,
//! then success. Nothing was wrong, and the log said something was wrong seven
//! times.
//!
//! A `warn` must mean something a human can act on. A poll that is EXPECTED to
//! miss until the thing appears is not that, and a ladder that logs one line
//! per attempt buries the two facts an operator actually wants — how many
//! attempts it took, and how long it waited.
//!
//! # The shape
//!
//! One `info` when the wait begins, one `info` when it resolves, and `debug`
//! for every repeat in between:
//!
//! ```text
//! info   actuator.window.wait      attempt=1 waited_ms=0   backoff_ms=200 budget_ms=45000
//! debug  actuator.window.wait      attempt=2 waited_ms=201 backoff_ms=200 budget_ms=45000
//! info   actuator.window.running   attempt=3 waited_ms=402
//! ```
//!
//! That is strictly more useful than seven identical warnings AND it is quiet.
//! The model is `discovery::ensure_running`, which already reported the attempt
//! count and the elapsed time on its resolution line.
//!
//! # What this type does NOT own
//!
//! It never decides whether to keep waiting. The budget and the backoff are
//! reported on the line because an operator reading one line should not have to
//! find the constant, but the loop still owns its own deadline and its own
//! sleep. A logging helper that could end a wait would be a behavior change
//! wearing a logging change's clothes.

use std::time::{Duration, Instant};

/// The three event names one ladder writes under.
///
/// Named at the call site rather than derived from a prefix, so every event in
/// the stream is a literal somebody can grep for and none is assembled at
/// runtime.
#[derive(Debug, Clone, Copy)]
pub struct LadderEvents {
    /// The wait line: `info` once on entry, `debug` for every repeat.
    pub waiting: &'static str,
    /// The resolution line, `info`.
    pub resolved: &'static str,
    /// The line for a wait that ended WITHOUT the thing appearing, `error`.
    /// This one is the signal, and it is the reason none of the others has to
    /// be loud.
    pub failed: &'static str,
}

/// One bounded wait, and the lines it writes.
#[derive(Debug)]
pub struct Ladder {
    events: LadderEvents,
    /// What is being waited FOR — a tmux session name, a tmux verb. One field,
    /// because a ladder line nobody can attribute is a line nobody can use.
    subject: String,
    started: Instant,
    /// Waits emitted so far. `0` until the first [`Ladder::waiting`], so the
    /// entry line reads `attempt=1`.
    attempts: u64,
    budget_ms: u64,
    backoff_ms: u64,
}

impl Ladder {
    /// Begin a wait. Writes nothing: a wait that resolves on its first look
    /// costs no lines at all, which is the common case and the quiet one.
    #[must_use]
    pub fn new(
        events: LadderEvents,
        subject: impl Into<String>,
        budget: Duration,
        backoff: Duration,
    ) -> Self {
        Self {
            events,
            subject: subject.into(),
            started: Instant::now(),
            attempts: 0,
            budget_ms: chiefd_log::duration_ms(budget),
            backoff_ms: chiefd_log::duration_ms(backoff),
        }
    }

    /// How many waits this ladder has emitted.
    #[must_use]
    pub const fn attempts(&self) -> u64 {
        self.attempts
    }

    /// Record that the thing is not there YET, and the wait continues.
    ///
    /// The first call is `info` — the entry line, which says a wait has begun
    /// and what its budget is. Every later call is `debug`, because a repeat of
    /// a line already written is not new information at the default level.
    pub fn waiting(&mut self) {
        self.attempts += 1;
        let attempt = self.attempts;
        let waited_ms = chiefd_log::elapsed_ms(self.started);
        let subject = self.subject.as_str();
        if attempt == 1 {
            tracing::info!(
                event = self.events.waiting,
                subject,
                attempt,
                waited_ms,
                backoff_ms = self.backoff_ms,
                budget_ms = self.budget_ms,
                "not there yet; waiting"
            );
        } else {
            tracing::debug!(
                event = self.events.waiting,
                subject,
                attempt,
                waited_ms,
                backoff_ms = self.backoff_ms,
                budget_ms = self.budget_ms,
                "still not there; waiting"
            );
        }
    }

    /// Record that the wait ended because the thing appeared.
    ///
    /// Always written, even when nothing was ever waited for, so the attempt
    /// count and the elapsed time of a step are readable without correlating
    /// two lines that may not both exist.
    pub fn resolved(&self) {
        tracing::info!(
            event = self.events.resolved,
            subject = self.subject.as_str(),
            attempt = self.attempts + 1,
            waited_ms = chiefd_log::elapsed_ms(self.started),
            "the wait resolved"
        );
    }

    /// Record that the wait ended and the thing never appeared. THE signal, and
    /// the reason everything above it can be quiet.
    ///
    /// `reason` separates the two ways a bounded wait ends badly, because they
    /// are different faults with different next moves: the budget ran out, or
    /// the thing being waited for died. A ladder that reported both as
    /// "exhausted" would name the wrong one half the time.
    pub fn failed(&self, reason: &str) {
        tracing::error!(
            event = self.events.failed,
            subject = self.subject.as_str(),
            reason,
            attempt = self.attempts + 1,
            waited_ms = chiefd_log::elapsed_ms(self.started),
            budget_ms = self.budget_ms,
            "the wait ended without the thing appearing"
        );
    }
}

/// Reading back what a ladder actually wrote — shared by every test in this
/// crate that asserts a log LEVEL rather than a log message.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::Value;
    use tracing_subscriber::layer::SubscriberExt as _;

    /// Run `body` under a subscriber whose only layer is the JSONL sink, and
    /// read back every line it wrote.
    ///
    /// `with_default` rather than a global install, for the reason
    /// `chiefd-log`'s own layer tests give: these run beside each other in one
    /// process and a global subscriber can be set exactly once. No level filter
    /// is attached, so the `debug` repeats reach the file and can be asserted —
    /// in production the installed `EnvFilter` defaults to `info` and drops
    /// them, which is the whole point.
    ///
    /// The directory is per INVOCATION, not per process and name.
    /// `with_default` is thread-local and already keeps one test's lines out of
    /// another's; what a name plus a pid cannot keep out is a second invocation
    /// that reuses the name, a re-run whose pid the OS handed back, or a
    /// parallel binary that landed on the same one. A recorder a test does not
    /// own is a reading a test cannot trust, and the reading is the whole
    /// evidence. Same mechanism as the tmux fixtures' `unique_socket`: this
    /// process, this instant, this call.
    ///
    /// The subscriber is thread-local; the CALLSITE CACHE it reads is not, and
    /// that is what [`permit_every_callsite`] exists for. Nothing here works
    /// without it — see its own comment.
    pub(crate) fn recorded(name: &str, body: impl FnOnce()) -> Vec<Value> {
        permit_every_callsite();

        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let instant = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        let directory = std::env::temp_dir().join(format!(
            "chiefd-recorded-{}-{name}-{instant}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("create the throwaway log directory");
        let sink = chiefd_log::OrgLog::new(&directory, "chiefd", chiefd_log::NO_ORGANIZATION);
        let path = sink.path().to_path_buf();
        let subscriber = tracing_subscriber::registry().with(chiefd_log::SinkLayer::new(sink));
        tracing::subscriber::with_default(subscriber, body);
        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line must be valid JSON"))
            .collect()
    }

    /// Install one permissive process-wide subscriber, so that no `tracing`
    /// callsite in this test binary can ever be cached as "nobody is
    /// interested".
    ///
    /// # The race this closes
    ///
    /// `with_default` is thread-local, so one test's lines cannot reach
    /// another's recorder. True — and beside the point, because whether a
    /// callsite emits AT ALL is decided once, process-globally, the first time
    /// that line of code executes, and is then cached in a static for the life
    /// of the process (`tracing_core::callsite`). `tracing-core` computes that
    /// decision from every LIVE dispatcher — except when exactly one is alive,
    /// where it takes the shortcut of asking only the REGISTERING THREAD's
    /// default subscriber.
    ///
    /// A test binary hits that shortcut constantly. While one test holds the
    /// only live dispatcher (its own scoped recorder), every other test thread
    /// has no subscriber at all, and `NoSubscriber` answers `Interest::never`.
    /// Whichever thread reaches a ladder's `debug!` first therefore decides
    /// whether that line can ever be recorded again, and a test that lost the
    /// race read an EMPTY file — a failure that reproduced only under
    /// parallelism and passed every time in isolation. It was diagnosed and
    /// fixed on the tmux side of this crate first; the same comment stands
    /// there, over the same fix, because the two recorders belong to different
    /// targets and cannot share one.
    ///
    /// A permanently-installed global default fixes both halves. It is a
    /// dispatcher that is always alive, so the one-dispatcher shortcut is never
    /// taken while a recorder is running and the decision is the union over the
    /// recorder too; and when it IS the only one, it is a bare `Registry`,
    /// whose answer is `Interest::always` rather than `never`. It has no
    /// layers, so it records nothing and cannot pollute a recorder — the scoped
    /// subscriber still wins on the thread that installs it.
    ///
    /// Idempotent: `set_global_default` succeeds once per process and every
    /// later call is a no-op.
    pub(crate) fn permit_every_callsite() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
        });
    }

    /// One `tracing` callsite two threads can reach.
    ///
    /// A `debug!` written inline in each place would be two DIFFERENT
    /// callsites with independent caches, and the test below would prove
    /// nothing. This is the shared line.
    fn shared_callsite() {
        tracing::debug!(event = "test.shared.callsite", "one callsite, two threads");
    }

    /// The RULE every recorder in this crate rests on: what a test records is
    /// decided by the subscriber that test installed, and no thread running
    /// beside it can switch the line off.
    ///
    /// The subscriber-less thread reaches the callsite FIRST, which is the
    /// whole race. Without [`permit_every_callsite`] the spawned thread caches
    /// `Interest::never` for the process and this fails every single run.
    #[test]
    fn a_recording_survives_a_subscriberless_thread_reaching_the_same_callsite() {
        let lines = recorded("callsite-cache", || {
            std::thread::spawn(shared_callsite).join().expect("the racing thread must finish");
            shared_callsite();
        });

        assert!(
            lines.iter().any(|line| line["event"] == "test.shared.callsite"),
            "a thread with no subscriber must not be able to silence a recorder: {lines:?}"
        );
    }

    /// Every line's level, in order — the whole shape of a wait in one vector.
    pub(crate) fn levels(lines: &[Value]) -> Vec<&str> {
        lines.iter().filter_map(|line| line["level"].as_str()).collect()
    }

    /// The lines a human would be paged for.
    pub(crate) fn loud(lines: &[Value]) -> Vec<&Value> {
        lines.iter().filter(|line| line["level"] == "warn" || line["level"] == "error").collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{levels, loud, recorded};
    use super::{Ladder, LadderEvents};
    use serde_json::Value;
    use std::time::Duration;

    const EVENTS: LadderEvents = LadderEvents {
        waiting: "test.ladder.wait",
        resolved: "test.ladder.resolved",
        failed: "test.ladder.failed",
    };

    /// The acceptance criterion, stated as a test: the ladder from the incident
    /// — seven polls, then success — writes NOTHING a human must act on.
    #[test]
    fn a_wait_that_resolves_emits_no_warning_at_all() {
        let lines = recorded("resolves", || {
            let mut ladder = Ladder::new(
                EVENTS,
                "org-tribes-capital_",
                Duration::from_secs(45),
                Duration::from_millis(200),
            );
            for _ in 0..7 {
                ladder.waiting();
            }
            ladder.resolved();
        });

        assert!(
            loud(&lines).is_empty(),
            "a successful wait must emit no warn and no error, got {:?}",
            levels(&lines)
        );
        // One entry line, six quiet repeats, one resolution.
        assert_eq!(
            levels(&lines),
            vec!["info", "debug", "debug", "debug", "debug", "debug", "debug", "info"],
            "the shape is one info on entry, debug repeats, one info on resolution"
        );
    }

    /// The entry line is the one an operator reads at the default level, so it
    /// carries every fact needed to judge the wait without opening the source.
    #[test]
    fn the_entry_line_carries_the_attempt_count_the_elapsed_time_and_the_budget() {
        let lines = recorded("entry", || {
            let mut ladder = Ladder::new(
                EVENTS,
                "chiefd-actuator-org-acme",
                Duration::from_secs(45),
                Duration::from_millis(200),
            );
            ladder.waiting();
            ladder.waiting();
        });

        let entry = &lines[0];
        assert_eq!(entry["event"], "test.ladder.wait");
        assert_eq!(entry["level"], "info");
        assert_eq!(entry["detail"]["subject"], "chiefd-actuator-org-acme");
        assert_eq!(entry["detail"]["attempt"], 1);
        assert!(entry["detail"]["waited_ms"].as_u64().is_some(), "the entry line must be timed");
        assert_eq!(entry["detail"]["backoff_ms"], 200);
        assert_eq!(entry["detail"]["budget_ms"], 45_000);

        // The repeat is the same fields at a level the default filter drops.
        assert_eq!(lines[1]["level"], "debug");
        assert_eq!(lines[1]["detail"]["attempt"], 2);
    }

    /// The resolution line is what replaced seven warnings: it says how many
    /// attempts it took and how long it took.
    #[test]
    fn the_resolution_line_carries_the_attempt_count_and_the_elapsed_time() {
        let lines = recorded("resolution", || {
            let mut ladder = Ladder::new(
                EVENTS,
                "org-acme",
                Duration::from_secs(45),
                Duration::from_millis(200),
            );
            ladder.waiting();
            ladder.waiting();
            ladder.resolved();
        });

        let resolved = lines.last().expect("a resolution line");
        assert_eq!(resolved["event"], "test.ladder.resolved");
        assert_eq!(resolved["level"], "info");
        assert_eq!(resolved["detail"]["subject"], "org-acme");
        assert_eq!(resolved["detail"]["attempt"], 3, "the look that succeeded is counted");
        assert!(resolved["detail"]["waited_ms"].as_u64().is_some(), "the wait must be timed");
    }

    /// A wait that resolves on its first look costs exactly one line. Quiet is
    /// not the same as silent: the step is still in the stream.
    #[test]
    fn a_wait_that_never_had_to_wait_writes_only_its_resolution() {
        let lines = recorded("immediate", || {
            let ladder = Ladder::new(
                EVENTS,
                "org-acme",
                Duration::from_secs(45),
                Duration::from_millis(200),
            );
            ladder.resolved();
        });

        assert_eq!(lines.len(), 1, "one line, got {lines:?}");
        assert_eq!(lines[0]["event"], "test.ladder.resolved");
        assert_eq!(lines[0]["detail"]["attempt"], 1);
    }

    /// The signal that must never be lost: a ladder that runs out of budget is
    /// a real failure and stays loud.
    #[test]
    fn a_ladder_that_exhausts_its_budget_is_loud() {
        let lines = recorded("exhausted", || {
            let mut ladder = Ladder::new(
                EVENTS,
                "org-acme",
                Duration::from_secs(45),
                Duration::from_millis(200),
            );
            for _ in 0..4 {
                ladder.waiting();
            }
            ladder.failed("budget expired");
        });

        let failures: Vec<&Value> = loud(&lines);
        assert_eq!(failures.len(), 1, "exactly one loud line, got {:?}", levels(&lines));
        assert_eq!(failures[0]["event"], "test.ladder.failed");
        assert_eq!(failures[0]["level"], "error");
        assert_eq!(failures[0]["detail"]["subject"], "org-acme");
        assert_eq!(failures[0]["detail"]["reason"], "budget expired");
        assert_eq!(failures[0]["detail"]["attempt"], 5);
        assert_eq!(failures[0]["detail"]["budget_ms"], 45_000);
        assert!(failures[0]["detail"]["waited_ms"].as_u64().is_some(), "a failure must be timed");
    }
}
