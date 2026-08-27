/**
 * The typed error taxonomy for `ChiefApiClientService`. Every non-2xx
 * response from apps/api decodes to exactly one of these `kind`s — the E5
 * Contract's status table (`## Error envelope`), never a message string a
 * caller has to pattern-match.
 */
export type ChiefApiErrorKind =
  | 'unauthorized' // 401 — triggers one token re-acquire + single retry
  | 'not-found' // 404
  | 'conflict' // 409 (e.g. company-not-api-hosted, person-not-running)
  | 'refusal' // 422 — chiefd refusal, passed through verbatim; NEVER retried
  | 'upstream' // 503, or a non-JSON/malformed error body
  | 'bad-request' // 400
  | 'network' // fetch failed / aborted

export interface ChiefApiErrorShape {
  kind: ChiefApiErrorKind
  status?: number
  /** Machine-readable code from the `{error: {code, detail}}` envelope. */
  code?: string
  /** Human sentence from the envelope — for a refusal, chiefd's own words,
   * shown verbatim, never reworded. */
  detail?: string
}

/** Thrown by every `ChiefApiClientService` method on a non-2xx response or a
 * transport failure. `kind` is the only thing a caller should branch on. */
export class ChiefApiError extends Error implements ChiefApiErrorShape {
  readonly kind: ChiefApiErrorKind
  readonly status?: number
  readonly code?: string
  readonly detail?: string

  constructor(params: {
    kind: ChiefApiErrorKind
    status?: number
    code?: string
    detail?: string
    message?: string
    cause?: unknown
  }) {
    super(params.message ?? params.detail ?? `chief-api error (${params.kind})`, {
      cause: params.cause
    })
    this.name = 'ChiefApiError'
    this.kind = params.kind
    this.status = params.status
    this.code = params.code
    this.detail = params.detail
  }
}
