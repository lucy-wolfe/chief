// #775 load-bearing subtlety (#31): the multiplexing hub — ONE connection per
// url|slug shared by every subscriber; addStores widens + reconnects
// preserving cursor; last unsubscribe closes the connection (idle -> zero,
// the HARD RULE); a different slug gets its own independent watcher.
import { createFakeOpener, docChange, flushMicrotasks, sseFrame } from '@test/sse/FakeSseStream'
import { afterEach, describe, expect, it } from 'vitest'

import { activeSseHubCount, subscribeSse } from '@/sse/SseHub'
import type { SseSubscription } from '@/types/Watch'

const subs: SseSubscription[] = []
afterEach(() => {
  for (const s of subs.splice(0)) s.close()
})

describe('subscribeSse multiplexing hub', () => {
  it('two subscribers to the same url|slug and store share exactly one connection', async () => {
    const { open, calls } = createFakeOpener()
    const eventsA: unknown[] = []
    const eventsB: unknown[] = []
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'],
        onEvent: (e) => {
          eventsA.push(e)
        },
        openStream: open
      })
    )
    await flushMicrotasks()
    expect(activeSseHubCount()).toBe(1)
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'], // same store — nothing new, must NOT reconnect
        onEvent: (e) => {
          eventsB.push(e)
        },
        openStream: open
      })
    )
    await flushMicrotasks()
    expect(activeSseHubCount()).toBe(1) // still one connection, not two
    expect(calls).toHaveLength(1) // only ONE openStream call for both subscribers
  })

  it('a doc-change event is delivered only to subscribers of that store', async () => {
    const { open, connections } = createFakeOpener()
    const eventsA: unknown[] = []
    const eventsB: unknown[] = []
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'],
        onEvent: (e) => {
          eventsA.push(e)
        },
        openStream: open
      })
    )
    await flushMicrotasks()
    // A NEW store widens the shared connection (its own behavior, proven in
    // the addStores test below); this test only needs to prove per-store
    // delivery filtering, so wait for that reconnect to settle first and
    // always read from the CURRENT (last) connection, not a superseded one.
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['supervision'],
        onEvent: (e) => {
          eventsB.push(e)
        },
        openStream: open
      })
    )
    await flushMicrotasks()
    const current = connections[connections.length - 1]
    current?.push(
      sseFrame({ id: 1, event: 'doc-change', data: docChange({ seq: 1, store: 'activity' }) })
    )
    await flushMicrotasks()
    expect(eventsA).toHaveLength(1)
    expect(eventsB).toHaveLength(0)
  })

  it('a wildcard subscriber receives ordinary changes from every store without widening an exact subscriber', async () => {
    const { open, connections } = createFakeOpener()
    const wildcardEvents: string[] = []
    const activityEvents: string[] = []
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['*'],
        onEvent: (event) => {
          wildcardEvents.push(event.store)
        },
        openStream: open
      })
    )
    await flushMicrotasks()
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'],
        onEvent: (event) => {
          activityEvents.push(event.store)
        },
        openStream: open
      })
    )
    await flushMicrotasks()
    const current = connections[connections.length - 1]
    current?.push(
      sseFrame({ id: 1, event: 'doc-change', data: docChange({ seq: 1, store: 'activity' }) })
    )
    current?.push(
      sseFrame({ id: 2, event: 'doc-change', data: docChange({ seq: 2, store: 'supervision' }) })
    )
    await flushMicrotasks()

    expect(wildcardEvents).toEqual(['activity', 'supervision'])
    expect(activityEvents).toEqual(['activity'])
  })

  it('addStores widens the shared connection: a second subscriber joining a NEW store reconnects preserving cursor', async () => {
    const { open, calls, connections } = createFakeOpener()
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'],
        onEvent: () => {},
        openStream: open
      })
    )
    await flushMicrotasks()
    // Establish a cursor.
    connections[0]?.push(
      sseFrame({ id: 3, event: 'doc-change', data: docChange({ seq: 3, store: 'activity' }) })
    )
    await flushMicrotasks()
    expect(calls).toHaveLength(1)
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['supervision'], // a genuinely new store — must widen + reconnect
        onEvent: () => {},
        openStream: open
      })
    )
    await flushMicrotasks()
    expect(calls).toHaveLength(2) // reconnected to widen the store set
    expect(calls[1]).toContain('stores=activity%2Csupervision')
    expect(calls[1]).toContain('after=3') // cursor preserved across the widen
  })

  it('a joining subscriber to an ALREADY-covered store set does not force a reconnect', async () => {
    const { open, calls } = createFakeOpener()
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'],
        onEvent: () => {},
        openStream: open
      })
    )
    await flushMicrotasks()
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'], // same store, nothing new
        onEvent: () => {},
        openStream: open
      })
    )
    await flushMicrotasks()
    expect(calls).toHaveLength(1)
  })

  it('last unsubscribe closes the connection — idle keeps nothing running', async () => {
    const { open, calls, connections } = createFakeOpener()
    const subA = subscribeSse({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      openStream: open
    })
    await flushMicrotasks()
    const subB = subscribeSse({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      openStream: open
    })
    await flushMicrotasks()
    expect(activeSseHubCount()).toBe(1)
    subA.close()
    expect(activeSseHubCount()).toBe(1) // subB still needs it
    subB.close()
    expect(activeSseHubCount()).toBe(0) // last subscriber gone — connection closed
    // The forced teardown reaching the real stream is asynchronous (the
    // watcher's consumption loop notices the abort on its next microtask
    // tick, not synchronously inside close()) — the hub-level bookkeeping
    // above is synchronous and already proven; this confirms the stream
    // itself is actually released, not just the accounting.
    await flushMicrotasks()
    expect(connections[calls.length - 1]?.wasReturned()).toBe(true)
  })

  it('a different slug gets its own independent connection', async () => {
    const { open, calls } = createFakeOpener()
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'],
        onEvent: () => {},
        openStream: open
      })
    )
    await flushMicrotasks()
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'beta',
        stores: ['activity'],
        onEvent: () => {},
        openStream: open
      })
    )
    await flushMicrotasks()
    expect(activeSseHubCount()).toBe(2)
    expect(calls).toHaveLength(2)
  })

  it('a late joiner is replayed the current channel state', async () => {
    const { open, connections } = createFakeOpener()
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'],
        onEvent: () => {},
        openStream: open
      })
    )
    await flushMicrotasks()
    connections[0]?.push(': heartbeat\n\n') // proves healthy
    await flushMicrotasks()
    const states: string[] = []
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'], // same store — no reconnect, so the late joiner needs the replay
        onEvent: () => {},
        onChannelStateChange: (s) => states.push(s),
        openStream: open
      })
    )
    await flushMicrotasks()
    expect(states).toEqual(['healthy'])
  })

  // #835 (ported from tests/sse-watcher-multiplex.test.ts's "reorg and
  // channel-state transitions fan out to every subscriber"): every other
  // test in this file uses exactly one subscriber past the initial join, so
  // none of them exercise SseHub's own fan-out loop
  // (`for (const sub of [...this.subscribers]) sub.onReorg?.()`) — this is
  // that multi-subscriber case, proving a SHARED connection's reorg reaches
  // every subscriber sharing it, not just the one that happened to trigger
  // the connection.
  it('a reorg on the shared connection fans out onReorg to every subscriber sharing it', async () => {
    const { open, connections } = createFakeOpener()
    const reorgsA: number[] = []
    const reorgsB: number[] = []
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'],
        onEvent: () => {},
        onReorg: () => reorgsA.push(1),
        openStream: open
      })
    )
    await flushMicrotasks()
    subs.push(
      subscribeSse({
        url: 'http://chiefd.local',
        slug: 'acme',
        stores: ['activity'], // same store — shares the one connection above
        onEvent: () => {},
        onReorg: () => reorgsB.push(1),
        openStream: open
      })
    )
    await flushMicrotasks()
    expect(connections).toHaveLength(1) // confirms this really is the shared-connection case

    connections[0]?.push('id: 5\nevent: reorg\ndata: \n\n')
    await flushMicrotasks()

    expect(reorgsA).toEqual([1])
    expect(reorgsB).toEqual([1])
  })
})
