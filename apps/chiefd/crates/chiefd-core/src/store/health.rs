//! The health-monitor store: incidents, observations and log cursors.
//!
//! Port of the durable half of the deleted `org-health-monitor.ts` (formerly
//! `apps/cli/src/legacy/organization/`, retired whole by #825/E8-S3 — chiefd
//! is the sole health monitor now). The *collection* half — runtime audits,
//! `/proc`, reading log files — is executor
//! work and stays behind `HostExecutor`; what lives here is everything that
//! decides what the collected observations mean and what survives to the next
//! pass.
//!
//! Plan §2.7 lists five properties for this store. Each is a named test below:
//!
//! | Property | Where |
//! |---|---|
//! | fail-open (corruption silently resets) | [`read`], polarity `FailOpen` ×3 |
//! | 200-incident truncation | [`MAX_INCIDENTS`], [`apply_cycle`] |
//! | 64/256 KB log caps | [`HEALTH_LOG_READ_LIMIT`], [`per_log_read_limit`] |
//! | ≥15 s independent second sample before paging | [`HEALTH_OBSERVATION_CONFIRMATION_MS`] |
//! | all detail through `redact()` | every write goes through [`bounded_persisted_error`] |
//! | a moving detail keeps one identity | [`IncidentCandidate::identity`], [`apply_cycle`] |
//! | acknowledgement keeps the incident active | [`HealthMonitorIncident::acknowledged_at`], [`apply_cycle`] |
//!
//! # The fingerprint is a hash of the *redacted* detail
//!
//! `fingerprint = sha256(kind \n redact(detail))[..24]`. Redaction therefore
//! sits *inside* incident identity, not beside it: an incident recorded by one
//! build and re-observed by another with a drifted redactor is a **different**
//! incident, resolves the old one, and pages again. That is why
//! [`crate::diagnostics`] carries a byte-parity test against the TypeScript.
//!
//! # One deliberate divergence: which entries a cap keeps
//!
//! The TypeScript caps are `Object.entries(...).slice(-N)`, i.e. "the last N in
//! JavaScript object insertion order". Insertion order is not a property of the
//! stored JSON that survives a round trip through another implementation, and
//! TESTING.md §1.2 forbids behaviour that depends on it. chiefd keeps the
//! **lexicographically last N keys** instead: same cap, same size bound,
//! deterministic on every machine and in every order. Recorded in
//! `DECISIONS.md`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::diagnostics::bounded_persisted_error;
use crate::isotime::{iso_millis, parse_iso_millis};
use crate::ledger::Ledgers;
use crate::polarity::{decode_fail_open, Decoded, FailOpen, StoreKind};
use crate::store::CompanyContext;

/// The one incident kind whose lifecycle is closed by an immutable acceptance
/// marker rather than by ceasing to be observed.
pub const TERMINAL_SUPERVISION_HEALTH_INCIDENT_KIND: &str = "supervision_delivery_failed";

/// Per-file bound on how many newly appended log bytes one pass reads.
pub const HEALTH_LOG_READ_LIMIT: u64 = 64 * 1_024;

/// Bound across *all* monitored files in one pass.
pub const HEALTH_LOG_TOTAL_READ_LIMIT: u64 = 256 * 1_024;

/// How many log files may keep a cursor.
pub const MAX_HEALTH_LOG_FILES: usize = 16;

/// How many incidents, observations and resolutions survive a pass.
pub const MAX_INCIDENTS: usize = 200;

/// A single runtime sample is often taken while a launch is still
/// materializing. An independent later sample is required before paging.
pub const HEALTH_OBSERVATION_CONFIRMATION_MS: i64 = 15_000;

/// Most lines one bounded read yields.
pub const MAX_LOG_LINES_PER_READ: usize = 200;

/// Where a monitored log was last read to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthLogCursor {
    /// `st_dev`, as a string (it is an opaque identity, never arithmetic).
    pub device: String,
    /// `st_ino`, as a string.
    pub inode: String,
    /// Byte offset reached.
    pub offset: u64,
}

/// A provisional runtime observation awaiting confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthMonitorObservation {
    /// When this fingerprint was first seen in the current streak.
    pub first_observed_at: String,
    /// When it was last seen.
    pub last_observed_at: String,
    /// How many consecutive passes have seen it.
    pub count: u64,
}

/// Who may accept the alert for a terminal incident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertAuthority {
    /// The active operator runtime authorized to accept this alert lifecycle.
    pub recipient_person_id: String,
}

/// A recorded incident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthMonitorIncident {
    /// `sha256(kind \n redacted detail)[..24]`.
    pub fingerprint: String,
    /// Incident kind.
    pub kind: String,
    /// Redacted detail. Never raw.
    pub detail: String,
    /// First time this fingerprint was seen in the current streak.
    pub first_seen_at: String,
    /// Most recent sighting.
    pub last_seen_at: String,
    /// Consecutive sightings.
    pub count: u64,
    /// The manager who owns the unblock, or the CEO for company-wide faults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responsible_person_id: Option<String>,
    /// Content-free action rendered in the one deduplicated operator alert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unblock_action: Option<String>,
    /// How many underlying items the candidate summarized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_count: Option<u64>,
    /// Oldest underlying item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_at: Option<String>,
    /// When an operator accepted the alert for this incident.
    ///
    /// An acknowledgement records that a human SAW the alert. It has never
    /// meant the fault is gone, and treating it as though it did made the act
    /// of noticing a problem the thing that hid it: on the live company the CEO
    /// acknowledged at 13:25 and supervision failures went 4 -> 0 while four
    /// fences were still failed. The incident therefore stays active until the
    /// condition it describes actually clears; this field only says a human is
    /// already aware, so a reader can present it differently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<String>,
    /// The person whose own durable mailbox this incident reports as impaired.
    /// An alert routed *to* that person needs an additional out-of-band copy,
    /// since it would otherwise be delivered through the channel it reports
    /// broken. Carried for a later duty; no consumer today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impaired_mailbox_person_id: Option<String>,
    /// Exact operator runtime authorized to accept this alert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert_recipient_person_id: Option<String>,
}

/// A validated projection of an immutable acceptance marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResolution {
    /// The incident fingerprint this closes. Must equal its map key.
    pub fingerprint: String,
    /// Always [`TERMINAL_SUPERVISION_HEALTH_INCIDENT_KIND`].
    pub kind: String,
    /// The `firstSeenAt` of the incident that was accepted.
    pub first_seen_at: String,
    /// Who accepted it.
    pub recipient_person_id: String,
    /// When.
    pub accepted_at: String,
}

/// The durable monitor state, byte-compatible with `health-monitor.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthMonitorState {
    /// Schema version. Only `1`.
    pub version: u32,
    /// The company this state belongs to.
    pub organization: String,
    /// When the last pass ran. Absent ⇒ the next pass baselines log cursors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    /// Log path → cursor.
    #[serde(default)]
    pub cursors: BTreeMap<String, HealthLogCursor>,
    /// Fingerprint → unconfirmed observation.
    #[serde(default)]
    pub observations: BTreeMap<String, HealthMonitorObservation>,
    /// Fingerprint → active incident.
    #[serde(default)]
    pub incidents: BTreeMap<String, HealthMonitorIncident>,
    /// Fingerprint → accepted terminal resolution.
    #[serde(default)]
    pub terminal_resolutions: BTreeMap<String, TerminalResolution>,
}

impl HealthMonitorState {
    /// A clean state for `organization`.
    #[must_use]
    pub fn empty(organization: impl Into<String>) -> Self {
        Self {
            version: 1,
            organization: organization.into(),
            last_run_at: None,
            cursors: BTreeMap::new(),
            observations: BTreeMap::new(),
            incidents: BTreeMap::new(),
            terminal_resolutions: BTreeMap::new(),
        }
    }
}

/// The health-monitor store.
///
/// The store NAME is `"health-monitor"` — the SAME store read through
/// `/v1/org/health-monitor/read`. Its publish ROUTE is deleted (the
/// publisher-route sweep found no caller); the row is written in-process
/// through [`crate::actor::CompanyDb`] (rows in
/// [`crate::store::health_monitor_rows`]). It used to be `"health"`, which
/// collided with `daemon_health_rows::DAEMON_HEALTH_STORE` and routed every
/// duty commit into the daemon-internal `daemon_health_*` tables, invisible
/// to every org-health reader (F16). The rename is the un-cross-wiring: one
/// store, one row set, two merge-safe writers (Step 3 semantics).
pub struct HealthStore;

impl StoreKind for HealthStore {
    const NAME: &'static str = "health-monitor";
    type Body = HealthMonitorState;
}

impl FailOpen for HealthStore {
    fn empty() -> Self::Body {
        // The organization is only known to a caller, and a FailOpen `empty()`
        // takes no arguments. An empty slug can never be mistaken for a real
        // company (slugs match `^[a-z0-9]+(-[a-z0-9]+)*$`), so a state that
        // reached this constructor is inert rather than mis-attributed; `read`
        // stamps the real slug.
        HealthMonitorState::empty(String::new())
    }
}

/// `sha256(kind \n detail)[..24]` — over the **redacted** detail.
#[must_use]
pub fn fingerprint(kind: &str, redacted_detail: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update(b"\n");
    hasher.update(redacted_detail.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(24);
    for byte in digest.iter().take(12) {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Keep only the lexicographically last `limit` entries. See the module note on
/// the deliberate divergence from JavaScript insertion order.
fn cap_last<V>(map: &mut BTreeMap<String, V>, limit: usize) {
    while map.len() > limit {
        let Some(first) = map.keys().next().cloned() else { break };
        map.remove(&first);
    }
}

/// Decode the stored state, discarding entries that do not validate.
///
/// Fail-open at two levels, both ported: a state whose version or organization
/// disagrees resets **entirely**, and inside an otherwise-valid state each
/// individual malformed entry is dropped rather than poisoning the rest.
/// Takes `&str`, not `Option<&str>`: an absent row is not a parse outcome at
/// all, and the two facts stopped sharing a `None` here. See [`read`].
fn parse(
    ctx: &CompanyContext,
    body: &str,
) -> Result<HealthMonitorState, crate::polarity::DecodeRefusal> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| format!("the body is not JSON: {error}"))?;
    let Some(object) = value.as_object() else {
        return Err("the body is JSON but not an object".to_string());
    };
    let stored_version = object.get("version").and_then(serde_json::Value::as_u64);
    let stored_organization = object.get("organization").and_then(serde_json::Value::as_str);
    if stored_version != Some(1) || stored_organization != Some(ctx.slug()) {
        // Not "unreadable" — readable, and belonging to something else. Both
        // take the fail-open path, which is a reset for this store; the reset
        // now says which of the two it was, because "another company's health
        // state is in your row" and "your version moved" want different
        // operators looking at them.
        return Err(format!(
            "the body is version {stored_version:?} for organization {stored_organization:?}, \
             not version 1 for {}",
            ctx.slug()
        ));
    }
    let mut state = HealthMonitorState::empty(ctx.slug());
    state.last_run_at =
        object.get("lastRunAt").and_then(serde_json::Value::as_str).map(ToString::to_string);

    if let Some(cursors) = object.get("cursors").and_then(serde_json::Value::as_object) {
        for (path, candidate) in cursors {
            if let Ok(cursor) = serde_json::from_value::<HealthLogCursor>(candidate.clone()) {
                state.cursors.insert(path.clone(), cursor);
            }
        }
        cap_last(&mut state.cursors, MAX_HEALTH_LOG_FILES);
    }

    if let Some(observations) = object.get("observations").and_then(serde_json::Value::as_object) {
        for (key, candidate) in observations {
            let Ok(observation) =
                serde_json::from_value::<HealthMonitorObservation>(candidate.clone())
            else {
                continue;
            };
            if observation.count < 1
                || observation.count >= 1_000_000
                || parse_iso_millis(&observation.first_observed_at).is_none()
                || parse_iso_millis(&observation.last_observed_at).is_none()
            {
                continue;
            }
            state.observations.insert(key.clone(), observation);
        }
        cap_last(&mut state.observations, MAX_INCIDENTS);
    }

    if let Some(incidents) = object.get("incidents").and_then(serde_json::Value::as_object) {
        for (key, candidate) in incidents {
            let Ok(mut incident) =
                serde_json::from_value::<HealthMonitorIncident>(candidate.clone())
            else {
                continue;
            };
            // Re-redact on read: a record written by an older build with a
            // weaker redactor must not be handed back out unredacted.
            incident.detail = bounded_persisted_error(&incident.detail);
            state.incidents.insert(key.clone(), incident);
        }
    }

    if let Some(resolutions) =
        object.get("terminalResolutions").and_then(serde_json::Value::as_object)
    {
        for (key, candidate) in resolutions {
            let Ok(resolution) = serde_json::from_value::<TerminalResolution>(candidate.clone())
            else {
                continue;
            };
            if resolution.fingerprint != *key
                || resolution.kind != TERMINAL_SUPERVISION_HEALTH_INCIDENT_KIND
                || parse_iso_millis(&resolution.first_seen_at).is_none()
                || resolution.recipient_person_id.is_empty()
                || parse_iso_millis(&resolution.accepted_at).is_none()
            {
                continue;
            }
            state.terminal_resolutions.insert(key.clone(), resolution);
        }
        cap_last(&mut state.terminal_resolutions, MAX_INCIDENTS);
    }

    Ok(state)
}

/// Read the monitor state. Total: unreadable or foreign bytes reset it.
///
/// Three outcomes, not two. An absent row — every company, on its first pass —
/// is an empty state and says NOTHING: nobody monitored anything yet, and the
/// old reading of this store announced that reset as `store health-monitor was
/// unreadable`, naming a cause that had not happened. Bytes that are present
/// and will not decode still reset AND still warn, with the reason.
#[must_use]
pub fn read(ledgers: &Ledgers, ctx: &CompanyContext) -> Decoded<HealthMonitorState> {
    let Some(body) = ledgers.document_body(HealthStore::NAME) else {
        // Absence value: the same empty state a reset produces. Stated here,
        // out loud, because it is a decision and not a fallout of the polarity.
        return Decoded::absent(HealthMonitorState::empty(ctx.slug()));
    };
    match decode_fail_open::<HealthStore>(parse(ctx, body)) {
        Decoded::Value(state) => Decoded::Value(state),
        Decoded::RecoveredEmpty { warning, .. } => {
            Decoded::RecoveredEmpty { body: HealthMonitorState::empty(ctx.slug()), warning }
        }
        // `decode_fail_open` produces neither the restrictive nor the absent arm.
        other => other,
    }
}

/// Persist the state. A serialization failure drops the write, which is the
/// fail-open answer and matches the TypeScript `try`/`catch`.
pub fn write(ledgers: &mut Ledgers, state: &HealthMonitorState) {
    if let Ok(encoded) = serde_json::to_string(state) {
        ledgers.put_document(HealthStore::NAME, encoded);
    }
}

/// Drop the state entirely. Returns whether a row was present.
pub fn clear(ledgers: &mut Ledgers) -> bool {
    ledgers.remove_document(HealthStore::NAME)
}

/// The earliest instant a re-check could still matter: for every observation
/// still awaiting confirmation (`apply_cycle` requires `count >= 2` AND
/// `HEALTH_OBSERVATION_CONFIRMATION_MS` elapsed since it was first seen —
/// plan §2.7), the moment that window closes. `None` when nothing is
/// pending, so a quiet company's dynamic interval (E8-S2) rests at the
/// reactive fallback floor rather than keeping a synthetic deadline alive.
///
/// Mirrors [`crate::store::supervision::next_due_at`]: reports the raw
/// earliest deadline whether it is already past or still future — the
/// caller (`health_monitor_next_interval`, `chiefd/src/run.rs`) is what
/// decides overdue-vs-future and applies the #437 backoff guard. This
/// function is pure over already-committed state; it makes no host call and
/// runs no confirmation logic of its own.
#[must_use]
pub fn next_confirmation_deadline(state: &HealthMonitorState) -> Option<i64> {
    state
        .observations
        .values()
        .filter_map(|observation| parse_iso_millis(&observation.first_observed_at))
        .map(|first| first + HEALTH_OBSERVATION_CONFIRMATION_MS)
        .min()
}

/// The earliest confirmation deadline that is STRICTLY after `now`. The
/// #437 guard caps an overdue-backoff sleep at the next genuinely future
/// deadline, so a permanently-stuck observation's backoff can never sleep
/// through a DIFFERENT, newer one arming.
#[must_use]
pub fn next_confirmation_deadline_after(state: &HealthMonitorState, now: i64) -> Option<i64> {
    state
        .observations
        .values()
        .filter_map(|observation| parse_iso_millis(&observation.first_observed_at))
        .map(|first| first + HEALTH_OBSERVATION_CONFIRMATION_MS)
        .filter(|deadline| *deadline > now)
        .min()
}

/// A candidate produced by one collection pass, before dedup and confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentCandidate {
    /// Incident kind.
    pub kind: String,
    /// Raw detail. Redacted by [`apply_cycle`]; callers must not pre-redact,
    /// so there is exactly one place the fingerprint's input is produced.
    pub detail: String,
    /// What the fingerprint hashes when the human-readable `detail` cannot be
    /// hashed safely, because it interpolates a value that MOVES while the
    /// fault persists.
    ///
    /// Incident identity must be a function of WHAT IS WRONG, never of HOW LONG
    /// IT HAS BEEN WRONG. A `..._stale` kind embeds `${minutes}m ago` in its
    /// detail, so hashing the detail minted a fresh fingerprint every pass,
    /// dedup could never match a prior one, and the CEO was re-alerted every
    /// five minutes forever — a repeated alert is one people learn to ignore.
    /// A candidate whose detail interpolates a moving value supplies a stable
    /// identity here instead; [`apply_cycle`] hashes `identity` when present
    /// and the redacted `detail` otherwise. `supervisor_stale` has the same
    /// moving-minutes shape and deliberately gets NO identity, matching the
    /// TypeScript authority exactly.
    pub identity: Option<String>,
    /// The manager who owns the unblock.
    pub responsible_person_id: Option<String>,
    /// Content-free operator action.
    pub unblock_action: Option<String>,
    /// How many underlying items this summarizes.
    pub observed_count: Option<u64>,
    /// Oldest underlying item.
    pub oldest_at: Option<String>,
    /// The person whose own durable mailbox this incident reports as impaired.
    /// An alert routed *to* that person is delivered through the very channel
    /// it reports broken, so a later duty additionally sends an out-of-band
    /// copy. Carried through to [`HealthMonitorIncident`] now, before that
    /// consumer exists, so health.rs is touched once rather than twice.
    pub impaired_mailbox_person_id: Option<String>,
    /// Supplied by the caller for terminal incidents only: which operator
    /// runtime may accept the alert. Computed from the manifest and the
    /// supervision ledger, neither of which this store owns (M12/M15).
    pub alert_authority: Option<AlertAuthority>,
}

impl IncidentCandidate {
    /// A candidate with no optional metadata.
    #[must_use]
    pub fn new(kind: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            detail: detail.into(),
            identity: None,
            responsible_person_id: None,
            unblock_action: None,
            observed_count: None,
            oldest_at: None,
            impaired_mailbox_person_id: None,
            alert_authority: None,
        }
    }
}

/// What one pass changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleOutcome {
    /// Every incident active after this pass, fingerprint-ordered.
    pub active: Vec<HealthMonitorIncident>,
    /// The subset that was not active before.
    pub new_incidents: Vec<HealthMonitorIncident>,
    /// Fingerprints that were active before and are not now.
    pub resolved_fingerprints: Vec<String>,
}

/// A single runtime/runtime sample is often taken mid-launch. These kinds page
/// only after an independent later sample (plan §2.7).
#[must_use]
pub fn requires_confirmed_observation(kind: &str, detail: &str) -> bool {
    if matches!(
        kind,
        "runtime_activity_mismatch" | "runtime_projection_mismatch" | "runtime_session_missing"
    ) {
        return true;
    }
    if kind != "runtime_ownership_conflict" {
        return false;
    }
    let lower = detail.to_lowercase();
    lower.contains("not fully ownership-tagged") || lower.contains("server exited unexpectedly")
}

/// Verify a cached terminal resolution against its immutable marker.
///
/// The marker lives on disk and is the executor's to read (M15); the store
/// takes the decision as a function so the ported control flow — *"state is a
/// projection, never authority; a missing marker makes a cached resolution
/// active again instead of hiding work"* — is testable here and cannot be
/// skipped by the caller who supplies it.
pub trait TerminalResolutionOracle {
    /// Whether the immutable marker backing `resolution` still validates.
    fn marker_valid(&self, resolution: &TerminalResolution) -> bool;

    /// Accept `incident` now, if the operator has done so. `None` means the
    /// incident stays active.
    fn accept(&self, incident: &HealthMonitorIncident) -> Option<TerminalResolution>;
}

/// An oracle that never resolves anything — the correct behaviour when no
/// acceptance machinery is wired up, because it keeps work visible.
pub struct NeverResolves;

impl TerminalResolutionOracle for NeverResolves {
    fn marker_valid(&self, _resolution: &TerminalResolution) -> bool {
        false
    }

    fn accept(&self, _incident: &HealthMonitorIncident) -> Option<TerminalResolution> {
        None
    }
}

/// Fold one collection pass into the durable state.
///
/// Order is the ported order and is load-bearing: resolutions are released
/// before incidents are matched, confirmation gating happens before dedup, and
/// truncation happens last, so a truncated pass cannot make an incident look
/// new on the following one.
pub fn apply_cycle(
    state: &mut HealthMonitorState,
    candidates: &[IncidentCandidate],
    checked_at_millis: i64,
    oracle: &dyn TerminalResolutionOracle,
) -> CycleOutcome {
    let checked_at = iso_millis(checked_at_millis);

    // Redact once, up front: the fingerprint is a hash of the redacted detail,
    // so redacting per-use would risk two call sites disagreeing.
    let prepared: Vec<(String, IncidentCandidate)> = candidates
        .iter()
        .map(|candidate| {
            let detail = bounded_persisted_error(&candidate.detail);
            // Identity is what is wrong; the detail is how long. When a
            // candidate supplies a stable identity, hash THAT so a detail that
            // interpolates a moving value (e.g. `${minutes}m ago`) cannot churn
            // the fingerprint and re-alert forever. Identity is a static,
            // caller-supplied string and is hashed as-is, exactly as the
            // TypeScript `incidentFingerprint(candidate, detail)` does.
            let key =
                fingerprint(&candidate.kind, candidate.identity.as_deref().unwrap_or(&detail));
            (key, IncidentCandidate { detail, ..candidate.clone() })
        })
        .collect();

    // A resolution suppresses one continuously observed lifecycle, not every
    // future occurrence that hashes the same. A pass that no longer observes
    // the source closes that lifecycle and releases the cached projection.
    let observed_terminal: BTreeSet<&String> = prepared
        .iter()
        .filter(|(_, candidate)| candidate.kind == TERMINAL_SUPERVISION_HEALTH_INCIDENT_KIND)
        .map(|(key, _)| key)
        .collect();
    state.terminal_resolutions.retain(|key, _| observed_terminal.contains(key));

    let mut observed_transient: BTreeSet<String> = BTreeSet::new();
    // Fingerprint -> when an operator accepted its alert. An acknowledgement is
    // tracked, never obeyed: it records that a human SAW the alert and stops the
    // ALERT layer queuing a second card, but it must NOT drop the incident from
    // the active set. Suppressing the incident on acknowledgement is what hid
    // four still-failed fences (supervision failures went 4 -> 0 while the
    // faults persisted). The incident stays active, tagged with this timestamp,
    // and is released only when its condition stops being observed, exactly like
    // every other incident.
    let mut acknowledged_at: BTreeMap<String, String> = BTreeMap::new();
    let mut surviving: Vec<(String, IncidentCandidate)> = Vec::new();

    for (key, candidate) in prepared {
        if candidate.kind == TERMINAL_SUPERVISION_HEALTH_INCIDENT_KIND {
            if let Some(cached) = state.terminal_resolutions.get(&key) {
                if oracle.marker_valid(cached) {
                    acknowledged_at.insert(key.clone(), cached.accepted_at.clone());
                } else {
                    // State is a projection, never authority: a vanished marker
                    // makes a cached resolution active again.
                    state.terminal_resolutions.remove(&key);
                }
            }
            if !acknowledged_at.contains_key(&key) {
                // Own an owned `resolution` before touching `terminal_resolutions`,
                // so the immutable borrow of `incidents` ends first.
                let resolution = state.incidents.get(&key).and_then(|prior| oracle.accept(prior));
                if let Some(resolution) = resolution {
                    acknowledged_at.insert(key.clone(), resolution.accepted_at.clone());
                    state.terminal_resolutions.insert(key.clone(), resolution);
                }
            }
        }

        if !requires_confirmed_observation(&candidate.kind, &candidate.detail) {
            surviving.push((key, candidate));
            continue;
        }

        observed_transient.insert(key.clone());
        let prior = state.observations.get(&key);
        let observation = HealthMonitorObservation {
            first_observed_at: prior
                .map_or_else(|| checked_at.clone(), |prior| prior.first_observed_at.clone()),
            last_observed_at: checked_at.clone(),
            count: prior.map_or(0, |prior| prior.count).saturating_add(1),
        };
        let confirmed = observation.count >= 2
            && parse_iso_millis(&observation.first_observed_at).is_some_and(|first| {
                checked_at_millis - first >= HEALTH_OBSERVATION_CONFIRMATION_MS
            });
        state.observations.insert(key.clone(), observation);
        if confirmed {
            surviving.push((key, candidate));
        }
    }
    state.observations.retain(|key, _| observed_transient.contains(key));

    let mut current: BTreeMap<String, HealthMonitorIncident> = BTreeMap::new();
    for (key, candidate) in surviving {
        let prior = current.get(&key).or_else(|| state.incidents.get(&key));
        let next = HealthMonitorIncident {
            fingerprint: key.clone(),
            kind: candidate.kind.clone(),
            detail: candidate.detail.clone(),
            first_seen_at: prior
                .map_or_else(|| checked_at.clone(), |prior| prior.first_seen_at.clone()),
            last_seen_at: checked_at.clone(),
            count: prior.map_or(0, |prior| prior.count).saturating_add(1),
            responsible_person_id: candidate.responsible_person_id.clone(),
            unblock_action: candidate.unblock_action.clone(),
            observed_count: candidate.observed_count,
            oldest_at: candidate.oldest_at.clone(),
            acknowledged_at: acknowledged_at.get(&key).cloned(),
            impaired_mailbox_person_id: candidate.impaired_mailbox_person_id.clone(),
            alert_recipient_person_id: candidate
                .alert_authority
                .as_ref()
                .filter(|_| candidate.kind == TERMINAL_SUPERVISION_HEALTH_INCIDENT_KIND)
                .map(|authority| authority.recipient_person_id.clone()),
        };
        current.insert(key, next);
    }

    let active: Vec<HealthMonitorIncident> = current.values().cloned().collect();
    let new_incidents: Vec<HealthMonitorIncident> = active
        .iter()
        .filter(|incident| !state.incidents.contains_key(&incident.fingerprint))
        .cloned()
        .collect();
    let resolved_fingerprints: Vec<String> =
        state.incidents.keys().filter(|key| !current.contains_key(*key)).cloned().collect();

    state.incidents = current;
    cap_last(&mut state.incidents, MAX_INCIDENTS);
    cap_last(&mut state.terminal_resolutions, MAX_INCIDENTS);
    state.last_run_at = Some(checked_at);

    CycleOutcome { active, new_incidents, resolved_fingerprints }
}

/// Per-file byte budget for one pass over `monitored_files` files.
///
/// The total across all files is capped at [`HEALTH_LOG_TOTAL_READ_LIMIT`]; a
/// company with many rotated logs reads less of each rather than reading more
/// in total. Never zero: a budget of zero would stall a cursor forever.
#[must_use]
pub fn per_log_read_limit(monitored_files: usize) -> u64 {
    let files = u64::try_from(monitored_files.max(1)).unwrap_or(1);
    (HEALTH_LOG_TOTAL_READ_LIMIT / files).max(1)
}

/// The arithmetic half of a bounded appended-log read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedReadPlan {
    /// Byte offset to read from.
    pub read_start: u64,
    /// How many bytes to read.
    pub bytes: u64,
    /// True when the file was rotated or truncated under the cursor.
    pub reset: bool,
    /// Appended bytes this pass will not read.
    pub dropped_bytes: u64,
    /// True when the read starts mid-line and the first partial line must be
    /// discarded.
    pub skip_partial_first_line: bool,
    /// The cursor to store afterwards. Always EOF, so one pass never has to
    /// catch up twice.
    pub cursor: HealthLogCursor,
}

/// Plan a bounded read of the newly appended bytes of a log file.
///
/// Pure: the caller supplies `device`/`inode`/`size` from its own `fstat`, so
/// the tricky part (rotation detection, the tail window, the partial first
/// line, and the fact that the stored cursor is EOF rather than the end of what
/// was read) is unit-testable without a filesystem.
#[must_use]
pub fn plan_bounded_read(
    previous: Option<&HealthLogCursor>,
    device: &str,
    inode: &str,
    size: u64,
    maximum_bytes: u64,
) -> BoundedReadPlan {
    let same_file = previous.is_some_and(|cursor| cursor.device == device && cursor.inode == inode);
    let reset = previous.is_some_and(|cursor| !same_file || size < cursor.offset);
    let start = if same_file && !reset { previous.map_or(0, |cursor| cursor.offset) } else { 0 };
    let unread = size.saturating_sub(start);
    let bytes = unread.min(maximum_bytes);
    let read_start = start.max(size.saturating_sub(bytes));
    BoundedReadPlan {
        read_start,
        bytes,
        reset,
        dropped_bytes: unread.saturating_sub(bytes),
        skip_partial_first_line: read_start > start,
        cursor: HealthLogCursor {
            device: device.to_string(),
            inode: inode.to_string(),
            offset: size,
        },
    }
}

/// Turn the bytes a [`BoundedReadPlan`] selected into log lines.
#[must_use]
pub fn bounded_read_lines(text: &str, plan: &BoundedReadPlan) -> Vec<String> {
    let body = if plan.skip_partial_first_line {
        match text.find('\n') {
            Some(index) => &text[index + 1..],
            None => "",
        }
    } else {
        text
    };
    let lines: Vec<String> =
        body.split('\n').filter(|line| !line.is_empty()).map(ToString::to_string).collect();
    let skip = lines.len().saturating_sub(MAX_LOG_LINES_PER_READ);
    lines.into_iter().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::WallMillis;

    const EPOCH: i64 = 1_784_116_800_000; // 2026-07-15T12:00:00.000Z

    fn ctx() -> CompanyContext {
        CompanyContext::new("cobalt", "chief", ["chief"].map(String::from))
    }

    fn ledgers() -> Ledgers {
        Ledgers::empty(WallMillis(EPOCH))
    }

    fn transient() -> IncidentCandidate {
        IncidentCandidate::new("runtime_session_missing", "expected active people have no session")
    }

    #[test]
    fn unreadable_bytes_reset_the_state_and_warn_rather_than_erroring() {
        let mut l = ledgers();
        l.put_document(HealthStore::NAME, "}{ garbage");
        let (state, warning) = read(&l, &ctx()).into_parts();
        assert_eq!(state, HealthMonitorState::empty("cobalt"));
        assert!(warning.is_some_and(|w| w.contains("health")));
    }

    #[test]
    fn a_state_belonging_to_another_company_resets_instead_of_being_adopted() {
        let mut l = ledgers();
        let foreign = HealthMonitorState::empty("someone-else");
        write(&mut l, &foreign);
        let (state, warning) = read(&l, &ctx()).into_parts();
        assert_eq!(state.organization, "cobalt");
        assert!(state.incidents.is_empty());
        assert!(warning.is_some());
    }

    #[test]
    fn one_malformed_entry_does_not_poison_the_rest_of_the_state() {
        let mut l = ledgers();
        l.put_document(
            HealthStore::NAME,
            r#"{"version":1,"organization":"cobalt","cursors":{
                 "/a.log":{"device":"1","inode":"2","offset":10},
                 "/b.log":{"device":"1"}
               },"observations":{},"incidents":{},"terminalResolutions":{}}"#,
        );
        let (state, warning) = read(&l, &ctx()).into_parts();
        assert_eq!(warning, None, "a state with one bad row is readable");
        assert_eq!(state.cursors.len(), 1);
        assert!(state.cursors.contains_key("/a.log"));
    }

    #[test]
    fn a_detail_is_redacted_before_it_becomes_part_of_the_fingerprint() {
        let mut state = HealthMonitorState::empty("cobalt");
        let outcome = apply_cycle(
            &mut state,
            &[IncidentCandidate::new("supervisor_error", "poll failed api_key=abcdef1234567")],
            EPOCH,
            &NeverResolves,
        );
        let incident = &outcome.active[0];
        assert_eq!(incident.detail, "poll failed api_key=[redacted]");
        assert!(!incident.detail.contains("abcdef1234567"));
        assert_eq!(incident.fingerprint, fingerprint("supervisor_error", &incident.detail));
    }

    #[test]
    fn a_transient_observation_needs_a_second_sample_fifteen_seconds_later() {
        let mut state = HealthMonitorState::empty("cobalt");

        let first = apply_cycle(&mut state, &[transient()], EPOCH, &NeverResolves);
        assert!(first.active.is_empty(), "one sample never pages");
        assert_eq!(state.observations.len(), 1);

        // A second sample too soon is still not enough: the rule is an
        // *independent* observation, not merely a second one.
        let early = apply_cycle(&mut state, &[transient()], EPOCH + 14_999, &NeverResolves);
        assert!(early.active.is_empty(), "14.999s is not 15s");

        let confirmed = apply_cycle(&mut state, &[transient()], EPOCH + 15_000, &NeverResolves);
        assert_eq!(confirmed.active.len(), 1);
        assert_eq!(confirmed.new_incidents.len(), 1);
    }

    #[test]
    fn an_unconfirmed_observation_is_forgotten_once_it_stops_being_seen() {
        let mut state = HealthMonitorState::empty("cobalt");
        apply_cycle(&mut state, &[transient()], EPOCH, &NeverResolves);
        assert_eq!(state.observations.len(), 1);
        apply_cycle(&mut state, &[], EPOCH + 60_000, &NeverResolves);
        assert!(state.observations.is_empty(), "a streak that broke starts over");
    }

    #[test]
    fn a_repeated_incident_keeps_its_first_seen_at_and_counts_up() {
        let mut state = HealthMonitorState::empty("cobalt");
        let candidate = IncidentCandidate::new("supervisor_not_running", "stopped");
        apply_cycle(&mut state, std::slice::from_ref(&candidate), EPOCH, &NeverResolves);
        let second = apply_cycle(&mut state, &[candidate], EPOCH + 300_000, &NeverResolves);
        assert_eq!(second.active[0].count, 2);
        assert_eq!(second.active[0].first_seen_at, "2026-07-15T12:00:00.000Z");
        assert_eq!(second.active[0].last_seen_at, "2026-07-15T12:05:00.000Z");
        assert!(second.new_incidents.is_empty(), "a repeat is not a new page");
    }

    #[test]
    fn an_incident_that_stops_being_observed_is_reported_resolved_exactly_once() {
        let mut state = HealthMonitorState::empty("cobalt");
        let candidate = IncidentCandidate::new("supervisor_error", "boom");
        apply_cycle(&mut state, &[candidate], EPOCH, &NeverResolves);
        let cleared = apply_cycle(&mut state, &[], EPOCH + 1_000, &NeverResolves);
        assert_eq!(cleared.resolved_fingerprints.len(), 1);
        let again = apply_cycle(&mut state, &[], EPOCH + 2_000, &NeverResolves);
        assert!(again.resolved_fingerprints.is_empty());
    }

    #[test]
    fn active_incidents_are_truncated_to_two_hundred() {
        let mut state = HealthMonitorState::empty("cobalt");
        let candidates: Vec<IncidentCandidate> = (0..250)
            .map(|index| IncidentCandidate::new("supervisor_error", format!("failure {index}")))
            .collect();
        let outcome = apply_cycle(&mut state, &candidates, EPOCH, &NeverResolves);
        assert_eq!(outcome.active.len(), 250, "the report is complete");
        assert_eq!(state.incidents.len(), MAX_INCIDENTS, "the durable state is bounded");
    }

    #[test]
    fn an_accepted_terminal_incident_stays_active_with_an_acknowledgement_until_it_clears() {
        struct AcceptsOnce;
        impl TerminalResolutionOracle for AcceptsOnce {
            fn marker_valid(&self, _resolution: &TerminalResolution) -> bool {
                true
            }
            fn accept(&self, incident: &HealthMonitorIncident) -> Option<TerminalResolution> {
                Some(TerminalResolution {
                    fingerprint: incident.fingerprint.clone(),
                    kind: incident.kind.clone(),
                    first_seen_at: incident.first_seen_at.clone(),
                    recipient_person_id: "chief".to_string(),
                    accepted_at: incident.last_seen_at.clone(),
                })
            }
        }

        let mut state = HealthMonitorState::empty("cobalt");
        let candidate =
            IncidentCandidate::new(TERMINAL_SUPERVISION_HEALTH_INCIDENT_KIND, "wake failed");
        // First pass: the incident exists; there is no prior to accept yet.
        let first = apply_cycle(&mut state, std::slice::from_ref(&candidate), EPOCH, &AcceptsOnce);
        assert_eq!(first.active.len(), 1);
        assert_eq!(first.active[0].acknowledged_at, None);

        // Second pass: the operator accepts. Acceptance records that a human SAW
        // the alert — the fence is still failed, so the incident MUST stay active
        // and carry when it was seen. Dropping it here is the bug that hid four
        // failed fences ("supervision failures 4 -> 0" while the faults persisted).
        let accepted =
            apply_cycle(&mut state, std::slice::from_ref(&candidate), EPOCH + 1_000, &AcceptsOnce);
        assert_eq!(accepted.active.len(), 1, "an acknowledged fault is still a fault");
        // The oracle stamps acceptance from the incident it accepted — here the
        // prior (first-pass) record, whose last_seen_at is the EPOCH sighting.
        assert_eq!(
            accepted.active[0].acknowledged_at.as_deref(),
            Some("2026-07-15T12:00:00.000Z"),
            "the incident carries when it was acknowledged",
        );
        assert!(accepted.resolved_fingerprints.is_empty(), "acknowledgement is not resolution",);
        assert_eq!(state.terminal_resolutions.len(), 1);

        // A repeat pass keeps it active and acknowledged — the alert layer dedups
        // the card, the monitor never suppresses the incident.
        let repeated =
            apply_cycle(&mut state, std::slice::from_ref(&candidate), EPOCH + 2_000, &AcceptsOnce);
        assert_eq!(repeated.active.len(), 1);
        assert!(repeated.active[0].acknowledged_at.is_some());
        assert!(repeated.resolved_fingerprints.is_empty());

        // It stops being observed: NOW the lifecycle closes and the projection
        // goes — removed by the condition clearing, never by acceptance alone.
        let cleared = apply_cycle(&mut state, &[], EPOCH + 3_000, &AcceptsOnce);
        assert_eq!(cleared.resolved_fingerprints.len(), 1);
        assert!(state.terminal_resolutions.is_empty());

        // A later, identical occurrence is a NEW lifecycle, not a suppressed one.
        let reoccurred = apply_cycle(&mut state, &[candidate], EPOCH + 4_000, &AcceptsOnce);
        assert_eq!(reoccurred.active.len(), 1);
        assert_eq!(reoccurred.active[0].first_seen_at, "2026-07-15T12:00:04.000Z");
        assert_eq!(reoccurred.active[0].acknowledged_at, None, "a fresh lifecycle is unseen");
    }

    #[test]
    fn a_stale_incident_keeps_one_identity_as_its_detail_ages_so_it_alerts_once() {
        // Ported from the TypeScript "a staleness incident keeps ONE
        // identity as it ages, so the CEO is alerted once". The same fault seen
        // at three ages: its detail is EXPECTED to move (an operator wants to
        // read how long it has been stale), but its identity must not, or dedup
        // can never match a prior occurrence and the alert churns every pass.
        let mut state = HealthMonitorState::empty("cobalt");
        let mut fingerprints = BTreeSet::new();
        let mut details = BTreeSet::new();
        for (index, minutes) in [5_i64, 25, 90].into_iter().enumerate() {
            let candidate = IncidentCandidate {
                identity: Some(
                    "last successful poll is older than the stale threshold".to_string(),
                ),
                ..IncidentCandidate::new(
                    "supervisor_stale",
                    format!("last successful poll was {minutes}m ago"),
                )
            };
            let at = EPOCH + i64::try_from(index).unwrap() * 300_000;
            let outcome = apply_cycle(&mut state, &[candidate], at, &NeverResolves);
            let incident = outcome
                .active
                .iter()
                .find(|incident| incident.kind == "supervisor_stale")
                .expect("a supervisor_stale incident every pass");
            fingerprints.insert(incident.fingerprint.clone());
            details.insert(incident.detail.clone());
            assert!(
                outcome.new_incidents.is_empty() || index == 0,
                "only the first sighting is a new page; later ages dedup",
            );
        }
        // One identity across all three passes...
        assert_eq!(fingerprints.len(), 1, "a moving detail must not churn the fingerprint");
        // ...even though the human-readable detail still ages.
        assert!(details.len() > 1, "the detail still tells the operator how long");
    }

    #[test]
    fn without_an_identity_a_moving_detail_churns_the_fingerprint() {
        // The contrast that proves identity is load-bearing: the same candidate
        // shape WITHOUT an identity mints a fresh incident on every age — the
        // exact re-alert-every-five-minutes bug the identity field fixes.
        let mut state = HealthMonitorState::empty("cobalt");
        let mut fingerprints = BTreeSet::new();
        for (index, minutes) in [5_i64, 25, 90].into_iter().enumerate() {
            let candidate = IncidentCandidate::new(
                "supervisor_stale",
                format!("last successful poll was {minutes}m ago"),
            );
            let at = EPOCH + i64::try_from(index).unwrap() * 300_000;
            let outcome = apply_cycle(&mut state, &[candidate], at, &NeverResolves);
            fingerprints.insert(outcome.active[0].fingerprint.clone());
        }
        assert_eq!(fingerprints.len(), 3, "no identity ⇒ every age is a different incident");
    }

    #[test]
    fn the_impaired_mailbox_person_id_flows_from_candidate_to_incident_and_round_trips() {
        let mut state = HealthMonitorState::empty("cobalt");
        let candidate = IncidentCandidate {
            impaired_mailbox_person_id: Some("chief".to_string()),
            ..IncidentCandidate::new("mailbox_delivery_stale", "delivery to 'chief' unaccepted")
        };
        let outcome = apply_cycle(&mut state, &[candidate], EPOCH, &NeverResolves);
        assert_eq!(
            outcome.active[0].impaired_mailbox_person_id.as_deref(),
            Some("chief"),
            "the routing hint survives the fold onto the incident",
        );
        // And it survives serialization to and from the durable document.
        let mut l = ledgers();
        write(&mut l, &state);
        let (reread, _) = read(&l, &ctx()).into_parts();
        assert_eq!(
            reread.incidents.values().next().and_then(|i| i.impaired_mailbox_person_id.as_deref()),
            Some("chief"),
        );
    }

    #[test]
    fn a_cached_resolution_whose_marker_vanished_makes_the_incident_active_again() {
        struct MarkerGone;
        impl TerminalResolutionOracle for MarkerGone {
            fn marker_valid(&self, _resolution: &TerminalResolution) -> bool {
                false
            }
            fn accept(&self, _incident: &HealthMonitorIncident) -> Option<TerminalResolution> {
                None
            }
        }

        let candidate =
            IncidentCandidate::new(TERMINAL_SUPERVISION_HEALTH_INCIDENT_KIND, "wake failed");
        let detail = bounded_persisted_error(&candidate.detail);
        let key = fingerprint(&candidate.kind, &detail);

        let mut state = HealthMonitorState::empty("cobalt");
        state.terminal_resolutions.insert(
            key.clone(),
            TerminalResolution {
                fingerprint: key.clone(),
                kind: TERMINAL_SUPERVISION_HEALTH_INCIDENT_KIND.to_string(),
                first_seen_at: "2026-07-15T12:00:00.000Z".to_string(),
                recipient_person_id: "chief".to_string(),
                accepted_at: "2026-07-15T12:00:00.000Z".to_string(),
            },
        );

        let outcome = apply_cycle(&mut state, &[candidate], EPOCH, &MarkerGone);
        assert_eq!(outcome.active.len(), 1, "state is a projection, never authority");
        assert!(state.terminal_resolutions.is_empty());
    }

    #[test]
    fn log_read_budgets_are_sixty_four_kilobytes_per_file_and_two_fifty_six_in_total() {
        assert_eq!(HEALTH_LOG_READ_LIMIT, 65_536);
        assert_eq!(per_log_read_limit(1), 262_144);
        assert_eq!(per_log_read_limit(4), 65_536);
        assert_eq!(per_log_read_limit(1_000_000), 1, "a budget is never zero");
        assert_eq!(per_log_read_limit(0), 262_144);
    }

    #[test]
    fn a_rotated_log_resets_the_cursor_and_reads_from_the_start() {
        let previous =
            HealthLogCursor { device: "1".to_string(), inode: "2".to_string(), offset: 5_000 };
        let plan = plan_bounded_read(Some(&previous), "1", "9", 300, 64);
        assert!(plan.reset, "a different inode is a rotation");
        assert_eq!(plan.dropped_bytes, 300 - 64);
        assert_eq!(plan.cursor.offset, 300, "the stored cursor is always EOF");
        assert_eq!(plan.read_start, 300 - 64, "the tail is what matters");
        assert!(plan.skip_partial_first_line);
    }

    #[test]
    fn a_truncated_log_is_a_reset_even_with_the_same_inode() {
        let previous =
            HealthLogCursor { device: "1".to_string(), inode: "2".to_string(), offset: 900 };
        let plan = plan_bounded_read(Some(&previous), "1", "2", 100, 64);
        assert!(plan.reset);
    }

    #[test]
    fn an_unchanged_log_yields_no_bytes_and_no_partial_line() {
        let previous =
            HealthLogCursor { device: "1".to_string(), inode: "2".to_string(), offset: 500 };
        let plan = plan_bounded_read(Some(&previous), "1", "2", 500, 64);
        assert_eq!(plan.bytes, 0);
        assert_eq!(plan.dropped_bytes, 0);
        assert!(!plan.reset);
        assert!(!plan.skip_partial_first_line);
    }

    #[test]
    fn a_read_that_starts_mid_line_discards_that_line() {
        let plan = BoundedReadPlan {
            read_start: 10,
            bytes: 10,
            reset: false,
            dropped_bytes: 0,
            skip_partial_first_line: true,
            cursor: HealthLogCursor { device: "1".to_string(), inode: "2".to_string(), offset: 20 },
        };
        assert_eq!(bounded_read_lines("f a line\nwhole line\n", &plan), vec!["whole line"]);
        assert!(bounded_read_lines("no newline at all", &plan).is_empty());
    }

    #[test]
    fn a_bounded_read_yields_at_most_two_hundred_lines_and_keeps_the_newest() {
        let plan = BoundedReadPlan {
            read_start: 0,
            bytes: 0,
            reset: false,
            dropped_bytes: 0,
            skip_partial_first_line: false,
            cursor: HealthLogCursor { device: "1".to_string(), inode: "2".to_string(), offset: 0 },
        };
        let text = (0..500).map(|i| format!("line {i}\n")).collect::<String>();
        let lines = bounded_read_lines(&text, &plan);
        assert_eq!(lines.len(), MAX_LOG_LINES_PER_READ);
        assert_eq!(lines.last().map(String::as_str), Some("line 499"));
    }

    #[test]
    fn state_survives_a_round_trip_through_the_ledger() {
        let mut l = ledgers();
        let mut state = HealthMonitorState::empty("cobalt");
        apply_cycle(
            &mut state,
            &[IncidentCandidate::new("supervisor_error", "boom")],
            EPOCH,
            &NeverResolves,
        );
        write(&mut l, &state);
        let (reread, warning) = read(&l, &ctx()).into_parts();
        assert_eq!(warning, None);
        assert_eq!(reread, state);
        assert!(clear(&mut l));
        assert!(!clear(&mut l));
    }
}
