import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, test } from 'vitest'

// #827 (E8-S5): team-ui.ts's SSE poll floor is deleted. The org-pane render
// clock survives with its I/O branch removed (zero-I/O render-clock class in
// scripts/reactive-allowlist.ts); the plain-pane's unconditional 60s
// `refreshAndRender()` floor is deleted outright. The `fs.watch` on
// `.pi/loops` (#361) that then carried the plain pane went with the Pi
// `/loop` surface itself: a plain pane's footer reads no durable state at
// all now, so it has nothing left to re-read and needs no channel of its own.
//
// Source-shape regression test, same rationale as
// IntercomFloorRemoved.test.ts: `installFooter`'s org path drives real
// chiefd reads via the async chiefing client with no injectable read seam,
// so a full runtime "idle pane makes zero chiefd reads over 5 minutes"
// assertion needs a chiefd-backed harness this package does not yet have
// (tracked as an open item in the design record). What IS
// locked down here is real: the removed symbols staying gone, and the two
// floor shapes (org I/O branch, plain-pane timer) not silently reappearing.
//
// Detection consequence, stated here rather than only in the ledger (same
// pattern #826 established for org-runtime.ts's convergence waits): with
// both floors gone, detection is event-driven — a staleness that occurs
// between doc-change events (org pane) is caught at the next real event, not
// on a fixed cadence. This is the intended tradeoff, not a regression.

const PACKAGE_ROOT = fileURLToPath(new URL('../..', import.meta.url))
const SOURCE_PATH = join(PACKAGE_ROOT, 'extensions/team-ui.ts')

function source(): string {
  return readFileSync(SOURCE_PATH, 'utf8')
}

function matchOrThrow(text: string, pattern: RegExp, label: string): RegExpMatchArray {
  const match = text.match(pattern)
  if (!match) throw new Error(`expected to find ${label}`)
  return match
}

describe('team-ui.ts: FooterRenderClock (org pane)', () => {
  test('ORG_SSE_POLL_FLOOR_MS / org-sse-rollout import is gone', () => {
    const text = source()
    // Import/usage, not bare mention: FOOTER_STALE_AFTER_MS's own doc
    // comment legitimately names the deleted constant in backticks as
    // history (the design record records this), so these
    // checks target the import statement and declaration shape rather than
    // any string occurrence.
    expect(text).not.toMatch(/from\s+["']\.\/org-sse-rollout["']/)
    expect(text).not.toMatch(/\{[^}]*ORG_SSE_POLL_FLOOR_MS[^}]*\}\s*from/)
    expect(text).not.toContain('ORG_SSE_POLL_FLOOR_MS =')
  })

  test('the org-pane render-clock timer body is zero I/O: requestRender only, no sseHealthy branch', () => {
    const text = source()
    const match = matchOrThrow(
      text,
      /floor = createFloorTimer\(\(\) => \{([\s\S]*?)\n\s+\}, footerRepaintFloorMs\(\)\);/,
      'the org-pane floor timer callback'
    )
    const body = match[1] ?? ''
    expect(body).toContain('tui.requestRender()')
    expect(body).not.toContain('sseHealthy')
    expect(body).not.toContain('refreshAndRender()')
    expect(body).not.toContain('refreshSchedulesIfChanged()')
  })

  test('sseHealthy tracking variable is gone from the org-pane branch', () => {
    const text = source()
    expect(text.includes('sseHealthy')).toBe(false)
  })

  test('onChannelStateChange("dead") still drives exactly one catch-up cycle (not deleted alongside the floor)', () => {
    const text = source()
    const match = matchOrThrow(
      text,
      /onChannelStateChange: \(state: SseChannelState\) => \{([\s\S]*?)\n\s+\},/,
      'an onChannelStateChange handler'
    )
    const body = match[1] ?? ''
    expect(body).toContain('"dead"')
    expect(body).toContain('refreshAndRender()')
  })

  test('onReorg is unchanged — reorg is a resync trigger, not folded into channel-state handling (#296)', () => {
    const text = source()
    // `void refreshSchedulesIfChanged();` sat beside the render here. It
    // re-read the goal-schedule cards, and it went with the goal feature, so
    // a resync is now exactly one re-render.
    expect(text).toMatch(/onReorg: \(\) => \{ refreshAndRender\(\); \},/)
  })
})

describe('team-ui.ts: FooterPlainPaneReactive (non-org pane)', () => {
  test('the unconditional plain-pane floor timer (else branch) is gone', () => {
    const text = source()
    // The old shape had an `else { floor = createFloorTimer(...) }` arm
    // using the org-sse-rollout constant directly. Exactly one
    // `createFloorTimer(` call site should remain (the org-pane render
    // clock) — a reintroduced plain-pane branch would add a second.
    const floorTimerCalls = text.match(/createFloorTimer\(/g) ?? []
    expect(floorTimerCalls.length).toBe(1)
    expect(text).not.toMatch(/\}\s*else\s*\{\s*floor = createFloorTimer/)
  })

  test('the `.pi/loops` fs.watch went with the deleted /loop surface and is not re-added', () => {
    const text = source()
    // The watcher only ever existed to notice a persisted-loop FILE change.
    // With no loops file there is nothing to watch, and nothing on the plain
    // pane's footer is read from disk any more — so neither the watcher nor a
    // timer standing in for it may come back.
    expect(text).not.toContain('watchPersistedLoopDirectory')
    expect(text).not.toContain('loopWatcher')
    expect(text).not.toContain('createLoopWatcher')
    // No `fs.watch` at all: the file no longer imports node:fs.
    expect(text).not.toMatch(/from\s+["']node:fs["']/)
    expect(text).not.toContain('watchFs')
  })
})
