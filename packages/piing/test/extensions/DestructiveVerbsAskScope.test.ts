/**
 * THE TYPESCRIPT HALF OF THE DESTRUCTIVE NARROWING, LANDED AHEAD OF THE RUST.
 *
 * Operator ruling as it now stands (`AGENTS.md`, corrected 2026-08-13): a head
 * may do anything with anyone in its OWN SUBTREE, the CEO holds every tree, the
 * CEO is the only person nobody may act ON, and no protected REGION survives.
 *
 * chiefd does not hold that yet for the destructive verbs. Five sites still ask
 * the WIDE `guard_ceo_exempt` — `shutdown_person` (`org_ops.rs:375`),
 * `offboard_person` (`:1929`), `bench_person` (`:3164`),
 * `bench_person_lifecycle` (`:3298`) and `replace_head_and_offboard` (`:3742`)
 * — and that predicate exempts anybody whose home OR assigned unit is anywhere
 * in the executive-root region, `office-of-the-ceo` included. So a head cannot
 * yet offboard or bench somebody in its own subtree if that somebody happens to
 * be homed there.
 *
 * TypeScript has no such region and never did (#1066). This suite proves that
 * BEFORE the Rust changes, so that when the narrowing lands there is nothing to
 * change here and nothing to discover:
 *
 * 1. The pre-flight for a destructive verb is SCOPE and only scope. A head
 *    reaches a person homed in `office-of-the-ceo` inside its subtree, and the
 *    call leaves the pane.
 * 2. A peer is still refused, locally, with nothing posted.
 * 3. When chiefd refuses — which is what it does today for exactly the case in
 *    (1) — the tool surfaces chiefd's own code and detail verbatim and marks it
 *    non-retryable. That is the contract that lets the narrowing be a
 *    chiefd-only change: the refusal a caller reads is the daemon's, not a
 *    sentence this file invented.
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

const SLUG = 'destructive'
const CREATED_AT = '2026-01-01T00:00:00.000Z'
/** The head of `office-of-the-ceo` — the unit the Rust region still protects. */
const COS = 'cos'
/** A member of `office-of-the-ceo`, homed in the protected region. */
const AIDE = 'aide'
/** The head of a PEER department, who must reach neither of them. */
const RHEA = 'rhea'

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

/** A company WITH an `office-of-the-ceo`, because that unit is the whole
 *  difference between the region and the CEO. */
function manifest(): Record<string, unknown> {
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: SLUG,
    name: 'Destructive',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'office-of-the-ceo', 'engineering'],
    peopleOrder: ['ceo', COS, AIDE, RHEA],
    departments: {
      executive: {
        id: 'executive',
        name: 'Destructive',
        purpose: 'Run the company.',
        headPersonId: 'ceo',
        state: 'active'
      },
      'office-of-the-ceo': {
        id: 'office-of-the-ceo',
        name: 'Office of the CEO',
        purpose: 'Support the CEO.',
        parentDepartmentId: 'executive',
        headPersonId: COS,
        state: 'active'
      },
      engineering: {
        id: 'engineering',
        name: 'Engineering',
        purpose: 'Build the product.',
        parentDepartmentId: 'executive',
        headPersonId: RHEA,
        state: 'active'
      }
    },
    people: {
      ceo: person('ceo', 'executive', 'executive'),
      [COS]: person(COS, 'head', 'office-of-the-ceo'),
      [AIDE]: person(AIDE, 'worker', 'office-of-the-ceo'),
      [RHEA]: person(RHEA, 'head', 'engineering')
    }
  }
}

interface StubChiefd {
  readonly url: string
  readonly paths: string[]
  stop(): Promise<void>
}

/**
 * Serves the manifest and answers the staffing route with the refusal chiefd
 * gives TODAY for a person inside the executive-root region.
 *
 * Using chiefd's real code and detail is the point: the assertion below is that
 * this file forwards them untouched, which is what makes the narrowing a
 * chiefd-only change.
 */
async function startStubChiefd(): Promise<StubChiefd> {
  const paths: string[] = []
  const server: Server = createServer((request, response) => {
    const chunks: Buffer[] = []
    request.on('data', (chunk: Buffer) => chunks.push(chunk))
    request.on('end', () => {
      const path = (request.url ?? '').split('?')[0] ?? ''
      paths.push(path)
      /* eslint-disable lucy/no-json-stringify */
      // The replacement
      // helper is private to a sibling repo and is not a dependency here.
      if (path === '/v1/org/manifest/read') {
        response.writeHead(200, { 'content-type': 'application/json' })
        response.end(JSON.stringify({ found: true, manifest: JSON.stringify(manifest()), seq: 1 }))
        return
      }
      if (path === '/v1/org/staffing/lifecycle' || path === '/v1/org/person/bench-lifecycle') {
        response.writeHead(422, { 'content-type': 'application/json' })
        response.end(
          JSON.stringify({
            refused: 'exec-root-protected',
            // The REAL string chiefd emits. It said "the executive root (CEO /
            // office-of-the-ceo) never departs" until #1071 — a fixture that
            // teaches the retired protected-REGION model is how the model
            // comes back, even in a mock.
            detail:
              'the CEO heads the company root and never departs; everybody else may be ' +
              'offboarded, wherever they sit'
          })
        )
        return
      }
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ found: false, seq: 0 }))
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
    paths,
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

interface CallOutcome {
  readonly registered: boolean
  readonly ok: boolean
  readonly message: string
  readonly details: Record<string, unknown>
  readonly chiefdPaths: readonly string[]
}

async function callAs(
  personId: string,
  tool: string,
  params: Record<string, unknown>
): Promise<CallOutcome> {
  const chiefd = await startStubChiefd()
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
      ORG_LAUNCHER_ROOT: '/tmp/destructive-verbs-ask-scope/launcher'
    },
    pollIntervalMs: 0,
    turnWatchdogIntervalMs: 0,
    bootTransientRetryDelaysMs: [1, 1, 1]
  })

  const registered = tools.get(tool)
  if (isNullish(registered)) {
    return {
      registered: false,
      ok: false,
      message: '',
      details: {},
      chiefdPaths: [...chiefd.paths]
    }
  }
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Pi's own five-argument `execute` contract, invoked as the agent loop
  // invokes it. The recorder's argument types are `never` because this fixture
  // has no Pi session to build a real `ExtensionContext` from.
  const raw = (await (
    registered.execute as unknown as (
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
  const details = raw.details ?? {}
  return {
    registered: true,
    ok: details.ok === true,
    message: String(raw.content?.[0]?.text ?? ''),
    details,
    chiefdPaths: [...chiefd.paths]
  }
}

describe('a destructive verb asks scope, and no region', () => {
  it('a head reaches a member of its own subtree who is homed in office-of-the-ceo', async () => {
    // The case the Rust region still refuses. TypeScript does not: the
    // pre-flight is `departmentIsInScope(target.home)`, the aide is inside the
    // subtree `cos` heads, and the call leaves the pane.
    const outcome = await callAs(COS, 'org_offboard', {
      personId: AIDE
    })
    expect(outcome.registered).toBe(true)
    expect(outcome.message, 'no local authority refusal').not.toContain('does not manage person')
    expect(
      outcome.chiefdPaths,
      'the decision belongs to chiefd; TypeScript must reach it'
    ).toContain('/v1/org/staffing/lifecycle')
  }, 20_000)

  it('the CEO reaches the head of office-of-the-ceo, for the same reason', async () => {
    const outcome = await callAs('ceo', 'org_bench', {
      personId: COS,
      reason: 'The CEO holds every tree.'
    })
    expect(outcome.registered).toBe(true)
    expect(outcome.message).not.toContain('does not manage person')
    expect(outcome.chiefdPaths).toContain('/v1/org/person/bench-lifecycle')
  }, 20_000)

  it('a PEER head is refused locally, and posts nothing', async () => {
    // Scope is the whole safety model here. Narrowing the region in chiefd
    // must never be read as opening a person to somebody outside their tree.
    const outcome = await callAs(RHEA, 'org_offboard', {
      personId: AIDE
    })
    expect(outcome.registered).toBe(true)
    expect(outcome.ok).toBe(false)
    expect(outcome.message).toContain(`does not manage person '${AIDE}'`)
    expect(outcome.chiefdPaths, 'a refused pre-flight must post nothing').not.toContain(
      '/v1/org/staffing/lifecycle'
    )
  }, 20_000)
})

describe("chiefd's refusal is surfaced verbatim, which is what makes the narrowing chiefd-only", () => {
  it("carries chiefd's own code and detail, and marks it non-retryable", async () => {
    // TODAY this is what a head gets for its own aide. When the five sites
    // narrow to `is_ceo`, the same call succeeds with no change here — the
    // wording, the code and the retry decision are all chiefd's.
    const outcome = await callAs(COS, 'org_offboard', {
      personId: AIDE
    })
    expect(outcome.ok).toBe(false)
    expect(outcome.details.code, "chiefd's machine code, never re-classified").toBe(
      'exec-root-protected'
    )
    expect(outcome.details.retryable, 'a rule does not become true by asking again').toBe(false)
    expect(outcome.message).toContain('the CEO heads the company root and never departs')
  }, 20_000)
})
