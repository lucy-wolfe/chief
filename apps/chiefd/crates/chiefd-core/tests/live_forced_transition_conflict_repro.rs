// Live-state regression for the production `transition-conflict` refusal that
// came BACK (a live company, 2026-08-13), three weeks after the same wedge was
// closed for `Applied` (tribes-capital, 2026-07-22 — BUG-7 in
// `runtime/takeover-bug-log.md`, pinned by the sibling
// `live_transition_conflict_repro.rs`).
//
// The 2026-07-22 fix enumerated ONE status. It made an `Applied` pointer start
// fresh, and left `Forced` — the terminal status a routine idle auto-park is
// BORN with (#337) — fencing the person forever. Live symptom, every reconcile
// cycle, unbroken:
//
//     warn "reconcile actuation failed (ledger cycle already committed)"
//       error: "refused: transition-conflict: Person 'chief-of-staff' already
//               has park transition 'transition:1:chief-of-staff:park'"
//
// and all seven idle auto-parks sat at `status='forced'` with `applied_at`,
// `cancelled_at` and `abandoned_at` all null — no retirement path, ever.
//
// The rule this file pins is TERMINALITY, not a list of statuses: no terminal
// transition may fence a later one. Adding a status to `is_terminal()` must
// never again require remembering to add it here too.
#![allow(clippy::expect_used, clippy::panic, missing_docs)]

use chiefd_core::clock::WallMillis;
use chiefd_core::ledger::Ledgers;
use chiefd_core::store::activity::{
    LaunchFence, ReconcileInput, TransitionAction, TransitionStatus,
};
use chiefd_core::store::{activity, organization, supervision};

const NOW_MS: i64 = 1_784_693_100_000; // Same instant as the sibling capture.

/// The pinned person in the shared capture: their `activeTransitionId` names a
/// routine idle auto-park, which is exactly the live shape.
const PINNED: &str = "execution-runner";
const PINNED_PARK: &str = "transition:387:execution-runner:park";

/// The launch-intent fence the converge cycle computed, CEO excluded.
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

/// Load the shared live capture with the pinned person's park rewritten into
/// the 2026-08-13 shape — `forced`, never applied, never cancelled, never
/// abandoned — and the manifest moved so the pass derives a structural
/// transfer for that same person.
fn load_forced_park_capture() -> Ledgers {
    let mut ledgers = Ledgers::empty(WallMillis(NOW_MS));
    ledgers.put_document(
        "supervision",
        include_str!("fixtures/live-transition-conflict/supervision.json"),
    );

    // LIVE-SHAPE 1: the park is `forced`, the terminal status #337 gives a
    // routine idle auto-park that nobody ever released. `appliedAt` is
    // REMOVED: the live rows carried `applied_at=None` and it is the absence
    // of an applied stamp that made the 2026-07-22 predicate miss them.
    let mut activity_doc: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/live-transition-conflict/activity.json"))
            .expect("activity doc decodes");
    let park = &mut activity_doc["transitions"][PINNED_PARK];
    park["status"] = serde_json::Value::String("forced".to_owned());
    let requested_at = park["requestedAt"].clone();
    park["forcedAt"] = requested_at;
    park.as_object_mut().expect("transition is an object").remove("appliedAt");
    ledgers.put_document("activity", activity_doc.to_string());

    // LIVE-SHAPE 2: the manifest moved the pinned person, so the pass derives
    // a structural transfer against the person the forced park pins.
    let mut manifest_doc: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/live-transition-conflict/org-manifest.json"))
            .expect("manifest decodes");
    manifest_doc["people"][PINNED]["departmentId"] =
        serde_json::Value::String("market-intelligence".to_owned());
    ledgers.put_document("org-manifest", manifest_doc.to_string());

    ledgers
}

fn read_inputs(
    ledgers: &Ledgers,
) -> (organization::OrganizationManifest, supervision::SupervisionLedger) {
    let manifest = organization::read(ledgers).expect("manifest");
    let supervision = supervision::read(ledgers, &manifest).expect("supervision");
    (manifest, supervision)
}

fn reconcile_input() -> ReconcileInput {
    ReconcileInput {
        launch_intent: live_fence(),
        requested_person_ids: Vec::new(),
        watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
    }
}

/// Harness guard: the rewritten capture really does carry a FORCED park that
/// is the pinned person's active-transition pointer. If this ever stops being
/// true the regression below would pass for the wrong reason.
#[test]
fn the_capture_pins_the_person_against_a_forced_park() {
    let ledgers = load_forced_park_capture();
    let (manifest, _supervision) = read_inputs(&ledgers);
    let ledger = activity::read(&ledgers, &manifest).expect("activity readable");
    let park = &ledger.transitions[PINNED_PARK];
    assert_eq!(park.status, TransitionStatus::Forced);
    assert!(park.status.is_terminal(), "a forced park is terminal history");
    assert_eq!(park.applied_at, None);
    assert_eq!(park.cancelled_at, None);
    assert_eq!(park.abandoned_at, None);
    assert_eq!(park.intent_id, None, "a forced park is never intent-bound (#337)");
    assert_eq!(
        ledger.active_transition(PINNED).map(|current| current.id.clone()).as_deref(),
        Some(PINNED_PARK),
        "the forced park is still the person's active-transition pointer"
    );
}

/// The 2026-08-13 wedge itself.
///
/// On the 2026-07-22 code the start-fresh predicate asked `status == Applied`,
/// so a `Forced` pointer fell through to the action/target refusal and every
/// reconcile actuation aborted with `transition-conflict`. Terminality — not
/// an enumerated status — is the rule: the structural change must open a fresh
/// transfer and leave the forced park untouched history.
#[test]
fn a_forced_park_pointer_never_wedges_the_reconcile_commit() {
    let mut ledgers = load_forced_park_capture();
    let (manifest, supervision) = read_inputs(&ledgers);
    activity::reconcile(&mut ledgers, &manifest, &supervision, &reconcile_input())
        .expect("a forced-terminal pointer must start fresh, never refuse the commit");

    let ledger = activity::read(&ledgers, &manifest).expect("activity readable");
    let park = &ledger.transitions[PINNED_PARK];
    assert_eq!(
        park.status,
        TransitionStatus::Forced,
        "the forced park stays untouched terminal history"
    );
    assert_eq!(park.applied_at, None, "starting fresh never back-dates a forced park as applied");
    let current =
        ledger.active_transition(PINNED).expect("the structural change opened a fresh transition");
    assert_eq!(current.action, TransitionAction::Transfer);
    assert_eq!(current.to_department_id.as_deref(), Some("market-intelligence"));
    assert_ne!(current.id, PINNED_PARK, "the fresh transition is a new record, not the park");
    assert!(
        current.status.is_pending(),
        "the fresh transfer waits for its own release before the structural change may proceed"
    );
}

/// The wedge is not specific to a transfer: a later PARK against the same
/// person must also be admitted. This is the plain repeat of the live cycle —
/// the idle sweep re-derives a park every pass — and it is the arm that proves
/// the pointer is retired rather than merely bypassed by a different action.
#[test]
fn a_forced_park_does_not_fence_a_later_park_for_the_same_person() {
    let mut ledgers = load_forced_park_capture();
    // Undo LIVE-SHAPE 2: leave the person where they are, so the only thing
    // the pass can derive for them is another park.
    let manifest_doc: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/live-transition-conflict/org-manifest.json"))
            .expect("manifest decodes");
    ledgers.put_document("org-manifest", manifest_doc.to_string());

    let (manifest, supervision) = read_inputs(&ledgers);
    activity::begin_transition(
        &mut ledgers,
        &manifest,
        &supervision,
        &activity::BeginTransitionInput {
            person_id: PINNED.to_owned(),
            action: TransitionAction::Park,
            to_department_id: None,
            reason: "Idle auto-park.".to_owned(),
            intent_id: None,
        },
    )
    .expect("a forced park is terminal history and must not fence a later park");

    let ledger = activity::read(&ledgers, &manifest).expect("activity readable");
    let current = ledger.active_transition(PINNED).expect("a fresh park opened");
    assert_ne!(current.id, PINNED_PARK, "the terminal forced park was not re-used");
    assert_eq!(current.action, TransitionAction::Park);
    // A routine idle park is BORN `Forced` (#337), so the fresh record is
    // terminal from the first instant — which is precisely why the predicate
    // has to ask about terminality: the very next idle sweep meets it again.
    assert_eq!(current.status, TransitionStatus::Forced);
    assert!(current.forced_at.is_some(), "a forced park records when it was forced");
    assert_eq!(
        ledger.transitions[PINNED_PARK].status,
        TransitionStatus::Forced,
        "the earlier forced park stays untouched history"
    );
}
