// @vitest-environment jsdom
//
// The Founder conversation hook. The behaviour worth locking is the one that
// looks like a shortcut and is not: the transcript is re-read when a turn
// ENDS, including when it FAILS.
//
// A page that only appended the reply would silently drop the launch tool's
// own result row — the one line that says a company was created — and a page
// that skipped the re-read on failure would show the operator typing into
// nothing, because their own message is in the session either way.
import { act, type ReactElement, type ReactNode } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

interface HoistedMocks {
  api: Record<string, unknown>
}

const mocks = vi.hoisted((): HoistedMocks => ({ api: {} }))

vi.mock('@/providers/ApiSessionProvider', () => ({
  useChiefApi: () => mocks.api,
  ApiSessionProvider: ({ children }: { children: ReactNode }) => children
}))

import { useFounderConversation } from '@/hooks/UseFounderConversation'
import { ChiefApiError } from '@/types/ApiErrors'
import type { FounderConversationResult } from '@/types/Founder'

let latest: FounderConversationResult | undefined

function Probe(): ReactElement {
  latest = useFounderConversation()
  return <span />
}

/** One session entry in the shape `rowsFromTranscript` reads. */
function entry(id: string, role: 'user' | 'assistant', text: string): unknown {
  return { type: 'message', id, message: { role, content: [{ type: 'text', text }] } }
}

async function flushWork(): Promise<void> {
  for (let index = 0; index < 8; index += 1) await Promise.resolve()
}

describe('useFounderConversation', () => {
  let container: HTMLDivElement
  let root: Root

  beforeEach(() => {
    latest = undefined
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

  it('hydrates from the transcript the server holds', async () => {
    mocks.api = {
      founderTranscript: vi.fn(async () => ({ entries: [entry('e1', 'assistant', 'hello')] })),
      founderSay: vi.fn(),
      founderAbort: vi.fn()
    }

    await act(async () => {
      root.render(<Probe />)
      await flushWork()
    })

    expect(latest?.hydrating).toBe(false)
    expect(latest?.rows).toHaveLength(1)
  })

  it('re-reads the transcript after a turn, so tool rows are never lost', async () => {
    const founderTranscript = vi
      .fn()
      .mockResolvedValueOnce({ entries: [] })
      .mockResolvedValueOnce({
        entries: [entry('e1', 'user', 'build me a company'), entry('e2', 'assistant', 'Done.')]
      })
    mocks.api = {
      founderTranscript,
      founderSay: vi.fn(async () => ({ reply: 'Done.' })),
      founderAbort: vi.fn()
    }

    await act(async () => {
      root.render(<Probe />)
      await flushWork()
    })
    await act(async () => {
      await latest?.send('build me a company')
      await flushWork()
    })

    expect(founderTranscript).toHaveBeenCalledTimes(2)
    expect(latest?.rows).toHaveLength(2)
  })

  it('re-reads the transcript even when the turn was REFUSED', async () => {
    const founderTranscript = vi
      .fn()
      .mockResolvedValueOnce({ entries: [] })
      .mockResolvedValueOnce({ entries: [entry('e1', 'user', 'hello')] })
    mocks.api = {
      founderTranscript,
      founderSay: vi.fn(async () => {
        throw new ChiefApiError({
          kind: 'conflict',
          status: 409,
          code: 'founder-route-unset',
          detail: 'no route configured'
        })
      }),
      founderAbort: vi.fn()
    }

    await act(async () => {
      root.render(<Probe />)
      await flushWork()
    })
    await act(async () => {
      await latest?.send('hello')
      await flushWork()
    })

    expect(latest?.error?.code).toBe('founder-route-unset')
    expect(founderTranscript).toHaveBeenCalledTimes(2)
    expect(latest?.rows).toHaveLength(1)
    // And the composer is usable again: a refusal must not leave the page
    // stuck reporting a turn that is not running.
    expect(latest?.pending).toBe(false)
  })

  it('reports the launched company from the turn that created it', async () => {
    mocks.api = {
      founderTranscript: vi.fn(async () => ({ entries: [] })),
      founderSay: vi.fn(async () => ({
        reply: 'Done.',
        launched: { slug: 'acme-inc', name: 'Acme Inc' }
      })),
      founderAbort: vi.fn()
    }

    await act(async () => {
      root.render(<Probe />)
      await flushWork()
    })
    await act(async () => {
      await latest?.send('call it Acme Inc')
      await flushWork()
    })

    expect(latest?.launched).toEqual({ slug: 'acme-inc', name: 'Acme Inc' })
  })

  it('reads a null launch as no launch rather than as a company', async () => {
    // The wire says `nullish`. `null` reaching state would render a link to
    // `/c/undefined`.
    mocks.api = {
      founderTranscript: vi.fn(async () => ({ entries: [], launched: null })),
      founderSay: vi.fn(),
      founderAbort: vi.fn()
    }

    await act(async () => {
      root.render(<Probe />)
      await flushWork()
    })

    expect(latest?.launched).toBeUndefined()
  })
})
