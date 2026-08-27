/**
 * Where this app's own route handlers live, from the browser's point of view.
 *
 * # Why a constant and not a literal per call site
 *
 * Next.js serves route handlers under `/api/…`, so `app/api/companies/
 * [companyKey]/tree/route.ts` answers at `/api/companies/:companyKey/tree`.
 * Every client path in
 * this app used to be written without that prefix, because they were written
 * when apps/api was a SEPARATE service whose base URL was `http://host:8787`
 * and whose own paths started at `/companies`. apps/api is deleted and the
 * base URL is now this app's own origin, so those same literals resolved to
 * `/companies/…` — a path nothing serves.
 *
 * Every request from the page 404'd, and no unit test caught it: the client
 * agreed with itself and the routes agreed with themselves, so both halves
 * were green while nothing in between worked. That is why the prefix is one
 * constant, and why `ChiefApiClientService` and `SseClientService` are tested
 * against the URL they actually fetch rather than the path they were handed.
 */

/** The mount point of this app's route handlers. */
const API_PREFIX = '/api'

/** One of this app's own routes, as the browser must request it. */
export function appPath(path: string): string {
  return `${API_PREFIX}${path}`
}
