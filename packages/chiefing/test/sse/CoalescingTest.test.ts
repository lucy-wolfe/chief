// #775 load-bearing subtlety: at most one in-flight onEvent cycle per store —
// a burst of N events for one store while a cycle is pending collapses to
// exactly one follow-up (last-value-wins), independent stores proceed
// concurrently (never blocked by each other), and an onEvent rejection is
// swallowed so the watcher lives.
import { createFakeOpener, docChange, flushMicrotasks, sseFrame } from '@test/sse/FakeSseStream'
import { afterEach, describe, expect, it } from 'vitest'

import { SseWatcher } from '@/sse/SseWatcher'

const watchers: SseWatcher[] = []
afterEach(() => {
  for (const w of watchers.splice(0)) w.close()
})

/** Resolves when released — models a slow onEvent handler so the test can
 * push a burst while a cycle is genuinely still in flight. */
function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
  let resolve!: (v: T) => void
  const promise = new Promise<T>((res) => {
    resolve = res
  })
  return { promise, resolve }
}

function frame(seq: number, store: string): string {
  return sseFrame({ id: seq, event: 'doc-change', data: docChange({ seq, store }) })
}

describe('per-store coalescing', () => {
  it('a burst of N events for one store while onEvent is pending collapses to one follow-up', async () => {
    const { open, connections } = createFakeOpener()
    const calls: number[] = []
    const gates: Array<{ promise: Promise<void>; resolve: (v: void) => void }> = []
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: async (event) => {
        calls.push(event.seq)
        const gate = deferred<void>()
        gates.push(gate)
        await gate.promise
      },
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    connections[0]?.push(frame(1, 'activity'))
    await flushMicrotasks()
    expect(calls).toEqual([1]) // cycle 1 started, now blocked on gates[0]
    // A burst of 3 more events arrives while cycle 1 is still in flight.
    connections[0]?.push(frame(2, 'activity'))
    connections[0]?.push(frame(3, 'activity'))
    connections[0]?.push(frame(4, 'activity'))
    await flushMicrotasks()
    expect(calls).toEqual([1]) // still only one cycle running — burst is queued, not fanned out
    gates[0]?.resolve()
    await flushMicrotasks()
    // Exactly ONE follow-up cycle runs, and it sees the LAST event (4), not
    // 2 or 3 — a burst of N mid-cycle costs one follow-up, not N.
    expect(calls).toEqual([1, 4])
    gates[1]?.resolve()
    await flushMicrotasks()
    expect(calls).toEqual([1, 4]) // no third cycle — nothing more was pending
  })

  it('independent stores proceed concurrently, never blocked by each other', async () => {
    const { open, connections } = createFakeOpener()
    const started: string[] = []
    const gates = new Map<string, { promise: Promise<void>; resolve: (v: void) => void }>()
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity', 'supervision'],
      onEvent: async (event) => {
        started.push(event.store)
        const gate = deferred<void>()
        gates.set(event.store, gate)
        await gate.promise
      },
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    connections[0]?.push(frame(1, 'activity'))
    await flushMicrotasks()
    // 'activity' is now blocked mid-cycle, but 'supervision' must still start.
    connections[0]?.push(frame(2, 'supervision'))
    await flushMicrotasks()
    expect(started).toEqual(['activity', 'supervision'])
    gates.get('activity')?.resolve()
    gates.get('supervision')?.resolve()
    await flushMicrotasks()
  })

  it('an onEvent rejection is swallowed — the watcher keeps processing', async () => {
    const { open, connections } = createFakeOpener()
    const calls: number[] = []
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: (event) => {
        calls.push(event.seq)
        if (event.seq === 1) throw new Error('handler bug')
      },
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    connections[0]?.push(frame(1, 'activity'))
    await flushMicrotasks()
    connections[0]?.push(frame(2, 'activity'))
    await flushMicrotasks()
    expect(calls).toEqual([1, 2]) // the throw on 1 never stopped 2 from being delivered
  })
})
