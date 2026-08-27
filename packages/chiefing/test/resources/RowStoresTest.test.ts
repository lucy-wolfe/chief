// Table-driven test over the RowStoresClient methods (+ rowMutate, tested
// separately in RowMutateTest.test.ts). This is the in-package half of the
// path-drift guard the epic's Contract calls out: every route string below is
// frozen here, byte-exact.
import { fixedResponseTransport, RecordingTransport } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { isNullish } from '@/Nullish'
import { RowStoresClient } from '@/resources/RowStores'
import type { OrgRowReadResult, ReadOpts } from '@/types/OrgDocs'

type ReadCall = (
  client: RowStoresClient,
  slug: string,
  opts?: ReadOpts
) => Promise<OrgRowReadResult<unknown>>
type PublishCall = (client: RowStoresClient, slug: string, doc: unknown) => Promise<void>

/** Narrow an unknown recorded body to check for a string `at` field, without
 * a type assertion (banned repo-wide). */
function hasStringAt(value: unknown): value is { at: string } {
  return (
    typeof value === 'object' && !isNullish(value) && 'at' in value && typeof value.at === 'string'
  )
}

/** The store families sharing the plain `readDoc` mechanics (a bare `slug`,
 * `opts?: ReadOpts`). Each family's `read` is a thin named wrapper (not a
 * dynamic `client[methodName]` lookup) so every call stays fully typed — no
 * type assertions anywhere in this table.
 *
 * These used to be read/publish PAIRS. The publisher-route sweep deleted the
 * publish half of every one of them except `runtime`: no caller of any kind
 * posted those routes, and the row is written in-process through `CompanyDb`
 * inside the daemon's own transactions. `runtime` keeps its publish, and the
 * `publishDoc` mechanics are pinned on it below. */
const READ_FAMILIES: Array<{ store: string; read: ReadCall }> = [
  { store: 'session-epoch', read: (c, slug, opts) => c.readSessionEpoch(slug, opts) },
  {
    store: 'operator-escalation-push',
    read: (c, slug, opts) => c.readOperatorEscalationPush(slug, opts)
  },
  { store: 'runtime-owner', read: (c, slug, opts) => c.readRuntimeOwner(slug, opts) },
  { store: 'launch-intent', read: (c, slug, opts) => c.readLaunchIntent(slug, opts) },
  { store: 'mutation-journal', read: (c, slug, opts) => c.readMutationJournal(slug, opts) },
  { store: 'health-monitor', read: (c, slug, opts) => c.readHealthMonitor(slug, opts) },
  { store: 'runtime', read: (c, slug, opts) => c.readRuntime(slug, opts) },
  {
    store: 'operator-escalation-intents',
    read: (c, slug, opts) => c.readOperatorEscalationIntents(slug, opts)
  },
  { store: 'converge-safety', read: (c, slug, opts) => c.readConvergeSafety(slug, opts) }
]

/** The one surviving publish half. */
const PUBLISH_FAMILIES: Array<{ store: string; publish: PublishCall }> = [
  { store: 'runtime', publish: (c, slug, doc) => c.publishRuntime(slug, doc) }
]

describe('RowStoresClient — read families share readDoc mechanics', () => {
  for (const family of READ_FAMILIES) {
    it(`${family.store}: read exact route + found:false decode`, async () => {
      const transport = fixedResponseTransport(200, { found: false, seq: 0 })
      const client = new RowStoresClient(transport)
      await expect(family.read(client, 'acme')).resolves.toEqual({ found: false })
      expect(transport.calls[0]?.path).toBe(`/v1/org/${family.store}/read`)
      expect(transport.calls[0]?.body).toEqual({ slug: 'acme' })
    })

    it(`${family.store}: found doc + ifSeqNot + unchanged short-circuit`, async () => {
      const found = fixedResponseTransport(200, { found: true, doc: '{"n":1}', seq: 3 })
      const foundClient = new RowStoresClient(found)
      await expect(family.read(foundClient, 'acme')).resolves.toEqual({
        found: true,
        doc: { n: 1 }
      })

      const unchanged = fixedResponseTransport(200, { found: true, seq: 3, unchanged: true })
      const unchangedClient = new RowStoresClient(unchanged)
      await expect(family.read(unchangedClient, 'acme', { ifSeqNot: 3 })).resolves.toEqual({
        found: true
      })
      expect(unchanged.calls[0]?.body).toEqual({ slug: 'acme', ifSeqNot: 3 })
    })
  }
})

describe('RowStoresClient — the surviving publish shares publishDoc mechanics', () => {
  for (const family of PUBLISH_FAMILIES) {
    it(`${family.store}: publish exact route + {slug, doc: JSON.stringify(doc)}`, async () => {
      const transport = fixedResponseTransport(200, { applied: true, seq: 1 })
      const client = new RowStoresClient(transport)
      await family.publish(client, 'acme', { n: 1 })
      expect(transport.calls[0]?.path).toBe(`/v1/org/${family.store}/publish`)
      expect(transport.calls[0]?.body).toEqual({ slug: 'acme', doc: '{"n":1}' })
    })
  }
})

describe('RowStoresClient — clear verbs (fence-free, synthesize `at`)', () => {
  const clears: Array<{
    name: string
    route: string
    call: (client: RowStoresClient) => Promise<{ cleared: boolean }>
  }> = [
    {
      name: 'clearLaunchIntent',
      route: '/v1/org/launch-intent/clear',
      call: (c) => c.clearLaunchIntent('acme')
    },
    { name: 'clearRuntime', route: '/v1/org/runtime/clear', call: (c) => c.clearRuntime('acme') }
  ]

  for (const entry of clears) {
    it(`${entry.name}: exact route, {cleared} result, synthesized 'at'`, async () => {
      const transport = fixedResponseTransport(200, { cleared: true })
      const client = new RowStoresClient(transport)
      await expect(entry.call(client)).resolves.toEqual({ cleared: true })
      expect(transport.calls[0]?.path).toBe(entry.route)
      const body = transport.calls[0]?.body
      expect(body).toMatchObject({ slug: 'acme' })
      expect(hasStringAt(body)).toBe(true)
    })
  }
})

describe('RowStoresClient — semantic-queue inserts decode the inserted|duplicate|conflict union', () => {
  it('insertOperatorEscalationIntent: exact route + body', async () => {
    const transport = fixedResponseTransport(200, { status: 'inserted', seq: 1 })
    const client = new RowStoresClient(transport)
    await client.insertOperatorEscalationIntent('acme', { fingerprint: 'f1' })
    expect(transport.calls[0]?.path).toBe('/v1/org/operator-escalation-intents/insert')
    expect(transport.calls[0]?.body).toEqual({ slug: 'acme', intent: { fingerprint: 'f1' } })
  })
})

describe('RowStoresClient — event-journal (DocStore-direct, `marker` wire field)', () => {
  it('readEventOnceMarker: found:false decode', async () => {
    const transport = fixedResponseTransport(200, { found: false })
    const client = new RowStoresClient(transport)
    await expect(client.readEventOnceMarker('acme', 'digest-1')).resolves.toEqual({
      found: false
    })
    expect(transport.calls[0]?.path).toBe('/v1/org/event-journal/read')
    expect(transport.calls[0]?.body).toEqual({ slug: 'acme', keyDigest: 'digest-1' })
  })

  it('readEventOnceMarker: found -> parses the marker field (not doc/document)', async () => {
    const transport = fixedResponseTransport(200, {
      found: true,
      marker: '{"schemaVersion":1,"keyDigest":"digest-1","event":{}}'
    })
    const client = new RowStoresClient(transport)
    await expect(client.readEventOnceMarker('acme', 'digest-1')).resolves.toEqual({
      found: true,
      doc: { schemaVersion: 1, keyDigest: 'digest-1', event: {} }
    })
  })

  it('insertEventOnceMarker: exact route + body, decodes {created}', async () => {
    const transport = fixedResponseTransport(200, { created: true })
    const client = new RowStoresClient(transport)
    await expect(
      client.insertEventOnceMarker('acme', {
        keyDigest: 'digest-1',
        id: 'evt-1',
        event: { kind: 'x' },
        createdAtMs: 1000
      })
    ).resolves.toEqual({ created: true })
    expect(transport.calls[0]?.path).toBe('/v1/org/event-journal/insert-if-absent')
    expect(transport.calls[0]?.body).toEqual({
      slug: 'acme',
      keyDigest: 'digest-1',
      id: 'evt-1',
      event: { kind: 'x' },
      createdAtMs: 1000
    })
  })

  it('pruneEventOnceMarkers: exact route + body, decodes {rowsAffected}', async () => {
    const transport = fixedResponseTransport(200, { rowsAffected: 3 })
    const client = new RowStoresClient(transport)
    await expect(client.pruneEventOnceMarkers('acme', 1000)).resolves.toEqual({ rowsAffected: 3 })
    expect(transport.calls[0]?.path).toBe('/v1/org/event-journal/prune')
    expect(transport.calls[0]?.body).toEqual({ slug: 'acme', olderThanMs: 1000 })
  })
})

// TOMBSTONE (chief-home-is-cwd §4c): `describe('RowStoresClient — runtime-only
// verbs')` stood here with two `prepareCeoOnly` tests — the exact route + `at`
// param, and the pin that chiefd no longer claims who is actuating. The method
// and its route are deleted with the daemon-side CEO boot; the store operation
// they reached survives with genesis as its only caller, and no client speaks
// it.

describe('RowStoresClient — the company key on the wire', () => {
  /** The `slug` a route receives is the caller's, verbatim: it is already the
   * company key (`sha256(dir)[..12]`, served on the beacond row and in the
   * daemon rendezvous). The root-keyed rewrite this replaces turned a display
   * slug into `acme@074619b89d1b`, and is deleted with the composite. */
  it('sends the company key it was given, untranslated', async () => {
    const transport = fixedResponseTransport(200, { found: false, seq: 0 })
    const client = new RowStoresClient(transport)
    await client.readSessionEpoch('0123456789ab')
    expect(transport.calls[0]?.body).toEqual({ slug: '0123456789ab' })
  })
})

describe('RowStoresClient — failure classification', () => {
  it('422 {code, detail} -> OrgRowRefusalError with fields', async () => {
    const transport = fixedResponseTransport(422, { code: 'unmodeled-keys', detail: 'nope' })
    const client = new RowStoresClient(transport)
    await expect(client.publishRuntime('acme', { version: 1 })).rejects.toMatchObject({
      name: 'OrgRowRefusalError',
      status: 422,
      code: 'unmodeled-keys',
      detail: 'nope'
    })
  })

  it('500 -> ChiefdUnavailableError', async () => {
    const transport = new RecordingTransport(() => ({ status: 500, body: 'boom' }))
    const client = new RowStoresClient(transport)
    await expect(client.readSessionEpoch('acme')).rejects.toMatchObject({
      name: 'ChiefdUnavailableError',
      kind: 'http-error',
      status: 500
    })
  })

  it('malformed 2xx body -> ChiefdUnavailableError kind malformed-body', async () => {
    const transport = new RecordingTransport(() => ({ status: 200, body: 'not json' }))
    const client = new RowStoresClient(transport)
    await expect(client.readSessionEpoch('acme')).rejects.toMatchObject({
      name: 'ChiefdUnavailableError',
      kind: 'malformed-body'
    })
  })
})

describe('D0/D24/F25 — retired methods stay retired', () => {
  it('no startPerson or companyRemove on RowStoresClient', () => {
    const client = new RowStoresClient(fixedResponseTransport(200, {}))
    expect('startPerson' in client).toBe(false)
    expect('companyRemove' in client).toBe(false)
  })

  // The publisher-route sweep. Each of these dialled a route no caller of any
  // kind posted, and both sides are deleted; naming them here means a revert
  // reads as "the routes the sweep deleted", not as an anonymous diff.
  it('the caller-less publish and clear methods stay deleted', () => {
    const client = new RowStoresClient(fixedResponseTransport(200, {}))
    for (const method of [
      'publishSessionEpoch',
      'publishGoalDeliveryQuiesce',
      'clearGoalDeliveryQuiesce',
      'publishOperatorEscalationPush',
      'publishRuntimeOwner',
      'publishLaunchIntent',
      'publishMutationJournal',
      'publishHealthMonitor',
      // `publishCeoBootLease`/`clearCeoBootLease` were listed here beside
      // them. They are still absent, and now so is `readCeoBootLease` and the
      // whole store — see the two entries below.
      'readCeoBootLease',
      'prepareCeoOnly',
      'publishConvergeSafety',
      'publishOperatorEscalationIntents',
      'publishOperatorEscalationIntentsCas',
      'readMaterialization'
    ]) {
      expect(method in client, `${method} came back`).toBe(false)
    }
  })
})
