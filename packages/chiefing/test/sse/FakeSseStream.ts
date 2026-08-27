// Test-only fake for the `SseStreamOpener` seam
// (`(url) => Promise<AsyncIterable<string>>`). Drives raw SSE bytes into
// `SseWatcher`/`subscribeSse` under fake timers, per #775's own
// test-writing guidance (no real server/socket).
import { isNullish } from '@/Nullish'
import type { SseBearerProvider, SseStreamOpener } from '@/types/Watch'

// File-local only (lucy/no-exported-type-outside-types-dir): callers get
// this shape by inference from `createFakeOpener()`'s return type, never by
// importing the interface name directly.
interface FakeSseConnection {
  /** Deliver one raw text chunk to the watcher (may split a frame mid-line;
   * the watcher's own buffering must reassemble it). */
  push(text: string): void
  /** End the stream (server closed it / transport error) — the watcher
   * treats this as a disconnect and reconnects per the backoff ladder. */
  end(): void
  /** Whether the watcher (or hub) asked this connection to stop — proves the
   * forced-teardown path (heartbeat timeout, close(), addStores()) actually
   * reaches the stream, not just the watcher's internal state. */
  wasReturned(): boolean
  readonly url: string
  /** The headers the watcher decided to present on THIS dial — the only place
   * the re-authenticating reconnect is observable. */
  readonly headers: Record<string, string>
}

/**
 * A controllable fake `SseStreamOpener`. Each call to the returned function
 * opens a new logical connection (recorded in `.connections`); the caller
 * drives it via `push`/`end` and can inspect `.calls` (every requested URL,
 * in order — used to assert `after=<seq>` cursor replay) without any real
 * network or timer dependency.
 */
export function createFakeOpener(): {
  open: SseStreamOpener
  calls: string[]
  /** Every dial's headers, in the same order as `calls`. */
  headers: Array<Record<string, string>>
  connections: FakeSseConnection[]
} {
  const calls: string[] = []
  const headerCalls: Array<Record<string, string>> = []
  const connections: FakeSseConnection[] = []

  const open: SseStreamOpener = (url: string, headers: Record<string, string>) => {
    calls.push(url)
    headerCalls.push({ ...headers })
    let resolveNext: ((result: IteratorResult<string>) => void) | undefined
    const queued: string[] = []
    let ended = false
    let returned = false

    const iterable: AsyncIterable<string> = {
      [Symbol.asyncIterator]() {
        return {
          next(): Promise<IteratorResult<string>> {
            if (queued.length > 0) {
              const value = queued.shift()
              if (!isNullish(value)) return Promise.resolve({ value, done: false })
            }
            if (ended) return Promise.resolve({ value: undefined, done: true })
            return new Promise<IteratorResult<string>>((resolve) => {
              resolveNext = resolve
            })
          },
          return(): Promise<IteratorResult<string>> {
            returned = true
            ended = true
            if (resolveNext) {
              const resolve = resolveNext
              resolveNext = undefined
              resolve({ value: undefined, done: true })
            }
            return Promise.resolve({ value: undefined, done: true })
          }
        }
      }
    }

    const connection: FakeSseConnection = {
      url,
      headers: { ...headers },
      push(text: string) {
        if (resolveNext) {
          const resolve = resolveNext
          resolveNext = undefined
          resolve({ value: text, done: false })
        } else {
          queued.push(text)
        }
      },
      end() {
        ended = true
        if (resolveNext) {
          const resolve = resolveNext
          resolveNext = undefined
          resolve({ value: undefined, done: true })
        }
      },
      wasReturned: () => returned
    }
    connections.push(connection)
    return Promise.resolve(iterable)
  }

  return { open, calls, headers: headerCalls, connections }
}

/**
 * A controllable stand-in for the pane's `AgentTokenManager`, shaped exactly
 * like `SseBearerProvider`. Each acquisition after an `invalidate()` mints a
 * NEW token value, so a test can tell a fresh credential apart from a replayed
 * one by reading the header alone.
 */
export function createFakeBearer(options: { failing?: boolean } = {}): {
  provider: SseBearerProvider
  acquisitions: number
  invalidations: number
  /** The order of `invalidate` / `authHeader` calls, so a test can pin that
   * the drop invalidates BEFORE the new token is read, not after. */
  order: string[]
} {
  const order: string[] = []
  const state = { acquisitions: 0, invalidations: 0, order, cached: '' }
  const provider = {
    authHeader(): Promise<Record<string, string> | undefined> {
      state.order.push('authHeader')
      if (options.failing) return Promise.resolve(undefined)
      if (!state.cached) {
        state.acquisitions += 1
        state.cached = `token-${state.acquisitions}`
      }
      return Promise.resolve({ Authorization: `Bearer ${state.cached}` })
    },
    invalidate(): void {
      state.order.push('invalidate')
      state.invalidations += 1
      state.cached = ''
    }
  }
  return {
    provider,
    get acquisitions() {
      return state.acquisitions
    },
    get invalidations() {
      return state.invalidations
    },
    get order() {
      return state.order
    }
  }
}

/** Renders one SSE frame exactly as the docstore watch endpoint documents it. */
export function sseFrame(options: { id?: number; event?: string; data: unknown }): string {
  let out = ''
  if (!isNullish(options.id)) out += `id: ${options.id}\n`
  if (options.event) out += `event: ${options.event}\n`
  /* eslint-disable lucy/no-json-stringify */
  // @tribes-terminal/foundation (toJsonTreeString/ensureJsonTreeString) is
  // not a dependency anywhere in this workspace (see RowStores.ts's
  // matching disable block) — this renders a raw SSE `data:` line's JSON
  // body for a test fixture, not a production wire write.
  out += `data: ${JSON.stringify(options.data)}\n\n`
  /* eslint-enable lucy/no-json-stringify */
  return out
}

export const HEARTBEAT_COMMENT = ': heartbeat\n\n'

/** A minimal, valid `SseDocChangeEvent` wire body for fixture frames. */
export function docChange(
  overrides: Partial<Record<string, unknown>> = {}
): Record<string, unknown> {
  return {
    seq: 1,
    slug: 'acme',
    store: 'activity',
    updated_at: '2026-08-04T00:00:00.000Z',
    removed: false,
    ...overrides
  }
}

/** Let every pending microtask (promise chain) flush without advancing fake
 * timers — needed after `push`/`end` since those settle promises but don't
 * touch any timer. */
export async function flushMicrotasks(): Promise<void> {
  // The watcher's consumption loop hops through several microtasks per item
  // (Promise.race, the fake iterator's own Promise.resolve, the loop
  // continuation) — enough ticks to drain a small burst fully, not just the
  // first queued item.
  for (let i = 0; i < 20; i += 1) await Promise.resolve()
}
