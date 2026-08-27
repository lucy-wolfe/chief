//! Delivery-sink body tests. Real `Ledgers`, a recording fake waker. The load-
//! bearing ones prove the ordering property staff flagged: durable-publish
//! first, best-effort wake after, never coupled and never reversed.

use std::cell::RefCell;

use serde_json::json;

use super::*;
use crate::clock::WallMillis;
use crate::ledger::Ledgers;
use crate::store::mailbox::{self, MailboxState, RuntimeWaker, Urgency};

const EPOCH: i64 = 1_784_116_800_000;

fn ledgers() -> Ledgers {
    Ledgers::empty(WallMillis(EPOCH))
}

fn request(id: &str, kind: &str, payload: serde_json::Value) -> DeliveryRequest {
    DeliveryRequest { id: id.to_string(), kind: kind.to_string(), payload }
}

/// Records every host call; can be told to fail the wake.
#[derive(Default)]
struct RecordingWaker {
    wake_calls: RefCell<Vec<Vec<String>>>,
    wake_fails: bool,
}

impl RuntimeWaker for RecordingWaker {
    fn wake(&self, recipients: &[String]) -> Vec<String> {
        self.wake_calls.borrow_mut().push(recipients.to_vec());
        if self.wake_fails {
            Vec::new()
        } else {
            recipients.to_vec()
        }
    }
}

fn envelope_for(id: &str, recipient: &str) -> DeliveryRequest {
    request(
        id,
        "person_reminder",
        json!({
            "personId": recipient,
            // The reminder's prose. `evaluate_reminders` always writes it under
            // `message`, so a fixture that omits it is not a shape any producer
            // emits (#76).
            "message": "[reminder]\n\nRebalance the book before the close."
        }),
    )
}

// --- #76: every envelope kind carries its producer's real content -----------
//
// The envelope-polarity producers do not agree on where they put their prose:
// some write `body`, some write `message`, some write `request`. The sink used
// to read only `body` and silently substitute `[kind]` for everything else, so
// 545 live envelopes shipped as a content-free token. This asserts across EVERY
// surviving kind rather than the one that happened to work.

/// (kind, the payload a real producer emits, the prose it must deliver).
fn every_envelope_kind() -> Vec<(&'static str, serde_json::Value, &'static str)> {
    vec![(
        "person_reminder",
        json!({"personId": "signal-researcher", "message": "[reminder]\n\nRe-read the risk limits."}),
        "[reminder]\n\nRe-read the risk limits.",
    )]
}

/// The failed EFFECT IDS, in report order. `DispatchFailure` carries a reason
/// as well now, so a bare `assert_eq!(report.failed, vec![...])` no longer
/// type-checks; the reason is asserted separately at each site rather than
/// dropped, because carrying it is the point of the change.
fn failed_ids(report: &DeliveryReport) -> Vec<&str> {
    report.failed.iter().map(|f| f.effect_id.as_str()).collect()
}

#[test]
fn every_envelope_kind_delivers_its_producers_prose_never_a_kind_placeholder() {
    for (kind, payload, expected) in every_envelope_kind() {
        let mut l = ledgers();
        let waker = RecordingWaker::default();
        let report = deliver_batch(&mut l, "cobalt", &waker, &[request("e-1", kind, payload)]);

        assert_eq!(report.delivered, vec!["e-1".to_string()], "{kind} must deliver");
        let recipient = report.woken.first().cloned().expect("a recipient");
        let pending = mailbox::pending_for(&l, &recipient);
        assert_eq!(pending.len(), 1, "{kind} staged exactly one envelope");
        assert_eq!(pending[0].body, expected, "{kind} must carry its producer's prose");
        assert_ne!(
            pending[0].body,
            format!("[{kind}]"),
            "{kind} shipped the literal effect-kind token instead of content (#76)"
        );
    }
}

#[test]
fn an_envelope_with_no_content_fails_loudly_rather_than_shipping_an_empty_card() {
    // Fails the effect by id instead of delivering something that says nothing.
    // A card that arrives empty looks delivered and reports no problem -- which
    // is exactly how #76 survived three days on a real-money fleet.
    for payload in
        [json!({"assigneePersonId": "bob"}), json!({"assigneePersonId": "bob", "message": "   "})]
    {
        let mut l = ledgers();
        let waker = RecordingWaker::default();
        let report =
            deliver_batch(&mut l, "cobalt", &waker, &[request("e-1", "person_reminder", payload)]);

        assert_eq!(failed_ids(&report), vec!["e-1"], "a contentless envelope is poison");
        assert!(
            !report.failed[0].reason.is_empty(),
            "a failed effect must carry why: {:?}",
            report.failed[0]
        );
        assert!(report.delivered.is_empty());
        assert!(mailbox::pending_for(&l, "bob").is_empty(), "nothing empty was staged");
    }
}

// --- the happy path: durable then wake, per-id delivered --------------------

#[test]
fn an_envelope_effect_is_durably_staged_then_the_recipient_woken() {
    let mut l = ledgers();
    let waker = RecordingWaker::default();
    let report = deliver_batch(&mut l, "cobalt", &waker, &[envelope_for("del-1", "bob")]);

    assert_eq!(report.delivered, vec!["del-1".to_string()]);
    assert!(report.failed.is_empty());
    assert_eq!(report.woken, vec!["bob".to_string()]);

    // Durable: the envelope is a pending mailbox row bob will drain on wake.
    let pending = mailbox::pending_for(&l, "bob");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "del-1");
    // Exactly one wake call, after the durable write.
    assert_eq!(*waker.wake_calls.borrow(), vec![vec!["bob".to_string()]]);
}

// --- THE correctness property: durable-first, never coupled -----------------

#[test]
fn a_failed_wake_leaves_the_envelope_delivered_and_durable() {
    let mut l = ledgers();
    let waker = RecordingWaker { wake_fails: true, ..RecordingWaker::default() };
    let report = deliver_batch(&mut l, "cobalt", &waker, &[envelope_for("del-1", "bob")]);

    // The wake failed — but the effect is DELIVERED, because delivery is the
    // durable mailbox write, not the wake. An envelope is never lost to a
    // failed wake (the 19-hour-blackout property).
    assert_eq!(report.delivered, vec!["del-1".to_string()]);
    assert!(report.failed.is_empty(), "a failed wake is NEVER a failed delivery");
    assert!(report.woken.is_empty(), "the wake outcome is reported honestly as data");
    assert_eq!(mailbox::pending_for(&l, "bob").len(), 1, "the durable envelope stands");
}

#[test]
fn staging_is_host_free_and_the_wake_only_happens_in_the_actuate_phase() {
    // Structural proof of durable-first: stage_batch (the writer phase) takes NO
    // waker, so it CANNOT wake — the envelope is durable before any host call is
    // even possible. Only actuate_staged (the host phase, off the writer thread)
    // wakes.
    let mut l = ledgers();
    let staged = stage_batch(&mut l, "cobalt", &[envelope_for("del-1", "bob")]);
    assert_eq!(staged.delivered_envelopes, vec!["del-1".to_string()]);
    assert_eq!(staged.wake_recipients, vec!["bob".to_string()]);
    assert_eq!(mailbox::pending_for(&l, "bob").len(), 1, "durable before any host phase");

    let waker = RecordingWaker::default();
    let report = actuate_staged(staged, &waker);
    assert_eq!(report.woken, vec!["bob".to_string()]);
    assert_eq!(
        *waker.wake_calls.borrow(),
        vec![vec!["bob".to_string()]],
        "the wake is the host phase alone",
    );
}

#[test]
fn a_restaged_effect_after_a_crash_is_delivered_again_with_no_duplicate_row() {
    // The two commits are not atomic: if the process dies after the sink's stage
    // but before the scheduler's mark_delivered, the effect stays pending and the
    // NEXT wake pass re-presents the same id — at a later clock. It must deliver
    // again (an idempotent no-op success) with no duplicate row.
    let mut l = ledgers();
    let batch = [envelope_for("del-1", "bob")];
    let first = stage_batch(&mut l, "cobalt", &batch);
    assert_eq!(first.delivered_envelopes, vec!["del-1".to_string()]);

    l.set_now(WallMillis(EPOCH + 60_000)); // a later pass after a crash
    let second = stage_batch(&mut l, "cobalt", &batch);
    assert_eq!(
        second.delivered_envelopes,
        vec!["del-1".to_string()],
        "a re-presented effect is delivered again",
    );
    assert_eq!(second.wake_recipients, vec!["bob".to_string()], "still pending, still wakeable");
    assert_eq!(mailbox::pending_for(&l, "bob").len(), 1, "no duplicate row across the retry");
}

#[test]
fn every_durable_write_precedes_the_single_batch_wake() {
    let mut l = ledgers();
    let waker = RecordingWaker::default();
    // Two envelopes to two recipients in one ordered batch.
    let report = deliver_batch(
        &mut l,
        "cobalt",
        &waker,
        &[envelope_for("del-a", "bob"), envelope_for("del-b", "carol")],
    );
    assert_eq!(report.delivered, vec!["del-a".to_string(), "del-b".to_string()]);
    // Both durable rows exist, and there was exactly ONE wake call carrying BOTH
    // recipients — so both durable writes necessarily completed before any wake.
    assert_eq!(mailbox::pending_for(&l, "bob").len(), 1);
    assert_eq!(mailbox::pending_for(&l, "carol").len(), 1);
    assert_eq!(*waker.wake_calls.borrow(), vec![vec!["bob".to_string(), "carol".to_string()]]);
}

#[test]
fn an_unroutable_effect_is_failed_not_silently_dropped() {
    let mut l = ledgers();
    let waker = RecordingWaker::default();
    // person_reminder with no personId names no recipient.
    let report =
        deliver_batch(&mut l, "cobalt", &waker, &[request("del-1", "person_reminder", json!({}))]);
    assert_eq!(failed_ids(&report), vec!["del-1"], "the breaker owns a poison effect");
    assert!(
        !report.failed[0].reason.is_empty(),
        "an unroutable effect names what a routable one would carry: {:?}",
        report.failed[0]
    );
    assert!(report.delivered.is_empty());
    assert!(waker.wake_calls.borrow().is_empty());
}

#[test]
fn an_escalation_is_routed_to_the_manager_and_marked_interrupt() {
    let mut l = ledgers();
    let waker = RecordingWaker::default();
    let report = deliver_batch(
        &mut l,
        "cobalt",
        &waker,
        // `message` is what the escalation producer always writes; a payload
        // without it is not a shape supervision emits (#76).
        &[request(
            "esc-1",
            "reconcile_escalation",
            json!({"managerPersonId": "quant-head", "message": "the converge breaker tripped"}),
        )],
    );
    assert_eq!(report.delivered, vec!["esc-1".to_string()]);
    let pending = mailbox::pending_for(&l, "quant-head");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].urgency, Urgency::Interrupt, "an escalation interrupts");
}

#[test]
fn a_reminder_reaches_the_person_who_armed_it() {
    // The reachability seam. `person_reminder` addresses its recipient under
    // `personId` — neither of `recipients_for`'s fallback keys
    // (`assigneePersonId`, `managerPersonId`). Without its own routing arm the
    // effect renders NO recipient, fails the pass, and sits in the breaker:
    // produced forever, delivered never. That is the shape of the 638
    // undelivered native rows (#79), and a producer test alone cannot see it —
    // the reminder store's own suite passes either way.
    let mut l = ledgers();
    let waker = RecordingWaker::default();
    let body = "[reminder]\n\nRe-read the risk limits.\n\nRecurring every 1h.";
    let report = deliver_batch(
        &mut l,
        "cobalt",
        &waker,
        &[request(
            "person-reminder:reminder:signal-researcher:1:1784120400000",
            "person_reminder",
            json!({
                "personId": "signal-researcher",
                "reminderId": "reminder:signal-researcher:1",
                "message": body,
            }),
        )],
    );

    assert_eq!(report.delivered.len(), 1, "delivered, not failed as unroutable");
    assert!(report.failed.is_empty(), "failures: {:?}", report.failed);
    let pending = mailbox::pending_for(&l, "signal-researcher");
    assert_eq!(pending.len(), 1, "it reaches the person who armed it");
    assert_eq!(pending[0].body, body, "the renderer marker survives the transport");
    // A reminder must never interrupt a turn.
    assert_eq!(pending[0].urgency, Urgency::Normal);
    // And it wakes that person -- a due reminder for a stopped person IS work
    // arriving, which is what may legitimately bring them up (THE HARD RULE).
    assert_eq!(*waker.wake_calls.borrow(), vec![vec!["signal-researcher".to_string()]]);
}

// --- a mixed batch keeps per-id accounting straight -------------------------

#[test]
fn a_mixed_batch_reports_each_id_independently() {
    let mut l = ledgers();
    let waker = RecordingWaker::default();
    let report = deliver_batch(
        &mut l,
        "cobalt",
        &waker,
        &[envelope_for("del-1", "carol"), request("bad-1", "person_reminder", json!({}))],
    );
    // The envelope delivered, the unroutable failed — independent per-id
    // outcomes from one pass.
    assert_eq!(report.delivered, vec!["del-1".to_string()]);
    let mut failed = failed_ids(&report);
    failed.sort_unstable();
    assert_eq!(failed, vec!["bad-1"]);
    assert!(
        report.failed.iter().all(|f| !f.reason.is_empty()),
        "every failure carries a reason: {:?}",
        report.failed
    );
    assert_eq!(report.woken, vec!["carol".to_string()], "only the delivered envelope woke anyone");
    // The delivered envelope is durable and still pending (the sink never
    // archives — draining is the recipient's own act).
    assert_eq!(
        mailbox::pending_for(&l, "carol").first().map(|e| e.id.clone()),
        Some("del-1".to_string())
    );
    assert_eq!(
        MailboxState::parse(
            &l.mailbox(&mailbox::pending_for(&l, "carol")[0].row_id("carol")).expect("row").state
        ),
        Some(MailboxState::Pending)
    );
}
