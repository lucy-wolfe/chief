// `clippy.toml`'s `allow-expect-in-tests` only reaches functions that carry
// `#[test]`, and an integration test is its own crate. The helpers below are
// test scaffolding by construction — a failed `expect` here is the test
// failing, which is the intended outcome.
#![allow(clippy::expect_used, clippy::panic)]

//! The (store × operation) polarity matrix — TESTING.md §3.2, plan §5.5.
//!
//! This is the test that catches a future store being added without a polarity
//! decision. It works in three layers, and it is worth being explicit about
//! which layer catches which mistake, because only one of them is a runtime
//! assertion:
//!
//! 1. **Compile time, in the crate.** [`StoreKind`] is sealed and only
//!    `declare_stores!` implements the seal, so a store that is not in the
//!    registry cannot exist. The macro requires a polarity for all three
//!    operations, and emits an assertion that the declared polarity's marker
//!    trait is actually implemented.
//! 2. **Compile time, here.** [`observe`] matches on `(StoreId, StoreOp)` with
//!    no wildcard arm. Adding a store makes *this file* fail to compile until
//!    somebody writes down what that store does with corrupt bytes on each
//!    operation. A table-only test would have let them tick a box instead.
//! 3. **Runtime, here.** Every cell is driven with real corrupt bytes and the
//!    observed behaviour is compared with the declared polarity, and the
//!    declared set is reconciled against the plan's full inventory.

use chiefd_core::clock::WallMillis;
use chiefd_core::ledger::Ledgers;
use chiefd_core::polarity::{FailSafeValue, StoreKind};
use chiefd_core::polarity::{Polarity, StoreOp};
use chiefd_core::store::activity::{self, ActivityStore, BeginTransitionInput, TransitionAction};
use chiefd_core::store::converge_safety::{self, ActuationMode, ConvergeSafetyStore};
use chiefd_core::store::health::{self, HealthMonitorState, HealthStore};
use chiefd_core::store::launch_intent::{self, LaunchIntent, LaunchIntentStore};
use chiefd_core::store::organization::{self, OrganizationStore};
use chiefd_core::store::session_maintenance::{self, SessionMaintenanceLedger};
use chiefd_core::store::supervision::{self, ArmRequest, SupervisionStore};
use chiefd_core::store::supervisor_watermark::{
    self, SupervisorWatermarkState, SupervisorWatermarkStore,
};
use chiefd_core::store::{
    inventory_polarity, CompanyContext, StoreId, PENDING_STORES, POLARITY_MATRIX,
    STORE_POLARITY_INVENTORY,
};
use chiefd_core::test_support::northstar_manifest;

fn ctx() -> CompanyContext {
    CompanyContext::new("cobalt", "ceo", ["ceo", "quant-head"].map(String::from))
}

/// A ledger whose row for `store` is present, well-formed JSON, and not a
/// valid body — the realistic corruption (a schema drift or a half-migrated
/// row), not a shredded file.
fn corrupt(store: &str) -> Ledgers {
    corrupt_with(store, r#"{"version":"not-a-number"}"#)
}

/// The same idea where a store's schema means `{"version": …}` would parse
/// cleanly. The corruption still has to be *realistic* — a field of the wrong
/// type, i.e. schema drift — because a shredded file is the easy case and not
/// the one that shipped bugs.
fn corrupt_with(store: &str, body: &str) -> Ledgers {
    let mut ledgers = Ledgers::empty(WallMillis(1_752_000_000_000));
    ledgers.put_document(store, body);
    ledgers
}

/// What a caller actually got.
#[derive(Debug, PartialEq, Eq)]
enum Observed {
    /// The store read as empty and warned.
    RecoveredEmpty,
    /// The store resolved to its restrictive value and warned.
    RecoveredRestrictive,
    /// The caller got `Corrupt{store}` and durable state was untouched.
    Corrupt,
}

impl Observed {
    /// The behaviour each polarity promises. This mapping is the definition of
    /// the three markers; if it ever needs a special case, the polarity model
    /// has grown a fourth member and the plan needs revising, not this table.
    fn required_by(polarity: Polarity) -> Self {
        match polarity {
            Polarity::FailOpen => Self::RecoveredEmpty,
            Polarity::FailSafeValue => Self::RecoveredRestrictive,
            Polarity::FailClosed => Self::Corrupt,
        }
    }
}

/// In-memory row store for the session-maintenance cells (the blob corruption
/// drive is meaningless for a store whose authority is normalized rows).
fn maintenance_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
    conn.execute_batch("PRAGMA foreign_keys=ON;").expect("pragma");
    conn.execute_batch(chiefd_core::schema::COMPANY_SCHEMA_SQL).expect("schema");
    conn
}

/// One valid committed ledger — carrying a single request row — that the
/// corrupt drive can then damage.
fn seed_maintenance_ledger(conn: &mut rusqlite::Connection) {
    let ledger: SessionMaintenanceLedger = serde_json::from_value(serde_json::json!({
        "schemaVersion": 1,
        "organization": "acme",
        "requestOrder": ["session-maintenance:1:ada:compact"],
        "requests": {
            "session-maintenance:1:ada:compact": {
                "id": "session-maintenance:1:ada:compact",
                "action": "compact",
                "personId": "ada",
                "requestedBy": "ada",
                "reason": "context is nearly full",
                "automatic": false,
                "status": "queued",
                "requestedAt": "2026-07-25T00:00:00.000Z",
                "attempt": 1
            }
        },
        "createdAt": "2026-07-25T00:00:00.000Z",
        "updatedAt": "2026-07-25T00:00:00.000Z"
    }))
    .expect("fixture ledger");
    let tx = conn.transaction().expect("transaction");
    session_maintenance::rows::publish(&tx, "acme", &ledger).expect("seed a valid ledger");
    tx.commit().expect("commit");
}

fn maintenance_row_count(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM maintenance_requests", [], |row| row.get(0))
        .expect("count")
}

/// THE READ SITE'S CHOICE, driven through the real `read` — not a hand-built
/// `Decoded` arm.
///
/// This distinction is the whole test. A unit test that constructs
/// `Decoded::Absent { body: deny_all }` itself proves only that the ARM carries
/// the body it was handed; it cannot see the read site choosing a different
/// body, which is exactly where "absence is not corruption" would turn into
/// "absence is permissive". So this drives `launch_intent::read` against a
/// ledger with NO ROW and compares the value to the one the corrupt path
/// produces, which the matrix above already pins.
///
/// VALUE and SENTENCE, separately: one value, two sentences.
#[test]
fn an_unwritten_launch_fence_denies_everyone_and_is_not_called_corruption() {
    let empty = Ledgers::empty(WallMillis(1_752_000_000_000));
    let (absent_value, absent_note) = launch_intent::read(&empty, &ctx()).into_parts();
    let (corrupt_value, corrupt_warning) =
        launch_intent::read(&corrupt(LaunchIntentStore::NAME), &ctx()).into_parts();

    // VALUE — the security property. An absent fence authorizes nobody, and it
    // authorizes exactly as few people as an unreadable one.
    assert_eq!(
        absent_value,
        LaunchIntent::deny_all(),
        "a fence nobody has written must authorize NOBODY"
    );
    assert_eq!(absent_value, corrupt_value, "absence and corruption produce ONE value");

    // SENTENCE — the thing that was wrong, and all that was allowed to change.
    let absent_note = absent_note.expect("this store's absence is worth words: it refuses");
    assert!(
        !absent_note.contains("unreadable"),
        "a document nobody wrote must not be reported as damaged bytes: {absent_note}"
    );
    assert!(
        corrupt_warning.expect("a recovery warns").contains("unreadable"),
        "and real corruption must still say so"
    );
}

/// Drive one cell of the matrix with corrupt bytes.
///
/// No wildcard arm, deliberately: this match is the forcing function that
/// makes a new store's polarity a decision somebody has to write down.
fn observe(store: StoreId, op: StoreOp) -> Observed {
    match (store, op) {
        // --- launch intent: restrictive in all three directions -------------
        (StoreId::LaunchIntent, StoreOp::Read) => {
            let (intent, warning) =
                launch_intent::read(&corrupt(LaunchIntentStore::NAME), &ctx()).into_parts();
            assert_eq!(intent, LaunchIntent::deny_all(), "an unreadable fence denies everyone");
            assert!(warning.is_some(), "a fence conjured from corruption must warn");
            Observed::RecoveredRestrictive
        }
        (StoreId::LaunchIntent, StoreOp::Write) => {
            let mut ledgers = corrupt(LaunchIntentStore::NAME);
            // The caller supplies its own authoritative read now — see `add`'s
            // doc, and the wake it silently withdrew by unioning onto a stale
            // in-memory document. The polarity claim is unchanged and is now
            // stated where it belongs: a caller reading an unreadable fence gets
            // the fail-safe decode (deny-all), and unioning onto THAT still lets
            // the operator's launch through while contributing nobody.
            let (current, _) = launch_intent::read(&ledgers, &ctx()).into_parts();
            let written = launch_intent::add(&mut ledgers, &ctx(), &current, [])
                .expect("an operator launch is not blocked by an unreadable fence");
            assert_eq!(
                written,
                LaunchIntent::deny_all(),
                "corrupt bytes contribute nobody to the merged fence"
            );
            Observed::RecoveredRestrictive
        }
        (StoreId::LaunchIntent, StoreOp::Clear) => {
            let mut ledgers = corrupt(LaunchIntentStore::NAME);
            assert!(launch_intent::clear(&mut ledgers), "clearing never refuses");
            assert_eq!(
                launch_intent::read(&ledgers, &ctx()).into_parts().0,
                LaunchIntent::deny_all(),
                "clearing converges on the restrictive value"
            );
            Observed::RecoveredRestrictive
        }

        // --- health: fail-open in all three directions ----------------------
        (StoreId::Health, StoreOp::Read) => {
            let (state, warning) = health::read(&corrupt(HealthStore::NAME), &ctx()).into_parts();
            assert_eq!(state, HealthMonitorState::empty("cobalt"));
            assert!(warning.is_some(), "a reset monitor state is worth a warning");
            Observed::RecoveredEmpty
        }
        (StoreId::Health, StoreOp::Write) => {
            let mut ledgers = corrupt(HealthStore::NAME);
            let mut state = HealthMonitorState::empty("cobalt");
            health::apply_cycle(
                &mut state,
                &[health::IncidentCandidate::new("supervisor_error", "boom")],
                1_752_000_000_000,
                &health::NeverResolves,
            );
            health::write(&mut ledgers, &state);
            let (reread, warning) = health::read(&ledgers, &ctx()).into_parts();
            assert_eq!(warning, None, "the unreadable bytes are gone, not inherited");
            assert_eq!(reread.incidents.len(), 1);
            Observed::RecoveredEmpty
        }
        (StoreId::Health, StoreOp::Clear) => {
            let mut ledgers = corrupt(HealthStore::NAME);
            assert!(health::clear(&mut ledgers), "clearing an unreadable state never refuses");
            assert_eq!(
                health::read(&ledgers, &ctx()).into_parts().0,
                HealthMonitorState::empty("cobalt")
            );
            Observed::RecoveredEmpty
        }

        // --- session maintenance: fail-closed on the live ROW path --------
        // The Ledgers-blob verbs this store's cells used to drive were
        // deleted as a zero-production-caller shadow twin (arch-audit F11,
        // corrected row), so the corruption drive moves to the row store the
        // routes and the reconciler actually use.
        (StoreId::SessionMaintenance, StoreOp::Read) => {
            // An undecodable row (an action label the ledger cannot name) is
            // an error, never an empty "no work" answer.
            let mut conn = maintenance_db();
            seed_maintenance_ledger(&mut conn);
            conn.execute(
                "UPDATE maintenance_requests SET action = 'defragment' WHERE slug = 'acme'",
                [],
            )
            .expect("corrupt the action label");
            let tx = conn.transaction().expect("transaction");
            session_maintenance::rows::reconstruct(&tx, "acme")
                .expect_err("an undecodable row must never read as 'no work'");
            Observed::Corrupt
        }
        (StoreId::SessionMaintenance, StoreOp::Write) => {
            // A ledger that fails its structural validation is refused and
            // the committed rows are untouched.
            let mut conn = maintenance_db();
            seed_maintenance_ledger(&mut conn);
            let before = maintenance_row_count(&conn);
            let mut ledger = SessionMaintenanceLedger::initial("acme", "2026-07-25T00:00:01.000Z");
            ledger.request_order.push("session-maintenance:1:ada:compact".to_string()); // no matching map entry
            let tx = conn.transaction().expect("transaction");
            session_maintenance::rows::publish(&tx, "acme", &ledger)
                .expect_err("an invalid ledger must never overwrite the committed rows");
            drop(tx);
            assert_eq!(maintenance_row_count(&conn), before, "the refusal left the rows untouched");
            Observed::Corrupt
        }
        (StoreId::SessionMaintenance, StoreOp::Clear) => {
            // Clearing exists only as the explicit typed route; what it can
            // never do is make a garbage ledger publishable afterwards.
            let mut conn = maintenance_db();
            seed_maintenance_ledger(&mut conn);
            {
                let tx = conn.transaction().expect("transaction");
                session_maintenance::rows::clear(&tx, "acme", "2026-07-25T00:00:01.000Z")
                    .expect("the typed clear route");
                tx.commit().expect("commit");
            }
            let mut ledger = SessionMaintenanceLedger::initial("acme", "2026-07-25T00:00:02.000Z");
            ledger.request_order.push("session-maintenance:1:ada:compact".to_string()); // no matching map entry
            let tx = conn.transaction().expect("transaction");
            session_maintenance::rows::publish(&tx, "acme", &ledger)
                .expect_err("a cleared store still refuses an invalid ledger");
            Observed::Corrupt
        }

        // --- organization manifest rows: the sole structural authority, so no empty is safe ---
        (StoreId::Organization, StoreOp::Read) => {
            let err = organization::read(&corrupt(OrganizationStore::NAME))
                .expect_err("a manifest read as 'empty' is a company with no people");
            assert_eq!(err.kind(), "Corrupt");
            Observed::Corrupt
        }
        (StoreId::Organization, StoreOp::Write) => {
            let mut ledgers = corrupt(OrganizationStore::NAME);
            let before = ledgers.document_body(OrganizationStore::NAME).map(str::to_string);
            let err = organization::mutate(&mut ledgers, |draft| {
                draft.purpose = "clobbered".to_string();
                Ok(())
            })
            .expect_err("chiefd must not overwrite the file an operator repairs with");
            assert_eq!(err.kind(), "Corrupt");
            assert_eq!(ledgers.document_body(OrganizationStore::NAME).map(str::to_string), before);
            Observed::Corrupt
        }
        (StoreId::Organization, StoreOp::Clear) => {
            let mut ledgers = corrupt(OrganizationStore::NAME);
            let before = ledgers.document_body(OrganizationStore::NAME).map(str::to_string);
            let err = organization::clear(&mut ledgers)
                .expect_err("deleting an unreadable manifest is deleting the company blind");
            assert_eq!(err.kind(), "Corrupt");
            assert_eq!(ledgers.document_body(OrganizationStore::NAME).map(str::to_string), before);
            Observed::Corrupt
        }

        // --- activity: 'empty' would mean nobody owes a handoff -------------
        (StoreId::Activity, StoreOp::Read) => {
            let (ledgers, manifest) = seeded_company_with_corrupt(ActivityStore::NAME);
            let err = activity::read(&ledgers, &manifest)
                .expect_err("an unreadable ledger must never read as 'no handoff owed'");
            assert_eq!(err.kind(), "Corrupt");
            Observed::Corrupt
        }
        (StoreId::Activity, StoreOp::Write) => {
            let (mut ledgers, manifest) = seeded_company_with_corrupt(ActivityStore::NAME);
            let before = ledgers.document_body(ActivityStore::NAME).map(str::to_string);
            let supervision = supervision::read(&ledgers, &manifest).expect("supervision reads");
            let err = activity::begin_transition(
                &mut ledgers,
                &manifest,
                &supervision,
                &BeginTransitionInput {
                    person_id: "signal-researcher".to_string(),
                    action: TransitionAction::Park,
                    reason: "Polarity probe.".to_string(),
                    to_department_id: None,
                    intent_id: None,
                },
            )
            .expect_err("overwriting destroys handoffs that are the sole authority for D7");
            assert_eq!(err.kind(), "Corrupt");
            assert_eq!(ledgers.document_body(ActivityStore::NAME).map(str::to_string), before);
            Observed::Corrupt
        }
        (StoreId::Activity, StoreOp::Clear) => {
            let (mut ledgers, manifest) = seeded_company_with_corrupt(ActivityStore::NAME);
            let before = ledgers.document_body(ActivityStore::NAME).map(str::to_string);
            let err = activity::clear(&mut ledgers, &manifest)
                .expect_err("discarding the ledger is discarding the fence");
            assert_eq!(err.kind(), "Corrupt");
            assert_eq!(ledgers.document_body(ActivityStore::NAME).map(str::to_string), before);
            Observed::Corrupt
        }

        // --- supervision: 'empty' would discard every armed reminder --------
        (StoreId::Supervision, StoreOp::Read) => {
            let (ledgers, manifest) = seeded_company_with_corrupt(SupervisionStore::NAME);
            let err = supervision::read(&ledgers, &manifest)
                .expect_err("an unreadable ledger must never read as 'nobody owns anything'");
            assert_eq!(err.kind(), "Corrupt");
            Observed::Corrupt
        }
        (StoreId::Supervision, StoreOp::Write) => {
            let (mut ledgers, manifest) = seeded_company_with_corrupt(SupervisionStore::NAME);
            let before = ledgers.document_body(SupervisionStore::NAME).map(str::to_string);
            let err = supervision::arm_reminder(
                &mut ledgers,
                &manifest,
                &ArmRequest {
                    person_id: "signal-researcher".to_string(),
                    created_by_person_id: "signal-researcher".to_string(),
                    prompt: "Polarity probe.".to_string(),
                    interval_ms: 3_600_000,
                    recurring: true,
                    expires_at: None,
                },
            )
            .expect_err("writing over an unreadable ledger loses owned work");
            assert_eq!(err.kind(), "Corrupt");
            assert_eq!(ledgers.document_body(SupervisionStore::NAME).map(str::to_string), before);
            Observed::Corrupt
        }
        (StoreId::Supervision, StoreOp::Clear) => {
            let (mut ledgers, manifest) = seeded_company_with_corrupt(SupervisionStore::NAME);
            let before = ledgers.document_body(SupervisionStore::NAME).map(str::to_string);
            let err = supervision::clear(&mut ledgers, &manifest)
                .expect_err("there is no legitimate 'throw the supervision ledger away'");
            assert_eq!(err.kind(), "Corrupt");
            assert_eq!(ledgers.document_body(SupervisionStore::NAME).map(str::to_string), before);
            Observed::Corrupt
        }

        // --- supervisor watermark: a liveness watermark, fail-open ------------
        (StoreId::SupervisorWatermark, StoreOp::Read) => {
            let ledgers =
                corrupt_with(SupervisorWatermarkStore::NAME, r#"{"schemaVersion":"nope"}"#);
            let (state, warning) = supervisor_watermark::read(&ledgers, &ctx()).into_parts();
            assert_eq!(state, SupervisorWatermarkState::empty("cobalt"));
            assert!(warning.is_some(), "an unreadable watermark resets to empty and warns");
            Observed::RecoveredEmpty
        }
        (StoreId::SupervisorWatermark, StoreOp::Write) => {
            let mut ledgers =
                corrupt_with(SupervisorWatermarkStore::NAME, r#"{"schemaVersion":"nope"}"#);
            supervisor_watermark::record_success(
                &mut ledgers,
                &ctx(),
                supervisor_watermark::Duty::MailboxWake,
                1_752_000_000_000,
            );
            let (reread, warning) = supervisor_watermark::read(&ledgers, &ctx()).into_parts();
            assert_eq!(warning, None, "the unreadable bytes are gone, not inherited");
            assert_eq!(reread.duties.len(), 1, "the recorded duty survives the reset");
            Observed::RecoveredEmpty
        }
        (StoreId::SupervisorWatermark, StoreOp::Clear) => {
            let mut ledgers =
                corrupt_with(SupervisorWatermarkStore::NAME, r#"{"schemaVersion":"nope"}"#);
            assert!(supervisor_watermark::clear(&mut ledgers), "clearing never refuses");
            assert_eq!(
                supervisor_watermark::read(&ledgers, &ctx()).into_parts().0,
                SupervisorWatermarkState::empty("cobalt")
            );
            Observed::RecoveredEmpty
        }

        // --- converge safety: a corrupt gate must resolve to 'actuate nothing' -
        (StoreId::ConvergeSafety, StoreOp::Read) => {
            let ledgers = corrupt_with(ConvergeSafetyStore::NAME, r#"{"schemaVersion":"nope"}"#);
            let (state, warning) = converge_safety::read(&ledgers).into_parts();
            assert_eq!(state, ConvergeSafetyStore::restrictive());
            assert!(
                warning.is_some(),
                "an unreadable safety row resolves to the restrictive value"
            );
            assert_eq!(state.effective_config().actuation_mode, ActuationMode::Shadow);
            Observed::RecoveredRestrictive
        }
        (StoreId::ConvergeSafety, StoreOp::Write) => {
            // A cycle outcome recorded over corrupt bytes reads the restrictive
            // (tripped) value first, so the write cannot resurrect a clean
            // breaker into apply — corrupt bytes contribute the deny value.
            let mut ledgers =
                corrupt_with(ConvergeSafetyStore::NAME, r#"{"schemaVersion":"nope"}"#);
            converge_safety::record_cycle_outcome(&mut ledgers, false);
            let state = converge_safety::read(&ledgers).into_parts().0;
            assert!(
                state.breaker_tripped,
                "corrupt bytes contribute a tripped breaker, never a clean one"
            );
            assert_eq!(state.effective_config().actuation_mode, ActuationMode::Shadow);
            Observed::RecoveredRestrictive
        }
        (StoreId::ConvergeSafety, StoreOp::Clear) => {
            let mut ledgers =
                corrupt_with(ConvergeSafetyStore::NAME, r#"{"schemaVersion":"nope"}"#);
            assert!(
                converge_safety::clear(&mut ledgers),
                "clearing an unreadable safety row never refuses"
            );
            // Post-clear the row is absent, which is the shadow default: still
            // the safe direction (actuates nothing), which FailSafeValue promises.
            assert_eq!(
                converge_safety::read(&ledgers).into_parts().0.effective_config().actuation_mode,
                ActuationMode::Shadow,
                "clearing converges on shadow, never on apply"
            );
            Observed::RecoveredRestrictive
        } // TOMBSTONE: the three `StoreId::RuntimeActuation` arms.
          //
          // They asserted that corrupt actuation bytes recovered to a RESTRICTIVE
          // record rather than to "attached, and nothing is running" -- which
          // would have been a mandate to start a whole company a second time on
          // top of one already up. The store is deleted with the observation it
          // held, so there are no bytes to corrupt and no lease to recover.
          //
          // The property is not lost, it is relocated and made unrepresentable:
          // `chief-cli`'s `actuate::trust` holds the same line for that crate's
          // own reading of tmux, and an observation it cannot make is a pass it
          // declines to act on rather than a claim it sends anywhere.
    }
}

/// A real company with one store's row replaced by unreadable bytes.
///
/// The M12 stores validate against the manifest, so a bare `corrupt()` ledger
/// would fail for the wrong reason ("no company") and the cell would pass
/// vacuously. This builds the whole company first, then breaks exactly one row.
fn seeded_company_with_corrupt(
    store: &str,
) -> (Ledgers, chiefd_core::store::organization::OrganizationManifest) {
    const EPOCH: i64 = 1_784_116_800_000;
    let mut ledgers = Ledgers::empty(WallMillis(EPOCH));
    let manifest = northstar_manifest(EPOCH);
    organization::create(&mut ledgers, &manifest).expect("manifest");
    supervision::seed(&mut ledgers, &manifest).expect("supervision seeds");
    activity::seed(&mut ledgers, &manifest).expect("activity seeds");
    ledgers.put_document(store, r#"{"schemaVersion":"not-a-number"}"#);
    (ledgers, manifest)
}

#[test]
fn every_store_crossed_with_every_operation_behaves_as_its_polarity_declares() {
    let mut covered = 0_usize;
    for &store in StoreId::ALL {
        for &op in StoreOp::ALL {
            let declared = store.polarity(op);
            assert_eq!(
                observe(store, op),
                Observed::required_by(declared),
                "({}, {}) is declared {} but does not behave like it",
                store.name(),
                op.as_str(),
                declared.as_str()
            );
            covered += 1;
        }
    }
    assert_eq!(
        covered,
        StoreId::ALL.len() * StoreOp::ALL.len(),
        "the matrix must be the full cross product, not a sample"
    );
}

#[test]
fn the_generated_matrix_is_the_full_cross_product_in_declaration_order() {
    let expected: Vec<(&str, StoreOp, Polarity)> = StoreId::ALL
        .iter()
        .flat_map(|&store| {
            StoreOp::ALL.iter().map(move |&op| (store.name(), op, store.polarity(op)))
        })
        .collect();
    let actual: Vec<(&str, StoreOp, Polarity)> =
        POLARITY_MATRIX.iter().map(|row| (row.store, row.op, row.polarity)).collect();
    assert_eq!(actual, expected);
}

// The store that once proved polarity needs a per-operation axis —
// fleet-suppression, `FailSafeValue` on read and `FailClosed` on write/clear —
// was deleted with the CEO-only-is-a-boot decision. The `declare_stores!` macro
// still takes a polarity per operation, so the axis remains expressible; no
// surviving store exercises a split, so there is no per-store assertion to make
// here. `every_declared_store_matches_the_plan_inventory_on_every_operation`
// below still checks every (store, op) cell against the inventory.

#[test]
fn every_declared_store_matches_the_plan_inventory_on_every_operation() {
    for &store in StoreId::ALL {
        let entry = STORE_POLARITY_INVENTORY
            .iter()
            .find(|entry| entry.0 == store.name())
            .unwrap_or_else(|| panic!("store '{}' has no plan §5.5 inventory row", store.name()));
        for &op in StoreOp::ALL {
            assert_eq!(
                store.polarity(op),
                inventory_polarity(entry, op),
                "({}, {}) drifted from the plan inventory",
                store.name(),
                op.as_str()
            );
        }
    }
}

#[test]
fn the_inventory_is_partitioned_exactly_into_declared_and_pending_stores() {
    let declared: Vec<&str> = StoreId::ALL.iter().map(|store| store.name()).collect();
    let pending: Vec<&str> = PENDING_STORES.iter().map(|(name, _)| *name).collect();

    for name in &declared {
        assert!(
            !pending.contains(name),
            "'{name}' is implemented and still listed as pending — one of the two lists is lying"
        );
    }
    for entry in STORE_POLARITY_INVENTORY {
        assert!(
            declared.contains(&entry.0) || pending.contains(&entry.0),
            "inventory store '{}' is neither implemented nor listed in PENDING_STORES",
            entry.0
        );
    }
    for name in declared.iter().chain(pending.iter()) {
        assert!(
            STORE_POLARITY_INVENTORY.iter().any(|entry| entry.0 == *name),
            "'{name}' exists but plan §5.5 never assigned it a polarity"
        );
    }
    assert_eq!(
        declared.len() + pending.len(),
        STORE_POLARITY_INVENTORY.len(),
        "a duplicate name would make the two lists overlap without either check firing"
    );
}

#[test]
fn store_names_are_unique_because_they_are_the_documents_primary_key() {
    let mut names: Vec<&str> = StoreId::ALL.iter().map(|store| store.name()).collect();
    let total = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), total, "two stores sharing a name would share a documents row");
}
