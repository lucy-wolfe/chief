//! The resident company-lifecycle HTTP surface.
//!
//! # Three routes, and why the shape differs between them
//!
//! * `POST /v1/company/create` and `POST /v1/company/boot` answer
//!   `text/event-stream`. They are long operations whose intermediate steps a
//!   caller renders, and the stream is how those steps arrive — pushed as they
//!   happen, never polled for and never reconstructed from a log.
//! * `POST /v1/company/stop` answers one JSON object. It has no step worth
//!   narrating (see [`super::stop`]).
//!
//! # The stream's own vocabulary
//!
//! ```text
//! event: phase    data: {"phase":"durable-create","slug":"acme","detail":"…"}
//! event: created  data: {"slug":"acme","url":"…","chiefPersonId":"…","session":"…"}
//! event: booted   data: { … the same shape … }
//! event: failed   data: {"code":"lifecycle-failed","detail":"…"}
//! ```
//!
//! Exactly one terminal frame — `created`/`booted` or `failed` — always
//! arrives, including when the operation refuses. A stream that just ends is
//! indistinguishable from a dropped connection, and a caller that cannot tell
//! those apart cannot decide whether to retry.
//!
//! `apps/api` re-emits these three event names verbatim to the browser, so this
//! is the same vocabulary end to end rather than a translation at each hop.
//!
//! # The operation runs on its own task
//!
//! `create`/`boot` are spawned, and the response streams their phase channel.
//! A client that disconnects mid-launch therefore does NOT abort a sequence
//! that is committing durable rows — the phases fall on the floor
//! ([`super::phases::PhaseSink::emit`] is explicitly a no-op then) and the
//! company still finishes coming up. The alternative, tying a half-committed
//! genesis to a browser tab, is the failure Mandate 4 exists to prevent.

use std::path::PathBuf;

use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;

use crate::genesis::{self, LaunchOutcome, LaunchRequest};
use crate::LifecycleError;

use super::lifecycle_sse::REFUSED;
use super::phases::PhaseSink;
use super::{boot, create, stop};

/// `POST /v1/company/boot`'s and `/v1/company/stop`'s body.
///
/// # A DIRECTORY, and no longer a slug
///
/// A company is the directory it occupies, and two directories may hold
/// companies with the same slug — so a slug does not name one. It also does not
/// name one to the CALLER: `apps/web` holds a company as `{key, dir}` and had
/// to run a lookup translating its key back into a display word purely to
/// satisfy this body. That shim is deleted with this field.
///
/// `deny_unknown_fields` is what makes a stale caller's `{"slug":…}` a loud
/// refusal instead of a silently-defaulted directory. A door that accepted both
/// shapes would be the compatibility layer this stage exists to remove.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirRequest {
    /// The canonical absolute directory the company occupies.
    dir: PathBuf,
}

/// `POST /v1/company/create`'s body: a directory and two strings.
///
/// Genesis takes the plain spec. There is no provider, model, or credential in
/// it, on any door — an agent boots as plain Pi on the operator's own defaults.
///
/// The DIRECTORY is required and is never defaulted to this process's own cwd.
/// `chief host` is a resident service started by `scripts/start-stack.ts`; its
/// working directory is wherever that script ran, which is nobody's company. A
/// caller that does not say where a company goes must be refused, not given one
/// somewhere arbitrary.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    /// The canonical absolute directory the new company will occupy.
    dir: PathBuf,
    /// The confirmed company name. The slug is derived from it.
    name: String,
    /// The confirmed company purpose. chiefd derives the CEO's opening
    /// mandate from it.
    purpose: String,
}

/// Build the lifecycle router.
///
/// Stateless: every operation resolves beacond, the company's daemon and its
/// paths from the environment at call time, exactly as the one-shot operator
/// verbs do. A resident process that cached any of those would be holding a
/// location from before the last restart, which is the class of bug ruling D1
/// and E10's addendum both name.
pub(crate) fn router() -> Router {
    Router::new()
        .route("/v1/company/create", post(create_route))
        .route("/v1/company/boot", post(boot_route))
        .route("/v1/company/stop", post(stop_route))
        .route("/v1/health", get(health_route))
}

/// `GET /v1/health`.
///
/// Present because a caller that cannot reach this surface must be able to say
/// so precisely. It reports that the surface is answering and nothing else —
/// judging a company's health is `/v1/docs/health` on that company's own
/// daemon, and answering for it here would be inventing a second authority.
async fn health_route() -> Response {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

/// `POST /v1/company/create`.
///
/// The bootstrap is resolved INSIDE the streamed operation, not before it. A
/// box with no Founder route configured is a refusal like any other, and a
/// caller reading this stream must learn about it the way it learns about every
/// other refusal — as the one terminal `failed` frame — rather than as a status
/// code on a request whose response body it is already parsing as SSE.
#[tracing::instrument(name = "host.company.create", skip_all)]
async fn create_route(Json(request): Json<CreateRequest>) -> Response {
    // The slug is derived, not accepted: chiefd owns what a company is called.
    // Until genesis re-binds the sink to the canonical slug, frames carry this
    // best-effort label rather than an empty one.
    let provisional = genesis::slugify(&request.name);
    // The DIRECTORY and the NAME. The purpose is the caller's own prose and is
    // never logged.
    tracing::info!(
        event = "host.company.create.request",
        company = %request.dir.display(),
        slug = %provisional,
        "the company-create route was called"
    );
    stream_launch(provisional, "created", move |sink| async move {
        let launch = LaunchRequest { name: request.name, purpose: request.purpose };
        create::create(&request.dir, &launch, &sink).await
    })
}

/// `POST /v1/company/boot`.
#[tracing::instrument(name = "host.company.boot", skip_all)]
async fn boot_route(Json(request): Json<DirRequest>) -> Response {
    let dir = request.dir;
    tracing::info!(
        event = "host.company.boot.request",
        company = %dir.display(),
        "the boot route was called"
    );
    // The frame label is the DIRECTORY until `boot` has read the company's own
    // slug back out of its store — which it cannot do before its daemon is up.
    // A label the caller can act on beats a blank one, and the terminal frame
    // carries both.
    let label = dir.display().to_string();
    stream_launch(label, "booted", move |sink| async move { boot::boot(&dir, &sink).await })
}

/// `POST /v1/company/stop`.
#[tracing::instrument(name = "host.company.stop", skip_all)]
async fn stop_route(Json(request): Json<DirRequest>) -> Response {
    tracing::info!(
        event = "host.company.stop.request",
        company = %request.dir.display(),
        "the stop route was called"
    );
    match stop::stop(&request.dir).await {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(error) => {
            tracing::error!(
                event = "host.company.stop.failed",
                company = %request.dir.display(),
                reason = %error,
                "the stop route refused"
            );
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({ "code": REFUSED, "detail": error.to_string() })),
            )
                .into_response()
        }
    }
}

// The stream shape these routes grew now lives in
// [`super::lifecycle_sse`], because the Founder pane's own loopback endpoint
// needs the identical frames and had no way to reach them while they were
// private here (#1051). `stream_launch` below is the thin verb-shaped wrapper
// that remains: the hosted control plane's operations all answer a
// `LaunchOutcome`, so it fixes that type and forwards.

/// Run one narrated operation and serve its phases plus one terminal frame.
fn stream_launch<F, Fut>(slug: String, terminal: &'static str, operation: F) -> Response
where
    F: FnOnce(PhaseSink) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<LaunchOutcome, LifecycleError>> + Send + 'static,
{
    super::lifecycle_sse::stream_lifecycle(slug, terminal, operation)
}

#[cfg(test)]
mod tests {
    // The helpers moved to `super::lifecycle_sse` with the stream they encode
    // (#1051). These assertions did not move and did not change: they are the
    // hosted control plane's own proof that a phase frame and a terminal frame
    // encode the way `apps/web` reads them, and they now hold that line against
    // the shared implementation both surfaces use.
    use crate::genesis::LaunchOutcome;
    use crate::host::lifecycle_sse::{phase_payload, terminal_payload, ABANDONED, REFUSED};
    use crate::host::phases::{Phase, PhaseFrame};
    use crate::LifecycleError;

    fn outcome() -> LaunchOutcome {
        LaunchOutcome {
            slug: "acme".to_string(),
            dir: "/work/acme".to_string(),
            // Deliberately not derived from the slug and not equal to it: a
            // fixture whose key and name are the same string cannot tell a
            // caller that addresses by key from one that addresses by name.
            key: "4d0e2ed2cec4".to_string(),
            url: "http://127.0.0.1:8792".to_string(),
            chief_person_id: "executive-ceo".to_string(),
            session: "org-acme-012345_".to_string(),
        }
    }

    #[test]
    fn a_phase_frame_carries_its_name_slug_and_detail() {
        let frame = PhaseFrame {
            phase: Phase::DurableCreate.name(),
            slug: "acme".to_string(),
            detail: "/orgs".to_string(),
        };
        let (name, body) = phase_payload(&frame);
        assert_eq!(name, "phase");
        assert_eq!(body["phase"], "durable-create");
        assert_eq!(body["slug"], "acme");
        assert_eq!(body["detail"], "/orgs");
    }

    #[test]
    fn a_refusal_is_reported_as_failed_whichever_verb_produced_it() {
        // One error path for a caller, not one per verb — the success name
        // varies, the failure name does not.
        for terminal in ["created", "booted"] {
            // Turbofished: the tail is generic over the terminal body now that
            // the Founder's launch carries a handoff warning these verbs have
            // no concept of, and an `Err`/`None` arm names no type by itself.
            let (name, body) = terminal_payload::<LaunchOutcome>(
                Some(Err(LifecycleError::refused("no"))),
                terminal,
            );
            assert_eq!(name, "failed");
            assert_eq!(body["code"], REFUSED);
            assert_eq!(body["detail"], "no");
        }
    }

    #[test]
    fn a_success_uses_the_verbs_own_terminal_name() {
        let (created, body) = terminal_payload(Some(Ok(outcome())), "created");
        assert_eq!(created, "created");
        assert_eq!(body["slug"], "acme");
        assert_eq!(body["chiefPersonId"], "executive-ceo");

        let (booted, _) = terminal_payload(Some(Ok(outcome())), "booted");
        assert_eq!(booted, "booted");
    }

    /// The terminal frame states the company KEY, because the caller's next
    /// act is to address the company it just created.
    ///
    /// It carried only the slug, and `apps/web`'s founder built
    /// `/c/<slug>` out of it — a route that resolves by key. Two directories
    /// may hold companies with the same slug, so there is no repair that keeps
    /// the slug and stays correct: the server holds the directory and must say
    /// the key, or the caller has to hash a path and become a second producer
    /// of an identity that has exactly one definition.
    #[test]
    fn a_created_frame_states_the_key_that_addresses_the_company() {
        let (_, body) = terminal_payload(Some(Ok(outcome())), "created");
        assert_eq!(body["key"], "4d0e2ed2cec4");
        assert_eq!(body["dir"], "/work/acme");
        // And the two are distinguishable, which is what makes the assertion
        // above about addressing rather than about a coincidence of fixtures.
        assert_ne!(body["key"], body["slug"]);
    }

    /// CREATE TAKES THE DIRECTORY THE COMPANY WILL OCCUPY, and refuses
    /// anything else a caller tries to hand it.
    ///
    /// Two refusals in one rule. A caller cannot hand this route a model route
    /// and an observation, so nothing calling it needs a provider credential;
    /// and a caller cannot omit the directory, because `chief host` is a
    /// resident service whose own cwd is nobody's company.
    /// `deny_unknown_fields` is what makes an old-shaped body a refusal rather
    /// than a silently ignored field — a browser still sending `{"slug":…}`
    /// must be told, not quietly given a company somewhere it did not name.
    #[test]
    fn create_takes_a_directory_and_two_strings_and_refuses_everything_else() {
        let ok: super::CreateRequest =
            serde_json::from_str(r#"{"dir":"/work/acme","name":"Acme","purpose":"Sell anvils"}"#)
                .expect("the browser's body parses");
        assert_eq!(ok.dir, std::path::Path::new("/work/acme"));
        assert_eq!(ok.name, "Acme");
        assert_eq!(ok.purpose, "Sell anvils");

        assert!(
            serde_json::from_str::<super::CreateRequest>(r#"{"name":"Acme","purpose":"p"}"#)
                .is_err(),
            "a create with no directory has nowhere to put the company"
        );
        assert!(serde_json::from_str::<super::CreateRequest>(
            r#"{"dir":"/work/acme","name":"Acme","purpose":"p","bootstrap":{"provider":"x"}}"#
        )
        .is_err());
    }

    /// BOOT AND STOP NAME A DIRECTORY, and a stale slug-shaped body is refused
    /// out loud.
    ///
    /// `apps/web` held a company as `{key, dir}` and ran a lookup to translate
    /// its key into a display word purely to satisfy the old body. Accepting
    /// both shapes would have kept that shim alive; refusing the old one is
    /// what makes it deletable.
    #[test]
    fn boot_and_stop_take_a_directory_and_refuse_the_retired_slug_body() {
        let ok: super::DirRequest =
            serde_json::from_str(r#"{"dir":"/work/acme"}"#).expect("the caller's body parses");
        assert_eq!(ok.dir, std::path::Path::new("/work/acme"));

        assert!(
            serde_json::from_str::<super::DirRequest>(r#"{"slug":"acme"}"#).is_err(),
            "a slug names no company: two directories may hold one with the same name"
        );
    }

    #[test]
    fn an_abandoned_task_still_produces_a_terminal_frame() {
        // Silence and "it failed" are different answers, and a caller that
        // cannot tell them apart cannot decide whether to retry.
        let (name, body) = terminal_payload::<LaunchOutcome>(None, "created");
        assert_eq!(name, "failed");
        assert_eq!(body["code"], ABANDONED);
    }
}
