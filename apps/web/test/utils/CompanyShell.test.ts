// What the shell shows, and what it refuses to show.
//
// Three states, three questions: what does this company look like, what is
// this department doing, what is this one agent saying. The interesting rules
// are the ones that decide what NOT to show — a flattened subtree, a parent's
// borrowed headcount, a selection that no longer exists.
import { describe, expect, it } from 'vitest'

import type { CompanyTree, TreePerson } from '@/types/ChiefApi'
import { departmentOf, railDepartments, railPeople, shellView } from '@/utils/CompanyShell'

function person(id: string): TreePerson {
  return {
    id,
    name: id.toUpperCase(),
    title: 'Engineer',
    kind: 'worker',
    employmentState: 'active'
  }
}

const TREE: CompanyTree = {
  slug: 'acme',
  rootDepartmentId: 'executive',
  departments: [
    {
      id: 'executive',
      name: 'Executive',
      headPersonId: 'ceo',
      state: 'active',
      people: [person('ceo')],
      children: [
        {
          id: 'engineering',
          name: 'Engineering',
          headPersonId: 'ada',
          state: 'active',
          people: [person('ada'), person('bob')],
          children: [
            {
              id: 'platform',
              name: 'Platform',
              headPersonId: 'cleo',
              state: 'paused',
              people: [person('cleo')],
              children: []
            }
          ]
        }
      ]
    }
  ]
}

describe('railDepartments', () => {
  it('lists every department in tree order with its depth', () => {
    // Flat, with depth — a nested rail hides a department inside a collapsed
    // parent, and an operator looking for one they created a minute ago should
    // not have to guess which ancestor swallowed it.
    expect(railDepartments(TREE).map((row) => [row.id, row.depth])).toEqual([
      ['executive', 0],
      ['engineering', 1],
      ['platform', 2]
    ])
  })

  it('counts a department’s OWN people, never its subtree', () => {
    // A parent showing its children's headcount reads as its own: a department
    // of one with three sub-units would look like a department of ten.
    const rows = railDepartments(TREE)

    expect(rows.find((row) => row.id === 'executive')?.peopleCount).toBe(1)
    expect(rows.find((row) => row.id === 'engineering')?.peopleCount).toBe(2)
  })

  it('carries a paused department’s state rather than hiding it', () => {
    // A paused unit still exists and an operator needs to see it to resume it.
    expect(railDepartments(TREE).find((row) => row.id === 'platform')?.state).toBe('paused')
  })
})

describe('railPeople', () => {
  it('lists everybody in the company, in tree order', () => {
    expect(railPeople(TREE).map((entry) => entry.id)).toEqual(['ceo', 'ada', 'bob', 'cleo'])
  })
})

describe('shellView', () => {
  it('shows the company when nothing is selected', () => {
    const view = shellView(TREE, { kind: 'company' })

    expect(view.kind).toBe('company')
  })

  it('shows one column per person of the selected department only', () => {
    // Sub-department members are not pulled in: they have their own columns
    // under their own unit, and flattening a subtree into one row of columns
    // would make a two-person department look like a floor of twenty.
    const view = shellView(TREE, { kind: 'department', departmentId: 'engineering' })

    expect(view.kind).toBe('department')
    if (view.kind !== 'department') return
    expect(view.columns.map((entry) => entry.id)).toEqual(['ada', 'bob'])
  })

  it('shows one agent full width, and whose team it is on', () => {
    // An operator reading one agent still needs to know its department.
    const view = shellView(TREE, { kind: 'person', personId: 'cleo' })

    expect(view.kind).toBe('person')
    if (view.kind !== 'person') return
    expect(view.person.id).toBe('cleo')
    expect(view.departmentName).toBe('Platform')
  })

  it('falls back to the company when the selected department is gone', () => {
    // A department can be removed while somebody is looking at it. An empty
    // column would leave an operator staring at a unit that no longer exists
    // with no way back.
    const view = shellView(TREE, { kind: 'department', departmentId: 'deleted' })

    expect(view.kind).toBe('company')
  })

  it('falls back to the company when the selected person is gone', () => {
    // The same for somebody offboarded mid-view.
    const view = shellView(TREE, { kind: 'person', personId: 'departed' })

    expect(view.kind).toBe('company')
  })
})

describe('departmentOf', () => {
  it('finds a person’s department anywhere in the tree', () => {
    expect(departmentOf(TREE, 'cleo')?.id).toBe('platform')
  })

  it('answers nothing for somebody who is not in the tree', () => {
    expect(departmentOf(TREE, 'ghost')).toBeUndefined()
  })
})
