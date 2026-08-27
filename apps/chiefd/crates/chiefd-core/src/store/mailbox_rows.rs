//! The mailbox ROW implementation (org-data-normalization P0, N-mailbox).
//!
//! Reconstruct the whole-company mailbox from the columnarized `mailbox` table,
//! and publish it by diffing against the current rows — the N-mailbox half of
//! the repository seam, mirroring [`crate::store::organization_rows`]. Runs
//! inside chiefd-core because the rows live in `chief.db`; the raw
//! `&Transaction` comes from `CompanyDb::in_transaction`, and the fenced-publish
//! machinery from [`crate::store::rows_txn`].
//!
//! # The three document families collapse into one table
//!
//! The TypeScript store held three `org_documents` families — `mailbox/` (hot
//! pending), `mailbox-archive/` (settled envelopes) and `mailbox-index/` (a
//! messageId→shard lookup). In the relational model:
//! * `mailbox/`        → rows with `state='pending'`.
//! * `mailbox-archive/`→ the SAME table, a terminal `state`. No separate table.
//! * `mailbox-index/`  → a SQL lookup on the `mailbox_id`/PK indexes. DERIVED,
//!   no table.
//!
//! # Derived, never stored (schema Part B / Fable #7)
//!
//! `recipients` is the sorted sibling set sharing the logical `id` (a broadcast
//! writes one row per recipient; the whole 44-way list is NOT denormalized into
//! every row). `organization` is the company slug; `schemaVersion` is a
//! constant. None are columns.
//!
//! # #493 disjointness (Fable ruling #5)
//!
//! `delivered` is the fence-archive terminal; `accepted`/`superseded`/
//! `rejected`/`resolved` are the pane-drain terminals. The two families are
//! disjoint — a row is in exactly one — enforced by the single `state` column
//! and asserted by [`crate::store::mailbox::MailboxState::terminal_family`].
//!
//! Item D: a published entry carries NO unmodeled keys. Publish REJECTS any
//! `extra` with [`UNMODELED_KEYS`] (+ the offending paths), never silently drops.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::Refusal;
use crate::store::activity::rows as activity_rows;
use crate::store::mailbox::{
    HealthIncidentRef, MailboxEnvelope, MailboxState, Urgency, MAILBOX_ENVELOPE_SCHEMA_VERSION,
};
use crate::store::rows_txn::{apply_and_emit, EventTouch};
use crate::ChiefdError;

/// A publish carried a key the row model does not represent (item D).
pub const UNMODELED_KEYS: &str = "unmodeled-keys";
/// A published entry is not representable (bad state vocab, or an envelope_id
/// that does not match `id@person`). Maps to 422.
pub const MAILBOX_INVALID: &str = "mailbox-invalid";

/// The `ChangeFeed`/`org_documents` store key for one person's mailbox —
/// `mailbox/<personId>`, the exact string the TS
/// `createOrganizationMailboxWakeWatcher` filters on
/// (`event.store.startsWith("mailbox/")`) and the byte-for-byte analogue of
/// `org-mailbox-store.ts`'s `mailboxStoreName`. Used to publish a `WatchEvent`
/// when a row-path mailbox write commits (the write bypasses the Ledgers
/// snapshot, so `run_job`'s fan-out never emits it — see
/// `CompanyDb::publish_row_feed_hint`).
#[must_use]
pub fn mailbox_store_name(person: &str) -> String {
    format!("mailbox/{person}")
}

/// A SQL failure reading/writing the mailbox rows is a store failure.
fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("mailbox-rows", e)
}

fn invalid(message: impl Into<String>) -> ChiefdError {
    ChiefdError::from(Refusal::new(MAILBOX_INVALID, message))
}

/// `rusqlite::Error` lifts into `ChiefdError` via [`corrupt`] so `apply_and_emit`
/// (bound `E: From<rusqlite::Error>`) can `?` on its internal SQL.
pub struct RowsSqlError(pub ChiefdError);
impl From<rusqlite::Error> for RowsSqlError {
    fn from(e: rusqlite::Error) -> Self {
        RowsSqlError(store_failure(e))
    }
}
impl From<ChiefdError> for RowsSqlError {
    fn from(e: ChiefdError) -> Self {
        RowsSqlError(e)
    }
}

// ---- the aggregate DTO ----------------------------------------------------

/// One mailbox row as an aggregate entry: the typed envelope plus this row's
/// recipient, lifecycle bucket and stamp. The envelope fields are FLATTENED to
/// the entry level so a single `extra` map captures any unmodeled key anywhere
/// in the entry (item D). `envelope.recipients`/`organization`/`schemaVersion`
/// are derived on read and ignored on write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxEntry {
    /// The columnarized envelope (fields flattened onto the entry).
    #[serde(flatten)]
    pub envelope: MailboxEnvelope,
    /// This row's recipient (the mailbox owner).
    pub person: String,
    /// The lifecycle bucket (6-state vocab incl. `delivered`).
    pub state: String,
    /// Epoch millis of the last write.
    pub updated_at: i64,
    /// Any key the row model does not represent — rejected by publish (item D).
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl MailboxEntry {
    /// The `id@person` composite that is the row's primary key.
    #[must_use]
    pub fn envelope_id(&self) -> String {
        format!("{}@{}", self.envelope.id, self.person)
    }
}

/// The whole-company mailbox: every row, sorted by `envelope_id` for a
/// deterministic aggregate independent of storage order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MailboxSnapshot {
    /// Every mailbox entry, `envelope_id`-sorted.
    pub entries: Vec<MailboxEntry>,
}

// ---- reconstruct (read path) ---------------------------------------------

/// Reconstruct the whole-company mailbox for `company_slug` from the rows. The
/// mailbox is always present (possibly empty), so this returns a snapshot rather
/// than an `Option`.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure; [`ChiefdError::Corrupt`] on an unreadable
/// row.
/// Reconstruct the whole-company mailbox snapshot (every recipient row).
pub fn reconstruct(
    tx: &Transaction<'_>,
    company_slug: &str,
) -> Result<MailboxSnapshot, ChiefdError> {
    reconstruct_filtered(tx, company_slug, None)
}

/// Reconstruct ONE person's mailbox (rows WHERE person=?), with each
/// envelope's `recipients` COMPLETED from all sibling rows sharing its logical
/// id (a per-person filter alone would drop co-recipients). The per-person read
/// the flipped caller API uses; the whole-company `reconstruct` is impl detail.
pub fn reconstruct_person(
    tx: &Transaction<'_>,
    company_slug: &str,
    person: &str,
) -> Result<MailboxSnapshot, ChiefdError> {
    reconstruct_filtered(tx, company_slug, Some(person))
}

fn reconstruct_filtered(
    tx: &Transaction<'_>,
    company_slug: &str,
    person: Option<&str>,
) -> Result<MailboxSnapshot, ChiefdError> {
    struct Raw {
        envelope_id: String,
        id: String,
        person: String,
        from_person_id: String,
        to_person_id: String,
        message: String,
        urgency: String,
        reply_to: Option<String>,
        h_fp: Option<String>,
        h_kind: Option<String>,
        h_rcp: Option<String>,
        created_at: String,
        state: String,
        updated_at: i64,
    }
    let mut stmt = tx
        .prepare(
            "SELECT envelope_id, id, person, from_person_id, to_person_id, message, urgency, \
             reply_to, health_fingerprint, health_kind, health_recipient_person_id, \
             created_at, state, updated_at \
             FROM mailbox WHERE slug = ?1 AND (?2 IS NULL OR person = ?2) ORDER BY envelope_id",
        )
        .map_err(store_failure)?;
    let raws = stmt
        .query_map(params![company_slug, person], |r| {
            Ok(Raw {
                envelope_id: r.get(0)?,
                id: r.get(1)?,
                person: r.get(2)?,
                from_person_id: r.get(3)?,
                to_person_id: r.get(4)?,
                message: r.get(5)?,
                urgency: r.get(6)?,
                reply_to: r.get(7)?,
                h_fp: r.get(8)?,
                h_kind: r.get(9)?,
                h_rcp: r.get(10)?,
                created_at: r.get(11)?,
                state: r.get(12)?,
                updated_at: r.get(13)?,
            })
        })
        .map_err(store_failure)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(store_failure)?;
    drop(stmt);

    // recipients = the sorted sibling set sharing the logical envelope id.
    // Whole-company: group the rows we already read. Per-person: the filtered
    // rows only contain THIS person, so complete recipients from all sibling
    // rows sharing the logical ids this person received.
    let mut recipients: BTreeMap<String, Vec<String>> = BTreeMap::new();
    match person {
        None => {
            for raw in &raws {
                recipients.entry(raw.id.clone()).or_default().push(raw.person.clone());
            }
        }
        Some(p) => {
            let mut cstmt = tx
                .prepare(
                    "SELECT id, person FROM mailbox \
                     WHERE slug = ?1 AND id IN (SELECT id FROM mailbox WHERE slug = ?1 AND person = ?2)",
                )
                .map_err(store_failure)?;
            let pairs = cstmt
                .query_map(params![company_slug, p], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(store_failure)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(store_failure)?;
            for (id, recipient) in pairs {
                recipients.entry(id).or_default().push(recipient);
            }
        }
    }
    for people in recipients.values_mut() {
        people.sort();
        people.dedup();
    }

    let mut entries = Vec::with_capacity(raws.len());
    for raw in raws {
        // A row whose bucket cannot be read is corruption, not silently "no mail".
        if MailboxState::parse(&raw.state).is_none() {
            return Err(crate::error::corrupt_store_because(
                "mailbox-rows",
                format!("stored mailbox state '{}' is outside the modelled vocabulary", raw.state),
            ));
        }
        let urgency = Urgency::parse(&raw.urgency).ok_or_else(|| {
            crate::error::corrupt_store_because(
                "mailbox-rows",
                format!("stored urgency '{}' is outside the modelled vocabulary", raw.urgency),
            )
        })?;
        let health_incident = raw.h_fp.clone().map(|fingerprint| HealthIncidentRef {
            fingerprint,
            kind: raw.h_kind.clone().unwrap_or_default(),
            recipient_person_id: raw.h_rcp.clone().unwrap_or_default(),
        });
        let envelope = MailboxEnvelope {
            schema_version: MAILBOX_ENVELOPE_SCHEMA_VERSION,
            id: raw.id.clone(),
            organization: company_slug.to_string(),
            from_person_id: raw.from_person_id,
            to: raw.to_person_id,
            recipients: recipients.get(&raw.id).cloned().unwrap_or_default(),
            body: raw.message,
            urgency,
            reply_to: raw.reply_to,
            health_incident,
            created_at: raw.created_at,
        };
        let _ = raw.envelope_id; // PK is derived from id@person; kept only for order.
        entries.push(MailboxEntry {
            envelope,
            person: raw.person,
            state: raw.state,
            updated_at: raw.updated_at,
            extra: BTreeMap::new(),
        });
    }
    Ok(MailboxSnapshot { entries })
}

/// Every person id with at least one mailbox row (SELECT DISTINCT person). The
/// mailbox table is per-company (chief.db), so no slug filter — the analogue of
/// enumerating the old `mailbox/<personId>` hot-row owners. Read-only.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL fault.
pub fn list_persons(tx: &Transaction<'_>, company_slug: &str) -> Result<Vec<String>, ChiefdError> {
    let mut stmt = tx
        .prepare("SELECT DISTINCT person FROM mailbox WHERE slug = ?1 ORDER BY person")
        .map_err(store_failure)?;
    let people = stmt
        .query_map(params![company_slug], |r| r.get::<_, String>(0))
        .map_err(store_failure)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(store_failure)?;
    Ok(people)
}

// ---- publish (diff/write path) -------------------------------------------

/// Publish a whole mailbox into the rows as one atomic current-state operation.
///
/// Rejects unmodeled keys (item D) BEFORE writing, validates each entry, then
/// diffs the incoming snapshot against the current rows at ENTITY granularity
/// (one entity == one `(envelope,recipient)` row): each added or changed row is
/// rewritten and emits one `org_events` row; each removed row emits a delete.
///
/// # Errors
/// [`UNMODELED_KEYS`] / [`MAILBOX_INVALID`] refusals (map to 422); SQL failures
/// as [`ChiefdError::StoreFailure`].
pub fn publish(
    tx: &Transaction<'_>,
    company_slug: &str,
    incoming: &MailboxSnapshot,
) -> Result<i64, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    validate_snapshot(incoming)?;

    let current = reconstruct(tx, company_slug)?;
    // The event stamp is the max entry updated_at (ms) rendered ISO-8601-ish; a
    // mailbox publish has no manifest-style `updated_at`, so the newest touched
    // row supplies the clock the caller already stamped onto the rows.
    let at = incoming
        .entries
        .iter()
        .map(|e| e.updated_at)
        .max()
        .map(|ms| ms.to_string())
        .unwrap_or_default();

    let current_by_id: BTreeMap<String, &MailboxEntry> =
        current.entries.iter().map(|e| (e.envelope_id(), e)).collect();
    let incoming_by_id: BTreeMap<String, &MailboxEntry> =
        incoming.entries.iter().map(|e| (e.envelope_id(), e)).collect();

    apply_and_emit::<RowsSqlError, _>(tx, company_slug, &at, "", |tx| {
        let mut touches = Vec::new();
        // Removals: a current row absent from the incoming snapshot.
        for envelope_id in current_by_id.keys() {
            if !incoming_by_id.contains_key(envelope_id) {
                tx.execute(
                    "DELETE FROM mailbox WHERE slug = ?1 AND envelope_id = ?2",
                    params![company_slug, envelope_id],
                )?;
                touches.push(EventTouch::new(
                    "mailbox",
                    envelope_id.clone(),
                    "delete",
                    "mailbox",
                    company_slug,
                ));
            }
        }
        // Upserts: an incoming row that is new or changed.
        for (envelope_id, entry) in &incoming_by_id {
            let unchanged = current_by_id.get(envelope_id).map(|c| *c == *entry).unwrap_or(false);
            if unchanged {
                continue;
            }
            upsert_entry(tx, company_slug, envelope_id, entry)?;
            touches.push(EventTouch::new(
                "mailbox",
                envelope_id.clone(),
                "upsert",
                "mailbox",
                company_slug,
            ));
        }
        Ok(touches)
    })
    .map_err(|RowsSqlError(e)| e)
}

/// Fence-FREE per-person DELTA (org-data-normalization P0, N8): upsert/delete
/// ONLY the given envelopes for one person, emitting one `org_events` touch per
/// changed row. This is the O(1)-append path — NO whole-company snapshot read or
/// publish (that would be O(all) at the wire, the amplification the shard shape
/// was built to kill). No caller sequence fence: mailbox concurrency is
/// per-person, disjoint persons touch disjoint `id@person` rows, and a
/// same-envelope race serializes on the row (envelope_id PK, last write wins) —
/// a whole-company org_events CAS would serialize every person's mailbox writes,
/// which is exactly what the perf spirit forbids. Returns the new max seq.
///
/// # Errors
/// [`UNMODELED_KEYS`]/[`MAILBOX_INVALID`] refusals (map to 422), a
/// person-mismatch refusal, either authorization refusal from
/// `authorize_delta`, or [`ChiefdError::StoreFailure`] on a SQL fault.
pub fn delta(
    tx: &Transaction<'_>,
    company_slug: &str,
    person: &str,
    upserts: &[MailboxEntry],
    deletes: &[String],
    at: &str,
    actor: &str,
) -> Result<i64, ChiefdError> {
    // Every upsert must belong to THIS person — the delta is person-scoped and a
    // cross-person write here would bypass the per-person fence discipline.
    for entry in upserts {
        if entry.person != person {
            return Err(ChiefdError::refused(
                "mailbox-delta-person-mismatch",
                format!("mailbox delta for '{person}' carries an entry for '{}'", entry.person),
            ));
        }
    }
    // WHO IS ASKING, as opposed to WHOSE MAILBOX. See `authorize_delta`.
    authorize_delta(tx, company_slug, person, upserts, deletes, actor)?;
    // Reuse the snapshot validators (item D + representability) on just the delta.
    let validation = MailboxSnapshot { entries: upserts.to_vec() };
    reject_unmodeled_keys(&validation)?;
    validate_snapshot(&validation)?;

    apply_and_emit::<RowsSqlError, _>(tx, company_slug, at, "", |tx| {
        let mut touches = Vec::new();
        for envelope_id in deletes {
            // Scope the delete to this company AND person so a mistyped id can
            // never touch another company's or another person's row.
            tx.execute(
                "DELETE FROM mailbox WHERE slug = ?1 AND envelope_id = ?2 AND person = ?3",
                params![company_slug, envelope_id, person],
            )?;
            touches.push(EventTouch::new(
                "mailbox",
                envelope_id.clone(),
                "delete",
                "mailbox",
                company_slug,
            ));
        }
        for entry in upserts {
            let envelope_id = entry.envelope_id();
            upsert_entry(tx, company_slug, &envelope_id, entry)?;
            touches.push(EventTouch::new(
                "mailbox",
                envelope_id,
                "upsert",
                "mailbox",
                company_slug,
            ));
        }
        // A pending envelope is durable work arriving. Re-arm the recipient's
        // automatic-idle lease in this exact transaction so an already-planned
        // settle cannot win between the mailbox append and the next runtime
        // reconciliation. `accepted` is included for the message_start
        // boundary: it is the durable proof a queued envelope entered Pi.
        if upserts.iter().any(|entry| matches!(entry.state.as_str(), "pending" | "accepted")) {
            touches.extend(activity_rows::rearm_automatic_settle_for_activity(
                tx,
                company_slug,
                person,
                at,
            )?);
        }
        Ok(touches)
    })
    .map_err(|RowsSqlError(e)| e)
}

/// The actor for a mailbox write chiefd makes ON ITS OWN BEHALF, in-process,
/// with no HTTP caller anywhere — the `mailbox_view` settle/wipe helpers and
/// the delivery sink.
///
/// It is the empty string ON PURPOSE and must stay that way. `authorize_delta`
/// enforces only when the actor NAMES A PERSON ROW, so this value is not judged,
/// which is correct: these writes are the runtime acting on its own store, not
/// somebody asking it to.
///
/// It is a NAMED CONSTANT rather than a bare `""` at each site so that a later
/// reader does not "fix" the empty string by inventing a principal chiefd does
/// not have. The absence is the decision, and an absence that is not written
/// down reads as an oversight.
pub const IN_PROCESS_ACTOR: &str = "";

/// A caller deleted from a mailbox that is not its own. Maps to 422.
pub const MAILBOX_DELTA_FOREIGN_DELETE: &str = "mailbox-delta-foreign-delete";
/// A caller upserted into somebody else's mailbox an entry that is not a
/// delivery from itself. Maps to 422.
pub const MAILBOX_DELTA_NOT_A_DELIVERY: &str = "mailbox-delta-not-a-delivery";

/// **A mailbox delta is either CONSUMPTION or DELIVERY.**
///
/// # Why this route takes a rule and not a binding
///
/// `person` is WHOSE MAILBOX, never WHO IS ASKING — and the product calls the
/// route BOTH ways. `organization-intercom.ts` sends `personId = recipient` when
/// one person messages another (`publishMailboxEnvelope`), and `personId =
/// context.personId` when a pane settles its own queue (`settleMailboxEntry`,
/// `settleMailboxBatch`). Binding `person` to the caller the way the staffing
/// routes bind their requester would therefore refuse EVERY message the product
/// sends. The authorization question is a different one:
///
/// 1. **Delivery.** An upsert is authorized when `envelope.from_person_id ==
///    actor`. **That equality is the wire definition of a delivery**: the sender
///    field the recipient later renders as the author must name the principal
///    that presented the credential. Without it a leaf worker could put words in
///    the CEO's mouth inside the CEO's own inbox, which is the quietest forgery
///    in the product.
/// 2. **Consumption.** An upsert is also authorized when the caller is
///    re-writing a row it ALREADY HOLDS in its OWN mailbox — `person == actor`
///    and a row for this `envelope_id` exists. That is the drain path exactly:
///    `settleMailboxEntry` reads the envelope back, changes only `state`, and
///    posts it again, so the sender it carries is the sender already stored.
/// 3. **Deletes are consumption only.** A delete destroys a durable record. A
///    caller may delete only from its OWN mailbox, whatever the entries claim —
///    there is no such thing as delivering a deletion.
///
/// # Why case 2 is "a row you already hold" and not "your own mailbox"
///
/// The first draft of this rule allowed a caller ANY `from_person_id` when
/// writing into its own mailbox, on the grounds that self-forgery harms nobody.
/// That is true if and only if nothing but that person ever reads their mailbox,
/// and TWO other readers exist:
///
/// * `apps/web` serves `/api/companies/{slug}/people/{personId}/mailbox`
///   (`server/Mailbox.ts` → `personMailbox`), which forwards every envelope
///   OPAQUE to an operator's browser. `from_person_id` is what that page renders
///   as the sender, for a person who is not the caller.
/// * chiefd's OWN launch demand branches on it:
///   `reconciler_facts::read_pending_mail_facts_after` filters through
///   [`crate::store::mailbox::is_launcher_re_emission`], which is
///   `from_person_id == "launcher"` plus a `supervision-` id prefix. A
///   self-upsert wearing that shape makes your own pending mail stop counting as
///   demand — a person silently suppressing their own wake.
///
/// So an unrestricted self-upsert manufactures evidence of a message that was
/// never sent, and manufactures it for a third party. Constraining case 2 to a
/// row that already exists costs the drain path nothing (it only ever hands back
/// what it just read) and closes both.
///
/// It also subsumes the `launcher` case rather than special-casing it:
/// `from_person_id: "launcher"` envelopes are written by chiefd's OWN delivery
/// sink in-process (`runtime::delivery_sink`), so a pane settling one is
/// re-writing a row that already exists and passes, while a caller MINTING one
/// over HTTP is claiming to be the runtime and is refused.
///
/// # A mixed batch fails WHOLE
///
/// This is the first route in the sweep where authorization is a PER-ENTRY
/// question: one delta may carry several entries with different verdicts. The
/// whole request is refused if ANY entry fails, before a single row is written,
/// and the refusal names the offending `envelope_id` and the sender it carried.
/// Partial application was rejected twice over — the delta already runs in ONE
/// `BEGIN IMMEDIATE` and answers a single `seq`, so a partial apply would have
/// to invent a per-entry outcome shape no caller reads; and silently dropping an
/// entry is the exact failure mode this module refuses elsewhere, where an
/// unmodeled key is [`UNMODELED_KEYS`] rather than a drop.
///
/// # The actor rule
///
/// Enforced only when `actor` NAMES A PERSON ROW
/// ([`crate::store::org_ops::actor_names_a_person`]). `actor` is free-form audit
/// prose in this corpus — `operator`, `op` and the empty string all appear and
/// name nobody — so gating on its CONTENT needs a placeholder allowlist that
/// rots. Sound with credentials off (nothing authenticates, nothing is judged)
/// and on (the route hands core the authenticated principal, so what arrives is
/// always a real person).
///
/// # Errors
/// [`MAILBOX_DELTA_FOREIGN_DELETE`] or [`MAILBOX_DELTA_NOT_A_DELIVERY`]; a
/// `rusqlite` failure lifts through [`store_failure`].
fn authorize_delta(
    tx: &Transaction<'_>,
    company_slug: &str,
    person: &str,
    upserts: &[MailboxEntry],
    deletes: &[String],
    actor: &str,
) -> Result<(), ChiefdError> {
    if !crate::store::org_ops::actor_names_a_person(tx, company_slug, actor)
        .map_err(store_failure)?
    {
        return Ok(());
    }
    if actor != person {
        if let Some(envelope_id) = deletes.first() {
            return Err(ChiefdError::refused(
                MAILBOX_DELTA_FOREIGN_DELETE,
                format!(
                    "caller '{actor}' may delete only from its own mailbox; '{envelope_id}' is in \
                     '{person}'s"
                ),
            ));
        }
    }
    for entry in upserts {
        // A delivery from the caller — the wire definition, and the only way to
        // put a NEW envelope into any mailbox including your own.
        if entry.envelope.from_person_id == actor {
            continue;
        }
        // Or the drain path: re-writing a row you already hold in your own
        // mailbox, whose stored sender you are handing straight back.
        let envelope_id = entry.envelope_id();
        if actor == person && row_exists(tx, company_slug, &envelope_id, person)? {
            continue;
        }
        return Err(ChiefdError::refused(
            MAILBOX_DELTA_NOT_A_DELIVERY,
            format!(
                "caller '{actor}' may write into '{person}'s mailbox only a delivery from itself \
                 or a settle of a row it already holds; entry '{envelope_id}' is from '{}' and \
                 no such row exists",
                entry.envelope.from_person_id
            ),
        ));
    }
    Ok(())
}

/// Whether this exact `(company, envelope_id, person)` row is already durable.
fn row_exists(
    tx: &Transaction<'_>,
    company_slug: &str,
    envelope_id: &str,
    person: &str,
) -> Result<bool, ChiefdError> {
    tx.query_row(
        "SELECT 1 FROM mailbox WHERE slug = ?1 AND envelope_id = ?2 AND person = ?3",
        params![company_slug, envelope_id, person],
        |_| Ok(true),
    )
    .optional()
    .map(|found| found.unwrap_or(false))
    .map_err(store_failure)
}

/// Reject any `extra` key on any entry (item D). NEVER silently drops.
fn reject_unmodeled_keys(m: &MailboxSnapshot) -> Result<(), ChiefdError> {
    let mut paths = Vec::new();
    for entry in &m.entries {
        for key in entry.extra.keys() {
            paths.push(format!("entries.{}.{key}", entry.envelope_id()));
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!("mailbox carries unmodeled keys the row model cannot store: {}", paths.join(", ")),
    )))
}

/// Validate every entry's state vocab and envelope_id consistency (422 on fail).
fn validate_snapshot(m: &MailboxSnapshot) -> Result<(), ChiefdError> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for entry in &m.entries {
        if MailboxState::parse(&entry.state).is_none() {
            return Err(invalid(format!(
                "entry '{}' has unknown state '{}'",
                entry.envelope_id(),
                entry.state
            )));
        }
        if entry.person.is_empty() || entry.envelope.id.is_empty() {
            return Err(invalid("entry has an empty id or recipient"));
        }
        if !seen.insert(entry.envelope_id()) {
            return Err(invalid(format!("duplicate entry '{}'", entry.envelope_id())));
        }
    }
    Ok(())
}

/// Columnar upsert of one entry (composite `ON CONFLICT(slug, envelope_id)`).
/// `recipients`/`organization`/`schemaVersion` are DERIVED and NOT written; `slug`
/// is the company scope (delta #35), NOT a stored `organization` denormalization.
fn upsert_entry(
    tx: &Transaction<'_>,
    company_slug: &str,
    envelope_id: &str,
    entry: &MailboxEntry,
) -> rusqlite::Result<()> {
    let e = &entry.envelope;
    let h = e.health_incident.as_ref();
    tx.execute(
        "INSERT INTO mailbox(slug, envelope_id, id, person, from_person_id, to_person_id, message, \
         urgency, reply_to, health_fingerprint, health_kind, \
         health_recipient_person_id, created_at, state, updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15) \
         ON CONFLICT(slug, envelope_id) DO UPDATE SET id=?3, person=?4, from_person_id=?5, \
         to_person_id=?6, message=?7, urgency=?8, reply_to=?9, health_fingerprint=?10, \
         health_kind=?11, health_recipient_person_id=?12, created_at=?13, state=?14, \
         updated_at=?15",
        params![
            company_slug,
            envelope_id,
            e.id,
            entry.person,
            e.from_person_id,
            e.to,
            e.body,
            e.urgency.as_str(),
            e.reply_to,
            h.map(|h| &h.fingerprint),
            h.map(|h| &h.kind),
            h.map(|h| &h.recipient_person_id),
            e.created_at,
            entry.state,
            entry.updated_at,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
