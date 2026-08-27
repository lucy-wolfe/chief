import { fixedResponseTransport } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { ManifestClient } from '@/resources/Manifest'

describe('ManifestClient', () => {
  it('read: absent -> undefined', async () => {
    const transport = fixedResponseTransport(200, { found: false })
    const client = new ManifestClient(transport)
    await expect(client.read('acme')).resolves.toBeUndefined()
    expect(transport.calls[0]?.path).toBe('/v1/org/manifest/read')
  })

  it('read: found -> {manifest}', async () => {
    const transport = fixedResponseTransport(200, { found: true, manifest: '{"a":1}', seq: 3 })
    const client = new ManifestClient(transport)
    await expect(client.read('acme')).resolves.toEqual({ manifest: '{"a":1}' })
  })

  it('genesis: .created boolean', async () => {
    const transport = fixedResponseTransport(200, { created: true })
    const client = new ManifestClient(transport)
    await expect(
      client.genesis('acme', { name: 'Acme', purpose: 'p', chief: { name: 'Chief' } })
    ).resolves.toBe(true)
    const call = transport.calls[0]
    expect(call?.path).toBe('/v1/org/manifest/genesis')
    expect(call?.body).toMatchObject({
      slug: 'acme',
      // `spec`, not `manifest`. The route takes the QUESTION and derives the
      // manifest and the person contracts from it together, so they cannot
      // disagree; a caller that could post a
      // manifest could seed a company that never validated as a whole.
      spec: { name: 'Acme', purpose: 'p', chief: { name: 'Chief' } }
    })
    // `at` is synthesized from the wall clock when the caller supplies none
    // -- assert its presence/shape rather than an exact value (Mandate 3:
    // plumbing a clock read is not a business decision to pin bit-for-bit).
    const body = call?.body
    const at = body && typeof body === 'object' && 'at' in body ? body.at : undefined
    expect(typeof at).toBe('string')
  })

  it('genesis: a caller-supplied at passes through verbatim', async () => {
    const transport = fixedResponseTransport(200, { created: true })
    const client = new ManifestClient(transport)
    await client.genesis(
      'acme',
      { name: 'Acme', purpose: 'p', chief: { name: 'Chief' } },
      {
        at: '2026-08-04T01:00:00.000Z'
      }
    )
    expect(transport.calls[0]?.body).toEqual({
      slug: 'acme',
      spec: { name: 'Acme', purpose: 'p', chief: { name: 'Chief' } },
      at: '2026-08-04T01:00:00.000Z'
    })
  })

  /** The `slug` a route receives is the caller's, verbatim: it is already the
   * company key (`sha256(dir)[..12]`). The root-keyed rewrite this replaces
   * turned a display slug into `acme@074619b89d1b`, and is deleted with the
   * composite. */
  it('sends the company key it was given, untranslated', async () => {
    const transport = fixedResponseTransport(200, { found: false })
    const client = new ManifestClient(transport)
    await client.read('0123456789ab')
    expect(transport.calls[0]?.body).toEqual({ slug: '0123456789ab' })
  })

  it('500 -> ChiefdUnavailableError', async () => {
    const transport = fixedResponseTransport(500, { oops: true })
    const client = new ManifestClient(transport, 'http://x')
    await expect(client.read('acme')).rejects.toMatchObject({
      name: 'ChiefdUnavailableError',
      kind: 'http-error',
      status: 500
    })
  })
})
