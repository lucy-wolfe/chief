//! Activity: who should be running, and the graceful transition that must be
//! released before anyone stops.
//!
//! This module is the durable activity ledger and the reconcile over it,
//! carrying what a now-deleted pair of TypeScript activity modules used to own.
//! Three things here are load-bearing well beyond their line count:
//!
//! # The launch-intent fence is applied last, and omission is not an off switch
//!
//! [`reconcile`] computes every demand reason first and applies the fence
//! **after**, so replayed durable demand — business monitors, open assignments,
//! open manager goals, stale desired-active state — can never open the fleet on
//! its own. [`LaunchFence`] has no `Unfenced` variant reachable by omission:
//! the caller either passes a person list (possibly empty) or the explicit
//! [`LaunchFence::Unfenced`] sentinel. Absence-is-permissive is unrepresentable
//! (plan §5.5, inv c-1), which is the whole reason the type exists rather than
//! an `Option<Vec<String>>`.
//!
//! # Structural authority has no global counter fence
//!
//! Activity validates against normalized organization rows directly. Person
//! placement is the coordination fact; an unrelated structural update cannot
//! stale an otherwise valid ledger.
//!
//! # TOMBSTONE: the reflection payload is gone; the state machine is not
//!
//! Until #751-P4 a transition carried a bounded five-field "reflection"
//! (summary/learning/handoff/artifacts/openCommitments) that an agent wrote via
//! an `org_reflect` tool before parking, benching, transferring or offboarding,
//! plus everything that payload needed: an aggregate character budget with a
//! convergent canonicalizer, a durability re-read against a normalized
//! `reflection_handoffs` table, and a content-conflict refusal. The product no
//! longer has that concept, so all of it was deleted rather than left dormant —
//! including the two SQL tables (see `schema.rs`).
//!
//! What survives, and is genuinely load-bearing, is the transition STATE
//! MACHINE: an **applied** transition is what sheds launch intent and drives
//! pane teardown for bench/offboard, and the grace window
//! ([`HANDOFF_GRACE_MS`]) is the real pane-grace an offboard depends on.
//! [`release`] is what the payload-bearing `reflect` became: same identity
//! fence, same terminal refusals, no payload.
//!
//! # Polarity: `FailClosed` on read, write and clear
//!
//! §5.5 leaves this open (§5.5b, M12). Closed here as fail-closed throughout:
//!
//! * *read* — an unreadable ledger read as "empty" is "nobody has an open
//!   transition and nobody was running", which lets a structural change proceed
//!   past a release that never happened and lets the projection kill panes it
//!   has no record of.
//! * *write* — overwriting bytes chiefd could not read destroys the transition
//!   records that are the sole authority for D7.
//! * *clear* — activity holds the only durable record that a person owes a
//!   handoff; discarding it is discarding the fence.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::Refusal;
use crate::error::{corrupt_store, store_failure};
use crate::isotime::{iso_millis, parse_iso_millis};
use crate::ledger::Ledgers;
use crate::polarity::{FailClosed, StoreKind};
use crate::runtime::pointer_sweep::{
    reverify_clear, ClearPointerAction, ClearRecheck, SweepStatus,
};
use crate::store::organization::{
    organization_unit_is_active, EmploymentState, OrganizationManifest,
};
use crate::store::supervision::SupervisionLedger;
use crate::ChiefdError;

/// Schema version of the ledger body.
pub const ACTIVITY_SCHEMA_VERSION: u32 = 1;

/// How long a person has to release their transition before it goes overdue.
///
/// Formerly `REFLECTION_GRACE_MS`, with the same value and the same behaviour:
/// the window is REAL and survives the removal of the reflection payload,
/// because it is the pane-grace a bench/offboard relies on. Only
/// the name changed, so nothing here claims a reflection is being waited for.
pub const HANDOFF_GRACE_MS: i64 = 2 * 60 * 1_000;

/// Maximum characters in a transition reason.
pub const TRANSITION_MAX_REASON_CHARACTERS: usize = 500;

/// #312-follow-up: the retention bound on TERMINAL graceful transitions
/// (`Applied`/`Cancelled`/`Forced`). The `activity` ledger was unbounded — the
/// manifest-validity prune keeps every finished transition as long as its
/// person + departments exist, and routine idle auto-park mints one on the hot
/// reconcile path, so a stable-roster company grows this doc without limit
/// (1.29MB live). It is footer-polled at ≥1/30s, so its size is a direct chiefd
/// idle-CPU multiplier (~3ms/MB, #310/#312). Mirrors the TS twin and the
/// supervision terminal-assignment cap (#329).
pub const ACTIVITY_TERMINAL_TRANSITION_LIMIT: usize = 200;

/// The launcher's canonical reason for a routine idle park.
///
/// It is the *identity* of a routine park, not decoration: several rules below
/// ("an authorized staffing command may replace a routine park", "an expired
/// routine park yields its slot") are restricted to this exact string so that
/// an intent-bound park or a manually prepared park stays an authoritative
/// fence.
pub const IDLE_AUTO_PARK_REASON: &str = "Idle auto-park.";

/// How many routine idle parks may be in flight per company at once.
///
/// Bounds the foreground lifecycle work one reconcile can create.
pub const ORGANIZATION_AUTOMATIC_PARK_MAX_IN_FLIGHT: usize = 2;

// TOMBSTONE: THE SETTLE GRACE IS GONE, AND IT IS NOT COMING BACK AS A ZERO.
//
// Three constants used to sit here and stack. A routine idle park was minted
// `AwaitingHandoff` with `requested + HANDOFF_GRACE_MS`, promoted to `Overdue`
// at that deadline, and only FORCED terminal a further
// `ORGANIZATION_AUTOMATIC_PARK_OVERDUE_LEASE_MS` later — 120s + 120s on top of
// a 120s quiet lease. Six minutes, of which the operator was shown the last
// four (`shutting down in 3m 47s`) against a cap he had stated many times:
// TWO MINUTES MAXIMUM FROM SETTLE, TOTAL.
//
// The window is DELETED, not shortened and not set to zero — a constant at
// zero is a fallback in disguise and the next reader restores it. A routine
// idle park is now minted already terminal (see [`new_transition`]), so
// admission and teardown happen in the same reconcile pass and the quiet lease
// below is the entire settle window.
//
// Nothing was waiting in that window. `release` — the only thing that could
// have ended it early — has exactly ONE production caller, the staffing
// lifecycle verb, which prepares and releases inside a single request; the Pi
// extension surface has no release verb at all. So the 240s was a wait for a
// message no code path can send.
//
// [`HANDOFF_GRACE_MS`] is untouched and still real: it is the pane-grace a
// bench/transfer/offboard depends on, and those DO get released.

/// THE WHOLE SETTLE WINDOW: five minutes from idle to the pane being gone
/// (2026-08-24; it was two).
///
/// Non-CEO people stay resident for this long after their last durable demand
/// clears, measured from the moment a host observation proves them RESIDENT —
/// never from the moment chiefd decided they should run. See the stamp site in
/// `reconcile`: starting the clock at desired-active let a slow pane start burn
/// the whole lease before the process existed.
///
/// This is the operator's "2min max from settle", and it is the ONLY duration
/// on the path: nothing is added after it. A routine idle park is minted
/// already terminal, so the pass that admits the park is the pass that
/// withdraws the launch intent. The maximum elapsed from `idle_since` to the
/// pane being gone is therefore this constant plus the latency of one reconcile
/// pass — there is no second deadline to sum with. See the TOMBSTONE above for
/// the three-phase stack this replaced.
///
/// Deliberately NOT related to [`HANDOFF_GRACE_MS`]: that bounds a STRUCTURAL
/// handoff (bench/transfer/offboard), which is a different
/// operation with a real releaser. A routine idle stop has no handoff to bound.
///
/// # THE OBSERVED SETTLE IS LONGER THAN THIS, AND THIS IS NOT THE LEVER
///
/// "The latency of one reconcile pass" above is up to SIXTY SECONDS, so an
/// operator watching a quiet company sees the pane go at up to SIX minutes,
/// not five. The park decision is a branch inside the reconcile pass, and
/// `SupervisionReconcile` is a REACTIVE duty: its nominal 30s interval
/// (`supervisor_watermark.rs`) is demoted to `max(interval, fallback_floor)` by
/// `supervise` in `chiefd-daemon/src/run.rs`, and that floor is
/// `DEFAULT_REACTIVE_FALLBACK_FLOOR` = 60s. At rest, that is the sampling gap.
///
/// **Do NOT lower this constant to make the total look like 300s.** That is
/// the move the next reader reaches for, and it corrupts a correct number to
/// paper over a scheduling floor — the settle window would then be wrong
/// everywhere it is reasoned about, including on the reactive path where the
/// gap does not apply at all. If the observed latency is the problem, the
/// reactive floor is the thing to argue about. This lease is the operator's
/// stated cap and it already holds.
///
/// **`org_settings.supervision_interval_ms` is not the knob either.** It
/// exists in the schema and is projected out to callers, but no Rust loop
/// reads it; changing it moves nothing here.
/// # 2026-08-24: 120s -> 300s, by operator ruling
///
/// *"lets bump the 2mins to a 5mins."* Every number in this doc block and in
/// [`AGENT_ACTIVITY_LIVENESS_MS`]'s was re-derived from 300s in the same
/// commit; none of them is a remembered figure.
///
/// **THE OPERATOR WAKE FLOOR MOVES WITH IT, ON PURPOSE.**
/// [`operator_wake_lease_active`] reads THIS constant, so the "woken people are
/// left alone" floor goes 2 minutes to 5 with it. That is one constant by
/// deliberate choice, not a side effect: the wake floor IS the settle window
/// measured from the click rather than from the last beat, the invariant is a
/// FLOOR and not a ceiling (CLAUDE.md), and 5 minutes satisfies the original
/// ruling's "at least the 2 mins" strictly more than 2 did. Splitting the two
/// constants without an operator asking for a split would be inventing a
/// second number nobody has reasoned about.
pub const ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS: i64 = 300 * 1_000;

/// How long one reported agent-activity beat keeps the quiet lease cancelled
/// without a further beat.
///
/// THE DEFECT THIS BOUND EXISTS INSIDE (operator, 2026-08-10): a pane showed
/// "settling - 39s" while its agent was demonstrably mid-turn. The lease was
/// stamped from the ABSENCE OF DURABLE DEMAND alone, which says nothing about
/// what the process is doing, so an agent holding no open goal was counted idle
/// while it was thinking, calling tools and sending mail -- and 120s later
/// became a routine-park candidate that can be forced terminal under itself.
/// The fix is a fact about the AGENT, and this is how long that fact is trusted.
///
/// Derivation. The pane beats on the SAME event set that already feeds
/// `noteTurnProgress` (turn start, message start/update/end, tool execution
/// start/update/end), throttled in-process to at most one beat per 30s. So:
///   300_000 ms / 30_000 ms = 10 -- ten consecutive beats must be lost before a
///   genuinely working agent is misread as quiet.
/// Upper bound it must stay under: this is the only thing standing between a
/// pane that DIED MID-TURN and immortality, so it must expire well inside the
/// operator's patience for a dead pane holding a seat. Since the quiet lease
/// moved to 300s (2026-08-24) this is 1.0x it rather than 2.5x, so such a pane
/// settles in at most `AGENT_ACTIVITY_LIVENESS_MS` +
/// [`ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS`] = 600s.
///
/// **The ratio changed and this constant deliberately did NOT.** The operator's
/// ruling moved the quiet lease and named no other number, and raising liveness
/// to restore a 2.5x ratio would be inventing a figure to preserve an
/// arithmetic relationship that was never itself the requirement. The
/// requirement is the sentence above — expire well inside the operator's
/// patience for a dead pane holding a seat — and ten minutes still satisfies
/// it. The ten-lost-beats derivation is untouched: it depends on the 30s beat
/// throttle, not on the lease.
///
/// # The case this bound does NOT cover, stated plainly
///
/// "600s rather than never" is true for a pane that BEAT AT LEAST ONCE and then
/// died. It is false for a pane that boots and dies before its first beat.
/// [`agent_quiet_since`] derives the quiet instant from `agent_active_at`, so a
/// person who never beat has no `agent_active_at`, therefore no quiet instant,
/// therefore no clock -- ever. In chiefd that person stays desired-active
/// indefinitely.
///
/// That is not an oversight in this constant and it is not fixable here. A
/// timeout can only convert a beat that STOPPED into silence; it has nothing to
/// measure from when no beat ever arrived. Inventing a start instant -- the
/// launch, say -- would mean chiefd claiming to know when the process started,
/// which is a host fact it deliberately no longer has.
///
/// **The case is handled, in the one place that can handle it.** A pane that
/// boots and dies before its first beat is exactly what a crash loop looks like
/// from the actuator's side: `chief-cli`'s `actuate::crash_loop` sees that it
/// spawned somebody last pass and that their pane is gone this pass, counts
/// that, stops trying after five consecutive failures, and says so on the
/// operator's screen. So the person is never restarted in a loop and the
/// operator is told by name. What does NOT happen is chiefd settling them --
/// chiefd is not told, and cannot be. That is the accepted consequence recorded
/// in the design record, and this is where it actually bites.
///
/// A pane that settles CLEANLY never waits for this at all: `agent_settled`
/// clears the stamp outright and the lease starts on the next reconcile.
pub const AGENT_ACTIVITY_LIVENESS_MS: i64 = 300 * 1_000;

// --- refusal codes ------------------------------------------------------

/// No such transition, or it belongs to somebody else.
pub const UNKNOWN_TRANSITION: &str = "unknown-transition";
/// The transition has already been released and cannot be abandoned.
///
/// Formerly `REFLECTION_PRESENT` (`"reflection-present"`): the refusal survives
/// the removal of the reflection payload because the fact it protects is real —
/// a transition whose handoff already completed must not be retroactively
/// declared unreachable.
pub const TRANSITION_RELEASED: &str = "transition-released";
/// The transition is already applied or cancelled.
pub const TRANSITION_TERMINAL: &str = "transition-terminal";
/// The person's transition has not been released yet.
pub const HANDOFF_REQUIRED: &str = "handoff-required";
/// A field was missing, empty, or over its bound.
pub const INVALID_INPUT: &str = "invalid-input";
/// A named person is not in the manifest.
pub const UNKNOWN_PERSON: &str = "unknown-person";
/// A transition already exists for this person with different terms.
pub const TRANSITION_CONFLICT: &str = "transition-conflict";
/// The store body could not be encoded.
pub const LEDGER_UNSERIALIZABLE: &str = "activity-unserializable";
/// A row the same mutation had just resolved was gone. Always a chiefd bug.
pub const INTERNAL_INCONSISTENCY: &str = "activity-internal-inconsistency";

/// The activity store marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityStore;

impl StoreKind for ActivityStore {
    const NAME: &'static str = "activity";
    type Body = ActivityLedger;
}

impl FailClosed for ActivityStore {}

// --- record types -------------------------------------------------------

/// What a graceful transition is preparing for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransitionAction {
    /// Stop the pane, keep the person.
    Park,
    /// Move the person permanently.
    Transfer,
    /// Remove the person from the company.
    Offboard,
}

impl TransitionAction {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Park => "park",
            Self::Transfer => "transfer",
            Self::Offboard => "offboard",
        }
    }

    /// Parse the wire spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "park" => Some(Self::Park),
            "transfer" => Some(Self::Transfer),
            "offboard" => Some(Self::Offboard),
            _ => None,
        }
    }

    /// Whether the action needs a target department.
    #[must_use]
    pub const fn needs_target(self) -> bool {
        matches!(self, Self::Transfer)
    }

    /// Whether the action removes the pane.
    #[must_use]
    pub const fn is_removal(self) -> bool {
        matches!(self, Self::Park | Self::Offboard)
    }
}

/// Where a transition is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionStatus {
    /// Open: the person's grace window is running and they have not released
    /// the transition yet.
    AwaitingHandoff,
    /// Past the grace deadline, still not released.
    Overdue,
    /// Released by the person who owned it; the structural change may proceed.
    Ready,
    /// The structural change happened.
    Applied,
    /// Superseded or abandoned.
    Cancelled,
    /// Force-parked: stopped WITHOUT a release. The only producer is a routine
    /// with no release, so a routine idle park was applied anyway rather than
    /// retried forever (#337, "idle trends to zero"). Terminal and deliberately
    /// distinct from [`Self::Applied`], which records that the owner released
    /// the transition; `forced` records that nobody ever did. Only ever set on
    /// a plain, non-intent-bound automatic park.
    Forced,
}

impl TransitionStatus {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingHandoff => "awaiting_handoff",
            Self::Overdue => "overdue",
            Self::Ready => "ready",
            Self::Applied => "applied",
            Self::Cancelled => "cancelled",
            Self::Forced => "forced",
        }
    }

    /// Whether the transition is still waiting for a handoff.
    #[must_use]
    pub const fn is_pending(self) -> bool {
        matches!(self, Self::AwaitingHandoff | Self::Overdue)
    }

    /// Whether the transition has been released and the structural change may
    /// proceed. `Forced` is deliberately absent, and still correctly so: a
    /// force-park is applied without any release — now because a routine idle
    /// park never had a releaser, rather than because one failed to arrive.
    #[must_use]
    pub const fn is_released(self) -> bool {
        matches!(self, Self::Ready | Self::Applied)
    }

    /// Whether the transition has reached a terminal status and can no longer be
    /// released or abandoned. `Forced` joins `Applied`/`Cancelled` here.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Applied | Self::Cancelled | Self::Forced)
    }
}

/// Why a person is being kept online this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityReason {
    /// Somebody they manage has open work.
    ManagingOpenWork,
    /// They head the root unit.
    OrganizationRoot,
    /// A wake was explicitly requested.
    Requested,
    /// Their transition is open and has not been released yet.
    HandoffRequired,
    /// Their transition is released and the structural change is pending.
    TransitionReady,
    /// They are an idle-park candidate held back by the in-flight cap.
    MaintenanceBackpressure,
}

impl ActivityReason {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManagingOpenWork => "managing-open-work",
            Self::OrganizationRoot => "organization-root",
            Self::Requested => "requested",
            Self::HandoffRequired => "handoff-required",
            Self::TransitionReady => "transition-ready",
            Self::MaintenanceBackpressure => "maintenance-backpressure",
        }
    }

    /// Whether this reason is durable *work demand* rather than shutdown
    /// bookkeeping (`org-activity-state.ts` `hasEffectiveOnlineDemand`).
    ///
    /// #29 (the operator's settle-shutdown contract): `ManagingOpenWork` is shutdown
    /// bookkeeping, not demand — a manager holding no own goal/loop MUST settle;
    /// the report re-wakes it on cadence via the goal-watch/check-in/escalation
    /// mailbox envelopes. MUST stay in parity with the TS `hasEffectiveOnlineDemand`.
    #[must_use]
    pub const fn is_effective_demand(self) -> bool {
        !matches!(
            self,
            Self::HandoffRequired
                | Self::TransitionReady
                | Self::MaintenanceBackpressure
                | Self::ManagingOpenWork
        )
    }
}

// TOMBSTONE (#751-P4): `ReflectionHandoff` — the bounded five-field payload
// (summary/learning/handoff/artifacts/openCommitments + recordedAt) a person
// wrote here before parking/benching/transferring/offboarding — is DELETED
// along with the whole reflection concept. A transition now records only that
// it was released, never what was said. Nothing replaces it; do not reintroduce
// a payload field on `GracefulTransition` looking for the "missing" half.

/// One graceful transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GracefulTransition {
    /// `transition:<seq>:<person>:<action>`.
    pub id: String,
    /// Whose transition.
    pub person_id: String,
    /// What it prepares for.
    pub action: TransitionAction,
    /// Why, bounded to 500 characters.
    pub reason: String,
    /// Stable lifecycle command identity, when one owns this transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<String>,
    /// The person's placement at the moment the transition opened.
    pub placement_department_id: String,
    // TOMBSTONE (#751-P9): `from_pane_department_id` sat here and recorded the
    // terminal WINDOW the person's pane was drawn in at that instant. Deleted
    // with the backend head-in-parent rule: the two department fields above are
    // org facts, where a pane is drawn never was one, and nothing read this
    // back except the placement chain that is gone.
    /// Target unit; present iff the action needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_department_id: Option<String>,
    /// Lifecycle position.
    pub status: TransitionStatus,
    /// When it opened.
    pub requested_at: String,
    /// When it goes overdue.
    pub handoff_deadline_at: String,
    /// When the structural change happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,
    /// When it was cancelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<String>,
    /// Set **only** alongside `Forced` (#337): the person never released this
    /// transition before their full grace window expired, and it was parked
    /// anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_at: Option<String>,
    /// Set **only** alongside `Cancelled`, and only when the person provably
    /// could not run, so the release this waited for was unreachable. Never
    /// `applied`: `applied` records that the owner released the transition, and
    /// nobody did here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abandoned_at: Option<String>,
}

/// One person's last-known projection state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonActivityState {
    /// Whose state.
    pub person_id: String,
    /// Employment as of the last reconcile.
    pub last_employment_state: EmploymentState,
    /// Placement as of the last reconcile.
    pub last_department_id: String,
    // TOMBSTONE (#751-P9): `last_pane_department_id` sat here. It was the
    // head-in-parent DISPLAY answer, persisted — and therefore durably stale
    // between reconciles: reparent a department and the column still named the
    // old parent until the next activity mutation. The client derives the rule
    // from the CURRENT tree (`chief-cli/src/placement.rs`); nothing durable
    // replaces this.
    /// Whether the unit's ancestry was active.
    pub last_operational: bool,
    /// Whether the last reconcile wanted this person running.
    ///
    /// Seeded **false** (inv 20): a manifest's employment state records whether
    /// someone *may* be activated; it is not evidence that a Pi already exists.
    /// Seeding true made a fresh company manufacture one park/release cycle per
    /// department head before the CEO could delegate anything.
    pub last_desired_active: bool,
    /// Whether the last reconcile that carried a host observation proved this
    /// When the settle countdown started, or `None` when no countdown is
    /// running.
    ///
    /// DERIVED, NEVER ACCUMULATED. Every reconcile recomputes this from
    /// [`agent_quiet_since`] and overwrites it; nothing reads a value written
    /// by an earlier pass. That is deliberate and it fixes a live defect: this
    /// used to be stamped from chiefd's OWN bookkeeping ("I desired them
    /// active, and I see no demand"), so after a chiefd restart with panes
    /// still up, a persisted `idle_since` could already exceed the 120s lease
    /// and the person was stopped with NO grace whatever. A value that is
    /// recomputed from the agent's own reports every pass cannot go stale.
    ///
    /// It remains a stored column only so the surfaced lifecycle status can
    /// read one field rather than re-deriving the clock at every read site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_since: Option<String>,
    /// The instant this person's quiet lease began. This is normally the
    /// agent's explicit `agent_settled`; an explicit operator start also sets
    /// it to the new start time so an old run's expired clock cannot be
    /// inherited. `None` when it is working or no lease has begun.
    ///
    /// Split out from [`Self::agent_active_at`], which conflated "never
    /// reported at all" with "explicitly settled" behind a single `None`. The
    /// countdown rule needs those distinct: a person nobody ever started has no
    /// clock, while a person who said they finished has one starting at the
    /// instant they said it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_quiet_at: Option<String>,
    /// The last instant this person's own pane reported that the AGENT was
    /// doing something -- a turn started, a message streamed, a tool ran, mail
    /// arrived. `None` means the pane reported `agent_settled`, or has never
    /// reported at all.
    ///
    /// This is the only fact in the ledger about the PROCESS rather than about
    /// the supervision ledger's demand, and it exists because those are not the
    /// same question: an agent with no open goal is not thereby idle. Read only
    /// through [`agent_is_working`], which bounds it by
    /// [`AGENT_ACTIVITY_LIVENESS_MS`] so a pane that died mid-turn cannot pin
    /// itself resident for ever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_active_at: Option<String>,
    /// The instant an operator last WOKE this person, or `None` if nobody ever
    /// has.
    ///
    /// THE FLOOR A WAKE BUYS. Operator ruling, 2026-08-20: *"If I tell chief to
    /// message it, it'll come back up and do the 2min settling. We need it to
    /// always do that when woken. Message or not. If woken, it needs to wait the
    /// 2 mins."*
    ///
    /// **The QUOTE stays as spoken; the WINDOW is now 5 minutes.** The 2026-08-24
    /// ruling moved [`ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS`] to 300s and this
    /// floor reads that same constant, deliberately — see its doc for why the
    /// two are one number. The invariant is "at least the settle window,
    /// measured from the click", and a longer window satisfies the original
    /// ruling rather than weakening it: it is a FLOOR, not a ceiling.
    ///
    /// For [`ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS`] after this
    /// instant the person is not a park candidate and their launch intent is not
    /// withdrawn, whatever their agent has or has not said — read through
    /// [`operator_wake_lease_active`].
    ///
    /// Distinct from [`Self::agent_quiet_at`] and [`Self::agent_active_at`],
    /// which are the AGENT's reports about itself. This is the OPERATOR's
    /// decision about the person, and the two answer different questions: a
    /// woken agent that beats once and then has nothing to do is, to every
    /// agent-report rule, indistinguishable from one that finished its work —
    /// which is exactly how a wake came to be withdrawn seconds after it was
    /// paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_wake_at: Option<String>,
    /// The transition this person currently owes, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_transition_id: Option<String>,
    /// ISO-8601 stamp of the last change.
    pub updated_at: String,
}

/// The durable activity ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityLedger {
    /// Always [`ACTIVITY_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The company slug this ledger belongs to.
    pub organization: String,
    /// Canonical person ordering, mirroring the manifest.
    pub person_order: Vec<String>,
    /// Per-person state.
    pub people: BTreeMap<String, PersonActivityState>,
    /// Transitions in creation order.
    pub transition_order: Vec<String>,
    /// Transitions by id.
    pub transitions: BTreeMap<String, GracefulTransition>,
    /// Next value for the `transition:<seq>:` counter.
    pub next_transition_sequence: u64,
    /// Round-robin admission cursor for routine idle park candidates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_park_cursor: Option<usize>,
    /// ISO-8601 creation stamp.
    pub created_at: String,
    /// ISO-8601 stamp of the last write.
    pub updated_at: String,
}

impl ActivityLedger {
    /// The seed ledger for a freshly created company.
    #[must_use]
    pub fn initial(manifest: &OrganizationManifest, now: &str) -> Self {
        let people: BTreeMap<String, PersonActivityState> = manifest
            .people_order
            .iter()
            .filter_map(|person_id| {
                seed_person_state(manifest, person_id, now).map(|state| (person_id.clone(), state))
            })
            .collect();
        Self {
            schema_version: ACTIVITY_SCHEMA_VERSION,
            organization: manifest.slug.clone(),
            person_order: manifest.people_order.clone(),
            people,
            transition_order: Vec::new(),
            transitions: BTreeMap::new(),
            next_transition_sequence: 1,
            automatic_park_cursor: Some(0),
            created_at: now.to_string(),
            updated_at: now.to_string(),
        }
    }

    /// The transition `person_id` currently owes, ignoring cancelled ones.
    #[must_use]
    pub fn active_transition(&self, person_id: &str) -> Option<&GracefulTransition> {
        let state = self.people.get(person_id)?;
        let id = state.active_transition_id.as_deref()?;
        self.transitions.get(id).filter(|t| t.status != TransitionStatus::Cancelled)
    }

    fn active_transition_id_of(&self, person_id: &str) -> Option<String> {
        self.active_transition(person_id).map(|t| t.id.clone())
    }
}

pub(super) fn seed_person_state(
    manifest: &OrganizationManifest,
    person_id: &str,
    now: &str,
) -> Option<PersonActivityState> {
    let person = manifest.people.get(person_id)?;
    Some(PersonActivityState {
        person_id: person_id.to_string(),
        last_employment_state: person.employment_state,
        last_department_id: person.department_id.clone(),
        last_operational: organization_unit_is_active(manifest, &person.department_id),
        last_desired_active: false,
        idle_since: None,
        agent_quiet_at: None,
        agent_active_at: None,
        operator_wake_at: None,
        active_transition_id: None,
        updated_at: now.to_string(),
    })
}

// --- validation ----------------------------------------------------------

fn invalid(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal::new(code, message)
}

/// A row this function itself just resolved is gone.
///
/// Unreachable by construction — the draft is single-threaded and the closure
/// is the only writer — but `expect` is denied in this crate for a reason: a
/// panic on the writer thread takes the whole company's actor down, which is
/// strictly worse than a refused mutation that rolls back cleanly. So the
/// impossible case is an error, and if it ever fires the code names the caller.
fn vanished(what: &str) -> ChiefdError {
    ChiefdError::refused(
        INTERNAL_INCONSISTENCY,
        format!("{what} disappeared inside a single mutation; this is a chiefd bug"),
    )
}

fn exact_order<T>(
    order: &[String],
    records: &BTreeMap<String, T>,
    label: &str,
) -> Result<(), Refusal> {
    let unique: BTreeSet<&String> = order.iter().collect();
    if unique.len() != order.len()
        || order.iter().any(|id| !records.contains_key(id))
        || order.len() != records.len()
    {
        return Err(invalid(INVALID_INPUT, format!("Activity {label} order is invalid")));
    }
    Ok(())
}

/// Every rule `validateActivityLedger` enforces (`org-activity-state.ts:226-304`).
///
/// # Errors
/// [`INVALID_INPUT`] for every structural rule.
#[allow(clippy::too_many_lines)] // One rule per statement; the order is the port.
pub fn validate(ledger: &ActivityLedger, manifest: &OrganizationManifest) -> Result<(), Refusal> {
    if ledger.schema_version != ACTIVITY_SCHEMA_VERSION {
        return Err(invalid(INVALID_INPUT, "Unsupported activity ledger"));
    }
    if ledger.organization != manifest.slug {
        return Err(invalid(
            INVALID_INPUT,
            format!(
                "Activity ledger belongs to '{}', not '{}'",
                ledger.organization, manifest.slug
            ),
        ));
    }
    exact_order(&ledger.person_order, &ledger.people, "person")?;
    exact_order(&ledger.transition_order, &ledger.transitions, "transition")?;

    for person_id in &ledger.person_order {
        let state = &ledger.people[person_id];
        if !manifest.people.contains_key(person_id) || state.person_id != *person_id {
            return Err(invalid(UNKNOWN_PERSON, format!("Unknown activity person '{person_id}'")));
        }
        {
            let unit = &state.last_department_id;
            if !manifest.departments.contains_key(unit) {
                return Err(invalid(
                    INVALID_INPUT,
                    format!(
                        "Activity person '{person_id}' has a prior placement at '{unit}', \
                         which this organization has no department for; a prior placement \
                         must name one of its current departments"
                    ),
                ));
            }
        }
        if parse_iso_millis(&state.updated_at).is_none() {
            return Err(invalid(
                INVALID_INPUT,
                format!("Activity person '{person_id}' has invalid desired state"),
            ));
        }
        for (field, label) in [
            (state.idle_since.as_deref(), "idle lease time"),
            (state.agent_active_at.as_deref(), "agent activity time"),
        ] {
            if let Some(value) = field {
                if parse_iso_millis(value).is_none() {
                    return Err(invalid(
                        INVALID_INPUT,
                        format!("Activity person '{person_id}' has invalid {label}"),
                    ));
                }
            }
        }
        if let Some(transition_id) = state.active_transition_id.as_deref() {
            let transition = ledger.transitions.get(transition_id);
            let bad = transition.is_none_or(|transition| {
                transition.person_id != *person_id
                    || transition.status == TransitionStatus::Cancelled
            });
            if bad {
                return Err(invalid(
                    INVALID_INPUT,
                    format!("Activity person '{person_id}' has an invalid active transition"),
                ));
            }
        }
    }

    let mut maximum_sequence = 0_u64;
    for transition_id in &ledger.transition_order {
        let transition = &ledger.transitions[transition_id];
        if transition.id != *transition_id || !manifest.people.contains_key(&transition.person_id) {
            return Err(invalid(
                INVALID_INPUT,
                format!("Graceful transition '{transition_id}' is invalid"),
            ));
        }
        let sequence = transition_id
            .strip_prefix("transition:")
            .and_then(|rest| rest.split(':').next())
            .and_then(|digits| digits.parse::<u64>().ok())
            .ok_or_else(|| {
                invalid(
                    INVALID_INPUT,
                    format!("Graceful transition '{transition_id}' has an invalid id"),
                )
            })?;
        maximum_sequence = maximum_sequence.max(sequence);
        if transition.reason.trim().is_empty()
            || transition.reason.chars().count() > TRANSITION_MAX_REASON_CHARACTERS
        {
            return Err(invalid(
                INVALID_INPUT,
                format!("Graceful transition '{transition_id}' has an invalid reason"),
            ));
        }
        if !manifest.departments.contains_key(&transition.placement_department_id) {
            return Err(invalid(
                INVALID_INPUT,
                format!("Graceful transition '{transition_id}' has invalid prior placement"),
            ));
        }
        let has_target = transition.to_department_id.is_some();
        if transition.action.needs_target() != has_target
            || transition
                .to_department_id
                .as_deref()
                .is_some_and(|unit| !manifest.departments.contains_key(unit))
        {
            return Err(invalid(
                INVALID_INPUT,
                format!("Graceful transition '{transition_id}' has an invalid target"),
            ));
        }
        let requested = parse_iso_millis(&transition.requested_at);
        let deadline = parse_iso_millis(&transition.handoff_deadline_at);
        match (requested, deadline) {
            (Some(requested), Some(deadline)) if deadline >= requested => {}
            _ => {
                return Err(invalid(
                    INVALID_INPUT,
                    format!("Graceful transition '{transition_id}' has an invalid deadline"),
                ))
            }
        }
        if transition.status == TransitionStatus::Applied
            && transition.applied_at.as_deref().and_then(parse_iso_millis).is_none()
        {
            return Err(invalid(
                INVALID_INPUT,
                format!("Graceful transition '{transition_id}' has no applied timestamp"),
            ));
        }
        if transition.status == TransitionStatus::Cancelled
            && transition.cancelled_at.as_deref().and_then(parse_iso_millis).is_none()
        {
            return Err(invalid(
                INVALID_INPUT,
                format!("Graceful transition '{transition_id}' has no cancelled timestamp"),
            ));
        }
        if transition.status == TransitionStatus::Forced {
            // A forced outcome is only ever a routine idle PARK — the one
            // transition kind that is stopped without a release — and it must
            // carry its own stamp.
            if transition.action != TransitionAction::Park {
                return Err(invalid(
                    INVALID_INPUT,
                    format!("Graceful transition '{transition_id}' is 'forced' but not a 'park'"),
                ));
            }
            if transition.forced_at.as_deref().and_then(parse_iso_millis).is_none() {
                return Err(invalid(
                    INVALID_INPUT,
                    format!("Graceful transition '{transition_id}' has no forced timestamp"),
                ));
            }
        }
        if let Some(abandoned) = transition.abandoned_at.as_deref() {
            if transition.status != TransitionStatus::Cancelled
                || parse_iso_millis(abandoned).is_none()
            {
                return Err(invalid(
                    INVALID_INPUT,
                    format!(
                        "Graceful transition '{transition_id}' has an invalid abandoned timestamp"
                    ),
                ));
            }
        }
        // TOMBSTONE (#751-P4): a `ready`/`applied` transition used to be
        // required to carry a well-formed reflection payload here, and #452
        // then split that check into a fatal structural half and a non-fatal
        // aggregate-budget quarantine. Both halves are gone with the payload.
        //
        // The relaxation is deliberately in the SAFE direction. `validate` is
        // the gate of a fail-closed store: every read and every mutation runs
        // it, so a rule that newly REJECTS state which is already on disk
        // refuses every duty at every existing company. Dropping a requirement
        // can only widen what validates. A `ready`/`applied` transition with no
        // payload — which is now every one of them — is legal, exactly as it
        // must be.
    }
    if ledger.next_transition_sequence <= maximum_sequence {
        return Err(invalid(INVALID_INPUT, "Activity transition sequence is invalid"));
    }
    if let Some(cursor) = ledger.automatic_park_cursor {
        // `usize` already excludes negatives; the TS check is for a non-integer.
        let _ = cursor;
    }
    Ok(())
}

// --- durable read / mutate ----------------------------------------------

/// Read the ledger.
///
/// # Errors
/// `Absent{store:"activity"}` when the row was never written (#105), and
/// `Corrupt{store:"activity"}` when the body does not decode, and
/// `StoreFailure{store:"activity"}` when it decodes and then does not validate
/// — the two are distinct and the cause of each is now printed. (This doc
/// previously said absence was reported as corruption; the code has returned
/// `Absent` since #105 and the sentence had outlived it.)
///
/// No organization-wide counter can stale an otherwise valid ledger; all
/// remaining validation failures are `StoreFailure`.
/// Legacy blob keys tolerated on READ, each dropped with a recorded disposition.
/// This is the item-D read-tolerance allowlist (Fable, binding): a key that is
/// neither in the row model NOR on this list FAILS the read loudly (mapped to
/// `Corrupt` by the callers), so a corruption or a typo can never slip through
/// as silently ignored. Blanket absorption is forbidden. This list DIES with the
/// blobs at the N9 cutover.
const LEGACY_READ_ALLOWLIST: &[&str] = &[
    // #337: the pre-`forced`-park recycle/backoff field, removed from the model
    // when the Rust reconcile aligned to the TS terminal `forced` park. A legacy
    // blob still carries it (at `people.<id>.automaticParkRetryAfter`); it is
    // dropped on read.
    "automaticParkRetryAfter",
    // #751/P9: the two persisted head-in-parent columns, deleted from the model
    // with the backend rule that wrote them.
    //
    // They MUST be listed here, and the reason is a real upgrade defect that
    // this list exists to prevent. Every activity document written before that
    // deletion carries `lastPaneDepartmentId` on every person and
    // `fromPaneDepartmentId` on every transition. `parse_ledger_tolerating_legacy`
    // fails the read on any unmodeled key that is not allowlisted, and `read`
    // maps that to `Corrupt{store:"activity"}` — so without these two rows, the
    // first chiefd carrying the deletion would have refused to read the activity
    // ledger of EVERY existing company, permanently and fleet-wide, on upgrade.
    //
    // It surfaced as two failing tests against a captured live document, which
    // is exactly what that capture is for: the fixture is a real company's
    // bytes, and it caught a break that no hand-written fixture would have,
    // because a hand-written one would have been updated alongside the model.
    // Dropped on read; nothing writes them again.
    "lastPaneDepartmentId",
    "fromPaneDepartmentId",
];

/// Deserialize a legacy `activity` blob, tolerating ONLY the keys on
/// [`LEGACY_READ_ALLOWLIST`] (dropped) and FAILING on any OTHER unknown key.
/// Returns `None` on a JSON/shape error OR an unmodeled key — callers map `None`
/// to `Corrupt`, exactly as a parse failure. Lenient serde drops both the
/// allowlisted and the unknown keys, so the unknown set is re-derived by diffing
/// the incoming JSON against the re-serialized parse and subtracting the
/// allowlist (leaf-name granularity).
fn parse_ledger_tolerating_legacy(body: &str) -> Option<ActivityLedger> {
    let incoming: serde_json::Value = serde_json::from_str(body).ok()?;
    let ledger: ActivityLedger = serde_json::from_value(incoming.clone()).ok()?;
    let modeled = serde_json::to_value(&ledger).ok()?;
    let mut unknown_leaves = Vec::new();
    collect_unknown_leaves(&incoming, &modeled, &mut unknown_leaves);
    if unknown_leaves.iter().any(|leaf| !LEGACY_READ_ALLOWLIST.contains(&leaf.as_str())) {
        return None; // an unmodeled key OUTSIDE the allowlist: FAIL the read loudly.
    }
    Some(ledger)
}

/// Collect the LEAF key names present in `incoming` but absent from `modeled`
/// (the re-serialized parse), recursing objects/arrays. Leaf-name granularity is
/// what the read allowlist matches on.
fn collect_unknown_leaves(
    incoming: &serde_json::Value,
    modeled: &serde_json::Value,
    out: &mut Vec<String>,
) {
    use serde_json::Value;
    match (incoming, modeled) {
        (Value::Object(i), Value::Object(m)) => {
            for (key, iv) in i {
                match m.get(key) {
                    None => out.push(key.clone()),
                    Some(mv) => collect_unknown_leaves(iv, mv, out),
                }
            }
        }
        (Value::Array(i), Value::Array(m)) => {
            for (idx, iv) in i.iter().enumerate() {
                if let Some(mv) = m.get(idx) {
                    collect_unknown_leaves(iv, mv, out);
                }
            }
        }
        _ => {}
    }
}

/// Reconstruct and validate the activity ledger stored in the durable rows.
pub fn read(
    ledgers: &Ledgers,
    manifest: &OrganizationManifest,
) -> Result<ActivityLedger, ChiefdError> {
    let Some(body) = ledgers.document_body(ActivityStore::NAME) else {
        // #105: the document has never been written — absent, not damaged.
        // Reporting `Corrupt` here sent operators hunting for bytes that were
        // never there, and made a fresh company refuse every duty while the
        // daemon exited reporting success. Callers decide what absence means;
        // it must never silently become "empty".
        return Err(ChiefdError::Absent { store: ActivityStore::NAME });
    };
    // `.filter(validate.is_ok())` used to collapse two very different failures
    // into one causeless `Corrupt`: a body that did not PARSE, and a body that
    // parsed and then failed an INVARIANT. The second case has a `Refusal` in
    // hand naming the exact broken rule ("Activity person 'x' has an invalid
    // active transition") and `filter` threw it away — which is why
    // `corrupt store: activity` has been unexplainable at the one place an
    // operator reads it. Same variant returned; the reason now survives.
    let mut ledger = parse_ledger_tolerating_legacy(body)
        .ok_or_else(|| corrupt_store(ActivityStore::NAME, "the activity body did not parse"))?;
    // #1031: reconcile the READ copy too. Repairing only on the write paths left
    // every reader — the launch route, the health pass, the supervision cycle —
    // answering `corrupt store: activity` until some mutation happened to run,
    // which is exactly how one removed department took a live company down.
    //
    // Both halves of the same drift, and ONLY that drift: a removed department
    // strands person placements AND the transitions that referenced it, and a
    // reader that survived the first only to be taken down by the second is no
    // better off. Every other invariant failure still corrupts — running the
    // whole reconcile here would have silently repaired real damage, which is
    // what `read_still_corrupts_on_non_counter_validation_failure` exists to
    // forbid. It publishes nothing; the durable rows converge on the next
    // mutation.
    repair_dangling_departments(&mut ledger, manifest, &iso_millis(ledgers.now().0));
    validate(&ledger, manifest).map_err(|refusal| store_failure(ActivityStore::NAME, &refusal))?;
    Ok(ledger)
}

/// Seed the ledger for a freshly created company.
///
/// Seeding is the only path that constructs an initial ledger.
///
/// # Errors
/// [`INVALID_INPUT`] when the seeded ledger does not validate against the
/// manifest it was seeded from — which would mean the manifest is broken.
pub fn seed(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
) -> Result<ActivityLedger, ChiefdError> {
    let at = iso_millis(ledgers.now().0);
    let ledger = ActivityLedger::initial(manifest, &at);
    validate(&ledger, manifest)?;
    put(ledgers, &ledger)?;
    Ok(ledger)
}

/// Run one mutation against the ledger.
///
/// The ported publish rule: refuse an absent document, reconcile the roster
/// against the manifest, run `f`, then publish when anything changed. A
/// refusal from `f` publishes nothing — the draft is dropped.
///
/// # Errors
/// `Corrupt` when the stored body does not decode; `StoreFailure` when it
/// decodes and the reconciled draft then breaks an invariant the repair pass
/// cannot fix; whatever `f` refuses; [`INVALID_INPUT`] when the
/// mutation produced a ledger that does not validate.
pub fn mutate<T>(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    supervision: &SupervisionLedger,
    f: impl FnOnce(&mut ActivityLedger, &ActivityContext<'_>, &str) -> Result<T, ChiefdError>,
) -> Result<T, ChiefdError> {
    let at = iso_millis(ledgers.now().0);
    // Absence is corruption, never a default: [`seed`] is the only constructor
    // of an initial ledger, so a company without a document past creation has
    // LOST it and a fabricated empty ledger would bury that loss.
    let current = read_for_mutation(ledgers)?;

    let mut draft = current.clone();
    let reconciled = reconcile_people(&mut draft, manifest, &at);
    // #1031: validate AFTER the repair pass, never before it, so a ledger the
    // repair CAN fix gets fixed instead of refused for good.
    //
    // And it is a `StoreFailure`, exactly like the same check in [`read`] and in
    // [`reconcile_structural`]. This was the LAST site in the tree that still
    // answered `corrupt store: activity` for a body that decoded perfectly and
    // then broke an invariant, and it survived by a merge accident rather than a
    // decision: the reclassification pass (`18154c529`) rewrote this validate
    // where it then lived, inside `read_for_mutation`, and the parallel repair
    // branch (`830e9d7d1`) MOVED it out to here in the same batch, carrying the
    // pre-split label with it. `Corrupt` claims the stored bytes are damaged and
    // sends an operator to inspect a database that is intact; a violated
    // invariant is not evidence of damage. `/v1/org/runtime/launch` reaches this
    // through `project_activity_fence`, which is why the launch route was the
    // one place the misleading word was ever read (#1031).
    validate(&draft, manifest).map_err(|refusal| store_failure(ActivityStore::NAME, &refusal))?;
    let ctx = ActivityContext { manifest, supervision, now: ledgers.now().0 };
    let result = f(&mut draft, &ctx, &at)?;
    let changed = reconciled || draft != current;
    if changed {
        draft.updated_at = at;
        validate(&draft, manifest)?;
        put(ledgers, &draft)?;
    }
    Ok(result)
}

/// Apply the #29 pointer sweep's compare-and-clear under the transition writer
/// lock (design Q2).
///
/// Each planned [`ClearPointerAction`] (computed earlier by
/// [`compute_pointer_sweep`](crate::runtime::pointer_sweep::compute_pointer_sweep)
/// from a read snapshot) is re-verified against the *current* ledger, which may
/// have advanced since: the pointer must still equal the planned transition and
/// the status must still be the planned terminal category. A miss drops the
/// action silently and the next pass re-plans. Only
/// the dangling pointer is cleared; the terminal transition record stays in the
/// ledger as history.
///
/// Runs through [`mutate`], so it shares the exact lock, roster reconcile,
/// publication and validation every other transition mutation takes. Returns
/// the actions actually cleared, in input order.
///
/// Note the live reachability of each rule (see [`validate`]): a pointer at a
/// `Cancelled` transition is already rejected by `validate`, so a *readable*
/// ledger never yields a cancelled-clear action live — that rule is exercised
/// only at cold start.
///
/// # Errors
/// `Corrupt`/`StoreFailure`/`INVALID_INPUT` from [`mutate`].
pub fn apply_pointer_clears(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    supervision: &SupervisionLedger,
    actions: &[ClearPointerAction],
) -> Result<Vec<ClearPointerAction>, ChiefdError> {
    mutate(ledgers, manifest, supervision, |draft, _ctx, at| {
        let mut cleared = Vec::new();
        for action in actions {
            if !reverify_clear(action, &pointer_recheck(draft, action)) {
                continue;
            }
            if let Some(state) = draft.people.get_mut(&action.person_id) {
                state.active_transition_id = None;
                state.updated_at = at.to_owned();
                cleared.push(action.clone());
            }
        }
        Ok(cleared)
    })
}

/// Build the apply-time re-verify view of one planned clear from the current
/// draft.
fn pointer_recheck(draft: &ActivityLedger, action: &ClearPointerAction) -> ClearRecheck {
    let transition = draft.transitions.get(&action.transition_id);
    ClearRecheck {
        active_transition_id: draft
            .people
            .get(&action.person_id)
            .and_then(|state| state.active_transition_id.clone()),
        status: transition.map(|record| sweep_status(record.status)),
    }
}

const fn sweep_status(status: TransitionStatus) -> SweepStatus {
    match status {
        TransitionStatus::AwaitingHandoff => SweepStatus::AwaitingHandoff,
        TransitionStatus::Overdue => SweepStatus::Overdue,
        TransitionStatus::Ready => SweepStatus::Ready,
        TransitionStatus::Applied => SweepStatus::Applied,
        TransitionStatus::Cancelled => SweepStatus::Cancelled,
        TransitionStatus::Forced => SweepStatus::Forced,
    }
}

/// Read a ledger for a mutation. Structural reconciliation happens in the
/// mutation itself; no organization-wide version needs a repair path.
fn read_for_mutation(ledgers: &Ledgers) -> Result<ActivityLedger, ChiefdError> {
    let Some(body) = ledgers.document_body(ActivityStore::NAME) else {
        // #105: the document has never been written — absent, not damaged.
        // Reporting `Corrupt` here sent operators hunting for bytes that were
        // never there, and made a fresh company refuse every duty while the
        // daemon exited reporting success. Callers decide what absence means;
        // it must never silently become "empty".
        return Err(ChiefdError::Absent { store: ActivityStore::NAME });
    };
    // #1031: this deliberately does NOT validate. Validating here put
    // `reconcile_people` behind the very check it exists to satisfy, so a ledger
    // the repair pass could have healed was refused forever instead. `mutate`
    // validates the RECONCILED draft, and nothing is ever published unvalidated.
    let parsed = parse_ledger_tolerating_legacy(body)
        .ok_or_else(|| corrupt_store(ActivityStore::NAME, "the activity body did not parse"))?;
    Ok(parsed)
}

/// Repair activity immediately after an authoritative structural manifest
/// mutation.
///
/// Ordinary reads and lifecycle mutations deliberately validate before they
/// act: a caller must never make a malformed activity document look healthy by
/// performing an unrelated operation. A just-committed person or department
/// removal is the one ordered exception. Its formerly-valid activity aggregate
/// necessarily references the shape that was authoritative one statement ago,
/// and [`reconcile_people`] is the sole typed migration from that old shape to
/// the committed manifest.
///
/// This is intentionally not a general corruption scrubber. The stored body
/// must decode with the normal legacy allowlist; absence and malformed or
/// unmodeled bytes stay fail-closed. The reconciled candidate must then pass
/// full validation before it is published. On any failure this function writes
/// nothing, so the actor which calls it can abort its enclosing transaction.
///
/// Returns `true` only when the activity aggregate was changed and published.
/// The actor owns the surrounding single-writer transaction and event cursor;
/// this store function never accepts a caller-authored ledger or retry token.
///
/// # Errors
/// [`ChiefdError::Absent`] for a never-seeded ledger; [`ChiefdError::Corrupt`]
/// for bytes that do not decode; [`ChiefdError::StoreFailure`] for a candidate
/// that decodes and remains invalid after the allowed structural
/// reconciliation.
pub fn reconcile_structural(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
) -> Result<bool, ChiefdError> {
    let Some(body) = ledgers.document_body(ActivityStore::NAME) else {
        return Err(ChiefdError::Absent { store: ActivityStore::NAME });
    };
    let current = parse_ledger_tolerating_legacy(body)
        .ok_or_else(|| corrupt_store(ActivityStore::NAME, "the activity body did not parse"))?;
    let at = iso_millis(ledgers.now().0);
    let mut draft = current.clone();
    reconcile_people(&mut draft, manifest, &at);
    if draft == current {
        validate(&draft, manifest).map_err(|e| store_failure(ActivityStore::NAME, e))?;
        return Ok(false);
    }
    draft.updated_at = at;
    validate(&draft, manifest).map_err(|e| store_failure(ActivityStore::NAME, e))?;
    put(ledgers, &draft)?;
    Ok(true)
}

/// Remove the ledger, returning whether a row was present.
///
/// # Errors
/// `Corrupt{store:"activity"}` over unreadable bytes: the ledger holds the only
/// durable record that a person owes a handoff, so discarding what chiefd could
/// not read would discard the fence.
/// Cold-start recovery: drop any `active_transition_id` that points at a
/// TERMINAL transition.
///
/// An unclean shutdown can leave a person pointing at a transition that has
/// already been applied, cancelled or forced. Nothing will ever resolve that
/// pointer, so every subsequent start/staffing request for that person is
/// refused as "already transitioning" — the deadlock this clears. Only the
/// POINTER is dropped; the transition rows themselves are history and stay.
///
/// Returns whether anything changed.
///
/// # Errors
/// `Corrupt`/`StoreFailure{store:"activity"}` when the ledger cannot be read.
pub fn clear_orphaned_terminal_transitions(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
) -> Result<bool, ChiefdError> {
    let mut ledger = read(ledgers, manifest)?;
    let orphaned: Vec<String> = ledger
        .people
        .iter()
        .filter_map(|(person_id, state)| {
            let transition_id = state.active_transition_id.as_deref()?;
            let terminal = ledger
                .transitions
                .get(transition_id)
                .is_some_and(|transition| transition.status.is_terminal());
            terminal.then(|| person_id.clone())
        })
        .collect();
    if orphaned.is_empty() {
        return Ok(false);
    }
    for person_id in &orphaned {
        if let Some(state) = ledger.people.get_mut(person_id) {
            state.active_transition_id = None;
        }
    }
    ledger.updated_at = iso_millis(ledgers.now().0);
    put(ledgers, &ledger)?;
    Ok(true)
}

/// Remove the ledger, returning whether a row was present.
///
/// The parse happens first, and only when there is something to parse: this
/// ledger is the sole authority for the handoffs D7 fences, so a body chiefd
/// cannot read is a refusal, never a silent erase. Fail-closed exactly like
/// [`super::organization::clear`] and [`super::supervision::clear`], and
/// pinned by `chiefd-core/tests/polarity_matrix.rs`.
///
/// # Errors
/// `Corrupt{store:"activity"}` when the stored bytes are unreadable.
pub fn clear(ledgers: &mut Ledgers, manifest: &OrganizationManifest) -> Result<bool, ChiefdError> {
    if ledgers.document_body(ActivityStore::NAME).is_some() {
        read(ledgers, manifest)?;
    }
    Ok(ledgers.remove_document(ActivityStore::NAME))
}

// --- gh#499: the single-authority seam for the docstore router --------------
//
// Until gh#499 this store had NO router special case, so `/v1/docs/*` served
// and wrote the shared `org_documents` row while chiefd's duties wrote the
// native ledger below. Nothing reconciled the two after boot. Measured live on
// `tribes-capital` on 2026-07-24, thirteen seconds apart: native rev 4357 =
// 719,084 bytes against `org_documents` gen 5749 = 1,191,421 bytes, both
// current — a 66% divergence in the store that plans kills. The three
// accessors below are what let the router route every activity verb at THIS
// company onto the one authority, exactly as `supervision` (#372/#440) and
// `organization` (#442) already are.

/// gh#499: whether `store` names THIS store's own documents key.
///
/// Exists for the same reason `supervision::is_supervision_store` does: the
/// docstore router needs to recognize "the activity store" without naming the
/// literal key or this module's store type, both of which
/// `chiefd-core/tests/fence_containment.rs` fences shut for every caller
/// outside this file. A `bool` answers that one question and hands the caller
/// nothing that could read or write a row bypassing the typed accessors.
#[must_use]
pub fn is_activity_store(store: &str) -> bool {
    store == ActivityStore::NAME
}

/// gh#499: insert-if-absent for the activity ledger — the sibling of
/// `supervision::create_if_absent` and `organization::create_if_absent`.
///
/// The activity document is self-contained in the `documents` table (unlike
/// supervision, which projects assignments and effects into relational rows),
/// so an insert is the body and nothing else — there is no relational half to
/// seed here.
///
/// Returns whether this call created it. Presence check and insert share the
/// caller's one transaction.
///
/// # Errors
/// `Corrupt{store:"activity"}` when the offered body does not decode — an
/// insert that stored bytes no reader can parse would turn a create into a
/// silent corruption of the store that plans kills.
pub fn create_if_absent(ledgers: &mut Ledgers, body: &str) -> Result<bool, ChiefdError> {
    if ledgers.document_body(ActivityStore::NAME).is_some() {
        return Ok(false);
    }
    let ledger = parse_ledger_tolerating_legacy(body)
        .ok_or_else(|| corrupt_store(ActivityStore::NAME, "the offered body did not parse"))?;
    put(ledgers, &ledger)?;
    Ok(true)
}

/// gh#499: adopt a launcher-authored activity document into the native ledger.
///
/// This is what makes the native ledger the ONE authority rather than one of
/// two: the launcher's CAS write to `/v1/docs/write` lands HERE
/// instead of in the shared `org_documents` row, so there is a single writer of
/// activity state and no second copy to diverge from.
///
/// The incoming body is reconciled against the normalized manifest structure,
/// then published as the current durable activity state. Organization structure
/// has no shared counter to compare or repair.
///
/// # Errors
/// `Corrupt{store:"activity"}` when the body does not decode, or when the native
/// ledger is absent (absence is corruption — [`seed`] is the only constructor);
/// [`INVALID_INPUT`] when the ingested result does not validate.
pub fn ingest_external_document(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    body: &str,
) -> Result<(), ChiefdError> {
    let at = iso_millis(ledgers.now().0);
    let mut incoming = parse_ledger_tolerating_legacy(body)
        .ok_or_else(|| corrupt_store(ActivityStore::NAME, "the offered body did not parse"))?;
    // The native ledger is the authority; absence is corruption, so
    // this read is also the refusal that stops an ingest from fabricating a
    // ledger creation never seeded.
    read_for_mutation(ledgers)?;
    reconcile_people(&mut incoming, manifest, &at);
    incoming.updated_at = at;
    validate(&incoming, manifest)?;
    put(ledgers, &incoming)?;
    Ok(())
}

fn put(ledgers: &mut Ledgers, ledger: &ActivityLedger) -> Result<(), Refusal> {
    let encoded = serde_json::to_string(ledger).map_err(|error| {
        Refusal::new(LEDGER_UNSERIALIZABLE, format!("cannot encode the activity ledger: {error}"))
    })?;
    ledgers.put_document(ActivityStore::NAME, encoded);
    Ok(())
}

/// The manifest and supervision facts every activity mutation reads.
///
/// A mutation closure receives the activity draft, not the whole [`Ledgers`],
/// so anything outside the draft it needs to consult arrives here.
///
/// TOMBSTONE (#751-P4): this used to carry a `reflections: &BTreeSet<String>`
/// set and a `has_reflection` accessor, so the reconcile could demand a durable
/// reflection row for a `ready`/`applied` transition. Deleted with the payload;
/// see [`reconcile`], where the rule it fed was the fail-closed refusal
/// `handoff-not-durable`.
#[derive(Debug, Clone, Copy)]
pub struct ActivityContext<'a> {
    manifest: &'a OrganizationManifest,
    supervision: &'a SupervisionLedger,
    now: i64,
}

impl<'a> ActivityContext<'a> {
    /// The manifest.
    #[must_use]
    pub fn manifest(&self) -> &'a OrganizationManifest {
        self.manifest
    }

    /// The supervision ledger.
    #[must_use]
    pub fn supervision(&self) -> &'a SupervisionLedger {
        self.supervision
    }

    /// Epoch millis of the commit being assembled.
    #[must_use]
    pub fn now(&self) -> i64 {
        self.now
    }
}

/// Reconcile the roster against the manifest. Returns whether anything changed.
///
/// Port of `reconcileActivityPeople` (`org-activity-state.ts:174-218`).
/// Re-point any prior placement that names a department the manifest no longer
/// has, returning whether anything was repaired.
///
/// #1031. Removing a department strands `last_department_id` on a person the
/// manifest KEEPS, and `validate`
/// then rejects the entire ledger — so one removed department made a company's
/// activity unreadable, and every read path (launch, the health pass, the
/// supervision cycle) answered `corrupt store: activity` forever.
///
/// Note carefully what is NOT repaired. A prior placement that merely DIFFERS
/// from the manifest is legal and load-bearing: that difference is exactly what
/// raises a structural transfer (see `structural_transition_for`), so erasing it
/// would erase the transfer it exists to raise. Only a DANGLING reference — one
/// the manifest cannot name at all — is rewritten, and it is rewritten to the
/// value the settle path already advances to.
fn repair_dangling_departments(
    ledger: &mut ActivityLedger,
    manifest: &OrganizationManifest,
    now: &str,
) -> bool {
    let mut repaired_any = false;
    // A transition that recorded a department the manifest has since dropped.
    // `reconcile_people` already drops exactly these on the write paths; without
    // the same drop here a reader is taken down by the residue of a removal it
    // can do nothing about. Person membership is deliberately NOT a condition:
    // an unknown person is real damage and must still corrupt.
    let doomed: Vec<String> = ledger
        .transition_order
        .iter()
        .filter(|transition_id| {
            ledger.transitions.get(*transition_id).is_some_and(|transition| {
                !manifest.departments.contains_key(&transition.placement_department_id)
                    || transition
                        .to_department_id
                        .as_ref()
                        .is_some_and(|unit| !manifest.departments.contains_key(unit))
            })
        })
        .cloned()
        .collect();
    if !doomed.is_empty() {
        let dropped: BTreeSet<&String> = doomed.iter().collect();
        ledger.transition_order.retain(|id| !dropped.contains(id));
        for transition_id in &doomed {
            ledger.transitions.remove(transition_id);
        }
        for state in ledger.people.values_mut() {
            if state.active_transition_id.as_ref().is_some_and(|id| doomed.contains(id)) {
                state.active_transition_id = None;
            }
        }
        repaired_any = true;
    }
    for person_id in &manifest.people_order {
        let Some(person) = manifest.people.get(person_id) else { continue };
        let Some(state) = ledger.people.get_mut(person_id) else { continue };
        // ONE placement to repair. This was a pair of identical-shaped blocks,
        // home then assigned, and with one column a second block would be the
        // same rewrite applied twice to the same field.
        if !manifest.departments.contains_key(&state.last_department_id) {
            state.last_department_id.clone_from(&person.department_id);
            state.updated_at = now.to_string();
            repaired_any = true;
        }
    }
    repaired_any
}

fn reconcile_people(
    ledger: &mut ActivityLedger,
    manifest: &OrganizationManifest,
    now: &str,
) -> bool {
    let mut changed = false;
    if ledger.person_order != manifest.people_order {
        ledger.person_order.clone_from(&manifest.people_order);
        changed = true;
    }
    for person_id in &manifest.people_order {
        if !ledger.people.contains_key(person_id) {
            if let Some(state) = seed_person_state(manifest, person_id, now) {
                ledger.people.insert(person_id.clone(), state);
                changed = true;
            }
        }
    }
    let known: BTreeSet<&String> = manifest.people_order.iter().collect();
    let departed: Vec<String> =
        ledger.people.keys().filter(|id| !known.contains(id)).cloned().collect();
    for person_id in departed {
        ledger.people.remove(&person_id);
        changed = true;
    }

    if repair_dangling_departments(ledger, manifest, now) {
        changed = true;
    }

    let retained: Vec<String> = ledger
        .transition_order
        .iter()
        .filter(|transition_id| {
            ledger.transitions.get(*transition_id).is_some_and(|transition| {
                known.contains(&transition.person_id)
                    && manifest.departments.contains_key(&transition.placement_department_id)
                    && transition
                        .to_department_id
                        .as_ref()
                        .is_none_or(|unit| manifest.departments.contains_key(unit))
            })
        })
        .cloned()
        .collect();
    if retained.len() != ledger.transition_order.len() {
        let keep: BTreeSet<&String> = retained.iter().collect();
        let dropped: Vec<String> =
            ledger.transition_order.iter().filter(|id| !keep.contains(id)).cloned().collect();
        for transition_id in dropped {
            ledger.transitions.remove(&transition_id);
        }
        ledger.transition_order = retained.clone();
        let keep: BTreeSet<String> = retained.into_iter().collect();
        for state in ledger.people.values_mut() {
            if state.active_transition_id.as_ref().is_some_and(|id| !keep.contains(id)) {
                state.active_transition_id = None;
            }
        }
        changed = true;
    }
    // #312-follow-up: bound settled transition history so the hot-read `activity`
    // blob (footer polls it) has a hard size ceiling. Mirrors the TS twin
    // `reconcileActivityPeople` and the supervision terminal cap (#329): keep
    // every live transition and every one a person's `active_transition_id`
    // still points at (even an inheritable `Applied` park); of the remaining
    // terminal history, keep only the newest `ACTIVITY_TERMINAL_TRANSITION_LIMIT`
    // (transition_order is chronological, so the oldest settled records drop
    // first).
    let referenced: BTreeSet<String> =
        ledger.people.values().filter_map(|state| state.active_transition_id.clone()).collect();
    let droppable: Vec<String> = ledger
        .transition_order
        .iter()
        .filter(|transition_id| {
            !referenced.contains(*transition_id)
                && ledger
                    .transitions
                    .get(*transition_id)
                    .is_some_and(|transition| transition.status.is_terminal())
        })
        .cloned()
        .collect();
    if droppable.len() > ACTIVITY_TERMINAL_TRANSITION_LIMIT {
        let drop_count = droppable.len() - ACTIVITY_TERMINAL_TRANSITION_LIMIT;
        let doomed: BTreeSet<String> = droppable.into_iter().take(drop_count).collect();
        for transition_id in &doomed {
            ledger.transitions.remove(transition_id);
        }
        ledger.transition_order.retain(|id| !doomed.contains(id));
        changed = true;
    }
    if sweep_expired_transitions(ledger, now) {
        changed = true;
    }
    changed
}

/// atomic-reorg, TS parity (`sweepExpiredTransitions`,
/// org-activity.ts): expire stale in-flight transitions every reconcile pass.
///
/// A non-terminal transition (`awaiting_handoff`/`overdue`/`ready`) whose
/// handoff deadline plus one full extra grace window has passed is dead
/// weight: nothing will ever consume it, and while it occupies
/// `active_transition_id` it refuses every later lifecycle intent — the live
/// cobalt wedge. Cancel it truthfully.
/// Routine unowned idle auto-parks are excluded: #337's forced-park machinery
/// owns their overdue path and records the distinct `forced` terminal status.
fn sweep_expired_transitions(ledger: &mut ActivityLedger, now: &str) -> bool {
    let Some(now_ms) = parse_iso_millis(now) else { return false };
    let mut expired: Vec<String> = Vec::new();
    for person_id in &ledger.person_order {
        let Some(state) = ledger.people.get(person_id) else { continue };
        let Some(transition_id) = state.active_transition_id.as_deref() else { continue };
        let Some(transition) = ledger.transitions.get(transition_id) else { continue };
        if !matches!(
            transition.status,
            TransitionStatus::AwaitingHandoff | TransitionStatus::Overdue | TransitionStatus::Ready
        ) {
            continue;
        }
        if transition.action == TransitionAction::Park
            && transition.reason == IDLE_AUTO_PARK_REASON
            && transition.intent_id.is_none()
        {
            continue;
        }
        let Some(deadline_ms) = parse_iso_millis(&transition.handoff_deadline_at) else { continue };
        if now_ms <= deadline_ms + HANDOFF_GRACE_MS {
            continue;
        }
        expired.push(transition_id.to_string());
    }
    let changed = !expired.is_empty();
    for transition_id in expired {
        cancel_transition(ledger, &transition_id, now);
    }
    changed
}

// --- transitions ---------------------------------------------------------

/// What [`begin_transition`] needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeginTransitionInput {
    /// Whose transition.
    pub person_id: String,
    /// What it prepares for.
    pub action: TransitionAction,
    /// Why (bounded to 500 characters).
    pub reason: String,
    /// Target unit, for a transfer.
    pub to_department_id: Option<String>,
    /// Stable lifecycle command identity, when a command owns this transition.
    pub intent_id: Option<String>,
}

/// A deliberate decline from an operation (as opposed to a validation rule).
fn refused(code: &'static str, message: impl Into<String>) -> ChiefdError {
    ChiefdError::refused(code, message)
}

fn required(value: &str, label: &str, maximum: usize) -> Result<String, ChiefdError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(refused(INVALID_INPUT, format!("{label} is required")));
    }
    if trimmed.chars().count() > maximum {
        return Err(refused(
            INVALID_INPUT,
            format!("{label} must be at most {maximum} characters"),
        ));
    }
    Ok(trimmed.to_string())
}

// TOMBSTONE (#751-P4): `bounded_items` bounded the reflection payload's
// `artifacts` / `openCommitments` lists. It had no other caller and died with
// them. `required` survives — the transition `reason` and `intentId` still use
// it.

/// The terms of one transition, as a value.
///
/// A struct rather than eight positional parameters, and not only for clippy's
/// sake: `to_department_id` and `intent_id` are both `Option<&str>` and adjacent,
/// so a transposed pair would compile and would silently produce an
/// intent-bound transition targeting the wrong unit — the exact class of bug
/// the supersession rules below are made of.
#[derive(Debug, Clone, Copy)]
struct TransitionSpec<'a> {
    person_id: &'a str,
    action: TransitionAction,
    reason: &'a str,
    to_department_id: Option<&'a str>,
    intent_id: Option<&'a str>,
}

fn new_transition(
    ledger: &mut ActivityLedger,
    spec: TransitionSpec<'_>,
    at: &str,
) -> Result<GracefulTransition, ChiefdError> {
    let TransitionSpec { person_id, action, reason, to_department_id, intent_id } = spec;
    // Exactly `is_routine_idle_park`'s three fields, evaluated before the
    // record is built because they decide the status it is born with.
    let routine_idle_park =
        action == TransitionAction::Park && intent_id.is_none() && reason == IDLE_AUTO_PARK_REASON;
    let sequence = ledger.next_transition_sequence;
    ledger.next_transition_sequence = sequence.saturating_add(1);
    let id = format!("transition:{sequence}:{person_id}:{}", action.as_str());
    let requested = parse_iso_millis(at)
        .ok_or_else(|| refused(INVALID_INPUT, "commit timestamp is not ISO-8601"))?;
    let state = ledger.people.get(person_id).ok_or_else(|| {
        refused(UNKNOWN_PERSON, format!("Unknown organization person '{person_id}'"))
    })?;
    let transition = GracefulTransition {
        id: id.clone(),
        person_id: person_id.to_string(),
        action,
        reason: reason.to_string(),
        intent_id: intent_id.map(ToString::to_string),
        placement_department_id: state.last_department_id.clone(),
        to_department_id: to_department_id.map(ToString::to_string),
        status: if routine_idle_park {
            TransitionStatus::Forced
        } else {
            TransitionStatus::AwaitingHandoff
        },
        requested_at: at.to_string(),
        // A ROUTINE IDLE PARK IS BORN TERMINAL. There is no window between
        // admitting it and the pane going away, because there is nothing that
        // could arrive in one: the only thing that ends a park early is
        // `release`, whose single production caller is the staffing lifecycle
        // verb (which releases in the same request), and the Pi extension has
        // no release verb at all. The deadline is the admission instant, the
        // status is already terminal, and `is_pending()` is false — which is
        // what stops the reconcile adding the `HandoffRequired` reason that
        // used to keep the pane alive through the deleted grace.
        //
        // The three fields tested here are exactly the ones
        // `is_routine_idle_park` reads; they are read here because the status
        // has to be decided at mint time, not re-derived later.
        handoff_deadline_at: if routine_idle_park {
            at.to_string()
        } else {
            iso_millis(requested + HANDOFF_GRACE_MS)
        },
        applied_at: None,
        cancelled_at: None,
        forced_at: if routine_idle_park { Some(at.to_string()) } else { None },
        abandoned_at: None,
    };
    ledger.transitions.insert(id.clone(), transition.clone());
    ledger.transition_order.push(id.clone());
    let state = ledger.people.get_mut(person_id).ok_or_else(|| {
        refused(UNKNOWN_PERSON, format!("Unknown organization person '{person_id}'"))
    })?;
    state.active_transition_id = Some(id);
    state.updated_at = at.to_string();
    Ok(transition)
}

/// Find or create the transition matching these terms.
///
/// Port of `ensureMatchingTransition` (`org-activity.ts:429-475`). The
/// intent-supersession rules in it are subtle and each one is a real bug that
/// was fixed: an unowned idle park may be replaced by an explicit lifecycle
/// intent; an applied *unowned* park is terminal history and starts fresh; an
/// applied intent-bound handoff stays available for its own structural retry.
///
/// One deliberate hardening beyond the TS: the TERMINAL check runs BEFORE the
/// action/target refusal, so no terminal transition can pin a person against
/// different terms (the TS evaluates it after, sharing the wedge the live
/// 2026-07-22 incident proved out — see BUG-7 in
/// `runtime/takeover-bug-log.md`). It asks `is_terminal()`, never one named
/// status: enumerating `Applied` alone let the identical wedge come back live
/// on 2026-08-13 through `Forced`. See the comment on `start_fresh` below.
fn ensure_matching_transition(
    ledger: &mut ActivityLedger,
    spec: TransitionSpec<'_>,
    at: &str,
) -> Result<GracefulTransition, ChiefdError> {
    let TransitionSpec { person_id, action, to_department_id, intent_id, .. } = spec;
    let mut existing = ledger.active_transition_id_of(person_id);

    // Only park may supersede park, and only a previously unowned transition.
    if let Some(id) = existing.clone() {
        let current = &ledger.transitions[&id];
        if current.action == TransitionAction::Park
            && action == TransitionAction::Park
            && intent_id.is_some()
            && current.intent_id.is_none()
        {
            cancel_transition(ledger, &id, at);
            existing = None;
        }
    }

    // atomic-reorg, TS parity (org-activity.ts): an
    // explicit intent-bound lifecycle request SUPERSEDES any conflicting
    // NON-APPLIED transition instead of refusing forever. An
    // awaiting/overdue/ready transition protects no live work; leaving it
    // authoritative wedged live cobalt (stranded
    // a stale unit-stop marker refused every later stop/transfer
    // "for another lifecycle intent" permanently). Cancellation is truthful;
    // applied transitions keep their semantics below; a non-intent caller
    // still never steals an explicit in-flight transition.
    if let Some(id) = existing.clone() {
        let current = &ledger.transitions[&id];
        if intent_id.is_some()
            && current.status != TransitionStatus::Applied
            && (current.action != action
                || current.to_department_id.as_deref() != to_department_id
                || current.intent_id.as_deref() != intent_id)
        {
            cancel_transition(ledger, &id, at);
            existing = None;
        }
    }

    // A TERMINAL transition is history, not an in-flight fence, so it is
    // evaluated BEFORE the action/target refusal below: a person whose pointer
    // still names a terminal transition must start fresh on any new terms,
    // never hard-refuse.
    //
    // THE PREDICATE ASKS ABOUT TERMINALITY, NEVER ABOUT ONE NAMED STATUS, and
    // it must stay that way. This wedge has now been found live TWICE, and the
    // second time only because the first fix enumerated a status:
    //
    // * 2026-07-22 (tribes-capital): an APPLIED park left as
    //   `activeTransitionId` collided with the pass's structural computation,
    //   the refusal rolled back the whole reconcile commit, and every cycle
    //   wedged. The fix asked `status == Applied`.
    // * 2026-08-13 (a live company): identical wedge, identical refusal
    //   (`transition-conflict: Person '…' already has park transition '…'`),
    //   aborting every reconcile actuation — because seven routine idle
    //   auto-parks sat at `Forced`, which is equally terminal and which that
    //   predicate did not name. `Forced` has NO retirement path at all: it is
    //   never applied, cancelled or abandoned, so the pointer fenced the
    //   person forever.
    //
    // [`TransitionStatus::is_terminal`] admits exactly `Applied`, `Cancelled`
    // and `Forced`, and starting fresh over each is correct:
    // * `Applied` — the structural change already happened; nothing waits on it.
    // * `Cancelled` — superseded or abandoned; nothing will ever consume it.
    // * `Forced` — parked without a release (#337); it is an ENDING, and the
    //   only status of the three that can never be reached any other way.
    // Every non-terminal status (`AwaitingHandoff`/`Overdue`/`Ready`) is a live
    // fence and still falls through to the refusal below, so a caller can never
    // steal an in-flight transition. Adding a status to `is_terminal()` must
    // never again require remembering to add it here too.
    //
    // The one carve-out is unchanged and stays narrow: an APPLIED intent-bound
    // handoff is still available for its OWN structural retry — same action,
    // same target, and the same (or no) incoming intent. It is deliberately
    // limited to `Applied`; a cancelled or forced record is an ending nobody
    // may retry through.
    let start_fresh = match existing.as_deref() {
        None => true,
        Some(id) => {
            let current = &ledger.transitions[id];
            let own_structural_retry = current.status == TransitionStatus::Applied
                && current.action == action
                && current.to_department_id.as_deref() == to_department_id
                && current.intent_id.is_some()
                && (intent_id.is_none() || current.intent_id.as_deref() == intent_id);
            current.status.is_terminal() && !own_structural_retry
        }
    };
    if start_fresh {
        return new_transition(ledger, spec, at);
    }

    if let Some(id) = existing.clone() {
        let current = &ledger.transitions[&id];
        if current.action != action || current.to_department_id.as_deref() != to_department_id {
            return Err(refused(
                TRANSITION_CONFLICT,
                format!(
                    "Person '{person_id}' already has {} transition '{id}'",
                    current.action.as_str()
                ),
            ));
        }
    }

    let id = existing.ok_or_else(|| vanished("the active transition"))?;
    let current = ledger.transitions.get(&id).ok_or_else(|| vanished("the active transition"))?;
    if intent_id.is_some() && current.intent_id.as_deref() != intent_id {
        return Err(refused(
            TRANSITION_CONFLICT,
            format!(
                "Person '{person_id}' already has transition '{id}' for another lifecycle intent"
            ),
        ));
    }
    Ok(current.clone())
}

fn cancel_transition(ledger: &mut ActivityLedger, transition_id: &str, at: &str) {
    let person_id = {
        let Some(transition) = ledger.transitions.get_mut(transition_id) else { return };
        transition.status = TransitionStatus::Cancelled;
        transition.cancelled_at = Some(at.to_string());
        // A cancelled transition was, by definition, not forced. Now that a
        // routine idle park is BORN `Forced`, cancelling one would otherwise
        // leave a record stamped both cancelled and forced, which is two
        // different accounts of the same ending.
        transition.forced_at = None;
        transition.person_id.clone()
    };
    if let Some(state) = ledger.people.get_mut(&person_id) {
        if state.active_transition_id.as_deref() == Some(transition_id) {
            state.active_transition_id = None;
            state.updated_at = at.to_string();
        }
    }
}

/// Open (or find) a bounded handoff before a structural change.
///
/// # Errors
/// [`UNKNOWN_PERSON`], [`INVALID_INPUT`] (missing/duplicate target),
/// [`TRANSITION_CONFLICT`].
pub fn begin_transition(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    supervision: &SupervisionLedger,
    input: &BeginTransitionInput,
) -> Result<GracefulTransition, ChiefdError> {
    mutate(ledgers, manifest, supervision, |draft, ctx, at| {
        let person = ctx.manifest().people.get(&input.person_id).ok_or_else(|| {
            refused(UNKNOWN_PERSON, format!("Unknown organization person '{}'", input.person_id))
        })?;
        if !draft.people.contains_key(&input.person_id) {
            return Err(refused(
                UNKNOWN_PERSON,
                format!("Unknown organization person '{}'", input.person_id),
            ));
        }
        let target = input
            .to_department_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        if input.action.needs_target()
            && !target.as_deref().is_some_and(|unit| ctx.manifest().departments.contains_key(unit))
        {
            return Err(refused(
                INVALID_INPUT,
                format!("{} transition requires a known target department", input.action.as_str()),
            ));
        }
        if !input.action.needs_target() && target.is_some() {
            return Err(refused(
                INVALID_INPUT,
                format!("{} transition cannot have a target department", input.action.as_str()),
            ));
        }
        let state = &draft.people[&input.person_id];
        if let Some(target) = target.as_deref() {
            // TOMBSTONE (#1081): a second conjunct rode on this, exempting a
            // Transfer whose target matched the ASSIGNED unit but not the HOME
            // one. It existed because transferring a loaned person into the unit
            // they were already sitting in was not a no-op — it moved their
            // membership. With one column, a target that matches the placement
            // matches all of it, and the exemption had nothing left to exempt.
            if target == state.last_department_id {
                return Err(refused(
                    INVALID_INPUT,
                    format!("Person '{}' is already assigned to '{target}'", input.person_id),
                ));
            }
        }
        let _ = person;
        let intent_id = input
            .intent_id
            .as_deref()
            .map(|value| required(value, "transition.intentId", 300))
            .transpose()?;
        let reason =
            required(&input.reason, "transition.reason", TRANSITION_MAX_REASON_CHARACTERS)?;
        ensure_matching_transition(
            draft,
            TransitionSpec {
                person_id: &input.person_id,
                action: input.action,
                reason: &reason,
                to_department_id: target.as_deref(),
                intent_id: intent_id.as_deref(),
            },
            at,
        )
    })
}

/// Record, in the same lifecycle call that committed a non-removal placement
/// move, that the move happened.
///
/// # The defect this removes
///
/// Two placement moves in a row — the second returning the person where they
/// started — were refused `invalid-input: Person '<id>' is already assigned to
/// '<home>'`, and a manager doing the obvious thing had no way to read that
/// sentence as anything but wrong — because it was. The first move commits its
/// manifest rows at once (`move_person` plus history, one `apply_and_emit`),
/// but
/// [`begin_transition`]'s placement fence reads
/// [`PersonActivityState::last_department_id`], which is the
/// RECONCILER's observation of that same fact. Between the two the ledger still
/// said "home", so the return looked like a no-op and was refused. A live proof
/// had to wait for the projection; the product does not.
///
/// # Why the fix is here and not at the fence
///
/// Moving the fence onto the manifest alone is not enough and is not the real
/// answer. It clears the first refusal and lands on the second — the first
/// move's transition is still `Ready` rather than `Applied`, so `ensure_matching_transition`
/// refuses `transition-conflict`. Cancel that instead and it gets worse: two
/// structural moves that cancel out leave the projection with NO gap, so
/// [`structural_transition`] returns `None`, the reconcile never applies the
/// return's transition, and the person is pinned active on a dangling `Ready`
/// forever. The refusal is a symptom; the cause is that a mutation chiefd made
/// itself is relayed back to it by a reconciler pass.
///
/// So the observation commits with the move. This is exactly the work
/// [`reconcile`]'s `(Some(structural), Some(id)) if released` arm does for a
/// NON-REMOVAL action, pulled forward to the call that already knows the move
/// landed: mark the released transition applied, drop the person's pointer, and
/// advance the persisted placement to the manifest.
///
/// # What it deliberately does not touch
///
/// * **Removals.** `park` and `offboard` keep going through the reconcile
///   untouched, because for those the same arm also DECIDES: a removal with a
///   live work lease and no forced pause defers, keeping the pane up until the
///   work drains. That decision needs the host's observations and is not this
///   call's to make.
/// * **`last_desired_active`.** Whether the person should be running is the
///   reconcile's answer, gated by the launch-intent fence. Placement is not
///   residency.
/// * **Anything with no released transition.** A `direct_running_transfer` /
///   `direct_stopped_transfer` (the `ApplyDirectly` path, which never opens a
///   transition) is recognized by
///   the reconcile precisely by `transition.is_none()`, and advancing its
///   placement here would erase the edge it uses to retain a live pane.
///
/// Returns whether anything settled. A `false` is not an error: it means the
/// reconcile still owns this person's placement, exactly as before.
///
/// # Errors
/// [`UNKNOWN_PERSON`] when the manifest or the ledger does not have them.
pub fn settle_applied_move(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    supervision: &SupervisionLedger,
    person_id: &str,
) -> Result<bool, ChiefdError> {
    mutate(ledgers, manifest, supervision, |draft, ctx, at| {
        let person = ctx.manifest().people.get(person_id).ok_or_else(|| {
            refused(UNKNOWN_PERSON, format!("Unknown organization person '{person_id}'"))
        })?;
        if !draft.people.contains_key(person_id) {
            return Err(refused(
                UNKNOWN_PERSON,
                format!("Unknown organization person '{person_id}'"),
            ));
        }
        let Some(transition_id) = draft.active_transition_id_of(person_id) else {
            return Ok(false);
        };
        let current = &draft.transitions[&transition_id];
        if current.action.is_removal() || !current.status.is_released() {
            return Ok(false);
        }
        if let Some(record) = draft.transitions.get_mut(&transition_id) {
            if record.status != TransitionStatus::Applied {
                record.status = TransitionStatus::Applied;
                record.applied_at = Some(at.to_string());
            }
        }
        if let Some(state) = draft.people.get_mut(person_id) {
            state.active_transition_id = None;
            state.last_employment_state = person.employment_state;
            state.last_department_id.clone_from(&person.department_id);
            state.last_operational =
                organization_unit_is_active(ctx.manifest(), &person.department_id);
            state.updated_at = at.to_string();
        }
        Ok(true)
    })
}

/// Abandon a bounded handoff that provably cannot be completed.
///
/// A transition only reaches `ready` through that person's own identity-fenced
/// [`release`] from their own live pane. Marked `cancelled`
/// with `abandonedAt`, never `applied`: `applied` records that the owner
/// released it, and nobody did here.
///
/// # Errors
/// [`UNKNOWN_TRANSITION`], [`TRANSITION_RELEASED`], [`TRANSITION_TERMINAL`].
pub fn abandon_transition(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    supervision: &SupervisionLedger,
    transition_id: &str,
    person_id: &str,
    reason: &str,
) -> Result<GracefulTransition, ChiefdError> {
    mutate(ledgers, manifest, supervision, |draft, _ctx, at| {
        let current = draft
            .transitions
            .get(transition_id)
            .filter(|transition| transition.person_id == person_id)
            .ok_or_else(|| {
                refused(
                    UNKNOWN_TRANSITION,
                    format!("Unknown graceful transition '{transition_id}' for '{person_id}'"),
                )
            })?;
        if current.status.is_released() {
            return Err(refused(
                TRANSITION_RELEASED,
                format!(
                    "Graceful transition '{transition_id}' has been released and cannot be abandoned"
                ),
            ));
        }
        if current.status == TransitionStatus::Forced {
            return Err(refused(
                TRANSITION_TERMINAL,
                format!(
                    "Graceful transition '{transition_id}' was already force-completed without a release and cannot be abandoned"
                ),
            ));
        }
        if current.status == TransitionStatus::Cancelled {
            return Err(refused(
                TRANSITION_TERMINAL,
                format!("Graceful transition '{transition_id}' is already cancelled"),
            ));
        }
        let merged: String = format!("{} {reason}", current.reason)
            .trim()
            .chars()
            .take(TRANSITION_MAX_REASON_CHARACTERS)
            .collect();
        let transition = draft
            .transitions
            .get_mut(transition_id)
            .ok_or_else(|| vanished("the transition being abandoned"))?;
        transition.status = TransitionStatus::Cancelled;
        transition.cancelled_at = Some(at.to_string());
        transition.abandoned_at = Some(at.to_string());
        transition.reason = merged;
        let result = transition.clone();
        if let Some(state) = draft.people.get_mut(person_id) {
            if state.active_transition_id.as_deref() == Some(transition_id) {
                state.active_transition_id = None;
            }
        }
        Ok(result)
    })
}

/// What [`release`] needs.
///
/// TOMBSTONE (#751-P4): this was `ReflectInput`, and it carried the reflection
/// payload (`summary` / `learning` / `handoff` / `artifacts` /
/// `open_commitments`) alongside the three identity fields below. The payload
/// is gone; the identity fence is not, and it is the whole point of the type:
/// the caller proves WHICH transition and WHO they are, and neither may be
/// claimed rather than authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInput {
    /// Which transition.
    pub transition_id: String,
    /// Who is releasing it (the authenticated caller, never claimed).
    pub person_id: String,
}

/// Release a graceful transition so its structural change may proceed.
///
/// This is what `reflect` became when the reflection payload was deleted
/// (#751-P4). Everything that made `reflect` a fence is retained exactly:
/// the transition must exist and belong to `person_id`, and a terminal
/// transition refuses. On success the status moves to
/// [`TransitionStatus::Ready`].
///
/// It takes no payload and writes no memory record. The idempotency machinery
/// the payload needed went with it: a repeat call on an already-`ready`
/// transition simply re-writes the same status, which is a no-op diff, so the
/// convergence property the old content-conflict refusal protected now holds
/// by construction.
///
/// # Errors
/// [`UNKNOWN_TRANSITION`], [`TRANSITION_TERMINAL`].
pub fn release(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    supervision: &SupervisionLedger,
    input: &ReleaseInput,
) -> Result<GracefulTransition, ChiefdError> {
    mutate(ledgers, manifest, supervision, |draft, _ctx, _at| {
        let current = draft
            .transitions
            .get(&input.transition_id)
            .filter(|transition| transition.person_id == input.person_id)
            .ok_or_else(|| {
                refused(
                    UNKNOWN_TRANSITION,
                    format!(
                        "Unknown graceful transition '{}' for '{}'",
                        input.transition_id, input.person_id
                    ),
                )
            })?
            .clone();
        if current.status.is_terminal() {
            return Err(refused(
                TRANSITION_TERMINAL,
                format!(
                    "Graceful transition '{}' is already {}",
                    input.transition_id,
                    current.status.as_str()
                ),
            ));
        }
        let transition = draft
            .transitions
            .get_mut(&input.transition_id)
            .ok_or_else(|| vanished("the transition being released"))?;
        transition.status = TransitionStatus::Ready;
        Ok(transition.clone())
    })
}

/// Record what this person's own pane says the AGENT is doing, so the settle
/// countdown can start on a transition to idle and on nothing else.
///
/// `working = true` is any activity event the pane observes -- a turn starting,
/// the model streaming, a tool executing, mail arriving. It stamps
/// [`PersonActivityState::agent_active_at`], CLEARS the quiet lease outright
/// (never pauses it: the next idle starts a full lease from the top), and
/// cancels a pending ROUTINE idle park, because a park admitted by a clock that
/// should not have been running is an order to tear down a pane mid-turn. An
/// intent-bound park, an offboard or a structural handoff is a real instruction
/// and survives untouched -- exactly the carve-out the arrival reset already
/// makes in [`reconcile`].
///
/// `working = false` is `agent_settled`: it clears the stamp and does nothing
/// else. The lease is stamped by [`reconcile`] alone, which is the only place
/// that knows whether the person also has durable demand; the commit here wakes
/// it through the ordinary change feed.
///
/// Idempotent by value: a repeated identical beat that changes no field leaves
/// the ledger untouched and returns `false`.
///
/// # Errors
/// [`UNKNOWN_PERSON`] when the manifest has no such person; otherwise whatever
/// [`mutate`] refuses.
pub fn note_agent_activity(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    supervision: &SupervisionLedger,
    person_id: &str,
    working: bool,
) -> Result<bool, ChiefdError> {
    let person_id = person_id.to_string();
    mutate(ledgers, manifest, supervision, move |draft, _ctx, at| {
        if !draft.people.contains_key(&person_id) {
            return Err(refused(
                UNKNOWN_PERSON,
                format!("Unknown person '{person_id}' for an agent activity beat"),
            ));
        }
        let cancel = working
            && draft.active_transition(&person_id).is_some_and(|transition| {
                is_routine_idle_park(transition) && transition.status.is_pending()
            });
        if cancel {
            if let Some(id) = draft.active_transition_id_of(&person_id) {
                cancel_transition(draft, &id, at);
            }
        }
        let Some(state) = draft.people.get_mut(&person_id) else {
            return Err(vanished("the person receiving an agent activity beat"));
        };
        let before = state.clone();
        if working {
            state.agent_active_at = Some(at.to_string());
            state.agent_quiet_at = None;
            state.idle_since = None;
        } else {
            // `agent_settled`. The quiet instant is recorded EXACTLY, rather
            // than merely clearing the working stamp: "settled at 10:31" and
            // "never said anything" are different facts and the countdown
            // treats them differently -- the first starts a clock, the second
            // starts nothing. `agent_active_at` is cleared so the inferred
            // path in `agent_quiet_since` cannot also fire and disagree.
            state.agent_active_at = None;
            state.agent_quiet_at = Some(at.to_string());
        }
        let changed = *state != before;
        if changed {
            state.updated_at = at.to_string();
        }
        Ok(changed || cancel)
    })
}

/// Prove a person has a released transition for `action` before the structural
/// change proceeds.
///
/// Pure read.
///
/// TOMBSTONE (#751-P4): the filter used to require an embedded reflection
/// payload alongside the released status. Status is now the whole fact.
///
/// # Errors
/// [`HANDOFF_REQUIRED`].
pub fn require_ready(
    ledgers: &Ledgers,
    manifest: &OrganizationManifest,
    _supervision: &SupervisionLedger,
    person_id: &str,
    action: TransitionAction,
) -> Result<GracefulTransition, ChiefdError> {
    let ledger = read(ledgers, manifest)?;
    let transition = ledger
        .active_transition(person_id)
        .filter(|transition| transition.action == action && transition.status.is_released());
    let Some(transition) = transition else {
        return Err(ChiefdError::refused(
            HANDOFF_REQUIRED,
            format!(
                "Person '{person_id}' must release their graceful transition before {}",
                action.as_str()
            ),
        ));
    };
    Ok(transition.clone())
}

// --- reconcile -----------------------------------------------------------

/// The per-node launch-intent fence, as a type with no permissive default.
///
/// Plan §5.5 and inv c-1: *omission is not an off switch.* An
/// `Option<Vec<String>>` would have made forgetting the field silently disable
/// the product's primary safety guarantee, which is the exact refactor bug the
/// predecessor shipped. Here the caller must choose a variant, and the only
/// permissive one is spelled out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchFence {
    /// Exactly these non-CEO people may run. An empty list means CEO-only.
    Fenced(BTreeSet<String>),
    /// The deliberate sentinel: run the fleet unfenced. No caller can reach
    /// this by dropping a key.
    Unfenced,
}

impl LaunchFence {
    /// A fence naming exactly these people.
    pub fn fenced(person_ids: impl IntoIterator<Item = String>) -> Self {
        Self::Fenced(person_ids.into_iter().collect())
    }

    /// CEO-only — what an omitted or empty allow-list means.
    #[must_use]
    pub fn deny_all() -> Self {
        Self::Fenced(BTreeSet::new())
    }

    fn admits(&self, person_id: &str, chief_person_id: &str) -> bool {
        match self {
            Self::Unfenced => true,
            Self::Fenced(allowed) => person_id == chief_person_id || allowed.contains(person_id),
        }
    }
}

/// What one reconcile is told about the world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileInput {
    /// The fence. Required, by type.
    pub launch_intent: LaunchFence,
    /// Explicit wake requests. A hint, never authority.
    pub requested_person_ids: Vec<String>,
    /// When THIS chiefd started watching, ISO-8601.
    ///
    /// Not a preference and not a clock offset: it is the answer to "over what
    /// window is a MISSING heartbeat evidence of anything?". A heartbeat can
    /// only go missing while somebody is listening, and between chiefd
    /// stopping and chiefd starting nobody was. See [`agent_quiet_since`],
    /// which is the one reader.
    ///
    /// Required rather than optional-with-a-default, and stated at every
    /// construction site, because a field wired in some places and defaulted in
    /// others gives the clamp in tests and not in production — which is worse
    /// than no clamp, since it makes the gap invisible exactly where it bites.
    pub watching_since: String,
}

// TOMBSTONE: `observed_person_ids: Option<BTreeSet<String>>`.
//
// This carried a HOST FACT — who the tmux actuator saw — into a DURABLE
// decision, and it is gone with the whole observation path. It was also
// actively wrong: `cycle.rs` passed `Some(observed_person_ids)`
// unconditionally, so an `Observation::Untrusted` report ("I could not look")
// arrived here as `Some(EMPTY)` ("I looked, nobody is there"). The three
// readers all used `is_none_or`/`is_some_and`, so `None` was safe and
// `Some(EMPTY)` was catastrophic: retention dropped, arrival never fired,
// transfers took the stopped fork. The planner withheld its ACTIONS on an
// untrusted pass, but this reconcile committed in the same pass, so the false
// conclusion was persisted while the correct plan was discarded.
//
// chiefd now publishes the desired state and the actuator diffs it against
// tmux itself. If a richer desired state is ever needed the answer is a THIRD
// VALUE here (Running / RetainIfPresent / Stopped), never a fact travelling
// back up.

/// One person's projection decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonActivityDecision {
    /// Whose decision.
    pub person_id: String,
    /// Whether the person should be running.
    pub active: bool,
    // TOMBSTONE (#751-P9): `pane_department_id` sat here — the head-in-parent
    // display answer, carried out of the reconcile so it could be persisted and
    // re-read. A reconcile decision says WHO runs; WHERE their pane is drawn is
    // the operator client's derivation.
    /// Why, sorted for determinism.
    pub reasons: Vec<ActivityReason>,
    /// The transition they owe, if any.
    pub transition_id: Option<String>,
}

/// The result of one reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySnapshot {
    /// Per-person decisions, in manifest order.
    pub people: BTreeMap<String, PersonActivityDecision>,
}

fn add_reason(
    reasons: &mut BTreeMap<String, BTreeSet<ActivityReason>>,
    person_id: &str,
    reason: ActivityReason,
) {
    reasons.entry(person_id.to_string()).or_default().insert(reason);
}

fn has_effective_demand(
    reasons: &BTreeMap<String, BTreeSet<ActivityReason>>,
    person_id: &str,
) -> bool {
    reasons.get(person_id).is_some_and(|set| set.iter().any(|reason| reason.is_effective_demand()))
}

/// #29 (the operator's settle-shutdown contract): whether the person is pinned against
/// automatic park by demand attached to ITSELF. A pure `ManagingOpenWork` reason
/// (a manager above a report with open work, nothing of its own) is supervisory
/// bookkeeping, not own demand, so it must NOT keep the manager resident — the
/// report re-wakes it on cadence. Every other reason pins exactly as before.
/// Parity with the TS `hasOwnPinningDemand` (src/organization/org-activity.ts).
fn has_own_pinning_demand(
    reasons: &BTreeMap<String, BTreeSet<ActivityReason>>,
    person_id: &str,
) -> bool {
    reasons
        .get(person_id)
        .is_some_and(|set| set.iter().any(|reason| *reason != ActivityReason::ManagingOpenWork))
}

/// Whether current organization truth permits this person to receive new
/// runtime demand.
///
/// This is public for the host convergence boundary. A retained departed or
/// paused person can still have readable mail and durable maintenance history,
/// but neither fact can make them operational again. Existing attended
/// handoffs are separate: their already-held launch fence stays until the
/// transition is terminal.
#[must_use]
pub fn person_is_operational(manifest: &OrganizationManifest, person_id: &str) -> bool {
    let Some(person) = manifest.people.get(person_id) else { return false };
    if person.employment_state != EmploymentState::Active
        || !organization_unit_is_active(manifest, &person.department_id)
    {
        return false;
    }
    manifest
        .headed_department(person_id)
        .is_none_or(|headed| organization_unit_is_active(manifest, &headed.id))
}

/// Whether a transition is a routine idle park — the scheduler hint, as
/// opposed to an operator's or a lifecycle command's park.
///
/// Public so the converge cycle's settle path can recognize the same decision
/// it must turn into a committed launch-intent withdrawal (F8): only a
/// routine idle park de-authorizes a person by itself.
#[must_use]
pub fn is_routine_idle_park(transition: &GracefulTransition) -> bool {
    transition.action == TransitionAction::Park
        && transition.intent_id.is_none()
        && transition.reason == IDLE_AUTO_PARK_REASON
}

/// The instant this person's quiet lease began, or `None` when no settle
/// countdown should be running at all.
///
/// THE WHOLE CLOCK, in one function. Three states, and only the third has a
/// clock:
///
/// - **Never beaten** -> `None`. Idle means "was working and stopped"; a person
///   whose process has never said anything never started, so there is nothing
///   to time. This is the case that used to be timed anyway, against a pane
///   that did not exist.
/// - **Beating** -> `None`. A fresh beat is proof the agent is mid-turn. It is
///   also proof the PROCESS EXISTS, which is how this satisfies
///   [`ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS`]'s residency contract by
///   construction rather than by asking the host.
/// - **Went quiet or was explicitly started** -> the exact stored lease
///   baseline. An explicit start is a new operator decision and replaces any
///   older quiet interval before supervision runs again.
///
/// Going quiet has two shapes and they get different instants. An explicit
/// `agent_settled` is exact, so the clock starts where the agent said. A beat
/// that simply stopped arriving is inferred, and the clock starts at
/// `agent_active_at + AGENT_ACTIVITY_LIVENESS_MS` -- the moment the missing
/// heartbeat became conclusive -- never at the last beat itself, which would
/// bill the agent for a silence chiefd had not yet decided was silence.
///
/// This is what [`AGENT_ACTIVITY_LIVENESS_MS`] means now, and why it survives
/// the deletion of the host observation: it is the timeout that converts a
/// MISSING heartbeat into a quiet instant, which is the only remaining thing
/// standing between a pane that died mid-turn and immortality. A pane that beat
/// and then died settles in at most `AGENT_ACTIVITY_LIVENESS_MS` +
/// `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS` = 600s (2026-08-24: the lease
/// moved 120s -> 300s, so this total moved 420s -> 600s).
///
/// **A pane that NEVER BEAT gets no clock at all**, and the `?` below is where
/// that happens: no `agent_active_at`, no conclusive instant, no quiet instant,
/// `None`. It is deliberate -- there is nothing honest to measure from -- and
/// the case is covered by the actuator's crash-loop counter rather than here.
/// See [`AGENT_ACTIVITY_LIVENESS_MS`]'s doc for the whole argument; do not
/// "fix" it by defaulting the missing stamp, which would make chiefd assert a
/// start instant it cannot know.
/// # chiefd downtime: an absence nobody was listening for is not evidence
///
/// The inferred branch reads `agent_active_at + AGENT_ACTIVITY_LIVENESS_MS <=
/// now`. Unclamped, chiefd being DOWN for longer than that bound brings every
/// person who was mid-turn when it stopped back with a conclusive quiet instant
/// ALREADY IN THE PAST — `idle_since` immediately older than the whole quiet
/// lease, and every one of them a park candidate the moment chiefd returns,
/// with no grace at all. A restart would settle a company that was busy.
///
/// The rising-edge clear does not cover it: nobody crosses the
/// desired-inactive-to-active edge when chiefd merely restarts, so there is no
/// transition to hang a clear on.
///
/// So `watching_since` clamps it. A heartbeat can only be MISSING relative to
/// somebody listening for it, and chiefd was not listening between its own stop
/// and its own start; over that window a silence is not evidence. The
/// conclusive instant is therefore the later of the two — the agent's own
/// stamp, and chiefd's watch start — each plus one full liveness window,
/// because a beat that arrives one millisecond after chiefd is back is not
/// late.
///
/// The clamp is deliberately NOT applied to an explicit `agent_quiet_at`. That
/// is a report the agent SENT, not an absence chiefd inferred, and it stays
/// true across a restart: the agent said it had settled, and chiefd not
/// watching afterwards does not un-say it.
///
/// # THE RESIDUAL, and it is the other direction
///
/// A person who genuinely went quiet DURING the downtime now gets a full grace
/// period after the restart rather than being settled at once. That is the
/// error this is willing to make: it costs one idle agent one extra lease, and
/// the error it replaces tears down a company that was working. See
/// `DECISIONS.md`.
fn agent_quiet_since(state: &PersonActivityState, watching_since: i64, now: i64) -> Option<String> {
    if let Some(quiet_at) = state.agent_quiet_at.clone() {
        return Some(quiet_at);
    }
    let active_at = state.agent_active_at.as_deref().and_then(parse_iso_millis)?;
    // SATURATING. The fail-closed parse above maps an unparseable watch
    // instant to `i64::MAX`, and an unchecked `+` on that panics in debug and
    // WRAPS NEGATIVE in release -- and a negative conclusive instant is already
    // past, so it would settle everybody at once. That is the exact defect this
    // clamp exists to prevent, reached through the exact input the fail-closed
    // parse was chosen to make safe.
    let conclusive_at = active_at.max(watching_since).saturating_add(AGENT_ACTIVITY_LIVENESS_MS);
    (conclusive_at <= now).then(|| iso_millis(conclusive_at))
}

/// Is this person inside the quiet lease an operator's wake bought them?
///
/// THE MANDATE, in one function. Operator ruling, 2026-08-20: *"If I tell chief
/// to message it, it'll come back up and do the 2min settling. We need it to
/// always do that when woken. Message or not. If woken, it needs to wait the 2
/// mins."*
///
/// A wake is an operator DECISION, and until this existed the product had no
/// durable record of one. Every rule that could stop a person read the AGENT's
/// own reports — [`agent_quiet_at`](PersonActivityState::agent_quiet_at),
/// [`agent_active_at`](PersonActivityState::agent_active_at),
/// `last_desired_active` — and by those reports a woken agent that beats once and
/// is then given nothing to do is identical to one that finished its work and
/// went quiet. So it was settled, or had its launch intent withdrawn as a fence
/// with no demand behind it, within seconds of the click that asked for it.
///
/// # THE MEASUREMENT, so nobody has to take the paragraph above on trust
///
/// `research-promoter` ("Pru") on `taperoom-inc`, a live box,
/// 2026-08-20, read from `org_events` and the daemon log:
///
/// ```text
/// 20:34:00.543  launch-intent    research-promoter  upsert  actor='service'
/// 20:34:02.708  launch-intent    research-promoter  delete  actor=''
/// 20:34:07.760  person-activity  research-promoter  upsert
/// 20:34:13+     reconcile.people.withheld: research-promoter[nothing-demanded-them]
/// ```
///
/// Line 1 is the operator pressing Wake Up. Line 2 is 2.165 seconds later, and
/// the pass that wrote it reported `launching: ..., research-promoter, ...` in
/// the same second — it enforced a fence that still named her while committing
/// one that did not. Line 3 is her agent beating ONCE, into a company that had
/// already stopped wanting her. Line 4 is every pass after that, for ever.
/// Nothing was ever sent to her, and no `launch intent withdrawn (...)` line
/// names her anywhere in that window.
///
/// That trace is the whole argument for this function existing. Each rule that
/// contributed to it was individually correct about what the AGENT had said;
/// together they deleted what the OPERATOR had said, because nothing in the
/// product recorded that at all.
///
/// # WHAT IT IS NOT
///
/// The lease is a FLOOR, never a ceiling. Work that arrives inside the window
/// behaves exactly as it does today, and the instant the window closes this
/// returns `false` for ever after: the ordinary settle owns the person again
/// with no residue and nobody is pinned. A wake that pinned somebody
/// permanently would be a different defect, not a fix.
///
/// An unparseable stamp is NOT a lease, and neither is one in the FUTURE. Both
/// fail safe in the direction that keeps the settle working: a damaged column,
/// or a stamp written against a clock that disagrees with this one, can prolong
/// nobody. The window is closed at both ends deliberately —
/// `woke_at <= now < woke_at + lease` — because an open-ended `now < woke_at +
/// lease` makes any stamp far enough ahead a permanent pin, which is the one
/// outcome this whole mechanism must not be able to produce.
///
/// # THIS IS A PRODUCT INVARIANT, NOT A TUNING KNOB
///
/// It is named as one in `CLAUDE.md` beside organization hierarchy, tmux
/// placement, messaging and staffing, and it is the one on that list a reader
/// will mistake for an implementation detail — because every call site it
/// gates looks locally correct without it. Anything that removes, shortens or
/// conditionalizes this floor is a product change and needs the operator, not
/// a refactor. The tests that fail when it goes are named for the rule rather
/// than for the mechanism, so the failure says what was lost:
/// `a_wake_holds_the_launch_intent_for_the_whole_quiet_lease_with_no_message`
/// (converge, end to end),
/// `a_wake_holds_somebody_up_even_when_their_quiet_clock_says_they_are_long_idle`
/// (this rule, with the agent clocks deliberately disagreeing), and
/// `a_wake_supplies_demand_for_its_whole_lease_and_not_one_pass_longer` (both
/// ends of the window).
#[must_use]
pub fn operator_wake_lease_active(state: &PersonActivityState, now: i64) -> bool {
    state.operator_wake_at.as_deref().and_then(parse_iso_millis).is_some_and(|woke_at| {
        woke_at <= now && now < woke_at + ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS
    })
}

/// The pure, fenceable decision made before an ordinary non-CEO idle stop
/// begins (`org-activity-state.ts:84-88`).
///
/// The wake lease gates it FIRST, and gates it here rather than at the candidate
/// filter, because this is the one question every stop path asks. See
/// [`operator_wake_lease_active`].
fn settled_idle_stop_lease_expired(state: &PersonActivityState, now: i64) -> bool {
    !operator_wake_lease_active(state, now)
        && state.last_desired_active
        && state
            .idle_since
            .as_deref()
            .and_then(parse_iso_millis)
            .is_some_and(|idle_at| idle_at + ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS <= now)
}

/// A structural change the manifest has already committed that the persisted
/// activity state has not caught up with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StructuralTransition {
    action: TransitionAction,
    to_department_id: Option<String>,
    /// An explicit unit pause overrides stale work leases after the handoff.
    force_removal: bool,
}

/// Derive the structural transition implied by the gap between the persisted
/// state and the manifest (`org-activity.ts:776-786`).
///
/// The order matters and is the port: departure beats benching, benching beats
/// a unit pause, and a placement change is a transfer. That last clause used to
/// read "a home-department change is a transfer even when the assigned
/// department also moved", which was a tie-break between two columns; there is
/// one column and so no tie to break.
fn structural_transition(
    state: &PersonActivityState,
    manifest: &OrganizationManifest,
    person_id: &str,
) -> Option<StructuralTransition> {
    let person = manifest.people.get(person_id)?;
    let plain = |action: TransitionAction| {
        Some(StructuralTransition { action, to_department_id: None, force_removal: false })
    };
    if state.last_employment_state == EmploymentState::Active
        && person.employment_state == EmploymentState::Departed
    {
        return plain(TransitionAction::Offboard);
    }
    if state.last_employment_state == EmploymentState::Active
        && person.employment_state != EmploymentState::Active
    {
        return plain(TransitionAction::Park);
    }
    if state.last_operational && !organization_unit_is_active(manifest, &person.department_id) {
        return Some(StructuralTransition {
            action: TransitionAction::Park,
            to_department_id: None,
            force_removal: true,
        });
    }
    if state.last_department_id != person.department_id {
        return Some(StructuralTransition {
            // ONE arm, and a placement change is a TRANSFER, always.
            //
            // This was two arms tested in order — home-changed, then
            // assigned-changed — because the two columns could move
            // independently and the second arm raised a LOAN rather than a
            // transfer. The loan verbs went on 2026-08-13, which collapsed the
            // two actions into one; #1081 collapsed the two columns, which
            // collapses the two arms into this one. A second arm here would now
            // be the same comparison written twice, and unreachable besides.
            action: TransitionAction::Transfer,
            to_department_id: Some(person.department_id.clone()),
            force_removal: false,
        });
    }
    None
}

// #551 (DEFECT 3, manager cascade): `mark_management_chain` used to walk
// EVERY manager above a person with a live reason and add `ManagingOpenWork`
// to each of them in turn -- removed entirely, not bounded with a depth
// limit, per the design ruling: "reasons flow to the person they name,
// never up the hierarchy... delete the upward propagation; do not add a
// depth limit." A manager is desired-active because IT has a live reason,
// never because a report does.
//
// NAMED UNCERTAINTY: `ManagingOpenWork::is_effective_demand()` (this file,
// above) already returned `false`, so by direct trace this reason alone
// never set `active = true` even before this removal -- I could not find a
// live path where the cascade currently pins a manager on its own. Removing
// the propagation is still correct per the design ruling regardless (a
// reason that can never cause activity has no reason to exist, and its
// mere presence is exactly the kind of state a future change could start
// reading), so this is not held pending that question -- it is the
// instructed fix, applied. If a live pinning path through `ManagingOpenWork`
// existed that this trace missed, this removal closes it too.

/// Recompute who should be running, and commit the result.
///
/// The launch-intent fence is applied **last**, after every demand reason has
/// been computed, so replayed durable demand and stale persisted
/// desired-active state can never open the fleet. The durable
/// `lastDesiredActive` is written fenced too, so the supervisor's
/// exact-projection comparison stays consistent.
///
/// # Errors
/// [`UNKNOWN_PERSON`] for a requested or monitored person outside the
/// manifest — refused rather than silently ignored; `Corrupt` from [`read`].
#[allow(clippy::too_many_lines)] // The D9 order is the contract; splitting hides it.
pub fn reconcile(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    supervision: &SupervisionLedger,
    input: &ReconcileInput,
) -> Result<ActivitySnapshot, ChiefdError> {
    let ceo = manifest.chief_person_id()?.to_string();
    // Parsed ONCE, here, and fail-closed: an unparseable watch instant becomes
    // `i64::MAX`, which clamps every inferred quiet instant into the future and
    // therefore settles NOBODY. The other rounding -- treating it as 0, "chiefd
    // has always been watching" -- restores the exact defect the clamp exists to
    // remove, silently, on a malformed string.
    let watching_since = parse_iso_millis(&input.watching_since).unwrap_or(i64::MAX);
    let mut snapshot_people: BTreeMap<String, PersonActivityDecision> = BTreeMap::new();
    {
        let people = &mut snapshot_people;
        mutate(ledgers, manifest, supervision, move |draft, ctx, at| {
            let manifest = ctx.manifest();
            let now = ctx.now();

            // --- inputs, validated before anything is touched --------------
            let requested: BTreeSet<&String> = input.requested_person_ids.iter().collect();
            for person_id in &requested {
                if !manifest.people.contains_key(*person_id) {
                    return Err(refused(
                        UNKNOWN_PERSON,
                        format!("Unknown requested person '{person_id}'"),
                    ));
                }
            }

            // --- demand reasons -------------------------------------------
            let mut reasons: BTreeMap<String, BTreeSet<ActivityReason>> = BTreeMap::new();
            // THE CEO NEVER SLEEPS. Operator ruling, 2026-08-14, given on a
            // live box where the root had settled and its pane was gone while
            // its staff kept running: "CEO can never go to sleep."
            //
            // This supersedes the earlier "everybody, dead" ruling for the ROOT
            // only. Everybody else still settles on the ordinary two-minute
            // lease — that ruling stands unchanged and is what the rest of this
            // function implements. The root is the operator's door into the
            // company: a company whose CEO has parked cannot be talked to at
            // all, which is not a resting state but an unreachable one.
            //
            // #1148 deleted this line and the deletion was live for one
            // evening. Its own tombstone recorded what that cost — the root
            // does not merely park after the settle window, it never starts, because
            // this lease was the only thing that made a CEO run AT ALL. The
            // work done in the meantime is NOT reverted and still matters:
            // genesis now records the CEO's start decision, and the launch
            // fence no longer eats it. Those make the root's demand honest
            // rather than implicit, so this lease is now a floor under a
            // working mechanism instead of the only thing holding it up.
            add_reason(&mut reasons, &ceo, ActivityReason::OrganizationRoot);

            for person_id in &requested {
                if person_is_operational(manifest, person_id) {
                    add_reason(&mut reasons, person_id, ActivityReason::Requested);
                }
            }

            // #551 (DEFECT 3): this pass used to walk `mark_management_chain`
            // for every person with any reason, adding `ManagingOpenWork` to
            // their entire management chain. Removed -- see the comment at
            // `mark_management_chain`'s former definition (below, kept as a
            // marker for the next reader) for the full ruling.

            // TOMBSTONE: the arrival edge (#, operator 2026-08-10 "they all
            // started fine ... as soon as they started they immediately all
            // shut down").
            //
            // A pass over `observed_person_ids` used to cancel a routine idle
            // park for anyone the host observation proved had just come up, and
            // reset `idle_since` on the transition into residency. It existed
            // to paper over a clock that started at the wrong moment: the lease
            // was stamped when chiefd DECIDED a person should run, so a person
            // queued behind the writer thread burned the whole lease before the
            // process existed.
            //
            // The clock is now founded on the AGENT's own report of going
            // quiet, so there is no early start to rescue and no residency edge
            // to detect -- a heartbeat IS proof the process exists. The
            // `last_observed_resident` field it maintained is gone with it.
            // --- idle leases and routine-park recycling --------------------
            //
            // Routine idle parking is passive maintenance. The root CEO is
            // deliberately resident; everyone else gets a durable sixty-second
            // quiet lease after effective demand clears, persisted BEFORE
            // candidate selection so a restart cannot bypass the fence.
            for person_id in draft.person_order.clone() {
                let effective = has_effective_demand(&reasons, &person_id);
                // #29: managing-open-work alone is not own demand, so it must not
                // reset the automatic-park retry backoff either.
                if let Some(state) = draft.people.get_mut(&person_id) {
                    // THE COUNTDOWN STARTS WHEN THE AGENT REPORTS IT WENT QUIET.
                    // An explicit operator start is the one additional
                    // authority: it begins a fresh lease in the same durable
                    // transaction as the launch fence, so this pass cannot
                    // inherit a clock from the person's earlier run.
                    //
                    // Recomputed from scratch every pass rather than stamped
                    // once and carried, so there is no clock to leave running
                    // and none to resume part-spent. The root CEO is
                    // deliberately resident and effective demand answers the
                    // question outright; everything else is the agent's own
                    // report, read through `agent_quiet_since`.
                    //
                    // TOMBSTONE: this branch used to stamp `idle_since` from
                    // chiefd's own bookkeeping -- `last_desired_active` && no
                    // demand && no recent beat -- which timed a person's silence
                    // against a pane that might not exist yet. Under load that
                    // measured the WRITER QUEUE rather than the person, and the
                    // arrival edge existed solely to undo it ("they all started
                    // fine ... as soon as they started they immediately all shut
                    // down"). Both the wrong clock and its rescue are gone.
                    //
                    // Demand and idleness stay different questions. An agent
                    // mid-turn with no open goal is NOT idle -- that is what
                    // `agent_quiet_since` protects -- and an agent that never
                    // stops working never settles, which is the ruling and not
                    // an accident.
                    //
                    // The `last_desired_active` guard is NOT redundant with
                    // `settled_idle_stop_lease_expired`, which already refuses
                    // to park anybody who is not desired-active. That guard
                    // makes the column harmless; this one makes it HONEST. A
                    // parked person still carries the `agent_quiet_at` from
                    // before they were parked, so without this they show an
                    // idle clock -- ticking, and by now enormous -- on a person
                    // who is not running at all, on every surface that reads
                    // the column. "Idle for six days" and "not running" are
                    // different sentences and only one of them is true.
                    // The CEO is excluded, because the CEO never sleeps
                    // (operator ruling, 2026-08-14). A root that accrued an
                    // idle clock would read as "idle for six days" on every
                    // surface, which is the same untruth this block exists to
                    // prevent one case up: the root holds a permanent lease, so
                    // it is never idle in the sense this column means.
                    state.idle_since =
                        if person_id == ceo || effective || !state.last_desired_active {
                            None
                        } else {
                            agent_quiet_since(state, watching_since, now)
                        };
                }
                // TOMBSTONE: the grace-expiry force. #337 watched for a routine
                // idle park that had sat `Overdue` for a further
                // `ORGANIZATION_AUTOMATIC_PARK_OVERDUE_LEASE_MS` and only THEN
                // forced it terminal. Both the wait and its constant are gone:
                // a routine idle park is born `Forced` in `new_transition`, so
                // there is no interval for anything to expire in and nothing
                // here to sweep. Everything that outcome bought is unchanged —
                // `forced` is terminal, is never retried, and its
                // `active_transition_id` pointer is still KEPT, so a person
                // parked this way can never re-enter automatic-park candidacy on
                // their own; only fresh demand or an explicit staffing request
                // restarts them.
            }

            // --- bounded routine-park admission ---------------------------
            let in_flight = draft
                .person_order
                .iter()
                .filter(|person_id| {
                    draft.active_transition(person_id).is_some_and(|transition| {
                        is_routine_idle_park(transition) && transition.status.is_pending()
                    })
                })
                .count();
            let person_count = draft.person_order.len();
            let cursor = if person_count == 0 {
                0
            } else {
                draft.automatic_park_cursor.unwrap_or(0) % person_count
            };
            let round_robin: Vec<String> = draft.person_order[cursor..]
                .iter()
                .chain(draft.person_order[..cursor].iter())
                .cloned()
                .collect();
            let candidates: Vec<String> = round_robin
                .into_iter()
                .filter(|person_id| {
                    // The belt to the lease's braces. The root holds a
                    // permanent `OrganizationRoot` reason, so it should never
                    // reach candidacy anyway — but "should never" is not a
                    // guarantee, and an imported or repaired ledger that lost
                    // that reason would silently make the operator's own door
                    // an automatic-park candidate. The CEO never sleeps
                    // (operator ruling, 2026-08-14); this is the second lock on
                    // that, and it is cheap.
                    if *person_id == ceo {
                        return false;
                    }
                    let Some(state) = draft.people.get(person_id) else { return false };
                    // #29: "managing-open-work" alone no longer pins a manager
                    // against automatic park -- see `has_own_pinning_demand`.
                    if !state.last_desired_active
                        || has_own_pinning_demand(&reasons, person_id)
                        || structural_transition(state, manifest, person_id).is_some()
                        || !settled_idle_stop_lease_expired(state, now)
                    {
                        return false;
                    }
                    // A person mid-transition — including one holding a terminal
                    // `forced`/`applied` park pointer (#337) — is never a fresh
                    // candidate; that pointer is only released by new demand or
                    // an explicit staffing request, so no separate retry/backoff
                    // bookkeeping is needed.
                    if draft.active_transition(person_id).is_some() {
                        return false;
                    }
                    true
                })
                .collect();
            let slots = ORGANIZATION_AUTOMATIC_PARK_MAX_IN_FLIGHT.saturating_sub(in_flight);
            let admitted: BTreeSet<String> = candidates.into_iter().take(slots).collect();
            if let Some(last) = admitted.iter().max_by_key(|person_id| {
                draft.person_order.iter().position(|id| id == *person_id).unwrap_or(0)
            }) {
                if person_count > 0 {
                    let index = draft.person_order.iter().position(|id| id == last).unwrap_or(0);
                    draft.automatic_park_cursor = Some((index + 1) % person_count);
                }
            }

            // Structural handoffs abandoned because their person provably
            // cannot run. The structural mutation is applied unattended below
            // and nothing claims the person released it.
            let mut abandoned: BTreeSet<String> = BTreeSet::new();
            // An atomic transfer of an already-running person is placement
            // continuity, not a new admission and not a lifecycle handoff.
            // The transition pass proves that narrow edge; the decision pass
            // advances its durable placement and retains it for this reconcile.
            // ONE set, where there were two. Whether the person was RUNNING is
            // read from chiefd's own durable `last_desired_active` at the point
            // of use, never from a host observation of the pane: chiefd already
            // knows what it last desired, and that is the fact the decision
            // actually needs. A transfer of somebody chiefd was not running is
            // still not a reason to start them.
            let mut direct_transfers: BTreeSet<String> = BTreeSet::new();

            // --- transitions ----------------------------------------------
            for person_id in draft.person_order.clone() {
                let transition = draft.active_transition_id_of(&person_id);

                if let Some(id) = transition.clone() {
                    let deadline = parse_iso_millis(&draft.transitions[&id].handoff_deadline_at);
                    let Some(current) = draft.transitions.get_mut(&id) else {
                        return Err(vanished("a transition mid-reconcile"));
                    };
                    if current.status == TransitionStatus::AwaitingHandoff
                        && deadline.is_some_and(|deadline| deadline <= now)
                    {
                        current.status = TransitionStatus::Overdue;
                    }
                }
                // TOMBSTONE (#751-P4): a "durable reflection" invariant used to
                // sit here. A `ready` transition whose reflection row was
                // missing was demoted back to `awaiting_handoff`, and an
                // `applied` one HARD-REFUSED the whole reconcile commit with
                // `handoff-not-durable` ("Applied graceful transition '<id>'
                // has no durable reflection memory"). Both are deleted with the
                // payload they guarded — an applied transition with no
                // reflection is now the only kind there is, and a rule that
                // refuses it would refuse every duty at every existing company.
                // Nothing replaces it: `applied` is self-authenticating, and
                // the identity fence in [`release`] is what makes it earned.

                // --- structural transitions ---------------------------------
                let state = draft.people[&person_id].clone();
                let structural = structural_transition(&state, manifest, &person_id);
                if let Some(structural) = structural {
                    // ONE PATH for a transfer. `org_transfer` has already
                    // committed the normalized placement move; chiefd
                    // publishes the resulting state ("val belongs to department
                    // B") and stops there. No transition is opened, no handoff
                    // is requested, and nothing is waited for.
                    //
                    // TOMBSTONE: `direct_running_transfer` /
                    // `direct_stopped_transfer` forked this ONE durable move
                    // into three runtime behaviours -- preserve-and-retag the
                    // live pane, apply unattended, or open a graceful transition
                    // and wait -- selected by the host observation. That was
                    // chiefd deciding a PROJECTION question it has no business
                    // asking. Whether the actuator relocates the running pane
                    // between windows (tmux `break-pane` moves it without
                    // touching the process, and `create_window_by_move` already
                    // does exactly this) or kills and lets the agent resume is a
                    // purely local choice, made where tmux actually is, and
                    // chiefd never learns which happened.
                    let direct_transfer = structural.action == TransitionAction::Transfer
                        && transition.is_none()
                        && input.launch_intent.admits(&person_id, &ceo);
                    if direct_transfer {
                        direct_transfers.insert(person_id.clone());
                        continue;
                    }
                    let pending = draft
                        .active_transition(&person_id)
                        .is_none_or(|transition| transition.status.is_pending());
                    if pending && !input.launch_intent.admits(&person_id, &ceo) {
                        // Reconciliation must never manufacture work that only
                        // un-fencing can finish. This person is fenced out of
                        // running, so the release this change would wait for is
                        // unreachable: abandon it truthfully and let the change
                        // proceed. `cancelled` with `abandonedAt`, never
                        // `applied` — `applied` asserts a release happened.
                        if let Some(id) = draft.active_transition_id_of(&person_id) {
                            cancel_transition(draft, &id, at);
                            if let Some(transition) = draft.transitions.get_mut(&id) {
                                transition.abandoned_at = Some(at.to_string());
                            }
                        }
                        abandoned.insert(person_id.clone());
                        continue;
                    }
                    let reason = format!(
                        "Release the transition before {} changes pane ownership.",
                        structural.action.as_str()
                    );
                    let transition = ensure_matching_transition(
                        draft,
                        TransitionSpec {
                            person_id: &person_id,
                            action: structural.action,
                            reason: &reason,
                            to_department_id: structural.to_department_id.as_deref(),
                            intent_id: None,
                        },
                        at,
                    )?;
                    add_reason(
                        &mut reasons,
                        &person_id,
                        if transition.status.is_pending() {
                            ActivityReason::HandoffRequired
                        } else {
                            ActivityReason::TransitionReady
                        },
                    );
                } else if !has_own_pinning_demand(&reasons, &person_id) && state.last_desired_active
                {
                    let park_or_none = draft
                        .active_transition(&person_id)
                        .is_none_or(|transition| transition.action == TransitionAction::Park);
                    if park_or_none {
                        let existing = draft.active_transition(&person_id).is_some();
                        if existing || admitted.contains(&person_id) {
                            let transition = ensure_matching_transition(
                                draft,
                                TransitionSpec {
                                    person_id: &person_id,
                                    action: TransitionAction::Park,
                                    reason: IDLE_AUTO_PARK_REASON,
                                    to_department_id: None,
                                    intent_id: None,
                                },
                                at,
                            )?;
                            if transition.status.is_pending() {
                                add_reason(
                                    &mut reasons,
                                    &person_id,
                                    ActivityReason::HandoffRequired,
                                );
                            }
                        } else {
                            add_reason(
                                &mut reasons,
                                &person_id,
                                ActivityReason::MaintenanceBackpressure,
                            );
                        }
                    }
                } else if has_own_pinning_demand(&reasons, &person_id) {
                    // Ordinary idle parking yields to newly arrived work. An
                    // explicit, intent-bound lifecycle handoff does NOT:
                    // cancelling it here made a department with a goal,
                    // assignment or loop impossible to remove, because every
                    // pass manufactured another attempt.
                    let yields = draft.active_transition(&person_id).is_some_and(|transition| {
                        transition.action == TransitionAction::Park
                            && transition.intent_id.is_none()
                    });
                    if yields {
                        if let Some(id) = draft.active_transition_id_of(&person_id) {
                            let applied =
                                draft.transitions[&id].status == TransitionStatus::Applied;
                            if applied {
                                if let Some(state) = draft.people.get_mut(&person_id) {
                                    state.active_transition_id = None;
                                    state.updated_at = at.to_string();
                                }
                            } else {
                                cancel_transition(draft, &id, at);
                            }
                        }
                    }
                }

                // Explicit lifecycle requests can be prepared before a person
                // has ever been projected; their bounded handoff is still a
                // live lease.
                if let Some(id) = draft.active_transition_id_of(&person_id) {
                    if draft.transitions[&id].status.is_pending() {
                        add_reason(&mut reasons, &person_id, ActivityReason::HandoffRequired);
                    }
                }
            }

            // #551 (DEFECT 3): a second `mark_management_chain` pass used to
            // run here too ("a report retained for its handoff also retains
            // its chain"), gated by an `only_bookkeeping` check. Removed with
            // the first pass, same ruling: reasons name the person they
            // describe, never propagate to a manager who has none of their
            // own.

            // --- decisions -------------------------------------------------
            people.clear();
            for person_id in draft.person_order.clone() {
                let person = &manifest.people[&person_id];
                let state = draft.people[&person_id].clone();
                let structural = structural_transition(&state, manifest, &person_id);
                let transition = draft.active_transition_id_of(&person_id);
                let lease: BTreeSet<ActivityReason> =
                    reasons.get(&person_id).cloned().unwrap_or_default();
                let has_work_lease = lease.iter().any(|reason| reason.is_effective_demand());
                let mut active = has_work_lease
                    || lease.contains(&ActivityReason::HandoffRequired)
                    || lease.contains(&ActivityReason::MaintenanceBackpressure);
                // Whether this person's persisted placement — their employment
                // state, department and operational flag — advances this pass. A
                // pending structural transition freezes all three at the values
                // the transition opened against, so the same structural change
                // is re-derived until it is released.
                let mut advance_placement = structural.is_none();

                if direct_transfers.contains(&person_id) {
                    // The placement always advances. The lease is supplied only
                    // when chiefd was ALREADY running this person -- its own
                    // durable fact, not a look at tmux. Retention is exactly one
                    // reconcile edge: the following ordinary activity pass still
                    // owns idle shutdown, so this cannot pin the person or grow
                    // the fleet, and a transfer of a stopped person supplies no
                    // demand and therefore starts nobody.
                    advance_placement = true;
                    if draft.people.get(&person_id).is_some_and(|s| s.last_desired_active) {
                        active = true;
                    }
                }

                match (structural.as_ref(), transition.clone()) {
                    // Abandoned: the ledger says `cancelled` with `abandonedAt`,
                    // so nothing here claims a release happened. The persisted
                    // placement MUST advance — leaving it stale would re-derive
                    // the same structural transition on every future pass.
                    (Some(_), _) if abandoned.contains(&person_id) => {
                        advance_placement = true;
                        active = false;
                    }
                    (Some(structural), Some(id)) if draft.transitions[&id].status.is_released() => {
                        let current = draft.transitions[&id].clone();
                        let removal = current.action.is_removal();
                        if !removal || !has_work_lease || structural.force_removal {
                            if current.status != TransitionStatus::Applied {
                                let record = draft
                                    .transitions
                                    .get_mut(&id)
                                    .ok_or_else(|| vanished("a structural transition"))?;
                                record.status = TransitionStatus::Applied;
                                record.applied_at = Some(at.to_string());
                            }
                            advance_placement = true;
                            active = if removal { false } else { has_work_lease };
                            if let Some(state) = draft.people.get_mut(&person_id) {
                                state.active_transition_id = None;
                            }
                        } else {
                            active = true;
                        }
                    }
                    (None, Some(id))
                        if draft.transitions[&id].action == TransitionAction::Park
                            && draft.transitions[&id].status.is_released()
                            && (!has_work_lease || draft.transitions[&id].intent_id.is_some()) =>
                    {
                        if draft.transitions[&id].status != TransitionStatus::Applied {
                            let record = draft
                                .transitions
                                .get_mut(&id)
                                .ok_or_else(|| vanished("an applied park transition"))?;
                            record.status = TransitionStatus::Applied;
                            record.applied_at = Some(at.to_string());
                        }
                        // A completed explicit stop handoff is authoritative
                        // even if a stale goal, wake, assignment or loop still
                        // advertises work.
                        active = false;
                    }
                    (None, Some(id)) if draft.transitions[&id].action != TransitionAction::Park => {
                        active = true;
                        let ready = draft.transitions[&id].status == TransitionStatus::Ready;
                        add_reason(
                            &mut reasons,
                            &person_id,
                            if ready {
                                ActivityReason::TransitionReady
                            } else {
                                ActivityReason::HandoffRequired
                            },
                        );
                    }
                    _ => {}
                }

                // TOMBSTONE: `physically_retainable` / `bounded_idle_retention`.
                //
                // A person already desired-active used to be retained past the
                // withdrawal of their launch intent so they could finish a
                // routine idle handoff -- guarded by the host observation, so a
                // lease could retain an existing pane but never resurrect a
                // dead one. Both the retention and its guard are gone.
                //
                // THE RULING: there is no "let them finish". chiefd declares the
                // final state, the actuator makes it true, and the agent RESUMES
                // from its transcript exactly as if it had crashed. Everything
                // durable -- goals, tasks, memory, staffing history, files,
                // identity key -- lives in chiefd and on disk, never in the
                // pane, so a kill costs the in-flight turn and nothing else. Pi
                // restores the transcript by default, which is why
                // `fresh_session` has to be an explicit opt-OUT maintenance
                // action rather than the other way round.
                //
                // The wait this deleted was never a real wait: there is no
                // releaser for a ROUTINE idle park (see the constant tombstone
                // above), so it could only ever expire. A structural handoff --
                // bench, transfer, offboard -- does have a real
                // releaser and keeps `HANDOFF_GRACE_MS` untouched.

                // ---- THE LAUNCH-INTENT FENCE, applied last ----------------
                //
                // Nothing may promote a non-CEO node to running without an
                // explicit per-node intent record. Applied AFTER every demand
                // and transition reason, so it overrides all replayed durable
                // demand and stale persisted desired-active state. The durable
                // `lastDesiredActive` is written fenced too, keeping the
                // supervisor's exact-projection comparison consistent.
                if active && !input.launch_intent.admits(&person_id, &ceo) {
                    active = false;
                }

                if let Some(state) = draft.people.get_mut(&person_id) {
                    // THE RISING EDGE: desired-inactive -> desired-active.
                    //
                    // A stored agent stamp is evidence about the interval the
                    // person was CONTINUOUSLY DESIRED-ACTIVE. Crossing this edge
                    // ends that interval, so every stamp from before it is
                    // evidence about a process that is gone.
                    //
                    // Without this clear, a re-hire settles with NO GRACE AT
                    // ALL: the person's `agent_quiet_at` still names the instant
                    // they went quiet before they were parked, that instant is
                    // long past, so `idle_since` is immediately older than
                    // `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS` and the person
                    // is a park candidate before the actuator has even booted
                    // them. `idle_since` goes with the stamps because it was
                    // derived from them earlier in THIS pass, and leaving it
                    // would carry the same stale conclusion one pass further.
                    //
                    // Clearing is right and defaulting would be wrong: the
                    // honest state for a person about to be started is "no
                    // report yet", which is exactly what all three being absent
                    // means. Stamping a fresh instant instead would be chiefd
                    // asserting the agent said something it never said.
                    if active && !state.last_desired_active {
                        state.agent_quiet_at = None;
                        state.agent_active_at = None;
                        state.idle_since = None;
                    }
                    state.last_desired_active = active;
                    if advance_placement {
                        state.last_employment_state = person.employment_state;
                        state.last_department_id.clone_from(&person.department_id);
                        state.last_operational =
                            organization_unit_is_active(manifest, &person.department_id);
                    }
                    state.updated_at = at.to_string();
                }

                let mut reason_list: Vec<ActivityReason> =
                    reasons.get(&person_id).cloned().unwrap_or_default().into_iter().collect();
                reason_list.sort_unstable();
                people.insert(
                    person_id.clone(),
                    PersonActivityDecision {
                        person_id: person_id.clone(),
                        active,
                        reasons: reason_list,
                        transition_id: draft.active_transition_id_of(&person_id),
                    },
                );
            }
            Ok(())
        })?;
    };
    Ok(ActivitySnapshot { people: snapshot_people })
}

/// The normalized-row persistence for this store (org-data-normalization P0,
/// N4): reconstruct/diff over transitions/person_activity/activity_meta,
/// replacing the `activity` JSON blob.
pub mod rows;

#[cfg(test)]
mod tests;
