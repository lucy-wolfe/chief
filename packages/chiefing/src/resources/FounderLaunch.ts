// The Founder's company-creation client.
//
// Company genesis lives in Rust (`apps/chiefd/crates/chief-cli/src/
// genesis.rs`). `chief` publishes a loopback endpoint into the Founder
// pane's environment as `CHIEFD_FOUNDER_URL`, and this is the only way to
// reach it.
//
// # What this replaced
//
// The Founder extension used to spawn a CLI subprocess — `triber
// _create-and-boot --spec <tmpfile> --founder-bootstrap <tmpfile>` — writing
// two temporary JSON files for another process to read back, and inferring
// success from an exit code. That bridge is deleted: the retired command
// namespace, the hidden subcommand, the temp files and the subprocess are all
// gone, and creating a company is one typed call into chiefd like every other
// business decision.
// Relative, never the `@/` alias: this file is in the extension-runtime
// closed graph, which is FLATTENED into a Pi home where no tsconfig path
// mapping exists (`ClosedGraph.test.ts`). `SseFrames.ts` is itself relative-only,
// so it joins the closure cleanly.
import { readSseFrames } from '../sse/SseFrames.js'
import type {
  FounderLaunchInput,
  FounderLaunchPhase,
  FounderLaunchResult
} from '../types/OrgDocs.js'

/** The environment variable `chief` publishes its genesis endpoint as. */
export const FOUNDER_URL_ENV = 'CHIEFD_FOUNDER_URL'

/** The one genesis route. */
const LAUNCH_PATH = '/v1/founder/launch'

/**
 * How long this client waits for chiefd to answer a launch.
 *
 * A bound, not a deadline anybody expects to reach. `fetch` carried no
 * `AbortSignal` at all, so a chiefd that accepted the connection and then
 * stalled left the Founder waiting for ever with nothing on screen.
 *
 * Sized from chiefd's OWN budgets on this route so it can only fire after
 * chiefd has itself given up: beacond discovery is bounded at 5 s
 * (`discovery.rs` ENSURE_BUDGET) and the company daemon start at 15 s
 * (`daemon.rs` DEFAULT_START_BUDGET), around a genesis transaction and a
 * handover that a healthy launch completes in about 2.6 s. Two minutes is far
 * outside every one of those and still finite.
 *
 * A shorter bound would be actively harmful. Genesis is one transaction and
 * this client never retries, so aborting early does not undo anything — it
 * only destroys this side's knowledge of whether a company exists.
 */
const LAUNCH_BUDGET_MS = 120_000

/**
 * Resolve the genesis endpoint from the pane's environment.
 *
 * Throws rather than guessing an address: a Founder pane that was not started
 * by `chief` has no company-creation authority, and inventing a localhost
 * port would either fail confusingly or reach an unrelated listener.
 */
export function founderUrlFromEnvironment(environment: Record<string, string | undefined>): string {
  const url = environment[FOUNDER_URL_ENV]?.trim()
  if (!url) {
    throw new Error(
      'Founder launch is missing its ChiefD genesis endpoint. Start the Founder with `chief`.'
    )
  }
  return url.replace(/\/+$/, '')
}

/** Create a company through chiefd. */
export class FounderLaunchClient {
  readonly #url: string

  constructor(options: { readonly url: string }) {
    this.#url = options.url.replace(/\/+$/, '')
  }

  /**
   * `POST /v1/founder/launch`, reading the phase stream it answers with.
   *
   * One request, one answer, no retry: chiefd's genesis is a single
   * transaction that either committed or did not, and a retry against a
   * partially-created company is exactly the state Mandate 4 exists to make
   * impossible. A non-2xx answer carries chiefd's own refusal text, which is
   * what the Founder shows the human — never a status code alone.
   *
   * # Why this reads a stream now (#1051)
   *
   * The route used to answer one JSON body at the end. A launch takes minutes —
   * one measured at 4m34s — and the Founder pane showed a bare spinner for all
   * of it, so the human concluded, reasonably, that it had hung. chiefd emits a
   * phase for every step and `apps/web` has always rendered them; this route
   * threw them away. `onPhase` is how they reach the pane.
   *
   * The CONTRACT IS UNCHANGED: this still resolves only when the company is up,
   * and still rejects with chiefd's own text. Returning early would trade a
   * visible wait for an invisible failure.
   */
  async launch(
    input: FounderLaunchInput,
    onPhase?: (phase: FounderLaunchPhase) => void
  ): Promise<FounderLaunchResult> {
    let response: Response
    try {
      response = await fetch(`${this.#url}${LAUNCH_PATH}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', accept: 'text/event-stream' },
        // Bounded, so a stalled chiefd cannot hang the Founder for ever. See
        // LAUNCH_BUDGET_MS for why it is this large and why it must never be
        // small. The budget covers the WHOLE stream, not just the first byte:
        // phases now prove chiefd is alive, but liveness is not progress, and a
        // launch that narrates itself for ever is still a launch that never
        // finishes.
        signal: AbortSignal.timeout(LAUNCH_BUDGET_MS),
        /* eslint-disable lucy/no-json-stringify */
        // Request wire text — the same exemption every other chiefing client
        // takes to serialize a body for `fetch` (see `FetchTransport.ts`).
        body: JSON.stringify({
          name: input.name.trim(),
          purpose: input.purpose.trim()
        })
        /* eslint-enable lucy/no-json-stringify */
      })
    } catch (error) {
      // A timeout is NOT a refusal, and reporting it as one would be the more
      // damaging lie. chiefd's genesis is a single transaction that may well
      // have committed while this side stopped listening, so the one honest
      // statement is that the answer is unknown, plus the command that settles
      // it. Every other transport failure keeps its own cause.
      if (error instanceof Error && error.name === 'TimeoutError') {
        throw new Error(
          `ChiefD did not answer the launch within ${Math.round(LAUNCH_BUDGET_MS / 1000)}s. ` +
            'The company may or may not have been created — this client cannot tell. ' +
            'Check with `chief ls` before trying again; do not assume the launch failed.',
          { cause: error }
        )
      }
      throw error
    }
    if (!response.ok) {
      const text = await response.text()
      throw new Error(refusalDetail(text) ?? `ChiefD refused the launch (${response.status}).`)
    }
    if (!response.body) {
      throw new Error('ChiefD answered the launch with no body to read.')
    }
    return readLaunchStream(response.body, onPhase)
  }
}

/**
 * Drain the launch stream: report every phase, resolve on the terminal frame.
 *
 * The frame decoding is `readSseFrames` — the same reader `CompanyLifecycleClient`
 * drives for `/v1/company/create`, which is the same stream shape from the same
 * chiefd module. A second hand-rolled parser here would be a second opinion
 * about where one frame ends, and `decodeBody`'s `cancel()`-then-release order
 * is exactly the detail such a copy gets wrong: releasing the lock on a body
 * still being fed leaves the socket open.
 *
 * A launch that ends without a terminal frame REJECTS, never resolves. The
 * whole point of this surface is that an operator is never left guessing, and
 * resolving on a truncated stream would hand the model a company it cannot
 * prove exists.
 */
async function readLaunchStream(
  body: ReadableStream<Uint8Array>,
  onPhase?: (phase: FounderLaunchPhase) => void
): Promise<FounderLaunchResult> {
  let launched: FounderLaunchResult | undefined
  for await (const frame of readSseFrames(decodeBody(body))) {
    // The keep-alive comment is liveness, not an event.
    if (frame.event === 'comment') continue
    if (frame.event === 'phase') {
      const phase = asPhase(frame.data)
      // A malformed progress line is SKIPPED, deliberately: it is the one frame
      // class whose loss costs nothing, and failing a launch that is still
      // running because its narration glitched would be the worse trade.
      if (phase) onPhase?.(phase)
      continue
    }
    if (frame.event === 'failed') {
      throw new Error(refusalDetail(frame.data) ?? 'ChiefD refused the launch.')
    }
    if (frame.event === 'launched') {
      const parsed: unknown = parseJson(frame.data)
      if (!isLaunchResult(parsed)) {
        throw new Error('ChiefD answered the launch with a body this client cannot read.')
      }
      launched = parsed
      // Not a `break`: letting the loop reach the stream's own end is what
      // releases the connection. chiefd closes it right after this frame.
      continue
    }
    // An unknown event name is ignored, forward-compatibly.
  }
  if (!launched) {
    throw new Error(
      'ChiefD ended the launch stream without reporting an outcome. The company may or may not ' +
        'have been created; check with `chief ls` before launching the same name again.'
    )
  }
  return launched
}

/** Decoded text chunks from a response body.
 *
 * `cancel()` before `releaseLock()`: releasing the lock on a stream still being
 * fed leaves the body unread but open. Same order, same reason, as
 * `CompanyLifecycle`'s. */
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
      // Already cancelled, or the stream errored — a teardown must not throw
      // over the caller's own reason for tearing down.
    }
    reader.releaseLock()
  }
}

/** Parse a frame body, answering `undefined` rather than throwing. */
function parseJson(raw: string): unknown {
  // Declared `unknown` rather than asserted: `JSON.parse` returns `any`, and a
  // type assertion is banned repo-wide. Same shape as `CompanyLifecycle`'s.
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return undefined
  }
  return parsed
}

/** Narrow a phase frame. An unreadable one is skipped by the caller. */
function asPhase(raw: string): FounderLaunchPhase | undefined {
  const value = parseJson(raw)
  if (!value || typeof value !== 'object') return undefined
  if (!('phase' in value) || typeof value.phase !== 'string') return undefined
  const slug = 'slug' in value && typeof value.slug === 'string' ? value.slug : ''
  const detail = 'detail' in value && typeof value.detail === 'string' ? value.detail : ''
  return { phase: value.phase, slug, detail }
}

/** chiefd's refusal text, when the body carries one. */
function refusalDetail(body: string): string | undefined {
  try {
    const parsed: unknown = JSON.parse(body)
    if (parsed && typeof parsed === 'object' && 'detail' in parsed) {
      const detail: unknown = parsed.detail
      if (typeof detail === 'string' && detail.trim()) return detail
    }
  } catch {
    // A non-JSON body is still the most specific thing chiefd said.
  }
  return body.trim() || undefined
}

/** Narrow chiefd's answer before a caller reports a company as launched. */
function isLaunchResult(value: unknown): value is FounderLaunchResult {
  if (!value || typeof value !== 'object') return false
  // `handoffWarning` is optional and must stay readable when present: it is the
  // only signal that the operator was NOT taken to the CEO, and a narrowing
  // that dropped it would let the Founder announce a handoff that never ran.
  if ('handoffWarning' in value && typeof value.handoffWarning !== 'string') return false
  return (
    'slug' in value &&
    typeof value.slug === 'string' &&
    'url' in value &&
    typeof value.url === 'string' &&
    'chiefPersonId' in value &&
    typeof value.chiefPersonId === 'string' &&
    'session' in value &&
    typeof value.session === 'string'
  )
}
