// @vitest-environment jsdom
// This file is named `.ts` (per #807's Contract), not `.tsx` — component
// rendering below uses `createElement` rather than JSX syntax, which a
// `.ts` file's parser rejects.
import { act, createElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { ApiSessionProvider, useAccessToken, useChiefApi } from '@/providers/ApiSessionProvider'
import { SessionClientService } from '@/services/SessionClientService'
import type { FetchImpl } from '@/types/Fetch'

function json(value: unknown): string {
  /* eslint-disable lucy/no-json-stringify */
  // Test-only fixture serialization; @tribes-terminal/foundation is not a
  // dependency anywhere in this workspace (see FetchTransport.ts's matching
  // disable block, E2-S1).
  return JSON.stringify(value)
  /* eslint-enable lucy/no-json-stringify */
}

describe('SessionClientService', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('acquire() POSTs /api/session and returns { token, identityId }', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          new Response(json({ token: 'jwt-1', identityId: 'operator' }), {
            status: 200,
            headers: { 'content-type': 'application/json' }
          })
      )
    )
    const session = new SessionClientService()
    const result = await session.acquire()
    expect(result).toEqual({ token: 'jwt-1', identityId: 'operator' })
    expect(vi.mocked(fetch)).toHaveBeenCalledWith(
      '/api/session',
      expect.objectContaining({ method: 'POST' })
    )
  })

  it('acquire() returns { token: null } in auth-off mode', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          new Response(json({ token: null, identityId: 'operator' }), {
            status: 200,
            headers: { 'content-type': 'application/json' }
          })
      )
    )
    const session = new SessionClientService()
    const result = await session.acquire()
    expect(result.token).toBeNull()
  })

  it('throws on a non-200 response', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('boom', { status: 500 }))
    )
    const session = new SessionClientService()
    await expect(session.acquire()).rejects.toThrow()
  })
})

describe('ApiSessionProvider', () => {
  let container: HTMLDivElement
  let root: Root

  beforeEach(() => {
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
    localStorage.clear()
    sessionStorage.clear()
  })

  afterEach(() => {
    act(() => {
      root.unmount()
    })
    container.remove()
    vi.unstubAllGlobals()
  })

  it('acquires a token on mount and exposes it via useAccessToken/useChiefApi', async () => {
    let sessionCalls = 0
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        if (input.toString() === '/api/session') {
          sessionCalls += 1
          return new Response(json({ token: 'jwt-first', identityId: 'operator' }), {
            status: 200,
            headers: { 'content-type': 'application/json' }
          })
        }
        return new Response('not found', { status: 404 })
      })
    )

    const seen: Array<string | null> = []
    function Probe(): null {
      const accessToken = useAccessToken()
      useChiefApi() // proves the hook resolves without throwing outside the provider
      seen.push(accessToken())
      return null
    }

    await act(async () => {
      root.render(createElement(ApiSessionProvider, null, createElement(Probe)))
    })
    // Flush the mount effect's acquire() microtask/rerender.
    await act(async () => {
      await Promise.resolve()
    })

    expect(sessionCalls).toBe(1)
    expect(seen.at(-1)).toBe('jwt-first')
  })

  it('nothing is ever written to localStorage or sessionStorage across an acquire cycle', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          new Response(json({ token: 'jwt-1', identityId: 'operator' }), {
            status: 200,
            headers: { 'content-type': 'application/json' }
          })
      )
    )

    await act(async () => {
      root.render(createElement(ApiSessionProvider, null, createElement('div')))
    })
    await act(async () => {
      await Promise.resolve()
    })

    expect(localStorage.length).toBe(0)
    expect(sessionStorage.length).toBe(0)
  })

  it("the api client's onUnauthorized hook re-acquires and the retried request uses the new token", async () => {
    // Mirrors what ApiSessionProvider wires internally (SessionClientService
    // + a token ref feeding ChiefApiClientService's accessToken/onUnauthorized)
    // without racing React's own scheduling — this is the same `refresh()`
    // logic the provider's mount effect and 401 hook both call.
    const tokens = ['jwt-first', 'jwt-second']
    let sessionCalls = 0
    const sessionFetch: FetchImpl = async () => {
      const token = tokens[sessionCalls] ?? 'jwt-fallback'
      sessionCalls += 1
      return new Response(json({ token, identityId: 'operator' }), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      })
    }
    const session = new SessionClientService(sessionFetch)

    let currentToken: string | null = null
    const refresh = async (): Promise<void> => {
      const result = await session.acquire()
      currentToken = result.token
    }
    await refresh()
    expect(currentToken).toBe('jwt-first')

    const authorizationsSeen: (string | undefined)[] = []
    const apiFetch: FetchImpl = async (_input, init) => {
      const headers = new Headers(init?.headers)
      authorizationsSeen.push(headers.get('authorization') ?? undefined)
      const stillStale = authorizationsSeen.length === 1
      if (stillStale) {
        return new Response(json({ error: { code: 'unauthorized', detail: 'stale' } }), {
          status: 401,
          headers: { 'content-type': 'application/json' }
        })
      }
      // apps/api's real `/health` body: `{ok, service, agents:{running}}`.
      // This stub used to send a `version` the handler has never served.
      return new Response(json({ ok: true, service: 'chief-api', agents: { running: 0 } }), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      })
    }
    const { ChiefApiClientService } = await import('@/services/ChiefApiClientService')
    const client = new ChiefApiClientService({
      baseUrl: 'http://fake-api.test',
      accessToken: () => currentToken,
      fetchImpl: apiFetch,
      onUnauthorized: refresh
    })

    const health = await client.health()
    expect(health.ok).toBe(true)
    expect(authorizationsSeen).toEqual(['Bearer jwt-first', 'Bearer jwt-second'])
    expect(currentToken).toBe('jwt-second')
    expect(sessionCalls).toBe(2)
  })
})
