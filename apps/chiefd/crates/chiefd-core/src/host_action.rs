//! The `host_actions` journal row — commit 1 and commit 2 of the DB↔filesystem
//! two-phase commit (plan §5.6).
//!
//! **Why this row exists at all.** SQLite's atomicity covers *one database*. It
//! does not cover a DB↔filesystem pair. In the predecessor system one logical
//! operation wrote a file store and a SQL store in sequence with no atomicity
//! across them, so a service blip advanced one and not the other and left them
//! divergent with nothing to reconcile. A host effect is therefore never an
//! unrecorded side effect of a DB transaction: it is *journalled as an intent
//! first*, so that a crash at any instant leaves a durable row saying what was
//! being attempted and how to converge.
//!
//! **What lives where.** This module owns the *row*: its identity, its phase,
//! and its persistence alongside `documents` in the writer actor's single
//! transaction. It deliberately does **not** own the meaning of `plan_json` —
//! that is `chiefd_host::host_txn`'s, because the plan describes host effects
//! and `chiefd-core` performs none. The column is opaque TEXT here, which is
//! what keeps "core does no host work" a structural fact rather than a
//! convention.
//!
//! The load-bearing consequence of putting the row in [`Ledgers`] rather than
//! in a connection of its own: commit 2 — *manifest advance and intent close* —
//! is one `mutate` closure and therefore one SQL transaction, exactly as plan
//! §5.6 requires. A separate journal connection could not promise that.
//!
//! [`Ledgers`]: crate::ledger::Ledgers

use crate::clock::WallMillis;

/// Durable codec for the only structural-JSON column left in `chief.db`.
pub const HOST_TXN_PAYLOAD_SCHEMA: &str = "host-txn-v1";
/// Durable codec for non-replayable converge audit intents.
pub const CONVERGE_INTENT_PAYLOAD_SCHEMA: &str = "converge-intent-v1";
const CONVERGE_INTENT_KIND: &str = "converge";

/// The mechanically constrained codec for one host-action kind.
#[must_use]
pub fn payload_schema_for_kind(kind: &str) -> &'static str {
    if kind == CONVERGE_INTENT_KIND {
        CONVERGE_INTENT_PAYLOAD_SCHEMA
    } else {
        HOST_TXN_PAYLOAD_SCHEMA
    }
}

/// Lifecycle phase of a `host_actions` row.
///
/// The startup recovery pass reads this to decide whether to roll forward or
/// roll back. Replay is always idempotent, never a restart (invariant 40).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostActionPhase {
    /// Commit 1 landed; the executor phase has **not** been observed to
    /// finish. Files may be untouched, partly published, or fully published —
    /// the row cannot distinguish, so recovery rolls *back* from the backups,
    /// which is safe in all three cases because the manifest never advanced.
    Pending,
    /// The executor published every file and said so durably; commit 2 had not
    /// landed. Recovery replays the plan idempotently and completes commit 2.
    Published,
    /// Commit 2 landed. Recovery ignores such a row and prunes it; the normal
    /// path deletes it as part of commit 2, so seeing one is either a prune
    /// left over from an older build or a future explicit close.
    Closed,
}

impl HostActionPhase {
    /// The durable spelling stored in `host_actions.phase`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Published => "published",
            Self::Closed => "closed",
        }
    }

    /// Parse a durable spelling back.
    ///
    /// `None` for anything else — the caller must fail closed rather than
    /// guess. A journal row whose phase cannot be read is exactly the case
    /// where guessing loses a filesystem rollback (plan §5.5: journals are
    /// `FailClosed`).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "published" => Some(Self::Published),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }

    /// Whether the startup recovery pass must act on a row in this phase.
    #[must_use]
    pub const fn needs_recovery(self) -> bool {
        matches!(self, Self::Pending | Self::Published)
    }
}

/// One journalled host-transaction intent.
///
/// `kind` names the operation family (`materialize`, `model-catalog-swap`,
/// `provider-env-scrub`, …) and exists for diagnostics and for the recovery
/// pass's logging; the machine-readable content is all in `plan_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostActionRecord {
    kind: String,
    plan_json: String,
    phase: HostActionPhase,
    created_at: WallMillis,
}

impl HostActionRecord {
    /// A fresh intent row, in phase [`HostActionPhase::Pending`].
    #[must_use]
    pub fn pending(
        kind: impl Into<String>,
        plan_json: impl Into<String>,
        created_at: WallMillis,
    ) -> Self {
        Self {
            kind: kind.into(),
            plan_json: plan_json.into(),
            phase: HostActionPhase::Pending,
            created_at,
        }
    }

    /// The operation family.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Closed durable codec paired with this action kind.
    #[must_use]
    pub fn payload_schema(&self) -> &'static str {
        payload_schema_for_kind(&self.kind)
    }

    /// The serialized plan. Opaque to `chiefd-core`.
    #[must_use]
    pub fn plan_json(&self) -> &str {
        &self.plan_json
    }

    /// Current phase.
    #[must_use]
    pub const fn phase(&self) -> HostActionPhase {
        self.phase
    }

    /// Wall reading of the commit that created the row. The recovery pass
    /// replays open intents in this order so a crash mid-sequence converges in
    /// the order the sequence was attempted.
    #[must_use]
    pub const fn created_at(&self) -> WallMillis {
        self.created_at
    }

    /// The same row advanced to a later phase.
    ///
    /// Phases only ever move forward; there is no "un-publish", because the way
    /// back from a published plan is the backup set, not the row.
    #[must_use]
    pub fn advanced_to(&self, phase: HostActionPhase) -> Self {
        Self { phase, ..self.clone() }
    }

    /// Reconstruct a row read back from SQLite. Writer-only.
    pub(crate) fn from_row(
        kind: String,
        plan_json: String,
        phase: HostActionPhase,
        created_at: WallMillis,
    ) -> Self {
        Self { kind, plan_json, phase, created_at }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_closed_rows_are_ignored_by_recovery() {
        assert!(HostActionPhase::Pending.needs_recovery());
        assert!(HostActionPhase::Published.needs_recovery());
        assert!(!HostActionPhase::Closed.needs_recovery());
    }

    #[test]
    fn phase_spellings_are_stable_because_they_are_durable() {
        // These strings live in `host_actions.phase` rows that survive a
        // restart, so renaming one is a migration, not a refactor.
        for phase in [HostActionPhase::Pending, HostActionPhase::Published, HostActionPhase::Closed]
        {
            assert_eq!(HostActionPhase::parse(phase.as_str()), Some(phase));
        }
        assert_eq!(HostActionPhase::Pending.as_str(), "pending");
        assert_eq!(HostActionPhase::Published.as_str(), "published");
        assert_eq!(HostActionPhase::Closed.as_str(), "closed");
    }

    #[test]
    fn an_unknown_phase_does_not_parse_into_a_permissive_default() {
        // A journal row is fail-closed: an unreadable phase must not become
        // "closed" (silently abandoning a filesystem rollback) nor "pending"
        // (silently rolling back a completed publish).
        assert_eq!(HostActionPhase::parse("PENDING"), None);
        assert_eq!(HostActionPhase::parse(""), None);
        assert_eq!(HostActionPhase::parse("done"), None);
    }

    #[test]
    fn advancing_a_phase_changes_nothing_else_about_the_intent() {
        let row = HostActionRecord::pending("materialize", r#"{"files":[]}"#, WallMillis(9));
        let published = row.advanced_to(HostActionPhase::Published);
        assert_eq!(published.phase(), HostActionPhase::Published);
        assert_eq!(published.kind(), row.kind());
        assert_eq!(published.plan_json(), row.plan_json());
        assert_eq!(published.created_at(), row.created_at());
    }
}
