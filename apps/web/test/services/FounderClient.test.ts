// The browser half of Founder Mode's three routes.
//
// Kept apart from `ChiefApiClientService.test.ts` because its fake answers the
// company/session surface from a fixture map; these three paths are Founder's
// and answer nothing there. What is asserted is the seam that keeps breaking:
// the exact URL dialled, and the exact body sent.
import { describe, expect, it } from 'vitest'

import { ChiefApiClientService } from '@/services/ChiefApiClientService'
import type { FetchImpl } from '@/types/Fetch'

function json(value: unknown): string {
  /* eslint-disable lucy/no-json-stringify */
  // Test-only fixture serialization, the same disable the sibling suite takes.
  return JSON.stringify(value)
  /* eslint-enable lucy/no-json-stringify */
}

interface Recorded {
  url: string
  method: string
  body: string | undefined
}

function recordingClient(answer: unknown): {
  client: ChiefApiClientService
  calls: Recorded[]
} {
  const calls: Recorded[] = []
  const fetchImpl: FetchImpl = async (input, init) => {
    calls.push({
      url: typeof input === 'string' ? input : input.toString(),
      method: init?.method ?? 'GET',
      body: typeof init?.body === 'string' ? init.body : undefined
    })
    return new Response(json(answer), { status: 200 })
  }
  return {
    client: new ChiefApiClientService({ baseUrl: 'http://web.example', fetchImpl }),
    calls
  }
}

describe('the Founder client', () => {
  it('reads the transcript from this app’s own /api route', async () => {
    const { client, calls } = recordingClient({ entries: [] })
    await client.founderTranscript()
    expect(calls).toEqual([
      { url: 'http://web.example/api/founder/transcript', method: 'GET', body: undefined }
    ])
  })

  it('sends a turn as {text}, the one word the route reads', async () => {
    // The person `say` route learned this the hard way: the client sent
    // `message`, the route read `text`, both suites were green, and every
    // message an operator typed came back 422.
    const { client, calls } = recordingClient({ reply: 'hello' })
    await client.founderSay('build me a company')
    expect(calls[0]?.url).toBe('http://web.example/api/founder/say')
    expect(calls[0]?.method).toBe('POST')
    expect(calls[0]?.body).toBe(json({ text: 'build me a company' }))
  })

  it('parses a launch off the say response', async () => {
    const { client } = recordingClient({
      reply: 'Done.',
      launched: { key: '4d0e2ed2cec4', slug: 'acme-inc', name: 'Acme Inc' }
    })
    expect(await client.founderSay('go')).toEqual({
      reply: 'Done.',
      launched: { key: '4d0e2ed2cec4', slug: 'acme-inc', name: 'Acme Inc' }
    })
  })

  it('accepts a transcript with no launch', async () => {
    const { client } = recordingClient({ entries: [] })
    expect((await client.founderTranscript()).launched).toBeUndefined()
  })

  it('posts an abort with an empty body', async () => {
    const { client, calls } = recordingClient({ aborted: true })
    expect(await client.founderAbort()).toEqual({ aborted: true })
    expect(calls[0]?.url).toBe('http://web.example/api/founder/abort')
    expect(calls[0]?.method).toBe('POST')
    expect(calls[0]?.body).toBe(json({}))
  })

  it('raises this app’s error taxonomy for a refusal, not a raw response', async () => {
    const fetchImpl: FetchImpl = async () =>
      new Response(json({ error: { code: 'founder-route-unset', detail: 'no route' } }), {
        status: 409
      })
    const client = new ChiefApiClientService({ baseUrl: 'http://web.example', fetchImpl })
    await expect(client.founderSay('hello')).rejects.toMatchObject({
      kind: 'conflict',
      status: 409,
      code: 'founder-route-unset'
    })
  })
})
