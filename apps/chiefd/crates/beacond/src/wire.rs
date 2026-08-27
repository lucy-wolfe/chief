//! Wire types, camelCase, matching chiefd's convention.
//!
//! # A company is named by its DIRECTORY
//!
//! Every request below carries `dir`, the canonical absolute path of the
//! directory the operator ran `chief` in. That path IS the company: two
//! directories may hold companies with the same slug, and the registry must
//! be able to say so. The slug rides along as a display word and names
//! nothing.
//!
//! `dir` is the caller's canonical spelling and beacond compares it byte for
//! byte. Canonicalising here would be a second opinion about a path only the
//! caller can resolve (its own cwd, possibly already deleted), so the
//! contract is stated instead: send the canonical path, or key one company
//! two ways.

use std::time::{SystemTime, UNIX_EPOCH};

/// One company. The location fields are `Option` because a company that is
/// not running has no location — `skip_serializing_if` keeps them off the
/// wire entirely rather than sending `null`, so the TS `CompanyRow`'s
/// optional fields (E10-S4) are simply absent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Company {
    /// **The identity.** The canonical absolute directory this company
    /// occupies. One row per directory, forever.
    pub dir: String,
    /// The directory-derived company key, `sha256(dir)[..12]`.
    ///
    /// SERVED, never computed here. beacond records the identity its caller
    /// minted and hands it back to every reader, so the composite key that
    /// was recomputed independently in nine places has exactly one producer
    /// and one field on the wire. A registry that hashed the path itself
    /// would be a second producer of the same answer.
    pub key: String,
    /// The company's DISPLAY name. Not an identity: two directories may hold
    /// companies with the same slug, and both rows are legitimate.
    pub slug: String,
    /// When the COMPANY was created. ISO-8601 millis, e.g.
    /// "2026-08-03T12:00:00.000Z".
    pub registered_at: String,
    /// Base URL of the chiefd currently serving this company, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Port of the chiefd currently serving this company, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// OS pid of the chiefd currently serving this company, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
    /// Host the daemon registered from, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// When the location was last refreshed, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
}

/// The location columns a daemon publishes. Never partially applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The company directory this location belongs to.
    pub dir: String,
    /// Base URL of the serving chiefd.
    pub url: String,
    /// Port of the serving chiefd.
    pub port: u16,
    /// OS pid of the serving chiefd.
    pub pid: i64,
    /// Host the daemon registered from.
    pub hostname: String,
}

/// Body of `POST /v1/company/create`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCompanyRequest {
    /// The canonical absolute directory the company occupies.
    pub dir: String,
    /// `sha256(dir)[..12]`, minted by the caller and recorded verbatim.
    pub key: String,
    /// The company's display name.
    pub slug: String,
}

/// Body of `POST /v1/company/delete`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteCompanyRequest {
    /// The directory whose company row is removed.
    pub dir: String,
}

/// Body of `POST /v1/register`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    /// The company directory registering a location.
    pub dir: String,
    /// Base URL of the registering chiefd.
    pub url: String,
    /// Port of the registering chiefd.
    pub port: u16,
    /// OS pid of the registering chiefd.
    pub pid: i64,
    /// Host the registering daemon is running on.
    pub hostname: String,
}

/// Body of `POST /v1/heartbeat`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRequest {
    /// The company directory refreshing its location.
    pub dir: String,
    /// The caller's pid, fenced against the recorded one.
    pub pid: i64,
}

/// Body of `POST /v1/deregister`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeregisterRequest {
    /// The company directory clearing its location.
    pub dir: String,
    /// The caller's pid, fenced against the recorded one.
    pub pid: i64,
}

/// Query string of `GET /v1/lookup`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LookupQuery {
    /// The company directory to look up.
    pub dir: String,
}

/// Is `key` the shape a directory hash has: twelve lowercase hex characters?
///
/// A SHAPE check, deliberately not a re-derivation. beacond cannot recompute
/// `sha256(dir)[..12]` without becoming a second producer of the identity,
/// but it can refuse a caller that sent a slug, a path, or an empty string
/// where the key belongs — which is the whole class of mistake a registry
/// can catch on its own.
#[must_use]
pub fn is_company_key(key: &str) -> bool {
    key.len() == 12
        && key.chars().all(|character| character.is_ascii_hexdigit())
        && !key.chars().any(|character| character.is_ascii_uppercase())
}

/// `%Y-%m-%dT%H:%M:%S%.3fZ` for a `SystemTime`, computed locally (chiefd-core
/// is not a dependency here) — matches `chiefd_core::isotime::iso_millis`'s
/// shape byte-for-byte so the timestamp format cannot drift from the rest of
/// the fleet.
#[must_use]
pub fn iso_millis(time: SystemTime) -> String {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let total_millis = duration.as_millis();
    let seconds = total_millis / 1000;
    let millis = total_millis % 1000;
    let (year, month, day, hour, minute, second) = civil_from_unix_seconds(seconds as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Now, in the same format as [`iso_millis`].
#[must_use]
pub fn now_iso_millis() -> String {
    iso_millis(SystemTime::now())
}

/// Civil (Gregorian) date/time from a Unix timestamp in seconds, UTC.
/// Howard Hinnant's `civil_from_days` algorithm (public domain), the standard
/// allocation-free way to do this without a chrono dependency.
#[allow(clippy::many_single_char_names)]
fn civil_from_unix_seconds(unix_seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix_seconds.div_euclid(86400);
    let secs_of_day = unix_seconds.rem_euclid(86400);
    let hour = (secs_of_day / 3600) as u32;
    let minute = ((secs_of_day % 3600) / 60) as u32;
    let second = (secs_of_day % 60) as u32;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };

    (year, m, d, hour, minute, second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_format_for_a_fixed_epoch_millisecond() {
        // 2026-08-03T12:00:00.000Z
        let epoch_millis: u64 = 1_785_758_400_000;
        let time = UNIX_EPOCH + std::time::Duration::from_millis(epoch_millis);
        assert_eq!(iso_millis(time), "2026-08-03T12:00:00.000Z");
    }

    #[test]
    fn preserves_milliseconds() {
        let epoch_millis: u64 = 1_785_758_400_123;
        let time = UNIX_EPOCH + std::time::Duration::from_millis(epoch_millis);
        assert_eq!(iso_millis(time), "2026-08-03T12:00:00.123Z");
    }

    /// The key check accepts the exact shape `sha256(dir)[..12]` produces and
    /// nothing else — a slug, a path or a truncated digest is a caller that
    /// filled the wrong field.
    #[test]
    fn the_company_key_shape_is_twelve_lowercase_hex_characters() {
        for good in ["0123456789ab", "deadbeefcafe", "000000000000"] {
            assert!(is_company_key(good), "{good} is a directory hash");
        }
        for bad in ["", "0123456789a", "0123456789abc", "0123456789AB", "acme", "/work/acme"] {
            assert!(!is_company_key(bad), "{bad} is not a directory hash");
        }
    }
}
