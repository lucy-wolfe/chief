// What reaches a browser, and what must never.
//
// The translator is a total function, so every rule about the pane stream is
// testable here without a provider, a network, or a running agent. Three of
// these tests are about DROPPING: thinking parts, tool results, and unknown
// events. Each would be a leak or a break if forwarded, and each is the kind of
// thing an "improvement" quietly adds back.
//
// # The frame NAMES are the browser's, and that is a defect scar
//
// This module used to invent its own vocabulary — `event: text`,
// `event: turn-start`, `event: idle` — while the browser's reader
// (`UsePersonStream`, `foldSessionEvent`) has only ever understood three names:
// `host`, `state`, and `session` carrying a Pi-shaped event. So the stream
// delivered perfectly good frames that nothing on the other end could read: an
// agent answered and its pane stayed empty. The turn was real, the reply was in
// the transcript, and the operator saw nothing — the same shape as the missing
// `/api` prefix, one layer down.
//
// So the tests below assert Pi's event NAMES with this program's own sanitised
// PAYLOADS. Asserting a private vocabulary here is what let the two halves each
// agree with themselves.
import { describe, expect, it, vi } from 'vitest'

import { HEARTBEAT_MS, personStreamFrame, streamPerson } from '@/server/PersonStream'

/** An assistant message as the harness emits one: readable words mixed with
 * parts a viewer must not receive. */
function assistantMessage(): Record<string, unknown> {
  return {
    role: 'assistant',
    content: [
      { type: 'text', text: 'On it.' },
      { type: 'thinking', thinking: 'the CEO is lying about' },
      { type: 'toolCall', name: 'bash', input: { command: 'cat ~/.env' } }
    ]
  }
}

describe('personStreamFrame', () => {
  it('carries the assistant’s words under the event name the browser folds', () => {
    expect(
      personStreamFrame({ type: 'message_update', message: { role: 'assistant', content: [] } })
    ).toEqual({ type: 'message_update', message: { role: 'assistant', content: [] } })
  })

  it('never streams the agent’s thinking, or a tool call, as speech', () => {
    // Private reasoning streamed to every viewer of a company pane publishes
    // something the operator never asked to publish; a tool call rendered as
    // content puts a tool's JSON on screen as if the agent had said it. The
    // message survives — only the unreadable parts are removed.
    const frame = personStreamFrame({ type: 'message_end', message: assistantMessage() })

    expect(frame).toEqual({
      type: 'message_end',
      message: { role: 'assistant', content: [{ type: 'text', text: 'On it.' }] }
    })
  })

  it('drops a message event whose message it cannot read', () => {
    // A frame with no message is a frame the fold cannot apply, and inventing
    // an empty one would render as the agent saying nothing.
    expect(personStreamFrame({ type: 'message_start' })).toBeUndefined()
    expect(personStreamFrame({ type: 'message_start', message: 'On it.' })).toBeUndefined()
  })

  it('reports a tool by name and call id, never its arguments or its result', () => {
    // A tool result is arbitrary data — file contents, a transcript, another
    // person's mailbox — and its ARGUMENTS are just as arbitrary. On a pane
    // stream either is a leak dressed up as a progress indicator.
    const started = personStreamFrame({
      type: 'tool_execution_start',
      toolName: 'read',
      toolCallId: 'call-1',
      args: { path: '/root/.env' }
    })
    const ended = personStreamFrame({
      type: 'tool_execution_end',
      toolName: 'read',
      toolCallId: 'call-1',
      isError: false,
      result: { content: 'OPENROUTER_API_KEY=sk-live-secret' }
    })

    expect(started).toEqual({
      type: 'tool_execution_start',
      toolName: 'read',
      toolCallId: 'call-1'
    })
    expect(ended).toEqual({
      type: 'tool_execution_end',
      toolName: 'read',
      toolCallId: 'call-1',
      isError: false
    })
  })

  it('marks a failed tool as failed, and defaults an unstated outcome to success', () => {
    expect(
      personStreamFrame({ type: 'tool_execution_end', toolName: 'bash', isError: true })
    ).toMatchObject({ isError: true })
    expect(personStreamFrame({ type: 'tool_execution_end', toolName: 'bash' })).toMatchObject({
      isError: false
    })
  })

  it('falls back to the tool name when a call carries no id', () => {
    // The browser's fold keys tool rows by `toolCallId`. An absent one would
    // make every call of that tool one row that overwrites itself.
    expect(personStreamFrame({ type: 'tool_execution_start', toolName: 'bash' })).toMatchObject({
      toolCallId: 'bash'
    })
  })

  it('drops a tool event with no name rather than emitting an anonymous one', () => {
    expect(personStreamFrame({ type: 'tool_execution_start' })).toBeUndefined()
    expect(personStreamFrame({ type: 'tool_execution_end', isError: true })).toBeUndefined()
  })

  it('marks the turn’s edges and the agent going idle, carrying nothing else', () => {
    // `turn_end` carries `message` and `toolResults` on the harness's own event,
    // and `agent_end` carries `messages` — the WHOLE conversation. The browser's
    // fold reads none of them, and each is exactly the payload this module
    // exists to keep off the wire.
    expect(personStreamFrame({ type: 'agent_start' })).toEqual({ type: 'agent_start' })
    expect(personStreamFrame({ type: 'turn_start' })).toEqual({ type: 'turn_start' })
    expect(
      personStreamFrame({ type: 'turn_end', message: assistantMessage(), toolResults: ['secret'] })
    ).toEqual({ type: 'turn_end' })
    expect(personStreamFrame({ type: 'agent_end', messages: ['everything'] })).toEqual({
      type: 'agent_end'
    })
  })

  it('drops an event it does not recognize instead of forwarding it', () => {
    // No default pass-through. The provider-request event carries the WHOLE
    // conversation and the request headers; forwarding an unknown event by
    // default is how that reaches a browser.
    expect(
      personStreamFrame({ type: 'before_provider_request', payload: { messages: ['everything'] } })
    ).toBeUndefined()
    expect(personStreamFrame({ type: 'session_tree' })).toBeUndefined()
    expect(personStreamFrame(undefined)).toBeUndefined()
    expect(personStreamFrame('turn_start')).toBeUndefined()
    expect(personStreamFrame(['turn_start'])).toBeUndefined()
  })
})

/** Read whatever the stream has produced so far, as text. */
async function drain(stream: ReadableStream<Uint8Array>, chunks: number): Promise<string> {
  const reader = stream.getReader()
  const decoder = new TextDecoder()
  let text = ''
  for (let index = 0; index < chunks; index += 1) {
    const chunk = await reader.read()
    if (chunk.done) break
    text += decoder.decode(chunk.value)
  }
  await reader.cancel()
  return text
}

/** A stream plus the handle that feeds it, so a test can emit harness events. */
function connected(): {
  stream: ReadableStream<Uint8Array>
  emit(event: unknown): void
} {
  let listener: ((event: unknown) => void) | undefined
  const stream = streamPerson({
    subscribe: (incoming) => {
      listener = incoming
      return () => undefined
    }
  })
  return {
    stream,
    emit: (event: unknown): void => listener?.(event)
  }
}

describe('streamPerson', () => {
  it('greets with a host frame before anything else', async () => {
    // The pane uses the greeting to decide whether there is a transcript to
    // fetch at all (`agentIsLive`). Without it the pane never hydrates — the
    // other half of why a live agent looked like an empty one. The route only
    // reaches here for a person the roster is already hosting, so `running` is
    // the honest greeting.
    const { stream } = connected()

    const text = await drain(stream, 1)

    expect(text).toContain('event: host')
    expect(text).toContain('"state":"running"')
    expect(text).toContain('event: state')
  })

  it('writes a session frame per translated event, with a replay cursor', async () => {
    // The cursor is `<generation>.<seq>`, and it is not decoration: a frame
    // without one is dropped by `parseSessionCursor`, so the browser would
    // receive the agent's words and discard them.
    const { stream, emit } = connected()

    emit({ type: 'turn_start' })
    emit({ type: 'message_update', message: { role: 'assistant', content: [] } })

    const text = await drain(stream, 3)

    expect(text).toContain('id: 1.1\nevent: session')
    expect(text).toContain('id: 1.2\nevent: session')
    expect(text).toContain('"type":"turn_start"')
    expect(text).toContain('"type":"message_update"')
  })

  it('does not advance the cursor for an event it dropped', async () => {
    // A sequence number burned on a frame nobody sent would make the browser's
    // replay logic see a gap and treat a healthy stream as a lossy one.
    const { stream, emit } = connected()

    emit({ type: 'before_provider_request', payload: { messages: ['everything'] } })
    emit({ type: 'turn_start' })

    expect(await drain(stream, 2)).toContain('id: 1.1\n')
  })

  it('escapes a newline rather than truncating the agent mid-sentence', async () => {
    // A newline is the SSE frame separator. An unescaped one ends the frame
    // early, and the browser shows half a reply as if that were all the agent
    // said. Serializing the payload is what keeps that impossible.
    const { stream, emit } = connected()

    emit({
      type: 'message_update',
      message: { role: 'assistant', content: [{ type: 'text', text: 'line one\nline two' }] }
    })

    const text = await drain(stream, 2)
    const frame = text.slice(text.indexOf('id: 1.1'))

    expect(frame).toContain('line one\\nline two')
    // One frame, not two: exactly one blank-line terminator after the data.
    expect(frame.split('\n\n').filter((part) => part.length > 0)).toHaveLength(1)
  })

  it('beats often enough that a thinking agent does not look dead', async () => {
    // The last SSE stream in this program died because its heartbeat was 15s
    // against a 10s server idle timeout: the stream was healthy and the
    // connection was closed under it, which looks exactly like a hung agent.
    vi.useFakeTimers()
    try {
      const stream = streamPerson({ subscribe: () => () => undefined })
      vi.advanceTimersByTime(HEARTBEAT_MS)
      const reader = stream.getReader()
      const decoder = new TextDecoder()
      // The greeting first, then the beat.
      await reader.read()
      const beat = await reader.read()
      expect(decoder.decode(beat.value)).toBe(': beat\n\n')
      await reader.cancel()
    } finally {
      vi.useRealTimers()
    }
  })

  it('releases the subscription when the client goes away', async () => {
    // A listener left on a long-lived harness accumulates one per page load
    // until the process is holding every viewer who ever opened the pane.
    const unsubscribe = vi.fn()
    const stream = streamPerson({ subscribe: () => unsubscribe })

    await stream.cancel()

    expect(unsubscribe).toHaveBeenCalledTimes(1)
  })

  it('stops beating when the client goes away', async () => {
    vi.useFakeTimers()
    try {
      const stream = streamPerson({ subscribe: () => () => undefined })
      await stream.cancel()
      // An interval left running enqueues into a closed controller forever,
      // which throws on a timer nobody is watching.
      expect(() => vi.advanceTimersByTime(HEARTBEAT_MS * 3)).not.toThrow()
    } finally {
      vi.useRealTimers()
    }
  })
})
