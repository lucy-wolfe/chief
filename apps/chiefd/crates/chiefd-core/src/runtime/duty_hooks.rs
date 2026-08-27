//! Injected host-side seams for the one-daemon duty scheduler (`chiefd run`).
//!
//! # Why these traits exist, and where the line is
//!
//! The daemon loop (`chiefd/src/run`) drives the five [`Duty`] variants
//! (`crate::store::supervisor_watermark::Duty`) at their cadences. Each duty is
//! two phases with a hard boundary between them:
//!
//! 1. **Host observation / effect** — reads the runtime, sends an
//!    envelope. This is I/O, it can block or fail, and
//!    `clippy.toml` forbids it inside `chiefd-core`. It happens OUTSIDE the
//!    writer transaction, in one of the async hooks below.
//! 2. **Durable mutation** — the pure core (`supervision::cycle`,
//!    `health_collect::collect`, `evaluate_due_work`, …) run on the company
//!    writer thread inside one `BEGIN IMMEDIATE … COMMIT`, with
//!    `supervisor_watermark::record_success` folded into that *same* commit.
//!
//! These traits are phase 1. They are declared here, in `chiefd-core`, because
//! all three parties must see the same contract: `chiefd-core` owns the return
//! *types* (they are this crate's store types), `chiefd-host` owns the concrete
//! implementations that touch runtime / the network, and the `chiefd` binary owns
//! the scheduler that injects them plus the no-op stubs its tests run against.
//! `chiefd-host` depends on `chiefd-core`; the binary depends on both — so a
//! trait any of them could implement or consume can only live here.
//!
//! # Object safety via boxed futures
//!
//! Every hook is held as `Arc<dyn Trait>` and injected by constructor. Native
//! `async fn` in traits is not `dyn`-compatible, so each method returns a
//! [`BoxFuture`] explicitly — the exact pattern [`crate::clock::Clock::sleep`]
//! already uses for the one other boxed-future seam in the crate.
//!
//! # These are drafts pinned to real core signatures
//!
//! Nothing here is speculative shape: `CycleInput`, `HealthCollectionSnapshot`,
//! `DispatchPlan` and `PollOutcome` are the
//! literal inputs/outputs of the pure cores the loop calls, so a concrete impl
//! that satisfies one of these traits already produces exactly what the writer
//! phase feeds to the core with no adapter.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::ledger::LedgerSnapshot;
use crate::store::health_collect::HealthCollectionSnapshot;
use crate::store::supervision::{CycleInput, DispatchFailure};

/// A host hook's in-flight work. Boxed and `Send + 'static` so the trait stays
/// object-safe and the future can be awaited on any scheduler task.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A transient, non-fatal host failure from a duty hook.
///
/// A hook failing is not a reason to stop the loop — it is one skipped pass,
/// logged, exactly as `chiefd observe` treats a read error today. The duty's
/// watermark simply does not advance this tick, so a *persistent* hook failure
/// surfaces as a stalled duty through the self-audit, which is the correct
/// escalation path rather than a crashed daemon.
#[derive(Debug, Clone)]
pub struct DutyError {
    /// A bounded, log-safe description of what the host hook could not do.
    pub detail: String,
}

impl DutyError {
    /// Build a duty error from anything displayable.
    pub fn new(detail: impl Into<String>) -> Self {
        Self { detail: detail.into() }
    }
}

impl std::fmt::Display for DutyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for DutyError {}

/// Read-only context every host hook receives for one duty pass.
///
/// It carries the company slug and the last *committed* durable snapshot, so a
/// gatherer can read the manifest, the supervision ledger, the health state,
/// etc. without a second connection and without racing the writer — the
/// snapshot is at most one in-flight mutation stale, the same guarantee
/// [`crate::actor::CompanyDb::read`] gives every other reader.
#[derive(Clone)]
pub struct DutyContext {
    /// The company slug (`CompanyDb::label`).
    pub slug: String,
    /// The last committed durable state.
    pub snapshot: Arc<LedgerSnapshot>,
}

/// Whether the runtime-actuation half of a reconcile tick may touch the runtime.
///
/// Defaults to [`ActuationMode::Shadow`] per M2's safety posture: compute the
/// plan and log it, mutate nothing on the host. `Apply` is opt-in per company
/// and the scheduler must never assume it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActuationMode {
    /// Compute the converge plan and log it; touch no pane, spawn no pi.
    #[default]
    Shadow,
    /// Actuate the converge plan against the live runtime.
    Apply,
}

// ---------------------------------------------------------------------------
// Duty #1 — SupervisionReconcile, phase-1 observation for `supervision::cycle`.
// ---------------------------------------------------------------------------

/// Gathers the host observation `supervision::cycle` consumes.
///
/// Everything only the host can know for one D9 cycle: the fleet-suppression
/// verdict, the ownership identity, the fast-health unhealthy set, the runtime
/// audit, and the observed projection — assembled into the core's own
/// [`CycleInput`]. Read-only; it mutates nothing. Owner: `od-host-gatherers`.
pub trait CycleInputGatherer: Send + Sync + 'static {
    /// Observe the host and assemble one cycle's [`CycleInput`].
    fn gather_cycle_input(&self, ctx: &DutyContext)
        -> BoxFuture<'_, Result<CycleInput, DutyError>>;
}

/// The runtime-actuation half of a SupervisionReconcile tick — M2's
/// `reconcile_cycle`, wrapped as an injected seam.
///
/// Runs *after* `supervision::cycle` has committed this tick's ledger state,
/// against `ctx.snapshot` refreshed to that post-cycle commit. It projects the
/// just-committed manifest + activity ledger into a desired runtime topology,
/// observes the live topology, computes the converge plan, and — only in
/// [`ActuationMode::Apply`] — actuates it. The #29 pointer-sweep
/// compare-and-clear it owns is its own single ledger mutation on the same
/// writer, sequenced first inside this call. Owner: M2 (units A+B, m2-impl).
pub trait ReconcileActuator: Send + Sync + 'static {
    /// Converge runtime toward the committed ledger state, honoring `mode`.
    fn reconcile(
        &self,
        ctx: &DutyContext,
        mode: ActuationMode,
    ) -> BoxFuture<'_, Result<ReconcileReport, DutyError>>;
}

/// A bounded report of one runtime-actuation pass. Shape owned by M2; this is the
/// minimum the scheduler logs. M2 may widen it.
#[derive(Debug, Clone, Default)]
pub struct ReconcileReport {
    /// The mode the pass actually ran in.
    pub applied: bool,
    /// How many people this pass DESIRES running.
    ///
    /// It was `planned_steps`, a count of the per-person `Start`/`Kill`/
    /// `Restart` actions chiefd emitted. chiefd emits no actions: a verb is a
    /// statement about a TRANSITION, and only something that can see the
    /// current state may compute one. Renamed rather than reinterpreted,
    /// because the two counts differ in every case that matters -- a steady
    /// company of four people plans ZERO steps and desires FOUR -- and a field
    /// whose meaning changes while its name does not is read wrongly by
    /// everybody who does not happen to re-derive it.
    pub desired_people: usize,
    /// Whether this pass recorded anything NEW: a changed desired set, or a
    /// live pointer sweep.
    ///
    /// #367's "silent at idle" property needs this and can no longer be spelled
    /// any other way. It used to be `planned_steps == 0` — no actions emitted,
    /// which a converged company satisfied every pass. Under a desired SET a
    /// live company always desires somebody and always names them, so both the
    /// count and the notes are permanently non-empty and neither can say
    /// "nothing happened".
    pub changed: bool,
    /// Whether this pass made a LAUNCH DECISION an operator must be able to
    /// read back: it granted launch intent, withdrew it, or held demand it
    /// refused to desire.
    ///
    /// # Why this is not [`Self::changed`]
    ///
    /// `changed` asks "does this pass's audit body differ from the last
    /// committed one" — an audit-identity question, derived entirely from the
    /// desired SET plus two safety flags. A launch decision is a different
    /// question and the two come apart in exactly the case that matters. A
    /// wake grant, a settle withdrawal and a refused mail demand all produce
    /// notes on the pass that makes them, and all three can leave the desired
    /// set — and therefore the audit body — identical. The pass then took the
    /// no-op arm and its notes went to DEBUG.
    ///
    /// That is how a live company relaunched six people with `daemon.log`
    /// holding nothing but `supervision cycle committed`: the record of WHO was
    /// launched and WHY existed, on the report, and was logged at a level
    /// nobody runs at.
    ///
    /// `launching: <names>` is not a substitute. It names the whole desired set
    /// on every pass, so it is silent precisely when the set is steady — which
    /// is the state almost every wake lands next to.
    pub actuation_record: bool,
    // TOMBSTONE: `actuated_steps`. "Count of steps actuated (zero in shadow
    // mode)", and it was zero in EVERY mode: chiefd emits no actions and
    // applies none, so the number it could honestly report was always 0. It was
    // logged on every pass, as `actuated=0` beside a `planned=N` that N never
    // agreed with, on a live company across passes where people demonstrably
    // came up -- so the one line an operator reads to judge a pass said, once a
    // second, that nothing was happening while everything was. The rule is
    // already written down one field below and was simply not applied here: a
    // field permanently zero is worse than no field. How many actions were
    // actually applied is the CLIENT's count, and it arrives on its next
    // observed-runtime POST; nothing on this side may invent it.
    // TOMBSTONE: `deferred_starts`. It counted start-actuations this pass capped
    // and deferred to a follow-up by the #431 start budget, and the scheduler
    // armed a floor-delayed reactive follow-up when it was non-zero. There is no
    // cap, so nothing is ever deferred, so no follow-up is ever owed for that
    // reason -- the ramp is deleted by operator ruling and every missing pane is
    // created in the pass that finds it. A field permanently zero is worse than
    // no field: a reader branching on it would arm a follow-up that never comes
    // or, more likely, conclude a drained ramp that never existed.
    /// A reactive request arrived inside the reconcile single-flight floor.
    ///
    /// The scheduler must coalesce and replay that request after the floor
    /// instead of dropping a durable intent until its slow fallback cadence.
    pub retry_after_floor: bool,
    /// Human-readable notes, joined into a single log line by the caller.
    ///
    /// #107: nothing here enforces a bound — the field name once claimed one and
    /// no `truncate`/`dedup`/length check exists at any push site. Each producer
    /// is therefore responsible for capping its own contribution (see
    /// `converge_apply::cycle::MAX_NAMED_LAUNCH_SUBJECTS`), and a capped note
    /// must say so rather than truncating silently.
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Duty #2 — HealthMonitor, phase-1 observation for `health_collect::collect`.
// ---------------------------------------------------------------------------

/// Gathers the [`HealthCollectionSnapshot`] the health monitor folds into
/// incident candidates. Read-only. Owner: `od-host-gatherers`.
pub trait HealthSnapshotGatherer: Send + Sync + 'static {
    /// Observe supervisor and runtime liveness into one snapshot.
    fn gather_health(
        &self,
        ctx: &DutyContext,
    ) -> BoxFuture<'_, Result<HealthCollectionSnapshot, DutyError>>;
}

// ---------------------------------------------------------------------------
// Duty #3 — MailboxWake, effect delivery for `delivery::dispatch_plan`.
// ---------------------------------------------------------------------------

/// One pending effect handed to the sink for out-of-band dispatch.
///
/// The scheduler builds these from the committed snapshot in the order
/// [`crate::store::supervision::dispatch_plan`] returns (fences first, then
/// urgent, then routine), so the sink never re-derives ordering or eligibility.
#[derive(Debug, Clone)]
pub struct EffectEnvelope {
    /// The durable effect id — the exactly-once key and the token the writer
    /// phase passes to `mark_delivered` / `record_delivery_failure`.
    pub id: String,
    /// The effect kind (`assignment_delivery`, `manager_goal_watch`, …).
    pub kind: String,
    /// The effect payload to render and route.
    pub payload: serde_json::Value,
}

/// What one delivery pass achieved, by effect id.
#[derive(Debug, Clone, Default)]
pub struct DeliveryOutcome {
    /// Effects dispatched successfully — fed to `mark_delivered`.
    pub delivered: Vec<String>,
    /// Effects that failed this pass, each WITH the reason it failed — fed to
    /// `record_delivery_failure` by id.
    ///
    /// The reason is carried across this seam rather than logged where it
    /// arises: the sink's writer phase runs inside the pure core, and the
    /// scheduler is the boundary that already reports a pass.
    pub failed: Vec<DispatchFailure>,
}

/// Dispatches pending supervision effects to their panes / mailboxes.
///
/// Delivery is a host effect and lives entirely off the writer thread; the
/// scheduler commits the *result* (`mark_delivered` / `record_delivery_failure`)
/// afterward. Idempotent per id: redelivering an already-delivered effect is a
/// no-op success, because the writer may re-present an effect a prior pass sent
/// but crashed before recording. Owner: `od-delivery-mailbox`.
pub trait DeliverySink: Send + Sync + 'static {
    /// Dispatch `envelopes` (already ordered) and report per-id outcomes.
    fn deliver(
        &self,
        ctx: &DutyContext,
        envelopes: Vec<EffectEnvelope>,
    ) -> BoxFuture<'_, DeliveryOutcome>;
}
