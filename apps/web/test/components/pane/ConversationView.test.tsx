// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { ConversationView } from '@/components/pane/ConversationView'
import type { ConversationRow } from '@/types/Conversation'

function rows(text: string): readonly ConversationRow[] {
  return [
    {
      kind: 'message',
      id: text,
      role: 'assistant',
      content: [{ type: 'text', text }],
      streaming: false
    }
  ]
}

describe('ConversationView', () => {
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

  it('does not fight scrollback and offers an explicit jump-to-latest action', () => {
    act(() => {
      root.render(<ConversationView accent="#5b1fa8" rows={rows('First')} />)
    })
    const list = container.querySelector<HTMLDivElement>('[aria-label="Agent conversation"]')
    if (!list) throw new Error('expected conversation list')
    Object.defineProperties(list, {
      clientHeight: { configurable: true, value: 100 },
      scrollHeight: { configurable: true, value: 500 },
      scrollTop: { configurable: true, value: 100, writable: true }
    })

    act(() => {
      list.scrollTop = 10
      list.dispatchEvent(new Event('scroll', { bubbles: true }))
    })
    expect(container.textContent).toContain('Jump to latest')

    act(() => {
      root.render(<ConversationView accent="#5b1fa8" rows={rows('Second')} />)
    })
    expect(list.scrollTop).toBe(10)

    const jump = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Jump to latest'
    )
    if (!jump) throw new Error('expected jump control')
    act(() => jump.click())
    expect(list.scrollTop).toBe(500)
  })
})
