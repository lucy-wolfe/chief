//! The deliberate cold start: prove the company is stopped, then drop the
//! replayable state.
//!
//! This module is the whole of that routine. The TypeScript cold-start module
//! it grew out of is deleted, so the comments below name the behaviour rather
//! than the old file.
//!
//! # Why the stopped proof comes first, and from SQL
//!
//! Mailbox deltas are intentionally fence-free, and this routine owns the
//! stable owner list only while no agent can publish more mail. So both
//! authorities are read and checked before anything is dropped: runtime
//! `stopped` and runtime ownership `released`. Both are rows — there is no
//! file, marker or pid to probe.
//!
//! There used to be a third, `supervisor.state.status`, and it was never once
//! consulted: the detached org-supervisor whose process state it read was
//! retired by #825 and its writer deleted by 5681617a4, so the arm saw `None`
//! on every call from the day it was ported. It is gone rather than repointed —
//! nothing in the one-daemon model has a supervisor process to be `running`.
//!
//! # There is no lock, and there does not need to be
//!
//! An earlier version took the same structural and runtime file locks company
//! start/stop held (#828, deleted). A raw CLI call still cannot delete live
//! mail or race an agent publisher: it can only race chiefd's single-writer
//! queue, which orders it. The stopped/released checks and the clear happen in
//! ONE transaction (`clear_stopped_cold_start_state`), so nothing can start
//! between the proof and the drop.
//!
//! # The post-verify is the point, not decoration
//!
//! An operator recipe that reported "clean start" while rows survived would be
//! worse than one that failed: the next boot would replay mail the operator
//! believes is gone. The clear re-reads both row views inside the same
//! transaction and refuses — rolling the whole thing back — if anything
//! remains.

use crate::error::Refusal;

/// Refusal code for a cold-start clear attempted on a company that is not
/// fully stopped.
pub const COMPANY_NOT_STOPPED: &str = "company-not-stopped";

/// Refusal code for a clear that left rows behind.
pub const CLEAR_INCOMPLETE: &str = "cold-start-clear-incomplete";

/// What a cold-start clear removed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColdStartClearResult {
    /// How many people had a mailbox.
    pub mailbox_persons: usize,
    /// How many envelopes were dropped across them.
    pub mailbox_envelopes: usize,
    /// How many people the launch-intent fence had authorized.
    pub launch_intent_persons: usize,
}

/// The two durable authorities a cold start must find at rest.
///
/// Each is `Option`/`None` for "the company never wrote one", which is a legal
/// stopped state — a company that never ran cannot be running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoppedProof<'a> {
    /// `runtime.status`, when a runtime document exists.
    pub runtime_status: Option<&'a str>,
    /// `runtime_owner.status`; absent means never claimed.
    pub runtime_owner_status: Option<&'a str>,
}

/// Refuse unless every authority reports the company at rest.
///
/// # Errors
/// [`COMPANY_NOT_STOPPED`], naming the authority and the value it actually
/// reported — an operator needs to know which one to stop, not that "something"
/// was running.
pub fn assert_company_stopped(slug: &str, proof: &StoppedProof<'_>) -> Result<(), Refusal> {
    if let Some(status) = proof.runtime_status {
        if status != "stopped" {
            return Err(Refusal::new(
                COMPANY_NOT_STOPPED,
                format!(
                    "Refusing cold-state clear for '{slug}': normalized runtime status is '{status}', not 'stopped'"
                ),
            ));
        }
    }
    // Ownership is the one authority whose ABSENCE is also at-rest: a company
    // that never claimed a runtime has nothing to release.
    if let Some(status) = proof.runtime_owner_status {
        if status != "released" {
            return Err(Refusal::new(
                COMPANY_NOT_STOPPED,
                format!(
                    "Refusing cold-state clear for '{slug}': normalized runtime ownership is '{status}', not 'released'"
                ),
            ));
        }
    }
    Ok(())
}

/// Refuse when the clear left anything behind.
///
/// # Errors
/// [`CLEAR_INCOMPLETE`], naming the surviving ids so the operator can see
/// exactly what did not go.
pub fn assert_clear_complete(
    remaining_mailbox_persons: &[String],
    remaining_launch_intent: &[String],
) -> Result<(), Refusal> {
    if !remaining_mailbox_persons.is_empty() {
        let mut sorted = remaining_mailbox_persons.to_vec();
        sorted.sort();
        return Err(Refusal::new(
            CLEAR_INCOMPLETE,
            format!("Cold-start mailbox clear left SQL rows for: {}", sorted.join(", ")),
        ));
    }
    if !remaining_launch_intent.is_empty() {
        let mut sorted = remaining_launch_intent.to_vec();
        sorted.sort();
        return Err(Refusal::new(
            CLEAR_INCOMPLETE,
            format!("Cold-start launch-intent clear left SQL rows for: {}", sorted.join(", ")),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_rest() -> StoppedProof<'static> {
        StoppedProof { runtime_status: Some("stopped"), runtime_owner_status: Some("released") }
    }

    #[test]
    fn a_fully_stopped_company_passes() {
        assert_company_stopped("acme", &at_rest()).expect("stopped");
    }

    #[test]
    fn a_company_that_never_ran_passes() {
        let proof = StoppedProof { runtime_status: None, runtime_owner_status: None };
        assert_company_stopped("acme", &proof).expect("stopped");
    }

    #[test]
    fn a_running_runtime_names_itself() {
        let mut proof = at_rest();
        proof.runtime_status = Some("running");
        let err = assert_company_stopped("acme", &proof).expect_err("refusal");
        assert_eq!(err.code, COMPANY_NOT_STOPPED);
        assert!(err.message.contains("runtime status is 'running'"));
    }

    #[test]
    fn an_active_claim_names_itself() {
        let mut proof = at_rest();
        proof.runtime_owner_status = Some("active");
        let err = assert_company_stopped("acme", &proof).expect_err("refusal");
        assert!(err.message.contains("runtime ownership is 'active'"));
    }

    #[test]
    fn a_complete_clear_passes() {
        assert_clear_complete(&[], &[]).expect("complete");
    }

    #[test]
    fn a_surviving_mailbox_refuses_with_its_owners_sorted() {
        let err = assert_clear_complete(&["zed".to_string(), "ana".to_string()], &[])
            .expect_err("refusal");
        assert_eq!(err.code, CLEAR_INCOMPLETE);
        assert!(err.message.ends_with("ana, zed"));
    }

    #[test]
    fn a_surviving_launch_intent_refuses() {
        let err = assert_clear_complete(&[], &["ana".to_string()]).expect_err("refusal");
        assert_eq!(err.code, CLEAR_INCOMPLETE);
        assert!(err.message.contains("launch-intent clear left"));
    }
}
