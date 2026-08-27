// @vitest-environment jsdom
import {
  ACME_DAEMON_URL,
  createFakeChiefApi,
  FIXTURE_COMPANY_KEY,
  FIXTURE_JWT,
  GLOBEX_DAEMON_URL
} from '@test/harness/FakeChiefApi'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { useCompanyDirectoryWithClient } from '@/hooks/UseCompanyDirectory'
import { ChiefApiClientService } from '@/services/ChiefApiClientService'
import type { CompaniesResponse, StopResponse } from '@/types/ChiefApi'
import type { FetchImpl } from '@/types/Fetch'
import type { SseHubDeps } from '@/types/Sse'

const BASE_URL = 'http://fake-api.test'

type DirectoryHook = ReturnType<typeof useCompanyDirectoryWithClient>

interface DirectoryBox {
  current: DirectoryHook | undefined
}

interface DirectoryClient {
  listCompanies(signal?: AbortSignal): Promise<CompaniesResponse>
  stopCompany(companyKey: string, signal?: AbortSignal): Promise<StopResponse>
}

function Harness({
  client,
  deps,
  box
}: {
  client: DirectoryClient
  deps: SseHubDeps
  box: DirectoryBox
}): null {
  box.current = useCompanyDirectoryWithClient(client, deps)
  return null
}

async function flushWork(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

describe('useCompanyDirectoryWithClient', () => {
  let container: HTMLDivElement
  let root: Root

  beforeEach(() => {
    vi.useFakeTimers()
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
  })

  afterEach(() => {
    act(() => {
      root.unmount()
    })
    container.remove()
    vi.useRealTimers()
  })

  it('lists through apps/api, re-reads after stop, and never addresses a fixture daemon', async () => {
    const fake = createFakeChiefApi()
    const urls: string[] = []
    const fetchImpl: FetchImpl = async (input, init) => {
      urls.push(typeof input === 'string' ? input : input.toString())
      return fake.fetchImpl(input, init)
    }
    const client = new ChiefApiClientService({
      baseUrl: BASE_URL,
      accessToken: () => FIXTURE_JWT,
      fetchImpl
    })
    const deps: SseHubDeps = { baseUrl: BASE_URL, accessToken: () => FIXTURE_JWT, fetchImpl }
    const box: DirectoryBox = { current: undefined }

    await act(async () => {
      root.render(<Harness box={box} client={client} deps={deps} />)
      await flushWork()
    })
    const initial = box.current
    if (!initial) throw new Error('directory hook did not mount')
    expect(initial.companies.map((company) => company.slug)).toEqual(['acme', 'globex'])
    expect(initial.loading).toBe(false)

    await act(async () => {
      await initial.stop(FIXTURE_COMPANY_KEY)
      await flushWork()
    })
    const refreshed = box.current
    if (!refreshed) throw new Error('directory hook did not keep its snapshot')
    // Found by KEY, because that is what `stop` was given: a slug would match
    // whichever company the registry happened to list first.
    expect(refreshed.companies.find((company) => company.key === FIXTURE_COMPANY_KEY)?.status).toBe(
      'stopped'
    )
    expect(urls.every((url) => url.startsWith(BASE_URL))).toBe(true)
    expect(urls.some((url) => url.startsWith(ACME_DAEMON_URL))).toBe(false)
    expect(urls.some((url) => url.startsWith(GLOBEX_DAEMON_URL))).toBe(false)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('submits the create spec once, receives opaque phases, and refreshes after the terminal', async () => {
    const fake = createFakeChiefApi()
    const client = new ChiefApiClientService({
      baseUrl: BASE_URL,
      accessToken: () => FIXTURE_JWT,
      fetchImpl: fake.fetchImpl
    })
    const deps: SseHubDeps = {
      baseUrl: BASE_URL,
      accessToken: () => FIXTURE_JWT,
      fetchImpl: fake.fetchImpl
    }
    const box: DirectoryBox = { current: undefined }
    await act(async () => {
      root.render(<Harness box={box} client={client} deps={deps} />)
      await flushWork()
    })
    const directory = box.current
    if (!directory) throw new Error('directory hook did not mount')
    const phases: string[] = []

    let terminal: unknown
    await act(async () => {
      terminal = await directory.create(
        { name: 'New Company', purpose: 'Exercise the create flow' },
        (frame) => phases.push(frame.phase)
      )
      await flushWork()
    })

    expect(terminal).toEqual({ kind: 'created', slug: 'new-company' })
    expect(phases).toEqual(['company-daemon-start', 'unrecognized-fixture-phase'])
    const createRequest = fake.requests.find(
      (request) => request.method === 'POST' && request.path === '/companies'
    )
    // `{name, purpose}` and nothing else. This asserted a `{spec:{…}}`
    // envelope carrying a CEO and a department list — a body no version of
    // apps/api's schema ever accepted, which is why every create from the
    // browser failed validation before any lifecycle code ran. What a company
    // IS at birth is chiefd's decision, so the browser sends only what the
    // operator typed.
    expect(createRequest?.body).toEqual({
      name: 'New Company',
      purpose: 'Exercise the create flow'
    })
    expect(
      fake.requests.filter((request) => request.method === 'GET' && request.path === '/companies')
    ).toHaveLength(2)
    expect(vi.getTimerCount()).toBe(0)
  })

  // REWRITTEN CONTRACT. This case used to be "…preserves the tmux refusal
  // detail from the stop endpoint" and stopped `globex`, a fixture company
  // marked `hosting: 'tmux'`, expecting a 409 `company-not-api-hosted` /
  // "company is hosted by tmux". Both halves were fiction: apps/api's
  // lifecycle routes have no hosting branch (that 409 comes from
  // `AgentTalkService.requireApiHosted`, on the live verbs), and no route
  // serves a `hosting` field at all. What is preserved is the assertion that
  // actually mattered — the hook surfaces the stop endpoint's error envelope
  // verbatim with the right taxonomy kind — now driven by a refusal apps/api
  // really produces: `UnknownResourceError` -> 404 `unknown-resource`.
  it('uses the boot stream and preserves the stop endpoint refusal verbatim', async () => {
    const fake = createFakeChiefApi()
    const client = new ChiefApiClientService({
      baseUrl: BASE_URL,
      accessToken: () => FIXTURE_JWT,
      fetchImpl: fake.fetchImpl
    })
    const deps: SseHubDeps = {
      baseUrl: BASE_URL,
      accessToken: () => FIXTURE_JWT,
      fetchImpl: fake.fetchImpl
    }
    const box: DirectoryBox = { current: undefined }
    await act(async () => {
      root.render(<Harness box={box} client={client} deps={deps} />)
      await flushWork()
    })
    const directory = box.current
    if (!directory) throw new Error('directory hook did not mount')
    const phases: string[] = []

    let terminal: unknown
    await act(async () => {
      terminal = await directory.boot(FIXTURE_COMPANY_KEY, (frame) => phases.push(frame.phase))
      await flushWork()
    })
    expect(terminal).toEqual({ kind: 'booted', slug: 'acme' })
    expect(phases).toEqual(['chief-start'])
    expect(
      fake.requests.some((request) => request.path === `/companies/${FIXTURE_COMPANY_KEY}/boot`)
    ).toBe(true)

    let refusal: unknown
    await act(async () => {
      try {
        await directory.stop('no-such-company')
      } catch (error) {
        refusal = error
      }
    })
    expect(refusal).toMatchObject({
      kind: 'not-found',
      code: 'unknown-resource',
      detail: 'unknown company: no-such-company'
    })
    expect(vi.getTimerCount()).toBe(0)
  })

  it('exposes a scripted lifecycle failure verbatim and still re-reads the directory', async () => {
    const fake = createFakeChiefApi({
      lifecycle: {
        create: {
          phases: [{ phase: 'durable-create', detail: 'company exists now' }],
          terminal: {
            event: 'failed',
            error: { code: 'chief-start-failed', detail: 'CEO could not start' }
          }
        }
      }
    })
    const client = new ChiefApiClientService({
      baseUrl: BASE_URL,
      accessToken: () => FIXTURE_JWT,
      fetchImpl: fake.fetchImpl
    })
    const deps: SseHubDeps = {
      baseUrl: BASE_URL,
      accessToken: () => FIXTURE_JWT,
      fetchImpl: fake.fetchImpl
    }
    const box: DirectoryBox = { current: undefined }
    await act(async () => {
      root.render(<Harness box={box} client={client} deps={deps} />)
      await flushWork()
    })
    const directory = box.current
    if (!directory) throw new Error('directory hook did not mount')
    const phases: string[] = []

    let failure: unknown
    await act(async () => {
      try {
        await directory.create(
          { name: 'Acme Two', purpose: 'Exercise failed boot recovery' },
          (frame) => {
            phases.push(frame.phase)
          }
        )
      } catch (error) {
        failure = error
      }
      await flushWork()
    })

    expect(phases).toEqual(['durable-create'])
    expect(failure).toMatchObject({ code: 'chief-start-failed', detail: 'CEO could not start' })
    expect(
      fake.requests.filter((request) => request.method === 'GET' && request.path === '/companies')
    ).toHaveLength(2)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('aborts an in-flight directory request when its consumer unmounts', async () => {
    let observedSignal: AbortSignal | undefined
    const client: DirectoryClient = {
      listCompanies: async (signal?: AbortSignal) =>
        new Promise<CompaniesResponse>((_resolve, reject) => {
          observedSignal = signal
          signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')))
        }),
      stopCompany: async () => ({ stopped: true as const })
    }
    const deps: SseHubDeps = { baseUrl: BASE_URL, accessToken: () => FIXTURE_JWT }
    const box: DirectoryBox = { current: undefined }

    act(() => {
      root.render(<Harness box={box} client={client} deps={deps} />)
    })
    expect(observedSignal).toBeDefined()
    act(() => {
      root.unmount()
    })
    expect(observedSignal?.aborted).toBe(true)
    expect(vi.getTimerCount()).toBe(0)
  })
})
