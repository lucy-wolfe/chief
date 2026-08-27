// Live-state regression for the production `transition-conflict` refusal
// (tribes-capital, 2026-07-22 — BUG-7 in `runtime/takeover-bug-log.md`).
// chiefd's reconcile rolled back every cycle: execution-runner's
// `activeTransitionId` still named an APPLIED park transition while the pass
// derived a structural change for the same person, and
// `ensure_matching_transition` evaluated the action/target refusal BEFORE the
// applied-terminal start-fresh rule, so the terminal record hard-refused the
// whole commit.
//
// The fixtures are the captured chief.db documents from mid-incident. The
// capture postdates the operational repair that cleared the worst of it, so two details of the
// failure shape are re-injected in the test — each marked LIVE-SHAPE — exactly
// as the incident log records them: the applied park carried a lifecycle
// intentId, and the manifest had moved so the pass derived a structural
// transition for the pinned person.
#![allow(clippy::expect_used, clippy::panic, missing_docs)]

use chiefd_core::clock::WallMillis;
use chiefd_core::ledger::Ledgers;
use chiefd_core::store::activity::{
    LaunchFence, ReconcileInput, TransitionAction, TransitionStatus,
};
use chiefd_core::store::{activity, organization, supervision};

const NOW_MS: i64 = 1_784_693_100_000; // 2026-07-22T04:05:00Z, mid-incident.

/// The launch-intent fence the live converge cycle computed during the
/// incident: the woken fleet, CEO excluded.
fn live_fence() -> LaunchFence {
    LaunchFence::fenced(
        [
            "engineering-head",
            "engineering-ws1-data-platform-engineer",
            "market-intel-head",
            "validator-2",
            "execution-head",
            "execution-runner",
            "quant-head",
            "quant-analyst",
        ]
        .map(String::from),
    )
}

/// Load the captured documents verbatim, plus the relational assignment rows
/// the supervision ledger hydrates from.
///
/// TOMBSTONE (#751-P4): this loader also used to seed three durable rows in
/// the `reflections` table, one for each of the applied parks
/// `transition:384:mi-data-tech:park`, `transition:385:market-intel-head:park`
/// and `transition:387:execution-runner:park` (verified live at capture time:
/// all three ids had rows there). They were never part of the BUG-7 shape.
/// Exactly those three ids appear because they are the three people whose
/// `activeTransitionId` pointer names an applied park, and `reconcile` ran a
/// durability check on precisely that pointer: an applied transition with no
/// durable reflection row hard-refused `handoff-not-durable` and rolled the
/// whole commit back. Without the three rows the fixture would have died on
/// that refusal before reaching the `transition-conflict` path under test —
/// which is to say they were scaffolding for an unrelated invariant, not
/// evidence about the incident. That invariant, the `reflections` table and
/// the `reflection_handoffs`/`reflection_handoff_items` tables are all gone,
/// so the rows have nothing left to satisfy and the capture loads as-is.
///
/// Two edits WERE made to the captured `activity.json`, both load-bearing, and
/// both the opposite of cosmetic:
///
/// * the inline `reflection` object on each applied transition is REMOVED.
///   Leaving it does not "get dropped by serde": activity is read through
///   [`parse_ledger_tolerating_legacy`], which re-derives the unknown-key set by
///   diffing the incoming JSON against the re-serialized parse and FAILS the
///   read on any leaf outside `LEGACY_READ_ALLOWLIST`. A retained `reflection`
///   key therefore made the whole capture `Corrupt` and both tests below died
///   before reaching the path they exist to protect.
/// * every `"Record a compact reflection before idle auto-park."` reason is
///   retargeted to the current [`IDLE_AUTO_PARK_REASON`], `"Idle auto-park."`.
///   That constant is compared by EXACT STRING in `is_routine_idle_park`, so
///   leaving the captured text would have silently reclassified every routine
///   idle park in the capture as an authoritative non-routine park — the fixture
///   would still load and still pass, while no longer describing the scenario
///   it was captured for. A regression fixture has to keep its MEANING under
///   current code, not its bytes.
///
/// What is deliberately NOT rewritten is captured prose that nothing compares:
/// several abandonment reasons still read "…cannot run to reflect." Those are
/// inert historical strings from a database the old product wrote, read by no
/// constant and asserted by no test, and blanking them would make the capture a
/// worse witness for no gain.
fn load_live_capture() -> Ledgers {
    let mut ledgers = Ledgers::empty(WallMillis(NOW_MS));
    ledgers.put_document(
        "org-manifest",
        include_str!("fixtures/live-transition-conflict/org-manifest.json"),
    );
    ledgers.put_document(
        "supervision",
        include_str!("fixtures/live-transition-conflict/supervision.json"),
    );
    ledgers
        .put_document("activity", include_str!("fixtures/live-transition-conflict/activity.json"));
    ledgers
}

fn read_inputs(
    ledgers: &Ledgers,
) -> (organization::OrganizationManifest, supervision::SupervisionLedger) {
    let manifest = organization::read(ledgers).expect("manifest");
    let supervision = supervision::read(ledgers, &manifest).expect("supervision");
    (manifest, supervision)
}

/// Harness guard: the captured documents as they stand — after the live
/// operational repair — are a healthy state. Reconcile must commit. This
/// passed on unfixed code too; it locks the fixture load path and the
/// repaired-state shape so the regression test below stays honest.
#[test]
fn the_repaired_live_capture_converges() {
    let mut ledgers = load_live_capture();
    let (manifest, supervision) = read_inputs(&ledgers);
    let input = ReconcileInput {
        launch_intent: live_fence(),
        requested_person_ids: Vec::new(),
        watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
    };
    activity::reconcile(&mut ledgers, &manifest, &supervision, &input)
        .expect("the operationally repaired capture must converge");
}

/// BUG-7 itself. Re-inject the two details the capture lost, then run the
/// exact activity-fence projection the converge cycle runs:
///
/// * LIVE-SHAPE 1 — the applied park `transition:387:execution-runner:park`
///   still carried its lifecycle intentId (the incident log: "an APPLIED park
///   transition that still carries an intentId").
/// * LIVE-SHAPE 2 — the manifest had moved execution-runner, so the pass
///   derives a structural transfer for the pinned person (the incident log:
///   the pointer "collided with the pass's structural/activity computation").
///
/// On unfixed code the pass refuses `transition-conflict` at the action/target
/// check and the whole reconcile commit rolls back. The fix evaluates the
/// applied-terminal start-fresh rule first: the structural change opens a
/// fresh transfer transition and the applied park stays untouched history.
#[test]
fn an_applied_park_pointer_never_wedges_the_reconcile_commit() {
    let mut ledgers = load_live_capture();

    // LIVE-SHAPE 1: the applied park still carries its lifecycle intentId.
    let mut activity_doc: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/live-transition-conflict/activity.json"))
            .expect("activity doc decodes");
    activity_doc["transitions"]["transition:387:execution-runner:park"]["intentId"] =
        serde_json::Value::String("lifecycle:stop:execution-runner:rev149".to_string());
    ledgers.put_document("activity", activity_doc.to_string());

    // LIVE-SHAPE 2: the manifest moved execution-runner to another desk.
    let mut manifest_doc: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/live-transition-conflict/org-manifest.json"))
            .expect("manifest decodes");
    let runner = &mut manifest_doc["people"]["execution-runner"];
    runner["departmentId"] = serde_json::Value::String("market-intelligence".to_string());
    ledgers.put_document("org-manifest", manifest_doc.to_string());

    let (manifest, supervision) = read_inputs(&ledgers);
    let input = ReconcileInput {
        launch_intent: live_fence(),
        requested_person_ids: Vec::new(),
        watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
    };
    activity::reconcile(&mut ledgers, &manifest, &supervision, &input)
        .expect("an applied-terminal pointer must start fresh, never refuse the commit");

    let ledger = activity::read(&ledgers, &manifest).expect("activity readable");
    let parked = &ledger.transitions["transition:387:execution-runner:park"];
    assert_eq!(
        parked.status,
        TransitionStatus::Applied,
        "the applied park stays untouched terminal history"
    );
    let current = ledger
        .active_transition("execution-runner")
        .expect("the structural change opened a fresh transition");
    assert_eq!(current.action, TransitionAction::Transfer);
    assert_eq!(current.to_department_id.as_deref(), Some("market-intelligence"));
    assert!(
        current.status.is_pending(),
        "the fresh transfer waits for its own release before the structural change may proceed"
    );
}
