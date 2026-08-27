//! §2.2 — structure and staffing.
//!
//! Every structural operation is serialized against normalized current rows;
//! this declarative public schema never asks its caller for a whole-company
//! revision. Two rules are visible in these types:
//!
//! * **The capability vocabulary is closed.** [`Capability`] is an enum, not a
//!   free-form string, so a capability outside the set cannot be smuggled in
//!   at the schema boundary. (It used to also carry invariant 34, "managers
//!   can never be granted bash"; that rule was removed by operator decision on
//!   2026-08-10 — every role may hold every capability.)
//! * Any op whose manifest change implies pi-home/file changes runs as a host
//!   transaction (plan §5.6): the manifest commit happens only after the files
//!   it references are published (inv 8). That is invisible on the wire except
//!   that response fields describe committed facts only.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Bounded, DepartmentId, IdempotencyKey, PersonId, Slug, Warning};

/// A closed capability vocabulary: an enum, with `deny_unknown_fields` plus
/// enum exhaustiveness rejecting anything else at the schema boundary, so a
/// capability outside the set cannot arrive as a free-form string.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum Capability {
    /// Shell access. Available to every role.
    Bash,
    /// Read files in the person's workspace.
    Read,
    /// Write files in the person's workspace.
    Write,
    /// Web search / fetch.
    Web,
    /// Organization tools (send, roster, …).
    Org,
}

/// Role of a person within a department.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PersonRole {
    /// Individual contributor.
    Worker,
    /// Manages a department.
    Manager,
    /// The company executive.
    Executive,
}

/// The three staffing labels, preserved exactly (inv 18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StaffingStatus {
    /// Working in their department.
    Active,
    /// Benched: no pane, still on the roster.
    Benched,
    /// Departed: fired, and the row is retained for durable history/audit and
    /// GC ordering. The id is never reusable — but the PERSON can come back:
    /// `start_person` rehires them as a worker (#1036).
    Departed,
}

/// A department as the manifest describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DepartmentSpec {
    /// Department id.
    pub id: DepartmentId,
    /// Human-facing name.
    pub name: String,
    /// Parent department; absent only for the executive root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<DepartmentId>,
    /// Charter prose handed to the department's people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charter: Option<String>,
}

/// `org.department.add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DepartmentAddRequest {
    /// The company.
    pub slug: Slug,
    /// The department to add.
    pub department: DepartmentSpec,
}

/// `org.department.move`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DepartmentMoveRequest {
    /// The company.
    pub slug: Slug,
    /// The department being moved.
    pub department: DepartmentId,
    /// Its new parent.
    pub new_parent: DepartmentId,
}

/// `org.department.pause` / `org.department.resume`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DepartmentPauseRequest {
    /// The company.
    pub slug: Slug,
    /// The department.
    pub department: DepartmentId,
}

/// `org.department.launch`.
///
/// Fleet suppression is checked **early**; the op opens a launch intent for
/// exactly these people, which is the sole authority for pane admission (D7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DepartmentLaunchRequest {
    /// The company.
    pub slug: Slug,
    /// The department to launch.
    pub department: DepartmentId,
    // TOMBSTONE (#751-P9): `target: Option<RuntimeTargetRef>` sat here. A
    // launch opens a launch intent over PEOPLE; the socket and session it was
    // once allowed to name are the operator client's.
}

/// `org.department.launch` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DepartmentLaunchResponse {
    /// People whose panes were launched. Re-entrant per pane: a repeat launch
    /// reports the same set without spawning duplicates.
    pub launched: Vec<PersonId>,
    /// Epoch millis until which startup admission stays open. Carried forward
    /// across re-entrant launches (inv 21).
    pub startup_admission_until: i64,
}

/// `org.department.stop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DepartmentStopRequest {
    /// The company.
    pub slug: Slug,
    /// The department.
    pub department: DepartmentId,
}

/// `org.department.remove`.
///
/// The normalized removal operation validates the current company state inside
/// its transaction; retry identity is an operation/journal concern, not a
/// caller-provided organization revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DepartmentRemoveRequest {
    /// The company.
    pub slug: Slug,
    /// The department.
    pub department: DepartmentId,
}

/// `org.contract.open` — contracts require an engagement and an expiry; both
/// are non-optional so an unbounded contract is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractOpenRequest {
    /// The company.
    pub slug: Slug,
    /// The person the contract covers.
    pub person_id: PersonId,
    /// The engagement this contract exists for.
    pub engagement: String,
    /// Expiry, epoch millis.
    pub expires_at: i64,
}

/// `org.contract.close`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractCloseRequest {
    /// The company.
    pub slug: Slug,
    /// The contract to close.
    pub contract_id: String,
}

/// `org.hire`.
///
/// The complete candidate is materialized in a tmpdir **first** (inv 37): a
/// preflight failure advances nothing. Create-once via `idempotencyKey`
/// (plan §6.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HireRequest {
    /// The company.
    pub slug: Slug,
    /// Department to hire into.
    pub department: DepartmentId,
    /// Role for the new person.
    pub role: PersonRole,
    /// Capabilities to grant. Every capability is available to every role.
    pub capabilities: Vec<Capability>,
    /// Model to run the person on. Model selection is free at hire time and
    /// forever after (`model-switch-freedom`).
    pub model: String,
    /// Client-supplied create-once key.
    pub idempotency_key: IdempotencyKey,
}

/// `org.hire` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HireResponse {
    /// The new person.
    pub person_id: PersonId,
}

/// `org.bench`.
///
/// Two-phase: the readiness check runs **after**
/// `beginGracefulStaffingTransition` (inv 19 — the ordering is guarded by a
/// comment and a call-order test).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BenchRequest {
    /// The company.
    pub slug: Slug,
    /// The person to bench.
    pub person_id: PersonId,
}

/// `org.bench` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BenchResponse {
    /// Resulting staffing label (inv 18).
    pub status: StaffingStatus,
}

/// `org.recall` / `org.offboard` — verbs that name one person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersonVerbRequest {
    /// The company.
    pub slug: Slug,
    /// The person.
    pub person_id: PersonId,
}

/// `org.transfer` — a permanent move.
///
/// Effective on the very next call: authorization is checked per call against
/// a freshly loaded manifest, never derived from the registered toolset
/// (plan §3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferRequest {
    /// The company.
    pub slug: Slug,
    /// The person to transfer.
    pub person_id: PersonId,
    /// Destination department.
    pub to_department: DepartmentId,
    /// New role, when the transfer is also a promotion or demotion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<PersonRole>,
}

/// One roster row — a bounded projection, never the person's ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterEntry {
    /// The person.
    pub person_id: PersonId,
    /// Their department.
    pub department: DepartmentId,
    /// Their role.
    pub role: PersonRole,
    /// Their staffing label.
    pub status: StaffingStatus,
    /// Model in effect. Last-set wins, forever (`model-switch-freedom`).
    pub model: String,
}

/// `org.roster` response.
///
/// Never mutates and never observes runtime (plan §2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterResponse {
    /// Bounded roster projection.
    pub people: Bounded<RosterEntry>,
    /// Non-fatal observations.
    pub warnings: Vec<Warning>,
}

/// `org.lifecycle_status` response.
///
/// The projection itself lives in `chiefd_core::store::lifecycle_status` and is
/// re-exported rather than re-declared: the pinned conformance fixtures
/// (`conformance/fixtures/tools/org-lifecycle-status-*.json`) record the exact
/// bytes that type serializes, so a parallel wire struct could only ever drift
/// from them.
///
/// # What this replaced
///
/// The predecessor reported a durable `fleetSuppressed` state (a
/// `SuppressionState` value) and a launch-intent roster, and failed CLOSED on
/// an unreadable suppression marker — so a scrap of debris made the control
/// board tell the CEO its whole company was frozen. A `ceoOnlyBootInFlight`
/// column replaced it, reporting a boot **in flight** rather than a mode; that
/// column is deleted too (chief-home-is-cwd §4c), because it read the CEO boot
/// lease and the daemon boots no pane, so nothing can ever be in flight.
pub use chiefd_core::store::lifecycle_status::OrganizationLifecycleStatus as LifecycleStatusResponse;

/// `org.extension_drift` response.
///
/// **Both** staleness detectors are kept: revision drift is not content drift
/// (D15). `chiefctl` exits non-zero when `drift` is true (inv 25) — migration
/// gates depend on that exit code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDriftResponse {
    /// True when either detector fires.
    pub drift: bool,
    /// What the materialized files say.
    pub on_disk: String,
    /// What the loaded extension copies report. Re-materializing a file does
    /// not change loaded code — this is the detector that catches it.
    pub in_process: String,
}

/// `org.list` response row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrgListEntry {
    /// Department id.
    pub department: DepartmentId,
    /// Headcount in that department.
    pub people: u64,
}

/// `org.list` response.
///
/// Keeps the **raw** department count, unlike `company.list` which reports
/// `departments.length - 1` (flag a-5). Do not unify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrgListResponse {
    /// Raw department count, executive department included.
    pub department_count: u64,
    /// Bounded projection.
    pub departments: Bounded<OrgListEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_ops_reject_legacy_revision_fences() {
        let parsed = serde_json::from_str::<DepartmentAddRequest>(
            r#"{"slug":"cobalt","department":{"id":"eng","name":"Engineering"}}"#,
        );
        assert!(parsed.is_ok(), "the normalized operation reads current rows itself");

        let stale = serde_json::from_str::<DepartmentAddRequest>(
            r#"{"slug":"cobalt","expectedRevision":12,"department":{"id":"eng","name":"Engineering"}}"#,
        );
        assert!(stale.is_err(), "expectedRevision must not silently survive in the public schema");
    }

    #[test]
    fn transfer_and_move_requests_reject_legacy_revision_fields() {
        let move_request = serde_json::from_str::<DepartmentMoveRequest>(
            r#"{"slug":"cobalt","expectedRevision":12,"department":"eng","newParent":"executive"}"#,
        );
        assert!(move_request.is_err(), "department moves must not be caller-revision fenced");

        let transfer = serde_json::from_str::<TransferRequest>(
            r#"{"slug":"cobalt","expectedRevision":12,"personId":"p1","toDepartment":"eng"}"#,
        );
        assert!(transfer.is_err(), "person transfers must not be caller-revision fenced");
    }

    #[test]
    fn hire_rejects_a_free_form_capability_string() {
        let parsed = serde_json::from_str::<HireRequest>(
            r#"{"slug":"cobalt","department":"eng","role":"worker","capabilities":["shell"],"model":"opus","idempotencyKey":"k1"}"#,
        );
        assert!(parsed.is_err(), "the capability vocabulary is closed");
    }

    #[test]
    fn hire_requires_an_idempotency_key() {
        let parsed = serde_json::from_str::<HireRequest>(
            r#"{"slug":"cobalt","department":"eng","role":"worker","capabilities":["read"],"model":"opus"}"#,
        );
        assert!(parsed.is_err(), "org.hire is create-once and the key is the fence");
    }

    #[test]
    fn hire_rejects_stripped_cli_fields() {
        let parsed = serde_json::from_str::<HireRequest>(
            r#"{"slug":"cobalt","department":"eng","role":"worker","capabilities":["read"],"model":"opus","idempotencyKey":"k","verbose":true}"#,
        );
        assert!(parsed.is_err());
    }
}
