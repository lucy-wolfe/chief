/**
 * NOBODY IS INTERROGATED BEFORE A STRUCTURAL CHANGE.
 *
 * Five verbs — `org_reparent_department`, `org_move_department_members`,
 * `org_appoint_department_head`, `org_transfer` and `org_offboard` — used to
 * require the caller to type a sentence explaining itself. Authorization is the
 * gate; prose is not a gate, and a required justification nobody reads is
 * friction in front of every structural operation (operator ruling,
 * 2026-08-13).
 *
 * This suite pins BOTH halves of the deletion, because a deletion that also
 * stopped recording would be the regression:
 *
 * 1. No `reason` survives in any of the five parameter schemas, so a model can
 *    never be refused for omitting one.
 * 2. Each verb is ACCEPTED with no reason supplied, reaches its chiefd route,
 *    and posts a body carrying no `reason` key at all. The ledger line on the
 *    staffing row is authored by chiefd from the act and the authenticated
 *    actor — pinned on the Rust side in `org_ops.rs`.
 *
 * REAL: the extension module, its registration, the registered `execute`, the
 * production chiefd client and wire. FAKE: `pi` (a recorder) and one loopback
 * server standing in for this company's chiefd, and a real
 * rendezvous file naming it.
 */
import type { Server } from 'node:http'
import { createServer } from 'node:http'

import { createCompanyDirectory } from '@test/support/CompanyRendezvous'
import { isNullish } from '@test/support/Nullish'
import { installOrganizationIntercom } from '@test-assets/organization-intercom'
import { afterEach, describe, expect, it } from 'vitest'

const SLUG = 'noreason'
const CREATED_AT = '2026-01-01T00:00:00.000Z'
/** The CEO, who heads the root and therefore manages every person. */
const CEO = 'ceo'
/** The head of `ops`. */
const LEAD = 'lead'
/** An ordinary member of `ops`, and the successor in the appointment. */
const MEMBER = 'member'
/** The head of `depot`, so no person heads two departments. */
const KEEPER = 'keeper'

function person(id: string, kind: string, departmentId: string): Record<string, unknown> {
  return {
    id,
    name: id,
    title: id,
    kind,
    departmentId,
    employmentState: 'active',
    createdAt: CREATED_AT
  }
}

function manifest(): Record<string, unknown> {
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: SLUG,
    name: 'No Reason',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'ops', 'depot'],
    peopleOrder: [CEO, LEAD, MEMBER, KEEPER],
    departments: {
      executive: {
        id: 'executive',
        name: 'No Reason',
        purpose: 'Run the company.',
        headPersonId: CEO,
        state: 'active'
      },
      ops: {
        id: 'ops',
        name: 'Operations',
        purpose: 'Operate.',
        parentDepartmentId: 'executive',
        headPersonId: LEAD,
        state: 'active'
      },
      depot: {
        id: 'depot',
        name: 'Depot',
        purpose: 'Store.',
        parentDepartmentId: 'ops',
        headPersonId: KEEPER,
        state: 'active'
      }
    },
    people: {
      [CEO]: person(CEO, 'executive', 'executive'),
      [LEAD]: person(LEAD, 'head', 'ops'),
      [MEMBER]: person(MEMBER, 'worker', 'ops'),
      [KEEPER]: person(KEEPER, 'head', 'depot')
    }
  }
}

interface PostedRequest {
  readonly path: string
  /** The top-level keys of the posted body — all this suite asks of it. */
  readonly keys: readonly string[]
}

interface StubChiefd {
  readonly url: string
  readonly posted: PostedRequest[]
  stop(): Promise<void>
}

/** Answers every mutation route with `applied: true`, and records what was
 *  posted so the body can be asserted to carry no audit prose. */
async function startStubChiefd(): Promise<StubChiefd> {
  const posted: PostedRequest[] = []
  const server: Server = createServer((request, response) => {
    const chunks: Buffer[] = []
    request.on('data', (chunk: Buffer) => chunks.push(chunk))
    request.on('end', () => {
      const path = (request.url ?? '').split('?')[0] ?? ''
      /* eslint-disable lucy/no-json-stringify */
      // The replacement
      // helper is private to a sibling repo and is not a dependency here.
      if (path === '/v1/org/manifest/read') {
        response.writeHead(200, { 'content-type': 'application/json' })
        response.end(JSON.stringify({ found: true, manifest: JSON.stringify(manifest()), seq: 1 }))
        return
      }
      const raw = Buffer.concat(chunks).toString('utf8')
      const parsed: unknown = raw.length > 0 ? JSON.parse(raw) : {}
      const keys = isNullish(parsed) || typeof parsed !== 'object' ? [] : Object.keys(parsed)
      posted.push({ path, keys })
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(
        JSON.stringify({
          applied: true,
          departmentId: 'depot',
          moved: [MEMBER],
          status: 'applied',
          structuralChanged: true
        })
      )
      /* eslint-enable lucy/no-json-stringify */
    })
  })
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (typeof address === 'string' || isNullish(address)) {
    throw new Error('the stub chiefd did not bind a port')
  }
  return {
    url: `http://127.0.0.1:${address.port}`,
    posted,
    stop: () => new Promise<void>((resolve) => server.close(() => resolve()))
  }
}

const running: { chiefd?: StubChiefd; company?: { remove(): void } } = {}

afterEach(async () => {
  running.company?.remove()
  await running.chiefd?.stop()
  running.company = undefined
  running.chiefd = undefined
})

interface RegisteredTool {
  readonly name: string
  readonly parameters?: { properties?: Record<string, unknown>; required?: readonly string[] }
  execute: (...args: never[]) => Promise<unknown>
}

async function registerTools(personId: string): Promise<{
  tools: Map<string, RegisteredTool>
  chiefd: StubChiefd
}> {
  const chiefd = await startStubChiefd()
  const company = createCompanyDirectory(chiefd.url)
  running.chiefd = chiefd
  running.company = company

  const tools = new Map<string, RegisteredTool>()
  const recorder = {
    registerTool(definition: RegisteredTool) {
      tools.set(definition.name, definition)
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
    on() {
      /* no lifecycle is delivered: this call needs no session */
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
      ORG_LAUNCHER_PERSON: personId,
      ORG_LAUNCHER_ROOT: '/tmp/structural-verbs-ask-no-reason/launcher'
    },
    pollIntervalMs: 0,
    turnWatchdogIntervalMs: 0,
    bootTransientRetryDelaysMs: [1, 1, 1]
  })
  return { tools, chiefd }
}

async function call(
  tool: RegisteredTool,
  params: Record<string, unknown>
): Promise<{ ok: boolean; message: string }> {
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Pi's own five-argument `execute` contract, invoked as the agent loop
  // invokes it. The recorder's argument types are `never` because this fixture
  // has no Pi session to build a real `ExtensionContext` from.
  const raw = (await (
    tool.execute as unknown as (
      id: string,
      params: Record<string, unknown>,
      signal: AbortSignal | undefined,
      onUpdate: undefined,
      context: undefined
    ) => Promise<unknown>
  )('tool-call-1', params, undefined, undefined, undefined)) as {
    content?: readonly { text?: string }[]
    details?: Record<string, unknown>
  }
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
  return {
    ok: (raw.details ?? {}).ok === true || (raw.details ?? {}).status === 'applied',
    message: String(raw.content?.[0]?.text ?? '')
  }
}

const STRUCTURAL_VERBS = [
  'org_reparent_department',
  'org_move_department_members',
  'org_appoint_department_head',
  'org_transfer',
  'org_offboard'
] as const

describe('a structural verb never asks the caller to justify itself', () => {
  it('no structural verb declares a reason parameter at all', async () => {
    const { tools } = await registerTools(CEO)
    for (const name of STRUCTURAL_VERBS) {
      const tool = tools.get(name)
      expect(tool, `${name} must be registered`).toBeDefined()
      const properties = tool?.parameters?.properties ?? {}
      expect(Object.keys(properties), `${name} still asks for prose`).not.toContain('reason')
      expect(tool?.parameters?.required ?? []).not.toContain('reason')
    }
  }, 20_000)

  // TOMBSTONE: `the maintenance verb asks for its target and no prose`.
  //
  // A3 pinned that `org_maintain_session` had stopped asking for a `reason` —
  // its keyword scan over that prose had decided whether a `fresh_session` was
  // allowed at all, so a caller who honestly wrote "changing the model" was
  // refused and one who wrote nothing of the kind got the identical action.
  // The whole tool is deleted, so there is no prose gate left to keep shut.

  it('each verb applies with no reason supplied, and posts none', async () => {
    const { tools, chiefd } = await registerTools(CEO)
    const invocations: Record<(typeof STRUCTURAL_VERBS)[number], Record<string, unknown>> = {
      org_reparent_department: { departmentId: 'depot', newParentDepartmentId: 'executive' },
      org_move_department_members: { fromDepartmentId: 'ops', toDepartmentId: 'depot' },
      org_appoint_department_head: {
        departmentId: 'ops',
        newHeadPersonId: MEMBER,
        incumbentDisposition: 'retain'
      },
      org_transfer: { personId: MEMBER, departmentId: 'depot' },
      org_offboard: { personId: MEMBER }
    }
    for (const name of STRUCTURAL_VERBS) {
      const tool = tools.get(name)
      expect(tool, `${name} must be registered`).toBeDefined()
      if (isNullish(tool)) continue
      const outcome = await call(tool, invocations[name])
      expect(outcome.ok, `${name} refused a call with no reason: ${outcome.message}`).toBe(true)
    }
    // Every mutation reached chiefd, and not one body carried audit prose.
    expect(chiefd.posted.length).toBeGreaterThanOrEqual(STRUCTURAL_VERBS.length)
    for (const request of chiefd.posted) {
      expect(request.keys, `${request.path} still posts a reason`).not.toContain('reason')
    }
  }, 20_000)
})
