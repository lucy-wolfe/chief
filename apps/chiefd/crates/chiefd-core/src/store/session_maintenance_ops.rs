//! The session-maintenance verbs: queue, claim, defer, interrupt, recover,
//! finish.
//!
//! A Pi asks for its own context to be compacted, its session to be reset, or
//! its thinking level or model to change. Each request is a durable, immutable
//! record with a status machine, claimed by exactly one live Pi through a
//! process/session/token triple.
//!
//! Every verb here is a pure function of `(ledger, input, at)` run by the
//! writer thread inside the transaction that publishes it. There is no CAS, no
//! retry and no sleep — the writer queue IS the serialization, and the claim
//! triple is the only guard a request needs.
//!
//! Identity is checked by [`ExpectedIdentity`]: an operation that names a
//! request must present the

use crate::error::Refusal;
use crate::isotime::{iso_millis, parse_iso_millis};
use crate::store::control_authority::{person_is_in_scope, ControlActor};
use crate::store::organization::{EmploymentState, OrganizationManifest};
use crate::store::session_maintenance::{
    session_maintenance_retry_delay_ms, MaintenanceAction, MaintenanceRequest, MaintenanceStatus,
    SessionMaintenanceLedger, SESSION_MAINTENANCE_MAX_ATTEMPTS,
    SESSION_MAINTENANCE_PROCESS_INTERRUPTION_ERROR, SESSION_MAINTENANCE_TARGET_PARKED_ERROR,
};
use crate::ChiefdError;

/// The requester identity an operator-issued request carries.
pub const SESSION_MAINTENANCE_OPERATOR_REQUESTER: &str = "operator";

/// The caller's input did not describe a legal maintenance request.
pub const INVALID_MAINTENANCE: &str = "invalid-session-maintenance";

/// The named request does not exist.
pub const UNKNOWN_MAINTENANCE_REQUEST: &str = "unknown-session-maintenance-request";

/// The caller is not the identity the request was minted for.
pub const MAINTENANCE_IDENTITY_MISMATCH: &str = "session-maintenance-identity-mismatch";

/// The request is not owned by the exact live claim the caller presented.
pub const MAINTENANCE_CLAIM_MISMATCH: &str = "session-maintenance-claim-mismatch";

/// The request is not in a status this verb can act on.
pub const MAINTENANCE_STATUS_CONFLICT: &str = "session-maintenance-status-conflict";

fn refuse(code: &'static str, detail: impl Into<String>) -> ChiefdError {
    ChiefdError::from(Refusal::new(code, detail))
}

/// The exact live Pi that owns a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The claiming OS process.
    pub process_id: i64,
    /// The claiming native Pi session.
    pub session_id: String,
    /// A crash-unique token minted per claim.
    pub claim_token: String,
}

impl Claim {
    /// Validate the triple's shape.
    ///
    /// # Errors
    /// [`INVALID_MAINTENANCE`] when the pid is not positive or a field is blank.
    pub fn validated(&self) -> Result<(), ChiefdError> {
        if self.process_id < 1 {
            return Err(refuse(
                INVALID_MAINTENANCE,
                "session maintenance claim processId must be a positive integer",
            ));
        }
        if self.session_id.trim().is_empty() || self.claim_token.trim().is_empty() {
            return Err(refuse(INVALID_MAINTENANCE, "session maintenance claim is incomplete"));
        }
        Ok(())
    }

    /// Whether `request` is held by exactly this claim.
    #[must_use]
    pub fn owns(&self, request: &MaintenanceRequest) -> bool {
        request.claimed_process_id == Some(self.process_id)
            && request.claimed_session_id.as_deref() == Some(self.session_id.as_str())
            && request.claim_token.as_deref() == Some(self.claim_token.as_str())
    }
}

/// The chiefd-injected identity a worker presents with every verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedIdentity {
    /// Who the caller is.
    pub person_id: String,
}

impl ExpectedIdentity {
    fn assert_owns(&self, request: &MaintenanceRequest) -> Result<(), ChiefdError> {
        if request.person_id != self.person_id {
            return Err(refuse(
                MAINTENANCE_IDENTITY_MISMATCH,
                "Session maintenance request does not match the ChiefD-injected person",
            ));
        }
        Ok(())
    }
}

/// Everything `queue` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueInput {
    /// What kind of maintenance.
    pub action: MaintenanceAction,
    /// Whose session.
    pub person_id: String,
    /// Who asked.
    pub requested_by: String,
    /// Why.
    pub reason: String,
    /// Whether the supervisor raised it rather than a person.
    pub automatic: bool,
    // TOMBSTONE: `model` and `model_provider`, `set_model`'s only inputs.
    /// Interrupt the live turn before claiming.
    pub force: Option<bool>,
}

fn bounded(value: &str, label: &str, maximum: usize) -> Result<String, ChiefdError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(refuse(INVALID_MAINTENANCE, format!("{label} is required")));
    }
    if trimmed.chars().count() > maximum {
        return Err(refuse(
            INVALID_MAINTENANCE,
            format!("{label} must be at most {maximum} characters"),
        ));
    }
    Ok(trimmed.to_string())
}

/// How long two identical maintenance asks are treated as ONE request.
///
/// A replay is the same turn being re-executed after an interruption, so it
/// lands within seconds of the original; a genuine repeat is a later decision
/// by an agent that saw the first one finish. Sixty seconds is comfortably
/// longer than a relaunch-and-resume, and far shorter than any sane interval
/// between two deliberate compactions of the same session.
///
/// The failure directions are not symmetric, which is why this is not tuned
/// tighter: too SHORT lets a slow replay through as a second real pane restart;
/// too LONG delays a genuine second ask, which is recoverable by asking again.
pub const MAINTENANCE_REPLAY_WINDOW_MS: i64 = 60_000;

/// A maintenance request's durable identity: a hash of what was asked for.
///
/// CONTENT ONLY. No clock, no counter, no bucket. Two asks for the same thing
/// produce the same id, and WHEN they arrived is decided separately, by the
/// sliding window at the call site.
///
/// Length-prefixed fields rather than a separator: `person_id` is
/// operator-influenced and a model string can contain very nearly anything, so
/// a separator is unambiguous only until somebody uses one in a name.
fn request_identity(person_id: &str, action: MaintenanceAction, force: bool) -> String {
    use crate::hexdigest::hex_digest;
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut field = |value: &str| {
        hasher.update(value.len().to_le_bytes());
        hasher.update(value.as_bytes());
    };
    field(person_id);
    field(action.as_str());
    field(if force { "force" } else { "no-force" });
    // TWO CONSTANT `unset` FIELDS, KEPT ON PURPOSE.
    //
    // `set_model`'s model and provider were hashed here, each contributing
    // either `set`+value or `unset`. The action is deleted, so both were always
    // going to be `unset` from now on — but REMOVING the fields would change
    // every family digest, and the digest is the request id. An in-flight
    // compaction minted before this deploy would then not match one minted
    // after, and the duplicate-suppression window would miss exactly once
    // across the upgrade.
    //
    // Two constant fields cost nothing and keep the ids stable. They are the
    // cheapest possible answer to "do not change a durable identifier as a side
    // effect of deleting something unrelated to it".
    field("unset");
    field("unset");
    format!("session-maintenance:{}", hex_digest(hasher.finalize()))
}

/// The newest record in one content family, and the id the next one would take.
///
/// A content-only id cannot be the durable key on its own: two asks that are
/// genuinely different DECISIONS -- a compaction today and another next week --
/// hash identically, and writing the second under the same key would overwrite
/// the first's audit trail and push a duplicate onto `request_order`. So the
/// durable id is `<family>` for the first and `<family>#<n>` after it.
///
/// THIS IS NOT #1039's CANDIDATE-INDEX ESCAPE HATCH, and the difference is the
/// whole ruling. There, an index advanced so a DELIBERATE repeat could still be
/// sent -- right for a message, since a second "any update?" is a real message
/// somebody needs. Here `n` advances ONLY when the caller has already been
/// found outside the replay window, so it can never let an identical ask inside
/// the window through. Inside the window there is no next id to reach: the
/// prior record is returned and this function is not consulted for a mint.
fn newest_in_family(ledger: &SessionMaintenanceLedger, family: &str) -> (Option<String>, String) {
    let mut newest: Option<String> = None;
    let mut candidate = family.to_owned();
    let mut next = 1u32;
    while ledger.requests.contains_key(&candidate) {
        newest = Some(candidate);
        candidate = format!("{family}#{next}");
        next += 1;
    }
    (newest, candidate)
}

/// Queue one durable, idempotent request.
///
/// IDEMPOTENT BY IDENTITY, not merely by open status. A duplicate open request
/// for the same person, session, action, force intent AND value is reused;
/// and because the id is a hash of exactly those fields, a replay of a request
/// that has already been CONSUMED is recognized too, and returns what happened
/// rather than doing it again. That second half matters under kill-and-resume,
/// where a `fresh_session` replay would otherwise be a second real pane
/// restart. The value comparison is
/// load-bearing: a later `set_model` for a DIFFERENT model must never be
/// silently absorbed into a stale queued request for the model it is trying to
/// change away from.
///
/// # Errors
/// [`INVALID_MAINTENANCE`] for an unknown person, an out-of-set thinking level,
/// or a missing value for a value-bearing action.
pub fn queue(
    ledger: &mut SessionMaintenanceLedger,
    manifest: &OrganizationManifest,
    input: &QueueInput,
    at: &str,
) -> Result<MaintenanceRequest, ChiefdError> {
    let person_id = bounded(&input.person_id, "session maintenance.personId", 128)?;
    let requested_by = bounded(&input.requested_by, "session maintenance.requestedBy", 128)?;
    if manifest.person(&person_id).is_none()
        || (manifest.person(&requested_by).is_none()
            && requested_by != SESSION_MAINTENANCE_OPERATOR_REQUESTER)
    {
        return Err(refuse(INVALID_MAINTENANCE, "Session maintenance person is unknown"));
    }
    // AUTHORIZATION, not existence. This asked only whether both ids were
    // KNOWN to the manifest, so two strangers in one company passed: anybody
    // could queue a compaction, a thinking change or a model change against
    // anybody else. Track B1 of the design record.
    //
    // The predicate is the one the server already answers to clients at
    // `/v1/org/control-authority/person-in-scope`; applying it to the MUTATION
    // is the whole point, because a client is free not to ask.
    let actor = if requested_by == SESSION_MAINTENANCE_OPERATOR_REQUESTER {
        ControlActor::Operator
    } else {
        ControlActor::Person(requested_by.clone())
    };
    if !person_is_in_scope(manifest, &actor, &person_id) {
        return Err(refuse(
            INVALID_MAINTENANCE,
            format!(
                "'{requested_by}' does not manage '{person_id}': session maintenance acts on \
                 your own subtree, or on yourself"
            ),
        ));
    }
    // THE LEDGER IS KEPT, THE INTERROGATION IS NOT. A blank reason used to
    // refuse the whole request; it now records what the daemon knows — the
    // action and the authenticated requester — which is the same provenance
    // every structural verb writes. A reason the caller DOES supply is still
    // bounded and still recorded verbatim.
    let reason = if input.reason.trim().is_empty() {
        format!("{} requested by {requested_by}", input.action.as_str())
    } else {
        bounded(&input.reason, "session maintenance.reason", 500)?
    };

    let force = input.force.unwrap_or(false);
    let existing = ledger.ordered_requests().find(|request| {
        request.status.is_open()
            && request.action == input.action
            && request.person_id == person_id
            && request.force.unwrap_or(false) == force
    });
    if let Some(existing) = existing {
        return Ok(existing.clone());
    }

    // THE REQUEST'S IDENTITY IS ITS CONTENT PLUS A BOUNDED REPLAY WINDOW.
    //
    // The id used to be `session-maintenance:<N>:<person>:<action>`, counted
    // from `request_order.len()`, so a REPLAY minted a brand-new request. Under
    // kill-and-resume a replay is an ordinary event: an agent whose pane is
    // killed mid-turn resumes from its transcript and may reissue a tool call
    // that already committed but whose result never reached the model.
    //
    // The `existing` reuse above is not enough on its own, because it only
    // matches an OPEN request. Once the first had been CONSUMED, a replay
    // sailed past it and queued a second -- benign for `compact`, a second REAL
    // pane restart for `fresh_session`.
    //
    // WHY A WINDOW. A replay of a CONSUMED request is benign for `compact` and
    // a second REAL pane restart for `fresh_session`. For a self-requested
    // `fresh_session` that loops: each restart re-interrupts the call whose
    // result never arrived.
    //
    // A pure content hash is wrong in the other direction: with nothing but
    // content in the id, a completed request absorbs every later identical ask
    // for ever. A long-lived session legitimately compacts many times, and
    // `set_thinking` high->low->high or `set_model` A->B->A would silently
    // return the first completed record and never apply.
    //
    // The window separates the two cases on the axis that actually
    // distinguishes them: a replay arrives SECONDS after the original, because
    // it is the same turn being re-executed; a genuine repeat is a later
    // decision.
    //
    // IT IS A SLIDING WINDOW, MEASURED AGAINST THE PRIOR RECORD'S OWN STAMP,
    // and not a fixed bucket. `now.div_euclid(WINDOW)` was the first attempt and
    // it is subtly broken: bucket boundaries fall at fixed instants, so the
    // protection an ask receives is uniform over (0, WINDOW] depending on where
    // in a bucket it happens to land. A `fresh_session` completing at 59.5s of
    // a bucket and replayed one second later lands in the NEXT bucket, hashes
    // to a different id, and restarts the pane -- the exact loop this exists to
    // stop, at whatever rate the boundary happens to allow. Comparing against
    // the prior record's own `requested_at` gives every ask the full window.
    //
    // This is #1039's shape (`org-send-replay.ts`), deliberately: one rule for
    // replay-prone tools, because two mechanisms that disagree about what a
    // replay IS would be worse than either alone.
    //
    // WHAT IS NOT COPIED FROM #1039: its candidate-INDEX escape hatch, which
    // lets a deliberate repeat outside the window take the next index. That is
    // right for messages -- a second "any update?" twenty minutes later is a
    // real message somebody needs -- and wrong here. A second `fresh_session`
    // is a second REAL PANE RESTART, and there is no legitimate "send it
    // anyway" for a restart. Outside the window a fresh request mints normally
    // because it is a new decision; inside it, an identical ask has no way
    // through at all.
    let family = request_identity(&person_id, input.action, force);
    let (newest, next_id) = newest_in_family(ledger, &family);
    if let Some(prior) = newest.as_ref().and_then(|id| ledger.requests.get(id)) {
        let within = match (parse_iso_millis(at), parse_iso_millis(&prior.requested_at)) {
            (Some(now), Some(prior_at)) => {
                now.saturating_sub(prior_at) <= MAINTENANCE_REPLAY_WINDOW_MS
            }
            // An unreadable stamp on either side is NOT evidence that the
            // window has expired. Absorbing is the safe direction: the cost of
            // being wrong is a delayed genuine repeat, which the caller
            // recovers by asking again, against a duplicated REAL PANE RESTART,
            // which they cannot.
            _ => true,
        };
        if within {
            return Ok(prior.clone());
        }
    }
    let id = next_id;
    let request = MaintenanceRequest {
        id: id.clone(),
        action: input.action,
        person_id,
        requested_by,
        reason,
        automatic: input.automatic,
        status: MaintenanceStatus::Queued,
        requested_at: at.to_string(),
        started_at: None,
        completed_at: None,
        error: None,
        attempt: Some(1),
        recovered_from_request_id: None,
        retry_not_before: None,
        claimed_process_id: None,
        claimed_session_id: None,
        claim_token: None,
        completed_process_id: None,
        completed_session_id: None,
        completion_claim_token: None,
        company_action_id: None,
        force: input.force,
        interrupted_process_id: None,
        interrupted_session_id: None,
        interrupted_claim_token: None,
        interrupted_at: None,
        compact_session_id: None,
        compact_anchor_entry_id: None,
        completed_compaction_entry_id: None,
        extra: Default::default(),
    };
    ledger.requests.insert(id.clone(), request.clone());
    ledger.request_order.push(id);
    ledger.updated_at = at.to_string();
    Ok(request)
}

/// The optional native-compact branch boundary a claim may pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactAnchor {
    /// The Pi session the anchor entry belongs to.
    pub session_id: String,
    /// The entry the compaction must branch from.
    pub entry_id: String,
}

/// What a claimer presents, as one value.
///
/// The same shape [`QueueInput`] has for the queue verb: the caller-supplied
/// half of the claim travels together, leaving `ledger`, `identity`, the
/// fresh-session fact and `at` — which the route layer derives, not the
/// caller — as the ambient arguments around it.
#[derive(Debug, Clone, Copy)]
pub struct StartInput<'a> {
    /// Which kind of maintenance is being claimed.
    pub action: MaintenanceAction,
    /// Claim this exact request, rather than the next admissible one.
    pub request_id: Option<&'a str>,
    /// The live process/session/token fence, when the caller has one.
    pub claim: Option<&'a Claim>,
    /// `compact` only: the branch boundary this claim pins.
    pub compact_anchor: Option<&'a CompactAnchor>,
}

/// Claim the next queued request for the exact running Pi session.
///
/// Returns `None` when there is nothing to claim — which is the ordinary case
/// on a bounded idle probe and never an error. A request whose
/// `retry_not_before` has not elapsed is also `None`: it exists, but it is not
/// admissible yet.
///
/// # Errors
/// [`INVALID_MAINTENANCE`] for a malformed claim or an anchor on a non-compact
/// action; [`MAINTENANCE_STATUS_CONFLICT`] when a fresh-session transition is
/// open for this person.
pub fn start(
    ledger: &mut SessionMaintenanceLedger,
    identity: &ExpectedIdentity,
    input: &StartInput<'_>,
    at: &str,
) -> Result<Option<MaintenanceRequest>, ChiefdError> {
    let StartInput { action, request_id, claim, compact_anchor } = *input;
    if let Some(claim) = claim {
        claim.validated()?;
    }
    let now = parse_iso_millis(at);
    let selected = ledger
        .ordered_requests()
        .find(|candidate| {
            candidate.status == MaintenanceStatus::Queued
                && candidate.person_id == identity.person_id
                && candidate.action == action
                && request_id.is_none_or(|id| candidate.id == id)
        })
        .map(|request| request.id.clone());
    let Some(id) = selected else {
        return Ok(None);
    };
    {
        let request = ledger
            .requests
            .get(&id)
            .ok_or_else(|| refuse(UNKNOWN_MAINTENANCE_REQUEST, "the request being claimed"))?;
        if let (Some(now), Some(not_before)) =
            (now, request.retry_not_before.as_deref().and_then(parse_iso_millis))
        {
            if now < not_before {
                return Ok(None);
            }
        }
    }
    if compact_anchor.is_some() && action != MaintenanceAction::Compact {
        return Err(refuse(
            INVALID_MAINTENANCE,
            "Only compact maintenance can persist a native compact anchor",
        ));
    }
    let request = ledger
        .requests
        .get_mut(&id)
        .ok_or_else(|| refuse(UNKNOWN_MAINTENANCE_REQUEST, "the request being claimed"))?;
    request.status = MaintenanceStatus::Running;
    request.started_at = Some(at.to_string());
    request.retry_not_before = None;
    if let Some(claim) = claim {
        request.claimed_process_id = Some(claim.process_id);
        request.claimed_session_id = Some(claim.session_id.trim().to_string());
        request.claim_token = Some(claim.claim_token.trim().to_string());
    }
    if let Some(anchor) = compact_anchor {
        request.compact_session_id =
            Some(bounded(&anchor.session_id, "session maintenance compact sessionId", 300)?);
        request.compact_anchor_entry_id =
            Some(bounded(&anchor.entry_id, "session maintenance compact anchorEntryId", 300)?);
    }
    let claimed = request.clone();
    ledger.updated_at = at.to_string();
    Ok(Some(claimed))
}

fn request_mut<'a>(
    ledger: &'a mut SessionMaintenanceLedger,
    id: &str,
) -> Result<&'a mut MaintenanceRequest, ChiefdError> {
    ledger.requests.get_mut(id).ok_or_else(|| {
        refuse(UNKNOWN_MAINTENANCE_REQUEST, format!("Unknown session maintenance request '{id}'"))
    })
}

/// Return an exact live claim to the queue when the Pi turn that made
/// maintenance safe is no longer current.
///
/// Deliberately claim-fenced: an older process, native session, or extension
/// installation can never release another runtime's work. Deferral is not an
/// execution attempt, so the request identity and its bounded crash-recovery
/// attempt are unchanged.
///
/// # Errors
/// [`UNKNOWN_MAINTENANCE_REQUEST`], [`MAINTENANCE_IDENTITY_MISMATCH`],
/// [`MAINTENANCE_CLAIM_MISMATCH`], or [`INVALID_MAINTENANCE`].
pub fn defer(
    ledger: &mut SessionMaintenanceLedger,
    id: &str,
    claim: &Claim,
    identity: &ExpectedIdentity,
    at: &str,
) -> Result<MaintenanceRequest, ChiefdError> {
    claim.validated()?;
    let request = request_mut(ledger, id)?;
    identity.assert_owns(request)?;
    // An already-unclaimed queued request is the post-condition this verb
    // exists to reach, so a replay is a success rather than a claim mismatch.
    if request.status == MaintenanceStatus::Queued
        && request.claimed_process_id.is_none()
        && request.claimed_session_id.is_none()
        && request.claim_token.is_none()
    {
        return Ok(request.clone());
    }
    if request.status != MaintenanceStatus::Running || !claim.owns(request) {
        return Err(refuse(
            MAINTENANCE_CLAIM_MISMATCH,
            format!("Session maintenance request '{id}' is not owned by this exact live claim"),
        ));
    }
    request.status = MaintenanceStatus::Queued;
    request.started_at = None;
    request.claimed_process_id = None;
    request.claimed_session_id = None;
    request.claim_token = None;
    request.compact_session_id = None;
    request.compact_anchor_entry_id = None;
    let deferred = request.clone();
    ledger.updated_at = at.to_string();
    Ok(deferred)
}

/// Persist the exact supported Pi interrupt before invoking it.
///
/// Repeated polls from the same live installation are idempotent; a replacement
/// installation may publish its own receipt only while the forced request is
/// still queued.
///
/// # Errors
/// [`UNKNOWN_MAINTENANCE_REQUEST`], [`MAINTENANCE_IDENTITY_MISMATCH`],
/// [`MAINTENANCE_STATUS_CONFLICT`], or [`INVALID_MAINTENANCE`].
pub fn record_interrupt(
    ledger: &mut SessionMaintenanceLedger,
    id: &str,
    claim: &Claim,
    identity: &ExpectedIdentity,
    at: &str,
) -> Result<MaintenanceRequest, ChiefdError> {
    claim.validated()?;
    let request = request_mut(ledger, id)?;
    identity.assert_owns(request)?;
    if request.force != Some(true) {
        return Err(refuse(
            MAINTENANCE_STATUS_CONFLICT,
            format!("Session maintenance request '{id}' is not a forced request"),
        ));
    }
    let already = request.interrupted_process_id == Some(claim.process_id)
        && request.interrupted_session_id.as_deref() == Some(claim.session_id.trim())
        && request.interrupted_claim_token.as_deref() == Some(claim.claim_token.trim());
    if already {
        return Ok(request.clone());
    }
    if request.status != MaintenanceStatus::Queued {
        return Err(refuse(
            MAINTENANCE_STATUS_CONFLICT,
            format!("Session maintenance request '{id}' is no longer waiting for interruption"),
        ));
    }
    request.interrupted_process_id = Some(claim.process_id);
    request.interrupted_session_id = Some(claim.session_id.trim().to_string());
    request.interrupted_claim_token = Some(claim.claim_token.trim().to_string());
    request.interrupted_at = Some(at.to_string());
    let recorded = request.clone();
    ledger.updated_at = at.to_string();
    Ok(recorded)
}

// TOMBSTONE: `complete_native_company_fresh_session`. It credited a company
// native reset whose replacement session had landed — the one completion that
// had to distinguish a source session from its successor. Deleted with the
// feature; no request can be a company native reset any more.

/// What one recovery pass produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveredMaintenance {
    /// The records this pass terminalized as interrupted.
    pub interrupted: Vec<MaintenanceRequest>,
    /// The successors it queued for them.
    pub replacements: Vec<MaintenanceRequest>,
}

/// A newly started Pi proves that a same-person/session request claimed by a
/// different process or claim token was interrupted.
///
/// The failed attempt is preserved and exactly one replacement is queued.
/// Ordinary per-person maintenance stays bounded to
/// [`SESSION_MAINTENANCE_MAX_ATTEMPTS`]; a human company action stays durable
/// with bounded backoff until every target reaches a terminal outcome.
///
/// Re-entering from the SAME process and claim token is a no-op, so an
/// extension reload cannot steal a live compaction callback from itself. A live
/// process legitimately changes its Pi session id during an in-process native
/// replacement, so a session change alone is never read as a process death.
///
/// # Errors
/// [`INVALID_MAINTENANCE`] for a malformed claim;
/// [`MAINTENANCE_STATUS_CONFLICT`] when a company target lost its current
/// request authority or its attempt sequence is exhausted.
pub fn recover_interrupted(
    ledger: &mut SessionMaintenanceLedger,
    identity: &ExpectedIdentity,
    claim: &Claim,
    at: &str,
) -> Result<RecoveredMaintenance, ChiefdError> {
    claim.validated()?;
    let mut report = RecoveredMaintenance::default();
    let recoverable: Vec<String> = ledger
        .ordered_requests()
        .filter(|request| {
            if request.person_id != identity.person_id {
                return false;
            }
            if request.status == MaintenanceStatus::Running {
                return request.claimed_process_id != Some(claim.process_id)
                    || request.claim_token.as_deref() != Some(claim.claim_token.trim());
            }
            // TOMBSTONE: the FAILED-request arm. It readmitted a company
            // request that had died mid-flight — the `historical_native` case,
            // plus the ordinary process interruption — and it was gated on
            // `is_company_request()`, which can never be true again: nothing
            // can mint a `companyActionId`, and the actions it pointed at are
            // gone from both the ledger and the row layer. Only a RUNNING
            // request whose claim no longer matches is recoverable now.
            false
        })
        .map(|request| request.id.clone())
        .collect();

    for id in recoverable {
        let (action, requested_by, reason, automatic, attempt, company_action_id, force) = {
            let request = request_mut(ledger, &id)?;
            if request.status == MaintenanceStatus::Running {
                request.status = MaintenanceStatus::Failed;
                request.completed_at = Some(at.to_string());
                request.error = Some(SESSION_MAINTENANCE_PROCESS_INTERRUPTION_ERROR.to_string());
            }
            report.interrupted.push(request.clone());
            (
                request.action,
                request.requested_by.clone(),
                request.reason.clone(),
                request.automatic,
                request.attempt.unwrap_or(1),
                request.company_action_id.clone(),
                request.force == Some(true),
            )
        };
        let is_company = company_action_id.as_ref().is_some_and(|id| !id.trim().is_empty());
        if !is_company && attempt >= SESSION_MAINTENANCE_MAX_ATTEMPTS {
            continue;
        }
        let next_attempt = attempt.checked_add(1).ok_or_else(|| {
            refuse(
                MAINTENANCE_STATUS_CONFLICT,
                "Company session maintenance attempt sequence is exhausted",
            )
        })?;
        let replacement_id = format!(
            "session-maintenance:{}:{}:{}",
            ledger.request_order.len() + 1,
            identity.person_id,
            action.as_str()
        );
        // The SAME ladder the session-rebind recovery path uses. It lived
        // here in a second copy with a 1000 ms base against that one's 250 ms,
        // so a successor's admission time depended on which way its predecessor
        // died; the conformance corpus caught it (#751/G14).
        let retry_not_before = if is_company {
            let delay =
                session_maintenance_retry_delay_ms(next_attempt).map_err(ChiefdError::from)?;
            parse_iso_millis(at).map(|now| iso_millis(now + delay))
        } else {
            None
        };
        let replacement = MaintenanceRequest {
            id: replacement_id.clone(),
            action,
            person_id: identity.person_id.clone(),
            requested_by,
            reason,
            automatic,
            status: MaintenanceStatus::Queued,
            requested_at: at.to_string(),
            started_at: None,
            completed_at: None,
            error: None,
            attempt: Some(next_attempt),
            recovered_from_request_id: Some(id.clone()),
            retry_not_before,
            claimed_process_id: None,
            claimed_session_id: None,
            claim_token: None,
            completed_process_id: None,
            completed_session_id: None,
            completion_claim_token: None,
            company_action_id: company_action_id.clone(),
            force: if is_company { Some(force) } else { None },
            interrupted_process_id: None,
            interrupted_session_id: None,
            interrupted_claim_token: None,
            interrupted_at: None,
            compact_session_id: None,
            compact_anchor_entry_id: None,
            completed_compaction_entry_id: None,
            extra: Default::default(),
        };
        ledger.requests.insert(replacement_id.clone(), replacement.clone());
        ledger.request_order.push(replacement_id.clone());
        // TOMBSTONE: the owning company target's pointer advance, which moved
        // in the SAME draft that created the successor so a crash between the
        // two could not leave an action pointing at a terminal record with a
        // live successor nobody owns. No request carries a `companyActionId`
        // any more, so there is no pointer and no second write to keep atomic.
        report.replacements.push(replacement);
    }
    if !report.interrupted.is_empty() {
        ledger.updated_at = at.to_string();
    }
    Ok(report)
}

/// What a finisher presents, as one value — [`StartInput`]'s counterpart for
/// the closing verb.
#[derive(Debug, Clone, Copy)]
pub struct FinishInput<'a> {
    /// The request being closed.
    pub id: &'a str,
    /// The terminal status it lands on.
    pub status: MaintenanceStatus,
    /// The persisted diagnostic, when there is one.
    pub error: Option<&'a str>,
    /// The native compaction entry that completed anchored compact
    /// maintenance.
    pub compact_entry_id: Option<&'a str>,
}

/// Close a request.
///
/// Re-finishing an already-terminal request is an idempotent success, so a Pi
/// that dies between the durable write and its own acknowledgement can replay
/// safely — unless it presents a DIFFERENT native compaction entry, which means
/// two branches claim the same request and is refused.
///
/// # Errors
/// [`UNKNOWN_MAINTENANCE_REQUEST`], [`MAINTENANCE_IDENTITY_MISMATCH`],
/// [`MAINTENANCE_STATUS_CONFLICT`], or [`INVALID_MAINTENANCE`].
pub fn finish(
    ledger: &mut SessionMaintenanceLedger,
    input: &FinishInput<'_>,
    identity: &ExpectedIdentity,
    at: &str,
) -> Result<MaintenanceRequest, ChiefdError> {
    let FinishInput { id, status, error, compact_entry_id } = *input;
    if !status.is_terminal() {
        return Err(refuse(
            INVALID_MAINTENANCE,
            "session maintenance.status must be completed, failed, or skipped",
        ));
    }
    let request = request_mut(ledger, id)?;
    identity.assert_owns(request)?;
    if request.status.is_terminal() {
        if let Some(entry) = compact_entry_id {
            let entry = bounded(entry, "session maintenance compactEntryId", 300)?;
            if request.completed_compaction_entry_id.as_deref() != Some(entry.as_str()) {
                return Err(refuse(
                    MAINTENANCE_STATUS_CONFLICT,
                    format!(
                        "Session maintenance request '{id}' was completed by another native compaction entry"
                    ),
                ));
            }
        }
        return Ok(request.clone());
    }
    if status == MaintenanceStatus::Completed
        && request.action == MaintenanceAction::Compact
        && request.compact_anchor_entry_id.is_some()
    {
        let entry = bounded(
            compact_entry_id.unwrap_or_default(),
            "session maintenance compactEntryId",
            300,
        )?;
        request.completed_compaction_entry_id = Some(entry);
    } else if compact_entry_id.is_some() {
        return Err(refuse(
            INVALID_MAINTENANCE,
            "A native compaction entry may only complete anchored compact maintenance",
        ));
    }
    request.status = status;
    request.completed_at = Some(at.to_string());
    if let Some(error) = error.map(str::trim).filter(|e| !e.is_empty()) {
        request.error = Some(bounded_error(error));
    }
    let finished = request.clone();
    ledger.updated_at = at.to_string();
    Ok(finished)
}

/// The persisted-diagnostic bound the ledger validator enforces.
fn bounded_error(error: &str) -> String {
    const LIMIT: usize = 600;
    if error.chars().count() <= LIMIT {
        return error.to_string();
    }
    error.chars().take(LIMIT).collect()
}

/// Close every QUEUED company-action request whose target is no longer desired
/// active, so a blocked fleet self-heals without an operator.
///
/// Idempotent and company-scoped: it only ever moves queued company requests to
/// `skipped`, and only for people the caller proved are parked.
///
/// # Errors
/// Never refuses; the signature mirrors its siblings so the route layer has one
/// error path.
pub fn skip_parked_company_targets(
    ledger: &mut SessionMaintenanceLedger,
    parked: &[String],
    at: &str,
) -> Result<Vec<String>, ChiefdError> {
    let ids: Vec<String> = ledger
        .ordered_requests()
        .filter(|request| {
            request.status == MaintenanceStatus::Queued
                && request.is_company_request()
                && parked.contains(&request.person_id)
        })
        .map(|request| request.id.clone())
        .collect();
    for id in &ids {
        if let Some(request) = ledger.requests.get_mut(id) {
            request.status = MaintenanceStatus::Skipped;
            request.completed_at = Some(at.to_string());
            request.error = Some(SESSION_MAINTENANCE_TARGET_PARKED_ERROR.to_string());
        }
    }
    if !ids.is_empty() {
        ledger.updated_at = at.to_string();
    }
    Ok(ids)
}

/// Close every open request belonging to a person the manifest no longer has,
/// or whose requester departed. The read path calls this so a structural
/// removal cannot leave the ledger permanently invalid.
///
/// Returns whether anything changed.
pub fn reconcile_people(
    ledger: &mut SessionMaintenanceLedger,
    manifest: &OrganizationManifest,
    at: &str,
) -> bool {
    let orphaned: Vec<String> = ledger
        .ordered_requests()
        .filter(|request| {
            request.status.is_open()
                && !manifest
                    .person(&request.person_id)
                    .is_some_and(|person| person.employment_state != EmploymentState::Departed)
        })
        .map(|request| request.id.clone())
        .collect();
    for id in &orphaned {
        if let Some(request) = ledger.requests.get_mut(id) {
            request.status = MaintenanceStatus::Skipped;
            request.completed_at = Some(at.to_string());
            request.error = Some("The person left the company before maintenance ran.".to_string());
        }
    }
    if orphaned.is_empty() {
        return false;
    }
    ledger.updated_at = at.to_string();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: &str = "2026-08-07T00:00:00.000Z";

    fn manifest() -> OrganizationManifest {
        crate::test_support::northstar_manifest(1_784_116_800_000)
    }

    fn ledger() -> SessionMaintenanceLedger {
        SessionMaintenanceLedger::initial("northstar", AT)
    }

    fn person(manifest: &OrganizationManifest) -> String {
        manifest.chief_person_id().expect("root head").to_string()
    }

    fn compact_input(person_id: &str) -> QueueInput {
        QueueInput {
            action: MaintenanceAction::Compact,
            person_id: person_id.to_string(),
            requested_by: person_id.to_string(),
            reason: "context is full".to_string(),
            automatic: false,
            force: None,
        }
    }

    fn claim() -> Claim {
        Claim {
            process_id: 4242,
            session_id: "session-a".to_string(),
            claim_token: "token-a".to_string(),
        }
    }

    fn identity(person_id: &str) -> ExpectedIdentity {
        ExpectedIdentity { person_id: person_id.to_string() }
    }

    /// TWO STRANGERS IN ONE COMPANY USED TO PASS.
    ///
    /// `queue` asked only whether both ids were KNOWN to the manifest, so any
    /// person could queue a compaction, a thinking change or a model change
    /// against any other. Track B1 of the design record: the check is
    /// scope, and it is the same predicate the server already answers to
    /// clients at `/v1/org/control-authority/person-in-scope` — applied to the
    /// mutation, because a client is free not to ask.
    ///
    /// The fixture is a CEO over two SIBLING departments (`quant`, `it`), so
    /// `it-head` and `signal-researcher` are a genuinely unrelated pair:
    /// neither is in the other's subtree, and both exist, which is exactly the
    /// case the old existence check waved through.
    #[test]
    fn a_stranger_cannot_queue_maintenance_against_somebody_they_do_not_manage() {
        let manifest = manifest();
        let mut input = compact_input("signal-researcher");
        input.requested_by = "it-head".to_string();

        let refusal =
            queue(&mut ledger(), &manifest, &input, AT).expect_err("a stranger must be refused");
        let message = format!("{refusal}");
        assert!(message.contains("it-head"), "must name who was refused: {message}");
        assert!(message.contains("signal-researcher"), "must name the target: {message}");
        assert!(message.contains("does not manage"), "must say WHY: {message}");
    }

    /// The other half, so the refusal above is not merely "everything is
    /// refused": the CEO manages the whole company, a head manages its own
    /// member, and a person acting on THEMSELVES needs no management scope.
    #[test]
    fn a_manager_and_a_self_service_caller_are_both_allowed() {
        let manifest = manifest();

        let mut as_ceo = compact_input("signal-researcher");
        as_ceo.requested_by = person(&manifest);
        queue(&mut ledger(), &manifest, &as_ceo, AT).expect("the CEO manages the whole company");

        let mut as_own_head = compact_input("signal-researcher");
        as_own_head.requested_by = "quant-head".to_string();
        queue(&mut ledger(), &manifest, &as_own_head, AT).expect("a head manages its own member");

        let on_self = compact_input("signal-researcher");
        queue(&mut ledger(), &manifest, &on_self, AT).expect("self-service needs no scope");
    }

    /// The operator is not a person and is in nobody's subtree, so it must
    /// keep passing — otherwise this change would lock out the one caller that
    /// legitimately acts company-wide.
    #[test]
    fn the_operator_requester_still_reaches_everybody() {
        let manifest = manifest();
        let mut input = compact_input("signal-researcher");
        input.requested_by = SESSION_MAINTENANCE_OPERATOR_REQUESTER.to_string();
        queue(&mut ledger(), &manifest, &input, AT).expect("the operator acts company-wide");
    }

    /// NOBODY IS ASKED TO JUSTIFY A MAINTENANCE ACTION, AND THE LEDGER STILL
    /// SAYS WHAT HAPPENED. A blank reason used to refuse the whole request.
    /// It is now accepted, and the daemon authors the record from the action
    /// and the requester — the same rule the structural verbs follow.
    #[test]
    fn a_blank_reason_is_accepted_and_the_line_is_authored() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let mut input = compact_input(&who);
        input.reason = "   ".to_string();
        let queued = queue(&mut ledger, &manifest, &input, AT).expect("a blank reason is accepted");
        assert_eq!(
            ledger.requests.get(&queued.id).expect("the queued request").reason,
            format!("compact requested by {who}")
        );
    }

    /// A reason the caller DOES supply is still recorded verbatim.
    #[test]
    fn a_supplied_reason_is_kept_verbatim() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let queued = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        assert_eq!(
            ledger.requests.get(&queued.id).expect("the queued request").reason,
            "context is full"
        );
    }

    #[test]
    fn queueing_twice_reuses_the_open_request() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let first = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        let second = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("reused");
        assert_eq!(first.id, second.id);
        assert_eq!(ledger.request_order.len(), 1);
    }

    /// THE KILL-AND-RESUME REGRESSION.
    ///
    /// An agent whose pane is killed mid-turn resumes from its transcript and
    /// may reissue a tool call that already COMMITTED but whose result never
    /// reached the model. Before the id became content-derived, the reuse guard
    /// only matched an OPEN request, so a replay arriving after the first had
    /// been consumed sailed past it and queued a second -- a second REAL pane
    /// restart for `fresh_session`.
    #[test]
    fn a_replay_of_a_consumed_request_returns_what_happened_and_does_not_queue_a_second() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        // Was a `fresh_session`, purely to be an action distinct from
        // `compact`. One action exists now, and the subject of these tests —
        // replay absorption — never depended on WHICH action it was.
        let fresh = compact_input(&who);
        let first = queue(&mut ledger, &manifest, &fresh, AT).expect("queued");

        // Consume it: the request is no longer open, which is exactly the state
        // the old guard could not see past.
        if let Some(request) = ledger.requests.get_mut(&first.id) {
            request.status = MaintenanceStatus::Completed;
            request.completed_at = Some(AT.to_string());
        }

        // The replay: the same turn re-executed, inside the replay window.
        let replay = queue(&mut ledger, &manifest, &fresh, AT).expect("replayed");
        assert_eq!(replay.id, first.id, "a replay is the request it already is");
        assert_eq!(
            replay.status,
            MaintenanceStatus::Completed,
            "the caller is told what actually happened, not handed a fresh restart"
        );
        assert_eq!(
            ledger.request_order.len(),
            1,
            "a replayed fresh_session must never become a second real pane restart"
        );
    }

    /// The other direction, and the twin that proves the guard is not simply
    /// wired shut: a GENUINE later ask must still go through.
    ///
    /// This is the case that rules OUT a pure content hash. A long-lived
    /// session legitimately compacts many times, and `set_thinking`
    /// high->low->high or `set_model` A->B->A must each apply. With content
    /// alone in the id, the completed first request would absorb every later
    /// identical ask for ever. The replay WINDOW is what separates them, on the
    /// axis that actually distinguishes a replay from a repeat: a replay is the
    /// same turn re-executed seconds later, a repeat is a later decision.
    #[test]
    fn a_genuine_later_ask_outside_the_replay_window_is_a_new_request() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let first = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        if let Some(request) = ledger.requests.get_mut(&first.id) {
            request.status = MaintenanceStatus::Completed;
        }

        // Well past the window, same session -- a session that filled up
        // again and legitimately needs a second compaction.
        let later = "2026-08-07T00:05:00.000Z";
        let second = queue(&mut ledger, &manifest, &compact_input(&who), later).expect("queued");
        assert_ne!(second.id, first.id);
        assert_eq!(ledger.request_order.len(), 2, "a real second ask must still be honoured");
    }

    /// THE BUCKET BUG, pinned at the boundary that exposed it.
    ///
    /// The first attempt bucketed the clock — `now.div_euclid(WINDOW)` — and
    /// hashed the bucket ordinal into the id. Bucket edges fall at fixed
    /// instants, so the protection an ask received depended on where in a bucket
    /// it happened to land: uniform over (0, WINDOW], and arbitrarily close to
    /// zero near an edge. A `fresh_session` completing just before a boundary
    /// and replayed one second later hashed into the NEXT bucket and restarted
    /// the pane — the exact loop the mechanism exists to stop.
    ///
    /// The sliding comparison is against the PRIOR RECORD'S OWN stamp, so every
    /// ask gets the full window no matter when it arrives. This test places the
    /// original and its replay either side of a bucket edge, which is precisely
    /// the arrangement the old code got wrong and the new code must not.
    #[test]
    fn a_replay_one_second_later_is_absorbed_even_across_a_bucket_boundary() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        // Was a `fresh_session`, purely to be an action distinct from
        // `compact`. One action exists now, and the subject of these tests —
        // replay absorption — never depended on WHICH action it was.
        let fresh = compact_input(&who);

        // 00:00:59.500 — half a second before a 60s bucket edge.
        let at_first = "2026-08-07T00:00:59.500Z";
        let first = queue(&mut ledger, &manifest, &fresh, at_first).expect("queued");
        if let Some(request) = ledger.requests.get_mut(&first.id) {
            request.status = MaintenanceStatus::Completed;
        }

        // 00:01:00.500 — ONE SECOND later, and on the far side of the edge.
        let at_replay = "2026-08-07T00:01:00.500Z";
        let replay = queue(&mut ledger, &manifest, &fresh, at_replay).expect("replayed");

        assert_eq!(replay.id, first.id, "a bucket edge is not a new decision");
        assert_eq!(
            ledger.request_order.len(),
            1,
            "one second after a restart is a replay wherever the clock's bucket edges happen to \
             fall; a second record here is a second REAL pane restart"
        );
    }

    /// The twin, so the window is not simply wired shut: outside it, a genuine
    /// repeat mints a DISTINCT durable record rather than overwriting the first.
    ///
    /// The distinctness matters as much as the minting. The id is content-only,
    /// so without a family suffix the second record would be written under the
    /// first's key — silently destroying the earlier request's audit trail and
    /// pushing a duplicate onto `request_order`.
    #[test]
    fn a_genuine_repeat_outside_the_window_mints_a_distinct_record() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        // Was a `fresh_session`, purely to be an action distinct from
        // `compact`. One action exists now, and the subject of these tests —
        // replay absorption — never depended on WHICH action it was.
        let fresh = compact_input(&who);

        let first = queue(&mut ledger, &manifest, &fresh, AT).expect("queued");
        if let Some(request) = ledger.requests.get_mut(&first.id) {
            request.status = MaintenanceStatus::Completed;
        }

        // Well outside the window: a later decision by an agent that saw the
        // first one finish.
        let later = "2026-08-07T00:10:00.000Z";
        let second = queue(&mut ledger, &manifest, &fresh, later).expect("queued");

        assert_ne!(second.id, first.id, "a later decision is its own request");
        assert_eq!(ledger.request_order.len(), 2, "and both are durable");
        assert_eq!(
            ledger.requests.get(&first.id).map(|r| r.status),
            Some(MaintenanceStatus::Completed),
            "the first record must survive the second: its audit trail is not overwritten"
        );

        // And the escape hatch #1039 has is deliberately absent here: an
        // identical ask INSIDE the window after that second one is still
        // absorbed, never handed the next id.
        let replay_of_second = queue(&mut ledger, &manifest, &fresh, later).expect("replayed");
        assert_eq!(replay_of_second.id, second.id);
        assert_eq!(
            ledger.request_order.len(),
            2,
            "a sequence that advances only OUTSIDE the window can never let an inside-window ask \
             through; there is no legitimate `restart anyway`"
        );
    }

    #[test]
    fn a_different_force_intent_is_a_different_request() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        let forced = QueueInput { force: Some(true), ..compact_input(&who) };
        queue(&mut ledger, &manifest, &forced, AT).expect("queued");
        assert_eq!(ledger.request_order.len(), 2);
    }

    // TOMBSTONE: `model_value_is_durable_and_part_of_request_identity`. It
    // pinned that two `set_model` requests for DIFFERENT models are different
    // families rather than duplicates of each other. `set_model` is deleted, so
    // there is no second field left to distinguish two requests by — see the
    // two constant `unset` fields in `request_identity` for why the digest
    // still includes them.

    #[test]
    fn start_claims_the_queued_request_and_records_the_triple() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        let claimed = start(
            &mut ledger,
            &identity(&who),
            &StartInput {
                action: MaintenanceAction::Compact,
                request_id: None,
                claim: Some(&claim()),
                compact_anchor: None,
            },
            AT,
        )
        .expect("start")
        .expect("a queued request");
        assert_eq!(claimed.status, MaintenanceStatus::Running);
        assert_eq!(claimed.claimed_process_id, Some(4242));
        assert_eq!(claimed.claim_token.as_deref(), Some("token-a"));
    }

    #[test]
    fn start_on_an_empty_queue_is_none_not_an_error() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let claimed = start(
            &mut ledger,
            &identity(&who),
            &StartInput {
                action: MaintenanceAction::Compact,
                request_id: None,
                claim: Some(&claim()),
                compact_anchor: None,
            },
            AT,
        )
        .expect("start");
        assert!(claimed.is_none());
        let _ = manifest;
    }

    #[test]
    fn a_retry_not_before_in_the_future_is_not_yet_claimable() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let request = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        ledger.requests.get_mut(&request.id).expect("request").retry_not_before =
            Some("2026-08-07T00:00:30.000Z".to_string());
        let claimed = start(
            &mut ledger,
            &identity(&who),
            &StartInput {
                action: MaintenanceAction::Compact,
                request_id: None,
                claim: Some(&claim()),
                compact_anchor: None,
            },
            AT,
        )
        .expect("start");
        assert!(claimed.is_none());
    }

    #[test]
    fn defer_returns_the_request_to_the_queue_and_drops_the_claim() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let queued = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        start(
            &mut ledger,
            &identity(&who),
            &StartInput {
                action: MaintenanceAction::Compact,
                request_id: None,
                claim: Some(&claim()),
                compact_anchor: None,
            },
            AT,
        )
        .expect("start")
        .expect("claimed");
        let deferred =
            defer(&mut ledger, &queued.id, &claim(), &identity(&who), AT).expect("deferred");
        assert_eq!(deferred.status, MaintenanceStatus::Queued);
        assert!(deferred.claim_token.is_none());
    }

    #[test]
    fn a_foreign_claim_cannot_defer_another_runtimes_work() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let queued = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        start(
            &mut ledger,
            &identity(&who),
            &StartInput {
                action: MaintenanceAction::Compact,
                request_id: None,
                claim: Some(&claim()),
                compact_anchor: None,
            },
            AT,
        )
        .expect("start")
        .expect("claimed");
        let foreign = Claim { claim_token: "token-b".to_string(), ..claim() };
        let error = defer(&mut ledger, &queued.id, &foreign, &identity(&who), AT)
            .expect_err("foreign claim");
        assert_eq!(error.code(), Some(MAINTENANCE_CLAIM_MISMATCH));
    }

    #[test]
    fn a_mismatched_person_is_refused_before_the_claim_is_even_considered() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let queued = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        let stale = ExpectedIdentity { person_id: "somebody-else".to_string() };
        let error =
            defer(&mut ledger, &queued.id, &claim(), &stale, AT).expect_err("stale identity");
        assert_eq!(error.code(), Some(MAINTENANCE_IDENTITY_MISMATCH));
    }

    #[test]
    fn interrupt_is_refused_on_an_unforced_request() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let queued = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        let error = record_interrupt(&mut ledger, &queued.id, &claim(), &identity(&who), AT)
            .expect_err("not forced");
        assert_eq!(error.code(), Some(MAINTENANCE_STATUS_CONFLICT));
    }

    #[test]
    fn interrupt_from_the_same_installation_is_idempotent() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let forced = QueueInput { force: Some(true), ..compact_input(&who) };
        let queued = queue(&mut ledger, &manifest, &forced, AT).expect("queued");
        record_interrupt(&mut ledger, &queued.id, &claim(), &identity(&who), AT).expect("first");
        let second = record_interrupt(&mut ledger, &queued.id, &claim(), &identity(&who), AT)
            .expect("replay");
        assert_eq!(second.interrupted_process_id, Some(4242));
    }

    #[test]
    fn finish_records_the_terminal_status_once_and_replays_cleanly() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let queued = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        let done = finish(
            &mut ledger,
            &FinishInput {
                id: &queued.id,
                status: MaintenanceStatus::Completed,
                error: None,
                compact_entry_id: None,
            },
            &identity(&who),
            AT,
        )
        .expect("finished");
        assert_eq!(done.status, MaintenanceStatus::Completed);
        let replay = finish(
            &mut ledger,
            &FinishInput {
                id: &queued.id,
                status: MaintenanceStatus::Failed,
                error: Some("late"),
                compact_entry_id: None,
            },
            &identity(&who),
            AT,
        )
        .expect("replay");
        assert_eq!(replay.status, MaintenanceStatus::Completed);
    }

    /// The `ambiguous` -> `failed` row, driven end to end through the ops that
    /// write it.
    ///
    /// #751 left this as the one compaction outcome nobody had watched chiefd
    /// record. The live proof it was blocked on needs a credentialed host, a
    /// real runtime identity and four or five large reads before the request
    /// is anything but `skipped` — but the NEGATIVE case needs none of that,
    /// and it is the case that actually writes a terminal row. A compact
    /// request claimed against a session id that is not the live one makes
    /// `nativeCompactionProof` answer `ambiguous` on its very first branch
    /// (`organization-intercom.ts:10176`, `sessionManager.getSessionId() !==
    /// request.compactSessionId`) with no compaction and no provider spend, and
    /// the session-start receipt maps every non-`proven`, non-`absent` state to
    /// exactly this `finish status=failed`.
    ///
    /// What was already covered, and what was not: `nativeCompactionProof`'s
    /// own `ambiguous` verdict is unit-locked in
    /// `packages/piing/test/extensions/NativeCompactionReceipt.test.ts`, and
    /// `finish` was covered for `Completed` and for replay. The DURABLE row —
    /// a first-outcome `Failed` carrying the operator-facing reason — was
    /// asserted nowhere, which is how a receipt could have terminalised a
    /// request with its reason dropped and no test would have moved.
    #[test]
    fn a_diverged_compaction_receipt_terminalises_failed_and_keeps_its_reason() {
        // The exact sentence the session-start receipt sends for a proof that
        // is neither `proven` nor `absent`.
        const DIVERGED: &str =
            "Native compaction receipt diverged from the persisted Pi session anchor; refusing to compact twice.";

        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        let running = start(
            &mut ledger,
            &identity(&who),
            &StartInput {
                action: MaintenanceAction::Compact,
                request_id: None,
                claim: Some(&claim()),
                compact_anchor: None,
            },
            AT,
        )
        .expect("start")
        .expect("a queued request");
        assert_eq!(running.status, MaintenanceStatus::Running);

        let failed = finish(
            &mut ledger,
            &FinishInput {
                id: &running.id,
                status: MaintenanceStatus::Failed,
                error: Some(DIVERGED),
                // No entry id: nothing was compacted. A `Failed` terminal that
                // carried one would be claiming a receipt it does not have.
                compact_entry_id: None,
            },
            &identity(&who),
            AT,
        )
        .expect("a diverged receipt terminalises rather than refusing");

        assert_eq!(failed.status, MaintenanceStatus::Failed);
        assert_eq!(
            failed.error.as_deref(),
            Some(DIVERGED),
            "the reason is the whole value of this row — a `failed` with no reason sends an operator \
             looking for a compaction that never happened"
        );
        assert!(
            failed.completed_compaction_entry_id.is_none(),
            "nothing was compacted, so there is no entry to point at"
        );

        // And it is terminal in the same way `Completed` is: a later receipt
        // arriving from a slower path cannot rewrite the reason or promote the
        // row, which is what keeps the operator's account of it stable.
        let replay = finish(
            &mut ledger,
            &FinishInput {
                id: &running.id,
                status: MaintenanceStatus::Completed,
                error: None,
                compact_entry_id: None,
            },
            &identity(&who),
            AT,
        )
        .expect("replay");
        assert_eq!(replay.status, MaintenanceStatus::Failed);
        assert_eq!(replay.error.as_deref(), Some(DIVERGED));
    }

    #[test]
    fn a_non_terminal_finish_status_is_refused() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let queued = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        let error = finish(
            &mut ledger,
            &FinishInput {
                id: &queued.id,
                status: MaintenanceStatus::Running,
                error: None,
                compact_entry_id: None,
            },
            &identity(&who),
            AT,
        )
        .expect_err("not terminal");
        assert_eq!(error.code(), Some(INVALID_MAINTENANCE));
    }

    #[test]
    fn a_compaction_entry_on_unanchored_maintenance_is_refused() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let queued = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        let error = finish(
            &mut ledger,
            &FinishInput {
                id: &queued.id,
                status: MaintenanceStatus::Completed,
                error: None,
                compact_entry_id: Some("entry-1"),
            },
            &identity(&who),
            AT,
        )
        .expect_err("unanchored");
        assert_eq!(error.code(), Some(INVALID_MAINTENANCE));
    }

    #[test]
    fn recovery_from_the_same_process_and_token_is_a_no_op() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        start(
            &mut ledger,
            &identity(&who),
            &StartInput {
                action: MaintenanceAction::Compact,
                request_id: None,
                claim: Some(&claim()),
                compact_anchor: None,
            },
            AT,
        )
        .expect("start")
        .expect("claimed");
        let report =
            recover_interrupted(&mut ledger, &identity(&who), &claim(), AT).expect("recovery");
        assert!(report.interrupted.is_empty());
        assert!(report.replacements.is_empty());
    }

    #[test]
    fn recovery_from_a_different_process_terminalizes_and_queues_one_successor() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        start(
            &mut ledger,
            &identity(&who),
            &StartInput {
                action: MaintenanceAction::Compact,
                request_id: None,
                claim: Some(&claim()),
                compact_anchor: None,
            },
            AT,
        )
        .expect("start")
        .expect("claimed");
        let successor = Claim { process_id: 5151, ..claim() };
        let report =
            recover_interrupted(&mut ledger, &identity(&who), &successor, AT).expect("recovery");
        assert_eq!(report.interrupted.len(), 1);
        assert_eq!(report.replacements.len(), 1);
        assert_eq!(report.replacements[0].attempt, Some(2));
        assert_eq!(
            report.interrupted[0].error.as_deref(),
            Some(SESSION_MAINTENANCE_PROCESS_INTERRUPTION_ERROR)
        );
    }

    #[test]
    fn per_person_recovery_stops_at_the_attempt_ceiling() {
        let manifest = manifest();
        let who = person(&manifest);
        let mut ledger = ledger();
        let queued = queue(&mut ledger, &manifest, &compact_input(&who), AT).expect("queued");
        {
            let request = ledger.requests.get_mut(&queued.id).expect("request");
            request.attempt = Some(SESSION_MAINTENANCE_MAX_ATTEMPTS);
            request.status = MaintenanceStatus::Running;
            request.claimed_process_id = Some(1);
            request.claimed_session_id = Some("old".to_string());
            request.claim_token = Some("old-token".to_string());
        }
        let report =
            recover_interrupted(&mut ledger, &identity(&who), &claim(), AT).expect("recovery");
        assert_eq!(report.interrupted.len(), 1);
        assert!(report.replacements.is_empty());
    }

    // TOMBSTONE: `a_recovered_company_successor_is_admitted_on_the_shared_ladder`.
    // It pinned that a PROCESS-death successor and a SESSION-change successor
    // are admitted on the ONE ladder, by computing the expectation from
    // `company_session_action`'s definition rather than restating a number —
    // the fix for two copies with a 1000 ms and a 250 ms base, each with its own
    // passing test. The company-action family is deleted, so there is only one
    // ladder left and nothing to diverge from.

    #[test]
    fn a_blank_claim_token_is_refused_rather_than_stored() {
        let bad =
            Claim { process_id: 1, session_id: "s".to_string(), claim_token: "  ".to_string() };
        let error = bad.validated().expect_err("blank token");
        assert_eq!(error.code(), Some(INVALID_MAINTENANCE));
    }

    #[test]
    fn a_non_positive_process_id_is_refused() {
        let bad =
            Claim { process_id: 0, session_id: "s".to_string(), claim_token: "t".to_string() };
        let error = bad.validated().expect_err("bad pid");
        assert_eq!(error.code(), Some(INVALID_MAINTENANCE));
    }
}
