/**
 * apps/web's ONE way to call apps/api (the E5 Contract, `#805`). Base URL
 * and token provider are injected — this service reads no environment
 * variables itself and never constructs any address but the one it was
 * given (rulings D1/D2). `chiefd.url` fields decoded from apps/api
 * responses are typed and returned to the caller unread — this service
 * never fetches them.
 */
import { appPath } from '@/common/AppRoutes'
import { ChiefApiError, type ChiefApiErrorKind } from '@/types/ApiErrors'
import type {
  AbortResponse,
  ChiefApiClientOptions,
  CompaniesResponse,
  CompanySummary,
  CompanyTree,
  CreateDepartmentRequest,
  CreateDepartmentResponse,
  DepartmentStateChangeResponse,
  HealthResponse,
  HirePersonRequest,
  HirePersonResponse,
  MailboxResponse,
  PeopleResponse,
  SayInput,
  SayResponse,
  StaffingApplied,
  StopResponse,
  TranscriptResponse
} from '@/types/ChiefApi'
import {
  AbortResponseSchema,
  CompaniesResponseSchema,
  CompanySummarySchema,
  CompanyTreeSchema,
  CreateDepartmentResponseSchema,
  DepartmentStateChangeResponseSchema,
  ErrorEnvelopeSchema,
  HealthResponseSchema,
  HirePersonResponseSchema,
  MailboxResponseSchema,
  PeopleResponseSchema,
  SayResponseSchema,
  StaffingAppliedSchema,
  StopResponseSchema,
  TranscriptResponseSchema
} from '@/types/ChiefApi'
import type { FetchImpl } from '@/types/Fetch'
import {
  type FounderAbortOutcome,
  FounderAbortResponseSchema,
  type FounderSayOutcome,
  FounderSayResponseSchema,
  type FounderTranscript,
  FounderTranscriptResponseSchema
} from '@/types/Founder'

/** Every status the E5 error-envelope table names, mapped to its taxonomy
 * kind. A status this table does not name (or an envelope that fails to
 * parse) is `'upstream'` — the catch-all for "the server said something we
 * cannot cleanly classify" rather than inventing a state it did not report.
 *
 * 403 has no dedicated kind in the Contract's `ChiefApiErrorKind` union (it
 * lists only unauthorized/not-found/conflict/refusal/upstream/bad-request/
 * network) — folded into `'unauthorized'` here since both are permission
 * failures, but the retry-once mechanic below triggers on the literal `401`
 * status, never on this mapped kind, so a 403 can never enter a retry loop. */
function errorKindForStatus(status: number): ChiefApiErrorKind {
  switch (status) {
    case 400:
      return 'bad-request'
    case 401:
      return 'unauthorized'
    case 403:
      return 'unauthorized'
    case 404:
      return 'not-found'
    case 409:
      return 'conflict'
    case 422:
      return 'refusal'
    case 503:
      return 'upstream'
    default:
      return 'upstream'
  }
}

async function readErrorBody(response: Response): Promise<ChiefApiError> {
  const status = response.status
  const text = await response.text()
  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  } catch {
    return new ChiefApiError({ kind: 'upstream', status, detail: text })
  }
  const envelope = ErrorEnvelopeSchema.safeParse(parsed)
  if (!envelope.success) {
    return new ChiefApiError({ kind: 'upstream', status, detail: text })
  }
  return new ChiefApiError({
    kind: errorKindForStatus(status),
    status,
    code: envelope.data.error.code,
    detail: envelope.data.error.detail
  })
}

function personPath(companyKey: string, personId: string): string {
  return `/companies/${encodeURIComponent(companyKey)}/people/${encodeURIComponent(personId)}`
}

function buildQuery(params: Record<string, string | undefined>): string {
  const entries = Object.entries(params).filter((entry): entry is [string, string] => {
    const [, value] = entry
    return typeof value === 'string' && value.length > 0
  })
  if (entries.length === 0) return ''
  const search = new URLSearchParams(entries)
  return `?${search.toString()}`
}

export class ChiefApiClientService {
  private readonly baseUrl: string
  private readonly accessToken?: () => string | null
  private readonly fetchImpl: FetchImpl
  private readonly onUnauthorized?: () => Promise<void>

  constructor(options: ChiefApiClientOptions) {
    this.baseUrl = options.baseUrl
    this.accessToken = options.accessToken
    // `fetch` MUST be bound to the global before it is stored on an instance.
    // Called as `this.fetchImpl(...)`, an unbound browser `fetch` receives this
    // service as its `this` and Chrome throws
    // "Failed to execute 'fetch' on 'Window': Illegal invocation" — every
    // request from the company page failed this way. A caller-supplied
    // `fetchImpl` is already a plain function and binds harmlessly.
    this.fetchImpl = options.fetchImpl ?? fetch.bind(globalThis)
    this.onUnauthorized = options.onUnauthorized
  }

  private async request<T>(
    method: 'GET' | 'POST',
    path: string,
    schema: { parse: (value: unknown) => T },
    options: { body?: unknown; signal?: AbortSignal; attempt?: number } = {}
  ): Promise<T> {
    const attempt = options.attempt ?? 0
    const headers: Record<string, string> = {}
    if (method === 'POST') headers['content-type'] = 'application/json'
    const token = this.accessToken?.()
    if (token) headers.authorization = `Bearer ${token}`

    let response: Response
    try {
      response = await this.fetchImpl(`${this.baseUrl}${appPath(path)}`, {
        method,
        headers,
        body: method === 'POST' ? serializeRequestBody(options.body) : undefined,
        signal: options.signal
      })
    } catch (error) {
      // `fetch` itself failing (DNS/connection refused/aborted) is not an
      // apps/api response at all -- there is no envelope to parse. Wrapping
      // it as `ChiefApiError { kind: 'network' }` keeps the taxonomy the one
      // thing every caller branches on (Mandate 0: "one error taxonomy"),
      // instead of letting a raw TypeError/AbortError escape untouched.
      throw new ChiefApiError({
        kind: 'network',
        detail: error instanceof Error ? error.message : String(error),
        cause: error
      })
    }

    if (response.status >= 200 && response.status < 300) {
      const text = await response.text()
      const parsed: unknown = text.length > 0 ? JSON.parse(text) : undefined
      return schema.parse(parsed)
    }

    if (response.status === 401 && attempt === 0 && this.onUnauthorized) {
      await this.onUnauthorized()
      return this.request(method, path, schema, { ...options, attempt: 1 })
    }

    throw await readErrorBody(response)
  }

  /**
   * Founder Mode's three verbs.
   *
   * No companyKey and no personId, and that is the whole shape of Founder: it is the
   * one agent that exists before a company does, so there is nothing to key it
   * by. Every other method on this service names a company because every other
   * agent is somebody in one.
   */
  async founderTranscript(signal?: AbortSignal): Promise<FounderTranscript> {
    return this.request('GET', '/founder/transcript', FounderTranscriptResponseSchema, { signal })
  }

  async founderSay(text: string, signal?: AbortSignal): Promise<FounderSayOutcome> {
    return this.request('POST', '/founder/say', FounderSayResponseSchema, {
      body: { text },
      signal
    })
  }

  async founderAbort(signal?: AbortSignal): Promise<FounderAbortOutcome> {
    return this.request('POST', '/founder/abort', FounderAbortResponseSchema, {
      body: {},
      signal
    })
  }

  async health(signal?: AbortSignal): Promise<HealthResponse> {
    return this.request('GET', '/health', HealthResponseSchema, { signal })
  }

  async listCompanies(signal?: AbortSignal): Promise<CompaniesResponse> {
    return this.request('GET', '/companies', CompaniesResponseSchema, { signal })
  }

  async getCompany(companyKey: string, signal?: AbortSignal): Promise<CompanySummary> {
    return this.request(
      'GET',
      `/companies/${encodeURIComponent(companyKey)}`,
      CompanySummarySchema,
      {
        signal
      }
    )
  }

  async getCompanyTree(companyKey: string, signal?: AbortSignal): Promise<CompanyTree> {
    return this.request(
      'GET',
      `/companies/${encodeURIComponent(companyKey)}/tree`,
      CompanyTreeSchema,
      {
        signal
      }
    )
  }

  async listPeople(companyKey: string, signal?: AbortSignal): Promise<PeopleResponse> {
    return this.request(
      'GET',
      `/companies/${encodeURIComponent(companyKey)}/people`,
      PeopleResponseSchema,
      { signal }
    )
  }

  async getTranscript(
    companyKey: string,
    personId: string,
    since?: string,
    signal?: AbortSignal
  ): Promise<TranscriptResponse> {
    const query = buildQuery({ since })
    const base = personPath(companyKey, personId)
    return this.request('GET', `${base}/transcript${query}`, TranscriptResponseSchema, {
      signal
    })
  }

  async getMailbox(
    companyKey: string,
    personId: string,
    signal?: AbortSignal
  ): Promise<MailboxResponse> {
    return this.request(
      'GET',
      `/companies/${encodeURIComponent(companyKey)}/people/${encodeURIComponent(personId)}/mailbox`,
      MailboxResponseSchema,
      { signal }
    )
  }

  async say(
    companyKey: string,
    personId: string,
    input: SayInput,
    signal?: AbortSignal
  ): Promise<SayResponse> {
    return this.request(
      'POST',
      `/companies/${encodeURIComponent(companyKey)}/people/${encodeURIComponent(personId)}/say`,
      SayResponseSchema,
      { body: input, signal }
    )
  }

  async abort(companyKey: string, personId: string, signal?: AbortSignal): Promise<AbortResponse> {
    return this.request(
      'POST',
      `/companies/${encodeURIComponent(companyKey)}/people/${encodeURIComponent(personId)}/abort`,
      AbortResponseSchema,
      { body: {}, signal }
    )
  }

  // NO `startPerson`. Which people are up is chiefd's roster decision — it
  // converges the host from the durable roster — and this server has no route
  // that starts one. The method dialed `…/people/:id/start`, which has never
  // existed here: every click 404'd and rendered the 404 as a pane error. See
  // the deletion note in `types/ChiefApi.ts`.

  // NO `newSession` and NO `compactSession`. Both dialed routes this server
  // has never served (`…/session/new`, `…/session/compact`), and neither is a
  // missing handler: a fresh session is a durable maintenance protocol in
  // chiefd, not one call a browser can make on an agent's behalf. See the
  // deletion note in `types/ChiefApi.ts`.

  async stopCompany(companyKey: string, signal?: AbortSignal): Promise<StopResponse> {
    return this.request(
      'POST',
      `/companies/${encodeURIComponent(companyKey)}/stop`,
      StopResponseSchema,
      {
        body: {},
        signal
      }
    )
  }

  // ---- staffing ------------------------------------------------------------
  // Intent in, outcome out. The provider observation, the model authority and
  // chiefd's preview/commit handshake are all apps/api's — see the staffing
  // section of `types/ChiefApi.ts` for why the browser cannot hold them.

  async createDepartment(
    companyKey: string,
    request: CreateDepartmentRequest,
    signal?: AbortSignal
  ): Promise<CreateDepartmentResponse> {
    return this.request(
      'POST',
      `/companies/${encodeURIComponent(companyKey)}/departments`,
      CreateDepartmentResponseSchema,
      { body: request, signal }
    )
  }

  async hirePerson(
    companyKey: string,
    request: HirePersonRequest,
    signal?: AbortSignal
  ): Promise<HirePersonResponse> {
    return this.request(
      'POST',
      `/companies/${encodeURIComponent(companyKey)}/people/hire`,
      HirePersonResponseSchema,
      { body: request, signal }
    )
  }

  /** Move a person to another department. `reason` is the operator's own
   * words and is carried into the durable record, not interpreted here. */
  async transferPerson(
    companyKey: string,
    personId: string,
    destinationId: string,
    reason: string,
    signal?: AbortSignal
  ): Promise<StaffingApplied> {
    return this.request(
      'POST',
      `${personPath(companyKey, personId)}/transfer`,
      StaffingAppliedSchema,
      { body: { destinationId, reason }, signal }
    )
  }

  /** Fire. The verb is `offboard` because that is the route and the durable
   * event; the UI says "Offboard" for the same reason. */
  async offboardPerson(
    companyKey: string,
    personId: string,
    reason: string,
    signal?: AbortSignal
  ): Promise<StaffingApplied> {
    return this.request(
      'POST',
      `${personPath(companyKey, personId)}/offboard`,
      StaffingAppliedSchema,
      { body: { reason }, signal }
    )
  }

  /** Re-parent a department — the restructure verb. */
  async reparentDepartment(
    companyKey: string,
    departmentId: string,
    newParentId: string,
    signal?: AbortSignal
  ): Promise<StaffingApplied> {
    const company = encodeURIComponent(companyKey)
    const department = encodeURIComponent(departmentId)
    return this.request(
      'POST',
      `/companies/${company}/departments/${department}/reparent`,
      StaffingAppliedSchema,
      { body: { newParentId }, signal }
    )
  }

  /** Stop a unit without deleting its history.
   *
   * The route takes NO body — it reads only its two path parameters — and the
   * `{}` below is this service's uniform POST payload, not a request shape.
   *
   * The response is a UNION, unlike every neighbour above: chiefd answers this
   * verb with a refusal AS A VALUE on a 200 (`AtomicDirectOutcome`), so a
   * caller that only ever awaits this promise learns nothing about a pause
   * chiefd declined. See `DepartmentStateChangeResponse`. */
  async pauseDepartment(
    companyKey: string,
    departmentId: string,
    signal?: AbortSignal
  ): Promise<DepartmentStateChangeResponse> {
    const company = encodeURIComponent(companyKey)
    const department = encodeURIComponent(departmentId)
    return this.request(
      'POST',
      `/companies/${company}/departments/${department}/pause`,
      DepartmentStateChangeResponseSchema,
      { body: {}, signal }
    )
  }

  /** Bring a paused unit back — the exact inverse of `pauseDepartment`, with
   * the same refusal-as-a-value answer. Which people come back and which panes
   * are rebuilt is chiefd's decision, derived from what it recorded when the
   * unit was paused. */
  async resumeDepartment(
    companyKey: string,
    departmentId: string,
    signal?: AbortSignal
  ): Promise<DepartmentStateChangeResponse> {
    const company = encodeURIComponent(companyKey)
    const department = encodeURIComponent(departmentId)
    return this.request(
      'POST',
      `/companies/${company}/departments/${department}/resume`,
      DepartmentStateChangeResponseSchema,
      { body: {}, signal }
    )
  }
}

/* eslint-disable lucy/no-json-stringify */
// @tribes-terminal/foundation's toJsonTreeString is not a dependency
// anywhere in this workspace (same reasoning as @chief/chiefing's
// FetchTransport.ts, E2-S1) — this is the one seam that serializes every
// outgoing request body.
function serializeRequestBody(body: unknown): string {
  return JSON.stringify(body ?? {})
}
/* eslint-enable lucy/no-json-stringify */
