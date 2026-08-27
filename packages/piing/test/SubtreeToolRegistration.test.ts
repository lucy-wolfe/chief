/**
 * GATE 1: THE PANE THAT NEVER OFFERED A WORKER THE VERB.
 *
 * A live CEO told its operator that its Chief of Staff "is Chief of Staff
 * (general staff) and doesn't hold the org-management tools needed to create a
 * department or hire a department head — those are CEO/head-level functions".
 * Three layers said otherwise: `staffingAuthority` has no role gate, the roster
 * names the accepted path, and chiefd's create path allows a leaf to create
 * beneath itself. The fourth layer agreed with the CEO — and it is the only one
 * the model can feel. `installOrganizationIntercom` registered the WHOLE
 * structural family inside one `if (manager(person))`, so a worker's pane
 * carried none of it.
 *
 * `ORGANIZATION_SUBTREE_TOOL_NAMES` has documented the opposite since the
 * catalog was split: "the subtree-growth surface — every person carries it,
 * whatever their kind", added because "a worker's pane was launched without
 * `org_add_department` at all and the mandate was unreachable in practice".
 * The catalog was split; the registration gate was not split with it. Gate 2
 * (`planPerson`) and gate 3 (chiefd's actuator) both granted these verbs while
 * gate 1 withheld them.
 *
 * This suite is the missing half of `ManagerToolGate3Parity.test.ts`. That one
 * reconciles gate 2 against gate 3; this one reconciles the CATALOG against
 * gate 1, for a person of each kind.
 */
import { isNullish } from '@test/support/Nullish'
import { captureRegisteredTools } from '@test/support/ToolRegistrationHarness'
import type { ToolRegistrationCapture } from '@test/types/ToolRegistrationHarness'
import {
  ORGANIZATION_MANAGER_TOOL_NAMES,
  ORGANIZATION_SUBTREE_TOOL_NAMES
} from '@test-assets/organization-intercom'
import { afterEach, describe, expect, test } from 'vitest'

/** The person from the incident: general staff, homed in the executive root,
 *  heading nothing. */
const CARLA = 'carla'

const CARLA_RECORD = {
  id: CARLA,
  name: 'Carla',
  title: 'Chief of Staff',
  kind: 'worker',
  departmentId: 'executive',
  employmentState: 'active',
  createdAt: '2026-01-01T00:00:00.000Z'
}

let capture: ToolRegistrationCapture | undefined

afterEach(async () => {
  await capture?.stop()
  capture = undefined
})

async function registeredNamesFor(personId: string): Promise<Set<string>> {
  capture = await captureRegisteredTools({ personId, people: { [CARLA]: CARLA_RECORD } })
  return new Set(capture.tools.map((tool) => tool.name))
}

describe('gate 1 registers the subtree family for every person', () => {
  test('a WORKER carries every verb the catalog says every person carries', async () => {
    const registered = await registeredNamesFor(CARLA)
    const missing = ORGANIZATION_SUBTREE_TOOL_NAMES.filter((tool) => !registered.has(tool))
    // If this fails, a worker's pane is short a verb its own catalog grants —
    // the exact shape of the incident, whatever the authority layer says.
    expect(missing).toEqual([])
  })

  test('org_add_department in particular, because that is the one the CEO denied', async () => {
    const registered = await registeredNamesFor(CARLA)
    expect(registered.has('org_add_department')).toBe(true)
    expect(registered.has('org_hire')).toBe(true)
  })

  test('nothing is withheld from a worker by what it IS', () => {
    // This used to assert the opposite half — that a worker got none of the
    // manager-only tools. That list is EMPTY now, so the old assertion would
    // pass vacuously and prove nothing. Re-expressed against the rule that
    // replaced it, and pinned on the catalog rather than on a registration, so
    // it fails the moment a name comes back rather than after somebody wires
    // one up: no tool in this product is granted or withheld by a person's
    // kind. The one kind-SHAPED grant left is `org_escalate_to_operator`, and
    // it asks whether the person has a manager to escalate TO — a fact about
    // the tree, checked by the test below.
    expect(ORGANIZATION_MANAGER_TOOL_NAMES, 'the role-gated catalog must stay empty').toEqual([])
  })

  test('a worker is not the structural root, so it gets no operator escalation', async () => {
    const registered = await registeredNamesFor(CARLA)
    // Every non-root person escalates to its own manager. Registering this for
    // them re-opens the "there is no valid recipient" trap #270 closed.
    expect(registered.has('org_escalate_to_operator')).toBe(false)
  })

  test('the CEO loses nothing: subtree, manager-only and escalation all stay', async () => {
    const registered = await registeredNamesFor('ceo')
    const missingSubtree = ORGANIZATION_SUBTREE_TOOL_NAMES.filter((tool) => !registered.has(tool))
    const missingManager = ORGANIZATION_MANAGER_TOOL_NAMES.filter((tool) => !registered.has(tool))
    expect(missingSubtree).toEqual([])
    expect(missingManager).toEqual([])
    expect(registered.has('org_escalate_to_operator')).toBe(true)
  })

  test('the install is non-vacuous: it really read this company from chiefd', async () => {
    await registeredNamesFor(CARLA)
    // Without this, a harness that failed to reach chiefd at all could register
    // a defensive surface and every assertion above would still pass.
    expect(isNullish(capture)).toBe(false)
    expect(capture?.chiefdPaths).toContain('/v1/org/manifest/read')
  })
})
