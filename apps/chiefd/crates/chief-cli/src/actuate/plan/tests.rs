//! Semantics ported from `tests/org-tmux.test.ts` and
//! `tests/org-department-start-stagger.test.ts`. The TypeScript suites drive a
//! live tmux server; these reproduce the *planning* semantics they assert
//! (placement, adopt/move/respawn/kill decisions and the fail-closed errors)
//! against the pure planner.
//!
//! TOMBSTONE: the stagger-delay-sequence and admission-window assertions. The
//! ramp is deleted by operator ruling, so the property they pinned no longer
//! exists to pin. What replaced them is the OPPOSITE assertion --
//! `boots_every_missing_pane_in_one_pass_with_no_ramp_at_all` -- which is a
//! deliberately changed ruling written down as a test, not a contract quietly
//! dropped.
//!
//! The other deliberate change: the diff key. The planner diffs on the derived
//! launch hash, so [`fixture_hashes`] publishes one per person and a drift test
//! moves the hash directly.

use std::collections::BTreeMap;

use super::{
    compute_converge_plan, distributed_sizes, organization_tmux_layout, ConvergePlan, ObservedPane,
    ObservedTopology, ObservedWindow, PaneId, PaneRef, PlanErr, SpawnSpec, Step, WindowRef,
    WindowSym,
};
// #751/P8: the fixtures build a ROSTER — the wire shape chiefd actually
// publishes — and derive the desired topology through the same
// `placement::desired_topology` the resident actuator calls. They used to build
// a `chiefd-core`-shaped `Manifest` and an `ActivitySnapshot` and call a second
// `desired_topology` that came across with the walk; no client can obtain that
// manifest over HTTP, so those fixtures exercised a path the product does not
// have. Same fixture data, same assertions, now reached the way the client
// reaches it.
use crate::placement::{self, Topology};
use crate::roster::{Roster, RosterCompany, RosterDepartment, RosterPerson};

// --- builders --------------------------------------------------------------

fn dep(order: usize, id: &str, name: &str, parent: Option<&str>, head: &str) -> RosterDepartment {
    RosterDepartment {
        id: id.to_string(),
        name: name.to_string(),
        parent_department_id: parent.map(ToString::to_string),
        head_person_id: head.to_string(),
        order,
        state: "active".to_string(),
    }
}

/// One person. `desired_active` is chiefd's answer, which is what the roster
/// carries — the old fixtures spelled the same fact as an `EmploymentState`
/// the client would have had to re-interpret. A departed or benched person is
/// simply `desired_active: false` here, because deciding that is the BACKEND's
/// job and the roster is where the decision arrives.
fn per(order: usize, id: &str, department: &str, desired_active: bool) -> RosterPerson {
    RosterPerson {
        id: id.to_string(),
        display_name: id.to_string(),
        title: "Engineer".to_string(),
        department_id: department.to_string(),
        is_head_of: None,
        display_order: order,
        desired_active,
        employment_state: "active".to_string(),
    }
}

/// Mark a person as the head of a department, which is what moves their pane
/// into the PARENT's window.
fn heads(mut person: RosterPerson, department: &str) -> RosterPerson {
    person.is_head_of = Some(department.to_string());
    person
}

/// The `org-tmux.test.ts` fixture. `departed = true` mirrors `activeManifest()`
/// (the departed data engineer), which yields the six-pane desired plan.
fn cobalt(departed: bool) -> Roster {
    Roster {
        company: RosterCompany { slug: "cobalt".to_string(), display_name: "Cobalt".to_string() },
        root_department_id: "executive".to_string(),
        departments: vec![
            dep(0, "executive", "Executive", None, "chief"),
            dep(1, "quant", "Quant", Some("executive"), "quant-head"),
            dep(2, "quant-data", "Data", Some("quant"), "quant-data-head"),
            dep(3, "it", "IT", Some("executive"), "it-head"),
        ],
        people: vec![
            heads(per(0, "chief", "executive", true), "executive"),
            heads(per(1, "quant-head", "quant", true), "quant"),
            per(2, "quant-active-quant", "quant", true),
            // Benched: chiefd does not desire them, so no pane.
            per(3, "quant-benched-quant", "quant", false),
            heads(per(4, "quant-data-head", "quant-data", true), "quant-data"),
            per(5, "quant-data-active-data-engineer", "quant-data", true),
            per(6, "quant-data-departed-data-engineer", "quant-data", !departed),
            heads(per(7, "it-head", "it", true), "it"),
        ],
    }
}

/// The `org-department-start-stagger.test.ts` fixture: a CEO plus three
/// separately-startable departments, ten people total.
fn company() -> Roster {
    Roster {
        company: RosterCompany { slug: "acme".to_string(), display_name: "Acme".to_string() },
        root_department_id: "executive".to_string(),
        departments: vec![
            dep(0, "executive", "Executive", None, "casey"),
            dep(1, "engineering", "Engineering", Some("executive"), "eng-head"),
            dep(2, "sales", "Sales", Some("executive"), "sales-head"),
            dep(3, "research", "Research", Some("executive"), "research-head"),
        ],
        people: vec![
            heads(per(0, "casey", "executive", true), "executive"),
            heads(per(1, "eng-head", "engineering", true), "engineering"),
            per(2, "eng-w1", "engineering", true),
            per(3, "eng-w2", "engineering", true),
            heads(per(4, "sales-head", "sales", true), "sales"),
            per(5, "sales-w1", "sales", true),
            per(6, "sales-w2", "sales", true),
            heads(per(7, "research-head", "research", true), "research"),
            per(8, "research-w1", "research", true),
            per(9, "research-w2", "research", true),
        ],
    }
}

fn solo() -> Roster {
    Roster {
        company: RosterCompany { slug: "solo".to_string(), display_name: "Solo".to_string() },
        root_department_id: "executive".to_string(),
        departments: vec![dep(0, "executive", "Executive", None, "chief")],
        people: vec![heads(per(0, "chief", "executive", true), "executive")],
    }
}

/// Look a fixture person up by id.
///
/// Deliberately not a raw index. `Roster::people` is a `Vec`, and a positional
/// `people[3]` in a test silently retargets the moment somebody inserts a
/// person into the fixture — the assertion still passes, against the wrong
/// subject. Panicking on an unknown id makes a fixture edit that breaks a test
/// say so.
fn person<'a>(roster: &'a mut Roster, id: &str) -> &'a mut RosterPerson {
    roster
        .people
        .iter_mut()
        .find(|person| person.id == id)
        .unwrap_or_else(|| panic!("fixture has no person '{id}'"))
}

/// Look a fixture department up by id, for the same reason [`person`] exists.
fn unit<'a>(roster: &'a mut Roster, id: &str) -> &'a mut RosterDepartment {
    roster
        .departments
        .iter_mut()
        .find(|unit| unit.id == id)
        .unwrap_or_else(|| panic!("fixture has no department '{id}'"))
}

/// The session every fixture topology is drawn into.
///
/// `desired_topology` takes the composed NAME rather than deriving one: its two
/// hottest callers — the actuator's converge pass and the brain's click path —
/// already hold the session they are drawing into, so re-deriving it there
/// would hash a directory on every click to reproduce a string the caller was
/// looking at. The fixtures state it for the same reason they state a roster.
const FIXTURE_SESSION: &str = "org-acme-012345_";

fn desired(roster: &Roster) -> Topology {
    placement::desired_topology(roster, &fixture_hashes(roster), FIXTURE_SESSION)
        .expect("the fixture roster must hold together")
}

/// The desired set chiefd would publish for this fixture: every desired-active
/// person, with one stable launch hash each.
///
/// This is also where a fixture's membership comes from: a person absent here
/// gets no pane, whatever the roster's `desiredActive` says. A drift test moves
/// a person's hash directly — that is what a changed department, a changed
/// launch command or a launcher deploy does in production.
fn fixture_hashes(roster: &Roster) -> BTreeMap<String, String> {
    roster
        .people
        .iter()
        .filter(|person| person.desired_active)
        .map(|person| (person.id.clone(), format!("hash-{}", person.id)))
        .collect()
}

fn empty_observed() -> ObservedTopology {
    ObservedTopology {
        session_exists: false,
        session_organization: String::new(),
        windows: Vec::new(),
        panes: Vec::new(),
    }
}

/// Fabricate the observed topology a prior successful reconcile of `desired`
/// would have left: `@<logical>` windows, `%<person>` panes, fully tagged.
fn observe(desired: &placement::Topology) -> ObservedTopology {
    let mut windows = Vec::new();
    let mut panes = Vec::new();
    for window in &desired.windows {
        let window_id = format!("@{}", window.logical_id);
        windows.push(ObservedWindow {
            tmux_id: window_id.clone(),
            organization_id: desired.organization.clone(),
            logical_id: window.logical_id.clone(),
            protected_ui: false,
            sleeping_notice: false,
        });
        for pane in &window.panes {
            panes.push(ObservedPane {
                tmux_id: format!("%{}", pane.person_id),
                tmux_window_id: window_id.clone(),
                organization_id: desired.organization.clone(),
                logical_window_id: window.logical_id.clone(),
                person_id: pane.person_id.clone(),
                launch_hash: pane.launch_hash.clone(),
                start_command: String::new(),
            });
        }
    }
    ObservedTopology {
        session_exists: true,
        session_organization: desired.organization.clone(),
        windows,
        panes,
    }
}

// --- assertions helpers ----------------------------------------------------

/// Persons spawned (created or respawned), in step order.
///
/// No delay travels beside them any more: there is no ramp, so every spawn in a
/// plan is due the instant the plan is applied.
fn admitted(plan: &ConvergePlan) -> Vec<String> {
    plan.steps
        .iter()
        .filter_map(|step| match step {
            Step::CreateSession { first } | Step::CreateWindowWithSpawn { first, .. } => {
                Some(first.person_id.clone())
            }
            Step::SplitPane { spec, .. } | Step::Respawn { spec, .. } => {
                Some(spec.person_id.clone())
            }
            _ => None,
        })
        .collect()
}

/// Person ids as the topology stores them, for comparing against a literal.
fn ids(people: &[&str]) -> Vec<String> {
    people.iter().map(|person| (*person).to_string()).collect()
}

fn window_persons(desired: &placement::Topology, logical: &str) -> Vec<String> {
    desired
        .windows
        .iter()
        .find(|w| w.logical_id == logical)
        .map(|w| w.panes.iter().map(|p| p.person_id.clone()).collect())
        .unwrap_or_default()
}

/// The window one person is placed in — `placement::person_window_id`, spelled
/// once so a fixture reads as the operator's model rather than as a prefix.
fn pw(person_id: &str) -> String {
    placement::person_window_id(person_id)
}

/// The tmux id `observe` fabricates for one person's window.
fn pwid(person_id: &str) -> String {
    format!("@{}", pw(person_id))
}

fn window_logical(window_ref: &WindowRef) -> String {
    match window_ref {
        WindowRef::Observed(id) => id.clone(),
        WindowRef::Created(WindowSym(sym)) => sym.clone(),
    }
}

fn count<F: Fn(&Step) -> bool>(plan: &ConvergePlan, predicate: F) -> usize {
    plan.steps.iter().filter(|step| predicate(step)).count()
}

fn is_hex4(value: &str) -> bool {
    value.len() == 4 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// True when `step` acts on the observed pane whose tmux id is `pane_id`.
fn step_targets_pane(step: &Step, pane_id: &str) -> bool {
    let matches_id = |p: &PaneId| p.0 == pane_id;
    match step {
        Step::MovePane { pane, .. }
        | Step::Respawn { pane, .. }
        | Step::Retag { pane, .. }
        | Step::KillPane { pane }
        | Step::CreateWindowByMove { move_pane: pane, .. } => matches_id(pane),
        Step::ApplyLayout { panes, .. } => panes.iter().any(|p| match p {
            PaneRef::Observed(id) => matches_id(id),
            PaneRef::Created(_) => false,
        }),
        _ => false,
    }
}

#[test]
fn a_sleeping_notice_survives_speculation_and_retires_only_after_live_ownership() {
    let desired = desired(&solo());
    let mut observed = ObservedTopology {
        session_exists: true,
        session_organization: desired.organization.clone(),
        windows: vec![ObservedWindow {
            tmux_id: pwid("chief"),
            organization_id: desired.organization.clone(),
            logical_id: pw("chief"),
            protected_ui: true,
            sleeping_notice: true,
        }],
        panes: Vec::new(),
    };

    let speculative = compute_converge_plan(&desired, &observed).expect("speculative plan");
    assert!(speculative.steps.iter().any(|step| matches!(
        step,
        Step::SplitPane { spec, .. } if spec.person_id == "chief"
    )));
    assert!(
        speculative.steps.iter().any(|step| matches!(
            step,
            Step::ApplyLayout { retire_sleeping_notice: false, panes, .. }
                if panes == &[PaneRef::Created("chief".into())]
        )),
        "a new process is not positive live ownership: {:?}",
        speculative.steps
    );

    observed.panes.push(ObservedPane {
        tmux_id: "%chief".into(),
        tmux_window_id: pwid("chief"),
        organization_id: desired.organization.clone(),
        logical_window_id: pw("chief"),
        person_id: "chief".into(),
        launch_hash: desired.windows[0].panes[0].launch_hash.clone(),
        start_command: String::new(),
    });
    let proven = compute_converge_plan(&desired, &observed).expect("proven-live plan");
    assert_eq!(
        proven.steps.iter().filter(|step| matches!(step, Step::ApplyLayout { .. })).count(),
        1
    );
    assert!(
        proven.steps.iter().any(|step| matches!(
            step,
            Step::ApplyLayout { retire_sleeping_notice: true, panes, .. }
                if panes == &[PaneRef::Observed(PaneId("%chief".into()))]
        )),
        "the first positive observation retires the notice once: {:?}",
        proven.steps
    );
}

// --- desired_topology ------------------------------------------------------

/// Every window the plan kills, by the tmux id it names.
fn killed_windows(plan: &ConvergePlan) -> Vec<String> {
    plan.steps
        .iter()
        .filter_map(|step| match step {
            Step::KillWindow { w: WindowRef::Observed(id) } => Some(id.clone()),
            Step::KillWindow { w: WindowRef::Created(sym) } => Some(sym.0.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn desired_topology_places_every_desired_person_alone() {
    // It used to assert the four DEPARTMENT windows and the tiled grid of
    // people inside each. That model decided a person's pane width by how
    // crowded their department was, so showing them alone meant moving them,
    // which is a resize — see `placement::desired_topology`.
    let d = desired(&cobalt(true));
    let logicals: Vec<&str> = d.windows.iter().map(|w| w.logical_id.as_str()).collect();
    assert_eq!(
        logicals,
        [
            pw("chief"),
            pw("quant-head"),
            pw("quant-active-quant"),
            pw("quant-data-head"),
            pw("quant-data-active-data-engineer"),
            pw("it-head"),
        ]
    );
    for window in &d.windows {
        assert_eq!(window.panes.len(), 1, "no window may hold two people: {window:?}");
    }
    // The benched and departed engineers are excluded.
    assert_eq!(d.windows.len(), 6);
}

#[test]
fn a_deleted_department_is_removed_once_and_never_planned_back() {
    // The tmux half of the operator's deleted-department storm. Begin from a fully
    // converged Quant subtree, then apply the exact durable result of
    // remove-tree: both departments are absent, their retained people are
    // departed and re-homed to the parent, and none is desired.
    let before = desired(&cobalt(false));
    let observed_before = observe(&before);
    let mut after_roster = cobalt(false);
    after_roster
        .departments
        .retain(|department| department.id != "quant" && department.id != "quant-data");
    for person in &mut after_roster.people {
        if person.department_id == "quant" || person.department_id == "quant-data" {
            person.department_id = "executive".to_owned();
            person.is_head_of = None;
            person.desired_active = false;
            person.employment_state = "departed".to_owned();
        }
    }
    let after = desired(&after_roster);

    let removal = compute_converge_plan(&after, &observed_before).expect("one-way removal plan");
    // Each departed person's own window is removed exactly once. It used to be
    // the two DEPARTMENT windows; the durable gesture is the same one — a
    // remove-tree that departs everybody in the subtree — and what it reaps is
    // now one window per person rather than one per unit.
    assert_eq!(
        killed_windows(&removal),
        [
            pwid("quant-head"),
            pwid("quant-active-quant"),
            pwid("quant-data-head"),
            pwid("quant-data-active-data-engineer"),
            pwid("quant-data-departed-data-engineer"),
        ],
        "each departed person's window is removed exactly once: {:?}",
        removal.steps
    );
    assert!(
        !removal.steps.iter().any(|step| matches!(
            step,
            Step::CreateWindowWithSpawn { .. }
                | Step::CreateWindowByMove { .. }
                | Step::SplitPane { .. }
                | Step::Respawn { .. }
        )),
        "no departed person or deleted department is reminted: {:?}",
        removal.steps
    );

    let settled = observe(&after);
    let second = compute_converge_plan(&after, &settled).expect("settled pass");
    assert!(
        second.steps.is_empty(),
        "a later supervision pass has no deleted window to remove or recreate: {:?}",
        second.steps
    );
}

#[test]
fn the_rosters_desired_active_is_authoritative_in_both_directions() {
    // TOMBSTONE (#751/P8). This was
    // `a_handoff_required_decision_beats_roster_state_in_both_directions`, and
    // it reached inside an `ActivitySnapshot` to set a `handoff-required`
    // reason that overrode employment state. That decision is the BACKEND's and
    // always was: chiefd folds handoff leases, benching, pausing and employment
    // into ONE published answer, `desired_active`. The client does not
    // re-derive it and must not — a second opinion about who should be running
    // is the failure this whole workstream exists to remove.
    //
    // What survives, and is worth pinning, is that the client OBEYS that answer
    // in both directions rather than second-guessing it from anything else on
    // the person record. The handoff-lease semantics themselves are covered by
    // `chiefd-core`'s activity tests, where the decision is made.
    let mut roster = cobalt(true);

    // Benched by every other signal, but chiefd says run: a pane appears.
    person(&mut roster, "quant-benched-quant").desired_active = true;
    let d = desired(&roster);
    assert_eq!(window_persons(&d, &pw("quant-benched-quant")), ids(&["quant-benched-quant"]));

    // Ordinary and active by every other signal, but chiefd says no: no pane.
    person(&mut roster, "quant-active-quant").desired_active = false;
    let d = desired(&roster);
    assert!(d.windows.iter().all(|window| window.logical_id != pw("quant-active-quant")));
}

#[test]
fn a_paused_department_subtree_contributes_no_windows() {
    // Pausing is folded into `desired_active` server-side (see
    // `RosterDepartment::state`'s doc: the flag is carried for DISPLAY only),
    // so a paused subtree reaches the client as people chiefd does not desire.
    // The rule this still pins is the client's: a person chiefd does not desire
    // gets no window at all, and that must hold for a whole subtree, not just
    // one level.
    let mut roster = cobalt(true);
    unit(&mut roster, "quant").state = "paused".to_string();
    for person in &mut roster.people {
        // quant, its child quant-data, and both their heads, who sit in the
        // departments they head.
        if person.department_id == "quant" || person.department_id == "quant-data" {
            person.desired_active = false;
        }
    }

    let d = desired(&roster);

    let logicals: Vec<&str> = d.windows.iter().map(|w| w.logical_id.as_str()).collect();
    assert_eq!(
        logicals,
        [pw("chief"), pw("it-head")],
        "nobody in the paused subtree contributes a window"
    );
    assert_eq!(window_persons(&d, &pw("chief")), ids(&["chief"]));
    assert_eq!(window_persons(&d, &pw("it-head")), ids(&["it-head"]));
}

#[test]
fn a_roster_that_does_not_hold_together_fails_closed() {
    // TOMBSTONE (#751/P8). This was
    // `desired_topology_rejects_a_mismatched_or_incomplete_activity_snapshot`,
    // checking that a snapshot disagreeing with its manifest was refused. There
    // is no snapshot and no manifest on this side of the split; the equivalent
    // fail-closed obligation is that a roster which does not hold together is
    // refused rather than half-read.
    //
    // The reason is unchanged and is the important part: a topology computed
    // from a partial roster names windows for departments that do not exist
    // and, far worse, silently OMITS people — and an actuator reads an omission
    // as "stop them".
    let mut duplicate = cobalt(true);
    let clone = person(&mut duplicate, "quant-active-quant").clone();
    duplicate.people.push(clone);
    assert!(matches!(
        placement::desired_topology(&duplicate, &fixture_hashes(&duplicate), FIXTURE_SESSION),
        Err(crate::roster::RosterError::DuplicateId { kind: "person", .. })
    ));

    let mut unknown = cobalt(true);
    person(&mut unknown, "quant-active-quant").department_id = "nowhere".to_string();
    assert!(matches!(
        placement::desired_topology(&unknown, &fixture_hashes(&unknown), FIXTURE_SESSION),
        Err(crate::roster::RosterError::UnknownDepartment { .. })
    ));

    let mut collided = cobalt(true);
    unit(&mut collided, "quant-data").order = 1;
    assert!(matches!(
        placement::desired_topology(&collided, &fixture_hashes(&collided), FIXTURE_SESSION),
        Err(crate::roster::RosterError::DuplicateOrder { kind: "department", order: 1 })
    ));
}

// --- compute_converge_plan: creation ---------------------------------------

/// THE RAMP RULING, WRITTEN DOWN AS A TEST.
///
/// This replaces `admits_only_new_processes_in_deterministic_stagger_order`,
/// which asserted a 150 ms stagger producing delays of 0/150/300/450/600/750
/// and an admission window of 900. That contract is deliberately GONE, by
/// operator ruling -- "just boot them all at the same time" -- so the assertion
/// is inverted rather than deleted: every missing pane is spawned in the one
/// pass that finds it, in deterministic walk order, and nothing anywhere
/// carries a delay, a cap or a batch.
///
/// The walk ORDER is still pinned, because determinism was never the part that
/// was ruled against. Two actuators that disagree about order fight over a
/// window; two actuators that disagree about pacing merely start at different
/// speeds.
#[test]
fn boots_every_missing_pane_in_one_pass_with_no_ramp_at_all() {
    let d = desired(&cobalt(true));
    let plan = compute_converge_plan(&d, &empty_observed()).unwrap();

    assert_eq!(
        admitted(&plan),
        ids(&[
            "chief",
            "quant-head",
            "quant-active-quant",
            "quant-data-head",
            "quant-data-active-data-engineer",
            "it-head",
        ]),
        "every missing pane is created in this pass, in walk order"
    );
    assert!(matches!(plan.steps.first(), Some(Step::CreateSession { .. })));
    assert_eq!(count(&plan, |s| matches!(s, Step::Respawn { .. })), 0);
    assert_eq!(count(&plan, |s| matches!(s, Step::KillPane { .. })), 0);
    assert!(plan.predicted_respawn_persons.is_empty());
    assert!(plan.predicted_kill_panes.is_empty());
}

/// The same ruling at the size that provoked the ramp in the first place.
///
/// The ten-person fixture is the one the deleted stagger suite used, and it is
/// kept for exactly this: #431 watched a big pass drive load to ~25 on 6 cores,
/// and that is the case an accidental reintroduction of a cap would be reached
/// for. Ten people, ten spawns, one pass, and every one of them due
/// immediately.
///
/// The pacing concern was not wrong, only misplaced. If it ever needs answering
/// again it belongs at the exec seam in this crate, where the processes are
/// actually spawned and the load is visible — never in a plan, and never in a
/// desired set chiefd publishes.
#[test]
fn a_ten_person_company_still_boots_in_one_pass() {
    let plan = compute_converge_plan(&desired(&company()), &empty_observed()).unwrap();
    assert_eq!(admitted(&plan).len(), 10, "every desired person is spawned in this pass");
}

#[test]
fn creates_exactly_one_pane_per_active_person_binding_each() {
    let d = desired(&cobalt(true));
    let plan = compute_converge_plan(&d, &empty_observed()).unwrap();

    // One admission per active person; six people, six admissions.
    assert_eq!(admitted(&plan).len(), 6);
    // One window per person, so one ApplyLayout each.
    assert_eq!(count(&plan, |s| matches!(s, Step::ApplyLayout { .. })), 6);
}

#[test]
fn reapplying_the_same_identity_and_launch_hash_is_a_no_op_empty_plan() {
    // #367: a fully converged company (desired == observed) must
    // produce an EMPTY plan — no Retag, no OrderWindows, no ApplyLayout, no
    // anything. This is the flagship idle→zero contract: the steady-state pass
    // must exec zero tmux subprocesses and cause zero writes.
    let d = desired(&cobalt(true));
    let observed = observe(&d);
    let plan = compute_converge_plan(&d, &observed).unwrap();

    assert!(plan.steps.is_empty(), "a converged plan is empty, got {:?}", plan.steps);
    assert!(admitted(&plan).is_empty(), "nothing new is admitted");
    assert!(plan.predicted_respawn_persons.is_empty());
    assert!(plan.predicted_kill_panes.is_empty());
}

/// OUR OWN PANE WITH A STALE WINDOW LABEL IS RETAGGED, NOT A REASON TO STOP.
///
/// # Why the old assertion here was wrong
///
/// This test used to require the whole plan to fail closed, on the reasoning
/// that "a conflicting identity is never repaired optimistically". The ownership
/// half of that is right and is still pinned — by
/// `fails_closed_when_a_pane_and_its_window_disagree_on_identity`, which uses a
/// person this company does not know and still fails closed.
///
/// But a pane's `@organization_window_id` is a CACHED ANSWER to "which window am
/// I in", and the window physically containing it is tmux's own fact. Moving a
/// person is a join plus a retag, and NO ordering makes those one operation: tag
/// first and the pane disagrees with the window it is still in, join first and
/// it disagrees with the window it has just entered. Any process listing panes
/// in between sees a disagreement — so this was not a corrupt state to refuse,
/// it was a normal instant to pass through.
///
/// Measured on a live company: six converge passes in thirty seconds applied
/// NOTHING because one pane was mid-move, and the operator watched a person sit
/// in the sidebar's own column until they clicked the department to force a
/// re-lay. Nothing was being protected by that; a whole company stopped
/// converging over a label.
#[test]
fn our_own_pane_with_a_stale_window_label_is_retagged_and_the_pass_continues() {
    let d = desired(&cobalt(true));
    let mut observed = observe(&d);
    let drifted = observed
        .panes
        .iter_mut()
        .find(|p| p.person_id == "quant-active-quant")
        .expect("fixture has the pane");
    let pane_id = drifted.tmux_id.clone();
    drifted.logical_window_id = "wrong-window".to_string();

    let plan = compute_converge_plan(&d, &observed)
        .expect("a company does not stop converging because one label lags a move");

    assert!(
        plan.warnings.iter().any(|w| w.contains(&pane_id) && w.contains("wrong-window")),
        "the drift is SAID, naming the pane and the label it still carries: {:?}",
        plan.warnings
    );
    assert!(
        plan.steps.iter().any(|step| matches!(
            step,
            Step::Retag { pane, person_id, .. }
                if pane.0 == pane_id && person_id == "quant-active-quant"
        )),
        "and the ordinary retag machinery corrects it — no separate repair path exists or \
         is needed: {:?}",
        plan.steps
    );
    assert!(
        !plan.steps.iter().any(|step| matches!(step, Step::CreateWindowWithSpawn { .. })),
        "the person stays ACCOUNTED FOR, so nothing spawns a second pane for somebody who \
         already has one: {:?}",
        plan.steps
    );
}

#[test]
fn a_kill_takes_the_window_and_re_lays_out_nobody() {
    // #367 asked that removing a person re-lay out only the window whose
    // membership changed. Under one window per person, no SURVIVING window's
    // membership can change when somebody stops — their window was theirs — so
    // the strongest form of that rule is that nothing is re-laid out at all.
    let observed = observe(&desired(&cobalt(true)));
    // Retained panes remain fully converged; the only change is the kill.
    let mut roster = cobalt(true);
    person(&mut roster, "quant-active-quant").desired_active = false;
    let plan = compute_converge_plan(&desired(&roster), &observed).unwrap();

    // The stopped person was alone in their own window, so the window goes
    // whole and NOTHING is re-laid out: every survivor keeps the geometry it
    // already had, which is the point of one window per person.
    assert_eq!(count(&plan, |s| matches!(s, Step::KillPane { .. })), 0);
    assert_eq!(killed_windows(&plan), vec![pwid("quant-active-quant")]);
    let laid_out: Vec<String> = plan
        .steps
        .iter()
        .filter_map(|s| {
            if let Step::ApplyLayout { w, .. } = s {
                Some(window_logical(w))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(laid_out, Vec::<String>::new(), "nobody is re-laid out: {:?}", plan.steps);
    // The converged survivors need no retag.
    assert_eq!(count(&plan, |s| matches!(s, Step::Retag { .. })), 0);
}

#[test]
fn identical_topologies_never_respawn() {
    let observed = observe(&desired(&cobalt(true)));
    let plan = compute_converge_plan(&desired(&cobalt(true)), &observed).unwrap();
    assert!(plan.predicted_respawn_persons.is_empty());
    assert_eq!(count(&plan, |s| matches!(s, Step::Respawn { .. })), 0);
    assert_eq!(count(&plan, |s| matches!(s, Step::Retag { .. })), 0);
}

/// THE HASH-DRIFT RESPAWN RULE — the single most important new actuator rule in
/// this change, and the reason a stale process can never be adopted as current.
///
/// This is the successor to
/// `staggers_a_multi_person_replacement_and_leaves_retained_panes_untouched`.
/// Two things changed and both are deliberate rulings, not relaxations:
///
/// * the DIFF KEY is the derived launch hash rather than a hand-bumped
///   counter, so nothing has to remember to advance it;
/// * the stagger assertion is gone with the ramp, so both replacements happen
///   in this one pass.
///
/// Everything the old test actually protected is asserted unchanged: exactly
/// the drifted people re-run, in walk order; the pane IDENTITY survives, so a
/// respawn is not a kill-and-create; and every converged pane is left strictly
/// alone -- no respawn, no retag, no layout churn.
#[test]
fn a_drifted_launch_hash_respawns_exactly_those_panes_and_touches_no_other() {
    let observed = observe(&desired(&cobalt(true)));

    // Move two people's launch inputs: their published hash changes -- exactly
    // what a changed department, a changed launch command or a launcher deploy
    // would do in production.
    let roster = cobalt(true);
    let mut hashes = fixture_hashes(&roster);
    for moved in ["quant-head", "quant-data-head"] {
        hashes.insert(moved.to_owned(), "hash-moved".to_owned());
    }
    let d = placement::desired_topology(&roster, &hashes, FIXTURE_SESSION)
        .expect("the fixture roster plans");

    let plan = compute_converge_plan(&d, &observed).unwrap();

    assert_eq!(plan.predicted_respawn_persons, ids(&["quant-head", "quant-data-head"]));
    assert_eq!(
        admitted(&plan),
        ids(&["quant-head", "quant-data-head"]),
        "both drifted panes are replaced in the same pass; there is no ramp to spread them"
    );
    // THE PANE IS REPLACED, NOT ADOPTED. A pane whose tag no longer matches is
    // running something other than what chiefd wants, and adopting it is the
    // silent-stale-fleet incident this whole fence exists to prevent.
    let respawned: Vec<&PaneId> = plan
        .steps
        .iter()
        .filter_map(|s| if let Step::Respawn { pane, .. } = s { Some(pane) } else { None })
        .collect();
    assert_eq!(
        respawned,
        vec![&PaneId("%quant-head".to_string()), &PaneId("%quant-data-head".to_string())],
        "a respawn preserves the pane id; it is a replacement of the PROCESS"
    );
    // The spawn carries the NEW hash, so the pane is re-tagged with what it is
    // actually running. Tagging it with the old one would make the next pass
    // replace it again, forever.
    for step in &plan.steps {
        if let Step::Respawn { spec, .. } = step {
            assert_eq!(spec.launch_hash, "hash-moved", "the pane is tagged with what it now runs");
        }
    }
    // No new panes; no churn anywhere else.
    assert_eq!(count(&plan, |s| matches!(s, Step::SplitPane { .. })), 0);
    assert_eq!(count(&plan, |s| matches!(s, Step::CreateSession { .. })), 0);
    assert_eq!(count(&plan, |s| matches!(s, Step::CreateWindowWithSpawn { .. })), 0);
    assert_eq!(count(&plan, |s| matches!(s, Step::KillPane { .. })), 0);
}

/// The other half of the same rule, and the one that keeps a steady company
/// still: a pane whose tag EQUALS the desired hash is adopted untouched, no
/// matter how long it has been up. Without this the fence would be a restart
/// loop rather than a fence.
#[test]
fn a_matching_launch_hash_adopts_the_pane_and_emits_nothing() {
    let d = desired(&cobalt(true));
    let plan = compute_converge_plan(&d, &observe(&d)).unwrap();
    assert!(plan.steps.is_empty(), "a converged company yields no steps at all: {:?}", plan.steps);
}

// --- compute_converge_plan: remove / move ----------------------------------

#[test]
fn removes_a_now_benched_person_while_retaining_every_other_pane() {
    let observed = observe(&desired(&cobalt(true)));
    let mut roster = cobalt(true);
    person(&mut roster, "quant-active-quant").desired_active = false;
    let plan = compute_converge_plan(&desired(&roster), &observed).unwrap();

    // The pane is still REPORTED as killed — that is what the actuator's round
    // counters and crash-loop registry read — but the step is the whole window,
    // because the window was theirs and holds nothing else.
    assert_eq!(plan.predicted_kill_panes, vec![PaneId("%quant-active-quant".to_string())]);
    assert_eq!(count(&plan, |s| matches!(s, Step::KillPane { .. })), 0);
    assert_eq!(killed_windows(&plan), vec![pwid("quant-active-quant")]);
    assert!(plan.predicted_respawn_persons.is_empty());
    // The remaining panes retain their matching identity tags; none respawn.
    assert_eq!(count(&plan, |s| matches!(s, Step::Retag { .. })), 0);
}

#[test]
fn moves_a_stable_pane_into_a_new_window_by_move_not_respawn() {
    // The gesture used to be a DEPARTMENT reassignment, which moved the person
    // between department windows. A reassignment moves nobody now — a window is
    // a person — so the gesture that still reaches `CreateWindowByMove` is the
    // UPGRADE: a live pane observed in a window that is not its person's, and
    // whose person's window does not exist yet.
    let roster = cobalt(true);
    let d = desired(&roster);
    let mut observed = observe(&d);
    // Fold `quant-active-quant`'s pane into the CEO's window and delete the
    // window that was theirs, exactly as the department model left it.
    observed.windows.retain(|window| window.logical_id != pw("quant-active-quant"));
    let stray = observed
        .panes
        .iter_mut()
        .find(|pane| pane.person_id == "quant-active-quant")
        .expect("the quant");
    stray.tmux_window_id = pwid("chief");
    stray.logical_window_id = pw("chief");

    let plan = compute_converge_plan(&d, &observed).unwrap();
    let by_move: Vec<(&WindowSym, &PaneId)> = plan
        .steps
        .iter()
        .filter_map(|s| {
            if let Step::CreateWindowByMove { w, move_pane, .. } = s {
                Some((w, move_pane))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        by_move,
        vec![(&WindowSym(pw("quant-active-quant")), &PaneId("%quant-active-quant".to_string()))]
    );
    assert_eq!(count(&plan, |s| matches!(s, Step::Respawn { .. })), 0);
    assert!(plan.predicted_kill_panes.is_empty());
    // The pane keeps its identity: no admission consumed for the move.
    assert!(admitted(&plan).is_empty());
}

#[test]
fn moves_a_stable_pane_into_an_existing_window() {
    // Same repair, one step smaller: the person's own window is already there —
    // an interrupted move left it empty — so the pane is joined rather than
    // broken out into a new one.
    let roster = cobalt(true);
    let d = desired(&roster);
    let mut observed = observe(&d);
    let stray = observed
        .panes
        .iter_mut()
        .find(|pane| pane.person_id == "quant-active-quant")
        .expect("the quant");
    stray.tmux_window_id = pwid("chief");
    stray.logical_window_id = pw("chief");

    let plan = compute_converge_plan(&d, &observed).unwrap();
    let moves: Vec<(&PaneId, String)> = plan
        .steps
        .iter()
        .filter_map(|s| {
            if let Step::MovePane { pane, to } = s {
                Some((pane, window_logical(to)))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        moves,
        vec![(&PaneId("%quant-active-quant".to_string()), pwid("quant-active-quant"))]
    );
    assert_eq!(count(&plan, |s| matches!(s, Step::CreateWindowByMove { .. })), 0);
    assert_eq!(count(&plan, |s| matches!(s, Step::Respawn { .. })), 0);
    assert!(plan.predicted_kill_panes.is_empty());
}

/// PLAN AC 12: A TRANSFER MOVES NOBODY AT ALL, which is stronger than the rule
/// it replaces.
///
/// This used to assert exactly one `MovePane`: a transferred person left their
/// old department's window and joined their new one, without their process
/// being touched. The "without being touched" half was always the point — a
/// live Pi must survive a reorganisation — and the move half was the cost of
/// placing people by department.
///
/// A window is a person now, so a transfer changes NO window, and the operator
/// watching somebody who gets reorganised keeps the same pane, at the same
/// width, with the same scrollback.
#[test]
fn a_transfer_relocates_nothing_and_never_replaces_the_process() {
    let observed = observe(&desired(&cobalt(true)));
    let mut roster = cobalt(true);
    person(&mut roster, "quant-active-quant").department_id = "executive".to_string();
    let d = desired(&roster);
    let plan = compute_converge_plan(&d, &observed).unwrap();

    assert!(plan.steps.is_empty(), "a transfer is not a display event: {:?}", plan.steps);
    assert!(plan.predicted_respawn_persons.is_empty());
    assert!(plan.predicted_kill_panes.is_empty(), "and nothing is killed");
    assert!(admitted(&plan).is_empty(), "nothing is spawned either");
}

/// PLAN AC 12: KILL-AND-RELAUNCH WHEN A MOVE IS NOT ENOUGH.
///
/// The other half of the plan's sentence — "a seamless move when possible,
/// kill-and-relaunch when not". A person who is BOTH transferred and has a real
/// launch input changed under them (a launcher deploy, here) is replaced,
/// because the second change is a genuine reason to replace the process while
/// the first is not.
///
/// Without this the test above would be satisfied by a planner that had simply
/// stopped noticing anything, which is exactly what "a transfer plans nothing"
/// would otherwise be indistinguishable from.
#[test]
fn a_transfer_that_also_changes_a_real_launch_input_moves_and_replaces() {
    let observed = observe(&desired(&cobalt(true)));
    let mut roster = cobalt(true);
    person(&mut roster, "quant-active-quant").department_id = "executive".to_string();
    // A launcher deploy under the same person: the published hash moves.
    let mut hashes = fixture_hashes(&roster);
    hashes.insert("quant-active-quant".to_owned(), "hash-moved".to_owned());
    let d = placement::desired_topology(&roster, &hashes, FIXTURE_SESSION)
        .expect("the fixture roster plans");
    let plan = compute_converge_plan(&d, &observed).unwrap();

    assert_eq!(
        count(&plan, |s| matches!(s, Step::MovePane { .. })),
        0,
        "still relocated by nobody"
    );
    assert_eq!(
        plan.predicted_respawn_persons,
        ids(&["quant-active-quant"]),
        "and replaced, because a real launch input moved: {:?}",
        plan.steps
    );
}

/// PLAN AC 12: IDLE STOP, as the actuator sees it.
///
/// chiefd settles an idle person by removing them from the desired set. There
/// is no stop VERB any more — absence IS the instruction — so this is what an
/// idle stop looks like from the only process that can act on one: the person's
/// pane is killed, everybody else is left strictly alone, and no layout churn
/// touches an untouched window.
#[test]
fn an_idle_person_dropped_from_the_desired_set_has_exactly_their_pane_killed() {
    let observed = observe(&desired(&cobalt(true)));
    let mut roster = cobalt(true);
    // Settled: chiefd stops desiring them. Nothing else about them changes.
    person(&mut roster, "quant-active-quant").desired_active = false;
    let plan = compute_converge_plan(&desired(&roster), &observed).unwrap();

    assert_eq!(
        plan.predicted_kill_panes,
        vec![PaneId("%quant-active-quant".to_string())],
        "absence from the desired set is the whole instruction: {:?}",
        plan.steps
    );
    assert_eq!(count(&plan, |s| matches!(s, Step::Respawn { .. })), 0);
    assert!(admitted(&plan).is_empty(), "settling one person starts nobody");
    assert_eq!(
        count(&plan, |s| matches!(s, Step::StopSession)),
        0,
        "one person settling is not the company shutting down"
    );
}

#[test]
fn removes_the_owned_session_when_the_disk_plan_has_no_active_people() {
    let observed = observe(&desired(&cobalt(true)));
    let mut roster = cobalt(true);
    for person in &mut roster.people {
        person.desired_active = false;
    }
    let d = desired(&roster);
    assert!(d.windows.is_empty(), "a company that desires nobody has no desired windows");

    let plan = compute_converge_plan(&d, &observed).unwrap();
    assert_eq!(plan.steps, vec![Step::StopSession]);

    let mut killed: Vec<String> = plan.predicted_kill_panes.iter().map(|p| p.0.clone()).collect();
    killed.sort();
    let mut expected: Vec<String> = observed.panes.iter().map(|p| p.tmux_id.clone()).collect();
    expected.sort();
    assert_eq!(killed, expected);
}

#[test]
fn an_empty_plan_against_no_session_does_nothing() {
    let mut roster = cobalt(true);
    for person in &mut roster.people {
        person.desired_active = false;
    }
    let plan = compute_converge_plan(&desired(&roster), &empty_observed()).unwrap();
    assert!(plan.steps.is_empty());
    assert!(plan.predicted_kill_panes.is_empty());
}

// --- compute_converge_plan: fail closed ------------------------------------

#[test]
fn fails_closed_for_a_partially_ownership_tagged_window() {
    let d = desired(&cobalt(true));
    let mut observed = observe(&d);
    observed.windows.push(ObservedWindow {
        tmux_id: "@99".to_string(),
        organization_id: "cobalt".to_string(),
        logical_id: String::new(),
        protected_ui: false,
        sleeping_notice: false,
    });
    let err = compute_converge_plan(&d, &observed).unwrap_err();
    assert!(matches!(err, PlanErr::WindowNotFullyTagged { ref tmux_id, .. } if tmux_id == "@99"));
    assert!(err.to_string().contains("tmux window @99 is not fully ownership-tagged"));
}

#[test]
fn quarantines_a_partially_ownership_tagged_pane_without_failing_the_plan() {
    // #410: a stray untagged pane inside a tagged company window used to abort
    // the WHOLE plan (PaneNotFullyTagged), zeroing the company's actuation every
    // pass. It must now be quarantined: skipped, never actuated, and recorded as
    // a legible warning while the converged company still yields an empty plan.
    let d = desired(&cobalt(true));
    let observed_clean = observe(&d);
    let mut observed = observed_clean.clone();
    observed.panes.push(ObservedPane {
        tmux_id: "%x".to_string(),
        tmux_window_id: pwid("chief"),
        organization_id: String::new(),
        logical_window_id: String::new(),
        person_id: String::new(),
        launch_hash: String::new(),
        start_command: String::new(),
    });
    let plan = compute_converge_plan(&d, &observed).expect("a single stray pane is non-fatal");

    // Exactly the converged plan the clean observation would produce — the stray
    // pane adds NO step (never killed, never retagged, never adopted).
    let clean = compute_converge_plan(&d, &observed_clean).unwrap();
    assert_eq!(plan.steps, clean.steps);
    assert!(
        !plan.steps.iter().any(|s| step_targets_pane(s, "%x")),
        "the stray pane is never actuated"
    );

    // The signal is preserved as a warning naming the pane, keeping the
    // health-classifier phrase.
    assert_eq!(plan.warnings.len(), 1);
    assert!(plan.warnings[0].contains("%x"));
    assert!(plan.warnings[0].contains("not fully ownership-tagged"));
}

#[test]
fn a_stray_pane_does_not_block_a_managed_pane_from_converging() {
    // The load-bearing #410 regression: a managed pane that needs convergence
    // (a drifted launch hash -> Respawn) STILL converges when a stray untagged
    // pane shares its tagged window. Pre-fix this returned Err and yielded
    // nothing at all.
    let d = desired(&cobalt(true));
    let mut observed = observe(&d);
    // Drift one managed pane's launch hash so it must be respawned.
    let managed = observed
        .panes
        .iter_mut()
        .find(|p| p.tmux_window_id == pwid("chief"))
        .expect("executive has a managed pane");
    let managed_pane_id = managed.tmux_id.clone();
    let managed_person = managed.person_id.clone();
    managed.launch_hash = "hash-0".to_string();
    // A stray untagged pane in the same tagged window.
    observed.panes.push(ObservedPane {
        tmux_id: "%stray".to_string(),
        tmux_window_id: pwid("chief"),
        organization_id: String::new(),
        logical_window_id: String::new(),
        person_id: String::new(),
        launch_hash: String::new(),
        start_command: String::new(),
    });

    let plan =
        compute_converge_plan(&d, &observed).expect("a stray pane must not fail the whole plan");

    // The managed pane's convergence step is present...
    assert!(
        plan.predicted_respawn_persons.contains(&managed_person),
        "the managed pane still converges alongside the stray one"
    );
    assert!(
        plan.steps.iter().any(|s| step_targets_pane(s, &managed_pane_id)),
        "a Respawn/Retag targets the managed pane"
    );
    // ...and the stray pane is never touched.
    assert!(!plan.steps.iter().any(|s| step_targets_pane(s, "%stray")));
    assert!(plan.warnings.iter().any(|w| w.contains("%stray")));
}

/// Build the observed topology of a converged company plus one fully-untagged
/// orphan pane in the (managed) executive window, carrying `start_command` as
/// its only surviving evidence of ownership. Mirrors the TS real-tmux #64 test.
fn observed_with_orphan(d: &placement::Topology, orphan_start_command: &str) -> ObservedTopology {
    let mut observed = observe(d);
    observed.panes.push(ObservedPane {
        tmux_id: "%orphan".to_string(),
        tmux_window_id: pwid("chief"),
        organization_id: String::new(),
        logical_window_id: String::new(),
        person_id: String::new(),
        launch_hash: String::new(),
        start_command: orphan_start_command.to_string(),
    });
    observed
}

#[test]
fn reaps_the_untagged_orphan_of_a_departed_person() {
    // #64: the phantom-pane leak. A `split-window` signal-killed after setting
    // `ORG_LAUNCHER_PERSON=<person>` but before writing the ownership tags leaves
    // a live, fully-untagged pane. When that person is no longer desired
    // (`quant-benched-quant` is benched in the fixture), no owned-kill sees it
    // (it carries no tags) and adoption never claims it (not desired) — pre-fix
    // it was quarantined and survived forever. It must be REAPED.
    let d = desired(&cobalt(true));
    let observed = observed_with_orphan(
        &d,
        "/usr/bin/env ORG_LAUNCHER_PERSON=quant-benched-quant pi --tools read",
    );

    let plan = compute_converge_plan(&d, &observed).expect("reap is non-fatal");

    // The orphan is reaped: a KillPane targets it and it is a predicted kill.
    assert!(
        plan.steps.iter().any(|s| matches!(s, Step::KillPane { pane } if pane.0 == "%orphan")),
        "a KillPane reaps the orphan, {:?}",
        plan.steps,
    );
    assert!(plan.predicted_kill_panes.contains(&PaneId("%orphan".to_string())));
    // It is reaped, NOT quarantined — no leftover phantom warning.
    assert!(
        !plan.warnings.iter().any(|w| w.contains("%orphan")),
        "reaped, not quarantined, {:?}",
        plan.warnings,
    );
}

#[test]
fn does_not_reap_an_untagged_pane_naming_a_stranger() {
    // The #438 safety boundary: a pane whose env names someone who is NOT a
    // member of this org is a foreign/stray pane — quarantined, never killed.
    let d = desired(&cobalt(true));
    let observed = observed_with_orphan(&d, "/usr/bin/env ORG_LAUNCHER_PERSON=someone-who-left pi");

    let plan = compute_converge_plan(&d, &observed).expect("non-fatal");
    assert!(
        !plan.steps.iter().any(|s| step_targets_pane(s, "%orphan")),
        "stranger is never actuated"
    );
    assert!(plan
        .warnings
        .iter()
        .any(|w| w.contains("%orphan") && w.contains("not fully ownership-tagged")));
}

#[test]
fn does_not_reap_an_untagged_pane_naming_a_still_desired_person() {
    // A pane naming a person still in the running fleet is not an orphan; the
    // reap only ever takes a DEPARTED person's leaked pane. Quarantined, not killed.
    let d = desired(&cobalt(true));
    let observed = observed_with_orphan(&d, "/usr/bin/env ORG_LAUNCHER_PERSON=quant-head pi");

    let plan = compute_converge_plan(&d, &observed).expect("non-fatal");
    assert!(
        !plan.steps.iter().any(|s| step_targets_pane(s, "%orphan")),
        "a desired person's pane is never reaped"
    );
    assert!(plan.warnings.iter().any(|w| w.contains("%orphan")));
}

#[test]
fn fails_closed_when_one_person_has_two_ownership_tagged_panes() {
    let d = desired(&cobalt(true));
    let mut observed = observe(&d);
    observed.panes.push(ObservedPane {
        tmux_id: "%dup".to_string(),
        tmux_window_id: pwid("quant-active-quant"),
        organization_id: "cobalt".to_string(),
        logical_window_id: pw("quant-active-quant"),
        person_id: "quant-active-quant".to_string(),
        launch_hash: "hash-1".to_string(),
        start_command: String::new(),
    });
    let err = compute_converge_plan(&d, &observed).unwrap_err();
    assert!(err
        .to_string()
        .contains("Ambiguous duplicate organization person 'quant-active-quant'"));
}

#[test]
fn fails_closed_when_two_windows_claim_the_same_logical_window() {
    let d = desired(&cobalt(true));
    let mut observed = observe(&d);
    observed.windows.push(ObservedWindow {
        tmux_id: "@dup".to_string(),
        organization_id: "cobalt".to_string(),
        logical_id: pw("chief"),
        protected_ui: false,
        sleeping_notice: false,
    });
    let err = compute_converge_plan(&d, &observed).unwrap_err();
    assert!(err
        .to_string()
        .contains(&format!("Ambiguous duplicate organization window '{}'", pw("chief"))));
}

#[test]
fn fails_closed_when_a_pane_and_its_window_disagree_on_identity() {
    let d = desired(&cobalt(true));
    let mut observed = observe(&d);
    // A fully tagged pane in the CEO's window, but tagged for somebody else's.
    observed.panes.push(ObservedPane {
        tmux_id: "%mislabel".to_string(),
        tmux_window_id: pwid("chief"),
        organization_id: "cobalt".to_string(),
        logical_window_id: pw("quant-head"),
        person_id: "ghost".to_string(),
        launch_hash: "hash-1".to_string(),
        start_command: String::new(),
    });
    let err = compute_converge_plan(&d, &observed).unwrap_err();
    assert!(matches!(err, PlanErr::WindowPaneDisagree { .. }));
}

#[test]
fn refuses_a_session_tagged_for_a_different_organization() {
    let d = desired(&cobalt(true));
    let mut observed = observe(&d);
    observed.session_organization = "someone-else".to_string();
    let err = compute_converge_plan(&d, &observed).unwrap_err();
    assert!(matches!(err, PlanErr::SessionOwnership { .. }));
    let message = err.to_string();
    assert!(message.contains("ownership tag is 'someone-else'"));
    assert!(message.contains("expected 'cobalt'"));
}

// --- compute_converge_plan: ordering ---------------------------------------

#[test]
fn keeps_managed_windows_ordered_and_omits_ordering_for_a_single_window() {
    let plan = compute_converge_plan(&desired(&cobalt(true)), &empty_observed()).unwrap();
    let orders: Vec<Vec<String>> = plan
        .steps
        .iter()
        .filter_map(|s| {
            if let Step::OrderWindows { order } = s {
                Some(order.iter().map(window_logical).collect())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        orders,
        vec![vec![
            pw("chief"),
            pw("quant-head"),
            pw("quant-active-quant"),
            pw("quant-data-head"),
            pw("quant-data-active-data-engineer"),
            pw("it-head"),
        ]]
    );

    let solo_plan = compute_converge_plan(&desired(&solo()), &empty_observed()).unwrap();
    assert_eq!(count(&solo_plan, |s| matches!(s, Step::OrderWindows { .. })), 0);
}

#[test]
fn reorders_windows_only_when_the_observed_order_drifts() {
    // #367: at convergence the owned windows already sit in department order, so
    // OrderWindows is suppressed; only a genuinely scrambled observed order emits
    // it, and it does so without churning any pane tag.
    let d = desired(&cobalt(true)); // executive, quant, quant-data — in order

    let ordered = observe(&d);
    let converged = compute_converge_plan(&d, &ordered).unwrap();
    assert_eq!(count(&converged, |s| matches!(s, Step::OrderWindows { .. })), 0);
    assert!(converged.steps.is_empty(), "converged is empty, got {:?}", converged.steps);

    let mut scrambled = observe(&d);
    scrambled.windows.reverse(); // quant-data, quant, executive
    let plan = compute_converge_plan(&d, &scrambled).unwrap();
    assert_eq!(
        count(&plan, |s| matches!(s, Step::OrderWindows { .. })),
        1,
        "one reorder, {:?}",
        plan.steps
    );
    assert_eq!(
        count(&plan, |s| matches!(s, Step::Retag { .. })),
        0,
        "reorder alone retags nothing"
    );
    assert_eq!(count(&plan, |s| matches!(s, Step::ApplyLayout { .. })), 0, "membership unchanged");
    assert_eq!(plan.steps.len(), 1, "the single OrderWindows is the whole plan");
}

// --- TOMBSTONE: department-start-stagger semantics --------------------------
//
// Five tests lived here and all five are DELETED rather than weakened:
// `the_production_default_ramp_adds_no_delay_of_its_own`,
// `a_carried_offset_keeps_a_following_start_ramping_instead_of_restarting_at_zero`,
// and the three that pinned the accountant's arithmetic against an explicit
// `stagger_ms` and `concurrency`.
//
// The distinction matters. A test is weakened when the property it pins still
// exists and the assertion stops covering it. Here the property itself was
// RULED AGAINST -- "just boot them all at the same time" -- so `RampConfig`,
// `Admission`, `SpawnSpec::delay_ms` and `ConvergePlan::admission_ms` do not
// exist for a test to name. What replaced the whole section is one assertion
// in the opposite direction, `boots_every_missing_pane_in_one_pass_with_no_ramp_at_all`,
// which fails the moment any cap, batch or delay reappears.
//
// The two tests that pinned PARSING (`RampConfig::from_values`, bounds in both
// directions, and a prior batch never carried forward) were already not this
// crate's: they survive at the owner in
// `chiefd-core/src/runtime/actuation/tests.rs`. They die with the ramp there,
// not here.

// --- layout math -----------------------------------------------------------

#[test]
fn distributed_sizes_splits_with_a_one_cell_gap() {
    assert_eq!(distributed_sizes(10, 3).unwrap(), vec![3, 3, 2]);
    assert_eq!(distributed_sizes(80, 1).unwrap(), vec![80]);
    assert_eq!(distributed_sizes(5, 0).unwrap(), Vec::<i64>::new());
    assert!(distributed_sizes(2, 3).is_err(), "a window too small fails closed");
}

#[test]
fn organization_tmux_layout_lays_a_grid() {
    let single = organization_tmux_layout(80, 24, None, &["%1"]).unwrap();
    let checksum = single.split(',').next().unwrap();
    assert!(is_hex4(checksum), "layout is prefixed with a four-hex checksum");
    assert!(single.ends_with("80x24,0,0,1"));

    let two = organization_tmux_layout(80, 24, None, &["%1", "%2"]).unwrap();
    assert!(!two.contains('['), "two people stay side by side");

    let five = organization_tmux_layout(80, 24, None, &["%1", "%2", "%3", "%4", "%5"]).unwrap();
    assert!(five.contains('['), "five people are a grid, NOT a single row of slivers");

    let six =
        organization_tmux_layout(80, 24, None, &["%1", "%2", "%3", "%4", "%5", "%6"]).unwrap();
    assert!(six.contains('['), "six panes wrap into two balanced rows");
    assert!(is_hex4(six.split(',').next().unwrap()));
}

#[test]
fn organization_tmux_layout_fails_closed_on_bad_input() {
    assert!(organization_tmux_layout(0, 24, None, &["%1"]).is_err());
    assert!(organization_tmux_layout(80, 24, None, &[]).is_err());
    assert!(organization_tmux_layout(80, 2, None, &["%1", "%2", "%3", "%4", "%5", "%6"]).is_err());
    assert!(organization_tmux_layout(80, 24, None, &["1"]).is_err(), "a pane id must start with %");
}

// --- the shared placement golden -------------------------------------------

/// The fixture both sides of the P5 split are asserted against, byte for byte.
///
/// `chief-cli` computes the same geometry from the same cases; while both
/// implementations exist, this file is what holds them to one answer. See
/// `runtime/roster/tests.rs` for the roster and topology halves of the same
/// golden.
const PLACEMENT_GOLDEN: &str = include_str!("../../../../../tests/fixtures/placement-golden.json");

#[test]
fn the_golden_layouts_are_this_planners_own_geometry() {
    let golden: serde_json::Value =
        serde_json::from_str(PLACEMENT_GOLDEN).expect("the shared golden must be readable JSON");
    let cases = golden["layouts"].as_array().expect("the golden carries layout cases");
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
            organization_tmux_layout(width, height, None, &refs).expect("the golden cases lay out"),
            case["layout"].as_str().expect("a layout string"),
            "layout case {panes:?} at {width}x{height}"
        );
    }
}

// --- sleeping UI is a steady state ------------------------------------------

fn topology_with_sleeping_sales() -> placement::Topology {
    let mut roster = company();
    person(&mut roster, "sales-head").desired_active = false;
    person(&mut roster, "sales-w1").desired_active = false;
    person(&mut roster, "sales-w2").desired_active = false;
    desired(&roster)
}

fn add_retired_sales_window(observed: &mut ObservedTopology, protected_ui: bool) {
    observed.windows.push(ObservedWindow {
        tmux_id: "@sales".to_string(),
        organization_id: observed.session_organization.clone(),
        logical_id: "sales".to_string(),
        protected_ui,
        sleeping_notice: false,
    });
}

/// The sidebar owns a sleeping department window while its notice is visible.
/// That complete UI state is stable: it causes neither a kill nor order churn.
#[test]
fn a_sleeping_department_window_with_clean_chief_furniture_is_steady_state() {
    let desired = topology_with_sleeping_sales();
    let mut observed = observe(&desired);
    add_retired_sales_window(&mut observed, true);

    let plan = compute_converge_plan(&desired, &observed).expect("plan");
    assert!(plan.steps.is_empty(), "clean sleeping UI must converge: {:#?}", plan.steps);
}

/// Protection comes from exact Chief furniture, not from the window identity.
/// A managed window with no pane or protected UI is still stale and is reaped.
#[test]
fn a_real_empty_managed_window_is_still_killed() {
    let desired = topology_with_sleeping_sales();
    let mut observed = observe(&desired);
    add_retired_sales_window(&mut observed, false);

    let plan = compute_converge_plan(&desired, &observed).expect("plan");
    assert_eq!(killed_windows(&plan), vec!["@sales".to_string()]);
    assert_eq!(count(&plan, |step| matches!(step, Step::OrderWindows { .. })), 0);
}

/// A furniture marker with partial ownership is not trusted as protected UI.
/// Its pane stays quarantined, and the managed window remains eligible for the
/// interpreter's apply-time refusal.
#[test]
fn partial_furniture_stays_quarantined_and_does_not_protect_its_window() {
    let desired = topology_with_sleeping_sales();
    let mut observed = observe(&desired);
    add_retired_sales_window(&mut observed, false);
    observed.panes.push(ObservedPane {
        tmux_id: "%partial-sales".to_string(),
        tmux_window_id: "@sales".to_string(),
        organization_id: desired.organization.clone(),
        logical_window_id: String::new(),
        person_id: String::new(),
        launch_hash: String::new(),
        start_command: String::new(),
    });

    let plan = compute_converge_plan(&desired, &observed).expect("plan");
    assert_eq!(killed_windows(&plan), vec!["@sales".to_string()]);
    assert!(
        plan.warnings
            .iter()
            .any(|warning| { warning.contains("Quarantined stray tmux pane %partial-sales") }),
        "partial furniture must stay visible to quarantine: {:?}",
        plan.warnings
    );
    assert_eq!(count(&plan, |step| matches!(step, Step::KillPane { .. })), 0);
}

/// A SPENT PERSON WINDOW GOES WHOLE, and the frame in between never exists.
///
/// This replaces `the_last_live_person_becomes_sleeping_furniture_instead_of_
/// leaving_a_bare_rail`, which pinned `Step::ParkLastPane`: the last person in
/// a railed DEPARTMENT window had their pane respawned in place as that
/// department's sleeping body, so no command boundary could show the rail as
/// the window's only pane. There is no department window left to keep — the
/// window a person was alone in is THEIRS — so the same frame is answered by
/// killing the window rather than by furnishing it.
///
/// Killing the PANE first would be two commands and therefore two frames, and
/// the frame in between is a window that is nothing but a sidebar. Worse, it
/// would be permanent whenever the operator is looking at it, because
/// `interpret::kill_window` defers on the active window and `kill_pane` does
/// not — so the reap the operator blocks is the one that would have tidied up
/// after the pane they just lost.
#[test]
fn a_stopped_persons_window_is_killed_whole_rather_than_emptied() {
    let prior = company();
    let prior_desired = desired(&prior);
    let mut observed = observe(&prior_desired);
    // Every window has a rail, which is exactly why `protected_ui` cannot be
    // what spares a window from the reap any more.
    for window in &mut observed.windows {
        window.protected_ui = true;
    }

    let mut current = prior;
    for person_id in ["sales-head", "sales-w1", "sales-w2"] {
        person(&mut current, person_id).desired_active = false;
    }
    let plan = compute_converge_plan(&desired(&current), &observed).expect("plan");

    let expected: Vec<String> =
        ["sales-head", "sales-w1", "sales-w2"].iter().map(|id| pwid(id)).collect();
    assert_eq!(killed_windows(&plan), expected, "one window per stopped person: {:#?}", plan.steps);
    assert_eq!(
        count(&plan, |step| matches!(step, Step::KillPane { .. })),
        0,
        "no pane is killed on its own, so the rail-only frame never happens: {:#?}",
        plan.steps
    );
    // The kill is still REPORTED as a pane going away, because that is what the
    // actuator's crash-loop and round counters read.
    assert_eq!(
        plan.predicted_kill_panes,
        vec![PaneId("%sales-head".into()), PaneId("%sales-w1".into()), PaneId("%sales-w2".into())]
    );
    // And nobody else is touched.
    assert!(
        !killed_windows(&plan).iter().any(|window| window == &pwid("casey")),
        "the CEO's window survives: {:#?}",
        plan.steps
    );
}

/// A person whose pane is NOT alone in its window still loses only the pane.
///
/// This is the transitional shape — a pane observed in some other window, which
/// converge is mid-way through repairing — and the whole-window kill must not
/// take a colleague down with it.
#[test]
fn a_stopped_person_sharing_a_window_loses_only_their_pane() {
    let prior = company();
    let prior_desired = desired(&prior);
    let mut observed = observe(&prior_desired);
    // Put `sales-w1` in `sales-head`'s window, as an interrupted move would.
    let borrowed = observed
        .panes
        .iter_mut()
        .find(|pane| pane.person_id == "sales-w1")
        .expect("the sales worker");
    borrowed.tmux_window_id = pwid("sales-head");
    borrowed.logical_window_id = pw("sales-head");

    let mut current = prior;
    person(&mut current, "sales-w1").desired_active = false;
    let plan = compute_converge_plan(&desired(&current), &observed).expect("plan");

    assert!(
        plan.steps
            .iter()
            .any(|step| matches!(step, Step::KillPane { pane } if pane.0 == "%sales-w1")),
        "a pane sharing a window is killed on its own: {:#?}",
        plan.steps
    );
    assert!(
        !killed_windows(&plan).iter().any(|window| window == &pwid("sales-head")),
        "and the colleague's window is not taken with it: {:#?}",
        plan.steps
    );
}

/// A window whose person is gone is reaped EVEN THOUGH it holds clean rail
/// furniture, which is the one exemption a person window may not have.
///
/// `a_sleeping_department_window_with_clean_chief_furniture_is_steady_state`
/// above is the case the exemption exists for and is unchanged. This is its
/// boundary: every window has a rail, so an exemption keyed on furniture alone
/// would leave one rail-only leftover per person who has ever been up.
#[test]
fn a_railed_person_window_whose_person_is_gone_is_still_reaped() {
    let prior = company();
    let prior_desired = desired(&prior);
    let mut observed = observe(&prior_desired);
    for window in &mut observed.windows {
        window.protected_ui = true;
    }
    // The pane is already gone — only the railed shell is left, which is what a
    // pass after a crash observes.
    observed.panes.retain(|pane| pane.person_id != "sales-w2");

    let mut current = prior;
    person(&mut current, "sales-w2").desired_active = false;
    let plan = compute_converge_plan(&desired(&current), &observed).expect("plan");

    assert_eq!(
        killed_windows(&plan),
        vec![pwid("sales-w2")],
        "the leftover shell is reaped: {:#?}",
        plan.steps
    );
}

// --- the focus window is permanent (Stage 4) --------------------------------

/// The observed topology of `desired`, plus the session's PARKED focus window:
/// it exists, it is tagged `__focus__`, and nobody is in it, because the
/// operator is looking at a department.
///
/// This is what a session looks like for most of its life once the focus window
/// stopped being minted and reaped per gesture. The window is real — it holds a
/// rail and a standing notice — but neither of those is a PERSON pane, so
/// nothing about it appears in `panes`.
fn observe_with_parked_focus(desired: &placement::Topology) -> ObservedTopology {
    let mut observed = observe(desired);
    observed.windows.push(ObservedWindow {
        tmux_id: "@focus".to_string(),
        organization_id: desired.organization.clone(),
        logical_id: placement::FOCUS_WINDOW_ID.to_string(),
        protected_ui: true,
        sleeping_notice: false,
    });
    observed
}

/// A COLD CLICK MINTS THE PERSON'S OWN WINDOW, and the card stays a card.
///
/// This replaces `a_cold_focus_claims_its_existing_body_and_never_fails_on_the_
/// absent_empty_home`, which pinned `Step::ClaimWakingFocus`: the rail painted
/// "… is starting" into the permanent focus body and converge respawned the
/// person's process into that very pane, so the cell the operator clicked
/// became the person's pane with no second pane in between.
///
/// It worked because the focus window was where that person's pane was going to
/// LIVE. One window per person means it is not, and a claim that put them there
/// would be a pane converge immediately wanted somewhere else — which is the
/// move, and therefore the resize, this whole model deletes. The waking body
/// stays furniture the rail owns, the person is minted in a window of their
/// own, and `brain::finish_pending_zoom` selects it when the pane turns up.
#[test]
fn a_cold_click_mints_the_persons_own_window_and_leaves_the_card_alone() {
    let desired = placement::Topology {
        organization: "cobalt".into(),
        session: FIXTURE_SESSION.into(),
        windows: vec![placement::Window {
            logical_id: pw("eli"),
            name: "Eli".into(),
            panes: vec![placement::Pane {
                person_id: "eli".into(),
                launch_hash: "eli-hash".into(),
                order: 1,
            }],
        }],
        known_person_ids: ["eli".to_owned()].into_iter().collect(),
    };
    // The session as a cold click leaves it: one card window, holding a rail
    // and the waking body, and no person pane anywhere.
    let observed = ObservedTopology {
        session_exists: true,
        session_organization: "cobalt".into(),
        windows: vec![ObservedWindow {
            tmux_id: "@focus".into(),
            organization_id: "cobalt".into(),
            logical_id: placement::FOCUS_WINDOW_ID.into(),
            protected_ui: true,
            sleeping_notice: false,
        }],
        panes: Vec::new(),
    };

    let plan = compute_converge_plan(&desired, &observed).expect("cold click plan");
    assert_eq!(
        plan.steps,
        vec![
            Step::CreateWindowWithSpawn {
                w: WindowSym(pw("eli")),
                name: "Eli".into(),
                first: SpawnSpec { person_id: "eli".into(), launch_hash: "eli-hash".into() },
            },
            Step::ApplyLayout {
                w: WindowRef::Created(WindowSym(pw("eli"))),
                panes: vec![PaneRef::Created("eli".into())],
                retire_sleeping_notice: false,
            },
        ],
        "one window, minted around the person, and nothing done to the card: {:#?}",
        plan.steps
    );
    assert_eq!(
        killed_windows(&plan),
        Vec::<String>::new(),
        "the rail's card window is never converge's business"
    );

    // And the pass after the pane arrives is empty: no second launch, no move.
    let settled = ObservedTopology {
        panes: vec![ObservedPane {
            tmux_id: "%755".into(),
            tmux_window_id: "@eli".into(),
            organization_id: "cobalt".into(),
            logical_window_id: pw("eli"),
            person_id: "eli".into(),
            launch_hash: "eli-hash".into(),
            start_command: "chiefd-pane-startup".into(),
        }],
        windows: vec![
            observed.windows[0].clone(),
            ObservedWindow {
                tmux_id: "@eli".into(),
                organization_id: "cobalt".into(),
                logical_id: pw("eli"),
                protected_ui: false,
                sleeping_notice: false,
            },
        ],
        ..observed
    };
    let repeat = compute_converge_plan(&desired, &settled).expect("settled plan");
    assert!(repeat.steps.is_empty(), "the next pass must not launch Pi again: {:#?}", repeat.steps);
}

/// THE CEO IS NOT SPARED BY BEING THE CEO, and the card window is not touched.
///
/// Replaces `final_person_parking_never_converts_focus_or_the_ceo_into_
/// department_furniture`. The claim that survives is the one about the CARD
/// window: a person pane observed inside `__focus__` — which a half-finished
/// upgrade can still leave — is killed on its own, and the card window is not
/// reaped with it, because that window is the rail's for the life of the
/// session.
#[test]
fn a_person_pane_stranded_in_the_card_window_is_killed_without_taking_it() {
    let prior = company();
    let prior_desired = desired(&prior);
    let mut observed = observe(&prior_desired);
    observed.windows.push(ObservedWindow {
        tmux_id: "@focus".to_string(),
        organization_id: prior_desired.organization.clone(),
        logical_id: placement::FOCUS_WINDOW_ID.to_string(),
        protected_ui: true,
        sleeping_notice: false,
    });
    let stranded = observed
        .panes
        .iter_mut()
        .find(|pane| pane.person_id == "research-w1")
        .expect("research worker");
    stranded.tmux_window_id = "@focus".to_string();
    stranded.logical_window_id = placement::FOCUS_WINDOW_ID.to_string();

    let mut current = prior;
    person(&mut current, "research-w1").desired_active = false;
    let plan = compute_converge_plan(&desired(&current), &observed).expect("plan");

    assert!(
        plan.steps
            .iter()
            .any(|step| matches!(step, Step::KillPane { pane } if pane.0 == "%research-w1")),
        "the stranded pane goes on its own: {:#?}",
        plan.steps
    );
    assert!(
        !killed_windows(&plan).contains(&"@focus".to_owned()),
        "and the rail's card window stays: {:#?}",
        plan.steps
    );
    assert!(
        !plan
            .steps
            .iter()
            .any(|step| matches!(step, Step::KillPane { pane } if pane.0 == "%casey")),
        "the CEO is not touched: {:#?}",
        plan.steps
    );
}

/// THE STAGE 4 RULE: converge never destroys the focus window.
///
/// It used to be minted by a person click and killed by the next department
/// click, so every navigation churned a window and a rail process. Now the brain
/// mints it once per session and it stays, holding the person the operator is
/// looking at or a standing notice saying nobody is being looked at.
///
/// The planner is where that has to be true, because the planner is what aims
/// the reap: `kill_window` DEFERRED this kill whenever the window happened to
/// hold rail furniture, which made the window's survival a side effect of its
/// contents rather than a property of the design.
///
/// Proved RED against the previous planner: it emitted
/// `KillWindow { w: Observed("@focus") }` here.
#[test]
fn the_parked_focus_window_is_never_reaped_however_long_nobody_is_focused() {
    let roster = company();
    let now = desired(&roster);
    let observed = observe_with_parked_focus(&now);

    let plan = compute_converge_plan(&now, &observed).expect("plan");

    assert!(
        !killed_windows(&plan).iter().any(|id| id == "@focus"),
        "the focus window is the session's one permanent view artifact and converge must \
         never aim a reap at it, but the plan was: {:?}",
        plan.steps
    );
}

/// And the whole company still converges to an EMPTY plan with it standing
/// there — no reap, and no `OrderWindows` either.
///
/// The ordering half is its own defect and would be invisible without this
/// assertion: `desired.windows` holds no `__focus__` while nobody is focused, so
/// an unfiltered comparison reads `[executive, engineering, sales, research]`
/// against `[…, __focus__]`, differs on every single pass, and emits a
/// `move-window` sequence for every window in the session for ever.
///
/// Proved RED against the previous planner: one `KillWindow` and one
/// `OrderWindows`, on a company where nothing at all had changed.
#[test]
fn a_session_holding_a_parked_focus_window_converges_to_an_empty_plan() {
    let roster = company();
    let now = desired(&roster);

    let plan = compute_converge_plan(&now, &observe_with_parked_focus(&now)).expect("plan");

    assert!(
        plan.steps.is_empty(),
        "a converged company with its permanent focus window parked must emit nothing, but \
         got: {:?}",
        plan.steps
    );
}

/// A DEPARTMENT'S OVERVIEW SURVIVES CONVERGE, and this is what kept the card
/// off the glass entirely.
///
/// The card is minted by the rail on a click and placement has never heard of
/// it, so the reap read it as a stray window and killed it — measured on
/// a live box, once per click, so the operator clicked a department and nothing
/// happened. It gets the same exemption the focus window has, for the same
/// reason: its lifetime belongs to the surface that minted it, and
/// `effects::close_sleeping_notices` retires it when its department leaves the
/// roster.
#[test]
fn a_department_overview_window_is_never_reaped_by_converge() {
    let roster = company();
    let now = desired(&roster);
    let mut observed = observe(&now);
    observed.windows.push(ObservedWindow {
        tmux_id: "@overview".to_string(),
        organization_id: now.organization.clone(),
        logical_id: placement::overview_window_id("sales"),
        protected_ui: true,
        sleeping_notice: false,
    });

    let plan = compute_converge_plan(&now, &observed).expect("plan");

    assert!(
        killed_windows(&plan).is_empty(),
        "converge must not reap a window the rail owns: {:?}",
        plan.steps
    );
    assert!(
        !plan.steps.iter().any(|step| format!("{step:?}").contains("overview")),
        "and must not order or re-lay it either: {:?}",
        plan.steps
    );
}

/// A spent PERSON window is STILL reaped while the rail's card window stands
/// beside it. The exemption is for one logical id and is not a hole in the reap.
#[test]
fn the_focus_exemption_does_not_shelter_a_spent_person_window_beside_it() {
    let roster = company();
    let before = desired(&roster);
    let observed = observe_with_parked_focus(&before);

    let mut after = roster;
    person(&mut after, "sales-head").desired_active = false;
    person(&mut after, "sales-w1").desired_active = false;
    person(&mut after, "sales-w2").desired_active = false;
    let now = desired(&after);

    let plan = compute_converge_plan(&now, &observed).expect("plan");

    assert_eq!(
        killed_windows(&plan),
        vec![pwid("sales-head"), pwid("sales-w1"), pwid("sales-w2")],
        "every stopped person's window dies and the card window does not"
    );
}

/// The reap ends the churn: once the zombie window is gone, the same company
/// converges to an EMPTY plan. This is the regression the focus-only scope
/// left open for department windows.
#[test]
fn a_company_whose_zombie_window_was_reaped_converges_to_an_empty_plan() {
    let mut roster = company();
    person(&mut roster, "sales-head").desired_active = false;
    person(&mut roster, "sales-w1").desired_active = false;
    person(&mut roster, "sales-w2").desired_active = false;
    let now = desired(&roster);
    let plan = compute_converge_plan(&now, &observe(&now)).expect("plan");
    assert!(plan.steps.is_empty(), "converged after the reap, but got: {:?}", plan.steps);
}

// --- one window per person, at the planner ---------------------------------
//
// TOMBSTONE, twice over. An earlier set pinned a `@chief_sidebar_focus` tmux
// option; `a1a7aca9f` deleted it because every click landed the operator on the
// CEO. The set that replaced it pinned `focus: Option<&str>` reaching this
// planner, so a click produced `Step::CreateWindowByMove` into `__focus__` and
// the department kept its window. Both are gone for the same reason, stated by
// the operator on 2026-08-21 while watching their own screen recording: *"when
// I click on an agent I want it should be in the final position, right? Why is
// it going half screen and growing?"*
//
// A pane that changes window changes width, and a Pi whose pane changes width
// repaints its whole scrollback. Every one of those designs was an attempt to
// make that transition acceptable. There is no such transition now: a click
// reaches tmux as `select-window` and reaches this planner not at all.

/// THE CLICK PRODUCES NO STEPS, because the click is not an input.
///
/// `a_gesture_the_rail_already_performed_plans_absolutely_nothing` made this
/// claim about a rail that had already broken a pane out, which required the
/// planner to agree with a move it could otherwise have undone within its
/// cadence. The claim is unconditional now: a converged session plans nothing
/// whoever the operator is looking at, because there is nothing a selection
/// could change.
#[test]
fn a_converged_session_plans_nothing_whoever_the_operator_clicks() {
    let roster = company();
    let now = desired(&roster);
    let observed = observe_with_parked_focus(&now);
    let plan = compute_converge_plan(&now, &observed).expect("plan");
    assert!(
        plan.steps.is_empty(),
        "a click cannot move a pane if placement offers nowhere to move it to: {:#?}",
        plan.steps
    );
}

/// #1207: RESTARTING MUST NEVER BE WORSE THAN STAYING DEAD.
///
/// The supervisor restarts the actuator on every death, for ever, so a
/// destructive first pass would not be one bad minute — at the ceiling it would
/// kill and re-mint the whole company every ten seconds, which is a far worse
/// product than the dead pane it replaced.
///
/// It cannot, and this is the proof. A restarted actuator is by definition one
/// with EMPTY in-process state: a fresh `EverObserved`, a fresh `CrashLoop`,
/// no memory of anything. The plan is computed from what it OBSERVES and never
/// from what it remembers, so meeting a company whose every person is already
/// materialised, it plans nothing at all — no kill, no spawn, no respawn.
///
/// `EverObserved` starting empty can only ever REFUSE a kill it would otherwise
/// allow, which is the direction that fails toward keeping a person's pane.
#[test]
fn a_restarted_actuator_adopts_the_company_it_finds() {
    let roster = company();
    let now = desired(&roster);
    // The world a restarted actuator wakes up to: every desired person already
    // has their pane, exactly as the predecessor left it.
    let observed = observe(&now);

    let plan = compute_converge_plan(&now, &observed).expect("plan");

    assert_eq!(
        count(&plan, |step| matches!(step, Step::KillPane { .. } | Step::KillWindow { .. })),
        0,
        "a restart must not kill anybody it finds alive: {:#?}",
        plan.steps
    );
    assert_eq!(
        count(&plan, |step| matches!(
            step,
            Step::CreateWindowWithSpawn { .. } | Step::SplitPane { .. } | Step::Respawn { .. }
        )),
        0,
        "and must not start a second copy of anybody already running: {:#?}",
        plan.steps
    );
    assert!(
        plan.steps.is_empty(),
        "a company that is already converged is nothing to do, whoever just booted: {:#?}",
        plan.steps
    );
}

/// EVERY PERSON'S PANE IS IN A WINDOW OF ITS OWN, and converge is what puts it
/// there — by MOVING it, once, on the pass that discovers the old shape.
///
/// This is the upgrade path: a session laid out by the previous model holds a
/// tiled department window, and every person in it must end up alone. The move
/// is `CreateWindowByMove` and never a respawn, because the launch hash has not
/// changed and nobody's process may be restarted to change their geometry.
#[test]
fn a_session_laid_out_by_the_old_model_is_moved_one_person_per_window() {
    let roster = company();
    let now = desired(&roster);
    // The world as the department model left it: one window per department,
    // every colleague tiled into it.
    let mut observed = observe(&now);
    observed.windows.retain(|window| window.logical_id == pw("casey"));
    observed.windows[0].tmux_id = "@engineering".into();
    observed.windows[0].logical_id = "engineering".into();
    for pane in &mut observed.panes {
        pane.tmux_window_id = "@engineering".into();
        pane.logical_window_id = "engineering".into();
    }

    let plan = compute_converge_plan(&now, &observed).expect("plan");
    let minted: Vec<String> = plan
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::CreateWindowByMove { w: WindowSym(id), .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let expected: Vec<String> = roster.people.iter().map(|person| pw(&person.id)).collect();
    assert_eq!(minted, expected, "every person is moved into their own window: {:#?}", plan.steps);
    assert_eq!(
        count(&plan, |step| matches!(step, Step::Respawn { .. })),
        0,
        "nobody's process is restarted to change their geometry: {:#?}",
        plan.steps
    );
    // And the pass after it is empty.
    let settled = compute_converge_plan(&now, &observe(&now)).expect("settled plan");
    assert!(
        settled.steps.is_empty(),
        "the completed move does not flip back: {:#?}",
        settled.steps
    );
}

/// A window minted by move is NAMED FOR THE PERSON, so the operator reads their
/// agents' names in the tmux window list.
#[test]
fn a_person_window_is_named_for_the_person() {
    let roster = solo();
    let now = desired(&roster);
    let plan = compute_converge_plan(&now, &empty_observed()).expect("plan");
    assert!(
        plan.steps.iter().any(|step| matches!(
            step,
            Step::CreateSession { first } if first.person_id == "chief"
        )),
        "{:#?}",
        plan.steps
    );
    assert_eq!(now.windows[0].logical_id, pw("chief"));
    assert_eq!(now.windows[0].name, "chief", "the person's display name");
}
