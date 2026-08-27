/**
 * THE COMPACTION THAT COULD ONLY BE CLAIMED BY A SESSION THAT DID NOT NEED IT.
 *
 * `org_maintain_session action=compact` reached every state except the one it
 * exists for. Observed on a live company (`delta-works`, 2026-08-10): one
 * `compact` request, `status=queued`, every claim column NULL, unchanged from
 * 20:24 to past 21:47; four `provider-turn-failed` events beside it carrying
 * the provider's own `400 ... maximum context length is 1048576 tokens.
 * However, you requested 1053371`; and NOT ONE `session-maintenance-started`,
 * `-start-deferred` or `-skipped` event. The claim was never attempted.
 *
 * The reason is the lifecycle lease. Both existing claim points — the
 * `agent_settled` handler and the SSE-driven `runMaintenanceCycle` — require
 * `isCurrent()`, which required the settled epoch to still be current AND the
 * pane to be `isLiveIdle`: idle with NO pending messages. A wedged pane always
 * has pending messages (it cannot consume them; every turn dies at the
 * provider), so `settled()` mints nothing, `capture()` returns nothing, and
 * `processSessionMaintenance` is never even called. That is the deadlock, and
 * the first test below is that deadlock, asserted as a live fact rather than
 * described in a comment.
 *
 * The fix is a claim point Pi holds open BEFORE the turn:
 * `before_agent_start`, which Pi awaits inside `prompt()` before the agent run
 * begins and before the provider is contacted. The second test drives the same
 * wedged pane through that boundary and requires the durable claim to reach
 * chiefd and Pi's compaction to run.
 *
 * REAL here: the extension module, its lifecycle registrations, the production
 * chiefd client and wire shapes. FAKE: `pi` (a recorder), the extension
 * context (a wedged pane: idle, WITH pending messages), and one loopback
 * server standing in for this company's chiefd, and a real
 * rendezvous file naming it.
 */
import { mkdtempSync } from 'node:fs'
import type { Server } from 'node:http'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { createCompanyDirectory } from '@test/support/CompanyRendezvous'
import { isNullish } from '@test/support/Nullish'
import {
  installOrganizationIntercom,
  resetConditionalReadCacheForTest
} from '@test-assets/organization-intercom'
import { afterEach, describe, expect, it } from 'vitest'

const SLUG = 'preturncompaction'
const CEO = 'ceo'
const AT = '2026-08-10T20:24:33.587Z'
const REQUEST_ID = 'session-maintenance:1:ceo:compact'
/** The live session the wedged pane is running, and its leaf entry — the
 *  anchor the claim must record so the receipt has a witness. */
const SESSION_ID = 'preturn-wedged-session'
const LEAF_ENTRY = 'preturn-leaf-entry'

function person(id: string): Record<string, unknown> {
  return {
    id,
    name: 'Cleo',
    title: 'CEO',
    kind: 'executive',
    departmentId: 'executive',
    employmentState: 'active',
    createdAt: '2026-01-01T00:00:00.000Z'
  }
}

function manifest(): Record<string, unknown> {
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: SLUG,
    name: 'Pre Turn Compaction',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive'],
    peopleOrder: [CEO],
    departments: {
      executive: {
        id: 'executive',
        name: 'Executive',
        purpose: 'Run the company.',
        headPersonId: CEO,
        state: 'active'
      }
    },
    people: { [CEO]: person(CEO) }
  }
}

/** No open assignments and no goals — exactly the live company's state, so the
 *  settled-work gate is provably NOT what holds the request in either test. */
function supervisionLedger(): Record<string, unknown> {
  return {
    schemaVersion: 1,
    organization: SLUG,
    assignmentOrder: [],
    assignments: {}
  }
}

function activityLedger(): Record<string, unknown> {
  return {
    schemaVersion: 1,
    organization: SLUG,
    personOrder: [CEO],
    people: { [CEO]: { personId: CEO, lastDesiredActive: true } },
    transitionOrder: [],
    transitions: {}
  }
}

/** One queued request, as chiefd's ledger holds it — the row read off the live
 *  company, with its identifiers renamed. */
function maintenanceRequest(action: string): Record<string, unknown> {
  return {
    id: REQUEST_ID,
    action,
    personId: CEO,
    requestedBy: CEO,
    reason: 'operator compaction proof',
    automatic: false,
    status: 'queued',
    requestedAt: AT
  }
}

function maintenanceLedger(action: string): Record<string, unknown> {
  return {
    schemaVersion: 1,
    organization: SLUG,
    requestOrder: [REQUEST_ID],
    requests: { [REQUEST_ID]: maintenanceRequest(action) }
  }
}

/** The compact anchor chiefd records on a claimed `compact`, and nothing on
 *  any other action. */
function claimedAnchor(action: string): Record<string, unknown> {
  return action === 'compact'
    ? { compactSessionId: SESSION_ID, compactAnchorEntryId: LEAF_ENTRY }
    : {}
}

/* eslint-disable @typescript-eslint/consistent-type-assertions */
// `JSON.parse` is `any` and chiefd's request bodies are wide, open-ended
// records; the same narrowing every stub server in this suite does at its own
// wire edge. Confined to these two helpers so no test body needs an assertion.
function parseBody(raw: string): Record<string, unknown> {
  return raw ? (JSON.parse(raw) as Record<string, unknown>) : {}
}

function claimTokenOf(body: Record<string, unknown>): unknown {
  return (body.claim as Record<string, unknown> | undefined)?.claimToken
}
/* eslint-enable @typescript-eslint/consistent-type-assertions */

interface StubChiefd {
  readonly url: string
  /** Every `/v1/org/session-maintenance/start` body chiefd received. */
  readonly starts: Record<string, unknown>[]
  /** Every `/v1/org/session-maintenance/finish` body chiefd received — a
   *  `failed` here would mean the claim landed and the compaction did not. */
  readonly finishes: Record<string, unknown>[]
  stop(): Promise<void>
}

async function startStubChiefd(action: string): Promise<StubChiefd> {
  const starts: Record<string, unknown>[] = []
  const finishes: Record<string, unknown>[] = []
  const server: Server = createServer((request, response) => {
    const chunks: Buffer[] = []
    request.on('data', (chunk: Buffer) => chunks.push(chunk))
    request.on('end', () => {
      const path = (request.url ?? '').split('?')[0] ?? ''
      const raw = Buffer.concat(chunks).toString('utf8')
      /* eslint-disable lucy/no-json-stringify */
      // The replacement
      // helper is private to a sibling repo and is not a dependency here.
      const parsed = parseBody(raw)
      const body = ((): string => {
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
        if (path === '/v1/org/activity/read') {
          return JSON.stringify({ found: true, seq: 1, ledger: JSON.stringify(activityLedger()) })
        }
        if (path === '/v1/org/session-maintenance/read') {
          return JSON.stringify({
            found: true,
            seq: 1,
            ledger: JSON.stringify(maintenanceLedger(action))
          })
        }
        if (path === '/v1/org/session-maintenance/finish') {
          finishes.push(parsed)
          return JSON.stringify({
            ...maintenanceRequest(action),
            status: parsed.status,
            ...claimedAnchor(action)
          })
        }
        if (path === '/v1/org/session-maintenance/start') {
          starts.push(parsed)
          // The route's own wrapper shape: `{request: … | null}`.
          return JSON.stringify({
            request: {
              ...maintenanceRequest(action),
              status: 'running',
              claimedProcessId: process.pid,
              claimedSessionId: SESSION_ID,
              claimToken: claimTokenOf(parsed),
              ...claimedAnchor(action)
            }
          })
        }
        return JSON.stringify({ found: false, seq: 0 })
      })()
      /* eslint-enable lucy/no-json-stringify */
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(body)
    })
  })
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (typeof address === 'string' || isNullish(address)) {
    throw new Error('the stub chiefd did not bind a port')
  }
  return {
    url: `http://127.0.0.1:${address.port}`,
    starts,
    finishes,
    stop: () => new Promise<void>((resolve) => server.close(() => resolve()))
  }
}

interface CompactCall {
  readonly customInstructions?: string
  /** Set when Pi's asynchronous `onComplete` fires. The pre-turn handler must
   *  not return before this is true, or the turn underneath reaches the
   *  provider with the branch it could not send. */
  completed: boolean
}

/**
 * The wedged pane, as Pi presents it at both boundaries under test:
 *
 * - `isIdle()` is TRUE. At `agent_settled` Pi has already cleared its run
 *   flag; at `before_agent_start` the run has not begun. Both are honest.
 * - `hasPendingMessages()` is TRUE, and that is the whole point. The live CEO
 *   had two queued notifications it could never consume, because consuming one
 *   needs a turn and every turn died at the provider. This single fact is what
 *   makes `isLiveIdle` false and starves the settled claim.
 */
function wedgedContext(compacts: CompactCall[]): Record<string, unknown> {
  return {
    isIdle: () => true,
    hasPendingMessages: () => true,
    sessionManager: {
      getSessionId: () => SESSION_ID,
      getLeafId: () => LEAF_ENTRY,
      // The leaf the claim anchors against must EXIST in the session, or
      // `nativeCompactionProof` reads an anchor it cannot find and refuses as
      // `ambiguous` before Pi is ever asked to compact.
      getEntries: () => [{ type: 'message', id: LEAF_ENTRY, parentId: null }],
      getBranch: () => []
    },
    ui: { notify: () => undefined },
    compact: (options: { customInstructions?: string; onComplete?: () => void }) => {
      const call: CompactCall = { customInstructions: options.customInstructions, completed: false }
      compacts.push(call)
      // Pi's callback is asynchronous; the pre-turn wait must actually wait
      // for it rather than racing past on the same tick.
      setTimeout(() => {
        call.completed = true
        options.onComplete?.()
      }, 5)
    }
  }
}

/** The same pane with its queue empty. This is the POSITIVE CONTROL: it is
 *  the one state in which the pre-fix settled path could always claim, so it
 *  proves the ledger, the stub and the claim path are all live. Without it,
 *  every "no claim happened" assertion in this file would pass against an
 *  inert fixture. */
function settleableContext(compacts: CompactCall[]): Record<string, unknown> {
  return { ...wedgedContext(compacts), hasPendingMessages: () => false }
}

interface Installed {
  readonly handlers: Map<string, ((event: unknown, ctx: unknown) => Promise<unknown>)[]>
  readonly chiefd: StubChiefd
  readonly compacts: CompactCall[]
  readonly context: Record<string, unknown>
  readonly settleable: Record<string, unknown>
}

const running: { chiefd?: StubChiefd; company?: { remove(): void } } = {}

afterEach(async () => {
  running.company?.remove()
  await running.chiefd?.stop()
  running.company = undefined
  running.chiefd = undefined
  resetConditionalReadCacheForTest()
})

async function install(action = 'compact'): Promise<Installed> {
  const root = mkdtempSync(join(tmpdir(), 'preturn-compaction-'))
  const chiefd = await startStubChiefd(action)
  const company = createCompanyDirectory(chiefd.url)
  running.chiefd = chiefd
  running.company = company

  const handlers = new Map<string, ((event: unknown, ctx: unknown) => Promise<unknown>)[]>()
  const recorder = {
    registerTool() {
      /* the tool surface is not what is under test */
    },
    registerMessageRenderer() {
      /* presentation only */
    },
    registerEntryRenderer() {
      /* presentation only */
    },
    appendEntry() {
      /* cards are not this test's subject */
    },
    on(event: string, handler: (event: unknown, ctx: unknown) => Promise<unknown>) {
      const existing = handlers.get(event) ?? []
      existing.push(handler)
      handlers.set(event, existing)
    },
    sendMessage() {
      /* presentation only */
    },
    setThinkingLevel() {
      /* presentation only */
    },
    setModel() {
      /* presentation only */
    }
  }
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Pi's `ExtensionAPI` is a large concrete class surface; the extension calls
  // exactly these methods, which is what Pi's own loader hands it. Same
  // reasoning `support/ToolRegistrationHarness.ts` records for its recorder.
  const pi = recorder as never
  /* eslint-enable @typescript-eslint/consistent-type-assertions */

  await installOrganizationIntercom(pi, {
    environment: {
      ORG_LAUNCHER_IDENTITY_DIR: `${company.dir}/.chief`,
      ORG_LAUNCHER_ORG_DIR: company.dir,
      ORG_LAUNCHER_ORGANIZATION: SLUG,
      ORG_LAUNCHER_PERSON: CEO,
      ORG_LAUNCHER_ROOT: join(root, 'launcher')
    },
    pollIntervalMs: 0,
    // pollIntervalMs 0 is itself the test-only seam that suppresses the SSE
    // watcher, so no background cycle can race the boundary under test.
    turnWatchdogIntervalMs: 0,
    idleResumeDelayMs: 0,
    bootTransientRetryDelaysMs: [1, 1, 1]
  })

  const compacts: CompactCall[] = []
  return {
    handlers,
    chiefd,
    compacts,
    context: wedgedContext(compacts),
    settleable: settleableContext(compacts)
  }
}

async function fire(
  installed: Installed,
  event: string,
  context = installed.context
): Promise<void> {
  const registered = installed.handlers.get(event) ?? []
  if (registered.length === 0) {
    throw new Error(
      `no handler is registered for '${event}'. Registered: ${[...installed.handlers.keys()]
        .sort()
        .join(', ')}`
    )
  }
  // The non-compact actions carry on past the claim into apply/model routes
  // this stub does not serve. What is under test is whether the CLAIM was
  // sent, so a later refusal must not be read as "no claim happened".
  for (const handler of registered) await handler({}, context).catch(() => undefined)
}

describe('a wedged pane claims its queued compaction before the turn, not inside it', () => {
  it('the settled boundary alone cannot claim it — that is the deadlock', async () => {
    const installed = await install()
    await fire(installed, 'session_start')
    await fire(installed, 'agent_settled')

    // Not a description of the bug: the bug. The pane is idle, the request is
    // queued, there is no open work — and the durable claim is never sent,
    // because pending messages alone disqualify the settled lease.
    expect(installed.chiefd.starts).toHaveLength(0)
    expect(installed.compacts).toHaveLength(0)

    // POSITIVE CONTROL, same install, same ledger, same request: drain the
    // queue and the settled boundary claims immediately. So nothing above is
    // an inert fixture — one fact, `hasPendingMessages()`, is the whole
    // difference between a company that compacts and one that cannot.
    const drained = await install()
    await fire(drained, 'session_start', drained.settleable)
    await fire(drained, 'agent_settled', drained.settleable)
    expect(drained.chiefd.starts).toHaveLength(1)
    expect(drained.compacts).toHaveLength(1)
  }, 30_000)

  it('the pre-turn boundary claims it and holds the turn until Pi has compacted', async () => {
    const installed = await install()
    await fire(installed, 'session_start')
    await fire(installed, 'before_agent_start')

    // The durable claim reached chiefd, for THIS request, naming the session
    // and the leaf the receipt will be proven against.
    expect(installed.chiefd.starts).toHaveLength(1)
    const start = installed.chiefd.starts[0] ?? {}
    expect(start.requestId).toBe(REQUEST_ID)
    expect(start.action).toBe('compact')
    expect(start.compactSessionId).toBe(SESSION_ID)
    expect(start.compactAnchorEntryId).toBe(LEAF_ENTRY)

    // And Pi actually compacted. The handler returned only after the
    // asynchronous `onComplete` fired, so the turn underneath cannot reach the
    // provider with the un-compacted branch.
    expect(installed.compacts).toHaveLength(1)
    expect(installed.compacts[0]?.customInstructions).toContain('durable commitments')
    // The handler HELD the turn. Pi's `compact()` is fire-and-forget, so
    // reaching this line with `completed` still false would mean the pane had
    // been released back into the very turn the compaction was meant to make
    // possible.
    expect(installed.compacts[0]?.completed).toBe(true)
    // The claim was not terminalised on the way past: nothing was refused,
    // skipped, or failed while the pane was being rescued.
    expect(installed.chiefd.finishes.map((finish) => finish.status)).not.toContain('failed')
  }, 30_000)

  // TOMBSTONE: `refuses to claim fresh_session before a turn — only compaction
  // is safe inside prompt()`.
  //
  // The pre-turn claim deliberately admits ONE action, because a compaction is
  // safe to run inside `prompt()` and a session replacement is not. That
  // restriction is unchanged and still enforced; what is gone is the second
  // action it was restricting AGAINST. With one action left there is nothing to
  // refuse, so a test that drove `fresh_session` at the pre-turn boundary can
  // no longer construct its own subject.
  //
  // The POSITIVE half of that test — a request IS claimable at the boundary
  // that owns it — survives in the compaction cases above.
})
