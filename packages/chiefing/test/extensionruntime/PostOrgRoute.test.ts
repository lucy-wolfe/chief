import { fixedResponseTransport, RecordingTransport } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { ChiefdUnavailableError, OrgRowRefusalError, postOrgRoute } from '@/extensionruntime/index'

const url = 'http://pane-chiefd.example'
const path = '/v1/org/example'

describe('extension-runtime postOrgRoute', () => {
  it('is publicly imported and returns parsed 2xx JSON through its supplied transport', async () => {
    const transport = fixedResponseTransport(200, { applied: true, seq: 7 })

    await expect(
      postOrgRoute<{ applied: boolean; seq: number }>(transport, url, path, { value: 1 })
    ).resolves.toEqual({ applied: true, seq: 7 })
    expect(transport.calls).toEqual([{ method: 'POST', path, body: { value: 1 } }])
  })

  it('maps a malformed 2xx body to ChiefdUnavailableError with endpoint context', async () => {
    const transport = new RecordingTransport(() => ({ status: 200, body: 'not json' }))
    const request = postOrgRoute(transport, url, path, {})

    await expect(request).rejects.toBeInstanceOf(ChiefdUnavailableError)
    await expect(request).rejects.toMatchObject({
      name: 'ChiefdUnavailableError',
      kind: 'malformed-body',
      url,
      path,
      status: 200
    } satisfies Partial<ChiefdUnavailableError>)
  })

  // #1004: the table IS the taxonomy's refusal set. 403 and 409 were missing,
  // so an authorization refusal and a lost fence both reached the agent as
  // `chiefd unavailable (http-error)` — the string that makes an agent retry a
  // rule that will never answer differently.
  for (const refusal of [
    { status: 400, body: { code: 'invalid-request', message: 'bad input' } },
    { status: 403, body: { code: 'requester-identity-mismatch', detail: 'not you' } },
    { status: 404, body: { code: 'unknown-company', detail: 'not found' } },
    { status: 409, body: { code: 'seq-conflict', detail: 'expected 4, actual 5' } },
    { status: 422, body: { code: 'policy-refused', detail: 'not allowed' } }
  ] as const) {
    it(`maps ${refusal.status} to OrgRowRefusalError`, async () => {
      const transport = fixedResponseTransport(refusal.status, refusal.body)
      const expectedDetail = 'detail' in refusal.body ? refusal.body.detail : refusal.body.message
      const request = postOrgRoute(transport, url, path, {})

      await expect(request).rejects.toBeInstanceOf(OrgRowRefusalError)
      await expect(request).rejects.toMatchObject({
        name: 'OrgRowRefusalError',
        status: refusal.status,
        code: refusal.body.code,
        detail: expectedDetail
      } satisfies Partial<OrgRowRefusalError>)
    })
  }

  // The other half of the same contract: the statuses that mean "chiefd could
  // not answer" must NEVER decode as a refusal, or the fix would have traded
  // one indistinguishable pair for another.
  for (const status of [429, 500, 502, 503] as const) {
    it(`keeps ${status} an outage, never a refusal`, async () => {
      const transport = fixedResponseTransport(status, { code: 'busy', detail: 'waited 4s' })
      const request = postOrgRoute(transport, url, path, {})

      await expect(request).rejects.toBeInstanceOf(ChiefdUnavailableError)
      await expect(request).rejects.toMatchObject({ kind: 'http-error', status })
    })
  }

  it('maps a non-refusal 503 to ChiefdUnavailableError with path and status', async () => {
    const transport = new RecordingTransport(() => ({
      status: 503,
      body: 'temporarily unavailable'
    }))
    const request = postOrgRoute(transport, url, path, {})

    await expect(request).rejects.toBeInstanceOf(ChiefdUnavailableError)
    await expect(request).rejects.toMatchObject({
      name: 'ChiefdUnavailableError',
      kind: 'http-error',
      url,
      path,
      status: 503
    } satisfies Partial<ChiefdUnavailableError>)
  })
})
