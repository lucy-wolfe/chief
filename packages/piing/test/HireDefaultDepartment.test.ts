/**
 * THE SCHEMA MUST PERMIT WHAT THE DESCRIPTION PROMISES.
 *
 * `org_hire`'s `departmentId` was a REQUIRED field whose own description opened
 * "DEFAULT: the department YOU head". An agent read that, reasoned correctly
 * that it should omit the field, met a schema that would not allow it, and
 * improvised the most salient name in context — the company's.
 *
 * It obeyed the instrument over the claim, which is the right thing for it to
 * do. The description was a promise about the schema that the schema did not
 * implement, and prose is the one part of a tool surface no gate falsifies.
 */
import { isNullish } from '@test/support/Nullish'
import type { IntercomOrganizationManifest, PersonRecord } from '@test-assets/organization-intercom'
import {
  departmentScopeDenial,
  hireDefaultDepartmentForTest,
  unknownDepartmentMessage
} from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

function company(): IntercomOrganizationManifest {
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: 'acme-capital',
    name: 'Acme Capital',
    rootDepartmentId: 'executive',
    departmentOrder: ['executive', 'engineering'],
    peopleOrder: ['chief', 'eng-head', 'worker'],
    departments: {
      executive: {
        id: 'executive',
        name: 'Executive',
        headPersonId: 'chief',
        parentDepartmentId: undefined,
        purpose: 'Run the company.',
        state: 'active' as const
      },
      engineering: {
        id: 'engineering',
        name: 'Engineering',
        headPersonId: 'eng-head',
        parentDepartmentId: 'executive',
        purpose: 'Ship it.',
        state: 'active' as const
      }
    },
    people: {
      chief: person('chief', 'Ada', 'executive'),
      'eng-head': person('eng-head', 'Priya', 'engineering'),
      worker: person('worker', 'Dana', 'engineering')
    }
  }
}

function person(id: string, name: string, departmentId: string): PersonRecord {
  return {
    id,
    name,
    title: 'Person',
    kind: 'worker' as const,
    departmentId,
    employmentState: 'active',
    createdAt: '2026-01-01T00:00:00.000Z'
  }
}

describe('a hire with no departmentId lands in the caller’s own department', () => {
  test('a HEAD gets the department they head', () => {
    const manifest = company()

    expect(hireDefaultDepartmentForTest(manifest, manifest.people['eng-head'])).toBe('engineering')
    expect(hireDefaultDepartmentForTest(manifest, manifest.people.chief)).toBe('executive')
  })

  /**
   * THE OTHER ARM. A person who heads nothing still has a department — the one
   * they sit in — and the resolver has always had both branches. Asserting only
   * the head case would pass against a resolver that returned the headed
   * department or nothing.
   */
  test('a NON-head gets the department they sit in', () => {
    const manifest = company()

    expect(hireDefaultDepartmentForTest(manifest, manifest.people.worker)).toBe('engineering')
  })

  test('the resolved default is a department this person may actually hire into', () => {
    // The default is only worth having if it survives the scope check the hire
    // then applies to it — otherwise omitting the field would trade a guess for
    // a refusal.
    const manifest = company()
    const head = manifest.people['eng-head']
    const resolved = hireDefaultDepartmentForTest(manifest, head)
    if (isNullish(resolved)) throw new Error('the default must resolve for a head')

    expect(departmentScopeDenial(manifest, head, resolved)).toBeUndefined()
  })
})

describe('the override path still refuses a company name', () => {
  /**
   * REACHABILITY, not merely behaviour. With a default in place, the refusal is
   * the only thing standing behind an EXPLICIT departmentId — so this drives
   * the explicit path on purpose. The fixture passes the field rather than
   * omitting it, or the test would quietly become a default-path test the day
   * the default landed and stop guarding anything.
   */
  test('an explicit company name is still refused, and the refusal names the root id', () => {
    const manifest = company()
    const explicitlyPassed = manifest.slug

    expect(departmentScopeDenial(manifest, manifest.people.chief, explicitlyPassed)).toBe(
      'unknown-department'
    )

    const refusal = unknownDepartmentMessage(
      manifest,
      manifest.people.chief,
      explicitlyPassed,
      'hire into'
    )
    expect(refusal).toContain("The root department id is 'executive'")
    expect(refusal).toContain('acme-capital')
    expect(explicitlyPassed).not.toBe(hireDefaultDepartmentForTest(manifest, manifest.people.chief))
  })
})
