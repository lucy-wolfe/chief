//! Live-control regression for the normalized session-maintenance row port.
//!
// org-data-normalization P0, N5: a LIVE-.bak control for the session-maintenance
// port. Deserializing the live 1.16MB blob into the row DTO and publishing it
// into a fresh row DB is the cheapest surface for DTO gaps: a missing field
// lands in `extra` and the write-strict half (item D) refuses it 422
// `unmodeled-keys`, and a shape the validator rejects surfaces as a 422 too. A
// clean publish + a byte-equivalent reconstruct proves the DTO covers the live
// corpus.
//
// The blob is NOT committed (1.16MB, live company data). The test reads it from
// `$LIVE_SM_BLOB` and SKIPS (passes) when the env var is unset, so it is a real
// control on the build box without shipping the sample.
#![allow(clippy::expect_used, clippy::panic)]

use chiefd_core::store::session_maintenance::rows;
use chiefd_core::store::session_maintenance::SessionMaintenanceLedger;
use rusqlite::Connection;

#[test]
fn the_live_session_maintenance_blob_ports_into_fresh_rows_cleanly() {
    let Ok(path) = std::env::var("LIVE_SM_BLOB") else {
        eprintln!("LIVE_SM_BLOB unset — skipping the live-.bak control");
        return;
    };
    let raw = std::fs::read_to_string(&path).expect("read the live blob");

    // 1. Deserialize into the row DTO. A type gap fails HERE (not a captured
    //    extra) — that is a hard schema finding.
    let ledger: SessionMaintenanceLedger =
        serde_json::from_str(&raw).expect("live blob deserializes into the row DTO");

    // Surface any captured unmodeled keys explicitly before publish, so the
    // finding names the exact path rather than a bare 422.
    let mut unmodeled: Vec<String> = Vec::new();
    for k in ledger.extra.keys() {
        unmodeled.push(format!("extra.{k}"));
    }
    for (id, r) in &ledger.requests {
        for k in r.extra.keys() {
            unmodeled.push(format!("requests.{id}.extra.{k}"));
        }
    }
    // TOMBSTONE: the company-action and target sweep. The ledger no longer
    // carries either, so a live blob that still holds them lands them in
    // `ledger.extra` — which the first loop above already reports. The control
    // is unchanged in strength: an unmodeled key is still caught, it is simply
    // caught one level up now.
    assert!(
        unmodeled.is_empty(),
        "live blob carries {} unmodeled key(s) the row DTO cannot store: {:?}",
        unmodeled.len(),
        unmodeled
    );

    eprintln!("live corpus: {} requests", ledger.requests.len());

    // 2. Publish into a fresh, schema'd row DB (item D + validate + diff, all in
    //    one direct transaction). A refusal or Corrupt fails the control.
    let mut conn = Connection::open_in_memory().expect("in-memory db");
    conn.execute_batch("PRAGMA foreign_keys=ON;").expect("pragma");
    conn.execute_batch(chiefd_core::schema::COMPANY_SCHEMA_SQL).expect("schema");
    let slug = ledger.organization.clone();
    let tx = conn.transaction().expect("txn");
    let audit_seq = rows::publish(&tx, &slug, &ledger)
        .expect("live blob publishes into fresh rows with 0 refusal / 0 corrupt");
    let observed_audit_seq: i64 = tx
        .query_row("SELECT COALESCE(MAX(seq), 0) FROM org_events WHERE slug = ?1", [&slug], |row| {
            row.get(0)
        })
        .expect("read audit cursor after direct publish");
    assert_eq!(audit_seq, observed_audit_seq, "publish returns the committed audit cursor");
    eprintln!("published through audit cursor {audit_seq}");

    // 3. Reconstruct and prove byte-equivalence (requestIds[] is DERIVED, so a
    //    mismatch there is a real ordering finding).
    let rebuilt = rows::reconstruct(&tx, &slug)
        .expect("reconstruct does not corrupt")
        .expect("a ledger that was just published reconstructs");
    tx.commit().expect("commit");

    let a = serde_json::to_value(&ledger).expect("serialize original");
    let b = serde_json::to_value(&rebuilt).expect("serialize rebuilt");
    assert_eq!(a, b, "reconstruct is byte-equivalent to the published ledger");
}
