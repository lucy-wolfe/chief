// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { AgentConversationResult } from '@/types/Conversation'

interface PaneHookMock {
  conversation: unknown
}

const mocked = vi.hoisted((): PaneHookMock => ({ conversation: undefined }))

vi.mock('@/hooks/UseAgentConversation', () => ({
  useAgentConversation: () => mocked.conversation
}))

import { AgentPane } from '@/components/pane/AgentPane'

function conversation(overrides: Partial<AgentConversationResult> = {}): AgentConversationResult {
  return {
    rows: [],
    session: { isStreaming: false, thinkingLevel: 'medium' },
    host: { state: 'running' },
    hostState: 'running',
    channel: 'healthy',
    hydrating: false,
    runtime: { isCompacting: false, isRetrying: false, isSettled: false, queuedMessages: 0 },
    paneError: undefined,
    send: vi.fn().mockResolvedValue(undefined),
    abort: vi.fn().mockResolvedValue(undefined),
    // NO `listModels`, `changeModel`, `changeThinking`, `newSession`,
    // `compact` or `startPerson`. The last three dialled routes this app has
    // never served; the first three are decisions chief no longer makes at
    // all. They are gone from the hook's result rather than stubbed here, so
    // a pane that reached for one would not compile.
    ...overrides
  }
}

describe('AgentPane', () => {
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

  it('uses the same pane body for tmux-hosted companies while hiding its controls', () => {
    mocked.conversation = conversation({
      host: { state: 'stopped' },
      hostState: 'stopped'
    })

    act(() => {
      root.render(
        <AgentPane
          pane={{ paneId: 'person-ceo', title: 'CEO', accentColor: null, kind: 'person' }}
          readOnly
          companyKey="0123456789ab"
        />
      )
    })

    expect(container.textContent).toContain('Visible via CLI tmux')
    expect(container.textContent).toContain('Agent is dormant')
    expect(container.querySelector('[aria-label="Pane controls"]')).toBeNull()
    expect(container.querySelector('textarea[aria-label="Message agent"]')).toBeNull()
    expect(container.textContent).not.toContain('Start')
  })

  it('renders server refusals as exact pane-local code and detail text', () => {
    mocked.conversation = conversation({
      paneError: {
        kind: 'refusal',
        code: 'company-not-api-hosted',
        detail: 'Acme is hosted in tmux.'
      }
    })

    act(() => {
      root.render(
        <AgentPane
          pane={{ paneId: 'person-ceo', title: 'CEO', accentColor: '#5b1fa8', kind: 'person' }}
          readOnly={false}
          companyKey="0123456789ab"
        />
      )
    })

    expect(container.querySelector('[role="alert"]')?.textContent).toBe(
      'company-not-api-hosted: Acme is hosted in tmux.'
    )
  })
})
