import { describe, expect, it } from 'vitest'

import { AuthAcquisitionError } from '@/Errors'
import { AgentTokenManager } from '@/resources/AgentToken'
import { generateAgentKeypair } from '@/resources/Identity'
import type { HttpResponse, HttpTransport } from '@/types/Transport'

class ScriptedTransport implements HttpTransport {
  posts: Array<{ path: string; body: unknown }> = []

  constructor(private readonly respond: (path: string, body: unknown) => HttpResponse) {}

  async post(path: string, body: unknown): Promise<HttpResponse> {
    this.posts.push({ path, body })
    return this.respond(path, body)
  }

  async get(path: string): Promise<HttpResponse> {
    return this.post(path, undefined)
  }
}

function json(value: unknown): string {
  /* eslint-disable lucy/no-json-stringify */
  // Test-only fixture serialization; @tribes-terminal/foundation is not a
  // dependency anywhere in this workspace (see FetchTransport.ts's matching
  // disable block, E2-S1).
  return JSON.stringify(value)
  /* eslint-enable lucy/no-json-stringify */
}

describe('AgentTokenManager', () => {
  const keypair = generateAgentKeypair()

  function happyTransport(): ScriptedTransport {
    return new ScriptedTransport((path) => {
      if (path === '/v1/auth/challenge') {
        return { status: 200, body: json({ nonceId: 'nonce-1', nonce: 'the-nonce' }) }
      }
      if (path === '/v1/auth/token') {
        return { status: 200, body: json({ token: 'jwt-1' }) }
      }
      throw new Error(`unexpected path ${path}`)
    })
  }

  it('first authHeader() performs challenge+token POSTs; a second call is cached', async () => {
    const transport = happyTransport()
    const manager = new AgentTokenManager(transport, 'person-1', keypair.privatePkcs8Pem)

    const first = await manager.authHeader()
    expect(first).toEqual({ Authorization: 'Bearer jwt-1' })
    expect(transport.posts.map((call) => call.path)).toEqual([
      '/v1/auth/challenge',
      '/v1/auth/token'
    ])
    expect(transport.posts[0]?.body).toEqual({ identityId: 'person-1' })

    const second = await manager.authHeader()
    expect(second).toEqual({ Authorization: 'Bearer jwt-1' })
    expect(transport.posts).toHaveLength(2) // no new round trip
  })

  it('invalidate() forces the next authHeader() to re-acquire', async () => {
    const transport = happyTransport()
    const manager = new AgentTokenManager(transport, 'person-1', keypair.privatePkcs8Pem)
    await manager.authHeader()
    manager.invalidate()
    await manager.authHeader()
    expect(transport.posts).toHaveLength(4)
  })

  it('authHeader() resolves undefined on transport failure; acquire() rejects with AuthAcquisitionError', async () => {
    const transport = new ScriptedTransport(() => ({ status: 500, body: 'boom' }))
    const manager = new AgentTokenManager(transport, 'person-1', keypair.privatePkcs8Pem)

    await expect(manager.authHeader()).resolves.toBeUndefined()
    await expect(manager.acquire()).rejects.toBeInstanceOf(AuthAcquisitionError)
  })

  it('currentToken() reflects the cached token', async () => {
    const transport = happyTransport()
    const manager = new AgentTokenManager(transport, 'person-1', keypair.privatePkcs8Pem)
    expect(manager.currentToken()).toBeUndefined()
    await manager.authHeader()
    expect(manager.currentToken()).toBe('jwt-1')
  })
})
