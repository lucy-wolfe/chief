'use client'

import type { ReactElement } from 'react'

import { ConversationView } from '@/components/pane/ConversationView'
import { PaneComposer } from '@/components/pane/PaneComposer'
import { SessionBanners } from '@/components/pane/SessionBanners'
import { useAgentConversation } from '@/hooks/UseAgentConversation'
import type { AgentPaneProps } from '@/types/Conversation'
import { CHIEF_THEME_TOKEN_HEX } from '@/utils/ThemeTokens'

function booleanSessionValue(
  session: { readonly [key: string]: unknown } | undefined,
  key: string
): boolean {
  return session?.[key] === true
}

function errorText(error: { code?: string; kind: string; detail?: string }): string {
  const code = error.code ?? error.kind
  return `${code}: ${error.detail ?? 'Request failed'}`
}

/** S3's person-pane body: one transcript source plus one structured event stream. */
export function AgentPane({ companyKey, pane, readOnly }: AgentPaneProps): ReactElement {
  const conversation = useAgentConversation(companyKey, pane.paneId)
  const accent = pane.accentColor ?? CHIEF_THEME_TOKEN_HEX['--chief-neutral-accent']
  const isStreaming = booleanSessionValue(conversation.session, 'isStreaming')
  const unavailable =
    conversation.hostState === 'starting' ||
    conversation.hostState === 'stopped' ||
    conversation.hostState === 'exited'
  const controlsDisabled = readOnly || conversation.hydrating || unavailable

  return (
    <section style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}>
      <SessionBanners
        channel={conversation.channel}
        host={conversation.host}
        hostState={conversation.hostState}
        readOnly={readOnly}
        runtime={conversation.runtime}
      />
      {conversation.paneError ? <p role="alert">{errorText(conversation.paneError)}</p> : null}
      <ConversationView accent={accent} rows={conversation.rows} />
      {!readOnly ? (
        <PaneComposer
          disabled={controlsDisabled}
          isStreaming={isStreaming}
          onAbort={conversation.abort}
          onSend={conversation.send}
        />
      ) : null}
    </section>
  )
}
