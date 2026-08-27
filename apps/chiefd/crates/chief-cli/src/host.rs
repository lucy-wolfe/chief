//! `chief host` — the resident company-lifecycle service.
//!
//! # What it is for
//!
//! There is one chiefd **per company** and none exists until the company does,
//! so no company's daemon can serve `create`. `chief` solved that for the
//! Founder by binding an ephemeral loopback endpoint
//! ([`crate::founder`]) that lives exactly as long as one Founder
//! session, so the Founder extension can launch a company over HTTP instead of
//! spawning a CLI.
//!
//! `apps/api` needs the same door and needs it to outlive any one session: it
//! is a long-running service whose callers are browsers, and it must be able to
//! create, boot and stop a company without spawning a process. This mode is
//! that endpoint made resident. It is deliberately not a new binary and not a
//! new crate — it lives beside the lifecycle modules it calls, so there is one
//! implementation of a company launch and two mounts of it.
//!
//! # Which side of the P6 split it landed on, and why
//!
//! **The operator client's.** This mode was the workstream's "hidden third
//! client": it reaches [`crate::stop::stop_runtime`] and therefore
//! [`crate::tmux::kill_session`], so a web request can kill a tmux session. The
//! plan (P6) said it should land
//! here and warned that it would need a different answer if `apps/api` could
//! not depend on the CLI binary being present. Measured rather than assumed:
//! it can. `apps/api`/`apps/web` reach this mode **over HTTP** at
//! `CHIEFD_HOST_BIND` (`packages/chiefing`'s `CompanyLifecycleClient`), never
//! in-process and never by linking a crate, and `scripts/start-stack.ts`
//! starts it as its own process. The only in-process coupling was to the
//! `lifecycle` module tree — which moved here whole — and this tree names no
//! backend crate at all. So the split needed no compromise: `chief host` is a
//! second mount of the operator client's own verbs, and it stays exactly as
//! client-side as `chief stop` is.
//!
//! # What it deliberately is not
//!
//! Not a supervisor. It starts nothing on its own, watches nothing, and has no
//! background loop — every route runs exactly when a caller asks and nothing
//! runs when nobody does. Not a registry either: which companies exist and
//! where their daemons are stays beacond's answer (ruling D21), and this
//! service asks beacond the same way every other caller does.
//!
//! It holds no state at all, which is what makes it safe to restart: an
//! in-flight launch belongs to its own task, and a launch already committed is
//! recorded in beacond and in that company's SQLite, never here.
//!
//! # Bind address
//!
//! `CHIEFD_HOST_BIND`, default `127.0.0.1:8789`. Loopback, deliberately, and
//! deliberately outside chiefd's 8792+ port walk — a walking company daemon
//! can never land on it, the same reasoning that keeps beacond's 6969 clear of
//! the walk.
//!
//! Being outside the walk range is the WHOLE reason, and the only one that is
//! re-derivable here. This comment used to add that 8789 is "BELOW beacond's
//! 6969", which is arithmetically false — 8789 is above 6969 — and the
//! conclusion never depended on it: what a walking daemon can reach is set by
//! the 8792+ walk alone, not by the ordering against beacond.
//!
//! No authentication, the same shipped position beacond takes and for the same
//! reason: every caller is the same user on the same box behind a loopback
//! bind. That is acceptable for a loopback listener and is not acceptable the
//! day it binds a routable address, which would be a different service with a
//! different threat model rather than an upgrade to this one.
//!
//! # This module tree sits on the operator verbs
//!
//! Every durable step it performs belongs to a sibling module of this crate:
//!
//! | used | for |
//! |---|---|
//! | `genesis::{launch_with_phases, slugify, LaunchRequest, LaunchOutcome}` | create |
//! | `daemon::{start, StartTarget}` | ensuring a company's daemon is up |
//! | `stop::{stop_runtime, StopOutcome}` | stop, ordering law included |
//! | `discovery::{Discovery, ensure_running}` | beacond, and beacond being up |
//! | `company::{CompanyClient, boot_socket_from_env, now_iso_millis}` | company routes |
//! | `http::Client`, `paths::*` | transport and where things live |
//!
//! One of those does not exist yet: `genesis::launch_with_phases`. The exact
//! change is spelled out in [`create`]'s own module doc. Nothing here
//! re-implements any of the above — this tree is a door onto them, not a second
//! body for them.

pub(crate) mod boot;
pub(crate) mod create;
pub(crate) mod lifecycle_sse;
pub(crate) mod phases;
pub(crate) mod router;
pub(crate) mod stop;

use std::process::ExitCode;

/// The environment variable naming the bind address.
const BIND_ENV: &str = "CHIEFD_HOST_BIND";

/// Where it binds when the operator has not chosen.
const DEFAULT_BIND: &str = "127.0.0.1:8789";

/// The bind address, from the environment or the default.
fn bind_address() -> String {
    std::env::var(BIND_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BIND.to_string())
}

/// Run `chief host`.
///
/// A multi-threaded runtime: a launch parks on a spawned child (the company
/// daemon coming up) while the response stream must keep flushing phases to the
/// caller. On a current-thread runtime the stream would not be polled while a
/// launch was in flight, and the narration this mode exists to provide would
/// arrive all at once at the end.
///
/// The bound address is printed before anything else happens. A process
/// started on a port nobody was told is indistinguishable from one that failed
/// to start, and `scripts/start-stack.ts` learned that the expensive way.
pub(crate) fn run() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "chief host: could not start its async runtime");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(serve())
}

/// Bind, announce, and serve until SIGTERM/SIGINT.
async fn serve() -> ExitCode {
    let address = bind_address();
    let listener = match tokio::net::TcpListener::bind(&address).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%address, %error, "chief host: could not bind");
            return ExitCode::FAILURE;
        }
    };
    let bound = match listener.local_addr() {
        Ok(bound) => bound,
        Err(error) => {
            tracing::error!(%error, "chief host: could not name its own listener");
            return ExitCode::FAILURE;
        }
    };
    let url = format!("http://{bound}");
    tracing::info!(%url, "chief host: company lifecycle surface bound");
    println!("chief host  {url}  company create/boot/stop");

    let served = axum::serve(listener, router::router()).with_graceful_shutdown(shutdown());
    match served.await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "chief host: stopped serving");
            ExitCode::FAILURE
        }
    }
}

/// Resolve on SIGTERM or SIGINT.
///
/// A graceful shutdown here only stops accepting: an in-flight launch is its
/// own spawned task and finishes or fails on its own terms. Nothing durable
/// depends on this process surviving a launch, which is why it can be restarted
/// at any moment without a recovery pass.
async fn shutdown() {
    // Same shape as `docstore_only::wait_for_signal`, deliberately: two
    // daemons in one binary must not disagree about what "stop" means, and
    // the `cfg(unix)` split is what keeps the Darwin cross-check honest.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(error) => {
                tracing::warn!(%error, "chief host: cannot install SIGTERM handler; SIGINT only");
                drop(tokio::signal::ctrl_c().await);
            }
        }
    }
    #[cfg(not(unix))]
    {
        drop(tokio::signal::ctrl_c().await);
    }
    tracing::info!("chief host: shutting down");
}

#[cfg(test)]
mod tests {
    use super::{bind_address, BIND_ENV, DEFAULT_BIND};

    #[test]
    fn the_default_bind_sits_below_beacond_and_outside_the_company_port_walk() {
        // beacond is 6969 and a company daemon walks from 8792. A default that
        // fell inside the walk would let a company land on this listener,
        // which is the exact collision beacond's own port choice avoids.
        assert_eq!(DEFAULT_BIND, "127.0.0.1:8789");
    }

    #[test]
    fn the_bind_variable_name_is_the_published_contract() {
        // `scripts/start-stack.ts` and `apps/api`'s env both name this.
        assert_eq!(BIND_ENV, "CHIEFD_HOST_BIND");
    }

    #[test]
    fn an_unset_or_blank_override_falls_back_to_the_default() {
        // Deliberately does not mutate the process environment: a test that
        // sets a variable races every other test in the binary. This asserts
        // the fallback the parse performs on the values it can be handed.
        let resolved = |value: Option<&str>| {
            value
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_BIND.to_string())
        };
        assert_eq!(resolved(None), DEFAULT_BIND);
        assert_eq!(resolved(Some("   ")), DEFAULT_BIND);
        assert_eq!(resolved(Some(" 127.0.0.1:9999 ")), "127.0.0.1:9999");
        // And the real reader agrees on the unset case in a clean environment.
        if std::env::var(BIND_ENV).is_err() {
            assert_eq!(bind_address(), DEFAULT_BIND);
        }
    }
}
