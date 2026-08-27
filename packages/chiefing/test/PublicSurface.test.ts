// The checklist lock (E0-S4, #755). Asserts every contracted export exists
// with the right shape, that the retired D1 constant is absent, and that the
// package contains none of the forbidden blocking/env primitives.

import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import * as chiefing from '@/index'

const here = dirname(fileURLToPath(import.meta.url))
const srcDir = join(here, '..', 'src')

function listFilesRecursive(dir: string): string[] {
  const out: string[] = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) {
      out.push(...listFilesRecursive(full))
    } else if (entry.name.endsWith('.ts')) {
      out.push(full)
    }
  }
  return out
}

describe('public surface — classes and functions exist with the right typeof', () => {
  it('ChiefdClient facade', () => {
    expect(typeof chiefing.ChiefdClient).toBe('function')
  })

  it('the daemon rendezvous — how a caller inside a directory finds its own daemon', () => {
    expect(typeof chiefing.readDaemonRendezvous).toBe('function')
    expect(typeof chiefing.parseDaemonRendezvous).toBe('function')
    expect(typeof chiefing.rendezvousPath).toBe('function')
    expect(chiefing.RENDEZVOUS_FILENAME).toBe('daemon.json')
  })

  it('error taxonomy', () => {
    expect(typeof chiefing.ChiefdUnavailableError).toBe('function')
    expect(typeof chiefing.isTransientChiefdError).toBe('function')
    expect(typeof chiefing.OrgRowRefusalError).toBe('function')
    expect(typeof chiefing.ReminderRefusalError).toBe('function')
    expect(typeof chiefing.PersonContractsRefusalError).toBe('function')
    expect(typeof chiefing.AuthAcquisitionError).toBe('function')
  })

  it('transport', () => {
    expect(typeof chiefing.FetchTransport).toBe('function')
    expect(Array.isArray(chiefing.CONNECT_RETRY_BACKOFFS_MS)).toBe(true)
    expect(chiefing.CONNECT_RETRY_BACKOFFS_MS).toEqual([25, 75, 150])
  })

  it('resource clients', () => {
    expect(typeof chiefing.DocsClient).toBe('function')
    expect(typeof chiefing.AuthClient).toBe('function')
    expect(typeof chiefing.ManifestClient).toBe('function')
    expect(typeof chiefing.AggregatesClient).toBe('function')
    expect(typeof chiefing.MailboxClient).toBe('function')
    expect(typeof chiefing.PersonContractsClient).toBe('function')
    expect(typeof chiefing.RowStoresClient).toBe('function')
    expect(typeof chiefing.StaffingClient).toBe('function')
    expect(typeof chiefing.RemindersClient).toBe('function')
    expect(chiefing.MIN_REMINDER_INTERVAL_MS).toBe(60_000)
  })

  it('E5 seam: chiefing-auth-primitives', () => {
    expect(typeof chiefing.authChallengeMessage).toBe('function')
    expect(typeof chiefing.verifyAuthChallenge).toBe('function')
    expect(typeof chiefing.AgentTokenManager).toBe('function')
  })

  it('E5 seam: chiefing-sse-hub', () => {
    expect(typeof chiefing.subscribeSse).toBe('function')
    expect(typeof chiefing.SseWatcher).toBe('function')
  })

  it('SSE', () => {
    expect(typeof chiefing.activeSseHubCount).toBe('function')
    expect(typeof chiefing.computeBackoffDelayMs).toBe('function')
  })

  it('identity helpers', () => {
    expect(typeof chiefing.AUTH_DOMAIN_TAG).toBe('string')
    expect(typeof chiefing.IDENTITY_KEY_FILENAME).toBe('string')
    expect(typeof chiefing.generateAgentKeypair).toBe('function')
    expect(typeof chiefing.publicSpkiBase64FromPrivatePem).toBe('function')
    expect(typeof chiefing.loadOrCreateAgentKeypair).toBe('function')
    expect(typeof chiefing.ensurePersonIdentityKey).toBe('function')
    expect(typeof chiefing.signAuthChallenge).toBe('function')
  })

  it('discovery surface (E10-chiefing-addendum.md)', () => {
    expect(typeof chiefing.DiscoveryClient).toBe('function')
    expect(typeof chiefing.resolveCompanyChiefdUrl).toBe('function')
    expect(typeof chiefing.companyStoreDbPath).toBe('function')
    expect(typeof chiefing.beacondUrlFromEnvironment).toBe('function')
    expect(typeof chiefing.parseCompanyRow).toBe('function')
    expect(chiefing.DEFAULT_BEACOND_URL).toBe('http://127.0.0.1:6969')
    expect(chiefing.BEACOND_URL_ENV).toBe('BEACOND_URL')
  })

  it('company lifecycle surface', () => {
    expect(typeof chiefing.CompanyLifecycleClient).toBe('function')
    expect(typeof chiefing.chiefdHostUrlFromEnvironment).toBe('function')
    expect(typeof chiefing.isCompanyLifecyclePhaseName).toBe('function')
    expect(chiefing.DEFAULT_CHIEFD_HOST_URL).toBe('http://127.0.0.1:8789')
    expect(chiefing.CHIEFD_HOST_URL_ENV).toBe('CHIEFD_HOST_URL')
    // The vocabulary is the contract apps/web renders; a silent addition or
    // rename here is a product change, not a refactor.
    expect(chiefing.COMPANY_LIFECYCLE_PHASE_NAMES).toEqual([
      'company-daemon-start',
      'company-daemon-ready',
      'durable-create',
      'durable-create-complete',
      'durable-create-failed',
      'company-daemon-stop',
      'company-daemon-stopped',
      'company-daemon-stop-failed',
      // 'ceo-prepare' / 'ceo-prepare-failed' were here until
      // chief-home-is-cwd §4c deleted the phases with the daemon-side CEO boot.
      // This assertion is exact rather than a superset precisely so that
      // removal had to be made here too.
      'chief-start',
      'chief-start-failed'
    ])
  })

  it('SSE frame decoding', () => {
    expect(typeof chiefing.SseFrameDecoder).toBe('function')
    expect(typeof chiefing.readSseFrames).toBe('function')
  })

  it('beacond error taxonomy (ruling D6)', () => {
    expect(typeof chiefing.BeacondUnavailableError).toBe('function')
    expect(typeof chiefing.isTransientBeacondError).toBe('function')
    expect(typeof chiefing.DiscoveryRefusalError).toBe('function')
    expect(typeof chiefing.UnknownCompanyError).toBe('function')
    expect(typeof chiefing.CompanyNotRunningError).toBe('function')
    expect(typeof chiefing.CompanyLifecycleRefusalError).toBe('function')
  })
})

describe('ruling D1 — DEFAULT_CHIEFD_URL must never exist', () => {
  it('is absent from the public barrel', () => {
    expect('DEFAULT_CHIEFD_URL' in chiefing).toBe(false)
  })
})

describe('error predicates', () => {
  it('isTransientChiefdError is true only for kind unreachable', () => {
    const unreachable = new chiefing.ChiefdUnavailableError({
      kind: 'unreachable',
      url: 'http://x',
      path: '/p'
    })
    const timeout = new chiefing.ChiefdUnavailableError({
      kind: 'timeout',
      url: 'http://x',
      path: '/p'
    })
    expect(chiefing.isTransientChiefdError(unreachable)).toBe(true)
    expect(chiefing.isTransientChiefdError(timeout)).toBe(false)
    expect(chiefing.isTransientChiefdError(new Error('plain'))).toBe(false)
  })
})

describe('beacond error predicate — ruling D6, mirrors isTransientChiefdError', () => {
  it('isTransientBeacondError is true only for kind unreachable', () => {
    const unreachable = new chiefing.BeacondUnavailableError({
      kind: 'unreachable',
      beacondUrl: 'http://x',
      path: '/p'
    })
    const timeout = new chiefing.BeacondUnavailableError({
      kind: 'timeout',
      beacondUrl: 'http://x',
      path: '/p'
    })
    expect(chiefing.isTransientBeacondError(unreachable)).toBe(true)
    expect(chiefing.isTransientBeacondError(timeout)).toBe(false)
    expect(
      chiefing.isTransientBeacondError(
        new chiefing.DiscoveryRefusalError({
          status: 400,
          code: 'bad-request'
        })
      )
    ).toBe(false)
    expect(
      chiefing.isTransientBeacondError(new chiefing.UnknownCompanyError({ dir: '/work/acme' }))
    ).toBe(false)
    expect(
      chiefing.isTransientBeacondError(new chiefing.CompanyNotRunningError({ dir: '/work/acme' }))
    ).toBe(false)
    expect(chiefing.isTransientBeacondError(new Error('plain'))).toBe(false)
  })
})

describe('computeBackoffDelayMs — contractual formula', () => {
  it('min(initial * 2^n, max)', () => {
    expect(chiefing.computeBackoffDelayMs(0, 1000, 30000)).toBe(1000)
    expect(chiefing.computeBackoffDelayMs(5, 1000, 30000)).toBe(30000)
  })
})

describe('stubbed I/O methods throw the not-implemented contract', () => {
  // docs/locks/auth are real as of E2-S3 (proven directly in
  // ChiefdClientTest.test.ts and their own resources/*Test.test.ts files).
  // manifest/aggregates/mailbox/personContracts/rows are real as of
  // E2-S4 (proven directly below and in their own resources/*Test.test.ts
  // files). staffing/reminders are real as of E2-S5 (proven in the
  // dedicated describe block below, ported out of this stub-lock once S5
  // filled them). This is the mechanical lock behind team-lead's ruling on
  // #772: the facade's "fail loudly, name the story" contract is satisfied
  // by each unimplemented METHOD throwing (not by bare property access
  // throwing) — one assertion per resource here is what would catch a
  // future story silently swapping a throw for a stub return value.
  const client = new chiefing.ChiefdClient({ url: 'http://127.0.0.1:1' })
  const notImplemented = /not implemented: @chief\/chiefing stub — implemented by E2-S\d/

  it('manifest is real (E2-S4) — no longer throws the stub contract', async () => {
    await expect(client.manifest.read('acme')).rejects.not.toThrow(notImplemented)
  })

  it('aggregates is real (E2-S4) — no longer throws the stub contract', async () => {
    await expect(client.aggregates.activityRead('acme')).rejects.not.toThrow(notImplemented)
  })

  it('mailbox is real (E2-S4) — no longer throws the stub contract', async () => {
    await expect(client.mailbox.read('acme')).rejects.not.toThrow(notImplemented)
  })

  it('personContracts is real (E2-S4) — no longer throws the stub contract', async () => {
    await expect(client.personContracts.read('acme')).rejects.not.toThrow(notImplemented)
  })

  it('rows is real (E2-S4) — no longer throws the stub contract', async () => {
    await expect(client.rows.readSessionEpoch('acme')).rejects.not.toThrow(notImplemented)
  })
})

describe('staffing/reminders are real — E2-S5, no longer the not-implemented stub', () => {
  // Ported out of the stub-lock describe above now that E2-S5 filled these
  // two resource clients: each now performs a real request against the
  // client's url (unroutable here) instead of throwing synchronously, so it
  // rejects with SOME transport-layer failure — never the not-implemented
  // stub message — proving the body is filled in, not merely present
  // (mirrors ChiefdClientTest.test.ts's docs/locks/auth assertion for
  // E2-S3). Not asserting the exact rejection type: an unroutable-port
  // connection failure's precise shape (ChiefdUnavailableError vs a raw
  // fetch TypeError) is a FetchTransport/runtime classification detail
  // outside this story's scope — asserting only "not the stub message" is
  // the invariant E2-S5 actually owns and keeps this lock robust to that
  // classification changing under it.
  const client = new chiefing.ChiefdClient({ url: 'http://127.0.0.1:1' })
  const notImplemented = /not implemented: @chief\/chiefing stub — implemented by E2-S\d/

  it('staffing — E2-S5', async () => {
    await expect(client.staffing.startPerson('acme', 'person-1')).rejects.not.toThrow(
      notImplemented
    )
  })

  it('reminders — E2-S5', async () => {
    await expect(
      client.reminders.armReminder({
        slug: 'acme',
        personId: 'person-1',
        prompt: 'check the deploy',
        intervalMs: 3_600_000
      })
    ).rejects.not.toThrow(notImplemented)
  })
})

describe('ChiefdClient facade constructs without I/O', () => {
  it('wires all resource fields', () => {
    const client = new chiefing.ChiefdClient({ url: 'http://127.0.0.1:1' })
    expect(client.docs).toBeInstanceOf(chiefing.DocsClient)
    expect(client.auth).toBeInstanceOf(chiefing.AuthClient)
    expect(client.manifest).toBeInstanceOf(chiefing.ManifestClient)
    expect(client.aggregates).toBeInstanceOf(chiefing.AggregatesClient)
    expect(client.mailbox).toBeInstanceOf(chiefing.MailboxClient)
    expect(client.personContracts).toBeInstanceOf(chiefing.PersonContractsClient)
    expect(client.rows).toBeInstanceOf(chiefing.RowStoresClient)
    expect(client.staffing).toBeInstanceOf(chiefing.StaffingClient)
    expect(client.reminders).toBeInstanceOf(chiefing.RemindersClient)
  })
})

describe('no forbidden blocking/env primitives anywhere under src/', () => {
  const forbidden = /Atomics\.wait|spawnSync|setInterval|process\.env|child_process/

  it('grep-equivalent scan of every source file', () => {
    const files = listFilesRecursive(srcDir)
    const hits: string[] = []
    for (const file of files) {
      const content = readFileSync(file, 'utf8')
      if (forbidden.test(content)) {
        hits.push(file)
      }
    }
    expect(hits).toEqual([])
  })
})

describe('rulings D1/D7 — no fixed-port fallback outside discovery', () => {
  it('DEFAULT_CHIEFD_URL / :8792 never appear outside src/discovery', () => {
    const files = listFilesRecursive(srcDir).filter(
      (file) => !file.includes(`${join('discovery')}${'/'}`)
    )
    const forbidden = /DEFAULT_CHIEFD_URL|8792|location\.json|location-registry/
    const hits = files.filter((file) => forbidden.test(readFileSync(file, 'utf8')))
    expect(hits).toEqual([])
  })

  it('6969/DEFAULT_BEACOND_URL is compiled in exactly once, in Company.ts', () => {
    const files = listFilesRecursive(srcDir)
    const hits = files.filter((file) =>
      /['"]http:\/\/127\.0\.0\.1:6969/.test(readFileSync(file, 'utf8'))
    )
    expect(hits).toEqual([join(srcDir, 'discovery', 'Company.ts')])
  })

  it('the chiefd-host address is the ONLY other compiled-in address', () => {
    // There are exactly two per-BOX services whose address cannot come from
    // discovery: beacond (discovery cannot discover itself) and `chief host`
    // (a company that does not exist yet has no registration to be found
    // through). Everything else — every company chiefd — is looked up per use.
    // A third entry in this list would mean that rule slipped.
    const files = listFilesRecursive(srcDir)
    const hits = files.filter((file) =>
      /['"]http:\/\/127\.0\.0\.1:8789/.test(readFileSync(file, 'utf8'))
    )
    expect(hits).toEqual([join(srcDir, 'resources', 'CompanyLifecycle.ts')])
  })
})

describe('the dead twins stay deleted (#751/G5, #751/G13)', () => {
  // These four assertions replace four `typeof ... === 'function'` checks that
  // used to sit in the blocks above. Each symbol was exported, unit-tested
  // green, and called by nothing:
  //
  //   registrationLiveness / currentLivenessHost — a liveness judge that
  //     pinned the rule the Rust port CORRECTED (an unnameable host is judged
  //     by pid, `chiefd/src/lifecycle/discovery.rs:90-125`, not answered
  //     'unknown'). A second implementation encoding the superseded rule.
  //   UnreachableCircuit — a breaker no consumer ever opted into.
  //   LOCK_RETRY_BASE_DELAYS_MS — a ladder for a lock surface E8-S6c deleted.
  //
  // The contract is now their absence. A revert is a red test, not a silent
  // second implementation waiting for its first caller.
  const DELETED_EXPORTS = [
    'registrationLiveness',
    'currentLivenessHost',
    'UnreachableCircuit',
    'LOCK_RETRY_BASE_DELAYS_MS'
  ] as const

  it.each(DELETED_EXPORTS)('@chief/chiefing does not export %s', (name) => {
    expect(Object.prototype.hasOwnProperty.call(chiefing, name)).toBe(false)
  })

  it('negative self-check: the probe finds a symbol that IS exported', () => {
    expect(Object.prototype.hasOwnProperty.call(chiefing, 'DiscoveryClient')).toBe(true)
  })
})
