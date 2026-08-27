//! Supervision: the reminder schedule and the durable effect outbox.
//!
//! This module is the durable supervision state itself — the reminders a person
//! armed for themselves, and the effect rows that carry a due reminder (or a
//! converge-actuation operator escalation) to a mailbox.
//!
//! `effects` are relational ROWS on `CompanyDb`, with `next_effect_sequence` as
//! an explicit `counters` row; everything else stays one document. An effect is
//! read, delivered and retired on the hot path, so it earns a table rather than
//! a whole-document rewrite per fire.
//!
//! Absence is fail-closed (`FailClosed`): the cost of reading an unreadable
//! supervision ledger as "empty" is every effect sequence reset to 1, which a
//! monotonicity check then refuses for the life of the company.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::Refusal;
use crate::error::{corrupt_store, store_failure};
use crate::isotime::{iso_millis, parse_iso_millis};
use crate::ledger::{EffectRow, Ledgers, NEXT_EFFECT_SEQUENCE};
use crate::polarity::{FailClosed, StoreKind};
use crate::store::organization::{EmploymentState, OrganizationManifest};
use crate::ChiefdError;

/// Schema version of the residual document body.
pub const SUPERVISION_SCHEMA_VERSION: u32 = 2;

/// The shortest delay any reminder may be armed at: one minute.
///
/// This is a PERFORMANCE fence, not a taste preference. Every fire is a commit,
/// an effect row, a mailbox delivery, and — for a stopped person — a whole agent
/// brought up. A reminder armed at seconds would be a poller wearing a
/// reminder's clothes, which is precisely the defect the repository's
/// reactive-never-polling rule exists to forbid, and it would do it while
/// holding the fleet open against THE HARD RULE.
///
/// **A RECURRING reminder must clear a higher bar** — see
/// [`MIN_RECURRING_REMINDER_INTERVAL_MS`], which is the floor a cadence has to
/// pass. This one is the delay floor that applies to every reminder, including
/// a one-shot, whose `interval_ms` is a delay rather than a cadence.
pub const MIN_REMINDER_INTERVAL_MS: i64 = 60 * 1_000;

/// The shortest cadence a RECURRING reminder may be armed at: **twice the
/// settle window**, derived from it rather than written down beside it.
///
/// # Why recurrence is the scope, and a one-shot keeps the smaller fence
///
/// The hazard above is a countdown that is RESET before it can finish. One fire
/// cannot do that: a one-shot delivers its turn, the person settles from the
/// last beat, and parks. `interval_ms` on a one-shot is a DELAY — "remind me in
/// two minutes" — and there is no cadence to be inside the window. Applying the
/// recurring floor there would forbid a harmless, useful request in the name of
/// a hazard that cannot occur in it, and [`MIN_REMINDER_INTERVAL_MS`] already
/// stops a one-shot being used as a seconds-scale trigger.
///
/// A RECURRING reminder is the whole defect: every fire delivers a turn, every
/// turn resets the countdown, so a cadence inside the window makes parking
/// unreachable and the person stays resident for ever, burning tokens, out of
/// entirely legal inputs.
///
/// # The measured incident, and why the floor is DERIVED
///
/// The recurring floor did not exist: one constant, `60 * 1_000`, governed
/// both shapes, while
/// [`ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS`] was `300 * 1_000`. A settle
/// countdown must run the whole lease UNINTERRUPTED before a person parks, and
/// every reminder fire delivers a turn, and every turn resets the countdown.
/// So a reminder armed at any legal value below the lease made parking
/// **unreachable**: the person stayed resident for ever, burning tokens, out of
/// entirely legal inputs. Measured on a live company on 2026-08-27 — a person
/// woken about once a minute, each turn correctly deciding that the standing
/// work needed no action, $2.295 spent deciding nothing, and the fleet held
/// open the whole time. The doc comment above already named that hazard
/// exactly; the number was five times too small for its own sentence.
///
/// # Why the floor is DERIVED, and why 2×
///
/// Derived because a floor written as its own literal is a second copy of a
/// relationship, and the two drift the moment either side moves — the lease
/// went 2 minutes to 5 by operator ruling and this floor did not follow,
/// because nothing said it had to. Now it must.
///
/// **2×, so that a whole park FITS BETWEEN FIRES with room for beat jitter.**
/// At exactly one lease the fire races the park and the outcome depends on
/// scheduling luck. At twice it, the person settles, parks at the lease, and
/// the next fire arrives at a parked person and WAKES them — which is the
/// designed mailbox-wake path, not a defect. The multiple is a judgement; the
/// RELATION is not, and `the_reminder_floor_lets_a_full_park_fit_between_fires`
/// pins it so neither constant can move alone.
pub const MIN_RECURRING_REMINDER_INTERVAL_MS: i64 =
    2 * crate::store::activity::ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS;

/// Bound on a reminder's prompt.
pub const REMINDER_PROMPT_LIMIT: usize = 2_000;

/// How many reminders one person may have armed at once.
///
/// Bounded for the same reason as the cadence floor: the supervision document is
/// rewritten whole on every mutation (#123), so an unbounded reminder list is an
/// unbounded per-commit cost paid by every other duty in the company.
pub const REMINDERS_PER_PERSON_LIMIT: usize = 16;

// --- refusal codes ------------------------------------------------------

/// A field was missing, empty, or over its bound.
pub const INVALID_INPUT: &str = "invalid-input";
/// The named person is not in the manifest.
pub const UNKNOWN_PERSON: &str = "unknown-person";
/// The person is not active.
pub const PERSON_NOT_ACTIVE: &str = "person-not-active";
/// Two effects were queued with the same id and different content.
pub const EFFECT_CONTENT_CONFLICT: &str = "effect-content-conflict";
/// The effect kind for a converge-actuation operator escalation (a tripped
/// circuit breaker or a refused destructive budget). Delivered as an ordinary
/// envelope; the durable audit trail is `store::converge_safety`'s.
pub const RECONCILE_ESCALATION_EFFECT_KIND: &str = "reconcile_escalation";
/// The store body could not be encoded.
pub const LEDGER_UNSERIALIZABLE: &str = "supervision-unserializable";
/// A row the same mutation had just resolved was gone. Always a chiefd bug.
pub const INTERNAL_INCONSISTENCY: &str = "supervision-internal-inconsistency";

/// The supervision store marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisionStore;

impl StoreKind for SupervisionStore {
    const NAME: &'static str = "supervision";
    type Body = SupervisionLedger;
}

impl FailClosed for SupervisionStore {}

// --- record types -------------------------------------------------------

/// Where an effect is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectStatus {
    /// Queued, not yet dispatched.
    Pending,
    /// Dispatched successfully.
    Delivered,
    /// Overtaken by a later effect; never dispatched.
    Superseded,
    /// Dispatch gave up.
    Failed,
}

impl EffectStatus {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Superseded => "superseded",
            Self::Failed => "failed",
        }
    }
}

/// One durable effect. The payload varies by `kind`; the fields chiefd fences
/// on are common.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effect {
    /// Deterministic effect id; the exactly-once key.
    pub id: String,
    /// Monotone sequence drawn from `next_effect_sequence`.
    pub sequence: u64,
    /// Effect kind (`person_reminder`, `reconcile_escalation`, …).
    #[serde(rename = "type")]
    pub kind: String,
    /// Lifecycle position.
    pub status: EffectStatus,
    /// ISO-8601 creation stamp.
    pub created_at: String,
    /// When it was dispatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    /// When it was overtaken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<String>,
    /// Dispatch failures recorded since the last reset. Absent ⇒ 0 (TS `?? 0`).
    /// At [`super::delivery::SUPERVISION_EFFECT_DELIVERY_ATTEMPT_LIMIT`] the
    /// effect trips to `failed` — a breaker with no half-open state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_failure_count: Option<u32>,
    /// When the last dispatch failure was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery_failure_at: Option<String>,
    /// When the breaker tripped this effect to `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<String>,
    /// Operator reopens consumed. Absent ⇒ 0; bounded by
    /// [`super::delivery::SUPERVISION_EFFECT_REOPEN_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reopen_count: Option<u32>,
    /// When an operator last reopened this effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reopened_at: Option<String>,
    /// Everything kind-specific. Kept as a map so an effect kind M15 owns
    /// round-trips through M12 unchanged.
    #[serde(flatten)]
    pub payload: BTreeMap<String, serde_json::Value>,
}

impl Effect {
    /// A payload field as a string.
    #[must_use]
    pub fn text(&self, key: &str) -> Option<&str> {
        self.payload.get(key).and_then(serde_json::Value::as_str)
    }

    /// A payload field as an integer.
    #[must_use]
    pub fn number(&self, key: &str) -> Option<i64> {
        self.payload.get(key).and_then(serde_json::Value::as_i64)
    }

    /// The content-comparable half: everything except the fields the queue
    /// itself owns. Two `enqueue` calls with the same id must agree on this.
    fn comparable(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.payload
    }
}

/// A durable, org-owned recurring reminder — the recurring wake-up an agent
/// arms for itself.
///
/// # Not a lease
///
/// `keeps_person_alive` has no counterpart here on purpose. A reminder must
/// never hold a person resident merely by existing (THE HARD RULE); it is a
/// row that comes due, enqueues one effect, and re-arms. Bringing the person up
/// is the mailbox's decision, made because work arrived — not this row's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    /// Stable id, unique within the company.
    pub id: String,
    /// Who is reminded. Always the person, never a manager-only concept: a
    /// worker may remind themselves.
    pub person_id: String,
    /// Who armed it — the person themselves, or a manager arming it for them.
    pub created_by_person_id: String,
    /// The text delivered when it fires.
    pub prompt: String,
    /// The cadence in milliseconds.
    pub interval_ms: i64,
    /// When it next fires (ISO-8601).
    pub next_due_at: String,
    /// `active` | `stopped`.
    pub status: String,
    /// False for a one-shot: it fires once and then goes `stopped`.
    pub recurring: bool,
    /// How many times it has fired.
    #[serde(default)]
    pub fire_count: u64,
    /// ISO-8601 creation stamp.
    pub created_at: String,
    /// ISO-8601 stamp of the last fire, absent until it first fires.
    ///
    /// Present for the same reason `advance_check_in` stamps `lastEvaluatedAt`
    /// (#70): without it, `next_due_at` is the only observable, and a reminder
    /// that was NEVER EVALUATED is indistinguishable from one that was
    /// evaluated and re-armed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<String>,
    /// Optional ISO-8601 expiry; past it the reminder stops re-arming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Why the reminder left `active` — `expired` | `fired` | `stopped`.
    ///
    /// PROMOTED from `extra` (org-data-normalization P0 N3): maps to
    /// reminders.stopped_reason (CHECK'd). Absent while active; serde-flatten
    /// kept the wire bytes identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_reason: Option<String>,
    /// ISO-8601 of the stop. PROMOTED from `extra`; maps to reminders.stopped_at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    /// Everything else, preserved verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Reminder {
    /// Whether this reminder is armed at `now`: active, and not past expiry.
    #[must_use]
    pub fn is_armed(&self, now: i64) -> bool {
        if self.status != "active" {
            return false;
        }
        // An unparseable expiry fails CLOSED — a reminder whose expiry we cannot
        // read must not fire forever. `validate` rejects one on the way in, so
        // this is defence against a hand-edited row, not the normal path.
        match self.expires_at.as_deref() {
            None => true,
            Some(stamp) => parse_iso_millis(stamp).is_some_and(|expiry| expiry > now),
        }
    }
}

/// The residual supervision document: everything that is not a hot row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisionLedger {
    /// Always [`SUPERVISION_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The company slug this ledger belongs to.
    pub organization: String,
    /// Durable recurring reminders, in creation order.
    ///
    /// `default` + `skip_serializing_if` so every supervision document written
    /// before reminders existed still decodes, and a company with none adds no
    /// bytes to a row every mutation already pays to rewrite whole (#123).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reminder_order: Vec<String>,
    /// Reminders by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reminders: BTreeMap<String, Reminder>,
    /// ISO-8601 creation stamp.
    pub created_at: String,
    /// ISO-8601 stamp of the last write.
    pub updated_at: String,
    /// The relational half, projected in for readers. Never serialized: the
    /// rows are the authority and duplicating them into the body would create
    /// exactly the two-writers-one-fact problem this project exists to delete.
    #[serde(skip)]
    effects: BTreeMap<String, Effect>,
    /// As `effects`.
    #[serde(skip)]
    effect_order: Vec<String>,
    /// As `effects`.
    #[serde(skip)]
    next_effect_sequence: u64,
}

impl SupervisionLedger {
    /// The seed ledger for a freshly created company.
    #[must_use]
    pub fn initial(manifest: &OrganizationManifest, now: &str) -> Self {
        Self {
            schema_version: SUPERVISION_SCHEMA_VERSION,
            organization: manifest.slug.clone(),
            // A fresh company arms nobody. THE HARD RULE applies to reminders
            // exactly as it applies to people: a seeded reminder is a schedule
            // nobody asked for, and every fire would be work nobody requested.
            reminder_order: Vec::new(),
            reminders: BTreeMap::new(),
            created_at: now.to_string(),
            updated_at: now.to_string(),
            effects: BTreeMap::new(),
            effect_order: Vec::new(),
            next_effect_sequence: 1,
        }
    }

    /// One effect by id.
    #[must_use]
    pub fn effect(&self, id: &str) -> Option<&Effect> {
        self.effects.get(id)
    }

    /// Effect ids in sequence order — the port of `effectOrder`.
    #[must_use]
    pub fn effect_order(&self) -> &[String] {
        &self.effect_order
    }

    /// The next sequence the effect queue will issue.
    #[must_use]
    pub fn next_effect_sequence(&self) -> u64 {
        self.next_effect_sequence
    }
}

fn invalid(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal::new(code, message)
}

// --- durable read / mutate ----------------------------------------------

/// Project the relational rows into a decoded document body.
fn hydrate(ledger: &mut SupervisionLedger, ledgers: &Ledgers) -> Result<(), Refusal> {
    ledger.effect_order = ledgers.effect_order().into_iter().map(ToString::to_string).collect();
    ledger.effects = ledger
        .effect_order
        .iter()
        .map(|id| {
            let row = ledgers
                .effect(id)
                .ok_or_else(|| invalid(INVALID_INPUT, format!("effect row '{id}' vanished")))?;
            let effect: Effect = serde_json::from_str(&row.body).map_err(|error| {
                invalid(INVALID_INPUT, format!("effect '{id}' does not decode: {error}"))
            })?;
            Ok((id.clone(), effect))
        })
        .collect::<Result<_, Refusal>>()?;
    ledger.next_effect_sequence =
        u64::try_from(ledgers.counter(NEXT_EFFECT_SEQUENCE)).unwrap_or(1).max(1);
    Ok(())
}

/// #372: whether `store` names THIS store's own documents key -- the
/// sanctioned way for a cross-cutting caller (`chiefd-api`'s docstore
/// router, serving `/v1/docs/read`'s live-supervision special case) to
/// recognize "the supervision store" without naming the literal key or this
/// module's store type outside this file, both of which
/// `chiefd-core/tests/fence_containment.rs`'s
/// `no_source_outside_a_stores_own_module_can_name_its_{documents_key,store_type}`
/// guards fence shut for every other caller. A thin `bool` function is the
/// whole point: it answers the one question a cross-cutting caller needs
/// ("is this the supervision store?") without handing that caller anything
/// that could read or write a row bypassing this store's own typed
/// accessors -- the exact bypass those guards exist to prevent.
#[must_use]
pub fn is_supervision_store(store: &str) -> bool {
    store == SupervisionStore::NAME
}

/// #127: insert-if-absent for the supervision ledger, as a typed store
/// operation — the sibling of `organization::create_if_absent`.
///
/// Projects the relational sub-data (effects) the same way
/// `bootstrap-store` does, because a document that exists without its
/// relational rows is not a seeded ledger; that half-landing is what surfaces
/// later as `corrupt store: supervision` (#55).
///
/// Returns whether this call created it. Presence check and seed share the
/// caller's one transaction.
///
/// # Errors
/// Whatever [`seed_relational_from_document`] refuses for a malformed body.
pub fn create_if_absent(ledgers: &mut Ledgers, body: &str) -> Result<bool, ChiefdError> {
    if ledgers.document_body(SupervisionStore::NAME).is_some() {
        return Ok(false);
    }
    seed_relational_from_document(ledgers, body)?;
    ledgers.put_document(SupervisionStore::NAME, body.to_string());
    Ok(true)
}

/// Read the ledger, hydrated with its relational rows.
///
/// # Errors
/// `Corrupt{store:"supervision"}` when the row does not decode or its
/// relational rows do not hydrate, and `StoreFailure{store:"supervision"}` when
/// it decodes and then does not validate. Absence is its own kind (#105 —
/// `Absent`), because `createOrganization` seeds this ledger.
pub fn read(
    ledgers: &Ledgers,
    manifest: &OrganizationManifest,
) -> Result<SupervisionLedger, ChiefdError> {
    let Some(body) = ledgers.document_body(SupervisionStore::NAME) else {
        // #105: the document has never been written — absent, not damaged.
        // Reporting `Corrupt` here sent operators hunting for bytes that were
        // never there, and made a fresh company refuse every duty while the
        // daemon exited reporting success. Callers decide what absence means;
        // it must never silently become "empty".
        return Err(ChiefdError::Absent { store: SupervisionStore::NAME });
    };
    let mut ledger = serde_json::from_str::<SupervisionLedger>(body)
        .map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    hydrate(&mut ledger, ledgers).map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    validate(&ledger, manifest).map_err(|e| store_failure(SupervisionStore::NAME, e))?;
    Ok(ledger)
}

/// Read for the reconcile CYCLE's gather, healing the protected roster IN MEMORY
/// before validating — exactly as the writer (`read_for_mutation` -> `mutate`)
/// does, and unlike the strict [`read`] which returns the ledger as stored
/// (#442: read never reconciles; only a write does).
///
/// This exists because the gather that feeds the reconcile cycle
/// (`cycle_input`, `plan_cycle`, `health_snapshot`) must not wedge on a purely
/// reconcilable staffing drift. Removing a contract/department leaves the ledger
/// still naming a person the manifest no longer has — as the owner of an
/// effect or a reminder — which is precisely what
/// `validate` rejects. Reading the cycle input through the strict [`read`] made
/// the very cycle that would repair the drift (and re-attribute the orphaned
/// goals to the executive) unreachable, so the store stayed "corrupt" forever on
/// a state a single cycle would have healed: the departed-manager trap, live on
/// cobalt. Healing the snapshot here is non-mutating — the durable repair still
/// lands when the cycle writes — and the returned ledger is what the plan should
/// see: the roster as it actually is now.
///
/// # Errors
/// `Absent` / `Corrupt` / `StoreFailure` on the same conditions as [`read`]; the reconcile only
/// drops what the manifest no longer has, so any GENUINELY structural corruption
/// still fails `validate`.
pub fn read_reconciled(
    ledgers: &Ledgers,
    manifest: &OrganizationManifest,
) -> Result<SupervisionLedger, ChiefdError> {
    let Some(body) = ledgers.document_body(SupervisionStore::NAME) else {
        return Err(ChiefdError::Absent { store: SupervisionStore::NAME });
    };
    let mut ledger = serde_json::from_str::<SupervisionLedger>(body)
        .map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    hydrate(&mut ledger, ledgers).map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    let mut touched_effects = BTreeSet::new();
    shed_departed_from_ledger(&mut ledger, manifest, &mut touched_effects);
    validate(&ledger, manifest).map_err(|e| store_failure(SupervisionStore::NAME, e))?;
    Ok(ledger)
}

/// The docstore live-read's FULLY tolerant read (#549). Like [`read_reconciled`]
/// it heals the protected roster and sheds departed owners IN MEMORY, but unlike
/// it — and unlike the strict [`read`] — it does **not** hard-fail on
/// `validate`. A single self-healable-but-invalid element (an invalid manager
/// goal, a stale sequence, an orphaned effect) must never zero the WHOLE live
/// read and force the docstore router onto the retired, frozen `org_documents`
/// mirror — the exact fault that froze the operator's "🎯 due" footer on
/// tribes-capital for ~17h (5487 `supervision live read FAILED` warnings) while
/// the reconcile CYCLE kept committing fine through the tolerant
/// [`read_for_mutation`]. This is the supervision twin of edb3d701/#26 (activity
/// read tolerates a self-healable shape drift): the duty path already serves through a
/// tolerant read; this makes the docstore READ path equally un-wedgeable, so one
/// bad row can never blind the footer for hours until a chiefd restart.
///
/// Only genuine `Absent` (never written) or non-decodable/hydratable bytes
/// (nothing coherent to serve) still error — the caller then falls through to
/// the ordinary `org_documents` path, exactly as before. Non-mutating: the
/// durable repair still lands when the cycle next writes.
///
/// # Errors
/// `Absent` when the document was never written; a store error only when the bytes
/// do not decode or the relational rows do not hydrate — never on a validation
/// failure a reconcile could heal.
pub fn read_live_tolerant(
    ledgers: &Ledgers,
    manifest: &OrganizationManifest,
) -> Result<SupervisionLedger, ChiefdError> {
    let Some(body) = ledgers.document_body(SupervisionStore::NAME) else {
        return Err(ChiefdError::Absent { store: SupervisionStore::NAME });
    };
    let mut ledger = serde_json::from_str::<SupervisionLedger>(body)
        .map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    hydrate(&mut ledger, ledgers).map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    let mut touched_effects = BTreeSet::new();
    shed_departed_from_ledger(&mut ledger, manifest, &mut touched_effects);
    // Deliberately NO terminal `validate`: a residual self-healable invalidity
    // must not zero the served view onto the stale mirror (#549).
    Ok(ledger)
}

/// Read for a mutation: decode and hydrate, but do **not** validate against the
/// manifest.
///
/// This is the port's shape, not a shortcut. `mutateLedger`
/// (`org-supervision.ts:280-311`) `JSON.parse`s the ledger, runs
/// `reconcileProtectedSupervision` over it, and only then validates the
/// *draft*. Validating the pre-reconcile state instead would make the repair
/// unreachable: `reconcileProtectedSupervision` exists precisely to drop the
/// check-ins and goal watches of people the manifest no
/// longer has — and a ledger still naming them is exactly what fails
/// validation. So a company would become permanently unmutatable the moment
/// somebody was offboarded.
///
/// A **reader** still validates in full ([`read`]); it is only the writer, the
/// one caller able to fix the disagreement, that is allowed to see it.
///
/// # Errors
/// `Corrupt{store:"supervision"}` when the bytes do not decode or the
/// relational rows do not hydrate.
fn read_for_mutation(
    ledgers: &Ledgers,
    _manifest: &OrganizationManifest,
) -> Result<SupervisionLedger, ChiefdError> {
    let Some(body) = ledgers.document_body(SupervisionStore::NAME) else {
        // #105: the document has never been written — absent, not damaged.
        // Reporting `Corrupt` here sent operators hunting for bytes that were
        // never there, and made a fresh company refuse every duty while the
        // daemon exited reporting success. Callers decide what absence means;
        // it must never silently become "empty".
        return Err(ChiefdError::Absent { store: SupervisionStore::NAME });
    };
    let mut ledger = serde_json::from_str::<SupervisionLedger>(body)
        .map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    hydrate(&mut ledger, ledgers).map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    Ok(ledger)
}

/// Seed the ledger for a freshly created company.
///
/// # Errors
/// [`INVALID_INPUT`] when the seeded ledger does not validate — which would
/// mean the manifest it was seeded from is broken.
pub fn seed(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
) -> Result<SupervisionLedger, ChiefdError> {
    let at = iso_millis(ledgers.now().0);
    let ledger = SupervisionLedger::initial(manifest, &at);
    validate(&ledger, manifest)?;
    ledgers.set_counter(NEXT_EFFECT_SEQUENCE, 1);
    put(ledgers, &ledger)?;
    Ok(ledger)
}

/// Run one mutation.
///
/// The ported publish rule (`mutateLedger` in `org-supervision.ts`): refuse an
/// absent document, reconcile the protected roster, run `f`, then publish when
/// anything changed. A refusal from `f` publishes nothing.
///
/// The closure receives the whole [`Ledgers`] rather than just the document,
/// because the effect rows it writes must land in the same
/// commit as the document (inv 14).
///
/// # Errors
/// `Corrupt`/`StoreFailure` from [`read`]; whatever `f` refuses;
/// [`INVALID_INPUT`] when the
/// result does not validate.
pub fn mutate<T>(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    f: impl FnOnce(&mut SupervisionDraft<'_>, &str) -> Result<T, ChiefdError>,
) -> Result<T, ChiefdError> {
    let at = iso_millis(ledgers.now().0);
    // Absence is corruption, never a default: [`seed`] is the only constructor
    // of an initial ledger, so a company without a document past creation has
    // LOST it and a fabricated empty ledger would bury that loss.
    let current = read_for_mutation(ledgers, manifest)?;

    let mut draft = SupervisionDraft {
        ledger: current.clone(),
        ledgers,
        manifest,
        touched_effects: BTreeSet::new(),
    };
    let reconciled = shed_departed_supervision(&mut draft);
    let result = f(&mut draft, &at)?;
    let SupervisionDraft { mut ledger, ledgers, touched_effects, .. } = draft;
    let changed = reconciled || ledger != current;
    let rows_changed = !touched_effects.is_empty();

    if changed || rows_changed {
        ledger.updated_at = at;
        // Flush the touched rows first: `validate` reads them back through the
        // relational tables, so a rule about them is checked against what is
        // actually about to be committed rather than against the draft's copy.
        for id in &touched_effects {
            flush_effect(ledgers, &ledger, id)?;
        }
        ledgers.set_counter(
            NEXT_EFFECT_SEQUENCE,
            i64::try_from(ledger.next_effect_sequence).unwrap_or(i64::MAX),
        );
        validate(&ledger, manifest)?;
        put(ledgers, &ledger)?;
    }
    Ok(result)
}

/// Ingest a launcher full-document supervision write (`/v1/docs/cas` /
/// `/v1/docs/insert-if-absent`) into this company's NATIVE ledger, making
/// `CompanyDb` the single supervision write authority (#440).
///
/// The #372 API-direct fix unified supervision READS onto `CompanyDb` but left
/// the launcher's WRITES landing in the `org_documents` mirror, which nothing
/// synced back — so a dynamically-hired person's document-level supervision
/// fields diverged and the
/// footer rendered a stale count. This applies that write into `CompanyDb`
/// instead, so read and write share one authority and `org_documents` stops
/// being a second one.
///
/// The launcher blob's document-level fields (the reminder roster) are adopted.
///
/// # The relational half is ADOPTED from the body, not carried forward (#444)
///
/// `effects`, their order and `next_effect_sequence` are
/// `#[serde(skip)]` on [`SupervisionLedger`], so a plain `from_str` deserializes
/// them EMPTY — but the launcher legitimately AUTHORS them through this very doc
/// CAS. #440's first cut carried the CURRENT native rows forward and dropped
/// the launcher's authored ones. So the relational half is parsed from the RAW body (it is
/// present as ordinary camelCase JSON; only the typed struct skips it) and
/// adopted, then flushed into the relational tables the next [`read`]'s
/// [`hydrate`] rebuilds from. The launcher holds the exclusive per-org mutation
/// lock and read this ledger live from `CompanyDb` immediately before mutating
/// it, so the relational half is committed from current normalized rows.
///
/// A body that carries NO relational half at all (a hypothetical document-only
/// writer) leaves the native rows untouched rather than wiping them.
///
/// The result is reconciled against the manifest (a launcher blob naming a
/// person this native manifest does not yet know has that person dropped rather
/// than wedging every future write), validated, and committed.
///
/// # Errors
/// `Corrupt` when the body does not decode, or when the native ledger is absent
/// (absence is corruption — creation is the only constructor); [`INVALID_INPUT`]
/// when the ingested result does not validate.
pub fn ingest_external_document(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    body: &str,
) -> Result<(), ChiefdError> {
    let at = iso_millis(ledgers.now().0);
    let mut incoming = serde_json::from_str::<SupervisionLedger>(body)
        .map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    // Read the native ledger (hydrated) first so we know the current
    // relational rows to reconcile flushes against.
    let current = read_for_mutation(ledgers, manifest)?;
    // #444: adopt the launcher-authored relational half from the raw body (it is
    // serde-skipped on the struct, so `incoming` deserialized it empty). Gate on
    // KEY PRESENCE, not emptiness: a launcher write that clears the last
    // effect sends `effectOrder: []` and that deletion must land, while a
    // body omitting the keys entirely carries the native rows forward untouched.
    let raw = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    let carries_relational_half = raw.get("effectOrder").is_some() || raw.get("effects").is_some();
    if carries_relational_half {
        // The complete relational half is one write contract. Its two
        // collection keys prove the launcher intends to mutate native rows;
        // without the counter, `#[serde(default)]` would deserialize zero and
        // the stale-race merge below would silently floor it before `validate`
        // can reject the malformed write (#405).
        if raw.get("nextEffectSequence").is_none() {
            return Err(ChiefdError::refused(
                INVALID_INPUT,
                "ChiefD supervision relational half omits nextEffectSequence; re-read and retry",
            ));
        }
        let relational = serde_json::from_value::<LauncherRelationalHalf>(raw)
            .map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
        if relational.next_effect_sequence == 0 {
            return Err(ChiefdError::refused(
                INVALID_INPUT,
                "ChiefD supervision nextEffectSequence must be positive; re-read and retry",
            ));
        }
        incoming.effects = relational.effects;
        incoming.effect_order = relational.effect_order;
        incoming.next_effect_sequence = relational.next_effect_sequence;
        merge_stale_launcher_relational_half(&mut incoming, &current)?;
    } else {
        incoming.effects = current.effects.clone();
        incoming.effect_order = current.effect_order.clone();
        incoming.next_effect_sequence = current.next_effect_sequence;
    }
    incoming.updated_at = at;
    // Flush the relational rows BEFORE validate/put, over the UNION of the prior
    // native ids and the adopted ids — so a removed row is deleted, a new one
    // inserted, a kept one updated, and the tables the next read hydrates from
    // match exactly what this write commits (mirrors `mutate`'s flush-then-put).
    let effect_ids: BTreeSet<String> =
        current.effect_order.iter().chain(incoming.effect_order.iter()).cloned().collect();
    for id in &effect_ids {
        flush_effect(ledgers, &incoming, id)?;
    }
    ledgers.set_counter(
        NEXT_EFFECT_SEQUENCE,
        i64::try_from(incoming.next_effect_sequence).unwrap_or(i64::MAX),
    );
    validate(&incoming, manifest)?;
    put(ledgers, &incoming)?;
    Ok(())
}

/// Repair only the rows a live effect-sequence collision proves are stale.
///
/// A launcher mutation reads the live ledger, allocates from the counter it
/// observed, then publishes without an actor fence. A native mutation between
/// that read and publish can consume the same sequence. The shared sequence (or
/// a same-id effect now carrying a different sequence on retry) proves exactly
/// which current effect must survive; it does not prove that every omitted row
/// is stale. Consequently this preserves complete current effect rows only for
/// those direct collisions, and resequences only new incoming effects that
/// occupied those live sequences.
///
/// Shared rows changed outside that proven set are ambiguous: the wire carries
/// no relational revision with which to distinguish a legitimate launcher edit
/// from a stale overwrite. Refuse that combination so the caller re-reads
/// instead of counterfeiting either authority. Omitted unrelated rows retain
/// the explicit-deletion semantics documented by [`ingest_external_document`].
fn merge_stale_launcher_relational_half(
    incoming: &mut SupervisionLedger,
    current: &SupervisionLedger,
) -> Result<(), ChiefdError> {
    let mut incoming_sequences = BTreeSet::new();
    for effect in incoming.effects.values() {
        if !incoming_sequences.insert(effect.sequence) {
            return Err(ChiefdError::refused(
                INVALID_INPUT,
                "ChiefD supervision effects reuse a sequence; re-read and retry",
            ));
        }
    }
    let current_sequence_owner: BTreeMap<u64, String> =
        current.effects.iter().map(|(id, effect)| (effect.sequence, id.clone())).collect();
    let mut protected_current_effects = BTreeSet::new();
    let mut resequence_incoming_effects = BTreeSet::new();

    for (id, effect) in &incoming.effects {
        match current.effects.get(id) {
            Some(current_effect) if current_effect.sequence != effect.sequence => {
                // A retry of a previously resequenced id is stale. Keep the
                // complete current row, not merely its sequence: delivery and
                // breaker state are server-authoritative state too.
                protected_current_effects.insert(id.clone());
                if let Some(owner) = current_sequence_owner.get(&effect.sequence) {
                    protected_current_effects.insert(owner.clone());
                }
            }
            None => {
                if let Some(owner) = current_sequence_owner.get(&effect.sequence) {
                    protected_current_effects.insert(owner.clone());
                    resequence_incoming_effects.insert(id.clone());
                } else if effect.sequence < current.next_effect_sequence {
                    // The sequence was issued but its row no longer exists.
                    // Reallocating here could resurrect a legitimately pruned
                    // effect; only a live collision is sufficient provenance.
                    return Err(ChiefdError::refused(
                        INVALID_INPUT,
                        format!(
                            "ChiefD supervision effect '{id}' uses stale sequence {} without a live collision; re-read and retry",
                            effect.sequence
                        ),
                    ));
                }
            }
            Some(_) => {}
        }
    }

    // The durable counter is a monotone high-water mark even when retention
    // has removed the row that originally consumed a sequence.
    incoming.next_effect_sequence =
        incoming.next_effect_sequence.max(current.next_effect_sequence).max(1);
    if protected_current_effects.is_empty() {
        return Ok(());
    }

    for (id, current_effect) in &current.effects {
        if protected_current_effects.contains(id) {
            continue;
        }
        if incoming.effects.get(id).is_some_and(|incoming_effect| incoming_effect != current_effect)
        {
            return Err(ChiefdError::refused(
                INVALID_INPUT,
                format!(
                    "ChiefD supervision effect '{id}' changed across a separate effect-sequence race; re-read and retry"
                ),
            ));
        }
    }

    for id in &protected_current_effects {
        if let Some(effect) = current.effects.get(id) {
            incoming.effects.insert(id.clone(), effect.clone());
            if !incoming.effect_order.contains(id) {
                incoming.effect_order.push(id.clone());
            }
        }
    }

    let mut used_sequences: BTreeSet<u64> = incoming
        .effects
        .iter()
        .filter(|(id, _)| !resequence_incoming_effects.contains(*id))
        .map(|(_, effect)| effect.sequence)
        .collect();
    fn sequence_exhausted() -> ChiefdError {
        ChiefdError::refused(
            INVALID_INPUT,
            "ChiefD supervision effect sequence is exhausted; operator repair is required",
        )
    }
    let sequence_floor = used_sequences
        .iter()
        .next_back()
        .copied()
        .map_or(Ok(1), |highest| highest.checked_add(1).ok_or_else(sequence_exhausted))?;
    let mut next_sequence = incoming.next_effect_sequence.max(sequence_floor);
    for id in &incoming.effect_order {
        if !resequence_incoming_effects.contains(id) {
            continue;
        }
        while used_sequences.contains(&next_sequence) {
            next_sequence = next_sequence.checked_add(1).ok_or_else(sequence_exhausted)?;
        }
        if let Some(effect) = incoming.effects.get_mut(id) {
            effect.sequence = next_sequence;
            used_sequences.insert(next_sequence);
            next_sequence = next_sequence.checked_add(1).ok_or_else(sequence_exhausted)?;
        }
    }
    let effects = &incoming.effects;
    incoming.effect_order.sort_by(|left, right| {
        let left_sequence = effects.get(left).map_or(u64::MAX, |effect| effect.sequence);
        let right_sequence = effects.get(right).map_or(u64::MAX, |effect| effect.sequence);
        left_sequence.cmp(&right_sequence).then_with(|| left.cmp(right))
    });
    incoming.next_effect_sequence = incoming.next_effect_sequence.max(next_sequence).max(1);
    Ok(())
}

/// The `#[serde(skip)]` relational half of [`SupervisionLedger`], parsed
/// SEPARATELY from the raw launcher body (#444). The launcher serializes these
/// as ordinary camelCase JSON; only the typed `SupervisionLedger` skips them (the
/// rows, not the body, are chiefd's authority). Every field defaults so a partial
/// body still deserializes; presence is decided by the caller on raw key lookup.
/// Hydrate the relational half (effects / effect_order /
/// next_effect_sequence) of a `SupervisionLedger` from its RAW
/// serialized body. Those fields are `#[serde(skip)]` (the rows are the
/// authority, never the body), so a plain `serde_json::from_str::<SupervisionLedger>`
/// leaves them EMPTY. The launcher serializes them as ordinary camelCase JSON,
/// so the ROW-PUBLISH path MUST re-read them from the raw body — mirroring
/// `seed_relational_from_document` / `ingest_external_document` — or every
/// launcher-authored effect is dropped at the wire, `next_effect_sequence`
/// never advances, and `loadSupervisionLedger` throws "effect sequence invalid".
///
/// `next_effect_sequence` is floored at one-past-the-highest adopted effect
/// (the #37 rule: the body may omit it since the ledger skips it), so a body
/// carrying effects but no counter still reconstructs a monotone counter.
///
/// # Errors
/// `Corrupt{store:"supervision"}` if `body` does not parse.
pub fn adopt_launcher_relational_half(
    ledger: &mut SupervisionLedger,
    body: &str,
) -> Result<(), ChiefdError> {
    let relational = serde_json::from_str::<LauncherRelationalHalf>(body)
        .map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    ledger.effects = relational.effects;
    ledger.effect_order = relational.effect_order;
    let effect_floor =
        ledger.effects.values().map(|e| e.sequence).max().map_or(1, |m| m.saturating_add(1));
    ledger.next_effect_sequence = relational.next_effect_sequence.max(effect_floor).max(1);
    Ok(())
}

/// Serialize a reconstructed `SupervisionLedger` to the launcher wire JSON,
/// SPLICING BACK the relational half (effects / effect_order /
/// next_effect_sequence). Those fields are `#[serde(skip)]` (the
/// rows are the authority, never the body), so a plain `serde_json::to_string`
/// DROPS them — which silently strips every effect from a row-path
/// read RESPONSE, so `loadSupervisionLedger` sees an empty set and throws
/// "effect sequence invalid" (the read-side twin of the publish-side wire drop).
/// The row-path read route MUST serialize through here.
///
/// # Errors
/// `StoreFailure{store:"supervision"}` if the ledger cannot serialize. This is
/// an ENCODE failure of an in-memory ledger, not a stored body that would not
/// decode, so it does not claim anything on disk is damaged.
pub fn to_launcher_json(ledger: &SupervisionLedger) -> Result<String, ChiefdError> {
    // Encoding an in-memory ledger cannot fail for a reason on disk, so this is
    // a store failure and never corruption — but the serde error still names the
    // field it choked on, and that reaches the caller.
    let failed = |e: serde_json::Error| store_failure(SupervisionStore::NAME, e);
    let mut value = serde_json::to_value(ledger).map_err(failed)?;
    let map = value.as_object_mut().ok_or_else(|| {
        crate::error::store_failure_because(
            SupervisionStore::NAME,
            "the encoded supervision ledger is not a JSON object",
        )
    })?;
    map.insert("effectOrder".into(), serde_json::to_value(&ledger.effect_order).map_err(failed)?);
    map.insert("effects".into(), serde_json::to_value(&ledger.effects).map_err(failed)?);
    map.insert(
        "nextEffectSequence".into(),
        serde_json::to_value(ledger.next_effect_sequence).map_err(failed)?,
    );
    serde_json::to_string(&value).map_err(failed)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LauncherRelationalHalf {
    #[serde(default)]
    effect_order: Vec<String>,
    #[serde(default)]
    effects: BTreeMap<String, Effect>,
    #[serde(default)]
    next_effect_sequence: u64,
}

fn flush_effect(
    ledgers: &mut Ledgers,
    ledger: &SupervisionLedger,
    id: &str,
) -> Result<(), Refusal> {
    let Some(effect) = ledger.effects.get(id) else {
        ledgers.remove_effect(id);
        return Ok(());
    };
    let body = serde_json::to_string(effect).map_err(|error| {
        invalid(LEDGER_UNSERIALIZABLE, format!("cannot encode effect '{id}': {error}"))
    })?;
    ledgers.put_effect(
        id,
        EffectRow {
            seq: effect.sequence,
            kind: effect.kind.clone(),
            body,
            delivered_at: effect.delivered_at.as_deref().and_then(parse_iso_millis),
        },
    );
    Ok(())
}

/// Bootstrap-only: replay every effect a `supervision` document
/// names into the relational table `hydrate` reads them from (plan §5.1,
/// M12), plus `next_effect_sequence` — through the exact same
/// `flush_effect` a real mutation uses, so a seeded row is
/// byte-for-byte what the daemon itself would have written.
///
/// The `next_effect_sequence` counter is DERIVED from the seeded effects (one
/// past the highest, and 1 for a ledger that has issued none) rather than taken
/// from the decoded body — see the comment at the `set_counter` call below for
/// why the body can never carry it.
///
/// Used by `chiefd bootstrap-store` to seed a pre-existing company's
/// `chief.db` from its real, currently-live `supervision` content: the
/// document body alone is not sufficient because [`hydrate`] overwrites
/// `effect_order`/`effects` from these relational tables on every read,
/// discarding whatever the JSON blob says if the tables are empty.
///
/// # Errors
/// [`ChiefdError::Corrupt`] if `body` does not decode as a
/// [`SupervisionLedger`] — the same failure [`read`] would give the daemon on
/// this content.
pub fn seed_relational_from_document(
    ledgers: &mut Ledgers,
    body: &str,
) -> Result<(usize, usize), ChiefdError> {
    let mut ledger = serde_json::from_str::<SupervisionLedger>(body)
        .map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    // #444: the typed parse `#[serde(skip)]`s the relational half, so a
    // launcher-authored body (which carries effects as ordinary JSON) decodes
    // them EMPTY. Adopt them from the raw body exactly as
    // `ingest_external_document` does — otherwise the seeded tables are empty
    // and `hydrate` discards the JSON's effect set on the very next read.
    let raw = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    let relational: LauncherRelationalHalf =
        serde_json::from_value(raw).map_err(|e| corrupt_store(SupervisionStore::NAME, e))?;
    ledger.effects = relational.effects;
    ledger.effect_order = relational.effect_order;
    for id in &ledger.effect_order {
        flush_effect(ledgers, &ledger, id).map_err(|e| store_failure(SupervisionStore::NAME, e))?;
    }
    // #37: derive the counter from the effects we just seeded, NOT from
    // `ledger.next_effect_sequence`.
    //
    // Found live, 2026-07-24 (eng-e2e, `chiefd bootstrap-store --store
    // supervision` on a fresh company): `next_effect_sequence` is `#[serde(skip)]`
    // — the relational counter is its authority, so it is deliberately absent
    // from the document body and ALWAYS deserializes to `0` here, whatever the
    // JSON says. Writing that `0` straight into the counter created a
    // `next_effect_sequence` row of 0 with no effects, which `Ledgers::validate`
    // then refused `effect-sequence-not-monotonic: next_effect_sequence is 0,
    // which is not beyond the highest issued sequence 0` — so seeding a fresh
    // company's supervision ledger could never commit at all.
    //
    // The counter's contract is "the next sequence to hand out, strictly beyond
    // every issued one", so the only correct value is one past the highest
    // sequence actually seeded, and 1 (never 0) for a ledger that has issued
    // nothing — sequence 0 is not a legal effect sequence (`validate` requires
    // `effect.sequence > 0`). This mirrors the identical `.max(1)` clamp
    // [`hydrate`] already applies on the read side for the same reason.
    let highest = ledger
        .effect_order
        .iter()
        .filter_map(|id| ledger.effects.get(id).map(|effect| effect.sequence))
        .max()
        .unwrap_or(0);
    let next = highest.saturating_add(1).max(1);
    ledgers.set_counter(NEXT_EFFECT_SEQUENCE, i64::try_from(next).unwrap_or(i64::MAX));
    Ok((0, ledger.effect_order.len()))
}

/// Remove the ledger and its rows, returning whether anything was present.
///
/// # Errors
/// `Corrupt{store:"supervision"}` over unreadable bytes.
pub fn clear(ledgers: &mut Ledgers, manifest: &OrganizationManifest) -> Result<bool, ChiefdError> {
    if ledgers.document_body(SupervisionStore::NAME).is_some() {
        read(ledgers, manifest)?;
    }
    let ids: Vec<String> = ledgers.effect_order().into_iter().map(ToString::to_string).collect();
    for id in ids {
        ledgers.remove_effect(&id);
    }
    Ok(ledgers.remove_document(SupervisionStore::NAME))
}

fn put(ledgers: &mut Ledgers, ledger: &SupervisionLedger) -> Result<(), Refusal> {
    let encoded = serde_json::to_string(ledger).map_err(|error| {
        invalid(LEDGER_UNSERIALIZABLE, format!("cannot encode the supervision ledger: {error}"))
    })?;
    ledgers.put_document(SupervisionStore::NAME, encoded);
    Ok(())
}

/// A supervision mutation in progress.
///
/// Holds the decoded document *and* the live [`Ledgers`], because an effect's
/// row and the document's reminder roster are written by the same closure and
/// must reach disk together.
pub struct SupervisionDraft<'a> {
    ledger: SupervisionLedger,
    ledgers: &'a mut Ledgers,
    manifest: &'a OrganizationManifest,
    touched_effects: BTreeSet<String>,
}

impl SupervisionDraft<'_> {
    /// The decoded ledger.
    #[must_use]
    pub fn ledger(&self) -> &SupervisionLedger {
        &self.ledger
    }

    /// The manifest this mutation is fenced against.
    #[must_use]
    pub fn manifest(&self) -> &OrganizationManifest {
        self.manifest
    }

    /// Mutable access to the decoded ledger.
    ///
    /// Exists for supervision mutations whose state does not live behind any
    /// of the narrow helpers above; routing each through a bespoke helper per
    /// field would be more surface, not less. [`mutate`] still validates and
    /// publishes whatever this produced, so a caller cannot commit an invalid
    /// ledger through it.
    pub fn ledger_mut(&mut self) -> &mut SupervisionLedger {
        &mut self.ledger
    }

    /// Epoch millis of the commit being assembled.
    #[must_use]
    pub fn now(&self) -> i64 {
        self.ledgers.now().0
    }

    fn touch_effect(&mut self, id: &str) {
        self.touched_effects.insert(id.to_string());
    }

    fn enqueue_effect(
        &mut self,
        id: &str,
        kind: &str,
        payload: BTreeMap<String, serde_json::Value>,
        at: &str,
    ) -> Result<bool, ChiefdError> {
        if let Some(existing) = self.ledger.effects.get(id) {
            if existing.comparable() != &payload || existing.kind != kind {
                return Err(ChiefdError::refused(
                    EFFECT_CONTENT_CONFLICT,
                    format!("Supervision effect id '{id}' has conflicting content"),
                ));
            }
            return Ok(false);
        }
        let sequence = self.ledger.next_effect_sequence;
        self.ledger.next_effect_sequence = sequence.saturating_add(1);
        self.ledger.effects.insert(
            id.to_string(),
            Effect {
                id: id.to_string(),
                sequence,
                kind: kind.to_string(),
                status: EffectStatus::Pending,
                created_at: at.to_string(),
                delivered_at: None,
                superseded_at: None,
                delivery_failure_count: None,
                last_delivery_failure_at: None,
                failed_at: None,
                reopen_count: None,
                last_reopened_at: None,
                payload,
            },
        );
        self.ledger.effect_order.push(id.to_string());
        self.touch_effect(id);
        Ok(true)
    }

    /// Enqueue the live operator-escalation effect for a converge-actuation
    /// safety event — a tripped circuit breaker or a refused destructive budget.
    ///
    /// The durable audit trail is recorded by
    /// [`crate::store::converge_safety`]; this is the live half. Per that
    /// store's module note the enqueue stays on the caller's side of the seam,
    /// because the effects pipeline and its sequence invariant belong to this
    /// store. Exactly-once by `id`, delivered as an ordinary envelope
    /// ([`RECONCILE_ESCALATION_EFFECT_KIND`]).
    ///
    /// `id` is the alert slot for one (organization, reason) — not a per-call
    /// nonce. The reconcile actuator retries the *same* escalation every cycle
    /// the condition persists, and rebuilds `detail` fresh from live state each
    /// time (e.g. the current predicted-vs-limit counts), so it legitimately
    /// drifts cycle to cycle even though it is logically the same alert. A call
    /// against an `id` that already holds a **pending, delivered, or
    /// superseded** effect is therefore a no-op — first escalation wins and
    /// sticks — rather than being compared content-for-content and refused:
    /// that comparison is for callers whose `id` is a per-content nonce, which
    /// this one is not. This never reaches [`EFFECT_CONTENT_CONFLICT`].
    ///
    /// A **failed** effect is the one exception: a delivery failure at this id
    /// must not permanently silence the alert slot for a condition that is
    /// still recurring. `enqueue_effect`'s exactly-once check has no idea the
    /// dispatch side ever gave up, so — live, 2026-07-21 — a `reconcile-
    /// escalation:*:{budget_exceeded,circuit_breaker}` effect failed delivery
    /// once and every subsequent re-escalation over the following two days was
    /// silently swallowed by this same no-op, with no operator doorbell ever
    /// firing while the condition churned. Re-arms the effect to `Pending`
    /// with the freshly recomputed payload instead, bounded by
    /// [`SUPERVISION_EFFECT_REOPEN_LIMIT`] the same as an operator's explicit
    /// [`super::delivery::reopen_failed_effects`] — once that budget is spent
    /// the effect is left `failed` for good rather than re-armed forever.
    pub fn enqueue_reconcile_escalation(
        &mut self,
        id: &str,
        reason: &str,
        detail: &str,
        at: &str,
    ) -> Result<bool, ChiefdError> {
        let payload = reconcile_escalation_payload(&self.manifest().slug, reason, detail);
        if let Some(existing) = self.ledger.effects.get(id) {
            if existing.status != EffectStatus::Failed {
                // Already escalated for this (organization, reason) slot and
                // still on a live track — a retry with drifted live detail
                // must not be fed through the exactly-once content check.
                return Ok(false);
            }
            if existing.reopen_count.unwrap_or(0) >= SUPERVISION_EFFECT_REOPEN_LIMIT {
                // The reopen budget is spent: leave it failed for good rather
                // than re-arm a genuinely poison alert forever.
                return Ok(false);
            }
            let Some(effect) = self.ledger.effects.get_mut(id) else {
                return Ok(false);
            };
            effect.status = EffectStatus::Pending;
            effect.reopen_count = Some(effect.reopen_count.unwrap_or(0).saturating_add(1));
            effect.last_reopened_at = Some(at.to_string());
            effect.delivery_failure_count = Some(0);
            effect.failed_at = None;
            effect.last_delivery_failure_at = None;
            effect.payload = payload;
            self.touch_effect(id);
            return Ok(true);
        }
        self.enqueue_effect(id, RECONCILE_ESCALATION_EFFECT_KIND, payload, at)
    }

    /// Enqueue one effect directly, for tests that need a specific queue
    /// state without driving the op that would produce it.
    pub fn enqueue_effect_for_test(
        &mut self,
        id: &str,
        kind: &str,
        at: &str,
    ) -> Result<bool, ChiefdError> {
        self.enqueue_effect(id, kind, BTreeMap::new(), at)
    }

    /// Move the durable effect-sequence high-water mark forward, so a test can
    /// stage the "sequence was issued but its row no longer exists" state a
    /// retention prune produces. Never lowers the counter.
    #[cfg(any(test, feature = "test-support"))]
    pub fn bump_next_effect_sequence_for_test(&mut self, next: u64) {
        self.ledger.next_effect_sequence = self.ledger.next_effect_sequence.max(next);
    }
}

/// The payload for a `reconcile-escalation:*` effect: shared by the
/// first-enqueue and the failed-reopen paths in
/// [`SupervisionDraft::enqueue_reconcile_escalation`] so a reopen's refreshed
/// `detail`/`message` are built exactly the same way a fresh alert's are.
fn reconcile_escalation_payload(
    organization: &str,
    reason: &str,
    detail: &str,
) -> BTreeMap<String, serde_json::Value> {
    let message =
        format!("Converge actuation escalation ({reason}) for '{organization}': {detail}");
    [
        ("organization".to_string(), serde_json::Value::String(organization.to_string())),
        ("reason".to_string(), serde_json::Value::String(reason.to_string())),
        ("detail".to_string(), serde_json::Value::String(detail.to_string())),
        ("message".to_string(), serde_json::Value::String(message)),
    ]
    .into_iter()
    .collect()
}

// --- validation ----------------------------------------------------------

fn unique_order<T>(
    order: &[String],
    records: &BTreeMap<String, T>,
    label: &str,
) -> Result<(), Refusal> {
    let unique: BTreeSet<&String> = order.iter().collect();
    if unique.len() != order.len()
        || order.iter().any(|id| !records.contains_key(id))
        || order.len() != records.len()
    {
        return Err(invalid(INVALID_INPUT, format!("Supervision {label} order is invalid")));
    }
    Ok(())
}

/// Every rule `validateSupervisionLedger` enforces that this milestone owns.
///
/// # Errors
/// [`INVALID_INPUT`].
#[allow(clippy::too_many_lines)] // One rule per statement; the order is the port.
pub fn validate(
    ledger: &SupervisionLedger,
    manifest: &OrganizationManifest,
) -> Result<(), Refusal> {
    if ledger.schema_version != 1 && ledger.schema_version != SUPERVISION_SCHEMA_VERSION {
        return Err(invalid(INVALID_INPUT, "Unsupported supervision ledger"));
    }
    if ledger.organization != manifest.slug {
        return Err(invalid(
            INVALID_INPUT,
            format!(
                "Supervision ledger belongs to '{}', not '{}'",
                ledger.organization, manifest.slug
            ),
        ));
    }
    unique_order(&ledger.effect_order, &ledger.effects, "effect")?;
    unique_order(&ledger.reminder_order, &ledger.reminders, "reminder")?;

    for reminder_id in &ledger.reminder_order {
        let reminder = &ledger.reminders[reminder_id];
        // A reminder for a person who no longer exists would fire forever at
        // nobody. Offboarding sheds them (`shed_departed_supervision`); this is
        // the fence that makes the shed mandatory rather than best-effort.
        let bad = reminder.id != *reminder_id
            || !manifest.people.contains_key(&reminder.person_id)
            || !manifest.people.contains_key(&reminder.created_by_person_id)
            || reminder.prompt.trim().is_empty()
            // THE DELAY FLOOR, DELIBERATELY, AND NOT THE CADENCE FLOOR. A row
            // armed before the cadence floor existed is not corrupt — it is
            // legacy, and the re-arm clamp corrects it at its next fire.
            // Validating it against the cadence floor would reject exactly the
            // rows that mechanism exists to migrate, and a rejected document is
            // a company that will not load.
            || reminder.interval_ms < MIN_REMINDER_INTERVAL_MS
            || !["active", "stopped"].contains(&reminder.status.as_str())
            || parse_iso_millis(&reminder.next_due_at).is_none()
            // An unparseable expiry is refused on the way IN rather than fudged
            // at read time: `Reminder::is_armed` fails closed on one, so a row
            // that slipped through would silently never fire again.
            || reminder.expires_at.as_deref().is_some_and(|s| parse_iso_millis(s).is_none());
        if bad {
            return Err(invalid(INVALID_INPUT, format!("Reminder '{reminder_id}' is invalid")));
        }
    }

    let mut previous_sequence = 0_u64;
    for effect_id in &ledger.effect_order {
        let effect = &ledger.effects[effect_id];
        if effect.id != *effect_id || effect.sequence <= previous_sequence {
            return Err(invalid(
                INVALID_INPUT,
                format!("Supervision effect '{effect_id}' has an invalid sequence"),
            ));
        }
        previous_sequence = effect.sequence;
    }
    if ledger.next_effect_sequence <= previous_sequence {
        return Err(invalid(INVALID_INPUT, "Supervision effect sequence is invalid"));
    }
    Ok(())
}

/// Shed every reminder whose owner (or author) the manifest no longer staffs.
///
/// A person is "gone" when the manifest no longer STAFFS them — whether they
/// are flagged `departed` in place OR removed from the manifest outright (a
/// whole offboarded department). The earlier form derived this from
/// `people_order`, so a person deleted from the manifest entirely never counted
/// as departed and `validate` then rejected the WHOLE ledger, wedging every
/// future read and the reconcile that would have repaired it (found live on
/// tribes-capital).
///
/// A departed person's reminders go with them. Left behind they would fail
/// [`validate`]'s own person check on the very next read, and a reminder that
/// fires at nobody is work nobody asked for.
fn shed_departed_from_ledger(
    ledger: &mut SupervisionLedger,
    manifest: &OrganizationManifest,
    _touched_effects: &mut BTreeSet<String>,
) -> bool {
    let staffed = staffed_people(manifest);
    let gone = |person_id: &str| !staffed.contains(person_id);
    let shed: Vec<String> = ledger
        .reminder_order
        .iter()
        .filter(|id| {
            ledger.reminders.get(*id).is_some_and(|reminder| {
                gone(&reminder.person_id) || gone(&reminder.created_by_person_id)
            })
        })
        .cloned()
        .collect();
    for id in &shed {
        ledger.reminders.remove(id);
        ledger.reminder_order.retain(|kept| kept != id);
    }
    !shed.is_empty()
}

fn shed_departed_supervision(draft: &mut SupervisionDraft<'_>) -> bool {
    let SupervisionDraft { ledger, manifest, touched_effects, .. } = draft;
    shed_departed_from_ledger(ledger, manifest, touched_effects)
}

/// Everybody supervision may still hold runtime state for: the roster minus
/// anyone who has departed. Mirrors `staffedPeople` in `org-supervision-state.ts`.
fn staffed_people(manifest: &OrganizationManifest) -> BTreeSet<String> {
    manifest
        .people_order
        .iter()
        .filter(|person_id| {
            manifest
                .people
                .get(*person_id)
                .is_some_and(|person| person.employment_state != EmploymentState::Departed)
        })
        .cloned()
        .collect()
}

mod cycle;
mod delivery;
mod dispatch;
mod reminders;
pub mod rows;

pub use cycle::{
    cycle, CycleInput, CycleReport, IdentityObservation, RuntimeAuditObservation, Stage,
};
pub use delivery::{
    dispatch_plan, mark_delivered, record_delivery_failure, reopen_failed_effects, DispatchPlan,
    SUPERVISION_EFFECT_DELIVERY_ATTEMPT_LIMIT, SUPERVISION_EFFECT_REOPEN_LIMIT,
};
pub use dispatch::{
    actuate_staged, deliver_batch, stage_batch, DeliveryReport, DeliveryRequest, DispatchFailure,
    StagedBatch,
};
pub use reminders::{
    arm_reminder, armed_count as armed_reminder_count, ensure_reminder_scope, evaluate_reminders,
    list_reminders, next_due_at as next_reminder_due_at, stop_reminder, ArmRequest, ReminderReport,
    INVALID_REMINDER, REMINDER_EFFECT_KIND, REMINDER_LIMIT_REACHED, REMINDER_MARKER,
    REMINDER_NOT_IN_SCOPE, UNKNOWN_REMINDER,
};
