/**
 * THE COUNTER, DRIVEN — not the classifier, and not the counter's shape.
 *
 * `ProviderFailureClassification.test.ts` proves `providerFailureDiagnostic`
 * NAMES a context overflow `request_too_large`. That is half the fix and it is
 * the half that cannot catch the regression: the line that actually protects
 * the operator is one `if` inside `installOrganizationIntercom`'s `agent_end`
 * handler —
 *
 *     if (!permanentRequestFailure) consecutiveProviderFailures += 1;
 *
 * — and every assertion about the classifier stays green with that guard
 * deleted. A classifier that names a thing correctly while the caller ignores
 * the name is exactly the failure the operator saw: eight permanent 400s were
 * classified, counted, and escalated to a manager as "check that Pi's provider
 * access and model health" while the provider answered HTTP 200 to a replay of
 * the same call.
 *
 * This drives the handler, through `test/support/InstalledPane.ts`. Real
 * extension module, real install, real durable event trail
 * (`.chief/bus/events.jsonl`) read back off disk; FAKE `pi` (a recorder that
 * hands back the `agent_end` handler Pi's own loop would call) and one
 * loopback server standing in for this company's chiefd — the same split
 * `RosterRuntimeProcessProjection.test.ts` uses and for the same reason.
 *
 * # Both directions, because either one alone is the bug facing the other way
 *
 * A classifier that calls everything permanent silences the alert, and a
 * company that has genuinely stopped thinking then fails with nobody told. So
 * the transient direction is asserted here too, in the same file, against the
 * same handler: three connection errors must still climb 1, 2, 3 and must still
 * escalate on the third.
 */
import {
  CONTEXT_OVERFLOW,
  installedPane,
  stopInstalledPanes,
  TRANSIENT
} from '@test/support/InstalledPane'
import type { OrganizationEvent, Pane } from '@test/types/InstalledPane'
import { afterEach, describe, expect, test } from 'vitest'

afterEach(stopInstalledPanes)

function failedTurns(pane: Pane): readonly OrganizationEvent[] {
  return pane.events().filter((entry) => entry.event === 'provider-turn-failed')
}

function escalations(pane: Pane): readonly OrganizationEvent[] {
  return pane.events().filter((entry) => entry.event === 'provider-failure-escalated')
}

describe('the consecutive-provider-failure counter', () => {
  test('a request too large for the window is recorded, never counted, never escalated', async () => {
    const pane = await installedPane()

    // Three of them: one past the escalation limit, which is the whole point.
    // The bug produced eight in a row and escalated on the third.
    await pane.endTurn(CONTEXT_OVERFLOW)
    await pane.endTurn(CONTEXT_OVERFLOW)
    await pane.endTurn(CONTEXT_OVERFLOW)

    const failures = failedTurns(pane)
    // STILL RECORDED. The turn really failed and the trail must say so —
    // this fix suppresses the COUNT, not the diagnosis.
    expect(failures).toHaveLength(3)
    expect(failures.map((entry) => entry.kind)).toEqual([
      'request_too_large',
      'request_too_large',
      'request_too_large'
    ])
    // AND NEVER COUNTED. This is the assertion that goes red when
    // `if (!permanentRequestFailure)` is deleted: without it these read 1, 2, 3.
    expect(failures.map((entry) => entry.consecutiveFailures)).toEqual([0, 0, 0])
    // So the manager is never told the provider is unhealthy. It is not.
    expect(escalations(pane)).toHaveLength(0)
  })

  test('a genuine transient failure still counts, and still escalates on the third', async () => {
    const pane = await installedPane()

    await pane.endTurn(TRANSIENT)
    await pane.endTurn(TRANSIENT)
    await pane.endTurn(TRANSIENT)

    const failures = failedTurns(pane)
    expect(failures.map((entry) => entry.kind)).toEqual([
      'provider_error',
      'provider_error',
      'provider_error'
    ])
    // The counter is supposed to WORK. A fix that narrowed it into silence
    // would pass the test above and fail here, which is why both live together.
    expect(failures.map((entry) => entry.consecutiveFailures)).toEqual([1, 2, 3])
    expect(escalations(pane)).toHaveLength(1)
  })

  test('a permanent refusal does not mask the transient failures around it', async () => {
    const pane = await installedPane()

    // The operator's actual ten minutes: both models failing at once. The
    // overflow must be inert with respect to the count — it neither advances it
    // nor resets it — so the two real outages still reach the limit together.
    await pane.endTurn(TRANSIENT)
    await pane.endTurn(CONTEXT_OVERFLOW)
    await pane.endTurn(TRANSIENT)
    await pane.endTurn(CONTEXT_OVERFLOW)
    await pane.endTurn(TRANSIENT)

    expect(failedTurns(pane).map((entry) => entry.consecutiveFailures)).toEqual([1, 1, 2, 2, 3])
    expect(escalations(pane)).toHaveLength(1)
  })
})
