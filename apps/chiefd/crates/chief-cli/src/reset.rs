//! `chief reset [--yes]` — shed this directory's company to CEO-only fresh
//! state.
//!
//! Ported from the deleted TypeScript `reset.ts`.
//!
//! # THE HARD RULE
//!
//! The end state is EXACTLY ONE pane — the CEO — with everyone else stopped,
//! their Pi sessions cleared, and NO durable state deleted: agents, goals,
//! mail, memory, assignments and the SQL store file all survive.
//!
//! The structural guarantee the TypeScript chose deliberately — exactly ONE
//! boot-shaped call available here — is now absolute: there is NO boot-shaped
//! call on the client at all. `CompanyClient::prepare_ceo_only` was the one and
//! it is deleted with the daemon-side CEO boot (chief-home-is-cwd §4c); nothing
//! roster-shaped ever existed. A future edit has nowhere to reach for either.
//!
//! CEO-only is not something this verb asks for; it is the state the teardown
//! LEAVES. An omitted launch intent is an empty allow-list rather than an off
//! switch, so the fence admits the root head and denies everyone else, and the
//! root holds an unconditional organization-root lease that keeps it desired
//! (`conformance/fixtures/activity/fence-omitted-is-chief-only-not-unfenced.json`).
//! Step 4 clears the launch intent, and clearing it IS the reset.
//!
//! # The ordering, and why each step is where it is
//!
//! 1. **Confirm first.** Nothing is touched before the operator has said yes,
//!    and a non-interactive caller without `--yes` is REFUSED rather than
//!    silently declined — a scripted reset that quietly does nothing is worse
//!    than one that fails loudly.
//! 2. **Bring the daemon up BEFORE trusting any read.** The manifest and the
//!    ownership record live in the SQL store this company's daemon serves, and
//!    a read against a down daemon throws rather than degrading. Reading first
//!    is what crashed every stopped-company reset.
//! 3. **Stamp the session epoch**, so every member's NEXT boot starts with an
//!    empty Pi session. A reset is the operator overriding cooperative
//!    settling, not asking politely.
//! 4. **Stop the whole runtime, CEO included**, reusing [`super::stop`]'s exact
//!    sequencing — but with the live listener PRESERVED, because step 5 needs
//!    it and re-spawning tears down a healthy listener for nothing. This step
//!    is also what REACHES the end state: it clears the launch intent, and an
//!    empty allow-list is CEO-only.
//! 5. **Leave the company able to converge.** The teardown kills the COMPANY
//!    session, never the actuator's, so an actuator survives a reset — and it
//!    can only put the CEO back if a daemon is serving the desired set it
//!    reads. Prove one is up; never roll forward by starting extra people.
//!
//! TOMBSTONE (chief-home-is-cwd §4c): step 5 was `prepare-ceo-only`, a POST
//! that retracted every non-CEO fence and stamped the quiesce watermark. It was
//! re-stating what step 4 had already made true. The route is deleted with the
//! daemon-side CEO boot, and nothing replaces it, because nothing had to.

use std::path::Path;

use super::company::{now_iso_millis, CompanyClient};
use super::confirm::{decide, Confirmation, Terminal};
use super::daemon;
use super::http::Client;
use super::{LifecycleError, Result};

/// One reset's result, printed as JSON.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResetOutcome {
    /// `reset` or `declined`.
    pub(crate) outcome: &'static str,
    /// The company — the directory it occupies, which is its identity.
    pub(crate) dir: String,
    /// The CEO who remains, when the reset ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chief_person_id: Option<String>,
    /// The clean-session fence every member's next boot starts from: a
    /// transcript last modified before this instant is not resumed. An INSTANT,
    /// never a counter — the store holds `session_epoch(slug, at, reason)` and
    /// has no counter to report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session_epoch_at: Option<String>,
    /// The end state, when the reset ran.
    ///
    /// Still `ceo-only`, and still true, though no step asks for it by name any
    /// more: clearing the launch intent leaves an empty allow-list, the fence
    /// admits the root head alone, and the root's unconditional
    /// organization-root lease keeps it desired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) end_state: Option<&'static str>,
}

/// Run one reset.
///
/// Works from every starting state — fully running, partially running,
/// daemon-down-with-an-orphan-session, and fully stopped — because each step is
/// idempotent on its own.
///
/// # Errors
/// [`LifecycleError`] naming the refusal and the operator's next move.
pub(crate) async fn run(dir: &Path, yes: bool) -> Result<()> {
    let home = super::paths::home()?;
    super::preflight::require_ready(super::preflight::Surface::Reset)?;
    super::require_a_company_here(dir, "chief reset")?;

    // Step 1 — confirm before anything at all is touched.
    match decide(
        &format!("Reset the company in {} to CEO-only? [y/N] ", dir.display()),
        yes,
        &Terminal,
    ) {
        Confirmation::Confirmed => {}
        Confirmation::Declined => {
            print(&ResetOutcome {
                outcome: "declined",
                dir: dir.display().to_string(),
                chief_person_id: None,
                session_epoch_at: None,
                end_state: None,
            });
            return Ok(());
        }
        Confirmation::RefusedNonInteractive => {
            return Err(LifecycleError::refused(
                "chief reset: this caller has no terminal to confirm on. Run: chief reset --yes"
                    .to_owned(),
            ));
        }
    }

    // AUTHENTICATED: every request below goes to a COMPANY DAEMON, which
    // verifies a presented bearer.
    let client = Client::operator(dir);
    let key = super::paths::company_key(dir);

    // Step 2 — the daemon must be up before any manifest/ownership read. Before
    // a runtime is proven there is no recorded ownership socket to consult, so
    // the environment tiers alone resolve the spawn socket; the recorded tier
    // is applied below, once the daemon can answer for it.
    let running = match daemon::resolve_running(&client, dir).await {
        Some(running) => running,
        None => {
            daemon::start(
                &client,
                &home,
                dir,
                &super::company::boot_socket_request(&super::paths::company_key(dir)),
            )
            .await?
        }
    };

    let at = now_iso_millis();
    let company_client = CompanyClient::new(&client, &running.url, dir, &key);
    let facts = company_client.facts().await?.ok_or_else(|| {
        LifecycleError::refused(format!(
            "The company in {} has no manifest to reset",
            dir.display()
        ))
    })?;

    // Step 3 — force every member's NEXT boot onto an empty Pi session.
    let session_epoch_at = company_client.stamp_session_epoch(&at).await?;

    // Step 4 — stop the whole runtime, CEO included, preserving the listener.
    super::stop::stop_runtime(&client, dir, true).await?;

    // Step 5 — leave the company able to converge. Step 4's degraded branch
    // stops the daemon outright, so the earlier proof does not hold across that
    // lifecycle boundary and is re-taken rather than assumed.
    //
    // This is not housekeeping. `stop_runtime` kills the COMPANY session, never
    // the actuator's, so an actuator normally SURVIVES a reset — and it can put
    // the CEO back only if a daemon is serving the desired set it reads.
    // Reporting `ceo-only` over a company with no daemon would name an end
    // state nothing can reach.
    if daemon::resolve_running(&client, dir).await.is_none() {
        daemon::start(
            &client,
            &home,
            dir,
            &super::company::boot_socket_request(&super::paths::company_key(dir)),
        )
        .await?;
    }

    print(&ResetOutcome {
        outcome: "reset",
        dir: dir.display().to_string(),
        chief_person_id: Some(facts.chief_person_id),
        session_epoch_at: Some(session_epoch_at),
        end_state: Some("ceo-only"),
    });
    Ok(())
}

/// Print an outcome as the one-JSON-object style `stop` also uses.
fn print(outcome: &ResetOutcome) {
    println!(
        "{}",
        serde_json::to_string_pretty(outcome)
            .unwrap_or_else(|_| format!("{{\"outcome\":\"{}\"}}", outcome.outcome))
    );
}

#[cfg(test)]
mod tests {
    use super::ResetOutcome;

    /// The reset sequence, stated as data. `run` performs exactly these, in
    /// this order; the tests below are what make a reordering a visible edit.
    ///
    /// TOMBSTONE (chief-home-is-cwd §4c): a fifth row, `prepare-ceo-only`, and
    /// the test that pinned it last — `the_ceo_only_intent_is_the_last_step_
    /// and_the_only_boot_shaped_one`. That test asserted two things: the intent
    /// came last, and exactly one step was boot-shaped so a roster boot had
    /// nowhere to appear. Both are now enforced by the CLIENT'S SHAPE instead
    /// of by a list: `CompanyClient` has no boot-shaped method at all, so the
    /// count this list could hold is zero and a test asserting `== 1` would be
    /// asserting the opposite of the rule. The rule did not weaken; the thing
    /// it feared became unreachable.
    const RESET_ORDER: [&str; 4] =
        ["confirm", "ensure-daemon-up", "stamp-session-epoch", "stop-runtime-preserving-daemon"];

    fn index(step: &str) -> usize {
        RESET_ORDER.iter().position(|candidate| *candidate == step).expect("named step")
    }

    #[test]
    fn nothing_is_touched_before_the_operator_confirms() {
        assert_eq!(index("confirm"), 0);
    }

    #[test]
    fn the_daemon_is_up_before_any_durable_read_or_write() {
        // The stopped-company crash: reading the manifest first threw, because
        // durable state is SQL-only and the daemon serving it was down.
        assert!(index("ensure-daemon-up") < index("stamp-session-epoch"));
        assert!(index("ensure-daemon-up") < index("stop-runtime-preserving-daemon"));
    }

    #[test]
    fn the_session_epoch_is_stamped_before_the_runtime_is_torn_down() {
        // Otherwise a member could be stopped and restarted between the two and
        // come back on the OLD session — the exact thing a reset promises not
        // to leave behind.
        assert!(index("stamp-session-epoch") < index("stop-runtime-preserving-daemon"));
    }

    /// The teardown is LAST, and it is what reaches the end state.
    ///
    /// It clears the launch intent, and an empty allow-list is CEO-only, so a
    /// step appended after it could only undo the reset. Nothing boot-shaped
    /// may appear here either — the client has no boot-shaped call left to
    /// reach for, and this keeps the sequence honest if one is ever re-added.
    #[test]
    fn the_teardown_is_the_last_step_and_no_step_is_boot_shaped() {
        assert_eq!(index("stop-runtime-preserving-daemon"), RESET_ORDER.len() - 1);
        assert_eq!(
            RESET_ORDER
                .iter()
                .filter(|step| step.contains("chief") || step.contains("launch"))
                .count(),
            0,
            "no boot-shaped step; a CEO or roster boot has nowhere to appear"
        );
    }

    #[test]
    fn a_declined_reset_reports_itself_and_names_nothing_it_did_not_do() {
        let declined = ResetOutcome {
            outcome: "declined",
            dir: "/work/acme".to_string(),
            chief_person_id: None,
            session_epoch_at: None,
            end_state: None,
        };
        let json = serde_json::to_value(&declined).expect("serialize");
        assert_eq!(json["outcome"], "declined");
        assert!(json.get("chiefPersonId").is_none());
        assert!(json.get("sessionEpochAt").is_none());
        assert!(json.get("endState").is_none());
    }

    #[test]
    fn a_completed_reset_reports_the_ceo_the_epoch_and_the_end_state() {
        let done = ResetOutcome {
            outcome: "reset",
            dir: "/work/acme".to_string(),
            chief_person_id: Some("executive-ceo".to_string()),
            session_epoch_at: Some("2026-08-10T09:41:02.113Z".to_string()),
            end_state: Some("ceo-only"),
        };
        let json = serde_json::to_value(&done).expect("serialize");
        assert_eq!(json["chiefPersonId"], "executive-ceo");
        assert_eq!(json["sessionEpochAt"], "2026-08-10T09:41:02.113Z");
        assert_eq!(json["endState"], "ceo-only");
    }

    /// The reported epoch is the INSTANT chiefd stamped, never a counter.
    ///
    /// `stamp_session_epoch` used to be `bump_session_epoch`, which read a
    /// nonexistent integer `epoch` key, added one to it and published
    /// `{epoch, updatedAt}` — a shape the store has never held. This asserts
    /// the reported value is a timestamp, so a future edit cannot quietly
    /// reintroduce a counter the `session_epoch` table cannot store.
    #[test]
    fn the_reported_epoch_is_an_instant_and_never_a_counter() {
        let done = ResetOutcome {
            outcome: "reset",
            dir: "/work/acme".to_string(),
            chief_person_id: Some("executive-ceo".to_string()),
            session_epoch_at: Some("2026-08-10T09:41:02.113Z".to_string()),
            end_state: Some("ceo-only"),
        };
        let reported = &serde_json::to_value(&done).expect("serialize")["sessionEpochAt"];
        assert!(reported.is_string(), "the fence is an instant, not a counter");
        assert!(reported.as_i64().is_none());
        assert!(reported.as_str().expect("instant").ends_with('Z'));
    }
}
