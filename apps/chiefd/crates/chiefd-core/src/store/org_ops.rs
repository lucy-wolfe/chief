//! `org_ops` — shared MACHINERY for the atomic org-chart operation family
//! (fable-arch ruling, atomic-shutdown design lock).
//!
//! The operator's mandate: every org-chart action (shutdown, transfer, bench, hire,
//! fire, reorg) is ONE fast atomic SQL transaction — no queuing, no lease, no
//! two-call "retry after handoffs" contract. This module owns the *preamble*
//! that every member of that family composes, and NOTHING else:
//!
//! - [`is_ceo`] / [`department_is_company_root`] — the ONLY two questions any
//!   guard in this family may ask. One person and one department; there is no
//!   protected REGION any more, and `guard_ceo_exempt` that computed one was
//!   deleted on 2026-08-13 rather than left available.
//! - [`supersede_open_transition`] — cancel any in-flight transition so the
//!   `transitions_one_active` partial-unique index (over
//!   `awaiting_handoff|overdue|ready`) can never contradict a terminal write.
//! - org_events emission comes from [`crate::store::rows_txn`]
//!   ([`apply_and_emit`]): every op emits one `org_events` row PER TOUCHED
//!   ENTITY inside the SAME txn. The company writer serializes each named
//!   decision with its rows, so there is no caller-supplied collision fence.
//!
//! fable ruling (share machinery, NOT a trait): each op keeps its OWN named fn
//! with its OWN `BEGIN IMMEDIATE` body and its OWN tests — readable
//! top-to-bottom in one place, because these txns are the product's correctness
//! core. There is deliberately no `AtomicOrgOp` trait growing inherited
//! default behavior; the ops merely CALL these composed helpers. The module is
//! named for the FAMILY so fable's structural verbs (transfer/bench/hire/…)
//! migrate onto the same preamble as their B1 ports land.

use rusqlite::Transaction;

use crate::error::store_failure;
use crate::error::ChiefdError;
use crate::isotime::{iso_millis, parse_iso_millis};
use crate::store::org_projection;
use crate::store::organization::{ContractMetadata, EmploymentState, PersonKind, UnitKind};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::store::{
    activity, goal_delivery_quiesce_rows, launch_intent_rows, organization_rows, organization_spec,
    stand_down,
};

/// Prepare one attended CEO-only boot as a single normalized transaction.
/// The operation derives the CEO from the normalized manifest, retracts every
/// non-CEO durable desired-active row, clears every OTHER person's launch
/// intent, **grants the root its own start decision**, and stamps the
/// pre-reset-mail quiesce watermark. This closes both
/// split states in which the empty fence coexists with either prior bulk
/// activity or mail that can immediately re-authorize a report.
///
/// # THE OPERATOR'S ARRIVAL IS THE ROOT'S DEMAND
///
/// This used to retract everybody else and say nothing about the CEO, because
/// the CEO did not need saying: `activity::reconcile` handed the root permanent
/// demand unconditionally, so it ran whether or not anything asked. That
/// exemption is what the operator's "everybody settles" ruling removes, and a
/// root with no demand does not park after the settle window -- it never starts at
/// all, because `active` is derived purely from demand reasons.
///
/// So the root gets a REAL reason instead of an exemption. A normalized launch
/// intent is the product's durable, explicit start decision (see
/// `converge_apply::cycle`, which carries `Fenced` ids into activity as
/// `Requested` demand), and this is the one call site that speaks for the
/// operator: `chief attach` runs it on every attach, and genesis runs it once.
/// Naming the CEO here is therefore exactly "the operator asked for the root",
/// said in the vocabulary the rest of the model already uses.
///
/// The decision LAPSES as a REQUEST, which is what keeps this from being the
/// old exemption in a new coat: the cycle contributes a fenced id as
/// `Requested` only while that person is not already desired-active, so this
/// call stops asking once the root is up, and the next attach asks again.
///
/// WHAT IT DOES NOT MEAN, because this comment said the opposite for months
/// and the opposite is what a reader will carry away. The root does NOT then
/// settle and park like anybody else. Operator ruling 2026-08-14 -- "CEO can
/// never go to sleep" -- narrowed the 2026-08-13 position for the ROOT alone:
/// the CEO holds a PERMANENT lease, accrues no idle clock, and never parks,
/// because a parked CEO is an unreachable company. That is pinned by
/// `activity::tests::the_ceo_holds_a_permanent_lease_and_never_parks`, which
/// replaced an earlier `the_ceo_settles_and_parks_like_everybody_else`. The
/// lapse here is about this call's REQUEST, not about the root's residency.
///
/// # Errors
/// A missing/invalid manifest is a typed refusal; row failures are store-failure
/// errors. No partial activity/fence write can commit.
pub fn prepare_ceo_only(
    tx: &Transaction<'_>,
    slug: &str,
    at: &str,
    actor: &str,
) -> Result<i64, ChiefdError> {
    let manifest = organization_rows::reconstruct(tx, slug)?.ok_or_else(|| {
        ChiefdError::refused("unknown-company", "organization manifest is absent")
    })?;
    let ceo = manifest.chief_person_id().map_err(ChiefdError::Refused)?.to_string();
    // `slug` keys the rows; `manifest.slug` is the company's DISPLAY name, which
    // is what the reconstructed fence and the quiesce touch are identified by.
    let company = manifest.slug.as_str();
    let intent = launch_intent_rows::reconstruct(tx, slug, company)?;

    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = activity::rows::retract_non_kept_desired_active(tx, slug, &ceo, at)?;
        // Every OTHER person's start decision is cleared. The root's is skipped
        // deliberately: this operation is about to assert it, and deleting then
        // re-inserting the same row would emit two audit touches on every
        // attach and break the idempotence this function promises.
        for person_id in intent.person_ids.iter().filter(|person_id| **person_id != ceo) {
            if let Some(touch) =
                launch_intent_rows::delete_person_fence(tx, slug, person_id, "attach-ceo-only")?
            {
                touches.push(touch);
            }
        }
        // THE ROOT'S OWN START DECISION -- the whole of "what brings the root
        // back". The operator arrived, so the operator's door asks for the root
        // BY NAME rather than relying on an exemption inside `reconcile` to run
        // it unasked. A duplicate is a no-op, so a repeat attach is silent.
        if let Some(touch) = launch_intent_rows::insert_person_fence(tx, slug, &ceo)? {
            touches.push(touch);
        }
        if let Some(touch) = goal_delivery_quiesce_rows::upsert_quiesce(tx, slug, company, at)? {
            touches.push(touch);
        }
        Ok(touches)
    })
    .map_err(|e| store_failure("ceo-only-prepare", e))
}

// ---------------------------------------------------------------------------
// Family preamble (shared by every atomic org op)
// ---------------------------------------------------------------------------

// TOMBSTONE (2026-08-13, doc block removed #1071): the doc of the deleted
// `guard_ceo_exempt` outlived its function here. It taught the WHOLE
// executive root — the company root, the CEO's ancestor chains and the
// `office-of-the-ceo` chain — and it survived the deletion by silently
// gluing itself to `eligibility_view` below, truncated mid-sentence. A
// doc comment teaching a retired model is how the code gets "fixed" back,
// so it is deleted rather than reworded. The live answers are `is_ceo` and
// `department_is_company_root`, and nothing else.

/// The pure eligibility view of `slug`, built from the structural row columns
/// alone.
///
/// Deliberately NOT `organization_rows::reconstruct`: eligibility depends only
/// on the shape of the tree, and the whole-manifest read additionally demands
/// every person's complete resource and model columns. Reading more than the
/// decision needs would let an unrelated column make a refusal check fail.
///
/// # Errors
/// Propagates any `rusqlite` failure.
fn eligibility_view(tx: &Transaction<'_>, slug: &str) -> rusqlite::Result<org_projection::OrgView> {
    let mut view = org_projection::OrgView::default();
    for (id, parent_department_id, head_person_id, state) in
        organization_rows::department_structure(tx, slug)?
    {
        view.departments.insert(
            id,
            org_projection::UnitView {
                parent_department_id,
                head_person_id,
                state: if state == "paused" {
                    crate::store::organization::UnitState::Paused
                } else {
                    crate::store::organization::UnitState::Active
                },
            },
        );
    }
    for (id, kind, employment_state, department_id) in
        organization_rows::person_structure(tx, slug)?
    {
        view.people.insert(
            id,
            org_projection::PersonView {
                kind: match kind.as_str() {
                    "executive" => PersonKind::Executive,
                    "head" => PersonKind::Head,
                    _ => PersonKind::Worker,
                },
                employment_state: match employment_state.as_str() {
                    "departed" => EmploymentState::Departed,
                    "benched" => EmploymentState::Benched,
                    _ => EmploymentState::Active,
                },
                department_id,
            },
        );
    }
    Ok(view)
}

/// proceed).
/// Is this person THE CEO — the head of the root department?
///
/// The one immovable node. Operator ruling, 2026-08-13, recorded in
/// `AGENTS.md` under "THE CEO IS THE ONLY IMMOVABLE NODE": the CEO never
/// moves, never converts into the head of another department, and always heads
/// the root. **Everyone else is fluid**, including a Chief of Staff and
/// including anybody who merely happens to be homed in the executive root.
///
/// This is the ONLY person-shaped question any guard in this family asks —
/// structural AND destructive alike, since the operator corrected the ruling on
/// 2026-08-13. The wider whole-root predicate it briefly shared the file with
/// is deleted: a region that no longer protects anybody but can still be
/// computed is an invitation to protect a region again.
///
/// # Errors
/// Propagates any `rusqlite` failure. A company with no root department has no
/// CEO, and answers `false` rather than erroring.
pub fn is_ceo(tx: &Transaction<'_>, slug: &str, person_id: &str) -> rusqlite::Result<bool> {
    Ok(organization_rows::root_department_head(tx, slug)?.as_deref() == Some(person_id))
}

/// Is this department THE company root — the one unit that never moves?
///
/// The department-shaped half of [`is_ceo`], and the same ruling. The CEO
/// always heads the root, so a root that reparented would stop being the root
/// and a root whose head was reassigned would stop being headed by the CEO.
/// Nothing else is fixed: `office-of-the-ceo` and the CEO's other chains
/// reparent and change head like any other department.
///
/// Read as the root ROW rather than through `executive_root_unit_ids`, for the
/// reason `reparent_department` already states — that set is ancestor chains,
/// and it freezes units whose only crime is sitting near the CEO.
///
/// # Errors
/// Propagates any `rusqlite` failure. A company with no root department
/// answers `false` rather than erroring.
pub fn department_is_company_root(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
) -> rusqlite::Result<bool> {
    Ok(organization_rows::company_root(tx, slug)?
        .is_some_and(|(root_id, _)| root_id == department_id))
}

// ---------------------------------------------------------------------------
// shutdown_person — the first family member (settle-ux owns this one)
// ---------------------------------------------------------------------------

/// Why a shutdown is happening. A CALLER precondition, not a txn-body branch:
/// the txn body is IDENTICAL for both. `AutomaticSettle` is only ever passed
/// after the 60s settle lease elapsed AND the settle-shutdown contract holds
/// (no loop ∧ no goal attached); `Commanded` is an explicit operator/manager
/// stop carrying the originating `person-stop:` intent id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownKind {
    /// An operator/manager said "stop X". `intent_id` is the originating
    /// `person-stop:<…>` id, recorded on the terminal transition's `intent_id`.
    Commanded {
        /// The originating `person-stop:<…>` id, recorded on the terminal
        /// transition's `intent_id` (an owned park).
        intent_id: String,
    },
    /// An idle person trending to CEO-only. The terminal transition's
    /// `intent_id` is NULL (an unowned settle — delta #1 semantics).
    AutomaticSettle,
}

impl ShutdownKind {
    /// The terminal transition `action` is `park` for BOTH kinds (fable ruling):
    /// a commanded STOP is NOT a departure — the person stays EMPLOYED with
    /// their pane down (an OWNED park, carrying the `person-stop:` intent_id),
    /// exactly as the landed atomic-reorg stop path synthesizes. An automatic
    /// settle is an UNOWNED park (NULL intent_id). Writing `offboard` here would
    /// fabricate departures in the durable ledger (employment_state flip +
    /// staffing_history `offboarded`) — actual firing is a SEPARATE family op
    /// (`offboard_person`) that flips those in its own txn, never folded in here.
    fn action(&self) -> activity::TransitionAction {
        match self {
            Self::Commanded { .. } | Self::AutomaticSettle => activity::TransitionAction::Park,
        }
    }

    /// The verb this shutdown names when it withdraws the person's launch
    /// intent. A commanded stop and an automatic settle are different sentences
    /// to whoever reads the log — one is somebody's decision about this person,
    /// the other is the quiet lease running out.
    fn withdrawal_reason(&self) -> &'static str {
        match self {
            Self::Commanded { .. } => "commanded-stop",
            Self::AutomaticSettle => "automatic-settle",
        }
    }

    /// The transition's `intent_id` column: the commanded stop id, or NULL.
    fn intent_id(&self) -> Option<&str> {
        match self {
            Self::Commanded { intent_id } => Some(intent_id.as_str()),
            Self::AutomaticSettle => None,
        }
    }

    /// The opaque `reason` VALUE stamped on a superseded open transition.
    fn supersede_marker(&self) -> String {
        match self {
            Self::Commanded { intent_id } => {
                format!("superseded-by-shutdown:{intent_id}")
            }
            Self::AutomaticSettle => "superseded-by-settle".to_string(),
        }
    }

    /// The terminal row's own audit `reason`. `validate` requires a non-empty
    /// reason on EVERY transition, applied terminal rows included — an empty
    /// one made every later whole-ledger read throw "corrupt store: activity".
    fn terminal_reason(&self, person_id: &str) -> String {
        match self {
            Self::Commanded { intent_id } => format!("stopped {person_id} ({intent_id})"),
            Self::AutomaticSettle => format!("settled idle {person_id}"),
        }
    }
}

/// The reason a shutdown was refused without writing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownRefusal {
    /// The person is the org-root/CEO — the one hard exemption.
    CeoExempt,
    /// The actor names a real person who does not manage the department the
    /// target is homed in. Authority is the subtree, never the job title.
    ActorOutOfScope,
}

impl ShutdownRefusal {
    /// The kebab-case machine code the HTTP surface returns as the 422 refusal
    /// class (fable family convention: policy refusals are LOUD — 422 {code},
    /// never a quiet 200 body). Every future family refusal adds its own code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::CeoExempt => "ceo-exempt",
            Self::ActorOutOfScope => "actor-out-of-scope",
        }
    }

    /// A one-line human detail for the refusal (the 422 `detail`).
    #[must_use]
    pub fn detail(&self) -> &'static str {
        match self {
            Self::CeoExempt => {
                "the CEO heads the company root and never shuts down; everybody else is \
                 stoppable, wherever they sit"
            }
            Self::ActorOutOfScope => {
                "the actor does not manage the department the person they are shutting down is \
                 homed in"
            }
        }
    }
}

/// The outcome of a [`shutdown_person`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// The terminal transition was written and committed. `transition_id` is
    /// the new terminal row (the converge reaps the pane reactively off
    /// `last_desired_active = 0`).
    Applied {
        /// The terminal transition row that was written.
        transition_id: String,
    },
    /// Refused without touching a row (retryable only if the precondition
    /// changes — CeoExempt never will).
    Refused {
        /// Why the shutdown was refused without touching a row.
        reason: ShutdownRefusal,
    },
}

/// The placement a terminal transition row must carry to survive the model's
/// own `validate` (`store/activity.rs`): the `from_*` columns are the person's
/// current department. A NULL there used to wedge every
/// later whole-ledger ingest with "corrupt store: activity" (#39-followup).
fn terminal_transition_context(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
) -> rusqlite::Result<String> {
    let Some((_employment, department_id)) =
        organization_rows::person_placement(tx, slug, person_id)?
    else {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    };
    // TOMBSTONE (#751-P9): this also returned a `pane` — a fourth, raw-SQL
    // implementation of head-in-parent (head → the headed unit's parent, else
    // the assigned unit), written to `transitions.from_pane_department_id`.
    // Both the column and the rule are gone from the backend; a transition
    // records where the person was, which is an org fact.
    Ok(department_id)
}

/// Atomically shut a person down: ONE `BEGIN IMMEDIATE` txn that supersedes any
/// open transition, writes a terminal transition, clears the person's desired
/// state + launch-intent fence row, and emits per-entity `org_events` in the
/// same writer transaction. Runtime teardown is NOT here: a `KillPane` never lives
/// inside a SQL txn (that is the inline-teardown-under-lock bug atomic-reorg
/// killed). The DECISION is durable + atomic; the converge observes
/// `last_desired_active = 0` and kills the pane a moment later.
///
/// Contract (norm-approved spec + fable rulings):
/// 0.   CEO-exempt guard → [`ShutdownOutcome::Refused`], writes nothing.
/// 0.5. Supersede any open transition ([`supersede_open_transition`]).
/// 1.   INSERT terminal transition — `status = 'applied'` ALWAYS (fable ruling:
///      `forced` is NEVER overloaded; the override fact is owned by 0.5's
///      cancelled row + its `org_events` cancel touch).
/// 2.   `person_activity`: `last_desired_active = 0`, `active_transition_id`
///      → the terminal row (`idle_since`/`last_*_department_id` left as-is).
/// 3.   DELETE the targeted `launch_intent[slug, person_id]` row.
/// 4.   Emit one `org_events` touch per changed entity (person + terminal
///      transition + the superseded transition when present).
///
/// `at` is the ISO-8601 stamp (caller owns the clock); `actor` is the change's
/// author (`""` when anonymous / automatic).
///
/// # Errors
/// Propagates `rusqlite` failures lifted through `apply_and_emit`.
pub fn shutdown_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    kind: &ShutdownKind,
    at: &str,
    actor: &str,
) -> rusqlite::Result<ShutdownOutcome> {
    // 0. CEO-exempt guard — read-only, BEFORE the fence, writes nothing.
    //    THE CEO ALONE. This asked the whole executive root until the operator
    //    corrected the ruling on 2026-08-13: "he should be able to shut down a
    //    department, keep him around, do whatever they want with it". A head
    //    may act on anyone in its own subtree, and the CEO holds every tree.
    //    Nobody is protected by WHERE THEY SIT.
    if is_ceo(tx, slug, person_id)? {
        return Ok(ShutdownOutcome::Refused { reason: ShutdownRefusal::CeoExempt });
    }
    // 0b. WHO IS STOPPING WHOM. This verb took an `actor` and asked nothing of
    //     it, and the route handed it `String::new()` — so any caller reaching
    //     `/v1/org/person/shutdown` could stop anybody in the company, and the
    //     ledger recorded the author as the empty string.
    //
    //     The check is SCOPE over the department the target is homed in, not a
    //     role, and it is enforced only when the actor NAMES A PERSON ROW —
    //     `actor` is free-form audit prose in this corpus (`operator`, `op`,
    //     the empty string all name nobody). See `actor_names_a_person` for
    //     why that rule is sound both before and after credentials exist.
    if actor_out_of_scope_for_person(tx, slug, actor, person_id)? {
        return Ok(ShutdownOutcome::Refused { reason: ShutdownRefusal::ActorOutOfScope });
    }

    // A deterministic terminal-row id: no clock/RNG read here (the writer
    // thread forbids Date/random-in-txn). The id carries the model's embedded
    // sequence — `validate` rejects any other shape, so the old
    // `shutdown:<person>:<at>` form poisoned the ledger on its very next read.
    // The allocation happens inside the commit closure below; `apply_and_emit`
    // yields only the event seq, so the minted id travels out by capture.
    let minted = std::cell::RefCell::new(String::new());

    let _touches = apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        // Compose the OWNING stores' typed txn-accessors — NO raw cross-store
        // SQL (fable's containment contract). Each accessor does its own write
        // and returns the EventTouch, so the feed is byte-identical to the N4
        // port's for the same rows. Order = the org_events seq order.
        let mut touches = Vec::new();

        // The terminal row's id carries the model's embedded sequence
        // (`transition:<seq>:<person>:<action>`): `validate` rejects any
        // other shape, so a `shutdown:<person>:<at>` id poisoned every
        // subsequent whole-ledger ingest with "corrupt store: activity"
        // (the live #39-followup failure). Allocated inside the commit so
        // a retried transaction simply advances past the gap.
        let transition_id = format!(
            "transition:{}:{person_id}:park",
            crate::store::rows_txn::allocate_seq(
                tx,
                &activity::rows::transitions_counter_key(slug)
            )?
        );
        minted.replace(transition_id.clone());

        // 0.5. Supersede any open transition. N4 stamps the override fact on
        //      the cancelled row's `reason` VALUE (superseded-by-shutdown:…),
        //      which is why the terminal row stays a clean `applied`.
        if let Some((_cancelled_id, touch)) = activity::rows::supersede_open_transition(
            tx,
            slug,
            person_id,
            &kind.supersede_marker(),
            at,
        )? {
            touches.push(touch);
        }

        // 1. Terminal transition — `cancelled` with the explicit
        //    `abandoned_at` marker (the sanctioned shape for a lifecycle
        //    change nobody released): NEVER `applied`. `applied` records
        //    that the transition's OWNER released it and the structural
        //    change then went through; an unattended stop had no owner
        //    awake to release anything, so writing `applied` here would
        //    make a forced teardown indistinguishable from a cooperative
        //    one in every later read of the ledger. `abandoned_at` keeps
        //    that distinction durable. (Until #751-P4 there was a second
        //    reason: a whole-ledger validator rule that rejected an
        //    `applied` transition with no durable reflection memory. The
        //    reflection payload and that rule are both deleted; the shape
        //    below is unchanged because the first reason was always the
        //    real one.) action `park` (person stays employed); `intent_id` = the
        //    person-stop id for a commanded stop, NULL for an auto-settle.
        //    `placement_department_id` is the person's CURRENT placement:
        //    `validate` reads every transition through those rules, terminal
        //    rows included.
        let placement = terminal_transition_context(tx, slug, person_id)?;
        touches.push(activity::rows::insert_abandoned_transition(
            tx,
            slug,
            &transition_id,
            person_id,
            kind.action(),
            Some(&placement),
            kind.intent_id(),
            // `validate` requires a non-empty reason on every transition,
            // terminal rows included — an empty one wedged every later
            // whole-ledger ingest (#39-followup). The override fact still
            // lives on the cancelled row; this is the row's own audit line.
            &kind.terminal_reason(person_id),
            at,
        )?);

        // 2. person_activity: desired-off. The active-transition pointer
        //    stays EMPTY: `validate` rejects a pointer at a cancelled row,
        //    and an abandoned terminal is history, not a live pointer.
        touches.push(activity::rows::upsert_person_activity_desired(
            tx, slug, person_id, false, None, at,
        )?);

        // 3. Drop the launch-intent fence row (b4 accessor; None ⇒ no row).
        if let Some(touch) =
            launch_intent_rows::delete_person_fence(tx, slug, person_id, kind.withdrawal_reason())?
        {
            touches.push(touch);
        }

        Ok(touches)
    })?;

    Ok(ShutdownOutcome::Applied { transition_id: minted.into_inner() })
}

// ---------------------------------------------------------------------------
// create_department — P1-a of the atomic org-op family (fable's 8-verb spec)
// ---------------------------------------------------------------------------

/// The explicit head decision a `create_department` MUST carry (R3: "create
/// requires an explicit head decision"). There is no headless department — a
/// department is created WITH its head, in the SAME txn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadDecision {
    /// Appoint an EXISTING person as the new department's head. The person is
    /// re-pointed (home + assigned) into the department they now head — the
    /// "head re-pointer" of create's acceptance. Their `people.ordinal` is
    /// untouched (a re-parent is not an add/remove, so the bijection holds).
    AppointExisting {
        /// The existing person's id.
        person_id: String,
    },
    /// Hire a NEW person as the head, inserted in the SAME txn (placed in
    /// the new department) from the complete normalized person seed. Hiring
    /// never starts a pane (THE HARD RULE).
    HireNew {
        /// The new person's id.
        person_id: String,
        /// Complete normalized row seed, including resource child rows.
        /// Boxed: the hire variant dwarfs `AppointExisting` and would
        /// otherwise inflate every `HeadDecision` (clippy::large_enum_variant).
        seed: Box<OwnedNewPersonSeed>,
    },
}

/// What becomes of the department somebody ALREADY heads when they leave it.
///
/// A person heads one department, and that is enforced in SQL — schema.rs's
/// `departments_one_head` is a UNIQUE INDEX on `(slug, head_person_id)`, not
/// merely a `validate` rule. So a sitting head can take a different department,
/// or be transferred out of their own, only if the one they leave gets an
/// answer inside the same transaction. There are exactly two, and which one
/// applies is not a preference:
///
/// * [`Self::HandOver`] — the department keeps running under another of its
///   members.
/// * [`Self::Dissolve`] — the head is that department's LAST member. A head
///   must be homed in the department they head (`validate_organization_manifest`,
///   "Head '…' must belong to department '…'"), so a department always holds at
///   least one person, and one that loses its last cannot exist. This moves and
///   offboards nobody, because there is nobody left in it to move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadVacancy {
    /// Promote this member of the vacated department to head it.
    HandOver {
        /// A member of the vacated department: home and assigned there,
        /// employed, and not the outgoing head.
        successor_person_id: String,
    },
    /// Remove the emptied department.
    Dissolve,
}

impl HeadDecision {
    /// The head person's id, whichever decision this is.
    pub(crate) fn person_id(&self) -> &str {
        match self {
            Self::AppointExisting { person_id } | Self::HireNew { person_id, .. } => person_id,
        }
    }
}

/// One new non-manager created as part of a department's initial roster. The id
/// and complete normalized seed are validated before the transaction writes
/// the department or any person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepartmentStaffSeed {
    /// Stable person id derived by the launcher preview.
    pub person_id: String,
    /// Complete normalized worker seed, including all child rows.
    pub seed: OwnedNewPersonSeed,
}

/// Typed unit fields accepted by the atomic create-department operation. The
/// root `company` kind is deliberately absent: this operation always creates a
/// child unit and therefore admits only an ordinary department or a contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepartmentCreateUnit {
    /// An ordinary, non-transient department.
    Department,
    /// A transient contract carrying its complete durable metadata.
    Contract(ContractMetadata),
}

impl DepartmentCreateUnit {
    fn kind(&self) -> UnitKind {
        match self {
            Self::Department => UnitKind::Department,
            Self::Contract(_) => UnitKind::Contract,
        }
    }

    fn transient(&self) -> Option<&ContractMetadata> {
        match self {
            Self::Department => None,
            Self::Contract(metadata) => Some(metadata),
        }
    }
}

/// The reason a `create_department` was refused without writing anything. Every
/// variant maps to a 422 kebab machine code (the family convention — a policy
/// "no" is LOUD, never a quiet 200 body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateDepartmentRefusal {
    /// `parent_id` names no department.
    UnknownParent,
    /// The parent department or one of its ancestors is paused.
    ParentPaused,
    /// The requested department id already exists.
    DuplicateDepartmentId,
    /// The hire-new decision names an existing person id.
    DuplicatePersonId,
    /// The complete hire-new seed would reconstruct an invalid person.
    InvalidSeed {
        /// The offending seed field path.
        field: String,
        /// One line naming the constraint and the accepted vocabulary, so the
        /// caller can correct the field instead of abandoning it.
        detail: String,
    },
    /// The requester no longer manages the parent or appointed person's source
    /// department in current normalized rows.
    RequesterOutOfScope,
    /// No valid head decision (R3): an appoint-existing that names no real
    /// person. (A [`HeadDecision`] is structurally required, so "absent" only
    /// manifests as an appointee who does not exist.)
    HeadDecisionRequired,
    /// The appointed EXISTING person is the CEO — the head re-pointer would
    /// MOVE them out of the root they always head. The CEO alone; anybody
    /// else homed in the executive root, `office-of-the-ceo` included, is an
    /// ordinary appointee. Refused; nothing is written.
    ///
    /// Carries the person so the refusal can NAME them. A CEO told only that
    /// "the appointed head is executive-root protected" cannot tell which of
    /// its people that was, and one did exactly that on 2026-08-13 — it then
    /// spent a dozen turns guessing and gave up.
    ExecRootProtected {
        /// The appointee the executive root protects.
        person_id: String,
    },
    /// The existing appointee is not a movable, employed worker.
    ///
    /// Carries WHICH of the four conditions failed. Reciting all four made the
    /// caller test its own person against a checklist, and a refusal that
    /// leaves the reader to work out which clause fired is a refusal that has
    /// not said anything.
    HeadNotEligible {
        /// The appointee that cannot take the role.
        person_id: String,
        /// The condition that failed.
        because: HeadIneligibility,
    },
    /// The appointee already heads a department, and what becomes of it is
    /// either unstated or unworkable. See [`HeadVacancyRefusal`].
    HeadVacancy(HeadVacancyRefusal),
}

/// A head is leaving the department they lead, and the request has not said
/// what becomes of it — or has said something that cannot apply.
///
/// ONE type, carried by BOTH [`CreateDepartmentRefusal`] and
/// [`TransferRefusal`], because both verbs move a head out of the department
/// they lead and both owe the caller the same answer. Two enums each wording
/// this in their own way is how one rule becomes two that disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadVacancyRefusal {
    /// No decision was given.
    ///
    /// This REPLACED a flat `head-not-eligible`/`AlreadyHeads` refusal, which
    /// told a sitting head only that it "already heads one" and offered
    /// "appoint somebody else, or hand their current department over first" —
    /// a dead end for the case the operator actually hit, where the department
    /// being left has no other member to hand it to.
    ///
    /// Carries the department that would be left without a head and the members
    /// who could take it. An EMPTY list is the meaningful case rather than an
    /// error: the person is that department's last member, so
    /// [`HeadVacancy::Dissolve`] is the only answer that exists.
    Required {
        /// The sitting head the request wants to move.
        person_id: String,
        /// The department they would leave without a head.
        department_id: String,
        /// Its other active members, in manifest order; empty when there are none.
        eligible_successor_ids: Vec<String>,
    },
    /// A decision was supplied that cannot apply to that department.
    Invalid {
        /// The department the decision was about.
        department_id: String,
        /// Which clause failed.
        because: VacancyRefusal,
    },
}

impl HeadVacancyRefusal {
    /// The kebab-case machine code the HTTP surface returns as the 422 class.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Required { .. } => "vacancy-decision-required",
            Self::Invalid { .. } => "vacancy-decision-invalid",
        }
    }

    /// The one-line human detail. NAMES THE DEPARTMENT AND THE WAY THROUGH.
    ///
    /// The refusal this replaced said only "they already head a department …
    /// appoint somebody else, or hand their current department over first",
    /// which is unanswerable for a head who is their department's only member —
    /// the genesis shape of a Chief of Staff, and the case the operator hit. An
    /// empty successor list is not an error here; it is the fact that decides
    /// WHICH of the two answers exists.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Required { person_id, department_id, eligible_successor_ids } => {
                if eligible_successor_ids.is_empty() {
                    format!(
                        "'{person_id}' heads '{department_id}' and is its only member, so moving \
                         them leaves '{department_id}' with nobody at all. A department always \
                         holds at least its own head, so an emptied one cannot exist: send \
                         vacates=dissolve and '{department_id}' is removed in the same change. \
                         Nobody is moved or offboarded — there is nobody left in it to move."
                    )
                } else {
                    format!(
                        "'{person_id}' heads '{department_id}', and a person heads one department \
                         here. Say what becomes of '{department_id}': send vacates=hand-over \
                         naming one of its members ({}) as its new head, in the same change.",
                        eligible_successor_ids.join(", ")
                    )
                }
            }
            Self::Invalid { department_id, because } => {
                format!("that decision does not apply to '{department_id}': {}", because.detail())
            }
        }
    }
}

/// Why one vacancy decision does not fit the department it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VacancyRefusal {
    /// A decision was supplied for somebody who heads nothing.
    HeadsNothing,
    /// The named successor is not an active member of the vacated department.
    SuccessorNotAMember {
        /// The successor the caller named.
        successor_person_id: String,
    },
    /// `Dissolve` was asked for a department that still holds people.
    StillHasMembers {
        /// Who is still in it, in manifest order.
        member_person_ids: Vec<String>,
    },
    /// `Dissolve` was asked for a department that still has child departments.
    ///
    /// Refused rather than reparenting them: a create or a transfer that
    /// silently moved departments the caller never named would restructure a
    /// tree behind them. `remove_department_tree` is the verb that takes a
    /// whole subtree, and it fires the people in it.
    StillHasChildren {
        /// The children that must be moved or removed first.
        child_department_ids: Vec<String>,
    },
}

impl VacancyRefusal {
    /// The clause that failed, and the move that clears it.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::HeadsNothing => {
                "they head no department, so there is nothing to vacate; drop the decision"
                    .to_string()
            }
            Self::SuccessorNotAMember { successor_person_id } => format!(
                "'{successor_person_id}' is not an active member of it; a successor must already \
                 belong to the department they take over"
            ),
            Self::StillHasMembers { member_person_ids } => format!(
                "it still holds {}, so it is not emptied by this change; hand it over to one of \
                 them instead. To close a department that still has people, remove it — that \
                 fires them.",
                member_person_ids.join(", ")
            ),
            Self::StillHasChildren { child_department_ids } => format!(
                "it still has {} beneath it; move or remove them first. Dissolving it here would \
                 silently reparent departments this request never named.",
                child_department_ids.join(", ")
            ),
        }
    }
}

/// Why one appointee cannot become a department head.
///
/// Each arm is a DIFFERENT operator move, which is the whole reason they are
/// separate values: a departed person must be re-hired.
///
/// TWO arms were deleted here on 2026-08-13, by different packets and for
/// different reasons. `OnLoan` went with the loan concept itself. `AlreadyHeads`
/// is deleted rather than left unreachable: a sitting head is now asked what
/// becomes of the department they are leaving
/// ([`HeadVacancyRefusal::Required`]), which is a question with two answers,
/// and the old arm was a refusal with none for the case that mattered — a head
/// who is their department's only member had nobody to hand it to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadIneligibility {
    /// They have left the company.
    Departed,
    /// They are not a worker — an executive. An existing HEAD is no longer
    /// refused here: they are asked for a vacancy decision instead.
    NotAWorker,
}

impl HeadIneligibility {
    /// The clause that failed, and the move that clears it.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Departed => {
                "they have left the company; re-hire them before giving them a department"
            }
            Self::NotAWorker => {
                "appointing an existing head takes a WORKER and promotes them; an executive is \
                 not eligible"
            }
        }
    }
}

impl CreateDepartmentRefusal {
    /// The kebab-case machine code the HTTP surface returns as the 422 class.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownParent => "unknown-parent",
            Self::ParentPaused => "parent-paused",
            Self::DuplicateDepartmentId => "duplicate-department-id",
            Self::DuplicatePersonId => "duplicate-person-id",
            Self::InvalidSeed { .. } => "invalid-seed",
            Self::RequesterOutOfScope => "requester-out-of-scope",
            Self::HeadDecisionRequired => "head-decision-required",
            // The machine codes are UNCHANGED. Every caller that branches on
            // one keeps working; only the prose a human or an agent reads is
            // different.
            Self::ExecRootProtected { .. } => "exec-root-protected",
            Self::HeadNotEligible { .. } => "head-not-eligible",
            Self::HeadVacancy(refusal) => refusal.code(),
        }
    }

    /// A one-line human detail for the refusal (the 422 `detail`).
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::UnknownParent => "the parent department does not exist".to_string(),
            Self::ParentPaused => {
                "the parent department or one of its ancestors is paused (resume it first)".to_string()
            }
            Self::DuplicateDepartmentId => {
                "a department with this id already exists".to_string()
            }
            Self::DuplicatePersonId => "a person with this id already exists".to_string(),
            Self::InvalidSeed { field, detail } => {
                format!("invalid department person seed: {field} — {detail}")
            }
            Self::RequesterOutOfScope => {
                "the requester no longer manages the parent department or appointed person's source department".to_string()
            }
            Self::HeadDecisionRequired => {
                "create requires an explicit head decision (appoint an existing member or hire one)"
                    .to_string()
            }
            // NAMES THE MOVE. The fact whose absence produced a dozen turns of
            // guessing on 2026-08-13 is that appointing an existing head MOVES
            // that person: the CEO read "cannot be moved" and never learned
            // that its own request was the move. Both accepted paths follow,
            // because a refusal that states no way through is a dead end.
            Self::ExecRootProtected { person_id } => format!(
                "'{person_id}' is the CEO, the one person who never moves: appointing an \
                 existing head MOVES that person into the department they would head, and the \
                 CEO always heads the company root. Everybody else is appointable — create the \
                 department with a NEW head, or appoint any other person."
            ),
            Self::HeadNotEligible { person_id, because } => {
                format!("'{person_id}' cannot head a department: {}", because.detail())
            }
            Self::HeadVacancy(refusal) => refusal.detail(),
        }
    }
}

/// The outcome of a [`create_department`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateDepartmentOutcome {
    /// The department (and, on hire-new, its head) was written and committed.
    Applied {
        /// The created department's id.
        department_id: String,
    },
    /// Refused without touching a row (retryable only if the precondition changes).
    Refused {
        /// Why the create was refused.
        reason: CreateDepartmentRefusal,
    },
}

/// Backward-compatible head-only entry point. The production route calls
/// [`create_department_with_staff`] so an ordinary department's complete
/// initial roster can share the same transaction.
#[allow(clippy::too_many_arguments)]
pub fn create_department(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    parent_id: &str,
    name: &str,
    purpose: &str,
    head: &HeadDecision,
    requester_person_id: &str,
    audit_reason: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<CreateDepartmentOutcome> {
    create_department_with_staff(
        tx,
        slug,
        department_id,
        parent_id,
        name,
        purpose,
        head,
        &[],
        Some(requester_person_id),
        audit_reason,
        at,
        actor,
    )
}

/// Atomically create a department: ONE `BEGIN IMMEDIATE` txn that inserts the
/// department row, makes the explicit head decision (R3 — appoint-existing OR
/// hire-new), and inserts every supplied initial worker in the SAME txn. It
/// normalizes department/person order and emits per-entity `org_events`.
/// Launching a department brings its people up: one `launch_intent` fence row
/// is written for the head (appointed or hired) and for every ACTIVE initial
/// staff seed, so the live reconciler converges them to running. NO pane is
/// spawned inside this transaction, and only the settle path stops anybody
/// again. Composes the manifest store's typed accessors only (no raw
/// cross-store SQL).
///
/// Contract:
/// 0. Refusal guards (read-only, BEFORE any write, write nothing):
///    `unknown-parent` → `parent-paused` → `duplicate-department-id` →
///    head decision (`head-decision-required` when an appointee is absent;
///    `exec-root-protected` when the appointee is executive-root protected).
/// 1. INSERT the department at an append ordinal.
/// 2. Head decision: appoint-existing re-points the person's home+assigned into
///    the new department; hire-new INSERTs the person at ordinal =
///    `people_count`.
/// 3. INSERT every validated initial worker with child rows, activity row,
///    staffing history, and — for an active seed — a launch fence row.
/// 4. Normalize department preorder and head-first people order.
/// 5. Emit one `org_events` touch per changed entity.
///
/// `at` is the ISO-8601 stamp (caller owns the clock); `actor` the author.
///
/// # Errors
/// Propagates `rusqlite` failures lifted through `apply_and_emit`.
#[allow(clippy::too_many_arguments)]
pub fn create_department_with_staff(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    parent_id: &str,
    name: &str,
    purpose: &str,
    head: &HeadDecision,
    staff: &[DepartmentStaffSeed],
    requester_person_id: Option<&str>,
    audit_reason: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<CreateDepartmentOutcome> {
    create_department_with_staff_unit(
        tx,
        slug,
        department_id,
        parent_id,
        name,
        purpose,
        head,
        staff,
        &DepartmentCreateUnit::Department,
        None,
        requester_person_id,
        audit_reason,
        at,
        actor,
    )
}

/// Typed-unit counterpart of [`create_department_with_staff`]. The kind and
/// complete contract metadata join the same single transaction as the row,
/// head decision, initial staff, and events.
#[allow(clippy::too_many_arguments)]
pub fn create_department_with_staff_unit(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    parent_id: &str,
    name: &str,
    purpose: &str,
    head: &HeadDecision,
    staff: &[DepartmentStaffSeed],
    unit: &DepartmentCreateUnit,
    head_vacates: Option<&HeadVacancy>,
    requester_person_id: Option<&str>,
    audit_reason: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<CreateDepartmentOutcome> {
    // 0. Refusal guards — read-only, BEFORE any write, write nothing.
    //
    // ONE definition of eligibility: the pure projection. The route preflights
    // the SAME function and materializes the manifest it returns before this
    // transaction is ever opened, so a seed that cannot be built refuses with
    // no person committed and its id still free. A second copy of these rules
    // here would be free to drift from the copy the caller checked, which is
    // the exact failure the projection exists to remove.
    let view = eligibility_view(tx, slug)?;
    let projection = org_projection::check_department_create(
        &view,
        &org_projection::DepartmentCreateProposal {
            department_id,
            parent_id,
            name,
            purpose,
            head,
            staff,
            unit,
            requester_person_id,
            audit_reason,
            at,
            head_vacates,
        },
    );
    if let Err(reason) = projection {
        return Ok(CreateDepartmentOutcome::Refused { reason });
    }

    let dept_id = department_id.to_string();
    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::new();

        // The appointee's TRUE origin, read BEFORE any re-home below. A
        // `Dissolve` re-homes them out of their old department first (step 0),
        // so the "transferred" staffing entry must remember where they actually
        // came from rather than re-read a home the vacate has already changed.
        // `None` for a fresh hire, who came from nowhere.
        let appointee_origin = match head {
            HeadDecision::AppointExisting { person_id } => {
                organization_rows::person_department(tx, slug, person_id)?
            }
            HeadDecision::HireNew { .. } => None,
        };

        // 0. END THE OLD HEADSHIP FIRST. This is not a stylistic ordering.
        //    `departments_one_head` is a UNIQUE INDEX on
        //    `(slug, head_person_id)` (schema.rs), so inserting the new
        //    department below while the appointee still heads another one
        //    collides with that index and fails the whole transaction. The
        //    failure would name the index, not the cause. Moving this step
        //    after the insert is the refactor that reintroduces it.
        //
        //    For `Dissolve` the vacate DELETEs the appointee's old department,
        //    and `people.department_id` carries a FOREIGN KEY onto
        //    `departments` (enforced in production under `PRAGMA
        //    foreign_keys=ON`). The appointee is that department's sole member,
        //    so they must LEAVE it before it is removed or the DELETE breaks the
        //    FK (SQLite extended code 787) — the same fault the transfer path
        //    hit. Re-home them to the new department's parent, which exists and
        //    survives; the AppointExisting arm below moves them on into the
        //    department they now head. (The unique-head index still forces this
        //    delete before the insert, which is why the appointee cannot simply
        //    move straight into the new department: it does not exist yet.)
        if let Some(decision) = head_vacates {
            if matches!(decision, HeadVacancy::Dissolve) {
                touches.push(organization_rows::move_person(
                    tx,
                    slug,
                    head.person_id(),
                    parent_id,
                    at,
                )?);
            }
            vacate_headship(tx, slug, head.person_id(), decision, at, audit_reason, &mut touches)?;
        }

        // 1. Insert the department at its append-ordinal (gapless bijection).
        let ordinal = organization_rows::department_count(tx, slug)?;
        touches.push(organization_rows::insert_department(
            tx,
            slug,
            department_id,
            parent_id,
            name,
            purpose,
            unit.kind(),
            unit.transient(),
            "active",
            head.person_id(),
            ordinal,
            at,
        )?);

        // 2. The head decision, in the SAME txn — composing settle-ux's
        //    shared manifest write-accessors (norm-n1: compose, don't
        //    duplicate). Both decisions end with a launch fence row: a
        //    department that has just been launched has a head who is UP.
        match head {
            HeadDecision::AppointExisting { person_id } => {
                // The appointed member becomes a real head: move them into
                // the department they now head (the "head re-pointer"),
                // flip kind→head, and preserve the legacy transfer +
                // appointment audit pair. Their tool grant is left ALONE —
                // invariant 34 (which stripped `bash` here) was removed by
                // operator decision, 2026-08-10.
                //
                // `from_department` is the origin captured at the top of this
                // closure, not a fresh read: on the `Dissolve` answer the
                // appointee was already re-homed to the parent above, so
                // re-reading here would record the parent as their origin
                // instead of the department they truly left.
                let from_department = appointee_origin.clone();
                touches.push(organization_rows::move_person(
                    tx,
                    slug,
                    person_id,
                    department_id,
                    at,
                )?);
                touches.push(organization_rows::set_person_kind(
                    tx,
                    slug,
                    person_id,
                    crate::store::organization::PersonKind::Head,
                    at,
                )?);
                organization_rows::append_staffing_history(
                    tx,
                    slug,
                    person_id,
                    "transferred",
                    from_department.as_deref(),
                    Some(department_id),
                    audit_reason,
                    at,
                )?;
                organization_rows::append_staffing_history(
                    tx,
                    slug,
                    person_id,
                    "appointed-head",
                    None,
                    Some(department_id),
                    audit_reason,
                    at,
                )?;
                // The head of a department that was just launched is up. The
                // appointee can never be the CEO — the projection's
                // `exec-root-protected` guard refuses that decision outright
                // above — so this is always a non-CEO fence. A benched
                // appointee is left stopped.
                let appointee_is_active = organization_rows::person_placement(tx, slug, person_id)?
                    .is_some_and(|(employment, _)| employment == "active");
                if appointee_is_active {
                    fence_started_person(tx, slug, person_id, &mut touches)?;
                }
            }
            HeadDecision::HireNew { person_id, seed } => {
                touches.push(organization_rows::insert_person(
                    tx,
                    slug,
                    person_id,
                    department_id,
                    &OwnedNewPersonSeed::as_ref(seed),
                    at,
                )?);
                touches.push(activity::rows::insert_person_activity_desired_off(
                    tx,
                    slug,
                    person_id,
                    seed.employment_state,
                    department_id,
                    true,
                    at,
                )?);
                organization_rows::append_staffing_history(
                    tx,
                    slug,
                    person_id,
                    "hired",
                    None,
                    Some(department_id),
                    audit_reason,
                    at,
                )?;
                if seed_comes_up(seed.employment_state) {
                    fence_started_person(tx, slug, person_id, &mut touches)?;
                }
            }
        }

        // 3. Insert the whole initial worker roster into this SAME
        //    transaction. An active seed is fenced as it is inserted: the
        //    roster a department was launched with comes up with it, and the
        //    settle path — not creation — is what stops anybody.
        for member in staff {
            touches.push(organization_rows::insert_person(
                tx,
                slug,
                &member.person_id,
                department_id,
                &member.seed.as_ref(),
                at,
            )?);
            touches.push(activity::rows::insert_person_activity_desired_off(
                tx,
                slug,
                &member.person_id,
                member.seed.employment_state,
                department_id,
                true,
                at,
            )?);
            organization_rows::append_staffing_history(
                tx,
                slug,
                &member.person_id,
                "hired",
                None,
                Some(department_id),
                audit_reason,
                at,
            )?;
            if seed_comes_up(member.seed.employment_state) {
                fence_started_person(tx, slug, &member.person_id, &mut touches)?;
            }
        }

        // 4. Restore the same canonical order a whole-manifest mutation
        //    publishes: department preorder, then people grouped by home
        //    department with each head first.
        touches.extend(organization_rows::refresh_department_order(tx, slug, at)?);
        touches.extend(organization_rows::refresh_people_order(tx, slug, at)?);

        // One event per changed entity even when insert/move/order
        // normalization touched the same row more than once.
        let mut seen = std::collections::HashSet::new();
        touches.retain(|touch| seen.insert((touch.entity.clone(), touch.entity_id.clone())));
        Ok(touches)
    })?;
    Ok(CreateDepartmentOutcome::Applied { department_id: dept_id })
}

// ---------------------------------------------------------------------------
// appoint_department_head — org_ops family member 2 (H2 verb)
// ---------------------------------------------------------------------------

/// Why an appoint-head was refused without writing anything (422 family class).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppointRefusal {
    /// The target department does not exist.
    UnknownDepartment,
    /// The successor is not a member of the department (absent, or assigned
    /// elsewhere).
    NotAMember,
    /// The successor already heads another department.
    AlreadyHeadsElsewhere,
    /// The successor is already the SITTING head of this same department — a
    /// no-op re-appointment (production-bug restoration: `person_heads_department_other_than`
    /// deliberately excepts `department_id`, which is correct for
    /// `AlreadyHeadsElsewhere` but silently let a re-appointment of the same
    /// head through with no check at all).
    AlreadySittingHead,
    /// The successor has departed (employment_state = departed).
    DepartedSuccessor,
    /// The department IS the company root — the CEO always heads it. Only the
    /// root; the rest of the CEO's chains, `office-of-the-ceo` included, may
    /// change head like any other department (AGENTS.md, 2026-08-13).
    RootHeadNotReassignable,
    /// The actor names a real person who does not manage the department whose
    /// head is being appointed, or the department the outgoing head is being
    /// demoted into. Authority is the subtree, never the job title.
    ActorOutOfScope,
}

impl AppointRefusal {
    /// The kebab-case 422 machine code (family convention).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownDepartment => "unknown-department",
            Self::NotAMember => "not-a-member",
            Self::AlreadyHeadsElsewhere => "already-heads-elsewhere",
            Self::AlreadySittingHead => "already-sitting-head",
            Self::DepartedSuccessor => "departed-successor",
            Self::RootHeadNotReassignable => "root-head-not-reassignable",
            Self::ActorOutOfScope => "actor-out-of-scope",
        }
    }

    /// A one-line human detail for the 422 body.
    #[must_use]
    pub fn detail(&self) -> &'static str {
        match self {
            Self::UnknownDepartment => "no such department",
            Self::NotAMember => "the successor is not a member of that department",
            Self::AlreadyHeadsElsewhere => "the successor already heads another department",
            Self::AlreadySittingHead => "the person already heads this department",
            Self::DepartedSuccessor => "the successor has departed",
            Self::RootHeadNotReassignable => {
                "the company root is always headed by the CEO; its head cannot be reassigned"
            }
            Self::ActorOutOfScope => {
                "the actor does not manage the department they are appointing a head for, or the \
                 department the outgoing head is demoted into; appointing a head MOVES that \
                 person into the department they now head"
            }
        }
    }
}

/// The outcome of an [`appoint_department_head`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppointOutcome {
    /// The appointment committed.
    Applied,
    /// Refused without touching a row.
    Refused {
        /// Why it was refused.
        reason: AppointRefusal,
    },
}

/// Atomically appoint a new department head (H2): ONE `BEGIN IMMEDIATE` txn that
/// re-points the head, flips the incoming/outgoing person kinds, optionally
/// demotes the
/// outgoing head into the replacer's department (R4), records staffing history,
/// and — the H2 fix — performs an OWNERSHIP TRANSFER of supervision state: every
/// manager_goal / delegated_goal / goal_watch / check-in owned by the outgoing
/// head is RE-KEYED to the incoming head, goal ids STABLE, nothing cancelled.
/// This is the blob-era bug that broke the operator's live reorg (appoint re-keyed/
/// dropped the old head's delegated goals). Composes typed store accessors only
/// — no raw cross-store SQL (fable containment). NO activity transition (pure
/// structural). Runtime placement follows reactively via the converge.
///
/// `demote_to_department_id`: R4 — the outgoing head moves to that department
/// (the tool supplies the replacer's home); `None` = left in place.
///
/// # Errors
/// Propagates `rusqlite` failures lifted through the direct row transaction.
pub fn appoint_department_head(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    successor_person_id: &str,
    demote_to_department_id: Option<&str>,
    at: &str,
    actor: &str,
) -> rusqlite::Result<AppointOutcome> {
    use AppointRefusal as R;
    macro_rules! refuse {
        ($r:expr) => {
            return Ok(AppointOutcome::Refused { reason: $r })
        };
    }

    // 0. Refusal guards (read-only, BEFORE the fence — write nothing). Composed
    //    from organization_rows read accessors (no raw departments/people SQL).
    let Some(outgoing_head) = organization_rows::department_head(tx, slug, department_id)? else {
        refuse!(R::UnknownDepartment);
    };
    // STRUCTURAL: the company root alone. The CEO always heads it, so
    // reassigning its head is really a demand to replace the CEO. This asked
    // the WHOLE executive-root set until 2026-08-13, which also froze
    // `office-of-the-ceo` — a department whose head may hand over like anybody
    // else's. #1063 narrowed create, transfer and reparent and left this one.
    if department_is_company_root(tx, slug, department_id)? {
        refuse!(R::RootHeadNotReassignable);
    }
    if successor_person_id == outgoing_head {
        refuse!(R::AlreadySittingHead);
    }
    // WHO IS APPOINTING. Scope over the department whose head changes, and —
    // when the outgoing head is R4-demoted — over the department they land in
    // too. One department is not enough: demoting somebody into a unit the
    // actor does not manage would push a person across a boundary the tree
    // forbids reaching sideways over.
    //
    // The successor needs no separate check: they are refused `NotAMember`
    // below unless they are already ASSIGNED to this department, so scope over
    // the department is scope over them. That is the same fact as "appointing
    // a head MOVES that person" seen from the other side — heading a
    // department means living in it, and this verb only promotes somebody who
    // already does.
    if actor_out_of_scope(tx, slug, actor, department_id)? {
        refuse!(R::ActorOutOfScope);
    }
    if let Some(demote_to) = demote_to_department_id {
        if actor_out_of_scope(tx, slug, actor, demote_to)? {
            refuse!(R::ActorOutOfScope);
        }
    }
    let Some((employment, successor_department_id)) =
        organization_rows::person_placement(tx, slug, successor_person_id)?
    else {
        refuse!(R::NotAMember); // no such person
    };
    if employment == "departed" {
        refuse!(R::DepartedSuccessor);
    }
    if successor_department_id != department_id {
        refuse!(R::NotAMember);
    }
    // THIS REFUSAL CANNOT FIRE, and that is deliberate rather than missed.
    //
    // It is left alone on purpose, and the argument is written here because the
    // next reader arrives holding a list of head refusals with no way to tell an
    // unreachable one from an unfixed one. The vacancy decision that gave
    // department create and person transfer a way through was NOT extended to
    // this guard, because there is no state in which a caller could reach it:
    //
    //   * line above: `assigned != department_id` already refused `NotAMember`,
    //     so by here the successor is assigned to THIS department;
    //   * `validate_organization_manifest` requires a head's assigned department
    //     to BE the department they head.
    //
    // So a successor who headed some OTHER department would be assigned there
    // and would have been refused two lines up. Only a manifest that already
    // fails `validate` reaches this line, and building a way through for a
    // state the store forbids is speculative code. The argument got SHORTER
    // when the loan concept was deleted the same day: a loan was the one thing
    // that could make a person's home and assigned departments disagree, so
    // there is no longer even a caveat to state. `replace_head_and_offboard`
    // has the same guard order and the same result.
    //
    // Keep it as the defence it is. Do not "finish the job" here.
    if organization_rows::person_heads_department_other_than(
        tx,
        slug,
        successor_person_id,
        department_id,
    )?
    .is_some()
    {
        refuse!(R::AlreadyHeadsElsewhere);
    }

    let reason = format!("appointed {successor_person_id} to head {department_id}");

    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::new();

        // Manifest: re-point head, flip kinds (successor→head, outgoing→worker),
        // and R4-demote the outgoing head. Tool grants are untouched.
        touches.push(organization_rows::set_department_head(
            tx,
            slug,
            department_id,
            successor_person_id,
            at,
        )?);
        touches.push(organization_rows::set_person_kind(
            tx,
            slug,
            successor_person_id,
            PersonKind::Head,
            at,
        )?);
        touches.push(organization_rows::set_person_kind(
            tx,
            slug,
            &outgoing_head,
            PersonKind::Worker,
            at,
        )?);
        if let Some(dest) = demote_to_department_id {
            touches.push(organization_rows::move_person(tx, slug, &outgoing_head, dest, at)?);
        }

        // Staffing ledger (its own D2 feed — no org_events touch).
        organization_rows::append_staffing_history(
            tx,
            slug,
            successor_person_id,
            "appointed-head",
            Some(department_id),
            Some(department_id),
            &reason,
            at,
        )?;
        let to_dept = demote_to_department_id.unwrap_or(department_id);
        organization_rows::append_staffing_history(
            tx,
            slug,
            &outgoing_head,
            "stepped-down",
            Some(department_id),
            Some(to_dept),
            &reason,
            at,
        )?;
        // The R4 move is TWO audit facts, matching the rest of the family
        // (every `move_person` caller appends `transferred`): the stepped-down
        // out of the led department, then the transfer into the replacer's
        // home. Folding the move into the stepped-down entry made it invisible
        // to the action vocabulary the roster audit reads.
        if let Some(dest) = demote_to_department_id {
            organization_rows::append_staffing_history(
                tx,
                slug,
                &outgoing_head,
                "transferred",
                Some(department_id),
                Some(dest),
                &reason,
                at,
            )?;
        }

        // De-duplicate per changed ENTITY: the successor's kind + tool changes
        // (and the outgoing head's kind + demote) each touch the same people
        // row; the feed carries ONE person event per entity (entity granularity).
        let mut seen = std::collections::HashSet::new();
        touches.retain(|t| seen.insert((t.entity.clone(), t.entity_id.clone())));
        Ok(touches)
    })?;

    Ok(AppointOutcome::Applied)
}

// ---------------------------------------------------------------------------
// reparent_department — P1-d (the reorg keystone; H1 dept-ordinal verb)
// ---------------------------------------------------------------------------

/// The reason a reparent was refused without writing anything (all 422 kebab).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReparentRefusal {
    /// The department to move has no row.
    UnknownDepartment,
    /// The target parent has no row.
    UnknownNewParent,
    /// The target parent or one of its ancestors is paused.
    NewParentPaused,
    /// The new parent IS the department or one of its descendants (self-parent
    /// is the degenerate case) — the move would create a cycle.
    WouldCreateCycle,
    /// The department already belongs to the requested parent. A structural
    /// no-op is refused so it cannot create a meaningless audit event.
    AlreadyUnderParent,
    /// The department IS the company root: never reparentable, because a root
    /// that gained a parent would stop being the root. The root ALONE — the
    /// `office-of-the-ceo` chain and the CEO's other chains reparent like any
    /// other department.
    ExecRootProtected,
    /// The actor does not manage the department they are moving. Taking a unit
    /// out of somebody else's subtree is a theft of that subtree.
    ActorOutOfScope,
    /// The actor does not manage the destination parent. This is the SECOND
    /// half of the reparent question and the one that is easy to miss: scope
    /// over the moved unit alone lets a head graft their own subtree — and
    /// everyone in it — under a parent they have no authority over.
    NewParentOutOfScope,
}

impl ReparentRefusal {
    /// The kebab-case machine code the HTTP surface returns as the 422 class.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownDepartment => "unknown-department",
            Self::UnknownNewParent => "unknown-new-parent",
            Self::NewParentPaused => "new-parent-paused",
            Self::WouldCreateCycle => "would-create-cycle",
            Self::AlreadyUnderParent => "already-under-parent",
            Self::ExecRootProtected => "exec-root-protected",
            Self::ActorOutOfScope => "actor-out-of-scope",
            Self::NewParentOutOfScope => "new-parent-out-of-scope",
        }
    }

    /// A one-line human detail for the refusal (the 422 `detail`).
    #[must_use]
    pub fn detail(&self) -> &'static str {
        match self {
            Self::UnknownDepartment => "no such department to reparent",
            Self::UnknownNewParent => "the new parent department does not exist",
            Self::NewParentPaused => "the new parent department or one of its ancestors is paused",
            Self::WouldCreateCycle => {
                "reparenting there would create a cycle (the new parent is the department or a descendant of it)"
            }
            Self::AlreadyUnderParent => "the department already belongs to that parent",
            Self::ActorOutOfScope => "the actor does not manage the department they are moving",
            Self::NewParentOutOfScope => {
                "the actor does not manage the department they are moving it under"
            }
            // The DEPARTMENT subject — deliberately worded so it cannot be
            // read as a claim about a person. This is the one refusal in the
            // family whose subject is a unit, and copy that said "is protected
            // and cannot be moved" would send the reader looking for a person
            // who is not involved.
            Self::ExecRootProtected => {
                "that department IS the company root — the node everything else hangs from, \
                 with no parent to move it to. Every department BENEATH it can be reparented \
                 anywhere, including beneath each other"
            }
        }
    }
}

/// The outcome of a [`reparent_department`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReparentOutcome {
    /// The reparent committed. The dept tree stays a valid rooted, cycle-free
    /// hierarchy and the dept ordinals are a gapless preorder bijection.
    Applied {
        /// The reparented department id.
        department_id: String,
    },
    /// Refused without touching a row (retryable only if the precondition
    /// changes).
    Refused {
        /// Why the reparent was refused.
        reason: ReparentRefusal,
    },
}

/// Atomically reparent a department (the operator's reorg): ONE `BEGIN IMMEDIATE` txn
/// that re-points `departments.parent_id`, recomputes the WHOLE-TREE preorder
/// ordinal bijection (H1 for dept ordinals), and emits one contiguous
/// `org_events` run per touched department. ChiefD's one writer serializes
/// concurrent calls, so callers send no sequence or revision. Pane
/// placement (a head's window follows its parent) is NOT actuated here; converge
/// observes the committed rows and moves panes reactively.
///
/// Composes the manifest store's TYPED accessors only (no raw cross-store SQL —
/// fence_containment). Subtree PEOPLE keep their transitions (their placement
/// department is unchanged by a dept move), so there is no person terminal row to
/// contradict and thus no open transition to supersede for this verb.
///
/// Contract:
/// 0. Read-only refusals BEFORE the transaction writes anything:
///    unknown-department · exec-root-protected · unknown-new-parent ·
///    new-parent-paused · already-under-parent · would-create-cycle ·
///    actor-out-of-scope · new-parent-out-of-scope. The two scope refusals come
///    LAST so a structural impossibility is still reported as one to a caller
///    who also lacks scope: told "out of scope" for a move that could never
///    apply, they would go looking for authority they do not need.
/// 1. `set_department_parent` — parent_id + park at the append sentinel.
/// 2. `refresh_department_order` — normalize the tree to a gapless preorder
///    bijection; emit one `department` touch per moved ordinal.
/// # Errors
/// Propagates `rusqlite` failures lifted through `apply_and_emit`.
pub fn reparent_department(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    new_parent_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<ReparentOutcome> {
    use organization_rows as manifest;

    // 0. Read-only policy/validation refusals — all BEFORE writes.
    // 0a. The department must exist.
    let Some((current_parent_id, _)) = manifest::department_parent_state(tx, slug, department_id)?
    else {
        return Ok(ReparentOutcome::Refused { reason: ReparentRefusal::UnknownDepartment });
    };
    // 0b. THE ROOT never moves, and only the root. It is the node the company
    //     hangs from and it has no parent to move it to — a structural fact,
    //     not a policy. Everything beneath it, `office-of-the-ceo` included,
    //     is reparentable: the operator's ruling on 2026-08-13 is that "you can
    //     move any child to any other department" and only the CEO is fixed.
    //     Read as `parent IS NULL` rather than through the executive-root SET,
    //     because that set is ancestor chains and would keep freezing units
    //     whose only crime is sitting near the CEO.
    if current_parent_id.is_none() {
        return Ok(ReparentOutcome::Refused { reason: ReparentRefusal::ExecRootProtected });
    }
    // 0c. The new parent and its complete ancestor chain must be active.
    match manifest::department_parent_state(tx, slug, new_parent_id)? {
        None => {
            return Ok(ReparentOutcome::Refused { reason: ReparentRefusal::UnknownNewParent });
        }
        Some((_, state)) if state == "paused" => {
            return Ok(ReparentOutcome::Refused { reason: ReparentRefusal::NewParentPaused });
        }
        Some(_) => {}
    }
    if department_or_ancestor_is_paused(tx, slug, new_parent_id)? {
        return Ok(ReparentOutcome::Refused { reason: ReparentRefusal::NewParentPaused });
    }
    // 0d. A move to the existing parent is a business refusal, never an audit
    // event.
    if current_parent_id.as_deref() == Some(new_parent_id) {
        return Ok(ReparentOutcome::Refused { reason: ReparentRefusal::AlreadyUnderParent });
    }
    // 0e. No cycle (covers self-parent and reparent-under-own-descendant).
    let parent_map = manifest::department_parent_map(tx, slug)?;
    if manifest::would_create_cycle(&parent_map, department_id, new_parent_id) {
        return Ok(ReparentOutcome::Refused { reason: ReparentRefusal::WouldCreateCycle });
    }
    // 0f. WHO IS REORGANIZING. `reparent_department` took an actor and asked
    // nothing of it, and the route handed it `String::new()` — so any caller
    // reaching the route could re-hang any department in any company under any
    // other, and the ledger recorded the author as the empty string.
    //
    // A REPARENT IS TWO QUESTIONS, NOT ONE, and this is the whole reason the
    // verb is not a copy of remove-tree. It detaches a subtree from one place
    // and attaches it somewhere else, so scope over the MOVED unit alone is not
    // enough: a head who manages Engineering and nothing else would still be
    // able to hang Engineering — every person in it, their panes and their
    // reporting line — beneath a department they have no authority over. That
    // is a grant of somebody else's authority over their own subtree, made
    // unilaterally, and it reads as an ordinary move in the audit trail.
    // Checking only the destination has the mirror flaw: it would let a head
    // pull a department out of a peer's subtree into their own. So both ends
    // are asked, with separate codes, because "you cannot move that" and "you
    // cannot move it THERE" send the caller to different fixes.
    //
    // The CEO manages the root and therefore every department, so the ordinary
    // whole-company reorg passes both halves unchanged — which is the point of
    // asking scope rather than a role.
    //
    // Enforced only when the actor NAMES A PERSON ROW, for the reason spelled
    // out on `actor_names_a_person`: the actor is free-form audit prose in this
    // corpus and fires as `operator`, as `op` and as the empty string.
    if actor_names_a_person(tx, slug, actor)? {
        if !organization_rows::person_manages_department(tx, slug, actor, department_id)? {
            return Ok(ReparentOutcome::Refused { reason: ReparentRefusal::ActorOutOfScope });
        }
        if !organization_rows::person_manages_department(tx, slug, actor, new_parent_id)? {
            return Ok(ReparentOutcome::Refused { reason: ReparentRefusal::NewParentOutOfScope });
        }
    }

    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        // 1. Re-point + park at the append sentinel; keep its dept touch.
        let mut touches = vec![manifest::set_parent(tx, slug, department_id, new_parent_id, at)?];
        // 2. Normalize the whole-tree preorder bijection; each OTHER moved
        //    dept emits one touch. Drop the refresh's duplicate for the
        //    just-reparented dept (step 1 already emitted it) so exactly one
        //    org_events row lands per department.
        for touch in manifest::refresh_department_order(tx, slug, at)? {
            if touch.entity_id != department_id {
                touches.push(touch);
            }
        }
        // 3. H1 people-bijection guard (settle-ux's shared primitive): a
        //    dept move changes no person's department, so the
        //    whole-company `people.ordinal` stays a gapless permutation and
        //    this is a NO-OP (emits touches only for moved rows — none). We
        //    compose it anyway so the family's H1 contract is explicit and
        //    the verb stays correct if a future variant ever shifts people.
        touches.extend(manifest::refresh_people_order(tx, slug, at)?);
        Ok(touches)
    })?;

    Ok(ReparentOutcome::Applied { department_id: department_id.to_string() })
}

/// Why a transfer/member-move was refused without writing a row. Each maps to a
/// kebab-case 422 machine code (family convention: a legitimate "no" is LOUD).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferRefusal {
    /// No such person (transfer_person only — a member-move uses `NotAMember`).
    UnknownPerson,
    /// The listed person is not a member (home) of the stated source department,
    /// or has no row at all (move_department_members).
    NotAMember,
    /// The destination department has no row.
    UnknownDestination,
    /// The destination department is paused (`assertActiveDestination`).
    DestinationPaused,
    /// The person is departed (offboarded) — nothing to place.
    PersonDeparted,
    /// The person heads their home department and this verb does not carry a
    /// vacancy decision. Only the BATCH verb answers this way now — see
    /// [`HeadMoveRule`] — and its detail points at the verb that does.
    HeadNeedsSuccessor,
    /// The person heads a department, and what becomes of it is unstated or
    /// unworkable. The same type department create carries, worded once.
    HeadVacancy(HeadVacancyRefusal),
    /// The person is the CEO and may not be moved. The CEO alone: living
    /// beside them in the executive root protects nobody.
    ExecRootProtected,
    /// The actor names a real person who does not manage the department the
    /// people are leaving, or the one they are moving into. BOTH are asked:
    /// scope over the source alone would let a head graft its own people into
    /// a department it does not manage, and scope over the destination alone
    /// would let it pull people out of one.
    ActorOutOfScope,
}

impl TransferRefusal {
    /// The kebab-case machine code the HTTP surface returns as the 422 class.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownPerson => "unknown-person",
            Self::NotAMember => "not-a-member",
            Self::UnknownDestination => "unknown-destination",
            Self::DestinationPaused => "destination-paused",
            Self::PersonDeparted => "person-departed",
            Self::HeadNeedsSuccessor => "head-needs-successor",
            Self::HeadVacancy(refusal) => refusal.code(),
            Self::ExecRootProtected => "exec-root-protected",
            Self::ActorOutOfScope => "actor-out-of-scope",
        }
    }

    /// A one-line human detail for the refusal (the 422 `detail`).
    ///
    /// Returns `String` rather than `&'static str` because the vacancy refusal
    /// names the department and its eligible successors, which are data.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::UnknownPerson => "no such person".to_string(),
            Self::NotAMember => "the person is not a member of the source department".to_string(),
            Self::UnknownDestination => "the destination department does not exist".to_string(),
            Self::DestinationPaused => "the destination department is paused".to_string(),
            Self::PersonDeparted => "the person is departed and cannot be placed".to_string(),
            // The BATCH verb's answer, and it names the verb that does carry a
            // vacancy decision. It used to end at "appoint a successor first",
            // which is unanswerable for a head who is their department's only
            // member — the same dead end the create path had.
            Self::HeadNeedsSuccessor => {
                "the person heads their department, and moving a set of members never moves a \
                 head. Move them with a single transfer, saying what becomes of the department \
                 they head, or appoint a successor there first"
                    .to_string()
            }
            Self::HeadVacancy(refusal) => refusal.detail(),
            // The person subject. Names the reserved units rather than the
            // abstraction, and states the accepted path: this person stays,
            // somebody else goes.
            Self::ExecRootProtected => {
                "that person is the CEO, who always heads the company root and never moves. \
                 Every other person may be transferred anywhere"
                    .to_string()
            }
            Self::ActorOutOfScope => {
                "the actor must manage BOTH the department the people are leaving and the one \
                 they are moving into; managing only one of the two is how a subtree gets \
                 grafted somewhere its head has no authority"
                    .to_string()
            }
        }
    }
}

/// How a verb treats a person who heads their own home department.
///
/// The two movement verbs answer differently ON PURPOSE, and the difference is
/// stated here rather than inferred from an unrelated flag. A single transfer
/// CAN move a head, provided the caller says what becomes of the department
/// they leave. A batch member-move never moves a head at all — its empty-batch
/// default is "every ordinary member", so a head only appears when named
/// explicitly — and inventing per-person vacancy decisions inside a batch would
/// be a second shape for one rule.
#[derive(Debug, Clone, Copy)]
pub enum HeadMoveRule<'a> {
    /// A head may go, carrying the caller's decision about their department.
    Vacates(Option<&'a HeadVacancy>),
    /// A head never goes by this verb; the refusal names the one that moves them.
    Refuse,
}

/// The outcome of a [`transfer_person`] or [`move_department_members`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferOutcome {
    /// Committed. `moved` is the person ids re-homed to the destination (one for
    /// a transfer, N for a member-move), in the order they were applied.
    Applied {
        /// The person ids re-homed to the destination.
        moved: Vec<String>,
    },
    /// Refused without touching a row (never retryable — fix the precondition).
    Refused {
        /// Why the move was refused.
        reason: TransferRefusal,
    },
}

/// Validate one mover against the destination and its own state. `require_home`
/// (move_department_members) additionally pins the person's home to the stated
/// source department (`NotAMember`); a bare transfer passes `None`.
/// `missing_is_not_a_member` selects the refusal for a nonexistent person
/// (`NotAMember` for a member-move, `UnknownPerson` for a transfer). Read-only.
fn validate_mover(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    require_home: Option<&str>,
    missing_is_not_a_member: bool,
    head_rule: HeadMoveRule<'_>,
) -> rusqlite::Result<Result<(), TransferRefusal>> {
    // person_placement → (employment_state, department_id) or None.
    let Some((employment_state, department_id)) =
        organization_rows::person_placement(tx, slug, person_id)?
    else {
        return Ok(Err(if missing_is_not_a_member {
            TransferRefusal::NotAMember
        } else {
            TransferRefusal::UnknownPerson
        }));
    };
    if let Some(source) = require_home {
        if department_id != source {
            return Ok(Err(TransferRefusal::NotAMember));
        }
    }
    // THE CEO, and nobody else. This asked the whole-executive-root question
    // until 2026-08-13, which froze every person who merely happened to be
    // homed in the root department — a Chief of Staff hired there could not be
    // transferred anywhere. Operator ruling: the CEO is the only immovable
    // node (`AGENTS.md`). A transfer is a structural verb, so it asks
    // `is_ceo` — the only person-shaped question left.
    if is_ceo(tx, slug, person_id)? {
        return Ok(Err(TransferRefusal::ExecRootProtected));
    }
    if employment_state == "departed" {
        return Ok(Err(TransferRefusal::PersonDeparted));
    }
    // A head must not leave their department without one. What that means
    // depends on the verb — see `HeadMoveRule`.
    if organization_rows::department_head(tx, slug, &department_id)?.as_deref() == Some(person_id) {
        return Ok(match head_rule {
            HeadMoveRule::Refuse => Err(TransferRefusal::HeadNeedsSuccessor),
            HeadMoveRule::Vacates(decision) => {
                // The SAME predicate the create path asks, over the same view,
                // so the two verbs cannot come to disagree about who may take a
                // department over.
                let view = eligibility_view(tx, slug)?;
                org_projection::check_head_vacancy(&view, Some(&department_id), person_id, decision)
                    .map_err(TransferRefusal::HeadVacancy)
            }
        });
    }
    Ok(Ok(()))
}

/// True when `department_id` itself or one of its ancestors is paused.
///
/// Callers first perform their own existence check so they can retain their
/// public `unknown-*` refusal. A missing ancestor or a malformed cycle is
/// fail-closed as paused: neither is a route under which staffing may enter a
/// subtree. This mirrors the TypeScript `stoppedOrganizationUnitAncestor`
/// predicate at the native direct-call boundary.
fn department_or_ancestor_is_paused(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
) -> rusqlite::Result<bool> {
    let mut cursor = Some(department_id.to_string());
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) {
            return Ok(true);
        }
        let Some((parent, state)) = organization_rows::department_parent_state(tx, slug, &id)?
        else {
            return Ok(true);
        };
        if state != "active" {
            return Ok(true);
        }
        cursor = parent;
    }
    Ok(false)
}

/// Assert the destination department exists and has an active ancestor chain.
/// Read-only.
fn validate_destination(
    tx: &Transaction<'_>,
    slug: &str,
    destination_id: &str,
) -> rusqlite::Result<Result<(), TransferRefusal>> {
    let Some(state) = organization_rows::department_state(tx, slug, destination_id)? else {
        return Ok(Err(TransferRefusal::UnknownDestination));
    };
    if state != "active" || department_or_ancestor_is_paused(tx, slug, destination_id)? {
        return Ok(Err(TransferRefusal::DestinationPaused));
    }
    Ok(Ok(()))
}

/// The shared normalized-row body for both H1 verbs: supersede each mover's
/// open transition, move each mover to the destination, append
/// the `transferred` staffing entry, then restore the whole-company ordinal
/// bijection (H1). The caller chooses whether this body is reached through a
/// caller-fenced compatibility operation or directly by the CompanyDb writer;
/// the movers must already be validated (this only writes). Emits, in one
/// contiguous seq run: per mover a
/// supersede-cancel touch (when an open row existed) + a person upsert touch,
/// then one person upsert per OTHER person whose ordinal shifted during the H1
/// densify (a mover's own re-densify folds into its move touch — same row).
///
/// `adopt_ready_transfer`: when true, a mover whose open transition is already
/// `ready` (released) is ADOPTED, not superseded — the single-person
/// `transfer_person` path is reached AFTER the staffing lifecycle has already
/// released this exact transition, so cancelling it here throws away the
/// release the caller just obtained and mints a fresh `awaiting_handoff` row
/// that nobody is left to release (the transfer/bench class-D bug:
/// `offboard_person_atomic` already carries this exact adoption via
/// `ready_open_transition_id`, ported here).
/// `move_department_members` passes `false`: it is a direct bulk admin verb
/// with no per-person graceful-transition lifecycle preceding it, so there is
/// never a fresh release to protect and every open transition is
/// unconditionally stale.
#[allow(clippy::too_many_arguments)]
fn move_touches(
    tx: &Transaction<'_>,
    slug: &str,
    destination_id: &str,
    movers: &[String],
    intent: &str,
    at: &str,
    audit_reason: &str,
    adopt_ready_transfer: bool,
) -> rusqlite::Result<Vec<EventTouch>> {
    let supersede_marker = format!("superseded-by-transfer:{intent}");
    let mut touches: Vec<EventTouch> = Vec::new();
    for person_id in movers {
        // The mover's current home BEFORE the move (the staffing entry's
        // `from`); read through the manifest accessor, never raw SQL.
        let from_department =
            organization_rows::person_placement(tx, slug, person_id)?.map(|(_, unit)| unit);
        let adopted = adopt_ready_transfer
            && activity::rows::ready_open_transition_id(
                tx,
                slug,
                person_id,
                activity::TransitionAction::Transfer,
            )?
            .is_some();
        if !adopted {
            if let Some((_cancelled, touch)) = activity::rows::supersede_open_transition(
                tx,
                slug,
                person_id,
                &supersede_marker,
                at,
            )? {
                touches.push(touch);
            }
        }
        // A transfer re-points the person's one placement column.
        touches.push(organization_rows::move_person(tx, slug, person_id, destination_id, at)?);
        // The append-only staffing entry (`transferred`); its own D2 feed, so
        // NO org_events touch is collected here.
        organization_rows::append_staffing_history(
            tx,
            slug,
            person_id,
            "transferred",
            from_department.as_deref(),
            Some(destination_id),
            audit_reason,
            at,
        )?;
    }
    // H1: restore the gapless whole-company ordinal bijection from the SAME
    // rows the activity projection reads. A mover already has a person touch
    // above, so its densify does not double-emit; only OTHER shifted people
    // add a touch here (dedup by the touch's entity_id == person id).
    for touch in organization_rows::refresh_people_order(tx, slug, at)? {
        if !movers.iter().any(|m| m == &touch.entity_id) {
            touches.push(touch);
        }
    }
    Ok(touches)
}

/// The staffing-history line this daemon AUTHORS for `act`.
///
/// A caller is never asked for audit prose. It was required on five structural
/// verbs, nothing read it, and a sentence somebody typed is weaker provenance
/// than the act plus the authenticated principal who performed it — which
/// cannot be gamed. The precedent is `remove_department_tree`, which has always
/// written `department <id> removed` itself. An empty actor records the act
/// alone rather than a dangling "by".
fn authored_ledger_line(act: &str, actor: &str) -> String {
    let actor = actor.trim();
    if actor.is_empty() {
        act.to_owned()
    } else {
        format!("{act} by {actor}")
    }
}

/// Atomically transfer ONE person to `destination_id`: ONE `BEGIN IMMEDIATE`
/// that supersedes any open transition, re-homes the person, appends the
/// staffing entry, and restores the H1 ordinal bijection. It is revisionless:
/// `CompanyDb::in_transaction` owns SQLite's single-writer serialization, so a
/// separate organization event cannot reject this semantic decision as stale.
/// Pane placement is NOT here: converge moves the pane reactively off the
/// changed `people` row (#448). `intent` is stamped on superseded transition
/// rows, while the staffing-history entry carries the line this function
/// authors from the act and the actor.
///
/// # Errors
/// Propagates `rusqlite` failures lifted through `move_touches`.
#[allow(clippy::too_many_arguments)]
pub fn transfer_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    destination_id: &str,
    intent: &str,
    at: &str,
    actor: &str,
    head_vacates: Option<&HeadVacancy>,
) -> rusqlite::Result<TransferOutcome> {
    let audit_reason = authored_ledger_line("transferred", actor);
    let audit_reason = audit_reason.as_str();
    if let Err(reason) = validate_destination(tx, slug, destination_id)? {
        return Ok(TransferOutcome::Refused { reason });
    }
    if let Err(reason) =
        validate_mover(tx, slug, person_id, None, false, HeadMoveRule::Vacates(head_vacates))?
    {
        return Ok(TransferOutcome::Refused { reason });
    }
    // WHO IS MOVING WHOM, AND WHERE TO. This verb took an `actor` from the
    // body, recorded it unjudged and asked nothing of it, so any caller could
    // move anybody anywhere.
    //
    // BOTH DEPARTMENTS ARE ASKED. Scope over the department the person is
    // leaving alone is a privilege escalation — a head could push its own
    // people into a unit it has no authority over — and scope over the
    // destination alone would let it pull people out of somebody else's
    // subtree. Enforced only when the actor names a person row.
    if actor_out_of_scope_for_person(tx, slug, actor, person_id)?
        || actor_out_of_scope(tx, slug, actor, destination_id)?
    {
        return Ok(TransferOutcome::Refused { reason: TransferRefusal::ActorOutOfScope });
    }
    let vacates = head_vacates.cloned();
    let mover = person_id.to_string();
    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::new();
        // RE-HOME THE MOVER FIRST, THEN VACATE THEIR OLD HEADSHIP. This order is
        // not stylistic: `people.department_id` carries a FOREIGN KEY onto
        // `departments(slug, id)` (schema.rs), enforced in production under
        // `PRAGMA foreign_keys=ON`. On the `Dissolve` answer `vacate_headship`
        // DELETEs the department the mover heads, and while the mover still
        // homes there that DELETE violates the FK (SQLite extended code 787,
        // "FOREIGN KEY constraint failed"). So the move — which re-points the
        // mover's one placement column onto the destination — must land before
        // the old department is removed, leaving nothing that references it.
        //
        // The create path vacates FIRST for the opposite reason
        // (`departments_one_head` is a UNIQUE index and the create INSERTs a new
        // department headed by this same person). That reasoning does not reach
        // here: a transfer targets an EXISTING department, so no second
        // department headed by the mover is ever created, and there is no unique
        // collision to pre-empt. Nor does the move lose the department to
        // vacate: `department_headed_by_person` keys off `head_person_id`, which
        // the move never touches, so it still names the old department after the
        // mover has left it.
        touches.extend(move_touches(
            tx,
            slug,
            destination_id,
            std::slice::from_ref(&mover),
            intent,
            at,
            audit_reason,
            true,
        )?);
        if let Some(decision) = vacates.as_ref() {
            vacate_headship(tx, slug, &mover, decision, at, audit_reason, &mut touches)?;
            // A transferred head heads NOTHING once they land, so they stop
            // being one. `vacate_headship` deliberately does not decide this:
            // on the create path the same person becomes the head of the new
            // department a moment later, and demoting them there would fight
            // the very appointment being made. The precedent is
            // `remove_department_tree` — "a head of a department that no longer
            // exists is not a head" — and the manifest validator enforces it
            // either way ("Worker '…' cannot head a department" is the mirror
            // of "Leader '…' must head exactly one department").
            touches.push(organization_rows::set_person_kind(
                tx,
                slug,
                &mover,
                PersonKind::Worker,
                at,
            )?);
        }
        dedupe_touches(&mut touches);
        Ok(touches)
    })?;
    Ok(TransferOutcome::Applied { moved: vec![mover] })
}

/// Atomically move a SET of members from `from_department_id` to
/// `destination_id` in ONE `BEGIN IMMEDIATE` — N transfers composed as one
/// atomic decision (all-or-nothing). Every listed person must be a member (home)
/// of the source department and individually movable; a single refusal fails the
/// WHOLE batch WITHOUT touching a row (R5: a head or departed person may
/// not be listed — appoint or return them first). Restores the H1 ordinal
/// bijection once for the whole batch. The company writer serializes validation
/// and the row changes.
///
/// # Errors
/// Propagates `rusqlite` failures lifted through `move_touches`.
#[allow(clippy::too_many_arguments)]
pub fn move_department_members(
    tx: &Transaction<'_>,
    slug: &str,
    from_department_id: &str,
    destination_id: &str,
    person_ids: &[String],
    intent: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<TransferOutcome> {
    if let Err(reason) = validate_destination(tx, slug, destination_id)? {
        return Ok(TransferOutcome::Refused { reason });
    }
    // An EMPTY batch means "every ordinary member of the source" (#751/P3).
    // A caller that has to enumerate the members first needs its own copy of
    // "who is an ordinary member", and the manifest it enumerates from is
    // already one read stale by the time the batch arrives; deriving the set
    // inside this transaction removes both. A caller that DOES name ids is
    // honoured untouched and still validated below.
    let derived;
    let person_ids = if person_ids.is_empty() {
        derived = organization_rows::department_ordinary_members(tx, slug, from_department_id)?;
        derived.as_slice()
    } else {
        person_ids
    };
    // Validate EVERY mover up front — all-or-nothing (no partial batch).
    for person_id in person_ids {
        if let Err(reason) = validate_mover(
            tx,
            slug,
            person_id,
            Some(from_department_id),
            true,
            HeadMoveRule::Refuse,
        )? {
            return Ok(TransferOutcome::Refused { reason });
        }
    }
    // WHO IS MOVING THE BATCH, AND WHERE TO. This verb took an `actor` and
    // asked nothing of it, and the route handed it `String::new()`, so any
    // caller could empty any department into any other and the ledger recorded
    // the author as the empty string.
    //
    // BOTH DEPARTMENTS ARE ASKED. Scope over the source alone would let a head
    // push its whole department into a unit it has no authority over; scope
    // over the destination alone would let it drain somebody else's. Enforced
    // only when the actor names a person row.
    if actor_out_of_scope(tx, slug, actor, from_department_id)?
        || actor_out_of_scope(tx, slug, actor, destination_id)?
    {
        return Ok(TransferOutcome::Refused { reason: TransferRefusal::ActorOutOfScope });
    }
    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        move_touches(tx, slug, destination_id, person_ids, intent, at, intent, false)
    })?;
    Ok(TransferOutcome::Applied { moved: person_ids.to_vec() })
}

/// Why an offboard was refused without writing anything (422 family class).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OffboardRefusal {
    /// No such person.
    UnknownPerson,
    /// The person is the CEO, who heads the company root and never departs.
    /// The CEO alone — a plain member of `office-of-the-ceo` is offboarded
    /// like anybody else. The variant keeps its `exec-root-protected` code
    /// for the machine surface; the NAME is history, the rule is one person.
    ExecRootProtected,
    /// The person OWNS manager/delegated goals — firing a goal-owning manager
    /// without succession would strand their reports; heads go through the R4
    /// fire-with-successor composite (appoint + offboard) instead.
    HeadNeedsSuccessor,
    /// The person has already departed.
    AlreadyDeparted,
    /// The ACTOR does not manage the person they are firing.
    ///
    /// `offboard_person` took an actor and never asked anything of it, so any
    /// person could fire any other. `hire` was bound and `offboard` was not —
    /// the sharpest asymmetry in the authz audit (the design record,
    /// closed by track B1 of the design record).
    ActorOutOfScope,
}

impl OffboardRefusal {
    /// The kebab-case 422 machine code (family convention).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownPerson => "unknown-person",
            Self::ExecRootProtected => "exec-root-protected",
            Self::HeadNeedsSuccessor => "head-needs-successor",
            Self::AlreadyDeparted => "already-departed",
            Self::ActorOutOfScope => "actor-out-of-scope",
        }
    }

    /// A one-line human detail for the 422 body.
    #[must_use]
    pub fn detail(&self) -> &'static str {
        match self {
            Self::UnknownPerson => "no such person",
            Self::ExecRootProtected => {
                "the CEO heads the company root and never departs; everybody else may be \
                 offboarded, wherever they sit"
            }
            Self::HeadNeedsSuccessor => {
                "the person owns goals as a manager; appoint a successor first"
            }
            Self::AlreadyDeparted => "the person has already departed",
            Self::ActorOutOfScope => {
                "you do not manage that person: firing somebody needs them inside the subtree \
                 you head"
            }
        }
    }
}

/// The outcome of an [`offboard_person`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OffboardOutcome {
    /// The offboard committed.
    Applied,
    /// Refused without touching a row.
    Refused {
        /// Why it was refused.
        reason: OffboardRefusal,
    },
}

/// Atomically offboard (FIRE) a person (P2) — org_ops family member 3, the op
/// `shutdown_person` deliberately excluded (shutdown is a re-wakeable PARK; this
/// is a departure). ONE `BEGIN IMMEDIATE` that supersedes any open transition —
/// EXCEPT a READY offboard handoff, which it ADOPTS as its graceful row (see
/// below) — writes a terminal `offboard` transition, flips `employment_state →
/// departed`, leaves the person in their department,
/// appends `staffing_history 'offboarded'`, and clears the launch-intent fence.
/// The person ROW is RETAINED (departed-retention — durable history/audit); the
/// converge reaps the pane reactively off `last_desired_active = 0`. Composes
/// typed store accessors only (fable containment).
///
/// # It does NOT clear the launch-intent fence, and that is the rule
///
/// This doc used to say it did. It does not, it must not, and the difference
/// is a fired person's last act. Every branch below leaves the person holding
/// an OPEN offboard transition -- an adopted READY row, or a freshly minted
/// `awaiting_handoff` one -- and the departure does not apply until they
/// release it. They have to be running to write that handoff. Clearing the
/// fence here would de-authorize them in the same commit, and the reconcile
/// CANCELS a pending structural transition for anybody the fence does not
/// admit (`activity::reconcile`, the abandon branch): the handoff would be
/// abandoned by the very op that asked for it.
///
/// So authorization is a DERIVED TERM, not a roster state: **Active, OR
/// holding an open offboard handoff.** The second half expires by itself --
/// once the handoff goes terminal the person has no active transition and is
/// not desired-active, which is exactly the condition the converge pass's F8
/// withdrawal half already sweeps (`converge_apply::cycle`, the `else` arm of
/// the withdraw filter). That sweep is where a departed person is
/// de-authorized, and `a_departed_person_is_de_authorized_once_the_handoff_is_terminal`
/// is the test that ties the two together -- which nothing did before.
///
/// # Errors
/// Propagates `rusqlite` failures lifted through the direct row transaction.
pub fn offboard_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<OffboardOutcome> {
    use OffboardRefusal as R;
    macro_rules! refuse {
        ($r:expr) => {
            return Ok(OffboardOutcome::Refused { reason: $r })
        };
    }

    // 0. Refusal guards (read-only, BEFORE the fence). THE CEO ALONE — a head
    //    may fire anyone in its own subtree, and the CEO holds every tree.
    //    Living beside the CEO stopped being a protection on 2026-08-13.
    if is_ceo(tx, slug, person_id)? {
        refuse!(R::ExecRootProtected);
    }
    let Some((employment, department_id)) =
        organization_rows::person_placement(tx, slug, person_id)?
    else {
        refuse!(R::UnknownPerson);
    };
    if employment == "departed" {
        refuse!(R::AlreadyDeparted);
    }
    // WHO IS FIRING. This function took an `actor` and asked nothing of it, so
    // any person could fire any other; `hire` was bound and `offboard` was not,
    // which the authz audit called its sharpest asymmetry.
    //
    // THE CHECK APPLIES ONLY WHEN THE ACTOR NAMES A PERSON, and that is a
    // deliberate rule rather than a convenience. `actor` has never been a
    // principal: it is a free-form audit string, and this crate's own corpus
    // proves it by firing as `operator`, as `op`, and as the empty string.
    // Gating on "the actor is not somebody I can find" would refuse every
    // caller that writes prose there.
    //
    // It is sound in both worlds, which is why it can land before credentials
    // exist. With enforcement OFF nothing authenticates the caller anyway, so
    // there is no attacker this could stop. With enforcement ON the route
    // OVERWRITES `actor` with the authenticated caller's principal, so the
    // value reaching here is always a real person and the check always applies.
    // A prose placeholder can never be a live bypass, because by the time it
    // could matter it can no longer arrive.
    let actor_is_a_person =
        !actor.is_empty() && organization_rows::person_placement(tx, slug, actor)?.is_some();
    if actor_is_a_person
        && !organization_rows::person_manages_department(tx, slug, actor, &department_id)?
    {
        refuse!(R::ActorOutOfScope);
    }
    // A goal-owning manager needs succession (the original check); so does ANY
    // department head, goal-owning or not (production-bug restoration: this
    // half of the guard was silently dropped by the atomic-reorg migration —
    // offboarding a headless-of-goals department head used to refuse and
    // instead became a silent no-op that left `departments.head_person_id`
    // pointing at the now-departed person). Keyed on `people.kind`, the SAME
    // signal the existing head-needs-successor check already uses
    // (`person_kind(...) != "worker"`) — NOT `departments.head_person_id`
    // directly, which several existing fixtures set without keeping
    // `people.kind` in sync (a raw-SQL seed shortcut, not a real headship).
    // The R4 composite (`offboard_department_head_with_successor` / TS
    // `offboardDepartmentHeadWithSuccessor`) is the sanctioned path for firing
    // a head; this direct verb refuses and sends the caller there.
    if organization_rows::person_kind(tx, slug, person_id)?.as_deref() == Some("head") {
        refuse!(R::HeadNeedsSuccessor);
    }

    // THE DAEMON AUTHORS THE LEDGER LINE. A caller used to have to type a
    // sentence before it could fire anybody, and this function threaded that
    // sentence through. Requiring prose gates nothing — authorization is the
    // gate — so the requirement is gone and the RECORD is not: the line names
    // the act and the authenticated principal who performed it, which is
    // strictly better provenance than free text and cannot be gamed. The
    // earlier defect this replaces was a synthetic `offboarded <person>` that
    // named nobody at all.
    let reason = authored_ledger_line("offboarded", actor);
    let reason = reason.as_str();

    // A READY offboard transition has ALREADY been released, which is the whole
    // thing the graceful row below exists to wait for: the staffing lifecycle
    // releases the transition BEFORE calling this op — the same
    // released-transition-first contract `bench_person` documents. Superseding
    // that row and minting a fresh `awaiting_handoff` one throws the release
    // away and puts the person back at the start of a grace window they can
    // never finish: the manifest change below marks them departed, so there is
    // nobody left to release the new row, and the reconcile therefore retains
    // the pane forever (the live staffing wedge this adoption was added to
    // fix). Adopt the ready row instead: the reconcile consumes it — applies
    // the departure and reaps the pane — exactly as if it had been minted here.
    let adopted = activity::rows::ready_open_transition_id(
        tx,
        slug,
        person_id,
        activity::TransitionAction::Offboard,
    )?;

    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::new();

        let transition_id = match &adopted {
            Some(id) => id.clone(),
            None => {
                // The terminal row's id carries the model's embedded sequence
                // (`transition:<seq>:<person>:offboard`): `validate` rejects any
                // other shape, so an `offboard:<person>:<at>` id durably poisoned
                // the store — every subsequent whole-ledger ingest failed
                // "corrupt store: activity" (the live #39-followup wedge).
                // Allocated inside the commit so a retried transaction simply
                // advances past the gap.
                let minted = format!(
                    "transition:{}:{person_id}:offboard",
                    crate::store::rows_txn::allocate_seq(
                        tx,
                        &activity::rows::transitions_counter_key(slug)
                    )?
                );

                // Supersede any open transition (same body as shutdown + the state flip).
                if let Some((_cancelled_id, touch)) = activity::rows::supersede_open_transition(
                    tx,
                    slug,
                    person_id,
                    &format!("superseded-by-offboard:{person_id}"),
                    at,
                )? {
                    touches.push(touch);
                }
                // The GRACEFUL offboard transition (`awaiting_handoff`): the departure
                // does not apply until the person releases it. This is the e2e
                // #39-followup contract. The row carries the person's current
                // placement, a non-empty
                // reason, and the standard handoff grace deadline — every `validate`
                // rule admits it on every later whole-ledger read.
                let placement = terminal_transition_context(tx, slug, person_id)?;
                let handoff_deadline_at = parse_iso_millis(at)
                    .map(|requested| iso_millis(requested + activity::HANDOFF_GRACE_MS))
                    .unwrap_or_else(|| at.to_string());
                touches.push(activity::rows::insert_awaiting_handoff_transition(
                    tx,
                    slug,
                    &minted,
                    person_id,
                    activity::TransitionAction::Offboard,
                    &placement,
                    None,
                    reason,
                    at,
                    &handoff_deadline_at,
                )?);
                minted
            }
        };
        // The ONE durable-departure writer, shared with `remove_department_tree`
        // (see [`depart_person_rows`]). Desired-off points AT the live
        // transition here: the reconcile retains the pane for exactly the
        // bounded grace window, then applies the departure when the release
        // lands.
        touches.extend(depart_person_rows(
            tx,
            slug,
            &Departure {
                person_id,
                department_id: &department_id,
                from_department_id: &department_id,
                active_transition_id: Some(&transition_id),
                reason,
            },
            at,
        )?);

        // The launch-intent fence stays for exactly the handoff window: the
        // reconcile's graceful machinery can only mint/complete a handoff for
        // a person who can run (`canCompleteHandoff`), and a departed person
        // is inert under `operationalPerson` regardless — so intent is never
        // demand for them, and the ordinary offboard lifecycle owns its
        // withdrawal when the handoff applies.

        // De-duplicate per changed ENTITY (person is touched by several accessors).
        let mut seen = std::collections::HashSet::new();
        touches.retain(|t| seen.insert((t.entity.clone(), t.entity_id.clone())));
        Ok(touches)
    })?;

    Ok(OffboardOutcome::Applied)
}

/// The durable fact "this person left the company", written in ONE place.
///
/// [`offboard_person`] (attended — a graceful handoff transition the person
/// still has to release) and [`remove_department_tree`] (unattended — the
/// department they would hand off to is deleted in the same transaction) are
/// both "firing", and the product says so in both surfaces. They therefore may
/// not disagree about what firing durably means. Before this existed, one flipped
/// `employment_state` and appended the ledger row while the other ran
/// `DELETE FROM people`, and the divergence was invisible because the copy on
/// both said "fires".
///
/// Callers own everything that differs — which transition (if any) is live,
/// whether the fence survives a grace window, whether a head is demoted. This
/// owns only what is the same:
///
/// * desired-off, pointing at `active_transition_id` when one is live;
/// * `employment_state → departed`;
/// * the row comes to rest at `department_id`, assigned = home;
/// * `staffing_history 'offboarded'` recording `from_department_id`, the unit
///   they actually left — which is NOT `department_id` when the removal
///   re-homes them out of a unit it is about to delete;
/// * their OPEN assignments released to `failed` (norm's authoritative vocab:
///   assignee departed → did not complete). A `failed` assignment renders its
///   queued effects inert — readers gate on the parent assignment's status — so
///   a departed person's effects never fire. Effect retirement stays a separate
///   supervision concern.
struct Departure<'a> {
    /// Who is leaving.
    person_id: &'a str,
    /// Where their retained row comes to rest, assigned = home.
    department_id: &'a str,
    /// The unit they actually LEFT, for the staffing ledger. It is NOT
    /// `department_id` when a subtree removal re-homes them out of a unit
    /// it is about to delete.
    from_department_id: &'a str,
    /// The live transition their pane's teardown waits on, when there is one.
    /// `None` is the unattended departure: nothing is left to release.
    active_transition_id: Option<&'a str>,
    /// The caller's own audit text. Never a synthetic placeholder — a fake
    /// audit record is worse than none.
    reason: &'a str,
}

fn depart_person_rows(
    tx: &Transaction<'_>,
    slug: &str,
    departure: &Departure<'_>,
    at: &str,
) -> rusqlite::Result<Vec<EventTouch>> {
    let Departure { person_id, department_id, from_department_id, active_transition_id, reason } =
        *departure;
    let touches = vec![
        activity::rows::upsert_person_activity_desired(
            tx,
            slug,
            person_id,
            false,
            active_transition_id,
            at,
        )?,
        organization_rows::set_employment_state(
            tx,
            slug,
            person_id,
            EmploymentState::Departed,
            at,
        )?,
        organization_rows::move_person(tx, slug, person_id, department_id, at)?,
    ];
    // Staffing ledger (its own D2 feed — no org_events touch).
    organization_rows::append_staffing_history(
        tx,
        slug,
        person_id,
        "offboarded",
        Some(from_department_id),
        None,
        reason,
        at,
    )?;
    Ok(touches)
}

/// Record the START DECISION for a person this transaction just created: ONE
/// `launch_intent` row, committed alongside the person row.
///
/// Creation brings people UP. The row is not a spawn and nothing in the writer
/// thread waits on a pane: the live reconciler's fence admits exactly the
/// people it names, and `project_activity_fence` turns a fenced person who is
/// not yet desired-active into `ActivityReason::Requested`, so the next
/// converge pass computes `active = true` and the runtime projection starts
/// them. Once they are up the fence stops contributing demand (a start
/// decision is not a residency permit) and the ordinary settle path — quiet
/// lease, routine idle park, withdrawal — is the ONLY route back down.
///
/// Deliberately NOT a `person_activity` desired-active write. `activity::reconcile`
/// recomputes `last_desired_active` from demand reasons on every pass, and
/// `project_activity_fence` suppresses the `Requested` reason for anyone
/// already desired-active — so pre-setting the flag here would erase the very
/// demand that brings the person up.
fn fence_started_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    touches: &mut Vec<EventTouch>,
) -> rusqlite::Result<()> {
    // STOP MEANS STOP, at the one helper every staffing verb shares. A hire or
    // a department creation while the operator has this company stood down
    // still creates the durable person and the durable unit — that is the
    // structure, and nothing about a stand-down makes it wrong — but it starts
    // NOBODY. See `store::stand_down`.
    //
    // Silent here and loud in `start_person`/`wake_person` on purpose. Those
    // two verbs mean "run this person", so refusing them without a reason
    // would leave the caller to invent one. This one is a side effect of a
    // structural verb that otherwise succeeded, and refusing the whole hire
    // would tell the caller their department was not created when it was.
    if stand_down::is_stood_down(tx, slug).map_err(|_| rusqlite::Error::InvalidQuery)? {
        return Ok(());
    }
    if let Some(touch) = launch_intent_rows::insert_person_fence(tx, slug, person_id)? {
        touches.push(touch);
    }
    Ok(())
}

/// Whether a freshly seeded person should come up. Only ACTIVE employment does:
/// a benched seed is durable and stopped by its own definition, and fencing it
/// would start somebody the caller explicitly said is not staffed.
fn seed_comes_up(employment_state: EmploymentState) -> bool {
    matches!(employment_state, EmploymentState::Active)
}

// hire_person — org_ops family member 4 (P2-f)
// ---------------------------------------------------------------------------

/// The durable seed a caller supplies to [`hire_person`] — re-export of the
/// manifest store's [`organization_rows::NewPersonSeed`] so a caller composes
/// ONE type across the family (org_ops owns the verb, the manifest store owns
/// the row shape).
pub use crate::store::organization_rows::NewPersonSeed;

/// An OWNED [`NewPersonSeed`] — the async writer (`CompanyDb::hire_person`) moves
/// the seed into its transaction closure, so it cannot hold the borrowed
/// `NewPersonSeed<'a>`. Borrow it back with [`OwnedNewPersonSeed::as_ref`] at the
/// call site. Kind is carried as the string the transport speaks
/// (`worker`/`head`/`executive`); an unknown value is coerced to `worker`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedNewPersonSeed {
    /// Display name (`people.name`).
    pub name: String,
    /// Role title (`people.title`).
    pub title: String,
    /// The person's mandate (`people.mandate`).
    pub mandate: String,
    /// worker / head / executive.
    pub kind: PersonKind,
    /// active / benched. Runtime launch remains independently desired-off.
    pub employment_state: EmploymentState,
    /// `resident` | `on-demand` (`people.activation`).
    pub activation: String,
    /// Tool grants (child `person_tools` rows).
    ///
    /// TOMBSTONE (chief-home-is-cwd §4e): the owned `skills`/`extensions`/
    /// `packages` lists went with [`NewPersonSeed`]'s — a hire selects no Pi
    /// resource.
    pub tools: Vec<String>,
    /// Project-local prompt template ids.
    pub prompts: Vec<String>,
}

impl OwnedNewPersonSeed {
    /// Borrow this owned seed as a [`NewPersonSeed`] for the op.
    #[must_use]
    pub fn as_ref(&self) -> NewPersonSeed<'_> {
        NewPersonSeed {
            name: &self.name,
            title: &self.title,
            mandate: &self.mandate,
            kind: self.kind,
            employment_state: self.employment_state,
            activation: &self.activation,
            tools: &self.tools,
            prompts: &self.prompts,
        }
    }
}

/// The [`valid_entity_id`] rule, spelled out for a refusal detail.
pub(crate) const ENTITY_ID_RULE: &str =
    "an entity id must be 1-64 characters of lowercase ASCII letters, \
                              digits and hyphens, starting with a letter";

pub(crate) fn valid_entity_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn valid_prompt_template(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.starts_with("prompts/")
        && value.ends_with(".md")
        && !value.contains('\\')
        && !value.starts_with('/')
        && value.split('/').all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

pub use crate::store::organization_spec::MAX_REFUSAL_ECHO_BYTES;

/// Render a caller-supplied value for a refusal detail: quoted and truncated
/// to [`MAX_REFUSAL_ECHO_BYTES`] on a char boundary.
pub(crate) fn echo(value: &str) -> String {
    let kept = organization_spec::bounded(value);
    if kept.len() == value.len() {
        format!("'{value}'")
    } else {
        format!("'{kept}'…")
    }
}

/// A rejected seed field, carrying the vocabulary that would have been
/// accepted.
///
/// The `field` alone was the whole refusal until #tools-refusal, and the
/// observed consequence is why this type exists: told only `head.tools`, an
/// operator's CEO could not find a legal value, concluded the field was
/// unusable, and silently omitted it across five departments and 23 people.
/// A policy "no" enumerates — the same discipline `org_hire` already applies
/// to skill/extension/package ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedRejection {
    /// The offending seed field path (e.g. `tools`, `taskClass`).
    pub field: String,
    /// One line naming the constraint and the values that would be accepted.
    pub detail: String,
}

impl SeedRejection {
    /// A rejection of `field`, explained by `detail`.
    #[must_use]
    pub fn new(field: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { field: field.into(), detail: detail.into() }
    }

    /// The same rejection, re-rooted under a seed path (`head.`, `staff[2].`).
    #[must_use]
    pub fn under(self, prefix: &str) -> Self {
        Self { field: format!("{prefix}{}", self.field), detail: self.detail }
    }
}

/// Validate every seed field reconstructed into a TypeScript `PersonRecord`.
/// The exact field is returned, with the vocabulary that would have been
/// accepted, so malformed durable-API calls are semantic no-write refusals a
/// caller can actually correct instead of rows the launcher cannot load.
pub(crate) fn validate_new_person_seed(
    seed: &NewPersonSeed<'_>,
    expected_kind: PersonKind,
) -> Result<(), SeedRejection> {
    if seed.kind != expected_kind {
        return Err(SeedRejection::new(
            "kind",
            format!("this seed position requires kind {expected_kind:?}, got {:?}", seed.kind),
        ));
    }
    for (value, field) in [(seed.name, "name"), (seed.title, "title"), (seed.mandate, "mandate")] {
        if value.trim().is_empty() {
            return Err(SeedRejection::new(
                field,
                format!("{field} is required and must not be blank"),
            ));
        }
    }
    if !matches!(seed.activation, "resident" | "on-demand") {
        return Err(SeedRejection::new(
            "activation",
            format!("activation must be one of resident, on-demand; got {}", echo(seed.activation)),
        ));
    }
    if seed.tools.iter().any(|tool| tool.trim().is_empty()) {
        return Err(SeedRejection::new(
            "tools",
            tools_vocabulary(expected_kind, "every tools entry must be a non-blank tool name"),
        ));
    }
    // The silent twin of the blank-name check above: until this check, `tools`
    // accepted every meaningless string. `["bahs"]` was stored verbatim into
    // `person_tools`, granted nothing, and said nothing — a person who reads as
    // configured and holds none of what was asked for.
    if let Some(unknown) = organization_spec::undeclarable_tool(seed.tools) {
        return Err(SeedRejection::new(
            "tools",
            tools_vocabulary(
                expected_kind,
                &format!("{} is not a declarable tool name", echo(unknown)),
            ),
        ));
    }
    // TOMBSTONE (chief-home-is-cwd §4e): the blank-entry check over the seed's
    // `skills`/`extensions`/`packages` stood here. There are no resource fields
    // left to be blank — Pi owns an agent's skills — so the rule has no subject.
    for (index, prompt) in seed.prompts.iter().enumerate() {
        if !valid_prompt_template(prompt) {
            return Err(SeedRejection::new(
                format!("prompts[{index}]"),
                format!(
                    "every prompts entry must be a repo-relative 'prompts/<name>.md' path with no \
                     '.', '..', backslash or leading slash; got {}",
                    echo(prompt)
                ),
            ));
        }
    }
    Ok(())
}

/// The one-line `tools` refusal: the rule that fired, then the vocabulary that
/// would have been accepted. The vocabulary itself is
/// [`organization_spec::declarable_tools_sentence`] — the module that owns the
/// builtin list owns this too, so the refusal can never enumerate a set the
/// authoring rules disagree with.
///
/// The clause about what omitting `tools` does is GONE, and its removal is the
/// point: it used to warn that an omitted `tools` granted nothing, which was
/// true and was the defect. Since the operator made the Pi builtin floor
/// unconditional (2026-08-10) omitting the field costs a person nothing, so a
/// warning about it would now be false. The words track the behaviour; when
/// the behaviour was fixed the warning had to go rather than be softened.
fn tools_vocabulary(expected_kind: PersonKind, rule: &str) -> String {
    format!("{rule}. In full: {}", organization_spec::declarable_tools_sentence(expected_kind))
}

/// Why a hire was refused without writing anything (422 family class).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HireRefusal {
    /// The destination department does not exist.
    UnknownDepartment,
    /// The destination department is paused (assertActiveDestination).
    DestinationPaused,
    /// A person with that id already exists (hire NEVER resurrects/overwrites —
    /// re-employment is a separate recall, not a hire).
    DuplicatePersonId,
    /// The requester no longer manages the destination in normalized rows.
    RequesterOutOfScope,
    /// A seed field failed preflight validation; `field` is the offending path.
    InvalidSeed {
        /// The offending seed field path (e.g. `name`, `model`).
        field: String,
        /// One line naming the constraint and the accepted vocabulary.
        detail: String,
    },
}

impl HireRefusal {
    /// The kebab-case 422 machine code (family convention).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownDepartment => "unknown-department",
            Self::DestinationPaused => "destination-paused",
            Self::DuplicatePersonId => "duplicate-person-id",
            Self::RequesterOutOfScope => "requester-out-of-scope",
            Self::InvalidSeed { .. } => "invalid-seed",
        }
    }

    /// A one-line human detail for the 422 body (carries the field path for
    /// `invalid-seed`).
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::UnknownDepartment => "no such department".to_string(),
            Self::DestinationPaused => "the destination department is paused".to_string(),
            Self::DuplicatePersonId => "a person with that id already exists".to_string(),
            Self::RequesterOutOfScope => {
                "the requester no longer manages the destination department".to_string()
            }
            Self::InvalidSeed { field, detail } => {
                format!("invalid hire seed: {field} — {detail}")
            }
        }
    }
}

/// The outcome of a [`hire_person`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HireOutcome {
    /// The hire committed (durable person + child rows + the launch fence;
    /// ZERO panes spawned in the writer transaction).
    Applied,
    /// Refused without touching a row.
    Refused {
        /// Why it was refused.
        reason: HireRefusal,
    },
}

/// Atomically HIRE a new person into a department (P2-f) — org_ops family member
/// 4. ONE `BEGIN IMMEDIATE` that inserts the `people` row (placed in the
/// hiring department, requested active/benched employment) + its child rows, seeds a
/// complete `person_activity` row, writes ONE `launch_intent` fence row for an
/// ACTIVE seed (hiring somebody IS the decision to bring them up — the
/// reconciler converges to that decision and the settle path is the only route
/// back down; a BENCHED seed is fenceless and stays stopped),
/// appends `staffing_history 'hired'` (from = NULL → to = department),
/// and restores canonical head-first department-grouped people order via
/// `refresh_people_order`. NO pane is spawned inside this transaction.
/// Composes typed store accessors only
/// (fable containment) — no raw cross-store SQL. Returns a VALUE, never throws
/// on a refusal.
///
/// # Errors
/// Propagates SQL failures from the transaction.
#[allow(clippy::too_many_arguments)]
pub fn hire_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    department_id: &str,
    seed: &NewPersonSeed<'_>,
    requester_person_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<HireOutcome> {
    hire_person_authorized(
        tx,
        slug,
        person_id,
        department_id,
        seed,
        Some(requester_person_id),
        at,
        actor,
    )
}

/// Production entry point supporting either a transaction-attested manager or
/// an explicitly attributed direct operator.
#[allow(clippy::too_many_arguments)]
pub fn hire_person_authorized(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    department_id: &str,
    seed: &NewPersonSeed<'_>,
    requester_person_id: Option<&str>,
    at: &str,
    actor: &str,
) -> rusqlite::Result<HireOutcome> {
    macro_rules! refuse {
        ($r:expr) => {
            return Ok(HireOutcome::Refused { reason: $r })
        };
    }

    // 0. Refusal guards — read-only, BEFORE the fence, write nothing.
    //
    // ONE definition of eligibility: the pure projection, which the route
    // preflights and materializes before this transaction exists. See
    // `create_department_with_staff_unit` for why the rules live there and not
    // in a second copy here.
    let view = eligibility_view(tx, slug)?;
    let projection = org_projection::check_hire(
        &view,
        &org_projection::HireProposal { person_id, department_id, seed, requester_person_id, at },
    );
    if let Err(reason) = projection {
        refuse!(reason);
    }

    let reason = format!("hired {person_id} into {department_id}");

    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::new();

        // Manifest: insert the people row + tool grants (placed in dept,
        // employment active, next gapless ordinal). Composes the manifest suite.
        touches.push(organization_rows::insert_person(
            tx,
            slug,
            person_id,
            department_id,
            seed,
            at,
        )?);

        // Activity: seed the complete person_activity row. It is written
        // desired-off and stays that way here — `activity::reconcile` owns
        // `last_desired_active` and derives it from demand every pass; the
        // launch fence below is what supplies that demand.
        touches.push(activity::rows::insert_person_activity_desired_off(
            tx,
            slug,
            person_id,
            seed.employment_state,
            department_id,
            true,
            at,
        )?);

        // Launch fence: a hire IS the decision to bring that person up. One
        // row, no pane in this transaction; the reconciler converges to it and
        // the settle path is the only thing that stops them again.
        if seed_comes_up(seed.employment_state) {
            fence_started_person(tx, slug, person_id, &mut touches)?;
        }

        // Staffing ledger (its own D2 feed — no org_events touch): hired, from
        // NULL → to the hiring department.
        organization_rows::append_staffing_history(
            tx,
            slug,
            person_id,
            "hired",
            None,
            Some(department_id),
            &reason,
            at,
        )?;

        // H1: re-assert the gapless 0..N ordinal bijection (the new row is at
        // MAX+1 initially; canonicalization groups it with its department
        // and keeps that department's head first).
        touches.extend(organization_rows::refresh_people_order(tx, slug, at)?);

        // De-duplicate per changed ENTITY (insert + refresh may both name the
        // new person row; the feed carries ONE person event per entity).
        let mut seen = std::collections::HashSet::new();
        touches.retain(|t| seen.insert((t.entity.clone(), t.entity_id.clone())));
        Ok(touches)
    })?;
    Ok(HireOutcome::Applied)
}

/// Why a pause/resume was refused without writing anything (422 family class).
/// Shared by both verbs — `AlreadyPaused` is pause-only, `NotPaused` resume-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseRefusal {
    /// No such department.
    UnknownDepartment,
    /// The department is (part of) the executive root — the CEO's chain never
    /// pauses (the landed task-#2 invariant: exec-root is always up).
    ExecRootProtected,
    /// The department is already paused (pause is idempotent-refuse, not a no-op
    /// write — a redundant pause must not churn the feed).
    AlreadyPaused,
    /// The department is not paused (nothing to resume).
    NotPaused,
    /// The actor names a real person who does not manage the department being
    /// paused or resumed. Authority is the subtree, never the job title.
    ActorOutOfScope,
}

impl PauseRefusal {
    /// The kebab-case 422 machine code (family convention).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownDepartment => "unknown-department",
            Self::ExecRootProtected => "exec-root-protected",
            Self::AlreadyPaused => "already-paused",
            Self::NotPaused => "not-paused",
            Self::ActorOutOfScope => "actor-out-of-scope",
        }
    }

    /// A one-line human detail for the 422 body.
    #[must_use]
    pub fn detail(&self) -> &'static str {
        match self {
            Self::UnknownDepartment => "no such department",
            Self::ExecRootProtected => {
                "the company root never pauses; every other department, `office-of-the-ceo` \
                 included, may be paused"
            }
            Self::AlreadyPaused => "the department is already paused",
            Self::NotPaused => "the department is not paused",
            Self::ActorOutOfScope => {
                "the actor does not manage the department they are pausing or resuming"
            }
        }
    }
}

/// The outcome of a [`pause_department`] / [`resume_department`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseOutcome {
    /// The state flip committed.
    Applied,
    /// Refused without touching a row.
    Refused {
        /// Why it was refused.
        reason: PauseRefusal,
    },
}

/// Atomically PAUSE a department (P2-h): ONE `BEGIN IMMEDIATE` txn that flips
/// `departments.state → 'paused'` AND, in the SAME txn, sweeps the paused
/// SUBTREE (the dept + all recursive descendant departments) — for every
/// assigned member it DELETES the `launch_intent` fence row, SUPERSEDES any
/// open transition (`superseded-by-pause:<dept>`), completes an active
/// lifecycle with a synthetic unit-stop handoff (or a truthful abandoned park
/// when only a launch fence existed), and persists a complete desired-off
/// activity projection. The direct transaction emits one
/// `department` touch plus the changed activity/transition/launch-intent rows.
///
/// Why the subtree sweep is IN the txn (norm-n1 ruling, overriding a reactive
/// deferral): these are DURABLE ROW changes, and an uncleared `launch_intent`
/// RE-CREEPS a pane (#534 — the live recurring bug where a reactive clear races
/// and a paused member's pane comes back). Atomic clearing is the robust fix.
/// Only the pane KILL stays out-of-txn (the converge reaps paused members
/// reactively off the committed rows — the #448 contract); no pane actuation
/// lives inside this SQL txn.
///
/// A paused department also refuses transfers into it (`destination-paused`,
/// transfer verb) because both read the SAME `departments.state` column.
/// Composes typed store accessors only (fable containment). The executive root
/// can never be paused (`exec-root-protected`, parity with the task-#2 invariant).
///
/// # Errors
/// Propagates `rusqlite` failures lifted through the direct row transaction.
pub fn pause_department(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<PauseOutcome> {
    set_department_paused_op(tx, slug, department_id, true, at, actor)
}

/// Atomically RESUME a department (P2-h): ONE `BEGIN IMMEDIATE` txn that flips
/// `departments.state → 'active'` and emits the single `department` org_events
/// touch. Resume restores STATE ONLY — nobody spawns (THE
/// HARD RULE: people come up when the head asks, not because a dept resumed).
///
/// # Errors
/// Propagates `rusqlite` failures lifted through the direct row transaction.
pub fn resume_department(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<PauseOutcome> {
    set_department_paused_op(tx, slug, department_id, false, at, actor)
}

/// Resume a set of departments in one direct transaction. A batch caller may
/// treat an already-active department as satisfied (`skip_active`) while the
/// strict single-department endpoint retains its `not-paused` refusal. Resume
/// changes state only: it never creates launch intent or starts a person.
pub fn resume_departments(
    tx: &Transaction<'_>,
    slug: &str,
    department_ids: &[String],
    skip_active: bool,
    at: &str,
    actor: &str,
) -> rusqlite::Result<PauseOutcome> {
    use PauseRefusal as R;
    if department_ids.is_empty() {
        return Ok(PauseOutcome::Refused { reason: R::UnknownDepartment });
    }
    let mut seen = std::collections::HashSet::new();
    for department_id in department_ids {
        if !seen.insert(department_id) {
            continue;
        }
        let Some(state) = organization_rows::department_state(tx, slug, department_id)? else {
            return Ok(PauseOutcome::Refused { reason: R::UnknownDepartment });
        };
        // THE COMPANY ROOT ALONE. The CEO's other units are ordinary
        // departments, and a head may stop and start anything in its subtree.
        if department_is_company_root(tx, slug, department_id)? {
            return Ok(PauseOutcome::Refused { reason: R::ExecRootProtected });
        }
        // EVERY department in the batch is asked, not the first one. A batch
        // is all-or-nothing, so one unit the actor does not manage refuses the
        // whole call — checking a sample would make the batch verb a way round
        // the single verb's guard.
        if actor_out_of_scope(tx, slug, actor, department_id)? {
            return Ok(PauseOutcome::Refused { reason: R::ActorOutOfScope });
        }
        if state != "paused" && !skip_active {
            return Ok(PauseOutcome::Refused { reason: R::NotPaused });
        }
    }

    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::new();
        let mut emitted = std::collections::HashSet::new();
        for department_id in department_ids {
            if !emitted.insert(department_id) {
                continue;
            }
            if organization_rows::department_state(tx, slug, department_id)?
                == Some("paused".to_string())
            {
                touches.push(organization_rows::set_department_paused(
                    tx,
                    slug,
                    department_id,
                    false,
                    at,
                )?);
            }
        }
        Ok(touches)
    })?;
    Ok(PauseOutcome::Applied)
}

/// Shared body for [`pause_department`] / [`resume_department`]: guards (read-
/// only, before the transaction) → one direct row state flip.
fn set_department_paused_op(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    paused: bool,
    at: &str,
    actor: &str,
) -> rusqlite::Result<PauseOutcome> {
    use PauseRefusal as R;
    macro_rules! refuse {
        ($r:expr) => {
            return Ok(PauseOutcome::Refused { reason: $r })
        };
    }

    // 0. Refusal guards (read-only, BEFORE the fence — write nothing).
    let Some(state) = organization_rows::department_state(tx, slug, department_id)? else {
        refuse!(R::UnknownDepartment);
    };
    // THE COMPANY ROOT ALONE. Pausing the root would stop the whole company
    // including the CEO, and the CEO is the one person nobody may act on. Every
    // other unit — `office-of-the-ceo` included — is an ordinary department a
    // head may stop. The CEO's own runtime is protected INSIDE the sweep below
    // rather than by refusing the unit, so "stop the department, keep him
    // around" is expressible instead of refused.
    if department_is_company_root(tx, slug, department_id)? {
        refuse!(R::ExecRootProtected);
    }
    // WHO IS STOPPING OR STARTING THE UNIT. Pausing a department stops every
    // person under it; this verb took an `actor` and asked nothing of it, and
    // the route handed it `String::new()`. Scope over the department, enforced
    // only when the actor names a person row.
    if actor_out_of_scope(tx, slug, actor, department_id)? {
        refuse!(R::ActorOutOfScope);
    }
    // Idempotent-refuse: a redundant flip must not churn the feed.
    if paused && state == "paused" {
        refuse!(R::AlreadyPaused);
    }
    if !paused && state != "paused" {
        refuse!(R::NotPaused);
    }

    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        // 1. The state flip (department upsert touch) — first in the seq run.
        let mut touches =
            vec![organization_rows::set_department_paused(tx, slug, department_id, paused, at)?];

        // 2. Pause ONLY: sweep the paused subtree IN-txn (norm-n1 ruling). For
        //    every member of the dept + its recursive descendant departments,
        //    DELETE the launch_intent fence (#534), SUPERSEDE any open
        //    transition, complete an active lifecycle with an atomic synthetic
        //    unit-stop handoff (or abandon a fence-only park truthfully), and
        //    rewrite that member's desired-off activity projection from
        //    authoritative organization rows. This narrow repair is what lets
        //    pause recover stale/partial projection columns without teaching the
        //    generic activity reader to tolerate unrelated corruption. The pane
        //    KILL stays out-of-txn. Resume restores state only and spawns nobody.
        if paused {
            let marker = format!("superseded-by-pause:{department_id}");
            // THE CEO IS NEVER SWEPT. Pausing a unit stops every member of its
            // subtree, and until 2026-08-13 the CEO could not be reached by
            // that sweep only because the whole executive root refused to
            // pause at all. Now that `office-of-the-ceo` is an ordinary
            // department, a CEO homed there would be stopped by a pause of the
            // unit it sits in — the company's own supervisor taken down as a
            // side effect of stopping a team. The exemption moves from the
            // REFUSAL to the SWEEP, which is what the operator asked for in
            // words: "shut down a department, keep him around". The unit
            // pauses, its people stop, the CEO keeps running.
            let ceo = organization_rows::root_department_head(tx, slug)?;
            for member in organization_rows::department_subtree_members(tx, slug, department_id)? {
                if ceo.as_deref() == Some(member.as_str()) {
                    continue;
                }
                let (has_projection, lifecycle_active) =
                    activity::rows::pause_activity_status(tx, slug, &member)?;
                if let Some((_cancelled_id, touch)) =
                    activity::rows::supersede_open_transition(tx, slug, &member, &marker, at)?
                {
                    touches.push(touch);
                }
                let deleted_fence =
                    launch_intent_rows::delete_person_fence(tx, slug, &member, "department-pause")?;
                let fence_was_deleted = deleted_fence.is_some();
                if let Some(touch) = deleted_fence {
                    touches.push(touch);
                }

                let Some((employment, member_department_id)) =
                    organization_rows::person_placement(tx, slug, &member)?
                else {
                    continue;
                };
                let employment = match employment.as_str() {
                    "active" => EmploymentState::Active,
                    "benched" => EmploymentState::Benched,
                    "departed" => continue,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                if lifecycle_active || fence_was_deleted {
                    let transition_id = format!(
                        "transition:{}:{member}:park",
                        crate::store::rows_txn::allocate_seq(
                            tx,
                            &activity::rows::transitions_counter_key(slug),
                        )?
                    );
                    if lifecycle_active {
                        // A plain `cancelled` row carrying the unit-stop intent:
                        // the stop superseded this member's live lifecycle, and
                        // that is the entire fact. It is deliberately NOT
                        // `abandoned` — `abandoned_at` means the release was
                        // provably unreachable, and here the member was running
                        // fine; the unit was stopped out from under them.
                        //
                        // TOMBSTONE (#751-P4): this call used to be
                        // `insert_cancelled_transition_with_reflection` and passed
                        // a FABRICATED five-field handoff ("Auto-handoff for unit
                        // stop: reflection fence removed.") that no reader ever
                        // consumed. It existed only because the row shape demanded
                        // a payload. The payload is deleted product-wide, so the
                        // fabrication is gone with it — the row now says exactly
                        // what happened and invents nothing.
                        let intent_id = format!("unit-stop:{department_id}:{transition_id}");
                        touches.push(activity::rows::insert_cancelled_transition(
                            tx,
                            slug,
                            &transition_id,
                            &member,
                            activity::TransitionAction::Park,
                            Some(&member_department_id),
                            &intent_id,
                            &marker,
                            at,
                        )?);
                    } else {
                        // A launch fence without active lifecycle evidence means
                        // nobody was ever running to release anything. Preserve
                        // the truthful abandoned terminal record while still
                        // withdrawing its authorization atomically.
                        touches.push(activity::rows::insert_abandoned_transition(
                            tx,
                            slug,
                            &transition_id,
                            &member,
                            activity::TransitionAction::Park,
                            Some(&member_department_id),
                            None,
                            &format!("paused {member} with department {department_id}"),
                            at,
                        )?);
                    }
                }

                if has_projection || lifecycle_active || fence_was_deleted {
                    touches.push(activity::rows::insert_person_activity_desired_off(
                        tx,
                        slug,
                        &member,
                        employment,
                        &member_department_id,
                        false,
                        at,
                    )?);
                }
            }
        }

        Ok(touches)
    })?;

    Ok(PauseOutcome::Applied)
}

/// Why a bench was refused without writing anything (422 family class).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchRefusal {
    /// No such person.
    UnknownPerson,
    /// The person is the CEO, who heads the company root and is never
    /// benched. The CEO alone; where anybody else sits protects nothing.
    ExecRootProtected,
    /// The person is already benched.
    AlreadyBenched,
    /// The person has departed — a departed person cannot be benched.
    AlreadyDeparted,
    /// The actor names a real person who does not manage the department the
    /// target is homed in. Authority is the subtree, never the job title.
    ActorOutOfScope,
}

impl BenchRefusal {
    /// The kebab-case 422 machine code (family convention).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownPerson => "unknown-person",
            Self::ExecRootProtected => "exec-root-protected",
            Self::AlreadyBenched => "already-benched",
            Self::AlreadyDeparted => "already-departed",
            Self::ActorOutOfScope => "actor-out-of-scope",
        }
    }

    /// A one-line human detail for the 422 body.
    #[must_use]
    pub fn detail(&self) -> &'static str {
        match self {
            Self::UnknownPerson => "no such person",
            Self::ExecRootProtected => {
                "the CEO heads the company root and is never benched; everybody else may be, \
                 wherever they sit"
            }
            Self::AlreadyBenched => "the person is already benched",
            Self::AlreadyDeparted => "the person has departed and cannot be benched",
            Self::ActorOutOfScope => {
                "the actor does not manage the department the person they are benching is homed in"
            }
        }
    }
}

/// The outcome of a [`bench_person`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchOutcome {
    /// The bench committed.
    Applied,
    /// Refused without touching a row.
    Refused {
        /// Why it was refused.
        reason: BenchRefusal,
    },
}

/// Immutable identity of one committed released bench lifecycle.
///
/// This value is carried only inside Rust. The structural HTTP response stays
/// data-free; the live daemon uses the identity to acknowledge the exact
/// operation only after a fresh tagged runtime audit proves the pane absent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BenchCompletionKey {
    /// The released transition id, which is the durable operation identity.
    pub operation_id: String,
    /// The person whose pane must disappear.
    pub person_id: String,
}

/// The outcome of the complete released bench lifecycle transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchLifecycleOutcome {
    /// The durable lifecycle committed.
    Applied {
        /// Exact identity the completion registry must await — `None` when
        /// there is no pane whose disappearance could ever be acknowledged.
        completion: Option<BenchCompletionKey>,
    },
    /// Refused without touching a row.
    Refused {
        /// Why it was refused.
        reason: BenchRefusal,
    },
}

/// Atomically BENCH a person (P2) — org_ops family member 4, the durable
/// idle/bench distinct from `shutdown_person`'s transient re-wakeable PARK and
/// from `offboard_person`'s terminal departure. ONE `BEGIN IMMEDIATE` that
/// supersedes any open transition, clears the activity transition pointer while
/// setting `person_activity` desired-off so the converge reaps the pane
/// reactively, flips `employment_state → benched`, appends `staffing_history
/// 'benched'`, and clears the launch-intent fence. A lifecycle command has
/// already written its valid released transition before it reaches this named
/// SQL mutation; bench must not manufacture a second terminal transition that
/// cannot be represented by the activity ledger. The person ROW and their placement are
/// RETAINED (bench is reversible via `set_employment_state(Active)` / a recall
/// verb) — bench does NOT move the person or renumber ordinals, so the H1
/// whole-company gapless ordinal bijection is preserved untouched. Composes
/// typed store accessors only (fable containment) — zero raw cross-store SQL,
/// zero new accessor, zero DDL (`employment_state IN ('active','benched',
/// 'departed')` already admits `benched`).
///
/// # Errors
/// Propagates `rusqlite` failures lifted through the direct row transaction.
pub fn bench_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<BenchOutcome> {
    use BenchRefusal as R;
    macro_rules! refuse {
        ($r:expr) => {
            return Ok(BenchOutcome::Refused { reason: $r })
        };
    }

    // 0. Refusal guards (read-only, BEFORE the fence). THE CEO ALONE.
    if is_ceo(tx, slug, person_id)? {
        refuse!(R::ExecRootProtected);
    }
    let Some((employment, department_id)) =
        organization_rows::person_placement(tx, slug, person_id)?
    else {
        refuse!(R::UnknownPerson);
    };
    if employment == "departed" {
        refuse!(R::AlreadyDeparted);
    }
    if employment == "benched" {
        refuse!(R::AlreadyBenched);
    }
    // WHO IS BENCHING WHOM. Scope over the department the target lives in,
    // enforced only when the actor names a person row — the B1 rule stated
    // once on `actor_names_a_person`.
    if actor_out_of_scope(tx, slug, actor, &department_id)? {
        refuse!(R::ActorOutOfScope);
    }

    let reason = format!("benched {person_id}");

    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::new();

        // A ready open transition has already been RELEASED for THIS bench by
        // the staffing lifecycle: adopt it, don't supersede it, exactly as
        // `offboard_person_atomic` already does via `ready_open_transition_id`
        // (class-D fix — bench had the same unconditional-supersede bug as
        // `move_touches`/transfer).
        // Otherwise, supersede any open transition (same body as shutdown + the
        // state flip); superseding cancels it and mints nothing bench-side to
        // take its place, so `None` below is correct on that path — there is
        // nothing left to consume.
        let adopted_id = activity::rows::ready_open_transition_id(
            tx,
            slug,
            person_id,
            activity::TransitionAction::Park,
        )?;
        if adopted_id.is_none() {
            if let Some((_cancelled_id, touch)) = activity::rows::supersede_open_transition(
                tx,
                slug,
                person_id,
                &format!("superseded-by-bench:{person_id}"),
                at,
            )? {
                touches.push(touch);
            }
        }
        // Desired-off so the converge reaps the pane (row + placement retained).
        // On the adopted path, point AT the ready transition instead of
        // clearing it — `LiveOrganizationProjection::reconstruct` (writer.rs)
        // rebuilds the org_documents "activity" blob's per-person
        // `activeTransitionId` from THIS column after every commit, so a
        // cleared pointer orphans the adopted row: nothing points at it, no
        // later reconcile pass (TS's `reconcileOrganizationActivity` or
        // chiefd's own duty) can ever find it to flip it `ready` -> `applied`,
        // and it sits stranded forever even though the bench itself applied
        // cleanly. `offboard_person` already does this (`Some(&transition_id)`
        // below its own adoption) for the identical reason. On the
        // NON-adopted path `adopted_id` is `None` and the transition was just
        // cancelled above, so `None` here is correct — there is nothing to
        // point at. Never invent a second `bench:*` terminal row: the
        // lifecycle already persisted the valid, released transition, and a
        // direct terminal row here lacks that representation and makes the
        // normalized activity document unreadable on its next publish.
        // Staffing history below is the atomic audit of this bench.
        touches.push(activity::rows::upsert_person_activity_desired(
            tx,
            slug,
            person_id,
            false,
            adopted_id.as_deref(),
            at,
        )?);
        // Manifest: employment → benched (person + placement retained; no move,
        // no ordinal renumber — H1 bijection is untouched by a bench).
        touches.push(organization_rows::set_employment_state(
            tx,
            slug,
            person_id,
            EmploymentState::Benched,
            at,
        )?);

        // Staffing ledger (its own D2 feed — no org_events touch). `from` = the
        // retained home; `to` = None (bench does not re-home).
        organization_rows::append_staffing_history(
            tx,
            slug,
            person_id,
            "benched",
            Some(&department_id),
            None,
            &reason,
            at,
        )?;

        // Drop the launch-intent fence row.
        if let Some(touch) = launch_intent_rows::delete_person_fence(tx, slug, person_id, "bench")?
        {
            touches.push(touch);
        }

        // De-duplicate per changed ENTITY (person is touched by several accessors).
        let mut seen = std::collections::HashSet::new();
        touches.retain(|t| seen.insert((t.entity.clone(), t.entity_id.clone())));
        Ok(touches)
    })?;

    Ok(BenchOutcome::Applied)
}

/// Atomically prepare the server-owned released park transition and bench a
/// running person.  This is the lifecycle authority for the manager bench
/// command; callers never supply a transition.
///
/// "Server-owned" is the point: the transition is written straight into `ready`
/// here rather than opened `awaiting_handoff` and waited on. Bench is a manager
/// command against a person who may be mid-thought, and blocking it on the
/// pane's cooperation cost a round-trip per bench and stalled real moves. The
/// transition still exists because an applied transition is what sheds launch
/// intent and drives the pane teardown.
///
/// The preflight deliberately runs before the historical transition is read, so
/// a retry reports [`BenchRefusal::AlreadyBenched`] rather than treating a
/// completed bench as a stale handoff.
pub fn bench_person_lifecycle(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<BenchLifecycleOutcome> {
    use BenchRefusal as R;
    // THE CEO ALONE, matching the direct bench above.
    if is_ceo(tx, slug, person_id)? {
        return Ok(BenchLifecycleOutcome::Refused { reason: R::ExecRootProtected });
    }
    let Some((employment, department_id)) =
        organization_rows::person_placement(tx, slug, person_id)?
    else {
        return Ok(BenchLifecycleOutcome::Refused { reason: R::UnknownPerson });
    };
    if employment == "departed" {
        return Ok(BenchLifecycleOutcome::Refused { reason: R::AlreadyDeparted });
    }
    if employment == "benched" {
        return Ok(BenchLifecycleOutcome::Refused { reason: R::AlreadyBenched });
    }
    // The reflected lifecycle asks the same question as the direct bench: a
    // second door onto one mutation must not be a second answer about who may
    // walk through it.
    if actor_out_of_scope(tx, slug, actor, &department_id)? {
        return Ok(BenchLifecycleOutcome::Refused { reason: R::ActorOutOfScope });
    }

    let manifest = organization_rows::reconstruct(tx, slug)
        .map_err(|_| rusqlite::Error::QueryReturnedNoRows)?
        .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let mut completion = None;
    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        // Reuse an already-released park exactly as the direct bench operation
        // does. Otherwise the server writes the released transition into
        // normalized rows here, before the structural operation adopts it.
        let mut ledger = activity::rows::read_rows(tx, slug, &manifest)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        if let Some(id) = activity::rows::ready_open_transition_id(
            tx,
            slug,
            person_id,
            activity::TransitionAction::Park,
        )? {
            completion =
                Some(BenchCompletionKey { operation_id: id, person_id: person_id.to_string() });
            return Ok(Vec::new());
        }
        let state = ledger.people.get_mut(person_id).ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let id = format!("transition:{}:{person_id}:park", ledger.next_transition_sequence);
        let transition = activity::GracefulTransition {
            id: id.clone(),
            person_id: person_id.to_string(),
            action: activity::TransitionAction::Park,
            reason: format!("Bench '{person_id}' after a bounded handoff."),
            intent_id: None,
            placement_department_id: state.last_department_id.clone(),
            to_department_id: None,
            // Born `Ready`: the server is the one releasing it, in this same
            // transaction, so there is never a moment at which anybody could
            // observe it open. `handoff_deadline_at == requested_at` says the
            // same thing — a grace window of zero, because nothing is being
            // waited for. (Until #751-P4 this literal also carried a fabricated
            // five-field handoff so the row would satisfy the payload
            // requirement; the payload is deleted product-wide and the
            // fabrication went with it. `Ready` alone is the whole fact now.)
            status: activity::TransitionStatus::Ready,
            requested_at: at.to_string(),
            handoff_deadline_at: at.to_string(),
            applied_at: None,
            cancelled_at: None,
            forced_at: None,
            abandoned_at: None,
        };
        state.active_transition_id = Some(id.clone());
        state.updated_at = at.to_string();
        ledger.transitions.insert(id.clone(), transition);
        ledger.transition_order.push(id.clone());
        ledger.next_transition_sequence += 1;
        ledger.updated_at = at.to_string();
        completion =
            Some(BenchCompletionKey { operation_id: id, person_id: person_id.to_string() });
        activity::rows::write_rows(tx, slug, &ledger, &manifest)
    })?;

    match bench_person(tx, slug, person_id, at, actor)? {
        BenchOutcome::Applied => Ok(BenchLifecycleOutcome::Applied { completion }),
        BenchOutcome::Refused { reason } => Ok(BenchLifecycleOutcome::Refused { reason }),
    }
}

// ---------------------------------------------------------------------------
// Revisionless lifecycle and preference operations
// ---------------------------------------------------------------------------

/// A named direct operation either commits on the company writer or refuses a
/// current-row policy precondition.  There is deliberately no stale sequence
/// result: the writer serializes the decision and the write in one SQLite
/// transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectOutcome {
    /// The named operation committed.
    Applied,
    /// Current durable state makes the named operation invalid.
    Refused {
        /// Stable machine-readable refusal class.
        code: &'static str,
        /// Short explanation suitable for the HTTP `detail` field.
        detail: &'static str,
    },
}

fn direct_refusal(code: &'static str, detail: &'static str) -> DirectOutcome {
    DirectOutcome::Refused { code, detail }
}

/// Admit exactly one active person to run.  This is the growth counterpart to
/// the direct lifecycle verbs: one writer transaction raises only the named
/// person's durable activity demand and (for a non-CEO) their launch fence.
/// It never starts a department or authorizes a sibling.  The route wakes the
/// live reconciler after this transaction commits.
///
/// A BENCHED person is implicitly recalled here, and a DEPARTED person is
/// implicitly REHIRED here, by the same ruling and in the same transaction:
/// "If I'm bringing you up to work, that means I'm asking you to return. That
/// is always a given." (human ruling, P0). Rehire is deliberately not its own
/// verb — a CEO that has to discover a second tool to undo a firing is a CEO
/// that burns the id instead, which is the live incident this fixes.
///
/// A rehire is NOT a re-creation. The id is still permanently non-reusable
/// (`hire_person` refuses a departed id exactly as before) and
/// departed-retention is untouched: this is the SAME person coming back, with
/// the same never-deleted home and therefore the same identity key. They come
/// back as a `worker` whatever they were, because a fired head's seat has a
/// successor in it and a rehire must not contest it; leading again is a
/// separate appointment.
pub fn start_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<DirectOutcome> {
    // STOP MEANS STOP. While the operator has this company stood down, no verb
    // puts anybody into the launch fence — see `store::stand_down`. Asked
    // first, because a start that reports success and starts nobody is worse
    // than a refusal that says why.
    if stand_down::is_stood_down(tx, slug).map_err(|_| rusqlite::Error::InvalidQuery)? {
        return Ok(direct_refusal(
            "company-stood-down",
            "the operator stood this company down, so nobody is started until it is resumed; \
             run `chief resume` to lift it",
        ));
    }
    let Some((employment, department_id)) =
        organization_rows::person_placement(tx, slug, person_id)?
    else {
        return Ok(direct_refusal("unknown-person", "no such person"));
    };
    // WHO IS STARTING WHOM. Starting somebody spends a runtime seat and puts
    // a pane on their screen; this verb took an `actor` and asked nothing of
    // it. Scope over the department they live in, enforced only when the actor
    // names a person row (`actor_names_a_person`).
    if actor_out_of_scope(tx, slug, actor, &department_id)? {
        return Ok(direct_refusal(
            "actor-out-of-scope",
            "the actor does not manage the department the person they are starting lives in",
        ));
    }
    // A benched person is implicitly recalled, and a departed person is
    // implicitly rehired, by the SAME start-person transaction -- never a
    // caller-visible two-step. The recall/rehire and the start either both
    // commit or neither does: there is no durable state where a start left a
    // person recalled-but-not-started or benched-but-fenced.
    let is_rehire = employment == "departed";
    let needs_recall = employment != "active";
    // `start_person` takes no department argument, so a rehire can only put
    // the person back where they already are. A departure comes to rest at
    // `department_id` (assigned = home), and that department may since
    // have been DELETED -- which is exactly what the live incident did, taking
    // `office-of-the-ceo` down with its only member. There is then nowhere to
    // put them, and inventing a placement (silently re-homing them at the
    // root) would be a worse answer than saying so: refuse, and let the caller
    // name a department explicitly.
    if is_rehire && organization_rows::department_state(tx, slug, &department_id)?.is_none() {
        return Ok(direct_refusal(
            "home-department-gone",
            "the person departed from a department that no longer exists; \
             there is nowhere to put them, so name a department for them first",
        ));
    }
    if organization_rows::department_state(tx, slug, &department_id)?.as_deref() != Some("active")
        || department_or_ancestor_is_paused(tx, slug, &department_id)?
    {
        return Ok(direct_refusal("destination-paused", "the person's department is paused"));
    }
    let manifest = organization_rows::reconstruct(tx, slug)
        .map_err(|_| rusqlite::Error::QueryReturnedNoRows)?
        .ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;
    let is_ceo =
        manifest.chief_person_id().map_err(|_| rusqlite::Error::QueryReturnedNoRows)? == person_id;
    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::with_capacity(4);
        if needs_recall {
            touches.push(organization_rows::set_employment_state(
                tx,
                slug,
                person_id,
                EmploymentState::Active,
                at,
            )?);
            // `rehired` and `recalled` are separate durable events on purpose:
            // a recall returns a BENCHED person, a rehire returns a DEPARTED
            // one, and the person's own history has to say which happened.
            let action = if is_rehire { "rehired" } else { "recalled" };
            organization_rows::append_staffing_history(
                tx,
                slug,
                person_id,
                action,
                Some(&department_id),
                Some(&department_id),
                action,
                at,
            )?;
        }
        if is_rehire {
            // Always a worker, whatever they were fired as. A fired head's old
            // seat has a successor in it by now (the R4 composite is the only
            // sanctioned way to fire a head), and the returning person must not
            // contest it. It is also what keeps the manifest invariant at
            // `store/organization.rs` satisfied for somebody `kind = 'head'`
            // whose department was deleted underneath them.
            touches.push(organization_rows::set_person_kind(
                tx,
                slug,
                person_id,
                PersonKind::Worker,
                at,
            )?);
            // The departure left `person_activity` desired-off with its pointer
            // aimed at the offboard transition, and
            // `ensure_person_activity_desired_active` deliberately PRESERVES
            // that pointer -- correct for a mailbox wake, wrong here, because
            // the transition it names is the very departure being undone.
            // Clear it, and supersede the row itself if it is still open.
            if let Some((_cancelled_id, touch)) = activity::rows::supersede_open_transition(
                tx,
                slug,
                person_id,
                &format!("superseded-by-rehire:{person_id}"),
                at,
            )? {
                touches.push(touch);
            }
            touches.push(activity::rows::upsert_person_activity_desired(
                tx, slug, person_id, true, None, at,
            )?);
        }
        if !is_ceo {
            if let Some(touch) = launch_intent_rows::insert_person_fence(tx, slug, person_id)? {
                touches.push(touch);
            }
        }
        if let Some(touch) =
            activity::rows::ensure_person_activity_desired_active(tx, slug, person_id, at)?
        {
            touches.push(touch);
        }
        if !is_ceo {
            // Starting is a new operator decision, not a replay of the
            // person's previous quiet interval. Release only a scheduler-owned
            // idle park, then start the new lease in this SAME transaction as
            // the launch fence. Intent-bound stops remain authoritative.
            let override_fact = format!("superseded-by-start:{person_id}");
            touches.extend(activity::rows::release_idle_park(
                tx,
                slug,
                person_id,
                at,
                &override_fact,
            )?);
            if let Some(touch) =
                activity::rows::begin_explicit_start_idle_lease(tx, slug, person_id, at)?
            {
                touches.push(touch);
            }
        }
        dedupe_touches(&mut touches);
        Ok(touches)
    })?;
    Ok(DirectOutcome::Applied)
}

/// Bring one PARKED person back up — the operator pointing at somebody who is
/// asleep and asking for them.
///
/// # Why this is not [`start_person`]
///
/// `start_person` is the growth verb: it recalls the benched, rehires the
/// departed, and writes `person_activity.last_desired_active = 1` alongside the
/// fence. That last write is exactly what a WAKE must not do.
/// `project_activity_fence` suppresses `ActivityReason::Requested` for anybody
/// already desired-active — the rule stated at `fence_started_person` and
/// pinned by `create_with_hire_new_inserts_head_at_people_append_ordinal`'s
/// trailing zero — so pre-setting the flag erases the very demand that brings
/// the person up. It suppresses it a second time (#638) for anybody whose
/// routine idle park has reached a terminal status, which every settled person
/// has and keeps, because the settle path never drops the pointer.
///
/// So a wake writes the two things the fence actually reads, and nothing else:
///
/// 1. **The launch-intent grant**, through
///    [`launch_intent_rows::insert_person_fence`] — the same single writer
///    every other start decision in this file uses. There is no second notion
///    here of who may be launched.
/// 2. **The release of the lapsed routine idle park**
///    ([`activity::rows::release_idle_park`]), so the fence reads the grant as
///    the fresh demand it is instead of discarding it and withdrawing it again
///    on the same pass.
///
/// `last_desired_active` is deliberately left alone: the next converge pass
/// computes it from the `Requested` reason this transaction just made
/// reachable, which is the only ordering in which a wake actually converges.
///
/// # Refusals
///
/// * `unknown-person` — no such person.
/// * `actor-out-of-scope` — the SUBTREE rule, the same question
///   [`start_person`] asks and with no role gate anywhere near it.
/// * `person-not-staffed` — benched or departed. That person is not asleep,
///   they are off the roster, and returning them is [`start_person`]'s job;
///   quietly rehiring somebody because a rail row was clicked would be a
///   staffing decision nobody made.
/// * `destination-paused` — their department, or one above it, is paused.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn wake_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<DirectOutcome> {
    // STOP MEANS STOP. While the operator has this company stood down, no verb
    // puts anybody into the launch fence — see `store::stand_down`. Asked
    // first, because a start that reports success and starts nobody is worse
    // than a refusal that says why.
    if stand_down::is_stood_down(tx, slug).map_err(|_| rusqlite::Error::InvalidQuery)? {
        return Ok(direct_refusal(
            "company-stood-down",
            "the operator stood this company down, so nobody is started until it is resumed; \
             run `chief resume` to lift it",
        ));
    }
    let Some((employment, department_id)) =
        organization_rows::person_placement(tx, slug, person_id)?
    else {
        return Ok(direct_refusal("unknown-person", "no such person"));
    };
    if actor_out_of_scope(tx, slug, actor, &department_id)? {
        return Ok(direct_refusal(
            "actor-out-of-scope",
            "the actor does not manage the department the person they are waking lives in",
        ));
    }
    // A WAKE RECALLS A BENCHED PERSON. IT DOES NOT REFUSE THEM.
    //
    // Operator ruling, 2026-08-14: "there's no such thing as bench. When I
    // click, it's always wake. There is no stopping decision. Wake him right
    // away."
    //
    // This used to answer `person-not-staffed` — "starting them is a staffing
    // decision and takes the start verb" — which is a true statement about the
    // old model and a useless one to the person holding the mouse. They clicked
    // a row the rail drew as `sleeping`, got a notice that flashed past, and
    // read it as the product being stuck. Measured on their own company:
    // `org.person.wake.refused person=dev reason=person-not-staffed`, and the
    // operator's next words were that Dev was stuck.
    //
    // The bench CONCEPT is not deleted here — it is ~850 references across the
    // durable schema, the wire types, the HTTP routes and the Pi tool surface,
    // and tearing it out is a different piece of work. What is deleted is the
    // bench standing between a CLICK and a person running: a wake now raises a
    // benched person to active and carries on, which is exactly what
    // `recall_person` does and is the same durable event.
    //
    // DEPARTED IS STILL REFUSED, and that is not the same thing. Firing is a
    // decision somebody made about a person, the rail never draws a departed
    // person at all ("we never see fired employees"), and a click cannot reach
    // one — so a wake that silently un-fired somebody would be answering a
    // question nobody asked.
    let benched = employment == "benched";
    if employment == "departed" {
        return Ok(direct_refusal(
            "person-departed",
            "that person was fired; bringing them back is a rehire and takes the start verb",
        ));
    }
    if organization_rows::department_state(tx, slug, &department_id)?.as_deref() != Some("active")
        || department_or_ancestor_is_paused(tx, slug, &department_id)?
    {
        return Ok(direct_refusal("destination-paused", "the person's department is paused"));
    }
    let manifest = organization_rows::reconstruct(tx, slug)
        .map_err(|_| rusqlite::Error::QueryReturnedNoRows)?
        .ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;
    let is_ceo =
        manifest.chief_person_id().map_err(|_| rusqlite::Error::QueryReturnedNoRows)? == person_id;
    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::with_capacity(4);
        // THE RECALL, IN THE SAME TRANSACTION AS THE WAKE. Two commits would
        // leave a window in which the person is active but unfenced, which the
        // very next reconcile would read as somebody who should be up and is
        // not — and act on.
        //
        // `recalled` is the same durable staffing event `recall_person` writes,
        // so a person's own history says what happened to them in the words it
        // already uses. A wake is not a new kind of staffing change; it is the
        // existing one, reached by clicking instead of by typing.
        if benched {
            touches.push(organization_rows::set_employment_state(
                tx,
                slug,
                person_id,
                EmploymentState::Active,
                at,
            )?);
            organization_rows::append_staffing_history(
                tx,
                slug,
                person_id,
                "recalled",
                Some(&department_id),
                Some(&department_id),
                "recalled",
                at,
            )?;
        }
        // The CEO is never fenced — `launch_intent::person_can_run` admits the
        // root unconditionally — so a grant for them would be a row that means
        // nothing. Their park still releases below: the CEO can settle.
        if !is_ceo {
            if let Some(touch) = launch_intent_rows::insert_person_fence(tx, slug, person_id)? {
                touches.push(touch);
            }
        }
        let override_fact = format!("superseded-by-wake:{person_id}");
        touches.extend(activity::rows::release_idle_park(tx, slug, person_id, at, &override_fact)?);
        dedupe_touches(&mut touches);
        Ok(touches)
    })?;
    if benched {
        tracing::info!(
            event = "org.person.wake.recalled",
            person = person_id,
            department = %department_id,
            "the operator clicked somebody who was benched; the wake recalled them rather \
             than refusing, because a click is always a wake"
        );
    }
    Ok(DirectOutcome::Applied)
}

/// End the headship `person_id` holds, inside the caller's transaction.
///
/// THE ONE STATEMENT OF THE RULE, shared by department create and person
/// transfer. Both verbs move a head out of the department they lead and both
/// leave it without one; writing the answer at each call site is how two
/// statements of a single rule drift apart, which is the defect this whole
/// packet exists to close.
///
/// # Order
///
/// Every caller must run this BEFORE writing the person's new headship or
/// placement. `departments_one_head` is a UNIQUE INDEX on
/// `(slug, head_person_id)`, so a create that inserts its department first —
/// or a transfer that re-homes first — collides with the row this function is
/// about to change or delete. The eligibility half already ran in
/// `org_projection::check_head_vacancy`, so by here the decision is known to
/// fit: a hand-over names a real member, and a dissolve names a department
/// holding nobody else and nothing beneath it.
///
/// # Errors
/// Propagates any `rusqlite` failure.
fn vacate_headship(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    decision: &HeadVacancy,
    at: &str,
    audit_reason: &str,
    touches: &mut Vec<EventTouch>,
) -> rusqlite::Result<()> {
    let Some(vacated) = organization_rows::department_headed_by_person(tx, slug, person_id)? else {
        // The projection refuses a decision for somebody who heads nothing
        // (`VacancyRefusal::HeadsNothing`), so this is unreachable through any
        // caller. Returning rather than panicking keeps a damaged row set from
        // taking the process down.
        return Ok(());
    };
    match decision {
        HeadVacancy::HandOver { successor_person_id } => {
            touches.push(organization_rows::set_department_head(
                tx,
                slug,
                &vacated,
                successor_person_id,
                at,
            )?);
            touches.push(organization_rows::set_person_kind(
                tx,
                slug,
                successor_person_id,
                PersonKind::Head,
                at,
            )?);
            organization_rows::append_staffing_history(
                tx,
                slug,
                successor_person_id,
                "appointed-head",
                Some(&vacated),
                Some(&vacated),
                audit_reason,
                at,
            )?;
        }
        HeadVacancy::Dissolve => {
            // **DEPARTED RESIDENTS ARE INVISIBLE TO VALIDATION AND VISIBLE TO
            // THE FOREIGN KEY, and that disagreement was a live 500.**
            //
            // This arm said "the department is empty of everyone but the person
            // leaving it, so there is nobody to re-home". That is true of
            // ACTIVE and BENCHED people and false of DEPARTED ones.
            // `org_projection::unit_members_other_than` excludes `Departed`
            // deliberately — a dissolve must not be blocked by alumni nobody
            // can act on — so `check_head_vacancy` passes. But departed rows
            // are RETAINED by design for the rehire rule, they keep
            // `people.department_id` pointing here, and that column is NOT NULL
            // with a foreign key onto `departments`. So `delete_department`
            // died on `FOREIGN KEY constraint failed`.
            //
            // Measured on a live box 2026-08-24: a CEO offboarded two
            // engineers and then dissolved their department, and every attempt
            // 500'd. The model saw "chiefd unavailable" — a validated write
            // hitting a constraint wears an infrastructure costume — and
            // retried four times before giving up with the department still
            // standing.
            //
            // THE FIX IS THE ONE THE SIBLING VERB ALREADY SHIPS.
            // `remove_department_tree` re-homes its departed rows to the
            // removed unit's PARENT, for exactly this reason, and pins it. This
            // is the same rule at a smaller scale, so it is written the same
            // way rather than invented again.
            //
            // The LEAVING head is skipped: the caller re-homes them immediately
            // after this function returns, and the doc above requires that
            // order because `departments_one_head` is unique on the head
            // column.
            //
            // A `None` parent is the company root, which cannot be dissolved —
            // the CEO heads it and is immovable — so there is no case where a
            // departed row has nowhere to point.
            if let Some(parent_department_id) =
                organization_rows::department_parent_state(tx, slug, &vacated)?
                    .and_then(|(parent_id, _state)| parent_id)
            {
                for (resident_id, _home) in organization_rows::people_in_departments(
                    tx,
                    slug,
                    std::slice::from_ref(&vacated),
                )? {
                    if resident_id == person_id {
                        continue;
                    }
                    touches.push(organization_rows::move_person(
                        tx,
                        slug,
                        &resident_id,
                        &parent_department_id,
                        at,
                    )?);
                    // `transferred`, NOT a new verb. `staffing_history.action`
                    // carries a CHECK constraint, and the schema records that
                    // widening it is the one change `CREATE TABLE IF NOT
                    // EXISTS` cannot deliver on a historical database — so
                    // inventing `re-homed` here would have replaced one
                    // constraint-violation 500 with another, in the same
                    // transaction, for the same reason. `transferred` is in the
                    // allowed set and is what actually happened to this row:
                    // its home moved.
                    //
                    // THE STANDING QUESTION THIS COST US TWICE IN ONE EVENING:
                    // before writing any INSERT or UPDATE, ask whether the
                    // column carries a CHECK or a FOREIGN KEY. Both are
                    // invisible in the calling code and both fail at runtime as
                    // a 500 that reads like infrastructure. It is the
                    // schema-layer sibling of asking, before deleting anything,
                    // what is COMPUTED from it rather than only what calls it.
                    organization_rows::append_staffing_history(
                        tx,
                        slug,
                        &resident_id,
                        "transferred",
                        Some(&vacated),
                        Some(&parent_department_id),
                        audit_reason,
                        at,
                    )?;
                }
            }
            touches.push(organization_rows::delete_department(tx, slug, &vacated)?);
            touches.extend(organization_rows::refresh_department_order(tx, slug, at)?);
        }
    }
    // The outgoing head stepped down from a department they no longer lead.
    // Recorded for BOTH answers: the roster audit reads this vocabulary, and a
    // dissolve that recorded nothing would leave a person who led a department
    // yesterday with no trace of having stopped.
    organization_rows::append_staffing_history(
        tx,
        slug,
        person_id,
        "stepped-down",
        Some(&vacated),
        None,
        audit_reason,
        at,
    )?;
    Ok(())
}

/// Whether `actor` identifies a real person in this company.
///
/// THE ONE STATEMENT OF THE ACTOR RULE. `actor` is free-form audit prose, not a
/// principal: this corpus writes `operator`, `op` and the empty string, and a
/// guard that exempted placeholder spellings by name would need a list that
/// rots the first time somebody writes `sys`. So authorization is enforced only
/// when the actor NAMES A PERSON ROW, and every other value passes through to
/// the ledger unjudged.
///
/// Sound in both worlds, which is what lets track B1 land ahead of credentials:
/// while no daemon sets them nothing authenticates anyway, and once it does the
/// route replaces the actor with the authenticated caller's principal, so what
/// reaches these guards is always a real person.
///
/// `offboard_person` holds the same predicate inline (it landed first); collapse
/// the two when next in that function.
///
/// `pub(crate)` because the rule is not this module's: `store::mailbox_rows`
/// asks the identical question of the identical corpus, and a second copy there
/// would be the second statement this doc-comment exists to prevent.
///
/// # Relative of, and deliberately not, `control_authority::department_is_in_scope`
///
/// The two are relatives asking one question of two different corpora, with
/// OPPOSITE defaults, and the difference is intended rather than drift.
///
/// `department_is_in_scope` takes a [`ControlActor`](crate::store::control_authority::ControlActor):
/// an authenticated principal, already resolved to "a person" or "the
/// operator", and it answers `false` for a person the manifest does not have.
/// This function takes a `&str` of free-form audit prose written inside a SQL
/// transaction, where `operator`, `op` and `""` are ordinary values, and
/// [`actor_out_of_scope`] composes it so that an actor naming NOBODY is never
/// out of scope.
///
/// Folding the two together would mean picking ONE of those defaults for both
/// corpora. Refusing an unrecognised actor string would refuse every
/// placeholder spelling this corpus already contains; passing an unrecognised
/// principal would hand a credential naming nobody the scope the manifest side
/// exists to withhold. That is a product decision with real consequences, not a
/// parameter change, so the two stay separate and this paragraph — plus its
/// twin on `department_is_in_scope` — is what stops a later reader tidying an
/// inconsistency that is not one.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub(crate) fn actor_names_a_person(
    tx: &Transaction<'_>,
    slug: &str,
    actor: &str,
) -> rusqlite::Result<bool> {
    if actor.is_empty() {
        return Ok(false);
    }
    Ok(organization_rows::person_placement(tx, slug, actor)?.is_some())
}

/// Whether `actor` is a real person who does NOT manage `department_id`.
///
/// THE ONE PREDICATE EVERY B1 GUARD ASKS. Authority over structure is the
/// SUBTREE you head and never the job title: a head reaches its own unit and
/// everything beneath it, and the CEO heads the company root and therefore
/// reaches everybody. `person_manages_department` is the rows-side twin of
/// `control_authority::department_is_in_scope`, which the same question is
/// already answered with at `/v1/org/control-authority/department-in-scope`.
///
/// It composes [`actor_names_a_person`], so an actor that names nobody is
/// never out of scope — see that function for why the corpus makes that the
/// only sound rule.
///
/// # Errors
/// Propagates any `rusqlite` failure.
fn actor_out_of_scope(
    tx: &Transaction<'_>,
    slug: &str,
    actor: &str,
    department_id: &str,
) -> rusqlite::Result<bool> {
    if !actor_names_a_person(tx, slug, actor)? {
        return Ok(false);
    }
    Ok(!organization_rows::person_manages_department(tx, slug, actor, department_id)?)
}

/// Whether `actor` is a real person who does not manage the department
/// `person_id` is homed in.
///
/// A person target reduces to a department question: you may act on somebody
/// when you manage where they live. An unknown person yields `false` — the
/// verbs that care already have their own `UnknownPerson` refusal, and the
/// ones that do not have never refused an unknown target, so this must not
/// invent a new refusal for them.
///
/// # Errors
/// Propagates any `rusqlite` failure.
fn actor_out_of_scope_for_person(
    tx: &Transaction<'_>,
    slug: &str,
    actor: &str,
    person_id: &str,
) -> rusqlite::Result<bool> {
    let Some((_employment, department_id)) =
        organization_rows::person_placement(tx, slug, person_id)?
    else {
        return Ok(false);
    };
    actor_out_of_scope(tx, slug, actor, &department_id)
}

fn dedupe_touches(touches: &mut Vec<EventTouch>) {
    let mut seen = std::collections::HashSet::new();
    touches.retain(|touch| seen.insert((touch.entity.clone(), touch.entity_id.clone())));
}

/// Recall a benched person without starting a pane.  This is the durable roster
/// half only; a manager still makes a separate, explicit start decision.
pub fn recall_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<DirectOutcome> {
    let Some((employment, department_id)) =
        organization_rows::person_placement(tx, slug, person_id)?
    else {
        return Ok(direct_refusal("unknown-person", "no such person"));
    };
    if employment == "departed" {
        return Ok(direct_refusal(
            "already-departed",
            "the person has departed and cannot be recalled",
        ));
    }
    if employment == "active" {
        return Ok(direct_refusal("already-active", "the person is already active"));
    }
    // WHO IS RECALLING WHOM. Scope over the department they live in, enforced
    // only when the actor names a person row.
    if actor_out_of_scope(tx, slug, actor, &department_id)? {
        return Ok(direct_refusal(
            "actor-out-of-scope",
            "the actor does not manage the department the person they are recalling lives in",
        ));
    }
    if organization_rows::department_state(tx, slug, &department_id)?.as_deref() != Some("active")
        || department_or_ancestor_is_paused(tx, slug, &department_id)?
    {
        return Ok(direct_refusal("destination-paused", "the person's department is paused"));
    }
    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = vec![organization_rows::set_employment_state(
            tx,
            slug,
            person_id,
            EmploymentState::Active,
            at,
        )?];
        organization_rows::append_staffing_history(
            tx,
            slug,
            person_id,
            "recalled",
            Some(&department_id),
            Some(&department_id),
            "recalled",
            at,
        )?;
        dedupe_touches(&mut touches);
        Ok(touches)
    })?;
    Ok(DirectOutcome::Applied)
}

/// so it is the only unit this repairs.
pub fn reactivate_executive_root(
    tx: &Transaction<'_>,
    slug: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<DirectOutcome> {
    let Some((root_id, _)) = organization_rows::company_root(tx, slug)? else {
        return Ok(direct_refusal("unknown-company", "the company has no executive root"));
    };
    // WHO IS RESTARTING THE WHOLE COMPANY. The target is the company ROOT, and
    // the only person who manages the root is the one who heads it — the CEO.
    // That falls out of the same subtree predicate every other verb asks; it
    // is not a role gate and no title is named. Enforced only when the actor
    // names a person row.
    if actor_out_of_scope(tx, slug, actor, &root_id)? {
        return Ok(direct_refusal(
            "actor-out-of-scope",
            "the actor does not manage the company root they are reactivating",
        ));
    }
    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::new();
        if organization_rows::department_state(tx, slug, &root_id)?.as_deref() == Some("paused") {
            touches.push(organization_rows::set_department_paused(tx, slug, &root_id, false, at)?);
        }
        dedupe_touches(&mut touches);
        Ok(touches)
    })?;
    Ok(DirectOutcome::Applied)
}

/// The committed identities from a named recursive department removal. The
/// values are immutable audit/result facts, not an aggregate version token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveDepartmentOutcome {
    /// The department subtree was deleted and everyone under it departed.
    Applied {
        /// Departments deleted deepest-child first inside the transaction.
        removed_department_ids: Vec<String>,
        /// People OFFBOARDED by the removal, in roster order. Their rows are
        /// retained (`employment_state = 'departed'`) — the removal deletes
        /// units, never people.
        departed_person_ids: Vec<String>,
    },
    /// Current durable placement makes removal unsafe; no row was touched.
    Refused {
        /// Stable machine-readable refusal class.
        code: &'static str,
        /// Short explanation suitable for the HTTP `detail` field.
        detail: &'static str,
    },
}

/// Remove a non-root department subtree in one named SQLite transaction, and
/// OFFBOARD every person placed beneath it. There is no placement refusal left
/// to raise: a person sits in exactly one department, so "placed beneath this
/// subtree" is a single unambiguous answer and every such person departs — see
/// the tombstone at the former refusal site below.
///
/// # A person is never deleted, because a company keeps its history
///
/// This op used to `DELETE FROM people` for everyone in the removed subtree
/// while [`offboard_person`] — the other verb the product calls "firing" —
/// retained the row. Two answers to one act, and the tool copy said "fires" for
/// both, so the difference was invisible at the call site. The deletion was the
/// wrong half:
///
/// * `staffing_history` deliberately carries no people FK precisely so a
///   person's ledger outlives them. Deleting the row therefore did not clear
///   the history, it made it WRONG — an orphaned `hired` entry with no
///   `offboarded` entry and nobody it belongs to. That is worse than either
///   consistent answer.
/// * A person who was hired and later left is a durable fact about the
///   company. `employment_state = 'departed'` is a first-class, queryable
///   state; a deleted row is indistinguishable from someone who never existed.
/// * `org_offboard`'s own description sends the caller here when the head is
///   its department's only member. The product already substitutes these two
///   paths for each other, so they must mean the same thing.
///
/// So the commit does exactly what an unattended offboard does, for each
/// person, in the same transaction: supersede any open transition, desired-off,
/// demote a head of a now-deleted department to `worker`, flip
/// `employment_state → departed`, re-home them to the removed subtree's PARENT
/// (`root-department-protected` above guarantees it exists, and
/// `people.department_id` references a live department), append
/// `staffing_history 'offboarded'`, release their open assignments, and
/// withdraw the launch-intent fence.
///
/// The fence goes NOW rather than after a handoff grace window, unlike the
/// attended [`offboard_person`]: the department they would hand off to has been
/// deleted in this very transaction, so nobody can ever release the
/// transition — the pane would stay open forever.
pub fn remove_department_tree(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<RemoveDepartmentOutcome> {
    let Some((parent_id, _state)) =
        organization_rows::department_parent_state(tx, slug, department_id)?
    else {
        return Ok(RemoveDepartmentOutcome::Refused {
            code: "unknown-department",
            detail: "no such department",
        });
    };
    // WHO DELETED A WHOLE SUBTREE. `remove_department_tree` took an actor and
    // asked nothing of it, and the route handed it `String::new()` — so the
    // most destructive verb in the crate recorded its author as the empty
    // string, and any caller reaching the route could remove any department in
    // any company.
    //
    // The check is SCOPE over the department being removed, not a role: a head
    // reaches its own unit and everything beneath it, and the CEO reaches the
    // whole company (`person_manages_department`). It is the rows-side twin of
    // `control_authority::department_is_in_scope`, which the same question is
    // already answered with at `/v1/org/control-authority/department-in-scope`.
    // The DEPARTMENT predicate is the right one because the target is a
    // department; the person predicate would ask about the wrong subject.
    //
    // Enforced only when the actor NAMES A PERSON ROW. The actor is free-form
    // audit prose in this corpus and fires as `operator`, as `op` and as the
    // empty string, so gating on the string's CONTENT would need a list of
    // placeholder spellings that rots on the first new one. This rule is sound
    // in both worlds: while no daemon sets credentials nothing authenticates
    // anyway, and once they exist the route overwrites the actor with the
    // caller's principal, so what arrives here is always a real person.
    if actor_names_a_person(tx, slug, actor)?
        && !organization_rows::person_manages_department(tx, slug, actor, department_id)?
    {
        return Ok(RemoveDepartmentOutcome::Refused {
            code: "actor-out-of-scope",
            detail: "the actor does not manage the department they are removing",
        });
    }
    // Where the departed rows come to rest. A parent is never inside its own
    // child's subtree — the recursive walk below descends from `department_id` —
    // so the destination always survives the same transaction that deletes the
    // subtree. When the removed unit hangs directly off the company root, the
    // root IS the parent and the departed people rest there; the root itself is
    // never removable, so the recursion has a floor and there is no case where a
    // departed row has nowhere to point.
    let Some(parent_department_id) = parent_id else {
        return Ok(RemoveDepartmentOutcome::Refused {
            code: "root-department-protected",
            detail: "remove the company instead of its executive root",
        });
    };
    let department_ids =
        organization_rows::department_subtree_ids_descending(tx, slug, department_id)?;
    // TOMBSTONE (#1081): a `borrowed-person-present` refusal stood here, and
    // before it a `person-on-loan` twin. Both asked whether somebody's two
    // placement columns straddled the subtree boundary — one inside, one
    // outside — which only a loan could produce. The loan verbs went first
    // (2026-08-13) and the second column with them, so the question no longer
    // has two answers to compare: "everybody in this subtree" and "everybody
    // placed in these departments" are now the SAME query over the SAME column,
    // and the refusal could not fire for any row.
    //
    // The read that fed it is not merely kept, it is now the ONLY read needed.
    // This used to fetch each leaver's unit a second time through
    // `person_placement`, because the first read returned the ASSIGNED unit and
    // the staffing ledger needs the unit they LEFT. One column, one read: a
    // leaver placed in a descendant of `department_id` leaves that descendant,
    // never the parent they are re-homed to.
    let departing = organization_rows::people_in_departments(tx, slug, &department_ids)?;
    let audit_reason = format!("department {department_id} removed");
    let departed_person_ids =
        departing.iter().map(|(person_id, _)| person_id.clone()).collect::<Vec<_>>();
    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        let mut touches = Vec::with_capacity(departing.len() + department_ids.len());
        for (person_id, department_id) in &departing {
            // Unattended: the fence goes NOW. A person whose department is
            // deleted in this transaction can never complete the handoff that
            // would clear it, so a grace window would hold their pane open
            // forever.
            if let Some(touch) =
                launch_intent_rows::delete_person_fence(tx, slug, person_id, "department-removed")?
            {
                touches.push(touch);
            }
            // Nobody can hand off to a deleted department, so any open
            // transition is cancelled rather than adopted or replaced.
            if let Some((_cancelled_id, touch)) = activity::rows::supersede_open_transition(
                tx,
                slug,
                person_id,
                &format!("superseded-by-department-removal:{department_id}"),
                at,
            )? {
                touches.push(touch);
            }
            // A head of a department that no longer exists is not a head. The
            // precedent is `replace_head_and_offboard`, which demotes the
            // outgoing head in the same commit that appoints their successor.
            if organization_rows::person_kind(tx, slug, person_id)?.as_deref() == Some("head") {
                touches.push(organization_rows::set_person_kind(
                    tx,
                    slug,
                    person_id,
                    PersonKind::Worker,
                    at,
                )?);
            }
            touches.extend(depart_person_rows(
                tx,
                slug,
                &Departure {
                    person_id,
                    department_id: &parent_department_id,
                    from_department_id: department_id,
                    active_transition_id: None,
                    reason: &audit_reason,
                },
                at,
            )?);
        }
        for subtree_department_id in &department_ids {
            touches.push(organization_rows::delete_department(tx, slug, subtree_department_id)?);
        }
        touches.extend(organization_rows::refresh_department_order(tx, slug, at)?);
        touches.extend(organization_rows::refresh_people_order(tx, slug, at)?);
        dedupe_touches(&mut touches);
        Ok(touches)
    })?;
    Ok(RemoveDepartmentOutcome::Applied {
        removed_department_ids: department_ids,
        departed_person_ids,
    })
}

/// Replace a non-root department's head and offboard the former head in one
/// transaction, so no observer can ever see a headless department.
pub fn replace_head_and_offboard(
    tx: &Transaction<'_>,
    slug: &str,
    head_person_id: &str,
    successor_person_id: &str,
    at: &str,
    actor: &str,
) -> rusqlite::Result<DirectOutcome> {
    let reason = authored_ledger_line("head replaced", actor);
    let reason = reason.as_str();
    if head_person_id == successor_person_id {
        return Ok(direct_refusal("same-successor", "name a different successor"));
    }
    // THE CEO ALONE. This guard was examined earlier in this packet and kept
    // WIDE, on the argument that the verb offboards rather than hands over, so
    // narrowing it would route around `offboard_person`'s refusal of every
    // executive-root person. `offboard_person` no longer makes that refusal,
    // so the argument is gone and the exception is retired with it rather than
    // left standing on a premise that no longer holds.
    if is_ceo(tx, slug, head_person_id)? {
        return Ok(direct_refusal(
            "exec-root-protected",
            "the CEO always heads the company root and is never replaced here",
        ));
    }
    let Some(department_id) =
        organization_rows::department_headed_by_person(tx, slug, head_person_id)?
    else {
        return Ok(direct_refusal(
            "not-a-department-head",
            "the person does not head a department",
        ));
    };
    // WHO IS REPLACING A HEAD. This verb fires the sitting head and installs a
    // successor in one transaction, and it asked nothing of its `actor`. The
    // check is scope over the department whose head is being replaced — the
    // successor is required below to be a member of that same department, so
    // one predicate covers both people this verb moves.
    if actor_out_of_scope(tx, slug, actor, &department_id)? {
        return Ok(direct_refusal(
            "actor-out-of-scope",
            "the actor does not manage the department whose head they are replacing",
        ));
    }
    let Some((successor_employment, successor_department_id)) =
        organization_rows::person_placement(tx, slug, successor_person_id)?
    else {
        return Ok(direct_refusal("unknown-successor", "no such successor"));
    };
    if successor_employment == "departed" {
        return Ok(direct_refusal("departed-successor", "the successor has departed"));
    }
    if successor_department_id != department_id {
        return Ok(direct_refusal(
            "not-a-member",
            "the successor must be an active member of that department",
        ));
    }
    if organization_rows::person_heads_department_other_than(
        tx,
        slug,
        successor_person_id,
        &department_id,
    )?
    .is_some()
    {
        return Ok(direct_refusal(
            "already-heads-elsewhere",
            "the successor already heads another department",
        ));
    }
    let Some((_employment, head_department_id)) =
        organization_rows::person_placement(tx, slug, head_person_id)?
    else {
        return Ok(direct_refusal("unknown-person", "no such head"));
    };
    apply_and_emit::<rusqlite::Error, _>(tx, slug, at, actor, |tx| {
        // The schema permits one open transition per person. Clear a stranded
        // outgoing-head transition before minting the replacement handoff: if
        // this ran after the insert, an existing row would reject the insert,
        // while no existing row would let this supersede the row just minted.
        let superseded_transition = activity::rows::supersede_open_transition(
            tx,
            slug,
            head_person_id,
            &format!("superseded-by-offboard:{head_person_id}"),
            at,
        )?;
        // Same embedded-sequence id grammar as every other terminal row
        // (`transition:<seq>:<person>:offboard`) — the `offboard:<person>:<at>`
        // form failed `validate` and wedged every later whole-ledger ingest.
        let transition_id = format!(
            "transition:{}:{head_person_id}:offboard",
            crate::store::rows_txn::allocate_seq(
                tx,
                &activity::rows::transitions_counter_key(slug)
            )?
        );
        let placement = terminal_transition_context(tx, slug, head_person_id)?;
        let handoff_deadline_at = parse_iso_millis(at)
            .map(|requested| iso_millis(requested + activity::HANDOFF_GRACE_MS))
            .unwrap_or_else(|| at.to_string());
        let mut touches = vec![
            organization_rows::set_department_head(
                tx,
                slug,
                &department_id,
                successor_person_id,
                at,
            )?,
            organization_rows::set_person_kind(
                tx,
                slug,
                successor_person_id,
                PersonKind::Head,
                at,
            )?,
            organization_rows::set_person_kind(tx, slug, head_person_id, PersonKind::Worker, at)?,
            organization_rows::set_employment_state(
                tx,
                slug,
                head_person_id,
                EmploymentState::Departed,
                at,
            )?,
            organization_rows::move_person(tx, slug, head_person_id, &head_department_id, at)?,
            // The GRACEFUL offboard handoff (`awaiting_handoff`): the departure
            // does not apply until the outgoing head releases the transition —
            // the same shape the offboard e2e drives end to end. The fence
            // stays for exactly the handoff window (a departed person is inert
            // under `operationalPerson` regardless).
            activity::rows::insert_awaiting_handoff_transition(
                tx,
                slug,
                &transition_id,
                head_person_id,
                activity::TransitionAction::Offboard,
                &placement,
                None,
                &format!("offboarded {head_person_id}"),
                at,
                &handoff_deadline_at,
            )?,
            // Desired-off, pointing AT the live transition: the reconcile
            // retains the pane for the bounded grace window, then applies.
            activity::rows::upsert_person_activity_desired(
                tx,
                slug,
                head_person_id,
                false,
                Some(&transition_id),
                at,
            )?,
        ];
        if let Some((_cancelled, touch)) = superseded_transition {
            touches.push(touch);
        }
        // The outgoing head's launch-intent fence stays for exactly the
        // handoff window — the reconcile's graceful machinery can only
        // complete a handoff for a person who can run; the offboard lifecycle
        // owns its withdrawal when the handoff applies.
        organization_rows::append_staffing_history(
            tx,
            slug,
            successor_person_id,
            "appointed-head",
            Some(&department_id),
            Some(&department_id),
            reason,
            at,
        )?;
        organization_rows::append_staffing_history(
            tx,
            slug,
            head_person_id,
            "stepped-down",
            Some(&department_id),
            Some(&head_department_id),
            reason,
            at,
        )?;
        organization_rows::append_staffing_history(
            tx,
            slug,
            head_person_id,
            "offboarded",
            Some(&head_department_id),
            None,
            reason,
            at,
        )?;
        dedupe_touches(&mut touches);
        Ok(touches)
    })?;
    Ok(DirectOutcome::Applied)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;
    use rusqlite::Connection;

    /// Open a FULL-schema in-memory company (so the real typed accessors write
    /// every column) and seed a minimal exec-root org:
    ///   executive (root, head ada=CEO)
    ///     ├─ office-of-the-ceo (head cos = chief-of-staff)
    ///     └─ eng (head bo = a normal report)
    /// ada & cos are executive-root protected; bo is the normal shutdown target.
    /// FKs OFF: this unit exercises org_ops' composition, not the manifest FKs.
    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO org_settings(slug, display_slug, supervision_interval_ms, acknowledgement_timeout_ms, acknowledgement_retry_limit, replacement_limit) VALUES('acme', 'acme',60000,30000,3,3); INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('acme','executive',NULL,'Executive','company','active','ada',0,'t','t'), ('acme','office-of-the-ceo','executive','Office of the CEO','department','active','cos',1,'t','t'), ('acme','eng','executive','Engineering','department','active','bo',2,'t','t');
             INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','ada','Ada','CEO','lead','executive','active','executive',0,'t','t'), ('acme','cos','Cos','Chief of Staff','support','head','active','office-of-the-ceo',1,'t','t'), ('acme','bo','Bo','Engineer','build','worker','active','eng',2,'t','t');",
        )
        .expect("seed");
        conn
    }

    fn desired(conn: &Connection, person: &str) -> Option<i64> {
        conn.query_row(
            "SELECT last_desired_active FROM person_activity WHERE slug='acme' AND person_id=?1",
            rusqlite::params![person],
            |r| r.get(0),
        )
        .ok()
    }

    #[test]
    fn start_person_is_exactly_one_person_and_each_success_refreshes_its_idle_lease() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            start_person(&tx, "acme", "bo", "2026-08-02T13:10:44.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
        );
        assert_eq!(desired(&tx, "bo"), Some(1));
        assert_eq!(desired(&tx, "ada"), None, "the CEO is not re-authorized as a side effect");
        assert_eq!(desired(&tx, "cos"), None, "a sibling is never started");
        let first_lease: (Option<String>, Option<String>, Option<String>) = tx
            .query_row(
                "SELECT agent_quiet_at, idle_since, agent_active_at FROM person_activity \
                 WHERE slug='acme' AND person_id='bo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            first_lease,
            (
                Some("2026-08-02T13:10:44.000Z".to_string()),
                Some("2026-08-02T13:10:44.000Z".to_string()),
                None,
            ),
            "the successful start begins this person's fresh idle lease"
        );
        let fences: Vec<String> = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(fences, vec!["bo"]);
        let first_seq: i64 = tx
            .query_row("SELECT MAX(seq) FROM org_events WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            start_person(&tx, "acme", "bo", "2026-08-02T13:10:45.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
            "a repeated explicit start remains valid",
        );
        let second_seq: i64 = tx
            .query_row("SELECT MAX(seq) FROM org_events WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            second_seq,
            first_seq + 1,
            "the new start decision emits exactly its refreshed person-activity fact"
        );
        let refreshed: (Option<String>, Option<String>, Option<String>) = tx
            .query_row(
                "SELECT agent_quiet_at, idle_since, agent_active_at FROM person_activity \
                 WHERE slug='acme' AND person_id='bo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            refreshed,
            (
                Some("2026-08-02T13:10:45.000Z".to_string()),
                Some("2026-08-02T13:10:45.000Z".to_string()),
                None,
            ),
            "each successful explicit start replaces the prior lease instead of inheriting it"
        );
    }

    // --- the wake -----------------------------------------------------------
    //
    // EVERY TEST BELOW RUNS AGAINST A GENUINELY PARKED PERSON, and that is not
    // a stylistic choice. A wake tested against somebody who is merely idle
    // cannot tell a working wake from a broken one — both pass, because an
    // already-desired-active person needs nothing done to them. The whole
    // defect lives in the state `park` builds.

    /// Put `person` in the state the two-minute settle actually leaves behind:
    ///
    /// * an `applied` ROUTINE idle park (`intent_id IS NULL`, the reason
    ///   `activity::IDLE_AUTO_PARK_REASON`) that is still their
    ///   `active_transition_id` — the settle path applies the transition and
    ///   never drops the pointer;
    /// * `last_desired_active = 0`;
    /// * and NO `launch_intent` row, because the same pass that terminated the
    ///   park withdrew their intent (the F8 shrink half).
    ///
    /// This is the person the operator clicks. Nothing in it is invented: it is
    /// the row-level shadow of `converge_apply::cycle`'s settle.
    fn park(conn: &Connection, person: &str, department: &str) {
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, intent_id, reason, \
             placement_department_id, requested_at, applied_at) \
             VALUES('acme', ?1, ?2, 'park', 'applied', NULL, ?3, ?4, 't', 't')",
            rusqlite::params![
                format!("park-{person}"),
                person,
                crate::store::activity::IDLE_AUTO_PARK_REASON,
                department
            ],
        )
        .expect("park row");
        conn.execute(
            "INSERT INTO person_activity(slug, person_id, last_desired_active, \
             active_transition_id, updated_at) VALUES('acme', ?1, 0, ?2, 't')",
            rusqlite::params![person, format!("park-{person}")],
        )
        .expect("parked activity row");
    }

    fn fenced(conn: &Connection) -> Vec<String> {
        conn.prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn pointer(conn: &Connection, person: &str) -> Option<String> {
        conn.query_row(
            "SELECT active_transition_id FROM person_activity \
             WHERE slug='acme' AND person_id=?1",
            rusqlite::params![person],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// THE WHOLE PACKET, in one test. A parked person is granted launch intent
    /// AND has their lapsed park released, and their `last_desired_active` is
    /// left alone — because `project_activity_fence` suppresses the `Requested`
    /// reason for anybody already desired-active, so setting it here would
    /// erase the demand the grant exists to raise.
    #[test]
    fn waking_a_parked_person_grants_intent_and_releases_the_lapsed_park() {
        let mut conn = open();
        park(&conn, "bo", "eng");
        assert_eq!(fenced(&conn), Vec::<String>::new(), "a settled person carries no intent");

        let tx = conn.transaction().unwrap();
        assert_eq!(
            wake_person(&tx, "acme", "bo", "2026-08-13T09:00:00.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
        );
        tx.commit().unwrap();

        assert_eq!(fenced(&conn), vec!["bo".to_string()], "THE GRANT, and nobody else's");
        assert_eq!(
            pointer(&conn, "bo"),
            None,
            "the lapsed park is released, or the fence discards the grant it was just given \
             and withdraws it again on the same pass (#638)"
        );
        assert_eq!(
            desired(&conn, "bo"),
            Some(0),
            "LOAD-BEARING: pre-setting desired-active suppresses the `Requested` reason, which \
             is the only thing that brings a stopped person up"
        );
    }

    /// The park row itself survives as the historical fact it is. Only the
    /// pointer moves, because the pointer is what the fence reads.
    #[test]
    fn waking_leaves_a_terminal_park_applied_rather_than_rewriting_history() {
        let mut conn = open();
        park(&conn, "bo", "eng");
        let tx = conn.transaction().unwrap();
        wake_person(&tx, "acme", "bo", "2026-08-13T09:00:00.000Z", "operator").unwrap();
        tx.commit().unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM transitions WHERE slug='acme' AND id='park-bo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "applied", "an applied park happened; a wake does not un-happen it");
    }

    /// A person still SETTLING — their park open rather than terminal — is
    /// woken by cancelling it, which is exactly what `activity::reconcile` does
    /// for itself when work arrives ("ordinary idle parking yields to newly
    /// arrived work").
    #[test]
    fn waking_someone_mid_settle_cancels_the_open_park() {
        let mut conn = open();
        park(&conn, "bo", "eng");
        conn.execute(
            "UPDATE transitions SET status='awaiting_handoff', applied_at=NULL \
             WHERE slug='acme' AND id='park-bo'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        wake_person(&tx, "acme", "bo", "2026-08-13T09:00:00.000Z", "operator").unwrap();
        tx.commit().unwrap();
        let (status, reason): (String, String) = conn
            .query_row(
                "SELECT status, reason FROM transitions WHERE slug='acme' AND id='park-bo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(reason, "superseded-by-wake:bo", "the override fact lives on the cancelled row");
        assert_eq!(pointer(&conn, "bo"), None);
    }

    /// An OPERATOR'S park, or a lifecycle command's, is somebody's explicit
    /// decision and is never touched. A wake releases the SCHEDULER's hint
    /// only — anything else would let a rail click undo an attended shutdown.
    #[test]
    fn waking_never_releases_an_intent_bound_park() {
        let mut conn = open();
        park(&conn, "bo", "eng");
        conn.execute(
            "UPDATE transitions SET intent_id='shutdown-1', reason='Operator shutdown.' \
             WHERE slug='acme' AND id='park-bo'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        wake_person(&tx, "acme", "bo", "2026-08-13T09:00:00.000Z", "operator").unwrap();
        tx.commit().unwrap();
        assert_eq!(
            pointer(&conn, "bo"),
            Some("park-bo".to_string()),
            "an attended decision is not a scheduler hint and a click must not undo it"
        );
    }

    /// Idempotent, and writeless on a REPLAY: the same click delivered twice, or
    /// a retried request, must not grow the event stream.
    ///
    /// RE-RULED, and narrowed to what it can still honestly claim. It used to
    /// wake at 09:00:00 and again at 09:00:01 and assert the second wrote
    /// nothing, on the reading that "the fence and the pointer are already where
    /// the wake wants them". That reading is no longer complete: a wake also
    /// stamps `person_activity.operator_wake_at`, and that stamp is the durable
    /// quiet lease the operator's ruling of 2026-08-20 buys — "if woken, it needs
    /// to wait the 2 mins". A click one second after the last one is a SECOND
    /// decision and it restarts that floor, so writing nothing would be losing
    /// it.
    ///
    /// So the two halves are separated. An exact replay is still writeless,
    /// which is what "an operator clicking twice must not grow the event stream"
    /// was really protecting.
    #[test]
    fn waking_twice_is_harmless_and_an_exact_replay_writes_nothing() {
        let mut conn = open();
        park(&conn, "bo", "eng");
        let tx = conn.transaction().unwrap();
        wake_person(&tx, "acme", "bo", "2026-08-13T09:00:00.000Z", "operator").unwrap();
        let first: i64 = tx
            .query_row("SELECT MAX(seq) FROM org_events WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        // THE SAME CLICK AGAIN: nothing has changed, so nothing is written.
        assert_eq!(
            wake_person(&tx, "acme", "bo", "2026-08-13T09:00:00.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
        );
        let replayed: i64 = tx
            .query_row("SELECT MAX(seq) FROM org_events WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            replayed, first,
            "the fence, the pointer and the wake instant are already where the wake wants them"
        );

        // A LATER CLICK: a second decision, and it restarts the quiet lease.
        assert_eq!(
            wake_person(&tx, "acme", "bo", "2026-08-13T09:00:01.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
        );
        let woken_at: Option<String> = tx
            .query_row(
                "SELECT operator_wake_at FROM person_activity WHERE slug='acme' AND person_id='bo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            woken_at.as_deref(),
            Some("2026-08-13T09:00:01.000Z"),
            "a second click restarts the floor rather than inheriting what is left of the first"
        );
    }

    /// THE SUBTREE FENCE, and there is no role gate beside it. `bo` heads
    /// nothing, so `bo` may not wake `cos`, who lives in a different unit.
    #[test]
    fn waking_refuses_an_actor_who_does_not_manage_the_target() {
        let mut conn = open();
        park(&conn, "cos", "office-of-the-ceo");
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            wake_person(&tx, "acme", "cos", "2026-08-13T09:00:00.000Z", "bo").unwrap(),
            DirectOutcome::Refused { code: "actor-out-of-scope", .. }
        ));
        drop(tx);
        assert_eq!(fenced(&conn), Vec::<String>::new(), "a refused wake opens no fence");
    }

    /// The CEO heads the root, so the CEO reaches everybody — the subtree rule,
    /// not an exemption granted to a title.
    #[test]
    fn the_ceo_wakes_anybody_because_the_ceo_heads_the_root() {
        let mut conn = open();
        park(&conn, "cos", "office-of-the-ceo");
        let tx = conn.transaction().unwrap();
        assert_eq!(
            wake_person(&tx, "acme", "cos", "2026-08-13T09:00:00.000Z", "ada").unwrap(),
            DirectOutcome::Applied,
        );
        tx.commit().unwrap();
        assert_eq!(fenced(&conn), vec!["cos".to_string()]);
    }

    /// A WAKE RECALLS A BENCHED PERSON.
    ///
    /// # Why the old assertion here was wrong
    ///
    /// This test used to require the opposite, on the reasoning that returning
    /// somebody is a STAFFING decision the product must not make because a rail
    /// row was clicked. That is a coherent model and it is not the operator's:
    /// "there's no such thing as bench. When I click, it's always wake. There is
    /// no stopping decision. Wake him right away."
    ///
    /// The old rule cost them a confusing minute on their own company — they
    /// clicked a row the rail drew as `sleeping`, chiefd answered
    /// `person-not-staffed`, the notice flashed past, and they reported the
    /// person as stuck. A refusal whose remedy is "type a different verb" is a
    /// refusal that reads as a fault.
    ///
    /// The recall is the SAME durable event `recall_person` writes, in the same
    /// transaction as the fence — two commits would leave a window where the
    /// person is active but unfenced, which the next reconcile would read as
    /// somebody who should be up and is not.
    #[test]
    fn waking_a_benched_person_recalls_them_instead_of_refusing() {
        let mut conn = open();
        park(&conn, "bo", "eng");
        conn.execute(
            "UPDATE people SET employment_state='benched' WHERE slug='acme' AND id='bo'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            wake_person(&tx, "acme", "bo", "2026-08-13T09:00:00.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
            "a click is always a wake"
        );
        tx.commit().unwrap();

        assert_eq!(fenced(&conn), vec!["bo".to_string()], "and they are fenced to run");
        let employment: String = conn
            .query_row(
                "SELECT employment_state FROM people WHERE slug='acme' AND id='bo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(employment, "active", "the bench did not survive the click");
    }

    /// A WAKE RESETS THE QUIET CLOCK, not merely the park.
    ///
    /// # The loop this closes
    ///
    /// Cancelling the park removed the DECISION and left the REASON standing.
    /// `agent_quiet_at` is when the person last went quiet, and a wake did not
    /// touch it — so somebody woken after an hour asleep was, to the very next
    /// reconcile, somebody who had been quiet for an hour, and it parked them
    /// again at once.
    ///
    /// Measured on the operator's own company: `rhea` went quiet at 23:18:42 and
    /// was clicked at 00:01:43. Her pane came up and thirty seconds later the
    /// log read `launch intent withdrawn (settled)`. Their report — she "shows
    /// starting and never resolves" — is exactly what a wake-park-reap loop
    /// looks like from the rail.
    #[test]
    fn waking_clears_the_quiet_clock_so_the_settle_cannot_reparked_them_at_once() {
        let mut conn = open();
        park(&conn, "bo", "eng");
        // Bo went quiet long ago — the state a sleeping person is always in.
        conn.execute(
            "UPDATE person_activity SET agent_quiet_at='2026-08-13T08:00:00.000Z' \
             WHERE slug='acme' AND person_id='bo'",
            [],
        )
        .unwrap();

        let tx = conn.transaction().unwrap();
        assert_eq!(
            wake_person(&tx, "acme", "bo", "2026-08-13T09:00:00.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
        );
        tx.commit().unwrap();

        let (quiet, idle): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT agent_quiet_at, idle_since FROM person_activity \
                 WHERE slug='acme' AND person_id='bo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            quiet, None,
            "an hour of silence BEFORE the wake is spent — the countdown to the next park \
             starts from the wake, not from before it"
        );
        assert_eq!(idle, None, "and the idle clock with it");
    }

    /// DEPARTED IS STILL REFUSED, and it is not the same thing as benched.
    ///
    /// Firing is a decision somebody made about a person. The rail never draws a
    /// departed person at all — "we never see fired employees" — so a click
    /// cannot reach one, and a wake that silently un-fired somebody would be
    /// answering a question nobody asked.
    #[test]
    fn waking_a_departed_person_still_refuses_because_a_rehire_is_not_a_click() {
        let mut conn = open();
        park(&conn, "bo", "eng");
        conn.execute(
            "UPDATE people SET employment_state='departed' WHERE slug='acme' AND id='bo'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            wake_person(&tx, "acme", "bo", "2026-08-13T09:00:00.000Z", "operator").unwrap(),
            DirectOutcome::Refused { code: "person-departed", .. }
        ));
        drop(tx);
        assert_eq!(fenced(&conn), Vec::<String>::new(), "and nothing was opened for them");
    }

    #[test]
    fn waking_refuses_a_paused_department_without_opening_the_fence() {
        let mut conn = open();
        park(&conn, "bo", "eng");
        conn.execute("UPDATE departments SET state='paused' WHERE slug='acme' AND id='eng'", [])
            .unwrap();
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            wake_person(&tx, "acme", "bo", "2026-08-13T09:00:00.000Z", "operator").unwrap(),
            DirectOutcome::Refused { code: "destination-paused", .. }
        ));
        drop(tx);
        assert_eq!(fenced(&conn), Vec::<String>::new());
    }

    #[test]
    fn waking_a_stranger_refuses_and_names_the_reason() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            wake_person(&tx, "acme", "nobody", "2026-08-13T09:00:00.000Z", "operator").unwrap(),
            DirectOutcome::Refused { code: "unknown-person", .. }
        ));
    }

    /// STOP MEANS STOP, at the two verbs that mean "run this person".
    ///
    /// The incident: an operator told the company to stop, the CEO obeyed and
    /// parked six people, and the company put every one of them back. A
    /// stand-down that the start verbs did not consult would be the same rule
    /// the per-person watermark already was — one enforced against the path
    /// that happened to break, and against everything else by goodwill.
    #[test]
    fn a_stood_down_company_refuses_every_start_and_wake_and_opens_no_fence() {
        let mut conn = open();
        {
            let tx = conn.transaction().unwrap();
            crate::store::stand_down::stand_down(&tx, "acme", "2026-08-18T10:00:00.000Z", "")
                .unwrap();
            tx.commit().unwrap();
        }
        let tx = conn.transaction().unwrap();

        for outcome in [
            start_person(&tx, "acme", "bo", "2026-08-18T10:01:00.000Z", "operator").unwrap(),
            wake_person(&tx, "acme", "bo", "2026-08-18T10:01:00.000Z", "operator").unwrap(),
        ] {
            let DirectOutcome::Refused { code, detail } = outcome else {
                panic!("a stood-down company must refuse: {outcome:?}");
            };
            assert_eq!(code, "company-stood-down");
            assert!(detail.contains("chief resume"), "the refusal names the way out: {detail}");
        }

        let fences: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fences, 0, "a refused start never opens a fence");
        assert_eq!(desired(&tx, "bo"), None, "and never raises the durable demand either");
    }

    /// Resuming makes the same verb work again — the stand-down is a state, not
    /// a one-way door.
    #[test]
    fn a_resumed_company_starts_people_again() {
        let mut conn = open();
        {
            let tx = conn.transaction().unwrap();
            crate::store::stand_down::stand_down(&tx, "acme", "t0", "").unwrap();
            crate::store::stand_down::resume(&tx, "acme", "t1").unwrap();
            tx.commit().unwrap();
        }
        let tx = conn.transaction().unwrap();
        assert_eq!(
            start_person(&tx, "acme", "bo", "2026-08-18T10:02:00.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
        );
        assert_eq!(desired(&tx, "bo"), Some(1));
    }

    /// A hire during a stand-down still creates the durable person — the
    /// structure is not what was stopped — and starts NOBODY.
    ///
    /// Silent rather than refused, deliberately: refusing the whole hire would
    /// tell the caller their person was not created when they were. The
    /// operator's instruction said "do not hire" AND "do not start anyone"; the
    /// half this mechanism owns is the second.
    #[test]
    fn a_hire_during_a_stand_down_creates_the_person_and_fences_nobody() {
        let mut conn = open();
        {
            let tx = conn.transaction().unwrap();
            crate::store::stand_down::stand_down(&tx, "acme", "t0", "").unwrap();
            tx.commit().unwrap();
        }
        let tx = conn.transaction().unwrap();
        let mut touches = Vec::new();
        fence_started_person(&tx, "acme", "bo", &mut touches).unwrap();
        assert!(touches.is_empty(), "no fence touch was emitted");
        let fences: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fences, 0, "the shared staffing helper opens no fence while stood down");
    }

    #[test]
    fn start_person_refuses_paused_people_without_opening_the_fence() {
        let mut conn = open();
        conn.execute("UPDATE departments SET state='paused' WHERE slug='acme' AND id='eng'", [])
            .unwrap();
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            start_person(&tx, "acme", "bo", "2026-08-02T13:10:45.000Z", "operator").unwrap(),
            DirectOutcome::Refused { code: "destination-paused", .. }
        ));
        let fences: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fences, 0, "a refused start never opens a fence");
    }

    #[test]
    fn starting_the_ceo_never_creates_an_idle_lease() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            start_person(&tx, "acme", "ada", "2026-08-02T13:10:44.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
        );
        let clocks: (Option<String>, Option<String>, Option<String>) = tx
            .query_row(
                "SELECT agent_quiet_at, idle_since, agent_active_at FROM person_activity \
                 WHERE slug='acme' AND person_id='ada'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(clocks, (None, None, None), "the root CEO holds a permanent lease");
    }

    #[test]
    fn explicit_start_refreshes_the_lease_but_does_not_release_an_intent_bound_stop() {
        let mut conn = open();
        conn.execute_batch(
            "INSERT INTO transitions(slug, id, person_id, action, status, intent_id, reason, \
               placement_department_id, requested_at, handoff_deadline_at) \
             VALUES('acme','stop-bo','bo','park','ready','person-stop:bo','Operator stop.', \
               'eng','2026-08-02T13:00:00.000Z','2026-08-02T13:00:00.000Z'); \
             INSERT INTO person_activity(slug, person_id, last_desired_active, \
               agent_quiet_at, idle_since, active_transition_id, updated_at) \
             VALUES('acme','bo',0,'2026-08-02T12:00:00.000Z','2026-08-02T12:00:00.000Z', \
               'stop-bo','2026-08-02T13:00:00.000Z');",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            start_person(&tx, "acme", "bo", "2026-08-02T13:10:44.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
        );
        let (status, intent, pointer): (String, Option<String>, Option<String>) = tx
            .query_row(
                "SELECT t.status, t.intent_id, p.active_transition_id \
                 FROM transitions t JOIN person_activity p \
                   ON p.slug=t.slug AND p.person_id=t.person_id \
                 WHERE t.slug='acme' AND t.id='stop-bo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "ready");
        assert_eq!(intent.as_deref(), Some("person-stop:bo"));
        assert_eq!(pointer.as_deref(), Some("stop-bo"), "the attended stop stays authoritative");
        let (desired, quiet, idle): (i64, Option<String>, Option<String>) = tx
            .query_row(
                "SELECT last_desired_active, agent_quiet_at, idle_since FROM person_activity \
                 WHERE slug='acme' AND person_id='bo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(desired, 1, "the explicit start remains a successful start decision");
        assert_eq!(quiet.as_deref(), Some("2026-08-02T13:10:44.000Z"));
        assert_eq!(idle.as_deref(), Some("2026-08-02T13:10:44.000Z"));
        let fences: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM launch_intent WHERE slug='acme' AND person_id='bo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fences, 1, "the stop is preserved without discarding the new start fence");
    }

    /// #1036 REPLACES the old `start_person_refuses_a_departed_person`. The
    /// bench ruling ("If I'm bringing you up to work, that means I'm asking
    /// you to return. That is always a given.") extends to departure: a
    /// departed person whose home department still exists is REHIRED and
    /// started by one call. Departed-retention itself is untouched -- this is
    /// the same person coming back, not a re-creation, which is why
    /// `hire_person` still refuses the id (pinned separately below).
    #[test]
    fn start_person_atomically_rehires_a_departed_person_and_starts_them() {
        let mut conn = open();
        conn.execute(
            "UPDATE people SET employment_state='departed' WHERE slug='acme' AND id='bo'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            start_person(&tx, "acme", "bo", "2026-08-02T13:10:44.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
            "a departed person is rehired and started in one call, never refused",
        );
        let (employment, kind): (String, String) = tx
            .query_row(
                "SELECT employment_state, kind FROM people WHERE slug='acme' AND id='bo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(employment, "active");
        assert_eq!(kind, "worker");
        assert_eq!(desired(&tx, "bo"), Some(1), "the rehired person comes back UP");
        let fences: Vec<String> = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(fences, vec!["bo"], "the reconciler converges them to running");
        let action: String = tx
            .query_row(
                "SELECT action FROM staffing_history WHERE slug='acme' AND person_id='bo' ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(action, "rehired", "the rehire is durably auditable, and is not a `recalled`");
    }

    /// The live incident's exact shape: the department the person departed
    /// from was deleted along with them. `start_person` carries no department
    /// argument, so there is genuinely nowhere to put them -- refuse and say
    /// so, rather than silently re-homing them at the root.
    #[test]
    fn start_person_refuses_a_rehire_whose_home_department_is_gone() {
        let mut conn = open();
        conn.execute_batch(
            "UPDATE people SET employment_state='departed' WHERE slug='acme' AND id='bo'; \
             DELETE FROM departments WHERE slug='acme' AND id='eng';",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            start_person(&tx, "acme", "bo", "2026-08-02T13:10:44.000Z", "operator").unwrap(),
            DirectOutcome::Refused { code: "home-department-gone", .. }
        ));
        let employment: String = tx
            .query_row(
                "SELECT employment_state FROM people WHERE slug='acme' AND id='bo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(employment, "departed", "a refused rehire touches no row");
        let fences: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fences, 0, "a refused rehire never opens a fence");
    }

    /// A fired HEAD comes back as a worker. Their old seat has a successor in
    /// it and the rehire must not contest it; leading again is a separate
    /// appointment.
    #[test]
    fn a_rehired_head_returns_as_a_worker() {
        let mut conn = open();
        conn.execute(
            "UPDATE people SET employment_state='departed', kind='head' \
             WHERE slug='acme' AND id='bo'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            start_person(&tx, "acme", "bo", "2026-08-02T13:10:44.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
        );
        let kind: String = tx
            .query_row("SELECT kind FROM people WHERE slug='acme' AND id='bo'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(kind, "worker", "a rehire never restores a headship");
    }

    /// The departure left the activity pointer aimed at the offboard
    /// transition. `ensure_person_activity_desired_active` preserves an
    /// existing pointer by design, so the rehire has to clear it explicitly --
    /// otherwise the person comes back up still pointing at their own
    /// departure.
    #[test]
    fn a_rehire_clears_the_departure_transition_pointer() {
        let mut conn = open();
        conn.execute_batch(
            "UPDATE people SET employment_state='departed' WHERE slug='acme' AND id='bo'; \
             INSERT INTO person_activity(slug, person_id, last_desired_active, active_transition_id, updated_at) \
               VALUES('acme','bo',0,'transition:3:bo:offboard','before');",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            start_person(&tx, "acme", "bo", "2026-08-02T13:10:44.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
        );
        let pointer: Option<String> = tx
            .query_row(
                "SELECT active_transition_id FROM person_activity WHERE slug='acme' AND person_id='bo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pointer, None, "the departure transition no longer governs the returned person");
        assert_eq!(desired(&tx, "bo"), Some(1));
    }

    /// #1036: ids are NEVER reusable, and a rehire does not change that.
    /// Departed-retention is untouched -- rehiring is the SAME retained person
    /// coming back through `start_person`, so creating a NEW person under that
    /// id stays refused both before and after.
    #[test]
    fn a_rehire_never_makes_the_person_id_reusable() {
        let replacement = NewPersonSeed {
            name: "Bo II",
            title: "Engineer",
            mandate: "build",
            kind: PersonKind::Worker,
            employment_state: EmploymentState::Active,
            activation: "resident",
            tools: &[],
            prompts: &[],
        };
        let mut conn = open();
        conn.execute(
            "UPDATE people SET employment_state='departed' WHERE slug='acme' AND id='bo'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            hire_person(&tx, "acme", "bo", "eng", &replacement, "ada", "t1", "chief").unwrap(),
            HireOutcome::Refused { reason: HireRefusal::DuplicatePersonId },
            "departed-retention burns the id -- a hire never resurrects",
        );
        assert_eq!(
            start_person(&tx, "acme", "bo", "2026-08-02T13:10:44.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
            "but start_person brings the retained person back",
        );
        assert_eq!(
            hire_person(&tx, "acme", "bo", "eng", &replacement, "ada", "t2", "chief").unwrap(),
            HireOutcome::Refused { reason: HireRefusal::DuplicatePersonId },
            "and the id is still not reusable afterwards",
        );
    }

    /// P0 (human ruling, verbatim): "If I'm bringing you up to work, that
    /// means I'm asking you to return. That is always a given." A benched
    /// person is recalled and started by ONE start-person call, in the same
    /// transaction -- the standalone "recall the person before starting
    /// them" refusal no longer exists on this path.
    #[test]
    fn start_person_atomically_recalls_a_benched_person_and_starts_them() {
        let mut conn = open();
        conn.execute(
            "UPDATE people SET employment_state='benched' WHERE slug='acme' AND id='bo'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            start_person(&tx, "acme", "bo", "2026-08-02T13:10:44.000Z", "operator").unwrap(),
            DirectOutcome::Applied,
            "a benched person is recalled and started in one call, never refused",
        );
        let employment: String = tx
            .query_row(
                "SELECT employment_state FROM people WHERE slug='acme' AND id='bo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(employment, "active");
        assert_eq!(desired(&tx, "bo"), Some(1));
        let fences: Vec<String> = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(fences, vec!["bo"]);
        let action: String = tx
            .query_row(
                "SELECT action FROM staffing_history WHERE slug='acme' AND person_id='bo' ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            action, "recalled",
            "the implicit recall is durably recorded in staffing history"
        );
    }

    /// The recall half and the start half either both land or neither does --
    /// no caller-visible intermediate "recalled but not started" state. Force
    /// a real failure AFTER the recall write (a missing `launch_intent` table
    /// breaks the fence insert) and prove the transaction, once rolled back,
    /// left the person exactly as benched as it found them.
    #[test]
    fn start_person_leaves_no_partial_recall_when_the_start_half_fails() {
        let mut conn = open();
        conn.execute(
            "UPDATE people SET employment_state='benched' WHERE slug='acme' AND id='bo'",
            [],
        )
        .unwrap();
        conn.execute("DROP TABLE launch_intent", []).unwrap();
        {
            let tx = conn.transaction().unwrap();
            assert!(
                start_person(&tx, "acme", "bo", "2026-08-02T13:10:44.000Z", "operator").is_err(),
                "the broken fence table must surface as an error, not a silent partial commit",
            );
            // `tx` drops here without `.commit()` -- rusqlite rolls the whole
            // transaction back, including the recall write the closure
            // already ran before the fence insert failed.
        }
        let employment: String = conn
            .query_row(
                "SELECT employment_state FROM people WHERE slug='acme' AND id='bo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            employment, "benched",
            "a failed start-person must leave no partial recall behind"
        );
    }

    #[test]
    fn ceo_only_prepare_retracts_everybody_else_and_asks_for_the_root_by_name() {
        let mut conn = open_reorg();
        conn.execute_batch(
            "INSERT INTO person_activity(slug, person_id, last_desired_active, active_transition_id, updated_at) VALUES \
               ('acme','ada',1,NULL,'before'), \
               ('acme','cos',1,'transition:7:cos:park','before'), \
               ('acme','bo',1,NULL,'before'); \
             INSERT INTO launch_intent(slug, person_id) VALUES ('acme','cos'), ('acme','bo');",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let seq = prepare_ceo_only(&tx, "acme", "2026-08-02T13:10:44.000Z", "company-ceo")
            .expect("CEO-only preparation");
        assert_eq!(
            seq, 6,
            "activity, launch-intent (two clears plus the root's own grant) and quiesce touches \
             commit together"
        );
        assert_eq!(desired(&tx, "ada"), Some(1), "the normalized root head remains desired-active");
        assert_eq!(desired(&tx, "cos"), Some(0));
        assert_eq!(desired(&tx, "bo"), Some(0));
        let pointer: Option<String> = tx
            .query_row(
                "SELECT active_transition_id FROM person_activity WHERE slug='acme' AND person_id='cos'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            pointer.as_deref(),
            Some("transition:7:cos:park"),
            "unrelated activity state is preserved"
        );
        // THE ROOT IS ASKED FOR BY NAME, and it is the ONLY one asked for.
        //
        // This used to assert an EMPTY fence, which was only survivable because
        // `activity::reconcile` handed the CEO permanent demand unconditionally.
        // With that exemption going, an empty fence here would mean the operator
        // attached to a company where nobody is desired-active and nothing can
        // ever start one. The grant IS the answer to "what brings the root
        // back", so it is asserted by CONTENT -- which is also this operation's
        // own negative: prepare CEO ONLY grants the root and nobody else.
        assert_eq!(
            fenced_people(&tx),
            vec!["ada".to_owned()],
            "the root gets a start decision and every report's is cleared"
        );
        let quiesced_at: String = tx
            .query_row("SELECT since FROM quiesce WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(quiesced_at, "2026-08-02T13:10:44.000Z");
        let quiesce_event: (i64, String, String) = tx
            .query_row(
                "SELECT seq, entity_id, op FROM org_events \
                 WHERE slug='acme' AND entity='goal-delivery-quiesce'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(quiesce_event, (6, "acme".into(), "upsert".into()));

        let same = prepare_ceo_only(&tx, "acme", "2026-08-02T13:10:44.000Z", "company-ceo")
            .expect("idempotent CEO-only preparation");
        assert_eq!(same, seq, "the identical CEO-only durable state emits no new event");

        let advanced = prepare_ceo_only(&tx, "acme", "2026-08-02T13:10:45.000Z", "company-ceo")
            .expect("new CEO-only episode");
        assert_eq!(advanced, seq + 1, "a newer episode advances only its quiesce touch");
        let advanced_quiesce: String = tx
            .query_row("SELECT since FROM quiesce WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(advanced_quiesce, "2026-08-02T13:10:45.000Z");
        tx.commit().unwrap();
    }

    /// A FRESH COMPANY COMES UP WITH THE ROOT RUNNING.
    ///
    /// Genesis calls this once on a company with no activity and no intent at
    /// all. Before the root had a real start decision it came up on an exemption
    /// in `activity::reconcile`; the grant below is what replaces that, so a
    /// brand-new company still has somebody to talk to.
    #[test]
    fn a_fresh_company_gets_a_start_decision_for_its_root() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();

        prepare_ceo_only(&tx, "acme", "2026-08-02T13:10:44.000Z", "company-ceo")
            .expect("CEO-only preparation on a fresh company");

        assert_eq!(
            fenced_people(&tx),
            vec!["ada".to_owned()],
            "a company with nothing durable yet still asks for its root by name"
        );
        tx.commit().unwrap();
    }

    /// AN ATTACH RE-ASSERTS THE ROOT AFTER IT HAS SETTLED.
    ///
    /// The lapse is the point of using a start decision rather than an
    /// exemption: once the root is up the grant stops contributing demand, it
    /// settles on the ordinary quiet lease, and the settle path withdraws its
    /// intent -- modelled here by deleting the row and clearing desired-active.
    /// The operator's next attach must be able to ask again, or a company that
    /// went quiet could never be re-entered.
    #[test]
    fn a_later_attach_asks_for_the_root_again_after_it_was_withdrawn() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        prepare_ceo_only(&tx, "acme", "2026-08-02T13:10:44.000Z", "company-ceo")
            .expect("first attach");

        // The root ran, went quiet, parked, and the settle path withdrew it.
        tx.execute("DELETE FROM launch_intent WHERE slug='acme' AND person_id='ada'", []).unwrap();
        tx.execute(
            "INSERT INTO person_activity(slug, person_id, last_desired_active, updated_at) \
             VALUES('acme','ada',0,'settled') \
             ON CONFLICT(slug, person_id) DO UPDATE SET last_desired_active = 0",
            [],
        )
        .unwrap();
        assert!(fenced_people(&tx).is_empty(), "the settled root holds no start decision");

        prepare_ceo_only(&tx, "acme", "2026-08-02T13:10:46.000Z", "company-ceo")
            .expect("the operator attaches again");

        assert_eq!(
            fenced_people(&tx),
            vec!["ada".to_owned()],
            "the operator's arrival is the root's demand, every time they arrive"
        );
        tx.commit().unwrap();
    }

    /// The launch-intent ids this company currently names, in id order.
    fn fenced_people(tx: &Transaction<'_>) -> Vec<String> {
        let mut statement = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    #[test]
    fn shutdown_writes_an_abandoned_terminal_park_clears_desired_and_intent() {
        let mut conn = open();
        conn.execute("INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo')", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out = shutdown_person(
            &tx,
            "acme",
            "bo",
            &ShutdownKind::AutomaticSettle,
            "2026-07-25T00:00:00.000Z",
            "",
        )
        .unwrap();
        assert!(matches!(out, ShutdownOutcome::Applied { .. }));
        // The sanctioned terminal shape for a settle nobody released:
        // `cancelled` with the explicit `abandoned_at` marker — NEVER `applied`,
        // which records that the transition's OWNER released it. An automatic
        // settle has no owner awake to do that, so `applied` here would make a
        // forced teardown indistinguishable from a cooperative one forever
        // after (#39-followup).
        let (status, action, intent, abandoned, reason): (String, String, Option<String>, Option<String>, String) = tx
            .query_row(
                "SELECT status, action, intent_id, abandoned_at, reason FROM transitions WHERE slug='acme' AND person_id='bo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(action, "park");
        assert_eq!(intent, None);
        assert!(abandoned.is_some(), "the abandoned marker is explicit");
        assert!(!reason.trim().is_empty(), "validate requires a non-empty reason");
        // The row is valid under the model's own `validate`: real placement,
        // never NULLs.
        let (placement, deadline): (String, String) = tx
            .query_row(
                "SELECT placement_department_id, handoff_deadline_at \
                 FROM transitions WHERE slug='acme' AND person_id='bo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        // #751-P9: the row records `bo`'s OWN unit. It used to also carry
        // `from_pane_department_id` = "executive", because `bo` heads `eng` and
        // a head's pane was drawn in the parent's window — a display transform
        // the backend no longer performs or stores.
        assert_eq!(placement, "eng");
        assert!(!deadline.is_empty());
        assert_eq!(desired(&tx, "bo"), Some(0));
        let fenced: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM launch_intent WHERE slug='acme' AND person_id='bo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fenced, 0);
        tx.commit().unwrap();
    }

    #[test]
    fn commanded_stop_stamps_the_person_stop_intent_on_the_terminal_park() {
        // The owned-park intent must survive the accessor composition (gap-1 fix).
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        shutdown_person(
            &tx,
            "acme",
            "bo",
            &ShutdownKind::Commanded { intent_id: "person-stop:e2e".into() },
            "2026-07-25T00:00:00.000Z",
            "operator",
        )
        .unwrap();
        let (action, intent): (String, Option<String>) = tx
            .query_row(
                "SELECT action, intent_id FROM transitions WHERE slug='acme' AND person_id='bo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(action, "park");
        assert_eq!(intent.as_deref(), Some("person-stop:e2e"));
        tx.commit().unwrap();
    }

    #[test]
    fn shutdown_supersedes_an_open_transition_leaving_only_terminal_history() {
        let mut conn = open();
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, requested_at) \
             VALUES('acme','open-1','bo','park','ready','t0')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = shutdown_person(
            &tx,
            "acme",
            "bo",
            &ShutdownKind::Commanded { intent_id: "person-stop:42".into() },
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert!(matches!(out, ShutdownOutcome::Applied { .. }));
        // The open row is cancelled with the supersede marker (gap-2 fix: N4
        // stamps the reason the composition passes)...
        let (status, reason): (String, String) = tx
            .query_row(
                "SELECT status, reason FROM transitions WHERE slug='acme' AND id='open-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(reason, "superseded-by-shutdown:person-stop:42");
        // ...and the terminal row the stop minted is in the SAME cancelled family
        // (abandoned, not applied): both rows are closed history, nothing left
        // open, and nothing claims a release that never happened.
        let open: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM transitions WHERE slug='acme' AND person_id='bo' AND status NOT IN ('cancelled')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(open, 0, "no live transition survives a completed stop");
        let abandoned: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM transitions WHERE slug='acme' AND person_id='bo' AND abandoned_at IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(abandoned, 1, "the terminal row is the explicit abandoned record");
        tx.commit().unwrap();
    }

    #[test]
    fn shutdown_refuses_the_ceo_and_writes_nothing() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = shutdown_person(
            &tx,
            "acme",
            "ada",
            &ShutdownKind::Commanded { intent_id: "person-stop:1".into() },
            "2026-07-25T00:00:00.000Z",
            "operator",
        )
        .unwrap();
        assert_eq!(out, ShutdownOutcome::Refused { reason: ShutdownRefusal::CeoExempt });
        let txns: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(txns, 0);
        assert_eq!(desired(&tx, "ada"), None);
        tx.commit().unwrap();
    }

    /// INVERTED on 2026-08-13, deliberately, and kept rather than deleted so
    /// the reversal is visible in the file that asserted the old rule.
    ///
    /// This test was `refuses_office_of_the_ceo_staff_not_just_the_root_head`
    /// and it LOCKED the whole-executive-root exemption: a chief of staff in
    /// `office-of-the-ceo` was refused a commanded stop exactly like the CEO.
    /// The operator's corrected ruling is that a head may act on anyone in its
    /// own subtree and the CEO holds every tree — "he should be able to shut
    /// down a department, keep him around". Being homed beside the CEO is not
    /// a protection. The CEO half is unchanged and still asserted.
    #[test]
    fn shutdown_refuses_the_ceo_alone_not_the_staff_beside_them() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let cos = shutdown_person(
            &tx,
            "acme",
            "cos",
            &ShutdownKind::Commanded { intent_id: "person-stop:9".into() },
            "2026-07-25T00:00:00.000Z",
            "operator",
        )
        .unwrap();
        assert!(
            matches!(cos, ShutdownOutcome::Applied { .. }),
            "a chief of staff in office-of-the-ceo is stoppable like anybody else: {cos:?}"
        );
        assert!(is_ceo(&tx, "acme", "ada").unwrap());
        assert!(!is_ceo(&tx, "acme", "cos").unwrap(), "a chief of staff is not the CEO");
        assert!(!is_ceo(&tx, "acme", "bo").unwrap());
        tx.commit().unwrap();
    }

    /// The CEO is refused a commanded stop, and the refusal writes nothing.
    /// The other half of the inversion above: exactly one person is exempt.
    #[test]
    fn shutdown_still_refuses_the_ceo_and_writes_nothing_at_all() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let ada = shutdown_person(
            &tx,
            "acme",
            "ada",
            &ShutdownKind::Commanded { intent_id: "person-stop:9".into() },
            "2026-07-25T00:00:00.000Z",
            "operator",
        )
        .unwrap();
        assert_eq!(ada, ShutdownOutcome::Refused { reason: ShutdownRefusal::CeoExempt });
        let txns: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(txns, 0);
        tx.commit().unwrap();
    }

    #[test]
    fn unrelated_events_do_not_block_a_shutdown() {
        let mut conn = open();
        conn.execute(
            "INSERT INTO org_events(slug, seq, entity, entity_id, op, at) \
             VALUES('acme', 1, 'person', 'x', 'noop', 't')",
            [],
        )
        .unwrap();
        // D2 sequence allocation is counter-backed. Seed the counter alongside
        // the prior event exactly as a real committed writer would (see
        // `unrelated_prior_event_does_not_reject_a_direct_transfer`), so this
        // regression checks the "unrelated event doesn't block" semantics
        // rather than a deliberately corrupt feed (a raw insert with no
        // matching counter row can never happen through any production write
        // path — `rows_txn::allocate_seq` is the only non-test writer of
        // `org_events.seq`, verified by grep across chiefd-core/chiefd-host).
        conn.execute("INSERT INTO counters(name, value) VALUES('org-events:acme', 1)", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out = shutdown_person(
            &tx,
            "acme",
            "bo",
            &ShutdownKind::AutomaticSettle,
            "2026-07-25T00:00:00.000Z",
            "",
        )
        .unwrap();
        assert!(matches!(out, ShutdownOutcome::Applied { .. }));
        let txns: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(txns, 1);
        tx.commit().unwrap();
    }

    #[test]
    fn emits_row_level_touches_as_one_contiguous_seq_run() {
        // Composition order = seq order: supersede(cancel) → terminal → activity
        // → launch-fence delete. All touches come FROM the typed accessors, so
        // the feed is byte-uniform with N4/b4; op verbs stay N4 CRUD (upsert/
        // delete), never an intent word; one contiguous seq run (adjacency pin).
        let mut conn = open();
        conn.execute("INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo')", []).unwrap();
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, requested_at) \
             VALUES('acme','open-1','bo','park','ready','t0')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        shutdown_person(
            &tx,
            "acme",
            "bo",
            &ShutdownKind::Commanded { intent_id: "person-stop:7".into() },
            "2026-07-25T00:00:00.000Z",
            "operator",
        )
        .unwrap();
        let rows: Vec<(i64, String, String, String)> = tx
            .prepare(
                "SELECT seq, entity, op, detail_ref FROM org_events WHERE slug='acme' ORDER BY seq",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let ops: Vec<(&str, &str)> =
            rows.iter().map(|(_, e, o, _)| (e.as_str(), o.as_str())).collect();
        assert_eq!(
            ops,
            vec![
                ("transition", "upsert"),      // supersede (cancelled open row)
                ("transition", "upsert"),      // terminal park
                ("person-activity", "upsert"), // desired-off
                ("launch-intent", "delete"),   // fence row dropped
            ]
        );
        assert!(
            rows.iter().all(|(_, _, op, _)| op == "upsert" || op == "delete"),
            "op verbs must be N4 CRUD-level (upsert/delete) only"
        );
        let seqs: Vec<i64> = rows.iter().map(|(s, ..)| *s).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4]);
        assert!(rows.iter().any(|(_, _, _, d)| d == "person_activity:acme/bo"));
        assert!(rows.iter().any(|(_, _, _, d)| d == "launch_intent:acme/bo"));
        tx.commit().unwrap();
    }

    // --- H2: appoint_department_head fixture ------------------------------

    /// A full-schema org with the minimal rows needed by the H2 verb. `eng`
    /// (under root `executive`) is headed by `emery`; `quinn` is a worker member
    /// of `eng` holding `bash`. `emery` owns two manager goals, a delegated goal,
    /// a goal-watch and a check-in; `ceo` owns one manager goal (isolation
    /// control). The canonical schema is load-bearing for lifecycle regressions:
    /// head replacement also writes transitions and person activity.
    fn open_h2() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO departments(
                 slug, id, parent_id, name, kind, state, head_person_id,
                 ordinal, created_at, updated_at
             ) VALUES
               ('acme','executive',NULL,'Executive','company','active','chief',0,'t0','t0'),
               ('acme','eng','executive','Engineering','department','active','emery',1,'t0','t0');
             INSERT INTO people(
                 slug, id, name, title, mandate, kind, employment_state,
                 department_id, ordinal,
                 created_at, updated_at
             ) VALUES
               ('acme','chief','Chief','Chief','lead','executive','active','executive',0,'t0','t0'),
               ('acme','emery','Emery','Engineering Head','lead','head','active','eng',1,'t0','t0'),
               ('acme','quinn','Quinn','Engineer','build','worker','active','eng',2,'t0','t0');
             INSERT INTO person_tools(slug, person_id, tool, ordinal) VALUES
               ('acme','quinn','bash',0),('acme','quinn','read',1);",
        )
        .expect("seed");
        conn
    }

    // -----------------------------------------------------------------------
    // THE VACANCY DECISION — a head leaving the department they lead
    // -----------------------------------------------------------------------

    /// The genesis shape the operator hit: a Chief of Staff who heads
    /// `office-of-the-ceo` and is its ONLY member, plus a second department
    /// that has a spare member to hand over to.
    fn open_vacancy() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO org_settings(
                 slug, display_slug, supervision_interval_ms, acknowledgement_timeout_ms,
                 acknowledgement_retry_limit, replacement_limit
             ) VALUES('acme','acme',60000,30000,3,3);
             INSERT INTO departments(
                 slug, id, parent_id, name, kind, state, head_person_id,
                 ordinal, created_at, updated_at
             ) VALUES
               ('acme','executive',NULL,'Executive','company','active','chief',0,'2026-08-13T00:00:00.000Z','2026-08-13T00:00:00.000Z'),
               ('acme','office-of-the-ceo','executive','Office of the CEO','department','active','cos',1,'2026-08-13T00:00:00.000Z','2026-08-13T00:00:00.000Z'),
               ('acme','eng','executive','Engineering','department','active','emery',2,'2026-08-13T00:00:00.000Z','2026-08-13T00:00:00.000Z');
             INSERT INTO people(
                 slug, id, name, title, mandate, kind, employment_state,
                 department_id, ordinal, created_at, updated_at
             ) VALUES
               ('acme','chief','Chief','Chief','lead','executive','active','executive',0,'2026-08-13T00:00:00.000Z','2026-08-13T00:00:00.000Z'),
               ('acme','cos','Cos','Chief of Staff','support','head','active','office-of-the-ceo',1,'2026-08-13T00:00:00.000Z','2026-08-13T00:00:00.000Z'),
               ('acme','emery','Emery','Engineering Head','lead','head','active','eng',2,'2026-08-13T00:00:00.000Z','2026-08-13T00:00:00.000Z'),
               ('acme','quinn','Quinn','Engineer','build','worker','active','eng',3,'2026-08-13T00:00:00.000Z','2026-08-13T00:00:00.000Z'),
               ('acme','gone','Gone','Engineer','build','worker','departed','eng',4,'2026-08-13T00:00:00.000Z','2026-08-13T00:00:00.000Z');",
        )
        .expect("seed");
        conn
    }

    fn head_of(tx: &Transaction<'_>, department_id: &str) -> Option<String> {
        organization_rows::department_head(tx, "acme", department_id).unwrap()
    }

    fn placement_of(tx: &Transaction<'_>, person_id: &str) -> Option<(String, String)> {
        organization_rows::person_placement(tx, "acme", person_id).unwrap()
    }

    /// The reconstructed manifest must satisfy the SAME validator every later
    /// whole-ledger read runs. This is the claim the whole design rests on:
    /// no unit is ever left without a head, and no person ever heads two.
    fn assert_manifest_valid(tx: &Transaction<'_>) {
        let manifest =
            organization_rows::reconstruct(tx, "acme").expect("reconstruct").expect("a manifest");
        crate::store::organization::validate_organization_manifest(&manifest)
            .expect("the committed rows must satisfy validate");
    }

    /// THE OPERATOR'S CASE. A Chief of Staff who is the only member of the
    /// department they head becomes the head of a NEW department, and the
    /// emptied one goes with them — in ONE transaction.
    #[test]
    fn a_last_member_heads_a_new_department_and_the_emptied_one_dissolves() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = create_department_with_staff_unit(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "cos".into() },
            &[],
            &DepartmentCreateUnit::Department,
            Some(&HeadVacancy::Dissolve),
            Some("chief"),
            "cos takes product",
            "2026-08-13T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert!(matches!(out, CreateDepartmentOutcome::Applied { .. }), "{out:?}");
        assert_eq!(head_of(&tx, "product").as_deref(), Some("cos"));
        assert_eq!(
            head_of(&tx, "office-of-the-ceo"),
            None,
            "the emptied department must be gone, not headless"
        );
        let (_employment, placement) = placement_of(&tx, "cos").expect("cos");
        assert_eq!(placement, "product");
        assert_eq!(
            organization_rows::person_kind(&tx, "acme", "cos").unwrap().as_deref(),
            Some("head")
        );
        assert_manifest_valid(&tx);
        tx.commit().unwrap();
    }

    /// The other answer: the department keeps running under a member of its own.
    #[test]
    fn a_head_with_a_successor_hands_over_and_the_department_survives() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = create_department_with_staff_unit(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "emery".into() },
            &[],
            &DepartmentCreateUnit::Department,
            Some(&HeadVacancy::HandOver { successor_person_id: "quinn".into() }),
            Some("chief"),
            "emery takes product",
            "2026-08-13T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert!(matches!(out, CreateDepartmentOutcome::Applied { .. }), "{out:?}");
        assert_eq!(head_of(&tx, "product").as_deref(), Some("emery"));
        assert_eq!(head_of(&tx, "eng").as_deref(), Some("quinn"), "eng keeps running");
        assert_eq!(
            organization_rows::person_kind(&tx, "acme", "quinn").unwrap().as_deref(),
            Some("head"),
            "the successor is a head now, not a worker heading something"
        );
        assert_manifest_valid(&tx);
        tx.commit().unwrap();
    }

    /// No decision refuses, names the department, and lists exactly the members
    /// who could take it — never the departed one.
    #[test]
    fn no_decision_refuses_and_names_the_department_and_its_successors() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = create_department_with_staff_unit(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "emery".into() },
            &[],
            &DepartmentCreateUnit::Department,
            None,
            Some("chief"),
            "emery takes product",
            "2026-08-13T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        let CreateDepartmentOutcome::Refused {
            reason: CreateDepartmentRefusal::HeadVacancy(vacancy),
        } = &out
        else {
            panic!("expected a vacancy refusal, got {out:?}");
        };
        assert_eq!(vacancy.code(), "vacancy-decision-required");
        let HeadVacancyRefusal::Required { department_id, eligible_successor_ids, .. } = vacancy
        else {
            panic!("expected the required arm, got {vacancy:?}");
        };
        assert_eq!(department_id, "eng");
        assert_eq!(
            eligible_successor_ids,
            &vec!["quinn".to_string()],
            "a departed member is not a successor"
        );
        assert!(vacancy.detail().contains("quinn"), "{}", vacancy.detail());
        // ZERO WRITES.
        assert_eq!(head_of(&tx, "eng").as_deref(), Some("emery"));
        assert!(organization_rows::department_state(&tx, "acme", "product").unwrap().is_none());
        tx.commit().unwrap();
    }

    /// A dissolve is refused while the department still holds somebody, and the
    /// refusal names who. Removing a populated department is a different verb,
    /// and it fires people.
    #[test]
    fn dissolve_is_refused_while_the_department_still_has_members() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = create_department_with_staff_unit(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "emery".into() },
            &[],
            &DepartmentCreateUnit::Department,
            Some(&HeadVacancy::Dissolve),
            Some("chief"),
            "emery takes product",
            "2026-08-13T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        let CreateDepartmentOutcome::Refused {
            reason: CreateDepartmentRefusal::HeadVacancy(vacancy),
        } = &out
        else {
            panic!("expected a vacancy refusal, got {out:?}");
        };
        assert_eq!(vacancy.code(), "vacancy-decision-invalid");
        assert!(vacancy.detail().contains("quinn"), "{}", vacancy.detail());
        assert_eq!(head_of(&tx, "eng").as_deref(), Some("emery"), "zero writes");
        tx.commit().unwrap();
    }

    /// A dissolve is refused while the department still has children, rather
    /// than silently reparenting departments the caller never named.
    #[test]
    fn dissolve_is_refused_while_the_department_still_has_children() {
        let mut conn = open_vacancy();
        {
            let tx = conn.transaction().unwrap();
            tx.execute_batch(
                "INSERT INTO departments(
                     slug, id, parent_id, name, kind, state, head_person_id,
                     ordinal, created_at, updated_at
                 ) VALUES
                   ('acme','ops','office-of-the-ceo','Ops','department','active','opie',3,'2026-08-13T00:00:00.000Z','2026-08-13T00:00:00.000Z');
                 INSERT INTO people(
                     slug, id, name, title, mandate, kind, employment_state,
                     department_id, ordinal, created_at, updated_at
                 ) VALUES
                   ('acme','opie','Opie','Ops Head','lead','head','active','ops',5,'2026-08-13T00:00:00.000Z','2026-08-13T00:00:00.000Z');",
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let tx = conn.transaction().unwrap();
        let out = create_department_with_staff_unit(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "cos".into() },
            &[],
            &DepartmentCreateUnit::Department,
            Some(&HeadVacancy::Dissolve),
            Some("chief"),
            "cos takes product",
            "2026-08-13T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        let CreateDepartmentOutcome::Refused {
            reason: CreateDepartmentRefusal::HeadVacancy(vacancy),
        } = &out
        else {
            panic!("expected a vacancy refusal, got {out:?}");
        };
        assert_eq!(vacancy.code(), "vacancy-decision-invalid");
        assert!(vacancy.detail().contains("ops"), "{}", vacancy.detail());
        assert!(head_of(&tx, "office-of-the-ceo").is_some(), "zero writes");
        tx.commit().unwrap();
    }

    /// A hand-over to somebody who is not a member of the vacated department is
    /// refused, naming them.
    #[test]
    fn hand_over_to_a_non_member_is_refused() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = create_department_with_staff_unit(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "emery".into() },
            &[],
            &DepartmentCreateUnit::Department,
            // `cos` heads a different department entirely.
            Some(&HeadVacancy::HandOver { successor_person_id: "cos".into() }),
            Some("chief"),
            "emery takes product",
            "2026-08-13T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        let CreateDepartmentOutcome::Refused {
            reason: CreateDepartmentRefusal::HeadVacancy(vacancy),
        } = &out
        else {
            panic!("expected a vacancy refusal, got {out:?}");
        };
        assert_eq!(vacancy.code(), "vacancy-decision-invalid");
        assert!(vacancy.detail().contains("cos"), "{}", vacancy.detail());
        tx.commit().unwrap();
    }

    /// A departed member is never offered, and never accepted.
    #[test]
    fn hand_over_to_a_departed_member_is_refused() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = create_department_with_staff_unit(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "emery".into() },
            &[],
            &DepartmentCreateUnit::Department,
            Some(&HeadVacancy::HandOver { successor_person_id: "gone".into() }),
            Some("chief"),
            "emery takes product",
            "2026-08-13T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert!(
            matches!(
                &out,
                CreateDepartmentOutcome::Refused {
                    reason: CreateDepartmentRefusal::HeadVacancy(_)
                }
            ),
            "{out:?}"
        );
        tx.commit().unwrap();
    }

    /// A decision supplied for somebody who heads nothing is refused rather
    /// than ignored: it means the caller has the wrong person in mind.
    #[test]
    fn a_decision_for_somebody_who_heads_nothing_is_refused() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = create_department_with_staff_unit(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "quinn".into() },
            &[],
            &DepartmentCreateUnit::Department,
            Some(&HeadVacancy::Dissolve),
            Some("chief"),
            "quinn takes product",
            "2026-08-13T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        let CreateDepartmentOutcome::Refused {
            reason: CreateDepartmentRefusal::HeadVacancy(vacancy),
        } = &out
        else {
            panic!("expected a vacancy refusal, got {out:?}");
        };
        assert_eq!(vacancy.code(), "vacancy-decision-invalid");
        assert!(vacancy.detail().contains("nothing to vacate"), "{}", vacancy.detail());
        tx.commit().unwrap();
    }

    /// THE ORDER THE UNIQUE INDEX FORCES. `departments_one_head` is UNIQUE on
    /// `(slug, head_person_id)`, so the old headship must end BEFORE the new
    /// department row names the same person. This test is what fails if a later
    /// refactor moves the vacate step after the insert: the whole transaction
    /// would be rejected by the index, with an error naming the index rather
    /// than the cause.
    #[test]
    fn the_old_headship_ends_before_the_new_row_names_the_same_head() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        create_department_with_staff_unit(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "cos".into() },
            &[],
            &DepartmentCreateUnit::Department,
            Some(&HeadVacancy::Dissolve),
            Some("chief"),
            "cos takes product",
            "2026-08-13T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        let headed: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND head_person_id='cos'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(headed, 1, "exactly one department may name a person as its head");
        tx.commit().unwrap();
    }

    /// The CEO stays immovable, decision or no decision. The root has no
    /// parent, so it can never be dissolved, and the CEO can never be a last
    /// member who leaves.
    #[test]
    fn the_ceo_is_still_refused_with_or_without_a_decision() {
        for decision in [None, Some(&HeadVacancy::Dissolve)] {
            let mut conn = open_vacancy();
            let tx = conn.transaction().unwrap();
            let out = create_department_with_staff_unit(
                &tx,
                "acme",
                "product",
                "executive",
                "Product",
                "Ship product",
                &HeadDecision::AppointExisting { person_id: "chief".into() },
                &[],
                &DepartmentCreateUnit::Department,
                decision,
                Some("chief"),
                "ceo takes product",
                "2026-08-13T00:00:00.000Z",
                "chief",
            )
            .unwrap();
            let CreateDepartmentOutcome::Refused { reason } = &out else {
                panic!("the CEO must never be appointed elsewhere: {out:?}");
            };
            assert_eq!(reason.code(), "exec-root-protected", "{reason:?}");
            tx.commit().unwrap();
        }
    }

    /// TRANSFER, the second call site, through the SAME helper. A head who is
    /// their department's last member can be moved out, and the emptied
    /// department goes with them.
    #[test]
    fn transferring_a_last_member_head_dissolves_the_department_they_leave() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = transfer_person(
            &tx,
            "acme",
            "cos",
            "eng",
            "person-transfer:cos",
            "2026-08-13T00:00:00.000Z",
            "chief",
            Some(&HeadVacancy::Dissolve),
        )
        .unwrap();
        assert_eq!(out, TransferOutcome::Applied { moved: vec!["cos".into()] });
        assert_eq!(head_of(&tx, "office-of-the-ceo"), None);
        let (_employment, placement) = placement_of(&tx, "cos").expect("cos");
        assert_eq!(placement, "eng");
        assert_eq!(
            organization_rows::person_kind(&tx, "acme", "cos").unwrap().as_deref(),
            Some("worker"),
            "somebody who heads nothing is a worker"
        );
        assert_manifest_valid(&tx);
        tx.commit().unwrap();
    }

    /// **THE LIVE 500.** A dissolve of a department whose only other residents
    /// are DEPARTED succeeds, and those residents land in the parent.
    ///
    /// Measured on a live box 2026-08-24: a CEO offboarded two
    /// engineers and then transferred the head out with `vacates: dissolve`.
    /// `check_head_vacancy` passed — `unit_members_other_than` excludes
    /// `Departed` deliberately, so a dissolve is not blocked by alumni nobody
    /// can act on — and then `delete_department` died on
    /// `FOREIGN KEY constraint failed`, because departed rows are RETAINED for
    /// the rehire rule and keep `people.department_id` pointing at the doomed
    /// department. Validation and the schema disagreed about who counts, and
    /// the disagreement surfaced to the model as "chiefd unavailable", which it
    /// retried four times.
    ///
    /// The destination is the PARENT, mirroring
    /// `remove_department_tree_re_homes_to_the_removed_units_parent_not_the_root`
    /// — the sibling verb solved this first and this is the same rule at a
    /// smaller scale.
    #[test]
    fn dissolving_a_department_re_homes_its_departed_residents_to_the_parent() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        // `quinn` is the only ACTIVE member besides the head; move them out so
        // the dissolve is legal. `gone` is DEPARTED and stays homed in `eng` —
        // invisible to validation, visible to the foreign key.
        transfer_person(
            &tx,
            "acme",
            "quinn",
            "executive",
            "person-transfer:quinn",
            "2026-08-13T00:00:00.000Z",
            "chief",
            None,
        )
        .unwrap();
        assert_eq!(
            placement_of(&tx, "gone").expect("gone is retained").1,
            "eng",
            "precondition: a departed row is still homed in the department being dissolved"
        );

        let out = transfer_person(
            &tx,
            "acme",
            "emery",
            "executive",
            "person-transfer:emery",
            "2026-08-13T00:00:00.000Z",
            "chief",
            Some(&HeadVacancy::Dissolve),
        )
        .unwrap();

        assert_eq!(out, TransferOutcome::Applied { moved: vec!["emery".into()] });
        assert!(
            organization_rows::department_state(&tx, "acme", "eng").unwrap().is_none(),
            "the department dissolved"
        );
        let (employment, placement) = placement_of(&tx, "gone").expect("gone survives");
        assert_eq!(placement, "executive", "the departed resident re-homed to the PARENT");
        assert_eq!(employment, "departed", "and is still departed — only their home moved");
        assert_manifest_valid(&tx);
        tx.commit().unwrap();
    }

    /// The refusal for ACTIVE members is unchanged, and it still never names a
    /// departed person: naming somebody the caller cannot act on is the
    /// confusion that message exists to prevent.
    #[test]
    fn the_still_has_members_refusal_names_active_people_and_never_departed_ones() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = transfer_person(
            &tx,
            "acme",
            "emery",
            "executive",
            "person-transfer:emery",
            "2026-08-13T00:00:00.000Z",
            "chief",
            Some(&HeadVacancy::Dissolve),
        )
        .unwrap();
        let TransferOutcome::Refused { reason: TransferRefusal::HeadVacancy(vacancy) } = &out
        else {
            panic!("expected a vacancy refusal, got {out:?}");
        };
        assert!(vacancy.detail().contains("quinn"), "{}", vacancy.detail());
        assert!(
            !vacancy.detail().contains("gone"),
            "a departed resident must not be named as a blocker: {}",
            vacancy.detail()
        );
        tx.commit().unwrap();
    }

    /// A transfer with no decision refuses with the SAME refusal create gives —
    /// naming the department and its successors — rather than the old bare
    /// `head-needs-successor`, which a one-member department could never satisfy.
    #[test]
    fn transferring_a_head_with_no_decision_refuses_with_the_shared_refusal() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = transfer_person(
            &tx,
            "acme",
            "cos",
            "eng",
            "person-transfer:cos",
            "2026-08-13T00:00:00.000Z",
            "chief",
            None,
        )
        .unwrap();
        let TransferOutcome::Refused { reason: TransferRefusal::HeadVacancy(vacancy) } = &out
        else {
            panic!("expected a vacancy refusal, got {out:?}");
        };
        assert_eq!(vacancy.code(), "vacancy-decision-required");
        assert!(vacancy.detail().contains("office-of-the-ceo"), "{}", vacancy.detail());
        assert_eq!(head_of(&tx, "office-of-the-ceo").as_deref(), Some("cos"), "zero writes");
        tx.commit().unwrap();
    }

    /// The batch verb never moves a head, and its refusal names the verb that
    /// does. A way through, not a dead end.
    #[test]
    fn the_batch_move_still_refuses_a_head_and_names_the_single_transfer() {
        let mut conn = open_vacancy();
        let tx = conn.transaction().unwrap();
        let out = move_department_members(
            &tx,
            "acme",
            "eng",
            "office-of-the-ceo",
            &["emery".to_string()],
            "reorg",
            "2026-08-13T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        let TransferOutcome::Refused { reason } = &out else {
            panic!("a head named in a batch must be refused: {out:?}");
        };
        assert_eq!(reason.code(), "head-needs-successor");
        assert!(reason.detail().contains("single transfer"), "{}", reason.detail());
        tx.commit().unwrap();
    }

    #[test]
    fn appoint_head_flips_kinds_keeps_bash_and_repoints_the_department() {
        // THE H2 HEADLINE (the operator's broken reorg): kinds flipped, bash KEPT,
        // head re-pointed, and the staffing ledger records both halves.
        let mut conn = open_h2();
        let tx = conn.transaction().unwrap();
        let out =
            appoint_department_head(&tx, "acme", "eng", "quinn", None, "t1", "operator").unwrap();
        assert_eq!(out, AppointOutcome::Applied);

        // Head re-pointed; kinds flipped; bash KEPT (inv-34 removed).
        let head: String = tx
            .query_row("SELECT head_person_id FROM departments WHERE id='eng'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(head, "quinn");
        let qkind: String =
            tx.query_row("SELECT kind FROM people WHERE id='quinn'", [], |r| r.get(0)).unwrap();
        let ekind: String =
            tx.query_row("SELECT kind FROM people WHERE id='emery'", [], |r| r.get(0)).unwrap();
        assert_eq!(qkind, "head");
        assert_eq!(ekind, "worker");
        // Was `assert_eq!(has_bash, 0)` — appointment used to DELETE the new
        // head's `bash` row (invariant 34). The operator removed that rule on
        // 2026-08-10 ("every agent should have a bash"), so the contract is
        // inverted: promotion must leave the shell alone.
        let has_bash: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM person_tools WHERE person_id='quinn' AND tool='bash'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_bash, 1, "appointment must not strip a new head's bash");

        // Staffing ledger: appointed-head + stepped-down.
        let actions: Vec<(String, String)> = tx
            .prepare("SELECT person_id, action FROM staffing_history ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            actions,
            vec![
                ("quinn".to_string(), "appointed-head".to_string()),
                ("emery".to_string(), "stepped-down".to_string()),
            ]
        );

        // Feed: one event per changed ENTITY — dept + 2 people (deduped).
        let entities: Vec<(String, String)> = tx
            .prepare("SELECT entity, entity_id FROM org_events WHERE slug='acme' ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            entities.iter().filter(|(e, id)| e == "person" && id == "quinn").count(),
            1,
            "one deduped person event for quinn"
        );
        assert!(entities.contains(&("department".to_string(), "eng".to_string())));
        tx.commit().unwrap();
    }

    #[test]
    fn appoint_head_r4_demotes_outgoing_to_the_replacers_department() {
        // R4: with demote_to, the outgoing head lands in that department.
        let mut conn = open_h2();
        let tx = conn.transaction().unwrap();
        appoint_department_head(&tx, "acme", "eng", "quinn", Some("executive"), "t1", "operator")
            .unwrap();
        let placement: String = tx
            .query_row("SELECT department_id FROM people WHERE id='emery'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(placement, "executive");
        // The R4 move is TWO audit facts, like every other `move_person` caller:
        // stepped-down out of the led department, then `transferred` into the
        // replacer's home — the move must be visible in the action vocabulary,
        // not folded silently into the stepped-down entry.
        let entries: Vec<(String, String, Option<String>, Option<String>)> = tx
            .prepare("SELECT person_id, action, from_department_id, to_department_id FROM staffing_history WHERE person_id='emery' ORDER BY seq")
            .unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(
            entries,
            vec![
                (
                    "emery".to_string(),
                    "stepped-down".to_string(),
                    Some("eng".to_string()),
                    Some("executive".to_string())
                ),
                (
                    "emery".to_string(),
                    "transferred".to_string(),
                    Some("eng".to_string()),
                    Some("executive".to_string())
                ),
            ]
        );
        tx.commit().unwrap();
    }

    #[test]
    fn replace_head_and_offboard_supersedes_a_stranded_park_before_minting_its_handoff() {
        // R7: this is the exact valid state that Unit23 supplies — an
        // intent-bound park remains open while the operator appoints a
        // successor and offboards the old head. The one-open-transition index
        // requires the old row to close before the fresh offboard row exists.
        let mut conn = open_h2();
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, intent_id, reason, requested_at, handoff_deadline_at, placement_department_id) VALUES('acme','transition:7:emery:park','emery','park','awaiting_handoff','unit-stop:eng:direct','Record a handoff before park.','t0','t9','eng')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO person_activity(slug, person_id, last_desired_active, active_transition_id, updated_at) \
             VALUES('acme','emery',0,'transition:7:emery:park','t0')",
            [],
        )
        .unwrap();

        let tx = conn.transaction().unwrap();
        let out = replace_head_and_offboard(
            &tx,
            "acme",
            "emery",
            "quinn",
            "2026-08-02T00:00:00.000Z",
            "operator",
        )
        .unwrap();
        assert_eq!(out, DirectOutcome::Applied);

        let (old_status, old_reason): (String, String) = tx
            .query_row(
                "SELECT status, reason FROM transitions WHERE slug='acme' AND id='transition:7:emery:park'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(old_status, "cancelled");
        assert_eq!(old_reason, "superseded-by-offboard:emery");

        let (handoff_id, action, status): (String, String, String) = tx
            .query_row(
                "SELECT id, action, status FROM transitions \
                 WHERE slug='acme' AND person_id='emery' AND id <> 'transition:7:emery:park'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(action, "offboard");
        assert_eq!(status, "awaiting_handoff", "the newly inserted handoff must not self-cancel");
        let pointer: String = tx
            .query_row(
                "SELECT active_transition_id FROM person_activity WHERE slug='acme' AND person_id='emery'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pointer, handoff_id, "desired-off points at the live replacement handoff");
        let open: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM transitions WHERE slug='acme' AND person_id='emery' AND status IN ('awaiting_handoff','overdue','ready')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(open, 1, "one-open-transition invariant survives the atomic reorg");

        let department_head: String = tx
            .query_row(
                "SELECT head_person_id FROM departments WHERE slug='acme' AND id='eng'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let employment: String = tx
            .query_row(
                "SELECT employment_state FROM people WHERE slug='acme' AND id='emery'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(department_head, "quinn", "the successor is appointed atomically");
        assert_eq!(employment, "departed", "the outgoing head is offboarded atomically");
        tx.commit().unwrap();
    }

    #[test]
    fn appoint_head_refusals() {
        let mut conn = open_h2();
        let tx = conn.transaction().unwrap();
        let refuse = |dept: &str, who: &str| match appoint_department_head(
            &tx, "acme", dept, who, None, "t1", "op",
        )
        .unwrap()
        {
            AppointOutcome::Refused { reason } => reason.code().to_string(),
            other => panic!("expected refusal, got {other:?}"),
        };
        // ceo is a worker? no — 'chief' heads executive (root) → root-head-not-reassignable.
        assert_eq!(refuse("executive", "chief"), "root-head-not-reassignable");
        // unknown dept.
        assert_eq!(refuse("nope", "quinn"), "unknown-department");
        // ceo is not a member of eng.
        assert_eq!(refuse("eng", "chief"), "not-a-member");
        tx.commit().unwrap();
    }

    /// Production-bug restoration (U6 casualty table row #8): re-appointing
    /// the SITTING head used to refuse; the atomic-reorg migration's
    /// `person_heads_department_other_than` deliberately excepts the target
    /// department (correct for the `already-heads-elsewhere` refusal below,
    /// which must NOT fire for a person heading the SAME department) — but
    /// that exception left the same-department re-appointment with no check
    /// AT ALL, silently passing.
    #[test]
    fn appoint_head_refuses_reappointing_the_sitting_head() {
        let mut conn = open_h2();
        let tx = conn.transaction().unwrap();
        match appoint_department_head(&tx, "acme", "eng", "emery", None, "t1", "operator").unwrap()
        {
            AppointOutcome::Refused { reason } => assert_eq!(reason.code(), "already-sitting-head"),
            other => panic!("expected already-sitting-head, got {other:?}"),
        }
        // Zero writes: emery is still eng's head, still kind='head'.
        let head: String = tx
            .query_row("SELECT head_person_id FROM departments WHERE id='eng'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(head, "emery");
        let kind: String =
            tx.query_row("SELECT kind FROM people WHERE id='emery'", [], |r| r.get(0)).unwrap();
        assert_eq!(kind, "head");
        tx.commit().unwrap();
    }
    // ----- reparent_department (P1-d) -------------------------------------

    /// A richer org for the reorg tests, seeded WITH the ordinal/created_at/
    /// updated_at columns so the whole-tree preorder recompute is exercised:
    ///   executive (root, ord0, ada=CEO)
    ///     ├─ office-of-the-ceo (ord1, cos)
    ///     ├─ eng (ord2, bo)
    ///     │    └─ eng-platform (ord3, pi)   // a grandchild (cycle-target)
    ///     └─ fund-ops (ord4, fo)            // the reorg subject
    /// `executive` is the company root and is the ONLY protected node here;
    /// `office-of-the-ceo` is an ordinary department (AGENTS.md, 2026-08-13).
    fn open_reorg() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('acme','executive',NULL,'Executive','company','active','ada',0,'t','t'), ('acme','office-of-the-ceo','executive','Office of the CEO','department','active','cos',1,'t','t'), ('acme','eng','executive','Engineering','department','active','bo',2,'t','t'), ('acme','eng-platform','eng','Platform','department','active','pi',3,'t','t'), ('acme','fund-ops','executive','Fund Ops','department','active','fo',4,'t','t');
             INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','ada','Ada','CEO','lead','executive','active','executive',0,'t','t'), ('acme','cos','Cos','Chief of Staff','support','head','active','office-of-the-ceo',1,'t','t'), ('acme','bo','Bo','Head','lead','head','active','eng',2,'t','t'), ('acme','pi','Pi','Head','lead','head','active','eng-platform',3,'t','t'), ('acme','fo','Fo','Head','lead','head','active','fund-ops',4,'t','t');
             INSERT INTO org_settings(
                 slug, display_slug, supervision_interval_ms, acknowledgement_timeout_ms,
                 acknowledgement_retry_limit, replacement_limit
             ) VALUES ('acme', 'acme', 60000, 300000, 3, 3);",
        )
        .expect("seed");
        conn
    }

    fn dept_rows(conn: &Connection) -> Vec<(String, Option<String>, i64)> {
        conn.prepare(
            "SELECT id, parent_id, ordinal FROM departments WHERE slug='acme' ORDER BY ordinal",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    /// Ordinals form a gapless 0..n-1 bijection (H1 dept-ordinal guard).
    fn assert_ordinals_bijective(conn: &Connection) {
        let rows = dept_rows(conn);
        let mut ords: Vec<i64> = rows.iter().map(|(_, _, o)| *o).collect();
        ords.sort_unstable();
        let expected: Vec<i64> = (0..rows.len() as i64).collect();
        assert_eq!(ords, expected, "dept ordinals must be a gapless 0..n-1 bijection");
    }

    #[test]
    fn reparent_moves_the_dept_and_keeps_a_valid_bijective_tree() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(
            &tx,
            "acme",
            "fund-ops",
            "eng",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(out, ReparentOutcome::Applied { department_id: "fund-ops".into() });
        // fund-ops now parents under eng.
        let parent: Option<String> = tx
            .query_row(
                "SELECT parent_id FROM departments WHERE slug='acme' AND id='fund-ops'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent.as_deref(), Some("eng"));
        tx.commit().unwrap();
        assert_ordinals_bijective(&conn);
        // Preorder after the move: exec, ooc, eng, eng-platform, fund-ops.
        let order: Vec<String> = dept_rows(&conn).into_iter().map(|(id, ..)| id).collect();
        assert_eq!(
            order,
            vec!["executive", "office-of-the-ceo", "eng", "eng-platform", "fund-ops"],
        );
    }

    #[test]
    fn reparent_refuses_an_unknown_department() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(&tx, "acme", "ghost", "eng", "t", "").unwrap();
        assert_eq!(out, ReparentOutcome::Refused { reason: ReparentRefusal::UnknownDepartment });
        // nothing written.
        let events: i64 = tx
            .query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 0);
        tx.commit().unwrap();
    }

    #[test]
    fn reparent_refuses_an_unknown_new_parent() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(&tx, "acme", "fund-ops", "ghost", "t", "").unwrap();
        assert_eq!(out, ReparentOutcome::Refused { reason: ReparentRefusal::UnknownNewParent });
        tx.commit().unwrap();
    }

    #[test]
    fn reparent_refuses_a_paused_new_parent() {
        let mut conn = open_reorg();
        conn.execute("UPDATE departments SET state='paused' WHERE slug='acme' AND id='eng'", [])
            .unwrap();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(&tx, "acme", "fund-ops", "eng", "t", "").unwrap();
        assert_eq!(out, ReparentOutcome::Refused { reason: ReparentRefusal::NewParentPaused });
        tx.commit().unwrap();
    }

    #[test]
    fn reparent_refuses_an_active_new_parent_below_a_paused_ancestor() {
        let mut conn = open_reorg();
        conn.execute("UPDATE departments SET state='paused' WHERE slug='acme' AND id='eng'", [])
            .unwrap();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(&tx, "acme", "fund-ops", "eng-platform", "t", "").unwrap();
        assert_eq!(out, ReparentOutcome::Refused { reason: ReparentRefusal::NewParentPaused });
        tx.commit().unwrap();
    }

    #[test]
    fn reparent_refuses_the_existing_parent_without_writing_an_audit_event() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(&tx, "acme", "fund-ops", "executive", "t", "").unwrap();
        assert_eq!(out, ReparentOutcome::Refused { reason: ReparentRefusal::AlreadyUnderParent });
        let events: i64 = tx
            .query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 0, "a no-op must not create an audit event");
        tx.commit().unwrap();
    }

    /// The company root's head is the CEO and stays fixed; every OTHER unit in
    /// the CEO's chains may change head like any other department.
    ///
    /// #1063 narrowed create, transfer and reparent to the CEO alone and left
    /// this verb asking the whole executive-root set, so the head of
    /// `office-of-the-ceo` still could not hand over — the same defect the
    /// ruling removed, one verb across. Both halves are asserted.
    #[test]
    fn appoint_head_refuses_the_company_root_and_nothing_else() {
        let mut conn = open_reorg();
        conn.execute(
            "INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','aide','Aide','Aide','support','worker','active','office-of-the-ceo',5,'t','t')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            appoint_department_head(&tx, "acme", "executive", "cos", None, "t", "").unwrap(),
            AppointOutcome::Refused { reason: AppointRefusal::RootHeadNotReassignable },
            "the CEO always heads the company root"
        );
        assert_eq!(
            appoint_department_head(&tx, "acme", "office-of-the-ceo", "aide", None, "t", "")
                .unwrap(),
            AppointOutcome::Applied,
            "office-of-the-ceo is an ordinary department for succession purposes"
        );
        let head: String = tx
            .query_row(
                "SELECT head_person_id FROM departments WHERE slug='acme' \
                 AND id='office-of-the-ceo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(head, "aide", "the hand-over must be durable, not merely accepted");
        tx.commit().unwrap();
    }

    /// NO REFUSAL IN THIS FAMILY TEACHES A PROTECTED REGION.
    ///
    /// The guards were all narrowed to `is_ceo` / `department_is_company_root`
    /// by #1063 and #1067, but four operator-facing `detail()` strings kept
    /// saying "the executive root (CEO / office-of-the-ceo) never …" — the
    /// retired model, spoken by the product to the person using it, long after
    /// the code stopped believing it. An operator who reads that concludes the
    /// chief of staff cannot be fired, benched or stopped, which is false.
    ///
    /// This test pins the RULE rather than the wording: every refusal in the
    /// family must name the ONE exempt person or the ONE fixed department, and
    /// none may describe a protected REGION. Reword them freely; do not
    /// reintroduce a region.
    #[test]
    fn no_refusal_detail_describes_a_protected_region() {
        let details: [(&str, &str); 6] = [
            ("shutdown", ShutdownRefusal::CeoExempt.detail()),
            ("offboard", OffboardRefusal::ExecRootProtected.detail()),
            ("pause", PauseRefusal::ExecRootProtected.detail()),
            ("bench", BenchRefusal::ExecRootProtected.detail()),
            ("reparent", ReparentRefusal::ExecRootProtected.detail()),
            ("appoint-head", AppointRefusal::RootHeadNotReassignable.detail()),
        ];
        for (verb, detail) in details {
            assert!(!detail.contains("executive root"), "{verb} still refuses a REGION: {detail}");
            assert!(
                detail.contains("company root"),
                "{verb} must name the one fixed department: {detail}"
            );
        }
        // The transfer refusal owns a String rather than a &'static str.
        let transfer = TransferRefusal::ExecRootProtected.detail();
        assert!(!transfer.contains("executive root"), "transfer refuses a REGION: {transfer}");
        assert!(transfer.contains("the CEO"), "transfer must name the person: {transfer}");
    }

    #[test]
    fn reparent_refuses_a_self_parent_cycle() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(&tx, "acme", "eng", "eng", "t", "").unwrap();
        assert_eq!(out, ReparentOutcome::Refused { reason: ReparentRefusal::WouldCreateCycle });
        tx.commit().unwrap();
    }

    #[test]
    fn reparent_refuses_reparenting_under_own_descendant() {
        // eng -> eng-platform (eng's own child) is a cycle.
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(&tx, "acme", "eng", "eng-platform", "t", "").unwrap();
        assert_eq!(out, ReparentOutcome::Refused { reason: ReparentRefusal::WouldCreateCycle });
        tx.commit().unwrap();
    }

    /// ONLY the company root is unreparentable, and it is unreparentable
    /// because it has no parent — a structural fact, not a policy.
    ///
    /// `office-of-the-ceo` USED to be refused here too, as part of the
    /// executive-root set. The operator's ruling on 2026-08-13 (`AGENTS.md`,
    /// "THE CEO IS THE ONLY IMMOVABLE NODE") is that any child may move to any
    /// other department, so that half is now an ALLOW and is asserted as one.
    #[test]
    fn reparent_refuses_the_company_root_and_nothing_else() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        // The company root: no parent to move it to, refused for ever.
        let root = reparent_department(&tx, "acme", "executive", "eng", "t", "").unwrap();
        assert_eq!(root, ReparentOutcome::Refused { reason: ReparentRefusal::ExecRootProtected });
        // office-of-the-ceo sits BENEATH the root, so it moves like anything
        // else. This is the behaviour change the ruling demands.
        let office = reparent_department(&tx, "acme", "office-of-the-ceo", "eng", "t", "").unwrap();
        assert_eq!(
            office,
            ReparentOutcome::Applied { department_id: "office-of-the-ceo".into() },
            "a unit near the CEO is still just a unit"
        );
        // ...and it is an ordinary DESTINATION too. The subject half was
        // asserted above; the destination half never was, and a stale comment
        // in `reparent_that_shifts_preorder_touches_every_moved_dept` still
        // told the next reader to route around it as "exec-root" (#1071).
        let into_office =
            reparent_department(&tx, "acme", "fund-ops", "office-of-the-ceo", "t", "").unwrap();
        assert_eq!(
            into_office,
            ReparentOutcome::Applied { department_id: "fund-ops".into() },
            "office-of-the-ceo accepts children like any other department"
        );
        tx.commit().unwrap();
    }

    #[test]
    fn reparent_commits_despite_a_prior_unrelated_event() {
        let mut conn = open_reorg();
        conn.execute(
            "INSERT INTO org_events(slug, seq, entity, entity_id, op, at) VALUES('acme',1,'person','x','noop','t')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO counters(name, value) VALUES('org-events:acme', 1)", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(&tx, "acme", "fund-ops", "eng", "t", "").unwrap();
        assert_eq!(out, ReparentOutcome::Applied { department_id: "fund-ops".into() });
        // An unrelated event cannot make a valid reorganization stale.
        let parent: Option<String> = tx
            .query_row(
                "SELECT parent_id FROM departments WHERE slug='acme' AND id='fund-ops'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent.as_deref(), Some("eng"));
        tx.commit().unwrap();
    }

    #[test]
    fn reparent_emits_one_contiguous_seq_run_of_department_touches() {
        // Moving fund-ops under eng shifts NO other dept's preorder position
        // except fund-ops itself (it was already last), so exactly one dept
        // touch is emitted, contiguous from the fence.
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        reparent_department(&tx, "acme", "fund-ops", "eng", "t", "").unwrap();
        let rows: Vec<(i64, String, String, String)> = tx
            .prepare(
                "SELECT seq, entity, entity_id, op FROM org_events WHERE slug='acme' ORDER BY seq",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(
            rows.iter().any(|(_, entity, id, op)| {
                entity == "department" && id == "fund-ops" && op == "upsert"
            }),
            "the moved department has an append-only audit event"
        );
        assert!(
            !rows
                .iter()
                .any(|(_, entity, id, op)| { entity == "org" && id == "acme" && op == "upsert" }),
            "a direct normalized reparent never emits an organization-wide manifest event"
        );
        let seqs: Vec<i64> = rows.iter().map(|(s, ..)| *s).collect();
        let contiguous: Vec<i64> = (1..=rows.len() as i64).collect();
        assert_eq!(seqs, contiguous, "one contiguous seq run");
        tx.commit().unwrap();
    }

    #[test]
    fn reparent_that_shifts_preorder_touches_every_moved_dept() {
        // Move eng-platform (currently ord3, under eng) to under office-of-the-ceo
        // (ord1). New preorder: exec, ooc, eng-platform, eng, fund-ops — so
        // eng-platform, eng and fund-ops all shift ordinal. Bijection holds.
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        // This test moves under fund-ops (ord4) instead. It once said
        // "office-of-the-ceo is exec-root", which stopped being true on
        // 2026-08-13; the destination is now merely an arbitrary choice, and
        // `reparent_refuses_the_company_root_and_nothing_else` asserts that
        // office-of-the-ceo does accept children.
        // New preorder: exec, ooc, eng, fund-ops, fund-ops/eng-platform.
        let out = reparent_department(&tx, "acme", "eng-platform", "fund-ops", "t", "").unwrap();
        assert_eq!(out, ReparentOutcome::Applied { department_id: "eng-platform".into() });
        tx.commit().unwrap();
        assert_ordinals_bijective(&conn);
        let order: Vec<String> = dept_rows(&conn).into_iter().map(|(id, ..)| id).collect();
        assert_eq!(
            order,
            vec!["executive", "office-of-the-ceo", "eng", "fund-ops", "eng-platform"],
        );
    }

    // -- scope on the reorg keystone (B1) ---------------------------------
    //
    // `reparent_department` took an actor and asked nothing of it. Every test
    // above passes an actor that names no person row ("", "chief", "under
    // carlos"), which is exactly the unjudged case, so none of them would go
    // red if the guard below were deleted. These four are the ones that would.

    /// The POSITIVE case, and the one that keeps the refusals honest: a guard
    /// that refused everybody would satisfy both negatives on its own.
    ///
    /// `ada` heads `executive`, the root, so it manages every department and
    /// an ordinary whole-company reorg is unaffected.
    #[test]
    fn the_ceo_may_reparent_anything_because_it_manages_the_whole_company() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(&tx, "acme", "fund-ops", "eng", "t", "ada").unwrap();
        assert_eq!(out, ReparentOutcome::Applied { department_id: "fund-ops".into() });
        tx.commit().unwrap();
    }

    /// Half one: you cannot move a unit OUT of somebody else's subtree. `fo`
    /// heads `fund-ops` and nothing else, so `eng` — a sibling — is not its to
    /// take.
    #[test]
    fn a_head_may_not_reparent_a_department_outside_its_own_subtree() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        let out = reparent_department(&tx, "acme", "eng", "fund-ops", "t", "fo").unwrap();
        assert_eq!(out, ReparentOutcome::Refused { reason: ReparentRefusal::ActorOutOfScope });
        // ZERO WRITES: eng still hangs off executive.
        let parent: Option<String> = tx
            .query_row(
                "SELECT parent_id FROM departments WHERE slug='acme' AND id='eng'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent.as_deref(), Some("executive"));
        tx.commit().unwrap();
    }

    /// THE ESCALATION, and the reason this verb is not a copy of remove-tree.
    ///
    /// `bo` heads `eng` and therefore manages `eng-platform` beneath it — so a
    /// guard that asked only about the MOVED unit would let this through. It
    /// hangs Platform, every person in it and their whole reporting line under
    /// `fund-ops`, a department `bo` has no authority over: a unilateral grant
    /// of somebody else's authority that reads as an ordinary move in the
    /// audit trail.
    #[test]
    fn a_head_may_not_graft_its_own_subtree_under_a_parent_it_does_not_manage() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        // The moved unit IS in bo's scope — this is refused on the destination.
        assert!(
            organization_rows::person_manages_department(&tx, "acme", "bo", "eng-platform")
                .unwrap(),
            "fixture check: bo must manage the unit being moved, or this proves nothing"
        );
        let out = reparent_department(&tx, "acme", "eng-platform", "fund-ops", "t", "bo").unwrap();
        assert_eq!(out, ReparentOutcome::Refused { reason: ReparentRefusal::NewParentOutOfScope });
        // ZERO WRITES: platform still hangs off eng.
        let parent: Option<String> = tx
            .query_row(
                "SELECT parent_id FROM departments WHERE slug='acme' AND id='eng-platform'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent.as_deref(), Some("eng"));
        tx.commit().unwrap();
    }

    /// THE ACTOR RULE. `operator` names no person row, so it is not judged.
    /// The actor is free-form audit prose in this corpus and fires as
    /// `operator`, as `op` and as the empty string; gating on the string's
    /// CONTENT would need a placeholder allowlist that rots on the first new
    /// spelling.
    #[test]
    fn an_actor_that_names_no_person_is_not_judged() {
        let mut conn = open_reorg();
        let tx = conn.transaction().unwrap();
        let out =
            reparent_department(&tx, "acme", "eng-platform", "fund-ops", "t", "operator").unwrap();
        assert_eq!(out, ReparentOutcome::Applied { department_id: "eng-platform".into() });
        tx.commit().unwrap();
    }
}

/// A refusal that names only the field teaches a caller to stop sending the
/// field. These lock the enumerated vocabulary into the refusal text itself.
#[cfg(test)]
mod seed_refusal_vocabulary_tests {
    use super::*;

    fn seed<'a>(kind: PersonKind, tools: &'a [String]) -> NewPersonSeed<'a> {
        NewPersonSeed {
            name: "Pat",
            title: "Head of Platform",
            mandate: "lead platform",
            kind,
            employment_state: EmploymentState::Active,
            activation: "resident",
            tools,
            prompts: &[],
        }
    }

    /// Was `bash_on_a_head_seed_is_refused_with_the_rule_and_the_declarable_names`,
    /// which asserted the refusal that started this whole packet. The operator
    /// removed invariant 34 on 2026-08-10 ("Everybody should have bash"), so
    /// the value that was refused is now simply accepted — on EVERY kind.
    /// Inverted rather than deleted so a re-narrowing has to fail something.
    #[test]
    fn bash_is_accepted_on_every_kind_of_seed() {
        let tools = vec!["bash".to_string()];
        for kind in [PersonKind::Head, PersonKind::Worker, PersonKind::Executive] {
            assert!(
                validate_new_person_seed(&seed(kind, &tools), kind).is_ok(),
                "bash must be declarable on {kind:?}"
            );
        }
    }

    /// The refusal machinery survived both decisions: a `tools` rejection still
    /// enumerates the builtins and still explains where anything else must come
    /// from. The bash clause went with invariant 34; the omission warning went
    /// when the floor became unconditional and made it false.
    #[test]
    fn a_tools_refusal_still_enumerates_the_builtins_and_the_resource_rule() {
        let tools = vec!["bahs".to_string()];
        let rejection = validate_new_person_seed(&seed(PersonKind::Head, &tools), PersonKind::Head)
            .expect_err("a mistyped name is still refused");
        assert_eq!(rejection.field, "tools");
        assert!(
            rejection.detail.contains("read, bash, edit, write, grep, find, ls"),
            "the builtin list is still shown: {}",
            rejection.detail
        );
        assert!(
            rejection.detail.contains("granted to every person automatically"),
            "the refusal must say the builtins are automatic: {}",
            rejection.detail
        );
        assert!(
            !rejection.detail.contains("grants NO builtin"),
            "the old omission warning is false now that the floor is unconditional: {}",
            rejection.detail
        );
        assert!(
            !rejection.detail.contains("WORKER only"),
            "the worker-only clause must be gone: {}",
            rejection.detail
        );
    }

    #[test]
    fn a_worker_tools_refusal_lists_bash_among_the_declarable_names() {
        let tools = vec![String::new()];
        let rejection =
            validate_new_person_seed(&seed(PersonKind::Worker, &tools), PersonKind::Worker)
                .expect_err("a blank tool name is invalid");
        assert_eq!(rejection.field, "tools");
        assert!(
            rejection.detail.contains("read, bash, edit, write, grep, find, ls"),
            "the builtin list is shown: {}",
            rejection.detail
        );
    }

    /// The silent half of the defect. `["bahs"]` used to be stored verbatim
    /// into `person_tools`, grant nothing, and say nothing.
    #[test]
    fn a_mistyped_tool_name_is_refused_and_names_the_declarable_set() {
        let tools = vec!["bahs".to_string()];
        let rejection =
            validate_new_person_seed(&seed(PersonKind::Worker, &tools), PersonKind::Worker)
                .expect_err("a mistyped tool name must be refused, not stored");
        assert_eq!(rejection.field, "tools");
        assert!(
            rejection.detail.contains("'bahs' is not a declarable tool name"),
            "{}",
            rejection.detail
        );
        assert!(
            rejection.detail.contains("read, bash, edit, write, grep, find, ls"),
            "the refusal must name the valid ids: {}",
            rejection.detail
        );
    }

    /// The declarable set is deliberately NARROWER than the set a person ends
    /// up holding: the org_* surface is composed in from the person's kind and
    /// is never declared. Declaring one on a worker would otherwise be written
    /// straight into `person_tools` as a grant it is not entitled to.
    #[test]
    fn a_composed_org_tool_may_not_be_declared() {
        let tools = vec!["org_send".to_string()];
        let rejection =
            validate_new_person_seed(&seed(PersonKind::Worker, &tools), PersonKind::Worker)
                .expect_err("a composed manager tool is not declarable");
        assert_eq!(rejection.field, "tools");
        assert!(
            rejection.detail.contains("must NEVER be declared"),
            "the refusal must teach the declared/composed split: {}",
            rejection.detail
        );
    }

    // TOMBSTONE (chief-home-is-cwd §4e):
    //   `an_unrecognized_name_is_deferred_when_the_seed_selects_a_resource`
    //   pinned the escape hatch — a seed that selected an extension could
    //   declare the tool that extension exports, because chiefd resolved no
    //   catalog at seed time and materialization could. A seed selects no
    //   extension and no package now, so no seed can export a tool name and
    //   there is nothing left to defer: every name outside the builtins is a
    //   typo, which `a_mistyped_tool_name_...` above still refuses.

    #[test]
    fn every_builtin_name_is_declarable_by_a_worker() {
        let tools: Vec<String> =
            organization_spec::BUILTIN_TOOLS.iter().map(|t| (*t).to_string()).collect();
        assert!(
            validate_new_person_seed(&seed(PersonKind::Worker, &tools), PersonKind::Worker).is_ok()
        );
    }

    #[test]
    fn a_rejection_re_roots_under_its_seed_path_without_losing_the_detail() {
        let rejection = SeedRejection::new("tools", "why").under("staff[2].");
        assert_eq!(
            (rejection.field.as_str(), rejection.detail.as_str()),
            ("staff[2].tools", "why")
        );
    }
}

#[cfg(test)]
mod create_department_tests {
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;
    use rusqlite::Connection;

    fn new_head_seed(name: &str, title: &str, mandate: &str) -> OwnedNewPersonSeed {
        OwnedNewPersonSeed {
            name: name.into(),
            title: title.into(),
            mandate: mandate.into(),
            kind: PersonKind::Head,
            employment_state: EmploymentState::Active,
            activation: "resident".into(),
            tools: vec![],
            prompts: vec![],
        }
    }

    fn new_staff_seed(person_id: &str, name: &str) -> DepartmentStaffSeed {
        let mut seed = new_head_seed(name, name, "own assigned work");
        seed.kind = PersonKind::Worker;
        seed.employment_state = EmploymentState::Benched;
        DepartmentStaffSeed { person_id: person_id.into(), seed }
    }

    /// Same exec-root shape as the shutdown tests, PLUS a non-head worker
    /// (`nita`, a plain eng member) to promote as a newly-created department's
    /// head — appointing a person who already heads a department would violate
    /// the `departments_one_head` unique index (that conflict is P1-b's
    /// `already-heads-elsewhere`, not create's concern). FKs OFF.
    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO org_settings(slug, display_slug, supervision_interval_ms, acknowledgement_timeout_ms, acknowledgement_retry_limit, replacement_limit) VALUES('acme', 'acme',60000,30000,3,3); INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('acme','executive',NULL,'Executive','company','active','ada',0,'t','t'), ('acme','office-of-the-ceo','executive','Office of the CEO','department','active','cos',1,'t','t'), ('acme','eng','executive','Engineering','department','active','bo',2,'t','t'); INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','ada','Ada','CEO','lead','executive','active','executive',0,'t','t'), ('acme','cos','Cos','Chief of Staff','support','head','active','office-of-the-ceo',1,'t','t'), ('acme','bo','Bo','Engineer','build','worker','active','eng',2,'t','t'), ('acme','nita','Nita','Engineer','build','worker','active','eng',3,'t','t');",
        )
        .expect("seed");
        conn
    }

    fn dept_ordinals(conn: &Connection) -> Vec<i64> {
        let mut stmt = conn
            .prepare("SELECT ordinal FROM departments WHERE slug='acme' ORDER BY ordinal")
            .unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    fn people_ordinals(conn: &Connection) -> Vec<i64> {
        let mut stmt =
            conn.prepare("SELECT ordinal FROM people WHERE slug='acme' ORDER BY ordinal").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    /// A gapless 0..N bijection guard (H1): the ordinals are exactly 0,1,..,N-1.
    fn is_gapless_bijection(ordinals: &[i64]) -> bool {
        ordinals.iter().enumerate().all(|(i, o)| *o == i as i64)
    }

    /// The whole reported incident, at the surface the operator's CEO actually
    /// read — now with the opposite outcome. `tools: ["bash"]` on a head was
    /// what got refused; after the operator removed invariant 34 (2026-08-10)
    /// it CREATES the department, and the head keeps the shell durably.
    #[test]
    fn a_head_seeded_with_bash_is_now_created_and_keeps_it() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut head = new_head_seed("Pat", "Head of Platform", "lead platform");
        head.tools = vec!["read".to_string(), "bash".to_string()];
        let out = create_department(
            &tx,
            "acme",
            "platform",
            "executive",
            "Platform",
            "Own platform",
            &HeadDecision::HireNew { person_id: "platform-head".into(), seed: Box::new(head) },
            "ada",
            "Create Platform.",
            "2026-07-25T00:00:00.000Z",
            "ada",
        )
        .unwrap();
        assert_eq!(
            out,
            CreateDepartmentOutcome::Applied { department_id: "platform".to_string() },
            "a head seeded with bash must now be created"
        );
        // Durable, not merely accepted by the validator: the row writer used to
        // skip a non-worker's `bash` on insert, which would have made this look
        // fixed while the head still came up without a shell.
        let stored: Vec<String> = tx
            .prepare(
                "SELECT tool FROM person_tools WHERE slug='acme' AND person_id='platform-head' \
                 ORDER BY ordinal",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(stored, vec!["read", "bash"], "the head's bash must reach person_tools");
        tx.commit().unwrap();
    }

    /// The refusal machinery this packet added is unaffected by the bash
    /// decision: a mistyped name on a head still refuses, still enumerates.
    #[test]
    fn the_head_tools_refusal_detail_still_carries_the_vocabulary() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut head = new_head_seed("Pat", "Head of Platform", "lead platform");
        head.tools = vec!["bahs".to_string()];
        let out = create_department(
            &tx,
            "acme",
            "platform",
            "executive",
            "Platform",
            "Own platform",
            &HeadDecision::HireNew { person_id: "platform-head".into(), seed: Box::new(head) },
            "ada",
            "Create Platform.",
            "2026-07-25T00:00:00.000Z",
            "ada",
        )
        .unwrap();
        let CreateDepartmentOutcome::Refused { reason } = out else {
            panic!("a mistyped tool name must be refused, got {out:?}");
        };
        assert_eq!(reason.code(), "invalid-seed");
        let detail = reason.detail();
        assert!(detail.starts_with("invalid department person seed: head.tools"), "{detail}");
        assert!(detail.contains("'bahs' is not a declarable tool name"), "{detail}");
        assert!(detail.contains("read, bash, edit, write, grep, find, ls"), "{detail}");
        assert!(detail.contains("granted to every person automatically"), "{detail}");
        let rows: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='platform'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
        tx.commit().unwrap();
    }

    #[test]
    fn create_with_existing_head_writes_dept_repoints_head_and_keeps_ordinals_bijective() {
        // THE P1-a acceptance: create with an existing-person head → one txn,
        // dept row + head re-pointer + ordinals bijective; NO launch-intent row.
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = create_department(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "nita".into() },
            "ada",
            "Create Product with Nita as head.",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(out, CreateDepartmentOutcome::Applied { department_id: "product".into() });

        // Department row exists, parented under executive, headed by bo, at the
        // append-ordinal (3).
        let (parent, head, ordinal, kind, state): (String, String, i64, String, String) = tx
            .query_row(
                "SELECT parent_id, head_person_id, ordinal, kind, state FROM departments \
                 WHERE slug='acme' AND id='product'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(parent, "executive");
        assert_eq!(head, "nita");
        assert_eq!(ordinal, 3);
        assert_eq!(kind, "department");
        assert_eq!(state, "active");

        // Head re-pointer: nita now sits in the new department, and nita is
        // promoted worker→head.
        let (placement, pkind): (String, String) = tx
            .query_row(
                "SELECT department_id, kind FROM people WHERE slug='acme' AND id='nita'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(placement, "product");
        assert_eq!(pkind, "head");

        // Preserve the legacy transfer + appointment audit pair, including the
        // caller's rationale (staffing history has its own D2 feed).
        let history: Vec<(String, Option<String>, String, String)> = tx
            .prepare(
                "SELECT action, from_department_id, to_department_id, reason \
                 FROM staffing_history WHERE slug='acme' AND person_id='nita' ORDER BY seq",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            history,
            vec![
                (
                    "transferred".into(),
                    Some("eng".into()),
                    "product".into(),
                    "Create Product with Nita as head.".into(),
                ),
                (
                    "appointed-head".into(),
                    None,
                    "product".into(),
                    "Create Product with Nita as head.".into(),
                ),
            ],
        );

        // Ordinals stay a gapless bijection (H1) — depts 0..3, people unchanged 0..2.
        assert!(is_gapless_bijection(&dept_ordinals(&tx)));
        assert!(is_gapless_bijection(&people_ordinals(&tx)));

        // BIRTH STATE (operator, 2026-08-10): the head of a department that has
        // just been launched comes UP. Asserted BY VALUE -- exactly one durable
        // `launch_intent` row, naming Nita and nobody else -- because a count
        // alone would still pass if the transaction fenced the wrong person.
        //
        // That row is a FENCE, not a one-shot start signal: `project_activity_fence`
        // re-derives `Requested` demand from it on EVERY converge pass while the
        // person is not yet desired-active, so a dropped signal cannot strand her
        // stopped. Still NO pane inside the transaction, and appointing an
        // EXISTING person seeds no fresh `person_activity` row.
        let fenced: Vec<String> = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(fenced, vec!["nita".to_string()]);
        let pa: i64 = tx
            .query_row("SELECT COUNT(*) FROM person_activity WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pa, 0);
        tx.commit().unwrap();
    }

    #[test]
    fn create_with_hire_new_inserts_head_at_people_append_ordinal() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = create_department(
            &tx,
            "acme",
            "finance",
            "executive",
            "Finance",
            "Money",
            &HeadDecision::HireNew {
                person_id: "fin-lead".into(),
                seed: Box::new(new_head_seed("Fin", "Head of Finance", "own finance")),
            },
            "ada",
            "Create Finance with a new head.",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert!(matches!(out, CreateDepartmentOutcome::Applied { .. }));
        let (placement, ordinal, employment): (String, i64, String) = tx
            .query_row(
                "SELECT department_id, ordinal, employment_state \
                 FROM people WHERE slug='acme' AND id='fin-lead'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(placement, "finance");
        assert_eq!(ordinal, 4); // appended after ada/cos/bo/nita (0,1,2,3)
        assert_eq!(employment, "active");
        let activity: (String, String, i64, i64) = tx
            .query_row(
                "SELECT last_employment_state, last_department_id, last_operational, \
                 last_desired_active FROM person_activity \
                 WHERE slug='acme' AND person_id='fin-lead'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        // #751-P9: the seeded row records the head's OWN units. It used to
        // carry a fourth column, `last_pane_department_id` = "executive" (the
        // parent's window), which is a display answer and is no longer stored.
        //
        // The trailing 0 is `last_desired_active` and it MUST stay 0 here. That
        // is not the old "creation starts nobody" rule surviving by accident:
        // `activity::reconcile` recomputes `last_desired_active` from demand
        // reasons on every pass, and `project_activity_fence` suppresses the
        // `Requested` reason for anyone ALREADY desired-active. A transaction
        // that pre-set the flag would therefore erase the very demand that
        // brings the new head up.
        assert_eq!(activity, ("active".into(), "finance".into(), 1, 0));
        assert!(is_gapless_bijection(&dept_ordinals(&tx)));
        assert!(is_gapless_bijection(&people_ordinals(&tx)));
        // BIRTH STATE: a hired head comes up, so the same transaction records
        // the launch fence -- by value, exactly the head and nobody else.
        let fenced: Vec<String> = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(fenced, vec!["fin-lead".to_string()]);
        tx.commit().unwrap();
    }

    #[test]
    fn create_under_non_last_subtree_normalizes_both_manifest_orders() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = create_department(
            &tx,
            "acme",
            "ceo-strategy",
            "office-of-the-ceo",
            "CEO Strategy",
            "Own CEO strategy",
            &HeadDecision::AppointExisting { person_id: "nita".into() },
            "ada",
            "Create CEO Strategy with Nita as head.",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert!(matches!(out, CreateDepartmentOutcome::Applied { .. }));

        let departments: Vec<String> = tx
            .prepare("SELECT id FROM departments WHERE slug='acme' ORDER BY ordinal")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            departments,
            vec!["executive", "office-of-the-ceo", "ceo-strategy", "eng"],
            "the new child belongs immediately after its non-last parent subtree",
        );
        let people: Vec<String> = tx
            .prepare("SELECT id FROM people WHERE slug='acme' ORDER BY ordinal")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            people,
            vec!["ada", "cos", "nita", "bo"],
            "people remain grouped by department with every head first",
        );
        tx.commit().unwrap();
    }

    #[test]
    fn create_commits_complete_initial_staff_and_their_launch_fences_in_one_transaction() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let staff = vec![
            new_staff_seed("platform-developer", "Developer"),
            new_staff_seed("platform-reviewer", "Reviewer"),
        ];
        let out = create_department_with_staff(
            &tx,
            "acme",
            "platform",
            "executive",
            "Platform",
            "Own platform",
            &HeadDecision::HireNew {
                person_id: "platform-head".into(),
                seed: Box::new(new_head_seed("Pat", "Head of Platform", "lead platform")),
            },
            &staff,
            Some("ada"),
            "Create Platform and its initial roster.",
            "2026-07-25T00:00:00.000Z",
            "ada",
        )
        .unwrap();
        assert!(matches!(out, CreateDepartmentOutcome::Applied { .. }));

        let people: Vec<String> = tx
            .prepare(
                "SELECT id FROM people WHERE slug='acme' AND department_id='platform' ORDER BY ordinal",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(people, vec!["platform-head", "platform-developer", "platform-reviewer"],);
        let desired_off: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM person_activity WHERE slug='acme' \
                 AND person_id IN ('platform-head','platform-developer','platform-reviewer') \
                 AND last_desired_active=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // Still 3, and deliberately so: the writer transaction never pre-sets
        // `last_desired_active`, because `activity::reconcile` owns that field
        // and derives it from demand on every pass. The fence below is what
        // supplies the demand -- see the launch_intent assertion at the end.
        assert_eq!(desired_off, 3);
        let history: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM staffing_history WHERE slug='acme' \
                 AND person_id IN ('platform-head','platform-developer','platform-reviewer') \
                 AND action='hired'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history, 3);
        // BIRTH STATE, and the exact line the rule draws. These two staff seeds
        // are BENCHED (`new_staff_seed`), and a benched seed is durable and
        // stopped by its own definition -- fencing it would start somebody the
        // caller explicitly said is not staffed. So the head, who IS active,
        // comes up, and neither worker does.
        //
        // Read the whole fence set by value rather than counting the three ids
        // the query above already names: a count filtered by the expected ids
        // cannot see a fence written for somebody else, which is the failure
        // that matters when a rule says "start exactly these people".
        // `create_launches_an_active_initial_staff_seed_with_the_department`
        // below is the same assertion with the seeds active.
        let fenced: Vec<String> = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(fenced, vec!["platform-head".to_string()]);
        tx.commit().unwrap();
    }

    /// The active half of the same rule: an initial staff seed the caller
    /// declared ACTIVE is part of the roster the department was launched with,
    /// so it comes up with the department. Without this the only staff coverage
    /// would be the benched case above, where the correct answer and the old
    /// "creation starts nobody" answer happen to be identical.
    #[test]
    fn create_launches_an_active_initial_staff_seed_with_the_department() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut developer = new_staff_seed("platform-developer", "Developer");
        developer.seed.employment_state = EmploymentState::Active;
        let staff = vec![developer, new_staff_seed("platform-reviewer", "Reviewer")];
        let out = create_department_with_staff(
            &tx,
            "acme",
            "platform",
            "executive",
            "Platform",
            "Own platform",
            &HeadDecision::HireNew {
                person_id: "platform-head".into(),
                seed: Box::new(new_head_seed("Pat", "Head of Platform", "lead platform")),
            },
            &staff,
            Some("ada"),
            "Create Platform and its initial roster.",
            "2026-07-25T00:00:00.000Z",
            "ada",
        )
        .unwrap();
        assert!(matches!(out, CreateDepartmentOutcome::Applied { .. }));
        let fenced: Vec<String> = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            fenced,
            vec!["platform-developer".to_string(), "platform-head".to_string()],
            "the active head and the active worker come up; the benched reviewer does not"
        );
        tx.commit().unwrap();
    }

    #[test]
    fn invalid_late_initial_staff_refuses_before_department_head_or_prior_staff_write() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut invalid = new_staff_seed("platform-reviewer", "Reviewer");
        invalid.seed.kind = PersonKind::Head;
        let staff = vec![new_staff_seed("platform-developer", "Developer"), invalid];
        let out = create_department_with_staff(
            &tx,
            "acme",
            "platform",
            "executive",
            "Platform",
            "Own platform",
            &HeadDecision::HireNew {
                person_id: "platform-head".into(),
                seed: Box::new(new_head_seed("Pat", "Head of Platform", "lead platform")),
            },
            &staff,
            Some("ada"),
            "Create Platform and its initial roster.",
            "2026-07-25T00:00:00.000Z",
            "ada",
        )
        .unwrap();
        assert!(matches!(
            out,
            CreateDepartmentOutcome::Refused {
                reason: CreateDepartmentRefusal::InvalidSeed { ref field, .. }
            } if field == "staff[1].kind"
        ));
        let writes: (i64, i64, i64, i64, i64) = (
            tx.query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='platform'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            tx.query_row(
                "SELECT COUNT(*) FROM people WHERE slug='acme' \
                 AND id IN ('platform-head','platform-developer','platform-reviewer')",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            tx.query_row(
                "SELECT COUNT(*) FROM person_activity WHERE slug='acme' \
                 AND person_id IN ('platform-head','platform-developer','platform-reviewer')",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            tx.query_row(
                "SELECT COUNT(*) FROM staffing_history WHERE slug='acme' \
                 AND person_id IN ('platform-head','platform-developer','platform-reviewer')",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            tx.query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |row| row.get(0))
                .unwrap(),
        );
        assert_eq!(writes, (0, 0, 0, 0, 0));
        tx.commit().unwrap();
    }

    #[test]
    fn duplicate_late_initial_staff_refuses_before_any_write() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let staff = vec![
            new_staff_seed("platform-developer", "Developer"),
            new_staff_seed("platform-developer", "Duplicate Developer"),
        ];
        let out = create_department_with_staff(
            &tx,
            "acme",
            "platform",
            "executive",
            "Platform",
            "Own platform",
            &HeadDecision::HireNew {
                person_id: "platform-head".into(),
                seed: Box::new(new_head_seed("Pat", "Head of Platform", "lead platform")),
            },
            &staff,
            Some("ada"),
            "Create Platform and its initial roster.",
            "2026-07-25T00:00:00.000Z",
            "ada",
        )
        .unwrap();
        assert_eq!(
            out,
            CreateDepartmentOutcome::Refused { reason: CreateDepartmentRefusal::DuplicatePersonId },
        );
        let rows: (i64, i64, i64) = (
            tx.query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='platform'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            tx.query_row(
                "SELECT COUNT(*) FROM people WHERE slug='acme' \
                 AND id IN ('platform-head','platform-developer')",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            tx.query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |row| row.get(0))
                .unwrap(),
        );
        assert_eq!(rows, (0, 0, 0));
        tx.commit().unwrap();
    }

    #[test]
    fn refuses_unknown_parent_and_writes_nothing() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = create_department(
            &tx,
            "acme",
            "x",
            "does-not-exist",
            "X",
            "",
            &HeadDecision::AppointExisting { person_id: "bo".into() },
            "ada",
            "test create",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(
            out,
            CreateDepartmentOutcome::Refused { reason: CreateDepartmentRefusal::UnknownParent }
        );
        let n: i64 = tx
            .query_row("SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='x'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
        tx.commit().unwrap();
    }

    #[test]
    fn refuses_paused_parent() {
        let mut conn = open();
        conn.execute("UPDATE departments SET state='paused' WHERE slug='acme' AND id='eng'", [])
            .unwrap();
        let tx = conn.transaction().unwrap();
        let out = create_department(
            &tx,
            "acme",
            "eng-sub",
            "eng",
            "Eng Sub",
            "",
            &HeadDecision::AppointExisting { person_id: "bo".into() },
            "ada",
            "test create",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(
            out,
            CreateDepartmentOutcome::Refused { reason: CreateDepartmentRefusal::ParentPaused }
        );
        tx.commit().unwrap();
    }

    #[test]
    fn refuses_an_active_parent_below_a_paused_ancestor() {
        let mut conn = open();
        conn.execute_batch(
            "INSERT INTO departments(slug,id,parent_id,name,kind,state,head_person_id,ordinal,created_at,updated_at) \
             VALUES('acme','eng-platform','eng','Platform','department','active','nita',3,'t','t'); \
             UPDATE departments SET state='paused' WHERE slug='acme' AND id='eng'",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = create_department(
            &tx,
            "acme",
            "eng-platform-sub",
            "eng-platform",
            "Eng Platform Sub",
            "",
            &HeadDecision::AppointExisting { person_id: "bo".into() },
            "ada",
            "test create",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(
            out,
            CreateDepartmentOutcome::Refused { reason: CreateDepartmentRefusal::ParentPaused }
        );
        tx.commit().unwrap();
    }

    #[test]
    fn refuses_duplicate_department_id() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = create_department(
            &tx,
            "acme",
            "eng",
            "executive",
            "Eng Two",
            "",
            &HeadDecision::AppointExisting { person_id: "bo".into() },
            "ada",
            "test create",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(
            out,
            CreateDepartmentOutcome::Refused {
                reason: CreateDepartmentRefusal::DuplicateDepartmentId
            }
        );
        tx.commit().unwrap();
    }

    #[test]
    fn refuses_head_decision_when_appointee_absent() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = create_department(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "ghost".into() },
            "ada",
            "test create",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(
            out,
            CreateDepartmentOutcome::Refused {
                reason: CreateDepartmentRefusal::HeadDecisionRequired
            }
        );
        tx.commit().unwrap();
    }

    /// ONLY THE CEO. Operator ruling, 2026-08-13 (`AGENTS.md`).
    ///
    /// The CEO is refused because appointing an existing head MOVES that
    /// person and the CEO always heads the root. The chief of staff is no
    /// longer refused FOR THAT REASON — he is not the CEO — but this fixture's
    /// `cos` heads `office-of-the-ceo`, so the eligibility rule catches him
    /// instead and says so by name. Both walls are asserted here because the
    /// operator will meet the second one the moment the first is gone.
    #[test]
    fn only_the_ceo_is_refused_as_an_appointed_head() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let ada = create_department(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "ada".into() },
            "ada",
            "test create",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(
            ada,
            CreateDepartmentOutcome::Refused {
                reason: CreateDepartmentRefusal::ExecRootProtected { person_id: "ada".into() }
            },
            "the CEO is the one immovable node"
        );
        let CreateDepartmentOutcome::Refused { reason } = &ada else {
            panic!("the CEO must be refused");
        };
        let message = reason.detail();
        assert!(message.contains("'ada'"), "must name the person: {message}");
        assert!(message.contains("the CEO"), "must say WHY: {message}");
        // The fact whose absence produced a dozen turns of guessing.
        assert!(
            message.contains("MOVES that person"),
            "must say the appointment is itself the move: {message}"
        );
        assert!(message.contains("NEW head"), "must name a way through: {message}");
        assert_eq!(reason.code(), "exec-root-protected", "the machine code is unchanged");

        // The chief of staff is NOT the CEO. He is refused only because he
        // already heads a unit — a different rule, with a different answer.
        let cos = create_department(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "cos".into() },
            "ada",
            "test create",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        // CHANGED DELIBERATELY, with the reason. This asserted
        // `HeadNotEligible { because: AlreadyHeads }` — a refusal that told a
        // sitting head only that they already led something and offered
        // "appoint somebody else, or hand their current department over
        // first". For `cos`, who is the ONLY member of `office-of-the-ceo`,
        // there is nobody to hand it to, so that refusal had no way through
        // and this is the exact case the operator hit. The rule did not
        // weaken: `cos` is still refused for stating no decision, and the
        // refusal now names the department he would empty and the successors
        // that exist — none, which is what makes dissolve the only answer.
        let CreateDepartmentOutcome::Refused {
            reason: CreateDepartmentRefusal::HeadVacancy(vacancy),
        } = &cos
        else {
            panic!("expected a vacancy refusal, got {cos:?}");
        };
        assert_eq!(vacancy.code(), "vacancy-decision-required");
        let HeadVacancyRefusal::Required { person_id, department_id, eligible_successor_ids } =
            vacancy
        else {
            panic!("expected the required arm, got {vacancy:?}");
        };
        assert_eq!(person_id, "cos");
        assert_eq!(department_id, "office-of-the-ceo");
        assert!(
            eligible_successor_ids.is_empty(),
            "cos is its only member, so there is nobody to hand it to: {eligible_successor_ids:?}"
        );
        let detail = vacancy.detail();
        assert!(
            detail.contains("dissolve") && !detail.contains("hand-over"),
            "a last member must be offered dissolve and never a hand-over: {detail}"
        );
        assert!(detail.contains("office-of-the-ceo"), "the refusal must name it: {detail}");
        tx.commit().unwrap();
    }

    /// THE OPERATOR'S CASE, end to end, on the real shape it happened on.
    ///
    /// `tribes-capital` on 2026-08-13: Carlos is `kind=worker`, homed in the
    /// ROOT department, heads nothing, and there is no `office-of-the-ceo` in
    /// that company at all. The CEO was told to put Engineering under him and
    /// could not: the executive-root guard froze every person whose home row
    /// named the root. Exactly one thing refused him, and this is it gone.
    #[test]
    fn a_worker_homed_in_the_root_becomes_a_head_and_engineering_moves_beneath_him() {
        let mut conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        // The live table, reproduced: no office-of-the-ceo, and a chief of
        // staff who is an ordinary worker sitting in the root.
        conn.execute_batch(
            "INSERT INTO org_settings(slug, display_slug, supervision_interval_ms, acknowledgement_timeout_ms, acknowledgement_retry_limit, replacement_limit) VALUES('tribes-capital', 'tribes-capital',60000,30000,3,3); INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('tribes-capital','executive',NULL,'Tribes Capital','company','active','chief',0,'t','t'), ('tribes-capital','engineering','executive','Engineering','department','active','head-of-engineering',1,'t','t');
             INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('tribes-capital','chief','Chief','CEO','lead','executive','active','executive',0,'t','t'), ('tribes-capital','carlos','Carlos','Chief of Staff','support','worker','active','executive',1,'t','t'), ('tribes-capital','head-of-engineering','Gordon','Head of Engineering','build','head','active','engineering',2,'t','t');",
        )
        .expect("seed");
        let tx = conn.transaction().unwrap();

        let created = create_department(
            &tx,
            "tribes-capital",
            "leadership",
            "executive",
            "Leadership",
            "Carlos runs this",
            &HeadDecision::AppointExisting { person_id: "carlos".into() },
            "chief",
            "the operator asked for it",
            "2026-08-13T01:01:04.000Z",
            "chief",
        )
        .unwrap();
        assert!(
            matches!(created, CreateDepartmentOutcome::Applied { .. }),
            "the operator's request must succeed: {created:?}"
        );

        // He IS the head, and heading a department means living in it.
        assert_eq!(
            organization_rows::department_head(&tx, "tribes-capital", "leadership").unwrap(),
            Some("carlos".to_string())
        );
        let (_, department_id) =
            organization_rows::person_placement(&tx, "tribes-capital", "carlos").unwrap().unwrap();
        assert_eq!(department_id, "leadership");

        // And Engineering reparents beneath him — the second half of the ask.
        let moved = reparent_department(
            &tx,
            "tribes-capital",
            "engineering",
            "leadership",
            "2026-08-13T01:02:00.000Z",
            "under carlos",
        )
        .unwrap();
        assert_eq!(moved, ReparentOutcome::Applied { department_id: "engineering".into() });

        // The CEO is still the one immovable node, on the same company.
        let ceo = create_department(
            &tx,
            "tribes-capital",
            "office",
            "executive",
            "Office",
            "Nope",
            &HeadDecision::AppointExisting { person_id: "chief".into() },
            "chief",
            "must refuse",
            "2026-08-13T01:03:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(
            ceo,
            CreateDepartmentOutcome::Refused {
                reason: CreateDepartmentRefusal::ExecRootProtected { person_id: "chief".into() }
            }
        );
        tx.commit().unwrap();
    }

    /// Every ineligibility names ITSELF and the move that clears it.
    ///
    /// Reciting all four conditions made the caller test its own person
    /// against a checklist. The four are separate values because each one is a
    /// different operator move.
    #[test]
    fn head_ineligibility_names_the_clause_that_fired_and_the_way_out() {
        use super::HeadIneligibility as Why;
        // TWO arms stood here and are DELETED, not merely unasserted: `OnLoan`
        // went with the loan concept, and `AlreadyHeads` was replaced by the
        // vacancy decision — its detail, "appoint somebody else, or hand their
        // current department over first", was a dead end for a head who is
        // their department's only member.
        for (why, expected) in [(Why::Departed, "re-hire"), (Why::NotAWorker, "WORKER")] {
            assert!(
                why.detail().contains(expected),
                "{why:?} must say {expected}: {}",
                why.detail()
            );
        }
        let refusal = CreateDepartmentRefusal::HeadNotEligible {
            person_id: "cos".into(),
            because: Why::NotAWorker,
        };
        assert!(refusal.detail().contains("'cos'"), "{}", refusal.detail());
        assert_eq!(refusal.code(), "head-not-eligible", "the machine code is unchanged");
    }

    /// The one refusal in this family whose subject is a DEPARTMENT. Its copy
    /// must not send the reader looking for a protected person.
    #[test]
    fn the_reparent_refusal_is_about_a_unit_not_a_person() {
        let message = super::ReparentRefusal::ExecRootProtected.detail();
        assert!(message.contains("that department"), "{message}");
        assert!(!message.contains("the person"), "a department is not a person: {message}");
        // States the narrowed rule: the ROOT is the only fixed unit, and
        // everything under it moves.
        assert!(message.contains("the company root"), "{message}");
        assert!(message.contains("Every department BENEATH it"), "{message}");
    }

    /// The transfer refusal keeps a PERSON subject, and names what to do.
    #[test]
    fn the_transfer_refusal_names_the_person_and_a_way_through() {
        let message = super::TransferRefusal::ExecRootProtected.detail();
        assert!(message.contains("that person is the CEO"), "{message}");
        assert!(
            message.contains("Every other person may be transferred anywhere"),
            "the refusal must say who CAN move, now that it is only the CEO who cannot: {message}"
        );
    }

    #[test]
    fn unrelated_prior_event_does_not_block_department_create() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        apply_and_emit::<rusqlite::Error, _>(&tx, "acme", "t", "other", |_tx| {
            Ok(vec![crate::store::rows_txn::EventTouch::new(
                "person", "x", "upsert", "people", "acme",
            )])
        })
        .unwrap();
        let out = create_department(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "nita".into() },
            "ada",
            "test create",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(out, CreateDepartmentOutcome::Applied { department_id: "product".into() });
        let n: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='product'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        let events: Vec<(String, String)> = tx
            .prepare(
                "SELECT entity, entity_id FROM org_events \
                 WHERE slug='acme' AND seq > 1 ORDER BY seq",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(
            events.iter().any(|row| row == &("department".into(), "product".into())),
            "the caller-revisionless create still emits its durable department audit event"
        );
        tx.commit().unwrap();
    }

    #[test]
    fn emits_department_then_person_touches_as_one_contiguous_seq_run() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        create_department(
            &tx,
            "acme",
            "product",
            "executive",
            "Product",
            "Ship product",
            &HeadDecision::AppointExisting { person_id: "nita".into() },
            "ada",
            "test create",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        let rows: Vec<(i64, String, String)> = tx
            .prepare("SELECT seq, entity, op FROM org_events WHERE slug='acme' ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let ops: Vec<(&str, &str)> =
            rows.iter().map(|(_, e, o)| (e.as_str(), o.as_str())).collect();
        // Repeated person mutations are de-duplicated before the event feed is
        // committed. The third touch is the head's launch fence: creation now
        // brings the head UP, and that decision is a durable row change like
        // any other, so it takes its place in the SAME contiguous run rather
        // than arriving as a separate later write.
        assert_eq!(
            ops,
            vec![("department", "upsert"), ("person", "upsert"), ("launch-intent", "upsert")],
        );
        let seqs: Vec<i64> = rows.iter().map(|(s, ..)| *s).collect();
        assert_eq!(seqs, vec![1, 2, 3]); // contiguous run
        tx.commit().unwrap();
    }

    /// The other half of the same rule, and the reason the probe above had to
    /// move: a LEAF may grow a unit beneath the unit it sits in. Bo heads
    /// nothing here, so under the old `person_manages_department` gate this was
    /// refused `requester-out-of-scope` no matter which tool Bo held — the
    /// documented "every leaf can become a parent" was unreachable at the store
    /// boundary. Creating takes authority over nobody: nothing that already
    /// exists changes hands, and Bo heads only what Bo just made.
    #[test]
    fn a_leaf_may_create_a_department_beneath_the_unit_it_sits_in() {
        let mut conn = open();
        conn.execute(
            "UPDATE departments SET head_person_id='nita' WHERE slug='acme' AND id='eng'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let headed: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND head_person_id='bo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(headed, 0, "the premise: Bo heads nothing at all");
        let out = create_department(
            &tx,
            "acme",
            "eng-platform",
            "eng", // Bo's unit, and the only parent Bo may reach.
            "Platform",
            "Own platform",
            &HeadDecision::HireNew {
                person_id: "platform-head".into(),
                seed: Box::new(new_head_seed("Pat", "Platform Head", "own platform")),
            },
            "bo",
            "Create Platform.",
            "2026-07-25T00:00:00.000Z",
            "bo",
        )
        .unwrap();
        assert!(
            matches!(out, CreateDepartmentOutcome::Applied { .. }),
            "a leaf grows a unit beneath the unit it sits in: {out:?}"
        );
        // Growth is DOWNWARD only, and the new unit hangs where it was asked to.
        let parent: String = tx
            .query_row(
                "SELECT parent_id FROM departments WHERE slug='acme' AND id='eng-platform'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(parent, "eng");
        // And nobody who already existed changed hands.
        let bo_home: String = tx
            .query_row("SELECT department_id FROM people WHERE id='bo'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(bo_home, "eng", "creating a child unit moves nobody, including its creator");
        tx.commit().unwrap();
    }

    /// THE OVER-GRANT AXIS. Growth is DOWNWARD only, and "downward" is the
    /// whole safety claim: the create-path authority is deliberately more
    /// permissive than management scope, so it is exactly where an over-grant
    /// would hide. A leaf reaches the unit it sits in and nothing else — not a
    /// SIBLING unit, and not an ancestor.
    #[test]
    fn a_leaf_cannot_create_beneath_a_unit_it_neither_heads_nor_sits_in() {
        for parent in ["office-of-the-ceo", "executive"] {
            let mut conn = open();
            conn.execute(
                "UPDATE departments SET head_person_id='nita' WHERE slug='acme' AND id='eng'",
                [],
            )
            .unwrap();
            let tx = conn.transaction().unwrap();
            let out = create_department(
                &tx,
                "acme",
                "eng-platform",
                parent,
                "Platform",
                "Own platform",
                &HeadDecision::HireNew {
                    person_id: "platform-head".into(),
                    seed: Box::new(new_head_seed("Pat", "Platform Head", "own platform")),
                },
                "bo", // a leaf in `eng`
                "Create Platform.",
                "2026-07-25T00:00:00.000Z",
                "bo",
            )
            .unwrap();
            assert_eq!(
                out,
                CreateDepartmentOutcome::Refused {
                    reason: CreateDepartmentRefusal::RequesterOutOfScope
                },
                "a leaf must not grow sideways or upward: parent '{parent}'"
            );
            let written: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='eng-platform'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(written, 0, "a refused create writes nothing: parent '{parent}'");
            tx.commit().unwrap();
        }
    }

    /// A person who has left the company reaches nothing at all. The
    /// create-path predicate answers on the CURRENT employment row, so a
    /// departed leaf cannot grow a unit under the department it used to sit in.
    #[test]
    fn a_departed_person_cannot_create_a_department_anywhere() {
        let mut conn = open();
        conn.execute(
            "UPDATE departments SET head_person_id='nita' WHERE slug='acme' AND id='eng'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE people SET employment_state='departed' WHERE slug='acme' AND id='bo'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = create_department(
            &tx,
            "acme",
            "eng-platform",
            "eng", // the unit Bo used to sit in
            "Platform",
            "Own platform",
            &HeadDecision::HireNew {
                person_id: "platform-head".into(),
                seed: Box::new(new_head_seed("Pat", "Platform Head", "own platform")),
            },
            "bo",
            "Create Platform.",
            "2026-07-25T00:00:00.000Z",
            "bo",
        )
        .unwrap();
        assert_eq!(
            out,
            CreateDepartmentOutcome::Refused {
                reason: CreateDepartmentRefusal::RequesterOutOfScope
            },
            "leaving the company ends the authority to grow one"
        );
        tx.commit().unwrap();
    }

    #[test]
    fn stale_manager_scope_refuses_department_create_without_writes() {
        let mut conn = open();
        // Bo's old pane was authorized while Bo headed Engineering. The
        // normalized hierarchy now names Nita instead; the transaction must
        // evaluate this current fact rather than trusting the old projection.
        //
        // The PROBE moved and the subject did not. Bo creating under `eng` is
        // no longer the question: Bo is ASSIGNED to eng, and every leaf may
        // grow a unit beneath the unit it sits in, so that create is allowed on
        // its own merits and would prove nothing about staleness. What proves
        // staleness is a parent Bo could only reach by still heading eng —
        // `office-of-the-ceo` is neither Bo's unit nor inside any
        // subtree Bo heads, so a stale `kind='head'` row buys Bo nothing there.
        conn.execute("UPDATE people SET kind='head' WHERE slug='acme' AND id='bo'", []).unwrap();
        conn.execute(
            "UPDATE departments SET head_person_id='nita' WHERE slug='acme' AND id='eng'",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = create_department(
            &tx,
            "acme",
            "eng-platform",
            "office-of-the-ceo",
            "Platform",
            "Own platform",
            &HeadDecision::HireNew {
                person_id: "platform-head".into(),
                seed: Box::new(new_head_seed("Pat", "Platform Head", "own platform")),
            },
            "bo",
            "Create Platform.",
            "2026-07-25T00:00:00.000Z",
            "bo",
        )
        .unwrap();
        assert_eq!(
            out,
            CreateDepartmentOutcome::Refused {
                reason: CreateDepartmentRefusal::RequesterOutOfScope
            },
            "a person whose head row is stale reaches no unit outside the one it sits in"
        );
        let departments: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='eng-platform'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let people: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM people WHERE slug='acme' AND id='platform-head'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let events: i64 = tx
            .query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!((departments, people, events), (0, 0, 0));
        tx.commit().unwrap();
    }

    #[test]
    fn manager_cannot_appoint_a_head_from_an_out_of_scope_sibling() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let placement_before = organization_rows::person_placement(&tx, "acme", "nita").unwrap();

        // Cos manages office-of-the-ceo, so the parent is in scope. Nita lives
        // in sibling Engineering, which Cos does not manage. Destination scope
        // alone must not authorize this implicit cross-sibling transfer.
        let out = create_department(
            &tx,
            "acme",
            "ceo-strategy",
            "office-of-the-ceo",
            "CEO Strategy",
            "Own CEO strategy",
            &HeadDecision::AppointExisting { person_id: "nita".into() },
            "cos",
            "Create CEO Strategy with Nita as head.",
            "2026-07-25T00:00:00.000Z",
            "cos",
        )
        .unwrap();
        assert_eq!(
            out,
            CreateDepartmentOutcome::Refused {
                reason: CreateDepartmentRefusal::RequesterOutOfScope
            }
        );
        assert_eq!(
            organization_rows::person_placement(&tx, "acme", "nita").unwrap(),
            placement_before
        );
        let department_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='ceo-strategy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let event_count: i64 = tx
            .query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!((department_count, event_count), (0, 0));
        tx.commit().unwrap();
    }
}

#[cfg(test)]
mod transfer_move_tests {
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;
    use rusqlite::Connection;

    /// Open a FULL-schema in-memory company (so the real typed accessors write
    /// every column) and seed a minimal exec-root org:
    ///   executive (root, head ada=CEO)
    ///     ├─ office-of-the-ceo (head cos = chief-of-staff)
    ///     └─ eng (head bo = a normal report)
    /// ada & cos are executive-root protected; bo is the normal shutdown target.
    /// FKs OFF: this unit exercises org_ops' composition, not the manifest FKs.
    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('acme','executive',NULL,'Executive','company','active','ada',0,'t','t'), ('acme','office-of-the-ceo','executive','Office of the CEO','department','active','cos',1,'t','t'), ('acme','eng','executive','Engineering','department','active','bo',2,'t','t');
             INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','ada','Ada','CEO','lead','executive','active','executive',0,'t','t'), ('acme','cos','Cos','Chief of Staff','support','head','active','office-of-the-ceo',1,'t','t'), ('acme','bo','Bo','Engineer','build','worker','active','eng',2,'t','t');",
        )
        .expect("seed");
        conn
    }

    /// Seed two non-head workers in `eng`: dana(ord 3), eli(ord 4). eng's head
    /// stays bo, so these are legitimate (non-head) transfer subjects.
    fn seed_eng_workers(conn: &Connection) {
        conn.execute(
            "INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','dana','Dana','Eng','build','worker','active','eng',3,'t','t'), ('acme','eli','Eli','Eng','build','worker','active','eng',4,'t','t')",
            [],
        )
        .unwrap();
    }

    fn home(conn: &Connection, person: &str) -> String {
        conn.query_row(
            "SELECT department_id FROM people WHERE slug='acme' AND id=?1",
            rusqlite::params![person],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Assert the whole-company ordinals are a gapless 0..N-1 bijection.
    fn ordinals(conn: &Connection) -> Vec<i64> {
        let mut stmt =
            conn.prepare("SELECT ordinal FROM people WHERE slug='acme' ORDER BY ordinal").unwrap();
        let got: Vec<i64> = stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        let n = got.len() as i64;
        assert_eq!(got, (0..n).collect::<Vec<_>>(), "ordinals must be a gapless bijection");
        got
    }

    #[test]
    fn transfer_moves_home_assigned_records_staffing_and_keeps_the_bijection() {
        let mut conn = open();
        seed_eng_workers(&conn);
        let tx = conn.transaction().unwrap();
        let out = transfer_person(
            &tx,
            "acme",
            "dana",
            "office-of-the-ceo",
            "person-transfer:1",
            "2026-07-25T00:00:00.000Z",
            "chief",
            None,
        )
        .unwrap();
        assert_eq!(out, TransferOutcome::Applied { moved: vec!["dana".into()] });
        let placement: String = tx
            .query_row(
                "SELECT department_id FROM people WHERE slug='acme' AND id='dana'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(placement, "office-of-the-ceo");
        ordinals(&tx);
        let (action, from, to, reason): (String, String, String, String) = tx
            .query_row(
                "SELECT action, from_department_id, to_department_id, reason FROM staffing_history \
                 WHERE slug='acme' AND person_id='dana'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (action.as_str(), from.as_str(), to.as_str()),
            ("transferred", "eng", "office-of-the-ceo")
        );
        assert_eq!(
            reason, "transferred by chief",
            "the daemon authors the ledger line and names the actor"
        );
        tx.commit().unwrap();
    }

    /// NOBODY IS ASKED TO JUSTIFY A TRANSFER, AND THE LEDGER STILL NAMES WHO
    /// DID IT. The caller supplies no audit prose at all; the staffing-history
    /// line is authored here from the act and the actor, and the org-event
    /// actor is the same principal. A deletion that also stopped recording
    /// would be the regression this pins.
    #[test]
    fn transfer_authors_its_ledger_line_from_the_act_and_the_actor() {
        let mut conn = open();
        seed_eng_workers(&conn);
        let tx = conn.transaction().unwrap();
        let out = transfer_person(
            &tx,
            "acme",
            "dana",
            "office-of-the-ceo",
            "person-transfer:actor-test",
            "2026-07-25T00:00:00.000Z",
            "chief",
            // Dana heads nothing, so there is nothing to vacate.
            None,
        )
        .unwrap();
        assert_eq!(out, TransferOutcome::Applied { moved: vec!["dana".into()] });
        let reason: String = tx
            .query_row(
                "SELECT reason FROM staffing_history WHERE slug='acme' AND person_id='dana'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let actor: String = tx.query_row(
            "SELECT actor FROM org_events WHERE slug='acme' AND entity='person' AND entity_id='dana'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(reason, "transferred by chief");
        assert_eq!(actor, "chief");
        tx.commit().unwrap();
    }

    /// An actorless in-process transfer records the ACT alone rather than a
    /// dangling "by".
    #[test]
    fn transfer_with_no_actor_records_the_act_alone() {
        let mut conn = open();
        seed_eng_workers(&conn);
        let tx = conn.transaction().unwrap();
        transfer_person(
            &tx,
            "acme",
            "dana",
            "office-of-the-ceo",
            "person-transfer:no-actor",
            "2026-07-25T00:00:00.000Z",
            "",
            None,
        )
        .unwrap();
        let reason: String = tx
            .query_row(
                "SELECT reason FROM staffing_history WHERE slug='acme' AND person_id='dana'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, "transferred");
        tx.commit().unwrap();
    }

    #[test]
    fn transfer_refuses_unknown_person_destination_and_exec_root_and_head() {
        let mut conn = open();
        conn.execute("INSERT INTO departments(slug,id,parent_id,name,kind,state,head_person_id,ordinal,created_at,updated_at) \
             VALUES('acme','sink','executive','Sink','department','paused','sink-head',3,'t','t')", []).unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            transfer_person(&tx, "acme", "ghost", "eng", "i", "t", "a", None).unwrap(),
            TransferOutcome::Refused { reason: TransferRefusal::UnknownPerson }
        );
        assert_eq!(
            transfer_person(&tx, "acme", "bo", "does-not-exist", "i", "t", "a", None).unwrap(),
            TransferOutcome::Refused { reason: TransferRefusal::UnknownDestination }
        );
        assert_eq!(
            transfer_person(&tx, "acme", "bo", "sink", "i", "t", "a", None).unwrap(),
            TransferOutcome::Refused { reason: TransferRefusal::DestinationPaused }
        );
        // ada is the CEO (exec-root protected).
        assert_eq!(
            transfer_person(&tx, "acme", "ada", "eng", "i", "t", "a", None).unwrap(),
            TransferOutcome::Refused { reason: TransferRefusal::ExecRootProtected }
        );
        // bo heads eng, and `transfer_person` carries no vacancy decision, so
        // the move is still refused. CHANGED DELIBERATELY: this asserted the
        // bare `HeadNeedsSuccessor`, whose only advice was "appoint a successor
        // first" — unanswerable when the department has no other member, which
        // is the dead end this packet closes. The refusal is not weaker; it
        // names the department that would be left without a head and the
        // members who could take it, and `eligible_successor_ids` being EMPTY
        // here is the fact that says dissolve is the only answer for `eng`.
        assert_eq!(
            transfer_person(&tx, "acme", "bo", "office-of-the-ceo", "i", "t", "a", None).unwrap(),
            TransferOutcome::Refused {
                reason: TransferRefusal::HeadVacancy(HeadVacancyRefusal::Required {
                    person_id: "bo".into(),
                    department_id: "eng".into(),
                    eligible_successor_ids: Vec::new(),
                })
            }
        );
        tx.commit().unwrap();
    }

    #[test]
    fn transfer_refuses_an_active_destination_below_a_paused_ancestor() {
        let mut conn = open();
        conn.execute_batch(
            "INSERT INTO departments(slug,id,parent_id,name,kind,state,head_person_id,ordinal,created_at,updated_at) \
             VALUES('acme','sink','eng','Sink','department','active','sink-head',3,'t','t'); \
             UPDATE departments SET state='paused' WHERE slug='acme' AND id='eng';",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            transfer_person(&tx, "acme", "bo", "sink", "i", "t", "a", None).unwrap(),
            TransferOutcome::Refused { reason: TransferRefusal::DestinationPaused },
        );
        tx.commit().unwrap();
    }

    #[test]
    fn transfer_supersedes_an_open_transition_with_the_transfer_marker() {
        let mut conn = open();
        seed_eng_workers(&conn);
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, requested_at) \
             VALUES('acme','open-1','dana','park','ready','t0')",
            [],
        )
        .unwrap();
        // The person's activity pointer names the open row, as every seeded
        // in-flight transition does; the supersede must clear it in the same
        // commit or the next whole-ledger read fails validation.
        conn.execute(
            "INSERT INTO person_activity(slug, person_id, active_transition_id, updated_at) \
             VALUES('acme','dana','open-1','t0')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        transfer_person(
            &tx,
            "acme",
            "dana",
            "office-of-the-ceo",
            "person-transfer:9",
            "2026-07-25T00:00:00.000Z",
            "chief",
            None,
        )
        .unwrap();
        let (status, reason): (String, String) = tx
            .query_row(
                "SELECT status, reason FROM transitions WHERE slug='acme' AND id='open-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(reason, "superseded-by-transfer:person-transfer:9");
        let pointer: Option<String> = tx
            .query_row(
                "SELECT active_transition_id FROM person_activity WHERE slug='acme' AND person_id='dana'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pointer, None, "the superseded transition cannot remain dana's active pointer");
        tx.commit().unwrap();
    }

    #[test]
    fn transfer_adopts_a_released_transition_instead_of_superseding_it() {
        // class-D: the staffing lifecycle RELEASES the transition before calling
        // transfer_person, so the open transfer transition is already `ready`.
        // Superseding it here throws that release away and mints a fresh
        // `awaiting_handoff` row in its place, so the move applies while the
        // ledger still shows the person owing a handoff nobody asked them for.
        // Mirrors offboard_person_atomic's established adoption via
        // `ready_open_transition_id`.
        let mut conn = open();
        seed_eng_workers(&conn);
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, reason, requested_at, handoff_deadline_at, placement_department_id) VALUES('acme','transition:9:dana:transfer','dana','transfer','ready','transferred dana','t0','t9','eng')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO person_activity(slug, person_id, active_transition_id, updated_at) \
             VALUES('acme','dana','transition:9:dana:transfer','t0')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = transfer_person(
            &tx,
            "acme",
            "dana",
            "office-of-the-ceo",
            "person-transfer:9",
            "2026-07-25T00:00:00.000Z",
            "chief",
            None,
        )
        .unwrap();
        assert_eq!(out, TransferOutcome::Applied { moved: vec!["dana".to_string()] });

        // The ready row is ADOPTED, not cancelled; no second transition is minted.
        let transitions: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE person_id='dana'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(transitions, 1, "adoption mints no replacement transition");
        let status: String = tx
            .query_row(
                "SELECT status FROM transitions WHERE id='transition:9:dana:transfer'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            status, "ready",
            "a released transition is consumed by the reconcile, never superseded"
        );
        let pointer: String = tx
            .query_row(
                "SELECT active_transition_id FROM person_activity WHERE person_id='dana'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            pointer, "transition:9:dana:transfer",
            "the ADOPTED handoff stays dana's active pointer"
        );
        tx.commit().unwrap();
        // The move itself still applies -- adoption is not a refusal.
        assert_eq!(home(&conn, "dana"), "office-of-the-ceo");
    }

    #[test]
    fn move_department_members_still_supersedes_a_ready_transition_unconditionally() {
        // move_department_members is a direct bulk admin verb (org-ops R5) with
        // no per-person graceful-transition lifecycle preceding it -- unlike
        // transfer_person, it never has a synthetic handoff to protect, so it
        // must keep its unconditional supersede exactly as before this fix.
        let mut conn = open();
        seed_eng_workers(&conn);
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, reason, requested_at, handoff_deadline_at, placement_department_id) VALUES('acme','transition:9:dana:transfer','dana','transfer','ready','transferred dana','t0','t9','eng')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO person_activity(slug, person_id, active_transition_id, updated_at) \
             VALUES('acme','dana','transition:9:dana:transfer','t0')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        move_department_members(
            &tx,
            "acme",
            "eng",
            "office-of-the-ceo",
            &["dana".to_string()],
            "unit-move:9",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        let status: String = tx
            .query_row(
                "SELECT status FROM transitions WHERE id='transition:9:dana:transfer'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "cancelled", "the batch-move verb keeps its unconditional supersede");
        tx.commit().unwrap();
    }

    #[test]
    fn transfer_preserves_launch_intent_and_never_touches_supervision() {
        let mut conn = open();
        seed_eng_workers(&conn);
        conn.execute("INSERT INTO launch_intent(slug, person_id) VALUES('acme','dana')", [])
            .unwrap();
        let tx = conn.transaction().unwrap();
        transfer_person(
            &tx,
            "acme",
            "dana",
            "office-of-the-ceo",
            "person-transfer:2",
            "2026-07-25T00:00:00.000Z",
            "chief",
            None,
        )
        .unwrap();
        let fenced: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM launch_intent WHERE slug='acme' AND person_id='dana'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fenced, 1, "launch-intent membership is preserved by a transfer");
        tx.commit().unwrap();
    }

    #[test]
    fn transfer_emits_row_level_touches_as_one_contiguous_seq_run() {
        // Composition order = seq order: supersede(cancel) → mover → any other
        // person shifted by canonical department-grouped order. The staffing
        // entry is its own D2 feed (no org_events touch).
        let mut conn = open();
        seed_eng_workers(&conn);
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, requested_at) \
             VALUES('acme','open-1','dana','park','ready','t0')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        transfer_person(
            &tx,
            "acme",
            "dana",
            "office-of-the-ceo",
            "person-transfer:7",
            "2026-07-25T00:00:00.000Z",
            "chief",
            None,
        )
        .unwrap();
        let rows: Vec<(i64, String, String, String)> = tx
            .prepare(
                "SELECT seq, entity, entity_id, op FROM org_events \
                 WHERE slug='acme' ORDER BY seq",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let ops: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|(_, entity, id, op)| (entity.as_str(), id.as_str(), op.as_str()))
            .collect();
        assert_eq!(
            ops,
            vec![
                ("transition", "open-1", "upsert"),
                ("person", "dana", "upsert"),
                ("person", "bo", "upsert"),
            ],
        );
        assert_eq!(rows.iter().map(|(seq, ..)| *seq).collect::<Vec<_>>(), vec![1, 2, 3],);
        tx.commit().unwrap();
    }

    #[test]
    fn unrelated_prior_event_does_not_reject_a_direct_transfer() {
        let mut conn = open();
        seed_eng_workers(&conn);
        conn.execute(
            "INSERT INTO org_events(slug, seq, entity, entity_id, op, at) \
             VALUES('acme', 1, 'person', 'x', 'noop', 't')",
            [],
        )
        .unwrap();
        // D2 sequence allocation is counter-backed. Seed the counter alongside
        // the prior event exactly as a real committed writer would, so this
        // regression checks revisionless transfer semantics rather than a
        // deliberately corrupt feed.
        conn.execute("INSERT INTO counters(name, value) VALUES('org-events:acme', 1)", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out = transfer_person(
            &tx,
            "acme",
            "dana",
            "office-of-the-ceo",
            "i",
            "2026-07-25T00:00:00.000Z",
            "chief",
            None,
        )
        .unwrap();
        assert_eq!(out, TransferOutcome::Applied { moved: vec!["dana".into()] });
        assert_eq!(home(&tx, "dana"), "office-of-the-ceo");
        tx.commit().unwrap();
    }

    #[test]
    fn move_members_moves_all_listed_and_keeps_bijection() {
        let mut conn = open();
        seed_eng_workers(&conn);
        let tx = conn.transaction().unwrap();
        let out = move_department_members(
            &tx,
            "acme",
            "eng",
            "office-of-the-ceo",
            &["dana".to_string(), "eli".to_string()],
            "dept-move:1",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(out, TransferOutcome::Applied { moved: vec!["dana".into(), "eli".into()] });
        assert_eq!(home(&tx, "dana"), "office-of-the-ceo");
        assert_eq!(home(&tx, "eli"), "office-of-the-ceo");
        ordinals(&tx);
        tx.commit().unwrap();
    }

    #[test]
    fn move_members_is_all_or_nothing_when_one_is_not_a_member() {
        let mut conn = open();
        seed_eng_workers(&conn);
        let tx = conn.transaction().unwrap();
        // cos is homed in office-of-the-ceo, not eng → not-a-member fails the batch.
        let out = move_department_members(
            &tx,
            "acme",
            "eng",
            "office-of-the-ceo",
            &["dana".to_string(), "cos".to_string()],
            "dept-move:2",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(out, TransferOutcome::Refused { reason: TransferRefusal::NotAMember });
        assert_eq!(home(&tx, "dana"), "eng", "nothing moved — all-or-nothing");
        tx.commit().unwrap();
    }

    /// #751/P3: an EMPTY batch means "every ordinary member of the source".
    ///
    /// The caller that used to enumerate this set was a CLI that read the
    /// manifest first, so the batch it sent was one commit stale and carried a
    /// second copy of "who counts as a member". Both are gone: the set is
    /// derived inside this transaction.
    #[test]
    fn move_members_derives_every_ordinary_member_when_the_batch_is_empty() {
        let mut conn = open();
        seed_eng_workers(&conn);
        let tx = conn.transaction().unwrap();
        let out = move_department_members(
            &tx,
            "acme",
            "eng",
            "office-of-the-ceo",
            &[],
            "dept-move:derived",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        // bo HEADS eng and is deliberately absent: the source is never left
        // headless, which is exactly what the tool surface promises.
        assert_eq!(out, TransferOutcome::Applied { moved: vec!["dana".into(), "eli".into()] });
        assert_eq!(home(&tx, "dana"), "office-of-the-ceo");
        assert_eq!(home(&tx, "eli"), "office-of-the-ceo");
        assert_eq!(home(&tx, "bo"), "eng", "the head stays");
        ordinals(&tx);
        tx.commit().unwrap();
    }

    /// The derived batch can never be refused for containing somebody it should
    /// not have: the exclusions are exactly `validate_mover`'s refusals. There
    /// were three until 2026-08-13; the on-loan one went with the concept, so
    /// the departed exclusion is asserted alone.
    #[test]
    fn move_members_derivation_skips_the_departed() {
        let mut conn = open();
        seed_eng_workers(&conn);
        conn.execute(
            "INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','gone','Gone','Eng','build','worker','departed','eng',5,'t','t')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = move_department_members(
            &tx,
            "acme",
            "eng",
            "office-of-the-ceo",
            &[],
            "dept-move:derived-2",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(out, TransferOutcome::Applied { moved: vec!["dana".into(), "eli".into()] });
        assert_eq!(home(&tx, "gone"), "eng", "a departed person is left in place");
        tx.commit().unwrap();
    }

    /// An EXPLICIT batch is still honoured untouched — the derivation is a
    /// default, not an override.
    #[test]
    fn move_members_honours_an_explicit_batch_rather_than_deriving_one() {
        let mut conn = open();
        seed_eng_workers(&conn);
        let tx = conn.transaction().unwrap();
        let out = move_department_members(
            &tx,
            "acme",
            "eng",
            "office-of-the-ceo",
            &["dana".to_string()],
            "dept-move:explicit",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(out, TransferOutcome::Applied { moved: vec!["dana".into()] });
        assert_eq!(home(&tx, "eli"), "eng", "eli was not named, so eli did not move");
        tx.commit().unwrap();
    }

    #[test]
    fn move_members_refuses_listing_the_head() {
        let mut conn = open();
        seed_eng_workers(&conn);
        let tx = conn.transaction().unwrap();
        // bo heads eng → head-needs-successor fails the whole batch.
        let out = move_department_members(
            &tx,
            "acme",
            "eng",
            "office-of-the-ceo",
            &["dana".to_string(), "bo".to_string()],
            "dept-move:3",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(out, TransferOutcome::Refused { reason: TransferRefusal::HeadNeedsSuccessor });
        assert_eq!(home(&tx, "dana"), "eng");
        tx.commit().unwrap();
    }
}

#[cfg(test)]
mod offboard_tests {
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;
    use rusqlite::Connection;

    /// Open a FULL-schema in-memory company (so the real typed accessors write
    /// every column) and seed a minimal exec-root org:
    ///   executive (root, head ada=CEO)
    ///     ├─ office-of-the-ceo (head cos = chief-of-staff)
    ///     └─ eng (head bo = a normal report)
    /// ada & cos are executive-root protected; bo is the normal shutdown target.
    /// FKs OFF: this unit exercises org_ops' composition, not the manifest FKs.
    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('acme','executive',NULL,'Executive','company','active','ada',0,'t','t'), ('acme','office-of-the-ceo','executive','Office of the CEO','department','active','cos',1,'t','t'), ('acme','eng','executive','Engineering','department','active','bo',2,'t','t');
             INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','ada','Ada','CEO','lead','executive','active','executive',0,'t','t'), ('acme','cos','Cos','Chief of Staff','support','head','active','office-of-the-ceo',1,'t','t'), ('acme','bo','Bo','Engineer','build','worker','active','eng',2,'t','t');",
        )
        .expect("seed");
        conn
    }

    /// WHO FIRED WHOM, ENFORCED. `offboard_person` took an actor and asked
    /// nothing of it: any person could fire any other, and the ledger recorded
    /// whatever name the caller supplied. `hire` was bound and `offboard` was
    /// not — the authz audit's sharpest asymmetry, closed by track B1.
    ///
    /// The fixture is a CEO over office-of-the-ceo and eng, so `cos` (who
    /// heads the CEO's office) and `bo` (an eng worker) are in sibling
    /// subtrees: neither manages the other, and both exist.
    #[test]
    fn a_person_cannot_fire_somebody_outside_their_own_subtree() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = offboard_person(&tx, "acme", "bo", "t1", "cos").unwrap();
        assert_eq!(
            out,
            OffboardOutcome::Refused { reason: OffboardRefusal::ActorOutOfScope },
            "a head of another department must not be able to fire this worker"
        );
        // AND NOTHING WAS WRITTEN: a refusal that half-applied would be worse
        // than no check.
        let employment: String = tx
            .query_row("SELECT employment_state FROM people WHERE id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(employment, "active");
        tx.commit().unwrap();
    }

    /// The other side, so the refusal above is not "everything is refused":
    /// the CEO manages the whole company, and every actor that is NOT a person
    /// row still passes — `operator`, this corpus's other placeholder `op`, and
    /// the empty pre-authentication actor. That set is the evidence for the
    /// rule: `actor` is a free-form audit string, not a principal.
    #[test]
    fn the_ceo_the_operator_and_an_unauthenticated_caller_can_all_still_fire() {
        for actor in ["ada", "operator", "op", ""] {
            let mut conn = open();
            let tx = conn.transaction().unwrap();
            let out = offboard_person(&tx, "acme", "bo", "t1", actor).unwrap();
            assert_eq!(out, OffboardOutcome::Applied, "actor {actor:?} must be allowed to fire");
            tx.commit().unwrap();
        }
    }

    #[test]
    fn offboard_departs_a_worker_retaining_the_row() {
        let mut conn = open(); // ada=CEO(exec), cos=office, bo=eng worker (full schema)
        conn.execute("INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo')", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out = offboard_person(&tx, "acme", "bo", "t1", "operator").unwrap();
        assert_eq!(out, OffboardOutcome::Applied);

        // employment departed; placement retained; ROW RETAINED.
        let (emp, placement): (String, String) = tx
            .query_row(
                "SELECT employment_state, department_id FROM people WHERE id='bo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(emp, "departed");
        assert_eq!(placement, "eng", "an offboard leaves the person where they were");
        let retained: i64 =
            tx.query_row("SELECT COUNT(*) FROM people WHERE id='bo'", [], |r| r.get(0)).unwrap();
        assert_eq!(retained, 1, "departed row is RETAINED, never deleted");

        // The GRACEFUL offboard transition: `awaiting_handoff` — the departure
        // does not apply until the person releases it (the e2e #39-followup
        // contract). The whole-ledger validator admits it: real placement,
        // non-empty reason, a real deadline.
        let (action, status, reason): (String, String, String) = tx
            .query_row(
                "SELECT action, status, reason FROM transitions WHERE person_id='bo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(action, "offboard");
        assert_eq!(status, "awaiting_handoff");
        assert!(!reason.trim().is_empty(), "validate requires a non-empty reason");
        let (from_home, deadline): (String, String) = tx
            .query_row("SELECT placement_department_id, handoff_deadline_at FROM transitions WHERE person_id='bo'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(from_home, "eng");
        assert!(!deadline.is_empty(), "a bounded handoff owes a real deadline");
        let desired: i64 = tx
            .query_row(
                "SELECT last_desired_active FROM person_activity WHERE person_id='bo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(desired, 0);
        let staffed: i64 = tx.query_row("SELECT COUNT(*) FROM staffing_history WHERE person_id='bo' AND action='offboarded'", [], |r| r.get(0)).unwrap();
        assert_eq!(staffed, 1);
        // The launch-intent fence stays for exactly the handoff window: the
        // reconcile's graceful machinery can only be released by a person who
        // can still run. A departed person is inert under
        // `operationalPerson` regardless, and the offboard lifecycle owns the
        // withdrawal when the handoff applies.
        let fenced: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE person_id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fenced, 1, "the fence survives until the handoff completes");
        tx.commit().unwrap();
    }

    #[test]
    fn offboard_adopts_a_released_transition_instead_of_superseding_it() {
        // The staffing lifecycle RELEASES the transition before the op runs, so
        // the open offboard transition is already `ready`. Superseding it and
        // minting a fresh `awaiting_handoff` row throws that release away and
        // puts the person back at the start of a grace window they can never
        // finish: this very op marks them departed, so nobody is left who can
        // release the new row, and the reconcile therefore retains the pane
        // forever (the live staffing wedge). The op must ADOPT the ready row as
        // its graceful transition.
        let mut conn = open();
        conn.execute("INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo')", []).unwrap();
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, reason, requested_at, handoff_deadline_at, placement_department_id) VALUES('acme','transition:7:bo:offboard','bo','offboard','ready','offboarded bo','t0','t9','eng')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = offboard_person(&tx, "acme", "bo", "t1", "operator").unwrap();
        assert_eq!(out, OffboardOutcome::Applied);

        // The ready row is ADOPTED, not cancelled; no second transition is minted.
        let transitions: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE person_id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(transitions, 1, "adoption mints no replacement transition");
        let status: String = tx
            .query_row(
                "SELECT status FROM transitions WHERE id='transition:7:bo:offboard'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            status, "ready",
            "a released transition is consumed by the reconcile, never superseded"
        );
        let pointer: String = tx
            .query_row(
                "SELECT active_transition_id FROM person_activity WHERE person_id='bo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            pointer, "transition:7:bo:offboard",
            "desired-off points at the ADOPTED transition"
        );
        let departed: String = tx
            .query_row("SELECT employment_state FROM people WHERE id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(departed, "departed");
        let staffed: i64 = tx.query_row("SELECT COUNT(*) FROM staffing_history WHERE person_id='bo' AND action='offboarded'", [], |r| r.get(0)).unwrap();
        assert_eq!(staffed, 1);
        tx.commit().unwrap();
    }

    /// THE FENCE SURVIVES AN OFFBOARD, deliberately, and this is the test that
    /// says so out loud.
    ///
    /// `offboard_person`'s doc claimed for a long time that it cleared the
    /// launch-intent fence. It never did, and it must not: every branch leaves
    /// the person holding an open offboard transition, the departure does not
    /// apply until they RELEASE it, and they have to be running to write it.
    /// The reconcile cancels a pending structural transition for anybody the
    /// fence does not admit, so clearing here would have the offboard abandon
    /// its own handoff.
    #[test]
    fn offboard_keeps_the_fence_while_the_handoff_is_open_because_the_person_must_write_it() {
        let mut conn = open();
        conn.execute("INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo')", []).unwrap();
        let tx = conn.transaction().unwrap();

        let out = offboard_person(&tx, "acme", "bo", "t1", "operator").unwrap();
        assert_eq!(out, OffboardOutcome::Applied);

        let fenced: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM launch_intent WHERE slug='acme' AND person_id='bo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            fenced, 1,
            "a departed person keeps their authorization until the handoff they were fired with \
             goes terminal -- de-authorizing them here abandons it"
        );

        // ...and the thing they are being kept alive FOR is really open.
        let status: String = tx
            .query_row(
                "SELECT status FROM transitions WHERE slug='acme' AND person_id='bo' \
                 AND action='offboard'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "awaiting_handoff", "the open handoff is what the fence is holding for");
    }

    #[test]
    fn offboard_still_supersedes_an_open_transition_that_was_never_released() {
        // An open offboard transition nobody has released yet is stale by
        // definition — it was opened for some earlier intent and no release is
        // riding on it — so the op keeps its established supersede-and-remint
        // shape: a fresh bounded window belonging to THIS offboard.
        let mut conn = open();
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, reason, requested_at, handoff_deadline_at, placement_department_id) VALUES('acme','transition:7:bo:offboard','bo','offboard','awaiting_handoff','offboarded bo','t0','t9','eng')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = offboard_person(&tx, "acme", "bo", "t1", "operator").unwrap();
        assert_eq!(out, OffboardOutcome::Applied);

        let (status, reason): (String, String) = tx
            .query_row(
                "SELECT status, reason FROM transitions WHERE id='transition:7:bo:offboard'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(reason, "superseded-by-offboard:bo");
        let fresh: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE person_id='bo' AND status='awaiting_handoff'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fresh, 1, "an unreleased transition is replaced with a fresh bounded window");
        tx.commit().unwrap();
    }
    /// INVERTED on 2026-08-13 for `cos`, and kept rather than deleted.
    ///
    /// It asserted that office-of-the-ceo staff were unfireable purely for
    /// living there. The operator's corrected ruling is that a head may act on
    /// anyone in its own subtree and the CEO holds every tree, so `cos` now
    /// meets the SAME guards as anybody else — and trips
    /// `head-needs-successor`, because `cos` heads office-of-the-ceo. That is a
    /// real refusal about a real fact (do not leave a unit headless) rather
    /// than one about where the person sits, and it is asserted here so the
    /// difference is on the record.
    #[test]
    fn offboard_refuses_the_ceo_alone_and_judges_everyone_else_on_the_facts() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let code = |who: &str| match offboard_person(&tx, "acme", who, "t1", "op").unwrap() {
            OffboardOutcome::Refused { reason } => reason.code().to_string(),
            other => panic!("expected refusal, got {other:?}"),
        };
        assert_eq!(code("ada"), "exec-root-protected", "the CEO is never fired");
        assert_eq!(
            code("cos"),
            "head-needs-successor",
            "a chief of staff is refused for HEADING a unit, not for living beside the CEO"
        );
        assert_eq!(code("ghost"), "unknown-person");
        tx.commit().unwrap();
    }

    /// A plain member of `office-of-the-ceo`, heading nothing, is fireable.
    /// This is the half the old whole-root guard made unreachable.
    #[test]
    fn offboard_accepts_a_plain_member_of_the_ceos_own_office() {
        let mut conn = open();
        conn.execute(
            "INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','aide','Aide','Aide','support','worker','active','office-of-the-ceo',9,'t','t')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            offboard_person(&tx, "acme", "aide", "t1", "op").unwrap(),
            OffboardOutcome::Applied,
            "living in the CEO's office is not a protection"
        );
        tx.commit().unwrap();
    }

    /// Production-bug restoration (U6 casualty table row #1b): offboarding a
    /// department head used to refuse UNCONDITIONALLY; the atomic-reorg
    /// migration kept only the headship half
    /// (`offboard_refuses_a_goal_owning_manager_head_needs_successor` above),
    /// so a head who owns no manager goals fell through to a silent no-op —
    /// worse, the department's `head_person_id` was left pointing at the
    /// now-departed person (state corruption, not just a no-op). `bo` in the
    /// shared `open()` fixture is `eng`'s `head_person_id` in the SQL seed
    /// but its own `people.kind` is deliberately `worker` (several other
    /// tests in this module rely on `bo` behaving as an ordinary worker) —
    /// so this test flips `kind` to `head` explicitly to seed a GENUINE head,
    /// proving the guard is keyed on `people.kind` (matching the transfer's
    /// existing check) and not on the raw `departments.head_person_id`
    /// pointer alone.
    #[test]
    fn offboard_refuses_a_non_goal_owning_department_head() {
        let mut conn = open();
        conn.execute("UPDATE people SET kind = 'head' WHERE slug = 'acme' AND id = 'bo'", [])
            .unwrap();
        let tx = conn.transaction().unwrap();
        match offboard_person(&tx, "acme", "bo", "t1", "op").unwrap() {
            OffboardOutcome::Refused { reason } => {
                assert_eq!(reason.code(), "head-needs-successor")
            }
            other => panic!("expected head-needs-successor, got {other:?}"),
        }
        // Zero writes: bo still active, eng's head_person_id UNCHANGED — the
        // exact state corruption the migration introduced (a departed head
        // left on record) cannot occur if nothing was written at all.
        let emp: String = tx
            .query_row("SELECT employment_state FROM people WHERE id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(emp, "active");
        let head: String = tx
            .query_row("SELECT head_person_id FROM departments WHERE id='eng'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(head, "bo");
        tx.commit().unwrap();
    }

    // U7 (operator-mandated, non-negotiable): the refusal test above proves
    // the GUARD fires; this proves the STATE it protects stays coherent. The
    // operator's own framing is why this second assertion exists as its own
    // test rather than a few more lines on the one above: a refusal test
    // alone would have passed just as trivially against a "fix" that only
    // swallowed the error without truly refusing before any write, and the
    // original bug was exactly that shape (silent no-op, not silent
    // exception). Deliberately broader than the sibling test's single
    // `head_person_id FROM departments WHERE id='eng'` check: this queries
    // the WHOLE company for any department whose recorded head has departed,
    // so it keeps catching the invariant even if a future guard regression
    // picks a different department or a different departed person.
    #[test]
    fn offboard_of_a_department_head_leaves_no_department_pointing_at_a_departed_person() {
        let mut conn = open();
        conn.execute("UPDATE people SET kind = 'head' WHERE slug = 'acme' AND id = 'bo'", [])
            .unwrap();
        let tx = conn.transaction().unwrap();
        let out = offboard_person(&tx, "acme", "bo", "t1", "op").unwrap();
        assert!(matches!(out, OffboardOutcome::Refused { .. }), "expected a refusal, got {out:?}");

        let dangling: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM departments d JOIN people p \
                 ON p.slug = d.slug AND p.id = d.head_person_id \
                 WHERE d.slug = 'acme' AND p.employment_state = 'departed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            dangling, 0,
            "no department in the company may reference a departed person as its head"
        );
        tx.commit().unwrap();
    }

    /// A FIRING IS RECORDED WITH THE NAME OF WHOEVER FIRED. Nobody types a
    /// reason — the requirement is gone — and the durable `staffing_history`
    /// row plus the graceful-offboard `transitions` row both carry the line
    /// this op authors. The old synthetic `offboarded <person>` named nobody
    /// at all, which is what makes the actor half of this assertion the point.
    #[test]
    fn offboard_authors_a_ledger_line_that_names_the_actor() {
        let mut conn = open();
        conn.execute("INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo')", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out = offboard_person(&tx, "acme", "bo", "t1", "operator").unwrap();
        assert_eq!(out, OffboardOutcome::Applied);

        let staffing_reason: String = tx
            .query_row(
                "SELECT reason FROM staffing_history WHERE person_id='bo' AND action='offboarded'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(staffing_reason, "offboarded by operator");
        assert_ne!(
            staffing_reason, "offboarded bo",
            "the line names the actor, never the synthetic placeholder"
        );

        let transition_reason: String = tx
            .query_row("SELECT reason FROM transitions WHERE person_id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(transition_reason, "offboarded by operator");
        tx.commit().unwrap();
    }
}

#[cfg(test)]
mod hire_tests {
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;
    use rusqlite::Connection;

    fn ordinals(conn: &Connection) -> Vec<i64> {
        let mut stmt =
            conn.prepare("SELECT ordinal FROM people WHERE slug='acme' ORDER BY ordinal").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    fn desired(conn: &Connection, person: &str) -> Option<i64> {
        conn.query_row(
            "SELECT last_desired_active FROM person_activity WHERE slug='acme' AND person_id=?1",
            rusqlite::params![person],
            |r| r.get(0),
        )
        .ok()
    }

    /// Open a FULL-schema in-memory company (so the real typed accessors write
    /// every column) and seed a minimal exec-root org:
    ///   executive (root, head ada=CEO)
    ///     ├─ office-of-the-ceo (head cos = chief-of-staff)
    ///     └─ eng (head bo = a normal report)
    /// ada & cos are executive-root protected; bo is the normal shutdown target.
    /// FKs OFF: this unit exercises org_ops' composition, not the manifest FKs.
    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO org_settings(slug, display_slug, supervision_interval_ms, acknowledgement_timeout_ms, acknowledgement_retry_limit, replacement_limit) VALUES('acme', 'acme',60000,30000,3,3); INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('acme','executive',NULL,'Executive','company','active','ada',0,'t','t'), ('acme','office-of-the-ceo','executive','Office of the CEO','department','active','cos',1,'t','t'), ('acme','eng','executive','Engineering','department','active','bo',2,'t','t');
             INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','ada','Ada','CEO','lead','executive','active','executive',0,'t','t'), ('acme','cos','Cos','Chief of Staff','support','head','active','office-of-the-ceo',1,'t','t'), ('acme','bo','Bo','Engineer','build','worker','active','eng',2,'t','t');",
        )
        .expect("seed");
        conn
    }

    fn seed<'a>(name: &'a str, tools: &'a [String]) -> NewPersonSeed<'a> {
        NewPersonSeed {
            name,
            title: "Engineer",
            mandate: "build",
            kind: PersonKind::Worker,
            employment_state: EmploymentState::Active,
            activation: "resident",
            tools,
            prompts: &[],
        }
    }

    #[test]
    fn independent_hires_after_prior_events_both_persist_with_audit_events() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        // A real unrelated write has already advanced both the audit feed and
        // its sequence allocator.
        apply_and_emit::<rusqlite::Error, _>(&tx, "acme", "t", "other", |_tx| {
            Ok(vec![crate::store::rows_txn::EventTouch::new(
                "person", "x", "upsert", "people", "acme",
            )])
        })
        .unwrap();
        let first = hire_person(&tx, "acme", "zoe", "eng", &seed("Zoe", &[]), "ada", "t1", "chief")
            .unwrap();
        let second =
            hire_person(&tx, "acme", "yuki", "eng", &seed("Yuki", &[]), "ada", "t2", "chief")
                .unwrap();
        assert_eq!(first, HireOutcome::Applied);
        assert_eq!(second, HireOutcome::Applied);
        let people: Vec<String> = tx
            .prepare("SELECT id FROM people WHERE slug='acme' AND id IN ('yuki','zoe') ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(people, vec!["yuki", "zoe"]);
        let events: Vec<String> = tx
            .prepare(
                "SELECT entity_id FROM org_events \
                 WHERE slug='acme' AND entity='person' AND entity_id IN ('yuki','zoe') \
                 ORDER BY entity_id",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(events, vec!["yuki", "zoe"]);
        tx.commit().unwrap();
    }
    #[test]
    fn hire_inserts_person_placement_staffing_and_takes_next_gapless_ordinal() {
        let mut conn = open(); // ada(0), cos(1), bo(2)
        let tx = conn.transaction().unwrap();
        let out = hire_person(
            &tx,
            "acme",
            "zoe",
            "eng",
            &seed("Zoe", &[]),
            "ada",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(out, HireOutcome::Applied);
        // Placement: eng, employment active.
        let (placement, emp, ordinal): (String, String, i64) = tx
            .query_row(
                "SELECT department_id, employment_state, ordinal FROM people WHERE id='zoe'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((placement.as_str(), emp.as_str()), ("eng", "active"));
        assert_eq!(ordinal, 3); // next gapless after 0,1,2
                                // Bijection intact: 0..N with no gaps/dupes.
        assert_eq!(ordinals(&tx), vec![0, 1, 2, 3]);
        // Staffing 'hired' from NULL → eng.
        let (action, from, to): (String, Option<String>, Option<String>) = tx
            .query_row(
                "SELECT action, from_department_id, to_department_id FROM staffing_history \
                 WHERE person_id='zoe'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(action, "hired");
        assert_eq!(from, None);
        assert_eq!(to.as_deref(), Some("eng"));
        // The activity seed is written desired-off and stays that way in the
        // transaction: `activity::reconcile` owns `last_desired_active`, and a
        // pre-set flag would suppress the `Requested` reason the fence exists to
        // raise. This assertion is unchanged for that reason, NOT because a hire
        // still starts nobody -- see the fence assertion below.
        assert_eq!(desired(&tx, "zoe"), Some(0));
        let activity: (String, String, i64) = tx
            .query_row(
                "SELECT last_employment_state, last_department_id, last_operational \
                 FROM person_activity WHERE slug='acme' AND person_id='zoe'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(activity, ("active".into(), "eng".into(), 1));
        // BIRTH STATE (operator, 2026-08-10): hiring somebody IS the decision to
        // bring them up, so the hire commits its launch fence in the SAME
        // transaction. By value, and against the whole table: the fence names
        // Zoe and nobody else, so an over-broad write is a failure here too.
        let fenced: Vec<String> = tx
            .prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(fenced, vec!["zoe".to_string()]);
        // The fence is exactly the structure the converge cycle reads: the
        // reconstructed launch-intent document names the new hire, so
        // `project_activity_fence` raises `Requested` for her on the next pass
        // and the runtime projection starts her (the far half of that path is
        // `cycle::tests::an_explicit_launch_intent_starts_its_person_from_a_zero_pane_company`).
        let intent = launch_intent_rows::reconstruct(&tx, "acme", "acme").unwrap();
        assert!(
            intent.person_ids.contains(&"zoe".to_string()),
            "the hire's start decision must be visible to the reconciler, not only in the table"
        );
        tx.commit().unwrap();
    }

    /// The other half of the birth-state rule, and the one an over-eager fence
    /// would break: a BENCHED seed is durable and stopped BY ITS OWN
    /// DEFINITION. Fencing it would start somebody the caller explicitly said is
    /// not staffed, which is the same over-granting failure as an absent fence
    /// that authorizes everybody.
    #[test]
    fn hiring_a_benched_person_records_no_launch_fence() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let mut benched = seed("Zoe", &[]);
        benched.employment_state = EmploymentState::Benched;
        let out = hire_person(
            &tx,
            "acme",
            "zoe",
            "eng",
            &benched,
            "ada",
            "2026-07-25T00:00:00.000Z",
            "chief",
        )
        .unwrap();
        assert_eq!(out, HireOutcome::Applied);
        let employment: String = tx
            .query_row("SELECT employment_state FROM people WHERE id='zoe'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(employment, "benched", "the person is durable, and durably not staffed");
        let fenced: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fenced, 0, "a benched hire is fenceless and stays stopped");
        tx.commit().unwrap();
    }

    #[test]
    fn hire_into_non_last_department_normalizes_people_order() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            hire_person(
                &tx,
                "acme",
                "zoe",
                "office-of-the-ceo",
                &seed("Zoe", &[]),
                "ada",
                "t1",
                "ada",
            )
            .unwrap(),
            HireOutcome::Applied,
        );
        let people: Vec<String> = tx
            .prepare("SELECT id FROM people WHERE slug='acme' ORDER BY ordinal")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            people,
            vec!["ada", "cos", "zoe", "bo"],
            "the hire is grouped after its home-department head, before later departments",
        );
        tx.commit().unwrap();
    }
    #[test]
    fn hire_refuses_a_duplicate_person_id_and_writes_nothing() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = hire_person(&tx, "acme", "bo", "eng", &seed("Bo II", &[]), "ada", "t1", "chief")
            .unwrap();
        assert_eq!(out, HireOutcome::Refused { reason: HireRefusal::DuplicatePersonId });
        // bo unchanged, no new staffing row.
        let n: i64 =
            tx.query_row("SELECT COUNT(*) FROM people WHERE id='bo'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let s: i64 =
            tx.query_row("SELECT COUNT(*) FROM staffing_history", [], |r| r.get(0)).unwrap();
        assert_eq!(s, 0);
        tx.commit().unwrap();
    }
    #[test]
    fn hire_refuses_an_invalid_seed_with_the_field_path() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let out = hire_person(&tx, "acme", "zoe", "eng", &seed("   ", &[]), "ada", "t1", "chief")
            .unwrap();
        match out {
            HireOutcome::Refused { reason } => {
                assert_eq!(reason.code(), "invalid-seed");
                assert!(reason.detail().contains("name"));
            }
            other => panic!("expected invalid-seed, got {other:?}"),
        }
        tx.commit().unwrap();
    }
    #[test]
    fn hire_refuses_unknown_and_paused_destination() {
        let mut conn = open();
        conn.execute("UPDATE departments SET state='paused' WHERE id='eng'", []).unwrap();
        let tx = conn.transaction().unwrap();
        let unknown =
            hire_person(&tx, "acme", "zoe", "ghost", &seed("Zoe", &[]), "ada", "t1", "chief")
                .unwrap();
        assert_eq!(unknown, HireOutcome::Refused { reason: HireRefusal::UnknownDepartment });
        let paused =
            hire_person(&tx, "acme", "zoe", "eng", &seed("Zoe", &[]), "ada", "t1", "chief")
                .unwrap();
        assert_eq!(paused, HireOutcome::Refused { reason: HireRefusal::DestinationPaused });
        // Nobody inserted.
        let n: i64 =
            tx.query_row("SELECT COUNT(*) FROM people WHERE id='zoe'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        tx.commit().unwrap();
    }

    #[test]
    fn hire_refuses_an_active_destination_below_a_paused_ancestor() {
        let mut conn = open();
        conn.execute_batch(
            "INSERT INTO departments(slug,id,parent_id,name,kind,state,head_person_id,ordinal,created_at,updated_at) \
             VALUES('acme','eng-platform','eng','Platform','department','active','ph',3,'t','t'); \
             UPDATE departments SET state='paused' WHERE id='eng';",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = hire_person(
            &tx,
            "acme",
            "zoe",
            "eng-platform",
            &seed("Zoe", &[]),
            "ada",
            "t1",
            "chief",
        )
        .unwrap();
        assert_eq!(out, HireOutcome::Refused { reason: HireRefusal::DestinationPaused });
        let n: i64 =
            tx.query_row("SELECT COUNT(*) FROM people WHERE id='zoe'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        tx.commit().unwrap();
    }

    #[test]
    fn hire_persists_the_complete_normalized_person_seed() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let tools = vec!["read".to_string(), "bash".to_string()];
        let prompts = vec!["prompts/reviewer.md".to_string()];
        let full = NewPersonSeed {
            name: "Zoe",
            title: "Senior Engineer",
            mandate: "Build and review",
            kind: PersonKind::Worker,
            employment_state: EmploymentState::Benched,
            activation: "on-demand",
            tools: &tools,
            prompts: &prompts,
        };
        assert_eq!(
            hire_person(&tx, "acme", "zoe", "eng", &full, "ada", "t1", "ada").unwrap(),
            HireOutcome::Applied,
        );
        /// The scalar `people` columns asserted below, in SELECT order.
        type PeopleScalars = (String, String);
        let scalars: PeopleScalars = tx
            .query_row(
                "SELECT employment_state, activation \
                 FROM people WHERE slug='acme' AND id='zoe'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(scalars, ("benched".into(), "on-demand".into()));
        let stored_tools: Vec<String> = tx
            .prepare(
                "SELECT tool FROM person_tools WHERE slug='acme' AND person_id='zoe' ORDER BY ordinal",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(stored_tools, tools);
        // The resource half of "complete" is gone (§4e): the seed used to carry
        // a skill, an extension and a package, and this asserted all three
        // landed in `person_resources`. There is no such column, table or field
        // to persist, so what remains complete is the scalars, the tool grant
        // and the prompt templates.
        let stored_prompts: Vec<String> = tx
            .prepare(
                "SELECT template FROM person_prompts WHERE slug='acme' AND person_id='zoe' \
                 ORDER BY ordinal",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(stored_prompts, prompts);
        tx.commit().unwrap();
    }

    // TOMBSTONE (chief-home-is-cwd §4e): `selecting_a_resource_needs_no_rationale`
    //   pinned that a hire naming a skill, an extension and a package was
    //   applied and stored all three `person_resources` rows. #1093 deleted the
    //   justification the selection required; §4e deletes the selection itself,
    //   so there is no call left to make and no row left to count.

    /// A malformed seed refuses BEFORE any row or event is written.
    ///
    /// The subject used to be a missing `resourceRationale`, then a blank
    /// resource id once #1093 deleted that requirement. Both subjects are gone
    /// — a seed carries no resource field at all (§4e) — so the no-write
    /// invariant is pinned on the malformed entry that survives: a prompt
    /// template that is not a repo-relative `prompts/<name>.md` path.
    #[test]
    fn malformed_complete_seed_is_a_no_write_refusal() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let prompts = vec!["../escape.md".to_string()];
        let invalid = NewPersonSeed { prompts: &prompts, ..seed("Zoe", &[]) };
        let out = hire_person(&tx, "acme", "zoe", "eng", &invalid, "ada", "t1", "ada").unwrap();
        assert!(matches!(
            out,
            HireOutcome::Refused {
                reason: HireRefusal::InvalidSeed { ref field, .. }
            } if field == "prompts[0]"
        ));
        let rows: i64 = tx
            .query_row("SELECT COUNT(*) FROM people WHERE slug='acme' AND id='zoe'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let events: i64 = tx
            .query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!((rows, events), (0, 0));
        tx.commit().unwrap();
    }

    #[test]
    fn stale_manager_scope_refuses_hire_without_writes() {
        let mut conn = open();
        conn.execute("UPDATE people SET kind='worker' WHERE slug='acme' AND id='bo'", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out =
            hire_person(&tx, "acme", "zoe", "eng", &seed("Zoe", &[]), "bo", "t1", "bo").unwrap();
        assert_eq!(out, HireOutcome::Refused { reason: HireRefusal::RequesterOutOfScope });
        let people: i64 = tx
            .query_row("SELECT COUNT(*) FROM people WHERE slug='acme' AND id='zoe'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let events: i64 = tx
            .query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |row| row.get(0))
            .unwrap();
        assert_eq!((people, events), (0, 0));
        tx.commit().unwrap();
    }
}

#[cfg(test)]
mod pause_tests {
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;
    use rusqlite::Connection;

    /// Open a FULL-schema in-memory company (so the real typed accessors write
    /// every column) and seed a minimal exec-root org:
    ///   executive (root, head ada=CEO)
    ///     ├─ office-of-the-ceo (head cos = chief-of-staff)
    ///     └─ eng (head bo = a normal report)
    /// ada & cos are executive-root protected; bo is the normal shutdown target.
    /// FKs OFF: this unit exercises org_ops' composition, not the manifest FKs.
    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('acme','executive',NULL,'Executive','company','active','ada',0,'t','t'), ('acme','office-of-the-ceo','executive','Office of the CEO','department','active','cos',1,'t','t'), ('acme','eng','executive','Engineering','department','active','bo',2,'t','t');
             INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','ada','Ada','CEO','lead','executive','active','executive',0,'t','t'), ('acme','cos','Cos','Chief of Staff','support','head','active','office-of-the-ceo',1,'t','t'), ('acme','bo','Bo','Engineer','build','worker','active','eng',2,'t','t');",
        )
        .expect("seed");
        conn
    }
    fn dept_state(conn: &Connection, dept: &str) -> String {
        conn.query_row(
            "SELECT state FROM departments WHERE slug='acme' AND id=?1",
            rusqlite::params![dept],
            |r| r.get(0),
        )
        .unwrap()
    }
    fn desired(conn: &Connection, person: &str) -> Option<i64> {
        conn.query_row(
            "SELECT last_desired_active FROM person_activity WHERE slug='acme' AND person_id=?1",
            rusqlite::params![person],
            |r| r.get(0),
        )
        .ok()
    }
    /// Seed a descendant department `eng-sub` under `eng` with a worker `sub1`,
    /// give both `bo` (direct member) and `sub1` (descendant member) a
    /// launch_intent fence + an open transition — the paused-subtree sweep fodder.
    fn seed_subtree_sweep_fodder(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('acme','eng-sub','eng','Eng Sub','department','active','sub1',3,'t','t'); INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','sub1','Sub','Engineer','build','worker','active','eng-sub',3,'t','t'); INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo'),('acme','sub1'); INSERT INTO transitions(slug, id, person_id, action, status, requested_at) VALUES ('acme','open-bo','bo','park','ready','t0'), ('acme','open-sub1','sub1','park','ready','t0');",
        )
        .unwrap();
    }

    #[test]
    fn a_paused_department_reads_as_the_transfer_destination_paused_gate() {
        // Compatibility with the transfer verb (@072ab4e4): its `destination-paused`
        // guard reads `organization_rows::department_state` and refuses on 'paused'.
        // Pausing a dept flips exactly that column, so a transfer INTO it refuses.
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        pause_department(&tx, "acme", "eng", "t1", "op").unwrap();
        assert_eq!(
            organization_rows::department_state(&tx, "acme", "eng").unwrap().as_deref(),
            Some("paused"),
            "the destination-paused gate the transfer verb reads must see 'paused'"
        );
        tx.commit().unwrap();
    }
    #[test]
    fn pause_already_paused_refuses_idempotent() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        pause_department(&tx, "acme", "eng", "t1", "op").unwrap();
        match pause_department(&tx, "acme", "eng", "t2", "op").unwrap() {
            PauseOutcome::Refused { reason } => assert_eq!(reason.code(), "already-paused"),
            other => panic!("expected already-paused, got {other:?}"),
        }
        // Only the first pause emitted an event (redundant pause churns nothing).
        let events: i64 = tx
            .query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 1);
        tx.commit().unwrap();
    }
    #[test]
    fn pause_applies_against_current_rows_even_after_unrelated_event() {
        let mut conn = open();
        conn.execute(
            "INSERT INTO org_events(slug, seq, entity, entity_id, op, at) \
             VALUES('acme', 1, 'department', 'x', 'noop', 't')",
            [],
        )
        .unwrap();
        // D2 sequence allocation is counter-backed (see
        // `unrelated_prior_event_does_not_reject_a_direct_transfer`); seed the
        // counter alongside the prior event exactly as a real committed writer
        // would, so this checks the "unrelated event doesn't block pause"
        // semantics rather than a deliberately corrupt feed.
        conn.execute("INSERT INTO counters(name, value) VALUES('org-events:acme', 1)", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out = pause_department(&tx, "acme", "eng", "t1", "op").unwrap();
        assert_eq!(out, PauseOutcome::Applied);
        assert_eq!(dept_state(&tx, "eng"), "paused");
        tx.commit().unwrap();
    }
    #[test]
    fn pause_sweeps_even_after_unrelated_event() {
        let mut conn = open();
        seed_subtree_sweep_fodder(&conn);
        conn.execute(
            "INSERT INTO org_events(slug, seq, entity, entity_id, op, at) \
             VALUES('acme', 1, 'department', 'x', 'noop', 't')",
            [],
        )
        .unwrap();
        // D2 sequence allocation is counter-backed; seed the counter alongside
        // the prior event exactly as a real committed writer would (see
        // `unrelated_prior_event_does_not_reject_a_direct_transfer`).
        conn.execute("INSERT INTO counters(name, value) VALUES('org-events:acme', 1)", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out = pause_department(&tx, "acme", "eng", "t1", "op").unwrap();
        assert_eq!(out, PauseOutcome::Applied);
        // Direct pause always applies to the current rows: both fences are
        // withdrawn and both transitions superseded in this same transaction.
        let fences: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE slug='acme' AND person_id IN ('bo','sub1')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fences, 0);
        let open_count: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE status='ready'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(open_count, 0);
        assert_eq!(dept_state(&tx, "eng"), "paused");
        tx.commit().unwrap();
    }
    /// THE ANTI-STRANDING GUARANTEE, and the reason narrowing the pause guard
    /// is safe at all.
    ///
    /// Pausing a unit stops every member of its subtree. While the whole
    /// executive root refused to pause, the CEO could not be reached by that
    /// sweep; once `office-of-the-ceo` became an ordinary department, a CEO
    /// HOMED there would have been stopped as a side effect of stopping a team
    /// — the company's own supervisor taken down by a routine operation. The
    /// exemption therefore moved from the REFUSAL to the SWEEP.
    ///
    /// The fixture is the genesis shape that makes this reachable: the CEO
    /// assigned to `office-of-the-ceo`, alongside an ordinary member. The unit
    /// pauses, the member is stopped, and the CEO is untouched — which is the
    /// operator's sentence made executable: "shut down a department, keep him
    /// around".
    #[test]
    fn pausing_the_unit_the_ceo_sits_in_stops_everyone_except_the_ceo() {
        let mut conn = open();
        conn.execute_batch(
            "UPDATE people SET department_id='office-of-the-ceo' WHERE slug='acme' AND id='ada'; INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','aide','Aide','Aide','support','worker','active','office-of-the-ceo',3,'t','t'); INSERT INTO launch_intent(slug, person_id) VALUES('acme','ada'),('acme','aide');",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert!(
            matches!(
                pause_department(&tx, "acme", "office-of-the-ceo", "t1", "op").unwrap(),
                PauseOutcome::Applied
            ),
            "the unit the CEO sits in is still an ordinary department"
        );
        assert_eq!(dept_state(&tx, "office-of-the-ceo"), "paused");

        // The launch fence is the durable "may run" bit the sweep deletes.
        let fenced = |person: &str| -> i64 {
            tx.query_row(
                "SELECT COUNT(*) FROM launch_intent WHERE slug='acme' AND person_id=?1",
                [person],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(fenced("aide"), 0, "an ordinary member of a paused unit is stopped");
        assert_eq!(
            fenced("ada"),
            1,
            "the CEO keeps running: the sweep must never reach the one person nobody may act on"
        );
        tx.commit().unwrap();
    }

    /// INVERTED on 2026-08-13 for `office-of-the-ceo`, and kept rather than
    /// deleted so the reversal is visible where the old rule was asserted.
    ///
    /// Only the COMPANY ROOT still refuses a pause — pausing it would stop the
    /// whole company including the CEO. The CEO's office is an ordinary
    /// department, and "shut down a department, keep him around" is the
    /// operator's own description of what must be possible.
    #[test]
    fn pause_refuses_the_company_root_and_nothing_else() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        match pause_department(&tx, "acme", "executive", "t1", "op").unwrap() {
            PauseOutcome::Refused { reason } => assert_eq!(reason.code(), "exec-root-protected"),
            other => panic!("the company root never pauses, got {other:?}"),
        }
        assert_eq!(dept_state(&tx, "executive"), "active", "a refusal writes nothing");

        assert!(
            matches!(
                pause_department(&tx, "acme", "office-of-the-ceo", "t1", "op").unwrap(),
                PauseOutcome::Applied
            ),
            "the CEO's office is an ordinary department"
        );
        assert_eq!(dept_state(&tx, "office-of-the-ceo"), "paused");
        tx.commit().unwrap();
    }
    #[test]
    fn pause_refuses_unknown_department_and_writes_nothing() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        match pause_department(&tx, "acme", "ghost", "t1", "op").unwrap() {
            PauseOutcome::Refused { reason } => assert_eq!(reason.code(), "unknown-department"),
            other => panic!("expected unknown-department, got {other:?}"),
        }
        let events: i64 = tx
            .query_row("SELECT COUNT(*) FROM org_events WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 0);
        tx.commit().unwrap();
    }
    #[test]
    fn pause_sets_the_flag_and_touches_the_department_only() {
        let mut conn = open(); // eng is a normal active department
        let tx = conn.transaction().unwrap();
        let out =
            pause_department(&tx, "acme", "eng", "2026-07-25T00:00:00.000Z", "operator").unwrap();
        assert_eq!(out, PauseOutcome::Applied);
        assert_eq!(dept_state(&tx, "eng"), "paused");
        // Exactly one org_events touch: the department upsert (N4 CRUD verb).
        let events: Vec<(String, String, String)> = tx
            .prepare("SELECT entity, entity_id, op FROM org_events WHERE slug='acme' ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            events,
            vec![("department".to_string(), "eng".to_string(), "upsert".to_string())]
        );
        tx.commit().unwrap();
    }
    #[test]
    fn pause_sweeps_the_whole_subtree_clearing_launch_intent_and_superseding_transitions() {
        // norm-n1 ruling: pause DELETEs launch_intent + SUPERSEDEs open transitions
        // for the dept AND its recursive descendants, all IN the same txn (#534).
        let mut conn = open();
        seed_subtree_sweep_fodder(&conn);
        let tx = conn.transaction().unwrap();
        let out = pause_department(&tx, "acme", "eng", "t1", "op").unwrap();
        assert_eq!(out, PauseOutcome::Applied);
        // Both members' launch_intent fences are GONE (direct + descendant).
        let fences: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE slug='acme' AND person_id IN ('bo','sub1')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fences, 0, "both subtree members' launch_intent cleared in-txn");
        // Both pre-existing open transitions are cancelled with the pause
        // supersede marker.
        let cancelled: Vec<(String, String)> = tx
            .prepare("SELECT person_id, reason FROM transitions WHERE id LIKE 'open-%' ORDER BY person_id")
            .unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(
            cancelled,
            vec![
                ("bo".to_string(), "superseded-by-pause:eng".to_string()),
                ("sub1".to_string(), "superseded-by-pause:eng".to_string()),
            ]
        );
        // The same atomic pause leaves a fresh unit-stop audit for each active
        // member: a `cancelled` row carrying the unit-stop intent, written
        // directly in its terminal shape inside the one transaction, so no
        // intermediate open row is ever observable. `abandoned_at` stays NULL
        // — the member was running fine and the unit was stopped out from
        // under them, which is a supersession, not an unreachable release.
        let audits: Vec<(String, String, String, Option<String>)> = tx
            .prepare(
                "SELECT person_id, intent_id, reason, abandoned_at \
                 FROM transitions \
                 WHERE status='cancelled' AND intent_id LIKE 'unit-stop:eng:%' \
                 ORDER BY person_id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(audits.len(), 2);
        for (person, intent, reason, abandoned_at) in audits {
            assert!(matches!(person.as_str(), "bo" | "sub1"));
            assert!(intent.starts_with("unit-stop:eng:transition:"));
            assert_eq!(reason, "superseded-by-pause:eng");
            assert_eq!(abandoned_at, None);
        }
        tx.commit().unwrap();
    }
    #[test]
    fn pause_repairs_the_active_heads_reconcilable_projection_and_resume_keeps_that_head() {
        let mut conn = open();
        conn.execute_batch(
            "INSERT INTO person_activity(slug, person_id, last_desired_active, last_operational, last_department_id, last_employment_state, updated_at) VALUES('acme','bo',1,1,NULL,NULL,'before'); INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo');",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();

        assert_eq!(
            pause_department(&tx, "acme", "eng", "pause-at", "operator").unwrap(),
            PauseOutcome::Applied
        );
        assert_eq!(dept_state(&tx, "eng"), "paused");
        let head: String = tx
            .query_row(
                "SELECT head_person_id FROM departments WHERE slug='acme' AND id='eng'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(head, "bo");
        let projection: (i64, i64, String, String, Option<String>) = tx
            .query_row(
                "SELECT last_desired_active, last_operational, last_department_id, \
                 last_employment_state, active_transition_id FROM person_activity \
                 WHERE slug='acme' AND person_id='bo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        // #751-P9: the repaired projection no longer carries a fifth column,
        // `last_pane_department_id` = "executive" — the parent's window a head's
        // pane was drawn in. The repair still restores every ORG fact.
        assert_eq!(projection, (0, 0, "eng".into(), "active".into(), None));
        let terminal: (String, String, Option<String>, String) = tx
            .query_row(
                "SELECT status, reason, abandoned_at, intent_id FROM transitions WHERE slug='acme' AND person_id='bo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(terminal.0, "cancelled");
        assert_eq!(terminal.1, "superseded-by-pause:eng");
        assert_eq!(terminal.2, None);
        assert!(terminal.3.starts_with("unit-stop:eng:transition:"));
        // Exactly one terminal row, and nothing hangs off it. (Until #751-P4
        // this assertion also read a fabricated five-field handoff out of
        // `reflection_handoffs`. Both the payload and the table are deleted;
        // the unit-stop row now stands alone and invents nothing.)
        let rows: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM transitions WHERE slug='acme' AND person_id='bo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1, "the pause writes one terminal transition per member");

        assert_eq!(
            resume_department(&tx, "acme", "eng", "resume-at", "operator").unwrap(),
            PauseOutcome::Applied
        );
        assert_eq!(dept_state(&tx, "eng"), "active");
        let resumed_head: String = tx
            .query_row(
                "SELECT head_person_id FROM departments WHERE slug='acme' AND id='eng'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            resumed_head, "bo",
            "resume restores the same department and never substitutes its head"
        );
        assert_eq!(desired(&tx, "bo"), Some(0), "resume does not eagerly restart the exact head");
        tx.commit().unwrap();
    }
    #[test]
    fn pause_does_not_mask_unrelated_activity_corruption() {
        let mut conn = open();
        conn.execute_batch(
            "INSERT INTO person_activity(slug, person_id, last_desired_active, last_operational, last_department_id, last_employment_state, updated_at) VALUES('acme','bo',1,1,'eng','active','before'), ('acme','cos',1,1,'office-of-the-ceo',NULL,'before'); INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo');",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            pause_department(&tx, "acme", "eng", "pause-at", "operator").unwrap(),
            PauseOutcome::Applied
        );
        let unrelated: Option<String> = tx
            .query_row(
                "SELECT last_employment_state FROM person_activity WHERE slug='acme' AND person_id='cos'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unrelated, None, "pause repairs only members of the addressed subtree");
        tx.commit().unwrap();
    }
    #[test]
    fn resume_after_pause_leaves_launch_intent_absent_and_spawns_nobody() {
        // Resume restores state ONLY — it does NOT re-create launch_intent, so a
        // paused member stays down until an explicit start (THE HARD RULE).
        let mut conn = open();
        seed_subtree_sweep_fodder(&conn);
        let tx = conn.transaction().unwrap();
        pause_department(&tx, "acme", "eng", "t1", "op").unwrap();
        assert_eq!(
            resume_department(&tx, "acme", "eng", "t2", "op").unwrap(),
            PauseOutcome::Applied
        );
        assert_eq!(dept_state(&tx, "eng"), "active");
        let fences: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE slug='acme' AND person_id IN ('bo','sub1')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fences, 0, "resume never re-adds launch_intent — nobody spawns");
        tx.commit().unwrap();
    }
    #[test]
    fn resume_clears_the_flag() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        pause_department(&tx, "acme", "eng", "t1", "op").unwrap();
        assert_eq!(dept_state(&tx, "eng"), "paused");
        let out = resume_department(&tx, "acme", "eng", "t2", "op").unwrap();
        assert_eq!(out, PauseOutcome::Applied);
        assert_eq!(dept_state(&tx, "eng"), "active");
        tx.commit().unwrap();
    }
    #[test]
    fn resume_refuses_a_department_that_is_not_paused() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        match resume_department(&tx, "acme", "eng", "t1", "op").unwrap() {
            PauseOutcome::Refused { reason } => assert_eq!(reason.code(), "not-paused"),
            other => panic!("expected not-paused, got {other:?}"),
        }
        tx.commit().unwrap();
    }
}

#[cfg(test)]
mod bench_tests {
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;
    use rusqlite::Connection;

    /// Open a FULL-schema in-memory company (so the real typed accessors write
    /// every column) and seed a minimal exec-root org:
    ///   executive (root, head ada=CEO)
    ///     ├─ office-of-the-ceo (head cos = chief-of-staff)
    ///     └─ eng (head bo = a normal report)
    /// ada & cos are executive-root protected; bo is the normal shutdown target.
    /// FKs OFF: this unit exercises org_ops' composition, not the manifest FKs.
    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO org_settings(slug, display_slug, supervision_interval_ms, acknowledgement_timeout_ms, acknowledgement_retry_limit, replacement_limit) VALUES('acme', 'acme',30000,30000,3,3); INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('acme','executive',NULL,'Executive','company','active','ada',0,'t','t'), ('acme','office-of-the-ceo','executive','Office of the CEO','department','active','cos',1,'t','t'), ('acme','eng','executive','Engineering','department','active','bo',2,'t','t');
             INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','ada','Ada','CEO','lead','executive','active','executive',0,'t','t'), ('acme','cos','Cos','Chief of Staff','support','head','active','office-of-the-ceo',1,'t','t'), ('acme','bo','Bo','Engineer','build','worker','active','eng',2,'t','t');",
        )
        .expect("seed");
        conn
    }

    fn seed_lifecycle_authority(conn: &Connection) {
        conn.execute("INSERT INTO activity_meta(slug, created_at) VALUES('acme','t0')", [])
            .expect("activity authority");
    }

    #[test]
    fn released_bench_lifecycle_returns_its_internal_completion_identity() {
        let mut conn = open();
        seed_lifecycle_authority(&conn);
        let tx = conn.transaction().unwrap();
        let manifest = organization_rows::reconstruct(&tx, "acme")
            .expect("manifest query")
            .expect("manifest authority");
        activity::rows::read_rows(&tx, "acme", &manifest)
            .expect("activity query")
            .expect("activity authority");
        let outcome = bench_person_lifecycle(&tx, "acme", "bo", "t1", "operator").unwrap();
        let BenchLifecycleOutcome::Applied { completion: Some(completion) } = outcome else {
            panic!("expected applied lifecycle with a completion key");
        };
        assert_eq!(completion.operation_id, "transition:1:bo:park");
        assert_eq!(completion.person_id, "bo");
        assert_eq!(
            tx.query_row::<String, _, _>(
                "SELECT status FROM transitions WHERE slug='acme' AND id=?1",
                [&completion.operation_id],
                |row| row.get(0),
            )
            .unwrap(),
            "ready",
            "the structural commit retains the released transition for reconcile"
        );
        assert_eq!(
            tx.query_row::<String, _, _>(
                "SELECT employment_state FROM people WHERE slug='acme' AND id='bo'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            "benched"
        );
        tx.commit().unwrap();
    }

    #[test]
    fn bench_benches_a_worker_retaining_row_and_placement() {
        let mut conn = open(); // ada=CEO, cos=office, bo=eng worker (full schema)
        conn.execute("INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo')", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out = bench_person(&tx, "acme", "bo", "t1", "operator").unwrap();
        assert_eq!(out, BenchOutcome::Applied);

        // employment → benched; placement UNCHANGED (no re-home); ROW RETAINED.
        let (emp, placement): (String, String) = tx
            .query_row(
                "SELECT employment_state, department_id FROM people WHERE id='bo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(emp, "benched");
        assert_eq!(placement, "eng", "bench does NOT move anybody");
        let retained: i64 =
            tx.query_row("SELECT COUNT(*) FROM people WHERE id='bo'", [], |r| r.get(0)).unwrap();
        assert_eq!(retained, 1, "benched row is RETAINED");

        // Bench is audited in staffing_history and leaves no invented terminal
        // activity transition. A row minted here would carry none of the
        // identity the released-transition contract needs (the lifecycle owns
        // that write), and would make the normalized activity document
        // unreadable on its next publish.
        let transitions: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE person_id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(transitions, 0, "direct bench must not write an invalid terminal activity row");
        let (desired, pointer): (i64, Option<String>) = tx
            .query_row("SELECT last_desired_active, active_transition_id FROM person_activity WHERE person_id='bo'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(desired, 0, "desired-off so the converge reaps the pane");
        assert_eq!(pointer, None, "bench clears the stale activity transition pointer");
        let staffed: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM staffing_history WHERE person_id='bo' AND action='benched'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(staffed, 1);
        let fenced: i64 = tx
            .query_row("SELECT COUNT(*) FROM launch_intent WHERE person_id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fenced, 0);
        tx.commit().unwrap();
    }

    #[test]
    fn bench_adopts_a_released_transition_instead_of_superseding_it() {
        // class-D: same shape as offboard/transfer -- the bench lifecycle
        // releases a `park`-action transition (bench's action maps to
        // TransitionAction::Park) before bench_person is called.
        let mut conn = open(); // ada=CEO, cos=office, bo=eng worker (full schema)
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, reason, requested_at, handoff_deadline_at, placement_department_id) VALUES('acme','transition:7:bo:park','bo','park','ready','benched bo','t0','t9','eng')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO person_activity(slug, person_id, active_transition_id, updated_at) \
             VALUES('acme','bo','transition:7:bo:park','t0')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = bench_person(&tx, "acme", "bo", "t1", "operator").unwrap();
        assert_eq!(out, BenchOutcome::Applied);

        let transitions: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE person_id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(transitions, 1, "adoption mints no replacement transition");
        let status: String = tx
            .query_row("SELECT status FROM transitions WHERE id='transition:7:bo:park'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            status, "ready",
            "a released transition is consumed by the reconcile, never superseded"
        );
        let emp: String = tx
            .query_row("SELECT employment_state FROM people WHERE id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(emp, "benched");
        // On the ADOPTED path the pointer must keep naming the ready
        // transition, not clear to NULL: `LiveOrganizationProjection::reconstruct`
        // (writer.rs) rebuilds the org_documents "activity" blob's per-person
        // `activeTransitionId` from this column after every commit, so a
        // cleared pointer here orphans the ready row -- nothing points at it,
        // so no later reconcile pass can ever flip it `ready` -> `applied`,
        // and it strands forever even though the bench itself applied
        // cleanly. This regressed once already (a prior version of this test
        // asserted the pointer cleared here, matching the bug it should have
        // caught).
        let pointer: Option<String> = tx
            .query_row(
                "SELECT active_transition_id FROM person_activity WHERE person_id='bo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pointer, Some("transition:7:bo:park".to_string()), "adoption must point AT the adopted transition so a later reconcile can consume it, mirroring offboard_person");
        tx.commit().unwrap();
    }

    #[test]
    fn bench_still_clears_the_pointer_when_no_ready_transition_is_adopted() {
        // The NON-adopted path: no ready `park` transition exists, so
        // bench_person supersedes (cancels) whatever open transition it finds
        // and mints nothing bench-side to take its place -- `None` is correct
        // here, there is nothing left to consume. Regression pinning that the
        // adoption fix above does not accidentally start pointing at a
        // cancelled row.
        let mut conn = open();
        conn.execute(
            "INSERT INTO transitions(slug, id, person_id, action, status, requested_at) \
             VALUES('acme','open-1','bo','park','awaiting_handoff','t0')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO person_activity(slug, person_id, active_transition_id, updated_at) \
             VALUES('acme','bo','open-1','t0')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let out = bench_person(&tx, "acme", "bo", "t1", "operator").unwrap();
        assert_eq!(out, BenchOutcome::Applied);

        let status: String = tx
            .query_row("SELECT status FROM transitions WHERE id='open-1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "cancelled", "an unreleased open transition is still superseded");
        let pointer: Option<String> = tx
            .query_row(
                "SELECT active_transition_id FROM person_activity WHERE person_id='bo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            pointer, None,
            "nothing survives to point at once the only open transition was cancelled"
        );
        tx.commit().unwrap();
    }

    #[test]
    fn bench_applies_against_current_rows_after_unrelated_event() {
        let mut conn = open();
        conn.execute(
            "INSERT INTO org_events(slug, seq, entity, entity_id, op, at) \
             VALUES('acme', 1, 'person', 'x', 'noop', 't')",
            [],
        )
        .unwrap();
        // D2 allocation is counter-backed. Seed the matching counter with the
        // synthetic prior event so this regression exercises revisionless
        // bench semantics instead of a deliberately corrupt event feed.
        conn.execute("INSERT INTO counters(name, value) VALUES('org-events:acme', 1)", []).unwrap();
        let tx = conn.transaction().unwrap();
        let out = bench_person(&tx, "acme", "bo", "t1", "op").unwrap();
        assert_eq!(out, BenchOutcome::Applied);
        // A prior unrelated event is not a transaction collision gate.
        let emp: String = tx
            .query_row("SELECT employment_state FROM people WHERE id='bo'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(emp, "benched");
        let txns: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(txns, 0, "a direct bench does not manufacture an invalid activity transition");
        tx.commit().unwrap();
    }
    #[test]
    fn bench_preserves_h1_gapless_ordinal_bijection() {
        // H1: bench changes NO membership, so the whole-company ordinal set must
        // stay the identical gapless 0..N-1 bijection it was before the bench.
        let mut conn = open();
        let before: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT ordinal FROM people WHERE slug='acme' ORDER BY ordinal")
                .unwrap();
            let v = stmt
                .query_map([], |r| r.get::<_, i64>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            v
        };
        assert_eq!(before, vec![0, 1, 2], "seed is already a gapless bijection");
        let tx = conn.transaction().unwrap();
        assert_eq!(bench_person(&tx, "acme", "bo", "t1", "op").unwrap(), BenchOutcome::Applied);
        let after: Vec<i64> = {
            let mut stmt = tx
                .prepare("SELECT ordinal FROM people WHERE slug='acme' ORDER BY ordinal")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, i64>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
        };
        assert_eq!(after, vec![0, 1, 2], "bench leaves the gapless 0..N-1 bijection untouched");
        // every ordinal is still distinct and dense (bijection check).
        let n: i64 = tx
            .query_row("SELECT COUNT(*) FROM people WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        let distinct: i64 = tx
            .query_row("SELECT COUNT(DISTINCT ordinal) FROM people WHERE slug='acme'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let max: i64 = tx
            .query_row("SELECT MAX(ordinal) FROM people WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(distinct, n);
        assert_eq!(max, n - 1);
        tx.commit().unwrap();
    }
    #[test]
    fn bench_refuses_already_benched_and_departed() {
        let mut conn = open();
        conn.execute("UPDATE people SET employment_state='benched' WHERE id='bo'", []).unwrap();
        conn.execute("UPDATE people SET employment_state='departed' WHERE id='cos'", []).unwrap();
        // cos is exec-root protected, so re-target: make bo the two cases in turn.
        let tx = conn.transaction().unwrap();
        match bench_person(&tx, "acme", "bo", "t1", "op").unwrap() {
            BenchOutcome::Refused { reason } => assert_eq!(reason.code(), "already-benched"),
            other => panic!("expected already-benched, got {other:?}"),
        }
        tx.commit().unwrap();
        // departed case on a NON-exec-root worker.
        conn.execute("UPDATE people SET employment_state='departed' WHERE id='bo'", []).unwrap();
        let tx = conn.transaction().unwrap();
        match bench_person(&tx, "acme", "bo", "t1", "op").unwrap() {
            BenchOutcome::Refused { reason } => assert_eq!(reason.code(), "already-departed"),
            other => panic!("expected already-departed, got {other:?}"),
        }
        tx.commit().unwrap();
    }
    /// INVERTED on 2026-08-13 for `cos`, kept rather than deleted. The CEO is
    /// still never benched; the person sitting beside them now is.
    #[test]
    fn bench_refuses_the_ceo_alone() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let code = |who: &str| match bench_person(&tx, "acme", who, "t1", "op").unwrap() {
            BenchOutcome::Refused { reason } => reason.code().to_string(),
            other => panic!("expected refusal, got {other:?}"),
        };
        assert_eq!(code("ada"), "exec-root-protected", "the CEO is never benched");
        assert_eq!(code("ghost"), "unknown-person");
        // No writes on either refusal, asserted BEFORE the accepted bench below
        // so an applied write cannot mask a refused one.
        let txns: i64 = tx
            .query_row("SELECT COUNT(*) FROM transitions WHERE slug='acme'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(txns, 0);
        assert_eq!(
            bench_person(&tx, "acme", "cos", "t1", "op").unwrap(),
            BenchOutcome::Applied,
            "the CEO's chief of staff is benched like anybody else"
        );
        tx.commit().unwrap();
    }

    /// Every person id the staffing ledger mentions that the roster no longer
    /// has, plus every `hired` with no matching `offboarded`. Both are empty in
    /// a consistent company; both were non-empty after a subtree removal for as
    /// long as that removal ran `DELETE FROM people`.
    fn ledger_inconsistencies(tx: &Transaction<'_>) -> (Vec<String>, Vec<String>) {
        let orphans: Vec<String> = tx
            .prepare(
                "SELECT DISTINCT h.person_id FROM staffing_history h \
                 WHERE h.slug = 'acme' AND NOT EXISTS ( \
                     SELECT 1 FROM people p WHERE p.slug = h.slug AND p.id = h.person_id) \
                 ORDER BY h.person_id",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let unclosed: Vec<String> = tx
            .prepare(
                "SELECT DISTINCT h.person_id FROM staffing_history h \
                 WHERE h.slug = 'acme' AND h.action = 'hired' AND NOT EXISTS ( \
                     SELECT 1 FROM staffing_history o WHERE o.slug = h.slug \
                       AND o.person_id = h.person_id AND o.action = 'offboarded') \
                 ORDER BY h.person_id",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        (orphans, unclosed)
    }

    /// SCOPE, ON THE MOST DESTRUCTIVE VERB IN THE CRATE.
    ///
    /// Before this, `remove_department_tree` took an actor and asked nothing of
    /// it: any caller reaching the route could delete any department in any
    /// company, and the ledger recorded who did it as the empty string.
    ///
    /// The CEO manages the whole company, so this is the POSITIVE case — and it
    /// is the one that keeps the two refusals below honest, because a guard
    /// that refused everybody would satisfy them both.
    #[test]
    fn the_ceo_may_remove_a_department_because_it_manages_the_whole_company() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let outcome = remove_department_tree(&tx, "acme", "eng", "t1", "ada").unwrap();
        assert!(
            matches!(outcome, RemoveDepartmentOutcome::Applied { .. }),
            "the CEO manages every department: {outcome:?}"
        );
        tx.commit().unwrap();
    }

    /// A head reaches its OWN subtree and nothing sideways. `cos` heads
    /// `office-of-the-ceo`, which is a sibling of `eng`, so it may not delete
    /// it — the sideways reach the tree model forbids.
    #[test]
    fn a_head_may_not_remove_a_department_outside_its_own_subtree() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let outcome = remove_department_tree(&tx, "acme", "eng", "t1", "cos").unwrap();
        let RemoveDepartmentOutcome::Refused { code, .. } = &outcome else {
            panic!("a sibling head must not delete eng: {outcome:?}");
        };
        assert_eq!(*code, "actor-out-of-scope");
        // ZERO WRITES: the department and its people are untouched.
        assert!(organization_rows::department_state(&tx, "acme", "eng").unwrap().is_some());
        tx.commit().unwrap();
    }

    /// THE ACTOR RULE. `operator` names no person row, so it is not judged —
    /// and every pre-existing test in this file passes exactly that value.
    ///
    /// Gating on the string's CONTENT would need a list of placeholder
    /// spellings (`operator`, `op`, the empty string, and whatever is written
    /// next), which rots. Enforcing only when the actor names a real person is
    /// sound while nothing authenticates AND once everything does, because the
    /// route then overwrites the actor with the caller's principal.
    #[test]
    fn an_actor_that_names_no_person_is_not_judged() {
        for actor in ["operator", "op", ""] {
            let mut conn = open();
            let tx = conn.transaction().unwrap();
            let outcome = remove_department_tree(&tx, "acme", "eng", "t1", actor).unwrap();
            assert!(
                matches!(outcome, RemoveDepartmentOutcome::Applied { .. }),
                "{actor:?} names nobody and must pass through: {outcome:?}"
            );
            tx.commit().unwrap();
        }
    }

    /// The refusal must say WHO and WHAT, not merely that something was denied.
    #[test]
    fn the_out_of_scope_refusal_names_the_department_relationship() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let outcome = remove_department_tree(&tx, "acme", "eng", "t1", "cos").unwrap();
        let RemoveDepartmentOutcome::Refused { detail, .. } = &outcome else {
            panic!("expected a refusal: {outcome:?}");
        };
        assert!(detail.contains("does not manage"), "{detail}");
        assert!(detail.contains("removing"), "{detail}");
        tx.commit().unwrap();
    }

    // ---- B1: the remaining destructive staffing and structure verbs --------
    //
    // Every verb below took an `actor`, recorded it and asked nothing of it,
    // so any person could stop, bench, start, recall, move, promote or replace
    // any other. The rule each test pins is the same one, and it is a SCOPE
    // rule, never a role: a head reaches its own subtree and nothing sideways,
    // and the CEO heads the company root and therefore reaches everybody.
    //
    // In this fixture `ada` is the CEO (kind `executive`), `cos` heads
    // `office-of-the-ceo`, and `bo` lives in the sibling department `eng`. So
    // `cos` acting on `bo` is the sideways reach the tree forbids, and `ada`
    // acting on `bo` is the positive case that stops a guard which refused
    // everybody from satisfying the negatives on its own.

    /// Make `person_id` a real head of `department_id`, which the fixture's
    /// `eng` row points at without setting `people.kind` (a seed shortcut, not
    /// a headship — `person_manages_department` keys on the kind).
    fn make_head(conn: &Connection, person_id: &str, department_id: &str) {
        conn.execute(
            "UPDATE people SET kind = 'head' WHERE slug = 'acme' AND id = ?1",
            [person_id],
        )
        .expect("promote");
        conn.execute(
            "UPDATE departments SET head_person_id = ?1 WHERE slug = 'acme' AND id = ?2",
            [person_id, department_id],
        )
        .expect("head the department");
    }

    /// Add an ordinary worker to `eng`, so appointment and replacement have a
    /// successor that is neither the sitting head nor the CEO.
    fn seed_eng_worker(conn: &Connection) {
        conn.execute(
            "INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, \
             department_id, ordinal, created_at, \
             updated_at) VALUES ('acme','ci','Ci','Engineer','build','worker','active','eng',3,'t','t')",
            [],
        )
        .expect("seed worker");
    }

    #[test]
    fn shutdown_refuses_an_actor_that_does_not_manage_the_targets_department() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let outcome =
            shutdown_person(&tx, "acme", "bo", &ShutdownKind::AutomaticSettle, "t1", "cos")
                .unwrap();
        let ShutdownOutcome::Refused { reason } = &outcome else {
            panic!("a sibling head must not stop bo: {outcome:?}");
        };
        assert_eq!(reason.code(), "actor-out-of-scope");
        tx.commit().unwrap();
    }

    /// THE POSITIVE CASE for shutdown. The CEO holds every tree.
    #[test]
    fn shutdown_applies_for_the_ceo_and_for_an_actor_that_names_nobody() {
        for actor in ["ada", "operator", ""] {
            let mut conn = open();
            let tx = conn.transaction().unwrap();
            let outcome =
                shutdown_person(&tx, "acme", "bo", &ShutdownKind::AutomaticSettle, "t1", actor)
                    .unwrap();
            assert!(
                matches!(outcome, ShutdownOutcome::Applied { .. }),
                "{actor:?} must be allowed to stop bo: {outcome:?}"
            );
            tx.commit().unwrap();
        }
    }

    #[test]
    fn bench_refuses_an_actor_outside_the_targets_subtree() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let outcome = bench_person(&tx, "acme", "bo", "t1", "cos").unwrap();
        assert_eq!(outcome, BenchOutcome::Refused { reason: BenchRefusal::ActorOutOfScope });
        // ZERO WRITES: the refusal happens before the fence.
        let employment: String = tx
            .query_row("SELECT employment_state FROM people WHERE id='bo'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(employment, "active");
        tx.commit().unwrap();
    }

    #[test]
    fn bench_applies_for_the_ceo() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(bench_person(&tx, "acme", "bo", "t1", "ada").unwrap(), BenchOutcome::Applied);
        tx.commit().unwrap();
    }

    /// The reflected lifecycle is a second door onto one mutation, so it must
    /// not be a second answer about who may walk through it.
    #[test]
    fn bench_lifecycle_refuses_the_same_actor_the_direct_bench_refuses() {
        let mut conn = open();
        seed_lifecycle_authority(&conn);
        let tx = conn.transaction().unwrap();
        let outcome = bench_person_lifecycle(&tx, "acme", "bo", "t1", "cos").unwrap();
        assert!(
            matches!(
                outcome,
                BenchLifecycleOutcome::Refused { reason: BenchRefusal::ActorOutOfScope }
            ),
            "a sibling head must not bench bo: {outcome:?}"
        );
        tx.commit().unwrap();
    }

    #[test]
    fn bench_lifecycle_applies_for_the_ceo() {
        let mut conn = open();
        seed_lifecycle_authority(&conn);
        let tx = conn.transaction().unwrap();
        let outcome = bench_person_lifecycle(&tx, "acme", "bo", "t1", "ada").unwrap();
        assert!(
            matches!(outcome, BenchLifecycleOutcome::Applied { .. }),
            "the CEO holds every tree: {outcome:?}"
        );
        tx.commit().unwrap();
    }

    #[test]
    fn start_refuses_an_actor_outside_the_targets_subtree() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let outcome = start_person(&tx, "acme", "bo", "t1", "cos").unwrap();
        let DirectOutcome::Refused { code, detail } = &outcome else {
            panic!("a sibling head must not start bo: {outcome:?}");
        };
        assert_eq!(*code, "actor-out-of-scope");
        assert!(detail.contains("does not manage"), "{detail}");
        tx.commit().unwrap();
    }

    #[test]
    fn start_applies_for_the_ceo() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(start_person(&tx, "acme", "bo", "t1", "ada").unwrap(), DirectOutcome::Applied);
        tx.commit().unwrap();
    }

    #[test]
    fn recall_refuses_an_actor_outside_the_targets_subtree() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        // Bench first as an actor that names nobody, so the recall under test
        // is the only judged call in this transaction.
        assert_eq!(
            bench_person(&tx, "acme", "bo", "t0", "operator").unwrap(),
            BenchOutcome::Applied
        );
        let outcome = recall_person(&tx, "acme", "bo", "t1", "cos").unwrap();
        let DirectOutcome::Refused { code, .. } = &outcome else {
            panic!("a sibling head must not recall bo: {outcome:?}");
        };
        assert_eq!(*code, "actor-out-of-scope");
        tx.commit().unwrap();
    }

    #[test]
    fn recall_applies_for_the_ceo() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            bench_person(&tx, "acme", "bo", "t0", "operator").unwrap(),
            BenchOutcome::Applied
        );
        assert_eq!(recall_person(&tx, "acme", "bo", "t1", "ada").unwrap(), DirectOutcome::Applied);
        tx.commit().unwrap();
    }

    /// TRANSFER ASKS BOTH DEPARTMENTS. `cos` manages the DESTINATION
    /// (`office-of-the-ceo`) and not the source, so it may not pull `bo` out of
    /// `eng` — the half a source-only check would have missed.
    #[test]
    fn transfer_refuses_an_actor_that_manages_only_the_destination() {
        let mut conn = open();
        // An ORDINARY member, not `bo`: the fixture points `eng.head_person_id`
        // at `bo`, so moving them is refused `vacancy-decision-required` by
        // `validate_mover` long before any authorization question is asked.
        seed_eng_worker(&conn);
        let tx = conn.transaction().unwrap();
        let outcome =
            transfer_person(&tx, "acme", "ci", "office-of-the-ceo", "i1", "t1", "cos", None)
                .unwrap();
        let TransferOutcome::Refused { reason } = &outcome else {
            panic!("cos does not manage eng: {outcome:?}");
        };
        assert_eq!(reason.code(), "actor-out-of-scope");
        tx.commit().unwrap();
    }

    /// The mirror half, and the escalation the packet names: a head that
    /// manages the SOURCE may not push its people into a unit it does not
    /// manage. `ci` is promoted to head `eng`, so it holds the source alone.
    #[test]
    fn transfer_refuses_an_actor_that_manages_only_the_source() {
        let mut conn = open();
        seed_eng_worker(&conn);
        make_head(&conn, "ci", "eng");
        let tx = conn.transaction().unwrap();
        let outcome =
            transfer_person(&tx, "acme", "bo", "office-of-the-ceo", "i1", "t1", "ci", None)
                .unwrap();
        let TransferOutcome::Refused { reason } = &outcome else {
            panic!("ci manages eng but not office-of-the-ceo: {outcome:?}");
        };
        assert_eq!(reason.code(), "actor-out-of-scope");
        assert!(reason.detail().contains("BOTH"), "{}", reason.detail());
        tx.commit().unwrap();
    }

    #[test]
    fn transfer_applies_for_the_ceo_who_manages_both_departments() {
        let mut conn = open();
        seed_eng_worker(&conn);
        let tx = conn.transaction().unwrap();
        let outcome =
            transfer_person(&tx, "acme", "ci", "office-of-the-ceo", "i1", "t1", "ada", None)
                .unwrap();
        assert!(
            matches!(outcome, TransferOutcome::Applied { .. }),
            "the CEO manages every department: {outcome:?}"
        );
        tx.commit().unwrap();
    }

    #[test]
    fn appoint_head_refuses_an_actor_outside_the_department() {
        let mut conn = open();
        seed_eng_worker(&conn);
        let tx = conn.transaction().unwrap();
        let outcome = appoint_department_head(&tx, "acme", "eng", "ci", None, "t1", "cos").unwrap();
        let AppointOutcome::Refused { reason } = &outcome else {
            panic!("a sibling head must not appoint eng's head: {outcome:?}");
        };
        assert_eq!(reason.code(), "actor-out-of-scope");
        // The refusal must SAY that an appointment moves the person, or the
        // caller cannot tell why a structural request failed.
        assert!(reason.detail().contains("MOVES"), "{}", reason.detail());
        tx.commit().unwrap();
    }

    /// The DEMOTE destination is asked separately. `ci` heads `eng` and so may
    /// appoint inside it, but demoting the outgoing head into
    /// `office-of-the-ceo` would push a person into a unit `ci` does not
    /// manage.
    #[test]
    fn appoint_head_refuses_a_demotion_into_a_department_the_actor_does_not_manage() {
        let mut conn = open();
        seed_eng_worker(&conn);
        make_head(&conn, "ci", "eng");
        let tx = conn.transaction().unwrap();
        let outcome = appoint_department_head(
            &tx,
            "acme",
            "eng",
            "bo",
            Some("office-of-the-ceo"),
            "t1",
            "ci",
        )
        .unwrap();
        let AppointOutcome::Refused { reason } = &outcome else {
            panic!("ci does not manage office-of-the-ceo: {outcome:?}");
        };
        assert_eq!(reason.code(), "actor-out-of-scope");
        tx.commit().unwrap();
    }

    #[test]
    fn appoint_head_applies_for_the_ceo() {
        let mut conn = open();
        seed_eng_worker(&conn);
        let tx = conn.transaction().unwrap();
        let outcome = appoint_department_head(&tx, "acme", "eng", "ci", None, "t1", "ada").unwrap();
        assert_eq!(outcome, AppointOutcome::Applied);
        tx.commit().unwrap();
    }

    #[test]
    fn replace_head_and_offboard_refuses_an_actor_outside_the_department() {
        let mut conn = open();
        seed_eng_worker(&conn);
        make_head(&conn, "bo", "eng");
        let tx = conn.transaction().unwrap();
        let outcome = replace_head_and_offboard(&tx, "acme", "bo", "ci", "t1", "cos").unwrap();
        let DirectOutcome::Refused { code, detail } = &outcome else {
            panic!("a sibling head must not replace eng's head: {outcome:?}");
        };
        assert_eq!(*code, "actor-out-of-scope");
        assert!(detail.contains("does not manage"), "{detail}");
        tx.commit().unwrap();
    }

    // ---- B1: the remaining DEPARTMENT verbs --------------------------------
    //
    // `reparent` is NOT here: it landed separately on main (#1095) with its own
    // two-code refusal and its own tests.

    #[test]
    fn move_members_refuses_an_actor_that_manages_only_the_destination() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let outcome = move_department_members(
            &tx,
            "acme",
            "eng",
            "office-of-the-ceo",
            &[],
            "i1",
            "t1",
            "cos",
        )
        .unwrap();
        let TransferOutcome::Refused { reason } = &outcome else {
            panic!("cos does not manage eng: {outcome:?}");
        };
        assert_eq!(reason.code(), "actor-out-of-scope");
        tx.commit().unwrap();
    }

    #[test]
    fn move_members_applies_for_the_ceo() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let outcome = move_department_members(
            &tx,
            "acme",
            "eng",
            "office-of-the-ceo",
            &[],
            "i1",
            "t1",
            "ada",
        )
        .unwrap();
        assert!(
            matches!(outcome, TransferOutcome::Applied { .. }),
            "the CEO manages every department: {outcome:?}"
        );
        tx.commit().unwrap();
    }

    #[test]
    fn pause_refuses_an_actor_outside_the_departments_subtree() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let outcome = pause_department(&tx, "acme", "eng", "t1", "cos").unwrap();
        assert_eq!(outcome, PauseOutcome::Refused { reason: PauseRefusal::ActorOutOfScope });
        // ZERO WRITES: the department is still active.
        assert_eq!(
            organization_rows::department_state(&tx, "acme", "eng").unwrap().as_deref(),
            Some("active")
        );
        tx.commit().unwrap();
    }

    #[test]
    fn pause_applies_for_the_ceo() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            pause_department(&tx, "acme", "eng", "t1", "ada").unwrap(),
            PauseOutcome::Applied
        );
        tx.commit().unwrap();
    }

    #[test]
    fn resume_refuses_an_actor_outside_the_departments_subtree() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            pause_department(&tx, "acme", "eng", "t0", "operator").unwrap(),
            PauseOutcome::Applied
        );
        let outcome = resume_department(&tx, "acme", "eng", "t1", "cos").unwrap();
        assert_eq!(outcome, PauseOutcome::Refused { reason: PauseRefusal::ActorOutOfScope });
        tx.commit().unwrap();
    }

    #[test]
    fn resume_applies_for_the_ceo() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            pause_department(&tx, "acme", "eng", "t0", "operator").unwrap(),
            PauseOutcome::Applied
        );
        assert_eq!(
            resume_department(&tx, "acme", "eng", "t1", "ada").unwrap(),
            PauseOutcome::Applied
        );
        tx.commit().unwrap();
    }

    /// EVERY DEPARTMENT IN THE BATCH IS ASKED. `cos` manages
    /// `office-of-the-ceo` and not `eng`, so a batch naming both is refused
    /// whole — checking only the first entry would make the batch verb a way
    /// round the single verb's guard.
    #[test]
    fn resume_many_refuses_when_one_department_in_the_batch_is_out_of_scope() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        for department_id in ["office-of-the-ceo", "eng"] {
            assert_eq!(
                pause_department(&tx, "acme", department_id, "t0", "operator").unwrap(),
                PauseOutcome::Applied
            );
        }
        let batch = ["office-of-the-ceo".to_string(), "eng".to_string()];
        let outcome = resume_departments(&tx, "acme", &batch, false, "t1", "cos").unwrap();
        assert_eq!(outcome, PauseOutcome::Refused { reason: PauseRefusal::ActorOutOfScope });
        tx.commit().unwrap();
    }

    #[test]
    fn resume_many_applies_for_the_ceo() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        for department_id in ["office-of-the-ceo", "eng"] {
            assert_eq!(
                pause_department(&tx, "acme", department_id, "t0", "operator").unwrap(),
                PauseOutcome::Applied
            );
        }
        let batch = ["office-of-the-ceo".to_string(), "eng".to_string()];
        assert_eq!(
            resume_departments(&tx, "acme", &batch, false, "t1", "ada").unwrap(),
            PauseOutcome::Applied
        );
        tx.commit().unwrap();
    }

    /// The target is the company ROOT, and the only person who manages the root
    /// is the one who heads it. That falls out of the same subtree predicate;
    /// no title is named and no region is protected.
    #[test]
    fn reactivate_executive_root_refuses_a_head_that_does_not_hold_the_root() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let outcome = reactivate_executive_root(&tx, "acme", "t1", "cos").unwrap();
        let DirectOutcome::Refused { code, .. } = &outcome else {
            panic!("cos heads a child, not the root: {outcome:?}");
        };
        assert_eq!(*code, "actor-out-of-scope");
        tx.commit().unwrap();
    }

    #[test]
    fn reactivate_executive_root_applies_for_the_ceo() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(
            reactivate_executive_root(&tx, "acme", "t1", "ada").unwrap(),
            DirectOutcome::Applied
        );
        tx.commit().unwrap();
    }

    #[test]
    fn replace_head_and_offboard_applies_for_the_ceo() {
        let mut conn = open();
        seed_eng_worker(&conn);
        make_head(&conn, "bo", "eng");
        let tx = conn.transaction().unwrap();
        let outcome = replace_head_and_offboard(&tx, "acme", "bo", "ci", "t1", "ada").unwrap();
        assert_eq!(outcome, DirectOutcome::Applied);
        tx.commit().unwrap();
    }

    #[test]
    fn remove_department_tree_offboards_its_people_and_never_deletes_one() {
        let mut conn = open();
        conn.execute("INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo')", []).unwrap();
        conn.execute_batch(
            "INSERT INTO person_activity(slug, person_id, last_desired_active, last_employment_state, last_operational, updated_at) \
               VALUES('acme','bo',1,'active',1,'t'); \
             INSERT INTO transitions(slug, id, person_id, action, status, requested_at) \
               VALUES('acme','transition:1:bo:park','bo','park','awaiting_handoff','t'); \
             INSERT INTO staffing_history(slug, seq, person_id, action, from_department_id, to_department_id, reason, at) \
               VALUES('acme',1,'bo','hired',NULL,'eng','the first engineer','t0'); \
             INSERT INTO counters(name, value) VALUES('staffing:acme',1);",
        )
        .expect("seed activity + ledger rows");
        let tx = conn.transaction().unwrap();
        let outcome = remove_department_tree(&tx, "acme", "eng", "t1", "operator").unwrap();
        assert_eq!(
            outcome,
            RemoveDepartmentOutcome::Applied {
                removed_department_ids: vec!["eng".to_string()],
                departed_person_ids: vec!["bo".to_string()],
            }
        );

        // THE defect this replaces. `org_offboard` retains a departed person's
        // record deliberately, and this path called the same act "fires" while
        // running `DELETE FROM people`. The delete did not erase the history —
        // `staffing_history` carries no people FK on purpose — it made the
        // history WRONG: an orphaned `hired` row, no `offboarded` row, and
        // nobody it belongs to. Assert the ledger, not the mechanism: this
        // question keeps its meaning through any later change of implementation.
        let (orphans, unclosed) = ledger_inconsistencies(&tx);
        assert!(
            orphans.is_empty(),
            "the staffing ledger names people the roster no longer has: {orphans:?}"
        );
        assert!(
            unclosed.is_empty(),
            "these people were hired and the ledger never records them leaving: {unclosed:?}"
        );

        let (employment, placement, kind): (String, String, String) = tx
            .query_row(
                "SELECT employment_state, department_id, kind \
                 FROM people WHERE slug='acme' AND id='bo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("the fired person's row is RETAINED, never deleted");
        assert_eq!(employment, "departed");
        assert_eq!(placement, "executive", "re-homed to the removed subtree's parent");
        assert_eq!(kind, "worker");
        let (offboarded_from, offboard_reason): (Option<String>, String) = tx
            .query_row(
                "SELECT from_department_id, reason FROM staffing_history \
                 WHERE slug='acme' AND person_id='bo' AND action='offboarded'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the departure is on the durable ledger");
        assert_eq!(
            offboarded_from.as_deref(),
            Some("eng"),
            "the ledger records the unit they LEFT, not the parent they came to rest in"
        );
        assert_eq!(offboard_reason, "department eng removed");

        let remaining_departments: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='eng'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let remaining_fences: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM launch_intent WHERE slug='acme' AND person_id='bo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining_departments, 0, "the DEPARTMENT is what a removal deletes");
        assert_eq!(remaining_fences, 0, "unattended: the fence goes with the department");

        // #526 inverted. The invariant is `person_activity ⊆ people_order`, and
        // it now holds because BOTH sides are retained — the stronger reason.
        // Deleting these rows today would break it in the other direction and
        // would destroy the history of a person the company still remembers.
        let (desired, active_transition): (i64, Option<String>) = tx
            .query_row(
                "SELECT last_desired_active, active_transition_id FROM person_activity \
                 WHERE slug='acme' AND person_id='bo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the activity row is retained with the person");
        assert_eq!(desired, 0, "desired-off: the reconcile reaps the pane off this");
        assert_eq!(active_transition, None, "no handoff is pending into a deleted department");
        let transition_status: String = tx
            .query_row(
                "SELECT status FROM transitions WHERE slug='acme' AND id='transition:1:bo:park'",
                [],
                |row| row.get(0),
            )
            .expect("their transitions are retained");
        assert_eq!(
            transition_status, "cancelled",
            "nobody can release a handoff into a department deleted in this transaction"
        );

        let retained_root: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='executive'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained_root, 1);
        tx.commit().unwrap();
    }

    #[test]
    fn remove_department_tree_re_homes_to_the_removed_units_parent_not_the_root() {
        let mut conn = open();
        conn.execute_batch(
            "INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES('acme','platform','eng','Platform','department','active','pat',3,'t','t'); INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES('acme','pat','Pat','Platform Lead','build','head','active','platform',3,'t','t');",
        )
        .expect("seed a deeper subtree");
        let tx = conn.transaction().unwrap();
        let outcome = remove_department_tree(&tx, "acme", "platform", "t1", "operator").unwrap();
        assert_eq!(
            outcome,
            RemoveDepartmentOutcome::Applied {
                removed_department_ids: vec!["platform".to_string()],
                departed_person_ids: vec!["pat".to_string()],
            }
        );
        let (home, kind): (String, String) = tx
            .query_row(
                "SELECT department_id, kind FROM people WHERE slug='acme' AND id='pat'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(home, "eng", "the parent absorbs them — not the company root");
        assert_eq!(kind, "worker", "a head of a department that no longer exists is not a head");
        let survivor: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='eng'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(survivor, 1, "only the named subtree is removed");
        tx.commit().unwrap();
    }

    #[test]
    fn remove_department_tree_leaves_nothing_partial_when_its_transaction_is_abandoned() {
        let mut conn = open();
        conn.execute("INSERT INTO launch_intent(slug, person_id) VALUES('acme','bo')", []).unwrap();
        let tx = conn.transaction().unwrap();
        remove_department_tree(&tx, "acme", "eng", "t1", "operator").expect("removal applies");
        // Everything — the departures, the ledger row, the fence withdrawal and
        // the department deletes — is one caller-owned transaction. Abandoning
        // it must leave a company that never heard of the removal.
        tx.rollback().unwrap();
        let (employment, home): (String, String) = conn
            .query_row(
                "SELECT employment_state, department_id FROM people WHERE slug='acme' AND id='bo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(employment, "active");
        assert_eq!(home, "eng");
        let counts: (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM departments WHERE slug='acme' AND id='eng'), \
                        (SELECT COUNT(*) FROM launch_intent WHERE slug='acme' AND person_id='bo'), \
                        (SELECT COUNT(*) FROM staffing_history WHERE slug='acme')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 1, 0), "no department gone, no fence dropped, no ledger row");
    }
}
