//! `POST /v1/org/roster/desired`, as this client reads it.
//!
//! # Re-derived from the wire, never imported
//!
//! chiefd serves this body from `chiefd_core::runtime::roster::DesiredRoster`.
//! These types are a SECOND declaration of the same shape, written against the
//! JSON rather than against the Rust, because `chief-cli` links none of
//! `chiefd-core`, `chiefd-host`, `chiefd-api` or `chiefd`
//! (`scripts/test/backend-tmux-boundary.test.mjs` rule 7). That is the same
//! decision the binary's `document_key` already made about the composite key:
//! a client composes the contract from published facts, and a fixed vector in
//! a test is what keeps the two declarations honest.
//!
//! # What is NOT here, and must not be wanted
//!
//! There is no `paneDepartmentId`. chiefd computed that rule and persisted the
//! answer as `last_pane_department_id`, but it is a PLACEMENT decision, it was
//! rewritten only when the activity ledger was, and it was therefore stale from
//! the moment anybody moved. [`crate::placement::pane_department_id`] derives
//! it instead, from the person's own [`RosterPerson::department_id`] — which
//! tracks the current tree.
//! `chiefd-core/src/runtime/roster/tests.rs` pins the divergence from the
//! other side.
//!
//! # `desiredActive` is consumed, never re-derived
//!
//! Who should be running is chiefd's decision and it is already made:
//! `desiredActive` IS `is_desired_person`, which folds together the
//! paused-subtree walk, the employment state, the head-of-a-paused-department
//! rule and the `handoff-required` override. A client that re-read those
//! columns would be the second implementation of a predicate this program has
//! already been burned by duplicating — `apps/api` re-derived it in TypeScript
//! against a field name chiefd never wrote, concluded nobody was ever desired,
//! and launched no agent for weeks while every suite stayed green.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The company a roster belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterCompany {
    /// The company slug. The session name is minted from it.
    pub slug: String,
    /// The company's display name.
    pub display_name: String,
}

/// One department, as a fact.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterDepartment {
    /// Hierarchical department id. This is the window's logical id.
    pub id: String,
    /// Display name, published RAW. Sanitizing it for a terminal is this
    /// client's job ([`crate::placement::safe_window_name`]) and a browser
    /// renders the same string as a heading.
    pub name: String,
    /// The parent department, or `None` for the root.
    pub parent_department_id: Option<String>,
    /// The person who heads it.
    pub head_person_id: String,
    /// Index in the company's canonical department order.
    ///
    /// READ THIS FIELD, never the array position. The order chiefd publishes
    /// is the store's depth-first walk (`preorder_departments`); a hand-built
    /// fixture that happens to be in insertion order agrees with it by
    /// accident and stops agreeing the moment a department is nested.
    pub order: usize,
    /// `active` or `paused`. Carried for display only: whether a paused
    /// department's people should be running is already folded into
    /// [`RosterPerson::desired_active`].
    pub state: String,
}

/// One person, as a fact.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterPerson {
    /// Stable person id.
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Job title.
    pub title: String,
    /// The department this person WORKS in — the assigned one.
    pub department_id: String,
    /// The department this person heads, or `None`. Both halves of the fact in
    /// one field: whether they are a head, and which department it is.
    pub is_head_of: Option<String>,
    /// Index in the company's canonical person order. Pane order within a
    /// window is this, ascending.
    pub display_order: usize,
    /// chiefd's own answer to "should this person be running right now".
    pub desired_active: bool,
    /// `active`, `benched` or `departed`.
    ///
    /// NOT the same question as [`RosterPerson::desired_active`]: a benched
    /// person, a person in a paused department and somebody who settled a minute
    /// ago are all undesired and all coming back. Only `departed` is final, and
    /// only this field says so.
    ///
    /// # Why this one field defaults, when nothing else here does
    ///
    /// Which of `active` / `benched` / `departed` this person is.
    ///
    /// REQUIRED, with no default. It briefly carried
    /// `#[serde(default = "employed")]` so a freshly built `chief` could talk to
    /// a chiefd started before the field existed — and that is a compatibility
    /// fallback, which `AGENTS.md` rules out by name: "Do not preserve backward
    /// compatibility. Remove obsolete paths instead of adding compatibility
    /// layers, fallbacks, or migrations."
    ///
    /// It was also the wrong default for THIS field. Absent meant `active`, so a
    /// daemon that did not send it made every FIRED person clickable in the
    /// rail — the one outcome the operator ruled out outright ("we never see
    /// fired employees"). A missing field is now a decode refusal, which is
    /// loud, and the fix for it is to restart a daemon that is out of date
    /// rather than to keep a shape alive that lets a departed person through.
    pub employment_state: String,
}

// TOMBSTONE: `employed()`, the serde default for `employment_state`, deleted
// 2026-08-14. It made a missing field read as `active` so a freshly built
// `chief` could talk to a chiefd started before the field existed — a
// compatibility fallback, which `AGENTS.md` rules out by name, and one that
// defaulted in the UNSAFE direction: absent meant active, so an out-of-date
// daemon put every FIRED person back in the rail. The field is required now and
// its absence is a loud decode refusal.

/// The employment state of somebody who has left the company.
pub const DEPARTED: &str = "departed";

impl RosterPerson {
    /// Has this person left the company?
    ///
    /// The rail draws every OTHER person, awake or asleep, because a sleeping
    /// person is a state the operator acts on. A departed one is not a state —
    /// they are gone, and the operator said so: "we never see fired employees".
    #[must_use]
    pub fn departed(&self) -> bool {
        self.employment_state == DEPARTED
    }
}

/// The complete client-agnostic roster for one company.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Roster {
    /// The company this roster belongs to.
    pub company: RosterCompany,
    /// The root department's id.
    pub root_department_id: String,
    /// Every department, in the company's canonical department order.
    pub departments: Vec<RosterDepartment>,
    /// Every person — desired or not.
    pub people: Vec<RosterPerson>,
}

/// Why a roster could not be turned into a topology.
///
/// Every variant is a roster that does not hold together. The client fails
/// CLOSED on all of them, exactly as `desired_topology` does on an activity
/// snapshot that does not cover its manifest: a topology computed from a
/// half-read roster would name windows for departments that do not exist and,
/// worse, would silently omit people — and an actuator reads an omission as
/// "stop them".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RosterError {
    /// Two departments, or two people, share an id.
    #[error("roster names {kind} '{id}' twice")]
    DuplicateId {
        /// `department` or `person`.
        kind: &'static str,
        /// The repeated id.
        id: String,
    },
    /// Two departments, or two people, claim the same ordinal.
    #[error("roster gives two {kind}s the same order {order}")]
    DuplicateOrder {
        /// `department` or `person`.
        kind: &'static str,
        /// The repeated ordinal.
        order: usize,
    },
    /// A department id was referenced but never declared.
    #[error("{referrer} references department '{department}', which the roster does not declare")]
    UnknownDepartment {
        /// Who pointed at it.
        referrer: String,
        /// The department that does not exist.
        department: String,
    },
    /// The root department is not in the department list, or has a parent.
    #[error("root department '{0}' is not a declared department with no parent")]
    RootInvalid(String),
    /// A department claims the logical window id the client reserves for the
    /// focused-person window.
    ///
    /// REFUSED, never assumed away. `placement::FOCUS_WINDOW_ID` is a logical
    /// window id in the same namespace as a department id, and converge's
    /// undesired-window reap is aimed by that id. A department that carried the
    /// reserved value would be a real department the reap could destroy, and a
    /// focus window a real department could inherit. The deleted ancestor of
    /// this mechanism argued in prose that a slug can never contain `__`; prose
    /// is not a check, so this is one.
    #[error("department '{0}' claims the reserved focus-window id")]
    ReservedDepartmentId(String),
}

impl Roster {
    /// Decode a `/v1/org/roster/desired` body.
    ///
    /// # Errors
    /// The serde error, verbatim: this is a peer service in the same
    /// workspace, so there is no tolerant second arm for a body it has never
    /// sent.
    pub fn from_json(body: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(body)
    }

    /// Every person the company knows, desired or not.
    ///
    /// The membership set, not the running set. It is what distinguishes this
    /// company's OWN departed person's leaked process from a stranger's —
    /// the first is reapable, the second is never touched.
    #[must_use]
    pub fn known_person_ids(&self) -> BTreeSet<String> {
        self.people.iter().map(|person| person.id.clone()).collect()
    }

    /// The department with this id.
    #[must_use]
    pub fn department(&self, id: &str) -> Option<&RosterDepartment> {
        self.departments.iter().find(|unit| unit.id == id)
    }

    /// Refuse a roster that does not hold together.
    ///
    /// # Errors
    /// [`RosterError`] naming the first inconsistency found.
    pub fn validate(&self) -> Result<(), RosterError> {
        let mut department_ids = BTreeSet::new();
        let mut department_orders = BTreeSet::new();
        for unit in &self.departments {
            // THE RESERVED ID, refused at the door. See
            // `RosterError::ReservedDepartmentId` — the reap is aimed by logical
            // window id, so a department wearing the focus window's id is a
            // department the reap could destroy.
            if unit.id == crate::placement::FOCUS_WINDOW_ID {
                return Err(RosterError::ReservedDepartmentId(unit.id.clone()));
            }
            if !department_ids.insert(unit.id.clone()) {
                return Err(RosterError::DuplicateId { kind: "department", id: unit.id.clone() });
            }
            if !department_orders.insert(unit.order) {
                return Err(RosterError::DuplicateOrder { kind: "department", order: unit.order });
            }
        }

        let root = self.department(&self.root_department_id);
        if !root.is_some_and(|unit| unit.parent_department_id.is_none()) {
            return Err(RosterError::RootInvalid(self.root_department_id.clone()));
        }

        for unit in &self.departments {
            if let Some(parent) = &unit.parent_department_id {
                if !department_ids.contains(parent) {
                    return Err(RosterError::UnknownDepartment {
                        referrer: format!("department '{}'", unit.id),
                        department: parent.clone(),
                    });
                }
            }
        }

        let mut person_ids = BTreeSet::new();
        let mut person_orders = BTreeSet::new();
        for person in &self.people {
            if !person_ids.insert(person.id.clone()) {
                return Err(RosterError::DuplicateId { kind: "person", id: person.id.clone() });
            }
            if !person_orders.insert(person.display_order) {
                return Err(RosterError::DuplicateOrder {
                    kind: "person",
                    order: person.display_order,
                });
            }
            if !department_ids.contains(&person.department_id) {
                return Err(RosterError::UnknownDepartment {
                    referrer: format!("person '{}'", person.id),
                    department: person.department_id.clone(),
                });
            }
            if let Some(headed) = &person.is_head_of {
                if !department_ids.contains(headed) {
                    return Err(RosterError::UnknownDepartment {
                        referrer: format!("person '{}' heads", person.id),
                        department: headed.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}
