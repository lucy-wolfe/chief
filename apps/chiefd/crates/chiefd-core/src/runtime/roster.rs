//! The client-agnostic desired roster: WHO exists and WHO should be running.
//!
//! # The only published answer to "who runs"
//!
//! There used to be a second one beside it. The old topology planner answered
//! the same underlying question and then made a *presentation* decision on top
//! of it — it grouped people into the operator client's windows, named those
//! windows, dropped the departments that would render empty, and stamped a
//! terminal session name onto the result. That grouping is a client's decision,
//! not the backend's, so the planner is gone (#751/P10): chiefd decides WHO
//! runs, a client decides WHERE it is shown.
//!
//! This module publishes the half chiefd owns, and now the only half it has.
//! Every field below is a FACT about the company: an id, a name, an ordering
//! the operator chose, a structural relationship, or a decision chiefd
//! genuinely made about whether a person should be running. Nothing here names
//! a session, a window, a pane, a socket or a layout, and nothing here is
//! derivable only by the terminal client.
//!
//! # The one rule that is NOT duplicated
//!
//! `desiredActive` is [`is_desired_person`](super::desired::is_desired_person)
//! — chiefd's single implementation of the policy, not a second reading of the
//! same columns. That predicate folds together the paused-subtree walk, the
//! employment state, the head-of-a-paused-department rule and the
//! `handoff-required` override. A second implementation of it is exactly the
//! failure this program keeps producing: `apps/api` re-derived it in TypeScript
//! against the wrong field name, concluded nobody was ever desired, and no
//! agent launched for weeks while every suite stayed green.
//!
//! # What a client derives from this, and why chiefd does not
//!
//! Pane placement is `head-in-parent`: a department head's pane belongs in the
//! PARENT department's window, everyone else's in their own. chiefd used to
//! compute that rule AND *persist* its answer, which is how a display decision
//! came to be durable state — and a durably STALE one, rewritten only when the
//! activity ledger was. #751-P9 deleted the rule, both its columns and every
//! caller. It is not published here and there is no longer anything to
//! publish. A client has everything it needs to derive it against the CURRENT
//! tree: [`RosterPerson::is_head_of`] names the department a person heads, and
//! [`RosterDepartment::parent_department_id`] plus
//! [`DesiredRoster::root_department_id`] resolve where that department sits.

use serde::Serialize;

use crate::runtime::desired::{self as plan, is_desired_person};
use crate::runtime::project::{project_activity_from_ledger, project_manifest};
use crate::store::activity::ActivityLedger;
use crate::store::organization::{OrganizationManifest, UnitState};

/// The company a roster belongs to.
///
/// The slug only. The terminal client mints its session name from it
/// (`org-<slug>`, the rule `organization_rows.rs` uses when it derives the
/// company's session), a browser uses it as a route segment, and chiefd has no
/// opinion about either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterCompany {
    /// The company slug.
    pub slug: String,
    /// The company's display name.
    pub display_name: String,
}

/// One department, as a fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterDepartment {
    /// Hierarchical department id.
    pub id: String,
    /// Display name. The terminal client uses it as a window name and applies
    /// its own sanitization; a browser renders it as a heading. Which of those
    /// happens is not chiefd's business, so the name is published raw.
    pub name: String,
    /// The parent department, or `null` for the root. Explicitly serialized
    /// rather than omitted: "this department has no parent" is the fact that
    /// terminates a client's ancestry walk, and an absent key would make a
    /// truncated response indistinguishable from the root.
    pub parent_department_id: Option<String>,
    /// The person who heads it.
    pub head_person_id: String,
    /// Index in the company's canonical department order. The operator chose
    /// this ordering; it is an org fact, not a layout.
    pub order: usize,
    /// `active` or `paused`.
    pub state: String,
}

/// One person, as a fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterPerson {
    /// Stable person id.
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Job title.
    pub title: String,
    /// The department this person currently works in — the ASSIGNED one. A
    /// client shows people where they are working.
    pub department_id: String,
    /// The department this person heads, or `null` if they head none. Both
    /// halves of the fact in one field: whether they are a head, and which
    /// department it is. A client that places a head relative to their
    /// department's parent needs the id, not a boolean.
    pub is_head_of: Option<String>,
    /// Index in the company's canonical person order.
    pub display_order: usize,
    /// chiefd's own answer to "should this person be running right now".
    ///
    /// This is the decision the backend owns, and the reason the response is
    /// worth asking for at all. A person who is not desired still appears in
    /// the roster: a client needs the full membership to tell its OWN departed
    /// person's leaked process from a stranger's.
    pub desired_active: bool,
    /// `active`, `benched` or `departed` — the EMPLOYMENT fact, which
    /// [`RosterPerson::desired_active`] cannot carry.
    ///
    /// "Not desired" is equally true of a benched person, of a person in a
    /// paused department, and of somebody who settled a minute ago; all three
    /// come back. `departed` is the one that does not, and no client can tell it
    /// from the other three unless it is told. The terminal rail draws a
    /// company's sleeping people and hides its departed ones, while the reap
    /// still needs the departed row to tell its own leaked pane from a
    /// stranger's — one fact, two readers, so it is published and never
    /// inferred.
    pub employment_state: String,
}

/// The complete client-agnostic roster for one company.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesiredRoster {
    /// The company this roster belongs to.
    pub company: RosterCompany,
    /// The root department's id.
    pub root_department_id: String,
    /// Every department, in the company's canonical department order.
    pub departments: Vec<RosterDepartment>,
    /// Every person, in the company's canonical person order — desired or not.
    pub people: Vec<RosterPerson>,
}

/// Project a committed manifest plus the runtime ledgers into the roster facts.
///
/// `activity` is `None` for a company that has never converged; every person
/// then carries no decision, which is precisely the case
/// [`is_desired_person`] already answers ("no decision" defaults to desired,
/// subject to the roster and paused-subtree filters). There is no second
/// "everybody" branch, because a second branch is a second rule.
///
/// Ordering is the manifest's own (`department_order`, `people_order`), never
/// map iteration order: the canonical orders are what make two reads of an
/// unchanged company byte-identical, and a `BTreeMap` walk would silently
/// re-sort a company's departments alphabetically.
#[must_use]
pub fn project_desired_roster(
    org: &OrganizationManifest,
    activity: Option<&ActivityLedger>,
) -> DesiredRoster {
    let manifest = project_manifest(org);
    let snapshot = match activity {
        Some(ledger) => project_activity_from_ledger(org, ledger),
        None => plan::ActivitySnapshot {
            organization: org.slug.clone(),
            people: std::collections::BTreeMap::new(),
        },
    };

    let departments = org
        .department_order
        .iter()
        .enumerate()
        .filter_map(|(order, id)| {
            let unit = org.departments.get(id)?;
            Some(RosterDepartment {
                id: unit.id.clone(),
                name: unit.name.clone(),
                parent_department_id: unit.parent_department_id.clone(),
                head_person_id: unit.head_person_id.clone(),
                order,
                state: match unit.state {
                    UnitState::Active => "active".to_owned(),
                    UnitState::Paused => "paused".to_owned(),
                },
            })
        })
        .collect();

    let people = org
        .people_order
        .iter()
        .enumerate()
        .filter_map(|(display_order, id)| {
            let person = org.people.get(id)?;
            let planned = manifest.people.get(id)?;
            Some(RosterPerson {
                id: person.id.clone(),
                display_name: person.name.clone(),
                title: person.title.clone(),
                department_id: person.department_id.clone(),
                is_head_of: org.headed_department(id).map(|unit| unit.id.clone()),
                display_order,
                desired_active: is_desired_person(&manifest, planned, &snapshot),
                employment_state: format!("{:?}", person.employment_state).to_lowercase(),
            })
        })
        .collect();

    DesiredRoster {
        company: RosterCompany { slug: org.slug.clone(), display_name: org.name.clone() },
        root_department_id: org.root_department_id.clone(),
        departments,
        people,
    }
}

#[cfg(test)]
mod tests;
