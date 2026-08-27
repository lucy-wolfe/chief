//! The single-transaction `CompanyDb` verbs for the runtime slice.
//!
//! These are the write half of the port that deleted
//! `apps/cli/src/legacy/organization/org-company-session-actions.ts` and
//! `org-runtime-ownership.ts`. The decisions themselves are pure functions in
//! [`crate::store::company_session_action`] and
//! [`crate::store::runtime_ownership`]; this module is only the plumbing that
//! reads the inputs, applies one decision, and writes the result.
//!
//! # Why a separate module rather than more of `writer.rs`
//!
//! An inherent `impl` may be split across modules of one crate, and `writer.rs`
//! is already ~4700 lines that three parallel ports were editing at once. One
//! module line in `actor/mod.rs` is the whole footprint of this file.
//!
//! # Mandate 4, concretely
//!
//! Every verb below reads *and* writes inside a single `in_transaction`
//! closure, so it is one `BEGIN IMMEDIATE`. There is no read-then-write window
//! and no compensating cleanup pass: a mid-operation failure rolls the whole
//! thing back and leaves the database exactly as it was. There is likewise no
//! retry ladder here — the TypeScript wrapped each of these in a
//! `SeqConflictError` loop with sleeps, and the writer actor's queue replaced
//! both (mandate 1).

use rusqlite::Transaction;

use crate::actor::writer::CompanyDb;
use crate::actor::{MutationClass, MutationName};
use crate::error::{ChiefdError, Refusal};
use crate::store::runtime_owner_rows::RuntimeOwner;
use crate::store::runtime_ownership::{
    audit_ownership, claimed_ownership, initial_runtime_ownership, released_ownership,
    validate_ownership, OwnershipVerdict,
};

/// Refusal code for a verb that needs a manifest the company has not seeded.
pub const RUNTIME_VERB_NO_MANIFEST: &str = "runtime-verb-no-manifest";

// TOMBSTONE: `RUNTIME_VERB_NO_MAINTENANCE_LEDGER` and
// `RUNTIME_VERB_UNKNOWN_ACTION`. Both were company-action refusal codes: no
// ledger to queue a fanout against, and a progress lookup naming an action that
// is not there. Nothing outside this module and its re-export ever read them.

// TOMBSTONE: `desired_active` and `maintenance_ledger`. Both existed only for
// the company-session-action verbs — the first to know which people a fanout
// should target, the second to refuse queueing against a ledger that did not
// exist yet rather than minting ids that would collide when it appeared.

/// The committed manifest, refusing when genesis has not run.
fn manifest(
    tx: &Transaction<'_>,
    row_slug: &str,
) -> Result<crate::store::organization::OrganizationManifest, ChiefdError> {
    crate::store::organization_rows::reconstruct(tx, row_slug)?.ok_or_else(|| {
        ChiefdError::from(Refusal::new(
            RUNTIME_VERB_NO_MANIFEST,
            "This company has no committed manifest yet",
        ))
    })
}

impl CompanyDb {
    // TOMBSTONE: the five company-session-action verbs —
    // `company_action_unresolved`, `company_action_progress`,
    // `company_action_queue`, `company_action_skip_parked` and
    // `company_action_reconcile_claims`.
    //
    // They were the chiefd half of #54's "company-wide native reset and
    // compact actions", which the operator ruled out whole. Nothing in
    // production could ever QUEUE one: the only caller of the queue verb was
    // chiefing's own client method, whose callers are contract tests, and the
    // historical queuer was the legacy CLI deleted in `ca2da9b57` — no
    // replacement ever arrived. So this was a control plane fencing against an
    // action nothing could create.

    /// The company's runtime ownership row, deriving the documented initial
    /// state when it has never been written.
    ///
    /// Absence is the DECIDED initial state ("released"), not a refusal: a
    /// company that has never claimed a runtime does not own one. Even a lost
    /// row is safe as released, because a claim still proves no the live runtime
    /// projection before any takeover. Replaces
    /// `loadOrganizationRuntimeOwnership`.
    ///
    /// # Errors
    /// [`crate::store::runtime_ownership::RUNTIME_OWNERSHIP_INVALID`] when the
    /// stored row does not describe this company.
    pub async fn runtime_ownership_read(&self) -> Result<RuntimeOwner, ChiefdError> {
        let row_slug = self.label().to_string();
        let company_slug = row_slug.clone();
        self.read_txn(move |tx| {
            let org = manifest(tx, &row_slug)?;
            let stored =
                crate::store::runtime_owner_rows::reconstruct(tx, &row_slug, &company_slug)?;
            match stored {
                None => Ok(initial_runtime_ownership(&org)),
                Some(mut owner) => {
                    // chiefd derives the identity field from the wire
                    // document key, which is the composite `<slug>@<hash>`
                    // on the multi-company surface. The key already gates
                    // this read to THIS company at THIS root, so the bare
                    // identity is authoritative — restore it before the
                    // validator compares it against the manifest.
                    owner.organization.clone_from(&org.slug);
                    validate_ownership(&org, &owner)?;
                    Ok(owner)
                }
            }
        })
        .await
    }

    /// Claim the company's runtime for `socket_name`.
    ///
    /// The audit input — whether the previously recorded socket may still
    /// project a session — is the caller's runtime observation, because this
    /// transaction must not do I/O. The decision and the write share one
    /// `BEGIN IMMEDIATE`. Replaces `claimOrganizationRuntimeOwnership`.
    ///
    /// # Errors
    /// Every refusal in [`crate::store::runtime_ownership::audit_ownership`].
    pub async fn runtime_ownership_claim(
        &self,
        socket_name: String,
        prior_projection_exists: bool,
        now: String,
    ) -> Result<(RuntimeOwner, OwnershipVerdict), ChiefdError> {
        let row_slug = self.label().to_string();
        let company_slug = row_slug.clone();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("runtime-ownership.claim"),
            move |tx| {
                let org = manifest(tx, &row_slug)?;
                let previous = match crate::store::runtime_owner_rows::reconstruct(
                    tx,
                    &row_slug,
                    &company_slug,
                )? {
                    None => initial_runtime_ownership(&org),
                    Some(mut owner) => {
                        owner.organization.clone_from(&org.slug);
                        validate_ownership(&org, &owner)?;
                        owner
                    }
                };
                let verdict =
                    audit_ownership(&org, &previous, &socket_name, prior_projection_exists)?;
                let claimed = claimed_ownership(&org, &verdict, &previous, &socket_name, &now);
                crate::store::runtime_owner_rows::publish(tx, &row_slug, &company_slug, &claimed)?;
                Ok((claimed, verdict))
            },
        )
        .await
    }

    /// Release the company's runtime from `socket_name`.
    ///
    /// Refuses when a DIFFERENT socket owns it — releasing someone else's
    /// runtime is how two launchers end up believing they both own the
    /// session. Replaces `releaseOrganizationRuntimeOwnership`.
    ///
    /// # Errors
    /// [`crate::store::runtime_ownership::RUNTIME_OWNERSHIP_RELEASE_FOREIGN`].
    pub async fn runtime_ownership_release(
        &self,
        socket_name: String,
        now: String,
    ) -> Result<RuntimeOwner, ChiefdError> {
        let row_slug = self.label().to_string();
        let company_slug = row_slug.clone();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("runtime-ownership.release"),
            move |tx| {
                let org = manifest(tx, &row_slug)?;
                let current = match crate::store::runtime_owner_rows::reconstruct(
                    tx,
                    &row_slug,
                    &company_slug,
                )? {
                    None => initial_runtime_ownership(&org),
                    Some(mut owner) => {
                        owner.organization.clone_from(&org.slug);
                        validate_ownership(&org, &owner)?;
                        owner
                    }
                };
                let released = released_ownership(&org, &current, &socket_name, &now)?;
                crate::store::runtime_owner_rows::publish(tx, &row_slug, &company_slug, &released)?;
                Ok(released)
            },
        )
        .await
    }
}
