'use client'

import type { ReactElement } from 'react'

import type { CompanyShellViewProps } from '@/types/CompanyShell'

/**
 * What the shell renders to the right of the rail.
 *
 * # Layout here, panes from the caller
 *
 * This decides SHAPE — one column per person, or one agent across the whole
 * width — and asks the caller to render each agent through `renderAgent`. The
 * shell therefore has no opinion about what an agent pane is, and a pane has
 * no opinion about how many of it are on screen. Wiring the pane in here would
 * make every layout test drag a conversation, a stream and a transport behind
 * it, and layout would only ever be testable by rendering the whole world.
 *
 * # Columns are equal width and scroll on their own
 *
 * A department of six is six columns, not a grid that reflows into rows: the
 * point of the department view is watching people work side by side, and a
 * reflow puts somebody below the fold where nobody sees them. The strip scrolls
 * horizontally instead, which keeps every column the same height and the same
 * width whatever the headcount.
 *
 * # The company branch is the landing state, so it has to DO something
 *
 * Nothing is selected when the page opens, which means the company branch is
 * the first — and for an operator who never clicks the rail, the only — thing
 * the product shows. It used to be a single sentence counting departments and
 * telling the operator to pick one, while the structure editor
 * sat fully built with no importer anywhere in the app.
 * The count stays (it is the one fact the branch already knew) and the overview
 * comes from `renderOverview` for the same reason panes come from
 * `renderAgent`: layout here, content from the caller.
 */
export function CompanyShellView(props: CompanyShellViewProps): ReactElement {
  const { view, renderAgent, renderOverview } = props

  if (view.kind === 'person') {
    return (
      <section
        aria-label={`Agent ${view.person.name}`}
        style={{ display: 'flex', flexDirection: 'column', height: '100%', minWidth: 0 }}
      >
        <header style={{ padding: '4px 8px' }}>
          {view.person.name} · {view.person.title} · {view.departmentName}
        </header>
        <div style={{ flex: 1, minHeight: 0 }}>{renderAgent(view.person)}</div>
      </section>
    )
  }

  if (view.kind === 'department') {
    return (
      <section
        aria-label={`Department ${view.departmentName}`}
        style={{ display: 'flex', flexDirection: 'column', height: '100%', minWidth: 0 }}
      >
        <header style={{ padding: '4px 8px' }}>
          {view.departmentName}
          {/* Named, not styled away: a paused unit still exists and an
              operator needs to see it to resume it. */}
          {view.state === 'paused' ? ' · paused' : ''}
        </header>
        {view.columns.length === 0 ? (
          // An empty department is a real state — a unit created before anybody
          // was hired into it. Saying so beats a blank area that reads as a
          // failure to load.
          <p style={{ padding: '4px 8px' }}>No agents in this department yet.</p>
        ) : (
          <div style={{ display: 'flex', flex: 1, gap: '8px', minHeight: 0, overflowX: 'auto' }}>
            {view.columns.map((person) => (
              <div
                aria-label={`Agent ${person.name}`}
                key={person.id}
                style={{
                  display: 'flex',
                  flex: '1 0 320px',
                  flexDirection: 'column',
                  minWidth: 0
                }}
              >
                <header style={{ padding: '4px 8px' }}>
                  {person.name} · {person.title}
                </header>
                <div style={{ flex: 1, minHeight: 0 }}>{renderAgent(person)}</div>
              </div>
            ))}
          </div>
        )}
      </section>
    )
  }

  return (
    <section
      aria-label="Company"
      style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0 }}
    >
      <p style={{ padding: '4px 8px' }}>
        {view.departments.length} department{view.departments.length === 1 ? '' : 's'}. Pick one to
        watch its agents side by side, or pick an agent to read it on its own.
      </p>
      {/* Scrolls on its own rather than growing the section: the content
          is a column strip and the structure editor grows with the tree, and a
          company of thirty would otherwise push the departments line off the
          top of the page. */}
      <div style={{ flex: 1, minHeight: 0, overflowY: 'auto', padding: '0 8px 8px' }}>
        {renderOverview()}
      </div>
    </section>
  )
}
