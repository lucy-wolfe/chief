import { ChiefdUnavailableError, isTransientChiefdError } from '../Errors.js'
import { awaitedDelay, ENSURE_SCHEMA_RETRY_DELAYS_MS } from '../transport/RetryPolicy.js'
import type { DocsRuntime, HealthProbe, WriterQueueSnapshot } from '../types/Health.js'
import type { HttpTransport } from '../types/Transport.js'

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object'
}

/** Parse a response body as JSON, returning `undefined` on a malformed body
 * (never throws — a truncated/invalid body is a data point for the caller,
 * not a transport failure). */
function parseJsonBody(body: string): unknown {
  try {
    return JSON.parse(body)
  } catch {
    return undefined
  }
}

function parseDocsRuntime(value: unknown): DocsRuntime | undefined {
  if (!isRecord(value)) return undefined
  if (value.mode !== 'company' && value.mode !== 'docstore-only') return undefined
  const company = typeof value.company === 'string' ? value.company : undefined
  return { mode: value.mode, company }
}

export class DocsClient {
  private schemaReady = false

  constructor(protected readonly transport: HttpTransport) {}

  /** POST `/v1/docs/ensure-schema`. The response body is discarded — this is
   * the idempotent create-if-absent preflight; only a transport-level
   * failure (never an answered non-2xx) is meaningful here. */
  async ensureSchema(): Promise<void> {
    await this.transport.post('/v1/docs/ensure-schema', {})
  }

  /**
   * Memoized `ensureSchema` with the #441 restart-blip ladder: retries ONLY
   * `isTransientChiefdError` failures (a connect refusal FetchTransport's own
   * inner ladder already gave up on) through `ENSURE_SCHEMA_RETRY_DELAYS_MS`,
   * awaited. ensure-schema commits nothing observable, so retrying it can
   * never duplicate a write. A genuinely down service (timeout, or the ladder
   * exhausted) still rethrows.
   */
  async ensureSchemaReady(): Promise<void> {
    if (this.schemaReady) return
    for (let attempt = 0; ; attempt += 1) {
      try {
        await this.ensureSchema()
        break
      } catch (error) {
        if (!isTransientChiefdError(error) || attempt >= ENSURE_SCHEMA_RETRY_DELAYS_MS.length) {
          throw error
        }
        await awaitedDelay(ENSURE_SCHEMA_RETRY_DELAYS_MS[attempt])
      }
    }
    this.schemaReady = true
  }

  /**
   * POST /v1/admin/shutdown -- asks the daemon to drain and exit through its
   * own shutdown watch. Fire-and-return: the daemon does not block the
   * response on the drain (router.rs's own doc: "the caller observes exit by
   * the socket closing / the beacond registration going stale, not by this
   * response"). A launcher never needs a pid to stop a daemon again (D7) --
   * this route's response carries no pid, and callers judge exit via
   * registrationLiveness against beacond, not this call.
   */
  async shutdown(reason: string): Promise<{ accepted: boolean }> {
    const response = await this.transport.post('/v1/admin/shutdown', { reason })
    const body = parseJsonBody(response.body)
    const accepted = isRecord(body) && body.accepted === true
    return { accepted }
  }

  /** True iff HTTP 200 with body `{"status":"ok"}`. */
  async health(): Promise<boolean> {
    try {
      const response = await this.transport.get('/v1/docs/health')
      if (response.status !== 200) return false
      const body = parseJsonBody(response.body)
      return isRecord(body) && body.status === 'ok'
    } catch {
      return false
    }
  }

  /** `GET /v1/docs/queue` — the single writer's depth, age and current
   * mutation.
   *
   * The only contention diagnostic there is, and the only one there needs to
   * be: chiefd serializes on one writer, so "is something stuck?" is a
   * question about queue depth rather than about who holds what. `current` is
   * left absent when the writer is idle and is never defaulted, so a caller
   * cannot mistake idleness for a running mutation. */
  async queueSnapshot(): Promise<WriterQueueSnapshot> {
    const response = await this.transport.get('/v1/docs/queue')
    const body = parseJsonBody(response.body)
    if (!isRecord(body) || typeof body.depth !== 'number') {
      // Object form, matching every other construction in this package
      // (Reminders.ts, FetchTransport.ts): the error carries a typed `kind`
      // and the path, not a pre-formatted sentence.
      throw new ChiefdUnavailableError({
        kind: 'malformed-body',
        url: '',
        path: '/v1/docs/queue',
        status: response.status
      })
    }
    const current = isRecord(body.current) ? body.current : undefined
    return {
      depth: body.depth,
      oldestEnqueuedMs: typeof body.oldestEnqueuedMs === 'number' ? body.oldestEnqueuedMs : null,
      deadlineMs: typeof body.deadlineMs === 'number' ? body.deadlineMs : null,
      ...(current &&
      typeof current.name === 'string' &&
      typeof current.enqueuedMs === 'number' &&
      (current.class === 'small' || current.class === 'normal' || current.class === 'reconcile')
        ? {
            current: {
              name: current.name,
              class: current.class,
              enqueuedMs: current.enqueuedMs
            }
          }
        : {})
    }
  }

  /** True iff the health endpoint answered at all, regardless of what it
   * said — the transport not throwing is the whole signal. */
  async reachable(): Promise<boolean> {
    try {
      await this.transport.get('/v1/docs/health')
      return true
    } catch {
      return false
    }
  }

  /** The runtime-identity probe: `GET /v1/docs/health` then, only when that
   * answered `ok`, `GET /v1/docs/runtime` to prove mode + company. Reports
   * what chiefd actually said, unchanged — including a runtime identity that
   * does not match what the caller expected; matching is the caller's own
   * decision, never a tolerance rule in here. */
  async probe(): Promise<HealthProbe> {
    let response
    try {
      response = await this.transport.get('/v1/docs/health')
    } catch (error) {
      return { ok: false, reason: error instanceof Error ? error.message : String(error) }
    }

    const body = parseJsonBody(response.body)
    if (typeof body === 'undefined') {
      // Answered, but no/invalid JSON body — still a live endpoint, so keep
      // the status without a reason.
      return { ok: false, httpStatus: response.status }
    }
    const reason = isRecord(body) && typeof body.status === 'string' ? body.status : undefined
    const ok = response.status === 200 && isRecord(body) && body.status === 'ok'
    if (!ok) return { ok, httpStatus: response.status, reason }

    try {
      const runtimeResponse = await this.transport.get('/v1/docs/runtime')
      if (runtimeResponse.status < 200 || runtimeResponse.status >= 300) {
        return { ok, httpStatus: response.status, reason }
      }
      const runtime = parseDocsRuntime(parseJsonBody(runtimeResponse.body))
      if (!runtime) return { ok, httpStatus: response.status, reason }
      return {
        ok,
        httpStatus: response.status,
        reason,
        runtimeMode: runtime.mode,
        company: runtime.company
      }
    } catch {
      return { ok, httpStatus: response.status, reason }
    }
  }
}
