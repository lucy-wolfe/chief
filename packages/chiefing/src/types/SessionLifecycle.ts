// Wire types for the supervision & session-lifecycle verb surface.
//
// Rust authority: apps/chiefd/crates/chiefd-core/src/store/session_maintenance.rs,
// session_maintenance_ops.rs, operator_escalation.rs, and the
// handlers in chiefd-api/src/docstore/router.rs.
//
// These are VERBS, not documents. There is deliberately no read-modify-publish
// pair here: the decision belongs to chiefd, and a client that could publish a
// whole maintenance ledger could also publish an illegal one.

/** What kind of session maintenance a request performs. The Rust side closes
 * the same set. */
// ONE ACTION. `fresh_session` and `set_model` are deleted with
// `org_maintain_session`; the automatic compaction still queues through this
// pipeline, so the type stays rather than collapsing to a bare marker.
export type MaintenanceAction = 'compact'

/** Where a maintenance request is in its life. */
export type MaintenanceStatus =
  'queued' | 'running' | 'applying' | 'completed' | 'failed' | 'skipped'

/** The chiefd-injected caller identity every worker verb presents. */
export interface WorkerIdentity {
  personId: string
}

/** The process/session/token triple that owns a maintenance request. */
export interface MaintenanceClaim {
  processId: number
  sessionId: string
  claimToken: string
}

/** One durable maintenance request, as chiefd returns it. */
export interface MaintenanceRequest {
  id: string
  action: MaintenanceAction
  personId: string
  requestedBy: string
  reason: string
  automatic: boolean
  status: MaintenanceStatus
  requestedAt: string
  startedAt?: string
  completedAt?: string
  error?: string
  attempt?: number
  recoveredFromRequestId?: string
  retryNotBefore?: string
  claimedProcessId?: number
  claimedSessionId?: string
  claimToken?: string
  completedProcessId?: number
  completedSessionId?: string
  completionClaimToken?: string
  companyActionId?: string
  force?: boolean
  interruptedProcessId?: number
  interruptedSessionId?: string
  interruptedClaimToken?: string
  interruptedAt?: string
  compactSessionId?: string
  compactAnchorEntryId?: string
  completedCompactionEntryId?: string
  // TOMBSTONE: `requestedModelProvider` and `requestedModel`, `set_model`'s
  // ledger columns. Dropped from the Rust row layer with the action.
}

// TOMBSTONE: `CompanyActionTarget` and `CompanySessionAction`, the TS mirror of
// the whole-company action the Rust ledger no longer carries. Deleted with
// `org_maintain_session`; the `maintenance_company_action_targets` table went
// with them and the empty parent survives only for a foreign key.

/** The whole session-maintenance ledger. */
export interface SessionMaintenanceLedger {
  schemaVersion: number
  organization: string
  requestOrder: string[]
  requests: Record<string, MaintenanceRequest>
  createdAt: string
  updatedAt: string
}

/** Everything `queue` needs. */
export interface QueueMaintenanceInput {
  action: MaintenanceAction
  personId: string
  requestedBy: string
  /** An optional operator note. NEVER required: chiefd authors the ledger
   * line from the action and the requester when a caller sends none. */
  reason?: string
  automatic?: boolean
  force?: boolean
}

/** Optional native-compact branch boundary a claim may pin. */
export interface CompactAnchor {
  sessionId: string
  entryId: string
}

/** What `recover` produced. */
export interface RecoveredMaintenance {
  interrupted: MaintenanceRequest[]
  replacements: MaintenanceRequest[]
}

/** What the acknowledgement drain folded and dropped. */
export interface AckDrainResult {
  acknowledged: string[]
  rejected: string[]
}

/** What the operator-escalation drain recorded and dropped. */
export interface OperatorEscalationDrainResult {
  recordedFingerprints: string[]
  rejectedFingerprints: string[]
  doorbellArmed: boolean
}

/** One row of the out-of-band operator escalation log. */
export interface OperatorEscalationRecord {
  fingerprint: string
  kind: string
  personId: string
  blocker: string
  operatorAction: string
  queuedAt: string
  recordedAt: string
}

/** What chiefd says to do about the pending human doorbell. */
export type DoorbellPlan =
  | { plan: 'nothing-pending' }
  | { plan: 'suppressed-by-cooldown' }
  | { plan: 'ring'; text: string; fingerprint: string; attempts: number }

/** What a delivery attempt produced. */
export type DoorbellOutcome = 'delivered' | 'not-delivered' | 'skipped'

/** How the doorbell state moved after the outcome was committed. */
export interface DoorbellSettlement {
  settled: 'delivered' | 'deferred' | 'dropped'
}

/** The clean-session epoch. */
export interface SessionEpoch {
  version: number
  organization: string
  epochAt: string
  reason: string
}
