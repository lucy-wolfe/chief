//! The in-memory ledgers the writer actor mutates, and the committed snapshot
//! readers see.
//!
//! Plan §5.1: *"Until the supervision body splits, the writer holds the
//! deserialized ledger in memory and serializes once per commit."* That is what
//! this module is. [`Ledgers`] is the mutable working set a `mutate` closure
//! receives; [`LedgerSnapshot`] is an immutable, **committed** view published
//! through `arc-swap` after every commit so `read()` never queues behind the
//! writer (plan §5.3).
//!
//! # Milestone boundary
//!
//! M4 lands the mechanism plus the Phase-1/2 strangler shape — the `documents`
//! table, ported as-is (plan §5.1). The relational store ledgers (
//! effects, mailbox, transitions…) are M10/M12; they add fields
//! to [`Ledgers`] and arms to [`Validate::validate`]. Nothing about the actor
//! changes when they do, which is the point of putting the seam here.
//!
//! # Why the working set is a clone
//!
//! A `mutate` closure may mutate the ledgers and *then* return a [`Refusal`],
//! and `validate()` may reject a state the closure thought was fine. Running
//! the closure against a clone of the last committed snapshot makes rollback
//! total and free: on any non-success path the clone is simply dropped, so no
//! partially-applied in-memory state can survive a rolled-back transaction.
//! This is the in-memory half of the guarantee the SQL transaction gives on
//! disk, and the pair is asserted by
//! `validation_failure_rolls_back_both_the_transaction_and_the_ledger`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::clock::WallMillis;
use crate::error::Refusal;
use crate::host_action::{HostActionPhase, HostActionRecord};
use crate::store::mailbox::MailboxEnvelope;

/// Test-only counter of document-body JSON parses performed by validation.
///
/// #123: per-commit validation must be **incremental** — a commit that changes
/// one store parses one body, not the whole ~1 MB `documents` ledger. This
/// counter is the seam that proves it: RED against the whole-ledger parse,
/// GREEN once [`Ledgers::validate_since`] only re-parses changed bodies. Gated
/// behind `test`/`test-support` so release validation carries no counter — the
/// same convention `set_now_for_test` uses (the integration/conformance crates
/// cannot see a `cfg(test)` item).
#[cfg(any(test, feature = "test-support"))]
pub static DOCUMENT_BODY_PARSES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Parse-check one document body as JSON, counting the parse under test builds.
///
/// The only body-bytes-proportional cost in validation: a full
/// `serde_json::from_str::<Value>` allocation over the whole body. Every other
/// rule operates on already-parsed relational rows.
fn document_body_is_json(body: &str) -> bool {
    #[cfg(any(test, feature = "test-support"))]
    DOCUMENT_BODY_PARSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    serde_json::from_str::<serde_json::Value>(body).is_ok()
}

/// One transient in-memory store projection.
///
/// Normalized rows remain durable authority. The actor keeps this serialized
/// body only for existing in-process store readers; it is neither a persistent
/// document record nor a caller-visible mutation fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentRecord {
    /// The serialized store body. `Arc<str>` rather than `String` so cloning the
    /// committed snapshot on every mutation (`run_job` clones the last committed
    /// `Ledgers` as its working set) bumps a refcount instead of deep-copying
    /// every untouched body — #468: the per-commit clone cost no longer scales
    /// with the size of documents this commit never touched. A `put_document`
    /// mints a fresh `Arc` for exactly the store it writes; every other store's
    /// body Arc is shared with the base snapshot.
    body: Arc<str>,
    updated_at: WallMillis,
}

impl DocumentRecord {
    /// The serialized store body (JSON).
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Wall-clock milliseconds of the commit that last wrote this row.
    #[must_use]
    pub fn updated_at(&self) -> WallMillis {
        self.updated_at
    }

    /// Reconstruct a record read back from SQLite.
    ///
    /// Used by the writer actor when it loads the ledger at open. Deliberately
    /// not `pub`: outside the crate a record only ever comes from a snapshot.
    pub(crate) fn from_row(body: String, updated_at: WallMillis) -> Self {
        Self { body: Arc::from(body), updated_at }
    }
}

/// One `effects` row (plan §5.1, M12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRow {
    /// The monotone sequence. Never reused after a prune — see
    /// [`Ledgers::counter`] and `next_effect_sequence`.
    pub seq: u64,
    /// Effect type discriminant.
    pub kind: String,
    /// The full record, as the store layer's typed body.
    pub body: String,
    /// Epoch millis of delivery, once dispatched.
    pub delivered_at: Option<i64>,
}

// TOMBSTONE (#751-P4): `ReflectionRecord` — the durable memory row a
// `reflect` call wrote alongside the activity ledger, re-read as proof that a
// handoff had reached durable storage — is DELETED with the reflection concept
// itself. A graceful transition now records only that it was released
// (`store::activity::release`), so there is no second fact to make durable and
// nothing on `Ledgers` to keep it in.

/// One `mailbox` row (plan §5.1; duty #8).
///
/// The durable per-recipient envelope, one row per `(envelope, recipient)`.
/// Relational rather than a per-person document (the TypeScript shape) because
/// chiefd's native schema already owns a `mailbox` table (`schema.rs`) and the
/// one-daemon migration's whole direction is off the `org_documents` contract
/// and onto chiefd-native rows. The five-bucket map the TypeScript store held
/// inside one document becomes the [`state`](MailboxRow::state) column here.
///
/// `envelope_id` (the row key on [`Ledgers`]) is a deterministic function of the
/// logical envelope and its recipient, so re-publishing after a crash is an
/// idempotent no-op rather than a duplicate — the property that makes the
/// two-commit dispatch (publish, then mark the effect delivered) at-least-once
/// safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxRow {
    /// The recipient whose mailbox this envelope sits in.
    pub person: String,
    /// The typed envelope this row carries. Columnarized at the SQL
    /// boundary (schema Part B / Fable #7): there is no opaque `body`
    /// blob. `recipients`/`organization`/`schemaVersion` are DERIVED at
    /// reconstruct, so a row's in-memory copy of them is advisory only.
    pub envelope: MailboxEnvelope,
    /// Lifecycle bucket: `pending` while awaiting drain, then a terminal
    /// `accepted`/`superseded`/`rejected`/`resolved`.
    pub state: String,
    /// Epoch millis of the commit that last wrote this row.
    pub updated_at: i64,
}

/// The name of the explicit monotonic effect-sequence counter.
///
/// Plan §5.1 makes this a real row rather than `max(seq)+1`: a prune of the
/// maximum rows must never let a later insert reuse a sequence a reader already
/// observed, and `AUTOINCREMENT` alone does not survive a table rebuild.
pub const NEXT_EFFECT_SEQUENCE: &str = "next_effect_sequence";

/// The complete mutable working set of one company database.
///
/// A `mutate` closure receives `&mut Ledgers`. It must not read the clock
/// itself — [`Ledgers::now`] carries the single wall reading the writer took
/// for this commit, so every row written by one transaction shares one
/// timestamp and tests on a [`ManualClock`](crate::test_support::ManualClock)
/// are exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledgers {
    now: WallMillis,
    documents: BTreeMap<String, DocumentRecord>,
    /// Open host-transaction intents, keyed by action id (plan §5.6).
    ///
    /// These live in the same working set as the documents on purpose: commit
    /// 2 of a host transaction is *"manifest advance **and** intent close, in
    /// one transaction"*, and that is only true if both rows are written by
    /// one `mutate` closure.
    host_actions: BTreeMap<String, HostActionRecord>,
    /// Effect rows (M12), keyed by effect id.
    effects: BTreeMap<String, EffectRow>,
    /// Durable mailbox rows (duty #8), keyed by the deterministic
    /// per-`(envelope, recipient)` id. The durable half of effect delivery.
    mailbox: BTreeMap<String, MailboxRow>,
    /// Explicit monotonic counters (M12): `next_effect_sequence`.
    counters: BTreeMap<String, i64>,
}

impl Ledgers {
    /// An empty ledger set stamped at `now`.
    #[must_use]
    pub fn empty(now: WallMillis) -> Self {
        Self {
            now,
            documents: BTreeMap::new(),
            host_actions: BTreeMap::new(),
            effects: BTreeMap::new(),
            mailbox: BTreeMap::new(),
            counters: BTreeMap::new(),
        }
    }

    /// The wall reading of the commit currently being assembled.
    #[must_use]
    pub fn now(&self) -> WallMillis {
        self.now
    }

    /// Restamp for a new commit. Writer-only: a closure must never move time.
    pub(crate) fn set_now(&mut self, now: WallMillis) {
        self.now = now;
    }

    /// Restamp from outside the crate, for tests only.
    ///
    /// Gated behind `test-support`: the
    /// conformance runner and the integration tests are separate crates and
    /// cannot see a `cfg(test)` item, and the corpus's `clock.advance` op has to
    /// move a ledger's clock without going through the writer. CI asserts the
    /// feature is off in release builds, so production keeps the property that
    /// only the writer moves time.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_now_for_test(&mut self, now: WallMillis) {
        self.now = now;
    }

    /// The record for `store`, if it exists.
    #[must_use]
    pub fn document(&self, store: &str) -> Option<&DocumentRecord> {
        self.documents.get(store)
    }

    /// The body for `store`, if it exists.
    #[must_use]
    pub fn document_body(&self, store: &str) -> Option<&str> {
        self.documents.get(store).map(DocumentRecord::body)
    }

    /// Every store name present, in sorted order.
    pub fn stores(&self) -> impl Iterator<Item = &str> {
        self.documents.keys().map(String::as_str)
    }

    /// Write `body` to `store` and stamp it with this commit's wall reading.
    /// A new [`Arc`] marks the projection changed without a mutable version.
    pub fn put_document(&mut self, store: &str, body: impl Into<String>) {
        self.documents.insert(
            store.to_string(),
            DocumentRecord { body: Arc::from(body.into()), updated_at: self.now },
        );
    }

    /// Drop `store` entirely. Returns whether a row was present.
    pub fn remove_document(&mut self, store: &str) -> bool {
        self.documents.remove(store).is_some()
    }

    /// Rows present in `self` but changed or absent in `previous` — the upsert
    /// set for one commit.
    pub(crate) fn changed_since<'a>(
        &'a self,
        previous: &Ledgers,
    ) -> Vec<(&'a str, &'a DocumentRecord)> {
        // #468: compare `Arc` allocation identity, never body bytes. The
        // working ledger starts as a clone of the committed snapshot, so every
        // untouched body shares the exact same Arc. `put_document` mints one
        // fresh Arc, which makes a rewritten or newly created store changed in
        // O(1) without a mutable per-document version. This diff runs once for
        // persistence and once for the feed, so avoiding a body comparison is
        // material for multi-megabyte ledgers.
        self.documents
            .iter()
            .filter(|(store, record)| {
                previous
                    .documents
                    .get(*store)
                    .is_none_or(|prior| !Arc::ptr_eq(&record.body, &prior.body))
            })
            .map(|(store, record)| (store.as_str(), record))
            .collect()
    }

    /// Rows present in `previous` but gone in `self` — the delete set.
    pub(crate) fn removed_since(&self, previous: &Ledgers) -> Vec<String> {
        let live: BTreeSet<&String> = self.documents.keys().collect();
        previous.documents.keys().filter(|store| !live.contains(store)).cloned().collect()
    }

    /// Insert a row read back from SQLite at open. Writer-only.
    pub(crate) fn load_document(&mut self, store: String, record: DocumentRecord) {
        self.documents.insert(store, record);
    }

    // --- host-transaction intents (plan §5.6) ---------------------------

    /// The intent row for `action_id`, if one is open.
    #[must_use]
    pub fn host_action(&self, action_id: &str) -> Option<&HostActionRecord> {
        self.host_actions.get(action_id)
    }

    /// Every journalled intent, in action-id order.
    pub fn host_actions(&self) -> impl Iterator<Item = (&str, &HostActionRecord)> {
        self.host_actions.iter().map(|(id, record)| (id.as_str(), record))
    }

    /// Intents the startup recovery pass must act on, ordered by the commit
    /// that created them.
    ///
    /// Creation order, not id order: a sequence of host transactions that was
    /// interrupted part-way must converge in the order it was attempted, or a
    /// later plan's rollback could undo an earlier plan's completed publish.
    /// Ties break on the id so the order is total and the recovery pass is
    /// deterministic (TESTING.md §1.2 — never a coin flip).
    #[must_use]
    pub fn open_host_actions(&self) -> Vec<(&str, &HostActionRecord)> {
        let mut open: Vec<(&str, &HostActionRecord)> =
            self.host_actions.iter().map(|(id, r)| (id.as_str(), r)).collect();
        open.sort_by_key(|(id, record)| (record.created_at().0, *id));
        open
    }

    /// Journal an intent (commit 1) or overwrite one wholesale.
    pub fn put_host_action(&mut self, action_id: impl Into<String>, record: HostActionRecord) {
        self.host_actions.insert(action_id.into(), record);
    }

    /// Advance an intent to a later phase. Returns whether the row existed.
    ///
    /// Absence is reported rather than created: a phase advance for an intent
    /// nobody journalled would mean the executor ran without commit 1, which is
    /// the exact ordering violation this journal exists to make impossible.
    pub fn advance_host_action(&mut self, action_id: &str, phase: HostActionPhase) -> bool {
        match self.host_actions.get_mut(action_id) {
            Some(record) => {
                *record = record.advanced_to(phase);
                true
            }
            None => false,
        }
    }

    /// Close an intent by deleting it. Returns whether a row was present.
    ///
    /// Deletion rather than a `phase='closed'` tombstone: a closed intent has
    /// no further use, and an unbounded table of them is precisely the litter
    /// (`.preference-transaction-*` directories) that made the predecessor's
    /// recovery pass unreadable.
    pub fn close_host_action(&mut self, action_id: &str) -> bool {
        self.host_actions.remove(action_id).is_some()
    }

    fn host_actions_changed_since<'a>(
        &'a self,
        previous: &Self,
    ) -> Vec<(&'a str, &'a HostActionRecord)> {
        self.host_actions
            .iter()
            .filter(|(id, record)| previous.host_actions.get(*id) != Some(record))
            .map(|(id, record)| (id.as_str(), record))
            .collect()
    }

    fn host_actions_removed_since(&self, previous: &Self) -> Vec<String> {
        let live: BTreeSet<&String> = self.host_actions.keys().collect();
        previous.host_actions.keys().filter(|id| !live.contains(id)).cloned().collect()
    }

    /// Insert an intent read back from SQLite at open. Writer-only.
    pub(crate) fn load_host_action(&mut self, action_id: String, record: HostActionRecord) {
        self.host_actions.insert(action_id, record);
    }
}

// --- the M12 relational tables -------------------------------------------

impl Ledgers {
    /// The effect row for `id`.
    #[must_use]
    pub fn effect(&self, id: &str) -> Option<&EffectRow> {
        self.effects.get(id)
    }

    /// Every effect, in sequence order — the port of `effectOrder`.
    ///
    /// Sequence order and insertion order are the same thing here because a
    /// sequence is drawn from [`NEXT_EFFECT_SEQUENCE`] at insert and never
    /// reassigned. The two are asserted equal by
    /// `effect_order_is_sequence_order_even_after_a_prune`.
    #[must_use]
    pub fn effect_order(&self) -> Vec<&str> {
        let mut rows: Vec<(&u64, &str)> =
            self.effects.iter().map(|(id, row)| (&row.seq, id.as_str())).collect();
        rows.sort_unstable();
        rows.into_iter().map(|(_, id)| id).collect()
    }

    /// Insert or replace an effect row.
    pub fn put_effect(&mut self, id: impl Into<String>, row: EffectRow) {
        self.effects.insert(id.into(), row);
    }

    /// Remove an effect row. Returns whether one was present.
    pub fn remove_effect(&mut self, id: &str) -> bool {
        self.effects.remove(id).is_some()
    }

    /// The mailbox row for `envelope_id`.
    #[must_use]
    pub fn mailbox(&self, envelope_id: &str) -> Option<&MailboxRow> {
        self.mailbox.get(envelope_id)
    }

    /// Every mailbox row, in envelope-id order — `(envelope_id, row)`.
    ///
    /// Envelope-id order is deterministic and independent of insertion order
    /// (TESTING.md §1.2); the mailbox store re-sorts a recipient's page by the
    /// envelope's `createdAt` for the reader.
    pub fn mailbox_rows(&self) -> impl Iterator<Item = (&str, &MailboxRow)> {
        self.mailbox.iter().map(|(id, row)| (id.as_str(), row))
    }

    /// Insert or replace a mailbox row.
    pub fn put_mailbox(&mut self, envelope_id: impl Into<String>, row: MailboxRow) {
        self.mailbox.insert(envelope_id.into(), row);
    }

    /// Remove a mailbox row. Returns whether one was present.
    pub fn remove_mailbox(&mut self, envelope_id: &str) -> bool {
        self.mailbox.remove(envelope_id).is_some()
    }

    // TOMBSTONE (#751-P4): `reflection` / `reflection_ids` / `put_reflection`
    // accessed the deleted reflection map (see the tombstone above
    // `MailboxRow`). Nothing replaces them.

    /// The value of a named counter, or `0`.
    #[must_use]
    pub fn counter(&self, name: &str) -> i64 {
        self.counters.get(name).copied().unwrap_or(0)
    }

    /// Set a named counter.
    pub fn set_counter(&mut self, name: &str, value: i64) {
        self.counters.insert(name.to_string(), value);
    }

    /// Every counter, in name order.
    pub fn counters(&self) -> impl Iterator<Item = (&str, i64)> {
        self.counters.iter().map(|(name, value)| (name.as_str(), *value))
    }

    /// Insert a row read back from SQLite at open. Writer-only.
    pub(crate) fn load_effect(&mut self, id: String, row: EffectRow) {
        self.effects.insert(id, row);
    }

    /// Insert a mailbox row read back from SQLite at open. Writer-only.
    pub(crate) fn load_mailbox(&mut self, envelope_id: String, row: MailboxRow) {
        self.mailbox.insert(envelope_id, row);
    }

    /// Insert a row read back from SQLite at open. Writer-only.
    pub(crate) fn load_counter(&mut self, name: String, value: i64) {
        self.counters.insert(name, value);
    }
}

/// The per-commit disk work for the M12 relational tables.
pub(crate) struct RelationalDiff<'a> {
    pub effects: Vec<(&'a str, &'a EffectRow)>,
    pub removed_effects: Vec<String>,
    pub mailbox: Vec<(&'a str, &'a MailboxRow)>,
    pub removed_mailbox: Vec<String>,
    pub counters: Vec<(&'a str, i64)>,
}

fn changed<'a, T: PartialEq>(
    previous: &BTreeMap<String, T>,
    working: &'a BTreeMap<String, T>,
) -> Vec<(&'a str, &'a T)> {
    working
        .iter()
        .filter(|(key, value)| previous.get(*key) != Some(*value))
        .map(|(key, value)| (key.as_str(), value))
        .collect()
}

fn removed<T>(previous: &BTreeMap<String, T>, working: &BTreeMap<String, T>) -> Vec<String> {
    previous.keys().filter(|key| !working.contains_key(*key)).cloned().collect()
}

/// Compute the upsert/delete sets for the M12 tables in one commit.
pub(crate) fn relational_diff<'a>(previous: &Ledgers, working: &'a Ledgers) -> RelationalDiff<'a> {
    RelationalDiff {
        effects: changed(&previous.effects, &working.effects),
        removed_effects: removed(&previous.effects, &working.effects),
        // Mailbox rows both upsert (enqueue, pending→terminal move) and delete
        // (a drained/archived mailbox is pruned), so it carries a delete set.
        mailbox: changed(&previous.mailbox, &working.mailbox),
        removed_mailbox: removed(&previous.mailbox, &working.mailbox),
        counters: working
            .counters
            .iter()
            .filter(|(name, value)| previous.counters.get(*name) != Some(*value))
            .map(|(name, value)| (name.as_str(), *value))
            .collect(),
    }
}

/// The per-commit disk work for the `host_actions` table.
///
/// Exposed to the writer only; it is the exact analogue of
/// [`Ledgers::changed_since`]/[`Ledgers::removed_since`] for documents.
pub(crate) fn host_action_diff<'a>(
    previous: &Ledgers,
    working: &'a Ledgers,
) -> (Vec<(&'a str, &'a HostActionRecord)>, Vec<String>) {
    (working.host_actions_changed_since(previous), working.host_actions_removed_since(previous))
}

/// The rules that cannot live in the schema.
///
/// Plan §5.1: rules expressible as `CHECK`/unique-index constraints go into the
/// DDL **as assertions whose firing is a bug**; everything else — bijectivity,
/// lane-key re-derivation — lives here and surfaces as
/// [`Refusal`]. The writer runs `validate` after every mutation and **before**
/// commit, so a rejected state never reaches disk.
pub trait Validate {
    /// Check every rule this ledger owns.
    ///
    /// # Errors
    /// Returns the [`Refusal`] the caller will see, with a stable machine code
    /// the conformance corpus matches on.
    fn validate(&self) -> Result<(), Refusal>;
}

impl Ledgers {
    /// Validate one in-memory projection's JSON body.
    /// The JSON parse is the only body-bytes-proportional cost in validation
    /// (#123), so isolating it here is what lets [`Ledgers::validate_since`]
    /// skip unchanged bodies.
    fn validate_document(store: &str, record: &DocumentRecord) -> Result<(), Refusal> {
        // Every strangler-shape body is a serialized JSON store document.
        // A non-JSON body would be readable by nothing and is caught here
        // rather than at the next reader's deserialize.
        if !document_body_is_json(record.body()) {
            return Err(Refusal::new(
                "document-body-not-json",
                format!("store '{store}' body is not valid JSON"),
            )
            .with_routes(["write a serialized store document".to_string()]));
        }
        Ok(())
    }

    /// Every rule that is NOT a per-document-body check: host-transaction
    /// intents and the M12 relational tables. These operate on already-parsed
    /// rows (proportional to row count, not body bytes), so both the full and
    /// incremental validators run them whole.
    fn validate_non_documents(&self) -> Result<(), Refusal> {
        for (action_id, record) in &self.host_actions {
            // An intent whose plan cannot be parsed is an intent the recovery
            // pass cannot execute — it would leave a permanently unrecoverable
            // row. Rejecting it before commit 1 is the only moment at which
            // that is still a refusal rather than a wedge.
            let plan = serde_json::from_str::<serde_json::Value>(record.plan_json());
            if !matches!(plan, Ok(serde_json::Value::Object(_))) {
                return Err(Refusal::new(
                    "host-action-plan-not-object",
                    format!(
                        "host action '{action_id}' must carry one schema-discriminated JSON object"
                    ),
                )
                .with_routes(["journal a typed host-transaction payload".to_string()]));
            }
            if record.kind().is_empty() {
                return Err(Refusal::new(
                    "host-action-kind-empty",
                    format!("host action '{action_id}' has no kind"),
                ));
            }
        }
        // --- the M12 relational tables ---------------------------------
        // The `org-supervision-state.ts:815` rule: the explicit counter must
        // stay strictly ahead of every sequence any reader has observed, so a
        // prune of the maximum rows can never let a later insert reuse a value.
        let mut seen: BTreeSet<u64> = BTreeSet::new();
        let mut highest = 0_u64;
        for (id, row) in &self.effects {
            if !seen.insert(row.seq) {
                return Err(Refusal::new(
                    "effect-sequence-duplicated",
                    format!("effect '{id}' reuses sequence {}", row.seq),
                ));
            }
            highest = highest.max(row.seq);
        }
        let next = self.counter(NEXT_EFFECT_SEQUENCE);
        // An untouched company has neither effects nor a counter row; that is
        // "no supervision ledger yet", not a violation.
        let initialized =
            !self.effects.is_empty() || self.counters.contains_key(NEXT_EFFECT_SEQUENCE);
        if initialized && next <= i64::try_from(highest).unwrap_or(i64::MAX) {
            return Err(Refusal::new(
                "effect-sequence-not-monotonic",
                format!("{NEXT_EFFECT_SEQUENCE} is {next}, which is not beyond the highest issued sequence {highest}"),
            ));
        }
        // Mailbox rows: SQLite constrains only NOT NULL, so the rules that make
        // a row meaningful live here. A row with no recipient can never be
        // listed or woken; an unknown lifecycle bucket would silently drop mail
        // out of every scan; a non-JSON body would be unreadable by the drainer.
        for (envelope_id, row) in &self.mailbox {
            if row.person.is_empty() {
                return Err(Refusal::new(
                    "mailbox-recipient-empty",
                    format!("mailbox envelope '{envelope_id}' has no recipient"),
                ));
            }
            // 6-state vocab incl. the #493 `delivered` fence-archive terminal
            // (Fable ruling #5). A row is in exactly one terminal family.
            if !matches!(
                row.state.as_str(),
                "pending" | "delivered" | "accepted" | "superseded" | "rejected" | "resolved"
            ) {
                return Err(Refusal::new(
                    "mailbox-state-unknown",
                    format!("mailbox envelope '{envelope_id}' has unknown state '{}'", row.state),
                ));
            }
            // Columnarized (schema Part B / Fable #7): there is no opaque `body`
            // blob to JSON-parse. The envelope is typed; its logical id must be
            // present so the derived `envelope_id` (`id@person`) is well-formed.
            if row.envelope.id.is_empty() {
                return Err(Refusal::new(
                    "mailbox-envelope-id-empty",
                    format!("mailbox envelope '{envelope_id}' has an empty logical id"),
                ));
            }
        }
        Ok(())
    }

    /// Incremental validation (#123): re-parse only the document bodies that
    /// changed since `previous`, then run every non-document rule. Sound
    /// because chiefd is the single writer and every already-committed body was
    /// validated before it was persisted, so an unchanged body cannot have
    /// become invalid; out-of-band corruption is caught by the full
    /// [`Validate::validate`] at open and by #128's divergence watch.
    pub(crate) fn validate_since(&self, previous: &Ledgers) -> Result<(), Refusal> {
        for (store, record) in self.changed_since(previous) {
            Self::validate_document(store, record)?;
        }
        self.validate_non_documents()
    }
}

impl Validate for Ledgers {
    fn validate(&self) -> Result<(), Refusal> {
        for (store, record) in &self.documents {
            Self::validate_document(store, record)?;
        }
        self.validate_non_documents()
    }
}

/// A committed, immutable view of one company database.
///
/// Published by the writer through `arc-swap` after **every** commit and read
/// by [`CompanyDb::read`](crate::actor::CompanyDb::read) without touching the
/// queue (plan §5.3). A reader is therefore at most one in-flight mutation
/// stale — the same guarantee HTTP callers had against the file stores — and
/// never observes uncommitted state, because a snapshot is only ever
/// constructed from a working set whose transaction committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerSnapshot {
    ledgers: Ledgers,
    commit_seq: u64,
}

impl LedgerSnapshot {
    /// Wrap a committed working set. Writer-only, by construction: nothing
    /// outside the crate can mint a snapshot, so "snapshots are committed
    /// state" is a type-level fact, not a convention.
    pub(crate) fn committed(ledgers: Ledgers, commit_seq: u64) -> Self {
        Self { ledgers, commit_seq }
    }

    /// The committed ledgers.
    #[must_use]
    pub fn ledgers(&self) -> &Ledgers {
        &self.ledgers
    }

    /// How many commits this actor has published, starting at `0` for a
    /// freshly opened database.
    #[must_use]
    pub fn commit_seq(&self) -> u64 {
        self.commit_seq
    }
}

impl std::ops::Deref for LedgerSnapshot {
    type Target = Ledgers;

    fn deref(&self) -> &Ledgers {
        &self.ledgers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledgers() -> Ledgers {
        Ledgers::empty(WallMillis(1_000))
    }

    #[test]
    fn put_document_marks_a_new_projection_without_a_counter() {
        let mut l = ledgers();
        l.put_document("org", "{}");
        let first = Arc::clone(&l.documents["org"].body);
        l.put_document("org", r#"{"a":1}"#);
        assert!(
            !Arc::ptr_eq(&first, &l.documents["org"].body),
            "a rewritten projection receives a distinct body allocation",
        );
        assert_eq!(l.document_body("org"), Some(r#"{"a":1}"#));
    }

    #[test]
    fn every_row_of_one_commit_shares_the_writers_single_wall_reading() {
        let mut l = Ledgers::empty(WallMillis(4_242));
        l.put_document("org", "{}");
        l.put_document("health", "{}");
        assert_eq!(l.document("org").map(DocumentRecord::updated_at), Some(WallMillis(4_242)));
        assert_eq!(l.document("health").map(DocumentRecord::updated_at), Some(WallMillis(4_242)));
    }

    #[test]
    fn changed_and_removed_sets_describe_exactly_one_commits_disk_work() {
        let mut base = ledgers();
        base.put_document("org", "{}");
        base.put_document("health", "{}");

        let mut next = base.clone();
        next.put_document("org", r#"{"v":2}"#);
        next.put_document("runtime", "{}");
        next.remove_document("health");

        let changed: Vec<&str> = next.changed_since(&base).into_iter().map(|(s, _)| s).collect();
        assert_eq!(changed, vec!["org", "runtime"], "untouched rows are not rewritten");
        assert_eq!(next.removed_since(&base), vec!["health".to_string()]);
        assert!(base.changed_since(&base).is_empty(), "a no-op commit writes nothing");
    }

    /// #468: the per-commit cost must not scale with the size of documents the
    /// commit never touched. `run_job` clones the last committed `Ledgers` as
    /// its working set, so the guarantee is: cloning the ledger, then rewriting
    /// ONE store, shares every UNTOUCHED body with the base snapshot (no byte
    /// copy) and mints a fresh body only for the store actually written. Proven
    /// structurally by `Arc` pointer identity — deterministic, not a timing
    /// test that a loaded CI box could flake.
    #[test]
    fn a_commit_shares_untouched_bodies_and_copies_only_what_it_writes() {
        let mut base = ledgers();
        // A deliberately large body stands in for the multi-MB `activity` /
        // `supervision` ledgers the live per-commit clone used to deep-copy.
        let big = format!(r#"{{"big":"{}"}}"#, "x".repeat(4_000_000));
        base.put_document("activity", &big);
        base.put_document("supervision", "{}");

        // The working set of a commit: a clone of the committed snapshot.
        let mut working = base.clone();
        // This commit writes ONLY `supervision`.
        working.put_document("supervision", r#"{"v":2}"#);

        // The untouched `activity` body is the SAME allocation as the base's —
        // the clone bumped a refcount, it did not copy 4 MB.
        let base_activity = &base.documents.get("activity").unwrap().body;
        let working_activity = &working.documents.get("activity").unwrap().body;
        assert!(
            Arc::ptr_eq(base_activity, working_activity),
            "an untouched body must be shared with the base snapshot, never deep-copied",
        );

        // The store this commit actually wrote holds a fresh, distinct body.
        let base_sup = &base.documents.get("supervision").unwrap().body;
        let working_sup = &working.documents.get("supervision").unwrap().body;
        assert!(!Arc::ptr_eq(base_sup, working_sup), "the written store gets its own fresh body",);

        // And the diff the writer persists / feeds names exactly that one store,
        // so neither the disk write nor the change-feed re-scans the 4 MB body.
        let changed: Vec<&str> = working.changed_since(&base).into_iter().map(|(s, _)| s).collect();
        assert_eq!(
            changed,
            vec!["supervision"],
            "only the written store is in the commit's disk work"
        );
    }

    #[test]
    fn validate_refuses_a_body_that_is_not_a_store_document() {
        let mut l = ledgers();
        l.put_document("org", "{}");
        assert!(l.validate().is_ok());

        l.put_document("org", "not json at all");
        let refusal = l.validate().expect_err("non-JSON body must be refused");
        assert_eq!(refusal.code, "document-body-not-json");
        assert!(refusal.message.contains("org"));
        assert!(!refusal.legal_routes.is_empty(), "a refusal names a route that works");
    }

    #[test]
    fn snapshot_derefs_to_committed_ledgers_and_counts_commits() {
        let mut l = ledgers();
        l.put_document("org", "{}");
        let snap = LedgerSnapshot::committed(l, 7);
        assert_eq!(snap.commit_seq(), 7);
        assert_eq!(snap.document_body("org"), Some("{}"), "Deref reaches the ledger API");
    }
}
