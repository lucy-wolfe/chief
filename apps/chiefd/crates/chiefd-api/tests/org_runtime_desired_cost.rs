//! What `/v1/org/runtime/desired` COSTS — the route the actuator polls.
//!
//! # Why this is a test file and not a `cargo bench`
//!
//! Because the number it produces is the acceptance criterion of
//! the design record Stage 2 (`desired` median <10ms), and the operator
//! has been told "should be faster" enough times today. A criterion nobody can
//! re-run on demand is a claim, not a measurement.
//!
//! Two things are measured, against the same fixture, in one run:
//!
//! - **`commit_seq`** — the honest witness for "a read pays write prices".
//!   `run_job` advances it once per COMMIT, so it counts write transactions and
//!   nothing else. Before this stage, one request advanced it by FIVE.
//! - **wall-clock latency**, reported as median / p95 / max over a warm loop.
//!
//! `#[ignore]` because it needs the real launcher checkout on disk (the route
//! refuses a launcher root with no `packages/piing/extensions`, and the catalog
//! walk it used to perform is exactly one of the costs under measurement). Run
//! it by name:
//!
//! ```text
//! cargo test -p chiefd-api --test org_runtime_desired_cost -- --ignored --nocapture
//! ```

#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{
    router_with_supervision_live, ChangeFeed, DocStore, SupervisionLiveSource,
};
use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::clock::SystemClock;
use chiefd_core::store::{activity, organization, supervision, COMPANY_DB_FILENAME};
use chiefd_core::test_support::northstar_manifest;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

const LABEL: &str = "northstar-conformance";
const SLUG: &str = "northstar@desired-cost";
const PATH: &str = "/v1/org/runtime/desired";

/// The repository root, which is also the launcher checkout this route reads.
///
/// Derived from the crate rather than from the working directory: `cargo test`
/// sets `CARGO_MANIFEST_DIR` to `apps/chiefd/crates/chiefd-api`, and the four
/// ancestors above it are the repo root.
fn launcher_root() -> std::path::PathBuf {
    let mut root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..4 {
        root.pop();
    }
    root
}

struct Fixture {
    _dir: tempfile::TempDir,
    app: axum::Router,
    company: Arc<CompanyDb>,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(COMPANY_DB_FILENAME);
    let company = Arc::new(
        CompanyDb::open(LABEL, &path, Arc::new(SystemClock::default())).expect("open company"),
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
    let live = SupervisionLiveSource::new(Arc::clone(&company), SLUG.to_owned())
        .with_reconcile_actuator_config(chiefd_host::converge_apply::ActuatorConfig {
            socket: "chiefd-desired-cost".to_owned(),
            watching_since: "1970-01-01T00:00:00.000Z".to_owned(),
            dir: dir.path().to_path_buf(),
            home: dir.path().to_path_buf(),
            pi_binary: dir.path().join("pi"),
            floor: Duration::ZERO,
            launcher_root: launcher_root(),
            root_pi_agent_dir: dir.path().join("pi-agent"),
        });
    let app =
        router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live))
            .layer(axum::extract::Extension(actuator_caller()));
    Fixture { _dir: dir, app, company }
}

/// The resident actuator's own credential — this route's real caller.
fn actuator_caller() -> chiefd_api::authn::middleware::CallerIdentity {
    chiefd_api::authn::middleware::CallerIdentity(chiefd_core::store::identities::Identity {
        identity_id: "chiefd-actuator".to_owned(),
        principal: "chiefd-actuator".to_owned(),
        kind: chiefd_core::store::identities::IdentityKind::Service,
        company_slug: None,
        pubkey: Some("test-key".to_owned()),
        fingerprint: "fp-chiefd-actuator".to_owned(),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

async fn desired(app: &axum::Router) -> StatusCode {
    let request = Request::builder()
        .method("POST")
        .uri(PATH)
        .header("content-type", "application/json")
        .body(Body::from(json!({ "slug": SLUG }).to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    // Drain it: an undrained body is not a served request.
    let _ = response.into_body().collect().await.expect("body").to_bytes();
    status
}

/// A READ REQUEST COMMITS NOTHING. The rule, and the one assertion in this file
/// that runs unconditionally in CI would be vacuous without the launcher
/// checkout, so it lives with the measurement it explains.
///
/// `/v1/org/runtime/desired` used to make five `BEGIN IMMEDIATE` write
/// transactions per request — each cloning the company's whole ledger history,
/// validating it, diffing it twice and committing — to answer a question that
/// changes nothing. `commit_seq` counts exactly those commits.
#[tokio::test]
#[ignore = "needs the launcher checkout on disk; run with --ignored"]
async fn one_desired_request_commits_nothing_and_stays_inside_its_budget() {
    use chiefd_core::ledger::LedgerSnapshot;

    let f = fixture().await;

    // Warm: the first request pays for lazily-prepared statements and the OS
    // page cache, and the actuator polls this route continuously — the steady
    // state is what the operator lives in.
    for _ in 0..5 {
        assert_eq!(desired(&f.app).await, StatusCode::OK, "the route serves");
    }

    let commits_before = f.company.read(LedgerSnapshot::commit_seq);
    let mut samples: Vec<Duration> = Vec::with_capacity(50);
    for _ in 0..50 {
        let started = Instant::now();
        assert_eq!(desired(&f.app).await, StatusCode::OK, "the route serves");
        samples.push(started.elapsed());
    }
    let commits_after = f.company.read(LedgerSnapshot::commit_seq);

    samples.sort_unstable();
    let median = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    let max = samples[samples.len() - 1];
    println!(
        "desired-cost n=50 median={median:?} p95={p95:?} max={max:?} \
         commits_per_request={:.2}",
        f64::from(u32::try_from(commits_after - commits_before).unwrap_or(u32::MAX)) / 50.0
    );

    assert_eq!(
        commits_after,
        commits_before,
        "fifty reads committed {} write transactions; a question that changes nothing must \
         not take the database's write lock, clone the whole ledger history, validate it, \
         diff it twice and commit",
        commits_after - commits_before
    );
    assert!(
        median < Duration::from_millis(10),
        "the design record budgets this route at a 10ms median; it measured {median:?} \
         (p95 {p95:?}, max {max:?})"
    );
}

/// How many requests this route must tolerate IN FLIGHT AT ONCE.
///
/// Not a guess: the operator's box was measured at a peak of 104 concurrent
/// requests, because every rail refreshes on the same changefeed wake and each
/// refresh fires three reads at once. Sixteen is comfortably inside that and
/// still far outside what a one-at-a-time read path can absorb.
const IN_FLIGHT: usize = 16;

/// **CONCURRENCY MUST NOT BECOME LATENCY.** This is the arm the sequential
/// measurement above could not provide, and its absence is why a 3x live
/// regression shipped behind a green bench.
///
/// The test above issues fifty requests ONE AT A TIME. A server that can only
/// serve one read at a time is indistinguishable from a perfectly parallel one
/// under that load, so the number it printed (13.93ms -> 3.05ms) was true and
/// meaningless: it measured the cost of a read and never the cost of asking for
/// several. Live, the same build went to a 6,782ms median because reads still
/// ran one at a time on the writer thread while callers asked for 104 at once.
///
/// # Why the assertion is RELATIVE and not another millisecond budget
///
/// Because an absolute budget on a small fixture is exactly the lie that got
/// through: an empty company's read is fast enough that even a serialized
/// server can serve sixteen of them inside any round number a reviewer would
/// accept. What must hold on EVERY fixture, large or small, is the SHAPE — the
/// cost of a request must not scale with how many other requests are in flight.
/// A serialized path fails this at any company size, and a parallel one passes
/// it at any company size, so the assertion no longer depends on the fixture
/// being representative to be honest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the launcher checkout on disk; run with --ignored"]
async fn desired_does_not_slow_down_when_many_are_asked_at_once() {
    let f = fixture().await;
    for _ in 0..5 {
        assert_eq!(desired(&f.app).await, StatusCode::OK, "the route serves");
    }

    // The baseline: one at a time, which is what the old measurement reported.
    let mut serial: Vec<Duration> = Vec::with_capacity(20);
    for _ in 0..20 {
        let started = Instant::now();
        assert_eq!(desired(&f.app).await, StatusCode::OK, "the route serves");
        serial.push(started.elapsed());
    }
    serial.sort_unstable();
    let serial_median = serial[serial.len() / 2];

    // The same route, asked IN_FLIGHT times at once, five rounds of it.
    let mut concurrent: Vec<Duration> = Vec::with_capacity(IN_FLIGHT * 5);
    for _ in 0..5 {
        let mut tasks = Vec::with_capacity(IN_FLIGHT);
        for _ in 0..IN_FLIGHT {
            let app = f.app.clone();
            tasks.push(tokio::spawn(async move {
                let started = Instant::now();
                let status = desired(&app).await;
                (status, started.elapsed())
            }));
        }
        for task in tasks {
            let (status, took) = task.await.expect("join request");
            assert_eq!(status, StatusCode::OK, "the route serves under concurrency");
            concurrent.push(took);
        }
    }
    concurrent.sort_unstable();
    let concurrent_median = concurrent[concurrent.len() / 2];
    let concurrent_p95 = concurrent[concurrent.len() * 95 / 100];

    println!(
        "desired-concurrency in_flight={IN_FLIGHT} serial_median={serial_median:?} \
         concurrent_median={concurrent_median:?} concurrent_p95={concurrent_p95:?}"
    );

    // WHERE THE BAR COMES FROM. This runtime has four worker threads, so
    // sixteen requests that each need CPU cannot finish in less than four
    // serial times however parallel the storage is: `IN_FLIGHT / worker_threads`
    // is the floor, not a defect. Measured on the fixed path it lands at 3-6x,
    // and a fully serialized read path lands at IN_FLIGHT (16x) or worse.
    //
    // Ten is between those two facts, with the wide side facing the flaky
    // direction: a busy CI box makes the honest number drift up, so the bar
    // must sit well above the healthy range and still well below the broken
    // one. The absolute floor stops a fixture so fast that a ratio on
    // sub-millisecond numbers becomes noise.
    const CPU_FLOOR: u32 = 4; // IN_FLIGHT / worker_threads
    const HEADROOM: u32 = 25; // 2.5x, in tenths, to stay in integer arithmetic
    let ceiling = serial_median
        .checked_mul(CPU_FLOOR * HEADROOM / 10)
        .unwrap_or(Duration::MAX)
        .max(Duration::from_millis(20));
    assert!(
        concurrent_median < ceiling,
        "asking for {IN_FLIGHT} at once made each one {concurrent_median:?}, against \
         {serial_median:?} asked one at a time. Concurrency is turning into queue latency, \
         which means the read path is serialized somewhere — that is the defect that took \
         the operator's box to a 6.8s median while this file's sequential arm stayed green."
    );
}
