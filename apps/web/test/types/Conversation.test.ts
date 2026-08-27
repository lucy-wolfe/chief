import { describe, expect, it } from 'vitest'

import { type ConversationRow, foldSessionEvent, rowsFromTranscript } from '@/types/Conversation'
import type { AgentSessionEvent } from '@/types/SessionEvents'
import type { SessionEventEnvelope } from '@/types/Sse'

function envelope(id: string, event: AgentSessionEvent): SessionEventEnvelope {
  const [generationText, seqText] = id.split('.')
  return {
    id,
    generation: Number(generationText),
    seq: Number(seqText),
    event
  }
}

function message(
  role: 'user' | 'assistant',
  text: string
): {
  role: 'user' | 'assistant'
  content: { type: 'text'; text: string }[]
} {
  return { role, content: [{ type: 'text', text }] }
}

function messageRows(
  rows: readonly ConversationRow[]
): Extract<ConversationRow, { kind: 'message' }>[] {
  return rows.filter(
    (row): row is Extract<ConversationRow, { kind: 'message' }> => row.kind === 'message'
  )
}

describe('Conversation rows', () => {
  it('hydrates transcript message and custom rows without a Pi package dependency', () => {
    const rows = rowsFromTranscript([
      { type: 'message', id: 'u-1', message: message('user', 'Please inspect this.') },
      { type: 'custom_message', id: 'card-1', content: 'ASSIGNMENT 42' },
      { type: 'compaction', id: 'compact-1' }
    ])

    expect(rows).toEqual([
      {
        kind: 'message',
        id: 'entry:u-1',
        role: 'user',
        content: [{ type: 'text', text: 'Please inspect this.' }],
        streaming: false
      },
      {
        kind: 'message',
        id: 'entry:card-1',
        role: 'custom',
        content: [{ type: 'text', text: 'ASSIGNMENT 42' }],
        streaming: false
      },
      { kind: 'activity', id: 'entry:2', label: 'session compacted' }
    ])
  })

  it('replaces a streamed message payload instead of concatenating deltas and finalizes it', () => {
    const started = foldSessionEvent(
      [],
      envelope('1.1', { type: 'message_start', message: message('assistant', 'Hel') })
    )
    const updated = foldSessionEvent(
      started,
      envelope('1.2', {
        type: 'message_update',
        message: message('assistant', 'Hello'),
        assistantMessageEvent: { type: 'text_delta' }
      })
    )
    const finished = foldSessionEvent(
      updated,
      envelope('1.3', { type: 'message_end', message: message('assistant', 'Hello') })
    )

    expect(messageRows(finished)).toEqual([
      {
        kind: 'message',
        id: 'message:1.1',
        role: 'assistant',
        content: [{ type: 'text', text: 'Hello' }],
        streaming: false
      }
    ])
  })

  it('keeps interleaved tool and message events ordered while tool progress changes in place', () => {
    const withMessage = foldSessionEvent(
      [],
      envelope('1.1', { type: 'message_start', message: message('assistant', 'Investigating') })
    )
    const withTool = foldSessionEvent(
      withMessage,
      envelope('1.2', {
        type: 'tool_execution_start',
        toolCallId: 'tool-1',
        toolName: 'read',
        args: { path: 'README.md' }
      })
    )
    const progressed = foldSessionEvent(
      withTool,
      envelope('1.3', {
        type: 'tool_execution_update',
        toolCallId: 'tool-1',
        toolName: 'read',
        args: { path: 'README.md' },
        partialResult: 'first line'
      })
    )
    const complete = foldSessionEvent(
      progressed,
      envelope('1.4', {
        type: 'tool_execution_end',
        toolCallId: 'tool-1',
        toolName: 'read',
        result: 'complete',
        isError: false
      })
    )

    expect(complete).toEqual([
      {
        kind: 'message',
        id: 'message:1.1',
        role: 'assistant',
        content: [{ type: 'text', text: 'Investigating' }],
        streaming: true
      },
      {
        kind: 'tool',
        id: 'tool:tool-1',
        toolCallId: 'tool-1',
        toolName: 'read',
        argsPreview: '{…}',
        state: 'done',
        resultPreview: 'complete'
      }
    ])
  })

  it('adds a stable turn separator once for a completed turn', () => {
    const turn = envelope('1.9', {
      type: 'turn_end',
      message: message('assistant', 'Finished'),
      toolResults: []
    })
    const once = foldSessionEvent([], turn)

    expect(once).toEqual([{ kind: 'turn-break', id: 'turn:1.9' }])
    expect(foldSessionEvent(once, turn)).toBe(once)
  })

  it('keeps known lifecycle events and future event types visible as activity rows', () => {
    const lifecycleEvents: readonly AgentSessionEvent[] = [
      { type: 'agent_start' },
      { type: 'agent_end', messages: [], willRetry: false },
      { type: 'turn_start' },
      { type: 'agent_settled' },
      { type: 'queue_update', steering: ['one'], followUp: ['two'] },
      { type: 'compaction_start', reason: 'manual' },
      {
        type: 'compaction_end',
        reason: 'manual',
        result: undefined,
        aborted: false,
        willRetry: false,
        errorMessage: undefined
      },
      { type: 'auto_retry_start', attempt: 1, maxAttempts: 3, delayMs: 10, errorMessage: 'retry' },
      { type: 'auto_retry_end', success: true, attempt: 1, finalError: undefined },
      { type: 'thinking_level_changed', level: 'high' },
      { type: 'session_info_changed', name: 'Focused session' },
      { type: 'future_pi_event' }
    ]

    const rows = lifecycleEvents.reduce<readonly ConversationRow[]>(
      (current, event, index) => foldSessionEvent(current, envelope(`2.${index + 1}`, event)),
      []
    )

    expect(rows.map((row) => row.kind)).toEqual([
      'activity',
      'activity',
      'activity',
      'activity',
      'activity',
      'activity',
      'activity',
      'activity',
      'activity',
      'activity',
      'activity',
      'activity'
    ])
    expect(rows.map((row) => (row.kind === 'activity' ? row.label : ''))).toContain('agent idle')
    expect(rows.map((row) => (row.kind === 'activity' ? row.label : ''))).toContain(
      'activity: future_pi_event'
    )
  })

  it('makes duplicate envelopes a no-op by their stable event and row identities', () => {
    const unknown = envelope('4.9', { type: 'future_pi_event' })
    const once = foldSessionEvent([], unknown)
    expect(foldSessionEvent(once, unknown)).toBe(once)

    const started = envelope('5.1', { type: 'message_start', message: message('assistant', 'One') })
    const messageOnce = foldSessionEvent([], started)
    expect(foldSessionEvent(messageOnce, started)).toBe(messageOnce)
  })

  it('maps live appended custom entries through the same transcript projection', () => {
    const rows = foldSessionEvent(
      [],
      envelope('6.1', {
        type: 'entry_appended',
        entry: { type: 'custom_message', id: 'extension-card', content: 'mail delivered' }
      })
    )

    expect(rows).toEqual([
      {
        kind: 'message',
        id: 'entry:extension-card',
        role: 'custom',
        content: [{ type: 'text', text: 'mail delivered' }],
        streaming: false
      }
    ])
  })
})
