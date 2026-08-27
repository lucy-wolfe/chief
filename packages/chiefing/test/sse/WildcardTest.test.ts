// #775 load-bearing subtlety: a whole-company `store: "*"` wildcard frame
// (drop_company) fans out into one synthetic doc-change per SUBSCRIBED
// store, so onEvent never has to special-case "*" itself.
import { createFakeOpener, flushMicrotasks } from '@test/sse/FakeSseStream'
import { afterEach, describe, expect, it } from 'vitest'

import { SseWatcher } from '@/sse/SseWatcher'
import type { SseDocChangeEvent } from '@/types/Watch'

const watchers: SseWatcher[] = []
afterEach(() => {
  for (const w of watchers.splice(0)) w.close()
})

describe('wildcard fan-out', () => {
  it('a store:"*" frame fans out into one event per subscribed store', async () => {
    const { open, connections } = createFakeOpener()
    const events: SseDocChangeEvent[] = []
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity', 'supervision', 'mailbox'],
      onEvent: (e) => {
        events.push(e)
      },
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    connections[0]?.push(
      'id: 10\nevent: doc-change\ndata: {"seq":10,"slug":"acme","store":"*","updated_at":"","removed":true}\n\n'
    )
    await flushMicrotasks()
    expect(events).toHaveLength(3)
    expect(events.map((e) => e.store).sort()).toEqual(['activity', 'mailbox', 'supervision'])
    // Every synthetic event carries the same seq/removed as the
    // wildcard frame, only `store` differs.
    for (const e of events) {
      expect(e.seq).toBe(10)
      expect(e.removed).toBe(true)
    }
  })

  it('a wildcard fans out to exactly one store when only one is subscribed', async () => {
    const { open, connections } = createFakeOpener()
    const events: SseDocChangeEvent[] = []
    const watcher = new SseWatcher({
      url: 'http://chiefd.local',
      slug: 'acme',
      stores: ['activity'],
      onEvent: (e) => {
        events.push(e)
      },
      openStream: open
    })
    watchers.push(watcher)
    await flushMicrotasks()
    connections[0]?.push(
      'id: 11\nevent: doc-change\ndata: {"seq":11,"slug":"acme","store":"*","updated_at":"","removed":true}\n\n'
    )
    await flushMicrotasks()
    expect(events).toEqual([
      { seq: 11, slug: 'acme', store: 'activity', updated_at: '', removed: true }
    ])
  })
})
