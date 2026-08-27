'use client'

/**
 * The company view's ONE org store (E6-S7, #812, mandate 0). Loads the four
 * `GET /companies/:companyKey/…` snapshots once on mount, in parallel, abortably;
 * subscribes to the doc change stores below through `useCompanyEvents`
 * (E6-S4/S9's shared connection layer, `#809`); and re-reads exactly the
 * whole typed route the Contract names for each store — never a diff
 * applied from the event payload, never a timer. `useCompanyEvents`'s hub
 * already coalesces per store (`DocSseHubEntry.runDocCycle`, awaiting the
 * subscriber before dispatching the next queued event for that store), so
 * an event burst mid-refetch costs exactly one follow-up read.
 */
import {
  createContext,
  type ReactElement,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState
} from 'react'

import { useCompanyEvents } from '@/hooks/UseCompanyEvents'
import { useChiefApi } from '@/providers/ApiSessionProvider'
import type { ChiefApiClientService } from '@/services/ChiefApiClientService'
import { ChiefApiError } from '@/types/ApiErrors'
import type { CompanySummary, CompanyTree, PeopleResponse } from '@/types/ChiefApi'
import type { OrgStoreApi, OrgStoreProviderInternalApi, PersonFooterModel } from '@/types/OrgStore'
import type { DocChangeEvent, SseChannelState } from '@/types/Sse'
import { buildWindows } from '@/utils/OrgSelectors'

/** The six doc change stores this story re-reads from, verified against
 * chiefd rather than guessed (see the issue's Context section for the exact
 * source lines). A witness test in `OrgStoreProvider.test.tsx` pins this
 * list so a chiefd rename cannot silently stop the web from re-reading. */
export const ORG_STORE_BASE_STORES = ['org-manifest', 'activity', 'runtime'] as const

function mailboxStoreName(personId: string): string {
  return `mailbox/${personId}`
}

type OrgApiClient = Pick<
  ChiefApiClientService,
  'getCompany' | 'getCompanyTree' | 'listPeople' | 'getMailbox'
>

interface InternalSnapshot {
  ready: boolean
  tree: CompanyTree | undefined
  company: CompanySummary | undefined
  runningOverrides: Map<string, boolean>
  mailboxCounts: Map<string, number>
  channel: SseChannelState
}

function initialInternalSnapshot(): InternalSnapshot {
  return {
    ready: false,
    tree: undefined,
    company: undefined,
    runningOverrides: new Map(),
    mailboxCounts: new Map(),
    channel: 'connecting'
  }
}

/** A class-based external store (matching `PersonStreamStore`/`ChannelStore`
 * elsewhere in this app) so `useSyncExternalStore` gets one stable public
 * snapshot object per real change, never a fresh object per render. */
class OrgStore {
  private raw: InternalSnapshot = initialInternalSnapshot()
  private cached: OrgStoreApi | undefined
  private readonly listeners = new Set<() => void>()

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  readonly getSnapshot = (): OrgStoreApi => {
    if (!this.cached) this.cached = this.rebuild()
    return this.cached
  }

  setChannel(channel: SseChannelState): void {
    if (this.raw.channel === channel) return
    this.raw = { ...this.raw, channel }
    this.publish()
  }

  hydrate(input: {
    company: CompanySummary
    tree: CompanyTree
    running: ReadonlyMap<string, boolean>
  }): void {
    this.raw = {
      ...this.raw,
      ready: true,
      company: input.company,
      tree: input.tree,
      runningOverrides: new Map(input.running)
    }
    this.publish()
  }

  setTree(tree: CompanyTree): void {
    this.raw = { ...this.raw, tree }
    this.publish()
  }

  setRunning(running: ReadonlyMap<string, boolean>): void {
    this.raw = { ...this.raw, runningOverrides: new Map(running) }
    this.publish()
  }

  /** The roster's own running answer for one person (`/people`'s
   * `session !== null`). Read by the mailbox path, which cannot ask apps/api
   * for a dormant person's mailbox — see `refetchMailbox`. */
  isRunning(personId: string): boolean {
    return this.raw.runningOverrides.get(personId) ?? false
  }

  setMailboxCount(personId: string, count: number): void {
    const mailboxCounts = new Map(this.raw.mailboxCounts)
    mailboxCounts.set(personId, count)
    this.raw = { ...this.raw, mailboxCounts }
    this.publish()
  }

  private publish(): void {
    this.cached = undefined
    for (const listener of this.listeners) listener()
  }

  private rebuild(): OrgStoreApi {
    const raw = this.raw
    const windows = raw.tree ? buildWindows(raw.tree.departments, raw.runningOverrides) : []
    const footerFor = (personId: string): PersonFooterModel => ({
      pendingMailboxCount: raw.mailboxCounts.get(personId)
    })
    return {
      ready: raw.ready,
      windows,
      // The served forest, unprojected — `windows` drops people-less
      // departments and the structure editor needs them.
      departments: raw.tree?.departments ?? [],
      company: raw.company,
      footerFor,
      channel: raw.channel
    }
  }
}

export const OrgStoreContext = createContext<OrgStoreProviderInternalApi | undefined>(undefined)

/** The roster's running signal, straight from the host.
 *
 * `GET /api/companies/:companyKey/people` answers `{hosted, degraded}` — the roster
 * this server actually converged. It used to answer an array of people each
 * carrying a `session` object, where `session !== null` meant running; that
 * signal belonged to an RPC child that no longer exists.
 *
 * Only `hosted` produces `true`. Anybody absent from it is absent rather than
 * mapped to `false`, because this map answers "who is up here", never "who is
 * down and why". */
function runningByPersonId(people: PeopleResponse): Map<string, boolean> {
  return new Map(people.hosted.map((personId) => [personId, true]))
}

/** The roster of people THIS server hosts, and the empty answer that is
 * correct rather than fatal.
 *
 * A company whose agents run in tmux hosts nobody here, and
 * `GET /companies/:companyKey/people` says so with a 409 `company-not-api-hosted`
 * (`server/HostedRoster.ts`). That refusal is right — it is the difference
 * between "the daemon is down" and "the daemon is fine and this is not where
 * its agents live" — but it is not a hydration failure, and treating it as one
 * is what this function exists to stop.
 *
 * Measured live, not reasoned about: on a company created with `chief create`
 * and brought up by `chief attach`/`chief actuate`, the company page issued
 * `GET /tree` (200, the real departments and the real people) beside
 * `GET /people` (409), and because the two were awaited in one `Promise.all`
 * the 409 rejected the whole snapshot. `store.hydrate` never ran, `ready`
 * stayed false, and `CompanyView` rendered "Loading company…" FOREVER — for
 * every tmux-run company, which is every company an operator makes the normal
 * way. The tree was fetched correctly and then thrown away.
 *
 * So exactly this refusal, and no other, degrades to the empty roster it
 * actually means: nobody is hosted HERE. `hosted` is empty, so
 * {@link runningByPersonId} marks nobody running, which is the truth — the
 * tmux actuator runs them, and this server neither knows nor claims otherwise.
 * Every other failure (an unreachable daemon, a malformed envelope, a 500)
 * still rejects, because those really are hydration failures and a page that
 * silently renders half a company is the defect this whole file's comments
 * keep describing. */
async function listPeopleHostedHere(
  client: Pick<OrgApiClient, 'listPeople'>,
  companyKey: string
): Promise<PeopleResponse> {
  try {
    return await client.listPeople(companyKey)
  } catch (error: unknown) {
    if (isNotApiHostedRefusal(error)) return { hosted: [], degraded: [] }
    throw error
  }
}

/** The one refusal above, identified by chiefd's own code rather than by
 * matching prose — reconstructing a refusal's meaning from its text is the
 * mechanism that rotted last time. The status is checked too so a future code
 * reuse under a different status cannot quietly widen this. */
function isNotApiHostedRefusal(error: unknown): boolean {
  return (
    error instanceof ChiefApiError &&
    error.status === 409 &&
    error.code === 'company-not-api-hosted'
  )
}

function storeNamesFor(mounted: ReadonlySet<string>): string[] {
  return [...ORG_STORE_BASE_STORES, ...[...mounted].map(mailboxStoreName)]
}

function InnerOrgStoreProvider(props: {
  companyKey: string
  client: OrgApiClient
  children: ReactNode
}): ReactElement {
  const { companyKey, client } = props
  const storeRef = useRef<OrgStore | undefined>(undefined)
  if (typeof storeRef.current === 'undefined') storeRef.current = new OrgStore()
  const store = storeRef.current
  const [mountedPersons, setMountedPersons] = useState<ReadonlySet<string>>(new Set())
  const mountedRef = useRef(true)

  const registerMountedPerson = useCallback((personId: string): void => {
    setMountedPersons((previous) => {
      if (previous.has(personId)) return previous
      const next = new Set(previous)
      next.add(personId)
      return next
    })
  }, [])
  const unregisterMountedPerson = useCallback((personId: string): void => {
    setMountedPersons((previous) => {
      if (!previous.has(personId)) return previous
      const next = new Set(previous)
      next.delete(personId)
      return next
    })
  }, [])

  const refetchTree = useCallback(async (): Promise<void> => {
    const tree = await client.getCompanyTree(companyKey)
    store.setTree(tree)
  }, [client, companyKey, store])

  const refetchMailbox = useCallback(
    async (personId: string): Promise<void> => {
      // apps/api gates `…/mailbox` behind `requireHostedClient`, so it answers
      // 409 `person-not-running` for a dormant agent. The roster already told
      // us which people have a live session, so this asks only where the route
      // can answer instead of firing a request whose refusal is certain.
      if (!store.isRunning(personId)) return
      const mailbox = await client.getMailbox(companyKey, personId)
      store.setMailboxCount(personId, mailbox.pendingCount)
    },
    [client, companyKey, store]
  )

  // The mount set is read inside `fetchPendingMailboxes`, which a roster
  // re-read also calls — a ref keeps that callback stable rather than
  // rebuilding `refetchRunning` every time a pane mounts.
  const mountedPersonsRef = useRef<ReadonlySet<string>>(mountedPersons)
  mountedPersonsRef.current = mountedPersons
  const fetchedMailboxRef = useRef<Set<string>>(new Set())

  /** One mailbox read per mounted person, issued as soon as that person is
   * BOTH mounted and running — the two conditions apps/api needs to be able
   * to answer at all (issue #404: the single fetch this makes is what keeps
   * footer freshness observable in a unit test rather than only over a live
   * multi-day daemon connection). A person who is mounted but dormant is NOT
   * marked as fetched, so the next roster re-read that finds them running
   * issues the read then — pushed by a `runtime` doc event, with no timer and
   * no retry loop (mandate 1). */
  const fetchPendingMailboxes = useCallback((): void => {
    for (const personId of mountedPersonsRef.current) {
      if (fetchedMailboxRef.current.has(personId)) continue
      if (!store.isRunning(personId)) continue
      fetchedMailboxRef.current.add(personId)
      void refetchMailbox(personId).catch(() => undefined)
    }
  }, [refetchMailbox, store])

  const refetchRunning = useCallback(async (): Promise<void> => {
    const people = await listPeopleHostedHere(client, companyKey)
    store.setRunning(runningByPersonId(people))
    fetchPendingMailboxes()
  }, [client, fetchPendingMailboxes, companyKey, store])

  // Initial parallel, abortable snapshot load — the store's only fetch that
  // is not triggered by a doc change event.
  useEffect(() => {
    mountedRef.current = true
    let cancelled = false
    void (async () => {
      const [company, tree, people] = await Promise.all([
        client.getCompany(companyKey),
        client.getCompanyTree(companyKey),
        listPeopleHostedHere(client, companyKey)
      ])
      if (cancelled || !mountedRef.current) return
      store.hydrate({
        company,
        tree,
        running: runningByPersonId(people)
      })
    })().catch((error: unknown) => {
      /* eslint-disable lucy/no-console-usage */
      // apps/web has no shared logger package (there is no `utils/Logger`
      // here), and a hydration failure has to be visible somewhere: it leaves
      // `ready: false`, and CompanyView renders "Loading company…" for that
      // state — forever, with nothing in the console. Swallowing this
      // silently made a broken company page and a slow one look identical.
      // The store still owns the UI decision; this only refuses to discard
      // the reason.
      console.error('company hydration failed', error)
      /* eslint-enable lucy/no-console-usage */
    })
    return () => {
      cancelled = true
      mountedRef.current = false
    }
  }, [client, companyKey, store])

  const onDoc = useCallback(
    async (event: DocChangeEvent): Promise<void> => {
      if (event.store === 'org-manifest' || event.store === 'activity') {
        await refetchTree()
        return
      }
      if (event.store === 'runtime') {
        await refetchRunning()
        return
      }
      if (event.store.startsWith('mailbox/')) {
        const personId = event.store.slice('mailbox/'.length)
        if (mountedPersons.has(personId)) await refetchMailbox(personId)
      }
    },
    [mountedPersons, refetchMailbox, refetchRunning, refetchTree]
  )

  const onReorg = useCallback((): void => {
    void Promise.all([
      refetchTree(),
      refetchRunning(),
      ...[...mountedPersons].map((personId) => refetchMailbox(personId))
    ]).catch(() => undefined)
  }, [mountedPersons, refetchMailbox, refetchRunning, refetchTree])

  const storeNames = useMemo(() => storeNamesFor(mountedPersons), [mountedPersons])
  const channel = useCompanyEvents(companyKey, storeNames, { onDoc, onReorg })

  useEffect(() => {
    store.setChannel(channel)
  }, [channel, store])

  // A newly mounted pane fetches its own footer mailbox count exactly once,
  // independent of the next mailbox doc event (issue #404: this makes
  // footer freshness observable in a unit test — assert the one fetch this
  // effect issues — rather than only over a live multi-day daemon
  // connection where a stale footer had no reproducible trigger).
  useEffect(() => {
    fetchPendingMailboxes()
  }, [fetchPendingMailboxes, mountedPersons])

  const internalApi = useMemo<OrgStoreProviderInternalApi>(
    () => ({ store, registerMountedPerson, unregisterMountedPerson }),
    [store, registerMountedPerson, unregisterMountedPerson]
  )

  return <OrgStoreContext.Provider value={internalApi}>{props.children}</OrgStoreContext.Provider>
}

function AuthenticatedOrgStoreProvider(props: {
  companyKey: string
  children: ReactNode
}): ReactElement {
  const client = useChiefApi()
  return (
    <InnerOrgStoreProvider companyKey={props.companyKey} client={client}>
      {props.children}
    </InnerOrgStoreProvider>
  )
}

/** Production children read the injected `ChiefApiClientService`; tests pass
 * their own narrow client (`FakeChiefApi`'s fetch behind the real service,
 * or a hand-rolled stub) via `client`. Mirrors `ApiSessionProvider`'s
 * injected-vs-authenticated split so no hook is ever called conditionally. */
export function OrgStoreProvider(props: {
  companyKey: string
  children: ReactNode
  client?: OrgApiClient
}): ReactElement {
  if (props.client) {
    return (
      <InnerOrgStoreProvider companyKey={props.companyKey} client={props.client}>
        {props.children}
      </InnerOrgStoreProvider>
    )
  }
  return (
    <AuthenticatedOrgStoreProvider companyKey={props.companyKey}>
      {props.children}
    </AuthenticatedOrgStoreProvider>
  )
}
