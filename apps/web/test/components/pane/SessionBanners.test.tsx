// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { SessionBanners } from '@/components/pane/SessionBanners'

describe('SessionBanners', () => {
  let container: HTMLDivElement
  let root: Root

  beforeEach(() => {
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
  })

  afterEach(() => {
    act(() => root.unmount())
    container.remove()
  })

  it('keeps tmux, dormant/exited, dead-channel, and active-session states distinct', () => {
    act(() => {
      root.render(
        <SessionBanners
          channel="dead"
          host={{ state: 'exited', exitCode: 17 }}
          hostState="exited"
          readOnly
          runtime={{ isCompacting: true, isRetrying: true, isSettled: false, queuedMessages: 2 }}
        />
      )
    })

    expect(container.textContent).toContain('Visible via CLI tmux')
    expect(container.textContent).toContain('exit 17')
    expect(container.textContent).toContain('Reconnecting session stream')
    expect(container.textContent).toContain('Compacting session')
    expect(container.textContent).toContain('Retrying agent response')
    expect(container.textContent).toContain('2 queued message(s)')
    expect(container.textContent).not.toMatch(/start/i)
  })

  // A dormant person is the RESTING state of every non-CEO pane — a company at
  // rest runs only its CEO — so the banner names it plainly and offers NO
  // action.
  //
  // THE "START AGENT" BUTTON IS GONE, and its absence is the assertion. It
  // posted to `…/people/:id/start`, a route this server has never served, so
  // the one action a dormant pane offered answered 404 and replaced the resting
  // notice with an error: pressing the only button on the pane made the pane
  // worse. And no handler is owed here — who is up is chiefd's roster decision,
  // converged from the durable roster, so a browser waking one person out of
  // band would be a second opinion about the roster. A control that cannot work
  // is worse than its absence, which is why this is a deletion and not a
  // disabled button.
  it('names a dormant agent and offers no control that could not work', () => {
    act(() => {
      root.render(
        <SessionBanners
          channel="healthy"
          host={undefined}
          hostState="stopped"
          readOnly={false}
          runtime={{ isCompacting: false, isRetrying: false, isSettled: false, queuedMessages: 0 }}
        />
      )
    })

    expect(container.textContent).toContain('Agent is dormant')
    expect(container.querySelector('button')).toBeNull()
    expect(container.textContent).not.toMatch(/start/i)
  })

  // Read-only panes are mirrors of a CLI-owned tmux session, and they reach the
  // same place from the other direction: no button, whoever owns the session.
  it('names a dormant agent on a read-only pane too, still with no control', () => {
    act(() => {
      root.render(
        <SessionBanners
          channel="healthy"
          host={undefined}
          hostState="stopped"
          readOnly
          runtime={{ isCompacting: false, isRetrying: false, isSettled: false, queuedMessages: 0 }}
        />
      )
    })

    expect(container.textContent).toContain('Agent is dormant')
    expect(container.querySelector('button')).toBeNull()
  })
})
