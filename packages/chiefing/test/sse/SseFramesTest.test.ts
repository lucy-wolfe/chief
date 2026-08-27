// The one SSE frame decoder, tested directly rather than through a watcher.
// Frame decoding used to be four private fields inside SseWatcher, which meant
// every one of these cases needed a fake connection to reach.

import { describe, expect, it } from 'vitest'

import { readSseFrames, SseFrameDecoder } from '@/sse/SseFrames'
import type { SseFrame } from '@/types/Watch'

async function* chunks(...values: string[]): AsyncGenerator<string> {
  for (const value of values) yield value
}

describe('SseFrameDecoder', () => {
  it('decodes a complete frame on its terminating blank line', () => {
    const decoder = new SseFrameDecoder()
    expect(decoder.push('event: phase\n')).toEqual([])
    expect(decoder.push('data: {"a":1}\n')).toEqual([])
    expect(decoder.push('\n')).toEqual([{ event: 'phase', data: '{"a":1}' }])
  })

  it('reassembles a frame split across arbitrary chunk boundaries', () => {
    // A reader does not get lines; it gets whatever the socket handed it. A
    // decoder that assumed otherwise would work in every test and fail on a
    // real connection under load.
    const decoder = new SseFrameDecoder()
    const frames = [
      ...decoder.push('eve'),
      ...decoder.push('nt: created\nda'),
      ...decoder.push('ta: {"slug":"acme"}\n\n')
    ]
    expect(frames).toEqual([{ event: 'created', data: '{"slug":"acme"}' }])
  })

  it('returns several frames when one chunk completes several', () => {
    const decoder = new SseFrameDecoder()
    const frames = decoder.push('event: phase\ndata: a\n\nevent: phase\ndata: b\n\n')
    expect(frames).toEqual([
      { event: 'phase', data: 'a' },
      { event: 'phase', data: 'b' }
    ])
  })

  it('joins multiple data lines with newlines', () => {
    const decoder = new SseFrameDecoder()
    expect(decoder.push('data: one\ndata: two\n\n')).toEqual([
      { event: 'message', data: 'one\ntwo' }
    ])
  })

  it('defaults a frame with no event field to "message"', () => {
    const decoder = new SseFrameDecoder()
    expect(decoder.push('data: x\n\n')).toEqual([{ event: 'message', data: 'x' }])
  })

  it('carries an id field through', () => {
    const decoder = new SseFrameDecoder()
    expect(decoder.push('id: 42\nevent: doc-change\ndata: {}\n\n')).toEqual([
      { event: 'doc-change', data: '{}', id: '42' }
    ])
  })

  it('strips exactly one leading space from a value, not all whitespace', () => {
    // ' ' after the colon is framing; a second one is data. Trimming both is a
    // silent corruption of any payload that legitimately starts with a space.
    const decoder = new SseFrameDecoder()
    expect(decoder.push('data:  x\n\n')).toEqual([{ event: 'message', data: ' x' }])
  })

  it('accepts CRLF line endings', () => {
    const decoder = new SseFrameDecoder()
    expect(decoder.push('event: phase\r\ndata: x\r\n\r\n')).toEqual([{ event: 'phase', data: 'x' }])
  })

  it('reports a comment heartbeat as its own frame rather than dropping it', () => {
    // A caller's liveness logic needs to see it; a second callback for
    // "something arrived" would be the same information twice.
    const decoder = new SseFrameDecoder()
    expect(decoder.push(':hb\n')).toEqual([{ event: 'comment', data: 'hb' }])
  })

  it('ignores unknown fields instead of rejecting the frame', () => {
    const decoder = new SseFrameDecoder()
    expect(decoder.push('retry: 5000\nevent: phase\ndata: x\n\n')).toEqual([
      { event: 'phase', data: 'x' }
    ])
  })

  it('emits nothing for a blank line that terminates nothing', () => {
    const decoder = new SseFrameDecoder()
    expect(decoder.push('\n\n\n')).toEqual([])
  })

  it('flush() recovers a final frame the stream ended without a blank line after', () => {
    // A per-operation stream closes right after its terminal frame. Losing it
    // to a missing final newline would hang the caller forever.
    const decoder = new SseFrameDecoder()
    expect(decoder.push('event: created\ndata: {"slug":"acme"}')).toEqual([])
    expect(decoder.flush()).toEqual({ event: 'created', data: '{"slug":"acme"}' })
  })

  it('flush() returns undefined when nothing is pending', () => {
    const decoder = new SseFrameDecoder()
    decoder.push('event: phase\ndata: x\n\n')
    expect(decoder.flush()).toBeUndefined()
  })
})

describe('readSseFrames', () => {
  it('yields every frame in order and flushes the last one', async () => {
    const seen: SseFrame[] = []
    for await (const frame of readSseFrames(
      chunks('event: phase\ndata: a\n\n', 'event: created\ndata: b')
    )) {
      seen.push(frame)
    }
    expect(seen).toEqual([
      { event: 'phase', data: 'a' },
      { event: 'created', data: 'b' }
    ])
  })

  it('stops reading the source when the consumer breaks out', async () => {
    let pulled = 0
    async function* endless(): AsyncGenerator<string> {
      for (;;) {
        pulled += 1
        yield 'event: phase\ndata: x\n\n'
      }
    }
    let taken = 0
    for await (const frame of readSseFrames(endless())) {
      expect(frame.event).toBe('phase')
      taken += 1
      break
    }
    // One pull produced the frame the consumer took; abandoning the loop must
    // not keep draining an endless source.
    expect(taken).toBe(1)
    expect(pulled).toBe(1)
  })
})
