//! The host half of effect delivery — duty #7's sink body.
//!
//! [`delivery`](super::delivery) is the pure core: it decides *what* to deliver
//! and in what order ([`dispatch_plan`](super::delivery::dispatch_plan)), and it
//! owns the ledger transitions ([`mark_delivered`](super::delivery::mark_delivered)
//! / [`record_delivery_failure`](super::delivery::record_delivery_failure)). This
//! module is the piece between them: it turns one already-ordered effect into a
//! durable [`MailboxEnvelope`] (or a strict runtime fence) and executes it, with
//! the durable-publish-first-then-best-effort-wake ordering that is the whole
//! correctness point.
//!
//! # Where the seam is (and what this module is NOT)
//!
//! The one-daemon scheduler (`chiefd run`, `runtime::duty_hooks::DeliverySink`)
//! owns the loop: it calls `dispatch_plan`, builds the ordered batch, and — after
//! this module reports per-id outcomes — commits `mark_delivered` /
//! `record_delivery_failure` on the writer thread. So [`deliver_batch`] is the
//! *body* of a delivery pass, NOT a driver: it does not call `dispatch_plan`, it
//! does not mark effects delivered, and it never re-derives ordering or
//! eligibility. It takes a pre-ordered batch in and returns per-effect-id
//! outcomes out. The trivial `impl DeliverySink` adapter that maps the
//! scheduler's `EffectEnvelope`/`DeliveryOutcome` onto [`DeliveryRequest`]/
//! [`DeliveryReport`] lands once `runtime::duty_hooks` merges to main.
//!
//! # The ordering property, stated precisely
//!
//! For an envelope effect: the durable mailbox row is written (and, in
//! production, committed) *before* any wake is attempted, and a wake that fails
//! leaves the envelope delivered — the failure is reported as data (which
//! recipients were `woken`), never as a delivery failure. An assignment is never
//! silently lost because a wake failed.

use serde_json::Value;

use crate::isotime::iso_millis;
use crate::ledger::Ledgers;
use crate::store::mailbox::{
    self, MailboxEnvelope, RuntimeWaker, Urgency, MAILBOX_ENVELOPE_SCHEMA_VERSION,
};

/// The sender stamped on every system-issued envelope.
const SYSTEM_SENDER: &str = "launcher";

/// One pending effect handed to the sink, already ordered by the scheduler. The
/// core mirror of the scheduler's `runtime::duty_hooks::EffectEnvelope`.
///
/// Not `Eq`: the payload is an arbitrary [`serde_json::Value`], which is only
/// `PartialEq` (JSON numbers include floats).
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryRequest {
    /// The durable effect id — the exactly-once key and the token the writer
    /// phase passes to `mark_delivered` / `record_delivery_failure`.
    pub id: String,
    /// The effect kind (`assignment_delivery`, `manager_check_in`, …).
    pub kind: String,
    /// The effect payload to render and route.
    pub payload: Value,
}

/// One failed dispatch, WITH the reason it failed.
///
/// # Why this type exists
///
/// Every failure on this path had a cause and nowhere to put it. `render`
/// builds a [`RenderError`] that says exactly what the payload was missing,
/// and `mailbox::enqueue` returns a [`ChiefdError`](crate::ChiefdError) — and
/// both were matched
/// as `Err(_)` and dropped, because the only field waiting downstream was a
/// `Vec<String>` of ids. An operator then read that an effect failed, four
/// times, and could not learn from anywhere in the system WHY.
///
/// So the reason travels as data, in the same value as the id, all the way to
/// the boundary that already reports (the scheduler's delivery pass). This is
/// deliberately NOT solved by logging from the store: `chiefd-core/src/store`
/// is a pure core, and a missing channel is an argument about where a reason
/// travels, never a licence to log from the writer.
///
/// `reason` is bounded, log-safe prose. It carries ids, kinds and shapes — not
/// message bodies, not payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchFailure {
    /// The durable effect id — unchanged, and still the token the writer phase
    /// passes to `record_delivery_failure`.
    pub effect_id: String,
    /// Why this effect could not be dispatched, and what would have been
    /// accepted instead.
    pub reason: String,
}

impl DispatchFailure {
    /// Pair an effect id with the reason its dispatch failed.
    #[must_use]
    pub fn new(effect_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { effect_id: effect_id.into(), reason: reason.into() }
    }
}

/// What one delivery pass achieved, by effect id. The core mirror of the
/// scheduler's `runtime::duty_hooks::DeliveryOutcome`, plus the wake result the
/// scheduler logs (`MsgSendResponse::woken`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeliveryReport {
    /// Effects dispatched successfully — the scheduler feeds these to
    /// `mark_delivered`.
    pub delivered: Vec<String>,
    /// Effects that failed this pass, each WITH its reason — fed to
    /// `record_delivery_failure` by id.
    pub failed: Vec<DispatchFailure>,
    /// Recipients whose pane the best-effort wake actually converged. Reported
    /// as data; a recipient missing here was still delivered.
    pub woken: Vec<String>,
}

/// What one effect renders to: a durable mailbox publication.
struct Rendered(Box<MailboxEnvelope>);

/// Why an effect could not be rendered into something deliverable. Its detail is
/// a bounded diagnostic surfaced through [`Display`](std::fmt::Display) — and it
/// now really is surfaced: it becomes the `reason` on the [`DispatchFailure`]
/// this effect id is reported under. It used to be built, formatted, and then
/// dropped by an `Err(_)` arm one call up, which is why a failed dispatch could
/// only ever be read as an id.
#[derive(Debug, Clone)]
struct RenderError {
    detail: String,
}

impl RenderError {
    fn new(detail: impl Into<String>) -> Self {
        Self { detail: detail.into() }
    }
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

fn text<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

/// The recipients an envelope effect routes to.
///
/// An explicit `recipientPersonId` always wins. Otherwise routing is by kind:
/// assignment delivery goes to the worker who owns the work; a manager
/// check-in goes to the ASSIGNEE being asked for status (its body tells the
/// recipient to report to their manager — parity with the TypeScript
/// `effectRecipient`, which routes `manager_check_in` to
/// `effect.assigneePersonId`); the manager-facing kinds (people-check, goal
/// watch, goal-stalled, an assignment's failure/result) go to the manager.
/// The fallbacks keep a well-formed but unusually-shaped payload routable
/// rather than dropping it. An effect that names nobody at all is unroutable
/// and fails this pass — the breaker then owns the poison effect.
fn recipients_for(kind: &str, payload: &Value) -> Vec<String> {
    if let Some(explicit) = text(payload, "recipientPersonId") {
        return vec![explicit.to_string()];
    }
    // The seven `assignment_*`/`manager_*` kinds that stood here went with the
    // goal feature. Their arms are deleted rather than kept for a producer that
    // cannot exist; the default arm below still reads both keys, so a payload
    // shaped like one of them is routed rather than dropped silently.
    let pick = match kind {
        // A durable reminder is addressed to the person it NAMES, under
        // `personId` — its owner, who is not always the person who armed it: a
        // manager may arm one on somebody they manage, and the wake belongs to
        // the report, never to the manager. It is neither assignment work nor a
        // manager-facing card, so neither fallback key below would have found
        // it. Without this arm the effect renders no recipient, fails the pass,
        // and lands in the breaker: staged forever, delivered never, which is
        // precisely the shape of the 638 undelivered native rows (#79). Routing
        // belongs in this function, the single routing authority, rather than
        // being smuggled in as a `recipientPersonId` the producer sets.
        "person_reminder" => text(payload, "personId"),
        _ => text(payload, "assigneePersonId").or_else(|| text(payload, "managerPersonId")),
    };
    pick.map(|person| vec![person.to_string()]).unwrap_or_default()
}

/// The urgency an envelope effect carries. Escalations interrupt; everything
/// else waits for the next drain — matching the urgent batch
/// [`delivery`](super::delivery) forms.
fn urgency_for(kind: &str) -> Urgency {
    match kind {
        // `manager_goal_stalled` and `assignment_failure` stood here and went
        // with the goal feature. `reconcile_escalation` is the surviving
        // escalation and carries the same urgency for the same reason: it is
        // the converge breaker telling a manager that nothing is converging,
        // which is worthless if it waits for the next drain.
        "reconcile_escalation" => Urgency::Interrupt,
        _ => Urgency::Normal,
    }
}

/// The human-readable content an envelope effect carries.
///
/// Every envelope producer writes its prose under a kind-specific key,
/// and they do NOT agree: some write `body`, some write `message`, some write
/// `request`. Reading only `body` therefore missed most kinds and silently
/// substituted a `[kind]` placeholder — 545 live envelopes shipped as a
/// content-free token. The
/// placeholder is what made it survive three days: it produced a *deliverable*
/// envelope, so nothing anywhere reported a problem.
///
/// So: read every key a producer actually uses, and if an envelope genuinely has
/// no content, fail the render. A supervision message with nothing in it is
/// never the right thing to deliver — a loud per-effect render failure (bounded,
/// logged, and reported by id) beats a card that arrives saying nothing.
fn envelope_body(kind: &str, payload: &Value) -> Result<String, RenderError> {
    text(payload, "body")
        .or_else(|| text(payload, "message"))
        .or_else(|| text(payload, "request"))
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            RenderError::new(format!(
                "effect kind '{kind}' carries no body/message/request content; an envelope \
                 effect is deliverable only with non-empty body, message or request text"
            ))
        })
}

/// Render one effect into a durable envelope.
fn render(
    effect_id: &str,
    kind: &str,
    payload: &Value,
    organization: &str,
    at: &str,
) -> Result<Rendered, RenderError> {
    {
        {
            let recipients = recipients_for(kind, payload);
            if recipients.is_empty() {
                return Err(RenderError::new(format!(
                    "effect kind '{kind}' names no recipient; an envelope effect is deliverable \
                     only when it carries recipientPersonId, or the routing key its own kind \
                     uses (assigneePersonId, managerPersonId or personId)"
                )));
            }
            let to = recipients[0].clone();
            let body = envelope_body(kind, payload)?;
            Ok(Rendered(Box::new(MailboxEnvelope {
                schema_version: MAILBOX_ENVELOPE_SCHEMA_VERSION,
                id: effect_id.to_string(),
                organization: organization.to_string(),
                from_person_id: SYSTEM_SENDER.to_string(),
                to,
                recipients,
                body,
                urgency: urgency_for(kind),
                reply_to: text(payload, "replyTo").map(ToString::to_string),
                health_incident: None,
                created_at: at.to_string(),
            })))
        }
    }
}

/// The result of the **writer phase** of a delivery pass: everything durably
/// committed, plus the host actuation the caller must perform off the writer
/// thread. Contains no host I/O and never touches a [`RuntimeWaker`] — that is
/// the whole point of the split, so the durable enqueue can ride the company's
/// single writer without a runtime wake blocking it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StagedBatch {
    /// Envelope effects whose mailbox rows are now durable — delivered
    /// regardless of what the later wake does.
    pub delivered_envelopes: Vec<String>,
    /// Effects that failed to render or to stage (a content conflict) — a real
    /// delivery failure, independent of any host actuation. Each carries the
    /// reason it failed; see [`DispatchFailure`].
    pub failed: Vec<DispatchFailure>,
    /// Recipients with newly-pending mail to wake, deduplicated and in person
    /// order. Best-effort; a wake that fails never un-delivers an envelope.
    pub wake_recipients: Vec<String>,
}

/// The **writer phase** of a delivery pass: render each already-ordered effect
/// and durably stage the envelope rows, with NO host I/O.
///
/// Runs inside the sink's own `CompanyDb::mutate`, so it must not block on runtime
/// or the network — it only reads the clock and writes mailbox rows. Envelope
/// effects are durably enqueued here (delivered-first). Unroutable/conflicting effects are
/// `failed` here, so the breaker — not a silent drop — owns a poison effect.
///
/// It never calls `dispatch_plan`, marks no effect delivered, and re-derives no
/// ordering: the caller supplies the order and commits the effect status.
pub fn stage_batch(
    ledgers: &mut Ledgers,
    organization: &str,
    batch: &[DeliveryRequest],
) -> StagedBatch {
    let at = iso_millis(ledgers.now().0);
    let mut staged = StagedBatch::default();
    let mut wake_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for request in batch {
        match render(&request.id, &request.kind, &request.payload, organization, &at) {
            Ok(Rendered(envelope)) => match mailbox::enqueue(ledgers, &envelope) {
                // Durable write landed; deliverable regardless of the later wake.
                Ok(pending_recipients) => {
                    staged.delivered_envelopes.push(request.id.clone());
                    wake_set.extend(pending_recipients);
                }
                Err(error) => staged.failed.push(DispatchFailure::new(
                    request.id.clone(),
                    format!(
                        "the durable mailbox row could not be staged: {error}. An envelope \
                         effect is delivered only once its mailbox row is committed on the \
                         writer thread — nothing was written and nothing was woken."
                    ),
                )),
            },
            Err(error) => staged.failed.push(DispatchFailure::new(
                request.id.clone(),
                format!("the effect could not be rendered: {error}"),
            )),
        }
    }
    staged.wake_recipients = wake_set.into_iter().collect();
    staged
}

/// The **host phase** of a delivery pass: actuate a [`StagedBatch`]'s host
/// effects and fold them into the per-id report. Touches only the injected
/// [`RuntimeWaker`]; it must run OFF the writer thread, strictly after the
/// writer phase committed.
///
/// One best-effort wake for all newly-pending recipients; its outcome is
/// reported as `woken`, never allowed to move an id from `delivered` to
/// `failed`.
#[must_use]
pub fn actuate_staged(staged: StagedBatch, waker: &dyn RuntimeWaker) -> DeliveryReport {
    let StagedBatch { delivered_envelopes, failed, wake_recipients } = staged;
    let mut report = DeliveryReport { delivered: delivered_envelopes, failed, woken: Vec::new() };
    if !wake_recipients.is_empty() {
        report.woken = waker.wake(&wake_recipients);
    }
    report
}

/// Execute one already-ordered batch of effects and report per-id outcomes — the
/// writer phase and the host phase composed.
///
/// This is the sync body of a `DeliverySink::deliver` pass and the shape the
/// tests drive. The real async sink instead runs [`stage_batch`] inside its own
/// `CompanyDb::mutate` (durable-first, no host I/O on the writer thread) and
/// [`actuate_staged`] off-thread — the two-commit design duty #7 requires: the
/// sink stages durably and actuates; the SCHEDULER commits
/// `mark_delivered`/`record_delivery_failure` afterward from what this returns.
///
/// Contract:
/// * **Durable-first, never coupled.** Every envelope's mailbox row is committed
///   in the writer phase before any wake in the host phase, so no id is reported
///   `delivered` before its envelope is durable, and a failed wake only *adds*
///   to `woken` — it never un-delivers.
/// * **Unroutable is failed, not silently dropped.**
#[must_use]
pub fn deliver_batch(
    ledgers: &mut Ledgers,
    organization: &str,
    waker: &dyn RuntimeWaker,
    batch: &[DeliveryRequest],
) -> DeliveryReport {
    let staged = stage_batch(ledgers, organization, batch);
    actuate_staged(staged, waker)
}

#[cfg(test)]
mod tests;
