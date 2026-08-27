// E2-S5 — StaffingClient: table-driven over the client's methods. Every case asserts:
// the exact route posted, a 2xx success shape decode, a 422 `{code, detail}`
// refusal decoded AS A VALUE with exactly one request recorded (no retry),
// a 500 non-refusal throwing ChiefdUnavailableError, and a malformed 2xx
// body throwing kind 'malformed-body'. Dedicated cases below the table cover
// the shapes the table can't: hirePerson, transferPerson,
// removeDepartmentTree, createDepartment.

import { jsonResponse, RecordingTransport, textResponse } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { ChiefdUnavailableError, OrgRowRefusalError } from '@/Errors'
import { StaffingClient } from '@/resources/Staffing'
import type { AtomicPersonSeed } from '@/types/Staffing'

interface DirectCase {
  name: string
  path: string
  successBody: unknown
  call: (staffing: StaffingClient) => Promise<unknown>
  // The EXACT request body this table's `call` is expected to post — every
  // key, nothing more. Checked with `toStrictEqual`, not `toMatchObject` or
  // `toEqual` (which treats an own `undefined` key as absent),
  // so an extra key (an optional field leaking onto the wire when its
  // caller-side value was never supplied) fails this test too, not only a
  // missing one (#881: this table asserted route+result shape for all 15
  // verbs and never inspected `.body` at all — a builder regression that
  // added or dropped a field was invisible here no matter what it did).
  expectedBody: Record<string, unknown>
}

const DIRECT_CASES: DirectCase[] = [
  {
    name: 'shutdownPerson',
    path: '/v1/org/person/shutdown',
    successBody: { applied: true, transitionId: 'tr-1' },
    call: (s) => s.shutdownPerson('acme', 'p1', 'commanded'),
    expectedBody: { slug: 'acme', personId: 'p1', kind: 'commanded' }
  },
  {
    name: 'offboardPerson',
    path: '/v1/org/person/offboard',
    successBody: { applied: true },
    call: (s) => s.offboardPerson('acme', 'p1'),
    expectedBody: { slug: 'acme', personId: 'p1' }
  },
  {
    name: 'benchPerson',
    path: '/v1/org/person/bench',
    successBody: { applied: true },
    call: (s) => s.benchPerson('acme', 'p1'),
    expectedBody: { slug: 'acme', personId: 'p1' }
  },
  {
    name: 'benchPersonLifecycle',
    path: '/v1/org/person/bench-lifecycle',
    successBody: { applied: true },
    call: (s) => s.benchPersonLifecycle('acme', 'p1'),
    expectedBody: { slug: 'acme', personId: 'p1' }
  },
  {
    name: 'recallPerson',
    path: '/v1/org/person/recall',
    successBody: { applied: true },
    call: (s) => s.recallPerson('acme', 'p1'),
    expectedBody: { slug: 'acme', personId: 'p1' }
  },
  {
    name: 'appointDepartmentHead',
    path: '/v1/org/person/appoint-head',
    successBody: { applied: true },
    call: (s) => s.appointDepartmentHead('acme', 'd1', 'p2'),
    expectedBody: { slug: 'acme', departmentId: 'd1', successorPersonId: 'p2' }
  },
  {
    name: 'replaceHeadAndOffboard',
    path: '/v1/org/person/replace-head-and-offboard',
    successBody: { applied: true },
    call: (s) => s.replaceHeadAndOffboard('acme', 'p1', 'p2'),
    expectedBody: { slug: 'acme', headPersonId: 'p1', successorPersonId: 'p2' }
  },
  {
    name: 'pauseDepartment',
    path: '/v1/org/department/pause',
    successBody: { applied: true },
    call: (s) => s.pauseDepartment('acme', 'd1'),
    expectedBody: { slug: 'acme', departmentId: 'd1' }
  },
  {
    name: 'resumeDepartment',
    path: '/v1/org/department/resume',
    successBody: { applied: true },
    call: (s) => s.resumeDepartment('acme', 'd1'),
    expectedBody: { slug: 'acme', departmentId: 'd1' }
  },
  {
    name: 'resumeDepartments',
    path: '/v1/org/department/resume-many',
    successBody: { applied: true },
    call: (s) => s.resumeDepartments('acme', ['d1', 'd2']),
    // `skipActive` is a plain default parameter (`= false`), not wrapped in
    // this file's `optional()` helper -- it is ALWAYS present on the wire,
    // never omitted. No omission behavior exists here to test; asserted
    // present-and-false to prove that, not skipped silently.
    expectedBody: { slug: 'acme', departmentIds: ['d1', 'd2'], skipActive: false }
  },
  {
    name: 'moveDepartmentMembers',
    path: '/v1/org/department/move-members',
    successBody: { applied: true, moved: ['p1'] },
    call: (s) => s.moveDepartmentMembers('acme', 'd1', 'd2', ['p1']),
    expectedBody: { slug: 'acme', fromDepartmentId: 'd1', destinationId: 'd2', personIds: ['p1'] }
  },
  {
    name: 'reactivateExecutiveRoot',
    path: '/v1/org/department/reactivate-executive-root',
    successBody: { applied: true },
    call: (s) => s.reactivateExecutiveRoot('acme'),
    expectedBody: { slug: 'acme' }
  }
]

// Of the 15 DIRECT_CASES verbs, exactly 4 have a caller-optional field
// wired through this file's `optional()` helper (omitted from the body
// entirely when its source value is nullish, not sent as `null`/`undefined`):
// shutdownPerson (intentId), offboardPerson (actor),
// appointDepartmentHead (demoteToDepartmentId), and
// moveDepartmentMembers (intent). The other 11 verbs' bodies are built
// entirely from required positional arguments -- there is no optional-field
// omission behavior to test for them, and DIRECT_CASES' `expectedBody`
// above (checked with `toStrictEqual`) is already a complete proof their
// bodies carry nothing extra. This list exists to prove the OTHER half of
// the omission guarantee for the 4 that have one: that the field DOES land
// on the wire, with the right value, when the caller actually supplies it.
interface OptionalFieldCase {
  verb: string
  field: string
  valueWhenSupplied: unknown
  successBody: unknown
  callWith: (staffing: StaffingClient) => Promise<unknown>
}

const OPTIONAL_FIELD_CASES: OptionalFieldCase[] = [
  {
    verb: 'shutdownPerson',
    field: 'intentId',
    valueWhenSupplied: 'intent-1',
    successBody: { applied: true, transitionId: 'tr-1' },
    callWith: (s) => s.shutdownPerson('acme', 'p1', 'commanded', { intentId: 'intent-1' })
  },
  {
    verb: 'offboardPerson',
    field: 'actor',
    valueWhenSupplied: 'operator-1',
    successBody: { applied: true },
    callWith: (s) => s.offboardPerson('acme', 'p1', { actor: 'operator-1' })
  },
  {
    verb: 'appointDepartmentHead',
    field: 'demoteToDepartmentId',
    valueWhenSupplied: 'd3',
    successBody: { applied: true },
    callWith: (s) => s.appointDepartmentHead('acme', 'd1', 'p2', { demoteToDepartmentId: 'd3' })
  },
  {
    verb: 'moveDepartmentMembers',
    field: 'intent',
    valueWhenSupplied: 'rebalance',
    successBody: { applied: true, moved: ['p1'] },
    callWith: (s) => s.moveDepartmentMembers('acme', 'd1', 'd2', ['p1'], { intent: 'rebalance' })
  }
]

describe('StaffingClient — #881 request-body coverage inventory', () => {
  it('pins all 12 direct verbs and the 4 supplied optional fields', () => {
    // This is the non-vacuity anchor for the table-driven assertions below.
    // A deleted or substituted row must fail loudly instead of merely making
    // the loop run fewer green cases.
    expect(DIRECT_CASES.map(({ name }) => name)).toEqual([
      'shutdownPerson',
      'offboardPerson',
      'benchPerson',
      'benchPersonLifecycle',
      'recallPerson',
      'appointDepartmentHead',
      'replaceHeadAndOffboard',
      'pauseDepartment',
      'resumeDepartment',
      'resumeDepartments',
      'moveDepartmentMembers',
      'reactivateExecutiveRoot'
    ])
    expect(OPTIONAL_FIELD_CASES.map(({ verb, field }) => `${verb}.${field}`)).toEqual([
      'shutdownPerson.intentId',
      'offboardPerson.actor',
      'appointDepartmentHead.demoteToDepartmentId',
      'moveDepartmentMembers.intent'
    ])
  })
})

describe('StaffingClient — table-driven direct/transfer outcomes', () => {
  for (const testCase of DIRECT_CASES) {
    describe(testCase.name, () => {
      it('posts the exact route and decodes a 2xx success shape', async () => {
        const transport = new RecordingTransport(() => jsonResponse(200, testCase.successBody))
        const staffing = new StaffingClient(transport)
        const result = await testCase.call(staffing)
        expect(transport.calls).toHaveLength(1)
        expect(transport.calls[0]?.path).toBe(testCase.path)
        expect(result).toMatchObject({ applied: true })
      })

      it('422 {code, detail} decodes to a returned refusal value — exactly one request, no retry', async () => {
        const transport = new RecordingTransport(() =>
          jsonResponse(422, { code: 'ceo-exempt', detail: 'cannot shut down the CEO' })
        )
        const staffing = new StaffingClient(transport)
        const result = await testCase.call(staffing)
        expect(result).toEqual({ refused: 'ceo-exempt', detail: 'cannot shut down the CEO' })
        expect(transport.calls).toHaveLength(1)
      })

      it('500 throws ChiefdUnavailableError kind http-error', async () => {
        const transport = new RecordingTransport(() => textResponse(500, 'boom'))
        const staffing = new StaffingClient(transport)
        let error: unknown
        try {
          await testCase.call(staffing)
        } catch (caught) {
          error = caught
        }
        expect(error).toBeInstanceOf(ChiefdUnavailableError)
        if (!(error instanceof ChiefdUnavailableError))
          throw new Error('expected ChiefdUnavailableError')
        expect(error.kind).toBe('http-error')
        expect(error.status).toBe(500)
      })

      it('a malformed 2xx body throws ChiefdUnavailableError kind malformed-body', async () => {
        const transport = new RecordingTransport(() => jsonResponse(200, { nonsense: true }))
        const staffing = new StaffingClient(transport)
        let error: unknown
        try {
          await testCase.call(staffing)
        } catch (caught) {
          error = caught
        }
        expect(error).toBeInstanceOf(ChiefdUnavailableError)
        if (!(error instanceof ChiefdUnavailableError))
          throw new Error('expected ChiefdUnavailableError')
        expect(error.kind).toBe('malformed-body')
      })

      // #881: route+result shape prove the request went to the right place
      // and came back parsed; neither says anything about whether the right
      // BYTES were sent. `toStrictEqual` (not `toMatchObject` or `toEqual`,
      // which treats own `undefined` keys as absent) so an unsupplied optional
      // field leaking onto the wire fails this exactly as loudly as a missing
      // required one would.
      it("posts the exact request body — the table's call supplies no optional fields", async () => {
        const transport = new RecordingTransport(() => jsonResponse(200, testCase.successBody))
        const staffing = new StaffingClient(transport)
        await testCase.call(staffing)
        expect(transport.calls).toHaveLength(1)
        expect(transport.calls[0]?.body).toStrictEqual(testCase.expectedBody)
      })
    })
  }
})

describe('StaffingClient — DIRECT_CASES optional field lands on the wire when supplied (#881)', () => {
  for (const optionalCase of OPTIONAL_FIELD_CASES) {
    it(`${optionalCase.verb}: ${optionalCase.field} is included when supplied`, async () => {
      const transport = new RecordingTransport(() => jsonResponse(200, optionalCase.successBody))
      const staffing = new StaffingClient(transport)
      await optionalCase.callWith(staffing)
      expect(transport.calls).toHaveLength(1)
      const body = transport.calls[0]?.body
      expect(body).toMatchObject({ [optionalCase.field]: optionalCase.valueWhenSupplied })
    })
  }
})

describe('StaffingClient — startPerson is the ONE non-refusal-as-value verb', () => {
  it('posts /v1/org/person/start and decodes {applied:true}', async () => {
    const transport = new RecordingTransport(() => jsonResponse(200, { applied: true }))
    const staffing = new StaffingClient(transport)
    const result = await staffing.startPerson('acme', 'p1')
    expect(transport.calls[0]?.path).toBe('/v1/org/person/start')
    expect(result).toEqual({ applied: true })
  })

  it('a 422 throws OrgRowRefusalError (row-style), never returns a refusal value', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(422, { code: 'unknown-person', detail: 'no such person' })
    )
    const staffing = new StaffingClient(transport)
    await expect(staffing.startPerson('acme', 'p1')).rejects.toBeInstanceOf(OrgRowRefusalError)
  })
})

describe('StaffingClient — hirePerson', () => {
  const SEED = {
    name: 'Ada',
    title: 'Engineer',
    mandate: 'ship it',
    kind: 'worker',
    employmentState: 'active',
    activation: 'resident',
    tools: [],
    prompts: []
  } satisfies AtomicPersonSeed

  // THE RULE: a hire carries no route and attests none. The seed has no
  // provider/model to leak, the request body has no `expected*` triple, and
  // the outcome is `{applied}` alone — every agent boots as plain Pi on the
  // operator's own defaults.
  it('posts the flattened hire body, with no route anywhere on the wire', async () => {
    const transport = new RecordingTransport(() => jsonResponse(200, { applied: true }))
    const staffing = new StaffingClient(transport)
    const result = await staffing.hirePerson('acme', 'p1', 'd1', SEED, { kind: 'operator' })
    expect(transport.calls[0]?.path).toBe('/v1/org/person/hire')
    expect(result).toEqual({ applied: true })
    const body = transport.calls[0]?.body
    for (const field of [
      'provider',
      'model',
      'modelReason',
      'observation',
      'expectedProvider',
      'expectedModel',
      'expectedModelReason'
    ]) {
      expect(body).not.toHaveProperty(field)
    }
  })

  it('422 decodes to a refusal value, exactly one request', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(422, { code: 'duplicate-person-id', detail: 'already exists' })
    )
    const result = await new StaffingClient(transport).hirePerson('acme', 'p1', 'd1', SEED, {
      kind: 'operator'
    })
    expect(result).toEqual({ refused: 'duplicate-person-id', detail: 'already exists' })
    expect(transport.calls).toHaveLength(1)
  })
})

describe('StaffingClient — transferPerson', () => {
  it('decodes {applied, moved}', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(200, { applied: true, moved: ['p1', 'p2'] })
    )
    const staffing = new StaffingClient(transport)
    const result = await staffing.transferPerson('acme', 'p1', 'dest')
    expect(transport.calls[0]?.path).toBe('/v1/org/person/transfer')
    expect(result).toEqual({ applied: true, moved: ['p1', 'p2'] })
  })

  it('422 decodes to a refusal value', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(422, { code: 'unknown-destination', detail: 'no such department' })
    )
    const staffing = new StaffingClient(transport)
    const result = await staffing.transferPerson('acme', 'p1', 'dest')
    expect(result).toEqual({ refused: 'unknown-destination', detail: 'no such department' })
  })
})

describe('StaffingClient — createDepartment', () => {
  it('decodes {applied, departmentId}', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(200, { applied: true, departmentId: 'd1' })
    )
    const staffing = new StaffingClient(transport)
    const result = await staffing.createDepartment(
      'acme',
      'd1',
      'root',
      'Eng',
      { kind: 'appoint-existing', personId: 'p1' },
      {
        kind: 'operator'
      }
    )
    expect(transport.calls[0]?.path).toBe('/v1/org/department/create')
    expect(result).toEqual({ applied: true, departmentId: 'd1' })
  })

  it('422 decodes to a refusal value', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(422, { code: 'duplicate-department', detail: 'exists' })
    )
    const staffing = new StaffingClient(transport)
    const result = await staffing.createDepartment(
      'acme',
      'd1',
      'root',
      'Eng',
      { kind: 'appoint-existing', personId: 'p1' },
      {
        kind: 'operator'
      }
    )
    expect(result).toEqual({ refused: 'duplicate-department', detail: 'exists' })
  })
})

describe('StaffingClient — reparentDepartment', () => {
  it('decodes {applied, departmentId}', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(200, { applied: true, departmentId: 'd1' })
    )
    const staffing = new StaffingClient(transport)
    const result = await staffing.reparentDepartment('acme', 'd1', 'd2')
    expect(transport.calls[0]?.path).toBe('/v1/org/department/reparent')
    expect(result).toEqual({ applied: true, departmentId: 'd1' })
  })
})

describe('StaffingClient — removeDepartmentTree', () => {
  // The people are DEPARTED, not removed: the route offboards everyone homed
  // under the subtree and retains every row. `removedPersonIds` was the old
  // name for the old behaviour and is gone with it, in both directions — a
  // body still carrying it is malformed, not silently accepted.
  it('validates both id arrays', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(200, {
        applied: true,
        removedDepartmentIds: ['d1', 'd2'],
        departedPersonIds: ['p1']
      })
    )
    const staffing = new StaffingClient(transport)
    const result = await staffing.removeDepartmentTree('acme', 'd1')
    expect(transport.calls[0]?.path).toBe('/v1/org/department/remove-tree')
    expect(result).toEqual({
      applied: true,
      removedDepartmentIds: ['d1', 'd2'],
      departedPersonIds: ['p1']
    })
  })

  it('a malformed array throws malformed-body', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(200, { applied: true, removedDepartmentIds: ['d1'], departedPersonIds: [42] })
    )
    const staffing = new StaffingClient(transport)
    let error: unknown
    try {
      await staffing.removeDepartmentTree('acme', 'd1')
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ChiefdUnavailableError)
  })
})

describe('StaffingClient — the company key on the wire', () => {
  // Every /v1/org/* route resolves its company by the key the caller sends as
  // `slug`. That key used to be built HERE, by a transport wrapper that
  // rewrote `slug -> documentKey(slug, orgsRoot)` — and for a while by nothing
  // at all, so every department, hire, transfer, offboard and reparent call
  // sent a bare display slug, chiefd answered `404 unknown-company`, and this
  // client reported that as "upstream unreachable". Nothing is built here now:
  // the key is `sha256(dir)[..12]`, served by beacond and by the daemon
  // rendezvous. These lock the caller's value onto the wire UNCHANGED, which
  // is the property that outlived the rewrite.
  const KEY = '0123456789ab'

  it('sends the key it was given on a direct-outcome verb', async () => {
    const transport = new RecordingTransport(() => jsonResponse(200, { applied: true }))
    await new StaffingClient(transport).benchPerson(KEY, 'p1')
    expect(transport.calls[0]?.body).toMatchObject({ slug: KEY, personId: 'p1' })
  })

  it('sends the key on a verb that posts through the module-level helper', async () => {
    // `offboardPerson` goes through `directOutcome(this.transport, …)`, not
    // through a method on the class — the exact shape a per-method rewrite
    // would have missed, and the reason the rewrite lived at the transport.
    const transport = new RecordingTransport(() => jsonResponse(200, { applied: true }))
    await new StaffingClient(transport).offboardPerson(KEY, 'p1')
    expect(transport.calls[0]?.body).toMatchObject({ slug: KEY, personId: 'p1' })
  })
})

describe('StaffingClient — a hire carries ONE authority and no route', () => {
  // `hiringManagerPersonId` is DELETED, and this is the test that used to pin
  // its blank-is-absent rule. That rule existed because chiefd read the field
  // as `Option<String>` and `validate_hire_requester` refused an operator
  // requester carrying any manager id at all — a fence around ROUTE
  // INHERITANCE, not around authority: the manager was named so the hire could
  // inherit that manager's model. With provider/model management deleted there
  // is no route to inherit, `validate_hire_requester` is gone, and the field
  // names nothing the route decides.
  //
  // So the rule INVERTS rather than relaxes, and this pins the inversion: the
  // wire carries the attested `requester` and nothing else that could be
  // mistaken for a second authority. `deny_unknown_fields` on the Rust request
  // makes a caller that still sends one fail loudly instead of having it
  // ignored — which is exactly the silent-drop this suite was written about.
  const SEED = {
    name: 'Ada',
    title: 'Engineer',
    mandate: 'ship it',
    kind: 'worker',
    employmentState: 'active',
    activation: 'resident',
    tools: [],
    prompts: []
  } satisfies AtomicPersonSeed

  it('sends the attested requester and no manager id at all', async () => {
    const transport = new RecordingTransport(() => jsonResponse(200, { applied: true }))
    await new StaffingClient(transport).hirePerson('acme', '', 'd1', SEED, {
      kind: 'person',
      personId: 'ada'
    })
    const body = transport.calls[0]?.body
    expect(body).toMatchObject({ requester: { kind: 'person', personId: 'ada' } })
    expect(body).not.toHaveProperty('hiringManagerPersonId')
  })

  it('sends no route, no effort and no task class, whatever the seed shape', async () => {
    // The seed type no longer HAS those fields, so this is a wire assertion
    // rather than a type one: a re-added field would have to reach the body to
    // be a defect, and this is where it would show up.
    const transport = new RecordingTransport(() => jsonResponse(200, { applied: true }))
    await new StaffingClient(transport).hirePerson('acme', '', 'd1', SEED, { kind: 'operator' })
    const body = transport.calls[0]?.body
    for (const retired of ['provider', 'model', 'modelReason', 'taskClass', 'thinking']) {
      expect(body).not.toHaveProperty(retired)
    }
  })

  it('an operator may hire, which the manager fence used to forbid', async () => {
    // THE PRODUCT RULE THAT CHANGED. `operator-hirer-invalid` refused an
    // operator-initiated hire outright, because an operator has no durable
    // route for the new person to inherit. There is no route, so there is
    // nothing to refuse, and the client must not withhold the call.
    const transport = new RecordingTransport(() => jsonResponse(200, { applied: true }))
    const outcome = await new StaffingClient(transport).hirePerson('acme', '', 'd1', SEED, {
      kind: 'operator'
    })
    expect(outcome).toMatchObject({ applied: true })
    expect(transport.calls[0]?.body).toMatchObject({ requester: { kind: 'operator' } })
  })
})
