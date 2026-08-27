//! Durable recurring reminders — the recurring wake-ups an agent arms on
//! itself, or a manager arms on somebody they manage.
//!
//! # The gap this closes
//!
//! Nothing in this repository could ever schedule a durable reminder. The
//! `@koltmcbride/pi-loop` addon that used to provide `/loop` persisted to
//! `<person>/workspace/.pi/loops/loops-<sessionId>.json`, keyed by SESSION id,
//! and only the newest session's file counted — a loop file from a superseded
//! Pi session was never a current business lease. That is right for a business
//! lease and fatal for a reminder: a pane restart mints a new session id and
//! every loop the person armed is orphaned — no record, no re-arm, and nothing
//! that even reports it vanished. The operator saw them "all disappear" because
//! panes restart.
//!
//! That is why reminders replaced it outright, and why the addon is gone: a
//! reminder is a company-ledger row, keyed by PERSON, and this module is its
//! clock.
//!
//! # Who owns what
//!
//! **chiefd owns a reminder end to end.** This module is the only writer;
//! [`super::super::supervisor_watermark::Duty::ReminderDispatch`] is the only
//! caller of [`evaluate_reminders`]; TypeScript reads and renders and never
//! writes. That separation is the whole point — the failure mode this project
//! keeps rediscovering is a durable layer and a visible layer that are different
//! systems, where the durable one does not drive the visible one. There is
//! exactly one copy of a reminder and it lives here.
//!
//! # Compute-then-apply, ONE commit
//!
//! Exactly as [`super::check_ins`]: fire and re-arm land in the same
//! `BEGIN…COMMIT`, and the enqueued effect id keys on the **pre-advance**
//! `dueAt`. If the advance committed separately from the enqueue, a crash
//! between them would move `nextDueAt` forward with no effect ever queued for
//! that window — a reminder silently skipped, which for a reminder is total
//! failure rather than a degraded mode.
//!
//! # Skip-aware, never a catch-up burst
//!
//! A reminder advances in whole intervals from its own due time until it is in
//! the future. A chiefd that was down for an hour fires each armed reminder
//! ONCE on recovery, not twelve times. The alternative — `now + interval` —
//! would silently erase the fact that it was late; the alternative of firing per
//! missed window would flood a returning fleet with backlog, which is how a
//! restart turns into a thundering herd.

use std::collections::BTreeMap;

use serde_json::json;

use crate::isotime::{iso_millis, parse_iso_millis, schedule_due_millis};
use crate::ledger::Ledgers;
use crate::store::activity::ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS;
use crate::store::control_authority::{person_is_in_scope, ControlActor};
use crate::store::organization::OrganizationManifest;
use crate::ChiefdError;

use super::{
    mutate, Reminder, SupervisionDraft, MIN_RECURRING_REMINDER_INTERVAL_MS,
    MIN_REMINDER_INTERVAL_MS, REMINDERS_PER_PERSON_LIMIT, REMINDER_PROMPT_LIMIT,
};

/// The effect kind a due reminder enqueues.
pub const REMINDER_EFFECT_KIND: &str = "person_reminder";

/// The stable marker the launcher's intercom renderer keys the "⏰ Reminder"
/// card on. The prefix is load-bearing exactly as `PEOPLE_CHECK_MARKER` is: the
/// renderer matches on it, so a body that does not start with it renders no card
/// at all (#41/#103).
pub const REMINDER_MARKER: &str = "[reminder]";

/// Refusal code for a malformed arm request.
pub const INVALID_REMINDER: &str = "INVALID_REMINDER";
/// Refusal code for arming past [`REMINDERS_PER_PERSON_LIMIT`].
pub const REMINDER_LIMIT_REACHED: &str = "REMINDER_LIMIT_REACHED";
/// Refusal code for stopping a reminder that is not there.
pub const UNKNOWN_REMINDER: &str = "UNKNOWN_REMINDER";
/// Refusal code for reaching a reminder on somebody the caller does not manage.
pub const REMINDER_NOT_IN_SCOPE: &str = "REMINDER_NOT_IN_SCOPE";

/// Refuse an actor reaching `target_person_id`'s reminders from outside their
/// authority.
///
/// A reminder is a person's own scheduled note to themselves, and the tools
/// have always TOLD an agent so — `personId` is documented as "only for a
/// manager arming a reminder for someone they manage". Nothing enforced it:
/// the deleted CLI passed `personId`/`createdByPersonId` straight through and
/// [`arm_reminder`] only checked that both people existed, so any person could
/// arm, list or stop a reminder on any other (DECISIONS.md 2026-08-09, filed
/// out of the transport port rather than fixed inside it). This is the check
/// that makes the claim true, and it is deliberately the SAME predicate every
/// other cross-person write in this product uses
/// ([`crate::store::control_authority::person_is_in_scope`]): self always,
/// otherwise the target's **home** unit must sit under the unit the actor
/// heads.
///
/// # Errors
/// [`REMINDER_NOT_IN_SCOPE`] when the actor neither is, nor manages, the
/// target. The message names both people and the rule, because a refusal an
/// agent cannot act on sends it back around the same call.
pub fn ensure_reminder_scope(
    manifest: &OrganizationManifest,
    actor_person_id: &str,
    target_person_id: &str,
) -> Result<(), ChiefdError> {
    let actor = ControlActor::Person(actor_person_id.to_string());
    if person_is_in_scope(manifest, &actor, target_person_id) {
        return Ok(());
    }
    Err(ChiefdError::refused(
        REMINDER_NOT_IN_SCOPE,
        format!(
            "'{actor_person_id}' does not manage '{target_person_id}' — a reminder may be armed, \
             listed or stopped only for yourself or for someone you manage"
        ),
    ))
}

/// The body delivered when a reminder fires.
///
/// The marker leads, the person's own words follow verbatim, and the footer
/// tells them how to stop it — a recurring message with no visible off switch
/// is how a helpful reminder becomes noise nobody can silence.
fn reminder_message(reminder: &Reminder) -> String {
    let cadence = format_cadence(reminder.interval_ms);
    let recurrence = if reminder.recurring {
        format!(
            "Recurring {cadence}. Stop it with `org_stop_reminder({{ id: \"{}\" }})`.",
            reminder.id
        )
    } else {
        "One-shot; it will not fire again.".to_string()
    };
    format!(
        "{REMINDER_MARKER}\n\
         \n\
         {}\n\
         \n\
         {recurrence}",
        reminder.prompt.trim()
    )
}

/// A cadence rendered for a human: "every 15m", "every 2h", "every 1d".
fn format_cadence(interval_ms: i64) -> String {
    let minutes = interval_ms / 60_000;
    if minutes % (24 * 60) == 0 {
        format!("every {}d", minutes / (24 * 60))
    } else if minutes % 60 == 0 {
        format!("every {}h", minutes / 60)
    } else {
        format!("every {minutes}m")
    }
}

/// What one reminder pass did. Bounded — a report, never the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReminderReport {
    /// Effect ids newly enqueued this pass.
    pub fired: Vec<String>,
    /// Reminder ids that fired for the last time and are now `stopped` —
    /// one-shots, and recurring reminders that reached their expiry.
    pub retired: Vec<String>,
}

/// Fire every due reminder and re-arm it, in one commit.
///
/// # Errors
/// Whatever the store refuses — most notably `EFFECT_CONTENT_CONFLICT` if an
/// effect id is re-enqueued with different content. A refusal publishes nothing.
pub fn evaluate_reminders(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
) -> Result<ReminderReport, ChiefdError> {
    mutate(ledgers, manifest, evaluate)
}

fn evaluate(draft: &mut SupervisionDraft<'_>, at: &str) -> Result<ReminderReport, ChiefdError> {
    let now = parse_iso_millis(at).unwrap_or(0);
    let mut report = ReminderReport::default();
    for id in draft.ledger().reminder_order.clone() {
        fire_if_due(draft, &id, now, at, &mut report)?;
    }
    Ok(report)
}

/// One reminder's whole due→fire→re-arm, or nothing at all.
fn fire_if_due(
    draft: &mut SupervisionDraft<'_>,
    id: &str,
    now: i64,
    at: &str,
    report: &mut ReminderReport,
) -> Result<(), ChiefdError> {
    // Read out everything the enqueue needs BEFORE mutating, because the effect
    // id keys on the pre-advance `dueAt`. This ordering is the single-commit
    // contract, not a borrow-checker accident.
    let Some(reminder) = draft.ledger().reminders.get(id).cloned() else {
        return Ok(());
    };
    if !reminder.is_armed(now) {
        // Expired but still `active` is a state nobody can act on and nothing
        // would ever clear, so retire it here rather than leave a row that
        // renders as armed forever.
        if reminder.status == "active" {
            if let Some(stored) = draft.ledger.reminders.get_mut(id) {
                stored.status = "stopped".to_string();
                stored.stopped_reason = Some("expired".to_string());
                report.retired.push(id.to_string());
            }
        }
        return Ok(());
    }
    // `schedule_due_millis` fails CLOSED on an unreadable stamp (#69): an
    // unparseable `nextDueAt` must not become `i64::MIN` and fire every pass
    // forever, nor `i64::MAX` and fall permanently silent.
    if schedule_due_millis(&reminder.next_due_at, "reminder.nextDueAt") > now {
        return Ok(());
    }
    let due_millis = parse_iso_millis(&reminder.next_due_at).unwrap_or(now);

    let payload: BTreeMap<String, serde_json::Value> = [
        ("personId".to_string(), json!(reminder.person_id)),
        ("reminderId".to_string(), json!(reminder.id)),
        ("dueAt".to_string(), json!(reminder.next_due_at)),
        ("prompt".to_string(), json!(reminder.prompt)),
        ("recurring".to_string(), json!(reminder.recurring)),
        ("intervalMs".to_string(), json!(reminder.interval_ms)),
        ("message".to_string(), json!(reminder_message(&reminder))),
    ]
    .into_iter()
    .collect();
    // Deterministic on the reminder and its pre-advance due epoch, so a repeated
    // pass over the same window can never double-fire.
    let effect_id = format!("person-reminder:{}:{due_millis}", reminder.id);
    if draft.enqueue_effect(&effect_id, REMINDER_EFFECT_KIND, payload, at)? {
        report.fired.push(effect_id);
    }

    // Re-arm in the SAME commit as the enqueue above.
    let Some(stored) = draft.ledger.reminders.get_mut(id) else {
        return Ok(());
    };
    stored.fire_count = stored.fire_count.saturating_add(1);
    stored.last_fired_at = Some(iso_millis(now));
    if stored.recurring {
        // CLAMPED TO THE CADENCE FLOOR, which is what migrates every row armed
        // before that floor existed: no backfill, no window in which an old row
        // keeps an old cadence — it is corrected at its next fire.
        let interval = stored.interval_ms.max(MIN_RECURRING_REMINDER_INTERVAL_MS);
        let mut next = due_millis;
        while next <= now {
            next = next.saturating_add(interval);
        }
        stored.next_due_at = iso_millis(next);
        // A recurring reminder whose next occurrence lands past its own expiry
        // will never fire again; say so now rather than render an armed
        // reminder that is silently already over.
        let expired = stored
            .expires_at
            .as_deref()
            .and_then(parse_iso_millis)
            .is_some_and(|expiry| expiry <= next);
        if expired {
            stored.status = "stopped".to_string();
            stored.stopped_reason = Some("expired".to_string());
            report.retired.push(id.to_string());
        }
    } else {
        stored.status = "stopped".to_string();
        stored.stopped_reason = Some("fired".to_string());
        report.retired.push(id.to_string());
    }
    Ok(())
}

/// What to arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmRequest {
    /// Who is reminded.
    pub person_id: String,
    /// Who armed it — the AUTHENTICATED caller, never a claim off the wire.
    /// This is also the actor [`ensure_reminder_scope`] judges, so the two can
    /// never disagree: the person credited with arming a reminder is exactly
    /// the person whose authority allowed it.
    pub created_by_person_id: String,
    /// The text delivered when it fires.
    pub prompt: String,
    /// The cadence, at least [`MIN_REMINDER_INTERVAL_MS`].
    pub interval_ms: i64,
    /// False for a one-shot.
    pub recurring: bool,
    /// Optional ISO-8601 expiry.
    pub expires_at: Option<String>,
}

/// Arm a reminder.
///
/// The first occurrence is one whole interval out, never immediately: arming a
/// reminder is not itself a request to be reminded right now, and firing on
/// creation would make every arm a wake.
///
/// # Errors
/// [`INVALID_REMINDER`] for a malformed request, [`REMINDER_LIMIT_REACHED`] past
/// [`REMINDERS_PER_PERSON_LIMIT`], [`REMINDER_NOT_IN_SCOPE`] when the creator
/// neither is, nor manages, the person being reminded.
pub fn arm_reminder(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    request: &ArmRequest,
) -> Result<Reminder, ChiefdError> {
    mutate(ledgers, manifest, |draft, at| {
        let now = parse_iso_millis(at).unwrap_or(0);
        let prompt = request.prompt.trim();
        if prompt.is_empty() || prompt.len() > REMINDER_PROMPT_LIMIT {
            return Err(ChiefdError::refused(
                INVALID_REMINDER,
                format!("Reminder prompt must be 1..={REMINDER_PROMPT_LIMIT} characters"),
            ));
        }
        // THE CADENCE FLOOR APPLIES TO A RECURRENCE, THE DELAY FLOOR TO
        // EVERYTHING. A one-shot cannot hold anybody resident: it delivers one
        // turn and the person settles from the last beat and parks.
        let floor = if request.recurring {
            MIN_RECURRING_REMINDER_INTERVAL_MS
        } else {
            MIN_REMINDER_INTERVAL_MS
        };
        if request.interval_ms < floor {
            // THE REFUSAL EXPLAINS ITSELF, because the caller's input was
            // legal-looking and the reason is not obvious from it. It names the
            // floor, WHY the floor is where it is, and what to do instead —
            // including the case the caller usually actually has, which is work
            // that belongs inside one turn rather than in the scheduler.
            return Err(ChiefdError::refused(
                INVALID_REMINDER,
                if request.recurring {
                    format!(
                        "A recurring reminder must be at least {floor}s apart. A cadence inside \
                         the {lease}s settle window is a polling loop: every fire delivers a \
                         turn, every turn resets the settle countdown, so the person can never \
                         park and the fleet is held open for as long as the reminder is armed. \
                         Arm it at {floor}s or more; if the work genuinely needs a faster loop, \
                         it belongs inside one turn rather than in the scheduler.",
                        floor = MIN_RECURRING_REMINDER_INTERVAL_MS / 1_000,
                        lease = ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS / 1_000,
                    )
                } else {
                    format!(
                        "Reminder interval must be at least {}s — anything faster is a poll",
                        MIN_REMINDER_INTERVAL_MS / 1_000
                    )
                },
            ));
        }
        if let Some(stamp) = request.expires_at.as_deref() {
            match parse_iso_millis(stamp) {
                None => {
                    return Err(ChiefdError::refused(
                        INVALID_REMINDER,
                        "Reminder expiry is not an ISO-8601 timestamp",
                    ))
                }
                Some(expiry) if expiry <= now => {
                    return Err(ChiefdError::refused(
                        INVALID_REMINDER,
                        "Reminder expiry is already in the past",
                    ))
                }
                Some(_) => {}
            }
        }
        for person_id in [&request.person_id, &request.created_by_person_id] {
            if !draft.manifest().people.contains_key(person_id) {
                return Err(ChiefdError::refused(
                    INVALID_REMINDER,
                    format!("Unknown person '{person_id}'"),
                ));
            }
        }
        // Judged against the manifest THIS mutation reads, not one the route
        // read a moment earlier: two reads of the same authority in one call
        // are two chances to disagree, and the second is what the write would
        // then be authorized against.
        ensure_reminder_scope(draft.manifest(), &request.created_by_person_id, &request.person_id)?;
        // A killed pane resumes and the agent re-arms the reminder it never
        // saw a result for. An identical ACTIVE reminder is that reminder.
        //
        // No time window, deliberately: two identical armed reminders have no
        // legitimate version, and a duplicate RECURRING one is the worst decay
        // shape on the tool surface — it mails the same prompt forever, and
        // nothing after the fact can tell the copies apart. Stopped reminders
        // are excluded, so re-arming something deliberately stopped still
        // works.
        if let Some(existing) = draft
            .ledger()
            .reminder_order
            .iter()
            .filter_map(|id| draft.ledger().reminders.get(id))
            .find(|r| {
                r.status == "active"
                    && r.person_id == request.person_id
                    && r.created_by_person_id == request.created_by_person_id
                    && r.prompt == prompt
                    && r.interval_ms == request.interval_ms
                    && r.recurring == request.recurring
                    && r.expires_at == request.expires_at
            })
        {
            return Ok(existing.clone());
        }

        let armed = draft
            .ledger()
            .reminder_order
            .iter()
            .filter_map(|id| draft.ledger().reminders.get(id))
            .filter(|r| r.person_id == request.person_id && r.status == "active")
            .count();
        if armed >= REMINDERS_PER_PERSON_LIMIT {
            return Err(ChiefdError::refused(
                REMINDER_LIMIT_REACHED,
                format!(
                    "'{}' already has {REMINDERS_PER_PERSON_LIMIT} reminders armed",
                    request.person_id
                ),
            ));
        }

        // Sequence off the existing order rather than a random id, so the same
        // company replayed produces the same ids and a test can name one.
        let id = next_reminder_id(draft, &request.person_id);
        let reminder = Reminder {
            id: id.clone(),
            person_id: request.person_id.clone(),
            created_by_person_id: request.created_by_person_id.clone(),
            prompt: prompt.to_string(),
            interval_ms: request.interval_ms,
            next_due_at: iso_millis(now.saturating_add(request.interval_ms)),
            status: "active".to_string(),
            recurring: request.recurring,
            fire_count: 0,
            created_at: at.to_string(),
            last_fired_at: None,
            expires_at: request.expires_at.clone(),
            stopped_reason: None,
            stopped_at: None,
            extra: BTreeMap::new(),
        };
        draft.ledger.reminder_order.push(id.clone());
        draft.ledger.reminders.insert(id, reminder.clone());
        Ok(reminder)
    })
}

/// The next free `reminder:<person>:<n>` id.
fn next_reminder_id(draft: &SupervisionDraft<'_>, person_id: &str) -> String {
    let prefix = format!("reminder:{person_id}:");
    // Highest existing suffix plus one, over the WHOLE order including stopped
    // reminders: reusing a stopped reminder's id would collide with the effect
    // ids it already published (`person-reminder:<id>:<dueMillis>`), and
    // `enqueue_effect` would refuse the reuse as a content conflict.
    let highest = draft
        .ledger()
        .reminder_order
        .iter()
        .filter_map(|id| id.strip_prefix(&prefix))
        .filter_map(|suffix| suffix.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", highest.saturating_add(1))
}

/// Stop a reminder. Idempotent for an already-stopped one; refuses an unknown id.
///
/// The row is kept rather than deleted, so `fire_count` and `last_fired_at`
/// remain answerable and the id is never recycled into an effect-id collision.
///
/// # Errors
/// [`REMINDER_NOT_IN_SCOPE`] when `actor_person_id` neither is, nor manages,
/// `person_id`; [`UNKNOWN_REMINDER`] if no such reminder exists, or if it
/// belongs to someone other than `person_id`.
pub fn stop_reminder(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    actor_person_id: &str,
    person_id: &str,
    reminder_id: &str,
) -> Result<Reminder, ChiefdError> {
    mutate(ledgers, manifest, |draft, at| {
        // Scope FIRST, so an out-of-scope caller learns nothing about whether
        // the id it guessed exists — the same reason a wrong owner is reported
        // as UNKNOWN below rather than as a distinct "not yours".
        ensure_reminder_scope(draft.manifest(), actor_person_id, person_id)?;
        let owned = draft
            .ledger()
            .reminders
            .get(reminder_id)
            .is_some_and(|reminder| reminder.person_id == person_id);
        // A wrong owner is reported as UNKNOWN, not as a distinct
        // "not yours": the two answers together would let anyone enumerate
        // another person's reminder ids.
        if !owned {
            return Err(ChiefdError::refused(
                UNKNOWN_REMINDER,
                format!("No reminder '{reminder_id}' for '{person_id}'"),
            ));
        }
        let Some(reminder) = draft.ledger.reminders.get_mut(reminder_id) else {
            return Err(ChiefdError::refused(
                UNKNOWN_REMINDER,
                format!("No reminder '{reminder_id}' for '{person_id}'"),
            ));
        };
        if reminder.status == "active" {
            reminder.status = "stopped".to_string();
            reminder.stopped_reason = Some("stopped".to_string());
            reminder.stopped_at = Some(at.to_string());
        }
        Ok(reminder.clone())
    })
}

/// Every reminder for one person, armed first, in creation order.
#[must_use]
pub fn list_reminders(ledger: &super::SupervisionLedger, person_id: &str) -> Vec<Reminder> {
    let mut reminders: Vec<Reminder> = ledger
        .reminder_order
        .iter()
        .filter_map(|id| ledger.reminders.get(id))
        .filter(|reminder| reminder.person_id == person_id)
        .cloned()
        .collect();
    reminders.sort_by_key(|reminder| reminder.status != "active");
    reminders
}

/// How many reminders are armed company-wide, for the footer's honest count.
#[must_use]
pub fn armed_count(ledger: &super::SupervisionLedger, now: i64) -> usize {
    ledger
        .reminder_order
        .iter()
        .filter_map(|id| ledger.reminders.get(id))
        .filter(|reminder| reminder.is_armed(now))
        .count()
}

/// The earliest armed reminder's due instant, or `None` when nothing is armed.
///
/// This is the `ReminderDispatch` duty's alarm clock: the duty sleeps exactly
/// until this instant instead of waking on a fixed cadence, so a company with no
/// reminders costs one blocked task and zero CPU. Pure and read-only, so it is
/// safe to call from a snapshot read off the writer thread — the same contract
/// as [`super::deadlines::next_due_at`], which it deliberately mirrors.
///
/// Past-due instants are INCLUDED: the duty must still act on a reminder it has
/// not yet fired, so the alarm cannot simply skip the past.
#[must_use]
pub fn next_due_at(ledger: &super::SupervisionLedger, now: i64) -> Option<i64> {
    ledger
        .reminder_order
        .iter()
        .filter_map(|id| ledger.reminders.get(id))
        .filter(|reminder| reminder.is_armed(now))
        .filter_map(|reminder| parse_iso_millis(&reminder.next_due_at))
        .min()
}

#[cfg(test)]
mod tests;
