/**
 * THE ACCEPTANCE CASE: A WORKER HAS THE VERB, CALLS IT, AND REACHES CHIEFD.
 *
 * Registration was gate 1 of three, and it was the one nobody could see: the
 * authority layer allowed a leaf to create beneath itself, chiefd's create path
 * allowed it, and the pane never handed the model the tool. Having the tool is
 * only half the claim, so this suite CALLS it.
 *
 * What it proves, in order:
 *
 * 1. A `kind=worker` person homed in the executive root reaches
 *    `/v1/org/department/create` — the verb is present AND usable.
 * 2. Growth stays DOWNWARD ONLY. The same person is refused beneath a peer and
 *    beneath their own manager's unit, locally, with no route touched.
 * 3. The destructive verbs a worker newly HOLDS refuse until they head
 *    something. `org_stop_department` is registered for them and answers "does
 *    not manage" for a unit they do not head, posting nothing.
 *
 * Point 3 is the blast-radius answer: registration gave a worker the verbs, and
 * scope gave them nothing to use the verbs on. A tool present and then refused
 * by scope is the safety model working; a tool absent is the bug.
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

const SLUG = 'growsubtree'
const CREATED_AT = '2026-01-01T00:00:00.000Z'
/** The incident shape: general staff, homed in the executive ROOT, heads
 *  nothing. */
const CARLA = 'carla'
/** A worker one level down, used for the upward/sideways refusals. */
const LEE = 'lee'

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

/** A CEO at the root, a chief of staff homed beside them, and two peer
 *  departments — enough for "beneath me", "beside me" and "above me". */
function manifest(): Record<string, unknown> {
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: SLUG,
    name: 'Grow Subtree',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'engineering', 'research'],
    peopleOrder: ['ceo', CARLA, 'rhea', LEE],
    departments: {
      executive: {
        id: 'executive',
        name: 'Grow Subtree',
        purpose: 'Run the company.',
        headPersonId: 'ceo',
        state: 'active'
      },
      engineering: {
        id: 'engineering',
        name: 'Engineering',
        purpose: 'Build the product.',
        parentDepartmentId: 'executive',
        headPersonId: 'rhea',
        state: 'active'
      },
      research: {
        id: 'research',
        name: 'Research',
        purpose: 'Study the market.',
        parentDepartmentId: 'executive',
        headPersonId: 'ceo',
        state: 'active'
      }
    },
    people: {
      ceo: person('ceo', 'executive', 'executive'),
      [CARLA]: person(CARLA, 'worker', 'executive'),
      rhea: person('rhea', 'head', 'engineering'),
      [LEE]: person(LEE, 'worker', 'engineering')
    }
  }
}

interface StubChiefd {
  readonly url: string
  readonly paths: string[]
  stop(): Promise<void>
}

/**
 * Serves the manifest and REFUSES the create on purpose.
 *
 * chiefd's own wording can only appear if the local pre-flight let the call
 * through, so the refusal is the instrument that proves the tool was reached;
 * the recorded path says it a second way.
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
      if (path === '/v1/org/department/create' || path === '/v1/org/department/pause') {
        response.writeHead(422, { 'content-type': 'application/json' })
        response.end(
          JSON.stringify({ refused: 'stub-reached', detail: `the ${path} route was reached` })
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
  /** False when the extension never offered this person the tool. */
  readonly registered: boolean
  readonly ok: boolean
  readonly message: string
  readonly chiefdPaths: readonly string[]
}

/** Install the extension AS `personId` and call one tool, exactly as Pi's agent
 *  loop calls it. */
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
      ORG_LAUNCHER_ROOT: '/tmp/worker-grows-a-subtree/launcher'
    },
    pollIntervalMs: 0,
    turnWatchdogIntervalMs: 0,
    bootTransientRetryDelaysMs: [1, 1, 1]
  })

  const registered = tools.get(tool)
  if (isNullish(registered)) {
    return { registered: false, ok: false, message: '', chiefdPaths: [...chiefd.paths] }
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
  return {
    registered: true,
    ok: (raw.details ?? {}).ok === true,
    message: String(raw.content?.[0]?.text ?? ''),
    chiefdPaths: [...chiefd.paths]
  }
}

/**
 * The department a create names, as the tool's schema accepts it — and with the
 * head decision the incident's own remediation copy names: the caller appoints
 * THEMSELVES. That path also takes no model observation, so this fixture needs
 * no Pi session to reach the route.
 */
function newDepartment(headPersonId: string, parentDepartmentId?: string): Record<string, unknown> {
  return {
    ...(isNullish(parentDepartmentId) ? {} : { parentDepartmentId }),
    name: 'Chief Of Staff Office',
    purpose: 'The unit a leaf grows beneath itself.',
    existingHeadPersonId: headPersonId
  }
}

describe('a worker grows a subtree', () => {
  it('has org_add_department and reaches chiefd with it', async () => {
    // THE ACCEPTANCE CASE. Homed in the executive root, kind worker, heading
    // nothing — and the create now leaves the pane.
    const outcome = await callAs(CARLA, 'org_add_department', newDepartment(CARLA))
    expect(outcome.registered, 'a worker must be offered the verb at all').toBe(true)
    expect(outcome.message).not.toContain('does not manage')
    expect(outcome.message).not.toContain('may create a department beneath')
    expect(
      outcome.chiefdPaths,
      'the create must reach chiefd, which is the authority that decides it'
    ).toContain('/v1/org/department/create')
  }, 20_000)

  it('is refused SIDEWAYS at a peer department, locally, with nothing posted', async () => {
    // `lee` works in engineering and heads nothing: their authority root is the
    // unit they sit in, and `research` is neither it nor under it.
    const outcome = await callAs(LEE, 'org_add_department', newDepartment(LEE, 'research'))
    expect(outcome.registered).toBe(true)
    expect(outcome.ok).toBe(false)
    expect(outcome.message).toContain(`'${LEE}' may create a department beneath`)
    expect(outcome.chiefdPaths, 'a refused pre-flight must post nothing').not.toContain(
      '/v1/org/department/create'
    )
  }, 20_000)

  it('is refused UPWARD at its own manager, locally, with nothing posted', async () => {
    // The one direction the tree model forbids. `lee` sits in engineering, so
    // `executive` is above them.
    const outcome = await callAs(LEE, 'org_add_department', newDepartment(LEE, 'executive'))
    expect(outcome.registered).toBe(true)
    expect(outcome.ok).toBe(false)
    expect(outcome.message).toContain(`'${LEE}' may create a department beneath`)
    expect(outcome.chiefdPaths).not.toContain('/v1/org/department/create')
  }, 20_000)
})

describe('what a worker newly HOLDS, it still cannot use out of scope', () => {
  it('org_stop_department is registered and refuses a unit the worker does not head', async () => {
    // The blast-radius answer for the destructive half of the catalog:
    // registration hands over the verb, scope hands over nothing to use it on.
    // A worker who heads no unit can stop none.
    const outcome = await callAs(CARLA, 'org_stop_department', { unitId: 'engineering' })
    expect(outcome.registered, 'the catalog grants this verb to every person').toBe(true)
    expect(outcome.ok).toBe(false)
    expect(outcome.message).toContain(`'${CARLA}' does not manage department 'engineering'`)
    expect(outcome.chiefdPaths, 'a refused stop must post nothing').not.toContain(
      '/v1/org/department/pause'
    )
  }, 20_000)

  it('org_offboard is registered and refuses a person outside the worker subtree', async () => {
    const outcome = await callAs(CARLA, 'org_offboard', {
      personId: LEE
    })
    expect(outcome.registered).toBe(true)
    expect(outcome.ok).toBe(false)
    expect(outcome.message).toContain(`does not manage person '${LEE}'`)
  }, 20_000)
})
