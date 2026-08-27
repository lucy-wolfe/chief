//! §2.4 — activity.
//!
//! Activity is coordinated by normalized activity and organization rows; no
//! whole-company counter participates in the request.
//!
//! This section used to carry `activity.reflect` and its budgets: a bounded
//! handoff (summary, note, artifacts, commitments) a pane wrote before its
//! park/bench/transfer/offboard could apply. That product is deleted. The
//! transition machinery it rode on survives -- an applied transition is what
//! sheds launch intent and tears the pane down -- but it carries no payload,
//! so nothing here describes one.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Bounded, PersonId, Slug};

/// `activity.status` — a pure read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActivityStatusRequest {
    /// The company.
    pub slug: Slug,
    /// Whose status to read. Absent means the authenticated caller — the
    /// person id is injected, not claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<PersonId>,
}

/// `activity.status` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActivityStatusResponse {
    /// The person the status is for.
    pub person_id: PersonId,
    /// Seed state is **false** (inv 20): a person with no activity record has
    /// never desired to be active, and inferring `true` would launch panes
    /// nobody asked for.
    pub last_desired_active: bool,
    /// Open handoff transition ids, bounded.
    pub open_transitions: Bounded<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_status_defaults_the_person_to_the_caller() {
        let parsed = serde_json::from_str::<ActivityStatusRequest>(r#"{"slug":"c"}"#);
        assert_eq!(parsed.ok().map(|r| r.person_id), Some(None));
    }
}
