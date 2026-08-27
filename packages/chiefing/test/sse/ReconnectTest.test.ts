// #775 load-bearing subtlety: reconnect backoff `min(1000·2^n, 30000)`,
// reset on proof of life, `after=<lastSeq>` cursor replay on every
// reconnect, and an initial `after` honored on the very first connect.
import { createFakeOpener, docChange, flushMicrotasks, sseFrame } from '@test/sse/FakeSseStream'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { computeBackoffDelayMs, SseWatcher } from '@/sse/SseWatcher'

const watchers: SseWatcher[] = []
afterEach(() => {
  for (const w of watchers.splice(0)) w.close()
  vi.useRealTimers()
})
beforeEach(() => {
  vi.useFakeTimers()
})

describe('computeBackoffDelayMs', () => {
  it('doubles from the floor and pins at the ceiling', () => {
    expect(computeBackoffDelayMs(0, 1000, 30000)).toBe(1000)
    expect(computeBackoffDelayMs(1, 1000, 30000)).toBe(2000)
    expect(computeBackoffDelayMs(2, 1000, 30000)).toBe(4000)
    expect(computeBackoffDelayMs(5, 1000, 30000)).toBe(30000) // 32000 capped
    expect(computeBackoffDelayMs(10, 1000, 30000)).toBe(30000)
  })
})

describe('reconnect ladder', () => {
  it('URL includes no after= on the very first connect with none supplied', async () => {
    const { open, calls } = createFakeOpener()
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    expect(calls[0]).not.toContain('after=')
  })

  it('an initial after seeds the very first connect URL', async () => {
    const { open, calls } = createFakeOpener()
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      after: 42,
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    expect(calls[0]).toContain('after=42')
  })

  it('reconnects with after=<lastSeq> once an event has been seen', async () => {
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
    connections[0]?.push(sseFrame({ id: 5, event: 'doc-change', data: docChange({ seq: 5 }) }))
    await flushMicrotasks()
    connections[0]?.end() // disconnect
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(1000) // backoff floor
    await flushMicrotasks()
    expect(calls[1]).toContain('after=5')
  })

  it('backoff doubles across consecutive failures without any proof of life', async () => {
    const { open, calls, connections } = createFakeOpener()
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      backoffInitialMs: 1000,
      backoffMaxMs: 30000,
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    // Fail three times in a row with no data ever flowing.
    connections[0]?.end()
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(1000)
    await flushMicrotasks()
    connections[1]?.end()
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(2000)
    await flushMicrotasks()
    connections[2]?.end()
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(4000)
    await flushMicrotasks()
    expect(calls.length).toBe(4)
  })

  it('backoff resets to the floor the moment the channel proves live again', async () => {
    const { open, calls, connections } = createFakeOpener()
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      backoffInitialMs: 1000,
      backoffMaxMs: 30000,
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    // First failure — attempt becomes 1, next backoff would be 2000ms if it
    // were NOT reset by the proof of life below.
    connections[0]?.end()
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(1000)
    await flushMicrotasks()
    expect(calls.length).toBe(2) // the first reconnect landed at the 1000ms floor
    // Prove life: a heartbeat comment resets the attempt counter to 0.
    connections[1]?.push(': heartbeat\n\n')
    await flushMicrotasks()
    connections[1]?.end()
    await flushMicrotasks()
    // If backoff had NOT reset, this 1000ms advance would be short of the
    // 2000ms the un-reset ladder would demand, and no third call would land.
    await vi.advanceTimersByTimeAsync(1000)
    await flushMicrotasks()
    expect(calls.length).toBe(3)
  })
})
