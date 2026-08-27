/**
 * The reminder delivery proof runs in its own live company.
 *
 * The main organization tool-contract file is intentionally ordered: later
 * families read state created by earlier families. This proof has one real
 * wall-clock wait, the daemon's 60-second reminder cadence, and needs none of
 * that ordered state. Keeping it in its own file lets Vitest run the wait in
 * parallel with the ordered file without changing the assertion or replacing
 * the real daemon with a fake clock.
 */
import { execFileSync } from 'node:child_process'
import { join } from 'node:path'

import { readAgentKeypair } from '@chief/chiefing'
import type { TmuxHostedCompany } from '@chief/testing'
import {
  acquireOperatorBearer,
  assertChiefdBinaryBuilt,
  startTmuxHostedCompany,
  surfaceDaemonLogOnFailure
} from '@chief/testing'
import { isNullish } from '@test/support/Nullish'
import { installOrganizationToolSurface } from '@test/support/OrganizationToolSurface'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

const REPO_ROOT = join(import.meta.dirname, '..', '..', '..', '..')
const SLUG = 'toolcontract-reminder'
const BOOT_TIMEOUT_MS = 120_000
const REMINDER_FIRE_BUDGET_MS = 120_000
const REMINDER_FIRE_TIMEOUT_MS = 180_000
let company: TmuxHostedCompany
let surface: Awaited<ReturnType<typeof installOrganizationToolSurface>>

function assertTmuxAvailable(): void {
  try {
    execFileSync('tmux', ['-V'], { stdio: 'ignore' })
  } catch {
    throw new Error(
      'the reminder delivery contract needs tmux: a full chiefd run mounts the ' +
        'host capability that the reminder tool uses after its durable write'
    )
  }
}

/**
 * The OPERATOR's bearer for this daemon, minted from the key it wrote at boot.
 *
 * This file boots a FULL `chiefd run`, which attaches an auth runtime, so the
 * genesis below is a real authenticated call rather than an anonymous one that
 * happened to be served while the universal gate was off. The acquirer is
 * `@chief/testing`'s own (A7, #1114) rather than a new one: it is pinned
 * byte-for-byte against `@chief/chiefing`'s signer by `HarnessSignatureParity`,
 * and a second copy here would be the one with no parity test. It takes the
 * company's own `<dir>/.chief`. Only the request itself stays
 * raw, so this proof still does not use the client it tests.
 */
async function operatorBearerFor(target: TmuxHostedCompany): Promise<Record<string, string>> {
  const token = await acquireOperatorBearer({
    url: target.url,
    keysRoot: join(target.dir, '.chief')
  })
  return { authorization: `Bearer ${token}` }
}

async function postGenesis(target: TmuxHostedCompany): Promise<void> {
  const authorization = await operatorBearerFor(target)
  const response = await fetch(`${target.url}/v1/org/manifest/genesis`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authorization },
    /* eslint-disable lucy/no-json-stringify */
    // The genesis wire must stay raw so this proof does not use the client it tests.
    body: JSON.stringify({
      slug: target.companyKey,
      at: new Date().toISOString(),
      spec: {
        name: target.slug,
        purpose: 'the company the reminder delivery contract drives',
        chief: { name: 'Chief' }
      }
    })
    /* eslint-enable lucy/no-json-stringify */
  })
  const text = await response.text()
  if (!response.ok) {
    throw new Error(`genesis failed with ${response.status}: ${text.slice(0, 400)}`)
  }
}

function reminderIdOf(details: unknown): string | undefined {
  if (!isRecord(details) || !isRecord(details.reminder)) return undefined
  return typeof details.reminder.id === 'string' ? details.reminder.id : undefined
}

function listedReminder(
  details: unknown,
  id: string | undefined
): { fireCount: number; lastFiredAt: unknown } | undefined {
  if (!isRecord(details) || !Array.isArray(details.reminders)) return undefined
  const row = details.reminders.filter(isRecord).find((entry) => entry.id === id)
  if (!row) return undefined
  return {
    fireCount: typeof row.fireCount === 'number' ? row.fireCount : 0,
    lastFiredAt: row.lastFiredAt
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && !isNullish(value) && !Array.isArray(value)
}

/**
 * Assert that genesis minted AND enrolled the Chief identity.
 *
 * This runs BEFORE the tool surface is installed, because the install is
 * itself an authenticated call: `installOrganizationIntercom` reads the
 * manifest through the pane transport, and that transport carries no
 * credential at all until this key is on disk. Genesis now provisions both
 * parts before its response. A direct read and challenge pin that synchronous
 * contract. A poll would hide a regression. The Chief key is directly under
 * `<dir>/.chief`; no Chief agent home exists.
 */
async function assertChiefIdentity(): Promise<void> {
  const identityDir = join(company.dir, '.chief')
  if (isNullish(readAgentKeypair(identityDir).keypair)) {
    throw new Error(`genesis returned without minting the '${SLUG}' Chief identity key`)
  }
  /* eslint-disable lucy/no-json-stringify */
  // Raw JSON keeps this authentication assertion independent of a client wrapper.
  const challenge = await fetch(`${company.url}/v1/auth/challenge`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ identityId: 'chief' })
  })
  /* eslint-enable lucy/no-json-stringify */
  const text = await challenge.text()
  if (challenge.status !== 200) {
    throw new Error(
      `genesis returned without enrolling the '${SLUG}' Chief identity: ` +
        `${challenge.status} ${text.slice(0, 400)}`
    )
  }
}

beforeAll(async () => {
  assertTmuxAvailable()
  assertChiefdBinaryBuilt(REPO_ROOT)
  company = await startTmuxHostedCompany({ slug: SLUG, repoRoot: REPO_ROOT })
  await postGenesis(company)
  await assertChiefIdentity()
  surface = await installOrganizationToolSurface({
    chiefdUrl: company.url,
    organization: company.slug,
    organizationDir: company.dir,
    personId: 'chief',
    launcherRoot: REPO_ROOT,
    tmuxSocket: company.tmuxSocket,
    tmuxSession: company.slug
  })
}, BOOT_TIMEOUT_MS)

afterAll(async () => {
  await company?.stop()
})

surfaceDaemonLogOnFailure(() => company)

describe('durable reminder delivery, in an isolated live company', () => {
  it(
    'a reminder actually FIRES — the tool reports a real delivery, not just an accepted write',
    async () => {
      // The whole product of a reminder is that it recurs. An arm that returns
      // ok and never fires is the failure this asserts against, so the proof
      // observes a delivery rather than an acceptance. 60s is chiefd's own
      // floor (anything faster is a poll), so this test cannot be made quick.
      const armed = await surface.call('org_create_reminder', {
        prompt: 'Fire once, so the suite can watch a delivery happen.',
        intervalMs: 60_000,
        recurring: false
      })
      expect(armed.ok, `org_create_reminder failed: ${armed.message}`).toBe(true)
      const reminderId = reminderIdOf(armed.details)

      const deadline = Date.now() + REMINDER_FIRE_BUDGET_MS
      let fired: { fireCount: number; lastFiredAt: unknown } | undefined
      for (;;) {
        const listed = await surface.call('org_list_reminders', {})
        expect(listed.ok, `org_list_reminders failed: ${listed.message}`).toBe(true)
        const row = listedReminder(listed.details, reminderId)
        if (row && row.fireCount >= 1) {
          fired = row
          break
        }
        if (Date.now() >= deadline) break
        await new Promise((resolve) => setTimeout(resolve, 2_000))
      }

      expect(
        fired,
        'the reminder never fired inside its budget — an armed reminder that never ' +
          'delivers is the entire failure this family exists to prevent'
      ).toBeDefined()
      expect(fired?.fireCount).toBeGreaterThanOrEqual(1)
      expect(typeof fired?.lastFiredAt, 'a fired reminder stamps lastFiredAt').toBe('string')
    },
    REMINDER_FIRE_TIMEOUT_MS
  )
})
