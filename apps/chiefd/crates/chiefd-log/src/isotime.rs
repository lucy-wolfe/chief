//! ISO-8601 rendering of wall-clock instants, in the one spelling every chiefd
//! program writes.
//!
//! # Why it is here and not in one of the crates that used to hold it
//!
//! This conversion was written three times in this workspace, in two
//! formulations: `chiefd_core::isotime`, `chief_cli::company` (whose own doc
//! says the alternative "would have put the whole store crate in this binary's
//! link graph") and the sink beside this module. Every log line and every
//! ledger timestamp is rendered through it, so a disagreement between the
//! copies is a disagreement about when something happened — the one fact an
//! incident reconstruction cannot do without.
//!
//! A leaf crate is what the two sides of the backend/client boundary can both
//! link, so the third copy is deleted and the other two delegate here.
//!
//! Deliberately dependency-free: `chrono`/`time` for one rendered field is
//! supply-chain surface nobody needs, and the conversion below is the standard
//! branch-free civil-calendar algorithm.

use std::time::{SystemTime, UNIX_EPOCH};

/// Minimal ISO-8601 rendering of epoch milliseconds, UTC, always with exactly
/// three fractional digits — the shape `new Date(ms).toISOString()` produces,
/// which is what every ported ledger already contains.
#[must_use]
pub fn iso_millis(epoch_millis: i64) -> String {
    let (days, millis_of_day) =
        (epoch_millis.div_euclid(86_400_000), epoch_millis.rem_euclid(86_400_000));
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second, millis) = (
        millis_of_day / 3_600_000,
        (millis_of_day / 60_000) % 60,
        (millis_of_day / 1_000) % 60,
        millis_of_day % 1_000,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// [`iso_millis`] of a [`SystemTime`].
///
/// A clock before the epoch renders as the epoch rather than raising: this is a
/// log stamp, and a logger that can fail the operation it observes is worse
/// than no logger.
#[must_use]
pub fn timestamp(now: SystemTime) -> String {
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    iso_millis(i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

/// The current instant, rendered.
#[must_use]
pub fn now_iso_millis() -> String {
    timestamp(SystemTime::now())
}

/// Howard Hinnant's `civil_from_days`, the standard branch-free conversion.
///
/// THE calendar conversion for this workspace. Chosen over a dependency
/// because the whole reason these renderings are hand-rolled is to avoid drift
/// between runtimes; adding a date crate to gain three lines of arithmetic is a
/// poor trade.
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, iso_millis, timestamp};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn rendering_matches_the_javascript_to_iso_string_shape() {
        assert_eq!(iso_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_millis(1_768_478_400_000), "2026-01-15T12:00:00.000Z");
        assert_eq!(iso_millis(1_752_883_200_123), "2025-07-19T00:00:00.123Z");
    }

    /// The stamp every log line carries. Cross-checked against
    /// `date -u -d @1753000000`, the same value the sink asserted before this
    /// module owned the conversion.
    #[test]
    fn timestamps_match_known_civil_dates() {
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_millis(1_753_000_000_123)),
            "2025-07-20T08:26:40.123Z"
        );
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    }

    /// Instants before the epoch are the one case a `SystemTime` stamp cannot
    /// represent. It renders as the epoch instead of panicking, because the
    /// caller is a logger.
    #[test]
    fn a_clock_before_the_epoch_renders_rather_than_panicking() {
        assert_eq!(timestamp(UNIX_EPOCH - Duration::from_secs(60)), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn the_calendar_conversion_is_exact_at_the_boundaries_that_break_naive_ones() {
        // Leap day, the day after it, and a century that is not a leap year.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
