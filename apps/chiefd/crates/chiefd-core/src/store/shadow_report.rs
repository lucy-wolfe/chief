//! The shadow-diff TYPE SURFACE (org-data-normalization P0, N9) — the single
//! source of truth every store slice's `shadow_diff_<store>` builds on.
//!
//! A shadow-diff proves ZERO DATA LOSS for one store: blob → rows →
//! reconstructed aggregate, then a field-by-field classification. Each field is
//! [`Disposition::Matched`], [`Disposition::Derived`] (a value the reconstruct
//! recomputes from a constant / process identity / another row / the feed,
//! asserted to reproduce the blob value), [`Disposition::ExpectedDropped`] (a
//! blob value provably preserved elsewhere, or an insignificant dimension like
//! deterministic child ordering), or [`Disposition::Lost`] (present in the blob,
//! NOT representable and NOT preserved — the violation the verifier exists to
//! catch). Per Fable's strictness rider a `Lost` field or an unmodeled LIVE key
//! is a LOUD failure recorded in [`ShadowReport::loud_failures`], NEVER a silent
//! drop.
//!
//! This module owns ONLY the types + the builder API. Each store's
//! `backfill_<store>` / `shadow_diff_<store>` FUNCTIONS live in that store's own
//! slice and construct a [`ShadowReport`] through [`ShadowReport::new`] +
//! [`ShadowReport::record`] / [`ShadowReport::record_loud`].

/// How one field of a reconstructed aggregate relates to the source blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Round-tripped byte-for-byte through the rows.
    Matched,
    /// Not stored as itself; recomputed on read from a constant, the process
    /// identity, another row, or the `org_events` feed. `proof` states the
    /// derivation and that it was verified to reproduce the blob value.
    Derived {
        /// The derivation, verified to reproduce the blob value.
        proof: String,
    },
    /// Present in the blob and provably preserved elsewhere (e.g. moved to its
    /// own store/table), or an insignificant dimension (deterministic child
    /// ordering with no positional consumer). `where_now` names the survivor /
    /// justification. A KNOWN, intentional drop.
    ExpectedDropped {
        /// Where the value now lives, or why the drop is safe.
        where_now: String,
    },
    /// Present in the blob, NOT representable in the rows, and NOT provably
    /// preserved anywhere. The zero-loss violation the verifier exists to catch.
    Lost {
        /// The blob value (or a description of it) that was lost.
        blob_value: String,
    },
}

/// One field's disposition in a shadow-diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiff {
    /// Dotted path, e.g. `people.ada.activation`.
    pub path: String,
    /// Its classification.
    pub disposition: Disposition,
}

/// The per-store zero-loss report. `loud_failures` is empty iff the store
/// migrated with provable zero loss.
#[derive(Debug, Clone, Default)]
pub struct ShadowReport {
    /// The `org_documents` store family, e.g. `"org-manifest"`.
    pub store: String,
    /// Rows written by the backfill (entity granularity).
    pub row_count: usize,
    /// Every field's disposition (the audit trail; includes the matched ones so
    /// the report proves the search could have returned a positive).
    pub fields: Vec<FieldDiff>,
    /// Zero-loss violations: `Lost` fields and unmodeled LIVE keys. Non-empty =>
    /// the store is NOT safe to cut over.
    pub loud_failures: Vec<String>,
}

impl ShadowReport {
    /// A fresh report for `store`, ready to accumulate field dispositions.
    #[must_use]
    pub fn new(store: impl Into<String>) -> Self {
        Self { store: store.into(), ..Default::default() }
    }

    /// Record one field's disposition. A [`Disposition::Lost`] is ALSO appended
    /// to [`Self::loud_failures`] — a lost field can never be silently swallowed.
    pub fn record(&mut self, path: impl Into<String>, disposition: Disposition) {
        let path = path.into();
        if let Disposition::Lost { blob_value } = &disposition {
            self.loud_failures
                .push(format!("LOST {path}: blob carried {blob_value:?}, absent after round-trip"));
        }
        self.fields.push(FieldDiff { path, disposition });
    }

    /// Record a LOUD failure that is not tied to a single field disposition —
    /// e.g. an unmodeled LIVE key the publish path rejected wholesale.
    pub fn record_loud(&mut self, message: impl Into<String>) {
        self.loud_failures.push(message.into());
    }

    /// True iff every field matched, derived, or expected-dropped — no loss.
    #[must_use]
    pub fn zero_loss(&self) -> bool {
        self.loud_failures.is_empty()
    }

    /// The LOUD-failure lines (`Lost` fields + unmodeled keys). Empty iff
    /// [`Self::zero_loss`].
    #[must_use]
    pub fn loud_failures(&self) -> &[String] {
        &self.loud_failures
    }

    /// `(matched, derived, expected_dropped, lost)` field counts for summaries.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut c = (0, 0, 0, 0);
        for f in &self.fields {
            match f.disposition {
                Disposition::Matched => c.0 += 1,
                Disposition::Derived { .. } => c.1 += 1,
                Disposition::ExpectedDropped { .. } => c.2 += 1,
                Disposition::Lost { .. } => c.3 += 1,
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_with_no_loss_is_zero_loss() {
        let mut r = ShadowReport::new("org-manifest");
        r.record("slug", Disposition::Derived { proof: "process slug".into() });
        r.record("name", Disposition::Matched);
        r.record("tools", Disposition::ExpectedDropped { where_now: "order deterministic".into() });
        assert!(r.zero_loss());
        assert!(r.loud_failures().is_empty());
        assert_eq!(r.counts(), (1, 1, 1, 0));
    }

    #[test]
    fn a_lost_field_is_loud_and_counted() {
        let mut r = ShadowReport::new("org-manifest");
        r.record("people.ada.activation", Disposition::Lost { blob_value: "on-demand".into() });
        assert!(!r.zero_loss());
        assert_eq!(r.counts(), (0, 0, 0, 1));
        assert!(
            r.loud_failures()[0].contains("activation")
                && r.loud_failures()[0].contains("on-demand")
        );
    }

    #[test]
    fn record_loud_carries_a_non_field_failure() {
        let mut r = ShadowReport::new("org-manifest");
        r.record_loud("UNMODELED KEYS rejected: extra.legacyFrozenMirror");
        assert!(!r.zero_loss());
        assert!(r.loud_failures()[0].contains("UNMODELED"));
    }
}
