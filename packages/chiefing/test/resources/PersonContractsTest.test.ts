import { fixedResponseTransport, RecordingTransport } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { PersonContractsClient } from '@/resources/PersonContracts'

const DOCUMENT = {
  version: 1 as const,
  organization: 'acme',
  contracts: {
    p1: { text: 'be excellent', md5: 'abc123' }
  }
}

describe('PersonContractsClient', () => {
  it('read: absent -> {found:false}', async () => {
    const transport = fixedResponseTransport(200, { found: false })
    const client = new PersonContractsClient(transport)
    await expect(client.read('acme')).resolves.toEqual({ found: false })
  })

  it('read: the contracts string parses back into the document', async () => {
    /* eslint-disable lucy/no-json-stringify */
    // Test-only fixture encoder — @tribes-terminal/foundation is not a
    // dependency anywhere in this workspace (see FetchTransportTest.test.ts's
    // matching disable block).
    const serializedDocument = JSON.stringify(DOCUMENT)
    /* eslint-enable lucy/no-json-stringify */

    const readTransport = fixedResponseTransport(200, {
      found: true,
      contracts: serializedDocument
    })
    const readClient = new PersonContractsClient(readTransport)
    await expect(readClient.read('acme')).resolves.toEqual({ found: true, document: DOCUMENT })
    expect(readTransport.calls[0]?.path).toBe('/v1/org/person-contracts/read')
  })

  // The publish and its route are deleted (the publisher-route sweep found no
  // caller). Contracts are written in-process through `CompanyDb` inside the
  // daemon's own transactions.
  it('the caller-less publish stays deleted', () => {
    const client = new PersonContractsClient(fixedResponseTransport(200, {}))
    expect('publish' in client).toBe(false)
  })

  /** The `slug` a route receives is the caller's, verbatim: it is already the
   * company key (`sha256(dir)[..12]`, served on the beacond row and in the
   * daemon rendezvous). The root-keyed rewrite this replaces turned a display
   * slug into `acme@074619b89d1b`, and is deleted with the composite. */
  it('sends the company key it was given, untranslated', async () => {
    const transport = fixedResponseTransport(200, { found: false })
    const client = new PersonContractsClient(transport)
    await client.read('0123456789ab')
    expect(transport.calls[0]?.body).toEqual({ slug: '0123456789ab' })
  })

  it('422 refusal -> PersonContractsRefusalError, never OrgRowRefusalError', async () => {
    const transport = fixedResponseTransport(422, {
      code: 'person-contracts-invalid',
      detail: 'bad shape'
    })
    const client = new PersonContractsClient(transport)
    await expect(client.read('acme')).rejects.toMatchObject({
      name: 'PersonContractsRefusalError',
      code: 'person-contracts-invalid',
      detail: 'bad shape'
    })
  })

  it('500 -> ChiefdUnavailableError', async () => {
    const transport = new RecordingTransport(() => ({ status: 500, body: 'boom' }))
    const client = new PersonContractsClient(transport)
    await expect(client.read('acme')).rejects.toMatchObject({
      name: 'ChiefdUnavailableError',
      kind: 'http-error',
      status: 500
    })
  })
})
