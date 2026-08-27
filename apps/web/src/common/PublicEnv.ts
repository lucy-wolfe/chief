/**
 * Public, build-time-inlined `process.env` access for apps/web.
 *
 * # There is no public service address any more
 *
 * This module used to inline `NEXT_PUBLIC_CHIEF_API_URL` — apps/api's address,
 * baked into the browser bundle. apps/api is DELETED, and the browser must
 * never hold a chiefd address: a company's daemon is started per company on a
 * port allocated at genesis, so any address shipped to a browser is a guess
 * that goes stale the first time that company restarts. Worse, it would make
 * every company's daemon directly reachable from the page.
 *
 * The browser therefore talks to its OWN ORIGIN — this Next server's route
 * handlers — and the server resolves each company's chiefd through beacond
 * (see `Env.ts`). An empty base URL is not a missing value here; it is the
 * same-origin answer, and it is why nothing in this module reads
 * `process.env` at all.
 */

/** The base URL the browser makes its requests against: this app's own origin. */
export function publicApiBaseUrl(): string {
  return ''
}
