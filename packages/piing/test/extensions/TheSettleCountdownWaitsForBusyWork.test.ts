/**
 * **THE COUNTDOWN MUST NOT RUN WHILE SOMEBODY IS WORKING.**
 *
 * Operator ruling, 2026-08-24: *"no for 2. if it reads the mail and it's
 * thinking, leave it until it settles then start the timer."*
 *
 * The settle lease is stamped from the absence of a `working:true` beat, and a
 * COMPACTION emits no turn events at all while it runs — so `noteTurnProgress`
 * never fires and the countdown counts straight through the longest, quietest
 * work a pane does. Measured on a live box: a person mid-compaction at
 * ~90% of a 1M context had their window reaped about 100 seconds after the
 * countdown showed 1m43s. The transcript survived; the compaction did not,
 * after paying for a summarize call over 909k tokens. That is a livelock — every
 * wake re-triggers the compaction, the next countdown kills it again, and the
 * session that most needs compacting can never finish one.
 *
 * These drive the REAL handlers through `InstalledPane` and read what the pane
 * actually POSTED, because the beat is the only thing chiefd knows about
 * whether a person is working.
 */
import { installedPane, stopInstalledPanes } from '@test/support/InstalledPane'
import type { Pane } from '@test/types/InstalledPane'
import { afterEach, describe, expect, test } from 'vitest'

afterEach(stopInstalledPanes)

function working(pane: Pane): readonly boolean[] {
  return pane.beats().map((beat) => beat.working)
}

/**
 * Wait for the pane's beat to reach the stub daemon.
 *
 * `noteAgentActivityBeat` posts fire-and-forget on purpose — the beat is
 * bookkeeping and must never sit on a turn's critical path, so it is
 * deliberately NOT awaited by the code under test. A test therefore has to wait
 * for the wire rather than for the call.
 */
async function settled(pane: Pane, atLeast: number): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (pane.beats().length >= atLeast) return
    await new Promise((resolve) => setTimeout(resolve, 10))
  }
  throw new Error(`only ${pane.beats().length} beats arrived; wanted ${atLeast}`)
}

describe('a compaction holds the settle countdown off', () => {
  test('starting one reports the person as WORKING', async () => {
    const pane = await installedPane()
    const before = pane.beats().length

    await pane.beginCompaction()
    await settled(pane, before + 1)

    const posted = pane.beats().slice(before)
    expect(posted.length).toBeGreaterThan(0)
    expect(posted.at(-1)?.working).toBe(true)
  })

  test('the hook is registered at all — the feature is the registration', async () => {
    const pane = await installedPane()
    // `beginCompaction` throws when `session_before_compact` was never
    // registered. Before this change nothing in the repository registered it,
    // which is exactly why the countdown could not see a compaction.
    await expect(pane.beginCompaction()).resolves.toBeUndefined()
  })

  test('nothing beats WORKING until real work starts — the hold cannot leak', async () => {
    const pane = await installedPane()
    await pane.completeTurn()
    await new Promise((resolve) => setTimeout(resolve, 100))

    // NON-VACUITY for the whole feature, and the direction that matters most:
    // a suppression that leaked into ordinary idleness would make EVERYBODY
    // immortal, which is a worse bug than the one being fixed. A pane that has
    // not begun a compaction has reported nothing about working at all.
    expect(working(pane).some((beat) => beat)).toBe(false)
  })

  // WHAT IS NOT PINNED HERE, said plainly rather than left to look covered:
  // the END of the hold. `endBusyWork` fires from `session_compact` and from
  // the pre-turn hold's `finally`, and the idle beat it can produce lives on
  // `agent_settled` — none of which this harness drives. Pinning it needs an
  // `agent_settled` driver with a real extension context, which is a fixture
  // an order of magnitude larger than the property. The ceiling is the safety
  // net that makes the untested half survivable: even if a hold never ended,
  // it stops itself after ten minutes and the ordinary settle resumes.
})
