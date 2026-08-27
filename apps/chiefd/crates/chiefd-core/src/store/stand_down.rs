//! The company stand-down: the operator's durable "stop working, and stay
//! stopped".
//!
//! # The defect this closes
//!
//! A live company was given one instruction: *"STOP ALL WORK NOW. Do not create
//! departments, do not hire, do not message anyone, do not start or recall
//! anyone. Tell every person to stop immediately and park all of them except
//! yourself. Then stay idle and do nothing until I ask."*
//!
//! **The CEO obeyed perfectly.** It reported `Stood down 6 people · everyone
//! else keeps running`, removed a reminder, said "All personnel stopped and
//! parked", and then refused two inbound messages on principle, reasoning in
//! its own pane: *"I should not respond to Rosa's message since I was
//! explicitly told to stay idle and do nothing."*
//!
//! **Forty-five seconds later all six were back up** — fresh pane ids, fresh
//! processes, brand-new contexts, everyone working again. A relaunch costs MORE
//! than leaving them alone, because each one re-reads AGENTS.md and the control
//! board; that generation alone cost about $1.85.
//!
//! Obeying the instruction was not enough, because something else put everyone
//! back. **There was no way for a user to make the company stop from inside the
//! product.**
//!
//! # What put them back
//!
//! A person's pending mail grants launch intent by itself
//! (`chiefd-host/src/converge_apply/cycle.rs`), and so does a queued session-
//! maintenance request. The only defence was `commanded_stop_watermarks`: a
//! per-person instant derived from that person's own Park transition, admitting
//! any mail created after it. Six people who had been messaging each other were
//! parked with that mail still queued, and every later message, every fired
//! reminder and every maintenance request carries a fresh timestamp. A
//! per-person heuristic cannot answer a company-level question.
//!
//! # The rule, and why it is this one
//!
//! > **While a company is stood down, nothing grants launch intent.**
//!
//! Not mail, not session maintenance, not a start, a wake, a hire or a
//! department creation. The fence stays exactly as the stand-down left it, and
//! the only way out is an explicit resume.
//!
//! It is stated as "nothing" rather than "no automatic path" deliberately. The
//! incident's lesson is that an agent obeying an instruction is not a mechanism:
//! the CEO did obey, and the company came back anyway. A rule enforced only
//! against the paths that happened to break this time is the same rule the
//! per-person watermark already was.
//!
//! **The CEO keeps running.** It is the one person nobody may act on, and the
//! operator asked to keep it — they need somebody to talk to, and somebody to
//! tell to resume. `LaunchFence::admits` and [`super::launch_intent::person_can_run`]
//! already admit the chief unconditionally, so this needs no exemption of its
//! own: emptying the fence leaves exactly the CEO.
//!
//! **Pending mail is HELD, never dropped.** A stand-down writes no mailbox row.
//! The converge pass names everyone whose mail it is holding, so an operator can
//! see what is waiting, and the moment the stand-down is lifted the ordinary
//! mail wake grants those people and they resume with their mail intact. The
//! alternative — dropping or quiescing the mail — would make a stand-down a
//! destructive act, and an operator must be able to pause a company without
//! losing what arrived while it was paused.
//!
//! # Why not the department pause
//!
//! It looks like the answer and it is not. `person_is_operational` already
//! consults `organization_unit_is_active`, which walks ancestors, so a paused
//! ROOT would already block every mail wake. But the root is refused
//! (`org_ops::set_department_paused_op`, `ExecRootProtected`) and lifting that
//! refusal takes the CEO down with the company: `runtime::desired::is_desired_person`
//! walks the same ancestor chain with no CEO exemption, so the CEO leaves the
//! desired roster, while `activity::reconcile` keeps re-adding
//! `ActivityReason::OrganizationRoot` — the two disagree and the CEO oscillates.
//! A stand-down is also not a statement about the org chart: it is an operator
//! decision about whether the company works, it must survive a reorganization,
//! and it must not be undoable by a head resuming a unit.

use rusqlite::{params, OptionalExtension, Transaction};

use crate::error::Refusal;
use crate::store::launch_intent_rows;
use crate::store::organization_rows::RowsSqlError;
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::ChiefdError;

/// The refusal code every verb that would grant launch intent answers with
/// while the company is stood down.
pub const COMPANY_STOOD_DOWN: &str = "company-stood-down";

/// An operator's stand-down, as it is stored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandDown {
    /// When the operator stood the company down.
    pub since: String,
    /// What they said about it, or empty. Free text, shown back to whoever asks
    /// why a verb was refused.
    pub reason: String,
}

fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("stand-down-rows", e)
}

/// The company's stand-down, or `None` when it is working normally.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn read(tx: &Transaction<'_>, slug: &str) -> Result<Option<StandDown>, ChiefdError> {
    tx.query_row("SELECT since, reason FROM stand_down WHERE slug = ?1", params![slug], |row| {
        Ok(StandDown { since: row.get(0)?, reason: row.get(1)? })
    })
    .optional()
    .map_err(store_failure)
}

/// Is this company stood down?
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn is_stood_down(tx: &Transaction<'_>, slug: &str) -> Result<bool, ChiefdError> {
    Ok(read(tx, slug)?.is_some())
}

/// Refuse this verb if the company is stood down, naming the stand-down.
///
/// # Why every granting verb calls this, and not just the automatic ones
///
/// The incident is the argument. The CEO was told to stop and did stop; the
/// company came back because a MECHANISM put it back. A stand-down that only
/// fenced the mail path would be a rule about the one path that happened to
/// break, enforced against everything else by an agent's goodwill — which is
/// exactly what was already tried and exactly what failed.
///
/// The refusal names `chief resume`, because a caller that cannot tell why a
/// start failed will try again, and a person told only "refused" will invent an
/// explanation.
///
/// # Errors
/// [`Refusal`] with [`COMPANY_STOOD_DOWN`] while a stand-down stands;
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn refuse_while_stood_down(
    tx: &Transaction<'_>,
    slug: &str,
    verb: &str,
) -> Result<(), ChiefdError> {
    let Some(stand_down) = read(tx, slug)? else {
        return Ok(());
    };
    let because = if stand_down.reason.trim().is_empty() {
        String::new()
    } else {
        format!(" ({})", stand_down.reason)
    };
    Err(ChiefdError::from(Refusal::new(
        COMPANY_STOOD_DOWN,
        format!(
            "{verb} is refused: the operator stood this company down at {}{because}, so nothing \
             starts anyone until it is resumed. Run `chief resume` to lift it. Pending mail is \
             held, not lost.",
            stand_down.since
        ),
    )))
}

/// Stand the company down: record the operator's decision and empty the launch
/// intent fence, in ONE transaction.
///
/// Both halves or neither. A stand-down recorded without the fence being
/// emptied is a company that says it is stopped and keeps working; a fence
/// emptied without the record is the incident — everyone parked, and the next
/// pass's mail putting them straight back.
///
/// Idempotent: standing down a company that is already stood down keeps the
/// original `since`, because the operator's decision is the FIRST one and a
/// repeated gesture must not look like a fresh event in the feed.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn stand_down(
    tx: &Transaction<'_>,
    slug: &str,
    at: &str,
    reason: &str,
) -> Result<(), ChiefdError> {
    if read(tx, slug)?.is_some() {
        return Ok(());
    }
    apply_and_emit::<RowsSqlError, _>(tx, slug, at, "", |tx| {
        tx.execute(
            "INSERT INTO stand_down(slug, since, reason) VALUES(?1, ?2, ?3)",
            params![slug, at, reason],
        )?;
        Ok(vec![EventTouch::new("stand-down", slug, "upsert", "stand_down", slug)])
    })
    .map_err(|RowsSqlError(e)| e)?;
    // THE FENCE GOES WITH IT, on the caller's own transaction: every launch
    // intent row for the company is deleted. The CEO is admitted without a row
    // (`launch_intent::person_can_run`), so what is left running is exactly the
    // CEO. Both writes or neither — a stand-down recorded without the fence
    // emptied is a company that says it is stopped and keeps working, and a
    // fence emptied without the record is the incident itself.
    launch_intent_rows::clear(tx, slug, at)
}

/// Lift the stand-down. The company works again from the next pass: held mail
/// is still pending, so the ordinary wake grants its recipients.
///
/// Idempotent — resuming a company that is not stood down writes nothing.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure.
pub fn resume(tx: &Transaction<'_>, slug: &str, at: &str) -> Result<(), ChiefdError> {
    let stood_down = read(tx, slug)?.is_some();
    apply_and_emit::<RowsSqlError, _>(tx, slug, at, "", |tx| {
        if !stood_down {
            return Ok(Vec::new());
        }
        tx.execute("DELETE FROM stand_down WHERE slug = ?1", params![slug])?;
        Ok(vec![EventTouch::new("stand-down", slug, "delete", "stand_down", slug)])
    })
    .map(|_seq| ())
    .map_err(|RowsSqlError(e)| e)
}

#[cfg(test)]
mod tests;
