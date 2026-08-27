// Request/response types for the `/v1/org/*` activity, staffing-lifecycle,
// units, cold-start, caller-authorization, control-authority, person-contracts
// routes — the `OrgSliceClient` resource's wire contract.
//
// Every type mirrors its Rust struct field-for-field; the Rust side
// serializes camelCase (`#[serde(rename_all = "camelCase")]`), and an
// `Option` field there is OMITTED from the wire when absent, never sent as
// `null` — modeled here as an optional (`?`) property.

import type { UnitState } from '@/types/Organization'

// ---- lifecycle status (lifecycle_status.rs) --------------------------------

/** Durable attribution attached to one explicit person-start decision. Rust
 * authority: store/launch_intent_rows.rs `StartAttribution`, re-exported by
 * store/lifecycle_status.rs. */
export interface StartAttribution {
  initiatorPersonId?: string
  reason: string
  startedAt: string
}

/** One unit's row on the read-only up/down control board. Rust authority:
 * store/lifecycle_status.rs `LifecycleDepartmentStatus`. */
export interface LifecycleDepartmentStatus {
  id: string
  name: string
  /** Absent only on the root. */
  parentDepartmentId?: string
  state: UnitState
  /** True only when this unit **and every ancestor** is active. */
  effectiveActive: boolean
}

/** One person's row on the read-only up/down control board. Rust authority:
 * store/lifecycle_status.rs `LifecyclePersonStatus`. */
export interface LifecyclePersonStatus {
  personId: string
  name: string
  /** Wire spelling of `PersonKind`: `'executive'` | `'head'` | `'worker'`. */
  kind: string
  departmentId: string
  /** Wire spelling of `EmploymentState`: `'active'` | `'benched'` |
   * `'departed'`. A departed person is never on the board, so this is
   * effectively `'active'` | `'benched'`. */
  employmentState: string
  /** Durable desired up/down from the activity ledger — not live
   * observation. */
  desiredActive: boolean
  /** First durable instant with no effective demand, if idle. */
  idleSince?: string
  /** The durable "why is this person up?" attribution, when one exists. */
  startIntent?: StartAttribution
}

/** The whole read-only up/down control board. Rust authority:
 * store/lifecycle_status.rs `OrganizationLifecycleStatus`. */
export interface OrganizationLifecycleStatus {
  organization: string
  /* TOMBSTONE (chief-home-is-cwd §4c): `ceoOnlyBootInFlight: boolean` stood
   * here. It reported the CEO boot lease, which had exactly one writer — the
   * daemon-side CEO boot — and that is deleted, so the column could only ever
   * have said `false`. */
  /** Unit rows, in MAP order — not `departmentOrder`. The two differ, and the
   * pinned conformance fixtures record the map order; the `people` list below
   * genuinely is `peopleOrder`. */
  departments: LifecycleDepartmentStatus[]
  /** Person rows, in `peopleOrder`, bounded by `maxPeople`. */
  people: LifecyclePersonStatus[]
  /** Non-fatal observations — an unreadable source names itself here. */
  warnings: string[]
  /** Whether the people list was cut short. */
  truncated: boolean
}

// ---- units (unit_preview.rs) -----------------------------------------------

/** Who a unit removal would fire. Rust authority: store/unit_preview.rs
 * `UnitRemovalImpact`. */
export interface UnitRemovalImpact {
  /** The unit's own head, if it has one — the person the delete primarily
   * fires. */
  headPersonId?: string
  /** Every OTHER person the removal fires (its staff plus everyone homed in a
   * descendant unit), in roster order. */
  memberPersonIds: string[]
  /** Display names for `memberPersonIds`, in the same order. */
  memberNames: string[]
}

/** The exact set a unit removal would change, without writing anything.
 * `/v1/org/unit/removal-preview` returns only these two fields — not the
 * previewed manifest the Rust-internal `UnitRemovalPreview` struct also
 * carries. */
export interface UnitRemovalPreview {
  /** Units removed, in `departmentOrder`. */
  removedDepartmentIds: string[]
  /** People the removal would offboard, in `peopleOrder`. Their records are
   * retained as departed members of the removed unit's parent. */
  departedPersonIds: string[]
}

// ---- activity (activity.rs / activity_command.rs) --------------------------

/** What a graceful transition is preparing for. Rust: `TransitionAction`. */
export type TransitionAction = 'park' | 'transfer' | 'offboard'

/** Where a transition is in its life. Rust: `TransitionStatus`. */
export type TransitionStatus =
  'awaiting_handoff' | 'overdue' | 'ready' | 'applied' | 'cancelled' | 'forced'

/** One graceful transition. Rust authority: store/activity.rs
 * `GracefulTransition`. */
export interface GracefulTransition {
  /** `transition:<seq>:<person>:<action>`. */
  id: string
  personId: string
  action: TransitionAction
  /** Why, bounded to 500 characters. */
  reason: string
  /** Stable lifecycle command identity, when one owns this transition. */
  intentId?: string
  /** Placement at the moment the transition opened. */
  placementDepartmentId: string
  fromPaneDepartmentId: string
  /** Target unit; present iff the action needs one. */
  toDepartmentId?: string
  status: TransitionStatus
  requestedAt: string
  handoffDeadlineAt: string
  appliedAt?: string
  cancelledAt?: string
  /** Set **only** alongside `'forced'`. */
  forcedAt?: string
  /** Set **only** alongside `'cancelled'`, and only when the person provably
   * could not run. */
  abandonedAt?: string
}

/** What the `status` verb answers with. Rust authority:
 * store/activity_command.rs `ActivityCommandStatus`. */
export interface ActivityCommandStatus {
  personId: string
  /** Every transition still open for this person, in creation order. This was
   * a list of `{transition, prompt}` pairs — the prompt asked the pane to
   * write a bounded handoff before the change could apply. That handoff is
   * deleted, so only the transitions remain. */
  pendingTransitions: GracefulTransition[]
  /** The exact pending lifecycle authority for this person, reported only
   * when it is one of `requests`. */
  activeTransitionId?: string
}

// ---- staffing lifecycle -----------------------------------------------------

/** Response of `/v1/org/staffing/lifecycle`: the outcome of one staffing
 * lifecycle action (`bench` | `transfer` | `offboard`)
 * run end to end. `organization` is the request's own slug, not the full
 * manifest. */
export interface StaffingLifecycleResult {
  organization: string
  /** `'bench'` | `'transfer'` | `'offboard'`. */
  action: string
  personId: string
  /** Always `'applied'` today. */
  status: string
  handoff: 'completed' | 'abandoned'
  retryable: boolean
  /** Absent only for a request that applied directly with no transition
   * (`bench`, and any no-op). */
  transitionId?: string
  structuralChanged: boolean
  /** Problems that happened AFTER the mutation was already durable — today,
   * a failure to record the applied move in the activity ledger. The route
   * reports these rather than answering an error, because a caller told its
   * committed request failed retries it and is refused as already applied. */
  warnings: string[]
}

// ---- cold start (cold_start.rs) --------------------------------------------

/** What a cold-start clear removed. Rust authority: store/cold_start.rs
 * `ColdStartClearResult`. */
export interface ColdStartClearResult {
  /** How many people had a mailbox. */
  mailboxPersons: number
  /** How many envelopes were dropped across them. */
  mailboxEnvelopes: number
  /** How many acknowledgement receipts were dropped. */
  /** How many people the launch-intent fence had authorized. */
  launchIntentPersons: number
}

// ---- non-Doc result shapes (small routes with no dedicated Rust struct) ---

/** Response of `/v1/org/tree/read`. */
export interface TreeLinesResult {
  lines: string[]
}

/** Response of `/v1/org/unit/subtree`. */
export interface UnitSubtreeResult {
  unitIds: string[]
}

/** Response of `/v1/org/control-authority/{person,department}-in-scope`. */
export interface InScopeResult {
  inScope: boolean
}

/** Response of `/v1/org/person-contracts/build`. `published: false` means
 * nothing changed, and nothing was written. */
export interface BuildPersonContractsResult {
  published: boolean
  seq: number
}

/** One person as `/v1/org/tree/structured` carries them: placement and
 * identity only. `accent` is ABSENT for a standard Pi identity — that is the
 * documented special case, not a missing value.
 *
 * `employmentState` is placement rather than runtime state: a durable manifest
 * field that hire and offboard write. It is REQUIRED because a roster that can
 * omit it is a roster where a departed person is indistinguishable from an
 * active one, which is exactly the defect this field was added to close. */
export interface CompanyTreePerson {
  readonly id: string
  readonly name: string
  readonly title: string
  readonly kind: string
  readonly employmentState: 'active' | 'benched' | 'departed'
  readonly accent?: string
}

/** A department and everything beneath it, in canonical order. */
export interface CompanyTreeDepartment {
  readonly id: string
  readonly name: string
  readonly headPersonId: string
  readonly state: 'active' | 'paused'
  readonly people: readonly CompanyTreePerson[]
  readonly children: readonly CompanyTreeDepartment[]
}

/** Response of `/v1/org/tree/structured`. No envelope: the body IS the tree. */
export interface CompanyTreeResult {
  readonly slug: string
  readonly rootDepartmentId: string
  readonly departments: readonly CompanyTreeDepartment[]
}
