/**
 * The client half of the refusal taxonomy, asserted directly.
 *
 * `scripts/test/refusal-taxonomy.test.mjs` proves this set equals chiefd's
 * `REFUSAL_STATUSES` (`docstore/route_error.rs`) by reading both files. This
 * suite proves the set is actually WIRED — that `isRefusalStatus` answers it
 * and nothing else, and that the three clients which used to carry their own
 * narrower copies now read this one. A shared constant nobody consults is the
 * same defect wearing a better name.
 */
import { fixedResponseTransport } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { ChiefdUnavailableError, isTransientChiefdError, OrgRowRefusalError } from '@/Errors'
import { AggregatesClient } from '@/resources/Aggregates'
import { isRefusalStatus, REFUSAL_STATUSES } from '@/resources/OrgRoutes'
import { RemindersClient } from '@/resources/Reminders'
import { StaffingClient } from '@/resources/Staffing'

const SLUG = '0123456789ab'

describe('the shared refusal set', () => {
  it('is exactly the six statuses that carry an actionable {code, detail}', () => {
    expect([...REFUSAL_STATUSES].sort((a, b) => a - b)).toEqual([400, 401, 403, 404, 409, 422])
  })

  it('accepts every member and rejects every non-member that matters', () => {
    for (const status of REFUSAL_STATUSES) expect(isRefusalStatus(status)).toBe(true)
    // 429/500/502/503/504 are the ways chiefd says it could not answer. None
    // of them is a product rule the caller can act on.
    for (const status of [200, 429, 500, 502, 503, 504]) {
      expect(isRefusalStatus(status), `${status} must not read as a refusal`).toBe(false)
    }
  })
})

describe('every client reads the one set', () => {
  // Before #1004 each of these carried its own narrower copy: Staffing read
  // 422 alone on its direct verbs, Reminders read {400, 404}, and the shared
  // predicate was re-declared three times in one file.
  for (const status of [400, 401, 403, 404, 409, 422] as const) {
    it(`StaffingClient reads ${status} as a refusal VALUE, not an outage`, async () => {
      const transport = fixedResponseTransport(status, { code: 'refused-here', detail: 'no' })
      const staffing = new StaffingClient(transport)

      await expect(staffing.benchPerson(SLUG, 'p1')).resolves.toEqual({
        refused: 'refused-here',
        detail: 'no'
      })
    })

    it(`RemindersClient reads ${status} as a refusal, not an outage`, async () => {
      const transport = fixedResponseTransport(status, { code: 'refused-here', detail: 'no' })
      const reminders = new RemindersClient(transport)

      await expect(reminders.listReminders({ slug: SLUG, personId: 'p1' })).rejects.toMatchObject({
        name: 'ReminderRefusalError',
        code: 'refused-here',
        detail: 'no'
      })
    })
  }

  for (const status of [429, 500, 503] as const) {
    it(`StaffingClient keeps ${status} an outage`, async () => {
      const transport = fixedResponseTransport(status, { code: 'busy', detail: 'waited' })
      const staffing = new StaffingClient(transport)

      await expect(staffing.benchPerson(SLUG, 'p1')).rejects.toBeInstanceOf(ChiefdUnavailableError)
    })
  }
})

describe("a refusal reaches the agent with chiefd's own words", () => {
  it('carries the code AND the detail into the message every layer above reads', () => {
    const error = new OrgRowRefusalError({
      status: 422,
      code: 'head-needs-successor',
      detail: 'a department head cannot be offboarded without a successor'
    })

    expect(error.message).toBe(
      'org row refused: head-needs-successor: a department head cannot be ' +
        'offboarded without a successor'
    )
    // The string it must NOT be — the one an agent retries against forever.
    expect(error.message).not.toMatch(/unavailable/i)
  })
})

/**
 * The DELIBERATELY OPEN edge of the taxonomy, pinned so it cannot drift shut
 * by accident.
 *
 * `ChiefdError::Absent` — "this store has never been written" — has no wire
 * discriminant of its own. `chiefd-api/src/wire/error.rs` projects it onto
 * `WireError::Unavailable { reason: <store name> }`, which `http_status()`
 * maps to 503. So a fresh company whose native ledgers were never seeded —
 * exactly what `createOrganization` leaves behind, and a HEALTHY state — is
 * answered in the same bytes as a daemon that is quiescing.
 *
 * The decision was taken to leave it that way for now: giving `Absent` its own
 * variant reopens the five-variant closed taxonomy and the conformance
 * corpus's `ErrorType` union, and both halves are Rust. These assertions state
 * the COST rather than describing it, and they are what fails the day someone
 * lands the variant — which is the point. That day, this block is deleted and
 * the client grows an `absent` branch; it must not happen quietly.
 */
describe('an absent store is indistinguishable from an unavailable daemon (open, and stated)', () => {
  const neverWritten = { code: 'unavailable', detail: 'supervision' }
  const quiescing = { code: 'unavailable', detail: 'removing' }

  for (const [what, body] of [
    ['a store that was NEVER WRITTEN', neverWritten],
    ['a daemon that is QUIESCING', quiescing]
  ] as const) {
    it(`${what} reaches the client as the same 503 outage`, async () => {
      const transport = fixedResponseTransport(503, body)
      const client = new AggregatesClient(transport)

      const thrown = await client
        .supervisionRead(SLUG)
        .then(() => undefined)
        .catch((error: unknown) => error)

      expect(thrown).toBeInstanceOf(ChiefdUnavailableError)
      expect(thrown).toMatchObject({ kind: 'http-error', status: 503, detail: body.detail })
      // Not a refusal, so there is no `code` to branch on: 503 is outside
      // REFUSAL_STATUSES by design, and the store name arrives only as prose.
      expect(isRefusalStatus(503)).toBe(false)
      // And not transient either, so no ladder resolves it. The caller is told
      // "chiefd could not answer" for a fact it could have acted on by
      // seeding the store.
      expect(isTransientChiefdError(thrown)).toBe(false)
    })
  }

  it('the two differ only in a prose field, which is why a client cannot decide whether to create', async () => {
    const absent = new ChiefdUnavailableError({
      kind: 'http-error',
      url: 'http://localhost:1',
      path: '/v1/org/supervision/read',
      status: 503,
      detail: neverWritten.detail
    })
    const unavailable = new ChiefdUnavailableError({
      kind: 'http-error',
      url: 'http://localhost:1',
      path: '/v1/org/supervision/read',
      status: 503,
      detail: quiescing.detail
    })

    expect(absent.kind).toBe(unavailable.kind)
    expect(absent.status).toBe(unavailable.status)
    // Everything that is machine-readable agrees. Only the human half differs,
    // and separating them on it means matching store names against a message.
    expect(absent.detail).not.toBe(unavailable.detail)
  })
})
