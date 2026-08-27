// Result shapes specific to RowStoresClient methods, plus every *Doc from
// org-row-stores.ts:213-339 — the reconstructed-document shapes chiefd's
// per-store `*_rows.rs` modules (apps/chiefd/crates/chiefd-core/src/store/)
// reconstruct on read and diff-and-commit on publish. Derived identity fields
// (version/schemaVersion/organization/sessionName/id) are INCLUDED because
// chiefd emits them on read and accepts them on publish; they are recomputed
// server-side, never stored as caller input.
//
// No whole-company removal wire family here at all (ruling D24/F25 + #751/G6):
// E7-S4/E7-S7 delete the PREPARE/FINALIZE verb protocol (its request,
// response, and stop-facts wire types) outright, and E7-S7 finished the job
// on the ROW family too — the four `company_removal*` tables are DROPped
// (`chiefd-core/src/schema.rs:496-510`) and no crate serves
// `/v1/org/company-removal/*`. This comment previously called those routes
// "still-served" while `RowStoresClient` shipped three clients for them;
// both are now deleted. `PendingUnitRemovalDoc`/`RemovalStateDoc` are deleted
// too: they claimed to be the department/contract removal journal behind the
// still-live `AtomicRemoveDepartmentOutcome` staffing verb, but that verb
// returns `RemoveDepartmentOutcome::Applied` and never touches a journal. The
// `unit_removals` tables those types mirrored had no writer in either language
// and are dropped from `schema.rs`.
//
// #844: none of the interfaces below model chiefd's `#[serde(flatten)] extra`
// catch-all ("item D"), and that is deliberate, not an oversight. Every
// `*_rows.rs` module's document struct carries `extra: BTreeMap<String,
// Value>`, but the write path REJECTS any publish where it is non-empty
// (422 `unmodeled-keys`) and read never populates it (reconstruct always
// builds it fresh-empty, or — where legacy keys are read-tolerated,
// `supervision/rows.rs` — strips them before the response leaves chiefd). So
// there is never a non-empty `extra` on the wire for any type here to lose:
// omitting the field is safe, not lossy. `chiefd-core/tests/
// serde_flatten_catchall_conformance.rs` makes this a structural guarantee
// (every `store/` module with a flatten catch-all must wire that rejection,
// or be on its explicit allowlist) rather than an implicit convention this
// comment could silently go stale against.

/** Rust authority: store/session_epoch_rows.rs `SessionEpoch`. */
export interface SessionEpochDoc {
  version: 1
  organization: string
  epochAt: string
  reason: string
}

/** Rust authority: store/goal_delivery_quiesce_rows.rs `GoalDeliveryQuiesce`.
 *
 * Named for a goal but not part of the goal feature: this is the converge
 * cycle's delivery-quiescence stamp (`runtime_lifecycle.rs`,
 * `converge_apply/cycle.rs`), which survived the goal deletion because the
 * mechanism it gates is effect delivery, not goals.
 *
 * AC6/#751: `sessionName` is gone here too, for the reason `RuntimeOwnerDoc`
 * already states — it stored `org-<slug>` for a slug the row is keyed by, and
 * the Rust model now treats it as a RETIRED key: dropped on publish so a
 * historical blob still backfills, never reconstructed, never served. */
export interface GoalDeliveryQuiesceDoc {
  version: 1
  organization: string
  quiescedAt: string
}

/** Rust authority: store/operator_escalation_push_rows.rs. */
export interface OperatorEscalationPushDoc {
  schemaVersion: 1
  lastPushedAt?: string
  pending?: { text: string; fingerprint: string; attempts: number }
}

/** Rust authority: store/runtime_owner_rows.rs.
 *
 * AC6: `sessionName` is gone. It stored `org-<slug>` for the slug this row is
 * already keyed by, and the Rust model treats it as a RETIRED key — dropped on
 * publish so a historical blob still backfills, never reconstructed, never
 * served. */
export interface RuntimeOwnerDoc {
  version: 1
  organization: string
  status: 'active' | 'released'
  socketName?: string
  claimedAt?: string
  validatedAt?: string
  releasedAt?: string
}

/** Rust authority: store/launch_intent_rows.rs / launch_intent.rs. */
export interface LaunchIntentAttribution {
  initiatorPersonId?: string
  reason: string
  startedAt: string
}

/** Rust authority: store/launch_intent_rows.rs / launch_intent.rs. *
 * AC6/#751: `sessionName` is gone here too, for the reason `RuntimeOwnerDoc`
 * already states — it stored `org-<slug>` for a slug the row is keyed by, and
 * the Rust model now treats it as a RETIRED key: dropped on publish so a
 * historical blob still backfills, never reconstructed, never served. */
export interface LaunchIntentDoc {
  version: 1
  organization: string
  personIds: string[]
  updatedAt: string
  attributions?: Record<string, LaunchIntentAttribution>
}

/** Rust authority: store/mutation_journal_rows.rs. */
export interface MutationJournalRecordDoc {
  mutationId: string
  verb: string
  fingerprint: string
  status: 'in-flight' | 'committed' | 'abandoned'
  startedAt: string
  updatedAt: string
  actor?: string
}

/** Rust authority: store/mutation_journal_rows.rs. */
export interface MutationJournalDoc {
  version: 1
  organization: string
  entries: MutationJournalRecordDoc[]
}

/** Rust authority: store/event_journal_rows.rs. */
export interface EventOnceMarkerDoc {
  schemaVersion: 1
  keyDigest: string
  event: Record<string, unknown>
}

/** Rust authority: `EventJournalInsertRequest` (router.rs) — the
 * insert-if-absent wire shape, distinct from `EventOnceMarkerDoc` (the
 * reconstructed read-back document): `keyDigest` is `sha256(id)` supplied by
 * the caller (chiefd needs no hasher), `id` is the logical event id used for
 * the `UNIQUE(slug, id)` constraint, and `createdAtMs` is the caller-clock
 * insert stamp. */
export interface EventOnceMarkerInsertInput {
  keyDigest: string
  id: string
  event: Record<string, unknown>
  createdAtMs: number
}

/** Rust authority: store/runtime_rows.rs. Written as an untyped Record on the
 * TS side — the runtime projection has no fixed shape here. */
export type RuntimeDoc = Record<string, unknown>

/* TOMBSTONE (chief-home-is-cwd §4c): `CeoBootLeaseDoc` stood here, mirroring
 * `store/boot_lease_rows.rs`. Both are deleted with the daemon-side CEO boot:
 * the lease was the exclusivity window that boot held, and the daemon boots no
 * pane, so nothing can hold one. */

/** Rust authority: store/converge_safety.rs `ConvergeSafetyState` (read via
 * store/converge_safety_rows.rs). The STORED converge/apply safety record —
 * `actuationMode` is the raw stored mode, never the breaker-folded
 * `effective_config()` projection (#861: a consumer deciding whether a
 * company is actuating needs the real value, not a computed approximation). */
export interface ConvergeSafetyDoc {
  schemaVersion: 1
  actuationMode: 'shadow' | 'apply'
  sweepLive: boolean
  budgetOverride: boolean
  consecutiveFailures: number
  breakerTripped: boolean
  breakerTrippedAt?: string
  cycleInProgress: boolean
  cycleStartedAtMs?: number
  lastRefusal?: {
    kind: string
    detail: string
    at: string
  }
}

/** Rust authority: store/health_monitor_rows.rs `HealthMonitorState` (+
 * `HealthLogCursor`/`HealthMonitorObservation`/`HealthMonitorIncident`/
 * `TerminalHealthIncidentResolution` — verified field-for-field). */
export interface HealthMonitorDoc {
  version: 1
  organization: string
  lastRunAt?: string
  cursors: Record<string, { device: string; inode: string; offset: number }>
  observations: Record<string, { firstObservedAt: string; lastObservedAt: string; count: number }>
  incidents: Record<
    string,
    {
      fingerprint: string
      kind: string
      detail: string
      firstSeenAt: string
      lastSeenAt: string
      count: number
      responsiblePersonId?: string
      unblockAction?: string
      observedCount?: number
      oldestAt?: string
      acknowledgedAt?: string
      alertRecipientPersonId?: string
      impairedMailboxPersonId?: string
    }
  >
  terminalResolutions: Record<
    string,
    {
      fingerprint: string
      kind: string
      firstSeenAt: string
      recipientPersonId: string
      acceptedAt: string
    }
  >
  /** Write-only explicit clear signal (F17 follow-up): incident fingerprints
   * the publishing pass positively resolved. Deletion requires positive
   * evidence server-side; a clear is not journaled, so the condition may
   * recur with a fresh firstSeenAt. */
  clearedFingerprints?: string[]
}

/** Rust authority: store/operator_escalation_intents_rows.rs. */
export interface OperatorEscalationIntentDoc {
  schemaVersion: 1
  fingerprint: string
  organization: string
  personId: string
  blocker: string
  operatorAction: string
  queuedAt: string
}

/** Rust authority: store/operator_escalation_intents_rows.rs. */
export interface OperatorEscalationIntentsDoc {
  schemaVersion: 1
  intents: Record<string, OperatorEscalationIntentDoc>
}

// TOMBSTONE: `ActuatorPresence`. Its Rust authority
// (`chiefd-core store/runtime_actuation.rs`) is deleted along with the whole
// observation path, so this mirror has no authority left to mirror.

/* TOMBSTONE (chief-home-is-cwd §4c): `PrepareCeoOnlyResult` stood here — the
 * answer of `POST /v1/org/runtime/prepare-ceo-only`, deleted with that route
 * and the daemon-side CEO boot it belonged to. The STORE operation survives and
 * genesis is its caller, but no client speaks it. */

/** Rust authority: store/org_ops.rs `DirectOutcome` / `start_person`. */
export interface StartPersonResult {
  applied: true
}

// ---- non-Doc result shapes (RowStoresClient methods that are not the plain
// OrgRowReadResult<T> read/publish pattern) ----

/** Rust authority: packages/piing/extensions/semantic-row-deltas.ts's route table
 * (`/v1/org/operator-escalation-intents/insert`) — the
 * ChiefD writer's insert-if-absent decision and audit sequence. */
export interface SemanticQueueInsertResult {
  status: 'inserted' | 'duplicate' | 'conflict'
  /** Immutable audit identity; never a write precondition. */
  seq: number
}

export interface ClearedResult {
  cleared: boolean
}

export interface InsertEventOnceMarkerResult {
  created: boolean
}

export interface PruneEventOnceMarkersResult {
  rowsAffected: number
}
