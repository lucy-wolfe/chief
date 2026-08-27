// A4 (the design record ruling 4): the SSE reader presents the
// caller's bearer, and — the one real design point — **a dropped stream is
// RE-AUTHENTICATED, not merely reconnected.**
//
// The rule exists because "re-acquire on 401" does not map onto a long-lived
// stream: the 401 is answered before the stream opens, and a token that dies
// mid-stream produces no status code at all — the daemon just drops the
// connection. So every reconnect invalidates the cached bearer and acquires a
// fresh one before dialling, rather than replaying the header that was just
// refused. `after=<seq>` is what makes doing that on EVERY drop safe, and one
// test below pins that the cursor survives the re-authentication.
//
// Deliberately NOT tested, because it is deliberately not implemented:
// telling "the token died" apart from "the network blinked". Treating every
// drop identically is both simpler and strictly safer.
import {
  createFakeBearer,
  createFakeOpener,
  docChange,
  flushMicrotasks,
  sseFrame
} from '@test/sse/FakeSseStream'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { subscribeSse } from '@/sse/SseHub'
import { SseWatcher } from '@/sse/SseWatcher'
import type { SseBearerProvider, SseStreamOpener } from '@/types/Watch'

const watchers: SseWatcher[] = []
afterEach(() => {
  for (const w of watchers.splice(0)) w.close()
  vi.useRealTimers()
})
beforeEach(() => {
  vi.useFakeTimers()
})

function watch(options: { open: SseStreamOpener; bearer?: SseBearerProvider }): SseWatcher {
  const watcher = new SseWatcher({
    url: 'http://chiefd.local',
    slug: 'acme',
    stores: ['activity'],
    onEvent: () => {},
    openStream: options.open,
    ...(options.bearer ? { bearer: options.bearer } : {})
  })
  watchers.push(watcher)
  return watcher
}

describe('the SSE reader presents its caller`s credential', () => {
  it('sends the acquired bearer beside accept on the very first dial', async () => {
    const { open, headers } = createFakeOpener()
    const bearer = createFakeBearer()
    watch({ open, bearer: bearer.provider })
    await flushMicrotasks()
    expect(headers[0]).toEqual({
      accept: 'text/event-stream',
      Authorization: 'Bearer token-1'
    })
  })

  it('a caller with no credential still dials, carrying accept alone', async () => {
    const { open, headers } = createFakeOpener()
    watch({ open })
    await flushMicrotasks()
    expect(headers[0]).toEqual({ accept: 'text/event-stream' })
  })

  it('a failed acquisition dials token-less rather than killing the reader — the daemon is the authority', async () => {
    const { open, calls, headers } = createFakeOpener()
    const bearer = createFakeBearer({ failing: true })
    watch({ open, bearer: bearer.provider })
    await flushMicrotasks()
    expect(calls.length).toBe(1)
    expect(headers[0]).toEqual({ accept: 'text/event-stream' })
  })

  it('does NOT invalidate on the first dial — re-authentication is a property of the reconnect', async () => {
    const { open } = createFakeOpener()
    const bearer = createFakeBearer()
    watch({ open, bearer: bearer.provider })
    await flushMicrotasks()
    expect(bearer.invalidations).toBe(0)
    expect(bearer.acquisitions).toBe(1)
  })
})

describe('a dropped stream is RE-AUTHENTICATED, not replayed', () => {
  it('a stream that ends invalidates the cached bearer and presents a FRESH token', async () => {
    const { open, headers, connections } = createFakeOpener()
    const bearer = createFakeBearer()
    watch({ open, bearer: bearer.provider })
    await flushMicrotasks()
    expect(headers[0]?.Authorization).toBe('Bearer token-1')

    connections[0]?.end()
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(1000) // backoff floor
    await flushMicrotasks()

    // The load-bearing assertion: the second dial does NOT carry `token-1`.
    // A reconnect that merely re-dialled would replay the header the daemon
    // had just stopped honouring, and would 401-with-no-status forever.
    expect(headers[1]?.Authorization).toBe('Bearer token-2')
    expect(bearer.invalidations).toBe(1)
    expect(bearer.acquisitions).toBe(2)
  })

  it('invalidates BEFORE reading the new header, so the fresh dial cannot observe the stale token', async () => {
    const { open, connections } = createFakeOpener()
    const bearer = createFakeBearer()
    watch({ open, bearer: bearer.provider })
    await flushMicrotasks()
    connections[0]?.end()
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(1000)
    await flushMicrotasks()
    expect(bearer.order).toEqual(['authHeader', 'invalidate', 'authHeader'])
  })

  it('a heartbeat-timeout teardown re-authenticates too — every drop is the same drop', async () => {
    const { open, headers } = createFakeOpener()
    const bearer = createFakeBearer()
    watch({ open, bearer: bearer.provider })
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(45_000) // silence past the ceiling
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(1000) // backoff floor
    await flushMicrotasks()
    expect(headers[1]?.Authorization).toBe('Bearer token-2')
  })

  it('a re-authenticated reconnect still resumes at after=<lastSeq> — that is what makes it safe on every drop', async () => {
    const { open, calls, headers, connections } = createFakeOpener()
    const bearer = createFakeBearer()
    watch({ open, bearer: bearer.provider })
    await flushMicrotasks()
    connections[0]?.push(sseFrame({ id: 7, event: 'doc-change', data: docChange({ seq: 7 }) }))
    await flushMicrotasks()
    connections[0]?.end()
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(1000)
    await flushMicrotasks()
    expect(calls[1]).toContain('after=7')
    expect(headers[1]?.Authorization).toBe('Bearer token-2')
  })

  it('the multiplexing hub carries the credential through to the one connection it opens', async () => {
    const { open, headers } = createFakeOpener()
    const bearer = createFakeBearer()
    const subscription = subscribeSse({
      url: 'http://chiefd.local/hub-bearer',
      slug: 'acme-hub-bearer',
      stores: ['activity'],
      onEvent: () => {},
      bearer: bearer.provider,
      openStream: open
    })
    await flushMicrotasks()
    try {
      expect(headers[0]?.Authorization).toBe('Bearer token-1')
    } finally {
      subscription.close()
    }
  })

  it('addStores is a deliberate widening, not a drop — it keeps the working token', async () => {
    const { open, headers } = createFakeOpener()
    const bearer = createFakeBearer()
    const watcher = watch({ open, bearer: bearer.provider })
    await flushMicrotasks()
    expect(watcher.addStores(['supervision'])).toBe(true)
    await flushMicrotasks()
    expect(bearer.invalidations).toBe(0)
    expect(headers[1]?.Authorization).toBe('Bearer token-1')
  })
})
