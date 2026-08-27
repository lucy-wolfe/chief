/**
 * A4 acceptance criterion 5: every in-pane caller class reaches chiefd with a
 * bearer, PROVED BY A TEST rather than by inspection.
 *
 * Three caller classes run inside one pane over one identity key, and until now
 * only the first of them reached for it:
 *
 *  1. `organization-intercom`'s org tools — already correct, asserted here so a
 *     regression in the shared acquirer is caught for all three at once;
 *  2. `team-ui`'s footer reads — `new FetchTransport(url)`, no credential;
 *  3. every SSE reader — `accept: text/event-stream` and no auth path at all.
 *
 * Each test below writes a REAL P-256 key into a REAL pi-home at a real mode,
 * lets the client really sign a daemon-issued challenge against a stubbed
 * daemon, and asserts the header that reaches the wire. Nothing here reads
 * source text.
 */
import { chmodSync, mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { IDENTITY_KEY_FILENAME, loadOrCreateAgentKeypair } from '@chief/chiefing'
import { afterEach, beforeEach, describe, expect, test } from 'vitest'

import {
  organizationSseBearer,
  readDurableDocumentCached
} from '../../extensions/organization-intercom'
import { chiefdPostJsonAsync, teamUiSseBearer } from '../../extensions/team-ui'

/* eslint-disable lucy/no-process-env, lucy/no-raw-null-check */
/* team-ui resolves its pane identity from the launcher-stamped environment —
   the same variables its footer already reads its stores from — so the claim
   under test is precisely about what the live process env produces, and there
   is no indirection to import instead. Restoring that environment afterwards
   then has to tell "this variable was absent" from "this variable was the
   empty string", because `delete` and assignment are different repairs, and
   `isNullish` collapses exactly the distinction the restore depends on. Same
   two carve-outs, and the same reasons, as PaneEndpoint.test.ts's. */

interface CapturedRequest {
  url: string
  headers: Record<string, string>
}

const realFetch = globalThis.fetch
const requests: CapturedRequest[] = []
const environmentKeys = [
  'ORG_LAUNCHER_IDENTITY_DIR',
  'ORG_LAUNCHER_ORGANIZATION',
  'ORG_LAUNCHER_PERSON',
  'ORG_LAUNCHER_ORG_DIR'
]
const savedEnvironment = new Map<string, string | undefined>()
let nextUrl = 0

/** A distinct base URL per test: the shared acquirer caches one manager per
 * (url, person, key) for the life of the process, so two tests sharing a URL
 * would share a token and the second would prove nothing. */
function freshUrl(): string {
  nextUrl += 1
  return `http://pane-callers-${nextUrl}.test`
}

/** A company directory holding the Chief's real identity key. */
async function paneHome(mode = 0o600): Promise<string> {
  // A company IS a directory, so the fixture mints one rather than a slug
  // under a shared orgs root — and everything chief owns for it, pi-homes
  // included, lives under that directory's `.chief` root. The Chief has no
  // agent home, so its identity key lives directly in that root.
  const organizationDir = mkdtempSync(join(tmpdir(), 'piing-pane-callers-'))
  const identityDir = join(organizationDir, '.chief')
  mkdirSync(identityDir, { recursive: true })
  await loadOrCreateAgentKeypair(identityDir)
  chmodSync(join(identityDir, IDENTITY_KEY_FILENAME), mode)
  return organizationDir
}

function authorizationOn(pathSuffix: string): string | undefined {
  return requests.find((request) => request.url.endsWith(pathSuffix))?.headers.Authorization
}

/** These tests assert what reached the WIRE, so how the stub's body decodes is
 * beside the point: a decode refusal still proves the request was sent, and
 * pinning the exact envelope here would make this a test of the row-read
 * decoder instead of a test of the credential. */
async function reaching(call: Promise<unknown>): Promise<void> {
  try {
    await call
  } catch {
    /* the request is what is under test, not the answer */
  }
}

beforeEach(() => {
  requests.length = 0
  for (const key of environmentKeys) savedEnvironment.set(key, process.env[key])
  globalThis.fetch = (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input)
    const headers: Record<string, string> = {}
    for (const [name, value] of Object.entries(init?.headers ?? {})) headers[name] = String(value)
    requests.push({ url, headers })
    if (url.endsWith('/v1/auth/challenge')) {
      return Promise.resolve(new Response('{"nonceId":"n-1","nonce":"a-fixed-width-nonce"}'))
    }
    if (url.endsWith('/v1/auth/token')) {
      return Promise.resolve(new Response('{"token":"pane-token"}'))
    }
    return Promise.resolve(new Response('{"ledger":{},"seq":1}'))
  }
})

afterEach(() => {
  globalThis.fetch = realFetch
  for (const key of environmentKeys) {
    const value = savedEnvironment.get(key)
    if (value === undefined) delete process.env[key]
    else process.env[key] = value
  }
})

describe('team-ui reaches chiefd with a bearer', () => {
  test('a footer read carries the pane`s minted bearer, not a bare request', async () => {
    const url = freshUrl()
    const organizationDir = await paneHome()
    process.env.ORG_LAUNCHER_IDENTITY_DIR = join(organizationDir, '.chief')
    process.env.ORG_LAUNCHER_ORGANIZATION = 'acme'
    process.env.ORG_LAUNCHER_PERSON = 'ceo'
    process.env.ORG_LAUNCHER_ORG_DIR = organizationDir

    await reaching(chiefdPostJsonAsync(url, '/v1/org/supervision/read', { slug: 'acme' }))

    expect(authorizationOn('/v1/org/supervision/read')).toBe('Bearer pane-token')
  })

  test('a pane that is not a company pane still calls, token-less — no invented identity', async () => {
    const url = freshUrl()
    for (const key of environmentKeys) delete process.env[key]

    await reaching(chiefdPostJsonAsync(url, '/v1/org/supervision/read', { slug: 'acme' }))

    expect(requests).toHaveLength(1)
    expect(authorizationOn('/v1/org/supervision/read')).toBeUndefined()
  })

  test('the footer`s SSE reader is handed a credential that mints the same bearer', async () => {
    const url = freshUrl()
    const organizationDir = await paneHome()
    process.env.ORG_LAUNCHER_IDENTITY_DIR = join(organizationDir, '.chief')
    process.env.ORG_LAUNCHER_ORGANIZATION = 'acme'
    process.env.ORG_LAUNCHER_PERSON = 'ceo'
    process.env.ORG_LAUNCHER_ORG_DIR = organizationDir

    const bearer = teamUiSseBearer(url)
    expect(await bearer?.authHeader()).toEqual({ Authorization: 'Bearer pane-token' })
  })
})

describe('organization-intercom reaches chiefd with a bearer', () => {
  test('an org read carries the bearer — the acquirer the other two now share', async () => {
    const url = freshUrl()
    const organizationDir = await paneHome()

    await reaching(
      readDurableDocumentCached(
        {
          organizationDir,
          identityDir: join(organizationDir, '.chief'),
          organization: 'acme',
          personId: 'ceo',
          launcherRoot: '/tmp/launcher',
          chiefdUrl: url,
          companyKey: '0123456789ab'
        },
        'supervision'
      )
    )

    expect(authorizationOn('/v1/org/supervision/read')).toBe('Bearer pane-token')
  })

  test('the intercom`s SSE reader is handed a credential that mints the same bearer', async () => {
    const url = freshUrl()
    const organizationDir = await paneHome()

    const bearer = organizationSseBearer(
      {
        organizationDir,
        identityDir: join(organizationDir, '.chief'),
        organization: 'acme',
        personId: 'ceo',
        launcherRoot: '/tmp/launcher',
        chiefdUrl: url,
        companyKey: '0123456789ab'
      },
      undefined
    )
    expect(await bearer?.authHeader()).toEqual({ Authorization: 'Bearer pane-token' })
  })

  test('a context with no address mints nothing rather than guessing one', async () => {
    const organizationDir = await paneHome()
    expect(
      organizationSseBearer(
        {
          organizationDir,
          identityDir: join(organizationDir, '.chief'),
          organization: 'acme',
          personId: 'ceo',
          launcherRoot: '/tmp/launcher'
        },
        undefined
      )
    ).toBeUndefined()
  })
})

/* eslint-enable lucy/no-process-env, lucy/no-raw-null-check */
