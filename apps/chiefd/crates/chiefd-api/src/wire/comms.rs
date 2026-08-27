//! §2.7 — messaging, health, events.
//!
//! * `msg.send` is **durable-before-wake** (inv 16): the mailbox commit
//!   happens first and a failed wake is diagnostic, never a delivery failure.
//!   [`MsgSendResponse::woken`] reports the wake outcome as data.
//! * `msg.drain` is at-least-once; dispatch is idempotent by `effect.id` and a
//!   failed batch leaves **all** effects pending (inv 15).
//! * `events.emit` is exactly-once by `sha256(id)`: `INSERT OR IGNORE` on a
//!   unique key, so a constraint hit means somebody won. The row is the
//!   authority and the journal line is best-effort — a failed append returns
//!   `{created:true, journalAppended:false}` (inv 29), not an error.
//! * Health is a fail-open store: token-free failure records.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Bounded, PersonId, Slug, Warning};

/// Maximum incidents a health projection returns.
pub const HEALTH_INCIDENT_LIMIT: usize = 200;
/// Byte cap on a short log excerpt.
pub const HEALTH_LOG_CAP_SHORT: usize = 64 * 1024;
/// Byte cap on a long log excerpt.
pub const HEALTH_LOG_CAP_LONG: usize = 256 * 1024;
/// Minimum gap before a second, independent sample may page (plan §2.7).
pub const HEALTH_SECOND_SAMPLE_MIN_MS: i64 = 15_000;

/// `msg.send`.
///
/// `"launcher"` is never a recipient — that behavioral debt lives in the shim's
/// separately versioned description catalog, and an unknown recipient is
/// refused here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MsgSendRequest {
    /// The company.
    pub slug: Slug,
    /// Recipients. Empty is refused: the repeated-empty-send loop is exactly
    /// what the shim's circuit breaker exists for.
    pub to: Vec<PersonId>,
    /// The message. Never omitted.
    pub body: String,
    /// Complete this assignment with the message body as its result — the
    /// **only** route to `assignment.result` for a worker (plan §3.4). The id
    /// must be the exact one from the active ASSIGNMENT header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete_assignment: Option<String>,
}

/// `msg.send` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MsgSendResponse {
    /// The durable envelope. It exists before any wake was attempted.
    pub envelope_id: String,
    /// Recipients whose pane chiefd managed to wake. A recipient missing here
    /// was still delivered — the mailbox was authoritative first (inv 16).
    pub woken: Vec<PersonId>,
}

/// `msg.drain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MsgDrainRequest {
    /// The company.
    pub slug: Slug,
    /// Maximum envelopes to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
}

/// One delivered envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    /// Envelope id.
    pub envelope_id: String,
    /// Who sent it.
    pub from: PersonId,
    /// The body.
    pub body: String,
    /// When it was committed, epoch millis.
    pub created_at: i64,
}

/// `msg.drain` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MsgDrainResponse {
    /// The drained page.
    pub envelopes: Bounded<Envelope>,
    /// Effects still pending after this batch. A failed batch leaves ALL of
    /// them pending (inv 15) — the count does not decrease on partial success.
    pub pending_effects: u64,
}

/// Severity of a health incident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum IncidentSeverity {
    /// Recorded, not paging.
    Info,
    /// Degraded.
    Warn,
    /// Paging after an independent second sample ≥ 15 s later.
    Critical,
}

/// `health.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthStatusRequest {
    /// The company.
    pub slug: Slug,
}

/// One health incident, already redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthIncident {
    /// Incident id.
    pub incident_id: String,
    /// Who or what it concerns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<PersonId>,
    /// Severity.
    pub severity: IncidentSeverity,
    /// Redacted detail. **All** detail goes through `redact()` before it
    /// reaches this field.
    pub detail: String,
    /// When it opened, epoch millis.
    pub opened_at: i64,
    /// When it resolved, epoch millis; absent while open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
}

/// `health.status` response.
///
/// The health store is fail-open: corruption silently resets it and reports a
/// warning rather than refusing the read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthStatusResponse {
    /// Incidents, truncated at [`HEALTH_INCIDENT_LIMIT`].
    pub incidents: Bounded<HealthIncident>,
    /// Non-fatal observations, including "the store was reset".
    pub warnings: Vec<Warning>,
}

/// `health.record`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthRecordRequest {
    /// The company.
    pub slug: Slug,
    /// Who or what it concerns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<PersonId>,
    /// Severity.
    pub severity: IncidentSeverity,
    /// Detail; redacted server-side before storage.
    pub detail: String,
}

/// `health.resolve`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthResolveRequest {
    /// The company.
    pub slug: Slug,
    /// The incident to resolve.
    pub incident_id: String,
}

/// `health.record` / `health.resolve` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthIncidentResponse {
    /// The incident id, created or resolved.
    pub incident_id: String,
}

/// `events.emit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventEmitRequest {
    /// The company.
    pub slug: Slug,
    /// Caller-chosen event id. Exactly-once is keyed on its sha256.
    pub id: String,
    /// Event kind.
    pub kind: String,
    /// Event payload.
    pub payload: serde_json::Value,
}

/// `events.emit` response (inv 29).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventEmitResponse {
    /// False when the unique key already existed — somebody else won, which is
    /// success, not an error.
    pub created: bool,
    /// Whether the best-effort journal line was appended. `false` with
    /// `created: true` is a documented, successful outcome.
    pub journal_appended: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_send_rejects_a_claimed_sender() {
        let parsed = serde_json::from_str::<MsgSendRequest>(
            r#"{"slug":"c","to":["p"],"body":"b","from":"p-other"}"#,
        );
        assert!(parsed.is_err(), "the sender is the authenticated caller");
    }

    #[test]
    fn msg_send_carries_the_only_route_to_assignment_result() {
        let parsed = serde_json::from_str::<MsgSendRequest>(
            r#"{"slug":"c","to":["p"],"body":"done","completeAssignment":"a1"}"#,
        );
        assert_eq!(parsed.ok().and_then(|r| r.complete_assignment), Some("a1".to_owned()));
    }

    #[test]
    fn events_emit_reports_a_failed_journal_append_as_success() {
        let value =
            serde_json::to_value(EventEmitResponse { created: true, journal_appended: false });
        assert_eq!(
            value.ok(),
            Some(serde_json::json!({"created": true, "journalAppended": false})),
            "inv 29"
        );
    }

    #[test]
    fn health_caps_are_the_documented_numbers() {
        assert_eq!(
            (
                HEALTH_INCIDENT_LIMIT,
                HEALTH_LOG_CAP_SHORT,
                HEALTH_LOG_CAP_LONG,
                HEALTH_SECOND_SAMPLE_MIN_MS
            ),
            (200, 65536, 262_144, 15_000)
        );
    }

    #[test]
    fn health_record_rejects_stripped_cli_fields() {
        let parsed = serde_json::from_str::<HealthRecordRequest>(
            r#"{"slug":"c","severity":"warn","detail":"d","json":true}"#,
        );
        assert!(parsed.is_err());
    }
}
