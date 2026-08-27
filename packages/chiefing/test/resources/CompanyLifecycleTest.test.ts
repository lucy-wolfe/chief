// CompanyLifecycleClient — the only client for chiefd's resident lifecycle
// surface. Driven through a fake `fetch` that answers a real `text/event-stream`
// body, so what is under test is the wire contract rather than a mock's shape.

import { describe, expect, it } from 'vitest'

import { CompanyLifecycleRefusalError } from '@/Errors'
import {
  chiefdHostUrlFromEnvironment,
  CompanyLifecycleClient,
  DEFAULT_CHIEFD_HOST_URL
} from '@/resources/CompanyLifecycle'
import type { CompanyLifecyclePhase } from '@/types/CompanyLifecycle'

// `key` and `slug` are different strings on purpose: the key ADDRESSES the
// company and the slug is only its display name, and a fixture that reused one
// value could not tell the two apart.
const LAUNCHED = {
  slug: 'acme',
  key: '4d0e2ed2cec4',
  dir: '/work/acme',
  url: 'http://127.0.0.1:8792',
  chiefPersonId: 'executive-ceo',
  session: 'org-acme'
}

function sse(body: string): Response {
  return new Response(body, {
    status: 200,
    headers: { 'content-type': 'text/event-stream' }
  })
}

function phaseFrame(phase: string, detail = ''): string {
  /* eslint-disable lucy/no-json-stringify */
  // Test fixture wire text — the same exemption the production module takes.
  return `event: phase\ndata: ${JSON.stringify({ phase, slug: 'acme', detail })}\n\n`
}

function terminalFrame(event: string, payload: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(payload)}\n\n`
  /* eslint-enable lucy/no-json-stringify */
}

interface Call {
  url: string
  method: string | undefined
  body: unknown
  accept: string | undefined
}

function recordingFetch(responder: () => Response): { fetchImpl: typeof fetch; calls: Call[] } {
  const calls: Call[] = []
  const fetchImpl: typeof fetch = async (input, init) => {
    const headers = new Headers(init?.headers)
    calls.push({
      url: String(input),
      method: init?.method,
      body: typeof init?.body === 'string' ? JSON.parse(init.body) : undefined,
      accept: headers.get('accept') ?? undefined
    })
    return responder()
  }
  return { fetchImpl, calls }
}

async function drain(
  generator: AsyncGenerator<CompanyLifecyclePhase, unknown>
): Promise<{ frames: CompanyLifecyclePhase[]; result: unknown }> {
  const frames: CompanyLifecyclePhase[] = []
  let next = await generator.next()
  while (!next.done) {
    frames.push(next.value)
    next = await generator.next()
  }
  return { frames, result: next.value }
}

describe('chiefdHostUrlFromEnvironment', () => {
  it('returns the default for an empty or absent value', () => {
    expect(chiefdHostUrlFromEnvironment({})).toBe(DEFAULT_CHIEFD_HOST_URL)
    expect(chiefdHostUrlFromEnvironment({ CHIEFD_HOST_URL: '   ' })).toBe(DEFAULT_CHIEFD_HOST_URL)
  })

  it('returns a real http(s) address verbatim', () => {
    expect(chiefdHostUrlFromEnvironment({ CHIEFD_HOST_URL: 'http://127.0.0.1:9999' })).toBe(
      'http://127.0.0.1:9999'
    )
  })

  it('falls back rather than returning an unreachable address', () => {
    // A wrong address produces a failure that names the wrong host, which is
    // strictly worse than the documented default.
    expect(chiefdHostUrlFromEnvironment({ CHIEFD_HOST_URL: 'file:///tmp/x' })).toBe(
      DEFAULT_CHIEFD_HOST_URL
    )
    expect(chiefdHostUrlFromEnvironment({ CHIEFD_HOST_URL: 'not a url' })).toBe(
      DEFAULT_CHIEFD_HOST_URL
    )
  })

  it('the default sits below beacond and outside the company port walk', () => {
    expect(DEFAULT_CHIEFD_HOST_URL).toBe('http://127.0.0.1:8789')
  })
})

describe('CompanyLifecycleClient.create()', () => {
  it('POSTs the launch request and yields every phase, returning the result', async () => {
    const { fetchImpl, calls } = recordingFetch(() =>
      sse(
        phaseFrame('company-daemon-start', '/data/orgs') +
          phaseFrame('durable-create') +
          phaseFrame('chief-start', 'executive-ceo') +
          terminalFrame('created', LAUNCHED)
      )
    )
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })

    const { frames, result } = await drain(client.create({ name: 'Acme', purpose: 'Make things' }))

    expect(calls).toEqual([
      {
        url: 'http://127.0.0.1:8789/v1/company/create',
        method: 'POST',
        body: { name: 'Acme', purpose: 'Make things' },
        accept: 'text/event-stream'
      }
    ])
    expect(frames).toEqual([
      { phase: 'company-daemon-start', slug: 'acme', detail: '/data/orgs' },
      { phase: 'durable-create', slug: 'acme', detail: '' },
      { phase: 'chief-start', slug: 'acme', detail: 'executive-ceo' }
    ])
    expect(result).toEqual(LAUNCHED)
  })

  it('carries a phase name it has never seen through unchanged', async () => {
    // chiefd and this client deploy separately. Coercing or dropping an
    // unrecognised phase would make adding one a breaking change, and would
    // hang a caller waiting for a step that already went past.
    const { fetchImpl } = recordingFetch(() =>
      sse(phaseFrame('pi-home-materialize') + terminalFrame('created', LAUNCHED))
    )
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })

    const { frames } = await drain(client.create({ name: 'Acme', purpose: 'p' }))
    expect(frames.map((f) => f.phase)).toEqual(['pi-home-materialize'])
  })

  it('ignores a comment heartbeat', async () => {
    const { fetchImpl } = recordingFetch(() =>
      sse(':hb\n' + phaseFrame('chief-start') + ':hb\n' + terminalFrame('created', LAUNCHED))
    )
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })
    const { frames } = await drain(client.create({ name: 'Acme', purpose: 'p' }))
    expect(frames.map((f) => f.phase)).toEqual(['chief-start'])
  })

  it('throws the refusal a failed frame carries, after yielding the phases before it', async () => {
    const { fetchImpl } = recordingFetch(() =>
      sse(
        phaseFrame('durable-create-failed', 'slug-taken') +
          terminalFrame('failed', { code: 'lifecycle-failed', detail: 'slug-taken' })
      )
    )
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })

    const generator = client.create({ name: 'Acme', purpose: 'p' })
    const first = await generator.next()
    expect(first.value).toMatchObject({ phase: 'durable-create-failed' })
    await expect(generator.next()).rejects.toMatchObject({
      name: 'CompanyLifecycleRefusalError',
      code: 'lifecycle-failed',
      detail: 'slug-taken'
    })
  })

  it('treats a stream that ends with no terminal frame as abandoned, never as success', async () => {
    // Silence and success are different answers. Reporting "created" for a
    // connection that died mid-launch is the worst possible one.
    const { fetchImpl } = recordingFetch(() => sse(phaseFrame('durable-create')))
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })

    await expect(drain(client.create({ name: 'Acme', purpose: 'p' }))).rejects.toMatchObject({
      code: 'lifecycle-abandoned'
    })
  })

  it('turns a non-2xx answer into a refusal carrying chiefd’s code', async () => {
    const { fetchImpl } = recordingFetch(
      () =>
        new Response('{"code":"lifecycle-failed","detail":"no tmux"}', {
          status: 422,
          headers: { 'content-type': 'application/json' }
        })
    )
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })

    await expect(drain(client.create({ name: 'Acme', purpose: 'p' }))).rejects.toBeInstanceOf(
      CompanyLifecycleRefusalError
    )
  })

  it('refuses a malformed phase frame rather than dropping it', async () => {
    // On a document feed an unreadable frame is safely dropped — the next one
    // supersedes it. Here every frame is the only one of its kind.
    const { fetchImpl } = recordingFetch(() =>
      sse('event: phase\ndata: not json\n\n' + terminalFrame('created', LAUNCHED))
    )
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })

    await expect(drain(client.create({ name: 'Acme', purpose: 'p' }))).rejects.toMatchObject({
      code: 'lifecycle-malformed'
    })
  })
})

describe('CompanyLifecycleClient.boot()', () => {
  it('POSTs the slug and ends on the booted terminal name', async () => {
    const { fetchImpl, calls } = recordingFetch(() =>
      sse(phaseFrame('chief-start') + terminalFrame('booted', LAUNCHED))
    )
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })

    const { frames, result } = await drain(client.boot('acme'))

    expect(calls[0]?.url).toBe('http://127.0.0.1:8789/v1/company/boot')
    expect(calls[0]?.body).toEqual({ slug: 'acme' })
    expect(frames.map((f) => f.phase)).toEqual(['chief-start'])
    expect(result).toEqual(LAUNCHED)
  })

  it('does not accept the create verb’s terminal name', async () => {
    // `created` on a boot stream would mean chiefd and this client disagree
    // about which operation ran. Accepting it would hide that.
    const { fetchImpl } = recordingFetch(() => sse(terminalFrame('created', LAUNCHED)))
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })
    await expect(drain(client.boot('acme'))).rejects.toMatchObject({
      code: 'lifecycle-abandoned'
    })
  })
})

describe('CompanyLifecycleClient.stop()', () => {
  it('POSTs the slug and decodes the outcome', async () => {
    const { fetchImpl, calls } = recordingFetch(
      () =>
        new Response(
          '{"mode":"supervised","slug":"acme","session":"org-acme","sessionStopped":true,"daemonStopped":true}',
          { status: 200, headers: { 'content-type': 'application/json' } }
        )
    )
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })

    const outcome = await client.stop('acme')

    expect(calls[0]?.url).toBe('http://127.0.0.1:8789/v1/company/stop')
    expect(calls[0]?.accept).toBe('application/json')
    expect(outcome).toEqual({
      mode: 'supervised',
      slug: 'acme',
      session: 'org-acme',
      sessionStopped: true,
      daemonStopped: true
    })
  })

  it('turns a refusal body into a typed error', async () => {
    const { fetchImpl } = recordingFetch(
      () => new Response('{"code":"lifecycle-failed","detail":"unknown company"}', { status: 422 })
    )
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789', fetchImpl })
    await expect(client.stop('ghost')).rejects.toMatchObject({
      code: 'lifecycle-failed',
      detail: 'unknown company'
    })
  })
})

describe('CompanyLifecycleClient construction', () => {
  it('refuses an empty host URL at construction, not at first use', () => {
    // The failure otherwise surfaces as an opaque network error far from the
    // call site that got the configuration wrong — the same guard
    // `ChiefdClient` carries, for the same reason.
    expect(() => new CompanyLifecycleClient({ hostUrl: '  ' })).toThrow(/non-empty/)
  })

  it('tolerates a trailing slash on the host URL', async () => {
    const { fetchImpl, calls } = recordingFetch(
      () => new Response('{"mode":"already-stopped"}', { status: 200 })
    )
    const client = new CompanyLifecycleClient({ hostUrl: 'http://127.0.0.1:8789/', fetchImpl })
    await client.stop('acme')
    // Not `…8789//v1/company/stop`, which most servers answer with a 404 that
    // reads as "the surface is not there".
    expect(calls[0]?.url).toBe('http://127.0.0.1:8789/v1/company/stop')
  })
})
