// @vitest-environment jsdom
import { act, createElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { PaneFooter } from '@/components/company/PaneFooter'
import type { PersonFooterModel } from '@/types/OrgStore'

describe('PaneFooter', () => {
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

  function render(footer: PersonFooterModel): void {
    act(() => {
      root.render(createElement(PaneFooter, { footer }))
    })
  }

  it('renders no segment when every count is zero/undefined', () => {
    render({ pendingMailboxCount: undefined })
    expect(container.querySelectorAll('[data-footer-segment]')).toHaveLength(0)
  })

  it('hides 📬 at exactly 0 (read, but empty) and before the first read (undefined)', () => {
    render({ pendingMailboxCount: 0 })
    expect(container.querySelector('[data-footer-segment="mailbox-pending"]')).toBeNull()

    render({ pendingMailboxCount: undefined })
    expect(container.querySelector('[data-footer-segment="mailbox-pending"]')).toBeNull()
  })

  it('renders the mailbox segment once its count is positive', () => {
    render({ pendingMailboxCount: 3 })
    expect(container.querySelector('[data-footer-segment="mailbox-pending"]')?.textContent).toBe(
      '📬 3'
    )
  })

  it('renders no countdown text anywhere (apps/api exposes no typed schedule fields yet)', () => {
    render({ pendingMailboxCount: 1 })
    expect(container.textContent).not.toMatch(/\d+[hm]\b/)
    expect(container.textContent).not.toContain('due')
  })

  it('updates segments when the footer model changes (store reactivity)', () => {
    render({ pendingMailboxCount: undefined })
    expect(container.querySelectorAll('[data-footer-segment]')).toHaveLength(0)
    render({ pendingMailboxCount: 5 })
    expect(container.querySelector('[data-footer-segment="mailbox-pending"]')?.textContent).toBe(
      '📬 5'
    )
  })
})
