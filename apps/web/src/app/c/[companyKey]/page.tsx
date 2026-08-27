'use client'

import { useParams } from 'next/navigation'
import { type ReactElement, Suspense } from 'react'

import { CompanyShell } from '@/components/shell/CompanyShell'
import { OrgStoreProvider } from '@/providers/OrgStoreProvider'

function CompanyPageContent(): ReactElement {
  const params = useParams()
  const raw = params.companyKey
  // `sha256(<dir>)[..12]`, never a slug: two directories may hold companies
  // with the same slug, so a route keyed by one would open whichever the
  // registry listed first.
  const companyKey = Array.isArray(raw) ? (raw[0] ?? '') : (raw ?? '')

  return (
    <OrgStoreProvider companyKey={companyKey}>
      <CompanyShell companyKey={companyKey} />
    </OrgStoreProvider>
  )
}

/** The company, as an operator reads it (#751/P4): a rail on the left and
 * whatever they picked on the right.
 *
 * This replaces `CompanyView`, which rendered a company as tmux WINDOWS and
 * PANES — a faithful mirror of the terminal, and the wrong shape for a
 * browser. A window per department meant an operator had to switch tabs to see
 * two teams, and a pane grid re-derived a layout the tmux side already owns.
 * The shell asks the two questions a browser is good at instead: show me this
 * department's people side by side, or show me this one agent. */
export default function CompanyPage(): ReactElement {
  return (
    <Suspense fallback={<p>Loading company…</p>}>
      <CompanyPageContent />
    </Suspense>
  )
}
