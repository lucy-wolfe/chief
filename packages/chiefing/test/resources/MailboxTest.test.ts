import { fixedResponseTransport } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { MailboxClient } from '@/resources/Mailbox'

describe('MailboxClient', () => {
  it('read: mailbox field normalizes to document', async () => {
    const transport = fixedResponseTransport(200, { found: true, mailbox: '{"m":1}', seq: 5 })
    const client = new MailboxClient(transport)
    await expect(client.read('acme')).resolves.toEqual({
      found: true,
      seq: 5,
      unchanged: undefined,
      document: '{"m":1}'
    })
    expect(transport.calls[0]?.path).toBe('/v1/org/mailbox/read')
  })

  // The whole-mailbox `publish` and its route are deleted (the
  // publisher-route sweep found no caller). The mailbox is written by the
  // send/delivery verbs and by the O(delta) path tested below.
  it('the caller-less whole-mailbox publish stays deleted', () => {
    const client = new MailboxClient(fixedResponseTransport(200, {}))
    expect('publish' in client).toBe(false)
  })

  it('readPerson: ifSeqNot conditional read', async () => {
    const transport = fixedResponseTransport(200, { found: true, seq: 9, unchanged: true })
    const client = new MailboxClient(transport)
    await expect(client.readPerson('acme', 'p1', { ifSeqNot: 9 })).resolves.toEqual({
      found: true,
      seq: 9,
      unchanged: true,
      document: undefined
    })
    expect(transport.calls[0]?.path).toBe('/v1/org/mailbox/read-person')
    expect(transport.calls[0]?.body).toEqual({ slug: 'acme', personId: 'p1', ifSeqNot: 9 })
  })

  it('delta: body shape {slug, personId, upserts, deletes, at} — the O(delta) fence-free path', async () => {
    const transport = fixedResponseTransport(200, { applied: true, seq: 10 })
    const client = new MailboxClient(transport)
    await client.delta('acme', 'p1', '[{"envelopeId":"e1"}]', ['e0'], '2026-08-04T00:00:00.000Z')
    expect(transport.calls[0]?.path).toBe('/v1/org/mailbox/delta')
    expect(transport.calls[0]?.body).toEqual({
      slug: 'acme',
      personId: 'p1',
      upserts: '[{"envelopeId":"e1"}]',
      deletes: ['e0'],
      at: '2026-08-04T00:00:00.000Z'
    })
  })

  it('listPersons', async () => {
    const transport = fixedResponseTransport(200, { personIds: ['p1'] })
    const client = new MailboxClient(transport)
    await expect(client.listPersons('acme')).resolves.toEqual(['p1'])
  })

  /** The `slug` a route receives is the caller's, verbatim: it is already the
   * company key (`sha256(dir)[..12]`, served on the beacond row and in the
   * daemon rendezvous). The root-keyed rewrite this replaces turned a display
   * slug into `acme@074619b89d1b`, and is deleted with the composite. */
  it('sends the company key it was given, untranslated', async () => {
    const transport = fixedResponseTransport(200, { found: false, seq: 0 })
    const client = new MailboxClient(transport)
    await client.read('0123456789ab')
    expect(transport.calls[0]?.body).toEqual({ slug: '0123456789ab' })
  })
})
