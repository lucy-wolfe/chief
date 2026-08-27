// @vitest-environment jsdom
// The rail: what an operator can reach, and what it tells them.
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { CompanyRail } from '@/components/shell/CompanyRail'
import type { TreePerson } from '@/types/ChiefApi'
import type { CompanyRailProps, RailDepartment } from '@/types/CompanyShell'

const DEPARTMENTS: RailDepartment[] = [
  {
    id: 'executive',
    name: 'Executive',
    depth: 0,
    state: 'active',
    headPersonId: 'ceo',
    peopleCount: 1
  },
  {
    id: 'platform',
    name: 'Platform',
    depth: 1,
    state: 'paused',
    headPersonId: 'ada',
    peopleCount: 2
  }
]
const PEOPLE: TreePerson[] = [
  { id: 'ceo', name: 'Cleo', title: 'CEO', kind: 'executive', employmentState: 'active' },
  { id: 'ada', name: 'Ada', title: 'Engineer', kind: 'worker', employmentState: 'active' }
]

let container: HTMLDivElement
let root: Root

beforeEach(() => {
  container = document.createElement('div')
  document.body.appendChild(container)
  root = createRoot(container)
})

afterEach(() => {
  act(() => root.unmount())
  container.remove()
})

function render(props: Partial<CompanyRailProps> = {}): {
  onSelect: ReturnType<typeof vi.fn>
} {
  const onSelect = vi.fn()
  act(() => {
    root.render(
      <CompanyRail
        departments={DEPARTMENTS}
        onSelect={onSelect}
        people={PEOPLE}
        running={new Set(['ceo'])}
        selection={{ kind: 'company' }}
        {...props}
      />
    )
  })
  return { onSelect }
}

describe('CompanyRail', () => {
  it('shows departments AND agents at once', () => {
    // A rail that showed only the selected department's agents would make an
    // operator navigate up and back down to reach anybody else.
    render()

    expect(container.querySelector('[aria-label="Departments"]')).not.toBeNull()
    expect(container.querySelector('[aria-label="Agents"]')).not.toBeNull()
  })

  it('makes every row a real button', () => {
    // Keyboard-reachable and announced as actionable, with no aria needed. The
    // one time this app hand-rolled a clickable container it swallowed the
    // space key and the composer dropped every space an operator typed.
    render()

    const buttons = Array.from(container.querySelectorAll('button'))
    expect(buttons).toHaveLength(DEPARTMENTS.length + PEOPLE.length)
    expect(buttons.every((button) => button.type === 'button')).toBe(true)
  })

  it('reports which department was clicked', () => {
    const { onSelect } = render()
    const platform = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.startsWith('Platform')
    )

    act(() => platform?.click())

    expect(onSelect).toHaveBeenCalledWith({ kind: 'department', departmentId: 'platform' })
  })

  it('reports which agent was clicked', () => {
    const { onSelect } = render()
    const ada = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Ada')
    )

    act(() => ada?.click())

    expect(onSelect).toHaveBeenCalledWith({ kind: 'person', personId: 'ada' })
  })

  it('names a paused department rather than styling it away', () => {
    // A paused unit still exists, and an operator needs to see it to resume it.
    render()

    const platform = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.startsWith('Platform')
    )
    expect(platform?.textContent).toContain('paused')
  })

  it('distinguishes a running agent from a dormant one', () => {
    // "Is this thing alive" is the question an operator asks first, and a
    // dormant agent that looks identical to a live one is the failure this
    // whole rewrite has been repairing.
    render()

    const buttons = Array.from(container.querySelectorAll('button'))
    const cleo = buttons.find((button) => button.textContent?.includes('Cleo'))
    const ada = buttons.find((button) => button.textContent?.includes('Ada'))
    expect(cleo?.textContent?.startsWith('●')).toBe(true)
    expect(ada?.textContent?.startsWith('○')).toBe(true)
  })

  it('marks the current selection for a screen reader too', () => {
    render({ selection: { kind: 'person', personId: 'ada' } })

    const ada = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Ada')
    )
    expect(ada?.getAttribute('aria-current')).toBe('true')
  })
})
