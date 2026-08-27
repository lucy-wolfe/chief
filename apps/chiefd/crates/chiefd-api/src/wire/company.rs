//! §2.1 — company lifecycle.
//!
//! The company-owned removal journal is the authority for lifecycle recovery:
//! every operation records its durable phase before idempotent filesystem work
//! advances it. That server property shows on the wire in two places:
//! `remove.plan` returns a `transactionId` the caller must present again, and a
//! retry reuses the durable removal transaction it already opened (inv 39).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Bounded, PersonId, Slug, Warning};

/// `company.create` — create-once; a duplicate request is refused.
///
/// The slug regex is checked before any path use. `requestedBy` is injected,
/// not carried here.
///
/// # The directory is REQUIRED, and it is what makes the request unique
///
/// This carried `dataRoot: Option<PathBuf>` — "absent means the launcher
/// default" — beside a slug that was expected to identify the company on its
/// own. Neither half survives: there is no launcher default and no data root,
/// and a slug names nothing, because two directories may hold companies with
/// the same name. The DIRECTORY is the company, so it is required and it is
/// the whole of the identity.
///
/// A non-normalized path is refused rather than normalized — normalizing would
/// make two different requests claim the same directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanyCreateRequest {
    /// The canonical absolute directory the new company will occupy.
    pub dir: PathBuf,
    /// The new company's slug — its display name, not its identity.
    pub slug: Slug,
    /// Organization template name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// `company.create` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompanyCreateResponse {
    /// The created slug.
    pub slug: Slug,
}

/// Lifecycle state of a company.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CompanyState {
    /// Registered, no runtime.
    Stopped,
    /// Booted with a live session.
    Running,
    /// Fleet suppression is set: supervision is deliberately inert.
    Suppressed,
    /// A removal transaction is open; actor re-creation is refused.
    RemovalPending,
    /// A fail-closed store for this company would not read (plan §7.2
    /// per-company isolation).
    Corrupt,
}

/// `company.list` — pure read, no request fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanyListRequest {}

/// One row of `company.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompanySummary {
    /// The slug — the display name.
    pub slug: Slug,
    /// The directory the company occupies. Part of the row because the slug
    /// does not identify a company: two directories may hold companies with
    /// the same name, and the directory is what tells them apart.
    pub dir: PathBuf,
    /// Lifecycle state.
    pub state: CompanyState,
    /// **`departments.length - 1`** — the executive department is excluded
    /// here and included by `org.list`. Flag a-5: the two counts differ on
    /// purpose; do NOT unify them.
    pub department_count: u64,
}

/// `company.list` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompanyListResponse {
    /// Bounded projection of registered companies.
    pub companies: Bounded<CompanySummary>,
}

/// A request that names only a company. Every company-scoped read and simple
/// verb uses it, so the disk-authority check (§1) has exactly one shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanyRef {
    /// The company.
    pub slug: Slug,
}

/// `company.show` response — a bounded projection, never the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompanyShowResponse {
    /// Company-level summary.
    pub company: CompanySummary,
    // TOMBSTONE (#751-P9): `session: Option<RuntimeTargetRef>` sat here and
    // reported the terminal socket + session a booted company was drawn on.
    // Which multiplexer a company is displayed through is not a company fact.
    /// Headcount, excluding departed people.
    pub people: u64,
    /// Observations that did not rise to an error.
    pub warnings: Vec<Warning>,
}

/// One node of `company.tree`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompanyTreeNode {
    /// Department id.
    pub department: String,
    /// Parent department id; absent for the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// People currently assigned to this department.
    pub people: Vec<PersonId>,
    /// Whether the department is paused.
    pub paused: bool,
}

/// `company.tree` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompanyTreeResponse {
    /// Bounded projection of the tree.
    pub nodes: Bounded<CompanyTreeNode>,
}

/// `company.boot` — it names the company and nothing else.
///
/// #751-P9: it used to carry an optional, fully-specified runtime target
/// (socket + session, D17's unrepresentable-half-set pair). Booting a company
/// is a decision about PEOPLE; which terminal socket the operator happens to
/// be attached to is the operator client's own business and never travelled
/// usefully — the durable ownership record already answers "where", for the
/// one client that has a "where" at all.
///
/// `company.ceo` shared this request type until chief-home-is-cwd §4c deleted
/// the verb with the daemon-side CEO boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanyTargetRequest {
    /// The company.
    pub slug: Slug,
}

/// `company.boot` response. Re-entrant: repeated boots converge to running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompanyBootResponse {
    // TOMBSTONE (#751-P9): `session: RuntimeTargetRef` sat here. A boot answers
    // that the company is now running — not which terminal session it was
    // painted into.
    /// Whether the boot brought the company up.
    pub running: bool,
}

// TOMBSTONE (chief-home-is-cwd §4c): `CompanyCeoResponse { running: bool }`
// stood here, the answer to the `company.ceo` verb. Its doc described
// contention on the `runtime:<slug>` lease. Both the verb and the CEO boot
// lease are deleted: the daemon brings up no pane, so it can neither be asked
// to nor report having done so.

/// `company.resume` response.
///
/// Resume does **not** clear launch intent (flag a-4 — a load-bearing
/// asymmetry with `stop`). Suppression-corrupt fails closed and the failure
/// path writes back the exact prior marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompanyResumeResponse {
    /// People whose panes were resumed.
    pub resumed: Vec<PersonId>,
}

/// `company.stop` response.
///
/// Clears suppression (flag a-3: only explicit stop/resume/ceo write the
/// marker; liveness inference never does). `provider.env` is scrubbed only
/// after proving no pane remains (inv 31), via a host-action intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompanyStopResponse {
    /// How many panes were stopped.
    pub stopped: u64,
    /// Whether `provider.env` was scrubbed. False when a pane remained — the
    /// scrub is never done on an unproven-empty stop (inv 31).
    pub provider_env_scrubbed: bool,
}

/// Whether a compact/reset was requested in force mode. A mode change
/// mid-flight is refused; an unresolved same-kind action is reused only when
/// the force mode matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ForceMode {
    /// Ordinary request.
    Normal,
    /// Force mode.
    Force,
}

/// `company.compact` / `company.reset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompanyMaintenanceRequest {
    /// The company.
    pub slug: Slug,
    /// Force mode. A target the company cannot maintain aborts the whole queue
    /// regardless.
    #[serde(default = "normal_mode")]
    pub force: ForceMode,
}

fn normal_mode() -> ForceMode {
    ForceMode::Normal
}

/// One queued maintenance target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueuedTarget {
    /// The person queued.
    pub person_id: PersonId,
    /// The maintenance request id created or reused.
    pub request_id: String,
    /// True when an unresolved same-kind action was reused rather than a new
    /// one created.
    pub reused: bool,
}

/// `company.compact` / `company.reset` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompanyMaintenanceResponse {
    /// Everything queued. Empty only when the whole queue aborted.
    pub queued: Vec<QueuedTarget>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_rejects_the_injected_requested_by_field() {
        let parsed = serde_json::from_str::<CompanyCreateRequest>(
            r#"{"slug":"cobalt","requestedBy":"p-ceo"}"#,
        );
        assert!(parsed.is_err(), "requestedBy is injected from CallerIdentity");
    }

    #[test]
    fn create_rejects_a_stripped_field_loudly_instead_of_ignoring_it() {
        let parsed =
            serde_json::from_str::<CompanyCreateRequest>(r#"{"slug":"cobalt","dryRun":true}"#);
        assert!(parsed.is_err(), "stripped CLI fields must be a schema error, not a no-op");
    }

    #[test]
    fn create_accepts_the_minimal_documented_body() {
        let parsed = serde_json::from_str::<CompanyCreateRequest>(
            r#"{"dir":"/work/cobalt-seal","slug":"cobalt-seal"}"#,
        );
        assert_eq!(parsed.ok().map(|r| r.slug.to_string()), Some("cobalt-seal".to_owned()));
    }

    /// The directory is REQUIRED, and a body without one is refused rather
    /// than defaulted.
    ///
    /// It used to be `dataRoot: Option<PathBuf>`, where absent meant "the
    /// launcher default" — a default that no longer exists, and whose absence
    /// would otherwise be the quietest possible way to create a company
    /// somewhere nobody asked for.
    #[test]
    fn create_refuses_a_body_that_names_no_directory() {
        let parsed = serde_json::from_str::<CompanyCreateRequest>(r#"{"slug":"cobalt-seal"}"#);
        assert!(parsed.is_err(), "a company with no directory is not a company");
    }

    #[test]
    fn create_refuses_a_slug_that_could_escape_its_directory() {
        // The directory is VALID here on purpose: with it missing the body
        // would be refused for the wrong reason and this would assert nothing.
        let parsed =
            serde_json::from_str::<CompanyCreateRequest>(r#"{"dir":"/work/x","slug":"../etc"}"#);
        assert!(parsed.is_err(), "slug validation happens before any path use");
    }

    /// #751-P9. `boot_target_cannot_be_half_specified` stood here and pinned
    /// D17's socket+session pair. The pair is deleted from the wire, so that
    /// rule has nothing left to enforce — and `deny_unknown_fields`, which was
    /// already on this struct, turns its absence into a real refusal rather
    /// than a silently-ignored field. A boot names a company; nothing else.
    #[test]
    fn a_boot_names_a_company_and_refuses_a_terminal_target() {
        let bare = serde_json::from_str::<CompanyTargetRequest>(r#"{"slug":"cobalt"}"#);
        assert!(bare.is_ok(), "the company alone is the whole request");
        let with_target = serde_json::from_str::<CompanyTargetRequest>(
            r#"{"slug":"cobalt","target":{"socket":"s","session":"org-cobalt"}}"#,
        );
        assert!(
            with_target.is_err(),
            "a terminal socket and session are the client's, and are refused loudly"
        );
    }

    #[test]
    fn compact_defaults_to_normal_mode() {
        let parsed = serde_json::from_str::<CompanyMaintenanceRequest>(r#"{"slug":"cobalt"}"#);
        assert_eq!(parsed.ok().map(|r| r.force), Some(ForceMode::Normal));
    }
}
