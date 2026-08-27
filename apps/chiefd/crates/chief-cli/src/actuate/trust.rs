//! The tmux trust rules — ported verbatim from the TypeScript launcher.
//!
//! Plan §4: *"the 20 × 25 ms 'server exited unexpectedly' retry never reads as
//! takeover permission; the 'invalid option' no-tag response is equally
//! untrusted; foreign/partial sessions are never killed or adopted;
//! absence-only rebuild (invariant 9)."*
//!
//! The whole module exists because of one class of bug: **a tmux command that
//! failed to answer being read as tmux answering "nothing is there."** That
//! reading is what turns a hiccup into a takeover — killing or adopting panes
//! belonging to a live company. So the classification here is a three-valued
//! logic, not a boolean:
//!
//! | Observation | Meaning |
//! |---|---|
//! | [`SessionPresence::Present`] | tmux answered: it is there |
//! | [`SessionPresence::ProvablyAbsent`] | tmux answered: it is not there |
//! | [`SessionPresence::Unproven`] | tmux did not answer |
//!
//! Only the middle one authorizes a rebuild. There is deliberately **no**
//! `From<SessionPresence> for bool`: the moment absence and non-answer can be
//! spelled the same way, the historical bug is reachable again.
//!
//! Ported sources: `src/organization/org-tmux.ts:374-387` (`sessionExists`
//! with `requireProvableAbsence`), `:588-620` (`assertUnambiguousObservation`),
//! `src/organization/org-runtime-ownership.ts:133-151` (the 20 × 25 ms retry),
//! `src/organization/org-supervisor.ts:637` (`invalid option` untrusted).

/// Number of retries for the transient "server exited unexpectedly" condition.
///
/// Ported from `org-runtime-ownership.ts:135` (`attempt < 20`).
pub const SERVER_EXITED_RETRIES: u32 = 20;

/// Delay between those retries, in milliseconds
/// (`org-runtime-ownership.ts:149`).
pub const SERVER_EXITED_RETRY_DELAY_MS: u64 = 25;

/// tmux option names carrying launcher ownership
/// (`org-tmux.ts:5-11`, `ORGANIZATION_TMUX_TAGS`).
pub mod tags {
    /// Company slug tag, set on session, window and pane.
    pub const ORGANIZATION: &str = "@organization_id";
    /// Logical window id tag.
    pub const WINDOW: &str = "@organization_window_id";
    /// Person id tag, panes only.
    pub const PERSON: &str = "@organization_person_id";
    /// The derived launch hash the pane was started at, panes only.
    ///
    /// `chiefd_core::runtime::launch_hash::desired_launch_hash`: a content hash
    /// of what the process was BUILT FROM (identity, placement, launch command,
    /// extension source digest), published in chiefd's desired set. Any change
    /// to an input moves the hash whether or not its author knew this fence
    /// existed, and a pane whose tag does not equal the desired hash is
    /// REPLACED rather than adopted.
    pub const LAUNCH_HASH: &str = "@organization_launch_hash";
    /// The permanent focus body reserved by a cold person click.
    ///
    /// It is clean Chief furniture, not person ownership. The actuator may
    /// claim only this exact pane for the named person, then removes the tag
    /// as it publishes the final person ownership and process in place.
    pub const WAKING_PERSON: &str = "@chief_waking_person";
    /// Unique acknowledgement for the card action that changed SLEEPING into
    /// WAKING. It prevents a lost tmux reply from accepting another actor's
    /// wake for the same person.
    pub const WAKE_CLAIM: &str = "@chief_wake_claim";
    /// The claim is a new local hand-off which has not yet been observed in
    /// ChiefD's desired set. This shared pane-local fence protects it from a
    /// different rail process during the POST-to-changefeed interval.
    pub const WAKING_PENDING: &str = "@chief_waking_pending_claim";
    /// The exact waking claim which a rail observed while the person was
    /// desired. Only the later withdrawal of this same shared claim can make
    /// it orphan recovery authority.
    pub const WAKING_DESIRED_SEEN: &str = "@chief_waking_desired_claim";
    /// A focused sleeping-person card, before its Wake Up button is activated.
    pub const SLEEPING_PERSON: &str = "@chief_sleeping_person";
    /// #18 P2 / task #23: a chiefd-INTERNAL marker, not part of the ported
    /// `ORGANIZATION_TMUX_TAGS` contract above and never read or written by
    /// the TypeScript side. Set on a freshly minted window/pane BEFORE its
    /// first identity tag and cleared AFTER its last — see
    /// `converge_apply::interpret`'s mint/tag sequence doc. A window or pane
    /// still carrying this marker when the NEXT pass starts means the
    /// process that minted it died mid-sequence; it is reaped
    /// (`interpret::reap_torn_mints`) before that pass observes or plans,
    /// rather than surfacing as a permanently-fatal or permanently-duplicated
    /// object.
    pub const MINTING: &str = "@organization_minting";
    /// The operator's sidebar rail. PANES ONLY, and that is load-bearing.
    ///
    /// A rail pane carries [`ORGANIZATION`] and this, and deliberately NEVER
    /// carries [`PERSON`]: it is not a person and must never be adopted as one.
    ///
    /// Because it lives only on panes, it answers TWO questions with one fact,
    /// and neither answer can drift from the other: "is this pane the rail?"
    /// (`interpret::observe_rail`, which reserves its column in the layout) and
    /// "is this company operated with a rail?" — which is simply whether any
    /// pane in the session carries it. The second question deliberately has no
    /// separate session-scoped flag: a flag would need something to clear it,
    /// nothing would, and an operator who closed every rail would keep getting
    /// a fresh one on every window minted afterwards.
    ///
    /// The attach sweep decides a company is railed and rails every window it
    /// enumerates; the converge loop then maintains that invariant for windows
    /// minted later (`interpret::ensure_rail_in_window`).
    pub const SIDEBAR: &str = "@organization_sidebar";

    /// The rail's own SLEEPING-DEPARTMENT notice, named for the department it
    /// speaks for.
    ///
    /// Panes only, outside the
    /// `@organization_*` family, so the converge audit reads it as unrelated and
    /// neither adopts nor reaps it. It exists because a department where
    /// everybody is asleep has no window of its own, and the answer to clicking
    /// one must be that department saying so — never another department's
    /// window standing in for it.
    pub const ASLEEP: &str = "@chief_asleep_for";

    /// The FINGERPRINT of what a department overview card is currently drawing.
    ///
    /// A SHA-256 of the card's own JSON payload, stamped on the card pane by
    /// whichever verb spawned the program in it, and the whole of the guard
    /// that keeps `effects::refresh_department_card` off the glass.
    ///
    /// It is a fingerprint and not the payload because the value is read back
    /// through a tmux FORMAT (`#{@chief_department_card}`) and a JSON document
    /// carries `#`, `{` and `}` — a payload read back that way would be
    /// re-expanded by tmux rather than compared. Hex answers the only question
    /// asked of it, which is "is this the same card", and answers it in 64
    /// characters whatever the department's size.
    ///
    /// Panes only, and outside the `@organization_*` family for the same reason
    /// [`ASLEEP`] is: the converge audit must read it as unrelated furniture and
    /// neither adopt nor reap the pane that carries it.
    pub const DEPARTMENT_CARD: &str = "@chief_department_card";
}

/// The tmux session options that hold the operator's sidebar preference.
///
/// # What used to be here, and why one process needs none of it
///
/// Five constants: `COMPANY` (the whole company as JSON, published by the
/// actuator and rendered by every rail), `SELECTION` (what the operator last
/// clicked, plus the gesture that decided it), `WAKING` (a set of in-flight
/// wakes with a sixty-second grace), `GESTURE` (when any process last moved
/// this session's geometry) and `COLUMNS`.
///
/// The first four were a COORDINATION PROTOCOL between rail processes, and
/// tmux was the shared store because it was the only thing all of them already
/// talked to. There is one process now — `crate::sidebar::brain` — and every
/// one of those four is a plain field of it, so the bus, its two parsers, the
/// `send-keys` doorbells that told a sibling to re-read it and the grace timer
/// that bounded a record a dead rail could leave behind are all deleted.
///
/// These survive because they are operator preferences, not coordination
/// between rail processes. They are session-local so two companies can keep
/// different widths.
pub mod sidebar_options {
    /// The expanded width the operator last chose with a rail-border drag.
    pub const COLUMNS: &str = "@chief_sidebar_columns";
    /// Whether the operator collapsed this session's rails with the control.
    pub const COLLAPSED: &str = "@chief_sidebar_collapsed";
}

/// Ephemeral tmux-server authority for the no-transit viewport path.
pub mod viewport_options {
    /// Random identity of one tmux server lifetime on this socket.
    pub const SERVER_NONCE: &str = "@chief_viewport_server_nonce";
    /// The only company session with exactly one eligible ordinary client.
    pub const FAST_SESSION: &str = "@chief_viewport_fast_session";
    /// The exact ordinary client that owns the current fast manifest.
    pub const FAST_OWNER: &str = "@chief_viewport_fast_owner";
    /// The organization tag captured with the current fast manifest.
    pub const FAST_ORGANIZATION: &str = "@chief_viewport_fast_organization";
    /// The membership generation captured with the current fast manifest.
    pub const FAST_GENERATION: &str = "@chief_viewport_fast_generation";
    /// Session-local hidden command that rebuilds the current literal manifest.
    pub const REFRESH_COMMAND: &str = "@chief_viewport_refresh_command";
    /// Session-local hidden command that commits one human rail drag.
    pub const WIDTH_COMMAND: &str = "@chief_viewport_width_command";
    /// Monotonic client-membership generation used by the census CAS.
    pub const MEMBERSHIP_GENERATION: &str = "@chief_viewport_membership_generation";
    /// Server-global monotonic counter that mints topology epochs.
    pub const TOPOLOGY_GENERATION: &str = "@chief_viewport_topology_generation";
    /// The exact topology epoch owned by one company session.
    pub const TOPOLOGY_EPOCH: &str = "@chief_viewport_topology_epoch";
    /// The session epoch embedded in the currently installed native manifest.
    pub const MANIFEST_EPOCH: &str = "@chief_viewport_manifest_epoch";
}

/// True only for one product-owned company session name that is safe as a
/// literal tmux target and format operand.
#[must_use]
pub fn is_safe_company_session(value: &str) -> bool {
    value.starts_with("org-")
        && value.ends_with('_')
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// True for one logical organization identifier stored in tmux options.
#[must_use]
pub fn is_safe_logical_id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// True for one logical WINDOW id — the `@organization_window_id` tag value.
///
/// # Why this is not [`is_safe_logical_id`]
///
/// That rule permits `[A-Za-z0-9_-]` and no colon, which is correct for what it
/// actually guards: an ORGANIZATION id, which is interpolated into tmux targets
/// where `:` separates session from window. A colon there would be a target
/// injection, so the ban is load-bearing and stays.
///
/// **The product's own window ids contain a colon**, by construction:
/// `__person__:<person>` and `__overview__:<department>` (`placement.rs`), plus
/// the bare `__focus__`. So the generic rule rejected the grammar this codebase
/// writes, and `viewport_manifest_survey` — its only window-tag caller —
/// therefore VOIDED on every session containing a person or overview window,
/// which is every real session. Measured on a live box: a refusal every
/// ~20 seconds, each one a distinct hook-spawned `chief` process that refused
/// and exited, reading `window @1 carries an unsafe logical id
/// '__person__:chief'`. `@1` is the Chief's window only because it is first in
/// scan order.
///
/// **The interpolation-safety property is preserved exactly** where it varies:
/// the SUFFIX after the prefix is validated by [`is_safe_logical_id`], so a
/// person or department id can no more carry a colon than before. What is
/// accepted is the product's three shapes and nothing else — a colon-bearing
/// tag that is not one of them (`foo:bar`) is still refused.
#[must_use]
pub fn is_safe_window_logical_id(value: &str) -> bool {
    if value == crate::placement::FOCUS_WINDOW_ID {
        return true;
    }
    for prefix in ["__person__:", "__overview__:"] {
        if let Some(suffix) = value.strip_prefix(prefix) {
            return is_safe_logical_id(suffix);
        }
    }
    // Everything else is judged by the ordinary rule, so a plain department
    // window id keeps working exactly as it did.
    is_safe_logical_id(value)
}

/// True for one random tmux server-lifetime nonce.
#[must_use]
pub fn is_safe_server_nonce(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// How chiefd is allowed to read one tmux response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// The response describes the world; act on it.
    Authoritative,
    /// The response tells us nothing. Retry, but never conclude absence and
    /// never take over.
    Untrusted,
}

/// Why an observation could not be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnprovenCause {
    /// A just-crashed tmux server briefly leaves a socket that reports neither
    /// presence nor absence. This one — and only this one — is retried.
    ServerExitedUnexpectedly,
    /// `show-options -v` against a tmux that does not know the option answers
    /// `invalid option`. A live session with no readable tag has told us
    /// **nothing** about ownership; it has not told us the session is free.
    InvalidOption,
    /// tmux failed in a way this port has never seen. Unrecognized is
    /// untrusted, by construction: the safe default for an unknown diagnostic
    /// is "we do not know", never "nobody is home".
    UnrecognizedDiagnostic,
}

impl UnprovenCause {
    /// Whether the retry ladder applies. Only the transient server crash is
    /// retried; an `invalid option` will answer identically twenty times.
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::ServerExitedUnexpectedly)
    }
}

/// The three-valued answer to "is this session there?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPresence {
    /// tmux answered yes.
    Present,
    /// tmux answered no, with a diagnostic that proves absence.
    ProvablyAbsent,
    /// tmux did not answer.
    Unproven(UnprovenCause),
}

/// The result of reading one ownership tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagRead {
    /// tmux answered with the option's value. An empty string is a *value*:
    /// the tag is set to nothing, which fails the ownership comparison.
    Value(String),
    /// tmux did not answer. Never collapse this into `Value(String::new())`.
    Untrusted(UnprovenCause),
}

/// Ownership verdict for one observed tmux object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// Fully tagged and tagged for *this* company.
    Ours,
    /// Carries no launcher tag at all — somebody else's pane in somebody
    /// else's session. Left strictly alone (`org-tmux.ts:592`, `:607`).
    Unrelated,
    /// Carries a launcher tag naming a different company.
    Foreign,
    /// Carries some launcher tags but not all of them. Refusing here is the
    /// point: a half-tagged object is exactly what an interrupted reconcile
    /// leaves behind, and guessing at it is how a live pane dies.
    NotFullyTagged,
}

impl Ownership {
    /// Whether chiefd may adopt this object into its projection.
    #[must_use]
    pub fn may_adopt(self) -> bool {
        matches!(self, Self::Ours)
    }

    /// Whether chiefd may kill this object.
    #[must_use]
    pub fn may_kill(self) -> bool {
        matches!(self, Self::Ours)
    }

    /// Whether encountering this object must abort the whole reconcile rather
    /// than being skipped. A `Foreign`/`NotFullyTagged` object inside the
    /// session we believe we own means our model of the world is wrong.
    #[must_use]
    pub fn aborts_reconcile(self) -> bool {
        matches!(self, Self::Foreign | Self::NotFullyTagged)
    }
}

/// Which kind of tmux object was observed — panes carry two more required
/// tags than windows do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxObjectKind {
    /// A window: requires organization + logical window id.
    Window,
    /// A pane: additionally requires person id and a launch hash.
    Pane,
}

/// The launcher tags read off one tmux object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedTags {
    /// `@organization_id`.
    pub organization_id: String,
    /// `@organization_window_id`.
    pub window_id: String,
    /// `@organization_person_id` (panes).
    pub person_id: String,
    /// `@organization_launch_hash` (panes) — the derived hash the pane was
    /// launched at. A string, compared for equality and never parsed.
    pub launch_hash: String,
}

impl ObservedTags {
    /// Whether any launcher tag is set at all (`org-tmux.ts:591`, `:606`).
    #[must_use]
    pub fn has_any_launcher_tag(&self) -> bool {
        !self.organization_id.is_empty()
            || !self.window_id.is_empty()
            || !self.person_id.is_empty()
            || !self.launch_hash.is_empty()
    }
}

/// Classify one observed object against the company chiefd believes owns it.
#[must_use]
pub fn classify_ownership(
    kind: TmuxObjectKind,
    tags: &ObservedTags,
    expected_organization: &str,
) -> Ownership {
    if !tags.has_any_launcher_tag() {
        return Ownership::Unrelated;
    }
    if !tags.organization_id.is_empty() && tags.organization_id != expected_organization {
        return Ownership::Foreign;
    }
    if tags.organization_id.is_empty() || tags.window_id.is_empty() {
        return Ownership::NotFullyTagged;
    }
    if kind == TmuxObjectKind::Pane {
        if tags.person_id.is_empty() {
            return Ownership::NotFullyTagged;
        }
        // A pane with no launch hash is not usable as evidence of anything.
        // Presence is the whole test, and that is deliberate: any value other
        // than the one chiefd published simply fails the diff and the pane is
        // replaced, so there is nothing a stricter check here could protect.
        if tags.launch_hash.is_empty() {
            return Ownership::NotFullyTagged;
        }
    }
    Ownership::Ours
}

/// What chiefd may do about a session, given a presence observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildDecision {
    /// Absence is proven; rebuild the session from the durable plan
    /// (invariant 9).
    Rebuild,
    /// The session is there; audit and reconcile it, never rebuild.
    LeaveRunning,
    /// tmux did not answer. Do nothing at all.
    Refuse(UnprovenCause),
}

/// Invariant 9, as one function: rebuild on **proven absence only**.
#[must_use]
pub fn rebuild_decision(presence: SessionPresence) -> RebuildDecision {
    match presence {
        SessionPresence::ProvablyAbsent => RebuildDecision::Rebuild,
        SessionPresence::Present => RebuildDecision::LeaveRunning,
        SessionPresence::Unproven(cause) => RebuildDecision::Refuse(cause),
    }
}

fn mentions_server_exit(diagnostic: &str) -> bool {
    diagnostic.contains("server exited unexpectedly")
}

fn mentions_invalid_option(diagnostic: &str) -> bool {
    diagnostic.contains("invalid option")
}

/// The diagnostics that genuinely prove absence
/// (`org-tmux.ts:378-384`).
fn proves_absence(diagnostic: &str) -> bool {
    let trimmed = diagnostic.trim();
    trimmed == "no server"
        || diagnostic.contains("no server running")
        || diagnostic.contains("can't find session")
        || diagnostic.contains("no such session")
        || (diagnostic.contains("error connecting to")
            && diagnostic.contains("no such file or directory"))
}

/// Classify a `has-session` result. The caller must have asked for provable
/// absence — there is no lenient mode here, because the lenient mode in the
/// TypeScript source (`requireProvableAbsence = false`) is what treats any
/// non-zero exit as "not there".
#[must_use]
pub fn classify_presence(status: i32, stdout: &str, stderr: &str) -> SessionPresence {
    if status == 0 {
        return SessionPresence::Present;
    }
    let diagnostic = format!("{stderr}\n{stdout}").to_lowercase();
    if mentions_server_exit(&diagnostic) {
        return SessionPresence::Unproven(UnprovenCause::ServerExitedUnexpectedly);
    }
    if mentions_invalid_option(&diagnostic) {
        return SessionPresence::Unproven(UnprovenCause::InvalidOption);
    }
    if proves_absence(&diagnostic) {
        return SessionPresence::ProvablyAbsent;
    }
    SessionPresence::Unproven(UnprovenCause::UnrecognizedDiagnostic)
}

/// Classify an ownership-tag read (`show-options -v @organization_id`).
///
/// There is no `Absent` variant on purpose: tmux reports an unset option as
/// `invalid option`, which is indistinguishable from a tmux too old to know
/// the option at all. Both mean *we could not read ownership*, and neither
/// means *this object is unowned*.
#[must_use]
pub fn classify_tag_read(status: i32, stdout: &str, stderr: &str) -> TagRead {
    if status == 0 {
        return TagRead::Value(stdout.trim().to_owned());
    }
    let diagnostic = format!("{stderr}\n{stdout}").to_lowercase();
    if mentions_server_exit(&diagnostic) {
        return TagRead::Untrusted(UnprovenCause::ServerExitedUnexpectedly);
    }
    if mentions_invalid_option(&diagnostic) {
        return TagRead::Untrusted(UnprovenCause::InvalidOption);
    }
    TagRead::Untrusted(UnprovenCause::UnrecognizedDiagnostic)
}

/// Classify a general tmux invocation result.
///
/// A non-zero status is *not* by itself evidence of absence: the transient and
/// no-tag cases below are where tmux failed to answer rather than answering
/// "no".
#[must_use]
pub fn classify(status: i32, stderr: &str) -> Trust {
    match classify_presence(status, "", stderr) {
        SessionPresence::Present | SessionPresence::ProvablyAbsent => Trust::Authoritative,
        SessionPresence::Unproven(_) => Trust::Untrusted,
    }
}

#[cfg(test)]
mod tests {
    use crate::actuate::*;

    #[test]
    fn transient_server_exit_is_never_takeover_permission() {
        assert_eq!(classify(1, "server exited unexpectedly"), Trust::Untrusted);
        assert_eq!(classify(1, "tmux: server exited unexpectedly\n"), Trust::Untrusted);
        assert_eq!(
            classify_presence(1, "", "server exited unexpectedly"),
            SessionPresence::Unproven(UnprovenCause::ServerExitedUnexpectedly)
        );
        assert_eq!(
            rebuild_decision(classify_presence(1, "", "server exited unexpectedly")),
            RebuildDecision::Refuse(UnprovenCause::ServerExitedUnexpectedly),
            "a transient failure must never authorize a rebuild"
        );
    }

    #[test]
    fn invalid_option_no_tag_response_is_untrusted() {
        // An old tmux that cannot report our tags has not told us the pane is
        // unowned (`org-supervisor.ts:637`).
        assert_eq!(classify(1, "invalid option: -F"), Trust::Untrusted);
        assert_eq!(
            classify_tag_read(1, "", "invalid option: @organization_id"),
            TagRead::Untrusted(UnprovenCause::InvalidOption)
        );
    }

    #[test]
    fn an_unreadable_tag_never_becomes_an_empty_tag() {
        // The bug this forbids: `show-options` fails, the caller stores "" and
        // then concludes the object is untagged, therefore free.
        let read = classify_tag_read(1, "", "invalid option: @organization_id");
        assert!(!matches!(read, TagRead::Value(_)));
        let TagRead::Untrusted(cause) = read else { panic!("expected untrusted") };
        assert!(!cause.is_retryable(), "an invalid option answers identically forever");
    }

    #[test]
    fn a_tag_set_to_the_empty_string_is_a_value_and_fails_the_comparison() {
        assert_eq!(classify_tag_read(0, "", ""), TagRead::Value(String::new()));
        let tags = ObservedTags { organization_id: String::new(), ..ObservedTags::default() };
        assert!(!tags.has_any_launcher_tag());
    }

    #[test]
    fn genuine_absence_answers_are_authoritative() {
        assert_eq!(classify(1, "no server running on /tmp/tmux-0/co"), Trust::Authoritative);
        assert_eq!(classify(1, "can't find session: co"), Trust::Authoritative);
        for diagnostic in [
            "no server running on /tmp/tmux-1000/cobalt",
            "can't find session: cobalt",
            "no server",
            "error connecting to /tmp/tmux-1000/cobalt (No such file or directory)",
        ] {
            assert_eq!(
                classify_presence(1, "", diagnostic),
                SessionPresence::ProvablyAbsent,
                "{diagnostic} proves absence"
            );
            assert_eq!(
                rebuild_decision(classify_presence(1, "", diagnostic)),
                RebuildDecision::Rebuild
            );
        }
    }

    #[test]
    fn rebuild_happens_on_proven_absence_only() {
        assert_eq!(rebuild_decision(SessionPresence::Present), RebuildDecision::LeaveRunning);
        assert_eq!(rebuild_decision(SessionPresence::ProvablyAbsent), RebuildDecision::Rebuild);
        for cause in [
            UnprovenCause::ServerExitedUnexpectedly,
            UnprovenCause::InvalidOption,
            UnprovenCause::UnrecognizedDiagnostic,
        ] {
            assert_eq!(
                rebuild_decision(SessionPresence::Unproven(cause)),
                RebuildDecision::Refuse(cause)
            );
        }
    }

    #[test]
    fn success_is_authoritative_and_unknown_failures_are_not() {
        assert_eq!(classify(0, ""), Trust::Authoritative);
        assert_eq!(classify(1, "something nobody has seen before"), Trust::Untrusted);
        assert_eq!(
            classify_presence(1, "", "something nobody has seen before"),
            SessionPresence::Unproven(UnprovenCause::UnrecognizedDiagnostic)
        );
    }

    #[test]
    fn only_the_transient_server_crash_is_retried() {
        assert!(UnprovenCause::ServerExitedUnexpectedly.is_retryable());
        assert!(!UnprovenCause::InvalidOption.is_retryable());
        assert!(!UnprovenCause::UnrecognizedDiagnostic.is_retryable());
    }

    fn ours() -> ObservedTags {
        ObservedTags {
            organization_id: "cobalt".into(),
            window_id: "w-eng".into(),
            person_id: "p-1".into(),
            launch_hash: "9f2c4a".into(),
        }
    }

    #[test]
    fn a_fully_tagged_object_of_ours_may_be_adopted_and_killed() {
        let verdict = classify_ownership(TmuxObjectKind::Pane, &ours(), "cobalt");
        assert_eq!(verdict, Ownership::Ours);
        assert!(verdict.may_adopt() && verdict.may_kill());
        assert!(!verdict.aborts_reconcile());
    }

    #[test]
    fn foreign_sessions_are_never_killed_or_adopted() {
        let mut tags = ours();
        tags.organization_id = "someone-else".into();
        let verdict = classify_ownership(TmuxObjectKind::Pane, &tags, "cobalt");
        assert_eq!(verdict, Ownership::Foreign);
        assert!(!verdict.may_adopt(), "a foreign pane is never adopted");
        assert!(!verdict.may_kill(), "a foreign pane is never killed");
        assert!(verdict.aborts_reconcile());
    }

    #[test]
    fn partially_tagged_objects_are_never_killed_or_adopted() {
        for missing in ["window_id", "person_id", "launch_hash", "organization_id"] {
            let mut tags = ours();
            match missing {
                "window_id" => tags.window_id.clear(),
                "person_id" => tags.person_id.clear(),
                "launch_hash" => tags.launch_hash.clear(),
                _ => tags.organization_id.clear(),
            }
            let verdict = classify_ownership(TmuxObjectKind::Pane, &tags, "cobalt");
            assert_eq!(verdict, Ownership::NotFullyTagged, "missing {missing}");
            assert!(!verdict.may_adopt() && !verdict.may_kill());
            assert!(verdict.aborts_reconcile(), "missing {missing} must abort, not be skipped");
        }
    }

    /// A pane with NO launch hash is not fully tagged. Presence is the whole
    /// test: a hash has no structure to check, and any value other than the one
    /// chiefd published simply fails the diff and the pane is replaced.
    #[test]
    fn a_pane_with_no_launch_hash_is_not_fully_tagged() {
        let tags = ObservedTags { launch_hash: String::new(), ..ours() };
        assert_eq!(
            classify_ownership(TmuxObjectKind::Pane, &tags, "cobalt"),
            Ownership::NotFullyTagged
        );
        for present in ["0", "-1", "abc", "9f2c4a"] {
            let tags = ObservedTags { launch_hash: present.into(), ..ours() };
            assert_eq!(
                classify_ownership(TmuxObjectKind::Pane, &tags, "cobalt"),
                Ownership::Ours,
                "a hash is compared, never parsed: {present:?}"
            );
        }
    }

    #[test]
    fn an_untagged_object_belongs_to_somebody_else_and_is_left_alone() {
        let verdict = classify_ownership(TmuxObjectKind::Pane, &ObservedTags::default(), "cobalt");
        assert_eq!(verdict, Ownership::Unrelated);
        assert!(!verdict.may_kill(), "an unrelated pane is never killed");
        assert!(!verdict.may_adopt());
        assert!(!verdict.aborts_reconcile(), "unrelated objects are skipped, not fatal");
    }

    #[test]
    fn windows_do_not_require_the_pane_only_tags() {
        let tags = ObservedTags {
            organization_id: "cobalt".into(),
            window_id: "w-eng".into(),
            ..ObservedTags::default()
        };
        assert_eq!(classify_ownership(TmuxObjectKind::Window, &tags, "cobalt"), Ownership::Ours);
        assert_eq!(
            classify_ownership(TmuxObjectKind::Pane, &tags, "cobalt"),
            Ownership::NotFullyTagged
        );
    }

    #[test]
    fn retry_budget_matches_the_ported_constants() {
        assert_eq!(SERVER_EXITED_RETRIES, 20);
        assert_eq!(SERVER_EXITED_RETRY_DELAY_MS, 25);
    }
}
