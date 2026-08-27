/**
 * #1051 — the Founder pane went quiet for four and a half minutes.
 *
 * `chiefd_launch_company` was called at 21:17:59.552Z and answered at
 * 21:22:33.643Z. For the whole 4m34s the pane showed:
 *
 *     chiefd_launch_company
 *     ⠹ Working...
 *
 * Cost stayed at `$0.000` — the agent was not thinking, it was blocking on a
 * synchronous tool call — and every process on the box was idle. The operator
 * concluded it had hung. It had not; it was waiting, legitimately, and nothing
 * said so.
 *
 * Two halves of the fix are asserted here: the words a human reads for each
 * phase, and the elapsed clock that distinguishes slow from stuck. The stream
 * that carries the phases is `packages/chiefing`'s
 * (`FounderLaunchTest.test.ts`); the Pi `onUpdate` seam that renders them is
 * this file's.
 */
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, test } from 'vitest'

import { launchPhaseLabel, launchProgressText } from '../../extensions/founder-launch'

const PACKAGE_ROOT = join(import.meta.dirname, '..', '..')
const SOURCE = readFileSync(join(PACKAGE_ROOT, 'extensions/founder-launch.ts'), 'utf8')

/** chiefd's closed phase vocabulary, as `host/phases.rs` publishes it. */
const CHIEFD_PHASES = [
  'preflight',
  'beacond-ensure',
  'beacond-starting',
  'beacond-ready',
  'company-claim',
  'company-daemon-start',
  'company-daemon-ready',
  'durable-create',
  'durable-create-complete',
  'durable-create-failed',
  'company-daemon-stop',
  'company-daemon-stopped',
  'company-daemon-stop-failed',
  // 'ceo-prepare' / 'ceo-prepare-failed' were here until chief-home-is-cwd §4c
  // deleted the phases. A phase left in this list with no label is what the
  // test below catches; one deleted from chiefd but LEFT here would instead
  // assert that a dead phase still renders, which is the vacuous half.
  'chief-start',
  'chief-start-failed',
  'handover',
  'handover-complete'
] as const

describe('every phase a human can be shown has words for it', () => {
  test('each chiefd phase renders as a sentence, never as its wire name', () => {
    for (const phase of CHIEFD_PHASES) {
      const label = launchPhaseLabel(phase)
      expect(label, `${phase} has no label`).not.toBe(phase)
      expect(label.length, `${phase} renders empty`).toBeGreaterThan(0)
    }
  })

  test('the step that happens before chiefd is called is labelled too', () => {
    // `starting` is the tool's own first moment, before any HTTP call is made,
    // so chiefd's stream cannot narrate it and this process must.
    expect(launchPhaseLabel('starting')).not.toBe('starting')
  })

  test('an unknown phase falls back to its raw name and is never dropped', () => {
    // A chiefd that learns a new step must not make this pane go quiet again.
    // Silence is the defect; an ugly label is not.
    expect(launchPhaseLabel('a-phase-from-the-future')).toBe('a-phase-from-the-future')
  })
})

describe('the cold beacond path reads differently from the warm one', () => {
  test('starting the directory and finding it up are different sentences', () => {
    // The user asked to SEE beacond boot. A shared label for both paths would
    // say "checking" while a process is being spawned and waited for.
    const starting = launchPhaseLabel('beacond-starting')
    const ready = launchPhaseLabel('beacond-ready')
    const probing = launchPhaseLabel('beacond-ensure')
    expect(starting).not.toBe(probing)
    expect(starting).not.toBe(ready)
    expect(starting.toLowerCase()).toContain('starting')
  })

  test('the preflight, which is the very first thing that happens, has words', () => {
    expect(launchPhaseLabel('preflight').toLowerCase()).toContain('host')
  })
})

describe('the progress line tells slow apart from stuck', () => {
  test('it names the company and the running step', () => {
    const text = launchProgressText('Leo Capital', 'durable-create', 0)
    expect(text).toContain('Leo Capital')
    expect(text).toContain(launchPhaseLabel('durable-create'))
  })

  test('the elapsed clock advances, which is the half that carries the meaning', () => {
    // One phase owned most of the reported 4m34s. A phase name alone still
    // looks frozen for four minutes; a number that climbs does not.
    const early = launchProgressText('Leo Capital', 'durable-create', 3_000)
    const later = launchProgressText('Leo Capital', 'durable-create', 42_000)
    expect(early).toContain('3s')
    expect(later).toContain('42s')
    expect(early).not.toBe(later)
  })

  test('a wait past a minute reads in minutes and seconds, not raw seconds', () => {
    // The incident's own duration. "274s" is a number a human has to convert.
    expect(launchProgressText('Leo Capital', 'durable-create', 274_000)).toContain('4m 34s')
  })

  test('it never reports a negative elapsed time', () => {
    expect(launchProgressText('Leo Capital', 'starting', -5)).toContain('0s')
  })
})

describe('the tool actually uses the Pi progress seam', () => {
  /* The defect was not a missing mechanism — Pi has always handed `execute` an
   * `onUpdate` — but a tool that declared `(toolCallId, params)` and dropped
   * it. A rendering helper that nothing calls would leave the pane exactly as
   * quiet as before, so the wiring is asserted against the source. */
  test('execute takes the onUpdate callback Pi passes it', () => {
    expect(SOURCE).toMatch(/async execute\(_toolCallId, params, _signal, onUpdate\)/)
  })

  test('progress is reported before the first await, not only after chiefd answers', () => {
    // The pane must speak before the launch reaches chiefd at all: a first
    // report placed after the awaited call would leave it silent for however
    // long the whole launch takes.
    const launchAt = SOURCE.indexOf('founder.chiefd.launch')
    const firstReportAt = SOURCE.indexOf('report()')
    expect(launchAt).toBeGreaterThan(-1)
    expect(firstReportAt).toBeGreaterThan(-1)
    expect(firstReportAt).toBeLessThan(launchAt)
  })

  /* #1052 instruments the same awaits this file reports on. The two must stay
   * at the SAME boundaries: a trace record whose step does not correspond to a
   * reported phase describes a launch the pane never showed, and vice versa. */
  test('the trace and the progress report bracket the same call', () => {
    const traced = SOURCE.indexOf('founder.chiefd.launch')
    const forwarded = SOURCE.indexOf('phase = frame.phase')
    expect(traced).toBeGreaterThan(-1)
    expect(forwarded).toBeGreaterThan(traced)
  })

  test('the ticker is always cleared, including when the launch fails', () => {
    expect(SOURCE).toContain('finally {')
    expect(SOURCE).toContain('clearInterval(tick)')
  })

  test('the launch is still awaited — a tool that returns early is not the fix', () => {
    // A tool that answered before the company was up would trade a visible
    // wait for an invisible failure. The call is wrapped in `trace.measure`
    // since #1052, so the await is asserted on the wrapper and the call itself
    // on the client.
    expect(SOURCE).toContain('const launched = await trace.measure(')
    expect(SOURCE).toContain('client.launch({ name, purpose }')
  })
})
