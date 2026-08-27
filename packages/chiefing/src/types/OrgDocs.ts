// Generic org-doc read/write shapes shared across the aggregates, mailbox,
// row-store, and manifest resource families.

/** Result of a conditional-read-capable org route. The wire `doc`/`document`/
 * `ledger`/`mailbox`/`manifest` field is normalized to `document` — still the
 * serialized inner JSON string; parsing stays with the caller. */
export interface RowReadResult {
  found: boolean
  seq: number
  unchanged?: boolean
  document?: string
}

/** Result of a conditional-read-capable route that parses its document. */
export interface OrgRowReadResult<T> {
  found: boolean
  doc?: T
}

/**
 * #950/#954: `OrgRowReadResult<T>` plus the row's current audit sequence.
 * A SEPARATE type, not a widened `OrgRowReadResult`, deliberately -- adding
 * a required `seq` to the base type broke 42 existing tests asserting its
 * exact `{found: false}`/`{found: true, doc}` shape (deep-equality, not a
 * TS compile break). Only the `*PublishCas` read path needs `seq`; every
 * other `OrgRowReadResult` caller is untouched.
 */
export interface OrgRowReadResultWithSeq<T> extends OrgRowReadResult<T> {
  /** 0 for an absent row (no prior write to conflict with) -- matches
   * `RowReadResult.seq`'s own `wire.seq ?? 0` default. */
  seq: number
}

/** A 422 body decoded as a value, never thrown, never retried. */
export type AtomicDirectOutcome = { applied: true } | { refused: string; detail: string }

export interface ReadOpts {
  ifSeqNot?: number
}

/** Refusal fields decoded from a 422/404/400 body. Every refusal body chiefd
 * sends is `{code, detail}` (`{code|refused, detail|message}` covers the two
 * field-name variants seen across today's ported clients); a non-JSON body
 * (plain-text infra text) still yields a usable `detail`. */
export interface DecodedRefusal {
  code: string
  detail: string
}

/** The wire shapes every conditional-read-capable org route answers with,
 * across its five payload-field spellings. */
export interface WireRowRead {
  found: boolean
  seq?: number
  unchanged?: boolean
  doc?: string
  document?: string
  ledger?: string
  mailbox?: string
  manifest?: string
}

/** What the Founder hands chiefd to create a company (`POST
 * /v1/founder/launch`, see `resources/FounderLaunch.ts`). Lives here rather
 * than beside the client because the extension-runtime graph is materialized
 * FLAT into a Pi home, where two `FounderLaunch.ts` basenames collide. */
export interface FounderLaunchInput {
  /** The confirmed company name. */
  readonly name: string
  /** The confirmed company purpose. */
  readonly purpose: string
}

/**
 * One step of a company launch, as chiefd narrates it while it happens.
 *
 * The `phase` values are chiefd's own closed vocabulary
 * (`chief-cli/src/host/phases.rs`, `Phase::name`) and are NOT parsed here: a
 * reader renders the label it recognizes and falls back to the raw name for one
 * it does not, so a chiefd that learns a new phase never breaks an older
 * reader. `detail` is context for a human — a URL, a path, a refusal — and is
 * never interpreted.
 */
export interface FounderLaunchPhase {
  /** chiefd's phase name. */
  readonly phase: string
  /** The company the frame is about. Present from the first frame. */
  readonly slug: string
  /** Human-readable context. Never parsed. */
  readonly detail: string
}

/** What chiefd created. */
export interface FounderLaunchResult {
  /** The company slug chiefd derived from the name. */
  readonly slug: string
  /** The company daemon's published URL. */
  readonly url: string
  /** The CEO chiefd prepared. */
  readonly chiefPersonId: string
  /** The runtime session the company projects onto. */
  readonly session: string
  /**
   * Why the operator was NOT handed over to the CEO, when they were not.
   *
   * Absent means the handoff happened. Present means the company is created,
   * durable and running but the operator's terminal is still wherever it was —
   * so the text carries the `chief attach <slug>` that gets them there. A
   * caller must never announce a CEO handoff without checking this: a launch
   * that reported "CEO booted in its ChiefD runtime session" while the operator
   * sat in the Founder pane is exactly the defect this field exists to make
   * un-sayable.
   */
  readonly handoffWarning?: string
}
