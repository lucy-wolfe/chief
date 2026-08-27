/**
 * A REMINDER A WEB-HOSTED PERSON ARMED ACTUALLY FIRES, AND WAKES ITS OWNER.
 *
 * THE DEFECT THIS PINS: a hosted person had every tool and no lifecycle. The
 * extensions' `on(...)` registrations went into an empty function and the
 * intercom was installed with `pollIntervalMs: 0`. That option does not mean
 * "do not poll" — since #827 there is no poll floor to disable; `0` is a test
 * seam meaning "construct NO `SseWatcher`". So the wake path was switched off
 * entirely, and a frame that did arrive had no handler to reach.
 *
 * # What is real here, and what is not
 *
 * REAL: `installOrganizationIntercom` and the two other hosted extensions,
 * their registered handlers, the whole mailbox drain, the production
 * `SseWatcher` reading a real `text/event-stream` over a real socket, and the
 * driver under test. FAKE: one loopback server standing in for this company's
 * chiefd, and a recorder standing in for `AgentHarness` and `Session`, neither
 * of which can be constructed without a provider route.
 *
 * The daemon is INPUT. It decides what is in the mailbox and when a
 * `doc-change` arrives; it is never the subject.
 */
import { createHash } from 'node:crypto'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import type { Server, ServerResponse } from 'node:http'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { activeSseHubCount } from '@chief/chiefing'
import { harnessStub, subjectFor } from '@test/harness/HostedPersonStubs'
import type { HarnessStubOptions } from '@test/types/HostedPersonStubs'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { selectTools } from '@/server/AgentTools'
import {
  driveLifecycle,
  DRIVEN_HOOKS,
  REFUSED_HOOKS,
  unclassifiedHookReason
} from '@/server/ExtensionLifecycle'
import { HOSTED_EXTENSIONS, installExtensions } from '@/server/ExtensionTools'
import type { AgentProfile } from '@/types/AgentHost'
import type { HostedContextUsage } from '@/types/ContextUsage'
import type {
  HostedLifecycle,
  SessionReplacementRequest,
  SessionReplacer
} from '@/types/ExtensionLifecycle'
import { isNullish } from '@/utils/Nullish'

const SLUG = 'remindco'
/** The person the reminder NAMES. chiefd routes `person_reminder` on
 *  `payload.personId`, which is the owner and not always the arming manager. */
const OWNER = 'researcher'
/** The manager who armed it — the CEO, so the hosted set under test is the
 *  full one, including anything that installs for `ceo` alone.
 *  `dispatch::recipients_for` must never route the wake here, and this host
 *  must never deliver it here either. */
const MANAGER = 'ceo'
const ROOT = '/tmp/web-hosted-reactive-lifecycle'
const REMINDER_BODY = '[reminder] Check whether the pilot batch finished.'

/** One live `/v1/docs/watch` reader, as the stub daemon holds it. */
interface WatchStream {
  readonly stores: readonly string[]
  readonly response: ServerResponse
  closed: boolean
}

interface StubChiefd {
  readonly url: string
  readonly watchers: readonly WatchStream[]
  /** Put one envelope in a person's mailbox. Nothing is delivered by this
   *  alone: chiefd's own wake is the separate `doc-change` below. */
  stage(personId: string, envelope: Record<string, unknown>): void
  /** Publish the `doc-change` a committed mailbox row write publishes.
   *  `mailbox_store_name` is `mailbox/<personId>`, verbatim. */
  publishMailboxChange(personId: string): void
  stop(): Promise<void>
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && !isNullish(value) && !Array.isArray(value)
}

/** A one-department company whose two people are a manager and their report. */
function manifest(): Record<string, unknown> {
  const createdAt = '2026-01-01T00:00:00.000Z'
  const person = (id: string, title: string, kind: string): Record<string, unknown> => ({
    id,
    name: id,
    title,
    kind,
    departmentId: 'executive',
    employmentState: 'active',
    createdAt
  })
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: SLUG,
    name: 'Remind Co',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive'],
    peopleOrder: [MANAGER, OWNER],
    departments: {
      executive: {
        id: 'executive',
        name: 'Executive',
        purpose: 'Run the company.',
        headPersonId: MANAGER,
        state: 'active'
      }
    },
    people: {
      [MANAGER]: person(MANAGER, 'CEO', 'executive'),
      [OWNER]: person(OWNER, 'Analyst', 'worker')
    }
  }
}

function supervisionLedger(): Record<string, unknown> {
  return {
    schemaVersion: 1,
    organization: SLUG,
    assignmentOrder: [],
    assignments: {}
  }
}

/** The envelope chiefd's `person_reminder` dispatch stages, as the intercom's
 *  `readMailboxDoc` reconstructs it. `body` carries the `[reminder]` marker
 *  because that literal is what the card renderer keys on. */
function reminderEnvelope(personId: string): Record<string, unknown> {
  return {
    schemaVersion: 1,
    id: `reminder-for-${personId}`,
    person: personId,
    state: 'pending',
    updatedAt: 1,
    organization: SLUG,
    fromPersonId: 'system',
    to: personId,
    recipients: [personId],
    body: REMINDER_BODY,
    urgency: 'normal',
    createdAt: '2026-01-02T00:00:00.000Z'
  }
}

/* eslint-disable lucy/no-json-stringify */
// The same exemption every other wire fixture records: the
// replacement helper is private to a sibling repo and is not a dependency
// here, and this stub must answer chiefd's wire bytes verbatim.
async function startStubChiefd(): Promise<StubChiefd> {
  // Filled the moment the socket binds, and read only from a request handler,
  // which cannot run before that -- so no initial value is ever read, and
  // eslint 10's `no-useless-assignment` says so. Declared without one; the
  // definite-assignment analysis is satisfied by the bind callback below.
  let selfPort: number
  let selfUrl: string
  const watchers: WatchStream[] = []
  const mailboxes = new Map<string, Record<string, unknown>[]>()

  const server: Server = createServer((request, response) => {
    const url = request.url ?? ''
    const path = url.split('?')[0] ?? ''
    // DELETED: the `/v1/lookup` branch. This stub used to be its own beacond,
    // because a pane's extensions resolved their company's daemon through it
    // and a test that skipped discovery would prove nothing about which daemon
    // a hosted person reaches. They read `<dir>/.chief/run/daemon.json` now, so
    // no code under test calls this route — and the row it served still had an
    // `orgsRoot` and no `dir`, a shape `parseCompanyRow` refuses outright. It
    // went with the `paths` recorder beside it, which collected every request
    // path and was asserted by nothing.

    if (path === '/v1/docs/watch') {
      // chiefd's own contract: `text/event-stream`, one `doc-change` frame per
      // matching mutation, plus a comment heartbeat. The stream is held open
      // exactly as the daemon holds it.
      const query = new URL(url, 'http://localhost')
      const stores = (query.searchParams.get('stores') ?? '').split(',').filter((s) => s !== '')
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache, no-transform',
        connection: 'keep-alive'
      })
      response.write(': hb\n\n')
      const stream: WatchStream = { stores, response, closed: false }
      watchers.push(stream)
      // Both, because they answer the same question from two sides and Node
      // does not guarantee which one a client-side `fetch` abort reaches
      // first on a streaming response.
      const disconnected = (): void => {
        stream.closed = true
      }
      request.on('close', disconnected)
      request.on('aborted', disconnected)
      response.on('close', disconnected)
      return
    }

    const chunks: Buffer[] = []
    request.on('data', (chunk: Buffer) => chunks.push(chunk))
    request.on('end', () => {
      const raw = Buffer.concat(chunks).toString('utf8')
      const parsed: unknown = raw === '' ? {} : JSON.parse(raw)
      const body = isRecord(parsed) ? parsed : {}
      const answer = ((): string => {
        if (path === '/v1/org/manifest/read') {
          return JSON.stringify({ found: true, manifest: JSON.stringify(manifest()), seq: 1 })
        }
        if (path === '/v1/org/supervision/read') {
          return JSON.stringify({
            found: true,
            seq: 1,
            ledger: JSON.stringify(supervisionLedger())
          })
        }
        if (path === '/v1/org/mailbox/read-person') {
          const person = typeof body.personId === 'string' ? body.personId : ''
          const entries = mailboxes.get(person) ?? []
          return JSON.stringify({ found: true, seq: 1, mailbox: JSON.stringify({ entries }) })
        }
        return JSON.stringify({ found: false, seq: 0 })
      })()
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(answer)
    })
  })

  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (typeof address === 'string' || isNullish(address)) {
    throw new Error('the stub chiefd did not bind a port')
  }
  selfPort = address.port
  selfUrl = `http://127.0.0.1:${selfPort}`

  let seq = 0
  return {
    url: selfUrl,
    watchers,
    stage: (personId, envelope) => {
      const existing = mailboxes.get(personId) ?? []
      existing.push(envelope)
      mailboxes.set(personId, existing)
    },
    publishMailboxChange: (personId) => {
      seq += 1
      const store = `mailbox/${personId}`
      const data = JSON.stringify({
        seq,
        slug: SLUG,
        store,
        updated_at: '2026-01-02T00:00:00.000Z',
        removed: false
      })
      for (const watcher of watchers) {
        if (watcher.closed || !watcher.stores.includes(store)) continue
        watcher.response.write(`id: ${seq}\nevent: doc-change\ndata: ${data}\n\n`)
      }
    },
    stop: () =>
      new Promise<void>((resolve) => {
        for (const watcher of watchers) watcher.response.end()
        server.close(() => resolve())
      })
  }
}
/* eslint-enable lucy/no-json-stringify */

/** `sha256(<dir>)[..12]` — the key the daemon publishes and every route
 * resolves by. Derived here because this fixture stands in for the daemon that
 * WRITES the rendezvous; the extension under test only ever reads it. */
function companyKeyFor(dir: string): string {
  return createHash('sha256').update(dir).digest('hex').slice(0, 12)
}

/**
 * Publish the company's rendezvous, exactly as `chiefd` writes it.
 *
 * THE COMPANY DIRECTORY IS HOW AN EXTENSION FINDS ITS DAEMON. It used to ask
 * beacond by slug, and a slug names no company — two directories may hold
 * companies called the same thing. A pane's (and a hosted agent's) own
 * directory answers instead, from one local read.
 */
function publishRendezvous(dir: string, url: string): void {
  mkdirSync(join(dir, '.chief', 'run'), { recursive: true })
  /* eslint-disable lucy/no-json-stringify */
  // The same exemption every other wire fixture records: the replacement
  // helper is private to a sibling repo and is not a dependency here, and this
  // is a WIRE body two programs decode.
  writeFileSync(
    join(dir, '.chief', 'run', 'daemon.json'),
    JSON.stringify({ dir, key: companyKeyFor(dir), url, pid: process.pid })
  )
  /* eslint-enable lucy/no-json-stringify */
}

function profileFor(personId: string): AgentProfile {
  return {
    personId,
    cwd: `${ROOT}/${personId}/workspace`,
    env: {
      ORG_LAUNCHER_IDENTITY_DIR: join(companyDir, '.chief'),
      // The COMPANY DIRECTORY. Everything the daemon owns for this company —
      // including the rendezvous that names it — is under its `.chief` folder.
      ORG_LAUNCHER_ORG_DIR: companyDir,
      ORG_LAUNCHER_ORGANIZATION: SLUG,
      ORG_LAUNCHER_PERSON: personId,
      ORG_LAUNCHER_ROOT: `${ROOT}/orgs`
    },
    tools: [],
    displayName: `Remind Co · ${personId}`
  }
}

const running: { chiefd?: StubChiefd; lifecycles: HostedLifecycle[] } = { lifecycles: [] }

/** The company directory for this run — a real one, because the extension
 * reads a real rendezvous file out of it. */
let companyDir = ''

beforeEach(() => {
  companyDir = mkdtempSync(join(tmpdir(), 'web-hosted-company-'))
})

afterEach(async () => {
  for (const lifecycle of running.lifecycles) await lifecycle.shutdown('quit')
  running.lifecycles = []
  await running.chiefd?.stop()
  running.chiefd = undefined
  rmSync(companyDir, { recursive: true, force: true })
  vi.restoreAllMocks()
})

/** Host one person exactly as `AgentHost` does: install, bind, start. */
async function host(
  chiefd: StubChiefd,
  personId: string,
  options: HarnessStubOptions = {},
  replaceSession?: SessionReplacer
): Promise<{ stub: ReturnType<typeof harnessStub>; lifecycle: HostedLifecycle }> {
  const profile = profileFor(personId)
  const selection = await selectTools(profile)
  const stub = harnessStub(options)
  const lifecycle = selection.bind(
    isNullish(replaceSession)
      ? subjectFor(stub, profile.cwd)
      : subjectFor(stub, profile.cwd, replaceSession),
    `${ROOT}/${personId}/pi-home/sessions/session.jsonl`
  )
  running.lifecycles.push(lifecycle)
  await lifecycle.start('startup')
  return { stub, lifecycle }
}

/** Only the reminder deliveries.
 *
 * A settle delivers more than mail: an idle person with no open work gets the
 * intercom's own work-resume prompt, which is the OTHER thing `agent_settled`
 * drives and is itself evidence the hook fired. Filtering to the marker keeps
 * these assertions about the reminder without pretending the queue is empty. */
function reminders(delivered: readonly { mode: string; text: string }[]): readonly {
  mode: string
  text: string
}[] {
  return delivered.filter((message) => message.text.includes(REMINDER_BODY))
}

/** Wait until `check` holds, without a fixed sleep the machine's load can
 *  invalidate. The wake being tested is a socket write, so the wait is for the
 *  reader that consumed it — never for a duration. */
async function until(check: () => boolean, what: string): Promise<void> {
  const deadline = Date.now() + 10_000
  while (Date.now() < deadline) {
    if (check()) return
    await new Promise<void>((resolve) => setTimeout(resolve, 20))
  }
  throw new Error(`timed out waiting for ${what}`)
}

describe('a web-hosted person’s reactive lifecycle', { timeout: 30_000 }, () => {
  it('subscribes to its OWN mailbox store on its OWN company’s daemon', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    await host(chiefd, OWNER)

    // The subscription is the whole wake path. Its absence was invisible:
    // every tool worked, every turn answered, and nothing ever arrived.
    await until(() => chiefd.watchers.length > 0, 'the SSE subscription to open')
    const subscribed = chiefd.watchers.flatMap((watcher) => watcher.stores)
    expect(subscribed).toContain(`mailbox/${OWNER}`)
    // The maintenance stores ride the same connection; a subscription that
    // carried only the mailbox would leave supervision changes unheard.
    expect(subscribed).toContain('supervision')
  })

  it('wakes a person on a mailbox doc-change, with no settle and no timer', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const interval = vi.spyOn(globalThis, 'setInterval')
    const { stub } = await host(chiefd, OWNER)
    await until(() => chiefd.watchers.length > 0, 'the SSE subscription to open')

    // One settle with an EMPTY mailbox. No reminder is delivered, because none
    // is staged — the work-resume prompt that IS delivered is the same
    // handler's other half, and a person who used to get neither now gets
    // both.
    await stub.fire('settled', { nextTurnCount: 0 })
    expect(reminders(stub.delivered)).toEqual([])
    expect(stub.delivered.length).toBeGreaterThan(0)

    // chiefd's reminder duty fires and stages the durable row...
    chiefd.stage(OWNER, reminderEnvelope(OWNER))
    // ...and the row write publishes the `doc-change`. THAT is the wake.
    chiefd.publishMailboxChange(OWNER)

    await until(() => reminders(stub.delivered).length > 0, 'the reminder to reach the person')
    const [wake] = reminders(stub.delivered)
    expect(wake?.text).toContain(REMINDER_BODY)
    // It arrived as a TURN, not as a queue entry. A drain fires precisely when
    // a person is idle, and both live queues refuse an idle harness while the
    // third is never drained by the harness at all — so a reminder handed to
    // any of them is a reminder nobody reads. This is the assertion a live run
    // had to teach: the drain succeeded and `followUp` threw "Cannot follow up
    // while idle" into a fire-and-forget void.
    expect(wake?.mode).toBe('prompt')

    // No recurring timer was armed by any of this. The turn watchdog's
    // interval is armed at `turn_start` and this flow starts no turn; the wake
    // itself is a socket read, which is the point of #751.
    expect(interval).not.toHaveBeenCalled()
  })

  it('wakes the OWNER of the reminder and never the manager who armed it', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const owner = await host(chiefd, OWNER)
    const manager = await host(chiefd, MANAGER)
    await until(() => chiefd.watchers.length > 0, 'the SSE subscriptions to open')

    await owner.stub.fire('settled', { nextTurnCount: 0 })
    await manager.stub.fire('settled', { nextTurnCount: 0 })

    // Exactly what chiefd stages: `recipients_for("person_reminder")` reads
    // `payload.personId`, which is the owner. The manager's mailbox stays
    // empty, and this host must not invent an entry in it.
    chiefd.stage(OWNER, reminderEnvelope(OWNER))
    chiefd.publishMailboxChange(OWNER)

    await until(() => reminders(owner.stub.delivered).length > 0, 'the owner to be woken')
    expect(reminders(owner.stub.delivered)[0]?.text).toContain(REMINDER_BODY)

    // The defect pinned in Rust yesterday, pinned again on the consumer side:
    // a manager woken on their report's cadence, forever, with the report
    // never hearing it. The manager's own session is fully alive — it settled
    // and got its own work-resume prompt — and the reminder is not in it.
    expect(reminders(manager.stub.delivered)).toEqual([])
    expect(manager.stub.delivered.length).toBeGreaterThan(0)
  })

  it('drains mail already staged when the person settles', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    // Staged BEFORE the person is hosted: a reminder that came due while the
    // server was restarting has no live `doc-change` to ride, and the settle
    // boundary is what recovers it.
    chiefd.stage(OWNER, reminderEnvelope(OWNER))
    const { stub } = await host(chiefd, OWNER)

    await stub.fire('settled', { nextTurnCount: 0 })

    await until(() => reminders(stub.delivered).length > 0, 'the staged reminder to be drained')
    expect(reminders(stub.delivered)[0]?.text).toContain(REMINDER_BODY)
  })

  it('releases the person’s SSE subscription when they are shut down', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const { stub, lifecycle } = await host(chiefd, OWNER)
    await until(() => chiefd.watchers.length > 0, 'the SSE subscription to open')
    expect(stub.listenerCount()).toBeGreaterThan(0)
    const before = activeSseHubCount()

    await lifecycle.shutdown('quit')

    // `subscribeSse` holds ONE hub per `url|slug` and closes it when the last
    // subscriber leaves. The count going down is that release: an offboarded
    // person who kept a subscription would go on draining a mailbox into a
    // harness nobody can reach, for the life of the server process.
    //
    // Asserted on the hub rather than on the server's socket deliberately.
    // `SseWatcher` documents that a forced teardown cannot preempt an
    // in-flight `reader.read()`, so the socket is released best-effort while
    // the watcher's own state transition is immediate. The subscription is
    // what this host owns; the socket is Pi's transport keeping its own
    // promise.
    expect(activeSseHubCount()).toBe(before - 1)
    // And the driver stops observing the harness, which is the other half of
    // the same leak: a detached driver still holding listeners is a driver
    // still running.
    expect(stub.listenerCount()).toBe(0)

    // A doc-change published to a person who has been shut down reaches
    // nobody. This is the property an operator cares about: dropped means
    // dropped.
    chiefd.stage(OWNER, reminderEnvelope(OWNER))
    chiefd.publishMailboxChange(OWNER)
    await new Promise<void>((resolve) => setTimeout(resolve, 300))
    expect(reminders(stub.delivered)).toEqual([])
  })
})

describe('waking an idle person', { timeout: 30_000 }, () => {
  it('honours deliverAs while a turn IS running', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const { stub, lifecycle } = await host(chiefd, OWNER)

    // A turn is in flight, so the queue the extension named exists and is the
    // right place: an interrupt must reach the running turn as a steer rather
    // than starting a second one behind it.
    await stub.emit({ type: 'turn_start' })
    lifecycle.deliver('mid-turn note', 'followUp')
    lifecycle.deliver('stop what you are doing', 'steer')
    await until(() => stub.delivered.length >= 2, 'both queued messages')

    expect(stub.delivered.map((message) => message.mode)).toEqual(['followUp', 'steer'])
  })

  it('serializes two wakes rather than colliding on a busy harness', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const { stub, lifecycle } = await host(chiefd, OWNER)
    const before = stub.delivered.length

    // `prompt()` refuses a busy harness, so two envelopes arriving together
    // must become two turns in order — never one turn and one dropped
    // "AgentHarness is busy".
    lifecycle.deliver('first', 'followUp')
    lifecycle.deliver('second', 'followUp')
    await until(() => stub.delivered.length >= before + 2, 'both wakes to run')

    const texts = stub.delivered.slice(before).map((message) => message.text)
    expect(texts).toEqual(['first', 'second'])
    expect(stub.delivered.slice(before).every((message) => message.mode === 'prompt')).toBe(true)
  })
})

describe('a hosted person’s context window', { timeout: 30_000 }, () => {
  /** Pi's `ContextUsage`, as an extension handler reads it off the context. */
  function usageOf(lifecycle: HostedLifecycle): unknown {
    /* eslint-disable @typescript-eslint/consistent-type-assertions */
    // `ExtensionContext` is Pi's large concrete facade; this reads the one
    // member under test, exactly as `queueAutomaticParkCompaction` does.
    const context = lifecycle.context as unknown as {
      getContextUsage?: () => HostedContextUsage | undefined
    }
    /* eslint-enable @typescript-eslint/consistent-type-assertions */
    // The intercom's own guard, reproduced: it refuses to decide anything
    // about a park unless this member is a function.
    if (typeof context.getContextUsage !== 'function') return 'no getContextUsage on this host'
    return context.getContextUsage()
  }

  it('reports how full the window is, from the model and the transcript', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const { lifecycle } = await host(chiefd, OWNER, {
      contextWindow: 200_000,
      contextTokens: 50_000
    })

    // The number an operator and an extension both need, and the one this host
    // published nothing for: a long-lived hosted CEO grew until the provider
    // refused the request, with no reading anybody could act on.
    expect(usageOf(lifecycle)).toEqual({
      tokens: 50_000,
      contextWindow: 200_000,
      percent: 25
    })
  })

  it('answers nothing at all for a model that publishes no window', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const { lifecycle } = await host(chiefd, OWNER, { contextTokens: 50_000 })

    // Pi's own answer, and the honest one: a percentage of an unknown window
    // is not a smaller fact, it is an invented one.
    expect(usageOf(lifecycle)).toBeUndefined()
  })

  it('compacts when the window says it is due, and stops when it is not', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    // Over `shouldCompact`'s threshold — the window less its 16384 reserve.
    const { stub, lifecycle } = await host(chiefd, OWNER, {
      contextWindow: 100_000,
      contextTokens: 90_000,
      tokensAfterCompaction: 20_000
    })

    await stub.fire('settled', { nextTurnCount: 0 })

    // A tmux pane gets this threshold from Pi for nothing. `AgentHarness`
    // publishes `compact()` and never calls it, so without this the hosted
    // person's first long life ends at the window.
    expect(stub.compactions.map((entry) => entry.tokensBefore)).toEqual([90_000])
    expect(usageOf(lifecycle)).toEqual({
      tokens: 20_000,
      contextWindow: 100_000,
      percent: 20
    })

    // And it stops. The reading is re-taken from the compacted transcript, so
    // a second settle finds a window that is no longer full — the runaway a
    // stale snapshot would produce.
    await stub.fire('settled', { nextTurnCount: 0 })
    expect(stub.compactions.length).toBe(1)
  })

  it('publishes the SAME snapshot to a reader outside the process, never a second computation', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const { stub, lifecycle } = await host(chiefd, OWNER, {
      contextWindow: 100_000,
      contextTokens: 90_000,
      tokensAfterCompaction: 20_000
    })

    // The whole point of the reader: what an HTTP caller sees is what the
    // extensions see. Same object, not an equal one — a route that recomputed
    // would answer from a different read of the transcript than the handler
    // standing next to it.
    expect(lifecycle.contextUsage()).toBe(usageOf(lifecycle))
    expect(lifecycle.contextUsage()).toEqual({
      tokens: 90_000,
      contextWindow: 100_000,
      percent: 90
    })

    // …and it MOVES with the snapshot rather than being frozen at install.
    // A reader that answered the boot-time number for the life of the process
    // would look exactly like this one until the first compaction.
    await stub.fire('settled', { nextTurnCount: 0 })
    expect(lifecycle.contextUsage()).toEqual({
      tokens: 20_000,
      contextWindow: 100_000,
      percent: 20
    })
    expect(lifecycle.contextUsage()).toBe(usageOf(lifecycle))
  })

  it('stamps the reading with the boundary it was taken at, and does not re-stamp it on a read', async () => {
    // The reading is a SNAPSHOT and stops advancing when boundaries stop. A
    // person whose every turn the provider refuses produces no `settled`, no
    // `start` and no `compact` — so the number freezes, and without a stamp a
    // value frozen fourteen minutes ago is byte-identical to one taken this
    // second. Observed live: a CEO reported 19,636 tokens for fourteen minutes
    // while the same session recomputed to 1,054,396, which is what the
    // provider had just rejected the request at. The arithmetic was right; the
    // answer was old, and nothing said so.
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    vi.setSystemTime(new Date('2026-08-10T20:00:00.000Z'))
    const { stub, lifecycle } = await host(chiefd, OWNER, {
      contextWindow: 100_000,
      contextTokens: 90_000,
      tokensAfterCompaction: 20_000
    })

    const installed = lifecycle.contextUsageAsOf()
    expect(installed).toBe(Date.parse('2026-08-10T20:00:00.000Z'))

    // Reading it again does NOT freshen it: the stamp belongs to the snapshot,
    // not to whoever asked. A stamp that moved on read would say "current" for
    // a number nobody had recomputed — the exact lie this exists to prevent.
    vi.setSystemTime(new Date('2026-08-10T20:14:00.000Z'))
    expect(lifecycle.contextUsageAsOf()).toBe(installed)
    expect(lifecycle.contextUsage()).toEqual({
      tokens: 90_000,
      contextWindow: 100_000,
      percent: 90
    })

    // A real boundary moves both together.
    await stub.fire('settled', { nextTurnCount: 0 })
    expect(lifecycle.contextUsageAsOf()).toBe(Date.parse('2026-08-10T20:14:00.000Z'))
    expect(lifecycle.contextUsage()).toEqual({
      tokens: 20_000,
      contextWindow: 100_000,
      percent: 20
    })
    vi.useRealTimers()
  })

  it('reports no reading, rather than a zero, for a model with no window', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const { lifecycle } = await host(chiefd, OWNER, { contextTokens: 50_000 })

    // `undefined`, the same answer the extensions get. A route that turned
    // this into `0` would report a person with all their window free at the
    // moment nothing can be measured about them.
    expect(lifecycle.contextUsage()).toBeUndefined()
  })

  it('never compacts a window it has no reading for', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const { stub } = await host(chiefd, OWNER, { contextWindow: 100_000, contextTokens: 90_000 })

    // One compaction leaves the transcript with no assistant turn behind it,
    // so the token count is genuinely unknown until the next provider
    // response. Pi answers `null`; a host that read that as "empty" would be
    // fine, and a host that read the PRE-compaction usage would compact again
    // immediately, and again, forever.
    await stub.fire('settled', { nextTurnCount: 0 })
    expect(stub.compactions.length).toBe(1)

    await stub.fire('settled', { nextTurnCount: 0 })
    await stub.fire('settled', { nextTurnCount: 0 })
    expect(stub.compactions.length).toBe(1)
  })

  it('drives session_compact, carrying the harness’s own fact under Pi’s name', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const { stub } = await host(chiefd, OWNER, {
      contextWindow: 100_000,
      contextTokens: 90_000,
      tokensAfterCompaction: 10_000
    })

    // The hook is DRIVEN now, and the refusal it used to carry is retired:
    // `fromHook` and `fromExtension` are one predicate under two names, both
    // written to the entry's single `fromHook` field.
    expect(DRIVEN_HOOKS.has('session_compact')).toBe(true)
    // #1208 added the first live refusal: `input`, which the intercom registers
    // to rescue Pi interactive-TUI submissions and which this host has no event
    // to drive. 2026-08-24 added the second: `session_before_compact`, the
    // START of a compaction — this host drives the END and never decides to
    // compact, so there is no moment to fire it from, and mapping it to the end
    // would beat "working" at the instant the work finished.
    //
    // Named rather than counted, so a THIRD refusal cannot slip in beside them
    // without somebody editing this line deliberately, and the stale check
    // below still proves both are actually registered.
    expect([...REFUSED_HOOKS.keys()]).toEqual(['input', 'session_before_compact'])

    await stub.fire('settled', { nextTurnCount: 0 })
    expect(stub.compactions.length).toBe(1)
  })
})

describe('a hosted person asking for a fresh session', { timeout: 30_000 }, () => {
  /** Ask the way the intercom's late fallback asks: straight at the context. */
  function ask(lifecycle: HostedLifecycle, request: SessionReplacementRequest): boolean {
    /* eslint-disable @typescript-eslint/consistent-type-assertions */
    // The patched Pi member, read off the context exactly as
    // `scheduleLateNativeFreshSession` reads it.
    const context = lifecycle.context as unknown as {
      requestSessionReplacement: (request: SessionReplacementRequest) => boolean
    }
    /* eslint-enable @typescript-eslint/consistent-type-assertions */
    return context.requestSessionReplacement(request)
  }

  it('is SERVED, not queued for a tmux client that may never attach', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const served: SessionReplacementRequest[] = []
    const { lifecycle } = await host(chiefd, OWNER, {}, (request) => {
      served.push(request)
      return Promise.resolve()
    })
    const outcomes: string[] = []

    const accepted = ask(lifecycle, {
      customType: 'organization-company-native-reset',
      data: { requestId: 'req-1', sourceSessionId: 'stub-session' },
      onResult: (result) => void outcomes.push(result.status)
    })

    // `false` was the old answer, and it did not make the request go away: the
    // durable fresh-session request stayed `running` in chiefd's ledger,
    // claimed by a person nobody was going to serve. A durable request with no
    // server is a leak.
    expect(accepted).toBe(true)
    await until(() => served.length > 0, 'the host to serve the replacement')
    expect(served[0]?.customType).toBe('organization-company-native-reset')
    await until(() => outcomes.length > 0, 'the outcome to be reported')
    expect(outcomes).toEqual(['completed'])
  })

  it('refuses a second request while one is in flight', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const { lifecycle } = await host(chiefd, OWNER, {}, () => new Promise<void>(() => {}))

    const request: SessionReplacementRequest = { customType: 'marker' }
    expect(ask(lifecycle, request)).toBe(true)
    // The patched runner's own single-flight rule. Two replacements racing
    // would leave two transcripts under one agent.
    expect(ask(lifecycle, request)).toBe(false)
  })

  it('replaces nothing while a turn is running, and says so', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const served: SessionReplacementRequest[] = []
    const { stub, lifecycle } = await host(chiefd, OWNER, {}, (request) => {
      served.push(request)
      return Promise.resolve()
    })
    const outcomes: { status: string; error?: string }[] = []

    // A turn starts between the request and the moment it would be served. A
    // replacement swaps the transcript under the agent, so this one must not
    // happen — and must not silently not happen either.
    expect(
      ask(lifecycle, {
        customType: 'marker',
        onResult: (result) => void outcomes.push({ ...result })
      })
    ).toBe(true)
    await stub.emit({ type: 'turn_start' })

    await until(() => outcomes.length > 0, 'the refusal to be reported')
    expect(outcomes[0]?.status).toBe('failed')
    expect(outcomes[0]?.error).toContain('idle')
    expect(served).toEqual([])
  })

  it('honours a replacement a settled handler asks for by RETURNING it', async () => {
    const served: SessionReplacementRequest[] = []
    const stub = harnessStub()
    // The driver alone, with one handler that answers the way the intercom's
    // `agent_settled` answers when it holds a claimed fresh-session request.
    // This is the intercom's PRIMARY path — `ctx.requestSessionReplacement` is
    // its late fallback — and a host that drove the hook and dropped the
    // returned value would have exactly half the mechanism.
    const lifecycle = driveLifecycle(
      new Map([
        [
          'agent_settled',
          [
            (): unknown => ({
              newSession: {
                customType: 'organization-company-native-reset',
                data: { requestId: 'r' }
              }
            })
          ]
        ]
      ]),
      subjectFor(stub, `${ROOT}/returned/workspace`, (request) => {
        served.push(request)
        return Promise.resolve()
      }),
      `${ROOT}/returned/pi-home/sessions/session.jsonl`
    )
    running.lifecycles.push(lifecycle)

    await stub.fire('settled', { nextTurnCount: 0 })

    await until(() => served.length > 0, 'the returned replacement to be served')
    expect(served[0]?.customType).toBe('organization-company-native-reset')
  })
})

describe('a compaction the extensions are told about', { timeout: 30_000 }, () => {
  it('reaches session_compact with Pi’s field names and the host’s own reason', async () => {
    const seen: Record<string, unknown>[] = []
    const stub = harnessStub({
      contextWindow: 100_000,
      contextTokens: 90_000,
      tokensAfterCompaction: 10_000
    })
    const lifecycle = driveLifecycle(
      new Map([
        [
          'session_compact',
          [
            (event: unknown): unknown => {
              if (isRecord(event)) seen.push({ ...event })
              return undefined
            }
          ]
        ]
      ]),
      subjectFor(stub, `${ROOT}/compacted/workspace`),
      `${ROOT}/compacted/pi-home/sessions/session.jsonl`
    )
    running.lifecycles.push(lifecycle)

    await stub.fire('settled', { nextTurnCount: 0 })

    // The refusal on `main` said driving this hook would mean asserting that a
    // hook-supplied compaction is an extension-requested one. Both booleans are
    // computed from the same expression — "did a `session_before_compact`
    // handler supply the summary?" — and both are written to the entry's one
    // `fromHook` field, so the carry is a field carry.
    expect(seen.length).toBe(1)
    expect(seen[0]?.fromExtension).toBe(false)
    // Not mapped from anything: the host started this compaction on its own
    // threshold, so it is the host that knows why.
    expect(seen[0]?.reason).toBe('threshold')
    expect(seen[0]?.willRetry).toBe(false)
    // The entry the harness persisted, forwarded unchanged.
    expect(seen[0]?.compactionEntry).toMatchObject({ fromHook: false, tokensBefore: 90_000 })
  })
})

describe('the hook classification', { timeout: 30_000 }, () => {
  it('accounts for EVERY hook the real hosted extensions register', async () => {
    const registered = new Set<string>()
    const recorder = {
      registerTool: (): void => {},
      registerMessageRenderer: (): void => {},
      registerEntryRenderer: (): void => {},
      appendEntry: (): void => {},
      on: (event: string): void => void registered.add(event),
      sendMessage: (): void => {},
      setThinkingLevel: (): void => {},
      setModel: (): Promise<boolean> => Promise.resolve(true)
    }
    /* eslint-disable @typescript-eslint/consistent-type-assertions */
    // A SECOND, independent recorder. Reading the driver's own bookkeeping
    // would pass however much it dropped.
    const pi = recorder as never
    /* eslint-enable @typescript-eslint/consistent-type-assertions */
    // The MANAGER, because an extension may install for `ceo` alone: a
    // classification proved against a worker could miss registrations the
    // full hosted set makes.
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const profile = profileFor(MANAGER)
    for (const install of HOSTED_EXTENSIONS) await install(pi, profile.env)

    // Every registration is either driven or refused. A hook that is neither
    // means somebody added a lifecycle hook to an extension and nobody here
    // decided what this host does about it — which is the exact silence this
    // whole packet exists to end.
    const unclassified = [...registered].filter(
      (hook) => !DRIVEN_HOOKS.has(hook) && !REFUSED_HOOKS.has(hook)
    )
    expect(unclassified).toEqual([])

    // And the reverse: a refusal naming a hook nothing registers is a stale
    // decision about code that moved, the same bidirectional discipline the
    // reactive allowlist keeps.
    const stale = [...REFUSED_HOOKS.keys()].filter((hook) => !registered.has(hook))
    expect(stale).toEqual([])
    expect(registered.size).toBeGreaterThan(10)
  })

  it('refuses a hook by name rather than accepting it into an empty function', async () => {
    const chiefd = await startStubChiefd()
    running.chiefd = chiefd
    publishRendezvous(companyDir, chiefd.url)
    const profile = profileFor(MANAGER)
    const adapted = await installExtensions(profile)

    // `session_compact` is the one hook this host will not drive, and it is
    // reported rather than swallowed. Reported is the whole product here: an
    // accepted-and-never-called callback looks identical to a working one.
    expect(adapted.refusedHandlers).toEqual([...REFUSED_HOOKS.keys()].sort())
  })

  it('names an unclassified hook in its refusal, so a reader knows what to do', () => {
    const reason = unclassifiedHookReason('session_before_fork')
    expect(reason).toContain('session_before_fork')
    expect(reason).toContain('server/ExtensionLifecycle')
  })
})
