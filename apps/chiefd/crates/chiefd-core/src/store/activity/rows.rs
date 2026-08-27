//! The activity/lifecycle store on normalized rows (org-data-normalization P0,
//! N4). This is the blob→row half of the port: it reconstructs the in-memory
//! [`ActivityLedger`] aggregate from the normalized tables and diffs a whole
//! ledger back into them, so the `activity` JSON blob (the second #501 unbounded
//! accumulator — 1.19 MB live on cobalt, read at the footer's ≥1/30s cadence)
//! is replaced by indexed rows with bounded, individually-deletable history.
//!
//! Tables owned here (schema frozen in `schema.rs`):
//!   - `transitions` (+ `transitions_one_active` partial unique index),
//!   - `person_activity`,
//!   - `activity_meta` (per-slug singleton: `automatic_park_cursor`,
//!     `created_at`).
//!
//! `next_transition_sequence` is DELIBERATELY not a column: it is a D2 per-slug
//! counter row `transitions:<slug>` in `counters`, holding the LAST allocated
//! sequence (so `next = counter + 1`, matching [`rows_txn::allocate_seq`]
//! semantics — never `MAX(seq)+1`). `updated_at` is derived from
//! `MAX(org_events.at)` (no column, per the landed schema).
//!
//! TOMBSTONE (#751-P4): this module also owned `reflection_handoffs` and
//! `reflection_handoff_items`, the normalized home of the per-transition
//! reflection payload, plus the `reflection-memory/<person>` blob fold and
//! shadow-diff that migrated the TypeScript launcher's records into them. The
//! reflection concept is gone from the product, so the read/write/durability/
//! fold/backfill/verify functions were deleted and BOTH tables are dropped
//! (`schema.rs`). A transition row is now the whole durable fact.

use rusqlite::{params, OptionalExtension, Transaction};

use super::{
    ActivityLedger, GracefulTransition, PersonActivityState, TransitionAction, TransitionStatus,
    ACTIVITY_SCHEMA_VERSION,
};
use crate::error::corrupt_store;
use crate::error::Refusal;
use crate::store::organization::{EmploymentState, OrganizationManifest};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::store::shadow_report::{Disposition, ShadowReport};
use crate::ChiefdError;

/// A publish carried a key the row model does not represent (item D). The detail
/// lists the offending dotted paths so the caller fixes the exact field.
pub const UNMODELED_KEYS: &str = "unmodeled-keys";

/// The store name the boundary maps a row failure onto
/// (`ChiefdError::Corrupt`(crate::ChiefdError::Corrupt)); see
/// [`activity_store_failed`].
pub const ACTIVITY_STORE: &str = "activity";

/// Map a row-layer `rusqlite::Error` onto the store's fail-closed error. Called
/// at the `in_transaction`/route boundary (the `lease.rs` pattern) — the row
/// functions themselves stay `rusqlite::Result` so they compose with
/// [`rows_txn::apply_and_emit`]'s `E: From<rusqlite::Error>` bound.
///
/// The `tracing` line is kept for a host that installs a subscriber, and the
/// error ALSO travels inside the returned value — `cargo test` installs no
/// subscriber, so the traced copy is discarded in exactly the context where
/// this failure has been observed.
#[must_use]
pub fn activity_store_failed(err: rusqlite::Error) -> crate::ChiefdError {
    tracing::error!(error = %err, "the activity rows could not be read or written");
    crate::error::store_failure(ACTIVITY_STORE, err)
}

/// The `counters` key for a company's transition sequence (D2). Holds the LAST
/// allocated `transition:<seq>:` value; `next_transition_sequence = value + 1`.
#[must_use]
pub fn transitions_counter_key(slug: &str) -> String {
    format!("transitions:{slug}")
}

fn employment_state_as_str(state: EmploymentState) -> &'static str {
    match state {
        EmploymentState::Active => "active",
        EmploymentState::Benched => "benched",
        EmploymentState::Departed => "departed",
    }
}

fn employment_state_from_str(text: &str) -> Option<EmploymentState> {
    match text {
        "active" => Some(EmploymentState::Active),
        "benched" => Some(EmploymentState::Benched),
        "departed" => Some(EmploymentState::Departed),
        _ => None,
    }
}

fn transition_status_from_str(text: &str) -> Option<TransitionStatus> {
    match text {
        "awaiting_handoff" => Some(TransitionStatus::AwaitingHandoff),
        "overdue" => Some(TransitionStatus::Overdue),
        "ready" => Some(TransitionStatus::Ready),
        "applied" => Some(TransitionStatus::Applied),
        "cancelled" => Some(TransitionStatus::Cancelled),
        "forced" => Some(TransitionStatus::Forced),
        _ => None,
    }
}

/// A stored value outside its column's modelled vocabulary is a corrupt-store
/// condition; surface it as a `rusqlite::Error` so it flows through the row
/// layer and is mapped to `Corrupt` at the boundary by [`activity_store_failed`].
fn corrupt_value(detail: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("activity: {detail}").into(),
    )
}

/// The embedded monotonic sequence of a `transition:<seq>:<person>:<action>` id.
/// `transition_order` is chronological, so we recover it by sorting ids on this
/// value (deterministic, and it survives the retention prune since the newest
/// ids — the highest sequences — are always retained).
fn embedded_sequence(transition_id: &str) -> Option<u64> {
    transition_id.strip_prefix("transition:")?.split(':').next()?.parse().ok()
}

/// Reconstruct the activity ledger for `slug` from its rows, or `None` when the
/// company has no `activity_meta` row (never seeded / dropped). Absence is the
/// caller's decision — this never fabricates an empty ledger (the store's
/// "absence is corruption, never a default" contract).
///
/// # Errors
/// `ChiefdError::Corrupt` when a stored row holds a value outside its column's
/// modelled vocabulary (e.g. an unknown status), or any `rusqlite` failure.
pub fn read_rows(
    tx: &Transaction<'_>,
    slug: &str,
    manifest: &OrganizationManifest,
) -> rusqlite::Result<Option<ActivityLedger>> {
    let meta: Option<(Option<i64>, String)> = tx
        .query_row(
            "SELECT automatic_park_cursor, created_at FROM activity_meta WHERE slug = ?1",
            params![slug],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((automatic_park_cursor, created_at)) = meta else {
        return Ok(None);
    };

    // --- transitions ------------------------------------------------------
    let mut transitions = std::collections::BTreeMap::new();
    let mut ids: Vec<String> = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT id, person_id, action, status, intent_id, reason, \
             placement_department_id, \
             to_department_id, requested_at, handoff_deadline_at, \
             applied_at, cancelled_at, forced_at, abandoned_at \
             FROM transitions WHERE slug = ?1",
        )?;
        let rows = stmt.query_map(params![slug], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, Option<String>>(13)?,
            ))
        })?;
        for row in rows {
            let row = row?;
            let action = TransitionAction::parse(&row.2)
                .ok_or_else(|| corrupt_value("unmodeled stored value"))?;
            let status = transition_status_from_str(&row.3)
                .ok_or_else(|| corrupt_value("unmodeled stored value"))?;
            let transition = GracefulTransition {
                id: row.0.clone(),
                person_id: row.1,
                action,
                reason: row.5,
                intent_id: row.4,
                placement_department_id: row.6.unwrap_or_default(),
                to_department_id: row.7,
                status,
                requested_at: row.8,
                handoff_deadline_at: row.9.unwrap_or_default(),
                applied_at: row.10,
                cancelled_at: row.11,
                forced_at: row.12,
                abandoned_at: row.13,
            };
            ids.push(row.0);
            transitions.insert(transition.id.clone(), transition);
        }
    }
    ids.sort_by_key(|id| embedded_sequence(id).unwrap_or(0));
    let transition_order = ids;

    // --- per-person activity ----------------------------------------------
    let mut people = std::collections::BTreeMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT person_id, last_desired_active, idle_since, last_department_id, \
             last_employment_state, \
             last_operational, active_transition_id, updated_at, \
             agent_quiet_at, agent_active_at, operator_wake_at \
             FROM person_activity WHERE slug = ?1",
        )?;
        let rows = stmt.query_map(params![slug], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })?;
        for row in rows {
            let row = row?;
            let last_employment_state = row
                .4
                .as_deref()
                .and_then(employment_state_from_str)
                .ok_or_else(|| corrupt_value("unmodeled stored value"))?;
            let state = PersonActivityState {
                person_id: row.0.clone(),
                last_employment_state,
                // #1031: these were `unwrap_or_default()`, so a NULL column
                // became `""`, which is never a key in `manifest.departments`
                // and therefore made the WHOLE ledger unreadable. An absent
                // prior placement is not a legal state — every person has been
                // somewhere — and the manifest already in hand holds the right
                // answer, so read it rather than manufacture one nothing accepts.
                last_department_id: row.3.unwrap_or_else(|| {
                    manifest
                        .people
                        .get(&row.0)
                        .map(|person| person.department_id.clone())
                        .unwrap_or_default()
                }),
                last_operational: row.5.unwrap_or(0) != 0,
                last_desired_active: row.1.unwrap_or(0) != 0,
                agent_quiet_at: row.8,
                idle_since: row.2,
                agent_active_at: row.9,
                operator_wake_at: row.10,
                active_transition_id: row.6,
                updated_at: row.7,
            };
            people.insert(row.0, state);
        }
    }
    // `person_activity` is populated by activity mutations, while a structural
    // organization operation may commit a newly hired/head person first.  That
    // short (and, after a daemon restart, potentially long) interval must not
    // make an addressed mailbox recipient unreadable: the direct-message wake
    // needs to reach reconcile so that it can start exactly that person.
    //
    // This is the normalized-row counterpart of `reconcile_people`: retain the
    // row authority when it exists, and project an absent manifest person with
    // the same desired-off seed used at organization creation.  It is a
    // read-only repair; a later activity mutation persists the missing row.  A
    // missing row is therefore distinct from an absent `activity_meta` row,
    // which remains an authority-loss error above.
    for person_id in &manifest.people_order {
        if people.contains_key(person_id) {
            continue;
        }
        let Some(state) = super::seed_person_state(manifest, person_id, &created_at) else {
            return Err(corrupt_value("manifest person cannot seed activity state"));
        };
        people.insert(person_id.clone(), state);
    }
    // person_order mirrors the manifest (reconcile keeps them in lockstep); the
    // table stores no order column because the manifest is the one authority.
    let person_order = manifest.people_order.clone();

    // --- aggregate scalars -------------------------------------------------
    let counter: i64 = tx
        .query_row(
            "SELECT value FROM counters WHERE name = ?1",
            params![transitions_counter_key(slug)],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let max_embedded =
        transition_order.iter().filter_map(|id| embedded_sequence(id)).max().unwrap_or(0);
    // `next = counter + 1`, but never below `max embedded id + 1` (covers a
    // migration read where transition rows exist before the counter is seeded).
    let next_transition_sequence = u64::try_from(counter)
        .map_err(|_| corrupt_value("value out of range"))?
        .saturating_add(1)
        .max(max_embedded + 1);

    let updated_at: Option<String> = tx
        .query_row("SELECT MAX(at) FROM org_events WHERE slug = ?1", params![slug], |r| r.get(0))
        .optional()?
        .flatten();

    Ok(Some(ActivityLedger {
        schema_version: ACTIVITY_SCHEMA_VERSION,
        organization: manifest.slug.clone(),
        person_order,
        people,
        transition_order,
        transitions,
        next_transition_sequence,
        automatic_park_cursor: automatic_park_cursor
            .map(|c| usize::try_from(c).map_err(|_| corrupt_value("value out of range")))
            .transpose()?,
        created_at: created_at.clone(),
        updated_at: updated_at.unwrap_or(created_at),
    }))
}

/// Diff a whole `ledger` into the normalized rows for `slug`, returning the
/// entities that actually changed (one [`EventTouch`] each) for the enclosing
/// [`rows_txn::apply_and_emit`] to record on the feed. Only changed rows are
/// rewritten, so an idle re-publish touches nothing and advances no seq.
///
/// # Errors
/// `ChiefdError::Corrupt` on a value that cannot be stored, or any `rusqlite`
/// failure.
pub fn write_rows(
    tx: &Transaction<'_>,
    slug: &str,
    ledger: &ActivityLedger,
    manifest: &OrganizationManifest,
) -> rusqlite::Result<Vec<EventTouch>> {
    let current = read_rows(tx, slug, manifest)?;
    let mut touches = Vec::new();

    // Transitions: upsert changed, delete vanished.
    let current_transitions = current.as_ref().map(|c| &c.transitions);
    for (id, transition) in &ledger.transitions {
        let unchanged = current_transitions
            .and_then(|m| m.get(id))
            .is_some_and(|existing| existing == transition);
        if unchanged {
            continue;
        }
        upsert_transition(tx, slug, transition)?;
        touches.push(EventTouch::new("transition", id.clone(), "upsert", "transitions", slug));
    }
    if let Some(current) = &current {
        for id in current.transitions.keys() {
            if ledger.transitions.contains_key(id) {
                continue;
            }
            delete_transition(tx, slug, id)?;
            touches.push(EventTouch::new("transition", id.clone(), "delete", "transitions", slug));
        }
    }

    // Per-person activity: upsert changed, delete vanished.
    let current_people = current.as_ref().map(|c| &c.people);
    for (person_id, state) in &ledger.people {
        let unchanged =
            current_people.and_then(|m| m.get(person_id)).is_some_and(|existing| existing == state);
        if unchanged {
            continue;
        }
        upsert_person_activity(tx, slug, state)?;
        touches.push(EventTouch::new(
            "person-activity",
            person_id.clone(),
            "upsert",
            "person_activity",
            slug,
        ));
    }
    if let Some(current) = &current {
        for person_id in current.people.keys() {
            if ledger.people.contains_key(person_id) {
                continue;
            }
            tx.execute(
                "DELETE FROM person_activity WHERE slug = ?1 AND person_id = ?2",
                params![slug, person_id],
            )?;
            touches.push(EventTouch::new(
                "person-activity",
                person_id.clone(),
                "delete",
                "person_activity",
                slug,
            ));
        }
    }

    // Aggregate scalars: activity_meta singleton + the transitions counter.
    let cursor = ledger
        .automatic_park_cursor
        .map(|c| i64::try_from(c).map_err(|_| corrupt_value("value out of range")))
        .transpose()?;
    let meta_changed = current.as_ref().is_none_or(|c| {
        c.automatic_park_cursor != ledger.automatic_park_cursor || c.created_at != ledger.created_at
    });
    tx.execute(
        "INSERT INTO activity_meta(slug, automatic_park_cursor, created_at) \
         VALUES(?1, ?2, ?3) \
         ON CONFLICT(slug) DO UPDATE SET automatic_park_cursor = ?2, created_at = ?3",
        params![slug, cursor, ledger.created_at],
    )?;
    // The counter holds the LAST allocated sequence (== next - 1). Never below
    // its current value: the sequence is monotonic even across a prune.
    let last_allocated = i64::try_from(ledger.next_transition_sequence.saturating_sub(1))
        .map_err(|_| corrupt_value("value out of range"))?;
    tx.execute(
        "INSERT INTO counters(name, value) VALUES(?1, ?2) \
         ON CONFLICT(name) DO UPDATE SET value = MAX(value, ?2)",
        params![transitions_counter_key(slug), last_allocated],
    )?;
    if meta_changed {
        touches.push(EventTouch::new("activity", slug, "publish", "activity_meta", slug));
    }

    Ok(touches)
}

fn upsert_transition(
    tx: &Transaction<'_>,
    slug: &str,
    t: &GracefulTransition,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO transitions(slug, id, person_id, action, status, intent_id, reason, \
         placement_department_id, \
         to_department_id, requested_at, handoff_deadline_at, \
         applied_at, cancelled_at, forced_at, abandoned_at) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15) \
         ON CONFLICT(slug, id) DO UPDATE SET \
         person_id=?3, action=?4, status=?5, intent_id=?6, reason=?7, \
         placement_department_id=?8, \
         to_department_id=?9, requested_at=?10, handoff_deadline_at=?11, \
         applied_at=?12, cancelled_at=?13, forced_at=?14, abandoned_at=?15",
        params![
            slug,
            t.id,
            t.person_id,
            t.action.as_str(),
            t.status.as_str(),
            t.intent_id,
            t.reason,
            t.placement_department_id,
            t.to_department_id,
            t.requested_at,
            t.handoff_deadline_at,
            t.applied_at,
            t.cancelled_at,
            t.forced_at,
            t.abandoned_at,
        ],
    )?;
    Ok(())
}

fn delete_transition(tx: &Transaction<'_>, slug: &str, id: &str) -> rusqlite::Result<()> {
    // #751-P4: this used to delete the transition's `reflection_handoff_items`
    // and `reflection_handoffs` rows first (items FK'd the handoff head). Both
    // tables are gone, so a transition is a single row with nothing beneath it.
    tx.execute("DELETE FROM transitions WHERE slug = ?1 AND id = ?2", params![slug, id])?;
    Ok(())
}

// TOMBSTONE: `delete_person_state` — every `transitions` row a person owned
// plus their `person_activity` row, deleted together. It existed for exactly
// one caller, `org_ops::remove_department_tree`, and for exactly one reason:
// the row model's invariant is `person_activity ⊆ manifest.people_order`, so a
// removal that hard-deleted the `people` row had to take the activity rows with
// it or leave the `/v1/org/activity/read` reconstruction an exact-order
// corruption on the very next `org_roster` read (the #526 roster wedge:
// `person_order=[chief]` from the manifest, `people=[ceo, eng-head]` from the
// orphaned rows).
//
// That removal no longer deletes anybody — it offboards them, and their row
// stays in `people_order`. The invariant now holds because BOTH sides are
// retained, which is the stronger reason. Deleting these rows today would break
// it in the other direction and would also destroy the transition history of a
// person the company still remembers.

// TOMBSTONE (#751-P4): `ReflectionOwner` (the denormalized person/action/
// columns on a handoff row), `MemoryReflection` (one folded
// `reflection-memory/<person>` record), `write_reflection` and `insert_item`
// all wrote the two now-dropped reflection tables. Deleted whole; there is no
// payload left to persist beside a transition.

fn upsert_person_activity(
    tx: &Transaction<'_>,
    slug: &str,
    s: &PersonActivityState,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO person_activity(slug, person_id, last_desired_active, idle_since, \
         last_department_id, \
         last_employment_state, last_operational, active_transition_id, updated_at, \
         agent_quiet_at, agent_active_at, operator_wake_at) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) \
         ON CONFLICT(slug, person_id) DO UPDATE SET \
         last_desired_active=?3, idle_since=?4, last_department_id=?5, \
         last_employment_state=?6, \
         last_operational=?7, active_transition_id=?8, updated_at=?9, \
         agent_quiet_at=?10, agent_active_at=?11, operator_wake_at=?12",
        params![
            slug,
            s.person_id,
            i64::from(s.last_desired_active),
            s.idle_since,
            s.last_department_id,
            employment_state_as_str(s.last_employment_state),
            i64::from(s.last_operational),
            s.active_transition_id,
            s.updated_at,
            s.agent_quiet_at,
            s.agent_active_at,
            s.operator_wake_at,
        ],
    )?;
    Ok(())
}

// ---- Typed txn-accessors for atomic cross-store ops (fence_containment) ------
//
// M7 polarity invariant (Fable-ratified): an atomic op that spans stores must
// COMPOSE each owning store's typed txn-accessor inside ITS OWN `BEGIN
// IMMEDIATE`, never hand-roll raw cross-store SQL. These three are the minimal
// primitives `org_ops::shutdown_person` composes; the activity module owns its
// person_activity/transitions SQL (containment), and each returns the SAME
// [`EventTouch`] the diff path emits so the caller feeds `apply_and_emit`
// identically. The caller holds the clock authority (store/backend split): every
// NOT-NULL timestamp column is written from the caller's `at`.

/// Set a person's desired-active flag and active-transition pointer, leaving
/// `idle_since` / `last_*_department_id` / employment columns UNTOUCHED (an
/// UPSERT that updates only these three columns; on a first-write insert the
/// untouched columns default to NULL — there is no prior value to preserve).
/// `at` becomes `updated_at`. Returns the `person-activity` touch.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn upsert_person_activity_desired(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    last_desired_active: bool,
    active_transition_id: Option<&str>,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "INSERT INTO person_activity(slug, person_id, last_desired_active, active_transition_id, updated_at) \
         VALUES(?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(slug, person_id) DO UPDATE SET \
         last_desired_active = ?3, active_transition_id = ?4, updated_at = ?5",
        params![slug, person_id, i64::from(last_desired_active), active_transition_id, at],
    )?;
    Ok(EventTouch::new("person-activity", person_id, "upsert", "person_activity", slug))
}

/// Raise one existing or newly-created activity demand without changing any
/// other person's projection.  An explicit start or durable mailbox wake owns
/// this narrow growth decision; it must not wait for a later aggregate planner
/// pass before the live reconciler can see the demand.
///
/// The current transition pointer is preserved.  In particular, a mailbox
/// write does not silently countermand an attended lifecycle decision; the
/// activity reconciler remains the authority that settles that transition.
/// Returns `None` when the row already records desired-active, avoiding an
/// event-stream storm when a pending mailbox is observed more than once.
pub fn ensure_person_activity_desired_active(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    at: &str,
) -> rusqlite::Result<Option<EventTouch>> {
    use rusqlite::OptionalExtension;
    let current: Option<(bool, Option<String>)> = tx
        .query_row(
            "SELECT last_desired_active, active_transition_id FROM person_activity \
             WHERE slug = ?1 AND person_id = ?2",
            params![slug, person_id],
            |row| Ok((row.get::<_, i64>(0)? != 0, row.get(1)?)),
        )
        .optional()?;
    if matches!(current, Some((true, _))) {
        return Ok(None);
    }
    let pointer = current.and_then(|(_, pointer)| pointer);
    upsert_person_activity_desired(tx, slug, person_id, true, pointer.as_deref(), at).map(Some)
}

/// Start a new two-minute idle lease for one explicit person start.
///
/// An explicit start is a new operator decision. It must not inherit the
/// person's quiet or idle clock from an earlier run, even when their previous
/// desired-active projection has not yet been withdrawn. The start instant is
/// therefore the new quiet-lease baseline until the agent reports working or
/// quiet through the normal activity route. This accessor owns only the
/// activity row and is composed with the launch-intent writer in the caller's
/// transaction.
///
/// Returns `None` only for an exact replay at the same timestamp.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn begin_explicit_start_idle_lease(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    at: &str,
) -> rusqlite::Result<Option<EventTouch>> {
    let changed = tx.execute(
        "UPDATE person_activity \
         SET agent_quiet_at = ?3, idle_since = ?3, agent_active_at = NULL, updated_at = ?3 \
         WHERE slug = ?1 AND person_id = ?2 \
           AND (agent_quiet_at IS NOT ?3 OR idle_since IS NOT ?3 OR agent_active_at IS NOT NULL)",
        params![slug, person_id, at],
    )?;
    Ok((changed > 0)
        .then(|| EventTouch::new("person-activity", person_id, "upsert", "person_activity", slug)))
}

/// The people this company's activity projection currently wants active.
///
/// The read-side counterpart of [`upsert_person_activity_desired`], and the
/// same containment argument: `person_activity` is this module's table, so a
/// verb in another slice composes this accessor inside its own `BEGIN
/// IMMEDIATE` rather than hand-rolling the query — which also means it never
/// has to re-spell the `activity` store key at its own error boundary (it maps
/// through [`activity_store_failed`]).
///
/// A company with no activity rows yet yields an empty set. That is fail-safe
/// for the caller that reads it: skipping nobody can only prolong a block,
/// never lose a reset.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn desired_active_people(
    tx: &Transaction<'_>,
    slug: &str,
) -> rusqlite::Result<std::collections::BTreeSet<String>> {
    let mut statement = tx.prepare(
        "SELECT person_id FROM person_activity \
         WHERE slug = ?1 AND last_desired_active = 1",
    )?;
    let rows = statement.query_map(params![slug], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// Return whether a person has a persisted activity projection and whether its
/// pause lifecycle is currently active. This deliberately reads only the
/// pause-owned booleans plus the existence of an open transition: malformed
/// descriptive projection columns are repaired from authoritative organization
/// rows by the pause transaction, while malformed transition facts remain for
/// the strict whole-ledger reader to reject.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn pause_activity_status(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
) -> rusqlite::Result<(bool, bool)> {
    let projected: Option<(Option<i64>, Option<i64>)> = tx
        .query_row(
            "SELECT last_desired_active, last_operational FROM person_activity \
             WHERE slug = ?1 AND person_id = ?2",
            params![slug, person_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let open_transition: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM transitions WHERE slug = ?1 AND person_id = ?2 \
         AND status IN ('awaiting_handoff', 'overdue', 'ready'))",
        params![slug, person_id],
        |row| row.get(0),
    )?;
    let has_projection = projected.is_some();
    let active = projected.as_ref().is_some_and(|(desired, operational)| {
        desired.unwrap_or(0) != 0 || operational.unwrap_or(0) != 0
    }) || open_transition;
    Ok((has_projection, active))
}

/// Retract durable desired-active state for every person except `keep_person_id`.
/// Only rows that are currently desired-active are touched; transition pointers,
/// idle leases, placement, employment, and operational snapshots are preserved.
/// The caller composes the returned touches into its own atomic event batch.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn retract_non_kept_desired_active(
    tx: &Transaction<'_>,
    slug: &str,
    keep_person_id: &str,
    at: &str,
) -> rusqlite::Result<Vec<EventTouch>> {
    let rows: Vec<(String, Option<String>)> = {
        let mut statement = tx.prepare(
            "SELECT person_id, active_transition_id FROM person_activity \
             WHERE slug = ?1 AND person_id <> ?2 AND last_desired_active = 1 \
             ORDER BY person_id",
        )?;
        let collected = statement
            .query_map(params![slug, keep_person_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        collected
    };
    rows.into_iter()
        .map(|(person_id, active_transition_id)| {
            upsert_person_activity_desired(
                tx,
                slug,
                &person_id,
                false,
                active_transition_id.as_deref(),
                at,
            )
        })
        .collect()
}

/// Atomically re-arm a person's automatic-idle lease after durable work
/// arrives. This is deliberately narrower than [`supersede_open_transition`]:
/// only an unowned `park` transition may be cancelled. An explicit
/// stop/transfer remains the operator's decision and is never bypassed by an
/// inbox write.
///
/// The mailbox delta calls this in the SAME `BEGIN IMMEDIATE` as the incoming
/// row. That closes the race where activity reconciliation had already chosen
/// an automatic settle, then a message committed immediately before the stale
/// projection removed the recipient's pane. A missing activity row is a safe
/// no-op: the mailbox wake path owns starting a person who is not currently
/// live.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn rearm_automatic_settle_for_activity(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    at: &str,
) -> rusqlite::Result<Vec<EventTouch>> {
    let active_transition_id: Option<Option<String>> = tx
        .query_row(
            "SELECT active_transition_id FROM person_activity WHERE slug = ?1 AND person_id = ?2",
            params![slug, person_id],
            |row| row.get(0),
        )
        .optional()?;
    // A message for a person who is entirely down is handled by the mailbox
    // wake authority. Do not manufacture an activity row here: doing so would
    // turn a mailbox append into an implicit fleet start.
    let Some(_active_transition_id) = active_transition_id else {
        return Ok(Vec::new());
    };

    // Keep this wire value identical to [`super::IDLE_AUTO_PARK_REASON`] — it
    // is compared against the stored `reason` column, so the two must not
    // drift. `intent_id IS NULL` alone is not enough: an older/manual unowned
    // park is still a deliberate lifecycle action and durable mail must never
    // countermand it. (#751-P4 changed the text with the constant; no company
    // state is migrated, per the disposable-test-data ruling.)
    const IDLE_AUTO_PARK_REASON: &str = super::IDLE_AUTO_PARK_REASON;
    let open_transition: Option<(String, String, Option<String>, String)> = tx
        .query_row(
            "SELECT id, action, intent_id, reason FROM transitions \
             WHERE slug = ?1 AND person_id = ?2 \
               AND status IN ('awaiting_handoff', 'overdue', 'ready')",
            params![slug, person_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;

    let mut touches = Vec::new();
    let automatic_park_id = match open_transition {
        Some((id, action, None, reason)) if action == "park" && reason == IDLE_AUTO_PARK_REASON => {
            Some(id)
        }
        // A direct `stop`/`transfer` is an owned lifecycle decision. New work
        // is recorded durably, but it must not silently countermand it.
        Some(_) => return Ok(touches),
        None => None,
    };
    if let Some(transition_id) = automatic_park_id.as_deref() {
        tx.execute(
            "UPDATE transitions SET status = 'cancelled', cancelled_at = ?3, \
             reason = 'superseded-by-durable-activity' WHERE slug = ?1 AND id = ?2",
            params![slug, transition_id, at],
        )?;
        touches.push(EventTouch::new("transition", transition_id, "upsert", "transitions", slug));
    }

    tx.execute(
        "UPDATE person_activity SET last_desired_active = 1, idle_since = NULL, \
         active_transition_id = CASE WHEN active_transition_id = ?3 THEN NULL ELSE active_transition_id END, \
         updated_at = ?4 WHERE slug = ?1 AND person_id = ?2",
        params![slug, person_id, automatic_park_id, at],
    )?;
    // A live row always changed meaningfully: it either cancelled a pending
    // automatic park or reset the quiet lease before projection may reap it.
    touches.push(EventTouch::new("person-activity", person_id, "upsert", "person_activity", slug));
    Ok(touches)
}

/// Seed the complete last-known activity projection for a newly inserted
/// person, desired-off. Unlike [`upsert_person_activity_desired`], this is a
/// first-write accessor: every scalar the normalized activity reader requires
/// is populated, so a hire cannot leave an unreadable partial activity row.
///
/// TOMBSTONE (#751-P9): this took a trailing-but-one `pane_department_id`
/// argument the caller had to derive head-in-parent for. The column is gone;
/// the caller supplies the person's own unit and nothing else.
///
/// # Errors
/// Any `rusqlite` failure.
#[allow(clippy::too_many_arguments)]
pub fn insert_person_activity_desired_off(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    employment_state: EmploymentState,
    department_id: &str,
    operational: bool,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    let state = PersonActivityState {
        person_id: person_id.to_string(),
        last_employment_state: employment_state,
        last_department_id: department_id.to_string(),
        last_operational: operational,
        last_desired_active: false,
        agent_quiet_at: None,
        idle_since: None,
        agent_active_at: None,
        operator_wake_at: None,
        active_transition_id: None,
        updated_at: at.to_string(),
    };
    upsert_person_activity(tx, slug, &state)?;
    Ok(EventTouch::new("person-activity", person_id, "upsert", "person_activity", slug))
}

/// Insert ONE terminal (`status = 'applied'`) transition — the durable record a
/// shutdown writes for the park/offboard it just applied. `at` becomes both
/// `requested_at` and `applied_at`; `status` is `applied` (a terminal row is
/// OUTSIDE the `transitions_one_active` partial index, so it never contends with
/// a live one). `intent_id` carries the transition's OWNER — a COMMANDED stop
/// stamps `"person-stop:<id>"` (an owned park keeps its stop intent, Fable),
/// while an automatic settle passes `None` (an unowned idle park). Returns the
/// `transition` touch. Call [`supersede_open_transition`] first if an open
/// transition may exist.
///
/// # Errors
/// Any `rusqlite` failure (a duplicate `id` is a real caller error, surfaced).
#[allow(clippy::too_many_arguments)]
pub fn insert_terminal_transition(
    tx: &Transaction<'_>,
    slug: &str,
    id: &str,
    person_id: &str,
    action: TransitionAction,
    placement_department_id: Option<&str>,
    intent_id: Option<&str>,
    reason: &str,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "INSERT INTO transitions(slug, id, person_id, action, status, intent_id, reason, \
         placement_department_id, \
         requested_at, handoff_deadline_at, applied_at) \
         VALUES(?1, ?2, ?3, ?4, 'applied', ?5, ?6, ?7, ?8, ?8, ?8)",
        params![
            slug,
            id,
            person_id,
            action.as_str(),
            intent_id,
            reason,
            placement_department_id,
            at,
        ],
    )?;
    Ok(EventTouch::new("transition", id, "upsert", "transitions", slug))
}

/// Insert an ABANDONED terminal transition — the sanctioned record for a
/// lifecycle change that completed with nobody releasing it (CHANGELOG
/// 2026-07-23, "When the fence proves a person cannot run, the unreachable
/// handoff is abandoned and the structural change applies unattended"):
/// `cancelled` with an explicit `abandoned_at` marker, NEVER `applied`, so
/// nothing claims a release that did not happen. `applied` and `cancelled +
/// abandoned_at` are both terminal; the distinction is the honest one between
/// "the owner released it" and "nobody could".
///
/// `handoff_deadline_at` is stamped with `at` (a terminal row owes nobody).
#[allow(clippy::too_many_arguments)]
pub fn insert_abandoned_transition(
    tx: &Transaction<'_>,
    slug: &str,
    id: &str,
    person_id: &str,
    action: TransitionAction,
    placement_department_id: Option<&str>,
    intent_id: Option<&str>,
    reason: &str,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "INSERT INTO transitions(slug, id, person_id, action, status, intent_id, reason, \
         placement_department_id, \
         requested_at, handoff_deadline_at, cancelled_at, abandoned_at) \
         VALUES(?1, ?2, ?3, ?4, 'cancelled', ?5, ?6, ?7, ?8, ?8, ?8, ?8)",
        params![
            slug,
            id,
            person_id,
            action.as_str(),
            intent_id,
            reason,
            placement_department_id,
            at,
        ],
    )?;
    Ok(EventTouch::new("transition", id, "upsert", "transitions", slug))
}

/// Insert the terminal audit left by an atomic unit pause after its synthetic
/// handoff is immediately superseded by that same pause. Unlike an abandoned
/// transition, this cancelled row carries its unit-stop intent: the pause
/// itself is what made the one-call structural change safe. The row is inserted
/// directly in its terminal shape because no intermediate state is observable
/// inside the enclosing `BEGIN IMMEDIATE` transaction.
///
/// TOMBSTONE (#751-P4): this was `insert_cancelled_transition_with_reflection`
/// and took a trailing `&ReflectionHandoff` it wrote to the now-dropped
/// `reflection_handoffs` table. The transition row is the whole record.
///
/// # Errors
/// Any `rusqlite` failure (including a duplicate transition id).
#[allow(clippy::too_many_arguments)]
pub fn insert_cancelled_transition(
    tx: &Transaction<'_>,
    slug: &str,
    id: &str,
    person_id: &str,
    action: TransitionAction,
    placement_department_id: Option<&str>,
    intent_id: &str,
    reason: &str,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "INSERT INTO transitions(slug, id, person_id, action, status, intent_id, reason, \
         placement_department_id, \
         requested_at, handoff_deadline_at, cancelled_at) \
         VALUES(?1, ?2, ?3, ?4, 'cancelled', ?5, ?6, ?7, ?8, ?8, ?8)",
        params![
            slug,
            id,
            person_id,
            action.as_str(),
            intent_id,
            reason,
            placement_department_id,
            at,
        ],
    )?;
    Ok(EventTouch::new("transition", id, "upsert", "transitions", slug))
}

/// Insert a graceful `awaiting_handoff` transition — the opening of a bounded
/// lifecycle handoff (the offboard e2e's expected shape: the person must
/// release it before the structural change applies). Carries the person's
/// current placement, a non-empty reason, and a real deadline.
#[allow(clippy::too_many_arguments)]
pub fn insert_awaiting_handoff_transition(
    tx: &Transaction<'_>,
    slug: &str,
    id: &str,
    person_id: &str,
    action: TransitionAction,
    placement_department_id: &str,
    intent_id: Option<&str>,
    reason: &str,
    at: &str,
    handoff_deadline_at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "INSERT INTO transitions(slug, id, person_id, action, status, intent_id, reason, \
         placement_department_id, \
         requested_at, handoff_deadline_at) \
         VALUES(?1, ?2, ?3, ?4, 'awaiting_handoff', ?5, ?6, ?7, ?8, ?9)",
        params![
            slug,
            id,
            person_id,
            action.as_str(),
            intent_id,
            reason,
            placement_department_id,
            at,
            handoff_deadline_at,
        ],
    )?;
    Ok(EventTouch::new("transition", id, "upsert", "transitions", slug))
}

/// The person's open transition id when that transition is a READY handoff for
/// `action` — the handoff an op would mint its own graceful row to demand is
/// already satisfied. `None` for no open transition, a different action, or any
/// not-yet-released status. Read-only companion to
/// [`supersede_open_transition`]: an op that would supersede-and-remint its own
/// graceful row checks here first, because superseding a READY row throws away
/// a release that already happened, and the fresh row can never be released — a
/// departed person has no pane to release from, so the reconcile retains their
/// pane forever (the live staffing-no-release-fence wedge).
///
/// # Errors
/// Any `rusqlite` failure.
pub fn ready_open_transition_id(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    action: TransitionAction,
) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT id FROM transitions \
         WHERE slug = ?1 AND person_id = ?2 AND action = ?3 AND status = 'ready'",
        params![slug, person_id, action.as_str()],
        |r| r.get(0),
    )
    .optional()
}

/// The people whose operator wake lease is still running at `now_ms`.
///
/// The row-level read of `activity::operator_wake_lease_active`, for the one
/// caller that cannot hold a `PersonActivityState`:
/// [`launch_intent_rows::publish`](crate::store::launch_intent_rows::publish)
/// runs inside a bare transaction and must answer "may I withdraw this fence?"
/// against the same rule the reconcile uses. Composed rather than hand-rolled
/// because `person_activity` is this module's table (M7 containment).
///
/// An unparseable stamp contributes nobody — fail-safe in the direction that
/// keeps the settle working, exactly as the typed predicate is.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn operator_wake_leased_people(
    tx: &Transaction<'_>,
    slug: &str,
    now_ms: i64,
) -> rusqlite::Result<std::collections::BTreeSet<String>> {
    let mut statement = tx.prepare(
        "SELECT person_id, operator_wake_at FROM person_activity \
         WHERE slug = ?1 AND operator_wake_at IS NOT NULL",
    )?;
    let rows = statement
        .query_map(params![slug], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    let mut leased = std::collections::BTreeSet::new();
    for row in rows {
        let (person_id, woke_at) = row?;
        if crate::isotime::parse_iso_millis(&woke_at).is_some_and(|woke_at| {
            woke_at <= now_ms
                && now_ms
                    < woke_at + crate::store::activity::ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS
        }) {
            leased.insert(person_id);
        }
    }
    Ok(leased)
}

/// Let go of the person's ROUTINE IDLE PARK, whatever state it reached — the
/// row half of a wake.
///
/// # Why a wake has to do this, and why a launch-intent grant alone does not
///
/// `project_activity_fence` turns a fenced person who is not yet
/// desired-active into `ActivityReason::Requested`, and that reason is the
/// only thing that brings a stopped person up. It suppresses the reason for
/// two people, and a parked person is BOTH of them: anybody already
/// desired-active, and (#638) anybody whose routine idle park has reached a
/// terminal status. The second filter exists so a LAPSED start decision is not
/// re-read as fresh demand — but the settle path leaves the applied park as
/// the person's `active_transition_id` forever, so it also discards a grant
/// made minutes later, on purpose, by an operator pointing at that person. The
/// grant is then withdrawn again by the same pass's shrink half, silently, for
/// as long as anybody keeps asking.
///
/// So the wake releases the park in the same transaction as the grant. It is
/// not a second opinion about who may run — `launch_intent` remains the sole
/// authority — it is the removal of a settled record that has already been
/// acted on, which is exactly what `activity::reconcile` does for itself when
/// work arrives for a parked person ("ordinary idle parking yields to newly
/// arrived work").
///
/// **Only a routine idle park.** `action = 'park'`, `intent_id IS NULL`, and
/// the reason `activity::IDLE_AUTO_PARK_REASON` — an operator's park or a
/// lifecycle command's park is somebody's explicit decision and is never
/// touched here. An OPEN row is cancelled with the override fact in its
/// `reason`, exactly like [`supersede_open_transition`]; a TERMINAL row is
/// left standing as the historical fact it is and only the pointer is
/// dropped. Either way the pointer goes, because that is what the fence reads.
/// The caller supplies the durable override fact because both an explicit
/// start and a direct wake can release this scheduler row, and the transition
/// history must name the action that actually superseded it.
///
/// # THE QUIET CLOCK IS RESET TOO, and this is the whole of a live defect
///
/// Cancelling the park removed the DECISION and left the REASON for it standing.
/// `agent_quiet_at` is when the person last went quiet, and nothing here used to
/// touch it — so a person woken after an hour asleep was, to the very next
/// reconcile, somebody who had been quiet for an hour. It parked them again
/// immediately.
///
/// Measured on the operator's own company: `rhea` went quiet at 23:18:42 and was
/// clicked at 00:01:43. Her pane came up, and thirty seconds later the log read
/// `launch intent withdrawn (settled)`. She never stayed. The operator's report
/// was that she "shows starting and never resolves", which is exactly what a
/// wake-park-reap loop looks like from the rail.
///
/// So a wake clears the clock the same way an agent's own `working` beat does
/// (`activity.rs`: `agent_active_at = now, agent_quiet_at = None, idle_since =
/// None`). That is the honest reading: the operator has just asked for this
/// person, so whatever silence preceded the wake is spent, and the countdown to
/// the next park starts from the wake rather than from before it.
///
/// It is cleared UNCONDITIONALLY, before the park lookup, because the two facts
/// are independent: a person can carry a stale quiet clock with no park row
/// (their intent was already withdrawn), and that person is precisely the one a
/// wake must not hand straight back to the settle.
///
/// # AND THE WAKE ITSELF IS RECORDED
///
/// Clearing the clock says only "whatever silence preceded the wake is spent".
/// It does not say that an operator asked for this person, so the moment their
/// agent beats once and has nothing to do, every rule in the product reads them
/// as an agent that finished — and settles them, or withdraws their launch
/// intent as a fence with no demand behind it, seconds after the click.
///
/// Operator ruling, 2026-08-20: *"If I tell chief to message it, it'll come back
/// up and do the 2min settling. We need it to always do that when woken. Message
/// or not. If woken, it needs to wait the 2 mins."* So this stamps
/// `person_activity.operator_wake_at`, which is the durable record of the
/// decision, and `activity::operator_wake_lease_active` is how the park rule,
/// the demand rule and the fence writer all read it. This function is THE row
/// half of a wake and is called by both `org_ops::wake_person` and
/// `org_ops::start_person`, which is why the stamp lives here and not at either
/// call site: one definition, both operator doors.
///
/// Returns the touches to fold into the caller's event feed.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn release_idle_park(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    at: &str,
    override_fact: &str,
) -> rusqlite::Result<Vec<EventTouch>> {
    use rusqlite::OptionalExtension;
    // THE CLOCK FIRST, AND WHETHER OR NOT A PARK EXISTS. See the doc above: the
    // park is the decision and the quiet clock is the reason, and leaving the
    // reason behind is what re-parked a person seconds after the operator asked
    // for them.
    let mut touches = Vec::with_capacity(3);
    // THE WAKE INSTANT, STAMPED UNCONDITIONALLY AND FIRST. See
    // `activity::operator_wake_lease_active`: this is the operator's own
    // decision, and it is the only durable record of one. Unconditional because
    // the clock-clearing UPDATE below is conditional — a person carrying no
    // quiet clock at all is precisely the one a wake must not hand straight back
    // to the settle — and because a SECOND wake must restart the floor rather
    // than inherit the first one's remaining seconds.
    //
    // It is deliberately not folded into the same statement: that one is a
    // repair of a stale clock and reports itself by whether it changed
    // anything, and merging the two would make every wake report a change it
    // may not have made.
    //
    // `IS NOT ?3` keeps an exact REPLAY writeless — the same click delivered
    // twice, or a retried request — while a click at a LATER instant writes,
    // because that one is a second decision and it restarts the floor.
    let stamped = tx.execute(
        "UPDATE person_activity SET operator_wake_at = ?3, updated_at = ?3 \
         WHERE slug = ?1 AND person_id = ?2 AND operator_wake_at IS NOT ?3",
        params![slug, person_id, at],
    )?;
    if stamped > 0 {
        touches.push(EventTouch::new(
            "person-activity",
            person_id,
            "upsert",
            "person_activity",
            slug,
        ));
    }

    // THE CLOCKS GO, AND NOTHING IS INVENTED IN THEIR PLACE.
    //
    // This used to write `agent_active_at = <the wake instant>` — chiefd
    // asserting that the agent had just reported activity, to buy the person a
    // liveness window while their pane started. That claim is now read as an
    // ANSWER: `fence_still_supplies_demand` (converge_apply/cycle.rs) keeps a
    // grant as demand only while the person has said NOTHING, so the wake's own
    // stamp made the next pass conclude the agent had spoken, drop the demand,
    // and let the shrink half sweep the grant the wake had just made. The wake
    // defeated itself.
    //
    // Measured on a live box, 2026-08-20: `engineering-kimi3` was hired
    // at 17:06:41 and clicked awake; his row was left `agent_active_at =
    // 17:08:50` with no quiet stamp, no launch-intent row and
    // `last_desired_active = 0`, and no pane was ever started for him. Nothing
    // but this statement can have written that stamp — his pane never existed
    // to report anything.
    //
    // The honest state for a person about to start is "no report yet", which is
    // all three columns absent — the same rule the rising edge in
    // `activity.rs` already applies when somebody crosses into desired-active.
    // The grant itself is what holds them up now, and it holds until the agent
    // genuinely answers.
    //
    // UNCONDITIONAL, and that is the second half of the repair: the WHERE used
    // to require a clock to already exist, so a person whose clocks were
    // already clear got no write at all — including no clearing of a stale
    // `agent_active_at` left by an earlier run.
    let cleared = tx.execute(
        "UPDATE person_activity \
         SET agent_quiet_at = NULL, idle_since = NULL, agent_active_at = NULL, updated_at = ?3 \
         WHERE slug = ?1 AND person_id = ?2 \
           AND (agent_quiet_at IS NOT NULL OR idle_since IS NOT NULL \
                OR agent_active_at IS NOT NULL)",
        params![slug, person_id, at],
    )?;
    if cleared > 0 {
        touches.push(EventTouch::new(
            "person-activity",
            person_id,
            "upsert",
            "person_activity",
            slug,
        ));
    }
    let parked: Option<(String, String)> = tx
        .query_row(
            "SELECT t.id, t.status FROM transitions t \
             JOIN person_activity p \
               ON p.slug = t.slug AND p.person_id = t.person_id \
              AND p.active_transition_id = t.id \
             WHERE t.slug = ?1 AND t.person_id = ?2 \
               AND t.action = 'park' AND t.intent_id IS NULL AND t.reason = ?3",
            params![slug, person_id, crate::store::activity::IDLE_AUTO_PARK_REASON],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((id, status)) = parked else {
        return Ok(touches);
    };
    if matches!(status.as_str(), "awaiting_handoff" | "overdue" | "ready") {
        tx.execute(
            "UPDATE transitions SET status = 'cancelled', cancelled_at = ?3, reason = ?4 \
             WHERE slug = ?1 AND id = ?2",
            params![slug, id, at, override_fact],
        )?;
        touches.push(EventTouch::new("transition", id.clone(), "upsert", "transitions", slug));
    }
    tx.execute(
        "UPDATE person_activity SET active_transition_id = NULL, updated_at = ?3 \
         WHERE slug = ?1 AND active_transition_id = ?2",
        params![slug, id, at],
    )?;
    touches.push(EventTouch::new("person-activity", person_id, "upsert", "person_activity", slug));
    Ok(touches)
}

/// Cancel the person's OPEN transition, if any — the `awaiting_handoff` /
/// `overdue` / `ready` row (at most one, per the `transitions_one_active` partial
/// unique index). Sets `status = 'cancelled'`, `cancelled_at = at`, and stamps
/// `reason` with the OVERRIDE FACT (Fable: `"superseded-by-shutdown:<intent_id>"`
/// / `"superseded-by-settle"`). That fact living on the CANCELLED row is exactly
/// why the terminal replacement stays a clean `'applied'` and never `'forced'`.
/// Returns `Some((cancelled_id, touch))`; `None` when the person has no open
/// transition.
///
/// The person's `person_activity.active_transition_id` is cleared in the same
/// commit when it names the just-cancelled row: the pointer may only ever name
/// an OPEN transition, and a ledger still pointing at a cancelled one fails the
/// whole-ledger `validate` on its next read — surfacing as `corrupt store:
/// activity` on every later publish (the TS authority clears the pointer in the
/// same commit for the same reason, org-activity.ts). A caller that installs a
/// replacement pointer (a shutdown's terminal row) overwrites the NULL with its
/// own upsert immediately after, so clearing here is never the final word for
/// that flow. No `person-activity` touch is emitted: the caller composes the
/// event feed, and the transition touch below already advances the seq every
/// watcher fences on.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn supersede_open_transition(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    reason: &str,
    at: &str,
) -> rusqlite::Result<Option<(String, EventTouch)>> {
    let open_id: Option<String> = tx
        .query_row(
            "SELECT id FROM transitions \
             WHERE slug = ?1 AND person_id = ?2 \
               AND status IN ('awaiting_handoff', 'overdue', 'ready')",
            params![slug, person_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = open_id else {
        return Ok(None);
    };
    tx.execute(
        "UPDATE transitions SET status = 'cancelled', cancelled_at = ?3, reason = ?4 \
         WHERE slug = ?1 AND id = ?2",
        params![slug, id, at, reason],
    )?;
    // Clear the dangling active-transition pointer in the same commit (see the
    // doc above): only the row that names THIS cancelled id is touched, so a
    // pointer already repurposed elsewhere is never clobbered.
    tx.execute(
        "UPDATE person_activity SET active_transition_id = NULL, updated_at = ?3 \
         WHERE slug = ?1 AND active_transition_id = ?2",
        params![slug, id, at],
    )?;
    let touch = EventTouch::new("transition", id.clone(), "upsert", "transitions", slug);
    Ok(Some((id, touch)))
}

// TOMBSTONE (#751-P4): `reflection_is_durable` — the field-for-field owner +
// payload equality check the reconcile's `has_reflection` gate relied on — and
// `fold_reflection_memory`, which folded per-person `reflection-memory/*`
// records into `reflection_handoffs` with content dedup, are deleted with the
// tables they read and wrote. There is no durability question left to ask: a
// released transition IS the transitions row.

/// Publish a whole activity ledger (serialized JSON) into the rows through the
/// direct atomic contract. Its HTTP route is deleted (the publisher-route
/// sweep found no caller); this is now an in-process seam only.
///
/// Item D: rejects any key the row model cannot represent BEFORE writing (422
/// `unmodeled-keys`), so a newer launcher field is never silently dropped. Then
/// validates against the manifest reconstructed from the company's rows and
/// diffs at entity granularity via [`rows_txn::apply_and_emit`].
///
/// # Errors
/// [`UNMODELED_KEYS`] / [`super::INVALID_INPUT`] refusals (map to 422); SQL
/// failures as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming_raw: &str,
) -> Result<i64, ChiefdError> {
    publish_impl(tx, row_slug, incoming_raw, false)
}

/// Shared body of [`publish`] and [`backfill_activity`]. Every validation check
/// (schema, ownership, person/department references, ordering) runs against
/// the reconstructed normalized organization rows.
fn publish_impl(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming_raw: &str,
    tolerate_legacy: bool,
) -> Result<i64, ChiefdError> {
    let malformed = |e: serde_json::Error| {
        ChiefdError::from(Refusal::new(
            super::INVALID_INPUT,
            format!("malformed activity ledger: {e}"),
        ))
    };
    let incoming_value: serde_json::Value =
        serde_json::from_str(incoming_raw).map_err(malformed)?;
    let ledger: ActivityLedger =
        serde_json::from_value(incoming_value.clone()).map_err(malformed)?;
    reject_unmodeled_keys(&incoming_value, &ledger, tolerate_legacy)?;

    // The activity ledger references people/departments, so the manifest rows
    // must already be present (published first). Their absence is a caller
    // ordering error, not row corruption.
    let manifest =
        crate::store::organization_rows::reconstruct(tx, row_slug)?.ok_or_else(|| {
            ChiefdError::from(Refusal::new(
                super::INVALID_INPUT,
                "cannot publish activity before the organization manifest rows exist",
            ))
        })?;
    super::validate(&ledger, &manifest)?;

    let at = ledger.updated_at.clone();
    apply_and_emit::<rusqlite::Error, _>(tx, row_slug, &at, "", |tx| {
        write_rows(tx, row_slug, &ledger, &manifest)
    })
    .map_err(activity_store_failed)
}

/// Backfill the `activity` blob into the normalized rows for one company — the
/// migration (N9) counterpart of [`publish`], mirroring `backfill_manifest`'s
/// signature so N9 wires it into the migrate dispatch beside the manifest arm.
///
/// Parses `blob` as the serialized [`ActivityLedger`] and publishes it through
/// the live row path ([`publish`]) inside the writer transaction — the rows the
/// backfill writes are indistinguishable from a normal mutation's, and
/// `org_events` is seeded exactly as ongoing writes expect. The publish's
/// item-D [`UNMODELED_KEYS`] / `INVALID_INPUT` refusals pass through so the
/// shadow-diff can turn them into loud report lines.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on a non-UTF-8 blob (a corrupt source is not a
/// caller error); the [`publish`] refusals otherwise.
pub fn backfill_activity(
    tx: &Transaction<'_>,
    row_slug: &str,
    blob: &[u8],
) -> Result<i64, ChiefdError> {
    let raw = std::str::from_utf8(blob).map_err(|e| corrupt_store("activity-blob", e))?;
    publish_impl(tx, row_slug, raw, true)
}

// TOMBSTONE (#751-P4): a whole migration family lived here — the
// `ReflectionMemoryRecord` decode of a per-person `reflection-memory/<person>`
// blob, its `REFLECTION_RECORD_KEYS` item-D allowlist and
// `reject_unmodeled_reflection_keys` write-strict gate, and
// `backfill_reflection`, which folded those records into `reflection_handoffs`
// inside the writer transaction. All of it existed to carry the TypeScript
// launcher's reflection payloads across the N9 cutover. The payload no longer
// exists in the product and the tables are dropped, so there is nothing left to
// migrate and nothing to verify.

/// `Matched` when the derivation reproduced the blob value, else `Lost` — the
/// shadow-diff helper for a value the reconstruct recomputes (constant / process
/// identity / another row / the feed) rather than storing as itself.
fn derived_if(reproduced: bool, proof: &str) -> Disposition {
    if reproduced {
        Disposition::Derived { proof: proof.to_string() }
    } else {
        Disposition::Lost {
            blob_value: format!("derivation did not reproduce the blob value ({proof})"),
        }
    }
}

/// The `activity` zero-loss verifier (N9 shadow-diff): blob → rows →
/// reconstructed [`ActivityLedger`], then a field-by-field disposition report.
/// The transaction is left holding the backfilled rows; the caller rolls it back
/// (dry-run) or commits it (cutover) — the verifier never decides. The manifest
/// rows must already be present (backfill the manifest family first); their
/// absence surfaces as a LOUD failure, not a panic.
///
/// # Errors
/// [`ChiefdError::Corrupt`] on unparseable bytes, [`ChiefdError::StoreFailure`] on a SQL
/// failure. An unmodeled
/// key / manifest-ordering refusal is NOT an error — it is a loud report line.
pub fn shadow_diff_activity(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    blob: &[u8],
) -> Result<ShadowReport, ChiefdError> {
    let mut report = ShadowReport::new("activity");
    let raw = std::str::from_utf8(blob).map_err(|e| corrupt_store("activity-blob", e))?;
    let original: ActivityLedger =
        serde_json::from_str(raw).map_err(|e| corrupt_store("activity-blob", e))?;

    // Backfill, catching the item-D refusal (and the manifest-ordering refusal)
    // as a LOUD line rather than aborting — the point is to surface every drop.
    if let Err(e) = backfill_activity(tx, row_slug, blob) {
        report.record_loud(format!("backfill refused: {e}"));
        return Ok(report);
    }

    let manifest =
        crate::store::organization_rows::reconstruct(tx, row_slug)?.ok_or_else(|| {
            crate::error::store_failure_because(
                "activity-rows",
                "no organization manifest rows, so the activity rows cannot be interpreted",
            )
        })?;
    let got =
        read_rows(tx, row_slug, &manifest).map_err(activity_store_failed)?.ok_or_else(|| {
            crate::error::store_failure_because(
                "activity-rows",
                "the activity rows are missing immediately after their own publish",
            )
        })?;

    report.row_count = got.transitions.len() + got.people.len() + 1;

    // --- DERIVED identity (constants / process identity / feed / counter) ---
    report.record(
        "schemaVersion",
        derived_if(
            got.schema_version == original.schema_version,
            "constant ACTIVITY_SCHEMA_VERSION",
        ),
    );
    report.record(
        "organization",
        derived_if(got.organization == company_slug, "process company slug (manifest.slug)"),
    );
    report.record(
        "personOrder",
        derived_if(
            got.person_order == original.person_order,
            "manifest.peopleOrder filtered to people with an activity row",
        ),
    );
    report.record(
        "transitionOrder",
        derived_if(
            got.transition_order == original.transition_order,
            "sorted by the embedded transition sequence",
        ),
    );
    report.record(
        "nextTransitionSequence",
        derived_if(
            got.next_transition_sequence == original.next_transition_sequence,
            "counters(transitions:<slug>) + 1",
        ),
    );
    report.record("updatedAt", Disposition::Derived { proof: "MAX(org_events.at)".into() });

    // --- MATCHED scalars (activity_meta singleton) -------------------------
    report.record(
        "automaticParkCursor",
        matched_or_lost_val(
            got.automatic_park_cursor == original.automatic_park_cursor,
            &original.automatic_park_cursor,
        ),
    );
    report.record(
        "createdAt",
        matched_or_lost_val(got.created_at == original.created_at, &original.created_at),
    );

    // --- per-person activity + per-transition ------------------------------
    for (id, state) in &original.people {
        report.record(
            format!("people.{id}"),
            match got.people.get(id) {
                Some(g) if g == state => Disposition::Matched,
                Some(_) => Disposition::Lost {
                    blob_value: "person-activity fields differ after round-trip".into(),
                },
                None => Disposition::Lost {
                    blob_value: "person-activity absent after round-trip".into(),
                },
            },
        );
    }
    for (id, transition) in &original.transitions {
        report.record(
            format!("transitions.{id}"),
            match got.transitions.get(id) {
                Some(g) if g == transition => Disposition::Matched,
                Some(_) => Disposition::Lost {
                    blob_value: "transition fields differ after round-trip".into(),
                },
                None => {
                    Disposition::Lost { blob_value: "transition absent after round-trip".into() }
                }
            },
        );
    }

    // --- #337-dropped legacy leaves (audit trail; tolerated on backfill) ----
    // The live blob still carries `people.<id>.automaticParkRetryAfter` (the
    // pre-`forced`-park backoff cursor); #337 removed it from the model and the
    // read path already drops it, so it is a KNOWN, intentional drop — recorded
    // ExpectedDropped so the report proves it was seen, never silently absorbed.
    if let Ok(serde_json::Value::Object(root)) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(serde_json::Value::Object(people)) = root.get("people") {
            for (id, person) in people {
                if let serde_json::Value::Object(fields) = person {
                    for key in fields.keys() {
                        if super::LEGACY_READ_ALLOWLIST.contains(&key.as_str()) {
                            report.record(
                                format!("people.{id}.{key}"),
                                Disposition::ExpectedDropped {
                                    where_now: "removed by #337 (pre-`forced`-park backoff cursor); dropped on read, dies with the blob at N9".into(),
                                },
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(report)
}

/// `Matched` when equal, else `Lost` carrying the blob value — the scalar
/// counterpart of [`derived_if`].
fn matched_or_lost_val<T: std::fmt::Debug>(equal: bool, blob_value: &T) -> Disposition {
    if equal {
        Disposition::Matched
    } else {
        Disposition::Lost { blob_value: format!("{blob_value:?}") }
    }
}

// TOMBSTONE (#751-P4): `shadow_diff_reflection` — the N9 zero-loss verifier
// that proved each `reflection-memory/<person>` blob folded into
// `reflection_handoffs` content-for-content — is deleted with the family it
// verified. `shadow_diff_activity` above still covers transitions,
// person_activity and activity_meta.

/// Item D: reject any key in the incoming JSON the row model does not represent.
/// Lenient serde silently drops unknown keys, so a key present in `incoming` but
/// absent from the re-serialized (parsed) `ledger` is exactly an unmodeled key.
///
/// `tolerate_legacy` (backfill only) subtracts the [`super::LEGACY_READ_ALLOWLIST`]
/// (the #337-dropped `automaticParkRetryAfter` leaf, Fable-ruled dropped) so a
/// real legacy snapshot is not refused — the SAME tolerance the runtime read path
/// (`parse_ledger_tolerating_legacy`) already applies. A live publish stays
/// write-strict (`tolerate_legacy=false`): the read path already strips the
/// legacy leaf, so a live mutation never legitimately carries it.
fn reject_unmodeled_keys(
    incoming: &serde_json::Value,
    ledger: &ActivityLedger,
    tolerate_legacy: bool,
) -> Result<(), ChiefdError> {
    let modeled = serde_json::to_value(ledger)
        .map_err(|e| ChiefdError::from(Refusal::new(super::INVALID_INPUT, e.to_string())))?;
    let mut paths = Vec::new();
    collect_unmodeled(incoming, &modeled, String::new(), &mut paths);
    if tolerate_legacy {
        // Leaf-name granularity, matching `parse_ledger_tolerating_legacy`.
        paths.retain(|p| {
            let leaf = p.rsplit(['.', ']', '[']).next().unwrap_or(p);
            !super::LEGACY_READ_ALLOWLIST.contains(&leaf)
        });
    }
    if paths.is_empty() {
        return Ok(());
    }
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!(
            "activity ledger carries unmodeled keys the row model cannot store: {}",
            paths.join(", ")
        ),
    )))
}

fn collect_unmodeled(
    incoming: &serde_json::Value,
    modeled: &serde_json::Value,
    prefix: String,
    out: &mut Vec<String>,
) {
    use serde_json::Value;
    match (incoming, modeled) {
        (Value::Object(i), Value::Object(m)) => {
            for (key, iv) in i {
                let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                match m.get(key) {
                    None => out.push(path),
                    Some(mv) => collect_unmodeled(iv, mv, path, out),
                }
            }
        }
        (Value::Array(i), Value::Array(m)) => {
            for (idx, iv) in i.iter().enumerate() {
                if let Some(mv) = m.get(idx) {
                    collect_unmodeled(iv, mv, format!("{prefix}[{idx}]"), out);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
