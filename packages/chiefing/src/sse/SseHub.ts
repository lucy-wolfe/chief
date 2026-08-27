/**
 * #31 multiplexing hub: ONE watcher per `url|slug` for the whole process.
 * Ported from `apps/cli/src/legacy/organization/org-sse-watcher.ts` (#775,
 * E2-S6) — the public shape is unchanged.
 *
 * The production entry point: joins the process-wide watcher for this
 * `url + slug`, creating it on first use and closing it when the last
 * subscriber leaves. Events are delivered only for the stores THIS caller
 * subscribed to, so multiple callers on one pane share one connection while
 * each still sees exactly its own stores.
 *
 * Imports ONLY relative siblings + node/web builtins (no `@/` alias, no
 * package specifier) and reads no env, touches no disk — see
 * `./SseWatcher.ts`'s module doc for why (E2-S8's hard constraint).
 */

import type {
  SseChannelState,
  SseDocChangeEvent,
  SseSubscription,
  SseWatcherOptions
} from '../types/Watch.js'
import { SseWatcher } from './SseWatcher.js'

interface HubSubscriber {
  stores: Set<string>
  onEvent: (event: SseDocChangeEvent) => void | Promise<void>
  onReorg?: () => void
  onChannelStateChange?: (state: SseChannelState) => void
}

class SseWatcherHub {
  private readonly subscribers = new Set<HubSubscriber>()
  private watcher: SseWatcher | undefined
  private readonly key: string
  private readonly registry: Map<string, SseWatcherHub>

  constructor(key: string, registry: Map<string, SseWatcherHub>) {
    this.key = key
    this.registry = registry
  }

  subscriberCount(): number {
    return this.subscribers.size
  }

  add(subscriber: HubSubscriber, options: SseWatcherOptions): SseSubscription {
    this.subscribers.add(subscriber)
    if (!this.watcher) {
      // `...options` carries `bearer` through with everything else: the hub
      // multiplexes ONE connection per `url|slug`, and a pane holds exactly one
      // person's credential for that pair, so the first joiner's provider is
      // the right one for every co-tenant by construction.
      this.watcher = new SseWatcher({
        ...options,
        stores: [...subscriber.stores],
        onEvent: (event) => this.fanOut(event),
        onReorg: () => {
          for (const sub of [...this.subscribers]) sub.onReorg?.()
        },
        onChannelStateChange: (state) => {
          for (const sub of [...this.subscribers]) sub.onChannelStateChange?.(state)
        }
      })
    } else {
      this.watcher.addStores([...subscriber.stores])
      // A late joiner never saw the transition that set the current state, so
      // replay it once — callers gate their fallback poll floor on it.
      const state = this.watcher.currentChannelState()
      if (state) subscriber.onChannelStateChange?.(state)
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

  private remove(subscriber: HubSubscriber): void {
    this.subscribers.delete(subscriber)
    if (this.subscribers.size > 0) return
    // Last subscriber left: nothing idle keeps running (the HARD RULE).
    this.watcher?.close()
    this.watcher = undefined
    if (this.registry.get(this.key) === this) this.registry.delete(this.key)
  }

  private async fanOut(event: SseDocChangeEvent): Promise<void> {
    // `stores=*` is a selector for every ordinary store, not a literal
    // store name. The watcher correctly asks chiefd for the wildcard, but
    // the hub still owns per-subscriber filtering after multiplexing.
    const targets = [...this.subscribers].filter(
      (sub) => sub.stores.has('*') || sub.stores.has(event.store)
    )
    for (const sub of targets) {
      try {
        await sub.onEvent(event)
      } catch {
        // One subscriber's bug must never starve its co-tenants on the shared
        // connection, nor kill the reader.
      }
    }
  }
}

const sseHubs = new Map<string, SseWatcherHub>()

/**
 * Multiplexing hub: ONE connection per `url|slug` process-wide. `addStores`
 * widens + reconnects preserving cursor; last unsubscribe closes the
 * connection (idle -> zero, the HARD RULE).
 */
export function subscribeSse(options: SseWatcherOptions): SseSubscription {
  const key = `${options.url.replace(/\/+$/, '')}|${options.slug}`
  let hub = sseHubs.get(key)
  if (!hub) {
    hub = new SseWatcherHub(key, sseHubs)
    sseHubs.set(key, hub)
  }
  return hub.add(
    {
      stores: new Set(options.stores),
      onEvent: options.onEvent,
      onReorg: options.onReorg,
      onChannelStateChange: options.onChannelStateChange
    },
    options
  )
}

/** TEST-ONLY: how many multiplexed connections this process currently holds. */
export function activeSseHubCount(): number {
  return sseHubs.size
}
