#![allow(clippy::expect_used, clippy::panic)]

//! The Rust half of the conformance corpus, `activity` family — plan §9 M12,
//! TESTING.md §2 and §7.
//!
//! Every fixture states `setup` → `op` → `expect` → `expectState`, recorded
//! from the **TypeScript**. The runner replays each one against the Rust store
//! and compares the bounded projection and every durable read byte for byte.
//!
//! The family's centre of gravity is the launch-intent fence (inv c-1), which
//! is why the fixtures include the three that say the same thing three
//! different ways — a named list, an empty list, and an omitted field — and one
//! that says the only way to run unfenced is the explicit sentinel. In the Rust
//! port that last one is not a runtime check at all:
//! [`LaunchFence`](chiefd_core::store::activity::LaunchFence) has no permissive
//! variant a caller can reach by dropping a key, so the fixture passes because
//! the shape is unrepresentable rather than because a branch happened to be
//! right.
//!
//! # TOMBSTONE (#751-P4): the reflection budget (inv 17) is gone from this family
//!
//! This family used to have a second centre of gravity: the bounded five-field
//! reflection payload a person wrote before parking, and the aggregate
//! character budget (inv 17) that canonicalized an over-budget one so a
//! replayed write converged on identical durable bytes. The whole reflection
//! concept — the payload, its per-field and aggregate budgets, the
//! same-content replay rule, and the "an applied transition must have a durable
//! reflection" invariant — is deleted from the product. `activity.reflect` is
//! now [`activity::release`], which carries no content at all: it proves who is
//! calling, and moves the transition to
//! [`TransitionStatus::Ready`](chiefd_core::store::activity::TransitionStatus).
//!
//! One consequence shows up directly in this runner: the transition projection
//! no longer has a `reflection` key, because there is nothing to project.
//! The fixtures that pinned the *state machine* — unknown transition, another
//! person's transition, an abandoned transition, and the `require_ready` gate
//! on either side of the release — all survive, renamed.

mod conformance_common;

use chiefd_core::store::activity::{
    self, ActivityLedger, BeginTransitionInput, LaunchFence, ReconcileInput, ReleaseInput,
    TransitionAction,
};
use conformance_common::{
    assert_no_fixture_depends_on_the_model_catalog, assert_person_ids_come_from_the_template,
    caller_person, expectation, integer, load_fixtures, optional_text, run_setup, sorted,
    string_list, taxonomy, text, Expectation, World,
};
use serde_json::{json, Value};

const FAMILY: &str = "activity";

/// The sentinel the corpus uses for "run the fleet unfenced".
///
/// It is a *string*, deliberately: a fixture cannot produce it by omitting a
/// key or by passing an empty array, which is the whole content of inv c-1.
const UNFENCED_SENTINEL: &str = "UNFENCED";

// --- projections: these must match the recorded fixtures exactly -----------
// (`conformance/lib/ops.ts` held the TypeScript read registry and is deleted;
//  the fixtures ARE the contract now, and every one is compared byte for byte.)

/// The durable shape of one transition, as a fixture's `expectState` records it.
///
/// TOMBSTONE (#751-P4): this projection used to carry a `reflection` key — the
/// bounded five-field payload, or `null` while the transition was still
/// awaiting one. Both halves are gone: there is no payload to project, and
/// "not released yet" was never a separate fact from
/// `status == "awaiting_handoff"`. Everything the corpus actually depends on
/// about a release is already visible here in `status`, which is why removing
/// the key costs no coverage rather than hiding a fact.
fn transition_view(ledger: &ActivityLedger, transition_id: &str) -> Value {
    ledger.transitions.get(transition_id).map_or(Value::Null, |transition| {
        json!({
            "id": transition.id,
            "personId": transition.person_id,
            "action": transition.action.as_str(),
            "status": transition.status.as_str(),
            "abandoned": transition.abandoned_at.is_some(),
        })
    })
}

fn activity_ledger(world: &World) -> ActivityLedger {
    activity::read(&world.ledgers, world.manifest()).expect("the activity ledger is readable")
}

// --- fixture input helpers --------------------------------------------------

fn action_of(input: &Value, key: &str) -> TransitionAction {
    TransitionAction::parse(&text(input, key))
        .unwrap_or_else(|| panic!("fixture names unknown transition action '{}'", text(input, key)))
}

/// The fence, as a fixture spells it.
///
/// Three spellings, three meanings, and the type keeps them apart:
/// * absent — CEO only (an omitted field is not an off switch);
/// * `[]` — CEO only, said out loud;
/// * `"UNFENCED"` — the deliberate sentinel.
fn launch_fence(input: &Value) -> LaunchFence {
    match input.get("launchIntentPersonIds") {
        None => LaunchFence::deny_all(),
        Some(Value::String(sentinel)) => {
            assert_eq!(
                sentinel, UNFENCED_SENTINEL,
                "the only string the fence accepts is the explicit unfenced sentinel"
            );
            LaunchFence::Unfenced
        }
        Some(Value::Array(_)) => LaunchFence::fenced(string_list(input, "launchIntentPersonIds")),
        Some(other) => panic!("launchIntentPersonIds must be a list or the sentinel: {other}"),
    }
}

// --- the op registry --------------------------------------------------------

fn run_op(
    world: &mut World,
    op: &str,
    input: &Value,
    caller: Option<&Value>,
) -> Result<Value, (String, String)> {
    match op {
        "company.create" => Ok(world.create_company(&text(input, "template"))),
        "clock.advance" => {
            world.advance(integer(input, "milliseconds"));
            Ok(json!({ "now": chiefd_core::isotime::iso_millis(world.ledgers.now().0) }))
        }
        "activity.begin_transition" => {
            let manifest = world.manifest().clone();
            let supervision = world.supervision();
            let begin = BeginTransitionInput {
                person_id: text(input, "personId"),
                action: action_of(input, "action"),
                reason: text(input, "reason"),
                to_department_id: optional_text(input, "toDepartmentId"),
                intent_id: optional_text(input, "intentId"),
            };
            activity::begin_transition(&mut world.ledgers, &manifest, &supervision, &begin)
                .map(|transition| {
                    json!({
                        "id": transition.id,
                        "personId": transition.person_id,
                        "action": transition.action.as_str(),
                        "status": transition.status.as_str(),
                    })
                })
                .map_err(|error| taxonomy(&error))
        }
        // Formerly `activity.reflect`. The whole content half of the old verb
        // (summary/learning/handoff/artifacts/openCommitments, and the inv-17
        // budget arithmetic over them) is deleted with the reflection concept.
        // What survives — and what every remaining fixture in this family
        // actually pinned — is the identity check: only the named person may
        // move their own transition to `ready`. A fixture that still names
        // `activity.reflect` falls through to the panic below, which is the
        // intended loud failure rather than a silent skip.
        "activity.release" => {
            let manifest = world.manifest().clone();
            let supervision = world.supervision();
            let release = ReleaseInput {
                transition_id: text(input, "transitionId"),
                person_id: caller_person(caller),
            };
            activity::release(&mut world.ledgers, &manifest, &supervision, &release)
                .map(|transition| {
                    json!({
                        "id": transition.id,
                        "status": transition.status.as_str(),
                    })
                })
                .map_err(|error| taxonomy(&error))
        }
        "activity.abandon_transition" => {
            let manifest = world.manifest().clone();
            let supervision = world.supervision();
            activity::abandon_transition(
                &mut world.ledgers,
                &manifest,
                &supervision,
                &text(input, "transitionId"),
                &text(input, "personId"),
                &text(input, "reason"),
            )
            .map(|transition| {
                json!({
                    "id": transition.id,
                    "status": transition.status.as_str(),
                    "abandoned": transition.abandoned_at.is_some(),
                    "reason": transition.reason,
                })
            })
            .map_err(|error| taxonomy(&error))
        }
        "activity.require_ready" => {
            let manifest = world.manifest().clone();
            let supervision = world.supervision();
            activity::require_ready(
                &world.ledgers,
                &manifest,
                &supervision,
                &text(input, "personId"),
                action_of(input, "action"),
            )
            .map(|transition| {
                json!({
                    "id": transition.id,
                    "status": transition.status.as_str(),
                })
            })
            .map_err(|error| taxonomy(&error))
        }
        "activity.reconcile" => {
            let manifest = world.manifest().clone();
            let supervision = world.supervision();
            let reconcile = ReconcileInput {
                launch_intent: launch_fence(input),
                requested_person_ids: string_list(input, "requestedPersonIds"),
                // The conformance vectors predate the clamp and assert quiet
                // instants exactly; "watching for ever" is the pre-clamp rule,
                // so every existing expectation stays exact.
                watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
            };
            assert!(
                input.get("businessMonitors").is_none(),
                "no recorded fixture supplies business monitors; wire them through before \
                 a fixture starts depending on them"
            );
            activity::reconcile(&mut world.ledgers, &manifest, &supervision, &reconcile)
                .map(|snapshot| {
                    json!({
                        "people": snapshot
                            .people
                            .iter()
                            .map(|(person_id, decision)| {
                                (
                                    person_id.clone(),
                                    json!({
                                        "active": decision.active,
                                        "reasons": decision
                                            .reasons
                                            .iter()
                                            .map(|reason| reason.as_str())
                                            .collect::<Vec<_>>(),
                                        "transitionId": decision.transition_id,
                                    }),
                                )
                            })
                            .collect::<serde_json::Map<_, _>>(),
                    })
                })
                .map_err(|error| taxonomy(&error))
        }
        other => panic!(
            "fixture names op '{other}', which the Rust runner cannot execute. \
             `conformance/FORMAT.md`: a fixture the Rust runner cannot execute is a \
             missing chiefd verb, and it should be loud."
        ),
    }
}

fn run_read(world: &World, read: &str, args: &Value) -> Value {
    let ledger = activity_ledger(world);
    match read {
        "activity.person" => {
            ledger.people.get(&text(args, "personId")).map_or(Value::Null, |state| {
                json!({
                    "personId": state.person_id,
                    "lastEmploymentState": state.last_employment_state,
                    "lastDesiredActive": state.last_desired_active,
                    "lastOperational": state.last_operational,
                    "activeTransitionId": state.active_transition_id,
                    "idleSince": state.idle_since,
                })
            })
        }
        "activity.transition" => transition_view(&ledger, &text(args, "transitionId")),
        "activity.summary" => json!({
            "personOrder": ledger.person_order,
            "transitionOrder": ledger.transition_order,
            "nextTransitionSequence": ledger.next_transition_sequence,
        }),
        other => panic!("fixture names read '{other}', which the Rust runner cannot execute"),
    }
}

#[test]
fn every_activity_fixture_replays_against_the_rust_store() {
    let fixtures = load_fixtures(FAMILY);
    for (name, fixture) in &fixtures {
        let mut world = World::new();
        run_setup(&mut world, name, fixture, run_op);

        let op = fixture["op"].as_str().expect("an op");
        let input = fixture.get("in").cloned().unwrap_or_else(|| json!({}));
        let observed = run_op(&mut world, op, &input, fixture.get("caller"));

        match expectation(fixture) {
            Expectation::Ok(recorded) => {
                let value = observed.unwrap_or_else(|(kind, code)| {
                    panic!("{name}: expected ok, got {kind}/{code}")
                });
                assert_eq!(sorted(&value), recorded, "{name}: response projection differs");
            }
            Expectation::Error { kind, code } => match observed {
                Ok(value) => panic!("{name}: expected {kind}/{code}, got ok: {value}"),
                Err((observed_kind, observed_code)) => assert_eq!(
                    (observed_kind.as_str(), observed_code.as_str()),
                    (kind.as_str(), code.as_str()),
                    "{name}: taxonomy differs"
                ),
            },
        }

        let expect_state = fixture["expectState"].as_array().expect("`expectState` is an array");
        assert!(!expect_state.is_empty(), "{name}: a fixture must assert durable state");
        for expectation in expect_state {
            let read = expectation["read"].as_str().expect("a read name");
            let args = expectation.get("args").cloned().unwrap_or_else(|| json!({}));
            let observed = run_read(&world, read, &args);
            assert_eq!(
                sorted(&observed),
                sorted(&expectation["equals"]),
                "{name}: durable read '{read}' differs"
            );
        }
    }
    println!("{}/{} activity fixtures replayed", fixtures.len(), fixtures.len());
}

#[test]
fn the_corpus_still_pins_the_invariants_this_milestone_owes() {
    let names: Vec<String> = load_fixtures(FAMILY).into_iter().map(|(name, _)| name).collect();
    for required in [
        "inv20-seed-state-is-not-desired-active",
        "inv-c1-unfenced-requires-the-explicit-sentinel",
        "fence-omitted-is-chief-only-not-unfenced",
        // #751-P4: `inv17-oversized-replay-produces-identical-durable-bytes`
        // stood here and is deleted with the reflection budget it pinned — an
        // aggregate character bound over a payload that no longer exists
        // cannot be load-bearing. The two names below replace it, and they
        // pin the thing that actually gates a structural change now: a
        // transition is `ready` only because its owner released it, and
        // `require_ready` refuses until that has happened.
        "release-marks-the-transition-ready",
        "require-ready-without-a-release-is-refused",
    ] {
        assert!(
            names.iter().any(|name| name == required),
            "the corpus lost '{required}', which is one of this family's load-bearing fixtures"
        );
    }
    // Nothing that pins the surviving transition state machine may be
    // dropped, so this floor may only ever go up from here.
    assert!(names.len() >= 17, "the activity family lost fixtures: {}", names.len());
}

#[test]
fn the_activity_family_declares_no_plan_divergences() {
    for (name, _) in load_fixtures(FAMILY) {
        assert!(
            !name.starts_with("PLAN-DELTA-"),
            "{name} is a declared divergence, but this runner asserts exact equality"
        );
    }
}

#[test]
fn no_activity_fixture_depends_on_the_model_catalog() {
    assert_no_fixture_depends_on_the_model_catalog(FAMILY);
}

#[test]
fn the_northstar_template_matches_what_the_fixtures_were_recorded_against() {
    assert_person_ids_come_from_the_template(FAMILY);
}
