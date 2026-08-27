/**
 * Only the routes that HOST an agent may reach the agent runtime.
 *
 * # The defect this exists to make impossible
 *
 * `GET /api/companies` lists companies out of beacond and probes each daemon's
 * health. It touches no model, no provider and no harness. It nevertheless
 * loaded all of `@earendil-works/pi-agent-core` and `@earendil-works/pi-ai`,
 * through a chain nobody had ever looked at:
 *
 *   companies/route.ts → server/RouteResult → server/PersonTalk
 *                      → server/AgentHost + server/HostedRoster
 *                      → server/OperatorPi → @earendil-works/pi-ai
 *
 * `RouteResult` imported the three refusal classes from the modules that raise
 * them, and those modules ARE the runtime. The cost was not merely a slow
 * compile: `pi-ai`'s `env-api-keys` module fires three dynamic `import()`s at
 * module scope with no `.catch`, the bundler cannot resolve a computed
 * specifier so it substitutes a throwing stub, and Node exits on an unhandled
 * rejection. Listing companies crashed the server. The operator saw a `502` on
 * the very first screen with eight identical stack traces, none of which named
 * a file in this repository.
 *
 * `next.config.ts` stops the bundler mangling those packages. This stops the
 * import graph growing back — a guard the config fix does not provide, because
 * a listing route dragging in a provider catalog is wrong whether or not the
 * bundler copes with it.
 *
 * # Why the graph is walked rather than the routes executed
 *
 * A test that imported each route under Vitest would prove nothing about this:
 * Vitest does not use Turbopack, resolves the same computed specifier fine,
 * and would be green while the product crashed. The evidence has to be the
 * import graph itself, so that is what this reads — from the files on disk,
 * never from a fixture.
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const here = dirname(fileURLToPath(import.meta.url))
const srcRoot = join(here, '..', 'src')
const apiRoot = join(srcRoot, 'app', 'api')

/** The packages that ARE the agent runtime. */
const RUNTIME_PACKAGES = ['@earendil-works/pi-agent-core', '@earendil-works/pi-ai']

/**
 * The routes that legitimately host an agent, and why.
 *
 * Every one of these runs a turn, reads the transcript the harness is writing,
 * subscribes to it, or changes the live harness's route. They cannot do their
 * job without the runtime. Every OTHER route is a pass-through to chiefd or to
 * beacond and has no business loading a provider catalog.
 */
const MAY_REACH_RUNTIME: Record<string, string> = {
  '/companies/:x/people/:x/say': 'runs a turn on the hosted harness',
  '/companies/:x/people/:x/abort': 'stops the turn the hosted harness is running',
  '/companies/:x/people/:x/transcript': 'reads the session the hosted harness writes',
  '/companies/:x/people/:x/stream': 'subscribes to the hosted harness',
  '/companies/:x/people': 'converges this process’s roster against chiefd’s',
  // Founder is hosted in this process exactly as a person is — it is the same
  // `AgentHarness` — so its three verbs need the runtime for the same reasons
  // the person verbs do. What it is NOT is a person: no company, no roster, no
  // company key, which is why these paths carry none.
  '/founder/say': 'runs a turn on the hosted Founder harness',
  '/founder/abort': 'stops the turn the hosted Founder harness is running',
  '/founder/transcript': 'reads the session the hosted Founder harness writes'
}

function walk(dir: string): string[] {
  const found: string[] = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) found.push(...walk(full))
    else if (entry.isFile()) found.push(full)
  }
  return found
}

/** Every `@/…` and package specifier a file imports. */
function importsOf(file: string): string[] {
  const source = readFileSync(file, 'utf8')
  const found: string[] = []
  // `import … from 'x'`, `import 'x'`, and `export … from 'x'`. Type-only
  // imports are INCLUDED deliberately at this stage and filtered below: a
  // reader cannot tell from the specifier alone, and treating every one as
  // erased would let a value import hide behind the word `type`.
  for (const match of source.matchAll(/(?:^|\n)\s*(?:import|export)[\s\S]*?from\s+'([^']+)'/g)) {
    const specifier = match[1]
    if (typeof specifier === 'string') found.push(specifier)
  }
  for (const match of source.matchAll(/(?:^|\n)\s*import\s+'([^']+)'/g)) {
    const specifier = match[1]
    if (typeof specifier === 'string') found.push(specifier)
  }
  return found
}

/** A file's imports, with the ones erased at compile time removed.
 *
 * `import type { X } from 'y'` emits no `require`, so it cannot drag a package
 * into a bundle and must not fail this test. Anything else can. */
function valueImportsOf(file: string): string[] {
  const source = readFileSync(file, 'utf8')
  const typeOnly = new Set<string>()
  for (const match of source.matchAll(/(?:^|\n)\s*import\s+type\s[\s\S]*?from\s+'([^']+)'/g)) {
    const specifier = match[1]
    if (typeof specifier === 'string') typeOnly.add(specifier)
  }
  // A file may import both a type and a value from the same module
  // (`import { type A, B } from 'm'`), which the pattern above does not match
  // because it requires `import type` at the head — so such a module stays a
  // value import, which is correct.
  return importsOf(file).filter((specifier) => !typeOnly.has(specifier))
}

/** Resolve a `@/…` specifier to a file on disk, or nothing. */
function resolveLocal(specifier: string): string | undefined {
  if (!specifier.startsWith('@/')) return undefined
  const base = resolve(srcRoot, specifier.slice(2))
  for (const candidate of [`${base}.ts`, `${base}.tsx`, join(base, 'index.ts')]) {
    if (statSync(candidate, { throwIfNoEntry: false })?.isFile() === true) return candidate
  }
  return undefined
}

/** Every runtime package reachable from `entry` by value imports. */
function runtimeReachedFrom(entry: string): string[] {
  const seen = new Set<string>()
  const reached = new Set<string>()
  const queue = [entry]
  while (queue.length > 0) {
    const file = queue.pop()
    if (typeof file !== 'string' || seen.has(file)) continue
    seen.add(file)
    for (const specifier of valueImportsOf(file)) {
      const runtime = RUNTIME_PACKAGES.find(
        (name) => specifier === name || specifier.startsWith(`${name}/`)
      )
      if (typeof runtime === 'string') reached.add(runtime)
      const local = resolveLocal(specifier)
      if (typeof local === 'string') queue.push(local)
    }
  }
  return [...reached].sort()
}

/** A route file's browser path, named the way `MAY_REACH_RUNTIME` names it. */
function routePath(file: string): string {
  return `/${relative(apiRoot, dirname(file))}`.replaceAll('\\', '/').replace(/\[[^\]]+\]/g, ':x')
}

function routeFiles(): string[] {
  const stat = statSync(apiRoot, { throwIfNoEntry: false })
  if (stat?.isDirectory() !== true) {
    throw new Error(
      '[RouteRuntimeReach] src/app/api is missing — refusing to pass on an empty scan'
    )
  }
  return walk(apiRoot).filter((file) => file.endsWith(`${'route'}.ts`))
}

describe('only agent-hosting routes reach the agent runtime', () => {
  const routes = routeFiles()

  it('found routes at all (an empty scan proves nothing)', () => {
    expect(routes.length).toBeGreaterThan(5)
  })

  it('proves the walker can SEE the runtime (a blind walker passes everything)', () => {
    // The say route is the one that must reach it. If this ever comes back
    // empty the guard below is vacuous, and a vacuous guard is worse than
    // none: it reports safety it never checked.
    const say = routes.find((file) => routePath(file) === '/companies/:x/people/:x/say')
    expect(say).toBeDefined()
    expect(runtimeReachedFrom(say ?? '')).not.toEqual([])
  })

  it('keeps the runtime out of every route that does not host an agent', () => {
    const offenders = routes
      .map((file) => ({ path: routePath(file), reached: runtimeReachedFrom(file) }))
      .filter((route) => route.reached.length > 0 && !(route.path in MAY_REACH_RUNTIME))
      .map((route) => `${route.path} reaches ${route.reached.join(', ')}`)

    expect(offenders).toEqual([])
  })

  it('carries no stale allowance (a row for a route that no longer needs it)', () => {
    const paths = new Set(routes.map(routePath))
    const stale = Object.keys(MAY_REACH_RUNTIME).filter((path) => {
      if (!paths.has(path)) return true
      const file = routes.find((entry) => routePath(entry) === path)
      return runtimeReachedFrom(file ?? '').length === 0
    })

    expect(stale).toEqual([])
  })
})
