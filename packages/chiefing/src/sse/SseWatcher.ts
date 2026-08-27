/**
 * SseWatcher: one long-lived SSE reader per docstore subscription. Ported
 * from `apps/cli/src/legacy/organization/org-sse-watcher.ts` (#775, E2-S6) —
 * the public shape is unchanged, and the legacy child-process (`curl -sN`)
 * reader seam is deleted outright (Mandate 1: no child processes; the
 * in-process streaming `fetch()` reader below is the only production path).
 *
 * STREAM SHAPE RECONCILIATION (#775): the legacy source's `openStream` seam
 * was `(url, sink) => SseStreamHandle` (a synchronous handle + callback
 * pair). `types/Watch.ts` (landed by an earlier story, whose names win) uses
 * `SseStreamOpener = (url, headers) => Promise<AsyncIterable<string>>`
 * instead — an async-iterable-of-chunks seam (`headers` was added by A4, so
 * the credential this watcher decided to present is visible AT the seam
 * rather than hidden inside it). This port is written directly
 * against that shape, not adapted from the callback version. Because
 * `AsyncGenerator.prototype.return()` does NOT preempt an in-flight `await`
 * (it only takes effect once the current awaited promise settles — the spec
 * behavior, not a bug), a forced teardown (heartbeat timeout, explicit
 * `close()`, `addStores()`-triggered reconnect) cannot rely on iterator
 * `.return()` alone to stop promptly: the consumption loop below races each
 * `iterator.next()` against a per-connection abort promise this class
 * resolves directly, so the WATCHER's own state transition (schedule
 * reconnect, mark dead, etc.) happens immediately regardless of whether the
 * underlying source is still mid-read. `.return()` is still called
 * best-effort alongside, so a cooperating source (including the real
 * fetch-based default below, which also holds its own `AbortController`)
 * gets a chance to release its socket/reader promptly too.
 *
 * Multiplexing (see `./SseHub.ts`): `subscribeSse()` opens exactly ONE
 * watcher per `url|slug` for the whole process and fans `doc-change` events
 * out to the subscribers that asked for that store — the wire already
 * accepts a multi-store CSV `stores=`, so multiple callers on one pane ride
 * one connection instead of many. Prefer it over `new SseWatcher(...)` at
 * every production call site.
 *
 * Wire contract (chiefd docstore router — verified against the live
 * `WatchEvent` struct, `apps/chiefd/crates/chiefd-api/src/docstore/
 * feed.rs:89`, no `rename_all`, snake_case verbatim):
 * `GET /v1/docs/watch?slug=<slug>&stores=<csv>[&after=<seq>]` streams
 * `text/event-stream`; one frame per matching mutation —
 * `id: <seq>` / `event: doc-change` /
 * `data: {seq, slug, store, updated_at, removed}` (snake_case,
 * the Rust struct verbatim; `seq` duplicates the `id:` line inside the JSON
 * body too) — `removed` is `true` only for a `drop_company`/
 * `drop_company_store` deletion, in which case
 * `updated_at` is `""` (no clock threaded through a delete). `store: "*"` is
 * a WILDCARD (whole-company `drop_company`, meaning every store for the slug
 * is gone) — this module fans that one frame out into one synthetic
 * doc-change per subscribed store, so `onEvent` never has to special-case
 * `"*"` itself. Plus a comment heartbeat every ~15s, and a control
 * `event: reorg` frame when a requested `after` is older than the server's
 * retained ring (the client must resync via a full poll rather than trust
 * the stream alone) — never fired on a fresh subscribe with no `after`.
 *
 * Coalescing: at most one in-flight `onEvent` cycle per store; further
 * doc-change events for that store arriving mid-cycle collapse into a single
 * follow-up cycle run immediately after (a "dirty flag" drain loop, not a
 * queue — a burst of N events mid-cycle costs one follow-up, not N).
 *
 * Authentication (A4): the stream presents the caller's bearer on every dial,
 * and **a dropped stream is RE-AUTHENTICATED, not merely reconnected.** A
 * long-lived stream never sees the 401 that drives `FetchTransport`'s
 * re-acquire-and-retry: the 401 is answered before the stream opens, and a
 * token that dies mid-stream produces no status code at all — the daemon just
 * drops the connection. So every reconnect INVALIDATES the cached bearer and
 * acquires a fresh one before dialling, rather than replaying the header that
 * was just refused. Distinguishing "the token died" from "the network blinked"
 * is not attempted and must not be: treating every drop identically is both
 * simpler and strictly safer, and it is safe at all because `after=<seq>`
 * (below) makes a re-authenticated reconnect lossless. A dial that is NOT a
 * drop — the first connect, and `addStores()`'s deliberate widening — keeps
 * the cached token, because nothing has suggested it is stale.
 *
 * Reconnect: exponential backoff from `backoffInitialMs` (default 1s),
 * doubling per consecutive failure, capped at `backoffMaxMs` (default 30s);
 * resets to the floor the moment the channel proves live again (a heartbeat
 * or a real event). The last-seen sequence id is replayed via `after=` on
 * every (re)connect; a `reorg` frame drops that cursor (the ring may have
 * rotated past it) and fires `onReorg` exactly once so the caller can run one
 * full poll to resync before trusting the stream again — the channel stays
 * HEALTHY through a reorg; it is the only signal covering a lost window, not
 * a failure.
 *
 * Liveness: silence past `heartbeatTimeoutMs` tears the connection down and
 * reconnects. The monitor is a SINGLE resettable `setTimeout` re-armed on
 * every chunk — not a repeating interval — so a healthy quiet channel costs
 * one pending timer and zero wakeups until real silence.
 * `onChannelStateChange("dead"|"healthy")` lets a caller's own fallback poll
 * floor tell "alive but quiet" apart from "actually disconnected."
 *
 * Mandate 3: this module delivers events; it never decides what a change
 * means. A `reorg` triggers the caller's own resync callback, never a
 * client-side reconciliation here.
 *
 * Frame decoding lives in `./SseFrames.ts` and is shared with
 * `resources/CompanyLifecycle.ts`. It used to be four private fields and two
 * methods here, which meant chiefd's second SSE stream (the company lifecycle
 * one) arrived with nothing to parse it and no way to reuse this without also
 * inheriting reconnect backoff, heartbeat deadlines and connect-id fencing —
 * none of which a per-operation stream has any use for. What is left here is
 * exactly the connection state machine; what a frame IS is decided next door.
 *
 * Imports ONLY relative siblings + node/web builtins (no `@/` alias, no
 * package specifier) and reads no env, touches no disk — E2-S8
 * (`chiefing/extension-runtime`) republishes these exact files into
 * pi-homes with no `node_modules`, and depends on that constraint holding.
 */

import { isNullish } from '../Nullish.js'
import type {
  SseBearerProvider,
  SseChannelState,
  SseDocChangeEvent,
  SseFrame,
  SseStreamOpener,
  SseWatcherOptions
} from '../types/Watch.js'
import { SseFrameDecoder } from './SseFrames.js'

const DEFAULT_HEARTBEAT_TIMEOUT_MS = 45_000
const DEFAULT_BACKOFF_INITIAL_MS = 1_000
const DEFAULT_BACKOFF_MAX_MS = 30_000
/** Parity with the legacy reader's `curl --connect-timeout 5`. */
const CONNECT_TIMEOUT_MS = 5_000

/**
 * The default reader: an in-process streaming `fetch()` exposed as an async
 * generator of decoded text chunks. No child process, no pipe, no extra PID
 * per watcher. Holds its own `AbortController` so a caller with access to
 * the returned generator can force it to stop (see the module doc on why
 * the consumption loop below does not rely on this alone).
 */
async function openFetchStream(
  url: string,
  headers: Record<string, string>
): Promise<AsyncIterable<string>> {
  const controller = new AbortController()
  const connectTimer = setTimeout(() => {
    try {
      controller.abort()
    } catch {
      /* already aborted */
    }
  }, CONNECT_TIMEOUT_MS)
  connectTimer.unref?.()
  try {
    const response = await fetch(url, {
      signal: controller.signal,
      headers
    })
    clearTimeout(connectTimer)
    if (!response.ok || !response.body) {
      return (async function* emptyStream(): AsyncGenerator<string, void, unknown> {
        /* no body — an immediately-exhausted iterable ends the connect attempt */
      })()
    }
    const reader = response.body.getReader()
    const decoder = new TextDecoder()
    return (async function* chunks(): AsyncGenerator<string, void, unknown> {
      try {
        for (;;) {
          const { done, value } = await reader.read()
          if (done) return
          if (value && value.length > 0) yield decoder.decode(value, { stream: true })
        }
      } catch {
        // An abort (our own forced teardown) or a transport failure both
        // mean "the stream ended"; the reconnect ladder decides what
        // happens next.
      } finally {
        try {
          controller.abort()
        } catch {
          /* already aborted */
        }
        reader.releaseLock()
      }
    })()
  } finally {
    clearTimeout(connectTimer)
  }
}

/**
 * Pure exponential-backoff-with-cap formula, extracted so its exact values
 * (doubling from the floor, pinned at the ceiling) are unit-testable without
 * depending on real elapsed wall-clock time.
 */
export function computeBackoffDelayMs(attempt: number, initialMs: number, maxMs: number): number {
  return Math.min(initialMs * 2 ** attempt, maxMs)
}

/** Narrows `unknown` to a plain object without an `as` cast (banned
 * repo-wide) — a user-defined type guard's own body only needs to return a
 * boolean; TypeScript trusts the predicate at the call site. */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && !isNullish(value)
}

/** Narrows an `unknown` parsed JSON value to `SseDocChangeEvent`'s shape.
 * Malformed/missing fields fail this check and the frame is dropped rather
 * than trusted. */
function isSseDocChangeEvent(value: unknown): value is SseDocChangeEvent {
  if (!isRecord(value)) return false
  return (
    typeof value.slug === 'string' &&
    typeof value.store === 'string' &&
    typeof value.updated_at === 'string' &&
    typeof value.removed === 'boolean' &&
    (isNullish(value.seq) || typeof value.seq === 'number')
  )
}

/** A resolvable "stop now" signal for one connection attempt's consumption
 * loop — see the module doc for why this exists alongside `.return()`. */
function makeAbortGate(): { promise: Promise<'aborted'>; resolve: () => void } {
  let resolve!: () => void
  const promise = new Promise<'aborted'>((res) => {
    resolve = () => res('aborted')
  })
  return { promise, resolve }
}

export class SseWatcher {
  private readonly url: string
  private readonly slug: string
  private stores: string[]
  private readonly onEventCb: (event: SseDocChangeEvent) => void | Promise<void>
  private readonly onReorgCb?: () => void
  private readonly onChannelStateChangeCb?: (state: SseChannelState) => void
  private readonly initialAfter?: number
  private readonly heartbeatTimeoutMs: number
  private readonly backoffInitialMs: number
  private readonly backoffMaxMs: number
  private readonly openStreamFn: SseStreamOpener
  private readonly bearer: SseBearerProvider | undefined

  private closed = false
  /** Bumped on every connect/close/addStores; a stale connection's loop
   * checks this to know it has been superseded and must not act. Names the
   * connection attempt only. */
  private activeConnectId = 0
  private abortGate: { promise: Promise<'aborted'>; resolve: () => void } | undefined
  private reconnectTimer: ReturnType<typeof setTimeout> | undefined
  private heartbeatTimer: ReturnType<typeof setTimeout> | undefined
  private attempt = 0
  private lastEventId: number | undefined
  private channelState: SseChannelState | undefined
  private connected = false

  /** Frame decoding, delegated to chiefing's one decoder (`./SseFrames.ts`)
   * rather than re-implemented here. Replaced wholesale on every (re)connect:
   * a partial frame from a connection that died is not a prefix of the next
   * connection's first frame, and carrying one across would corrupt it. */
  private decoder = new SseFrameDecoder()

  // Coalescing state, per store name.
  private readonly inFlightStores = new Set<string>()
  private readonly pendingByStore = new Map<string, SseDocChangeEvent>()

  constructor(options: SseWatcherOptions) {
    this.url = options.url.replace(/\/+$/, '')
    this.slug = options.slug
    this.stores = [...options.stores]
    this.onEventCb = options.onEvent
    this.onReorgCb = options.onReorg
    this.onChannelStateChangeCb = options.onChannelStateChange
    this.initialAfter = options.after
    this.heartbeatTimeoutMs = options.heartbeatTimeoutMs ?? DEFAULT_HEARTBEAT_TIMEOUT_MS
    this.backoffInitialMs = options.backoffInitialMs ?? DEFAULT_BACKOFF_INITIAL_MS
    this.backoffMaxMs = options.backoffMaxMs ?? DEFAULT_BACKOFF_MAX_MS
    this.openStreamFn = options.openStream ?? openFetchStream
    this.bearer = options.bearer
    // The FIRST dial is not a drop: nothing has suggested the cached token is
    // stale, so it is presented as-is. Re-authentication is a property of the
    // RECONNECT (see `scheduleReconnect`), never of every dial — invalidating
    // here would throw away a token the pane's org tools are still using.
    this.connect(false)
  }

  /** Tears the reader down and cancels every timer. Idempotent. */
  close(): void {
    if (this.closed) return
    this.closed = true
    this.teardownConnection()
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = undefined
    }
  }

  /** The stores currently subscribed (the hub widens this set as callers join). */
  subscribedStores(): string[] {
    return [...this.stores]
  }

  /** The last sequence id seen on the wire, for cursor-preserving re-subscribes. */
  lastSeq(): number | undefined {
    return this.lastEventId
  }

  /** The channel state as last reported, if the channel has ever reported one. */
  currentChannelState(): SseChannelState | undefined {
    return this.channelState
  }

  /**
   * Widens the subscription to include `stores`, reconnecting (resuming from
   * the last seen `seq`, so nothing is missed) only when something is
   * genuinely new. Returns `true` if a reconnect was needed.
   */
  addStores(stores: string[]): boolean {
    if (this.closed) return false
    const added = stores.filter((store) => !this.stores.includes(store))
    if (added.length === 0) return false
    this.stores = [...this.stores, ...added]
    this.teardownConnection()
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = undefined
    }
    // A widening is a DELIBERATE re-dial, not a drop. The token that was
    // working one line ago is still working, so it is kept.
    this.connect(false)
    return true
  }

  /** Stops the current connection attempt's consumption loop immediately
   * (via the abort gate) and clears the heartbeat timer. Bumps the
   * active connect id so the abandoned loop's eventual settling is a no-op. */
  private teardownConnection(): void {
    this.activeConnectId += 1
    this.abortGate?.resolve()
    this.abortGate = undefined
    this.connected = false
    this.clearHeartbeatTimer()
  }

  private buildUrl(): string {
    const params = new URLSearchParams()
    params.set('slug', this.slug)
    params.set('stores', this.stores.join(','))
    const after = this.lastEventId ?? this.initialAfter
    if (!isNullish(after)) params.set('after', String(after))
    return `${this.url}/v1/docs/watch?${params.toString()}`
  }

  /**
   * @param reauthenticate `true` when this dial follows a DROP. The cached
   * bearer is dropped first so the acquisition below mints a fresh one — see
   * the module doc: a dropped stream is re-authenticated, never replayed.
   */
  private connect(reauthenticate: boolean): void {
    if (this.closed) return
    // Before the connect id is bumped and before any header is read, so no
    // in-flight acquisition for this dial can observe the stale token.
    if (reauthenticate) this.bearer?.invalidate()
    this.decoder = new SseFrameDecoder()
    this.activeConnectId += 1
    const myConnectId = this.activeConnectId
    const gate = makeAbortGate()
    this.abortGate = gate
    void this.runConnection(myConnectId, gate.promise)
  }

  /** The headers for ONE dial: the wire's `accept`, plus whatever credential
   * the provider hands back. Acquisition failure is not an error here — the
   * provider's contract is to answer `undefined` and let the daemon refuse. */
  private async openWithCredential(url: string): Promise<AsyncIterable<string>> {
    const headers: Record<string, string> = { accept: 'text/event-stream' }
    const auth = await this.bearer?.authHeader()
    if (auth) Object.assign(headers, auth)
    return this.openStreamFn(url, headers)
  }

  private async runConnection(myConnectId: number, abort: Promise<'aborted'>): Promise<void> {
    let iterable: AsyncIterable<string>
    try {
      const opened = await Promise.race([
        this.openWithCredential(this.buildUrl()).then((v) => ({ kind: 'ok' as const, v })),
        abort.then(() => ({ kind: 'aborted' as const }))
      ])
      if (this.activeConnectId !== myConnectId) return
      if (opened.kind === 'aborted') return
      iterable = opened.v
    } catch {
      if (this.activeConnectId !== myConnectId) return
      this.scheduleReconnect()
      return
    }
    // Connection opened (or at least the promise resolved without throwing);
    // arm the heartbeat monitor now — a stream that never sends a byte is
    // exactly the silence the heartbeat ceiling exists to catch.
    this.connected = true
    this.armHeartbeatMonitor(myConnectId)
    const iterator = iterable[Symbol.asyncIterator]()
    for (;;) {
      const outcome = await Promise.race([
        iterator.next().then((r) => ({ kind: 'next' as const, r })),
        abort.then(() => ({ kind: 'aborted' as const }))
      ])
      if (this.activeConnectId !== myConnectId) {
        void iterator.return?.()
        return
      }
      if (outcome.kind === 'aborted') {
        void iterator.return?.()
        return
      }
      if (outcome.r.done) break
      this.handleChunk(outcome.r.value, myConnectId)
      if (this.activeConnectId !== myConnectId) {
        void iterator.return?.()
        return
      }
    }
    // The stream ended on its own (server closed it, transport error, etc.)
    // — this connect id is still current, so it is a real disconnect.
    if (this.activeConnectId !== myConnectId) return
    this.handleDisconnect()
  }

  private handleDisconnect(): void {
    this.connected = false
    this.clearHeartbeatTimer()
    this.markDead()
    this.scheduleReconnect()
  }

  private scheduleReconnect(): void {
    if (this.closed) return
    const delay = computeBackoffDelayMs(this.attempt, this.backoffInitialMs, this.backoffMaxMs)
    this.attempt += 1
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = undefined
      // EVERY path into this method is a drop: the stream ended, the open
      // threw, or the heartbeat ceiling expired. All three re-authenticate,
      // and the `after=<lastSeq>` cursor `buildUrl()` replays is what makes
      // that free — a re-authenticated reconnect resumes exactly where the
      // dead one stopped.
      this.connect(true)
    }, delay)
    this.reconnectTimer.unref?.()
  }

  private clearHeartbeatTimer(): void {
    if (this.heartbeatTimer) {
      clearTimeout(this.heartbeatTimer)
      this.heartbeatTimer = undefined
    }
  }

  /**
   * A SINGLE one-shot timer, re-armed on every chunk — never a repeating
   * interval. A healthy channel that is merely quiet costs one pending timer
   * and zero wakeups; the timer fires only after real silence past the
   * ceiling.
   */
  private armHeartbeatMonitor(myConnectId: number): void {
    this.clearHeartbeatTimer()
    this.heartbeatTimer = setTimeout(() => {
      this.heartbeatTimer = undefined
      if (this.closed || this.activeConnectId !== myConnectId || !this.connected) return
      // Silence past the ceiling: the connection may still look alive to the
      // OS (a stalled TCP socket) but the channel is dead to us. Tear it
      // down (aborting the in-flight read via the gate) and reconnect.
      this.teardownConnection()
      this.markDead()
      this.scheduleReconnect()
    }, this.heartbeatTimeoutMs)
    this.heartbeatTimer.unref?.()
  }

  private setChannelState(state: SseChannelState): void {
    if (this.channelState === state) return
    this.channelState = state
    this.onChannelStateChangeCb?.(state)
  }

  private markHealthy(): void {
    this.attempt = 0
    this.setChannelState('healthy')
  }

  private markDead(): void {
    this.setChannelState('dead')
  }

  private handleChunk(text: string, myConnectId: number): void {
    this.armHeartbeatMonitor(myConnectId)
    for (const frame of this.decoder.push(text)) this.dispatchEvent(frame)
  }

  private dispatchEvent(frame: SseFrame): void {
    if (frame.event === 'comment') {
      // Comment heartbeat: liveness deadline already reset by handleChunk.
      this.markHealthy()
      return
    }
    const eventType = frame.event
    const dataRaw = frame.data
    const id = frame.id
    if (dataRaw === '' && eventType === 'message') return // pure blank dispatch, nothing to do

    if (!isNullish(id)) {
      // Base 10 explicitly: an SSE `id:` is a decimal counter, and a radix-less
      // parse would read a `0x`-prefixed one as hex — eslint 10 turns that class
      // of silent misread into an error (`radix`).
      const parsedId = Number.parseInt(id, 10)
      if (Number.isFinite(parsedId)) this.lastEventId = parsedId
    }
    this.markHealthy()

    if (eventType === 'reorg') {
      // The ring rotated past our cursor: drop it (go live from here forward)
      // and tell the caller to run one full poll to resync. The channel
      // stays HEALTHY — a reorg is not a failure, it is the signal covering
      // a lost window.
      this.lastEventId = undefined
      this.onReorgCb?.()
      return
    }
    if (eventType !== 'doc-change') return // ignore unknown event types, forward-compatibly

    let candidate: unknown
    try {
      candidate = JSON.parse(dataRaw)
    } catch {
      return // malformed frame — drop it rather than crash the reader
    }
    if (!isSseDocChangeEvent(candidate)) return
    let parsed: SseDocChangeEvent = candidate
    // The JSON body carries its own `seq` (the wire duplicates it there
    // alongside the SSE `id:` line); fall back to the `id:` line only if the
    // body is somehow missing it.
    if (!Number.isFinite(parsed.seq) && !isNullish(this.lastEventId)) {
      parsed = { ...parsed, seq: this.lastEventId }
    }
    this.dispatchDocChange(parsed)
  }

  /** Fans a whole-company `store: "*"` wildcard out to every subscribed store. */
  private dispatchDocChange(event: SseDocChangeEvent): void {
    if (event.store === '*') {
      for (const store of this.stores) this.handleDocChange({ ...event, store })
      return
    }
    this.handleDocChange(event)
  }

  private handleDocChange(event: SseDocChangeEvent): void {
    const key = event.store
    if (this.inFlightStores.has(key)) {
      // A cycle for this store is already running: coalesce into the single
      // follow-up (last-value-wins; the follow-up needs the latest state, not
      // every intermediate one).
      this.pendingByStore.set(key, event)
      return
    }
    this.inFlightStores.add(key)
    void this.runCycle(key, event)
  }

  private async runCycle(key: string, first: SseDocChangeEvent): Promise<void> {
    let current: SseDocChangeEvent | undefined = first
    while (current) {
      try {
        await this.onEventCb(current)
      } catch {
        // A handler's own bug must never kill the reader — the next event
        // (or the fallback poll floor) still gets a chance to catch up.
      }
      const next = this.pendingByStore.get(key)
      if (next) {
        this.pendingByStore.delete(key)
        current = next
        continue
      }
      current = undefined
    }
    this.inFlightStores.delete(key)
  }
}
