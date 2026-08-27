//! The surviving chiefd infrastructure HTTP surface.
//!
//! Product state is available only through named `/v1/org/*` routes. This
//! regression locks the final SQL-normalization cutover: schema readiness does
//! not create `org_documents`, every generic state-bearing document route is
//! unmounted, and the non-state-bearing health operation still works.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{router, DocStore};
use serde_json::{json, Value};
use tower::ServiceExt;

fn fresh_store() -> (tempfile::TempDir, PathBuf, Arc<DocStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("org.sqlite");
    let store = Arc::new(DocStore::open(&path.display().to_string(), 4).expect("open"));
    (dir, path, store)
}

async fn call(store: &Arc<DocStore>, path: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response =
        router(Arc::clone(store), 256 * 1024 * 1024).oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn get(store: &Arc<DocStore>, path: &str) -> (StatusCode, Value) {
    let request = Request::builder().method("GET").uri(path).body(Body::empty()).expect("request");
    let response =
        router(Arc::clone(store), 256 * 1024 * 1024).oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    (status, serde_json::from_slice(&bytes).expect("json"))
}

#[tokio::test]
async fn health_becomes_ready_without_creating_org_documents() {
    let (_dir, path, store) = fresh_store();
    let (status, body) = get(&store, "/v1/docs/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["status"].as_str().unwrap().contains("schema-missing"));

    let (status, body) = call(&store, "/v1/docs/ensure-schema", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "ok": true }));

    let (status, body) = get(&store, "/v1/docs/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("ok"));

    let conn = chiefd_core::store::open_company_db_readonly(&path).expect("read-only db");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='org_documents'",
            [],
            |row| row.get(0),
        )
        .expect("schema census");
    assert_eq!(count, 0, "readiness must never recreate the retired blob table");
}

#[tokio::test]
async fn every_generic_document_route_is_unmounted() {
    let (_dir, _path, store) = fresh_store();
    call(&store, "/v1/docs/ensure-schema", json!({})).await;

    for path in [
        "/v1/docs/read",
        "/v1/docs/insert-if-absent",
        "/v1/docs/cas",
        "/v1/docs/drop-company",
        "/v1/docs/drop-company-store",
        "/v1/docs/prune-prefix",
        "/v1/docs/export-all",
        "/v1/docs/list-stores",
    ] {
        let (status, _) = call(&store, path, json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} must stay retired");
    }
}
