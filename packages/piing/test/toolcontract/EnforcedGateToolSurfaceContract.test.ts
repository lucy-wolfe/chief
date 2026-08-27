/**
 * The tool surface installs under an ENFORCED auth gate.
 *
 * # The defect this exists for
 *
 * `installOrganizationToolSurface` does not merely register tools. Its first
 * act is an authenticated HTTP call: `installOrganizationIntercom` →
 * `loadIntercomOrganization` → `readIntercomManifestWire` →
 * `POST /v1/org/manifest/read`. That call goes out through
 * `paneChiefdTransport`, which carries the pane's P-256 credential — but only
 * once `readAgentKeypair` finds a key in the person's identity directory.
 * Before the key exists, A4's documented fallback hands the intercom a bare `FetchTransport`
 * with no `Authorization` header at all.
 *
 * With the universal gate off, an unauthenticated manifest read is served and
 * nothing is visibly wrong. With the gate ENFORCED it is
 * `401 missing bearer token`, and the whole tool surface fails to install.
 *
 * # Why this file boots its own company with the gate on
 *
 * The gate is off in every deployment `main` can produce today, so a proof
 * that ran in the default posture would pass identically whether or not the
 * install authenticated — it would prove nothing, and it would go on proving
 * nothing after the switch is deleted. Arming `CHIEFD_AUTH_ENABLED=enforce`
 * here runs the install against exactly the posture that deletion makes
 * permanent, BEFORE it lands. When the switch goes, the `env` line goes and
 * every assertion below stands unchanged — the same shape
 * `packages/testing/test/CompanyDaemonAuth.test.ts` (A7) already uses.
 *
 * # The synchronous genesis claim, asserted rather than assumed
 *
 * Genesis creates and enrols the Chief identity before it returns. The direct
 * file read and challenge below pin that boundary before a tool surface is
 * installed. A wait would hide a broken synchronous contract. The Chief has
 * no agent home; its key is directly under `<dir>/.chief`.
 *
 * It never skips, for the reason `OrganizationToolContract.test.ts` gives at
 * length: a machine that has not built the product is a red, not a pass.
 */
import { execFileSync } from 'node:child_process'
import { join } from 'node:path'

import { readAgentKeypair } from '@chief/chiefing'
import type { AuthorizedFetch, TmuxHostedCompany } from '@chief/testing'
import {
  assertChiefdBinaryBuilt,
  createOperatorFetch,
  startTmuxHostedCompany,
  surfaceDaemonLogOnFailure
} from '@chief/testing'
import { isNullish } from '@test/support/Nullish'
import { installOrganizationToolSurface } from '@test/support/OrganizationToolSurface'
import type { OrganizationToolSurface } from '@test/types/OrganizationToolSurface'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

const REPO_ROOT = join(import.meta.dirname, '..', '..', '..', '..')
const SLUG = 'toolcontractenforced'
const BOOT_TIMEOUT_MS = 120_000
const TOOL_TIMEOUT_MS = 90_000

/**
 * The universal gate, ENFORCED. One place, so the switch's deletion is one
 * edit here — and after that edit this suite asserts the only posture there
 * is.
 */
const ENFORCED = { CHIEFD_AUTH_ENABLED: 'enforce' } as const

let company: TmuxHostedCompany
let operatorFetch: AuthorizedFetch
let surface: OrganizationToolSurface

function assertTmuxAvailable(): void {
  try {
    execFileSync('tmux', ['-V'], { stdio: 'ignore' })
  } catch {
    throw new Error(
      'this proof needs tmux: a full `chiefd run` mounts the host capability the ' +
        'organization tools reconcile against after their durable write'
    )
  }
}

/** The folder holding `keys/operator.key`: the company's own `<dir>/.chief`,
 * minted there by the daemon that serves that directory. */
function keysRootOf(target: TmuxHostedCompany): string {
  return join(target.dir, '.chief')
}

function chiefIdentityDir(): string {
  return join(company.dir, '.chief')
}

/**
 * Genesis, carried by the harness's OPERATOR bearer.
 *
 * Under the enforced gate this route requires a credential like every other
 * non-exempt route, and the operator is the correct principal for it: a
 * company that does not exist yet has no person who could have signed. The
 * bearer comes from `@chief/testing`'s A7 acquirer, which reads the key
 * `chiefd run` minted in this harness's own temp data root — never a fresh
 * acquirer written here.
 */
async function postGenesis(): Promise<void> {
  /* eslint-disable lucy/no-json-stringify */
  // The genesis wire stays raw so this proof does not use the client it tests.
  const body = JSON.stringify({
    slug: company.companyKey,
    at: new Date().toISOString(),
    spec: {
      name: SLUG,
      purpose: 'the company the enforced-gate install proof drives',
      chief: { name: 'Chief' }
    }
  })
  /* eslint-enable lucy/no-json-stringify */
  const response = await operatorFetch('/v1/org/manifest/genesis', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body
  })
  const text = await response.text()
  if (!response.ok) {
    throw new Error(`genesis under the enforced gate failed with ${response.status}: ${text}`)
  }
}

/**
 * Assert the Chief key and enrolment immediately after genesis. Nothing here
 * installs a tool surface or calls a tool.
 */
async function assertChiefIdentity(): Promise<void> {
  if (isNullish(readAgentKeypair(chiefIdentityDir()).keypair)) {
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
  company = await startTmuxHostedCompany({ slug: SLUG, repoRoot: REPO_ROOT, env: ENFORCED })
  operatorFetch = createOperatorFetch({ url: company.url, keysRoot: keysRootOf(company) })
  await postGenesis()
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

describe('the organization tool surface installs under an enforced auth gate', () => {
  it(
    'the gate really enforces: the install-time manifest read is 401 without a credential',
    async () => {
      // Asserted FIRST and by hand, because every other assertion in this file
      // is worthless if the gate is not actually on. This is the exact route
      // `readIntercomManifestWire` calls, with the exact body it sends, and
      // the only difference from the install is the missing header.
      /* eslint-disable lucy/no-json-stringify */
      // The refusal is about the bytes on the wire, so nothing helps compose
      // them.
      const response = await fetch(`${company.url}/v1/org/manifest/read`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ slug: company.companyKey })
      })
      /* eslint-enable lucy/no-json-stringify */
      const body = await response.text()

      expect(
        response.status,
        `the enforced gate must refuse an unauthenticated manifest read, got ` +
          `${response.status}: ${body.slice(0, 300)}`
      ).toBe(401)
    },
    TOOL_TIMEOUT_MS
  )

  it('genesis returned with the Chief key ready before a tool surface was installed', () => {
    expect(
      readAgentKeypair(chiefIdentityDir()).keypair,
      'genesis must mint the Chief identity directly under the company state root'
    ).not.toBeUndefined()
  })

  it('the install completed, so the intercom read the manifest with the pane credential', () => {
    // Reaching this line at all is half the assertion: `installOrganizationIntercom`
    // THROWS on a refused manifest read, so a failed install fails `beforeAll`
    // with `Cannot read normalized organization authority`. The tool count is
    // the other half — an install that registered nothing is not an install.
    expect(surface.tools.size).toBeGreaterThan(0)
    expect([...surface.tools.keys()]).toContain('org_roster')
  })

  it(
    'and a tool call goes through, so the pane credential works past install too',
    async () => {
      // The install proves the FIRST authenticated call. This proves the
      // steady state: `org_roster` reads through the same pane transport, and
      // under this gate an unauthenticated read of it is the 401 asserted
      // above.
      const roster = await surface.call('org_roster', {})

      expect(roster.ok, `org_roster failed under the enforced gate: ${roster.message}`).toBe(true)
      // The one person this company has. Matched case-insensitively because
      // the rendering may name the display name or the id, and which of the
      // two it picks is not this file's subject.
      expect(roster.message, `the roster named nobody: ${roster.message}`).toMatch(/chief/i)
    },
    TOOL_TIMEOUT_MS
  )
})
