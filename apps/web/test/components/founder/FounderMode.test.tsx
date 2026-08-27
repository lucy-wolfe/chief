// @vitest-environment jsdom
//
// Founder Mode as an operator meets it: a conversation, a composer, and — once
// a company exists — one link into it.
import { act, type ReactNode } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

interface HoistedMocks {
  conversation: unknown
}

const mocks = vi.hoisted((): HoistedMocks => ({ conversation: undefined }))

vi.mock('next/link', () => ({
  default: ({ href, children }: { href: string; children: ReactNode }) => (
    <a href={href}>{children}</a>
  )
}))

vi.mock('@/hooks/UseFounderConversation', () => ({
  useFounderConversation: () => mocks.conversation
}))

import { FounderMode } from '@/components/founder/FounderMode'

function conversation(overrides: Record<string, unknown> = {}): unknown {
  return {
    rows: [],
    pending: false,
    hydrating: false,
    error: undefined,
    launched: undefined,
    send: vi.fn(async () => undefined),
    abort: vi.fn(async () => undefined),
    ...overrides
  }
}

async function flushWork(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

function composer(container: HTMLElement): { form: HTMLFormElement; field: HTMLTextAreaElement } {
  const form = container.querySelector('form')
  const field = container.querySelector('textarea')
  if (!form || !field) throw new Error('the founder composer did not render')
  return { form, field }
}

function setValue(element: HTMLTextAreaElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value')?.set
  if (!setter) throw new Error('field value setter was not available')
  setter.call(element, value)
  element.dispatchEvent(new Event('input', { bubbles: true }))
}

describe('FounderMode', () => {
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

  it('sends what the operator typed, once', async () => {
    const send = vi.fn(async () => undefined)
    mocks.conversation = conversation({ send })

    act(() => {
      root.render(<FounderMode />)
    })
    const { form, field } = composer(container)
    await act(async () => {
      setValue(field, '  A company that ships trading agents  ')
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
      await flushWork()
    })

    expect(send).toHaveBeenCalledTimes(1)
    expect(send).toHaveBeenCalledWith('A company that ships trading agents')
    // Cleared, because a Founder turn can run for minutes and a message left
    // in the box looks unsent.
    expect(field.value).toBe('')
  })

  it('will not start a second turn while one is running', async () => {
    const send = vi.fn(async () => undefined)
    mocks.conversation = conversation({ send, pending: true })

    act(() => {
      root.render(<FounderMode />)
    })
    const { form, field } = composer(container)
    expect(field.disabled).toBe(true)
    await act(async () => {
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
      await flushWork()
    })
    expect(send).not.toHaveBeenCalled()
  })

  it('offers Stop only while a turn is running', () => {
    mocks.conversation = conversation()
    act(() => {
      root.render(<FounderMode />)
    })
    const idle = [...container.querySelectorAll('button')].find(
      (button) => button.textContent === 'Stop'
    )
    // Disabled rather than absent: a control that appears and disappears is
    // harder to read than one that is plainly unavailable.
    expect(idle?.disabled).toBe(true)

    act(() => {
      mocks.conversation = conversation({ pending: true })
      root.render(<FounderMode />)
    })
    const running = [...container.querySelectorAll('button')].find(
      (button) => button.textContent === 'Stop'
    )
    expect(running?.disabled).toBe(false)
  })

  it('renders the conversation rows the server reported', () => {
    mocks.conversation = conversation({
      rows: [
        {
          kind: 'message',
          id: 'e1',
          role: 'user',
          content: [{ type: 'text', text: 'I want a trading company' }],
          streaming: false
        },
        {
          kind: 'message',
          id: 'e2',
          role: 'assistant',
          content: [{ type: 'text', text: 'What should it be called?' }],
          streaming: false
        }
      ]
    })

    act(() => {
      root.render(<FounderMode />)
    })
    expect(container.textContent).toContain('I want a trading company')
    expect(container.textContent).toContain('What should it be called?')
  })

  it('links to the company it created BY KEY, never by the display slug', () => {
    // The three values are deliberately all different. A fixture that reused
    // one string could not tell a key-addressed link from a slug-addressed one.
    mocks.conversation = conversation({
      launched: { key: '4d0e2ed2cec4', slug: 'acme-inc', name: 'Acme Inc' }
    })

    act(() => {
      root.render(<FounderMode />)
    })
    const link = [...container.querySelectorAll('a')].find((anchor) =>
      anchor.textContent?.includes('Acme Inc')
    )
    // `/c/[companyKey]` resolves a company by its DIRECTORY KEY. This link
    // carried the display slug, so it addressed a route that does not resolve
    // one — and two directories may hold companies with the same slug, so
    // there is no repair that keeps the slug and stays correct.
    expect(link?.getAttribute('href')).toBe('/c/4d0e2ed2cec4')
    expect(link?.getAttribute('href')).not.toContain('acme-inc')
  })

  it('surfaces a refusal by its code rather than a blank pane', () => {
    mocks.conversation = conversation({
      error: {
        kind: 'conflict',
        status: 409,
        code: 'founder-route-unset',
        detail: 'This box has not been told which model route to run Founder on.'
      }
    })

    act(() => {
      root.render(<FounderMode />)
    })
    const alert = container.querySelector('[role="alert"]')
    expect(alert?.textContent).toContain('founder-route-unset')
    expect(alert?.textContent).toContain('which model route to run Founder on')
  })
})
