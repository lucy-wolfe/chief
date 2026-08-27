//! Unit tests for the activity paths the conformance corpus does **not** reach.
//!
//! The corpus is the definition of done for the behaviour it recorded, and it
//! records the launch fence and the transition lifecycle. It records almost
//! nothing about `reconcile`'s *structural* half —
//! the transitions derived from a manifest change the persisted state has not
//! caught up with, and the bounded round-robin admission of routine idle parks
//! — because no recorded fixture pauses a department, benches a person, or lets
//! a company sit idle past its quiet lease. That half is ported from
//! `src/organization/org-activity.ts:776-1195` and is covered here by hand,
//! because a port with no coverage is a guess with good formatting.

use super::*;
use crate::clock::WallMillis;
use crate::store::organization::{self, UnitState};
use crate::store::supervision;
use crate::test_support::northstar_manifest;

const EPOCH: i64 = 1_784_116_800_000;

struct World {
    ledgers: Ledgers,
    manifest: OrganizationManifest,
    /// When this chiefd is pretending to have started watching.
    ///
    /// The epoch by default — "watching for ever", which is the pre-clamp rule
    /// and keeps every quiet-instant expectation in this file exact. A test
    /// about a chiefd RESTART moves it with [`World::chiefd_restarted_now`].
    watching_since: String,
}

impl World {
    fn new() -> Self {
        let mut ledgers = Ledgers::empty(WallMillis(EPOCH));
        let manifest = northstar_manifest(EPOCH);
        organization::create(&mut ledgers, &manifest).expect("manifest");
        supervision::seed(&mut ledgers, &manifest).expect("supervision");
        seed(&mut ledgers, &manifest).expect("activity");
        Self { ledgers, manifest, watching_since: iso_millis(0) }
    }

    /// This chiefd started watching NOW: everything before this instant
    /// happened while nobody was listening.
    fn chiefd_restarted_now(&mut self) {
        self.watching_since = iso_millis(self.ledgers.now().0);
    }

    fn supervision(&self) -> SupervisionLedger {
        supervision::read(&self.ledgers, &self.manifest).expect("supervision readable")
    }

    fn ledger(&self) -> ActivityLedger {
        read(&self.ledgers, &self.manifest).expect("activity readable")
    }

    fn advance(&mut self, millis: i64) {
        let now = self.ledgers.now().0 + millis;
        self.ledgers.set_now_for_test(WallMillis(now));
    }

    fn reconcile(&mut self, fence: LaunchFence, requested: &[&str]) -> ActivitySnapshot {
        let supervision = self.supervision();
        let manifest = self.manifest.clone();
        let watching_since = self.watching_since.clone();
        super::reconcile(
            &mut self.ledgers,
            &manifest,
            &supervision,
            &ReconcileInput {
                launch_intent: fence,
                requested_person_ids: requested.iter().map(ToString::to_string).collect(),
                watching_since,
            },
        )
        .expect("reconcile")
    }

    fn begin(&mut self, person: &str, action: TransitionAction, intent: Option<&str>) -> String {
        let manifest = self.manifest.clone();
        let supervision = self.supervision();
        begin_transition(
            &mut self.ledgers,
            &manifest,
            &supervision,
            &BeginTransitionInput {
                person_id: person.to_string(),
                action,
                reason: "Test transition.".to_string(),
                to_department_id: None,
                intent_id: intent.map(ToString::to_string),
            },
        )
        .expect("a transition opens")
        .id
    }

    /// [`World::begin`] for the two actions that carry a destination.
    fn begin_to(
        &mut self,
        person: &str,
        action: TransitionAction,
        to_department_id: &str,
    ) -> Result<String, ChiefdError> {
        let manifest = self.manifest.clone();
        let supervision = self.supervision();
        begin_transition(
            &mut self.ledgers,
            &manifest,
            &supervision,
            &BeginTransitionInput {
                person_id: person.to_string(),
                action,
                reason: "Test transition.".to_string(),
                to_department_id: Some(to_department_id.to_string()),
                intent_id: None,
            },
        )
        .map(|transition| transition.id)
    }

    fn settle(&mut self, person: &str) -> bool {
        let manifest = self.manifest.clone();
        let supervision = self.supervision();
        settle_applied_move(&mut self.ledgers, &manifest, &supervision, person)
            .expect("the settle commits")
    }

    /// What this person's OWN pane says the agent is doing -- the fact the
    /// supervision ledger cannot supply, because "holds no open goal" and "is
    /// not doing anything" are different questions.
    fn note_activity(&mut self, person: &str, working: bool) -> bool {
        let manifest = self.manifest.clone();
        let supervision = self.supervision();
        note_agent_activity(&mut self.ledgers, &manifest, &supervision, person, working)
            .expect("the beat commits")
    }

    /// The operator clicked Wake Up on this person, NOW.
    ///
    /// The durable half of a wake lives in the rows
    /// (`activity::rows::release_idle_park`, which `org_ops::wake_person`
    /// composes) and this World is document-shaped, so the stamp is written
    /// here directly. The SUBJECT is the rule the stamp drives, not the SQL
    /// that writes it — `activity/rows/tests.rs` owns that half.
    fn woken(&mut self, person: &str) {
        self.woken_carrying_a_quiet_clock(person, None);
    }

    /// The operator clicked Wake Up, and the person is left carrying a quiet
    /// clock from BEFORE the click.
    ///
    /// This is the state the floor exists to survive, and it is why the floor
    /// is not merely a restatement of the agent clocks. `release_idle_park`
    /// clears `agent_quiet_at` today, so the two rules happen to agree — but
    /// they are independent facts, reached by different routes, and "these two
    /// clocks happen to agree" is not the operator's ruling. The ruling is that
    /// a wake buys the window outright. A `carry` here is that agreement being
    /// broken, which is exactly what a regression in the wake's row half would
    /// look like.
    fn woken_carrying_a_quiet_clock(&mut self, person: &str, carry: Option<i64>) {
        let mut ledger = self.ledger();
        let at = iso_millis(self.ledgers.now().0);
        let state = ledger.people.get_mut(person).expect("a person to wake");
        state.operator_wake_at = Some(at.clone());
        state.agent_quiet_at = carry.map(iso_millis);
        state.idle_since = carry.map(iso_millis);
        state.updated_at = at;
        super::put(&mut self.ledgers, &ledger).expect("the wake commits");
    }

    fn now_ms(&self) -> i64 {
        self.ledgers.now().0
    }

    fn idle_since(&self, person: &str) -> Option<String> {
        self.ledger().people[person].idle_since.clone()
    }

    fn release_on(&mut self, transition_id: &str, person: &str) {
        let manifest = self.manifest.clone();
        let supervision = self.supervision();
        release(
            &mut self.ledgers,
            &manifest,
            &supervision,
            &ReleaseInput {
                transition_id: transition_id.to_string(),
                person_id: person.to_string(),
            },
        )
        .expect("the release commits");
    }

    /// Apply a structural manifest change, as the staffing verbs will once they
    /// land.
    fn mutate_manifest(&mut self, f: impl FnOnce(&mut OrganizationManifest)) {
        let (_, manifest) = organization::mutate(&mut self.ledgers, |draft| {
            f(draft);
            Ok(())
        })
        .expect("structural mutation");
        self.manifest = manifest;
    }
}

fn everyone() -> LaunchFence {
    LaunchFence::fenced(["quant-head", "signal-researcher", "it-head"].map(String::from))
}

// --- structural transitions ------------------------------------------------

#[test]
fn a_paused_department_derives_a_forced_park_for_everyone_under_it() {
    let mut world = World::new();
    // Get the department running first, so `lastOperational` is true and the
    // pause is a change rather than the initial state.
    world.reconcile(everyone(), &["signal-researcher", "quant-head"]);
    assert!(world.ledger().people["signal-researcher"].last_operational);

    world.mutate_manifest(|draft| {
        draft.departments.get_mut("quant").expect("quant").state = UnitState::Paused;
    });
    world.reconcile(everyone(), &[]);

    let ledger = world.ledger();
    let transition = ledger
        .active_transition("signal-researcher")
        .expect("a paused unit owes its people a handoff");
    assert_eq!(transition.action, TransitionAction::Park);
    assert!(transition.status.is_pending());
    assert!(
        transition.reason.contains("Release the transition before park"),
        "{}",
        transition.reason
    );
    // The head of the paused unit is in the same position…
    assert!(ledger.active_transition("quant-head").is_some());
    // …and the untouched department is untouched.
    assert!(ledger.active_transition("it-head").is_none());
}

#[test]
fn benching_derives_a_park_and_departure_derives_an_offboard() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.mutate_manifest(|draft| {
        draft.people.get_mut("signal-researcher").expect("worker").employment_state =
            EmploymentState::Benched;
    });
    world.reconcile(everyone(), &[]);
    assert_eq!(
        world.ledger().active_transition("signal-researcher").map(|t| t.action),
        Some(TransitionAction::Park),
        "benching is a park, not an offboard"
    );

    // Departure outranks benching, and it is derived from the *persisted*
    // active state, so start from a fresh world.
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.mutate_manifest(|draft| {
        draft.people.get_mut("signal-researcher").expect("worker").employment_state =
            EmploymentState::Departed;
    });
    world.reconcile(everyone(), &[]);
    assert_eq!(
        world.ledger().active_transition("signal-researcher").map(|t| t.action),
        Some(TransitionAction::Offboard)
    );
}

/// The bug this rule exists for: a structural change waiting on a release from
/// somebody the fence excludes waits forever, because a release only ever
/// arrives from that person's own live pane.
#[test]
fn a_structural_handoff_is_abandoned_when_the_fence_excludes_its_person() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.mutate_manifest(|draft| {
        draft.departments.get_mut("quant").expect("quant").state = UnitState::Paused;
    });

    // Nobody but the CEO may run, so the handoff is unreachable.
    world.reconcile(LaunchFence::deny_all(), &[]);

    let ledger = world.ledger();
    assert!(
        ledger.active_transition("signal-researcher").is_none(),
        "the unreachable handoff must not stay open"
    );
    // Nothing was even manufactured: reconciliation must never create work that
    // only un-fencing can finish.
    assert!(
        ledger.transition_order.is_empty(),
        "an unreachable handoff is abandoned rather than opened: {:?}",
        ledger.transition_order
    );
    assert!(
        !ledger.people["signal-researcher"].last_desired_active,
        "the structural change applied unattended"
    );
    assert!(
        !ledger.people["signal-researcher"].last_operational,
        "the persisted placement advanced — leaving it stale would re-derive the same \
         structural transition on every future pass"
    );

    // And it converges. This is the regression the TypeScript comment records:
    // restricting abandonment to forced removals left an operator-only transfer
    // landing its mutation and then manufacturing a fresh `awaiting_handoff` on
    // every subsequent pass, forever.
    world.reconcile(LaunchFence::deny_all(), &[]);
    world.reconcile(LaunchFence::deny_all(), &[]);
    assert!(
        world.ledger().transition_order.is_empty(),
        "a converged structural change must not re-derive on every pass"
    );
}

/// #608: normalized `org_transfer` has already committed by the time activity
/// observes the manifest. A worker who was already desired, physically present
/// (the production caller supplies that observation), still admitted, and has
/// no lifecycle handoff must move in-place rather than be held against the old
/// unit by a synthetic graceful transition.
#[test]
fn a_direct_transfer_retains_the_running_worker_and_advances_durable_placement_without_a_handoff() {
    let mut world = World::new();
    let admitted = LaunchFence::fenced(["signal-researcher".to_owned()]);
    let before = world.reconcile(admitted.clone(), &["signal-researcher"]);
    assert!(before.people["signal-researcher"].active, "precondition: admitted and running");
    assert_eq!(world.ledger().people["signal-researcher"].last_department_id, "quant");

    move_signal_researcher_to_it(&mut world);
    let moved = world.reconcile(admitted, &[]);

    assert!(moved.people["signal-researcher"].active, "the existing pane is retained");
    assert!(
        moved.people["signal-researcher"].transition_id.is_none(),
        "an atomic running transfer does not manufacture a graceful transition"
    );
    let state = &world.ledger().people["signal-researcher"];
    assert_eq!(state.last_department_id, "it");
    assert!(state.last_desired_active);
}

/// The continuity exception is retention, never admission. The same committed
/// move for a launch-admitted but stopped person advances durable placement
/// unattended and creates neither a transition nor a desired pane.
#[test]
fn a_direct_transfer_does_not_start_a_stopped_admitted_worker() {
    let mut world = World::new();
    move_signal_researcher_to_it(&mut world);
    let admitted = LaunchFence::fenced(["signal-researcher".to_owned()]);

    let moved = world.reconcile(admitted, &[]);

    assert!(!moved.people["signal-researcher"].active);
    assert!(moved.people["signal-researcher"].transition_id.is_none());
    let ledger = world.ledger();
    let state = &ledger.people["signal-researcher"];
    assert!(!state.last_desired_active);
    assert_eq!(state.last_department_id, "it");
    assert!(ledger.transition_order.is_empty(), "a stopped transfer creates no handoff row");
}

/// The same fenced-out person, but with a handoff already open: that record is
/// cancelled and stamped `abandonedAt`, never `applied`.
#[test]
fn an_open_handoff_for_a_fenced_out_person_is_cancelled_and_stamped_abandoned() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.mutate_manifest(|draft| {
        draft.departments.get_mut("quant").expect("quant").state = UnitState::Paused;
    });
    // One pass while the person may still run: the handoff is opened.
    world.reconcile(everyone(), &[]);
    let opened = world
        .ledger()
        .active_transition("signal-researcher")
        .expect("a pending handoff")
        .id
        .clone();

    // Now the fence closes: the handoff it was waiting for became unreachable.
    world.reconcile(LaunchFence::deny_all(), &[]);
    let ledger = world.ledger();
    let abandoned = &ledger.transitions[&opened];
    assert_eq!(
        abandoned.status,
        TransitionStatus::Cancelled,
        "abandoned is cancelled, never applied: applied asserts the owner released it"
    );
    assert!(abandoned.abandoned_at.is_some(), "and the ledger says why");
    assert!(
        abandoned.applied_at.is_none(),
        "nothing may claim the structural change was released here"
    );
    assert!(ledger.active_transition("signal-researcher").is_none());
}

#[test]
fn a_ready_structural_handoff_applies_and_advances_the_persisted_placement() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.mutate_manifest(|draft| {
        draft.departments.get_mut("quant").expect("quant").state = UnitState::Paused;
    });
    world.reconcile(everyone(), &[]);

    let transition_id = world
        .ledger()
        .active_transition("signal-researcher")
        .expect("a pending handoff")
        .id
        .clone();
    world.release_on(&transition_id, "signal-researcher");
    world.reconcile(everyone(), &[]);

    let ledger = world.ledger();
    assert_eq!(ledger.transitions[&transition_id].status, TransitionStatus::Applied);
    assert!(
        !ledger.people["signal-researcher"].last_operational,
        "the persisted placement caught up with the paused unit"
    );
    assert!(
        !ledger.people["signal-researcher"].last_desired_active,
        "a removal handoff that applied takes the pane down"
    );
}

// --- routine idle parking ---------------------------------------------------

// REPLACES `coming_up_restarts_the_quiet_lease_so_a_slow_start_is_never_parked_on_arrival`.
//
// That test pinned the ARRIVAL EDGE: a host observation proving a person had
// just come up cancelled the routine idle park decided while they did not
// exist. The edge existed only because the clock started at the wrong moment --
// it was stamped when chiefd DECIDED a person should run, so a person queued
// behind the writer thread burned the whole lease before their process existed
// ("they all started fine ... as soon as they started they immediately all shut
// down"). The rescue is deleted because the defect it rescued is deleted: the
// countdown now starts when the AGENT reports it went quiet and at no other
// moment, so there is no early clock to cancel. The three tests below assert
// the replacement ruling directly.

/// The settle window is TWO MINUTES, pinned as a number.
///
/// Every other test in this file advances by multiples of the constant, so all
/// of them stay green if somebody edits it -- the relationship is asserted and
/// the VALUE never is. That is exactly the shape that lets a correct number be
/// "fixed" to chase an observed latency it does not own: the park decision is
/// sampled by a reactive duty whose fallback floor is 60s, so an operator sees
/// up to SIX minutes and the constant is not what to change. This test is the
/// one place the operator's stated cap is written down as a value.
///
/// 2026-08-24: *"lets bump the 2mins to a 5mins."* The name moved with the
/// number, because a test called `..._two_minutes` asserting 300_000 is a lie
/// the next reader has to decode.
#[test]
fn the_settle_window_is_the_operators_five_minutes() {
    assert_eq!(
        ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS, 300_000,
        "the operator's cap is five minutes from settle; the reconcile sampling floor is a \
         separate number and is not fixed by moving this one"
    );
}

/// State one of three: NEVER BEATEN -> NO CLOCK.
///
/// Idle means "was working and stopped". A person whose process has never said
/// anything never started, so nothing is timed -- however long they sit there.
/// This is the exact case the arrival edge was invented to undo.
#[test]
fn a_person_whose_agent_has_never_reported_has_no_settle_clock_at_all() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    assert!(world.ledger().people["signal-researcher"].last_desired_active);

    // Far longer than a full lease, with no beat and no settle report.
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS * 3);
    world.reconcile(everyone(), &[]);

    assert!(
        world.idle_since("signal-researcher").is_none(),
        "a person who never reported was never working, so was never idle"
    );
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "and is never parked for a silence that was never measured"
    );
}

/// State two of three: BEATING -> NO CLOCK. State three: WENT QUIET -> the
/// lease runs from that instant, and from that instant exactly.
#[test]
fn the_settle_clock_starts_when_the_agent_says_it_went_quiet_and_not_before() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.note_activity("signal-researcher", true);
    world.reconcile(everyone(), &[]);
    assert!(world.idle_since("signal-researcher").is_none(), "a beating agent has no clock");

    // Still beating, well past a lease. A working agent is never settled.
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS * 2);
    world.note_activity("signal-researcher", true);
    world.reconcile(everyone(), &[]);
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "an agent that keeps working is never parked, however long it works"
    );

    // It says it finished. THE CLOCK STARTS HERE.
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    assert!(world.idle_since("signal-researcher").is_some(), "the quiet report starts the clock");
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "but the lease has not expired yet, so nothing is parked on the quiet pass itself"
    );

    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    world.reconcile(everyone(), &[]);
    assert!(
        world.ledger().active_transition("signal-researcher").is_some(),
        "a full lease after going quiet, the person parks"
    );
}

/// THE OTHER HALF OF THE RESTART BUG: chiefd down longer than the liveness
/// bound must not settle a company that was busy.
///
/// The agent was MID-TURN when chiefd stopped -- a beat, and then nothing,
/// because there was nobody to beat to. Unclamped, the inferred quiet instant
/// is `agent_active_at + AGENT_ACTIVITY_LIVENESS_MS`, which is already in the
/// past by the time chiefd is back, so `idle_since` is immediately older than
/// the whole quiet lease and the person is a park candidate on the FIRST pass
/// after the restart, with no grace at all.
///
/// The rising-edge clear does not save this: nobody crosses
/// desired-inactive-to-active when chiefd merely restarts.
#[test]
fn a_chiefd_restart_longer_than_the_liveness_bound_settles_nobody_with_no_grace() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);

    // Mid-turn, and then chiefd goes away for far longer than the bound.
    world.note_activity("signal-researcher", true);
    world.reconcile(everyone(), &[]);
    world.advance(AGENT_ACTIVITY_LIVENESS_MS * 4);
    world.chiefd_restarted_now();

    // THE FIRST PASS BACK. The silence spans an interval nobody was listening
    // over, so it is not evidence.
    world.reconcile(everyone(), &[]);
    assert!(
        world.idle_since("signal-researcher").is_none(),
        "a heartbeat cannot be missing while nobody is listening for it"
    );
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "a chiefd restart must not park a person who was mid-turn when it stopped"
    );

    // And the clamp is a GRACE, not an exemption: silence measured from the
    // restart still settles them on the ordinary schedule.
    world.advance(AGENT_ACTIVITY_LIVENESS_MS + ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    world.reconcile(everyone(), &[]);
    assert!(
        world.ledger().active_transition("signal-researcher").is_some(),
        "a full liveness window plus a full lease AFTER the restart still parks them"
    );
}

/// An agent's own SETTLE REPORT survives a restart untouched.
///
/// The clamp covers an absence chiefd inferred, and only that. "I have
/// finished" is a fact the agent SENT; chiefd not watching afterwards does not
/// un-say it, and clamping it would hand a full extra lease to every person who
/// had already declared themselves done.
#[test]
fn an_explicit_quiet_report_is_not_clamped_by_a_restart() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    let reported = world.idle_since("signal-researcher").expect("the report starts the clock");

    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS * 3);
    world.chiefd_restarted_now();
    world.reconcile(everyone(), &[]);

    assert_eq!(
        world.idle_since("signal-researcher").as_deref(),
        Some(reported.as_str()),
        "the instant the agent reported is unchanged by a restart"
    );
    assert!(
        world.ledger().active_transition("signal-researcher").is_some(),
        "an agent that said it had finished still settles"
    );
}

/// THE RESTART BUG, as a regression test.
///
/// `idle_since` used to be STAMPED and carried. After a chiefd restart with
/// panes still up, no arrival edge fired and the persisted stamp could already
/// exceed the 120s lease -- so the person was stopped with NO grace whatever.
/// It is now DERIVED from the agent's own reports on every pass, so a value
/// written before the restart cannot survive to be acted on.
#[test]
fn a_stale_persisted_idle_stamp_cannot_park_a_person_who_is_working_after_a_restart() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);

    // Simulate the pre-restart stamp: quiet long ago, clock fully expired.
    world.note_activity("signal-researcher", false);
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS * 5);

    // The pane survived the restart and its agent is demonstrably working.
    world.note_activity("signal-researcher", true);
    world.reconcile(everyone(), &[]);

    assert!(
        world.idle_since("signal-researcher").is_none(),
        "the expired stamp is recomputed away by the beat, not carried into the decision"
    );
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "a working agent is not parked with zero grace because of a clock from before the restart"
    );
}

/// A pane that DIED mid-turn stops beating without ever saying it settled. The
/// missing heartbeat is converted into a quiet instant by
/// `AGENT_ACTIVITY_LIVENESS_MS`, which is the whole reason that constant
/// survives the deletion of the host observation -- it is the only thing
/// between a dead pane and immortality. Worst case is unchanged and exact:
/// liveness + lease, never never.
#[test]
fn a_pane_that_stops_beating_without_settling_still_parks_after_liveness_plus_the_lease() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.note_activity("signal-researcher", true);
    world.reconcile(everyone(), &[]);

    // Just inside liveness: the missing beat is not yet conclusive.
    world.advance(AGENT_ACTIVITY_LIVENESS_MS - 1);
    world.reconcile(everyone(), &[]);
    assert!(
        world.idle_since("signal-researcher").is_none(),
        "a beat that is merely late has not yet become silence"
    );

    // Past liveness: the silence is conclusive, and the clock starts THERE --
    // not at the last beat, which would bill the agent for a silence chiefd had
    // not yet decided was silence.
    world.advance(2);
    world.reconcile(everyone(), &[]);
    assert!(world.idle_since("signal-researcher").is_some(), "the inferred quiet instant lands");
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "and the lease starts from there rather than being already spent"
    );

    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    world.reconcile(everyone(), &[]);
    assert!(
        world.ledger().active_transition("signal-researcher").is_some(),
        "a dead pane settles in liveness + lease, rather than never"
    );
}

#[test]
fn a_routine_idle_park_waits_for_the_settled_lease_and_is_capped_in_flight() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher", "quant-head", "it-head"]);
    for person in ["signal-researcher", "quant-head", "it-head"] {
        assert!(world.ledger().people[person].last_desired_active, "{person} should be up");
    }

    // All three agents report they went quiet. The countdown starts there and
    // nowhere else; demand clearing on its own no longer starts a clock.
    for person in ["signal-researcher", "quant-head", "it-head"] {
        world.note_activity(person, false);
    }

    // Demand clears. The first pass only starts the sixty-second quiet lease.
    world.reconcile(everyone(), &[]);
    assert!(world.ledger().people["signal-researcher"].idle_since.is_some());
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "the lease has not expired, so no park is admitted yet"
    );

    // Past the lease, parks are admitted — but at most two at a time.
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    world.reconcile(everyone(), &[]);
    let ledger = world.ledger();
    let parked: Vec<&str> = ["quant-head", "signal-researcher", "it-head"]
        .into_iter()
        .filter(|person| {
            ledger.active_transition(person).is_some_and(|t| t.reason == IDLE_AUTO_PARK_REASON)
        })
        .collect();
    assert_eq!(
        parked.len(),
        ORGANIZATION_AUTOMATIC_PARK_MAX_IN_FLIGHT,
        "routine parking is bounded per cycle; got {parked:?}"
    );
    let held_back = ["quant-head", "signal-researcher", "it-head"]
        .into_iter()
        .find(|person| !parked.contains(person))
        .expect("one person is held back");
    assert!(
        ledger.people[held_back].last_desired_active,
        "a held-back candidate stays up as a durable candidate; it is not silently dropped"
    );
}

#[test]
fn an_absent_person_is_not_recreated_for_a_bounded_idle_lease() {
    let mut world = World::new();
    let caller_fence = LaunchFence::fenced(["signal-researcher".to_owned()]);

    world.reconcile(caller_fence, &["signal-researcher"]);
    assert!(world.ledger().people["signal-researcher"].last_desired_active);

    let supervision = world.supervision();
    let manifest = world.manifest.clone();
    let quiet = super::reconcile(
        &mut world.ledgers,
        &manifest,
        &supervision,
        &ReconcileInput {
            launch_intent: LaunchFence::deny_all(),
            requested_person_ids: Vec::new(),
            watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
        },
    )
    .expect("reconcile an observed-absent person");

    assert!(!quiet.people["signal-researcher"].active);
    assert!(!world.ledger().people["signal-researcher"].last_desired_active);
}

// REPLACES `an_already_active_person_finishes_the_bounded_idle_lease_after_its_launch_intent_withdraws`.
//
// That test pinned `bounded_idle_retention`: a person whose launch intent had
// been withdrawn stayed ACTIVE anyway, to finish a routine idle handoff, and
// only stopped once the quiet lease expired. The whole mechanism is deleted.
//
// THE RULING: there is no "let them finish". chiefd declares the final state,
// the actuator makes it true, and the agent RESUMES from its transcript exactly
// as if it had crashed. The wait was never a real wait in any case -- a ROUTINE
// idle park has no releaser, so it could only ever expire -- and its guard was
// a host observation, which is the thing being removed. A STRUCTURAL handoff
// (bench, transfer, offboard) does have a real releaser and keeps
// `HANDOFF_GRACE_MS` untouched; this test is about the routine case only.
#[test]
fn a_withdrawn_launch_intent_stops_the_person_at_once_with_no_handoff_wait() {
    let mut world = World::new();
    let caller_fence = LaunchFence::fenced(["signal-researcher".to_owned()]);

    // The state immediately after an authenticated public lifecycle tool
    // projected its caller: genuinely running, but the one-turn admission is
    // not durable launch intent.
    world.reconcile(caller_fence, &["signal-researcher"]);
    assert!(world.ledger().people["signal-researcher"].last_desired_active);

    // The intent withdraws. The person is desired-inactive on THAT pass -- not
    // one lease later, and not conditionally on whether a pane happened to be
    // observed.
    let quiet = world.reconcile(LaunchFence::deny_all(), &[]);
    assert!(
        !quiet.people["signal-researcher"].active,
        "a withdrawn intent stops the person on the same pass"
    );
    assert!(!world.ledger().people["signal-researcher"].last_desired_active);
    assert!(
        quiet.people["signal-researcher"].transition_id.is_none(),
        "and no graceful transition is manufactured to wait inside"
    );

    // The fence still refuses to bring them back. Deleting the retention did
    // not weaken the launch-intent fence, which is a different mechanism and
    // stays: nothing may raise a non-CEO person without an explicit intent.
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    let later = world.reconcile(LaunchFence::deny_all(), &[]);
    assert!(!later.people["signal-researcher"].active);
}

#[test]
fn a_routine_idle_park_is_born_forced_terminal_and_never_retried() {
    // #337's OUTCOME, reached at admission instead of after two waits. A
    // routine idle park is FORCED terminal — never cancelled-and-retried, which
    // is what left a hung agent recycling forever in violation of THE HARD
    // RULE's "idle trends to zero", and no longer held in a grace window first,
    // because nothing can arrive in one: `release` has a single production
    // caller (the staffing-lifecycle verb, which releases inside the same
    // request) and the Pi extension surface has no release verb at all. The two
    // deleted windows were a wait for a message no code path can send.
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    // The agent reports it went quiet. THE COUNTDOWN STARTS HERE and
    // nowhere else -- chiefd no longer starts a clock on its own bookkeeping.
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);

    // The pass that admits the park is the pass that ends the pane.
    let forcing = world.reconcile(everyone(), &[]);
    assert!(
        !forcing.people["signal-researcher"].active,
        "a force-parked person settles STOPPED (idle trends to zero)"
    );
    let ledger = world.ledger();
    let parked = ledger
        .active_transition("signal-researcher")
        .filter(|t| t.reason == IDLE_AUTO_PARK_REASON)
        .map(|t| t.id.clone())
        .expect("a routine park was admitted");
    let forced = &ledger.transitions[&parked];
    assert!(
        !forced.status.is_pending(),
        "a park born terminal is not pending, so it never adds the HandoffRequired reason \
         that used to keep the pane alive through the deleted window"
    );
    assert_eq!(
        forced.status,
        TransitionStatus::Forced,
        "the fully-overdue routine park is forced terminal (never retried)"
    );
    assert!(forced.forced_at.is_some(), "a forced park stamps forced_at");
    assert!(
        forced.abandoned_at.is_none(),
        "a forced park is not an abandonment: nobody was fenced out"
    );
    // The pointer is KEPT (as with an applied park), so the person cannot
    // re-enter automatic-park candidacy on their own.
    assert_eq!(
        ledger.people["signal-researcher"].active_transition_id.as_deref(),
        Some(parked.as_str()),
        "a forced park keeps the active-transition pointer (no self-restart)"
    );

    // NO retry state: a later reconcile does NOT resurrect the person or mint a
    // fresh park. The forced transition is terminal history and STAYS the active
    // pointer; nothing recycles it back into candidacy.
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS * 10);
    let later = world.reconcile(everyone(), &[]);
    assert!(
        !later.people["signal-researcher"].active,
        "a force-parked person stays STOPPED across later reconciles (no retry)"
    );
    let after = world.ledger();
    assert_eq!(
        after.people["signal-researcher"].active_transition_id.as_deref(),
        Some(parked.as_str()),
        "the forced park is never superseded by a fresh retry transition"
    );
    assert_eq!(
        after.transitions[&parked].status,
        TransitionStatus::Forced,
        "the forced status is durable — never rewritten to a retry cycle"
    );
}

/// PARKED MEANS NO PANE AND NO COMPUTE, NOT A DELETED PERSON.
///
/// The durability half of the idle park (`docs/testing/TEST_SUITE.md`, Case 15)
/// rested on structure and on nothing asserting it: no test asked what a park
/// leaves behind. It leaves EVERYTHING behind. The roster is where a client
/// reads a person's identity, title, department, headship and employment, and
/// after an idle park every one of those is byte-identical to what it was while
/// they were running. `desired_active` is the ONE field a park may change, and
/// this compares the whole projected row so a future change cannot quietly take
/// a second one with it.
#[test]
fn a_parked_person_keeps_everything_except_the_decision_to_run_them() {
    use crate::runtime::roster::project_desired_roster;

    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    let running = project_desired_roster(&world.manifest, Some(&world.ledger()));
    let before = running
        .people
        .iter()
        .find(|person| person.id == "signal-researcher")
        .expect("the roster carries a running person")
        .clone();
    assert!(before.desired_active, "the person is up before the park");

    // The ordinary idle park: the agent reports it went quiet, the settled
    // lease expires, and the pass that admits the park ends the pane.
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    let parked = world.reconcile(everyone(), &[]);
    assert!(!parked.people["signal-researcher"].active, "the park was decided");
    assert!(
        world
            .ledger()
            .active_transition("signal-researcher")
            .is_some_and(|transition| transition.reason == IDLE_AUTO_PARK_REASON),
        "this is the routine idle park and not some other removal"
    );

    let after = project_desired_roster(&world.manifest, Some(&world.ledger()));
    let kept = after
        .people
        .iter()
        .find(|person| person.id == "signal-researcher")
        .expect("a parked person is still on the roster — Case 15's own words");

    assert!(!kept.desired_active, "the one field a park changes");
    assert_eq!(
        *kept,
        crate::runtime::roster::RosterPerson { desired_active: false, ..before },
        "a park changes whether they RUN and nothing else about who they are"
    );
}

#[test]
fn item_d_read_tolerates_the_allowlisted_legacy_key_but_fails_any_other_unknown() {
    // Item-D read-tolerance (Fable, binding), the READ half of the both-halves
    // fixture. The PUBLISH half (422 on an unmodeled key) lives in
    // `rows::tests::item_d_rejects_unmodeled_keys`. Read tolerates ONLY the
    // bounded allowlist (`automaticParkRetryAfter`, dropped by #337) and FAILS
    // loudly on any other unknown key — never blanket absorption.
    let manifest = northstar_manifest(EPOCH);
    let seed = ActivityLedger::initial(&manifest, &iso_millis(EPOCH));
    let base = serde_json::to_value(&seed).expect("serialize");

    // (1) READ TOLERATES the allowlisted legacy key: a legacy blob carrying
    // people.signal-researcher.automaticParkRetryAfter parses, with the field
    // dropped (it is not in the row model).
    let mut legacy = base.clone();
    legacy["people"]["signal-researcher"].as_object_mut().unwrap().insert(
        "automaticParkRetryAfter".to_string(),
        serde_json::json!("2026-07-20T19:43:24.809Z"),
    );
    let parsed = parse_ledger_tolerating_legacy(&legacy.to_string())
        .expect("a legacy blob with the allowlisted key reads successfully");
    assert!(
        parsed.people.contains_key("signal-researcher"),
        "the rest of the ledger round-trips; only the allowlisted key is dropped"
    );

    // (2) READ FAILS on any other unknown key — nested...
    let mut nested_typo = base.clone();
    nested_typo["people"]["signal-researcher"]
        .as_object_mut()
        .unwrap()
        .insert("automaticParkRetryAfterTypo".to_string(), serde_json::json!("x"));
    assert!(
        parse_ledger_tolerating_legacy(&nested_typo.to_string()).is_none(),
        "an unknown key OUTSIDE the allowlist FAILS the read (a typo cannot slip through)"
    );

    // ...and top-level.
    let mut top = base.clone();
    top.as_object_mut().unwrap().insert("mysteryField".to_string(), serde_json::json!(1));
    assert!(
        parse_ledger_tolerating_legacy(&top.to_string()).is_none(),
        "an unknown top-level key FAILS the read"
    );
}

/// THE CEO NEVER SLEEPS, AND EVERYBODY ELSE STILL DOES.
///
/// Operator ruling, 2026-08-14, given on a live box where the root had settled
/// and its pane was gone while its staff kept running: "CEO can never go to
/// sleep." It supersedes the earlier "everybody, dead" for the ROOT ONLY.
///
/// This replaces `the_ceo_settles_and_parks_like_everybody_else`, which pinned
/// the opposite and was live for one evening. The reason the reversal is right
/// rather than a preference: the root is the operator's door into the company.
/// A parked CEO is not a resting company, it is an unreachable one — nobody can
/// be talked to, including to ask for anybody to be woken.
#[test]
fn the_ceo_holds_a_permanent_lease_and_never_parks() {
    let mut world = World::new();
    let snapshot = world.reconcile(everyone(), &[]);
    assert!(
        snapshot.people["chief"].active,
        "the root runs with nobody asking — that IS the lease, and it is what \
         makes a company reachable at all"
    );

    // The root's agent reports it went quiet. Everybody else starts a clock
    // here; the CEO does not, because an idle column on a permanently-leased
    // person would read "idle for six days" on every surface that shows it.
    world.note_activity("chief", false);
    world.reconcile(everyone(), &[]);
    assert!(world.idle_since("chief").is_none(), "the root accrues no idle clock");

    // And well past the lease everybody else parks on, it is still up.
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS * 10);
    let snapshot = world.reconcile(everyone(), &[]);
    assert!(
        world.ledger().active_transition("chief").is_none(),
        "ten leases later the root has still not been parked"
    );
    assert!(snapshot.people["chief"].active, "and is still running");
}

/// The half that must NOT be lost with the reversal: everybody else still
/// settles on the ordinary two-minute lease. The operator's "everybody, dead"
/// ruling stands for the staff, and the exemption is the root ALONE.
#[test]
fn a_worker_still_settles_on_the_ordinary_lease_while_the_root_does_not() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    assert!(world.idle_since("signal-researcher").is_some(), "a worker accrues a clock");

    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    world.reconcile(everyone(), &[]);
    assert!(world.ledger().active_transition("signal-researcher").is_some(), "and parks on it");
    assert!(
        world.ledger().active_transition("chief").is_none(),
        "in the very same pass that left the root alone"
    );
}

#[test]
fn arriving_work_cancels_a_routine_park_but_never_an_intent_bound_one() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    // The agent reports it went quiet. THE COUNTDOWN STARTS HERE and
    // nowhere else -- chiefd no longer starts a clock on its own bookkeeping.
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    world.reconcile(everyone(), &[]);
    assert!(world.ledger().active_transition("signal-researcher").is_some());

    // Work arrives: the routine park yields.
    world.reconcile(everyone(), &["signal-researcher"]);
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "ordinary idle parking yields to newly arrived work"
    );

    // An explicit lifecycle intent does NOT yield: cancelling it made a
    // department with a goal or a loop impossible to remove, because every
    // supervisor pass manufactured another attempt.
    world.begin("signal-researcher", TransitionAction::Park, Some("intent-remove-quant"));
    world.reconcile(everyone(), &["signal-researcher"]);
    let ledger = world.ledger();
    let held = ledger
        .active_transition("signal-researcher")
        .expect("an intent-bound handoff survives arriving work");
    assert_eq!(held.intent_id.as_deref(), Some("intent-remove-quant"));
}

#[test]
fn an_explicit_intent_supersedes_an_unowned_routine_park() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    // The agent reports it went quiet. THE COUNTDOWN STARTS HERE and
    // nowhere else -- chiefd no longer starts a clock on its own bookkeeping.
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    world.reconcile(everyone(), &[]);
    let routine = world
        .ledger()
        .active_transition("signal-researcher")
        .filter(|t| t.reason == IDLE_AUTO_PARK_REASON)
        .map(|t| t.id.clone())
        .expect("a routine park");

    // Only park may supersede park, and only a previously unowned one.
    let owned = world.begin("signal-researcher", TransitionAction::Park, Some("intent-1"));
    assert_ne!(owned, routine, "the intent-bound request gets its own transition");
    let ledger = world.ledger();
    assert_eq!(ledger.transitions[&routine].status, TransitionStatus::Cancelled);
    assert_eq!(ledger.active_transition("signal-researcher").map(|t| t.id.clone()), Some(owned));
}

#[test]
fn a_conflicting_transition_for_the_same_person_is_refused() {
    let mut world = World::new();
    world.begin("signal-researcher", TransitionAction::Park, None);
    let manifest = world.manifest.clone();
    let supervision = world.supervision();
    let err = begin_transition(
        &mut world.ledgers,
        &manifest,
        &supervision,
        &BeginTransitionInput {
            person_id: "signal-researcher".to_string(),
            action: TransitionAction::Offboard,
            reason: "Different action.".to_string(),
            to_department_id: None,
            intent_id: None,
        },
    )
    .expect_err("one person, one active transition");
    assert_eq!(err.code(), Some(TRANSITION_CONFLICT));
}

/// atomic-reorg: an explicit intent-bound request supersedes ANY conflicting
/// non-applied transition (not just an unowned routine park). This is the live
/// cobalt wedge: a failed unit-stop stranded a superseded intent-bound park,
/// and every later stop/transfer was refused
/// "for another lifecycle intent" forever.
#[test]
fn an_explicit_intent_supersedes_a_stale_intent_bound_transition() {
    let mut world = World::new();
    let stale =
        world.begin("signal-researcher", TransitionAction::Park, Some("unit-stop:quant:old"));
    let fresh =
        world.begin("signal-researcher", TransitionAction::Park, Some("unit-stop:quant:new"));
    assert_ne!(fresh, stale, "the new intent gets its own transition");
    let ledger = world.ledger();
    assert_eq!(ledger.transitions[&stale].status, TransitionStatus::Cancelled);
    assert!(ledger.transitions[&stale].cancelled_at.is_some());
    assert_eq!(ledger.active_transition("signal-researcher").map(|t| t.id.clone()), Some(fresh));
}

/// atomic-reorg: a different-ACTION explicit intent also supersedes a stale
/// non-applied transition — a stranded stop-park must not block a transfer.
#[test]
fn a_different_action_explicit_intent_supersedes_a_stale_stop_park() {
    let mut world = World::new();
    let stale =
        world.begin("signal-researcher", TransitionAction::Park, Some("unit-stop:quant:old"));
    let offboard = world.begin(
        "signal-researcher",
        TransitionAction::Offboard,
        Some("offboard:signal-researcher:current"),
    );
    let ledger = world.ledger();
    assert_eq!(ledger.transitions[&stale].status, TransitionStatus::Cancelled);
    assert_eq!(ledger.active_transition("signal-researcher").map(|t| t.id.clone()), Some(offboard));
}

/// atomic-reorg: the expiry sweep. A non-terminal intent-bound transition past
/// its handoff deadline plus one extra grace window is cancelled by the next
/// reconcile pass, clearing the person's pointer, while a routine unowned idle
/// auto-park is left for #337's forced machinery.
#[test]
fn the_reconcile_pass_expires_a_stale_in_flight_transition() {
    let mut world = World::new();
    let stale =
        world.begin("signal-researcher", TransitionAction::Park, Some("unit-stop:quant:old"));
    world.advance(2 * HANDOFF_GRACE_MS + 1);
    world.reconcile(everyone(), &[]);
    let ledger = world.ledger();
    assert_eq!(
        ledger.transitions[&stale].status,
        TransitionStatus::Cancelled,
        "a stale in-flight transition must not outlive a reconcile pass"
    );
    assert!(
        ledger.active_transition("signal-researcher").map(|t| t.id.clone()) != Some(stale),
        "the expired transition no longer occupies the person's active pointer"
    );
}

// --- flag a-7: read/validate polarity ---------------------------------------

/// A structural metadata update does not invalidate an otherwise valid
/// activity ledger. The former whole-company fence made this ordinary
/// read fail until an unrelated mutation repaired it.
#[test]
fn activity_read_has_no_whole_company_fence() {
    let mut world = World::new();
    let before = world.ledger();
    world.mutate_manifest(|draft| draft.purpose = "changed".to_string());
    let read_back = read(&world.ledgers, &world.manifest)
        .expect("structural metadata must not stale the activity ledger");
    assert_eq!(read_back, before);
}

/// The tolerance is SCOPED to a benign structural update. Any OTHER validation
/// failure (here: a person in the ledger who is not in the manifest) must still
/// be fatal `Corrupt` on read — tolerating a stale counter must never become
/// tolerating real structural corruption.
#[test]
fn read_still_fails_closed_on_non_counter_validation_failure() {
    let mut world = World::new();
    // Inject a ledger person that does not exist in the manifest — a genuine
    // structural corruption, unrelated to a benign structural update.
    let mut ledger = world.ledger();
    let ghost = PersonActivityState {
        person_id: "ghost-person".to_string(),
        ..ledger.people.values().next().expect("a seeded person").clone()
    };
    ledger.person_order.push("ghost-person".to_string());
    ledger.people.insert("ghost-person".to_string(), ghost);
    put(&mut world.ledgers, &ledger).expect("a ghost person is a writable byte shape");

    // A ghost person is a body that DECODED and then failed `validate`. It is
    // a `StoreFailure`, not a `Corrupt`: the bytes are exactly what chiefd
    // wrote, so telling an operator they are damaged sends them to inspect a
    // file that is intact.
    let err = read(&world.ledgers, &world.manifest)
        .expect_err("a structural invariant violation must still fail closed");
    assert_eq!(err.kind(), "StoreFailure");
}

/// **The caller must learn WHY, from the error it was handed.**
///
/// The sibling test below asserts the reason reaches stderr. That was never
/// enough: stderr is not the error, no route reads it, and the contract
/// harness deletes it unread — so after the reason was recovered at the
/// mapping point, an operator hitting this STILL saw a bare store name. This
/// asserts the reason travels inside the VALUE, along the real store path,
/// naming the invariant that actually broke.
///
/// Deliberately NOT `assert!(err.cause().is_some())`: a cause that arrives as
/// an empty string satisfies that and is the defect. The assertions below are
/// on the offending person's id and the refusal code, neither of which any
/// placeholder can produce.
#[test]
fn a_validation_failure_hands_the_caller_the_invariant_that_broke() {
    let mut world = World::new();
    let mut ledger = world.ledger();
    let ghost = PersonActivityState {
        person_id: "ghost-person".to_string(),
        ..ledger.people.values().next().expect("a seeded person").clone()
    };
    ledger.person_order.push("ghost-person".to_string());
    ledger.people.insert("ghost-person".to_string(), ghost);
    put(&mut world.ledgers, &ledger).expect("a ghost person is a writable byte shape");

    let err = read(&world.ledgers, &world.manifest).expect_err("the ghost person fails validate");
    let cause = err.cause().expect("a store failure carries the reason it failed");
    assert!(
        cause.contains("ghost-person"),
        "the cause must name the offending person, not just the store: {cause}"
    );
    assert!(
        cause.contains("unknown-person"),
        "the cause must carry the refusal code that names the invariant: {cause}"
    );
    // …and it must reach the rendered error too, because that is what a caller
    // that only logs `{err}` will print.
    assert!(err.to_string().contains("ghost-person"), "{err}");
}

/// **A broken invariant inside a MUTATION is not damaged bytes either.**
///
/// `read` has answered `StoreFailure` for a body that decoded and then failed
/// `validate` since the taxonomy split. `mutate` was the LAST site in the tree
/// still answering `Corrupt` for the identical event, and it survived by a
/// merge accident rather than a decision: the reclassification pass
/// (`18154c529`) rewrote that check where it then lived, inside
/// `read_for_mutation`, and the parallel repair branch (`830e9d7d1`) moved it
/// out to `mutate` in the same batch, carrying the pre-split label with it.
/// Neither commit is an ancestor of the other, so no review saw both.
///
/// It matters because of WHERE it is reached from: `/v1/org/runtime/launch`
/// runs `project_activity_fence`, which is an `activity::mutate`. That is the
/// one place an operator ever read `corrupt store: activity` — the loudest
/// sentence this product has — for a database that was perfectly intact
/// (#1031).
///
/// The subject is deliberately an invariant `reconcile_people` CANNOT repair —
/// a ledger stamped with another company's slug — so this test cannot pass by
/// being healed instead of refused.
#[test]
fn a_mutation_on_a_broken_invariant_is_a_store_failure_not_corruption() {
    let mut world = World::new();
    let mut ledger = world.ledger();
    ledger.organization = "not-this-company".to_string();
    put(&mut world.ledgers, &ledger).expect("a foreign slug is a writable byte shape");

    let supervision = world.supervision();
    let manifest = world.manifest.clone();
    let err = mutate(&mut world.ledgers, &manifest, &supervision, |_draft, _ctx, _at| Ok(()))
        .expect_err("a ledger stamped with another company must not mutate");
    assert_eq!(err.kind(), "StoreFailure", "intact bytes that broke a rule are not corruption");
    let cause = err.cause().expect("a store failure carries the reason it failed");
    assert!(cause.contains("not-this-company"), "the cause must name the invariant: {cause}");
    assert!(
        !err.to_string().contains("corrupt"),
        "the word that sends an operator hunting for damaged bytes: {err}"
    );
}

/// …and the narrow door stays open. A body that genuinely does not DECODE is
/// still `Corrupt` through `mutate`, because that one IS evidence of damage.
/// Relabelling the invariant check must not quietly relabel this with it —
/// that would hide the failure mode the loud word exists for.
#[test]
fn a_mutation_on_an_undecodable_body_is_still_corruption() {
    let mut world = World::new();
    let supervision = world.supervision();
    let manifest = world.manifest.clone();
    world.ledgers.put_document(ActivityStore::NAME, "{ this is not an activity ledger");

    let err = mutate(&mut world.ledgers, &manifest, &supervision, |_draft, _ctx, _at| Ok(()))
        .expect_err("bytes that do not decode must not mutate");
    assert_eq!(err.kind(), "Corrupt", "a body that will not decode IS damaged");
}

/// The reason a validation failure happened must ESCAPE, not just the label.
///
/// `read` used to be `parse(...).filter(|l| validate(l).is_ok()).ok_or(Corrupt)`
/// — and `filter` discarded a `Refusal` that names the exact broken invariant.
/// That is why `corrupt store: activity` was unexplainable at the only place an
/// operator reads it (#1031: four sightings, identical label, zero causes).
///
/// The reason is a side effect on stderr, so an in-process assertion cannot see
/// it; this re-runs the test binary as a child and asserts on what it printed.
#[test]
fn a_validation_failure_prints_which_invariant_broke() {
    const MARKER: &str = "CHIEFD_ACTIVITY_VALIDATE_ECHO_CHILD";
    if std::env::var(MARKER).is_ok() {
        let mut world = World::new();
        let mut ledger = world.ledger();
        let ghost = PersonActivityState {
            person_id: "ghost-person".to_string(),
            ..ledger.people.values().next().expect("a seeded person").clone()
        };
        ledger.person_order.push("ghost-person".to_string());
        ledger.people.insert("ghost-person".to_string(), ghost);
        put(&mut world.ledgers, &ledger).expect("a ghost person is a writable byte shape");
        let _ = read(&world.ledgers, &world.manifest);
        return;
    }
    let exe = std::env::current_exe().expect("the test binary");
    let output = std::process::Command::new(exe)
        .args([
            "--exact",
            "store::activity::tests::a_validation_failure_prints_which_invariant_broke",
            "--nocapture",
        ])
        .env(MARKER, "1")
        .output()
        .expect("re-running the test binary as a child");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[activity] store error:"), "no store line: {stderr}");
    assert!(
        stderr.contains("ghost-person"),
        "the refusal must name the offending person, not just the store: {stderr}"
    );
}

/// Structural activity reconciliation is an explicit migrate-on-touch action,
/// not a relaxation of the ordinary read. A real person removal first leaves
/// the activity aggregate stale; the repair removes the departed person's
/// state and their open transition in the same candidate before validation.
#[test]
fn structural_reconcile_migrates_a_departed_person_and_transition() {
    let mut world = World::new();
    let transition_id = world.begin("signal-researcher", TransitionAction::Park, None);
    world.advance(1_000);
    world.mutate_manifest(|draft| {
        draft.people.remove("signal-researcher");
        draft.people_order.retain(|person_id| person_id != "signal-researcher");
    });

    let before = read(&world.ledgers, &world.manifest)
        .expect_err("the ordinary read must still fail closed on the stale aggregate");
    assert_eq!(before.kind(), "StoreFailure");

    assert!(
        reconcile_structural(&mut world.ledgers, &world.manifest)
            .expect("the explicit structural migration repairs only stale references"),
        "the departed person requires a durable activity change"
    );
    let repaired = world.ledger();
    assert!(!repaired.people.contains_key("signal-researcher"));
    assert!(!repaired.person_order.iter().any(|person_id| person_id == "signal-researcher"));
    assert!(!repaired.transitions.contains_key(&transition_id));
    assert!(!repaired.transition_order.iter().any(|id| id == &transition_id));
    assert!(
        !reconcile_structural(&mut world.ledgers, &world.manifest)
            .expect("a repaired aggregate is a no-op"),
        "the second call must not manufacture another activity write"
    );
}

#[test]
fn structural_reconcile_refuses_absent_or_malformed_activity_without_writing() {
    let mut absent = World::new();
    assert!(absent.ledgers.remove_document(ActivityStore::NAME));
    let error = reconcile_structural(&mut absent.ledgers, &absent.manifest)
        .expect_err("a structural repair must never fabricate an absent ledger");
    assert_eq!(error.kind(), "Absent");
    assert!(absent.ledgers.document_body(ActivityStore::NAME).is_none());

    let mut malformed = World::new();
    malformed.ledgers.put_document(ActivityStore::NAME, "not JSON".to_string());
    let before =
        malformed.ledgers.document_body(ActivityStore::NAME).expect("stored body").to_string();
    let error = reconcile_structural(&mut malformed.ledgers, &malformed.manifest)
        .expect_err("malformed activity is not a structural migration candidate");
    assert_eq!(error.kind(), "Corrupt");
    assert_eq!(malformed.ledgers.document_body(ActivityStore::NAME), Some(before.as_str()));
}

#[test]
fn structural_reconcile_refuses_a_candidate_invalid_after_reconciliation() {
    let mut world = World::new();
    let mut ledger = world.ledger();
    ledger.organization = "foreign-company".to_string();
    put(&mut world.ledgers, &ledger).expect("the byte shape is serializable");
    let before = world.ledgers.document_body(ActivityStore::NAME).expect("stored body").to_string();

    let error = reconcile_structural(&mut world.ledgers, &world.manifest)
        .expect_err("a non-structural invariant violation must remain fail-closed");
    assert_eq!(error.kind(), "StoreFailure");
    assert_eq!(world.ledgers.document_body(ActivityStore::NAME), Some(before.as_str()));
}

#[test]
fn an_applied_transition_with_no_reflection_data_validates_and_reconstructs_cleanly() {
    // THE #751-P4 FAIL-CLOSED REGRESSION. Activity is a fail-closed store: a
    // single validator that refuses state already on disk refuses every read
    // and every mutation, which means every duty at every existing company.
    //
    // Before this packet, an `applied` transition asserted that a reflection
    // existed: `validate` rejected one without a well-formed payload, and
    // `reconcile` HARD-REFUSED `handoff-not-durable` for an applied transition
    // with no durable reflection row. Deleting the reflection concept makes
    // "applied with no reflection" the ONLY shape there is, so both rules had
    // to relax in the SAFE direction. This test is the proof that they did:
    // such a ledger must decode, validate, read back, and survive a reconcile
    // pass — never `Corrupt`, never a refusal.
    let mut world = World::new();
    let transition = world.begin("signal-researcher", TransitionAction::Park, None);
    let manifest = world.manifest.clone();
    let supervision = world.supervision();
    mutate(&mut world.ledgers, &manifest, &supervision, |draft, _ctx, at| {
        let record = draft
            .transitions
            .get_mut(&transition)
            .ok_or_else(|| ChiefdError::refused(INTERNAL_INCONSISTENCY, "test setup"))?;
        record.status = TransitionStatus::Applied;
        record.applied_at = Some(at.to_string());
        Ok(())
    })
    .expect("an applied transition with no reflection data COMMITS (mutate validates)");

    // The person still POINTS at it — the exact shape `handoff-not-durable`
    // used to refuse on every reconcile pass.
    let ledger = read(&world.ledgers, &manifest).expect("the durable read must not be Corrupt");
    assert_eq!(ledger.transitions[&transition].status, TransitionStatus::Applied);
    assert_eq!(
        ledger.people["signal-researcher"].active_transition_id.as_deref(),
        Some(transition.as_str()),
        "precondition: the applied transition is still the active pointer"
    );
    validate(&ledger, &manifest).expect("an applied transition with no payload is legal");

    // The wire body carries no reflection key at all, and decodes+validates
    // straight from bytes.
    let body = world
        .ledgers
        .document_body(ActivityStore::NAME)
        .expect("activity document present")
        .to_string();
    assert!(!body.contains("reflection"), "no reflection key may survive on the wire: {body}");
    let decoded: ActivityLedger = serde_json::from_str(&body).expect("the body decodes");
    validate(&decoded, &manifest).expect("the decoded ledger validates");

    // And the control loop keeps running over it, repeatedly.
    for _ in 0..3 {
        world.reconcile(everyone(), &[]);
    }
    assert_eq!(
        world.ledger().transitions[&transition].status,
        TransitionStatus::Applied,
        "reconcile neither refuses nor demotes an applied transition back to awaiting_handoff"
    );
}

/// A repeat [`release`] converges: the second call re-writes the same `ready`
/// status, so the durable bytes are identical. This is the property the deleted
/// reflection-content-conflict refusal used to protect by comparing payloads;
/// with no payload it holds by construction, and it is still worth pinning
/// because a retrying caller must never be punished for at-least-once delivery.
#[test]
fn a_repeated_release_converges_on_byte_identical_durable_state() {
    let mut world = World::new();
    let transition = world.begin("signal-researcher", TransitionAction::Park, None);
    world.release_on(&transition, "signal-researcher");
    let first =
        world.ledgers.document_body(ActivityStore::NAME).expect("activity document").to_string();

    world.advance(60_000);
    world.release_on(&transition, "signal-researcher");
    let replay =
        world.ledgers.document_body(ActivityStore::NAME).expect("activity document").to_string();

    assert_eq!(
        world.ledger().transitions[&transition].status,
        TransitionStatus::Ready,
        "the transition stays released"
    );
    assert_eq!(first, replay, "a replay must produce byte-identical durable state");
}

#[test]
fn require_ready_accepts_a_committed_released_transition() {
    let mut world = World::new();
    let manifest = world.manifest.clone();
    let supervision = world.supervision();

    // Nothing to be ready with.
    let err = require_ready(
        &world.ledgers,
        &manifest,
        &supervision,
        "signal-researcher",
        TransitionAction::Park,
    )
    .expect_err("no transition, no readiness");
    assert_eq!(err.code(), Some(HANDOFF_REQUIRED));

    // An OPEN transition is not enough either: `require_ready` gates on the
    // release having happened, which is the whole fence.
    let transition = world.begin("signal-researcher", TransitionAction::Park, None);
    let supervision = world.supervision();
    let err = require_ready(
        &world.ledgers,
        &manifest,
        &supervision,
        "signal-researcher",
        TransitionAction::Park,
    )
    .expect_err("an unreleased transition does not satisfy require_ready");
    assert_eq!(err.code(), Some(HANDOFF_REQUIRED));

    world.release_on(&transition, "signal-researcher");
    let supervision = world.supervision();
    require_ready(
        &world.ledgers,
        &manifest,
        &supervision,
        "signal-researcher",
        TransitionAction::Park,
    )
    .expect("a committed released transition satisfies require_ready");
}

/// The same committed state, seen by `reconcile` rather than by
/// `require_ready`: a released transition lets the park PROCEED (ready →
/// applied); it is never flipped back to `awaiting_handoff`.
#[test]
fn reconcile_applies_a_committed_released_transition() {
    let mut world = World::new();
    let transition = world.begin("signal-researcher", TransitionAction::Park, None);
    world.release_on(&transition, "signal-researcher");
    assert_eq!(world.ledger().transitions[&transition].status, TransitionStatus::Ready);

    world.reconcile(everyone(), &[]);
    assert_eq!(
        world.ledger().transitions[&transition].status,
        TransitionStatus::Applied,
        "a committed released transition's park proceeds"
    );
}

/// `release` is authenticated, never claimed, so a transition belonging to
/// somebody else is simply unknown.
#[test]
fn a_release_for_another_persons_transition_is_unknown() {
    let mut world = World::new();
    let transition = world.begin("signal-researcher", TransitionAction::Park, None);
    let manifest = world.manifest.clone();
    let supervision = world.supervision();
    let err = release(
        &mut world.ledgers,
        &manifest,
        &supervision,
        &ReleaseInput { transition_id: transition.clone(), person_id: "quant-head".to_string() },
    )
    .expect_err("only the transition's own person may release it");
    assert_eq!(err.code(), Some(UNKNOWN_TRANSITION));
    assert_eq!(world.ledger().transitions[&transition].status, TransitionStatus::AwaitingHandoff);
}

// --- the fence --------------------------------------------------------------

/// The fence is applied **last**, after every demand reason: replayed durable
/// demand can never open the fleet on its own.
#[test]
fn the_fence_overrides_demand_and_is_written_into_the_durable_desired_state() {
    let mut world = World::new();
    let snapshot = world.reconcile(LaunchFence::deny_all(), &["signal-researcher"]);
    let decision = &snapshot.people["signal-researcher"];
    assert!(
        decision.reasons.contains(&ActivityReason::Requested),
        "the demand reason is still computed — the fence overrides, it does not erase"
    );
    assert!(!decision.active, "…and the fence wins");
    assert!(
        !world.ledger().people["signal-researcher"].last_desired_active,
        "the durable desired state is written FENCED, so the supervisor's \
         exact-projection comparison stays consistent"
    );
}

/// The FENCE still never takes the root down — which is a different claim from
/// "the root is always up", and this test used to conflate them.
///
/// It passed on the CEO's unconditional demand, so it proved nothing about the
/// fence. With that demand deleted the CEO is REQUESTED here, which isolates
/// the property actually under test: no fence, not even the empty one, may
/// switch off a root that something has asked for. That exemption is
/// deliberately retained — it is what lets the operator's door re-admit the
/// root without first minting a per-node intent record.
#[test]
fn no_fence_takes_down_a_root_that_has_been_asked_for() {
    let mut world = World::new();
    for fence in [LaunchFence::deny_all(), LaunchFence::fenced(Vec::new()), LaunchFence::Unfenced] {
        let snapshot = world.reconcile(fence, &["chief"]);
        assert!(
            snapshot.people["chief"].active,
            "the fence is applied last and must not veto demand for the root"
        );
    }
}

/// Absence is corruption, never a default. `seed` is the only constructor of an
/// initial ledger, so a company whose document vanished has LOST it — and a
/// mutator that fabricated a fresh empty one would turn total state loss into a
/// silent no-op write. Twin of
/// `supervision::tests::a_mutation_refuses_an_absent_document_instead_of_reseeding_it`.
#[test]
fn a_mutation_refuses_an_absent_document_instead_of_reseeding_it() {
    let mut world = World::new();
    assert!(world.ledgers.remove_document(ActivityStore::NAME), "the document was there");

    let supervision = world.supervision();
    let manifest = world.manifest.clone();
    let err = mutate(&mut world.ledgers, &manifest, &supervision, |_, _, _| Ok(()))
        .expect_err("an absent activity document is refused, never reseeded");
    // #105: absent is its own classification. The BEHAVIOUR this test exists
    // for is unchanged and still asserted below -- the mutation refuses and
    // writes nothing -- but a document that was never written must no longer
    // report as damaged bytes.
    assert_eq!(err.kind(), "Absent");
    assert!(
        world.ledgers.document_body(ActivityStore::NAME).is_none(),
        "and nothing may be written on the way out"
    );
}

/// #105: the READ path must distinguish the two failures as well, with a
/// positive control — a test that only proves "absent is Absent" cannot show
/// the classification is narrow, and a fix that swallowed real corruption too
/// would pass it.
#[test]
fn an_absent_document_reads_as_absent_and_damaged_bytes_still_read_as_corrupt() {
    let mut world = World::new();
    let manifest = world.manifest.clone();

    assert!(world.ledgers.remove_document(ActivityStore::NAME), "the document was there");
    let absent = read(&world.ledgers, &manifest).expect_err("nothing to read");
    assert_eq!(absent.kind(), "Absent", "never written is not damaged");
    assert_eq!(absent.to_string(), "store never written: activity");

    // Positive control: bytes that ARE present but unreadable are still
    // corruption, which is the whole reason the split is safe.
    world.ledgers.put_document(ActivityStore::NAME, "{ not json".to_string());
    let corrupt = read(&world.ledgers, &manifest).expect_err("unparseable");
    assert_eq!(corrupt.kind(), "Corrupt", "present-but-unreadable is still corruption");
}

// --- #29 pointer sweep: apply_pointer_clears --------------------------------

use crate::runtime::pointer_sweep::ClearReason;

/// Drive a real transition to `Applied` (the only status that both passes
/// `validate` while pointed-at and can strand a live pointer): start the person,
/// pause their unit, let the removal handoff open, release it, and reconcile it
/// applied. Returns the world and the applied transition id.
fn applied_transition_world() -> (World, String) {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.mutate_manifest(|draft| {
        draft.departments.get_mut("quant").expect("quant").state = UnitState::Paused;
    });
    world.reconcile(everyone(), &[]);
    let transition_id = world
        .ledger()
        .active_transition("signal-researcher")
        .expect("a pending handoff")
        .id
        .clone();
    world.release_on(&transition_id, "signal-researcher");
    world.reconcile(everyone(), &[]);
    let ledger = world.ledger();
    assert_eq!(ledger.transitions[&transition_id].status, TransitionStatus::Applied);
    (world, transition_id)
}

/// Point a person at a transition via the private `put` (a pointer at an
/// *applied* transition is a valid ledger, so this survives the next read).
fn point_at(world: &mut World, person: &str, transition_id: &str) {
    let mut ledger = world.ledger();
    ledger.people.get_mut(person).expect("person").active_transition_id =
        Some(transition_id.to_owned());
    put(&mut world.ledgers, &ledger).expect("re-point: pointer->applied is valid");
}

#[test]
fn apply_pointer_clears_leaves_a_claimable_applied_pointer_alone() {
    // The critical live-safety property: a stale plan that names an APPLIED
    // transition as cancelled must be dropped by the apply-time re-verify,
    // because an applied handoff is still legitimately claimable.
    let (mut world, transition_id) = applied_transition_world();
    point_at(&mut world, "signal-researcher", &transition_id);
    let supervision = world.supervision();
    let action = ClearPointerAction {
        person_id: "signal-researcher".to_owned(),
        transition_id: transition_id.clone(),
        reason: ClearReason::Cancelled,
    };
    let manifest = world.manifest.clone();
    let cleared = apply_pointer_clears(&mut world.ledgers, &manifest, &supervision, &[action])
        .expect("sweep");

    assert!(cleared.is_empty(), "re-verify against the current status drops it");
    assert_eq!(
        world.ledger().people["signal-researcher"].active_transition_id.as_deref(),
        Some(transition_id.as_str()),
        "the claimable handoff is preserved — the critical live-safety property",
    );
}

#[test]
fn apply_pointer_clears_drops_an_action_whose_pointer_has_moved() {
    let (mut world, transition_id) = applied_transition_world();
    point_at(&mut world, "signal-researcher", &transition_id);
    let supervision = world.supervision();
    // The plan named a transition the pointer no longer holds.
    let action = ClearPointerAction {
        person_id: "signal-researcher".to_owned(),
        transition_id: "transition:999:signal-researcher:park".to_owned(),
        reason: ClearReason::Cancelled,
    };
    let manifest = world.manifest.clone();
    let cleared = apply_pointer_clears(&mut world.ledgers, &manifest, &supervision, &[action])
        .expect("sweep");

    assert!(cleared.is_empty());
    assert!(
        world.ledger().people["signal-researcher"].active_transition_id.is_some(),
        "a moved pointer is never cleared on a stale plan",
    );
}

// --- BUG-7: an applied-terminal transition is never a fence ------------------
//
// Live 2026-07-22 (tribes-capital; `runtime/takeover-bug-log.md`): a person
// whose `activeTransitionId` still named an APPLIED park hit the action/target
// refusal in `ensure_matching_transition` before the applied-terminal
// start-fresh rule was ever evaluated, and the refusal rolled back the whole
// reconcile commit every cycle. The applied-terminal check now runs FIRST:
// terminal history starts fresh on any new terms and can never hard-refuse.

/// Give an applied transition a lifecycle intent — the shape the incident
/// pinned ("an APPLIED park transition that still carries an intentId").
fn bind_intent(world: &mut World, transition_id: &str, intent_id: &str) {
    let mut ledger = world.ledger();
    ledger.transitions.get_mut(transition_id).expect("transition").intent_id =
        Some(intent_id.to_owned());
    put(&mut world.ledgers, &ledger).expect("an intent on an applied transition is valid");
}

/// Move `signal-researcher` to the `it` unit, so the next pass derives a
/// structural transfer — the "collided with the pass's structural/activity
/// computation" half of the incident.
fn move_signal_researcher_to_it(world: &mut World) {
    world.mutate_manifest(|draft| {
        let person = draft.people.get_mut("signal-researcher").expect("worker");
        person.department_id = "it".to_string();
    });
}

/// The incident's exact shape: an applied park still carrying its lifecycle
/// intentId, pinned as the person's active pointer, while the pass derives a
/// structural transfer. Unfixed code refuses `transition-conflict` at the
/// action/target check and the whole reconcile commit rolls back.
#[test]
fn an_applied_intent_bound_park_starts_fresh_on_a_structural_change() {
    let (mut world, parked) = applied_transition_world();
    bind_intent(&mut world, &parked, "lifecycle:stop:signal-researcher");
    point_at(&mut world, "signal-researcher", &parked);

    move_signal_researcher_to_it(&mut world);
    world.reconcile(everyone(), &["signal-researcher"]);

    let ledger = world.ledger();
    assert_eq!(
        ledger.transitions[&parked].status,
        TransitionStatus::Applied,
        "the applied park stays untouched terminal history"
    );
    let current = ledger
        .active_transition("signal-researcher")
        .expect("the structural change opened a fresh transition");
    assert_eq!(current.action, TransitionAction::Transfer);
    assert_eq!(current.to_department_id.as_deref(), Some("it"));
    assert!(current.status.is_pending(), "the fresh transfer waits for its own handoff");
}

/// The unowned variant — the byte shape of the captured live document, where
/// the applied parks were ordinary idle auto-parks.
#[test]
fn an_applied_unowned_park_starts_fresh_on_a_structural_change() {
    let (mut world, parked) = applied_transition_world();
    point_at(&mut world, "signal-researcher", &parked);

    move_signal_researcher_to_it(&mut world);
    world.reconcile(everyone(), &["signal-researcher"]);

    let ledger = world.ledger();
    assert_eq!(ledger.transitions[&parked].status, TransitionStatus::Applied);
    let current = ledger
        .active_transition("signal-researcher")
        .expect("the structural change opened a fresh transition");
    assert_eq!(current.action, TransitionAction::Transfer);
    assert_eq!(current.to_department_id.as_deref(), Some("it"));
}

/// The reorder must not weaken the rule it moved: an applied intent-bound
/// handoff is still consumed by its own structural retry — same action, same
/// target, same intent — and mints nothing.
#[test]
fn an_applied_intent_bound_handoff_still_serves_its_own_retry() {
    let (mut world, parked) = applied_transition_world();
    bind_intent(&mut world, &parked, "lifecycle:stop:signal-researcher");
    point_at(&mut world, "signal-researcher", &parked);

    let minted_before = world.ledger().transition_order.len();
    let again = world.begin(
        "signal-researcher",
        TransitionAction::Park,
        Some("lifecycle:stop:signal-researcher"),
    );
    assert_eq!(again, parked, "an identical retry consumes the applied handoff");
    assert_eq!(
        world.ledger().transition_order.len(),
        minted_before,
        "no fresh transition was minted"
    );
}

/// …and it still starts fresh for a DIFFERENT explicit lifecycle intent, the
/// second half of the rule the reorder moved.
#[test]
fn an_applied_intent_bound_handoff_starts_fresh_for_a_different_intent() {
    let (mut world, parked) = applied_transition_world();
    bind_intent(&mut world, &parked, "intent-one");
    point_at(&mut world, "signal-researcher", &parked);

    let fresh = world.begin("signal-researcher", TransitionAction::Park, Some("intent-two"));
    assert_ne!(fresh, parked, "a different lifecycle intent gets its own transition");
    assert_eq!(
        world.ledger().active_transition("signal-researcher").map(|t| t.id.clone()),
        Some(fresh)
    );
}

// TOMBSTONE (#751-P4): the whole #452 fixture family lived here — an
// over-budget park reflection was quarantined (not fatal) on read, clamped to
// disk on the next write, and structurally-corrupt prose stayed fatal. Those
// tests pinned the reflection payload's character budget and canonicalization,
// and they die with it. Nothing they covered survives as behaviour: there is no
// payload to be over-budget, so there is no quarantine, no clamp, and no
// heal-on-write pass.

// --- #455: the `forced` terminal status (schema drift with the TS launcher) --

/// Force one park transition to `Forced` in the durable ledger, as the TS
/// launcher's automatic-park drain (#337) does. Leaves the person's pointer at
/// it, which `forced` (unlike `cancelled`) permits.
fn force_park(world: &mut World, transition_id: &str) {
    let manifest = world.manifest.clone();
    let supervision = world.supervision();
    mutate(&mut world.ledgers, &manifest, &supervision, |draft, _ctx, at| {
        let record = draft
            .transitions
            .get_mut(transition_id)
            .ok_or_else(|| ChiefdError::refused(INTERNAL_INCONSISTENCY, "test setup"))?;
        record.status = TransitionStatus::Forced;
        record.forced_at = Some(at.to_string());
        Ok(())
    })
    .expect("a forced park commits and validates");
}

/// The P0 regression (#455): a ledger carrying a `"forced"` transition — exactly
/// what the TS launcher writes at `org-activity-state.ts:41` — DECODES and
/// VALIDATES. Before `TransitionStatus::Forced` existed, serde could not decode
/// the `"forced"` status string, so `read` returned `Corrupt{store:"activity"}`
/// before validation ran and every forced idle-park bricked supervision
/// fleet-wide. Revert the variant and this fails: the wire body no longer
/// decodes.
#[test]
fn a_forced_transition_decodes_and_validates_from_the_wire() {
    let mut world = World::new();
    let transition = world.begin("signal-researcher", TransitionAction::Park, None);
    force_park(&mut world, &transition);

    // The durable body is the TS wire shape: status "forced" plus a forcedAt.
    let body = world
        .ledgers
        .document_body(ActivityStore::NAME)
        .expect("activity document present")
        .to_string();
    assert!(body.contains("\"status\":\"forced\""), "wire status must be 'forced'");
    assert!(body.contains("\"forcedAt\""), "a forced transition carries forcedAt");

    // A raw serde decode of that body succeeds (the exact step that was `Corrupt`).
    let decoded: ActivityLedger = serde_json::from_str(&body).expect("a 'forced' body decodes");
    validate(&decoded, &world.manifest).expect("a 'forced' ledger validates");

    // And the durable read path (decode + validate) returns Ok, not Corrupt.
    let ledger = read(&world.ledgers, &world.manifest).expect("read must not be Corrupt");
    let record = &ledger.transitions[&transition];
    assert_eq!(record.status, TransitionStatus::Forced);
    assert_eq!(record.status.as_str(), "forced");
    assert!(record.forced_at.is_some());
    // The pointer survives a forced park (forced is not cancelled).
    assert_eq!(
        ledger.active_transition("signal-researcher").map(|t| t.id.clone()),
        Some(transition),
    );
}

/// A force-park was never released: it is applied *because* the release never
/// arrived. `is_released` must stay false for `Forced`, so nothing downstream
/// treats a force-park as an owner-authorized outcome.
#[test]
fn forced_is_not_released() {
    assert!(!TransitionStatus::Forced.is_released());
    assert!(TransitionStatus::Forced.is_terminal());
    // The released statuses are unchanged.
    assert!(TransitionStatus::Ready.is_released());
    assert!(TransitionStatus::Applied.is_released());
    // …and the open ones are not released either.
    assert!(!TransitionStatus::AwaitingHandoff.is_released());
    assert!(!TransitionStatus::Overdue.is_released());
    assert!(!TransitionStatus::Cancelled.is_released());
}

/// `validate` mirrors the two TS invariants for `forced`
/// (`org-activity-state.ts:289,307`): it is only ever a `park`, and it must
/// carry a `forcedAt` stamp.
#[test]
fn validate_rejects_a_malformed_forced_transition() {
    let mut world = World::new();
    let transition = world.begin("signal-researcher", TransitionAction::Park, None);
    force_park(&mut world, &transition);
    let good = read(&world.ledgers, &world.manifest).expect("baseline forced ledger reads");

    // A forced status with no forcedAt is rejected.
    let mut no_stamp = good.clone();
    no_stamp.transitions.get_mut(&transition).expect("transition").forced_at = None;
    let err = validate(&no_stamp, &world.manifest).expect_err("forced needs a forcedAt");
    assert_eq!(err.code, INVALID_INPUT);

    // A forced status on a non-park action is rejected.
    let mut not_park = good;
    not_park.transitions.get_mut(&transition).expect("transition").action =
        TransitionAction::Transfer;
    let err = validate(&not_park, &world.manifest).expect_err("only a park can be forced");
    assert_eq!(err.code, INVALID_INPUT);
}

/// A `forced` transition is terminal: it can be neither released nor
/// abandoned. Both guards are the same terminal rule, reached from the two
/// different verbs a caller can hold.
#[test]
fn a_forced_transition_is_terminal_for_release_and_abandon() {
    let mut world = World::new();
    let transition = world.begin("signal-researcher", TransitionAction::Park, None);
    force_park(&mut world, &transition);

    let manifest = world.manifest.clone();
    let supervision = world.supervision();
    let release_err = release(
        &mut world.ledgers,
        &manifest,
        &supervision,
        &ReleaseInput {
            transition_id: transition.clone(),
            person_id: "signal-researcher".to_string(),
        },
    )
    .expect_err("a forced transition cannot be released");
    assert_eq!(release_err.code(), Some(TRANSITION_TERMINAL));

    let supervision = world.supervision();
    let abandon_err = abandon_transition(
        &mut world.ledgers,
        &manifest,
        &supervision,
        &transition,
        "signal-researcher",
        "give up",
    )
    .expect_err("a forced transition cannot be abandoned");
    assert_eq!(abandon_err.code(), Some(TRANSITION_TERMINAL));
}

// --- #312: terminal-transition retention cap --------------------------------

/// The `activity` blob was unbounded: the manifest-validity prune keeps every
/// finished transition as long as its person + departments exist, and routine
/// idle auto-park mints one on the hot reconcile path, so a stable-roster
/// company grew this footer-polled doc to 1.29MB live — a direct chiefd
/// idle-CPU multiplier (~3ms/MB, #310/#312). `reconcile_people` now caps
/// terminal (Applied/Cancelled/Forced) history at ACTIVITY_TERMINAL_TRANSITION_LIMIT,
/// dropping the oldest first (transition_order is chronological), while never
/// touching a live transition or one a person's active_transition_id points at.
/// Mirrors the TS twin and the supervision cap (#329).
#[test]
fn terminal_transitions_are_capped_dropping_oldest_first_keeping_live_and_referenced() {
    let world = World::new();
    let manifest = world.manifest.clone();
    let now = iso_millis(EPOCH + 10_000_000);
    let mut ledger = world.ledger();
    let person = "signal-researcher";

    let statuses =
        [TransitionStatus::Applied, TransitionStatus::Cancelled, TransitionStatus::Forced];
    // +6 terminal records; the oldest (seq 0) is the person's active_transition_id
    // target and is excluded from the droppable set, leaving LIMIT+5 droppable →
    // exactly seq 1..=5 fall off.
    let overflow = ACTIVITY_TERMINAL_TRANSITION_LIMIT + 6;
    let terminal_record = |seq: usize, status: TransitionStatus| -> GracefulTransition {
        let at = iso_millis(EPOCH + seq as i64 * 1_000);
        GracefulTransition {
            id: format!("transition:{seq}:{person}:park"),
            person_id: person.to_string(),
            action: TransitionAction::Park,
            reason: "idle".to_string(),
            intent_id: None,
            placement_department_id: "quant".to_string(),
            to_department_id: None,
            status,
            requested_at: at.clone(),
            handoff_deadline_at: at.clone(),
            applied_at: (status == TransitionStatus::Applied).then(|| at.clone()),
            cancelled_at: (status == TransitionStatus::Cancelled).then(|| at.clone()),
            forced_at: (status == TransitionStatus::Forced).then(|| at.clone()),
            abandoned_at: None,
        }
    };
    for seq in 0..overflow {
        let record = terminal_record(seq, statuses[seq % 3]);
        ledger.transition_order.push(record.id.clone());
        ledger.transitions.insert(record.id.clone(), record);
    }
    // A live (non-terminal) transition — never dropped.
    let mut live = terminal_record(overflow, TransitionStatus::Applied);
    live.id = format!("transition:{overflow}:{person}:park");
    live.status = TransitionStatus::AwaitingHandoff;
    live.applied_at = None;
    ledger.transition_order.push(live.id.clone());
    ledger.transitions.insert(live.id.clone(), live.clone());
    // The oldest terminal (seq 0, Applied) is an inheritable park the person
    // still owns — kept despite being terminal and oldest.
    let referenced = "transition:0:signal-researcher:park".to_string();
    ledger.people.get_mut(person).expect("person state").active_transition_id =
        Some(referenced.clone());

    let changed = reconcile_people(&mut ledger, &manifest, &now);
    assert!(changed, "the cap dropped rows");

    let surviving_terminal = ledger
        .transition_order
        .iter()
        .filter(|id| ledger.transitions[*id].status != TransitionStatus::AwaitingHandoff)
        .count();
    assert_eq!(
        surviving_terminal,
        ACTIVITY_TERMINAL_TRANSITION_LIMIT + 1,
        "cap of terminal history plus the one referenced survivor"
    );
    assert!(ledger.transitions.contains_key(&referenced), "referenced oldest kept");
    assert!(ledger.transitions.contains_key(&live.id), "live transition kept");
    for seq in 1..=5 {
        assert!(
            !ledger.transitions.contains_key(&format!("transition:{seq}:{person}:park")),
            "oldest droppable seq {seq} dropped"
        );
    }
    assert!(
        ledger.transitions.contains_key(&format!("transition:{}:{person}:park", overflow - 1)),
        "newest terminal kept"
    );
    // order/map agree — no dangling entries.
    assert_eq!(ledger.transition_order.len(), ledger.transitions.len());
}

/// Under the cap nothing is dropped — the pass must not churn the ledger.
#[test]
fn terminal_transition_cap_is_a_noop_under_the_limit() {
    let world = World::new();
    let manifest = world.manifest.clone();
    let now = iso_millis(EPOCH + 10_000_000);
    let mut ledger = world.ledger();
    let person = "signal-researcher";
    for seq in 0..3 {
        let at = iso_millis(EPOCH + seq as i64 * 1_000);
        let id = format!("transition:{seq}:{person}:park");
        ledger.transitions.insert(
            id.clone(),
            GracefulTransition {
                id: id.clone(),
                person_id: person.to_string(),
                action: TransitionAction::Park,
                reason: "idle".to_string(),
                intent_id: None,
                placement_department_id: "quant".to_string(),
                to_department_id: None,
                status: TransitionStatus::Applied,
                requested_at: at.clone(),
                handoff_deadline_at: at.clone(),
                applied_at: Some(at),
                cancelled_at: None,
                forced_at: None,
                abandoned_at: None,
            },
        );
        ledger.transition_order.push(id);
    }
    let before = ledger.transition_order.clone();
    reconcile_people(&mut ledger, &manifest, &now);
    assert_eq!(ledger.transition_order, before, "under-cap pass drops nothing");
}

// --- the back-to-back placement fence --------------------------------------
//
// One placement move immediately followed by the move back was refused
// `invalid-input: Person '<id>' is already assigned to '<home>'`. These four
// tests hold the whole shape of that defect and its fix. The sequence below is
// exactly what `/v1/org/staffing/lifecycle` performs — prepare, release, apply
// the structural mutation, settle — with NO reconcile pass anywhere, because
// the reconcile pass is the thing a manager should never have to wait for.
//
// The defect was FOUND through `org_loan` then `org_return`, and the loan
// concept was deleted on 2026-08-13 (operator ruling). The defect is not about
// loans, so these tests are re-expressed with the verb that remains rather than
// deleted with the one that went: two transfers in a row reach it identically.

/// The staffing route's own sequence for a move, minus the reconcile.
fn move_signal_researcher(world: &mut World, to: &str, settle: bool) {
    let transition =
        world.begin_to("signal-researcher", TransitionAction::Transfer, to).expect("a move opens");
    world.release_on(&transition, "signal-researcher");
    world.mutate_manifest(|draft| {
        let person = draft.people.get_mut("signal-researcher").expect("worker");
        // BOTH, always: a person works where they belong, and the concept that
        // let the two differ is gone.
        person.department_id = to.to_string();
    });
    if settle {
        assert!(world.settle("signal-researcher"), "a released non-removal move must settle");
    }
}

#[test]
fn a_move_back_issued_straight_after_a_move_is_not_refused() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    move_signal_researcher(&mut world, "it", true);

    // THE LEDGER AGREES WITH THE MANIFEST, with no reconcile in between. This
    // is the whole fix: the observation of the move commits with the move.
    let ledger = world.ledger();
    assert_eq!(ledger.people["signal-researcher"].last_department_id, "it");
    assert!(
        ledger.active_transition("signal-researcher").is_none(),
        "an applied move leaves no transition in flight to conflict with the next one"
    );

    // THE REGRESSION. This was `Err(invalid-input: Person 'signal-researcher'
    // is already assigned to 'quant')` — the ledger had not seen the first
    // move, so the move back looked like a no-op. The manifest answers now.
    world
        .begin_to("signal-researcher", TransitionAction::Transfer, "quant")
        .expect("a move back straight after a move must open");
}

#[test]
fn without_the_settle_the_return_is_refused_against_the_stale_projection() {
    // THE OTHER DIRECTION, and the proof that the assertion above is not
    // vacuous: run the identical sequence with the settle omitted and the old
    // refusal comes straight back. If `settle_applied_move` is ever removed or
    // stops firing, the test above goes red and this one explains why.
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    move_signal_researcher(&mut world, "it", false);

    let refusal = world
        .begin_to("signal-researcher", TransitionAction::Transfer, "quant")
        .expect_err("an unsettled move leaves the projection stale");
    assert!(
        format!("{refusal}").contains("is already assigned to"),
        "the unsettled path must still be the placement fence, not some other refusal: {refusal}"
    );
}

#[test]
fn the_placement_fence_still_refuses_a_genuine_no_op() {
    // The fence is not disabled — it is fed the truth. A move to where the
    // person already is, is still refused, and the settle is what makes that
    // judgement current rather than lagging.
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    let refusal = world
        .begin_to("signal-researcher", TransitionAction::Transfer, "quant")
        .expect_err("transferring somebody to the department they are already in is a no-op");
    assert!(format!("{refusal}").contains("is already assigned to"), "{refusal}");

    // And after a settled move, the no-op is the OTHER end: 'it' is now where
    // they are, so asking for 'it' again is what has nothing to do.
    move_signal_researcher(&mut world, "it", true);
    let refusal = world
        .begin_to("signal-researcher", TransitionAction::Transfer, "it")
        .expect_err("moving somebody to where they already are is a no-op");
    assert!(format!("{refusal}").contains("is already assigned to"), "{refusal}");
}

#[test]
fn a_removal_is_never_settled_early() {
    // A park or an offboard is NOT observation-only: the reconcile's arm for a
    // removal also decides whether to defer the teardown while live work
    // drains, and that decision needs the host's observations. Settling one
    // here would take that decision away from it, so the settle must decline.
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    let transition = world.begin("signal-researcher", TransitionAction::Park, Some("intent-1"));
    world.release_on(&transition, "signal-researcher");
    assert!(!world.settle("signal-researcher"), "a removal must be left to the reconcile");
    assert_eq!(
        world.ledger().active_transition("signal-researcher").map(|t| t.status),
        Some(TransitionStatus::Ready),
        "the park keeps its pointer and its unapplied status"
    );

    // A person with no transition at all is left alone too — that is the
    // `direct_running_transfer` edge the reconcile recognizes BY the pointer
    // being absent, and advancing its placement here would erase it.
    assert!(!world.settle("it-head"), "nothing to settle is not an error");
}

// ---------------------------------------------------------------------------
// A prior placement whose department the manifest no longer has.
//
// Removing a department strands `last_department_id` /
// `last_department_id` on a person the manifest KEEPS. `validate`
// rejected the whole ledger for it, and `reconcile_people` — the only pass that
// could repair it — ran after the validation that refused it, so the ledger was
// unreadable for good and no mutation could heal it.
//
// The repair is deliberately narrow. A prior placement that merely DIFFERS from
// the manifest is legal and load-bearing: that difference is what raises a
// structural transfer. Only a DANGLING reference is rewritten.
// ---------------------------------------------------------------------------

/// A department id the manifest does not have. Distinctive on purpose: reusing
/// a real fixture name here would quietly turn the negative control positive.
const DISSOLVED_UNIT: &str = "dissolved-signals-desk-1031";

fn strand_prior_placement(world: &mut World, person_id: &str) {
    let mut ledger = world.ledger();
    let state = ledger.people.get_mut(person_id).expect("the person is in the activity ledger");
    state.last_department_id = DISSOLVED_UNIT.to_owned();
    put(&mut world.ledgers, &ledger).expect("a stranded ledger still encodes");
}

/// `transitions` is PRUNED, not archived — the fact #1081 rests on.
///
/// This is the phase-2 gate the design named. Collapsing the home/assigned pair
/// accepts that a historical transition loses the divergence it recorded, and
/// the whole argument for accepting that is that this table never preserved
/// such a row in the first place: the moment the department it names goes, the
/// ROW goes. `staffing_history` is the real audit log, and it carries a
/// different pair (`from_department_id`/`to_department_id`) that this change
/// does not touch.
///
/// Both halves are asserted, because either alone would be consistent with an
/// archive: `validate` REFUSES a row naming a department the manifest no longer
/// has (so no legal ledger can carry one), and the repair pass DROPS the row
/// rather than re-pointing it the way it re-points a person's prior placement.
#[test]
fn a_transition_naming_a_dissolved_department_is_dropped_not_archived() {
    let mut world = World::new();
    let transition_id = world.begin("signal-researcher", TransitionAction::Park, None);

    let mut ledger = world.ledger();
    let transition =
        ledger.transitions.get_mut(&transition_id).expect("the transition was just opened");
    transition.placement_department_id = DISSOLVED_UNIT.to_owned();
    put(&mut world.ledgers, &ledger).expect("a stranded transition still encodes");

    let stored = parse_ledger_tolerating_legacy(
        world.ledgers.document_body(ActivityStore::NAME).expect("a stored body"),
    )
    .expect("the stranded body PARSES - this is an invariant failure, not a parse failure");
    validate(&stored, &world.manifest)
        .expect_err("a transition naming a department the manifest lacks is not a legal ledger");

    let seen = read(&world.ledgers, &world.manifest).expect("a read must not be taken down");
    assert!(
        !seen.transitions.contains_key(&transition_id),
        "the row is DROPPED, not re-pointed: nothing preserves what it recorded"
    );
    assert!(!seen.transition_order.iter().any(|id| id == &transition_id));

    let manifest = world.manifest.clone();
    assert!(
        reconcile_structural(&mut world.ledgers, &manifest)
            .expect("the repair pass must not refuse"),
        "and the drop is durable, not only a repair of the copy handed back"
    );
    let after = parse_ledger_tolerating_legacy(
        world.ledgers.document_body(ActivityStore::NAME).expect("a stored body"),
    )
    .expect("parses");
    assert!(!after.transitions.contains_key(&transition_id));
}

#[test]
fn a_reader_is_never_taken_down_by_a_stranded_prior_placement() {
    let mut world = World::new();
    strand_prior_placement(&mut world, "signal-researcher");

    // The failure this closes: every read path — the launch route, the health
    // pass, the supervision cycle — answered `corrupt store: activity` for as
    // long as the strand lasted. A reader now gets a coherent ledger.
    let seen = read(&world.ledgers, &world.manifest).expect("a read must not be taken down");
    assert_eq!(
        seen.people["signal-researcher"].last_department_id,
        world.manifest.people["signal-researcher"].department_id,
        "the repair value is the manifest placement, the same one the settle path advances to"
    );
}

#[test]
fn the_read_repair_publishes_nothing_and_the_reconcile_is_what_makes_it_durable() {
    let mut world = World::new();
    strand_prior_placement(&mut world, "signal-researcher");

    // A read repairs only the copy it hands back; the stored body still carries
    // the strand, so the repair is never a silent write.
    read(&world.ledgers, &world.manifest).expect("readable");
    let stored = parse_ledger_tolerating_legacy(
        world.ledgers.document_body(ActivityStore::NAME).expect("a stored body"),
    )
    .expect("parses");
    assert_eq!(
        stored.people["signal-researcher"].last_department_id, DISSOLVED_UNIT,
        "a read must not publish its repair"
    );

    // The reconcile is what makes it durable.
    let manifest = world.manifest.clone();
    let applied = reconcile_structural(&mut world.ledgers, &manifest)
        .expect("the repair pass must not refuse");
    assert!(applied, "repairing a stranded placement is a real write");

    let after = parse_ledger_tolerating_legacy(
        world.ledgers.document_body(ActivityStore::NAME).expect("a stored body"),
    )
    .expect("parses");
    assert_eq!(
        after.people["signal-researcher"].last_department_id,
        world.manifest.people["signal-researcher"].department_id,
        "and now the STORED body carries the repair"
    );
}

#[test]
fn a_mutation_heals_a_stranded_prior_placement_instead_of_refusing_forever() {
    let mut world = World::new();
    strand_prior_placement(&mut world, "signal-researcher");
    let supervision = world.supervision();
    let manifest = world.manifest.clone();

    // Before the fix this refused with `corrupt store: activity`, and every
    // later mutation refused the same way — there was no path back.
    mutate(&mut world.ledgers, &manifest, &supervision, |_draft, _ctx, _at| Ok(()))
        .expect("a mutation must repair the ledger rather than refuse it");

    read(&world.ledgers, &world.manifest).expect("readable after the mutation");
}

#[test]
fn a_prior_placement_the_manifest_can_still_name_is_left_exactly_alone() {
    let mut world = World::new();
    let researcher_home = world.manifest.people["signal-researcher"].department_id.clone();
    // Any real department that is NOT this person's home: a legal divergence,
    // and the input that raises a structural transfer.
    let other = world
        .manifest
        .departments
        .keys()
        .find(|unit| **unit != researcher_home)
        .expect("the fixture has more than one department")
        .clone();

    let mut ledger = world.ledger();
    ledger.people.get_mut("signal-researcher").expect("the researcher").last_department_id =
        other.clone();
    put(&mut world.ledgers, &ledger).expect("encodable");

    let manifest = world.manifest.clone();
    reconcile_structural(&mut world.ledgers, &manifest).expect("a legal divergence is not damage");

    let after = read(&world.ledgers, &world.manifest).expect("still readable");
    assert_eq!(
        after.people["signal-researcher"].last_department_id, other,
        "a divergence the manifest CAN name must survive the repair pass — erasing it \
         would erase the transfer it exists to raise"
    );
}

#[test]
fn the_refusal_names_the_department_and_what_would_be_accepted() {
    let mut world = World::new();
    strand_prior_placement(&mut world, "signal-researcher");

    let body = world.ledgers.document_body(ActivityStore::NAME).expect("a stored activity body");
    let ledger = parse_ledger_tolerating_legacy(body).expect("the stranded body still parses");

    let refusal = validate(&ledger, &world.manifest).expect_err("still refused when unrepaired");
    assert!(
        refusal.message.contains(DISSOLVED_UNIT),
        "the refusal must name the department it could not resolve: {}",
        refusal.message
    );
    assert!(
        refusal.message.contains("must name one of its current departments"),
        "and must name what WOULD be accepted: {}",
        refusal.message
    );
}

/// An activity person the manifest does not have. The narrow repair
/// deliberately does not touch this: an unknown person is real damage, not the
/// residue of a department removal, so it still corrupts — which is what makes
/// it the right subject for observing the preserved cause.
const GHOST_PERSON: &str = "ghost-analyst-1031";

fn add_unknown_person(world: &mut World) {
    let mut ledger = world.ledger();
    let mut state = ledger.people.get("signal-researcher").expect("a person to copy").clone();
    state.person_id = GHOST_PERSON.to_owned();
    ledger.people.insert(GHOST_PERSON.to_owned(), state);
    ledger.person_order.push(GHOST_PERSON.to_owned());
    put(&mut world.ledgers, &ledger).expect("a ledger with a ghost still encodes");
}

/// The one test here that observes the ACTUAL SUBJECT. Every other assertion
/// would stay green if the split were reverted to the old causeless
/// `.filter(...)`, because they assert on the returned variant, which is
/// unchanged BY DESIGN. Preserving the cause is a side effect on stderr, so it
/// can only be observed from a child process — the same idiom, and the same
/// reason, as `error::tests::the_preserved_cause_actually_reaches_stderr`.
///
/// It used to strand a prior placement. That case is now REPAIRED before
/// `validate` sees it, so it no longer produces a cause to observe; the test is
/// re-pointed rather than removed, because what it pins — that the reason
/// reaches stderr at all — is exactly what made #1031 explicable.
#[test]
fn the_preserved_cause_names_the_broken_invariant_on_stderr() {
    const MARKER: &str = "CHIEFD_ACTIVITY_PRIOR_PLACEMENT_ECHO_CHILD";
    const TEST_PATH: &str =
        "store::activity::tests::the_preserved_cause_names_the_broken_invariant_on_stderr";

    if std::env::var(MARKER).is_ok() {
        let mut world = World::new();
        add_unknown_person(&mut world);
        let _ = read(&world.ledgers, &world.manifest);
        return;
    }

    let exe = std::env::current_exe().expect("the test binary");
    let output = std::process::Command::new(exe)
        .args(["--exact", TEST_PATH, "--nocapture"])
        .env(MARKER, "1")
        .output()
        .expect("re-running the test binary as a child");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "the child must actually have RUN the case; a renamed test silently runs zero: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[activity] store error:"), "no store line in child stderr: {stderr}");
    assert!(
        stderr.contains("Unknown activity person"),
        "the split must put the BROKEN INVARIANT on stderr, not just a label: {stderr}"
    );
}

/// The repair is only worth having while `validate` would still REFUSE what it
/// repairs. `read` no longer surfaces that refusal — it heals first, which is
/// the whole point — so the refusal is pinned here against `validate` directly.
/// Written during batch-9 integration: the packet that added the repair dropped
/// the case that proved the repair was load-bearing, and a repair whose
/// underlying refusal has quietly been loosened is dead code no test would
/// notice.
#[test]
fn validate_still_refuses_a_stranded_prior_placement_and_names_it() {
    let mut world = World::new();
    strand_prior_placement(&mut world, "signal-researcher");

    let body = world.ledgers.document_body(ActivityStore::NAME).expect("a stored activity body");
    let ledger = parse_ledger_tolerating_legacy(body)
        .expect("the stranded body PARSES - this is an invariant failure, not a parse failure");

    let refusal = validate(&ledger, &world.manifest)
        .expect_err("validate must still refuse a dangling prior placement");
    // Asserted on the three FACTS the message must carry, not on its sentence:
    // who broke it, which dangling id broke it, and the rule. The packet
    // rewrote this message from the older "unknown prior placement" wording,
    // and pinning prose would make an improvement to it read as a regression.
    assert!(
        refusal.message.contains("signal-researcher"),
        "the refusal must name which person broke it: {}",
        refusal.message
    );
    assert!(
        refusal.message.contains(DISSOLVED_UNIT),
        "and the dangling department it points at: {}",
        refusal.message
    );
    assert!(
        refusal.message.contains("no department for"),
        "and the rule that was broken: {}",
        refusal.message
    );
}

/// The repair is narrow ON PURPOSE, and this is the guard for that: a person
/// the manifest does not have is real damage and must still corrupt, never be
/// quietly reconciled away by a read.
#[test]
fn an_unknown_activity_person_still_corrupts_and_is_never_repaired_away() {
    let mut world = World::new();
    add_unknown_person(&mut world);

    let error = read(&world.ledgers, &world.manifest)
        .expect_err("real damage must not be silently repaired");
    // Two packets in this batch moved this assertion, and both moves are kept.
    // The `StoreFailure`/`Corrupt` split: an unknown person is an invariant
    // broken by a body that DECODED fine, so it is a store failure, not damaged
    // bytes. And the kinds now CARRY the cause, so this pins the reason rather
    // than only the label - the whole of what #1031 lacked for seven sightings.
    let ChiefdError::StoreFailure { store, cause } = &error else {
        panic!("an unknown activity person must be a store failure: {error:?}");
    };
    assert_eq!(*store, "activity");
    assert!(
        cause.contains("Unknown activity person"),
        "the cause must name the broken invariant, not just the store: {cause}"
    );
}

// --- the settle countdown: it runs on IDLE, and it fits in two minutes ------
//
// The operator's two sentences, on 2026-08-10, are the whole specification of
// this section:
//
//   "An agent can be settling while thinking. If it starts doing stuff, the
//    settling countdown is turned off. Only when the agent idles is when you
//    kick off the settle countdown."
//
//   "2 minutes maximum from settle start to shutdown."
//
// The first was violated because `idle_since` was stamped from the ABSENCE OF
// DURABLE DEMAND, which says nothing about what the process is doing. The
// second was violated by a SUM: three phases, each sized on its own merits,
// stacked into `shutting down in 3m 47s`.

#[test]
fn an_activity_beat_cancels_the_settle_countdown_before_it_can_park_anyone() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    // The agent reports it went quiet. THE COUNTDOWN STARTS HERE and
    // nowhere else -- chiefd no longer starts a clock on its own bookkeeping.
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    assert!(world.idle_since("signal-researcher").is_some(), "the countdown is running");

    // The lease is now fully spent: the very next pass would admit a routine
    // park, and that park is born TERMINAL -- admission and teardown are one
    // step, with no window in between for anything to intervene. Which is
    // exactly why the beat has to land BEFORE admission rather than rescue a
    // pane afterwards, and why "the countdown is cancelled" is the whole of the
    // protection rather than half of it.
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);

    // The agent starts doing something. This is the pane reporting a fact about
    // itself, and it is the only thing in the system that knows.
    assert!(world.note_activity("signal-researcher", true), "a first beat changes the ledger");
    assert!(
        world.idle_since("signal-researcher").is_none(),
        "the countdown is CANCELLED outright, not paused: the next idle starts a full lease"
    );

    // So the pass that would have parked this person does not: they are not a
    // candidate at all, because the clock that made them one has been reset.
    let after = world.reconcile(everyone(), &[]);
    assert!(after.people["signal-researcher"].active, "a working agent is not settled");
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "no routine park is admitted against a countdown that is not running"
    );
    assert!(
        world.idle_since("signal-researcher").is_none(),
        "and the countdown stays off while the pane keeps reporting work"
    );
}

#[test]
fn the_settle_countdown_starts_only_on_the_transition_to_idle() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);

    // The state that used to start the clock under a thinking agent: no
    // requested demand -- and a pane that is
    // demonstrably mid-turn.
    world.note_activity("signal-researcher", true);
    for _ in 0..3 {
        world.advance(30 * 1_000);
        world.reconcile(everyone(), &[]);
        assert!(
            world.idle_since("signal-researcher").is_none(),
            "no demand is not idleness -- the countdown must not start while the pane reports work"
        );
    }
    // Ninety seconds, already past the whole quiet lease, with nothing admitted.
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "and no routine park may be admitted against a clock that never started"
    );

    // `agent_settled`. THIS is the transition to idle, and the only place the
    // countdown is allowed to start.
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    let idle_at = world.idle_since("signal-researcher").expect("the countdown starts on idle");
    assert_eq!(
        parse_iso_millis(&idle_at),
        Some(world.ledgers.now().0),
        "it starts from the top at the moment the agent went quiet -- not from the earlier \
         moment its demand cleared, which is a clock that was part-spent on work"
    );
}

#[test]
fn a_pane_that_died_mid_turn_cannot_pin_itself_resident_for_ever() {
    // The bound that makes the beat safe. A sticky busy flag set by a pane that
    // then crashed would be a permanent pin, and "a person nobody can ever
    // settle" is the failure this ledger exists to prevent -- so the fact is a
    // BOUNDED freshness test, and the beats stop arriving when the pane dies.
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.note_activity("signal-researcher", true);
    world.reconcile(everyone(), &[]);
    assert!(world.idle_since("signal-researcher").is_none(), "a live beat holds the countdown off");

    world.advance(AGENT_ACTIVITY_LIVENESS_MS + 1);
    world.reconcile(everyone(), &[]);
    assert!(
        world.idle_since("signal-researcher").is_some(),
        "once the beat is stale the countdown starts, so a dead pane settles rather than \
         holding its seat for ever"
    );
}

/// C4: A RE-HIRED PERSON MUST GET A FULL GRACE PERIOD, NOT NONE AT ALL.
///
/// The hole this closes. A person goes quiet, settles, is parked. Their
/// `agent_quiet_at` keeps naming the instant they went quiet, because it is a
/// durable record of something the agent actually said. Re-hire them minutes or
/// days later and that stamp is still there — so `agent_quiet_since` hands the
/// reconcile an ancient quiet instant, `idle_since` is immediately older than
/// `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS`, and the person is a park
/// candidate before the actuator has had a chance to boot them.
///
/// The rule, stated once: a stored agent stamp is evidence about the interval
/// the person was CONTINUOUSLY DESIRED-ACTIVE. The rising edge ends that
/// interval, so everything from before it is evidence about a process that no
/// longer exists and is cleared.
///
/// This is the same class as the stale persisted `idle_since` this branch
/// already fixed by deriving rather than storing — a value outliving the world
/// it described — but the answer differs, and deliberately. `idle_since` could
/// be derived away entirely because chiefd can recompute it. These stamps
/// CANNOT be derived: they are reports the agent sent, and chiefd has no way to
/// recompute what somebody else said. So they stay stored, and what is fixed is
/// the interval they are trusted over.
#[test]
fn a_rehired_person_starts_with_no_clock_rather_than_an_expired_one() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);

    // They work, then settle. The clock starts, honestly. Note the empty
    // `requested` from here on: a requested person carries effective demand,
    // and demand answers the idleness question outright.
    world.note_activity("signal-researcher", true);
    world.reconcile(everyone(), &[]);
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    assert!(
        world.idle_since("signal-researcher").is_some(),
        "an agent that reported settling starts a countdown; that part is correct"
    );

    // They stay quiet for far longer than the whole quiet lease, which is what
    // makes the stale stamp dangerous rather than merely untidy.
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS * 10);
    world.reconcile(everyone(), &[]);

    // Re-hired. The stamps from their previous life must not survive the edge.
    world.reconcile(everyone(), &["signal-researcher"]);
    let ledger = world.ledger();
    let state = &ledger.people["signal-researcher"];
    assert!(
        state.agent_quiet_at.is_none(),
        "the quiet instant described a process that no longer exists"
    );
    assert!(state.agent_active_at.is_none(), "so did the working stamp");
    assert!(
        state.idle_since.is_none(),
        "and therefore there is NO CLOCK: a person about to be booted has made no report yet,          which is a different state from one whose grace has already run out"
    );

    // The proof that matters to an operator: no routine park is admitted
    // against them on the pass that re-hires them, nor on the next one.
    world.reconcile(everyone(), &["signal-researcher"]);
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "a re-hired person must not be parked before they have had a chance to run"
    );

    // And the ordinary path still works from scratch: they settle only once
    // they say so, and only then does a clock start.
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    assert_eq!(
        world.idle_since("signal-researcher").and_then(|at| parse_iso_millis(&at)),
        Some(world.ledgers.now().0),
        "the new clock starts from the top, at the moment the agent went quiet"
    );
}

#[test]
fn an_intent_bound_park_survives_an_activity_beat() {
    // The carve-out, stated as a test rather than as a comment: the beat may
    // cancel only a ROUTINE idle park, which is a decision the system made about
    // a clock. An intent-bound park is a real instruction somebody issued, and a
    // busy agent must not be able to countermand it by being busy.
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    let intended = world.begin("signal-researcher", TransitionAction::Park, Some("intent-1"));

    world.note_activity("signal-researcher", true);

    let ledger = world.ledger();
    assert_eq!(
        ledger.transitions[&intended].status,
        TransitionStatus::AwaitingHandoff,
        "an explicit park is an instruction and survives the beat untouched"
    );
    assert_eq!(
        ledger.people["signal-researcher"].active_transition_id.as_deref(),
        Some(intended.as_str()),
        "and it stays the person's active transition"
    );
}

#[test]
fn the_whole_settle_path_from_idle_to_pane_down_fits_the_five_minute_budget() {
    // THE REQUIREMENT (operator, 2026-08-10, stated repeatedly and finally with
    // a screenshot of `shutting down in 3m 47s`): TWO MINUTES MAXIMUM from
    // settle start to shutdown. TOTAL, not per phase.
    //
    // What produced `3m 47s` was a SUM -- a quiet lease, then a park minted with
    // its own handoff window, then a further overdue lease before the admission
    // was forced terminal -- so shortening any single term proves nothing. This
    // fix is to delete the stack: a routine idle park is now born terminal, so
    // the quiet lease is the ENTIRE window and there is nothing to add to it.
    //
    // That claim is exactly what this test refuses to take on trust. It
    // measures the WHOLE span on the real clock and compares elapsed
    // milliseconds against the requirement — never one constant against
    // another. A single named constant reading 120s proves nothing about a
    // second deadline joining the path later, or a phase being entered twice;
    // both would leave every constant in the file correct and be caught here.
    // The requirement, in milliseconds. The one literal these tests own: every
    // other number here is read from the code under test, and this is the
    // number the code under test is answerable TO.
    // 2026-08-24: *"lets bump the 2mins to a 5mins."* It moves only when the
    // operator moves it — which is the whole reason it is a literal here rather
    // than being read from the code it holds answerable.
    const OPERATOR_SETTLE_CAP_MS: i64 = 5 * 60 * 1_000;

    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    // The agent reports it went quiet. THE COUNTDOWN STARTS HERE and
    // nowhere else -- chiefd no longer starts a clock on its own bookkeeping.
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    let idle_at = parse_iso_millis(
        &world.idle_since("signal-researcher").expect("the settle countdown is running"),
    )
    .expect("idle_since is an ISO instant");

    // Advance one second at a time -- the daemon's converge cadence -- rather
    // than jumping to each phase boundary, so this measures when the pane
    // ACTUALLY goes down and cannot step over a shorter or a longer path.
    //
    // Nothing releases a ROUTINE idle park -- `release` has exactly one
    // production caller, the staffing verb, and the Pi extension surface has no
    // release verb at all -- which is why the park is born terminal and why
    // there is only one path to measure.
    let mut down_at = None;
    for _ in 0..600 {
        world.advance(1_000);
        if !world.reconcile(everyone(), &[]).people["signal-researcher"].active {
            down_at = Some(world.ledgers.now().0);
            break;
        }
    }
    let elapsed = down_at.expect("the pane goes down") - idle_at;
    assert!(
        elapsed <= OPERATOR_SETTLE_CAP_MS,
        "idle -> pane-down took {elapsed} ms, over the operator's {OPERATOR_SETTLE_CAP_MS} ms cap"
    );

    // ...and it is really shutdown, not a pause part-way along a longer path.
    let ledger = world.ledger();
    let transition =
        ledger.active_transition("signal-researcher").expect("the routine park is on record");
    assert_eq!(
        transition.status,
        TransitionStatus::Forced,
        "the routine park is FORCED terminal, which is what makes the quiet lease the whole \
         window rather than its first phase"
    );
    assert!(
        !ledger.people["signal-researcher"].last_desired_active,
        "and the person is durably stopped, which is what shutdown means here"
    );
}

/// THE DEFECT THE DELETED CLIENT-SIDE LEASE WAS PATCHING, PINNED WHERE IT
/// ACTUALLY LIVES.
///
/// On 2026-08-17 a sidebar wake could start a quiet person and lose them again
/// within one reconcile cycle: the one-shot grant lapsed, the desired set
/// dropped them, and the pane the operator had just clicked for went away. The
/// answer at the time was a lease in the ACTUATOR — it re-added the operator's
/// selected person to its own placement — and that lease was measured on
/// 2026-08-18 holding a force-parked person's pane and Pi process open
/// indefinitely, because its expiry could only be released by a signal it
/// suppressed. It is deleted (`chief-cli`'s `actuate::resident`).
///
/// Deleting it is only sound if THIS is true: chiefd holds a freshly woken
/// person up on its own, with no client help, until their idle lease genuinely
/// expires. It is, and this is the mechanism — a person whose quiet lease is
/// still running is an idle-park CANDIDATE, and candidacy itself carries
/// [`ActivityReason::MaintenanceBackpressure`], which is one of the three
/// reasons that keep somebody active. So the person stays up across every pass
/// between going quiet and the lease expiring, on a fence that supplies no
/// fresh `Requested` at all.
///
/// GUARD-RAIL, deliberately: nothing in the change this test ships with can
/// make it fail. Its value is the FUTURE regression — if chiefd ever goes back
/// to dropping a woken person one pass after their demand clears, the operator
/// sees a clicked pane vanish again, and there is no longer a client-side lease
/// THE OPERATOR'S CLICK, ON A BOX WHERE PI TAKES FORTY SECONDS TO BOOT.
///
/// Measured on `taperoom-inc` (2026-08-19, four consecutive clicks): the wake
/// granted `dev`'s launch-intent row and chiefd deleted that same row within a
/// second — `org_events` reads `launch-intent dev upsert` at 23:24:21.570 and
/// `launch-intent dev delete` at 23:24:22.132. dev's Pi went on booting and
/// reported `interactive-loop-ready` at 23:25:02, forty seconds after the
/// click, into a company that no longer wanted him, so the pane was reaped.
/// From the rail: click, nothing, for ever.
///
/// The sibling test above pins the person who ANSWERED and then went quiet.
/// This one pins the person who has not answered YET — no `agent_active_at`,
/// no `agent_quiet_at`, because their pane is still starting. That person has
/// no clock to settle against, so nothing may conclude they are idle, and the
/// fence the operator just paid for must still be theirs on the next pass.
#[test]
fn a_woken_person_whose_pane_is_still_starting_keeps_their_demand() {
    let mut world = World::new();
    let fence = LaunchFence::fenced(["signal-researcher".to_owned()]);

    // The click: fenced and requested in the same pass, exactly as the wake
    // verb writes it.
    let first = world.reconcile(fence.clone(), &["signal-researcher"]);
    assert!(first.people["signal-researcher"].active, "the wake started them");
    assert_eq!(
        world.ledger().people["signal-researcher"].agent_active_at,
        None,
        "the fixture is the case: nothing has reported yet"
    );

    // Every pass after the first supplies NO fresh `Requested` — the fence's
    // start demand lapses once chiefd is desiring them. Pi is still booting
    // through all of these.
    for pass in 0..3 {
        let snapshot = world.reconcile(fence.clone(), &[]);
        let decision = &snapshot.people["signal-researcher"];
        assert!(
            decision.active,
            "pass {pass}: chiefd stopped wanting a person whose pane had not \
             finished starting — the operator's click is discarded before the \
             agent can answer it: {:?}",
            decision.reasons
        );
    }

    // And the whole time, nothing has parked them: a person who never reported
    // has no quiet instant, so there is no idleness to conclude.
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "a person who has never reported was parked as though they had gone idle"
    );
}

/// to hide it. This is the test that says so.
#[test]
fn a_woken_person_who_goes_quiet_is_held_up_by_chiefd_until_the_lease_expires() {
    let mut world = World::new();
    let fence = LaunchFence::fenced(["signal-researcher".to_owned()]);

    // The click: fenced and requested in the same pass, exactly as the wake
    // verb writes it.
    world.reconcile(fence.clone(), &["signal-researcher"]);
    assert!(
        world.ledger().people["signal-researcher"].last_desired_active,
        "the wake started them"
    );

    // The agent answers and reports it went quiet. From here chiefd supplies NO
    // fresh `Requested`: `converge_apply::cycle` only turns a fenced person into
    // requested demand while they are NOT already desired-active, so every pass
    // below asks for nobody.
    world.note_activity("signal-researcher", false);

    for pass in 0..3 {
        let snapshot = world.reconcile(fence.clone(), &[]);
        let decision = &snapshot.people["signal-researcher"];
        assert!(
            decision.active,
            "pass {pass} after the settle dropped a person the operator had just woken; \
             this is the 2026-08-17 defect, and the client-side lease that used to hide it \
             is gone"
        );
        assert!(
            decision.reasons.contains(&ActivityReason::MaintenanceBackpressure),
            "and the reason is candidacy for the park that has not come due yet: {:?}",
            decision.reasons
        );
    }

    // Only the lease ends it, and it ends it with a real park rather than a
    // silent withdrawal.
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    let settled = world.reconcile(fence, &[]);
    assert!(!settled.people["signal-researcher"].active, "the lease expired, so the park lands");
    assert!(
        world
            .ledger()
            .active_transition("signal-researcher")
            .is_some_and(|transition| transition.reason == IDLE_AUTO_PARK_REASON),
        "the person goes down through the idle park, never through a bare withdrawal"
    );
}

/// A WOKEN PERSON IS NOT AN AUTOMATIC-PARK CANDIDATE FOR THE WHOLE LEASE.
///
/// Operator ruling, 2026-08-20: *"If I tell chief to message it, it'll come back
/// up and do the 2min settling. We need it to always do that when woken. Message
/// or not. If woken, it needs to wait the 2 mins."*
///
/// The rule this pins is `settled_idle_stop_lease_expired`, which is the one
/// question every automatic stop asks.
///
/// GUARD-RAIL, and named as one: it is GREEN on a tree that never heard of the
/// wake lease, because `release_idle_park` clears `agent_quiet_at` and the two
/// rules therefore happen to agree today. Its value is that the gesture stays
/// covered end to end whichever of the two mechanisms is doing the work. The
/// sibling below (`a_wake_holds_somebody_up_even_when_their_quiet_clock_says_
/// they_are_long_idle`) is the one that goes red without the floor: it breaks
/// the coincidence deliberately, which is what turns "these clocks agree" into
/// "the operator's decision is respected".
#[test]
fn a_woken_person_is_not_parked_inside_the_lease_and_is_parked_after_it() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.note_activity("signal-researcher", true);

    // A full lease of silence accrues, and the ordinary settle parks them.
    world.note_activity("signal-researcher", false);
    world.reconcile(everyone(), &[]);
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS + 1);
    world.reconcile(everyone(), &[]);
    assert!(
        world.ledger().active_transition("signal-researcher").is_some(),
        "the precondition is somebody the ordinary settle has already parked"
    );

    // THE OPERATOR CLICKS. The park is released, the wake is stamped, and the
    // launch fence asks for them again.
    world.woken("signal-researcher");
    world.reconcile(everyone(), &["signal-researcher"]);
    assert!(
        world.ledger().people["signal-researcher"].last_desired_active,
        "the wake brings them back up"
    );

    // AND THEIR AGENT SAYS IT HAS NOTHING TO DO, ONE SECOND LATER. True, and
    // beside the point: the operator has just asked for this person.
    world.advance(1_000);
    world.note_activity("signal-researcher", false);
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS - 2_000);
    world.reconcile(everyone(), &[]);
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "one millisecond short of the lease, the operator's wake still holds them up"
    );

    // PAST THE LEASE the ordinary settle owns them again, exactly as before.
    world.advance(2_000);
    world.reconcile(everyone(), &[]);
    assert!(
        world
            .ledger()
            .active_transition("signal-researcher")
            .is_some_and(|transition| transition.reason == IDLE_AUTO_PARK_REASON),
        "the lease is a floor, not a pin: past it the ordinary settle parks them"
    );
}

/// THE FLOOR IS THE OPERATOR'S DECISION, NOT A RESTATEMENT OF THE AGENT CLOCKS.
///
/// The test above passes on a tree that never heard of the wake lease, because
/// `release_idle_park` clears `agent_quiet_at` and the two rules therefore
/// happen to agree. That agreement is a coincidence of two independent clocks,
/// and the operator's ruling of 2026-08-20 is not a coincidence: *"If woken, it
/// needs to wait the 2 mins."*
///
/// So this breaks the agreement. The person is woken while still carrying a
/// quiet clock a full lease old — the shape a regression in the wake's row half
/// produces, and the shape a future path that stamps `agent_quiet_at` from an
/// agent-supplied instant would produce by design. Without the floor they are a
/// park candidate on the very next pass, one millisecond after the click.
#[test]
fn a_wake_holds_somebody_up_even_when_their_quiet_clock_says_they_are_long_idle() {
    let mut world = World::new();
    world.reconcile(everyone(), &["signal-researcher"]);
    world.note_activity("signal-researcher", true);
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS * 4);

    // Woken NOW, carrying a quiet instant from a full lease ago.
    let stale = world.now_ms() - ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS - 1;
    world.woken_carrying_a_quiet_clock("signal-researcher", Some(stale));
    world.reconcile(everyone(), &["signal-researcher"]);
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "an operator's click is not undone by a clock that predates it"
    );
    assert!(
        world.ledger().people["signal-researcher"].last_desired_active,
        "and they are up, which is the point of the click"
    );

    // Still held one millisecond short of the window.
    world.advance(ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS - 1);
    world.reconcile(everyone(), &[]);
    assert!(
        world.ledger().active_transition("signal-researcher").is_none(),
        "the whole window, whatever the agent's own clocks say"
    );

    // And released the millisecond it closes.
    world.advance(1);
    world.reconcile(everyone(), &[]);
    assert!(
        world
            .ledger()
            .active_transition("signal-researcher")
            .is_some_and(|transition| transition.reason == IDLE_AUTO_PARK_REASON),
        "past the window the stale clock is authoritative again: a floor, never a pin"
    );
}
