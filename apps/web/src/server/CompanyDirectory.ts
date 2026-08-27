/**
 * Which companies exist, and which are actually up.
 *
 * # Two sources, and neither answers for the other
 *
 * beacond knows which companies EXIST — it holds the registry, keyed by the
 * DIRECTORY each occupies. It does not know whether a daemon is alive: a row
 * carries a url only while a chiefd has registered one, and a process that
 * died without deregistering leaves that url behind.
 *
 * So existence comes from beacond and liveness comes from asking the daemon
 * itself. Treating a registered url as proof of life is how a directory shows
 * "running" for a company nobody can talk to — the operator clicks it and
 * every request 502s.
 *
 * # A dead daemon is one company's problem
 *
 * Health is probed per company and a failure becomes that row's status, never
 * an error for the whole list. A directory that failed because ONE company's
 * daemon was down would hide every healthy company behind the one broken one.
 */
import { describeFetchFailure, DiscoveryClient } from '@chief/chiefing'

import { beacondUrl } from '@/common/Env'
import type { ChiefdHealth, CompanyDirectoryEntry } from '@/types/CompanyDirectory'
import { isNullish } from '@/utils/Nullish'

/** How long a health probe may take before the company counts as unreachable.
 *
 * Short on purpose: this runs once per company on a page the operator is
 * waiting for, and a daemon that cannot answer in a second is not one they can
 * usefully click into. */
const PROBE_TIMEOUT_MS = 1000

async function probe(url: string): Promise<ChiefdHealth> {
  try {
    // `/v1/docs/health`, the route chiefd actually serves. An invented path
    // answers 404, which `response.ok` reads as unhealthy — so every RUNNING
    // company reported as stopped, and the directory was uniformly wrong in
    // the one direction an operator cannot argue with. Observed live against a
    // company that was answering perfectly.
    const response = await fetch(new URL('/v1/docs/health', url), {
      signal: AbortSignal.timeout(PROBE_TIMEOUT_MS)
    })
    return {
      healthy: response.ok,
      httpStatus: response.status,
      reason: response.ok ? 'ok' : `chiefd answered ${response.status}`
    }
  } catch (error) {
    // Refused, timed out, or DNS — all the same fact to an operator: nobody is
    // listening. The message is carried so they can tell a timeout from a
    // refusal without reading a log.
    return {
      healthy: false,
      // `describeFetchFailure`, not `error.message`: undici says `fetch
      // failed` and keeps the code and port on `error.cause`. "Refused at
      // 127.0.0.1:8799" and "timed out" are different operator actions, and
      // the two words distinguish neither.
      reason: describeFetchFailure(error)
    }
  }
}

/** Every registered company, with whether its daemon actually answers. */
export async function companyDirectory(): Promise<CompanyDirectoryEntry[]> {
  const rows = await new DiscoveryClient({ beacondUrl: beacondUrl() }).list()
  return Promise.all(
    rows.map(async (row) => {
      if (isNullish(row.url)) {
        // Registered, never started, or cleanly deregistered. Not an error —
        // it is the ordinary state of a company nobody has attached yet.
        return {
          key: row.key,
          dir: row.dir,
          slug: row.slug,
          status: 'stopped' as const,
          chiefd: { healthy: false, reason: 'no chiefd registered for this company' }
        }
      }
      const chiefd = await probe(row.url)
      return {
        key: row.key,
        dir: row.dir,
        slug: row.slug,
        // A url that does not answer is STOPPED, not running. The registry
        // says a daemon once registered; only the probe says one is there now.
        status: chiefd.healthy ? ('running' as const) : ('stopped' as const),
        url: row.url,
        chiefd
      }
    })
  )
}

/** One company by its KEY, or `undefined` when beacond has never heard of it.
 *
 * By key and not by slug, because a slug names no company: two directories may
 * hold companies called the same thing, and `find` would answer with whichever
 * the registry happened to list first. */
export async function companySummary(
  companyKey: string
): Promise<CompanyDirectoryEntry | undefined> {
  const directory = await companyDirectory()
  return directory.find((entry) => entry.key === companyKey)
}
