/**
 * The pane-side transient-retry backoff must not park the JS thread.
 *
 * `withTransientReadRetry`'s rungs (150ms, 400ms) run on `Atomics.wait`, which
 * blocks the WHOLE thread — during them a pane reads no SSE chunk, renders no
 * footer, drains no mailbox and fires no timer. The load-bearing part is worse
 * than responsiveness: a parked thread cannot service the event loop, so any
 * in-flight `fetch` behind it cannot complete. On an awaited path that is a
 * DEADLOCK, not a slow retry, which is why the transport cutover (#30) needs an
 * awaited ladder to convert onto.
 *
 * These tests pin the awaited twin: same ladder, same shared classifier, same
 * rethrow contract, and — the whole point — the event loop keeps turning while
 * it waits, where the synchronous one starves it dead.
 *
 * #794/E4-S8 migrated the chiefd arm of the shared classifier from a message
 * regex to a structural check (`ChiefdUnavailableError` with
 * `kind: 'unreachable'` — see `packages/chiefing/src/Errors.ts`, whose header
 * bans inspecting `error.message` entirely). The producer here throws that
 * real typed error instead of a plain `Error` with a matching string.
 */
import { ChiefdUnavailableError } from '@chief/chiefing'
import {
  isTransientTransportFailure,
  withTransientReadRetryAsync
} from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

/** The exact producer the shared classifier's chiefd arm has to match. */
const TRANSIENT = (): never => {
  throw new ChiefdUnavailableError({
    kind: 'unreachable',
    url: 'http://127.0.0.1:8792',
    path: '/v1/docs/cas'
  })
}

describe('withTransientReadRetryAsync', () => {
  test('the event loop keeps running while the ladder waits', async () => {
    let ticks = 0
    const ticker = setInterval(() => {
      ticks += 1
    }, 5)
    try {
      let attempts = 0
      const value = await withTransientReadRetryAsync(() => {
        attempts += 1
        if (attempts < 3) TRANSIENT()
        return 'recovered'
      })
      expect(value).toBe('recovered')
      expect(attempts).toBe(3)
      // 150ms + 400ms of waiting: a 5ms interval must have fired many times.
      // Under the synchronous ladder it could not fire even once.
      expect(ticks).toBeGreaterThan(10)
    } finally {
      clearInterval(ticker)
    }
  }, 15_000)

  test('a non-transient failure rethrows immediately, unchanged and unretried', async () => {
    let attempts = 0
    await expect(
      withTransientReadRetryAsync(() => {
        attempts += 1
        throw new Error('Activity authority is missing')
      })
    ).rejects.toThrow('Activity authority is missing')
    expect(attempts).toBe(1)
  })

  test('a transient failure that never clears rethrows the original error after the whole ladder', async () => {
    let attempts = 0
    await expect(
      withTransientReadRetryAsync(() => {
        attempts += 1
        TRANSIENT()
      })
    ).rejects.toBeInstanceOf(ChiefdUnavailableError)
    // Three attempts total: the initial one plus one per ladder rung.
    expect(attempts).toBe(3)
  })

  test('it succeeds without waiting at all when the read works first time', async () => {
    const started = Date.now()
    await expect(withTransientReadRetryAsync(() => 'immediate')).resolves.toBe('immediate')
    expect(Date.now() - started).toBeLessThan(100)
  })

  test('it classifies through the SHARED predicate, never a private copy', () => {
    // #59's root cause was a second, divergent copy of this pattern: a
    // message-regex classifier that no longer matched its producer's exact
    // text, so the retry ladder was dead code from the day it shipped. #794
    // removed that whole class of bug for the chiefd arm by classifying
    // structurally (`instanceof ChiefdUnavailableError && kind ===
    // 'unreachable'`) instead of by string — there is no message to drift
    // out of sync with.
    expect(
      isTransientTransportFailure(
        new ChiefdUnavailableError({
          kind: 'unreachable',
          url: 'http://127.0.0.1:8792',
          path: '/v1/docs/cas'
        })
      )
    ).toBe(true)
    // A non-'unreachable' ChiefdUnavailableError (e.g. a timeout) is
    // deliberately NOT transient — retrying a timeout can double-apply a
    // write (see Errors.ts's doc comment).
    expect(
      isTransientTransportFailure(
        new ChiefdUnavailableError({
          kind: 'timeout',
          url: 'http://127.0.0.1:8792',
          path: '/v1/docs/cas'
        })
      )
    ).toBe(false)
    expect(isTransientTransportFailure(new Error('Activity authority is missing'))).toBe(false)
  })

  test('an async read is awaited, so the cutover can pass it a fetch-backed reader', async () => {
    let attempts = 0
    const value = await withTransientReadRetryAsync(async () => {
      attempts += 1
      if (attempts < 2) TRANSIENT()
      return 'async-recovered'
    })
    expect(value).toBe('async-recovered')
    expect(attempts).toBe(2)
  }, 15_000)
})
