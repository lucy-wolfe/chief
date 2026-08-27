/**
 * `org_roster` WAS BROKEN FOR EVERY PERSON IN EVERY COMPANY, AND NO SUITE SAW IT.
 *
 * #751 moved tmux out of chiefd. The backend correctly stopped publishing tmux
 * ids: the durable `runtime` row's process map became person -> the process
 * handle the ACTUATOR reported — the pid as a decimal string, and the EMPTY
 * STRING when the actuator proved a person alive but could read no pid — and
 * `windows` became a map both publishers hardcode to `{}`
 * (`converge_apply/cycle.rs`, `runtime_lifecycle.rs`; `actuate/report.rs`:
 * "People and processes, never panes").
 *
 * The TypeScript reader was never moved with it. Against the real payload
 * `{"panes":{"ceo":"","head-of-engineering":""},"windows":{}}` it refused FIVE
 * ways:
 *
 *   1. `stringMap` — "runtime panes must contain non-empty string ids"
 *   2. `/^%\d+$/` on every pane value — a tmux pane id
 *   3. `/^@\d+$/` on every window value
 *   4. a duplicate-value check that two `""` values also tripped
 *   5. and, past all four, a `windows[department]` lookup that can never hit
 *
 * The live CEO and the live head of engineering both got (1), and the CEO
 * escalated it to the operator itself.
 *
 * # What is real here and what is not
 *
 * REAL: the extension module, its `org_roster` registration, the registered
 * `execute`, the production beacond client, and the production wire. FAKE:
 * `pi` (a recorder) and one loopback server standing in for
 * this company's chiefd. The payloads this server returns are the ones the
 * Rust producer builds, copied field for field — that is the whole point,
 * because every suite that hand-built a tmux-shaped row stayed green through
 * the outage.
 */
import type { Server } from 'node:http'
import { createServer } from 'node:http'

import { createCompanyDirectory } from '@test/support/CompanyRendezvous'
import { isNullish } from '@test/support/Nullish'
import { installOrganizationIntercom } from '@test-assets/organization-intercom'
import { afterEach, describe, expect, it } from 'vitest'

const SLUG = 'rosterproc'
const CEO = 'ceo'
/** The person whose process the actuator proved alive WITHOUT a readable pid.
 *  That is the empty-string case, and it is exactly what broke. */
const ENGINEER = 'head-of-engineering'
const SOCKET = 'roster-runtime-process-projection'
const OBSERVED_AT = '2026-08-07T12:00:00.000Z'
/** A real pid, as chiefd stringifies one. Not `%1`. */
const CEO_PID = '48213'

/** A one-department company: the CEO heads the root, the engineer works in it.
 *  Both resolve to `executive` through the manifest tree, which is the ONLY
 *  place a department comes from now — chiefd publishes no placement. */
function manifest(): Record<string, unknown> {
  const createdAt = '2026-01-01T00:00:00.000Z'
  const person = (
    id: string,
    name: string,
    title: string,
    kind: string
  ): Record<string, unknown> => ({
    id,
    name,
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
    name: 'Roster Proc',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive'],
    peopleOrder: [CEO, ENGINEER],
    departments: {
      executive: {
        id: 'executive',
        name: 'Executive',
        purpose: 'Run the company.',
        headPersonId: CEO,
        state: 'active'
      }
    },
    people: {
      [CEO]: person(CEO, 'Cleo', 'CEO', 'executive'),
      [ENGINEER]: person(ENGINEER, 'Enzo', 'Head of Engineering', 'worker')
    }
  }
}

function activityLedger(desiredActive: readonly string[]): Record<string, unknown> {
  const active = new Set(desiredActive)
  return {
    schemaVersion: 1,
    organization: SLUG,
    personOrder: [CEO, ENGINEER],
    people: {
      [CEO]: { personId: CEO, lastDesiredActive: active.has(CEO) },
      [ENGINEER]: { personId: ENGINEER, lastDesiredActive: active.has(ENGINEER) }
    },
    transitionOrder: [],
    transitions: {}
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

/**
 * The `runtime` row exactly as chiefd's converge pass publishes it.
 *
 * `processHandles` values are pid-or-empty-string, never `%N`. `windows` is
 * included and empty because that is what the row carried on the day the outage
 * was measured; a reader that has genuinely stopped consulting it must not care
 * either way — and a key the reader ignores is exactly how `windows` survived
 * as a dead mechanism in the first place.
 */
function runtimeRow(processes: Record<string, string>): Record<string, unknown> {
  return {
    version: 1,
    observedAt: OBSERVED_AT,
    socketName: SOCKET,
    status: 'running',
    processHandles: processes,
    windows: {}
  }
}

interface StubChiefd {
  readonly url: string
  stop(): Promise<void>
}

async function startStubChiefd(options: {
  readonly runtime: Record<string, unknown> | undefined
  readonly desiredActive: readonly string[]
}): Promise<StubChiefd> {
  const server: Server = createServer((request, response) => {
    const chunks: Buffer[] = []
    request.on('data', (chunk: Buffer) => chunks.push(chunk))
    request.on('end', () => {
      const path = (request.url ?? '').split('?')[0] ?? ''
      /* eslint-disable lucy/no-json-stringify */
      // The replacement
      // helper is private to a sibling repo and is not a dependency here.
      const body = ((): string => {
        if (path === '/v1/org/manifest/read') {
          return JSON.stringify({ found: true, manifest: JSON.stringify(manifest()), seq: 1 })
        }
        if (path === '/v1/org/activity/read') {
          return JSON.stringify({
            found: true,
            seq: 1,
            ledger: JSON.stringify(activityLedger(options.desiredActive))
          })
        }
        if (path === '/v1/org/supervision/read') {
          return JSON.stringify({
            found: true,
            seq: 1,
            ledger: JSON.stringify(supervisionLedger())
          })
        }
        if (path === '/v1/org/runtime/read') {
          return isNullish(options.runtime)
            ? JSON.stringify({ found: false, seq: 0 })
            : JSON.stringify({ found: true, seq: 1, doc: JSON.stringify(options.runtime) })
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

/** Install the extension for the CEO and call `org_roster` exactly as Pi's
 *  agent loop calls it. */
async function roster(options: {
  readonly runtime: Record<string, unknown> | undefined
  readonly desiredActive: readonly string[]
}): Promise<{ ok: boolean; message: string }> {
  const chiefd = await startStubChiefd(options)
  const company = createCompanyDirectory(chiefd.url)
  running.chiefd = chiefd
  running.company = company

  const tools = new Map<string, { execute: (...args: never[]) => Promise<unknown> }>()
  const recorder = {
    registerTool(definition: { name: string; execute: (...args: never[]) => Promise<unknown> }) {
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
      /* no lifecycle is delivered: a read needs no session */
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
      ORG_LAUNCHER_ROOT: '/tmp/roster-runtime-process-projection/launcher'
    },
    pollIntervalMs: 0,
    turnWatchdogIntervalMs: 0,
    bootTransientRetryDelaysMs: [1, 1, 1]
  })

  const tool = tools.get('org_roster')
  if (isNullish(tool)) {
    throw new Error(
      `org_roster is not registered. Registered: ${[...tools.keys()].sort().join(', ')}`
    )
  }
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
  )('tool-call-1', {}, undefined, undefined, undefined)) as {
    content?: readonly { text?: string }[]
    details?: Record<string, unknown>
  }
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
  const details = raw.details ?? {}
  // `toolResult` puts the flag in `details.ok` and the rendered roster — the
  // text the CEO actually reads — in `content[0].text`.
  return { ok: details.ok === true, message: String(raw.content?.[0]?.text ?? '') }
}

describe('org_roster reads the runtime row chiefd actually publishes', () => {
  it('answers for a person WITH a pid and a person WITHOUT one, in the same read', async () => {
    const outcome = await roster({
      runtime: runtimeRow({ [CEO]: CEO_PID, [ENGINEER]: '' }),
      desiredActive: [CEO, ENGINEER]
    })

    // SEEN TO FAIL. Before this fix the whole read threw and the tool
    // answered `ok:false` carrying the operator-visible sentence below.
    expect(outcome.ok, `org_roster failed: ${outcome.message}`).toBe(true)
    expect(outcome.message).not.toContain('must contain non-empty string ids')
    expect(outcome.message).not.toContain('invalid runtime pane or window ids')
    expect(outcome.message).not.toContain('duplicate pane or window ids')
    expect(outcome.message).not.toContain('has no observed window for')

    // BOTH people are running. The engineer's empty pid is chiefd's honest
    // "alive, pid unknown", not an absence — reading liveness off the VALUE
    // reported a running person as parked.
    expect(outcome.message).toMatch(/Cleo \[ceo\].*running/)
    expect(outcome.message).toMatch(/Enzo \[head-of-engineering\].*running/)
    // …and each one's process is described truthfully, in the two ways
    // chiefd can describe one.
    expect(outcome.message).toContain(`pid ${CEO_PID}`)
    expect(outcome.message).toContain('pid unknown')
    // No pane, no window: chiefd has neither, so the roster names neither.
    expect(outcome.message).not.toMatch(/\bpane\b/)
    expect(outcome.message).not.toMatch(/\bwindow\b/)
  }, 20_000)

  it('reports the exact payload from the live outage — every person pid-less', async () => {
    // The bytes the live company published on the day the CEO escalated:
    // two people alive, neither pid readable, `windows` empty.
    const outcome = await roster({
      runtime: runtimeRow({ [CEO]: '', [ENGINEER]: '' }),
      desiredActive: [CEO, ENGINEER]
    })
    expect(outcome.ok, `org_roster failed: ${outcome.message}`).toBe(true)
    // Two identical `""` values are not a duplicate id. The retired
    // uniqueness rule counted them as one and faulted the read.
    expect(outcome.message).toMatch(/Cleo \[ceo\].*running/)
    expect(outcome.message).toMatch(/Enzo \[head-of-engineering\].*running/)
    expect(outcome.message).toContain('Runtime observation: running')
  }, 20_000)

  it('still reports a person the runtime is NOT running as parked', async () => {
    // The gate is narrowed, never deleted. A person absent from the map is
    // absent; only the VALUE stopped being evidence, never the key.
    const outcome = await roster({
      runtime: runtimeRow({ [CEO]: CEO_PID }),
      desiredActive: [CEO]
    })
    expect(outcome.ok, `org_roster failed: ${outcome.message}`).toBe(true)
    expect(outcome.message).toMatch(/Cleo \[ceo\].*running/)
    expect(outcome.message).toMatch(/Enzo \[head-of-engineering\].*parked/)
  }, 20_000)

  it('reports an absent runtime row as absent rather than failing', async () => {
    const outcome = await roster({ runtime: undefined, desiredActive: [] })
    expect(outcome.ok, `org_roster failed: ${outcome.message}`).toBe(true)
    expect(outcome.message).toContain('Runtime observation: absent')
  }, 20_000)
})
