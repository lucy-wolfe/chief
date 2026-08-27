import { createFakeOpener, flushMicrotasks } from '@test/sse/FakeSseStream'
import { describe, expect, it } from 'vitest'

import { ChiefdClient } from '@/ChiefdClient'
import { AuthClient } from '@/resources/Auth'
import { DocsClient } from '@/resources/Docs'

describe('ChiefdClient facade', () => {
  // #929/#936: the row-store factory this client's `rows` surface replaced
  // (org-row-stores.ts's `orgRowStoreForTribe`/`orgRowStoreAmbient`) refused
  // synchronously on an empty URL; #929 moved three consumers off it onto
  // this client with no equivalent, disclosed rather than silently dropped.
  // Demonstrates the refusal actually fires, not merely that the check
  // exists in source.
  it('#936: refuses an empty URL synchronously, at construction, not per-request', () => {
    expect(() => new ChiefdClient({ url: '' })).toThrow(/non-empty chiefd URL/)
  })

  it('#936: refuses a whitespace-only URL the same way', () => {
    expect(() => new ChiefdClient({ url: '   ' })).toThrow(/non-empty chiefd URL/)
  })

  it('wires docs/auth against one shared transport', async () => {
    const client = new ChiefdClient({ url: 'http://127.0.0.1:1' })
    expect(client.docs).toBeInstanceOf(DocsClient)
    expect(client.auth).toBeInstanceOf(AuthClient)
  })

  it('docs/auth are real — no longer throw the E2-S3 not-implemented stub', async () => {
    const client = new ChiefdClient({ url: 'http://127.0.0.1:1' })
    // health() catches its own transport failure and resolves false, rather
    // than throwing NOT_IMPLEMENTED — proving the body is filled in, not
    // merely present.
    await expect(client.docs.health()).resolves.toBe(false)
  })

  it('manifest/aggregates/mailbox/personContracts/rows are implemented (E2-S4) and no longer throw the stub contract', async () => {
    const client = new ChiefdClient({ url: 'http://127.0.0.1:1' })
    const NOT_IMPLEMENTED = /not implemented: @chief\/chiefing stub — implemented by E2-S\d/
    // Each of these attempts a real request against an unreachable port, so
    // it rejects with a transport failure (ChiefdUnavailableError) — never
    // the NOT_IMPLEMENTED stub message the E0-S4 scaffold shipped. Proves
    // the body is filled in, not merely present.
    await expect(client.manifest.read('acme')).rejects.not.toThrow(NOT_IMPLEMENTED)
    await expect(client.aggregates.activityRead('acme')).rejects.not.toThrow(NOT_IMPLEMENTED)
    await expect(client.mailbox.read('acme')).rejects.not.toThrow(NOT_IMPLEMENTED)
    await expect(client.personContracts.read('acme')).rejects.not.toThrow(NOT_IMPLEMENTED)
    await expect(client.rows.readSessionEpoch('acme')).rejects.not.toThrow(NOT_IMPLEMENTED)
  })

  it('staffing/reminders are implemented (E2-S5) and no longer throw the stub contract', async () => {
    const client = new ChiefdClient({ url: 'http://127.0.0.1:1' })
    const NOT_IMPLEMENTED = /not implemented: @chief\/chiefing stub — implemented by E2-S\d/
    // Each of these attempts a real request against an unreachable port, so
    // it rejects with a transport failure — never the NOT_IMPLEMENTED stub
    // message the E0-S4 scaffold shipped. Proves the body is filled in, not
    // merely present (mirrors the E2-S4 assertion above).
    await expect(client.staffing.startPerson('acme', 'p1')).rejects.not.toThrow(NOT_IMPLEMENTED)
    await expect(
      client.reminders.armReminder({
        slug: 'acme',
        personId: 'p1',
        prompt: 'check in',
        intervalMs: 60_000
      })
    ).rejects.not.toThrow(NOT_IMPLEMENTED)
  })

  it('a caller-supplied url is used as-is (no default, ruling D1)', () => {
    const client = new ChiefdClient({ url: 'http://127.0.0.1:8792' })
    expect(client.url).toBe('http://127.0.0.1:8792')
  })

  /** THE SLUG A WATCH SENDS IS THE COMPANY KEY, VERBATIM.
   *
   * This used to be a translation: a client built with a `root` turned a
   * display slug into the composite `documentKey(slug, root)` here, and a
   * rootless one passed it through. There is one behaviour now, because there
   * is one identity — `sha256(dir)[..12]`, served on the beacond row and in
   * the daemon rendezvous — and this client never derives it. */
  it('watch sends the company key it was given, untranslated', async () => {
    const key = '0123456789ab'
    const opener = createFakeOpener()
    const client = new ChiefdClient({ url: 'http://chiefd.test' })
    const subscription = client.watch({
      slug: key,
      stores: ['activity'],
      onEvent: () => {},
      openStream: opener.open
    })

    try {
      await flushMicrotasks()
      const opened = opener.calls[0]
      if (typeof opened !== 'string') throw new Error('expected a watch opener call')
      expect(new URL(opened).searchParams.get('slug')).toBe(key)
    } finally {
      subscription.close()
      await flushMicrotasks()
    }
  })
})
