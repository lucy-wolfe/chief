// Wire types for chiefd's resident company-lifecycle surface (`chief host`,
// `apps/chiefd/crates/chief-cli/src/host/`). Every field name here is the Rust
// struct's own serde name; nothing is renamed on the way through.
//
// Type-only. The address helpers live beside the client that uses them, in
// `resources/CompanyLifecycle.ts`.

/**
 * The lifecycle phase vocabulary chiefd emits today.
 *
 * The Rust authority is `host::phases::Phase::name`, which IS a closed enum —
 * chiefd cannot emit a name outside it. This mirror is deliberately a
 * documentation type rather than a wire guard, because the two sides deploy
 * separately: a chiefd that learned a new phase must not be unreadable to a
 * client that has not, and a client that dropped an unrecognised phase would
 * hang a caller waiting for a step that already went past.
 *
 * So `CompanyLifecyclePhase.phase` is `string`, and this union plus
 * `isCompanyLifecyclePhaseName` are how a caller narrows it when it wants
 * exhaustiveness.
 *
 * The three groups:
 *
 * - `company-daemon-*` — the company's own chiefd process coming up or going
 *   down. Emitted by both create and boot.
 * - `durable-create*` — genesis. Create only.
 * - `ceo-*` — the CEO reaching a durably-started state. Both.
 */
export type CompanyLifecyclePhaseName =
  | 'company-daemon-start'
  | 'company-daemon-ready'
  | 'durable-create'
  | 'durable-create-complete'
  | 'durable-create-failed'
  | 'company-daemon-stop'
  | 'company-daemon-stopped'
  | 'company-daemon-stop-failed'
  // TOMBSTONE (chief-home-is-cwd §4c): 'ceo-prepare' and 'ceo-prepare-failed'.
  // They named the `prepare-ceo-only` POST, which is deleted with the
  // daemon-side CEO boot — the operator client owns every pane, so the daemon
  // has no boot to prepare. A create now goes straight from
  // 'durable-create-complete' to 'chief-start', and that is not a gap: the
  // company IS CEO-only at 'durable-create-complete', because genesis records
  // the start decision in the same transaction that seeds it.
  | 'chief-start'
  | 'chief-start-failed'

/** Every name in [`CompanyLifecyclePhaseName`], as a value. Ordered as a
 * successful create emits them, with each step's failure name beside it. */
export const COMPANY_LIFECYCLE_PHASE_NAMES: readonly CompanyLifecyclePhaseName[] = [
  'company-daemon-start',
  'company-daemon-ready',
  'durable-create',
  'durable-create-complete',
  'durable-create-failed',
  'company-daemon-stop',
  'company-daemon-stopped',
  'company-daemon-stop-failed',
  'chief-start',
  'chief-start-failed'
]

/** Narrow a wire phase name to the vocabulary this client knows. `false` means
 * "chiefd is newer than this client", never "the frame is bad". */
export function isCompanyLifecyclePhaseName(value: string): value is CompanyLifecyclePhaseName {
  return COMPANY_LIFECYCLE_PHASE_NAMES.some((name) => name === value)
}

/** One `event: phase` frame. */
export interface CompanyLifecyclePhase {
  /** Which step this is. Typically a [`CompanyLifecyclePhaseName`]; see that
   * type for why this is not narrowed on the wire. */
  readonly phase: string
  /** The company, present on every frame including the first. */
  readonly slug: string
  /** Human-readable context — a URL, a path, a refusal. Never parsed: the
   * phase name carries the meaning, the detail is for a human reading it. */
  readonly detail: string
}

/** The `event: created` / `event: booted` terminal frame. */
export interface CompanyLaunchResult {
  /** The canonical slug chiefd derived or confirmed — a DISPLAY name.
   *
   * It does not address the company. Two directories may hold companies with
   * the same slug, so a caller that navigates by this reaches whichever the
   * router resolves first, or nothing. Use `key`. */
  readonly slug: string
  /** The created company's directory key — what ADDRESSES it.
   *
   * Stated by the server rather than derived by the caller: the identity has
   * one definition (`host_primitives::rendezvous::company_key`) and a second
   * producer here is the duplication Stage 2 deleted nine of. */
  readonly key: string
  /** The directory the company occupies. */
  readonly dir: string
  /** The company daemon's proven base URL. */
  readonly url: string
  /** The person chiefd brought up as CEO. */
  readonly chiefPersonId: string
  /** The runtime session this company projects onto. Present even for an
   * api-hosted company, which has no session: it is the company's NAME for
   * one, not a claim that one exists. */
  readonly session: string
}

/** `POST /v1/company/create`'s body.
 *
 * Two strings. It used to carry a `bootstrap` — the exact model route of the
 * session that confirmed the design plus a refreshed observation — which made a
 * provider credential a precondition for every caller of this route, and this
 * route's callers are browsers. `chief host` resolves the box's own Founder
 * route and proves it against the operator's registry instead, so nothing this
 * side of chiefd holds a secret to create a company. The Founder pane's own
 * door (`FounderLaunch`) still carries a bootstrap, because a live Pi session
 * can attest the route it is actually running on. */
export interface CreateCompanyInput {
  /** The confirmed company name. chiefd derives the slug from it; a caller
   * never supplies one, because what a company is called is chiefd's. */
  readonly name: string
  /** The confirmed company purpose. */
  readonly purpose: string
}

/** `POST /v1/company/stop`'s answer. */
export interface CompanyStopResult {
  /** Which branch ran: `supervised`, `orphan-session-stopped`, or
   * `already-stopped`. */
  readonly mode: string
  /** The company. */
  readonly slug: string
  /** The runtime session that was addressed. */
  readonly session: string
  /** Whether a session was actually killed. */
  readonly sessionStopped: boolean
  /** Whether the daemon was asked to exit. */
  readonly daemonStopped: boolean
}
