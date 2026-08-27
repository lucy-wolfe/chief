//! The store layer — the **only** module allowed to open a SQLite connection.
//!
//! `clippy.toml` bans `rusqlite::Connection::open` workspace-wide; the two
//! call sites below carry a narrow, commented `allow` so that every place
//! chiefd can acquire a connection is one `grep` away (plan §5.2 item 4).
//!
//! One SQLite database per company at `<dataRoot>/<slug>/chief.db` is the
//! durable authority for that company. Per-company databases make three hazards
//! structural: same-slug-different-root collision is impossible, `dropCompany`
//! becomes "the file leaves with the quarantined directory", and temp-dir test
//! isolation needs no namespacing discipline (plan §5.1).
//!
//! # The store registry (M7)
//!
//! This module is also the **registry**: [`declare_stores!`](crate::declare_stores)
//! is invoked exactly once, here, and it is the only way a store can come into
//! existence (the [`StoreKind`](crate::polarity::StoreKind) seal). Every entry
//! must state a polarity for every operation, so "a store was added and nobody
//! decided what happens when its bytes are unreadable" is a compile error
//! rather than a latent default.
//!
//! The per-node launch-intent fence — [`launch_intent`] — is now the single
//! authority on who is allowed to run (the fleet-suppression latch it was once
//! paired with was deleted with the CEO-only-is-a-boot decision). The rest of
//! the plan's inventory is recorded in [`STORE_POLARITY_INVENTORY`] with its
//! polarity already decided and its milestone in [`PENDING_STORES`]; the matrix
//! test asserts the two lists partition the inventory exactly, so landing a
//! store means moving its name from one list to the other and nothing else.

use std::path::Path;

use rusqlite::Connection;

use crate::polarity::{Polarity, StoreKind, StoreOp};
use crate::schema::{COMPANY_PRAGMAS, COMPANY_SCHEMA_SQL};

pub mod activity;
pub mod activity_command;
pub mod agent_contracts;
// TOMBSTONE (chief-home-is-cwd §4c): `pub mod boot_lease_rows;` stood here and
// registered the `ceo-boot-lease` singleton — the mutual-exclusion object an
// attended CEO-only boot took before its slow pre-converge phase, so chiefd's
// own reconcile duty could not re-plan the fleet underneath it. It has no
// subject: the daemon boots no pane at all now (the operator client owns every
// pane), so `launch_ceo_only_runtime` — the ONLY writer a lease ever had — is
// deleted, and with no writer every reader was answering a question about an
// event that can no longer occur. Nothing else was serialized by it: writes are
// serialized by beacond's single-daemon admission, the one writer actor with
// its `BEGIN IMMEDIATE` per mutation, and `converge_safety::begin_cycle`'s
// durable single-flight claim, none of which is this lease.
pub mod cold_start;
// TOMBSTONE: `company_session_action`. The chiefd half of #54's company-wide
// native reset and compact actions, deleted whole with the feature. Nothing in
// production could queue one — the only caller of the queue verb was chiefing's
// own client method, exercised by contract tests, and the historical queuer was
// the legacy CLI deleted in `ca2da9b57`.
pub mod context;
pub mod control_authority;
pub mod converge_safety;
pub mod converge_safety_rows;
pub mod event_journal;
pub mod event_journal_rows;
pub mod goal_delivery_quiesce_rows;
pub mod health;
pub mod health_collect;
pub mod health_monitor_rows;
pub mod identities;
pub mod launch_intent;
pub mod launch_intent_rows;
pub mod lifecycle_status;
pub mod mailbox;
pub mod mailbox_rows;
pub mod mailbox_view;
pub mod mutation_journal;
pub mod mutation_journal_rows;
pub mod operator_escalation;
pub mod operator_escalation_intents_rows;
pub mod operator_escalation_push_rows;
pub mod org_ops;
pub mod org_projection;
pub mod org_settings;
pub mod organization;
pub mod organization_rows;
pub mod organization_spec;
pub mod persist_dispatch;
pub mod person_contracts;
pub mod reconciler_facts;
pub mod rows_txn;
pub mod runtime_owner_rows;
pub mod runtime_ownership;
pub mod runtime_projection;
pub mod runtime_rows;
pub mod session_epoch_ops;
pub mod session_epoch_rows;
pub mod session_maintenance;
pub mod session_maintenance_ops;
pub mod shadow_report;
pub mod staffing_lifecycle;
/// The operator's durable company-level "stop working, and stay stopped".
pub mod stand_down;
pub mod supervision;
pub mod supervision_intake;
pub mod supervisor_watermark;
pub mod supervisor_watermark_rows;
pub mod unit_preview;

// TOMBSTONE — `bare_slug(company_slug)` lived here and is deleted.
//
// It split a composite company label `<slug>@<sha256(orgs_root)[..12]>` on the
// at-sign to recover the display slug. A company key is now pure hex with no
// suffix, so the split matched nothing and the helper returned the KEY at every
// site that wanted the NAME — stamping `organization: "71a6cc3805dc"` into
// documents whose validators compare them with `manifest.slug`.
//
// The name is a stored fact now: `org_settings.display_slug`, read by
// `organization_rows::read_policy` (into `manifest.slug`) and by
// `org_settings::display_slug` for callers holding only a row key. There is no
// derivation of a company's name from its key, because a hash carries none.

pub use context::CompanyContext;

crate::declare_stores! {
    /// The fence over "who is allowed to run" (plan §5.5, D7).
    ///
    /// `FailSafeValue` on **all three** operations, each for its own reason
    /// along the operation axis:
    ///
    /// - *read* — unreadable bytes decode to `Fenced(∅)`: the CEO, nobody else.
    /// - *write* — the union write re-reads through that same fail-safe decode,
    ///   so a corrupt ledger loses its (unreadable) contents rather than
    ///   blocking an operator's explicit launch. The result is still a fence
    ///   naming exactly the people the operator just named.
    /// - *clear* — clearing *is* the restrictive value. Refusing to clear a
    ///   corrupt fence would leave unreadable bytes standing in for authority,
    ///   which is strictly worse than resetting to CEO-only.
    LaunchIntent => launch_intent::LaunchIntentStore {
        read: FailSafeValue,
        write: FailSafeValue,
        clear: FailSafeValue,
    },

    /// The health monitor's incidents, observations and log cursors (M10).
    ///
    /// `FailOpen` throughout: losing health data degrades observability, never
    /// safety, and a monitor that refuses to run because it could not parse its
    /// own five-minute state file is a monitor that stops reporting the outage
    /// it exists to report.
    Health => health::HealthStore {
        read: FailOpen,
        write: FailOpen,
        clear: FailOpen,
    },

    /// The session-maintenance queue (M10).
    ///
    /// Plan §5.5 assigns no polarity to this store — it names twelve and this
    /// is not one of them — so M10 closes the decision, which is exactly what
    /// [`STORE_POLARITY_INVENTORY`]'s forcing function is for. `FailClosed` on
    /// all three operations:
    ///
    /// - *read* — an unreadable ledger read as "empty" answers `maint.start`
    ///   with `null` while a company-wide reset is in flight, and hands a
    ///   claimed request to a second Pi. Both are worse than an error.
    /// - *write* — overwriting a ledger chiefd could not read destroys the
    ///   audit history the recovery path (inv 12) re-reads.
    /// - *clear* — there is no legitimate "discard the maintenance queue".
    ///
    /// Absence is not corruption: a company that has never needed maintenance
    /// has no ledger, and that decodes to the initial ledger.
    SessionMaintenance => session_maintenance::SessionMaintenanceStore {
        read: FailClosed,
        write: FailClosed,
        clear: FailClosed,
    },

    /// The organization manifest rows — the sole structural authority (M12).
    ///
    /// Plan §5.5 assigns no polarity (§5.5b lists it as open); M12 closes it as
    /// `FailClosed` on all three operations. There is no safe default
    /// structure: an unreadable manifest read as "empty" is a company with no
    /// CEO, no departments and no people, and every dependent store would then
    /// reconcile that emptiness into its own ledger and delete real state.
    /// Write and clear are fail-closed for the operator's sake — this is the
    /// file a human edits to unstick a company, and chiefd must never overwrite
    /// or delete bytes it could not read.
    Organization => organization::OrganizationStore {
        read: FailClosed,
        write: FailClosed,
        clear: FailClosed,
    },

    /// Who should be running, and the bounded handoff owed before they stop
    /// (M12).
    ///
    /// Plan §5.5 assigns no polarity (§5.5b); M12 closes it as `FailClosed`
    /// throughout. An unreadable ledger read as "empty" says *nobody owes a
    /// handoff and nobody was running*, which lets a structural change proceed
    /// past a reflection someone actually wrote and lets the projection audit
    /// kill panes it has no record of. Write and clear follow: the ledger holds
    /// the only durable record of an owed handoff, so overwriting or discarding
    /// bytes chiefd could not read discards the D7 fence itself.
    Activity => activity::ActivityStore {
        read: FailClosed,
        write: FailClosed,
        clear: FailClosed,
    },

    /// Assignments, effects, goals and the runtime-generation fence (M12).
    ///
    /// Plan §5.5 assigns no polarity (§5.5b); M12 closes it as `FailClosed`
    /// throughout. The decisive case is *read*: an unreadable ledger read as
    /// empty resets every runtime generation to 1, which invalidates every live
    /// assignment fence in the company and re-admits work from panes that no
    /// longer exist. There is no safe empty value for a fence.
    Supervision => supervision::SupervisionStore {
        read: FailClosed,
        write: FailClosed,
        clear: FailClosed,
    },

    /// The per-duty supervisor liveness watermark (od-supervisor #14).
    ///
    /// A one-daemon addition with no plan §5.5 row and no TS counterpart — it
    /// is the fix for the 41-hour-blackout class, a detector that must not be
    /// hosted solely by what it detects. `FailOpen` throughout for the same
    /// reason as `health`: losing the watermark degrades
    /// observability, never safety, and a self-audit that refused to run
    /// because it could not parse its own state would be the very silent
    /// detector this store exists to replace. Its inventory row is added
    /// alongside this declaration (the partition test forces the pair).
    SupervisorWatermark => supervisor_watermark::SupervisorWatermarkStore {
        read: FailOpen,
        write: FailOpen,
        clear: FailOpen,
    },

    /// The converge/apply safety scaffold (M2, Unit C) — a one-daemon addition
    /// with no plan §5.5 row and no TS counterpart. One durable record per
    /// company gating host actuation: shadow/apply mode + the #29 sweep sub-flag,
    /// the destructive-action budget override, the 3-strike breaker, and the
    /// floor-interval start stamp. `FailSafeValue` throughout, for the reason the
    /// launch-intent fence is: unreadable bytes must resolve to the *deny* value,
    /// which for a safety gate is "shadow, breaker tripped, actuate nothing" —
    /// never "apply with a clear breaker". Its inventory row is added alongside
    /// this declaration (the partition test forces the pair).
    ConvergeSafety => converge_safety::ConvergeSafetyStore {
        read: FailSafeValue,
        write: FailSafeValue,
        clear: FailSafeValue,
    },
}

/// One inventory entry: `(store, read, write, clear)`.
///
/// A tuple rather than three [`PolarityRow`]s so that an entry cannot be
/// written with an operation missing — the same property the registry macro
/// gets from requiring all three keys.
pub type InventoryEntry = (&'static str, Polarity, Polarity, Polarity);

/// Every store named in plan §5.5, with its polarity already decided.
///
/// The point of recording polarity for stores that do not exist yet: the
/// decision is the expensive part and the plan already made it. A milestone
/// that lands `health` must produce exactly these rows, and the matrix test
/// fails if it produces anything else — including if it quietly appears
/// without an inventory row.
///
/// **This is §5.5's list, not the complete store list.** Plan §0 counts "~20
/// durable stores"; §5.5 assigns a polarity to twelve of them. The stores it
/// does not name — session-maintenance, the organization manifest rows, activity, supervision and
/// its relational sub-stores — reach this table when their milestone lands,
/// and `the_inventory_is_partitioned_exactly_into_declared_and_pending_stores`
/// makes adding one here a required step rather than an optional one. That is
/// the intended forcing function: the plan left those polarities open, so the
/// milestone that ports the store has to close them in review.
pub const STORE_POLARITY_INVENTORY: &[InventoryEntry] = &[
    // --- FailOpen: losing this degrades observability, not safety ----------
    ("health-monitor", Polarity::FailOpen, Polarity::FailOpen, Polarity::FailOpen),
    ("startup-admission", Polarity::FailOpen, Polarity::FailOpen, Polarity::FailOpen),
    // --- FailSafeValue: corruption resolves to a deny value ----------------
    ("launch-intent", Polarity::FailSafeValue, Polarity::FailSafeValue, Polarity::FailSafeValue),
    // --- FailClosed: corruption is the caller's problem ---------------------
    ("removal-journal", Polarity::FailClosed, Polarity::FailClosed, Polarity::FailClosed),
    ("registry", Polarity::FailClosed, Polarity::FailClosed, Polarity::FailClosed),
    ("loop-control", Polarity::FailClosed, Polarity::FailClosed, Polarity::FailClosed),
    // --- closed by M10, because plan §5.5 left it open (see the registry) ---
    ("session-maintenance", Polarity::FailClosed, Polarity::FailClosed, Polarity::FailClosed),
    // --- closed by M12, for the three stores §5.5b listed as open ----------
    ("org-manifest", Polarity::FailClosed, Polarity::FailClosed, Polarity::FailClosed),
    ("activity", Polarity::FailClosed, Polarity::FailClosed, Polarity::FailClosed),
    ("supervision", Polarity::FailClosed, Polarity::FailClosed, Polarity::FailClosed),
    // --- one-daemon addition (od-supervisor #14), no §5.5 row: the duty
    //     liveness watermark, FailOpen like health -----------------------------
    ("supervisor-watermark", Polarity::FailOpen, Polarity::FailOpen, Polarity::FailOpen),
    // --- one-daemon addition (M2 Unit C), no §5.5 row: the converge/apply
    //     safety scaffold, FailSafeValue — corruption resolves to
    //     shadow-and-tripped, the deny value for a safety gate ------------------
    ("converge-safety", Polarity::FailSafeValue, Polarity::FailSafeValue, Polarity::FailSafeValue),
    // TOMBSTONE: `runtime-actuation`. It held the actuator's committed
    // observation, and its FailSafeValue polarity existed so unreadable bytes
    // resolved to an UNTRUSTED observation with a dead lease -- never to
    // "attached, and nothing is running", which is a mandate to start a whole
    // company a second time on top of one already up.
    //
    // The store is deleted with the direction it carried, so there are no bytes
    // to resolve. The polarity it needed is not weakened but made
    // unrepresentable: chiefd holds no observation, and `chief-cli`'s own
    // `actuate::trust` keeps the identical line for that crate's reading of the
    // host -- an observation it cannot make is a pass it declines to act on,
    // never a claim it sends anywhere.
];

/// The polarity this inventory entry assigns to `op`.
#[must_use]
pub const fn inventory_polarity(entry: &InventoryEntry, op: StoreOp) -> Polarity {
    match op {
        StoreOp::Read => entry.1,
        StoreOp::Write => entry.2,
        StoreOp::Clear => entry.3,
    }
}

/// Inventory stores whose implementation has not landed yet, with the
/// milestone that owns them (plan §9).
///
/// A name here is a promise, not an excuse: the matrix test asserts
/// `pending ∪ declared == inventory` and that the two sets are disjoint, so a
/// store cannot be both "pending" and quietly implemented with a different
/// polarity.
///
/// Where plan §9 does not name an owning milestone the entry says so rather
/// than guessing — an invented milestone reads like a commitment somebody made.
pub const PENDING_STORES: &[(&str, &str)] = &[
    // `startupAdmissionUntil` (inv 21) is grouped with M10 by plan §9's
    // "readiness/admission" phrasing, but it is not provider admission: it is
    // the launch throttle `org.department.launch` carries forward, and it has
    // no meaning until the launch path exists. It lands with M12 rather than
    // being invented here against a store that cannot yet be exercised.
    ("startup-admission", "M16 (launch path); §5.5b records the M10→M12→M16 correction"),
    ("removal-journal", "M16"),
    ("registry", "M16"),
    ("loop-control", "unassigned in plan §9"),
];

/// File name of a company database inside its org directory.
pub const COMPANY_DB_FILENAME: &str = "chief.db";

/// The native ledgers `createOrganization` seeds into the shared
/// `org_documents` table but never into CompanyDb, and which `chiefd run`
/// therefore adopts at boot (#37/#55).
///
/// #98: this list lives HERE, in the registry module, because naming a store's
/// documents key from outside its own module is exactly the bypass
/// `fence_containment`'s `no_source_outside_a_stores_own_module_can_name_its_documents_key`
/// forbids — a caller that can spell the key can read or write the store
/// without the polarity that store was given. `store/mod.rs` is the one place
/// already permitted to hold the inventory, so the boot-adopt loop takes its
/// names from a typed const here instead of two string literals in `run.rs`.
/// The names come from the `StoreKind` impls themselves, so a store that
/// renames its key cannot leave this list stale.
pub const BOOT_ADOPTABLE_STORES: [&str; 2] =
    [supervision::SupervisionStore::NAME, activity::ActivityStore::NAME];

/// Seed one of [`BOOT_ADOPTABLE_STORES`] into its deterministic initial state
/// (#105). Lives here for the same reason the name list does: dispatching on a
/// store's documents key is the registry's job, not a caller's.
///
/// # Errors
/// Whatever the store's own `seed` refuses — an initial ledger that does not
/// validate means the manifest it was built from is broken. An unknown store
/// name is a no-op `false`.
pub fn seed_native_ledger(
    ledgers: &mut crate::ledger::Ledgers,
    manifest: &organization::OrganizationManifest,
    store: &str,
) -> Result<bool, crate::ChiefdError> {
    if store == supervision::SupervisionStore::NAME {
        supervision::seed(ledgers, manifest)?;
        return Ok(true);
    }
    if store == activity::ActivityStore::NAME {
        activity::seed(ledgers, manifest)?;
        return Ok(true);
    }
    Ok(false)
}

/// Open (creating if absent) a company database and apply its schema.
///
/// The returned connection is intended to be moved onto that company's writer
/// thread and never shared; nothing in this crate hands it out.
///
/// # Errors
/// Propagates any `rusqlite` failure from opening, pragma application, or DDL.
pub fn open_company_db(path: &Path) -> rusqlite::Result<Connection> {
    // Seam exception: chiefd_core::store is one of the two modules permitted
    // to open a connection (plan §5.2 item 4).
    #[allow(clippy::disallowed_methods)]
    let conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    conn.execute_batch(COMPANY_SCHEMA_SQL)?;
    // `CREATE TABLE IF NOT EXISTS` is silent about COLUMNS: on a database whose
    // tables already exist it adds none, so a column added to the schema
    // reaches new companies only and the readers — which select it by name —
    // cannot open the old ones at all. Reconciling here is what makes the
    // schema's declaration true for a database that already exists. See
    // `schema::add_missing_columns` for the measured brick this prevents.
    crate::schema::add_missing_columns(&conn).map_err(|error| match error {
        crate::schema::AdditiveColumnError::Database(error) => error,
        needs_migration => rusqlite::Error::ToSqlConversionFailure(Box::new(needs_migration)),
    })?;
    Ok(conn)
}

/// Open a company database **read-only**, applying no schema and taking no
/// write lock.
///
/// This is not a second writer and cannot become one: `SQLITE_OPEN_READ_ONLY`
/// makes every write fail at the SQLite layer, and no DDL is executed, so the
/// call cannot contend with the owning process's writer thread.
///
/// It exists for observers: the invariant-8 test (TESTING.md §4.3) needs a
/// reader *outside* the writing process polling durable state for the whole
/// duration of a host transaction, to assert that no observable instant ever
/// shows a manifest referencing an unpublished file. Diagnostics and the
/// backup verifier use the same door.
///
/// # Errors
/// Propagates any `rusqlite` failure, including "the file does not exist" —
/// read-only never creates.
pub fn open_company_db_readonly(path: &Path) -> rusqlite::Result<Connection> {
    // Seam exception: chiefd_core::store is one of the two modules permitted
    // to open a connection (plan §5.2 item 4). Read-only, so the seam's actual
    // subject — who may *write* — is untouched.
    #[allow(clippy::disallowed_methods)]
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok(conn)
}

fn apply_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    for pragma in COMPANY_PRAGMAS {
        // `journal_mode` returns a row; `execute_batch` tolerates that while
        // `execute` would reject it.
        conn.execute_batch(pragma)?;
    }
    Ok(())
}

/// Bootstrap-only dispatch: some stores keep hot sub-data in relational
/// tables outside their document body (currently just `supervision`'s
/// assignments/effects, plan §5.1 M12) and need it seeded alongside the
/// document itself — see [`supervision::seed_relational_from_document`].
/// Every other store is a no-op.
///
/// This lives here, not in `chiefd bootstrap-store`'s own source, because
/// `fence_containment.rs` forbids any source outside a store's own module
/// from naming its documents key or store type (`chiefd-core/src/store/mod.rs`
/// is the one caller-facing exemption both containment tests carry, alongside
/// each store's own module) — a bootstrap tool that took a `--store <name>`
/// CLI argument and matched it against a store literal itself would be
/// exactly the bypass those tests exist to catch. Dispatching by name here
/// keeps the CLI itself store-agnostic.
///
/// # Errors
/// Whatever the underlying store's seed function returns.
pub fn seed_relational_extra(
    ledgers: &mut crate::ledger::Ledgers,
    store: &str,
    body: &str,
) -> Result<(usize, usize), crate::ChiefdError> {
    if store == supervision::SupervisionStore::NAME {
        return supervision::seed_relational_from_document(ledgers, body);
    }
    Ok((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OPENING A COMPANY THAT PREDATES A COLUMN STILL WORKS, through the real
    /// open — not through the reconcile called by hand.
    ///
    /// The measured failure was an operator's own box after an upgrade
    /// (a live box, 2026-08-20T23:40Z): `cannot open the company
    /// database ... no such column: operator_wake_at`. Every company on disk
    /// was unopenable and a freshly created one was perfect. This test is the
    /// one that would have caught it, and it is deliberately at the OPEN rather
    /// than at `schema::add_missing_columns`, because the defect that brings
    /// this back is somebody deleting the call.
    #[test]
    fn opening_a_database_that_predates_a_column_adds_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        {
            let conn = open_company_db(&path).expect("create");
            // Age it back one release: the table as it was before the column
            // was declared, with a row already written under the old shape.
            conn.execute_batch(
                "ALTER TABLE person_activity DROP COLUMN operator_wake_at; \
                 INSERT INTO person_activity(slug, person_id, updated_at) \
                 VALUES('acme', 'bo', 'before');",
            )
            .expect("age the database");
        }

        let conn = open_company_db(&path).expect("re-open a company that predates the column");
        let stamp: Option<String> = conn
            .query_row(
                "SELECT operator_wake_at FROM person_activity WHERE person_id = 'bo'",
                [],
                |row| row.get(0),
            )
            .expect("the reader's own column is selectable on the aged row");
        assert_eq!(stamp, None, "an existing row gets the column with no value, which is honest");
    }

    /// #98: routing the boot-adopt loop through `BOOT_ADOPTABLE_STORES` must
    /// leave its BEHAVIOUR unchanged — only its access path changed. This pins
    /// the contents AND the order against the two string literals `run.rs`
    /// used to spell, so "same list" is a test rather than a claim.
    ///
    /// Pinned rather than assumed because the caller is `chiefd run`'s
    /// boot-time adoption on a FRESH company, and #97 proved that exact path
    /// was already deadlocked in a promoted binary. A subtle change there does
    /// not surface until somebody creates a company — the worst possible place
    /// for "I'm sure it's the same".
    ///
    /// Order is asserted deliberately, not incidentally: adoption is sequential
    /// and a failed adopt is logged-and-continued, so a reordering changes which
    /// ledger is present when a later step observes the company.
    ///
    /// Verified to fire: swapping the two entries in the const fails this test
    /// with `left: ["activity", "supervision"]`.
    #[test]
    fn the_adoptable_ledgers_are_exactly_what_the_adopt_loop_used_to_spell() {
        assert_eq!(BOOT_ADOPTABLE_STORES, ["supervision", "activity"]);
    }

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .expect("sqlite_master is queryable");
        let names = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query runs")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows decode");
        names
    }

    #[test]
    fn opening_a_company_db_creates_the_full_schema() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_company_db(&dir.path().join(COMPANY_DB_FILENAME)).expect("open");
        let tables = table_names(&conn);
        for expected in
            ["counters", "effects", "event_once_markers", "host_actions", "identities", "mailbox"]
        {
            assert!(tables.contains(&expected.to_string()), "missing table {expected}");
        }
        for retired in [
            "documents",
            "provider_admission",
            "provider_slots",
            "provider_reservations",
            // Chief holds no provider, model or credential state at all: an
            // agent boots as plain Pi on the operator's own defaults, so a
            // fresh database must never grow a catalog to select from.
            "organization_model_defaults",
            "provider_model_observations",
            "provider_models",
            "provider_model_modalities",
            "provider_model_observation_ids",
            "model_change_preparations",
            "reflections",
            "leases",
            // #1047: deleted features, dropped on open rather than merely
            // un-created -- a `CREATE TABLE IF NOT EXISTS` says nothing about a
            // database that already has the table.
            "manager_goals",
            "delegated_goals",
            "goal_watches",
            "goal_intents",
            "manager_check_ins",
            "assignments",
            "ack_receipts",
            "runtime_generations",
            "memory_records",
            "learned_skills",
        ] {
            assert!(
                !tables.contains(&retired.to_string()),
                "retired table {retired} survived the DROP"
            );
        }
    }

    #[test]
    fn reopening_a_company_adds_identities_without_disturbing_existing_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        {
            let conn = open_company_db(&path).expect("fresh company");
            conn.execute("DROP TABLE identities", []).expect("simulate pre-auth company");
            conn.execute(
                "INSERT INTO counters(name, value) VALUES ('existing-company-row', 41)",
                [],
            )
            .expect("seed existing authoritative row");
        }

        let reopened = open_company_db(&path).expect("open upgraded company");
        let count: i64 = reopened
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='identities'",
                [],
                |row| row.get(0),
            )
            .expect("identity table exists after company open");
        assert_eq!(count, 1);
        let preserved: i64 = reopened
            .query_row("SELECT value FROM counters WHERE name='existing-company-row'", [], |row| {
                row.get(0)
            })
            .expect("existing company data survives the DDL addition");
        assert_eq!(preserved, 41);
    }

    #[test]
    fn company_db_is_durable_wal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_company_db(&dir.path().join(COMPANY_DB_FILENAME)).expect("open");
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal_mode readable");
        assert_eq!(journal.to_lowercase(), "wal");
        let synchronous: i64 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("synchronous readable");
        assert_eq!(synchronous, 2, "synchronous must be FULL");
    }

    #[test]
    fn opening_is_idempotent_and_preserves_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        {
            let conn = open_company_db(&path).expect("first open");
            conn.execute("INSERT INTO counters(name, value) VALUES ('test:org', 1)", [])
                .expect("insert");
        }
        let conn = open_company_db(&path).expect("second open");
        let count: i64 = conn
            .query_row("SELECT count(*) FROM counters WHERE name = 'test:org'", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(count, 1, "re-opening must not recreate or clear tables");
    }

    #[test]
    fn opening_drops_the_retired_provider_pool_tables_and_keeps_everything_else() {
        // #748 migration boundary: a historical company database that still
        // carries provider_slots/provider_reservations rows must cross an
        // explicit idempotent migration that removes ONLY the retired pool
        // state. No provider request depends on this migration completing —
        // nothing reads the retired tables anymore — and unrelated rows must
        // survive it untouched.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        {
            let conn = open_company_db(&path).expect("fresh open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS provider_slots(
                    slug TEXT NOT NULL, token TEXT NOT NULL, provider TEXT NOT NULL,
                    person TEXT NOT NULL, pid INTEGER NOT NULL, process_start TEXT NOT NULL,
                    boot_id TEXT NOT NULL, at_ms INTEGER NOT NULL, state TEXT NOT NULL,
                    PRIMARY KEY (slug, token));
                 CREATE TABLE IF NOT EXISTS provider_reservations(
                    slug TEXT NOT NULL, lane_key TEXT NOT NULL, person TEXT NOT NULL,
                    generation INTEGER NOT NULL, reservation_uuid TEXT NOT NULL, token TEXT NOT NULL,
                    UNIQUE(slug, lane_key, person, generation, reservation_uuid));
                 INSERT INTO provider_slots VALUES
                    ('acme', 't1', 'anthropic', 'chief', 42, 's1', 'b1', 0, 'holder');
                 INSERT INTO provider_reservations VALUES
                    ('acme', 'k1', 'chief', 1, 'u1', 't1');
                 INSERT INTO counters(name, value) VALUES ('next_effect_sequence:acme', 9);",
            )
            .expect("seed historical pool state");
        }
        let conn = open_company_db(&path).expect("migration open");
        let pool_tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('provider_slots','provider_reservations')",
                [],
                |row| row.get(0),
            )
            .expect("count retired pool tables");
        let retained: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM counters WHERE name = 'next_effect_sequence:acme'",
                [],
                |row| row.get(0),
            )
            .expect("count unrelated audit sequence");
        assert_eq!(pool_tables, 0, "the retired pool tables must not survive open");
        assert_eq!(retained, 1, "the migration must not delete unrelated rows");
    }

    #[test]
    fn opening_drops_the_retired_company_removal_journal_tables_and_keeps_everything_else() {
        // #946/#820 migration boundary: a historical company database that
        // still carries a retained two-phase removal journal must cross an
        // explicit idempotent migration that drops the retired journal
        // tables. Simulate that history the same way the provider-pool
        // precedent above does: manually recreate the retired schema
        // (bypassing the normal open, which already drops it) and seed a
        // row, then reopen and confirm the migration removes it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        {
            let conn = open_company_db(&path).expect("fresh open");
            conn.execute_batch(
                "CREATE TABLE company_removal(
                     slug TEXT PRIMARY KEY, transaction_id TEXT NOT NULL,
                     source_created_at TEXT NOT NULL, source_runtime_session TEXT NOT NULL,
                     phase TEXT NOT NULL, runtime_state_path TEXT NOT NULL,
                     already_stopped INTEGER NOT NULL, created_at TEXT NOT NULL,
                     updated_at TEXT NOT NULL);
                 INSERT INTO company_removal VALUES(
                     'acme','txn','2026-08-01T00:00:00.000Z','org-acme','quarantined',
                     '/runtime',1,'2026-08-01T00:00:00.000Z','2026-08-01T00:00:01.000Z'
                 );",
            )
            .expect("seed a historical retained journal row");
        }
        let conn = open_company_db(&path).expect("reopen after the journal tables retire");
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN (
                     'company_removal', 'company_removal_stopped_windows',
                     'company_removal_stopped_panes', 'company_removal_completion_receipts'
                 )",
                [],
                |row| row.get(0),
            )
            .expect("query sqlite_master");
        assert_eq!(
            remaining, 0,
            "the retired company-removal journal tables must not survive open"
        );
    }

    #[test]
    fn opening_drops_the_never_implemented_unit_removal_journal_tables() {
        // The per-unit removal journal is retired for a blunter reason than
        // its company-level sibling above: it never ran. `unit_removals` and
        // `unit_removal_members` shipped as DDL and never acquired a store
        // module, a route or a writer, so no phase its CHECK admitted was
        // ever produced. A historical database still carries the empty
        // tables, so open must cross the same explicit idempotent migration
        // boundary. Recreate the retired schema (bypassing the normal open,
        // which already drops it) and confirm reopening removes both, child
        // first so the foreign key cannot block the drop.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        {
            let conn = open_company_db(&path).expect("fresh open");
            conn.execute_batch(
                "CREATE TABLE unit_removals(
                     slug TEXT NOT NULL, unit_id TEXT NOT NULL, txn_id TEXT NOT NULL,
                     kind TEXT NOT NULL, phase TEXT NOT NULL,
                     subtree_fingerprint TEXT NOT NULL, created_at TEXT NOT NULL,
                     updated_at TEXT NOT NULL, PRIMARY KEY (slug, unit_id));
                 CREATE TABLE unit_removal_members(
                     slug TEXT NOT NULL, unit_id TEXT NOT NULL, entity TEXT NOT NULL,
                     entity_id TEXT NOT NULL, created_at_snapshot TEXT NOT NULL,
                     PRIMARY KEY (slug, unit_id, entity, entity_id),
                     FOREIGN KEY (slug, unit_id) REFERENCES unit_removals(slug, unit_id));
                 INSERT INTO unit_removals VALUES(
                     'acme','dept-1','txn','department','planned','fp',
                     '2026-08-01T00:00:00.000Z','2026-08-01T00:00:01.000Z');
                 INSERT INTO unit_removal_members VALUES(
                     'acme','dept-1','person','p-1','2026-08-01T00:00:00.000Z');",
            )
            .expect("seed a historical unit-removal journal");
        }
        let conn = open_company_db(&path).expect("reopen after the journal tables retire");
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN (
                     'unit_removals', 'unit_removal_members'
                 )",
                [],
                |row| row.get(0),
            )
            .expect("query sqlite_master");
        assert_eq!(remaining, 0, "the retired unit-removal journal tables must not survive open");
    }

    /// Pins the busy timeout every company connection actually carries, because
    /// a plausible-looking defect was reported off the opposite assumption.
    ///
    /// `open_company_db_readonly` applies no pragmas, and the docstore's reader
    /// and writer both set `busy_timeout` explicitly — which reads like an
    /// asymmetry where the read-only door alone would inherit SQLite's default
    /// of ZERO and fail instantly on any lock. It does not. **`rusqlite` calls
    /// `sqlite3_busy_timeout(db, 5000)` for every connection it opens**
    /// (`inner_connection.rs`), so the door already waits; the explicit docstore
    /// calls are re-stating the default, and `actor::writer` raising it to its
    /// queue deadline is the only place the value genuinely changes.
    ///
    /// This test exists so that stays true: a `rusqlite` upgrade that dropped
    /// the default would silently turn every read-only company read into an
    /// instant BUSY, which is exactly the failure that was mistakenly reported.
    #[test]
    fn every_company_connection_waits_on_a_lock_rather_than_failing_instantly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        drop(open_company_db(&path).expect("create"));
        for (label, conn) in [
            ("read-only", open_company_db_readonly(&path).expect("open read-only")),
            ("read-write", open_company_db(&path).expect("open read-write")),
        ] {
            let timeout: i64 = conn
                .pragma_query_value(None, "busy_timeout", |row| row.get(0))
                .expect("busy_timeout readable");
            assert!(
                timeout > 0,
                "{label}: a zero busy_timeout fails instantly on contention; rusqlite's \
                 5000ms default must not have gone away"
            );
        }
    }

    #[test]
    fn a_readonly_connection_can_read_but_cannot_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        {
            let conn = open_company_db(&path).expect("open");
            conn.execute("INSERT INTO counters(name, value) VALUES ('test:readonly', 1)", [])
                .expect("seed");
        }
        let reader = open_company_db_readonly(&path).expect("open read-only");
        let value: i64 = reader
            .query_row("SELECT value FROM counters WHERE name='test:readonly'", [], |row| {
                row.get(0)
            })
            .expect("read");
        assert_eq!(value, 1);
        let write = reader.execute("DELETE FROM counters", []);
        assert!(write.is_err(), "an observer must not be able to become a writer");
    }

    #[test]
    fn a_readonly_open_never_creates_the_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("absent.db");
        assert!(open_company_db_readonly(&path).is_err());
        assert!(!path.exists(), "an observer must not conjure a company out of a typo");
    }

    /// E7-S3: `launcher_root` is declared in [`crate::schema::COMPANY_SCHEMA_SQL`]
    /// like every other `org_settings` column, so a brand-new database has it
    /// from its first open with no ALTER involved.
    #[test]
    fn a_fresh_database_has_the_launcher_root_column_from_first_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        let conn = open_company_db(&path).expect("fresh open");
        let columns = conn
            .prepare("PRAGMA table_info(org_settings)")
            .expect("table info")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query columns")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("decode columns");
        assert!(columns.iter().any(|column| column == "launcher_root"));
    }
}
