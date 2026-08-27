/**
 * The Founder→CEO handoff, as it crosses the wire.
 *
 * chiefd hands the operator's tmux client to the new company's CEO as part of
 * a launch. When it cannot — nobody attached, or tmux refused — it says so on
 * the launch outcome as `handoffWarning`, and the company is still created,
 * durable and running.
 *
 * This client used to drop that field on the floor: the narrowing check only
 * asked for `slug`/`url`/`chiefPersonId`/`session`, the result type declared
 * nothing else, and so the Founder announced "CEO booted in its ChiefD tmux
 * session" to an operator who was still sitting in the Founder pane. A launch
 * that reports a place the operator is not is the one claim they have no
 * reason to check.
 *
 * #1051 moved this route from one JSON body to a phase stream, so the FIXTURE
 * below now answers `text/event-stream`. Every rule these tests already held is
 * unchanged and still asserted — the warning is carried verbatim, and a warning
 * this client cannot read is never reported as a launch. Only the shape of the
 * wire they are asserted over moved.
 */
import { afterEach, describe, expect, test } from 'vitest'

import { FounderLaunchClient } from '@/resources/FounderLaunch'

const CREATED = {
  slug: 'leo-capital',
  url: 'http://127.0.0.1:8791',
  chiefPersonId: 'executive-ceo',
  session: 'org-leo-capital'
}

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

/* eslint-disable lucy/no-json-stringify */
// Test fixture wire text — the same exemption the production module takes.

/** One SSE frame, exactly as chiefd's `lifecycle_sse` encodes it. */
function frame(event: string, payload: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(payload)}\n\n`
}
/* eslint-enable lucy/no-json-stringify */

function sse(body: string): Response {
  return new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } })
}

/** Answer with the phases, then one `launched` terminal frame. */
function answerWithStream(phases: readonly string[], body: Record<string, unknown>): void {
  const text =
    phases.map((phase) => frame('phase', { phase, slug: 'leo-capital', detail: '' })).join('') +
    frame('launched', body)
  const fake: typeof fetch = async () => sse(text)
  globalThis.fetch = fake
}

/** Answer with a terminal frame and no phases — what the assertions carried
 *  over from the pre-stream wire need, and nothing more. */
function answerWith(body: Record<string, unknown>): void {
  answerWithStream([], body)
}

function launch(): Promise<unknown> {
  return new FounderLaunchClient({ url: 'http://127.0.0.1:54321' }).launch({
    name: 'Leo Capital',
    purpose: 'Invest'
  })
}

describe('FounderLaunchClient carries chiefd’s handoff verdict', () => {
  test('a launch that moved the operator carries no warning', async () => {
    answerWith(CREATED)
    const result = await new FounderLaunchClient({ url: 'http://127.0.0.1:54321' }).launch({
      name: 'Leo Capital',
      purpose: 'Invest'
    })
    expect(result.session).toBe('org-leo-capital')
    expect(result.handoffWarning).toBeUndefined()
  })

  test('a launch that handed nobody over surfaces chiefd’s exact words', async () => {
    const warning =
      "No tmux client was attached to the Founder session 'e2e', so nobody was handed over. Attach with: chief attach leo-capital"
    answerWith({ ...CREATED, handoffWarning: warning })
    const result = await new FounderLaunchClient({ url: 'http://127.0.0.1:54321' }).launch({
      name: 'Leo Capital',
      purpose: 'Invest'
    })
    // Verbatim: the recovery command inside it is the operator's next move.
    expect(result.handoffWarning).toBe(warning)
  })

  test('a handoff verdict this client cannot read is never reported as a launch', async () => {
    // A non-string warning means chiefd and this client disagree about the
    // contract. Guessing "probably fine" is how the missing field went
    // unnoticed the first time.
    answerWith({ ...CREATED, handoffWarning: { moved: 0 } })
    await expect(launch()).rejects.toThrow(/body this client cannot read/)
  })
})

describe('the launch is bounded, and a timeout never claims the launch failed', () => {
  test('the request carries an abort signal', async () => {
    // The second unbounded wait on this path: `fetch` had no signal at all, so
    // a chiefd that accepted the connection and then stalled left the Founder
    // waiting for ever with nothing on screen.
    let seen: RequestInit | undefined
    // Answers the STREAM #1051 made this route, so the launch this test drives
    // actually completes; the assertion below — that a signal was sent at all —
    // is unchanged.
    const fake: typeof fetch = async (_input, init) => {
      seen = init
      return sse(frame('launched', CREATED))
    }
    globalThis.fetch = fake

    await launch()
    expect(seen?.signal).toBeInstanceOf(AbortSignal)
  })

  test('a timeout reports an UNKNOWN outcome, never a failed one', async () => {
    // Genesis is one transaction and this client never retries, so aborting
    // undoes nothing — it only destroys this side's knowledge of whether a
    // company exists. Reporting that as "the launch failed" is the more
    // damaging lie, because it tells the operator to ignore a company that may
    // be running.
    const timeout = new Error('The operation was aborted due to timeout')
    timeout.name = 'TimeoutError'
    const fake: typeof fetch = async () => {
      throw timeout
    }
    globalThis.fetch = fake

    await expect(launch()).rejects.toThrow(/may or may not have been created/)
    await expect(launch()).rejects.toThrow(/chief ls/)
    await expect(launch()).rejects.toThrow(/do not assume the launch failed/)
  })

  test('every other transport failure keeps its own cause', async () => {
    // Only a timeout is ambiguous. A refused connection means chiefd was never
    // reached, and replacing that with "the outcome is unknown" would lose the
    // most specific thing this side knows.
    const refused = new Error('connect ECONNREFUSED 127.0.0.1:54321')
    const fake: typeof fetch = async () => {
      throw refused
    }
    globalThis.fetch = fake

    await expect(launch()).rejects.toThrow(/ECONNREFUSED/)
  })
})

describe('#1051: the launch narrates itself while it runs', () => {
  test('every phase reaches the caller, in order, before the answer', async () => {
    // THE INCIDENT: a 4m34s launch showed `⠹ Working...` and nothing else, so
    // the operator concluded it had hung. Each of these is a step chiefd was
    // already emitting into a channel whose receiver had been dropped.
    answerWithStream(
      [
        'beacond-ensure',
        'company-claim',
        'company-daemon-start',
        'company-daemon-ready',
        'durable-create',
        'durable-create-complete',
        'ceo-prepare',
        'handover',
        'handover-complete'
      ],
      CREATED
    )
    const seen: string[] = []
    const result = await new FounderLaunchClient({ url: 'http://127.0.0.1:54321' }).launch(
      { name: 'Leo Capital', purpose: 'Invest' },
      (phase) => seen.push(phase.phase)
    )
    expect(seen).toEqual([
      'beacond-ensure',
      'company-claim',
      'company-daemon-start',
      'company-daemon-ready',
      'durable-create',
      'durable-create-complete',
      'ceo-prepare',
      'handover',
      'handover-complete'
    ])
    // The contract is unchanged: it still answers only the finished launch.
    expect(result.slug).toBe('leo-capital')
  })

  test('a caller that wants no progress still gets its launch', async () => {
    answerWithStream(['durable-create'], CREATED)
    const result = await launch()
    expect(result).toMatchObject({ slug: 'leo-capital' })
  })

  test('a phase name this client has never heard of is still reported', async () => {
    // Forward compatibility: chiefd adding a step must never make this pane go
    // quiet, which is the entire defect being fixed.
    answerWithStream(['a-phase-from-the-future'], CREATED)
    const seen: string[] = []
    await new FounderLaunchClient({ url: 'http://127.0.0.1:54321' }).launch(
      { name: 'Leo Capital', purpose: 'Invest' },
      (phase) => seen.push(phase.phase)
    )
    expect(seen).toEqual(['a-phase-from-the-future'])
  })

  test('a malformed progress line is skipped, never fatal to a running launch', async () => {
    const text = 'event: phase\ndata: not json\n\n' + frame('launched', CREATED)
    globalThis.fetch = async () => sse(text)
    const seen: string[] = []
    const result = await new FounderLaunchClient({ url: 'http://127.0.0.1:54321' }).launch(
      { name: 'Leo Capital', purpose: 'Invest' },
      (phase) => seen.push(phase.phase)
    )
    expect(seen).toEqual([])
    expect(result.slug).toBe('leo-capital')
  })

  test('a terminal failure carries chiefd’s own words, not a status code', async () => {
    const text =
      frame('phase', { phase: 'durable-create', slug: 'leo-capital', detail: '' }) +
      frame('failed', { code: 'lifecycle-failed', detail: "'leo-capital' already exists" })
    globalThis.fetch = async () => sse(text)
    await expect(launch()).rejects.toThrow(/already exists/)
  })

  test('a stream that stops without a terminal frame rejects, and never resolves', async () => {
    // The worst possible answer is a resolved promise for a company nobody can
    // prove exists — the model would tell the operator it was launched.
    globalThis.fetch = async () =>
      sse(frame('phase', { phase: 'durable-create', slug: 'leo-capital', detail: '' }))
    await expect(launch()).rejects.toThrow(/without reporting an outcome/)
  })

  test('the keep-alive comment is liveness, not a phase', async () => {
    const text = ':hb\n\n' + frame('launched', CREATED)
    globalThis.fetch = async () => sse(text)
    const seen: string[] = []
    await new FounderLaunchClient({ url: 'http://127.0.0.1:54321' }).launch(
      { name: 'Leo Capital', purpose: 'Invest' },
      (phase) => seen.push(phase.phase)
    )
    expect(seen).toEqual([])
  })
})
