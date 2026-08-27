/**
 * A HIRE IS A PERSON, NOT A BOX TO PUT THEM IN.
 *
 * Observed live on two companies. On one live box, 2026-08-21T13:55:52Z, one
 * instruction to staff a team produced THREE departments in the same second —
 * `growth`, `marketing`, `social-media` — each holding exactly one person, and
 * the operator deleted all three fourteen minutes later. On
 * another live box at 16:10:15Z, "hire a chief of staff" hit
 * `/v1/org/department/create` and NOT `/v1/org/person/hire`, producing a
 * `chief-of-staff` department containing one person titled "Chief of Staff".
 *
 * Nothing in the product was broken, and that is the point of this file.
 * `org_hire` takes a `departmentId` and places the person there; no code
 * validates or objects to a senior-sounding title. That same roster
 * holds `bro`, titled "Head of Recruiting", living in `executive` — the code
 * never had a problem with it.
 *
 * THE COPY WAS THE CAUSE. Two emphatic rules told the agent that a head is made
 * by creating a department ("never hire a head here first", "`org_add_department`
 * creates the head"), and nothing anywhere told it WHEN a department is
 * warranted. So a title that reads as head-shaped — "Chief of Staff", "Head of
 * Growth" — was enough to make a unit, because the only compliant way the agent
 * knew to create a head was to create a department around them.
 *
 * The word "head" was doing two jobs: head OF A DEPARTMENT, which is
 * structural, and "a senior-sounding job", which is a title. This pins the
 * distinction where the agent decides — the two tool descriptions and the
 * `departmentId` argument read at every hire — plus the skill, because a skill
 * that explains it while the argument beside the cursor says nothing is a fix
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

function property(value: unknown, key: string): unknown {
  if (isNullish(value) || typeof value !== 'object') return undefined
  return Reflect.get(value, key)
}

/** One argument's `description`, off the SERIALIZED schema — that string is
 *  what the provider hands the model. */
function argumentDescription(name: string, argument: string): string {
  /* eslint-disable lucy/no-json-stringify */
  // The standard encoder IS the transformation under test: this asserts what a
  // provider is handed, and reading the schema any other way would prove
  // something about TypeBox rather than about the copy. Same exemption
  // `ToolParameterSchemas.test.ts` documents.
  const encoded = JSON.stringify(tool(name).parameters ?? null)
  /* eslint-enable lucy/no-json-stringify */
  const decoded: unknown = JSON.parse(encoded)
  const description = property(property(property(decoded, 'properties'), argument), 'description')
  return typeof description === 'string' ? description : ''
}

describe('a hire lands in an existing department unless the operator asked otherwise', () => {
  it('the departmentId argument names the default, and it is the department you head', () => {
    const description = argumentDescription('org_hire', 'departmentId')
    expect(description).not.toBe('')
    expect(description).toMatch(/DEFAULT/)
    expect(description).toMatch(/you head/i)
  })

  it('the departmentId argument says this call creates no department', () => {
    const description = argumentDescription('org_hire', 'departmentId')
    expect(description).toMatch(/never creates a department/i)
    // The exact inference that produced both live incidents.
    expect(description).toContain('Chief of Staff')
  })

  it('org_hire says a new department is the operator’s decision, never inferred', () => {
    const description = tool('org_hire').description ?? ''
    expect(description).toMatch(/OPERATOR'S DECISION/i)
    expect(description).toMatch(/never yours to infer/i)
    // "never hire a head here first" survives, but ONLY as the ordering rule it
    // is — scoped to a department the operator actually asked for.
    expect(description).toMatch(/if they did not/i)
  })

  it('org_hire says a senior title is a title and not a request for a unit', () => {
    const description = tool('org_hire').description ?? ''
    expect(description).toContain('TITLES')
    expect(description).toContain('Head of Growth')
  })

  it('org_add_department is gated on the operator asking for one', () => {
    const description = tool('org_add_department').description ?? ''
    expect(description).toMatch(/ONLY when the operator asked/i)
    expect(description).toMatch(/hired into an existing department with org_hire/i)
  })

  it('the skill teaches the default before it teaches how to create a department', () => {
    const landsAt = SKILL.indexOf('A hire lands in the department you head')
    const createsAt = SKILL.indexOf('`org_add_department` creates the head.')
    expect(landsAt).toBeGreaterThan(-1)
    expect(createsAt).toBeGreaterThan(-1)
    expect(landsAt).toBeLessThan(createsAt)
  })

  it('the skill scopes the create-the-head rule to an ORDERING rule', () => {
    expect(SKILL).toMatch(/ORDERING rule/)
    expect(SKILL).toMatch(/not a reason to create one/i)
  })

  it('the skill carries the operator’s own two sentences, side by side', () => {
    expect(SKILL).toMatch(/hire a Chief of Staff/i)
    expect(SKILL).toMatch(/create a growth department/i)
  })
})
