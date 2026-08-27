//! Regression test for #376: countdown hits due ("👤due"/"🎯due" on the
//! footer) but no "checking on my goals"/"checked in" fire-card ever renders.
//!
//! Authored by eng-repro as a failing repro against pre-fix `main`, then
//! flipped here (per TESTING.md: a bug fix keeps its regression test, updated
//! rather than deleted) once the fix landed.
//!
//! Root cause (audit-loops): every supervision duty — including Duty #5
//! `run_deadline_evaluation` in `chiefd/src/run.rs` (the goal-check/
//! people-check countdown sweep, via `supervision::evaluate_due_work`/
//! `evaluate_check_ins`) — commits through `chiefd_core::actor::CompanyDb::mutate`,
//! which writes straight into the `documents` table on the writer thread's own
//! `rusqlite::Connection` and NEVER touched `chiefd-api`'s `DocStore`/
//! `ChangeFeed`. `ChangeFeed` is what backs `GET /v1/docs/watch` (SSE), which
//! is what `team-ui.ts`'s `SseWatcher` subscribes to for fire-cards. So a
//! supervision-duty write produced NO SSE event -> no card, even though the
//! ledger genuinely advanced (confirmed by reading the write straight back off
//! `CompanyDb`).
//!
//! The fix (#376): `CompanyDb::set_change_feed_sink` installs a hook
//! `writer.rs`'s `run_job` calls once per changed/removed store right after a
//! commit lands. `chiefd`'s `run_company` wires this to the SAME
//! `Arc<ChangeFeed>` the mounted `DocStore` publishes from
//! (`DocStore::open_with_feed` / `docstore::bind_with_feed`) — this test
//! reproduces that exact production wiring by hand: one `Arc<ChangeFeed>`,
//! shared between a `CompanyDb` (via `set_change_feed_sink`) and a `DocStore`
//! (via `open_with_feed`), both opened against the SAME physical sqlite file
//! per `chiefd/src/run.rs`'s "ONE store, not two" doc comment on
//! `resolve_company_db_path`.
//!
//! It performs a real `CompanyDb::mutate` the way `run_deadline_evaluation`
//! does, and proves it now IS visible on a live `/v1/docs/watch` subscription
//! — the same stream a `chiefd-api` `DocStore` write to the same file was
//! always visible on. That contrast (both now fire) is the #376 fix proven in
//! one file.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{router, ChangeFeed, DocStore};
use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::test_support::ManualClock;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Read the next SSE frame within `timeout`, or `None` if none arrives.
async fn next_frame_within(body: &mut Body, timeout: Duration) -> Option<String> {
    match tokio::time::timeout(timeout, body.frame()).await {
        Err(_) => None,   // timed out waiting: no frame arrived
        Ok(None) => None, // stream ended
        Ok(Some(frame)) => {
            let bytes = frame.expect("frame ok").into_data().expect("data frame, not trailers");
            Some(String::from_utf8(bytes.to_vec()).expect("utf8"))
        }
    }
}

/// The credential these watch requests carry.
///
/// A SERVICE identity, daemon-scoped (`company_slug: None`) — the shape the
/// resident actuator authenticates as, and the only caller that may watch any
/// company. This file is about the SEAM (does a write reach the feed), not
/// about who may subscribe; the company fence on this route is pinned by
/// `watch_company_fence_http.rs`.
///
/// It is not optional: with the absent-caller arm deleted, a watch request with
/// no identity is 401 and the stream this file reads would never open.
fn caller() -> chiefd_api::authn::middleware::CallerIdentity {
    chiefd_api::authn::middleware::CallerIdentity(chiefd_core::store::identities::Identity {
        identity_id: "changefeed-seam-service".to_owned(),
        principal: "changefeed-seam-service".to_owned(),
        kind: chiefd_core::store::identities::IdentityKind::Service,
        company_slug: None,
        pubkey: Some("test-key".to_owned()),
        fingerprint: "fp-changefeed-seam-service".to_owned(),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

/// Open a live `/v1/docs/watch` SSE body exactly as `team-ui.ts`'s
/// `SseWatcher` does: `stores=*` is the documented debug wildcard for "every
/// store for this slug" (`router.rs` `parse_store_filter`), so this watches
/// both the store a `DocStore` write lands in and the one a `CompanyDb`
/// write's underlying table lands in.
async fn watch_body(store: &Arc<DocStore>, slug: &str) -> Body {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/v1/docs/watch?slug={slug}&stores=*"))
        .body(Body::empty())
        .expect("request");
    let response = router(Arc::clone(store), 256 * 1024 * 1024)
        .layer(axum::extract::Extension(caller()))
        .oneshot(request)
        .await
        .expect("route");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").expect("content-type header"),
        "text/event-stream"
    );
    response.into_body()
}

/// The production wiring (`chiefd`'s `run.rs::wire_change_feed`, in miniature):
/// a `CompanyDb`'s change-feed sink closes over the SAME `Arc<ChangeFeed>` a
/// `DocStore` was opened with, converting the sink's `WallMillis` into the
/// ISO-8601 string `ChangeFeed::publish` expects.
fn wire_change_feed(company: &CompanyDb, feed: Arc<ChangeFeed>) {
    company.set_change_feed_sink(Arc::new(
        move |slug: &str, store: &str, _body: &str, updated_at: &str, removed: bool| {
            // The sink hands over the caller-supplied ISO-8601 string directly
            // (run_job renders its WallMillis via to_iso8601 before the sink) —
            // the ChangeFeedSink contract is &str, not WallMillis.
            feed.publish(slug.to_string(), store.to_string(), updated_at.to_string(), removed);
        },
    ));
}

#[tokio::test]
async fn a_companydb_supervision_write_now_reaches_the_changefeed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("org.sqlite");
    let path_str = path.display().to_string();

    // Same physical file, two independent connections -- chiefd's real
    // production wiring (see the module doc + `run.rs::resolve_company_db_path`).
    // ONE feed, shared between them (see the module doc + `wire_change_feed`
    // above) -- also chiefd's real production wiring, post-#376.
    let clock = Arc::new(ManualClock::default());
    let company = Arc::new(CompanyDb::open("co@abc", &path, clock).expect("open company db"));
    let feed = Arc::new(ChangeFeed::new());
    wire_change_feed(&company, Arc::clone(&feed));
    let docstore = Arc::new(DocStore::open_with_feed(&path_str, 4, feed).expect("open docstore"));
    docstore.ensure_schema().await.expect("schema");

    // Subscribe to the SSE surface BEFORE the write -- exactly what
    // team-ui.ts's SseWatcher does on a live pane -- so a delivered event
    // cannot be missed by test timing.
    let mut body = watch_body(&docstore, "co@abc").await;

    // A supervision-duty-style write through the ONLY path
    // `run_deadline_evaluation` (Duty #5 -- the goal-check/people-check
    // countdown sweep, `chiefd/src/run.rs:590-609`) uses: `CompanyDb::mutate`
    // over the transient in-memory projection, which the actor persists into
    // normalized supervision rows.
    //
    // BLOB-DEATH (N8): supervision's meta half is rows-authoritative, so the
    // committed body must decode as a real `SupervisionLedger` (an arbitrary
    // `{"checked_in":true}` fixture, valid before the dispatch existed, now
    // fails the commit) -- a real ledger body changes nothing about what this
    // test measures (the SSE doc-change frame), only what a valid commit body
    // is.
    let seam_manifest = chiefd_core::test_support::northstar_manifest(1_784_116_800_000);
    let seam_body =
        serde_json::to_string(&chiefd_core::store::supervision::SupervisionLedger::initial(
            &seam_manifest,
            "2026-07-26T00:00:00.000Z",
        ))
        .expect("serialize ledger");
    let seam_body_for_read = seam_body.clone();
    company
        .mutate(MutationClass::Normal, MutationName("duty.deadline_evaluation"), move |l| {
            l.put_document("supervision", seam_body);
            Ok(())
        })
        .await
        .expect("supervision write commits");
    assert_eq!(
        company.read(|s| s.document_body("supervision").map(str::to_string)),
        Some(seam_body_for_read),
        "the committed row reads back from CompanyDb -- the backend check really fired"
    );

    // THE FIX: an SSE frame now arrives for it, promptly -- #376's whole
    // point. Before the fix this timed out (`frame.is_none()`); the assertion
    // below is the inverted, post-fix form of that same check.
    let frame = next_frame_within(&mut body, Duration::from_millis(500)).await.expect(
        "#376: a CompanyDb write must now produce a real SSE frame -- if this fails, the \
             change-feed sink either wasn't installed or didn't fire",
    );
    assert!(frame.contains("doc-change"), "expected a doc-change SSE event, got: {frame}");
    assert!(
        frame.contains("\"store\":\"supervision\""),
        "expected the supervision store, got: {frame}"
    );

    // Contrast: any typed writer can publish another committed-row hint onto
    // the SAME live stream. Raw generic document writes no longer exist.
    docstore.feed().publish("co@abc", "activity", "t0", false);
    let frame = next_frame_within(&mut body, Duration::from_secs(5))
        .await
        .expect("a DocStore write on the same file must still publish an SSE frame too");
    assert!(frame.contains("doc-change"), "expected a doc-change SSE event, got: {frame}");
}
