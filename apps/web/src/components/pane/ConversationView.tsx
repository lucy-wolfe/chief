'use client'

import { type ReactElement, type UIEvent, useEffect, useRef, useState } from 'react'

import { MessageBubble } from '@/components/pane/MessageBubble'
import { ToolActivityRow } from '@/components/pane/ToolActivityRow'
import type { ConversationRow } from '@/types/Conversation'

interface ConversationViewProps {
  rows: readonly ConversationRow[]
  accent: string
}

/** Dense scrollback that follows new rows only while the reader remains pinned. */
export function ConversationView({ rows, accent }: ConversationViewProps): ReactElement {
  const listRef = useRef<HTMLDivElement | null>(null)
  const [pinned, setPinned] = useState(true)

  useEffect(() => {
    const list = listRef.current
    if (!list || !pinned) return
    list.scrollTop = list.scrollHeight
  }, [pinned, rows])

  function handleScroll(event: UIEvent<HTMLDivElement>): void {
    const list = event.currentTarget
    const distance = list.scrollHeight - list.clientHeight - list.scrollTop
    setPinned(distance <= 4)
  }

  function jumpToLatest(): void {
    const list = listRef.current
    if (!list) return
    list.scrollTop = list.scrollHeight
    setPinned(true)
  }

  return (
    <div style={{ display: 'flex', flex: 1, flexDirection: 'column', minHeight: 0 }}>
      <div
        aria-label="Agent conversation"
        data-autoscroll-pinned={pinned}
        onScroll={handleScroll}
        ref={listRef}
        style={{
          display: 'flex',
          flex: 1,
          flexDirection: 'column',
          gap: '6px',
          overflowY: 'auto',
          padding: '6px'
        }}
      >
        {rows.map((row) => {
          switch (row.kind) {
            case 'message':
              return <MessageBubble accent={accent} key={row.id} row={row} />
            case 'tool':
              return <ToolActivityRow key={row.id} row={row} />
            case 'turn-break':
              return <hr key={row.id} style={{ width: '100%' }} />
            case 'activity':
              return (
                <p key={row.id} style={{ margin: 0, opacity: 0.7 }}>
                  {row.label}
                </p>
              )
          }
        })}
      </div>
      {!pinned && rows.length > 0 ? (
        <button onClick={jumpToLatest} type="button">
          Jump to latest
        </button>
      ) : null}
    </div>
  )
}
