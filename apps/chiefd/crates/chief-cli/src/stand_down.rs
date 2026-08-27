//! `chief stand-down` and `chief resume` — the operator's own door to
//! "stop working, and stay stopped".
//!
//! # Why this is not `chief stop`
//!
//! `chief stop` takes the RUNTIME down: the panes, the actuator and the daemon
//! all go, and afterwards there is nobody to talk to and nothing to ask. That
//! is the right verb for "I am done with this company for now".
//!
//! It is the wrong verb for what an operator actually asked a live company:
//! *"STOP ALL WORK NOW … Tell every person to stop immediately and park all of
//! them except yourself. Then stay idle and do nothing until I ask."* They
//! wanted the company to stop working and the CEO to stay available. The CEO
//! obeyed exactly and the company put all six people back forty-five seconds
//! later, because there was no durable state saying it had been stopped — see
//! `chiefd_core::store::stand_down`.
//!
//! So a stand-down leaves the company attached, leaves the CEO running, and
//! stops everybody else until an explicit resume. Queued mail is HELD: nothing
//! is delivered, nothing is dropped, and the resume gives it back.

use std::path::Path;

use super::company::{now_iso_millis, CompanyClient};
use super::daemon;
use super::http::Client;
use super::{LifecycleError, Result};

/// One stand-down or resume, printed as JSON so a script can read it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StandDownOutcome {
    /// The company — the directory it occupies, which is its identity.
    pub(crate) dir: String,
    /// Whether the company is stood down AFTER this command.
    pub(crate) stood_down: bool,
    /// When the standing stand-down began, or empty when the company is
    /// working. Read back from the daemon rather than echoed, so the answer is
    /// the durable one and a repeated stand-down reports its FIRST instant.
    pub(crate) since: String,
    /// The operator's reason, or empty.
    pub(crate) reason: String,
}

/// Resolve the live company in `dir`, or refuse in the operator's words.
async fn live_company<'a>(client: &'a Client, dir: &Path, verb: &str) -> Result<CompanyClient<'a>> {
    let running = daemon::resolve_running(client, dir).await.ok_or_else(|| {
        LifecycleError::refused(format!(
            "chief {verb}: the company in {} is not running, so there is nothing working to \
             {verb}. Start it with `chief` first.",
            dir.display()
        ))
    })?;
    Ok(CompanyClient::new(client, &running.url, dir, &super::paths::company_key(dir)))
}

/// Report the company's current stand-down state as JSON.
async fn report(dir: &Path, company: &CompanyClient<'_>) -> Result<()> {
    let stand_down = company.read_stand_down().await?;
    let outcome = StandDownOutcome {
        dir: dir.display().to_string(),
        stood_down: stand_down.is_some(),
        since: stand_down.as_ref().map(|s| s.since.clone()).unwrap_or_default(),
        reason: stand_down.as_ref().map(|s| s.reason.clone()).unwrap_or_default(),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&outcome)
            .unwrap_or_else(|_| format!("{{\"stoodDown\":{}}}", outcome.stood_down))
    );
    Ok(())
}

/// `chief stand-down [reason]` — stop every person and keep them stopped.
///
/// # Errors
/// [`super::LifecycleError`] when this directory holds no running company or
/// the route refuses.
pub(crate) async fn run_stand_down(dir: &Path, reason: &str) -> Result<()> {
    super::require_a_company_here(dir, "chief stand-down")?;
    // AUTHENTICATED: this goes to a COMPANY DAEMON, which verifies a bearer.
    let client = Client::operator(dir);
    let company = live_company(&client, dir, "stand-down").await?;
    company.stand_down(&now_iso_millis(), reason).await?;
    report(dir, &company).await
}

/// `chief resume` — let this company work again.
///
/// # Errors
/// [`super::LifecycleError`] when this directory holds no running company or
/// the route refuses.
pub(crate) async fn run_resume(dir: &Path) -> Result<()> {
    super::require_a_company_here(dir, "chief resume")?;
    let client = Client::operator(dir);
    let company = live_company(&client, dir, "resume").await?;
    company.resume(&now_iso_millis()).await?;
    report(dir, &company).await
}

#[cfg(test)]
mod tests {
    use super::StandDownOutcome;

    /// The JSON a script reads. Camel-case, and `stoodDown` is the field that
    /// answers the only question worth asking after either verb.
    #[test]
    fn the_outcome_serializes_as_the_camel_case_object_a_script_reads() {
        let json = serde_json::to_value(StandDownOutcome {
            dir: "/work/acme".into(),
            stood_down: true,
            since: "2026-08-18T10:00:00.000Z".into(),
            reason: "stop all work now".into(),
        })
        .expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "dir": "/work/acme",
                "stoodDown": true,
                "since": "2026-08-18T10:00:00.000Z",
                "reason": "stop all work now",
            })
        );
    }

    /// A working company reports the state plainly rather than omitting fields,
    /// so a script can read one shape either way.
    #[test]
    fn a_working_company_reports_false_and_empty_strings() {
        let json = serde_json::to_value(StandDownOutcome {
            dir: "/work/acme".into(),
            stood_down: false,
            since: String::new(),
            reason: String::new(),
        })
        .expect("serialize");
        assert_eq!(json["stoodDown"], serde_json::json!(false));
        assert_eq!(json["since"], serde_json::json!(""));
    }
}
