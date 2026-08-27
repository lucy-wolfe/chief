import { describe, expect, it } from 'vitest'

import {
  AuthAcquisitionError,
  ChiefdUnavailableError,
  isTransientChiefdError,
  OrgRowRefusalError,
  PersonContractsRefusalError,
  ReminderRefusalError
} from '@/Errors'

describe('isTransientChiefdError truth table', () => {
  it('is true for ChiefdUnavailableError with kind unreachable', () => {
    expect(
      isTransientChiefdError(
        new ChiefdUnavailableError({ kind: 'unreachable', url: 'http://x', path: '/p' })
      )
    ).toBe(true)
  })

  // #1004: 429 is `ChiefdError::Busy`, which chiefd mints only after actually
  // waiting its documented ladder. Its entire meaning is "back off and ask
  // again"; calling it permanent throws away the one instruction it carries.
  it('is true for a 429 — the one http-error status that means "ask again"', () => {
    expect(
      isTransientChiefdError(
        new ChiefdUnavailableError({ kind: 'http-error', url: 'http://x', path: '/p', status: 429 })
      )
    ).toBe(true)
  })

  it('is false for every OTHER http-error status, including a 503', () => {
    for (const status of [400, 404, 409, 422, 500, 502, 503]) {
      expect(
        isTransientChiefdError(
          new ChiefdUnavailableError({ kind: 'http-error', url: 'http://x', path: '/p', status })
        ),
        `${status} must not be transient`
      ).toBe(false)
    }
  })

  it("carries chiefd's own sentence into the message an operator reads", () => {
    const error = new ChiefdUnavailableError({
      kind: 'http-error',
      url: 'http://x',
      path: '/v1/org/rows',
      status: 429,
      detail: 'database is locked'
    })
    expect(error.message).toBe(
      'chiefd unavailable (http-error) at http://x/v1/org/rows: database is locked'
    )
    expect(error.detail).toBe('database is locked')
    // Absent detail leaves the message exactly as it always was.
    expect(
      new ChiefdUnavailableError({ kind: 'unreachable', url: 'http://x', path: '/p' }).message
    ).toBe('chiefd unavailable (unreachable) at http://x/p')
  })

  it('is false for every other ChiefdUnavailableKind', () => {
    expect(
      isTransientChiefdError(
        new ChiefdUnavailableError({ kind: 'timeout', url: 'http://x', path: '/p' })
      )
    ).toBe(false)
    expect(
      isTransientChiefdError(
        new ChiefdUnavailableError({ kind: 'http-error', url: 'http://x', path: '/p', status: 500 })
      )
    ).toBe(false)
    expect(
      isTransientChiefdError(
        new ChiefdUnavailableError({ kind: 'malformed-body', url: 'http://x', path: '/p' })
      )
    ).toBe(false)
  })

  it('is false for every refusal class and for a plain Error', () => {
    expect(
      isTransientChiefdError(new OrgRowRefusalError({ status: 422, code: 'x', detail: 'y' }))
    ).toBe(false)
    expect(
      isTransientChiefdError(
        new ReminderRefusalError({
          status: 422,
          code: 'unknown-company',
          detail: 'no such company'
        })
      )
    ).toBe(false)
    expect(
      isTransientChiefdError(new PersonContractsRefusalError({ code: 'x', detail: 'y' }))
    ).toBe(false)
    expect(isTransientChiefdError(new AuthAcquisitionError())).toBe(false)
    expect(isTransientChiefdError(new Error('random'))).toBe(false)
  })
})

describe('classification is by kind, never by message', () => {
  it('an unreachable error with an empty message is still transient', () => {
    const unreachable = new ChiefdUnavailableError({
      kind: 'unreachable',
      url: 'http://x',
      path: '/p',
      message: ''
    })
    expect(unreachable.message).toBe('')
    expect(isTransientChiefdError(unreachable)).toBe(true)
  })

  it('a plain Error carrying the legacy regex bait text is never transient', () => {
    const bait = new Error('unreachable at http://x/p')
    expect(isTransientChiefdError(bait)).toBe(false)
  })
})

describe('OrgRowRefusalError message', () => {
  it('carries chiefd’s DETAIL, not just the code', () => {
    // chiefd writes the actionable half in `detail` — which mode is effective
    // versus configured, which source a launcher root came from, the command
    // that fixes it. Every layer above reads `.message`, so a message built
    // from the code alone silently decides that an operator sees nothing. Three
    // separate layers were doing that to one chain before this.
    const error = new OrgRowRefusalError({
      status: 422,
      code: 'company-not-api-hosted',
      detail: 'the effective actuation mode is apply, configured apply. Set `shadow` with …'
    })

    expect(error.message).toContain('company-not-api-hosted')
    expect(error.message).toContain('effective actuation mode is apply')
  })

  it('falls back to the code alone when there is no detail', () => {
    const error = new OrgRowRefusalError({ status: 404, code: 'unknown-company', detail: '  ' })

    expect(error.message).toBe('org row refused: unknown-company')
  })
})
