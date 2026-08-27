/**
 * chiefd's doc-change feed, adapted for the page.
 *
 * # Why this is not a byte pipe
 *
 * Forwarding chiefd's stream verbatim would connect, heartbeat, look perfectly
 * healthy, and deliver nothing the browser can act on. Three real differences:
 *
 *   - chiefd names the frame `doc-change`; the page's client matches `doc` and
 *     silently drops anything else;
 *   - chiefd's payload is its Rust struct verbatim (`updated_at`), and the
 *     page parses `updatedAt`, so the frame would fail validation and be
 *     dropped without a word;
 *   - chiefd sends `store: "*"` for a whole-company drop. A page that has
 *     never heard of `*` would ignore the one frame that says everything it
 *     is showing is gone.
 *
 * So the adaptation is real work, and this is the ONE place it happens —
 * server-side, per connection, in front of a browser that then only ever sees
 * one spelling. `@chief/chiefing`'s own watcher performs the identical `*`
 * fan-out for the tmux side; this is that rule for the web side, not a second
 * opinion about what a frame means.
 *
 * The translation is a pure function over parsed frames, so every one of those
 * three rules is testable without a socket.
 */
import { operatorAuthorizationHeaders } from '@/server/OperatorBearer'
import type { SseFrame } from '@/types/Sse'
import { isNullish } from '@/utils/Nullish'
import { createSseFrameParser } from '@/utils/SseFrames'

/** chiefd's frame name for a store mutation. */
const CHIEFD_DOC_EVENT = 'doc-change'
/** The name the page matches. */
const PAGE_DOC_EVENT = 'doc'
/** chiefd's whole-company drop: every store for the companyKey is gone. */
const WILDCARD_STORE = '*'

function record(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== 'object' || isNullish(value) || Array.isArray(value)) return undefined
  return Object.fromEntries(Object.entries(value))
}

/** One frame's JSON body, or nothing. */
function frameBody(frame: SseFrame): Record<string, unknown> | undefined {
  if (isNullish(frame.data)) return undefined
  try {
    return record(JSON.parse(frame.data))
  } catch {
    return undefined
  }
}

/** chiefd's payload in the page's spelling. */
function pageDoc(body: Record<string, unknown>, store: string, companyKey: string): string {
  const payload = {
    // The company handle the page asked with, echoed back so a frame matches
    // what the browser holds. chiefd's own frame carries none, so this field is
    // invented here and named for what it carries.
    companyKey,
    store,
    seq: body.seq,
    // The rename that the page's own schema requires. chiefd serializes its
    // Rust struct verbatim; nothing between here and the browser would have
    // corrected it.
    updatedAt: body.updated_at,
    removed: body.removed
  }
  /* eslint-disable lucy/no-json-stringify */
  // A WIRE frame: this is the SSE body a browser parses, so it must be real
  // JSON rather than a tree helper's rendering of one.
  return JSON.stringify(payload)
  /* eslint-enable lucy/no-json-stringify */
}

/**
 * One chiefd frame as zero or more frames for the page.
 *
 * `stores` is what this connection asked for, and it is needed only to expand
 * `*`: the page must be told that EACH store it is watching is gone, because
 * it has no rule for a wildcard and would otherwise ignore the whole-company
 * drop entirely.
 */
export function translateFeedFrame(
  frame: SseFrame,
  companyKey: string,
  stores: readonly string[]
): string[] {
  // `reorg` means the page's cursor predates chiefd's retained ring, so it
  // must resync. Carried through under its own name, which the page already
  // matches — a resync trigger, not an unhealthy channel.
  if (frame.event === 'reorg') return ['event: reorg\ndata: {}\n\n']
  if (frame.event !== CHIEFD_DOC_EVENT) return []

  const body = frameBody(frame)
  if (isNullish(body)) return []
  const store = typeof body.store === 'string' ? body.store : undefined
  if (isNullish(store)) return []

  // The page's own company handle, echoed back unchanged so a frame matches
  // what the browser is holding.
  const expanded = store === WILDCARD_STORE ? stores : [store]
  return expanded.map(
    (name) =>
      `id: ${String(body.seq ?? '')}\nevent: ${PAGE_DOC_EVENT}\ndata: ${pageDoc(body, name, companyKey)}\n\n`
  )
}

/**
 * Open chiefd's feed and translate it, frame by frame.
 *
 * The upstream connection is bound to the caller's signal: a browser going
 * away must close chiefd's feed too, or the daemon holds one per page load
 * forever.
 */
export async function companyFeed(options: {
  endpoint: { url: string; dir: string }
  companyKey: string
  stores: readonly string[]
  after?: string
  signal: AbortSignal
}): Promise<ReadableStream<Uint8Array>> {
  const upstream = new URL('/v1/docs/watch', options.endpoint.url)
  upstream.searchParams.set('companyKey', options.companyKey)
  upstream.searchParams.set('stores', options.stores.join(','))
  if (!isNullish(options.after)) upstream.searchParams.set('after', options.after)

  // AUTHENTICATED, and it is the one caller here that cannot use
  // `ChiefdClient`: an event stream must be forwarded as bytes, so this holds a
  // raw `fetch` and attaches the header itself.
  //
  // The bearer is attached when the stream is OPENED. There is deliberately no
  // re-acquire here: a 401 is answered before the stream exists, and a token
  // that dies mid-stream produces no status code at all — the daemon simply
  // drops the connection. Re-authenticating a DROPPED stream before re-dialling
  // (and resuming at `after=<seq>`) is A4's design point, not this packet's.
  const response = await fetch(upstream, {
    headers: {
      accept: 'text/event-stream',
      ...(await operatorAuthorizationHeaders(options.endpoint.url, options.endpoint.dir))
    },
    signal: options.signal
  })
  if (!response.ok || isNullish(response.body)) {
    throw new Error(`chiefd answered ${response.status} for the change feed`)
  }

  const parser = createSseFrameParser()
  const decoder = new TextDecoder()
  const encoder = new TextEncoder()
  return response.body.pipeThrough(
    new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        // The parser is incremental because a network chunk is not
        // frame-aligned: a frame can arrive split across two reads, and
        // translating half a frame would drop the change it carried.
        for (const frame of parser.push(decoder.decode(chunk, { stream: true }))) {
          for (const out of translateFeedFrame(frame, options.companyKey, options.stores)) {
            controller.enqueue(encoder.encode(out))
          }
        }
      }
    })
  )
}
