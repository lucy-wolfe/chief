//! The company-lifecycle phase vocabulary and the channel it is pushed on.
//!
//! # Why this exists
//!
//! A company launch is the one operator action long enough that "it is still
//! going" has to be visible while it happens. The retired shape printed
//! `ChiefD launch: phase=<name> …` to stdout and had `apps/api` spawn the
//! launcher as a subprocess and re-derive structure from those lines with a
//! regular expression. That made a log format a wire contract: a phase name
//! could not be renamed without breaking a consumer that had never been told
//! it was one, and a consumer could not learn a phase existed except by
//! matching text.
//!
//! Here a phase is a typed value with a fixed name, pushed on a channel, and
//! rendered as an SSE frame at the edge. The name is still stable — that is the
//! point — but it is stable because [`Phase::name`] is the single place it is
//! written, not because two programs agreed on a regex.
//!
//! # The vocabulary is closed
//!
//! [`Phase`] is an enum, not a string. A launch step that wants to say
//! something new adds a variant here, and every consumer's exhaustive match
//! tells its author about it. Nothing in this crate constructs a phase from a
//! caller-supplied string, so no code path can invent one.
//!
//! # Reactive (Mandate 1)
//!
//! [`PhaseSink`] is an unbounded `tokio` channel. Emitting never blocks, never
//! waits for a reader, and never fails in a way a launch step has to handle: a
//! caller that hung up mid-launch must not change what the launch does. The
//! stream half is drained by the SSE response, so a phase reaches the browser
//! as the step that produced it returns — nothing polls, and nothing buffers a
//! whole launch to report it at the end.

use serde::Serialize;
use tokio::sync::mpsc;

/// One step of a company lifecycle operation.
///
/// The names are the published contract (`Phase::name`), carried over verbatim
/// from the retired `ChiefD launch: phase=…` lines so a consumer's rendering of
/// a launch is unchanged by the transport moving under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The host preflight — the very FIRST thing a launch does, and measured
    /// at 673 ms on a live box, so not nothing.
    Preflight,
    /// Discovery is being checked: is beacond answering?
    ///
    /// #1051: the head of a launch used to emit NOTHING. `create_company` runs
    /// before the first frame, so a Founder pane watching a four-and-a-half
    /// minute launch saw silence for the whole wait and then four frames in
    /// three seconds. A step nobody narrates is a step nobody can attribute.
    BeacondEnsure,
    /// beacond was NOT answering and is being started.
    ///
    /// Deliberately a different phase from [`Self::BeacondEnsure`], not a
    /// detail string on it. The warm path is one probe and is over before a
    /// human could read it; the cold path spawns a process and waits on a
    /// 5000 ms budget, and on a loaded box that is a wait somebody is sitting
    /// through. Only one of the two is worth saying out loud, and a shared
    /// name could not tell them apart.
    BeacondStarting,
    /// beacond bound its port and answered, after having been started here.
    BeacondReady,
    /// The company row is being claimed in beacond. This is the moment the
    /// company begins to exist.
    CompanyClaim,
    /// The company's own daemon is being spawned.
    CompanyDaemonStart,
    /// That daemon registered its address and answered a health probe.
    CompanyDaemonReady,
    /// Durable genesis is beginning.
    DurableCreate,
    /// Genesis committed: manifest, model catalogue, materialization
    /// checkpoint and person contracts, in one transaction.
    DurableCreateComplete,
    /// Genesis did not commit. The beacond company claim remains; nothing
    /// durable was written.
    DurableCreateFailed,
    /// Tearing the daemon down after a failed genesis — a daemon must never
    /// persist with nothing to justify it.
    CompanyDaemonStop,
    /// That teardown succeeded.
    CompanyDaemonStopped,
    /// That teardown itself failed. Reported rather than swallowed: the
    /// original genesis failure is still the outcome, but an operator now
    /// knows a daemon may be left behind.
    CompanyDaemonStopFailed,
    // TOMBSTONE (chief-home-is-cwd §4c): `CeoPrepare` (`"ceo-prepare"`) and
    // `CeoPrepareFailed` (`"ceo-prepare-failed"`). They narrated the
    // `prepare-ceo-only` POST, which is deleted with the daemon-side CEO boot.
    //
    // These are PUBLISHED CONTRACT strings, so removing them is a product
    // change and not a rename, and the question it has to answer is what a
    // caller sees in their place. `DurableCreateComplete` is now genesis's last
    // word and it is the truthful one: after it the company is durable AND
    // CEO-only, because an omitted launch intent is an empty allow-list. No
    // silent gap opens, because the step they announced no longer happens —
    // there is nothing left between `durable-create-complete` and `chief-start`
    // that can fail unreported.
    //
    // `ChiefStart`/`ChiefStartFailed` below deliberately SURVIVE. They were listed
    // beside these two in the plan, but they narrate different work: the
    // api-host tail's two durable writes in `host::create::activate_ceo`
    // (`/v1/org/converge-safety/set-actuation-config` and `/v1/org/person/start`),
    // both of which still exist and can still refuse. Deleting them would leave
    // a `chief create` that fails while starting the CEO reporting nothing at
    // all, which is the one outcome worse than a stale phase name.
    /// The CEO is durably started.
    ChiefStart,
    /// Starting the CEO was refused.
    ChiefStartFailed,
    /// The company is being brought up and the operator moved into its CEO.
    /// Founder-only: no other surface hands a terminal over.
    Handover,
    /// The handover finished. It does NOT claim the operator moved — an
    /// unattended launch legitimately has nobody to move — only that the step
    /// is over, which is what a reader waiting on the stream needs.
    HandoverComplete,
}

impl Phase {
    /// The published wire name.
    ///
    /// These strings are the contract `apps/web` renders. Changing one is a
    /// product change, not a rename.
    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::BeacondEnsure => "beacond-ensure",
            Self::BeacondStarting => "beacond-starting",
            Self::BeacondReady => "beacond-ready",
            Self::CompanyClaim => "company-claim",
            Self::CompanyDaemonStart => "company-daemon-start",
            Self::CompanyDaemonReady => "company-daemon-ready",
            Self::DurableCreate => "durable-create",
            Self::DurableCreateComplete => "durable-create-complete",
            Self::DurableCreateFailed => "durable-create-failed",
            Self::CompanyDaemonStop => "company-daemon-stop",
            Self::CompanyDaemonStopped => "company-daemon-stopped",
            Self::CompanyDaemonStopFailed => "company-daemon-stop-failed",
            Self::ChiefStart => "chief-start",
            Self::ChiefStartFailed => "chief-start-failed",
            Self::Handover => "handover",
            Self::HandoverComplete => "handover-complete",
        }
    }
}

/// One phase frame as it appears in an SSE `data:` field.
///
/// `slug` is present from the first frame, including on `create`, because
/// chiefd derives the slug from the confirmed name before it claims anything —
/// a caller never has to guess which company its own stream is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PhaseFrame {
    /// The phase name (`Phase::name`).
    pub(crate) phase: &'static str,
    /// The company this frame is about.
    pub(crate) slug: String,
    /// Human-readable context for the step: a URL, a path, a refusal message.
    /// Never parsed by a consumer — the phase name carries the meaning.
    pub(crate) detail: String,
}

/// The write half a lifecycle operation emits into.
///
/// Cloneable and cheap: every orchestration step takes `&PhaseSink` and emits
/// directly, so no step has to thread a return value back up to a reporter.
#[derive(Debug, Clone)]
pub(crate) struct PhaseSink {
    slug: String,
    sender: mpsc::UnboundedSender<PhaseFrame>,
}

impl PhaseSink {
    /// Create a sink bound to one company, plus the stream half to serve.
    pub(crate) fn channel(slug: impl Into<String>) -> (Self, mpsc::UnboundedReceiver<PhaseFrame>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self { slug: slug.into(), sender }, receiver)
    }

    /// Re-bind the sink to a slug the operation only learned later.
    ///
    /// `create` claims its slug before it emits anything, so this exists for
    /// the one honest case: a request that names a company by a value chiefd
    /// then canonicalises. The frames already sent keep the label they were
    /// sent with — rewriting history on a stream a client is already reading
    /// is not something a sink is allowed to do.
    #[must_use]
    pub(crate) fn with_slug(&self, slug: impl Into<String>) -> Self {
        Self { slug: slug.into(), sender: self.sender.clone() }
    }

    /// Push one phase.
    ///
    /// Deliberately infallible. A disconnected receiver means the client hung
    /// up; a launch that is already committing rows must not change course
    /// because nobody is watching, and a step must not have to decide what to
    /// do about it. The send result is dropped for exactly that reason.
    pub(crate) fn emit(&self, phase: Phase, detail: impl Into<String>) {
        let frame =
            PhaseFrame { phase: phase.name(), slug: self.slug.clone(), detail: detail.into() };
        drop(self.sender.send(frame));
    }
}

#[cfg(test)]
mod tests {
    use super::{Phase, PhaseSink};

    /// The names crossed a process boundary as text before they were an enum,
    /// and `apps/web` still renders exactly these. Pinning them here is what
    /// makes a rename show up as a failing test rather than as a browser that
    /// silently stops narrating a launch.
    #[test]
    fn the_phase_names_are_the_published_contract() {
        let expected = [
            (Phase::CompanyDaemonStart, "company-daemon-start"),
            (Phase::CompanyDaemonReady, "company-daemon-ready"),
            (Phase::DurableCreate, "durable-create"),
            (Phase::DurableCreateComplete, "durable-create-complete"),
            (Phase::DurableCreateFailed, "durable-create-failed"),
            (Phase::CompanyDaemonStop, "company-daemon-stop"),
            (Phase::CompanyDaemonStopped, "company-daemon-stopped"),
            (Phase::CompanyDaemonStopFailed, "company-daemon-stop-failed"),
            (Phase::ChiefStart, "chief-start"),
            (Phase::ChiefStartFailed, "chief-start-failed"),
        ];
        for (phase, name) in expected {
            assert_eq!(phase.name(), name);
        }
    }

    #[test]
    fn every_frame_carries_the_sink_slug() {
        let (sink, mut frames) = PhaseSink::channel("acme");
        sink.emit(Phase::DurableCreate, "at /orgs");
        let frame = frames.try_recv().expect("one frame was emitted");
        assert_eq!(frame.slug, "acme");
        assert_eq!(frame.phase, "durable-create");
        assert_eq!(frame.detail, "at /orgs");
    }

    #[test]
    fn a_hung_up_reader_does_not_stop_a_launch() {
        // The whole reason `emit` returns nothing: dropping the stream half is
        // what a closed browser tab looks like, and it must not be able to
        // interrupt a sequence that is committing durable rows.
        let (sink, frames) = PhaseSink::channel("acme");
        drop(frames);
        sink.emit(Phase::ChiefStart, "");
        sink.emit(Phase::ChiefStart, "");
    }

    #[test]
    fn rebinding_the_slug_leaves_already_sent_frames_alone() {
        let (sink, mut frames) = PhaseSink::channel("provisional");
        sink.emit(Phase::CompanyDaemonStart, "");
        let rebound = sink.with_slug("acme");
        rebound.emit(Phase::CompanyDaemonReady, "");
        let first = frames.try_recv().expect("first frame");
        let second = frames.try_recv().expect("second frame");
        assert_eq!(first.slug, "provisional");
        assert_eq!(second.slug, "acme");
    }
}
