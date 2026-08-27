// Scratch diagnostic for BUG-7 (the live `transition-conflict` refusal,
// tribes-capital 2026-07-22): run the production activity-fence projection
// against a read-only copy of the live databases, replicating
// `project_activity_fence` call-for-call, and print the FULL refusal Debug so
// the conflicting person/transition is visible. The committed regression is
// `chiefd-core/tests/live_transition_conflict_repro.rs`; this stays as the
// against-real-databases mirror and is `#[ignore]`d because it needs operator
// copies of the live stores.
//
// Env: REPRO_CHIEF_DB, REPRO_ORG_SQLITE, REPRO_NOW_MS (all optional).
// Run: cargo test -p chiefd-host --test live_repro -- --ignored --nocapture
#![allow(clippy::expect_used, clippy::panic, missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;

use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::clock::SharedClock;
use chiefd_core::store::activity::{self, LaunchFence, ReconcileInput};
use chiefd_core::store::{organization, supervision};
use chiefd_core::test_support::ManualClock;
use chiefd_host::gather::reconciler_facts::ReconcilerFactsStore;

#[tokio::test]
#[ignore = "needs operator copies of the live databases (REPRO_CHIEF_DB / REPRO_ORG_SQLITE)"]
async fn live_production_projection_repro() {
    let chief = std::env::var("REPRO_CHIEF_DB").unwrap_or_else(|_| "/tmp/tl-repro/chief.db".into());
    let org =
        std::env::var("REPRO_ORG_SQLITE").unwrap_or_else(|_| "/tmp/tl-repro/org.sqlite".into());
    let now_ms: i64 = std::env::var("REPRO_NOW_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_784_694_000_000);
    println!("inputs: chief={chief} org={org} now_ms={now_ms}");
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("chief.db");
    std::fs::copy(&chief, &db_path).expect("copy chief.db");
    let clock: SharedClock = Arc::new(ManualClock::starting_at(now_ms as u64, now_ms));
    let db = CompanyDb::open("tribes-capital", &db_path, clock).expect("open company db");

    let store = ReconcilerFactsStore::new(PathBuf::from(&org), "/root/.tribes/data/orgs");
    let snapshot = db.snapshot();
    let manifest = organization::read(snapshot.as_ref()).expect("manifest");
    let person_ids =
        store.launch_intent_person_ids(&manifest.slug, &manifest.slug).expect("launch intent");
    let ceo = manifest.chief_person_id().expect("chief").to_owned();
    let fence = LaunchFence::fenced(
        person_ids.into_iter().filter(|id| *id != ceo && manifest.people.contains_key(id)),
    );
    println!("fence: {fence:?}");

    let result = db
        .mutate(MutationClass::Reconcile, MutationName("repro.activity_fence"), move |ledgers| {
            let manifest = organization::read(ledgers)?;
            let supervision = supervision::read(ledgers, &manifest)?;
            activity::reconcile(
                ledgers,
                &manifest,
                &supervision,
                &ReconcileInput {
                    launch_intent: fence,
                    requested_person_ids: Vec::new(),
                    watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
                },
            )?;
            Ok(())
        })
        .await;
    match result {
        Ok(()) => println!("PROJECTION OK"),
        Err(error) => panic!("REPRODUCED: {error:?}"),
    }
}
