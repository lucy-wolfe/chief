/**
 * Structural conversation rows for the browser pane. Pi's session objects
 * remain opaque at the API boundary; this module narrows only the fields the
 * renderer needs and leaves unfamiliar event kinds visible as activity rows.
 */
import { z } from 'zod'

import type { ChiefApiErrorShape } from '@/types/ApiErrors'
import type { SayMode } from '@/types/ChiefApi'
import type { PaneDescriptor } from '@/types/PaneLayout'
import type {
  PersonHostEvent,
  PersonHostState,
  PersonSessionState,
  SessionEventEnvelope,
  SseChannelState
} from '@/types/Sse'

export type ConversationRole = 'user' | 'assistant' | 'custom'

/** A tolerant, display-safe projection of a Pi content block. */
interface ContentBlock {
  type: string
  text?: string
}

export type ConversationRow =
  | {
      kind: 'message'
      id: string
      role: ConversationRole
      content: readonly ContentBlock[]
      streaming: boolean
    }
  | {
      kind: 'tool'
      id: string
      toolCallId: string
      toolName: string
      argsPreview: string
      state: 'running' | 'done' | 'error'
      resultPreview?: string
    }
  | { kind: 'turn-break'; id: string }
  | { kind: 'activity'; id: string; label: string }

/** Live facts derived only from S4's event/state stream. */
export interface AgentConversationRuntime {
  isCompacting: boolean
  isRetrying: boolean
  isSettled: boolean
  queuedMessages: number
}

/** The complete browser-facing result for the agent pane's one data hook. */
export interface AgentConversationResult {
  rows: readonly ConversationRow[]
  session: PersonSessionState | undefined
  host: PersonHostEvent | undefined
  hostState: PersonHostState | undefined
  channel: SseChannelState
  hydrating: boolean
  runtime: AgentConversationRuntime
  paneError: ChiefApiErrorShape | undefined
  send(message: string, mode: SayMode): Promise<void>
  abort(): Promise<void>
}

/** The S3 PaneGrid seam's concrete person-pane implementation contract. */
export interface AgentPaneProps {
  companyKey: string
  pane: PaneDescriptor
  readOnly: boolean
}

interface UnknownRecord {
  readonly [key: string]: unknown
}

const UnknownRecordSchema = z.object({}).passthrough()

function recordOf(value: unknown): UnknownRecord | undefined {
  const parsed = UnknownRecordSchema.safeParse(value)
  return parsed.success ? parsed.data : undefined
}

function stringField(value: unknown, field: string): string | undefined {
  const record = recordOf(value)
  const candidate = record?.[field]
  return typeof candidate === 'string' ? candidate : undefined
}

function numberField(value: unknown, field: string): number | undefined {
  const record = recordOf(value)
  const candidate = record?.[field]
  return typeof candidate === 'number' ? candidate : undefined
}

function booleanField(value: unknown, field: string): boolean {
  const record = recordOf(value)
  return record?.[field] === true
}

function valueField(value: unknown, field: string): unknown {
  return recordOf(value)?.[field]
}

function arrayLength(value: unknown, field: string): number {
  const candidate = valueField(value, field)
  return Array.isArray(candidate) ? candidate.length : 0
}

function conversationRole(value: unknown): ConversationRole {
  const role = stringField(value, 'role')
  switch (role) {
    case 'user':
      return 'user'
    case 'assistant':
      return 'assistant'
    case undefined:
    default:
      return 'custom'
  }
}

/** Turn an opaque Pi content value into display-safe structural blocks. */
function contentBlocks(value: unknown): readonly ContentBlock[] {
  if (typeof value === 'string') return [{ type: 'text', text: value }]
  if (!Array.isArray(value)) return []

  const blocks: ContentBlock[] = []
  for (const item of value) {
    const record = recordOf(item)
    if (!record) continue
    const type = stringField(record, 'type') ?? 'unknown'
    const text = stringField(record, 'text') ?? stringField(record, 'thinking')
    if (typeof text === 'string') {
      blocks.push({ type, text })
    } else if (type === 'toolCall') {
      const name = stringField(record, 'name') ?? 'tool'
      blocks.push({ type, text: `[tool call: ${name}]` })
    } else {
      blocks.push({ type })
    }
  }
  return blocks
}

/** Compact, non-serializing text for tool arguments and results. */
function previewValue(value: unknown): string {
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  if (typeof value === 'undefined') return ''
  if (Array.isArray(value)) return `[${value.length} values]`
  const record = recordOf(value)
  if (record) {
    const name = stringField(record, 'name')
    return typeof name === 'string' ? `{${name}}` : '{…}'
  }
  return String(value)
}

/** Text the presentational components render for a tolerant content block. */
export function contentBlockText(block: ContentBlock): string {
  return block.text ?? `[${block.type}]`
}

function sameContent(left: readonly ContentBlock[], right: readonly ContentBlock[]): boolean {
  if (left.length !== right.length) return false
  return left.every((block, index) => {
    const candidate = right[index]
    return block.type === candidate?.type && block.text === candidate.text
  })
}

function messageRow(
  id: string,
  role: ConversationRole,
  content: readonly ContentBlock[],
  streaming: boolean
): Extract<ConversationRow, { kind: 'message' }> {
  return { kind: 'message', id, role, content, streaming }
}

function lastOpenMessageIndex(rows: readonly ConversationRow[], role: ConversationRole): number {
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    const row = rows[index]
    if (row?.kind === 'message' && row.role === role && row.streaming) return index
  }
  return -1
}

function rowWithMessage(
  rows: readonly ConversationRow[],
  event: SessionEventEnvelope,
  message: unknown,
  phase: 'start' | 'update' | 'end'
): readonly ConversationRow[] {
  const role = conversationRole(message)
  const content = contentBlocks(recordOf(message)?.content)
  const eventRowId = `message:${event.id}`
  const openIndex = lastOpenMessageIndex(rows, role)

  if (phase === 'start') {
    if (openIndex >= 0) {
      const current = rows[openIndex]
      if (
        current?.kind === 'message' &&
        current.streaming &&
        sameContent(current.content, content)
      ) {
        return rows
      }
    }
    return [...rows, messageRow(eventRowId, role, content, true)]
  }

  if (openIndex >= 0) {
    const current = rows[openIndex]
    if (current?.kind === 'message') {
      const streaming = phase !== 'end'
      if (current.streaming === streaming && sameContent(current.content, content)) return rows
      const next = [...rows]
      next[openIndex] = messageRow(current.id, role, content, streaming)
      return next
    }
  }

  const existing = rows.find((row) => row.kind === 'message' && row.id === eventRowId)
  if (
    existing?.kind === 'message' &&
    existing.streaming === (phase !== 'end') &&
    sameContent(existing.content, content)
  ) {
    return rows
  }
  return [...rows, messageRow(eventRowId, role, content, phase !== 'end')]
}

function toolRowIndex(rows: readonly ConversationRow[], toolCallId: string): number {
  return rows.findIndex((row) => row.kind === 'tool' && row.toolCallId === toolCallId)
}

function withToolRow(
  rows: readonly ConversationRow[],
  input: {
    toolCallId: string
    toolName: string
    args: unknown
    state: 'running' | 'done' | 'error'
    result?: unknown
  }
): readonly ConversationRow[] {
  const index = toolRowIndex(rows, input.toolCallId)
  const previous = index >= 0 ? rows[index] : undefined
  const row: Extract<ConversationRow, { kind: 'tool' }> = {
    kind: 'tool',
    id: `tool:${input.toolCallId}`,
    toolCallId: input.toolCallId,
    toolName: input.toolName,
    argsPreview:
      previous?.kind === 'tool' && previous.argsPreview.length > 0
        ? previous.argsPreview
        : previewValue(input.args),
    state: input.state,
    resultPreview: typeof input.result === 'undefined' ? undefined : previewValue(input.result)
  }

  if (
    previous?.kind === 'tool' &&
    previous.toolName === row.toolName &&
    previous.argsPreview === row.argsPreview &&
    previous.state === row.state &&
    previous.resultPreview === row.resultPreview
  ) {
    return rows
  }
  if (index >= 0) {
    const next = [...rows]
    next[index] = row
    return next
  }
  return [...rows, row]
}

function activityRow(
  rows: readonly ConversationRow[],
  event: SessionEventEnvelope,
  label: string
): readonly ConversationRow[] {
  const id = `activity:${event.id}`
  if (rows.some((row) => row.id === id)) return rows
  return [...rows, { kind: 'activity', id, label }]
}

function transcriptMessageRow(entry: UnknownRecord, index: number): ConversationRow | undefined {
  const message = entry.message
  const id = stringField(entry, 'id') ?? `entry-${index}`
  if (typeof message === 'undefined') return undefined
  return messageRow(
    `entry:${id}`,
    conversationRole(message),
    contentBlocks(recordOf(message)?.content),
    false
  )
}

function customTranscriptRow(entry: UnknownRecord, index: number): ConversationRow {
  const id = stringField(entry, 'id') ?? `entry-${index}`
  const content = contentBlocks(entry.content ?? entry.data)
  const customType = stringField(entry, 'customType')
  const fallback = typeof customType === 'string' ? `[${customType}]` : '[custom session entry]'
  return messageRow(
    `entry:${id}`,
    'custom',
    content.length > 0 ? content : [{ type: 'text', text: fallback }],
    false
  )
}

/** Project the hydrate route's opaque session entries into ordered rows. */
export function rowsFromTranscript(entries: readonly unknown[]): readonly ConversationRow[] {
  const rows: ConversationRow[] = []
  for (let index = 0; index < entries.length; index += 1) {
    const entry = recordOf(entries[index])
    if (!entry) continue
    switch (stringField(entry, 'type')) {
      case 'message': {
        const row = transcriptMessageRow(entry, index)
        if (row) rows.push(row)
        break
      }
      case 'custom':
      case 'custom_message':
        rows.push(customTranscriptRow(entry, index))
        break
      case 'compaction':
        rows.push({ kind: 'activity', id: `entry:${index}`, label: 'session compacted' })
        break
      case 'model_change':
        rows.push({ kind: 'activity', id: `entry:${index}`, label: 'model changed' })
        break
      case 'thinking_level_change':
        rows.push({ kind: 'activity', id: `entry:${index}`, label: 'thinking level changed' })
        break
      case undefined:
      default:
        break
    }
  }
  return rows
}

/**
 * Fold one cursor-bearing session event into rows. Updates replace the full
 * message payload supplied by Pi; no browser-side delta concatenation occurs.
 */
export function foldSessionEvent(
  rows: readonly ConversationRow[],
  envelope: SessionEventEnvelope
): readonly ConversationRow[] {
  const event = envelope.event
  switch (event.type) {
    case 'message_start':
      return rowWithMessage(rows, envelope, valueField(event, 'message'), 'start')
    case 'message_update':
      return rowWithMessage(rows, envelope, valueField(event, 'message'), 'update')
    case 'message_end':
      return rowWithMessage(rows, envelope, valueField(event, 'message'), 'end')
    case 'tool_execution_start': {
      const toolCallId = stringField(event, 'toolCallId') ?? `unknown-${envelope.id}`
      return withToolRow(rows, {
        toolCallId,
        toolName: stringField(event, 'toolName') ?? 'tool',
        args: valueField(event, 'args'),
        state: 'running'
      })
    }
    case 'tool_execution_update': {
      const toolCallId = stringField(event, 'toolCallId') ?? `unknown-${envelope.id}`
      return withToolRow(rows, {
        toolCallId,
        toolName: stringField(event, 'toolName') ?? 'tool',
        args: valueField(event, 'args'),
        state: 'running',
        result: valueField(event, 'partialResult')
      })
    }
    case 'tool_execution_end': {
      const toolCallId = stringField(event, 'toolCallId') ?? `unknown-${envelope.id}`
      return withToolRow(rows, {
        toolCallId,
        toolName: stringField(event, 'toolName') ?? 'tool',
        args: undefined,
        state: booleanField(event, 'isError') ? 'error' : 'done',
        result: valueField(event, 'result')
      })
    }
    case 'turn_end': {
      const id = `turn:${envelope.id}`
      if (rows.some((row) => row.id === id)) return rows
      return [...rows, { kind: 'turn-break', id }]
    }
    case 'entry_appended': {
      const appended = rowsFromTranscript([valueField(event, 'entry')])
      const unseen = appended.filter((row) => !rows.some((existing) => existing.id === row.id))
      return unseen.length === 0 ? rows : [...rows, ...unseen]
    }
    case 'agent_settled':
      return activityRow(rows, envelope, 'agent idle')
    case 'queue_update':
      return activityRow(
        rows,
        envelope,
        `queued messages: ${arrayLength(event, 'steering') + arrayLength(event, 'followUp')}`
      )
    case 'compaction_start':
      return activityRow(rows, envelope, 'compacting session…')
    case 'compaction_end':
      return activityRow(rows, envelope, 'session compaction finished')
    case 'auto_retry_start':
      return activityRow(
        rows,
        envelope,
        `retrying (${numberField(event, 'attempt') ?? 0}/${numberField(event, 'maxAttempts') ?? 0})`
      )
    case 'auto_retry_end':
      return activityRow(rows, envelope, event.success ? 'retry recovered' : 'retry stopped')
    case 'thinking_level_changed':
      return activityRow(
        rows,
        envelope,
        `thinking level: ${stringField(event, 'level') ?? 'changed'}`
      )
    case 'session_info_changed':
      return activityRow(rows, envelope, 'session information changed')
    default:
      return activityRow(rows, envelope, `activity: ${event.type}`)
  }
}
