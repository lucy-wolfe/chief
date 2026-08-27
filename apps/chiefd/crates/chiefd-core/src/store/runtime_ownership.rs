//! Who owns a company's runtime runtime — the pure decision half of
//! `org-runtime-ownership.ts`.
//!
//! The durable record itself is [`RuntimeOwner`]
//! (`crate::store::runtime_owner_rows`); this module owns only the *rules*
//! over it: what a valid record looks like, when a second socket is allowed to
//! take a company over from the socket that recorded the claim, what the
//! claimed record becomes, and when a release is refused. Every function here
//! is a total function of values the caller already read, so the whole
//! claim/release decision runs inside one `BEGIN IMMEDIATE` transaction
//! (Mandate 4) with no I/O of its own.
//!
//! The one observation the decision needs but cannot make itself is a
//! parameter, not a call: `prior_projection_exists` — whether the previously
//! recorded socket may still project a live runtime session (a `chiefd_host`
//! observation).
//!
//! # DELETED BY OMISSION: the live-supervisor probe
//!
//! There used to be a second such parameter, `supervisor`, and it fed two
//! refusals. It described a SECOND OS PROCESS — the detached org-supervisor,
//! proven alive by `kill(pid, 0)` and a starttime match — which #825 retired
//! and `5681617a4` deleted the writer for. See [`audit_ownership`] for why it
//! is not re-sourced from the one daemon.
//!
//! # DELETED BY OMISSION: `acquireOrganizationRuntimeLock`
//!
//! The TypeScript module also exported `acquireOrganizationRuntimeLock`, a
//! crash-safe **file lock** at `<org>/state/.runtime.lock` guarding lifecycle
//! commands. It has no counterpart here and must never get one. Mandate 4
//! bans locks outright: correctness comes from a single writer plus one
//! `BEGIN IMMEDIATE` transaction, over the one daemon beacond admitted for
//! this company. A second, file-based mutual exclusion mechanism over the
//! same resource would not add safety — it would add a way for the two to
//! disagree, plus a stale lock file nobody can attribute after a crash.
//!
//! The `runtime_writer_lease` that once stood here said the same thing about
//! itself and was deleted for the same reason (see `crate::actor`): a second
//! mutual-exclusion mechanism over a resource that already has one is not
//! defence in depth, it is a disagreement waiting to be observed.
//!
//! # DELETED BY TYPE: the status-domain refusal
//!
//! `validateOwnership` also threw `"Runtime ownership status must be active or
//! released"`. [`RuntimeOwnerStatus`] is a two-variant enum, so a third status
//! is unrepresentable and the check has nothing left to reject. The
//! deserialization boundary rejects an unknown label; there is no runtime
//! branch to port.

use crate::error::Refusal;
use crate::store::organization::OrganizationManifest;
use crate::store::runtime_owner_rows::{RuntimeOwner, RuntimeOwnerStatus};

/// The recorded ownership does not describe this company, or an active record
/// is missing its socket.
pub const RUNTIME_OWNERSHIP_INVALID: &str = "runtime-ownership-invalid";
/// A claim was attempted without an explicit runtime socket.
pub const RUNTIME_OWNERSHIP_IDENTITY_REQUIRED: &str = "runtime-ownership-identity-required";
/// The previously recorded socket still projects a live runtime session.
pub const RUNTIME_OWNERSHIP_PROJECTION_LIVE: &str = "runtime-ownership-projection-live";
/// A release was attempted from a socket that does not own the runtime.
pub const RUNTIME_OWNERSHIP_RELEASE_FOREIGN: &str = "runtime-ownership-release-foreign";

/// The outcome of auditing an ownership claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipVerdict {
    /// Nothing to take over: the company is released, or the requesting socket
    /// already owns it.
    Unchanged,
    /// The recorded socket is provably dead and the requesting socket may take
    /// the company over.
    Takeover {
        /// The socket the claim is being taken from.
        previous_socket_name: String,
    },
}

impl OwnershipVerdict {
    /// Whether this verdict takes the company from another socket.
    #[must_use]
    pub const fn is_takeover(&self) -> bool {
        matches!(self, Self::Takeover { .. })
    }
}

/// The decided initial record for a company that has never claimed a runtime:
/// released, owned by nobody.
///
/// Absence of a stored row resolves to exactly this — not to a refusal. A
/// company that never claimed a runtime *is* released, and even a lost row is
/// safe as released because a claim still has to prove no live projection via
/// [`audit_ownership`] before any takeover.
#[must_use]
pub fn initial_runtime_ownership(manifest: &OrganizationManifest) -> RuntimeOwner {
    RuntimeOwner {
        version: 1,
        organization: manifest.slug.clone(),
        status: RuntimeOwnerStatus::Released,
        socket_name: None,
        claimed_at: None,
        validated_at: None,
        released_at: None,
        extra: Default::default(),
    }
}

/// Check a stored ownership record against the manifest it claims to describe.
///
/// # Errors
/// [`RUNTIME_OWNERSHIP_INVALID`] when the version or company does not match the
/// manifest, or when an `active` record carries no non-blank socket.
///
/// # DELETED (AC6): the session check
///
/// This also compared `owner.session_name` against `manifest.runtime_session`.
/// Both sides were `org-<slug>` derived from the SAME slug — the stored
/// `session` column was written from the manifest and reconstructed from the
/// company slug — so the comparison could not fail for any record this store
/// had ever written. It was a self-comparison dressed as a validation, and it
/// is deleted with the column and the manifest field it compared.
pub fn validate_ownership(
    manifest: &OrganizationManifest,
    owner: &RuntimeOwner,
) -> Result<(), Refusal> {
    if owner.version != 1 || owner.organization != manifest.slug {
        return Err(Refusal::new(
            RUNTIME_OWNERSHIP_INVALID,
            format!("Runtime ownership does not match organization '{}'", manifest.slug),
        ));
    }
    if owner.status == RuntimeOwnerStatus::Active && !has_socket(owner) {
        return Err(Refusal::new(
            RUNTIME_OWNERSHIP_INVALID,
            format!(
                "Active runtime ownership for '{}' is missing its explicit runtime socket",
                manifest.slug
            ),
        ));
    }
    Ok(())
}

/// Whether an ownership record carries a usable socket name.
fn has_socket(owner: &RuntimeOwner) -> bool {
    owner.socket_name.as_ref().is_some_and(|socket| !socket.trim().is_empty())
}

/// Decide whether `socket_name` may claim this company's runtime.
///
/// This is `auditOrganizationRuntimeOwnership`'s decision half. The one live
/// observation it cannot make for itself is an input: `prior_projection_exists`,
/// whether the recorded socket may still project a live runtime.
///
/// # There were two supervisor-gated refusals here, and they could never fire
///
/// `supervised-elsewhere` and `runtime-ownership-conflict` took a third input:
/// what a live supervisor PROCESS claimed. That claim came from
/// `supervisor_process_state`, written only by `org-supervisor-state.ts`, whose
/// `supervisorProcessIsLive` proved a second OS process alive with
/// `kill(pid, 0)` plus a `/proc` starttime match. #825 retired that process
/// (`809c402a5`) and `5681617a4` deleted the writer, so the input was `None` on
/// every call and both refusals were dead code guarding a live safety decision.
///
/// They are deleted rather than re-sourced. The only writer available in the
/// one-daemon model is the daemon recording its own identity as it claims — and
/// then `supervisor.socket_name` is a copy of `owner.socket_name`, so
/// `supervised-elsewhere` would fire on EVERY takeover of an active claim
/// (destroying the legitimate provably-dead-owner path below) and `conflict`
/// could never fire at all. Two facts that were only independent because they
/// described two processes cannot be made independent again by writing one of
/// them twice.
///
/// Checks run in order — explicit identity, record validity, the trivially-
/// unchanged case, then the live projection — because each later check assumes
/// the earlier ones passed.
///
/// # There were two supervisor-gated refusals here, and they could never fire
///
/// `supervised-elsewhere` and `runtime-ownership-conflict` took a third input:
/// what a live supervisor PROCESS claimed. That claim came from
/// `supervisor_process_state`, written only by `org-supervisor-state.ts`, whose
/// `supervisorProcessIsLive` proved a second OS process alive with
/// `kill(pid, 0)` plus a `/proc` starttime match. #825 retired that process
/// (`809c402a5`) and `5681617a4` deleted the writer, so the input was `None` on
/// every call and both refusals were dead code guarding a live safety decision.
///
/// They are deleted rather than re-sourced. The only writer available in the
/// one-daemon model is the daemon recording its own identity as it claims — and
/// then `supervisor.socket_name` is a copy of `owner.socket_name`, so
/// `supervised-elsewhere` would fire on EVERY takeover of an active claim
/// (destroying the legitimate provably-dead-owner path below) and `conflict`
/// could never fire at all. Two facts that were only independent because they
/// described two processes cannot be made independent again by writing one of
/// them twice.
///
/// The gate is `prior_projection_exists`, which names the actual hazard —
/// another runtime server still projecting this company.
///
/// # AC6: `socket_name` is an OPAQUE actuator identity
///
/// chiefd stores this string, compares it for equality, and never parses it.
/// It is the operator client's own handle for where it projects the company —
/// today a tmux socket name, and the client is the only party that interprets
/// it as one. What chiefd needs to identify an owner is exactly "a durable
/// string that distinguishes one actuator from another", which is why the
/// session half of the identity is gone: it was `org-<slug>`, derived from the
/// same slug on both sides of every comparison it took part in, and it
/// distinguished nothing.
///
/// # Errors
/// [`RUNTIME_OWNERSHIP_IDENTITY_REQUIRED`], [`RUNTIME_OWNERSHIP_INVALID`] or
/// [`RUNTIME_OWNERSHIP_PROJECTION_LIVE`].
pub fn audit_ownership(
    manifest: &OrganizationManifest,
    owner: &RuntimeOwner,
    socket_name: &str,
    prior_projection_exists: bool,
) -> Result<OwnershipVerdict, Refusal> {
    if socket_name.trim().is_empty() {
        return Err(Refusal::new(
            RUNTIME_OWNERSHIP_IDENTITY_REQUIRED,
            "An explicit runtime socket name is required",
        ));
    }
    // The TS loader validated on every read; the audit therefore never saw an
    // invalid record. Validating here keeps that contract explicit and gives
    // the socket lookup below a proof rather than an unwrap.
    validate_ownership(manifest, owner)?;

    let Some(previous_socket) = owner.socket_name.as_deref() else {
        // Unreachable for an active record (validate_ownership proved a socket)
        // and correct for a released one: nothing to take over.
        return Ok(OwnershipVerdict::Unchanged);
    };
    if owner.status != RuntimeOwnerStatus::Active || previous_socket == socket_name {
        return Ok(OwnershipVerdict::Unchanged);
    }

    if prior_projection_exists {
        return Err(Refusal::new(
            RUNTIME_OWNERSHIP_PROJECTION_LIVE,
            format!(
                "Organization '{}' is already running on runtime socket '{previous_socket}'; refusing '{socket_name}'",
                manifest.slug
            ),
        ));
    }
    Ok(OwnershipVerdict::Takeover { previous_socket_name: previous_socket.to_string() })
}

/// The record an accepted claim writes.
///
/// `claimedAt` carry-forward rule, ported verbatim: a takeover or a claim over
/// a *released* company starts a new claim and stamps `now`; re-validating a
/// claim this socket already holds keeps the original `claimedAt` (falling back
/// to `now` only when the prior record never carried one), so "how long has
/// this runtime been up" survives a re-validation. `validatedAt` is always
/// `now` — it is the liveness stamp, not the claim stamp.
#[must_use]
pub fn claimed_ownership(
    manifest: &OrganizationManifest,
    verdict: &OwnershipVerdict,
    previous: &RuntimeOwner,
    socket_name: &str,
    now: &str,
) -> RuntimeOwner {
    let starts_a_new_claim =
        verdict.is_takeover() || previous.status == RuntimeOwnerStatus::Released;
    let claimed_at = if starts_a_new_claim {
        now.to_string()
    } else {
        previous.claimed_at.clone().unwrap_or_else(|| now.to_string())
    };
    RuntimeOwner {
        version: 1,
        organization: manifest.slug.clone(),
        status: RuntimeOwnerStatus::Active,
        socket_name: Some(socket_name.to_string()),
        claimed_at: Some(claimed_at),
        validated_at: Some(now.to_string()),
        // A claim never carries a release stamp; the TS record omitted the key.
        released_at: None,
        extra: Default::default(),
    }
}

/// The record a release writes.
///
/// # Errors
/// [`RUNTIME_OWNERSHIP_RELEASE_FOREIGN`] when the runtime is actively owned by
/// a different socket — releasing someone else's runtime would hand the company
/// to a third socket while the real owner is still projecting it.
pub fn released_ownership(
    manifest: &OrganizationManifest,
    current: &RuntimeOwner,
    socket_name: &str,
    now: &str,
) -> Result<RuntimeOwner, Refusal> {
    if current.status == RuntimeOwnerStatus::Active
        && current.socket_name.as_deref() != Some(socket_name)
    {
        let owner_socket = current.socket_name.as_deref().unwrap_or_default();
        return Err(Refusal::new(
            RUNTIME_OWNERSHIP_RELEASE_FOREIGN,
            format!(
                "Refusing to release '{}' runtime owned by runtime socket '{owner_socket}' from '{socket_name}'",
                manifest.slug
            ),
        ));
    }
    Ok(RuntimeOwner {
        version: 1,
        organization: manifest.slug.clone(),
        status: RuntimeOwnerStatus::Released,
        socket_name: Some(socket_name.to_string()),
        // The claim stamp is history and survives the release.
        claimed_at: current.claimed_at.clone(),
        validated_at: Some(now.to_string()),
        released_at: Some(now.to_string()),
        extra: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::organization::{
        DepartmentRecord, OrganizationPolicy, UnitKind, UnitState, ORGANIZATION_SCHEMA_VERSION,
        ROOT_DEPARTMENT_ID,
    };
    use std::collections::BTreeMap;

    const AT: &str = "2026-08-01T00:00:00.000Z";
    const LATER: &str = "2026-08-02T00:00:00.000Z";

    fn manifest() -> OrganizationManifest {
        let mut departments = BTreeMap::new();
        departments.insert(
            ROOT_DEPARTMENT_ID.to_string(),
            DepartmentRecord {
                id: ROOT_DEPARTMENT_ID.to_string(),
                name: "Executive".to_string(),
                purpose: "run the company".to_string(),
                kind: Some(UnitKind::Company),
                transient: None,
                parent_department_id: None,
                head_person_id: "ada".to_string(),
                state: UnitState::Active,
                created_at: AT.to_string(),
                extra: Default::default(),
            },
        );
        OrganizationManifest {
            schema_version: ORGANIZATION_SCHEMA_VERSION,
            kind: "organization".to_string(),
            slug: "acme".to_string(),
            name: "Acme".to_string(),
            purpose: "ship".to_string(),
            root_department_id: ROOT_DEPARTMENT_ID.to_string(),
            policy: OrganizationPolicy {
                supervision_interval_ms: 1_000,
                acknowledgement_timeout_ms: 1_000,
                acknowledgement_retry_limit: 1,
                replacement_limit: 1,
            },
            department_order: vec![ROOT_DEPARTMENT_ID.to_string()],
            people_order: Vec::new(),
            departments,
            people: BTreeMap::new(),
            created_at: AT.to_string(),
            updated_at: AT.to_string(),
            extra: Default::default(),
        }
    }

    fn active(socket: &str) -> RuntimeOwner {
        RuntimeOwner {
            version: 1,
            organization: "acme".to_string(),
            status: RuntimeOwnerStatus::Active,
            socket_name: Some(socket.to_string()),
            claimed_at: Some(AT.to_string()),
            validated_at: Some(AT.to_string()),
            released_at: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn initial_ownership_is_released_and_names_only_the_company() {
        let owner = initial_runtime_ownership(&manifest());
        assert_eq!(owner.status, RuntimeOwnerStatus::Released);
        assert_eq!(owner.organization, "acme");
        assert!(owner.socket_name.is_none());
        assert!(validate_ownership(&manifest(), &owner).is_ok());
    }

    #[test]
    fn validate_refuses_a_foreign_company() {
        let mut owner = active("default");
        owner.organization = "other".to_string();
        let refusal = validate_ownership(&manifest(), &owner).expect_err("foreign company");
        assert_eq!(refusal.code, RUNTIME_OWNERSHIP_INVALID);
        assert_eq!(refusal.message, "Runtime ownership does not match organization 'acme'");

        let mut owner = active("default");
        owner.version = 2;
        assert!(validate_ownership(&manifest(), &owner).is_err());
    }

    #[test]
    fn validate_refuses_an_active_record_with_no_socket() {
        let mut owner = active("default");
        owner.socket_name = None;
        let refusal = validate_ownership(&manifest(), &owner).expect_err("no socket");
        assert_eq!(
            refusal.message,
            "Active runtime ownership for 'acme' is missing its explicit runtime socket"
        );
        // A blank socket is the same failure, not a valid one.
        let mut blank = active("default");
        blank.socket_name = Some("   ".to_string());
        assert!(validate_ownership(&manifest(), &blank).is_err());
    }

    #[test]
    fn audit_requires_an_explicit_actuator_identity() {
        let owner = initial_runtime_ownership(&manifest());
        let refusal = audit_ownership(&manifest(), &owner, "  ", false).expect_err("blank socket");
        assert_eq!(refusal.code, RUNTIME_OWNERSHIP_IDENTITY_REQUIRED);
        assert_eq!(refusal.message, "An explicit runtime socket name is required");
        // The identity is OPAQUE: any non-blank string is a usable owner name,
        // because chiefd compares it and never parses it. A session name is no
        // longer half of the identity, and a caller that supplies one gets no
        // special treatment.
        assert_eq!(
            audit_ownership(&manifest(), &owner, "org-acme", false).expect("opaque"),
            OwnershipVerdict::Unchanged
        );
    }

    #[test]
    fn a_released_company_or_the_same_socket_is_unchanged() {
        let released = initial_runtime_ownership(&manifest());
        assert_eq!(
            audit_ownership(&manifest(), &released, "default", false).expect("released"),
            OwnershipVerdict::Unchanged
        );
        assert_eq!(
            audit_ownership(&manifest(), &active("default"), "default", true).expect("same socket"),
            OwnershipVerdict::Unchanged
        );
    }

    #[test]
    fn a_live_prior_projection_refuses_the_takeover() {
        let refusal = audit_ownership(&manifest(), &active("default"), "other", true)
            .expect_err("projection live");
        assert_eq!(refusal.code, RUNTIME_OWNERSHIP_PROJECTION_LIVE);
        assert_eq!(
            refusal.message,
            "Organization 'acme' is already running on runtime socket 'default'; refusing 'other'"
        );
    }

    #[test]
    fn a_dead_prior_projection_allows_the_takeover() {
        let verdict =
            audit_ownership(&manifest(), &active("default"), "other", false).expect("takeover");
        assert_eq!(
            verdict,
            OwnershipVerdict::Takeover { previous_socket_name: "default".to_string() }
        );
        assert!(verdict.is_takeover());
    }

    #[test]
    fn a_takeover_restamps_claimed_at_and_a_revalidation_carries_it_forward() {
        let previous = active("default");
        let taken = claimed_ownership(
            &manifest(),
            &OwnershipVerdict::Takeover { previous_socket_name: "default".to_string() },
            &previous,
            "other",
            LATER,
        );
        assert_eq!(taken.claimed_at.as_deref(), Some(LATER));
        assert_eq!(taken.validated_at.as_deref(), Some(LATER));
        assert_eq!(taken.socket_name.as_deref(), Some("other"));
        assert_eq!(taken.status, RuntimeOwnerStatus::Active);
        assert!(taken.released_at.is_none());

        let revalidated = claimed_ownership(
            &manifest(),
            &OwnershipVerdict::Unchanged,
            &previous,
            "default",
            LATER,
        );
        assert_eq!(revalidated.claimed_at.as_deref(), Some(AT), "claimedAt carries forward");
        assert_eq!(revalidated.validated_at.as_deref(), Some(LATER));
    }

    #[test]
    fn claiming_a_released_company_starts_a_fresh_claim_stamp() {
        let released = initial_runtime_ownership(&manifest());
        let claimed = claimed_ownership(
            &manifest(),
            &OwnershipVerdict::Unchanged,
            &released,
            "default",
            LATER,
        );
        assert_eq!(claimed.claimed_at.as_deref(), Some(LATER));
    }

    /// THE HANDOFF'S RE-CLAIM, which is not the `initial_runtime_ownership`
    /// case above: a released row that STILL NAMES the socket it was released
    /// from.
    ///
    /// `chief attach` releases a stale claim, restarts the daemon onto another
    /// socket and claims again, and the row it claims over reads
    /// `status='released', socketName='default'`. The old socket must not
    /// refuse the new one from beyond the release — not even when something is
    /// still projecting there, because the release already said this company
    /// does not run there. Nothing else re-mints a claim after a handoff, so a
    /// refusal here would leave a running company holding none.
    #[test]
    fn a_released_row_that_still_names_its_old_socket_is_reclaimed_onto_a_new_one() {
        let handed_off = released_ownership(&manifest(), &active("default"), "default", LATER)
            .expect("the handoff releases the socket it holds");
        assert_eq!(handed_off.socket_name.as_deref(), Some("default"));
        assert_eq!(handed_off.status, RuntimeOwnerStatus::Released);

        assert_eq!(
            audit_ownership(&manifest(), &handed_off, "qa", true)
                .expect("a released row refuses nobody"),
            OwnershipVerdict::Unchanged
        );
        let claimed =
            claimed_ownership(&manifest(), &OwnershipVerdict::Unchanged, &handed_off, "qa", LATER);
        assert_eq!(claimed.status, RuntimeOwnerStatus::Active);
        assert_eq!(claimed.socket_name.as_deref(), Some("qa"));
        assert_eq!(claimed.claimed_at.as_deref(), Some(LATER));
    }

    #[test]
    fn a_revalidation_with_no_prior_claim_stamp_falls_back_to_now() {
        let mut previous = active("default");
        previous.claimed_at = None;
        let claimed = claimed_ownership(
            &manifest(),
            &OwnershipVerdict::Unchanged,
            &previous,
            "default",
            LATER,
        );
        assert_eq!(claimed.claimed_at.as_deref(), Some(LATER));
    }

    #[test]
    fn releasing_from_a_foreign_socket_is_refused() {
        let refusal = released_ownership(&manifest(), &active("default"), "other", LATER)
            .expect_err("foreign release");
        assert_eq!(refusal.code, RUNTIME_OWNERSHIP_RELEASE_FOREIGN);
        assert_eq!(
            refusal.message,
            "Refusing to release 'acme' runtime owned by runtime socket 'default' from 'other'"
        );
    }

    #[test]
    fn releasing_the_owning_socket_keeps_the_claim_stamp_and_records_the_release() {
        let released =
            released_ownership(&manifest(), &active("default"), "default", LATER).expect("release");
        assert_eq!(released.status, RuntimeOwnerStatus::Released);
        assert_eq!(released.claimed_at.as_deref(), Some(AT));
        assert_eq!(released.validated_at.as_deref(), Some(LATER));
        assert_eq!(released.released_at.as_deref(), Some(LATER));
        assert_eq!(released.socket_name.as_deref(), Some("default"));
    }

    #[test]
    fn releasing_an_already_released_company_from_any_socket_is_allowed() {
        // Nothing is being taken from anyone: the idempotent shutdown path.
        let released = released_ownership(
            &manifest(),
            &initial_runtime_ownership(&manifest()),
            "other",
            LATER,
        )
        .expect("idempotent release");
        assert_eq!(released.status, RuntimeOwnerStatus::Released);
    }
}
