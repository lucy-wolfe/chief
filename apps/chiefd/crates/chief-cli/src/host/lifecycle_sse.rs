//! One narrated lifecycle operation, served as Server-Sent Events.
//!
//! # Why this is its own module
//!
//! `host/router.rs` grew this shape for the hosted control plane
//! (`/v1/company/create`, `/v1/company/boot`) and kept it private, so the
//! Founder pane's own loopback endpoint — the surface with the LONGEST wait in
//! the product — had no way to reuse it and narrated nothing at all. An
//! operator watching `chiefd_launch_company` saw a bare spinner for four and a
//! half minutes while chiefd emitted every one of these phases into a channel
//! whose receiver had been dropped.
//!
//! Two callers, one stream shape. The alternative was a second copy of the
//! frame encoding in `founder.rs`, which is how the phase names would come to
//! mean two different things on two surfaces.
//!
//! # The terminal frame is generic, the phases are not
//!
//! Every narrated operation emits the same [`PhaseFrame`]s, and each one ends
//! with exactly ONE terminal frame whose body is that operation's own outcome:
//! `created` for the hosted create, `launched` for a Founder launch (which
//! carries a handoff warning the others have no concept of). So the phase half
//! is fixed and the tail is the caller's, rather than a union type that grows a
//! variant per verb.

use std::convert::Infallible;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{self, Stream};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

use super::phases::{PhaseFrame, PhaseSink};
use crate::LifecycleError;

/// How often a quiet lifecycle stream sends a comment line.
///
/// A launch can be quiet for a long time — a cold daemon start dominates it —
/// and every intermediary between chiefd and its reader has its own idle
/// timeout. Fifteen seconds matches `/v1/docs/watch`'s heartbeat so both of
/// chiefd's streams have one liveness cadence rather than two.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(15);

/// The one refusal code every lifecycle failure carries. Callers branch on
/// `code`, never on the message.
pub(crate) const REFUSED: &str = "lifecycle-failed";

/// The code for a task that ended without reporting. Distinct from [`REFUSED`]
/// because the operation's own outcome is genuinely unknown here, and telling a
/// caller "it failed" when it may well have succeeded is worse than saying so.
pub(crate) const ABANDONED: &str = "lifecycle-abandoned";

/// Run one narrated operation and serve its phases plus one terminal frame.
///
/// `terminal` is the success event name (`created`/`booted`/`launched`); a
/// refusal always answers `failed` regardless of which operation produced it,
/// so a caller needs one error path rather than one per verb.
pub(crate) fn stream_lifecycle<F, Fut, T>(
    slug: String,
    terminal: &'static str,
    operation: F,
) -> Response
where
    F: FnOnce(PhaseSink) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<T, LifecycleError>> + Send + 'static,
    T: Serialize + Send + 'static,
{
    let (sink, frames) = PhaseSink::channel(slug);
    let (done, outcome) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = operation(sink).await;
        // The sink is moved into `operation` and dropped when it returns, which
        // is what closes the phase channel. The terminal frame is only built
        // after this send, so the receiver has already drained every phase:
        // ordering by construction, not a race the reader has to tolerate.
        drop(done.send(result));
    });
    Sse::new(lifecycle_stream(frames, outcome, terminal))
        .keep_alive(KeepAlive::new().interval(HEARTBEAT).text("hb"))
        .into_response()
}

/// Every phase frame, then exactly one terminal frame.
pub(crate) fn lifecycle_stream<T: Serialize + Send + 'static>(
    frames: UnboundedReceiver<PhaseFrame>,
    outcome: tokio::sync::oneshot::Receiver<Result<T, LifecycleError>>,
    terminal: &'static str,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let phases = stream::unfold(frames, |mut frames| async move {
        frames.recv().await.map(|frame| {
            let (name, payload) = phase_payload(&frame);
            (encode(name, &payload), frames)
        })
    });
    let tail = stream::once(async move {
        // A `RecvError` means the task vanished without answering, which only
        // happens if the runtime tore it down. `None` says exactly that.
        let (name, payload) = terminal_payload(outcome.await.ok(), terminal);
        encode(name, &payload)
    });
    futures_util::stream::StreamExt::chain(phases, tail)
}

/// The event name and body for one phase frame.
pub(crate) fn phase_payload(frame: &PhaseFrame) -> (&'static str, Value) {
    ("phase", to_value(frame))
}

/// The event name and body for the one terminal frame.
///
/// `None` is a run that ended without reporting — reported as its own code
/// rather than folded into an ordinary refusal, because the operation's outcome
/// is genuinely unknown and a caller deciding whether to retry needs to know
/// that.
pub(crate) fn terminal_payload<T: Serialize>(
    outcome: Option<Result<T, LifecycleError>>,
    terminal: &'static str,
) -> (&'static str, Value) {
    match outcome {
        Some(Ok(launched)) => (terminal, to_value(&launched)),
        Some(Err(error)) => {
            ("failed", serde_json::json!({ "code": REFUSED, "detail": error.to_string() }))
        }
        None => (
            "failed",
            serde_json::json!({
                "code": ABANDONED,
                "detail": "the lifecycle run ended without reporting an outcome",
            }),
        ),
    }
}

/// Serialize a frame body.
///
/// Every value passed here is a plain `derive`d struct of owned strings, so the
/// error arm is unreachable in practice. It is still written rather than
/// unwrapped: `unwrap` is denied in this workspace, and a panic inside a
/// response stream drops the connection with no terminal frame — precisely the
/// silence this module exists to prevent.
fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|error| {
        serde_json::json!({ "code": REFUSED, "detail": format!("unserializable frame: {error}") })
    })
}

/// One SSE event, named and carrying a JSON body.
fn encode(name: &'static str, payload: &Value) -> Result<Event, Infallible> {
    Ok(Event::default().event(name).data(payload.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::phases::Phase;
    use futures_util::StreamExt as _;

    /// A terminal body with a field the hosted control plane has no concept
    /// of — the point of making the tail generic.
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Launched {
        slug: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        handoff_warning: Option<String>,
    }

    fn launched(warning: Option<&str>) -> Launched {
        Launched { slug: "acme".to_string(), handoff_warning: warning.map(str::to_string) }
    }

    /// #1051: the head of a launch used to emit nothing at all, so these two
    /// names are the ones that make a four-minute wait attributable.
    #[test]
    fn the_head_of_a_launch_has_its_own_phase_names() {
        assert_eq!(Phase::Preflight.name(), "preflight");
        assert_eq!(Phase::BeacondEnsure.name(), "beacond-ensure");
        assert_eq!(Phase::CompanyClaim.name(), "company-claim");
        assert_eq!(Phase::Handover.name(), "handover");
        assert_eq!(Phase::HandoverComplete.name(), "handover-complete");
    }

    /// Starting beacond and finding it already up are DIFFERENT phases, not one
    /// phase with a different detail string. The cold path spawns a process and
    /// waits on a 5000 ms budget; the warm path is one probe. A shared name
    /// could not tell a reader which of the two it was waiting through.
    #[test]
    fn the_cold_beacond_path_is_its_own_phase() {
        assert_eq!(Phase::BeacondStarting.name(), "beacond-starting");
        assert_eq!(Phase::BeacondReady.name(), "beacond-ready");
        assert_ne!(Phase::BeacondStarting.name(), Phase::BeacondEnsure.name());
    }

    #[test]
    fn a_founder_terminal_frame_carries_the_handoff_warning_when_there_is_one() {
        let (name, body) = terminal_payload(Some(Ok(launched(Some("nobody moved")))), "launched");
        assert_eq!(name, "launched");
        assert_eq!(body["slug"], "acme");
        assert_eq!(body["handoffWarning"], "nobody moved");
    }

    /// A launch that DID hand the operator over says nothing about a warning,
    /// rather than saying an empty one. The Founder reads the key's absence.
    #[test]
    fn a_clean_handover_omits_the_warning_entirely() {
        let (_, body) = terminal_payload(Some(Ok(launched(None))), "launched");
        assert!(body.get("handoffWarning").is_none(), "{body}");
    }

    #[test]
    fn a_refusal_is_failed_even_on_the_founder_stream() {
        let (name, body) =
            terminal_payload::<Launched>(Some(Err(LifecycleError::refused("no"))), "launched");
        assert_eq!(name, "failed");
        assert_eq!(body["code"], REFUSED);
        assert_eq!(body["detail"], "no");
    }

    #[test]
    fn an_abandoned_founder_launch_is_not_reported_as_a_refusal() {
        let (name, body) = terminal_payload::<Launched>(None, "launched");
        assert_eq!(name, "failed");
        assert_eq!(body["code"], ABANDONED);
    }

    /// Every phase reaches the reader BEFORE the terminal frame, which is the
    /// ordering the whole feature rests on: a Founder that received its phases
    /// after the answer would have learned nothing while it waited.
    #[tokio::test]
    async fn every_phase_is_streamed_before_the_one_terminal_frame() {
        let (sink, frames) = PhaseSink::channel("acme");
        let (done, outcome) = tokio::sync::oneshot::channel::<Result<Launched, LifecycleError>>();
        sink.emit(Phase::BeacondEnsure, "");
        sink.emit(Phase::CompanyClaim, "/orgs");
        sink.emit(Phase::DurableCreate, "");
        drop(sink);
        drop(done.send(Ok(launched(None))));

        let events: Vec<_> = lifecycle_stream(frames, outcome, "launched").collect().await;
        assert_eq!(events.len(), 4, "three phases and exactly one terminal frame");
    }

    /// A failure emits the phase that EXPLAINS it before it reports that it
    /// failed, so a reader sees where it stopped before it sees that it did.
    #[tokio::test]
    async fn a_failure_streams_its_explaining_phase_first() {
        let (sink, frames) = PhaseSink::channel("acme");
        let (done, outcome) = tokio::sync::oneshot::channel::<Result<Launched, LifecycleError>>();
        sink.emit(Phase::DurableCreateFailed, "no");
        drop(sink);
        drop(done.send(Err(LifecycleError::refused("no"))));

        let events: Vec<_> = lifecycle_stream(frames, outcome, "launched").collect().await;
        assert_eq!(events.len(), 2, "one explaining phase, then the failure");
    }
}
