/**
 * "REPORTS TO X" IS A STRUCTURAL INSTRUCTION, AND THE DECISION SURFACE SAYS SO.
 *
 * Observed live: the operator booted a Chief of Staff, then said *"I want to
 * boot an engineering team, and I want the head of engineering to report to
 * Carlos."* Carlos held no headship, the agent read that as "Carlos is a
 * worker, so he cannot be a parent", and it parked Engineering in the executive
 * branch.
 *
 * Nothing in the product was broken. `org_add_department` takes
 * `existingHeadPersonId`, which MOVES an existing person into the department
 * they will head, and `staffingAuthority` has no role gate — a worker becoming
 * a manager is an ordinary create. The agent simply had nothing that named the
 * move, and the one line it did read said "omit parentDepartmentId for your own
 * management root", which is the exact wrong default for a request that named
 * somebody.
 *
 * So the copy is pinned where the agent DECIDES: the `org_add_department`
 * description and its two arguments, read at every structural call, plus the
 * skill's worked example of the operator's own sentence. A skill that explains
 * it beautifully while the argument beside the cursor says nothing is a fix
 * that never fires.
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { isNullish } from '@test/support/Nullish'
import { captureRegisteredTools } from '@test/support/ToolRegistrationHarness'
import type { CapturedTool, ToolRegistrationCapture } from '@test/types/ToolRegistrationHarness'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

const SKILL = readFileSync(
  fileURLToPath(new URL('../skills/manager/SKILL.md', import.meta.url)),
  'utf8'
)

let capture: ToolRegistrationCapture

beforeAll(async () => {
  capture = await captureRegisteredTools()
}, 30_000)

afterAll(async () => {
  await capture?.stop()
})

function tool(name: string): CapturedTool {
  const found = capture.tools.find((candidate) => candidate.name === name)
  if (isNullish(found)) throw new Error(`the install registered no '${name}'`)
  return found
}

/** One key of an unknown value, or `undefined` when it is not an object. */
function property(value: unknown, key: string): unknown {
  if (isNullish(value) || typeof value !== 'object') return undefined
  return Reflect.get(value, key)
}

/** One argument's `description`, off the SERIALIZED schema — that string is
 *  what the provider hands the model, and reading it any other way would prove
 *  something about TypeBox rather than about the copy. */
function argumentDescription(name: string, argument: string): string {
  /* eslint-disable lucy/no-json-stringify */
  // Same exemption `ToolParameterSchemas.test.ts` documents: the standard
  // encoder IS the transformation under test.
  const encoded = JSON.stringify(tool(name).parameters ?? null)
  /* eslint-enable lucy/no-json-stringify */
  const decoded: unknown = JSON.parse(encoded)
  const description = property(property(property(decoded, 'properties'), argument), 'description')
  return typeof description === 'string' ? description : ''
}

describe('org_add_department teaches where a named person puts a department', () => {
  it('parentDepartmentId says a named person is the parent, and the root is only the fallback', () => {
    const description = argumentDescription('org_add_department', 'parentDepartmentId')
    expect(description).toContain('reports to')
    expect(description).toContain('heads')
    // The old copy offered the root with no condition on it. The root stays a
    // real default; what is pinned is that it is CONDITIONAL on nobody being
    // named, because the unconditional form is what produced the defect.
    expect(description).toMatch(/named nobody|nobody was named/i)
  })

  it('existingHeadPersonId says an appointment MOVES the person and needs no standing', () => {
    const description = argumentDescription('org_add_department', 'existingHeadPersonId')
    expect(description).toContain('MOVED')
    expect(description).toMatch(/worker becomes a manager/i)
  })

  it('the tool description names the two-step move for a person who heads nothing', () => {
    const description = tool('org_add_department').description ?? ''
    expect(description).toContain('STRUCTURAL')
    expect(description).toContain('existingHeadPersonId')
    expect(description).toContain('parentDepartmentId')
    // The refusal shape that was invented on the live box: "Carlos is only a
    // worker, so the team goes at the root".
    expect(description).toMatch(/not a blocker/i)
  })

  it('no surface here invents a role gate', () => {
    for (const name of ['org_add_department', 'org_reparent_department', 'org_hire']) {
      const description = tool(name).description ?? ''
      for (const banned of ['CEO-level', 'head-level', 'Only the CEO']) {
        expect(description, `${name} must not claim a job-title gate`).not.toContain(banned)
      }
    }
  })
})

describe('the skill works the operator sentence through, end to end', () => {
  it('quotes the request and reads it as structure', () => {
    expect(SKILL).toContain('report to Carlos')
    expect(SKILL).toContain('structural instruction')
  })

  it('makes the named person a head FIRST, then attaches the team beneath', () => {
    const promote = SKILL.indexOf('"existingHeadPersonId": "carlos"')
    const attach = SKILL.indexOf('"parentDepartmentId": "office-of-the-chief-of-staff"')
    expect(promote).toBeGreaterThan(-1)
    expect(attach).toBeGreaterThan(-1)
    expect(attach).toBeGreaterThan(promote)
  })

  it('states that the promotion is ordinary and that it moves the person', () => {
    expect(SKILL).toMatch(/worker becoming a manager is ordinary/i)
    expect(SKILL).toContain('MOVES Carlos')
  })

  it('keeps the defect itself on the observed-mistakes list', () => {
    expect(SKILL).toContain('org_reparent_department')
    expect(SKILL).toMatch(/landed in the executive branch/i)
  })
})
