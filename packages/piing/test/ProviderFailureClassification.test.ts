/**
 * A FAILURE THAT NO RETRY CAN CLEAR MUST NOT BE FILED AS A PROVIDER OUTAGE.
 *
 * Observed on the operator's live company (`Taperoom Inc`, 2026-08-18). Two
 * unrelated things went wrong in the same ten minutes and the log could not
 * tell them apart, because every one of the 39 `provider-turn-failed` events
 * carried `kind=provider_error`:
 *
 *   19  deepseek/deepseek-v4-flash-0731 | Connection error.
 *    8  moonshotai/kimi-k2.6            | 400: ... maximum context length ...
 *
 * The first is a genuine transient upstream window, and the reliability
 * escalation is right to count it. The second is a 400 about the request we
 * built — ~28k of prompt against a 233817-token OUTPUT reservation, overflowing
 * a 262144 window by 31 tokens. It will be rejected identically forever. Filed
 * as `provider_error` it did two harmful things: it inflated the consecutive
 * count that escalates "check that Pi's provider access and model health" to a
 * manager, and it did so while the provider was demonstrably healthy (a replay
 * of the same call with the pane's own environment returned HTTP 200).
 *
 * So the classifier must name it, and the count must ignore it.
 */
import { providerFailureDiagnostic } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

/** Pi's `agent_end` shape, reduced to the fields the classifier reads. */
function endedTurn(errorMessage: string): unknown {
  return { messages: [{ role: 'assistant', stopReason: 'error', content: [], errorMessage }] }
}

const CONTEXT_OVERFLOW =
  '400: {"message":"This endpoint\'s maximum context length is 262144 tokens. ' +
  'However, you requested about 262175 tokens (18355 of text input, 10003 of ' +
  'tool input, 233817 in the output)."}'

describe('providerFailureDiagnostic', () => {
  test('names the permanent context overflow instead of blaming the provider', () => {
    expect(providerFailureDiagnostic(endedTurn(CONTEXT_OVERFLOW))?.kind).toBe('request_too_large')
  })

  test('the transient failures from the same incident stay provider_error', () => {
    expect(providerFailureDiagnostic(endedTurn('Connection error.'))?.kind).toBe('provider_error')
    expect(providerFailureDiagnostic(endedTurn('503 status code (no body)'))?.kind).toBe(
      'provider_error'
    )
    expect(providerFailureDiagnostic(endedTurn('terminated'))?.kind).toBe('provider_error')
  })

  test('names an empty account instead of blaming the provider', () => {
    // Measured on a live box, 2026-08-20: 46 of these in one hour, 30
    // of them the Chief's own. Filed as `provider_error` they climbed the
    // reliability counter and mailed a manager AGENT "check that Pi's provider
    // access and model health" — a remedy only the account's owner can perform.
    expect(
      providerFailureDiagnostic(endedTurn('402: {"message":"insufficient_credits"}'))?.kind
    ).toBe('insufficient_credits')
    expect(
      providerFailureDiagnostic(endedTurn('402 insufficient credits for this request'))?.kind
    ).toBe('insufficient_credits')
    // A 402 that is NOT about credits is not this kind, and a credits mention
    // without the status is not either — both stay the generic tail rather than
    // being guessed into a permanent kind that suppresses the alert.
    expect(providerFailureDiagnostic(endedTurn('402: payment required'))?.kind).toBe(
      'provider_error'
    )
    expect(providerFailureDiagnostic(endedTurn('insufficient_credits'))?.kind).toBe(
      'provider_error'
    )
  })

  test('the kinds that already had names keep them', () => {
    expect(providerFailureDiagnostic(endedTurn('content_filter triggered'))?.kind).toBe(
      'content_filter'
    )
    expect(providerFailureDiagnostic(endedTurn('stream ended without finish_reason'))?.kind).toBe(
      'stream_ended_without_finish_reason'
    )
    expect(providerFailureDiagnostic(endedTurn('upstream idle timeout'))?.kind).toBe(
      'upstream_idle_timeout'
    )
  })

  test('the raw error string is still carried through for the log and the card', () => {
    expect(providerFailureDiagnostic(endedTurn(CONTEXT_OVERFLOW))?.errorMessage).toBe(
      CONTEXT_OVERFLOW
    )
  })

  test('a turn that did not end in error is not a failure at all', () => {
    expect(
      providerFailureDiagnostic({
        messages: [{ role: 'assistant', stopReason: 'stop', content: [] }]
      })
    ).toBeUndefined()
    expect(providerFailureDiagnostic({})).toBeUndefined()
  })
})
