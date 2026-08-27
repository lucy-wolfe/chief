//! Row-native durable-fact readers for `chiefd run`'s cycle and health hooks.
//!
//! The host gathers from the same normalized tables that typed HTTP routes and
//! the writer actor own. There is no second ("legacy") store behind these
//! reads: this is the reconciler's primary facts input, read-only, over the
//! one `org.sqlite` the docstore mounts.
//!
//! The polarity differs deliberately by fact, mirroring the TypeScript
//! authority exactly rather than applying one blanket rule:
//!
//! * **runtime-owner** is the actual safety boundary (misjudging it risks
//!   actuating runtime against a session another chiefd owns), so a present-but-
//!   undecodable row is an ERROR — fail closed, exactly like a runtime audit that
//!   did not answer.
//! * TOMBSTONE (chief-home-is-cwd §4c): a **ceo-boot-lease** bullet stood here
//!   and described a fail-open reader over the `boot_lease` table. The lease was
//!   the exclusivity window an attended CEO-only boot held against this very
//!   reconciler; the daemon boots no pane now, so nothing ever takes it and
//!   nothing here can observe it. Both readers are deleted rather than left
//!   answering `false` for ever.
//! * **supervisor-state** / **runtime** feed `health`, which is `FailOpen`
//!   store-wide: an absent or undecodable row is `None`, and the pure collector
//!   already turns `None` into its own incident (`supervisor_not_running`) —
//!   this reader must not pre-empt that by refusing to run.
//! * **launch-intent** is a mutual-exclusion fence, so its polarity is
//!   fail-**safe**: an absent or untrustworthy row reads as the empty set
//!   (CEO-only), exactly like `loadOrganizationLaunchIntentPersonIds` and the
//!   native store's own `read`. Only a store that cannot be opened at all is
//!   an error, and the converge caller treats that as "skip the pass" rather
//!   than actuating from a fabricated empty fence.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension};

use crate::store::health_collect::{
    RuntimeDocObservation, RuntimeReconciliationObservation, SupervisorObservation,
};
use crate::store::{launch_intent_rows, mailbox_rows, runtime_rows};

/// `org-runtime-ownership.ts`'s `storeName` — the runtime-ownership claim
/// (`Owned`/`Foreign`, keyed by runtime socket).
const RUNTIME_OWNER_STORE_KEY: &str = "runtime-owner";

// TOMBSTONE — `document_key(slug, data_root)` lived here and is deleted.
//
// It minted `<slug>@<sha256(data_root)[..12]>`, and its own doc comment gave
// the reason: "the location registry allows the same slug to exist under
// different roots". A company IS its directory now, so there is no registry, no
// second root, and nothing for a slug to be disambiguated against.
//
// It was also a SECOND company-identity hash.
// `host_primitives::rendezvous::company_key(dir)` is the one definition, and
// one definition is the whole point: the manifest slug and the store label
// drifted apart precisely because two places each derived an identity their own
// way.

/// Who currently claims a company's runtime runtime, narrowed to what the D9
/// identity check needs. Mirrors `OrganizationRuntimeOwnership`
/// (`org-runtime-ownership.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOwnerObservation {
    /// `"active"` or `"released"` (or, in principle, an unrecognized future
    /// value — kept raw rather than an enum so a forward-compatible status
    /// this reader does not know about still reads as "somebody claims it",
    /// never silently as released).
    pub status: String,
    /// The runtime socket the active claim names. Required by
    /// `org-runtime-ownership.ts`'s own `validateOwnership` whenever
    /// `status == "active"`; absent otherwise.
    pub socket_name: Option<String>,
}

/// One normalized pending-mail fact used by activity reconciliation.
///
/// The envelope timestamp stays attached to its recipient so the activity
/// fence can compare it with that person's latest commanded stop. Reducing
/// this fact to a recipient set would destroy the ordering fact and let old
/// unread mail undo a later stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMailFact {
    /// The durable recipient identity.
    pub person_id: String,
    /// The envelope creation time, in the canonical ISO form stored on mail.
    pub created_at: String,
}

impl RuntimeOwnerObservation {
    /// Whether this claim is held by a socket other than `our_socket` — the
    /// exact test `IdentityObservation::Foreign` gates on. A `"released"`
    /// status (or any non-`"active"` status) is never foreign: nobody's claim
    /// conflicts with ours.
    #[must_use]
    pub fn foreign_to(&self, our_socket: &str) -> Option<&str> {
        if self.status != "active" {
            return None;
        }
        match self.socket_name.as_deref() {
            Some(holder) if holder != our_socket => Some(holder),
            _ => None,
        }
    }
}

/// Why a durable-fact read that matters for safety could not be trusted.
///
/// Reserved for the readers whose whole point is failing closed
/// (`read_runtime_owner`); the `FailOpen` readers below
/// (`read_supervisor_liveness`, `read_runtime_document`) never return this — a
/// present-but-corrupt row for those degrades to the fail-open default instead,
/// exactly as their TypeScript counterparts do.
#[derive(Debug, thiserror::Error)]
pub enum ReconcilerFactsError {
    /// The read-only connection or the query itself failed.
    #[error("reconciler facts read failed: {0}")]
    Rusqlite(#[from] rusqlite::Error),
    /// A normalized row aggregate was unreadable or violated its domain
    /// contract.  This remains fail-closed at the host boundary.
    #[error("normalized fact read failed: {0}")]
    Normalized(#[from] crate::ChiefdError),
    /// The row exists but its bytes do not decode into the shape this reader
    /// requires, or they decode but name a different company than requested.
    #[error("{store} document for '{slug}' could not be trusted: {detail}")]
    Untrusted {
        /// Which store's row.
        store: &'static str,
        /// The composite key it was read under.
        slug: String,
        /// What went wrong.
        detail: String,
    },
}

/// Read the runtime-ownership claim, keyed by the composite
/// `document_key(slug, data_root)`.
///
/// # Errors
/// [`ReconcilerFactsError::Rusqlite`] if the read itself fails.
/// [`ReconcilerFactsError::Untrusted`] if the typed row carries an unknown
/// status or an active claim without its required socket. This is the D9
/// identity gate, so an unproven claim must never be read as "released".
pub fn read_runtime_owner(
    conn: &Connection,
    composite_slug: &str,
    _organization: &str,
) -> Result<Option<RuntimeOwnerObservation>, ReconcilerFactsError> {
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT status, socket FROM runtime_owner WHERE slug = ?1",
            [composite_slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((status, socket_name)) = row else {
        // Absence is the documented initial state (`initialOrganizationRuntimeOwnership`):
        // a company that has never claimed a runtime is "released", not corrupt.
        return Ok(None);
    };
    // `runtime_owner.slug` is the exact composite namespace key. A row under
    // that key is therefore already scoped to this organization/data root; no
    // JSON self-declared organization field is available or trusted.
    if status != "active" && status != "released" {
        return Err(ReconcilerFactsError::Untrusted {
            store: RUNTIME_OWNER_STORE_KEY,
            slug: composite_slug.to_string(),
            detail: format!("unknown runtime-owner status '{status}'"),
        });
    }
    if status == "active" && socket_name.is_none() {
        return Err(ReconcilerFactsError::Untrusted {
            store: RUNTIME_OWNER_STORE_KEY,
            slug: composite_slug.to_string(),
            detail: "status is 'active' but socketName is missing".to_string(),
        });
    }
    Ok(Some(RuntimeOwnerObservation { status, socket_name }))
}

/// Chiefd-owned (Rust) tri-state supervisor liveness, derived from the
/// `SupervisionReconcile` duty's watermark row in `supervisor_watermarks`
/// ([`crate::store::supervisor_watermark_rows`]).
///
/// #825-prereq: this is the source [`crate::store::supervisor_watermark::record_success`]
/// and [`crate::store::supervisor_watermark::record_failure`] write and this
/// reader consumes, so "never started", "healthy", and "failing" are each
/// representable without ambiguity.
///
/// It replaced `read_supervisor_state`, which narrowed the `supervisor-state`
/// document's `state` half. That document's sole writer was TypeScript's
/// `org-supervisor-state.ts` — it had no bounded failure state and no
/// clear-on-success semantics, and #825 deleted it outright along with the
/// detached supervisor process it described. `read_supervisor_state` outlived
/// its subject with only its own tests for callers and is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorLiveness {
    /// chiefd is up but the `SupervisionReconcile` duty has not yet completed
    /// a first successful cycle for this company (no watermark row at all).
    /// Distinct from both other variants: a caller must not treat this as
    /// healthy (nothing has succeeded yet) NOR as failing (nothing has failed
    /// either — the duty simply has not run).
    NeverStarted,
    /// The duty's most recently recorded event was a success, and no failure
    /// since then is outstanding (a prior failure, if any, was cleared by
    /// this success).
    Healthy {
        /// ISO-8601 stamp of the last successful cycle.
        last_success_at: String,
        /// The duty's expected cadence, as last recorded.
        interval_ms: i64,
    },
    /// The duty's most recently recorded event was a failure that has not
    /// since been cleared by a success. Bounded: only the single most recent
    /// failure is ever carried, never a growing history.
    Failing {
        /// ISO-8601 stamp of the most recent failure.
        last_failure_at: String,
        /// Short, stable failure classification.
        kind: String,
        /// Raw diagnostic (not yet redacted — the health fold's job).
        detail: String,
        /// Consecutive failures recorded since the last success.
        consecutive_failures: u64,
        /// The duty's expected cadence, as last recorded.
        interval_ms: i64,
        /// The last success before this failing streak, when the duty has
        /// ever succeeded at all (`None` when every recorded event has been
        /// a failure since the duty first ran).
        last_success_at: Option<String>,
    },
}

impl SupervisorLiveness {
    /// Translate to the `Option<SupervisorObservation>` shape
    /// `health_collect::collect` already consumes, so wiring this new source
    /// into the health read path changes WHERE the observation comes from,
    /// not what `collect` does with it. `NeverStarted` maps to `None` —
    /// unchanged from today's "absent row" behavior, so the pre-existing
    /// `supervisor_not_running` incident policy is untouched by this packet.
    /// `Healthy` reports a fresh `"running"` heartbeat with no error.
    /// `Failing` still reports `"running"` (the reconcile ledger IS running;
    /// it is the cycle's own outcome that failed) with `last_error` set to
    /// the current bounded failure detail, so the existing `supervisor_error`
    /// incident still fires; `last_heartbeat_at` reflects the true last
    /// success (or `None` when the duty has never yet succeeded), so
    /// staleness detection is not distorted by a failure alone.
    #[must_use]
    pub fn to_observation(&self) -> Option<SupervisorObservation> {
        match self {
            Self::NeverStarted => None,
            Self::Healthy { last_success_at, interval_ms } => Some(SupervisorObservation {
                status: "running".to_string(),
                interval_ms: *interval_ms,
                last_heartbeat_at: Some(last_success_at.clone()),
                last_error: None,
            }),
            Self::Failing { last_success_at, interval_ms, detail, .. } => {
                Some(SupervisorObservation {
                    status: "running".to_string(),
                    interval_ms: *interval_ms,
                    last_heartbeat_at: last_success_at.clone(),
                    last_error: Some(detail.clone()),
                })
            }
        }
    }
}

/// Read the chiefd-owned tri-state supervisor liveness for `composite_slug`,
/// derived from the `SupervisionReconcile` duty watermark. Fail-open, same
/// polarity as [`read_runtime_document`]: an absent or undecodable row reads
/// as [`SupervisorLiveness::NeverStarted`], never an error that would skip
/// the whole health pass.
///
/// # Errors
/// [`rusqlite::Error`] only if the read itself fails.
pub fn read_supervisor_liveness(
    conn: &Connection,
    composite_slug: &str,
) -> Result<SupervisorLiveness, ReconcilerFactsError> {
    let tx = conn.unchecked_transaction()?;
    // A row present but undecodable degrades to NeverStarted rather than an
    // error, matching this store family's fail-open contract --
    // `unwrap_or_default()` on a `Result<Option<_>, _>` collapses `Err(_)` to
    // `None`, the exact fallback the removed `match` spelled out by hand
    // (#992: clippy::manual_unwrap_or_default).
    let state =
        crate::store::supervisor_watermark_rows::reconstruct(&tx, composite_slug, composite_slug)
            .unwrap_or_default();
    let Some(state) = state else {
        return Ok(SupervisorLiveness::NeverStarted);
    };
    let Some(watermark) =
        state.duties.get(crate::store::supervisor_watermark::Duty::SupervisionReconcile.as_str())
    else {
        return Ok(SupervisorLiveness::NeverStarted);
    };
    if watermark.is_failing() {
        return Ok(SupervisorLiveness::Failing {
            last_failure_at: watermark.last_failure_at.clone().unwrap_or_default(),
            kind: watermark.last_failure_kind.clone().unwrap_or_default(),
            detail: watermark.last_failure_detail.clone().unwrap_or_default(),
            consecutive_failures: watermark.consecutive_failures,
            interval_ms: watermark.interval_ms,
            last_success_at: (!watermark.last_success_at.is_empty())
                .then(|| watermark.last_success_at.clone()),
        });
    }
    if watermark.last_success_at.is_empty() {
        // A row exists (the duty has an entry) but carries neither a success
        // nor an outstanding failure — not reachable through the current
        // writers, but treated as NeverStarted rather than fabricating a
        // healthy state from nothing.
        return Ok(SupervisorLiveness::NeverStarted);
    }
    Ok(SupervisorLiveness::Healthy {
        last_success_at: watermark.last_success_at.clone(),
        interval_ms: watermark.interval_ms,
    })
}

/// Read the runtime-projection document, narrowed to
/// [`RuntimeDocObservation`]. Mirrors `readOrganizationRuntimeDocument`
/// (`org-runtime.ts`) as consumed by `collectIncidents`
/// (`org-health-monitor.ts:964-998`): `processHandles` is an object keyed by person id
/// (`Object.keys(runtime.processHandles)`), and `reconciliation` is the in-progress
/// marker the health monitor uses to suppress a mismatch it already knows is
/// being repaired.
///
/// Deliberately fail-**open**, for the same reason as [`read_supervisor_liveness`]:
/// an absent or undecodable document is `None`, which the pure collector reads
/// as "no processes, no reconciliation in flight" — never an error that would skip
/// the whole health pass over one bad projection snapshot.
///
/// # Errors
/// [`rusqlite::Error`] only if the read itself fails.
pub fn read_runtime_document(
    conn: &Connection,
    composite_slug: &str,
) -> Result<Option<RuntimeDocObservation>, ReconcilerFactsError> {
    let tx = conn.unchecked_transaction()?;
    let Some(value) = runtime_rows::reconstruct(&tx, composite_slug)? else {
        return Ok(None);
    };
    let reconciliation = value.reconciliation.map(|marker| RuntimeReconciliationObservation {
        phase: marker.phase,
        started_at: (!marker.started_at.is_empty()).then_some(marker.started_at),
    });
    Ok(Some(RuntimeDocObservation {
        version: Some(i64::from(value.version)),
        socket_name: Some(value.socket_name),
        process_person_ids: value.process_handles.into_keys().collect(),
        reconciliation,
    }))
}

/// Read the launch-intent fence's explicit person set, keyed by the
/// composite `document_key(slug, data_root)`. Mirrors
/// `loadOrganizationLaunchIntentPersonIds` (`org-launch-intent.ts`), parsing
/// the same [`LaunchIntentBody`] the chiefd-native store defines so the two
/// representations cannot drift.
///
/// Deliberately fail-**safe**, matching both the TypeScript loader and the
/// native store's [`launch_intent::read`](crate::store::launch_intent::read):
/// an absent, undecodable, or foreign (wrong version/organization/session, or
/// an empty `updatedAt`) row yields the EMPTY set — the strictest legal fence,
/// CEO-only. The fence's restrictive value is never an error; the caller feeds
/// the set to the activity reconcile as a [`LaunchFence`](crate::store::activity::LaunchFence).
///
/// # Errors
/// [`rusqlite::Error`] only if the read itself fails. Unlike the row-level
/// failures above, an unopened/unreadable STORE is not flattened into the
/// restrictive value: the caller (the converge cycle) treats it as "intent
/// unobservable" and skips the whole pass — actuating from a fabricated
/// empty fence would plan kills for every staffed person, the exact live bug
/// this reader exists to fix.
pub fn read_launch_intent_person_ids(
    conn: &Connection,
    row_slug: &str,
    organization: &str,
) -> Result<BTreeSet<String>, ReconcilerFactsError> {
    let tx = conn.unchecked_transaction()?;
    // `organization` — the caller's manifest slug — is the company's DISPLAY
    // name, and it is what `LaunchIntent.organization` MEANS. `row_slug` is the
    // key the rows are stored under, and it used to carry the name as a prefix
    // (`<slug>@<rootHash>`), so reconstruct was handed the key and stripped it
    // back down. A directory hash carries no name to strip, so the name is
    // passed in from the one place that holds it.
    let stored = launch_intent_rows::reconstruct(&tx, row_slug, organization)?;
    // TOMBSTONE: `|| stored.organization != organization` stood here, and a
    // `stored.session_name != session_name` arm before that (AC6). Both were
    // comparisons of a DERIVED field with the very value it is derived from —
    // `reconstruct` stamps `organization` from the argument above and never
    // reads it off stored bytes, so the arm answered its own question. Foreign
    // authority is excluded structurally instead: the rows are selected by
    // `row_slug`, and one store holds one company.
    if stored.version != 1 || stored.updated_at.trim().is_empty() {
        return Ok(BTreeSet::new());
    }
    Ok(stored.person_ids.into_iter().collect())
}

/// Read every `mailbox/<personId>` row with a NON-EMPTY `pending`
/// bucket, returning recipient-and-created-at facts — the demand half of the TypeScript
/// launcher's pending-mailbox wake scan, which the converge cycle's
/// activity-fence projection unions with the native mailbox's pending
/// recipients so inter-agent work mail keeps its recipient desired-active.
/// The person id is taken from the store key (the authoritative identity
/// `mailboxStoreName` writes under), never from the blob's self-declared
/// `personId`; the blob is only consulted for the pending bucket's
/// emptiness, and the envelope payloads themselves are never decoded.
///
/// #551 parity: a launcher cadence RE-EMISSION (check-in / people-check /
/// goal-watch) is NOT demand — it exists precisely so a settled person is
/// re-woken at dispatch time. The native demand read
/// ([`crate::store::mailbox::pending_demand_recipients`]) and the TypeScript
/// shrink boundary (`peopleWithPendingMailboxWork`) both filter it; this
/// legacy projection read must too, or a settling (e.g. durably-blocked)
/// worker's own unread cadence mail reads as `Requested` on every daemon
/// pass, cancelling every idle park — the live #638 failure.
///
/// Deliberately fail-**open** at the row level, matching
/// [`read_launch_intent_person_ids`]'s contract that a row-level problem is
/// never an error: an absent row, an undecodable blob, or a document whose
/// `pending` half is missing or not an object all simply yield no demand
/// for that person. Demand is a hint that keeps a recipient awake, not an
/// authority boundary — the launch-intent fence still gates it last — so
/// one corrupt archival blob must not wedge the pass.
///
/// # Errors
/// [`rusqlite::Error`] only if the read itself fails — like
/// [`read_launch_intent_person_ids`], an unobservable store is not flattened
/// into an empty answer: the caller (the converge cycle) treats it as
/// "demand unobservable" and skips the whole pass rather than planning
/// kills from a demand picture it could not see.
pub fn read_pending_mail_facts(
    conn: &Connection,
    composite_slug: &str,
) -> Result<Vec<PendingMailFact>, ReconcilerFactsError> {
    read_pending_mail_facts_after(conn, composite_slug, None)
}

/// The pending-mail fact read filtered through the #363 reset watermark.
/// Envelopes at or before `since_exclusive_ms` are durable history, not fresh
/// post-reset launch demand.
pub fn read_pending_mail_facts_after(
    conn: &Connection,
    composite_slug: &str,
    since_exclusive_ms: Option<i64>,
) -> Result<Vec<PendingMailFact>, ReconcilerFactsError> {
    let tx = conn.unchecked_transaction()?;
    let snapshot = mailbox_rows::reconstruct(&tx, composite_slug)?;
    Ok(pending_mail_facts_from_snapshot(snapshot, since_exclusive_ms))
}

/// The same classification, applied to a mailbox snapshot somebody else has
/// already read.
///
/// ONE definition of "this envelope is demand", because there are two callers
/// that must never disagree: this module's own connection read above, and the
/// converge cycle's fallback when no shared facts store is wired, which reads
/// the company's mailbox through its own writer actor. They see the same table
/// and now apply the same three filters to it by construction rather than by
/// two copies of the same three lines.
#[must_use]
pub fn pending_mail_facts_from_snapshot(
    snapshot: mailbox_rows::MailboxSnapshot,
    since_exclusive_ms: Option<i64>,
) -> Vec<PendingMailFact> {
    snapshot
        .entries
        .into_iter()
        .filter(|entry| entry.state == "pending")
        // #551: a launcher cadence re-emission is never demand (see the
        // fn-level docs) — the native read and the TS shrink boundary filter
        // the exact same classification, so the three can never diverge.
        .filter(|entry| !crate::store::mailbox::is_launcher_re_emission(&entry.envelope))
        .filter(|entry| {
            since_exclusive_ms.is_none_or(|since| {
                crate::isotime::parse_iso_millis(&entry.envelope.created_at)
                    .is_some_and(|created| created > since)
            })
        })
        .map(|entry| PendingMailFact {
            person_id: entry.person,
            created_at: entry.envelope.created_at,
        })
        .collect()
}

/// Read the exact people whose session maintenance can still execute.
///
/// Open maintenance is per-person runtime demand. The status predicate is the
/// SQL spelling of [`crate::store::session_maintenance::MaintenanceStatus::is_open`].
/// A query failure is not flattened into no demand because that could park the
/// process which owns the unread request.
pub fn read_open_maintenance_person_ids(
    conn: &Connection,
    composite_slug: &str,
) -> Result<BTreeSet<String>, ReconcilerFactsError> {
    let mut statement = conn.prepare(
        "SELECT DISTINCT person_id FROM maintenance_requests \
         WHERE slug = ?1 AND status IN ('queued','running','applying') ORDER BY person_id",
    )?;
    let rows = statement.query_map([composite_slug], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polarity::StoreKind;
    use crate::store::{open_company_db, open_company_db_readonly};
    use std::collections::BTreeMap;

    // -- read_runtime_owner ---------------------------------------------------

    fn write_doc(conn: &Connection, slug: &str, store: &str, blob: &str) {
        conn.execute(
            "INSERT INTO org_documents(slug, store, blob, generation, updated_at) \
             VALUES(?1, ?2, ?3, 1, ?4)",
            rusqlite::params![slug, store, blob, "2026-07-20T00:00:00.000Z"],
        )
        .expect("insert");
    }

    fn write_runtime_owner(conn: &Connection, slug: &str, status: &str, socket: Option<&str>) {
        conn.execute(
            "INSERT INTO runtime_owner(slug, status, socket, claimed_at, validated_at, released_at) \
             VALUES(?1, ?2, ?3, NULL, NULL, NULL)",
            rusqlite::params![slug, status, socket],
        )
        .expect("runtime-owner insert");
    }

    fn write_launch_intent(conn: &Connection, slug: &str, person_ids: &[&str]) {
        let tx = conn.unchecked_transaction().expect("launch-intent fixture transaction");
        let document = launch_intent_rows::LaunchIntent {
            version: 1,
            organization: "cobalt".to_string(),
            person_ids: person_ids.iter().map(|person_id| (*person_id).to_string()).collect(),
            updated_at: "2026-07-22T00:00:00.000Z".to_string(),
            attributions: BTreeMap::new(),
            extra: BTreeMap::new(),
        };
        launch_intent_rows::publish(&tx, slug, &document).expect("launch-intent row publish");
        tx.commit().expect("launch-intent fixture commit");
    }

    fn write_runtime_projection(conn: &Connection, slug: &str) {
        conn.execute(
            "INSERT INTO runtime(slug, version, observed_at, \
             socket_name, status, recon_phase, recon_started_at) \
             VALUES(?1, 1, ?2, 'cobalt-bison', 'running', \
                    'in_progress', ?2)",
            rusqlite::params![slug, "2026-07-20T00:00:00.000Z"],
        )
        .expect("runtime row insert");
        for (person, process_handle) in [("alice", "%1"), ("bob", "%2")] {
            conn.execute(
                "INSERT INTO runtime_process_handles(slug, person, process_handle) VALUES(?1, ?2, ?3)",
                rusqlite::params![slug, person, process_handle],
            )
            .expect("runtime process handle insert");
        }
    }

    fn write_mailbox_entry(conn: &Connection, slug: &str, person: &str, id: &str, state: &str) {
        conn.execute(
            "INSERT INTO mailbox( \
                 slug, envelope_id, id, person, from_person_id, to_person_id, message, urgency, \
                 created_at, state, updated_at \
             ) VALUES(?1, ?2, ?3, ?4, 'chief', ?4, 'fixture mail', 'normal', ?5, ?6, 1)",
            rusqlite::params![
                slug,
                format!("{id}@{person}"),
                id,
                person,
                "2026-07-22T00:00:00.000Z",
                state,
            ],
        )
        .expect("mailbox row insert");
    }

    fn company_org_sqlite() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("org.sqlite");
        let conn = open_company_db(&path).expect("open writable fixture");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS org_documents (slug TEXT NOT NULL, store TEXT NOT NULL, \
             blob TEXT NOT NULL, generation INTEGER NOT NULL, updated_at TEXT NOT NULL, \
             PRIMARY KEY (slug, store));",
        )
        .expect("ddl");
        (dir, path)
    }

    #[test]
    fn an_absent_runtime_owner_row_is_none_not_an_error() {
        let (_dir, path) = company_org_sqlite();
        let conn = open_company_db_readonly(&path).expect("open read-only");
        assert_eq!(read_runtime_owner(&conn, "co@abc", "cobalt").expect("reads"), None);
    }

    #[test]
    fn a_released_claim_is_never_foreign() {
        let (dir_guard, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_runtime_owner(&conn, "co@abc", "released", None);
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let owner = read_runtime_owner(&conn, "co@abc", "cobalt").expect("reads").expect("present");
        assert_eq!(owner.status, "released");
        assert_eq!(owner.foreign_to("cobalt-bison"), None, "a released claim never conflicts");
        drop(dir_guard);
    }

    #[test]
    fn an_active_claim_on_a_different_socket_is_foreign() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_runtime_owner(&conn, "co@abc", "active", Some("cobalt-bison"));
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let owner = read_runtime_owner(&conn, "co@abc", "cobalt").expect("reads").expect("present");
        assert_eq!(owner.foreign_to("some-other-socket"), Some("cobalt-bison"));
        assert_eq!(owner.foreign_to("cobalt-bison"), None, "our own socket is never foreign");
    }

    #[test]
    fn a_generic_runtime_owner_blob_cannot_override_the_typed_claim() {
        // The normalized table CHECK makes an unknown status impossible to
        // persist. The real regression is stronger: even if a stale arbitrary
        // blob remains on disk, this authority reader must ignore it entirely.
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_runtime_owner(&conn, "co@abc", "released", None);
            write_doc(
                &conn,
                "co@abc",
                "runtime-owner",
                r#"{"status":"active","socketName":"foreign-socket"}"#,
            );
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let owner = read_runtime_owner(&conn, "co@abc", "cobalt").expect("reads").expect("present");
        assert_eq!(owner.status, "released");
        assert_eq!(owner.foreign_to("our-socket"), None);
    }

    #[test]
    fn a_runtime_owner_row_is_scoped_by_its_composite_key() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_runtime_owner(&conn, "co@other", "active", Some("foreign-socket"));
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        assert_eq!(read_runtime_owner(&conn, "co@abc", "cobalt").expect("reads"), None);
    }

    #[test]
    fn an_active_claim_missing_its_socket_fails_closed() {
        // `validateOwnership` (`org-runtime-ownership.ts`) requires an explicit
        // socket whenever status is "active"; a row that skipped that is
        // exactly as untrustworthy as unparseable bytes.
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_runtime_owner(&conn, "co@abc", "active", None);
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let error = read_runtime_owner(&conn, "co@abc", "cobalt").expect_err("must fail closed");
        assert!(matches!(error, ReconcilerFactsError::Untrusted { .. }), "{error:?}");
    }

    // TOMBSTONE (chief-home-is-cwd §4c): four `ceo_boot_lease_is_active` /
    // `active_ceo_boot_lease_observation` tests stood here — absent, unexpired,
    // expired, malformed. They pinned the fail-open polarity of a reader over
    // the `boot_lease` table, and both reader and table are deleted with the
    // daemon-side CEO boot that was their only writer.

    // -- read_supervisor_liveness (#825-prereq: chiefd-owned tri-state) -------

    /// A `Ledgers` pre-loaded with the duty's CURRENT SQL-row state, so a
    /// second `record_success`/`record_failure` call in the same test sees
    /// what an earlier call already persisted rather than starting fresh —
    /// mirroring how `chiefd/src/run.rs`'s own duty commits always read
    /// through a `Ledgers` the writer actor keeps warm across passes.
    fn ledgers_seeded_from(
        tx: &rusqlite::Transaction<'_>,
        slug: &str,
        at_millis: i64,
    ) -> crate::ledger::Ledgers {
        let mut ledgers = crate::ledger::Ledgers::empty(crate::clock::WallMillis(at_millis));
        if let Some(state) = crate::store::supervisor_watermark_rows::reconstruct(tx, slug, slug)
            .expect("reconstruct")
        {
            let body = serde_json::to_string(&state).expect("encode");
            ledgers.load_document(
                crate::store::supervisor_watermark::SupervisorWatermarkStore::NAME.to_string(),
                crate::ledger::DocumentRecord::from_row(body, crate::clock::WallMillis(at_millis)),
            );
        }
        ledgers
    }

    fn record_reconcile_success(conn: &Connection, slug: &str, at_millis: i64) {
        let tx = conn.unchecked_transaction().expect("tx");
        let mut ledgers = ledgers_seeded_from(&tx, slug, at_millis);
        let ctx =
            crate::store::context::CompanyContext::new(slug, "chief", ["chief"].map(String::from));
        crate::store::supervisor_watermark::record_success(
            &mut ledgers,
            &ctx,
            crate::store::supervisor_watermark::Duty::SupervisionReconcile,
            at_millis,
        );
        let body = ledgers
            .document_body(crate::store::supervisor_watermark::SupervisorWatermarkStore::NAME)
            .expect("body");
        crate::store::supervisor_watermark_rows::backfill_supervisor_watermark(
            &tx,
            slug,
            body.as_bytes(),
        )
        .expect("backfill");
        tx.commit().expect("commit");
    }

    fn record_reconcile_failure(
        conn: &Connection,
        slug: &str,
        at_millis: i64,
        kind: &str,
        detail: &str,
    ) {
        let tx = conn.unchecked_transaction().expect("tx");
        let mut ledgers = ledgers_seeded_from(&tx, slug, at_millis);
        let ctx =
            crate::store::context::CompanyContext::new(slug, "chief", ["chief"].map(String::from));
        crate::store::supervisor_watermark::record_failure(
            &mut ledgers,
            &ctx,
            crate::store::supervisor_watermark::Duty::SupervisionReconcile,
            at_millis,
            kind,
            detail,
        );
        let body = ledgers
            .document_body(crate::store::supervisor_watermark::SupervisorWatermarkStore::NAME)
            .expect("body");
        crate::store::supervisor_watermark_rows::backfill_supervisor_watermark(
            &tx,
            slug,
            body.as_bytes(),
        )
        .expect("backfill");
        tx.commit().expect("commit");
    }

    #[test]
    fn no_watermark_row_is_never_started() {
        let (_dir, path) = company_org_sqlite();
        let conn = open_company_db_readonly(&path).expect("open read-only");
        assert_eq!(
            read_supervisor_liveness(&conn, "co@abc").expect("reads"),
            SupervisorLiveness::NeverStarted
        );
        assert_eq!(
            read_supervisor_liveness(&conn, "co@abc").expect("reads").to_observation(),
            None,
            "never-started must not read as healthy (Some) nor synthesize a failure"
        );
    }

    #[test]
    fn a_recorded_success_is_healthy() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            record_reconcile_success(&conn, "co@abc", 1_784_116_800_000);
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let liveness = read_supervisor_liveness(&conn, "co@abc").expect("reads");
        assert_eq!(
            liveness,
            SupervisorLiveness::Healthy {
                last_success_at: crate::isotime::iso_millis(1_784_116_800_000),
                interval_ms: crate::store::supervisor_watermark::Duty::SupervisionReconcile
                    .interval_ms(),
            }
        );
        let observation = liveness.to_observation().expect("healthy maps to Some");
        assert_eq!(observation.status, "running");
        assert_eq!(observation.last_error, None);
    }

    #[test]
    fn a_recorded_failure_before_any_success_is_failing_not_never_started() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            record_reconcile_failure(
                &conn,
                "co@abc",
                1_784_116_800_000,
                "cycle_input_gather_failed",
                "boom",
            );
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let liveness = read_supervisor_liveness(&conn, "co@abc").expect("reads");
        match &liveness {
            SupervisorLiveness::Failing { kind, consecutive_failures, last_success_at, .. } => {
                assert_eq!(kind, "cycle_input_gather_failed");
                assert_eq!(*consecutive_failures, 1);
                assert_eq!(*last_success_at, None, "no success has ever happened yet");
            }
            other => panic!("expected Failing, got {other:?}"),
        }
        assert!(liveness.to_observation().is_some(), "failing still surfaces as an observation");
    }

    #[test]
    fn a_failure_after_a_prior_success_clears_healthy_and_reports_failing() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            record_reconcile_success(&conn, "co@abc", 1_784_116_800_000);
            record_reconcile_failure(
                &conn,
                "co@abc",
                1_784_116_830_000,
                "reconcile_refused",
                "ledger mutate refused",
            );
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let liveness = read_supervisor_liveness(&conn, "co@abc").expect("reads");
        match &liveness {
            SupervisorLiveness::Failing { last_success_at, .. } => {
                assert_eq!(
                    last_success_at.as_deref(),
                    Some(crate::isotime::iso_millis(1_784_116_800_000).as_str()),
                    "the prior success is preserved even while failing"
                );
            }
            other => panic!("expected Failing, got {other:?}"),
        }
    }

    #[test]
    fn a_success_after_a_failure_clears_it_back_to_healthy() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            record_reconcile_failure(&conn, "co@abc", 1_784_116_800_000, "reconcile_refused", "x");
            record_reconcile_success(&conn, "co@abc", 1_784_116_830_000);
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        assert_eq!(
            read_supervisor_liveness(&conn, "co@abc").expect("reads"),
            SupervisorLiveness::Healthy {
                last_success_at: crate::isotime::iso_millis(1_784_116_830_000),
                interval_ms: crate::store::supervisor_watermark::Duty::SupervisionReconcile
                    .interval_ms(),
            },
            "a subsequent success clears the failure entirely"
        );
    }

    #[test]
    fn many_consecutive_failures_never_grow_past_one_row() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            for n in 0..40 {
                record_reconcile_failure(
                    &conn,
                    "co@abc",
                    1_784_116_800_000 + n * 1_000,
                    "reconcile_refused",
                    "x",
                );
            }
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM supervisor_watermarks WHERE slug='co@abc' AND duty='supervision_reconcile'",
                    [],
                    |r| r.get(0),
                )
                .expect("count");
            assert_eq!(count, 1, "the bounded singleton row never grows with repeated failures");
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        match read_supervisor_liveness(&conn, "co@abc").expect("reads") {
            SupervisorLiveness::Failing { consecutive_failures, .. } => {
                assert_eq!(consecutive_failures, 40);
            }
            other => panic!("expected Failing, got {other:?}"),
        }
    }

    // -- read_runtime_document -------------------------------------------------

    #[test]
    fn an_absent_runtime_document_is_none() {
        let (_dir, path) = company_org_sqlite();
        let conn = open_company_db_readonly(&path).expect("open read-only");
        assert!(read_runtime_document(&conn, "co@abc").expect("reads").is_none());
    }

    #[test]
    fn a_present_runtime_document_decodes_process_handles_and_reconciliation() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_runtime_projection(&conn, "co@abc");
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let observation = read_runtime_document(&conn, "co@abc").expect("reads").expect("present");
        assert_eq!(observation.version, Some(1));
        assert_eq!(observation.socket_name.as_deref(), Some("cobalt-bison"));
        assert_eq!(observation.process_person_ids, vec!["alice".to_string(), "bob".to_string()]);
        let reconciliation = observation.reconciliation.expect("reconciliation present");
        assert_eq!(reconciliation.phase, "in_progress");
    }

    #[test]
    fn a_malformed_runtime_document_fails_open_to_none() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_doc(&conn, "co@abc", "runtime", "{ not json");
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        assert!(read_runtime_document(&conn, "co@abc").expect("reads").is_none());
    }

    // -- read_launch_intent_person_ids -----------------------------------------

    #[test]
    fn an_absent_launch_intent_row_is_the_empty_fence() {
        let (_dir, path) = company_org_sqlite();
        let conn = open_company_db_readonly(&path).expect("open read-only");
        assert!(
            read_launch_intent_person_ids(&conn, "co@abc", "cobalt").expect("reads").is_empty(),
            "absence is the strictest fence, not an error"
        );
    }

    #[test]
    fn a_present_launch_intent_row_decodes_its_person_set() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_launch_intent(&conn, "cobalt@abc", &["alice", "bob"]);
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let ids = read_launch_intent_person_ids(&conn, "cobalt@abc", "cobalt").expect("reads");
        assert_eq!(
            ids.iter().map(String::as_str).collect::<Vec<_>>(),
            ["alice", "bob"],
            "the exact set the TypeScript staffing lifecycle wrote"
        );
    }

    /// WHAT THIS USED TO CLAIM: `a_malformed_or_foreign_launch_intent_row_
    /// authorizes_nobody` looped over three `org_documents` BLOBS — unparseable
    /// bytes, a fence naming another company, and an empty `updatedAt` — and
    /// asserted each fenced everybody out.
    ///
    /// Two of the three arms it aimed at no longer exist. `organization` is
    /// DERIVED by `reconstruct` from the caller's own manifest slug, so "a fence
    /// written for another company" is not a state the rows can hold; and the
    /// blob table it wrote to is not what this reader reads, so all three cases
    /// passed on absence rather than on the rule they named.
    ///
    /// The one live arm — no trustworthy timestamp — is asserted here against
    /// real rows: `updatedAt` DERIVES from `MAX(org_events.at)`, so a fence row
    /// with no audit event behind it has none, and an unattested fence
    /// authorizes nobody.
    #[test]
    fn a_fence_row_with_no_audit_event_behind_it_authorizes_nobody() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            conn.execute(
                "INSERT INTO launch_intent(slug, person_id) VALUES('co@abc', 'alice')",
                [],
            )
            .expect("hand-inserted fence row, no org_events");
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        assert!(
            read_launch_intent_person_ids(&conn, "co@abc", "cobalt").expect("reads").is_empty(),
            "an unattested fence must fence everybody out"
        );
    }

    // -- read_pending_mail_facts ---------------------------------------------

    #[test]
    fn pending_mail_facts_read_only_rows_with_pending_mail() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_mailbox_entry(&conn, "co@abc", "alice", "msg-1", "pending");
            // Bob's mailbox exists but is drained: no demand.
            write_mailbox_entry(&conn, "co@abc", "bob", "msg-2", "accepted");
            // Another company's rows are namespaced out.
            write_mailbox_entry(&conn, "other@def", "carol", "msg-3", "pending");
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let facts = read_pending_mail_facts(&conn, "co@abc").expect("reads");
        assert_eq!(
            facts.iter().map(|fact| fact.person_id.as_str()).collect::<Vec<_>>(),
            ["alice"],
            "pending mail is demand; a drained mailbox and another company's rows are not"
        );
    }

    #[test]
    fn pending_mail_facts_exclude_rows_at_or_before_the_reset_watermark() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_mailbox_entry(&conn, "co@abc", "alice", "msg-1", "pending");
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let since =
            crate::isotime::parse_iso_millis("2026-07-23T00:00:00.000Z").expect("valid watermark");
        assert!(
            read_pending_mail_facts_after(&conn, "co@abc", Some(since)).expect("reads").is_empty(),
            "pre-reset normalized mailbox rows are not fresh launch demand"
        );
    }

    #[test]
    fn a_pending_launcher_re_emission_is_never_demand() {
        // #551 parity (the live #638 failure): a settling worker's own unread
        // cadence mail must not read as `Requested` — it exists to re-wake
        // them at dispatch time, not to pin them against the idle park.
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_mailbox_entry(&conn, "co@abc", "alice", "msg-1", "pending");
            // The three `manager-check-in:`/`manager-people-check:`/
            // `manager-goal-watch:` ids stood beside this one. All three were
            // emitted by the protected goal loop and nothing emits them now, so
            // the only surviving cadence re-emission is the `supervision-` one.
            let id = "supervision-deadbeef";
            conn.execute(
                "INSERT INTO mailbox( \
                     slug, envelope_id, id, person, from_person_id, to_person_id, message, urgency, \
                     created_at, state, updated_at \
                 ) VALUES('co@abc', ?1, ?2, 'bob', 'launcher', 'bob', 'cadence', 'normal', \
                          '2026-07-22T00:00:00.000Z', 'pending', 1)",
                rusqlite::params![format!("{id}@bob"), id],
            )
            .expect("re-emission row insert");
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let facts = read_pending_mail_facts(&conn, "co@abc").expect("reads");
        assert_eq!(
            facts.iter().map(|fact| fact.person_id.as_str()).collect::<Vec<_>>(),
            ["alice"],
            "real pending mail is demand; every launcher cadence re-emission is not"
        );
    }

    #[test]
    fn an_absent_mailbox_store_is_no_demand() {
        let (_dir, path) = company_org_sqlite();
        let conn = open_company_db_readonly(&path).expect("open read-only");
        assert!(read_pending_mail_facts(&conn, "co@abc").expect("reads").is_empty());
    }

    #[test]
    fn malformed_mailbox_rows_degrade_without_failing_the_read() {
        // Malformed blobs left in the retired generic table are not mailbox
        // authority and cannot hide a good row from the normalized reader.
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            write_doc(&conn, "co@abc", "mailbox/broken", "{ not json");
            // This blob decodes, but its pending half is not the old shape.
            write_doc(
                &conn,
                "co@abc",
                "mailbox/shapeless",
                r#"{"schemaVersion":1,"personId":"shapeless","pending":[]}"#,
            );
            write_mailbox_entry(&conn, "co@abc", "alice", "msg-1", "pending");
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        let facts = read_pending_mail_facts(&conn, "co@abc").expect("reads");
        assert_eq!(
            facts.iter().map(|fact| fact.person_id.as_str()).collect::<Vec<_>>(),
            ["alice"],
            "retired malformed blobs are ignored while normalized pending mail remains visible"
        );
    }

    #[test]
    fn maintenance_facts_include_only_queued_running_and_applying_people() {
        let (_dir, path) = company_org_sqlite();
        {
            let conn = open_company_db(&path).expect("writable handle");
            for (ordinal, person, status) in [
                (1, "queued-person", "queued"),
                (2, "running-person", "running"),
                (3, "applying-person", "applying"),
                (4, "completed-person", "completed"),
                (5, "failed-person", "failed"),
                (6, "skipped-person", "skipped"),
            ] {
                conn.execute(
                    "INSERT INTO maintenance_requests( \
                         slug,id,ordinal,person_id,requested_by,action,status,requested_at) \
                     VALUES('co@abc',?1,?2,?3,'chief','fresh_session',?4,'2026-08-17T00:00:00.000Z')",
                    rusqlite::params![format!("request-{ordinal}"), ordinal, person, status],
                )
                .expect("maintenance row");
            }
            conn.execute(
                "INSERT INTO maintenance_requests( \
                     slug,id,ordinal,person_id,requested_by,action,status,requested_at) \
                 VALUES('other@def','other-request',1,'other-person','chief','fresh_session', \
                        'queued','2026-08-17T00:00:00.000Z')",
                [],
            )
            .expect("other company row");
        }
        let conn = open_company_db_readonly(&path).expect("open read-only");
        assert_eq!(
            read_open_maintenance_person_ids(&conn, "co@abc").expect("reads"),
            ["applying-person", "queued-person", "running-person"]
                .map(str::to_owned)
                .into_iter()
                .collect(),
            "terminal maintenance and another company's request are not runtime demand"
        );
    }
}
