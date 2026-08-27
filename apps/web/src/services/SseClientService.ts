/**
 * The browser's one streaming-fetch implementation for E5's doc, person,
 * and lifecycle streams. It mirrors chiefing's event-driven reconnect,
 * heartbeat, cursor, reorg, and ref-count invariants without importing its
 * server-runtime implementation or falling back to polling.
 */
import { appPath } from '@/common/AppRoutes'
import { ChiefApiError } from '@/types/ApiErrors'
import type { FetchImpl } from '@/types/Fetch'
import { parseSessionEvent } from '@/types/SessionEvents'
import type {
  DocChangeEvent,
  LifecycleTerminal,
  PersonHostEvent,
  PersonSessionEventFrame,
  PersonSessionState,
  SseChannelState,
  SseConnection,
  SseConnectionOptions,
  SseFrame,
  SseHubDeps,
  SseSubscription,
  StreamLifecycleOptions,
  SubscribeDocEventsOptions,
  SubscribePersonStreamOptions
} from '@/types/Sse'
import {
  DocChangeEventSchema,
  LifecycleBootedFrameSchema,
  LifecycleCreatedFrameSchema,
  LifecycleFailedFrameSchema,
  LifecyclePhaseFrameSchema,
  PersonHostEventSchema,
  PersonSessionStateSchema
} from '@/types/Sse'
import { computeBackoffDelayMs, createSseFrameParser } from '@/utils/SseFrames'

const DEFAULT_CONNECT_TIMEOUT_MS = 5_000
const DEFAULT_HEARTBEAT_TIMEOUT_MS = 45_000
const DEFAULT_BACKOFF_INITIAL_MS = 1_000
const DEFAULT_BACKOFF_MAX_MS = 30_000

type TimeoutHandle = ReturnType<typeof setTimeout>

function callSafely(callback: () => void): void {
  try {
    callback()
  } catch {
    // A consumer render bug must never kill the shared stream reader.
  }
}

function parseFrameJson(frame: SseFrame): unknown | undefined {
  if (typeof frame.data === 'undefined') return undefined
  try {
    return JSON.parse(frame.data)
  } catch {
    return undefined
  }
}

/* eslint-disable lucy/no-json-stringify */
// The streaming POST body is an app-API wire payload. This is the one SSE
// request seam, analogous to ChiefApiClientService's request serializer.
function serializeBody(body: unknown): string {
  return JSON.stringify(body ?? {})
}
/* eslint-enable lucy/no-json-stringify */

/**
 * Resolve a request path against the configured base.
 *
 * An EMPTY base is the same-origin answer, not a missing value: with apps/api
 * deleted the browser talks to this app's own origin, and `publicApiBaseUrl()`
 * returns `''` to say so. `new URL(path, '')` throws "Invalid URL" — a
 * relative base is not a base — so same-origin resolves against the document's
 * own origin instead.
 *
 * `location` is absent on the server (route handlers, tests): there is no
 * "current origin" to resolve against there, so a same-origin base is a
 * caller error rather than something to invent a host for.
 */
function appUrl(baseUrl: string, relativePath: string): URL {
  // The prefix belongs HERE, at the one place a path becomes a URL: every
  // caller passes a route-handler path, and a caller that remembered the
  // prefix itself would be the drift this centralisation exists to stop.
  const path = appPath(relativePath)
  if (baseUrl !== '') return new URL(path, baseUrl)
  const origin = globalThis.location?.origin
  if (typeof origin !== 'string') {
    throw new Error(
      `cannot resolve "${path}" against a same-origin base outside a browser — ` +
        'pass an absolute baseUrl when constructing this client server-side'
    )
  }
  return new URL(path, origin)
}

function personEventsPath(companyKey: string, personId: string): string {
  // `/stream`, not `/events`. The two names are not interchangeable and the
  // mismatch was silent: this client dialled `/events` while the server route
  // is `stream/route.ts`, so the person channel 404'd on every page while the
  // COMPANY feed (also `/events`) was fine. Two SSE channels with names that
  // differ by one word is exactly the kind of thing a fake papers over.
  const companyPath = `/companies/${encodeURIComponent(companyKey)}`
  return `${companyPath}/people/${encodeURIComponent(personId)}/stream`
}

function parseSessionCursor(
  id: string | undefined
): { generation: number; seq: number } | undefined {
  if (typeof id === 'undefined') return undefined
  const parts = id.split('.')
  if (parts.length !== 2) return undefined
  const generationPart = parts[0]
  const seqPart = parts[1]
  if (typeof generationPart !== 'string' || typeof seqPart !== 'string') return undefined
  const generation = Number(generationPart)
  const seq = Number(seqPart)
  if (!Number.isSafeInteger(generation) || !Number.isSafeInteger(seq)) return undefined
  if (generation < 0 || seq < 0) return undefined
  return { generation, seq }
}

/** One connection with no hidden periodic work: every timer is a single
 * event-driven timeout and `close()` clears all of them. */
class SseClientService implements SseConnection {
  private readonly url: string
  private readonly method: 'GET' | 'POST'
  private readonly body: unknown
  private readonly accessToken: () => string | null
  private readonly onFrame: (frame: SseFrame) => void
  private readonly onChannelState: ((state: SseChannelState) => void) | undefined
  private readonly connectTimeoutMs: number
  private readonly heartbeatTimeoutMs: number
  private readonly backoffInitialMs: number
  private readonly backoffMaxMs: number
  private readonly retry: boolean
  private readonly fetchImpl: FetchImpl
  private readonly parser = createSseFrameParser()

  private closed = false
  private attempt = 0
  private cursor: string | undefined
  private channelState: SseChannelState | undefined
  private generation = 0
  private controller: AbortController | undefined
  private connectTimer: TimeoutHandle | undefined
  private heartbeatTimer: TimeoutHandle | undefined
  private reconnectTimer: TimeoutHandle | undefined

  constructor(options: SseConnectionOptions) {
    // Parsing ensures the contract's absolute URL requirement before any
    // request can be issued. The supplied URL remains the exact request URL;
    // resume data stays in a header, never a credential-bearing query string.
    const parsed = new URL(options.url)
    if (parsed.protocol.length === 0 || parsed.host.length === 0) {
      throw new Error('SSE connections require an absolute app-API URL')
    }
    this.url = parsed.toString()
    this.method = options.method ?? 'GET'
    this.body = options.body
    this.accessToken = options.accessToken
    this.onFrame = options.onFrame
    this.onChannelState = options.onChannelState
    this.connectTimeoutMs = options.connectTimeoutMs ?? DEFAULT_CONNECT_TIMEOUT_MS
    this.heartbeatTimeoutMs = options.heartbeatTimeoutMs ?? DEFAULT_HEARTBEAT_TIMEOUT_MS
    this.backoffInitialMs = options.backoffInitialMs ?? DEFAULT_BACKOFF_INITIAL_MS
    this.backoffMaxMs = options.backoffMaxMs ?? DEFAULT_BACKOFF_MAX_MS
    this.retry = options.retry ?? true
    // `fetch` MUST be bound to the global before it is stored on an instance.
    // Called as `this.fetchImpl(...)`, an unbound browser `fetch` receives this
    // service as its `this` and Chrome throws
    // "Failed to execute 'fetch' on 'Window': Illegal invocation" — every
    // request from the company page failed this way. A caller-supplied
    // `fetchImpl` is already a plain function and binds harmlessly.
    this.fetchImpl = options.fetchImpl ?? fetch.bind(globalThis)
    this.cursor = options.lastEventId
    this.connect()
  }

  close(): void {
    if (this.closed) return
    this.closed = true
    this.generation += 1
    this.abortCurrentRequest()
    this.clearReconnectTimer()
  }

  lastEventId(): string | undefined {
    return this.cursor
  }

  private connect(): void {
    if (this.closed) return
    this.clearReconnectTimer()
    this.abortCurrentRequest()
    this.parser.reset()
    this.generation += 1
    const generation = this.generation
    const controller = new AbortController()
    this.controller = controller
    this.setChannelState('connecting')
    this.connectTimer = setTimeout(() => {
      this.failConnection(generation, controller)
    }, this.connectTimeoutMs)
    void this.runConnection(generation, controller)
  }

  private async runConnection(generation: number, controller: AbortController): Promise<void> {
    let response: Response
    try {
      response = await this.fetchImpl(this.url, {
        method: this.method,
        headers: this.requestHeaders(),
        body:
          this.method === 'POST' && typeof this.body !== 'undefined'
            ? serializeBody(this.body)
            : undefined,
        signal: controller.signal
      })
    } catch {
      this.failConnection(generation, controller)
      return
    }

    if (!this.isCurrent(generation, controller)) {
      await response.body?.cancel()
      return
    }
    this.clearConnectTimer()
    if (!response.ok || !response.body) {
      this.failConnection(generation, controller)
      return
    }

    const reader = response.body.getReader()
    const decoder = new TextDecoder()
    this.armHeartbeatTimer(generation, controller)
    try {
      for (;;) {
        const result = await reader.read()
        if (!this.isCurrent(generation, controller)) return
        if (result.done) {
          const tail = decoder.decode()
          if (tail.length > 0) this.handleChunk(tail, generation, controller)
          this.failConnection(generation, controller)
          return
        }
        const value = result.value
        if (!value || value.byteLength === 0) continue
        const chunk = decoder.decode(value, { stream: true })
        if (chunk.length > 0) this.handleChunk(chunk, generation, controller)
      }
    } catch {
      this.failConnection(generation, controller)
    } finally {
      reader.releaseLock()
    }
  }

  private requestHeaders(): Headers {
    const headers = new Headers({ accept: 'text/event-stream' })
    if (this.method === 'POST') headers.set('content-type', 'application/json')
    const token = this.accessToken()
    if (typeof token === 'string') headers.set('Authorization', `Bearer ${token}`)
    if (typeof this.cursor === 'string') headers.set('Last-Event-ID', this.cursor)
    return headers
  }

  private handleChunk(chunk: string, generation: number, controller: AbortController): void {
    if (!this.isCurrent(generation, controller)) return
    // Bytes, even an incomplete frame, prove the transport has not gone silent.
    this.armHeartbeatTimer(generation, controller)
    for (const frame of this.parser.push(chunk)) {
      if (!this.isCurrent(generation, controller)) return
      if (frame.comment) {
        this.proveHealthy()
        continue
      }
      if (typeof frame.id === 'string') this.cursor = frame.id
      if (frame.event === 'reorg') this.cursor = undefined
      this.proveHealthy()
      callSafely(() => this.onFrame(frame))
    }
  }

  private failConnection(generation: number, controller: AbortController): void {
    if (!this.isCurrent(generation, controller)) return
    this.abortCurrentRequest()
    this.setChannelState('dead')
    if (!this.retry || this.closed) return
    const delay = computeBackoffDelayMs(this.attempt, this.backoffInitialMs, this.backoffMaxMs)
    this.attempt += 1
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = undefined
      this.connect()
    }, delay)
  }

  private proveHealthy(): void {
    this.attempt = 0
    this.setChannelState('healthy')
  }

  private setChannelState(state: SseChannelState): void {
    if (this.channelState === state) return
    this.channelState = state
    if (!this.onChannelState) return
    callSafely(() => this.onChannelState?.(state))
  }

  private armHeartbeatTimer(generation: number, controller: AbortController): void {
    this.clearHeartbeatTimer()
    this.heartbeatTimer = setTimeout(() => {
      this.failConnection(generation, controller)
    }, this.heartbeatTimeoutMs)
  }

  private isCurrent(generation: number, controller: AbortController): boolean {
    return !this.closed && this.generation === generation && this.controller === controller
  }

  private abortCurrentRequest(): void {
    this.clearConnectTimer()
    this.clearHeartbeatTimer()
    const controller = this.controller
    this.controller = undefined
    if (!controller) return
    try {
      controller.abort()
    } catch {
      // An already-aborted controller is harmless during idempotent cleanup.
    }
  }

  private clearConnectTimer(): void {
    if (!this.connectTimer) return
    clearTimeout(this.connectTimer)
    this.connectTimer = undefined
  }

  private clearHeartbeatTimer(): void {
    if (!this.heartbeatTimer) return
    clearTimeout(this.heartbeatTimer)
    this.heartbeatTimer = undefined
  }

  private clearReconnectTimer(): void {
    if (!this.reconnectTimer) return
    clearTimeout(this.reconnectTimer)
    this.reconnectTimer = undefined
  }
}

/** Opens a streaming-fetch connection immediately; the returned handle owns cleanup. */
export function openSseConnection(options: SseConnectionOptions): SseConnection {
  return new SseClientService(options)
}

interface DocSubscriber {
  onDoc: (event: DocChangeEvent) => void | Promise<void>
  onReorg: () => void
  onChannelState: ((state: SseChannelState) => void) | undefined
}

class DocSseHubEntry {
  private readonly subscribers = new Set<DocSubscriber>()
  private readonly inFlightStores = new Set<string>()
  private readonly pendingByStore = new Map<string, DocChangeEvent>()
  private connection: SseConnection | undefined
  private state: SseChannelState | undefined

  constructor(
    private readonly key: string,
    private readonly registry: Map<string, DocSseHubEntry>,
    private readonly companyKey: string,
    private readonly stores: readonly string[],
    private readonly deps: SseHubDeps
  ) {}

  add(subscriber: DocSubscriber): SseSubscription {
    this.subscribers.add(subscriber)
    if (!this.connection) {
      this.connection = openSseConnection({
        url: this.docUrl(),
        accessToken: this.deps.accessToken,
        fetchImpl: this.deps.fetchImpl,
        onFrame: (frame) => this.handleFrame(frame),
        onChannelState: (state) => this.handleChannelState(state)
      })
    } else if (this.state) {
      const state = this.state
      callSafely(() => subscriber.onChannelState?.(state))
    }
    let closed = false
    return {
      close: () => {
        if (closed) return
        closed = true
        this.remove(subscriber)
      }
    }
  }

  private docUrl(): string {
    const url = appUrl(
      this.deps.baseUrl,
      `/companies/${encodeURIComponent(this.companyKey)}/events`
    )
    url.searchParams.set('stores', this.stores.join(','))
    return url.toString()
  }

  private remove(subscriber: DocSubscriber): void {
    this.subscribers.delete(subscriber)
    if (this.subscribers.size > 0) return
    this.connection?.close()
    this.connection = undefined
    if (this.registry.get(this.key) === this) this.registry.delete(this.key)
  }

  private handleFrame(frame: SseFrame): void {
    if (frame.event === 'reorg') {
      for (const subscriber of [...this.subscribers]) callSafely(subscriber.onReorg)
      return
    }
    if (frame.event !== 'doc') return
    const parsed = DocChangeEventSchema.safeParse(parseFrameJson(frame))
    if (!parsed.success) return
    this.dispatchDoc(parsed.data)
  }

  private handleChannelState(state: SseChannelState): void {
    this.state = state
    for (const subscriber of [...this.subscribers]) {
      if (subscriber.onChannelState) callSafely(() => subscriber.onChannelState?.(state))
    }
  }

  private dispatchDoc(event: DocChangeEvent): void {
    if (this.inFlightStores.has(event.store)) {
      this.pendingByStore.set(event.store, event)
      return
    }
    this.inFlightStores.add(event.store)
    void this.runDocCycle(event.store, event)
  }

  private async runDocCycle(store: string, first: DocChangeEvent): Promise<void> {
    let current: DocChangeEvent | undefined = first
    while (current) {
      for (const subscriber of [...this.subscribers]) {
        try {
          await subscriber.onDoc(current)
        } catch {
          // Keep every other subscriber and later change alive.
        }
      }
      const next = this.pendingByStore.get(store)
      if (!next) {
        current = undefined
        continue
      }
      this.pendingByStore.delete(store)
      current = next
    }
    this.inFlightStores.delete(store)
  }
}

interface PersonSubscriber {
  onState: (state: PersonSessionState) => void
  onSession: (event: PersonSessionEventFrame) => void
  onHost: (event: PersonHostEvent) => void
  onReorg: () => void
  onChannelState: ((state: SseChannelState) => void) | undefined
}

class PersonSseHubEntry {
  private readonly subscribers = new Set<PersonSubscriber>()
  private connection: SseConnection | undefined
  private state: SseChannelState | undefined

  constructor(
    private readonly key: string,
    private readonly registry: Map<string, PersonSseHubEntry>,
    private readonly companyKey: string,
    private readonly personId: string,
    private readonly deps: SseHubDeps
  ) {}

  add(subscriber: PersonSubscriber): SseSubscription {
    this.subscribers.add(subscriber)
    if (!this.connection) {
      this.connection = openSseConnection({
        url: appUrl(this.deps.baseUrl, personEventsPath(this.companyKey, this.personId)).toString(),
        accessToken: this.deps.accessToken,
        fetchImpl: this.deps.fetchImpl,
        onFrame: (frame) => this.handleFrame(frame),
        onChannelState: (state) => this.handleChannelState(state)
      })
    } else if (this.state) {
      const state = this.state
      callSafely(() => subscriber.onChannelState?.(state))
    }
    let closed = false
    return {
      close: () => {
        if (closed) return
        closed = true
        this.remove(subscriber)
      }
    }
  }

  private remove(subscriber: PersonSubscriber): void {
    this.subscribers.delete(subscriber)
    if (this.subscribers.size > 0) return
    this.connection?.close()
    this.connection = undefined
    if (this.registry.get(this.key) === this) this.registry.delete(this.key)
  }

  private handleFrame(frame: SseFrame): void {
    const value = parseFrameJson(frame)
    switch (frame.event) {
      case undefined:
        return
      case 'state': {
        const parsed = PersonSessionStateSchema.safeParse(value)
        if (!parsed.success) return
        for (const subscriber of [...this.subscribers]) {
          callSafely(() => subscriber.onState(parsed.data))
        }
        return
      }
      case 'session': {
        const cursor = parseSessionCursor(frame.id)
        const event = parseSessionEvent(value)
        if (!cursor || !event || typeof frame.id === 'undefined') return
        const envelope: PersonSessionEventFrame = { id: frame.id, ...cursor, event }
        for (const subscriber of [...this.subscribers]) {
          callSafely(() => subscriber.onSession(envelope))
        }
        return
      }
      case 'host': {
        const parsed = PersonHostEventSchema.safeParse(value)
        if (!parsed.success) return
        for (const subscriber of [...this.subscribers]) {
          callSafely(() => subscriber.onHost(parsed.data))
        }
        return
      }
      case 'reorg':
        for (const subscriber of [...this.subscribers]) callSafely(subscriber.onReorg)
        return
      default:
        return
    }
  }

  private handleChannelState(state: SseChannelState): void {
    this.state = state
    for (const subscriber of [...this.subscribers]) {
      if (subscriber.onChannelState) callSafely(() => subscriber.onChannelState?.(state))
    }
  }
}

const docSseHubs = new Map<string, DocSseHubEntry>()
const personSseHubs = new Map<string, PersonSseHubEntry>()

function sortedUniqueStores(stores: readonly string[]): string[] {
  return [...new Set(stores)].sort()
}

/** Joins exactly one document stream for its `<companyKey, sorted stores>` key. */
export function subscribeDocEvents(options: SubscribeDocEventsOptions): SseSubscription {
  const stores = sortedUniqueStores(options.stores)
  const key = `doc:${options.companyKey}:${stores.join(',')}`
  let hub = docSseHubs.get(key)
  if (!hub) {
    hub = new DocSseHubEntry(key, docSseHubs, options.companyKey, stores, options.deps)
    docSseHubs.set(key, hub)
  }
  return hub.add({
    onDoc: options.onDoc,
    onReorg: options.onReorg,
    onChannelState: options.onChannelState
  })
}

/** Joins exactly one per-person stream for its `<companyKey, person>` key. */
export function subscribePersonStream(options: SubscribePersonStreamOptions): SseSubscription {
  const key = `person:${options.companyKey}:${options.personId}`
  let hub = personSseHubs.get(key)
  if (!hub) {
    hub = new PersonSseHubEntry(
      key,
      personSseHubs,
      options.companyKey,
      options.personId,
      options.deps
    )
    personSseHubs.set(key, hub)
  }
  return hub.add({
    onState: options.onState,
    onSession: options.onSession,
    onHost: options.onHost,
    onReorg: options.onReorg,
    onChannelState: options.onChannelState
  })
}

/** Test probe for refcounted hub connections (one entry = one live connection). */
export function activeSseConnectionCount(): number {
  return docSseHubs.size + personSseHubs.size
}

/**
 * Streams an idempotency-sensitive lifecycle POST once. Its `retry: false`
 * is intentional: replaying a create request could create a second company.
 * The E5 create and boot endpoints retain their distinct `created` and
 * `booted` terminal frame names so callers receive the actual wire outcome.
 */
export function streamLifecycle(options: StreamLifecycleOptions): Promise<LifecycleTerminal> {
  return new Promise((resolve, reject) => {
    let settled = false
    let connection: SseConnection | undefined

    function finishWithError(error: ChiefApiError): void {
      if (settled) return
      settled = true
      connection?.close()
      reject(error)
    }

    function finishWithTerminal(terminal: LifecycleTerminal): void {
      if (settled) return
      settled = true
      connection?.close()
      resolve(terminal)
    }

    connection = openSseConnection({
      url: appUrl(options.deps.baseUrl, options.path).toString(),
      method: 'POST',
      body: options.body,
      accessToken: options.deps.accessToken,
      fetchImpl: options.deps.fetchImpl,
      retry: false,
      onFrame: (frame) => {
        const value = parseFrameJson(frame)
        switch (frame.event) {
          case undefined:
            return
          case 'phase': {
            const parsed = LifecyclePhaseFrameSchema.safeParse(value)
            if (parsed.success) callSafely(() => options.onPhase(parsed.data))
            return
          }
          case 'created': {
            const parsed = LifecycleCreatedFrameSchema.safeParse(value)
            if (parsed.success) finishWithTerminal({ kind: 'created', slug: parsed.data.slug })
            return
          }
          case 'booted': {
            const parsed = LifecycleBootedFrameSchema.safeParse(value)
            if (parsed.success) finishWithTerminal({ kind: 'booted', slug: parsed.data.slug })
            return
          }
          case 'failed': {
            const parsed = LifecycleFailedFrameSchema.safeParse(value)
            if (!parsed.success) {
              finishWithError(
                new ChiefApiError({ kind: 'upstream', detail: 'malformed lifecycle failure' })
              )
              return
            }
            finishWithError(
              new ChiefApiError({
                kind: 'upstream',
                code: parsed.data.error.code,
                detail: parsed.data.error.detail
              })
            )
            return
          }
          default:
            return
        }
      },
      onChannelState: (state) => {
        if (state === 'dead') {
          finishWithError(new ChiefApiError({ kind: 'network', detail: 'lifecycle stream ended' }))
        }
      }
    })
  })
}
