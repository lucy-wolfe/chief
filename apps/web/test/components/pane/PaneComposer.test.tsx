// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { PaneComposer } from '@/components/pane/PaneComposer'

function setControlValue(element: HTMLTextAreaElement | HTMLSelectElement, value: string): void {
  const prototype =
    element instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : HTMLSelectElement.prototype
  const descriptor = Object.getOwnPropertyDescriptor(prototype, 'value')
  if (!descriptor?.set) throw new Error('expected native value setter')
  descriptor.set.call(element, value)
  element.dispatchEvent(new Event('input', { bubbles: true }))
  element.dispatchEvent(new Event('change', { bubbles: true }))
}

describe('PaneComposer', () => {
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

  it('offers steer/follow-up only during a stream and blocks a double send', async () => {
    const send = vi.fn().mockResolvedValue(undefined)
    const abort = vi.fn().mockResolvedValue(undefined)

    act(() => {
      root.render(
        <PaneComposer disabled={false} isStreaming={false} onAbort={abort} onSend={send} />
      )
    })
    expect(container.querySelector('select[aria-label="Message mode"]')).toBeNull()

    act(() => {
      root.render(<PaneComposer disabled={false} isStreaming onAbort={abort} onSend={send} />)
    })
    const textarea = container.querySelector<HTMLTextAreaElement>('textarea')
    const mode = container.querySelector<HTMLSelectElement>('select[aria-label="Message mode"]')
    const form = container.querySelector('form')
    if (!textarea || !mode || !form) throw new Error('expected composer controls')

    act(() => {
      setControlValue(textarea, 'Change direction')
      setControlValue(mode, 'steer')
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
    })
    await act(async () => {
      await Promise.resolve()
    })

    expect(send).toHaveBeenCalledTimes(1)
    expect(send).toHaveBeenCalledWith('Change direction', 'steer')
  })

  it('moves Escape focus to the abort affordance', () => {
    const send = vi.fn().mockResolvedValue(undefined)
    const abort = vi.fn().mockResolvedValue(undefined)
    act(() => {
      root.render(<PaneComposer disabled={false} isStreaming onAbort={abort} onSend={send} />)
    })
    const textarea = container.querySelector<HTMLTextAreaElement>('textarea')
    const abortButton = container.querySelector<HTMLButtonElement>('[aria-label="Abort agent"]')
    if (!textarea || !abortButton) throw new Error('expected composer controls')

    act(() => {
      textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    })

    expect(document.activeElement).toBe(abortButton)
  })
})
