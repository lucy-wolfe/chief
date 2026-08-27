// A4: the ONE pane-side acquirer. Every in-pane caller resolves its bearer
// through this, so a pane holds one token cache per (daemon, person, key)
// rather than one per extension that happened to remember to authenticate.
//
// These are behaviour tests against a stubbed daemon, not shape tests: the key
// is a real P-256 key on disk at a real mode, the challenge is really signed,
// and the assertion is the header that reaches the wire.
import { chmodSync, mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { IDENTITY_KEY_FILENAME, loadOrCreateAgentKeypair } from '@/resources/Identity'
import { paneChiefdTransport, paneTokenManager } from '@/resources/PaneIdentity'
import type { AgentKeyRefusal, PaneIdentity } from '@/types/Auth'

interface CapturedRequest {
  url: string
  headers: Record<string, string>
}

const realFetch = globalThis.fetch
const requests: CapturedRequest[] = []
let nextUrl = 0

/** A distinct base URL per test: the acquirer caches one manager per (url,
 * person, key) for the life of the process, so two tests sharing a URL would
 * share a token and the second one would prove nothing. */
function freshUrl(): string {
  nextUrl += 1
  return `http://pane-identity-${nextUrl}.test`
}

/**
 * A pane with a real, correctly-moded identity key in its own agent directory.
 *
 * The path is spelled out rather than built with `personPiHome`, deliberately:
 * a fixture that composed the location with the same function the code under
 * test uses would agree with it wherever it pointed, which is precisely how the
 * `.chief` segment went missing here unnoticed.
 */
async function paneWithKey(url: string, mode = 0o600): Promise<PaneIdentity> {
  const organizationDir = mkdtempSync(join(tmpdir(), 'chiefing-pane-identity-'))
  const personId = 'ceo'
  const identityDir = join(organizationDir, '.chief', 'agent', personId)
  mkdirSync(identityDir, { recursive: true })
  await loadOrCreateAgentKeypair(identityDir)
  chmodSync(join(identityDir, IDENTITY_KEY_FILENAME), mode)
  return { url, personId, organizationDir, identityDir }
}

beforeEach(() => {
  requests.length = 0
  globalThis.fetch = (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input)
    const headers: Record<string, string> = {}
    for (const [name, value] of Object.entries(init?.headers ?? {})) {
      headers[name] = String(value)
    }
    requests.push({ url, headers })
    if (url.endsWith('/v1/auth/challenge')) {
      return Promise.resolve(new Response('{"nonceId":"n-1","nonce":"a-fixed-width-nonce"}'))
    }
    if (url.endsWith('/v1/auth/token')) {
      return Promise.resolve(new Response('{"token":"pane-token"}'))
    }
    return Promise.resolve(new Response('{"ok":true}'))
  }
})

afterEach(() => {
  globalThis.fetch = realFetch
})

describe('the pane transport presents the pane`s own credential', () => {
  it('signs the daemon challenge with the pi-home key and rides the minted bearer', async () => {
    const identity = await paneWithKey(freshUrl())
    await paneChiefdTransport(identity).post('/v1/org/supervision/read', { slug: 'acme' })
    const org = requests.find((request) => request.url.endsWith('/v1/org/supervision/read'))
    expect(org?.headers.Authorization).toBe('Bearer pane-token')
    // The handshake really happened rather than a header being assumed.
    expect(requests.map((request) => request.url.split('/v1/')[1])).toEqual([
      'auth/challenge',
      'auth/token',
      'org/supervision/read'
    ])
  })

  it('returns the SAME manager for one (daemon, person, key) triple — one token cache per pane', async () => {
    const identity = await paneWithKey(freshUrl())
    expect(paneTokenManager(identity)).toBe(paneTokenManager(identity))
  })

  it('a pane with no key calls token-less rather than inventing one', async () => {
    const organizationDir = mkdtempSync(join(tmpdir(), 'chiefing-pane-identity-'))
    const identityDir = join(organizationDir, '.chief', 'agent', 'ceo')
    const identity: PaneIdentity = {
      url: freshUrl(),
      personId: 'ceo',
      organizationDir,
      identityDir
    }
    await paneChiefdTransport(identity).post('/v1/org/supervision/read', { slug: 'acme' })
    expect(requests).toHaveLength(1)
    expect(requests[0]?.headers.Authorization).toBeUndefined()
  })

  /**
   * THE NEGATIVE CONTROL FOR THE `.chief` SEGMENT.
   *
   * The launch catalog carries the exact identity directory. A key in an old
   * reconstructed `people/<id>/pi-home` path must not satisfy that carried
   * answer.
   */
  it('a key one level too shallow is not this pane`s key — the `.chief` root is the location', async () => {
    const organizationDir = mkdtempSync(join(tmpdir(), 'chiefing-pane-identity-'))
    const shallow = join(organizationDir, 'people', 'ceo', 'pi-home')
    mkdirSync(shallow, { recursive: true })
    await loadOrCreateAgentKeypair(shallow)
    chmodSync(join(shallow, IDENTITY_KEY_FILENAME), 0o600)

    const identityDir = join(organizationDir, '.chief', 'agent', 'ceo')
    const identity: PaneIdentity = {
      url: freshUrl(),
      personId: 'ceo',
      organizationDir,
      identityDir
    }
    expect(paneTokenManager(identity)).toBeUndefined()
    await paneChiefdTransport(identity).post('/v1/org/supervision/read', { slug: 'acme' })
    expect(requests).toHaveLength(1)
    expect(requests[0]?.headers.Authorization).toBeUndefined()
  })
})

describe('a refused key is REPORTED, not swallowed', () => {
  it('reports a group- or world-readable key with the file and the exact mode', async () => {
    const refusals: AgentKeyRefusal[] = []
    const identity = await paneWithKey(freshUrl(), 0o644)
    const manager = paneTokenManager(identity, (refusal) => {
      refusals.push(refusal)
    })
    // No credential, exactly as before — the refusal is not a new outage.
    expect(manager).toBeUndefined()
    expect(refusals).toHaveLength(1)
    expect(refusals[0]?.reason).toBe('permissive-mode')
    expect(refusals[0]?.mode).toBe(0o644)
    expect(refusals[0]?.keyPath).toContain(IDENTITY_KEY_FILENAME)
  })

  // CLASSIFICATION, not policy. This pins that a missing key reaches the
  // reporter tagged `absent` rather than as some other reason — what the
  // reporter then DOES with it is `organization-intercom`'s rule, and that
  // rule is conditional: absent is benign only when nothing about the pane
  // claims a person. A pane running AS somebody that cannot find that
  // person's key is broken, and says so (`IdentityKeyRefusalIsReported`).
  it('classifies a missing key as `absent`, so the reporter can judge it', () => {
    const refusals: AgentKeyRefusal[] = []
    const organizationDir = mkdtempSync(join(tmpdir(), 'chiefing-pane-identity-'))
    const identityDir = join(organizationDir, '.chief')
    paneTokenManager(
      { url: freshUrl(), personId: 'chief', organizationDir, identityDir },
      (refusal) => {
        refusals.push(refusal)
      }
    )
    expect(refusals.map((refusal) => refusal.reason)).toEqual(['absent'])
  })

  it('never reports when the key is good — silence is the healthy signal', async () => {
    const refusals: AgentKeyRefusal[] = []
    const identity = await paneWithKey(freshUrl())
    paneTokenManager(identity, (refusal) => {
      refusals.push(refusal)
    })
    expect(refusals).toEqual([])
  })
})
