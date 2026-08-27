import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, test } from 'vitest'

// #827 step 7: scheduleIdleResume's company-maintenance-blocked branch used
// to re-arm itself every second forever while blocked -- a poll in disguise
// (a self-rescheduling setTimeout is functionally a setInterval wearing a
// disguise). It now waits on the session-maintenance/supervision doc-change
// the extension's sseWatcher already subscribes to, with exactly ONE bounded
// fallback attempt (ORGANIZATION_IDLE_RESUME_MAINTENANCE_FALLBACK_MS) so a
// missed event cannot wedge a pane's idle-resume forever. Allowlisted
// bounded-retry in scripts/reactive-allowlist.ts.
//
// Source-shape regression test (see IntercomFloorRemoved.test.ts for the
// rationale on why this is source-level, not a live-closure runtime test).

const PACKAGE_ROOT = fileURLToPath(new URL('../..', import.meta.url))
const SOURCE = readFileSync(join(PACKAGE_ROOT, 'extensions/organization-intercom.ts'), 'utf8')

function matchOrThrow(text: string, pattern: RegExp, label: string): RegExpMatchArray {
  const match = text.match(pattern)
  if (!match) throw new Error(`expected to find ${label}`)
  return match
}

describe('organization-intercom.ts: scheduleIdleResume is event-driven, not a self-rescheduling poll', () => {
  test('the company-maintenance-blocked branch no longer self-reschedules unconditionally', () => {
    const match = matchOrThrow(
      SOURCE,
      /if \(await companyMaintenanceBlocked\(\)\) \{([\s\S]*?)\n\s+return;\s*\n\s+\}/,
      'the companyMaintenanceBlocked branch'
    )
    const body = match[1] ?? ''
    // The old shape was a bare `scheduleIdleResume(epoch); return;` here --
    // an unconditional, unbounded self-reschedule. The new shape registers a
    // wait epoch and a bounded fallback timer instead.
    expect(body).toContain('idleResumeMaintenanceWaitEpoch = epoch')
    expect(body).toContain('idleResumeMaintenanceFallbackTimer')
    expect(body).toContain('ORGANIZATION_IDLE_RESUME_MAINTENANCE_FALLBACK_MS')
  })

  test('the fallback timer is a single bounded attempt, guarded so it cannot double-arm', () => {
    expect(SOURCE).toContain('if (idleResumeMaintenanceFallbackTimer === undefined) {')
  })

  test("the sseWatcher's onEvent resolves the wait on the next doc-change, cancelling the fallback", () => {
    // Anchored on the step-7 marker comment rather than the full
    // ORGANIZATION_SSE_MAINTENANCE_STORES condition text, which alone
    // exceeds the line-length limit as a regex literal.
    const eventHandlerMatch = matchOrThrow(
      SOURCE,
      /#827 step 7: an idle-resume waiting on company-maintenance([\s\S]*?)\n\s+\}\n\s+\},/,
      'the maintenance-store onEvent branch'
    )
    const body = eventHandlerMatch[1] ?? ''
    expect(body).toContain('idleResumeMaintenanceWaitEpoch !== undefined')
    expect(body).toContain('clearTimeout(idleResumeMaintenanceFallbackTimer)')
    expect(body).toContain('scheduleIdleResume(waitingEpoch)')
  })

  test('the wait state is cleared on every session-restart/shutdown boundary that already clears idleResumeTimer', () => {
    // The inline `if (var) clearTimeout(...)` shape is unique to the three
    // boundary-cleanup sites; the onEvent wake-resolution site (tested
    // above) uses a multi-line `if (var !== undefined) { clearTimeout(...) }`
    // shape instead, so this pattern does not double-count it.
    const clearSites = SOURCE.match(/if \(idleResumeMaintenanceFallbackTimer\) clearTimeout/g) ?? []
    // Three boundaries: session_start reset, agent_settled boundary, session_shutdown.
    expect(clearSites.length).toBe(3)
  })

  test('the bounded readiness-probe retry (ctx.isIdle) is untouched -- already capped by ORGANIZATION_IDLE_RESUME_READY_ATTEMPTS', () => {
    expect(SOURCE).toContain('++idleResumeReadyAttempts < ORGANIZATION_IDLE_RESUME_READY_ATTEMPTS')
  })

  // A live company (tribes-capital, 2026-08-13 21:11) launched four
  // departments at once. Every one of the four heads emitted
  // `work-resume-awaiting-idle attempts: 10` -- the ladder's EXHAUSTION
  // terminal, ten seconds of one-second timers -- and then settled anyway,
  // seconds later, from `agent_settled`. The budget was being spent waiting
  // for an answer an event already gives.
  test('a turn in flight hands off to agent_settled instead of burning the ten-attempt budget', () => {
    // Sliced between two stable markers rather than brace-matched: the branch
    // now contains a nested `return`, and a non-greedy brace pattern would
    // stop at it and silently assert against the wrong half.
    const probe = matchOrThrow(
      SOURCE,
      /if \(ctx\.isIdle\?\.\(\) !== true\) \{([\s\S]*?)const delivered = await drain\(epoch\);/,
      'the readiness probe branch'
    )
    const body = probe[1] ?? ''
    // The turn-in-flight hand-off must come BEFORE the counter, or the counter
    // still runs and the fix buys nothing.
    const handoff = body.indexOf('if (turnInFlight)')
    const counter = body.indexOf('++idleResumeReadyAttempts')
    expect(handoff).toBeGreaterThanOrEqual(0)
    expect(counter).toBeGreaterThanOrEqual(0)
    expect(handoff).toBeLessThan(counter)
    // And it says which of the two it did. `awaiting-turn` and
    // `awaiting-idle` are different facts and an operator reading the bus
    // must not have to guess which one happened.
    expect(body).toContain('"work-resume-awaiting-turn"')
  })

  test('agent_settled is what clears turnInFlight, so the hand-off has a receiver', () => {
    // If nothing cleared the flag, the hand-off would strand the resume: this
    // is the assertion that keeps the two halves attached to each other.
    expect(SOURCE).toContain('turnInFlight = false;')
    const settled = matchOrThrow(
      SOURCE,
      /pi\.on\("agent_settled"[\s\S]{0,600}?turnInFlight = false;/,
      'agent_settled clearing turnInFlight'
    )
    expect(settled[0]).toContain('turnInFlight = false;')
  })
})
