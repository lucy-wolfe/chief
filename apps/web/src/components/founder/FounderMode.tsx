'use client'

/**
 * Founder Mode: the web app's front door.
 *
 * You are not filling in a form. You are talking to Founder, and Founder is
 * the thing that creates the company — the same agent, with the same one
 * launch action, that `chief` opens in tmux.
 *
 * # The greeting is this page's, not the agent's
 *
 * Founder has no opening line, because a Pi harness does not speak until it is
 * spoken to, and making the page issue a hidden first turn to manufacture one
 * would spend a provider call to say hello. So the page says hello, in its own
 * voice, and every row below it is the real conversation.
 */
import Link from 'next/link'
import type { ReactElement } from 'react'

import { FounderComposer } from '@/components/founder/FounderComposer'
import { ConversationView } from '@/components/pane/ConversationView'
import { useFounderConversation } from '@/hooks/UseFounderConversation'
import { CHIEF_THEME_TOKEN_HEX } from '@/utils/ThemeTokens'

function errorText(error: { code?: string; kind: string; detail?: string }): string {
  return `${error.code ?? error.kind}: ${error.detail ?? 'Request failed'}`
}

export function FounderMode(): ReactElement {
  const conversation = useFounderConversation()

  return (
    <main className="chief-page chief-founder">
      <h1>Founder Mode</h1>
      <p className="chief-note">
        Tell Founder the company you want and why it exists. Founder creates it and boots its CEO —
        it does nothing else.
      </p>
      {conversation.launched ? (
        <p className="chief-founder-launched">
          <Link
            className="chief-link-button chief-link-button--primary"
            href={`/c/${encodeURIComponent(conversation.launched.key)}`}
          >
            Open {conversation.launched.name}
          </Link>
        </p>
      ) : null}
      {conversation.error ? (
        <p role="alert" className="chief-error">
          {errorText(conversation.error)}
        </p>
      ) : null}
      <div className="chief-founder-conversation">
        {conversation.hydrating ? (
          <p className="chief-empty">Waking Founder…</p>
        ) : (
          <ConversationView
            accent={CHIEF_THEME_TOKEN_HEX['--chief-index-active-bg']}
            rows={conversation.rows}
          />
        )}
      </div>
      <FounderComposer
        onAbort={conversation.abort}
        onSend={conversation.send}
        pending={conversation.pending}
      />
      <p className="chief-note">
        <Link href="/">Back to companies</Link>
      </p>
    </main>
  )
}
