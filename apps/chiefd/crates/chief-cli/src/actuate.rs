//! Actuation: the client runs the agents, and chiefd only says who should be
//! running and what they should be running.
//!
//! # What changed, in one sentence
//!
//! chiefd publishes a DESIRED SET — the people who should be up and a content
//! hash of what each should be built from — and everything else happens here:
//! this crate reads that set, reads tmux, diffs the two, and closes the
//! difference. **No host fact travels back up.** There is no report, no lease
//! and no action stream; `POST /v1/org/runtime/observed` and
//! `POST /v1/org/runtime/actions` are deleted, and the direction they
//! represented is barred rather than merely unused.
//!
//! The permitted direction is untouched: an AGENT may report facts about
//! ITSELF over HTTP — heartbeats, "I settled", a native model switch. That is a
//! fact about the agent, not about tmux, and it does not come from here.
//!
//! # The modules, and why each one had to physically move
//!
//! * [`host`] — the argv/exit-code/pane-id vocabulary, re-declared rather than
//!   imported, because this crate links no backend crate.
//! * [`runner`] — the process seam. It takes tmux argv and returns tmux exit
//!   codes and `#{pane_id}` stdout; it is an **emulation** seam, so no non-tmux
//!   backend can sit behind it.
//! * [`trust`] — classifies tmux's *literal stderr strings* into
//!   provably-absent versus unproven. The fail-closed invariant of the whole
//!   system lives in this file. It used to be the reason a report could say
//!   `observationTrusted: false` rather than carry an empty people list; there
//!   is no report, so it is now the reason [`resident`] DECLINES A PASS rather
//!   than concluding a company is empty. Same rule, one hop shorter, and no
//!   longer able to reach a durable decision anywhere.
//! * [`exec`] / [`session`] — sessions, windows, panes, tags, layout.
//! * [`observe`] — live tmux to an observed topology, fail-closed.
//! * [`desired`] — the one wire shape read: chiefd's desired set.
//! * [`plan`] — the diff. Desired placement plus observed topology in, ordered
//!   steps out. It was always here and was always the real diff engine; it is
//!   now the ONLY one.
//! * [`crash_loop`] — the retry ledger for somebody who will not stay up: an
//!   exponential backoff capped at ten seconds, a consecutive-failure count,
//!   and the sentence the operator reads. It never gives up and it never sends
//!   anything anywhere.
//! * [`interpret`] — the step interpreter: order, binding map, fail-stop, and
//!   the per-step precondition re-verify that closes the observe→apply TOCTOU
//!   gap.
//! * [`launch_catalog`] — the OTHER half of a start, re-declared against the
//!   JSON like [`crate::roster`]: the desired set says WHO, the catalog says
//!   with what. It is fetched rather than computed because its derivation is a
//!   fail-closed gate over the DAEMON's data root that also stages each
//!   person's provider credential, and materialization is the daemon's job.
//! * [`resident`] — the long-running actuator mode. The plan calls this the
//!   most under-estimated item in the workstream, and it is: before P8 a
//!   person's Pi process was a child of a pane the daemon made, and its
//!   lifetime was the pane's. Afterwards the client makes the pane, so a
//!   company with no attached client has nobody to actuate it at all.
//!
//! # The consequence nobody should discover late
//!
//! **A company with no attached client is un-actuated, and chiefd cannot tell
//! you so.** It used to report `presence: "never-attached" | "lapsed"` with
//! `withheld: "no-actuator"`, derived from a lease the actuator renewed by
//! reporting. There is no report, so there is no lease, so there is no
//! presence. This is a NAMED, ACCEPTED loss:
//! inventing a second upward channel to recover it would reintroduce exactly
//! the thing that was removed, and the actuator — which owns the operator's
//! screen — is where "nobody is converging this company" is visible anyway.
//!
//! Starting a company is still two facts, not one: chiefd wants the people up,
//! *and* somebody is running `chief actuate`.

//! # The real tmux implementation and the trust rules
//!
//! Split across files with deliberately different change rates:
//!
//! * [`trust`] — pure classification. No I/O, no process spawning; every rule
//!   in plan §4 is a total function over `(status, stdout, stderr)` or over
//!   observed tags, so the whole trust model is testable without tmux.
//! * [`runner`] — the two injectable seams: running `tmux` and waiting.
//! * [`exec`] — [`TmuxHost`], which is the only place that combines them, so
//!   no decision can be made on a raw exit status.
//!
//! The rules, ported verbatim with named tests (plan §4):
//!
//! * the 20 × 25 ms "server exited unexpectedly" retry never reads as
//!   permission to take over;
//! * the "invalid option" no-tag response is equally untrusted — an old tmux
//!   that cannot report tags has told us nothing about ownership;
//! * foreign or partially-matching sessions are never killed or adopted;
//! * rebuild happens on **absence only** (invariant 9).
//!
//! # Where the shared names live
//!
//! The re-export block below is the module's public face: consumers name
//! `crate::actuate::SessionAudit`, `crate::actuate::classify_presence`,
//! `crate::actuate::safe_window_name` and so on, never the file that happens
//! to declare them today.
//!
//! [`safe_window_name`] is NOT re-declared here. It is a **shared bounded
//! canonical-label contract** — two actuators that name the same window
//! differently create a second window and split a company's panes across both
//! — so the crate holds exactly one definition of it, in
//! [`crate::placement`], where the desired topology that uses it is computed.
//! This is a re-export so the actuation side can keep naming it locally.

/// THE OPERATOR TERMINAL, in ONE definition, applied by every bootstrap that
/// creates a session a person will sit in front of.
///
/// # Why this is shared rather than set in each bootstrap
///
/// It was not shared, and that is exactly how the bug that produced it
/// survived. `mouse on` lived in this module's own company-session bootstrap
/// (`interpret::push_server_input_configuration`) and nowhere else. Founder runs on its OWN tmux
/// socket -- a different server -- so it never received the option, and a
/// pane with the mouse off swallows the wheel: the terminal's native
/// scrollback is not the pane's, and without mouse mode the wheel never
/// reaches copy-mode either. The result was that the ONE session an operator
/// meets FIRST was the only one they could not scroll, while plain `pi` in the
/// same terminal scrolled fine -- which reads as a Pi problem and is not one.
///
/// Copying the options into the second bootstrap would have repaired that
/// instance and guaranteed the next: **two copies of a setting is how the two
/// answers start disagreeing.** So there is one list, both bootstraps read it,
/// and a test asserts they produce the same setup -- an option added to one is
/// then a RED rather than a surprise in six months.
///
/// # What is deliberately NOT here
///
/// The server-scoped input contract `actuate::interpret` also sets --
/// `extended-keys`, `escape-time`, `set-clipboard` -- is a different concern:
/// a keyboard and clipboard contract for the server, not the operator's
/// surface. Whether Founder wants those too is a real question that needs
/// measuring rather than assuming, and folding them in here silently would be
/// answering it by accident. Adding one is a deliberate act with this comment
/// as its decision point.
pub const OPERATOR_TERMINAL_OPTIONS: [[&str; 3]; 2] = [
    // The rail is a mouse surface and this is what makes the wheel reach a
    // pane at all.
    ["-g", "mouse", "on"],
    // THE STATUS BAR IS GONE. Not hidden behind an option and not restored on
    // any path: the operator ruled it out of the product, and the rail is the
    // navigation surface that replaced it. Founder has no rail, but it is the
    // same operator surface before there is a company, and a status bar
    // appearing in one session and not the next is a seam with nothing behind
    // it.
    ["-g", "status", "off"],
];

pub mod client;
pub mod crash_loop;
pub mod desired;
pub mod ever_observed;
pub mod exec;
pub mod fake;
pub mod host;
pub mod interpret;
pub mod launch_catalog;
pub mod observe;
pub mod plan;
pub mod probe;
pub mod terminal_features;
pub use host_primitives::redact;
pub mod resident;
pub mod runner;
pub mod spawn_cmd;
pub mod supervise;
pub mod trust;

pub use crate::placement::safe_window_name;
pub use crash_loop::{crash_loop_line, CrashLoop, CrashReport, MAX_RETRY_DELAY};
pub use desired::{DesiredPerson, DesiredRuntime, HoldReason};
pub use ever_observed::EverObserved;
pub use exec::{ObservedPane, SessionAudit, TmuxHost};
pub use fake::{FakeHostExecutor, ScriptedReply, ScriptedTmux};
pub use host::{
    DriftReport, HostErr, HostExecutor, MaterializeFile, MaterializePlan, PaneId, PaneIdentity,
    PanePlan, Pid, ProcIdentity, Socket, TmuxCmd, TmuxOut,
};
pub use interpret::{
    apply_plan, apply_plan_with_launch_roster, refresh_single_ordinary_viewport_session,
    resize_session_viewport, resize_session_viewport_for_attach,
    resize_session_viewport_for_client, revoke_client_viewport_tokens,
    revoke_client_viewport_tokens_for_client, viewport_client_is_eligible, ApplyReport,
    AttachViewportPublication, CommittedBindings, LaunchInputs, LaunchRosterDiagnostics, StepError,
};
pub use launch_catalog::{LaunchCatalog, LaunchEntry, ResolvedCatalog};
pub use observe::{observe, ObserveError};
pub use resident::{actuator_id, catalog_unavailable, round_line, round_outcome, Round};
pub use runner::{RecordingWaiter, SystemTmuxRunner, ThreadWaiter, TmuxRunner, Waiter};
pub use spawn_cmd::{launch_command, LaunchSpec, PaneCommand};
pub use trust::{
    classify, classify_ownership, classify_presence, classify_tag_read, rebuild_decision,
    ObservedTags, Ownership, RebuildDecision, SessionPresence, TagRead, TmuxObjectKind, Trust,
    UnprovenCause, SERVER_EXITED_RETRIES, SERVER_EXITED_RETRY_DELAY_MS,
};
