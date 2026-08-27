/**
 * A COMPACTION THAT CANNOT SUMMARIZE SAYS SO.
 *
 * The verb exists to rescue an oversized session, and the summarization call it
 * makes carries that same oversized session — so the case compaction is FOR is
 * the case the provider answers `400 … maximum context length …`. Recorded raw,
 * that reads as a transient provider fault and invites a retry that can never
 * succeed. Measured on `taperoom-inc` 2026-08-20: `research-lead` has two
 * compact requests, both failed with exactly that 400, and the pane showed the
 * raw provider dump.
 */
import { compactionFailureReason } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

const TOO_LARGE =
  '400: {"message":"This endpoint\'s maximum context length is 262144 tokens. However, you requested 418078 tokens."}'

describe('a failed compaction explains an unsummarizable session', () => {
  test('a context-length refusal names the wall and the way past it', () => {
    const reason = compactionFailureReason(TOO_LARGE)
    expect(reason).toContain('exceeded the model')
    // THE REMEDY MUST BE ONE THAT EXISTS. This asserted `fresh_session`, and
    // that tool is deleted — an operator-facing refusal naming a verb nobody
    // can call is worse than one that names nothing, because it sends them
    // somewhere. The two remedies that remain are a bigger window or a
    // stop/start, and the sentence now says both.
    expect(reason).toContain('larger window')
    expect(reason).toContain('stop and start')
    expect(reason).not.toContain('fresh_session')
    expect(reason).toContain('a retry will fail the same way')
    // The provider's own words survive: an operator diagnosing this needs the
    // numbers, not only our sentence about them.
    expect(reason).toContain(TOO_LARGE)
  })

  test('every other failure is passed through untouched', () => {
    for (const raw of [
      'Summarization failed: 502 status code (no body)',
      'Native compaction receipt diverged from the persisted Pi session anchor',
      'socket hang up'
    ]) {
      expect(compactionFailureReason(raw)).toBe(raw)
    }
  })
})
