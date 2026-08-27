//! Placement, proven three ways.
//!
//! 1. The four placement tests from
//!    `chiefd-core/src/runtime/reconcile_plan/tests.rs`, ported. Same
//!    fixture, same assertions — read against the roster wire instead of the
//!    planner's internal `Manifest`, because that is the whole change.
//! 2. The shared golden `apps/chiefd/tests/fixtures/placement-golden.json`,
//!    which chiefd computes from its own manifest and this crate computes
//!    from the published roster. Both sides assert byte-identity against the
//!    same file's text.
//! 3. The two facts a hand-built fixture will otherwise get wrong: window
//!    order comes from the person's `displayOrder` FIELD and never from the
//!    array position, and `desiredActive` is consumed, never re-derived.
//!
//! Every assertion here is read against ONE WINDOW PER PERSON. A window used
//! to be a department holding a tiled grid of its people, which is what made a
//! person's pane width a function of how crowded their department was — and
//! made showing them alone a MOVE, and therefore a resize. The tests that
//! locked that model down are rewritten rather than adjusted: their claims
//! ("the head of Quant is in Quant's window", "focusing moves exactly one pane")
//! are not true in a weaker form, they are void.

use super::{
    desired_topology, pane_department_id, person_window_id, person_window_person_id,
    safe_window_name, MAX_WINDOW_NAME_CHARS,
};
use crate::roster::{Roster, RosterCompany, RosterDepartment, RosterError, RosterPerson};

// --- builders --------------------------------------------------------------

/// `(id, name, parent, head, state)` — one department, at the given ordinal.
fn dep(
    order: usize,
    id: &str,
    name: &str,
    parent: Option<&str>,
    head: &str,
    state: &str,
) -> RosterDepartment {
    RosterDepartment {
        id: id.to_owned(),
        name: name.to_owned(),
        parent_department_id: parent.map(ToOwned::to_owned),
        head_person_id: head.to_owned(),
        order,
        state: state.to_owned(),
    }
}

/// One person, at the given ordinal.
fn per(
    display_order: usize,
    id: &str,
    department: &str,
    is_head_of: Option<&str>,
    desired_active: bool,
) -> RosterPerson {
    RosterPerson {
        id: id.to_owned(),
        display_name: id.to_owned(),
        title: id.to_owned(),
        department_id: department.to_owned(),
        is_head_of: is_head_of.map(ToOwned::to_owned),
        display_order,
        desired_active,
        employment_state: "active".to_owned(),
    }
}

/// The `cobalt` fixture from `reconcile_plan/tests.rs`, as the roster wire.
///
/// `undesired` names the people chiefd decided must NOT run. In the planner's
/// version those were a Benched and a Departed person; here they arrive as
/// `desiredActive: false`, because the client consumes that decision rather
/// than re-reading employment state. Everything else — the ids, the tree, the
/// two orders — is the same fixture.
fn cobalt(undesired: &[&str]) -> Roster {
    let desired = |id: &str| !undesired.contains(&id);
    Roster {
        company: RosterCompany { slug: "cobalt".to_owned(), display_name: "Cobalt".to_owned() },
        root_department_id: "executive".to_owned(),
        departments: vec![
            dep(0, "executive", "Executive", None, "chief", "active"),
            dep(1, "quant", "Quant", Some("executive"), "quant-head", "active"),
            dep(2, "quant-data", "Data", Some("quant"), "quant-data-head", "active"),
            dep(3, "it", "IT", Some("executive"), "it-head", "active"),
        ],
        people: vec![
            per(0, "chief", "executive", Some("executive"), desired("chief")),
            per(1, "quant-head", "quant", Some("quant"), desired("quant-head")),
            per(2, "quant-active-quant", "quant", None, desired("quant-active-quant")),
            per(3, "quant-benched-quant", "quant", None, desired("quant-benched-quant")),
            per(4, "quant-data-head", "quant-data", Some("quant-data"), desired("quant-data-head")),
            per(
                5,
                "quant-data-active-data-engineer",
                "quant-data",
                None,
                desired("quant-data-active-data-engineer"),
            ),
            per(
                6,
                "quant-data-departed-data-engineer",
                "quant-data",
                None,
                desired("quant-data-departed-data-engineer"),
            ),
            per(7, "it-head", "it", Some("it"), desired("it-head")),
        ],
    }
}

/// The steady-state `cobalt(true)` roster: the benched quant and the departed
/// data engineer are the two people chiefd does not want running.
fn cobalt_steady() -> Roster {
    cobalt(&["quant-benched-quant", "quant-data-departed-data-engineer"])
}

/// The window ids of a topology, in order.
fn window_ids(topology: &super::Topology) -> Vec<String> {
    topology.windows.iter().map(|window| window.logical_id.clone()).collect()
}

/// The people this topology places, in window order.
fn placed(topology: &super::Topology) -> Vec<String> {
    topology
        .windows
        .iter()
        .flat_map(|window| window.panes.iter().map(|pane| pane.person_id.clone()))
        .collect()
}

/// The people placed in one window, in order.
fn window_persons(topology: &super::Topology, logical: &str) -> Vec<String> {
    topology
        .windows
        .iter()
        .find(|window| window.logical_id == logical)
        .map(|window| window.panes.iter().map(|pane| pane.person_id.clone()).collect())
        .unwrap_or_default()
}

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// The session every fixture topology is drawn into.
///
/// `desired_topology` takes the composed NAME rather than deriving one: its two
/// hottest callers — the actuator's converge pass and the brain's click path —
/// already hold the session they are drawing into, so re-deriving it there
/// would hash a directory on every click to reproduce a string the caller was
/// looking at. The fixtures state it for the same reason they state a roster.
const FIXTURE_SESSION: &str = "org-cobalt-012345_";

fn planned(roster: &Roster) -> super::Topology {
    desired_topology(roster, &fixture_hashes(roster), FIXTURE_SESSION)
        .expect("the fixture roster must plan")
}

/// The desired set chiefd would publish for this fixture.
///
/// Membership now comes from HERE and not from the roster's `desiredActive`:
/// the desired set is the single authority on who should be up, and the roster
/// is read for structure. One stable hash per person; a drift test moves one
/// directly.
fn fixture_hashes(roster: &Roster) -> std::collections::BTreeMap<String, String> {
    roster
        .people
        .iter()
        .filter(|person| person.desired_active)
        .map(|person| (person.id.clone(), format!("hash-{}", person.id)))
        .collect()
}

// --- 1. the four ported placement tests ------------------------------------

#[test]
fn every_desired_person_gets_a_window_of_their_own() {
    // Re-aimed from `desired_topology_places_active_people_by_disk_model`,
    // which asserted the DEPARTMENT windows `["executive", "quant",
    // "quant-data", "it"]` and the tiled grid of people inside each. That model
    // is gone: a person's pane width used to be decided by how crowded their
    // department was, and showing them alone therefore meant MOVING them, which
    // is a resize. One window per person is the only shape in which a pane has
    // one size for its whole life.
    let d = planned(&cobalt_steady());
    assert_eq!(
        window_ids(&d),
        ids(&[
            "__person__:chief",
            "__person__:quant-head",
            "__person__:quant-active-quant",
            "__person__:quant-data-head",
            "__person__:quant-data-active-data-engineer",
            "__person__:it-head",
        ]),
        "one window per desired person, in the company's canonical person order"
    );
    for window in &d.windows {
        assert_eq!(window.panes.len(), 1, "no window may hold two people: {window:?}");
        assert_eq!(
            window.logical_id,
            person_window_id(&window.panes[0].person_id),
            "a window's id names the one person in it"
        );
    }
    // The benched and departed engineers are excluded; nobody gets an empty
    // window, because a window is a person and an undesired person is not one.
    assert_eq!(d.windows.len(), 6);
    assert!(!placed(&d).contains(&"quant-benched-quant".to_owned()));
    assert!(!placed(&d).contains(&"quant-data-departed-data-engineer".to_owned()));
}

/// A DEPARTMENT IS NOT A WINDOW ANY MORE — not an empty one, not any one.
///
/// The rule this replaced was "an empty department gets no window", which
/// implied a non-empty one did. Nothing places by department now, so the only
/// department-shaped windows a tmux server can hold are the rail's own card
/// windows (`overview_window_id`), which placement has never heard of.
#[test]
fn no_department_id_appears_as_a_window() {
    let roster = cobalt_steady();
    let d = planned(&roster);
    for unit in &roster.departments {
        assert!(
            !window_ids(&d).contains(&unit.id),
            "department '{}' must not be a window: {:?}",
            unit.id,
            window_ids(&d)
        );
    }
}

#[test]
fn a_handoff_required_decision_beats_roster_state_in_both_directions() {
    // The planner's version of this test flips `decision.active` and a
    // `handoff-required` reason and watches `is_desired_person` honour it.
    // That predicate is chiefd's and STAYS chiefd's: on the wire the override
    // has already been applied, and it arrives as `desiredActive`. So the
    // client-side property is the one that matters here — the decision is
    // obeyed in both directions, and nothing in this crate second-guesses it
    // from employment state, which it cannot even see.

    // Benched, but carrying a handoff lease: chiefd says desired, so placed.
    let keep = cobalt(&["quant-data-departed-data-engineer"]);
    let d = planned(&keep);
    assert_eq!(
        window_persons(&d, &person_window_id("quant-benched-quant")),
        ids(&["quant-benched-quant"])
    );

    // Roster-active, but a handoff decision says inactive: dropped.
    let drop =
        cobalt(&["quant-active-quant", "quant-benched-quant", "quant-data-departed-data-engineer"]);
    let d = planned(&drop);
    assert!(!placed(&d).contains(&"quant-active-quant".to_owned()));
    assert!(!window_ids(&d).contains(&person_window_id("quant-active-quant")));
}

#[test]
fn a_paused_department_subtree_contributes_no_windows() {
    // Ported. Pausing quant removes quant and its child quant-data, heads
    // included. The subtree WALK is chiefd's — it arrives as `desiredActive` —
    // and the consequence for the display is this crate's: a person chiefd does
    // not want gets no window, so a paused subtree leaves the windows of the
    // people who are still up and nothing else.
    let mut roster = cobalt(&[
        "quant-head",
        "quant-active-quant",
        "quant-benched-quant",
        "quant-data-head",
        "quant-data-active-data-engineer",
        "quant-data-departed-data-engineer",
    ]);
    for unit in &mut roster.departments {
        if unit.id == "quant" || unit.id == "quant-data" {
            unit.state = "paused".to_owned();
        }
    }
    let d = planned(&roster);
    assert_eq!(window_ids(&d), ids(&["__person__:chief", "__person__:it-head"]));
    assert_eq!(placed(&d), ids(&["chief", "it-head"]));
}

#[test]
fn desired_topology_rejects_a_roster_that_does_not_hold_together() {
    // The planner refuses an activity snapshot that does not match or cover
    // its manifest. The client has no activity snapshot — it has the roster,
    // and the same fail-closed property applies to it: a topology computed
    // from a roster that does not hold together silently omits people, and an
    // actuator reads an omission as "stop them".

    let mut unknown_department = cobalt_steady();
    unknown_department.people[2].department_id = "marketing".to_owned();
    assert!(matches!(
        desired_topology(&unknown_department, &fixture_hashes(&unknown_department), FIXTURE_SESSION),
        Err(RosterError::UnknownDepartment { department, .. }) if department == "marketing"
    ));

    let mut unknown_headship = cobalt_steady();
    unknown_headship.people[0].is_head_of = Some("marketing".to_owned());
    assert!(matches!(
        desired_topology(&unknown_headship, &fixture_hashes(&unknown_headship), FIXTURE_SESSION),
        Err(RosterError::UnknownDepartment { department, .. }) if department == "marketing"
    ));

    let mut rootless = cobalt_steady();
    rootless.root_department_id = "quant".to_owned();
    assert!(
        matches!(desired_topology(&rootless, &fixture_hashes(&rootless), FIXTURE_SESSION), Err(RosterError::RootInvalid(id)) if id == "quant")
    );

    let mut duplicated = cobalt_steady();
    duplicated.people.push(duplicated.people[0].clone());
    assert!(matches!(
        desired_topology(&duplicated, &fixture_hashes(&duplicated), FIXTURE_SESSION),
        Err(RosterError::DuplicateId { kind: "person", id }) if id == "chief"
    ));

    let mut duplicate_order = cobalt_steady();
    duplicate_order.departments[3].order = 0;
    assert!(matches!(
        desired_topology(&duplicate_order, &fixture_hashes(&duplicate_order), FIXTURE_SESSION),
        Err(RosterError::DuplicateOrder { kind: "department", order: 0 })
    ));
}

// --- 2. everybody in their own department, heads included ------------------

/// MOVED from `a_head_sits_in_the_parents_window_and_a_top_level_head_sits_at_
/// the_root`, which claimed the retired HEAD-IN-PARENT rule: `quant-head` in
/// `executive`, `quant-data-head` in `quant`, `it-head` in `executive`.
///
/// It claims the opposite now, at both depths — a TOP-LEVEL head and a NESTED
/// one — because those two cases are the ones a careless reading conflates:
/// "a top-level head sits at the root" happened to look like the new rule for
/// the head of the root department alone.
///
/// Why the old rule lost is in [`super::pane_department_id`]: the display was
/// the only surface placing a head outside the unit chiefd's own record puts
/// them in, and the operator met it as "clicking Engineering does not show the
/// person who heads Engineering".
#[test]
fn every_person_including_a_head_sits_in_their_own_department() {
    let roster = cobalt_steady();
    let placed = |id: &str| {
        let person = roster.people.iter().find(|person| person.id == id).expect("the person");
        pane_department_id(&roster, person).expect("the fixture places")
    };

    assert_eq!(placed("chief"), "executive", "the root's own head heads, and lives in, the root");
    assert_eq!(placed("quant-head"), "quant", "a TOP-LEVEL head sits in the unit they head");
    assert_eq!(placed("quant-data-head"), "quant-data", "and so does a NESTED head");
    assert_eq!(placed("it-head"), "it");
    assert_eq!(placed("quant-active-quant"), "quant", "a non-head sits in their own department");
}

/// MOVED from `placement_follows_a_reparent_the_stored_column_would_not_have_
/// seen`, which reparented a DEPARTMENT and watched its head follow the new
/// parent, and then from a version that watched a moved person appear in the
/// destination WINDOW. Neither gesture survives one window per person: nothing
/// on tmux is department-shaped any more, so a move changes no window at all.
///
/// The claim outlives both gestures, which is why the test does. A client
/// DERIVES the department answer from the roster it is holding, so a change is
/// visible on the next pass with no persisted column to go stale — and this is
/// the fact `person_activity.last_pane_department_id` used to lag behind
/// (#751-P9). The rail reads it to group its rows and to answer "is everybody
/// in Quant asleep"; placement reads it only to fail a bad roster closed.
#[test]
fn the_department_answer_follows_a_move_the_stored_column_would_not_have_seen() {
    let mut roster = cobalt_steady();
    let department_of = |roster: &Roster, id: &str| {
        let person = roster.people.iter().find(|person| person.id == id).expect("the person");
        pane_department_id(roster, person).expect("the fixture places")
    };
    assert_eq!(department_of(&roster, "quant-active-quant"), "quant");

    roster
        .people
        .iter_mut()
        .find(|person| person.id == "quant-active-quant")
        .expect("the quant")
        .department_id = "it".to_owned();

    assert_eq!(
        department_of(&roster, "quant-active-quant"),
        "it",
        "a moved person reads as a member of their destination immediately"
    );
    // And their WINDOW did not move, because a window is a person and not a
    // department. This is the whole product consequence: an operator watching
    // somebody who gets reorganised keeps watching the same pane, at the same
    // width, with the same scrollback.
    assert_eq!(
        window_ids(&planned(&roster)),
        window_ids(&planned(&cobalt_steady())),
        "a reorganisation moves nobody's window"
    );
}

/// THE INVARIANT THE OPERATOR ASKED FOR, at the placement layer.
///
/// `a_departments_window_holds_its_own_head_beside_its_team` stood here and
/// asserted `["quant-head", "quant-active-quant"]` in one window. It was a
/// correct statement about a model whose cost was the operator's own report:
/// *"when I click on an agent I want it should be in the final position,
/// right? Why is it going half screen and growing?"* — because a pane sharing
/// a window is narrower than a pane alone, and a click moved it from one to the
/// other. Two people in one window is now the defect, not the design.
#[test]
fn no_window_ever_holds_two_people() {
    let rosters = [
        cobalt_steady(),
        cobalt(&[]),
        cobalt(&["chief"]),
        cobalt(&["quant-head", "quant-benched-quant", "quant-data-departed-data-engineer"]),
    ];
    for roster in rosters {
        let d = planned(&roster);
        for window in &d.windows {
            assert_eq!(window.panes.len(), 1, "a window holds one person: {window:?}");
        }
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for window in &d.windows {
            assert!(
                seen.insert(window.logical_id.as_str()),
                "two windows claimed '{}'",
                window.logical_id
            );
        }
    }
}

/// A PERSON WINDOW ID ROUND-TRIPS, and cannot be mistaken for anything else.
///
/// Converge asks `person_window_person_id` to tell a spent person's window —
/// which is killed WHOLE the moment its person stops — from a department or
/// card window, which is not. A prefix that could collide with either would
/// make that reap destroy the rail's own furniture.
#[test]
fn a_person_window_id_names_its_person_and_nothing_else_does() {
    let roster = cobalt_steady();
    for person in &roster.people {
        let id = person_window_id(&person.id);
        assert_eq!(person_window_person_id(&id), Some(person.id.as_str()));
    }
    for unit in &roster.departments {
        assert_eq!(person_window_person_id(&unit.id), None, "a department id is a slug");
        assert_eq!(person_window_person_id(&super::overview_window_id(&unit.id)), None);
    }
    assert_eq!(person_window_person_id(super::FOCUS_WINDOW_ID), None);
    // A slug cannot contain a colon, which is what makes the prefix safe
    // without a validator refusing anything.
    assert!(person_window_id("x").contains(':'));
    assert!(roster.departments.iter().all(|unit| !unit.id.contains(':')));
}

// --- 3. the two facts a hand-built fixture gets wrong ----------------------

#[test]
fn window_order_comes_from_display_order_and_never_from_the_array_position() {
    // It used to be the DEPARTMENT `order` field, whose store walk is
    // depth-first rather than insertion order — a client that sorted by array
    // position agreed with the API by accident on a flat company and disagreed
    // the moment one was nested. The same trap is here one level down: the
    // operator's chosen person order is the `displayOrder` FIELD, and it now
    // orders the WINDOW LIST itself, which is what the operator reads along the
    // bottom of their terminal.
    let mut shuffled = cobalt_steady();
    shuffled.departments.reverse();
    shuffled.people.reverse();

    assert_eq!(
        window_ids(&planned(&shuffled)),
        ids(&[
            "__person__:chief",
            "__person__:quant-head",
            "__person__:quant-active-quant",
            "__person__:quant-data-head",
            "__person__:quant-data-active-data-engineer",
            "__person__:it-head",
        ]),
        "window order is displayOrder, not array position"
    );
    assert_eq!(planned(&shuffled), planned(&cobalt_steady()), "and the whole plan is identical");
}

#[test]
fn the_session_is_the_company_and_every_member_is_known_desired_or_not() {
    let topology = planned(&cobalt_steady());
    assert_eq!(topology.session, FIXTURE_SESSION);
    assert_eq!(topology.organization, "cobalt");
    // Membership, not the running set: it is what tells this company's OWN
    // departed person's leaked process from a stranger's.
    assert!(topology.known_person_ids.contains("quant-data-departed-data-engineer"));
    assert_eq!(topology.known_person_ids.len(), 8);
}

// --- 4. window names -------------------------------------------------------

#[test]
fn a_window_is_named_for_the_person_it_shows() {
    // `the_root_window_is_named_after_the_company_rather_than_the_root_
    // department` stood here: the root department's NAME is the company's, so
    // using `department.name` for every window printed the company across the
    // first tab. A window is a person now, so it carries the PERSON's display
    // name and the operator reads their agents' names in the tmux window list
    // instead of four copies of the org chart.
    let mut roster = cobalt_steady();
    roster.people.iter_mut().find(|person| person.id == "chief").expect("the CEO").display_name =
        "Avery Stone.Jr".to_owned();
    let topology = planned(&roster);
    let first = topology.windows.first().expect("the first window");

    assert_eq!(first.logical_id, person_window_id("chief"));
    assert_eq!(first.name, "Avery Stone.Jr", "the raw fact chiefd published");
    assert_eq!(first.window_name(), "Avery Stone-Jr", "sanitized for the terminal");
    // And no window is named for a department any more.
    let names: Vec<&str> = topology.windows.iter().map(|window| window.name.as_str()).collect();
    for unit in &roster.departments {
        assert!(!names.contains(&unit.name.as_str()), "'{}' is a department name", unit.name);
    }
}

// The window-name tests below are `chiefd-host/src/tmux/mod.rs`'s own,
// carried over unchanged. This canonicalization is a SHARED contract between
// every actuator that names the same windows: two actuators that disagree
// create a second window and split a company's panes across both.

#[test]
fn the_live_outage_name_becomes_legal() {
    // "Leo Capital Inc." crash-looped a real company.
    assert_eq!(safe_window_name("Leo Capital Inc."), "Leo Capital Inc");
}

#[test]
fn both_target_separators_are_replaced_anywhere_in_the_name() {
    assert_eq!(safe_window_name("Trading: Alpha"), "Trading- Alpha");
    assert_eq!(safe_window_name("a.b.c"), "a-b-c");
    assert_eq!(safe_window_name(":lead:"), "lead");
}

#[test]
fn characters_the_terminal_actually_accepts_are_left_alone() {
    assert_eq!(safe_window_name("R&D / Growth #1 (100%)"), "R&D / Growth #1 (100%)");
    assert_eq!(safe_window_name("under_score-dash"), "under_score-dash");
}

#[test]
fn truncation_cannot_leave_a_trailing_separator() {
    let name = format!("{}.", "x".repeat(MAX_WINDOW_NAME_CHARS - 1));
    let safe = safe_window_name(&name);
    assert_eq!(safe.chars().count(), MAX_WINDOW_NAME_CHARS - 1);
    assert!(!safe.ends_with('-'), "a cut must not leave a dangling separator: {safe}");
}

#[test]
fn a_long_name_is_bounded_and_multibyte_safe() {
    let safe = safe_window_name(&"é".repeat(100));
    assert_eq!(safe.chars().count(), MAX_WINDOW_NAME_CHARS);
}

#[test]
fn a_name_of_only_forbidden_characters_still_yields_something_launchable() {
    assert_eq!(safe_window_name("..."), "window");
    assert_eq!(safe_window_name("   "), "window");
    assert_eq!(safe_window_name(""), "window");
}

#[test]
fn the_rendered_topology_prints_the_sanitized_name_a_terminal_would_be_told() {
    // The subject is the same — `render` prints the name tmux is TOLD, so a
    // line can be diffed by eye against `tmux list-windows` — and the name is a
    // PERSON's now, so the forbidden character goes on a person.
    let mut roster = cobalt_steady();
    roster
        .people
        .iter_mut()
        .find(|person| person.id == "quant-head")
        .expect("the head of quant")
        .display_name = "Trading: Alpha".to_owned();

    assert_eq!(
        super::render(&planned(&roster)),
        vec![
            format!("session {FIXTURE_SESSION}"),
            "window __person__:chief \"chief\" panes=chief@hash-chief".to_owned(),
            "window __person__:quant-head \"Trading- Alpha\" panes=quant-head@hash-quant-h"
                .to_owned(),
            "window __person__:quant-active-quant \"quant-active-quant\" \
             panes=quant-active-quant@hash-quant-a"
                .to_owned(),
            "window __person__:quant-data-head \"quant-data-head\" \
             panes=quant-data-head@hash-quant-d"
                .to_owned(),
            "window __person__:quant-data-active-data-engineer \
             \"quant-data-active-data-engineer\" \
             panes=quant-data-active-data-engineer@hash-quant-d"
                .to_owned(),
            "window __person__:it-head \"it-head\" panes=it-head@hash-it-head".to_owned(),
        ],
        "the rendered name is the one tmux is told, not the raw fact"
    );
}

// --- 5. the shared golden --------------------------------------------------

/// The fixture BOTH implementations are asserted against, byte for byte.
///
/// chiefd computes its half from its own manifest
/// (`chiefd-core/src/runtime/roster/tests.rs`,
/// `chiefd-core/src/runtime/reconcile_plan/tests.rs`,
/// `chiefd-host/src/tmux/mod.rs`); this crate computes its half from the
/// published roster in the same file. Neither side links the other, so a
/// shared file is what makes "the same answer" checkable at all — and it
/// survives the switchover as the record of what the answer WAS.
const GOLDEN: &str = include_str!("../../../../tests/fixtures/placement-golden.json");

/// The golden, parsed.
fn golden() -> serde_json::Value {
    serde_json::from_str(GOLDEN).expect("the shared golden must be readable JSON")
}

/// Canonical text for a value. `serde_json::Value` maps are `BTreeMap`s, so
/// equal values pretty-print to identical bytes — which is what lets an
/// assertion on the TEXT mean byte-identity rather than merely "same data".
fn canonical(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).expect("a value must serialize")
}

#[test]
fn the_golden_roster_decodes_into_this_clients_own_wire_types() {
    // If a field placement needs were missing, or renamed, this fails to
    // decode — the wire types are a SECOND declaration of chiefd's shape and
    // this is what holds them to the first.
    // Decoded from TEXT, which is what the wire actually delivers.
    let roster =
        Roster::from_json(&canonical(&golden()["roster"])).expect("the golden roster decodes");
    assert_eq!(
        canonical(&serde_json::to_value(&roster).expect("the roster re-serializes")),
        canonical(&golden()["roster"]),
        "decoding and re-encoding the golden roster must lose nothing"
    );
}

#[test]
fn the_golden_topology_is_what_this_client_computes_from_the_golden_roster() {
    let roster =
        Roster::from_json(&canonical(&golden()["roster"])).expect("the golden roster decodes");
    // THE GOLDEN'S OWN SESSION, read out of the fixture rather than composed
    // here. `desired_topology` takes the name because its hot callers already
    // hold it (see its doc); a golden that recorded one answer and was checked
    // against a name invented by the test would prove nothing about the field.
    let recorded = golden();
    let session = recorded["topology"]["session"]
        .as_str()
        .expect("the golden records the session its topology is drawn into");
    let computed = serde_json::to_value(
        desired_topology(&roster, &fixture_hashes(&roster), session)
            .expect("the golden roster plans"),
    )
    .expect("the topology serializes");

    assert_eq!(
        canonical(&computed),
        canonical(&golden()["topology"]),
        "the client's placement must be byte-identical to the golden chiefd still plans"
    );

    // The golden must actually exercise the rules, or the equality is an
    // agreement about nothing.
    let windows = golden()["topology"]["windows"].as_array().expect("windows").clone();
    assert!(windows.len() > 1, "one window would exercise no ordering");
    let named: Vec<&str> =
        windows.iter().map(|window| window["logicalId"].as_str().expect("an id")).collect();
    // A NESTED department's people, and a HEAD's, are ordinary people here.
    // This assertion used to read `named.contains("quant-alpha")`, because a
    // nested department getting a window of its own was the rule under test.
    // The rule now is that neither depth nor headship changes anything: every
    // desired person gets one window, whoever they are and wherever they sit.
    for person in ["alpha-head", "alpha-worker", "it-head", "chief"] {
        assert!(
            named.contains(&person_window_id(person).as_str()),
            "{person} must have a window of their own: {named:?}"
        );
    }
    for window in &windows {
        let panes = window["panes"].as_array().expect("panes");
        assert_eq!(panes.len(), 1, "a golden window holds one person: {window}");
        assert_eq!(
            window["logicalId"].as_str().expect("an id"),
            person_window_id(panes[0]["personId"].as_str().expect("a person id")),
            "a golden window's id names the person in it"
        );
    }
    // And the golden records no department as a window.
    let roster = Roster::from_json(&canonical(&golden()["roster"])).expect("the golden decodes");
    for unit in &roster.departments {
        assert!(!named.contains(&unit.id.as_str()), "'{}' is a department: {named:?}", unit.id);
    }
}

#[test]
fn the_golden_window_names_are_this_clients_own_canonicalization() {
    let cases = golden()["windowNames"].as_array().expect("name cases").clone();
    assert!(!cases.is_empty(), "an empty case list would assert nothing");
    for case in cases {
        let raw = case["raw"].as_str().expect("a raw name");
        assert_eq!(
            safe_window_name(raw),
            case["safe"].as_str().expect("a canonical name"),
            "window-name case {raw:?}"
        );
    }
}

#[test]
fn the_golden_layouts_are_this_clients_own_geometry() {
    let cases = golden()["layouts"].as_array().expect("layout cases").clone();
    assert!(!cases.is_empty(), "an empty case list would assert nothing");
    for case in cases {
        let width = case["width"].as_i64().expect("a width");
        let height = case["height"].as_i64().expect("a height");
        let panes: Vec<String> = case["panes"]
            .as_array()
            .expect("pane ids")
            .iter()
            .map(|id| id.as_str().expect("a pane id string").to_owned())
            .collect();
        let refs: Vec<&str> = panes.iter().map(String::as_str).collect();
        assert_eq!(
            crate::layout::organization_tmux_layout(width, height, None, &refs)
                .expect("the golden cases lay out"),
            case["layout"].as_str().expect("a layout string"),
            "layout case {panes:?} at {width}x{height}"
        );
    }
}

#[test]
fn the_golden_file_on_disk_is_its_own_canonical_serialization() {
    // What makes every assertion above a claim about BYTES rather than about
    // data: the file's own text is the canonical pretty-print of its content,
    // so the strings compared are the file's strings.
    assert_eq!(
        GOLDEN,
        format!("{}\n", canonical(&golden())),
        "the golden file is not canonically formatted; regenerate it"
    );
}

// --- the click that moves nothing ------------------------------------------
//
// Operator ruling, 2026-08-14: "If I click on a person, move him into a new
// window so I can see him alone. If I click back to the department, move him
// back." That produced a `focus: Option<&str>` parameter here, which lifted one
// person's pane out of their department's window into `FOCUS_WINDOW_ID`.
//
// Operator report, 2026-08-21, on a screen recording of that shipping product:
// "when I click on an agent I want it should be in the final position, right?
// Why is it going half screen and growing?" Frame 020 shows the Chief filling
// the content area; frame 031, one click later, shows Sam's text wrapped to
// half the pane with the right half blank, and then it reflows out.
//
// Both rulings are the same ruling. "See him alone" was never a request to move
// a pane; the move was the mechanism, and the mechanism is what the operator
// then watched. `focus` is deleted, everybody is placed alone from the start,
// and the click is a `select-window` — see `sidebar::effects::show_person`.

/// THE PLACEMENT HALF OF "A CLICK RESIZES NOTHING".
///
/// A click cannot move a pane if placement offers nowhere to move it to. There
/// is no argument left that could name one, so the strongest available
/// statement at this layer is that the plan is a function of the ROSTER and the
/// DESIRED SET alone — click or no click, the same bytes.
#[test]
fn no_click_can_change_the_plan_because_no_click_reaches_it() {
    let steady = cobalt_steady();
    let before = planned(&steady);
    // Every person in the fixture, desired or not, plus an id nothing carries.
    // Under the old signature each of these was a distinct topology.
    for _clicked in steady
        .people
        .iter()
        .map(|person| person.id.as_str())
        .chain(std::iter::once("nobody-by-this-id"))
    {
        assert_eq!(
            desired_topology(&steady, &fixture_hashes(&steady), FIXTURE_SESSION)
                .expect("the fixture roster must plan"),
            before,
            "placement takes the roster and the desired set, and nothing else"
        );
    }
}

/// THE FOCUS WINDOW HOLDS NO LIVE PERSON, and placement never names it.
///
/// It still exists — the rail parks a standing notice in it, paints a sleeping
/// person's card there, and paints "… is starting" there while a wake is on its
/// way — and converge is still told never to reap it. What changed is that it
/// is FURNITURE ONLY. A live person in it would be a person whose window
/// converge does not want, which is a pane converge would move, which is the
/// resize this whole change deletes.
#[test]
fn the_focus_window_is_never_placed_and_never_holds_a_person() {
    for roster in [cobalt_steady(), cobalt(&[]), cobalt(&["chief"])] {
        let d = planned(&roster);
        assert!(
            d.windows.iter().all(|window| window.logical_id != super::FOCUS_WINDOW_ID),
            "placement must not name the rail's card window: {:?}",
            window_ids(&d)
        );
    }
}

/// THE RESERVED ID IS REFUSED, not assumed away.
///
/// The deleted ancestor of this mechanism asserted in PROSE that a department
/// id can never be `__focus__`, because chiefd mints slugs. Prose is not a
/// check, and the consequence of being wrong is not cosmetic: converge's
/// undesired-window reap is aimed BY LOGICAL WINDOW ID, so a department wearing
/// the reserved id would be a real department the reap could destroy, and a
/// card window a real department could inherit.
#[test]
fn a_department_claiming_the_reserved_focus_id_is_refused() {
    let mut colliding = cobalt_steady();
    colliding.departments[3].id = super::FOCUS_WINDOW_ID.to_owned();
    colliding.people[7].department_id = super::FOCUS_WINDOW_ID.to_owned();
    colliding.people[7].is_head_of = Some(super::FOCUS_WINDOW_ID.to_owned());
    assert!(
        matches!(
            desired_topology(&colliding, &fixture_hashes(&colliding), FIXTURE_SESSION),
            Err(RosterError::ReservedDepartmentId(id)) if id == super::FOCUS_WINDOW_ID
        ),
        "a department may not claim the card window's id"
    );
    // And the honest fixture never does.
    assert!(cobalt_steady().departments.iter().all(|unit| unit.id != super::FOCUS_WINDOW_ID));
    assert!(super::FOCUS_WINDOW_ID.starts_with("__"));
}
