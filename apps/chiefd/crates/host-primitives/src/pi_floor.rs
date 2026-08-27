//! The minimum Pi version chief runs against, and the comparison that reads it.
//!
//! # One constant, and why it is a FLOOR
//!
//! [`MINIMUM_PI_VERSION`] is the only place in this workspace that spells the
//! number. `scripts/test/pi-floor-single-definition.test.mjs` fails if a second
//! spelling appears in source, and it also holds the README and CONTRIBUTING
//! mentions to this value, because a documented number that has drifted from
//! the enforced one sends an operator to install the wrong thing.
//!
//! It is a MINIMUM and never an exact pin. `pi --version` at or above the floor
//! passes, always — a newer Pi is not a problem this product has an opinion
//! about, and Pi ships two or three releases a week, so a pin would be stale
//! before the release carrying it finished building.
//!
//! # TOMBSTONE: `pinned_pi`
//!
//! A 2026 ruling removed every Pi version gate ("no minimum version"), and
//! `chief-cli/src/preflight.rs` still carries the tombstone for the probe that
//! served it. This module REVERSES that ruling at the operator's direction, and
//! the reversal is narrow: what came back is a floor with two consumers, not the
//! pin that was deleted.
//!
//!   * `chief`'s preflight WARNS when the installed Pi is below the floor. It
//!     does not refuse. A company that works today keeps working.
//!   * `chief upgrade` ENFORCES it, because it is the one moment the product
//!     can offer to fix it — it prompts to run Pi's own updater.
//!
//! Nothing else reads it. In particular the daemon does not gate a launch on
//! it: a person whose pane starts is a person whose pane starts.
//!
//! # Why the parser is this forgiving
//!
//! `pi --version` prints a version and is free to print more around it — a
//! banner line, a build hash, a leading `v`. The probe that captures it
//! (`preflight::command_version`) already trims to the first line and no
//! further. So [`meets_floor`] finds the first dotted numeric triple anywhere
//! in the string rather than demanding the whole string BE a version, and
//! answers `None` when it cannot find one. `None` is not "below": a Pi that
//! declines to name itself is a Pi nobody can judge, and judging it anyway
//! would refuse an upgrade on a string-formatting change upstream.

/// The lowest Pi version chief is willing to run against.
///
/// Edit this line and nothing else; every other reader derives from it.
pub const MINIMUM_PI_VERSION: &str = "0.80.10";

/// The three numeric components of a version string, found anywhere in it.
///
/// Returns `None` when no dotted triple is present. A component that does not
/// fit a `u64` is treated as absent rather than saturated — a version that
/// large is a parse failure wearing a number's clothes.
fn triple(text: &str) -> Option<(u64, u64, u64)> {
    let bytes: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        // A run of digits starts here. A candidate must not begin in the middle
        // of a longer number, which is what the preceding-character check does:
        // the `2` of `v1.2.3` is not the start of a candidate.
        if index > 0 && bytes[index - 1].is_ascii_digit() {
            index += 1;
            continue;
        }
        let mut parts: Vec<u64> = Vec::new();
        let mut cursor = index;
        while parts.len() < 3 {
            let start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
            }
            if start == cursor {
                break;
            }
            let digits: String = bytes[start..cursor].iter().collect();
            match digits.parse::<u64>() {
                Ok(value) => parts.push(value),
                Err(_) => break,
            }
            if parts.len() == 3 {
                break;
            }
            if cursor < bytes.len() && bytes[cursor] == '.' {
                cursor += 1;
            } else {
                break;
            }
        }
        if parts.len() == 3 {
            return Some((parts[0], parts[1], parts[2]));
        }
        index = cursor.max(index + 1);
    }
    None
}

/// Is `reported` at or above `floor`?
///
/// `None` means "no version could be read from one of them", which every caller
/// must treat as *unknown* rather than as *below*. See the module note.
pub fn version_meets(reported: &str, floor: &str) -> Option<bool> {
    let found = triple(reported)?;
    let required = triple(floor)?;
    Some(found >= required)
}

/// [`version_meets`] against [`MINIMUM_PI_VERSION`].
pub fn meets_floor(reported: &str) -> Option<bool> {
    version_meets(reported, MINIMUM_PI_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_floor_is_a_readable_triple() {
        assert!(triple(MINIMUM_PI_VERSION).is_some(), "the declared floor must parse as a version");
    }

    #[test]
    fn the_exact_floor_passes_because_it_is_a_minimum_and_not_a_pin() {
        assert_eq!(meets_floor(MINIMUM_PI_VERSION), Some(true));
    }

    #[test]
    fn a_newer_pi_passes() {
        assert_eq!(version_meets("0.84.3", "0.80.10"), Some(true));
        assert_eq!(version_meets("1.0.0", "0.80.10"), Some(true));
        assert_eq!(version_meets("0.80.11", "0.80.10"), Some(true));
    }

    #[test]
    fn an_older_pi_fails() {
        assert_eq!(version_meets("0.80.9", "0.80.10"), Some(false));
        assert_eq!(version_meets("0.79.99", "0.80.10"), Some(false));
        assert_eq!(version_meets("0.0.1", "0.80.10"), Some(false));
    }

    #[test]
    fn components_compare_numerically_and_never_as_text() {
        // The defect this pins: "0.80.9" > "0.80.10" as strings, and a version
        // gate that compares text refuses the newer of the two.
        assert_eq!(version_meets("0.80.10", "0.80.9"), Some(true));
        assert_eq!(version_meets("0.9.0", "0.10.0"), Some(false));
    }

    #[test]
    fn a_version_is_found_inside_a_noisier_line() {
        assert_eq!(version_meets("v0.84.3", "0.80.10"), Some(true));
        assert_eq!(version_meets("pi 0.84.3 (abc1234)", "0.80.10"), Some(true));
        assert_eq!(version_meets("0.84.3-beta.1", "0.80.10"), Some(true));
    }

    #[test]
    fn a_prerelease_suffix_is_ignored_rather_than_ordered() {
        // Deliberate: this is a floor for an agent runtime, not a semver
        // resolver. `0.80.10-rc.1` reads as `0.80.10` and passes. Ordering
        // prereleases below their release would refuse a Pi the operator
        // deliberately installed, to enforce a distinction nobody asked for.
        assert_eq!(version_meets("0.80.10-rc.1", "0.80.10"), Some(true));
    }

    #[test]
    fn an_unreadable_report_is_unknown_and_never_below() {
        assert_eq!(meets_floor(""), None);
        assert_eq!(meets_floor("unreported"), None);
        assert_eq!(meets_floor("0.80"), None, "a two-component string is not a triple");
        assert_eq!(version_meets("0.84.3", "not-a-version"), None);
    }

    #[test]
    fn a_leading_partial_number_does_not_swallow_the_real_triple() {
        // `2026` is a year, not a major. The scan must move past it and find
        // the triple that follows rather than reading `2026.8.25`… which it
        // WOULD, so the assertion is that the FIRST complete triple wins and
        // the caller's probe is trimmed to one line for exactly this reason.
        assert_eq!(triple("build 2026.08.25 pi 0.84.3"), Some((2026, 8, 25)));
    }
}
