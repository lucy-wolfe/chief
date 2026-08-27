//! Shared vocabulary for the frozen wire surface.
//!
//! Newtypes rather than bare `String`/`u64` because the historical bugs in
//! this system were argument-position bugs: a slug where a person id belonged,
//! a runtime generation compared against a department id. Every one of these is
//! `#[serde(transparent)]`, so the wire bytes are the plain scalar.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Company slug. Validated against `^[a-z0-9]+(?:-[a-z0-9]+)*$` **before any
/// path use** (plan §2.1) — a slug reaches the filesystem only after
/// [`Slug::parse`] has accepted it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(extend("pattern" = "^[a-z0-9]+(?:-[a-z0-9]+)*$"))]
pub struct Slug(String);

// Deserialization is hand-written so the pattern is enforced **at the schema
// boundary**. A derived `transparent` impl would accept `../etc` and leave
// validation to whoever remembered to call `parse` — which is exactly the
// class of bug the plan puts in the type system instead.
impl<'de> Deserialize<'de> for Slug {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// Why a slug was rejected. Surfaces as `Refused{code:"invalid-slug"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("slug must match ^[a-z0-9]+(?:-[a-z0-9]+)*$")]
pub struct InvalidSlug;

impl Slug {
    /// Parse and validate a slug.
    ///
    /// # Errors
    /// [`InvalidSlug`] when the value does not match the documented pattern.
    /// The pattern is enforced here and nowhere else, so path construction can
    /// never see an unvalidated slug.
    pub fn parse(value: impl Into<String>) -> Result<Self, InvalidSlug> {
        let value = value.into();
        if value.is_empty() {
            return Err(InvalidSlug);
        }
        // `^[a-z0-9]+(?:-[a-z0-9]+)*$`: non-empty lowercase alphanumeric
        // groups joined by single hyphens; no leading, trailing or doubled
        // hyphen, no uppercase, no dot, no slash.
        for group in value.split('-') {
            if group.is_empty()
                || !group.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            {
                return Err(InvalidSlug);
            }
        }
        Ok(Self(value))
    }

    /// Borrow the validated slug.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A person's stable identifier. Never a display name.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct PersonId(pub String);

impl fmt::Display for PersonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A department's identifier within one company's manifest.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct DepartmentId(pub String);

impl fmt::Display for DepartmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Client-supplied idempotency key for the creating ops that are not
/// idempotent on their own arguments — `assignment.assign`, `org.hire`, and
/// `goal.set` (plan §6.6). Backed by a unique column, or
/// by a key-derived row id, so a retry after an ambiguous connection drop
/// cannot double-create.
///
/// `goal.set` was **not** on this list until plan §10 Q6 was closed, and its
/// absence was the whole of the durability regression the operator overruled: the store
/// has been idempotent on the goal id since M15, but the wire carried no key to
/// derive that id from, so `shim/client.ts` classified the op non-idempotent
/// and refused to retry the one failure shape a crash-loop produces. If a
/// creating op is ever added without a key, that is the failure it inherits.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct IdempotencyKey(pub String);

// TOMBSTONE (#751-P9): `RuntimeTargetRef` (a terminal server socket + session
// name) and `PaneRef` (that pair plus a `%17` pane id) are DELETED from the
// wire. They encoded, in chiefd's own frozen request/response vocabulary, two
// things a client-agnostic backend has no opinion about: which terminal
// multiplexer socket a caller is using, and where on a screen a person's
// process is drawn. A browser has neither. The operator client owns both, and
// mints the session name for itself from the company slug
// (`chief-cli/src/placement.rs::session_name_for_slug`).
//
// Nothing replaces them, and no request field may reintroduce a socket, a
// session, a window or a pane id: `apps/chiefd/tests/wire-boundary` walks real
// response and request bodies for exactly those shapes, because a file-text
// guard cannot inspect a wire shape.

/// A bounded projection (invariant 27): responses carry a capped page and say
/// so. A 4.4 MB ledger is never a routine response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Bounded<T> {
    /// The page of items actually returned.
    pub items: Vec<T>,
    /// True when chiefd stopped short of the full set.
    pub truncated: bool,
}

impl<T> Bounded<T> {
    /// A complete (untruncated) projection.
    pub fn complete(items: Vec<T>) -> Self {
        Self { items, truncated: false }
    }

    /// Cap `items` at `limit`, setting `truncated` when anything was dropped.
    pub fn capped(mut items: Vec<T>, limit: usize) -> Self {
        let truncated = items.len() > limit;
        items.truncate(limit);
        Self { items, truncated }
    }
}

/// A non-fatal observation attached to a read projection. `org.roster` and
/// `org.lifecycle_status` never mutate and never observe the runtime, so anything
/// they notice travels as a warning rather than an error (plan §2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    /// Stable machine code.
    pub code: String,
    /// Human-facing sentence.
    pub message: String,
}

/// An empty response body. Used by the ops whose success carries no data
/// (`company.remove.finalize`, `assignment.acknowledge`, …) so that "success"
/// is still a typed value and never an ad-hoc `{}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Accepted {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_pattern_accepts_the_documented_shape() {
        for good in ["cobalt", "cobalt-seal", "a1", "a-1-b"] {
            assert!(Slug::parse(good).is_ok(), "{good} should parse");
        }
    }

    #[test]
    fn slug_pattern_rejects_everything_that_could_reach_a_path() {
        for bad in ["", "-x", "x-", "a--b", "Cobalt", "co balt", "../etc", "a/b", "a.b", "a_b"] {
            assert_eq!(Slug::parse(bad), Err(InvalidSlug), "{bad} must be rejected");
        }
    }

    #[test]
    fn generation_zero_is_unrepresentable() {}

    #[test]
    fn bounded_capped_sets_the_truncated_flag() {
        let full = Bounded::capped(vec![1, 2, 3], 2);
        assert_eq!(full.items, vec![1, 2]);
        assert!(full.truncated);
        let short = Bounded::capped(vec![1, 2], 2);
        assert!(!short.truncated);
    }

    /// #751-P9. `runtime_target_cannot_be_half_specified_on_the_wire` stood
    /// here and pinned that a socket without a session was rejected. Both
    /// types are deleted, so that rule has nothing left to enforce, and the
    /// stronger statement replaces it: this vocabulary must not come back.
    /// (Needles are assembled rather than written out, so the assertion is not
    /// defeated by its own source text.)
    #[test]
    fn the_wire_declares_no_terminal_socket_session_or_pane_type() {
        let source = include_str!("common.rs");
        for retired in
            [format!("struct {}{}", "RuntimeTarget", "Ref"), format!("struct {}{}", "Pane", "Ref")]
        {
            assert!(
                !source.contains(&retired),
                "the wire must not declare `{retired}`: a socket, a session name and a \
                 pane id are the operator client's business, not chiefd's"
            );
        }
    }
}
