import { isNullish } from '../Nullish.js'
import type {
  OrgRowReadResult,
  OrgRowReadResultWithSeq,
  ReadOpts,
  WireRowRead
} from '../types/OrgDocs.js'
import type {
  ClearedResult,
  EventOnceMarkerInsertInput,
  InsertEventOnceMarkerResult,
  PruneEventOnceMarkersResult,
  SemanticQueueInsertResult
} from '../types/RowDocs.js'
import type { HttpTransport } from '../types/Transport.js'
import {
  normalizeTypedRowRead,
  normalizeTypedRowReadWithSeq,
  nowIso,
  postOrgRoute,
  postOrgRouteVoid,
  withReadOpts
} from './OrgRoutes.js'

/** 54 methods over one shared post/decode helper (`resources/OrgRoutes`).
 * Names preserved verbatim from `OrgRowStoreClient`
 * (org-row-stores.ts:343-412) so consumer migration is mechanical. Moved OFF
 * this surface: the person-lifecycle "start" verb (it lives on the staffing
 * resource instead — a staffing verb, not a row store). The
 * whole-company two-phase removal VERB (its PREPARE/FINALIZE protocol) is NOT
 * ported (ruling D24/F25 — that protocol is deleted, not relocated; building
 * a client for it here would itself be the stopgap D0 forbids).
 *
 * #751/G6: the `company-removal` ROW family
 * (`readCompanyRemoval`/`publishCompanyRemoval`/`clearCompanyRemoval`) is
 * deleted too, and the paragraph that used to sit here calling
 * `/v1/org/company-removal/*` "still-served" was the defect, not a stale
 * comment beside one. E7-S7 finished on the server: no crate registers those
 * three routes, and `chiefd-core/src/schema.rs:496-510` DROPs all four
 * `company_removal*` tables with `store/mod.rs:1323` asserting they cannot
 * survive an open. Three client methods survived, dialing routes that would
 * have 404'd, described by their own comments as live. Nothing caught it
 * because the route freeze was a hand-maintained fixture that had been
 * updated to match the client. It is now derived from the Rust router —
 * see `test/contract/RoutePathFreeze.test.ts`. */
export class RowStoresClient {
  constructor(
    protected readonly transport: HttpTransport,
    protected readonly url: string = ''
  ) {}

  private async readDoc<T>(
    path: string,
    slug: string,
    opts?: ReadOpts
  ): Promise<OrgRowReadResult<T>> {
    const wire = await postOrgRoute<WireRowRead>(
      this.transport,
      this.url,
      path,
      withReadOpts({ slug }, opts)
    )
    return normalizeTypedRowRead<T>(wire)
  }

  /** #950/#954: as `readDoc`, additionally carrying `seq` for a `*Cas`
   * read-modify-write's `expectedSeq`. Used only by the
   * operator-escalation-intents CAS read path -- every other reader of this
   * store family keeps calling the plain `readDoc`-backed method. */
  private async readDocWithSeq<T>(
    path: string,
    slug: string,
    opts?: ReadOpts
  ): Promise<OrgRowReadResultWithSeq<T>> {
    const wire = await postOrgRoute<WireRowRead>(
      this.transport,
      this.url,
      path,
      withReadOpts({ slug }, opts)
    )
    return normalizeTypedRowReadWithSeq<T>(wire)
  }

  private async publishDoc<T>(path: string, slug: string, doc: T): Promise<void> {
    /* eslint-disable lucy/no-json-stringify */
    // @tribes-terminal/foundation (toJsonTreeString/ensureJsonTreeString) is
    // not a dependency anywhere in this workspace (see FetchTransport.ts's
    // matching disable block). Every direct-atomic singleton route in this
    // Contract takes `{slug, doc: JSON.stringify(doc)}` verbatim
    // (org-row-stores.ts:82, `DirectOrgRowPublishRequest.doc`).
    const serializedDoc = JSON.stringify(doc)
    /* eslint-enable lucy/no-json-stringify */
    await postOrgRouteVoid(this.transport, this.url, path, {
      slug,
      doc: serializedDoc
    })
  }

  /** Fence-free CLEAR: unconditionally delete a store's rows. `at` is a
   * caller-clock event stamp the E0-S4 stub's frozen signature (`slug` only)
   * gives this client no way to accept, so it is synthesized here — plumbing
   * (a wall-clock read), never a business decision (Mandate 3). */
  private async clear(path: string, slug: string): Promise<ClearedResult> {
    return postOrgRoute(this.transport, this.url, path, { slug, at: nowIso() })
  }

  // session-epoch
  async readSessionEpoch<T = unknown>(slug: string, opts?: ReadOpts): Promise<OrgRowReadResult<T>> {
    return this.readDoc<T>('/v1/org/session-epoch/read', slug, opts)
  }

  // goal-delivery-quiesce. Named for a goal and NOT part of the goal feature:
  // this is the converge cycle's delivery-quiescence stamp, whose Rust writer
  // and reader both survive.
  async readGoalDeliveryQuiesce<T = unknown>(
    slug: string,
    opts?: ReadOpts
  ): Promise<OrgRowReadResult<T>> {
    return this.readDoc<T>('/v1/org/goal-delivery-quiesce/read', slug, opts)
  }

  // operator-escalation-push
  async readOperatorEscalationPush<T = unknown>(
    slug: string,
    opts?: ReadOpts
  ): Promise<OrgRowReadResult<T>> {
    return this.readDoc<T>('/v1/org/operator-escalation-push/read', slug, opts)
  }

  // runtime-owner
  async readRuntimeOwner<T = unknown>(slug: string, opts?: ReadOpts): Promise<OrgRowReadResult<T>> {
    return this.readDoc<T>('/v1/org/runtime-owner/read', slug, opts)
  }

  // launch-intent
  async readLaunchIntent<T = unknown>(slug: string, opts?: ReadOpts): Promise<OrgRowReadResult<T>> {
    return this.readDoc<T>('/v1/org/launch-intent/read', slug, opts)
  }
  async clearLaunchIntent(slug: string): Promise<ClearedResult> {
    return this.clear('/v1/org/launch-intent/clear', slug)
  }

  // mutation-journal
  async readMutationJournal<T = unknown>(
    slug: string,
    opts?: ReadOpts
  ): Promise<OrgRowReadResult<T>> {
    return this.readDoc<T>('/v1/org/mutation-journal/read', slug, opts)
  }

  // health-monitor
  async readHealthMonitor<T = unknown>(
    slug: string,
    opts?: ReadOpts
  ): Promise<OrgRowReadResult<T>> {
    return this.readDoc<T>('/v1/org/health-monitor/read', slug, opts)
  }

  // runtime
  async readRuntime<T = unknown>(slug: string, opts?: ReadOpts): Promise<OrgRowReadResult<T>> {
    return this.readDoc<T>('/v1/org/runtime/read', slug, opts)
  }
  async publishRuntime<T = unknown>(slug: string, doc: T): Promise<void> {
    await this.publishDoc('/v1/org/runtime/publish', slug, doc)
  }
  async clearRuntime(slug: string): Promise<ClearedResult> {
    return this.clear('/v1/org/runtime/clear', slug)
  }
  // TOMBSTONE (chief-home-is-cwd §4c): `prepareCeoOnly` (POST
  // /v1/org/runtime/prepare-ceo-only) and `readCeoBootLease` (POST
  // /v1/org/ceo-boot-lease/read) stood here. Both routes are deleted with the
  // daemon-side CEO boot — the daemon brings up no pane, so it can neither be
  // asked to prepare for one nor hold a lease while it does.

  // converge-safety (#861): the STORED converge/apply actuation mode. The
  // route returns `reconstruct()`'s raw doc, never the breaker-folded
  // `effective_config()` projection — callers read `.actuationMode` off the
  // result knowing it is the real stored value, not a computed one.
  async readConvergeSafety<T = unknown>(
    slug: string,
    opts?: ReadOpts
  ): Promise<OrgRowReadResult<T>> {
    return this.readDoc<T>('/v1/org/converge-safety/read', slug, opts)
  }

  // operator-escalation-intents
  async readOperatorEscalationIntents<T = unknown>(
    slug: string,
    opts?: ReadOpts
  ): Promise<OrgRowReadResult<T>> {
    return this.readDoc<T>('/v1/org/operator-escalation-intents/read', slug, opts)
  }
  /** Read companion carrying `seq`, for a caller that wants the audit cursor
   * alongside the document. */
  async readOperatorEscalationIntentsWithSeq<T = unknown>(
    slug: string,
    opts?: ReadOpts
  ): Promise<OrgRowReadResultWithSeq<T>> {
    return this.readDocWithSeq<T>('/v1/org/operator-escalation-intents/read', slug, opts)
  }
  async insertOperatorEscalationIntent<T = unknown>(
    slug: string,
    intent: T
  ): Promise<SemanticQueueInsertResult> {
    return postOrgRoute<SemanticQueueInsertResult>(
      this.transport,
      this.url,
      '/v1/org/operator-escalation-intents/insert',
      {
        slug,
        intent
      }
    )
  }

  // event-journal — DocStore-direct (no live-company gate, no org_events
  // fence): an independent atomic marker keyed by sha256(id). The wire reads
  // it back under `marker`, not `doc`/`document` — outside the generic
  // row-read normalizer, decoded here directly.
  async readEventOnceMarker<T = unknown>(
    slug: string,
    keyDigest: string
  ): Promise<OrgRowReadResult<T>> {
    const wire = await postOrgRoute<{ found: boolean; marker?: string }>(
      this.transport,
      this.url,
      '/v1/org/event-journal/read',
      { slug, keyDigest }
    )
    if (!wire.found || isNullish(wire.marker)) return { found: false }
    return { found: true, doc: JSON.parse(wire.marker) }
  }
  async insertEventOnceMarker(
    slug: string,
    marker: EventOnceMarkerInsertInput
  ): Promise<InsertEventOnceMarkerResult> {
    return postOrgRoute(this.transport, this.url, '/v1/org/event-journal/insert-if-absent', {
      slug,
      keyDigest: marker.keyDigest,
      id: marker.id,
      event: marker.event,
      createdAtMs: marker.createdAtMs
    })
  }
  async pruneEventOnceMarkers(
    slug: string,
    olderThanMs: number
  ): Promise<PruneEventOnceMarkersResult> {
    return postOrgRoute(this.transport, this.url, '/v1/org/event-journal/prune', {
      slug,
      olderThanMs
    })
  }

  /** Async read-compute-publish (from org-row-stores.ts:537). A direct
   * normalized publish is atomic inside chiefd, so this reads once, computes
   * once, and publishes once — no client-side CAS, no cursor, no retry
   * outcome (Mandate 3). "Unchanged" is a reference-identity check only
   * (the mutator handing back the exact document it was given) — a thin
   * mechanical skip, never a value comparison/diff, which would be the
   * client-side policy Mandate 3 forbids. */
  async rowMutate<T>(
    read: () => Promise<OrgRowReadResult<T>>,
    publish: (doc: T) => Promise<void>,
    mutator: (current: T | undefined) => T
  ): Promise<T> {
    const current = await read()
    const existing = current.found ? current.doc : undefined
    const value = mutator(existing)
    if (current.found && value === existing) {
      return value
    }
    await publish(value)
    return value
  }
}
