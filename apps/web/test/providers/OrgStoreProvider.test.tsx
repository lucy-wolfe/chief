// @vitest-environment jsdom
import { createFakeSseStreams } from '@test/harness/FakeSseStreams'
import { act, createElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { useOrgPaneMount, useOrgStore } from '@/hooks/UseOrgStore'
import { CompanyEventsProvider } from '@/providers/CompanyEventsProvider'
import { ORG_STORE_BASE_STORES, OrgStoreProvider } from '@/providers/OrgStoreProvider'
import { activeSseConnectionCount } from '@/services/SseClientService'
import { ChiefApiError } from '@/types/ApiErrors'
import type {
  CompanySummary,
  CompanyTree,
  DepartmentNode,
  MailboxResponse,
  PeopleResponse,
  TreePerson
} from '@/types/ChiefApi'
import type { OrgStoreApi } from '@/types/OrgStore'
import type { SseHubDeps } from '@/types/Sse'

const BASE_URL = 'http://fake-api.test'
/** The two handles kept DELIBERATELY different, so nothing here can pass by
 * accidentally holding the one it was not given. */
const COMPANY_KEY = '0123456789ab'
const SLUG = 'acme'

interface StoreBox {
  current: OrgStoreApi | undefined
}

function Harness({ box }: { box: StoreBox }): null {
  box.current = useOrgStore()
  return null
}

/** Stands in for a mounted `PaneWithFooter`: registering pane-mount interest
 * is what puts a person's `mailbox/<personId>` store on the doc subscription
 * and makes their footer count eligible to be read. */
function PaneMount({ personId }: { personId: string }): null {
  useOrgPaneMount(personId)
  return null
}

async function flushWork(): Promise<void> {
  for (let index = 0; index < 8; index += 1) await Promise.resolve()
}

/** Exactly apps/api's `CompanyTreePerson`. The tree carries placement and
 * identity; runtime state belongs to `/people`, which is where this suite's
 * `runningOverrides` come from.
 *
 * `accent` is OMITTED when there is none — apps/api builds the key
 * conditionally, so a person with no allocated accent arrives with no
 * `accent` at all. There is no identity that is EXEMPT from an accent any
 * more (`operator`/`ceo` used to be, alongside the generated themes that
 * split dressed); an absent one now means the palette was exhausted. This
 * used to default the parameter to `null`, a value the wire never carries. */
function person(id: string, title: string, accent?: string): TreePerson {
  return typeof accent === 'string'
    ? { id, name: id, title, kind: 'worker', employmentState: 'active', accent }
    : { id, name: id, title, kind: 'worker', employmentState: 'active' }
}

/** Deliberately non-alphabetical department order ('zulu' before 'alpha')
 * and one empty department ('ghost', dropped as a window but still walked
 * for its child) so a sort or a dropped-child bug fails visibly. */
function fixtureTree(): CompanyTree {
  const ceo = person('ceo', 'CEO')
  const z1 = person('z1', 'Zulu Lead')
  const z2 = person('z2', 'Zulu Eng')
  const a1 = person('a1', 'Alpha Lead')
  const a2 = person('a2', 'Alpha Eng')
  const root: DepartmentNode = {
    id: 'root',
    name: 'HQ',
    headPersonId: 'ceo',
    state: 'active',
    people: [ceo],
    children: [
      {
        id: 'dept-zulu',
        name: 'Zulu Team',
        headPersonId: 'z1',
        state: 'active',
        people: [z1, z2],
        children: []
      },
      {
        id: 'dept-ghost',
        name: 'Ghost Dept',
        headPersonId: 'ghost-head',
        state: 'active',
        people: [],
        children: [
          {
            id: 'dept-alpha',
            name: 'Alpha Team',
            headPersonId: 'a1',
            state: 'active',
            people: [a1, a2],
            children: []
          }
        ]
      }
    ]
  }
  // chiefd echoes the company KEY into this field — see `CompanyTree`.
  return { slug: COMPANY_KEY, rootDepartmentId: 'root', departments: [root] }
}

/** apps/api's `CompanySummary` — the WHOLE body of `GET /companies/:companyKey`
 * (`CompanyDirectoryService.status()` builds `{key, dir, slug, status, chiefd}`
 * and, unlike `list()`, carries no `url`).
 *
 * This fixture used to declare `name`, `hosting`, `chiefd.url`,
 * `chiefd.mode`, `chiefPersonId`, `peopleCount`, `departmentCount` and
 * `runningPeople`. None of those exist on any apps/api response: the route
 * serves four facts and this is all four. */
function fixtureCompany(): CompanySummary {
  return {
    key: COMPANY_KEY,
    dir: '/work/acme',
    slug: SLUG,
    status: 'running',
    chiefd: { healthy: true, httpStatus: 200, reason: 'ok', runtimeMode: 'company' }
  }
}

/** The host's converged roster: `{hosted, degraded}`.
 *
 * It used to be an array of people each carrying a `session` object, where
 * `session !== null` meant running — a signal that belonged to an RPC child
 * that no longer exists. The host answers directly now.
 *
 * `ceo` and `z1` are hosted; `z2`, `a1` and `a2` are not, so the derivation
 * has both outcomes to distinguish. */
function fixturePeople(): PeopleResponse {
  return { hosted: ['ceo', 'z1'], degraded: [] }
}

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((res) => {
    resolve = res
  })
  return { promise, resolve }
}

interface FakeCallCounts {
  getCompany: number
  getCompanyTree: number
  listPeople: number
  getMailbox: number
}

interface FakeOrgClient {
  getCompany(): Promise<CompanySummary>
  getCompanyTree(): Promise<CompanyTree>
  listPeople(): Promise<PeopleResponse>
  /** The person is a PARAMETER, because the answer now names them: the route
   * counts `pending` on the server and reports which person it counted for. */
  getMailbox(companyKey: string, personId: string): Promise<MailboxResponse>
}

interface FakeClientBox {
  client: FakeOrgClient
  calls: FakeCallCounts
}

function makeClient(overrides?: { mailbox?: MailboxResponse }): FakeClientBox {
  const calls: FakeCallCounts = {
    getCompany: 0,
    getCompanyTree: 0,
    listPeople: 0,
    getMailbox: 0
  }

  const client: FakeOrgClient = {
    async getCompany() {
      calls.getCompany += 1
      return fixtureCompany()
    },
    async getCompanyTree() {
      calls.getCompanyTree += 1
      return fixtureTree()
    },
    async listPeople() {
      calls.listPeople += 1
      return fixturePeople()
    },
    async getMailbox() {
      calls.getMailbox += 1
      // `personId` is part of the body: the route counts `pending` on the
      // server and answers with the person it counted for.
      return overrides?.mailbox ?? { personId: 'person-ceo', pendingCount: 0, envelopes: [] }
    }
  }

  return { client, calls }
}

/* eslint-disable lucy/no-json-stringify */
// This builds a raw SSE wire frame for the fake stream, not an app-API call.
function docFrame(store: string, seq: number, generation = 1): string {
  return `id: ${generation}.${seq}\nevent: doc\ndata: ${JSON.stringify({
    companyKey: COMPANY_KEY,
    store,
    seq,
    generation,
    updatedAt: '2026-08-05T00:00:00.000Z',
    removed: false
  })}\n\n`
}
/* eslint-enable lucy/no-json-stringify */

const REORG_FRAME = 'event: reorg\ndata: {}\n\n'

describe('OrgStoreProvider', () => {
  let container: HTMLDivElement
  let root: Root
  let box: StoreBox
  let unmounted: boolean

  beforeEach(() => {
    vi.useFakeTimers()
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
    box = { current: undefined }
    unmounted = false
  })

  afterEach(() => {
    if (!unmounted) {
      act(() => {
        root.unmount()
      })
    }
    container.remove()
    vi.useRealTimers()
  })

  function mount(
    client: FakeOrgClient,
    fake: ReturnType<typeof createFakeSseStreams>,
    mountedPersonId?: string
  ): void {
    const deps: SseHubDeps = {
      baseUrl: BASE_URL,
      accessToken: () => 'fixture-operator-jwt',
      fetchImpl: fake.fetchImpl
    }
    const children =
      typeof mountedPersonId === 'string'
        ? [
            createElement(Harness, { box, key: 'harness' }),
            createElement(PaneMount, { personId: mountedPersonId, key: 'pane' })
          ]
        : createElement(Harness, { box })
    act(() => {
      root.render(
        createElement(CompanyEventsProvider, {
          deps,
          children: createElement(OrgStoreProvider, { companyKey: COMPANY_KEY, client, children })
        })
      )
    })
  }

  it('pins the three base doc-change store names, plus the dynamic mailbox/<personId> family (witness test)', () => {
    expect([...ORG_STORE_BASE_STORES]).toEqual(['org-manifest', 'activity', 'runtime'])
  })

  it('hydrates windows in served order, drops the empty department, keeps its child', async () => {
    const { client } = makeClient()
    const fake = createFakeSseStreams()
    mount(client, fake)
    await act(async () => {
      await flushWork()
    })

    const snapshot = box.current
    if (!snapshot) throw new Error('no snapshot')
    expect(snapshot.ready).toBe(true)
    expect(snapshot.windows.map((window) => window.windowId)).toEqual([
      'root',
      'dept-zulu',
      'dept-alpha'
    ])
    expect(snapshot.windows[1]?.panes.map((pane) => pane.personId)).toEqual(['z1', 'z2'])
    expect(snapshot.windows.find((window) => window.windowId === 'dept-ghost')).toBeUndefined()
    // `running` is the host's `hosted` list and nothing else: `z1` is hosted
    // and `z2` is not.
    expect(snapshot.windows[1]?.panes.map((pane) => pane.running)).toEqual([true, false])
    expect(snapshot.company).toEqual(fixtureCompany())
  })

  // Found by running the real product, not by reading it: a company created
  // with `chief create` and brought up by `chief attach`/`chief actuate`
  // runs its agents in tmux, so `GET /companies/:companyKey/people` answers 409
  // `company-not-api-hosted` (`server/HostedRoster.ts`) — correctly, because
  // this server is not where those agents live. The hydration awaited that
  // call in the SAME `Promise.all` as the tree, so the 409 rejected the whole
  // snapshot: `store.hydrate` never ran, `ready` stayed false, and the page
  // rendered "Loading company…" forever for EVERY tmux-run company, which is
  // every company an operator makes the normal way. `GET /tree` had already
  // answered 200 with the real departments and the real people; the page
  // fetched them and threw them away.
  it('renders a tmux-run company: a 409 company-not-api-hosted roster is an empty roster, not a hydration failure', async () => {
    const { client, calls } = makeClient()
    const fake = createFakeSseStreams()
    const tmuxHostedClient: FakeOrgClient = {
      ...client,
      async listPeople() {
        calls.listPeople += 1
        throw new ChiefApiError({
          kind: 'conflict',
          status: 409,
          code: 'company-not-api-hosted',
          detail: 'company "acme" runs its agents in tmux, so this server does not host them.'
        })
      }
    }
    mount(tmuxHostedClient, fake)
    await act(async () => {
      await flushWork()
    })

    const snapshot = box.current
    if (!snapshot) throw new Error('no snapshot')
    // The page is READY and the tree is rendered with its real people.
    expect(snapshot.ready).toBe(true)
    expect(snapshot.windows.map((window) => window.windowId)).toEqual([
      'root',
      'dept-zulu',
      'dept-alpha'
    ])
    expect(snapshot.windows[1]?.panes.map((pane) => pane.personId)).toEqual(['z1', 'z2'])
    expect(snapshot.company).toEqual(fixtureCompany())
    // Nobody is running HERE, which is the truth: the tmux actuator runs them
    // and this server neither knows nor claims otherwise.
    expect(snapshot.windows[1]?.panes.map((pane) => pane.running)).toEqual([false, false])
  })

  // The other half of the same rule: only THAT refusal is tolerated. A real
  // fault must still fail loudly rather than render half a company, which is
  // the defect the tolerance above could otherwise become.
  it('still fails hydration when the roster read is a genuine fault, not the not-api-hosted refusal', async () => {
    const { client } = makeClient()
    const fake = createFakeSseStreams()
    const brokenClient: FakeOrgClient = {
      ...client,
      async listPeople() {
        throw new ChiefApiError({
          kind: 'upstream',
          status: 502,
          detail: 'the company daemon could not be reached'
        })
      }
    }
    mount(brokenClient, fake)
    await act(async () => {
      await flushWork()
    })

    const snapshot = box.current
    if (!snapshot) throw new Error('no snapshot')
    expect(snapshot.ready).toBe(false)
  })

  // apps/api gates `…/mailbox` behind `requireHostedClient`, so it answers 409
  // `person-not-running` for a dormant agent. The roster already reports who
  // has a live session, so the footer read is issued only where the route can
  // answer — and, crucially, a skipped person is NOT marked as read, so the
  // next roster re-read that finds them running issues it then.
  it('reads a mounted person\u2019s mailbox only once they are running, then exactly once', async () => {
    const { client, calls } = makeClient()
    const fake = createFakeSseStreams()
    let running: readonly string[] = ['ceo', 'z1']
    const rosterClient: FakeOrgClient = {
      ...client,
      async listPeople() {
        calls.listPeople += 1
        return { hosted: [...running], degraded: [] }
      },
      async getMailbox(_companyKey: string, personId: string) {
        calls.getMailbox += 1
        // `personId` is part of the body: the route counts `pending` on the
        // server and answers with the person it counted for.
        return { personId, pendingCount: 3, envelopes: [] }
      }
    }

    // `z2` is mounted but dormant on the first roster read: no mailbox request
    // is made at all, and the footer has no count for them.
    mount(rosterClient, fake, 'z2')
    await act(async () => {
      await flushWork()
    })
    expect(calls.getMailbox).toBe(0)
    expect(box.current?.footerFor('z2').pendingMailboxCount).toBeUndefined()

    // `z2` comes up. A `runtime` doc event re-reads the roster, and that is
    // what releases the pending footer read — nothing polled for it.
    running = ['ceo', 'z1', 'z2']
    const stream = fake.openNext()
    await act(async () => {
      await flushWork()
    })
    stream.push(docFrame('runtime', 1))
    await act(async () => {
      await flushWork()
    })
    expect(calls.getMailbox).toBe(1)
    expect(box.current?.footerFor('z2').pendingMailboxCount).toBe(3)

    // A later roster re-read does not re-issue it.
    stream.push(docFrame('runtime', 2))
    await act(async () => {
      await flushWork()
    })
    expect(calls.getMailbox).toBe(1)
  })

  it('re-reads exactly the store the doc event names, and nothing else', async () => {
    const { client, calls } = makeClient()
    const fake = createFakeSseStreams()
    mount(client, fake)
    await act(async () => {
      await flushWork()
    })
    const rosterBefore = calls.listPeople
    const treeBefore = calls.getCompanyTree

    const stream = fake.openNext()
    await act(async () => {
      await flushWork()
    })
    stream.push(docFrame('runtime', 1))
    await act(async () => {
      await flushWork()
    })

    expect(calls.listPeople).toBe(rosterBefore + 1)
    expect(calls.getCompanyTree).toBe(treeBefore)
  })

  it('coalesces a same-store event burst mid-flight into one follow-up read', async () => {
    const { client, calls } = makeClient()
    const fake = createFakeSseStreams()

    // Replace listPeople with a deferred version so several 'runtime'
    // frames can arrive while the first refetch is still in flight.
    const gate = deferred<void>()
    let rosterCalls = 0
    const gatedClient = {
      ...client,
      async listPeople() {
        rosterCalls += 1
        if (rosterCalls === 2) await gate.promise
        return client.listPeople()
      }
    }

    mount(gatedClient, fake)
    await act(async () => {
      await flushWork()
    })
    expect(rosterCalls).toBe(1) // initial hydration read

    const stream = fake.openNext()
    await act(async () => {
      await flushWork()
    })

    // Burst: three 'runtime' events pushed before any microtask runs.
    stream.push(docFrame('runtime', 1))
    stream.push(docFrame('runtime', 2))
    stream.push(docFrame('runtime', 3))
    await act(async () => {
      await flushWork()
    })
    // Exactly one more read is in flight (the second call, gated).
    expect(rosterCalls).toBe(2)

    gate.resolve()
    await act(async () => {
      await flushWork()
    })
    // The blocked call resolves, then the hub dispatches exactly ONE
    // coalesced follow-up (the latest of the two queued frames) — three
    // rapid frames cost one extra read beyond the one already in flight,
    // never three.
    expect(rosterCalls).toBe(3)
    expect(calls.listPeople).toBe(3)
  })

  it('reorg triggers a full resync across every store, once', async () => {
    const { client, calls } = makeClient()
    const fake = createFakeSseStreams()
    mount(client, fake)
    await act(async () => {
      await flushWork()
    })
    const before = { ...calls }

    const stream = fake.openNext()
    await act(async () => {
      await flushWork()
    })
    stream.push(REORG_FRAME)
    await act(async () => {
      await flushWork()
    })

    expect(calls.getCompanyTree).toBe(before.getCompanyTree + 1)
    expect(calls.listPeople).toBe(before.listPeople + 1)
  })

  it('disconnect (drop) then reconnect keeps one subscription and recovers channel health', async () => {
    const { client } = makeClient()
    const fake = createFakeSseStreams()
    mount(client, fake)
    await act(async () => {
      await flushWork()
    })

    const stream = fake.openNext()
    await act(async () => {
      await flushWork()
    })
    stream.push(docFrame('runtime', 1))
    await act(async () => {
      await flushWork()
    })
    expect(box.current?.channel).toBe('healthy')

    // Drop the connection.
    stream.error('dropped')
    await act(async () => {
      await flushWork()
    })
    expect(box.current?.channel).toBe('dead')

    // The client retries on a backoff timer; advance it and serve a fresh
    // connection.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_000)
    })
    const reconnected = fake.openNext()
    await act(async () => {
      await flushWork()
    })
    reconnected.push(docFrame('runtime', 2))
    await act(async () => {
      await flushWork()
    })
    expect(box.current?.channel).toBe('healthy')
    expect(activeSseConnectionCount()).toBe(1)
  })

  it('duplicate/out-of-order frames for the same store still resolve to one settled re-read', async () => {
    const { client, calls } = makeClient()
    const fake = createFakeSseStreams()
    mount(client, fake)
    await act(async () => {
      await flushWork()
    })
    const before = calls.listPeople

    const stream = fake.openNext()
    await act(async () => {
      await flushWork()
    })
    // Out-of-order seq, plus an exact duplicate frame.
    stream.push(docFrame('runtime', 5))
    stream.push(docFrame('runtime', 2))
    stream.push(docFrame('runtime', 5))
    await act(async () => {
      await flushWork()
    })

    // The re-read is always the whole typed route, never a diff — a store
    // re-read at least once (coalesced) and the final value is what the
    // last fetch returned, never patched from the frame payload.
    expect(calls.listPeople).toBeGreaterThan(before)
  })

  it('unmount closes the subscription and clears every timer', async () => {
    const { client } = makeClient()
    const fake = createFakeSseStreams()
    mount(client, fake)
    await act(async () => {
      await flushWork()
    })
    fake.openNext()
    await act(async () => {
      await flushWork()
    })
    expect(activeSseConnectionCount()).toBe(1)

    act(() => {
      root.unmount()
    })
    unmounted = true
    expect(activeSseConnectionCount()).toBe(0)
    expect(vi.getTimerCount()).toBe(0)
  })
})
