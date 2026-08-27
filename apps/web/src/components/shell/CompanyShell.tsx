'use client'

import { type ReactElement, type ReactNode, useMemo, useState } from 'react'

import { OverviewRail } from '@/components/company/OverviewRail'
import { PaneWithFooter } from '@/components/company/PaneWithFooter'
import { CompanyRail } from '@/components/shell/CompanyRail'
import { CompanyShellView } from '@/components/shell/CompanyShellView'
import { useOrgStore } from '@/hooks/UseOrgStore'
import type { CompanyTree, TreePerson } from '@/types/ChiefApi'
import type { ShellSelection } from '@/types/CompanyShell'
import { railDepartments, railPeople, shellView } from '@/utils/CompanyShell'

/**
 * The company, as an operator reads it: a rail on the left, and whatever they
 * picked on the right.
 *
 * # Selection is the only state here
 *
 * Everything else — the department forest, who is running — comes from the org
 * store, which is fed by chiefd's change feed. This component computes no
 * membership, no placement and no running state of its own; it holds one
 * `ShellSelection` and asks pure functions what that means. The layer that
 * decided such things for itself is the one that was deleted.
 *
 * # Panes are opened by SELECTION, never by existence
 *
 * `renderAgent` runs only for the people the current view actually shows — one
 * department's columns, or one agent. A shell that mounted a pane per person
 * would open a conversation, a transcript read and a live stream for every
 * agent in the company on first paint, and a company of thirty would hammer
 * its own daemon before the operator had clicked anything.
 */
export function CompanyShell(props: { companyKey: string; readOnly?: boolean }): ReactElement {
  const { companyKey, readOnly = false } = props
  const store = useOrgStore()
  const [selection, setSelection] = useState<ShellSelection>({ kind: 'company' })

  // The forest as the tree type wants it. `departments` is the store's
  // unabridged answer — it keeps people-less departments, which `windows`
  // drops — and an empty department is still somewhere to hire into, so the
  // rail must show it.
  const tree: CompanyTree = useMemo(
    () => ({
      // chiefd's own field name on `POST /v1/org/tree/structured`, and it
      // echoes back the company key it was asked with — see `CompanyTree`.
      slug: companyKey,
      rootDepartmentId: store.departments[0]?.id ?? '',
      departments: [...store.departments]
    }),
    [companyKey, store.departments]
  )

  // Who is actually up, from the roster the host converged. Not a guess from
  // the tree: being IN a company and being RUNNING are different facts, and
  // conflating them is how an operator ends up looking at an agent that seems
  // healthy and never answers.
  const running = useMemo(
    () =>
      new Set(
        store.windows.flatMap((window) =>
          window.panes.filter((pane) => pane.running).map((pane) => pane.personId)
        )
      ),
    [store.windows]
  )

  const view = useMemo(() => shellView(tree, selection), [tree, selection])
  const departments = useMemo(() => railDepartments(tree), [tree])
  const people = useMemo(() => railPeople(tree), [tree])

  /** The pane, with the launcher's footer under it.
   *
   * `PaneWithFooter` rather than `AgentPane` directly: the 🎯/🧭/📬 counts are
   * the only place the web says how much supervision a person is carrying, and
   * mounting the footer is also what registers pane-mount interest with the
   * org store — so a mailbox count is fetched for the people actually on
   * screen and for nobody else. Rendering `AgentPane` here left both facts
   * unreachable: the footer had no importer, and no pane ever registered, so
   * `registerMountedPerson` was called by tests alone. */
  const renderAgent = (person: TreePerson): ReactNode => (
    <PaneWithFooter
      pane={{
        paneId: person.id,
        title: person.title,
        accentColor: person.accent ?? null,
        kind: 'person'
      }}
      companyKey={companyKey}
      readOnly={readOnly}
    />
  )

  /** The company branch's content. `store.departments` rather than the view's
   * `RailDepartment` rows: those are flattened for display and the structure
   * editor needs the real forest — a re-parent target is a NODE, and its
   * subtree is what a cycle check reads.
   *
   * `defaultOpen` is `true`. An operator lands here with nothing selected, and
   * a collapsed rail would mean the company view opens showing one sentence —
   * the exact state this wiring exists to end. */
  const renderOverview = (): ReactNode => (
    <OverviewRail
      companyKey={companyKey}
      defaultOpen
      departments={store.departments}
      readOnly={readOnly}
    />
  )

  if (!store.ready) return <p>Loading company…</p>

  return (
    <div style={{ display: 'flex', gap: '8px', height: '100%', minHeight: 0 }}>
      <aside style={{ flex: '0 0 220px', overflowY: 'auto' }}>
        <CompanyRail
          departments={departments}
          onSelect={setSelection}
          people={people}
          running={running}
          selection={selection}
        />
      </aside>
      <main style={{ flex: 1, minWidth: 0 }}>
        <CompanyShellView renderAgent={renderAgent} renderOverview={renderOverview} view={view} />
      </main>
    </div>
  )
}
