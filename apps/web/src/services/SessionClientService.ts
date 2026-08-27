/**
 * The browser-side half of the operator auth session: POSTs `/api/session`
 * (the Next route handler that holds the operator private key server-side)
 * and hands back whatever it answered. Holds no state itself — the token
 * lives in `ApiSessionProvider`'s React state, in memory only.
 */
import type { SessionAcquireResult } from '@/types/ChiefApi'
import type { FetchImpl } from '@/types/Fetch'

function isSessionAcquireResult(value: unknown): value is SessionAcquireResult {
  if (!value || typeof value !== 'object') return false
  if (!('identityId' in value) || typeof value.identityId !== 'string') return false
  if (!('token' in value)) return false
  return Object.is(value.token, null) || typeof value.token === 'string'
}

export class SessionClientService {
  private readonly fetchImpl: FetchImpl

  // Bound to the global before it is stored: called as `this.fetchImpl(...)`,
  // an unbound browser `fetch` gets this service as its `this` and Chrome
  // throws "Illegal invocation". Same reason as `ChiefApiClientService`.
  constructor(fetchImpl: FetchImpl = fetch.bind(globalThis)) {
    this.fetchImpl = fetchImpl
  }

  /** POST /api/session. Throws on a transport failure or a malformed
   * response — the provider surfaces that as `ready: false`. */
  async acquire(signal?: AbortSignal): Promise<SessionAcquireResult> {
    const response = await this.fetchImpl('/api/session', { method: 'POST', signal })
    if (response.status !== 200) {
      throw new Error(`session acquire failed: status ${response.status}`)
    }
    const body: unknown = await response.json()
    if (!isSessionAcquireResult(body)) {
      throw new Error('session acquire response missing token/identityId')
    }
    return body
  }
}
