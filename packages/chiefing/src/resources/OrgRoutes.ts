// Shared private post/decode internals for the six resource families this
// story fills (Manifest, Aggregates, Mailbox, PersonContracts,
// RowStores). Internal module — not exported from the barrel (src/index.ts).
//
// One JSON POST + status dispatch used by every method in this story:
//   2xx -> parse; any REFUSAL_STATUSES status with a {code, detail} body ->
//   a refusal error carrying both; anything else -> ChiefdUnavailableError
//     (kind 'http-error' | 'malformed-body'), carrying chiefd's detail too.
// One row-read normalizer: {found, doc?|document?|ledger?|mailbox?|manifest?,
//   seq, unchanged?} -> RowReadResult (string) or OrgRowReadResult<T> (parsed
//   via JSON.parse).

import { ChiefdUnavailableError, OrgRowRefusalError } from '../Errors.js'
import { isNullish } from '../Nullish.js'
import type {
  DecodedRefusal,
  OrgRowReadResult,
  OrgRowReadResultWithSeq,
  ReadOpts,
  RowReadResult,
  WireRowRead
} from '../types/OrgDocs.js'
import type { HttpResponse, HttpTransport } from '../types/Transport.js'

// Company keying is no longer a computation. `keyedSlug(slug, root)` lived
// here and rewrote every request's `slug` into the composite
// `documentKey(slug, orgsRoot)`, because one slug under two data roots was two
// companies. A company is a DIRECTORY now: its key is `sha256(dir)[..12]`,
// minted once and SERVED — on the beacond row (`CompanyRow.key`) and in the
// rendezvous a daemon publishes into its own directory. Every caller reads
// that field and passes it as `slug`; nothing derives it a second time.

export function decodeRefusal(body: string): DecodedRefusal {
  try {
    const parsed: { code?: unknown; refused?: unknown; detail?: unknown; message?: unknown } =
      JSON.parse(body)
    const code =
      typeof parsed.code === 'string'
        ? parsed.code
        : typeof parsed.refused === 'string'
          ? parsed.refused
          : 'error'
    const detail =
      typeof parsed.detail === 'string'
        ? parsed.detail
        : typeof parsed.message === 'string'
          ? parsed.message
          : body.trim()
    return { code, detail }
  } catch {
    return { code: 'error', detail: body.trim() }
  }
}

/**
 * **The statuses chiefd answers with an actionable `{code, detail}` refusal.**
 *
 * This is one half of a two-sided contract. The other half is
 * `REFUSAL_STATUSES` in
 * `apps/chiefd/crates/chiefd-api/src/docstore/route_error.rs`, and
 * `scripts/test/refusal-taxonomy.test.mjs` fails if the two ever differ —
 * because a status chiefd means as a refusal and this client reads as an
 * outage is precisely the defect this set exists to close.
 *
 * The set was `{400, 404, 422}`, and everything else — including the **409**
 * the task family answers `not_terminal` / `illegal_transition` /
 * `not_blocked` with, and the 409 a lost fence carries — reached the agent as
 * `chiefd unavailable (http-error)`. An agent told "unavailable" retries; an
 * agent told "not terminal" acts. The whole cost of the missing entries was
 * paid in retries against rules that were never going to answer differently.
 *
 * 401 and 403 joined in #751/P7, when authentication stopped being a property
 * of the terminal pane a caller descended from and became a credential the
 * caller presents. Landing an auth answer in the `ChiefdUnavailableError`
 * branch would have been wrong twice over: the agent would be told "chiefd is
 * unavailable" instead of the one thing it needed to fix, AND
 * `isTransientChiefdError` would have sent it back around the ladder to be
 * refused identically every time.
 *
 * What is deliberately NOT here, and why each one is a different instruction:
 *
 * * **429** — chiefd waited its documented ladder and could not proceed. Back
 *   off and retry; there is no rule to act on. `isTransientChiefdError` says so.
 * * **503** — chiefd is not currently serving (starting, quiescing, no runtime
 *   host capability on this process). Retry later.
 * * **500** — chiefd faulted. An operator, not a retry.
 */
export const REFUSAL_STATUSES: readonly number[] = [400, 401, 403, 404, 409, 422]

/** True iff `status` is one of {@link REFUSAL_STATUSES}. */
export function isRefusalStatus(status: number): boolean {
  return REFUSAL_STATUSES.includes(status)
}

/** Shared status dispatch for ONE already-received response — never issues a
 * request itself. It is a single function, and deliberately so: a prior
 * version had a separate CAS poster delegate to `postOrgRoute` on any non-409
 * status, which issued a SECOND request — double-posting every non-conflict
 * write and reporting the SECOND request's status/body instead of the
 * first's, which is exactly what masked a real chiefd route failure as a
 * generic `postOrgRoute`-frame error with no visibility into what the FIRST
 * response actually was. */
function dispatchOrgRouteResponse<T>(response: HttpResponse, url: string, path: string): T {
  if (response.status >= 200 && response.status < 300) {
    try {
      return JSON.parse(response.body)
    } catch (cause) {
      throw new ChiefdUnavailableError({
        kind: 'malformed-body',
        url,
        path,
        status: response.status,
        cause
      })
    }
  }
  if (isRefusalStatus(response.status)) {
    const { code, detail } = decodeRefusal(response.body)
    throw new OrgRowRefusalError({ status: response.status, code, detail })
  }
  // Not a refusal — but chiefd still said something, and an outage message
  // that names only the endpoint is useless to whoever has to act on it.
  // `kind` remains the only classifier; this is the sentence, not a branch.
  throw new ChiefdUnavailableError({
    kind: 'http-error',
    url,
    path,
    status: response.status,
    detail: decodeRefusal(response.body).detail
  })
}

/**
 * **The one `code` a lost CAS sequence answers with.**
 *
 * Every `*_publish_cas` method in `chiefd-core/src/actor/writer.rs` compares
 * the caller's `expectedSeq` against the company's `org_events` cursor and, on
 * a mismatch, returns `ChiefdError::conflict("seq-conflict", expected,
 * actual)`. That projects onto `WireError::Conflict` -> HTTP 409 with
 * `{"code":"seq-conflict","detail":"expected <n>, actual <m>"}`, per
 * `chiefd-api/src/docstore/route_error.rs`, the taxonomy's single mapping.
 *
 * `scripts/test/refusal-taxonomy.test.mjs` reads both files and fails if this
 * literal and Rust's ever diverge — the same two-sided contract
 * {@link REFUSAL_STATUSES} carries, for the same reason.
 */
export const SEQ_CONFLICT_CODE = 'seq-conflict'

/** One POST + status dispatch shared by every `OrgRowRefusalError`-throwing
 * family (Manifest, Aggregates, Mailbox, RowStores — every named
 * `/v1/org/*` route except person-contracts, which carries its own refusal
 * class per the Contract). `url` is diagnostic only (`ChiefdUnavailableError.url`);
 * a bare `HttpTransport` does not expose its base URL, so callers thread it
 * through from the client that already holds it. */
export async function postOrgRoute<T>(
  transport: HttpTransport,
  url: string,
  path: string,
  body: unknown
): Promise<T> {
  const response = await transport.post(path, body)
  return dispatchOrgRouteResponse<T>(response, url, path)
}

/** As `postOrgRoute`, discarding a successful body — still validates it is
 * JSON so a malformed 2xx surfaces as `ChiefdUnavailableError`, never silently
 * swallowed. */
export async function postOrgRouteVoid(
  transport: HttpTransport,
  url: string,
  path: string,
  body: unknown
): Promise<void> {
  await postOrgRoute<unknown>(transport, url, path, body)
}

/** Normalize the wire `doc`/`document`/`ledger`/`mailbox`/`manifest` field to
 * `document` — still the serialized inner JSON string. */
export function normalizeRowRead(wire: WireRowRead): RowReadResult {
  const document = wire.doc ?? wire.document ?? wire.ledger ?? wire.mailbox ?? wire.manifest
  return {
    found: wire.found,
    seq: wire.seq ?? 0,
    unchanged: wire.unchanged,
    document
  }
}

/** As `normalizeRowRead`, additionally parsing the payload into `T`. A
 * `found:true` with no payload (the `unchanged:true` short-circuit) parses to
 * `{found:true}` with `doc` left undefined — never re-fetched, never guessed. */
export function normalizeTypedRowRead<T>(wire: WireRowRead): OrgRowReadResult<T> {
  const normalized = normalizeRowRead(wire)
  if (!normalized.found || isNullish(normalized.document)) {
    return { found: normalized.found }
  }
  return { found: true, doc: JSON.parse(normalized.document) }
}

/** #950/#954: as `normalizeTypedRowRead`, additionally carrying `seq` --
 * used only by the `*PublishCas` read path. See
 * `OrgRowReadResultWithSeq`'s own doc comment for why this is a separate
 * function rather than a widened `normalizeTypedRowRead`. */
export function normalizeTypedRowReadWithSeq<T>(wire: WireRowRead): OrgRowReadResultWithSeq<T> {
  const normalized = normalizeRowRead(wire)
  if (!normalized.found || isNullish(normalized.document)) {
    return { found: normalized.found, seq: normalized.seq }
  }
  return { found: true, doc: JSON.parse(normalized.document), seq: normalized.seq }
}

/** Apply `opts.ifSeqNot` to a request body under its wire name. */
export function withReadOpts(
  body: Record<string, unknown>,
  opts: ReadOpts | undefined
): Record<string, unknown> {
  return isNullish(opts?.ifSeqNot) ? body : { ...body, ifSeqNot: opts.ifSeqNot }
}

/** Every request in this story that needs a caller-clock `at` stamp but whose
 * public method signature (frozen by the E0-S4 stub) does not accept one —
 * `RowStoresClient`'s fence-free `clear*` verbs. Chiefd's own routes read
 * `at` as a plain event-stamp string, never as a business decision; this is
 * transport plumbing, not the client-side policy Mandate 3 forbids. */
export function nowIso(): string {
  return new Date().toISOString()
}
