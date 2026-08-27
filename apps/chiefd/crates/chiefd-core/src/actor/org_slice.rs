//! [`CompanyDb`] methods for the activity/staffing/units/people port.
//!
//! A separate file rather than more lines in `writer.rs` for one reason: these
//! are the compositions the ported TypeScript used to make by issuing several
//! HTTP calls in a row, and each one has to become exactly ONE transaction
//! (Mandate 4). Keeping them together makes that property inspectable — every
//! method below opens a single [`CompanyDb::in_transaction`] and does all of
//! its reading and writing inside it.

use crate::actor::{CompanyDb, MutationClass, MutationName};
use crate::error::store_failure;
use crate::error::Refusal;
use crate::store::cold_start::{
    assert_clear_complete, assert_company_stopped, ColdStartClearResult, StoppedProof,
};
use crate::store::person_contracts::build::rebuild_person_contracts;
use crate::ChiefdError;

impl CompanyDb {
    /// Prove the company is at rest and drop every replayable state family, in
    /// one transaction.
    ///
    /// Reads both stopped authorities, drops all mailbox rows, clears the
    /// acknowledgement-receipt queue and the launch-intent fence, then re-reads
    /// both row views and refuses if anything survived. Because the proof, the
    /// drop and the verification share one `BEGIN IMMEDIATE`, no company can
    /// start between them and a refusal rolls the whole thing back.
    ///
    /// # Errors
    /// * [`crate::store::cold_start::COMPANY_NOT_STOPPED`] when any authority
    ///   reports the company running.
    /// * [`crate::store::cold_start::CLEAR_INCOMPLETE`] when rows survived.
    /// * [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn cold_start_clear(&self, at: String) -> Result<ColdStartClearResult, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.cold-start.clear"),
            move |tx| {
                let runtime = crate::store::runtime_rows::reconstruct(tx, &slug)?;
                let owner = crate::store::runtime_owner_rows::reconstruct(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                )?;
                let owner_status = owner.as_ref().map(|o| match o.status {
                    crate::store::runtime_owner_rows::RuntimeOwnerStatus::Active => "active",
                    crate::store::runtime_owner_rows::RuntimeOwnerStatus::Released => "released",
                });
                assert_company_stopped(
                    &slug,
                    &StoppedProof {
                        runtime_status: runtime.as_ref().map(|r| r.status.as_str()),
                        runtime_owner_status: owner_status,
                    },
                )
                .map_err(ChiefdError::Refused)?;

                let mailbox_persons = crate::store::mailbox_rows::list_persons(tx, &slug)?;
                let mailbox_envelopes =
                    crate::store::mailbox_rows::reconstruct(tx, &slug)?.entries.len();
                let launch_intent_persons = crate::store::launch_intent_rows::reconstruct(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                )?
                .person_ids
                .len();

                crate::store::mailbox_rows::publish(
                    tx,
                    &slug,
                    &crate::store::mailbox_rows::MailboxSnapshot { entries: Vec::new() },
                )?;
                crate::store::launch_intent_rows::clear(tx, &slug, &at)?;

                let remaining_mailbox = crate::store::mailbox_rows::list_persons(tx, &slug)?;
                let remaining_intent = crate::store::launch_intent_rows::reconstruct(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                )?
                .person_ids;
                assert_clear_complete(&remaining_mailbox, &remaining_intent)
                    .map_err(ChiefdError::Refused)?;

                Ok(ColdStartClearResult {
                    mailbox_persons: mailbox_persons.len(),
                    mailbox_envelopes,
                    launch_intent_persons,
                })
            },
        )
        .await
    }

    /// Rebuild every person's operating contract from the manifest and publish
    /// it, in one transaction — but write nothing when no contract changed.
    ///
    /// Returns `(published, seq)`. `published` is false for the no-change case,
    /// where `seq` is the company's current audit sequence: a boot that
    /// rewrites nothing must not bump a revision, or every `AGENTS.md` mtime
    /// is re-stamped and extension drift detection loses its baseline.
    ///
    /// This is the BOOT entry point to [`rebuild_person_contracts`]. The roster
    /// mutations that create people call the same function inside their own
    /// transaction, so by the time this runs after one of them it finds nothing
    /// changed and writes nothing.
    ///
    /// # Errors
    /// * [`crate::store::organization::MANIFEST_INVALID`] when the manifest is
    ///   absent or a person references a unit the manifest does not contain.
    /// * [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn org_person_contracts_build(&self, at: String) -> Result<(bool, i64), ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.person-contracts.build"),
            move |tx| rebuild_person_contracts(tx, &slug, &at),
        )
        .await
    }

    /// Open a fenced graceful transition for one authenticated
    /// person, in one transaction.
    ///
    /// The caller supplies the person id it authenticated; there is no route
    /// through which a payload can name somebody else.
    ///
    /// # Errors
    /// Whatever [`crate::store::activity::begin_transition`] refuses —
    /// `unknown-person`, `invalid-input`, `transition-conflict`,
    /// `stale-fence`.
    pub async fn org_activity_prepare(
        &self,
        input: crate::store::activity::BeginTransitionInput,
    ) -> Result<crate::store::activity::GracefulTransition, ChiefdError> {
        self.mutate(MutationClass::Normal, MutationName("org.activity.prepare"), move |ledgers| {
            let manifest = crate::store::organization::read(ledgers)?;
            let supervision = crate::store::supervision::read(ledgers, &manifest)?;
            crate::store::activity::begin_transition(ledgers, &manifest, &supervision, &input)
        })
        .await
    }

    /// Release an open transition so its structural change may apply, in one
    /// transaction.
    ///
    /// This is what USED to be `org_activity_reflect`, and the rename is the
    /// point. The operation never needed a reflection: what a bench,
    /// return or offboard needs is for the person's pending transition to
    /// reach `ready`, because an APPLIED transition is what sheds launch
    /// intent and drives the pane teardown. The bounded handoff that used
    /// to ride along — summary, learning, artifacts, open commitments — was a
    /// separate product that has been deleted, and every caller of this was
    /// already fabricating one to get past it.
    ///
    /// # Errors
    /// Whatever [`crate::store::activity::release`] refuses —
    /// `unknown-transition`, `transition-fence-mismatch`,
    /// `transition-terminal`.
    pub async fn org_activity_release(
        &self,
        input: crate::store::activity::ReleaseInput,
    ) -> Result<crate::store::activity::GracefulTransition, ChiefdError> {
        self.mutate(MutationClass::Normal, MutationName("org.activity.release"), move |ledgers| {
            let manifest = crate::store::organization::read(ledgers)?;
            let supervision = crate::store::supervision::read(ledgers, &manifest)?;
            crate::store::activity::release(ledgers, &manifest, &supervision, &input)
        })
        .await
    }

    /// Record what one person's own pane says its AGENT is doing, in one
    /// transaction, so the settle countdown starts on a transition to idle and
    /// on nothing else.
    ///
    /// The caller supplies the person id it authenticated; a payload cannot
    /// nominate somebody else, exactly as on the prepare/release verbs above.
    ///
    /// # Errors
    /// Whatever [`crate::store::activity::note_agent_activity`] refuses --
    /// `unknown-person`.
    pub async fn org_activity_note_agent_state(
        &self,
        person_id: String,
        working: bool,
    ) -> Result<bool, ChiefdError> {
        self.mutate(
            MutationClass::Normal,
            MutationName("org.activity.agent-state"),
            move |ledgers| {
                let manifest = crate::store::organization::read(ledgers)?;
                let supervision = crate::store::supervision::read(ledgers, &manifest)?;
                crate::store::activity::note_agent_activity(
                    ledgers,
                    &manifest,
                    &supervision,
                    &person_id,
                    working,
                )
            },
        )
        .await
    }

    /// Record that a non-removal placement move applied, in one transaction.
    ///
    /// Issued by the staffing lifecycle immediately after the structural
    /// mutation commits, so the ledger's own account of where a person is
    /// stops lagging behind the rows chiefd just wrote. See
    /// [`crate::store::activity::settle_applied_move`] for why the
    /// the back-to-back placement refusal cannot be fixed at the fence.
    ///
    /// # Errors
    /// Whatever [`crate::store::activity::settle_applied_move`] refuses —
    /// `unknown-person`.
    pub async fn org_activity_settle_move(&self, person_id: String) -> Result<bool, ChiefdError> {
        self.mutate(
            MutationClass::Normal,
            MutationName("org.activity.settle-move"),
            move |ledgers| {
                let manifest = crate::store::organization::read(ledgers)?;
                let supervision = crate::store::supervision::read(ledgers, &manifest)?;
                crate::store::activity::settle_applied_move(
                    ledgers,
                    &manifest,
                    &supervision,
                    &person_id,
                )
            },
        )
        .await
    }

    /// Offboard a person who has no live pane, withdrawing
    /// their launch intent in the same transaction (#443).
    ///
    /// # Why this is not just `offboard_person`
    ///
    /// [`crate::store::org_ops::offboard_person`] deliberately LEAVES the
    /// launch-intent fence in place, because the ordinary graceful offboard
    /// sheds it when the bounded handoff applies — and the fence is what keeps
    /// the pane alive long enough to collect that handoff. A person absent
    /// from the supervision ledger has nothing to fence a handoff
    /// against, so that handoff can never complete: the fence would hold a
    /// departed person's pane open forever, which is the exact wedge the
    /// offboard path's own comment warns about. Withdrawing the intent here is
    /// what makes the reconcile take the pane down.
    ///
    /// Both writes share one `BEGIN IMMEDIATE`, so a crash cannot leave a
    /// departed person still authorized to run.
    ///
    /// # Errors
    /// Whatever `offboard_person` refuses; [`ChiefdError::StoreFailure`] on a SQL
    /// failure.
    pub async fn org_offboard_unattended(
        &self,
        person_id: String,
        at: String,
        actor: String,
    ) -> Result<crate::store::org_ops::OffboardOutcome, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.person.offboard-unattended"),
            move |tx| {
                let outcome =
                    crate::store::org_ops::offboard_person(tx, &slug, &person_id, &at, &actor)
                        .map_err(|e| store_failure("organization-rows", e))?;
                if !matches!(outcome, crate::store::org_ops::OffboardOutcome::Applied) {
                    return Ok(outcome);
                }
                crate::store::rows_txn::apply_and_emit::<rusqlite::Error, _>(
                    tx,
                    &slug,
                    &at,
                    &actor,
                    |tx| {
                        let mut touches = Vec::new();
                        if let Some(touch) = crate::store::launch_intent_rows::delete_person_fence(
                            tx,
                            &slug,
                            &person_id,
                            "offboard-unattended",
                        )? {
                            touches.push(touch);
                        }
                        Ok(touches)
                    },
                )
                .map_err(|e| store_failure("launch-intent-rows", e))?;
                Ok(outcome)
            },
        )
        .await
    }

    /// Read everything the staffing lifecycle decision needs, in one
    /// transaction: the manifest, the activity ledger, and the person's durable
    /// live pane.
    ///
    /// Read as a set rather than three calls so the decision cannot be made
    /// against a manifest from one instant and a fence from another — the
    /// exact divergence the #443 unattended-offboard branch exists to survive.
    ///
    /// # Errors
    /// [`crate::store::organization::MANIFEST_INVALID`] when the company has no
    /// manifest; [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn org_staffing_lifecycle_facts(
        &self,
    ) -> Result<
        (
            crate::store::organization::OrganizationManifest,
            Option<crate::store::activity::ActivityLedger>,
        ),
        ChiefdError,
    > {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Small,
            MutationName("org.staffing.lifecycle-facts"),
            move |tx| {
                let manifest = crate::store::organization_rows::reconstruct(tx, &slug)?
                    .ok_or_else(|| {
                        ChiefdError::Refused(Refusal::new(
                            crate::store::organization::MANIFEST_INVALID,
                            format!("Company '{slug}' has no organization manifest"),
                        ))
                    })?;
                let activity = crate::store::activity::rows::read_rows(tx, &slug, &manifest)
                    .map_err(crate::store::activity::rows::activity_store_failed)?;
                Ok((manifest, activity))
            },
        )
        .await
    }
}
