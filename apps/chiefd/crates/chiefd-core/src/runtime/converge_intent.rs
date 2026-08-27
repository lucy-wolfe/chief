//! The per-cycle converge intent row — audit + shadow-diff, never replayed.
//!
//! Design Q1/Q3: every reconcile cycle persists its computed plan as a
//! host-actions intent row of kind [`CONVERGE_INTENT_KIND`], for audit and
//! shadow-diff. Unlike a `materialize` intent it is **never advanced past
//! `Pending`** and **never replayed**: runtime actuation is not replayable (a
//! killed pane cannot be un-killed). On restart an open converge row is closed
//! as *aborted* — recovery is one fresh observe→plan→apply cycle.
//!
//! Because a converge row never reaches `Published` and carries no file backup
//! set, the existing host-transaction recovery already rolls it back to nothing
//! and closes it (`host_txn::rollback` treats a missing backup set as a no-op),
//! so it can never wedge startup even if the materialize recovery sweeps it.
//! [`abort_open`] makes that abort explicit and correctly attributed for a
//! daemon that prefers to clear converge rows itself at startup first.

use serde::{Deserialize, Serialize};

use crate::error::Refusal;
use crate::host_action::HostActionRecord;
use crate::ledger::Ledgers;
use crate::ChiefdError;

/// The `kind` every converge intent row carries in the host-actions journal.
pub const CONVERGE_INTENT_KIND: &str = "converge";

/// Error code when the audit body cannot be encoded or decoded.
const CONVERGE_INTENT_UNSERIALIZABLE: &str = "converge-intent-unserializable";

/// The audit body stored in a converge intent row's `plan_json`.
///
/// A structured summary rather than the action list itself: enough to audit a
/// cycle and diff a shadow run against a live one, while giving the row no
/// replay meaning whatsoever. Nothing reads it back to decide anything, which
/// is the property that keeps it from becoming a second source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConvergeIntentBody {
    /// Whether the cycle ran in shadow mode (planned everything, actuated nothing).
    pub shadow: bool,
    /// Whether the pointer sweep was allowed to mutate the ledger this cycle.
    pub sweep_live: bool,
    // TOMBSTONE: `admission_ms`. It recorded the plan's admission window, and
    // there is no ramp and therefore no window: the actuator boots everything
    // missing in one pass. An audit field that could only ever record `0` is
    // not a smaller fact, it is a false one.
    /// How many processes the pass asked to stop.
    pub predicted_kill_panes: usize,
    /// How many people the plan predicts respawning.
    pub predicted_respawn_persons: usize,
    /// How many dangling pointers the sweep planned to clear.
    pub pointer_clears: usize,
    /// One human-readable line per converge step, in plan order.
    pub steps: Vec<String>,
}

/// Open a converge intent row in `Pending`. Overwrites any row already at
/// `action_id` (a re-run of the same cycle id supersedes it wholesale).
///
/// # Errors
/// [`ChiefdError`] if the audit body cannot be serialized (unreachable for this
/// plain-data body, surfaced rather than panicked).
pub fn open(
    ledgers: &mut Ledgers,
    action_id: &str,
    body: &ConvergeIntentBody,
) -> Result<(), ChiefdError> {
    let plan_json = serde_json::to_string(body).map_err(|error| {
        Refusal::new(
            CONVERGE_INTENT_UNSERIALIZABLE,
            format!("cannot encode converge intent: {error}"),
        )
    })?;
    let now = ledgers.now();
    ledgers.put_host_action(
        action_id,
        HostActionRecord::pending(CONVERGE_INTENT_KIND, plan_json, now),
    );
    Ok(())
}

/// Close (delete) a converge intent row after its cycle completes. Returns
/// whether a row was present.
pub fn close(ledgers: &mut Ledgers, action_id: &str) -> bool {
    ledgers.close_host_action(action_id)
}

/// Read back a converge intent row's audit body, if one is open at `action_id`
/// and is a converge row.
///
/// # Errors
/// [`ChiefdError`] if a converge row is present but its body does not decode (a
/// corrupt journal), distinct from "no such row" which is `Ok(None)`.
pub fn read(ledgers: &Ledgers, action_id: &str) -> Result<Option<ConvergeIntentBody>, ChiefdError> {
    let Some(record) = ledgers.host_action(action_id) else {
        return Ok(None);
    };
    if record.kind() != CONVERGE_INTENT_KIND {
        return Ok(None);
    }
    serde_json::from_str(record.plan_json()).map(Some).map_err(|error| {
        Refusal::new(
            CONVERGE_INTENT_UNSERIALIZABLE,
            format!("cannot decode converge intent '{action_id}': {error}"),
        )
        .into()
    })
}

/// Close every open converge intent row, returning their ids in recovery order.
///
/// Called once at daemon startup: a converge row that outlived a crash names a
/// cycle whose runtime actuation is not replayable, so recovery is a fresh cycle,
/// never a replay. Materialize (and any other) rows are deliberately left for
/// their own recovery pass.
pub fn abort_open(ledgers: &mut Ledgers) -> Vec<String> {
    let ids: Vec<String> = ledgers
        .open_host_actions()
        .into_iter()
        .filter(|(_, record)| record.kind() == CONVERGE_INTENT_KIND)
        .map(|(id, _)| id.to_owned())
        .collect();
    for id in &ids {
        ledgers.close_host_action(id);
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::WallMillis;
    use crate::host_action::HostActionPhase;

    fn body() -> ConvergeIntentBody {
        ConvergeIntentBody {
            shadow: true,
            sweep_live: false,
            predicted_kill_panes: 1,
            predicted_respawn_persons: 2,
            pointer_clears: 1,
            steps: vec!["KillPane %9".to_owned(), "Respawn %3 -> gen 4".to_owned()],
        }
    }

    fn ledgers() -> Ledgers {
        Ledgers::empty(WallMillis(1_000))
    }

    #[test]
    fn open_read_close_round_trips_the_body() {
        let mut ledgers = ledgers();
        open(&mut ledgers, "converge:1", &body()).expect("open");
        assert_eq!(read(&ledgers, "converge:1").expect("read"), Some(body()));
        assert!(close(&mut ledgers, "converge:1"), "the row was present");
        assert_eq!(read(&ledgers, "converge:1").expect("read"), None);
        assert!(!close(&mut ledgers, "converge:1"), "already gone");
    }

    #[test]
    fn the_row_is_kind_converge_and_stays_pending() {
        // Audit-only: never advanced to Published, so recovery never replays it.
        let mut ledgers = ledgers();
        open(&mut ledgers, "converge:1", &body()).expect("open");
        let record = ledgers.host_action("converge:1").expect("row");
        assert_eq!(record.kind(), CONVERGE_INTENT_KIND);
        assert_eq!(record.phase(), HostActionPhase::Pending);
    }

    #[test]
    fn abort_open_closes_converge_rows_and_leaves_materialize_rows() {
        let mut ledgers = ledgers();
        open(&mut ledgers, "converge:1", &body()).expect("open");
        open(&mut ledgers, "converge:2", &body()).expect("open");
        ledgers.put_host_action(
            "materialize:1",
            HostActionRecord::pending("materialize", "{}", WallMillis(1_000)),
        );

        let aborted = abort_open(&mut ledgers);
        assert_eq!(aborted, vec!["converge:1".to_owned(), "converge:2".to_owned()]);
        assert!(ledgers.host_action("converge:1").is_none());
        assert!(ledgers.host_action("converge:2").is_none());
        assert!(
            ledgers.host_action("materialize:1").is_some(),
            "a materialize intent's recovery is not the converge sweep's to pre-empt",
        );
    }

    #[test]
    fn read_ignores_a_non_converge_row_at_the_same_id() {
        let mut ledgers = ledgers();
        ledgers.put_host_action(
            "shared-id",
            HostActionRecord::pending("materialize", "{}", WallMillis(1_000)),
        );
        assert_eq!(read(&ledgers, "shared-id").expect("read"), None);
    }
}
