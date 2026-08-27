/**
 * #1093 — A DOUBLE-ENCODED STRUCTURED ARGUMENT IS REPAIRED BEFORE VALIDATION.
 *
 * # The defect
 *
 * A live CEO called `org_add_department` and was refused:
 *
 *     Validation failed for tool "org_add_department":
 *     - department: must be object
 *     Received arguments: { "department": "{\"he…
 *
 * The model had emitted the nested `department` object as a JSON *string*. An
 * identical structure had parsed as a real object moments earlier, so the model
 * concluded "perhaps the tool schema binding is inconsistent". It is not.
 * Nothing on the path coerces a string into an object — the Pi runtime's
 * `Value.Convert` is a documented no-op for non-objects, and its primitive
 * coercion has no `object` case. The tools that survived a fumble were the ones
 * carrying a `prepareArguments` seam, which Pi calls BEFORE TypeBox validation.
 * `org_add_department` and `org_hire` had none, so they were the two strictest
 * surfaces in the family with the least repair.
 *
 * #1150 went one step further for `org_add_department`: the `department`
 * wrapper is DELETED, because the same fumble also arrived as an upstream
 * `tool arguments invalid: trailing characters`, which the provider raises
 * before this seam can run. The seam still guards the `head` seed and the
 * `staff` entries — the objects that remain — and the schema fixture below is
 * a synthetic one, so it goes on exercising nesting the real tool no longer
 * asks for.
 *
 * # The rule these tests pin
 *
 * The repair is SCHEMA-GUIDED, never key-guided: a string is parsed only where
 * the schema declares an object or an array, at any depth. A field the schema
 * declares as a string is never touched, whatever it happens to contain, and a
 * value that does not parse — or parses to something that is not an object — is
 * returned unchanged so the ordinary refusal still fires.
 */

import { captureRegisteredTools } from '@test/support/ToolRegistrationHarness'
import type { ToolRegistrationCapture } from '@test/types/ToolRegistrationHarness'
import { unwrapStringifiedArguments } from '@test-assets/organization-intercom'
import { afterAll, beforeAll, describe, expect, test } from 'vitest'

/* eslint-disable lucy/no-json-stringify */
// @tribes-terminal/foundation (toJsonTreeString/ensureJsonTreeString) is
// private to the sibling `terminal` repo this rule was ported from and is not a
// dependency anywhere in this workspace — the same exemption
// `ToolParameterSchemas.test.ts` and `support/CompanyRendezvous.ts` document. Here the
// STANDARD encoder is additionally the subject: a double-encoded argument is
// literally what `JSON.stringify` produced inside the model, so reproducing the
// live failure with a different encoder would not reproduce it at all.
const encode = (value: unknown): string => JSON.stringify(value)
/* eslint-enable lucy/no-json-stringify */

const DEPARTMENT_PARAMS = {
  type: 'object',
  properties: {
    parentDepartmentId: { type: 'string' },
    department: {
      type: 'object',
      properties: {
        name: { type: 'string' },
        purpose: { type: 'string' },
        head: {
          type: 'object',
          properties: { name: { type: 'string' }, mandate: { type: 'string' } }
        },
        staff: {
          type: 'array',
          items: { type: 'object', properties: { name: { type: 'string' } } }
        }
      }
    }
  }
}

describe('unwrapStringifiedArguments', () => {
  test('the exact live failure — a stringified `department` — becomes an object', () => {
    const department = {
      name: 'Diagnostics',
      purpose: 'Find faults.',
      head: { name: 'Ada', mandate: 'Lead.' }
    }
    const repaired = unwrapStringifiedArguments(DEPARTMENT_PARAMS, {
      department: encode(department)
    })
    expect(repaired).toEqual({ department })
  })

  test('a nested `head` and a `staff` entry are repaired at their own depth', () => {
    const repaired = unwrapStringifiedArguments(DEPARTMENT_PARAMS, {
      department: {
        name: 'Diagnostics',
        purpose: 'Find faults.',
        head: encode({ name: 'Ada', mandate: 'Lead.' }),
        staff: [encode({ name: 'Bo' }), { name: 'Cy' }]
      }
    })
    expect(repaired).toEqual({
      department: {
        name: 'Diagnostics',
        purpose: 'Find faults.',
        head: { name: 'Ada', mandate: 'Lead.' },
        staff: [{ name: 'Bo' }, { name: 'Cy' }]
      }
    })
  })

  test('a whole stringified argument envelope is repaired too', () => {
    const repaired = unwrapStringifiedArguments(
      DEPARTMENT_PARAMS,
      encode({
        parentDepartmentId: 'executive',
        department: { name: 'Ops', purpose: 'Run it.' }
      })
    )
    expect(repaired).toEqual({
      parentDepartmentId: 'executive',
      department: { name: 'Ops', purpose: 'Run it.' }
    })
  })

  test('a field the schema declares as a string is NEVER parsed', () => {
    // A purpose that happens to look like JSON is still a purpose. Key-guided
    // repair would corrupt this; schema-guided repair cannot.
    const hostile = {
      parentDepartmentId: '{"not":"an id"}',
      department: { name: 'Ops', purpose: '{"still":"prose"}' }
    }
    expect(unwrapStringifiedArguments(DEPARTMENT_PARAMS, hostile)).toEqual(hostile)
  })

  test('unparseable or non-object text is left alone so the normal refusal fires', () => {
    for (const department of ['not json at all', '"a bare string"', 'null', '7']) {
      expect(unwrapStringifiedArguments(DEPARTMENT_PARAMS, { department })).toEqual({
        department
      })
    }
  })

  test('a well-formed call passes through unchanged', () => {
    const good = {
      parentDepartmentId: 'executive',
      department: { name: 'Ops', purpose: 'Run it.' }
    }
    expect(unwrapStringifiedArguments(DEPARTMENT_PARAMS, good)).toEqual(good)
  })

  /**
   * A silent repair is where a regression hides. If a model began double-
   * encoding on EVERY call, or a new tool shipped with the wrong shape, an
   * unobservable seam would paper over it forever and no suite would go red.
   * The seam must therefore be COUNTABLE, and it must count nothing when there
   * was nothing to fix — an always-firing trace is as useless as none.
   */
  describe('every repair is reported by schema path, and only a real repair is', () => {
    const repairsFor = (value: unknown): string[] => {
      const seen: string[] = []
      unwrapStringifiedArguments(DEPARTMENT_PARAMS, value, (at) => seen.push(at))
      return seen
    }

    test('a nested repair reports the path it fixed', () => {
      expect(
        repairsFor({
          department: {
            name: 'Ops',
            purpose: 'Run it.',
            head: encode({ name: 'Ada', mandate: 'Lead.' }),
            staff: [encode({ name: 'Bo' }), { name: 'Cy' }]
          }
        })
      ).toEqual(['department.head', 'department.staff[0]'])
    })

    test('the whole envelope reports the empty path', () => {
      expect(repairsFor(encode({ department: { name: 'Ops', purpose: 'Run it.' } }))).toEqual([''])
    })

    test('a well-formed call reports NOTHING', () => {
      expect(
        repairsFor({
          parentDepartmentId: 'executive',
          department: { name: 'Ops', purpose: 'Run it.' }
        })
      ).toEqual([])
    })

    test('a value that could not be repaired reports nothing either', () => {
      expect(repairsFor({ department: 'not json at all' })).toEqual([])
      expect(repairsFor({ department: 'null' })).toEqual([])
    })
  })
})

/**
 * The helper is worthless unless the two tools that failed actually carry it.
 * This installs the real extension and reads the registered declarations, so a
 * tool that loses its seam fails here rather than on a live CEO's first turn.
 */
describe('the tools that take a nested object carry the pre-validation seam', () => {
  let capture: ToolRegistrationCapture

  beforeAll(async () => {
    capture = await captureRegisteredTools()
  })
  afterAll(async () => {
    await capture.stop()
  })

  test.each([
    // #1150 flattened `org_add_department`: there is no `department` wrapper
    // any more, so the object a model can still fumble is the head seed.
    ['org_add_department', 'head', { name: 'Ada', mandate: 'Lead.' }],
    ['org_hire', 'person', { name: 'Ada', mandate: 'Lead.' }]
  ])('%s repairs a stringified `%s`', (name, key, payload) => {
    const tool = capture.tools.find((candidate) => candidate.name === name)
    expect(tool, `${name} must be registered`).toBeDefined()
    const prepare = tool?.prepareArguments
    expect(typeof prepare, `${name} must declare prepareArguments`).toBe('function')
    if (typeof prepare !== 'function') return
    const repaired: unknown = prepare({ departmentId: 'executive', [key]: encode(payload) })
    expect(repaired).toMatchObject({ [key]: payload })
  })
})
