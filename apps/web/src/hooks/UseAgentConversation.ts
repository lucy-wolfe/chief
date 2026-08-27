'use client'

/** Transcript hydration plus the one live session-event fold used by AgentPane. */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { z } from 'zod'

import { usePersonStream } from '@/hooks/UsePersonStream'
import { useChiefApi } from '@/providers/ApiSessionProvider'
import { ChiefApiError, type ChiefApiErrorShape } from '@/types/ApiErrors'
import type { SayMode } from '@/types/ChiefApi'
import {
  type AgentConversationResult,
  type AgentConversationRuntime,
  type ConversationRow,
  foldSessionEvent,
  rowsFromTranscript
} from '@/types/Conversation'
import type { PersonSessionState, PersonStreamSnapshot, SessionEventEnvelope } from '@/types/Sse'

function paneErrorFrom(error: unknown): ChiefApiErrorShape {
  if (error instanceof ChiefApiError) {
    return {
      kind: error.kind,
      status: error.status,
      code: error.code,
      detail: error.detail
    }
  }
  return {
    kind: 'network',
    detail: error instanceof Error ? error.message : 'Unknown chief-api failure'
  }
}

function booleanState(state: PersonSessionState | undefined, field: string): boolean {
  const value = state?.[field]
  return value === true
}

function numberState(state: PersonSessionState | undefined, field: string): number | undefined {
  const value = state?.[field]
  return typeof value === 'number' ? value : undefined
}

function queueLength(event: unknown, field: string): number {
  const parsed = z.object({}).passthrough().safeParse(event)
  if (!parsed.success) return 0
  const candidate = parsed.data[field]
  return Array.isArray(candidate) ? candidate.length : 0
}

function runtimeFrom(
  session: PersonSessionState | undefined,
  events: readonly SessionEventEnvelope[]
): AgentConversationRuntime {
  let isCompacting = booleanState(session, 'isCompacting')
  let isRetrying = false
  let isSettled = !booleanState(session, 'isStreaming')
  let queuedMessages = numberState(session, 'pendingMessageCount') ?? 0

  for (const envelope of events) {
    const event = envelope.event
    switch (event.type) {
      case 'agent_start':
      case 'turn_start':
      case 'message_start':
        isSettled = false
        break
      case 'agent_settled':
        isSettled = true
        break
      case 'compaction_start':
        isCompacting = true
        break
      case 'compaction_end':
        isCompacting = false
        break
      case 'auto_retry_start':
        isRetrying = true
        break
      case 'auto_retry_end':
        isRetrying = false
        break
      case 'queue_update':
        queuedMessages = queueLength(event, 'steering') + queueLength(event, 'followUp')
        break
      default:
        break
    }
  }

  return { isCompacting, isRetrying, isSettled, queuedMessages }
}

/**
 * Is this person's agent child live right now, per the person event stream's
 * own open protocol?
 *
 * `GET /companies/:companyKey/people/:personId/events` greets every connection with
 * EXACTLY ONE of two frames (apps/api's `StreamService.agentEventStream`): a
 * `state` frame carrying the live `RpcSessionState` when `agentHost.state()`
 * has a child, or a `host` frame with `state: "stopped"` when it does not.
 * So a host frame is authoritative the moment it exists, and before one
 * arrives the presence of a session snapshot is itself the answer.
 *
 * This matters because apps/api gates `…/transcript` behind
 * `requireHostedClient`, which answers 409 `person-not-running` for a dormant
 * agent. Hydrating unconditionally therefore guaranteed a failed request and
 * a raw `person-not-running` banner on every pane of every company whose
 * agents were not up — the ordinary state of a company you have just opened.
 */
function agentIsLive(stream: PersonStreamSnapshot): boolean {
  if (typeof stream.hostState !== 'undefined') return stream.hostState === 'running'
  return typeof stream.session !== 'undefined'
}

/**
 * Hydrates exactly once per live agent for the selected person, and once for
 * each S4 reorg. Events that predate a hydration are represented by the
 * transcript; only events arriving after that snapshot are folded into its
 * rebuilt row list.
 */
export function useAgentConversation(
  companyKey: string,
  personId: string
): AgentConversationResult {
  const api = useChiefApi()
  const stream = usePersonStream(companyKey, personId)
  const [rows, setRows] = useState<readonly ConversationRow[]>([])
  const [hydrating, setHydrating] = useState(true)
  const [paneError, setPaneError] = useState<ChiefApiErrorShape | undefined>(undefined)
  const hydratingRef = useRef(true)
  const processedEventsRef = useRef(new Set<string>())
  const eventsRef = useRef(stream.events)
  eventsRef.current = stream.events

  const live = agentIsLive(stream)

  useEffect(() => {
    const controller = new AbortController()
    hydratingRef.current = true
    processedEventsRef.current = new Set(eventsRef.current.map((event) => event.id))
    setRows([])
    setHydrating(true)
    setPaneError(undefined)

    // A dormant agent has no transcript to serve — the route refuses with 409
    // `person-not-running`. `live` is a dependency, so the hydration runs by
    // itself the moment the stream reports the child came up (mandate 1: the
    // stream pushes, nothing here polls or retries).
    if (!live) {
      hydratingRef.current = false
      setHydrating(false)
      return () => controller.abort()
    }

    void api
      .getTranscript(companyKey, personId, undefined, controller.signal)
      .then((transcript) => {
        if (controller.signal.aborted) return
        setRows(rowsFromTranscript(transcript.entries))
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return
        setPaneError(paneErrorFrom(error))
      })
      .finally(() => {
        if (controller.signal.aborted) return
        hydratingRef.current = false
        setHydrating(false)
      })

    return () => controller.abort()
  }, [api, live, personId, companyKey, stream.reorgCount])

  useEffect(() => {
    if (hydratingRef.current) return
    const incoming = stream.events.filter((event) => !processedEventsRef.current.has(event.id))
    if (incoming.length === 0) return
    for (const event of incoming) processedEventsRef.current.add(event.id)
    setRows((current) => incoming.reduce(foldSessionEvent, current))
  }, [hydrating, stream.events])

  const invoke = useCallback(async (request: () => Promise<unknown>): Promise<void> => {
    try {
      await request()
      setPaneError(undefined)
    } catch (error) {
      setPaneError(paneErrorFrom(error))
    }
  }, [])

  const send = useCallback(
    async (message: string, mode: SayMode): Promise<void> =>
      invoke(() => api.say(companyKey, personId, { text: message, mode })),
    [api, invoke, personId, companyKey]
  )
  const abort = useCallback(
    async (): Promise<void> => invoke(() => api.abort(companyKey, personId)),
    [api, invoke, personId, companyKey]
  )
  // `newSession`, `compact` and `startPerson` used to live here. All three
  // dialled routes this app has never served, so all three were buttons that
  // produced a 404 an operator could do nothing about.
  //
  // They are gone rather than disabled, because each is a decision that does
  // not belong to this process: starting a person is chiefd's roster
  // convergence and a host that started somebody chiefd had not asked for
  // would be a second roster; a fresh session is a durable maintenance
  // protocol in chiefd — queue, claim, interrupt, complete — that a single
  // call cannot honestly approximate.

  const runtime = useMemo(
    () => runtimeFrom(stream.session, stream.events),
    [stream.events, stream.session]
  )

  return {
    rows,
    session: stream.session,
    host: stream.host,
    hostState: stream.hostState,
    channel: stream.channel,
    hydrating,
    runtime,
    paneError,
    send,
    abort
  }
}
