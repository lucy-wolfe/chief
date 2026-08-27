import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { ChiefdUnavailableError } from '@/Errors'
import { DocsClient } from '@/resources/Docs'
import { ENSURE_SCHEMA_RETRY_DELAYS_MS } from '@/transport/RetryPolicy'
import type { HttpResponse, HttpTransport } from '@/types/Transport'

type Responder = (method: 'GET' | 'POST', path: string) => HttpResponse

class ScriptedTransport implements HttpTransport {
  calls: Array<{ method: 'GET' | 'POST'; path: string }> = []

  constructor(private readonly respond: Responder) {}

  async post(path: string, _body: unknown): Promise<HttpResponse> {
    this.calls.push({ method: 'POST', path })
    return this.respond('POST', path)
  }

  async get(path: string): Promise<HttpResponse> {
    this.calls.push({ method: 'GET', path })
    return this.respond('GET', path)
  }
}

class QueueTransport implements HttpTransport {
  calls = 0

  constructor(private readonly steps: Array<() => HttpResponse>) {}

  async post(_path: string, _body: unknown): Promise<HttpResponse> {
    const step = this.steps[this.calls]
    this.calls += 1
    if (!step) throw new Error('QueueTransport exhausted')
    return step()
  }

  async get(_path: string): Promise<HttpResponse> {
    return this.post(_path, undefined)
  }
}

describe('DocsClient.health', () => {
  it('is true only for 200 + {"status":"ok"}', async () => {
    const ok = new DocsClient(
      new ScriptedTransport(() => ({ status: 200, body: '{"status":"ok"}' }))
    )
    expect(await ok.health()).toBe(true)
  })

  it('is false for a 503 schema-missing body, but reachable() is still true', async () => {
    const transport = new ScriptedTransport(() => ({
      status: 503,
      body: '{"status":"schema-missing: org_documents absent"}'
    }))
    const docs = new DocsClient(transport)
    expect(await docs.health()).toBe(false)
    expect(await docs.reachable()).toBe(true)
  })

  it('is false when the transport throws', async () => {
    const transport = new ScriptedTransport(() => {
      throw new ChiefdUnavailableError({
        kind: 'unreachable',
        url: 'http://x',
        path: '/v1/docs/health'
      })
    })
    const docs = new DocsClient(transport)
    expect(await docs.health()).toBe(false)
    expect(await docs.reachable()).toBe(false)
  })
})

describe('DocsClient.probe', () => {
  it('reports the runtime identity verbatim on success', async () => {
    const transport = new ScriptedTransport((_method, path) => {
      if (path === '/v1/docs/health') return { status: 200, body: '{"status":"ok"}' }
      return { status: 200, body: '{"mode":"company","company":"acme"}' }
    })
    const probe = await new DocsClient(transport).probe()
    expect(probe).toEqual({
      ok: true,
      httpStatus: 200,
      reason: 'ok',
      runtimeMode: 'company',
      company: 'acme'
    })
  })

  it('reports a runtime identity that does not match the caller unchanged — no tolerance rule', async () => {
    const transport = new ScriptedTransport((_method, path) => {
      if (path === '/v1/docs/health') return { status: 200, body: '{"status":"ok"}' }
      return { status: 200, body: '{"mode":"company","company":"a-totally-different-company"}' }
    })
    const probe = await new DocsClient(transport).probe()
    expect(probe.company).toBe('a-totally-different-company')
  })

  it('reports a reason and no runtime fields on a failed health check', async () => {
    const transport = new ScriptedTransport(() => ({
      status: 503,
      body: '{"status":"schema-missing: org_documents absent"}'
    }))
    const probe = await new DocsClient(transport).probe()
    expect(probe).toEqual({
      ok: false,
      httpStatus: 503,
      reason: 'schema-missing: org_documents absent'
    })
  })

  it('reports ok:false with a message reason when nothing answers', async () => {
    const transport = new ScriptedTransport(() => {
      throw new ChiefdUnavailableError({
        kind: 'unreachable',
        url: 'http://x',
        path: '/v1/docs/health'
      })
    })
    const probe = await new DocsClient(transport).probe()
    expect(probe.ok).toBe(false)
    expect(probe.httpStatus).toBeUndefined()
    expect(typeof probe.reason).toBe('string')
  })
})

describe('DocsClient.ensureSchemaReady', () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('retries an unreachable-then-ok sequence exactly per the ladder', async () => {
    const transport = new QueueTransport([
      () => {
        throw new ChiefdUnavailableError({
          kind: 'unreachable',
          url: 'http://x',
          path: '/v1/docs/ensure-schema'
        })
      },
      () => {
        throw new ChiefdUnavailableError({
          kind: 'unreachable',
          url: 'http://x',
          path: '/v1/docs/ensure-schema'
        })
      },
      () => ({ status: 200, body: '{"ok":true}' })
    ])
    const docs = new DocsClient(transport)

    const settled = docs.ensureSchemaReady()
    await vi.advanceTimersByTimeAsync(ENSURE_SCHEMA_RETRY_DELAYS_MS[0])
    await vi.advanceTimersByTimeAsync(ENSURE_SCHEMA_RETRY_DELAYS_MS[1])
    await settled

    expect(transport.calls).toBe(3)
  })

  it('never retries a 500 — the POST simply resolves and schema is marked ready', async () => {
    const transport = new QueueTransport([() => ({ status: 500, body: 'internal error' })])
    const docs = new DocsClient(transport)

    await docs.ensureSchemaReady()

    expect(transport.calls).toBe(1)
    // A second call is a no-op (memoized) — proves it did not loop internally.
    await docs.ensureSchemaReady()
    expect(transport.calls).toBe(1)
  })

  it('rethrows a timeout without retrying', async () => {
    const transport = new QueueTransport([
      () => {
        throw new ChiefdUnavailableError({
          kind: 'timeout',
          url: 'http://x',
          path: '/v1/docs/ensure-schema'
        })
      }
    ])
    const docs = new DocsClient(transport)

    await expect(docs.ensureSchemaReady()).rejects.toBeInstanceOf(ChiefdUnavailableError)
    expect(transport.calls).toBe(1)
  })
})
