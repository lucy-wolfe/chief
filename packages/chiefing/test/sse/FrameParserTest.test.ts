// #775 load-bearing subtlety: the hand-rolled SSE frame parser. id/event/data
// accumulation, blank-line dispatch, comment lines ignored (as data, but
// still proof-of-life for the heartbeat), unknown fields/events ignored
// (forward compat), and a frame split across chunk boundaries reassembles
// correctly (the parser buffers a trailing partial line across `handleChunk`
// calls).
import { createFakeOpener, docChange, flushMicrotasks, sseFrame } from '@test/sse/FakeSseStream'
import { afterEach, describe, expect, it } from 'vitest'

import { SseWatcher } from '@/sse/SseWatcher'
import type { SseStreamOpener } from '@/types/Watch'

const watchers: SseWatcher[] = []
afterEach(() => {
  for (const w of watchers.splice(0)) w.close()
})

function watch(open: SseStreamOpener): { watcher: SseWatcher; events: unknown[] } {
  const events: unknown[] = []
  const watcher = new SseWatcher({
    url: 'http://chiefd.local',
    slug: 'acme',
    stores: ['activity'],
    onEvent: (event) => {
      events.push(event)
    },
    openStream: open
  })
  watchers.push(watcher)
  return { watcher, events }
}

describe('SSE frame parser', () => {
  it('a single doc-change frame dispatches the parsed event', async () => {
    const { open, connections } = createFakeOpener()
    const { events } = watch(open)
    await flushMicrotasks()
    connections[0]?.push(sseFrame({ id: 1, event: 'doc-change', data: docChange({ seq: 1 }) }))
    await flushMicrotasks()
    expect(events).toEqual([docChange({ seq: 1 })])
  })

  it('comment lines are ignored as data but still count as proof of life', async () => {
    const { open, connections } = createFakeOpener()
    const { events } = watch(open)
    await flushMicrotasks()
    connections[0]?.push(': heartbeat\n\n')
    await flushMicrotasks()
    expect(events).toEqual([])
  })

  it('unknown event types are ignored (forward compat)', async () => {
    const { open, connections } = createFakeOpener()
    const { events } = watch(open)
    await flushMicrotasks()
    connections[0]?.push(sseFrame({ id: 1, event: 'something-new', data: { whatever: true } }))
    await flushMicrotasks()
    expect(events).toEqual([])
  })

  it('unknown fields (e.g. retry:) are ignored', async () => {
    const { open, connections } = createFakeOpener()
    const { events } = watch(open)
    await flushMicrotasks()
    const frame = sseFrame({ id: 1, event: 'doc-change', data: docChange({ seq: 1 }) })
    connections[0]?.push(`retry: 3000\n${frame}`)
    await flushMicrotasks()
    expect(events).toEqual([docChange({ seq: 1 })])
  })

  it('a frame split across chunk boundaries reassembles', async () => {
    const { open, connections } = createFakeOpener()
    const { events } = watch(open)
    await flushMicrotasks()
    const frame = sseFrame({ id: 7, event: 'doc-change', data: docChange({ seq: 7 }) })
    // Split mid-line, not just mid-frame.
    const cut = Math.floor(frame.length / 2)
    connections[0]?.push(frame.slice(0, cut))
    await flushMicrotasks()
    expect(events).toEqual([]) // nothing dispatched yet — the line isn't complete
    connections[0]?.push(frame.slice(cut))
    await flushMicrotasks()
    expect(events).toEqual([docChange({ seq: 7 })])
  })

  it('malformed JSON in the data field is dropped, not thrown', async () => {
    const { open, connections } = createFakeOpener()
    const { events } = watch(open)
    await flushMicrotasks()
    connections[0]?.push('id: 1\nevent: doc-change\ndata: {not-json\n\n')
    await flushMicrotasks()
    expect(events).toEqual([])
  })

  it('a doc-change body missing required fields is dropped, not trusted', async () => {
    const { open, connections } = createFakeOpener()
    const { events } = watch(open)
    await flushMicrotasks()
    connections[0]?.push(sseFrame({ id: 1, event: 'doc-change', data: { slug: 'acme' } }))
    await flushMicrotasks()
    expect(events).toEqual([])
  })

  it('a missing seq in the JSON body falls back to the id: line', async () => {
    const { open, connections } = createFakeOpener()
    const { events } = watch(open)
    await flushMicrotasks()
    const body = docChange()
    delete body.seq
    connections[0]?.push(sseFrame({ id: 9, event: 'doc-change', data: body }))
    await flushMicrotasks()
    expect(events).toEqual([{ ...docChange(), seq: 9 }])
  })
})
