import { isNullish } from '@/Nullish'
import { postOrgRoute } from '@/resources/OrgRoutes'
import type {
  CompactAnchor,
  DoorbellOutcome,
  DoorbellPlan,
  DoorbellSettlement,
  MaintenanceAction,
  MaintenanceClaim,
  MaintenanceRequest,
  MaintenanceStatus,
  OperatorEscalationDrainResult,
  OperatorEscalationRecord,
  QueueMaintenanceInput,
  RecoveredMaintenance,
  SessionEpoch,
  SessionMaintenanceLedger,
  WorkerIdentity
} from '@/types/SessionLifecycle'
import type { HttpTransport } from '@/types/Transport'

// PORT-SEAM: the seventeen modules this client replaces are deleted, and the
// files below still import symbols from them. Each line names the importer, the
// symbol it lost, and the method here that answers the same question. Every one
// of these files belongs to another slice of the same port, so the call sites
// are reconciled at merge rather than edited from here.
//
//   apps/cli/src/legacy/cli.ts
//     personHasPendingOrganizationMail        -> chiefd owns mailbox wake; see
//                                                the mailbox slice's seam
//     executeOrganizationSessionMaintenanceCommand,
//     projectOrganizationSessionMaintenanceCommandResult,
//     OrganizationSessionMaintenanceCommand   -> the eight maintenance verbs
//                                                below, dispatched directly
//     queueSessionMaintenance                 -> queueMaintenance
//     SESSION_MAINTENANCE_OPERATOR_REQUESTER  -> the literal 'operator'
//
//   apps/cli/src/legacy/organization/org-runtime.ts
//     ensureSupervisionLedger,
//     reopenFailedSupervisionEffects          -> chiefd's own cycle; no client
//                                                call is needed to ensure a
//                                                ledger that seeding creates
//     heldByPreemptibleConvergence            -> DELETED: there is no lock to
//                                                wait out (Mandate 4)
//     assertSupervisionOwner                  -> DELETED: chiefd reachability
//                                                is proven by the call itself
//     loadOrganizationCeoBootLease,
//     writeOrganizationCeoBootLease,
//     clearOrganizationCeoBootLease           -> DELETED: and since
//                                                chief-home-is-cwd §4c the boot
//                                                itself is deleted too, so
//                                                there is no lease anywhere
//     loadOrganizationSessionEpochMs          -> sessionEpochMs
//     writeOrganizationSessionEpoch           -> stampSessionEpoch
//
//   apps/cli/src/legacy/organization/org-assignment-command.ts
//   apps/cli/src/legacy/organization/org-company-session-actions.ts
//     loadSessionMaintenanceLedger            -> maintenanceLedger
//     mutate, mutateWithExternalDecisionSeq   -> DELETED: company actions are
//                                                queued through the verbs; there
//                                                is no client-side ledger mutator
//     SESSION_MAINTENANCE_*_INTERRUPTION_ERROR,
//     sessionMaintenanceRetryDelayMs          -> chiefd-side constants
//     liveOrganizationSupervisor              -> DELETED with the TS supervisor
//
//   org-units.ts, org-staffing.ts, org-staffing-lifecycle.ts
//     heldByPreemptibleConvergence            -> DELETED (Mandate 4)
//     queueSessionMaintenance                 -> queueMaintenance
//   org-lifecycle-status.ts
//     loadOrganizationCeoBootLease            -> DELETED (see above); the board
//                                                no longer carries a
//                                                `ceoOnlyBootInFlight` column
//   org-cold-start-state.ts
//     loadOrganizationSupervisor              -> DELETED with the TS supervisor
//   org-runtime-ownership.ts
//     liveOrganizationSupervisor              -> DELETED with the TS supervisor
//   org-activity.ts, org-repository.ts, org-runtime-contracts.ts, work-item.ts,
//   org-lifecycle-status.ts, org-loop-control.ts, org-model-command.ts,
//   org-task-command.ts, org-task-notification-drain.ts
//     type-only imports of SupervisionLedger / AssignmentRecord /
//     OrganizationEnvelope
//                                             -> the mailbox slice owns these
//                                                wire types now
//
//   conformance/lib/ops.ts (deleted; the fixtures preserve the shape)
//     the supervision + session-maintenance op verbs it backs
//                                             -> the corpus runner must call
//                                                these methods (and the
//                                                supervision slice's) instead
//                                                of the deleted modules

/**
 * The supervision & session-lifecycle verb client.
 *
 * Rust authority: `apps/chiefd/crates/chiefd-api/src/docstore/router.rs`
 * (`org_session_maintenance_*`, `org_operator_escalation_*`,
 * `org_session_epoch_*`). There was an `org_fresh_session_*` family; it is
 * deleted with `org_maintain_session`.
 *
 * Every method is one call to one chiefd verb. There is deliberately no
 * read-modify-publish pair and no compare-and-swap surface here: the decisions
 * these verbs make — who may claim a maintenance request, when a session
 * advances, whether an escalation is new — are chiefd's, and a client that
 * could publish a whole ledger could publish an illegal one. The TypeScript
 * that used to make them is deleted.
 */
export class SessionLifecycleClient {
  constructor(
    private readonly transport: HttpTransport,
    private readonly url: string = ''
  ) {}

  // --- session maintenance ------------------------------------------------

  /** The whole session-maintenance ledger. */
  async maintenanceLedger(slug: string): Promise<SessionMaintenanceLedger> {
    return postOrgRoute(this.transport, this.url, '/v1/org/session-maintenance/ledger', {
      slug
    })
  }

  /** Queue one durable, idempotent maintenance request. */
  async queueMaintenance(slug: string, input: QueueMaintenanceInput): Promise<MaintenanceRequest> {
    return postOrgRoute(this.transport, this.url, '/v1/org/session-maintenance/queue', {
      slug,
      ...input
    })
  }

  /**
   * Claim the next queued request for the exact running Pi session.
   *
   * `request` is `null` when there is nothing to claim, which is the ordinary
   * answer on a bounded idle probe and never an error.
   */
  async startMaintenance(
    slug: string,
    identity: WorkerIdentity,
    action: MaintenanceAction,
    options: {
      requestId?: string
      claim?: MaintenanceClaim
      compactAnchor?: CompactAnchor
    } = {}
  ): Promise<MaintenanceRequest | null> {
    const result = await postOrgRoute<{ request: MaintenanceRequest | null }>(
      this.transport,
      this.url,
      '/v1/org/session-maintenance/start',
      {
        slug,
        identity,
        action,
        ...(isNullish(options.requestId) ? {} : { requestId: options.requestId }),
        ...(isNullish(options.claim) ? {} : { claim: options.claim }),
        ...(isNullish(options.compactAnchor)
          ? {}
          : {
              compactSessionId: options.compactAnchor.sessionId,
              compactAnchorEntryId: options.compactAnchor.entryId
            })
      }
    )
    return result.request
  }

  /** Return an exact live claim to the queue. */
  async deferMaintenance(
    slug: string,
    requestId: string,
    identity: WorkerIdentity,
    claim: MaintenanceClaim
  ): Promise<MaintenanceRequest> {
    return postOrgRoute(this.transport, this.url, '/v1/org/session-maintenance/defer', {
      slug,
      requestId,
      identity,
      claim
    })
  }

  /** Persist the exact supported Pi interrupt before invoking it. */
  async recordMaintenanceInterrupt(
    slug: string,
    requestId: string,
    identity: WorkerIdentity,
    claim: MaintenanceClaim
  ): Promise<MaintenanceRequest> {
    return postOrgRoute(this.transport, this.url, '/v1/org/session-maintenance/interrupt', {
      slug,
      requestId,
      identity,
      claim
    })
  }

  // TOMBSTONE: `completeNativeCompanyFreshSession` and its
  // `/v1/org/session-maintenance/complete-native` route. Deleted with the
  // company native reset it credited.

  /** Recover an interrupted attempt and queue at most one successor. */
  async recoverMaintenance(
    slug: string,
    identity: WorkerIdentity,
    claim: MaintenanceClaim
  ): Promise<RecoveredMaintenance> {
    return postOrgRoute(this.transport, this.url, '/v1/org/session-maintenance/recover', {
      slug,
      identity,
      claim
    })
  }

  /** Close a request. */
  async finishMaintenance(
    slug: string,
    requestId: string,
    identity: WorkerIdentity,
    status: MaintenanceStatus,
    options: { error?: string; compactEntryId?: string } = {}
  ): Promise<MaintenanceRequest> {
    return postOrgRoute(this.transport, this.url, '/v1/org/session-maintenance/finish', {
      slug,
      requestId,
      identity,
      status,
      ...(isNullish(options.error) ? {} : { error: options.error }),
      ...(isNullish(options.compactEntryId) ? {} : { compactEntryId: options.compactEntryId })
    })
  }

  /** Skip every queued company request whose target is parked. */
  async reconcileParkedMaintenance(
    slug: string,
    parkedPersonIds: string[]
  ): Promise<{ skipped: string[] }> {
    return postOrgRoute(this.transport, this.url, '/v1/org/session-maintenance/reconcile-parked', {
      slug,
      parkedPersonIds
    })
  }

  // --- the three foreground queues ----------------------------------------

  /** Record every valid queued operator escalation and arm the doorbell. */
  async drainOperatorEscalations(slug: string, at: string): Promise<OperatorEscalationDrainResult> {
    return postOrgRoute(this.transport, this.url, '/v1/org/operator-escalation-intents/drain', {
      slug,
      at
    })
  }

  // --- operator escalation ------------------------------------------------

  /** The whole out-of-band escalation log, oldest first. */
  async operatorEscalationLog(slug: string): Promise<OperatorEscalationRecord[]> {
    const result = await postOrgRoute<{ records: OperatorEscalationRecord[] }>(
      this.transport,
      this.url,
      '/v1/org/operator-escalation-log/read',
      { slug }
    )
    return result.records
  }

  /**
   * What to do about the pending human doorbell right now.
   *
   * A cooldown-suppressed doorbell is dropped by the same call that reports the
   * suppression, so a caller never has to come back to clear it.
   */
  async doorbellPlan(slug: string, nowMs: number): Promise<DoorbellPlan> {
    return postOrgRoute(this.transport, this.url, '/v1/org/operator-escalation-push/plan', {
      slug,
      nowMs
    })
  }

  /** Commit the outcome of one delivery attempt. */
  async settleDoorbell(
    slug: string,
    outcome: DoorbellOutcome,
    nowMs: number
  ): Promise<DoorbellSettlement> {
    return postOrgRoute(this.transport, this.url, '/v1/org/operator-escalation-push/settle', {
      slug,
      outcome,
      nowMs
    })
  }

  // --- session epoch ------------------------------------------------------

  /** Stamp the clean-session epoch. It only ever moves forward. */
  async stampSessionEpoch(slug: string, epochAt: string, reason: string): Promise<SessionEpoch> {
    return postOrgRoute(this.transport, this.url, '/v1/org/session-epoch/stamp', {
      slug,
      epochAt,
      reason
    })
  }

  /** The current epoch in epoch-millis, or `0` when there is none. */
  async sessionEpochMs(slug: string): Promise<number> {
    const result = await postOrgRoute<{ epochMs: number }>(
      this.transport,
      this.url,
      '/v1/org/session-epoch/ms',
      { slug }
    )
    return result.epochMs
  }
}
