// @vitest-environment jsdom
// The container: what it holds, and what it refuses to open.
import { createFakeChiefApi, FIXTURE_JWT } from '@test/harness/FakeChiefApi'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { ApiSessionProvider } from '@/providers/ApiSessionProvider'
import { ChiefApiClientService } from '@/services/ChiefApiClientService'

const paneFor = vi.fn()

vi.mock('@/components/pane/AgentPane', () => ({
  AgentPane: (props: { pane: { paneId: string } }) => {
    paneFor(props.pane.paneId)
    return null
  }
}))

const snapshot = {
  ready: true,
  windows: [
    {
      windowId: 'engineering',
      name: 'Engineering',
      headAccentColor: null,
      panes: [
        { personId: 'ada', title: 'Engineer', name: 'Ada', accentColor: null, running: true },
        { personId: 'bob', title: 'Engineer', name: 'Bob', accentColor: null, running: false }
      ]
    }
  ],
  departments: [
    {
      id: 'executive',
      name: 'Executive',
      headPersonId: 'ceo',
      state: 'active' as const,
      people: [
        { id: 'ceo', name: 'Cleo', title: 'CEO', kind: 'executive', employmentState: 'active' }
      ],
      children: [
        {
          id: 'engineering',
          name: 'Engineering',
          headPersonId: 'ada',
          state: 'active' as const,
          people: [
            {
              id: 'ada',
              name: 'Ada',
              title: 'Engineer',
              kind: 'worker',
              employmentState: 'active'
            },
            { id: 'bob', name: 'Bob', title: 'Engineer', kind: 'worker', employmentState: 'active' }
          ],
          children: []
        }
      ]
    }
  ],
  // The company branch now renders the overview — structure and goals —
  // rather than one sentence telling the operator to pick something else, so
  // the store snapshot has to carry the two lists that branch reads.
  goals: [],
  // Every pane the shell opens carries the launcher's footer, so the snapshot
  // has to answer `footerFor` too. Only Ada is carrying anything: a footer
  // that renders for everybody would say nothing.
  footerFor: (personId: string) =>
    personId === 'ada'
      ? { activeGoalCount: 2, delegatedGoalCount: 1, pendingMailboxCount: 3 }
      : { activeGoalCount: 0, delegatedGoalCount: 0, pendingMailboxCount: undefined }
}

const mounted: string[] = []

vi.mock('@/hooks/UseOrgStore', () => ({
  useOrgStore: () => snapshot,
  useOrgPaneMount: (personId: string) => {
    mounted.push(personId)
  }
}))

const { CompanyShell } = await import('@/components/shell/CompanyShell')

let container: HTMLDivElement
let root: Root

beforeEach(() => {
  paneFor.mockReset()
  mounted.length = 0
  container = document.createElement('div')
  document.body.appendChild(container)
  root = createRoot(container)
})

afterEach(() => {
  act(() => root.unmount())
  container.remove()
})

/** The shell's company branch reaches the api client through the session
 * provider (the structure editor's verbs live there), so the shell can no
 * longer be rendered bare. The REAL client over the shared fake, rather than a
 * hand-written stub: a stub would be a second opinion about what the client
 * does, which is the shape of defect this suite keeps finding. */
function render(): void {
  const { fetchImpl } = createFakeChiefApi()
  const client = new ChiefApiClientService({
    baseUrl: 'http://web.example',
    accessToken: () => FIXTURE_JWT,
    fetchImpl
  })
  act(() => {
    root.render(
      <ApiSessionProvider client={client} tokenGetter={(): string => FIXTURE_JWT}>
        <CompanyShell companyKey="0123456789ab" />
      </ApiSessionProvider>
    )
  })
}

function click(text: string): void {
  const button = Array.from(container.querySelectorAll('button')).find((entry) =>
    entry.textContent?.includes(text)
  )
  act(() => button?.click())
}

describe('CompanyShell', () => {
  it('opens NO agent pane until something is selected', () => {
    // A shell that mounted a pane per person would open a conversation, a
    // transcript read and a live stream for every agent on first paint — a
    // company of thirty would hammer its own daemon before a single click.
    render()

    expect(paneFor).not.toHaveBeenCalled()
  })

  it('opens one pane per person of a selected department, and no others', () => {
    render()

    click('Engineering')

    expect(paneFor.mock.calls.map((call) => call[0])).toEqual(['ada', 'bob'])
  })

  it('opens exactly one pane for a selected agent', () => {
    render()

    click('Cleo')

    expect(paneFor.mock.calls.map((call) => call[0])).toEqual(['ceo'])
  })

  it('puts the pane footer under each pane it opens', () => {
    // The footer is the ONLY place the web says how much pending mail a person
    // is carrying. It had no importer at all until the shell rendered it, and
    // a component nothing renders is not a feature. The 🎯/🧭 goal segments it
    // also carried went with the goal feature; 📬 is the whole footer now.
    render()

    click('Engineering')

    const segments = Array.from(container.querySelectorAll('[data-footer-segment]')).map(
      (entry) => entry.textContent
    )
    expect(segments).toEqual(['📬 3'])
  })

  it('registers pane-mount interest for exactly the people on screen', () => {
    // Mounting the footer is what joins a person's mailbox to the doc
    // subscription. Registering for everybody would fetch a mailbox per
    // person in the company on first paint.
    render()

    click('Cleo')

    expect(mounted).toEqual(['ceo'])
  })

  it('shows a department that has nobody in it', () => {
    // `departments` is the store's unabridged answer; `windows` drops
    // people-less units. An empty department is still somewhere to hire into,
    // so the rail must show it.
    render()

    const rail = container.querySelector('[aria-label="Departments"]')
    expect(rail?.textContent).toContain('Executive')
    expect(rail?.textContent).toContain('Engineering')
  })

  it('takes running state from the roster, not from the tree', () => {
    // Being IN a company and being RUNNING are different facts. Conflating
    // them is how an operator ends up looking at an agent that seems healthy
    // and never answers.
    render()

    const agents = container.querySelector('[aria-label="Agents"]')
    expect(agents?.textContent).toContain('● Ada')
    expect(agents?.textContent).toContain('○ Bob')
    // The CEO has no pane in any window, so nothing claims they are running.
    expect(agents?.textContent).toContain('○ Cleo')
  })
})
