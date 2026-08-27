// The adaptation between chiefd's feed and the page's client.
//
// Every test here is a rule that, if it went the other way, produces the same
// symptom: a stream that connects, heartbeats, looks perfectly healthy, and
// delivers nothing the browser can act on. That is worse than a broken stream,
// because a broken stream is visible.
import { describe, expect, it } from 'vitest'

import { translateFeedFrame } from '@/server/CompanyFeed'

const STORES = ['activity', 'organization']

function docFrame(body: Record<string, unknown>): { event: string; data: string } {
  /* eslint-disable lucy/no-json-stringify */
  // A WIRE frame: this fixture stands in for bytes chiefd actually sends.
  return { event: 'doc-change', data: JSON.stringify(body) }
  /* eslint-enable lucy/no-json-stringify */
}

const CHANGE = {
  seq: 12,
  slug: '0123456789ab',
  store: 'activity',
  updated_at: '2026-08-09T00:00:00.000Z',
  removed: false
}

describe('translateFeedFrame', () => {
  it('renames the event to the one the page matches', () => {
    // chiefd says `doc-change`; the page matches `doc` and silently drops
    // anything else. Piping the name through delivers frames nobody reads.
    const [out] = translateFeedFrame(docFrame(CHANGE), 'acme', STORES)

    expect(out).toContain('event: doc\n')
    expect(out).not.toContain('doc-change')
  })

  it('renames updated_at to the key the page’s schema requires', () => {
    // chiefd serializes its Rust struct verbatim. The page parses `updatedAt`,
    // so an unrenamed frame fails validation and is dropped without a word.
    const [out] = translateFeedFrame(docFrame(CHANGE), 'acme', STORES)

    expect(out).toContain('"updatedAt":"2026-08-09T00:00:00.000Z"')
    expect(out).not.toContain('updated_at')
  })

  it('carries the page’s own company key, echoed back unchanged', () => {
    // The page holds ONE handle for the company it is watching, and a frame
    // that carried a different spelling would not match anything it holds. The
    // upstream frame's own `slug` is discarded for exactly that reason.
    const [out] = translateFeedFrame(docFrame(CHANGE), 'page-handle', STORES)

    expect(out).toContain('"companyKey":"page-handle"')
    expect(out).not.toContain('0123456789ab')
  })

  it('keeps the sequence as the SSE id so a reconnect can resume', () => {
    const [out] = translateFeedFrame(docFrame(CHANGE), 'acme', STORES)

    expect(out?.startsWith('id: 12\n')).toBe(true)
  })

  it('fans a whole-company drop out into one frame per watched store', () => {
    // chiefd sends `store: "*"`. The page has no rule for a wildcard, so it
    // would ignore the ONE frame saying everything it is showing is gone.
    const out = translateFeedFrame(
      docFrame({ ...CHANGE, store: '*', removed: true }),
      'acme',
      STORES
    )

    expect(out).toHaveLength(2)
    expect(out[0]).toContain('"store":"activity"')
    expect(out[1]).toContain('"store":"organization"')
    expect(out[0]).toContain('"removed":true')
  })

  it('carries a reorg through under its own name', () => {
    // The page's cursor predates chiefd's retained ring, so it must resync.
    // A resync trigger, not an unhealthy channel.
    expect(translateFeedFrame({ event: 'reorg', data: '{}' }, 'acme', STORES)).toEqual([
      'event: reorg\ndata: {}\n\n'
    ])
  })

  it('drops a heartbeat and anything it does not recognize', () => {
    // A comment frame keeps the connection alive and means nothing to the
    // page's doc handler; emitting a doc frame for it would invent a change.
    expect(translateFeedFrame({ data: 'beat' }, 'acme', STORES)).toEqual([])
    expect(translateFeedFrame({ event: 'lifecycle', data: '{}' }, 'acme', STORES)).toEqual([])
  })

  it('drops a malformed body rather than emitting a half-frame', () => {
    expect(translateFeedFrame({ event: 'doc-change', data: 'not json' }, 'acme', STORES)).toEqual(
      []
    )
    expect(translateFeedFrame({ event: 'doc-change', data: '{"seq":1}' }, 'acme', STORES)).toEqual(
      []
    )
  })
})
