//! `POST /v1/company/create` — genesis, narrated.
//!
//! # What this owns and what it does not
//!
//! Genesis itself — slug derivation, the beacond claim, the one daemon start,
//! the CEO-only manifest, the single-transaction seed — is
//! [`crate::genesis`]. Nothing about it is repeated here: a second
//! copy of an ordering whose whole value is that it is the only one would be
//! the exact failure Mandate 0 names.
//!
//! This module owns the two things genesis does not:
//!
//! 1. **The narration.** A phase sink threaded through the sequence so a caller
//!    watching an SSE stream sees each step as it happens.
//! 2. **The api-host tail.** A company created through this surface is actuated
//!    by `apps/api`, so its CEO is pinned to shadow mode and durably started
//!    ([`super::ceo`]) rather than left to a converge loop that will not
//!    actuate it.
//!
//! # PORT-SEAM
//!
//! `apps/chiefd/crates/chief-cli/src/genesis.rs` is another slice's
//! file. This module calls `genesis::launch_with_phases(&request, &sink)`,
//! which that file does not have yet. The expected change is mechanical and is
//! spelled out completely so it can be applied without re-deriving it:
//!
//! * `pub(crate) async fn launch(request: &LaunchRequest) -> Result<LaunchOutcome>`
//!   becomes
//!   `pub(crate) async fn launch_with_phases(request: &LaunchRequest, phases: &PhaseSink) -> Result<LaunchOutcome>`,
//!   with the body unchanged except for the emissions below. `launch` stays as
//!   a one-line wrapper that passes a sink whose receiver is dropped —
//!   `chief`'s Founder endpoint narrates nothing today and emitting into a
//!   hung-up sink is explicitly a no-op ([`super::phases::PhaseSink::emit`]).
//! * The sink is re-bound to the derived slug — `phases.with_slug(slug.as_str())`
//!   — right after `slugify` and before the beacond claim, so every frame a
//!   caller sees is labelled with the company it is about.
//! * Five emissions, at the points the sequence already has:
//!   - before `daemon::start`            -> `CompanyDaemonStart`  (detail: the orgs root)
//!   - after  `daemon::start`            -> `CompanyDaemonReady`  (detail: the proven URL)
//!   - before `genesis_with_models`      -> `DurableCreate`
//!   - on its `Ok`                       -> `DurableCreateComplete`
//!   - on its `Err`                      -> `DurableCreateFailed` (detail: the refusal),
//!     then `CompanyDaemonStop` before the `daemon::stop` it already performs,
//!     and `CompanyDaemonStopped` or `CompanyDaemonStopFailed` on its result —
//!     that result is currently discarded with `let _ =`, and the phase is the
//!     reason to stop discarding it.
//!
//! TOMBSTONE (chief-home-is-cwd §4c): a sixth emission, `CeoPrepare` before
//! `prepare_ceo_only` with `CeoPrepareFailed` on its error path. Both phases
//! and the call are deleted with the daemon-side CEO boot. `DurableCreate`
//! `Complete` is now the last frame genesis emits, and it is a truthful end:
//! the company is durable and CEO-only at that point.
//!
//! `ChiefStart`/`ChiefStartFailed` are NOT part of that seam and are NOT deleted
//! with the prepare pair: they belong to the api-host tail below, which the
//! Founder path does not take, and they narrate two durable writes that can
//! still refuse.

use std::path::Path;

use crate::company::CompanyClient;
use crate::genesis::{self, LaunchOutcome, LaunchRequest};
use crate::http::Client;
use crate::{paths, LifecycleError, Result};

use super::phases::{Phase, PhaseSink};

/// Create a company and bring its CEO up under `apps/api`'s actuation.
///
/// # Errors
/// [`LifecycleError`] naming the refusal. Every failure has already emitted the
/// phase that explains it, so a caller reading the stream sees WHERE it stopped
/// before it sees THAT it stopped.
pub(crate) async fn create(
    dir: &Path,
    request: &LaunchRequest,
    phases: &PhaseSink,
) -> Result<LaunchOutcome> {
    let outcome = genesis::launch_with_phases(dir, request, phases).await?;
    let phases = phases.with_slug(outcome.slug.as_str());
    activate_ceo(&outcome, &phases).await?;
    Ok(outcome)
}

/// The api-host tail: shadow mode, then the CEO durably started.
///
/// Shared with [`super::boot`], which reaches the same end state for a company
/// that already exists — one description of "an api-hosted company is up",
/// not two.
///
/// `ChiefStart` is emitted AFTER both writes commit, not before them. The
/// vocabulary distinguishes an intent from an outcome by name — `DurableCreate`
/// announces a step and `DurableCreateComplete` reports it — and `ChiefStart` is
/// an outcome word. A caller that saw it before the write would have been told
/// the CEO is up while the refusal was still in flight.
///
/// # Errors
/// [`LifecycleError`] when either durable write refuses.
pub(crate) async fn activate_ceo(outcome: &LaunchOutcome, phases: &PhaseSink) -> Result<()> {
    // AUTHENTICATED: `chief host` is the SERVER here, and this handler is
    // its operator acting on a COMPANY DAEMON. The host's own listener is a
    // separate surface with no auth runtime, but nothing below calls it.
    let dir = Path::new(&outcome.dir);
    let client = Client::operator(dir);
    let company = CompanyClient::new(&client, &outcome.url, dir, &paths::company_key(dir));
    if let Err(error) = company.set_actuation_shadow().await {
        phases.emit(Phase::ChiefStartFailed, error.to_string());
        return Err(error);
    }
    if let Err(error) = company.start_person(&outcome.chief_person_id).await {
        phases.emit(Phase::ChiefStartFailed, error.to_string());
        return Err(LifecycleError::refused(format!(
            "ChiefD: created '{}' but its CEO could not be started: {error}\nIt is durable but not running. Recover by booting {} again.",
            outcome.slug,
            outcome.dir
        )));
    }
    phases.emit(Phase::ChiefStart, outcome.chief_person_id.as_str());
    Ok(())
}
