//! ISO-8601 rendering and parsing for the millisecond timestamps the ported
//! store bodies carry.
//!
//! Every store ported in M10 keeps at least one ISO-8601 string field, because
//! the Phase-2 import (plan §8) is supposed to be a *parse* of the existing
//! JSON file rather than a translation of it. chiefd itself never reads
//! authority from these strings — `documents.updated_at` and the injected
//! [`Clock`](crate::clock::Clock) are the authority — but two of them are
//! compared against each other by ported logic (`retryNotBefore` versus `at`,
//! observation confirmation windows), so parsing has to be exact.
//!
//! Deliberately dependency-free in spirit: `chrono`/`time` for one
//! debug-visible field is supply-chain surface this crate does not need, and
//! the conversions are the standard branch-free civil-calendar algorithms.
//!
//! RENDERING is `chiefd_log::isotime`'s, and parsing is this module's. The
//! rendering moved out with the log sink: it was written three times in this
//! workspace, and every one of those copies stamps a timestamp somebody later
//! compares against another copy's. A leaf crate is what both sides of the
//! backend/client boundary can link, so there is now one.

pub use chiefd_log::isotime::{civil_from_days, iso_millis};

/// A schedule deadline, parsed to fail CLOSED (#69).
///
/// The bug this exists to delete: every schedule gate was written
/// `parse_iso_millis(&next_due_at).unwrap_or(i64::MAX) > now` — so an
/// UNPARSEABLE timestamp compares as infinitely far in the future, the
/// schedule is skipped, and because a schedule's `nextDueAt` is only ever
/// re-stamped BY firing, it is skipped again on every cycle, forever, with
/// nothing logged. A corrupt character in one field silently disables a
/// people-check or a goal-watch for the life of the company. That is the same
/// family as #63 (a countdown frozen while everything looks healthy) and #68
/// (a cycle that wrote nothing reporting "committed"): the failure reports as
/// normal operation.
///
/// Failing CLOSED — treating an unreadable deadline as already due — is both
/// louder and self-healing: the schedule fires once, and the advance that
/// follows re-stamps a VALID `nextDueAt` (the advance paths already fall back
/// to `now`), so the corruption is repaired rather than latched. A spurious
/// single fire is a far better failure than permanent silence.
///
/// Always logs, so the corruption is visible even where the caller has no
/// report to attach a warning to.
pub fn schedule_due_millis(text: &str, field: &str) -> i64 {
    match parse_iso_millis(text) {
        Some(millis) => millis,
        None => {
            tracing::warn!(
                field,
                value = text,
                "unreadable schedule timestamp: treating it as DUE NOW so the schedule fires and re-stamps a valid deadline, rather than being skipped forever"
            );
            // Due now, whatever `now` is. Not `i64::MIN`: a deadline compared
            // against arithmetic elsewhere must not invite overflow.
            0
        }
    }
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fff]Z` into epoch milliseconds.
///
/// Strict on purpose. The TypeScript predecessor validates these fields with
/// `Number.isFinite(Date.parse(value))`, which accepts a wide and
/// engine-defined set of spellings; chiefd accepts only the one spelling it
/// writes. A ledger containing anything else is unreadable bytes and takes its
/// store's polarity path (plan §5.5) rather than being silently reinterpreted.
#[must_use]
pub fn parse_iso_millis(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || !text.ends_with('Z') {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: i64 = text.get(5..7)?.parse().ok()?;
    let day: i64 = text.get(8..10)?.parse().ok()?;
    let hour: i64 = text.get(11..13)?.parse().ok()?;
    let minute: i64 = text.get(14..16)?.parse().ok()?;
    let second: i64 = text.get(17..19)?.parse().ok()?;
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let fraction = &text[19..text.len() - 1];
    let millis = if fraction.is_empty() {
        0
    } else {
        let digits = fraction.strip_prefix('.')?;
        if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut value: i64 = digits.parse().ok()?;
        for _ in digits.len()..3 {
            value *= 10;
        }
        value
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1_000 + millis)
}

/// Howard Hinnant's `days_from_civil`, the exact inverse of
/// [`civil_from_days`].
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #69: the whole point — an unreadable deadline must read as DUE, not as
    /// infinitely far away. The old `unwrap_or(i64::MAX)` made a corrupt
    /// timestamp compare as "not yet due" on every cycle forever, and since a
    /// schedule's `nextDueAt` is only re-stamped BY firing, the schedule was
    /// disabled permanently with nothing logged.
    #[test]
    fn an_unreadable_schedule_deadline_reads_as_due_now_not_as_never() {
        let now = 1_768_478_400_000_i64;
        for corrupt in [
            "",
            "not-a-timestamp",
            "2026-01-15T12:00:00.000",
            "20260115T120000Z",
            "2026-13-45T99:99:99.000Z",
        ] {
            let due = schedule_due_millis(corrupt, "test.nextDueAt");
            assert!(due <= now, "{corrupt:?} must read as already due, got {due}");
            // The precise old behaviour this replaces: i64::MAX > now, i.e.
            // "not due", every cycle, forever.
            assert_ne!(due, i64::MAX, "{corrupt:?} must not fail OPEN into permanent silence");
        }
    }

    /// #69, the spellings the ticket names: `parse_iso_millis` is deliberately
    /// STRICT (only the one spelling chiefd itself writes), while the launcher's
    /// TypeScript side validates with the lenient `Date.parse`. So a foreign
    /// spelling can be adopted verbatim into the ledger and then be unreadable
    /// here forever — the JS footer renders it fine and shows "due", while
    /// chiefd skips it silently. Each of these must now read as DUE, so the
    /// schedule fires and `advance_goal_watch` re-stamps it via `iso_millis`
    /// (that path already falls back to `now`), healing the spelling.
    #[test]
    fn every_foreign_spelling_reads_as_due_rather_than_disabling_the_schedule() {
        let now = 1_768_478_400_000_i64;
        let foreign = [
            "2026-01-15T12:00:00.000000Z", // microseconds (6 fractional digits)
            "2026-01-15T12:00:00+00:00",   // numeric offset instead of Z
            "2026-01-15T12:00:00.000z",    // lowercase z
            "2026-01-15 12:00:00.000Z",    // space separator
        ];
        for spelling in foreign {
            assert_eq!(
                parse_iso_millis(spelling),
                None,
                "{spelling:?} is strictly unreadable (the premise)"
            );
            let due = schedule_due_millis(spelling, "reminder.nextDueAt");
            assert!(due <= now, "{spelling:?} must fire rather than be skipped forever, got {due}");
        }
    }

    /// The positive control the ticket asks for: the exact 20-character
    /// spelling with no fractional part parses TODAY and must keep parsing —
    /// the fail-closed path must never swallow a deadline that is simply in
    /// the future.
    #[test]
    fn the_fractionless_twenty_character_spelling_still_parses_and_is_not_forced_due() {
        let text = "2026-01-15T06:29:04Z";
        let parsed = parse_iso_millis(text).expect("the canonical fractionless spelling parses");
        assert_eq!(schedule_due_millis(text, "reminder.nextDueAt"), parsed);
        assert!(
            schedule_due_millis(text, "reminder.nextDueAt") > 0,
            "a real deadline is not the due-now sentinel"
        );
    }

    /// A READABLE deadline is completely unaffected — the fail-closed path is
    /// reached only by genuinely corrupt data, so no live schedule changes
    /// timing because of this.
    #[test]
    fn a_readable_schedule_deadline_is_returned_exactly_as_parsed() {
        for millis in [0_i64, 1_752_883_200_123, 1_768_478_400_000] {
            let text = iso_millis(millis);
            assert_eq!(schedule_due_millis(&text, "test.nextDueAt"), millis);
            assert_eq!(
                schedule_due_millis(&text, "test.nextDueAt"),
                parse_iso_millis(&text).unwrap()
            );
        }
    }

    /// The self-healing property that makes failing closed safe: firing
    /// re-stamps a VALID deadline, so a corrupt schedule fires once and then
    /// behaves normally, rather than firing every cycle forever.
    #[test]
    fn a_corrupt_deadline_is_due_once_and_a_fresh_stamp_is_readable_again() {
        let now = 1_768_478_400_000_i64;
        assert!(schedule_due_millis("corrupt", "test.nextDueAt") <= now, "fires once");
        // What an advance writes next is a rendered timestamp, which parses.
        let restamped = iso_millis(now + 900_000);
        assert_eq!(schedule_due_millis(&restamped, "test.nextDueAt"), now + 900_000);
        assert!(
            schedule_due_millis(&restamped, "test.nextDueAt") > now,
            "and is not due again immediately"
        );
    }

    #[test]
    fn rendering_matches_the_javascript_to_iso_string_shape() {
        assert_eq!(iso_millis(0), "1970-01-01T00:00:00.000Z");
        // NOT the conformance epoch: this comment said so for as long as it has
        // existed and named the wrong number (#1047). The corpus's frozen instant
        // is `test_support::CONFORMANCE_EPOCH`, asserted in
        // `the_conformance_epoch_parses_to_the_value_the_fixtures_assume` below.
        assert_eq!(iso_millis(1_768_478_400_000), "2026-01-15T12:00:00.000Z");
        assert_eq!(iso_millis(1_752_883_200_123), "2025-07-19T00:00:00.123Z");
    }

    #[test]
    fn parsing_is_the_exact_inverse_of_rendering() {
        for millis in [0_i64, 1, 999, 1_752_883_200_123, 1_784_116_800_000, -86_400_000] {
            assert_eq!(parse_iso_millis(&iso_millis(millis)), Some(millis), "round trip {millis}");
        }
    }

    #[test]
    fn the_conformance_epoch_parses_to_the_value_the_fixtures_assume() {
        assert_eq!(parse_iso_millis("2026-07-15T12:00:00.000Z"), Some(1_784_116_800_000));
    }

    #[test]
    fn spellings_chiefd_does_not_write_are_unreadable_rather_than_guessed() {
        for text in [
            "2026-07-15T12:00:00+00:00", // offset form
            "2026-07-15 12:00:00.000Z",  // space separator
            "2026-07-15T12:00:00.0000Z", // four fractional digits
            "not a timestamp",
            "",
            "2026-13-15T12:00:00.000Z", // month 13
        ] {
            assert_eq!(parse_iso_millis(text), None, "'{text}' must not parse");
        }
    }

    #[test]
    fn a_second_without_a_fraction_still_parses() {
        assert_eq!(parse_iso_millis("2026-07-15T12:00:00Z"), Some(1_784_116_800_000));
    }
}
