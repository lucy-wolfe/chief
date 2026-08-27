/**
 * Unit lock for the SECOND half of the model-switch defect: a session
 * maintenance receipt must be DERIVED FROM A DURABLE WRITE, never manufactured.
 *
 * The operator's complaint was a confident success message with nothing behind
 * it. `org_maintain_session` used to compose its receipt unconditionally from
 * the target list, so a queue that returned nothing usable still produced a
 * success card.
 *
 * `assertDurableMaintenanceRecords` is the gate that makes that impossible. It
 * runs BEFORE the event journal is appended and before any receipt text is
 * built, so a failure is legible and retryable rather than a lie.
 *
 * These cases enumerate the individual ways a record can fail to back its
 * receipt, which an e2e cannot manufacture.
 */
import { assertDurableMaintenanceRecords } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

function record(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 'session-maintenance:1:eng-head:compact',
    action: 'compact',
    personId: 'eng-head',
    requestedBy: 'ceo',
    reason: "operator compacts this person's context",
    automatic: false,
    requestedAt: '2026-07-24T17:53:00.000Z',
    status: 'queued',
    ...overrides
  }
}

function assertDurableRecords(
  requests: readonly unknown[],
  targets: readonly string[],
  action = 'compact'
): void {
  Reflect.apply(assertDurableMaintenanceRecords, undefined, [requests, targets, action])
}

describe('a session maintenance receipt is derived from the durable record, never manufactured', () => {
  test('a complete, matching, non-terminal record for every target passes', () => {
    expect(() => assertDurableRecords([record()], ['eng-head'])).not.toThrow()
    expect(() =>
      assertDurableRecords(
        [
          record(),
          record({ id: 'session-maintenance:2:eng-worker:compact', personId: 'eng-worker' })
        ],
        ['eng-head', 'eng-worker']
      )
    ).not.toThrow()
  })

  test('a queue that returned NOTHING for a target is refused — the exact shape of the manufactured receipt', () => {
    // Fan-out: 1 of 2 targets got a record. A count-blind receipt would have
    // reported both as recorded.
    expect(() => assertDurableRecords([record()], ['eng-head', 'eng-worker'])).toThrow(
      /not recorded for every target/
    )
    // Single target, nothing at all back.
    expect(() => assertDurableRecords([], [])).not.toThrow()
    expect(() => assertDurableRecords([undefined], ['eng-head'])).toThrow(
      /returned no durable record/
    )
    expect(() => assertDurableRecords([record({ id: '' })], ['eng-head'])).toThrow(
      /returned no durable record/
    )
    expect(() => assertDurableRecords([record({ id: '   ' })], ['eng-head'])).toThrow(
      /returned no durable record/
    )
  })

  test('a record minted for somebody else, or for another action, cannot stand in', () => {
    expect(() => assertDurableRecords([record({ personId: 'eng-worker' })], ['eng-head'])).toThrow(
      /is for 'eng-worker'/
    )
    expect(() => assertDurableRecords([record({ action: 'fresh_session' })], ['eng-head'])).toThrow(
      /'fresh_session'/
    )
    // Order matters too: records are matched to targets positionally, so a
    // transposed fan-out is red rather than silently accepted.
    expect(() =>
      assertDurableRecords(
        [record({ personId: 'eng-worker' }), record()],
        ['eng-head', 'eng-worker']
      )
    ).toThrow(/is for 'eng-worker'/)
  })

  test('an already-terminal record was not queued and must not be reported as queued', () => {
    for (const status of ['completed', 'failed', 'skipped']) {
      expect(() => assertDurableRecords([record({ status })], ['eng-head'])).toThrow(
        new RegExp(`already ${status}`)
      )
    }
    // The live in-flight states are legitimate: a claim may already have begun.
    for (const status of ['queued', 'running', 'applying']) {
      expect(() => assertDurableRecords([record({ status })], ['eng-head'])).not.toThrow()
    }
  })
})
