//! THE 126 MB `daemon.log`, pinned at the layer that wrote it.
//!
//! Measured on a live company: 670k lines in five hours, 653k of them
//! `event="docstore.request"` at INFO. The volume is the changefeed — a quiet
//! company polls its own daemon several times a second, for ever, and every
//! poll is a 200 in a millisecond — so the file was 97.5% "a routine request
//! succeeded quickly", and the refusals an operator opens it for were one line
//! in forty.
//!
//! These drive the REAL router with REAL requests and read the level off the
//! event, because the failure was never about a value's shape: the old code
//! logged the right fields, at one level, on every request. Only the level and
//! the request that produced it can tell the fix from the bug.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{router, DocStore};
use tower::ServiceExt;

/// One served request, as the log saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    level: tracing::Level,
    fields: String,
}

fn fresh_store() -> (tempfile::TempDir, Arc<DocStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("org.sqlite");
    let store = Arc::new(DocStore::open(&path.display().to_string(), 4).expect("open"));
    (dir, store)
}

/// Drive one request through the production router and answer its status plus
/// every `docstore.request` line the request logged.
async fn serve(
    store: &Arc<DocStore>,
    method: &str,
    path: &str,
    body: Body,
) -> (StatusCode, Vec<Line>) {
    let log: Arc<Mutex<Vec<Line>>> = Arc::new(Mutex::new(Vec::new()));
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(body)
        .expect("request");
    let status = {
        let _guard = tracing::subscriber::set_default(CapturingSubscriber(Arc::clone(&log)));
        // The interest cache is global and remembers that an earlier
        // subscriber refused a callsite; without this a DEBUG event that
        // another test's subscriber declined stays declined here.
        tracing::callsite::rebuild_interest_cache();
        router(Arc::clone(store), 256 * 1024 * 1024).oneshot(request).await.expect("route").status()
    };
    let lines = log.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    let requests =
        lines.into_iter().filter(|line| line.fields.contains("docstore.request")).collect();
    (status, requests)
}

/// A HEALTHY, FAST POLL IS DEBUG. `/v1/docs/health` is one of the paths this
/// daemon polls itself with, and this line was 653,000 of the 670,000: it says
/// a poll answered, which is what a poll does.
#[tokio::test(flavor = "current_thread")]
async fn a_fast_served_request_is_logged_at_debug() {
    let (_dir, store) = fresh_store();
    let (status, _) = serve(&store, "POST", "/v1/docs/ensure-schema", Body::from("{}")).await;
    assert_eq!(status, StatusCode::OK, "the fixture must reach a ready store");
    let (status, lines) = serve(&store, "GET", "/v1/docs/health", Body::empty()).await;
    assert_eq!(status, StatusCode::OK, "a ready store answers 200 — the ordinary poll");
    let line = lines.first().expect("the request must still be logged, at some level");
    assert_eq!(
        line.level,
        tracing::Level::DEBUG,
        "a fast, non-refused request is DEBUG — at INFO it is 97.5% of daemon.log: {line:?}"
    );
    assert!(line.fields.contains("status=200"), "{line:?}");
    assert!(line.fields.contains("elapsed_ms"), "{line:?}");
    assert!(
        !line.fields.contains("/v1/docs/health?"),
        "the PATH and never the query string: {line:?}"
    );
}

/// A MUTATION KEEPS ITS LINE, and the line does not call it slow.
///
/// The wake POST is what one operator click produces, and `TEST_SUITE.md`
/// counts it to prove the click produced exactly one. Measured live on the
/// first run of this rule: the wake was back at INFO and read
/// `a docstore request was slow … elapsed_ms=10`, because two different reasons
/// reached INFO and the line only knew one of them.
#[tokio::test(flavor = "current_thread")]
async fn a_fast_mutation_is_logged_at_info_and_is_not_called_slow() {
    let (_dir, store) = fresh_store();
    // `ensure-schema` is a real 200 that is NOT one of the polls, which is the
    // shape a wake has on a live company: fast, successful, and news.
    let (status, lines) = serve(&store, "POST", "/v1/docs/ensure-schema", Body::from("{}")).await;
    assert_eq!(status, StatusCode::OK);
    let line = lines.first().expect("a mutation must be logged");
    assert_eq!(
        line.level,
        tracing::Level::INFO,
        "a state-changing request is never demoted: {line:?}"
    );
    assert!(
        !line.fields.contains("was slow"),
        "it answered in milliseconds; only the >= 1s arm may say slow: {line:?}"
    );
    assert!(line.fields.contains("was served"), "{line:?}");
}

/// A REFUSAL STAYS VISIBLE, and gains a level that says so. It was INFO —
/// indistinguishable from a successful poll — which is the half of this bug
/// that is about reading rather than about disk.
#[tokio::test(flavor = "current_thread")]
async fn a_refused_request_is_logged_at_warn() {
    let (_dir, store) = fresh_store();
    let (status, lines) = serve(&store, "POST", "/v1/docs/no-such-route", Body::from("{}")).await;
    assert!(status.is_client_error(), "the fixture must produce a 4xx, got {status}");
    let line = lines.first().expect("a refusal must be logged");
    assert_eq!(
        line.level,
        tracing::Level::WARN,
        "a refusal is the most operator-relevant thing this surface produces: {line:?}"
    );
}

/// The RULE, over the whole matrix, so no arm can be added or moved without a
/// test naming it. The three interesting rows are proven live above; this pins
/// the table they are rows of, including the slow-request row a unit test
/// cannot make a real router produce without sleeping a second in CI.
#[test]
fn the_level_table_keeps_every_line_an_operator_needs() {
    use chiefd_api::docstore::request_log_level as level;
    // THE POLLS — 98% of the file, and the only thing demoted.
    assert_eq!(level(200, 0, "/v1/org/activity/read"), tracing::Level::DEBUG);
    assert_eq!(level(200, 0, "/v1/docs/watch"), tracing::Level::DEBUG);
    assert_eq!(level(200, 0, "/v1/org/runtime/desired"), tracing::Level::DEBUG);
    assert_eq!(level(200, 0, "/v1/org/runtime/launch-catalog"), tracing::Level::DEBUG);
    assert_eq!(level(200, 0, "/v1/org/mailbox/read-person"), tracing::Level::DEBUG);
    assert_eq!(level(304, 12, "/v1/org/manifest/read"), tracing::Level::DEBUG);

    // A MUTATION IS NEVER DEMOTED. `/v1/org/person/wake` is the request one
    // operator click produces and the one the live suite COUNTS to prove that
    // click produced exactly one wake; a blanket demotion took it out of the
    // log and broke that measurement, which is how this rule earned its shape.
    assert_eq!(level(200, 0, "/v1/org/person/wake"), tracing::Level::INFO);
    assert_eq!(level(200, 0, "/v1/org/person/start"), tracing::Level::INFO);
    assert_eq!(level(200, 0, "/v1/org/department/add"), tracing::Level::INFO);
    // …and so is a route nobody has written yet: silence is the named
    // exception, so a new path is loud by default.
    assert_eq!(level(200, 0, "/v1/org/something/nobody/added/yet"), tracing::Level::INFO);

    assert_eq!(level(401, 1, "/v1/org/activity/read"), tracing::Level::WARN);
    assert_eq!(level(404, 1, "/v1/docs/watch"), tracing::Level::WARN);
    assert_eq!(level(500, 1, "/v1/org/activity/read"), tracing::Level::ERROR);
    assert_eq!(level(503, 1, "/v1/docs/health"), tracing::Level::ERROR);
    // SLOW IS NEWS WHATEVER IT ANSWERED. Genesis is one of these calls, and a
    // launch that stalls in it must not be demoted out of the file.
    assert_eq!(level(200, 1_000, "/v1/org/activity/read"), tracing::Level::INFO);
    assert_eq!(level(200, 30_000, "/v1/docs/watch"), tracing::Level::INFO);
    // …and a failure that is also slow keeps the LOUDER level, never the
    // slow one.
    assert_eq!(level(500, 30_000, "/v1/org/activity/read"), tracing::Level::ERROR);
    assert_eq!(level(403, 30_000, "/v1/docs/watch"), tracing::Level::WARN);
}

/// One minimal subscriber, capturing the LEVEL as well as the fields — the
/// level is the whole subject here.
struct CapturingSubscriber(Arc<Mutex<Vec<Line>>>);

struct FieldCapture(String);

impl tracing::field::Visit for FieldCapture {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.0, " {}={value:?}", field.name());
    }
}

impl tracing::Subscriber for CapturingSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut capture = FieldCapture(String::new());
        event.record(&mut capture);
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Line { level: *event.metadata().level(), fields: capture.0 });
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}
