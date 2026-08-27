//! §2.5 — session maintenance.
//!
//! # The `null` that already shipped as a bug once
//!
//! `maint.start` returns **either** a request **or** the literal JSON bytes
//! `null` (inv 26). `null` means "there is no work AND you hold nothing". It
//! is not `{}`, not `{"request":null}`, not an empty object. Re-presenting the
//! **same** claim triple returns the already-claimed request, never `null`.
//!
//! [`MaintStartResponse`] is therefore a transparent `Option`, and a wire-bytes
//! test asserts the four bytes.
//!
//! # The native-reset fix, ported
//!
//! `maint.recover` refuses unless the request is the current FAILED company
//! `fresh_session` (inv 12). This is the `&& !nativeFresh` guard: a historical
//! recovery candidate must never interrupt a running live reset.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{PersonId, Slug};

/// Refusal code for a `recover` that named a request which is not the current
/// failed company fresh-session (inv 12 — the native-reset fix).
pub const RECOVER_NOT_CURRENT_FAILURE: &str = "recover-not-current-failure";

/// Refusal code for `force` without a `companyActionId` or vice versa.
pub const FORCE_COMPANY_ACTION_UNPAIRED: &str = "force-company-action-unpaired";

/// Maximum attempts for a **non-company** maintenance request (D6). Company
/// requests are not attempt-capped.
pub const NON_COMPANY_ATTEMPT_LIMIT: u32 = 3;

/// What kind of session maintenance is being asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaintKind {
    /// Context compaction.
    Compact,
    // TOMBSTONE: `FreshSession`, the full session reset. Deleted with
    // `org_maintain_session`; core's `MaintenanceAction` narrowed to `Compact`
    // in the same change, so this enum can no longer name a kind the store
    // cannot represent.
}

/// Who asked. **Injected, not accepted**: the attribution comes from
/// [`super::identity::CallerIdentity`], never from a field the caller sets. It
/// used to be read off a live `companyActions` entry as well; company actions
/// are deleted, so the caller identity is the whole of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaintRequestedBy {
    /// A human, via a company action.
    Human,
    /// The supervisor.
    Supervisor,
    /// The person themself.
    Person,
}

/// `maint.queue` / `maint.auto_compact`.
///
/// `force` ⇔ `companyActionId` coupling (D6): forcing requires a live company
/// action, and a company action without force is equally refused. The types
/// keep them separate fields because the refusal must name which half is
/// missing; the pairing is checked in `validate()` and surfaces as
/// [`FORCE_COMPANY_ACTION_UNPAIRED`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintQueueRequest {
    /// The company.
    pub slug: Slug,
    /// Whose session.
    pub person_id: PersonId,
    /// What to do.
    pub kind: MaintKind,
    /// Force the request even when one is already unresolved.
    #[serde(default)]
    pub force: bool,
    /// The live `companyActions` entry backing a forced request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_action_id: Option<String>,
}

/// `maint.queue` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintQueueResponse {
    /// The queued (or reused) request.
    pub request_id: String,
    /// True when an unresolved same-kind request was reused.
    pub reused: bool,
}

/// The claim triple, minted **inside the running Pi** and validated by chiefd,
/// which can never mint one (plan §4).
///
/// Every fenced maintenance verb carries it. Re-presenting the same triple to
/// `maint.start` returns the already-claimed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintClaim {
    /// The Pi process id that minted the claim.
    pub process_id: String,
    /// The Pi session id.
    pub session_id: String,
    /// The claim token.
    pub claim_token: String,
}

/// `maint.start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintStartRequest {
    /// The company.
    pub slug: Slug,
    /// The claim triple.
    pub claim: MaintClaim,
}

/// A maintenance request handed to a claiming Pi.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaintRequestRecord {
    /// The request id, presented back on every subsequent fenced verb.
    pub request_id: String,
    /// Whose session.
    pub person_id: PersonId,
    /// What to do.
    pub kind: MaintKind,
    /// Who asked. Injected attribution, reported back for diagnostics.
    pub requested_by: MaintRequestedBy,
    /// Attempt number. Capped at [`NON_COMPANY_ATTEMPT_LIMIT`] for
    /// non-company requests only (D6).
    pub attempt: u32,
}

/// `maint.start` response: the claimed request, or **literal `null`**.
///
/// `#[serde(transparent)]` over an `Option` is the whole point — the wire form
/// is the record itself or the four bytes `null`, with no wrapper object. A
/// wrapper would be the inv-26 bug (which shipped once already).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct MaintStartResponse(pub Option<MaintRequestRecord>);

impl MaintStartResponse {
    /// "No work AND you hold nothing."
    #[must_use]
    pub fn no_work() -> Self {
        Self(None)
    }
}

/// The fenced maintenance verbs that need only the claim and the request id:
/// `interrupt`, `defer`, `recover`, `finish`, `apply`, `complete`.
///
/// TOMBSTONE: `complete_native` was a seventh. It completed a company-scoped
/// request whose only producer was `org_maintain_session`, so with that verb
/// gone nothing can reach it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintFencedRequest {
    /// The company.
    pub slug: Slug,
    /// The claim triple that owns the request.
    pub claim: MaintClaim,
    /// The request being acted on.
    pub request_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maint_start_no_work_is_the_literal_bytes_null() {
        let bytes = serde_json::to_vec(&MaintStartResponse::no_work());
        assert_eq!(bytes.ok().as_deref(), Some(&b"null"[..]), "inv 26");
    }

    #[test]
    fn maint_start_claim_replay_returns_the_record_not_null() {
        let held = MaintStartResponse(Some(MaintRequestRecord {
            request_id: "r1".to_owned(),
            person_id: PersonId("p1".to_owned()),
            kind: MaintKind::Compact,
            requested_by: MaintRequestedBy::Human,
            attempt: 1,
        }));
        let value = serde_json::to_value(&held);
        assert_eq!(
            value.ok(),
            Some(json!({
                "requestId": "r1",
                "personId": "p1",
                "kind": "compact",
                "requestedBy": "human",
                "attempt": 1,
            }))
        );
    }

    #[test]
    fn maint_start_response_round_trips_null() {
        let parsed = serde_json::from_str::<MaintStartResponse>("null");
        assert_eq!(parsed.ok(), Some(MaintStartResponse(None)));
    }

    #[test]
    fn queue_rejects_a_caller_supplied_requested_by() {
        let parsed = serde_json::from_str::<MaintQueueRequest>(
            r#"{"slug":"c","personId":"p","kind":"compact","requestedBy":"human"}"#,
        );
        assert!(parsed.is_err(), "requestedBy is injected attribution, never claimed");
    }

    #[test]
    fn fenced_verbs_require_the_whole_claim_triple() {
        let partial = serde_json::from_str::<MaintFencedRequest>(
            r#"{"slug":"c","requestId":"r1","claim":{"processId":"1","sessionId":"s"}}"#,
        );
        assert!(partial.is_err(), "the claim is a triple; two thirds is not a claim");
        let whole = serde_json::from_str::<MaintFencedRequest>(
            r#"{"slug":"c","requestId":"r1","claim":{"processId":"1","sessionId":"s","claimToken":"t"}}"#,
        );
        assert!(whole.is_ok());
    }
}
