import {
  ACME_DAEMON_URL,
  createFakeChiefApi,
  FIXTURE_COMPANY_KEY,
  FIXTURE_JWT,
  GLOBEX_DAEMON_URL
} from '@test/harness/FakeChiefApi'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { ChiefApiClientService } from '@/services/ChiefApiClientService'
import { ChiefApiError } from '@/types/ApiErrors'
import type { FetchImpl } from '@/types/Fetch'

function json(value: unknown): string {
  /* eslint-disable lucy/no-json-stringify */
  // Test-only fixture serialization; @tribes-terminal/foundation is not a
  // dependency anywhere in this workspace (see FetchTransport.ts's matching
  // disable block, E2-S1).
  return JSON.stringify(value)
  /* eslint-enable lucy/no-json-stringify */
}

const BASE_URL = 'http://fake-api.test'

function clientAgainst(
  fetchImpl: FetchImpl,
  options: { onUnauthorized?: () => Promise<void> } = {}
): ChiefApiClientService {
  return new ChiefApiClientService({
    baseUrl: BASE_URL,
    accessToken: () => FIXTURE_JWT,
    fetchImpl,
    onUnauthorized: options.onUnauthorized
  })
}

describe('ChiefApiClientService — happy path per method', () => {
  it('requests this app’s own route handlers, under /api', async () => {
    // Next serves `app/api/companies/[companyKey]/tree/route.ts` at
    // `/api/companies/:companyKey/tree`. This client's paths were written when
    // apps/api was a separate service whose own paths began at `/companies`;
    // with apps/api deleted and the base URL now this app's origin, those
    // literals addressed a path nothing serves. Every request from the page
    // 404'd, and no test caught it — the client agreed with itself and the
    // routes agreed with themselves.
    const fetched: string[] = []
    const client = new ChiefApiClientService({
      baseUrl: 'http://web.example',
      fetchImpl: async (input) => {
        fetched.push(typeof input === 'string' ? input : input.toString())
        return new Response('{"error":{"code":"x","detail":"x"}}', { status: 500 })
      }
    })

    await client.getCompanyTree('0123456789ab').catch(() => undefined)

    expect(fetched).toEqual(['http://web.example/api/companies/0123456789ab/tree'])
  })
  it('every method lands on its E5 route and every request URL starts with baseUrl', async () => {
    const { fetchImpl, requests } = createFakeChiefApi()
    const client = clientAgainst(fetchImpl)

    await client.health()
    await client.listCompanies()
    await client.getCompany(FIXTURE_COMPANY_KEY)
    await client.getCompanyTree(FIXTURE_COMPANY_KEY)
    await client.listPeople(FIXTURE_COMPANY_KEY)
    const transcript = await client.getTranscript(FIXTURE_COMPANY_KEY, 'person-ceo')
    await client.getMailbox(FIXTURE_COMPANY_KEY, 'person-ceo')
    // `text`, not `message`. The two spellings are the defect this method
    // carries a scar for: the browser sent `{message}` and the route read
    // `body.text`, both halves tested and green, and every message an operator
    // typed came back `422 empty-message`.
    await client.say(FIXTURE_COMPANY_KEY, 'person-ceo', { text: 'hi' })
    await client.abort(FIXTURE_COMPANY_KEY, 'person-ceo')
    // NO `listModels` and NO `changeRuntime`: chief chooses no model and no
    // thinking level, so neither route exists to dial.
    // NO `newSession` and NO `compactSession`. Both dialled routes this server
    // has never served, so both were buttons that produced a 404 — see the
    // deletion test below.
    await client.pauseDepartment(FIXTURE_COMPANY_KEY, 'engineering')
    await client.resumeDepartment(FIXTURE_COMPANY_KEY, 'engineering')
    await client.stopCompany(FIXTURE_COMPANY_KEY)

    const expectedPaths = [
      '/health',
      '/companies',
      `/companies/${FIXTURE_COMPANY_KEY}`,
      `/companies/${FIXTURE_COMPANY_KEY}/tree`,
      `/companies/${FIXTURE_COMPANY_KEY}/people`,
      `/companies/${FIXTURE_COMPANY_KEY}/people/person-ceo/transcript`,
      `/companies/${FIXTURE_COMPANY_KEY}/people/person-ceo/mailbox`,
      `/companies/${FIXTURE_COMPANY_KEY}/people/person-ceo/say`,
      `/companies/${FIXTURE_COMPANY_KEY}/people/person-ceo/abort`,
      `/companies/${FIXTURE_COMPANY_KEY}/departments/engineering/pause`,
      `/companies/${FIXTURE_COMPANY_KEY}/departments/engineering/resume`,
      `/companies/${FIXTURE_COMPANY_KEY}/stop`
    ]
    expect(requests.map((request) => request.path)).toEqual(expectedPaths)
    expect(transcript.entries).toMatchObject([
      {
        type: 'message',
        message: { role: 'user', content: [{ text: 'Please check the current plan.' }] }
      }
    ])

    // D1/D2 proof: every request went to the injected baseUrl, and none of
    // them ever targeted a fixture's chiefd.url — the two walked ports are
    // never dialed directly.
    for (const request of requests) {
      expect(request.headers.authorization).toBe(`Bearer ${FIXTURE_JWT}`)
      expect(request.path.includes('52341')).toBe(false)
      expect(request.path.includes('58117')).toBe(false)
    }
  })

  it('response shapes validate against the zod schemas (a malformed one would throw)', async () => {
    const { fetchImpl } = createFakeChiefApi()
    const client = clientAgainst(fetchImpl)

    // A BARE ARRAY, and the walked url is a TOP-LEVEL field. This used to
    // read `companies.companies[i].chiefd.url` — a `{companies}` envelope
    // with the url nested inside `chiefd`, neither of which apps/api serves.
    const companies = await client.listCompanies()
    expect(companies.map((company) => company.slug)).toEqual(['acme', 'globex'])
    expect(companies[0]?.url).toBe(ACME_DAEMON_URL)
    expect(companies[1]?.url).toBe(GLOBEX_DAEMON_URL)
    expect(companies[0]?.chiefd).toEqual({
      healthy: true,
      httpStatus: 200,
      reason: 'ok',
      runtimeMode: 'company'
    })

    // No `{tree}` envelope, and the department id field is `id`.
    const tree = await client.getCompanyTree(FIXTURE_COMPANY_KEY)
    expect(tree.rootDepartmentId).toBe('root')
    const [rootDepartment] = tree.departments
    expect(rootDepartment?.children.map((child) => child.id)).toEqual(['engineering', 'sales'])
    // 6 people in engineering forces the >5 two-row layout downstream.
    expect(rootDepartment?.children[0]?.people).toHaveLength(6)
    // EVERY person carries the accent the allocator gave them, the chief
    // included. There used to be an appearance split here — `operator`/`ceo`
    // were exempted from a roster hue and arrived with no `accent` at all —
    // and it is gone with the generated themes it dressed. It is a fact about
    // a PERSON's identity, so it belongs to the tree; the roster now answers
    // only who is up.
    expect(rootDepartment?.people[0]?.accent).toBe('#8a8aaa')
    expect(rootDepartment?.children[0]?.people[1]?.accent).toBe('#e24033')
    // `accent` is still OMITTED rather than null when there is none, and an
    // absent one now means exactly one thing: the palette was exhausted.
    expect(rootDepartment?.children[0]?.people.at(-1)?.accent).toBeUndefined()

    // The HOST's converged roster: who is up. It used to be an array of people
    // carrying a `session` object from an RPC child that no longer exists.
    const people = await client.listPeople(FIXTURE_COMPANY_KEY)
    expect(people.hosted).toEqual(['person-ceo'])

    // `{ok, service, agents:{running}}` — never a `version`.
    const health = await client.health()
    expect(health).toEqual({ ok: true, service: 'chief-api', agents: { running: 1 } })
  })
})

describe('ChiefApiClientService — the talk verbs speak the route’s own words', () => {
  it('sends a turn as { text, mode } and reads back what the agent SAID', async () => {
    // Two defects in one seam, both of which were green on each side alone:
    //
    //   - the request field. The browser sent `{message}` while the route read
    //     `body.text`, so every operator message answered `422 empty-message`.
    //   - the response shape. The client expected apps/api's fire-and-forget
    //     `{queued: true}`; this server AWAITS the turn and answers
    //     `{personId, mode, reply}`, so a SUCCESSFUL turn threw a ZodError in
    //     the client while the route returned 200 with the agent's words in it.
    const { fetchImpl, requests } = createFakeChiefApi()
    const client = clientAgainst(fetchImpl)

    const outcome = await client.say(FIXTURE_COMPANY_KEY, 'person-ceo', {
      text: 'status?',
      mode: 'prompt'
    })

    expect(requests[0]?.body).toEqual({ text: 'status?', mode: 'prompt' })
    expect(outcome).toMatchObject({
      personId: 'person-ceo',
      mode: 'prompt',
      reply: 'Fixture reply.'
    })
  })

  it('reads a queued mode back with no reply rather than an empty one', async () => {
    // `steer` and `followUp` join a turn that is already running. An empty
    // `reply` here would be indistinguishable from an agent that said nothing.
    const { fetchImpl } = createFakeChiefApi()
    const client = clientAgainst(fetchImpl)

    const outcome = await client.say(FIXTURE_COMPANY_KEY, 'person-ceo', {
      text: 'actually…',
      mode: 'steer'
    })

    expect(outcome.mode).toBe('steer')
    expect(outcome.reply).toBeUndefined()
  })

  it('reads abort as the queued messages it threw away', async () => {
    // It used to expect `{aborted: true}`, a field the route has never sent.
    // The counts are what an operator acts on: somebody who steered three
    // messages and then stopped must know those three are gone, not pending.
    const { fetchImpl } = createFakeChiefApi()
    const client = clientAgainst(fetchImpl)

    expect(await client.abort(FIXTURE_COMPANY_KEY, 'person-ceo')).toEqual({
      clearedSteer: 0,
      clearedFollowUp: 0
    })
  })

  it('reads a mailbox as the server’s own count, not chiefd’s storage format', async () => {
    // chiefd answers the mailbox read with a row whose `document` is a
    // SERIALIZED JSON string. `server/Mailbox.ts` parses it and counts
    // `pending` once, on the server; this schema describes THAT, and it used
    // to describe a shape apps/api synthesised and nothing sends any more —
    // so every mailbox read threw a ZodError in the client.
    const { fetchImpl } = createFakeChiefApi()
    const client = clientAgainst(fetchImpl)

    expect(await client.getMailbox(FIXTURE_COMPANY_KEY, 'person-ceo')).toEqual({
      personId: 'person-ceo',
      pendingCount: 0,
      envelopes: []
    })
  })

  it('has no startPerson, newSession or compactSession to call', () => {
    // All three dialled routes this server has never served, so all three were
    // buttons that produced a 404 an operator could do nothing about. They are
    // DELETED rather than disabled: who is up is chiefd's roster decision, and
    // a fresh session and a compaction are durable maintenance protocols in
    // chiefd, not one call a browser makes on an agent's behalf. A re-added
    // method must arrive as a route first and a client method second.
    const client = clientAgainst(createFakeChiefApi().fetchImpl)

    expect('startPerson' in client).toBe(false)
    expect('newSession' in client).toBe(false)
    expect('compactSession' in client).toBe(false)
  })
})

describe('ChiefApiClientService — pause/resume answer with a refusal as a VALUE', () => {
  it('resolves the applied arm for a department that changed state', async () => {
    const { fetchImpl, requests } = createFakeChiefApi()
    const client = clientAgainst(fetchImpl)

    expect(await client.pauseDepartment(FIXTURE_COMPANY_KEY, 'engineering')).toEqual({
      applied: true
    })
    expect(await client.resumeDepartment(FIXTURE_COMPANY_KEY, 'engineering')).toEqual({
      applied: true
    })
    expect(requests.map((request) => request.method)).toEqual(['POST', 'POST'])
  })

  it('resolves — never throws — when chiefd declines the change', async () => {
    // Unlike every neighbouring verb: chiefd's `AtomicDirectOutcome` carries
    // `{refused, detail}` as a SUCCESSFUL value and the route serializes it
    // with a 200. Reusing the applied-only schema here would have thrown a
    // ZodError on the exact cases an operator most needs to read — pausing the
    // executive root answers `exec-root-protected` — and the message would
    // have named zod rather than chiefd. `StructureRail` is what turns this
    // value into a visible refusal.
    const fetchImpl: FetchImpl = async () =>
      new Response(
        json({ refused: 'exec-root-protected', detail: 'the executive root stays up' }),
        {
          status: 200,
          headers: { 'content-type': 'application/json' }
        }
      )
    const client = clientAgainst(fetchImpl)

    expect(await client.pauseDepartment(FIXTURE_COMPANY_KEY, 'executive')).toEqual({
      refused: 'exec-root-protected',
      detail: 'the executive root stays up'
    })
  })
})

describe('ChiefApiClientService — D1/D2: chiefd.url is typed, never fetched', () => {
  it('no recorded request ever targets a fixture chiefd.url', async () => {
    const { fetchImpl, requests } = createFakeChiefApi()
    const client = clientAgainst(fetchImpl)
    await client.listCompanies()
    await client.getCompany(FIXTURE_COMPANY_KEY)

    for (const request of requests) {
      expect(request.path.startsWith('/companies') || request.path === '/health').toBe(true)
    }
    // The client never even constructs a request against either walked
    // port — proven structurally: FakeChiefApi's own router only recognizes
    // /health and /companies/** paths against BASE_URL, so a request routed
    // to ACME_DAEMON_URL/GLOBEX_DAEMON_URL would 404 in the fake, not
    // silently succeed. No such request was made (see path list above).
  })
})

describe('ChiefApiClientService — error taxonomy', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('422 → ChiefApiError kind refusal, detail verbatim, never retried (exactly one request, no retry timer)', async () => {
    vi.useFakeTimers()
    let calls = 0
    const fetchImpl: FetchImpl = async () => {
      calls += 1
      return new Response(
        json({
          error: { code: 'illegal_transition', detail: "can't go there from here" }
        }),
        { status: 422, headers: { 'content-type': 'application/json' } }
      )
    }
    const client = clientAgainst(fetchImpl)

    let error: unknown
    try {
      await client.listCompanies()
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ChiefApiError)
    if (!(error instanceof ChiefApiError)) throw new Error('expected ChiefApiError')
    expect(error.kind).toBe('refusal')
    expect(error.code).toBe('illegal_transition')
    expect(error.detail).toBe("can't go there from here")
    expect(calls).toBe(1)
    // No status is ever retried on a timer (mandate 1) — a refusal
    // especially never schedules one.
    expect(vi.getTimerCount()).toBe(0)
    vi.useRealTimers()
  })

  it('401 → onUnauthorized invoked once, request retried once; a second 401 surfaces (two requests)', async () => {
    let calls = 0
    const fetchImpl: FetchImpl = async () => {
      calls += 1
      return new Response(json({ error: { code: 'unauthorized', detail: 'nope' } }), {
        status: 401,
        headers: { 'content-type': 'application/json' }
      })
    }
    let onUnauthorizedCalls = 0
    const client = clientAgainst(fetchImpl, {
      onUnauthorized: async () => {
        onUnauthorizedCalls += 1
      }
    })

    let error: unknown
    try {
      await client.listCompanies()
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ChiefApiError)
    if (!(error instanceof ChiefApiError)) throw new Error('expected ChiefApiError')
    expect(error.kind).toBe('unauthorized')
    expect(onUnauthorizedCalls).toBe(1)
    expect(calls).toBe(2)
  })

  it('503 envelope → kind upstream', async () => {
    const fetchImpl: FetchImpl = async () =>
      new Response(json({ error: { code: 'upstream-unreachable', detail: 'chiefd is down' } }), {
        status: 503,
        headers: { 'content-type': 'application/json' }
      })
    const client = clientAgainst(fetchImpl)

    let error: unknown
    try {
      await client.health()
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ChiefApiError)
    if (!(error instanceof ChiefApiError)) throw new Error('expected ChiefApiError')
    expect(error.kind).toBe('upstream')
  })

  it('a non-JSON error body is tolerated as kind upstream with the raw text as detail', async () => {
    const fetchImpl: FetchImpl = async () =>
      new Response('<html>502 Bad Gateway</html>', {
        status: 502,
        headers: { 'content-type': 'text/html' }
      })
    const client = clientAgainst(fetchImpl)

    let error: unknown
    try {
      await client.health()
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ChiefApiError)
    if (!(error instanceof ChiefApiError)) throw new Error('expected ChiefApiError')
    expect(error.kind).toBe('upstream')
    expect(error.detail).toBe('<html>502 Bad Gateway</html>')
  })

  it('an AbortSignal cancels the request, wrapped in the one taxonomy', async () => {
    const controller = new AbortController()
    const fetchImpl: FetchImpl = async (_input, init) =>
      new Promise((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => {
          reject(new DOMException('aborted', 'AbortError'))
        })
      })
    const client = clientAgainst(fetchImpl)

    const pending = client.health(controller.signal)
    controller.abort()
    let error: unknown
    try {
      await pending
    } catch (caught) {
      error = caught
    }
    // A transport-level failure (fetch itself throwing, never reaching an
    // apps/api response) is still a ChiefApiError -- Mandate 0's "one error
    // taxonomy" means a caller only ever branches on `.kind`, never on
    // whether the failure happened to be an HTTP status or a raw exception.
    expect(error).toBeInstanceOf(ChiefApiError)
    if (!(error instanceof ChiefApiError)) throw new Error('expected ChiefApiError')
    expect(error.kind).toBe('network')
  })

  it('a fetch failure that never reaches apps/api is also the one taxonomy', async () => {
    const fetchImpl: FetchImpl = async () => {
      throw new TypeError('fetch failed: connection refused')
    }
    const client = clientAgainst(fetchImpl)

    let error: unknown
    try {
      await client.health()
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(ChiefApiError)
    if (!(error instanceof ChiefApiError)) throw new Error('expected ChiefApiError')
    expect(error.kind).toBe('network')
    expect(error.detail).toBe('fetch failed: connection refused')
  })
})
