//! The crash loop never gives up, and says what is happening while it does not.

use super::*;

fn desired(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(person, hash)| ((*person).to_owned(), (*hash).to_owned())).collect()
}

/// Seen alive, each in their own stable pane — the ordinary case, where a
/// person's pane persists from pass to pass.
fn seen(people: &[&str]) -> BTreeMap<String, String> {
    people.iter().map(|person| ((*person).to_owned(), format!("%{person}"))).collect()
}

/// Seen alive in an EXPLICIT pane, for the respawn cases: the same person in a
/// different pane between passes is a process that died.
fn seen_in(people: &[(&str, &str)]) -> BTreeMap<String, String> {
    people.iter().map(|(person, pane)| ((*person).to_owned(), (*pane).to_owned())).collect()
}

/// A clock a test drives, so the backoff curve is asserted rather than slept
/// through. Every crash below happens far enough apart that the delay has
/// always elapsed, unless the test is specifically about the delay.
struct Clock {
    base: Instant,
    at: Duration,
}

impl Clock {
    fn new() -> Self {
        Self { base: Instant::now(), at: Duration::ZERO }
    }
    fn now(&self) -> Instant {
        self.base + self.at
    }
    fn advance(&mut self, by: Duration) {
        self.at += by;
    }
}

/// Crash a person once: spawned last pass, and their pane is gone this pass.
fn crash(registry: &mut CrashLoop, set: &BTreeMap<String, String>, person: &str, now: Instant) {
    registry.spawning([person.to_owned()]);
    registry.observed(set, &seen(&[]), now);
}

// ---------------------------------------------------------------------------
// NEVER GIVE UP
// ---------------------------------------------------------------------------

/// THE LIVE WEDGE, 2026-08-19, and the whole reason this module was rewritten.
///
/// `ivo`, `sasha`, `eli` and `rune` crash-looped through a 90-second chiefd
/// outage on the owner's box, hit the five-failure limit, and were dropped from
/// placement PERMANENTLY. An hour and a half after the outage ended they were
/// still down, still desired-active in chiefd's store, and the rail still drew
/// `starting` at an operator whose clicks did nothing. The hold's only escapes
/// needed a live pane, and a person dropped from placement never gets one.
///
/// Twenty consecutive failures — four times the limit that used to exist — and
/// the person is still being attempted.
#[test]
fn a_person_who_keeps_crashing_keeps_being_retried_for_ever() {
    let set = desired(&[("ivo", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for _ in 0..20 {
        clock.advance(MAX_RETRY_DELAY);
        crash(&mut registry, &set, "ivo", clock.now());
    }
    let report = registry.reports(clock.now());
    assert_eq!(report["ivo"].failures, 20, "every failure is counted");
    assert_eq!(report["ivo"].retry_in, MAX_RETRY_DELAY, "and waits the ceiling, never for ever");
    clock.advance(MAX_RETRY_DELAY);
    assert!(
        registry.retry_due("ivo", clock.now()),
        "twenty consecutive failures and the next attempt is still due: there is no give-up"
    );
    assert!(
        registry.waiting(clock.now()).is_empty(),
        "nobody is withheld from the plan once their delay has elapsed"
    );
}

/// The delay is the only consequence of a failure, and it is bounded.
#[test]
fn the_backoff_grows_exponentially_and_stops_at_ten_seconds() {
    assert_eq!(retry_delay(0), Duration::ZERO, "a person with no failures waits for nothing");
    assert_eq!(retry_delay(1), Duration::from_millis(500));
    assert_eq!(retry_delay(2), Duration::from_secs(1));
    assert_eq!(retry_delay(3), Duration::from_secs(2));
    assert_eq!(retry_delay(4), Duration::from_secs(4));
    assert_eq!(retry_delay(5), Duration::from_secs(8));
    assert_eq!(retry_delay(6), MAX_RETRY_DELAY, "the ceiling is reached at the sixth failure");
    for failures in 6..1_000 {
        assert_eq!(
            retry_delay(failures),
            MAX_RETRY_DELAY,
            "and every failure after it waits exactly the ceiling, for ever"
        );
    }
}

/// The delay is real: a person inside their window is withheld from THIS pass
/// and no other.
#[test]
fn a_person_inside_their_backoff_window_is_withheld_and_then_released() {
    let set = desired(&[("ivo", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    // Five consecutive failures, each retried the moment it was due, which is
    // exactly where the old limit stopped trying.
    for failure in 1..=5 {
        crash(&mut registry, &set, "ivo", clock.now());
        assert!(!registry.retry_due("ivo", clock.now()), "the delay starts at the failure");
        clock.advance(retry_delay(failure));
        assert!(registry.retry_due("ivo", clock.now()), "and ends when it elapses");
    }
    assert_eq!(registry.reports(clock.now())["ivo"].failures, 5);
    // The sixth failure waits the ceiling, and not a millisecond more.
    crash(&mut registry, &set, "ivo", clock.now());
    clock.advance(MAX_RETRY_DELAY - Duration::from_millis(1));
    assert_eq!(
        registry.waiting(clock.now()).into_iter().collect::<Vec<_>>(),
        vec!["ivo".to_owned()],
        "one millisecond short of the ceiling they are still waiting"
    );
    clock.advance(Duration::from_millis(1));
    assert!(registry.waiting(clock.now()).is_empty(), "and at the ceiling they are attempted");
}

/// THE SELF-HEAL. The owner's box could not do this: the outage ended and
/// nothing changed. A person whose fault clears comes back up with no operator
/// action, and their count goes with it.
#[test]
fn a_person_whose_fault_clears_comes_back_up_on_their_own() {
    let set = desired(&[("ivo", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for _ in 0..12 {
        clock.advance(MAX_RETRY_DELAY);
        crash(&mut registry, &set, "ivo", clock.now());
    }
    assert_eq!(registry.reports(clock.now())["ivo"].failures, 12);
    // The fault clears. The next attempt mints a pane, and the pass after that
    // finds the SAME pane still there.
    clock.advance(MAX_RETRY_DELAY);
    registry.spawning(["ivo".to_owned()]);
    registry.observed(&set, &seen(&["ivo"]), clock.now());
    clock.advance(Duration::from_secs(1));
    registry.observed(&set, &seen(&["ivo"]), clock.now());
    assert!(
        registry.reports(clock.now()).is_empty(),
        "a pane that survives a whole pass ends the crash loop, with nothing to release"
    );
}

// ---------------------------------------------------------------------------
// WHAT THE SCREEN IS TOLD
// ---------------------------------------------------------------------------

/// The operator asked for the retry number, how long it has been going on, and
/// a sentence or two about the error. All three, or the report is not doing its
/// job.
#[test]
fn the_report_carries_the_count_the_elapsed_time_and_the_error() {
    let set = desired(&[("ivo", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    crash(&mut registry, &set, "ivo", clock.now());
    registry.note_error("ivo", "pi exited during extension bind: chiefd unavailable (timeout)");
    for _ in 0..3 {
        clock.advance(Duration::from_secs(30));
        crash(&mut registry, &set, "ivo", clock.now());
    }
    let report = registry.reports(clock.now())["ivo"].clone();
    assert_eq!(report.failures, 4);
    assert_eq!(report.elapsed, Duration::from_secs(90), "measured from the FIRST failure");
    assert_eq!(report.retry_in, retry_delay(4));
    assert_eq!(
        report.last_error.as_deref(),
        Some("pi exited during extension bind: chiefd unavailable (timeout)"),
        "the sentence the actuator learned survives every later failure"
    );

    let line = crash_loop_line("acme", "ivo", &report);
    assert!(line.contains("'ivo'"), "{line}");
    assert!(line.contains('4'), "the retry number: {line}");
    assert!(line.contains("1m 30s"), "how long it has been going on: {line}");
    assert!(line.contains("extension bind"), "what went wrong: {line}");
    assert!(line.contains("retrying"), "and that it has not given up: {line}");
    assert!(!line.contains("STOPPED"), "nothing stops any more: {line}");
}

/// THE LINE NEVER TRAILS OFF. A crash loop the actuator learned no sentence
/// about still has one fact worth saying, and a line that stops after the
/// numbers leaves the operator with nowhere to look.
#[test]
fn a_crash_with_no_learned_error_still_says_what_is_known() {
    let set = desired(&[("ivo", "aaa")]);
    let mut registry = CrashLoop::new();
    let clock = Clock::new();
    crash(&mut registry, &set, "ivo", clock.now());
    let report = registry.reports(clock.now())["ivo"].clone();
    assert_eq!(report.last_error, None, "nothing was learned");
    let line = crash_loop_line("acme", "ivo", &report);
    assert!(line.contains(NO_ERROR_LEARNED), "and the line says so anyway: {line}");
    assert!(line.contains("exited during start-up"), "naming where to look: {line}");
}

/// An error is a fact about a run of failures, so it goes when the run does.
#[test]
fn a_recovered_person_carries_no_error_into_their_next_crash() {
    let set = desired(&[("ivo", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    crash(&mut registry, &set, "ivo", clock.now());
    registry.note_error("ivo", "the old sentence");
    clock.advance(Duration::from_secs(1));
    registry.observed(&set, &seen(&["ivo"]), clock.now());
    clock.advance(Duration::from_secs(1));
    registry.observed(&set, &seen(&["ivo"]), clock.now());
    assert!(registry.reports(clock.now()).is_empty());
    clock.advance(Duration::from_secs(1));
    crash(&mut registry, &set, "ivo", clock.now());
    let report = registry.reports(clock.now())["ivo"].clone();
    assert_eq!(report.failures, 1, "the count starts again");
    assert_eq!(report.last_error, None, "and so does the sentence");
}

#[test]
fn an_error_about_a_person_who_is_not_crashing_is_not_recorded() {
    let mut registry = CrashLoop::new();
    registry.note_error("ivo", "a step failure that has nothing to do with them");
    assert!(registry.reports(Instant::now()).is_empty());
}

#[test]
fn durations_read_the_way_an_operator_reads_them() {
    assert_eq!(human_duration(Duration::from_millis(500)), "500ms");
    assert_eq!(human_duration(Duration::from_secs(9)), "9s");
    assert_eq!(human_duration(Duration::from_secs(90)), "1m 30s");
    assert_eq!(human_duration(Duration::from_secs(3_720)), "1h 2m");
}

// ---------------------------------------------------------------------------
// WHAT COUNTS AS A FAILED BOOT — the hard-won rules, kept
// ---------------------------------------------------------------------------

/// THE LIVE FAILURE, 2026-08-18. Five people respawned about once a second for
/// half an hour, in complete silence, and the actuator called every pass a
/// success. Their processes lived longer than the gap between passes, so a pane
/// for them was ALWAYS present at the next observation. The pane id is the
/// evidence that was being thrown away.
#[test]
fn a_person_respawning_into_a_new_pane_every_pass_is_counted() {
    let set = desired(&[("priya", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    // The FIRST sighting has nothing to compare against, so it establishes the
    // baseline rather than counting against anybody.
    for pass in 0..=5 {
        clock.advance(MAX_RETRY_DELAY);
        registry.spawning(["priya".to_owned()]);
        registry.observed(&set, &seen_in(&[("priya", &format!("%{pass}"))]), clock.now());
    }
    assert_eq!(
        registry.reports(clock.now())["priya"].failures,
        5,
        "a pane that is replaced every pass is a process that dies every pass"
    );
}

/// THE LIVE WEDGE, 2026-08-18. One un-tagged stray pane made `select-layout`
/// unappliable, so every converge pass fail-stopped AFTER its splits had run.
/// The registry saw thirteen people in a different pane every pass and called
/// each one a death. A pass that FAILED is not evidence about anybody's
/// process.
#[test]
fn pane_churn_caused_by_a_failed_pass_is_not_a_death() {
    let set = desired(&[("priya", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for pass in 0..10 {
        clock.advance(MAX_RETRY_DELAY);
        registry.spawning(["priya".to_owned()]);
        registry.pass_failed(true);
        registry.observed(&set, &seen_in(&[("priya", &format!("%{pass}"))]), clock.now());
    }
    assert!(
        registry.reports(clock.now()).is_empty(),
        "panes the actuator's own abandoned plan replaced are not deaths"
    );
}

/// The rule is about the FAILED pass and nothing wider.
#[test]
fn pane_churn_after_the_passes_recover_is_a_death_again() {
    let set = desired(&[("priya", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for pass in 0..10 {
        clock.advance(MAX_RETRY_DELAY);
        registry.spawning(["priya".to_owned()]);
        registry.pass_failed(true);
        registry.observed(&set, &seen_in(&[("priya", &format!("%stuck{pass}"))]), clock.now());
    }
    assert!(registry.reports(clock.now()).is_empty());
    for pass in 0..4 {
        clock.advance(MAX_RETRY_DELAY);
        registry.spawning(["priya".to_owned()]);
        registry.pass_failed(false);
        registry.observed(&set, &seen_in(&[("priya", &format!("%live{pass}"))]), clock.now());
    }
    assert_eq!(
        registry.reports(clock.now())["priya"].failures,
        4,
        "a person who really does respawn every pass is counted again"
    );
}

#[test]
fn a_persisting_pane_clears_earlier_failures() {
    let set = desired(&[("priya", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for pass in 0..4 {
        clock.advance(MAX_RETRY_DELAY);
        registry.spawning(["priya".to_owned()]);
        registry.observed(&set, &seen_in(&[("priya", &format!("%{pass}"))]), clock.now());
    }
    assert_eq!(registry.reports(clock.now())["priya"].failures, 3);
    for _ in 0..3 {
        clock.advance(Duration::from_secs(1));
        registry.observed(&set, &seen_in(&[("priya", "%settled")]), clock.now());
    }
    assert!(
        registry.reports(clock.now()).is_empty(),
        "the same pane across passes is a boot that took"
    );
}

#[test]
fn a_person_who_boots_and_stays_up_never_appears_in_a_report() {
    let set = desired(&[("vera", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for _ in 0..15 {
        clock.advance(Duration::from_secs(1));
        registry.spawning(["vera".to_owned()]);
        registry.observed(&set, &seen(&["vera"]), clock.now());
    }
    assert!(registry.reports(clock.now()).is_empty());
    assert!(registry.retry_due("vera", clock.now()));
}

/// One person's broken workspace must not slow anybody else down.
#[test]
fn one_persons_crash_loop_leaves_everybody_else_alone() {
    let set = desired(&[("vera", "aaa"), ("chief", "bbb")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for _ in 0..8 {
        clock.advance(MAX_RETRY_DELAY);
        crash(&mut registry, &set, "vera", clock.now());
    }
    assert_eq!(registry.reports(clock.now())["vera"].failures, 8);
    assert!(registry.retry_due("chief", clock.now()), "nobody else is throttled");
    assert!(!registry.reports(clock.now()).contains_key("chief"));
}

/// A new launch hash is a NEW question — the operator who changed it is
/// entitled to see it tried at once, not after the old question's ceiling.
#[test]
fn a_changed_launch_hash_starts_the_count_again() {
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for _ in 0..8 {
        clock.advance(MAX_RETRY_DELAY);
        crash(&mut registry, &desired(&[("vera", "aaa")]), "vera", clock.now());
    }
    assert_eq!(registry.reports(clock.now())["vera"].failures, 8);
    registry.observed(&desired(&[("vera", "bbb")]), &seen(&[]), clock.now());
    assert!(registry.reports(clock.now()).is_empty(), "a different hash is a different question");
    assert!(registry.retry_due("vera", clock.now()));
}

/// A pass this actuator did not spawn in contributes no verdict.
#[test]
fn only_a_boot_this_actuator_attempted_can_fail() {
    let set = desired(&[("vera", "aaa")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for _ in 0..10 {
        clock.advance(Duration::from_secs(1));
        registry.observed(&set, &seen(&[]), clock.now());
    }
    assert!(registry.reports(clock.now()).is_empty());
}

#[test]
fn a_person_no_longer_desired_is_forgotten() {
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for _ in 0..6 {
        clock.advance(MAX_RETRY_DELAY);
        crash(&mut registry, &desired(&[("vera", "aaa")]), "vera", clock.now());
    }
    assert!(!registry.reports(clock.now()).is_empty());
    registry.observed(&desired(&[]), &seen(&[]), clock.now());
    assert!(registry.reports(clock.now()).is_empty());
}

/// THE BUG THE REVIEW CAUGHT, pinned so it cannot come back. The interpreter is
/// fail-stop: a plan that fails at step k never attempts step k+1, so the people
/// ordered behind the failure were not tried, and blaming them would throttle a
/// healthy company to one attempt every ten seconds.
#[test]
fn a_person_whose_boot_was_never_attempted_never_accrues_a_failure() {
    let set = desired(&[("vera", "aaa"), ("theo", "bbb")]);
    let mut registry = CrashLoop::new();
    let mut clock = Clock::new();
    for _ in 0..9 {
        clock.advance(MAX_RETRY_DELAY);
        registry.spawning(["vera".to_owned()]);
        registry.observed(&set, &seen(&[]), clock.now());
    }
    assert!(registry.reports(clock.now()).contains_key("vera"));
    assert!(
        !registry.reports(clock.now()).contains_key("theo"),
        "a person nobody tried to boot has not failed to boot"
    );
    assert!(registry.retry_due("theo", clock.now()));
}

// ---------------------------------------------------------------------------
// A CAUSE THAT CANNOT BE EMPTY (the 2026-08-26 start outage)
// ---------------------------------------------------------------------------

/// THE RULE: no surface may publish a crash report with no cause.
///
/// The `actuator.person.crash-looping` log line read `last_error` directly and
/// printed the empty string when nothing had been learned. A live company
/// produced that line every five seconds for seven minutes with a blank cause
/// on every one of them, and nobody could tell whether the actuator had learned
/// nothing or the field was broken. `cause()` is the only reader now, and it is
/// not constructible empty: from `None`, from an empty sentence, or from a
/// sentence of blanks, it answers with the pane-walk evidence instead.
#[test]
fn a_crash_report_can_never_carry_an_empty_cause() {
    for last_error in [None, Some(String::new()), Some("   \n ".to_owned())] {
        let report = CrashReport {
            failures: 3,
            elapsed: Duration::from_secs(9),
            retry_in: Duration::ZERO,
            last_error,
        };
        assert_eq!(
            report.cause(),
            NO_ERROR_LEARNED,
            "an absent or blank sentence falls back to the evidence the pane walk gives"
        );
        assert!(!report.cause().trim().is_empty(), "and the cause is never empty");
        assert!(
            crash_loop_line("acme", "ivo", &report).contains(NO_ERROR_LEARNED),
            "the operator's line says it too"
        );
    }
}

/// And a learned sentence is never replaced by the fallback.
#[test]
fn a_learned_sentence_is_what_the_cause_says() {
    let report = CrashReport {
        failures: 1,
        elapsed: Duration::from_secs(1),
        retry_in: Duration::ZERO,
        last_error: Some("tmux select-layout said no: no space for new pane".to_owned()),
    };
    assert_eq!(report.cause(), "tmux select-layout said no: no space for new pane");
}
