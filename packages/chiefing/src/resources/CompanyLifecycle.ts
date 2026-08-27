/**
 * The company-lifecycle client: create, boot and stop a company.
 *
 * Rust authority: `apps/chiefd/crates/chief-cli/src/host/` (`chief host`), whose
 * routes this is the only client for.
 *
 * # Why this is not on `ChiefdClient`
 *
 * `ChiefdClient` is bound to ONE company's daemon URL, and every resource on it
 * answers questions about that company. Create has no company yet, and boot and
 * stop are precisely the operations that decide whether one is running — asking
 * a company's own daemon to start itself is not a shape that exists. This talks
 * to the resident lifecycle surface instead, which is per-BOX, so it is its own
 * client with its own base URL, exactly as `DiscoveryClient` is for beacond.
 *
 * # Phases are pushed, not derived
 *
 * `create()` and `boot()` return async generators that yield each phase as
 * chiefd emits it and RETURN the terminal result. That shape is deliberate: a
 * caller writes `const result = yield* client.create(input)` and gets both the
 * narration and the outcome without a second callback, and `for await` gives it
 * ordinary `break` semantics that close the underlying stream.
 *
 * Nothing here parses a log line, and nothing polls. The retired path spawned
 * the CLI, read its stdout and rebuilt phases with a regular expression, which
 * made a log format a wire contract; every value below is a field chiefd chose
 * to send.
 *
 * # Mandate 3
 *
 * This client decides nothing. It does not derive a slug, does not know what a
 * phase means, does not retry a refused launch and does not judge whether a
 * company came up — chiefd answers all of that, and the client's whole job is
 * to carry the answer across the process boundary with its structure intact.
 */

import { CompanyLifecycleRefusalError } from '@/Errors'
import { isNullish } from '@/Nullish'
import { readSseFrames } from '@/sse/SseFrames'
import type {
  CompanyLaunchResult,
  CompanyLifecyclePhase,
  CompanyStopResult,
  CreateCompanyInput
} from '@/types/CompanyLifecycle'

/** The compiled-in `chief host` address. Loopback, because — like beacond —
 * the surface has no auth: every caller is the same user on the same box. The
 * port sits below beacond's and outside the company-daemon port walk, so a
 * walking company daemon can never land on it — the same reasoning that chose
 * beacond's own port. This is NOT a chiefd URL fallback (ruling D1): a
 * company's daemon is still only ever found through beacond, and this address
 * exists because a company that does not exist yet has no registration to be
 * found through. */
export const DEFAULT_CHIEFD_HOST_URL = 'http://127.0.0.1:8789'

/** Env var naming a non-default `chief host`. Read by CALLERS and passed in —
 * chiefing never touches the ambient environment itself. */
export const CHIEFD_HOST_URL_ENV = 'CHIEFD_HOST_URL'

/** The trimmed `CHIEFD_HOST_URL` when it is a valid http:/https: URL, else
 * [`DEFAULT_CHIEFD_HOST_URL`]. Pure over an explicit record, exactly like
 * `beacondUrlFromEnvironment`, and for the same reason: a resident per-box
 * service cannot be found through the registry it is used to populate, so its
 * address is configuration rather than a discovery answer. */
export function chiefdHostUrlFromEnvironment(
  environment: Readonly<Record<string, string | undefined>>
): string {
  const trimmed = (environment[CHIEFD_HOST_URL_ENV] ?? '').trim()
  if (trimmed.length === 0) return DEFAULT_CHIEFD_HOST_URL
  try {
    const parsed = new URL(trimmed)
    return parsed.protocol === 'http:' || parsed.protocol === 'https:'
      ? trimmed
      : DEFAULT_CHIEFD_HOST_URL
  } catch {
    return DEFAULT_CHIEFD_HOST_URL
  }
}

/** How long the initial POST may take to produce response headers. The stream
 * itself is unbounded — a launch legitimately runs for minutes — so this bounds
 * only "did the surface answer at all". */
const CONNECT_TIMEOUT_MS = 10_000

/** Non-streaming requests (`stop`) get a whole-request budget. A stop that has
 * not answered in a minute is not going to. */
const REQUEST_TIMEOUT_MS = 60_000

/** The terminal event names, by verb. A refusal always arrives as `failed`
 * regardless of verb, so a caller needs one error path, not one per verb. */
const TERMINAL = { create: 'created', boot: 'booted' } as const

function isRecord(value: unknown): value is Record<string, unknown> {
  return !isNullish(value) && typeof value === 'object' && !Array.isArray(value)
}

function stringField(record: Record<string, unknown>, field: string): string | undefined {
  const value = record[field]
  return typeof value === 'string' ? value : undefined
}

/** A malformed frame is a refusal, never a silently dropped one. On a
 * long-lived document feed dropping an unreadable frame is right — the next
 * one supersedes it. Here every frame is the only one of its kind, so dropping
 * one would hang a caller forever waiting for a step that already went past. */
function refuseMalformed(what: string, raw: string): never {
  throw new CompanyLifecycleRefusalError({
    code: 'lifecycle-malformed',
    detail: `chiefd sent a ${what} this client cannot read: ${raw}`
  })
}

function parsePhase(raw: string): CompanyLifecyclePhase {
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    refuseMalformed('phase frame', raw)
  }
  if (!isRecord(parsed)) refuseMalformed('phase frame', raw)
  const phase = stringField(parsed, 'phase')
  const slug = stringField(parsed, 'slug')
  if (isNullish(phase) || isNullish(slug)) refuseMalformed('phase frame', raw)
  // The name is carried through exactly as chiefd sent it. A phase this client
  // has not been taught about is still a real step that really happened, and
  // coercing or dropping it would make adding one to chiefd a breaking change.
  // `isCompanyLifecyclePhaseName` is how a caller narrows when it wants to.
  return { phase, slug, detail: stringField(parsed, 'detail') ?? '' }
}

function parseLaunchResult(raw: string): CompanyLaunchResult {
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    refuseMalformed('terminal frame', raw)
  }
  if (!isRecord(parsed)) refuseMalformed('terminal frame', raw)
  const slug = stringField(parsed, 'slug')
  const key = stringField(parsed, 'key')
  const dir = stringField(parsed, 'dir')
  const url = stringField(parsed, 'url')
  const chiefPersonId = stringField(parsed, 'chiefPersonId')
  // `key` is REQUIRED, not defaulted. It is the only field on this frame that
  // addresses the company, so a frame without one is malformed rather than
  // partially usable — defaulting it to `''` would produce a link to `/c/`.
  if (
    isNullish(slug) ||
    isNullish(key) ||
    isNullish(dir) ||
    isNullish(url) ||
    isNullish(chiefPersonId)
  ) {
    refuseMalformed('terminal frame', raw)
  }
  return { slug, key, dir, url, chiefPersonId, session: stringField(parsed, 'session') ?? '' }
}

function refusalFrom(raw: string): CompanyLifecycleRefusalError {
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return new CompanyLifecycleRefusalError({ code: 'lifecycle-failed', detail: raw })
  }
  if (!isRecord(parsed)) {
    return new CompanyLifecycleRefusalError({ code: 'lifecycle-failed', detail: raw })
  }
  return new CompanyLifecycleRefusalError({
    code: stringField(parsed, 'code') ?? 'lifecycle-failed',
    detail: stringField(parsed, 'detail') ?? raw
  })
}

/** Decoded text chunks from a `fetch` response body. Extracted so the stream
 * reader below never touches a `Response` and can be driven from a plain
 * iterable in tests.
 *
 * The `finally` runs when a consumer abandons the loop (`break`, `throw`, or
 * the generator being `.return()`ed), which is what closes the socket rather
 * than leaving a half-read response pinned for the process's lifetime.
 * `cancel()` first, then release: releasing a lock on a stream still being fed
 * leaves the body unread but open. */
async function* decodeBody(body: ReadableStream<Uint8Array>): AsyncGenerator<string> {
  const reader = body.getReader()
  const decoder = new TextDecoder()
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) return
      if (value && value.length > 0) yield decoder.decode(value, { stream: true })
    }
  } finally {
    try {
      await reader.cancel()
    } catch {
      // Already cancelled, or the stream errored — either way there is nothing
      // left to close, and a teardown must not throw over the caller's own
      // reason for tearing down.
    }
    reader.releaseLock()
  }
}

export class CompanyLifecycleClient {
  private readonly hostUrl: string
  private readonly fetchImpl: typeof fetch

  /**
   * @param hostUrl base URL of `chief host` (default `CHIEFD_HOST_BIND`,
   *   `http://127.0.0.1:8789`). Held as a plain address, like beacond's, for
   *   the same reason: a per-box service cannot be discovered through the
   *   registry it is used to populate.
   */
  constructor(options: { hostUrl: string; fetchImpl?: typeof fetch }) {
    if (!options.hostUrl.trim()) {
      throw new Error('CompanyLifecycleClient requires a non-empty chief host URL')
    }
    this.hostUrl = options.hostUrl.replace(/\/+$/, '')
    this.fetchImpl = options.fetchImpl ?? fetch
  }

  /**
   * Create a company. Yields each phase; returns the launch result.
   *
   * The slug is chiefd's answer, not an argument — read it off the returned
   * result (and off every phase frame, which carries it from the first one).
   */
  async *create(
    input: CreateCompanyInput
  ): AsyncGenerator<CompanyLifecyclePhase, CompanyLaunchResult> {
    return yield* this.stream('/v1/company/create', input, TERMINAL.create)
  }

  /** Boot an already-created company. Yields each phase; returns the result. */
  async *boot(slug: string): AsyncGenerator<CompanyLifecyclePhase, CompanyLaunchResult> {
    return yield* this.stream('/v1/company/boot', { slug }, TERMINAL.boot)
  }

  /**
   * Stop a company's runtime, preserving every byte of durable state.
   *
   * Not a stream: it has no step a caller can act on differently, and chiefd
   * answers it with one JSON object.
   */
  async stop(slug: string): Promise<CompanyStopResult> {
    const response = await this.post(
      '/v1/company/stop',
      { slug },
      AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      'json'
    )
    const text = await response.text()
    if (!response.ok) throw refusalFrom(text)
    let parsed: unknown
    try {
      parsed = JSON.parse(text)
    } catch {
      refuseMalformed('stop result', text)
    }
    if (!isRecord(parsed)) refuseMalformed('stop result', text)
    return {
      mode: stringField(parsed, 'mode') ?? 'already-stopped',
      slug: stringField(parsed, 'slug') ?? slug,
      session: stringField(parsed, 'session') ?? '',
      sessionStopped: parsed.sessionStopped === true,
      daemonStopped: parsed.daemonStopped === true
    }
  }

  /** `GET /v1/health`. `true` iff the surface answered `{"status":"ok"}`.
   * Never throws — "is it there" must be answerable without a try/catch. */
  async health(): Promise<boolean> {
    try {
      const response = await this.fetchImpl(`${this.hostUrl}/v1/health`, {
        signal: AbortSignal.timeout(CONNECT_TIMEOUT_MS)
      })
      if (!response.ok) return false
      const parsed: unknown = JSON.parse(await response.text())
      return isRecord(parsed) && parsed.status === 'ok'
    } catch {
      return false
    }
  }

  private async post(
    path: string,
    body: unknown,
    signal: AbortSignal,
    accept: 'json' | 'sse'
  ): Promise<Response> {
    /* eslint-disable lucy/no-json-stringify */
    // @tribes-terminal/foundation's tree serializer is private to the sibling
    // `terminal` repo and is not a dependency here — the same exemption
    // FetchTransport.ts and DiscoveryClient.ts already take, for the same
    // reason.
    return this.fetchImpl(`${this.hostUrl}${path}`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: accept === 'sse' ? 'text/event-stream' : 'application/json'
      },
      body: JSON.stringify(body),
      signal
    })
    /* eslint-enable lucy/no-json-stringify */
  }

  /**
   * POST, then read the response as phases plus exactly one terminal frame.
   *
   * # The timeout bounds the CONNECT, never the stream
   *
   * `AbortSignal.timeout()` aborts the whole `fetch`, body included — using it
   * here would have killed every launch at ten seconds, which is well inside a
   * cold daemon start. The controller below is cancelled the moment response
   * headers arrive, so what is bounded is "did the surface answer at all"; a
   * launch then runs for as long as it runs, and liveness is the server's
   * `:hb` heartbeat rather than a deadline this side invents.
   *
   * The `finally` aborts on any exit — a refusal, a `break`, or the caller
   * abandoning the generator — so the socket is released rather than left
   * pinned by a half-read response.
   *
   * A stream that ends without a terminal frame is a refusal, not a success:
   * chiefd always sends one, so its absence means the connection died mid-
   * launch, and reporting that as "created" would be the worst possible
   * answer.
   */
  private async *stream(
    path: string,
    body: unknown,
    terminal: string
  ): AsyncGenerator<CompanyLifecyclePhase, CompanyLaunchResult> {
    const connection = new AbortController()
    const connectTimer = setTimeout(() => connection.abort(), CONNECT_TIMEOUT_MS)
    connectTimer.unref?.()
    try {
      const response = await this.post(path, body, connection.signal, 'sse')
      clearTimeout(connectTimer)
      if (!response.ok) throw refusalFrom(await response.text())
      if (isNullish(response.body)) {
        throw new CompanyLifecycleRefusalError({
          code: 'lifecycle-failed',
          detail: `chief host answered ${path} with no body`
        })
      }
      let result: CompanyLaunchResult | undefined
      for await (const frame of readSseFrames(decodeBody(response.body))) {
        if (frame.event === 'comment') continue
        if (frame.event === 'phase') {
          yield parsePhase(frame.data)
          continue
        }
        if (frame.event === 'failed') throw refusalFrom(frame.data)
        if (frame.event === terminal) {
          result = parseLaunchResult(frame.data)
          // Do not `break`: letting the loop finish lets the reader observe
          // the stream's own end, which is how the connection is released.
          // chiefd closes it immediately after the terminal frame, so this
          // costs one more `read()` that returns done.
          continue
        }
        // An unknown event name is ignored, forward-compatibly — the terminal
        // frame is what ends this loop, and a future control frame must not.
      }
      if (isNullish(result)) {
        throw new CompanyLifecycleRefusalError({
          code: 'lifecycle-abandoned',
          detail: `chief host closed the ${path} stream without a terminal frame`
        })
      }
      return result
    } finally {
      clearTimeout(connectTimer)
      connection.abort()
    }
  }
}
