// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { BootPhaseConsole } from '@/components/companies/BootPhaseConsole'

function phaseTexts(container: HTMLDivElement): string[] {
  const phases: HTMLDivElement[] = []
  for (const candidate of container.querySelectorAll('[data-lifecycle-phase]')) {
    if (candidate instanceof HTMLDivElement) phases.push(candidate)
  }
  return phases.map((phase) => phase.textContent ?? '')
}

describe('BootPhaseConsole', () => {
  let container: HTMLDivElement
  let root: Root

  beforeEach(() => {
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
  })

  afterEach(() => {
    act(() => {
      root.unmount()
    })
    container.remove()
  })

  it('renders every received phase in arrival order without a browser-owned vocabulary', () => {
    act(() => {
      root.render(
        <BootPhaseConsole
          label="Create company"
          phases={[
            { phase: 'company-daemon-start', detail: 'starting' },
            { phase: 'new-upstream-phase', detail: 'still opaque' }
          ]}
          running
        />
      )
    })

    expect(phaseTexts(container)).toEqual([
      'company-daemon-start — starting',
      'new-upstream-phase — still opaque'
    ])
    expect(container.textContent).toContain('Waiting for lifecycle result')
  })

  it('surfaces a failed terminal verbatim and offers a retry', () => {
    const retry = vi.fn()
    act(() => {
      root.render(
        <BootPhaseConsole
          failure={{ code: 'company-not-api-hosted', detail: 'company is hosted by tmux' }}
          label="Boot company"
          onRetry={retry}
          phases={[]}
          running={false}
        />
      )
    })

    expect(container.textContent).toContain('company-not-api-hosted: company is hosted by tmux')
    act(() => {
      container.querySelector('button')?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    expect(retry).toHaveBeenCalledTimes(1)
  })
})
