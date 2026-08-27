import { fixedResponseTransport } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { OrgRowRefusalError, SeqConflictError } from '@/Errors'
import { AggregatesClient } from '@/resources/Aggregates'
import { SEQ_CONFLICT_CODE } from '@/resources/OrgRoutes'

describe('AggregatesClient', () => {
  it('activityRead: ledger field normalizes to document', async () => {
    const transport = fixedResponseTransport(200, { found: true, ledger: '{"x":1}', seq: 7 })
    const client = new AggregatesClient(transport)
    await expect(client.activityRead('acme')).resolves.toEqual({
      found: true,
      seq: 7,
      unchanged: undefined,
      document: '{"x":1}'
    })
    expect(transport.calls[0]?.path).toBe('/v1/org/activity/read')
  })

  it('activityRead: ifSeqNot passed through; unchanged short-circuits', async () => {
    const transport = fixedResponseTransport(200, { found: true, seq: 7, unchanged: true })
    const client = new AggregatesClient(transport)
    await expect(client.activityRead('acme', { ifSeqNot: 7 })).resolves.toEqual({
      found: true,
      seq: 7,
      unchanged: true,
      document: undefined
    })
    expect(transport.calls[0]?.body).toEqual({ slug: 'acme', ifSeqNot: 7 })
  })

  it('supervisionRead', async () => {
    const transport = fixedResponseTransport(200, { found: true, ledger: '{}', seq: 1 })
    const client = new AggregatesClient(transport)
    await client.supervisionRead('acme')
    expect(transport.calls[0]?.path).toBe('/v1/org/supervision/read')
  })

  it('sessionMaintenanceRead', async () => {
    const transport = fixedResponseTransport(200, { found: false, seq: 0 })
    const client = new AggregatesClient(transport)
    await client.sessionMaintenanceRead('acme')
    expect(transport.calls[0]?.path).toBe('/v1/org/session-maintenance/read')
  })

  // TOMBSTONE: the seven `*PublishCas` / `*Publish` / `*Clear` /
  // `activityReconcileStructural` tests this file used to carry. Their subject
  // routes are deleted — the publisher-route sweep found no caller of any
  // kind — and so are the client methods. What those tests actually PROVED
  // about the transport, and what must survive them, is below: one POST per
  // call, and the 409 discrimination on the body's `code`.

  // #950/#954 regression: a separate CAS poster used to delegate any non-409
  // status to `postOrgRoute`, which issued a SECOND, independent POST to the
  // same path -- a write sent twice, and whatever the caller saw was the
  // second request's outcome, not the first's. `transport.calls` having
  // length 1 is the whole regression; a length of 2 is the bug. There is now
  // exactly ONE poster, which is the structural fix.
  it('a read posts exactly once on a clean 2xx', async () => {
    const transport = fixedResponseTransport(200, { found: true, ledger: '{}', seq: 8 })
    const client = new AggregatesClient(transport)
    await client.supervisionRead('acme')
    expect(transport.calls).toHaveLength(1)
    expect(transport.calls[0]).toEqual({
      method: 'POST',
      path: '/v1/org/supervision/read',
      body: { slug: 'acme' }
    })
  })

  it('a non-409 refusal posts exactly once, never retried in-process', async () => {
    const transport = fixedResponseTransport(404, { code: 'error', detail: '' })
    const client = new AggregatesClient(transport)
    await expect(client.supervisionRead('acme')).rejects.toMatchObject({
      name: 'OrgRowRefusalError',
      status: 404
    })
    expect(transport.calls).toHaveLength(1)
  })

  // 409 discrimination, in the one direction that still has a client. Since
  // the refusal taxonomy landed (`docstore/route_error.rs`), 409 means exactly
  // one thing at the server -- a fence moved, re-read and retry -- but WHICH
  // fence is the whole question, and only the body's `code` answers it. Every
  // 409 that reaches this client is an `OrgRowRefusalError` carrying chiefd's
  // own code, which is what a caller can actually act on.
  //
  // The typed `SeqConflictError` and {@link SEQ_CONFLICT_CODE} are NOT deleted
  // with the CAS poster: the code is a two-sided contract with Rust that
  // `scripts/test/refusal-taxonomy.test.mjs` pins, and every `*_publish_cas`
  // in `chiefd-core/src/actor/writer.rs` still raises it. What went is the
  // client method that dialled a CAS route nobody called.
  for (const code of [SEQ_CONFLICT_CODE, 'fence-mismatch', 'organization-exists'] as const) {
    it(`a 409 '${code}' is a refusal carrying chiefd's own code`, async () => {
      const transport = fixedResponseTransport(409, { code, detail: 'a fence moved' })
      const client = new AggregatesClient(transport)

      const thrown = await client
        .supervisionRead('acme')
        .then(() => undefined)
        .catch((error: unknown) => error)

      expect(thrown).toBeInstanceOf(OrgRowRefusalError)
      expect(thrown).not.toBeInstanceOf(SeqConflictError)
      expect(thrown).toMatchObject({
        name: 'OrgRowRefusalError',
        status: 409,
        code,
        detail: 'a fence moved'
      })
      expect(transport.calls).toHaveLength(1)
    })
  }

  // A 409 whose body chiefd could not have produced (no `code` at all)
  // decodes to `code: 'error'` and must still take the refusal branch.
  it('a 409 with no code is a refusal', async () => {
    const transport = fixedResponseTransport(409, {})
    const client = new AggregatesClient(transport)
    await expect(client.supervisionRead('acme')).rejects.toMatchObject({
      name: 'OrgRowRefusalError',
      status: 409
    })
    expect(transport.calls).toHaveLength(1)
  })

  /** The `slug` a route receives is the caller's, verbatim: it is already the
   * company key (`sha256(dir)[..12]`, served on the beacond row and in the
   * daemon rendezvous). The root-keyed rewrite this replaces turned a display
   * slug into `acme@074619b89d1b`, and is deleted with the composite. */
  it('sends the company key it was given, untranslated', async () => {
    const transport = fixedResponseTransport(200, { found: false, seq: 0 })
    const client = new AggregatesClient(transport)
    await client.activityRead('0123456789ab')
    expect(transport.calls[0]?.body).toEqual({ slug: '0123456789ab' })
  })

  it('422 refusal -> OrgRowRefusalError', async () => {
    const transport = fixedResponseTransport(422, { code: 'unmodeled-keys', detail: 'nope' })
    const client = new AggregatesClient(transport)
    await expect(client.activityRead('acme')).rejects.toMatchObject({
      name: 'OrgRowRefusalError',
      status: 422,
      code: 'unmodeled-keys',
      detail: 'nope'
    })
  })
})
