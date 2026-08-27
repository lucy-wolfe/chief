/**
 * Structural browser declarations for the Pi session-event surface. Apps/web
 * intentionally does not import Pi: apps/api relays these payloads verbatim,
 * so every schema is a tolerant reader and future event kinds remain visible.
 */
import { z } from 'zod'

const OpaqueValueSchema = z.unknown()
const OpaqueValuesSchema = z.array(OpaqueValueSchema)

const AgentStartEventSchema = z.object({ type: z.literal('agent_start') }).passthrough()
type AgentStartEvent = z.infer<typeof AgentStartEventSchema>

const AgentEndEventSchema = z
  .object({
    type: z.literal('agent_end'),
    messages: OpaqueValuesSchema,
    willRetry: z.boolean().nullish()
  })
  .passthrough()
type AgentEndEvent = z.infer<typeof AgentEndEventSchema>

const TurnStartEventSchema = z.object({ type: z.literal('turn_start') }).passthrough()
type TurnStartEvent = z.infer<typeof TurnStartEventSchema>

const TurnEndEventSchema = z
  .object({
    type: z.literal('turn_end'),
    message: OpaqueValueSchema,
    toolResults: OpaqueValuesSchema
  })
  .passthrough()
type TurnEndEvent = z.infer<typeof TurnEndEventSchema>

const MessageStartEventSchema = z
  .object({ type: z.literal('message_start'), message: OpaqueValueSchema })
  .passthrough()
type MessageStartEvent = z.infer<typeof MessageStartEventSchema>

const MessageUpdateEventSchema = z
  .object({
    type: z.literal('message_update'),
    message: OpaqueValueSchema,
    assistantMessageEvent: OpaqueValueSchema
  })
  .passthrough()
type MessageUpdateEvent = z.infer<typeof MessageUpdateEventSchema>

const MessageEndEventSchema = z
  .object({ type: z.literal('message_end'), message: OpaqueValueSchema })
  .passthrough()
type MessageEndEvent = z.infer<typeof MessageEndEventSchema>

const ToolExecutionStartEventSchema = z
  .object({
    type: z.literal('tool_execution_start'),
    toolCallId: z.string(),
    toolName: z.string(),
    args: OpaqueValueSchema
  })
  .passthrough()
type ToolExecutionStartEvent = z.infer<typeof ToolExecutionStartEventSchema>

const ToolExecutionUpdateEventSchema = z
  .object({
    type: z.literal('tool_execution_update'),
    toolCallId: z.string(),
    toolName: z.string(),
    args: OpaqueValueSchema,
    partialResult: OpaqueValueSchema
  })
  .passthrough()
type ToolExecutionUpdateEvent = z.infer<typeof ToolExecutionUpdateEventSchema>

const ToolExecutionEndEventSchema = z
  .object({
    type: z.literal('tool_execution_end'),
    toolCallId: z.string(),
    toolName: z.string(),
    result: OpaqueValueSchema,
    isError: z.boolean()
  })
  .passthrough()
type ToolExecutionEndEvent = z.infer<typeof ToolExecutionEndEventSchema>

const AgentSettledEventSchema = z.object({ type: z.literal('agent_settled') }).passthrough()
type AgentSettledEvent = z.infer<typeof AgentSettledEventSchema>

const QueueUpdateEventSchema = z
  .object({
    type: z.literal('queue_update'),
    steering: OpaqueValuesSchema,
    followUp: OpaqueValuesSchema
  })
  .passthrough()
type QueueUpdateEvent = z.infer<typeof QueueUpdateEventSchema>

const CompactionStartEventSchema = z
  .object({
    type: z.literal('compaction_start'),
    reason: z.enum(['manual', 'threshold', 'overflow'])
  })
  .passthrough()
type CompactionStartEvent = z.infer<typeof CompactionStartEventSchema>

const EntryAppendedEventSchema = z
  .object({ type: z.literal('entry_appended'), entry: OpaqueValueSchema })
  .passthrough()
type EntryAppendedEvent = z.infer<typeof EntryAppendedEventSchema>

const SessionInfoChangedEventSchema = z
  .object({ type: z.literal('session_info_changed'), name: z.string().nullish() })
  .passthrough()
type SessionInfoChangedEvent = z.infer<typeof SessionInfoChangedEventSchema>

const ThinkingLevelChangedEventSchema = z
  .object({ type: z.literal('thinking_level_changed'), level: z.string() })
  .passthrough()
type ThinkingLevelChangedEvent = z.infer<typeof ThinkingLevelChangedEventSchema>

const CompactionEndEventSchema = z
  .object({
    type: z.literal('compaction_end'),
    reason: z.enum(['manual', 'threshold', 'overflow']),
    result: OpaqueValueSchema.nullish(),
    aborted: z.boolean(),
    willRetry: z.boolean(),
    errorMessage: z.string().nullish()
  })
  .passthrough()
type CompactionEndEvent = z.infer<typeof CompactionEndEventSchema>

const AutoRetryStartEventSchema = z
  .object({
    type: z.literal('auto_retry_start'),
    attempt: z.number(),
    maxAttempts: z.number(),
    delayMs: z.number(),
    errorMessage: z.string()
  })
  .passthrough()
type AutoRetryStartEvent = z.infer<typeof AutoRetryStartEventSchema>

const AutoRetryEndEventSchema = z
  .object({
    type: z.literal('auto_retry_end'),
    success: z.boolean(),
    attempt: z.number(),
    finalError: z.string().nullish()
  })
  .passthrough()
type AutoRetryEndEvent = z.infer<typeof AutoRetryEndEventSchema>

const UnknownSessionEventSchema = z.object({ type: z.string() }).passthrough()
type UnknownSessionEvent = z.infer<typeof UnknownSessionEventSchema>

export type AgentSessionEvent =
  | AgentStartEvent
  | AgentEndEvent
  | TurnStartEvent
  | TurnEndEvent
  | MessageStartEvent
  | MessageUpdateEvent
  | MessageEndEvent
  | ToolExecutionStartEvent
  | ToolExecutionUpdateEvent
  | ToolExecutionEndEvent
  | AgentSettledEvent
  | QueueUpdateEvent
  | CompactionStartEvent
  | EntryAppendedEvent
  | SessionInfoChangedEvent
  | ThinkingLevelChangedEvent
  | CompactionEndEvent
  | AutoRetryStartEvent
  | AutoRetryEndEvent
  | UnknownSessionEvent

interface ParseSuccess<T> {
  success: true
  data: T
}

interface ParseFailure {
  success: false
}

interface TolerantSchema<T> {
  safeParse(value: unknown): ParseSuccess<T> | ParseFailure
}

function knownOrUnknown<T>(
  schema: TolerantSchema<T>,
  value: unknown,
  fallback: UnknownSessionEvent
): T | UnknownSessionEvent {
  const parsed = schema.safeParse(value)
  return parsed.success ? parsed.data : fallback
}

/** Parse a verbatim Pi event without rejecting fields added by a newer Pi. */
export function parseSessionEvent(value: unknown): AgentSessionEvent | undefined {
  const unknownEvent = UnknownSessionEventSchema.safeParse(value)
  if (!unknownEvent.success) return undefined

  switch (unknownEvent.data.type) {
    case 'agent_start':
      return knownOrUnknown(AgentStartEventSchema, value, unknownEvent.data)
    case 'agent_end':
      return knownOrUnknown(AgentEndEventSchema, value, unknownEvent.data)
    case 'turn_start':
      return knownOrUnknown(TurnStartEventSchema, value, unknownEvent.data)
    case 'turn_end':
      return knownOrUnknown(TurnEndEventSchema, value, unknownEvent.data)
    case 'message_start':
      return knownOrUnknown(MessageStartEventSchema, value, unknownEvent.data)
    case 'message_update':
      return knownOrUnknown(MessageUpdateEventSchema, value, unknownEvent.data)
    case 'message_end':
      return knownOrUnknown(MessageEndEventSchema, value, unknownEvent.data)
    case 'tool_execution_start':
      return knownOrUnknown(ToolExecutionStartEventSchema, value, unknownEvent.data)
    case 'tool_execution_update':
      return knownOrUnknown(ToolExecutionUpdateEventSchema, value, unknownEvent.data)
    case 'tool_execution_end':
      return knownOrUnknown(ToolExecutionEndEventSchema, value, unknownEvent.data)
    case 'agent_settled':
      return knownOrUnknown(AgentSettledEventSchema, value, unknownEvent.data)
    case 'queue_update':
      return knownOrUnknown(QueueUpdateEventSchema, value, unknownEvent.data)
    case 'compaction_start':
      return knownOrUnknown(CompactionStartEventSchema, value, unknownEvent.data)
    case 'entry_appended':
      return knownOrUnknown(EntryAppendedEventSchema, value, unknownEvent.data)
    case 'session_info_changed':
      return knownOrUnknown(SessionInfoChangedEventSchema, value, unknownEvent.data)
    case 'thinking_level_changed':
      return knownOrUnknown(ThinkingLevelChangedEventSchema, value, unknownEvent.data)
    case 'compaction_end':
      return knownOrUnknown(CompactionEndEventSchema, value, unknownEvent.data)
    case 'auto_retry_start':
      return knownOrUnknown(AutoRetryStartEventSchema, value, unknownEvent.data)
    case 'auto_retry_end':
      return knownOrUnknown(AutoRetryEndEventSchema, value, unknownEvent.data)
    default:
      return unknownEvent.data
  }
}
