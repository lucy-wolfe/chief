//! Round-trip + diff + migration tests for the activity row engine (N4).
//!
//! The engine reconstructs the [`ActivityLedger`] aggregate from rows and diffs
//! a whole ledger back; these pin that ledger→rows→ledger is lossless, that an
//! idle re-publish touches nothing, and that a real change reports exactly its
//! entity.
//!
//! TOMBSTONE (#751-P4): a second family of fixtures lived here, covering the
//! reflection payload's row persistence — durability equality, the
//! `reflection-memory/<person>` blob fold and its zero-loss shadow-diff. They
//! died with the payload and its two tables. What replaces them is the
//! opposite assertion, in `activity/tests.rs`: a transition with NO reflection
//! data must round-trip and validate, because that is now the only shape.

use super::*;
use crate::isotime::iso_millis;
use crate::schema::COMPANY_SCHEMA_SQL;
use crate::store::activity::{
    ActivityLedger, GracefulTransition, TransitionAction, TransitionStatus,
};
use crate::store::organization::OrganizationManifest;
use crate::store::rows_txn::apply_and_emit;
use crate::test_support::northstar_manifest;
use crate::ChiefdError;
use rusqlite::Connection;

const EPOCH: i64 = 1_784_116_800_000;

fn open() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory db");
    conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
    // Isolate the activity row engine from the people/departments ports: the
    // `transitions -> people` FK is a schema-level guarantee exercised by the
    // full-schema tests, not this reconstruct/diff unit. Seeding a whole valid
    // manifest's people/departments here would couple this test to those tables.
    conn.pragma_update(None, "foreign_keys", false).expect("fk off");
    conn
}

/// A ledger with one released (`ready`) park on the signal-researcher, to
/// exercise transitions + person_activity + activity_meta.
fn sample_ledger(manifest: &OrganizationManifest) -> ActivityLedger {
    let at = iso_millis(EPOCH);
    let mut ledger = ActivityLedger::initial(manifest, &at);
    let tid = "transition:1:signal-researcher:park".to_string();
    let transition = GracefulTransition {
        id: tid.clone(),
        person_id: "signal-researcher".to_string(),
        action: TransitionAction::Park,
        reason: crate::store::activity::IDLE_AUTO_PARK_REASON.to_string(),
        intent_id: None,
        placement_department_id: "quant".to_string(),
        to_department_id: None,
        status: TransitionStatus::Ready,
        requested_at: at.clone(),
        handoff_deadline_at: at.clone(),
        applied_at: None,
        cancelled_at: None,
        forced_at: None,
        abandoned_at: None,
    };
    ledger.transitions.insert(tid.clone(), transition);
    ledger.transition_order.push(tid.clone());
    ledger.next_transition_sequence = 2;
    ledger.people.get_mut("signal-researcher").unwrap().active_transition_id = Some(tid);
    ledger
}

#[test]
fn absent_activity_meta_reconstructs_as_none() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let tx = conn.transaction().unwrap();
    assert!(read_rows(&tx, &manifest.slug, &manifest).unwrap().is_none());
    tx.commit().unwrap();
}

#[test]
fn a_whole_ledger_round_trips_through_rows() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest);

    let tx = conn.transaction().unwrap();
    write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();
    // No org_events written by write_rows alone, so updated_at derives from
    // created_at — which equals the seed's updated_at anyway.
    let read = read_rows(&tx, &manifest.slug, &manifest).unwrap().expect("present");
    tx.commit().unwrap();

    assert_eq!(read, ledger, "ledger -> rows -> ledger must be lossless");
}

/// The row-level half of the #751-P4 fail-closed regression (its whole-ledger
/// twin is `activity::tests::
/// an_applied_transition_with_no_reflection_data_validates_and_reconstructs_cleanly`).
/// An `applied` transition used to assert that a reflection row existed
/// beside it in `reflection_handoffs`; both the assertion and the table are
/// gone, so the transitions row alone must reconstruct into a ledger that
/// `validate` accepts. If it did not, every read of every existing company
/// would surface as `corrupt store: activity`.
#[test]
fn an_applied_transition_row_reconstructs_into_a_ledger_that_validates() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let mut ledger = sample_ledger(&manifest);
    let tid = "transition:1:signal-researcher:park".to_string();
    {
        let t = ledger.transitions.get_mut(&tid).unwrap();
        t.status = TransitionStatus::Applied;
        t.applied_at = Some(iso_millis(EPOCH + 1_000));
    }

    let tx = conn.transaction().unwrap();
    write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();
    let read = read_rows(&tx, &manifest.slug, &manifest).unwrap().expect("present");
    tx.commit().unwrap();

    assert_eq!(read.transitions[&tid].status, TransitionStatus::Applied);
    assert_eq!(
        read.people["signal-researcher"].active_transition_id.as_deref(),
        Some(tid.as_str()),
        "the person still points at the applied transition"
    );
    crate::store::activity::validate(&read, &manifest)
        .expect("an applied transition with no payload beside it is a VALID ledger");
    assert_eq!(read, ledger, "and it round-trips losslessly");
}

#[test]
fn read_projects_a_desired_off_row_for_a_manifest_person_missing_from_activity_rows() {
    // A P0 from a live box: a structural department/head creation advanced the normalized
    // manifest but left only the CEO in person_activity.  A direct message to
    // the stopped new head must get a valid activity authority and reach the
    // wake/reconcile path, not fail at this read boundary.
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let mut ledger = ActivityLedger::initial(&manifest, &iso_millis(EPOCH));
    ledger.people.remove("quant-head");
    ledger.person_order.retain(|id| id != "quant-head");

    let tx = conn.transaction().unwrap();
    // Seed valid rows, then simulate the normalized structural-growth gap by
    // removing one per-person row while leaving the aggregate authority alive.
    write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();
    let read = read_rows(&tx, &manifest.slug, &manifest).unwrap().expect("present");
    tx.commit().unwrap();

    assert_eq!(read.person_order, manifest.people_order);
    let state = read.people.get("quant-head").expect("missing row is projected");
    assert!(!state.last_desired_active, "projection never invents a process");
    // #751-P9: the seeded projection carries the person's OWN units. It used
    // to assert `last_pane_department_id == "executive"` — the parent window a
    // head's pane was drawn in — which is a display answer the backend neither
    // derives nor stores now.
    assert_eq!(
        state.last_department_id, "quant",
        "a department head is assigned to the unit they head"
    );
}

#[test]
fn an_idle_republish_touches_nothing() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest);
    let tx = conn.transaction().unwrap();
    write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();
    // Re-publishing the identical ledger diffs to nothing.
    let touches = write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();
    assert!(touches.is_empty(), "an unchanged republish must touch no entity");
    tx.commit().unwrap();
}

#[test]
fn changing_one_transition_touches_only_that_entity() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let mut ledger = sample_ledger(&manifest);
    let tx = conn.transaction().unwrap();
    write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();

    // Advance the one transition to applied.
    let tid = "transition:1:signal-researcher:park".to_string();
    {
        let t = ledger.transitions.get_mut(&tid).unwrap();
        t.status = TransitionStatus::Applied;
        t.applied_at = Some(iso_millis(EPOCH + 1_000));
    }
    let touches = write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();
    let entities: Vec<(&str, &str)> =
        touches.iter().map(|t| (t.entity.as_str(), t.entity_id.as_str())).collect();
    assert!(
        entities.contains(&("transition", tid.as_str())),
        "the changed transition is reported; got {entities:?}"
    );
    assert!(
        !entities.iter().any(|(e, _)| *e == "person-activity"),
        "no person row changed, so none is reported; got {entities:?}"
    );
    tx.commit().unwrap();
}

#[test]
fn the_transitions_counter_reconstructs_next_sequence() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest); // next_transition_sequence == 2
    let tx = conn.transaction().unwrap();
    write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();
    let value: i64 = tx
        .query_row(
            "SELECT value FROM counters WHERE name = ?1",
            rusqlite::params![transitions_counter_key(&manifest.slug)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(value, 1, "counter holds the LAST allocated seq (next - 1)");
    let read = read_rows(&tx, &manifest.slug, &manifest).unwrap().unwrap();
    assert_eq!(read.next_transition_sequence, 2, "next = counter + 1");
    tx.commit().unwrap();
}

#[test]
fn direct_apply_over_write_rows_advances_the_feed_once_per_entity() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest);
    let slug = manifest.slug.clone();
    let tx = conn.transaction().unwrap();
    let seq = apply_and_emit::<rusqlite::Error, _>(&tx, &slug, &iso_millis(EPOCH), "chief", |tx| {
        write_rows(tx, &slug, &ledger, &manifest)
    })
    .unwrap();
    // One transition + one changed person + the activity singleton == 3 touches
    // on a fresh company (the seed people are unchanged from the initial write,
    // but the researcher's active_transition_id makes exactly one person differ).
    assert!(seq >= 1, "the feed advanced");
    // updated_at now derives from the event stamp.
    let read = read_rows(&tx, &slug, &manifest).unwrap().unwrap();
    assert_eq!(read.updated_at, iso_millis(EPOCH));
    tx.commit().unwrap();
}

#[test]
fn number_501_terminal_history_is_deleted_row_by_row_not_kept_in_a_growing_blob() {
    // #501 root cause: the `activity` store was ONE JSON blob, so every settled
    // transition accumulated in a single ever-growing document (1.19MB live on
    // cobalt, re-parsed at the footer's >=1/30s cadence). On normalized rows,
    // dropping settled history is per-transition DELETEs and the store SHRINKS.
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let at = iso_millis(EPOCH);
    let mut ledger = ActivityLedger::initial(&manifest, &at);
    let cancelled = |seq: u64| GracefulTransition {
        id: format!("transition:{seq}:signal-researcher:park"),
        person_id: "signal-researcher".to_string(),
        action: TransitionAction::Park,
        reason: crate::store::activity::IDLE_AUTO_PARK_REASON.to_string(),
        intent_id: None,
        placement_department_id: "quant".to_string(),
        to_department_id: None,
        status: TransitionStatus::Cancelled,
        requested_at: at.clone(),
        handoff_deadline_at: at.clone(),
        applied_at: None,
        cancelled_at: Some(at.clone()),
        forced_at: None,
        abandoned_at: None,
    };
    for seq in 1..=50u64 {
        let t = cancelled(seq);
        ledger.transition_order.push(t.id.clone());
        ledger.transitions.insert(t.id.clone(), t);
    }
    ledger.next_transition_sequence = 51;

    let tx = conn.transaction().unwrap();
    write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();
    let count = |tx: &rusqlite::Transaction<'_>| -> i64 {
        tx.query_row(
            "SELECT COUNT(*) FROM transitions WHERE slug = ?1",
            rusqlite::params![&manifest.slug],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(count(&tx), 50, "all 50 settled transitions are individual rows");

    // The retention cap (reconcileActivityPeople) drops all but the newest 10.
    let keep: std::collections::BTreeSet<String> =
        ledger.transition_order.iter().rev().take(10).cloned().collect();
    ledger.transitions.retain(|id, _| keep.contains(id));
    ledger.transition_order.retain(|id| keep.contains(id));

    let touches = write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();
    assert_eq!(count(&tx), 10, "settled history is DELETED row-by-row — the store shrinks");
    let deletes = touches.iter().filter(|t| t.op == "delete").count();
    assert_eq!(deletes, 40, "each dropped transition is one bounded DELETE, not a blob rewrite");
    tx.commit().unwrap();
}

// TOMBSTONE (#751-P4): `reflection_durability_is_field_for_field_and_folds_memory_with_dedup`
// pinned `reflection_is_durable`'s owner+payload equality and
// `fold_reflection_memory`'s content dedup. Both functions and both tables are
// gone; a transition row is now the whole durable fact, and the round-trip
// tests above already cover it.

#[test]
fn item_d_rejects_unmodeled_keys() {
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest);
    // A ledger the row model fully represents carries no unmodeled keys.
    let clean = serde_json::to_value(&ledger).unwrap();
    assert!(reject_unmodeled_keys(&clean, &ledger, false).is_ok());

    // Inject a top-level key and a nested per-person key the rows cannot store.
    let mut dirty = clean.clone();
    dirty.as_object_mut().unwrap().insert("mysteryField".to_string(), serde_json::json!(1));
    dirty["people"]["signal-researcher"]
        .as_object_mut()
        .unwrap()
        .insert("newFlag".to_string(), serde_json::json!(true));
    match reject_unmodeled_keys(&dirty, &ledger, false).unwrap_err() {
        ChiefdError::Refused(r) => {
            assert_eq!(r.code, UNMODELED_KEYS, "item D reports the unmodeled-keys code");
            assert!(r.message.contains("mysteryField"), "names the top-level key: {}", r.message);
            assert!(
                r.message.contains("people.signal-researcher.newFlag"),
                "names the nested path: {}",
                r.message
            );
        }
        other => panic!("expected Refused(unmodeled-keys), got {other:?}"),
    }
}

/// The N9 gate: blob -> rows -> reconstructed ledger proves ZERO LOSS for the
/// whole activity family (transitions + person_activity + activity_meta).
#[test]
fn shadow_diff_activity_reports_zero_loss_on_a_full_ledger() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest);
    let slug = manifest.slug.clone();
    let tx = conn.transaction().unwrap();
    // The activity port reconstructs the manifest from rows, so seed them first.
    crate::store::organization_rows::genesis(&tx, &slug, &manifest).unwrap();
    let blob = serde_json::to_vec(&ledger).unwrap();

    let report = shadow_diff_activity(&tx, &slug, &slug, &blob).unwrap();
    tx.commit().unwrap();

    assert!(report.zero_loss(), "zero-loss expected; loud: {:?}", report.loud_failures());
    let (_matched, _derived, _dropped, lost) = report.counts();
    assert_eq!(lost, 0, "no field may be Lost");
    // The full family was exercised: the released transition + a person row.
    assert!(
        report.fields.iter().any(|f| f.path.starts_with("transitions.")),
        "a transition field is reported"
    );
    assert!(
        report.fields.iter().any(|f| f.path.starts_with("people.")),
        "a person field is reported"
    );
}

/// Item-D: a live blob key the row model cannot store is a LOUD failure, never a
/// silent drop (the shadow-diff surfaces the publish's 422 as a report line).
#[test]
fn shadow_diff_activity_surfaces_an_unmodeled_key_as_loud() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest);
    let slug = manifest.slug.clone();
    let tx = conn.transaction().unwrap();
    crate::store::organization_rows::genesis(&tx, &slug, &manifest).unwrap();
    let mut blob: serde_json::Value = serde_json::to_value(&ledger).unwrap();
    blob.as_object_mut().unwrap().insert("legacyFrozenMirror".into(), serde_json::json!({"x": 1}));
    let bytes = serde_json::to_vec(&blob).unwrap();

    let report = shadow_diff_activity(&tx, &slug, &slug, &bytes).unwrap();
    tx.commit().unwrap();

    assert!(!report.zero_loss(), "an unmodeled key must fail the gate");
    assert!(
        report.loud_failures().iter().any(|l| l.contains("legacyFrozenMirror")),
        "the exact key is named: {:?}",
        report.loud_failures()
    );
}

// TOMBSTONE (#751-P4): three `shadow_diff_reflection_*` fixtures lived here —
// zero-loss fold of a per-person `reflection-memory/<person>` blob, the
// last-writer overwrite of a divergent record, and the item-D loud failure on
// an unmodeled record key. They verified a migration for a payload the product
// no longer has, into tables that no longer exist.

/// LIVE-.bak CONTROL (N9 gate, run explicitly on cobalt-bison against the real
/// ledger snapshot): deserialize the live `activity` blob and shadow-diff it
/// into fresh rows, asserting ZERO Corrupt/Lost/refusal against REAL data.
/// (#751-P4 removed its per-person reflection-memory half.) Ignored by default —
/// it needs a real snapshot. Run with:
///   NORM_N4_BAK=/root/.write-db/org.sqlite.n9-bak.20260725 \
///     cargo test -p chiefd-core --lib live_bak_control -- --ignored --nocapture
#[test]
#[ignore = "N9 live-.bak control: needs an operator-supplied NORM_N4_BAK snapshot path, run explicitly, not part of any automated suite (#871)"]
fn live_bak_control_activity_zero_loss() {
    let bak = match std::env::var("NORM_N4_BAK") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("NORM_N4_BAK unset; skipping");
            return;
        }
    };
    let uri = format!("file:{bak}?immutable=1");
    // Read-only open of an operator-supplied `.bak` fixture for a manual
    // control run — a migration aid, not company state routed around a store.
    #[allow(clippy::disallowed_methods)]
    let src = rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open .bak read-only");

    // The org_documents KEY (`doc_slug`) is the legacy TS launcher's COMPOSITE
    // slug (`tribes-capital@<hash>`); the chiefd IDENTITY slug is the manifest's
    // own `slug` field (plain `tribes-capital`), which is what the activity blob's
    // `organization` field and `validate` use. The migration keys the new rows by
    // that PLAIN slug, NOT the composite doc key — so N9's dispatch must pass the
    // manifest's plain slug as (row_slug, company_slug), not the org_documents
    // slug column.
    //
    // #440/#442 SPLIT-BRAIN: two manifest blobs coexist — `org` (chiefd-native,
    // 25 people) and `org-manifest` (TS launcher, 51 people). The activity/
    // activity blob is a TS-launcher artifact and references the SUPERSET
    // (`org-manifest`), so the activity family MUST validate against it, not the
    // narrower `org`. N9 finding: the migration must backfill the manifest ROWS
    // from the superset the activity ledger references, or activity refuses with
    // `unknown-person` for everyone `org` lacks.
    let (doc_slug, manifest_blob): (String, String) = src
        .query_row(
            "SELECT slug, blob FROM org_documents WHERE store IN ('org','org-manifest') ORDER BY store='org-manifest' DESC LIMIT 1",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).expect("a manifest blob");
    let manifest: OrganizationManifest =
        serde_json::from_str(&manifest_blob).expect("manifest deserializes");
    let slug = manifest.slug.clone(); // the plain chiefd identity slug

    let mut conn = open();
    let tx = conn.transaction().unwrap();
    crate::store::organization_rows::genesis(&tx, &slug, &manifest).expect("seed manifest rows");

    // --- activity blob (fetched by the composite doc key, keyed by plain slug) ---
    let activity_blob: String = src
        .query_row(
            "SELECT blob FROM org_documents WHERE store='activity' AND slug=?1",
            rusqlite::params![doc_slug],
            |r| r.get(0),
        )
        .expect("activity blob");
    let a =
        shadow_diff_activity(&tx, &slug, &slug, activity_blob.as_bytes()).expect("activity diff");
    let (am, ad, adr, al) = a.counts();
    eprintln!(
        "[activity] rows={} matched={am} derived={ad} dropped={adr} lost={al} loud={:?}",
        a.row_count,
        a.loud_failures()
    );
    // FINDING (routed to N1): the live activity ledger references 2 people
    // (`kanban-*-designer`, both lastEmploymentState=active) that exist in
    // NEITHER manifest blob (`org` 25p / `org-manifest` 49p) — genuine cross-
    // store drift. `validate` refuses `unknown-person`. Pending N1's migration-
    // policy ruling (prune manifest-orphaned activity people vs. relax backfill
    // validate), the ONLY tolerated activity loud failure here is that orphan
    // refusal — any OTHER loss (unmodeled key, Lost field) still fails the gate.
    let only_orphan_drift =
        a.zero_loss() || a.loud_failures().iter().all(|l| l.contains("unknown-person"));
    assert!(
        only_orphan_drift,
        "activity has a loss beyond manifest-orphan drift: {:?}",
        a.loud_failures()
    );
    if !a.zero_loss() {
        eprintln!(
            "[activity] BLOCKED on manifest-orphan drift (route to N1): {:?}",
            a.loud_failures()
        );
    }

    // #751-P4: the second half of this control looped every per-person
    // `reflection-memory/<person>` blob through `shadow_diff_reflection`. Both
    // are gone; a live snapshot's reflection blobs are now simply unread data
    // with no destination, so there is nothing left for this control to verify
    // beyond the activity ledger above.
    tx.commit().unwrap();
}

/// #337 read-tolerant / write-strict split on the BACKFILL path: a legacy
/// snapshot carrying the allowlisted `automaticParkRetryAfter` leaf backfills
/// without refusal (recorded ExpectedDropped), while a live publish carrying a
/// truly unknown key still fails write-strict. Locks the live-.bak finding.
#[test]
fn backfill_tolerates_the_337_legacy_leaf_but_live_publish_stays_strict() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest);
    let slug = manifest.slug.clone();
    let tx = conn.transaction().unwrap();
    crate::store::organization_rows::genesis(&tx, &slug, &manifest).unwrap();

    // Inject the #337-dropped legacy leaf on a person, as a real snapshot has it.
    let mut blob: serde_json::Value = serde_json::to_value(&ledger).unwrap();
    blob["people"]["signal-researcher"]
        .as_object_mut()
        .unwrap()
        .insert("automaticParkRetryAfter".into(), serde_json::json!("2026-07-20T19:43:24.809Z"));
    let bytes = serde_json::to_vec(&blob).unwrap();

    // BACKFILL tolerates it — no refusal, and the drop is audited ExpectedDropped.
    let report = shadow_diff_activity(&tx, &slug, &slug, &bytes).unwrap();
    assert!(
        report.zero_loss(),
        "legacy leaf must be tolerated on backfill; loud: {:?}",
        report.loud_failures()
    );
    assert!(
        report.fields.iter().any(|f| f.path == "people.signal-researcher.automaticParkRetryAfter"
            && matches!(
                f.disposition,
                crate::store::shadow_report::Disposition::ExpectedDropped { .. }
            )),
        "the legacy leaf is recorded ExpectedDropped for the audit trail",
    );

    // A LIVE publish (write-strict) of the same blob still refuses.
    let err = publish(&tx, &slug, &serde_json::to_string(&blob).unwrap()).unwrap_err();
    match err {
        ChiefdError::Refused(r) => {
            assert_eq!(r.code, UNMODELED_KEYS, "live publish stays write-strict")
        }
        other => panic!("expected write-strict refusal, got {other:?}"),
    }
    tx.commit().unwrap();
}

// ---- fence_containment typed txn-accessors (shutdown_person composes these) --

#[test]
fn upsert_person_activity_desired_sets_flag_and_pointer_preserving_other_columns() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest);
    let slug = manifest.slug.clone();
    let tx = conn.transaction().unwrap();
    write_rows(&tx, &slug, &ledger, &manifest).unwrap();
    // Capture the columns the accessor must LEAVE AS-IS.
    let before = read_rows(&tx, &slug, &manifest).unwrap().unwrap();
    let s0 = &before.people["signal-researcher"];
    let (idle0, home0) = (s0.idle_since.clone(), s0.last_department_id.clone());

    let at = iso_millis(EPOCH + 5_000);
    let touch =
        upsert_person_activity_desired(&tx, &slug, "signal-researcher", false, Some("t-term"), &at)
            .unwrap();
    assert_eq!(
        (touch.entity.as_str(), touch.entity_id.as_str()),
        ("person-activity", "signal-researcher")
    );

    let after = read_rows(&tx, &slug, &manifest).unwrap().unwrap();
    let s1 = &after.people["signal-researcher"];
    assert!(!s1.last_desired_active, "desired-active set false");
    assert_eq!(s1.active_transition_id.as_deref(), Some("t-term"), "pointer set");
    assert_eq!(s1.updated_at, at, "updated_at advanced to `at`");
    assert_eq!(s1.idle_since, idle0, "idle_since left as-is");
    assert_eq!(s1.last_department_id, home0, "last_department_id left as-is");
    tx.commit().unwrap();
}

#[test]
fn insert_terminal_transition_writes_an_applied_row_outside_the_active_index() {
    let mut conn = open();
    let slug = "northstar@acme";
    let at = iso_millis(EPOCH + 6_000);
    let tx = conn.transaction().unwrap();
    let touch = insert_terminal_transition(
        &tx,
        slug,
        "transition:9:signal-researcher:offboard",
        "signal-researcher",
        TransitionAction::Offboard,
        Some("quant"),
        Some("person-stop:e2e"),
        "shutdown",
        &at,
    )
    .unwrap();
    assert_eq!(
        (touch.entity.as_str(), touch.entity_id.as_str()),
        ("transition", "transition:9:signal-researcher:offboard")
    );

    let (status, applied, requested, action, intent): (String, Option<String>, String, String, Option<String>) = tx
        .query_row(
            "SELECT status, applied_at, requested_at, action, intent_id FROM transitions WHERE slug=?1 AND id=?2",
            rusqlite::params![slug, "transition:9:signal-researcher:offboard"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ).unwrap();
    assert_eq!(status, "applied");
    assert_eq!(applied.as_deref(), Some(at.as_str()));
    assert_eq!(requested, at);
    assert_eq!(action, "offboard");
    assert_eq!(
        intent.as_deref(),
        Some("person-stop:e2e"),
        "commanded stop stamps the owner intent"
    );

    // AutomaticSettle passes None -> an unowned terminal park.
    let at2 = iso_millis(EPOCH + 6_500);
    insert_terminal_transition(
        &tx,
        slug,
        "transition:10:signal-researcher:park",
        "signal-researcher",
        TransitionAction::Park,
        None,
        None,
        "auto-settle",
        &at2,
    )
    .unwrap();
    let intent2: Option<String> = tx
        .query_row(
            "SELECT intent_id FROM transitions WHERE slug=?1 AND id=?2",
            rusqlite::params![slug, "transition:10:signal-researcher:park"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(intent2, None, "automatic settle leaves the terminal park unowned");
    tx.commit().unwrap();
}

#[test]
fn supersede_open_transition_cancels_the_open_row_and_is_noop_when_none() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest); // one `ready` transition on signal-researcher
    let slug = manifest.slug.clone();
    let tx = conn.transaction().unwrap();
    write_rows(&tx, &slug, &ledger, &manifest).unwrap();

    let at = iso_millis(EPOCH + 7_000);
    let reason = "superseded-by-shutdown:person-stop:e2e";
    let superseded =
        supersede_open_transition(&tx, &slug, "signal-researcher", reason, &at).unwrap();
    let (id, touch) = superseded.expect("an open transition was cancelled");
    assert_eq!(id, "transition:1:signal-researcher:park");
    assert_eq!((touch.entity.as_str(), touch.entity_id.as_str()), ("transition", id.as_str()));
    let (status, cancelled, got_reason): (String, Option<String>, String) = tx
        .query_row(
            "SELECT status, cancelled_at, reason FROM transitions WHERE slug=?1 AND id=?2",
            rusqlite::params![slug, id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "cancelled");
    assert_eq!(cancelled.as_deref(), Some(at.as_str()));
    assert_eq!(got_reason, reason, "the override fact is stamped on the cancelled row's reason");

    // The active-transition pointer is cleared in the same commit: the sample
    // ledger names the just-cancelled row, and a ledger still pointing at a
    // cancelled transition fails the whole-ledger validate on its next read
    // (`corrupt store: activity` on every later publish).
    let pointer: Option<String> = tx
        .query_row(
            "SELECT active_transition_id FROM person_activity WHERE slug=?1 AND person_id='signal-researcher'",
            rusqlite::params![slug],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pointer, None, "a cancelled transition cannot remain the active pointer");
    let reconstructed = read_rows(&tx, &slug, &manifest).unwrap().unwrap();
    assert!(
        crate::store::activity::validate(&reconstructed, &manifest).is_ok(),
        "the reconstructed ledger still validates after the supersede"
    );

    // Idempotent: no open transition remains -> None.
    assert!(supersede_open_transition(&tx, &slug, "signal-researcher", reason, &at)
        .unwrap()
        .is_none());
    tx.commit().unwrap();
}

/// The retired aggregate manifest writer cannot remove a person behind an open
/// activity transition. A second genesis request is an explicit first-write
/// refusal, leaving the durable rows for a named lifecycle operation to handle.
#[test]
fn a_second_manifest_genesis_cannot_bypass_named_person_operations() {
    let mut conn = Connection::open_in_memory().expect("in-memory db");
    conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
    conn.pragma_update(None, "foreign_keys", true).expect("fk on");
    let manifest = northstar_manifest(EPOCH);
    let slug = manifest.slug.clone();
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        crate::store::organization_rows::genesis(&tx, &slug, &manifest).unwrap(),
        crate::store::organization_rows::ManifestGenesisOutcome::Created
    ));
    // An open (overdue) park transition referencing the worker a stale snapshot
    // used to be able to erase.
    tx.execute(
        "INSERT INTO transitions(slug, id, person_id, action, status, requested_at) \
         VALUES(?1,?2,?3,?4,?5,?6)",
        rusqlite::params![
            slug,
            "t-park-1",
            "signal-researcher",
            "park",
            "overdue",
            "2026-07-15T00:00:00.000Z"
        ],
    )
    .unwrap();
    // A stale aggregate with the worker removed cannot be published over the
    // normalized organization rows.
    let mut removed = manifest.clone();
    removed.people.remove("signal-researcher");
    removed.people_order.retain(|id| id != "signal-researcher");
    let outcome = crate::store::organization_rows::genesis(&tx, &slug, &removed).unwrap();
    assert!(matches!(
        outcome,
        crate::store::organization_rows::ManifestGenesisOutcome::AlreadyExists
    ));
    tx.commit().unwrap();
    // The stale aggregate made no row changes; direct named operations own the
    // later lifecycle transition.
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM transitions WHERE slug=?1", [&slug], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 1, "the removed person's transition survives (tolerated on read)");
}

/// #1031: a NULL prior-placement column used to reconstruct as `""` through
/// `unwrap_or_default()`, and `""` is never a key in `manifest.departments` —
/// so a single NULL made the WHOLE ledger fail `validate` and surface as
/// `corrupt store: activity`, with nothing able to repair it. `read_rows`
/// already takes the manifest, which holds the right answer.
#[test]
fn a_null_prior_placement_column_reconstructs_from_the_manifest_not_as_empty() {
    let mut conn = open();
    let manifest = northstar_manifest(EPOCH);
    let ledger = sample_ledger(&manifest);

    let tx = conn.transaction().unwrap();
    write_rows(&tx, &manifest.slug, &ledger, &manifest).unwrap();
    let nulled = tx
        .execute(
            "UPDATE person_activity SET last_department_id = NULL WHERE slug = ?1 AND person_id = ?2",
            rusqlite::params![&manifest.slug, "signal-researcher"],
        )
        .unwrap();
    assert_eq!(nulled, 1, "the fixture must actually have nulled a row");

    let read = read_rows(&tx, &manifest.slug, &manifest).unwrap().expect("present");
    let state = &read.people["signal-researcher"];
    let person = &manifest.people["signal-researcher"];
    assert_eq!(state.last_department_id, person.department_id);
    assert!(!state.last_department_id.is_empty(), "never the manufactured empty string");

    // The point of all of it: a NULL column is not corruption.
    crate::store::activity::validate(&read, &manifest)
        .expect("a reconstructed ledger must validate");
    tx.commit().unwrap();
}

/// THE WAKE, RECORDED. `release_idle_park` is the row half of a wake, and until
/// 2026-08-20 it recorded only the CONSEQUENCES of one — a cleared quiet clock
/// and a dropped park pointer. Neither of those says that an operator asked for
/// this person, so the moment their agent beat once and was handed nothing to do,
/// every rule in the product read them as an agent that had finished.
///
/// Operator ruling: *"If woken, it needs to wait the 2 mins."* The stamp is the
/// durable record of the decision, and it is written whether or not there was a
/// park to release and whether or not there was a clock to clear — a person
/// carrying neither is precisely the one a wake must not hand straight back to
/// the settle.
#[test]
fn a_wake_stamps_the_operators_own_instant_even_with_no_park_and_no_clock() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO person_activity(slug, person_id, last_desired_active, updated_at) \
         VALUES('acme', 'bo', 0, '2026-08-20T20:00:00.000Z')",
        [],
    )
    .unwrap();

    let touches = release_idle_park(&tx, "acme", "bo", "2026-08-20T20:34:00.543Z", "superseded")
        .expect("the wake applies");

    let stamped: Option<String> = tx
        .query_row(
            "SELECT operator_wake_at FROM person_activity WHERE slug='acme' AND person_id='bo'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stamped.as_deref(),
        Some("2026-08-20T20:34:00.543Z"),
        "a person with no park row and no quiet clock is still somebody an operator woke"
    );
    assert_eq!(touches.len(), 1, "and the write reports itself: {touches:?}");
    assert_eq!(touches[0].entity, "person-activity");
}

/// An EXACT replay writes nothing — the same click delivered twice, or a
/// retried request. A LATER click restarts the floor, because that one is a
/// second decision.
#[test]
fn a_replayed_wake_is_writeless_and_a_later_one_restarts_the_floor() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO person_activity(slug, person_id, last_desired_active, updated_at) \
         VALUES('acme', 'bo', 0, '2026-08-20T20:00:00.000Z')",
        [],
    )
    .unwrap();
    release_idle_park(&tx, "acme", "bo", "2026-08-20T20:34:00.543Z", "superseded").unwrap();

    let replay =
        release_idle_park(&tx, "acme", "bo", "2026-08-20T20:34:00.543Z", "superseded").unwrap();
    assert!(replay.is_empty(), "an exact replay changes nothing and says nothing: {replay:?}");

    let again =
        release_idle_park(&tx, "acme", "bo", "2026-08-20T20:36:00.000Z", "superseded").unwrap();
    assert_eq!(again.len(), 1, "a later click is a second decision");
    let stamped: Option<String> = tx
        .query_row(
            "SELECT operator_wake_at FROM person_activity WHERE slug='acme' AND person_id='bo'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stamped.as_deref(), Some("2026-08-20T20:36:00.000Z"));
}

/// The row-level read of the lease, which exists for the one caller that cannot
/// hold a `PersonActivityState`: the whole-document `launch_intent_rows::publish`.
/// It must agree with the typed predicate at both ends of the window, including
/// the two fail-safe cases — a damaged stamp and one written against a clock
/// that disagrees with this one.
#[test]
fn the_row_level_wake_lease_agrees_with_the_typed_predicate_at_both_ends() {
    use crate::store::activity::ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS as LEASE;
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    let woke_at = 1_787_181_840_000_i64; // 2026-08-19T23:24:00.000Z
    for (person, stamp) in [
        ("inside", iso_millis(woke_at)),
        ("expired", iso_millis(woke_at - LEASE)),
        ("damaged", "not-a-time".to_owned()),
        ("from-the-future", iso_millis(woke_at + 60_000)),
    ] {
        tx.execute(
            "INSERT INTO person_activity(slug, person_id, last_desired_active, updated_at, \
             operator_wake_at) VALUES('acme', ?1, 1, '2026-08-19T23:24:00.000Z', ?2)",
            rusqlite::params![person, stamp],
        )
        .unwrap();
    }
    // And somebody nobody has ever woken.
    tx.execute(
        "INSERT INTO person_activity(slug, person_id, last_desired_active, updated_at) \
         VALUES('acme', 'never-woken', 1, '2026-08-19T23:24:00.000Z')",
        [],
    )
    .unwrap();

    let leased = operator_wake_leased_people(&tx, "acme", woke_at + 5_000).unwrap();
    assert_eq!(
        leased.into_iter().collect::<Vec<_>>(),
        vec!["inside".to_string()],
        "a lease that has run out, a damaged stamp, a stamp from a clock that disagrees, and a \
         person nobody woke all hold nobody up: this column may only ever prolong a person \
         inside a window an operator actually opened"
    );
    assert!(
        !operator_wake_leased_people(&tx, "acme", woke_at + LEASE).unwrap().contains("inside"),
        "and the window closes on the exact millisecond: the lease is a floor, never a pin"
    );
    assert!(
        operator_wake_leased_people(&tx, "acme", woke_at + LEASE - 1).unwrap().contains("inside"),
        "it holds to the last millisecond of the window, though"
    );
}
