//! Durable reminder tests. Real `Ledgers`, the northstar manifest, no mocks and
//! no sleeps — time advances by setting the clock, exactly as the check-in and
//! deadline tests do.

use super::super::*;
use super::{
    arm_reminder, armed_count, evaluate_reminders, list_reminders, next_due_at, stop_reminder,
    ArmRequest, INVALID_REMINDER, REMINDER_EFFECT_KIND, REMINDER_LIMIT_REACHED, REMINDER_MARKER,
    REMINDER_NOT_IN_SCOPE, UNKNOWN_REMINDER,
};
use crate::clock::WallMillis;
use crate::isotime::{iso_millis, parse_iso_millis};
use crate::ledger::Ledgers;
use crate::store::activity::ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS;
use crate::store::organization::{self, OrganizationManifest};
use crate::test_support::northstar_manifest;

const EPOCH: i64 = 1_784_116_800_000;
const CEO: &str = "chief";
const RESEARCHER: &str = "signal-researcher";
/// Heads `quant`, the unit `RESEARCHER` lives in.
const QUANT_HEAD: &str = "quant-head";
/// Heads `it` — a SIBLING unit, so nothing of `RESEARCHER`'s is theirs.
const IT_HEAD: &str = "it-head";
const HOUR: i64 = 60 * 60 * 1_000;

struct World {
    ledgers: Ledgers,
    manifest: OrganizationManifest,
}

impl World {
    fn new() -> Self {
        let mut ledgers = Ledgers::empty(WallMillis(EPOCH));
        let manifest = northstar_manifest(EPOCH);
        organization::create(&mut ledgers, &manifest).expect("manifest");
        seed(&mut ledgers, &manifest).expect("supervision");
        Self { ledgers, manifest }
    }

    fn at(&mut self, millis: i64) {
        self.ledgers.set_now(WallMillis(millis));
    }

    fn arm(&mut self, person: &str, interval_ms: i64, recurring: bool) -> Reminder {
        self.try_arm(person, interval_ms, recurring, None).expect("arm")
    }

    /// Arm `count` DISTINCT reminders — one per slot.
    ///
    /// Filling the cap by arming the same reminder over and over stopped
    /// working when an identical re-arm became the replay of the first one
    /// (a resumed agent must not double-arm). A person who really reaches the
    /// cap reaches it with different reminders, so that is what these tests
    /// arm. The cap assertions themselves are unchanged.
    fn arm_distinct(&mut self, person: &str, count: usize) -> Vec<Reminder> {
        (0..count)
            .map(|index| {
                let manifest = self.manifest.clone();
                arm_reminder(
                    &mut self.ledgers,
                    &manifest,
                    &ArmRequest {
                        person_id: person.to_string(),
                        created_by_person_id: person.to_string(),
                        prompt: format!("Re-read the risk limits ({index})."),
                        interval_ms: HOUR,
                        recurring: true,
                        expires_at: None,
                    },
                )
                .expect("arm")
            })
            .collect()
    }

    fn try_arm(
        &mut self,
        person: &str,
        interval_ms: i64,
        recurring: bool,
        expires_at: Option<&str>,
    ) -> Result<Reminder, ChiefdError> {
        let manifest = self.manifest.clone();
        arm_reminder(
            &mut self.ledgers,
            &manifest,
            &ArmRequest {
                person_id: person.to_string(),
                created_by_person_id: person.to_string(),
                prompt: "Re-read the risk limits.".to_string(),
                interval_ms,
                recurring,
                expires_at: expires_at.map(str::to_string),
            },
        )
    }

    /// Arm on `person` AS `actor` — the cross-person case the authority gate
    /// judges. `try_arm` is the self case and passes the same id twice.
    fn try_arm_as(&mut self, actor: &str, person: &str) -> Result<Reminder, ChiefdError> {
        let manifest = self.manifest.clone();
        arm_reminder(
            &mut self.ledgers,
            &manifest,
            &ArmRequest {
                person_id: person.to_string(),
                created_by_person_id: actor.to_string(),
                prompt: "Re-read the risk limits.".to_string(),
                interval_ms: HOUR,
                recurring: true,
                expires_at: None,
            },
        )
    }

    fn evaluate(&mut self) -> ReminderReport {
        let manifest = self.manifest.clone();
        evaluate_reminders(&mut self.ledgers, &manifest).expect("evaluate")
    }

    fn ledger(&mut self) -> SupervisionLedger {
        let manifest = self.manifest.clone();
        read(&self.ledgers, &manifest).expect("read")
    }

    fn reminder(&mut self, id: &str) -> Reminder {
        self.ledger().reminders.get(id).cloned().expect("reminder")
    }

    /// Every effect of the reminder kind, in enqueue order.
    fn reminder_effects(&mut self) -> Vec<Effect> {
        let ledger = self.ledger();
        ledger
            .effect_order()
            .iter()
            .filter_map(|id| ledger.effect(id))
            .filter(|effect| effect.kind == REMINDER_EFFECT_KIND)
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The gap: a reminder exists at all, and survives.
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_company_arms_nobody() {
    // THE HARD RULE at the ledger level: a seeded reminder would be a schedule
    // nobody asked for, firing work nobody requested.
    let mut world = World::new();
    let ledger = world.ledger();
    assert!(ledger.reminder_order.is_empty());
    assert_eq!(armed_count(&ledger, EPOCH), 0);
    assert_eq!(next_due_at(&ledger, EPOCH), None);
}

#[test]
fn an_armed_reminder_is_durable_across_a_reopen() {
    // The whole point: a reminder lives in the company ledger, not in a Pi
    // session file keyed by a session id a restart invalidates. Re-reading the
    // ledger from the store is the durability this replaces `.pi/loops` for.
    let mut world = World::new();
    let armed = world.arm(RESEARCHER, HOUR, true);

    let reread = world.reminder(&armed.id);
    assert_eq!(reread, armed);
    assert_eq!(reread.person_id, RESEARCHER);
    assert_eq!(reread.status, "active");
    assert_eq!(reread.fire_count, 0);
    assert_eq!(reread.last_fired_at, None);
}

#[test]
fn arming_does_not_fire_immediately() {
    // Arming a reminder is not a request to be reminded right now; a first
    // occurrence at `now` would make every arm a wake.
    let mut world = World::new();
    let armed = world.arm(RESEARCHER, HOUR, true);

    assert_eq!(parse_iso_millis(&armed.next_due_at), Some(EPOCH + HOUR));
    assert!(world.evaluate().fired.is_empty());
}

// ---------------------------------------------------------------------------
// Firing and re-arming.
// ---------------------------------------------------------------------------

#[test]
fn a_due_reminder_fires_and_rearms() {
    let mut world = World::new();
    let armed = world.arm(RESEARCHER, HOUR, true);

    world.at(EPOCH + HOUR);
    let report = world.evaluate();
    assert_eq!(report.fired.len(), 1);
    assert!(report.retired.is_empty());

    let after = world.reminder(&armed.id);
    assert_eq!(after.status, "active", "a recurring reminder stays armed");
    assert_eq!(after.fire_count, 1);
    assert_eq!(after.last_fired_at, Some(iso_millis(EPOCH + HOUR)));
    assert_eq!(
        parse_iso_millis(&after.next_due_at),
        Some(EPOCH + 2 * HOUR),
        "the NEXT occurrence is armed"
    );
}

#[test]
fn the_fired_effect_carries_the_marker_and_the_persons_own_words() {
    // The renderer keys the card on the marker prefix; a body that does not
    // start with it renders no card at all (#41/#103).
    let mut world = World::new();
    world.arm(RESEARCHER, HOUR, true);
    world.at(EPOCH + HOUR);
    world.evaluate();

    let effects = world.reminder_effects();
    assert_eq!(effects.len(), 1);
    let message = effects[0].text("message").expect("message").to_string();
    assert!(message.starts_with(REMINDER_MARKER), "body must lead with the marker: {message}");
    assert!(message.contains("Re-read the risk limits."));
    assert!(message.contains("org_stop_reminder"), "a recurring reminder must show its off switch");
    assert_eq!(effects[0].text("personId"), Some(RESEARCHER));
}

#[test]
fn firing_and_rearming_land_in_one_commit() {
    // The single-commit contract. If the advance committed separately from the
    // enqueue, a crash between them would move `nextDueAt` forward with no
    // effect ever queued for the window — a reminder silently skipped.
    let mut world = World::new();
    let armed = world.arm(RESEARCHER, HOUR, true);
    world.at(EPOCH + HOUR);
    world.evaluate();

    let after = world.ledger();
    // Both halves are visible after the one evaluation.
    assert_eq!(after.reminders[&armed.id].fire_count, 1);
    assert_eq!(world.reminder_effects().len(), 1);
}

#[test]
fn a_repeated_pass_over_the_same_window_does_not_double_fire() {
    let mut world = World::new();
    world.arm(RESEARCHER, HOUR, true);

    world.at(EPOCH + HOUR);
    assert_eq!(world.evaluate().fired.len(), 1);
    // Same instant, second pass: nothing is due any more.
    assert!(world.evaluate().fired.is_empty());
    assert_eq!(world.reminder_effects().len(), 1);
}

#[test]
fn a_chiefd_that_was_down_fires_once_not_a_catch_up_burst() {
    // Skip-aware, exactly as `advance_check_in`. Firing per missed window would
    // flood a returning fleet with backlog — a restart becoming a herd.
    let mut world = World::new();
    let armed = world.arm(RESEARCHER, HOUR, true);

    world.at(EPOCH + 12 * HOUR + 1);
    let report = world.evaluate();

    assert_eq!(report.fired.len(), 1, "ONE fire, not twelve");
    assert_eq!(world.reminder_effects().len(), 1);
    let next = parse_iso_millis(&world.reminder(&armed.id).next_due_at).expect("next");
    assert!(next > EPOCH + 12 * HOUR + 1, "re-armed into the FUTURE, not into the backlog");
    assert_eq!(next, EPOCH + 13 * HOUR, "whole intervals from its own due time");
}

#[test]
fn a_one_shot_fires_exactly_once_and_stops() {
    let mut world = World::new();
    let armed = world.arm(RESEARCHER, HOUR, false);

    world.at(EPOCH + HOUR);
    let report = world.evaluate();
    assert_eq!(report.fired.len(), 1);
    assert_eq!(report.retired, vec![armed.id.clone()]);

    let after = world.reminder(&armed.id);
    assert_eq!(after.status, "stopped");
    assert_eq!(after.stopped_reason.as_deref(), Some("fired"));

    world.at(EPOCH + 10 * HOUR);
    assert!(world.evaluate().fired.is_empty(), "a stopped reminder never fires again");
    assert_eq!(world.reminder_effects().len(), 1);
}

// ---------------------------------------------------------------------------
// Expiry, stopping, and the honest count.
// ---------------------------------------------------------------------------

#[test]
fn a_reminder_whose_next_occurrence_is_past_its_expiry_retires_rather_than_lying() {
    let mut world = World::new();
    let expiry = iso_millis(EPOCH + 90 * 60 * 1_000);
    let armed = world.try_arm(RESEARCHER, HOUR, true, Some(&expiry)).expect("arm");

    world.at(EPOCH + HOUR);
    let report = world.evaluate();
    assert_eq!(report.fired.len(), 1, "it is still due, so it fires");
    assert_eq!(report.retired, vec![armed.id.clone()], "but it can never fire again");

    let after = world.reminder(&armed.id);
    assert_eq!(after.status, "stopped");
    assert_eq!(after.stopped_reason.as_deref(), Some("expired"));
    assert_eq!(armed_count(&world.ledger(), EPOCH + HOUR), 0, "and it does not render as armed");
}

#[test]
fn stopping_is_idempotent_and_keeps_the_row() {
    let mut world = World::new();
    let armed = world.arm(RESEARCHER, HOUR, true);
    world.at(EPOCH + HOUR);
    world.evaluate();

    let manifest = world.manifest.clone();
    let stopped = stop_reminder(&mut world.ledgers, &manifest, RESEARCHER, RESEARCHER, &armed.id)
        .expect("stop");
    assert_eq!(stopped.status, "stopped");
    // The row survives, so `fireCount` stays answerable and the id is never
    // recycled into an effect-id collision.
    assert_eq!(stopped.fire_count, 1);

    let manifest = world.manifest.clone();
    let again = stop_reminder(&mut world.ledgers, &manifest, RESEARCHER, RESEARCHER, &armed.id)
        .expect("again");
    assert_eq!(again.status, "stopped");

    world.at(EPOCH + 5 * HOUR);
    assert!(world.evaluate().fired.is_empty());
}

#[test]
fn stopping_someone_elses_reminder_is_refused_as_unknown() {
    // Reported as UNKNOWN rather than "not yours": the two answers together
    // would let anyone enumerate another person's reminder ids.
    let mut world = World::new();
    let armed = world.arm(RESEARCHER, HOUR, true);

    let manifest = world.manifest.clone();
    let refusal =
        stop_reminder(&mut world.ledgers, &manifest, CEO, CEO, &armed.id).expect_err("refuse");
    assert_eq!(refusal.code(), Some(UNKNOWN_REMINDER));
    assert_eq!(world.reminder(&armed.id).status, "active", "and it stays armed");
}

#[test]
fn a_stopped_reminder_never_reuses_its_id() {
    let mut world = World::new();
    let first = world.arm(RESEARCHER, HOUR, false);
    world.at(EPOCH + HOUR);
    world.evaluate();
    let second = world.arm(RESEARCHER, HOUR, true);

    assert_ne!(first.id, second.id, "reusing the id would collide with published effect ids");
}

#[test]
fn the_armed_count_and_the_alarm_clock_agree_with_the_rows() {
    let mut world = World::new();
    assert_eq!(next_due_at(&world.ledger(), EPOCH), None, "nothing armed: sleep the floor");

    world.arm(RESEARCHER, 4 * HOUR, true);
    let near = world.arm(CEO, HOUR, true);

    let ledger = world.ledger();
    assert_eq!(armed_count(&ledger, EPOCH), 2);
    assert_eq!(
        next_due_at(&ledger, EPOCH),
        parse_iso_millis(&near.next_due_at),
        "the alarm is the EARLIEST armed reminder"
    );
}

#[test]
fn a_past_due_reminder_is_included_in_the_alarm_clock() {
    // The duty must still act on a reminder it has not yet fired, so the alarm
    // cannot skip the past — the same reason `next_due_at_after` is a separate
    // function from `next_due_at` for deadlines.
    let mut world = World::new();
    let armed = world.arm(RESEARCHER, HOUR, true);
    world.at(EPOCH + 5 * HOUR);

    let ledger = world.ledger();
    assert_eq!(next_due_at(&ledger, EPOCH + 5 * HOUR), parse_iso_millis(&armed.next_due_at));
}

#[test]
fn listing_shows_a_persons_own_reminders_armed_first() {
    let mut world = World::new();
    let one_shot = world.arm(RESEARCHER, HOUR, false);
    let recurring = world.arm(RESEARCHER, 2 * HOUR, true);
    world.arm(CEO, HOUR, true);
    world.at(EPOCH + HOUR);
    world.evaluate();

    let listed = list_reminders(&world.ledger(), RESEARCHER);
    assert_eq!(listed.len(), 2, "only this person's reminders");
    assert_eq!(listed[0].id, recurring.id, "armed first");
    assert_eq!(listed[1].id, one_shot.id);
}

// ---------------------------------------------------------------------------
// The fences.
// ---------------------------------------------------------------------------

#[test]
fn a_sub_minute_cadence_is_refused_as_a_poll() {
    // Not taste: every fire is a commit, an effect row, a delivery, and — for a
    // stopped person — a whole agent brought up. A seconds cadence is a poller
    // wearing a reminder's clothes.
    let mut world = World::new();
    let refusal = world.try_arm(RESEARCHER, 5_000, true, None).expect_err("refuse");
    assert_eq!(refusal.code(), Some(INVALID_REMINDER));
    assert!(world.ledger().reminder_order.is_empty(), "a refusal publishes nothing");
}

/// THE MEASURED DEFECT, 2026-08-27. One minute was LEGAL and it made parking
/// unreachable: every fire delivers a turn, every turn resets the settle
/// countdown, and the countdown is five minutes. A person on this cadence stays
/// resident for ever — measured at $2.295 spent correctly deciding that nothing
/// needed doing, once a minute, with the fleet held open throughout.
///
/// The refusal has to EXPLAIN itself, because the input looks reasonable and
/// the reason is two constants away.
#[test]
fn a_reminder_cadence_inside_the_settle_window_is_refused_at_arm() {
    let mut world = World::new();
    let refusal = world
        .try_arm(RESEARCHER, 60 * 1_000, true, None)
        .expect_err("one minute is inside the settle window and must now refuse");
    assert_eq!(refusal.code(), Some(INVALID_REMINDER));

    let said = refusal.to_string();
    assert!(
        said.contains(&format!("{}s", MIN_RECURRING_REMINDER_INTERVAL_MS / 1_000)),
        "it names the floor: {said}"
    );
    assert!(
        said.contains(&format!("{}s", ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS / 1_000)),
        "and the settle window the floor exists to clear: {said}"
    );
    assert!(said.contains("settle countdown"), "and WHY, not just what: {said}");
    assert!(said.contains("never park"), "naming the outcome that was measured: {said}");
    assert!(
        said.contains("inside one turn"),
        "and the remedy for the caller who really does need a fast loop: {said}"
    );
    assert!(world.ledger().reminder_order.is_empty(), "a refusal publishes nothing");
}

/// THE RELATION, pinned so neither constant can move alone.
///
/// The floor was 60s while the lease was 300s, and nothing in the tree said
/// those two numbers had anything to do with each other — so when an operator
/// ruling moved the lease from 120s to 300s, the floor stayed where it was and
/// the gap became a defect nobody could see. The floor is now DERIVED, and this
/// test is what fails if somebody re-writes it as a literal.
///
/// Two lease-lengths, not one: at exactly one lease the fire races the park.
/// At two, the person settles, parks, and the next fire wakes a parked person —
/// which is the designed mailbox-wake path.
#[test]
// THE CONSTANT-NESS IS THE POINT. `assertions_on_constants` exists to catch
// `assert!(true)` — a tautology that tests nothing. This asserts a RELATION
// between two compile-time constants that a human may change independently,
// and it is precisely because both are constants that the relation can be
// broken silently by an edit to either one. A named test is what makes the
// coupling greppable from the constants it couples.
#[allow(clippy::assertions_on_constants)]
fn the_reminder_floor_lets_a_full_park_fit_between_fires() {
    assert!(
        MIN_RECURRING_REMINDER_INTERVAL_MS >= 2 * ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS,
        "the recurring floor ({MIN_RECURRING_REMINDER_INTERVAL_MS}ms) must clear two settle \
         windows \
         ({ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS}ms each): a whole park has to FIT between two \
         fires, or a legal reminder holds its person resident for ever"
    );
    // And the floor is not merely large — it is the lease's own multiple, so
    // moving the lease moves it. A literal that happened to satisfy the bound
    // above would pass that assertion and drift at the next ruling.
    assert_eq!(
        MIN_RECURRING_REMINDER_INTERVAL_MS,
        2 * ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS,
        "the floor is DERIVED from the lease, not written beside it"
    );
    // AND A ONE-SHOT IS NOT GOVERNED BY IT. Its `interval_ms` is a delay, not a
    // cadence: one fire cannot reset a countdown forever, so the hazard this
    // floor exists for cannot occur there.
    assert!(
        MIN_REMINDER_INTERVAL_MS < MIN_RECURRING_REMINDER_INTERVAL_MS,
        "a one-shot keeps the smaller delay fence"
    );
}

/// A ONE-SHOT AT THE DELAY FLOOR IS STILL LEGAL, and it must be.
///
/// The cadence floor is about a countdown that never finishes; one fire cannot
/// do that. "Remind me in two minutes" is a legitimate request with no hazard
/// in it, and forbidding it would also make every live delivery test wait ten
/// minutes to watch one fire — a test nobody would then run.
#[test]
fn a_one_shot_inside_the_settle_window_is_still_allowed() {
    let mut world = World::new();
    let armed = world
        .try_arm(RESEARCHER, MIN_REMINDER_INTERVAL_MS, false, None)
        .expect("a one-shot at the delay floor is not a poll");
    assert!(!armed.recurring, "the case under test is the non-recurring one");
    assert_eq!(armed.interval_ms, MIN_REMINDER_INTERVAL_MS);

    // And the same value RECURRING is refused, which is the whole distinction.
    let refusal = world
        .try_arm(RESEARCHER, MIN_REMINDER_INTERVAL_MS, true, None)
        .expect_err("the identical interval, recurring, is the measured defect");
    assert_eq!(refusal.code(), Some(INVALID_REMINDER));
}

/// LIVE ROWS MIGRATE THEMSELVES, and that is load-bearing rather than lucky.
///
/// Raising the constant fixes every already-armed reminder at its next fire,
/// because the re-arm clamps to the floor — so there is no migration, no
/// backfill, and no window in which an old row keeps its old cadence. That
/// property was previously incidental; this test makes it a promise, because
/// the whole no-migration argument rests on it.
#[test]
fn a_legacy_sub_floor_reminder_is_clamped_at_its_next_re_arm() {
    let mut world = World::new();
    // A row from before the floor moved: armed at a minute, stored directly,
    // because `arm_reminder` would now refuse it — which is the point.
    let legacy = world.arm(RESEARCHER, HOUR, true);
    let due = EPOCH + 60 * 1_000;
    // Planted through the store's own mutate path — the only way in, because
    // `arm_reminder` now refuses this cadence, which is exactly the point: this
    // row can no longer be CREATED, only inherited.
    let manifest = world.manifest.clone();
    mutate(&mut world.ledgers, &manifest, |draft, _at| {
        let stored = draft.ledger.reminders.get_mut(&legacy.id).expect("the armed row");
        stored.interval_ms = 60 * 1_000;
        stored.next_due_at = iso_millis(due);
        Ok(())
    })
    .expect("plant a legacy row");

    world.at(due);
    let manifest = world.manifest.clone();
    evaluate_reminders(&mut world.ledgers, &manifest).expect("fire");

    let stored = world.ledger().reminders.get(&legacy.id).expect("still armed").clone();
    let next = parse_iso_millis(&stored.next_due_at).expect("a next occurrence");
    assert!(
        next - due >= MIN_RECURRING_REMINDER_INTERVAL_MS,
        "the re-arm must clamp a legacy cadence up to the floor: next was {}ms after due",
        next - due
    );
}

#[test]
fn an_empty_prompt_is_refused() {
    let mut world = World::new();
    let manifest = world.manifest.clone();
    let refusal = arm_reminder(
        &mut world.ledgers,
        &manifest,
        &ArmRequest {
            person_id: RESEARCHER.to_string(),
            created_by_person_id: RESEARCHER.to_string(),
            prompt: "   ".to_string(),
            interval_ms: HOUR,
            recurring: true,
            expires_at: None,
        },
    )
    .expect_err("refuse");
    assert_eq!(refusal.code(), Some(INVALID_REMINDER));
}

#[test]
fn an_expiry_already_in_the_past_is_refused() {
    let mut world = World::new();
    let past = iso_millis(EPOCH - HOUR);
    let refusal = world.try_arm(RESEARCHER, HOUR, true, Some(&past)).expect_err("refuse");
    assert_eq!(refusal.code(), Some(INVALID_REMINDER));
}

#[test]
fn an_unknown_person_cannot_be_reminded() {
    let mut world = World::new();
    let manifest = world.manifest.clone();
    let refusal = arm_reminder(
        &mut world.ledgers,
        &manifest,
        &ArmRequest {
            person_id: "nobody".to_string(),
            created_by_person_id: RESEARCHER.to_string(),
            prompt: "Do a thing.".to_string(),
            interval_ms: HOUR,
            recurring: true,
            expires_at: None,
        },
    )
    .expect_err("refuse");
    assert_eq!(refusal.code(), Some(INVALID_REMINDER));
}

// --- who may reach whose reminders -----------------------------------------
//
// `org_create_reminder` has always told an agent that `personId` is "only for
// a manager arming a reminder for someone they manage". Until this gate that
// sentence was false everywhere: the deleted CLI passed the ids through and
// this module only checked both people EXISTED, so any worker could arm a
// recurring wake-up on the CEO. These four tests assert BOTH directions —
// a suite that only proved the allowed case would stay green for ever if the
// gate were removed again.

#[test]
fn a_person_arms_their_own_reminder() {
    let mut world = World::new();
    let armed = world.try_arm_as(RESEARCHER, RESEARCHER).expect("self-service needs no manager");
    assert_eq!(armed.person_id, RESEARCHER);
    assert_eq!(armed.created_by_person_id, RESEARCHER);
}

#[test]
fn a_manager_arms_a_reminder_on_somebody_they_manage() {
    let mut world = World::new();
    // The head of the unit the worker lives in, and the executive above it:
    // both directions of "manages" the product recognizes.
    let by_head = world.try_arm_as(QUANT_HEAD, RESEARCHER).expect("the head manages the worker");
    assert_eq!(by_head.created_by_person_id, QUANT_HEAD);
    let by_ceo = world.try_arm_as(CEO, RESEARCHER).expect("the executive manages every unit");
    assert_eq!(by_ceo.created_by_person_id, CEO);
}

/// The half of the manager rule the scope gate cannot see: WHO gets woken.
///
/// `ensure_reminder_scope` decides whether a manager may arm one at all, and
/// stops there — every scope test asserts the refusal or the
/// `created_by_person_id` and never fires the thing. But a reminder's whole
/// product is the wake, and the wake belongs to the person the reminder NAMES,
/// not to the manager who armed it. Routing reads `personId` alone
/// (`dispatch::recipients_for`), so a producer that ever wrote the creator
/// there would wake the manager on their report's cadence and the report would
/// never hear about it — the reminder would look armed, fire on time, and
/// deliver to the wrong person, which no assertion in the suite could see.
#[test]
fn a_manager_armed_reminder_wakes_its_owner_and_never_the_manager() {
    let mut world = World::new();
    let armed = world.try_arm_as(QUANT_HEAD, RESEARCHER).expect("the head manages the worker");
    assert_eq!(armed.person_id, RESEARCHER, "the owner is the person it names");
    assert_eq!(armed.created_by_person_id, QUANT_HEAD, "credited to the manager who armed it");

    world.at(EPOCH + HOUR);
    world.evaluate();

    let effects = world.reminder_effects();
    assert_eq!(effects.len(), 1, "one due reminder enqueues exactly one effect");
    assert_eq!(
        effects[0].text("personId"),
        Some(RESEARCHER),
        "the wake is addressed to the owner, not the manager who armed it"
    );
    assert_ne!(effects[0].text("personId"), Some(QUANT_HEAD));

    // And the manager owns nothing by having armed it: their own list is empty,
    // so the reminder cannot be stopped out from under the owner by appearing
    // in two people's rosters at once.
    assert!(
        list_reminders(&world.ledger(), QUANT_HEAD).is_empty(),
        "arming for somebody else must not put the reminder on the manager's own list"
    );
    let owned = list_reminders(&world.ledger(), RESEARCHER);
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0].id, armed.id);
}

#[test]
fn arming_a_reminder_on_somebody_you_do_not_manage_is_refused() {
    let mut world = World::new();
    // A worker reaching UP at the executive, and a sibling head reaching
    // ACROSS into another head's unit. Neither is in scope.
    let upward = world.try_arm_as(RESEARCHER, CEO).expect_err("a worker manages nobody");
    assert_eq!(upward.code(), Some(REMINDER_NOT_IN_SCOPE));
    let named = upward.to_string();
    assert!(
        named.contains(RESEARCHER) && named.contains(CEO),
        "the refusal must name both people: {named}"
    );

    let sideways = world.try_arm_as(IT_HEAD, RESEARCHER).expect_err("a sibling head is not theirs");
    assert_eq!(sideways.code(), Some(REMINDER_NOT_IN_SCOPE));

    assert!(world.ledger().reminder_order.is_empty(), "a refusal publishes nothing");
}

#[test]
fn stopping_a_reminder_outside_your_scope_is_refused() {
    let mut world = World::new();
    let armed = world.arm(RESEARCHER, HOUR, true);

    let manifest = world.manifest.clone();
    let refusal = stop_reminder(&mut world.ledgers, &manifest, IT_HEAD, RESEARCHER, &armed.id)
        .expect_err("a sibling head may not stop it");
    assert_eq!(refusal.code(), Some(REMINDER_NOT_IN_SCOPE));
    assert_eq!(world.reminder(&armed.id).status, "active", "and it stays armed");

    // The other direction: the head who DOES manage them stops it.
    let manifest = world.manifest.clone();
    let stopped = stop_reminder(&mut world.ledgers, &manifest, QUANT_HEAD, RESEARCHER, &armed.id)
        .expect("their own head may");
    assert_eq!(stopped.status, "stopped");
}

#[test]
fn a_person_cannot_arm_past_the_per_person_limit() {
    // Bounded because the supervision document is rewritten whole on every
    // mutation (#123): an unbounded reminder list is an unbounded per-commit
    // cost paid by every other duty in the company.
    let mut world = World::new();
    world.arm_distinct(RESEARCHER, REMINDERS_PER_PERSON_LIMIT);
    let refusal = world.try_arm(RESEARCHER, HOUR, true, None).expect_err("refuse");
    assert_eq!(refusal.code(), Some(REMINDER_LIMIT_REACHED));

    // The limit is PER PERSON, not company-wide.
    world.arm(CEO, HOUR, true);
}

#[test]
fn a_stopped_reminder_frees_a_slot() {
    let mut world = World::new();
    let armed = world.arm_distinct(RESEARCHER, REMINDERS_PER_PERSON_LIMIT);
    let manifest = world.manifest.clone();
    stop_reminder(&mut world.ledgers, &manifest, RESEARCHER, RESEARCHER, &armed[0].id)
        .expect("stop");

    world.try_arm(RESEARCHER, HOUR, true, None).expect("a freed slot is usable");
}

#[test]
fn a_reminder_never_marks_anyone_as_kept_alive() {
    // THE HARD RULE: a reminder must never hold a person resident merely by
    // existing. It has no `keepsPersonAlive` concept at all.
    let mut world = World::new();
    world.arm(RESEARCHER, HOUR, true);
    let ledger = world.ledger();

    let serialized = serde_json::to_value(&ledger.reminders).expect("serialize");
    assert!(
        !serialized.to_string().contains("keepsPersonAlive"),
        "a reminder has no lease concept to get wrong"
    );
}

/// A pane killed mid-turn resumes and the agent re-arms the reminder it never
/// saw a result for. Two armed copies of one recurring reminder is the worst
/// decay shape on the tool surface — it mails the same prompt forever, and
/// after the fact nothing can tell the copies apart.
#[test]
fn re_arming_an_identical_recurring_reminder_returns_the_one_already_armed() {
    let mut world = World::new();
    let first = world.arm(RESEARCHER, HOUR, true);

    // The resumed agent re-issues the same call, minutes later. No window is
    // consulted, deliberately: an identical armed reminder has no legitimate
    // second version at any distance.
    world.at(EPOCH + 7 * 60 * 1_000);
    let replay = world.arm(RESEARCHER, HOUR, true);

    assert_eq!(replay.id, first.id);
    assert_eq!(replay.created_at, first.created_at, "the replay did not re-stamp the original");
    assert_eq!(replay.next_due_at, first.next_due_at, "the replay did not move the clock forward");
    assert_eq!(armed_count(&world.ledger(), EPOCH), 1);
}

/// The one-shot case, and the proof the guard is not recurring-only.
#[test]
fn re_arming_an_identical_one_shot_reminder_arms_nothing_new() {
    let mut world = World::new();
    let first = world.arm(RESEARCHER, HOUR, false);
    let replay = world.arm(RESEARCHER, HOUR, false);
    assert_eq!(replay.id, first.id);
    assert_eq!(armed_count(&world.ledger(), EPOCH), 1);
}

/// The guard must not swallow a genuinely different reminder. A different
/// cadence for the same words is a second reminder the person asked for.
#[test]
fn a_different_interval_is_a_second_reminder() {
    let mut world = World::new();
    let hourly = world.arm(RESEARCHER, HOUR, true);
    let daily = world.arm(RESEARCHER, 24 * HOUR, true);
    assert_ne!(daily.id, hourly.id);
    assert_eq!(armed_count(&world.ledger(), EPOCH), 2);
}

/// Re-arming something deliberately STOPPED is a real new reminder: the
/// dedupe looks only at what is currently armed.
#[test]
fn re_arming_after_a_stop_arms_a_new_reminder() {
    let mut world = World::new();
    let first = world.arm(RESEARCHER, HOUR, true);
    let manifest = world.manifest.clone();
    stop_reminder(&mut world.ledgers, &manifest, RESEARCHER, RESEARCHER, &first.id).expect("stop");
    assert_eq!(armed_count(&world.ledger(), EPOCH), 0);

    let again = world.arm(RESEARCHER, HOUR, true);
    assert_ne!(again.id, first.id, "a stopped reminder's id is never reused");
    assert_eq!(armed_count(&world.ledger(), EPOCH), 1);
}

/// A replay must not be refused by the very reminder it created. The dedupe
/// therefore runs BEFORE the per-person cap, not after it.
#[test]
fn a_replay_at_the_armed_limit_returns_the_existing_reminder_rather_than_refusing() {
    let mut world = World::new();
    let armed = world.arm_distinct(RESEARCHER, REMINDERS_PER_PERSON_LIMIT);
    assert_eq!(armed_count(&world.ledger(), EPOCH), REMINDERS_PER_PERSON_LIMIT);

    let manifest = world.manifest.clone();
    let replay = arm_reminder(
        &mut world.ledgers,
        &manifest,
        &ArmRequest {
            person_id: RESEARCHER.to_string(),
            created_by_person_id: RESEARCHER.to_string(),
            prompt: "Re-read the risk limits (0).".to_string(),
            interval_ms: HOUR,
            recurring: true,
            expires_at: None,
        },
    )
    .expect("a replay of an armed reminder is not a new arm and must not hit the cap");
    assert_eq!(replay.id, armed[0].id);
    assert_eq!(armed_count(&world.ledger(), EPOCH), REMINDERS_PER_PERSON_LIMIT);
}
