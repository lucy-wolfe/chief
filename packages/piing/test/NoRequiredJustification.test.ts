/**
 * #1139 — NO TOOL ASKS A CALLER TO JUSTIFY A STRUCTURAL OR EFFORT CHOICE.
 *
 * # The ruling
 *
 * > "You're removing all the reasons, right? No model reason, nothing. There's
 * > no reason to ask anything. Remove all the reasons. It's either you have
 * > permission or not. You don't need reasons for anything."
 *
 * And, on the spend gate the same family had grown:
 *
 * > "Remove any spend control. Remove that concept. If somebody switches the
 * > model, just do it. There's nothing that blocks it. If they have permission
 * > they can do it."
 *
 * Permission is the gate. Prose is not a gate, and cost is not a reason to
 * refuse an authorized caller.
 *
 * # Why this enumerates rather than names two fields
 *
 * `modelReason` and `thinkingReason` are the two this packet deleted, and a
 * test naming exactly those two would not stop the third being added next
 * week. The family is what was ruled on, so the check walks the whole
 * advertised catalog and fails on any REQUIRED property whose name reads as a
 * justification the caller must invent.
 *
 * The check is deliberately narrow in one way and wide in another. It looks at
 * REQUIRED properties only, because an optional note somebody may write is not
 * a gate. And it looks at every depth, because the two fields this packet
 * deleted both lived inside a nested seed object rather than at a tool's top
 * level — a top-level-only walk would have found neither.
 *
 * `reason` on its own is NOT an offender here, and that is a ruling rather
 * than an oversight: the structural verbs' `reason` is an AUDIT LEDGER entry
 * that `staffing_history` keeps, and the session-maintenance `reason` shares
 * its column with system-authored outcome text an operator reads on a failure.
 * A record of what happened is not a gate on who may act. Deleting the
 * REQUIREMENT to write those is a separate packet; blinding the ledger was
 * never asked for.
 */
import { isNullish } from '@test/support/Nullish'
import { captureRegisteredTools } from '@test/support/ToolRegistrationHarness'
import type { ToolRegistrationCapture } from '@test/types/ToolRegistrationHarness'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

/**
 * Property names that are a justification the caller has to invent.
 *
 * Suffix-matched rather than listed whole, so `modelReason`, `thinkingReason`,
 * `resourceRationale` (#1093) and anything shaped like them are all caught by
 * the same rule. A bare `reason` does not match — see the file header for why
 * that is deliberate.
 */
const JUSTIFICATION_SUFFIX = /(?:Reason|Rationale|Justification)$/

/** One required property that reads as a justification, and where it lives. */
interface Offender {
  readonly tool: string
  readonly path: string
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !isNullish(value) && typeof value === 'object' && !Array.isArray(value)
}

/**
 * Walk one JSON-Schema node and report every REQUIRED justification-shaped
 * property under it, at any depth.
 *
 * Depth matters: `PERSON_SEED.modelReason` sat two levels down, inside the
 * `ceo`/`head`/`staff` seed of a company or contract-unit definition, and
 * `thinkingReason` sat inside a hire seed. Neither was ever a top-level tool
 * argument.
 */
function requiredJustifications(node: unknown, tool: string, path: string): Offender[] {
  if (!isRecord(node)) return []
  const found: Offender[] = []
  const properties = node.properties
  const required = Array.isArray(node.required) ? node.required : []
  if (isRecord(properties)) {
    for (const name of Object.keys(properties)) {
      const here = path ? `${path}.${name}` : name
      if (required.includes(name) && JUSTIFICATION_SUFFIX.test(name)) {
        found.push({ tool, path: here })
      }
      found.push(...requiredJustifications(properties[name], tool, here))
    }
  }
  // Arrays carry their element schema under `items`, and both deleted fields
  // were reachable through one (`staff`, `people`).
  found.push(...requiredJustifications(node.items, tool, `${path}[]`))
  for (const key of ['anyOf', 'oneOf', 'allOf'] as const) {
    const branches = node[key]
    if (Array.isArray(branches)) {
      for (const [index, branch] of branches.entries()) {
        found.push(...requiredJustifications(branch, tool, `${path}.${key}[${index}]`))
      }
    }
  }
  return found
}

let capture: ToolRegistrationCapture

beforeAll(async () => {
  capture = await captureRegisteredTools()
}, 30_000)

afterAll(async () => {
  await capture?.stop()
})

describe('#1139 — permission is the gate, prose is not', () => {
  it('registers a real, non-trivial catalog (this test is not vacuous)', () => {
    // Without this floor every assertion below passes over an empty list and
    // reports a catalog full of justifications as clean.
    expect(capture.tools.length).toBeGreaterThan(20)
    const names = capture.tools.map((tool) => tool.name)
    // The verb this packet actually changed must be IN the capture, or its
    // fields could have survived unseen. (`org_change_thinking` was the
    // other; it is deleted with the rest of the model business.)
    expect(names).toContain('org_launch_contract')
  })

  it('no registered tool REQUIRES a caller-invented justification, at any depth', () => {
    const offenders = capture.tools.flatMap((tool) =>
      requiredJustifications(tool.parameters, tool.name, '')
    )
    expect(
      offenders.map((offender) => `${offender.tool}: ${offender.path}`),
      offenders.map((offender) => `${offender.tool} requires ${offender.path}`).join('\n')
    ).toEqual([])
  })

  it('the walk really can see a nested required justification', () => {
    // The assertion above is a proof only if the walk would have caught the
    // fields it is checking for. This is the exact shape `PERSON_SEED` had
    // inside `org_launch_contract`: two levels down, behind an array.
    const shape = {
      type: 'object',
      properties: {
        contract: {
          type: 'object',
          properties: {
            head: {
              type: 'object',
              properties: { modelReason: { type: 'string' } },
              required: ['modelReason']
            },
            staff: {
              type: 'array',
              items: {
                type: 'object',
                properties: { thinkingReason: { type: 'string' } },
                required: ['thinkingReason']
              }
            }
          },
          required: ['head']
        }
      },
      required: ['contract']
    }
    expect(requiredJustifications(shape, 'probe', '').map((offender) => offender.path)).toEqual([
      'contract.head.modelReason',
      'contract.staff[].thinkingReason'
    ])
  })
})
