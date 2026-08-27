//! `POST /v1/company/boot` — bring an existing company up under `apps/api`.
//!
//! # The same end state as create, minus genesis
//!
//! Create and boot differ in exactly one step: create seeds the durable
//! company, boot finds one already seeded. Everything after that — the daemon,
//! shadow mode, the CEO durably started — is identical, and is reached through
//! the same [`super::create::activate_ceo`] rather than a second description
//! of it.
//!
//! # Phases
//!
//! `company-daemon-start`, `company-daemon-ready`, `chief-start`. The genesis
//! phases are absent because genesis does not happen, not because they are
//! suppressed: a caller rendering the stream sees a shorter sequence of the
//! same vocabulary.
//!
//! # An already-running daemon is adopted, never restarted
//!
//! `daemon::start` adopts a `Live`, identity-proven registration and spawns
//! only when there is nothing to adopt, so booting a company whose daemon is
//! already up does not tear a healthy listener down. `company-daemon-start` is
//! still emitted first: it names the step, and "ensure the daemon" is the same
//! step whether or not it ends in a spawn.

use std::path::Path;

use crate::company::{self, CompanyClient};
use crate::daemon;
use crate::genesis::LaunchOutcome;
use crate::http::Client;
use crate::{paths, LifecycleError, Result};

use super::create;
use super::phases::{Phase, PhaseSink};

/// Boot the already-created company in `dir`.
///
/// # Errors
/// [`LifecycleError`] naming the refusal — a directory with no company, a
/// daemon that would not come up, or a durable write the company refused.
pub(crate) async fn boot(dir: &Path, phases: &PhaseSink) -> Result<LaunchOutcome> {
    let home = paths::home()?;
    // THE STORE FILE IS THE COMPANY. Refusing here names the real problem
    // instead of letting a spawn fail minutes later inside a daemon log this
    // caller cannot read.
    crate::require_a_company_here(dir, "chief host: boot")?;
    // AUTHENTICATED: `chief host` is the SERVER here, and this handler is
    // its operator acting on a COMPANY DAEMON. The host's own listener is a
    // separate surface with no auth runtime, but nothing below calls it.
    let client = Client::operator(dir);
    let key = paths::company_key(dir);

    phases.emit(Phase::CompanyDaemonStart, dir.display().to_string());
    let started = daemon::start(&client, &home, dir, &company::boot_socket_request(&key)).await?;
    phases.emit(Phase::CompanyDaemonReady, started.url.as_str());

    let live = CompanyClient::new(&client, &started.url, dir, &key);
    let facts = live.facts().await?.ok_or_else(|| {
        LifecycleError::refused(format!(
            "{} has a daemon but no manifest — it was never created. Create it before booting it.",
            dir.display()
        ))
    })?;
    if facts.chief_person_id.is_empty() {
        return Err(LifecycleError::refused(format!(
            "the company in {} has no root department head to bring up",
            dir.display()
        )));
    }

    // TOMBSTONE (chief-home-is-cwd §4c): `Phase::CeoPrepare`, the
    // `prepare_ceo_only` POST, and `Phase::CeoPrepareFailed`.
    //
    // The api-host path runs no tmux actuator, so this write was inert HERE
    // even before the route was deleted — its own comment said so. What
    // actually brings this company's CEO up is `create::activate_ceo` below,
    // which pins the company to shadow and starts the CEO through the api host,
    // and that step keeps its phases: an operator watching this stream still
    // sees `ChiefStart`, or `ChiefStartFailed` carrying the refusal.
    let outcome = LaunchOutcome {
        session: crate::company::conventional_session_name(&facts.slug, &key),
        slug: facts.slug,
        dir: dir.display().to_string(),
        key,
        url: started.url,
        chief_person_id: facts.chief_person_id,
    };
    create::activate_ceo(&outcome, phases).await?;
    Ok(outcome)
}
