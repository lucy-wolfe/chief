/**
 * A MISSING SUBJECT WAS REPORTED AS A MISSING PERMISSION.
 *
 * The live failure, on the operator's own box: a CEO created a department with
 * a new head and staff, went to bring the three of them up, and got
 *
 *     Start failed · 'ceo' does not manage person 'ada-lovelace'
 *
 * That sentence cannot be true, and the proof is in this file. The CEO heads
 * the ROOT department, so its subtree is the whole company; `departmentScopeDenial`
 * short-circuits on `kind === "executive"` before it walks a single parent
 * edge. There is no tree shape in which a CEO fails a scope check.
 *
 * So the refusal came from the OTHER half of the same `if`:
 *
 *     if (!target || !departmentIsInScope(...)) throw `does not manage person`
 *
 * `ada-lovelace` was absent from the manifest that call had read. An absent
 * person is a missing SUBJECT — the roster this call saw predates them — and it
 * was rendered as a permission the caller already held. The operator read it as
 * the tree model being broken and went hunting authority they had never lost.
 *
 * This is precisely the defect #1048 fixed for DEPARTMENTS and nobody carried
 * across to PEOPLE. Its comment on `DepartmentScopeDenial` says it outright:
 * "An authority message for a typo sends the caller hunting a permission it
 * already holds." The two answers were kept apart there and collapsed here.
 *
 * The tests below pin the RULE in both directions, because a fix that only made
 * the message nicer would leave the real gate free to drift:
 *
 *   1. A head's authority IS its whole subtree, at every depth — the property
 *      the operator was told did not hold.
 *   2. An absent person produces a refusal that does NOT claim an authority
 *      problem, and says what to do.
 *   3. A genuine out-of-scope refusal still happens, and still names management.
 *
 * REAL: the extension module's own exported scope predicate and its source.
 * FAKE: nothing — these are pure over a fixture manifest.
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { withoutComments } from '@test/support/TypeScriptSource'
import type { IntercomOrganizationManifest, PersonRecord } from '@test-assets/organization-intercom'
import { departmentScopeDenial } from '@test-assets/organization-intercom'
import { describe, expect, it } from 'vitest'

const SLUG = 'absent-person'
const CREATED_AT = '2026-01-01T00:00:00.000Z'
const SOURCE_PATH = fileURLToPath(
  new URL('../../extensions/organization-intercom.ts', import.meta.url)
)

/** The person the operator could not start. Deliberately named after the live
 *  incident so a future reader can find the report from the test. */
const ADA = 'ada-lovelace'

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
 * THREE LEVELS, on purpose. The incident happened one level down, but the claim
 * the operator made is about every level: "every head of a tree should be able
 * to manage all its subagents, anything in the subagents."
 *
 * executive (ceo)
 *   └── engineering (eng-head)
 *         └── research (ADA)  ← plus two researchers
 */
function manifest(): IntercomOrganizationManifest {
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: SLUG,
    name: 'Absent Person',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'engineering', 'research'],
    peopleOrder: ['ceo', 'eng-head', ADA, 'researcher-alpha', 'researcher-beta'],
    departments: {
      executive: {
        id: 'executive',
        name: 'Absent Person',
        purpose: 'Run the company.',
        headPersonId: 'ceo',
        state: 'active'
      },
      engineering: {
        id: 'engineering',
        name: 'Engineering',
        purpose: 'Build the product.',
        parentDepartmentId: 'executive',
        headPersonId: 'eng-head',
        state: 'active'
      },
      research: {
        id: 'research',
        name: 'Research',
        purpose: 'Study the market.',
        parentDepartmentId: 'engineering',
        headPersonId: ADA,
        state: 'active'
      }
    },
    people: {
      ceo: person('ceo', 'executive', 'executive'),
      'eng-head': person('eng-head', 'head', 'engineering'),
      [ADA]: person(ADA, 'head', 'research'),
      'researcher-alpha': person('researcher-alpha', 'worker', 'research'),
      'researcher-beta': person('researcher-beta', 'worker', 'research')
    }
  }
}

function personOf(tree: IntercomOrganizationManifest, id: string): PersonRecord {
  const found = tree.people[id]
  if (!found) throw new Error(`the fixture has no person '${id}'`)
  return found
}

/** `requireManagedTarget`'s body, comments stripped. */
function managedTargetBody(): string {
  const source = withoutComments(readFileSync(SOURCE_PATH, 'utf8'))
  const start = source.indexOf('function requireManagedTarget(')
  expect(start, 'requireManagedTarget must still exist').toBeGreaterThan(-1)
  return source.slice(start, source.indexOf('\n}', start))
}

describe('an absent person is not an authority refusal', () => {
  it('a head manages its whole subtree, at every depth', () => {
    // The operator's claim, asserted as a property rather than one example.
    const tree = manifest()
    const ceo = personOf(tree, 'ceo')
    for (const departmentId of tree.departmentOrder) {
      expect(
        departmentScopeDenial(tree, ceo, departmentId),
        `the CEO heads the root, so '${departmentId}' is inside its subtree`
      ).toBeUndefined()
    }

    // And a head one level down reaches its OWN subtree, including the
    // grandchild department — but never upward or sideways.
    const engHead = personOf(tree, 'eng-head')
    expect(departmentScopeDenial(tree, engHead, 'engineering')).toBeUndefined()
    expect(
      departmentScopeDenial(tree, engHead, 'research'),
      'a nested department is still inside the subtree its parent heads'
    ).toBeUndefined()
    expect(
      departmentScopeDenial(tree, engHead, 'executive'),
      'upward is the one direction the tree model forbids'
    ).toBe('out-of-scope')
  })

  it('no tree shape makes a CEO fail a scope check on a person who exists', () => {
    // The direct refutation of the message the operator saw. Every person in
    // the fixture, checked through the department they sit in.
    const tree = manifest()
    const ceo = personOf(tree, 'ceo')
    for (const personId of tree.peopleOrder) {
      const target = personOf(tree, personId)
      expect(
        departmentScopeDenial(tree, ceo, target.departmentId),
        `'ceo' must manage '${personId}'`
      ).toBeUndefined()
    }
  })

  it('the person gate answers ABSENT and OUT-OF-SCOPE as two different things', () => {
    // The structural half. A single `if (!target || !departmentIsInScope(...))`
    // is what produced an authority sentence for a person who did not exist, so
    // the two conditions may not share a branch again.
    const body = managedTargetBody()
    expect(
      body,
      'the absent-person and out-of-scope branches must not be one condition'
    ).not.toMatch(/!target\s*\|\|/)
    expect(body).toContain('departmentIsInScope')
  })

  it('the absent-person refusal does not claim an authority problem', () => {
    const body = managedTargetBody()
    const absent = body.slice(body.indexOf('if (!target)'), body.indexOf('departmentIsInScope'))
    expect(absent, 'it must say the person does not exist').toContain('exists in this company')
    expect(
      absent,
      'and must explicitly disclaim authority, because that is what was misread'
    ).toContain('not an authority refusal')
    expect(absent, 'the old sentence must not be reachable from the absent branch').not.toContain(
      'does not manage person'
    )
  })

  it('a real out-of-scope refusal still names the management relation', () => {
    // The half that must survive: peers and members are still refused, and the
    // refusal still says WHY in the vocabulary of the tree.
    const tree = manifest()
    const alpha = personOf(tree, 'researcher-alpha')
    expect(
      departmentScopeDenial(tree, alpha, 'engineering'),
      'a worker heads nothing, so it reaches no department at all'
    ).toBe('out-of-scope')

    const body = managedTargetBody()
    const scoped = body.slice(body.indexOf('departmentIsInScope'))
    expect(scoped).toContain('does not manage person')
  })

  it('an unknown department is still separated from an out-of-scope one', () => {
    // #1048, restated here because this file is the person-shaped twin of it.
    // If these ever collapse again, the same class of bug returns.
    const tree = manifest()
    expect(departmentScopeDenial(tree, personOf(tree, 'ceo'), 'no-such-department')).toBe(
      'unknown-department'
    )
  })
})
