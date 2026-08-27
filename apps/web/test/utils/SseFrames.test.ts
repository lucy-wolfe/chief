import { describe, expect, it } from 'vitest'

import { computeBackoffDelayMs, createSseFrameParser } from '@/utils/SseFrames'

describe('createSseFrameParser', () => {
  it('reassembles a frame split in the middle of a line', () => {
    const parser = createSseFrameParser()
    expect(parser.push('id: 7\nevent: do')).toEqual([])
    expect(parser.push('c\ndata: {"ok":true}\n\n')).toEqual([
      { id: '7', event: 'doc', data: '{"ok":true}' }
    ])
  })

  it('joins multiline data, accepts CRLF, and ignores unknown fields', () => {
    const parser = createSseFrameParser()
    expect(
      parser.push('event: session\r\ndata: first\r\ndata: second\r\nretry: 10\r\n\r\n')
    ).toEqual([{ event: 'session', data: 'first\nsecond' }])
  })

  it('reports comments as proof-of-life frames without dispatching a data frame', () => {
    const parser = createSseFrameParser()
    expect(parser.push(': hb\n\n')).toEqual([{ comment: true }])
  })

  it('resets incomplete state when a connection is replaced', () => {
    const parser = createSseFrameParser()
    expect(parser.push('id: stale\nevent: doc\n')).toEqual([])
    parser.reset()
    expect(parser.push('data: fresh\n\n')).toEqual([{ data: 'fresh' }])
  })
})

describe('computeBackoffDelayMs', () => {
  it('doubles from the floor and caps at the configured ceiling', () => {
    expect(computeBackoffDelayMs(0, 1_000, 30_000)).toBe(1_000)
    expect(computeBackoffDelayMs(1, 1_000, 30_000)).toBe(2_000)
    expect(computeBackoffDelayMs(2, 1_000, 30_000)).toBe(4_000)
    expect(computeBackoffDelayMs(8, 1_000, 30_000)).toBe(30_000)
  })
})
