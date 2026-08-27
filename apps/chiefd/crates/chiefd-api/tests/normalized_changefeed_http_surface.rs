//! Real SSE coverage for normalized-row commits.
//!
//! Raw generic document mutations are gone, but `/v1/docs/watch` remains the
//! wake-hint transport. These tests prove that normalized mailbox and operator
//! escalation writes publish onto the same feed consumed by the real HTTP
//! route, preserving reactive wakeups without an `org_documents` mirror.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{router, ChangeFeed, DocStore};
use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SystemClock;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// The company's KEY — `sha256(canonical <dir>)[..12]` — which is what labels
/// the actor and scopes every row and every feed event.
const COMPANY_KEY: &str = "c84afac7d8ad";
/// The company's DISPLAY slug: what genesis named it, and what every derived
/// `organization` field on a document means. It is NOT the key, and on a real
/// company it never is.
const COMPANY_SLUG: &str = "cobalt";

struct World {
    _dir: tempfile::TempDir,
    company: CompanyDb,
    store: Arc<DocStore>,
}

async fn world() -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chief.db");
    let feed = Arc::new(ChangeFeed::new());
    // Genesis before the actor opens: a document's derived `organization` is
    // the company's display name, so a company nothing has named cannot have a
    // document published for it at all.
    {
        let mut conn = chiefd_core::store::open_company_db(&path).expect("create company db");
        let tx = conn.transaction().expect("genesis txn");
        let mut manifest = chiefd_core::test_support::northstar_manifest(1_700_000_000_000);
        manifest.slug = COMPANY_SLUG.to_owned();
        chiefd_core::store::organization_rows::genesis(&tx, COMPANY_KEY, &manifest)
            .expect("genesis names the company");
        tx.commit().expect("commit genesis");
    }
    let company = CompanyDb::open(COMPANY_KEY, &path, Arc::new(SystemClock::default()))
        .expect("open company db");
    company.set_change_feed_sink(Arc::new({
        let feed = Arc::clone(&feed);
        move |slug: &str, store: &str, _body: &str, updated_at: &str, removed: bool| {
            feed.publish(slug, store, updated_at, removed);
        }
    }));
    let store = Arc::new(
        DocStore::open_with_feed(&path.display().to_string(), 2, feed).expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    World { _dir: dir, company, store }
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
        identity_id: "normalized-changefeed-service".to_owned(),
        principal: "normalized-changefeed-service".to_owned(),
        kind: chiefd_core::store::identities::IdentityKind::Service,
        company_slug: None,
        pubkey: Some("test-key".to_owned()),
        fingerprint: "fp-normalized-changefeed-service".to_owned(),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

async fn open_watch(store: Arc<DocStore>) -> Body {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/v1/docs/watch?slug={COMPANY_KEY}&stores=*"))
        .body(Body::empty())
        .expect("request");
    let response = router(store, 256 * 1024 * 1024)
        .layer(axum::extract::Extension(caller()))
        .oneshot(request)
        .await
        .expect("watch route");
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body()
}

async fn next_event(body: &mut Body) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
        .await
        .expect("SSE frame within five seconds")
        .expect("stream remains open")
        .expect("body frame");
    let bytes: Bytes = frame.into_data().expect("data frame");
    let wire = String::from_utf8(bytes.to_vec()).expect("utf8 SSE");
    let data = wire
        .lines()
        .find_map(|line| line.strip_prefix("data:").map(str::trim))
        .unwrap_or_else(|| panic!("missing SSE data line: {wire}"));
    serde_json::from_str(data).expect("SSE data JSON")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mailbox_row_delta_reaches_the_watch_route() {
    let world = world().await;
    let mut body = open_watch(Arc::clone(&world.store)).await;
    let entry: chiefd_core::store::mailbox_rows::MailboxEntry = serde_json::from_value(json!({
        "schemaVersion": 1,
        "id": "env-1",
        "organization": COMPANY_SLUG,
        "fromPersonId": "launcher",
        "to": "alice",
        "recipients": ["alice"],
        "body": "you have work",
        "urgency": "normal",
        "createdAt": "2026-07-25T00:00:00.000Z",
        "person": "alice",
        "state": "pending",
        "updatedAt": 1_700_000_000_000_i64
    }))
    .expect("valid mailbox entry");

    world
        .company
        .mailbox_delta(
            "alice".to_string(),
            vec![entry],
            vec![],
            "2026-07-25T00:00:00.000Z".to_string(),
            // Unauthenticated harness: an actor naming no person row is unjudged.
            String::new(),
        )
        .await
        .expect("mailbox delta");

    let event = next_event(&mut body).await;
    assert_eq!(event["slug"], COMPANY_KEY);
    assert_eq!(event["store"], "mailbox/alice");
    assert_eq!(event["removed"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runtime_publish_reaches_the_watch_route() {
    // The `/v1/docs/watch?stores=runtime` promise: a committed change to the
    // runtime row emits exactly one frame, and a re-read at that frame's seq
    // matches the write that woke the subscriber.
    //
    // It was written against `runtime_publish_observation` -- chiefd's converge
    // cycle committing what it had SEEN. That writer is deleted with the
    // observation it carried, and the two OBSERVED maps it wrote are now
    // refused on decode outright, so this drives the writer that remains. The
    // property under test is the feed seam and is unchanged; what changed is
    // that the content crossing it is chiefd's own state rather than a report
    // about a host.
    let world = world().await;

    let genesis = chiefd_core::store::runtime_rows::RuntimeState {
        version: 1,
        organization: None,
        observed_at: "2026-08-04T00:00:00.000Z".into(),
        session: None,
        socket_name: "sock".into(),
        status: "starting".into(),
        startup_admission_until: None,
        recovery_fingerprint: None,
        recovery_observed_at: None,
        recovery_confirmed: None,
        recovery: None,
        reconciliation: None,
        process_handles: std::collections::BTreeMap::new(),
        monitor_warnings: vec![],
        missing_durable_person_ids: vec![],
        unexpected_observed_person_ids: vec![],
        extra: std::collections::BTreeMap::new(),
    };
    world.company.runtime_publish(genesis.clone()).await.expect("genesis runtime publish");

    // The watch route opens AFTER genesis, so the frame asserted below is the
    // second publish alone.
    let mut body = open_watch(Arc::clone(&world.store)).await;

    let changed = chiefd_core::store::runtime_rows::RuntimeState {
        observed_at: "2026-08-04T00:05:00.000Z".into(),
        status: "running".into(),
        monitor_warnings: vec!["the company came up".to_string()],
        ..genesis
    };
    let seq = world.company.runtime_publish(changed).await.expect("runtime publish");
    assert!(seq > 0, "a changed runtime row must commit");

    let event = next_event(&mut body).await;
    assert_eq!(event["slug"], COMPANY_KEY);
    assert_eq!(event["store"], "runtime");
    assert_eq!(event["removed"], false);

    let (runtime, read_seq) =
        world.company.runtime_read().await.expect("runtime read").expect("runtime row exists");
    assert_eq!(read_seq, seq, "the frame's seq matches the re-read document's cursor");
    assert_eq!(runtime.status, "running");
    assert_eq!(runtime.monitor_warnings, vec!["the company came up".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operator_escalation_publish_reaches_the_watch_route() {
    let world = world().await;
    let mut body = open_watch(Arc::clone(&world.store)).await;
    let document: chiefd_core::store::operator_escalation_intents_rows::OperatorEscalationIntents =
        serde_json::from_value(json!({
            "schemaVersion": 1,
            "intents": {
                "fp-1": {
                    "schemaVersion": 1,
                    "fingerprint": "fp-1",
                    "organization": COMPANY_SLUG,
                    "personId": "alice",
                    "blocker": "needs operator input",
                    "operatorAction": "approve budget",
                    "queuedAt": "2026-07-25T00:00:00.000Z"
                }
            }
        }))
        .expect("valid escalation document");

    let seq = world
        .company
        .operator_escalation_intents_publish(document)
        .await
        .expect("escalation publish");
    // Not an absolute cursor any more: genesis names the company first and
    // stamps its own audit events, so what this asserts is that the publish
    // committed one — same claim the sibling runtime test makes.
    assert!(seq > 0, "an escalation publish must commit an audit cursor");

    let event = next_event(&mut body).await;
    assert_eq!(event["slug"], COMPANY_KEY);
    assert_eq!(event["store"], "operator-escalation-intents");
    assert_eq!(event["removed"], false);
}
