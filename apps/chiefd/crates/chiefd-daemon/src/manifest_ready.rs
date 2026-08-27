//! The bounded wait for genesis to commit the organization manifest.
//!
//! # The race this closes
//!
//! `chief_cli::genesis` starts the company's daemon and THEN posts
//! `/v1/org/manifest/genesis-with-models` to that daemon's own URL. It cannot do
//! it the other way round: the daemon is the single writer for the company's
//! SQLite file, so genesis writes THROUGH the process it just spawned (see
//! `chief_cli::genesis`'s module doc — "ONE daemon start for the whole flow").
//!
//! The daemon therefore starts life on a company that has no manifest yet, and
//! every startup duty reads that manifest. Measured on a live box
//! (`tribes-capital`, 2026-08-11) the daemon lost that race by 229 ms and logged
//! six refusals — the two boot ledger seeds, the startup self-audit, the
//! cycle-input gather, the supervision self-heal and the reminder dispatch — all
//! naming the same cause, `unknown-company: this company has no organization
//! manifest`. Every launch did this. None of it was a fault.
//!
//! # What this is, and what it is NOT
//!
//! This gate makes "genesis has not committed yet" a distinct, EXPECTED startup
//! state, waited for once, instead of six duty refusals. It is not a retry
//! ladder and not a silencer:
//!
//! * The wait is bounded by [`MANIFEST_READY_BUDGET`] and taken through the
//!   injected clock, so a test resolves it by advancing time rather than by
//!   waiting (TESTING.md §4.2) and no production path can wait forever.
//! * A budget that expires is a loud `ERROR`, and the caller proceeds into the
//!   duty loop exactly as it does today — refusing and then self-healing. The
//!   gate never invents readiness it did not observe, and it never removes the
//!   safety net that catches a company whose manifest arrives later still.
//!
//! # Why polling, and why it costs nothing
//!
//! `CompanyDb::read` serves the last committed in-memory snapshot, so a poll is
//! a lock and a map lookup, not SQL. The loop only ever runs on a company that
//! has no manifest at all — which is a company being born, once, for the few
//! hundred milliseconds genesis takes. Subscribing to the change feed instead
//! would thread an `Arc<ChangeFeed>` through `Daemon::serve` for one boot-time
//! read; the snapshot poll needs nothing that is not already here.

use std::time::Duration;

use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SharedClock;
use chiefd_core::store::organization;

/// How long a booting daemon waits for genesis before it gives up and starts
/// its duties anyway.
///
/// Genesis measured 229 ms end to end on the box this gate was written for. The
/// budget is two orders of magnitude above that on purpose: it is a ceiling on
/// a pathological launch, not a tuned expectation, and the cost of it being too
/// generous is nothing (a company that never gets a manifest was never going to
/// run duties either), while the cost of it being too tight is a daemon that
/// starts refusing on a slow box exactly as it did before this existed.
pub const MANIFEST_READY_BUDGET: Duration = Duration::from_secs(30);

/// How often the wait re-reads the committed snapshot.
///
/// Short enough that the daemon's first duty pass follows genesis closely (the
/// operator-visible cost of this gate is at most one interval), long enough that
/// the whole budget is a few hundred snapshot reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// What the gate observed. Every variant is reported by the caller; none of them
/// is a silent pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestReadiness {
    /// The manifest was already durable when this daemon reached the gate —
    /// every restart of an existing company, and no waiting at all.
    AlreadyDurable,
    /// The daemon won the race and genesis committed while it waited: the
    /// ordinary birth of a new company.
    CommittedWhileWaiting,
    /// [`MANIFEST_READY_BUDGET`] expired with no manifest. The caller proceeds
    /// into its duty loop, which refuses and self-heals as before.
    BudgetExpired,
}

impl ManifestReadiness {
    /// Whether a manifest was actually observed. `false` means the caller is
    /// starting duties on a company that is not there yet — knowingly.
    #[must_use]
    pub fn is_ready(self) -> bool {
        matches!(self, Self::AlreadyDurable | Self::CommittedWhileWaiting)
    }
}

/// True when this company's organization manifest is committed and readable.
fn manifest_is_durable(company: &CompanyDb) -> bool {
    company.read(|snapshot| organization::exists(snapshot.ledgers()))
}

/// Wait, at most `budget`, for `company`'s organization manifest to exist.
///
/// The caller must have mounted the HTTP surface FIRST: genesis arrives over
/// that surface, so a gate placed before the mount would wait for a write that
/// can never be delivered. [`crate::run`]'s `Daemon::serve` is the one caller
/// and does exactly that.
pub async fn await_manifest(
    company: &CompanyDb,
    slug: &str,
    clock: &SharedClock,
    budget: Duration,
) -> ManifestReadiness {
    if manifest_is_durable(company) {
        return ManifestReadiness::AlreadyDurable;
    }
    let budget_ms = u64::try_from(budget.as_millis()).unwrap_or(u64::MAX);
    let started = clock.monotonic();
    tracing::info!(
        company = %slug,
        budget_ms,
        "chiefd run: no organization manifest yet; holding the startup duties until genesis \
         commits one (this is the ordinary state of a company being created — the daemon is \
         the writer genesis writes through, so it always starts first)"
    );
    loop {
        clock.sleep(POLL_INTERVAL).await;
        if manifest_is_durable(company) {
            tracing::info!(
                company = %slug,
                waited_ms = started.millis_until(clock.monotonic()),
                "chiefd run: genesis committed the organization manifest; startup duties proceed"
            );
            return ManifestReadiness::CommittedWhileWaiting;
        }
        if started.millis_until(clock.monotonic()) >= budget_ms {
            tracing::error!(
                company = %slug,
                budget_ms,
                "chiefd run: no organization manifest after the whole readiness budget; starting \
                 the duties anyway, so every one of them will refuse `unknown-company` until a \
                 manifest is committed and the reactive self-heal picks it up"
            );
            return ManifestReadiness::BudgetExpired;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
    use chiefd_core::clock::SharedClock;
    use chiefd_core::test_support::{northstar_manifest, ManualClock};

    use super::{await_manifest, ManifestReadiness, MANIFEST_READY_BUDGET, POLL_INTERVAL};

    const SLUG: &str = "northstar-conformance";

    /// An EMPTY company writer — schema present, no manifest — which is exactly
    /// what genesis spawns this daemon onto.
    fn empty_company(clock: SharedClock) -> (tempfile::TempDir, Arc<CompanyDb>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
        let company = Arc::new(CompanyDb::open(SLUG, &path, clock).expect("open company db"));
        (dir, company)
    }

    /// What genesis commits, atomically, in `org_manifest_genesis_with_models`:
    /// the manifest AND both scheduler ledgers in ONE transaction.
    async fn commit_genesis(company: &CompanyDb) {
        company
            .mutate(MutationClass::Normal, MutationName("test.genesis"), move |ledgers| {
                let manifest = northstar_manifest(0);
                chiefd_core::store::organization::create(ledgers, &manifest)?;
                chiefd_core::store::supervision::seed(ledgers, &manifest)?;
                chiefd_core::store::activity::seed(ledgers, &manifest)?;
                Ok(())
            })
            .await
            .expect("genesis commits");
    }

    /// Yield cooperatively until the manual clock has exactly `target` parked
    /// waits, or fail — a bounded settle, so a wiring bug fails the test rather
    /// than hanging the suite (the same shape `run::tests::settle_sleeps` uses).
    async fn settle_sleeps(clock: &ManualClock, target: usize) {
        for _ in 0..1_000 {
            if clock.pending_sleeps() == target {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("clock never settled to {target} pending sleeps (saw {})", clock.pending_sleeps());
    }

    /// Every restart of an existing company: the gate costs nothing and parks no
    /// wait at all. Asserted through the clock, because `pending_sleeps() == 0`
    /// is a state rather than a timing guess.
    #[tokio::test]
    async fn a_company_whose_manifest_is_already_durable_never_waits() {
        let mc = Arc::new(ManualClock::default());
        let clock: SharedClock = mc.clone();
        let (_dir, company) = empty_company(clock.clone());
        commit_genesis(&company).await;

        assert_eq!(
            await_manifest(&company, SLUG, &clock, MANIFEST_READY_BUDGET).await,
            ManifestReadiness::AlreadyDurable
        );
        assert_eq!(mc.pending_sleeps(), 0, "a durable company parks no wait on the clock");
    }

    /// THE REGRESSION. The daemon reaches the gate FIRST — as it always does,
    /// because genesis writes through it — and the manifest lands while it waits.
    /// The gate must report readiness, so every startup duty that follows runs
    /// against a company that exists instead of refusing `unknown-company`.
    #[tokio::test]
    async fn the_manifest_committed_while_the_daemon_waits_releases_the_gate() {
        let mc = Arc::new(ManualClock::default());
        let clock: SharedClock = mc.clone();
        let (_dir, company) = empty_company(clock.clone());

        let waiting = {
            let company = Arc::clone(&company);
            let clock = clock.clone();
            tokio::spawn(async move {
                await_manifest(&company, SLUG, &clock, MANIFEST_READY_BUDGET).await
            })
        };
        // Park the wait on the clock BEFORE genesis commits, so this is the real
        // ordering (daemon first, manifest second) and not a race the test won.
        settle_sleeps(&mc, 1).await;
        commit_genesis(&company).await;
        mc.advance(POLL_INTERVAL);

        assert_eq!(
            waiting.await.expect("the wait completes"),
            ManifestReadiness::CommittedWhileWaiting
        );
    }

    /// The other half of a bounded wait: it ENDS. A manifest that never arrives
    /// must not hold the daemon for ever — the budget expires, the caller is told
    /// so, and the duties start anyway (refusing, then self-healing) exactly as
    /// they did before this gate existed.
    ///
    /// Driven one poll at a time on a SHORT explicit budget rather than the
    /// production one, so what is under test is the loop's own deadline
    /// arithmetic and not a 600-step replay of it.
    #[tokio::test]
    async fn a_manifest_that_never_arrives_expires_the_budget_rather_than_waiting_for_ever() {
        let mc = Arc::new(ManualClock::default());
        let clock: SharedClock = mc.clone();
        let (_dir, company) = empty_company(clock.clone());
        let budget = POLL_INTERVAL * 3;

        let waiting = {
            let company = Arc::clone(&company);
            let clock = clock.clone();
            tokio::spawn(async move { await_manifest(&company, SLUG, &clock, budget).await })
        };
        // Two polls inside the budget: still waiting, and the assertion is the
        // parked wait itself, never elapsed wall time.
        for _ in 0..2 {
            settle_sleeps(&mc, 1).await;
            assert!(!waiting.is_finished(), "the gate holds while the budget has time left");
            mc.advance(POLL_INTERVAL);
        }
        // The third poll reaches the budget exactly.
        settle_sleeps(&mc, 1).await;
        mc.advance(POLL_INTERVAL);

        assert_eq!(waiting.await.expect("the wait completes"), ManifestReadiness::BudgetExpired);
    }

    /// The production budget is a real bound, and generous against the 229 ms
    /// genesis this gate was written for. Pinned so that shrinking it toward the
    /// measured value — which would put a slow box back on the refusal path this
    /// closes — is a deliberate edit rather than a tweak.
    #[test]
    fn the_production_budget_is_bounded_and_far_above_a_measured_genesis() {
        assert_eq!(MANIFEST_READY_BUDGET, Duration::from_secs(30));
        assert!(MANIFEST_READY_BUDGET > Duration::from_millis(229) * 10);
        assert!(POLL_INTERVAL < MANIFEST_READY_BUDGET);
    }

    /// The outcome type is what the caller branches on, so the two ready variants
    /// must answer together and the expired one must not.
    #[test]
    fn only_an_observed_manifest_reports_ready() {
        assert!(ManifestReadiness::AlreadyDurable.is_ready());
        assert!(ManifestReadiness::CommittedWhileWaiting.is_ready());
        assert!(!ManifestReadiness::BudgetExpired.is_ready());
    }
}
