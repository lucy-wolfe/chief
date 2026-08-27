//! The converge/apply **safety scaffold** — M2, Unit C.
//!
//! One durable, per-company record that gates the reconcile pipeline before any
//! host actuation. It is the day-1 answer to *"we now compute a real converge
//! plan; what stops a wrong plan from tearing a live company apart the first
//! time it runs?"*. Four independent guards live here, and every one of them
//! defaults to the safe direction:
//!
//! 1. **Actuation mode** — [`ActuationMode::Shadow`] (default) computes the full
//!    plan and persists it as intent, but executes **nothing** against the host;
//!    [`ActuationMode::Apply`] is the explicit, operator-set opt-in. A separate
//!    [`ConvergeSafetyState::sweep_live`] sub-flag independently lets the #29
//!    pointer-sweep apply live while the rest of actuation stays in shadow,
//!    because the sweep is ledger-only and fence-proven safe — but it, too,
//!    defaults off and is flipped explicitly.
//! 2. **The action budgets — DELETED.** This module used to own two numbers
//!    that bounded one reconcile pass: a restart budget (`max(2, 25%)`) and a
//!    start cap (`MAX_STARTS_PER_PASS = 8`), both enforced where the actions
//!    were minted. There are no actions any more. chiefd publishes a desired
//!    SET and the actuator computes the transition, so a budget over "how many
//!    things one pass may ask for" has nothing left to bound. See the two
//!    tombstones below for why the numbers did not simply move here, and why a
//!    ramp belongs in the actuator: it is a statement about what one MACHINE
//!    can absorb, and chiefd is not on that machine.
//!
//!    The rule those budgets protected is unchanged and now needs no
//!    arithmetic: **stops were always exempt**, because stopping a
//!    no-longer-desired person IS the mandated shrink (CLAUDE.md's HARD RULE —
//!    "shrink is as important as grow"). Under a desired set, a person who
//!    should not run is simply ABSENT from it, so the shrink cannot be
//!    throttled by construction rather than by a carve-out somebody has to
//!    remember.
//! 3. **3-strike circuit breaker** — three *consecutive* failed apply cycles
//!    trip the company back to shadow ([`ConvergeSafetyState::breaker_tripped`])
//!    and stamp the trip; any single success resets the counter, and only an
//!    explicit operator clear ([`operator_clear_breaker`]) or a config change
//!    ([`set_actuation_config`]) resumes apply. [`record_cycle_outcome`] returns
//!    [`BreakerAction::Tripped`] on the transition so the caller — which is
//!    already inside a supervision mutation context — enqueues the escalation
//!    effect. This store records the trip **durably**; the effects pipeline
//!    belongs to the supervision store and its sequence invariant, so the
//!    enqueue stays on the caller's side of the seam.
//! 4. **Single-flight + floor interval** — [`begin_cycle`]/[`end_cycle`] take and
//!    release a durable `cycle_in_progress` claim. Because the writer actor
//!    serializes every mutation for a company, the claim's check-and-set is
//!    atomic: a second cycle that begins while one is in flight sees the claim
//!    and is [`CycleGate::Skipped`]. The same call enforces a minimum spacing
//!    between cycle *starts*. A crashed cycle's claim is reclaimed after
//!    [`CLAIM_STALE_MS`] so a lost `end_cycle` cannot wedge a company forever.
//!
//! # Polarity: `FailSafeValue`, and why the restrictive value is "execute nothing"
//!
//! Registered `FailSafeValue` on all three operations. The safe direction for a
//! safety gate is unambiguous: unreadable bytes must never read as
//! *apply-everything-with-a-clear-breaker*. [`ConvergeSafetyStore::restrictive`]
//! is the fully-conservative value — shadow, sweep off, override off, breaker
//! **tripped** — so a corrupt row degrades to "compute the plan, actuate
//! nothing" plus a warning. It is deliberately **not** `FailClosed`: surfacing a
//! `Corrupt` error on the hot gate path would force every caller to translate it
//! back into "so, shadow then", and the point of the polarity type is to make
//! that translation unwritable.
//!
//! An **absent** row is the ordinary default (a company nobody has configured
//! actuation for yet), not a recovery, so it reads as
//! [`ConvergeSafetyState::default_shadow`] with **no** warning. Both resolve the
//! gate to shadow; the difference is that an operator's first
//! `set_actuation_config(Apply)` on a fresh company enables apply immediately.
//!
//! # No company-context validation
//!
//! Unlike `health`/`launch_intent`, this store does not re-validate its body's
//! company against a [`crate::store::CompanyContext`]: the database is per-company
//! by construction (`<company dir>/.chief/db/chief.db` -- the CURRENT DIRECTORY
//! is the company; the retired `<dataRoot>/<slug>/` sibling layout this comment
//! used to name is gone), the reconcile caller holds a
//! [`crate::actor::writer::CompanyDb`] for exactly one company, and there is no
//! manifest fact this row needs to agree with. The schema-version check is the
//! whole of its corruption guard. Dropping the check is what lets the host read be a plain
//! `read_safety_config(db)` with nothing to thread.

use serde::{Deserialize, Serialize};

use crate::isotime::iso_millis;
use crate::ledger::Ledgers;
use crate::polarity::{decode_fail_safe_value, Decoded, FailSafeValue, StoreKind};

/// Schema version of the converge-safety document body.
pub const CONVERGE_SAFETY_SCHEMA_VERSION: u32 = 1;

/// Consecutive failed apply cycles that trip the breaker back to shadow.
pub const BREAKER_TRIP_THRESHOLD: u32 = 3;

// TOMBSTONE: `MAX_STARTS_PER_PASS = 8`, and with it the whole admission ramp.
//
// It capped the start actions one reconcile pass could ask for, deferring the
// rest. It came from a real incident (#431: 34 staffing changes in one pass
// pushed the box into swap -- free mem 204MB, load ~25 on 6 cores), and the
// value was derived from that incident's measured per-spawn cost.
//
// DELETED BY OPERATOR RULING: "just boot them all at the same time." Two
// reasons it is the right deletion rather than a regression. First, chiefd no
// longer mints start actions at all -- it publishes a desired SET, and there is
// no such thing as a partial truth about who should be running; capping the
// published set would make chiefd's stated desired state depend on how busy a
// box is. Second, a ramp is a decision about a MACHINE's capacity, and chiefd
// is not on that machine. If a boot storm needs pacing, the pacing belongs in
// the actuator, where the processes are actually spawned and where the load can
// actually be observed.

/// How long a `cycle_in_progress` claim is honoured before it is treated as
/// crash residue and reclaimed. Must exceed the longest legitimate apply cycle;
/// ten minutes is far beyond any converge pass and well under "wedged forever".
pub const CLAIM_STALE_MS: i64 = 600_000;

/// Whether the reconcile pipeline may actuate against the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActuationMode {
    /// Compute and persist the plan as intent; execute nothing. The default.
    #[default]
    Shadow,
    /// Execute the plan against the host. Explicit, operator-set opt-in.
    Apply,
}

/// The operator-visible actuation configuration the interpreter reads each pass.
///
/// A **projection** of the durable state with the breaker already folded in:
/// [`ConvergeSafetyState::effective_config`] returns [`ActuationMode::Shadow`]
/// whenever the breaker is tripped, so the interpreter never has to know *why*
/// it is in shadow — only that it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SafetyConfig {
    /// The effective actuation mode (shadow if the breaker is tripped).
    pub actuation_mode: ActuationMode,
    /// Whether the #29 pointer-sweep may apply live this pass.
    pub sweep_live: bool,
    /// Whether the durable operator override suspends the destructive budget.
    pub budget_override_active: bool,
}

/// What one apply-cycle outcome did to the breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerAction {
    /// The breaker did not change state; keep going.
    Continue,
    /// This outcome tripped the breaker back to shadow. The caller escalates —
    /// exactly once, because a tripped company runs no further apply cycles.
    Tripped,
}

/// Whether a cycle may start this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleGate {
    /// The claim was taken and the floor was met — run the cycle.
    Proceed,
    /// No cycle this pass, with why.
    Skipped(SkipReason),
}

impl CycleGate {
    /// Whether a cycle may run.
    #[must_use]
    pub fn may_proceed(&self) -> bool {
        matches!(self, Self::Proceed)
    }
}

/// Why [`begin_cycle`] withheld a cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Another cycle already holds the single-flight claim.
    AlreadyRunning,
    /// The floor interval since the last cycle start has not yet elapsed.
    FloorNotElapsed,
}

/// A recorded refusal or breaker trip — the durable half of escalation. The
/// live escalation *effect* is enqueued by the caller (see the module note).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusalRecord {
    /// Stable machine kind, e.g. `circuit-breaker` or `destructive-budget`.
    pub kind: String,
    /// Human-facing detail. Already bounded by the caller; stored verbatim.
    pub detail: String,
    /// ISO-8601 instant the refusal was recorded.
    pub at: String,
}

/// The complete durable safety state for one company.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvergeSafetyState {
    /// Always [`CONVERGE_SAFETY_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Actuate against the host, or compute-only. The *stored* mode; the
    /// breaker overrides it in [`ConvergeSafetyState::effective_config`].
    pub actuation_mode: ActuationMode,
    /// Independently let the #29 pointer-sweep apply live even in shadow mode.
    pub sweep_live: bool,
    /// A durable operator override that suspends the destructive-action budget.
    pub budget_override: bool,
    /// Consecutive failed apply cycles since the last success or clear.
    pub consecutive_failures: u32,
    /// Whether the breaker has tripped the company back to shadow.
    pub breaker_tripped: bool,
    /// When the breaker last tripped, if it is tripped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breaker_tripped_at: Option<String>,
    /// Whether a cycle currently holds the single-flight claim.
    pub cycle_in_progress: bool,
    /// The wall-clock start of the most recent cycle: both the floor-spacing
    /// anchor and the staleness anchor for a held claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_started_at_ms: Option<i64>,
    /// The most recent recorded refusal or trip. Escalation audit trail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refusal: Option<RefusalRecord>,
}

impl ConvergeSafetyState {
    /// The default a fresh, never-configured company reads as: shadow, every
    /// sub-flag off, a healthy breaker, no claim held.
    #[must_use]
    pub fn default_shadow() -> Self {
        Self {
            schema_version: CONVERGE_SAFETY_SCHEMA_VERSION,
            actuation_mode: ActuationMode::Shadow,
            sweep_live: false,
            budget_override: false,
            consecutive_failures: 0,
            breaker_tripped: false,
            breaker_tripped_at: None,
            cycle_in_progress: false,
            cycle_started_at_ms: None,
            last_refusal: None,
        }
    }

    /// The operator-visible config, with the breaker folded into the mode.
    ///
    /// A tripped breaker forces [`ActuationMode::Shadow`] and disables the live
    /// sweep, so a company in trouble goes conservative everywhere, not just on
    /// the risky effects — the stored `actuation_mode`/`sweep_live` are left
    /// intact so an operator clear resumes exactly what they last chose.
    #[must_use]
    pub fn effective_config(&self) -> SafetyConfig {
        SafetyConfig {
            actuation_mode: if self.breaker_tripped {
                ActuationMode::Shadow
            } else {
                self.actuation_mode
            },
            sweep_live: self.sweep_live && !self.breaker_tripped,
            budget_override_active: self.budget_override,
        }
    }
}

/// The converge-safety store.
pub struct ConvergeSafetyStore;

/// The `documents.store` key for this store, addressable without naming the
/// sealed [`ConvergeSafetyStore`] type — fence containment
/// (`chiefd-core/tests/fence_containment.rs`) confines the type and the
/// literal to this module, and the host's cycle-refresh needs the key.
pub const STORE_NAME: &str = ConvergeSafetyStore::NAME;

impl StoreKind for ConvergeSafetyStore {
    const NAME: &'static str = "converge-safety";
    type Body = ConvergeSafetyState;
}

impl FailSafeValue for ConvergeSafetyStore {
    fn restrictive() -> Self::Body {
        // The fully-conservative value: shadow, everything off, breaker tripped.
        // A gate reading this actuates nothing and can only be lifted by an
        // explicit operator action.
        ConvergeSafetyState {
            schema_version: CONVERGE_SAFETY_SCHEMA_VERSION,
            actuation_mode: ActuationMode::Shadow,
            sweep_live: false,
            budget_override: false,
            consecutive_failures: BREAKER_TRIP_THRESHOLD,
            breaker_tripped: true,
            breaker_tripped_at: None,
            cycle_in_progress: false,
            cycle_started_at_ms: None,
            last_refusal: None,
        }
    }
}

/// Parse a stored body. An `Err` — the restrictive path — for unreadable bytes
/// or the wrong schema version, and it says which: a breaker that reads as
/// restrictive because its schema moved is a deploy, while one that reads as
/// restrictive because its bytes are damaged is an incident.
fn parse(body: &str) -> Result<ConvergeSafetyState, crate::polarity::DecodeRefusal> {
    let state: ConvergeSafetyState =
        serde_json::from_str(body).map_err(|error| format!("the body did not decode: {error}"))?;
    if state.schema_version != CONVERGE_SAFETY_SCHEMA_VERSION {
        return Err(format!(
            "the body is schema version {}, not {CONVERGE_SAFETY_SCHEMA_VERSION}",
            state.schema_version
        ));
    }
    Ok(state)
}

/// Read the durable safety state. **Total** — every failure mode is still a
/// state. An absent row is the shadow default (no warning); a present-but-
/// unreadable row is the restrictive value (with a warning).
///
/// This store already drew that line; it now draws it in the type as well.
/// The absent row answers [`Decoded::Absent`] rather than [`Decoded::Value`],
/// which carries the same body and the same silence to every caller
/// (`into_parts` is unchanged) while no longer claiming a value was decoded
/// from bytes that were never there.
#[must_use]
pub fn read(ledgers: &Ledgers) -> Decoded<ConvergeSafetyState> {
    match ledgers.document_body(ConvergeSafetyStore::NAME) {
        // Absence value: the shadow default — unchanged. A company that has
        // never tripped anything is not a company in recovery.
        None => Decoded::absent(ConvergeSafetyState::default_shadow()),
        Some(body) => decode_fail_safe_value::<ConvergeSafetyStore>(parse(body)),
    }
}

/// The current state a mutator reads, modifies and writes back. Corrupt bytes
/// contribute the restrictive value, so a mutation can never *inherit* an
/// unreadable breaker into apply — only replace it with a clean one.
fn current(ledgers: &Ledgers) -> ConvergeSafetyState {
    read(ledgers).into_parts().0
}

/// Persist a state, stamping the schema version.
fn put(ledgers: &mut Ledgers, mut state: ConvergeSafetyState) -> ConvergeSafetyState {
    state.schema_version = CONVERGE_SAFETY_SCHEMA_VERSION;
    if let Ok(encoded) = serde_json::to_string(&state) {
        ledgers.put_document(ConvergeSafetyStore::NAME, encoded);
    }
    state
}

/// Has this company's safety state ever been written?
///
/// [`read`] deliberately cannot answer this: it folds absence into the shadow
/// default, which is right for every gate that only needs to know what is in
/// force. Exactly one caller needs the other question — `chiefd run`'s boot
/// actuation write, which must adopt apply for a company nobody has configured
/// and must NOT overwrite a company somebody has. Without this distinction that
/// write had to hardcode a mode, and hardcoding it made
/// `chiefd set-actuation-config --mode shadow` a setting that survived only
/// until the next daemon start.
#[must_use]
pub fn configured(ledgers: &Ledgers) -> bool {
    ledgers.document_body(ConvergeSafetyStore::NAME).is_some()
}

/// Drop the safety state entirely. Returns whether a row was present. Absence
/// reads as the shadow default, so clearing is always safe (never opens apply).
pub fn clear(ledgers: &mut Ledgers) -> bool {
    ledgers.remove_document(ConvergeSafetyStore::NAME)
}

// TOMBSTONE: `destructive_budget` and `MIN_DESTRUCTIVE_BUDGET`.
//
// The restart budget bounded how many live, still-desired processes ONE PASS
// could replace, at `max(2, 25% of population)`. It was enforced in the action
// planner, which deferred the excess rather than refusing the pass.
//
// It dies with the planner, because a budget is a property of an ACTION STREAM
// and there is no longer an action stream to bound. chiefd publishes the
// desired set; the actuator computes which panes are stale and replaces them.
// If replacing a whole fleet at once needs bounding, that bound belongs in the
// actuator for exactly the reason the start cap's does: it is a statement about
// what one machine can absorb, made where the machine is.
//
// The operator's `budget_override_active` lever goes with it. Note what that
// lever's history says about keeping an unenforced number here: it was stored,
// settable through `chiefd set-actuation-config`, surfaced on `SafetyConfig` --
// and read by nothing, so an operator could set it and believe they were
// covered. An inert safety lever is worse than no lever at all.

// ---------------------------------------------------------------------------
// Durable mutators: config, breaker, single-flight + floor, escalation record.
// ---------------------------------------------------------------------------

/// Set the operator-facing actuation config, and **resume** the breaker.
///
/// A config change is one of the two documented resume paths (the other is
/// [`operator_clear_breaker`]), so this clears the consecutive-failure counter
/// and any trip. The whole config is specified by the caller, so nothing unsafe
/// is inherited even when it writes over a corrupt row.
pub fn set_actuation_config(
    ledgers: &mut Ledgers,
    mode: ActuationMode,
    sweep_live: bool,
    budget_override: bool,
) -> ConvergeSafetyState {
    let mut state = current(ledgers);
    state.actuation_mode = mode;
    state.sweep_live = sweep_live;
    state.budget_override = budget_override;
    state.consecutive_failures = 0;
    state.breaker_tripped = false;
    state.breaker_tripped_at = None;
    put(ledgers, state)
}

/// Fold one apply-cycle outcome into the breaker counter.
///
/// A success resets the counter. A failure advances it, and the
/// [`BREAKER_TRIP_THRESHOLD`]-th consecutive failure trips the breaker and
/// returns [`BreakerAction::Tripped`] — the one signal on which the caller
/// escalates. Success never *un-trips*: once tripped, only an explicit clear or
/// a config change resumes, because a tripped company runs no apply cycles.
pub fn record_cycle_outcome(ledgers: &mut Ledgers, cycle_succeeded: bool) -> BreakerAction {
    let mut state = current(ledgers);
    if cycle_succeeded {
        state.consecutive_failures = 0;
        put(ledgers, state);
        return BreakerAction::Continue;
    }
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    let newly_tripped =
        state.consecutive_failures >= BREAKER_TRIP_THRESHOLD && !state.breaker_tripped;
    if newly_tripped {
        let at = iso_millis(ledgers.now().0);
        state.breaker_tripped = true;
        state.breaker_tripped_at = Some(at.clone());
        state.last_refusal = Some(RefusalRecord {
            kind: "circuit-breaker".to_string(),
            detail: format!(
                "{} consecutive apply-cycle failures; company dropped to shadow",
                state.consecutive_failures
            ),
            at,
        });
    }
    put(ledgers, state);
    if newly_tripped {
        BreakerAction::Tripped
    } else {
        BreakerAction::Continue
    }
}

/// The explicit operator acknowledgement that resumes a tripped breaker.
///
/// Resets the counter and the trip. The `last_refusal`/`tripped_at` trail is left
/// intact for audit until the next event overwrites it.
pub fn operator_clear_breaker(ledgers: &mut Ledgers) -> ConvergeSafetyState {
    let mut state = current(ledgers);
    state.consecutive_failures = 0;
    state.breaker_tripped = false;
    state.breaker_tripped_at = None;
    put(ledgers, state)
}

/// Take the single-flight claim for one apply cycle, subject to the floor.
///
/// Called inside one writer mutation, so the read-check-write is atomic against
/// every other mutation for this company: a second concurrent `begin_cycle` sees
/// the claim and is [`CycleGate::Skipped`]. A claim older than [`CLAIM_STALE_MS`]
/// is crash residue and is reclaimed. `floor_interval_ms` is the minimum spacing
/// between cycle *starts*.
pub fn begin_cycle(ledgers: &mut Ledgers, floor_interval_ms: i64) -> CycleGate {
    let now = ledgers.now().0;
    let mut state = current(ledgers);

    // A held-and-fresh claim means a cycle is genuinely running: single-flight.
    let claim_fresh = state
        .cycle_started_at_ms
        .is_none_or(|started| now.saturating_sub(started) < CLAIM_STALE_MS);
    if state.cycle_in_progress && claim_fresh {
        return CycleGate::Skipped(SkipReason::AlreadyRunning);
    }

    // Floor spacing measures from the most recent start, in progress or not.
    if let Some(started) = state.cycle_started_at_ms {
        if now.saturating_sub(started) < floor_interval_ms {
            return CycleGate::Skipped(SkipReason::FloorNotElapsed);
        }
    }

    state.cycle_in_progress = true;
    state.cycle_started_at_ms = Some(now);
    put(ledgers, state);
    CycleGate::Proceed
}

/// Release the single-flight claim. The start stamp is kept for floor spacing.
pub fn end_cycle(ledgers: &mut Ledgers) -> ConvergeSafetyState {
    let mut state = current(ledgers);
    state.cycle_in_progress = false;
    put(ledgers, state)
}

/// Record a refusal for audit and (caller-side) escalation. Returns the state.
pub fn record_refusal(
    ledgers: &mut Ledgers,
    kind: impl Into<String>,
    detail: impl Into<String>,
) -> ConvergeSafetyState {
    let mut state = current(ledgers);
    state.last_refusal = Some(RefusalRecord {
        kind: kind.into(),
        detail: detail.into(),
        at: iso_millis(ledgers.now().0),
    });
    put(ledgers, state)
}

#[cfg(test)]
mod tests;
