/**
 * An in-process `fetch` implementation serving apps/api's REAL contract from
 * fixtures. Shared by every E6 seat from S4 onward — append fixtures and
 * routes, never redesign the router shape.
 *
 * THIS FAKE IS THE REASON THE WEB/API DIVERGENCE SURVIVED. It used to serve
 * the E5 epic's prose route table rather than the shapes apps/api's handlers
 * actually return: a `{companies}` envelope, a `{people}` envelope, a
 * a `{tree: {departmentId, …}}` envelope, people keyed
 * by `personId` carrying `running`/`provider`/`model`/`thinkingLevel`/
 * `accentColor`, companies carrying `name`/`hosting`/`peopleCount`/
 * `departmentCount`, a `/health` with a `version`, and 404s coded
 * `unknown-company`/`not-found`. apps/api serves NONE of that. Every one of
 * apps/web's suites passed against this fiction while the live product
 * rendered "Loading company…" forever.
 *
 * So the rule for this file is now explicit: a response body here is only
 * allowed to be a shape some apps/api handler can actually produce. Each
 * fixture below cites the apps/api type it mirrors, and the whole set was
 * checked against a live `curl` of every route.
 *
 * Auth simulation: every route except `/health`, `/v1/auth/challenge`,
 * `/v1/auth/token` requires `Authorization: Bearer <fixture JWT>` — checked
 * ONLY on that header. A request carrying the token as a `?accessToken=`
 * query parameter is NOT an accepted alternative (apps/web's streaming-fetch
 * SSE layer uses the header exclusively, per the E6 epic Contract) — the
 * query string is never inspected here, so such a request 401s like any
 * other unauthenticated one, making the misuse a visible test failure
 * rather than a silently-accepted shortcut.
 */
import { authChallengeMessage, verifyAuthChallenge } from '@chief/chiefing'
import type {
  FakeChiefApiFixtures,
  FakeLifecycleScript,
  RecordedRequest
} from '@test/types/FakeChiefApi'

import type { CompanyTree, PeopleResponse, TranscriptResponse, TreePerson } from '@/types/ChiefApi'
import type { FetchImpl } from '@/types/Fetch'

/** The token every non-exempt fake route accepts, and the only one
 * `/v1/auth/token` ever issues. Exported so tests can construct an
 * `accessToken` getter without going through the real challenge flow when
 * they are testing something else (e.g. `ChiefApiClientService`'s route
 * behavior, not the auth flow itself). */
export const FIXTURE_JWT = 'fixture-operator-jwt'

/** How this fixture's default company is keyed: a DIRECTORY, and the
 * `sha256(dir)[..12]` its creator minted for it. `apps/web` addresses the
 * company by the key and reads that company's operator key out of its
 * directory, so a suite staging a real key on disk overrides `companies` with
 * its own `dir`. */
export const FIXTURE_COMPANY_KEY = '0123456789ab'

/** Two DIFFERENT walked ports, deliberately — nothing in `apps/web` may
 * encode a chiefd port as a constant, and any code path that tried to fetch
 * one of these directly would be immediately obvious (ruling D1/D2).
 *
 * These live on the company row's TOP-LEVEL `url`, which is where apps/api's
 * `CompanySummary` carries the walked address. They are NOT inside `chiefd`
 * — that object is `CompanyChiefdHealth` (`healthy`/`httpStatus`/`reason`/
 * `runtimeMode`) and has never had a `url` member. */
export const ACME_DAEMON_URL = 'http://127.0.0.1:52341'
export const GLOBEX_DAEMON_URL = 'http://127.0.0.1:58117'

/** One person as apps/api's `CompanyTreePerson` carries them. `accent` is
 * OMITTED, not null, when there is none — the key is built conditionally and
 * `JSON.stringify` drops it. Absence is the palette-exhausted case and
 * nothing else: chief allocates an accent for EVERY person, the chief
 * included. */
function treePerson(
  id: string,
  name: string,
  title: string,
  kind: string,
  accent?: string
): TreePerson {
  const employmentState = 'active' as const
  return typeof accent === 'string'
    ? { id, name, title, kind, employmentState, accent }
    : { id, name, title, kind, employmentState }
}

const AGENT_PANE_TRANSCRIPT: TranscriptResponse = {
  entries: [
    {
      type: 'message',
      id: 'fixture-user-1',
      message: {
        role: 'user',
        content: [{ type: 'text', text: 'Please check the current plan.' }]
      }
    }
  ],
  leafId: 'fixture-leaf-1'
}

function defaultFixtures(): FakeChiefApiFixtures {
  const ceo = treePerson(
    'person-ceo',
    'Cora CEO',
    'Chief Executive Officer',
    'executive',
    '#8a8aaa'
  )
  const engHead = treePerson(
    'person-eng-head',
    'Erin Engineer',
    'Head of Engineering',
    'head',
    '#5b8fd6'
  )
  const engWorkers = [
    treePerson('person-eng-1', 'Wes Worker', 'Engineer', 'worker', '#e24033'),
    treePerson('person-eng-2', 'Priya Patel', 'Engineer', 'worker', '#c75e00'),
    treePerson('person-eng-3', 'Omar Osei', 'Engineer', 'worker', '#a27400'),
    treePerson('person-eng-4', 'Nadia Novak', 'Engineer', 'worker', '#2c8e46'),
    // The one accent-less person in this fixture, and the ONLY reason a
    // person may be one: the palette ran out. Kept so the optional wire
    // field keeps a live decode case.
    treePerson('person-eng-5', 'Kai Kim', 'Engineer', 'worker')
  ]

  /** apps/api's `CompanyTree`: the company handle, the root department's id,
   * and the departments as a FOREST. There is no `{tree}` envelope and no
   * `departmentId` key — the id field is `id`.
   *
   * `slug` carries the company KEY, because chiefd echoes back what
   * `POST /v1/org/tree/structured` was asked with. */
  const tree: CompanyTree = {
    slug: FIXTURE_COMPANY_KEY,
    rootDepartmentId: 'root',
    departments: [
      {
        id: 'root',
        name: 'Acme',
        headPersonId: ceo.id,
        state: 'active',
        people: [ceo],
        children: [
          {
            id: 'engineering',
            name: 'Engineering',
            headPersonId: engHead.id,
            state: 'active',
            // 6 people in one department forces the >5 two-row pane layout.
            people: [engHead, ...engWorkers],
            children: []
          },
          {
            id: 'sales',
            name: 'Sales',
            headPersonId: ceo.id,
            state: 'active',
            people: [],
            children: []
          }
        ]
      }
    ]
  }

  // The host's converged roster: the CEO is up, and everybody else is simply
  // not desired — chiefd never asked for them, so they do not appear.
  const peopleResponse: PeopleResponse = { hosted: [ceo.id], degraded: [] }

  return {
    // A BARE ARRAY of apps/api's `CompanySummary` — no `{companies}`
    // envelope, no `name`/`hosting`/`peopleCount`/`departmentCount`. `globex`
    // is the documented "registered a location but chiefd did not answer"
    // case: a url is present and the probe failed.
    companies: [
      {
        key: FIXTURE_COMPANY_KEY,
        dir: '/work/acme',
        slug: 'acme',
        status: 'running',
        url: ACME_DAEMON_URL,
        chiefd: { healthy: true, httpStatus: 200, reason: 'ok', runtimeMode: 'company' }
      },
      {
        key: 'cafebabe0011',
        dir: '/work/globex',
        slug: 'globex',
        status: 'stopped',
        url: GLOBEX_DAEMON_URL,
        chiefd: { healthy: false, httpStatus: 503, reason: 'probe failed' }
      }
    ],
    // `GET /companies/:companyKey` is `CompanyDirectoryService.status()`,
    // which builds `{key, dir, slug, status, chiefd}` and — unlike `list()` —
    // never carries `url`.
    //
    // Every per-company map below is keyed by the company KEY, because that is
    // what the routes resolve by. Keying them by the slug would let a client
    // that addressed a company by its display word pass here and 404 in
    // production, which is the exact divergence this file's header is about.
    companyDetails: {
      [FIXTURE_COMPANY_KEY]: {
        key: FIXTURE_COMPANY_KEY,
        dir: '/work/acme',
        slug: 'acme',
        status: 'running',
        chiefd: { healthy: true, httpStatus: 200, reason: 'ok', runtimeMode: 'company' }
      }
    },
    trees: { [FIXTURE_COMPANY_KEY]: tree },
    people: { [FIXTURE_COMPANY_KEY]: peopleResponse },
    transcripts: {
      [FIXTURE_COMPANY_KEY]: {
        'person-ceo': AGENT_PANE_TRANSCRIPT
      }
    },
    // `personId` is part of the body, not just the path. The route derives
    // `pendingCount` server-side (`server/Mailbox.ts`) and answers with the
    // person it counted for; a fixture without it described a shape the route
    // cannot produce, which is the one thing this file forbids.
    mailboxes: {
      [FIXTURE_COMPANY_KEY]: {
        'person-ceo': { personId: 'person-ceo', pendingCount: 0, envelopes: [] }
      }
    },
    lifecycle: {
      create: {
        phases: [
          { phase: 'company-daemon-start', detail: 'starting company daemon' },
          { phase: 'unrecognized-fixture-phase', detail: 'forwarded unchanged' }
        ],
        terminal: { event: 'created', slug: 'new-company' }
      },
      boot: {
        // Keyed by the company KEY the route is dialled with; the terminal
        // frame answers with the SLUG, because `chief host`'s lifecycle wire
        // does. The two differ here so nothing can pass by confusing them.
        [FIXTURE_COMPANY_KEY]: {
          phases: [{ phase: 'chief-start', detail: 'starting CEO' }],
          terminal: { event: 'booted', slug: 'acme' }
        }
      }
    }
  }
}

function mergeFixtures(overrides?: Partial<FakeChiefApiFixtures>): FakeChiefApiFixtures {
  const base = defaultFixtures()
  if (!overrides) return base
  return { ...base, ...overrides }
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(typeof body === 'undefined' ? undefined : jsonStringify(body), {
    status,
    headers: { 'content-type': 'application/json' }
  })
}

/* eslint-disable lucy/no-json-stringify */
// Test-only harness fixture serialization; @tribes-terminal/foundation is
// not a dependency anywhere in this workspace (see FetchTransport.ts's
// matching disable block, E2-S1).
function jsonStringify(value: unknown): string {
  return JSON.stringify(value)
}
/* eslint-enable lucy/no-json-stringify */

function errorResponse(status: number, code: string, detail: string): Response {
  return jsonResponse({ error: { code, detail } }, status)
}

/** apps/api's `UnknownResourceError` — 404, code `unknown-resource`, for a
 * company/person/task that does not exist. This fake used to invent two codes
 * (`unknown-company` and `not-found`) that appear nowhere in
 * `apps/api/src/common/Errors.ts`. */
function unknownResource(detail: string): Response {
  return errorResponse(404, 'unknown-resource', detail)
}

function lifecycleFrame(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${jsonStringify(data)}\n\n`
}

function lifecycleResponse(script: FakeLifecycleScript): Response {
  const frames = script.phases.map((frame) => lifecycleFrame('phase', frame))
  switch (script.terminal.event) {
    case 'created':
    case 'booted':
      frames.push(lifecycleFrame(script.terminal.event, { slug: script.terminal.slug }))
      break
    case 'failed':
      frames.push(lifecycleFrame('failed', { error: script.terminal.error }))
      break
  }
  const bytes = new TextEncoder().encode(frames.join(''))
  const stream = new ReadableStream<Uint8Array>({
    start(controller): void {
      controller.enqueue(bytes)
      controller.close()
    }
  })
  return new Response(stream, {
    status: 200,
    headers: { 'content-type': 'text/event-stream' }
  })
}

function matchPath(pattern: string, path: string): Record<string, string> | undefined {
  const patternSegments = pattern.split('/').filter((segment) => segment.length > 0)
  const pathSegments = path.split('/').filter((segment) => segment.length > 0)
  if (patternSegments.length !== pathSegments.length) return undefined
  const params: Record<string, string> = {}
  for (let index = 0; index < patternSegments.length; index += 1) {
    const patternSegment = patternSegments[index]
    const pathSegment = pathSegments[index]
    if (typeof patternSegment !== 'string' || typeof pathSegment !== 'string') return undefined
    if (patternSegment.startsWith(':')) {
      params[patternSegment.slice(1)] = decodeURIComponent(pathSegment)
    } else if (patternSegment !== pathSegment) {
      return undefined
    }
  }
  return params
}

// `/v1/auth/*`, matching the three paths chiefd's own verify-middleware
// exempts (`EXEMPT_PATHS` in `authn/middleware.rs`). They were `/auth/*` here
// while `apiUrl` meant the deleted apps/api and carried the version prefix
// already; A2 corrected the caller, so the fake has to serve the real wire or
// it would go on validating the 404 shape.
const EXEMPT_PATHS = new Set(['/health', '/v1/auth/challenge', '/v1/auth/token'])

export function createFakeChiefApi(overrides?: Partial<FakeChiefApiFixtures>): {
  fetchImpl: FetchImpl
  issuedTokens: string[]
  requests: RecordedRequest[]
  /** Replaces a company's served tree in place — a test-only mutator (E6-S7,
   * #812) for proving a story reacts to a RE-SERVED tree rather than
   * anything it computed itself. Additive: every existing caller of
   * `createFakeChiefApi` is unaffected. */
  setTree(companyKey: string, tree: CompanyTree): void
} {
  const fixtures = mergeFixtures(overrides)
  let companies = fixtures.companies
  let companyDetails = fixtures.companyDetails
  let trees = fixtures.trees
  const issuedTokens: string[] = []
  const requests: RecordedRequest[] = []
  const pendingNonces = new Map<string, { identityId: string; nonce: string }>()
  let nonceCounter = 0

  const fetchImpl: FetchImpl = async (input, init) => {
    const url = new URL(typeof input === 'string' ? input : input.toString())
    // Every app route is mounted under `/api` — Next serves
    // `app/api/companies/[companyKey]/tree/route.ts` at `/api/companies/:companyKey/tree`.
    // The fake strips the prefix so its route table stays readable, and
    // REFUSES a company path that arrived without it.
    //
    // That refusal is the point. Every client path in this app was once
    // written without the prefix, because apps/api was a separate service
    // whose own paths began at `/companies`. With apps/api deleted and the
    // base URL now this app's own origin, those literals addressed
    // `/companies/…` — which nothing serves. Every request from the page
    // 404'd while both halves stayed green, because the client agreed with
    // itself and the routes agreed with themselves. A fake that answered the
    // unprefixed path would keep agreeing with the broken half forever.
    if (url.pathname.startsWith('/api/')) {
      url.pathname = url.pathname.slice('/api'.length)
    } else if (url.pathname.startsWith('/companies')) {
      throw new Error(
        `[FakeChiefApi] "${url.pathname}" is missing the /api prefix — ` +
          'this app serves its route handlers under /api, so this request would 404 in a browser'
      )
    }
    const method = init?.method ?? 'GET'
    const headers: Record<string, string> = {}
    if (init?.headers) {
      new Headers(init.headers).forEach((value, key) => {
        headers[key] = value
      })
    }
    let body: unknown
    if (typeof init?.body === 'string' && init.body.length > 0) {
      body = JSON.parse(init.body)
    }
    requests.push({ method, path: url.pathname, search: url.search, headers, body })

    // beacond's company listing, answered BEFORE the auth gate below:
    // discovery is what tells a caller which chiefd to authenticate against, so
    // it cannot itself require a token. apps/api used to hide this hop; with it
    // deleted, the session route resolves the company itself and this fake
    // stands in for both halves — beacond, then that company's daemon.
    //
    // `/v1/list` and not `/v1/lookup`: the registry's lookup takes the
    // company's DIRECTORY, which only a process standing in it knows. This
    // server matches the KEY on the list it already reads.
    if (method === 'GET' && url.pathname === '/v1/list') {
      return jsonResponse({
        // Derived from the SAME company set this fake serves through
        // `/api/companies`, so a test cannot register one company with beacond
        // and render another. All five location fields together or none —
        // beacond's own invariant, so a fixture that supplied only `url` is
        // not a company row this client will parse.
        companies: companies.map((company) => ({
          dir: company.dir,
          key: company.key,
          slug: company.slug,
          registeredAt: '2026-08-08T00:00:00.000Z',
          url: url.origin,
          port: 8792,
          pid: 4242,
          hostname: 'fixture-host',
          lastSeenAt: '2026-08-08T00:00:00.000Z'
        }))
      })
    }

    if (!EXEMPT_PATHS.has(url.pathname)) {
      const authorization = headers.authorization ?? headers.Authorization
      if (authorization !== `Bearer ${FIXTURE_JWT}`) {
        return errorResponse(401, 'unauthorized', 'missing or invalid bearer token')
      }
    }

    if (method === 'POST' && url.pathname === '/v1/auth/challenge') {
      const identityId = stringField(body, 'identityId') ?? 'operator'
      const nonceId = `nonce-${(nonceCounter += 1)}`
      const nonce = `fixture-nonce-${nonceCounter}`
      pendingNonces.set(nonceId, { identityId, nonce })
      return jsonResponse({ nonceId, nonce })
    }

    if (method === 'POST' && url.pathname === '/v1/auth/token') {
      const nonceId = stringField(body, 'nonceId')
      const signature = stringField(body, 'signature')
      if (typeof nonceId !== 'string' || typeof signature !== 'string') {
        return errorResponse(401, 'unauthorized', 'challenge not satisfied')
      }
      const pending = pendingNonces.get(nonceId)
      if (!pending) {
        return errorResponse(401, 'unauthorized', 'challenge not satisfied')
      }
      const message = authChallengeMessage(pending.identityId, pending.nonce)
      const operatorPublicKey = fixtureOperatorPublicKey()
      // No fixture keypair registered (setFixtureOperatorPublicKey never
      // called) → nothing can verify, so refuse rather than let
      // verifyAuthChallenge throw on a malformed key.
      const verified =
        typeof operatorPublicKey === 'string' && safeVerify(message, signature, operatorPublicKey)
      if (!verified) {
        return errorResponse(401, 'unauthorized', 'challenge not satisfied')
      }
      pendingNonces.delete(nonceId)
      issuedTokens.push(FIXTURE_JWT)
      return jsonResponse({ token: FIXTURE_JWT })
    }

    if (method === 'GET' && url.pathname === '/health') {
      // apps/api's HealthController: `{ok, service, agents:{running}}`. It has
      // never served a `version`.
      return jsonResponse({ ok: true, service: 'chief-api', agents: { running: 1 } })
    }

    if (method === 'GET' && url.pathname === '/companies') {
      // A BARE ARRAY — `CompanyController.list` has no `{companies}` envelope.
      return jsonResponse(companies)
    }

    const companyMatch = matchPath('/companies/:companyKey', url.pathname)
    if (method === 'GET' && companyMatch) {
      const detail = companyDetails[companyMatch.companyKey]
      if (!detail)
        return errorResponse(404, 'unknown-company', `no company '${companyMatch.companyKey}'`)
      return jsonResponse(detail)
    }

    if (method === 'POST' && url.pathname === '/companies') {
      const script = fixtures.lifecycle?.create
      if (!script)
        return errorResponse(503, 'upstream-unreachable', 'create fixture is unavailable')
      return lifecycleResponse(script)
    }

    const bootMatch = matchPath('/companies/:companyKey/boot', url.pathname)
    if (method === 'POST' && bootMatch) {
      const company = companies.find((row) => row.key === bootMatch.companyKey)
      if (!company) return unknownResource(`no company '${bootMatch.companyKey}'`)
      // No `hosting === 'tmux'` gate any more. apps/api's boot/stop routes have
      // no such branch: `company-not-api-hosted` is a 409 raised by
      // `AgentTalkService.requireApiHosted` on the LIVE verbs (say/abort/
      // transcript/mailbox/models/thinking/session/model), never by the
      // lifecycle routes — and no route serves a `hosting` field for a caller
      // to have predicted it from anyway.
      const script = fixtures.lifecycle?.boot?.[bootMatch.companyKey]
      if (!script) return errorResponse(503, 'upstream-unreachable', 'boot fixture is unavailable')
      return lifecycleResponse(script)
    }

    const stopMatch = matchPath('/companies/:companyKey/stop', url.pathname)
    if (method === 'POST' && stopMatch) {
      const company = companies.find((row) => row.key === stopMatch.companyKey)
      if (!company) return unknownResource(`unknown company: ${stopMatch.companyKey}`)
      companies = companies.map((row) =>
        row.key === stopMatch.companyKey ? { ...row, status: 'stopped' as const } : row
      )
      const detail = companyDetails[stopMatch.companyKey]
      if (detail) {
        companyDetails = {
          ...companyDetails,
          [stopMatch.companyKey]: { ...detail, status: 'stopped' }
        }
      }
      return jsonResponse({ stopped: true })
    }

    const treeMatch = matchPath('/companies/:companyKey/tree', url.pathname)
    if (method === 'GET' && treeMatch) {
      const tree = trees[treeMatch.companyKey]
      if (!tree) return unknownResource(`unknown company: ${treeMatch.companyKey}`)
      return jsonResponse(tree)
    }

    const peopleMatch = matchPath('/companies/:companyKey/people', url.pathname)
    if (method === 'GET' && peopleMatch) {
      const people = fixtures.people[peopleMatch.companyKey]
      if (!people) return unknownResource(`unknown company: ${peopleMatch.companyKey}`)
      return jsonResponse(people)
    }

    const transcriptMatch = matchPath(
      '/companies/:companyKey/people/:personId/transcript',
      url.pathname
    )
    if (method === 'GET' && transcriptMatch) {
      const transcript =
        fixtures.transcripts[transcriptMatch.companyKey]?.[transcriptMatch.personId]
      // apps/api gates every live verb behind `requireHostedClient`, which
      // throws 409 `person-not-running` when the person has no live child.
      if (!transcript) {
        return errorResponse(
          409,
          'person-not-running',
          `person "${transcriptMatch.personId}" has no running agent`
        )
      }
      return jsonResponse(transcript)
    }

    const mailboxMatch = matchPath('/companies/:companyKey/people/:personId/mailbox', url.pathname)
    if (method === 'GET' && mailboxMatch) {
      const mailbox = fixtures.mailboxes[mailboxMatch.companyKey]?.[mailboxMatch.personId]
      if (!mailbox) {
        return errorResponse(
          409,
          'person-not-running',
          `person "${mailboxMatch.personId}" has no running agent`
        )
      }
      return jsonResponse(mailbox)
    }

    const sayMatch = matchPath('/companies/:companyKey/people/:personId/say', url.pathname)
    if (method === 'POST' && sayMatch) {
      // The wire word is `text`. The browser used to send `{message}` while
      // the route read `body.text`, so every message an operator typed came
      // back `422 empty-message` — and a fake that accepted either spelling
      // would keep both halves green forever. It refuses the old one.
      const text = stringField(body, 'text')
      if (typeof text !== 'string') {
        return errorResponse(422, 'invalid-request', '"text" is required and must be a string')
      }
      const mode = stringField(body, 'mode') ?? 'prompt'
      // The server AWAITS the turn, so a `prompt` carries the agent's words.
      // `steer`/`followUp` join a turn already running and have no reply yet;
      // inventing an empty one would make a queued message look like an agent
      // that said nothing.
      return jsonResponse(
        mode === 'prompt'
          ? { personId: sayMatch.personId, mode, reply: 'Fixture reply.' }
          : { personId: sayMatch.personId, mode }
      )
    }

    const abortMatch = matchPath('/companies/:companyKey/people/:personId/abort', url.pathname)
    if (method === 'POST' && abortMatch) {
      // What abort THREW AWAY, which is the part an operator acts on. It used
      // to answer `{aborted: true}`, a field the route has never sent.
      return jsonResponse({ clearedSteer: 0, clearedFollowUp: 0 })
    }

    // NO `…/session/new` and NO `…/session/compact` handlers. Neither route
    // has ever existed in this server, so a fake that answered them served the
    // fiction this file exists to end: the client's two methods and their two
    // buttons passed here and 404'd in a browser. Both are deleted from the
    // client, so a request arriving at either path now falls through to the
    // `no fixture route` 404 — which is what a browser would have seen.

    // Pause and resume a department. chiefd answers both with
    // `AtomicDirectOutcome`, so a REFUSAL ARRIVES AS A 200 WITH A BODY; the
    // applied arm is what the fixture serves, and the refusal arm is exercised
    // directly in `ChiefApiClientService.test.ts` (a fixture that could only
    // ever refuse would be a fixture nothing else could use).
    const pauseMatch = matchPath(
      '/companies/:companyKey/departments/:departmentId/pause',
      url.pathname
    )
    const resumeMatch = matchPath(
      '/companies/:companyKey/departments/:departmentId/resume',
      url.pathname
    )
    if (method === 'POST' && (pauseMatch || resumeMatch)) {
      return jsonResponse({ applied: true })
    }

    return unknownResource(`no fixture route for ${method} ${url.pathname}`)
  }

  return {
    fetchImpl,
    issuedTokens,
    requests,
    setTree: (companyKey: string, tree: CompanyTree): void => {
      trees = { ...trees, [companyKey]: tree }
    }
  }
}

function stringField(value: unknown, key: string): string | undefined {
  if (!value || typeof value !== 'object') return undefined
  if (!(key in value)) return undefined
  const field = Reflect.get(value, key)
  return typeof field === 'string' ? field : undefined
}

/** `verifyAuthChallenge` throws on a malformed public key (e.g. an
 * unregistered fixture); an unverifiable signature is exactly the same
 * outcome as an invalid one here, so both collapse to `false`. */
function safeVerify(message: string, signature: string, publicSpkiBase64: string): boolean {
  try {
    return verifyAuthChallenge(message, signature, publicSpkiBase64)
  } catch {
    return false
  }
}

/** The fixture operator keypair's public key, set by
 * `test/harness/OperatorKeypairFixture.ts` via `setFixtureOperatorPublicKey`
 * so `ApiSessionRoute.test.ts` can prove the fake validated a REAL
 * IEEE-P1363 signature rather than trusting any string. */
let fixtureOperatorPublicKeyValue: string | undefined
export function setFixtureOperatorPublicKey(publicSpkiBase64: string | undefined): void {
  fixtureOperatorPublicKeyValue = publicSpkiBase64
}
function fixtureOperatorPublicKey(): string | undefined {
  return fixtureOperatorPublicKeyValue
}
