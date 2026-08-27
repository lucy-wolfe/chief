//! Unit D / D2 — crash injection at the reconcile-cycle seam boundaries.
//!
//! A daemon dies at the worst moment. These cases fix each seam's recovery
//! contract:
//!
//!   D2.3  converge-intent orphan — a `Pending` converge row that outlived a
//!         crash is aborted at startup, not replayed (runtime is not replayable).
//!   D2.4  the delivery **two-commit gap** — the sink dispatches and stages, the
//!         process dies BEFORE the scheduler's `mark_delivered` commit; on retry
//!         the writer re-presents the same effect id; the sink must redeliver as
//!         an idempotent no-op success, so delivery is exactly-once observable.
//!
//! #751/P8-P10 removed D2.1 (mid-observe) and D2.2 (mid-apply). Both were
//! written against a scripted runtime server driven through chiefd's own host
//! executor, and both asserted about a converge plan of pane steps. chiefd
//! neither observes a display nor applies a plan any more: it publishes a
//! desired roster and an action stream, and the crash seams those two cases
//! guarded now sit inside the operator client. They are not re-homed here
//! because there is nothing left in THIS crate for them to be about.
//!
//! ACTIVATION: move to `crates/chiefd-daemon/tests/crash_injection.rs`.

// clippy.toml's `allow-*-in-tests` switches are keyed off `#[test]`-attributed
// functions specifically, not "this whole file lives under tests/" — a
// helper called only from tests (here, `IdempotentSink`'s lock handling) does
// not inherit the exemption, hence an explicit file-level allow rather than
// scattering `#[allow]` per call site.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use chiefd_core::clock::WallMillis;
use chiefd_core::ledger::Ledgers;
use chiefd_core::runtime::converge_intent::{self, ConvergeIntentBody};

// --- D2.3: an orphaned converge-intent row is aborted, never replayed --------

fn converge_body() -> ConvergeIntentBody {
    ConvergeIntentBody {
        shadow: false,
        sweep_live: false,
        predicted_kill_panes: 2,
        predicted_respawn_persons: 1,
        pointer_clears: 0,
        steps: vec![
            "stop signal-researcher: not-desired".into(),
            "restart quant-head @g4 after 0ms: generation-drift".into(),
        ],
    }
}

#[test]
fn d2_3_orphaned_converge_intent_is_aborted_at_startup_not_replayed() {
    let mut ledgers = Ledgers::empty(WallMillis(1_000));
    // A cycle opened its audit row and then the process crashed mid-apply.
    converge_intent::open(&mut ledgers, "converge:1", &converge_body()).expect("open");
    assert!(converge_intent::read(&ledgers, "converge:1").expect("read").is_some());

    // Startup recovery: converge rows are aborted (closed), because a stopped
    // process cannot be un-stopped — recovery is a fresh pass against a fresh
    // observation, never a replay of a recorded one.
    let aborted = converge_intent::abort_open(&mut ledgers);
    assert_eq!(aborted, vec!["converge:1".to_string()], "the orphan is named and closed");
    assert!(
        converge_intent::read(&ledgers, "converge:1").expect("read").is_none(),
        "no converge row survives startup to be replayed",
    );
}

// --- D2.4: the delivery two-commit gap is idempotent per effect id ------------
//
// Models the seam: `run_mailbox_wake` reads undelivered effects from the
// committed snapshot, hands them to the sink, then commits `mark_delivered` in a
// SEPARATE transaction. A crash in the gap loses the `mark_delivered` commit but
// not the host send. The recovery invariant is on the SINK: re-presented ids are
// no-op successes, so the transport fires once even though `deliver` ran twice.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// A stand-in for od-delivery-mailbox's concrete sink that honours the
/// idempotency contract: it remembers what it has physically sent and treats a
/// re-presented id as an already-satisfied success (the real sink checks the
/// pane's delivered-watermark; this fake checks a set).
#[derive(Default)]
struct IdempotentSink {
    /// effect id -> number of times the underlying transport actually fired.
    transport_sends: Mutex<BTreeMap<String, usize>>,
}

impl IdempotentSink {
    /// The contract `DeliverySink::deliver` must satisfy, distilled: every id in
    /// `ids` ends up `delivered`, but the transport fires at most once per id
    /// across repeated presentations.
    fn deliver(&self, ids: &[&str]) -> Vec<String> {
        let mut sent = self.transport_sends.lock().unwrap();
        let mut delivered = Vec::new();
        for id in ids {
            let count = sent.entry((*id).to_string()).or_insert(0);
            if *count == 0 {
                // First presentation: physically send.
                *count += 1;
            }
            // Either way the effect is now delivered (idempotent success).
            delivered.push((*id).to_string());
        }
        delivered
    }

    fn transport_count(&self, id: &str) -> usize {
        *self.transport_sends.lock().unwrap().get(id).unwrap_or(&0)
    }
}

#[test]
fn d2_4_redelivery_after_a_lost_mark_delivered_commit_fires_the_transport_once() {
    let sink = IdempotentSink::default();

    // Pass 1: the scheduler presents effect "e-42"; the sink dispatches it and
    // stages the send... then the process crashes BEFORE `mark_delivered`.
    let first = sink.deliver(&["e-42"]);
    assert_eq!(first, vec!["e-42".to_string()], "pass 1 delivered");

    // Recovery: the snapshot still shows "e-42" undelivered (the commit was
    // lost), so the scheduler re-presents the very same id.
    let second = sink.deliver(&["e-42"]);
    assert_eq!(second, vec!["e-42".to_string()], "pass 2 reports delivered (idempotent no-op)");

    // The decisive assertion: the underlying transport fired EXACTLY ONCE despite
    // two `deliver` passes — at-least-once dispatch + at-most-once record =
    // exactly-once observable delivery across the crash gap.
    assert_eq!(sink.transport_count("e-42"), 1, "no double-send across the two-commit gap");
}

// --- D2.4 wiring to the concrete sink (STUB) ---------------------------------
#[test]
#[ignore = "Unit D (#879 finding): MailboxDeliverySink exists, but no seam in this codebase lets a test genuinely fail a CompanyDb::mutate mid-transaction to reproduce the two-commit gap — see this fn's doc comment"]
fn d2_4_concrete_delivery_sink_is_idempotent_under_re_presentation() {
    // #879 finding: `chiefd_core::runtime::delivery_sink::MailboxDeliverySink`
    // is real and has its own test,
    // `a_restaged_effect_after_a_crash_stays_one_durable_row`
    // (`delivery_sink.rs`) — but that test proves re-presentation is
    // idempotent by calling `deliver` twice with the SAME id, never by
    // actually interrupting a write between the sink's staging commit and the
    // scheduler's separate `mark_delivered` commit. This case's own TODO asks
    // for exactly that interruption: "drop the writer BEFORE the
    // `mark_delivered` commit (inject a mutate failure)". `CompanyDb::mutate`
    // has no injectable failure seam anywhere in this codebase — every
    // existing crash-injection test in this suite (`host_txn_crash.rs`,
    // D2.1–D2.2 above) crashes a REAL PROCESS via SIGKILL or fails a SCRIPTED
    // RUNTIME REPLY, never a mid-transaction database write, because
    // `chiefd-core`'s own design goal is that the single-writer actor's
    // mutations are atomic by construction (README §5.2) — there is
    // deliberately no "fail here" knob on `mutate` to inject.
    //
    // Two ways to close this for real, neither of which is "write the test
    // and hope": (a) a genuine process-level crash test analogous to
    // `host_txn_crash.rs`, killing the daemon between the two real commits
    // (heavier — needs a `crash_child`-style re-exec harness, not a unit-d
    // helper), or (b) a purpose-built injectable seam on the mailbox-wake
    // path specifically, mirroring the `BlockingWait`
    // fn-pointer pattern — a real production-code change, not a test-only
    // addition, and outside what a `tests/unit-d` file should decide to add
    // unilaterally.
    //
    // Retiring rather than writing a test that calls `deliver` twice again
    // and relabels the already-real D2.4 fake-sink case as "concrete."
    unimplemented!("retired per #879 finding above — CompanyDb::mutate has no injectable mid-transaction failure seam to reproduce the two-commit gap for real")
}
