'use client'

import { type ReactElement, useState } from 'react'

import { StructureRail } from '@/components/company/StructureRail'
import type { DepartmentNode } from '@/types/ChiefApi'

/**
 * Collapsible rail holding the structure rail. Open/closed is in-memory only
 * (mandate 2) — it defaults closed on narrow viewports and carries no
 * persistence.
 */
export function OverviewRail(props: {
  companyKey: string
  departments: readonly DepartmentNode[]
  defaultOpen: boolean
  /** A stopped company cannot be restructured — every staffing route
   * resolves a chiefd client first. */
  readOnly: boolean
}): ReactElement {
  const [open, setOpen] = useState(props.defaultOpen)

  return (
    <aside data-overview-rail={open ? 'open' : 'closed'} className={open ? 'chief-rail' : ''}>
      <button
        type="button"
        aria-expanded={open}
        className="chief-toggle"
        onClick={(): void => setOpen((previous) => !previous)}
      >
        {open ? 'Hide overview' : 'Show overview'}
      </button>
      {open ? (
        <StructureRail
          companyKey={props.companyKey}
          departments={props.departments}
          readOnly={props.readOnly}
        />
      ) : null}
    </aside>
  )
}
