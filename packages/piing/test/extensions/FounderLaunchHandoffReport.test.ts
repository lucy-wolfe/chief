/**
 * What the Founder tells the operator after a launch.
 *
 * A live end-to-end run got this back:
 *
 *     ✅ Company launched · Leo Capital
 *     CEO booted in its ChiefD tmux session
 *
 * The CEO had booted. The operator had not been taken there — their client was
 * still attached to the Founder session, and `tmux list-panes -a` showed the
 * CEO pane sitting unattached in `org-leo-capital`. chiefd knew: it put a
 * `handoffWarning` on the launch outcome. The tool announced the handoff
 * anyway, because the success text was unconditional.
 *
 * The claim is now conditional on chiefd's own verdict, and the failure branch
 * carries the recovery command instead of a place the operator is not.
 */
import { reportLaunch } from '@test-assets/founder-launch'
import { describe, expect, test } from 'vitest'

const CREATED = {
  slug: 'leo-capital',
  url: 'http://127.0.0.1:8791',
  chiefPersonId: 'executive-ceo',
  session: 'org-leo-capital'
}

describe('reportLaunch', () => {
  test('a real handoff says the operator is in the CEO', () => {
    const { text, details } = reportLaunch('Leo Capital', CREATED)
    expect(text).toContain('✅ Company launched · Leo Capital')
    expect(text).toContain('You are now in the CEO of Leo Capital')
    expect(text).toContain('org-leo-capital')
    expect(details).toEqual({
      ok: true,
      slug: 'leo-capital',
      session: 'org-leo-capital',
      handedOver: true
    })
  })

  test('a launch that moved nobody never claims the operator was moved', () => {
    // chiefd's own words for the unattended launch, which is now the only
    // branch that hands this recovery over: the company is UP, and there was
    // simply no client to move into it.
    const warning =
      'The company is running, but no tmux client was attached to the Founder session, so nobody was handed over. chiefd decides who runs, and a client is what runs them. In another terminal run and LEAVE OPEN: chief actuate leo-capital — then: chief attach leo-capital'
    const { text, details } = reportLaunch('Leo Capital', { ...CREATED, handoffWarning: warning })

    // The company IS created — saying otherwise would be a lie in the more
    // damaging direction.
    expect(text).toContain('✅ Company created · Leo Capital')
    // But the handover claim is retracted, in words, and chiefd's recovery
    // command comes with it.
    expect(text).toContain('You were NOT moved to the CEO')
    expect(text).toContain('chief actuate leo-capital')
    expect(text).not.toContain('You are now in the CEO')
    expect(details.handedOver).toBe(false)
  })

  /**
   * THE LIVE-PROOF REGRESSION (#751/P8), and its own correction.
   *
   * The warning branch used to open with "The CEO is booted in tmux session
   * org-leo-capital, but you were NOT moved there." After the actuation
   * switchover the first half was false for exactly the launches that reach
   * this branch: chiefd publishes actions and a client applies them, so a
   * company `chief` had just created had a daemon and a CEO-only intent
   * and NOBODY running — the session it named did not exist, which is why the
   * handoff failed in the first place. A live run watched the Founder report a
   * booted CEO for a company whose `tmux list-panes -a` was empty.
   *
   * That fix replaced the claim with its opposite — "the CEO is NOT running" —
   * which THIS TEST ASSERTED, and which the Founder→CEO transition makes false
   * in turn. chiefd now starts the actuator, waits for the lease, states the
   * CEO-only intent and waits for the company's session BEFORE it switches any
   * client, so a warning here means the operator was not moved, and no longer
   * means the company is dead: an unattended launch (a script, apps/api) has no
   * client to move and leaves a company that is up and running.
   *
   * The assertion is therefore rewritten rather than relaxed: the tool must
   * claim NEITHER state, in either direction, because it is on the far side of
   * an HTTP boundary from the only process that watched the bring-up. Both of
   * the sentences it got wrong are pinned as forbidden, and chiefd's own
   * warning — whatever it says — must survive into the text intact.
   */
  test('the warning branch claims nothing about whether the CEO is running', () => {
    const warning =
      "chief attach: started an actuator for 'leo-capital' in tmux session 'chiefd-actuator-org-leo-capital_' on socket 'default', and its window exited. What it printed:\nPi is required but no runtime was found.\nchiefd attach: 'leo-capital' has nobody actuating it, so its people are not running and there is no session to enter. In another terminal, run and LEAVE OPEN:\n    chief actuate leo-capital\nthen run 'chief attach leo-capital' again."
    const { text } = reportLaunch('Leo Capital', { ...CREATED, handoffWarning: warning })

    // The claim that a live run proved false.
    expect(text).not.toContain('The CEO is booted')
    expect(text).not.toContain('is booted in tmux session')
    // And its opposite, which the transition proves false in turn — the tool
    // must not assert it on its own account. It appears in this text only if
    // chiefd's own warning said it.
    expect(text.replace(warning, '')).not.toContain('the CEO is NOT running')
    // What this process actually observed, and nothing more.
    expect(text).toContain('✅ Company created · Leo Capital')
    expect(text).toContain('You were NOT moved to the CEO')
    // chiefd's verdict survives whole, including the command that runs it.
    expect(text).toContain(warning)
    expect(text).toContain('chief actuate leo-capital')
  })

  test('a blank warning is not a warning', () => {
    // Whitespace must not be able to retract a handoff that happened.
    expect(
      reportLaunch('Leo Capital', { ...CREATED, handoffWarning: '   ' }).details.handedOver
    ).toBe(true)
  })
})
