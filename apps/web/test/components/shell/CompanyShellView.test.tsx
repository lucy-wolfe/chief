// @vitest-environment jsdom
// The shape the shell renders, and nothing about what a pane contains.
//
// Layout is tested WITHOUT a conversation, a stream or a transport, because
// `renderAgent` belongs to the caller. That split is the point: wiring a pane
// into the layout would make every one of these tests drag the whole world
// behind it, and layout would only ever be testable by rendering all of it.
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { CompanyShellView } from '@/components/shell/CompanyShellView'
import type { TreePerson } from '@/types/ChiefApi'
import type { ShellView } from '@/types/CompanyShell'

const ADA: TreePerson = {
  id: 'ada',
  name: 'Ada',
  title: 'Engineer',
  kind: 'worker',
  employmentState: 'active'
}
const BOB: TreePerson = {
  id: 'bob',
  name: 'Bob',
  title: 'Engineer',
  kind: 'worker',
  employmentState: 'active'
}

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

/** The overview is the CALLER's, exactly like a pane: the shell decides shape
 * and has no opinion about what an overview is. A stub keeps these layout tests
 * from dragging the org store, the api client and five staffing verbs behind
 * them — the same split `renderAgent` already buys. */
const OVERVIEW = 'overview:stub'

function render(view: ShellView): string[] {
  const rendered: string[] = []
  act(() => {
    root.render(
      <CompanyShellView
        renderAgent={(person): string => {
          rendered.push(person.id)
          return `pane:${person.id}`
        }}
        renderOverview={(): string => OVERVIEW}
        view={view}
      />
    )
  })
  return rendered
}

describe('CompanyShellView', () => {
  it('renders one column per person of the department, side by side', () => {
    const rendered = render({
      kind: 'department',
      departmentId: 'engineering',
      departmentName: 'Engineering',
      state: 'active',
      headPersonId: 'ada',
      columns: [ADA, BOB]
    })

    expect(rendered).toEqual(['ada', 'bob'])
    expect(container.querySelectorAll('[aria-label^="Agent "]')).toHaveLength(2)
  })

  it('scrolls the column strip rather than reflowing people below the fold', () => {
    // The point of the department view is watching people work side by side. A
    // grid that wrapped would put somebody under the fold where nobody looks.
    render({
      kind: 'department',
      departmentId: 'engineering',
      departmentName: 'Engineering',
      state: 'active',
      headPersonId: 'ada',
      columns: [ADA, BOB]
    })

    const strip = container.querySelector<HTMLElement>('[aria-label="Department Engineering"] div')
    expect(strip?.style.overflowX).toBe('auto')
    expect(strip?.style.flexWrap).not.toBe('wrap')
  })

  it('says an empty department is empty rather than showing a blank area', () => {
    // A unit created before anybody was hired into it is a real state. A blank
    // area reads as a failure to load.
    render({
      kind: 'department',
      departmentId: 'new',
      departmentName: 'New',
      state: 'active',
      headPersonId: 'ada',
      columns: []
    })

    expect(container.textContent).toContain('No agents in this department yet')
  })

  it('names a paused department rather than styling it away', () => {
    render({
      kind: 'department',
      departmentId: 'platform',
      departmentName: 'Platform',
      state: 'paused',
      headPersonId: 'ada',
      columns: [ADA]
    })

    expect(container.textContent).toContain('paused')
  })

  it('renders one agent across the width, with whose team it is on', () => {
    // An operator reading one agent still needs to know its department.
    const rendered = render({
      kind: 'person',
      person: ADA,
      departmentId: 'engineering',
      departmentName: 'Engineering'
    })

    expect(rendered).toEqual(['ada'])
    expect(container.textContent).toContain('Engineering')
  })

  it('renders no agent at all for the company view', () => {
    // Nothing is selected, so nothing is hosted on screen: rendering every
    // agent in the company here would open a pane per person on first paint.
    const rendered = render({ kind: 'company', departments: [] })

    expect(rendered).toEqual([])
  })

  it('renders the caller’s overview in the company branch, not just a sentence', () => {
    // The company branch is the LANDING state — nothing is selected when the
    // page opens — so for an operator who never clicks the rail it is the only
    // thing the product shows. It used to be a single sentence counting
    // departments and telling them to pick one, while the structure editor, the
    // goals rail sat fully built with no importer anywhere
    // in the app. `renderOverview` is REQUIRED rather than optional for exactly
    // that reason: a caller that forgot it would silently land back on the
    // sentence.
    render({ kind: 'company', departments: [] })

    expect(container.textContent).toContain(OVERVIEW)
  })
})
