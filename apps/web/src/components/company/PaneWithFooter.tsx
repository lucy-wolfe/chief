'use client'

import type { ReactElement } from 'react'

import { PaneFooter } from '@/components/company/PaneFooter'
import { AgentPane } from '@/components/pane/AgentPane'
import { useOrgPaneMount, useOrgStore } from '@/hooks/UseOrgStore'
import type { PaneDescriptor } from '@/types/PaneLayout'

/**
 * S3's `renderPaneBody` slot for a `kind: 'person'` pane: S6's `AgentPane`
 * plus the tmux footer segments. Registers pane-mount interest with the org
 * store so its mailbox count joins the doc subscription and is fetched once
 * on mount (issue #404).
 */
export function PaneWithFooter(props: {
  companyKey: string
  pane: PaneDescriptor
  readOnly: boolean
}): ReactElement {
  useOrgPaneMount(props.pane.paneId)
  const store = useOrgStore()
  const footer = store.footerFor(props.pane.paneId)

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}>
      <div style={{ flex: 1, minHeight: 0 }}>
        <AgentPane companyKey={props.companyKey} pane={props.pane} readOnly={props.readOnly} />
      </div>
      <PaneFooter footer={footer} />
    </div>
  )
}
