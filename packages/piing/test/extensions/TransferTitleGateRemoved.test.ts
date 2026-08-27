/**
 * THE LAST JOB TITLE IN A STAFFING GATE, AND IT DECIDED NOTHING.
 *
 * `executeAtomicPersonTransfer` — the local pre-flight behind `org_transfer` —
 * refused unless `manager(managerPerson)` AND the target's department was in
 * scope. `manager` reads `person.kind`, which is a TITLE, and the operator
 * ruling of 2026-08-13 (`AGENTS.md`) forbids exactly that: "authority is the
 * subtree you head, never the job title".
 *
 * The title half was dead. `departmentScopeDenial` already answers
 * `out-of-scope` for every non-executive who heads no department, so everybody
 * the title check would have refused is refused by scope anyway; the only state
 * in which the conjunct could change an answer is one chiefd never writes — a
 * head recorded as a worker — and there it refused a person their OWN subtree.
 *
 * This suite is the pair that proves it, and the pair is the deliverable:
 *
 *  1. A NON-MANAGER WITH SCOPE reaches chiefd. The head of `engineering`,
 *     recorded `worker`, moves a member of `engineering` into `platform`
 *     beneath it. The title check refused this; scope always allowed it.
 *  2. A MANAGER WITHOUT SCOPE is still refused, locally, posting nothing. The
 *     head of the peer department `research` is kind `head` and passes any
 *     title question there is — and scope alone stops them.
 *
 * Neither half is enough on its own: the first shows the gate denied legitimate
 * callers, the second shows scope still refuses without it.
 *
 * REAL: the extension module, its registration, the registered `execute`, the
 * production chiefd client and wire. FAKE: `pi` (a recorder) and one loopback
 * server standing in for this company's chiefd, and a real
 * rendezvous file naming it.
 */
import { readFileSync } from 'node:fs'
import type { Server } from 'node:http'
import { createServer } from 'node:http'
import { fileURLToPath } from 'node:url'

import { createCompanyDirectory } from '@test/support/CompanyRendezvous'
import { isNullish } from '@test/support/Nullish'
import { withoutComments } from '@test/support/TypeScriptSource'
import type { IntercomOrganizationManifest, PersonRecord } from '@test-assets/organization-intercom'
import {
  departmentScopeDenial,
  installOrganizationIntercom
} from '@test-assets/organization-intercom'
import { afterEach, describe, expect, it } from 'vitest'

const SLUG = 'transfergate'
const CREATED_AT = '2026-01-01T00:00:00.000Z'
const SOURCE_PATH = fileURLToPath(
  new URL('../../extensions/organization-intercom.ts', import.meta.url)
)
/** The head of `engineering`, which is where the moved person lives and which
 *  contains the destination. */
const PAT = 'pat'
/** The head of `research`, a PEER: kind `head`, and no scope over
 *  `engineering`. */
const RHEA = 'rhea'
/** The member of `engineering` being moved. */
const MIRA = 'mira'

function personOf(manifest: IntercomOrganizationManifest, id: string): PersonRecord {
  const found = manifest.people[id]
  if (isNullish(found)) throw new Error(`the fixture has no person '${id}'`)
  return found
}

function person(
  id: string,
  kind: 'executive' | 'head' | 'worker',
  departmentId: string
): PersonRecord {
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

/**
 * A CEO over two peer departments, and one sub-unit beneath `engineering`.
 *
 * The sub-unit is what makes the positive case a real transfer: a move needs
 * scope over the source AND the destination, so the destination has to sit
 * inside the caller's own subtree.
 */
function manifest(): IntercomOrganizationManifest {
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: SLUG,
    name: 'Transfer Gate',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'engineering', 'platform', 'research'],
    peopleOrder: ['ceo', PAT, RHEA, MIRA, 'dana'],
    departments: {
      executive: {
        id: 'executive',
        name: 'Transfer Gate',
        purpose: 'Run the company.',
        headPersonId: 'ceo',
        state: 'active'
      },
      engineering: {
        id: 'engineering',
        name: 'Engineering',
        purpose: 'Build the product.',
        parentDepartmentId: 'executive',
        headPersonId: PAT,
        state: 'active'
      },
      platform: {
        id: 'platform',
        name: 'Platform',
        purpose: 'Own the runtime.',
        parentDepartmentId: 'engineering',
        headPersonId: 'dana',
        state: 'active'
      },
      research: {
        id: 'research',
        name: 'Research',
        purpose: 'Study the market.',
        parentDepartmentId: 'executive',
        headPersonId: RHEA,
        state: 'active'
      }
    },
    people: {
      ceo: person('ceo', 'executive', 'executive'),
      [PAT]: person(PAT, 'head', 'engineering'),
      [RHEA]: person(RHEA, 'head', 'research'),
      [MIRA]: person(MIRA, 'worker', 'engineering'),
      dana: person('dana', 'head', 'platform')
    }
  }
}

/** The same company with one row chiefd never writes: the head of
 *  `engineering` is recorded a worker. This is the ONLY state in which the
 *  deleted conjunct could have changed an answer, and it changed it the wrong
 *  way. */
function headRecordedAsWorker(): IntercomOrganizationManifest {
  const tree = manifest()
  tree.people[PAT] = person(PAT, 'worker', 'engineering')
  return tree
}

interface StubChiefd {
  readonly url: string
  readonly paths: string[]
  stop(): Promise<void>
}

/**
 * Serves the given manifest and REFUSES the transfer route on purpose.
 *
 * chiefd's own wording can only appear if the local pre-flight let the call
 * through, so the refusal is the instrument that proves the route was reached;
 * the recorded path says it a second way.
 */
async function startStubChiefd(tree: IntercomOrganizationManifest): Promise<StubChiefd> {
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
        response.end(JSON.stringify({ found: true, manifest: JSON.stringify(tree), seq: 1 }))
        return
      }
      if (path === '/v1/org/person/transfer') {
        // 422 is a refusal status on both sides of the contract, so the client
        // decodes `{refused, detail}` rather than reporting an outage.
        response.writeHead(422, { 'content-type': 'application/json' })
        response.end(
          JSON.stringify({ refused: 'stub-reached', detail: 'the transfer route was reached' })
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

interface TransferOutcome {
  /** False when the extension never offered this person the tool. */
  readonly registered: boolean
  readonly ok: boolean
  readonly message: string
  readonly chiefdPaths: readonly string[]
}

/** Install the extension AS `personId` against `tree`, and call `org_transfer`
 *  exactly as Pi's agent loop calls it. */
async function transfer(
  personId: string,
  tree: IntercomOrganizationManifest,
  params: Record<string, unknown>
): Promise<TransferOutcome> {
  const chiefd = await startStubChiefd(tree)
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
      ORG_LAUNCHER_ROOT: '/tmp/transfer-title-gate-removed/launcher'
    },
    pollIntervalMs: 0,
    turnWatchdogIntervalMs: 0,
    bootTransientRetryDelaysMs: [1, 1, 1]
  })

  const tool = tools.get('org_transfer')
  if (isNullish(tool)) {
    return { registered: false, ok: false, message: '', chiefdPaths: [...chiefd.paths] }
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
  )('tool-call-1', params, undefined, undefined, undefined)) as {
    content?: readonly { text?: string }[]
    details?: Record<string, unknown>
  }
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
  return {
    registered: true,
    ok: (raw.details ?? {}).ok === true,
    message: String(raw.content?.[0]?.text ?? ''),
    chiefdPaths: [...chiefd.paths]
  }
}

describe('the transfer pre-flight decides by scope alone', () => {
  it('a NON-MANAGER with scope reaches chiefd, the real authority', async () => {
    // Kind `worker`, heads `engineering`, moves a member of `engineering` into
    // `platform` beneath it. The deleted conjunct refused this person their own
    // subtree; scope admits them, and the call leaves the pane.
    const outcome = await transfer(PAT, headRecordedAsWorker(), {
      personId: MIRA,
      departmentId: 'platform'
    })
    expect(outcome.registered, 'the catalog grants this verb to every person').toBe(true)
    expect(outcome.message, 'a title must not refuse a caller who holds the subtree').not.toContain(
      'does not manage'
    )
    expect(outcome.message).toContain('the transfer route was reached')
    expect(
      outcome.chiefdPaths,
      'the decision belongs to chiefd; the pre-flight must reach it'
    ).toContain('/v1/org/person/transfer')
    expect(outcome.ok).toBe(false)
  }, 20_000)

  it('a MANAGER without scope is refused, and posts nothing', async () => {
    // Kind `head`, so every title question there is answers yes — and `mira`
    // lives in a peer subtree. Reaching sideways is the one direction the tree
    // model forbids, and scope alone is what stops it.
    const outcome = await transfer(RHEA, manifest(), {
      personId: MIRA,
      departmentId: 'research'
    })
    expect(outcome.registered).toBe(true)
    expect(outcome.ok).toBe(false)
    expect(outcome.message).toContain(`'${RHEA}' does not manage person '${MIRA}'`)
    expect(outcome.chiefdPaths, 'a refused pre-flight must post no mutation').not.toContain(
      '/v1/org/person/transfer'
    )
  }, 20_000)

  it('the DESTINATION check is untouched: scope over the source is not enough', async () => {
    // Deleting the title conjunct must not soften the second half. `pat` heads
    // `engineering` and manages `mira`, and `research` is outside that subtree.
    const outcome = await transfer(PAT, manifest(), {
      personId: MIRA,
      departmentId: 'research'
    })
    expect(outcome.registered).toBe(true)
    expect(outcome.ok).toBe(false)
    expect(outcome.message).toContain(
      `Permanent transfer target 'research' is outside '${PAT}' management scope`
    )
    expect(outcome.chiefdPaths).not.toContain('/v1/org/person/transfer')
  }, 20_000)

  it('an unknown destination is still reported as unknown, never as authority', async () => {
    const outcome = await transfer(PAT, manifest(), {
      personId: MIRA,
      departmentId: 'no-such-department'
    })
    expect(outcome.ok).toBe(false)
    expect(outcome.message).toContain("Unknown department 'no-such-department'")
    expect(outcome.chiefdPaths).not.toContain('/v1/org/person/transfer')
  }, 20_000)
})

describe('the deleted conjunct decided nothing', () => {
  it('scope admits the head even when the manifest records them a worker', () => {
    const tree = headRecordedAsWorker()
    expect(departmentScopeDenial(tree, personOf(tree, PAT), 'engineering')).toBeUndefined()
    expect(departmentScopeDenial(tree, personOf(tree, PAT), 'platform')).toBeUndefined()
  })

  it('scope alone refuses a peer head and a member, with no help from a title', () => {
    const tree = manifest()
    expect(departmentScopeDenial(tree, personOf(tree, RHEA), 'engineering')).toBe('out-of-scope')
    expect(departmentScopeDenial(tree, personOf(tree, MIRA), 'engineering')).toBe('out-of-scope')
  })

  it('the transfer pre-flight consults no job title at all', () => {
    // Coupled to production source text on purpose, exactly as
    // `ManagerRoleGateRemoved.test.ts` couples the department pre-flight: the
    // ban is on a SHAPE, and a shape has no export to assert against.
    const source = withoutComments(readFileSync(SOURCE_PATH, 'utf8'))
    const start = source.indexOf('export async function executeAtomicPersonTransfer(')
    expect(start, 'executeAtomicPersonTransfer must still exist').toBeGreaterThan(-1)
    const body = source.slice(start, source.indexOf('\n}', start))
    expect(body).toContain('departmentIsInScope')
    expect(body).toContain('departmentScopeDenial')
    expect(body, 'a title check must not come back into the transfer gate').not.toContain(
      'manager(managerPerson)'
    )
  })
})
