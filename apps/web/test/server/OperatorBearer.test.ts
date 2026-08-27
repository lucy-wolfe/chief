// This server presents an operator bearer to chiefd — acceptance criterion 5
// of the design record: every caller class is proved to arrive with a
// credential BY A TEST, never by inspection.
//
// Before A2 the acquirer existed (`helpers/OperatorChallenge.ts`) and both
// server-side call sites skipped it: `CompanyChiefd.ts` built a `ChiefdClient`
// with no auth hooks, and `CompanyFeed.ts` opened chiefd's change feed with
// `{ accept: 'text/event-stream' }` and nothing else. Both reached chiefd
// anonymously, on every route this app serves.
//
// So each test below asserts on the WIRE — the `authorization` header the
// stand-in daemon actually received — rather than on this server's own belief
// about what it sent.
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import {
  authChallengeMessage,
  generateAgentKeypair,
  operatorKeyPath,
  verifyAuthChallenge
} from '@chief/chiefing'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { companyChiefd, companyChiefdEndpoint } from '@/server/CompanyChiefd'
import { companyFeed } from '@/server/CompanyFeed'
import { operatorAuth } from '@/server/OperatorBearer'
import type { FetchImpl } from '@/types/Fetch'

/* eslint-disable lucy/no-process-env */
// The route handlers under test read the real `process.env` through
// `common/Env.ts`, so exercising them requires setting this process's `HOME`.
function readHome(): string | undefined {
  return process.env.HOME
}
function writeHome(value: string | undefined): void {
  if (typeof value === 'string') process.env.HOME = value
  else delete process.env.HOME
}
/* eslint-enable lucy/no-process-env */

/** A distinct daemon address per test, so the module-level token cache — one
 * bearer per chiefd URL, because a token is only good at the daemon that
 * minted it — cannot leak an acquisition from one test into another. */
let nextPort = 40000
function daemonUrl(): string {
  nextPort += 1
  return `http://127.0.0.1:${nextPort}`
}

/** The company this fixture's beacond registers. `apps/web` addresses it by
 * KEY and signs with the operator key inside its own DIRECTORY, so the
 * fixture has to carry both. */
const COMPANY_KEY = '0123456789ab'

/** beacond's `/v1/list` answer, written as the WIRE.
 *
 * `/v1/list` and not `/v1/lookup`: the registry's lookup takes the company's
 * DIRECTORY, and a server rendering companies for an operator does not stand
 * in one. It matches the key on the list it already reads. */
function listBody(url: string, dir: string): string {
  return [
    '{"companies":[{',
    `"dir":"${dir}","key":"${COMPANY_KEY}","slug":"cobalt",`,
    '"registeredAt":"2026-08-13T00:00:00.000Z",',
    `"url":"${url}","port":1,"pid":11,`,
    '"hostname":"fixture","lastSeenAt":"2026-08-13T00:00:00.000Z"}]}'
  ].join('')
}

interface Daemon {
  fetchImpl: FetchImpl
  /** The `authorization` header of every NON-auth request, in order. */
  seen: (string | undefined)[]
  /** How many bearers were minted. */
  minted: () => number
}

/**
 * beacond plus one company daemon, in one `fetch`.
 *
 * The signature is VERIFIED with chiefing's own `verifyAuthChallenge` rather
 * than accepted: a stub that minted a token for any body would prove the header
 * arrived while proving nothing about what it contains.
 */
function fakeCompany(options: {
  url: string
  /** The company's directory — where its own `operator.key` lives. */
  dir: string
  publicSpkiBase64: string
  /** Refuse this many protected requests with 401 before answering. */
  refusals?: number
}): Daemon {
  const seen: (string | undefined)[] = []
  const nonces = new Map<string, { identityId: string; nonce: string }>()
  let counter = 0
  let refusals = options.refusals ?? 0

  const fetchImpl: FetchImpl = async (input, init) => {
    const url = new URL(input instanceof Request ? input.url : String(input))
    const headers = new Headers(init?.headers ?? {})

    if (url.pathname === '/v1/list') {
      return new Response(listBody(options.url, options.dir), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      })
    }

    if (url.pathname === '/v1/auth/challenge') {
      counter += 1
      const nonceId = `n-${counter}`
      const nonce = `fixture-nonce-${counter}`
      nonces.set(nonceId, { identityId: 'operator', nonce })
      return Response.json({ nonceId, nonce })
    }

    if (url.pathname === '/v1/auth/token') {
      const body: unknown = JSON.parse(String(init?.body ?? '{}'))
      const nonceId =
        body && typeof body === 'object' && 'nonceId' in body ? String(body.nonceId) : ''
      const signature =
        body && typeof body === 'object' && 'signature' in body ? String(body.signature) : ''
      const pending = nonces.get(nonceId)
      if (!pending) return new Response('challenge not satisfied', { status: 401 })
      const message = authChallengeMessage(pending.identityId, pending.nonce)
      if (!verifyAuthChallenge(message, signature, options.publicSpkiBase64)) {
        return new Response('challenge not satisfied', { status: 401 })
      }
      nonces.delete(nonceId)
      return Response.json({ token: `jwt-${counter}` })
    }

    seen.push(headers.get('authorization') ?? undefined)
    if (refusals > 0) {
      refusals -= 1
      return new Response('missing bearer token', { status: 401 })
    }
    if (url.pathname === '/v1/docs/watch') {
      return new Response(new ReadableStream<Uint8Array>({ start: (c) => c.close() }), {
        status: 200,
        headers: { 'content-type': 'text/event-stream' }
      })
    }
    return new Response('{}', { status: 200, headers: { 'content-type': 'application/json' } })
  }

  return { fetchImpl, seen, minted: () => counter }
}

describe('the apps/web server presents an operator bearer to chiefd', () => {
  let savedHome: string | undefined
  let home: string
  let companyDir: string
  let publicSpkiBase64: string

  beforeEach(() => {
    savedHome = readHome()
    const keypair = generateAgentKeypair()
    publicSpkiBase64 = keypair.publicSpkiBase64
    home = mkdtempSync(join(tmpdir(), 'chief-web-bearer-'))
    // Staged exactly as the daemon mints it at boot: 0600, inside the
    // COMPANY's own directory, with nothing configuring it. There is no
    // box-wide operator key any more — the key belongs to the company whose
    // daemon minted it.
    companyDir = join(home, 'cobalt')
    mkdirSync(join(companyDir, '.chief', 'keys'), { recursive: true })
    const keyPath = operatorKeyPath(join(companyDir, '.chief'))
    writeFileSync(keyPath, keypair.privatePkcs8Pem, { mode: 0o600 })
    chmodSync(keyPath, 0o600)
    writeHome(home)
  })

  afterEach(() => {
    writeHome(savedHome)
    vi.unstubAllGlobals()
    rmSync(home, { recursive: true, force: true })
  })

  it('puts a bearer on every request the server-side ChiefdClient sends', async () => {
    const url = daemonUrl()
    const daemon = fakeCompany({ url, dir: companyDir, publicSpkiBase64 })
    vi.stubGlobal('fetch', daemon.fetchImpl)

    const client = await companyChiefd(COMPANY_KEY)
    await client.docs.ensureSchema()

    expect(daemon.seen).toEqual(['Bearer jwt-1'])
  })

  it('acquires one bearer per daemon and reuses it', async () => {
    // A token minted per request would mean a challenge and a signature on
    // every route this app serves.
    const url = daemonUrl()
    const daemon = fakeCompany({ url, dir: companyDir, publicSpkiBase64 })
    vi.stubGlobal('fetch', daemon.fetchImpl)

    const client = await companyChiefd(COMPANY_KEY)
    await client.docs.ensureSchema()
    await client.docs.ensureSchema()
    await client.docs.ensureSchema()

    expect(daemon.minted()).toBe(1)
    expect(daemon.seen).toEqual(['Bearer jwt-1', 'Bearer jwt-1', 'Bearer jwt-1'])
  })

  it('re-acquires once when chiefd refuses the cached bearer', async () => {
    // chiefd's HS256 signing secret is ephemeral unless a secret file was
    // provisioned, so a restart rotates it and invalidates every cached bearer
    // at once. Without `authInvalidate` this server would 401 every call until
    // the process was restarted — each side assuming the other recovered.
    const url = daemonUrl()
    const daemon = fakeCompany({ url, dir: companyDir, publicSpkiBase64, refusals: 1 })
    vi.stubGlobal('fetch', daemon.fetchImpl)

    const client = await companyChiefd(COMPANY_KEY)
    await client.docs.ensureSchema()

    expect(daemon.seen).toEqual(['Bearer jwt-1', 'Bearer jwt-2'])
  })

  it('puts a bearer on the SSE proxy’s upstream change feed', async () => {
    // The one caller that cannot use `ChiefdClient`: an event stream must be
    // forwarded as bytes, so it holds a raw `fetch` and attaches the header
    // itself. `/v1/docs/watch` is a disclosure route on the company's whole
    // durable state and it was opened with `{ accept }` and nothing else.
    const url = daemonUrl()
    const daemon = fakeCompany({ url, dir: companyDir, publicSpkiBase64 })
    vi.stubGlobal('fetch', daemon.fetchImpl)

    const endpoint = await companyChiefdEndpoint(COMPANY_KEY)
    await companyFeed({
      endpoint,
      companyKey: COMPANY_KEY,
      stores: ['organization'],
      signal: new AbortController().signal
    })

    expect(daemon.seen).toEqual(['Bearer jwt-1'])
  })

  it('sends no header, rather than failing, when this box has no operator key', async () => {
    // The daemon mints the key at boot, so a box that has never run one
    // legitimately has none. chiefd is the authority on whether the call may
    // proceed; this server refusing on its own behalf would turn a missing file
    // into a 500 on every route.
    rmSync(join(companyDir, '.chief'), { recursive: true, force: true })
    const url = daemonUrl()
    const daemon = fakeCompany({ url, dir: companyDir, publicSpkiBase64 })
    vi.stubGlobal('fetch', daemon.fetchImpl)

    const client = await companyChiefd(COMPANY_KEY)
    await client.docs.ensureSchema()

    expect(daemon.seen).toEqual([undefined])
    expect(daemon.minted()).toBe(0)
  })

  it('does not replay one company’s bearer at another company', async () => {
    // Identities live in each company's own database and the HS256 secret is
    // that daemon's, so a token minted by one chiefd is only good there. A
    // single cached token would 401 every call at the second company and read
    // as a credential problem.
    const first = daemonUrl()
    const second = daemonUrl()
    const firstDaemon = fakeCompany({ url: first, dir: companyDir, publicSpkiBase64 })
    const secondDaemon = fakeCompany({ url: second, dir: companyDir, publicSpkiBase64 })

    vi.stubGlobal('fetch', firstDaemon.fetchImpl)
    await (await companyChiefd(COMPANY_KEY)).docs.ensureSchema()

    vi.stubGlobal('fetch', secondDaemon.fetchImpl)
    await (await companyChiefd(COMPANY_KEY)).docs.ensureSchema()

    expect(firstDaemon.minted()).toBe(1)
    expect(secondDaemon.minted()).toBe(1)

    // And each cache entry is independently droppable, which is what makes one
    // restarted daemon recoverable without disturbing the other.
    operatorAuth(first, companyDir).authInvalidate()
    vi.stubGlobal('fetch', firstDaemon.fetchImpl)
    await (await companyChiefd(COMPANY_KEY)).docs.ensureSchema()
    expect(firstDaemon.minted()).toBe(2)
    expect(secondDaemon.minted()).toBe(1)
  })
})
