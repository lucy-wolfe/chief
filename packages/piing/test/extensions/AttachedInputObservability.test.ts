import {
  AttachedInputTracer,
  persistedUserEntryCount
} from '@test-assets/attached-input-observability'
import { describe, expect, test } from 'vitest'

describe('attached input observability (#645)', () => {
  test('records opaque Pi-input through transcript persistence and turn start without text', () => {
    const events: unknown[] = []
    const tracer = new AttachedInputTracer(
      (event) => events.push(event),
      () => 'opaque-1'
    )
    tracer.inputReceived('session-1', '%7', [
      { type: 'message', message: { role: 'assistant', content: 'secret' } }
    ])
    tracer.transcriptChecked([
      { type: 'message', message: { role: 'assistant', content: 'secret' } },
      { type: 'message', message: { role: 'user', content: 'must-not-log' } }
    ])
    tracer.turnStarted()

    expect(events).toEqual([
      { id: 'opaque-1', sessionId: 'session-1', paneId: '%7', lastBoundary: 'pi_input' },
      {
        id: 'opaque-1',
        sessionId: 'session-1',
        paneId: '%7',
        lastBoundary: 'transcript_user_persisted'
      },
      { id: 'opaque-1', sessionId: 'session-1', paneId: '%7', lastBoundary: 'turn_started' }
    ])
    /* eslint-disable lucy/no-json-stringify */
    // See packages/piing/test/support/JsonFixture.ts's header (#833/#842):
    // a leak scan over the recorded events, not production formatting.
    expect(JSON.stringify(events)).not.toContain('must-not-log')
    /* eslint-enable lucy/no-json-stringify */
  })

  test('failure injection identifies Pi input as the last boundary', () => {
    const tracer = new AttachedInputTracer(
      () => {},
      () => 'opaque-failure'
    )
    tracer.inputReceived('session-2', '%8', [])
    tracer.transcriptChecked([])
    expect(tracer.diagnostic()).toBe(
      'ChiefD attached-input trace opaque-failure: last observed boundary=pi_input; session=session-2; pane=%8. No message content was recorded.'
    )
  })

  test('does not associate an unrelated turn before this input is persisted', () => {
    const events: unknown[] = []
    const tracer = new AttachedInputTracer(
      (event) => events.push(event),
      () => 'opaque-fence'
    )
    tracer.inputReceived('session-3', '%9', [])
    tracer.turnStarted()
    expect(events).toEqual([
      { id: 'opaque-fence', sessionId: 'session-3', paneId: '%9', lastBoundary: 'pi_input' }
    ])
  })

  test('counts only persisted user messages', () => {
    expect(
      persistedUserEntryCount([
        { type: 'message', message: { role: 'assistant' } },
        { type: 'message', message: { role: 'user' } },
        null
      ])
    ).toBe(1)
  })
})
