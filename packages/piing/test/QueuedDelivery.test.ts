/**
 * A mailbox delivery to a BUSY pane must not ask Pi to start a turn.
 *
 * Pi 0.80's `sendCustomMessage` routes on `isStreaming`, which is false during
 * an active run's tool execution. A delivery arriving in that window therefore
 * skipped the follow-up queue, fell through to `triggerTurn`, and called
 * `agent.prompt()` — which throws `Agent is already processing a prompt` while
 * a run is active. Pi catches that itself and emits it as an
 * `Extension "<runtime>" error`, so the extension's own try/catch never saw it
 * and the message was dropped. A live CEO pane showed 44 of those errors with
 * nine messages still unread.
 *
 * The previous version of this rule was tested only as "the option names are
 * both present", which is why the busy case shipped: the shape was asserted,
 * the CONDITION was not.
 */
import {
  firstRunGateForTest,
  queuedPiDeliveryForTest,
  workResumeNeedsRedrive
} from '@test-assets/organization-intercom'
import { beforeEach, describe, expect, it } from 'vitest'

describe('queued Pi delivery', () => {
  // #1208: every case below describes a session PAST its first run, which is
  // what they always meant — before the boot gate existed there was no other
  // state to be in. Stating it here rather than changing any assertion: the
  // three cases are byte-for-byte what they were, and the gate's own states are
  // the separate block at the bottom.
  beforeEach(() => {
    firstRunGateForTest().open()
  })

  it('asks for a turn only when no turn is in flight', () => {
    const idle = queuedPiDeliveryForTest('followUp', false)
    expect(idle.triggerTurn).toBe(true)
    expect(idle.deliverAs).toBe('followUp')
    expect(idle.streamingBehavior).toBe('followUp')
  })

  it('never asks a busy pane to start a turn', () => {
    for (const mode of ['followUp', 'steer'] as const) {
      const busy = queuedPiDeliveryForTest(mode, true)
      expect(busy.triggerTurn, `${mode} must not trigger a turn mid-run`).toBeUndefined()
      // The message is still DELIVERED: streaming takes the queue, and a
      // non-streaming active run appends it for the turn already running.
      expect(busy.deliverAs).toBe(mode)
      expect(busy.streamingBehavior).toBe(mode)
    }
  })

  it('keeps both option names so either Pi generation honours one', () => {
    for (const turnActive of [true, false]) {
      const options = queuedPiDeliveryForTest('steer', turnActive)
      expect(options.deliverAs).toBe('steer')
      expect(options.streamingBehavior).toBe('steer')
    }
  })
})

/**
 * #1208 — the boot window, where a delivery must not start a turn at all.
 *
 * Pi's interactive TUI calls `prompt()` BARE for the initial message chief
 * passes on every spawn. Anything that flips `isStreaming` between the TUI's
 * idle judgment and `prompt()`'s own check turns that call into
 * `Error: Agent is already processing…`, and the operator's boot instruction is
 * gone — the TUI clears its editor at submit and `showError` persists nothing.
 *
 * Before this gate, the intercom was that flipper: a delivery landing in the
 * boot window took `triggerTurn`, whose `_runAgentPrompt` sets the run-active
 * flag on its first line.
 */
describe('the boot gate', () => {
  it('answers nextTurn and never triggers, in every mode and either busy state', () => {
    for (const mode of ['followUp', 'steer'] as const) {
      for (const turnActive of [true, false]) {
        const boot = queuedPiDeliveryForTest(mode, turnActive, true)
        expect(boot.deliverAs, `${mode}/${turnActive} rides the coming turn`).toBe('nextTurn')
        const started = boot.triggerTurn
        expect(started, 'starting a turn in the boot window is the whole defect').toBeUndefined()
        expect(boot.streamingBehavior, 'nextTurn is not a streaming disposition').toBeUndefined()
      }
    }
  })

  it('restores the exact busy/idle table once it opens', () => {
    // The test that stops the gate widening into a behaviour change.
    expect(queuedPiDeliveryForTest('followUp', false, false)).toEqual({
      deliverAs: 'followUp',
      streamingBehavior: 'followUp',
      triggerTurn: true
    })
    expect(queuedPiDeliveryForTest('steer', true, false)).toEqual({
      deliverAs: 'steer',
      streamingBehavior: 'steer'
    })
    expect(queuedPiDeliveryForTest('followUp', true, false)).toEqual({
      deliverAs: 'followUp',
      streamingBehavior: 'followUp'
    })
  })

  it('opens on its own when no first run ever arrives', async () => {
    // chief always passes an initial message, so `agent_start` always comes.
    // A hand-run `pi` need not, and mail on such a pane must not be parked for
    // ever.
    const gate = firstRunGateForTest()
    gate.close(1)
    expect(gate.isOpen(), 'closed the instant the session starts').toBe(false)
    await new Promise((resolve) => setTimeout(resolve, 25))
    expect(gate.isOpen(), 'the fallback opens it without an agent_start').toBe(true)
    expect(queuedPiDeliveryForTest('followUp', false).triggerTurn).toBe(true)
  })

  /**
   * THE MEASURED LIVELOCK, as a regression.
   *
   * A live company, 2026-08-27: mail delivered inside the boot window parks in
   * Pi's `_pendingNextTurnMessages`, whose only reader is the next prompt
   * submission. On a RESUME relaunch no first turn ever comes, so nothing reads
   * it; `mailboxDeliveryAttempts` is released only at `agent_settled`, which
   * needs that absent turn; and the next retry rides a fresh session with a
   * fresh window and parks again. One envelope, queued twice ninety seconds
   * apart, never consumed — **every retry arriving through a door that is
   * closed at the moment of arrival.**
   *
   * The fix is that gate RESOLUTION now has a consequence. This pins it.
   */
  it('runs the re-delivery consequence when no first run ever arrives', async () => {
    const gate = firstRunGateForTest()
    let resolutions = 0
    gate.close(1, () => {
      resolutions += 1
    })
    expect(resolutions, 'nothing happens while the window is open').toBe(0)
    await new Promise((resolve) => setTimeout(resolve, 25))
    expect(resolutions, 'the fallback must re-deliver what the window parked').toBe(1)
  })

  /**
   * ORDER IS LOAD-BEARING, and getting it backwards would be silent.
   *
   * The consequence drains, and the drain asks `queuedPiDelivery`, which reads
   * the gate. If the consequence ran while the gate were still closed, every
   * envelope would park exactly as it did on the way in and the rescue would
   * re-create the livelock it exists to end — with no error anywhere.
   */
  it('opens the gate BEFORE running the consequence, or the rescue re-parks', async () => {
    const gate = firstRunGateForTest()
    let openAtResolution: boolean | undefined
    let deliveryAtResolution: string | undefined
    gate.close(1, () => {
      openAtResolution = gate.isOpen()
      deliveryAtResolution = queuedPiDeliveryForTest('followUp', false).deliverAs
    })
    await new Promise((resolve) => setTimeout(resolve, 25))
    expect(openAtResolution, 'the gate is open by the time the consequence runs').toBe(true)
    expect(
      deliveryAtResolution,
      'so the re-delivery starts a turn instead of parking again'
    ).not.toBe('nextTurn')
  })

  /**
   * THE HAPPY PATH IS A NO-OP, so nothing is delivered twice.
   *
   * When a real first turn came, `agent_start` opened the gate and cleared the
   * fallback timer. The ordinary drain rides that turn; a second re-delivery
   * would be mail delivered twice for no reason.
   */
  it('never runs the consequence when a first run did arrive', async () => {
    const gate = firstRunGateForTest()
    let resolutions = 0
    gate.close(1, () => {
      resolutions += 1
    })
    gate.open()
    await new Promise((resolve) => setTimeout(resolve, 25))
    expect(resolutions, 'agent_start clears the fallback: no second delivery').toBe(0)
  })

  /**
   * THE RESUME PROMPT NEEDS ITS FLAGS RESET, or the rescue rescues nothing.
   *
   * `requestWorkResume` guards on `pending && !prompted`. A prompt parked in
   * the boot window left the opposite — `prompted`, nothing `pending` — so a
   * bare re-drive at resolution returns early. This is the predicate the
   * consequence uses, pinned as the rule it is.
   */
  it('re-drives a work-resume prompt that was parked, and only that one', () => {
    expect(
      workResumeNeedsRedrive(true, false),
      'prompted into a queue nobody read: this is the parked case'
    ).toBe(true)
    expect(
      workResumeNeedsRedrive(false, true),
      'still pending and never prompted: the ordinary path owns it'
    ).toBe(false)
    expect(workResumeNeedsRedrive(false, false), 'nothing to resume').toBe(false)
    expect(workResumeNeedsRedrive(true, true), 'prompted AND pending is not the parked shape').toBe(
      false
    )
  })

  it('closes again for the next session and reopens on its first run', () => {
    const gate = firstRunGateForTest()
    gate.close()
    expect(queuedPiDeliveryForTest('steer', false).deliverAs).toBe('nextTurn')
    gate.open()
    expect(queuedPiDeliveryForTest('steer', false).deliverAs).toBe('steer')
  })
})
