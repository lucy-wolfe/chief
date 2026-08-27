// @vitest-environment jsdom
import { act, createElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { ChiefApiError } from '@/types/ApiErrors'
import type { AgentConversationResult } from '@/types/Conversation'
import type { AgentSessionEvent } from '@/types/SessionEvents'
import type { PersonStreamSnapshot, SessionEventEnvelope } from '@/types/Sse'

interface MockDependencies {
  api: unknown
  stream: unknown
}

const mocked = vi.hoisted((): MockDependencies => ({ api: undefined, stream: undefined }))

vi.mock('@/providers/ApiSessionProvider', () => ({
  useChiefApi: () => mocked.api
}))

vi.mock('@/hooks/UsePersonStream', () => ({
  usePersonStream: () => mocked.stream
}))

import { useAgentConversation } from '@/hooks/UseAgentConversation'

interface ResultBox {
  current: AgentConversationResult | undefined
}

function Harness({ resultBox }: { resultBox: ResultBox }): null {
  resultBox.current = useAgentConversation('acme', 'person-ceo')
  return null
}

function snapshot(overrides: Partial<PersonStreamSnapshot> = {}): PersonStreamSnapshot {
  return {
    channel: 'healthy',
    session: { isStreaming: false, pendingMessageCount: 0 },
    events: [],
    host: { state: 'running', pid: 42 },
    hostState: 'running',
    reorgCount: 0,
    ...overrides
  }
}

function envelope(id: string, event: AgentSessionEvent): SessionEventEnvelope {
  return { id, generation: 1, seq: Number(id), event }
}

async function flush(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

function result(box: ResultBox): AgentConversationResult {
  if (!box.current) throw new Error('agent conversation did not render')
  return box.current
}

describe('useAgentConversation', () => {
  let container: HTMLDivElement
  let root: Root
  let resultBox: ResultBox
  let api: {
    getTranscript: ReturnType<typeof vi.fn>
    say: ReturnType<typeof vi.fn>
    abort: ReturnType<typeof vi.fn>
    listModels: ReturnType<typeof vi.fn>
    changeModel: ReturnType<typeof vi.fn>
    changeThinking: ReturnType<typeof vi.fn>
    newSession: ReturnType<typeof vi.fn>
    compactSession: ReturnType<typeof vi.fn>
  }

  beforeEach(() => {
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
    resultBox = { current: undefined }
    api = {
      getTranscript: vi.fn().mockResolvedValue({ entries: [], leafId: 'leaf-1' }),
      say: vi.fn().mockResolvedValue({ queued: true, generation: 1 }),
      abort: vi.fn().mockResolvedValue({ aborted: true }),
      // A BARE ARRAY — `GET .../models` has no `{models}` envelope.
      listModels: vi.fn().mockResolvedValue([]),
      changeModel: vi.fn().mockResolvedValue({ status: 'applied' }),
      changeThinking: vi.fn().mockResolvedValue({ level: 'medium' }),
      newSession: vi.fn().mockResolvedValue({ cancelled: false }),
      compactSession: vi.fn().mockResolvedValue({ compacted: true })
    }
    mocked.api = api
    mocked.stream = snapshot()
  })

  afterEach(() => {
    act(() => {
      root.unmount()
    })
    container.remove()
  })

  async function render(): Promise<void> {
    act(() => {
      root.render(createElement(Harness, { resultBox }))
    })
    await act(async () => {
      await flush()
    })
  }

  // apps/api answers `…/transcript` with 409 `person-not-running` for an
  // agent with no live child (`AgentTalkService.requireHostedClient`), and the
  // person event stream says which it is in its very first frame: a `state`
  // snapshot for a live child, a `host` frame with `state:"stopped"` for a
  // dormant one. Hydrating regardless meant every pane of every company whose
  // agents were down issued a request whose refusal was certain and then
  // rendered the raw refusal — the ordinary state of a company you just
  // opened.
  it('never asks a dormant agent for a transcript, and hydrates itself when it comes up', async () => {
    mocked.stream = snapshot({ session: undefined, host: undefined, hostState: 'stopped' })
    await render()
    expect(api.getTranscript).not.toHaveBeenCalled()
    expect(result(resultBox).hydrating).toBe(false)

    // The stream reports the child came up. Nothing polled and nothing
    // retried — the push is what triggers the read (mandate 1).
    mocked.stream = snapshot({ hostState: 'running' })
    act(() => {
      root.render(createElement(Harness, { resultBox }))
    })
    await act(async () => {
      await flush()
    })
    expect(api.getTranscript).toHaveBeenCalledTimes(1)
  })

  // Before any host frame arrives there is still an unambiguous answer: the
  // stream's only other possible greeting is a `state` snapshot, which
  // apps/api sends ONLY when there is a live child.
  it('treats a session snapshot with no host frame yet as a live agent', async () => {
    mocked.stream = snapshot({ host: undefined, hostState: undefined })
    await render()
    expect(api.getTranscript).toHaveBeenCalledTimes(1)
  })

  it('treats a stream that has said nothing at all as not live', async () => {
    mocked.stream = snapshot({ session: undefined, host: undefined, hostState: undefined })
    await render()
    expect(api.getTranscript).not.toHaveBeenCalled()
  })

  it('hydrates transcript rows once and folds later live events', async () => {
    api.getTranscript.mockResolvedValue({
      entries: [
        {
          type: 'message',
          id: 'prior-user',
          message: { role: 'user', content: [{ type: 'text', text: 'Earlier request' }] }
        }
      ],
      leafId: 'leaf-1'
    })

    await render()

    expect(api.getTranscript).toHaveBeenCalledTimes(1)
    expect(result(resultBox).rows).toMatchObject([
      { kind: 'message', role: 'user', streaming: false }
    ])

    mocked.stream = snapshot({
      events: [
        envelope('2', {
          type: 'message_start',
          message: { role: 'assistant', content: [{ type: 'text', text: 'Live response' }] }
        })
      ]
    })
    await render()

    expect(result(resultBox).rows).toMatchObject([
      { kind: 'message', role: 'user' },
      { kind: 'message', role: 'assistant', streaming: true }
    ])
  })

  it('rehydrates exactly once for a reorg and rebuilds rows without duplicates', async () => {
    api.getTranscript
      .mockResolvedValueOnce({
        entries: [
          {
            type: 'message',
            id: 'old',
            message: { role: 'user', content: [{ type: 'text', text: 'Old transcript' }] }
          }
        ],
        leafId: 'leaf-1'
      })
      .mockResolvedValueOnce({
        entries: [
          {
            type: 'message',
            id: 'fresh',
            message: { role: 'assistant', content: [{ type: 'text', text: 'Fresh transcript' }] }
          }
        ],
        leafId: 'leaf-2'
      })

    await render()
    mocked.stream = snapshot({ reorgCount: 1 })
    await render()

    expect(api.getTranscript).toHaveBeenCalledTimes(2)
    expect(result(resultBox).rows).toEqual([
      {
        kind: 'message',
        id: 'entry:fresh',
        role: 'assistant',
        content: [{ type: 'text', text: 'Fresh transcript' }],
        streaming: false
      }
    ])
  })

  it('sends and aborts through the API without inserting a browser-only message row', async () => {
    await render()
    const before = result(resultBox).rows

    await act(async () => {
      await result(resultBox).send('Please continue', 'prompt')
      await result(resultBox).abort()
    })

    // `text`, not `message`. The hook used to build `{message}` while the route
    // read `body.text`; both halves were tested, both were green, and every
    // message an operator typed came back `422 empty-message`. One word for one
    // thing is the only durable fix, and this pins the word.
    expect(api.say).toHaveBeenCalledWith('acme', 'person-ceo', {
      text: 'Please continue',
      mode: 'prompt'
    })
    expect(api.abort).toHaveBeenCalledWith('acme', 'person-ceo')
    expect(result(resultBox).rows).toBe(before)
  })

  it('surfaces a refusal verbatim as pane-local error state', async () => {
    api.say.mockRejectedValue(
      new ChiefApiError({
        kind: 'refusal',
        status: 422,
        code: 'model-policy-refused',
        detail: 'This model is not allowed for this person.'
      })
    )
    await render()

    await act(async () => {
      await result(resultBox).send('Try it', 'prompt')
    })

    expect(result(resultBox).paneError).toEqual({
      kind: 'refusal',
      status: 422,
      code: 'model-policy-refused',
      detail: 'This model is not allowed for this person.'
    })
  })

  it('aborts an in-flight hydration request on unmount', async () => {
    let requestSignal: AbortSignal | undefined
    api.getTranscript.mockImplementation(
      (_companyKey: string, _personId: string, _since: undefined, signal: AbortSignal) => {
        requestSignal = signal
        return new Promise(() => {})
      }
    )

    act(() => {
      root.render(createElement(Harness, { resultBox }))
    })
    expect(requestSignal?.aborted).toBe(false)

    act(() => {
      root.unmount()
    })
    expect(requestSignal?.aborted).toBe(true)
  })
})
