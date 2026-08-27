//! The crash loop: a person who will not stay up is retried FOR EVER, and the
//! screen says what is happening while it goes on.
//!
//! # The ruling this module now serves
//!
//! Operator, 2026-08-19: *"We need to never give up. Why is there a crash loop
//! of five? It shouldn't be like that. If something needs to start it should
//! just start. If it's crash looping, just do a backoff on it with a maximum of
//! let's say 10 seconds ... Always keep retrying ... and then show some kind of
//! indication on the screen that it's crashing and this is retry number blah
//! blah blah, so we can know how many retries happened."*
//!
//! This replaces a give-up. The previous design counted five consecutive failed
//! boots and then STOPPED, dropping the person from placement and publishing
//! the verdict to a tmux session option so a replacement actuator would not
//! retry either. It wedged a real company: `ivo`, `sasha`, `eli` and `rune` on
//! the owner's box crash-looped through a 90-second chiefd outage at 12:26 UTC,
//! were held at 12:34, and were still held at 14:05 — long after the outage
//! ended — because a held person is dropped from placement, so no pane is ever
//! minted for them, so the "an exact live pane releases the hold" escape can
//! never fire. The hold sealed its own exit. chiefd meanwhile still published
//! them as desired-active, and the rail drew `starting` at an operator whose
//! clicks did nothing.
//!
//! **A transient fault must never become a permanent one.** There is no limit
//! here any more, and there is nothing to release: a person chiefd wants up is
//! attempted again, and again, until they stay up or chiefd stops wanting them.
//!
//! # What counts as a failed boot
//!
//! One pass spawns a person; the next pass observes tmux and their pane is not
//! there, or is a DIFFERENT pane than last time. That is the only evidence
//! available, and it is enough: a pane the actuator created that is gone by the
//! next pass held a process that exited.
//!
//! Deliberately NOT counted:
//!
//! * a pass that could not observe tmux at all — an unreadable runtime is not
//!   an absent one, and counting it would let a flaky tmux socket slow a
//!   healthy company down to the backoff ceiling;
//! * a pass where the spawn step itself failed — that failure is already a
//!   named, loud interpreter error;
//! * a pass that fail-stopped BEFORE the spawn step — its own abandoned plan is
//!   what moved the panes, and charging that to the people charges them for the
//!   actuator's work (see `previous_pass_failed`);
//! * a person chiefd stopped desiring — they are simply forgotten.
//!
//! # What happens at each failure
//!
//! The person's next attempt is DELAYED, and nothing else. Converge passes run
//! about once a second, so an unthrottled crash loop respawns a broken
//! workspace sixty times a minute and buries every other line on the operator's
//! screen. The delay grows [`FIRST_RETRY_DELAY`] → ×[`RETRY_BACKOFF_FACTOR`] →
//! … → [`MAX_RETRY_DELAY`] and then stays there for ever. It never becomes
//! infinite, which is the whole difference between this and what it replaces.
//!
//! While a person waits out their delay they stay in the desired placement and
//! stay in their department's window: only their SPAWN step is skipped, exactly
//! the way chiefd's launch gate skips a refused person's. A wait that dropped
//! them from placement would reap the window their diagnostics are drawn in and
//! re-mint it a few seconds later, which is churn where the operator is trying
//! to read.
//!
//! # What the screen gets
//!
//! [`CrashLoop::reports`] is the whole operator-facing surface: per person, how
//! many consecutive boots failed, how long it has been going on, when the next
//! attempt is due, and one or two sentences about what actually went wrong.
//! The rail draws it as `crashing`, and the person's own card carries the
//! sentence. Nothing here travels to chiefd: desired-state-only is intact,
//! because there is no verdict to reconcile any more.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// The delay before the FIRST retry of a person whose boot just failed.
///
/// Short on purpose: the operator's rule is "if something needs to start it
/// should just start". One lost race — a pane that died to a chiefd blip — must
/// cost the company half a second, not a visible pause.
pub const FIRST_RETRY_DELAY: Duration = Duration::from_millis(500);

/// How the delay grows with each further consecutive failure.
pub const RETRY_BACKOFF_FACTOR: u32 = 2;

/// The ceiling the delay grows to and never passes.
///
/// The operator named this number: *"a maximum of let's say 10 seconds ... and
/// then after that just every 10 seconds keep retrying"*. With
/// [`FIRST_RETRY_DELAY`] and [`RETRY_BACKOFF_FACTOR`] the curve is
/// 0.5s, 1s, 2s, 4s, 8s, 10s, 10s, … — the ceiling is reached at the sixth
/// consecutive failure, about 25 seconds after the first one.
pub const MAX_RETRY_DELAY: Duration = Duration::from_secs(10);

/// How long to wait before retrying a person whose last `failures` consecutive
/// boots failed.
///
/// Pure, and public, because it is the curve the operator specified and a test
/// must be able to hold the whole of it rather than sample it.
#[must_use]
pub fn retry_delay(failures: u32) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    let mut delay = FIRST_RETRY_DELAY;
    for _ in 1..failures {
        if delay >= MAX_RETRY_DELAY {
            break;
        }
        delay = delay.saturating_mul(RETRY_BACKOFF_FACTOR);
    }
    delay.min(MAX_RETRY_DELAY)
}

/// What the actuator has seen of one person's boots.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attempts {
    /// Consecutive failed boots.
    failures: u32,
    /// The launch hash those failures were against. A different hash is a
    /// different question, so the count starts again.
    launch_hash: String,
    /// When the FIRST of this run of failures happened. This is what answers
    /// the operator's "how long has it been going on".
    first_failed_at: Instant,
    /// When the most recent one happened; the delay is measured from here.
    last_failed_at: Instant,
    /// One or two sentences about what went wrong, for the screen.
    last_error: Option<String>,
}

/// Per-person consecutive-failure counts for one actuator process.
#[derive(Debug, Clone, Default)]
pub struct CrashLoop {
    people: BTreeMap<String, Attempts>,
    /// People this actuator spawned in the pass that just ended, awaiting the
    /// next pass's verdict on whether they survived.
    pending: Vec<String>,
    /// The pane each person was last observed in.
    ///
    /// PRESENCE IS NOT SURVIVAL, and this map is what tells them apart. A
    /// process that starts and exits a few seconds later is PRESENT at the next
    /// observation — passes run about once a second — so a registry that judged
    /// a boot by "is there a pane for them now" cleared the failure count on
    /// every single cycle and could never count anything. A live company spent
    /// half an hour respawning five people about once a second, in silence,
    /// because every spawn looked like a success.
    ///
    /// A person whose pane id CHANGED between passes had their old process die.
    /// That is the positive evidence of a death this registry previously had no
    /// way to see.
    panes: BTreeMap<String, String>,
    /// Did the pass whose panes these are FAIL part-way?
    ///
    /// A pane that changed identity between two passes is this registry's only
    /// positive evidence of a death — but it is evidence only when the pass
    /// that ran in between was the ordinary kind. A pass that fail-stops has
    /// already executed its kills and its splits and then abandoned the rest,
    /// so the panes it leaves behind moved because THIS ACTUATOR moved them.
    /// Charging that to the people is charging them for the actuator's own
    /// abandoned work.
    ///
    /// THE WEDGE THIS ENDS. One un-tagged stray pane made `select-layout`
    /// unappliable, so every converge pass died on the same step after minting
    /// twelve fresh panes. Five passes later this registry had counted five
    /// consecutive deaths for all thirteen people. Under the give-up that
    /// parked the whole company; under backoff it would still have throttled
    /// thirteen healthy people to one attempt every ten seconds for no reason.
    /// The module doc already refuses to count a pass whose spawn step failed;
    /// a pass that failed BEFORE the spawn step is the same question and this
    /// is where it is answered.
    previous_pass_failed: bool,
}

/// Everything the screen is told about one person who will not stay up.
///
/// Pure data. The rail draws it, the person's card draws it, and a test can
/// hold the whole of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReport {
    /// How many consecutive boots have failed. The operator's "retry number".
    pub failures: u32,
    /// How long this run of failures has been going on.
    pub elapsed: Duration,
    /// How long until the next attempt. Zero once it is due.
    pub retry_in: Duration,
    /// What went wrong, in one or two sentences.
    pub last_error: Option<String>,
}

impl CrashReport {
    /// THE CAUSE, AND NEVER AN EMPTY ONE.
    ///
    /// Every surface that tells somebody a person is crash-looping reads the
    /// cause through here, so a blank cause cannot be published. When the
    /// actuator learned a sentence — tmux's own words, chiefd's own words —
    /// that sentence is the answer. When it learned nothing, the pane walk is
    /// still evidence and [`NO_ERROR_LEARNED`] says what it proves and where to
    /// look next.
    ///
    /// The `crash-looping` log line used to read `report.last_error` and print
    /// the empty string for `None`, so a live outage produced hundreds of lines
    /// that named a person, a count and no cause at all. That is the defect
    /// this method exists to make unconstructible.
    #[must_use]
    pub fn cause(&self) -> &str {
        match self.last_error.as_deref() {
            Some(detail) if !detail.trim().is_empty() => detail,
            _ => NO_ERROR_LEARNED,
        }
    }
}

impl CrashLoop {
    /// A fresh registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record what this pass observed.
    ///
    /// `observed` is the set of people whose pane was found alive. `desired` is
    /// person → launch hash, exactly as chiefd published it. `now` is this
    /// pass's clock reading, injected so the backoff curve is a value a test
    /// can drive rather than a duration a test has to sleep through.
    ///
    /// Call this ONLY after a pass that actually observed tmux. A pass that
    /// could not look must not reach here — see the module doc.
    pub fn observed(
        &mut self,
        desired: &BTreeMap<String, String>,
        observed: &BTreeMap<String, String>,
        now: Instant,
    ) {
        // Verdict on the previous pass's spawns, before anything else touches
        // the counts.
        let spawned = std::mem::take(&mut self.pending);
        for person_id in spawned {
            let Some(launch_hash) = desired.get(&person_id) else {
                // No longer desired. Not a failure, and not a person to
                // remember: chiefd stopped asking, so the question is gone.
                self.people.remove(&person_id);
                continue;
            };
            if observed.contains_key(&person_id) {
                // A pane exists for them, which is NOT yet a surviving boot.
                // The pane walk below decides, because only it can tell a pane
                // that persisted from a pane that was replaced since last pass.
                continue;
            }
            if self.previous_pass_failed {
                // The pass that spawned them abandoned itself part-way. An
                // absent pane after that is the abandoned plan, not a process
                // that exited — the same reason the pane walk below stops
                // counting.
                continue;
            }
            self.fail(person_id, launch_hash, now);
        }

        // Survival, judged by the pane rather than by the person.
        for (person_id, pane) in observed {
            let Some(launch_hash) = desired.get(person_id) else { continue };
            // Cloned before the match so the arms may take `&mut self`.
            let previous = self.panes.get(person_id).cloned();
            match previous.as_deref() {
                // The SAME pane is still there: this person survived a whole
                // pass, which is the only evidence of a boot that took. The
                // run of failures is over and the count starts from nothing.
                Some(previous) if previous == pane.as_str() => {
                    self.people.remove(person_id);
                }
                // A DIFFERENT pane: the process this registry was watching is
                // gone and something has already replaced it. That is a death,
                // and counting it is the entire point of this map — UNLESS the
                // pass in between fail-stopped, in which case the actuator's
                // own half-applied plan is what replaced the pane. See
                // `previous_pass_failed`.
                Some(_) if !self.previous_pass_failed => {
                    self.fail(person_id.clone(), launch_hash, now);
                }
                Some(_) => {}
                // First sighting. Nothing to compare against yet; the next pass
                // is what judges it.
                None => {}
            }
        }
        self.panes.clone_from(observed);
        // A person chiefd no longer desires, or whose launch hash moved, is a
        // question this registry is no longer answering.
        self.people.retain(|person_id, attempts| {
            desired.get(person_id).is_some_and(|hash| hash == &attempts.launch_hash)
        });
    }

    /// Count one failed boot for a person, against the launch chiefd wants.
    fn fail(&mut self, person_id: String, launch_hash: &str, now: Instant) {
        let entry = self.people.entry(person_id).or_insert_with(|| Attempts {
            failures: 0,
            launch_hash: launch_hash.to_owned(),
            first_failed_at: now,
            last_failed_at: now,
            last_error: None,
        });
        if entry.launch_hash != launch_hash {
            // A new launch hash is a new question. Start again.
            entry.launch_hash = launch_hash.to_owned();
            entry.failures = 0;
            entry.first_failed_at = now;
            entry.last_error = None;
        }
        if entry.failures == 0 {
            entry.first_failed_at = now;
        }
        entry.failures = entry.failures.saturating_add(1);
        entry.last_failed_at = now;
    }

    /// Attach the sentence the operator reads to a person's current run of
    /// failures.
    ///
    /// The actuator knows things the pane walk cannot: the step error tmux
    /// answered with, chiefd's own words when a launch was declined. Whatever
    /// it learns about a person who is crash-looping belongs on their card.
    /// Recorded only for a person who HAS a live run of failures — an error
    /// about somebody who is up is not a crash report.
    pub fn note_error(&mut self, person_id: &str, detail: impl Into<String>) {
        if let Some(attempts) = self.people.get_mut(person_id) {
            attempts.last_error = Some(detail.into());
        }
    }

    /// Note that this pass is about to spawn these people.
    ///
    /// Their fate is judged by the NEXT pass's observation, which is the only
    /// place the evidence exists.
    pub fn spawning(&mut self, people: impl IntoIterator<Item = String>) {
        self.pending = people.into_iter().collect();
    }

    /// Record whether the pass that just ended FAILED part-way.
    ///
    /// Called once per pass, beside [`CrashLoop::spawning`], and read by the
    /// next [`CrashLoop::observed`]. A failed pass leaves panes it moved and
    /// steps it never ran, so the next observation is not evidence about
    /// anybody's process. See `previous_pass_failed`.
    pub fn pass_failed(&mut self, failed: bool) {
        self.previous_pass_failed = failed;
    }

    /// Whether this person's next boot attempt is due yet.
    ///
    /// A person with no failures is always due. A person inside their backoff
    /// window is not, and their spawn step is skipped for this pass only —
    /// never for ever.
    #[must_use]
    pub fn retry_due(&self, person_id: &str, now: Instant) -> bool {
        let Some(attempts) = self.people.get(person_id) else { return true };
        now.saturating_duration_since(attempts.last_failed_at) >= retry_delay(attempts.failures)
    }

    /// Everyone whose next attempt is not due yet, for the plan to skip.
    #[must_use]
    pub fn waiting(&self, now: Instant) -> BTreeSet<String> {
        self.people
            .iter()
            .filter(|(person_id, _)| !self.retry_due(person_id, now))
            .map(|(person_id, _)| person_id.clone())
            .collect()
    }

    /// What the screen is told, per person who is crash-looping.
    #[must_use]
    pub fn reports(&self, now: Instant) -> BTreeMap<String, CrashReport> {
        self.people
            .iter()
            .map(|(person_id, attempts)| {
                let waited = now.saturating_duration_since(attempts.last_failed_at);
                (
                    person_id.clone(),
                    CrashReport {
                        failures: attempts.failures,
                        elapsed: now.saturating_duration_since(attempts.first_failed_at),
                        retry_in: retry_delay(attempts.failures).saturating_sub(waited),
                        last_error: attempts.last_error.clone(),
                    },
                )
            })
            .collect()
    }
}

/// The operator-facing sentence for one person who will not stay up.
///
/// Pure, so the line an operator reads is a value a test can hold. It says WHO,
/// HOW MANY consecutive boots failed, HOW LONG it has been going on, WHEN the
/// next attempt is, and WHAT WENT WRONG. Every one of those is in the
/// operator's own list of what they need in order to decide whether to step in.
///
/// THE LAST CLAUSE IS NEVER EMPTY, and that is deliberate. When the actuator
/// learned a sentence — a tmux verb that said no, a host that could not run —
/// it says it. When it learned nothing, it says the one thing it does know:
/// the pane it minted was gone by the next pass, which means the process
/// exited during start-up and the operator should look at that person's own
/// launch. A line that trails off after the numbers reads as a report the
/// product did not finish writing, and the operator has nowhere to go next.
#[must_use]
pub fn crash_loop_line(company: &str, person_id: &str, report: &CrashReport) -> String {
    let error = format!(" Last error: {}", report.cause());
    format!(
        "{company}: '{person_id}' has failed to stay up {} times in a row over {}; retrying in \
         {} and for as long as chiefd wants them up.{error}",
        report.failures,
        human_duration(report.elapsed),
        human_duration(report.retry_in),
    )
}

/// What the actuator knows when it learned no sentence of its own.
///
/// Not "unknown". The pane walk IS evidence — a pane this actuator minted that
/// was gone by the next pass held a process that exited — and naming that
/// evidence tells the operator exactly where to look.
pub const NO_ERROR_LEARNED: &str =
    "the pane the actuator started for them was gone by the next converge pass, so their process \
     exited during start-up; their own launch is where to look";

/// A duration an operator can read at a glance.
///
/// Rounded to whole seconds above one second, because a crash loop measured to
/// the millisecond tells nobody anything, and to milliseconds below it, because
/// the first retry is faster than a second and "0s" would read as "never".
#[must_use]
pub fn human_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds == 0 {
        return format!("{}ms", duration.subsec_millis());
    }
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", seconds % 60);
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests;
