/**
 * Resolve a company's chiefd, server-side.
 *
 * Every Next route handler that touches a company starts here. The browser
 * holds no service address (see `common/PublicEnv.ts`), so this server asks
 * beacond — at its static, well-known port — which daemon serves a company, and
 * then talks to that daemon through `@chief/chiefing`.
 *
 * # A company is addressed by its KEY, and the key comes off the registry row
 *
 * A company is the DIRECTORY the operator ran `chief` in, and `sha256(dir)[..12]`
 * is its identity on the wire. The browser cannot hold a directory — it holds
 * the key, which is URL-safe and is exactly what chiefd resolves a route by.
 *
 * That is why this reads `GET /v1/list` rather than `GET /v1/lookup?dir=`: the
 * registry's lookup takes the directory, which only a process standing IN that
 * directory knows. A server enumerating companies for an operator does not
 * stand anywhere, so it reads the list it already renders and matches the key.
 * A slug cannot be matched at all — two directories may hold companies with the
 * same one, and the registry lists both.
 *
 * This module makes no organization decisions and never will: it resolves an
 * address and hands back a client. Anything a caller wants to KNOW is a
 * question for chiefd — the deleted apps/api stopped being a thin layer
 * exactly when it started answering such questions itself.
 */
import { ChiefdClient, DiscoveryClient } from '@chief/chiefing'

import { beacondUrl } from '@/common/Env'
import { operatorAuth } from '@/server/OperatorBearer'
import { RouteRefusalError } from '@/server/RouteRefusal'
import { isNullish } from '@/utils/Nullish'

/** Why a company could not be reached, in the caller's own vocabulary. */
export class CompanyUnavailableError extends RouteRefusalError {}

/** A registry row that is actually serving: every field a caller here needs,
 * with `url` PROVEN present rather than optional. */
interface RunningCompany {
  readonly key: string
  readonly dir: string
  readonly slug: string
  readonly url: string
}

/**
 * The registry row for one company key, or a typed refusal.
 *
 * The two failure modes are deliberately DIFFERENT answers, because they call
 * for different actions: beacond not knowing the key at all is a 404, while a
 * company beacond knows but that is not running is a 409 naming the command
 * that starts it. Collapsing them is how an operator ends up trying to restart
 * a company that never existed.
 */
async function runningCompany(companyKey: string): Promise<RunningCompany> {
  const rows = await new DiscoveryClient({ beacondUrl: beacondUrl() }).list()
  const company = rows.find((row) => row.key === companyKey)
  if (isNullish(company)) {
    throw new CompanyUnavailableError({
      status: 404,
      code: 'unknown-company',
      message: `no company is registered under key "${companyKey}"`
    })
  }
  const { url } = company
  if (isNullish(url)) {
    throw new CompanyUnavailableError({
      status: 409,
      code: 'company-not-running',
      message:
        `company "${company.slug}" in ${company.dir} is registered but not running — ` +
        `start it by running chief in that directory`
    })
  }
  // `url` is destructured and narrowed rather than asserted: a `CompanyRow`
  // carries it optionally, because a company that has never booted has none,
  // and the whole job of the 409 above is to turn that absence into an answer.
  return { key: company.key, dir: company.dir, slug: company.slug, url }
}

/** A chiefd client for the company with this key, or a typed refusal. */
export async function companyChiefd(companyKey: string): Promise<ChiefdClient> {
  const company = await runningCompany(companyKey)
  // AUTHENTICATED. Every route handler in this app reaches chiefd through this
  // one client, and until A2 it reached it with no credential at all — the
  // acquirer existed (`helpers/OperatorChallenge.ts`) and this call site simply
  // did not use it. `authInvalidate` is not optional for a long-lived server:
  // chiefd's HS256 secret is ephemeral unless provisioned, so a daemon restart
  // invalidates every cached bearer at once and, without the hook, every later
  // call 401s until this process is restarted.
  //
  // The operator key is that COMPANY's own — `<dir>/.chief/keys/operator.key`,
  // minted by the daemon that serves it. There is no box-wide operator key any
  // more, because there is no box-wide company tree for one to sit at the top
  // of.
  return new ChiefdClient({
    url: company.url,
    ...operatorAuth(company.url, company.dir)
  })
}

/**
 * A company's chiefd as a raw endpoint, for the one thing a typed client
 * cannot carry: a byte stream.
 *
 * `ChiefdClient` decodes responses, which is right for every JSON route and
 * wrong for `text/event-stream` — an SSE body must be forwarded as bytes.
 * Parsing it here and re-emitting would be a SECOND implementation of the
 * frame contract, in a language that does not own it, sitting between two
 * implementations that already agree.
 *
 * The refusals are the same ones `companyChiefd` raises, from the same
 * registry read, so a caller cannot get a different answer about whether a
 * company exists depending on which helper it happened to use.
 */
export async function companyChiefdEndpoint(
  companyKey: string
): Promise<{ url: string; dir: string }> {
  const company = await runningCompany(companyKey)
  // No company handle comes back: it used to, as a `documentSlug` this server
  // BUILT (`documentKey(slug, company.orgsRoot)`), and that composite is gone —
  // the key the caller asked with is the key chiefd resolves by, so returning a
  // second copy of it could only ever disagree with the first.
  return { url: company.url, dir: company.dir }
}
