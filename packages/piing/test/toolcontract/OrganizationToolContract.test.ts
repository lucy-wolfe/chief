/**
 * THE TOOL CONTRACT SUITE (#751/P4).
 *
 * Nothing else in CI calls a tool. Every other suite drives a pure unit or an
 * HTTP route, and on 2026-08-09 that gap shipped three broken packets in one
 * day — each one proved its route returned 200 and each one was wrong, because
 * an org tool does more than call its route:
 *
 *     tool.execute()  ->  POST the route  ->  reconcile the runtime  ->  classify
 *
 * Both defects lived AFTER the 200:
 *
 *   1. `org_launch_department` created the department, then failed with
 *      `chiefd: unknown command 'org'` because `reconcileRuntime` still spawned
 *      a deleted TypeScript CLI (fixed in d2b235c90).
 *   2. The staffing-lifecycle verbs — `org_offboard` among them — committed the
 *      whole atomic mutation, then threw `returned an invalid outcome` at the
 *      manager, because `/v1/org/staffing/lifecycle` answered
 *      `{"status":"applied"}` with no `applied` key (fixed in abfaf6d11).
 *
 * In both cases the change happened and the agent was told it did not. This
 * suite is red against the commit before either fix, and green after.
 *
 * # Three things this suite refuses to do
 *
 * **It never ends in an HTTP POST.** Assertions are made on what
 * `tool.execute` resolved, and read back through `/v1/org/tree/structured` —
 * the COMPOSITE-keyed route that names people. `tree/read` is a summary that
 * does not name people and looks right when it is wrong.
 *
 * **It never injects a `LauncherRunner`.** The scripted runner every previous
 * tool harness used is exactly what hid defect 1: it answered where the real
 * one spawns a CLI that no longer exists.
 *
 * **It never skips.** The nine `packages/chiefing/test/contract` suites gate
 * on `chiefdBinaryTestGate()` and skip when the debug test binary is absent;
 * that convention is why they sat silently unrun and why
 * `ContractSuiteResidual` had to be invented to name them. This suite hard
 * fails instead. A missing binary is a machine that has not built the product,
 * not a reason to stop checking it.
 */
import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import type { Server } from 'node:http'
import { createServer } from 'node:http'
import { join } from 'node:path'

import {
  AgentTokenManager,
  FetchTransport,
  generateAgentKeypair,
  IDENTITY_KEY_FILENAME,
  readAgentKeypair
} from '@chief/chiefing'
import type { TmuxHostedCompany } from '@chief/testing'
import {
  acquireOperatorBearer,
  assertChiefdBinaryBuilt,
  startTmuxHostedCompany,
  surfaceDaemonLogOnFailure
} from '@chief/testing'
import { isNullish } from '@test/support/Nullish'
import { installOrganizationToolSurface } from '@test/support/OrganizationToolSurface'
import type { OrganizationToolSurface, ToolCallOutcome } from '@test/types/OrganizationToolSurface'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

const REPO_ROOT = join(import.meta.dirname, '..', '..', '..', '..')
const SLUG = 'toolcontract'
/** One boot for the whole suite: a per-test daemon would triple the cost for
 *  isolation this suite does not need. */
const BOOT_TIMEOUT_MS = 120_000
const TOOL_TIMEOUT_MS = 90_000

let company: TmuxHostedCompany
let surface: OrganizationToolSurface

/**
 * A full `chiefd run` actuates tmux, so tmux must exist. Loud and specific,
 * never a skip: a machine without tmux cannot run the product either.
 */
function assertTmuxAvailable(): void {
  try {
    execFileSync('tmux', ['-V'], { stdio: 'ignore' })
  } catch {
    throw new Error(
      'the organization tool contract suite needs tmux: a full `chiefd run` mounts ' +
        'the tmux host capability that `/v1/org/runtime/launch` requires, and every ' +
        'org tool calls that route after its durable write commits. Install tmux.'
    )
  }
}

/**
 * THE READ-BACK's OWN CREDENTIAL: this daemon's OPERATOR bearer.
 *
 * Every `/v1/org/*` route authenticates now, so a read-back with no
 * `Authorization` header is a 401 before any handler runs — it can no longer
 * prove anything about the state a tool wrote. The operator is the right
 * principal for it and not merely the available one: it is a NON-PERSON
 * principal, so it keeps unconditional scope and no disclosure fence narrows
 * what the read-back is allowed to see. A person's bearer would silently
 * scope every read to that person's subtree, and this file reads the WHOLE
 * tree back to check what a tool did.
 *
 * Cached per daemon URL. The handshake is three round trips and this suite
 * reads back after nearly every tool call; the token outlives the daemon,
 * which lives and dies with the file.
 *
 * The key it signs with is the COMPANY's own: `chiefd run` mints
 * `<dir>/.chief/keys/operator.key` inside the directory it was told to serve.
 */
const operatorBearers = new Map<string, Promise<string>>()

async function operatorHeadersFor(target: TmuxHostedCompany): Promise<Record<string, string>> {
  const cached =
    operatorBearers.get(target.url) ??
    acquireOperatorBearer({ url: target.url, keysRoot: join(target.dir, '.chief') })
  operatorBearers.set(target.url, cached)
  return { authorization: `Bearer ${await cached}` }
}

/**
 * POST to ONE named company's daemon, with that company's COMPOSITE key.
 *
 * Takes the company explicitly rather than closing over the suite's, because
 * the two-company block below reads two daemons back and the whole property it
 * asserts is that those two answers differ. A read-back helper that could only
 * ever reach one of them would make the negative half unwritable.
 */
async function postTo(
  target: TmuxHostedCompany,
  path: string,
  body: Record<string, unknown>
): Promise<unknown> {
  const authorization = await operatorHeadersFor(target)
  /* eslint-disable lucy/no-json-stringify */
  // The read-back path deliberately does not use `@chief/chiefing`'s client:
  // this suite must be able to disagree with the client the tool uses.
  const response = await fetch(`${target.url}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authorization },
    body: JSON.stringify({ slug: target.companyKey, ...body })
  })
  /* eslint-enable lucy/no-json-stringify */
  const text = await response.text()
  if (!response.ok) throw new Error(`${path} answered ${response.status}: ${text.slice(0, 400)}`)
  return JSON.parse(text)
}

async function post(path: string, body: Record<string, unknown>): Promise<unknown> {
  return postTo(company, path, body)
}

/** The pid the seeded observation gives its first person. Arbitrary, but a
 *  REAL pid shape — a decimal string, never a `%N` tmux pane id. */
const SEEDED_PID = '48213'

/**
 * MAKE THE `org_roster` READS NON-VACUOUS.
 *
 * This suite drives all 47 org tools against a real daemon and it did not
 * catch the outage that broke `org_roster` company-wide. The reason is here:
 * nothing in this suite ever actuates a person, so no actuator ever commits a
 * runtime observation, so the durable `runtime` row does not exist, so
 * `loadOrganizationRosterObservation` returned `absent` from its first branch
 * and the entire runtime projection — every validator that rejected chiefd's
 * real payload — was unreachable. Two assertions that read `expect(roster.ok)`
 * were passing on a code path the product never takes in production.
 *
 * So: commit a trusted actuator report AND publish the runtime row it implies,
 * then read. Both writes, because either alone leaves a race — the report
 * makes any later converge pass republish the SAME people, and the row makes
 * the fact readable now without waiting for one.
 *
 * The shape is chiefd's own, copied from `converge_apply/cycle.rs`:
 * `processHandles` is person -> pid, and the EMPTY STRING for a person the
 * actuator proved alive without a readable pid. `windows` does not exist.
 * Anything that changes that shape without moving the reader now turns this
 * suite red, which is the whole point of writing it down here.
 *
 * Hands back the people it named, so the caller can assert on them by id.
 */
async function seedRuntimeObservation(): Promise<{
  withPid: string
  withoutPid: readonly string[]
}> {
  const wire = await post('/v1/org/activity/read', {})
  if (!isRecord(wire) || wire.found !== true || typeof wire.ledger !== 'string') {
    throw new Error('the company has no activity ledger to derive desired-active people from')
  }
  const ledger: unknown = JSON.parse(wire.ledger)
  if (!isRecord(ledger) || !Array.isArray(ledger.personOrder) || !isRecord(ledger.people)) {
    throw new Error('the activity ledger names no people to derive desired-active from')
  }
  const people = ledger.people
  const desired = ledger.personOrder
    .filter((id): id is string => typeof id === 'string')
    .filter((id) => {
      const state = people[id]
      return isRecord(state) && state.lastDesiredActive === true
    })
  // NON-VACUITY, ASSERTED. With nobody desired-active there is no running
  // person to project and this helper would seed the empty case — the exact
  // shape of vacuity it exists to remove.
  expect(
    desired.length,
    'no person is desired-active, so a seeded runtime observation would be empty and the roster read vacuous again'
  ).toBeGreaterThan(0)
  const [withPid, ...withoutPid] = desired
  if (isNullish(withPid)) throw new Error('unreachable: desired-active is non-empty')

  // TOMBSTONE: the `/v1/org/runtime/observed` seed that used to stand here.
  // The route is deleted along with the whole upward direction — the actuator
  // never tells chiefd what it saw in tmux. The pids this helper's callers
  // need are seeded by the `/v1/org/runtime/publish` write below, which is a
  // different mechanism (the daemon's own runtime row) and is untouched.
  /* eslint-disable lucy/no-json-stringify */
  // `{slug, doc: JSON.stringify(doc)}` is this route family's verbatim wire
  // (see `RowStores.publishDoc`); serializing it here keeps the suite able to
  // disagree with the client about the bytes.
  await post('/v1/org/runtime/publish', {
    doc: JSON.stringify({
      version: 1,
      observedAt: new Date().toISOString(),
      socketName: company.tmuxSocket,
      status: 'running',
      processHandles: Object.fromEntries(
        desired.map((personId, index) => [personId, index === 0 ? SEEDED_PID : ''])
      )
    })
  })
  /* eslint-enable lucy/no-json-stringify */
  return { withPid, withoutPid }
}

/**
 * Read `org_roster` through the tool and assert it reached the runtime
 * projection rather than short-circuiting on an absent row.
 *
 * `runtimeStatus === 'absent'` is the vacuity signature: it is what every
 * `org_roster` call in this suite answered while the tool was broken in
 * production.
 */
async function nonVacuousRoster(from: OrganizationToolSurface): Promise<ToolCallOutcome> {
  const seeded = await seedRuntimeObservation()
  const roster = await from.call('org_roster', {})
  expect(roster.ok, `org_roster failed: ${roster.message}`).toBe(true)
  // THE ANTI-VACUITY ASSERTION. Before the seed above, every roster read in
  // this suite answered `absent` and validated nothing.
  expect(
    roster.details.runtimeStatus,
    'the roster read never reached the runtime projection, so it proves nothing about it'
  ).not.toBe('absent')
  // And the two renderings of a process chiefd can produce, both of which the
  // old tmux-shaped reader rejected outright.
  expect(roster.message, 'the person with a pid must be named as running').toContain(
    `pid ${SEEDED_PID}`
  )
  if (seeded.withoutPid.length) {
    expect(
      roster.message,
      'a person the actuator proved alive without a readable pid is still running'
    ).toContain('pid unknown')
  }
  // The retired vocabulary must not come back: chiefd has no pane and no
  // window to name.
  expect(roster.message).not.toMatch(/\bpane\b/)
  expect(roster.message).not.toMatch(/\bwindow\b/)
  // And not one level up either. The rendered copy was only ever half of it:
  // the map the roster reads was itself CALLED `panes` while holding pids, and
  // that name is what made a reader validate it as a tmux id and refuse every
  // real payload. A clean message over a lying wire key buys nothing, so the
  // WIRE is asserted here too, on the row chiefd actually serves.
  const runtimeRow = await post('/v1/org/runtime/read', {})
  expect(
    isRecord(runtimeRow) && runtimeRow.found === true && typeof runtimeRow.doc === 'string',
    'the seeded runtime row must be readable, or the key assertions below prove nothing'
  ).toBe(true)
  const parsedRuntime: unknown = JSON.parse(
    isRecord(runtimeRow) && typeof runtimeRow.doc === 'string' ? runtimeRow.doc : '{}'
  )
  const runtimeDoc = isRecord(parsedRuntime) ? parsedRuntime : {}
  expect(
    Object.keys(runtimeDoc).filter((key) => /pane|window/i.test(key)),
    'the runtime row must not name a pane or a window: it holds process handles'
  ).toEqual([])
  expect(
    Object.hasOwn(runtimeDoc, 'processHandles'),
    'the runtime row must carry the process-handle map under its real name'
  ).toBe(true)
  return roster
}

/** Birth one company on its own daemon: a CEO and nothing else. Genesis names
 *  no route — every agent boots as plain Pi on the operator's own defaults. */
async function genesis(target: TmuxHostedCompany): Promise<void> {
  await postTo(target, '/v1/org/manifest/genesis', {
    at: new Date().toISOString(),
    spec: {
      name: target.slug,
      purpose: 'the company the organization tool contract suite drives',
      chief: { name: 'Chief' }
    }
  })
}

/**
 * Install the CEO's tool surface for one live company, once that CEO can
 * authenticate.
 *
 * The wait is not politeness, it is the install's PRECONDITION. Installing
 * reads the manifest immediately (`installOrganizationIntercom` →
 * `loadIntercomOrganization` → `POST /v1/org/manifest/read`) and it reads it
 * through the pane transport, which carries a credential only once
 * materialization has minted this person's identity key. Ask before the key
 * exists and `paneChiefdTransport` hands back its documented credential-free
 * fallback — invisible while the universal auth gate is off, and a flat
 * `missing bearer token` 401 the moment it is on.
 *
 * Genesis now creates the Chief's company identity and enrols it before the
 * request returns. The direct check below pins both halves of that synchronous
 * contract. There is no Chief agent home to wait for.
 */
async function chiefSurfaceFor(target: TmuxHostedCompany): Promise<OrganizationToolSurface> {
  await assertChiefIdentity(target)
  return installOrganizationToolSurface({
    chiefdUrl: target.url,
    organization: target.slug,
    organizationDir: target.dir,
    personId: 'chief',
    launcherRoot: REPO_ROOT,
    tmuxSocket: target.tmuxSocket,
    tmuxSession: target.slug
  })
}

/**
 * Assert the Chief identity that genesis publishes on ONE named company.
 *
 * Both halves matter. The direct file read says the key was minted at the
 * Chief-only location. The challenge says it was enrolled. Genesis completed
 * before this function runs, so a poll would hide a broken synchronous
 * boundary.
 *
 * Named, never implied: this suite boots a second company for the
 * two-companies family, and a wait that always looked at the module-level
 * `company` would have watched the wrong daemon.
 */
async function assertChiefIdentity(target: TmuxHostedCompany = company): Promise<void> {
  const identityDir = join(target.dir, '.chief')
  if (isNullish(readAgentKeypair(identityDir).keypair)) {
    throw new Error(`genesis returned without minting '${target.slug}' Chief identity key`)
  }
  const challenge = await postStatus('/v1/auth/challenge', { identityId: 'chief' }, target)
  if (challenge.status !== 200) {
    throw new Error(
      `genesis returned without enrolling '${target.slug}' Chief identity; ` +
        `challenge answered ${challenge.status}`
    )
  }
}

/**
 * One live person's OWN tool surface on the suite's company.
 *
 * Every authority assertion below needs two callers, because the allowed
 * direction alone stays green forever once a gate is deleted. Each surface
 * reads the identity key materialization minted for its own person, so
 * "the worker is refused" is proven the way production proves it — by the
 * credential on the wire, not by a field the fixture chose.
 *
 */
async function surfaceFor(personId: string): Promise<OrganizationToolSurface> {
  return installOrganizationToolSurface({
    chiefdUrl: company.url,
    organization: SLUG,
    organizationDir: company.dir,
    personId,
    launcherRoot: REPO_ROOT,
    tmuxSocket: company.tmuxSocket,
    tmuxSession: SLUG
  })
}

/**
 * A bearer minted the way a pane mints one: the person's own enrolled identity
 * key signs a daemon-issued challenge (#751/P7).
 *
 * The reminder routes resolve their acting person from that credential, so a
 * read-back that reaches one has to carry it. The TOKEN is acquired through
 * `@chief/chiefing` because the challenge/sign/mint handshake is not something
 * a test should re-implement; the request itself is still the raw `fetch`
 * below, so this suite keeps its ability to disagree with the client the tool
 * uses about paths, bodies and keys.
 */
async function bearerFor(personId: string): Promise<Record<string, string>> {
  const identityDir =
    personId === 'chief'
      ? join(company.dir, '.chief')
      : join(company.dir, '.chief', 'agent', personId)
  const { keypair } = readAgentKeypair(identityDir)
  if (isNullish(keypair)) throw new Error(`'${personId}' has no enrolled identity key`)
  const manager = new AgentTokenManager(
    new FetchTransport(company.url),
    personId,
    keypair.privatePkcs8Pem
  )
  const header = await manager.authHeader()
  if (isNullish(header)) throw new Error(`could not mint a bearer for '${personId}'`)
  return header
}

/** As `post`, presenting `personId`'s own credential. */
async function postAs(
  personId: string,
  path: string,
  body: Record<string, unknown>
): Promise<unknown> {
  const authorization = await bearerFor(personId)
  /* eslint-disable lucy/no-json-stringify */
  // The read-back's bytes are the subject here exactly as they are in `post`:
  // the composite company key has to be the one that goes on the wire.
  const response = await fetch(`${company.url}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authorization },
    body: JSON.stringify({ slug: company.companyKey, ...body })
  })
  /* eslint-enable lucy/no-json-stringify */
  const text = await response.text()
  if (!response.ok) throw new Error(`${path} answered ${response.status}: ${text.slice(0, 400)}`)
  return JSON.parse(text)
}

/**
 * POST a body VERBATIM and hand back the status.
 *
 * `post` always injects the composite key and throws on a non-2xx, both of
 * which are the subject in the refusal assertions below rather than plumbing.
 *
 * The CREDENTIAL is a parameter for the same reason the body is verbatim.
 * Some callers here probe what an ANONYMOUS request answers, which is a real
 * assertion now that every `/v1/org/*` route refuses one; others probe a
 * BODY-level refusal and must therefore get past authentication first, or
 * every one of them would read 401 and prove nothing about the body. A helper
 * that picked for them could only express one of the two.
 */
async function postStatus(
  path: string,
  body: Record<string, unknown>,
  target: TmuxHostedCompany = company,
  authorization: Record<string, string> = {}
): Promise<{ status: number }> {
  /* eslint-disable lucy/no-json-stringify */
  // The refusal assertions are about the exact bytes on the wire, so the body
  // is serialized here rather than routed through a helper that would decide
  // any part of it.
  const response = await fetch(`${target.url}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authorization },
    body: JSON.stringify(body)
  })
  /* eslint-enable lucy/no-json-stringify */
  await response.text()
  return { status: response.status }
}

// TOMBSTONE: `maintenanceRequestsFor` and `maintenanceLedgerIsReadable`, the
// ledger projection and its readability floor. They existed for the two
// maintenance tests above, both of which are gone — one with the tool it drove,
// one recorded as a coverage loss. The floor is worth remembering if either
// ever returns: `requests` is a MAP keyed by request id, not a list, so reading
// it as a list yields an empty array for every input and makes a "nothing here
// yet" assertion pass for the wrong reason.

/** One journal line, or an empty record when it is not JSON at all. */
function parsedJournalLine(line: string): unknown {
  try {
    return JSON.parse(line)
  } catch {
    return {}
  }
}

/** Every line the extension has journaled to `.chief/bus/events.jsonl`, oldest first.
 *  A deferral in here is the extension telling itself a durable call failed. */
function journalEvents(): Record<string, unknown>[] {
  let raw: string
  try {
    raw = readFileSync(join(company.dir, '.chief', 'bus', 'events.jsonl'), 'utf8')
  } catch {
    return []
  }
  return raw
    .split('\n')
    .filter((line) => line.trim().length > 0)
    .map((line) => parsedJournalLine(line))
    .filter(isRecord)
}

/** The armed/stopped reminder id out of a tool result, without a type
 * assertion: the suite must be able to disagree with the shape the tool
 * claims, which is the whole point of reading `details` defensively. */
function reminderIdOf(details: unknown): string | undefined {
  if (!isRecord(details)) return undefined
  const reminder = details.reminder
  if (!isRecord(reminder)) return undefined
  return typeof reminder.id === 'string' ? reminder.id : undefined
}

/** The prompts of the reminders a list tool returned. A refused arm must not
 *  appear here — the write is the thing the gate has to prevent, not the 403. */
function reminderPromptsOf(details: unknown): string[] {
  if (!isRecord(details) || !Array.isArray(details.reminders)) return []
  return details.reminders
    .filter(isRecord)
    .map((row) => row.prompt)
    .filter((prompt): prompt is string => typeof prompt === 'string')
}

/** The reminder ids chiefd holds for `personId` under a given company key. */
function reminderIdsOf(wire: unknown): string[] {
  if (!isRecord(wire) || !Array.isArray(wire.reminders)) return []
  return wire.reminders
    .filter(isRecord)
    .map((row) => row.id)
    .filter((id): id is string => typeof id === 'string')
}

/**
 * The DURABLE MANIFEST, read straight off the daemon.
 *
 * The companion to `/v1/org/tree/structured` for the two facts the structured
 * tree deliberately does not carry: a person's `employmentState` (a bench is
 * invisible in a tree that lists whoever is placed somewhere) and a unit's
 * `kind` (a contract renders as a department node, because placement is all a
 * browser needs). Both are the whole product of a tool in this file, so both
 * have to be read from the authority that holds them.
 */
async function readManifest(): Promise<Record<string, unknown>> {
  const wire = await post('/v1/org/manifest/read', {})
  const body = isRecord(wire) && typeof wire.manifest === 'string' ? wire.manifest : ''
  expect(
    body,
    'the manifest read must produce a document — an unreadable one makes every ' +
      '"nothing was written" assertion below pass for the wrong reason'
  ).not.toBe('')
  const parsed: unknown = JSON.parse(body)
  return isRecord(parsed) ? parsed : {}
}

/** One person's durable staffing record, or undefined when the company has no
 *  such person at all. */
function manifestPerson(
  manifest: Record<string, unknown>,
  personId: string
): { employmentState: string; departmentId: string } | undefined {
  const people = isRecord(manifest.people) ? manifest.people : {}
  const person = people[personId]
  if (!isRecord(person)) return undefined
  return {
    employmentState: asText(person.employmentState),
    departmentId: asText(person.departmentId)
  }
}

/** The `kind` the manifest records for one unit (`department` | `contract`),
 *  or undefined when the unit is not there. */
function manifestUnitKind(manifest: Record<string, unknown>, unitId: string): string | undefined {
  const departments = isRecord(manifest.departments) ? manifest.departments : {}
  const unit = departments[unitId]
  if (!isRecord(unit)) return undefined
  // Absent `kind` is the schema-v1 shape and resolves to a department.
  return typeof unit.kind === 'string' ? unit.kind : 'department'
}

/** The engagement a contract was launched with, off the durable record — the
 *  one field that makes a contract a contract rather than a department. */
function manifestUnitEngagement(
  manifest: Record<string, unknown>,
  unitId: string
): string | undefined {
  const departments = isRecord(manifest.departments) ? manifest.departments : {}
  const unit = departments[unitId]
  if (!isRecord(unit) || !isRecord(unit.transient)) return undefined
  return asText(unit.transient.engagement)
}

/** The messages chiefd holds for one person, narrowed to sender and body. */
async function mailboxOf(personId: string): Promise<{ from: string; body: string }[]> {
  const wire = await post('/v1/org/mailbox/read-person', { personId })
  const body = isRecord(wire) && typeof wire.mailbox === 'string' ? wire.mailbox : ''
  expect(
    body,
    `the mailbox read for '${personId}' must produce a document — an unreadable ` +
      'one makes every "no message was queued" assertion vacuous'
  ).not.toBe('')
  const parsed: unknown = JSON.parse(body)
  const entries = isRecord(parsed) && Array.isArray(parsed.entries) ? parsed.entries : []
  return entries
    .filter(isRecord)
    .map((entry) => ({ from: asText(entry.fromPersonId), body: asText(entry.body) }))
}

/**
 * The operator-escalation queue chiefd holds for the company.
 *
 * The escalation family's durable artifact is an INTENT row, not the
 * escalation log: the log is written by the protected supervision loop that
 * drains this queue, and this fixture runs with every background timer at 0 so
 * that loop never fires. Reading the log would therefore assert an empty
 * surface forever, which is the "looks right when it is wrong" failure this
 * suite exists to remove.
 */
async function escalationIntents(): Promise<
  { fingerprint: string; personId: string; blocker: string; operatorAction: string }[]
> {
  const wire = await post('/v1/org/operator-escalation-intents/read', {})
  const found = isRecord(wire) && wire.found === true
  const body = found && typeof wire.doc === 'string' ? wire.doc : ''
  if (!found) return []
  const parsed: unknown = JSON.parse(body)
  const intents = isRecord(parsed) && isRecord(parsed.intents) ? parsed.intents : {}
  return Object.values(intents)
    .filter(isRecord)
    .map((intent) => ({
      fingerprint: asText(intent.fingerprint),
      personId: asText(intent.personId),
      blocker: asText(intent.blocker),
      operatorAction: asText(intent.operatorAction)
    }))
}

/**
 * chiefd's own durable answer to "should this person be running", read off the
 * activity ledger.
 *
 * `lastDesiredActive` is the projection a later staffing verb is fenced
 * against, so it is the fact `org_start_person` and `org_stop_person` actually
 * own. A pane is NOT that fact and must not stand in for it here: this box has
 * no provider credential, so a real `pi` exits on its own within seconds and
 * chiefd correctly reaps the pane — an assertion on pane presence would be
 * measuring the credential, not the tool. (`tmuxPaneOwners` below asserts the
 * one pane claim that IS stable: nobody chiefd has not authorized owns one.)
 */
async function desiredActive(personId: string): Promise<boolean | undefined> {
  const wire = await post('/v1/org/activity/read', {})
  const body = isRecord(wire) && typeof wire.ledger === 'string' ? wire.ledger : ''
  expect(
    body,
    'the activity ledger read must produce a document — an unreadable one makes ' +
      'every placement assertion below pass for the wrong reason'
  ).not.toBe('')
  const parsed: unknown = JSON.parse(body)
  const people = isRecord(parsed) && isRecord(parsed.people) ? parsed.people : {}
  const person = people[personId]
  if (!isRecord(person)) return undefined
  return typeof person.lastDesiredActive === 'boolean' ? person.lastDesiredActive : undefined
}

/**
 * The person ids TMUX ITSELF reports owning a pane on this company's private
 * socket.
 *
 * `@organization_person_id` is the ownership tag chiefd writes and reads
 * (`tmux/trust.rs`); a `pane_title` carries a rendered display name, so
 * matching on one never matches a person id and would report "no pane" while
 * the pane was right there. Read as an exact field, never a substring of the
 * row.
 */
function tmuxPaneOwners(): string[] {
  try {
    const listed = execFileSync(
      'tmux',
      [
        '-L',
        company.tmuxSocket,
        'list-panes',
        '-t',
        SLUG,
        '-a',
        '-F',
        '#{@organization_person_id}'
      ],
      { encoding: 'utf8' }
    )
    return listed
      .split('\n')
      .map((row) => row.trim())
      .filter(Boolean)
  } catch {
    // No server, or no session yet. Both mean nobody owns a pane, which is a
    // real answer here rather than a failure to read one.
    return []
  }
}

interface StructuredPerson {
  id: string
  kind: string
  name: string
}
interface StructuredDepartment {
  id: string
  name: string
  /** `active` | `paused`. The durable fact a resume moves. */
  state: string
  headPersonId?: string
  people: StructuredPerson[]
  /**
   * The id of the department this one hangs under, or undefined at the root.
   *
   * KEPT through the flattening: `org_move_department` is a pure structural
   * change of nesting. Flattening without keeping it would leave that tool with
   * no true form to assert against — the department is present before and
   * after, and only its position moves.
   */
  parentId?: string
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && !isNullish(value) && !Array.isArray(value)
}

function asText(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

/** Narrow the structured tree's person rows without asserting a shape onto them. */
function asPeople(value: unknown): StructuredPerson[] {
  if (!Array.isArray(value)) return []
  return value.filter(isRecord).map((person) => ({
    id: asText(person.id),
    kind: asText(person.kind),
    name: asText(person.name)
  }))
}

/**
 * The COMPOSITE-keyed structured tree, flattened. `/v1/org/tree/read` is a
 * summary that does not name people — it looks right when it is wrong, which
 * is the failure mode this whole suite exists to remove.
 */
async function readStructuredDepartmentsOf(
  target: TmuxHostedCompany
): Promise<StructuredDepartment[]> {
  const tree = await postTo(target, '/v1/org/tree/structured', {})
  const flat: StructuredDepartment[] = []
  const walk = (nodes: unknown, parentId: string | undefined): void => {
    if (!Array.isArray(nodes)) return
    for (const node of nodes) {
      if (!isRecord(node)) continue
      const id = asText(node.id)
      flat.push({
        id,
        name: asText(node.name),
        state: asText(node.state),
        headPersonId: typeof node.headPersonId === 'string' ? node.headPersonId : undefined,
        people: asPeople(node.people),
        parentId
      })
      walk(node.children, id)
    }
  }
  walk(isRecord(tree) ? tree.departments : undefined, undefined)
  return flat
}

async function readStructuredDepartments(): Promise<StructuredDepartment[]> {
  return readStructuredDepartmentsOf(company)
}

/** The department NAMES one company's tree currently holds. */
async function departmentNamesOf(target: TmuxHostedCompany): Promise<string[]> {
  return (await readStructuredDepartmentsOf(target)).map((department) => department.name)
}

async function findDepartmentByName(name: string): Promise<StructuredDepartment | undefined> {
  return (await readStructuredDepartments()).find((department) => department.name === name)
}

/** Which department the structured tree currently lists a person under. */
async function departmentOf(personId: string): Promise<string | undefined> {
  return (await readStructuredDepartments()).find((department) =>
    department.people.some((person) => person.id === personId)
  )?.id
}

beforeAll(async () => {
  assertTmuxAvailable()
  // Not `chiefdBinaryTestGate()`: this suite has no skip branch on purpose.
  assertChiefdBinaryBuilt(REPO_ROOT)

  company = await startTmuxHostedCompany({ slug: SLUG, repoRoot: REPO_ROOT })

  await genesis(company)

  surface = await chiefSurfaceFor(company)

  /* eslint-disable lucy/no-process-env */
  // CI selects the real contract lane; this is the test runner's input.
  const contractLane = process.env.CI_TOOL_CONTRACT_LANE
  /* eslint-enable lucy/no-process-env */
  if (
    !isNullish(contractLane) &&
    !['ordered', 'independent-a', 'independent-b', 'independent-c'].includes(contractLane)
  ) {
    throw new Error(`unknown CI_TOOL_CONTRACT_LANE: ${contractLane}`)
  }
  if (!isNullish(contractLane) && contractLane.startsWith('independent-')) {
    // The independent lane starts after the ordered lane's first department.
    // Create that small real fixture here so the tests keep their normal
    // company state without depending on another runner or a fake surface.
    const bootstrap = await surface.call('org_launch_department', {
      department: {
        name: 'Research',
        purpose: 'Provide the independent contract lane fixture.',
        head: { name: 'Rhea', mandate: 'Lead research.' },
        staff: [
          { name: 'Sol', mandate: 'Do research.' },
          { name: 'Nia', mandate: 'Do more research.' }
        ]
      }
    })
    if (!bootstrap.ok) {
      throw new Error(`independent contract bootstrap failed: ${bootstrap.message}`)
    }
  }
}, BOOT_TIMEOUT_MS)

afterAll(async () => {
  await company?.stop()
})

// #1031: this suite produced `corrupt store: activity` seven times and a cause
// zero times, because the daemon wrote the cause into a log that `stop()`
// deleted and nothing read. `afterEach` runs before `afterAll`, so a failure
// here now prints the daemon's own account while the log still exists.
surfaceDaemonLogOnFailure(() => company)

describe('the organization tools a manager actually calls, against a live company', () => {
  it('registers the manager tool surface for the CEO', () => {
    // A bare sanity floor: an install that silently registered nothing would
    // make every assertion below vacuously unreachable rather than red.
    expect(surface.tools.size).toBeGreaterThan(20)
    for (const name of ['org_launch_department', 'org_lifecycle_status', 'org_offboard']) {
      expect([...surface.tools.keys()]).toContain(name)
    }
  })

  it(
    'org_launch_department creates the department AND reports success (#751/P4 defect 1)',
    async () => {
      const outcome = await surface.call('org_launch_department', {
        department: {
          name: 'Research',
          purpose: 'Prove the tool, not the route.',
          head: { name: 'Rhea', mandate: 'Lead research.' },
          staff: [
            { name: 'Sol', mandate: 'Do research.' },
            { name: 'Nia', mandate: 'Do more research.' }
          ]
        }
      })

      // THE REGRESSION. Before d2b235c90 this resolved ok:false with
      // "org reconcile <slug> failed (exit 1): chiefd: unknown command 'org'"
      // — AFTER the department had already been created. Asserting on the
      // message as well as the flag keeps the failure legible when it returns.
      expect(outcome.ok, `org_launch_department failed: ${outcome.message}`).toBe(true)
      expect(outcome.message).not.toMatch(/unknown command/i)
      expect(outcome.message).not.toMatch(/system fault/i)

      // And the durable change is real, read back through the composite-keyed
      // structured tree that names people.
      const department = await findDepartmentByName('Research')
      expect(department, 'the structured tree has no Research department').toBeDefined()
      const names = (department?.people ?? []).map((person) => person.name).sort()
      expect(names).toEqual(['Nia', 'Rhea', 'Sol'])
      expect(department?.headPersonId).toBeTruthy()

      // READ-YOUR-WRITES, at the tool. The live sequence that produced this
      // packet had a CEO read the roster immediately after a create and act on
      // what it saw, so the read straight after a success is part of the
      // contract, not a separate concern. No sleep, no poll, no retry: the
      // very next tool call must already name what the previous one committed.
      const roster = await nonVacuousRoster(surface)
      expect(
        roster.message,
        'a roster read straight after a successful create must already name the department'
      ).toContain('Research')
      for (const name of ['Rhea', 'Sol', 'Nia']) {
        expect(roster.message, `the roster must already name ${name}`).toContain(name)
      }
    },
    TOOL_TIMEOUT_MS
  )

  it(
    'org_lifecycle_status projects the board through chiefd, not a subprocess',
    async () => {
      const outcome = await surface.call('org_lifecycle_status', {})
      expect(outcome.ok, `org_lifecycle_status failed: ${outcome.message}`).toBe(true)
      expect(outcome.message).not.toMatch(/unknown command/i)
      // Genesis' executive plus the department created above, and its four
      // people: the CEO, Rhea, Sol and Nia. Asserted exactly — a projection
      // that answered "0 departments, 0 people" is precisely how this verb
      // failed before, and a loose match would have called that a pass.
      expect(outcome.message).toMatch(/2 departments, 4 people/)
    },
    TOOL_TIMEOUT_MS
  )

  it(
    'org_offboard succeeds after it commits (#751/P4 defect 2)',
    async () => {
      const research = await findDepartmentByName('Research')
      const workers = (research?.people ?? []).filter(
        (person) => person.id !== research?.headPersonId
      )
      expect(workers.length, 'the Research department has no ordinary workers to offboard').toBe(2)
      // The SECOND worker, deliberately: the first is the subject of the
      // placement family further down, and swapping which one departs would
      // change the fixture every later test in this lane inherits.
      const offboardTarget = workers[1]

      // THE REGRESSION. Before abfaf6d11 this resolved ok:false with
      // "chiefd docstore /v1/org/staffing/lifecycle returned an invalid
      // outcome" — after the offboard had already committed.
      const offboarded = await surface.call('org_offboard', {
        personId: offboardTarget.id
      })
      expect(offboarded.ok, `org_offboard failed: ${offboarded.message}`).toBe(true)
      expect(offboarded.message).not.toMatch(/invalid outcome/i)

      // The offboard is DURABLE, read back through a second tool rather than a
      // route. `/v1/org/tree/structured` deliberately keeps naming a departed
      // person — offboarding "retains their stable identity, history
      // and audit record" — so tree absence would be the wrong question, and a
      // test that asked it would fail against correct behaviour. The headcount
      // is the question that has an answer: four people before, three after.
      const board = await surface.call('org_lifecycle_status', {})
      expect(board.ok, `org_lifecycle_status failed: ${board.message}`).toBe(true)
      expect(board.message).toMatch(/2 departments, 3 people/)
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * The durable-REMINDER family, end to end through the tools an agent calls.
 *
 * This family is why the suite exists. Deleting the `@koltmcbride/pi-loop`
 * addon made reminders the ONLY way anything happens again later, and all
 * three reminder tools were dead: they spawned `apps/cli/src/Main.ts org
 * reminder <action>`, and that CLI now serves exactly one command
 * (`founder-pi`), so every call answered `unknown command 'org'`. A route test
 * could never have seen it — the route was fine the whole time.
 */
describe('durable reminders, through the tools, against a live company', () => {
  it(
    'arms, lists and stops a reminder, and writes it under the COMPOSITE company key',
    async () => {
      const armed = await surface.call('org_create_reminder', {
        prompt: 'Re-read the risk limits before sizing anything.',
        // The recurring floor: twice the settle window, so a whole park fits
        // between fires. A minute was legal until 2026-08-27 and made parking
        // unreachable — this test neither waits for a fire nor cares about the
        // cadence, so it takes the legal one.
        intervalMs: 600_000
      })
      expect(armed.ok, `org_create_reminder failed: ${armed.message}`).toBe(true)
      // The exact shape of the CLI-era failure, asserted by name so a
      // regression that re-introduces a subprocess is unmistakable.
      expect(armed.message).not.toMatch(/unknown command/i)
      const reminderId = reminderIdOf(armed.details)
      expect(typeof reminderId, 'the armed reminder must come back with its id').toBe('string')

      const listed = await surface.call('org_list_reminders', {})
      expect(listed.ok, `org_list_reminders failed: ${listed.message}`).toBe(true)
      expect(listed.message).toMatch(/1 armed reminder/)

      // THE DEFECT LOCK (#751/P4's composite-key class): chiefd resolves a
      // reminder's authority by `req.slug == org_documents_slug`, which is the
      // COMPOSITE key. A bare slug does not fail loudly — it matches no live
      // company, so the route 404s and the reminder is silently never armed.
      // Fixtures make the two equal, which is exactly why this is asserted
      // against the running daemon and against BOTH keys.
      const underComposite = await postAs('chief', '/v1/reminders/list', { personId: 'chief' })
      expect(reminderIdsOf(underComposite)).toContain(reminderId)

      /* eslint-disable lucy/no-json-stringify */
      // Deliberately hand-rolled rather than routed through `post`, which
      // always sends the composite key — sending the BARE slug is the subject.
      // The CEO's own bearer rides along, and it has to: authentication is
      // universal, so a credential-free probe is answered 401 by the
      // middleware before any route resolves any company, and the assertion
      // below would be about the missing header instead of about the KEY.
      const bare = await fetch(`${company.url}/v1/reminders/list`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', ...(await bearerFor('chief')) },
        body: JSON.stringify({ slug: SLUG, personId: 'chief' })
      })
      /* eslint-enable lucy/no-json-stringify */
      // 404, not 403: past the middleware the route resolves its company
      // SOURCE before it resolves its caller, so a bare slug is refused as
      // "not this daemon's company" rather than as a scope failure.
      expect(
        bare.status,
        'a BARE slug must not resolve a live company — if this ever returns 2xx the ' +
          'composite-key lock below is vacuous and the tool could regress unnoticed'
      ).toBe(404)

      const stopped = await surface.call('org_stop_reminder', { reminderId })
      expect(stopped.ok, `org_stop_reminder failed: ${stopped.message}`).toBe(true)
      expect(stopped.message).toMatch(/Removed reminder/)

      const afterStop = await surface.call('org_list_reminders', {})
      expect(afterStop.ok).toBe(true)
      expect(afterStop.message).toMatch(/0 armed reminders, 1 stopped/)
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * THE LAST FOUR FAMILIES OFF THE SUBPROCESS (#751/P4).
 *
 * With these, `IntercomSeamClassification`'s `subprocessCallSites` is ZERO:
 * nothing in `organization-intercom.ts` reaches chiefd by spawning anything.
 * Each family had a required field the deleted CLI supplied from the pane's
 * launcher-injected identity, which is why a transport-only port would have
 * been refused rather than merely slower.
 */
describe('the last four families, through the tools, against a live company', () => {
  it(
    'org_resume_departments resumes a paused department in one transaction',
    async () => {
      const research = await findDepartmentByName('Research')
      const departmentId = research?.id ?? ''
      expect(departmentId, 'the Research department must exist by now').not.toBe('')

      // SETUP, not subject: put the unit into `paused` through the by-id route
      // so the tool has something real to resume. The tool is what is under
      // test, and it is the multi-unit route (`resume-many`) which is a
      // different request shape entirely.
      await post('/v1/org/department/pause', { departmentId })
      expect(
        (await findDepartmentByName('Research'))?.state,
        'the setup pause must actually have committed, or the resume below proves nothing'
      ).toBe('paused')

      const resumed = await surface.call('org_resume_departments', {
        departmentIds: [departmentId]
      })
      // Before this packet the tool spawned `apps/cli/src/Main.ts department
      // launch --units`, so it failed on a missing file. Underneath that it
      // had a second break: the CLI's own argument check for that command
      // allowed only `--units`, and the helper appended `--socket`/`--session`.
      expect(resumed.ok, `org_resume_departments failed: ${resumed.message}`).toBe(true)
      expect(resumed.message).not.toMatch(/unknown command/i)
      expect(resumed.message).toMatch(/Resumed 1 department/)

      // DURABLE, read back through the composite-keyed structured tree.
      expect((await findDepartmentByName('Research'))?.state).toBe('active')

      // The other direction: a unit id that does not exist must come back as
      // chiefd's REFUSAL, not as a success and not as a transport fault. A
      // one-directional assertion here would stay green if the route stopped
      // being called at all.
      const bogus = await surface.call('org_resume_departments', {
        departmentIds: ['no-such-department']
      })
      expect(bogus.ok, 'resuming an unknown department must be refused').toBe(false)
      expect(bogus.message).not.toMatch(/unknown command/i)
    },
    TOOL_TIMEOUT_MS
  )

  // TOMBSTONE: `org_maintain_session writes a durable, target-generation
  // maintenance request`. The tool is deleted whole; there is no call left to
  // make.

  /* TOMBSTONE (chief-home-is-cwd §3/§4e): 'org_hire reads the installed Pi
   * resource catalog through chiefd'. It read `/v1/org/resource-catalog/read`
   * for a real installed skill id, refused a hire naming an id the catalog did
   * not list (`hire_resources_invalid`), and accepted one that it did. The
   * route, the preflight and the `skills` field are all deleted: an agent's
   * skills are the files in `<dir>/.pi/skills`, which Pi loads through one
   * symlink, so `org_hire` has no resource argument to validate. The
   * `additionalProperties: false` seed schema now refuses `skills` outright,
   * which is the surviving rule and is pinned in `HireRequest.test.ts`. */

  it(
    'hires several people in ONE call, in order, and names every one of them',
    async () => {
      // BATCH HIRING. An operator asked one person to hire fifteen; the agent
      // issued fifteen PARALLEL org_hire calls and every one came back
      // `chiefd unavailable (timeout)`. chiefd runs one writer thread per
      // company and every mutation takes BEGIN IMMEDIATE, so concurrent hires
      // do not run concurrently — they queue, and the later ones exceed the
      // client's patience while still waiting their turn.
      //
      // One call that hires in order never forms that queue. It is also the
      // safer shape: a timeout is the CALLER giving up rather than the server
      // cancelling, so the parallel version could leave people hired that its
      // caller was told had failed.
      const batch = await surface.call('org_hire', {
        departmentId: 'research',
        people: [
          {
            name: 'Batch Alpha',
            mandate: 'First of a batch.'
          },
          {
            name: 'Batch Beta',
            mandate: 'Second of the same batch.'
          }
        ]
      })

      expect(batch.ok, `batch org_hire failed: ${batch.message}`).toBe(true)
      expect(
        batch.details.hired,
        'the result must name every person hired — an operator checking a batch is ' +
          'checking the count, and a partial answer is how a retry creates duplicates'
      ).toHaveLength(2)
      expect(batch.message).toMatch(/Hired 2 people/)

      // Both really landed, read back through a different surface than the one
      // that reported them.
      const research = (await findDepartmentByName('Research'))?.people.map((person) => person.name)
      expect(research).toContain('Batch Alpha')
      expect(research).toContain('Batch Beta')
    },
    TOOL_TIMEOUT_MS
  )

  it(
    '#1046: a CEO staffs the root it heads, and the company slug is corrected, not denied',
    async () => {
      // THE INCIDENT, replayed. A brand-new company came up with a CEO and
      // nobody else, the operator asked for a chief of staff, and the CEO
      // burned three attempts before escaping by luck. Each `attempt` below is
      // one of them, in order.
      const rootBefore = (await readStructuredDepartments()).find(
        (department) => department.id === 'executive'
      )
      expect(rootBefore, 'the company must have an executive root to staff').toBeDefined()

      // ATTEMPT 1 — the company slug passed as a department id, because the
      // root department's id is `executive` while its NAME is the company
      // display name. This used to answer "'chief' does not manage department
      // '<slug>'", which was false: the CEO manages every department, and the
      // id simply did not exist.
      const attempt = await surface.call('org_hire', {
        departmentId: SLUG,
        person: {
          name: 'Root Chief Of Staff',
          mandate: 'Run the CEO office.'
        }
      })
      expect(attempt.ok, 'an id that names no department must still be refused').toBe(false)
      expect(attempt.message).toContain(`Unknown department '${SLUG}'`)
      expect(
        attempt.message,
        'an unknown id must never be reported as an authority failure — that is what ' +
          'sent the CEO hunting a permission it already held'
      ).not.toMatch(/does not manage/)
      expect(attempt.message).toContain("root department id is 'executive'")
      expect(attempt.message).toMatch(/Departments you may hire into: [^.]*executive/)

      // ATTEMPT 2 — exactly what the old refusal advised: create a department
      // naming yourself as its existing head. chiefd refuses that for the CEO
      // and always has (`exec-root-protected`, a product invariant). The
      // refusal is asserted here so the invariant stays locked; what changed is
      // that nothing points a CEO at this path any more.
      const selfHeaded = await surface.call('org_add_department', {
        name: 'Dead End Office',
        purpose: 'Prove the advised path is refused.',
        existingHeadPersonId: 'chief'
      })
      expect(selfHeaded.ok, 'the CEO must never be movable into a new unit').toBe(false)
      // The CEO is still refused, and it is now the ONLY person who is:
      // operator ruling 2026-08-13 (`AGENTS.md`, "THE CEO IS THE ONLY
      // IMMOVABLE NODE"). The copy also changed — a CEO given only
      // "executive-root protected" burned a dozen turns guessing — so it now
      // names the person, states that appointing an existing head MOVES them,
      // and names a way through. Asserted here because this is the surface an
      // agent actually reads.
      expect(selfHeaded.message).toContain("'chief'")
      expect(selfHeaded.message).toContain('the CEO')
      expect(selfHeaded.message).toContain('MOVES that person')
      expect(selfHeaded.message).toContain('NEW head')

      // ATTEMPT 3 was never necessary. The CEO could always hire into the root
      // it heads; only the id was wrong.
      const hired = await surface.call('org_hire', {
        departmentId: 'executive',
        person: {
          name: 'Root Chief Of Staff',
          mandate: 'Run the CEO office.'
        }
      })
      expect(hired.ok, `a CEO must be able to hire into the root: ${hired.message}`).toBe(true)
      const rootAfter = (await readStructuredDepartments()).find(
        (department) => department.id === 'executive'
      )
      expect(rootAfter?.people.map((person) => person.name)).toContain('Root Chief Of Staff')
    },
    TOOL_TIMEOUT_MS
  )

  it(
    '#1065: the CEO is the only immovable node — a root-homed worker heads a new unit, and a department moves beneath them',
    async () => {
      // THE OPERATOR'S SHAPE, end to end. The operator told a CEO to have its
      // Chief of Staff stand up Engineering. The CEO refused, reporting that a
      // Chief of Staff "doesn't hold the org-management tools needed to create
      // a department or hire a department head — those are CEO/head-level
      // functions". No such gate exists. What DID refuse was chiefd, and only
      // because the person was homed in the executive root: the exemption
      // covered the whole root rather than the CEO alone.
      //
      // Operator ruling, 2026-08-13 (`AGENTS.md`): the CEO is the one exempt
      // person; everyone else is fluid. This drives that ruling through the
      // tools an agent actually calls, against a real chiefd.
      const staffed = await surface.call('org_hire', {
        departmentId: 'executive',
        person: {
          name: 'Carla Ruling',
          mandate: 'Run the CEO office as general staff.'
        }
      })
      expect(staffed.ok, `a CEO must be able to hire into the root: ${staffed.message}`).toBe(true)
      const rootMembers = (await readStructuredDepartments()).find(
        (department) => department.id === 'executive'
      )?.people
      const carla = rootMembers?.find((person) => person.name === 'Carla Ruling')
      const carlaId = carla?.id ?? ''
      expect(carlaId, 'the fixture needs a root-homed person who heads nothing').not.toBe('')

      // ---- the create that used to be refused ------------------------------
      // `existingHeadPersonId` names somebody who already works here, and the
      // appointment MOVES them into the unit they now head. Before the ruling
      // this answered `exec-root-protected` for anybody merely homed in the
      // root, which is the whole defect.
      const office = await surface.call('org_add_department', {
        name: 'Office Of The Chief Of Staff',
        purpose: 'Prove a root-homed worker may head a unit.',
        existingHeadPersonId: carlaId
      })
      expect(
        office.ok,
        `a root-homed worker must be appointable as a head: ${office.message}`
      ).toBe(true)
      const officeUnit = await findDepartmentByName('Office Of The Chief Of Staff')
      expect(officeUnit?.headPersonId, 'the appointed person must now head the new unit').toBe(
        carlaId
      )
      // The move half of the same atomic change (AGENTS.md consequence 1:
      // appointing an existing head MOVES that person). A create that appointed
      // without moving would leave one person listed in two departments.
      expect(
        (await readStructuredDepartments())
          .find((department) => department.id === 'executive')
          ?.people.map((person) => person.id),
        'the appointed head must have left the root they were homed in'
      ).not.toContain(carlaId)

      // ---- and a department moves beneath them -----------------------------
      // The CEO does this reparent, NOT the new head: the unit being moved is a
      // PEER, outside the subtree the new head owns, and reaching sideways is
      // the one direction the tree model forbids. That refusal is correct and
      // must survive the narrowing.
      const peer = await surface.call('org_add_department', {
        name: 'Ruling Engineering',
        purpose: 'The department the operator asked for.',
        head: { name: 'Enzo Ruling', mandate: 'Lead ruling engineering.' }
      })
      expect(peer.ok, `org_add_department failed: ${peer.message}`).toBe(true)
      const peerUnit = await findDepartmentByName('Ruling Engineering')
      const peerId = peerUnit?.id ?? ''
      expect(peerId, 'the fixture needs a peer department to move').not.toBe('')

      const reparented = await surface.call('org_reparent_department', {
        departmentId: peerId,
        newParentDepartmentId: officeUnit?.id
      })
      expect(
        reparented.ok,
        `a unit headed by a former root member must accept a child: ${reparented.message}`
      ).toBe(true)
      expect(
        (await findDepartmentByName('Ruling Engineering'))?.parentId,
        'the reparented department must hang under the chief-of-staff office'
      ).toBe(officeUnit?.id)

      // ---- the CEO is still refused every structural move -------------------
      // The exemption did not go away; it narrowed to exactly one person.
      const chiefHeadsNewUnit = await surface.call('org_add_department', {
        name: 'Ruling Dead End',
        purpose: 'The CEO may never leave the root it heads.',
        existingHeadPersonId: 'chief'
      })
      expect(chiefHeadsNewUnit.ok, 'the CEO must never be movable into a new unit').toBe(false)
      expect(
        (await departmentNamesOf(company)).filter((name) => name === 'Ruling Dead End'),
        'a refused create must not leave a department behind'
      ).toEqual([])

      const chiefTransfer = await surface.call('org_transfer', {
        personId: 'chief',
        departmentId: officeUnit?.id
      })
      expect(chiefTransfer.ok, 'the chief must never be transferred').toBe(false)
      expect(
        (await readStructuredDepartments()).find((department) => department.id === 'executive')
          ?.headPersonId,
        'the CEO must still head the root after every refused move'
      ).toBe('chief')
    },
    TOOL_TIMEOUT_MS
  )

  // TOMBSTONE, AND A NAMED COVERAGE LOSS: `a settled session CLAIMS and
  // CLOSES its own maintenance request`.
  //
  // The claim ladder it exercised — `start` then `finish`, driven from Pi's
  // settled boundary — SURVIVES: it is how the automatic compaction runs. What
  // is gone is the only way a CONTRACT test could create a request to drive it
  // with, because the tool was that way. An automatic compaction needs a
  // session over 50% context, which a booted fixture does not have.
  //
  // So this is a real reduction in contract coverage, recorded rather than
  // quietly dropped. What still covers the ladder: the unit tests over
  // `session_maintenance_ops`, which pin the logic at the level it lives at.
  //
  // AND THE LIVE BOX IS NOT A CONSOLATION HERE — IT IS THE BETTER INSTRUMENT.
  // This lane boots a real host, and after the Pi patch is removed it boots a
  // STOCK Pi, which is exactly where its evidence is worth most. For THIS
  // claim, a genuine auto-compact row on a running company proves more than a
  // fixture ever could: it proves the ladder ran against the Pi the operator
  // actually has.
  //
  // THE COMPENSATING CONTROL IS A NAMED CHECK SOMEBODY RUNS, not a hope that
  // production would notice. The first automatic compaction on the operator's
  // company is an explicit item in the post-deploy verification battery: an
  // `auto-compact` row appearing in the session-maintenance ledger after a
  // person crosses 50% context. It has never fired before — the gate that
  // guarded it was unsatisfiable until #1239 — so its first appearance is a
  // one-time, checkable result with a date, not a standing assumption.
  //
  // If that check does not produce a row, this deletion has cost real coverage
  // and the ladder needs an instrument again.
  //
  // THE RESTORATION CONDITION, concretely: if the claim ladder ever needs
  // contract-lane coverage again, the honest route is a fixture whose session
  // is GENUINELY over the threshold — not a test-only queue seam, which would
  // be the second entry point this file's own history is full of deleting.

  it(
    'a settled session drives the activity read live',
    async () => {
      // `org activity status` and the session-maintenance CLAIM verbs have no
      // tool. Their only entry point is Pi's settled boundary, which is why
      // the fixture can now deliver one: the alternative is a route POST, and
      // #751/P4 has already shipped three packets whose route returned 200
      // over a broken product.
      await surface.startSession()
      const before = journalEvents()

      // >50% context usage is what opens `queueAutomaticParkCompaction`, the
      // single reader of `org activity status`.
      await surface.settle({ contextUsagePercent: 90 })

      const added = journalEvents().slice(before.length)
      const deferrals = added.filter((event) =>
        /^(automatic-park-compaction-deferred|session-maintenance-.*-deferred)$/.test(
          asText(event.event)
        )
      )
      expect(
        deferrals.map((event) => `${asText(event.event)}: ${asText(event.error)}`),
        'a settled boundary must not journal a deferral: every one of these is the ' +
          'extension telling itself that a durable session-maintenance or activity call ' +
          'failed, which is exactly what a missing identity field produces'
      ).toEqual([])

      // NON-VACUITY, and the other direction. The positive assertion above
      // cannot by itself distinguish "the activity read succeeded" from "the
      // gate closed before it ran", so the two fields this packet supplies are
      // asserted to be load-bearing against the same live daemon: without
      // `callerPersonId` the route refuses, and under the BARE slug it is
      // refused too. If either ever answered 2xx, a transport-only port would
      // have looked correct.
      //
      // All three probes carry the CEO's OWN bearer, and that is what makes
      // them about the BODY. The route binds `callerPersonId` to the
      // authenticated caller, so an anonymous probe answers 401 in the
      // middleware and never reaches the field checks these assertions are
      // named for — three 401s that would pass the first assertion for a
      // reason it does not mean and fail the other two.
      const chiefAuthorization = await bearerFor('chief')
      expect(
        (
          await postStatus(
            '/v1/org/activity/command-status',
            { slug: company.companyKey },
            company,
            chiefAuthorization
          )
        ).status,
        'callerPersonId is a required non-Option field; a body without it must be refused'
      ).toBeGreaterThanOrEqual(400)
      // 403, not the 404 this used to read. The route applies its fences IN
      // SERIES and the FIRST refusal decides the code: `bind_caller_to_declared_
      // person` compares the caller's own company against the request's slug
      // before `live()` ever looks the company up, so a bare slug is now
      // `requester-company-mismatch` rather than `unknown-company`. The
      // property is the same one and it got stronger — a bare slug does not
      // reach this company from any credential.
      expect(
        (
          await postStatus(
            '/v1/org/activity/command-status',
            { slug: SLUG, callerPersonId: 'chief' },
            company,
            chiefAuthorization
          )
        ).status,
        'a BARE slug must not resolve a live company'
      ).toBe(403)
      expect(
        (
          await postStatus(
            '/v1/org/activity/command-status',
            {
              slug: company.companyKey,
              callerPersonId: 'chief'
            },
            company,
            chiefAuthorization
          )
        ).status,
        'the exact body the extension sends must be accepted'
      ).toBe(200)
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * THE P7 PROOF: an agent authenticates with its own key, and a caller without
 * one is refused — both directions, at the tool.
 *
 * Until #751/P7 chiefd proved who a caller was by walking pid ancestry from the
 * connecting process to the terminal pane it claimed, then matching the tags
 * the launcher had stamped on that pane. A client-agnostic daemon cannot see a
 * pane, so that proof is deleted. What replaces it is the P-256 key
 * provisioning writes into each person's identity directory and enrols: the pane signs
 * a daemon-issued challenge with it and presents the resulting bearer token.
 *
 * Why both directions, always. The positive assertion alone is worthless as a
 * regression test — delete the check tomorrow and it stays green forever,
 * because "the call succeeded" is exactly what an unguarded route also does.
 * Only the refusal can tell the two apart. And the refusal must SAY WHY: an
 * agent told "chiefd is unavailable" retries something that will never work.
 *
 * The impostor is a real second identity, not a mangled header. It is a
 * genuine, well-formed P-256 key that chiefd has simply never enrolled — the
 * shape a copied pi-home, a stale checkout and an outright forgery all take.
 * That is precisely the case pane ancestry used to make impossible and that
 * possession of a key does not: nothing about holding a key says which machine
 * you are on. It is the case the generation binding and enrolment exist for.
 */
describe('caller authentication is by key, and a caller without one is refused', () => {
  const keyPath = (): string => join(company.dir, '.chief', IDENTITY_KEY_FILENAME)

  /**
   * The cause-independent probe: no credential at all, on a protected route.
   *
   * Every other assertion here runs through the intercom, so each one depends
   * on the fixture, on materialization having minted a key, and on the company
   * coming up healthy. This one depends on none of that. It is a raw POST with
   * no `Authorization` header, and the fence it hits runs BEFORE the handler
   * resolves the company — so it answers the same way on a box with no provider
   * credentials, where the CEO never launches and nothing is ever enrolled.
   *
   * That is exactly the case worth pinning. "The trust table is empty" must
   * mean NOBODY is authorized, never "there is nothing to check against, so
   * pass". A verifier that fails open on an empty table would let every call
   * through on a fresh company and look perfectly healthy doing it.
   */
  it(
    'an unauthenticated call to a person-fenced route is refused even when nothing is enrolled',
    async () => {
      const response = await fetch(`${company.url}/v1/org/session-maintenance/queue`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        /* eslint-disable lucy/no-json-stringify */
        // Same reason the read-back helper at the top of this file disables it:
        // the body must be exactly what a client sends, byte for byte.
        body: JSON.stringify({
          slug: company.companyKey,
          action: 'compact',
          personId: 'chief',
          requestedBy: 'chief',
          reason: 'P7: a caller with no credential must not reach this route.'
        })
        /* eslint-enable lucy/no-json-stringify */
      })
      const body = await response.text()
      expect(
        response.status,
        `a person-fenced route must refuse an unauthenticated caller 401, ` +
          `got ${response.status}: ${body.slice(0, 300)}`
      ).toBe(401)
      // The refusal moved OUTWARD, and the property got stronger. It used to
      // come from the route's own person fence and read `enrolled identity
      // key`; always-on auth refuses a bearer-less request in the
      // verify-middleware, before any handler runs, so it now reads `missing
      // bearer token`. The test this file exists to be — "an empty trust table
      // means NOBODY is authorized, never 'nothing to check against, so pass'"
      // — is unchanged and is now proved one layer earlier, on every route
      // rather than only on the fenced ones.
      expect(body, 'and the refusal must name its cause').toMatch(/missing bearer token/i)
    },
    TOOL_TIMEOUT_MS
  )

  it(
    'the CEO acts with its enrolled key, and an unenrolled key cannot install at all',
    async () => {
      // The key materialization minted for this person, and the ONLY thing
      // that will differ between the two halves of this test.
      await assertChiefIdentity()
      const enrolled = readFileSync(keyPath(), 'utf8')
      expect(enrolled, 'materialization must have minted the CEO an identity key').toContain(
        'PRIVATE KEY'
      )

      await surface.startSession()

      // ---- chiefd's side of the positive direction ------------------------
      // Asserted directly rather than inferred from a tool succeeding. The
      // route below is fenced to an authenticated person, so a green tool call
      // does imply a token was accepted — but only while that fence exists, and
      // this test's whole job is to survive somebody removing it. `/v1/auth`
      // answering a challenge for `ceo` is chiefd stating, independently of any
      // tool, that it has an enrolled active identity under that name.
      const challenge = await fetch(`${company.url}/v1/auth/challenge`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        /* eslint-disable lucy/no-json-stringify */
        // The challenge body is a wire contract; it must be exactly this.
        body: JSON.stringify({ identityId: 'chief' })
        /* eslint-enable lucy/no-json-stringify */
      })
      expect(
        challenge.status,
        'materialization must have ENROLLED the CEO, not merely written it a key — ' +
          'an unenrolled person is refused here, and every assertion below would ' +
          'then be passing for the wrong reason'
      ).toBe(200)

      // ---- the positive direction ----------------------------------------
      const authenticated = await surface.call('org_list_reminders', {})
      expect(
        authenticated.message,
        'with its own enrolled key the CEO must get PAST authentication — whatever ' +
          'the verb itself then answers is that family own packet to prove'
      ).not.toMatch(/enrolled identity key|only that person may|stale credential/i)

      // ---- the negative direction ----------------------------------------
      // Same person, same company, same pane, same daemon. A different private
      // key, which chiefd has never seen.
      //
      // THE REFUSAL MOVED, AND IT GOT EARLIER. It used to arrive from the
      // thinking route's own person fence, on a tool surface that installed
      // happily and then called. Under universal authentication an unenrolled
      // key cannot become a BEARER at all: `/v1/auth/token` refuses the
      // signature, the pane transport is left with no credential, and the
      // first read the install makes — the manifest — is a 401. So the
      // impostor never reaches a verb at all; it never reaches any route.
      // Both steps are asserted, because the mint refusal is the cause and the
      // failed install is the consequence, and either one alone would stay
      // green if the other were removed.
      try {
        writeFileSync(keyPath(), generateAgentKeypair().privatePkcs8Pem, { mode: 0o600 })

        await expect(
          bearerFor('chief'),
          'A KEY CHIEFD NEVER ENROLLED MUST NOT MINT A BEARER. If this resolves, an ' +
            'agent authenticated with a forged key — the one result that would make ' +
            'this packet a decoration rather than a check'
        ).rejects.toThrow()

        await expect(
          installOrganizationToolSurface({
            chiefdUrl: company.url,
            organization: SLUG,
            organizationDir: company.dir,
            personId: 'chief',
            launcherRoot: REPO_ROOT,
            tmuxSocket: company.tmuxSocket,
            tmuxSession: SLUG
          }),
          'the failure must NAME its cause; an unnamed one sends the agent back ' +
            'around a retry ladder to be refused identically forever'
        ).rejects.toThrow(/missing bearer token/i)
      } finally {
        writeFileSync(keyPath(), enrolled, { mode: 0o600 })
      }
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * THE COMMIT BOUNDARY, from the failing side.
 *
 * Observed live: a CEO called `org_launch_department` on a real company and
 * was told a system fault. The department rows had ALREADY COMMITTED. The CEO
 * read the roster, concluded "no partial commit", retried, and was refused
 * `a department with this id already exists`. Six turns to reason its way out
 * of an answer that contradicted the database.
 *
 * The cause is a post-commit step: `/v1/org/department/create` answers 200 and
 * the rows are durable, and only then does the tool POST
 * `/v1/org/runtime/launch` to bring the panes up. That second call reaches a
 * different route over HTTP and can fail for reasons that have nothing to do
 * with whether the department exists — and its failure was thrown straight
 * through `lifecycleFailure` into `ok: false`.
 *
 * # Why this needs a proxy rather than a stub
 *
 * The whole point of this suite is that a stub is what hides post-2xx defects:
 * a scripted `LauncherRunner` answered where the real one spawned a deleted
 * CLI, and that is exactly how the original reconcile defect shipped. So
 * nothing here is stubbed. A real HTTP proxy sits in front of the real daemon
 * and forwards every byte of every request EXCEPT `/v1/org/runtime/launch`,
 * which it answers 503. The tool runs unmodified, against a real company, and
 * the create really does commit before the convergence really does fail.
 *
 * # Both directions
 *
 * The success direction is asserted above (`org_launch_department creates the
 * department AND reports success`). This is the other one, and it is the one
 * that decays silently: an assertion that success works stays green forever if
 * the post-commit honesty is removed again.
 */
describe('a post-commit convergence failure does not report a failed create', () => {
  const FAULT_PATH = '/v1/org/runtime/launch'
  let proxy: Server
  let proxyUrl: string
  let faulted = 0
  let faultedSurface: OrganizationToolSurface

  beforeAll(async () => {
    proxy = createServer((request, response) => {
      const chunks: Buffer[] = []
      request.on('data', (chunk: Buffer) => chunks.push(chunk))
      request.on('end', () => {
        void (async () => {
          const path = request.url ?? '/'
          if (path === FAULT_PATH) {
            // A genuine 503, not a refusal: `postOrgRoute` decodes 400/404/422
            // into a typed refusal VALUE and throws `ChiefdUnavailableError`
            // for anything else. The thrown branch is the one that used to
            // reach `lifecycleFailure`, so it is the branch under test.
            faulted += 1
            response.writeHead(503, { 'content-type': 'text/plain' })
            response.end('injected: the runtime launch route is unavailable')
            return
          }
          try {
            // EVERY header, verbatim. Since #751/P7 a pane authenticates with a
            // bearer token minted from its enrolled identity key, so a proxy
            // that rebuilt the header set would strip the credential and the
            // tool would be refused for a reason that has nothing to do with
            // the fault this fixture injects.
            const headers = new Headers()
            for (const [name, value] of Object.entries(request.headers)) {
              if (typeof value === 'string') headers.set(name, value)
              else if (Array.isArray(value)) for (const one of value) headers.append(name, one)
            }
            headers.delete('host')
            headers.delete('content-length')
            const upstream = await fetch(`${company.url}${path}`, {
              method: request.method ?? 'POST',
              headers,
              body: chunks.length ? Buffer.concat(chunks) : undefined
            })
            const body = Buffer.from(await upstream.arrayBuffer())
            response.writeHead(upstream.status, {
              'content-type': upstream.headers.get('content-type') ?? 'application/json'
            })
            response.end(body)
          } catch (error) {
            response.writeHead(502, { 'content-type': 'text/plain' })
            response.end(error instanceof Error ? error.message : String(error))
          }
        })()
      })
    })
    await new Promise<void>((resolve) => proxy.listen(0, '127.0.0.1', resolve))
    const address = proxy.address()
    if (typeof address === 'string' || isNullish(address)) {
      throw new Error('the fault-injecting proxy did not bind a port')
    }
    proxyUrl = `http://127.0.0.1:${address.port}`

    // An install reads its daemon's address out of its own directory's
    // rendezvous, so putting a surface behind the proxy means REPUBLISHING that
    // rendezvous at the proxy's address — which `installOrganizationToolSurface`
    // does for the directory it is given.
    //
    // The surrounding suite is unaffected, and this is the property that
    // matters: an install resolves ONCE, at install time, and carries the
    // answer for its whole life. A surface installed before this block keeps
    // the real daemon's address. The process-wide variable this block used to
    // stage had no such property, which is why it could put the whole suite
    // behind the proxy.
    faultedSurface = await installOrganizationToolSurface({
      chiefdUrl: proxyUrl,
      organization: SLUG,
      organizationDir: company.dir,
      personId: 'chief',
      launcherRoot: REPO_ROOT,
      tmuxSocket: company.tmuxSocket,
      tmuxSession: SLUG
    })
  }, BOOT_TIMEOUT_MS)

  afterAll(async () => {
    await new Promise<void>((resolve) => proxy.close(() => resolve()))
  })

  it(
    'org_launch_department answers success with a warning when the runtime launch route faults',
    async () => {
      const before = faulted
      const outcome = await faultedSurface.call('org_launch_department', {
        department: {
          name: 'Logistics',
          purpose: 'Prove the commit boundary from the failing side.',
          head: { name: 'Otto', mandate: 'Lead logistics.' },
          staff: [{ name: 'Pia', mandate: 'Do logistics.' }]
        }
      })

      // NON-VACUITY FIRST. If the fault never fired, every assertion below
      // would pass for the wrong reason — it would simply be the success path
      // a second time.
      expect(faulted, 'the injected runtime-launch fault never fired').toBeGreaterThan(before)

      // THE REGRESSION. This resolved `ok: false` with a "(system fault)" card
      // while the department, its head and its worker were already committed.
      expect(
        outcome.ok,
        `a create whose rows committed must not report failure: ${outcome.message}`
      ).toBe(true)
      expect(outcome.message).toMatch(/Created department/i)

      // The answer must be HONEST, not merely positive: it says the change is
      // durable, that the runtime did not come up, and — the fact that cost
      // six turns — that retrying the create is the wrong move.
      const warning = String(outcome.details.warning ?? '')
      expect(warning, 'a post-commit convergence failure must carry a warning').not.toBe('')
      expect(warning).toMatch(/durable/i)
      expect(warning).toMatch(/must not be retried/i)

      // THE DATABASE AGREES WITH THE ANSWER. Read back through the REAL
      // daemon, not the proxy, so nothing about this read is under the
      // fixture's control.
      const department = await findDepartmentByName('Logistics')
      expect(department, 'the department the tool reported must really exist').toBeDefined()
      expect((department?.people ?? []).map((person) => person.name).sort()).toEqual([
        'Otto',
        'Pia'
      ])

      // READ-YOUR-WRITES, straight after, through the tool the CEO actually
      // used. This is the read that showed nothing and sent a correct agent
      // down a wrong path.
      const roster = await nonVacuousRoster(faultedSurface)
      expect(
        roster.message,
        'the roster read straight after must already name the committed department'
      ).toContain('Logistics')
      expect(roster.message).toContain('Otto')
      expect(roster.message).toContain('Pia')

      // AND THE OTHER DIRECTION on the same live company: a genuine PRE-commit
      // refusal must still be a failure. Without this, "always answer true"
      // would pass everything above.
      const refused = await faultedSurface.call('org_launch_department', {
        parentUnitId: 'no-such-parent',
        department: {
          name: 'Nowhere',
          purpose: 'A parent that does not exist.',
          head: { name: 'Vex', mandate: 'Lead nothing.' }
        }
      })
      expect(refused.ok, 'a create under an unknown parent must still fail').toBe(false)
      expect(
        await findDepartmentByName('Nowhere'),
        'a refused create must leave no rows behind'
      ).toBeUndefined()
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * A PERSON WITH NO TMUX IDENTITY, AGAINST A REAL DAEMON.
 *
 * Some verbs used to register only behind a predicate that demanded a tmux
 * socket AND a session name. AC6 (`271c4c554`) removed both from chiefd's
 * API-host profile on purpose, so a person hosted by `apps/web` failed a test
 * about tmux and lost capabilities that are not about tmux. They now register
 * unconditionally.
 *
 * WHY THIS BELONGS AGAINST A LIVE DAEMON and not only against a stub: a stub
 * proves the extension can register against the shape the TEST believes in.
 * This proves the tools reach the routes chiefd actually serves.
 */
describe('a web-hosted person carries no tmux identity and still holds its tools', () => {
  let hosted: OrganizationToolSurface

  beforeAll(async () => {
    hosted = await installOrganizationToolSurface({
      chiefdUrl: company.url,
      organization: SLUG,
      organizationDir: company.dir,
      personId: 'chief',
      launcherRoot: REPO_ROOT
      // No `tmuxSocket` and no `tmuxSession` — exactly what
      // `api_host_environment` publishes for a hosted person.
    })
    await hosted.startSession()
  }, BOOT_TIMEOUT_MS)

  it(
    'registers its tools with no tmux identity and can really reach the daemon',
    async () => {
      // `org_maintain_session` stood first here until it was deleted whole.
      for (const name of ['org_hire', 'org_list_reminders']) {
        expect(
          [...hosted.tools.keys()],
          `${name} is granted to a hosted CEO and must be registered for one`
        ).toContain(name)
      }

      // A REAL CALL, not only a registration set: a tool that registers and
      // then cannot reach its route is the failure a stub would miss.
      const listed = await hosted.call('org_list_reminders', {})
      expect(listed.ok, `org_list_reminders failed: ${listed.message}`).toBe(true)
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * THE MULTI-COMPANY PROOF: two companies live in ONE process, each reaching
 * its own daemon.
 *
 * # Why a second company, and why the negative assertion is the whole test
 *
 * Every org tool used to resolve its company's daemon from one process-global
 * environment variable, read at CALL time. That is exactly right for one
 * deployment and one only — one Pi process per tmux pane, one company per
 * process — and unusable for a server that hosts several companies at once,
 * where there is no single correct value. Setting such a variable around a
 * call is not a fix but the same race with more steps. The variable itself is
 * retired now, and no production file may even name it
 * (`scripts/test/no-chiefd-url-stamp.test.mjs`); what this suite still proves
 * is the property that outlived it — two live companies in ONE process, each
 * tool call reaching its OWN daemon.
 *
 * What makes it worth a suite this expensive is that the failure is SILENT. A
 * wrong daemon ANSWERS. It does not refuse, it does not 500, it does not time
 * out — it commits the mutation into the wrong company and returns 200, and a
 * test that calls one company's tool and checks for success proves precisely
 * nothing. So the assertion that carries the weight here is the negative one:
 * company A's tree must NOT contain company B's department. Delete the
 * negative half and this block goes green against the very defect it exists
 * for.
 *
 * # And it is genuinely two of everything
 *
 * Two `chiefd run` daemons on two ports, two data roots, two tmux sockets, two
 * beaconds, two genesis passes, two installs — and both read back through
 * `/v1/org/tree/structured`, the COMPOSITE-keyed route that names people,
 * because `tree/read` is a summary that looks right when it is wrong.
 *
 * This file used to stage the race with the ambient variable itself: unset it
 * for the whole suite, then name the WRONG company for the duration of one
 * call. That staging is gone with the variable — a name no production file may
 * carry cannot steer anything, and the guard that proves it
 * (`scripts/test/no-chiefd-url-stamp.test.mjs`) is stronger and costs this
 * expensive one-boot suite nothing. What remains here is the part the staging
 * only ever dressed up: two real companies, two real daemons, and a mutation
 * that must land in exactly one of them.
 *
 * Since #983 each install RESOLVES its address from its own company's beacond
 * rather than being handed one, so the two surfaces below are pointed at two
 * registries and ask each for their own slug. That is what makes the negative
 * assertions reachable: there is no longer any value, anywhere, that both
 * companies share.
 */
describe('two companies hosted in one process each reach their own daemon', () => {
  const OTHER_SLUG = 'toolcontractb'
  let other: TmuxHostedCompany
  let otherSurface: OrganizationToolSurface

  beforeAll(async () => {
    other = await startTmuxHostedCompany({ slug: OTHER_SLUG, repoRoot: REPO_ROOT })
    await genesis(other)
    // The SECOND install in this process. The first (`surface`, company A) is
    // still live and still gets called below — that simultaneity is the point.
    otherSurface = await chiefSurfaceFor(other)
  }, BOOT_TIMEOUT_MS)

  afterAll(async () => {
    await other?.stop()
  })

  it(
    'a department created on B lands in B and is absent from A',
    async () => {
      const outcome: ToolCallOutcome = await otherSurface.call('org_launch_department', {
        department: {
          name: 'Beta Only',
          purpose: 'Exists in company B and nowhere else.',
          head: { name: 'Bea', mandate: 'Lead company B research.' }
        }
      })

      expect(outcome.ok, `org_launch_department on company B failed: ${outcome.message}`).toBe(true)

      // POSITIVE: it is in B's own durable tree, read from B's own daemon.
      expect(await departmentNamesOf(other)).toContain('Beta Only')

      // NEGATIVE, and this is the assertion that means anything: A never saw
      // it. A wrong daemon answers, so "the call returned 200" above is not
      // evidence of where the write went — only A's silence is.
      expect(await departmentNamesOf(company)).not.toContain('Beta Only')
    },
    TOOL_TIMEOUT_MS
  )

  it(
    'and the reverse: a department created on A lands in A and is absent from B',
    async () => {
      // Asserted in BOTH directions rather than once, because a one-way test
      // passes just as happily against an implementation that pins every call
      // to whichever company installed last. Only the pair can tell "each
      // install reaches its own" from "everything reaches one".
      const outcome = await surface.call('org_launch_department', {
        department: {
          name: 'Alpha Only',
          purpose: 'Exists in company A and nowhere else.',
          head: { name: 'Alma', mandate: 'Lead company A research.' }
        }
      })
      expect(outcome.ok, `org_launch_department on company A failed: ${outcome.message}`).toBe(true)

      expect(await departmentNamesOf(company)).toContain('Alpha Only')
      expect(await departmentNamesOf(other)).not.toContain('Alpha Only')

      // And B's own department is still B's: neither install has drifted onto
      // the other's daemon over the course of the block.
      expect(await departmentNamesOf(other)).toContain('Beta Only')
      expect(await departmentNamesOf(company)).not.toContain('Beta Only')
    },
    TOOL_TIMEOUT_MS
  )

  it(
    'each install signs as its own company: B refuses nothing A did, and both stay authenticated',
    async () => {
      // The credential travels with the address for the same reason the
      // address travels with the context: it used to be a single module-level
      // slot bound at install, so in this exact scenario — two installs, one
      // process — the second bind replaced the first and every later call from
      // EITHER company presented the last-installed person's bearer. chiefd
      // answers a wrong-but-valid credential with a refusal that names the
      // person, so the shape of the failure would have been a legible 403 on
      // the first company rather than anything obviously about identity.
      await surface.startSession()
      await otherSurface.startSession()

      const onB = await otherSurface.call('org_list_reminders', {})
      expect(
        onB.message,
        'company B must get past authentication with its own enrolled key'
      ).not.toMatch(/enrolled identity key|only that person may/i)

      const onA = await surface.call('org_list_reminders', {})
      expect(
        onA.message,
        'installing company B must not have replaced company A own credential'
      ).not.toMatch(/enrolled identity key|only that person may/i)
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * WHO MAY ARM A REMINDER ON WHOM.
 *
 * `org_create_reminder` has told every agent since it shipped that `personId`
 * is for "a manager arming a reminder for someone they manage". Nothing
 * enforced it. The deleted CLI passed `personId`/`createdByPersonId` straight
 * through, and chiefd's `arm_reminder` checked only that both people EXISTED
 * — so any worker could arm a recurring, durable wake-up on the CEO, and the
 * product's own description was false (DECISIONS.md, 2026-08-09, filed out of
 * the transport port rather than fixed inside it).
 *
 * This is the tool proof, not a route proof, for the reason the whole file
 * exists: the authority now rides on the caller's CREDENTIAL, and only a real
 * pane holding a real key exercises that. Each surface here reads the identity
 * key materialization minted for its own person, so "the worker is refused"
 * is proven the way production proves it.
 *
 * # Both directions, deliberately
 *
 * The allowed direction alone would stay green for ever if the gate were
 * deleted again — which is exactly how this defect survived from the day the
 * tool shipped. The refusal is the assertion that decays, so it is the one
 * asserted hardest: refused, NAMED, and with nothing written behind it.
 *
 * The two surfaces are independent: a caller's credential is resolved from its
 * own endpoint (daemon, person, key), so installing the worker's surface
 * leaves the CEO's alone and the read-back below is genuinely the CEO's.
 */
describe('a reminder may be armed only on yourself or on somebody you manage', () => {
  it(
    'the CEO arms one on a worker it manages; that worker cannot arm one back on the CEO',
    async () => {
      const built = await surface.call('org_launch_department', {
        department: {
          name: 'Treasury',
          purpose: 'Own the reminder-authority fixture.',
          head: { name: 'Rae', mandate: 'Lead treasury.' },
          staff: [{ name: 'Sol', mandate: 'Work the treasury.' }]
        }
      })
      expect(built.ok, `org_launch_department failed: ${built.message}`).toBe(true)

      const treasury = await findDepartmentByName('Treasury')
      expect(treasury, 'the reminder-authority fixture department must exist').toBeDefined()
      const worker = treasury?.people.find((person) => person.id !== treasury.headPersonId)
      expect(worker, 'the fixture department must have a non-head person').toBeDefined()
      const workerId = worker?.id
      expect(typeof workerId).toBe('string')

      // ---- the allowed direction ------------------------------------------
      // The executive manages every unit, so this is a real cross-person write
      // that must go through.
      const armed = await surface.call('org_create_reminder', {
        personId: workerId,
        prompt: 'Reconcile the treasury ledger before close.',
        // See above: the recurring floor. This test is about WHO may arm on
        // whom, not about cadence.
        intervalMs: 600_000
      })
      expect(armed.ok, `a manager arming on somebody they manage failed: ${armed.message}`).toBe(
        true
      )
      const reminderId = reminderIdOf(armed.details)
      expect(typeof reminderId, 'the armed reminder must come back with its id').toBe('string')
      // Credited to the AUTHENTICATED caller, not to a field the pane sent:
      // `createdByPersonId` left the wire, so this is chiefd naming the key it
      // verified.
      const armedRow = isRecord(armed.details) ? armed.details.reminder : undefined
      expect(isRecord(armedRow) ? armedRow.createdByPersonId : undefined).toBe('chief')
      expect(isRecord(armedRow) ? armedRow.personId : undefined).toBe(workerId)

      // ---- the refused direction -------------------------------------------
      const workerSurface = await installOrganizationToolSurface({
        chiefdUrl: company.url,
        organization: SLUG,
        organizationDir: company.dir,
        personId: workerId ?? '',
        launcherRoot: REPO_ROOT,
        tmuxSocket: company.tmuxSocket,
        tmuxSession: SLUG
      })

      // The worker CAN still arm one on itself — the gate is scope, not a
      // manager-only tool, and a baseline verb that stopped working for a
      // worker would be the feature not being delivered.
      const own = await workerSurface.call('org_create_reminder', {
        prompt: 'Read the treasury runbook.',
        intervalMs: 600_000
      })
      expect(own.ok, `a worker must still arm its OWN reminder: ${own.message}`).toBe(true)

      const upward = await workerSurface.call('org_create_reminder', {
        personId: 'chief',
        prompt: 'A worker should not be able to schedule this.',
        // A LEGAL cadence, deliberately: this test is about AUTHORITY, and a
        // sub-floor interval would earn a refusal from the cadence fence
        // instead. It would still be `ok: false` — passing for a reason that
        // has nothing to do with what it claims to test.
        intervalMs: 600_000
      })
      expect(upward.ok, 'a worker arming a reminder on the CEO must be refused').toBe(false)
      expect(
        upward.message,
        'the refusal must NAME its cause — an unnamed one sends the agent back around ' +
          'a retry ladder to be refused identically for ever'
      ).toMatch(/does not manage/i)

      // Reading is fenced by the same rule, so the worker cannot enumerate
      // the CEO's reminders either.
      const peek = await workerSurface.call('org_list_reminders', { personId: 'chief' })
      expect(peek.ok, "a worker listing the CEO's reminders must be refused").toBe(false)
      expect(peek.message).toMatch(/does not manage/i)

      // NOTHING was written behind the refusal. Read through the CEO's own
      // surface, which is the authority the worker tried to reach.
      const ceoReminders = await surface.call('org_list_reminders', {})
      expect(ceoReminders.ok, `org_list_reminders failed: ${ceoReminders.message}`).toBe(true)
      expect(
        reminderPromptsOf(ceoReminders.details),
        `a refused arm must leave no reminder behind (refusal was: ${upward.message})`
      ).not.toContain('A worker should not be able to schedule this.')
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * THE STRUCTURAL FAMILY — the unit and headship verbs, through the tools.
 *
 * The head appointment is the keystone of the whole family: a head is
 * otherwise permanent (transfer and offboard both refuse one), so appointing a
 * successor is the only way a head ever moves. It is also the only tool in the
 * product that deliberately REFUSES its own happy path on the first call — it
 * returns the incumbent and demands an explicit disposition — which is a
 * behaviour that reads as a bug to anything that only checks `ok`.
 */
describe('the structural family, through the tools, against a live company', () => {
  it(
    'appoints a head, moves members, reparents, pauses and resumes — with each refusal twin',
    async () => {
      const ops = await surface.call('org_launch_department', {
        department: {
          name: 'Operations',
          purpose: 'Own the structural fixture.',
          head: { name: 'Ola', mandate: 'Lead operations.' },
          staff: [
            { name: 'Uri', mandate: 'Work operations.' },
            { name: 'Ivo', mandate: 'Also work operations.' }
          ]
        }
      })
      expect(ops.ok, `org_launch_department failed: ${ops.message}`).toBe(true)
      const depot = await surface.call('org_launch_department', {
        department: {
          name: 'Depot',
          purpose: 'Receive the structural fixture moves.',
          head: { name: 'Dov', mandate: 'Lead depot.' }
        }
      })
      expect(depot.ok, `org_launch_department failed: ${depot.message}`).toBe(true)

      const operations = await findDepartmentByName('Operations')
      const depotUnit = await findDepartmentByName('Depot')
      const operationsId = operations?.id ?? ''
      const depotId = depotUnit?.id ?? ''
      const incumbentHeadId = operations?.headPersonId ?? ''
      const members = (operations?.people ?? []).filter((person) => person.id !== incumbentHeadId)
      expect(members.length, 'the structural fixture needs two ordinary members').toBe(2)
      const [successor, mover] = members

      // ---- org_appoint_department_head: the FIRST call is a refusal --------
      // Not an error condition — a designed two-phase confirmation. The tool
      // hands back the incumbent and demands a disposition, and nothing is
      // written. Asserting the tree afterwards is what separates "it asked"
      // from "it asked and appointed anyway".
      const unconfirmed = await surface.call('org_appoint_department_head', {
        departmentId: operationsId,
        newHeadPersonId: successor?.id
      })
      expect(unconfirmed.ok, 'an appointment with no incumbent disposition must not apply').toBe(
        false
      )
      expect(unconfirmed.details.status).toBe('incumbent_disposition_required')
      expect(
        (await findDepartmentByName('Operations'))?.headPersonId,
        'the unconfirmed appointment must have written nothing'
      ).toBe(incumbentHeadId)

      // ---- refused: a successor who is not a member of that department -----
      const outsider = await surface.call('org_appoint_department_head', {
        departmentId: operationsId,
        newHeadPersonId: depotUnit?.headPersonId,
        incumbentDisposition: 'retain'
      })
      expect(outsider.ok, 'a successor from another department must be refused').toBe(false)
      expect(
        (await findDepartmentByName('Operations'))?.headPersonId,
        'a refused appointment must leave the sitting head in place'
      ).toBe(incumbentHeadId)

      // ---- allowed: the confirmed appointment ------------------------------
      const appointed = await surface.call('org_appoint_department_head', {
        departmentId: operationsId,
        newHeadPersonId: successor?.id,
        incumbentDisposition: 'retain'
      })
      expect(appointed.ok, `org_appoint_department_head failed: ${appointed.message}`).toBe(true)
      const afterAppointment = await findDepartmentByName('Operations')
      expect(afterAppointment?.headPersonId, 'the successor must now head the department').toBe(
        successor?.id
      )
      // `retain` is the disposition that decays silently: an implementation
      // that dropped the incumbent entirely would satisfy every assertion
      // above. The department must still hold all three people.
      expect(
        (afterAppointment?.people ?? []).map((person) => person.id).sort(),
        'a retained incumbent must still be a member of the department it used to head'
      ).toEqual([incumbentHeadId, mover?.id ?? '', successor?.id ?? ''].sort())

      // ---- org_move_department_members -------------------------------------
      const depotBefore = (await findDepartmentByName('Depot'))?.people.length ?? 0
      const moved = await surface.call('org_move_department_members', {
        fromDepartmentId: operationsId,
        toDepartmentId: depotId
      })
      expect(moved.ok, `org_move_department_members failed: ${moved.message}`).toBe(true)

      const operationsAfterMove = await findDepartmentByName('Operations')
      const depotAfterMove = await findDepartmentByName('Depot')
      // The whole promise of this verb is "everyone EXCEPT the head". A move
      // that took the head too would leave a department headless, and the tool
      // would still have reported a success.
      expect(
        (operationsAfterMove?.people ?? []).map((person) => person.id),
        'the source department head must stay behind'
      ).toEqual([successor?.id])
      expect(operationsAfterMove?.headPersonId).toBe(successor?.id)
      expect(
        (depotAfterMove?.people ?? []).map((person) => person.id).sort(),
        'both ordinary members must have arrived, and nobody else'
      ).toEqual([depotUnit?.headPersonId ?? '', incumbentHeadId, mover?.id ?? ''].sort())
      expect(depotAfterMove?.people.length).toBe(depotBefore + 2)

      // Refused: a source that does not exist. Named as an UNKNOWN department
      // rather than a scope problem — telling a manager they lack permission
      // over something that is not there sends them hunting a fault they do
      // not have.
      const unknownSource = await surface.call('org_move_department_members', {
        fromDepartmentId: 'no-such-department',
        toDepartmentId: depotId
      })
      expect(unknownSource.ok, 'moving members out of an unknown department must fail').toBe(false)
      expect(unknownSource.message).toMatch(/unknown department/i)

      // ---- org_reparent_department -----------------------------------------
      const depotParentBefore = (await findDepartmentByName('Depot'))?.parentId
      expect(
        depotParentBefore,
        'the fixture must start with depot somewhere other than under operations'
      ).not.toBe(operationsId)

      const reparented = await surface.call('org_reparent_department', {
        departmentId: depotId,
        newParentDepartmentId: operationsId
      })
      expect(reparented.ok, `org_reparent_department failed: ${reparented.message}`).toBe(true)
      // Read back off the NESTING, which is the only place this change exists:
      // the department is present in the tree before and after, and only its
      // position moves.
      expect(
        (await findDepartmentByName('Depot'))?.parentId,
        'the reparented department must hang under its new parent'
      ).toBe(operationsId)

      const unknownParent = await surface.call('org_reparent_department', {
        departmentId: depotId,
        newParentDepartmentId: 'no-such-parent'
      })
      expect(unknownParent.ok, 'reparenting under an unknown parent must fail').toBe(false)
      expect(
        (await findDepartmentByName('Depot'))?.parentId,
        'a refused reparent must leave the tree exactly where it was'
      ).toBe(operationsId)

      // ---- org_pause_department / org_resume_department ---------------------
      // Driven through the TOOLS in both directions. The suite already proves
      // `org_resume_departments` (the multi-unit verb) against a pause staged
      // by a route; these two are the single-unit aliases an agent actually
      // reaches for, and neither had ever been called.
      const paused = await surface.call('org_pause_department', { departmentId: depotId })
      expect(paused.ok, `org_pause_department failed: ${paused.message}`).toBe(true)
      expect(paused.message).not.toMatch(/unknown command/i)
      expect(
        (await findDepartmentByName('Depot'))?.state,
        'a paused department must read as paused in the durable tree'
      ).toBe('paused')

      const resumed = await surface.call('org_resume_department', { departmentId: depotId })
      expect(resumed.ok, `org_resume_department failed: ${resumed.message}`).toBe(true)
      expect(
        (await findDepartmentByName('Depot'))?.state,
        'a resumed department must read as active again'
      ).toBe('active')

      const pauseNothing = await surface.call('org_pause_department', {
        departmentId: 'no-such-department'
      })
      expect(pauseNothing.ok, 'pausing an unknown department must be refused').toBe(false)
      expect(pauseNothing.message).not.toMatch(/unknown command/i)
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * THE OPERATOR ESCALATION — the one tool that speaks OUTSIDE the organization.
 *
 * `org_escalate_to_operator` is registered only for the STRUCTURAL ROOT, and
 * the gate is computed once at install (#270). That makes its refusal twin an
 * ABSENCE rather than a rejection, exactly like the manager-only goal family:
 * the `if (directManagerId(...) !== undefined) throw` inside the verb cannot be
 * reached through a real install, so a test asserting it would be asserting a
 * branch the product cannot produce. A registration-set assertion also fails in
 * BOTH directions — a verb that stopped being root-only would appear on the
 * department head's surface below.
 */

/**
 * THE OPERATOR ESCALATION — the one tool that speaks OUTSIDE the organization.
 *
 * `org_escalate_to_operator` is registered only for the STRUCTURAL ROOT, and
 * the gate is computed once at install (#270). That makes its refusal twin an
 * ABSENCE rather than a rejection, exactly like the other manager-only verbs:
 * the `if (directManagerId(...) !== undefined) throw` inside the verb cannot be
 * reached through a real install, so a test asserting it would be asserting a
 * branch the product cannot produce. A registration-set assertion also fails in
 * BOTH directions — a verb that stopped being root-only would appear on the
 * department head's surface below.
 */
describe('the operator escalation, through the tool, against a live company', () => {
  it(
    'records one durable intent for the structural root, and is not registered for anybody else',
    async () => {
      const built = await surface.call('org_launch_department', {
        department: {
          name: 'Bridge',
          purpose: 'Own the escalation fixture.',
          head: { name: 'Nero', mandate: 'Lead the bridge.' }
        }
      })
      expect(built.ok, `org_launch_department failed: ${built.message}`).toBe(true)
      const bridgeHeadId = (await findDepartmentByName('Bridge'))?.headPersonId ?? ''
      expect(bridgeHeadId, 'the escalation fixture needs a real department head').not.toBe('')

      const headSurface = await surfaceFor(bridgeHeadId)
      expect(
        [...headSurface.tools.keys()],
        'a department head escalates to its own manager and must never be handed the ' +
          'out-of-band operator channel'
      ).not.toContain('org_escalate_to_operator')
      expect(
        [...surface.tools.keys()],
        'the CEO has no manager to escalate to, so the CEO is the one who gets it'
      ).toContain('org_escalate_to_operator')

      const blocker = 'The provider account has no billing method, so no pane can start.'
      const operatorAction = 'Add a billing method to the provider account.'
      expect(
        (await escalationIntents()).map((intent) => intent.blocker),
        'the queue must not already hold this blocker, or the assertion after the ' +
          'call cannot distinguish a write from a leftover'
      ).not.toContain(blocker)

      const raised = await surface.call('org_escalate_to_operator', { blocker, operatorAction })
      expect(raised.ok, `org_escalate_to_operator failed: ${raised.message}`).toBe(true)
      expect(raised.details.status).toBe('queued')
      const fingerprint = asText(raised.details.fingerprint)
      expect(fingerprint, 'the tool must report the durable key it wrote').not.toBe('')

      // DURABLE, off chiefd's own queue rather than the tool's receipt. The
      // escalation LOG is deliberately not read: it is written by the
      // protected supervision loop that drains this queue, and every
      // background timer in this fixture is 0, so the log would be empty here
      // whether the tool worked or not.
      const queued = (await escalationIntents()).filter(
        (intent) => intent.fingerprint === fingerprint
      )
      expect(queued.length, 'exactly one intent must have been enqueued').toBe(1)
      expect(queued[0]).toEqual({ fingerprint, personId: 'chief', blocker, operatorAction })

      // The tool's own description promises that re-recording the identical
      // blocker is a safe no-op. The queue is keyed by fingerprint, so the
      // promise is really two claims: the second call succeeds, AND it does
      // not enqueue a second copy of the same blocker.
      const again = await surface.call('org_escalate_to_operator', { blocker, operatorAction })
      expect(again.ok, `a repeated identical escalation must be a no-op: ${again.message}`).toBe(
        true
      )
      expect(asText(again.details.fingerprint)).toBe(fingerprint)
      expect(
        (await escalationIntents()).filter((intent) => intent.fingerprint === fingerprint).length,
        'a repeated identical escalation must not enqueue a second intent'
      ).toBe(1)

      // ---- refused: an escalation with no concrete blocker -----------------
      // The schema's `minLength: 1` cannot see this — three spaces satisfy it
      // — so the only check that can is the tool's own, and what it has to
      // protect is the QUEUE. An operator woken by an empty blocker has been
      // woken for nothing.
      const before = (await escalationIntents()).length
      const blank = await surface.call('org_escalate_to_operator', {
        blocker: '   ',
        operatorAction: 'Do something about nothing.'
      })
      expect(blank.ok, 'an escalation with no blocker must be refused').toBe(false)
      expect(blank.message).toMatch(/concrete blocker/i)
      expect(
        (await escalationIntents()).length,
        'a refused escalation must not enqueue anything'
      ).toBe(before)
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * THE BASELINE MAILBOX VERB every person carries, manager or not.
 *
 * `org_send` is not gated on management, so it has a real refusal reachable
 * through `execute` — and it is read back off the durable row chiefd owns
 * rather than off the tool's own answer, because "the message was queued" and
 * "the message is in the recipient's mailbox" are different claims and only
 * the second one is the product.
 */
describe('the baseline mailbox verb, through the tools', () => {
  it(
    'org_send lands in the recipient mailbox, and a recipient outside the company is refused',
    async () => {
      const built = await surface.call('org_launch_department', {
        department: {
          name: 'Postal',
          purpose: 'Own the mailbox fixture.',
          head: { name: 'Juno', mandate: 'Lead postal.' }
        }
      })
      expect(built.ok, `org_launch_department failed: ${built.message}`).toBe(true)
      const postalHeadId = (await findDepartmentByName('Postal'))?.headPersonId ?? ''
      expect(postalHeadId, 'the mailbox fixture needs a recipient').not.toBe('')

      const body = 'Confirm the retention schedule before Friday, then reply with the date.'
      expect(
        (await mailboxOf(postalHeadId)).map((message) => message.body),
        'the recipient must not already hold this message'
      ).not.toContain(body)

      const sent = await surface.call('org_send', { to: postalHeadId, body })
      expect(sent.ok, `org_send failed: ${sent.message}`).toBe(true)

      const delivered = await mailboxOf(postalHeadId)
      expect(
        delivered.filter((message) => message.body === body),
        'the message must be durable in the recipient mailbox, sent by the caller'
      ).toEqual([{ from: 'chief', body }])

      // ---- refused: a recipient outside the organization -------------------
      // "launcher" is the exact trap the tool's own description names, and the
      // refusal has to protect the MAILBOX, not merely return false: an
      // envelope written for a recipient who does not exist is a message
      // nobody will ever read and nobody will ever be told about.
      const beforeStray = (await mailboxOf(postalHeadId)).length
      const stray = await surface.call('org_send', {
        to: 'launcher',
        body: 'There is no such person in this company.'
      })
      expect(stray.ok, 'a recipient outside the organization must be refused').toBe(false)
      // `launcher` has a NAMED guard ahead of the general unknown-recipient
      // path, and its wording is the product: it says what `launcher` is and
      // what to do instead, rather than listing every employee.
      expect(stray.message).toMatch(/never a message recipient/i)
      expect(
        (await mailboxOf(postalHeadId)).length,
        'a refused send must not write an envelope anywhere'
      ).toBe(beforeStray)

      // ---- refused: a send with no message text ----------------------------
      // The schema's `minLength: 1` is satisfied by whitespace, so the check
      // that matters is the tool's own, and its answer must be actionable
      // guidance rather than a schema reject the agent cannot self-correct
      // from.
      const empty = await surface.call('org_send', { to: postalHeadId, body: '   ' })
      expect(empty.ok, 'a send with no message text must be refused').toBe(false)
      expect(empty.message).toMatch(/body/i)
      expect(
        (await mailboxOf(postalHeadId)).length,
        'a refused empty send must not write an envelope'
      ).toBe(beforeStray)
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * THE DEPARTMENT END-OF-LIFE FAMILY — create, stop, remove.
 *
 * `org_add_department` is the head-decision verb: a department cannot exist
 * without a head, so the tool refuses rather than inventing one, and the prompt
 * lives in the tool surface. `org_stop_department` and `org_remove_department`
 * are the other end of the same life, and the removal of a populated unit is
 * the highest-blast-radius call in the whole surface — it fires everybody in
 * the subtree. Its confirmation gate is exactly the kind of check that keeps
 * every test green after it is deleted, because deleting it makes MORE calls
 * succeed.
 *
 * Everything is read back off the composite-keyed structured tree, with
 * parentage derived from the nesting.
 */
describe('the department end-of-life family, through the tools', () => {
  it(
    'creates with an explicit head decision, stops, and refuses to fire a team without confirmation',
    async () => {
      // ---- refused: no head decision at all --------------------------------
      const namesBefore = await departmentNamesOf(company)
      const headless = await surface.call('org_add_department', {
        name: 'Annex',
        purpose: 'A department with nobody to lead it.'
      })
      expect(headless.ok, 'a department with no head decision must be refused').toBe(false)
      expect(headless.message).toMatch(/exactly one head decision/i)
      expect(
        await departmentNamesOf(company),
        'a refused create must not leave a department behind'
      ).toEqual(namesBefore)

      // ---- refused: an automatic org tool declared on the new head --------
      // Live P0: the model put `org_send` in `head.tools`, chiefd refused
      // `invalid department person seed: head.tools`, and the model concluded
      // that it should delete tool arrays. The array is only for redundant Pi
      // builtins; org_send is already composed into the person's live surface.
      const automaticTool = await surface.call('org_add_department', {
        name: 'Invalid Tool Annex',
        purpose: 'Must never reach the staffing route.',
        head: { name: 'Iris', mandate: 'Lead it.', tools: ['org_send'] }
      })
      expect(automaticTool.ok, 'org_send must not be accepted as a declared head tool').toBe(false)
      expect(automaticTool.message).toMatch(
        /Never put org_\* names.*installed automatically.*omit them/i
      )
      expect(
        await departmentNamesOf(company),
        'a refused automatic tool declaration must not leave a department behind'
      ).toEqual(namesBefore)

      // ---- allowed: hire a new head ----------------------------------------
      const created = await surface.call('org_add_department', {
        name: 'Annex',
        purpose: 'Own the department end-of-life fixture.',
        head: { name: 'Otto Klein', mandate: 'Lead the annex.' },
        staff: [{ name: 'Petra', mandate: 'Work the annex.' }]
      })
      expect(created.ok, `org_add_department failed: ${created.message}`).toBe(true)
      const annex = await findDepartmentByName('Annex')
      const annexId = annex?.id ?? ''
      const annexHeadId = annex?.headPersonId ?? ''
      expect(annexId, 'the created department must be in the durable tree').not.toBe('')
      expect(annexHeadId, 'a department cannot exist without a head').not.toBe('')
      expect(annex?.people.length, 'the head and the staff seed must both have committed').toBe(2)

      // ---- refused: BOTH head decisions at once ----------------------------
      const bothWays = await surface.call('org_add_department', {
        name: 'Annex Two',
        purpose: 'Two head decisions is not a decision.',
        head: { name: 'Rurik', mandate: 'Lead nothing.' },
        existingHeadPersonId: annexHeadId
      })
      expect(bothWays.ok, 'two head decisions must be refused').toBe(false)
      expect(bothWays.message).toMatch(/you gave both/i)
      expect(
        (await departmentNamesOf(company)).filter((name) => name === 'Annex Two'),
        'a refused create must not leave a department behind'
      ).toEqual([])

      // ---- allowed: promote an EXISTING person, who is transferred in ------
      const annexWorkerId = annex?.people.find((person) => person.id !== annexHeadId)?.id ?? ''
      expect(annexWorkerId, 'the fixture needs an existing person to promote').not.toBe('')
      const promoted = await surface.call('org_add_department', {
        name: 'Annex Two',
        purpose: 'Led by somebody who already worked here.',
        existingHeadPersonId: annexWorkerId
      })
      expect(promoted.ok, `an existing-head create failed: ${promoted.message}`).toBe(true)
      const annexTwo = await findDepartmentByName('Annex Two')
      expect(annexTwo?.headPersonId, 'the promoted person must now head the new department').toBe(
        annexWorkerId
      )
      // The transfer half of the same atomic change: they are no longer where
      // they were. A create that appointed without moving would leave one
      // person listed in two departments.
      expect(
        (await findDepartmentByName('Annex'))?.people.map((person) => person.id),
        'the promoted person must have been transferred out of their old department'
      ).toEqual([annexHeadId])

      // ---- refused: an existing head brought in WITH staff ------------------
      const withStaff = await surface.call('org_add_department', {
        name: 'Annex Three',
        purpose: 'An existing head starts head-only.',
        existingHeadPersonId: annexHeadId,
        staff: [{ name: 'Sirin', mandate: 'Arrive with the head.' }]
      })
      expect(withStaff.ok, 'an existing-head create with staff must be refused').toBe(false)
      // The refusal used to say "starts head-only". It says "takes no initial
      // staff" now, because creation no longer leaves anybody stopped and
      // "starts head-only" had come to read as a claim about who is RUNNING.
      // The rule is unchanged, so match what the sentence has to tell the
      // caller -- that no staff came with the head -- rather than the phrase.
      expect(withStaff.message).toMatch(/no initial staff/i)
      expect(
        (await departmentNamesOf(company)).filter((name) => name === 'Annex Three'),
        'a refused create must not leave a department behind'
      ).toEqual([])

      // ---- org_stop_department ---------------------------------------------
      // A stop IS a pause in chiefd's model: the unit stays on disk and its
      // availability moves. Read the durable state, not the sentence.
      const stopped = await surface.call('org_stop_department', { unitId: annexId })
      expect(stopped.ok, `org_stop_department failed: ${stopped.message}`).toBe(true)
      expect(stopped.message).not.toMatch(/unknown command/i)
      expect(
        (await findDepartmentByName('Annex'))?.state,
        'a stopped department must read as paused in the durable tree'
      ).toBe('paused')

      const resumedAnnex = await surface.call('org_resume_department', { departmentId: annexId })
      expect(resumedAnnex.ok, `org_resume_department failed: ${resumedAnnex.message}`).toBe(true)
      expect((await findDepartmentByName('Annex'))?.state).toBe('active')

      const stopNothing = await surface.call('org_stop_department', {
        unitId: 'no-such-department'
      })
      expect(stopNothing.ok, 'stopping an unknown department must be refused').toBe(false)
      expect(stopNothing.message).not.toMatch(/unknown command/i)

      // ---- org_remove_department: the confirmation gate ---------------------
      // Two people sit under Annex Two's parent chain; the first call must
      // refuse and NAME the blast radius. A gate whose deletion makes more
      // calls succeed is invisible to every test that only drives the happy
      // path, so the refusal is asserted before the confirmation.
      const populated = await surface.call('org_launch_department', {
        department: {
          name: 'Annex Four',
          purpose: 'Be removed, with people in it.',
          head: { name: 'Tova', mandate: 'Lead annex four.' },
          staff: [{ name: 'Ulf', mandate: 'Work annex four.' }]
        }
      })
      expect(populated.ok, `org_launch_department failed: ${populated.message}`).toBe(true)
      const annexFour = await findDepartmentByName('Annex Four')
      const annexFourId = annexFour?.id ?? ''
      const annexFourPeople = (annexFour?.people ?? []).map((person) => person.id)
      expect(annexFourId, 'the removal fixture must exist').not.toBe('')
      expect(annexFourPeople.length, 'the removal fixture needs a head and a member').toBe(2)

      const unconfirmed = await surface.call('org_remove_department', { unitId: annexFourId })
      expect(unconfirmed.ok, 'removing a populated department must ask first').toBe(false)
      expect(
        unconfirmed.message,
        'the refusal must name who would be fired and the two ways out'
      ).toMatch(/confirmImpact: true/)
      expect(unconfirmed.message).toMatch(/Ulf/)
      expect(
        await departmentNamesOf(company),
        'an unconfirmed removal must have written nothing'
      ).toContain('Annex Four')

      const confirmed = await surface.call('org_remove_department', {
        unitId: annexFourId,
        confirmImpact: true
      })
      expect(confirmed.ok, `org_remove_department failed: ${confirmed.message}`).toBe(true)
      expect(
        await departmentNamesOf(company),
        'a confirmed removal must delete the department from the durable tree'
      ).not.toContain('Annex Four')
      // The people went with it — FIRED, which in this product means
      // offboarded. A removal that deleted the unit and left its members
      // ACTIVE somewhere would satisfy every tree assertion above and leave a
      // company holding staff nobody manages, so the person records are read
      // off the manifest.
      //
      // This assertion replaces one that required the person rows to be GONE.
      // That was the product's real behaviour and it was the defect: a subtree
      // removal ran `DELETE FROM people` while `org_offboard` retained a
      // departed person's record, identity and audit trail, and the tool copy
      // called both "fires". Two paths a product treats as interchangeable —
      // `org_offboard`'s own description sends a sole-head firing here — must
      // mean the same thing, and the retaining one is right: `staffing_history`
      // carries no people FK precisely so a person's ledger outlives them, so
      // the delete did not erase the history, it left an orphaned `hired` row
      // with no `offboarded` row and nobody it belongs to.
      //
      // Both people, head included, are now departed members of the removed
      // unit's PARENT — which is what a company that remembers who worked for
      // it looks like, and is the same shape `org_offboard` leaves behind.
      const afterRemoval = await readManifest()
      const annexFourParentId = annexFour?.parentId ?? ''
      expect(
        annexFourParentId,
        'the removed unit must have a parent to absorb its leavers'
      ).not.toBe('')
      expect(
        annexFourPeople.map((personId) => manifestPerson(afterRemoval, personId)),
        'a fired person keeps their record; the removal deletes the unit, not the people'
      ).toEqual(
        annexFourPeople.map(() => ({
          employmentState: 'departed',
          departmentId: annexFourParentId
        }))
      )

      // A head-only unit removes in ONE call. This is the other half of the
      // gate's contract, and without it a gate that simply always refused
      // would pass every assertion above.
      const soloBuilt = await surface.call('org_launch_department', {
        department: {
          name: 'Annex Five',
          purpose: 'Be removed in one call.',
          head: { name: 'Vidar', mandate: 'Lead annex five alone.' }
        }
      })
      expect(soloBuilt.ok, `org_launch_department failed: ${soloBuilt.message}`).toBe(true)
      const soloId = (await findDepartmentByName('Annex Five'))?.id ?? ''
      const soloRemoved = await surface.call('org_remove_department', { unitId: soloId })
      expect(
        soloRemoved.ok,
        `a head-only removal must need no confirmation: ${soloRemoved.message}`
      ).toBe(true)
      expect(await departmentNamesOf(company)).not.toContain('Annex Five')
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * THE CONTRACT FAMILY — launch, stop, remove.
 *
 * A contract is a department row carrying transient engagement metadata, so
 * all three verbs share the department routes and the whole risk is the KIND.
 * `managedUnit` is the only thing standing between `org_remove_contract` and a
 * real department, and a kind check is invisible to a suite that only ever
 * hands each verb the right sort of id.
 *
 * The engagement is read off the durable manifest rather than the structured
 * tree on purpose: the tree is placement and identity, and deliberately
 * carries no unit kind. Placement and state still come from the tree.
 */
describe('the contract family, through the tools, against a live company', () => {
  it(
    'launches, stops, resumes and removes a contract — and refuses to treat a department as one',
    async () => {
      const host = await surface.call('org_launch_department', {
        department: {
          name: 'Charter',
          purpose: 'Host the contract fixture.',
          head: { name: 'Wynn', mandate: 'Lead charter.' }
        }
      })
      expect(host.ok, `org_launch_department failed: ${host.message}`).toBe(true)
      const charterId = (await findDepartmentByName('Charter'))?.id ?? ''
      expect(charterId, 'the contract fixture needs a parent unit').not.toBe('')

      const engagement = 'Ship the retention audit and hand the findings back.'
      const launched = await surface.call('org_launch_contract', {
        parentUnitId: charterId,
        contract: {
          name: 'Audit Engagement',
          purpose: 'Run one bounded audit.',
          head: {
            name: 'Xenia',
            mandate: 'Deliver the audit.'
          },
          transient: { engagement }
        }
      })
      expect(launched.ok, `org_launch_contract failed: ${launched.message}`).toBe(true)
      expect(launched.message).not.toMatch(/unknown command/i)

      const contract = await findDepartmentByName('Audit Engagement')
      const contractId = contract?.id ?? ''
      expect(contractId, 'the contract must be in the durable tree').not.toBe('')
      expect(
        contract?.parentId,
        'the contract must hang under the parent it was launched against'
      ).toBe(charterId)

      const manifest = await readManifest()
      expect(
        manifestUnitKind(manifest, contractId),
        'a contract is not a department, and the manifest is the only place that says so'
      ).toBe('contract')
      expect(
        manifestUnitEngagement(manifest, contractId),
        'the engagement is the whole reason a contract is a different unit'
      ).toBe(engagement)
      expect(
        manifestUnitKind(manifest, charterId),
        'the ordinary department must not have been turned into a contract'
      ).toBe('department')

      // ---- refused: a department is not a contract -------------------------
      // Both end-of-life verbs, because the kind check lives in the shared
      // helper and a regression would take both at once.
      const stopWrongKind = await surface.call('org_stop_contract', { unitId: charterId })
      expect(stopWrongKind.ok, 'stopping a department as a contract must be refused').toBe(false)
      expect(stopWrongKind.message).toMatch(/is a department, not a contract/i)
      expect(
        (await findDepartmentByName('Charter'))?.state,
        'a refused wrong-kind stop must leave the department running'
      ).toBe('active')

      const removeWrongKind = await surface.call('org_remove_contract', { unitId: charterId })
      expect(removeWrongKind.ok, 'removing a department as a contract must be refused').toBe(false)
      expect(removeWrongKind.message).toMatch(/is a department, not a contract/i)
      expect(
        await departmentNamesOf(company),
        'a refused wrong-kind removal must leave the department in the tree'
      ).toContain('Charter')

      // ---- refused: a launch that is both a create and a resume -------------
      const ambiguous = await surface.call('org_launch_contract', {
        parentUnitId: charterId,
        unitId: contractId,
        contract: {
          name: 'Ambiguous',
          purpose: 'Neither a create nor a resume.',
          head: { name: 'Yuri', mandate: 'Lead nothing.' },
          transient: { engagement: 'Nothing at all.' }
        }
      })
      expect(ambiguous.ok, 'a launch that is both a create and a resume must be refused').toBe(
        false
      )
      expect(ambiguous.message).toMatch(/exactly one/i)

      // ---- org_stop_contract, then the resume direction --------------------
      const stopped = await surface.call('org_stop_contract', { unitId: contractId })
      expect(stopped.ok, `org_stop_contract failed: ${stopped.message}`).toBe(true)
      expect(
        (await findDepartmentByName('Audit Engagement'))?.state,
        'a stopped contract must read as paused in the durable tree'
      ).toBe('paused')

      const resumed = await surface.call('org_launch_contract', {
        parentUnitId: charterId,
        unitId: contractId
      })
      expect(resumed.ok, `resuming a stopped contract failed: ${resumed.message}`).toBe(true)
      expect(
        (await findDepartmentByName('Audit Engagement'))?.state,
        'a resumed contract must read as active again'
      ).toBe('active')

      // ---- org_remove_contract ---------------------------------------------
      // A contract removal takes NO `confirmImpact` — the gate is department
      // only, by design, because a contract is bounded by construction.
      const removed = await surface.call('org_remove_contract', { unitId: contractId })
      expect(removed.ok, `org_remove_contract failed: ${removed.message}`).toBe(true)
      expect(
        await departmentNamesOf(company),
        'a removed contract must be gone from the durable tree'
      ).not.toContain('Audit Engagement')
      expect(
        manifestUnitKind(await readManifest(), contractId),
        'a removed contract must be gone from the durable manifest too'
      ).toBeUndefined()
      expect(
        await departmentNamesOf(company),
        'removing a contract must not take its parent with it'
      ).toContain('Charter')
    },
    TOOL_TIMEOUT_MS
  )
})

/**
 * STAFFING AND PLACEMENT — the locked product invariants, through the tools.
 *
 * `org_transfer`, `org_bench`/`org_recall` and `org_start_person`/
 * `org_stop_person` are the five verbs that move where a person lives and
 * whether they run. All five are product invariants, so all five are asserted
 * against a REAL `chiefd run` actuating a REAL tmux server (the whole file
 * is: `startTmuxHostedCompany` boots the daemon with its own private
 * `--runtime-socket`), and each durable claim is read back off the authority
 * that owns it:
 *
 *  * WHERE somebody lives -> the composite-keyed structured tree, whose people
 *    hang off the department they are placed in;
 *  * WHETHER they are employed -> `employmentState` on the durable manifest,
 *    which the tree deliberately does not carry (a bench is invisible in a
 *    projection that lists whoever is placed somewhere);
 *  * WHETHER they should be running -> `lastDesiredActive` on the activity
 *    ledger, the projection a later staffing verb is fenced against.
 *
 * # What the tmux assertion here can and cannot prove
 *
 * `tmuxPaneOwners()` reads the real server through chiefd's own ownership tag,
 * and it is asserted as an ABSENCE after every teardown. Stated plainly: on a
 * box with no provider credential a real `pi` exits within seconds of being
 * spawned and chiefd correctly reaps the pane, so that absence can be true for
 * a reason other than the tool -- which is exactly why the durable reads
 * above, not the pane, carry the proof. `scripts/proof/751-p4-teardown-live.ts`
 * is the instrument that asserts pane teardown physically, against a company
 * genesised on a native provider, and it records the same reasoning for every
 * other placement verb.
 */
describe('staffing and placement, through the tools, against a live tmux-hosted company', () => {
  it(
    'transfers, benches, recalls, starts and stops one person — with each refusal twin',
    async () => {
      const harbor = await surface.call('org_launch_department', {
        department: {
          name: 'Harbor',
          purpose: 'Own the staffing fixture.',
          head: { name: 'Kai', mandate: 'Lead harbor.' },
          staff: [{ name: 'Lena', mandate: 'Work harbor.' }]
        }
      })
      expect(harbor.ok, `org_launch_department failed: ${harbor.message}`).toBe(true)
      const quay = await surface.call('org_launch_department', {
        department: {
          name: 'Quay',
          purpose: 'Receive the staffing fixture transfer.',
          head: { name: 'Ines', mandate: 'Lead quay.' }
        }
      })
      expect(quay.ok, `org_launch_department failed: ${quay.message}`).toBe(true)

      const harborUnit = await findDepartmentByName('Harbor')
      const quayUnit = await findDepartmentByName('Quay')
      const harborId = harborUnit?.id ?? ''
      const quayId = quayUnit?.id ?? ''
      const harborHeadId = harborUnit?.headPersonId ?? ''
      const quayHeadId = quayUnit?.headPersonId ?? ''
      const workerId = harborUnit?.people.find((person) => person.id !== harborHeadId)?.id ?? ''
      expect(harborId, 'the staffing fixture needs a home department').not.toBe('')
      expect(quayId, 'the staffing fixture needs a destination department').not.toBe('')
      expect(workerId, 'the staffing fixture needs a worker to move').not.toBe('')

      const harborHeadSurface = await surfaceFor(harborHeadId)

      // ---- refused: a manager moving somebody they do not manage -----------
      const notMine = await harborHeadSurface.call('org_transfer', {
        personId: quayHeadId,
        departmentId: harborId
      })
      expect(notMine.ok, 'transferring somebody outside the scope must be refused').toBe(false)
      expect(notMine.message).toMatch(/does not manage/i)
      expect(
        await departmentOf(quayHeadId),
        'a refused transfer must leave the person exactly where they were'
      ).toBe(quayId)

      // ---- refused: a destination that does not exist -----------------------
      const nowhere = await surface.call('org_transfer', {
        personId: workerId,
        departmentId: 'no-such-department'
      })
      expect(nowhere.ok, 'transferring into an unknown department must be refused').toBe(false)
      expect(await departmentOf(workerId)).toBe(harborId)

      // ---- allowed: the permanent move -------------------------------------
      const transferred = await surface.call('org_transfer', {
        personId: workerId,
        departmentId: quayId
      })
      expect(transferred.ok, `org_transfer failed: ${transferred.message}`).toBe(true)
      expect(transferred.details.status).toBe('applied')
      expect(
        await departmentOf(workerId),
        'a transferred person must be placed in their new department'
      ).toBe(quayId)
      // The tree only shows WHERE somebody is placed, so it cannot tell a
      // moved home from a moved assignment. The manifest is where the promise
      // is kept: a transfer re-points BOTH.
      const afterTransfer = manifestPerson(await readManifest(), workerId)
      expect(
        afterTransfer?.departmentId,
        'a transfer moves the permanent HOME, not the assignment alone'
      ).toBe(quayId)
      expect(afterTransfer?.departmentId).toBe(quayId)

      // ---- org_bench / org_recall ------------------------------------------
      const quayHeadSurface = await surfaceFor(quayHeadId)
      const benchNotMine = await harborHeadSurface.call('org_bench', { personId: workerId })
      expect(
        benchNotMine.ok,
        'benching somebody who has transferred out of your scope must be refused'
      ).toBe(false)
      expect(benchNotMine.message).toMatch(/does not manage/i)
      expect(
        manifestPerson(await readManifest(), workerId)?.employmentState,
        'a refused bench must leave employment exactly as it was'
      ).toBe('active')

      // REGRESSION (found by this family): `/v1/org/person/bench-lifecycle`
      // held its answer for 30s waiting for convergence while every client of
      // it aborts at 10s, so a committed bench came back to the manager as
      // `chiefd unavailable (timeout)` -- a client-side abort carries no
      // `status`, so the tool's own `status === 503` recovery could not match
      // it. A bench that committed must never be reported as a failure.
      const benched = await quayHeadSurface.call('org_bench', { personId: workerId })
      expect(benched.ok, `org_bench failed: ${benched.message}`).toBe(true)
      expect(
        manifestPerson(await readManifest(), workerId)?.employmentState,
        'a benched person must read as benched in the durable manifest'
      ).toBe('benched')
      expect(
        tmuxPaneOwners(),
        'a benched person must not own a pane on the live tmux server'
      ).not.toContain(workerId)

      const recalled = await quayHeadSurface.call('org_recall', { personId: workerId })
      expect(recalled.ok, `org_recall failed: ${recalled.message}`).toBe(true)
      expect(
        manifestPerson(await readManifest(), workerId)?.employmentState,
        'a recalled person must be back in active employment'
      ).toBe('active')

      // `already-active` is chiefd answering "there was nothing to do", which
      // for a recall is the caller's desired end state. Reported as a success
      // that says so, never as a failure -- the #141 shape, in this family.
      const recalledAgain = await quayHeadSurface.call('org_recall', { personId: workerId })
      expect(
        recalledAgain.ok,
        `recalling an already-active person must not fail: ${recalledAgain.message}`
      ).toBe(true)
      expect(recalledAgain.details.alreadyActive).toBe(true)

      // ---- org_start_person / org_stop_person -------------------------------
      const startNotMine = await harborHeadSurface.call('org_start_person', {
        personId: workerId,
        reason: 'Not this head to start.'
      })
      expect(startNotMine.ok, 'starting somebody outside the scope must be refused').toBe(false)
      expect(startNotMine.message).toMatch(/does not manage/i)

      // THE PROMISE, asserted as a BICONDITIONAL rather than as a success.
      //
      // `/v1/org/person/start` is fail-closed by design: it materializes the
      // roster, asks whether the person could actually be launched, and
      // refuses `person-not-launchable` with "Nothing was written" when the
      // answer is no. Whether the answer is yes depends on the BOX -- a
      // machine with no configured Pi provider credential cannot stage one, so
      // there is no start-succeeds assertion that is true both here and on a
      // developer machine that has one, and writing either single-sided
      // assertion would make this test lie on half the world's laptops.
      //
      // What IS true on every box is the promise the route was rebuilt around:
      // starting somebody is a promise that a pane is coming, so the durable
      // demand exists EXACTLY when the tool said the start applied. This
      // route used to commit `active`, a launch fence and the demand, answer
      // `{"applied": true}`, and leave the actuator to discover the person had
      // no home; the assertion below is red against that behaviour and red
      // against the inverse, a success that raised nothing.
      const demandBeforeStart = (await desiredActive(workerId)) === true
      const quayHeadDemandBeforeStart = await desiredActive(quayHeadId)
      const started = await quayHeadSurface.call('org_start_person', {
        personId: workerId,
        reason: 'There is audit work for exactly one person right now.'
      })
      expect(
        (await desiredActive(workerId)) === true,
        `org_start_person answered ok=${started.ok} (${started.message}) — a start that ` +
          'applied must have raised durable demand, and one that refused must have ' +
          'written nothing at all'
      ).toBe(started.ok ? true : demandBeforeStart)
      // Only this person, either way. The whole product of the verb is that it
      // does not start a department, and a start that raised demand for the
      // head too would report exactly the same success.
      //
      // Asserted as "unchanged BY THE START", not as "not running". The head is
      // running, and correctly so: creating a department now brings its head up
      // (`org_ops::create_department_with_staff_unit` writes the launch fence in
      // the same transaction), so `not.toBe(true)` had stopped meaning "this
      // verb touched nobody else" and started meaning "the department was never
      // launched". Comparing against the value captured before the call says
      // the thing the test is actually for, and is red against a start that
      // raises demand for a head who was NOT already up.
      expect(
        await desiredActive(quayHeadId),
        'starting one person must never change demand for anybody else'
      ).toBe(quayHeadDemandBeforeStart)

      const demandBeforeRefusedStop = (await desiredActive(workerId)) === true
      const stopNotMine = await harborHeadSurface.call('org_stop_person', {
        personId: workerId,
        reason: 'Not this head to stop.'
      })
      expect(stopNotMine.ok, 'stopping somebody outside the scope must be refused').toBe(false)
      expect(stopNotMine.message).toMatch(/does not manage/i)
      expect(
        (await desiredActive(workerId)) === true,
        'a refused stop must leave the durable demand exactly as it was'
      ).toBe(demandBeforeRefusedStop)

      const stopped = await quayHeadSurface.call('org_stop_person', {
        personId: workerId,
        reason: 'The audit work is done.'
      })
      expect(stopped.ok, `org_stop_person failed: ${stopped.message}`).toBe(true)
      expect(stopped.details.status).toBe('applied')
      expect(
        (await desiredActive(workerId)) === true,
        'a person who has been stood down must carry no durable demand'
      ).toBe(false)
      expect(
        manifestPerson(await readManifest(), workerId)?.employmentState,
        'a commanded stop is a park, never a bench: they stay employed'
      ).toBe('active')
      expect(
        tmuxPaneOwners(),
        'a person who has been stood down must not own a pane on the live tmux server'
      ).not.toContain(workerId)
    },
    TOOL_TIMEOUT_MS
  )

  it(
    'benches, recalls and stands down SEVERAL people in one call — and refuses the whole batch on one bad target',
    async () => {
      // BATCHING, and why it is not ergonomics. chiefd runs ONE writer thread
      // per company and every mutation takes BEGIN IMMEDIATE, so N parallel
      // tool calls do not run concurrently — they queue, and the later ones
      // exceed the client's 35s patience while still waiting their turn. An
      // operator hit exactly this with fifteen parallel hires and got fifteen
      // `chiefd unavailable (timeout)`. N sequential mutations inside ONE call
      // never form that queue.
      //
      // A timeout is also the CALLER giving up rather than the server
      // cancelling, so the parallel shape can leave work committed that its
      // caller was told had failed — benching is where that bites, because the
      // retry it invites answers `already-benched`.
      const marina = await surface.call('org_launch_department', {
        department: {
          name: 'Marina',
          purpose: 'Own the batch staffing fixture.',
          head: { name: 'Nils', mandate: 'Lead marina.' },
          staff: [
            { name: 'Ottilie', mandate: 'Work marina one.' },
            { name: 'Runa', mandate: 'Work marina two.' }
          ]
        }
      })
      expect(marina.ok, `org_launch_department failed: ${marina.message}`).toBe(true)
      const slipway = await surface.call('org_launch_department', {
        department: {
          name: 'Slipway',
          purpose: 'Hold somebody the marina head does not manage.',
          head: { name: 'Quintus', mandate: 'Lead slipway.' }
        }
      })
      expect(slipway.ok, `org_launch_department failed: ${slipway.message}`).toBe(true)

      const marinaUnit = await findDepartmentByName('Marina')
      const marinaHeadId = marinaUnit?.headPersonId ?? ''
      const crew = (marinaUnit?.people ?? [])
        .map((person) => person.id)
        .filter((id) => id !== marinaHeadId)
      const outsiderId = (await findDepartmentByName('Slipway'))?.headPersonId ?? ''
      expect(crew.length, 'the batch fixture needs two workers to move together').toBe(2)
      expect(
        outsiderId,
        'the batch fixture needs somebody outside the marina head’s scope'
      ).not.toBe('')

      // ---- ALL-OR-NOTHING: every target is checked before any is mutated ----
      //
      // This is the assertion the batch shape exists to earn. A bad target in
      // the LAST position must refuse the batch, not leave the people ahead of
      // it benched — a half-applied batch is worse than no batch at all,
      // because the operator cannot tell from the failure what landed.
      const marinaHeadSurface = await surfaceFor(marinaHeadId)
      const mixedScope = await marinaHeadSurface.call('org_bench', {
        personIds: [crew[0], outsiderId]
      })
      expect(
        mixedScope.ok,
        'a batch naming one person outside the caller’s scope must be refused'
      ).toBe(false)
      expect(mixedScope.message).toMatch(/does not manage/i)
      const afterRefusal = await readManifest()
      expect(
        manifestPerson(afterRefusal, crew[0])?.employmentState,
        'the person ahead of the bad target must NOT have been benched — the batch ' +
          'preflights every target before it mutates any of them'
      ).toBe('active')

      // ---- the batch bench --------------------------------------------------
      const benched = await surface.call('org_bench', { personIds: crew })
      expect(benched.ok, `batch org_bench failed: ${benched.message}`).toBe(true)
      expect(benched.message).toMatch(/Benched 2 people/)
      expect(
        benched.details.applied,
        'the result must name every person benched — an operator checking a batch is ' +
          'checking the count, and a partial answer is how a retry re-benches'
      ).toHaveLength(2)
      const afterBench = await readManifest()
      for (const personId of crew) {
        expect(
          manifestPerson(afterBench, personId)?.employmentState,
          `${personId} must read as benched in the durable manifest`
        ).toBe('benched')
      }
      expect(
        manifestPerson(afterBench, marinaHeadId)?.employmentState,
        'benching a named list must never touch anybody who was not named'
      ).toBe('active')

      // ---- the batch recall -------------------------------------------------
      const recalled = await surface.call('org_recall', { personIds: crew })
      expect(recalled.ok, `batch org_recall failed: ${recalled.message}`).toBe(true)
      expect(recalled.message).toMatch(/Recalled 2 people/)
      const afterRecall = await readManifest()
      for (const personId of crew) {
        expect(
          manifestPerson(afterRecall, personId)?.employmentState,
          `${personId} must be back in active employment`
        ).toBe('active')
      }

      // ---- the batch stand-down ---------------------------------------------
      const stoodDown = await surface.call('org_stop_person', {
        personIds: crew,
        reason: 'The marina work is done for both of them.'
      })
      expect(stoodDown.ok, `batch org_stop_person failed: ${stoodDown.message}`).toBe(true)
      expect(stoodDown.message).toMatch(/Stood 2 people down/)
      // THIS is what pins the unnamed-person invariant: a named batch acts on
      // the named people and nobody else. `applied` is what the batch WROTE —
      // had the unnamed marina head been touched it would read 3. Do not
      // "restore" a bystander check that re-reads `desiredActive`: that value
      // is DERIVED and convergence recomputes it, so comparing it across the
      // call window measures a settling pass, not the batch.
      expect(stoodDown.details.applied).toHaveLength(2)
      for (const personId of crew) {
        expect(
          (await desiredActive(personId)) === true,
          `${personId} was stood down and must carry no durable demand`
        ).toBe(false)
        expect(
          manifestPerson(await readManifest(), personId)?.employmentState,
          'a commanded stop is a park, never a bench: they stay employed'
        ).toBe('active')
      }
    },
    TOOL_TIMEOUT_MS
  )
})
