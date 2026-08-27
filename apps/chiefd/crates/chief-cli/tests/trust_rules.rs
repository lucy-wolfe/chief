//! The named tmux trust-rule tests (TESTING.md §3.4, plan §4).
//!
//! Every test here drives the **real** `TmuxHost` logic through a scripted
//! runner. No tmux server exists, nothing sleeps, and nothing is retried in
//! the hope of hitting a window — TESTING.md §1.2 makes a repetition-based
//! concurrency test an automatic review rejection, so the transient condition
//! is *scripted*, not raced.
//!
//! The four rules, one section each:
//!
//! 1. a transient error is never takeover permission;
//! 2. an "invalid option" no-tag response is untrusted;
//! 3. foreign or partially-tagged objects are never killed or adopted;
//! 4. rebuild happens on proven absence only (invariant 9).

use std::time::Duration;

use chief_cli::actuate::fake::{ScriptedReply, ScriptedTmux};
use chief_cli::actuate::host::{HostErr, PaneId, Pid, Socket};
use chief_cli::actuate::{
    RebuildDecision, RecordingWaiter, SessionPresence, TmuxHost, UnprovenCause,
    SERVER_EXITED_RETRIES, SERVER_EXITED_RETRY_DELAY_MS,
};

const SESSION: &str = "cobalt";
const ORG: &str = "cobalt";

fn socket() -> Socket {
    Socket("chiefd-test".into())
}

fn host(runner: ScriptedTmux) -> TmuxHost<ScriptedTmux, RecordingWaiter> {
    TmuxHost::new(runner, RecordingWaiter::default())
}

/// The four `show-options -qv` reads one object's tag scan makes, in the order
/// `read_object_tags` issues them.
fn object_tags(org: &str, window: &str, person: &str, launch_hash: &str) -> Vec<ScriptedReply> {
    vec![
        ScriptedReply::ok(org),
        ScriptedReply::ok(window),
        ScriptedReply::ok(person),
        ScriptedReply::ok(launch_hash),
    ]
}

// ---------------------------------------------------------------------------
// 1. A transient error is never takeover permission.
// ---------------------------------------------------------------------------

#[test]
fn transient_server_exit_is_retried_twenty_times_and_then_refused() {
    let host = host(ScriptedTmux::always(ScriptedReply::server_exited()));

    let error = host.session_presence(&socket(), SESSION).expect_err("never proven");

    assert!(
        matches!(error, HostErr::Untrusted { .. }),
        "exhausting the ladder must be Untrusted, not absence: {error}"
    );
    assert_eq!(
        host.runner().call_count(),
        usize::try_from(SERVER_EXITED_RETRIES).expect("fits"),
        "the ported ladder is exactly 20 attempts"
    );
    assert_eq!(
        host.waiter().waits(),
        vec![
            Duration::from_millis(SERVER_EXITED_RETRY_DELAY_MS);
            usize::try_from(SERVER_EXITED_RETRIES).expect("fits") - 1
        ],
        "19 waits between 20 attempts, at 25 ms"
    );
    assert!(!host.runner().ran_verb("kill-session"), "nothing was killed");
    assert!(!host.runner().ran_verb("new-session"), "nothing was rebuilt");
}

#[test]
fn a_transient_that_clears_before_the_ladder_ends_yields_the_real_answer() {
    // 19 transients then a genuine answer: the ladder exists to reach *this*,
    // and the answer must be the tmux one, not a timeout verdict.
    let mut script: Vec<ScriptedReply> = (0..19).map(|_| ScriptedReply::server_exited()).collect();
    script.push(ScriptedReply::ok(""));
    let host = host(ScriptedTmux::new(script));

    let presence = host.session_presence(&socket(), SESSION).expect("answered");

    assert_eq!(presence, SessionPresence::Present);
    assert_eq!(host.runner().call_count(), 20);
    assert_eq!(host.waiter().total(), Duration::from_millis(19 * SERVER_EXITED_RETRY_DELAY_MS));
    assert_eq!(host.waiter().waits().len(), 19);
}

#[test]
fn a_transient_during_an_audit_never_produces_an_empty_audit() {
    // The historically dangerous shape: an audit that "found no panes" because
    // tmux was restarting, followed by a rebuild that duplicates the fleet.
    let host = host(ScriptedTmux::always(ScriptedReply::server_exited()));

    let error = host.audit_session(&socket(), SESSION, ORG).expect_err("unproven");

    assert!(matches!(error, HostErr::Untrusted { .. }));
}

#[test]
fn a_transient_is_not_retried_when_tmux_gave_a_real_answer() {
    let host = host(ScriptedTmux::new([ScriptedReply::no_session(SESSION)]));

    assert_eq!(
        host.session_presence(&socket(), SESSION).expect("answered"),
        SessionPresence::ProvablyAbsent
    );
    assert_eq!(host.runner().call_count(), 1, "a proven answer is not retried");
    assert!(host.waiter().waits().is_empty(), "and nothing waited");
}

// ---------------------------------------------------------------------------
// 2. "invalid option" is untrusted, and is not retried.
// ---------------------------------------------------------------------------

#[test]
fn invalid_option_on_the_session_tag_refuses_rather_than_reporting_it_unowned() {
    let host = host(ScriptedTmux::new([
        ScriptedReply::ok(""),                             // has-session: present
        ScriptedReply::invalid_option("@organization_id"), // show-options -v
    ]));

    let error = host.audit_session(&socket(), SESSION, ORG).expect_err("untrusted");

    assert!(
        matches!(error, HostErr::Untrusted { .. }),
        "an unreadable ownership tag is not evidence the session is free: {error}"
    );
    assert!(!host.runner().ran_verb("kill-session"));
    assert!(!host.runner().ran_verb("kill-pane"));
}

#[test]
fn invalid_option_is_not_retried_because_it_answers_identically_forever() {
    let host = host(ScriptedTmux::always(ScriptedReply::invalid_option("-F")));

    let error = host.session_presence(&socket(), SESSION).expect_err("untrusted");

    assert!(matches!(error, HostErr::Untrusted { .. }));
    assert_eq!(host.runner().call_count(), 1, "no ladder for a deterministic failure");
    assert!(!UnprovenCause::InvalidOption.is_retryable());
}

#[test]
fn an_unreadable_pane_tag_stops_the_audit_instead_of_skipping_the_pane() {
    // Skipping would leave the pane out of the projection, and the very next
    // reconcile would "restore" a second pane for that person.
    let mut script = vec![
        ScriptedReply::ok(""),   // has-session
        ScriptedReply::ok(ORG),  // session ownership tag
        ScriptedReply::ok("@1"), // list-windows
    ];
    script.extend(object_tags(ORG, "w-eng", "", ""));
    script.push(ScriptedReply::ok("%1")); // list-panes
    script.push(ScriptedReply::invalid_option("@organization_id"));
    let host = host(ScriptedTmux::new(script));

    let error = host.audit_session(&socket(), SESSION, ORG).expect_err("untrusted");

    assert!(matches!(error, HostErr::Untrusted { .. }));
    assert!(!host.runner().ran_verb("kill-pane"));
}

// ---------------------------------------------------------------------------
// 3. Foreign and partially-tagged objects are never killed or adopted.
// ---------------------------------------------------------------------------

#[test]
fn a_session_tagged_for_another_company_is_refused_never_taken_over() {
    let host = host(ScriptedTmux::new([
        ScriptedReply::ok(""),             // has-session: present
        ScriptedReply::ok("someone-else"), // ownership tag
    ]));

    let error = host.audit_session(&socket(), SESSION, ORG).expect_err("foreign");

    match error {
        HostErr::ToolFailed { detail, .. } => {
            assert!(detail.contains("someone-else"), "{detail}");
            assert!(detail.contains("refusing"), "{detail}");
        }
        other => panic!("expected a refusal, got {other}"),
    }
    assert!(!host.runner().ran_verb("kill-session"), "a foreign session is never killed");
    assert!(!host.runner().ran_verb("new-session"), "and never rebuilt over");
}

#[test]
fn a_session_with_an_empty_ownership_tag_is_refused() {
    let host = host(ScriptedTmux::new([ScriptedReply::ok(""), ScriptedReply::ok("")]));

    let error = host.audit_session(&socket(), SESSION, ORG).expect_err("missing tag");

    match error {
        HostErr::ToolFailed { detail, .. } => assert!(detail.contains("missing"), "{detail}"),
        other => panic!("expected a refusal, got {other}"),
    }
}

#[test]
fn a_partially_tagged_pane_aborts_the_reconcile_and_kills_nothing() {
    let mut script = vec![ScriptedReply::ok(""), ScriptedReply::ok(ORG), ScriptedReply::ok("@1")];
    script.extend(object_tags(ORG, "w-eng", "", ""));
    script.push(ScriptedReply::ok("%1"));
    // A pane carrying our org tag but no person id: exactly what an
    // interrupted reconcile leaves behind.
    script.extend(object_tags(ORG, "w-eng", "", "hash-3"));
    let host = host(ScriptedTmux::new(script));

    let error = host.audit_session(&socket(), SESSION, ORG).expect_err("partial");

    match error {
        HostErr::ToolFailed { detail, .. } => {
            assert!(detail.contains("not fully ownership-tagged"), "{detail}");
        }
        other => panic!("expected a refusal, got {other}"),
    }
    assert!(!host.runner().ran_verb("kill-pane"));
    assert!(!host.runner().ran_verb("respawn-pane"));
}

#[test]
fn an_untagged_pane_in_our_session_is_left_strictly_alone() {
    let mut script = vec![ScriptedReply::ok(""), ScriptedReply::ok(ORG), ScriptedReply::ok("@1")];
    script.extend(object_tags(ORG, "w-eng", "", ""));
    script.push(ScriptedReply::ok("%1\n%2"));
    script.extend(object_tags(ORG, "w-eng", "p-1", "hash-3")); // ours
    script.extend(object_tags("", "", "", "")); // somebody's stray shell
    let host = host(ScriptedTmux::new(script));

    let audit = host.audit_session(&socket(), SESSION, ORG).expect("audit");

    assert_eq!(audit.panes.len(), 1, "the untagged pane is not adopted");
    assert_eq!(audit.panes["p-1"].pane, PaneId("%1".into()));
    assert_eq!(audit.panes["p-1"].launch_hash, "hash-3");
    assert!(!host.runner().ran_verb("kill-pane"), "and it is not killed either");
}

#[test]
fn two_panes_claiming_one_person_are_ambiguous_not_arbitrarily_resolved() {
    let mut script = vec![ScriptedReply::ok(""), ScriptedReply::ok(ORG), ScriptedReply::ok("@1")];
    script.extend(object_tags(ORG, "w-eng", "", ""));
    script.push(ScriptedReply::ok("%1\n%2"));
    script.extend(object_tags(ORG, "w-eng", "p-1", "hash-3"));
    script.extend(object_tags(ORG, "w-eng", "p-1", "hash-4"));
    let host = host(ScriptedTmux::new(script));

    let error = host.audit_session(&socket(), SESSION, ORG).expect_err("ambiguous");

    match error {
        HostErr::ToolFailed { detail, .. } => assert!(detail.contains("duplicate"), "{detail}"),
        other => panic!("expected a refusal, got {other}"),
    }
    assert!(!host.runner().ran_verb("kill-pane"));
}

// ---------------------------------------------------------------------------
// 4. Rebuild on proven absence only (invariant 9).
// ---------------------------------------------------------------------------

#[test]
fn rebuild_is_authorized_only_by_a_proven_absence() {
    for diagnostic in [
        "no server running on /tmp/tmux-1000/chiefd-test",
        "can't find session: cobalt",
        "error connecting to /tmp/tmux-1000/chiefd-test (No such file or directory)",
    ] {
        let host = host(ScriptedTmux::new([ScriptedReply::failed(diagnostic)]));
        let audit = host.audit_session(&socket(), SESSION, ORG).expect("proven absence");
        assert_eq!(audit.presence, SessionPresence::ProvablyAbsent, "{diagnostic}");
        assert_eq!(audit.rebuild_decision(), RebuildDecision::Rebuild, "{diagnostic}");
        assert!(audit.panes.is_empty() && audit.windows.is_empty());
    }
}

#[test]
fn an_unrecognized_diagnostic_never_authorizes_a_rebuild() {
    let host = host(ScriptedTmux::new([ScriptedReply::failed("some new tmux 4.0 diagnostic")]));

    let error = host.session_presence(&socket(), SESSION).expect_err("unproven");

    assert!(matches!(error, HostErr::Untrusted { .. }));
    assert_eq!(host.runner().call_count(), 1, "an unknown failure is not retried, only refused");
}

#[test]
fn a_live_session_is_left_running_not_rebuilt() {
    let mut script = vec![ScriptedReply::ok(""), ScriptedReply::ok(ORG), ScriptedReply::ok("@1")];
    script.extend(object_tags(ORG, "w-eng", "", ""));
    script.push(ScriptedReply::ok(""));
    let host = host(ScriptedTmux::new(script));

    let audit = host.audit_session(&socket(), SESSION, ORG).expect("audit");

    assert_eq!(audit.presence, SessionPresence::Present);
    assert_eq!(audit.rebuild_decision(), RebuildDecision::LeaveRunning);
    assert_eq!(audit.windows.get("w-eng"), Some(&"@1".to_string()));
    assert!(!host.runner().ran_verb("new-session"));
}

// ---------------------------------------------------------------------------
// pane_pid freshness (TESTING.md §3.4, plan §6.2)
// ---------------------------------------------------------------------------

#[test]
fn pane_pid_is_read_from_tmux_on_every_call_so_a_respawn_is_seen() {
    let host = host(ScriptedTmux::new([ScriptedReply::ok("1001"), ScriptedReply::ok("2002")]));
    let pane = PaneId("%1".into());

    assert_eq!(host.pane_pid(&socket(), &pane).expect("first"), Pid(1001));
    // Same pane id, new process: `respawn-pane` and the native fresh-session
    // path both do this. A cached pid would hard-deny the pane that just
    // completed a reset.
    assert_eq!(host.pane_pid(&socket(), &pane).expect("after respawn"), Pid(2002));
    assert_eq!(host.runner().call_count(), 2);
    for argv in host.runner().calls() {
        assert!(argv.contains(&"#{pane_pid}".to_string()), "{argv:?}");
    }
}

#[test]
fn a_pane_that_reports_no_pid_is_an_error_not_pid_zero() {
    let host = host(ScriptedTmux::new([ScriptedReply::ok("")]));

    let error = host.pane_pid(&socket(), &PaneId("%1".into())).expect_err("no pid");

    assert!(matches!(error, HostErr::ToolFailed { .. }));
}

#[test]
fn a_spawn_that_returns_no_pane_id_is_untrusted_not_a_success() {
    let host = host(ScriptedTmux::new([ScriptedReply::ok("   ")]));

    let error = host
        .spawn_pane(&socket(), SESSION, "eng", &["pi".to_owned()], &[])
        .expect_err("no pane id");

    assert!(matches!(error, HostErr::Untrusted { .. }));
}

#[test]
fn a_spawned_pane_is_tagged_before_it_is_reported() {
    let host = host(ScriptedTmux::new([ScriptedReply::ok("%9"), ScriptedReply::ok("")]));

    let pane = host
        .spawn_pane(
            &socket(),
            SESSION,
            "eng",
            &["pi".to_owned()],
            &[("@organization_id".to_owned(), ORG.to_owned())],
        )
        .expect("spawn");

    assert_eq!(pane, PaneId("%9".into()));
    let calls = host.runner().calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].first().map(String::as_str), Some("new-window"));
    assert_eq!(calls[1].first().map(String::as_str), Some("set-option"));
    assert!(calls[1].contains(&"@organization_id".to_string()));
}
