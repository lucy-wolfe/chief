// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { ToolActivityRow } from '@/components/pane/ToolActivityRow'

describe('ToolActivityRow', () => {
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

  it('shows running, done, and error states while keeping a result collapsible', () => {
    const base: {
      kind: 'tool'
      id: string
      toolCallId: string
      toolName: string
      argsPreview: string
    } = {
      kind: 'tool',
      id: 'tool:read-1',
      toolCallId: 'read-1',
      toolName: 'read',
      argsPreview: '{…}'
    }

    act(() => {
      root.render(<ToolActivityRow row={{ ...base, state: 'running' }} />)
    })
    expect(container.querySelector('[data-tool-state="running"]')).not.toBeNull()

    act(() => {
      root.render(
        <ToolActivityRow row={{ ...base, state: 'done', resultPreview: 'file contents' }} />
      )
    })
    expect(container.querySelector('[data-tool-state="done"]')).not.toBeNull()
    const reveal = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent === 'Show result'
    )
    if (!reveal) throw new Error('expected result disclosure')
    act(() => reveal.click())
    expect(container.querySelector('pre')?.textContent).toContain('file contents')

    act(() => {
      root.render(
        <ToolActivityRow row={{ ...base, state: 'error', resultPreview: 'permission denied' }} />
      )
    })
    expect(container.querySelector('[data-tool-state="error"]')).not.toBeNull()
  })
})
