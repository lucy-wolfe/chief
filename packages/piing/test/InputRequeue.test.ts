/**
 * A bare submission on a busy pane is re-queued, never thrown away.
 *
 * # The defect
 *
 * The operator's pane showed this "randomly and often":
 *
 *   Error: Agent is already processing. Specify streamingBehavior
 *   ('steer' or 'followUp') to queue the message.
 *
 * It comes from Pi's `AgentSession.prompt()`, which throws when `isStreaming`
 * is true and the call carried no `streamingBehavior`. Pi's own interactive
 * TUI makes exactly those bare calls — for the operator's typed line whenever
 * the TUI judged the pane idle at submit time, and for the initial message
 * chief passes on every spawn — and any turn started in between makes them
 * throw.
 *
 * **What is lost is the SUBMITTED PROMPT, never a mailbox envelope.** Mail
 * cannot take this path: every delivery goes through `sendCustomMessage`, which
 * routes rather than throws, and the durable envelope is held under the
 * delivery-attempt lease until Pi confirms it. The typed line has no such
 * protection: the TUI clears its editor at submit and `showError` renders the
 * bare error while persisting nothing at all. That is why the operator's
 * screenshot was the only evidence this defect had ever produced.
 *
 * # Why the rule lives in the intercom
 *
 * `team-ui.ts` described this interception in a comment, beside a flag with
 * three writers and zero readers and a helper with zero callers — it was never
 * wired. It is built where the RACER lives instead: the thing that makes a pane
 * busy underneath a bare submission is the intercom's own turn-triggering.
 */
import {
  inputInterceptionDecision,
  inputRequeueLogDetail
} from '@test-assets/organization-intercom'
import { describe, expect, it } from 'vitest'

describe('input interception', () => {
  it('re-queues a bare submission on a busy pane', () => {
    // The one case that would otherwise reach Pi's throw.
    const busy = inputInterceptionDecision({ text: 'ship it', source: 'interactive' }, false)
    expect(busy).toBe('requeue')
  })

  it('leaves an idle pane alone', () => {
    // Nothing to rescue: `prompt()` cannot throw when nothing is streaming, and
    // an interception here would put every ordinary line through a needless
    // re-submission.
    const idle = inputInterceptionDecision({ text: 'ship it', source: 'interactive' }, true)
    expect(idle).toBe('continue')
  })

  it('cannot loop, because a submission that already names a behaviour passes', () => {
    // THE LINE THAT PROVES THE RESCUE TERMINATES. The re-submission carries
    // `deliverAs: 'followUp'`, so it arrives at this handler with a behaviour
    // set and takes the `continue` arm — exactly once through, never twice.
    for (const streamingBehavior of ['steer', 'followUp'] as const) {
      const already = inputInterceptionDecision({ text: 'ship it', streamingBehavior }, false)
      expect(already, `${streamingBehavior} is already queueable`).toBe('continue')
    }
  })

  it('reads whether the behaviour is present, not whether it is truthy', () => {
    // A submitter can legitimately pass nothing; only `undefined` means "the
    // submitter believed the pane was idle".
    const explicit = inputInterceptionDecision({ text: '', streamingBehavior: undefined }, false)
    expect(explicit).toBe('requeue')
    // An empty submission on a busy pane is still a submission that would throw.
    expect(inputInterceptionDecision({ text: '' }, false)).toBe('requeue')
  })

  it('treats every source the same way', () => {
    // The throw does not care where the text came from, so neither does the
    // rescue. `source` is recorded in the log line, never used as a condition.
    for (const source of ['interactive', 'rpc', 'extension'] as const) {
      expect(inputInterceptionDecision({ text: 'x', source }, false)).toBe('requeue')
      expect(inputInterceptionDecision({ text: 'x', source }, true)).toBe('continue')
    }
  })
})

describe('the rescue log line', () => {
  it('carries the shape of the submission and never its text', () => {
    // #645. The text belongs to Pi's session writer; this line exists to make
    // the event COUNTABLE, which is the thing that did not exist before.
    const secret = 'deploy the thing with password hunter2'
    const detail = inputRequeueLogDetail('ada', {
      text: secret,
      source: 'interactive',
      images: [{}, {}]
    })
    expect(detail).toEqual({
      personId: 'ada',
      source: 'interactive',
      length: secret.length,
      images: 2
    })
    // Asserted over the VALUES rather than a serialisation: the rule is that no
    // field carries the text, and reading the fields says exactly that.
    const carried = Object.values(detail).join(' ')
    expect(carried).not.toContain('hunter2')
    expect(carried).not.toContain('deploy')
  })

  it('names an unknown source rather than omitting the field', () => {
    // A missing field reads as "nobody looked"; `unknown` reads as "looked, and
    // it did not say" — and the two want different follow-up investigations.
    const detail = inputRequeueLogDetail('ada', { text: 'x' })
    expect(detail.source).toBe('unknown')
    expect(detail.images).toBe(0)
    expect(detail.length).toBe(1)
  })
})
