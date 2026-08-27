import { describe, expect, it } from 'vitest'

import { DiscoveryClient, resolveCompanyChiefdUrl } from '@/discovery/DiscoveryClient'
import {
  BeacondUnavailableError,
  CompanyNotRunningError,
  DiscoveryRefusalError,
  UnknownCompanyError
} from '@/Errors'

const BEACOND_URL = 'http://127.0.0.1:6969'

interface Call {
  readonly url: string
  readonly init: RequestInit | undefined
}

/* eslint-disable lucy/no-json-stringify */
// Test-only HTTP fixture bodies; @tribes-terminal/foundation is not a
// dependency anywhere in this workspace (mirrors FetchTransportTest.test.ts's
// matching disable block for the same reason).
function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' }
  })
}
/* eslint-enable lucy/no-json-stringify */

function textResponse(status: number, body: string): Response {
  return new Response(body, { status })
}

function fakeFetch(responses: readonly Response[]): { fetchImpl: typeof fetch; calls: Call[] } {
  const calls: Call[] = []
  let index = 0
  const fetchImpl: typeof fetch = async (input, init) => {
    calls.push({ url: String(input), init })
    const response = responses[index]
    index += 1
    if (!response) throw new Error('fakeFetch: ran out of scripted responses')
    return response
  }
  return { fetchImpl, calls }
}

function client(fetchImpl: typeof fetch, timeoutMs?: number): DiscoveryClient {
  return new DiscoveryClient({ beacondUrl: BEACOND_URL, fetchImpl, timeoutMs })
}

/** Await `promise`, returning whatever it rejects with (or `undefined` if it
 * resolves) — the one place this file catches, so every test below narrows
 * with `instanceof` rather than an `as` assertion. */
async function captured(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise
    return undefined
  } catch (error) {
    return error
  }
}

function assertBeacondUnavailable(error: unknown): BeacondUnavailableError {
  expect(error).toBeInstanceOf(BeacondUnavailableError)
  if (error instanceof BeacondUnavailableError) return error
  throw new Error('expected a BeacondUnavailableError')
}

function assertDiscoveryRefusal(error: unknown): DiscoveryRefusalError {
  expect(error).toBeInstanceOf(DiscoveryRefusalError)
  if (error instanceof DiscoveryRefusalError) return error
  throw new Error('expected a DiscoveryRefusalError')
}

function assertUnknownCompany(error: unknown): UnknownCompanyError {
  expect(error).toBeInstanceOf(UnknownCompanyError)
  if (error instanceof UnknownCompanyError) return error
  throw new Error('expected an UnknownCompanyError')
}

function assertCompanyNotRunning(error: unknown): CompanyNotRunningError {
  expect(error).toBeInstanceOf(CompanyNotRunningError)
  if (error instanceof CompanyNotRunningError) return error
  throw new Error('expected a CompanyNotRunningError')
}

const DIR = '/work/acme'
/** `sha256(dir)[..12]`, minted by whoever created the company and SERVED back
 * on every row. A stand-in of the right shape here, because this client reads
 * the field and never derives one. */
const KEY = '0123456789ab'

const fullRow = {
  dir: DIR,
  key: KEY,
  slug: 'acme',
  registeredAt: '2026-08-03T12:00:00.000Z',
  url: 'http://127.0.0.1:8794',
  port: 8794,
  pid: 4242,
  hostname: 'box',
  lastSeenAt: '2026-08-04T00:00:00.000Z'
}

const noLocationRow = {
  dir: DIR,
  key: KEY,
  slug: 'acme',
  registeredAt: '2026-08-03T12:00:00.000Z'
}

describe('DiscoveryClient.lookup', () => {
  it('resolves the row on {found:true, company}', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { found: true, company: fullRow })])
    const row = await client(fetchImpl).lookup(DIR)
    expect(row).toEqual(fullRow)
  })

  it('resolves undefined on {found:false}', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { found: false })])
    const row = await client(fetchImpl).lookup('/work/ghost')
    expect(row).toBeUndefined()
  })

  it('a network throw becomes BeacondUnavailableError carrying beacondUrl', async () => {
    const fetchImpl: typeof fetch = async () => {
      throw new TypeError('fetch failed')
    }
    const error = assertBeacondUnavailable(await captured(client(fetchImpl).lookup(DIR)))
    expect(error.beacondUrl).toBe(BEACOND_URL)
    expect(error.kind).toBe('unreachable')
  })

  it('a {code,message} 400 becomes DiscoveryRefusalError carrying both', async () => {
    const { fetchImpl } = fakeFetch([
      jsonResponse(400, { code: 'bad-request', message: 'dir must not be empty' })
    ])
    const error = assertDiscoveryRefusal(await captured(client(fetchImpl).lookup('')))
    expect(error.code).toBe('bad-request')
    expect(error.message).toBe('dir must not be empty')
  })

  it('requests exactly GET <base>/v1/lookup?dir=<encoded>, no body', async () => {
    const { fetchImpl, calls } = fakeFetch([jsonResponse(200, { found: false })])
    await client(fetchImpl).lookup('/work/north-star')
    expect(calls).toHaveLength(1)
    expect(calls[0].url).toBe(
      `${BEACOND_URL}/v1/lookup?dir=${encodeURIComponent('/work/north-star')}`
    )
    expect(calls[0].init?.method).toBe('GET')
    expect(calls[0].init?.body).toBeUndefined()
  })

  it('encodes a directory containing characters the query string must escape', async () => {
    const { fetchImpl, calls } = fakeFetch([jsonResponse(200, { found: false })])
    await client(fetchImpl).lookup('/work/a b&c')
    expect(calls[0].url).toBe(`${BEACOND_URL}/v1/lookup?dir=${encodeURIComponent('/work/a b&c')}`)
  })

  /** TWO DIRECTORIES, ONE DISPLAY WORD, TWO COMPANIES.
   *
   * The pair the slug-keyed client could not represent: it looked a company up
   * BY the slug, so `acme` had exactly one answer per box and the second
   * directory's daemon was unreachable through this client. Each directory is
   * its own question here, and each answer carries its own key and its own
   * daemon. */
  it('two directories holding same-named companies resolve to different rows', async () => {
    const twin = {
      ...fullRow,
      dir: '/elsewhere/acme',
      key: 'cafebabe0011',
      url: 'http://127.0.0.1:9001'
    }
    const { fetchImpl } = fakeFetch([
      jsonResponse(200, { found: true, company: fullRow }),
      jsonResponse(200, { found: true, company: twin })
    ])
    const discovery = client(fetchImpl)
    const first = await discovery.lookup(DIR)
    const second = await discovery.lookup('/elsewhere/acme')
    expect(first?.slug).toBe(second?.slug)
    expect(first?.key).not.toBe(second?.key)
    expect(first?.url).not.toBe(second?.url)
  })
})

describe('DiscoveryClient.list', () => {
  it('an empty registry resolves [] and does not throw', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { companies: [] })])
    await expect(client(fetchImpl).list()).resolves.toEqual([])
  })

  it('a company with no location resolves a row whose url/pid are undefined, not an error', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { companies: [noLocationRow] })])
    const rows = await client(fetchImpl).list()
    expect(rows).toHaveLength(1)
    expect(rows[0].url).toBeUndefined()
    expect(rows[0].pid).toBeUndefined()
  })
})

describe('DiscoveryClient.createCompany', () => {
  it('posts exactly {dir, key, slug} to /v1/company/create', async () => {
    const { fetchImpl, calls } = fakeFetch([jsonResponse(200, { created: true })])
    await client(fetchImpl).createCompany({ dir: DIR, key: KEY, slug: 'acme' })
    expect(calls).toHaveLength(1)
    expect(calls[0].url).toBe(`${BEACOND_URL}/v1/company/create`)
    expect(calls[0].init?.method).toBe('POST')
    /* eslint-disable lucy/no-json-stringify */
    // Test-only wire-format assertion; @tribes-terminal/foundation is not a dependency here.
    expect(calls[0].init?.body).toBe(JSON.stringify({ dir: DIR, key: KEY, slug: 'acme' }))
    /* eslint-enable lucy/no-json-stringify */
  })

  it('200 {created:true} resolves true', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { created: true })])
    const created = await client(fetchImpl).createCompany({ dir: DIR, key: KEY, slug: 'acme' })
    expect(created).toBe(true)
  })

  it('200 {created:false} resolves false without throwing — the idempotent re-create', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { created: false })])
    const created = await client(fetchImpl).createCompany({ dir: DIR, key: KEY, slug: 'acme' })
    expect(created).toBe(false)
  })

  /** A SECOND DIRECTORY CLAIMING THE SAME DISPLAY WORD IS NOT A CONFLICT.
   *
   * This used to be `409 slug-taken`, and this client's own contract said so.
   * The refusal is deleted with the uniqueness it enforced: the pair is two
   * rows, and both creates are ordinary successes. */
  it('a same-named company in another directory is created, never refused', async () => {
    const { fetchImpl } = fakeFetch([
      jsonResponse(200, { created: true }),
      jsonResponse(200, { created: true })
    ])
    const discovery = client(fetchImpl)
    await expect(discovery.createCompany({ dir: DIR, key: KEY, slug: 'acme' })).resolves.toBe(true)
    await expect(
      discovery.createCompany({ dir: '/elsewhere/acme', key: 'cafebabe0011', slug: 'acme' })
    ).resolves.toBe(true)
  })

  it('makes exactly one request — no lookup pre-check', async () => {
    const { fetchImpl, calls } = fakeFetch([jsonResponse(200, { created: true })])
    await client(fetchImpl).createCompany({ dir: DIR, key: KEY, slug: 'acme' })
    expect(calls).toHaveLength(1)
  })
})

describe('DiscoveryClient.deleteCompany', () => {
  it('{deleted:true} resolves true', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { deleted: true })])
    const deleted = await client(fetchImpl).deleteCompany({ dir: DIR })
    expect(deleted).toBe(true)
  })

  it('{deleted:false} resolves false without throwing — idempotent', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { deleted: false })])
    const deleted = await client(fetchImpl).deleteCompany({ dir: DIR })
    expect(deleted).toBe(false)
  })

  it('makes exactly one request', async () => {
    const { fetchImpl, calls } = fakeFetch([jsonResponse(200, { deleted: true })])
    await client(fetchImpl).deleteCompany({ dir: DIR })
    expect(calls).toHaveLength(1)
  })
})

describe('DiscoveryClient.deregister', () => {
  it('200 {deregistered:true} resolves true', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { deregistered: true })])
    const deregistered = await client(fetchImpl).deregister({ dir: DIR, pid: 4242 })
    expect(deregistered).toBe(true)
  })

  it('200 {deregistered:false} resolves false without throwing', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { deregistered: false })])
    const deregistered = await client(fetchImpl).deregister({ dir: DIR, pid: 4242 })
    expect(deregistered).toBe(false)
  })

  it('409 pid mismatch resolves false without throwing — a live daemon reclaimed the company', async () => {
    const { fetchImpl } = fakeFetch([
      jsonResponse(409, { code: 'pid-mismatch', message: 'mismatch', recordedPid: 1 })
    ])
    const deregistered = await client(fetchImpl).deregister({ dir: DIR, pid: 4242 })
    expect(deregistered).toBe(false)
  })

  it('posts to /v1/deregister, never to /v1/company/delete', async () => {
    const { fetchImpl, calls } = fakeFetch([jsonResponse(200, { deregistered: true })])
    await client(fetchImpl).deregister({ dir: DIR, pid: 4242 })
    expect(calls[0].url).toBe(`${BEACOND_URL}/v1/deregister`)
  })
})

describe('DiscoveryClient.health', () => {
  it('true only for 200 + {"status":"ok"}', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { status: 'ok' })])
    await expect(client(fetchImpl).health()).resolves.toBe(true)
  })

  it('false for a 503', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(503, { status: 'schema-missing' })])
    await expect(client(fetchImpl).health()).resolves.toBe(false)
  })

  it('false for a non-JSON body', async () => {
    const { fetchImpl } = fakeFetch([textResponse(200, 'not json')])
    await expect(client(fetchImpl).health()).resolves.toBe(false)
  })

  it('false for a network throw — health never throws', async () => {
    const fetchImpl: typeof fetch = async () => {
      throw new TypeError('fetch failed')
    }
    await expect(client(fetchImpl).health()).resolves.toBe(false)
  })
})

describe('DiscoveryClient timeouts', () => {
  // A real, short AbortSignal.timeout — not `vi.useFakeTimers()`, which
  // FetchTransportTest.test.ts's own history records does not reliably drive
  // `AbortSignal.timeout`'s platform timer. The fetch mock never resolves on
  // its own and only settles when the real signal aborts, so this proves the
  // client surfaces the abort rather than hanging, without racing a server.
  it('every method aborts at timeoutMs and surfaces BeacondUnavailableError kind:"timeout"', async () => {
    let calls = 0
    const neverResolvesUntilAborted: typeof fetch = (_input, init) => {
      calls += 1
      return new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => {
          reject(init.signal?.reason instanceof Error ? init.signal.reason : new Error('aborted'))
        })
      })
    }

    const error = assertBeacondUnavailable(
      await captured(client(neverResolvesUntilAborted, 20).lookup(DIR))
    )
    expect(error.kind).toBe('timeout')
    expect(calls).toBe(1)
  })
})

describe('BeacondUnavailableError.kind truth table (ruling D6)', () => {
  it('a 500 with an HTML body is http-error carrying status', async () => {
    const { fetchImpl } = fakeFetch([textResponse(500, '<html>oops</html>')])
    const error = assertBeacondUnavailable(await captured(client(fetchImpl).lookup(DIR)))
    expect(error.kind).toBe('http-error')
    expect(error.status).toBe(500)
  })

  it('a 200 whose body is not JSON is malformed-body', async () => {
    const { fetchImpl } = fakeFetch([textResponse(200, 'not json')])
    const error = assertBeacondUnavailable(await captured(client(fetchImpl).lookup(DIR)))
    expect(error.kind).toBe('malformed-body')
  })

  it('a 200 whose row fails parseCompanyRow is malformed-body', async () => {
    const malformedCompany = { slug: 'acme' } // missing dir/key/registeredAt
    const { fetchImpl } = fakeFetch([jsonResponse(200, { found: true, company: malformedCompany })])
    const error = assertBeacondUnavailable(await captured(client(fetchImpl).lookup(DIR)))
    expect(error.kind).toBe('malformed-body')
  })

  /** A KEY THAT IS NOT A DIRECTORY HASH IS A MALFORMED ROW.
   *
   * The mistake the composite key made plausible: a producer filling `key`
   * with the company's NAME. Nothing downstream re-derives the key, so this
   * wire boundary is the only place that can catch it. */
  it('a 200 whose row carries a key that is not twelve lowercase hex is malformed-body', async () => {
    const { fetchImpl } = fakeFetch([
      jsonResponse(200, { found: true, company: { ...fullRow, key: 'acme' } })
    ])
    const error = assertBeacondUnavailable(await captured(client(fetchImpl).lookup(DIR)))
    expect(error.kind).toBe('malformed-body')
  })
})

describe('resolveCompanyChiefdUrl', () => {
  it('returns row.url when the company is located', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { found: true, company: fullRow })])
    const discovery = client(fetchImpl)
    await expect(resolveCompanyChiefdUrl(DIR, discovery)).resolves.toBe(fullRow.url)
  })

  it('throws UnknownCompanyError naming the directory when there is no row', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { found: false })])
    const discovery = client(fetchImpl)
    const error = assertUnknownCompany(
      await captured(resolveCompanyChiefdUrl('/work/ghost', discovery))
    )
    expect(error.dir).toBe('/work/ghost')
  })

  it('throws CompanyNotRunningError naming the directory when the row has no url', async () => {
    const { fetchImpl } = fakeFetch([jsonResponse(200, { found: true, company: noLocationRow })])
    const discovery = client(fetchImpl)
    const error = assertCompanyNotRunning(await captured(resolveCompanyChiefdUrl(DIR, discovery)))
    expect(error.dir).toBe(DIR)
  })

  it('UnknownCompanyError and CompanyNotRunningError are distinguishable by type', async () => {
    const { fetchImpl: fetchGhost } = fakeFetch([jsonResponse(200, { found: false })])
    const ghostError = await captured(resolveCompanyChiefdUrl('/work/ghost', client(fetchGhost)))

    const { fetchImpl: fetchStopped } = fakeFetch([
      jsonResponse(200, { found: true, company: noLocationRow })
    ])
    const stoppedError = await captured(resolveCompanyChiefdUrl(DIR, client(fetchStopped)))

    expect(ghostError).toBeInstanceOf(UnknownCompanyError)
    expect(stoppedError).toBeInstanceOf(CompanyNotRunningError)
    expect(ghostError).not.toBeInstanceOf(CompanyNotRunningError)
    expect(stoppedError).not.toBeInstanceOf(UnknownCompanyError)
  })
})
