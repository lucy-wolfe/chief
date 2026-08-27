/**
 * #1046 — the CEO of a brand-new company could not hire at the root.
 *
 * The incident, on a live box. `belfort-brothers-capital` came up with a CEO
 * and nobody else. The operator asked for a chief of staff and the CEO burned
 * three attempts:
 *
 *   1. `org_hire({ departmentId: 'belfort-brothers-capital' })` — refused with
 *      "'ceo' does not manage department 'belfort-brothers-capital'". False:
 *      the CEO manages every department. The id simply did not exist, because
 *      the root department's ID is `executive` while its NAME is the company
 *      display name.
 *   2. The remediation sentence in that refusal said to create a department
 *      "naming yourself as its existing head". chiefd refuses that for the
 *      CEO — `exec-root-protected` — and for any head — `head-not-eligible`.
 *   3. A department headed by a NEW person. That worked, by luck.
 *
 * Three defects, one per assertion block below: a boolean that collapsed
 * "no such department" into "you lack authority", a display name that invited
 * the wrong id, and one static remediation sentence that was impossible for
 * the caller it was shown to.
 */
import { isNullish } from '@test/support/Nullish'
import { describe, expect, test } from 'vitest'

import type {
  IntercomOrganizationManifest,
  PersonRecord
} from '../extensions/organization-intercom'
import {
  departmentScopeDenial,
  hiringPathAdvice,
  unknownDepartmentMessage
} from '../extensions/organization-intercom'

const CREATED_AT = '2026-08-12T00:00:00.000Z'

/** The named person, or a loud failure. A test that silently reads `undefined`
 *  proves nothing about a predicate that takes a person. */
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
 * The company from the incident: one CEO, no staff, and a root department
 * whose id and display name deliberately differ — which is the whole trap.
 */
function belfort(): IntercomOrganizationManifest {
  return {
    schemaVersion: 3,
    kind: 'organization',
    slug: 'belfort-brothers-capital',
    name: 'Belfort Brothers Capital',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'office-of-the-ceo'],
    peopleOrder: ['ceo'],
    departments: {
      executive: {
        id: 'executive',
        name: 'Belfort Brothers Capital',
        purpose: 'Run the company',
        kind: 'company',
        headPersonId: 'ceo',
        state: 'active'
      },
      'office-of-the-ceo': {
        id: 'office-of-the-ceo',
        name: 'Office of the CEO',
        purpose: 'Support the CEO',
        kind: 'department',
        parentDepartmentId: 'executive',
        headPersonId: 'ceo',
        state: 'active'
      }
    },
    people: { ceo: person('ceo', 'executive', 'executive') }
  }
}

/** A second company with a head and a peer department, so an out-of-scope
 *  denial is expressible at all (the CEO can never produce one). */
function twoDepartments(): IntercomOrganizationManifest {
  const manifest = belfort()
  manifest.departmentOrder = ['executive', 'engineering', 'finance']
  manifest.peopleOrder = ['ceo', 'eng-head', 'fin-head', 'eng-worker']
  delete manifest.departments['office-of-the-ceo']
  manifest.departments.engineering = {
    id: 'engineering',
    name: 'Engineering',
    purpose: 'Build',
    kind: 'department',
    parentDepartmentId: 'executive',
    headPersonId: 'eng-head',
    state: 'active'
  }
  manifest.departments.finance = {
    id: 'finance',
    name: 'Finance',
    purpose: 'Count',
    kind: 'department',
    parentDepartmentId: 'executive',
    headPersonId: 'fin-head',
    state: 'active'
  }
  manifest.people['eng-head'] = person('eng-head', 'head', 'engineering')
  manifest.people['fin-head'] = person('fin-head', 'head', 'finance')
  manifest.people['eng-worker'] = person('eng-worker', 'worker', 'engineering')
  return manifest
}

describe('#1046 defect 1: an unknown id and a scope denial are different answers', () => {
  test('an id that names no department reports "unknown-department", never a denial', () => {
    const manifest = belfort()
    expect(
      departmentScopeDenial(manifest, personOf(manifest, 'ceo'), 'belfort-brothers-capital')
    ).toBe('unknown-department')
  })

  test('a real department outside a head’s subtree reports "out-of-scope"', () => {
    const manifest = twoDepartments()
    expect(departmentScopeDenial(manifest, personOf(manifest, 'eng-head'), 'finance')).toBe(
      'out-of-scope'
    )
  })

  test('the two answers are distinguishable, which is the whole point', () => {
    const manifest = twoDepartments()
    const unknown = departmentScopeDenial(manifest, personOf(manifest, 'eng-head'), 'no-such-unit')
    const denied = departmentScopeDenial(manifest, personOf(manifest, 'eng-head'), 'finance')
    expect(unknown).not.toBe(denied)
  })

  test('a CEO is in scope for EVERY department that exists, including the root it heads', () => {
    const manifest = twoDepartments()
    for (const departmentId of manifest.departmentOrder) {
      expect(
        departmentScopeDenial(manifest, personOf(manifest, 'ceo'), departmentId),
        `ceo denied '${departmentId}'`
      ).toBeUndefined()
    }
  })

  test('a head is in scope for its own department and everything under it', () => {
    const manifest = twoDepartments()
    expect(
      departmentScopeDenial(manifest, personOf(manifest, 'eng-head'), 'engineering')
    ).toBeUndefined()
  })

  test('a worker that heads nothing is out of scope even for the department it sits in', () => {
    const manifest = twoDepartments()
    expect(departmentScopeDenial(manifest, personOf(manifest, 'eng-worker'), 'engineering')).toBe(
      'out-of-scope'
    )
  })
})

describe('#1046 defect 2: the id/name trap gets a corrective hint', () => {
  test('the company slug names the company, and the message gives the root id', () => {
    const manifest = belfort()
    const message = unknownDepartmentMessage(
      manifest,
      personOf(manifest, 'ceo'),
      'belfort-brothers-capital',
      'hire into'
    )
    expect(message).toContain("Unknown department 'belfort-brothers-capital'.")
    expect(message).toContain(
      "'belfort-brothers-capital' names the company, not a department: the company's root department id is 'executive'."
    )
    expect(message).toContain('Departments you may hire into: executive, office-of-the-ceo.')
  })

  test('a department DISPLAY NAME is named as a name, and its id is given', () => {
    const manifest = twoDepartments()
    const message = unknownDepartmentMessage(
      manifest,
      personOf(manifest, 'ceo'),
      'Engineering',
      'hire into'
    )
    expect(message).toContain("'Engineering' is the NAME of department 'engineering': pass the id.")
  })

  test('the wrong id is corrected, never silently accepted', () => {
    const manifest = belfort()
    // The hint exists so the caller can retry with the right id. The scope
    // answer for the wrong id stays a refusal.
    expect(
      departmentScopeDenial(manifest, personOf(manifest, 'ceo'), 'belfort-brothers-capital')
    ).toBe('unknown-department')
  })

  test('a genuine typo gets the id list with no misleading hint', () => {
    const manifest = twoDepartments()
    const message = unknownDepartmentMessage(
      manifest,
      personOf(manifest, 'ceo'),
      'enginering',
      'hire into'
    )
    expect(message).toContain("Unknown department 'enginering'.")
    expect(message).not.toContain('is the NAME of')
    expect(message).not.toContain('names the company')
    expect(message).toContain('Departments you may hire into: executive, engineering, finance.')
  })

  test('the listed ids are the ones the caller may actually use, not the whole tree', () => {
    const manifest = twoDepartments()
    const message = unknownDepartmentMessage(
      manifest,
      personOf(manifest, 'eng-head'),
      'finanace',
      'hire into'
    )
    expect(message).toContain('Departments you may hire into: engineering.')
    expect(message).not.toContain('finance')
  })

  test('a caller with no usable department is told exactly that, not given an empty list', () => {
    const manifest = twoDepartments()
    const message = unknownDepartmentMessage(
      manifest,
      personOf(manifest, 'eng-worker'),
      'nope',
      'hire into'
    )
    expect(message).toContain('You may hire into no department.')
  })
})

describe('#1046 defect 3: the remediation advice is derived, and always achievable', () => {
  /** `exec-root-protected` in `org_ops.rs` refuses to move the CEO into a new
   *  department it would head. Advice that names that path is a dead end. */
  test('a CEO is never told to name itself the existing head of a new department', () => {
    const manifest = belfort()
    const advice = hiringPathAdvice(manifest, personOf(manifest, 'ceo'))
    expect(advice).not.toContain('existingHeadPersonId')
    expect(advice).not.toContain('naming yourself')
    expect(advice).toContain("You head 'executive': hire into 'executive' directly")
  })

  /**
   * THE CARLOS SHAPE, and the TS half of a parity claim.
   *
   * Operator ruling 2026-08-13 (`AGENTS.md`): the CEO is the only immovable
   * node. A worker homed in the ROOT department, heading nothing — which is
   * exactly what `tribes-capital`'s chief of staff is — may now be made the
   * head of a new department, so the advice to name themselves is ACHIEVABLE
   * rather than a dead end.
   *
   * This is asserted on the TS side deliberately. The Rust guard's comment
   * warned that the whole-root shape existed partly to avoid a TS↔Rust
   * `executiveRootUnitIds` parity split; that TS implementation no longer
   * exists (the launcher holding it was deleted), so the only TS surface left
   * is this advice, and it must agree with the narrowed Rust guard.
   */
  test('a worker homed in the root is told to head a new department, and that now works', () => {
    const manifest = belfort()
    manifest.peopleOrder = ['ceo', 'carlos']
    manifest.people.carlos = person('carlos', 'executive', 'executive')

    const advice = hiringPathAdvice(manifest, personOf(manifest, 'carlos'))
    expect(advice).toContain('You head no department yet')
    expect(advice).toContain('existingHeadPersonId')
    // The CEO's own advice is unchanged: it heads the root, so it is told to
    // hire into it rather than to name itself the head of something new.
    const ceo = hiringPathAdvice(manifest, personOf(manifest, 'ceo'))
    expect(ceo).not.toContain('existingHeadPersonId')
  })

  /** `head-not-eligible`: the appointed head "must be an employed worker who
   *  heads no department". A head already fails that. */
  test('a department head is never told to name itself the existing head either', () => {
    const manifest = twoDepartments()
    const advice = hiringPathAdvice(manifest, personOf(manifest, 'eng-head'))
    expect(advice).not.toContain('existingHeadPersonId')
    expect(advice).toContain("You head 'engineering': hire into 'engineering' directly")
    expect(advice).toContain('a NEW head (the head argument)')
  })

  test('a person who heads nothing DOES get the self-as-existing-head path, which works for it', () => {
    const manifest = twoDepartments()
    const advice = hiringPathAdvice(manifest, personOf(manifest, 'eng-worker'))
    expect(advice).toContain('You head no department yet')
    expect(advice).toContain('the existingHeadPersonId argument')
  })

  /**
   * The 2026-08-13 incident, from the advice side. A Chief of Staff who is a
   * plain worker homed in the company ROOT gets the same self-as-existing-head
   * sentence — and until chiefd's guard narrowed to the CEO alone, chiefd
   * refused exactly what this sentence told him to do. The advice was right
   * and the guard was wrong; this case pins the shape of the person the guard
   * used to freeze, so the two cannot drift apart again.
   */
  test('a worker homed in the executive root gets the same path, and it is now achievable', () => {
    const manifest = belfort()
    manifest.peopleOrder = ['ceo', 'carlos']
    manifest.people.carlos = person('carlos', 'worker', 'executive')
    const carlos = personOf(manifest, 'carlos')
    expect(carlos.kind, 'the incident person is a plain worker, not a head').toBe('worker')
    expect(carlos.departmentId, 'and he lives in the company root').toBe('executive')
    const advice = hiringPathAdvice(manifest, carlos)
    expect(advice).toContain('You head no department yet')
    expect(advice).toContain('the existingHeadPersonId argument')
  })
})

describe('#1046 regression: the three-attempt incident sequence', () => {
  test('attempt 1 now answers "unknown department" and names executive, not an authority failure', () => {
    const manifest = belfort()
    const ceo = personOf(manifest, 'ceo')
    expect(departmentScopeDenial(manifest, ceo, 'belfort-brothers-capital')).toBe(
      'unknown-department'
    )
    const message = unknownDepartmentMessage(manifest, ceo, 'belfort-brothers-capital', 'hire into')
    expect(message).toMatch(/unknown department/i)
    expect(message).not.toContain('does not manage')
    expect(message).toContain("root department id is 'executive'")
  })

  test('attempt 2 is never proposed to the CEO any more', () => {
    const manifest = belfort()
    expect(hiringPathAdvice(manifest, personOf(manifest, 'ceo'))).not.toContain(
      'existingHeadPersonId'
    )
  })

  test('attempt 3 was never necessary: the CEO could always hire into executive', () => {
    const manifest = belfort()
    expect(departmentScopeDenial(manifest, personOf(manifest, 'ceo'), 'executive')).toBeUndefined()
  })
})
