//! Revisionless direct-person transfer over the real Chiefd HTTP surface.
//!
//! A transfer is a semantic staffing decision, not an optimistic whole-company
//! document publish.  These tests prove that a caller never sends an event
//! sequence, that two independent people can transfer concurrently through the
//! production router, and that the committed placements reconstruct from the
//! normalized SQL rows.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{
    router_with_supervision_live, ChangeFeed, DocStore, SupervisionLiveSource,
};
use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SystemClock;
use chiefd_core::test_support::northstar_manifest;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const EPOCH: i64 = 1_784_116_800_000;

struct World {
    _dir: tempfile::TempDir,
    slug: String,
    company: Arc<CompanyDb>,
    app: axum::Router,
}

async fn world(tag: &str) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let org_documents_slug = format!("cobalt@{tag}");
    let mut manifest = northstar_manifest(EPOCH);
    // The activity/supervision row backfills validate the seeded ledger's
    // embedded org slug against the reconstructed manifest, whose slug is
    // DERIVED from the CompanyDb label ("cobalt") — the fixture defaults to
    // "northstar-conformance", so it must be corrected before genesis.
    manifest.slug = "cobalt".to_string();
    // The fixture intentionally has one non-head worker. Clone it to exercise
    // two independent concurrent direct transfers without inventing a second
    // production-only route or bypassing normalized manifest validation.
    let mut second = manifest.people["signal-researcher"].clone();
    second.id = "signal-researcher-two".to_string();
    second.name = "Signal Researcher Two".to_string();
    manifest.people_order.push(second.id.clone());
    manifest.people.insert(second.id.clone(), second);

    let company = Arc::new(
        CompanyDb::open("cobalt", &path, Arc::new(SystemClock::default()))
            .expect("open company db"),
    );
    let genesis_manifest = manifest.clone();
    company
        .org_manifest_genesis(
            manifest,
            "2026-01-01T00:00:00.000Z".to_owned(),
            chiefd_core::store::person_contracts::build::build_organization_person_contracts(
                &genesis_manifest,
            )
            .expect("person contracts document"),
        )
        .await
        .expect("manifest genesis commits");
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&path.display().to_string(), 2, Arc::clone(&feed))
            .expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let live = SupervisionLiveSource::new(Arc::clone(&company), org_documents_slug.clone());
    let app =
        router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live))
            .layer(axum::extract::Extension(ceo_caller(&org_documents_slug)));
    World { _dir: dir, slug: org_documents_slug, company, app }
}

/// The credential every request in this file carries: northstar's CEO, as a
/// PERSON, scoped by the COMPOSITE document key the route's own-company gate
/// keys on.
///
/// It matches the `"actor": "chief"` the bodies below declare, and that is not a
/// coincidence: on `transfer` the caller's principal OVERWRITES the body's
/// `actor`, so a caller who was somebody else would silently change what these
/// concurrency tests are transferring on behalf of. The CEO heads the root, so
/// no authority refusal can mask the revisionless-concurrency behaviour under
/// test; who may transfer whom is pinned in `person_verbs_caller_http.rs`.
///
/// It is not optional: with the absent-caller arm deleted, a request with no
/// identity never reaches the handler.
fn ceo_caller(company_slug: &str) -> chiefd_api::authn::middleware::CallerIdentity {
    chiefd_api::authn::middleware::CallerIdentity(chiefd_core::store::identities::Identity {
        identity_id: "id-ceo".to_owned(),
        principal: "chief".to_owned(),
        kind: chiefd_core::store::identities::IdentityKind::Person,
        company_slug: Some(company_slug.to_owned()),
        pubkey: Some("test-key".to_owned()),
        fingerprint: "fp-ceo".to_owned(),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

async fn post(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/org/person/transfer")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    // Axum's extractor rejection for an unknown JSON field is deliberately a
    // bare client error. Successful and policy-refusal route responses are
    // JSON, while this test also needs to assert the bare legacy-field reject.
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn transfer_body(slug: &str, person_id: &str) -> Value {
    json!({
        "slug": slug,
        "personId": person_id,
        "destinationId": "it",
        "intent": format!("person-transfer:{person_id}"),
        // B1: `actor` is now authorized as well as recorded. `quant-head` heads
        // the SOURCE department and not `it`, so it may no longer push its own
        // people into a unit it does not manage — the fixture said `quant-head`
        // only because the value was audit prose nothing read. The CEO heads
        // the company root and manages both ends, which is what this test needs
        // it to do; the property under test here is revisionless concurrency,
        // not authority.
        "actor": "chief"
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_transfers_are_revisionless_and_concurrent_over_real_http() {
    let w = world("revisionless-transfer").await;

    // A legacy optimistic fence is a bad request, not a fallback that could
    // revive stale-retry handling in an agent or TS client.
    let mut stale_body = transfer_body(&w.slug, "signal-researcher");
    stale_body["expectedSeq"] = json!(0);
    let (status, _) = post(&w.app, stale_body).await;
    assert!(!status.is_success(), "the direct route must reject expectedSeq");

    let first = transfer_body(&w.slug, "signal-researcher");
    let second = transfer_body(&w.slug, "signal-researcher-two");
    let (first, second) = tokio::join!(post(&w.app, first), post(&w.app, second));
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(first.1["applied"], json!(true));
    assert_eq!(second.0, StatusCode::OK);
    assert_eq!(second.1["applied"], json!(true));

    let (manifest, _seq) = w
        .company
        .org_manifest_read()
        .await
        .expect("read normalized rows")
        .expect("manifest remains present");
    for person_id in ["signal-researcher", "signal-researcher-two"] {
        let person = &manifest.people[person_id];
        assert_eq!(person.department_id, "it");
        assert_eq!(
            person
                .staffing_history
                .as_ref()
                .and_then(|history| history.last())
                .and_then(|entry| entry.get("action")),
            Some(&json!("transferred"))
        );
    }
}
