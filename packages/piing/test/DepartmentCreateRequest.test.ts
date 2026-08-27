/**
 * #751/P1: the department a CEO describes, as chiefd's API accepts it.
 *
 * This mapping used to be spread across three places that could disagree — a
 * CLI's argument parsing, a JSON document on that CLI's stdin, and
 * `planDepartmentCreate`'s client-side id minting — and reached the daemon by
 * spawning `bun apps/cli/src/Main.ts department launch`. `apps/cli` is deleted,
 * so that spawn answered `chiefd: unknown command 'department'` and a CEO could
 * not create a department at all.
 *
 * `departmentCreateRequest` is the whole translation now, and it is pure, so
 * these tests pin it with no daemon, no subprocess and no company. What they
 * CANNOT prove is that chiefd accepts the shape — that is the Rust half's own
 * tests plus the live exercise, and no amount of agreement between a fake and
 * this function would substitute for either.
 */
import type {
  ChiefdCreateUnit,
  ChiefdDepartmentCreateRequest,
  ChiefdDepartmentHead,
  ChiefdDepartmentPersonSeed,
  ChiefdHeadVacancy,
  IntercomDepartmentSpec
} from '@test-assets/organization-intercom'
import { departmentCreateRequest, normalizeHeadVacancy } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

const HEAD = { name: 'Dana Rivers', mandate: 'Own the funnel end to end.' }

/** Narrow the head union to the hire-new seed. A real guard rather than an
 *  assertion: the union is the whole point of the appoint-existing case below,
 *  and a cast here would let that case silently pass this check too. */
function hired(head: ChiefdDepartmentHead): ChiefdDepartmentPersonSeed {
  if (head.kind !== 'hire-new') throw new Error(`expected a hire-new head, got ${head.kind}`)
  return head
}

function request(
  spec?: IntercomDepartmentSpec,
  extra: {
    modelAuthority?: unknown
    unit?: ChiefdCreateUnit
    existingHeadPersonId?: string
    vacates?: ChiefdHeadVacancy
  } = {}
): ChiefdDepartmentCreateRequest {
  return departmentCreateRequest({
    slug: 'acme@0123456789ab',
    parentUnitId: 'executive',
    spec: spec ?? { name: 'Growth Engineering', purpose: 'Grow revenue.', head: HEAD },
    requesterPersonId: 'ceo',
    ...extra
  })
}

describe('departmentCreateRequest', () => {
  test('an id the manager did not choose is left for chiefd to mint', () => {
    const body = request()
    // EMPTY, not invented. Client-side minting is exactly what #751/R3 removes:
    // an id computed here is a second opinion about a name chiefd already knows
    // how to derive, and the two drift the moment either changes.
    expect(body.departmentId).toBe('')
    expect(hired(body.head).personId).toBe('')
  })

  test('an id the manager DID choose is carried through untouched', () => {
    const body = request({
      id: 'growth-eng',
      name: 'Growth Engineering',
      purpose: 'Grow revenue.',
      head: { ...HEAD, id: 'dana' }
    })
    expect(body.departmentId).toBe('growth-eng')
    expect(hired(body.head).personId).toBe('dana')
  })

  test('the requester is the calling person, never the operator', () => {
    // A pane may only ever send its own launcher-bound person id. Attributing a
    // manager's staffing decision to "the operator" would launder authority
    // through the transport.
    expect(request().requester).toEqual({ kind: 'person', personId: 'ceo' })
  })

  test('the head is a hire-new seed and staff are workers', () => {
    const body = request({
      name: 'Growth Engineering',
      purpose: 'Grow revenue.',
      head: HEAD,
      staff: [{ name: 'Sam Ito', mandate: 'Ship the funnel.' }]
    })
    expect(body.head).toMatchObject({ kind: 'hire-new', personKind: 'head', name: 'Dana Rivers' })
    expect(body.staff).toHaveLength(1)
    expect(body.staff[0]).toMatchObject({
      kind: 'hire-new',
      personKind: 'worker',
      name: 'Sam Ito'
    })
  })

  test('startActive: false benches a person; the default is active', () => {
    const benched = request({
      name: 'Growth',
      purpose: 'p',
      head: HEAD,
      staff: [{ name: 'Sam Ito', mandate: 'm', startActive: false }]
    })
    expect(benched.staff[0]?.employmentState).toBe('benched')
    expect(hired(benched.head).employmentState).toBe('active')
  })

  test('automatic org tools are refused in both head and staff declarations', () => {
    expect(() =>
      request({
        name: 'Research',
        purpose: 'Find market signals.',
        head: { ...HEAD, tools: ['org_send'] }
      })
    ).toThrow(/Never put org_\* names.*installed automatically.*omit them/i)

    expect(() =>
      request({
        name: 'Research',
        purpose: 'Find market signals.',
        head: HEAD,
        staff: [{ name: 'Sam Ito', mandate: 'Analyze signals.', tools: ['org_roster'] }]
      })
    ).toThrow(/Never put org_\* names.*installed automatically.*omit them/i)
  })

  // THE RULE: a department create names no route and attests none. Chief is
  // out of the provider/model business, so a head and its staff boot as plain
  // Pi on the operator's own defaults. A caller that names a route is not
  // obeyed quietly — the field never reaches the wire.
  test('no route and no model authority travel with a create, whatever the caller passes', () => {
    const routed = request({
      name: 'Growth',
      purpose: 'p',
      head: { ...HEAD, provider: 'openrouter', model: 'anthropic/claude-sonnet-4' }
    })
    expect(Object.hasOwn(routed.head, 'provider')).toBe(false)
    expect(Object.hasOwn(routed.head, 'model')).toBe(false)
    expect(Object.hasOwn(request(), 'modelAuthority')).toBe(false)
    expect(Object.hasOwn(routed, 'modelAuthority')).toBe(false)
  })

  test('a nameless department and a headless one are refused here, not at the daemon', () => {
    expect(() => request({ name: '  ', purpose: 'p', head: HEAD })).toThrow(/name/i)
    expect(() => request({ name: 'Growth', purpose: 'p', head: { name: 'Dana' } })).toThrow(
      /mandate/i
    )
  })

  test('a contract is the same request with transient engagement metadata', () => {
    // Not a second route and not a second table: a contract IS a department row
    // carrying an engagement, which is why stop/resume/remove are one route for
    // both kinds.
    const unit: ChiefdCreateUnit = {
      kind: 'contract',
      transient: { engagement: 'Ship the Q3 migration.', launchedAt: '2026-08-09T00:00:00.000Z' }
    }
    const body = request(undefined, { unit })
    expect(body.unit).toBe(unit)
    expect(body.parentId).toBe('executive')
  })

  test('an existing head is appointed, not hired, and starts the unit head-only', () => {
    // The two head decisions are mutually exclusive by construction here. Staff
    // are dropped for an existing head because those people would need a model
    // authority for a manager whose own route the transfer is still settling —
    // which is exactly what the tool surface already promises ("An
    // existing-head create starts head-only").
    const body = request(
      {
        name: 'Growth',
        purpose: 'p',
        head: HEAD,
        staff: [{ name: 'Sam Ito', mandate: 'm' }]
      },
      { existingHeadPersonId: 'dana' }
    )
    expect(body.head).toEqual({ kind: 'appoint-existing', personId: 'dana' })
    expect(body.staff).toEqual([])
  })
})

/**
 * What becomes of the department the appointee ALREADY heads.
 *
 * A person heads one department, and `departments_one_head` is a UNIQUE index
 * rather than only a validator rule, so a sitting head can lead a new
 * department only if the old one gets an answer in the same transaction. This
 * function CARRIES that answer and does not compute it: which answer applies
 * depends on who else is in the vacated department, and chiefd holds the tree.
 */
describe('the vacancy decision is carried, never decided here', () => {
  test('a hand-over reaches chiefd exactly as the caller stated it', () => {
    const body = request(undefined, {
      existingHeadPersonId: 'dana',
      vacates: { kind: 'hand-over', successorPersonId: 'sam' }
    })
    expect(body.vacates).toEqual({ kind: 'hand-over', successorPersonId: 'sam' })
  })

  test('a dissolve reaches chiefd as a bare decision, naming nobody', () => {
    const body = request(undefined, {
      existingHeadPersonId: 'dana',
      vacates: { kind: 'dissolve' }
    })
    // No successor field at all. A dissolve happens because there is nobody
    // left to name, so inventing an empty successor here would be a lie the
    // route would then have to reject.
    expect(body.vacates).toEqual({ kind: 'dissolve' })
  })

  test('a create with no vacancy decision sends no field, rather than a null', () => {
    // ABSENT, not present-and-empty. The route uses `deny_unknown_fields` and
    // reads absence as "the caller made no such decision", which is itself
    // refusable when the appointee heads something — the refusal that names
    // the department and its eligible successors.
    expect(request(undefined, { existingHeadPersonId: 'dana' })).not.toHaveProperty('vacates')
  })

  test('a HIRE-NEW head never carries one, however the caller asks', () => {
    // Somebody hired a moment ago heads nothing, so there is nothing to
    // vacate. Dropped here rather than forwarded: the route would refuse it,
    // and a request this function knows is meaningless should not be built.
    const body = request(undefined, { vacates: { kind: 'dissolve' } })
    expect(body.head.kind).toBe('hire-new')
    expect(body).not.toHaveProperty('vacates')
  })
})

/**
 * The ONE shape check, shared by `org_add_department` and `org_transfer`.
 *
 * Shape only, deliberately. Whether a hand-over names a real member of the
 * department being left, and whether a dissolve is honest about that
 * department being empty, are chiefd's answers — it holds the tree and refuses
 * naming the department and the members who could take it. A second opinion
 * here would be the same rule written twice, which is the defect this whole
 * packet exists to close.
 */
describe('normalizeHeadVacancy', () => {
  test('no decision is not an error: most moves vacate nothing', () => {
    expect(normalizeHeadVacancy(undefined, 'vacates')).toEqual({})
  })

  test('a dissolve needs nobody, so it normalizes bare', () => {
    expect(normalizeHeadVacancy({ kind: 'dissolve' }, 'vacates')).toEqual({
      value: { kind: 'dissolve' }
    })
  })

  test('a hand-over keeps the successor, trimmed', () => {
    expect(
      normalizeHeadVacancy({ kind: 'hand-over', successorPersonId: '  sam  ' }, 'vacates')
    ).toEqual({ value: { kind: 'hand-over', successorPersonId: 'sam' } })
  })

  test('a hand-over naming nobody is refused, and offers the other answer', () => {
    const outcome = normalizeHeadVacancy({ kind: 'hand-over' }, 'vacates')
    if (!('refusal' in outcome)) throw new Error('a hand-over with no successor must be refused')
    // The refusal names the field the way the CALLER wrote it. Both tools that
    // can vacate a headship now spell it `vacates` at their top level — #1150
    // flattened the department-create surface onto the one `org_transfer`
    // already had — so the check stays parameterized and the two agree.
    expect(outcome.refusal).toContain('vacates.successorPersonId')
    // A refusal that states no way through is a dead end, and this one has an
    // obvious second answer to point at.
    expect(outcome.refusal).toContain('{ kind: "dissolve" }')
  })

  test('a hand-over whose successor is only whitespace is refused too', () => {
    expect(
      normalizeHeadVacancy({ kind: 'hand-over', successorPersonId: '   ' }, 'vacates')
    ).toHaveProperty('refusal')
  })
})
