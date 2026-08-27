import { ChiefdUnavailableError, OrgRowRefusalError } from '@/Errors'
import { isNullish } from '@/Nullish'
import { isRefusalStatus } from '@/resources/OrgRoutes'
import type { AtomicDirectOutcome } from '@/types/OrgDocs'
import type { StartPersonResult } from '@/types/RowDocs'
import type {
  AtomicCreateDepartmentOutcome,
  AtomicDepartmentHead,
  AtomicDepartmentStaff,
  AtomicDepartmentUnit,
  AtomicHireOutcome,
  AtomicPersonSeed,
  AtomicRemoveDepartmentOutcome,
  AtomicReparentDepartmentOutcome,
  AtomicStaffingRequester,
  AtomicTransferPersonOutcome,
  Refusal
} from '@/types/Staffing'
import type { HttpTransport } from '@/types/Transport'

/** `HttpTransport` carries no base URL of its own (only `FetchTransport`'s
 * connect-level failures know one, and those already throw with it). Every
 * error this file constructs from a decoded HTTP response — as opposed to a
 * transport-level rejection — carries an empty `url`; `kind`/`status`/`path`
 * are the fields callers branch on.
 *
 * `JSON.parse` returns `any`, so `return JSON.parse(body)` from a function
 * declared `T` needs no assertion — the shape is validated by each call
 * site's own field checks, never trusted wholesale. */
function decodeJson<T>(path: string, body: string): T {
  try {
    return JSON.parse(body)
  } catch (error) {
    throw new ChiefdUnavailableError({ kind: 'malformed-body', url: '', path, cause: error })
  }
}

function malformedBody(path: string): ChiefdUnavailableError {
  return new ChiefdUnavailableError({ kind: 'malformed-body', url: '', path })
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && !isNullish(value) && !Array.isArray(value)
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry): entry is string => typeof entry === 'string')
}

/** A 422 body decoded as a value: `{code, detail}` on the wire becomes
 * `{refused, detail}`. Never thrown, never retried. Throws
 * `ChiefdUnavailableError` (kind `'malformed-body'`) when the body is not
 * that exact shape — a refusal chiefd cannot even describe is not a value a
 * caller can branch on. */
function decodeRefusal(path: string, body: string): Refusal {
  const parsed = decodeJson<unknown>(path, body)
  if (!isRecord(parsed) || typeof parsed.code !== 'string' || typeof parsed.detail !== 'string') {
    throw malformedBody(path)
  }
  return { refused: parsed.code, detail: parsed.detail }
}

/** Decode the uniform response of a named, writer-serialized org operation
 * (org-durable-store.ts's `directOrgOperation`, ported async). NEVER
 * retried: a refused verb re-sent is a different op. */
async function directOutcome(
  transport: HttpTransport,
  path: string,
  body: Record<string, unknown>
): Promise<AtomicDirectOutcome> {
  const response = await transport.post(path, body)
  if (isRefusalStatus(response.status)) return decodeRefusal(path, response.body)
  if (response.status < 200 || response.status >= 300) {
    throw new ChiefdUnavailableError({ kind: 'http-error', url: '', path, status: response.status })
  }
  const parsed = decodeJson<unknown>(path, response.body)
  if (!isRecord(parsed) || parsed.applied !== true) {
    throw malformedBody(path)
  }
  return { applied: true }
}

/** Decode a named direct transfer operation (org-durable-store.ts's
 * `directTransferOutcome`, ported async) — it never exposes a
 * compare-and-swap fence key. */
async function transferOutcome(
  transport: HttpTransport,
  path: string,
  body: Record<string, unknown>
): Promise<{ applied: true; moved: string[] } | Refusal> {
  const response = await transport.post(path, body)
  if (isRefusalStatus(response.status)) return decodeRefusal(path, response.body)
  if (response.status < 200 || response.status >= 300) {
    throw new ChiefdUnavailableError({ kind: 'http-error', url: '', path, status: response.status })
  }
  const parsed = decodeJson<unknown>(path, response.body)
  if (!isRecord(parsed) || parsed.applied !== true || !isStringArray(parsed.moved)) {
    throw malformedBody(path)
  }
  return { applied: true, moved: parsed.moved }
}

function optional<K extends string, V>(key: K, value: V | undefined): Partial<Record<K, V>> {
  if (isNullish(value)) return {}
  const out: Partial<Record<K, V>> = {}
  out[key] = value
  return out
}

// A transport wrapper used to rewrite every request body's `slug` into the
// composite `documentKey(slug, orgsRoot)` before it left this class. It was
// done at the TRANSPORT rather than per method because this class has ~30
// verbs, several of which post through the module-level `directOutcome`
// helper rather than through `this.transport`, and a per-method rewrite had to
// be remembered at each one — it was remembered at none, and every staffing
// call in the product sent a bare slug chiefd answered `404 unknown-company`
// to. Nothing rewrites anything now: the caller's `slug` IS the company key
// (`sha256(dir)[..12]`, served on the beacond row and in the daemon
// rendezvous), so there is no second spelling for a call site to forget.

/** Refusal-as-value. Signatures preserved from ChiefdBackend
 * (org-durable-store.ts:1009+), made async. */
export class StaffingClient {
  constructor(protected readonly transport: HttpTransport) {}

  /** Relocated from `OrgRowStoreClient.startPerson` (org-row-stores.ts) per
   * the epic Contract — the ONE verb on this client that is NOT
   * refusal-as-value: it keeps its row-style thrown `OrgRowRefusalError`,
   * unchanged from its prior home. */
  async startPerson(slug: string, personId: string): Promise<{ applied: true }> {
    const path = '/v1/org/person/start'
    const response = await this.transport.post(path, { slug, personId })
    if (response.status >= 200 && response.status < 300) {
      return decodeJson<StartPersonResult>(path, response.body)
    }
    if (isRefusalStatus(response.status)) {
      let code = 'error'
      let detail = response.body.trim()
      try {
        const parsed: unknown = JSON.parse(response.body)
        if (isRecord(parsed)) {
          if (typeof parsed.code === 'string') code = parsed.code
          if (typeof parsed.detail === 'string') detail = parsed.detail
        }
      } catch {
        // Plain-text body; keep the trimmed text as the diagnostic detail.
      }
      throw new OrgRowRefusalError({ status: response.status, code, detail })
    }
    throw new ChiefdUnavailableError({ kind: 'http-error', url: '', path, status: response.status })
  }

  async shutdownPerson(
    slug: string,
    personId: string,
    kind: 'commanded' | 'settle',
    opts: { intentId?: string } = {}
  ): Promise<{ applied: true; transitionId: string } | Refusal> {
    const path = '/v1/org/person/shutdown'
    const response = await this.transport.post(path, {
      slug,
      personId,
      kind,
      ...optional('intentId', opts.intentId)
    })
    if (isRefusalStatus(response.status)) return decodeRefusal(path, response.body)
    if (response.status < 200 || response.status >= 300) {
      throw new ChiefdUnavailableError({
        kind: 'http-error',
        url: '',
        path,
        status: response.status
      })
    }
    const parsed = decodeJson<unknown>(path, response.body)
    if (!isRecord(parsed) || parsed.applied !== true || typeof parsed.transitionId !== 'string') {
      throw malformedBody(path)
    }
    return { applied: true, transitionId: parsed.transitionId }
  }

  async hirePerson(
    slug: string,
    personId: string,
    departmentId: string,
    seed: AtomicPersonSeed,
    requester: AtomicStaffingRequester
  ): Promise<AtomicHireOutcome> {
    const path = '/v1/org/person/hire'
    const response = await this.transport.post(path, {
      slug,
      personId,
      departmentId,
      requester,
      name: seed.name,
      title: seed.title,
      mandate: seed.mandate,
      kind: seed.kind,
      employmentState: seed.employmentState,
      activation: seed.activation,
      tools: seed.tools,
      prompts: seed.prompts
    })
    if (isRefusalStatus(response.status)) return decodeRefusal(path, response.body)
    if (response.status < 200 || response.status >= 300) {
      throw new ChiefdUnavailableError({
        kind: 'http-error',
        url: '',
        path,
        status: response.status
      })
    }
    const parsed = decodeJson<unknown>(path, response.body)
    if (!isRecord(parsed) || parsed.applied !== true) throw malformedBody(path)
    return { applied: true }
  }

  async offboardPerson(
    slug: string,
    personId: string,
    opts: { actor?: string } = {}
  ): Promise<AtomicDirectOutcome> {
    return directOutcome(this.transport, '/v1/org/person/offboard', {
      slug,
      personId,
      ...optional('actor', opts.actor)
    })
  }

  async benchPerson(slug: string, personId: string): Promise<AtomicDirectOutcome> {
    return directOutcome(this.transport, '/v1/org/person/bench', { slug, personId })
  }

  async benchPersonLifecycle(slug: string, personId: string): Promise<AtomicDirectOutcome> {
    return directOutcome(this.transport, '/v1/org/person/bench-lifecycle', { slug, personId })
  }

  async recallPerson(slug: string, personId: string): Promise<AtomicDirectOutcome> {
    return directOutcome(this.transport, '/v1/org/person/recall', { slug, personId })
  }

  async transferPerson(
    slug: string,
    personId: string,
    destinationId: string,
    opts: { intent?: string; actor?: string } = {}
  ): Promise<AtomicTransferPersonOutcome> {
    return transferOutcome(this.transport, '/v1/org/person/transfer', {
      slug,
      personId,
      destinationId,
      ...optional('intent', opts.intent),
      ...optional('actor', opts.actor)
    })
  }

  async appointDepartmentHead(
    slug: string,
    departmentId: string,
    successorPersonId: string,
    opts: { demoteToDepartmentId?: string } = {}
  ): Promise<AtomicDirectOutcome> {
    return directOutcome(this.transport, '/v1/org/person/appoint-head', {
      slug,
      departmentId,
      successorPersonId,
      ...optional('demoteToDepartmentId', opts.demoteToDepartmentId)
    })
  }

  async replaceHeadAndOffboard(
    slug: string,
    headPersonId: string,
    successorPersonId: string
  ): Promise<AtomicDirectOutcome> {
    return directOutcome(this.transport, '/v1/org/person/replace-head-and-offboard', {
      slug,
      headPersonId,
      successorPersonId
    })
  }

  async createDepartment(
    slug: string,
    departmentId: string,
    parentId: string,
    name: string,
    head: AtomicDepartmentHead,
    requester: AtomicStaffingRequester,
    opts: {
      purpose?: string
      reason?: string
      unit?: AtomicDepartmentUnit
      staff?: AtomicDepartmentStaff[]
    } = {}
  ): Promise<AtomicCreateDepartmentOutcome> {
    const path = '/v1/org/department/create'
    const response = await this.transport.post(path, {
      slug,
      departmentId,
      parentId,
      name,
      head,
      ...optional('unit', opts.unit),
      ...optional('staff', opts.staff),
      requester,
      ...optional('purpose', opts.purpose),
      ...optional('reason', opts.reason)
    })
    if (isRefusalStatus(response.status)) return decodeRefusal(path, response.body)
    if (response.status < 200 || response.status >= 300) {
      throw new ChiefdUnavailableError({
        kind: 'http-error',
        url: '',
        path,
        status: response.status
      })
    }
    const parsed = decodeJson<unknown>(path, response.body)
    if (!isRecord(parsed) || parsed.applied !== true || typeof parsed.departmentId !== 'string') {
      throw malformedBody(path)
    }
    return { applied: true, departmentId: parsed.departmentId }
  }

  async reparentDepartment(
    slug: string,
    departmentId: string,
    newParentId: string
  ): Promise<AtomicReparentDepartmentOutcome> {
    const path = '/v1/org/department/reparent'
    const response = await this.transport.post(path, { slug, departmentId, newParentId })
    if (isRefusalStatus(response.status)) return decodeRefusal(path, response.body)
    if (response.status < 200 || response.status >= 300) {
      throw new ChiefdUnavailableError({
        kind: 'http-error',
        url: '',
        path,
        status: response.status
      })
    }
    const parsed = decodeJson<unknown>(path, response.body)
    if (!isRecord(parsed) || parsed.applied !== true || typeof parsed.departmentId !== 'string') {
      throw malformedBody(path)
    }
    return { applied: true, departmentId: parsed.departmentId }
  }

  async pauseDepartment(slug: string, departmentId: string): Promise<AtomicDirectOutcome> {
    return directOutcome(this.transport, '/v1/org/department/pause', { slug, departmentId })
  }

  async resumeDepartment(slug: string, departmentId: string): Promise<AtomicDirectOutcome> {
    return directOutcome(this.transport, '/v1/org/department/resume', { slug, departmentId })
  }

  async resumeDepartments(
    slug: string,
    departmentIds: string[],
    skipActive = false
  ): Promise<AtomicDirectOutcome> {
    return directOutcome(this.transport, '/v1/org/department/resume-many', {
      slug,
      departmentIds,
      skipActive
    })
  }

  async moveDepartmentMembers(
    slug: string,
    fromDepartmentId: string,
    destinationId: string,
    personIds: string[],
    opts: { intent?: string } = {}
  ): Promise<AtomicTransferPersonOutcome> {
    return transferOutcome(this.transport, '/v1/org/department/move-members', {
      slug,
      fromDepartmentId,
      destinationId,
      personIds,
      ...optional('intent', opts.intent)
    })
  }

  async removeDepartmentTree(
    slug: string,
    departmentId: string
  ): Promise<AtomicRemoveDepartmentOutcome> {
    const path = '/v1/org/department/remove-tree'
    const response = await this.transport.post(path, { slug, departmentId })
    if (isRefusalStatus(response.status)) return decodeRefusal(path, response.body)
    if (response.status < 200 || response.status >= 300) {
      throw new ChiefdUnavailableError({
        kind: 'http-error',
        url: '',
        path,
        status: response.status
      })
    }
    const parsed = decodeJson<unknown>(path, response.body)
    if (
      !isRecord(parsed) ||
      parsed.applied !== true ||
      !isStringArray(parsed.removedDepartmentIds) ||
      !isStringArray(parsed.departedPersonIds)
    ) {
      throw malformedBody(path)
    }
    return {
      applied: true,
      removedDepartmentIds: parsed.removedDepartmentIds,
      departedPersonIds: parsed.departedPersonIds
    }
  }

  async reactivateExecutiveRoot(slug: string): Promise<AtomicDirectOutcome> {
    return directOutcome(this.transport, '/v1/org/department/reactivate-executive-root', { slug })
  }
}
