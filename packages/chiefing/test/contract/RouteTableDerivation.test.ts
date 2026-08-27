/**
 * #751/G6 — the route table is DERIVED from the Rust router, not transcribed
 * from it.
 *
 * What went wrong that this replaces. `test/fixtures/route-table.json` was
 * hand-maintained. Its own header said every path had been "copied verbatim
 * from the real Rust router ... at authoring time" — and that was true. But
 * "at authoring time" is the whole defect: when E7-S7 deleted
 * `/v1/org/company-removal/{read,publish,clear}` on the server (no crate
 * registers them; `chiefd-core/src/schema.rs:496-510` DROPs all four
 * `company_removal*` tables and `store/mod.rs:1323` asserts they cannot
 * survive an open), nothing updated the fixture, because the fixture was
 * checked against the CLIENT, never against the server. `RowStores.ts`
 * shipped three methods dialing routes that would 404, its comments called
 * those routes "still-served", `route-table.json` agreed with the comments,
 * and `RoutePathFreeze.test.ts` went green against a RecordingTransport that
 * answers anything. Two sides that look correct alone — with a guard that
 * could only ever see one of them.
 *
 * So this file reads the Rust `.route("...")` literals at test time and makes
 * the server the authority:
 *
 *  1. every `/v1/...` path the chiefing SOURCE dials must be served by some
 *     Rust router. This is the check that fails on the company-removal class,
 *     and it covers all ~200 client paths — including the ~85 belonging to
 *     the six clients (`OrgSlice`, `Runtime`, `SessionLifecycle`, `Settings`,
 *     `CompanyLifecycle`, `FounderLaunch`) that the invocation freeze never
 *     reached. It needs no maintenance: delete a route in Rust and this goes
 *     red on the next run.
 *  2. every VALUE in `route-table.json` must be served. The frozen table can
 *     no longer carry a row for a route that does not exist.
 *  3. every route the Rust routers serve is either dialed by chiefing or
 *     named in `ROUTES_WITH_NO_TYPESCRIPT_CLIENT` below, so a new server
 *     route is an explicit decision rather than a silent asymmetry.
 *
 * Deliberately NOT checked here: request/response BODY shapes. Those are the
 * real-binary contract suite's job (`RowsContract`, `OrgSliceContract`, …) —
 * a path can exist and still be dialed with the wrong body, and pretending a
 * path check covers that would be the same "guard that cannot see" mistake
 * one level up.
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

function repoRoot(): string {
  let dir = dirname(fileURLToPath(import.meta.url))
  for (let depth = 0; depth < 10; depth += 1) {
    try {
      if (statSync(join(dir, 'apps', 'chiefd', 'crates')).isDirectory()) return dir
    } catch {
      // keep walking up
    }
    dir = dirname(dir)
  }
  throw new Error('could not locate the repo root (no apps/chiefd/crates above this test)')
}

const ROOT = repoRoot()
const CRATES = join(ROOT, 'apps', 'chiefd', 'crates')

function walkFiles(dir: string, extension: string): string[] {
  const out: string[] = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) {
      if (entry.name === 'target' || entry.name === 'node_modules') continue
      out.push(...walkFiles(path, extension))
    } else if (entry.isFile() && entry.name.endsWith(extension)) {
      out.push(path)
    }
  }
  return out
}

/**
 * Remove every `#[cfg(test)] ... { ... }` item, brace-matched.
 *
 * Splitting on the FIRST `#[cfg(test)]` (the idiom two Rust files in this
 * repo use on themselves) is not good enough here and the difference is not
 * cosmetic: `chiefd-api/src/docstore/mod.rs` has an inline test module ahead
 * of the real `/v1/docs/runtime`, `/v1/docs/queue` and `/v1/admin/shutdown`
 * registrations, so a naive split silently drops three genuinely-served
 * routes and this file would then report three client paths as unserved.
 * Under-reporting the server's surface produces false REDs; over-reporting it
 * (counting a test-only fake router as production) produces false GREENs,
 * which is worse. Hence: exact regions, both directions.
 */
function stripTestModules(source: string): string {
  const marker = /#\[cfg\(test\)\]/g
  const out: string[] = []
  let cursor = 0
  for (;;) {
    marker.lastIndex = cursor
    const match = marker.exec(source)
    if (!match) {
      out.push(source.slice(cursor))
      break
    }
    out.push(source.slice(cursor, match.index))
    const open = source.indexOf('{', match.index + match[0].length)
    if (open === -1) break
    let depth = 0
    let index = open
    for (; index < source.length; index += 1) {
      const char = source[index]
      if (char === '{') depth += 1
      else if (char === '}') {
        depth -= 1
        if (depth === 0) break
      }
    }
    cursor = index + 1
  }
  return out.join('')
}

/** Strip `//` line comments and `/* *\/` block comments. A commented-out
 * `.route(...)` claiming a route is served is the one failure direction that
 * turns this whole file green on a lie. */
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, ' ').replace(/\/\/.*$/gm, ' ')
}

/** Each crate subtree that mounts its own axum router, and the server a
 * chiefing client reaches when it dials one of its paths. */
const ROUTER_ROOTS: ReadonlyArray<{ server: string; dir: string }> = [
  { server: 'beacond', dir: join(CRATES, 'beacond', 'src') },
  { server: 'chief host', dir: join(CRATES, 'chief-cli', 'src', 'host') },
  { server: 'chiefd founder', dir: join(CRATES, 'chief-cli', 'src') },
  { server: 'chiefd docstore api', dir: join(CRATES, 'chiefd-api', 'src') }
]

function servedRoutes(): Map<string, Set<string>> {
  const served = new Map<string, Set<string>>()
  for (const { server, dir } of ROUTER_ROOTS) {
    for (const file of walkFiles(dir, '.rs')) {
      const source = stripComments(stripTestModules(readFileSync(file, 'utf8')))
      for (const match of source.matchAll(/\.route\(\s*"([^"]+)"/g)) {
        const path = match[1]
        if (typeof path !== 'string') continue
        const servers = served.get(path) ?? new Set<string>()
        servers.add(server)
        served.set(path, servers)
      }
    }
  }
  return served
}

/**
 * Every `/v1/...` path this package dials, with the source files that dial
 * it. Matched on the bare path shape rather than on a quoted-string form so
 * the one dynamically-assembled URL in the package —
 * `sse/SseWatcher.ts`'s `` `${this.url}/v1/docs/watch?...` `` — is covered
 * exactly like the literals. Query strings and interpolations are cut off at
 * `?` / `$` because a route is registered by path alone.
 */
function dialedPaths(): Map<string, Set<string>> {
  const dialed = new Map<string, Set<string>>()
  for (const file of walkFiles(join(ROOT, 'packages', 'chiefing', 'src'), '.ts')) {
    const source = stripComments(readFileSync(file, 'utf8'))
    for (const match of source.matchAll(/\/v1\/[A-Za-z0-9/_-]+/g)) {
      const path = match[0]
      const files = dialed.get(path) ?? new Set<string>()
      files.add(
        file
          .slice(ROOT.length + 1)
          .split(sep)
          .join('/')
      )
      dialed.set(path, files)
    }
  }
  return dialed
}

const SERVED = servedRoutes()
const DIALED = dialedPaths()

const ROUTE_TABLE: Record<string, string> = JSON.parse(
  readFileSync(join(ROOT, 'packages', 'chiefing', 'test', 'fixtures', 'route-table.json'), 'utf8')
)
const FROZEN_PATHS = Object.entries(ROUTE_TABLE)
  .filter(([key]) => key !== '_comment')
  .map(([, path]) => path)

/**
 * Rust routes with no chiefing client, each with the reason. This list is the
 * only hand-maintained thing left, and it is maintained in the safe
 * direction: forgetting to add a row here fails the test, so a new server
 * route cannot quietly become client-less.
 */
const ROUTES_WITH_NO_TYPESCRIPT_CLIENT: Readonly<Record<string, string>> = {
  // beacond writes: only a chiefd daemon registers/heartbeats its own
  // location. chiefing READS discovery (list/lookup) and creates and deletes
  // companies; it never claims to be one.
  '/v1/register': 'chiefd registers its own location; no TypeScript caller is a daemon',
  // The Rust host enrols each person in the same operation that creates the
  // agent home. A TypeScript client would duplicate that one owner.
  '/v1/auth/enroll': 'chiefd enrols a person when it writes their home; no TypeScript caller',
  // The settle countdown's idleness beat. Its ONLY caller is the person's own
  // Pi pane, which posts it from `organization-intercom.ts` on the same event
  // set that feeds `noteTurnProgress` -- a Pi extension is copied verbatim into
  // each pi-home and cannot import a chiefing client, which is the same reason
  // `team-ui.ts` hand-copies the settle constants. A typed client here would be
  // a client nothing could dial.
  '/v1/org/activity/agent-state':
    'the pane reports its own agent activity; a Pi extension cannot import a chiefing client',
  '/v1/heartbeat': 'chiefd renews its own registration',
  // The operator's company stand-down, and both its doors are outside
  // TypeScript's reach. The CLI door is `chief stand-down` / `chief resume` in
  // the Rust operator client, which links no TypeScript; the other is
  // `org_stand_down` / `org_resume` in `organization-intercom.ts`, and a Pi
  // extension is copied verbatim into each pi-home and cannot import a
  // chiefing client — the same reason `/v1/org/activity/agent-state` is here.
  //
  // The reason none is WANTED, and not merely absent: a stand-down is a
  // decision about who RUNS, and only an actuator can carry it out. apps/web
  // renders a company from `/v1/org/tree/structured`; it does not spawn or
  // park its people, so a chiefing method here would be a typed client for an
  // effect no TypeScript caller can observe or complete — the objection that
  // already keeps `/v1/org/person/wake` off this surface. If a browser ever
  // gains a stop control, it gains an actuator too, and THAT is when these
  // rows come out.
  '/v1/org/stand-down':
    'the operator stands a company down from the CLI or the CEO pane; neither can be a chiefing client',
  '/v1/org/stand-down/clear':
    'the resume half of the same gesture, through the same two non-TypeScript doors',
  '/v1/org/stand-down/read':
    'read back by `chief stand-down`/`chief resume` to print what the durable row now says',
  // chiefd-internal: the converge/actuation surface `chief host` drives
  // in-process. apps/api reaches these through its own resident host
  // (DECISIONS.md:6365/6368), not through a chiefing client.
  '/v1/org/converge-safety/set-actuation-config':
    'converge actuation config is set by chief host, not by a client',
  '/v1/org/projection/reconcile': 'reconciliation is driven inside chief host',
  // The sidebar rail's ONE write. Its only caller is
  // `ActuationClient::wake_person` in the Rust operator client, posted when the
  // operator CLICKS a person the rail drew as `starting` or `sleeping` — the
  // gesture is a mouse event inside `chief attach`'s own tmux pane, and that
  // rail is a Rust TUI (`chief-cli/src/sidebar/`) that links no TypeScript.
  //
  // The reason no TypeScript client is WANTED, and not merely absent: waking is
  // a decision about who runs, which only an actuator can carry out, and the
  // Rust operator client is the only actuator this product has. apps/web
  // renders a company from `/v1/org/tree/structured`; it does not spawn or park
  // its people, so a chiefing method here would be a typed client for an
  // effect no TypeScript caller can observe or complete — the same objection
  // that keeps `/v1/org/runtime/desired` and `/v1/org/runtime/launch-catalog`
  // off the TypeScript surface. If a browser ever gains a wake control, it
  // gains an actuator too, and THAT is when this row comes out.
  '/v1/org/person/wake':
    'the rail wakes a clicked person from the Rust operator client; no TypeScript actuator exists',
  // #751/P4: the client-agnostic roster facts. Its consumer is the Rust
  // operator client (P5), which speaks HTTP directly and links no TypeScript.
  // apps/web renders a company from `/v1/org/tree/structured`, which answers
  // the identity/structure question a browser actually asks; a second
  // TypeScript client for the same company with a runtime-desired flag on it
  // would be a second roster in the language that has already produced one.
  '/v1/org/roster/desired':
    'the roster facts are consumed by the Rust operator client, not by chiefing',
  // The desired set. Its consumer is the Rust operator client's resident
  // actuator mode, which diffs it against the panes in front of it. There is
  // deliberately no TypeScript client: a second roster in the language that has
  // already produced one is what mandate 3 forbids, and apps/web is not an
  // actuator — it renders a company, it does not spawn its people.
  //
  // TOMBSTONE: `/v1/org/runtime/observed` and `/v1/org/runtime/actions`. The
  // first was allowlisted as "an actuator reports its own runtime", which is
  // exactly the direction that is now barred: the actuator reports NOTHING to
  // chiefd. Both routes are deleted rather than left client-less.
  '/v1/org/runtime/desired':
    'the desired set is consumed by the Rust operator client, not by chiefing',
  // #751/P8: the other half of a start. `actions` says WHO; this says WITH
  // WHAT — the pi binary, pi-home, workspace, tools, accent
  // and pane environment. Same consumer, same reason for having no TypeScript
  // client, plus one of its own: the body is launch inputs for a process only
  // the Rust actuator spawns, and a TypeScript reader of it would be reading
  // a contract it can never act on.
  '/v1/org/runtime/launch-catalog':
    'the launch catalog is consumed by the Rust operator client, not by chiefing'
}

describe('the Rust router is the authority on which routes exist (#751/G6)', () => {
  it('the derivation is not vacuous: the routers and the client both parse', () => {
    // Floors, not exact counts: an exact count would be a second
    // hand-maintained number drifting beside the first one. These only have
    // to be high enough that a parse returning almost nothing is loud.
    // 150 -> 120: the publisher-route sweep deleted 25 served routes and the
    // 18 client methods that dialled some of them, so both real surfaces are
    // genuinely smaller and the floors follow them down. 130 was measured too
    // finely and landed exactly ON the real count, which fails a `>` check —
    // these are floors that must stay loud on a parse returning nothing, not
    // second inventories tracking the surface to the unit.
    // 120 -> 110 with provider/model management: the provider-models family,
    // the two model-change routes, the two staffing previews, the runtime
    // switches and the runtime-preference write are deleted routes, so both
    // real surfaces are genuinely smaller and the floors follow them down.
    // 110 -> 100 with materialization and the resource catalog
    // (chief-home-is-cwd §4d/§4e): the four `POST /v1/org/materialize/*` routes
    // and `POST /v1/org/resource-catalog/read` are deleted, taking five client
    // methods with them. The real served count is now exactly 110, and a floor
    // sitting ON the real count fails a `>` check the first time anything else
    // shrinks — the same mistake the 130 note above records. These stay floors
    // that must be loud when a parse returns almost nothing, not inventories.
    //
    // 100 -> 80 with `org_maintain_session` (2026-08-24): the five
    // `/v1/org/company-session-action/*` routes and
    // `/v1/org/session-maintenance/complete-native` are deleted, taking their
    // client methods with them.
    //
    // AND THE FLOOR IS SET WELL BELOW THE REAL COUNT THIS TIME, DELIBERATELY.
    // The note above says a floor landing ON the count fails the next time
    // anything shrinks — and then the NEXT lowering put it back on the count,
    // and this deletion broke it again. Third time. The lesson was recorded
    // twice and applied neither time, because "lower it to the new count" is
    // the obvious move and the comment's warning reads as history rather than
    // instruction. 80 is a vacuity tripwire with real headroom: it is loud if a
    // parse returns almost nothing, and it does not have to be edited by every
    // deletion that follows. **A floor that must be re-measured on every change
    // is an inventory wearing a floor's clothes.**
    expect(SERVED.size).toBeGreaterThan(80)
    expect(DIALED.size).toBeGreaterThan(80)
    // 95 -> 83, for the same twelve deleted route families RoutePathFreeze's
    // own floor records. 83 -> 81 with the loan concept (operator ruling,
    // 2026-08-13): `/v1/org/person/loan` and `/v1/org/person/return` are
    // deleted verbs, not moved ones, so the real surface is two smaller and
    // the floor follows it down rather than the other way round. 81 -> 63
    // with the publisher-route sweep, which deleted eighteen frozen rows
    // whose routes no caller of any kind ever posted; same reasoning again.
    // 63 -> 54 with provider/model management: the two model-change routes,
    // the two staffing previews, the provider-models list and available reads,
    // the two runtime switches, the runtime-preference write and the
    // model-selection materialize all go with the feature, and their frozen
    // rows go with them.
    expect(FROZEN_PATHS.length).toBeGreaterThan(54)
  })

  it('every router subtree contributes routes (no silently-empty scan root)', () => {
    const perServer = new Map<string, number>()
    for (const servers of SERVED.values()) {
      for (const server of servers) perServer.set(server, (perServer.get(server) ?? 0) + 1)
    }
    for (const { server } of ROUTER_ROOTS) {
      expect(perServer.get(server) ?? 0, `${server} contributed no routes`).toBeGreaterThan(0)
    }
  })

  it('every path packages/chiefing/src dials is served by a Rust router', () => {
    const unserved = [...DIALED.entries()]
      .filter(([path]) => !SERVED.has(path))
      .map(([path, files]) => `${path} (dialed by ${[...files].sort().join(', ')})`)
      .sort()
    expect(
      unserved,
      'a chiefing client dials a route no crate registers — delete the client method, ' +
        'or add the route in Rust'
    ).toEqual([])
  })

  it('every frozen route-table.json path is served by a Rust router', () => {
    const stale = FROZEN_PATHS.filter((path) => !SERVED.has(path)).sort()
    expect(stale, 'route-table.json freezes a path chiefd no longer serves').toEqual([])
  })

  it('every served route is either dialed by chiefing or explicitly client-less', () => {
    const unaccounted = [...SERVED.keys()]
      .filter((path) => !DIALED.has(path))
      .filter((path) => !(path in ROUTES_WITH_NO_TYPESCRIPT_CLIENT))
      .sort()
    expect(
      unaccounted,
      'chiefd serves routes chiefing neither dials nor declares client-less — add a ' +
        'client, or add a row to ROUTES_WITH_NO_TYPESCRIPT_CLIENT with the reason'
    ).toEqual([])
  })

  it('ROUTES_WITH_NO_TYPESCRIPT_CLIENT carries no rows of its own that drifted', () => {
    // The allowlist is itself a hand-maintained table, so it gets the same
    // treatment it exists to enforce: a row naming a route that no longer
    // exists, or one that a client has since started dialing, is a defect
    // (this is #963's stale-allowlist class, one file over).
    const gone = Object.keys(ROUTES_WITH_NO_TYPESCRIPT_CLIENT)
      .filter((path) => !SERVED.has(path))
      .sort()
    expect(gone, 'allowlisted route is no longer served — delete the row').toEqual([])

    const nowDialed = Object.keys(ROUTES_WITH_NO_TYPESCRIPT_CLIENT)
      .filter((path) => DIALED.has(path))
      .sort()
    expect(nowDialed, 'allowlisted route now HAS a chiefing client — delete the row').toEqual([])
  })

  it('the company-removal routes stay deleted on both sides (#751/G6 regression)', () => {
    // The specific defect. Named rather than left to the generic checks so a
    // revert reads as "the thing #751 found", not as an anonymous diff.
    for (const path of [
      '/v1/org/company-removal/read',
      '/v1/org/company-removal/publish',
      '/v1/org/company-removal/clear'
    ]) {
      expect(SERVED.has(path), `${path} must not be served`).toBe(false)
      expect(DIALED.has(path), `${path} must not be dialed`).toBe(false)
      expect(FROZEN_PATHS).not.toContain(path)
    }
  })
})

describe('negative self-checks: each scanner can actually fail', () => {
  it('a fabricated client path would be reported unserved', () => {
    expect(SERVED.has('/v1/org/not-a-real-route/read')).toBe(false)
  })

  it('stripTestModules removes a test-only router but keeps production routes', () => {
    const source =
      'fn prod() { Router::new().route("/v1/real", get(h)) }\n' +
      '#[cfg(test)]\nmod tests {\n  fn fake() { Router::new().route("/v1/fake", get(h)); }\n}\n' +
      'fn late() { Router::new().route("/v1/late", get(h)) }\n'
    const stripped = stripTestModules(source)
    expect(stripped).toContain('/v1/real')
    expect(stripped).toContain('/v1/late')
    expect(stripped).not.toContain('/v1/fake')
  })

  it('stripComments removes a commented-out route registration', () => {
    expect(stripComments('// .route("/v1/ghost", get(h))\n')).not.toContain('/v1/ghost')
    expect(stripComments('/* .route("/v1/ghost2", get(h)) */')).not.toContain('/v1/ghost2')
    expect(stripComments('.route("/v1/kept", get(h))')).toContain('/v1/kept')
  })

  it('the dialed-path scanner normalises a query-string URL to its route path', () => {
    // sse/SseWatcher.ts builds `${url}/v1/docs/watch?slug=...`; the route is
    // registered as `/v1/docs/watch`, so the scanner must not carry the query.
    expect(DIALED.has('/v1/docs/watch')).toBe(true)
    expect([...DIALED.keys()].some((path) => path.includes('?'))).toBe(false)
  })
})
