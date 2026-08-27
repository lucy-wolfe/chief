//! HTTP contract for Rust-owned bench lifecycle completion acknowledgement.

#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{
    router_with_supervision_live, BenchCompletionRegistry, ChangeFeed, DocStore,
    SupervisionLiveSource,
};
use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::clock::SystemClock;
use chiefd_core::store::supervision::{CycleInput, RuntimeAuditObservation};
use chiefd_core::store::{activity, organization, supervision, COMPANY_DB_FILENAME};
use chiefd_core::test_support::northstar_manifest;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const SLUG: &str = "northstar@bench-http";
const PERSON: &str = "signal-researcher";

struct Fixture {
    _dir: tempfile::TempDir,
    app: axum::Router,
    completion: Option<Arc<BenchCompletionRegistry>>,
}

async fn fixture(with_completion: bool) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(COMPANY_DB_FILENAME);
    let company = Arc::new(
        CompanyDb::open("northstar-conformance", &path, Arc::new(SystemClock::default()))
            .expect("open company"),
    );
    company
        .mutate(MutationClass::Normal, MutationName("test.seed"), move |ledgers| {
            let manifest = northstar_manifest(1_785_542_400_000);
            organization::create(ledgers, &manifest)?;
            supervision::seed(ledgers, &manifest)?;
            activity::seed(ledgers, &manifest)?;
            Ok(())
        })
        .await
        .expect("seed normalized company");
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&path.display().to_string(), 2, feed).expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let mut live = SupervisionLiveSource::new(company, SLUG.to_string())
        .with_reconcile_trigger(Arc::new(tokio::sync::Notify::new()));
    let completion = with_completion.then(|| Arc::new(BenchCompletionRegistry::default()));
    if let Some(registry) = completion.as_ref() {
        live = live.with_bench_completion(Arc::clone(registry));
    }
    let app =
        router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live))
            .layer(axum::extract::Extension(ceo_caller()));
    Fixture { _dir: dir, app, completion }
}

/// The credential every request in this file carries: northstar's CEO, as a
/// PERSON, scoped by the COMPOSITE document key [`SLUG`].
///
/// The CEO because these are CONTRACT tests — strict request shape, the typed
/// 503 when no completion owner is live, and the post-commit tagged-absence
/// retry — and the CEO heads the root, so no authority refusal can stand in
/// front of the status code under test.
///
/// It is not optional: with the absent-caller arm deleted, a request with no
/// identity is 401 before the handler runs, which is exactly how the strict
/// request-shape assertion below started reading 401 instead of 422.
fn ceo_caller() -> chiefd_api::authn::middleware::CallerIdentity {
    chiefd_api::authn::middleware::CallerIdentity(chiefd_core::store::identities::Identity {
        identity_id: "id-ceo".to_owned(),
        principal: "chief".to_owned(),
        kind: chiefd_core::store::identities::IdentityKind::Person,
        company_slug: Some(SLUG.to_owned()),
        pubkey: Some("test-key".to_owned()),
        fingerprint: "fp-ceo".to_owned(),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

async fn post(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/org/person/bench-lifecycle")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn success_waits_for_post_commit_tagged_absence_and_retry_is_422() {
    let fixture = fixture(true).await;
    let registry = fixture.completion.as_ref().expect("completion registry");
    let app = fixture.app.clone();
    let request =
        tokio::spawn(async move { post(app, json!({"slug": SLUG, "personId": PERSON})).await });

    tokio::time::timeout(Duration::from_secs(5), async {
        while !registry.has_pending() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the committed operation registers before the server timeout");
    registry.observe(&CycleInput {
        audit: RuntimeAuditObservation::default(),
        ..CycleInput::default()
    });

    let (status, body) = request.await.expect("request task");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"applied": true, "structuralChanged": true, "handoff": "completed"}),
        "the public response stays structural and exposes no operation or topology data"
    );

    let (status, body) = post(fixture.app, json!({"slug": SLUG, "personId": PERSON})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], json!("already-benched"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_committed_bench_without_a_live_completion_owner_returns_typed_503() {
    let fixture = fixture(false).await;
    let (status, body) = post(fixture.app, json!({"slug": SLUG, "personId": PERSON})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], json!("bench-convergence-timeout"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_shape_remains_strict() {
    let fixture = fixture(true).await;
    let (status, _body) =
        post(fixture.app, json!({"slug": SLUG, "personId": PERSON, "socket": "forbidden"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
