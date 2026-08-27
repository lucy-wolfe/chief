/**
 * THE CEO IS THE ONE EXEMPT PERSON, AND TYPESCRIPT MUST NOT INVENT A SECOND.
 *
 * Operator ruling, 2026-08-13 (`AGENTS.md`): the CEO never moves, never
 * converts into the head of another department, and always heads the root.
 * "Everyone else is fluid — including a Chief of Staff, and including anyone
 * who merely happens to be homed in the executive root. A guard that refuses a
 * structural move for anybody but the CEO is wrong."
 *
 * `org_ops.rs` widened that exemption to the WHOLE executive root — the root,
 * both CEO department chains, and the conventional `office-of-the-ceo` — and
 * says it does so for "parity with the landed TS `executiveRootUnitIds`
 * invariant". THAT INVARIANT IS GONE. `executiveRootUnitIds` lived in
 * `apps/cli/src/legacy/organization/org-units.ts` and was deleted with the
 * other ported files by `4ecc06359` (#751); its suite went with the parked
 * corpus in `73b1f0503` (#1035). TypeScript holds subtree scope and nothing
 * else, so the narrowing in Rust has no TypeScript half to lag.
 *
 * This suite is the fence that keeps it that way. It asserts the property
 * directly — a person homed in the executive root gets the SAME answer from
 * every TypeScript gate as an identically-placed person anywhere else — rather
 * than asserting the absence of one identifier, because the exemption could
 * grow back under any name.
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { isNullish } from '@test/support/Nullish'
import { withoutComments } from '@test/support/TypeScriptSource'
import { describe, expect, test } from 'vitest'

import type {
  IntercomOrganizationManifest,
  PersonRecord
} from '../extensions/organization-intercom'
import {
  departmentScopeDenial,
  personAuthority,
  personAuthorityText
} from '../extensions/organization-intercom'

const CREATED_AT = '2026-08-13T00:00:00.000Z'
const SOURCE_PATH = fileURLToPath(
  new URL('../extensions/organization-intercom.ts', import.meta.url)
)

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
 * The operator's live shape, plus its mirror image.
 *
 * `carlos` is the Chief of Staff from the incident: general staff, homed in
 * the executive root, heading nothing. `dana` is the same person one
 * department over — general staff, homed in `research`, heading nothing. The
 * whole suite is the comparison between those two.
 */
function tribesCapital(): IntercomOrganizationManifest {
  return {
    schemaVersion: 3,
    kind: 'organization',
    slug: 'tribes-capital',
    name: 'Tribes Capital',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'research'],
    peopleOrder: ['ceo', 'carlos', 'rhea', 'dana'],
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
      }
    },
    people: {
      ceo: person('ceo', 'executive', 'executive'),
      carlos: person('carlos', 'worker', 'executive'),
      rhea: person('rhea', 'head', 'research'),
      dana: person('dana', 'worker', 'research')
    }
  }
}

/** The company after each of the two grows the department the ruling says they
 *  may grow: `carlos` beneath the executive root, `dana` beneath research. */
function afterBothGrow(): IntercomOrganizationManifest {
  const manifest = tribesCapital()
  manifest.departmentOrder = ['executive', 'research', 'cos-office', 'data']
  manifest.departments['cos-office'] = {
    id: 'cos-office',
    name: 'Chief of Staff Office',
    purpose: 'Run the CEO office',
    kind: 'department',
    parentDepartmentId: 'executive',
    headPersonId: 'carlos',
    state: 'active'
  }
  manifest.departments.data = {
    id: 'data',
    name: 'Data',
    purpose: 'Keep the data',
    kind: 'department',
    parentDepartmentId: 'research',
    headPersonId: 'dana',
    state: 'active'
  }
  // chiefd's `appoint-existing` re-points home AND assigned into the new unit
  // and promotes the kind to `head` (`org_ops.rs`: `move_person` then
  // `set_person_kind`). The fixture models the state chiefd actually writes.
  manifest.people.carlos = person('carlos', 'head', 'cos-office')
  manifest.people.dana = person('dana', 'head', 'data')
  return manifest
}

describe('a root-homed person is an ordinary person', () => {
  test('the general staff of the root may grow a department beneath it', () => {
    const manifest = tribesCapital()
    // This is the acceptance shape: TypeScript already permits the create the
    // CEO said was impossible.
    expect(personAuthority(manifest, personOf(manifest, 'carlos')).createBeneathDepartmentId).toBe(
      'executive'
    )
  })

  test('the answer is the same one an identically-placed person gets elsewhere', () => {
    const manifest = tribesCapital()
    const rootHomed = personAuthority(manifest, personOf(manifest, 'carlos'))
    const elsewhere = personAuthority(manifest, personOf(manifest, 'dana'))
    expect(rootHomed.headedDepartmentId).toBe(elsewhere.headedDepartmentId)
    expect(rootHomed.hireDepartmentIds).toEqual(elsewhere.hireDepartmentIds)
    expect(rootHomed.companyWide).toBe(elsewhere.companyWide)
    expect(rootHomed.createBeneathDepartmentId).toBe('executive')
    expect(elsewhere.createBeneathDepartmentId).toBe('research')
  })

  test('the roster says the same sentence about both, with only the parent changed', () => {
    const manifest = tribesCapital()
    const rootHomed = personAuthorityText(manifest, personOf(manifest, 'carlos'))
    const elsewhere = personAuthorityText(manifest, personOf(manifest, 'dana'))
    expect(rootHomed.replace('executive', 'research')).toBe(elsewhere)
  })

  test('the scope gate refuses both the same way, for the same reason', () => {
    const manifest = tribesCapital()
    // Neither heads anything yet, so both are out of scope for their own home
    // department — a state, not a permanent condition, and not an exemption.
    expect(departmentScopeDenial(manifest, personOf(manifest, 'carlos'), 'executive')).toBe(
      'out-of-scope'
    )
    expect(departmentScopeDenial(manifest, personOf(manifest, 'dana'), 'research')).toBe(
      'out-of-scope'
    )
  })
})

describe('growing a department beneath the root grants the root nothing', () => {
  test('both new heads read identically, each rooted at their own unit', () => {
    const manifest = afterBothGrow()
    const rootChild = personAuthorityText(manifest, personOf(manifest, 'carlos'))
    const elsewhere = personAuthorityText(manifest, personOf(manifest, 'dana'))
    expect(rootChild.replace(/cos-office/g, 'data')).toBe(elsewhere)
    expect(rootChild).toContain('heads cos-office')
  })

  test('a unit under the executive root does not reach the executive root', () => {
    const manifest = afterBothGrow()
    expect(departmentScopeDenial(manifest, personOf(manifest, 'carlos'), 'executive')).toBe(
      'out-of-scope'
    )
    // Nor sideways at a peer. Growth is downward only.
    expect(departmentScopeDenial(manifest, personOf(manifest, 'carlos'), 'research')).toBe(
      'out-of-scope'
    )
    expect(personAuthority(manifest, personOf(manifest, 'carlos')).hireDepartmentIds).toEqual([
      'cos-office'
    ])
  })

  test('the CEO keeps every exemption TypeScript expresses', () => {
    const manifest = afterBothGrow()
    const ceo = personAuthority(manifest, personOf(manifest, 'ceo'))
    expect(ceo.headedDepartmentId, 'the CEO always heads the root').toBe('executive')
    expect(ceo.companyWide, 'the CEO reaches every department').toBe(true)
    for (const departmentId of Object.keys(manifest.departments)) {
      expect(
        departmentScopeDenial(manifest, personOf(manifest, 'ceo'), departmentId),
        `ceo denied '${departmentId}'`
      ).toBeUndefined()
    }
  })
})

describe('the deleted executive-root set has not grown back', () => {
  test('the extension holds no executive-root unit set and no CEO-office convention', () => {
    const code = withoutComments(readFileSync(SOURCE_PATH, 'utf8'))
    // The identifier that named the deleted TypeScript set, and the literal
    // that made it more than the root: `org_ops.rs` protects the conventional
    // `office-of-the-ceo` chain, and a TypeScript file that learned that name
    // would be re-growing the same exemption under it.
    expect(code).not.toContain('executiveRootUnitIds')
    expect(code).not.toContain('office-of-the-ceo')
  })
})
