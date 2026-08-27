// #776: the /v1/docs/watch SSE channel against a real chiefd binary.
//
// A6(c): that binary is now `chiefd run --serve-only`, not `chiefd
// docstore-only` — the `/v1/org/*` ROUTE FAMILY left the unauthenticated mount,
// while docstore-only's own posture, its A5 fence and its `/v1/docs/*` surface
// are unchanged. Both facts this file proves survive the move intact, and the
// second one is now proved on a stronger surface. `serve_only_snapshot` wires
// the change feed (`wire_change_feed`) and serves the watch stream exactly as
// the docstore-only mount did, and it STILL runs no duty scheduler, so the
// delivery below is still the normalized write path reaching the feed on its
// own rather than a reconcile loop carrying it.
//
// The reader presents a bearer (A4's `SseBearerProvider` seam). A long-lived
// stream is a caller like any other, and an anonymous subscriber on an
// authenticated daemon would be a hole in exactly the surface this workstream
// is closing.
//
// PROMOTED TO A POSITIVE TEST (#711/#1002). This suite used to assert the
// OPPOSITE of the case below, and said so: a real `/v1/org/runtime/publish`
// produced no event across a 4-second window while the heartbeat arrived on
// schedule, because `direct_org_row_route_pair!`'s publish path only called
// `wake_reconcile(&source)` -- a `Notify` for the DUTY SCHEDULER's reconcile
// loop -- and never the `ChangeFeed`/SSE machinery. `chiefd docstore-only`
// has NO duty scheduler (docstore_only.rs's own module doc), so the only
// kind of write every chiefing resource client performs could never reach
// `/v1/docs/watch` on this daemon mode.
//
// The old header named the trigger for this edit exactly: "a future change
// that wires the normalized write path into the feed should make the
// negative assertion below fail loudly, which is the intended trigger to
// promote it to a positive test." #711 is that change -- `runtime_publish`
// now emits `publish_row_feed_hint` itself, because `runtime` bypasses
// `Ledgers` and an unhinted publish "left every fresh org's first runtime
// doc unable to ever wake a /v1/docs/watch subscriber". The negative
// assertion did fail loudly on the first run of this suite. Promoted.
import { chiefdBinarySkipTitle, chiefdBinaryTestGate } from '@chief/testing'
import {
  bootContractDaemon,
  contractBearer,
  genesisSpecFor
} from '@test/contract/support/bootContractDaemon'
import type { ContractDaemon } from '@test/types/Contract'
import { afterEach, describe, expect, it } from 'vitest'

import type { SseDocChangeEvent } from '@/types/Watch'

const SUITE_LABEL = 'SseContract (real chiefd --serve-only)'
const gate = chiefdBinaryTestGate()
const maybeDescribe = gate.present ? describe : describe.skip

function minimalRuntimeDoc(session: string): Record<string, unknown> {
  return {
    version: 1,
    observedAt: '2026-08-04T00:00:00.000Z',
    session,
    socketName: 'fixture-socket',
    status: 'running'
  }
}

maybeDescribe(gate.present ? SUITE_LABEL : chiefdBinarySkipTitle(SUITE_LABEL, gate), () => {
  let contract: ContractDaemon | undefined

  afterEach(async () => {
    await contract?.stop()
    contract = undefined
  })

  it('the channel stays alive past the real heartbeat cadence (never reports dead)', async () => {
    const slug = 'chiefing-contract-sse-heartbeat'
    const booted = await bootContractDaemon(slug)
    contract = booted
    const { client } = booted
    // `/v1/docs/watch` takes the COMPANY KEY, not the display slug: the change
    // feed publishes every event under the key `wire_change_feed` was handed,
    // and the stream filters on an exact match. A display slug here subscribes
    // to a company that does not exist and receives nothing forever.
    const companyKey = booted.daemon.companyKey

    // The public SseWatcher surface has no caller-visible heartbeat
    // callback -- heartbeats are internal `: hb` keep-alive comment lines
    // (confirmed directly against this same binary: they arrive on a ~15s
    // cadence). The observable proxy for "a heartbeat kept the connection
    // alive" is `onChannelStateChange` never reporting 'dead' across a
    // window comfortably past that cadence -- SseWatcher's own
    // `heartbeatTimeoutMs` default (45s / 3 missed beats) would otherwise
    // have fired well before 20s.
    let sawDead = false
    // AND that it ever went healthy, without which this case passes for a
    // stream that never opened at all. `markDead` only fires from the
    // heartbeat monitor of a CONNECTED channel, so a watcher stuck in
    // connect-fail/backoff -- a 401, a refused port, a wrong route -- reports
    // no state at all and `sawDead === false` is true vacuously. 'healthy' is
    // set by `dispatchEvent` on a received comment frame, so it is the daemon's
    // own heartbeat arriving, which is exactly what this case claims to prove.
    let sawHealthy = false
    const subscription = client.watch({
      slug: companyKey,
      stores: ['runtime'],
      bearer: contractBearer(booted.daemon.bearer),
      onEvent: () => {},
      onChannelStateChange: (state) => {
        if (state === 'dead') sawDead = true
        if (state === 'healthy') sawHealthy = true
      }
    })
    try {
      await new Promise((resolve) => setTimeout(resolve, 20_000))
      expect(sawHealthy, 'the stream never opened, so "not dead" proves nothing').toBe(true)
      expect(sawDead).toBe(false)
    } finally {
      subscription.close()
    }
  }, 45_000)

  it('a real /v1/org/runtime/publish DOES reach /v1/docs/watch (#711)', async () => {
    const slug = 'chiefing-contract-sse-normalized-delivery'
    const booted = await bootContractDaemon(slug)
    contract = booted
    const { client } = booted
    // The publish route, the subscription and the feed all name the company by
    // its KEY. They must agree exactly or this case cannot fail honestly: a
    // mismatch on either side is silence, which is indistinguishable from the
    // unwired feed this test was promoted from.
    const companyKey = booted.daemon.companyKey
    await client.manifest.genesis(companyKey, genesisSpecFor(slug))

    const events: SseDocChangeEvent[] = []
    const subscription = client.watch({
      slug: companyKey,
      stores: ['runtime'],
      bearer: contractBearer(booted.daemon.bearer),
      onEvent: (event) => {
        events.push(event)
      }
    })
    try {
      await new Promise((resolve) => setTimeout(resolve, 300))
      await client.rows.publishRuntime(companyKey, minimalRuntimeDoc('sse-delivery-check'))
      // 4s is ample: a real delivery arrives in well under a second. The old
      // negative version of this case used the same window to prove nothing
      // arrived, so the two are directly comparable.
      await new Promise((resolve) => setTimeout(resolve, 4_000))
      expect(events.length).toBeGreaterThan(0)
      // The hint must name the store that was written, not merely wake the
      // channel: a fan-out that woke every subscriber regardless of store
      // would satisfy a bare length check while telling a `runtime` watcher
      // nothing about `runtime`.
      expect(events.every((event) => event.store === 'runtime')).toBe(true)
    } finally {
      subscription.close()
    }
  }, 30_000)
})
