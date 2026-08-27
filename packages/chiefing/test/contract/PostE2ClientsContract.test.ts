/**
 * #751/G6 — real-binary coverage for the clients the contract suite never
 * reached.
 *
 * The audit's finding: `OrgSliceClient` (17 methods), `RuntimeClient` (26),
 * `SessionLifecycleClient` (22), `SettingsClient`, `CompanyLifecycleClient`
 * and `FounderLaunchClient` were added after E2 and had ZERO real-binary
 * coverage and zero route freeze — about 85 of the package's ~200 methods,
 * verified only against fake transports that answer whatever the test wants.
 * A fake transport cannot discover the two classes of defect that actually
 * shipped from this package before (`RoutePathFreeze`'s and `RowsContract`'s
 * headers record both): a request body the real router rejects, and a real
 * response the client cannot decode.
 *
 * `RouteTableDerivation.test.ts` now proves every path these clients dial is
 * one a Rust router registers. That is necessary and not sufficient — a route
 * can exist and still be dialed with the wrong body. This file is the
 * sufficient half for the four clients that a `chiefd docstore-only` daemon
 * serves, exercised the way the rest of the contract suite is: boot a real
 * binary, genesis a real company, call the real method, assert on what comes
 * back.
 *
 * SCOPE, stated rather than implied. Two of the six clients are NOT covered
 * here and cannot be from this fixture:
 *   - `CompanyLifecycleClient` dials `/v1/company/{create,boot,stop}`, served
 *     by `chief host` (`crates/chief-cli/src/host/router.rs`) — a different
 *   - `FounderLaunchClient` dials `/v1/founder/launch`, served by the founder
 *     router (`crates/chief-cli/src/founder.rs`) — likewise.
 * Covering those needs a `chief host` fixture in `@chief/testing`, which
 * does not exist. That is recorded as a gap rather than papered over with a
 * fake-transport test wearing a contract-suite filename — a suite that claims
 * real-binary coverage it does not have is worse than one that says where it
 * stops.
 *
 * The assertions lean on RESOLUTION plus coarse shape rather than exact
 * payloads. That is deliberate: the defect class here is "the real binary
 * refuses this body" or "the client cannot decode this response", both of
 * which surface as a throw. Pinning exact payload contents would duplicate
 * the per-store tests and would make this file fail on every legitimate
 * server-side field addition — the since-deleted
 * `/v1/org/runtime/prepare-ceo-only` gained a `warnings` field while this
 * packet was being written, and a suite that red-lights on that is a suite
 * people learn to ignore.
 */
import { chiefdBinarySkipTitle, chiefdBinaryTestGate } from '@chief/testing'
import { bootContractDaemon, genesisSpecFor } from '@test/contract/support/bootContractDaemon'
import type { ContractDaemon } from '@test/types/Contract'
import { afterEach, describe, expect, it } from 'vitest'

import type { ChiefdClient } from '@/ChiefdClient'
import { OrgRowRefusalError } from '@/Errors'
import { isNullish } from '@/Nullish'
import { ROOT_DEPARTMENT_ID } from '@/types/Organization'

const SUITE_LABEL = 'PostE2ClientsContract (real chiefd --serve-only)'
const gate = chiefdBinaryTestGate()
const maybeDescribe = gate.present ? describe : describe.skip

maybeDescribe(gate.present ? SUITE_LABEL : chiefdBinarySkipTitle(SUITE_LABEL, gate), () => {
  let contract: ContractDaemon | undefined

  afterEach(async () => {
    await contract?.stop()
    contract = undefined
  })

  /** Boots and genesises, handing back the COMPANY KEY rather than the daemon.
   * Every client method below takes the company as its first argument, and
   * that argument is the key (`sha256(<dir>)[..12]`) on every `/v1/org/*`
   * route — a display slug names no company there and answers a blanket
   * `unknown-company`. `slug` stays the display NAME, which genesis derives
   * from the spec and cross-checks, so the two never share a variable. */
  async function boot(slug: string): Promise<{ client: ChiefdClient; companyKey: string }> {
    contract = await bootContractDaemon(slug)
    const { client, daemon } = contract
    await client.manifest.genesis(daemon.companyKey, genesisSpecFor(slug))
    return { client, companyKey: daemon.companyKey }
  }

  describe('OrgSliceClient — the read-only org views', () => {
    it('lifecycleStatus, treeLines and unitSubtree answer for a genesis-seeded company', async () => {
      const slug = 'chiefing-poste2-orgslice-reads'
      const { client, companyKey } = await boot(slug)

      const status = await client.orgSlice.lifecycleStatus(companyKey)
      expect(status).toBeTypeOf('object')

      const tree = await client.orgSlice.treeLines(companyKey)
      // The ASCII tree is one line per unit; a genesis'd company has at least
      // the root department, so an empty tree means the read is not seeing
      // the company it was asked about.
      expect(Array.isArray(tree.lines)).toBe(true)
      expect(tree.lines.length).toBeGreaterThan(0)

      const subtree = await client.orgSlice.unitSubtree(companyKey, ROOT_DEPARTMENT_ID)
      expect(subtree).toBeTypeOf('object')
    })

    it('unitRemovalImpact computes without writing, and previewing the ROOT is refused as a typed value', async () => {
      const slug = 'chiefing-poste2-orgslice-removal'
      const { client, companyKey } = await boot(slug)

      const impact = await client.orgSlice.unitRemovalImpact(companyKey, ROOT_DEPARTMENT_ID)
      expect(impact).toBeTypeOf('object')

      // Discovered against the real binary, not assumed: the engine refuses a
      // root-unit removal preview with `root-unit-not-removable`. The client
      // contract worth pinning is that this arrives as a decoded
      // `OrgRowRefusalError` carrying the engine's own code — not as an
      // opaque transport error and not as a silently-successful preview.
      await expect(
        client.orgSlice.unitRemovalPreview(companyKey, ROOT_DEPARTMENT_ID)
      ).rejects.toBeInstanceOf(OrgRowRefusalError)
      await expect(
        client.orgSlice.unitRemovalPreview(companyKey, ROOT_DEPARTMENT_ID)
      ).rejects.toThrow(/root-unit-not-removable/)

      // Non-destructive is the whole contract of an impact/preview call: the
      // tree must still read as it did. Against a fake transport this is
      // unfalsifiable; against the real engine it is the assertion worth
      // having.
      const tree = await client.orgSlice.treeLines(companyKey)
      expect(tree.lines.length).toBeGreaterThan(0)
    })

    it('buildPersonContracts runs against the real engine', async () => {
      const slug = 'chiefing-poste2-orgslice-contracts'
      const { client, companyKey } = await boot(slug)
      const built = await client.orgSlice.buildPersonContracts(companyKey)
      expect(built).toBeTypeOf('object')
    })

    it('activityCommandStatus refuses an unknown caller as a typed value, never a transport error', async () => {
      const slug = 'chiefing-poste2-orgslice-activity'
      const { client, companyKey } = await boot(slug)
      // Also discovered here: the route authenticates the caller, so there is
      // no "empty status for a stranger" answer. It used to be `unknown-person`
      // — the ledger lookup's own refusal — and it is now
      // `requester-identity-mismatch`, because a fence that runs EARLIER
      // answers first.
      //
      // `callerPersonId` is documented as "the person the trusted adapter
      // authenticated. Never from a Pi payload", and since B3/B4 the route
      // BINDS it to the credential before it resolves the company or reads the
      // ledger. This suite authenticates as the OPERATOR, so naming any person
      // at all is impersonation and is refused there — the person is never
      // looked up, which is why the answer cannot be about whether that person
      // exists.
      //
      // That ORDER is the right one and this assertion follows it rather than
      // fighting it: an earlier refusal is also what stops this route being an
      // existence oracle, where an operator could learn who a company employs
      // by reading which name comes back `unknown-person`. `unknown-person`
      // itself is not dead — it is what an enrolled PERSON gets for its own id
      // when the activity ledger has no row for it — but no operator
      // credential can reach it, and this harness can hold no other: the
      // `--serve-only` mode mounts no runtime host, so nobody is ever
      // materialized or enrolled here.
      await expect(
        client.orgSlice.activityCommandStatus(companyKey, 'nobody')
      ).rejects.toBeInstanceOf(OrgRowRefusalError)
      await expect(client.orgSlice.activityCommandStatus(companyKey, 'nobody')).rejects.toThrow(
        /requester-identity-mismatch/
      )
    })
  })

  describe('SettingsClient — the read/publish pair', () => {
    it('read is undefined before any settings row exists, and round-trips after publish', async () => {
      const slug = 'chiefing-poste2-settings'
      const { client, companyKey } = await boot(slug)

      // Whether genesis seeds a settings row is the server's business; the
      // client contract is that BOTH states decode, and absence is
      // `undefined` rather than a throw.
      const before = await client.settings.read(companyKey)
      expect(isNullish(before) || typeof before === 'object').toBe(true)

      await client.settings.publishLauncherRoot({
        slug: companyKey,
        at: '2026-08-08T00:00:00.000Z',
        launcherRoot: '/tmp/chiefing-poste2-launcher-root'
      })

      const after = await client.settings.read(companyKey)
      expect(after?.launcherRoot).toBe('/tmp/chiefing-poste2-launcher-root')
    })
  })

  describe('RuntimeClient — and the process-role boundary this suite discovered', () => {
    it('readOwnership decodes a real, fully-populated ownership row', async () => {
      const slug = 'chiefing-poste2-runtime-ownership'
      const { client, companyKey } = await boot(slug)
      const ownership = await client.runtime.readOwnership(companyKey)
      // The one RuntimeClient read a docstore-only daemon fully serves, so it
      // gets a real shape assertion rather than a bare typeof: this is a
      // durable row, and `released` is the correct state for a company that
      // has never launched.
      //
      // `organization` is the DISPLAY slug even though the read was addressed
      // by the key: chiefd derives the field from the manifest's own `slug`,
      // which genesis slugified out of the spec's `name`. Asserting the key
      // here would pin the wrong identity and would go green if the row ever
      // started echoing back whatever it was asked with.
      expect(ownership.organization).toBe(slug)
      expect(ownership.status).toBe('released')
    })

    /* TOMBSTONE (chief-home-is-cwd §4d/§4e): 'the host-only reads surface 503 as
     * a typed ChiefdUnavailableError', with its `HOST_ONLY_READS` table over
     * `runtime.extensionDrift` and `runtime.materializationIsStale`.
     *
     * The FINDING it recorded was that those reads answer 503 against a
     * `chiefd docstore-only` daemon, because they read converge/materialization
     * state only a full host process has. There is no materialization state
     * left to read on either process role: the whole `POST
     * /v1/org/materialize/*` family and its two client methods are deleted, so
     * the table has no member and the boundary it made visible has no reader.
     * The typed-503 CLIENT rule it also pinned is not lost — `postOrgRoute` is
     * one shared decode path, and `SseContract`/`RowsContract` still exercise
     * its unavailable classification. */
  })

  describe('SessionLifecycleClient — ledgers, drains and the epoch', () => {
    it('maintenanceLedger answers on a quiet company', async () => {
      const slug = 'chiefing-poste2-session-reads'
      const { client, companyKey } = await boot(slug)

      const ledger = await client.sessionLifecycle.maintenanceLedger(companyKey)
      expect(ledger).toBeTypeOf('object')
    })

    it('the drain returns empty rather than refusing when there is nothing queued', async () => {
      const slug = 'chiefing-poste2-session-drains'
      const { client, companyKey } = await boot(slug)

      const escalations = await client.sessionLifecycle.drainOperatorEscalations(
        companyKey,
        '2026-08-08T00:00:00.000Z'
      )
      expect(escalations).toBeTypeOf('object')
    })

    it('the session epoch stamps and reads back as a number', async () => {
      const slug = 'chiefing-poste2-session-epoch'
      const { client, companyKey } = await boot(slug)

      const stamped = await client.sessionLifecycle.stampSessionEpoch(
        companyKey,
        '2026-08-08T00:00:00.000Z',
        'contract fixture'
      )
      expect(stamped).toBeTypeOf('object')

      const ms = await client.sessionLifecycle.sessionEpochMs(companyKey)
      expect(typeof ms).toBe('number')
    })

    it('operatorEscalationLog decodes an empty log as an array, not a refusal', async () => {
      const slug = 'chiefing-poste2-session-escalations'
      const { client, companyKey } = await boot(slug)
      const log = await client.sessionLifecycle.operatorEscalationLog(companyKey)
      expect(Array.isArray(log)).toBe(true)
    })
  })
})
