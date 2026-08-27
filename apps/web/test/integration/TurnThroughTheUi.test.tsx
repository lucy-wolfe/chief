// @vitest-environment jsdom
/**
 * An operator types into the composer and the agent gets exactly those words.
 *
 * # Why this test exists, and why the existing ones could not catch it
 *
 * Everything between the browser and the harness had unit coverage and all of
 * it was green while nothing worked end to end. Three separate defects lived
 * in the same blind spot:
 *
 *   - the browser sent `{"message": …}` and the route read `body.text`, so
 *     every message an operator typed came back `422 empty-message`;
 *   - the browser's response schema expected `{queued: true, generation}` —
 *     apps/api's fire-and-forget acknowledgement — while the route returns the
 *     agent's actual answer, so a SUCCESSFUL turn threw a ZodError;
 *   - `abort` had the same mismatch in the other direction.
 *
 * No fake can find any of those, because a fake is written from the client and
 * answers whatever the client sent. `CompanyShellMvp` mounts the real shell
 * against `FakeChiefApi`, which is the right test for the shell and structurally
 * blind here.
 *
 * So this fixture drives the UI'S OWN CLIENT — the real `PaneComposer`, the
 * real `useAgentConversation`, the real `ChiefApiClientService` — and dispatches
 * its `fetch` into the REAL route handler modules. The only thing faked is the
 * seam the routes themselves cannot own in a test: chiefd's roster and the Pi
 * harness behind it. Everything in between is the shipped code.
 *
 * What that buys: the request body, the URL, the response shape and the error
 * envelope are all checked against the code that actually serves them. A change
 * to either half that the other does not know about fails here.
 */
import { createFakeSseStreams } from '@test/harness/FakeSseStreams'
import { act, createElement, type ReactElement, type ReactNode } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('next/navigation', () => ({
  useRouter: () => ({ replace: vi.fn() }),
  usePathname: () => '/c/acme',
  useSearchParams: () => new URLSearchParams()
}))

/** Every turn the fake harness was asked to run, in order.
 *
 * Declared with `vi.hoisted` because the module factory below is hoisted above
 * ordinary `const`s, and a recorder the factory closes over must exist by then. */
interface HarnessRecorder {
  prompts: string[]
  steers: string[]
  followUps: string[]
  aborts: number
  reply: string
  /** The stop reason of the next turn. `error` is a provider failure. */
  stopReason: 'stop' | 'error'
}

const harness: HarnessRecorder = vi.hoisted(() => ({
  prompts: [],
  steers: [],
  followUps: [],
  aborts: 0,
  reply: 'ready',
  stopReason: 'stop'
}))

/**
 * The roster, faked at chiefd's boundary and nowhere closer.
 *
 * `agentFor` is the exact seam where this process stops deciding and chiefd
 * starts. Mocking anything nearer the route — `say` itself, say — would test
 * the fixture rather than the product.
 */
vi.mock('@/server/HostedRoster', () => ({
  agentFor: async () => ({
    prompt: async (text: string) => {
      harness.prompts.push(text)
      return {
        role: 'assistant',
        content: [{ type: 'text', text: harness.reply }],
        stopReason: harness.stopReason,
        errorMessage: harness.stopReason === 'error' ? 'Connection error.' : undefined
      }
    },
    steer: async (text: string) => void harness.steers.push(text),
    followUp: async (text: string) => void harness.followUps.push(text),
    abort: async () => {
      harness.aborts += 1
      return { clearedSteer: [], clearedFollowUp: [] }
    }
  })
}))

vi.mock('@/server/AgentHost', () => ({
  hostedSession: () => ({
    getBranch: async () => []
  })
}))

import { POST as abortRoute } from '@/app/api/companies/[companyKey]/people/[personId]/abort/route'
import { POST as sayRoute } from '@/app/api/companies/[companyKey]/people/[personId]/say/route'
import { GET as transcriptRoute } from '@/app/api/companies/[companyKey]/people/[personId]/transcript/route'
import { AgentPane } from '@/components/pane/AgentPane'
import { ApiSessionProvider } from '@/providers/ApiSessionProvider'
import { CompanyEventsProvider } from '@/providers/CompanyEventsProvider'
import { ChiefApiClientService } from '@/services/ChiefApiClientService'
import type { SseHubDeps } from '@/types/Sse'
import { isNullish } from '@/utils/Nullish'

const BASE_URL = 'http://web.test'
const COMPANY_KEY = 'acme'
const PERSON = 'ceo'

/** Every request the client actually issued, as it issued it. */
interface DialledRequest {
  readonly method: string
  readonly path: string
  readonly body: string
}

const dialled: DialledRequest[] = []

/**
 * The browser's `fetch`, wired to this app's real route handlers.
 *
 * Next resolves `[companyKey]`/`[personId]` from the URL and hands them to the
 * handler as `context.params`. That resolution is Next's, so it is reproduced
 * here rather than mocked away — and reproduced from the URL the client built,
 * which is the whole point: a client that dialled the wrong path would find no
 * handler here exactly as it finds no route in production.
 */
async function serve(input: string | URL | Request, init?: RequestInit): Promise<Response> {
  const url = new URL(typeof input === 'string' ? input : input.toString(), BASE_URL)
  const method = init?.method ?? 'GET'
  const body = typeof init?.body === 'string' ? init.body : ''
  dialled.push({ method, path: url.pathname, body })

  const request = new Request(url, {
    method,
    ...(body === '' ? {} : { body }),
    headers: { 'content-type': 'application/json' }
  })
  const params = Promise.resolve({ companyKey: COMPANY_KEY, personId: PERSON })

  const personBase = `/api/companies/${COMPANY_KEY}/people/${PERSON}`
  if (method === 'POST' && url.pathname === `${personBase}/say`) {
    return sayRoute(request, { params })
  }
  if (method === 'POST' && url.pathname === `${personBase}/abort`) {
    return abortRoute(request, { params })
  }
  if (method === 'GET' && url.pathname === `${personBase}/transcript`) {
    return transcriptRoute(request, { params })
  }
  // Anything else is a path this app does not serve, reported the way Next
  // reports it. A client dialling one must fail, not be quietly satisfied.
  return new Response('Not Found', { status: 404 })
}

function App(props: {
  children: ReactNode
  client: ChiefApiClientService
  deps: SseHubDeps
}): ReactElement {
  return createElement(ApiSessionProvider, {
    client: props.client,
    tokenGetter: () => null,
    children: createElement(CompanyEventsProvider, {
      deps: props.deps,
      children: props.children
    })
  })
}

async function flushWork(): Promise<void> {
  for (let index = 0; index < 20; index += 1) await Promise.resolve()
}

describe('a turn typed into the composer reaches the agent', () => {
  let container: HTMLDivElement
  let root: Root
  let sse: ReturnType<typeof createFakeSseStreams>

  function mount(): void {
    const client = new ChiefApiClientService({
      baseUrl: BASE_URL,
      fetchImpl: serve,
      accessToken: () => null
    })
    sse = createFakeSseStreams()
    const deps: SseHubDeps = {
      baseUrl: BASE_URL,
      accessToken: () => null,
      fetchImpl: sse.fetchImpl
    }
    act(() => {
      root.render(
        createElement(App, {
          client,
          deps,
          children: createElement(AgentPane, {
            companyKey: COMPANY_KEY,
            pane: { paneId: PERSON, title: 'CEO', accentColor: null, kind: 'person' },
            readOnly: false
          })
        })
      )
    })
  }

  function composer(): HTMLTextAreaElement {
    const field = container.querySelector('textarea')
    if (isNullish(field)) throw new Error('the composer has no text field')
    return field
  }

  function button(text: string): HTMLButtonElement {
    const found = Array.from(container.querySelectorAll('button')).find((entry) =>
      entry.textContent?.toLowerCase().includes(text.toLowerCase())
    )
    if (typeof found === 'undefined') throw new Error(`no button matching "${text}"`)
    return found
  }

  /** Type `text` the way a person does: set the value, then let React see it. */
  async function type(text: string): Promise<void> {
    const field = composer()
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
    await act(async () => {
      setter?.call(field, text)
      field.dispatchEvent(new Event('input', { bubbles: true }))
      await flushWork()
    })
  }

  async function send(): Promise<void> {
    await act(async () => {
      button('send').click()
      await flushWork()
    })
  }

  beforeEach(() => {
    harness.prompts.length = 0
    harness.steers.length = 0
    harness.followUps.length = 0
    harness.aborts = 0
    harness.reply = 'ready'
    harness.stopReason = 'stop'
    dialled.length = 0
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
  })

  afterEach(() => {
    act(() => root.unmount())
    container.remove()
  })

  it('delivers the operator’s exact words to the harness', async () => {
    mount()
    await act(async () => {
      await flushWork()
    })
    await type('Reply with the single word: ready.')
    await send()

    // The words themselves, not merely "a turn happened". The defect this
    // replaces delivered an EMPTY string to a route that then refused it.
    expect(harness.prompts).toEqual(['Reply with the single word: ready.'])
  })

  it('dials the path this app actually serves, with the field the route reads', async () => {
    mount()
    await act(async () => {
      await flushWork()
    })
    await type('hello')
    await send()

    const say = dialled.find((request) => request.path.endsWith('/say'))
    expect(say?.method).toBe('POST')
    // The `/api` prefix, spelled out. It has been missing twice.
    expect(say?.path).toBe('/api/companies/acme/people/ceo/say')
    // The FIELD NAME. This is the assertion that fails if either half is
    // renamed without the other.
    expect(JSON.parse(say?.body ?? '{}')).toMatchObject({ text: 'hello', mode: 'prompt' })
  })

  it('accepts the answer the route actually returns', async () => {
    mount()
    await act(async () => {
      await flushWork()
    })
    await type('hello')
    await send()
    await act(async () => {
      await flushWork()
    })

    // A schema mismatch surfaces as a pane error even though the turn ran, so
    // the absence of one is the assertion: the client parsed what the server
    // sent. `{queued: true, generation}` against `{personId, mode, reply}`
    // failed exactly here while both halves' own tests passed.
    expect(container.querySelector('[role="alert"]')).toBeNull()
  })

  it('surfaces a provider failure instead of showing an empty answer', async () => {
    harness.stopReason = 'error'
    mount()
    await act(async () => {
      await flushWork()
    })
    await type('hello')
    await send()
    await act(async () => {
      await flushWork()
    })

    // The turn "succeeded" with `reply: ""` and a 200 before this. An operator
    // watching an agent say nothing, twice, had no way to tell a broken route
    // from a quiet agent.
    const alert = container.querySelector('[role="alert"]')
    expect(alert?.textContent).toContain('turn-failed')
    expect(alert?.textContent).toContain('Connection error.')
  })

  it('sends a steer to the harness’s steer queue, not as a new turn', async () => {
    mount()
    await act(async () => {
      await flushWork()
    })
    // The mode picker exists only DURING a turn, which is right: steering an
    // idle agent is not a thing. So the stream has to say a turn is running
    // before the control the operator would use is even on screen.
    const stream = sse.openMatching((url) => url.includes('/people/'))
    await act(async () => {
      stream.push('event: state\ndata: {"isStreaming":true,"pendingMessageCount":0}\n\n')
      await flushWork()
    })
    const modeSelect = container.querySelector<HTMLSelectElement>(
      'select[aria-label="Message mode"]'
    )
    expect(modeSelect).not.toBeNull()
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value')?.set
      setter?.call(modeSelect, 'steer')
      modeSelect?.dispatchEvent(new Event('change', { bubbles: true }))
      await flushWork()
    })
    await type('actually, stop')
    await send()

    // The three modes were sent and ignored: every one ran as an ordinary
    // prompt, so correcting an agent mid-turn started a second turn instead.
    expect(harness.steers).toEqual(['actually, stop'])
    expect(harness.prompts).toEqual([])
  })
})
