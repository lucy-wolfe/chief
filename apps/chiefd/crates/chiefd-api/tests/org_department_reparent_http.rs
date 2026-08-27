//! The reparent-department HTTP surface — `POST /v1/org/department/reparent`
//! (org_ops atomic family, P1-d — the operator's reorg).
//!
//! ═══ What this file is FOR ═══
//!
//! `CompanyDb::reparent_department` runs the whole verb in ONE BEGIN IMMEDIATE,
//! but that engine is unreachable until the route exists — the "engine tested,
//! feature reachable" gap. This file proves the route end to end over REAL HTTP:
//! a valid reparent commits (200 {applied:true}) even after unrelated feed
//! activity, and each policy refusal is a LOUD 422 kebab code
//! (exec-root-protected, unknown-department, already-under-parent,
//! would-create-cycle), never a quiet
//! 200 body a retry loop would miss. `expectedSeq` is rejected at the boundary:
//! the company writer serializes the operation and callers never retry stale
//! snapshots.
//!
//! The route PATH is load-bearing: the TS `ChiefdBackend.reparentDepartment`
//! (`org-durable-store.ts`) posts `/v1/org/department/reparent` verbatim, so a
//! path drift here silently breaks the reorg. These assertions pin the path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
    app: axum::Router,
}

/// A booted company seeded with the northstar manifest (root `executive` with
/// children `quant` and `it`). label == the internal row slug ("cobalt"); the
/// route's own-company gate keys on `org_documents_slug`.
async fn world(tag: &str) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let org_documents_slug = format!("cobalt@{tag}");
    // The activity/supervision row backfills validate the seeded ledger's
    // embedded org slug against the reconstructed manifest, whose slug is
    // DERIVED from the CompanyDb label ("cobalt") — the fixture defaults to
    // "northstar-conformance", so it must be corrected before genesis.
    let mut manifest = northstar_manifest(EPOCH);
    manifest.slug = "cobalt".to_string();
    let company = Arc::new(
        CompanyDb::open("cobalt", &path, Arc::new(SystemClock::default()))
            .expect("open company db"),
    );
    // Seed the NORMALIZED departments/people rows (the surface reparent reads)
    // through the one-time N2 manifest genesis path.
    let genesis_manifest = manifest.clone();
    company
        .org_manifest_genesis(
            manifest.clone(),
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
    World { _dir: dir, slug: org_documents_slug, app }
}

/// The credential every request in this file carries: northstar's CEO, as a
/// PERSON, scoped by the COMPOSITE document key the route's own-company gate
/// keys on — a display slug would fail the match and turn every test here into
/// a company-mismatch refusal.
///
/// The CEO specifically, because these are WIRE-CONTRACT tests: they pin the
/// reparent route's status codes and its refusal shapes (cycle, unknown
/// department, existing parent, stale `expectedSeq`), and the CEO heads the
/// root so no authority refusal can mask the one under test. Who may reparent
/// what is pinned next door in `department_reparent_caller_http.rs`.
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

async fn post_raw(app: &axum::Router, body: Value) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/org/department/reparent")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8(bytes.to_vec()).expect("utf8 body"))
}

async fn post(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    let (status, raw) = post_raw(app, body).await;
    let json: Value = serde_json::from_str(&raw).expect("json");
    (status, json)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_valid_reparent_commits_over_http() {
    let w = world("valid").await;
    let (status, applied) =
        post(&w.app, json!({"slug": w.slug, "departmentId": "quant", "newParentId": "it"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(applied["departmentId"], json!("quant"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expected_seq_is_rejected_instead_of_creating_a_stale_retry_contract() {
    let w = world("no-seq").await;
    let (status, body) = post_raw(
        &w.app,
        json!({"slug": w.slug, "departmentId": "quant", "newParentId": "it", "expectedSeq": 1}),
    )
    .await;
    // Axum classifies body-deserialization rejections as 422. The status is
    // less important than the contract: legacy CAS input cannot be accepted
    // or silently ignored by a revisionless operation.
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("expectedSeq"), "unexpected rejection body: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reparenting_the_root_is_a_422_exec_root_protected() {
    let w = world("execroot").await;
    let (status, refusal) =
        post(&w.app, json!({"slug": w.slug, "departmentId": "executive", "newParentId": "it"}))
            .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refusal["code"], json!("exec-root-protected"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_department_is_a_422() {
    let w = world("unknown").await;
    let (status, refusal) =
        post(&w.app, json!({"slug": w.slug, "departmentId": "ghost", "newParentId": "it"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refusal["code"], json!("unknown-department"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cycle_is_a_422_would_create_cycle() {
    let w = world("cycle").await;
    // it -> it is a self-parent cycle.
    let (status, refusal) =
        post(&w.app, json!({"slug": w.slug, "departmentId": "it", "newParentId": "it"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refusal["code"], json!("would-create-cycle"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_existing_parent_is_a_422_instead_of_a_no_op_audit_write() {
    let w = world("already-parented").await;
    // northstar seeds quant directly below executive.
    let (status, refusal) =
        post(&w.app, json!({"slug": w.slug, "departmentId": "quant", "newParentId": "executive"}))
            .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refusal["code"], json!("already-under-parent"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_slug_is_isolated_404() {
    let w = world("foreign").await;
    let (status, refusal) =
        post(&w.app, json!({"slug": "someone-else", "departmentId": "quant", "newParentId": "it"}))
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(refusal["code"], json!("unknown-company"));
}
