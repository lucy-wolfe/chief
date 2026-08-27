/**
 * Install the REAL `organization-intercom` extension and capture every tool it
 * registers, with no chiefd binary and no company on disk.
 *
 * # Why this exists beside `OrganizationToolSurface`
 *
 * `OrganizationToolSurface` boots a real `chiefd run` against a real company
 * and is the right harness for asserting what a tool DOES. This one asserts
 * what a tool IS — its declared schema — which needs no daemon and must be
 * runnable on a checkout that has never been compiled. A schema defect that
 * only a built product can catch is a schema defect nobody catches.
 *
 * Everything that decides WHICH tools register is real: the extension module,
 * the manifest it reads over HTTP, `currentPerson`, and the manager/executive
 * gates. Only the server is a fixture — one chiefd answering the boot reads —
 * and it speaks the production wire to the production clients. The company's
 * own directory is real, with a real rendezvous in it: that is how the
 * extension finds the daemon, so faking it would fake the subject.
 */
import type { Server } from 'node:http'
import { createServer } from 'node:http'

import { createCompanyDirectory } from '@test/support/CompanyRendezvous'
import { isNullish } from '@test/support/Nullish'
import type {
  CapturedRenderer,
  CapturedTool,
  StubbedRoute,
  ToolRegistrationCapture,
  ToolRegistrationOptions
} from '@test/types/ToolRegistrationHarness'
import { installOrganizationIntercom } from '@test-assets/organization-intercom'

const SLUG = 'toolschema'

/** A one-department company whose CEO is a root executive, so every gated tool
 *  family registers — the manager set, the root-executive set, and the
 *  baseline. A worker-only fixture would silently skip most of the catalog. */
function manifest(extraPeople: Readonly<Record<string, unknown>> = {}): Record<string, unknown> {
  const createdAt = '2026-01-01T00:00:00.000Z'
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: SLUG,
    name: 'Tool Schema',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive'],
    peopleOrder: ['ceo', ...Object.keys(extraPeople)],
    departments: {
      executive: {
        id: 'executive',
        name: 'Executive',
        purpose: 'Run the company.',
        headPersonId: 'ceo',
        state: 'active'
      }
    },
    people: {
      ceo: {
        id: 'ceo',
        name: 'Cleo',
        title: 'CEO',
        kind: 'executive',
        departmentId: 'executive',
        employmentState: 'active',
        createdAt
      },
      ...extraPeople
    }
  }
}

/**
 * A chiefd that answers the boot reads and records the rest.
 *
 * Deliberately permissive: an unrecognized route answers `{found:false}`
 * rather than 404, because this harness is about tool DECLARATION and an
 * absent document is a legal state the extension already handles. Anything it
 * cannot answer shows up as a recorded path, not as a silent success.
 */
async function startBootChiefd(
  options: ToolRegistrationOptions
): Promise<{ url: string; paths: string[]; stop(): Promise<void> }> {
  const paths: string[] = []
  // How many times each path has answered, so a stub can hand back a
  // DIFFERENT row per call. A managed fanout queues one request per target
  // and the durable-record gate checks each row against its own target, so a
  // single canned body makes any multi-target test refuse — which reads as a
  // product fault and is a fixture limit.
  const served = new Map<string, number>()
  const server: Server = createServer((request, response) => {
    const chunks: Buffer[] = []
    request.on('data', (chunk: Buffer) => chunks.push(chunk))
    request.on('end', () => {
      const path = (request.url ?? '').split('?')[0] ?? ''
      paths.push(path)
      /* eslint-disable lucy/no-json-stringify */
      // The replacement
      // helper is private to a sibling repo and is not a dependency here.
      const configured = options.routes?.[path]
      const call = served.get(path) ?? 0
      served.set(path, call + 1)
      const stubbed: StubbedRoute | undefined =
        typeof configured === 'function'
          ? configured(Buffer.concat(chunks).toString('utf8'), call)
          : Array.isArray(configured)
            ? configured[Math.min(call, configured.length - 1)]
            : configured
      const body =
        path === '/v1/org/manifest/read'
          ? JSON.stringify({
              found: true,
              manifest: JSON.stringify(manifest(options.people ?? {})),
              seq: 1
            })
          : JSON.stringify({ found: false, seq: 0 })
      /* eslint-enable lucy/no-json-stringify */
      response.writeHead(stubbed?.status ?? 200, { 'content-type': 'application/json' })
      response.end(stubbed?.body ?? body)
    })
  })
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (typeof address === 'string' || isNullish(address)) {
    throw new Error('the boot chiefd fixture did not bind a port')
  }
  return {
    url: `http://127.0.0.1:${address.port}`,
    paths,
    stop: () => new Promise<void>((resolve) => server.close(() => resolve()))
  }
}

/**
 * Install the extension for the CEO and hand back every `registerTool` call.
 *
 * Every background timer is disabled (`0`), exactly as the tool-contract
 * fixture disables them: a supervision cycle or an SSE reader firing under a
 * declaration assertion would make the result depend on wall time.
 */
export async function captureRegisteredTools(
  options: ToolRegistrationOptions = {}
): Promise<ToolRegistrationCapture> {
  const chiefd = await startBootChiefd(options)
  const company = createCompanyDirectory(chiefd.url, 'tool-registration-harness-')
  const tools: CapturedTool[] = []
  const renderers = new Map<string, CapturedRenderer>()
  const entryRenderers = new Map<string, CapturedRenderer>()

  const recorder = {
    registerTool(definition: CapturedTool) {
      tools.push(definition)
    },
    // Captured rather than discarded. A renderer is the only place some
    // operator-facing text exists, and the card that rendered the literal word
    // `undefined` for a whole action was invisible to every test in this
    // repository until this recorder kept them.
    registerMessageRenderer(customType: string, render: CapturedRenderer) {
      renderers.set(customType, render)
    },
    // Kept for the same reason, and separately: a custom ENTRY is the delivery
    // a card uses when it must reach a pane whose next turn cannot run, so it
    // is a different registry with a different guarantee.
    registerEntryRenderer(customType: string, render: CapturedRenderer) {
      entryRenderers.set(customType, render)
    },
    appendEntry() {
      /* what is appended is `InstalledPane`'s subject, not this harness's */
    },
    on() {
      /* no lifecycle is delivered: a declaration needs no session */
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
  // reasoning `OrganizationToolSurface.ts` records for its own recorder.
  const pi = recorder as never
  /* eslint-enable @typescript-eslint/consistent-type-assertions */

  await installOrganizationIntercom(pi, {
    environment: {
      ORG_LAUNCHER_IDENTITY_DIR: `${company.dir}/.chief`,
      ORG_LAUNCHER_ORG_DIR: company.dir,
      ORG_LAUNCHER_ORGANIZATION: SLUG,
      ORG_LAUNCHER_PERSON: options.personId ?? 'ceo',
      ORG_LAUNCHER_ROOT: '/tmp/tool-registration-harness/launcher'
    },
    pollIntervalMs: 0,
    turnWatchdogIntervalMs: 0,
    bootTransientRetryDelaysMs: [1, 1, 1]
  })

  return {
    tools,
    renderers,
    entryRenderers,
    chiefdPaths: chiefd.paths,
    stop: async () => {
      company.remove()
      await chiefd.stop()
    }
  }
}
