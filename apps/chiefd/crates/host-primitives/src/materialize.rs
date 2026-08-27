//! The vocabulary of a materialization: what a plan publishes, and what
//! publishing it found.
//!
//! Both actuators materialize file trees and both described the result with
//! their own copy of these three types — the same fields, the same derives,
//! the same `deny_unknown_fields`. The copies still agree, which is the only
//! condition under which a move like this is free, and is why this one is
//! prevention rather than a fix: nothing here has drifted yet.
//!
//! The doc prose is `chief-cli`'s. The backend's said the same things about
//! the same fields in backend-only terms — the `host_actions` row, "a host
//! transaction" — and prose asserting a mechanism only one consumer has is
//! false for the other one.
//!
//! `files.rs`, which produces a [`DriftReport`] from a [`MaterializePlan`], is
//! NOT here: it is written against `HostErr`, which each crate still declares
//! for itself.

/// One file a materialization plan publishes.
///
/// Serializable because a caller may journal the **full plan** before touching
/// the filesystem: a recovery pass after a crash has nothing but that record to
/// work from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializeFile {
    /// Path relative to [`MaterializePlan::root`]. An entry that escapes the
    /// root is reported as a conflict and never written.
    pub relative_path: String,
    /// Exact desired content. Materialization is a convergence, not a patch:
    /// the plan states the whole file, which is what makes replay after a
    /// crash safe.
    pub contents: String,
    /// Unix mode the file is created with — `0o600` for anything holding a
    /// credential (invariant 32).
    pub mode: u32,
}

/// A plan for materializing pi-homes, skills and configuration on disk.
/// Idempotent and replayable by construction. Serializable for the same reason
/// as [`MaterializeFile`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializePlan {
    /// Root directory the plan writes under. Nothing outside it is touched.
    pub root: std::path::PathBuf,
    /// The files to publish, in order.
    pub files: Vec<MaterializeFile>,
}

/// What materialization found already-correct, changed, or in conflict.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriftReport {
    /// Paths the plan had to change.
    pub changed: Vec<String>,
    /// Paths that already matched the plan. A replay reports everything here
    /// and nothing under `changed` — which is how a caller proves its replay
    /// was a no-op.
    pub unchanged: Vec<String>,
    /// Paths that differed from expectation and were not safe to change.
    pub conflicts: Vec<String>,
}
