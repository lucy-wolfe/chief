//! The injected clock.
//!
//! Every TTL, retry ladder, queue deadline and admission timestamp in chiefd
//! reads time through [`Clock`] (TESTING.md §4.2). Expiry, renewal and backoff
//! tests advance a [`ManualClock`] explicitly; **no test sleeps to wait for a
//! timeout**. Real time is used only where a test measures genuine waiting
//! (the separate-process locktest budgets) and in the e2e harness.
//!
//! Owned by Track B. `chiefd-host` and `chiefd-api` take a `&dyn Clock` rather
//! than calling `Instant::now()` themselves.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A wait handed back by [`Clock::sleep`].
///
/// Boxed and `'static` so the trait stays object-safe and a wait can outlive
/// the borrow that created it — the lease ladder parks on one of these inside
/// a `select!`.
pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Monotonic instant, in milliseconds since an arbitrary process-local origin.
///
/// Deliberately not `std::time::Instant`: TTL-style comparisons need values a
/// manual clock can mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Monotonic(pub u64);

impl Monotonic {
    /// This instant advanced by `d`, saturating rather than wrapping.
    #[must_use]
    pub fn saturating_add(self, d: Duration) -> Self {
        Self(self.0.saturating_add(u64::try_from(d.as_millis()).unwrap_or(u64::MAX)))
    }

    /// Milliseconds from `self` to `later`, or zero if `later` is in the past.
    #[must_use]
    pub fn millis_until(self, later: Self) -> u64 {
        later.0.saturating_sub(self.0)
    }
}

/// Wall-clock milliseconds since the Unix epoch.
///
/// Used for durable columns (`admitted_at_ms`, `updated_at`) that outlive the
/// process; never for expiry decisions, which are monotonic (plan §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WallMillis(pub i64);

impl WallMillis {
    /// Minimal ISO-8601 rendering, UTC, millisecond precision
    /// (`2026-07-23T19:31:09.142Z`).
    ///
    /// The canonical home for this conversion — `store::launch_intent`'s own
    /// `updated_at` field delegates here rather than keeping a second copy.
    /// Added for #376: `chiefd-core`'s writer actor needs to hand a
    /// `ChangeFeed::publish`-shaped `updated_at: impl Into<String>` to its
    /// change-feed sink, and the feed's wire contract (see
    /// `chiefd-api::docstore::feed`'s module doc) is a caller-supplied ISO
    /// string, not a raw epoch integer — the same shape `DocStore`'s own
    /// write methods already pass in from the TypeScript-facing callers.
    #[must_use]
    pub fn to_iso8601(self) -> String {
        let epoch_millis = self.0;
        let (days, millis_of_day) =
            (epoch_millis.div_euclid(86_400_000), epoch_millis.rem_euclid(86_400_000));
        let (year, month, day) = civil_from_days(days);
        let (h, m, s, ms) = (
            millis_of_day / 3_600_000,
            (millis_of_day / 60_000) % 60,
            (millis_of_day / 1_000) % 60,
            millis_of_day % 1_000,
        );
        format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}.{ms:03}Z")
    }
}

use crate::isotime::civil_from_days;

/// The seam every time read in chiefd passes through.
pub trait Clock: Send + Sync + 'static {
    /// Monotonic reading, for expiry, renewal and backoff.
    fn monotonic(&self) -> Monotonic;
    /// Wall-clock reading, for durable timestamp columns.
    fn wall(&self) -> WallMillis;

    /// Wait for `d`.
    ///
    /// **Every** wait in chiefd that is not "queue depth" goes through here —
    /// the lease retry ladder and the renewal timer are the two at M8.
    /// `clippy.toml` bans `tokio::time::sleep` and `std::thread::sleep`
    /// outright, so this is the only route, and a test on a
    /// [`ManualClock`](crate::test_support::ManualClock) resolves the wait by
    /// advancing time rather than by taking that long (TESTING.md §4.2).
    fn sleep(&self, d: Duration) -> Sleep;
}

/// Production clock: `Instant` for monotonic, `SystemTime` for wall.
#[derive(Debug)]
pub struct SystemClock {
    origin: std::time::Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self { origin: std::time::Instant::now() }
    }
}

impl Clock for SystemClock {
    fn monotonic(&self) -> Monotonic {
        Monotonic(u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    fn wall(&self) -> WallMillis {
        // A host whose clock predates the epoch is not a case chiefd models;
        // clamping keeps the column monotone-ish instead of panicking.
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        WallMillis(millis)
    }

    fn sleep(&self, d: Duration) -> Sleep {
        // Seam exception, and the only one in the workspace: `clippy.toml`
        // bans `tokio::time::sleep` precisely so that every wait is reachable
        // from the injected clock and therefore skippable by a test. Handing
        // the wait to the runtime here is what makes that ban affordable.
        #[allow(clippy::disallowed_methods)]
        Box::pin(tokio::time::sleep(d))
    }
}

/// Convenience alias: clocks are shared across the actor, lease timer and API.
pub type SharedClock = Arc<dyn Clock>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_addition_saturates_instead_of_wrapping() {
        let near_max = Monotonic(u64::MAX - 5);
        assert_eq!(near_max.saturating_add(Duration::from_secs(1)), Monotonic(u64::MAX));
    }

    #[test]
    fn millis_until_is_zero_for_past_instants() {
        assert_eq!(Monotonic(500).millis_until(Monotonic(200)), 0);
        assert_eq!(Monotonic(200).millis_until(Monotonic(500)), 300);
    }

    #[test]
    fn system_clock_monotonic_never_goes_backwards() {
        let clock = SystemClock::default();
        let first = clock.monotonic();
        let second = clock.monotonic();
        assert!(second >= first);
        assert!(clock.wall().0 > 0);
    }

    #[test]
    fn to_iso8601_renders_the_documented_millisecond_format() {
        // 2026-07-23T19:31:09.142Z, precomputed epoch millis.
        assert_eq!(WallMillis(1_784_835_069_142).to_iso8601(), "2026-07-23T19:31:09.142Z");
    }

    #[test]
    fn to_iso8601_renders_the_unix_epoch() {
        assert_eq!(WallMillis(0).to_iso8601(), "1970-01-01T00:00:00.000Z");
    }
}
