//! WHAT THE OPERATOR'S OWN WRITE WAITS FOR, on a company shaped like theirs.
//!
//! # The question
//!
//! the design record says the operator's click — `POST
//! /v1/org/person/wake`, a [`MutationClass::Normal`] job — "waits ~2s **by
//! scheduler design**", because the scheduler admits a lower-priority op past a
//! `Small` stream only after 32 consecutive `Small` ops *or* a 2-second aging
//! window. Stage 2.3 proposed buying that back with an interactive class.
//!
//! The premise had a load-bearing assumption: that there IS a saturating `Small`
//! stream in front of the wake. It was true when it was written — every read
//! route took the WRITE path, so five `Small` write transactions were enqueued
//! per `desired` request at several requests per second.
//!
//! Two commits removed that stream. `70ec7d376` moved reads onto a bounded pool
//! of `SQLITE_OPEN_READ_ONLY` connections, so a read no longer enters this queue
//! AT ALL (`actor/queue.rs`: *"This queue is the WRITE queue: reads do not enter
//! it"*), and `efe2aeb38` indexed the two `MAX(at)` reads. This file measures
//! what is left.
//!
//! # What is measured, and with which instrument
//!
//! [`CompanyDb::queue_snapshot`] already publishes, for the job the writer is
//! running RIGHT NOW, the wait it served before it started
//! ([`chiefd_core::actor::CurrentJob::enqueued_ms`], stamped under the queue
//! lock at admission). A sampler thread reads it, so enqueue → start is the
//! writer's own number and not an inference from the caller's total. The
//! caller's total gives enqueue → end; the difference is the job itself.
//!
//! # The company
//!
//! Not a fixture. An empty company cannot express this defect — every job on it
//! is so cheap that a queue wait cannot form — and a bench on one hid a live 3x
//! regression on 2026-08-15. [`realistic_company`] builds the operator's
//! measured shape: their 322,329 `org_events` rows, which lands the file at
//! ~119MB against their 97MB, so the company under measurement is the harder
//! one. Their mailbox and effect counts are not known, so those are planted at a
//! plausible scale rather than a measured one — they set what the per-write
//! whole-`Ledgers` clone in `run_job` costs, and leaving them at zero would
//! measure a wake that costs nothing. Note that this affects the wake's own JOB
//! cost only: its queue WAIT, the question this file exists to answer, is a
//! property of what else is in the queue.
//!
//! # Reading the numbers
//!
//! Debug build, so every absolute figure is pessimistic. The comparison between
//! the arms is the result; the absolute milliseconds are context.
//!
//! ```text
//! cargo test -p chiefd-core --test wake_queue_wait -- --ignored --nocapture
//! ```

#![allow(clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::clock::SystemClock;
use chiefd_core::store::{activity, organization, supervision, COMPANY_DB_FILENAME};
use chiefd_core::test_support::northstar_manifest;

const LABEL: &str = "northstar-conformance";

/// The operator's own `org_events` row count, measured on their 97MB company
/// database by `efe2aeb38` (the commit that indexed the two `MAX(at)` reads).
const OPERATOR_EVENT_ROWS: usize = 322_329;

/// Mailbox envelopes to plant. Every one of these is loaded into `Ledgers` at
/// open and DEEP-CLONED by `run_job` on every single write — including the
/// operator's wake. This is the term §3 calls "proportional to accumulated
/// mail", and leaving it at zero would measure a wake that costs nothing.
const MAILBOX_ROWS: usize = 6_000;

/// Effect rows, cloned on every write for the same reason.
const EFFECT_ROWS: usize = 6_000;

/// Concurrent company reads to hold in flight. The operator's box was measured
/// at a peak of 104 in flight (`70ec7d376`); 64 is inside that and is the top of
/// the range `company_read_under_load` already reports.
const READERS_IN_FLIGHT: usize = 64;

/// How many wakes to time.
const WAKE_SAMPLES: usize = 20;

// ---------------------------------------------------------------------------
// The company
// ---------------------------------------------------------------------------

/// Seed a northstar company and inflate it to the operator's measured shape.
///
/// The events are bulk-inserted rather than committed one mutation at a time
/// because 322,329 commits would take hours and would produce the same table:
/// `org_events` is append-only and its readers (`MAX(at)`, the entity-filtered
/// `MAX(at)`) only ever see rows.
async fn realistic_company(dir: &std::path::Path) -> (Arc<CompanyDb>, u64, i64) {
    let path = dir.join(COMPANY_DB_FILENAME);
    {
        let company =
            CompanyDb::open(LABEL, &path, Arc::new(SystemClock::default())).expect("open company");
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
        // Drop closes the writer and checkpoints the WAL, so the inflation below
        // is the only connection open on this file.
    }

    let events = inflate(&path);

    let db = Arc::new(
        CompanyDb::open(LABEL, &path, Arc::new(SystemClock::default())).expect("reopen company"),
    );
    let bytes = std::fs::metadata(&path).expect("stat company db").len();
    (db, bytes, events)
}

/// Bulk-plant the accumulated history a long-lived company carries, returning
/// the resulting `org_events` row count.
fn inflate(path: &std::path::Path) -> i64 {
    // `store::open_company_db` is the sanctioned opener — it applies the same
    // pragmas the writer actor uses, so the planted history lands in a file the
    // actor will reopen without complaint. The actor is closed at this point.
    let mut conn = chiefd_core::store::open_company_db(path).expect("open for inflation");

    let existing: i64 = conn
        .query_row("SELECT COUNT(*) FROM org_events WHERE slug = ?1", [LABEL], |r| r.get(0))
        .expect("count events");
    let next_seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM org_events WHERE slug = ?1",
            [LABEL],
            |r| r.get(0),
        )
        .expect("max seq");

    // The feed a busy company actually writes: mostly the high-frequency
    // entities, with the three MANIFEST entities rare — which is precisely the
    // distribution that made the entity-filtered `MAX(at)` expensive before
    // `efe2aeb38`, so a uniform mix would measure an easier database than the
    // operator's.
    const HOT: [&str; 5] =
        ["person-activity", "supervision", "effect", "transition", "launch-intent"];
    const MANIFEST: [&str; 3] = ["person", "department", "org"];

    let txn = conn.transaction().expect("begin inflation");
    {
        let mut stmt = txn
            .prepare(
                "INSERT INTO org_events(slug, seq, entity, entity_id, op, actor, at, detail_ref) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .expect("prepare event insert");
        // One event every two seconds, ending now: a plausible history for a
        // company that has been up for weeks, and it spreads `at` over a real
        // range instead of collapsing the index to one key.
        let base_millis = 1_785_542_400_000_i64;
        let target = i64::try_from(OPERATOR_EVENT_ROWS).unwrap_or(i64::MAX) - existing;
        for i in 0..target.max(0) {
            let entity = if i % 500 == 0 {
                MANIFEST[usize::try_from(i).unwrap_or(0) / 500 % MANIFEST.len()]
            } else {
                HOT[usize::try_from(i).unwrap_or(0) % HOT.len()]
            };
            let at = chiefd_core::isotime::iso_millis(base_millis + i * 2_000);
            stmt.execute(rusqlite::params![
                LABEL,
                next_seq + i,
                entity,
                format!("{entity}-{}", i % 97),
                "update",
                "chiefd-actuator",
                at,
                format!("{entity}s:{LABEL}/{i}"),
            ])
            .expect("insert event");
        }
    }
    // The counter is what a real writer allocates from; leaving it behind the
    // planted rows would make the next real commit collide on the primary key.
    txn.execute(
        "INSERT INTO counters(name, value) VALUES(?1, ?2) \
         ON CONFLICT(name) DO UPDATE SET value = ?2",
        rusqlite::params![
            format!("org-events:{LABEL}"),
            next_seq + i64::try_from(OPERATOR_EVENT_ROWS).unwrap_or(i64::MAX) - existing - 1
        ],
    )
    .expect("advance the event counter");

    {
        let mut stmt = txn
            .prepare(
                "INSERT INTO mailbox(slug, envelope_id, id, person, from_person_id, \
                 to_person_id, message, urgency, reply_to, created_at, state, updated_at) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'normal', NULL, ?8, 'delivered', ?9)",
            )
            .expect("prepare mailbox insert");
        for i in 0..MAILBOX_ROWS {
            let id = format!("env-{i:07}");
            stmt.execute(rusqlite::params![
                LABEL,
                format!("{id}@signal-researcher"),
                id,
                "signal-researcher",
                "quant-head",
                "signal-researcher",
                // A real message body, not a token: the clone copies the bytes.
                format!("{} {i}", "status update on the overnight run; ".repeat(12)),
                chiefd_core::isotime::iso_millis(1_785_542_400_000 + (i as i64) * 60_000),
                1_785_542_400_000_i64 + (i as i64) * 60_000,
            ])
            .expect("insert mailbox row");
        }
    }

    {
        let mut stmt = txn
            .prepare(
                "INSERT INTO effects(slug, seq, id, kind, status, created_at, delivered_at) \
                 VALUES(?1, ?2, ?3, 'goal.published', 'delivered', ?4, ?5)",
            )
            .expect("prepare effect insert");
        let base: i64 = txn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM effects WHERE slug = ?1",
                [LABEL],
                |r| r.get(0),
            )
            .expect("max effect seq");
        for i in 0..EFFECT_ROWS {
            let at = 1_785_542_400_000_i64 + (i as i64) * 60_000;
            stmt.execute(rusqlite::params![
                LABEL,
                base + i as i64,
                format!("eff-{i:07}"),
                chiefd_core::isotime::iso_millis(at),
                at,
            ])
            .expect("insert effect row");
        }
        // `load_relational` refuses a company whose effect counter sits at or
        // below the highest issued sequence, so the planted history has to
        // advance it exactly as the writer that issued those sequences would.
        txn.execute(
            "INSERT INTO counters(name, value) VALUES(?1, ?2) \
             ON CONFLICT(name) DO UPDATE SET value = ?2",
            rusqlite::params![
                format!("next_effect_sequence:{LABEL}"),
                base + i64::try_from(EFFECT_ROWS).unwrap_or(i64::MAX)
            ],
        )
        .expect("advance the effect counter");
    }
    txn.commit().expect("commit inflation");
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;").expect("compact");
    conn.query_row("SELECT COUNT(*) FROM org_events WHERE slug = ?1", [LABEL], |r| r.get(0))
        .expect("count events")
}

// ---------------------------------------------------------------------------
// The instrument
// ---------------------------------------------------------------------------

/// One writer job as the writer itself saw it.
#[derive(Debug, Clone, Copy)]
struct Observed {
    name: &'static str,
    class: MutationClass,
    /// The writer's own enqueue → start figure, stamped at admission.
    queued_ms: u64,
}

/// Samples [`CompanyDb::queue_snapshot`] and records every distinct job the
/// writer runs, with the wait that job served.
struct Sampler {
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<Observed>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Sampler {
    fn start(db: &Arc<CompanyDb>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let seen: Arc<Mutex<Vec<Observed>>> = Arc::new(Mutex::new(Vec::new()));
        let db = Arc::clone(db);
        let stop_thread = Arc::clone(&stop);
        let seen_thread = Arc::clone(&seen);
        // A dedicated OS thread, not a tokio task: it must keep sampling while
        // every worker thread is busy serving the load under measurement.
        let handle = std::thread::spawn(move || {
            let mut last: Option<(&'static str, u64)> = None;
            while !stop_thread.load(Ordering::Relaxed) {
                if let Some(job) = db.queue_snapshot().current {
                    let here = (job.name.0, job.enqueued_ms);
                    if last != Some(here) {
                        seen_thread.lock().unwrap_or_else(|p| p.into_inner()).push(Observed {
                            name: job.name.0,
                            class: job.class,
                            queued_ms: here.1,
                        });
                        last = Some(here);
                    }
                } else {
                    last = None;
                }
                std::thread::yield_now();
            }
        });
        Self { stop, seen, handle: Some(handle) }
    }

    fn finish(mut self) -> Vec<Observed> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("join sampler");
        }
        let out = self.seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
        out
    }
}

fn summarize(mut samples: Vec<Duration>) -> (Duration, Duration, Duration) {
    samples.sort_unstable();
    (samples[samples.len() / 2], samples[samples.len() * 95 / 100], samples[samples.len() - 1])
}

// ---------------------------------------------------------------------------
// The load
// ---------------------------------------------------------------------------

/// What the wake is scheduled against.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Load {
    /// Nothing else running. The wake's own cost, with no queue at all.
    Idle,
    /// The operator's box: company reads held at their measured concurrency,
    /// plus the converge cycle's `Reconcile` commit at its measured rate.
    ///
    /// There is deliberately NO saturating `Small` stream here, because the
    /// operator's own request mix has none. Their 96,907-request window is 57%
    /// `/v1/docs/watch` and 43% reads; the only `Small`-class writers left in
    /// this daemon are `host-txn.*` (four per host action, i.e. per launch),
    /// `auth.identity.*`, `converge.set_config` / `clear_breaker` /
    /// `record_refusal`, the delivery-sink staging commit and a handful of
    /// route-driven preview/plan ops. None of them is a stream.
    OperatorShaped,
    /// The shape this daemon had before `70ec7d376`: the SAME reads, issued as
    /// `Small` jobs on the writer's queue, which is exactly what `read_on_actor`
    /// did (`class: MutationClass::Small, name: None`).
    ///
    /// This arm is why the numbers above it mean anything. Without it, a fast
    /// result in `OperatorShaped` is equally consistent with "the queue no
    /// longer delays the write" and with "this harness cannot see a queue delay".
    BeforeTheReadPool,
}

impl Load {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::OperatorShaped => "operator-shaped",
            Self::BeforeTheReadPool => "pre-70ec7d376 (reads on the queue)",
        }
    }
}

/// The converge cycle's measured commit rate on the operator's company: 0.26
/// writes per second (`70ec7d376`, "supervision cycle committed ran 0.289/s
/// before, 0.260/s after").
const CONVERGE_PERIOD: Duration = Duration::from_millis(3_846);

/// Wall time this harness spends GENERATING load, never waiting for a result.
///
/// The `tokio::time::sleep` ban exists so no test polls for a condition another
/// thread must make true — every such wait in this workspace goes through
/// `wait_until` or the injected `Clock`. Nothing here is such a wait: these are
/// the arrival rates of the background traffic under measurement and the gap
/// between two operator clicks. A benchmark that generated its load as fast as
/// the CPU allows would be measuring a different company.
#[allow(clippy::disallowed_methods)]
async fn pace(interval: Duration) {
    tokio::time::sleep(interval).await;
}

/// One arm: run `WAKE_SAMPLES` wakes under `load` and report what the writer saw.
async fn measure_under(db: &Arc<CompanyDb>, load: Load) -> (Vec<Duration>, Vec<Observed>) {
    let sampler = Sampler::start(db);
    let stop = Arc::new(AtomicBool::new(false));
    let mut background = Vec::new();

    if load != Load::Idle {
        for _ in 0..READERS_IN_FLIGHT {
            let db = Arc::clone(db);
            let stop = Arc::clone(&stop);
            background.push(tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    // `org_manifest_and_activity_read` is what
                    // `/v1/org/runtime/desired` and `/v1/org/activity/read` sit
                    // on, and the most expensive read the daemon serves.
                    match load {
                        Load::BeforeTheReadPool => {
                            // The old lane, carrying the identical read body:
                            // the two statements `org_manifest_and_activity_read`
                            // runs, on a `Small` writer job.
                            let _ = db
                                .in_transaction(
                                    MutationClass::Small,
                                    MutationName("read.on.actor"),
                                    |tx| {
                                        let manifest =
                                            chiefd_core::store::organization_rows::reconstruct(
                                                tx, LABEL,
                                            )?;
                                        if let Some(manifest) = &manifest {
                                            let _ = chiefd_core::store::activity::rows::read_rows(
                                                tx, LABEL, manifest,
                                            );
                                        }
                                        Ok(())
                                    },
                                )
                                .await;
                        }
                        _ => {
                            let _ = db.org_manifest_and_activity_read().await;
                        }
                    }
                }
            }));
        }
        let db = Arc::clone(db);
        let stop = Arc::clone(&stop);
        background.push(tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let _ = db
                    .mutate(
                        MutationClass::Reconcile,
                        MutationName("duty.supervision_reconcile"),
                        |_| Ok(()),
                    )
                    .await;
                pace(CONVERGE_PERIOD).await;
            }
        }));
        // Let the load reach steady state before timing anything.
        pace(Duration::from_millis(750)).await;
    }

    let mut totals = Vec::with_capacity(WAKE_SAMPLES);
    for _ in 0..WAKE_SAMPLES {
        let started = Instant::now();
        let outcome = db
            .wake_person(
                "signal-researcher".to_owned(),
                chiefd_core::isotime::iso_millis(1_785_542_400_000),
                "operator".to_owned(),
            )
            .await;
        totals.push(started.elapsed());
        assert!(outcome.is_ok(), "the wake must actually run: {outcome:?}");
        pace(Duration::from_millis(20)).await;
    }

    stop.store(true, Ordering::Relaxed);
    for task in background {
        let _ = task.await;
    }
    (totals, sampler.finish())
}

// ---------------------------------------------------------------------------
// The measurement
// ---------------------------------------------------------------------------

/// **THE STAGE 2.3 MEASUREMENT.** Enqueue → start and enqueue → end for the
/// operator's own write, on a company shaped like theirs, under three loads.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "builds a ~100MB company; run with --ignored --nocapture"]
async fn what_the_operators_wake_waits_for() {
    let dir = tempfile::tempdir().expect("tempdir");
    let built = Instant::now();
    let (db, bytes, events) = realistic_company(dir.path()).await;
    println!(
        "\ncompany: {:.1} MB, {events} org_events rows, built in {:?}",
        bytes as f64 / 1_048_576.0,
        built.elapsed()
    );

    // Warm: the daemon's steady state is warm, and the read below is also the
    // best available measure of what `LiveOrganizationProjection::reconstruct`
    // costs inside the wake's own transaction — it runs the same two statements.
    let mut reads = Vec::with_capacity(10);
    for _ in 0..10 {
        let started = Instant::now();
        let out = db.org_manifest_and_activity_read().await;
        assert!(matches!(out, Ok(Some(_))), "the read must find this company");
        reads.push(started.elapsed());
    }
    let (r_med, _, _) = summarize(reads);
    println!("  one manifest+activity read (= the projection rebuild the wake does): {r_med:?}");

    println!(
        "\n  {:<36} {:>10} {:>10} {:>10} {:>12}",
        "load", "wake med", "wake p95", "wake max", "queue wait"
    );
    let mut records = Vec::new();
    for load in [Load::Idle, Load::OperatorShaped, Load::BeforeTheReadPool] {
        let (totals, observed) = measure_under(&db, load).await;
        let (med, p95, max) = summarize(totals);
        let mut waits: Vec<u64> =
            observed.iter().filter(|o| o.name == "org.person.wake").map(|o| o.queued_ms).collect();
        assert!(
            !waits.is_empty(),
            "the sampler never caught a wake as the running job under {}, so there is no \
             enqueue -> start figure and this arm proves nothing",
            load.label()
        );
        waits.sort_unstable();
        let wait_med = waits[waits.len() / 2];
        println!(
            "  {:<36} {:>10?} {:>10?} {:>10?} {:>10}ms",
            load.label(),
            med,
            p95,
            max,
            wait_med
        );
        records.push((load, med, wait_med, observed));
    }

    for (load, _, _, observed) in &records {
        println!("\n  writer's own enqueue -> start under {}:", load.label());
        let mut by_op: std::collections::BTreeMap<(&str, String), Vec<u64>> =
            std::collections::BTreeMap::new();
        for o in observed {
            by_op.entry((o.name, format!("{:?}", o.class))).or_default().push(o.queued_ms);
        }
        for ((name, class), mut waits) in by_op {
            waits.sort_unstable();
            println!(
                "    {:<40} n={:<6} med={:>6}ms max={:>6}ms",
                format!("{name} ({class})"),
                waits.len(),
                waits[waits.len() / 2],
                waits[waits.len() - 1]
            );
        }
    }
    println!();

    // The arm that makes the rest evidence rather than assertion.
    let before_pool = records
        .iter()
        .find(|(load, ..)| *load == Load::BeforeTheReadPool)
        .expect("the counterfactual arm ran");
    let operator = records
        .iter()
        .find(|(load, ..)| *load == Load::OperatorShaped)
        .expect("the operator-shaped arm ran");
    assert!(
        before_pool.2 > operator.2,
        "putting the reads back on the writer's queue did not delay the wake ({}ms against \
         {}ms), so this measurement cannot see the defect it exists to rule out",
        before_pool.2,
        operator.2
    );
    // …and, given that it CAN see it, the conclusion Stage 2.3 was dropped on.
    // Half the aging interval: anything at or beyond that is the window binding
    // again, which is the finding that would put the stage back on the table.
    let aging_half =
        u64::try_from(chiefd_core::actor::AGING_INTERVAL.as_millis() / 2).unwrap_or(u64::MAX);
    assert!(
        operator.2 < aging_half,
        "the operator's wake waited {}ms in the queue under operator-shaped load. Stage 2.3 was \
         dropped because that number was 0 — something has put a Small stream back in front of \
         the one write a human is watching",
        operator.2
    );
}
