/**
 * TWO COMPANIES IN ONE PROCESS REACH TWO DAEMONS.
 *
 * # The defect, in two generations
 *
 * `organization-intercom.ts` first resolved its chiefd base URL from one
 * process-global environment variable, `ORG_CHIEFD_URL`, stamped into a pane
 * by the chiefd that spawned it. One value per PROCESS is correct for exactly
 * one deployment — one Pi process per tmux pane, one company per process — and
 * has no correct value at all in `apps/web`, where one server process serves
 * many companies. Every `org_*` tool call from the browser reached whichever
 * company that variable happened to name at boot.
 *
 * #983 replaced it with a beacond lookup BY SLUG, which fixed that and left a
 * subtler version of the same thing: a slug is not an identity. Two
 * directories may hold companies with the same display word and the registry
 * had one answer for the word.
 *
 * An install's cwd IS its company directory, and a directory knows where its
 * own daemon is. `resolveOrganizationRuntimeContext` reads
 * `<dir>/.chief/run/daemon.json` — written by that directory's own daemon,
 * carrying both the URL it bound and the company key it serves. No registry is
 * on the path.
 *
 * # Why the negative assertions carry the weight
 *
 * The failure is SILENT. A wrong daemon ANSWERS: it does not refuse, it does
 * not 500, it does not time out. So "the call succeeded" proves nothing about
 * where it landed. Every test below that proves company A was reached also
 * proves company B was NOT — delete the second half and the file goes green
 * against the very defect it exists for.
 *
 * # What is real here
 *
 * The production resolver (`resolveOrganizationRuntimeContext`), real
 * rendezvous files on disk written exactly as the daemon writes them, the
 * production transport, and two real HTTP servers standing in for two chiefd
 * daemons. Nothing is faked: there is no registry left to fake.
 */
import { createHash } from 'node:crypto'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import type { Server } from 'node:http'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { isNullish } from '@test/support/Nullish'
import {
  OrgChiefdUrlUnsetError,
  readDurableDocumentCached,
  readOrganizationRuntimeContext,
  resolveOrganizationRuntimeContext
} from '@test-assets/organization-intercom'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

function isRecord(value: unknown): value is Record<string, unknown> {
  return !isNullish(value) && typeof value === 'object' && !Array.isArray(value)
}

interface FakeDaemon {
  readonly url: string
  readonly requests: Array<{ path: string; slug: unknown }>
  stop(): Promise<void>
}

/**
 * A chiefd stand-in that records every org route it is asked for.
 *
 * It answers a supervision read with an empty ledger, which is enough for the
 * call to complete — the assertion is never on the BODY, it is on which
 * server received the request at all.
 */
async function startFakeDaemon(): Promise<FakeDaemon> {
  const requests: Array<{ path: string; slug: unknown }> = []
  const server: Server = createServer((request, response) => {
    const chunks: Buffer[] = []
    request.on('data', (chunk: Buffer) => chunks.push(chunk))
    request.on('end', () => {
      let slug: unknown
      try {
        const body: unknown = JSON.parse(Buffer.concat(chunks).toString('utf8'))
        slug = isRecord(body) ? body.slug : undefined
      } catch {
        slug = undefined
      }
      requests.push({ path: request.url ?? '', slug })
      response.writeHead(200, { 'content-type': 'application/json' })
      /* eslint-disable lucy/no-json-stringify */
      // The replacement helper is private to a sibling repo and is not a
      // dependency here.
      response.end(JSON.stringify({ found: true, ledger: JSON.stringify({}), seq: 1 }))
      /* eslint-enable lucy/no-json-stringify */
    })
  })
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (typeof address === 'string' || isNullish(address)) {
    throw new Error('the fake daemon did not bind a port')
  }
  return {
    url: `http://127.0.0.1:${address.port}`,
    requests,
    stop: () => new Promise<void>((resolve) => server.close(() => resolve()))
  }
}

/** `sha256(<dir>)[..12]`, as the daemon derives it before publishing. */
function companyKeyFor(dir: string): string {
  return createHash('sha256').update(dir).digest('hex').slice(0, 12)
}

/** Write a company directory's rendezvous exactly as `chiefd` publishes it. */
function publishRendezvous(dir: string, url: string, recordedDir = dir): void {
  mkdirSync(join(dir, '.chief', 'run'), { recursive: true })
  /* eslint-disable lucy/no-json-stringify */
  // The replacement helper is private to a sibling repo and is not a
  // dependency here.
  writeFileSync(
    join(dir, '.chief', 'run', 'daemon.json'),
    JSON.stringify({ dir: recordedDir, key: companyKeyFor(recordedDir), url, pid: process.pid })
  )
  /* eslint-enable lucy/no-json-stringify */
}

/** An install's environment. `ORG_LAUNCHER_ORG_DIR` is the COMPANY DIRECTORY. */
function environmentFor(dir: string): Record<string, string | undefined> {
  return {
    ORG_LAUNCHER_IDENTITY_DIR: `${dir}/.chief`,
    ORG_LAUNCHER_ORG_DIR: dir,
    ORG_LAUNCHER_ORGANIZATION: 'acme',
    ORG_LAUNCHER_PERSON: 'ceo',
    ORG_LAUNCHER_ROOT: '/tmp/org-daemon-resolution/launcher'
  }
}

let alphaDaemon: FakeDaemon
let betaDaemon: FakeDaemon
let root: string
/** TWO DIRECTORIES, ONE DISPLAY WORD — the pair a slug-keyed registry could
 * not tell apart. */
let alphaDir: string
let betaDir: string
/** Created but never started: the state that must refuse, never guess. */
let stoppedDir: string

beforeAll(async () => {
  alphaDaemon = await startFakeDaemon()
  betaDaemon = await startFakeDaemon()
  root = mkdtempSync(join(tmpdir(), 'org-daemon-resolution-'))
  alphaDir = join(root, 'one', 'acme')
  betaDir = join(root, 'two', 'acme')
  stoppedDir = join(root, 'three', 'acme')
  mkdirSync(stoppedDir, { recursive: true })
  publishRendezvous(alphaDir, alphaDaemon.url)
  publishRendezvous(betaDir, betaDaemon.url)
})

afterAll(async () => {
  await alphaDaemon?.stop()
  await betaDaemon?.stop()
  rmSync(root, { recursive: true, force: true })
})

describe('a company resolves its own daemon, per company, from its own directory', () => {
  it('two same-named companies resolved in ONE process get two different daemons', () => {
    const alpha = resolveOrganizationRuntimeContext(environmentFor(alphaDir))
    const beta = resolveOrganizationRuntimeContext(environmentFor(betaDir))

    expect(alpha.chiefdUrl).toBe(alphaDaemon.url)
    expect(beta.chiefdUrl).toBe(betaDaemon.url)
    // The assertion neither predecessor could satisfy: one process, two
    // companies with the SAME display word, two answers.
    expect(alpha.chiefdUrl).not.toBe(beta.chiefdUrl)
    expect(alpha.companyKey).not.toBe(beta.companyKey)
  })

  it('the key it carries is the SERVED one, not one this extension derived', () => {
    // The whole point of publishing the key: one producer. A resolver that
    // recomputed it would agree here and drift the first time the derivation
    // changed on one side only.
    const alpha = resolveOrganizationRuntimeContext(environmentFor(alphaDir))
    expect(alpha.companyKey).toBe(companyKeyFor(alphaDir))
    expect(alpha.companyKey).toMatch(/^[0-9a-f]{12}$/)
  })

  it('a call for company A reaches A’s daemon and never B’s', async () => {
    const alphaBefore = alphaDaemon.requests.length
    const betaBefore = betaDaemon.requests.length

    const alpha = resolveOrganizationRuntimeContext(environmentFor(alphaDir))
    await readDurableDocumentCached(alpha, 'supervision')

    // POSITIVE: A's daemon was asked, and asked under A's own key.
    expect(alphaDaemon.requests.length).toBe(alphaBefore + 1)
    expect(alphaDaemon.requests.at(-1)?.path).toBe('/v1/org/supervision/read')
    expect(alphaDaemon.requests.at(-1)?.slug).toBe(companyKeyFor(alphaDir))
    // NEGATIVE, and this is the half that means anything: B never heard about
    // it. A wrong daemon answers 200, so the success above is not evidence of
    // where the read went — only B's silence is.
    expect(betaDaemon.requests.length).toBe(betaBefore)
  })

  it('and the reverse, because a one-way test passes against "everything reaches one"', async () => {
    const alphaBefore = alphaDaemon.requests.length
    const betaBefore = betaDaemon.requests.length

    const beta = resolveOrganizationRuntimeContext(environmentFor(betaDir))
    await readDurableDocumentCached(beta, 'supervision')

    expect(betaDaemon.requests.length).toBe(betaBefore + 1)
    expect(betaDaemon.requests.at(-1)?.slug).toBe(companyKeyFor(betaDir))
    expect(alphaDaemon.requests.length).toBe(alphaBefore)
  })

  it('the two companies interleaved keep their own daemons, call for call', async () => {
    // Resolved once each, then used alternately — the shape a hosted server
    // actually has. An implementation that pinned every call to whichever
    // company resolved LAST passes both single-company tests above and fails
    // here.
    const alpha = resolveOrganizationRuntimeContext(environmentFor(alphaDir))
    const beta = resolveOrganizationRuntimeContext(environmentFor(betaDir))
    const alphaBefore = alphaDaemon.requests.length
    const betaBefore = betaDaemon.requests.length

    await readDurableDocumentCached(alpha, 'activity')
    await readDurableDocumentCached(beta, 'activity')
    await readDurableDocumentCached(alpha, 'session-maintenance')

    expect(alphaDaemon.requests.length).toBe(alphaBefore + 2)
    expect(betaDaemon.requests.length).toBe(betaBefore + 1)
    expect(betaDaemon.requests.at(-1)?.path).toBe('/v1/org/activity/read')
  })
})

describe('the ambient process cannot steer a call any more', () => {
  /* eslint-disable lucy/no-process-env */
  // THE REGRESSION, staged deliberately: the process variable names the WRONG
  // company for the duration of the call. Under the old call-time read this is
  // the state a shared host is permanently in, and the read below would have
  // landed on the other company's daemon. Written against `process.env`
  // directly because `process.env` IS this test's subject — there is no
  // indirection to import, and the extension is a self-contained copied file
  // with no injectable env seam for it.
  const previous = process.env.ORG_CHIEFD_URL

  afterAll(() => {
    if (isNullish(previous)) delete process.env.ORG_CHIEFD_URL
    else process.env.ORG_CHIEFD_URL = previous
  })

  it('an org_* read for B lands on B while ORG_CHIEFD_URL names A', async () => {
    const beta = resolveOrganizationRuntimeContext(environmentFor(betaDir))
    const alphaBefore = alphaDaemon.requests.length
    const betaBefore = betaDaemon.requests.length

    process.env.ORG_CHIEFD_URL = alphaDaemon.url
    try {
      await readDurableDocumentCached(beta, 'supervision')
    } finally {
      delete process.env.ORG_CHIEFD_URL
    }

    expect(betaDaemon.requests.length).toBe(betaBefore + 1)
    expect(alphaDaemon.requests.length).toBe(alphaBefore)
  })

  it('and the variable cannot SUPPLY an address either: a parsed-only context refuses', async () => {
    // The other direction of the same rule. With the variable set to a
    // perfectly reachable daemon, a context that was parsed but never resolved
    // must still refuse rather than pick the ambient value up.
    process.env.ORG_CHIEFD_URL = alphaDaemon.url
    try {
      const parsed = readOrganizationRuntimeContext(environmentFor(alphaDir))
      expect(parsed.chiefdUrl).toBe(undefined)
      expect(parsed.companyKey).toBe(undefined)
      await expect(readDurableDocumentCached(parsed, 'supervision')).rejects.toBeInstanceOf(
        OrgChiefdUrlUnsetError
      )
    } finally {
      delete process.env.ORG_CHIEFD_URL
    }
  })
  /* eslint-enable lucy/no-process-env */
})

describe('an unresolvable company is a refusal, never a guessed address', () => {
  it('a company whose daemon has never published carries no address and refuses on use', async () => {
    // "Boot it" is the answer, and it is not this extension's to invent. It
    // arrives as an absent rendezvous rather than as a registry miss, because
    // there is no registry on this path — the two states a lookup used to
    // distinguish (unknown company / not running) are one local fact here:
    // nothing has published in this directory.
    const context = resolveOrganizationRuntimeContext(environmentFor(stoppedDir))
    expect(context.chiefdUrl).toBe(undefined)
    expect(context.companyKey).toBe(undefined)
    await expect(readDurableDocumentCached(context, 'supervision')).rejects.toBeInstanceOf(
      OrgChiefdUrlUnsetError
    )
  })

  it('a rendezvous copied from another directory is refused, never followed', () => {
    // THE COPIED-PROJECT CASE. `.chief/` lives inside the company directory,
    // so copying a project copies its rendezvous — and the copy still names
    // the ORIGINAL directory. Following it would point the new directory's
    // install at the old directory's daemon, which answers, commits, and
    // returns 200.
    const copyDir = join(root, 'copied', 'acme')
    publishRendezvous(copyDir, alphaDaemon.url, alphaDir)
    expect(() => resolveOrganizationRuntimeContext(environmentFor(copyDir))).toThrow(/describes/)
  })
})
