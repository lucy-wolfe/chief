/** Controlled streaming-fetch fixture for the web SSE client tests. */
import type { FetchImpl } from '@/types/Fetch'

interface RecordedSseRequest {
  method: string
  url: string
  headers: Record<string, string>
  body: BodyInit | null | undefined
}

interface PendingFetch {
  resolve: (response: Response) => void
  reject: (error: Error) => void
  settled: boolean
  signal: AbortSignal | null
  url: string
}

/* eslint-disable lucy/no-json-stringify */
// Test-only script framing mirrors the browser's app-API SSE wire format.
function scriptedFrame(id: string | undefined, event: string, data: unknown): string {
  const idLine = typeof id === 'string' ? `id: ${id}\n` : ''
  return `${idLine}event: ${event}\ndata: ${JSON.stringify(data)}\n\n`
}
/* eslint-enable lucy/no-json-stringify */

function scriptedAgentPaneTurn(): readonly string[] {
  return [
    scriptedFrame(undefined, 'host', { state: 'running', pid: 811 }),
    scriptedFrame(undefined, 'state', {
      isStreaming: true,
      pendingMessageCount: 0,
      thinkingLevel: 'medium'
    }),
    scriptedFrame('1.1', 'session', { type: 'agent_start' }),
    scriptedFrame('1.2', 'session', {
      type: 'message_start',
      message: { role: 'assistant', content: [{ type: 'text', text: 'I am checking' }] }
    }),
    scriptedFrame('1.3', 'session', {
      type: 'message_update',
      message: { role: 'assistant', content: [{ type: 'text', text: 'I am checking the plan…' }] },
      assistantMessageEvent: { type: 'text_delta' }
    }),
    scriptedFrame('1.4', 'session', {
      type: 'tool_execution_start',
      toolCallId: 'fixture-read',
      toolName: 'read',
      args: { path: 'docs/example-plan.md' }
    }),
    scriptedFrame('1.5', 'session', {
      type: 'tool_execution_update',
      toolCallId: 'fixture-read',
      toolName: 'read',
      args: { path: 'docs/example-plan.md' },
      partialResult: 'Checklist found.'
    }),
    scriptedFrame('1.6', 'session', {
      type: 'tool_execution_end',
      toolCallId: 'fixture-read',
      toolName: 'read',
      result: 'Checklist found.',
      isError: false
    }),
    scriptedFrame('1.7', 'session', {
      type: 'message_update',
      message: {
        role: 'assistant',
        content: [{ type: 'text', text: 'I checked the plan and the next step is ready.' }]
      },
      assistantMessageEvent: { type: 'text_delta' }
    }),
    scriptedFrame('1.8', 'session', {
      type: 'message_end',
      message: {
        role: 'assistant',
        content: [{ type: 'text', text: 'I checked the plan and the next step is ready.' }]
      }
    }),
    scriptedFrame('1.9', 'session', { type: 'agent_settled' })
  ]
}

class FakeSseConnection {
  private controller: ReadableStreamDefaultController<Uint8Array> | undefined
  private closed = false
  private readonly stream = new ReadableStream<Uint8Array>({
    start: (controller) => {
      this.controller = controller
    }
  })

  response(): Response {
    return new Response(this.stream, {
      headers: { 'content-type': 'text/event-stream' }
    })
  }

  push(text: string): void {
    if (this.closed || !this.controller) return
    this.controller.enqueue(new TextEncoder().encode(text))
  }

  pushScriptedAgentPaneTurn(): void {
    for (const frame of scriptedAgentPaneTurn()) this.push(frame)
  }

  /** apps/api's own open protocol for `…/people/:personId/stream`: the stream
   * greets every caller with EXACTLY ONE frame before anything else — a
   * `state` snapshot carrying the live `RpcSessionState` when
   * `agentHost.state()` has a child, or a `host` frame with `state:"stopped"`
   * when it does not (`StreamService.agentEventStream`). The pane uses that
   * greeting to decide whether a transcript exists to fetch at all, so a test
   * that drives a pane has to send one. */
  pushLiveAgentGreeting(): void {
    this.push(
      scriptedFrame(undefined, 'state', {
        isStreaming: false,
        pendingMessageCount: 0,
        thinkingLevel: 'medium'
      })
    )
  }

  pushDormantAgentGreeting(): void {
    this.push(scriptedFrame(undefined, 'host', { state: 'stopped', pid: null, exitCode: null }))
  }

  close(): void {
    if (this.closed || !this.controller) return
    this.closed = true
    this.controller.close()
  }

  error(message = 'stream failed'): void {
    if (this.closed || !this.controller) return
    this.closed = true
    this.controller.error(new Error(message))
  }
}

function requestUrl(input: RequestInfo | URL): string {
  if (typeof input === 'string') return input
  if (input instanceof URL) return input.toString()
  return input.url
}

/**
 * A scripted `fetch` seam. Requests wait until `openNext()`/`failNext()` so
 * tests can exercise connect budgets, heartbeats, drops, and replays without
 * timers that represent application polling.
 */
export function createFakeSseStreams(): {
  fetchImpl: FetchImpl
  requests: RecordedSseRequest[]
  pendingCount(): number
  openNext(): FakeSseConnection
  openMatching(predicate: (url: string) => boolean): FakeSseConnection
  failNext(message?: string): void
} {
  const requests: RecordedSseRequest[] = []
  const pending: PendingFetch[] = []

  function takePending(): PendingFetch {
    for (;;) {
      const next = pending.shift()
      if (!next) throw new Error('expected a pending SSE fetch')
      if (!next.settled) return next
    }
  }

  function open(pendingFetch: PendingFetch): FakeSseConnection {
    pendingFetch.settled = true
    const connection = new FakeSseConnection()
    pendingFetch.resolve(connection.response())
    if (pendingFetch.signal) {
      pendingFetch.signal.addEventListener('abort', () => connection.error('aborted'), {
        once: true
      })
    }
    return connection
  }

  const fetchImpl: FetchImpl = (input, init) => {
    const headers: Record<string, string> = {}
    new Headers(init?.headers).forEach((value, key) => {
      headers[key] = value
    })
    const url = requestUrl(input)
    requests.push({
      method: init?.method ?? 'GET',
      url,
      headers,
      body: init?.body
    })
    return new Promise<Response>((resolve, reject) => {
      const pendingFetch: PendingFetch = {
        resolve,
        reject: (error) => reject(error),
        settled: false,
        signal: init?.signal ?? null,
        url
      }
      const abort = (): void => {
        if (pendingFetch.settled) return
        pendingFetch.settled = true
        pendingFetch.reject(new DOMException('aborted', 'AbortError'))
      }
      if (pendingFetch.signal?.aborted) {
        abort()
      } else {
        pendingFetch.signal?.addEventListener('abort', abort, { once: true })
      }
      pending.push(pendingFetch)
    })
  }

  return {
    fetchImpl,
    requests,
    pendingCount: () => pending.filter((entry) => !entry.settled).length,
    openNext: () => open(takePending()),
    openMatching: (predicate) => {
      const index = pending.findIndex((entry) => !entry.settled && predicate(entry.url))
      if (index === -1) throw new Error('expected a pending SSE fetch matching predicate')
      const [pendingFetch] = pending.splice(index, 1)
      if (!pendingFetch) throw new Error('expected a pending SSE fetch matching predicate')
      return open(pendingFetch)
    },
    failNext: (message = 'fetch failed') => {
      const pendingFetch = takePending()
      pendingFetch.settled = true
      pendingFetch.reject(new Error(message))
    }
  }
}
