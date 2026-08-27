// Which companies exist, and which are actually up — two different questions
// with two different sources.
//
// The rule under test: a registry row is NOT proof of life. A row keeps its
// url after a daemon dies without deregistering, so a directory that trusted
// the registry would show "running" for a company whose every request then
// 502s, and the operator would click into a dead company.
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { isNullish } from '@/utils/Nullish'

const list = vi.fn()

vi.mock('@chief/chiefing', async (importOriginal) => ({
  // `importOriginal` so `describeFetchFailure` is the REAL renderer. Stubbing
  // it would make the `reason` assertions below assert the stub.
  ...(await importOriginal<typeof import('@chief/chiefing')>()),
  DiscoveryClient: class {
    async list(): Promise<unknown> {
      return list()
    }
  }
}))
vi.mock('@/common/Env', () => ({ beacondUrl: () => 'http://beacond.test:6969' }))

const { companyDirectory, companySummary } = await import('@/server/CompanyDirectory')

const originalFetch = globalThis.fetch

beforeEach(() => {
  list.mockReset()
  globalThis.fetch = originalFetch
})

/**
 * A stand-in for the AMBIENT global `fetch` — the one `CompanyDirectory`
 * calls, because a server-side probe of a single URL needs no injection seam
 * and deliberately has none.
 *
 * `typeof fetch` in this workspace is the DOM lib's declaration merged with
 * Bun's (`@types/bun` is a root devDependency), so it also carries
 * `preconnect`. That method is a connection HINT: it has no observable effect
 * on any request, so a no-op is a COMPLETE implementation of it rather than a
 * hole. A bare handler is simply not the global's type and cannot stand in
 * for it.
 */
function globalFetchDouble(handler: (input: URL | RequestInfo) => Promise<Response>): typeof fetch {
  return Object.assign(handler, { preconnect: (): void => undefined })
}

function answering(byUrl: Record<string, number>): void {
  globalThis.fetch = globalFetchDouble(async (input) => {
    const url = typeof input === 'string' ? input : input.toString()
    const status = Object.entries(byUrl).find(([prefix]) => url.startsWith(prefix))?.[1]
    if (isNullish(status)) throw new Error('connection refused')
    return new Response('{}', { status })
  })
}

describe('companyDirectory', () => {
  it('probes the health route chiefd actually serves', async () => {
    // Observed live: the probe asked for `/v1/health`, chiefd serves
    // `/v1/docs/health`, and a 404 reads as unhealthy — so every RUNNING
    // company reported as stopped. Uniformly wrong in the one direction an
    // operator cannot argue with.
    list.mockResolvedValue([
      {
        dir: '/work/acme',
        key: '0123456789ab',
        slug: 'acme',
        registeredAt: 'x',
        url: 'http://a:8792'
      }
    ])
    const asked: string[] = []
    globalThis.fetch = globalFetchDouble(async (input) => {
      asked.push(typeof input === 'string' ? input : input.toString())
      return new Response('{}', { status: 200 })
    })

    await companyDirectory()

    expect(asked).toEqual(['http://a:8792/v1/docs/health'])
  })

  it('reports a company whose daemon answers as running', async () => {
    list.mockResolvedValue([
      {
        dir: '/work/acme',
        key: '0123456789ab',
        slug: 'acme',
        registeredAt: 'x',
        url: 'http://a:8792'
      }
    ])
    answering({ 'http://a:8792': 200 })

    const [entry] = await companyDirectory()

    expect(entry?.status).toBe('running')
    expect(entry?.chiefd.healthy).toBe(true)
  })

  it('reports a registered url that nobody answers as STOPPED', async () => {
    // The exact stale-row case: a process died without deregistering, so the
    // url is still there and nothing is listening.
    list.mockResolvedValue([
      {
        dir: '/work/acme',
        key: '0123456789ab',
        slug: 'acme',
        registeredAt: 'x',
        url: 'http://a:8792'
      }
    ])
    answering({})

    const [entry] = await companyDirectory()

    expect(entry?.status).toBe('stopped')
    expect(entry?.chiefd.healthy).toBe(false)
    // The reason is carried so an operator can tell a refusal from a timeout
    // without reading a log.
    expect(entry?.chiefd.reason).toContain('refused')
  })

  it('reports an unhealthy HTTP answer as stopped, with its status', async () => {
    list.mockResolvedValue([
      {
        dir: '/work/acme',
        key: '0123456789ab',
        slug: 'acme',
        registeredAt: 'x',
        url: 'http://a:8792'
      }
    ])
    answering({ 'http://a:8792': 503 })

    const [entry] = await companyDirectory()

    expect(entry?.status).toBe('stopped')
    expect(entry?.chiefd.httpStatus).toBe(503)
  })

  it('treats a company with no url as stopped, not as an error', async () => {
    // Registered but never attached is the ordinary state of a new company.
    list.mockResolvedValue([
      { dir: '/work/acme', key: '0123456789ab', slug: 'acme', registeredAt: 'x' }
    ])

    const [entry] = await companyDirectory()

    expect(entry?.status).toBe('stopped')
    expect(entry?.url).toBeUndefined()
  })

  it('lets one dead daemon be one company’s problem', async () => {
    // A directory that failed because ONE company was down would hide every
    // healthy company behind the broken one.
    list.mockResolvedValue([
      {
        dir: '/work/acme',
        key: '0123456789ab',
        slug: 'acme',
        registeredAt: 'x',
        url: 'http://a:8792'
      },
      {
        dir: '/work/globex',
        key: 'cafebabe0011',
        slug: 'globex',
        registeredAt: 'x',
        url: 'http://b:8793'
      }
    ])
    answering({ 'http://b:8793': 200 })

    const directory = await companyDirectory()

    expect(directory.map((entry) => [entry.slug, entry.status])).toEqual([
      ['acme', 'stopped'],
      ['globex', 'running']
    ])
  })
})

describe('companySummary', () => {
  it('answers for a company beacond knows', async () => {
    list.mockResolvedValue([
      { dir: '/work/acme', key: '0123456789ab', slug: 'acme', registeredAt: 'x' }
    ])

    expect((await companySummary('0123456789ab'))?.slug).toBe('acme')
  })

  it('answers nothing for a key beacond has never heard of', async () => {
    // Distinct from "exists but stopped": the first means the operator typed
    // something wrong, the second means they need to start it.
    list.mockResolvedValue([])

    expect(await companySummary('ghost')).toBeUndefined()
  })
})
