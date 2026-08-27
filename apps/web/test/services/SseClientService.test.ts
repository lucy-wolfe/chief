import { createFakeSseStreams } from '@test/harness/FakeSseStreams'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  activeSseConnectionCount,
  openSseConnection,
  streamLifecycle,
  subscribeDocEvents,
  subscribePersonStream
} from '@/services/SseClientService'
import { ChiefApiError } from '@/types/ApiErrors'
import type { FetchImpl } from '@/types/Fetch'
import type { SseConnection, SseSubscription, SubscribeDocEventsOptions } from '@/types/Sse'

const BASE_URL = 'http://fake-api.test'
const TOKEN = 'fixture-operator-jwt'
const ownedConnections: SseConnection[] = []
const ownedSubscriptions: SseSubscription[] = []

afterEach(() => {
  for (const connection of ownedConnections.splice(0)) connection.close()
  for (const subscription of ownedSubscriptions.splice(0)) subscription.close()
  vi.useRealTimers()
})

/* eslint-disable lucy/no-json-stringify */
// Test-only SSE fixture framing. Production serialization has one dedicated
// service seam; this helper merely writes its fixture wire text.
function json(value: unknown): string {
  return JSON.stringify(value)
}
/* eslint-enable lucy/no-json-stringify */

function sseFrame(options: { id?: string; event?: string; data?: unknown }): string {
  const lines: string[] = []
  if (typeof options.id === 'string') lines.push(`id: ${options.id}`)
  if (typeof options.event === 'string') lines.push(`event: ${options.event}`)
  if (typeof options.data !== 'undefined') lines.push(`data: ${json(options.data)}`)
  return `${lines.join('\n')}\n\n`
}

async function flushStreamWork(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

function openForTest(options: {
  fetchImpl: FetchImpl
  onFrame?: (frame: { id?: string; event?: string; data?: string }) => void
  onChannelState?: (state: 'connecting' | 'healthy' | 'dead') => void
}): SseConnection {
  const connection = openSseConnection({
    url: `${BASE_URL}/companies/acme/events?stores=activity`,
    accessToken: () => TOKEN,
    fetchImpl: options.fetchImpl,
    onFrame: options.onFrame ?? (() => undefined),
    onChannelState: options.onChannelState
  })
  ownedConnections.push(connection)
  return connection
}

describe('openSseConnection', () => {
  it('uses header auth, resumes the exact cursor, and never writes a token into its URL', async () => {
    vi.useFakeTimers()
    const fake = createFakeSseStreams()
    const received: string[] = []
    const connection = openForTest({
      fetchImpl: fake.fetchImpl,
      onFrame: (frame) => {
        if (frame.id) received.push(frame.id)
      }
    })
    const first = fake.openNext()
    await flushStreamWork()
    first.push(
      sseFrame({
        id: '17',
        event: 'doc',
        data: {
          companyKey: 'acme',
          store: 'activity',
          seq: 17,
          generation: 1,
          updatedAt: 'now',
          removed: false
        }
      })
    )
    await flushStreamWork()
    expect(received).toEqual(['17'])
    expect(fake.requests[0]?.headers.authorization).toBe(`Bearer ${TOKEN}`)
    expect(fake.requests[0]?.headers['last-event-id']).toBeUndefined()
    expect(fake.requests[0]?.url.includes('accessToken')).toBe(false)

    first.close()
    await flushStreamWork()
    await vi.advanceTimersByTimeAsync(1_000)
    await flushStreamWork()
    expect(fake.requests).toHaveLength(2)
    expect(fake.requests[1]?.headers['last-event-id']).toBe('17')
    expect(fake.requests[1]?.url).toBe(fake.requests[0]?.url)

    connection.close()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('aborts a stalled connect at five seconds and retries only after backoff', async () => {
    vi.useFakeTimers()
    const fake = createFakeSseStreams()
    const states: string[] = []
    const connection = openForTest({
      fetchImpl: fake.fetchImpl,
      onChannelState: (state) => states.push(state)
    })
    expect(fake.pendingCount()).toBe(1)
    await vi.advanceTimersByTimeAsync(5_000)
    await flushStreamWork()
    expect(states).toContain('dead')
    expect(fake.requests).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(1_000)
    await flushStreamWork()
    expect(fake.requests).toHaveLength(2)
    expect(fake.requests.map((request) => new URL(request.url).pathname)).toEqual([
      '/companies/acme/events',
      '/companies/acme/events'
    ])
    connection.close()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('re-arms heartbeat liveness on a comment and reconnects after actual silence', async () => {
    vi.useFakeTimers()
    const fake = createFakeSseStreams()
    const connection = openForTest({ fetchImpl: fake.fetchImpl })
    const stream = fake.openNext()
    await flushStreamWork()

    await vi.advanceTimersByTimeAsync(44_000)
    stream.push(': hb\n\n')
    await flushStreamWork()
    await vi.advanceTimersByTimeAsync(44_999)
    expect(fake.requests).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(1)
    await flushStreamWork()
    await vi.advanceTimersByTimeAsync(1_000)
    await flushStreamWork()
    expect(fake.requests).toHaveLength(2)
    connection.close()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('resets retry backoff after proof of life rather than escalating forever', async () => {
    vi.useFakeTimers()
    const fake = createFakeSseStreams()
    const connection = openForTest({ fetchImpl: fake.fetchImpl })
    fake.failNext()
    await flushStreamWork()
    await vi.advanceTimersByTimeAsync(1_000)
    await flushStreamWork()
    const recovered = fake.openNext()
    await flushStreamWork()
    recovered.push(': hb\n\n')
    await flushStreamWork()
    recovered.close()
    await flushStreamWork()

    await vi.advanceTimersByTimeAsync(999)
    expect(fake.requests).toHaveLength(2)
    await vi.advanceTimersByTimeAsync(1)
    await flushStreamWork()
    expect(fake.requests).toHaveLength(3)
    connection.close()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('keeps dead-channel retries on the same SSE route and never starts a polling fallback', async () => {
    vi.useFakeTimers()
    const fake = createFakeSseStreams()
    const connection = openForTest({ fetchImpl: fake.fetchImpl })
    fake.failNext()
    await flushStreamWork()
    await vi.advanceTimersByTimeAsync(1_000)
    fake.failNext()
    await flushStreamWork()
    await vi.advanceTimersByTimeAsync(2_000)
    await flushStreamWork()
    expect(fake.requests).toHaveLength(3)
    expect(new Set(fake.requests.map((request) => request.url))).toEqual(
      new Set([`${BASE_URL}/companies/acme/events?stores=activity`])
    )
    connection.close()
    expect(vi.getTimerCount()).toBe(0)
  })
})

describe('SSE hubs', () => {
  it('refcounts document subscribers, preserves healthy reorg state, and drops the resume cursor', async () => {
    vi.useFakeTimers()
    const fake = createFakeSseStreams()
    const docs: number[] = []
    const reorgs: number[] = []
    const states: string[] = []
    // Annotated against the real contract rather than inferred: the three
    // callbacks below return `void`, and an inferred object literal happily
    // let `Array.prototype.push`'s `number` stand in for that.
    const options: SubscribeDocEventsOptions = {
      companyKey: 'acme',
      stores: ['activity'],
      onDoc: (event) => {
        docs.push(event.seq)
      },
      onReorg: () => {
        reorgs.push(1)
      },
      onChannelState: (state) => {
        states.push(state)
      },
      deps: { baseUrl: BASE_URL, accessToken: () => TOKEN, fetchImpl: fake.fetchImpl }
    }
    // The path a browser must actually request. Next serves route handlers
    // under `/api`, and every client path in this app was once written without
    // that prefix — apps/api was a separate service whose own paths began at
    // `/companies`. With it deleted and the base URL now this app's own
    // origin, those literals addressed `/companies/…`, which nothing serves:
    // every request from the page 404'd while both halves stayed green.
    const prefixCheck = subscribeDocEvents(options)
    ownedSubscriptions.push(prefixCheck)
    expect(new URL(fake.requests[0]?.url ?? '').pathname).toBe('/api/companies/acme/events')
    prefixCheck.close()
    fake.requests.length = 0

    const first = subscribeDocEvents(options)
    const second = subscribeDocEvents({
      ...options,
      onDoc: () => undefined,
      onReorg: () => undefined
    })
    ownedSubscriptions.push(first, second)
    expect(activeSseConnectionCount()).toBe(1)
    expect(fake.requests).toHaveLength(1)
    const stream = fake.openNext()
    await flushStreamWork()
    stream.push(
      sseFrame({
        id: '9',
        event: 'doc',
        data: {
          companyKey: 'acme',
          store: 'activity',
          seq: 9,
          generation: 1,
          updatedAt: 'now',
          removed: false
        }
      })
    )
    await flushStreamWork()
    expect(docs).toEqual([9])
    stream.push('event: reorg\ndata: {}\n\n')
    await flushStreamWork()
    expect(reorgs).toEqual([1])
    expect(states.at(-1)).toBe('healthy')

    stream.close()
    await flushStreamWork()
    await vi.advanceTimersByTimeAsync(1_000)
    await flushStreamWork()
    expect(fake.requests[1]?.headers['last-event-id']).toBeUndefined()
    first.close()
    expect(activeSseConnectionCount()).toBe(1)
    second.close()
    expect(activeSseConnectionCount()).toBe(0)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('coalesces pending document notifications per store without delaying another store', async () => {
    vi.useFakeTimers()
    const fake = createFakeSseStreams()
    const delivered: Array<{ store: string; seq: number }> = []
    let releaseFirst: (() => void) | undefined
    const firstDelivery = new Promise<void>((resolve) => {
      releaseFirst = resolve
    })
    const subscription = subscribeDocEvents({
      companyKey: 'acme',
      stores: ['activity', 'supervision'],
      onDoc: async (event) => {
        delivered.push({ store: event.store, seq: event.seq })
        if (event.store === 'activity' && event.seq === 1) await firstDelivery
      },
      onReorg: () => undefined,
      deps: { baseUrl: BASE_URL, accessToken: () => TOKEN, fetchImpl: fake.fetchImpl }
    })
    ownedSubscriptions.push(subscription)
    const stream = fake.openNext()
    await flushStreamWork()

    const document = (store: string, seq: number): string =>
      sseFrame({
        id: String(seq),
        event: 'doc',
        data: {
          companyKey: 'acme',
          store,
          seq,
          generation: 1,
          updatedAt: 'now',
          removed: false
        }
      })
    stream.push(document('activity', 1))
    await flushStreamWork()
    stream.push(document('activity', 2))
    stream.push(document('activity', 3))
    stream.push(document('supervision', 8))
    await flushStreamWork()

    expect(delivered).toEqual([
      { store: 'activity', seq: 1 },
      { store: 'supervision', seq: 8 }
    ])
    if (!releaseFirst) throw new Error('first document delivery did not start')
    releaseFirst()
    await flushStreamWork()
    expect(delivered).toEqual([
      { store: 'activity', seq: 1 },
      { store: 'supervision', seq: 8 },
      { store: 'activity', seq: 3 }
    ])

    subscription.close()
    expect(activeSseConnectionCount()).toBe(0)
    expect(vi.getTimerCount()).toBe(0)
  })
})

describe('streamLifecycle', () => {
  it('streams phases, resolves created, authenticates via headers, and never retries the POST', async () => {
    vi.useFakeTimers()
    const fake = createFakeSseStreams()
    const phases: string[] = []
    const terminal = streamLifecycle({
      path: '/companies',
      body: { slug: 'acme' },
      onPhase: (frame) => phases.push(frame.phase),
      deps: { baseUrl: BASE_URL, accessToken: () => TOKEN, fetchImpl: fake.fetchImpl }
    })
    const stream = fake.openNext()
    await flushStreamWork()
    stream.push(sseFrame({ event: 'phase', data: { phase: 'create' } }))
    stream.push(sseFrame({ event: 'created', data: { slug: 'acme' } }))
    await expect(terminal).resolves.toEqual({ kind: 'created', slug: 'acme' })
    expect(phases).toEqual(['create'])
    expect(fake.requests).toHaveLength(1)
    expect(fake.requests[0]?.method).toBe('POST')
    expect(fake.requests[0]?.headers.authorization).toBe(`Bearer ${TOKEN}`)
    expect(fake.requests[0]?.url.includes('accessToken')).toBe(false)
    expect(vi.getTimerCount()).toBe(0)
  })

  it("preserves the E5 boot route's booted terminal rather than mistaking success for EOF", async () => {
    vi.useFakeTimers()
    const fake = createFakeSseStreams()
    const terminal = streamLifecycle({
      path: '/companies/acme/boot',
      onPhase: () => undefined,
      deps: { baseUrl: BASE_URL, accessToken: () => TOKEN, fetchImpl: fake.fetchImpl }
    })
    const stream = fake.openNext()
    await flushStreamWork()
    stream.push(sseFrame({ event: 'booted', data: { slug: 'acme' } }))

    await expect(terminal).resolves.toEqual({ kind: 'booted', slug: 'acme' })
    expect(fake.requests).toHaveLength(1)
    expect(fake.requests[0]?.method).toBe('POST')
    expect(vi.getTimerCount()).toBe(0)
  })

  it('throws the one error taxonomy on a failed lifecycle terminal without retrying', async () => {
    vi.useFakeTimers()
    const fake = createFakeSseStreams()
    const terminal = streamLifecycle({
      path: '/companies/acme/boot',
      onPhase: () => undefined,
      deps: { baseUrl: BASE_URL, accessToken: () => TOKEN, fetchImpl: fake.fetchImpl }
    })
    const stream = fake.openNext()
    await flushStreamWork()
    stream.push(
      sseFrame({
        event: 'failed',
        data: { error: { code: 'lifecycle-failed', detail: 'cannot boot' } }
      })
    )
    await expect(terminal).rejects.toBeInstanceOf(ChiefApiError)
    await expect(terminal).rejects.toMatchObject({
      kind: 'upstream',
      code: 'lifecycle-failed',
      detail: 'cannot boot'
    })
    expect(fake.requests).toHaveLength(1)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('dials the person channel at /stream, the route this app actually serves', async () => {
    // `/stream`, not `/events`. The two names are not interchangeable and the
    // mismatch was silent: this client dialled `/events` while the route file
    // is `stream/route.ts`, so the person channel 404'd on every page while
    // the COMPANY feed — also `/events` — was fine. No test asserted this
    // path, which is precisely why one word could be wrong for so long.
    const fake = createFakeSseStreams()
    const subscription = subscribePersonStream({
      companyKey: 'acme',
      personId: 'ceo',
      onState: () => undefined,
      onSession: () => undefined,
      onHost: () => undefined,
      onReorg: () => undefined,
      deps: { baseUrl: BASE_URL, accessToken: () => TOKEN, fetchImpl: fake.fetchImpl }
    })
    ownedSubscriptions.push(subscription)

    expect(new URL(fake.requests[0]?.url ?? '').pathname).toBe(
      '/api/companies/acme/people/ceo/stream'
    )
  })
})
