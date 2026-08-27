/**
 * Every path the browser dials is a path this app serves.
 *
 * # The defect this exists to make impossible
 *
 * Twice in one rewrite, a client path and a route file disagreed and the whole
 * suite stayed green — because the client agreed with itself, the routes
 * agreed with themselves, and the fake answered whatever the client sent.
 * Nothing in between was ever checked.
 *
 *   - every path was missing the `/api` prefix Next mounts route handlers
 *     under, so EVERY request from the page 404'd;
 *   - the person stream dialled `…/events` while its route file is
 *     `stream/route.ts`, so that channel 404'd while the company feed — also
 *     `…/events` — was fine.
 *
 * A fake cannot catch either, because a fake is written from the client. Only
 * the ROUTE FILES ON DISK are evidence, so this test derives its answer from
 * them and from the client source, and never from a fixture.
 *
 * # UNSERVED is a list of gaps, not an escape hatch
 *
 * A client method with no route is a 404 waiting to happen. Some are known
 * gaps with a reason to stay open for now, and each is named below with that
 * reason. Adding a row is a decision to ship a method that cannot work yet;
 * the honest alternatives are to serve the route or to delete the method.
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const here = dirname(fileURLToPath(import.meta.url))
const srcRoot = join(here, '..', 'src')

function walk(dir: string): string[] {
  const found: string[] = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) found.push(...walk(full))
    else if (entry.isFile()) found.push(full)
  }
  return found
}

/** Every path this app serves, derived from the route files themselves. */
function servedPaths(): string[] {
  const apiRoot = join(srcRoot, 'app', 'api')
  const stat = statSync(apiRoot, { throwIfNoEntry: false })
  if (!stat?.isDirectory()) {
    throw new Error('[ClientPaths] src/app/api is missing — refusing to pass on an empty scan')
  }
  return walk(apiRoot)
    .filter((path) => path.endsWith(`${'route'}.ts`))
    .map((path) =>
      `/${relative(apiRoot, dirname(path))}`
        .replaceAll('\\', '/')
        // `[companyKey]` is a parameter segment; its NAME is the route author's
        // choice and the client's is its own, so both normalise to `:x`.
        .replace(/\[[^\]]+\]/g, ':x')
    )
    .sort()
}

/**
 * Where a client path can be written.
 *
 * `services/` was the whole list, and that was a hole rather than a boundary:
 * the company-boot path lives in `hooks/UseCompanyDirectory.ts` and was never
 * scanned, so a button dialling a route that did not exist was invisible to
 * the one test whose entire job is to see that.
 */
const CLIENT_DIRECTORIES = ['services', 'hooks']

/**
 * A path built on a local base, resolved to the path it actually becomes.
 *
 * `getTranscript` writes `` `${base}/transcript` `` where `base` is
 * `personPath(companyKey, personId)`. The scanner's pattern required a literal to
 * START with `/companies`, so every one of those was invisible — and
 * `compactSession`, whose route does NOT exist, sat behind exactly that shape.
 * A 404 button hidden from the 404 test.
 *
 * So the bases are resolved: a `const NAME = helper(…)` is looked up against
 * the helper's own returned template in the same file, and `${NAME}` at the
 * head of a template is replaced by it.
 */
function baseValues(source: string): Map<string, string> {
  const helpers = new Map<string, string>()
  for (const match of source.matchAll(
    /function\s+(\w+)\s*\([^)]*\)\s*:\s*string\s*\{\s*return\s+`([^`]+)`/g
  )) {
    const [, name, template] = match
    if (typeof name === 'string' && typeof template === 'string') helpers.set(name, template)
  }
  const bases = new Map<string, string>()
  for (const match of source.matchAll(/const\s+(\w+)\s*=\s*(\w+)\(/g)) {
    const [, name, helper] = match
    if (typeof name !== 'string' || typeof helper !== 'string') continue
    const template = helpers.get(helper)
    if (typeof template === 'string') bases.set(name, template)
  }
  return bases
}

/** The HTTP methods a path's route file actually exports. */
function servedMethods(path: string): string[] {
  const apiRoot = join(srcRoot, 'app', 'api')
  const file = walk(apiRoot)
    .filter((entry) => entry.endsWith(`${'route'}.ts`))
    .find(
      (entry) =>
        `/${relative(apiRoot, dirname(entry))}`
          .replaceAll('\\', '/')
          .replace(/\[[^\]]+\]/g, ':x') === path
    )
  if (typeof file !== 'string') return []
  const source = readFileSync(file, 'utf8')
  return ['GET', 'POST', 'PUT', 'PATCH', 'DELETE'].filter((method) =>
    new RegExp(`export\\s+async\\s+function\\s+${method}\\b`).test(source)
  )
}

/** Every path the browser dials, derived from the client source. */
function dialledPaths(): string[] {
  const literals = new Set<string>()
  const files = CLIENT_DIRECTORIES.flatMap((directory) => walk(join(srcRoot, directory))).filter(
    (path) => path.endsWith('.ts')
  )
  for (const file of files) {
    const source = readFileSync(file, 'utf8')
    // A template whose head is a resolved base becomes the path it really is,
    // before the normalisation below ever sees it.
    const bases = baseValues(source)
    for (const match of source.matchAll(/`\$\{(\w+)\}(\/[^`]*)`/g)) {
      const [, name, tail] = match
      const head = typeof name === 'string' ? bases.get(name) : undefined
      if (typeof head === 'string' && typeof tail === 'string') {
        literals.add(normalisePath(`${head}${tail}`))
      }
    }
    // Backticks AND plain quotes. A path with no parameter is written as an
    // ordinary string (`'/companies'`), and a scanner that read only template
    // literals would be blind to exactly the routes that take no arguments.
    // `founder` joined `companies|session` when Founder Mode became the front
    // door, and its absence was not harmless: the scanner is an ALLOWLIST of
    // path heads, so three new client paths (`/founder/say`, `/founder/abort`,
    // `/founder/transcript`) were invisible to the one test whose whole job is
    // to see them. A head this list does not name is a whole surface the guard
    // silently ignores — the same class as the `/api` prefix it was written
    // for. Any new top-level route family must be added here.
    for (const match of source.matchAll(/[`'"](\/(?:companies|session|founder)[^`'"]*)[`'"]/g)) {
      const raw = match[1]
      if (typeof raw !== 'string') continue
      literals.add(normalisePath(raw))
    }
  }
  return [...literals].sort()
}

/** One written path as the shape a route file would serve. */
function normalisePath(raw: string): string {
  return (
    raw
      // A path SEGMENT is always encoded — that is how this codebase
      // writes them — so `${encodeURIComponent(x)}` becomes `:x`.
      .replace(/\$\{encodeURIComponent\([^)]*\)\}/g, ':x')
      // What remains at the END is a query string glued on
      // (`/goals${query}`), not a segment. Dropping every trailing
      // interpolation instead would erase a real parameter: `/x/:y`
      // would collapse onto the served `/x`, and a single-record read
      // route that does not exist would look served.
      .replace(/\$\{[^}]*\}$/, '')
      .replace(/\$\{[^}]*\}/g, ':x')
      .replace(/\/$/, '')
  )
}

/**
 * Paths the client dials that this app does not serve yet, each with the
 * reason it is still open. Every one is a method that would 404 if called.
 */
const UNSERVED: Record<string, string> = {
  // A person path with no verb: `personPath()` is a prefix helper, and every
  // real call appends a verb to it. It is dialled by nothing on its own.
  '/companies/:x/people/:x': 'prefix helper, never dialled on its own'
}

/**
 * Paths that ARE served, but not for the method a client uses on them.
 *
 * The register above compares PATHS, which is the whole shape of what it can
 * see: `/companies` has a route file, so a client POSTing to it looks served
 * and gets a 405. That is not hypothetical — company creation dials exactly
 * that, and the gap survived the guard for precisely this reason.
 *
 * A path here is a real gap with a real reason, named the same way, so a
 * method-level hole cannot hide behind a path-level pass.
 */
const UNSERVED_METHODS: Record<string, string> = {
  // `POST /companies` used to be here: creating a company needed a
  // `bootstrap` — a live Pi session's exact model route plus a refreshed
  // same-provider observation — which is a provider credential, and a browser
  // has none. It is served now, and NOT by giving this server a credential:
  // `chief host` resolves the box's own Founder route and proves it against
  // the operator's registry, so the browser posts `{name, purpose}` and holds
  // nothing.
}

describe('every path the browser dials is one this app serves', () => {
  const served = servedPaths()
  const dialled = dialledPaths()

  it('found routes and client paths at all (an empty scan proves nothing)', () => {
    expect(served.length).toBeGreaterThan(5)
    expect(dialled.length).toBeGreaterThan(5)
  })

  it('serves every dialled path, or names it as a known gap', () => {
    const missing = dialled.filter((path) => !served.includes(path) && !(path in UNSERVED))

    expect(missing).toEqual([])
  })

  it('carries no stale UNSERVED row (a row for a path now served is a lie)', () => {
    // A row that outlives the gap it describes reads as "we decided to leave
    // this broken" about something that works — and hides the next real gap
    // behind a wall of noise.
    const stale = Object.keys(UNSERVED).filter((path) => served.includes(path))

    expect(stale).toEqual([])
  })

  it('carries no UNSERVED row for a path nothing dials', () => {
    // The same rot from the other side: a row whose method was deleted.
    const orphaned = Object.keys(UNSERVED).filter((path) => !dialled.includes(path))

    expect(orphaned).toEqual([])
  })

  it('names every method-level gap, and none that has closed', () => {
    // A method row is only honest while the PATH is served (otherwise it
    // belongs in `UNSERVED`) and the METHOD is not. `route.ts` exports its
    // methods by name, so both halves are readable from disk.
    const stale = Object.entries(UNSERVED_METHODS).filter(([entry]) => {
      const [method, path] = entry.split(' ')
      if (typeof method !== 'string' || typeof path !== 'string') return true
      if (!served.includes(path)) return true
      return servedMethods(path).includes(method)
    })

    expect(stale.map(([entry]) => entry)).toEqual([])
  })
})
