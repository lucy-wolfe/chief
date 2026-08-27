// @vitest-environment jsdom
import { createFakeSseStreams } from '@test/harness/FakeSseStreams'
import { act, createElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { usePersonStream } from '@/hooks/UsePersonStream'
import { CompanyEventsProvider } from '@/providers/CompanyEventsProvider'
import { activeSseConnectionCount } from '@/services/SseClientService'
import type { PersonStreamSnapshot, SseHubDeps } from '@/types/Sse'

interface SnapshotBox {
  current: PersonStreamSnapshot | undefined
}

function Harness({ snapshotBox }: { snapshotBox: SnapshotBox }): null {
  snapshotBox.current = usePersonStream('acme', 'person-ceo')
  return null
}

/* eslint-disable lucy/no-json-stringify */
// Test fixture wire encoding only; production stream serialization is scoped
// to SseClientService.
function sseFrame(id: string | undefined, event: string, data: unknown): string {
  const idLine = typeof id === 'string' ? `id: ${id}\n` : ''
  return `${idLine}event: ${event}\ndata: ${JSON.stringify(data)}\n\n`
}
/* eslint-enable lucy/no-json-stringify */

async function flushStreamWork(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

describe('usePersonStream', () => {
  let container: HTMLDivElement
  let root: Root
  let snapshotBox: SnapshotBox
  let unmounted: boolean

  beforeEach(() => {
    vi.useFakeTimers()
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
    snapshotBox = { current: undefined }
    unmounted = false
  })

  afterEach(() => {
    if (!unmounted) {
      act(() => {
        root.unmount()
      })
    }
    container.remove()
    vi.useRealTimers()
  })

  it('subscribes once, delivers state/session/host/reactive reorg updates, and closes on unmount', async () => {
    const fake = createFakeSseStreams()
    const deps: SseHubDeps = {
      baseUrl: 'http://fake-api.test',
      accessToken: () => 'fixture-operator-jwt',
      fetchImpl: fake.fetchImpl
    }
    act(() => {
      root.render(
        createElement(CompanyEventsProvider, {
          deps,
          children: createElement(Harness, { snapshotBox })
        })
      )
    })
    expect(activeSseConnectionCount()).toBe(1)
    expect(fake.requests).toHaveLength(1)
    const stream = fake.openNext()
    await act(async () => {
      await flushStreamWork()
    })

    stream.push(sseFrame(undefined, 'state', { phase: 'idle', sessionId: 's-1' }))
    stream.push(sseFrame('1.2', 'session', { type: 'turn_start' }))
    stream.push(sseFrame('1.1', 'session', { type: 'agent_start' }))
    stream.push(sseFrame('1.2', 'session', { type: 'turn_start' }))
    stream.push(sseFrame(undefined, 'host', { state: 'running', pid: 42, exitCode: null }))
    stream.push('event: reorg\ndata: {}\n\n')
    await act(async () => {
      await flushStreamWork()
    })

    const snapshot = snapshotBox.current
    if (typeof snapshot === 'undefined') throw new Error('hook did not expose a snapshot')
    expect(snapshot.channel).toBe('healthy')
    expect(snapshot.session).toMatchObject({ phase: 'idle', sessionId: 's-1' })
    expect(snapshot.events.map((entry) => entry.id)).toEqual(['1.1', '1.2'])
    expect(snapshot.events.map((entry) => entry.event.type)).toEqual(['agent_start', 'turn_start'])
    expect(snapshot.hostState).toBe('running')
    expect(snapshot.host).toMatchObject({ state: 'running', pid: 42, exitCode: null })
    expect(snapshot.reorgCount).toBe(1)

    act(() => {
      root.unmount()
    })
    unmounted = true
    expect(activeSseConnectionCount()).toBe(0)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('offers the append-only agent-pane script with a streamed message and completed tool activity', async () => {
    const fake = createFakeSseStreams()
    const deps: SseHubDeps = {
      baseUrl: 'http://fake-api.test',
      accessToken: () => 'fixture-operator-jwt',
      fetchImpl: fake.fetchImpl
    }
    act(() => {
      root.render(
        createElement(CompanyEventsProvider, {
          deps,
          children: createElement(Harness, { snapshotBox })
        })
      )
    })
    const stream = fake.openNext()
    await act(async () => {
      await flushStreamWork()
    })

    stream.pushScriptedAgentPaneTurn()
    await act(async () => {
      await flushStreamWork()
    })

    const snapshot = snapshotBox.current
    if (typeof snapshot === 'undefined') throw new Error('hook did not expose a snapshot')
    expect(snapshot.hostState).toBe('running')
    expect(snapshot.events.map((entry) => entry.event.type)).toEqual([
      'agent_start',
      'message_start',
      'message_update',
      'tool_execution_start',
      'tool_execution_update',
      'tool_execution_end',
      'message_update',
      'message_end',
      'agent_settled'
    ])

    act(() => {
      root.unmount()
    })
    unmounted = true
    expect(activeSseConnectionCount()).toBe(0)
    expect(vi.getTimerCount()).toBe(0)
  })
})
