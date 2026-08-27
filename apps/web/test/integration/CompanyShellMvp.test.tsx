// @vitest-environment jsdom
/**
 * The shell against the real wiring: `FakeChiefApi` (REST) + `FakeSseStreams`
 * (doc/person SSE), mounted through the real providers.
 *
 * This MOVES the coverage that `CompanyViewMvp` held for the deleted tmux
 * view, in the shell's own vocabulary: rail rows instead of window tabs, a
 * selected department's columns instead of a focused pane. The properties are
 * the ones that matter and are the same either way — a doc event drives
 * exactly the typed re-read the Contract names and never a wider one, and a
 * selected agent streams a real turn.
 */
import { createFakeChiefApi, FIXTURE_COMPANY_KEY, FIXTURE_JWT } from '@test/harness/FakeChiefApi'
import { createFakeSseStreams } from '@test/harness/FakeSseStreams'
import { act, createElement, type ReactElement, type ReactNode } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('next/navigation', () => ({
  useRouter: () => ({ replace: vi.fn() }),
  usePathname: () => `/c/${FIXTURE_COMPANY_KEY}`,
  useSearchParams: () => new URLSearchParams()
}))

import { CompanyShell } from '@/components/shell/CompanyShell'
import { ApiSessionProvider } from '@/providers/ApiSessionProvider'
import { CompanyEventsProvider } from '@/providers/CompanyEventsProvider'
import { OrgStoreProvider } from '@/providers/OrgStoreProvider'
import { ChiefApiClientService } from '@/services/ChiefApiClientService'
import type { SseHubDeps } from '@/types/Sse'

const BASE_URL = 'http://fake-api.test'

async function flushWork(): Promise<void> {
  for (let index = 0; index < 10; index += 1) await Promise.resolve()
}

/* eslint-disable lucy/no-json-stringify */
// A raw SSE wire frame for the fake stream, not an app-API call.
function docFrame(store: string, seq: number): string {
  return `id: 1.${seq}\nevent: doc\ndata: ${JSON.stringify({
    companyKey: FIXTURE_COMPANY_KEY,
    store,
    seq,
    generation: 1,
    updatedAt: '2026-08-05T00:00:00.000Z',
    removed: false
  })}\n\n`
}
/* eslint-enable lucy/no-json-stringify */

/** The org doc bridge's own connection, not merely the first pending fetch.
 *
 * Each mounted `AgentPane` opens its own person stream, and the store widens
 * its own subscription once the roster hydrates — which replaces its fetch and
 * leaves the earlier attempt logged as aborted. `openMatching` scans only what
 * is still pending, so it cannot overcount. */
function openOrgEventsStream(
  sse: ReturnType<typeof createFakeSseStreams>
): ReturnType<typeof sse.openMatching> {
  return sse.openMatching((url) => url.includes('/events') && !url.includes('/people/'))
}

function App(props: {
  children: ReactNode
  client: ChiefApiClientService
  deps: SseHubDeps
}): ReactElement {
  return createElement(ApiSessionProvider, {
    client: props.client,
    tokenGetter: () => FIXTURE_JWT,
    children: createElement(CompanyEventsProvider, {
      deps: props.deps,
      children: createElement(OrgStoreProvider, {
        companyKey: FIXTURE_COMPANY_KEY,
        client: props.client,
        children: props.children
      })
    })
  })
}

describe('CompanyShell MVP integration', () => {
  let container: HTMLDivElement
  let root: Root

  function mount(): {
    chiefApi: ReturnType<typeof createFakeChiefApi>
    sse: ReturnType<typeof createFakeSseStreams>
  } {
    const chiefApi = createFakeChiefApi()
    const client = new ChiefApiClientService({
      baseUrl: BASE_URL,
      fetchImpl: chiefApi.fetchImpl,
      accessToken: () => FIXTURE_JWT
    })
    const sse = createFakeSseStreams()
    const deps: SseHubDeps = {
      baseUrl: BASE_URL,
      accessToken: () => FIXTURE_JWT,
      fetchImpl: sse.fetchImpl
    }
    act(() => {
      root.render(
        createElement(App, {
          client,
          deps,
          children: createElement(CompanyShell, { companyKey: FIXTURE_COMPANY_KEY })
        })
      )
    })
    return { chiefApi, sse }
  }

  function click(text: string): void {
    const button = Array.from(container.querySelectorAll('button')).find((entry) =>
      entry.textContent?.includes(text)
    )
    act(() => button?.click())
  }

  beforeEach(() => {
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
  })

  afterEach(() => {
    act(() => {
      root.unmount()
    })
    container.remove()
  })

  it('lists every department in the rail, including one nobody is in', async () => {
    // Fixture tree: root (Cora CEO), engineering (6), sales (0). The tmux view
    // DROPPED sales, because a window with no panes is not a window. A rail
    // has no such excuse: an empty department is still somewhere to hire into,
    // and one an operator cannot see is one they cannot staff.
    const { sse } = mount()
    openOrgEventsStream(sse)
    await act(async () => {
      await flushWork()
    })

    const rail = container.querySelector('[aria-label="Departments"]')
    expect(rail?.textContent).toContain('Engineering')
    expect(rail?.textContent).toContain('Sales')
  })

  it('opens no agent until one is selected, then streams its real turn', async () => {
    const { sse } = mount()
    openOrgEventsStream(sse)
    await act(async () => {
      await flushWork()
    })

    // Nothing selected: no person stream has been opened at all.
    expect(sse.requests.some((request) => request.url.includes('/people/'))).toBe(false)

    click('Cora')
    await act(async () => {
      await flushWork()
    })

    // The transcript hydrates only once the stream says a child is live —
    // `…/transcript` answers 409 `person-not-running` for a dormant agent, so
    // a pane that fetched unconditionally would show an error on every
    // dormant person.
    const ceoStream = sse.openMatching((url) => url.includes('/people/person-ceo/stream'))
    await act(async () => {
      await flushWork()
    })
    ceoStream.pushLiveAgentGreeting()
    await act(async () => {
      await flushWork()
    })

    expect(container.textContent).toContain('Please check the current plan.')
  })

  it('a runtime doc event re-reads exactly the roster route, and nothing wider', async () => {
    const { chiefApi, sse } = mount()
    const stream = openOrgEventsStream(sse)
    await act(async () => {
      await flushWork()
    })

    // Reset right before the event, so only what it triggers is visible.
    chiefApi.requests.length = 0
    stream.push(docFrame('runtime', 1))
    await act(async () => {
      await flushWork()
    })

    expect(chiefApi.requests.map((request) => request.path)).toEqual([
      `/companies/${FIXTURE_COMPANY_KEY}/people`
    ])
  })

  it('an org-manifest doc event adds a department to the rail live', async () => {
    const { chiefApi, sse } = mount()
    const stream = openOrgEventsStream(sse)
    await act(async () => {
      await flushWork()
    })

    // A re-served tree carrying a new department, in the REAL `CompanyTree`
    // shape: `{slug, rootDepartmentId, departments}`, people carrying only
    // `{id, name, title, kind, accent?}`.
    chiefApi.setTree(FIXTURE_COMPANY_KEY, {
      slug: FIXTURE_COMPANY_KEY,
      rootDepartmentId: 'root',
      departments: [
        {
          id: 'root',
          name: 'Acme',
          headPersonId: 'person-ceo',
          state: 'active',
          people: [
            {
              id: 'person-ceo',
              name: 'Cora',
              title: 'CEO',
              kind: 'executive',
              employmentState: 'active'
            }
          ],
          children: [
            {
              id: 'design',
              name: 'Design',
              headPersonId: 'person-ceo',
              state: 'active',
              people: [],
              children: []
            }
          ]
        }
      ]
    })
    chiefApi.requests.length = 0
    // `org-manifest`, the store name chiefd actually publishes for a
    // structural change. `organization` is not a store any subscriber names,
    // and a frame carrying it would be dropped without a word.
    stream.push(docFrame('org-manifest', 2))
    await act(async () => {
      await flushWork()
    })

    // Exactly the typed re-read the Contract names — the tree, and nothing
    // wider. A structural change that re-read people and goals too would cost
    // three round trips for one fact.
    expect(chiefApi.requests.map((request) => request.path)).toEqual([
      `/companies/${FIXTURE_COMPANY_KEY}/tree`
    ])
    expect(container.querySelector('[aria-label="Departments"]')?.textContent).toContain('Design')
  })
})
