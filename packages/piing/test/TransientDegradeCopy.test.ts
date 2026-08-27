/**
 * A RECOVERY THAT IS NOT RUNNING MUST NOT BE REPORTED AS RUNNING.
 *
 * `transientDegradeMessage` is produced at a tool's own catch block, and the
 * classifier's doc comment says precisely when: "even after retries are
 * exhausted". So at the instant the agent read `… is temporarily unavailable,
 * retrying.`, nothing was retrying — the backoff ladder had already run out and
 * the tool call was over.
 *
 * For a human that reads as a soft edge. For a machine it is an instruction:
 * a recovery reported as in flight is a recovery you wait for. The agent waits
 * on a retry that does not exist instead of re-issuing the call, which is the
 * only thing that can succeed. Every call site already attaches
 * `retryable: true` in the details — the prose contradicted the details.
 *
 * Two tools carry this string: `org_roster` and `org_send`.
 * (`org_maintain_session` was the third, deleted whole on 2026-08-24.)
 *
 * NOT RUN — per the no-build directive. Authored for the integrator.
 */
import { ChiefdUnavailableError } from '@chief/chiefing'
import { transientDegradeMessage } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

const TRANSIENT = new ChiefdUnavailableError({
  kind: 'unreachable',
  url: 'http://127.0.0.1:8792',
  path: '/v1/org/supervision/read'
})

describe('transientDegradeMessage', () => {
  test('never claims a retry is in flight', () => {
    const text = transientDegradeMessage('The roster', TRANSIENT)
    expect(text).toBeDefined()
    // THE NEGATIVE, and it is the whole point: the defect was not a missing
    // hedge, it was a present false claim about work in progress.
    expect(text).not.toMatch(/, retrying\.?$/)
    expect(text).not.toMatch(/\bretrying\b(?!\s+in the background)/)
  })

  test('names the state and the next action instead', () => {
    const text = transientDegradeMessage('The roster', TRANSIENT)
    expect(text).toContain('The roster is temporarily unavailable')
    expect(text).toContain('automatic retries are used up')
    expect(text).toContain('Nothing is retrying in the background')
    expect(text).toContain('re-issue this call')
  })

  test('keeps the capability name and leaks no internal address', () => {
    // The capability name is what makes the line legible without the raw
    // transport exception, which carries an internal URL and path.
    const text = transientDegradeMessage('Session maintenance', TRANSIENT)
    expect(text).toContain('Session maintenance is temporarily unavailable')
    expect(text).not.toContain('127.0.0.1')
    expect(text).not.toContain('/v1/org/supervision/read')
  })

  test('still answers undefined for a NON-transient failure', () => {
    // The classifier is the decision and is untouched: a corrupt ledger or a
    // refused write must keep surfacing loudly, exactly as before.
    expect(
      transientDegradeMessage('The roster', new Error('corrupt store: activity'))
    ).toBeUndefined()
    expect(transientDegradeMessage('The roster', new Error('unknown_task'))).toBeUndefined()
  })
})
