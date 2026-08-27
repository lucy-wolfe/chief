// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { MessageBubble } from '@/components/pane/MessageBubble'

describe('MessageBubble', () => {
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

  it('labels a streaming assistant message and renders custom entries as preformatted text', () => {
    act(() => {
      root.render(
        <>
          <MessageBubble
            accent="#5b1fa8"
            row={{
              kind: 'message',
              id: 'assistant-1',
              role: 'assistant',
              content: [{ type: 'text', text: 'Growing response' }],
              streaming: true
            }}
          />
          <MessageBubble
            accent="#5b1fa8"
            row={{
              kind: 'message',
              id: 'card-1',
              role: 'custom',
              content: [{ type: 'text', text: 'ASSIGNMENT 42' }],
              streaming: false
            }}
          />
        </>
      )
    })

    expect(container.textContent).toContain('Agent')
    expect(container.querySelector('[aria-label="Streaming response"]')).not.toBeNull()
    expect(container.querySelector('pre')?.textContent).toContain('ASSIGNMENT 42')
  })
})
