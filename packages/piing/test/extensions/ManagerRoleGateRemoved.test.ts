/**
 * A JOB TITLE STOOD IN AN AUTHORITY GATE, AND IT DECIDED NOTHING.
 *
 * `requireManagedDepartment` — the pre-flight for every verb that acts on a
 * department that already exists (launch-existing, stop, remove, and the
 * contract twins) — refused unless `manager(person)` AND the department was in
 * scope. `manager` reads `person.kind`, which is a TITLE, and the operator
 * ruling of 2026-08-13 (`AGENTS.md`) forbids exactly that: "authority is the
 * subtree you head, never the job title".
 *
 * The title half was dead twice over.
 *
 * 1. REDUNDANT. `departmentIsInScope` returns false for a non-executive who
 *    heads no department, so passing scope already implies heading one, and
 *    chiefd sets kind Head in the same transaction that makes anybody a head —
 *    the only two `set_department_head` sites (`org_ops.rs:1075` and `:3705`)
 *    each pair with a `set_person_kind(Head)`, a hired head's seed is validated
 *    as `Head` (`org_projection.rs:460`), and genesis writes `Executive` for
 *    the CEO. "Heads a unit while recorded a worker" is a state chiefd never
 *    writes.
 * 2. UNREACHABLE, WHEN THIS WAS WRITTEN. The whole structural family was
 *    registered inside `installManagerTools`, which ran only
 *    `if (manager(person))`, so a person of kind `worker` was never offered
 *    these tools at all and no caller who could reach the verb could fail the
 *    conjunct. That gate is now split — `installSubtreeTools` registers the
 *    catalog family for everybody — so a worker HOLDS these verbs today and is
 *    refused by scope instead. The last test here follows that change.
 *
 * What must NOT change is the check that does decide. Both refusal tests below
 * drive the REAL tool, so the refusal they assert is the one the product shows.
 * REAL: the extension module, its tool registration, the registered `execute`,
 * the production chiefd client and wire. FAKE: `pi` (a recorder) and one
 * loopback server standing in for this company's chiefd, and a real
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
  installOrganizationIntercom,
  personAuthority
} from '@test-assets/organization-intercom'
import { afterEach, describe, expect, it } from 'vitest'

const SLUG = 'rolegate'
const CREATED_AT = '2026-01-01T00:00:00.000Z'
const SOURCE_PATH = fileURLToPath(
  new URL('../../extensions/organization-intercom.ts', import.meta.url)
)
/** The head of `engineering`: in scope for its own department. */
const PAT = 'pat'
/** The head of `research`, a PEER. Scope must refuse them `engineering`, which
 *  is the direction the tree model forbids and the reason the check stays. */
const RHEA = 'rhea'
/** A member of `engineering` who heads nothing. */
const LEE = 'lee'

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

/** A CEO over two peer departments, each with its own head, plus one member
 *  who heads nothing. */
function manifest(): IntercomOrganizationManifest {
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: SLUG,
    name: 'Role Gate',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'engineering', 'research'],
    peopleOrder: ['ceo', PAT, RHEA, LEE],
    departments: {
      executive: {
        id: 'executive',
        name: 'Role Gate',
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
      [LEE]: person(LEE, 'worker', 'engineering')
    }
  }
}

/** The same company with one impossible row: the head of `engineering` is
 *  recorded as a worker. chiefd never writes this, and it is the ONLY state in
 *  which the deleted conjunct could have changed an answer. */
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
 * A chiefd that serves the manifest and REFUSES the pause route on purpose.
 *
 * The refusal is the assertion instrument: chiefd's own wording can only
 * appear if the local pre-flight let the call through, and the recorded path
 * says the same thing a second way.
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
      if (path === '/v1/org/department/pause') {
        // 422 is a refusal status on both sides of the contract, so the client
        // decodes `{refused, detail}` rather than reporting an outage.
        response.writeHead(422, { 'content-type': 'application/json' })
        response.end(
          JSON.stringify({ refused: 'stub-reached', detail: 'the pause route was reached' })
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

interface StopOutcome {
  /** False when the extension never offered this person the tool. */
  readonly registered: boolean
  readonly ok: boolean
  readonly message: string
  readonly chiefdPaths: readonly string[]
}

/** Install the extension AS `personId` and call `org_stop_department` on
 *  `engineering`, exactly as Pi's agent loop calls it. */
async function stopEngineering(personId: string): Promise<StopOutcome> {
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
      ORG_LAUNCHER_ROOT: '/tmp/manager-role-gate-removed/launcher'
    },
    pollIntervalMs: 0,
    turnWatchdogIntervalMs: 0,
    bootTransientRetryDelaysMs: [1, 1, 1]
  })

  const tool = tools.get('org_stop_department')
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
  )('tool-call-1', { unitId: 'engineering' }, undefined, undefined, undefined)) as {
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

describe('the department gate still decides by scope', () => {
  it('the head of the department reaches chiefd, the real authority', async () => {
    const outcome = await stopEngineering(PAT)
    expect(outcome.registered).toBe(true)
    expect(outcome.message, 'the head manages its own department').not.toContain(
      'does not manage department'
    )
    expect(outcome.message).toContain('the pause route was reached')
    expect(outcome.chiefdPaths).toContain('/v1/org/department/pause')
    expect(outcome.ok).toBe(false)
  }, 20_000)

  it('a peer head is refused, and posts no mutation', async () => {
    // Reaching sideways is the one direction the tree model forbids. Removing
    // the title check must not soften this by a word.
    const outcome = await stopEngineering(RHEA)
    expect(outcome.registered).toBe(true)
    expect(outcome.ok).toBe(false)
    expect(outcome.message).toContain(`'${RHEA}' does not manage department 'engineering'`)
    expect(outcome.chiefdPaths, 'a refused pre-flight must not post a mutation').not.toContain(
      '/v1/org/department/pause'
    )
  }, 20_000)

  it('a member who heads nothing HOLDS the verb and is refused by scope', async () => {
    // The subject of this assertion has not moved: a member who heads nothing
    // cannot stop a department. What changed underneath it is the MECHANISM.
    // When this suite was written, gate 1 withheld the whole structural family
    // from a worker's pane, so the refusal was "you were never offered the
    // verb" — a defect, recorded here at the time as a finding. Gate 1 now
    // registers the subtree family for every person, matching
    // `ORGANIZATION_SUBTREE_TOOL_NAMES`, so the refusal is the one it should
    // always have been: the tool is present, and SCOPE says no.
    const outcome = await stopEngineering(LEE)
    expect(outcome.registered, 'the catalog grants this verb to every person').toBe(true)
    expect(outcome.ok).toBe(false)
    expect(outcome.message).toContain(`'${LEE}' does not manage department 'engineering'`)
    expect(outcome.chiefdPaths, 'a refused pre-flight must post nothing').not.toContain(
      '/v1/org/department/pause'
    )
  }, 20_000)
})

describe('the deleted conjunct decided nothing', () => {
  it('scope admits the head even when the manifest records them a worker', () => {
    // The impossible state, answered by the gate that remains. Before the
    // change, the title check refused this person their OWN department.
    const tree = headRecordedAsWorker()
    expect(departmentScopeDenial(tree, personOf(tree, PAT), 'engineering')).toBeUndefined()
    expect(personAuthority(tree, personOf(tree, PAT)).headedDepartmentId).toBe('engineering')
  })

  it('scope alone refuses a member and a peer, with no help from a title', () => {
    const tree = manifest()
    expect(departmentScopeDenial(tree, personOf(tree, LEE), 'engineering')).toBe('out-of-scope')
    expect(personAuthority(tree, personOf(tree, LEE)).hireDepartmentIds).toEqual([])
    expect(departmentScopeDenial(tree, personOf(tree, RHEA), 'engineering')).toBe('out-of-scope')
    expect(personAuthority(tree, personOf(tree, RHEA)).hireDepartmentIds).toEqual(['research'])
  })

  it('the department pre-flight consults no job title at all', () => {
    const source = withoutComments(readFileSync(SOURCE_PATH, 'utf8'))
    const start = source.indexOf('async function requireManagedDepartment(')
    expect(start, 'requireManagedDepartment must still exist').toBeGreaterThan(-1)
    const body = source.slice(start, source.indexOf('\n}', start))
    expect(body).toContain('departmentIsInScope')
    expect(body, 'a title check must not come back into the department gate').not.toContain(
      'manager(person)'
    )
  })
})
