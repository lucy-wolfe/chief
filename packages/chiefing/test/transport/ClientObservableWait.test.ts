/**
 * A refusal chiefd went to the trouble of sending must reach a caller that is
 * still listening.
 *
 * chiefd's writer actor is one thread per company, and a mutation queued behind
 * deep work waits its turn. `actor::MUTATION_QUEUE_DEADLINE` (30s) is the ONE
 * bounded wait in the whole system: a job that reaches the front inside it
 * runs, and a job that does not is reaped with `ChiefdError::Busy` -> **429**,
 * the one status whose entire meaning is "back off and ask again"
 * (`isTransientChiefdError`).
 *
 * `FetchTransport` used to abandon that request after 10s. Every answer chiefd
 * produced between 10s and 30s — the successful commit AND the 429 — arrived at
 * nobody. Worse than a lost answer: the abort raises `kind: 'timeout'`, which
 * `isTransientChiefdError` deliberately classifies as NON-transient (retrying a
 * timeout can double-apply a write), so the caller stopped, reported a failure,
 * and the queued mutation then ran and committed anyway. The caller believed
 * the write did not happen; the write happened.
 *
 * The relationship, not the numbers, is the fix: the client's patience must
 * exceed every bound chiefd can hold it behind.
 * `scripts/test/client-observable-wait.test.mjs` derives both ends and fails
 * when they cross. This file pins the behaviour that relationship buys.
 */
import { describe, expect, it, vi } from 'vitest'

import { ChiefdUnavailableError, isTransientChiefdError } from '@/Errors'
import { postOrgRoute } from '@/resources/OrgRoutes'
import { FetchTransport } from '@/transport/FetchTransport'

const URL_BASE = 'http://127.0.0.1:1'
const PATH = '/v1/org/supervision/read'

/** The literal bytes `RouteError::busy` puts on the wire for a queue-deadline
 *  refusal. Written out rather than built: a fixture that stands in for
 *  chiefd's wire shape is only evidence if it is the shape chiefd sends. */
const BUSY_BODY = '{"code":"Busy","detail":"waited 30000ms at mutation-queue"}'

const OK_BODY = '{"ok":true}'

/**
 * How long chiefd is made to hold its answer.
 *
 * 11s is not arbitrary and must not be "tidied" downwards: it is just past the
 * 10s patience this client used to abandon at, and far inside the patience it
 * now has. Below 10s this test passes against the defect it exists to catch.
 */
const ANSWER_AFTER_MS = 11_000

interface DelayedAnswer {
  delayMs: number
  status: number
  body: string
}

/** A `fetch` that answers after `delayMs` with a real `Response`, and that
 *  honours the abort signal the way a real one does — rejecting with the
 *  signal's own reason, which is the `TimeoutError` `AbortSignal.timeout`
 *  raises. The `AbortSignal.timeout` inside `FetchTransport` is untouched: the
 *  client's own patience is the thing under test, so it is the one thing not
 *  stubbed. */
function answeringFetch(answers: DelayedAnswer[]): {
  calls: () => number
  impl: (url: string, init?: { signal?: AbortSignal }) => Promise<Response>
} {
  let calls = 0
  return {
    calls: () => calls,
    impl: (_url, init) => {
      const answer = answers[Math.min(calls, answers.length - 1)]
      calls += 1
      return new Promise<Response>((resolve, reject) => {
        const timer = setTimeout(() => {
          resolve(new Response(answer.body, { status: answer.status }))
        }, answer.delayMs)
        init?.signal?.addEventListener('abort', () => {
          clearTimeout(timer)
          const reason = init.signal?.reason
          reject(reason instanceof Error ? reason : new Error('aborted'))
        })
      })
    }
  }
}

describe('a mutation queued behind deep work', () => {
  it('answers the caller a 429 it observes and retries, never a client-side abort', async () => {
    const fetchStub = answeringFetch([
      { delayMs: ANSWER_AFTER_MS, status: 429, body: BUSY_BODY },
      { delayMs: 0, status: 200, body: OK_BODY }
    ])
    vi.stubGlobal('fetch', fetchStub.impl)
    try {
      const transport = new FetchTransport(URL_BASE)

      // What a caller does: post, and on chiefd's own "come back" go round
      // again. The predicate is the production one, not a stand-in.
      let observed: unknown
      let result: unknown
      for (let attempt = 0; ; attempt += 1) {
        try {
          result = await postOrgRoute(transport, URL_BASE, PATH, {})
          break
        } catch (error) {
          observed = error
          if (!isTransientChiefdError(error) || attempt >= 1) throw error
        }
      }

      // The refusal reached the caller in the form chiefd sent it. Before the
      // client outlived the queue deadline this was `kind: 'timeout'` with no
      // status at all, and the mutation ran anyway.
      expect(observed).toBeInstanceOf(ChiefdUnavailableError)
      if (!(observed instanceof ChiefdUnavailableError)) {
        throw new Error('expected ChiefdUnavailableError')
      }
      expect(observed.kind).toBe('http-error')
      expect(observed.status).toBe(429)
      expect(isTransientChiefdError(observed)).toBe(true)

      // …and the retry that instruction invites actually happened.
      expect(fetchStub.calls()).toBe(2)
      expect(result).toEqual({ ok: true })
    } finally {
      vi.unstubAllGlobals()
    }
  }, 40_000)

  it('the abandoned patience is what turned that same 429 into a non-transient failure', async () => {
    // The defect, pinned so it can never come back as a default. Scaled down to
    // milliseconds because the shape — an answer that arrives after the client
    // stopped listening — is what matters, not the magnitude.
    const fetchStub = answeringFetch([{ delayMs: 300, status: 429, body: BUSY_BODY }])
    vi.stubGlobal('fetch', fetchStub.impl)
    try {
      const transport = new FetchTransport(URL_BASE, 50)
      let observed: unknown
      try {
        await postOrgRoute(transport, URL_BASE, PATH, {})
      } catch (error) {
        observed = error
      }
      expect(observed).toBeInstanceOf(ChiefdUnavailableError)
      if (!(observed instanceof ChiefdUnavailableError)) {
        throw new Error('expected ChiefdUnavailableError')
      }
      expect(observed.kind).toBe('timeout')
      expect(observed.status).toBeUndefined()
      // The half that makes it worse than a lost answer: nobody retries this,
      // and chiefd's queued mutation runs regardless.
      expect(isTransientChiefdError(observed)).toBe(false)
      expect(fetchStub.calls()).toBe(1)
    } finally {
      vi.unstubAllGlobals()
    }
  })
})
