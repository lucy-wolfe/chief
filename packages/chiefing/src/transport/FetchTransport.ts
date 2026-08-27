import { ChiefdUnavailableError } from '../Errors.js'
import type {
  AuthHeaderProvider,
  AuthInvalidate,
  HttpResponse,
  HttpTransport
} from '../types/Transport.js'
import { describeFetchFailure, fetchFailureDetail } from './FetchFailure.js'
import { awaitedDelay, CONNECT_RETRY_BACKOFFS_MS } from './RetryPolicy.js'

/**
 * The client's patience, and the ONLY definition of it. Every production
 * `FetchTransport` inherits this number; nothing else supplies one, and
 * `scripts/test/client-observable-wait.test.mjs` fails if anything starts to.
 *
 * It must outlive every bound chiefd can hold a request behind — not the
 * reverse. chiefd's writer actor is one thread per company and a mutation
 * queued behind deep work waits its turn up to
 * `actor::MUTATION_QUEUE_DEADLINE` (30s), the single bounded wait in the whole
 * system, after which the job is reaped and answered `429 Busy` — the one
 * status whose entire meaning is "back off and ask again"
 * (`isTransientChiefdError`).
 *
 * At 10_000 this client abandoned that request 20 seconds early, EVERY time,
 * and the damage was not a lost answer. An abort raises `kind: 'timeout'`,
 * which `isTransientChiefdError` deliberately classifies as NON-transient
 * (retrying a timeout can double-apply a write), so the caller stopped and
 * reported a failure — while the mutation stayed queued and then ran and
 * committed. The caller believed the write did not happen; the write happened.
 * That is worse than either a clean success or a clean failure.
 *
 * How long chiefd may hold a mutation is chiefd's decision, and 30s of queueing
 * under a multi-second reconcile is a deliberate contention policy. A client
 * that gives up first and then misreports the outcome is the defect. 35s leaves
 * the queue deadline 5s of room for connect, the JSON round trip and the wire —
 * the guard's floor is 2s.
 */
const DEFAULT_TIMEOUT_MS = 35_000

type FetchFailureKind = 'unreachable' | 'timeout' | 'unknown'

/** Fetch rejections carrying one of these `cause`/`error` codes never
 * reached a handler — retrying cannot double-apply anything. Structural
 * (code inspection), never a message regex.
 *
 * `'ConnectionRefused'` (Bun's own mixed-case code, distinct from Node's
 * uppercase `ECONNREFUSED`) was missing here until #953: a real dead
 * chiefd's connection refusal on this runtime carries `error.name ===
 * 'Error'` and `error.code === 'ConnectionRefused'` — verified directly
 * against a real refused connection, not assumed from documentation — so
 * neither the name check below nor the (Node-shaped) code set matched it.
 * Every transient-outage caller (`isTransientChiefdError`) silently saw the
 * RAW, unwrapped fetch error instead of a classified `ChiefdUnavailableError`
 * with `kind: 'unreachable'`, which is exactly the property a real-chiefd
 * outage-and-recovery test needs to assert on. */
const UNREACHABLE_CODES = new Set(['ECONNREFUSED', 'ENOTFOUND', 'ECONNRESET', 'ConnectionRefused'])

function errorCode(error: unknown): string | undefined {
  if (!error || typeof error !== 'object') return undefined
  if ('code' in error && typeof error.code === 'string') return error.code
  if ('cause' in error) return errorCode(error.cause)
  return undefined
}

/** Classify a rejection from `fetch()` structurally: a timed-out
 * `AbortSignal.timeout` surfaces as a `TimeoutError`/`AbortError` DOMException;
 * a pre-send connect refusal surfaces (directly or via `error.cause`, per
 * undici/Bun's fetch wrapping) as one of `UNREACHABLE_CODES`, or as an
 * error whose `name` is itself `'ConnectionRefused'` (some runtimes set
 * this instead of `.code`; Bun today sets `.code`, not `.name` — both are
 * checked since neither is guaranteed by spec). Everything else is
 * `'unknown'` and is rethrown unmodified rather than mis-classified. */
function classifyFetchError(error: unknown): FetchFailureKind {
  const name = error instanceof Error ? error.name : ''
  if (name === 'TimeoutError' || name === 'AbortError') return 'timeout'
  if (name === 'ConnectionRefused') return 'unreachable'
  const code = errorCode(error)
  if (typeof code === 'string' && UNREACHABLE_CODES.has(code)) return 'unreachable'
  return 'unknown'
}

/** The only production transport. Async fetch with AbortSignal.timeout,
 * content-type: application/json on POST, per-request auth headers (a
 * single re-acquire-and-retry on 401, driven by `authInvalidate`), and the
 * connect-refusal-only retry ladder. Failure classification is structural (fetch error
 * cause.code), never message regex. */
export class FetchTransport implements HttpTransport {
  constructor(
    protected readonly baseUrl: string,
    protected readonly timeoutMs?: number,
    protected readonly authHeaderProvider?: AuthHeaderProvider,
    protected readonly authInvalidate?: AuthInvalidate
  ) {}

  async post(path: string, body: unknown): Promise<HttpResponse> {
    /* eslint-disable lucy/no-json-stringify */
    // @tribes-terminal/foundation (toJsonTreeString/ensureJsonTreeString) is
    // private to the sibling `terminal` repo this lint rule was ported from
    // and is not a dependency anywhere in this workspace. The story Contract
    // (E2-S1) requires the POST body to be exactly `JSON.stringify(body)` —
    // FetchTransportTest.test.ts asserts the literal serialized output.
    const serialized = JSON.stringify(body)
    /* eslint-enable lucy/no-json-stringify */
    return this.send('POST', path, serialized)
  }

  async get(path: string): Promise<HttpResponse> {
    return this.send('GET', path)
  }

  private async send(method: 'GET' | 'POST', path: string, body?: string): Promise<HttpResponse> {
    const first = await this.sendOnce(method, path, body)
    // A 401 is the ONLY status worth a second attempt, and it is worth exactly
    // one. The daemon's HS256 secret is ephemeral unless a secret file was
    // provisioned, so a chiefd restart rotates it and every cached bearer in
    // every surviving agent becomes garbage at the same instant. Before this,
    // the client cached its token for the life of the process and the server
    // documented that "clients simply re-acquire on the resulting 401" — a
    // recovery neither side actually performed, so a restart silently 401ed
    // every org tool call from every running agent until each was respawned.
    //
    // Re-acquiring on the client is the right half to fix: it also covers a
    // legitimate key rotation and a token expiring mid-life, neither of which a
    // persisted secret would help with.
    //
    // Bounded to one retry so a genuinely unauthorized identity fails fast
    // instead of looping against the challenge endpoint.
    if (first.status !== 401 || !this.authHeaderProvider || !this.authInvalidate) {
      return first
    }
    this.authInvalidate()
    return this.sendOnce(method, path, body)
  }

  private async sendOnce(
    method: 'GET' | 'POST',
    path: string,
    body?: string
  ): Promise<HttpResponse> {
    const headers: Record<string, string> = {}
    if (method === 'POST') headers['content-type'] = 'application/json'
    if (this.authHeaderProvider) {
      const authHeaders = await this.authHeaderProvider()
      if (authHeaders) Object.assign(headers, authHeaders)
    }

    const url = `${this.baseUrl}${path}`
    const timeoutMs = this.timeoutMs ?? DEFAULT_TIMEOUT_MS

    for (let attempt = 0; ; attempt += 1) {
      try {
        const response = await fetch(url, {
          method,
          headers,
          body,
          signal: AbortSignal.timeout(timeoutMs)
        })
        const responseBody = await response.text()
        return { status: response.status, body: responseBody }
      } catch (error) {
        const kind = classifyFetchError(error)
        if (kind === 'timeout') {
          throw new ChiefdUnavailableError({
            kind: 'timeout',
            url: this.baseUrl,
            path,
            detail: fetchFailureDetail(error),
            cause: error
          })
        }
        if (kind === 'unreachable') {
          if (attempt < CONNECT_RETRY_BACKOFFS_MS.length) {
            await awaitedDelay(CONNECT_RETRY_BACKOFFS_MS[attempt])
            continue
          }
          throw new ChiefdUnavailableError({
            // The code and port, not just the kind. `unreachable at
            // http://127.0.0.1:8789/v1/...` already named the address; this
            // adds WHICH failure it was, which is the difference between "the
            // daemon is not running" and "DNS cannot resolve that name".
            kind: 'unreachable',
            url: this.baseUrl,
            path,
            detail: fetchFailureDetail(error),
            cause: error
          })
        }
        // An unclassifiable rejection is still rethrown rather than
        // reclassified -- inventing a `kind` for a failure we did not
        // recognise is the defect this file's structural classification
        // exists to avoid. But it is rethrown with its own cause SPELLED
        // OUT: a bare `fetch failed` reaching an operator is the exact
        // symptom this packet is about, and the cause is on the object.
        throw wearingItsCause(error)
      }
    }
  }
}

/**
 * The same rejection, with its cause spelled into its message.
 *
 * A new Error would discard the type every `instanceof` above depends on, so
 * the original object is returned and only its `message` is rewritten — and
 * only when [`describeFetchFailure`] actually found something the message did
 * not already say. A frozen or getter-only `message` is left alone; a
 * diagnostic must never be the reason a request fails.
 */
function wearingItsCause(error: unknown): unknown {
  if (!(error instanceof Error)) return error
  const described = describeFetchFailure(error)
  if (described === error.message) return error
  try {
    error.message = described
  } catch {
    // Non-writable `message`. The cause is still on `error.cause`.
  }
  return error
}
