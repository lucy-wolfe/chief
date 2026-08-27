/**
 * #1011 — EVERY REGISTERED TOOL'S PARAMETERS SERIALIZE TO A TOP-LEVEL OBJECT.
 *
 * # The defect
 *
 * Three tools declared `parameters: Type.Union([Type.Object(…), …])`. TypeBox
 * serializes a union to `{"anyOf":[…]}` with NO top-level `type`, and a strict
 * provider refuses the whole request on it:
 *
 *     400 Invalid schema for function 'org_maintain_session':
 *     schema must be a JSON Schema of 'type: "object"', got 'type: null'
 *
 * The refusal is for the CATALOG, not for one tool, so a CEO booted correctly
 * — identity, tools, its 🎯 schedule — and then died on its very first call.
 * The product could not run an agent at all.
 *
 * # Why every suite was green
 *
 * Nothing serialized a tool schema. That is the second most expensive defect
 * shape in this codebase's history: an instrument that cannot see its subject.
 * The suites drove tools' `execute` and never looked at what is SENT to the
 * provider, so the declaration had no reader anywhere in CI.
 *
 * # Why this test enumerates rather than names
 *
 * A test for the three known-bad tools does not stop the fourth. This one
 * installs the real extension, takes every `registerTool` call it makes, and
 * checks all of them — so a tool added tomorrow with a union schema fails here
 * on the day it is written, not on the first live turn of the next CEO.
 *
 * It needs no provider, no credential and no built binary: the check is
 * `JSON.stringify` over the declaration, which is exactly the transformation
 * the provider client performs.
 */
import { isNullish } from '@test/support/Nullish'
import { captureRegisteredTools } from '@test/support/ToolRegistrationHarness'
import type { CapturedTool, ToolRegistrationCapture } from '@test/types/ToolRegistrationHarness'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

/** The broken one that survives. Named ONLY as a non-vacuity anchor for the
 *  enumeration below — no assertion singles it out. (`org_set_thinking` was one
 *  of three; it went with the rest of the model business. `org_maintain_session`
 *  was another; the operator's 2026-08-24 ruling deleted the whole tool.) */
const KNOWN_BROKEN = ['org_launch_department']

/**
 * A tool's declared parameters as the wire carries them.
 *
 * SERIALIZED, not read off the TypeBox object: `JSON.stringify` is the
 * transformation the provider client applies, and it is where a union's
 * missing `type` becomes visible. Reading `.type` off the in-memory schema
 * would prove the same thing one step earlier and would not survive an encoder
 * that later drops or renames a key.
 *
 * `undefined` when the declaration is not a JSON object at all, which is
 * itself a failure every assertion below reports by name.
 */
function serializedSchema(tool: CapturedTool): Record<string, unknown> | undefined {
  /* eslint-disable lucy/no-json-stringify */
  // @tribes-terminal/foundation (toJsonTreeString/ensureJsonTreeString) is
  // private to the sibling `terminal` repo this rule was ported from and is
  // not a dependency anywhere in this workspace — the same exemption
  // `support/CompanyRendezvous.ts` documents. Round-tripping through the STANDARD
  // encoder is the whole point here: a helper with its own rules would not be
  // the transformation under test.
  const encoded = JSON.stringify(isNullish(tool.parameters) ? null : tool.parameters)
  /* eslint-enable lucy/no-json-stringify */
  const decoded: unknown = JSON.parse(encoded)
  if (isNullish(decoded) || typeof decoded !== 'object' || Array.isArray(decoded)) return undefined
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // `JSON.parse` returns `any`; the three structural checks above are the
  // narrowing, and there is no predicate form of them that TypeScript can
  // apply to an index signature.
  return decoded as Record<string, unknown>
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

let capture: ToolRegistrationCapture

beforeAll(async () => {
  capture = await captureRegisteredTools()
}, 30_000)

afterAll(async () => {
  await capture?.stop()
})

describe('#1011 — a tool catalog a strict provider will accept', () => {
  it('registers a real, non-trivial catalog (this test is not vacuous)', () => {
    // If the install ever stops registering tools, every assertion below
    // passes over an empty list and reports the catalog as healthy. The floor
    // is deliberately well under the real count so a legitimate removal does
    // not fail it, and well over zero so an install that registered nothing
    // cannot.
    expect(capture.tools.length).toBeGreaterThan(20)
    // And the install really did read the company rather than registering off
    // a default: the manifest route is the extension's first durable contact.
    expect(capture.chiefdPaths).toContain('/v1/org/manifest/read')
    const names = capture.tools.map((tool) => tool.name)
    for (const name of KNOWN_BROKEN) {
      expect(
        names,
        `${name} must be in the captured catalog for this file to mean anything`
      ).toContain(name)
    }
  })

  it('every registered tool serializes to a schema with a top-level type: "object"', () => {
    const offenders: string[] = []
    for (const tool of capture.tools) {
      const schema = serializedSchema(tool)
      if (isNullish(schema)) {
        offenders.push(`${tool.name}: parameters did not serialize to an object`)
        continue
      }
      if (schema.type !== 'object') {
        offenders.push(
          `${tool.name}: top-level type is ${String(schema.type ?? 'null')}` +
            (Array.isArray(schema.anyOf)
              ? ' (a Type.Union — declare one Type.Object and validate the branches in execute)'
              : '')
        )
      }
    }
    expect(offenders, offenders.join('\n')).toEqual([])
  })

  it('no registered tool declares its top level as anyOf/oneOf/allOf', () => {
    // The same rule stated as the shape that produced it, so a future schema
    // carrying BOTH a `type` and a top-level combinator — legal JSON Schema,
    // and still refused by the provider that raised this — cannot slip past
    // the check above.
    const offenders = capture.tools
      .filter((tool) => {
        const schema = serializedSchema(tool)
        if (isNullish(schema)) return false
        return ['anyOf', 'oneOf', 'allOf'].some((key) => Array.isArray(schema[key]))
      })
      .map((tool) => tool.name)
    expect(offenders, offenders.join(', ')).toEqual([])
  })

  it('every registered tool names its properties, so a provider has something to fill', () => {
    // A bare `{"type":"object"}` satisfies the rule above and tells a model
    // nothing. A tool that genuinely takes no arguments may declare an empty
    // `properties` map, but the key must be present — an absent one is the
    // same silence with less intent.
    const offenders = capture.tools
      .filter((tool) => {
        const schema = serializedSchema(tool)
        if (isNullish(schema)) return true
        const properties = schema.properties
        return isNullish(properties) || typeof properties !== 'object' || Array.isArray(properties)
      })
      .map((tool) => tool.name)
    expect(offenders, offenders.join(', ')).toEqual([])
  })

  it('no tool description exceeds the 1024 characters a strict provider accepts', () => {
    // The SAME defect class as the union schema above, one field over: a
    // function description longer than 1024 characters is refused by strict
    // providers, and a refused definition kills the WHOLE catalog rather than
    // the one tool. #1150 put a worked example argument object into the two
    // hire-shaped descriptions — which is where a calling rule belongs, since a
    // model that never opens a skill still reads them — and that is exactly the
    // kind of edit that walks a description past a cap nobody was measuring.
    const offenders = capture.tools
      .filter((tool) => (tool.description ?? '').length > 1024)
      .map((tool) => `${tool.name}: ${(tool.description ?? '').length}`)
    expect(offenders, offenders.join(', ')).toEqual([])
    // Non-vacuity: the descriptions are really being read, not defaulted away.
    expect(
      Math.max(...capture.tools.map((tool) => (tool.description ?? '').length)),
      'no captured tool carries a description at all'
    ).toBeGreaterThan(200)
  })
})

/**
 * #1150 — `org_add_department` IS FLAT, AND THE STRICTEST SURFACE IS THE
 * SHALLOWEST.
 *
 * # The defect
 *
 * A live CEO could not create a department. Twice, the same fumble:
 *
 *     Validation failed for tool "org_add_department":
 *     - department: must be object
 *     Received arguments: { "department": "{\"he…
 *
 *     Upstream error: tool arguments invalid for org_add_department:
 *     trailing characters
 *
 * The model double-encoded the nested `department` object. #1134 answered the
 * FIRST refusal with a `prepareArguments` repair seam, and that seam cannot
 * answer the SECOND: the provider rejects the arguments before any of our code
 * is reached, so there is no seam to run.
 *
 * # The rule
 *
 * The only repair that reaches both refusals is to stop asking for the shape
 * that gets fumbled. `name`, `purpose`, `head`, `staff`, `departmentId`,
 * `existingHeadPersonId` and `vacates` are TOP-LEVEL arguments. There is no
 * `department` wrapper, and no argument of `org_add_department` may hold an
 * object that itself holds an object — the head seed is the deepest thing the
 * tool asks a model to build.
 *
 * The seam stays: `head` is still an object, `staff` is still an array of
 * them, and a model that fumbles one of those is still repaired. What changed
 * is that the wrapper it fumbled twice no longer exists to fumble.
 */
describe('#1150 — the department-create surface is flat', () => {
  const addDepartment = (): Record<string, unknown> => {
    const tool = capture.tools.find((candidate) => candidate.name === 'org_add_department')
    expect(
      tool,
      'org_add_department must be registered for this file to mean anything'
    ).toBeDefined()
    const schema = isNullish(tool) ? undefined : serializedSchema(tool)
    expect(schema, 'org_add_department must serialize to an object schema').toBeDefined()
    return schema ?? {}
  }

  /** Every property of `schema`, as `[name, definition]` pairs. */
  const propertiesOf = (
    schema: Record<string, unknown>
  ): Array<[string, Record<string, unknown>]> => {
    const properties = schema.properties
    if (isNullish(properties) || typeof properties !== 'object' || Array.isArray(properties)) {
      return []
    }
    const pairs: Array<[string, Record<string, unknown>]> = []
    for (const [name, definition] of Object.entries(properties)) {
      if (isNullish(definition) || typeof definition !== 'object' || Array.isArray(definition)) {
        continue
      }
      /* eslint-disable @typescript-eslint/consistent-type-assertions */
      // `Object.entries` over an index signature yields `unknown`; the three
      // structural checks above are the narrowing and TypeScript has no
      // predicate form of them here.
      pairs.push([name, definition as Record<string, unknown>])
      /* eslint-enable @typescript-eslint/consistent-type-assertions */
    }
    return pairs
  }

  const propertyOf = (
    schema: Record<string, unknown>,
    name: string
  ): Record<string, unknown> | undefined =>
    propertiesOf(schema).find(([candidate]) => candidate === name)?.[1]

  /** The `items` schema of an array node, when it has one. */
  const itemsOf = (node: Record<string, unknown>): Record<string, unknown> | undefined => {
    const items = node.items
    if (isNullish(items) || typeof items !== 'object' || Array.isArray(items)) return undefined
    /* eslint-disable @typescript-eslint/consistent-type-assertions */
    // The three structural checks on the line above ARE the narrowing; a
    // decoded JSON node is `unknown` and TypeScript has no predicate form of
    // them that applies to an index signature.
    return items as Record<string, unknown>
    /* eslint-enable @typescript-eslint/consistent-type-assertions */
  }

  it('takes name, purpose and the head decision at the TOP level, with no department wrapper', () => {
    const names = propertiesOf(addDepartment()).map(([name]) => name)
    expect(names, 'the wrapper the model double-encoded must not exist').not.toContain('department')
    for (const argument of [
      'parentDepartmentId',
      'departmentId',
      'name',
      'purpose',
      'head',
      'existingHeadPersonId',
      'vacates',
      'staff'
    ]) {
      expect(names, `${argument} must be a top-level argument`).toContain(argument)
    }
  })

  it('asks a model to build nothing deeper than one person seed', () => {
    // The measurement, rather than a restatement of the property list above: no
    // argument may hold an object that holds another object. `head` and a
    // `staff` item are person seeds of flat strings and string arrays, and that
    // is the deepest thing the tool asks for.
    const tooDeep: string[] = []
    const walk = (node: Record<string, unknown>, path: string, depth: number): void => {
      if (depth > 2) {
        tooDeep.push(path)
        return
      }
      const items = itemsOf(node)
      if (!isNullish(items)) walk(items, `${path}[]`, depth)
      for (const [name, definition] of propertiesOf(node)) {
        walk(definition, path === '' ? name : `${path}.${name}`, depth + 1)
      }
    }
    walk(addDepartment(), '', 0)
    expect(tooDeep, `nested too deep: ${tooDeep.join(', ')}`).toEqual([])
  })

  it('offers only Pi builtins in person tool arrays while org_send stays automatic', () => {
    const schema = addDepartment()
    const head = propertyOf(schema, 'head')
    const staff = propertyOf(schema, 'staff')
    const staffSeed = isNullish(staff) ? undefined : itemsOf(staff)
    const seeds = [
      ['head', head],
      ['staff[]', staffSeed]
    ] as const
    const builtins = ['read', 'bash', 'edit', 'write', 'grep', 'find', 'ls']

    for (const [path, seed] of seeds) {
      expect(seed, `${path} must be a person seed`).toBeDefined()
      const tools = isNullish(seed) ? undefined : propertyOf(seed, 'tools')
      const item = isNullish(tools) ? undefined : itemsOf(tools)
      expect(item?.enum, `${path}.tools must enumerate its closed vocabulary`).toEqual(builtins)
      expect(item?.description, `${path}.tools must teach that org tools are automatic`).toMatch(
        /Never put org_\* names.*installed automatically/i
      )
      expect(item?.enum).not.toContain('org_send')
    }

    expect(
      capture.tools.map((tool) => tool.name),
      'the schema exclusion must not remove the automatic organization surface'
    ).toContain('org_send')
  })

  it('asks for no justification anywhere in the create surface', () => {
    // #1134/#1138/#1139/#1140 deleted every caller-authored reason from this
    // family. A schema that advertised one again would teach a model to spend a
    // turn writing a field the product refuses.
    const banned = /reason|rationale|justification|approval/i
    const offenders: string[] = []
    const walk = (node: Record<string, unknown>, path: string): void => {
      const items = itemsOf(node)
      if (!isNullish(items)) walk(items, `${path}[]`)
      for (const [name, definition] of propertiesOf(node)) {
        const next = path === '' ? name : `${path}.${name}`
        if (banned.test(name)) offenders.push(next)
        walk(definition, next)
      }
    }
    walk(addDepartment(), '')
    expect(offenders, offenders.join(', ')).toEqual([])
  })
})
