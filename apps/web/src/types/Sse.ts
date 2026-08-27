/**
 * Browser-side SSE wire vocabulary for the E5 app API bridge. The browser
 * deliberately models Pi payloads structurally: apps/web does not import Pi
 * packages, and apps/api forwards their event bodies verbatim.
 */
import { z } from 'zod'

import type { FetchImpl } from '@/types/Fetch'
import type { AgentSessionEvent } from '@/types/SessionEvents'

/** One completed Server-Sent Event frame from the incremental parser. */
export interface SseFrame {
  id?: string
  event?: string
  data?: string
  comment?: boolean
}

/** A plain union, not a zod enum: nothing parses a channel state off the
 * wire — this app's own SSE client is the only producer, so the schema that
 * used to stand here was never `.parse`d and existed only to be read back out
 * by `z.infer`. */
export type SseChannelState = 'connecting' | 'healthy' | 'dead'

/** E5's app-API doc bridge is camelCase, unlike chiefd's internal watcher. */
export const DocChangeEventSchema = z
  .object({
    /** The handle the page subscribed with, echoed back: `sha256(<dir>)[..12]`.
     * Both ends of this frame are this app's own (`server/CompanyFeed`
     * translates chiefd's, which carries no company handle at all), so it names
     * the key it carries rather than borrowing chiefd's `slug` spelling. */
    companyKey: z.string(),
    store: z.string(),
    seq: z.number(),
    generation: z.number(),
    updatedAt: z.string(),
    removed: z.boolean()
  })
  .passthrough()
export type DocChangeEvent = z.infer<typeof DocChangeEventSchema>

/** Opaque structural state forwarded from a running Pi session. */
export const PersonSessionStateSchema = z.object({}).passthrough()
export type PersonSessionState = z.infer<typeof PersonSessionStateSchema>

export const PersonHostEventSchema = z
  .object({
    state: z.enum(['starting', 'running', 'exited', 'stopped']),
    pid: z.number().nullish(),
    exitCode: z.number().nullish()
  })
  .passthrough()
export type PersonHostEvent = z.infer<typeof PersonHostEventSchema>
export type PersonHostState = 'starting' | 'running' | 'exited' | 'stopped'

/** A session event plus its server-owned replay cursor. */
export interface PersonSessionEventFrame {
  id: string
  generation: number
  seq: number
  event: AgentSessionEvent
}

/** The bounded event value S6 renders from `usePersonStream`. */
export type SessionEventEnvelope = PersonSessionEventFrame

export interface PersonStreamSnapshot {
  channel: SseChannelState
  session: PersonSessionState | undefined
  events: readonly SessionEventEnvelope[]
  /** The full host frame preserves terminal exit details for the pane banner. */
  host: PersonHostEvent | undefined
  hostState: PersonHostState | undefined
  reorgCount: number
}

/* `slug` on the three lifecycle frames below is a REAL slug, and is the one
 * company handle in this file that is not a key. `chief host`'s company
 * lifecycle wire (`/v1/company/{create,boot,stop}`) is still slug-keyed, and
 * `server/CompanyLifecycle` re-emits chiefd's own answer verbatim rather than
 * inventing a second vocabulary for it. Nothing may address a company by this
 * value — see `displaySlugFor`, which exists because the translation only runs
 * one way. */

export const LifecyclePhaseFrameSchema = z
  .object({
    phase: z.string(),
    slug: z.string().nullish(),
    detail: z.string().nullish()
  })
  .passthrough()
export type LifecyclePhaseFrame = z.infer<typeof LifecyclePhaseFrameSchema>

export const LifecycleCreatedFrameSchema = z.object({ slug: z.string() }).passthrough()

/** The existing E5 boot route names its successful terminal `booted`. */
export const LifecycleBootedFrameSchema = z.object({ slug: z.string() }).passthrough()

export const LifecycleFailedFrameSchema = z
  .object({ error: z.object({ code: z.string(), detail: z.string() }).passthrough() })
  .passthrough()

export type LifecycleTerminal =
  | { kind: 'created'; slug: string }
  | { kind: 'booted'; slug: string }
  | { kind: 'failed'; error: { code: string; detail: string } }

/** Injected app-API-only connection dependencies; no direct daemon address exists in web. */
export interface SseHubDeps {
  baseUrl: string
  accessToken: () => string | null
  fetchImpl?: FetchImpl
}

export interface SseConnectionOptions {
  url: string
  method?: 'GET' | 'POST'
  body?: unknown
  accessToken: () => string | null
  lastEventId?: string
  onFrame: (frame: SseFrame) => void
  onChannelState?: (state: SseChannelState) => void
  connectTimeoutMs?: number
  heartbeatTimeoutMs?: number
  backoffInitialMs?: number
  backoffMaxMs?: number
  retry?: boolean
  fetchImpl?: FetchImpl
}

export interface SseConnection {
  close(): void
  lastEventId(): string | undefined
}

export interface SseSubscription {
  close(): void
}

export interface SubscribeDocEventsOptions {
  companyKey: string
  stores: readonly string[]
  onDoc: (event: DocChangeEvent) => void | Promise<void>
  onReorg: () => void
  onChannelState?: (state: SseChannelState) => void
  deps: SseHubDeps
}

export interface SubscribePersonStreamOptions {
  companyKey: string
  personId: string
  onState: (state: PersonSessionState) => void
  onSession: (event: PersonSessionEventFrame) => void
  onHost: (event: PersonHostEvent) => void
  onReorg: () => void
  onChannelState?: (state: SseChannelState) => void
  deps: SseHubDeps
}

export interface StreamLifecycleOptions {
  path: '/companies' | `/companies/${string}/boot`
  body?: unknown
  onPhase: (frame: LifecyclePhaseFrame) => void
  deps: SseHubDeps
}
