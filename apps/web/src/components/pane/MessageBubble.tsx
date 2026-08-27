import type { ReactElement } from 'react'

import { contentBlockText, type ConversationRole, type ConversationRow } from '@/types/Conversation'
import { contrastingInk } from '@/utils/Contrast'

interface MessageBubbleProps {
  row: Extract<ConversationRow, { kind: 'message' }>
  accent: string
}

function roleLabel(role: ConversationRole): string {
  switch (role) {
    case 'user':
      return 'You'
    case 'assistant':
      return 'Agent'
    case 'custom':
      return 'System'
  }
}

/** One role-labeled, stream-aware message row. */
export function MessageBubble({ row, accent }: MessageBubbleProps): ReactElement {
  const content = row.content.map(contentBlockText).join('\n')
  const label = roleLabel(row.role)

  return (
    <article
      data-conversation-role={row.role}
      style={{
        alignSelf: row.role === 'user' ? 'flex-end' : 'flex-start',
        maxWidth: '92%',
        border: '1px solid var(--chief-pane-border)',
        padding: '4px 6px'
      }}
    >
      <span
        style={{
          background: accent,
          color: contrastingInk(accent),
          display: 'inline-block',
          fontSize: '0.75rem',
          fontWeight: 700,
          marginBottom: '3px',
          padding: '0 4px'
        }}
      >
        {label}
      </span>
      {row.role === 'custom' ? (
        <pre style={{ margin: 0, overflowWrap: 'anywhere', whiteSpace: 'pre-wrap' }}>{content}</pre>
      ) : (
        <p style={{ margin: 0, overflowWrap: 'anywhere', whiteSpace: 'pre-wrap' }}>{content}</p>
      )}
      {row.streaming ? <span aria-label="Streaming response"> ▍</span> : null}
    </article>
  )
}
