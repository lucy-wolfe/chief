// Shared real-daemon boot for the contract suite (#776), on the AUTHENTICATED
// company surface since A6(c).
//
// Every org contract test boots its OWN daemon + company slug directly via
// `startCompanyDaemon` (never a bespoke spawn implementation -- `@chief/testing`
// is the only place that spawns `chiefd`), gated by `chiefdBinaryTestGate()` at
// the calling file's own top level per the #846 skip-visibly-locally/
// fail-loudly-in-CI split (mirrors the harness suites'
// own convention exactly -- the gate lives in the test file, not buried here, so
// `describe.skip(...)` shows up in every reporter).
//
// WHY `chiefd run --serve-only` (A6(c)). The mode this suite used to boot,
// `chiefd docstore-only`, had no company actor at mount, so it had no auth
// runtime, so no mandatory caller extractor could ever run there. The
// `/v1/org/*` ROUTE FAMILY left it rather than exempt a whole mode from
// authentication -- and the mode itself has since been deleted outright.
// `--serve-only` holds the company and builds the same auth runtime
// `run_company` does, which is why every request below can carry a bearer.
import type { CompanyDaemonOptions } from '@chief/testing'
import { startCompanyDaemon } from '@chief/testing'
import type { ContractDaemon } from '@test/types/Contract'

import { ChiefdClient } from '@/ChiefdClient'
import type { OrganizationSpec } from '@/types/Organization'
import type { SseBearerProvider } from '@/types/Watch'

/**
 * The frozen fixture's `slug`/`tmuxSession` are placeholders -- the real
 * engine refuses genesis with `invalid-input`
 * ("Activity ledger belongs to '<manifest.slug>', not '<request.slug>'")
 * unless the manifest's OWN `slug` field equals the bare request slug
 * exactly (only visible against the real binary: chiefd derives the
 * seeded activity ledger's `organization` from the manifest, then
 * cross-checks it against the request's company identity). Each contract
 * test boots its own daemon under its own slug, so the manifest body must
 * be stamped per-call, never reused verbatim from the static fixture.
 */
export function genesisSpecFor(slug: string): OrganizationSpec {
  // chiefd now takes a SPEC and derives the manifest (and therefore the slug)
  // itself, so the fixture is no longer a manifest body with a stamped-in
  // `slug`/`tmuxSession`. `name` must slugify back to the caller's slug, which
  // is why it is passed through verbatim: every contract test boots its own
  // daemon under its own slug and genesis cross-checks the two.
  return {
    name: slug,
    purpose: 'contract fixture company',
    chief: { name: 'Chief' }
  }
}

/**
 * The SSE reader's credential, from the same bearer the request path uses.
 *
 * `/v1/docs/watch` is a long-lived stream and A4 gave it its own provider seam
 * rather than a header argument, so a subscriber that presented nothing would
 * be an anonymous caller on an authenticated daemon. `invalidate()` is a no-op
 * here on purpose: this harness holds ONE token minted at boot for a daemon
 * that lives and dies with the test, so there is no rotation to recover from
 * and inventing one would be a second acquirer.
 */
export function contractBearer(bearer: string): SseBearerProvider {
  return {
    authHeader: () => Promise.resolve({ authorization: `Bearer ${bearer}` }),
    invalidate: () => {}
  }
}

/**
 * Boots a real `chiefd run --serve-only` daemon scoped to `slug`, ensures its
 * schema, and hands back a `ChiefdClient` wired to it and carrying its bearer.
 *
 * Every `/v1/org/*` request's `slug` field must be the COMPANY KEY,
 * `sha256(<dir>)[..12]` -- `daemon.companyKey`, which the harness minted from
 * the one directory it gave the daemon as `--dir`. Callers pass it themselves:
 * `ChiefdClient` used to take a `root` and rewrite every `slug` into a
 * composite `<bare>@<12-hex-digest>` key, and both the composite and the
 * rewrite are deleted. Sending a display slug produces a key no company
 * matches and EVERY org route answers a blanket `unknown-company` refusal --
 * which reads as the routes being broken rather than the key being wrong.
 *
 * NOT multi-company, and an earlier revision of this doc said otherwise. The
 * deleted `docstore-only` mode's lazy multi-company resolver was described here
 * as load-bearing for the contract suite; it never was. Every `boot(slug)`
 * spawns a fresh daemon for one slug and no database this suite creates has
 * ever held two companies. `--serve-only` serves exactly one company, which is
 * all any caller here asked for.
 *
 * Does NOT genesis a company on its own -- a caller that needs `/v1/org/*`
 * routes to answer `found: true` must call
 * `client.manifest.genesis(daemon.companyKey, genesisSpecFor(slug))` itself
 * after this resolves.
 *
 * The `ensureSchema()` below is kept although `--serve-only` proves its own
 * schema at boot: it is the first authenticated request this client makes, so
 * a broken credential fails HERE, at the boot, instead of inside whichever
 * route the suite happened to call first.
 */
export async function bootContractDaemon(
  slug: string,
  extraOptions: Partial<CompanyDaemonOptions> = {}
): Promise<ContractDaemon> {
  const daemon = await startCompanyDaemon({ slug, ...extraOptions })
  const client = new ChiefdClient({
    url: daemon.url,
    authHeaderProvider: () => Promise.resolve({ authorization: `Bearer ${daemon.bearer}` })
  })
  await client.docs.ensureSchema()
  return {
    daemon,
    client,
    stop: () => daemon.stop()
  }
}
