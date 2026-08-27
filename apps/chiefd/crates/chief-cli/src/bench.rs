//! **Measuring the thing the operator feels: a click, to pixels.**
//!
//! Stage 0 of that work. Everything here exists because three
//! separate claims about this product's speed were made on 2026-08-15 and all
//! three were wrong in the same way — they measured a COMMAND BEING ISSUED and
//! called it visible.
//!
//! * "department click to layout: 1–37ms" measured `sidebar.window.laid`, which
//!   fires a median **2ms BEFORE** the wake it was said to complete. The honest
//!   endpoint for the same gesture was 5,636ms at the median.
//! * A benchmark passed on an empty fixture while the live company regressed
//!   threefold. The fixture had no accumulated history, and the cost that
//!   regressed is proportional to exactly that.
//! * No number could be attributed to a specific click at all, because no
//!   correlator existed (`crate::sidebar::gesture` is the other half of this
//!   stage).
//!
//! # The two rules this harness is built to obey
//!
//! **1. VISIBLE MEANS TMUX'S GRID CHANGED.** The only thing an attached
//! terminal is ever sent is the cell grid tmux holds for the ACTIVE window, and
//! `capture-pane` returns exactly that grid. So the harness's clock stops when
//! the grid the operator is looking at differs from the grid at click time — not
//! when a `select-window` was issued, not when a pane was created, not when a
//! POST returned. A pane that exists and has printed nothing is not visible, and
//! [`click::Glass::satisfies`] refuses it.
//!
//! **2. A NUMBER WITHOUT ITS COMPANY'S WEIGHT IS NOT A RESULT.** Every report
//! states the size of the database the click was measured against
//! ([`Weight`]), because the whole of the fixture defect was a number quoted
//! without the fact that made it meaningless. See [`Weight`]'s own doc for how
//! this would have caught the 3x.
//!
//! # What is deliberately NOT here
//!
//! A pass/fail threshold in CI. `examples/tmux_transport_bench.rs` states the
//! rule this follows: a timing assertion in CI is a flake waiting to happen.
//! The tests in this module pin the RULES — what counts as visible, how a
//! percentile is taken, what a report must state — and the timings are reported
//! to a person.

pub mod click;

use std::path::{Path, PathBuf};

/// The weight of the company a measurement was taken against.
///
/// # The defect this exists to make impossible
///
/// A benchmark passed on a fixture with an empty `org_events` while the
/// operator's own company — 97MB, 322,329 rows — regressed threefold, and the
/// result shipped. The two runs produced comparable-looking numbers and nothing
/// on either said which company it was about.
///
/// The cost that hid there is proportional to accumulated history and to
/// nothing else: `in_transaction` deep-cloned the whole `Ledgers` — every
/// mailbox body and effect row the company had ever accumulated — per read. On
/// a fixture with no history that clone is free, and the same code on a real
/// company is a `desired` read at a 925ms median. So a fixture cannot fail the
/// benchmark that a real company fails, and a fixture number that does not say
/// it is a fixture number is worse than no number.
///
/// Printed on every report, beside every percentile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Weight {
    /// The company's durable store, `<dir>/.chief/db/chief.db`.
    pub database: PathBuf,
    /// Its size in bytes. `None` when the file could not be read, which is
    /// reported as an unknown weight and never as a zero one — "this company
    /// has no history" and "I could not tell" are different claims.
    pub bytes: Option<u64>,
}

impl Weight {
    /// Read the weight of the store at `database`.
    ///
    /// The PATH is given rather than composed from a root and a slug: this half
    /// is pure measurement and where a company's store lives is
    /// `chief_cli::paths`' single answer, which the binary holds.
    #[must_use]
    pub fn of(database: &Path) -> Self {
        let bytes = std::fs::metadata(database).ok().map(|meta| meta.len());
        Self { database: database.to_path_buf(), bytes }
    }

    /// A weight stated outright. Tests use it to pin the RENDERING without
    /// writing a multi-megabyte file, which they may not do anyway: the
    /// filesystem belongs to the host executor and `clippy.toml` says so.
    #[must_use]
    pub const fn stated(database: PathBuf, bytes: Option<u64>) -> Self {
        Self { database, bytes }
    }

    /// One line, stated before any percentile is.
    #[must_use]
    pub fn line(&self) -> String {
        match self.bytes {
            Some(bytes) => format!(
                "company store: {} ({:.1} MB). Every number below is about a company THIS \
                 SIZE — a fixture with no accumulated history cannot reproduce it.",
                self.database.display(),
                bytes as f64 / (1024.0 * 1024.0),
            ),
            None => format!(
                "company store: {} — UNREADABLE. The numbers below cannot be compared with \
                 any other run, because nothing here says what company they are about.",
                self.database.display()
            ),
        }
    }
}

/// The nearest-rank percentile of an already-sorted sample.
///
/// Nearest-rank, and not an interpolating definition, because that is what
/// every percentile already quoted in the design record is: the baseline
/// table, the route medians and the 5,636ms/16.5s cold-click figures were all
/// taken this way. Two definitions of p95 in one plan is a plan whose numbers
/// cannot be compared with each other.
///
/// `None` for an empty sample: a percentile of nothing is not zero.
#[must_use]
pub fn percentile(sorted: &[u64], percent: u32) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (f64::from(percent) / 100.0 * sorted.len() as f64).ceil().max(1.0);
    let index = (rank as usize).min(sorted.len()) - 1;
    sorted.get(index).copied()
}

/// One gesture's measurements, gathered over every repeat of it.
#[derive(Debug, Clone, Default)]
pub struct Samples {
    /// Click to the first changed frame, in microseconds.
    pub first_change_us: Vec<u64>,
    /// Click to the frame that is CORRECT for the gesture, in microseconds.
    pub visible_us: Vec<u64>,
    /// Repeats where the correct frame never arrived inside the budget.
    pub never_visible: usize,
}

impl Samples {
    /// Fold one repeat in.
    pub fn record(&mut self, outcome: &click::Measurement) {
        if let Some(first) = outcome.first_change_us {
            self.first_change_us.push(first);
        }
        match outcome.visible_us {
            Some(visible) => self.visible_us.push(visible),
            None => self.never_visible += 1,
        }
    }

    /// `n`, median, p95 and max of one series, as the plan's tables state them.
    #[must_use]
    fn row(label: &str, series: &[u64]) -> String {
        let mut sorted = series.to_vec();
        sorted.sort_unstable();
        let render = |value: Option<u64>| {
            value.map_or_else(|| "-".to_owned(), |us| format!("{:.1}ms", us as f64 / 1000.0))
        };
        format!(
            "  {label:<14} n={:<4} med={:<9} p95={:<9} max={}",
            sorted.len(),
            render(percentile(&sorted, 50)),
            render(percentile(&sorted, 95)),
            render(sorted.last().copied()),
        )
    }

    /// The two lines this gesture contributes to the report.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            Self::row("first-change", &self.first_change_us),
            Self::row("visible", &self.visible_us),
        ];
        if self.never_visible > 0 {
            // NOT folded into the percentiles as a large number, and not
            // dropped. A gesture that never arrived is a different fact from a
            // slow one, and a p95 computed over only the ones that answered is
            // the flattering half of the truth unless the other half is stated
            // beside it.
            lines.push(format!(
                "  NEVER VISIBLE  {} of {} clicks never reached a correct frame inside the \
                 budget; the percentiles above are over the ones that did",
                self.never_visible,
                self.never_visible + self.visible_us.len()
            ));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::{percentile, Samples, Weight};
    use crate::bench::click::Measurement;
    use std::path::Path;

    /// Nearest-rank, matching every percentile already quoted in the plan.
    /// An interpolating definition would give 5.5 for the p50 below and make
    /// this harness's numbers incomparable with the baseline it exists to
    /// reproduce.
    #[test]
    fn percentiles_are_nearest_rank() {
        let sorted = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&sorted, 50), Some(5), "rank ceil(0.50*10)=5 -> the 5th value");
        assert_eq!(percentile(&sorted, 95), Some(10), "rank ceil(0.95*10)=10 -> the 10th value");
        assert_eq!(percentile(&sorted, 100), Some(10));
        assert_eq!(percentile(&[7], 95), Some(7), "one sample is its own p95");
    }

    /// A percentile of nothing is not zero. A harness that reported 0ms for a
    /// gesture it never managed to measure would be the fixture defect again,
    /// in a different costume.
    #[test]
    fn an_empty_sample_has_no_percentile() {
        assert_eq!(percentile(&[], 50), None);
        assert_eq!(percentile(&[], 95), None);
    }

    /// A click that never produced a correct frame must be COUNTED, not
    /// silently dropped: percentiles over the successes alone are the
    /// flattering half of the truth.
    #[test]
    fn clicks_that_never_became_visible_are_reported_beside_the_percentiles() {
        let mut samples = Samples::default();
        samples.record(&Measurement {
            first_change_us: Some(4_000),
            visible_us: Some(8_000),
            samples: 9,
        });
        samples.record(&Measurement {
            first_change_us: Some(5_000),
            visible_us: None,
            samples: 90,
        });

        let lines = samples.lines();
        assert!(lines.iter().any(|line| line.contains("visible") && line.contains("n=1")));
        assert!(
            lines.iter().any(|line| line.contains("NEVER VISIBLE") && line.contains("1 of 2")),
            "the unanswered click must be stated: {lines:?}"
        );
    }

    /// THE HONESTY RULE, pinned. An unreadable store is reported as unknown and
    /// never as an empty company — the difference between the two is exactly
    /// what let a fixture result be quoted as a live one.
    #[test]
    fn an_unreadable_store_is_an_unknown_weight_and_never_a_zero_one() {
        let weight = Weight::of(Path::new("/nonexistent/.chief/db/chief.db"));
        assert_eq!(weight.bytes, None);
        let line = weight.line();
        assert!(line.contains("UNREADABLE"), "{line}");
        assert!(!line.contains("0.0 MB"), "an unknown weight must never render as an empty one");
    }

    /// The weight is the company's own store, named and sized, so a fixture run
    /// and a live run can be told apart at a glance. 97MB is the operator's own
    /// company; a fixture is a few tens of kilobytes.
    #[test]
    fn the_weight_names_the_database_the_clicks_were_measured_against() {
        let weight = Weight::stated(
            Path::new("/work/tribes-capital/.chief/db/chief.db").to_path_buf(),
            Some(97 * 1024 * 1024),
        );
        let line = weight.line();
        assert!(line.contains("97.0 MB"), "{line}");
        assert!(line.contains("/work/tribes-capital/.chief/db/chief.db"), "{line}");
        assert!(
            line.contains("a fixture with no accumulated history cannot reproduce it"),
            "the report must say what the size is FOR: {line}"
        );
    }

    /// The weight names the store it was ASKED about, so pointing the harness
    /// at a throwaway fixture and at the operator's real company yields two
    /// obviously different reports.
    ///
    /// The path is given rather than composed here: where a company's store
    /// lives is `chief_cli::paths`' single answer, and a second derivation
    /// inside a measurement helper is how a bench comes to report the size of a
    /// file nothing else reads.
    #[test]
    fn the_weight_names_the_store_it_was_asked_about() {
        let weight = Weight::of(Path::new("/work/acme/.chief/db/chief.db"));
        assert_eq!(weight.database, Path::new("/work/acme/.chief/db/chief.db"));
    }
}
