// #776: byte-exact route-path freeze. This test invokes ChiefdClient methods
// against a RecordingTransport and asserts the path each one actually
// posted/got equals its `route-table.json` entry -- so a client method that
// starts dialing a DIFFERENT (but real) route is a loud diff here.
//
// #751/G6 removed this file's second, unearned job. Its header used to say
// the table was "built by reading the real Rust router directly", which was
// true once and then quietly stopped being true: the three
// `/v1/org/company-removal/*` rows survived E7-S7's server-side deletion by
// an entire epic, green the whole time, because a RecordingTransport answers
// anything and nothing here ever consulted the router. Route EXISTENCE is now
// derived, not frozen -- see `RouteTableDerivation.test.ts`, which parses the
// Rust `.route("...")` literals at test time and covers every path the
// chiefing source dials, including the ~85 belonging to clients this file
// never reached.
//
// The RecordingTransport records `{method, path, body}` synchronously,
// BEFORE the caller-supplied responder ever runs -- so a call is captured
// even when the client throws afterward while decoding a fixture response
// that doesn't fully satisfy its parser. Every invocation below is wrapped
// in `.catch(() => {})` for exactly that reason: this test asserts the path
// that was DIALED, never the shape of what came back (that is RowsContract/
// TasksContract/etc.'s job against the real binary).
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { RecordingTransport } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { ChiefdClient } from '@/ChiefdClient'
import type { HttpResponse } from '@/types/Transport'

const HERE = dirname(fileURLToPath(import.meta.url))
const ROUTE_TABLE: Record<string, string> = JSON.parse(
  readFileSync(join(HERE, '..', 'fixtures', 'route-table.json'), 'utf8')
)

const routeTableEntries = Object.entries(ROUTE_TABLE).filter(([key]) => key !== '_comment')

// Non-vacuousness: the table itself must be non-trivial, so a bug that
// empties it (or a JSON.parse silently returning {}) fails loudly here
// rather than every `it.each` below passing on zero iterations.
// 96 -> 84: the goal, assignment, supervision-command, task and memory route
// families are deleted, so the table genuinely shrank by twelve. 84 -> 82: the
// loan concept is deleted and took `/v1/org/person/loan` and
// `/v1/org/person/return` with it. 82 -> 64: the publisher-route sweep deleted
// eighteen client methods whose routes no caller of any kind ever posted.
// 64 -> 55: provider/model management is deleted, taking the two model-change
// clients, the two staffing previews, the provider-models list and available
// reads, the two runtime switches, the runtime-preference write and the
// model-selection materialize with it. The floor tracks that; a table that
// empties still fails loudly here.
if (routeTableEntries.length < 55) {
  throw new Error(
    `route-table.json unexpectedly has only ${routeTableEntries.length} entries -- expected 55+`
  )
}

function okResponse(body: unknown = {}): HttpResponse {
  /* eslint-disable lucy/no-json-stringify */
  // Test-only fixture encoder -- @tribes-terminal/foundation is not a
  // dependency anywhere in this workspace (see FetchTransportTest.test.ts's
  // matching disable block).
  return { status: 200, body: JSON.stringify(body) }
  /* eslint-enable lucy/no-json-stringify */
}

const REQUESTER = { kind: 'person' as const, personId: 'person-1' }

const PERSON_SEED = {
  name: 'Ada',
  title: 'Engineer',
  mandate: 'Ship things',
  kind: 'worker' as const,
  employmentState: 'active' as const,
  activation: 'resident' as const,
  tools: [],
  prompts: []
}

describe('RoutePathFreeze (#776): every ChiefdClient method dials its frozen path', () => {
  function client(responder: (call: { path: string }) => HttpResponse): {
    client: ChiefdClient
    transport: RecordingTransport
  } {
    const transport = new RecordingTransport(responder)
    return { client: new ChiefdClient({ url: 'http://x', transport }), transport }
  }

  async function assertDialed(
    key: string,
    invoke: (c: ChiefdClient) => Promise<unknown>
  ): Promise<void> {
    const expectedPath = ROUTE_TABLE[key]
    expect(expectedPath, `route-table.json has no entry for "${key}"`).toBeDefined()
    const { client: c, transport } = client(() => okResponse())
    await invoke(c).catch(() => {})
    expect(transport.calls.length, `"${key}" recorded no call at all`).toBeGreaterThan(0)
    expect(transport.calls[0]?.path).toBe(expectedPath)
  }

  it('docs.ensureSchema', () => assertDialed('docs.ensureSchema', (c) => c.docs.ensureSchema()))
  it('docs.health', () => assertDialed('docs.health', (c) => c.docs.health()))
  it('docs.reachable', () => assertDialed('docs.reachable', (c) => c.docs.reachable()))
  it('docs.probe', () => assertDialed('docs.probe', (c) => c.docs.probe()))

  it('reminders.armReminder', () =>
    assertDialed('reminders.armReminder', (c) =>
      c.reminders.armReminder({
        slug: 'acme',
        personId: 'p1',
        prompt: 'hi',
        intervalMs: 60_000
      })
    ))
  it('reminders.listReminders', () =>
    assertDialed('reminders.listReminders', (c) =>
      c.reminders.listReminders({ slug: 'acme', personId: 'p1' })
    ))
  it('reminders.stopReminder', () =>
    assertDialed('reminders.stopReminder', (c) =>
      c.reminders.stopReminder({ slug: 'acme', personId: 'p1', reminderId: 'r1' })
    ))

  it('manifest.read', () => assertDialed('manifest.read', (c) => c.manifest.read('acme')))
  it('manifest.genesis', () =>
    assertDialed('manifest.genesis', (c) =>
      c.manifest.genesis('acme', { name: 'Acme', purpose: 'p', chief: { name: 'Chief' } })
    ))
  it('apiHostLaunchProfile.read', () =>
    assertDialed('apiHostLaunchProfile.read', (c) => c.apiHostLaunchProfile.read('acme')))

  it('personContracts.read', () =>
    assertDialed('personContracts.read', (c) => c.personContracts.read('acme')))
  it('mailbox.read', () => assertDialed('mailbox.read', (c) => c.mailbox.read('acme')))
  it('mailbox.readPerson', () =>
    assertDialed('mailbox.readPerson', (c) => c.mailbox.readPerson('acme', 'p1')))
  it('mailbox.delta', () =>
    assertDialed('mailbox.delta', (c) =>
      c.mailbox.delta('acme', 'p1', '{}', [], '2026-08-04T00:00:00.000Z')
    ))
  it('mailbox.listPersons', () =>
    assertDialed('mailbox.listPersons', (c) => c.mailbox.listPersons('acme')))

  it('aggregates.activityRead', () =>
    assertDialed('aggregates.activityRead', (c) => c.aggregates.activityRead('acme')))
  it('aggregates.supervisionRead', () =>
    assertDialed('aggregates.supervisionRead', (c) => c.aggregates.supervisionRead('acme')))
  it('aggregates.sessionMaintenanceRead', () =>
    assertDialed('aggregates.sessionMaintenanceRead', (c) =>
      c.aggregates.sessionMaintenanceRead('acme')
    ))

  // rows.* — each a thin read/publish/clear/insert over one shared helper;
  // exercised generically rather than repeating the same shape once per
  // method. The publish half of most stores is deleted with its route (the
  // publisher-route sweep found no caller), so the reads outnumber the
  // writes here now.
  const rowsInvocations: Array<[string, (c: ChiefdClient) => Promise<unknown>]> = [
    ['rows.readSessionEpoch', (c) => c.rows.readSessionEpoch('acme')],
    ['rows.readOperatorEscalationPush', (c) => c.rows.readOperatorEscalationPush('acme')],
    ['rows.readRuntimeOwner', (c) => c.rows.readRuntimeOwner('acme')],
    ['rows.readLaunchIntent', (c) => c.rows.readLaunchIntent('acme')],
    ['rows.clearLaunchIntent', (c) => c.rows.clearLaunchIntent('acme')],
    ['rows.readMutationJournal', (c) => c.rows.readMutationJournal('acme')],
    ['rows.readHealthMonitor', (c) => c.rows.readHealthMonitor('acme')],
    ['rows.readRuntime', (c) => c.rows.readRuntime('acme')],
    ['rows.publishRuntime', (c) => c.rows.publishRuntime('acme', {})],
    ['rows.clearRuntime', (c) => c.rows.clearRuntime('acme')],
    ['rows.readConvergeSafety', (c) => c.rows.readConvergeSafety('acme')],
    ['rows.readOperatorEscalationIntents', (c) => c.rows.readOperatorEscalationIntents('acme')],
    [
      'rows.insertOperatorEscalationIntent',
      (c) => c.rows.insertOperatorEscalationIntent('acme', {})
    ],
    ['rows.readEventOnceMarker', (c) => c.rows.readEventOnceMarker('acme', 'digest')],
    [
      'rows.insertEventOnceMarker',
      (c) =>
        c.rows.insertEventOnceMarker('acme', {
          keyDigest: 'digest',
          id: 'id-1',
          event: {},
          createdAtMs: 0
        })
    ],
    ['rows.pruneEventOnceMarkers', (c) => c.rows.pruneEventOnceMarkers('acme', 0)]
  ]
  it.each(rowsInvocations)('%s', async (key, invoke) => {
    await assertDialed(key, invoke)
  })

  it('staffing.startPerson', () =>
    assertDialed('staffing.startPerson', (c) => c.staffing.startPerson('acme', 'p1')))
  it('staffing.shutdownPerson', () =>
    assertDialed('staffing.shutdownPerson', (c) =>
      c.staffing.shutdownPerson('acme', 'p1', 'commanded')
    ))
  it('staffing.hirePerson', () =>
    assertDialed('staffing.hirePerson', (c) =>
      c.staffing.hirePerson('acme', 'p1', 'd1', PERSON_SEED, REQUESTER)
    ))
  it('staffing.offboardPerson', () =>
    assertDialed('staffing.offboardPerson', (c) => c.staffing.offboardPerson('acme', 'p1')))
  it('staffing.benchPerson', () =>
    assertDialed('staffing.benchPerson', (c) => c.staffing.benchPerson('acme', 'p1')))
  it('staffing.benchPersonLifecycle', () =>
    assertDialed('staffing.benchPersonLifecycle', (c) =>
      c.staffing.benchPersonLifecycle('acme', 'p1')
    ))
  it('staffing.recallPerson', () =>
    assertDialed('staffing.recallPerson', (c) => c.staffing.recallPerson('acme', 'p1')))
  it('staffing.transferPerson', () =>
    assertDialed('staffing.transferPerson', (c) => c.staffing.transferPerson('acme', 'p1', 'd2')))
  it('staffing.appointDepartmentHead', () =>
    assertDialed('staffing.appointDepartmentHead', (c) =>
      c.staffing.appointDepartmentHead('acme', 'd1', 'p1')
    ))
  it('staffing.replaceHeadAndOffboard', () =>
    assertDialed('staffing.replaceHeadAndOffboard', (c) =>
      c.staffing.replaceHeadAndOffboard('acme', 'p1', 'p2')
    ))
  it('staffing.createDepartment', () =>
    assertDialed('staffing.createDepartment', (c) =>
      c.staffing.createDepartment(
        'acme',
        'd1',
        'root',
        'Eng',
        { kind: 'appoint-existing', personId: 'p1' },
        REQUESTER
      )
    ))
  it('staffing.reparentDepartment', () =>
    assertDialed('staffing.reparentDepartment', (c) =>
      c.staffing.reparentDepartment('acme', 'd1', 'd2')
    ))
  it('staffing.pauseDepartment', () =>
    assertDialed('staffing.pauseDepartment', (c) => c.staffing.pauseDepartment('acme', 'd1')))
  it('staffing.resumeDepartment', () =>
    assertDialed('staffing.resumeDepartment', (c) => c.staffing.resumeDepartment('acme', 'd1')))
  it('staffing.resumeDepartments', () =>
    assertDialed('staffing.resumeDepartments', (c) => c.staffing.resumeDepartments('acme', ['d1'])))
  it('staffing.moveDepartmentMembers', () =>
    assertDialed('staffing.moveDepartmentMembers', (c) =>
      c.staffing.moveDepartmentMembers('acme', 'd1', 'd2', ['p1'])
    ))
  it('staffing.removeDepartmentTree', () =>
    assertDialed('staffing.removeDepartmentTree', (c) =>
      c.staffing.removeDepartmentTree('acme', 'd1')
    ))
  it('staffing.reactivateExecutiveRoot', () =>
    assertDialed('staffing.reactivateExecutiveRoot', (c) =>
      c.staffing.reactivateExecutiveRoot('acme')
    ))
  it('auth.challenge / auth.token', async () => {
    const transport = new RecordingTransport(() => okResponse({ nonceId: 'n', nonce: 'n' }))
    const c = new ChiefdClient({ url: 'http://x', transport })
    const manager = c.auth.tokenManager('person-1', 'not-a-real-key')
    await manager.acquire().catch(() => {})
    expect(transport.calls[0]?.path).toBe(ROUTE_TABLE['auth.challenge'])
  })
})
