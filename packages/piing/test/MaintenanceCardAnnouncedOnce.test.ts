/**
 * ONE ANNOUNCEMENT PER DURABLE RECORD.
 *
 * The operator photographed a pane carrying `Context compacted ·
 * @eng-engineer-2` twice in a row under a single `[compaction]` block. One
 * compaction, two cards.
 *
 * The cause is not the rendering. `finish` is idempotent at the store — a
 * finish on an already-terminal request REPLAYS it and returns the same record
 * with a 200 — and the extension finishes a compaction from more than one place
 * ON PURPOSE: the native completion callback, and the recovery branch that
 * finds a proven compaction whose callback never arrived. Measured on
 * a live box 2026-08-20: request `session-maintenance:e800bbc6…`,
 * `POST /v1/org/session-maintenance/finish` at 17:16:24 and again at 17:16:25.
 *
 * So the card cannot be drawn from the return value alone. It is keyed on the
 * request id and phase, which is what makes a replay silent while leaving every
 * genuine phase change audible.
 */
import { shouldAnnounceMaintenanceCard } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

const request = { id: 'session-maintenance:e800bbc69c57823ce4ba8ef61a83a259385104f6' }

describe('a maintenance card is announced once per record', () => {
  test('the second finish of one compaction says nothing', () => {
    const announced = new Set<string>()
    expect(shouldAnnounceMaintenanceCard(announced, request, 'completed')).toBe(true)
    expect(shouldAnnounceMaintenanceCard(announced, request, 'completed')).toBe(false)
  })

  test('each card-bearing phase of one request is still audible', () => {
    // Only terminal phases draw a card at all (`MAINTENANCE_CARD_PHASES`), and
    // the dedupe must not collapse them into one another: a request that
    // completes and a request that fails are different statements about the
    // same record.
    const announced = new Set<string>()
    expect(shouldAnnounceMaintenanceCard(announced, request, 'completed')).toBe(true)
    expect(shouldAnnounceMaintenanceCard(announced, request, 'failed')).toBe(true)
    expect(shouldAnnounceMaintenanceCard(announced, request, 'skipped')).toBe(true)
  })

  test('a phase that never draws a card stays silent', () => {
    const announced = new Set<string>()
    expect(shouldAnnounceMaintenanceCard(announced, request, 'running')).toBe(false)
    expect(shouldAnnounceMaintenanceCard(announced, request, 'queued')).toBe(false)
    expect(announced.size).toBe(0)
  })

  test('a different request is a different announcement', () => {
    const announced = new Set<string>()
    expect(shouldAnnounceMaintenanceCard(announced, request, 'completed')).toBe(true)
    expect(
      shouldAnnounceMaintenanceCard(announced, { id: 'session-maintenance:other' }, 'completed')
    ).toBe(true)
  })
})
