// E2-S5 — RemindersClient: 3 routes; a refusal status throws
// ReminderRefusalError carrying chiefd's own {code, detail}; the
// MIN_REMINDER_INTERVAL_MS constant is frozen at 60_000.

import { jsonResponse, RecordingTransport, textResponse } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { ChiefdUnavailableError, ReminderRefusalError } from '@/Errors'
import { MIN_REMINDER_INTERVAL_MS, RemindersClient } from '@/resources/Reminders'
import type { Reminder } from '@/types/Reminders'

/** The company key a caller already holds — `sha256(dir)[..12]`, read off the
 * beacond row or the daemon rendezvous. It travels as `slug` on the wire and
 * nothing here rewrites it. */
const SLUG = '0123456789ab'

const REMINDER: Reminder = {
  id: 'r1',
  personId: 'p1',
  createdByPersonId: 'p1',
  prompt: 'stand up',
  intervalMs: 3_600_000,
  nextDueAt: '2024-01-01T01:00:00Z',
  status: 'active',
  recurring: true,
  createdAt: '2024-01-01T00:00:00Z'
}

describe('MIN_REMINDER_INTERVAL_MS', () => {
  it('is frozen at 60_000', () => {
    expect(MIN_REMINDER_INTERVAL_MS).toBe(60_000)
  })
})

describe('RemindersClient — armReminder', () => {
  it('posts /v1/reminders/arm with the company key it was given', async () => {
    const transport = new RecordingTransport(() => jsonResponse(200, { reminder: REMINDER }))
    const reminders = new RemindersClient(transport)
    const result = await reminders.armReminder({
      slug: SLUG,
      personId: 'p1',
      prompt: 'stand up',
      intervalMs: 3_600_000
    })
    expect(transport.calls[0]?.path).toBe('/v1/reminders/arm')
    expect(transport.calls[0]?.body).toMatchObject({ slug: SLUG })
    expect(result).toEqual({ reminder: REMINDER })
  })
})

describe('RemindersClient — listReminders', () => {
  it('posts /v1/reminders/list', async () => {
    const transport = new RecordingTransport(() => jsonResponse(200, { reminders: [REMINDER] }))
    const reminders = new RemindersClient(transport)
    const result = await reminders.listReminders({ slug: SLUG, personId: 'p1' })
    expect(transport.calls[0]?.path).toBe('/v1/reminders/list')
    expect(result).toEqual({ reminders: [REMINDER] })
  })
})

describe('RemindersClient — stopReminder', () => {
  it('posts /v1/reminders/stop', async () => {
    const stopped = { ...REMINDER, status: 'stopped' as const }
    const transport = new RecordingTransport(() => jsonResponse(200, { reminder: stopped }))
    const reminders = new RemindersClient(transport)
    const result = await reminders.stopReminder({ slug: SLUG, personId: 'p1', reminderId: 'r1' })
    expect(transport.calls[0]?.path).toBe('/v1/reminders/stop')
    expect(result).toEqual({ reminder: stopped })
  })
})

describe('RemindersClient — the company key reaches every route unchanged', () => {
  // Ported from tests/org-reminder-store.test.ts's own #564 regression:
  // armReminder's test above already proves the arm route carries the key, but
  // list/stop never had a dedicated assertion of their own — a real incident
  // showed this was worth checking per-route rather than assumed from one call
  // site. What each route must carry changed (the composite `slug@hash` became
  // the served directory key); that it must reach all three did not.
  it('list and stop both send the key verbatim, adding nothing and rewriting nothing', async () => {
    const transport = new RecordingTransport(() => jsonResponse(200, { reminders: [REMINDER] }))
    const reminders = new RemindersClient(transport)
    await reminders.listReminders({ slug: SLUG, personId: 'p1' })
    expect(transport.calls[0]?.body).toMatchObject({ slug: SLUG })

    const stopTransport = new RecordingTransport(() =>
      jsonResponse(200, { reminder: { ...REMINDER, status: 'stopped' } })
    )
    const stopReminders = new RemindersClient(stopTransport)
    await stopReminders.stopReminder({ slug: SLUG, personId: 'p1', reminderId: 'r1' })
    expect(stopTransport.calls[0]?.body).toMatchObject({ slug: SLUG })
  })
})

describe('RemindersClient — refusal and infra failure shapes', () => {
  it("a refusal carries chiefd's own code and detail, not the raw body text", async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(422, {
        code: 'interval-below-floor',
        detail: 'interval below the one-minute floor'
      })
    )
    const reminders = new RemindersClient(transport)
    let error: unknown
    try {
      await reminders.armReminder({
        slug: SLUG,
        personId: 'p1',
        prompt: 'x',
        intervalMs: 1_000
      })
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ReminderRefusalError)
    if (!(error instanceof ReminderRefusalError)) throw new Error('expected ReminderRefusalError')
    expect(error.status).toBe(422)
    expect(error.code).toBe('interval-below-floor')
    expect(error.detail).toBe('interval below the one-minute floor')
    // The agent-visible string keeps BOTH halves: a message built from the code
    // alone throws away the sentence that says what to do about it.
    expect(error.message).toBe(
      'reminder refused: interval-below-floor: interval below the one-minute floor'
    )
  })

  it('404 (wrong daemon) also throws ReminderRefusalError', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(404, { code: 'unknown-company', detail: 'not the reminder authority' })
    )
    const reminders = new RemindersClient(transport)
    await expect(reminders.listReminders({ slug: SLUG, personId: 'p1' })).rejects.toBeInstanceOf(
      ReminderRefusalError
    )
  })

  // #1004: 409 used to be outside the client's refusal set entirely, so a
  // reminder route answering one reached the caller as
  // `chiefd unavailable (http-error)`. It is a refusal in the shared taxonomy.
  it('409 (a lost fence) is a refusal, not an outage', async () => {
    const transport = new RecordingTransport(() =>
      jsonResponse(409, { code: 'seq-conflict', detail: 'expected 4, actual 5' })
    )
    const reminders = new RemindersClient(transport)
    await expect(reminders.listReminders({ slug: SLUG, personId: 'p1' })).rejects.toBeInstanceOf(
      ReminderRefusalError
    )
  })

  it('a 503 stays a genuine outage: ChiefdUnavailableError', async () => {
    const transport = new RecordingTransport(() => textResponse(503, 'starting'))
    const reminders = new RemindersClient(transport)
    await expect(reminders.listReminders({ slug: SLUG, personId: 'p1' })).rejects.toBeInstanceOf(
      ChiefdUnavailableError
    )
  })

  it('a 500 is a genuine infra failure: ChiefdUnavailableError', async () => {
    const transport = new RecordingTransport(() => textResponse(500, 'chiefd is down'))
    const reminders = new RemindersClient(transport)
    await expect(reminders.listReminders({ slug: SLUG, personId: 'p1' })).rejects.toBeInstanceOf(
      ChiefdUnavailableError
    )
  })

  it('a malformed 2xx body throws ChiefdUnavailableError kind malformed-body', async () => {
    const transport = new RecordingTransport(() => ({ status: 200, body: 'not json' }))
    const reminders = new RemindersClient(transport)
    let error: unknown
    try {
      await reminders.listReminders({ slug: SLUG, personId: 'p1' })
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ChiefdUnavailableError)
    if (!(error instanceof ChiefdUnavailableError))
      throw new Error('expected ChiefdUnavailableError')
    expect(error.kind).toBe('malformed-body')
  })
})
