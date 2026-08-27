//! `GET /v1/docs/watch` (#259, SSE-B) exercised over its real HTTP route.
//!
//! The decision-level logic (filter/replay/dedup/gap/lag) is unit-tested
//! directly against `docstore::router`'s private `watch_outcomes` in that
//! module's own `#[cfg(test)] mod watch_tests` — that logic yields a plain
//! `WatchOutcome` enum, not axum's opaque `Event` (whose wire bytes are
//! produced by a private `finalize()` method, not visible outside the
//! `axum` crate). This file proves the actual bytes on the wire match the
//! documented contract: `id:`/`event:`/`data:` framing, the `Content-Type`,
//! heartbeat cadence, and behavior that genuinely needs concurrent real HTTP
//! requests (multiple watchers, a watcher nobody reads never blocking the
//! writer or the other watcher).

// In a `tests/` integration binary `cfg(test)` is not set, so the workspace's
// unwrap/expect/panic denies apply; here an `expect` IS the assertion.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{router, router_with_heartbeat_interval, DocStore};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// The credential every request in this file carries.
///
/// A SERVICE identity, daemon-scoped (`company_slug: None`) — the shape the
/// resident actuator authenticates as, and the one caller that may legitimately
/// watch any company. These are WIRE tests: they ask whether the SSE bytes
/// match the documented framing, so a company fence would only add a way for a
/// framing assertion to fail for a reason that is not about framing. The
/// caller fence on this route is pinned by `watch_company_fence_http.rs`.
///
/// It is not optional. There is no absent-caller arm any more — a request with
/// no identity is answered `401 caller-unauthenticated` before a handler runs,
/// which would make every stream in this file empty.
fn caller() -> chiefd_api::authn::middleware::CallerIdentity {
    chiefd_api::authn::middleware::CallerIdentity(chiefd_core::store::identities::Identity {
        identity_id: "watch-wire-service".to_owned(),
        principal: "watch-wire-service".to_owned(),
        kind: chiefd_core::store::identities::IdentityKind::Service,
        company_slug: None,
        pubkey: Some("test-key".to_owned()),
        fingerprint: "fp-watch-wire-service".to_owned(),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

/// The real router with that credential attached — the ONE place this file
/// builds a router, so no request in it can accidentally go out unauthenticated
/// and read as a wire-shape failure.
fn authed_router(store: &Arc<DocStore>) -> axum::Router {
    router(Arc::clone(store), 256 * 1024 * 1024).layer(axum::extract::Extension(caller()))
}

fn fresh_store() -> (tempfile::TempDir, Arc<DocStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("org.sqlite").display().to_string();
    let store = Arc::new(DocStore::open(&path, 4).expect("open"));
    (dir, store)
}

async fn ensure_schema(store: &Arc<DocStore>) {
    store.ensure_schema().await.expect("schema");
}

/// One parsed SSE frame's `id:`/`event:`/`data:` lines. A heartbeat comment
/// (`:hb\n\n`) parses to `id: None, event: None, data: None` — callers
/// distinguish it via `raw`.
#[derive(Debug)]
struct SseFrame {
    id: Option<String>,
    event: Option<String>,
    data: Option<Value>,
    raw: String,
}

fn parse_sse_frame(raw: &str) -> SseFrame {
    let mut id = None;
    let mut event = None;
    let mut data_lines = Vec::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("id:") {
            id = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim().to_string());
        }
    }
    let data = if data_lines.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&data_lines.join("\n")).expect("data is JSON"))
    };
    SseFrame { id, event, data, raw: raw.to_string() }
}

/// Read the next raw frame off an SSE body. Each axum `Event` (including a
/// `KeepAlive` comment) is exactly one `http_body` data frame — proven by
/// this test file actually parsing them, not assumed.
async fn next_frame(body: &mut Body) -> SseFrame {
    let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
        .await
        .expect("body produced a frame within 5s")
        .expect("stream ended early")
        .expect("frame ok");
    let bytes: Bytes = frame.into_data().expect("data frame, not trailers");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");
    parse_sse_frame(&text)
}

fn watch_uri(slug: &str, stores: &str, after: Option<u64>) -> String {
    let mut uri = format!("/v1/docs/watch?slug={slug}&stores={stores}");
    if let Some(after) = after {
        uri.push_str(&format!("&after={after}"));
    }
    uri
}

async fn watch(store: &Arc<DocStore>, slug: &str, stores: &str, after: Option<u64>) -> Body {
    watch_with_last_event_id(store, slug, stores, after, None).await
}

async fn watch_with_last_event_id(
    store: &Arc<DocStore>,
    slug: &str,
    stores: &str,
    after: Option<u64>,
    last_event_id: Option<&str>,
) -> Body {
    let mut builder = Request::builder().method("GET").uri(watch_uri(slug, stores, after));
    if let Some(id) = last_event_id {
        builder = builder.header("last-event-id", id);
    }
    let request = builder.body(Body::empty()).expect("request");
    let response = authed_router(store).oneshot(request).await.expect("route");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").expect("content-type header"),
        "text/event-stream"
    );
    response.into_body()
}

async fn call(store: &Arc<DocStore>, path: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = authed_router(store).oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn a_replayed_doc_change_matches_the_documented_wire_shape() {
    let (_dir, store) = fresh_store();
    ensure_schema(&store).await;
    store.feed().publish("co@abc", "activity", "t0", false);

    let mut body = watch(&store, "co@abc", "activity", None).await;
    let frame = next_frame(&mut body).await;

    assert_eq!(frame.event.as_deref(), Some("doc-change"));
    assert_eq!(frame.id.as_deref(), Some("1"), "id: is the seq");
    let data = frame.data.expect("data present");
    assert_eq!(
        data["seq"],
        json!(1),
        "seq is ALSO duplicated into data — see router.rs's module doc"
    );
    assert_eq!(data["slug"], json!("co@abc"));
    assert_eq!(data["store"], json!("activity"));
    assert!(
        data.get("updatedAt").is_none(),
        "field names are snake_case, not camelCase — a deliberate exception to this router's usual convention (see the module doc)"
    );
    assert_eq!(data["updated_at"], json!("t0"));
    assert_eq!(data["removed"], json!(false));
}

#[tokio::test]
async fn filtering_excludes_other_slugs_and_unsubscribed_stores() {
    let (_dir, store) = fresh_store();
    ensure_schema(&store).await;
    store.feed().publish("co@abc", "supervision", "t", false);
    store.feed().publish("co@xyz", "activity", "t", false);
    store.feed().publish("co@abc", "activity", "t1", false);

    let mut body = watch(&store, "co@abc", "activity", None).await;
    let frame = next_frame(&mut body).await;
    let data = frame.data.expect("data present");
    assert_eq!(data["slug"], json!("co@abc"));
    assert_eq!(data["store"], json!("activity"));
    assert_eq!(
        data["updated_at"],
        json!("t1"),
        "the only matching event, not the noise from another slug/store"
    );
}

#[tokio::test]
async fn last_event_id_header_wins_over_after_query_and_replays_only_whats_newer() {
    let (_dir, store) = fresh_store();
    ensure_schema(&store).await;
    store.feed().publish("co@abc", "activity", "t0", false);
    store.feed().publish("co@abc", "activity", "t1", false);

    // Header says "I've seen through seq=1"; a conflicting `after=0` query
    // param (which alone would replay both) must lose.
    let mut body = watch_with_last_event_id(&store, "co@abc", "activity", Some(0), Some("1")).await;
    let frame = next_frame(&mut body).await;
    assert_eq!(
        frame.id.as_deref(),
        Some("2"),
        "only the newer (seq=2) event replays — the header won"
    );
}

#[tokio::test]
async fn an_after_ahead_of_the_servers_own_counter_yields_reorg_then_resumes_live() {
    let (_dir, store) = fresh_store();
    ensure_schema(&store).await;

    // Nothing published yet (server's own seq counter is 0) — `after=500`
    // can only be a stale Last-Event-ID from a prior process epoch.
    let mut body = watch(&store, "co@abc", "activity", Some(500)).await;
    let frame = next_frame(&mut body).await;
    assert_eq!(frame.event.as_deref(), Some("reorg"));
    assert_eq!(frame.id, None, "a reorg carries no seq of its own");
    assert_eq!(
        frame.data,
        Some(json!({})),
        "non-empty data — a wholly empty SSE data field never dispatches"
    );

    // The connection stays open: a live write afterward must still arrive.
    store.feed().publish("co@abc", "activity", "t0", false);
    let next = next_frame(&mut body).await;
    assert_eq!(next.event.as_deref(), Some("doc-change"), "resumed live after the reorg");
}

#[tokio::test]
async fn heartbeat_fires_on_schedule_during_quiet_state() {
    let (_dir, store) = fresh_store();
    ensure_schema(&store).await;

    let request = Request::builder()
        .method("GET")
        .uri(watch_uri("co@abc", "activity", None))
        .body(Body::empty())
        .expect("request");
    let response = router_with_heartbeat_interval(
        Arc::clone(&store),
        256 * 1024 * 1024,
        Duration::from_millis(30),
    )
    .layer(axum::extract::Extension(caller()))
    .oneshot(request)
    .await
    .expect("route");
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();

    // No data was published and nothing is queried — the ONLY thing that can
    // arrive during quiet state is the heartbeat comment.
    let frame = next_frame(&mut body).await;
    assert!(frame.raw.starts_with(':'), "a heartbeat is an SSE comment line, got: {:?}", frame.raw);
    assert!(frame.event.is_none());
    assert!(frame.data.is_none());
}

#[tokio::test]
async fn concurrent_watchers_each_receive_the_full_matching_stream() {
    let (_dir, store) = fresh_store();
    ensure_schema(&store).await;

    let mut watcher_a = watch(&store, "co@abc", "activity", None).await;
    let mut watcher_b = watch(&store, "co@abc", "activity", None).await;

    store.feed().publish("co@abc", "activity", "t0", false);

    let frame_a = next_frame(&mut watcher_a).await;
    let frame_b = next_frame(&mut watcher_b).await;
    assert_eq!(frame_a.data, frame_b.data, "both concurrent watchers see the same event");
    assert_eq!(frame_a.data.expect("data")["updated_at"], json!("t0"));
}

#[tokio::test]
async fn a_watcher_nobody_reads_never_blocks_the_writer_or_another_watcher() {
    let (_dir, store) = fresh_store();
    ensure_schema(&store).await;

    // Connect but NEVER read from this one — it just sits there, exactly the
    // "slow or dead consumer" the acceptance criteria call out. `stores=*`
    // (debug wildcard) since the writes below use several store names.
    let _idle_watcher = watch(&store, "co@abc", "*", None).await;
    let mut active_watcher = watch(&store, "co@abc", "*", None).await;

    // A handful of committed-mutation hints. Typed row writers publish through
    // this same feed after their transaction commits.
    for i in 0..5 {
        store.feed().publish("co@abc", format!("s{i}"), "t", false);
    }

    // The ACTIVE watcher (subscribed before any of the five writes) still
    // receives them all, each within a bounded timeout.
    for i in 0..5 {
        let frame = next_frame(&mut active_watcher).await;
        assert_eq!(frame.data.expect("data")["store"], json!(format!("s{i}")));
    }
}

#[tokio::test]
async fn existing_routes_are_unaffected_by_the_new_watch_route() {
    let (_dir, store) = fresh_store();
    let (status, _) = call(&store, "/v1/docs/ensure-schema", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    // #830: this used to re-exercise `/v1/locks/list` (deleted with the rest
    // of the TTL lease) as a convenient second existing route; the subject
    // here was never locks, only "a route besides /v1/docs/watch still
    // answers" — re-calling the idempotent ensure-schema route proves the
    // same thing without depending on a deleted subject.
    let (status, resp) = call(&store, "/v1/docs/ensure-schema", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp, json!({ "ok": true }));
}
