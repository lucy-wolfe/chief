import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, test } from 'vitest'

// #827 (E8-S5): organization-intercom.ts's SSE fallback floor
// (ORGANIZATION_SSE_FALLBACK_FLOOR_MS, sourced from the now-deleted
// org-sse-rollout.ts) is deleted outright. The sseWatcher is constructed
// unconditionally except for the test/fixture-only `backgroundActivityDisabled`
// seam (options.pollIntervalMs === 0, kept for conformance's deterministic
// single-call fixtures). A dead channel drives exactly one catch-up
// maintenance cycle via onChannelStateChange, never a recurring re-read.
//
// Source-shape regression test (same rationale/pattern as
// IntercomChiefingCalls.test.ts and FooterFloorRemoved.test.ts): a full
// runtime "idle pane makes zero chiefd reads" assertion needs
// installOrganizationIntercom's whole closure live (tmux/chiefd/Pi context),
// which this file's own installer test suites would need to exercise —
// tracked as an open item in the design record.
//
// Detection consequence, stated here rather than only in the ledger (same
// pattern #826 established for org-runtime.ts's convergence waits): with the
// fallback floor gone, detection is event-driven — a staleness that occurs
// between doc-change events is caught at the watcher's own heartbeat-timeout
// deadline (its `dead` transition), not continuously re-sampled. That
// deadline is the failure bound; it is not a re-arm of the deleted floor.

const PACKAGE_ROOT = fileURLToPath(new URL('../..', import.meta.url))
const SOURCE = readFileSync(join(PACKAGE_ROOT, 'extensions/organization-intercom.ts'), 'utf8')

function matchOrThrow(text: string, pattern: RegExp, label: string): RegExpMatchArray {
  const match = text.match(pattern)
  if (!match) throw new Error(`expected to find ${label}`)
  return match
}

describe('organization-intercom.ts: SSE fallback floor removed', () => {
  test('ORGANIZATION_SSE_FALLBACK_FLOOR_MS / org-sse-rollout import is gone', () => {
    expect(SOURCE).not.toContain('ORGANIZATION_SSE_FALLBACK_FLOOR_MS')
    expect(SOURCE).not.toMatch(/from\s+["']\.\/org-sse-rollout["']/)
    expect(SOURCE).not.toContain('sseEnabled(')
    expect(SOURCE).not.toContain('ORG_SSE_DISABLED')
  })

  test('sseHealthy tracking variable and the "if (sseHealthy) return" floor-skip are gone', () => {
    expect(SOURCE).not.toContain('sseHealthy')
  })

  test('there is no recurring setInterval floor left in installOrganizationIntercom', () => {
    // The turn watchdog's setInterval (armed/disarmed at turn_start/settle,
    // #368) is the one allowlisted setInterval remaining in this file — it is
    // NOT the SSE floor. Assert the floor-specific shape is gone rather than
    // asserting zero setInterval calls file-wide.
    expect(SOURCE).not.toMatch(/const timer = pollIntervalMs \? setInterval/)
    expect(SOURCE).not.toContain('if (sseHealthy) return;')
  })

  test('the sseWatcher is gated only by the fixture-only seam, not a product poll floor', () => {
    expect(SOURCE).toContain('const backgroundActivityDisabled = options.pollIntervalMs === 0;')
    expect(SOURCE).toContain(
      'const sseWatcher: SseWatcherLike | undefined = backgroundActivityDisabled'
    )
    expect(SOURCE).toMatch(/backgroundActivityDisabled\s*\n\s*\? undefined/)
  })

  test('onChannelStateChange("dead") still drives exactly one catch-up maintenance cycle', () => {
    const match = matchOrThrow(
      SOURCE,
      /onChannelStateChange: \(state: SseChannelState\) => \{([\s\S]*?)\n\s+\},/,
      'an onChannelStateChange handler'
    )
    const body = match[1] ?? ''
    expect(body).toContain('"dead"')
    expect(body).toContain('runMaintenanceCycle()')
    expect(body).not.toContain('sseHealthy')
  })

  test('onReorg is unchanged — reorg is a resync trigger, not folded into channel-state handling (#296)', () => {
    expect(SOURCE).toMatch(/onReorg: \(\) => \{\s*\n\s*void runMaintenanceCycle\(\);\s*\n\s*\},/)
  })
})
