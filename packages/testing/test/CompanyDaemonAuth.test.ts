/**
 * A7 — the real-daemon harness is a caller class that presents a bearer, and
 * this is the test the design record's acceptance criterion 5 asks for:
 * "every caller in §5.1 reaches chiefd with a bearer, proved by a test per
 * caller class rather than by inspection".
 *
 * Every case here runs against a daemon that ENFORCES, and A6 is why that now
 * needs no arranging. A7 wrote this file with `env: { CHIEFD_AUTH_ENABLED:
 * 'enforce' }` on every boot, because the gate was unset in every deployment
 * the tree could produce and a suite that did not set it would have passed
 * identically whether or not the harness authenticated at all. A6 deleted the
 * variable, so enforcement is the only posture there is: **the `env` line is
 * gone and every assertion below stands unchanged**, which is exactly what A7
 * predicted and the cheapest possible proof that the deletion did what it said.
 *
 * Before A7 this file could not exist: `chiefd run --serve-only` REFUSED to
 * start whenever that variable was set. Both the refusal and the variable are
 * now deleted.
 */
import { existsSync, statSync } from 'node:fs'
import { join } from 'node:path'

import { afterEach, describe, expect, it } from 'vitest'

import { chiefdBinarySkipTitle, chiefdBinaryTestGate } from '@/ChiefdBinary'
import { seedCompany, startCompanyDaemon } from '@/CompanyDaemon'
import { operatorKeyPath } from '@/OperatorBearer'
import type { CompanyDaemon } from '@/types/CompanyDaemon'

const SUITE_LABEL = 'startCompanyDaemon under an enforced auth gate'
const gate = chiefdBinaryTestGate()
const maybeDescribe = gate.present ? describe : describe.skip

/** Its own deadline, per `lucy/no-unbounded-spawn-in-test`. A company daemon
 * opens a company database, mints two keypairs and mounts the whole route
 * surface, so it is slower than the docstore-only harness. Not a performance
 * budget: do not tighten this because a number "looks big." */
const SPAWN_DEADLOCK_TIMEOUT_MS = 40_000

/**
 * The refusal arms below deliberately use an `/v1/org/*` route rather than a
 * `/v1/docs/*` one. The org routes are the ones behind the verify layer, and
 * the last test in this file says what happened to the docs route that is not.
 */
const GENESIS_PATH = '/v1/org/manifest/genesis'

maybeDescribe(gate.present ? SUITE_LABEL : chiefdBinarySkipTitle(SUITE_LABEL, gate), () => {
  const booted: CompanyDaemon[] = []

  afterEach(async () => {
    await Promise.all(booted.splice(0).map((daemon) => daemon.stop()))
  })

  async function boot(slug: string): Promise<CompanyDaemon> {
    // No `env`. There is no gate to switch on any more — A6 deleted the
    // variable A7 set here, and enforcement is the daemon's only posture.
    const daemon = await startCompanyDaemon({ slug })
    booted.push(daemon)
    return daemon
  }

  it(
    'boots at all — the refusal that keyed on the gate is gone',
    async () => {
      const daemon = await boot('a7-boots')
      // Reaching this line IS the assertion the deleted refusal used to make
      // impossible; `startCompanyDaemon` throws with the log tail otherwise.
      expect(daemon.bearer.length).toBeGreaterThan(0)
    },
    SPAWN_DEADLOCK_TIMEOUT_MS
  )

  it(
    'mints the operator key 0600 inside its own company directory, never a shared one',
    async () => {
      const daemon = await boot('a7-key')
      const keyPath = operatorKeyPath(join(daemon.dir, '.chief'))

      expect(existsSync(keyPath)).toBe(true)
      // The low three octal digits, without a bitwise mask: a private key is
      // owner-only from the first byte, on both the daemon's writer and this
      // reader's rule.
      expect(statSync(keyPath).mode.toString(8).slice(-3)).toBe('600')
      // INSIDE the company's own `.chief` folder. The predecessor put the keys
      // one level below a temp data root, and when that was got wrong the key
      // landed in the OS temp directory itself: one `operator.key` shared by
      // every suite on the box, outliving all of them. A company directory has
      // no such level to get wrong.
      expect(keyPath.startsWith(join(daemon.dir, '.chief'))).toBe(true)
    },
    SPAWN_DEADLOCK_TIMEOUT_MS
  )

  it(
    'seeds a company over its bearer, and refuses the same route anonymously and forged',
    async () => {
      const daemon = await boot('a7-seed')
      // `seedCompany` throws with the daemon's log tail on any non-2xx, so a
      // `401` on genesis fails here loudly. This is the end-to-end caller-class
      // proof: a real `/v1/org/*` mutation, over a bearer, under the gate.
      await seedCompany(daemon)

      // The SAME route with no credential. A positive result alone would pass
      // on a surface that ignored the header entirely, which is exactly the
      // mistake `/v1/docs/runtime` turned out to be (see below).
      const anonymous = await fetch(`${daemon.url}${GENESIS_PATH}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: '{}'
      })
      expect(anonymous.status).toBe(401)

      // And with a bearer that is present but not this daemon's. Proves the
      // accepted token is VERIFIED rather than merely counted.
      const forged = await fetch(`${daemon.url}${GENESIS_PATH}`, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          authorization: `Bearer ${daemon.bearer}tampered`
        },
        body: '{}'
      })
      expect(forged.status).toBe(401)
    },
    SPAWN_DEADLOCK_TIMEOUT_MS
  )

  /**
   * FOUND BY A7, CLOSED BY #1115, AND THE ASSERTION IS RETARGETED RATHER THAN
   * DELETED — because the two halves it now pins are different facts.
   *
   * A7 found that `/v1/docs/runtime`, `/v1/admin/shutdown` and
   * `/v1/docs/queue` all answered an anonymous caller under an ENFORCED gate,
   * and correctly diagnosed it as PLACEMENT rather than policy: `serve_bound`
   * added them AFTER the verify layer, and `Router::layer` wraps only what
   * precedes it. It pinned the wrong behaviour on purpose so that closing the
   * gap would break this test rather than pass silently. That worked, and this
   * is the retarget it asked for.
   *
   * `/v1/docs/runtime` still answers `200`, and now for a REASON rather than by
   * accident: it is the fourth `EXEMPT_PATHS` entry, added deliberately as the
   * other half of the pre-auth liveness probe (`chief-cli`'s `probe_health`
   * asks health and then this route, and calls a listener whose mode it cannot
   * read unhealthy — so gating it would make a first boot race the operator key
   * that same boot mints). Acceptance criterion 2 was amended from three exempt
   * paths to four in the same change, with the argument recorded in
   * `DECISIONS.md`.
   *
   * The other two are now INSIDE the gate, which is the half that was a hole:
   * `/v1/admin/shutdown` drains and exits a company daemon.
   */
  it(
    'exempts /v1/docs/runtime by decision, and gates the other two that were outside the layer',
    async () => {
      const daemon = await boot('a7-unlayered')

      // Exempt, deliberately: the second half of the liveness probe.
      expect((await fetch(`${daemon.url}/v1/docs/runtime`)).status).toBe(200)

      // Gated. Both were outside the verify layer until #1115 moved route
      // registration ahead of it. A 401 here is the middleware refusing before
      // any handler runs, which is why it holds whether or not this particular
      // mount happens to register the route at all.
      expect((await fetch(`${daemon.url}/v1/docs/queue`)).status).toBe(401)
      expect(
        (await fetch(`${daemon.url}/v1/admin/shutdown`, { method: 'POST' })).status,
        'an anonymous caller must never be able to shut a company daemon down'
      ).toBe(401)
    },
    SPAWN_DEADLOCK_TIMEOUT_MS
  )
})
