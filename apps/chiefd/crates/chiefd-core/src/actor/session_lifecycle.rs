//! The writer-actor surface for supervision and session lifecycle.
//!
//! One method per verb, each one exactly one `BEGIN IMMEDIATE`. These are the
//! operations `apps/cli/src/legacy/organization/` used to perform by reading a
//! document over HTTP, deciding in TypeScript, and publishing the result back
//! under a compare-and-swap with a retry ladder. The decision now happens on
//! the writer thread between the read and the write, so the round trip, the
//! CAS, the retry and the sleep all disappear together.
//!
//! Two shapes are used, and the choice is not stylistic:
//!
//! * verbs whose whole subject is rows ([`operator_escalation`], the session
//!   epoch) run in [`CompanyDb::in_transaction`];
//! * verbs that also move the supervision ledger — whose assignment and effect
//!   rows are owned by `Ledgers` — run in `in_transaction_and_mutate`, so the
//!   row half and the ledger half land in the same commit.
//!
//! None of them polls, sleeps, holds a lease, or takes a lock: the writer queue
//! is the serialization, and `.await` is the wait.

use crate::actor::{CompanyDb, MutationClass, MutationName};
use crate::error::store_failure;
use crate::isotime::iso_millis;
use crate::ledger::Ledgers;
use crate::store::operator_escalation::{
    self, DoorbellOutcome, DoorbellPlan, DoorbellSettlement, OperatorEscalationRecord,
};
use crate::store::organization::{self, OrganizationManifest};
use crate::store::session_maintenance::rows::SESSION_MAINTENANCE_STORE;
use crate::store::session_maintenance::{
    MaintenanceAction, MaintenanceRequest, MaintenanceStatus, SessionMaintenanceLedger,
};
use crate::store::session_maintenance_ops::{
    self as maint_ops, Claim, CompactAnchor, ExpectedIdentity, QueueInput, RecoveredMaintenance,
};
use crate::store::supervision::{self, SupervisionLedger};
use crate::store::supervision_intake::{self, OperatorEscalationDrainReport};
use crate::ChiefdError;

/// Everything a session-maintenance verb decides against.
pub struct MaintenanceContext {
    /// The ledger being mutated. Staged back into `Ledgers` after the verb.
    pub maintenance: SessionMaintenanceLedger,
    /// A read-only supervision snapshot.
    pub supervision: SupervisionLedger,
    /// The manifest this mutation is fenced against.
    pub manifest: OrganizationManifest,
    /// The commit's instant.
    pub at: String,
}

/// Reconstruct the session-maintenance ledger from its rows.
///
/// Used as the SQL half of every maintenance verb, so the decision sees exactly
/// what is on disk in the transaction that will overwrite it.
fn read_maintenance(
    tx: &rusqlite::Transaction<'_>,
    slug: &str,
) -> Result<SessionMaintenanceLedger, ChiefdError> {
    let at = iso_millis(0);
    Ok(crate::store::session_maintenance::rows::reconstruct(tx, slug)
        .map_err(|e| store_failure("session-maintenance-rows", e))?
        .unwrap_or_else(|| SessionMaintenanceLedger::initial(slug, &at)))
}

/// Stage a mutated maintenance ledger for this commit.
///
/// `persist_dispatch` routes the `session-maintenance` document body to
/// `session_maintenance::rows`, so putting it here is what writes the rows —
/// in the same commit as whatever the supervision half did.
fn stage_maintenance(
    ledgers: &mut Ledgers,
    ledger: &SessionMaintenanceLedger,
) -> Result<(), ChiefdError> {
    let encoded = serde_json::to_string(ledger)
        .map_err(|e| store_failure("session-maintenance-encode", e))?;
    ledgers.put_document(SESSION_MAINTENANCE_STORE, encoded);
    Ok(())
}

impl CompanyDb {
    // --- session maintenance ---------------------------------------------

    /// The session-maintenance ledger as it stands.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn session_maintenance_ledger(
        &self,
    ) -> Result<SessionMaintenanceLedger, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| read_maintenance(tx, &slug)).await
    }

    /// Queue one durable maintenance request.
    ///
    /// # Errors
    /// A `Refused` from [`maint_ops::queue`], or [`ChiefdError::StoreFailure`].
    pub async fn session_maintenance_queue(
        &self,
        input: QueueInput,
    ) -> Result<MaintenanceRequest, ChiefdError> {
        self.maintenance_mutation(MutationName("org.session-maintenance.queue"), move |ctx| {
            maint_ops::queue(&mut ctx.maintenance, &ctx.manifest, &input, &ctx.at)
        })
        .await
    }

    /// Claim the next queued request for the exact running Pi generation.
    ///
    /// `Ok(None)` means "nothing to claim", which is the ordinary answer on a
    /// bounded idle probe and never an error.
    ///
    /// # Errors
    /// A `Refused` from [`maint_ops::start`], or [`ChiefdError::StoreFailure`].
    pub async fn session_maintenance_start(
        &self,
        identity: ExpectedIdentity,
        action: MaintenanceAction,
        request_id: Option<String>,
        claim: Option<Claim>,
        compact_anchor: Option<CompactAnchor>,
    ) -> Result<Option<MaintenanceRequest>, ChiefdError> {
        self.maintenance_mutation(MutationName("org.session-maintenance.start"), move |ctx| {
            maint_ops::start(
                &mut ctx.maintenance,
                &identity,
                &maint_ops::StartInput {
                    action,
                    request_id: request_id.as_deref(),
                    claim: claim.as_ref(),
                    compact_anchor: compact_anchor.as_ref(),
                },
                &ctx.at,
            )
        })
        .await
    }

    /// Return an exact live claim to the queue.
    ///
    /// # Errors
    /// A `Refused` from [`maint_ops::defer`], or [`ChiefdError::StoreFailure`].
    pub async fn session_maintenance_defer(
        &self,
        request_id: String,
        claim: Claim,
        identity: ExpectedIdentity,
    ) -> Result<MaintenanceRequest, ChiefdError> {
        self.maintenance_mutation(MutationName("org.session-maintenance.defer"), move |ctx| {
            maint_ops::defer(&mut ctx.maintenance, &request_id, &claim, &identity, &ctx.at)
        })
        .await
    }

    /// Persist the exact supported Pi interrupt before invoking it.
    ///
    /// # Errors
    /// A `Refused` from [`maint_ops::record_interrupt`], or
    /// [`ChiefdError::StoreFailure`].
    pub async fn session_maintenance_interrupt(
        &self,
        request_id: String,
        claim: Claim,
        identity: ExpectedIdentity,
    ) -> Result<MaintenanceRequest, ChiefdError> {
        self.maintenance_mutation(MutationName("org.session-maintenance.interrupt"), move |ctx| {
            maint_ops::record_interrupt(
                &mut ctx.maintenance,
                &request_id,
                &claim,
                &identity,
                &ctx.at,
            )
        })
        .await
    }

    // TOMBSTONE: `session_maintenance_complete_native`, the actor verb behind
    // `/v1/org/session-maintenance/complete-native`. It credited a company
    // native reset from a genuinely DIFFERENT Pi session — the completion that
    // had to tell a source session from its successor. Deleted with the
    // feature.

    /// Recover an interrupted attempt and queue at most one successor.
    ///
    /// # Errors
    /// A `Refused` from [`maint_ops::recover_interrupted`], or
    /// [`ChiefdError::StoreFailure`].
    pub async fn session_maintenance_recover(
        &self,
        identity: ExpectedIdentity,
        claim: Claim,
    ) -> Result<RecoveredMaintenance, ChiefdError> {
        self.maintenance_mutation(MutationName("org.session-maintenance.recover"), move |ctx| {
            maint_ops::recover_interrupted(&mut ctx.maintenance, &identity, &claim, &ctx.at)
        })
        .await
    }

    /// Recover running maintenance claims whose exact Pi process is proven
    /// dead by the daemon supervisor.
    ///
    /// Each `(request, pid)` pair is checked again inside the writer
    /// transaction. A stale observation cannot recover a newer claim. The
    /// maintenance ledger permits only one unresolved request per person, so
    /// the existing claim-fenced recovery state machine remains the one place
    /// which fails the attempt and creates its bounded successor.
    pub async fn session_maintenance_recover_dead_claims(
        &self,
        dead_claims: Vec<(String, i64)>,
    ) -> Result<RecoveredMaintenance, ChiefdError> {
        self.maintenance_mutation(
            MutationName("org.session-maintenance.recover-dead-claims"),
            move |ctx| {
                let mut combined = RecoveredMaintenance::default();
                for (request_id, dead_pid) in &dead_claims {
                    let Some(request) = ctx.maintenance.request(request_id) else { continue };
                    if request.status != MaintenanceStatus::Running
                        || request.claimed_process_id != Some(*dead_pid)
                    {
                        continue;
                    }
                    let person_id = request.person_id.clone();
                    let replacement_pid = if *dead_pid == 1 { 2 } else { 1 };
                    let recovered = maint_ops::recover_interrupted(
                        &mut ctx.maintenance,
                        &ExpectedIdentity { person_id },
                        &Claim {
                            process_id: replacement_pid,
                            session_id: "chiefd-supervision-dead-claim".to_string(),
                            claim_token: "chiefd-supervision-dead-claim".to_string(),
                        },
                        &ctx.at,
                    )?;
                    combined.interrupted.extend(recovered.interrupted);
                    combined.replacements.extend(recovered.replacements);
                }
                Ok(combined)
            },
        )
        .await
    }

    /// Close a request.
    ///
    /// # Errors
    /// A `Refused` from [`maint_ops::finish`], or [`ChiefdError::StoreFailure`].
    pub async fn session_maintenance_finish(
        &self,
        request_id: String,
        status: MaintenanceStatus,
        error: Option<String>,
        compact_entry_id: Option<String>,
        identity: ExpectedIdentity,
    ) -> Result<MaintenanceRequest, ChiefdError> {
        self.maintenance_mutation(MutationName("org.session-maintenance.finish"), move |ctx| {
            maint_ops::finish(
                &mut ctx.maintenance,
                &maint_ops::FinishInput {
                    id: &request_id,
                    status,
                    error: error.as_deref(),
                    compact_entry_id: compact_entry_id.as_deref(),
                },
                &identity,
                &ctx.at,
            )
        })
        .await
    }

    /// Skip every queued company request whose target is parked.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn session_maintenance_reconcile_parked(
        &self,
        parked: Vec<String>,
    ) -> Result<Vec<String>, ChiefdError> {
        self.maintenance_mutation(
            MutationName("org.session-maintenance.reconcile-parked"),
            move |ctx| {
                maint_ops::skip_parked_company_targets(&mut ctx.maintenance, &parked, &ctx.at)
            },
        )
        .await
    }

    // --- the foreground queue --------------------------------------------

    /// Record every valid queued operator escalation, arm the doorbell for
    /// genuinely new content, and drop the queue rows — one commit.
    ///
    /// Entirely a row operation: the queue, the log and the doorbell are all
    /// rows, and none of them is in `Ledgers`.
    ///
    /// # Errors
    /// A `Refused` from the drain, or [`ChiefdError::StoreFailure`].
    pub async fn drain_operator_escalations(
        &self,
        at: String,
    ) -> Result<OperatorEscalationDrainReport, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.operator-escalation-intents.drain"),
            move |tx| {
                let manifest = crate::store::organization_rows::reconstruct(tx, &slug)?
                    .ok_or_else(|| {
                        ChiefdError::refused(
                            "unknown-company",
                            "this company has no organization manifest",
                        )
                    })?;
                supervision_intake::drain_operator_escalations(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                    &manifest,
                    &at,
                )
            },
        )
        .await
    }

    // --- operator escalation ----------------------------------------------

    /// The whole out-of-band escalation log, oldest first.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn operator_escalation_log(
        &self,
    ) -> Result<Vec<OperatorEscalationRecord>, ChiefdError> {
        let slug = self.label().to_string();
        self.read_txn(move |tx| operator_escalation::read_log(tx, &slug)).await
    }

    /// Decide what to do with the pending doorbell right now.
    ///
    /// A cooldown-suppressed doorbell is dropped in the same call that reports
    /// the suppression, so the caller never has to come back to clear it.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn operator_escalation_doorbell_plan(
        &self,
        now_ms: i64,
    ) -> Result<DoorbellPlan, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Small,
            MutationName("org.operator-escalation-push.plan"),
            move |tx| {
                let push = operator_escalation::read_push(tx, &slug)?;
                let plan = operator_escalation::plan_doorbell(&push, now_ms);
                if plan == DoorbellPlan::SuppressedByCooldown {
                    operator_escalation::drop_pending_doorbell(
                        tx,
                        &slug,
                        &crate::store::org_settings::display_slug(tx, &slug)?,
                    )?;
                }
                Ok(plan)
            },
        )
        .await
    }

    /// Commit the outcome of one delivery attempt.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn operator_escalation_doorbell_settle(
        &self,
        outcome: DoorbellOutcome,
        now_ms: i64,
    ) -> Result<DoorbellSettlement, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.operator-escalation-push.settle"),
            move |tx| {
                operator_escalation::settle_doorbell(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                    outcome,
                    now_ms,
                )
            },
        )
        .await
    }

    // --- session epoch ----------------------------------------------------

    /// Stamp the clean-session epoch. It only ever moves forward.
    ///
    /// # Errors
    /// A `Refused` from [`crate::store::session_epoch_ops::stamp`], or
    /// [`ChiefdError::StoreFailure`].
    pub async fn session_epoch_stamp(
        &self,
        epoch_at: String,
        reason: String,
    ) -> Result<crate::store::session_epoch_rows::SessionEpoch, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(
            MutationClass::Normal,
            MutationName("org.session-epoch.stamp"),
            move |tx| {
                crate::store::session_epoch_ops::stamp(
                    tx,
                    &slug,
                    &crate::store::org_settings::display_slug(tx, &slug)?,
                    &epoch_at,
                    &reason,
                )
            },
        )
        .await
    }

    /// The current epoch in epoch-millis, or `0` when there is none.
    ///
    /// # Errors
    /// [`ChiefdError::StoreFailure`] on a SQL failure.
    pub async fn session_epoch_ms(&self) -> Result<i64, ChiefdError> {
        let slug = self.label().to_string();
        self.in_transaction(MutationClass::Small, MutationName("org.session-epoch.ms"), move |tx| {
            crate::store::session_epoch_ops::epoch_ms(
                tx,
                &slug,
                &crate::store::org_settings::display_slug(tx, &slug)?,
            )
        })
        .await
    }

    // --- shared maintenance mutation plumbing -----------------------------

    /// Reconstruct the maintenance ledger, run `f` against it plus the current
    /// supervision snapshot and manifest, then stage the result — one commit.
    ///
    /// `reconcile_people` runs after every verb, so a structural removal that
    /// left an open request for a departed person cannot leave the ledger
    /// permanently invalid. That repair used to be a read-path side effect in
    /// TypeScript, which meant a hot read took a write; here it rides the
    /// mutation that was happening anyway.
    async fn maintenance_mutation<T, F>(&self, op: MutationName, f: F) -> Result<T, ChiefdError>
    where
        F: FnOnce(&mut MaintenanceContext) -> Result<T, ChiefdError> + Send + 'static,
        T: Send + 'static,
    {
        let slug = self.label().to_string();
        self.in_transaction_and_mutate(
            MutationClass::Normal,
            op,
            move |tx| read_maintenance(tx, &slug),
            move |maintenance, ledgers| {
                let manifest = organization::read(ledgers)?;
                let supervision = supervision::read(ledgers, &manifest)?;
                let at = iso_millis(ledgers.now().0);
                let mut ctx = MaintenanceContext { maintenance, supervision, manifest, at };
                let result = f(&mut ctx)?;
                maint_ops::reconcile_people(&mut ctx.maintenance, &ctx.manifest, &ctx.at);
                stage_maintenance(ledgers, &ctx.maintenance)?;
                Ok(result)
            },
        )
        .await
    }
}
