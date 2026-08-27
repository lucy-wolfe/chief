/**
 * THE ONE PLACE IN THE AUTHORITY MODEL WITH NO FEEDBACK LOOP.
 *
 * Live incident on `tribes-capital`. The operator told the CEO to have the
 * Chief of Staff stand up an Engineering department. The CEO did the work
 * itself and reported that Carlos "is Chief of Staff (general staff) and
 * doesn't hold the org-management tools needed to create a department or hire a
 * department head — those are CEO/head-level functions."
 *
 * Every word of that is false. `staffingAuthority` has NO role gate: authority
 * is the subtree, not the job title, and Carlos holds `org_add_department` and
 * `org_hire` like everybody else. What Carlos lacks is SCOPE — he heads no
 * department, so he cannot hire into `executive` — and the accepted path was
 * open to him the whole time: create a department beneath himself, as its own
 * head, then hire into it.
 *
 * The company's exception log holds ZERO refusals for carlos. He never
 * attempted. Every real refusal in this system teaches; a refusal that never
 * fires teaches nothing, and nothing else told the CEO what another person may
 * do. `org_roster` is where a manager already looks, so the answer belongs
 * there — derived from the gates themselves, never described a second time.
 */
import { isNullish } from '@test/support/Nullish'
import { describe, expect, test } from 'vitest'

import type {
  IntercomOrganizationManifest,
  PersonRecord
} from '../extensions/organization-intercom'
import {
  departmentScopeDenial,
  formatOrganizationRoster,
  hiringPathAdvice,
  personAuthority,
  personAuthorityText
} from '../extensions/organization-intercom'

const CREATED_AT = '2026-08-12T00:00:00.000Z'

function personOf(manifest: IntercomOrganizationManifest, id: string): PersonRecord {
  const found = manifest.people[id]
  if (isNullish(found)) throw new Error(`the fixture has no person '${id}'`)
  return found
}

function person(
  id: string,
  kind: 'executive' | 'head' | 'worker',
  departmentId: string,
  employmentState: 'active' | 'benched' | 'departed' = 'active'
): PersonRecord {
  return {
    id,
    name: id,
    title: id,
    kind,
    departmentId,
    employmentState,
    createdAt: CREATED_AT
  }
}

/**
 * The company from the incident, plus the depth that makes "at or under" mean
 * something: a CEO, a general-staff Chief of Staff who heads nothing, a
 * research head with a child department beneath it, a worker inside that
 * child, and one departed person.
 */
function tribesCapital(): IntercomOrganizationManifest {
  return {
    schemaVersion: 3,
    kind: 'organization',
    slug: 'tribes-capital',
    name: 'Tribes Capital',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'research', 'research-data'],
    peopleOrder: ['ceo', 'carlos', 'rhea', 'dov', 'gone'],
    departments: {
      executive: {
        id: 'executive',
        name: 'Tribes Capital',
        purpose: 'Run the company',
        kind: 'company',
        headPersonId: 'ceo',
        state: 'active'
      },
      research: {
        id: 'research',
        name: 'Research',
        purpose: 'Study the market',
        kind: 'department',
        parentDepartmentId: 'executive',
        headPersonId: 'rhea',
        state: 'active'
      },
      'research-data': {
        id: 'research-data',
        name: 'Research Data',
        purpose: 'Keep the data',
        kind: 'department',
        parentDepartmentId: 'research',
        headPersonId: 'dov',
        state: 'active'
      }
    },
    people: {
      ceo: person('ceo', 'executive', 'executive'),
      carlos: person('carlos', 'worker', 'executive'),
      rhea: person('rhea', 'head', 'research'),
      dov: person('dov', 'head', 'research-data'),
      gone: person('gone', 'worker', 'research', 'departed')
    }
  }
}

/** Every department at or under `root`, derived in the TEST rather than read
 *  from the code under test — the point is that the two agree. */
function subtreeIds(manifest: IntercomOrganizationManifest, root: string): string[] {
  return Object.keys(manifest.departments).filter((id) => {
    let cursor: string | undefined = id
    const seen = new Set<string>()
    while (!isNullish(cursor) && !seen.has(cursor)) {
      if (cursor === root) return true
      seen.add(cursor)
      cursor = manifest.departments[cursor]?.parentDepartmentId
    }
    return false
  })
}

/** The person's line in the rendered roster, or a loud failure. */
function rosterLine(manifest: IntercomOrganizationManifest, personId: string): string {
  const line = formatOrganizationRoster(manifest)
    .split('\n')
    .find((candidate) => candidate.includes(`[${personId}]`))
  if (isNullish(line)) throw new Error(`the roster has no line for '${personId}'`)
  return line
}

describe('a person who heads a department reports it', () => {
  test('a head names its department, where it may add, and where it may hire', () => {
    const manifest = tribesCapital()
    const text = personAuthorityText(manifest, personOf(manifest, 'rhea'))
    expect(text).toContain('heads research')
    expect(text).toContain('may add departments under it with a new head')
    expect(text).toContain('may hire at or under research')
  })

  test('a head that heads a leaf still reports its own department', () => {
    const manifest = tribesCapital()
    expect(personAuthorityText(manifest, personOf(manifest, 'dov'))).toContain(
      'heads research-data'
    )
  })

  test('the CEO reads as company-wide, because the gate admits every department', () => {
    const manifest = tribesCapital()
    const text = personAuthorityText(manifest, personOf(manifest, 'ceo'))
    expect(text).toContain('heads executive')
    expect(text).toContain('may hire anywhere in the company')
  })

  test('a head is never told to name itself the head of its new department', () => {
    const manifest = tribesCapital()
    // The REASON changed and the assertion did not. A sitting head naming
    // itself the head of a NEW department is no longer refused outright — it is
    // refused until the caller says what becomes of the department being
    // vacated. So the roster still must not offer "as its own head" as a plain
    // path: that is the path for somebody who heads nothing, and offering it to
    // a head hides the decision the create now demands.
    expect(personAuthorityText(manifest, personOf(manifest, 'rhea'))).not.toContain(
      'as its own head'
    )
  })

  test('a head is told it may vacate its department to head another', () => {
    const manifest = tribesCapital()
    expect(personAuthorityText(manifest, personOf(manifest, 'rhea'))).toContain(
      'or vacate it to head another department'
    )
  })

  test('an ordinary head is told HOW, and told both answers', () => {
    const manifest = tribesCapital()
    const advice = hiringPathAdvice(manifest, personOf(manifest, 'rhea'))
    expect(advice).toContain('the vacates argument')
    expect(advice).toContain('hand it to one of its members')
    expect(advice).toContain('dissolve it if you are its last one')
  })

  test('THE CEO IS NEVER OFFERED IT: the root department is never vacated', () => {
    const manifest = tribesCapital()
    // The one exemption in the whole model, and the one place this copy could
    // do real harm: the CEO always heads the root, so a sentence offering the
    // vacate path would send the single person who cannot take it down it.
    const text = personAuthorityText(manifest, personOf(manifest, 'ceo'))
    const advice = hiringPathAdvice(manifest, personOf(manifest, 'ceo'))
    expect(text).not.toContain('vacate')
    expect(advice).not.toContain('vacates')
    expect(advice).toContain('You always head the company root')
  })
})

describe('a person who heads none reports the accepted path', () => {
  test('the general-staff person is told to add a department beneath itself', () => {
    const manifest = tribesCapital()
    const text = personAuthorityText(manifest, personOf(manifest, 'carlos'))
    expect(text).toContain('heads no department')
    expect(text).toContain('may add one under executive with org_add_department')
    expect(text).toContain('as its own head')
    expect(text).toContain('then hire into it')
  })

  test('the path named is the one the gate accepts, not a hire it would refuse', () => {
    const manifest = tribesCapital()
    // Carlos cannot hire into `executive`; the roster must not imply he can.
    expect(departmentScopeDenial(manifest, personOf(manifest, 'carlos'), 'executive')).toBe(
      'out-of-scope'
    )
    expect(personAuthorityText(manifest, personOf(manifest, 'carlos'))).not.toContain(
      'may hire at or under'
    )
  })
})

describe('THE REGRESSION: tonight, the CEO decided Carlos could not do this', () => {
  test('the roster shows the Chief of Staff able to add a department beneath himself', () => {
    const manifest = tribesCapital()
    const line = rosterLine(manifest, 'carlos')
    expect(line).toContain('authority: heads no department')
    expect(line).toContain('may add one under executive with org_add_department')
  })

  test('nothing in the roster says a job title decides authority', () => {
    const manifest = tribesCapital()
    const roster = formatOrganizationRoster(manifest)
    expect(roster).not.toMatch(/CEO-level|head-level|does not hold/i)
  })

  test('every active person carries exactly one compact authority field', () => {
    const manifest = tribesCapital()
    for (const personId of ['ceo', 'carlos', 'rhea', 'dov']) {
      const line = rosterLine(manifest, personId)
      expect(line.split('authority:').length - 1, `${personId} authority fields`).toBe(1)
      expect(
        line.slice(line.indexOf('authority:')).length,
        `${personId} field length`
      ).toBeLessThanOrEqual(160)
    }
  })

  test('a departed person carries no authority field: nobody is there to act', () => {
    const manifest = tribesCapital()
    expect(rosterLine(manifest, 'gone')).not.toContain('authority:')
  })
})

describe('the roster AGREES with the gate, for every person and every department', () => {
  test('the admitted set is exactly the set the gate admits', () => {
    const manifest = tribesCapital()
    for (const personId of manifest.peopleOrder) {
      const subject = personOf(manifest, personId)
      const admitted = new Set(personAuthority(manifest, subject).hireDepartmentIds)
      for (const departmentId of Object.keys(manifest.departments)) {
        const denial = departmentScopeDenial(manifest, subject, departmentId)
        expect(admitted.has(departmentId), `${personId} vs ${departmentId}`).toBe(isNullish(denial))
      }
    }
  })

  test('the WORDS mean that same set: "anywhere", "at or under X", or none', () => {
    const manifest = tribesCapital()
    for (const personId of manifest.peopleOrder) {
      const subject = personOf(manifest, personId)
      const view = personAuthority(manifest, subject)
      const text = personAuthorityText(manifest, subject)
      const admitted = new Set(view.hireDepartmentIds)
      const every = Object.keys(manifest.departments)
      if (text.includes('may hire anywhere in the company')) {
        expect(
          every.every((id) => admitted.has(id)),
          `${personId} claims the company`
        ).toBe(true)
        continue
      }
      const claimedRoot = /may hire at or under ([\w-]+)/.exec(text)?.[1]
      if (!isNullish(claimedRoot)) {
        expect([...admitted].sort(), `${personId} claims a subtree`).toEqual(
          subtreeIds(manifest, claimedRoot).sort()
        )
        continue
      }
      expect(admitted.size, `${personId} claims nothing`).toBe(0)
    }
  })

  test('the head named in the roster is the head the refusal copy names', () => {
    const manifest = tribesCapital()
    for (const personId of manifest.peopleOrder) {
      const subject = personOf(manifest, personId)
      const headed = personAuthority(manifest, subject).headedDepartmentId
      const advice = hiringPathAdvice(manifest, subject)
      if (isNullish(headed)) {
        expect(advice, `${personId} advice`).toContain('You head no department yet')
        expect(personAuthorityText(manifest, subject)).toContain('heads no department')
        continue
      }
      expect(advice, `${personId} advice`).toContain(`You head '${headed}'`)
      expect(personAuthorityText(manifest, subject)).toContain(`heads ${headed}`)
    }
  })

  test('a company with one person and no departments beneath it still reads truthfully', () => {
    const manifest = tribesCapital()
    manifest.departmentOrder = ['executive']
    manifest.peopleOrder = ['ceo']
    delete manifest.departments.research
    delete manifest.departments['research-data']
    const text = personAuthorityText(manifest, personOf(manifest, 'ceo'))
    expect(text).toContain('heads executive')
    expect(text).toContain('may hire anywhere in the company')
  })
})
