//! `POST /v1/company/stop` — tear a company's runtime down.
//!
//! # Not a phase stream
//!
//! Create and boot narrate because they are long enough that "still going" has
//! to be visible. Stop does not: it is one durable teardown followed by one
//! daemon shutdown, it has no step a caller can act on differently, and the
//! retired subprocess it replaces streamed nothing either. Giving it a phase
//! stream would be inventing a contract nobody asked for; it answers with the
//! outcome, once.
//!
//! # This is entirely [`crate::stop`]
//!
//! Including the ordering law that makes it correct — the durable teardown must
//! land while the daemon serving it is still up, so `clear_launch_intent` and
//! `clear_runtime` run before `daemon::stop`. That law is stated and tested
//! once, there; this module exists to give it an HTTP door, not a second body.
//!
//! # apps/api stops its own children first
//!
//! An api-hosted company's Pi children live inside `apps/api`, which is the
//! only process that can stop them, and it does so before calling this — see
//! `CompanyLifecycleService.stop`. That is not a business decision it is making
//! on its own: it is the one actuation step chiefd delegated to it by being in
//! shadow mode, and it is the mirror of the ordering law above.

use std::path::Path;

use crate::http::Client;
use crate::stop::{stop_runtime, StopOutcome};
use crate::Result;

/// Stop the company in `dir`.
///
/// # Errors
/// [`crate::LifecycleError`] when the directory holds no company or a teardown
/// step refuses.
pub(crate) async fn stop(dir: &Path) -> Result<StopOutcome> {
    // A directory with no company is a refusal naming it, not a silent
    // success: this door's callers are browsers, and a 200 for a company that
    // was never there is how a UI comes to show a company it just "stopped".
    crate::require_a_company_here(dir, "chief host: stop")?;
    // AUTHENTICATED: `chief host` is the SERVER here, and this handler is
    // its operator acting on a COMPANY DAEMON. The host's own listener is a
    // separate surface with no auth runtime, but nothing below calls it.
    let client = Client::operator(dir);
    stop_runtime(&client, dir, false).await
}
