//! The roster facts, proven two ways.
//!
//! 1. `desiredActive` is chiefd's own answer, across the paused-subtree,
//!    benched, departed, parked and `handoff-required` cases.
//! 2. A client can rebuild the ENTIRE display topology from these facts alone.
//!    [`client_topology`] below is the placement rule set that now lives in
//!    `chief-cli`, written against nothing but [`DesiredRoster`], and it is
//!    asserted against the shared golden — the same bytes `chief-cli` asserts
//!    its own answer against. If a field the placement rules need were missing
//!    from the roster, this test could not compile, let alone pass.
//!
//! #751/P10: these cases used to assert equality against
//! `reconcile_plan::desired_topology`, chiefd's own second implementation of
//! placement. That planner is deleted, so the comparison is now against the
//! golden rather than against a live copy of the thing being deleted — which is
//! the whole point: two implementations that agree today are still two
//! implementations.

use super::{project_desired_roster, DesiredRoster};
use crate::store::activity::{
    ActivityLedger, GracefulTransition, TransitionAction, TransitionStatus,
};
use crate::store::organization::{EmploymentState, OrganizationManifest, PersonRecord, UnitState};
use crate::test_support::northstar_manifest;

const EPOCH: i64 = 1_784_116_800_000; // 2026-07-15T12:00:00.000Z

/// The root department id every company uses.
const ROOT: &str = crate::store::organization::ROOT_DEPARTMENT_ID;

fn iso(millis: i64) -> String {
    crate::isotime::iso_millis(millis)
}

/// Northstar plus a department nested two levels deep, with its own head and
/// its own worker.
///
/// The shape the head-in-parent rule actually discriminates: `quant-alpha`'s
/// head must land in `quant`'s window, NOT the root's and NOT their own. A
/// fixture whose every department hangs off the root cannot tell the correct
/// rule from "every head sits at the root", and that is the whole rule a client
/// has to reproduce.
fn nested_manifest() -> OrganizationManifest {
    let mut manifest = northstar_manifest(EPOCH);
    let at = iso(EPOCH);

    let mut alpha = manifest.departments["quant"].clone();
    alpha.id = "quant-alpha".to_owned();
    alpha.name = "Alpha".to_owned();
    alpha.parent_department_id = Some("quant".to_owned());
    alpha.head_person_id = "alpha-head".to_owned();
    manifest.department_order.push("quant-alpha".to_owned());
    manifest.departments.insert("quant-alpha".to_owned(), alpha);

    for (id, name, title, template) in [
        ("alpha-head", "Alex", "Head of Alpha", "quant-head"),
        ("alpha-worker", "Ada", "Alpha Researcher", "signal-researcher"),
    ] {
        let mut person: PersonRecord = manifest.people[template].clone();
        person.id = id.to_owned();
        person.name = name.to_owned();
        person.title = title.to_owned();
        person.department_id = "quant-alpha".to_owned();
        person.created_at = at.clone();
        manifest.people_order.push(id.to_owned());
        manifest.people.insert(id.to_owned(), person);
    }
    manifest
}

/// The activity ledger for a company whose last reconcile wanted everybody
/// running. `ActivityLedger::initial` deliberately seeds `last_desired_active`
/// FALSE (inv 20), which is the "nobody has been launched yet" state; these
/// tests need the steady state.
fn converged_ledger(manifest: &OrganizationManifest) -> ActivityLedger {
    let mut ledger = ActivityLedger::initial(manifest, &iso(EPOCH));
    for person_id in &manifest.people_order {
        let state = ledger.people.get_mut(person_id).expect("seeded person state");
        state.last_desired_active = true;
    }
    ledger
}

// --- the client-side placement rules, from the facts alone -----------------

/// What the terminal client renders, expressed as `[(windowId, windowName,
/// [personId])]`.
///
/// Deliberately no session name: a tmux session name is the CLIENT's, minted
/// from the slug by `chief-cli/src/placement.rs::session_name_for_slug`, and it
/// is not the backend's `organization::runtime_session_for_slug` (a legacy blob
/// key that happens to look similar and must not be used as a tmux target).
/// Placement is windows and panes, and that is what these rules derive.
type ClientTopology = Vec<(String, String, Vec<String>)>;

/// Compute the display topology from the roster facts and NOTHING else.
///
/// This is the rule set that lives in `chief-cli`:
///
/// * window = PERSON, and a window holds exactly one pane
/// * window id = `__person__:<person id>`, window name = the person's display name
/// * window order = the company's canonical person order (`displayOrder`)
/// * a person chiefd does not desire gets no window
///
/// Note what it does NOT read: no `paneDepartmentId`, because the roster does
/// not publish one and chiefd no longer stores one. The window is DERIVED from
/// the roster the client is holding, which is the point of the exercise.
///
/// # What this replaced
///
/// It used to be *window = DEPARTMENT, pane = person* — its people tiled into
/// their unit's window — with an empty-department rule beneath it and, before
/// that, a HEAD-IN-PARENT rule that put a head's pane in their department's
/// parent. Every version of it decided a person's pane WIDTH by how crowded
/// their department was, so showing one person alone meant MOVING their pane
/// and therefore resizing it. The operator recorded the cost on 2026-08-21:
/// their agent's text arrived wrapped to half the pane and then grew out to
/// fill it, because a Pi whose pane changes width repaints its whole
/// scrollback. A pane has exactly one size; one window per person is the only
/// shape in which it keeps it. See `chief-cli/src/placement.rs`.
fn client_topology(roster: &DesiredRoster) -> ClientTopology {
    let mut ordered = roster.people.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|person| person.display_order);
    ordered
        .into_iter()
        .filter(|person| person.desired_active)
        .map(|person| {
            (
                format!("__person__:{}", person.id),
                person.display_name.clone(),
                vec![person.id.clone()],
            )
        })
        .collect()
}

fn roster(manifest: &OrganizationManifest, ledger: &ActivityLedger) -> DesiredRoster {
    project_desired_roster(manifest, Some(ledger))
}

// --- 1. the roster carries everything placement needs ----------------------

#[test]
fn a_client_rebuilds_the_whole_topology_from_the_roster_facts_alone() {
    let manifest = nested_manifest();
    let ledger = converged_ledger(&manifest);

    let windows = client_topology(&roster(&manifest, &ledger));

    assert_eq!(windows, golden_topology(), "the roster must carry every input placement needs");

    // The fixture must actually exercise the rules, or the equality above is
    // an agreement about nothing.
    let named: Vec<&str> = windows.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(named.len() > 1, "one window would exercise no ordering: {named:?}");
    // NEITHER DEPTH NOR HEADSHIP CHANGES ANYTHING. These assertions used to
    // claim HEAD-IN-PARENT — the root window holding
    // `["chief", "quant-head", "it-head"]` — and then, after that rule was
    // retired, one window per DEPARTMENT with its own head inside it. A person
    // nested two levels down and a person who heads a unit are ordinary people
    // here: each gets one window, holding themselves.
    for person in ["chief", "quant-head", "it-head", "alpha-head", "alpha-worker"] {
        let logical = format!("__person__:{person}");
        let window = windows
            .iter()
            .find(|(id, _, _)| *id == logical)
            .unwrap_or_else(|| panic!("{person} has a window of their own: {named:?}"));
        assert_eq!(window.2, vec![person.to_owned()], "and holds only them");
    }
    // AND NO DEPARTMENT IS A WINDOW.
    for unit in &roster(&manifest, &ledger).departments {
        assert!(!named.contains(&unit.id.as_str()), "'{}' is a department: {named:?}", unit.id);
    }
}

#[test]
fn the_derived_placement_tracks_a_move_immediately() {
    // The correction that motivated the whole split: the deleted planner
    // trusted a PERSISTED `last_pane_department_id`, so a structural change
    // the column had not seen produced a stale window. The roster publishes no
    // such column, so a client's derivation tracks the manifest the moment it
    // changes. Here the stored ledger is deliberately left untouched while the
    // manifest moves a person between departments.
    //
    // MOVED from `the_derived_placement_tracks_a_reparent_immediately`, which
    // reparented Alpha and watched its head follow the new parent. Placement
    // no longer reads a department's parent — see `client_topology` — so that
    // gesture proves nothing now, while the claim it made survives whole.
    let mut manifest = nested_manifest();
    let ledger = converged_ledger(&manifest);
    manifest.people.get_mut("alpha-worker").expect("the alpha worker").department_id =
        manifest.root_department_id.clone();

    // THE DERIVATION IS UNCHANGED BY THE MOVE, WHICH IS NOW THE POINT.
    //
    // The assertion used to be that the moved person appeared in the ROOT's
    // window. A window is a person now, so a reorganisation changes nothing on
    // the glass at all: the operator watching somebody who gets moved keeps the
    // same pane, at the same width, with the same scrollback. What survives is
    // the claim the test was written for — the client derives from the CURRENT
    // tree, with no persisted column to go stale — and it is read off the
    // ROSTER, which is where that fact now lands.
    let moved = roster(&manifest, &ledger)
        .people
        .into_iter()
        .find(|person| person.id == "alpha-worker")
        .expect("the alpha worker");
    assert_eq!(
        moved.department_id, manifest.root_department_id,
        "a client derives placement from the CURRENT tree, not a stored column"
    );
    assert_eq!(
        client_topology(&roster(&manifest, &ledger)),
        client_topology(&roster(&nested_manifest(), &ledger)),
        "and the displayed topology is untouched by the move"
    );
}

// --- 2. desiredActive is chiefd's own predicate ----------------------------

/// Who the roster says should be running, in canonical person order.
fn roster_desired(manifest: &OrganizationManifest, ledger: &ActivityLedger) -> Vec<String> {
    roster(manifest, ledger)
        .people
        .into_iter()
        .filter(|person| person.desired_active)
        .map(|person| person.id)
        .collect()
}

#[test]
fn desired_active_applies_every_roster_filter() {
    let base = nested_manifest();

    // 1. steady state: everybody the manifest knows.
    let ledger = converged_ledger(&base);
    assert_eq!(
        roster_desired(&base, &ledger),
        base.people_order,
        "a converged company desires its whole roster"
    );

    // 2. a paused SUBTREE — Quant pauses, so Alpha beneath it goes with it, and
    //    Quant's own head stops even though their pane would hang off the root.
    let mut paused = base.clone();
    paused.departments.get_mut("quant").expect("quant").state = UnitState::Paused;
    let ledger = converged_ledger(&paused);
    assert_eq!(
        roster_desired(&paused, &ledger),
        vec!["chief".to_owned(), "it-head".to_owned()],
        "a paused subtree desires nobody, and neither does its head"
    );

    // 3. benched and departed.
    let mut roster_states = base.clone();
    roster_states.people.get_mut("alpha-worker").expect("worker").employment_state =
        EmploymentState::Benched;
    roster_states.people.get_mut("signal-researcher").expect("worker").employment_state =
        EmploymentState::Departed;
    let ledger = converged_ledger(&roster_states);
    let desired = roster_desired(&roster_states, &ledger);
    assert!(!desired.contains(&"alpha-worker".to_owned()), "benched: {desired:?}");
    assert!(!desired.contains(&"signal-researcher".to_owned()), "departed: {desired:?}");
    assert!(desired.contains(&"chief".to_owned()), "everybody else is untouched: {desired:?}");

    // 4. the last reconcile parked somebody.
    let mut ledger = converged_ledger(&base);
    ledger.people.get_mut("alpha-worker").expect("worker").last_desired_active = false;
    let desired = roster_desired(&base, &ledger);
    assert!(!desired.contains(&"alpha-worker".to_owned()), "parked by activity: {desired:?}");
}

#[test]
fn a_pending_handoff_keeps_a_departed_person_desired() {
    // The one override that beats roster state: somebody kept alive just long
    // enough to write a required handoff.
    let mut manifest = nested_manifest();
    manifest.people.get_mut("alpha-worker").expect("worker").employment_state =
        EmploymentState::Departed;
    let mut ledger = converged_ledger(&manifest);
    let transition_id = "transition:1:alpha-worker:park".to_owned();
    ledger.people.get_mut("alpha-worker").expect("worker").active_transition_id =
        Some(transition_id.clone());
    ledger.transitions.insert(
        transition_id.clone(),
        GracefulTransition {
            id: transition_id,
            person_id: "alpha-worker".to_owned(),
            action: TransitionAction::Park,
            reason: "offboard".to_owned(),
            intent_id: None,
            placement_department_id: "quant-alpha".to_owned(),
            to_department_id: None,
            status: TransitionStatus::AwaitingHandoff,
            requested_at: iso(EPOCH),
            handoff_deadline_at: iso(EPOCH + 60_000),
            applied_at: None,
            cancelled_at: None,
            forced_at: None,
            abandoned_at: None,
        },
    );

    let desired = roster_desired(&manifest, &ledger);
    assert!(
        desired.contains(&"alpha-worker".to_owned()),
        "a DEPARTED person owing a handoff stays desired: {desired:?}"
    );
}

#[test]
fn a_company_that_has_never_converged_desires_its_whole_roster() {
    // No ledger at all is not a special case with its own rule: it is the
    // "person carries no decision" branch of the same predicate.
    let manifest = nested_manifest();
    let fresh = project_desired_roster(&manifest, None);

    assert_eq!(
        fresh.people.iter().filter(|p| p.desired_active).map(|p| p.id.clone()).collect::<Vec<_>>(),
        manifest.people_order,
        "an absent decision defaults to desired, subject to the roster filters"
    );
}

// --- 3. the facts are facts ------------------------------------------------

#[test]
fn every_person_and_department_appears_in_canonical_order_desired_or_not() {
    let manifest = nested_manifest();
    let mut ledger = converged_ledger(&manifest);
    ledger.people.get_mut("alpha-worker").expect("worker").last_desired_active = false;
    let facts = roster(&manifest, &ledger);

    // Membership, not just the running set: a client tells its OWN departed
    // person's leaked process from a stranger's using exactly this list.
    assert_eq!(
        facts.people.iter().map(|person| person.id.clone()).collect::<Vec<_>>(),
        manifest.people_order
    );
    assert_eq!(
        facts.departments.iter().map(|unit| unit.id.clone()).collect::<Vec<_>>(),
        manifest.department_order
    );
    assert!(facts.people.iter().any(|person| !person.desired_active), "the undesired stay listed");
    assert_eq!(facts.company.slug, manifest.slug);
    assert_eq!(facts.company.display_name, manifest.name);
    assert_eq!(facts.root_department_id, manifest.root_department_id);
}

#[test]
fn employment_state_is_published_because_undesired_does_not_say_which_kind() {
    // A client that must hide FIRED people and draw sleeping ones cannot work
    // from `desiredActive`: benched, paused, settled and departed all read
    // false there, and only the last one is final. So the state is a fact on
    // the wire rather than something a client guesses.
    let mut manifest = nested_manifest();
    manifest.people.get_mut("alpha-worker").expect("worker").employment_state =
        EmploymentState::Benched;
    manifest.people.get_mut("signal-researcher").expect("worker").employment_state =
        EmploymentState::Departed;
    let facts = roster(&manifest, &converged_ledger(&manifest));
    let state = |id: &str| {
        facts
            .people
            .iter()
            .find(|person| person.id == id)
            .map(|person| person.employment_state.clone())
            .expect("the person is listed")
    };

    assert_eq!(state("chief"), "active");
    assert_eq!(state("alpha-worker"), "benched");
    assert_eq!(state("signal-researcher"), "departed", "and the departed person is still LISTED");
}

#[test]
fn is_head_of_names_the_department_and_is_null_for_everybody_else() {
    let manifest = nested_manifest();
    let facts = roster(&manifest, &converged_ledger(&manifest));
    let head_of = |id: &str| {
        facts
            .people
            .iter()
            .find(|person| person.id == id)
            .and_then(|person| person.is_head_of.clone())
    };

    assert_eq!(head_of("chief").as_deref(), Some(ROOT));
    assert_eq!(head_of("quant-head").as_deref(), Some("quant"));
    assert_eq!(head_of("alpha-head").as_deref(), Some("quant-alpha"));
    assert_eq!(head_of("signal-researcher"), None);
    assert_eq!(head_of("alpha-worker"), None);
}

// --- 4. the shared golden, computed on both sides of the split -------------

/// The fixture BOTH implementations are asserted against, byte for byte.
///
/// Placement lives in the operator client now. A shared file is what makes "the
/// same answer" checkable without either side linking the other: this crate
/// asserts that its ROSTER is the body it serves and that the roster's facts
/// still reproduce the recorded placement, `chief-cli` asserts its own
/// placement against the same bytes, and neither can drift without one of the
/// two tests going red.
const GOLDEN: &str = include_str!("../../../../../tests/fixtures/placement-golden.json");

/// The golden, parsed.
fn golden() -> serde_json::Value {
    serde_json::from_str(GOLDEN).expect("the shared golden must be readable JSON")
}

/// The golden's recorded placement, in [`ClientTopology`]'s shape.
fn golden_topology() -> ClientTopology {
    golden()["topology"]["windows"]
        .as_array()
        .expect("the golden lists windows")
        .iter()
        .map(|window| {
            (
                window["logicalId"].as_str().expect("a logical id").to_owned(),
                window["name"].as_str().expect("a window name").to_owned(),
                window["panes"]
                    .as_array()
                    .expect("panes")
                    .iter()
                    .map(|pane| pane["personId"].as_str().expect("a person id").to_owned())
                    .collect(),
            )
        })
        .collect()
}

/// Canonical text for a value. `serde_json::Value` maps are `BTreeMap`s, so
/// equal values pretty-print to identical bytes — which is what lets an
/// assertion on the TEXT mean byte-identity rather than merely "same data".
fn canonical(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).expect("a value must serialize")
}

#[test]
fn the_golden_roster_is_the_body_this_backend_serves() {
    let manifest = nested_manifest();
    let ledger = converged_ledger(&manifest);
    let served = serde_json::to_value(roster(&manifest, &ledger)).expect("the roster serializes");

    assert_eq!(
        canonical(&served),
        canonical(&golden()["roster"]),
        "the golden roster must be exactly what this backend serves"
    );
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
