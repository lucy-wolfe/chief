'use client'

/** Reactive per-person session stream with bounded, cursor-deduped memory. */
import { useEffect, useRef, useSyncExternalStore } from 'react'

import { useCompanyEventsDeps } from '@/providers/CompanyEventsProvider'
import { subscribePersonStream } from '@/services/SseClientService'
import type {
  PersonHostEvent,
  PersonSessionEventFrame,
  PersonSessionState,
  PersonStreamSnapshot,
  SessionEventEnvelope,
  SseChannelState
} from '@/types/Sse'

const MAX_SESSION_EVENTS = 512

function initialSnapshot(): PersonStreamSnapshot {
  return {
    channel: 'connecting',
    session: undefined,
    events: [],
    host: undefined,
    hostState: undefined,
    reorgCount: 0
  }
}

function compareSessionEvents(left: SessionEventEnvelope, right: SessionEventEnvelope): number {
  if (left.generation !== right.generation) return left.generation - right.generation
  return left.seq - right.seq
}

class PersonStreamStore {
  private readonly listeners = new Set<() => void>()
  private snapshot = initialSnapshot()

  constructor(readonly key: string) {}

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  readonly getSnapshot = (): PersonStreamSnapshot => this.snapshot

  setChannel(channel: SseChannelState): void {
    if (this.snapshot.channel === channel) return
    this.publish({ ...this.snapshot, channel })
  }

  setState(session: PersonSessionState): void {
    this.publish({ ...this.snapshot, session })
  }

  setHost(event: PersonHostEvent): void {
    this.publish({ ...this.snapshot, host: event, hostState: event.state })
  }

  addSession(event: PersonSessionEventFrame): void {
    if (this.snapshot.events.some((existing) => existing.id === event.id)) return
    const sorted = [...this.snapshot.events, event].sort(compareSessionEvents)
    const events = sorted.length > MAX_SESSION_EVENTS ? sorted.slice(-MAX_SESSION_EVENTS) : sorted
    this.publish({ ...this.snapshot, events })
  }

  noteReorg(): void {
    this.publish({ ...this.snapshot, reorgCount: this.snapshot.reorgCount + 1 })
  }

  private publish(snapshot: PersonStreamSnapshot): void {
    this.snapshot = snapshot
    for (const listener of this.listeners) listener()
  }
}

/**
 * Supplies S6 with session state, ordered replay-safe events, host state,
 * and a reorg counter it can use to rehydrate the transcript once.
 */
export function usePersonStream(companyKey: string, personId: string): PersonStreamSnapshot {
  const deps = useCompanyEventsDeps()
  const key = `${companyKey}:${personId}`
  const storeRef = useRef<PersonStreamStore | undefined>(undefined)
  if (typeof storeRef.current === 'undefined' || storeRef.current.key !== key) {
    storeRef.current = new PersonStreamStore(key)
  }
  const streamStore = storeRef.current
  if (typeof streamStore === 'undefined') throw new Error('person stream store was not initialized')

  useEffect(() => {
    const subscription = subscribePersonStream({
      companyKey,
      personId,
      onState: (state) => streamStore.setState(state),
      onSession: (event) => streamStore.addSession(event),
      onHost: (event) => streamStore.setHost(event),
      onReorg: () => streamStore.noteReorg(),
      onChannelState: (state) => streamStore.setChannel(state),
      deps
    })
    return () => subscription.close()
  }, [deps, personId, companyKey, streamStore])

  return useSyncExternalStore(
    streamStore.subscribe,
    streamStore.getSnapshot,
    streamStore.getSnapshot
  )
}
