//! The clean-session epoch: transcripts older than it are never resumed.
//!
//! A person's pane resumes their newest transcript by modification time. That
//! is right for a respawn and wrong for a deliberate clean boot, so a CEO-only
//! boot stamps an epoch and session selection ignores every transcript older
//! than it.
//!
//! The epoch retires itself: the moment a person writes a transcript, that
//! transcript is newer than the epoch and is selected normally, so an ordinary
//! respawn resumes live context instead of wiping it.
//!
//! ## What this module deletes
//!
//! `org-session-epoch.ts` computed the monotonic maximum inside a client-side
//! compare-and-swap mutator, re-running it on every miss so a stale clock could
//! not clobber a concurrent stamp. [`stamp`] runs on the writer thread inside
//! one transaction, so the read and the max and the write cannot interleave
//! with anything and there is nothing to re-run.
//!
//! Reads fail **open** — an absent or malformed epoch resolves to 0, meaning
//! "resume normally". The risk here is losing an agent's working context, not
//! starting a pane that should not exist, so an unreadable epoch must never
//! silently wipe every transcript in the company on the next respawn.

use rusqlite::Transaction;

use crate::error::Refusal;
use crate::isotime::{iso_millis, parse_iso_millis};
use crate::store::session_epoch_rows::SessionEpoch;
use crate::ChiefdError;

/// The stamp did not carry a usable instant.
pub const INVALID_EPOCH: &str = "invalid-session-epoch";

/// The current epoch in epoch-millis, or `0` when there is none.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure. A row that exists but cannot be
/// interpreted is NOT an error: it resolves to `0`, per the fail-open rule.
pub fn epoch_ms(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
) -> Result<i64, ChiefdError> {
    let Some(current) = crate::store::session_epoch_rows::reconstruct(tx, row_slug, company_slug)?
    else {
        return Ok(0);
    };
    Ok(parse_iso_millis(&current.epoch_at).unwrap_or(0))
}

/// Stamp the epoch. It only ever moves forward.
///
/// The monotonic maximum is taken against the stored value inside the same
/// transaction, so a stamp issued from a lagging clock can never un-clear a
/// boot that already happened.
///
/// # Errors
/// [`INVALID_EPOCH`] when `epoch_at` is not a timestamp;
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn stamp(
    tx: &Transaction<'_>,
    row_slug: &str,
    company_slug: &str,
    epoch_at: &str,
    reason: &str,
) -> Result<SessionEpoch, ChiefdError> {
    let requested = parse_iso_millis(epoch_at).ok_or_else(|| {
        ChiefdError::from(Refusal::new(
            INVALID_EPOCH,
            format!("Session epoch for '{company_slug}' has an invalid time"),
        ))
    })?;
    let current = epoch_ms(tx, row_slug, company_slug)?;
    let next = SessionEpoch {
        version: 1,
        organization: company_slug.to_string(),
        epoch_at: iso_millis(current.max(requested)),
        reason: reason.to_string(),
        extra: std::collections::BTreeMap::new(),
    };
    crate::store::session_epoch_rows::publish(tx, row_slug, company_slug, &next)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unparseable_stamp_is_refused_before_anything_is_written() {
        // The parse is the first thing `stamp` does, so this exercise needs no
        // database: reaching SQL at all would already be the defect.
        assert!(parse_iso_millis("not-a-time").is_none());
        let refusal = Refusal::new(INVALID_EPOCH, "x");
        assert_eq!(refusal.code, INVALID_EPOCH);
    }

    #[test]
    fn the_monotonic_maximum_keeps_the_later_instant() {
        let stored = parse_iso_millis("2026-08-07T00:10:00.000Z").expect("parse");
        let stale = parse_iso_millis("2026-08-07T00:00:00.000Z").expect("parse");
        assert_eq!(iso_millis(stored.max(stale)), "2026-08-07T00:10:00.000Z");
    }

    #[test]
    fn a_newer_stamp_moves_the_epoch_forward() {
        let stored = parse_iso_millis("2026-08-07T00:00:00.000Z").expect("parse");
        let newer = parse_iso_millis("2026-08-07T01:00:00.000Z").expect("parse");
        assert_eq!(iso_millis(stored.max(newer)), "2026-08-07T01:00:00.000Z");
    }

    #[test]
    fn no_stored_epoch_reads_as_zero_and_never_blocks_a_resume() {
        let stored: i64 = 0;
        let requested = parse_iso_millis("2026-08-07T00:00:00.000Z").expect("parse");
        assert_eq!(stored.max(requested), requested);
    }
}
