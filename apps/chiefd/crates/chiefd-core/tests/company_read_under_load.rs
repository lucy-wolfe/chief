//! What a company read COSTS on a real company, when many are asked at once.
//!
//! # Why this file exists
//!
//! Because the bench that certified `aa3569355` measured an EMPTY fixture, one
//! request at a time, and reported 13.93ms -> 3.05ms while the operator's box
//! went the other way. Two things were wrong with it and only one of them was
//! the fixture:
//!
//! 1. **The load pattern.** Fifty strictly sequential requests cannot tell a
//!    one-at-a-time server from a perfectly parallel one. The operator's box was
//!    measured at a peak of 104 requests in flight.
//! 2. **The company.** A fixture with no accumulated mailbox or effect history
//!    makes every read cheap enough that a serialized path still looks fine.
//!
//! `org_runtime_desired_cost` now covers (1) with an assertion that holds on any
//! fixture. This file covers (2), and it cannot be a fixture at all — a
//! "realistic company" is an actual company database, so this reads one from
//! disk and is `#[ignore]`d without it.
//!
//! # Running it
//!
//! Point it at a copy of a real company database. **A COPY** — it is opened
//! read-write, because that is how the daemon opens it, and pointing this at a
//! live company would put a second writer on a database that permits exactly
//! one.
//!
//! ```text
//! CHIEF_REAL_COMPANY_DB=/path/to/copy/of/chief.db \
//!   cargo test -p chiefd-core --test company_read_under_load -- --ignored --nocapture
//! ```
//!
//! It prints the distribution at several concurrency levels rather than
//! asserting a millisecond budget: the number depends on the company, and a
//! budget that depends on somebody's private database is not a gate anyone else
//! can run. The GATE is the shape assertion below, which holds on any company.

#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SystemClock;

/// The environment variable naming the company database to measure.
const DB_ENV: &str = "CHIEF_REAL_COMPANY_DB";

/// The company slug inside that database.
///
/// It is NOT optional and it is not guessable: every row a read looks up is
/// keyed by slug, so a wrong one finds nothing and every read returns `None` in
/// microseconds. That failure looks exactly like a very fast company, which is
/// the same class of lie this whole file exists to stop — so the test asserts it
/// actually read a manifest before it believes any timing.
const SLUG_ENV: &str = "CHIEF_REAL_COMPANY_SLUG";

/// Concurrency levels to report. The top of the range is the operator's own
/// measured peak, rounded down to a power of two.
const LEVELS: [usize; 4] = [1, 4, 16, 64];

struct Stats {
    median: Duration,
    p95: Duration,
    max: Duration,
}

fn summarize(mut samples: Vec<Duration>) -> Stats {
    samples.sort_unstable();
    Stats {
        median: samples[samples.len() / 2],
        p95: samples[samples.len() * 95 / 100],
        max: samples[samples.len() - 1],
    }
}

/// One pass at `concurrency`, returning every request's latency.
async fn measure(db: &Arc<CompanyDb>, concurrency: usize, rounds: usize) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(concurrency * rounds);
    for _ in 0..rounds {
        let mut tasks = Vec::with_capacity(concurrency);
        for _ in 0..concurrency {
            let db = Arc::clone(db);
            tasks.push(tokio::spawn(async move {
                let started = Instant::now();
                // The read `/v1/org/runtime/desired` and `/v1/org/activity/read`
                // both sit on, and the most expensive one the daemon serves: it
                // reconstructs the whole manifest, which is `3 + 4N` statements
                // including a `staffing_history` scan per person.
                let out = db.org_manifest_and_activity_read().await;
                // `Some` and not merely `Ok`: a wrong slug answers `Ok(None)`
                // instantly, and timing that is measuring nothing.
                (matches!(out, Ok(Some(_))), started.elapsed())
            }));
        }
        for task in tasks {
            let (found, took) = task.await.expect("join read");
            assert!(
                found,
                "the read found no manifest: {SLUG_ENV} does not name a company in this \
                 database, so every timing here would be the cost of answering `None`"
            );
            samples.push(took);
        }
    }
    samples
}

/// **CONCURRENCY MUST NOT BECOME LATENCY, ON A COMPANY WITH REAL HISTORY.**
///
/// This is the measurement the shipping decision needed and did not have. The
/// assertion is deliberately the same SHAPE one as the route bench uses, not a
/// millisecond budget: what must hold on every company, of every size, is that
/// the cost of one read does not grow in proportion to how many other reads are
/// in flight. A serialized read path fails that at any company size; a parallel
/// one passes it at any company size.
///
/// The printed table is the actual deliverable for a human — it is what tells
/// you whether a real company's reads are 3ms or 300ms, which no assertion
/// should hard-code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a COPY of a real company database in CHIEF_REAL_COMPANY_DB"]
async fn company_reads_do_not_slow_down_when_many_are_asked_at_once() {
    let Ok(path) = std::env::var(DB_ENV) else {
        panic!("set {DB_ENV} to a COPY of a real company database");
    };
    let path = std::path::PathBuf::from(path);
    assert!(path.exists(), "{} does not exist", path.display());
    let Ok(slug) = std::env::var(SLUG_ENV) else {
        panic!("set {SLUG_ENV} to the company slug inside that database");
    };

    // The label IS the slug every read keys on, so this is not cosmetic.
    let db = Arc::new(
        CompanyDb::open(&slug, &path, Arc::new(SystemClock::default()))
            .expect("open the company database"),
    );

    // Warm: the first reads pay for lazily-prepared statements and the page
    // cache. The daemon's steady state is warm, so that is what to measure.
    let _ = measure(&db, 1, 5).await;

    let mut baseline = Duration::ZERO;
    println!("\n  concurrency        median           p95           max");
    for level in LEVELS {
        // Fewer rounds as concurrency grows: the sample count is what matters
        // and it is already `level * rounds`.
        let rounds = (64 / level).max(2);
        let stats = summarize(measure(&db, level, rounds).await);
        println!("  {level:>11}  {:>12?}  {:>12?}  {:>12?}", stats.median, stats.p95, stats.max);
        if level == 1 {
            baseline = stats.median;
        } else if level == 16 {
            // Sixteen requests on four worker threads cannot beat 4x however
            // parallel the storage is; a serialized path lands at 16x or worse.
            // Ten sits between those, with the wide side facing the flaky
            // direction.
            let ceiling =
                baseline.checked_mul(10).unwrap_or(Duration::MAX).max(Duration::from_millis(20));
            assert!(
                stats.median < ceiling,
                "asking this company for 16 reads at once made each one {:?}, against {:?} \
                 asked one at a time. Concurrency is turning into queue latency, which is \
                 the defect that took the operator's box to a 6.8s median.",
                stats.median,
                baseline
            );
        }
    }
    println!();
}
