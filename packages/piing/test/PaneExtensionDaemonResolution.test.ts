/**
 * THE FOOTER REACHES ITS OWN COMPANY.
 *
 * # The defect, in two generations
 *
 * `team-ui.ts` first learned its chiefd base URL from one process-global
 * environment variable, `ORG_CHIEFD_URL`, stamped into a tmux pane by the
 * chiefd that spawned it. One value per PROCESS is correct for exactly one
 * deployment — one Pi process per pane, one company per process — and has no
 * correct value at all in a host that serves several companies from ONE
 * process.
 *
 * #983 replaced it with a beacond lookup BY SLUG, which fixed that and left a
 * subtler version of the same thing: a slug is not an identity. Two
 * directories may hold companies with the same display word, and the registry
 * had one answer for the word — so the second company's panes reached the
 * first company's daemon, silently.
 *
 * A pane's cwd IS its company directory, and a directory knows where its own
 * daemon is. `resolveTeamUiCompany` reads `<dir>/.chief/run/daemon.json` —
 * written by that directory's own daemon, carrying both the URL it bound and
 * the company key it serves. No registry is on the path at all.
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
 * The production resolver (`resolveTeamUiCompany`), real rendezvous files on
 * disk written exactly as the daemon writes them, the production transport,
 * and two real HTTP servers standing in for two chiefd daemons. Nothing is
 * faked: there is no registry left to fake.
 */
import { createHash } from 'node:crypto'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import type { Server } from 'node:http'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { isNullish } from '@test/support/Nullish'
import {
  OrgChiefdUrlUnsetError as TeamUiUnsetError,
  readFooterStoreDocument,
  resetFooterStoreDocumentCache,
  resolveTeamUiCompany
} from '@test-assets/team-ui'
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'vitest'

interface FakeDaemon {
  readonly url: string
  readonly requests: Array<{ path: string; slug: unknown }>
  stop(): Promise<void>
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !isNullish(value) && typeof value === 'object' && !Array.isArray(value)
}

/**
 * A chiefd stand-in that records every org route it is asked for.
 *
 * It answers the route this extension reads — `/v1/org/supervision/read`
 * (the footer) — with an empty
 * payload, which is enough for the call to complete. The assertion is never on
 * the BODY; it is on which server received the request at all.
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
      const path = request.url ?? ''
      requests.push({ path, slug })
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

/**
 * Write a company directory's rendezvous exactly as `chiefd` publishes it:
 * `<dir>/.chief/run/daemon.json`, camelCase, four fields and no others.
 */
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

/** A pane's environment. `ORG_LAUNCHER_ORG_DIR` is the COMPANY DIRECTORY —
 * the one the operator ran `chief` in, and this pane's own cwd. */
function environmentFor(dir: string): Record<string, string | undefined> {
  return {
    ORG_LAUNCHER_ORG_DIR: dir,
    ORG_LAUNCHER_ORGANIZATION: 'acme',
    ORG_LAUNCHER_PERSON: 'ceo',
    ORG_LAUNCHER_ROOT: '/tmp/pane-extension-daemon-resolution/launcher'
  }
}

let alphaDaemon: FakeDaemon
let betaDaemon: FakeDaemon
let root: string
/** TWO DIRECTORIES, ONE DISPLAY WORD. Both companies are called `acme` — the
 * pair the slug-keyed resolver could not tell apart at all. */
let alphaDir: string
let betaDir: string
/** Created but never started: the state that must answer "no address", never
 * guess one. */
let stoppedDir: string

beforeAll(async () => {
  alphaDaemon = await startFakeDaemon()
  betaDaemon = await startFakeDaemon()
  root = mkdtempSync(join(tmpdir(), 'pane-extension-daemon-'))
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

beforeEach(() => {
  // The footer's last-good store cache is module state shared by every install
  // in this process; a stale entry would let a test pass without a request
  // reaching either daemon.
  resetFooterStoreDocumentCache()
})

describe('team-ui resolves its own company’s daemon, per company', () => {
  it('two same-named companies resolved in ONE process get two different daemons', () => {
    const alpha = resolveTeamUiCompany(environmentFor(alphaDir))
    const beta = resolveTeamUiCompany(environmentFor(betaDir))

    expect(alpha?.url).toBe(alphaDaemon.url)
    expect(beta?.url).toBe(betaDaemon.url)
    // The assertion neither predecessor could satisfy: one process, two
    // companies with the SAME display word, two answers. A process-global
    // variable makes these equal by construction; a slug-keyed registry has
    // exactly one row for the word.
    expect(alpha?.url).not.toBe(beta?.url)
    expect(alpha?.key).not.toBe(beta?.key)
  })

  it('the key it carries is the served one, not one this extension derived', () => {
    // The whole point of publishing the key: one producer. This asserts the
    // value came off the FILE — a resolver that recomputed it from the
    // directory would agree here and drift the first time the derivation
    // changed on one side only.
    publishRendezvous(alphaDir, alphaDaemon.url)
    const resolved = resolveTeamUiCompany(environmentFor(alphaDir))
    expect(resolved?.key).toBe(companyKeyFor(alphaDir))
    expect(resolved?.key).toMatch(/^[0-9a-f]{12}$/)
  })

  it('a footer read for company A reaches A’s daemon and never B’s', async () => {
    const alphaBefore = alphaDaemon.requests.length
    const betaBefore = betaDaemon.requests.length

    const alpha = resolveTeamUiCompany(environmentFor(alphaDir))
    await readFooterStoreDocument(alpha?.url, alpha?.key ?? '', 'supervision', undefined)

    expect(alphaDaemon.requests.length).toBe(alphaBefore + 1)
    expect(alphaDaemon.requests.at(-1)?.path).toBe('/v1/org/supervision/read')
    // And it carried THAT company's key, which is what chiefd resolves by.
    expect(alphaDaemon.requests.at(-1)?.slug).toBe(companyKeyFor(alphaDir))
    // NEGATIVE, and this is the half that means anything: B never heard about
    // it. A wrong daemon answers 200, so the success above is not evidence of
    // where the read went — only B's silence is.
    expect(betaDaemon.requests.length).toBe(betaBefore)
  })

  it('and the reverse, because a one-way test passes against "everything reaches one"', async () => {
    const alphaBefore = alphaDaemon.requests.length
    const betaBefore = betaDaemon.requests.length

    const beta = resolveTeamUiCompany(environmentFor(betaDir))
    await readFooterStoreDocument(beta?.url, beta?.key ?? '', 'supervision', undefined)

    expect(betaDaemon.requests.length).toBe(betaBefore + 1)
    expect(betaDaemon.requests.at(-1)?.slug).toBe(companyKeyFor(betaDir))
    expect(alphaDaemon.requests.length).toBe(alphaBefore)
  })

  it('the two companies interleaved keep their own daemons, call for call', async () => {
    // Resolved once each, then used alternately — the shape a hosted server
    // actually has. An implementation that pinned every read to whichever
    // company resolved LAST passes both single-company tests above and fails
    // here.
    const alpha = resolveTeamUiCompany(environmentFor(alphaDir))
    const beta = resolveTeamUiCompany(environmentFor(betaDir))
    const alphaBefore = alphaDaemon.requests.length
    const betaBefore = betaDaemon.requests.length

    await readFooterStoreDocument(alpha?.url, alpha?.key ?? '', 'supervision', undefined)
    await readFooterStoreDocument(beta?.url, beta?.key ?? '', 'supervision', undefined)
    await readFooterStoreDocument(alpha?.url, alpha?.key ?? '', 'activity', undefined)

    expect(alphaDaemon.requests.length).toBe(alphaBefore + 2)
    expect(betaDaemon.requests.length).toBe(betaBefore + 1)
    expect(betaDaemon.requests.at(-1)?.path).toBe('/v1/org/supervision/read')
    expect(alphaDaemon.requests.at(-1)?.path).toBe('/v1/org/activity/read')
  })

  it('a plain (non-organization) pane has no company, so it resolves to no address at all', async () => {
    // Not a fallback — the absence of a subject. A plain pane reads no durable
    // state, and any read attempted from that state must refuse.
    expect(resolveTeamUiCompany({})).toBe(undefined)
    await expect(
      readFooterStoreDocument(undefined, '0123456789ab', 'supervision', undefined)
    ).rejects.toBeInstanceOf(TeamUiUnsetError)
  })

  it('a company that has never been started resolves to no address, never a guess', () => {
    // The directory exists and holds a company; no daemon has published there.
    // "Boot it" is the answer, and it is not this extension's to invent.
    expect(resolveTeamUiCompany(environmentFor(stoppedDir))).toBe(undefined)
  })
})

describe('the ambient process cannot steer the extension any more', () => {
  /* eslint-disable lucy/no-process-env */
  // THE REGRESSION, staged deliberately: the process variable names the WRONG
  // company for the duration of the call. Under the old call-time read this is
  // the state a shared host is permanently in, and the reads below would have
  // landed on the other company's daemon. Written against `process.env`
  // directly because `process.env` IS this block's subject — there is no
  // indirection to import, and both extensions are self-contained copied files.
  const previous = process.env.ORG_CHIEFD_URL

  afterAll(() => {
    if (isNullish(previous)) delete process.env.ORG_CHIEFD_URL
    else process.env.ORG_CHIEFD_URL = previous
  })

  it('a footer read for B lands on B while ORG_CHIEFD_URL names A', async () => {
    const beta = resolveTeamUiCompany(environmentFor(betaDir))
    const alphaBefore = alphaDaemon.requests.length
    const betaBefore = betaDaemon.requests.length

    process.env.ORG_CHIEFD_URL = alphaDaemon.url
    try {
      await readFooterStoreDocument(beta?.url, beta?.key ?? '', 'supervision', undefined)
    } finally {
      delete process.env.ORG_CHIEFD_URL
    }

    expect(betaDaemon.requests.length).toBe(betaBefore + 1)
    expect(alphaDaemon.requests.length).toBe(alphaBefore)
  })

  it('and the same for the footer: an address-less read refuses beside a reachable daemon', async () => {
    process.env.ORG_CHIEFD_URL = alphaDaemon.url
    try {
      await expect(
        readFooterStoreDocument(undefined, '0123456789ab', 'supervision', undefined)
      ).rejects.toBeInstanceOf(TeamUiUnsetError)
    } finally {
      delete process.env.ORG_CHIEFD_URL
    }
  })
  /* eslint-enable lucy/no-process-env */
})

describe('a rendezvous that describes another directory is refused, never followed', () => {
  /**
   * THE COPIED-PROJECT CASE, and the reason the file carries its own directory.
   *
   * `.chief/` lives INSIDE the company directory, so copying a project copies
   * its rendezvous — and the copy still names the ORIGINAL directory. Following
   * it would point the new directory's pane at the old directory's daemon,
   * which answers, commits, and returns 200: the exact silent split-brain the
   * composite key existed to prevent.
   */
  it('throws rather than binding the origin company’s daemon', () => {
    const copyDir = join(root, 'copied', 'acme')
    // Alpha's rendezvous, byte for byte, in a directory that is not alpha.
    publishRendezvous(copyDir, alphaDaemon.url, alphaDir)
    expect(() => resolveTeamUiCompany(environmentFor(copyDir))).toThrow(/describes/)
  })
})
