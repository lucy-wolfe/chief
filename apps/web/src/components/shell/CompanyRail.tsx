'use client'

import type { CSSProperties, ReactElement } from 'react'

import type { CompanyRailProps } from '@/types/CompanyShell'

/**
 * The left rail: departments above, agents below.
 *
 * Both lists are always visible. A rail that showed only the selected
 * department's agents would make an operator navigate up and back down to
 * reach anybody else, and the whole point of a rail is that everything is one
 * click away.
 *
 * Rows are `button`s rather than clickable `div`s: a button is reachable by
 * keyboard and announced as actionable without a single aria attribute. The
 * one time this app hand-rolled a clickable container it swallowed the space
 * key, and the composer silently dropped every space an operator typed.
 */
function rowStyle(selected: boolean, depth = 0): CSSProperties {
  return {
    display: 'block',
    width: '100%',
    textAlign: 'left',
    border: 'none',
    background: selected ? 'var(--rail-selected, #2a2a3a)' : 'transparent',
    color: 'inherit',
    cursor: 'pointer',
    padding: '4px 8px',
    paddingLeft: `${8 + depth * 12}px`,
    font: 'inherit'
  }
}

export function CompanyRail(props: CompanyRailProps): ReactElement {
  const { departments, people, selection, running, onSelect } = props

  return (
    <nav aria-label="Company" style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
      <section aria-label="Departments">
        <h2 style={{ font: 'inherit', fontWeight: 600, padding: '4px 8px', margin: 0 }}>
          Departments
        </h2>
        {departments.map((department) => (
          <button
            aria-current={
              selection.kind === 'department' && selection.departmentId === department.id
                ? 'true'
                : undefined
            }
            key={department.id}
            onClick={(): void => onSelect({ kind: 'department', departmentId: department.id })}
            style={rowStyle(
              selection.kind === 'department' && selection.departmentId === department.id,
              department.depth
            )}
            type="button"
          >
            {department.name}
            {/* Paused is named, not styled away. A paused unit still exists,
                and an operator needs to see it to resume it. */}
            {department.state === 'paused' ? ' · paused' : ''} · {department.peopleCount}
          </button>
        ))}
      </section>

      <section aria-label="Agents">
        <h2 style={{ font: 'inherit', fontWeight: 600, padding: '4px 8px', margin: 0 }}>Agents</h2>
        {people.map((person) => (
          <button
            aria-current={
              selection.kind === 'person' && selection.personId === person.id ? 'true' : undefined
            }
            key={person.id}
            onClick={(): void => onSelect({ kind: 'person', personId: person.id })}
            style={rowStyle(selection.kind === 'person' && selection.personId === person.id)}
            type="button"
          >
            {/* Running state is shown per agent, because "is this thing alive"
                is the question an operator asks first — and a dormant agent
                that looks identical to a live one is the failure this whole
                rewrite has been repairing. */}
            {running.has(person.id) ? '● ' : '○ '}
            {person.name}
          </button>
        ))}
      </section>
    </nav>
  )
}
