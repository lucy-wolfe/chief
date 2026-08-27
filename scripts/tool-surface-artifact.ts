/**
 * The tool surface a person actually gets, measured from the ARTIFACT.
 *
 * # Why this exists
 *
 * Every suite in this repo tests the LIST of tools chiefd publishes. Nobody
 * tested the ARTIFACT handed to a provider. Three facts therefore coexisted
 * with a green board for a whole workstream:
 *
 *  - the launch profile names 60 tools for a CEO;
 *  - the web host builds 7 of them and reports 53 as unavailable;
 *  - three tools serialize their parameters as `{"anyOf":[...]}` with NO
 *    top-level `type`, which a strict provider rejects outright — and a
 *    rejected tool definition kills the WHOLE catalog, not just that tool.
 *
 * Not one of those is visible from a list of names. All three are visible in
 * one JSON document, which is what this probe prints:
 *
 *  - `grant`   — the ids the launch profile declares for a converged CEO;
 *  - `built`   — the `AgentTool[]` the host actually constructs from them;
 *  - `missing` — `selectTools`'s own `unavailable`, which is the exact value
 *                the roster publishes as `degraded[].missingTools`;
 *  - `schemas` — one entry per tool object that really exists in this process,
 *                carrying the serialized `parameters` a provider would see;
 *  - `resolvedFrom` — the tree each HALF of that comparison was read out of,
 *                so the guard can refuse two checkouts instead of diffing
 *                them (see `resolvedFrom` below).
 *
 * # What is real here, and what is not
 *
 * REAL: `apps/web`'s own `selectTools`, Pi's own tool constructors, the real
 * `organization-intercom` extension module, its real `pi.registerTool`
 * definitions, and each definition's real `parameters` object.
 *
 * FAKE: one loopback HTTP server standing in for this company's chiefd, the
 * rendezvous file naming it, and the manifest it serves. That is INPUT, not
 * subject —
 * it decides only WHICH person the extension installs for. The subject is the
 * tool objects, and every one of them is built by the real extension code.
 * The registered-tool floor below is what keeps that honest: a stand-in that
 * accidentally produced a worker instead of an executive would register far
 * fewer tools and fail rather than pass quietly.
 *
 * Run: `bun scripts/tool-surface-artifact.ts`
 */
import { createServer } from 'node:http'
import type { AddressInfo } from 'node:net'
import { createHash } from 'node:crypto'
import { mkdirSync, mkdtempSync, realpathSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { selectTools } from '../apps/web/src/server/AgentTools'
import type { AgentProfile } from '../apps/web/src/types/AgentHost'
import {
  installOrganizationIntercom,
  type IntercomOrganizationManifest
} from '../packages/piing/extensions/organization-intercom'
import {
  ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES,
  ORGANIZATION_MANAGER_TOOL_NAMES,
  ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES,
  ORGANIZATION_SUBTREE_TOOL_NAMES
} from '../packages/piing/src/extensionruntime/OrganizationTools'

/**
 * The tree each half of this comparison was actually read out of.
 *
 * # Why a guard has to measure its own inputs
 *
 * This probe reads the two halves through two DIFFERENT resolution paths, and
 * both of them are correct:
 *
 *  - the tool CATALOG by RELATIVE path (`../packages/piing/src/...`), because
 *    the subject is the tree this checkout has;
 *  - the EXTENSION by PACKAGE NAME, because that is how `apps/web` really
 *    imports it in production and the whole point is to measure the real host.
 *
 * The asymmetry is the measurement, not a defect, and neither side may be
 * "unified" away. Pointing `apps/web` at a relative path would be a package
 * reaching across the filesystem at a sibling; making this file read the
 * catalog by package name would be worse still, because both halves would then
 * come consistently from whatever tree `node_modules` happens to point at —
 * the guard would pass or fail on somebody else's uncommitted edits while
 * claiming to describe this commit.
 *
 * What CAN go wrong is that the two paths land in two different checkouts. A
 * worktree that symlinks `node_modules` from a shared tree resolves `@chief/*`
 * into that tree's `packages/`, which is another agent's working copy at
 * another commit. The guard then compares a NEW catalog against an OLD
 * extension and reports the delta as a product fault: measured, it accused a
 * shipped and working `org_stand_down`/`org_resume` of being granted but
 * unbuildable, which is the most alarming thing this file can say. The same
 * commit passed in CI, where a real install makes both halves one tree.
 *
 * So the probe reports where each half came from and the guard refuses on a
 * mismatch. A check that can be lied to should be able to say it was.
 */
function resolvedFrom(): { catalog: string; extension: string } {
  // The parent argument is the entire point: resolution must start from
  // `apps/web`, which has a `node_modules` of its own, and not from this
  // script's directory. `import.meta.resolve` is called with the same
  // specifier `ExtensionTools.ts` uses, from that file's own URL, so this is
  // the host's real resolution rather than a re-implementation of it.
  const resolve = import.meta.resolve as (specifier: string, parent: string) => string
  const host = new URL('../apps/web/src/server/ExtensionTools.ts', import.meta.url).href
  const extension = resolve('@chief/piing/extensions/organization-intercom', host)
  return {
    // `packages/piing`, reached each way. Compared at the PACKAGE root rather
    // than at a file, because every file under it moves together.
    catalog: realpathSync(
      fileURLToPath(new URL('../packages/piing', import.meta.url))
    ),
    extension: realpathSync(dirname(dirname(fileURLToPath(extension))))
  }
}

const SLUG = 'toolsurface'
const PERSON = 'ceo'
const ROOT_DEPARTMENT = 'company'

/** Pi's own coding tools, by the ids chiefd grants them under.
 *
 * These are the seven `selectTools` can build today. They are named here
 * because they are the DECLARED half of the measurement — the person record's
 * own `tools` grant, which chiefd copies onto the launch profile ahead of
 * everything the extensions contribute. What the host does with them is
 * measured, never assumed. */
const CODING_TOOLS = ['read', 'bash', 'edit', 'write', 'grep', 'find', 'ls'] as const

/** One tool definition as the extension registered it. */
interface RegisteredTool {
  name: string
  parameters?: unknown
}

/** A synthetic company with exactly one person: a structural-root executive.
 *
 * Executive because that is the person the 60-tool launch profile belongs to —
 * a worker's profile is a different, much smaller measurement, and measuring
 * that one instead is precisely how an instrument stops seeing its subject. */
function manifest(): IntercomOrganizationManifest {
  return {
    schemaVersion: 2,
    kind: 'organization',
    slug: SLUG,
    name: 'Tool Surface',
    rootDepartmentId: ROOT_DEPARTMENT,
    departmentOrder: [ROOT_DEPARTMENT],
    peopleOrder: [PERSON],
    departments: {
      [ROOT_DEPARTMENT]: {
        id: ROOT_DEPARTMENT,
        name: 'Company',
        purpose: 'the structural root',
        kind: 'department',
        headPersonId: PERSON,
        state: 'active'
      }
    },
    people: {
      [PERSON]: {
        id: PERSON,
        name: 'Chief',
        title: 'CEO',
        kind: 'executive',
        departmentId: ROOT_DEPARTMENT,
        employmentState: 'active',
        provider: 'openrouter',
        model: 'probe-model',
        taskClass: 'executive',
        createdAt: '2026-01-01T00:00:00.000Z'
      }
    }
  } as IntercomOrganizationManifest
}

/**
 * This company's chiefd, on one loopback port.
 *
 * The extension learns where its daemon is from `<dir>/.chief/run/daemon.json`
 * — no registry is involved — and then reads the normalized manifest from that
 * daemon. Every other route answers an empty object: the install path tolerates
 * a store that tells it nothing, and a probe that pretended to be a whole
 * daemon would be a second implementation of chiefd rather than an instrument.
 */
async function stubDaemon(): Promise<{ url: string; close: () => void }> {
  const server = createServer((request, response) => {
    const path = request.url ?? '/'
    const send = (body: unknown): void => {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify(body))
    }
    // Drain the body before answering; an unread request body leaves the
    // socket half-open and the next call on the same connection hangs.
    request.resume()
    request.on('end', () => {
      if (path === '/v1/org/manifest/read') {
        send({ found: true, manifest: JSON.stringify(manifest()), seq: 1 })
        return
      }
      // The supervision ledger. The probe serves an empty one: the extensions
      // read it at install, so a daemon that answered nothing here would be a
      // daemon no real company has.
      if (path === '/v1/org/supervision/read') {
        send({
          found: true,
          seq: 1,
          ledger: JSON.stringify({ schemaVersion: 1, organization: SLUG })
        })
        return
      }
      send({})
    })
  })
  await new Promise<void>((resolve) => {
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address() as AddressInfo
  return { url: `http://127.0.0.1:${address.port}`, close: () => server.close() }
}

/** The environment one person's extensions install against.
 *
 * Lifted out of `registeredTools` because `selectTools` installs against it
 * too: the probe reports `registered` and `built` for ONE person, and two
 * environments would be two people whose numbers happen to sit side by side.
 *
 * THE SUBJECT IS A WEB-HOSTED PERSON, so this carries exactly what chiefd's
 * API-host profile carries and nothing else (`api_host_environment`,
 * `chiefd-host/src/converge_apply/api_host_profile.rs`). It once carried a
 * tmux pane's identity instead, so the probe measured a pane and could not see
 * the tools a genuinely web-hosted CEO was missing. An instrument that cannot
 * see its subject is the recurring defect this whole guard exists to answer,
 * and it had it. */
function probeEnvironment(companyDir: string): Record<string, string | undefined> {
  return {
    ORG_LAUNCHER_IDENTITY_DIR: join(companyDir, '.chief'),
    ORG_LAUNCHER_ORG_DIR: companyDir,
    ORG_LAUNCHER_ORGANIZATION: SLUG,
    ORG_LAUNCHER_PERSON: PERSON,
    ORG_LAUNCHER_ROOT: companyDir
  }
}

/**
 * Publish the daemon rendezvous into the company directory, exactly as
 * `chiefd` writes it: `<dir>/.chief/run/daemon.json`, camelCase, four fields
 * and no others. This is how the extension finds its own daemon — one local
 * read, no registry on the path — so the probe has to write it or the install
 * refuses with `OrgChiefdUrlUnsetError`.
 */
function publishRendezvous(companyDir: string, url: string): void {
  mkdirSync(join(companyDir, '.chief', 'run'), { recursive: true })
  const key = createHash('sha256').update(companyDir).digest('hex').slice(0, 12)
  writeFileSync(
    join(companyDir, '.chief', 'run', 'daemon.json'),
    JSON.stringify({ dir: companyDir, key, url, pid: process.pid })
  )
}

/** Install every extension whose tools this person is granted, and hand back
 * every definition they registered — by enumeration, in registration order. */
async function registeredTools(companyDir: string): Promise<RegisteredTool[]> {
  const tools: RegisteredTool[] = []
  const pi = {
    registerTool(definition: RegisteredTool) {
      tools.push(definition)
    },
    registerMessageRenderer() {},
    registerEntryRenderer() {},
    appendEntry() {},
    on() {},
    sendMessage() {}
  } as never

  const environment = probeEnvironment(companyDir)

  await installOrganizationIntercom(pi, {
    environment,
    // Every background schedule off: this probe observes a REGISTRATION, and a
    // poll landing inside it would only add nondeterminism. `pollIntervalMs: 0`
    // is the one that carries that claim — its own docstring says 0 disables
    // ALL background activity, because no `SseWatcher` reader is constructed —
    // and the watchdog interval is named as well because 0 disables it
    // independently.
    //
    // This literal also carried `supervisionIntervalMs`, `modelObservationRegistry`
    // and `runner`, none of which are members of `InstallOrganizationIntercomOptions`.
    // They were silently ignored for as long as they sat here, which is exactly
    // what a directory in no typechecked project buys you: a probe whose own
    // comment described a configuration it was not applying.
    pollIntervalMs: 0,
    turnWatchdogIntervalMs: 0,
    bootTransientRetryDelaysMs: [],
    clock: () => Date.parse('2026-01-01T00:00:00.000Z')
  })
  return tools
}

async function main(): Promise<void> {
  const daemon = await stubDaemon()
  const companyDir = mkdtempSync(join(tmpdir(), 'tool-surface-artifact-'))
  publishRendezvous(companyDir, daemon.url)
  const registered = await registeredTools(companyDir)

  // The grant chiefd puts on a converged CEO's launch profile, in chiefd's own
  // construction order: the person record's declared tools, then the intercom
  // baseline, the subtree set every person carries, the role-gated manager
  // set, and the root-executive tool.
  const grant = [
    ...CODING_TOOLS,
    ...ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES,
    ...ORGANIZATION_SUBTREE_TOOL_NAMES,
    ...ORGANIZATION_MANAGER_TOOL_NAMES,
    ...ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES
  ]

  // The REAL host selection — the same call whose `unavailable` becomes the
  // roster's `degraded[].missingTools`. Nothing here recomputes that fact.
  //
  // It takes a PROFILE now, not a cwd and a list, because the `org_*` family
  // is no longer something the host constructs: `selectTools` installs the
  // real extensions against the person's own environment and adapts whatever
  // they register. The environment handed over is the same one
  // `registeredTools` above installed with, so `built` and `registered`
  // describe one person rather than two.
  const profile: AgentProfile = {
    personId: PERSON,
    cwd: mkdtempSync(join(tmpdir(), 'tool-surface-cwd-')),
    env: Object.fromEntries(
      Object.entries(probeEnvironment(companyDir)).filter(
        (entry): entry is [string, string] => entry[1] !== undefined
      )
    ),
    // No `provider`/`model` here: `AgentProfile` has never carried either, and
    // the keys that used to sit at this spot were leftovers of the
    // provider/model management this product deleted (an agent's reasoning
    // effort is Pi's own setting now). They typechecked nowhere and selected
    // nothing.
    tools: grant,
    displayName: `${SLUG} · CEO`
  }
  const selection = await selectTools(profile)
  // Closed only now: `selectTools` performs its OWN install, and the intercom's
  // first act is to read the company manifest from this daemon.
  daemon.close()

  // Every tool object that exists in this process: what the extensions
  // registered, plus what the host built. Serialized exactly the way a
  // provider payload is built, so a schema that cannot survive JSON is
  // reported as the empty thing it becomes.
  const schemas = [
    ...registered.map((tool) => ({ name: tool.name, source: 'extension', parameters: tool.parameters })),
    ...selection.tools.map((tool) => ({ name: tool.name, source: 'host', parameters: tool.parameters }))
  ].map((entry) => ({
    name: entry.name,
    source: entry.source,
    schema: JSON.parse(JSON.stringify(entry.parameters ?? null)) as unknown
  }))

  const document =
    JSON.stringify(
      {
        grant,
        resolvedFrom: resolvedFrom(),
        built: selection.tools.map((tool) => tool.name),
        missing: selection.unavailable,
        registered: registered.map((tool) => tool.name),
        schemas
      },
      undefined,
      2
    ) + '\n'
  // AWAITED, and that is load-bearing. `process.exit` does not flush a pipe:
  // whatever is still in the stdout buffer is discarded. This document was
  // ~7 host tools long when it was written and is now ~59, which pushed it
  // past the pipe buffer — the guard read a truncated JSON document and
  // reported `Unterminated string at position 146156`, which reads like a
  // corrupt probe rather than a lost write. `write`'s callback fires when the
  // bytes are gone.
  await new Promise<void>((resolve) => process.stdout.write(document, () => resolve()))
  // The extensions leave live handles behind (an SSE reader, a token manager).
  // The probe's whole product is the document above; exiting on it is honest
  // and keeps the guard bounded.
  process.exit(0)
}

await main()
