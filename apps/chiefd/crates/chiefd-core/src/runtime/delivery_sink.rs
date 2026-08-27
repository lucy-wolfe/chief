//! The concrete [`DeliverySink`] for duty #3 (MailboxWake) — the adapter that
//! wires the daemon scheduler's hook onto the mailbox store and the wake seam.
//!
//! [`super::duty_hooks::DeliverySink`] is the contract the `chiefd run` scheduler
//! calls off the writer thread; this is its implementation. The whole method is
//! the settled two-commit design:
//!
//! 1. **Writer phase** — [`stage_batch`] durably enqueues the rendered mailbox
//!    rows inside the sink's own [`CompanyDb::mutate`], under
//!    [`MutationClass::Small`] so it rides the fast channel and never queues
//!    behind a multi-second reconcile. No host I/O runs on the writer thread.
//! 2. **Host phase** — [`actuate_staged`] best-effort wakes the newly-pending
//!    recipients, OFF the writer thread.
//!
//! The sink returns per-effect-id `{delivered, failed}` and NOTHING else: it
//! never touches the supervision `Effect` row status — the scheduler commits
//! `mark_delivered`/`record_delivery_failure` in a separate, later transaction
//! from what this returns. Those two commits are deliberately not atomic;
//! [`stage_batch`]'s insert-if-absent staging makes the crash-between-them replay
//! a harmless no-op, which is the idempotency the trait's contract requires.
//!
//! The wake actuation is an injected [`RuntimeWaker`]; the the real runtime-respawn
//! implementation lives in `chiefd-host`, so this stays testable with a fake.

use std::sync::Arc;

use crate::actor::{CompanyDb, MutationClass, MutationName};
use crate::store::mailbox::RuntimeWaker;
use crate::store::supervision::{actuate_staged, stage_batch, DeliveryRequest, DispatchFailure};

use super::duty_hooks::{BoxFuture, DeliveryOutcome, DeliverySink, DutyContext, EffectEnvelope};

/// The mutation class the durable staging commit runs under. `Small` bypasses a
/// blocked reconcile (plan §2.3 [Δ]): mail must not wait behind a multi-second
/// D9 cycle, the same reason goal publication is `Small`.
const STAGING_CLASS: MutationClass = MutationClass::Small;

/// Delivers pending supervision effects by durably staging their mailbox rows
/// and waking the recipients — the concrete [`DeliverySink`].
///
/// Generic over the wake seam `W` (rather than holding `Arc<dyn RuntimeWaker>`)
/// so the host phase dispatches through the ordinary `&W -> &dyn RuntimeWaker`
/// unsizing coercion; the scheduler erases `W` at the `Arc<dyn DeliverySink>`
/// boundary. Owns an [`Arc<CompanyDb>`] for its own durable staging commit,
/// separate from the scheduler's effect-status commit.
pub struct MailboxDeliverySink<W> {
    db: Arc<CompanyDb>,
    waker: Arc<W>,
}

impl<W: RuntimeWaker + Send + Sync + 'static> MailboxDeliverySink<W> {
    /// Build a sink over a company's writer actor and a wake seam.
    #[must_use]
    pub fn new(db: Arc<CompanyDb>, waker: Arc<W>) -> Self {
        Self { db, waker }
    }
}

impl<W: RuntimeWaker + Send + Sync + 'static> DeliverySink for MailboxDeliverySink<W> {
    fn deliver(
        &self,
        ctx: &DutyContext,
        envelopes: Vec<EffectEnvelope>,
    ) -> BoxFuture<'_, DeliveryOutcome> {
        let requests: Vec<DeliveryRequest> = envelopes
            .into_iter()
            .map(|envelope| DeliveryRequest {
                id: envelope.id,
                kind: envelope.kind,
                payload: envelope.payload,
            })
            .collect();
        // Captured before `requests` is moved into the staging closure, so a
        // failed staging commit can still report every id as failed.
        let all_ids: Vec<String> = requests.iter().map(|request| request.id.clone()).collect();
        let organization = ctx.slug.clone();
        let db = Arc::clone(&self.db);
        let waker = Arc::clone(&self.waker);

        Box::pin(async move {
            // Writer phase: durable, host-free, on the fast channel.
            let staged = db
                .mutate(STAGING_CLASS, MutationName("mailbox.stage"), move |ledgers| {
                    Ok(stage_batch(ledgers, &organization, &requests))
                })
                .await;
            match staged {
                // Host phase: best-effort wake, off-thread.
                Ok(staged) => {
                    let report = actuate_staged(staged, waker.as_ref());
                    DeliveryOutcome { delivered: report.delivered, failed: report.failed }
                }
                // The staging commit itself failed, so nothing is durable and
                // nothing was woken. Report every id failed; the scheduler then
                // drives `record_delivery_failure`, and the breaker owns a
                // persistently unstageable effect.
                //
                // Every id in the batch failed for the SAME reason — one
                // refused transaction — so each carries that one refusal
                // rather than the bare id it used to carry. Without this, a
                // whole batch failing on a refused commit and a single effect
                // failing on its own unroutable payload were the same four
                // characters in a log.
                Err(error) => {
                    let reason = format!(
                        "the delivery staging commit was refused: {error}. An effect is \
                         delivered only once its staging transaction commits; every effect in \
                         this batch stays pending and is retried next pass."
                    );
                    DeliveryOutcome {
                        delivered: Vec::new(),
                        failed: all_ids
                            .into_iter()
                            .map(|id| DispatchFailure::new(id, reason.clone()))
                            .collect(),
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;
    use crate::clock::WallMillis;
    use crate::ledger::{LedgerSnapshot, Ledgers};
    use crate::store::mailbox;
    use crate::store::COMPANY_DB_FILENAME;
    use crate::test_support::ManualClock;

    /// A Send+Sync wake seam that counts wakes — the shape the generic bound
    /// requires (an interior-mutable `RefCell` fake would not be `Sync`, which
    /// is why the store-level tests use one and this does not).
    #[derive(Default)]
    struct CountingWaker {
        wakes: AtomicUsize,
    }

    impl RuntimeWaker for CountingWaker {
        fn wake(&self, recipients: &[String]) -> Vec<String> {
            self.wakes.fetch_add(1, Ordering::SeqCst);
            recipients.to_vec()
        }
    }

    fn dummy_context(slug: &str) -> DutyContext {
        DutyContext {
            slug: slug.to_string(),
            snapshot: Arc::new(LedgerSnapshot::committed(Ledgers::empty(WallMillis(0)), 0)),
        }
    }

    #[tokio::test]
    async fn deliver_durably_stages_the_envelope_and_then_wakes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        let clock = Arc::new(ManualClock::default());
        let db = Arc::new(CompanyDb::open("cobalt", &path, clock).expect("open"));
        let waker = Arc::new(CountingWaker::default());
        let sink = MailboxDeliverySink::new(Arc::clone(&db), Arc::clone(&waker));

        let envelopes = vec![EffectEnvelope {
            id: "del-1".to_string(),
            kind: "assignment_delivery".to_string(),
            payload: json!({
                "assignmentId": "a-1",
                "assigneePersonId": "bob",
                "managerPersonId": "quant-head",
                "generation": 1,
                // `request` is the delivery's prose; `enqueue_delivery` always
                // writes it, and an envelope without content now fails (#76).
                "request": "Rebalance the book"
            }),
        }];

        let outcome = sink.deliver(&dummy_context("cobalt"), envelopes).await;
        assert_eq!(outcome.delivered, vec!["del-1".to_string()]);
        assert!(outcome.failed.is_empty());

        // The envelope is durable in the company database — committed by the
        // sink's own writer-phase mutation, readable through a fresh snapshot.
        let pending = db.read(|snapshot| mailbox::pending_for(snapshot, "bob").len());
        assert_eq!(pending, 1, "the envelope is durably staged");
        // And the best-effort wake ran once, in the host phase after staging.
        assert_eq!(waker.wakes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_restaged_effect_after_a_crash_stays_one_durable_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        let clock = Arc::new(ManualClock::default());
        let db = Arc::new(CompanyDb::open("cobalt", &path, clock.clone()).expect("open"));
        let waker = Arc::new(CountingWaker::default());
        let sink = MailboxDeliverySink::new(Arc::clone(&db), Arc::clone(&waker));

        let envelope = || EffectEnvelope {
            id: "del-1".to_string(),
            kind: "assignment_delivery".to_string(),
            payload: json!({"assignmentId": "a-1", "assigneePersonId": "bob", "generation": 1, "request": "Rebalance the book"}),
        };

        // First pass delivers. Then a "crash" before the scheduler's
        // mark_delivered: the effect stays pending and is re-presented on a later
        // pass, at a later clock — it must deliver again with no duplicate row.
        let first = sink.deliver(&dummy_context("cobalt"), vec![envelope()]).await;
        assert_eq!(first.delivered, vec!["del-1".to_string()]);
        clock.advance(std::time::Duration::from_secs(60)); // a later pass
        let second = sink.deliver(&dummy_context("cobalt"), vec![envelope()]).await;
        assert_eq!(second.delivered, vec!["del-1".to_string()], "re-presented ⇒ delivered again");

        let pending = db.read(|snapshot| mailbox::pending_for(snapshot, "bob").len());
        assert_eq!(pending, 1, "one durable row across the crash-retry");
    }
}
