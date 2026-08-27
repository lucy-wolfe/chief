import {
  BeacondUnavailableError,
  CompanyNotRunningError,
  DiscoveryRefusalError,
  UnknownCompanyError
} from '../Errors.js'
import { isNullish } from '../Nullish.js'
import { fetchFailureDetail } from '../transport/FetchFailure.js'
import type { CompanyRow, DiscoveryClientOptions } from '../types/Discovery.js'
import { parseCompanyRow } from './Company.js'

const DEFAULT_TIMEOUT_MS = 2000

function isRecord(value: unknown): value is Record<string, unknown> {
  return !isNullish(value) && typeof value === 'object' && !Array.isArray(value)
}

/** Classify a rejection from `fetch()` structurally — never by message text.
 * A timed-out `AbortSignal.timeout` surfaces as a `TimeoutError`/`AbortError`
 * DOMException; every other fetch-level rejection (connection refused, DNS,
 * reset before send) is `'unreachable'` — the contract exposes only these
 * two kinds for a fetch-level failure, and "could not be reached" is the
 * closer statement than inventing a third client-only kind nothing else in
 * the taxonomy has. */
function classifyFetchError(error: unknown): 'unreachable' | 'timeout' {
  const name = error instanceof Error ? error.name : ''
  return name === 'TimeoutError' || name === 'AbortError' ? 'timeout' : 'unreachable'
}

interface RefusalBody {
  readonly code: string
  readonly message: string
}

function asRefusalBody(json: unknown): RefusalBody | undefined {
  if (!isRecord(json)) return undefined
  return typeof json.code === 'string' && typeof json.message === 'string'
    ? { code: json.code, message: json.message }
    : undefined
}

function foundBodyCompany(json: unknown): { found: boolean; company: unknown } | undefined {
  if (!isRecord(json) || typeof json.found !== 'boolean') return undefined
  return { found: json.found, company: json.company }
}

function companiesBody(json: unknown): readonly unknown[] | undefined {
  if (!isRecord(json) || !Array.isArray(json.companies)) return undefined
  return json.companies
}

function createdBody(json: unknown): boolean | undefined {
  return isRecord(json) && typeof json.created === 'boolean' ? json.created : undefined
}

function deletedBody(json: unknown): boolean | undefined {
  return isRecord(json) && typeof json.deleted === 'boolean' ? json.deleted : undefined
}

function deregisteredBody(json: unknown): boolean | undefined {
  return isRecord(json) && typeof json.deregistered === 'boolean' ? json.deregistered : undefined
}

interface SentResponse {
  readonly status: number
  readonly json: unknown
}

/**
 * The company registry client. There is no register and no heartbeat here on
 * purpose: publishing a LOCATION is chiefd's own business and lives in Rust.
 * TypeScript owns the company's EXISTENCE (create/delete) and nothing about
 * its location except clearing a location it has already proven dead.
 *
 * Every method is ONE on-demand request. There is no cache, no retry ladder,
 * no polling loop.
 */
export class DiscoveryClient {
  private readonly beacondUrl: string
  private readonly timeoutMs: number
  private readonly fetchImpl: typeof fetch

  constructor(options: DiscoveryClientOptions) {
    this.beacondUrl = options.beacondUrl
    this.timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS
    this.fetchImpl = options.fetchImpl ?? fetch
  }

  /** One request, no retry. Network/timeout failures throw
   * `BeacondUnavailableError` here, uniformly, for every caller. A
   * non-throwing JSON parse failure is reported as `json: undefined` so the
   * CALLER can classify it as `'http-error'` (a non-2xx, non-JSON body — an
   * HTML error page) or `'malformed-body'` (a 2xx whose body is not JSON),
   * per the wire kind table. */
  private async send(method: 'GET' | 'POST', path: string, body?: unknown): Promise<SentResponse> {
    let response: Response
    try {
      /* eslint-disable lucy/no-json-stringify */
      // @tribes-terminal/foundation (toJsonTreeString/ensureJsonTreeString) is
      // private to the sibling `terminal` repo this lint rule was ported from
      // and is not a dependency anywhere in this workspace — same exemption
      // FetchTransport.ts (E2-S1) already takes for the same reason.
      response = await this.fetchImpl(`${this.beacondUrl}${path}`, {
        method,
        headers: isNullish(body) ? undefined : { 'content-type': 'application/json' },
        body: isNullish(body) ? undefined : JSON.stringify(body),
        signal: AbortSignal.timeout(this.timeoutMs)
      })
      /* eslint-enable lucy/no-json-stringify */
    } catch (error) {
      throw new BeacondUnavailableError({
        kind: classifyFetchError(error),
        beacondUrl: this.beacondUrl,
        path,
        detail: fetchFailureDetail(error),
        cause: error
      })
    }
    const text = await response.text()
    let json: unknown
    try {
      json = text.length === 0 ? undefined : JSON.parse(text)
    } catch {
      json = undefined
    }
    return { status: response.status, json }
  }

  /** Any non-2xx that is not the documented `{code, message}` refusal shape
   * is `'http-error'`. Called only from the non-2xx branch of each method. */
  private httpError(path: string, status: number): never {
    throw new BeacondUnavailableError({
      kind: 'http-error',
      beacondUrl: this.beacondUrl,
      path,
      status
    })
  }

  private malformedBody(path: string, status?: number, cause?: unknown): never {
    throw new BeacondUnavailableError({
      kind: 'malformed-body',
      beacondUrl: this.beacondUrl,
      path,
      status,
      cause
    })
  }

  /** Throw the right error for a non-2xx response: the documented `{code,
   * message}` refusal shape becomes `DiscoveryRefusalError`, anything else
   * becomes `'http-error'`. Every method but `deregister` (its own one
   * documented exception) and `health` (which never throws) routes its
   * non-2xx branch through this. */
  private refuseOrHttpError(path: string, status: number, json: unknown): never {
    const refusal = asRefusalBody(json)
    if (refusal) {
      throw new DiscoveryRefusalError({ status, code: refusal.code, message: refusal.message })
    }
    this.httpError(path, status)
  }

  /** GET /v1/lookup?dir=. `undefined` when no company occupies that DIRECTORY
   * (not merely "not running" — a stopped company still resolves, with no
   * location).
   *
   * `dir` must be the canonical absolute path; beacond compares it byte for
   * byte and refuses a relative or empty one with `400 bad-request`. */
  async lookup(dir: string): Promise<CompanyRow | undefined> {
    const path = `/v1/lookup?dir=${encodeURIComponent(dir)}`
    const { status, json } = await this.send('GET', path)
    if (status < 200 || status >= 300) this.refuseOrHttpError(path, status, json)
    const body = foundBodyCompany(json)
    if (!body) this.malformedBody(path, status)
    if (!body.found) return undefined
    try {
      return parseCompanyRow(body.company)
    } catch (error) {
      this.malformedBody(path, status, error)
    }
  }

  /**
   * GET /v1/lookup?dir=, but a miss is an error rather than `undefined`.
   *
   * A miss carries nothing but `found:false`. The predecessor read a
   * "did you mean" suggestion and an operator-facing message out of the
   * response, because it looked a company up by a slug an operator had TYPED.
   * Nobody types a directory — a caller sends the one it is standing in — so
   * the nearest other directory on the box answers no question, and beacond
   * stopped computing one.
   */
  async requireCompany(dir: string): Promise<CompanyRow> {
    const row = await this.lookup(dir)
    if (isNullish(row)) throw new UnknownCompanyError({ dir })
    return row
  }

  /** GET /v1/list. Every company on this box, ordered by display name and then
   * by directory, running or not. Empty array when none have been created. */
  async list(): Promise<readonly CompanyRow[]> {
    const path = '/v1/list'
    const { status, json } = await this.send('GET', path)
    if (status < 200 || status >= 300) this.refuseOrHttpError(path, status, json)
    const companies = companiesBody(json)
    if (!companies) this.malformedBody(path, status)
    try {
      return companies.map((company) => parseCompanyRow(company))
    } catch (error) {
      this.malformedBody(path, status, error)
    }
  }

  /**
   * POST /v1/company/create — ONE atomic upsert INSERT on the DIRECTORY, the
   * FIRST durable step of a create (ruling D24/F27).
   *
   * Resolves `true` when this call inserted the row and `false` when the
   * directory already held a company — an idempotent success, which is what
   * makes a create re-runnable. A second create carrying a different `slug`
   * RENAMES the company: the slug is a display word, and the only thing a
   * caller can legitimately have changed.
   *
   * **There is no `slug-taken` refusal any more.** It was deleted with the
   * uniqueness it enforced — two directories may hold companies with the same
   * display name, and both rows are legitimate.
   *
   * `key` is minted by the CALLER — `sha256(dir)[..12]` — and recorded
   * verbatim. beacond never derives it, so it has exactly one producer.
   */
  async createCompany(input: {
    readonly dir: string
    readonly key: string
    readonly slug: string
  }): Promise<boolean> {
    const path = '/v1/company/create'
    const { status, json } = await this.send('POST', path, {
      dir: input.dir,
      key: input.key,
      slug: input.slug
    })
    if (status < 200 || status >= 300) this.refuseOrHttpError(path, status, json)
    const created = createdBody(json)
    if (isNullish(created)) this.malformedBody(path, status)
    return created
  }

  /**
   * POST /v1/company/delete — ONE atomic DELETE, idempotent. Resolves
   * `false` when there was no row (already gone: success, not an error).
   */
  async deleteCompany(input: { readonly dir: string }): Promise<boolean> {
    const path = '/v1/company/delete'
    const { status, json } = await this.send('POST', path, { dir: input.dir })
    if (status < 200 || status >= 300) this.refuseOrHttpError(path, status, json)
    const deleted = deletedBody(json)
    if (isNullish(deleted)) this.malformedBody(path, status)
    return deleted
  }

  /**
   * POST /v1/deregister. CLEARS a company's location columns; never deletes
   * the company. Resolves `false` when beacond refused (409 pid mismatch) or
   * there was nothing to clear — a refusal here means a live daemon
   * reclaimed the company, which is good news, not an error.
   */
  async deregister(input: { readonly dir: string; readonly pid: number }): Promise<boolean> {
    const path = '/v1/deregister'
    const { status, json } = await this.send('POST', path, { dir: input.dir, pid: input.pid })
    if (status < 200 || status >= 300) {
      const refusal = asRefusalBody(json)
      // The ONE documented exception: a 409 pid-mismatch means a live daemon
      // reclaimed the company — resolve false, never throw.
      if (status === 409 && refusal?.code === 'pid-mismatch') return false
      this.refuseOrHttpError(path, status, json)
    }
    const deregistered = deregisteredBody(json)
    if (isNullish(deregistered)) this.malformedBody(path, status)
    return deregistered
  }

  /** GET /v1/health. `true` iff 200 with `{"status":"ok"}`. Never throws. */
  async health(): Promise<boolean> {
    try {
      const { status, json } = await this.send('GET', '/v1/health')
      if (status !== 200) return false
      return isRecord(json) && json.status === 'ok'
    } catch {
      return false
    }
  }
}

/**
 * The one way to turn a company DIRECTORY into a chiefd base URL **through the
 * registry**.
 *
 * This is the box-wide question — "where is the daemon for that directory over
 * there" — and it is what `chief ls` and `apps/web` ask. A process standing
 * INSIDE the directory does not ask it: it reads
 * `<dir>/.chief/run/daemon.json` ({@link readDaemonRendezvous}), which carries
 * the same url plus the company key and puts no registry on the path between a
 * command and its own company.
 *
 * Two distinct failures, two distinct errors, because callers must tell them
 * apart — one is "create it", the other is "boot it":
 *   - no company row at all      -> `UnknownCompanyError`
 *   - a row with no `url`        -> `CompanyNotRunningError`
 *
 * There is NO fixed-port fallback, deliberately: the old `DEFAULT_CHIEFD_URL`
 * fallback is what let a bootstrap for company A silently adopt company B's
 * live daemon. "I don't know where it is" must be an error, not a guess.
 */
export async function resolveCompanyChiefdUrl(
  dir: string,
  discovery: DiscoveryClient
): Promise<string> {
  const row = await discovery.lookup(dir)
  if (isNullish(row)) {
    throw new UnknownCompanyError({ dir })
  }
  if (isNullish(row.url)) {
    throw new CompanyNotRunningError({ dir })
  }
  return row.url
}
