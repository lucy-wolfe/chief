// Transport-layer types shared by every resource client.

/** Raw HTTP response: status code + body text (not yet parsed as JSON). */
export interface HttpResponse {
  status: number
  body: string
}

/** The seam every resource client is built against. FetchTransport is the
 * only production implementation; tests inject a fake. */
export interface HttpTransport {
  post(path: string, body: unknown): Promise<HttpResponse>
  get(path: string): Promise<HttpResponse>
}

/** Called per request; the returned headers (if any) are merged into the
 * request's headers (agent-auth Bearer). */
export type AuthHeaderProvider = () => Promise<Record<string, string> | undefined>

/** Drops whatever bearer the matching {@link AuthHeaderProvider} has cached, so
 *  the next call to it performs a fresh challenge -> sign -> token round-trip.
 *  Synchronous by design: it only clears state, and the re-acquisition is the
 *  provider's job on its next call. */
export type AuthInvalidate = () => void

export type ChiefdUnavailableKind = 'unreachable' | 'timeout' | 'http-error' | 'malformed-body'

export interface ChiefdClientOptions {
  /** Base URL of ONE company's chiefd — `readDaemonRendezvous(dir).url` for a
   *  caller standing in the directory, or `resolveCompanyChiefdUrl(dir,
   *  discovery)` for one asking the registry about another. REQUIRED — never
   *  resolved from env, never defaulted to a fixed port. */
  url: string
  /** Called per request; merged into headers (agent-auth Bearer). */
  authHeaderProvider?: AuthHeaderProvider
  /** Drop the cached bearer so the next `authHeaderProvider()` re-acquires.
   *
   *  REQUIRED for any long-lived caller, and the reason it exists: the daemon's
   *  HS256 signing secret is EPHEMERAL unless a secret file was provisioned, so
   *  a chiefd restart rotates it and invalidates every token in flight. Without
   *  this hook the client caches its bearer for the life of the process and
   *  every call 401s until the process is respawned — each side assuming the
   *  other handled recovery. It also covers a legitimate key rotation and a
   *  token that simply expires mid-life. */
  authInvalidate?: AuthInvalidate
  /** Test seam. Default: FetchTransport(url, authHeaderProvider,
   *  authInvalidate) at the transport's own DEFAULT_TIMEOUT_MS. */
  transport?: HttpTransport
}
