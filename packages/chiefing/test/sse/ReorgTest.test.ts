// #775 load-bearing subtlety (#296): `event: reorg` fires `onReorg` exactly
// once, the channel state stays HEALTHY through it (a reorg is the signal
// covering a lost window, not a failure), and the cursor is dropped — the
// NEXT reconnect carries no `after=`.
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

describe('reorg handling', () => {
  it('fires onReorg exactly once per frame and never touches onEvent', async () => {
    const { open, connections } = createFakeOpener()
    const events: unknown[] = []
    let reorgs = 0
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: (e) => {
        events.push(e)
      },
      onReorg: () => {
        reorgs += 1
      },
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    connections[0]?.push('id: 5\nevent: reorg\ndata: \n\n')
    await flushMicrotasks()
    expect(reorgs).toBe(1)
    expect(events).toEqual([])
  })

  it('the channel state stays healthy through a reorg — it is not a failure', async () => {
    const { open, connections } = createFakeOpener()
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
    connections[0]?.push('id: 5\nevent: reorg\ndata: \n\n')
    await flushMicrotasks()
    expect(states).toEqual(['healthy'])
    expect(watcher.currentChannelState()).toBe('healthy')
  })

  it('drops the cursor: the next reconnect after a reorg carries no after=', async () => {
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
    // First, a real event to establish a cursor.
    connections[0]?.push(sseFrame({ id: 3, event: 'doc-change', data: docChange({ seq: 3 }) }))
    await flushMicrotasks()
    // A reorg drops that cursor.
    connections[0]?.push('id: 9\nevent: reorg\ndata: \n\n')
    await flushMicrotasks()
    connections[0]?.end()
    await flushMicrotasks()
    await vi.advanceTimersByTimeAsync(1000)
    await flushMicrotasks()
    expect(calls[1]).not.toContain('after=')
  })

  it('never fires on a fresh subscribe with no after', async () => {
    const { open } = createFakeOpener()
    let reorgs = 0
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: () => {},
      onReorg: () => {
        reorgs += 1
      },
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    expect(reorgs).toBe(0)
  })
})
