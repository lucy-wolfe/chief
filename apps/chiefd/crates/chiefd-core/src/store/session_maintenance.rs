//! Session maintenance: row persistence and structural validation for the
//! durable queue behind the automatic `compact`.
//!
//! chiefd's FIRST port of these verbs lived in this module as `Ledgers`-blob
//! mutations (`mutate`/`transact`/`reconcile_people`) with zero production
//! callers — a shadow duplicate of `org-session-maintenance.ts` that misled
//! readers and needed parallel maintenance while doing nothing. It was deleted
//! (Step 9). The verbs came back properly in
//! [`crate::store::session_maintenance_ops`] and
//! [`crate::store::company_session_action`], as total functions of
//! `(ledger, input, at)` that the writer actor (`actor::session_lifecycle`,
//! `actor::runtime_verbs`) and the eight `/v1/org/session-maintenance/*`
//! routes call for real, and that `conformance_session_maintenance.rs` replays
//! the whole golden corpus against. What lives HERE is the shared half both of
//! them build on:
//!
//! * the ledger types (`SessionMaintenanceLedger` and its records), which the
//!   `/v1/org/session-maintenance/*` HTTP routes deserialize;
//! * the two bounds on a retry — [`SESSION_MAINTENANCE_MAX_ATTEMPTS`] and
//!   [`session_maintenance_retry_delay_ms`] — and the two interruption
//!   diagnostics whose exact text is the protocol. Each of those had a second
//!   copy in a caller until #751/G14, and the two retry ladders had drifted
//!   apart; one definition is what stops that recurring;
//! * [`validate`], the structural invariant check [`rows::publish`] runs on
//!   every incoming ledger;
//! * [`rows`], the row-native persistence: `publish` (one live caller — the
//!   writer actor's `session_maintenance_publish`) applies a whole ledger
//!   inside one transaction with its audit event, and `reconstruct` rebuilds
//!   the ledger the read route and the reconciler's writer-side consumers
//!   use. Membership reconciliation stays TypeScript-side by design
//!   (`rows::publish`'s own docs).
//!
//! # Revisionless maintenance writes
//!
//! Session-maintenance never owns a logical revision. Its normalized rows are
//! the authority; [`rows::publish`] applies each row diff and its audit event
//! inside one transaction. This keeps maintenance mutations atomic without a
//! second version counter to maintain.
//!
//! # Polarity: `FailClosed`
//!
//! The store stays `FailClosed`: an unreadable ledger read as "empty" would
//! hide an in-flight company-wide reset, writing over a ledger chiefd could
//! not read would destroy audit history, and there is no legitimate "throw
//! the maintenance queue away". An **absent** ledger is not corruption: it is
//! a company that has never needed maintenance.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::Refusal;
use crate::isotime::parse_iso_millis;
use crate::polarity::{FailClosed, StoreKind};
use crate::store::CompanyContext;

/// Schema version of the ledger body.
pub const SESSION_MAINTENANCE_SCHEMA_VERSION: u32 = 1;

/// Lifetime attempt ceiling for a **non-company** request (D6); enforced by
/// [`validate`] on every published ledger. A human company action has no
/// ceiling — its bound on damage is temporal instead, and is
/// [`session_maintenance_retry_delay_ms`] below.
pub const SESSION_MAINTENANCE_MAX_ATTEMPTS: u32 = 3;

/// The attempt a retry ladder was asked about is not a retry.
pub const SESSION_MAINTENANCE_RETRY_ATTEMPT_INVALID: &str =
    "session-maintenance-retry-attempt-invalid";

/// The first recovered attempt's admission delay; each later attempt doubles it
/// up to [`SESSION_MAINTENANCE_RETRY_MAX_DELAY_MS`].
const SESSION_MAINTENANCE_RETRY_BASE_DELAY_MS: i64 = 250;

/// The longest a recovered attempt waits before it is admissible.
pub const SESSION_MAINTENANCE_RETRY_MAX_DELAY_MS: i64 = 30_000;

/// How long after `at` a recovered attempt may first be claimed.
///
/// A human fleet action has no lifetime retry ceiling (unlike an automatic
/// request, which [`SESSION_MAINTENANCE_MAX_ATTEMPTS`] caps), so the bound on
/// damage is temporal instead: each new process attempt is admitted with
/// bounded exponential backoff, and a repeatedly dying Pi therefore cannot turn
/// durable recovery into a hot restart loop.
///
/// # Why it lives here and not next to a caller
///
/// The same durable field (`retryNotBefore`) is written by TWO recovery paths —
/// [`crate::store::session_maintenance_ops::recover_interrupted`] when a
/// process dies, and
/// the deleted company-action reconcile
/// when a company action rebinds. Each one had its own copy of this
/// ladder, with different base delays (1000 ms and 250 ms), and each copy had
/// its own unit test pinning its own number — so both looked correct alone
/// while a successor's admission time depended on which way its predecessor
/// died. The conformance corpus records 250 ms, which is what the TypeScript
/// did; one definition is what stops the two from drifting again (#751/G14).
///
/// # Errors
/// [`SESSION_MAINTENANCE_RETRY_ATTEMPT_INVALID`] for an attempt below two —
/// attempt one is the original request and is never a retry.
pub fn session_maintenance_retry_delay_ms(next_attempt: u32) -> Result<i64, Refusal> {
    if next_attempt < 2 {
        return Err(Refusal::new(
            SESSION_MAINTENANCE_RETRY_ATTEMPT_INVALID,
            "Session maintenance retry attempt must be a safe integer greater than one",
        ));
    }
    // Clamped at 2^16 before the cap is applied, exactly as the TS did: the
    // shift is what would overflow, and the cap is what actually bounds it.
    let exponent = (next_attempt - 2).min(16);
    let scaled = SESSION_MAINTENANCE_RETRY_BASE_DELAY_MS.saturating_mul(1_i64 << exponent);
    Ok(scaled.min(SESSION_MAINTENANCE_RETRY_MAX_DELAY_MS))
}

/// The attribution a company request carries. Never caller-supplied.
pub const COMPANY_REQUESTED_BY: &str = "human";

/// The diagnostic a request carries when the Pi PROCESS holding it died.
///
/// Compared by exact string equality in
/// the deleted company-action reconcile,
/// because a request an older launcher terminalized with this text is
/// recoverable work rather than a real native failure. That comparison is the
/// reason this is a constant and not an inline literal: the text IS the
/// protocol — which is also why it has exactly one definition, written by
/// [`crate::store::session_maintenance_ops::recover_interrupted`] and read here.
pub const SESSION_MAINTENANCE_PROCESS_INTERRUPTION_ERROR: &str = "The Pi process ended before session maintenance completed; the durable attempt was recovered on the next exact runtime startup.";

/// The diagnostic a company request carries when its target parked before the
/// maintenance ran. See [`SESSION_MAINTENANCE_PROCESS_INTERRUPTION_ERROR`] for
/// why the exact text matters.
pub const SESSION_MAINTENANCE_TARGET_PARKED_ERROR: &str =
    "The targeted person parked before company session maintenance completed.";

/// A ledger chiefd produced failed its own validation. Never caller-caused.
pub const LEDGER_INVALID: &str = "session-maintenance-ledger-invalid";

/// What kind of maintenance.
///
/// ONE VARIANT. `FreshSession` and `SetModel` are deleted with
/// `org_maintain_session`, the tool that was their only source — operator
/// ruling, 2026-08-24: *"remove the whole feature… For number one yes remove
/// fresh session compact and set model"*.
///
/// The enum SURVIVES rather than collapsing to a bare marker, because the
/// automatic compaction still queues through this pipeline and its rows still
/// carry an action on the wire. A single-variant enum is the honest shape for
/// "one kind, and the column that names it is still there".
///
/// `is_company_capable` and `company_reason` went with the company-action
/// family, which is deleted whole: nothing in production could queue one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceAction {
    /// Native context compaction.
    Compact,
}

impl MaintenanceAction {
    /// The wire spelling, which is also the request-id suffix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
        }
    }
}

/// Where a request is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceStatus {
    /// Waiting to be claimed.
    Queued,
    /// Claimed by an exact live Pi.
    Running,
    /// Being applied.
    Applying,
    /// Finished successfully.
    Completed,
    /// Finished unsuccessfully.
    Failed,
    /// Closed without running.
    Skipped,
}

impl MaintenanceStatus {
    /// Whether this status still occupies its person.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Applying)
    }

    /// Whether this status is terminal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Skipped)
    }
}

/// One durable maintenance request. Field names and optionality mirror the
/// TypeScript record exactly, so the Phase-2 import is a parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceRequest {
    /// `session-maintenance:<n>:<person>:<action>`.
    pub id: String,
    /// What kind of maintenance.
    pub action: MaintenanceAction,
    /// Whose session.
    pub person_id: String,
    /// Who asked. `"human"` for a company action.
    pub requested_by: String,
    /// Why.
    pub reason: String,
    /// Whether the supervisor raised it rather than a person.
    pub automatic: bool,
    /// Where it is in its life.
    pub status: MaintenanceStatus,
    /// When it was queued.
    pub requested_at: String,
    /// When it was claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When it reached a terminal status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Bounded failure diagnostic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// One means the original request; crash recovery creates a **new**
    /// immutable record and increments this rather than erasing history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// The record this one replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_from_request_id: Option<String>,
    /// Durable admission boundary for a successor created after process exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_not_before: Option<String>,
    /// Claim triple, all three present or all three absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_process_id: Option<i64>,
    /// See [`Self::claimed_process_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_session_id: Option<String>,
    /// See [`Self::claimed_process_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_token: Option<String>,
    /// Completion triple, all three present or all three absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_process_id: Option<i64>,
    /// See [`Self::completed_process_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_session_id: Option<String>,
    /// See [`Self::completed_process_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_claim_token: Option<String>,
    /// The human control-plane action that owns this per-person request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_action_id: Option<String>,
    /// Force requests interrupt the current Pi turn before claiming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
    /// Interrupt receipt, all four present or all four absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_process_id: Option<i64>,
    /// See [`Self::interrupted_process_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_session_id: Option<String>,
    /// See [`Self::interrupted_process_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_claim_token: Option<String>,
    /// See [`Self::interrupted_process_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<String>,
    /// Native compact branch boundary, both present or both absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_session_id: Option<String>,
    /// See [`Self::compact_session_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_anchor_entry_id: Option<String>,
    /// The native compaction entry proving the requested branch was compacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_compaction_entry_id: Option<String>,
    // TOMBSTONE: `requested_model_provider` and `requested_model`, the two
    // fields a `set_model` request carried. Deleted with the action; Pi owns an
    // agent's model.
    /// Keys the row model does not model, captured verbatim. A row publish
    /// rejects any non-empty `extra` with 422 `unmodeled-keys` (item D).
    #[serde(flatten, default)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

impl MaintenanceRequest {
    /// Whether this request belongs to a human company action.
    #[must_use]
    pub fn is_company_request(&self) -> bool {
        self.company_action_id.as_ref().is_some_and(|id| !id.trim().is_empty())
    }
}

// TOMBSTONE: `CompanySessionAction` and its `CompanyActionTarget`. The
// in-memory shape of #54's whole-roster fanout, deleted with the family.

/// The durable ledger, byte-compatible with `session-maintenance.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMaintenanceLedger {
    /// Always [`SESSION_MAINTENANCE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The company.
    pub organization: String,
    /// Request ids in creation order.
    pub request_order: Vec<String>,
    /// Requests by id.
    pub requests: BTreeMap<String, MaintenanceRequest>,
    /// When the ledger was created.
    pub created_at: String,
    /// When it was last published.
    pub updated_at: String,
    /// Unmodeled top-level keys, captured verbatim; a row publish rejects any
    /// (item D).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl SessionMaintenanceLedger {
    /// A fresh ledger for `organization`, stamped `at`.
    #[must_use]
    pub fn initial(organization: &str, at: &str) -> Self {
        Self {
            schema_version: SESSION_MAINTENANCE_SCHEMA_VERSION,
            organization: organization.to_string(),
            request_order: Vec::new(),
            requests: BTreeMap::new(),
            created_at: at.to_string(),
            updated_at: at.to_string(),
            extra: Default::default(),
        }
    }

    /// Requests in creation order.
    pub fn ordered_requests(&self) -> impl Iterator<Item = &MaintenanceRequest> {
        self.request_order.iter().filter_map(|id| self.requests.get(id))
    }

    /// The request with this id.
    #[must_use]
    pub fn request(&self, id: &str) -> Option<&MaintenanceRequest> {
        self.requests.get(id)
    }

    // TOMBSTONE: `begin_fresh_session_launch` and
    // `complete_fresh_session_launch`. The actuator-owned fresh launch existed
    // only for the deleted `fresh_session` action, and `writer.rs` held its
    // only callers.
}

/// The session-maintenance store.
pub struct SessionMaintenanceStore;

impl StoreKind for SessionMaintenanceStore {
    const NAME: &'static str = "session-maintenance";
    type Body = SessionMaintenanceLedger;
}

impl FailClosed for SessionMaintenanceStore {}

/// Validate the ledger's structural invariants.
///
/// `allow_orphaned_open` is the ported two-phase read: a ledger loaded from
/// disk may legitimately contain an open request for a departed person (the
/// manifest committed first), and [`reconcile_people`] closes it. Only after
/// that is the stricter form applied.
fn validate(
    ledger: &SessionMaintenanceLedger,
    ctx: &CompanyContext,
    allow_orphaned_open: bool,
    allow_unbounded_diagnostics: bool,
) -> Result<(), Refusal> {
    let invalid = |detail: String| Refusal::new(LEDGER_INVALID, detail);

    if ledger.schema_version != SESSION_MAINTENANCE_SCHEMA_VERSION
        || ledger.organization != ctx.slug()
    {
        return Err(invalid("session maintenance ledger is invalid".to_string()));
    }
    let ordered: BTreeSet<&String> = ledger.request_order.iter().collect();
    if ordered.len() != ledger.request_order.len()
        || ledger.request_order.len() != ledger.requests.len()
    {
        return Err(invalid("session maintenance request order is invalid".to_string()));
    }

    for id in &ledger.request_order {
        let Some(request) = ledger.requests.get(id) else {
            return Err(invalid(format!("session maintenance request '{id}' is invalid")));
        };
        let bad = request.id != *id
            || (!allow_orphaned_open
                && request.status.is_open()
                && !(ctx.knows_person(&request.person_id)
                    && (ctx.knows_person(&request.requested_by)
                        || (request.is_company_request()
                            && request.requested_by == COMPANY_REQUESTED_BY))))
            || parse_iso_millis(&request.requested_at).is_none()
            || request.attempt.is_some_and(|attempt| {
                attempt < 1 || (!request.is_company_request() && attempt > SESSION_MAINTENANCE_MAX_ATTEMPTS)
            })
            || request.recovered_from_request_id.as_ref().is_some_and(|id| id.is_empty())
            || request.retry_not_before.as_ref().is_some_and(|at| {
                !request.is_company_request()
                    || request.status != MaintenanceStatus::Queued
                    || parse_iso_millis(at).is_none()
                    || parse_iso_millis(at) < parse_iso_millis(&request.requested_at)
            })
            || request.claimed_process_id.is_some_and(|pid| pid < 1)
            || request.claimed_session_id.as_ref().is_some_and(|s| s.trim().is_empty())
            || request.claim_token.as_ref().is_some_and(|s| s.trim().is_empty())
            || request.completed_process_id.is_some_and(|pid| pid < 1)
            || request.completed_session_id.as_ref().is_some_and(|s| s.trim().is_empty())
            || request.completion_claim_token.as_ref().is_some_and(|s| s.trim().is_empty())
            || request.company_action_id.as_ref().is_some_and(|s| s.trim().is_empty())
            // #319: a single-target force carries `force` with NO companyActionId
            // (TS allows it), so the pre-#319 clause rejecting that is DROPPED.
            // The company-action clause below still requires force+human+existing
            // action whenever a companyActionId IS present.
            // A `companyActionId` can no longer be produced by anything: the
            // verbs, routes and store that minted company actions are deleted,
            // and the ledger no longer carries the actions to point at. Any
            // value here is therefore invalid rather than merely unmatched.
            || request.company_action_id.is_some()
            || !all_or_none(&[
                request.interrupted_process_id.is_none(),
                request.interrupted_session_id.is_none(),
                request.interrupted_claim_token.is_none(),
                request.interrupted_at.is_none(),
            ])
            || request.interrupted_process_id.is_some_and(|pid| pid < 1)
            || request.interrupted_session_id.as_ref().is_some_and(|s| s.trim().is_empty())
            || request.interrupted_claim_token.as_ref().is_some_and(|s| s.trim().is_empty())
            || request.interrupted_at.as_ref().is_some_and(|at| parse_iso_millis(at).is_none())
            || !all_or_none(&[
                request.compact_session_id.is_none(),
                request.compact_anchor_entry_id.is_none(),
            ])
            || request.compact_session_id.as_ref().is_some_and(|s| {
                request.action != MaintenanceAction::Compact || s.trim().is_empty()
            })
            || request.compact_anchor_entry_id.as_ref().is_some_and(|s| {
                request.action != MaintenanceAction::Compact || s.trim().is_empty()
            })
            || request.completed_compaction_entry_id.as_ref().is_some_and(|s| {
                request.action != MaintenanceAction::Compact
                    || request.status != MaintenanceStatus::Completed
                    || s.trim().is_empty()
            })
            || (!allow_unbounded_diagnostics
                && request.error.as_ref().is_some_and(|e| e.chars().count() > 600))
            || !all_or_none(&[
                request.claimed_process_id.is_none(),
                request.claimed_session_id.is_none(),
                request.claim_token.is_none(),
            ])
            || !all_or_none(&[
                request.completed_process_id.is_none(),
                request.completed_session_id.is_none(),
                request.completion_claim_token.is_none(),
            ])
            || (request.completed_process_id.is_some()
                && request.status != MaintenanceStatus::Completed);
        if bad {
            return Err(invalid(format!("session maintenance request '{id}' is invalid")));
        }
        if request.status.is_terminal() && request.completed_at.is_none() {
            return Err(invalid(format!(
                "finished session maintenance request '{id}' has no completion timestamp"
            )));
        }
    }

    // TOMBSTONE: the company-action validation block — that every action in
    // the order map exists, that its targets are consistent, and that each
    // target's current request really belongs to it. The ledger no longer
    // carries actions, and the clause above rejects any request claiming a
    // `companyActionId` outright, so there is nothing left to cross-check.
    Ok(())
}

/// True when every flag agrees — the ported `new Set([...]).size <= 1` idiom
/// for "all present or all absent".
fn all_or_none(absent: &[bool]) -> bool {
    absent.iter().all(|value| *value == absent[0])
}

/// Row persistence for the session-maintenance store (org-data-normalization
/// P0, N5). Scaffold-agnostic: [`reconstruct`] rebuilds the in-memory
/// [`SessionMaintenanceLedger`] from the four `maintenance_*` tables, and
/// [`diff_into_rows`] writes ONLY the rows a publish changed and returns one
/// [`EventTouch`] per touched entity for `rows_txn::apply_and_emit`. The domain
/// logic (validate/reconcile/queue/…) is unchanged; these two functions are the
/// blob→row swap of `read`/`put`. Route wiring (which calls these through
/// `apply_and_emit` inside `CompanyDb::in_transaction`) lands with N2's
/// manifest-route reference; nothing here depends on it.
pub mod rows {
    use std::collections::{BTreeMap, BTreeSet};

    use rusqlite::{params, OptionalExtension, Transaction};

    use super::{
        MaintenanceAction, MaintenanceRequest, MaintenanceStatus, SessionMaintenanceLedger,
        SESSION_MAINTENANCE_SCHEMA_VERSION,
    };
    use crate::error::Refusal;
    use crate::store::rows_txn::{apply_and_emit, EventTouch};
    use crate::store::CompanyContext;
    use crate::ChiefdError;

    /// The native-ledger key this row seam owns. Callers outside this module
    /// use this seam rather than naming the key or `SessionMaintenanceStore`
    /// directly, preserving the store-containment fence after blob death.
    pub const SESSION_MAINTENANCE_STORE: &str = "session-maintenance";

    /// Item D (Fable): a normalized ledger carries NO unmodeled keys. A publish
    /// carrying any is refused 422, never silently dropped.
    pub const UNMODELED_KEYS: &str = "unmodeled-keys";

    /// A SQL failure reading/writing the maintenance rows is a store failure,
    /// not corruption. One greppable mapping point; the real error travels
    /// inside the value.
    fn store_failure(e: rusqlite::Error) -> ChiefdError {
        crate::error::store_failure("session-maintenance-rows", e)
    }

    /// Gives `ChiefdError` a `From<rusqlite::Error>` at the direct row-apply
    /// boundary without a blanket impl on `ChiefdError`. Unwrapped immediately
    /// by [`publish`]. Mirrors N2.
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

    const REQUEST_ENTITY: &str = "maintenance-request";
    const REQUEST_TABLE: &str = "maintenance_requests";

    /// Action labels a PREVIOUS build of this product wrote and this one cannot
    /// name. Enumerated, never inferred.
    ///
    /// THE DISTINCTION THIS LIST EXISTS TO DRAW: a row under one of these is
    /// HISTORY and is skipped; a row under any OTHER unrecognised label is
    /// CORRUPTION and fails the read closed. Collapsing the two -- which a
    /// blanket `action = 'compact'` filter does -- makes a genuinely undecodable
    /// row read as "no work", which is the fail-closed polarity
    /// `polarity_matrix.rs` exists to protect. Widen this list only for a label
    /// this product really did ship.
    const RETIRED_ACTIONS: &[&str] = &["fresh_session", "set_model"];

    /// What a stored `action` label means to THIS build.
    enum StoredAction {
        /// A live action.
        Live(MaintenanceAction),
        /// A label a previous build wrote; skip the row, keep it on disk.
        Retired,
    }

    fn stored_action(raw: &str) -> rusqlite::Result<StoredAction> {
        match raw {
            "compact" => Ok(StoredAction::Live(MaintenanceAction::Compact)),
            other if RETIRED_ACTIONS.contains(&other) => Ok(StoredAction::Retired),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }

    fn action_from_str(raw: &str) -> rusqlite::Result<MaintenanceAction> {
        match stored_action(raw)? {
            StoredAction::Live(action) => Ok(action),
            // Unreachable from `reconstruct`, which filters retired rows out
            // before building one. Kept exhaustive rather than `unreachable!`:
            // a second caller must get a refusal, not a panic.
            StoredAction::Retired => Err(rusqlite::Error::InvalidQuery),
        }
    }

    fn status_as_str(status: MaintenanceStatus) -> &'static str {
        match status {
            MaintenanceStatus::Queued => "queued",
            MaintenanceStatus::Running => "running",
            MaintenanceStatus::Applying => "applying",
            MaintenanceStatus::Completed => "completed",
            MaintenanceStatus::Failed => "failed",
            MaintenanceStatus::Skipped => "skipped",
        }
    }

    fn status_from_str(raw: &str) -> rusqlite::Result<MaintenanceStatus> {
        match raw {
            "queued" => Ok(MaintenanceStatus::Queued),
            "running" => Ok(MaintenanceStatus::Running),
            "applying" => Ok(MaintenanceStatus::Applying),
            "completed" => Ok(MaintenanceStatus::Completed),
            "failed" => Ok(MaintenanceStatus::Failed),
            "skipped" => Ok(MaintenanceStatus::Skipped),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }

    const REQUEST_COLUMNS: &str =
        "id, person_id, requested_by, action, status, reason, automatic, \
        attempt, recovered_from_request_id, retry_not_before, force, \
        company_action_id, claimed_process_id, claimed_session_id, claim_token, \
        completed_process_id, completed_session_id, completion_claim_token, \
        interrupted_process_id, interrupted_session_id, interrupted_claim_token, interrupted_at, \
        compact_session_id, compact_anchor_entry_id, completed_compaction_entry_id, \
        requested_at, started_at, \
        settled_at, error";

    fn request_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MaintenanceRequest> {
        Ok(MaintenanceRequest {
            id: row.get("id")?,
            person_id: row.get("person_id")?,
            requested_by: row.get("requested_by")?,
            action: action_from_str(&row.get::<_, String>("action")?)?,
            status: status_from_str(&row.get::<_, String>("status")?)?,
            reason: row.get("reason")?,
            automatic: row.get::<_, i64>("automatic")? != 0,
            attempt: row.get::<_, Option<i64>>("attempt")?.map(|a| a as u32),
            recovered_from_request_id: row.get("recovered_from_request_id")?,
            retry_not_before: row.get("retry_not_before")?,
            force: row.get::<_, Option<i64>>("force")?.map(|f| f != 0),
            company_action_id: row.get("company_action_id")?,
            claimed_process_id: row.get("claimed_process_id")?,
            claimed_session_id: row.get("claimed_session_id")?,
            claim_token: row.get("claim_token")?,
            completed_process_id: row.get("completed_process_id")?,
            completed_session_id: row.get("completed_session_id")?,
            completion_claim_token: row.get("completion_claim_token")?,
            interrupted_process_id: row.get("interrupted_process_id")?,
            interrupted_session_id: row.get("interrupted_session_id")?,
            interrupted_claim_token: row.get("interrupted_claim_token")?,
            interrupted_at: row.get("interrupted_at")?,
            compact_session_id: row.get("compact_session_id")?,
            compact_anchor_entry_id: row.get("compact_anchor_entry_id")?,
            completed_compaction_entry_id: row.get("completed_compaction_entry_id")?,
            requested_at: row.get("requested_at")?,
            started_at: row.get("started_at")?,
            completed_at: row.get("settled_at")?,
            error: row.get("error")?,
            extra: Default::default(),
        })
    }

    /// Rebuild the ledger from rows, or `None` when the company has no
    /// `maintenance_ledger` row (never needed maintenance — an absent ledger,
    /// not corruption). `requestIds[]` per target is DERIVED from the request
    /// rows (Fable ruling: no child table), preserving `ordinal` order.
    ///
    /// # Errors
    /// Any `rusqlite` failure, or an unrecognized action/status label in a row.
    pub fn reconstruct(
        tx: &Transaction<'_>,
        slug: &str,
    ) -> rusqlite::Result<Option<SessionMaintenanceLedger>> {
        let ledger_row = tx
            .query_row(
                "SELECT created_at, updated_at FROM maintenance_ledger WHERE slug = ?1",
                params![slug],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((created_at, updated_at)) = ledger_row else {
            return Ok(None);
        };

        let mut request_order = Vec::new();
        let mut requests = BTreeMap::new();
        {
            // RETIRED IS SKIPPED; UNRECOGNISED FAILS THE READ CLOSED. The SQL
            // deliberately does NOT filter, and this is the correction to my
            // own first attempt at it.
            //
            // `MaintenanceAction` narrowed to one variant when
            // `org_maintain_session` was deleted, so an unfiltered read fed
            // every historical row through a parser that now refuses it: ONE
            // legacy `fresh_session` or `set_model` row failed the whole
            // reconstruct, taking the session-maintenance surface AND the
            // surviving automatic compaction with it. Measured on the
            // operator's own company, 2026-08-25: 16 `compact` rows against 105
            // legacy ones. A fresh database has none, which is why every test
            // and every CI run was green over it.
            //
            // The obvious repair -- `WHERE action = 'compact'` -- traded that
            // loud failure for a silent one. It makes EVERY unrecognised label
            // invisible, so a genuinely corrupt row reads as "no work" instead
            // of refusing, which is the exact fail-closed polarity
            // `polarity_matrix.rs` pins (it corrupts a row to `'defragment'`
            // and requires an error). History and corruption are not the same
            // fact and must not share a branch.
            //
            // So every row is read, and `stored_action` decides per row:
            // `compact` is live, a label in `RETIRED_ACTIONS` is history and is
            // skipped, and anything else is an error. Retired rows are LEFT ON
            // DISK -- they are terminal, nothing live refers to them, and
            // deleting somebody's history to make a read parse is not a repair.
            // See `publish` for the half that makes leaving them safe.
            let sql = format!(
                "SELECT {REQUEST_COLUMNS} FROM maintenance_requests WHERE slug = ?1 ORDER BY ordinal"
            );
            let mut stmt = tx.prepare(&sql)?;
            let mut rows = stmt.query(params![slug])?;
            while let Some(row) = rows.next()? {
                // TOMBSTONE: the `maintenance_request_models` join, which
                // hydrated a `set_model` request's provider and model. Both the
                // action and that table are deleted.
                //
                // The label is read BEFORE the row is projected, so a retired
                // row costs nothing and an unrecognised one refuses here rather
                // than deeper in.
                if matches!(stored_action(&row.get::<_, String>("action")?)?, StoredAction::Retired)
                {
                    continue;
                }
                let request = request_from_row(row)?;
                request_order.push(request.id.clone());
                requests.insert(request.id.clone(), request);
            }
        }

        // TOMBSTONE: the two reconstruct blocks that hydrated company actions
        // and their targets out of `maintenance_company_actions` and
        // `maintenance_company_action_targets`. The targets table is dropped;
        // the actions table is kept EMPTY so the surviving
        // `maintenance_requests` foreign key still resolves, and nothing writes
        // it, so there is nothing to read back.

        Ok(Some(SessionMaintenanceLedger {
            schema_version: SESSION_MAINTENANCE_SCHEMA_VERSION,
            organization: slug.to_string(),
            request_order,
            requests,
            created_at,
            updated_at,
            extra: Default::default(),
        }))
    }

    /// The ordinal this request already occupies, if the row exists.
    ///
    /// Deliberately unfiltered by `action`: the point is to find ANY row with
    /// this id, including one a previous version wrote under a label that no
    /// longer parses, so that re-publishing it cannot land on top of a
    /// different row.
    fn stored_ordinal(tx: &Transaction<'_>, slug: &str, id: &str) -> rusqlite::Result<Option<i64>> {
        tx.query_row(
            "SELECT ordinal FROM maintenance_requests WHERE slug = ?1 AND id = ?2",
            params![slug, id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
    }

    /// One past the highest ordinal this company has ever used.
    ///
    /// Counts LEGACY rows too, for the same reason `stored_ordinal` does not
    /// filter: an ordinal the filtered read cannot see is still taken.
    fn next_ordinal(tx: &Transaction<'_>, slug: &str) -> rusqlite::Result<i64> {
        tx.query_row(
            "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM maintenance_requests WHERE slug = ?1",
            params![slug],
            |r| r.get::<_, i64>(0),
        )
    }

    fn upsert_request(
        tx: &Transaction<'_>,
        slug: &str,
        ordinal: i64,
        request: &MaintenanceRequest,
    ) -> rusqlite::Result<()> {
        tx.execute(
            "INSERT OR REPLACE INTO maintenance_requests(\
                 slug, id, ordinal, person_id, requested_by, action, status, reason, automatic, \
                 attempt, recovered_from_request_id, retry_not_before, force, \
                 company_action_id, claimed_process_id, claimed_session_id, claim_token, \
                 completed_process_id, completed_session_id, completion_claim_token, \
                 interrupted_process_id, interrupted_session_id, interrupted_claim_token, \
                 interrupted_at, compact_session_id, compact_anchor_entry_id, \
                 completed_compaction_entry_id, \
                 requested_at, started_at, settled_at, error) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,\
                 ?23,?24,?25,?26,?27,?28,?29,?30,?31)",
            params![
                slug,
                request.id,
                ordinal,
                request.person_id,
                request.requested_by,
                request.action.as_str(),
                status_as_str(request.status),
                request.reason,
                i64::from(request.automatic),
                request.attempt.map(i64::from),
                request.recovered_from_request_id,
                request.retry_not_before,
                request.force.map(i64::from),
                request.company_action_id,
                request.claimed_process_id,
                request.claimed_session_id,
                request.claim_token,
                request.completed_process_id,
                request.completed_session_id,
                request.completion_claim_token,
                request.interrupted_process_id,
                request.interrupted_session_id,
                request.interrupted_claim_token,
                request.interrupted_at,
                request.compact_session_id,
                request.compact_anchor_entry_id,
                request.completed_compaction_entry_id,
                request.requested_at,
                request.started_at,
                request.completed_at,
                request.error,
            ],
        )?;
        // TOMBSTONE: the `maintenance_request_models` upsert/delete pair. The
        // table is dropped and no request carries a model any more.
        Ok(())
    }

    // TOMBSTONE: `order_index`, which built a position map so `publish` could
    // notice that a request had MOVED in `request_order`. Ordinals now come
    // from the table and an existing row's cannot move, so there is no
    // position change left to detect.

    /// Diff `next` against the currently-stored ledger, writing ONLY the changed
    /// rows and returning one [`EventTouch`] per touched entity. An empty result
    /// means a no-op publish that writes no audit event. The
    /// `maintenance_ledger` metadata row is refreshed only when something
    /// changed (never on a no-op).
    ///
    /// # Errors
    /// Any `rusqlite` failure from the row writes.
    pub fn diff_into_rows(
        tx: &Transaction<'_>,
        slug: &str,
        current: Option<&SessionMaintenanceLedger>,
        next: &SessionMaintenanceLedger,
    ) -> rusqlite::Result<Vec<EventTouch>> {
        let mut touches = Vec::new();

        // TOMBSTONE: the company-action and target upserts. They ran FIRST so
        // `maintenance_requests.company_action_id` had a row to point at. The
        // ledger no longer carries actions and nothing can mint one, so the
        // ordering constraint they existed to satisfy has no second side.

        // --- requests ------------------------------------------------------
        // ORDINALS COME FROM THE TABLE, NEVER FROM THE POSITION IN THIS LIST.
        //
        // This loop used to write `ordinal` as its own `enumerate()` index,
        // which was correct only while `reconstruct` returned EVERY row: the
        // list position and the stored ordinal were then the same number by
        // construction. `reconstruct` now filters legacy actions out (see
        // there), so the two have come apart — and writing the list position
        // would renumber the survivors down into ordinals the legacy rows still
        // hold.
        //
        // That is not a constraint error. `upsert_request` uses INSERT OR
        // REPLACE, and there is a UNIQUE index on `(slug, ordinal)`, so SQLite
        // resolves the conflict by DELETING the row in the way. Measured:
        //
        //   before  a|0|fresh_session  b|1|compact  c|2|set_model  d|3|compact
        //   publish [b,d] at list positions 0,1
        //   after   b|0|compact        d|1|compact  c|2|set_model
        //
        // `a` is gone, with no error, no log and no touch event. On the
        // operator's box that is the low ordinals of 105 legacy rows being
        // overwritten a few at a time, for ever, while everything downstream
        // looks healthy. A loud failure traded for silent data loss.
        //
        // So an existing request keeps the ordinal it already has, and a new
        // one is appended above the table's current maximum.
        //
        // THE REASON THIS IS RIGHT IS NOT "so the filter is safe". Position-in-
        // working-set was never what this column meant: `schema.rs` has always
        // called it "append-chronological", and the two only agreed while the
        // read returned every row. This makes the column mean what it says, and
        // collisions impossible by construction rather than by the read and the
        // write happening to agree.
        //
        // `next_ordinal` is resolved PER INSERT, inside the transaction, and
        // that is load-bearing: two genuinely new requests in one diff must not
        // both compute the same maximum. Hoisting it out of the loop would look
        // like an optimisation and would reintroduce exactly the deletion
        // above, one publish later. `two_new_requests_in_one_diff_take_distinct_ordinals`
        // is the pin.
        //
        // One semantic consequence, for whoever adds a reorder feature: a
        // `request_order` that REORDERED in memory would no longer persist that
        // order, because the stored ordinal wins. Nothing reorders today -- the
        // queue is append-only -- so this owes a design rather than a fix.
        for id in &next.request_order {
            let Some(request) = next.requests.get(id) else { continue };
            // An existing row's ordinal cannot move any more, so the position
            // half of the old `unchanged` test has nothing left to compare.
            if current.and_then(|c| c.requests.get(id)) == Some(request) {
                continue;
            }
            let ordinal = match stored_ordinal(tx, slug, id)? {
                Some(existing) => existing,
                None => next_ordinal(tx, slug)?,
            };
            upsert_request(tx, slug, ordinal, request)?;
            touches.push(EventTouch::new(
                REQUEST_ENTITY,
                id.clone(),
                "upsert",
                REQUEST_TABLE,
                slug,
            ));
        }
        if let Some(cur) = current {
            let next_ids: BTreeSet<&str> = next.request_order.iter().map(String::as_str).collect();
            for id in &cur.request_order {
                if !next_ids.contains(id.as_str()) {
                    tx.execute(
                        "DELETE FROM maintenance_requests WHERE slug = ?1 AND id = ?2",
                        params![slug, id],
                    )?;
                    touches.push(EventTouch::new(
                        REQUEST_ENTITY,
                        id.clone(),
                        "delete",
                        REQUEST_TABLE,
                        slug,
                    ));
                }
            }
        }

        // TOMBSTONE: the company-action and target DELETE half, which removed
        // an action's targets before the action itself so the foreign key held.

        // --- ledger metadata row (only when something changed) --------------
        if !touches.is_empty() || current.is_none() {
            tx.execute(
                "INSERT OR REPLACE INTO maintenance_ledger(slug, created_at, updated_at) \
                 VALUES(?1, ?2, ?3)",
                params![slug, next.created_at, next.updated_at],
            )?;
        }
        Ok(touches)
    }

    /// Reject any unmodeled key on the ledger or a request (item D) — a
    /// normalized ledger carries none. NEVER silently drops.
    fn reject_unmodeled_keys(ledger: &SessionMaintenanceLedger) -> Result<(), ChiefdError> {
        let mut paths = Vec::new();
        for key in ledger.extra.keys() {
            paths.push(format!("extra.{key}"));
        }
        for (id, request) in &ledger.requests {
            for key in request.extra.keys() {
                paths.push(format!("requests.{id}.extra.{key}"));
            }
        }
        if paths.is_empty() {
            return Ok(());
        }
        paths.sort();
        Err(ChiefdError::from(Refusal::new(
            UNMODELED_KEYS,
            format!(
                "session maintenance ledger carries unmodeled keys the row model cannot store: {}",
                paths.join(", ")
            ),
        )))
    }

    /// Publish a whole ledger into the rows as a direct current-state mutation.
    /// The 1:1 template is `organization_rows::publish`: reject unmodeled keys
    /// (item D) → validate → diff the incoming ledger against current rows
    /// inside one `BEGIN IMMEDIATE` transaction. The event sequence is an
    /// immutable audit cursor, not a caller-supplied write precondition.
    ///
    /// # Errors
    /// [`UNMODELED_KEYS`] / `session-maintenance-ledger-invalid` refusals (map to
    /// 422); SQL failures as [`ChiefdError::StoreFailure`].
    pub fn publish(
        tx: &Transaction<'_>,
        slug: &str,
        incoming: &SessionMaintenanceLedger,
    ) -> Result<i64, ChiefdError> {
        reject_unmodeled_keys(incoming)?;
        // INTERNAL invariants only (row-level rules: status/action enums, claim
        // triples, thinking/model group, company-fanout shape, timestamps). NO
        // manifest/people-membership check here: pre-N9 the manifest ROWS are
        // empty (the blob is still manifest authority until the N9 cutover), so a
        // people-membership check would reject every legitimate request. The
        // authoritative people reconcile (`reconcileSessionMaintenancePeople`)
        // stays TS-side, where `loadOrganization` has the real manifest.
        // `allow_orphaned_open = true` is exactly "skip the people-membership
        // terms, enforce everything else". The organization-slug check compares
        // ledger.organization against the ctx slug, and the launcher publishes
        // the BARE slug while the row-publish `slug` param is the COMPOSITE wire
        // key (CompanyDb.label = slug@rootHash) -- so the ctx must carry the BARE
        // slug, symmetric to supervision's validate comparing against the bare
        // manifest.slug (the recurring composite->bare class-of-bug). Pre-N9 the
        // manifest rows are empty, so the bare slug is taken from the ledger's own
        // organization (the field the check validates), making the org-slug check
        // a structural self-consistency guard here while the authoritative
        // company-membership reconcile stays TS-side with the real manifest.
        let ctx = CompanyContext::new(&incoming.organization, "", std::iter::empty::<String>());
        super::validate(incoming, &ctx, true, false).map_err(ChiefdError::from)?;
        let current = reconstruct(tx, slug).map_err(store_failure)?;
        apply_and_emit::<RowsSqlError, _>(tx, slug, &incoming.updated_at, "", |tx| {
            Ok(diff_into_rows(tx, slug, current.as_ref(), incoming)?)
        })
        .map_err(|RowsSqlError(e)| e)
    }

    /// Backfill a legacy in-memory document body through the same typed,
    /// direct row publish path used by the public API. `writer::persist` owns
    /// the transaction and publishes directly against current rows.
    pub fn backfill_session_maintenance(
        tx: &Transaction<'_>,
        slug: &str,
        blob: &[u8],
    ) -> Result<i64, ChiefdError> {
        let ledger: SessionMaintenanceLedger = serde_json::from_slice(blob)
            .map_err(|e| crate::error::corrupt_store("session-maintenance-blob", e))?;
        publish(tx, slug, &ledger)
    }

    /// Delete the complete aggregate when its store is removed.  This is a
    /// real replacement clear: child rows first, then their action parents and
    /// ledger root; no compatibility document is retained.
    pub fn clear(tx: &Transaction<'_>, slug: &str, at: &str) -> Result<(), ChiefdError> {
        let exists =
            tx.query_row("SELECT 1 FROM maintenance_ledger WHERE slug = ?1", params![slug], |_| {
                Ok(())
            })
            .optional()
            .map_err(store_failure)?
            .is_some();
        apply_and_emit::<RowsSqlError, _>(tx, slug, at, "", |tx| {
            if !exists {
                return Ok(Vec::new());
            }
            tx.execute("DELETE FROM maintenance_requests WHERE slug = ?1", params![slug])?;
            // `maintenance_company_action_targets` was cleared here too and is
            // now DROPPED, so naming it fails the whole clear with
            // `no such table`. The company-actions PARENT survives — empty, for
            // the foreign key — and is still cleared, because a stale row there
            // would outlive the company it belonged to.
            tx.execute("DELETE FROM maintenance_company_actions WHERE slug = ?1", params![slug])?;
            tx.execute("DELETE FROM maintenance_ledger WHERE slug = ?1", params![slug])?;
            Ok(vec![EventTouch::new(
                "session-maintenance",
                slug,
                "delete",
                "maintenance_ledger",
                slug,
            )])
        })
        .map(|_| ())
        .map_err(|RowsSqlError(e)| e)
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn the_retry_ladder_doubles_and_then_caps() {
        assert_eq!(session_maintenance_retry_delay_ms(2), Ok(250));
        assert_eq!(session_maintenance_retry_delay_ms(3), Ok(500));
        assert_eq!(session_maintenance_retry_delay_ms(4), Ok(1_000));
        assert_eq!(session_maintenance_retry_delay_ms(9), Ok(30_000));
        assert_eq!(session_maintenance_retry_delay_ms(1_000), Ok(30_000));
        assert_eq!(session_maintenance_retry_delay_ms(u32::MAX), Ok(30_000));
    }

    #[test]
    fn the_first_attempt_has_no_retry_delay() {
        for attempt in [0_u32, 1] {
            let refusal = session_maintenance_retry_delay_ms(attempt).expect_err("not a retry");
            assert_eq!(refusal.code, SESSION_MAINTENANCE_RETRY_ATTEMPT_INVALID);
            assert_eq!(
                refusal.message,
                "Session maintenance retry attempt must be a safe integer greater than one"
            );
        }
    }
}

#[cfg(test)]
mod row_tests {
    use super::rows::reconstruct;
    use super::*;
    use rusqlite::{params, Connection, Transaction};

    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch("PRAGMA foreign_keys=ON;").expect("pragma");
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("schema");
        conn
    }

    // TOMBSTONE: the `request(seq, action, extra)` fixture builder. Its only
    // callers were the company-action and model-payload round-trip tests,
    // both deleted with the family.

    // TOMBSTONE: `launch_claims_only_the_exact_unclaimed_queued_fresh_session_request`
    // and `launch_completes_only_the_exact_applying_fresh_session_request`.
    //
    // Both pinned the actuator-owned fresh launch: that it claims exactly the
    // one queued fresh request for that person and forges no Pi claim, and that
    // it credits exactly the applying one and treats a replay as idempotent.
    // The ledger methods under them are deleted with `fresh_session`.

    #[test]
    fn an_unmodeled_key_is_captured_into_extra_not_dropped() {
        // Item D: the flatten catch-all captures any key the row model does not
        // model, so publish can refuse it 422 instead of silently dropping it.
        let ledger: SessionMaintenanceLedger = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "organization": "acme",
            "requestOrder": ["session-maintenance:1:ada:compact"],
            "requests": {
                "session-maintenance:1:ada:compact": {
                    "id": "session-maintenance:1:ada:compact",
                    "action": "compact",
                    "personId": "ada",
                    "requestedBy": "ada",
                    "reason": "test",
                    "automatic": false,
                    "status": "queued",
                    "requestedAt": "2026-07-25T00:00:00.000Z",
                    "attempt": 1,
                    "mysteryRequestKey": "x"
                }
            },
            "createdAt": "2026-07-25T00:00:00.000Z",
            "updatedAt": "2026-07-25T00:00:00.000Z",
            "mysteryTopKey": 42
        }))
        .expect("a ledger with unmodeled keys still parses (into extra)");
        assert!(ledger.extra.contains_key("mysteryTopKey"), "top-level unmodeled key is captured");
        assert!(
            ledger.requests["session-maintenance:1:ada:compact"]
                .extra
                .contains_key("mysteryRequestKey"),
            "a request's unmodeled key is captured"
        );
    }

    #[test]
    fn reconstruct_of_a_company_that_never_needed_maintenance_is_none() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(reconstruct(&tx, "acme").unwrap(), None);
        tx.commit().unwrap();
    }

    /// Insert a row directly, the way a PREVIOUS version of this product did,
    /// under a label `action_from_str` no longer accepts. Bypasses `publish`
    /// deliberately: the point is a row that exists and that today's writer
    /// could never produce.
    fn insert_legacy_row(tx: &Transaction<'_>, slug: &str, id: &str, ordinal: i64, action: &str) {
        tx.execute(
            "INSERT INTO maintenance_requests(\
                 slug, id, ordinal, person_id, requested_by, action, status, reason, automatic, \
                 requested_at) \
             VALUES(?1,?2,?3,'ada','ada',?4,'completed','legacy',0,'2026-01-01T00:00:00.000Z')",
            params![slug, id, ordinal, action],
        )
        .expect("legacy row inserts");
    }

    /// A ledger carrying one queued compact request for `ada`.
    fn one_compact_ledger(id: &str) -> SessionMaintenanceLedger {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "organization": "acme",
            "requestOrder": [id],
            "requests": {
                id: {
                    "id": id,
                    "action": "compact",
                    "personId": "ada",
                    "requestedBy": "ada",
                    "reason": "test",
                    "automatic": true,
                    "status": "queued",
                    "requestedAt": "2026-08-25T00:00:00.000Z",
                    "attempt": 1
                }
            },
            "createdAt": "2026-08-25T00:00:00.000Z",
            "updatedAt": "2026-08-25T00:00:00.000Z"
        }))
        .expect("ledger parses")
    }

    /// THE BLOCKER, DIRECTION ONE. Every company with history carries rows whose
    /// `action` this build cannot parse, and an unfiltered reconstruct fed the
    /// first of them to `action_from_str` and failed the whole read -- taking
    /// the surviving automatic compaction down with the surface. Measured on the
    /// operator's own company: 16 `compact` rows against 105 legacy ones. A
    /// fresh database has none, which is why every earlier green run agreed.
    #[test]
    fn reconstruct_ignores_legacy_action_rows_instead_of_failing_the_whole_read() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let live = "session-maintenance:9:ada:compact";
        super::rows::publish(&tx, "acme", &one_compact_ledger(live)).expect("publish");
        insert_legacy_row(&tx, "acme", "legacy-fresh", 40, "fresh_session");
        insert_legacy_row(&tx, "acme", "legacy-model", 41, "set_model");

        let ledger = reconstruct(&tx, "acme")
            .expect("a legacy row must not fail the read")
            .expect("the company has a ledger");

        assert_eq!(ledger.request_order, vec![live.to_owned()], "only parseable rows are surfaced");
        assert!(!ledger.requests.contains_key("legacy-fresh"));
        assert!(!ledger.requests.contains_key("legacy-model"));
        tx.commit().unwrap();
    }

    /// HISTORY AND CORRUPTION ARE NOT THE SAME FACT, and my first fix for the
    /// blocker above erased the difference between them.
    ///
    /// `WHERE action = 'compact'` made every unrecognised label invisible, so a
    /// genuinely corrupt row read as "no work" rather than refusing -- trading
    /// the loud failure for a silent one, which is the same mistake in the
    /// opposite direction. `polarity_matrix.rs` caught it by corrupting a row to
    /// `'defragment'` and requiring an error; this pins the rule at the store so
    /// the next narrowing meets it here first.
    #[test]
    fn an_unrecognised_action_still_fails_the_read_closed() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let live = "session-maintenance:9:ada:compact";
        super::rows::publish(&tx, "acme", &one_compact_ledger(live)).expect("publish");
        // Not in `RETIRED_ACTIONS`: this product never shipped it, so the only
        // way a row carries it is corruption.
        insert_legacy_row(&tx, "acme", "corrupt", 40, "defragment");

        reconstruct(&tx, "acme").expect_err("an undecodable row must never read as 'no work'");
        tx.commit().unwrap();
    }

    /// THE BLOCKER, DIRECTION TWO -- and the reason the one-line read filter is
    /// not the whole fix. `publish` used to write `ordinal` as its own list
    /// position, which was only ever correct because the read returned EVERY
    /// row. Filter the read and the survivors renumber down onto ordinals the
    /// legacy rows still hold; `upsert_request` is INSERT OR REPLACE against a
    /// UNIQUE `(slug, ordinal)` index, so SQLite resolves that by DELETING the
    /// legacy row. No error, no log, no touch event -- a loud failure traded for
    /// silent data loss. Ordinals now come from the table, so this cannot happen.
    #[test]
    fn publishing_over_legacy_rows_at_low_ordinals_destroys_none_of_them() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        // The legacy rows own the LOW ordinals, which is what a real company
        // looks like: the deleted actions are the old ones.
        insert_legacy_row(&tx, "acme", "legacy-a", 0, "fresh_session");
        insert_legacy_row(&tx, "acme", "legacy-b", 1, "set_model");
        insert_legacy_row(&tx, "acme", "legacy-c", 2, "set_model");

        let live = "session-maintenance:9:ada:compact";
        super::rows::publish(&tx, "acme", &one_compact_ledger(live)).expect("publish");

        let surviving: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM maintenance_requests WHERE slug = 'acme' AND action <> 'compact'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(surviving, 3, "every legacy row survives a publish");

        let ordinals: Vec<i64> = {
            let mut stmt = tx
                .prepare(
                    "SELECT ordinal FROM maintenance_requests WHERE slug = 'acme' \
                     AND action <> 'compact' ORDER BY ordinal",
                )
                .expect("prepare");
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).expect("query");
            rows.collect::<rusqlite::Result<Vec<_>>>().expect("collect")
        };
        assert_eq!(ordinals, vec![0, 1, 2], "and keeps the ordinal it was written with");

        let live_ordinal: i64 = tx
            .query_row(
                "SELECT ordinal FROM maintenance_requests WHERE slug = 'acme' AND id = ?1",
                params![live],
                |r| r.get(0),
            )
            .expect("the new request is stored");
        assert_eq!(live_ordinal, 3, "a new request appends above the table's maximum");
        tx.commit().unwrap();
    }

    /// TWO new requests in ONE diff. The guarantee is structural today --
    /// `next_ordinal` is a SQL subquery resolved per insert, so the second call
    /// sees the first insert inside the same transaction -- and this test exists
    /// because that structure is exactly what a future "hoist the subquery out
    /// of the loop" refactor deletes. Such a refactor looks like an
    /// optimisation, passes every other test here, and reintroduces the
    /// INSERT-OR-REPLACE deletion one publish later: both new rows would take
    /// `MAX+1` and the second would eat the first. Contract, not construction.
    #[test]
    fn two_new_requests_in_one_diff_take_distinct_ordinals() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        insert_legacy_row(&tx, "acme", "legacy-a", 0, "fresh_session");
        insert_legacy_row(&tx, "acme", "legacy-b", 1, "set_model");

        let first = "session-maintenance:9:ada:compact";
        let second = "session-maintenance:10:grace:compact";
        let mut ledger = one_compact_ledger(first);
        let mut extra = ledger.requests[first].clone();
        extra.id = second.to_owned();
        extra.person_id = "grace".to_owned();
        ledger.request_order.push(second.to_owned());
        ledger.requests.insert(second.to_owned(), extra);

        super::rows::publish(&tx, "acme", &ledger).expect("publish two new requests");

        let ordinal_of = |id: &str| -> i64 {
            tx.query_row(
                "SELECT ordinal FROM maintenance_requests WHERE slug = 'acme' AND id = ?1",
                params![id],
                |r| r.get(0),
            )
            .expect("row exists")
        };
        let (a, b) = (ordinal_of(first), ordinal_of(second));
        assert_ne!(a, b, "two new requests must not collide on one ordinal");
        assert_eq!(
            {
                let mut v = vec![a, b];
                v.sort_unstable();
                v
            },
            vec![2, 3],
            "both append above the legacy maximum"
        );

        let total: i64 = tx
            .query_row("SELECT COUNT(*) FROM maintenance_requests WHERE slug = 'acme'", [], |r| {
                r.get(0)
            })
            .expect("count");
        assert_eq!(total, 4, "two legacy rows and two new ones -- nothing was replaced");
        tx.commit().unwrap();
    }

    /// An ordinal is allocated ONCE. Re-publishing a changed request must update
    /// it in place rather than append it a second time, or every status
    /// transition would walk the ordinal space upward for ever.
    #[test]
    fn republishing_a_changed_request_keeps_its_original_ordinal() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        insert_legacy_row(&tx, "acme", "legacy-a", 0, "fresh_session");
        let live = "session-maintenance:9:ada:compact";
        super::rows::publish(&tx, "acme", &one_compact_ledger(live)).expect("first publish");

        let mut advanced = one_compact_ledger(live);
        advanced.requests.get_mut(live).expect("request").status = MaintenanceStatus::Running;
        super::rows::publish(&tx, "acme", &advanced).expect("second publish");

        let (ordinal, status): (i64, String) = tx
            .query_row(
                "SELECT ordinal, status FROM maintenance_requests WHERE slug = 'acme' AND id = ?1",
                params![live],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("one row");
        assert_eq!(ordinal, 1, "the ordinal it was first given");
        assert_eq!(status, "running", "and the update did land");
        tx.commit().unwrap();
    }

    /// #319 single-target force: a request may carry `force` with NO
    /// companyActionId (TS allows it). validate must NOT 422 on that — the
    /// pre-#319 clause rejecting force-without-companyActionId is dropped. The
    /// company-action clause still requires force+human+existing action when a
    /// companyActionId IS present (unchanged).
    #[test]
    fn publish_accepts_a_single_target_force_without_a_company_action_id() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let ledger: SessionMaintenanceLedger = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "organization": "acme",
            "requestOrder": ["session-maintenance:1:ada:compact"],
            "requests": {
                "session-maintenance:1:ada:compact": {
                    "id": "session-maintenance:1:ada:compact",
                    "action": "compact",
                    "personId": "ada",
                    "requestedBy": "ada",
                    "reason": "operator forced a single-target refresh",
                    "automatic": false,
                    "status": "queued",
                    "requestedAt": "2026-07-25T00:00:00.000Z",
                    "attempt": 1,
                    "force": true
                }
            },
            "createdAt": "2026-07-25T00:00:00.000Z",
            "updatedAt": "2026-07-25T00:00:00.000Z"
        }))
        .expect("ledger parses");
        let outcome = super::rows::publish(&tx, "acme", &ledger)
            .expect("single-target force (no companyActionId) must not 422");
        assert!(outcome > 0, "got audit cursor {outcome}");
        tx.commit().unwrap();
    }

    /// Composite->bare slug regression: the launcher publishes a ledger whose
    /// `organization` is the BARE slug, while the row-publish `slug` param is the
    /// COMPOSITE wire key (CompanyDb.label = slug@rootHash). The org-slug validate
    /// must NOT 422 `session-maintenance-ledger-invalid` on that mismatch
    /// (symmetric to supervision comparing against the bare manifest.slug).
    #[test]
    fn publish_accepts_a_bare_organization_under_a_composite_wire_slug() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let ledger = SessionMaintenanceLedger::initial("cobalt", "2026-07-25T00:00:00.000Z");
        let outcome = super::rows::publish(&tx, "cobalt@roothash123", &ledger)
            .expect("a bare organization under a composite wire slug must not 422");
        assert_eq!(outcome, 0, "the valid initial ledger has no maintenance rows to write");
        tx.commit().unwrap();
    }

    // TOMBSTONE: `round_trips_model_payload_with_the_other_action_types`. It
    // pinned that a `set_model` request's provider and model survive a round
    // trip while the other actions carry none. One action remains and it
    // carries no model.

    // TOMBSTONE: `round_trips_a_company_action_with_derived_request_ids`. It
    // pinned that a company action round-trips with its target request ids
    // DERIVED rather than stored in a child table. The family is deleted.
}
