// The whole typed error taxonomy. Message text is human-readable and NOT
// part of the contract — nothing in this package (or any caller) may inspect
// error.message; every classifier here is `kind`/`code`/`instanceof`.

import type { BeacondUnavailableKind } from './types/Discovery.js'
import type { ChiefdUnavailableKind } from './types/Transport.js'

/** chiefd could not be reached, timed out, answered non-2xx infra, or
 * answered non-JSON. */
export class ChiefdUnavailableError extends Error {
  readonly name: 'ChiefdUnavailableError' = 'ChiefdUnavailableError'
  readonly kind: ChiefdUnavailableKind
  readonly url: string
  readonly path: string
  readonly status?: number
  /** chiefd's own sentence, when it sent one. Diagnostic only — `kind` is
   * still the sole classifier and nothing may branch on this. */
  readonly detail?: string

  constructor(params: {
    kind: ChiefdUnavailableKind
    url: string
    path: string
    status?: number
    detail?: string
    cause?: unknown
    message?: string
  }) {
    // The DETAIL, when chiefd sent one. An outage message that names only the
    // endpoint is the same defect as a refusal that reaches the agent as a
    // generic string: `chiefd unavailable (http-error) at …/v1/org/rows`
    // says nothing about whether the store was contended, quiescing, or
    // genuinely broken, and every layer above reads `.message`.
    super(
      params.message ??
        `chiefd unavailable (${params.kind}) at ${params.url}${params.path}` +
          (isNonEmpty(params.detail) ? `: ${params.detail}` : ''),
      { cause: params.cause }
    )
    this.kind = params.kind
    this.url = params.url
    this.path = params.path
    this.status = params.status
    this.detail = params.detail
  }
}

function isNonEmpty(value: string | undefined): value is string {
  return typeof value === 'string' && value.trim() !== ''
}

/**
 * The one transient classifier: chiefd will plausibly answer if asked again.
 *
 * Two members, and each is chiefd saying so rather than the caller guessing:
 *
 * * kind `'unreachable'` — the restart-blip class.
 * * a **429** — `ChiefdError::Busy`, which chiefd mints only after actually
 *   waiting its documented ladder. 429 is the one status in the taxonomy whose
 *   entire meaning is "back off and ask again"; classifying it as a permanent
 *   failure discards the instruction chiefd went to the trouble of sending.
 *
 * Timeouts are deliberately NOT transient (retrying one can double-apply a
 * write), and neither is any other `http-error` — a 500 is an operator's
 * problem and a 4xx is a rule, not weather.
 */
export function isTransientChiefdError(error: unknown): boolean {
  if (!(error instanceof ChiefdUnavailableError)) return false
  return error.kind === 'unreachable' || (error.kind === 'http-error' && error.status === 429)
}

/**
 * chiefd answered a named `/v1/org/*` row route with a refusal — one of
 * `REFUSAL_STATUSES` (`resources/OrgRoutes.ts`), carrying chiefd's own
 * `{code, detail}`.
 *
 * `code` is the branch point and is chiefd's, verbatim: `not_terminal`,
 * `head-needs-successor`, `handoff-refused`. It reaches the agent
 * because the message below is built from it — before the taxonomy was fixed
 * most of these routes answered a plain-text body with no code at all, and
 * three whole route families answered 500, so an agent read "chiefd
 * unavailable" for a rule it could have acted on.
 */
export class OrgRowRefusalError extends Error {
  readonly name: 'OrgRowRefusalError' = 'OrgRowRefusalError'
  readonly status: number
  readonly code: string
  readonly detail: string

  constructor(params: { status: number; code: string; detail: string; message?: string }) {
    // The DETAIL, not just the code. chiefd writes the actionable half there —
    // which mode is effective versus configured, which source a launcher root
    // came from, which command fixes it — and a message built from the code
    // alone throws all of it away. Every layer above reads `.message`, so the
    // one that drops the detail silently determines what an operator sees, and
    // this was the third layer in one chain doing exactly that.
    super(
      params.message ??
        (params.detail.trim() === ''
          ? `org row refused: ${params.code}`
          : `org row refused: ${params.code}: ${params.detail}`)
    )
    this.status = params.status
    this.code = params.code
    this.detail = params.detail
  }
}

/** chiefd answered a reminder route with a documented refusal. `code` is the
 * branch point and `detail` is chiefd's own sentence; both used to be thrown
 * away in favour of the raw response text, which meant a caller could only
 * pattern-match prose. */
export class ReminderRefusalError extends Error {
  readonly name: 'ReminderRefusalError' = 'ReminderRefusalError'
  readonly status: number
  readonly code: string
  readonly detail: string

  constructor(params: { status: number; code: string; detail: string; message?: string }) {
    super(
      params.message ??
        (params.detail.trim() === ''
          ? `reminder refused: ${params.code}`
          : `reminder refused: ${params.code}: ${params.detail}`)
    )
    this.status = params.status
    this.code = params.code
    this.detail = params.detail
  }
}

export class PersonContractsRefusalError extends Error {
  readonly name: 'PersonContractsRefusalError' = 'PersonContractsRefusalError'
  readonly code: string
  readonly detail: string

  constructor(params: { code: string; detail: string; message?: string }) {
    super(params.message ?? `person contracts refused: ${params.code}`)
    this.code = params.code
    this.detail = params.detail
  }
}

export class AuthAcquisitionError extends Error {
  readonly name: 'AuthAcquisitionError' = 'AuthAcquisitionError'
}

/**
 * #950/#954: thrown by every `*PublishCas` method on a 409 seq-conflict
 * response. The caller lost the race -- another writer committed since this
 * caller's read. The correct response is a fresh read-modify-write retry
 * (see each `*PublishCas` call site's own retry loop), never a reuse of the
 * stale draft that produced `expectedSeq`.
 */
export class SeqConflictError extends Error {
  readonly name: 'SeqConflictError' = 'SeqConflictError'
  readonly expectedSeq: number
  readonly currentSeq: number

  constructor(params: { expectedSeq: number; currentSeq: number; message?: string }) {
    super(
      params.message ??
        `seq conflict: expected ${params.expectedSeq}, current is ${params.currentSeq}`
    )
    this.expectedSeq = params.expectedSeq
    this.currentSeq = params.currentSeq
  }
}

// ---- beacond taxonomy (E10-chiefing-addendum.md, ruling D6) ----
// Mirrors the chiefd taxonomy above, value for value, so a reader who knows
// one taxonomy knows both.

/** beacond could not be reached, timed out, answered a non-2xx that is not a
 * documented refusal, or answered something that is not JSON. */
export class BeacondUnavailableError extends Error {
  readonly name: 'BeacondUnavailableError' = 'BeacondUnavailableError'
  readonly kind: BeacondUnavailableKind
  readonly beacondUrl: string
  readonly path: string
  readonly status?: number
  /** What the transport rejection itself knew — `ECONNREFUSED 127.0.0.1:6969`.
   * Diagnostic only, exactly like `ChiefdUnavailableError.detail`: `kind` is
   * still the sole classifier and nothing may branch on this. */
  readonly detail?: string

  constructor(params: {
    kind: BeacondUnavailableKind
    beacondUrl: string
    path: string
    status?: number
    detail?: string
    cause?: unknown
    message?: string
  }) {
    // `beacond unavailable (unreachable) at http://127.0.0.1:6969/v1/list`
    // named the address but never which failure it was. A refusal and a DNS
    // miss are different operator actions and read identically without this.
    super(
      params.message ??
        `beacond unavailable (${params.kind}) at ${params.beacondUrl}${params.path}` +
          (isNonEmpty(params.detail) ? `: ${params.detail}` : ''),
      { cause: params.cause }
    )
    this.kind = params.kind
    this.beacondUrl = params.beacondUrl
    this.path = params.path
    this.status = params.status
    this.detail = params.detail
  }
}

/** The one transient classifier for discovery, mirroring isTransientChiefdError.
 * True IFF BeacondUnavailableError with kind 'unreachable'. Timeouts are
 * deliberately NOT transient here, for the same reason they are not in the
 * chiefd taxonomy: two predicates with different rules is how a caller
 * learns to guess. */
export function isTransientBeacondError(error: unknown): boolean {
  return error instanceof BeacondUnavailableError && error.kind === 'unreachable'
}

/** beacond answered a documented refusal (a 4xx with a {code, message} body).
 * code is the branch point — `'bad-request'` for a `dir` that is not an
 * absolute path, `'unknown-company'` for a directory holding no company. Never
 * branch on message.
 *
 * `'slug-taken'` used to be the notable one here. It is deleted with the
 * uniqueness it enforced: two directories may hold companies with the same
 * display word, so a create has no conflict arm left to refuse from. */
export class DiscoveryRefusalError extends Error {
  readonly name: 'DiscoveryRefusalError' = 'DiscoveryRefusalError'
  readonly status: number
  readonly code: string

  constructor(params: { status: number; code: string; message?: string }) {
    super(params.message ?? `discovery refused: ${params.code}`)
    this.status = params.status
    this.code = params.code
  }
}

/** chiefd's resident lifecycle surface refused a create/boot/stop, either as
 * a 4xx `{code, detail}` body or as the stream's own terminal `failed` frame.
 * `code` is the branch point — `'lifecycle-failed'` for an ordinary refusal,
 * `'lifecycle-abandoned'` for an operation that ended without reporting
 * (the outcome is genuinely unknown, so it is NOT safe to treat as "did not
 * happen"). Never branch on `detail`, which is operator-facing prose. */
export class CompanyLifecycleRefusalError extends Error {
  readonly name: 'CompanyLifecycleRefusalError' = 'CompanyLifecycleRefusalError'
  readonly code: string
  readonly detail: string

  constructor(params: { code: string; detail: string; message?: string }) {
    super(params.message ?? params.detail)
    this.code = params.code
    this.detail = params.detail
  }
}

/** beacond has no ROW for this DIRECTORY: no company occupies it — none was
 * ever created there, or it was deleted. Named by `dir` and not by a slug,
 * because a slug names no company: two directories may hold companies with the
 * same display word. */
export class UnknownCompanyError extends Error {
  readonly name: 'UnknownCompanyError' = 'UnknownCompanyError'
  readonly dir: string

  constructor(params: { dir: string; message?: string }) {
    super(params.message ?? `no company in directory: ${params.dir}`)
    this.dir = params.dir
  }
}

/** The company exists but nothing is serving it: its row has no url.
 * Distinct from UnknownCompanyError because the remedies differ — boot it
 * versus create it. */
export class CompanyNotRunningError extends Error {
  readonly name: 'CompanyNotRunningError' = 'CompanyNotRunningError'
  readonly dir: string

  constructor(params: { dir: string; message?: string }) {
    super(params.message ?? `company not running in directory: ${params.dir}`)
    this.dir = params.dir
  }
}
