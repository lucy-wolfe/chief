import { describe, expect, it } from 'vitest'

import type {
  HttpResponse,
  HttpTransport,
  SemanticQueueInsertResult
} from '@/extensionruntime/index'
import { RowStoresClient } from '@/extensionruntime/index'

class RecordingTransport implements HttpTransport {
  readonly calls: Array<{ path: string; body: unknown }> = []

  async post(path: string, body: unknown): Promise<HttpResponse> {
    this.calls.push({ path, body })
    return { status: 200, body: '{"status":"inserted","seq":7}' }
  }

  async get(_path: string): Promise<HttpResponse> {
    return { status: 500, body: '{}' }
  }
}

describe('extension-runtime semantic queue insert DTOs', () => {
  it('preserves the operator-escalations legacy route, body, and decoded result', async () => {
    const transport = new RecordingTransport()
    const client = new RowStoresClient(transport)
    const result: SemanticQueueInsertResult = await client.insertOperatorEscalationIntent('acme', {
      fingerprint: 'f1'
    })

    expect(result).toEqual({ status: 'inserted', seq: 7 })
    expect(transport.calls).toEqual([
      {
        path: '/v1/org/operator-escalation-intents/insert',
        body: { slug: 'acme', intent: { fingerprint: 'f1' } }
      }
    ])
  })
})
