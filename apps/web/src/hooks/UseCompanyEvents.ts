'use client'

/** Reactive doc-bridge subscription hook; it never polls snapshots. */
import { useEffect, useRef, useSyncExternalStore } from 'react'

import { useCompanyEventsDeps } from '@/providers/CompanyEventsProvider'
import { subscribeDocEvents } from '@/services/SseClientService'
import type { DocChangeEvent, SseChannelState } from '@/types/Sse'

class ChannelStore {
  private readonly listeners = new Set<() => void>()
  private state: SseChannelState = 'connecting'

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  readonly getSnapshot = (): SseChannelState => this.state

  set(state: SseChannelState): void {
    if (this.state === state) return
    this.state = state
    for (const listener of this.listeners) listener()
  }
}

/**
 * Subscribes the component to its exact doc-store key. Callback refs keep a
 * handler current without turning every render into a reconnect.
 */
export function useCompanyEvents(
  companyKey: string,
  stores: readonly string[],
  handlers: { onDoc: (event: DocChangeEvent) => void; onReorg: () => void }
): SseChannelState {
  const deps = useCompanyEventsDeps()
  const storeRef = useRef<ChannelStore | undefined>(undefined)
  if (typeof storeRef.current === 'undefined') storeRef.current = new ChannelStore()
  const channelStore = storeRef.current
  if (typeof channelStore === 'undefined') throw new Error('channel store was not initialized')
  const handlersRef = useRef(handlers)
  handlersRef.current = handlers
  const storesKey = [...new Set(stores)].sort().join(',')

  useEffect(() => {
    const subscribedStores = storesKey.length === 0 ? [] : storesKey.split(',')
    const subscription = subscribeDocEvents({
      companyKey,
      stores: subscribedStores,
      onDoc: (event) => handlersRef.current.onDoc(event),
      onReorg: () => handlersRef.current.onReorg(),
      onChannelState: (state) => channelStore.set(state),
      deps
    })
    return () => subscription.close()
  }, [channelStore, deps, companyKey, storesKey])

  return useSyncExternalStore(
    channelStore.subscribe,
    channelStore.getSnapshot,
    channelStore.getSnapshot
  )
}
