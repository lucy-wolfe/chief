import type { ChiefdRuntimeLaunchReport } from '@test-assets/organization-intercom'
import { runtimeConvergenceWarning } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

// #751/P4. `reconcileRuntime` spawned `org reconcile` at a CLI that no longer
// exists, so every tool that mutates structure and then converges the runtime —
// department create, unit resume/stop/remove, staffing, mailbox wakes — failed
// AFTER its durable write committed. It now posts `/v1/org/runtime/launch`, and
// this is the one decision left on the TypeScript side of that call: what, if
// anything, to tell the agent about a pass that did not converge.
//
// The rule is truthful-or-absent, and both directions matter. The OLD helper
// returned a warning for exactly one reason ("this pane has no tmux identity"),
// which was never about the company's runtime at all; returning nothing when
// chiefd says it converged nothing would be the same class of untruth pointed
// the other way.
describe('runtimeConvergenceWarning (#751/P4)', () => {
  const applied: ChiefdRuntimeLaunchReport = {
    applied: true,
    retryAfterFloor: false,
    monitorWarnings: []
  }

  test('a pass that converged with nothing held back says nothing', () => {
    expect(runtimeConvergenceWarning(applied)).toBeUndefined()
  })

  test('a pass that converged nothing YET is not a caveat on the card', () => {
    // Operator ruling: this sentence appeared on every department a manager
    // created, and it explained chiefd's single-flight window to somebody who
    // did not ask and could not act on it. The durable write committed and the
    // runtime comes up; whether a given pass was skipped or coalesced is
    // chiefd's own business, not the caller's.
    expect(
      runtimeConvergenceWarning({ ...applied, applied: false, retryAfterFloor: true })
    ).toBeUndefined()
    expect(runtimeConvergenceWarning({ ...applied, applied: false })).toBeUndefined()
    expect(runtimeConvergenceWarning({})).toBeUndefined()
  })

  test('no report of any shape names chiefd`s reconciler to the caller', () => {
    const reports: ChiefdRuntimeLaunchReport[] = [
      applied,
      {},
      { applied: false },
      { applied: false, retryAfterFloor: true }
    ]
    for (const report of reports) {
      expect(runtimeConvergenceWarning(report) ?? '').not.toContain('reconciler')
    }
  })

  // TOMBSTONE: two tests that pinned the launch-ramp sentence -- that a capped
  // ramp named how many starts were held and outranked the generic line, and
  // that a non-integer count was ignored rather than interpolated. Both are
  // deleted with the ramp itself: chiefd publishes a desired SET and the
  // actuator boots everything missing at once, so no start is ever deferred and
  // the sentence could only ever describe a mechanism that no longer exists.
  // The doc on `runtimeConvergenceWarning` calls that "the same defect as a
  // wrong number", which is exactly right and is why these go rather than being
  // adjusted.

  test("chiefd's own monitor warnings are surfaced verbatim on an otherwise clean pass", () => {
    expect(
      runtimeConvergenceWarning({
        ...applied,
        monitorWarnings: ['pane %3 drifted.', 'model stale.']
      })
    ).toBe('pane %3 drifted. model stale.')
  })

  test('blank monitor warnings never become an empty caveat', () => {
    expect(runtimeConvergenceWarning({ ...applied, monitorWarnings: ['', '   '] })).toBeUndefined()
  })

  test('the deleted tmux-identity warning is not reachable by any report', () => {
    // The only string the old helper could produce, and it described the CALLER
    // rather than the company. chiefd derives the session itself, so no report
    // may resurrect it.
    const reports: ChiefdRuntimeLaunchReport[] = [applied, {}, { applied: false }]
    for (const report of reports) {
      expect(runtimeConvergenceWarning(report) ?? '').not.toContain(
        'Runtime projection is not configured'
      )
    }
  })
})
