import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { generateAgentKeypair, operatorKeyPath } from '@chief/chiefing'
import {
  createFakeChiefApi,
  FIXTURE_COMPANY_KEY,
  setFixtureOperatorPublicKey
} from '@test/harness/FakeChiefApi'
import type { FakeChiefApiFixtures } from '@test/types/FakeChiefApi'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// `CHIEF_WEB_OPERATOR_PRIVATE_KEY` and `CHIEF_WEB_OPERATOR_IDENTITY_ID` are
// DELETED (A2). The operator key is derived — `<data-root>/keys/operator.key`,
// the same file the daemon mints and the `chiefd` CLI signs with — so this
// suite stages a real key under a temporary `HOME` instead of configuring one.
// The identity id is the literal `operator` the daemon hardcodes.
const ENV_KEYS = ['BEACOND_URL', 'HOME']

/* eslint-disable lucy/no-process-env */
// Test-only env fixture manipulation for the route handler under test
// (`app/api/session/route.ts` reads real `process.env` via `common/Env.ts`,
// so exercising it here requires setting the same process's env directly).
// Centralized to these three helpers rather than scattered `process.env`
// reads/writes through the file, mirroring the "one place touches env"
// convention `lucy/no-process-env` enforces for src/.
function readEnv(key: string): string | undefined {
  return process.env[key]
}
function writeEnv(key: string, value: string | undefined): void {
  if (typeof value === 'string') process.env[key] = value
  else delete process.env[key]
}
/* eslint-enable lucy/no-process-env */

function saveEnv(): Record<string, string | undefined> {
  const saved: Record<string, string | undefined> = {}
  for (const key of ENV_KEYS) saved[key] = readEnv(key)
  return saved
}

function restoreEnv(saved: Record<string, string | undefined>): void {
  for (const key of ENV_KEYS) writeEnv(key, saved[key])
}

/** A throwaway company directory with `.chief/keys/operator.key` staged 0600,
 * exactly as that company's daemon mints it at boot. There is no box-wide
 * operator key: the key belongs to the company whose daemon minted it, inside
 * that company's own directory. `mode` is a parameter so one test can widen
 * it. */
function stageOperatorKey(privatePkcs8Pem: string, mode = 0o600): string {
  const companyDir = mkdtempSync(join(tmpdir(), 'chief-web-session-'))
  const keyPath = operatorKeyPath(join(companyDir, '.chief'))
  mkdirSync(join(companyDir, '.chief', 'keys'), { recursive: true })
  writeFileSync(keyPath, privatePkcs8Pem, { mode })
  chmodSync(keyPath, mode)
  return companyDir
}

/** The registry row for a company whose directory is `companyDir`. The route
 * reads that company's operator key out of it, so the fixture's directory and
 * the staged key's directory have to be the same one. */
function registeredAt(companyDir: string): Partial<FakeChiefApiFixtures> {
  return {
    companies: [
      {
        key: FIXTURE_COMPANY_KEY,
        dir: companyDir,
        slug: 'cobalt',
        status: 'running',
        chiefd: { healthy: true, httpStatus: 200, reason: 'ok' }
      }
    ]
  }
}

describe('POST /api/session', () => {
  let saved: Record<string, string | undefined>
  const homes: string[] = []

  beforeEach(() => {
    saved = saveEnv()
    setFixtureOperatorPublicKey(undefined)
  })

  afterEach(() => {
    restoreEnv(saved)
    vi.unstubAllGlobals()
    setFixtureOperatorPublicKey(undefined)
    while (homes.length > 0) {
      const home = homes.pop()
      if (typeof home === 'string') rmSync(home, { recursive: true, force: true })
    }
  })

  // apps/api is deleted, so the token is minted by a COMPANY'S OWN chiefd,
  // with that company's OWN operator key, resolved through beacond. The route
  // therefore needs `?company=<key>` and the fake now stands in for beacond +
  // that company's daemon rather than for a single global api.
  it("completes challenge→token against the company's chiefd and returns the JWT", async () => {
    const keypair = generateAgentKeypair()
    const companyDir = stageOperatorKey(keypair.privatePkcs8Pem)
    homes.push(companyDir)
    writeEnv('BEACOND_URL', 'http://fake-beacond.test')
    setFixtureOperatorPublicKey(keypair.publicSpkiBase64)

    const { fetchImpl, issuedTokens, requests } = createFakeChiefApi(registeredAt(companyDir))
    vi.stubGlobal('fetch', fetchImpl)

    const { POST } = await import('@/app/api/session/route')
    const response = await POST(
      new Request(`http://web.test/api/session?company=${FIXTURE_COMPANY_KEY}`, { method: 'POST' })
    )
    expect(response.status).toBe(200)
    const body: unknown = await response.json()
    expect(body).toEqual({ token: issuedTokens[0], identityId: 'operator' })

    // FakeChiefApi validated a REAL IEEE-P1363 signature via chiefing's
    // verifyAuthChallenge (not a hardcoded fixture string) — a wrong
    // signature would have 401'd inside the fake, which the assertion
    // above (a successful 200 + issued token) rules out.
    // THREE hops now, and the order is the point: beacond first to learn which
    // chiefd owns this company, then that chiefd's challenge/token pair. With
    // apps/api deleted there is no single global auth endpoint to skip to.
    //
    // THE `/v1` PREFIX IS THE REGRESSION THIS PINS. These two paths were
    // `/auth/challenge` and `/auth/token`, written when `apiUrl` meant the
    // deleted apps/api and already carried the version. `apiUrl` is a company
    // daemon's bare origin now, where the routes are `/v1/auth/*`, so every
    // call was a 404 that surfaced as "operator challenge failed: status 404".
    expect(requests.map((request) => request.path)).toEqual([
      '/v1/list',
      '/v1/auth/challenge',
      '/v1/auth/token'
    ])
  })

  it('answers { token: null } when this company has no operator key yet', async () => {
    // No key staged: the daemon mints `<dir>/.chief/keys/operator.key` at boot,
    // so a company that has never been run legitimately has none.
    const companyDir = mkdtempSync(join(tmpdir(), 'chief-web-session-'))
    homes.push(companyDir)

    const { fetchImpl } = createFakeChiefApi(registeredAt(companyDir))
    vi.stubGlobal('fetch', fetchImpl)

    const { POST } = await import('@/app/api/session/route')
    const response = await POST(
      new Request(`http://web.test/api/session?company=${FIXTURE_COMPANY_KEY}`, { method: 'POST' })
    )
    expect(response.status).toBe(200)
    const body: unknown = await response.json()
    expect(body).toEqual({ token: null, identityId: 'operator' })
  })

  it('a group-readable operator key is not used at all', async () => {
    // The same rule the daemon (`identity_keys::load_private_key_pem`) and the
    // `chiefd` CLI apply to this exact file: a private key others can read is
    // a key to assume is copied. Here it reads as no key, which is the shape
    // this route already has for absence — chiefing's `readIdentityKeyPem`
    // cannot report, and the daemon refuses loudly on the same rule.
    const keypair = generateAgentKeypair()
    const companyDir = stageOperatorKey(keypair.privatePkcs8Pem, 0o644)
    homes.push(companyDir)
    setFixtureOperatorPublicKey(keypair.publicSpkiBase64)

    const { fetchImpl, requests } = createFakeChiefApi(registeredAt(companyDir))
    vi.stubGlobal('fetch', fetchImpl)

    const { POST } = await import('@/app/api/session/route')
    const response = await POST(
      new Request(`http://web.test/api/session?company=${FIXTURE_COMPANY_KEY}`, { method: 'POST' })
    )
    expect(await response.json()).toEqual({ token: null, identityId: 'operator' })
    // ONE request, and it is the registry read that finds the directory the
    // key would have been in. Nothing was sent to the daemon: an unusable key
    // must not produce a challenge.
    expect(requests.map((request) => request.path)).toEqual(['/v1/list'])
  })

  it('an upstream failure surfaces as 502 { error: { code: "auth-upstream" } }', async () => {
    const keypair = generateAgentKeypair()
    const companyDir = stageOperatorKey(keypair.privatePkcs8Pem)
    homes.push(companyDir)
    writeEnv('BEACOND_URL', 'http://fake-beacond.test')

    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('internal error', { status: 500 }))
    )

    const { POST } = await import('@/app/api/session/route')
    const response = await POST(
      new Request(`http://web.test/api/session?company=${FIXTURE_COMPANY_KEY}`, { method: 'POST' })
    )
    expect(response.status).toBe(502)
    const body: unknown = await response.json()
    expect(errorCode(body)).toBe('auth-upstream')
  })
})

function errorCode(body: unknown): string | undefined {
  if (!body || typeof body !== 'object' || !('error' in body)) return undefined
  const error = body.error
  if (!error || typeof error !== 'object' || !('code' in error)) return undefined
  return typeof error.code === 'string' ? error.code : undefined
}
