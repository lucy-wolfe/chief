// #775 load-bearing subtlety: 45s silence (default heartbeatTimeoutMs) tears
// the channel down and reconnects; the heartbeat re-arms on every chunk (so
// a channel that is merely QUIET but still receiving frames never fires);
// exactly one pending timer exists at any moment (proven indirectly: firing
// the ceiling exactly once triggers exactly one teardown+reconnect, not a
// repeating interval that would fire again on its own).
import { createFakeOpener, docChange, flushMicrotasks, sseFrame } from '@test/sse/FakeSseStream'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { SseWatcher } from '@/sse/SseWatcher'

const watchers: SseWatcher[] = []
afterEach(() => {
  for (const w of watchers.splice(0)) w.close()
  vi.useRealTimers()
})
beforeEach(() => {
  vi.useFakeTimers()
})

describe('heartbeat liveness', () => {
  it('45s of total silence tears the connection down and reconnects', async () => {
    const { open, calls, connections } = createFakeOpener()
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    expect(calls.length).toBe(1)
    await vi.advanceTimersByTimeAsync(45_000)
    await flushMicrotasks()
    // The forced teardown must reach the actual stream (proves this isn't
    // just an internal state flip that leaves the old connection dangling).
    expect(connections[0]?.wasReturned()).toBe(true)
    // Backoff floor (default 1000ms) then fires the reconnect.
    await vi.advanceTimersByTimeAsync(1000)
    await flushMicrotasks()
    expect(calls.length).toBe(2)
  })

  it('a comment heartbeat resets the deadline — no reconnect at 45s after one', async () => {
    const { open, calls, connections } = createFakeOpener()
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(40_000)
    await flushMicrotasks()
    connections[0]?.push(': heartbeat\n\n')
    await flushMicrotasks()
    // 40s more would be 80s total from connect — well past 45s — but only
    // 40s since the reset, so the channel must still be alive.
    await vi.advanceTimersByTimeAsync(40_000)
    await flushMicrotasks()
    expect(calls.length).toBe(1) // never reconnected
  })

  it('a real doc-change frame also resets the heartbeat deadline', async () => {
    const { open, calls, connections } = createFakeOpener()
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(40_000)
    await flushMicrotasks()
    connections[0]?.push(sseFrame({ id: 1, event: 'doc-change', data: docChange({ seq: 1 }) }))
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(40_000)
    await flushMicrotasks()
    expect(calls.length).toBe(1)
  })

  it('a custom heartbeatTimeoutMs is honored', async () => {
    const { open, calls } = createFakeOpener()
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      heartbeatTimeoutMs: 5_000,
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(5_000)
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(1_000) // backoff floor
    await flushMicrotasks()
    expect(calls.length).toBe(2)
  })

  it('onChannelStateChange reports dead exactly once on the heartbeat timeout, not repeatedly', async () => {
    const { open } = createFakeOpener()
    const states: string[] = []
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      onChannelStateChange: (s) => states.push(s),
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(45_000)
    await flushMicrotasks()
    expect(states.filter((s) => s === 'dead').length).toBe(1)
  })
})
