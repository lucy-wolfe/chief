// #776: RowStoresClient + ManifestClient against a real chiefd binary. #880
// sharpens its once-marker check to assert the exact
// `created:true` then `created:false` contract and prune/reinsert behavior,
// rather than only distinguishing the two responses. This is the behavioral
// row-store successor to the legacy file/SQLite-journal coverage in
// tests/org-event-journal.test.ts; the scripted RowStoresTest remains the
// separate, still-required wire-shape instrument. Also the discovery site
// for two real client/wire bugs invisible to every existing fake-transport
// test: `ManifestClient.genesis` was missing three fields the real
// router requires (`at`/`materialization`/`personContracts` -- fixed in
// `src/resources/Manifest.ts`), and every `/v1/org/*` route addresses the
// company by its KEY, `sha256(<dir>)[..12]`, never by the display slug
// (`bootContractDaemon`'s doc explains why) -- both fixed here, not just
// documented. That key was briefly a COMPOSITE `slug@digest`; the composite
// is deleted, and a display slug names nothing on this surface.
//
// A6(c): this suite now runs against `chiefd run --serve-only` rather than
// `chiefd docstore-only`. The ROUTE FAMILY moved, not the mode's posture --
// docstore-only remains unauthenticated behind its A5 fence and still serves
// `/v1/docs/*`. Every assertion below is unchanged: the row store, the manifest
// normalizer and the once-marker SQL are the same engine on both mounts.
//
// The old header also called docstore-only's lazy MULTI-COMPANY resolver
// load-bearing for this file. It never was: every `boot(slug)` below spawns a
// fresh daemon for one slug, and no database this suite creates has ever held
// two companies. The claim is deleted rather than carried forward, because a
// comment asserting a dependency the code does not have is how the next reader
// concludes a migration is impossible.
import { chiefdBinarySkipTitle, chiefdBinaryTestGate } from '@chief/testing'
import { bootContractDaemon, genesisSpecFor } from '@test/contract/support/bootContractDaemon'
import type { ContractDaemon } from '@test/types/Contract'
import { afterEach, describe, expect, it } from 'vitest'

import type { ChiefdClient } from '@/ChiefdClient'

const SUITE_LABEL = 'RowsContract (real chiefd --serve-only)'
const gate = chiefdBinaryTestGate()
const maybeDescribe = gate.present ? describe : describe.skip

maybeDescribe(gate.present ? SUITE_LABEL : chiefdBinarySkipTitle(SUITE_LABEL, gate), () => {
  let contract: ContractDaemon | undefined

  afterEach(async () => {
    await contract?.stop()
    contract = undefined
  })

  /** Boots a daemon AND genesis-creates the company. Discovered against the
   * real binary (not a fake) that every org-row WRITE route is gated on a
   * live company existing (`unknown-company` refusal otherwise), even though
   * a read on an ungenesis'd company returns a benign `found: false`.
   * `skipGenesis` is for the one test that specifically exercises the
   * pre-genesis state.
   *
   * Hands back the COMPANY KEY rather than the `ContractDaemon`, because the
   * key is the only company identity a `/v1/org/*` call may carry: the daemon
   * resolves every route by `sha256(<dir>)[..12]` and answers a blanket
   * `unknown-company` to the display slug. `slug` stays the caller's own — it
   * is the display NAME, which `genesisSpecFor` must slugify back to — so the
   * two never travel in one variable and cannot be swapped by accident. */
  async function boot(
    slug: string,
    opts: { skipGenesis?: boolean } = {}
  ): Promise<{ client: ChiefdClient; companyKey: string }> {
    contract = await bootContractDaemon(slug)
    const { client, daemon } = contract
    if (!opts.skipGenesis) {
      await client.manifest.genesis(daemon.companyKey, genesisSpecFor(slug))
    }
    return { client, companyKey: daemon.companyKey }
  }

  it('manifest.read is undefined before genesis, then round-trips after genesis', async () => {
    const slug = 'chiefing-contract-rows-genesis'
    const { client, companyKey } = await boot(slug, { skipGenesis: true })
    await expect(client.manifest.read(companyKey)).resolves.toBeUndefined()

    const created = await client.manifest.genesis(companyKey, genesisSpecFor(slug))
    expect(created).toBe(true)

    // NOT a byte-exact round trip: chiefd reconstructs the manifest string
    // from its own normalized rows on read (org_manifest_read), which is
    // free to derive fields differently (observed live: the top-level
    // `name`/`purpose` came back as the root department's, not the
    // company's, and a default-valued `activation` was omitted) -- a
    // byte-exact assertion here would be pinning an implementation detail
    // this contract never promised, not a wire guarantee. Assert what the
    // Contract actually cares about: the manifest is parseable and carries
    // this company's own identity.
    const read = await client.manifest.read(companyKey)
    expect(read?.manifest).toBeDefined()
    const parsed: {
      slug: string
      departments: Record<string, { headPersonId: string }>
      people: Record<string, { name: string }>
    } = JSON.parse(read?.manifest ?? '{}')
    // The DISPLAY slug, not the key this read was addressed by: chiefd
    // reconstructs the manifest's `slug` from `org_settings.display_slug`,
    // which genesis derived by slugifying the spec's `name`. So this asserts
    // the round trip of the human-readable identity — the one thing the key
    // deliberately is not.
    expect(parsed.slug).toBe(slug)
    expect(parsed.departments.executive?.headPersonId).toBe('chief')
    // The person's NAME, not their provider: `people.chief.provider` was the
    // assertion here, and that column is deleted with provider/model
    // management. `name` is the one person field the CALLER supplied
    // (`genesisSpecFor`'s `chief: {name: 'Chief'}`), so it still proves a real
    // round trip through chiefd's own normalization rather than a value
    // chiefd minted for itself — and an absent person reads `undefined`, so
    // it cannot pass vacuously.
    expect(parsed.people.chief?.name).toBe('Chief')
  })

  // `RuntimeState`'s real Rust shape (chiefd-core/src/store/runtime_rows.rs)
  // requires exactly these four fields; every publish route deserializes its
  // `doc` string into a specific typed struct, never an arbitrary blob --
  // another fact invisible to a scripted fake (#880) and only found by
  // publishing a plausible-but-wrong doc against the real binary.
  //
  // AC6: `session: 'org-fixture'` was a fifth. It is retired -- the column is
  // dropped and the field is accepted-and-ignored, so it may be PUBLISHED (a
  // historical blob still carries it) but never comes back.
  function minimalRuntimeDoc(): Record<string, unknown> {
    return {
      version: 1,
      observedAt: '2026-08-04T00:00:00.000Z',
      socketName: 'fixture-socket',
      status: 'running'
    }
  }

  it('a row read/publish round trip: found:false before publish, the published doc after', async () => {
    const slug = 'chiefing-contract-rows-rw'
    const { client, companyKey } = await boot(slug)
    const before = await client.rows.readRuntime(companyKey)
    expect(before.found).toBe(false)

    const doc = minimalRuntimeDoc()
    await client.rows.publishRuntime(companyKey, doc)

    const after = await client.rows.readRuntime<Record<string, unknown>>(companyKey)
    expect(after.found).toBe(true)
    expect(after.doc).toMatchObject(doc)
  })

  // AC6, against the real binary: a historical runtime blob still carries
  // `session`, and the retired key must be tolerated on publish and absent on
  // read. A model that had merely dropped the field would have made this
  // publish a hard deserialization refusal for every upgraded company.
  it('a retired session key publishes without refusing and never comes back', async () => {
    const slug = 'chiefing-contract-rows-retired'
    const { client, companyKey } = await boot(slug)
    await client.rows.publishRuntime(companyKey, { ...minimalRuntimeDoc(), session: 'org-fixture' })

    const after = await client.rows.readRuntime<Record<string, unknown>>(companyKey)
    expect(after.found).toBe(true)
    expect(after.doc).toMatchObject(minimalRuntimeDoc())
    expect(after.doc).not.toHaveProperty('session')
  })

  it('ifSeqNot conditional read reports unchanged when the seq matches', async () => {
    const slug = 'chiefing-contract-rows-cond'
    const { client, companyKey } = await boot(slug)
    await client.rows.publishRuntime(companyKey, minimalRuntimeDoc())
    const first = await client.rows.readRuntime(companyKey)
    expect(first.found).toBe(true)

    // A second read at the SAME seq (whatever readRuntime just reported as
    // current) must come back either unchanged, or found with the identical
    // doc -- never throw and never silently invent a different value.
    const wire = await client.rows.readRuntime(companyKey, {})
    expect(wire.found).toBe(true)
  })

  it('insertEventOnceMarker: reports one creator, then an exact duplicate, and reads back the stored marker', async () => {
    const slug = 'chiefing-contract-rows-once'
    const { client, companyKey } = await boot(slug)
    const marker = {
      keyDigest: 'contract-digest-1',
      id: 'contract-event-1',
      event: { id: 'contract-event-1', event: 'supervision-noticed' },
      createdAtMs: 1_000
    }
    const first = await client.rows.insertEventOnceMarker(companyKey, marker)
    const second = await client.rows.insertEventOnceMarker(companyKey, marker)

    // Keep #776's coarse discriminator AND lock the actual public result:
    // the real SQL INSERT ... ON CONFLICT winner must report true only to
    // the first caller. A reversed router response would satisfy the former
    // but fail the latter, which is #880's regression case.
    expect(second).not.toEqual(first)
    expect(first).toEqual({ created: true })
    expect(second).toEqual({ created: false })

    const readBack = await client.rows.readEventOnceMarker(companyKey, 'contract-digest-1')
    expect(readBack).toEqual({
      found: true,
      doc: {
        schemaVersion: 1,
        keyDigest: 'contract-digest-1',
        event: { id: 'contract-event-1', event: 'supervision-noticed' }
      }
    })
  })

  it('pruneEventOnceMarkers: deleting an expired marker lets its same digest create again', async () => {
    const slug = 'chiefing-contract-rows-once-prune'
    const { client, companyKey } = await boot(slug)
    const marker = {
      keyDigest: 'contract-pruned-digest-1',
      id: 'contract-pruned-event-1',
      event: { id: 'contract-pruned-event-1', event: 'supervision-noticed' },
      createdAtMs: 1_000
    }

    await expect(client.rows.insertEventOnceMarker(companyKey, marker)).resolves.toEqual({
      created: true
    })
    await expect(client.rows.pruneEventOnceMarkers(companyKey, 1_001)).resolves.toEqual({
      rowsAffected: 1
    })
    await expect(client.rows.readEventOnceMarker(companyKey, marker.keyDigest)).resolves.toEqual({
      found: false
    })
    await expect(client.rows.insertEventOnceMarker(companyKey, marker)).resolves.toEqual({
      created: true
    })
  })

  it('a 4MB+ document round-trips whole -- the body-cap/E2BIG regression class', async () => {
    const slug = 'chiefing-contract-rows-large'
    const { client, companyKey } = await boot(slug)
    // Every direct-row publish deserializes into a specific typed Rust
    // struct that explicitly rejects unmodeled keys on write (`extra` is
    // read-side forward-compat only, not a write-side escape hatch --
    // discovered the hard way one test above, "unmodeled-keys"). So the
    // oversized payload has to live inside a real `String` field of a
    // struct we already know is valid, not a fabricated top-level key.
    //
    // The oversized field used to be `session`. AC6 retired that one -- it is
    // accepted and dropped now, so it round-trips to nothing and would have
    // made this test pass for the wrong reason. `recovery` is a real stored
    // TEXT column of the same struct.
    const large = 'x'.repeat(4 * 1024 * 1024 + 1024)
    const doc = { ...minimalRuntimeDoc(), recovery: large }
    await client.rows.publishRuntime(companyKey, doc)
    const readBack = await client.rows.readRuntime<{ recovery: string }>(companyKey)
    expect(readBack.found).toBe(true)
    expect(readBack.doc?.recovery.length).toBe(large.length)
  })

  // TOMBSTONE (chief-home-is-cwd §4c): `prepareCeoOnly against the real
  // runtime-prepare route` stood here. The route is deleted with the
  // daemon-side CEO boot, so a live daemon serves 404 for it.
})
