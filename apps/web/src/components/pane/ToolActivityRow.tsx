'use client'

import { type ReactElement, useState } from 'react'

import type { ConversationRow } from '@/types/Conversation'

interface ToolActivityRowProps {
  row: Extract<ConversationRow, { kind: 'tool' }>
}

function stateMark(state: 'running' | 'done' | 'error'): string {
  switch (state) {
    case 'running':
      return '…'
    case 'done':
      return '✓'
    case 'error':
      return '×'
  }
}

/** Tool execution with stable running/done/error state and a result disclosure. */
export function ToolActivityRow({ row }: ToolActivityRowProps): ReactElement {
  const [expanded, setExpanded] = useState(false)
  const hasResult = typeof row.resultPreview === 'string' && row.resultPreview.length > 0

  return (
    <section
      aria-label={`Tool ${row.toolName} ${row.state}`}
      data-tool-state={row.state}
      style={{ borderLeft: '2px solid var(--chief-pane-border)', padding: '2px 6px' }}
    >
      <span aria-hidden="true">{stateMark(row.state)} </span>
      <strong>{row.toolName}</strong>
      {row.argsPreview.length > 0 ? <span> {row.argsPreview}</span> : null}
      {hasResult ? (
        <button type="button" onClick={(): void => setExpanded((current) => !current)}>
          {expanded ? 'Hide result' : 'Show result'}
        </button>
      ) : null}
      {expanded && hasResult ? (
        <pre style={{ whiteSpace: 'pre-wrap' }}>{row.resultPreview}</pre>
      ) : null}
    </section>
  )
}
