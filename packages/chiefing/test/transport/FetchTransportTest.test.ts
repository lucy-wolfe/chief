import type { IncomingMessage, Server, ServerResponse } from 'node:http'
import { createServer } from 'node:http'

import { afterEach, describe, expect, it, vi } from 'vitest'

import { ChiefdUnavailableError } from '@/Errors'
import { FetchTransport } from '@/transport/FetchTransport'
import { CONNECT_RETRY_BACKOFFS_MS } from '@/transport/RetryPolicy'

function readBody(request: IncomingMessage): Promise<string> {
  return new Promise((resolve) => {
    let body = ''
    request.on('data', (chunk: Buffer) => {
      body += chunk.toString('utf8')
    })
    request.on('end', () => resolve(body))
  })
}

/** Starts a `node:http` server on an ephemeral loopback port and resolves its
 * base URL. The Node server API keeps the test compiling without
 * ambient Bun types (this package's tsconfig only declares `"types": ["node"]`). */
function listen(
  handler: (request: IncomingMessage, response: ServerResponse) => void
): Promise<{ server: Server; baseUrl: string }> {
  return new Promise((resolve, reject) => {
    const server = createServer(handler)
    server.on('error', reject)
    server.listen(0, '127.0.0.1', () => {
      const address = server.address()
      if (!address || typeof address === 'string') {
        reject(new Error('expected a bound AddressInfo'))
        return
      }
      resolve({ server, baseUrl: `http://127.0.0.1:${address.port}` })
    })
  })
}

function close(server: Server): Promise<void> {
  return new Promise((resolve) => server.close(() => resolve()))
}

describe('FetchTransport against a real local server', () => {
  it('POST sends content-type json + JSON body; GET sends no body', async () => {
    let seenPostContentType: string | null = null
    let seenPostBody = ''
    let seenGetBody = ''
    const { server, baseUrl } = await listen((request, response) => {
      void readBody(request).then((body) => {
        if (request.method === 'POST') {
          seenPostContentType = request.headers['content-type'] ?? null
          seenPostBody = body
        } else {
          seenGetBody = body
        }
        /* eslint-disable lucy/no-json-stringify */
        // Test-only HTTP fixture body; @tribes-terminal/foundation is not a
        // dependency anywhere in this workspace (see FetchTransport.ts's
        // matching disable block).
        response.writeHead(200, { 'content-type': 'application/json' })
        response.end(JSON.stringify({ ok: true }))
        /* eslint-enable lucy/no-json-stringify */
      })
    })
    try {
      const transport = new FetchTransport(baseUrl)
      await transport.post('/p', { a: 1 })
      await transport.get('/g')
      expect(seenPostContentType).toBe('application/json')
      /* eslint-disable lucy/no-json-stringify */
      // Expected-wire-format assertion mirrors FetchTransport.post's own
      // JSON.stringify(body) contract (E2-S1's behavior spec item 1).
      expect(seenPostBody).toBe(JSON.stringify({ a: 1 }))
      /* eslint-enable lucy/no-json-stringify */
      expect(seenGetBody).toBe('')
    } finally {
      await close(server)
    }
  })

  it('auth header provider is awaited per request; changes between calls are honored', async () => {
    const seenAuth: (string | null)[] = []
    const { server, baseUrl } = await listen((request, response) => {
      seenAuth.push(request.headers.authorization ?? null)
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end('{}')
    })
    try {
      let token = 'first'
      const transport = new FetchTransport(baseUrl, undefined, async () => ({
        authorization: `Bearer ${token}`
      }))
      await transport.get('/g')
      token = 'second'
      await transport.get('/g')
      expect(seenAuth).toEqual(['Bearer first', 'Bearer second'])
    } finally {
      await close(server)
    }
  })

  it('a provider resolving undefined adds no headers', async () => {
    let seenAuth: string | null = 'unset-sentinel'
    const { server, baseUrl } = await listen((request, response) => {
      seenAuth = request.headers.authorization ?? null
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end('{}')
    })
    try {
      const transport = new FetchTransport(baseUrl, undefined, async () => undefined)
      await transport.get('/g')
      expect(seenAuth).toBeNull()
    } finally {
      await close(server)
    }
  })

  it('a provider that throws propagates as a caller bug', async () => {
    const transport = new FetchTransport('http://127.0.0.1:1', undefined, async () => {
      throw new Error('boom')
    })
    await expect(transport.get('/g')).rejects.toThrow('boom')
  })

  it('any 2xx-5xx response is returned as HttpResponse, unmapped', async () => {
    const { server, baseUrl } = await listen((_request, response) => {
      response.writeHead(500, { 'content-type': 'text/plain' })
      response.end('server exploded')
    })
    try {
      const transport = new FetchTransport(baseUrl)
      const response = await transport.get('/g')
      expect(response.status).toBe(500)
      expect(response.body).toBe('server exploded')
    } finally {
      await close(server)
    }
  })

  // A previous version of this test raced a real HTTP server against a real
  // 20ms AbortSignal.timeout, asserting the server-side handler had already
  // incremented a request counter by the time the client observed the abort.
  // That is a wall-clock race, not a property of FetchTransport: under CPU
  // load the client's abort can win before the accepted connection reaches
  // the handler, so the counter reads 0 and the assertion flakes — measured
  // by the merger at 1 failure in 4 runs once the workspace got busier
  // (#770 addendum). The invariant that actually matters — "no retry" — does
  // not require the request to have LANDED at all; it only requires the
  // client made exactly one fetch() call and surfaced the timeout instead of
  // retrying. Asserting that at the fetch() call boundary (not the server's
  // handler) makes the test hold regardless of scheduling.
  it('a timeout aborts and throws ChiefdUnavailableError kind timeout, with no retry', async () => {
    let calls = 0
    vi.stubGlobal('fetch', (_url: string, init?: { signal?: AbortSignal }) => {
      calls += 1
      return new Promise<Response>((_resolve, reject) => {
        const signal = init?.signal
        if (!signal) return
        signal.addEventListener('abort', () => {
          reject(signal.reason instanceof Error ? signal.reason : new Error('aborted'))
        })
      })
    })

    try {
      const transport = new FetchTransport('http://127.0.0.1:1', 20)
      let error: unknown
      try {
        await transport.get('/slow')
      } catch (caught) {
        error = caught
      }
      expect(error).toBeInstanceOf(ChiefdUnavailableError)
      if (!(error instanceof ChiefdUnavailableError)) {
        throw new Error('expected ChiefdUnavailableError')
      }
      expect(error.kind).toBe('timeout')
      expect(error.url).toBe('http://127.0.0.1:1')
      expect(error.path).toBe('/slow')
      // The whole point of the assertion: exactly one fetch() call, no retry
      // ladder entered for a timeout — proven at the call boundary, not by
      // racing a server's handler entry against the client's abort.
      expect(calls).toBe(1)
    } finally {
      vi.unstubAllGlobals()
    }
  })
})

describe('FetchTransport against a refused connection', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('retries exactly CONNECT_RETRY_BACKOFFS_MS.length + 1 attempts then throws ChiefdUnavailableError kind unreachable', async () => {
    let calls = 0
    vi.stubGlobal('fetch', async () => {
      calls += 1
      const cause = Object.assign(new Error('connect ECONNREFUSED'), { code: 'ECONNREFUSED' })
      throw Object.assign(new TypeError('fetch failed'), { cause })
    })

    const transport = new FetchTransport('http://127.0.0.1:1')
    let error: unknown
    try {
      await transport.get('/p')
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ChiefdUnavailableError)
    if (!(error instanceof ChiefdUnavailableError)) {
      throw new Error('expected ChiefdUnavailableError')
    }
    expect(error.kind).toBe('unreachable')
    expect(calls).toBe(CONNECT_RETRY_BACKOFFS_MS.length + 1)
  })

  // #953: the mocked case above only ever exercised Node's uppercase
  // `ECONNREFUSED` shape. Bun's OWN native fetch — the runtime every real
  // chiefd caller in this repo actually runs on — throws a refused
  // connection with `error.name === 'Error'` and `error.code ===
  // 'ConnectionRefused'` (mixed case, verified directly against a real
  // refused connection, not assumed from documentation). `UNREACHABLE_CODES`
  // only listed the Node-shaped code, so this exact case fell through to
  // `'unknown'` and the raw, unwrapped fetch error reached every caller of
  // `isTransientChiefdError` — discovered only because #953 exercised a
  // real chiefd outage rather than a mock shaped like the existing test.
  it("classifies Bun's own mixed-case 'ConnectionRefused' code as unreachable, not unknown", async () => {
    let calls = 0
    vi.stubGlobal('fetch', async () => {
      calls += 1
      throw Object.assign(new Error('Unable to connect. Is the computer able to access the url?'), {
        code: 'ConnectionRefused',
        errno: 0
      })
    })

    const transport = new FetchTransport('http://127.0.0.1:1')
    let error: unknown
    try {
      await transport.get('/p')
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ChiefdUnavailableError)
    if (!(error instanceof ChiefdUnavailableError)) {
      throw new Error('expected ChiefdUnavailableError')
    }
    expect(error.kind).toBe('unreachable')
    expect(calls).toBe(CONNECT_RETRY_BACKOFFS_MS.length + 1)
  })

  // No mock at all: a real closed TCP port, the same failure mode a real
  // dead chiefd produces. Ties the classifier to whatever this repo's
  // ACTUAL runtime fetch does today, immune to drift if a future runtime
  // upgrade changes the mocked shape without anyone updating the mock.
  it('REAL: a real closed port (no server listening) classifies as unreachable, not unknown', async () => {
    const { server, baseUrl } = await listen(() => {})
    const closedPortUrl = baseUrl
    await close(server)

    const transport = new FetchTransport(closedPortUrl)
    let error: unknown
    try {
      await transport.get('/p')
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ChiefdUnavailableError)
    if (!(error instanceof ChiefdUnavailableError)) {
      throw new Error('expected ChiefdUnavailableError')
    }
    expect(error.kind).toBe('unreachable')
  })

  // --- the 401 re-acquire ---------------------------------------------------
  //
  // The defect these pin: the daemon's HS256 secret is ephemeral unless a
  // secret file was provisioned, so a chiefd restart rotates it and every
  // cached bearer dies at once. The client cached its token forever and
  // `invalidate()` had no production caller, so every org tool call from every
  // surviving pane 401ed until the pane was respawned.

  it('re-acquires and retries exactly once when a request comes back 401', async () => {
    const issued: string[] = ['stale-token', 'fresh-token']
    let cursor = 0
    const seen: (string | undefined)[] = []

    const { server, baseUrl } = await listen((request, response) => {
      const auth = request.headers.authorization
      seen.push(typeof auth === 'string' ? auth : undefined)
      if (auth === 'Bearer fresh-token') {
        response.writeHead(200, { 'content-type': 'application/json' })
        response.end('{"ok":true}')
        return
      }
      response.writeHead(401)
      response.end('unauthorized')
    })

    const transport = new FetchTransport(
      baseUrl,
      undefined,
      async () => ({ Authorization: `Bearer ${issued[cursor]}` }),
      () => {
        cursor += 1
      }
    )

    const result = await transport.get('/v1/org/anything')
    await close(server)

    expect(result.status).toBe(200)
    expect(seen).toEqual(['Bearer stale-token', 'Bearer fresh-token'])
  })

  it('retries a 401 at most once, so an identity that is genuinely refused fails fast', async () => {
    let requests = 0
    let invalidations = 0

    const { server, baseUrl } = await listen((_request, response) => {
      requests += 1
      response.writeHead(401)
      response.end('unauthorized')
    })

    const transport = new FetchTransport(
      baseUrl,
      undefined,
      async () => ({ Authorization: 'Bearer never-valid' }),
      () => {
        invalidations += 1
      }
    )

    const result = await transport.get('/v1/org/anything')
    await close(server)

    expect(result.status).toBe(401)
    expect(requests).toBe(2)
    expect(invalidations).toBe(1)
  })

  it('does not retry a non-401 refusal, because re-acquiring a token cannot fix it', async () => {
    let requests = 0
    let invalidations = 0

    const { server, baseUrl } = await listen((_request, response) => {
      requests += 1
      response.writeHead(403)
      response.end('forbidden')
    })

    const transport = new FetchTransport(
      baseUrl,
      undefined,
      async () => ({ Authorization: 'Bearer valid-but-unauthorized' }),
      () => {
        invalidations += 1
      }
    )

    const result = await transport.get('/v1/org/anything')
    await close(server)

    expect(result.status).toBe(403)
    expect(requests).toBe(1)
    expect(invalidations).toBe(0)
  })

  it('leaves a 401 alone when no invalidate hook was supplied, rather than looping', async () => {
    let requests = 0
    const { server, baseUrl } = await listen((_request, response) => {
      requests += 1
      response.writeHead(401)
      response.end('unauthorized')
    })

    const transport = new FetchTransport(baseUrl, undefined, async () => ({
      Authorization: 'Bearer stale'
    }))

    const result = await transport.get('/v1/org/anything')
    await close(server)

    expect(result.status).toBe(401)
    expect(requests).toBe(1)
  })
})
