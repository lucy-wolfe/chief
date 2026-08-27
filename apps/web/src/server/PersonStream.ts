/**
 * One person's turn, as it happens.
 *
 * # A narrow payload, in the browser's own vocabulary
 *
 * The harness emits a large event union — provider payloads, session-tree
 * writes, resource updates, context builds. This forwards a SMALL part of it
 * and drops the rest.
 *
 * Dropping is the design, not laziness. Forwarding the union verbatim would
 * put the provider request payload on the wire, which carries the full
 * conversation and the request headers, and it would put tool RESULTS —
 * arbitrary data: file contents, a whole transcript, another person's mailbox
 * — on an unauthenticated pane stream, which is a leak dressed up as a
 * progress indicator.
 *
 * # Why the frames are named the way they are
 *
 * They were not always. This module used to invent its own vocabulary —
 * `event: text`, `event: turn-start`, `event: idle` — while the browser's
 * reader (`UsePersonStream`, `foldSessionEvent`) has only ever understood
 * three names: `host`, `state`, and `session` carrying a Pi-shaped event. So
 * the stream delivered perfectly good frames that nothing on the other end
 * could read: **an agent answered and its pane stayed empty.** The turn was
 * real, the reply was in the transcript, and the operator saw nothing.
 *
 * That is the same defect as the missing `/api` prefix, one layer down: two
 * halves that each agreed with themselves. So the frames are the ones the
 * browser reads, and the PAYLOAD is still this module's — a sanitised,
 * Pi-shaped event with the parts a reader must not receive removed.
 *
 * # The greeting
 *
 * The stream sends exactly one `host` frame before anything else. The pane
 * uses it to decide whether there is a transcript to fetch at all
 * (`agentIsLive`), and without it the pane never hydrates — which is the other
 * half of why a live agent looked like an empty one. The route only reaches
 * here for a person the roster is already hosting, so `running` is the honest
 * greeting.
 */
import type { PersonStreamFrame } from '@/types/PersonStream'
import { isNullish } from '@/utils/Nullish'

/** How often a comment frame goes out on an idle stream.
 *
 * Shorter than any proxy idle timeout worth worrying about. The last SSE
 * stream in this program died because its heartbeat was 15s against a 10s
 * server idle timeout — the stream was healthy and the connection was closed
 * under it, which looks exactly like a hung agent. */
export const HEARTBEAT_MS = 5000

function record(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== 'object' || isNullish(value) || Array.isArray(value)) return undefined
  return Object.fromEntries(Object.entries(value))
}

function stringField(value: Record<string, unknown> | undefined, name: string): string | undefined {
  const field = value?.[name]
  return typeof field === 'string' ? field : undefined
}

/** An assistant message reduced to the words a reader can read.
 *
 * Text parts only. Thinking blocks are the agent's private reasoning and tool
 * calls are not speech; forwarding either would publish something the operator
 * never asked to publish, or render a tool's JSON as if the agent had said
 * it. */
function readableMessage(value: unknown): Record<string, unknown> | undefined {
  const message = record(value)
  if (isNullish(message)) return undefined
  const content = message.content
  if (!Array.isArray(content)) return { role: message.role, content: [] }
  const text = content
    .map((part) => {
      const entry = record(part)
      return entry?.type === 'text' && typeof entry.text === 'string' ? entry.text : undefined
    })
    .filter((part): part is string => !isNullish(part))
  return { role: message.role, content: text.map((value) => ({ type: 'text', text: value })) }
}

/**
 * One harness event as one browser frame, or nothing.
 *
 * Total and pure: every event either maps to a frame this program has defined
 * or is dropped. There is no default case that forwards an unknown event — a
 * frame the browser cannot render is worse than no frame, and an unrecognized
 * event carrying provider internals is worse than both.
 */
export function personStreamFrame(event: unknown): PersonStreamFrame | undefined {
  const value = record(event)
  const type = stringField(value, 'type')
  if (isNullish(type)) return undefined

  switch (type) {
    case 'agent_start':
    case 'turn_start':
      return { type }
    case 'turn_end':
      // WITHOUT `message` and `toolResults`. The browser's fold reads neither
      // on this event, and both carry exactly the payloads this module exists
      // to keep off the wire.
      return { type }
    case 'message_start':
    case 'message_update':
    case 'message_end': {
      const message = readableMessage(value?.message)
      return isNullish(message) ? undefined : { type, message }
    }
    case 'tool_execution_start': {
      const toolName = stringField(value, 'toolName')
      if (isNullish(toolName)) return undefined
      // The NAME and the call id, never the ARGS: a tool's arguments are as
      // arbitrary as its result.
      return { type, toolName, toolCallId: stringField(value, 'toolCallId') ?? toolName }
    }
    case 'tool_execution_end': {
      const toolName = stringField(value, 'toolName')
      if (isNullish(toolName)) return undefined
      return {
        type,
        toolName,
        toolCallId: stringField(value, 'toolCallId') ?? toolName,
        isError: value?.isError === true
      }
    }
    case 'agent_end':
      // WITHOUT `messages`, which is the entire conversation.
      return { type }
    default:
      return undefined
  }
}

/* eslint-disable lucy/no-json-stringify */
// SSE wire payloads for this app's own person channel — the same seam
// `CompanyLifecycle` and `SseClientService` take for their own frames.

/** One `session` frame, with the replay cursor the browser's reader needs.
 *
 * The cursor is `<generation>.<seq>`. A frame without one is dropped by
 * `parseSessionCursor`, so the sequence is not decoration. */
function sessionFrame(frame: PersonStreamFrame, generation: number, seq: number): string {
  return `id: ${generation}.${seq}\nevent: session\ndata: ${JSON.stringify(frame)}\n\n`
}

/** The one greeting, sent before anything else. */
function greeting(): string {
  return (
    `event: host\ndata: ${JSON.stringify({ state: 'running' })}\n\n` +
    `event: state\ndata: ${JSON.stringify({ isStreaming: false, pendingMessageCount: 0 })}\n\n`
  )
}
/* eslint-enable lucy/no-json-stringify */

/**
 * An SSE body carrying one person's turn.
 *
 * The subscription is released when the client goes away — `cancel` fires on
 * disconnect, and a listener left attached to a long-lived harness accumulates
 * one per page load until the process is holding every viewer who ever opened
 * the pane.
 */
export function streamPerson(source: {
  subscribe(listener: (event: unknown) => void): () => void
}): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder()
  let unsubscribe: (() => void) | undefined
  let heartbeat: ReturnType<typeof setInterval> | undefined
  // One connection is one generation: the browser's cursor logic treats a
  // generation change as a reorg and re-hydrates, which is exactly right for a
  // reconnect and wrong for two frames of the same stream.
  const generation = 1
  let seq = 0

  return new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(encoder.encode(greeting()))
      unsubscribe = source.subscribe((event) => {
        const frame = personStreamFrame(event)
        if (isNullish(frame)) return
        seq += 1
        controller.enqueue(encoder.encode(sessionFrame(frame, generation, seq)))
      })
      // A comment frame: the browser ignores it, every proxy in between counts
      // it as traffic. An agent that thinks for two minutes must not look dead.
      heartbeat = setInterval(() => controller.enqueue(encoder.encode(': beat\n\n')), HEARTBEAT_MS)
    },
    cancel() {
      unsubscribe?.()
      if (!isNullish(heartbeat)) clearInterval(heartbeat)
    }
  })
}
