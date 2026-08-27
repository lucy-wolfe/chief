/**
 * Install the REAL `organization-intercom` extension against a stub chiefd and
 * hand back the `agent_end` handler Pi's own loop would call.
 *
 * # Why this is a harness and not a fixture inside one test
 *
 * Two separate defects live behind this one handler — the consecutive-failure
 * counter and the delivery of the failure card — and each is invisible to the
 * other's assertions. They need the same company, the same install, and the
 * same driving; they need different things read back. So the driving is here
 * and the reading is in the tests.
 *
 * Everything that decides behavior is real: the extension module, the manifest
 * it reads over HTTP, and the durable event trail it writes to
 * `.chief/bus/events.jsonl`, read back off disk. Only the daemon is a fixture.
 *
 * # The recorder keeps renderers, deliberately
 *
 * An earlier recorder registered renderers into a no-op, and that hole is
 * exactly the shape of the bug this harness now exists to catch: a card whose
 * CONTENT was always right and whose DELIVERY was not passes every assertion
 * made against a discarded renderer. So the renderer registry is real and
 * {@link Pane.render} runs it.
 */
import { readFileSync } from 'node:fs'
import type { Server } from 'node:http'
import { createServer } from 'node:http'
import { join } from 'node:path'

import { createCompanyDirectory } from '@test/support/CompanyRendezvous'
import { isNullish } from '@test/support/Nullish'
import type {
  Delivery,
  OrganizationEvent,
  Pane,
  PaneEntry,
  PlainCardTheme
} from '@test/types/InstalledPane'
import { installOrganizationIntercom, MESSAGE_TYPE } from '@test-assets/organization-intercom'

/** The exact 400 the operator's company produced, on 2026-08-18. 233817 tokens
 *  of OUTPUT reservation against a 262144 window: it overflows by 31 tokens and
 *  it will be rejected identically for ever, on every retry, by every provider
 *  serving that model. */
export const CONTEXT_OVERFLOW =
  '400: {"message":"This endpoint\'s maximum context length is 262144 tokens. ' +
  'However, you requested about 262175 tokens (18355 of text input, 10003 of ' +
  'tool input, 233817 in the output)."}'

/** The genuine transient failure from the SAME ten minutes, on a different
 *  model. The whole point of the fix is that these two stopped being the same
 *  event, so both belong in the same harness. */
export const TRANSIENT = 'Connection error.'

const SLUG = 'providerfailcount'
const CEO = 'ceo'
const ENGINEER = 'head-of-engineering'

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
    name: 'Provider Fail Count',
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

function activityLedger(): Record<string, unknown> {
  return {
    schemaVersion: 1,
    organization: SLUG,
    personOrder: [CEO, ENGINEER],
    people: {
      [CEO]: { personId: CEO, lastDesiredActive: true },
      [ENGINEER]: { personId: ENGINEER, lastDesiredActive: true }
    },
    transitionOrder: [],
    transitions: {}
  }
}

interface StubChiefd {
  readonly url: string
  stop(): Promise<void>
  /** Put one row in a person's mailbox at `pending`, as a delivery would. */
  seed(person: string, envelope: Record<string, unknown>): void
  /** Every agent-activity beat this pane posted, in order. */
  beats(): ReadonlyArray<{ person: string; working: boolean }>
}

/** One mailbox row on the wire: the envelope, flattened, plus its state. */
type Row = Record<string, unknown> & { id: string; person: string; state: string }

/** Answers the reads the install and the escalation make, and accepts every
 *  write. Nothing here is asserted against — the durable event trail on disk
 *  and the appended entries are the authority, not the wire.
 *
 *  THE MAILBOX IS REAL, though, and it has to be: the defect this harness now
 *  also covers is that a receipted envelope is GONE. A stub that answered every
 *  mailbox read with "nothing here" could never show a message being consumed,
 *  so `/v1/org/mailbox/read-person` and `/v1/org/mailbox/delta` keep rows and
 *  apply state changes, which is the whole of what the acceptance path uses. */
async function startStubChiefd(): Promise<StubChiefd> {
  const rows: Row[] = []
  const beats: Array<{ person: string; working: boolean }> = []
  const server: Server = createServer((request, response) => {
    const chunks: Buffer[] = []
    request.on('data', (chunk: Buffer) => chunks.push(chunk))
    request.on('end', () => {
      const path = (request.url ?? '').split('?')[0] ?? ''
      const request_ = ((): Record<string, unknown> => {
        try {
          // Declared-type narrowing from JSON.parse's `any`, not an assertion.
          const parsed: Record<string, unknown> = JSON.parse(Buffer.concat(chunks).toString('utf8'))
          return parsed
        } catch {
          return {}
        }
      })()
      /* eslint-disable lucy/no-json-stringify */
      // The wire body two programs decode; the replacement helper is private to
      // a sibling repo and is not a dependency here (#833/#842).
      const body = ((): string => {
        if (path === '/v1/org/manifest/read') {
          return JSON.stringify({ found: true, manifest: JSON.stringify(manifest()), seq: 1 })
        }
        if (path === '/v1/org/activity/read') {
          return JSON.stringify({ found: true, seq: 1, ledger: JSON.stringify(activityLedger()) })
        }
        if (path === '/v1/org/activity/agent-state') {
          // THE SETTLE COUNTDOWN'S ONE INPUT. A test that wants to know whether
          // the countdown was cancelled asks what this pane REPORTED, which is
          // the only thing chiefd knows about it.
          beats.push({
            person: typeof request_.callerPersonId === 'string' ? request_.callerPersonId : '',
            working: request_.working === true
          })
          return JSON.stringify({ ok: true, seq: 1 })
        }
        if (path === '/v1/org/mailbox/read-person') {
          const person = typeof request_.personId === 'string' ? request_.personId : ''
          const entries = rows.filter((row) => row.person === person)
          if (entries.length === 0) return JSON.stringify({ found: false, seq: 0 })
          return JSON.stringify({ found: true, seq: 1, mailbox: JSON.stringify({ entries }) })
        }
        if (path === '/v1/org/mailbox/delta') {
          // Declared-type narrowing from JSON.parse's `any`, the same way
          // `Pane.events` reads the durable trail back.
          const upserts: readonly Row[] =
            typeof request_.upserts === 'string' ? JSON.parse(request_.upserts) : []
          const deletes: readonly string[] = Array.isArray(request_.deletes)
            ? request_.deletes.filter((entry): entry is string => typeof entry === 'string')
            : []
          for (const upsert of upserts) {
            const at = rows.findIndex((row) => row.id === upsert.id && row.person === upsert.person)
            if (at >= 0) rows.splice(at, 1, upsert)
            else rows.push(upsert)
          }
          for (const rowId of deletes) {
            const at = rows.findIndex((row) => `${row.id}@${row.person}` === rowId)
            if (at >= 0) rows.splice(at, 1)
          }
          return JSON.stringify({ applied: true, seq: 1 })
        }
        if (path === '/v1/org/supervision/read') {
          return JSON.stringify({
            found: true,
            seq: 1,
            ledger: JSON.stringify({
              schemaVersion: 1,
              organization: SLUG,
              assignmentOrder: [],
              assignments: {}
            })
          })
        }
        return JSON.stringify({ found: false, ok: true, seq: 0 })
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
    stop: () => new Promise<void>((resolve) => server.close(() => resolve())),
    seed: (person: string, envelope: Record<string, unknown>) => {
      rows.push({ ...envelope, id: String(envelope.id), person, state: 'pending' })
    },
    beats: () => beats
  }
}

const running: Array<{ chiefd: StubChiefd; company: { readonly dir: string; remove(): void } }> = []

/** Tear every pane this file started down. Call from `afterEach`. */
export async function stopInstalledPanes(): Promise<void> {
  const started = running.splice(0, running.length)
  for (const entry of started) {
    entry.company.remove()
    await entry.chiefd.stop()
  }
}

type Renderer = (entry: unknown, options: { expanded?: boolean }, theme: unknown) => unknown

function plainTheme(): PlainCardTheme {
  return {
    bold: (text: string) => text,
    fg: (_token: string, text: string) => text,
    bg: (_token: string, text: string) => text
  }
}

export async function installedPane(): Promise<Pane> {
  const chiefd = await startStubChiefd()
  const company = createCompanyDirectory(chiefd.url, 'piing-installed-pane-')
  running.push({ chiefd, company })

  const handlers = new Map<string, (event: unknown) => unknown>()
  const entryRenderers = new Map<string, Renderer>()
  const appended: PaneEntry[] = []
  const recorder = {
    registerTool() {
      /* the tool surface is not this harness's subject */
    },
    registerMessageRenderer() {
      /* custom MESSAGES are not how a pane-failure card is delivered */
    },
    registerEntryRenderer(customType: string, renderer: Renderer) {
      entryRenderers.set(customType, renderer)
    },
    appendEntry(customType: string, data: Record<string, unknown>) {
      appended.push({ customType, data })
    },
    on(name: string, handler: (event: unknown) => unknown) {
      handlers.set(name, handler)
    },
    sendMessage() {
      /* a card this harness reads is appended, never sent */
    },
    setThinkingLevel() {
      /* presentation only */
    },
    setModel() {
      /* presentation only */
    },
    ui: {
      notify() {
        /* presentation only */
      }
    }
  }
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Pi's `ExtensionAPI` is a large concrete class surface; the extension calls
  // exactly these members, which is what Pi's own loader hands it. Same
  // reasoning `support/ToolRegistrationHarness.ts` records for its recorder.
  const pi = recorder as never
  /* eslint-enable @typescript-eslint/consistent-type-assertions */

  await installOrganizationIntercom(pi, {
    environment: {
      ORG_LAUNCHER_IDENTITY_DIR: `${company.dir}/.chief`,
      ORG_LAUNCHER_ORG_DIR: company.dir,
      ORG_LAUNCHER_ORGANIZATION: SLUG,
      ORG_LAUNCHER_PERSON: ENGINEER,
      ORG_LAUNCHER_ROOT: '/tmp/provider-failure-counter/launcher'
    },
    pollIntervalMs: 0,
    turnWatchdogIntervalMs: 0,
    bootTransientRetryDelaysMs: [1, 1, 1]
  })

  const agentEnd = handlers.get('agent_end')
  if (isNullish(agentEnd)) {
    throw new Error(
      `agent_end is not registered. Registered: ${[...handlers.keys()].sort().join(', ')}`
    )
  }
  const beforeCompact = handlers.get('session_before_compact')
  const messageStart = handlers.get('message_start')
  if (isNullish(messageStart)) {
    throw new Error(
      `message_start is not registered. Registered: ${[...handlers.keys()].sort().join(', ')}`
    )
  }

  /** The envelope as chiefd stores it and as Pi hands it back. */
  const envelopeOf = (delivery: Delivery): Record<string, unknown> => ({
    schemaVersion: 1,
    id: delivery.id,
    organization: SLUG,
    fromPersonId: delivery.fromPersonId,
    to: ENGINEER,
    recipients: [ENGINEER],
    body: delivery.body,
    urgency: 'normal',
    createdAt: '2026-08-24T00:00:00.000Z'
  })

  return {
    deliver: async (...deliveries: readonly Delivery[]) => {
      if (deliveries.length === 0) throw new Error('a delivery-driven turn needs a delivery')
      const envelopes = deliveries.map(envelopeOf)
      for (const envelope of envelopes) chiefd.seed(ENGINEER, envelope)
      // One envelope arrives on its own; several arrive as the batch card. Both
      // are shapes Pi really produces, and the batch id is content-addressed —
      // the extension recomputes it and refuses a mismatch, so it is built the
      // same way here rather than invented.
      const details =
        envelopes.length === 1
          ? envelopes[0]
          : {
              schemaVersion: 1,
              batchId: `organization-mailbox-batch-${deliveries.map((one) => one.id).join('+')}`,
              envelopes
            }
      await messageStart({ message: { customType: MESSAGE_TYPE, details } })
    },
    endTurn: async (errorMessage: string) => {
      // Pi's `agent_end` shape, reduced to the fields the handler reads.
      await agentEnd({
        messages: [{ role: 'assistant', stopReason: 'error', content: [], errorMessage }]
      })
    },
    beginCompaction: async () => {
      if (isNullish(beforeCompact)) {
        throw new Error(
          'session_before_compact is not registered — the compaction beat is the whole ' +
            `feature. Registered: ${[...handlers.keys()].sort().join(', ')}`
        )
      }
      await beforeCompact({})
    },
    endTurnWithToolCall: async (errorMessage: string) => {
      // The same failed turn, except that it had already reached a tool call —
      // which is what `hadToolCall` reads and what decides whether a sender can
      // safely resend.
      await agentEnd({
        messages: [
          {
            role: 'assistant',
            stopReason: 'error',
            content: [{ type: 'toolCall', toolName: 'org_send' }],
            errorMessage
          }
        ]
      })
    },
    completeTurn: async () => {
      await agentEnd({ messages: [{ role: 'assistant', stopReason: 'stop', content: [] }] })
    },
    events: () => {
      const path = join(company.dir, '.chief', 'bus', 'events.jsonl')
      let raw: string
      try {
        raw = readFileSync(path, 'utf8')
      } catch {
        return []
      }
      return raw
        .split('\n')
        .filter((line) => line.trim().length > 0)
        .map((line): OrganizationEvent => {
          // Declared-type narrowing from JSON.parse's `any`, not a type
          // assertion (`assertionStyle: 'never'` forbids `as` here).
          const parsed: OrganizationEvent = JSON.parse(line)
          return parsed
        })
    },
    beats: () => chiefd.beats(),
    entries: () => appended,
    render: (entry: PaneEntry): string => {
      const renderer = entryRenderers.get(entry.customType)
      if (isNullish(renderer)) {
        throw new Error(
          `no entry renderer is registered for '${entry.customType}'; registered: ` +
            `${[...entryRenderers.keys()].sort().join(', ') || '(none)'}`
        )
      }
      const node = renderer(entry, { expanded: false }, plainTheme())
      if (isNullish(node)) throw new Error(`the renderer for '${entry.customType}' drew nothing`)
      /* eslint-disable @typescript-eslint/consistent-type-assertions */
      // Pi's `Component` is the renderer's declared return; this harness needs
      // only its one rendering method, the same way `CardStyle.test.ts` does.
      const component = node as { render(width: number): readonly string[] }
      /* eslint-enable @typescript-eslint/consistent-type-assertions */
      return component.render(80).join('\n')
    }
  }
}
