/**
 * #529 / #404: the footer must NEVER present stale-mirror supervision data as
 * fresh. When the served supervision doc is DETECTABLY STALE (its `updatedAt`
 * lags the protected-schedule cadence — the frozen/off org_documents-mirror
 * shape that showed a live "🎯 due · 👤 due" + goal count for 3 days off a dead
 * doc), the footer must mark it not-fresh (dimmed + "⚠ stale"), never healthy.
 *
 * This locks the DETECTION contract deterministically via the pure, exported
 * `supervisionDocIsStale`. A faithful REAL-PANE e2e is blocked on the harness's
 * inability to hold a time-stale mirror (a live chiefd's duty loop rewrites
 * `updatedAt` to now within seconds, and the footer only reads the doc when
 * chiefd owns it — see #411's two-store harness gap), so the freshness rule is
 * locked at the unit boundary where it can be exercised without that.
 */
import {
  footerRepaintFloorMs,
  supervisionDocIsStale,
  supervisionSnapshotIsStale,
  supervisionStaleAfterMs
} from '@test-assets/team-ui'
import { describe, expect, test } from 'vitest'

const MINUTE = 60_000
const PRODUCTION_STALE_MS = 30 * MINUTE
const NOW = Date.parse('2026-07-25T12:00:00.000Z')
const iso = (msAgo: number): string => new Date(NOW - msAgo).toISOString()

describe("#529: supervisionDocIsStale — the footer's freshness contract", () => {
  test('a freshly-written doc (updatedAt = now) is NOT stale', () => {
    expect(supervisionDocIsStale({ updatedAt: iso(0) }, NOW, PRODUCTION_STALE_MS)).toBe(false)
  })

  test('a doc updated within the bound (e.g. 15m ago — one schedule cadence) is NOT stale', () => {
    expect(supervisionDocIsStale({ updatedAt: iso(15 * MINUTE) }, NOW, PRODUCTION_STALE_MS)).toBe(
      false
    )
  })

  test('a doc exactly at the 30m bound is NOT stale (strictly greater required)', () => {
    expect(
      supervisionDocIsStale({ updatedAt: iso(PRODUCTION_STALE_MS) }, NOW, PRODUCTION_STALE_MS)
    ).toBe(false)
  })

  test('a doc just past the 30m bound IS stale', () => {
    expect(
      supervisionDocIsStale({ updatedAt: iso(PRODUCTION_STALE_MS + 1) }, NOW, PRODUCTION_STALE_MS)
    ).toBe(true)
  })

  test('the frozen-mirror shape (updatedAt 3 days ago) IS stale', () => {
    expect(
      supervisionDocIsStale({ updatedAt: iso(3 * 24 * 60 * MINUTE) }, NOW, PRODUCTION_STALE_MS)
    ).toBe(true)
  })

  test("an UNREADABLE doc (undefined) is NOT stale — that is the separate 'unknown' treatment, not a false stale", () => {
    expect(supervisionDocIsStale(undefined, NOW, PRODUCTION_STALE_MS)).toBe(false)
  })

  test('a doc with a missing or malformed updatedAt is NOT stale (never a false positive off a bad field)', () => {
    expect(supervisionDocIsStale({}, NOW, PRODUCTION_STALE_MS)).toBe(false)
    expect(supervisionDocIsStale({ updatedAt: null }, NOW, PRODUCTION_STALE_MS)).toBe(false)
    expect(supervisionDocIsStale({ updatedAt: 'not-a-date' }, NOW, PRODUCTION_STALE_MS)).toBe(false)
    expect(supervisionDocIsStale({ updatedAt: 123456 }, NOW, PRODUCTION_STALE_MS)).toBe(false)
  })

  test('a cached initially-fresh snapshot becomes stale on the render clock without an SSE refresh', () => {
    const updatedAt = iso(0)
    expect(
      supervisionSnapshotIsStale(false, updatedAt, NOW + PRODUCTION_STALE_MS, PRODUCTION_STALE_MS)
    ).toBe(false)
    expect(
      supervisionSnapshotIsStale(
        false,
        updatedAt,
        NOW + PRODUCTION_STALE_MS + 1,
        PRODUCTION_STALE_MS
      )
    ).toBe(true)
  })

  test('the one-minute E2E horizon and fast repaint require the explicit test sentinel', () => {
    const e2e = {
      TEAM_LAUNCHER_E2E: '1',
      TEAM_LAUNCHER_E2E_SUPERVISION_STALE_MS: String(MINUTE),
      TEAM_LAUNCHER_E2E_FOOTER_REPAINT_MS: '1000'
    }
    expect(supervisionStaleAfterMs({})).toBe(30 * MINUTE)
    expect(footerRepaintFloorMs({})).toBe(MINUTE)
    expect(
      supervisionStaleAfterMs({ TEAM_LAUNCHER_E2E_SUPERVISION_STALE_MS: String(MINUTE) })
    ).toBe(30 * MINUTE)
    expect(footerRepaintFloorMs({ TEAM_LAUNCHER_E2E_FOOTER_REPAINT_MS: '1000' })).toBe(MINUTE)
    expect(supervisionStaleAfterMs(e2e)).toBe(MINUTE)
    expect(footerRepaintFloorMs(e2e)).toBe(1000)
    expect(
      supervisionDocIsStale({ updatedAt: iso(MINUTE + 1) }, NOW, supervisionStaleAfterMs(e2e))
    ).toBe(true)
    expect(
      supervisionSnapshotIsStale(false, iso(0), NOW + MINUTE + 1, supervisionStaleAfterMs(e2e))
    ).toBe(true)
  })
})
