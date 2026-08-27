'use client'

import type { ReactElement } from 'react'

import type { PersonFooterModel } from '@/types/OrgStore'
import { CHIEF_THEME_TOKEN_HEX } from '@/utils/ThemeTokens'

/**
 * Ported from the launcher tmux footer's segment composition
 * (`packages/piing/extensions/team-ui.ts`, the `📬 N` segment). One count of
 * rows apps/api already served — no local derivation, no countdown (apps/api
 * exposes no typed schedule fields yet; a fabricated countdown is worse than
 * an absent one).
 *
 * 📬 = mailbox (pending count) — rendered ONLY when a positive number has
 * actually been read; `undefined` (not yet read) and `0` both render nothing,
 * matching the launcher's "no idle 📬 0" rule.
 */
export function PaneFooter({ footer }: { footer: PersonFooterModel }): ReactElement {
  const segments: { key: string; text: string }[] = []
  if (typeof footer.pendingMailboxCount === 'number' && footer.pendingMailboxCount > 0) {
    segments.push({ key: 'mailbox-pending', text: `📬 ${footer.pendingMailboxCount}` })
  }

  if (segments.length === 0) return <></>

  return (
    <footer
      data-pane-footer="true"
      style={{
        display: 'flex',
        flexDirection: 'row',
        gap: '8px',
        padding: '2px 6px',
        fontSize: '0.85em',
        color: CHIEF_THEME_TOKEN_HEX['--chief-neutral-accent']
      }}
    >
      {segments.map((segment) => (
        <span key={segment.key} data-footer-segment={segment.key}>
          {segment.text}
        </span>
      ))}
    </footer>
  )
}
