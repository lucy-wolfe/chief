// Wire and client types for chiefd's SSE channels.

/** One decoded SSE frame, as `sse/SseFrames.ts` produces it.
 *
 * Shared by BOTH of chiefd's streams — `/v1/docs/watch`'s long-lived document
 * feed and `chief host`'s per-operation lifecycle stream — because framing is
 * the same problem for both and is solved once.
 *
 * `event` is never absent: a frame that carried no `event:` field gets the
 * spec's own `'message'` default applied at decode time, so no consumer has to
 * repeat it. A comment heartbeat (`:hb`) arrives as `event: 'comment'` with the
 * comment text as `data` — reported rather than dropped, because a caller's
 * liveness logic needs to see it. */
export interface SseFrame {
  /** The `event:` field, or `'message'`. */
  readonly event: string
  /** Every `data:` line joined with newlines; `''` when the frame had none. */
  readonly data: string
  /** The `id:` field, when present. */
  readonly id?: string
}

/** Verbatim serde shape of chiefd's WatchEvent (feed.rs). snake_case is the
 * wire contract — do NOT camelCase this. */
export interface SseDocChangeEvent {
  seq: number
  slug: string
  store: string
  updated_at: string
  removed: boolean
}

export type SseChannelState = 'healthy' | 'dead'

/**
 * The credential a long-lived SSE reader presents, and the only thing it is
 * allowed to know about authentication.
 *
 * Deliberately the structural shape `AgentTokenManager` already has, so the
 * pane hands its EXISTING acquirer straight to the watcher — one token cache,
 * one challenge/sign round trip, one definition of "my bearer" per (daemon,
 * person, key). A watcher that minted its own would be a second acquirer with
 * its own cache, and the two would disagree the first time either re-acquired.
 *
 * `authHeader()` must never throw into the connect path: an acquisition that
 * fails returns `undefined` and the stream dials token-less, to be refused by
 * the daemon. The daemon, not the reader, is the authority on that.
 */
export interface SseBearerProvider {
  authHeader(): Promise<Record<string, string> | undefined>
  invalidate(): void
}

/** TEST-ONLY seam: replaces the real streaming-fetch opener in unit tests.
 *
 * `headers` carries the credential (plus `accept`) the watcher resolved for
 * THIS connection attempt. It is an argument rather than something the opener
 * re-derives, because the re-authenticating reconnect below is only observable
 * — and therefore only testable — if the header the watcher decided to present
 * is visible at this seam. */
export type SseStreamOpener = (
  url: string,
  headers: Record<string, string>
) => Promise<AsyncIterable<string>>

export interface SseWatcherOptions {
  url: string
  slug: string
  stores: string[]
  onEvent: (event: SseDocChangeEvent) => void | Promise<void>
  onReorg?: () => void
  onChannelStateChange?: (state: SseChannelState) => void
  after?: number
  /** Default 45_000 (3 missed 15s heartbeats). */
  heartbeatTimeoutMs?: number
  /** Default 1_000. */
  backoffInitialMs?: number
  /** Default 30_000. */
  backoffMaxMs?: number
  /** The caller's own credential. Omitted by a caller that has none — the
   * stream then dials token-less exactly as it always did. */
  bearer?: SseBearerProvider
  /** TEST-ONLY seam. */
  openStream?: SseStreamOpener
}

export interface SseSubscription {
  close(): void
}

// STUB — E2-S6 confirms or corrects. E2's Contract never defines this type;
// watch() is "bound to this client's url", so every SseWatcherOptions field
// except `url` is caller-supplied.
export type WatchSubscribeOptions = Omit<SseWatcherOptions, 'url'>
