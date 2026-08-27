// The in-process agent registry: reuse, replacement, and teardown.
//
// These three properties are the whole reason the registry exists, and each
// one is a defect if it goes the other way:
//
//   - reuse: an agent rebuilt per request loses the conversation between the
//     message and the reply;
//   - replacement: an agent whose profile changed but whose harness was reused
//     keeps answering as its previous self, on the old model or cwd;
//   - teardown: a company whose roster shrank but whose harnesses stayed keeps
//     hosting people chiefd no longer wants running — a second roster, which
//     is exactly the defect that made every talk verb 409 while suites stayed
//     green.
//
// The registry's decisions are the first half. The SECOND half is transcript
// storage, and it is built for real below — the REAL `operatorRoute()` over a
// real fixture agent dir, a real person root on disk — because the defect it
// pins was invisible to every mock: `JsonlSessionStorage.open` refuses a file
// that does not exist, and only a real filesystem says so.
//
// The THIRD half is what the harness is built WITH, and it is the defect this
// registry shipped for longest: chiefd's `tools` and `displayName` were both
// dropped here. A hosted agent therefore had no tools and no identity — asked
// what company it ran, the CEO of `webproof-labs` answered "I don't run any
// company — I'm Claude, an AI assistant created by Anthropic". Nothing on any
// surface said so: the person was hosted, the pane was up, and only a
// conversation revealed that nobody was home.
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import type { AgentHarness, AgentHarnessOptions } from '@earendil-works/pi-agent-core'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  hostAgent,
  hostedPeople,
  hostedSession,
  replaceSession,
  retainDesired,
  unavailableTools
} from '@/server/AgentHost'
import { operatorRoute } from '@/server/OperatorPi'
import { sessionsDir } from '@/server/PiHome'
import type { AgentProfile } from '@/types/AgentHost'
import type { OperatorRoute } from '@/types/OperatorPi'
import { isNullish } from '@/utils/Nullish'

/** What every harness this file builds was CONSTRUCTED with.
 *
 * `AgentHarness.systemPrompt` is private and there is no getter, so the
 * constructor is the only place the identity prompt is observable at all —
 * and "the harness was built with a system prompt" is precisely the fact that
 * was false in production. Recorded through a subclass of the REAL harness
 * rather than a stub: every other assertion in this file (thinking level,
 * transcript storage, reuse identity) runs against Pi's own behaviour, and a
 * fake would quietly retire all of them. */
const built = vi.hoisted((): { systemPrompt: string; toolNames: string[] }[] => [])

vi.mock('@earendil-works/pi-agent-core', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@earendil-works/pi-agent-core')>()
  // Bound to a NAMED type before it is extended. Extending
  // `actual.AgentHarness` directly makes TypeScript infer the base
  // constructor's parameter as `ConstructorParameters<typeof …>[0]`, which is
  // the indexed access `lucy/no-indexed-type-access` forbids — and the rule is
  // right: the interface has a name, and reaching it through a tuple index
  // hides which type the subclass is actually bound to.
  const BaseHarness: typeof AgentHarness = actual.AgentHarness
  class RecordingHarness extends BaseHarness {
    constructor(options: AgentHarnessOptions) {
      super(options)
      const prompt = options.systemPrompt
      // A callback prompt is recorded as empty rather than resolved: this host
      // passes a plain string, and a silent coercion would let a harness built
      // with NO prompt satisfy the assertions below.
      built.push({
        systemPrompt: typeof prompt === 'string' ? prompt : '',
        toolNames: (options.tools ?? []).map((tool) => tool.name)
      })
    }
  }
  return { ...actual, AgentHarness: RecordingHarness }
})

/** Every harness this file bound its extension tools to.
 *
 * The binding is late by construction — the harness is built FROM the tools —
 * so "was it bound at all, and to the right harness" is a fact only the
 * registry can be asked. Unbound, a tool's circuit breaker cannot stop the
 * turn it is trying to stop. */
const bound = vi.hoisted((): unknown[] => [])

/** Every lifecycle boundary the registry drove, in order.
 *
 * The registry owns the two Pi session events no harness event can supply, and
 * a person is only alive between them: `session_start` is what opens their SSE
 * subscription and `session_shutdown` is what closes it. A registry that
 * dropped a harness without stopping it would leave an offboarded person
 * draining a mailbox into an object nobody holds. */
const lifecycleCalls = vi.hoisted((): string[] => [])

/** The reading the mocked driver publishes, so a test can decide what a hosted
 *  person's snapshot says without owning a harness. */
const hostedUsage = vi.hoisted(
  (): {
    reading: { tokens: number | null; contextWindow: number; percent: number | null } | undefined
    /** When the driver last took [`reading`]. The registry reads it out beside
     * the reading, because a snapshot that cannot say its age is
     * indistinguishable from a live one. */
    asOf: number | undefined
  } => ({
    reading: { tokens: 90_000, contextWindow: 100_000, percent: 90 },
    asOf: Date.parse('2026-08-10T20:00:00.000Z')
  })
)

// The extension install is the ONE thing in this file that is not real. It
// reads the company manifest from that person's live daemon, which is a
// running company; every other seam here (models, storage, harness) is the
// product's own. What the adapter does with what it installs is proven in
// `ExtensionTools.test.ts`, and end to end in `browser-org-tools-check.mjs`.
vi.mock('@/server/ExtensionTools', () => ({
  HOSTED_EXTENSIONS: [],
  installExtensions: () =>
    Promise.resolve({
      tools: new Map(),
      refusedHandlers: [],
      bind: (subject: { harness: unknown }) => {
        bound.push(subject.harness)
        return {
          context: {},
          // The driver's snapshot, as the registry reads it out. `hosted.ts`
          // proves the snapshot itself; what this file proves is that the
          // registry reaches the RIGHT person's driver and answers nothing at
          // all for a person it does not hold.
          contextUsage: () => hostedUsage.reading,
          contextUsageAsOf: () => hostedUsage.asOf,
          start: (reason: string) => {
            lifecycleCalls.push(`start:${reason}`)
            return Promise.resolve()
          },
          shutdown: (reason: string) => {
            lifecycleCalls.push(`shutdown:${reason}`)
            return Promise.resolve()
          }
        }
      }
    })
}))

afterEach(() => {
  vi.unstubAllEnvs()
})

describe('AgentHost registry', () => {
  it('hosts nobody for a company that has never been touched', () => {
    expect(hostedPeople('never-seen')).toEqual([])
  })

  it('retainDesired on an untouched company is a no-op, not an error', async () => {
    // Convergence runs before anyone has opened a pane, so the empty case is
    // the common one rather than an edge.
    await expect(retainDesired('never-seen', ['ceo'])).resolves.toBeUndefined()
    expect(hostedPeople('never-seen')).toEqual([])
  })

  it('never reports another company’s people', async () => {
    // The registry is keyed by `${companyKey}\0${personId}`; a prefix match
    // that forgot the separator would let `cobalt` claim `cobalt-two`'s agents.
    await retainDesired('cobalt', [])
    expect(hostedPeople('cobalt-two')).toEqual([])
  })
})

/** A person root laid out the way chiefd materializes one: `workspace/` beside
 * `pi-home/`. Real files rather than a mocked storage seam — the whole defect
 * below is that a real `open` refuses a file that is not there.
 *
 * NOTHING ABOUT A ROUTE IS ON IT. A person carries no provider, no model and
 * no credential of their own: chief is out of that business, and every agent
 * runs on the operator's own Pi defaults resolved once by
 * {@link operatorRouteFixture}.
 *
 * `tools` and `displayName` are on the profile because chiefd's launch profile
 * carries both and this host is required to use them. They were absent from
 * this fixture for as long as they were absent from the harness. */
function person(): { profile: AgentProfile; home: string } {
  const root = mkdtempSync(join(tmpdir(), 'agent-host-'))
  const home = join(root, 'pi-home')
  mkdirSync(home, { recursive: true })
  mkdirSync(join(root, 'workspace'), { recursive: true })
  return {
    profile: {
      personId: 'ada',
      cwd: join(root, 'workspace'),
      env: { ORG_PERSON_ID: 'ada' },
      tools: ['read', 'bash'],
      displayName: 'Acme · Engineer'
    },
    home
  }
}

/**
 * THE ROUTE EVERY HOSTED PERSON RUNS ON: the operator's OWN Pi defaults.
 *
 * The real `operatorRoute()` over a real Pi agent dir on disk — not a stub,
 * because the rule it pins is that chief chooses nothing. There is no
 * per-person catalog to build and no per-person credential to scope: one
 * `settings.json` names the route, one `auth.json` credentials it, and every
 * agent this file hosts gets that same pair.
 */
async function operatorRouteFixture(): Promise<OperatorRoute> {
  const agentDir = mkdtempSync(join(tmpdir(), 'operator-pi-'))
  writeFileSync(
    join(agentDir, 'settings.json'),
    '{"defaultProvider":"openrouter","defaultModel":"fixture-model-a"}'
  )
  writeFileSync(join(agentDir, 'auth.json'), '{"openrouter":{"type":"api_key","key":"fixture"}}')
  writeFileSync(
    join(agentDir, 'models.json'),
    '{"providers":{"openrouter":{"baseUrl":"https://openrouter.ai/api/v1",' +
      '"api":"openai-completions","models":[{"id":"fixture-model-a"}]}}}'
  )
  // `vi.stubEnv` rather than assigning `process.env` directly: apps/web's
  // `lucy/no-process-env` keeps every environment read in `common/Env`, and a
  // test reaching around it would be the first exception in the app.
  vi.stubEnv('PI_SOURCE_AGENT_DIR', agentDir)
  const route = await operatorRoute()
  if (isNullish(route)) throw new Error('the fixture operator Pi must resolve a route')
  return route
}

/** Every `.jsonl` in the directory chiefd scans. */
function transcripts(profile: AgentProfile): string[] {
  const directory = sessionsDir(profile.cwd)
  return readdirSync(directory).filter((entry) => entry.endsWith('.jsonl'))
}

describe('AgentHost transcript storage', () => {
  it('CREATES a transcript for somebody who has never spoken', async () => {
    // THIS IS THE DEFECT THAT MEANT NO AGENT HAD EVER REPLIED THROUGH THE WEB.
    //
    // `JsonlSessionStorage.open` REFUSES a file that does not exist, and chiefd
    // leaves `sessionFile` absent for "somebody who has never spoken" — which
    // is every person in a company nobody has talked to yet. The host called
    // `open` unconditionally, so the first turn of every fresh agent failed
    // `ENOENT … session.jsonl`, so `GET /people` answered `hosted: []` for a
    // company that was running perfectly.
    const { profile } = person()

    await hostAgent('fresh', profile, await operatorRouteFixture())

    expect(transcripts(profile)).toHaveLength(1)
    expect(hostedPeople('fresh')).toEqual(['ada'])
  })

  it('creates it where chiefd LOOKS, not beside the workspace', async () => {
    // The old fallback wrote `<cwd>/session.jsonl`, which chiefd never reads.
    // `resource_catalog::latest_session` scans `<pi-home>/sessions/` and
    // resumes the newest `.jsonl`, so a transcript written beside the workspace
    // is invisible to the daemon and to the CLI pane: the web would hold one
    // conversation and tmux another with the same agent, each unaware of the
    // other. Creating it where chiefd looks means the next converge hands that
    // exact file back as `sessionFile`.
    const { profile, home } = person()

    await hostAgent('placed', profile, await operatorRouteFixture())

    const [file] = transcripts(profile)
    expect(file).toBeDefined()
    expect(sessionsDir(profile.cwd)).toBe(join(home, 'sessions'))
    // Nothing was written into the agent's own working tree.
    expect(readdirSync(profile.cwd)).toEqual([])
  })

  it('OPENS the transcript a person already has instead of truncating it', async () => {
    // `open`, never `create`, for a transcript that exists: creating over one
    // would throw away the company's own history the moment a second request
    // arrived.
    const { profile } = person()
    const models = await operatorRouteFixture()
    await hostAgent('resumed', profile, models)
    const [created] = transcripts(profile)
    if (typeof created !== 'string') throw new Error('the first host must create a transcript')
    const path = join(sessionsDir(profile.cwd), created)
    const before = readFileSync(path, 'utf8')

    // chiefd's next converge names the file it just found.
    await hostAgent('resumed', { ...profile, sessionFile: path }, models)

    expect(transcripts(profile)).toEqual([created])
    expect(readFileSync(path, 'utf8')).toBe(before)
  })

  it('holds the SAME session the harness writes to, for a transcript read', async () => {
    // `AgentHarness.session` is private, so a transcript read has no other way
    // to reach the entries the agent is appending. A second `Session` over the
    // same file would answer from its own view and show a conversation one turn
    // behind the one the operator just had.
    const { profile } = person()

    await hostAgent('sessioned', profile, await operatorRouteFixture())

    expect(hostedSession('sessioned', 'ada')).toBeDefined()
    expect(hostedSession('sessioned', 'nobody')).toBeUndefined()
  })

  it('builds the harness on the route Pi resolved, choosing nothing of its own', async () => {
    // THE RULE. This host used to pass chiefd's per-person thinking level and
    // per-person model into the constructor; both are deleted, so the harness
    // runs on the operator's own Pi defaults exactly as a pane does. Read back
    // off the harness rather than off the fixture, so this asserts what the
    // agent is actually built with.
    const route = await operatorRouteFixture()
    const { profile } = person()

    const harness = await hostAgent('thinking', profile, route)

    expect(harness.getModel().id).toBe(route.model.id)
    expect(harness.getModel().provider).toBe(route.provider)
  })

  it('reuses one harness per person and rebuilds when the profile changes', async () => {
    // Reuse: an agent rebuilt per request loses the conversation between the
    // message and the reply. Replacement: an agent whose profile changed but
    // whose harness was reused keeps answering as its previous self.
    const { profile } = person()
    const models = await operatorRouteFixture()

    const first = await hostAgent('reuse', profile, models)
    expect(await hostAgent('reuse', profile, models)).toBe(first)

    const rebuilt = await hostAgent('reuse', { ...profile, cwd: `${profile.cwd}-moved` }, models)
    expect(rebuilt).not.toBe(first)
  })

  it('reuses the harness when chiefd names the transcript this host just created', async () => {
    // THE DEFECT: a repeat turn in a long-lived pane answered 200 in about a
    // second and appended NOTHING.
    //
    // The first time a person is hosted chiefd reports no `sessionFile` and
    // this host creates one. On the very next converge chiefd reports that
    // exact file, so comparing `sessionFile` by field equality called the same
    // agent a different agent and rebuilt the harness — a second
    // `JsonlSessionStorage` over one file, each with its own in-memory view.
    // The turn then ran against a harness whose storage had already been
    // superseded, which is the transcript fork this module's own header warns
    // about. Comparing against the path actually in use makes chiefd naming
    // our own file the no-op it is.
    const { profile } = person()
    const models = await operatorRouteFixture()

    const first = await hostAgent('settled', profile, models)
    const [created] = transcripts(profile)
    if (typeof created !== 'string') throw new Error('the first host must create a transcript')
    const path = join(sessionsDir(profile.cwd), created)

    // chiefd's next converge hands back the file this host created.
    expect(await hostAgent('settled', { ...profile, sessionFile: path }, models)).toBe(first)
  })

  it('still rebuilds for a transcript that is not the one in use', async () => {
    // The other half: the comparison must not become "any session file is our
    // session file". A profile naming a DIFFERENT transcript is a different
    // agent, and reusing the harness would leave it appending to a file
    // nobody asked for.
    const { profile } = person()
    const models = await operatorRouteFixture()

    const first = await hostAgent('reseated', profile, models)
    const [created] = transcripts(profile)
    if (typeof created !== 'string') throw new Error('the first host must create a transcript')
    // A real second transcript, copied from the first so `open` has a valid
    // file to resume: `JsonlSessionStorage.open` refuses one that is not there.
    const other = join(sessionsDir(profile.cwd), 'other.jsonl')
    writeFileSync(other, readFileSync(join(sessionsDir(profile.cwd), created)))

    expect(await hostAgent('reseated', { ...profile, sessionFile: other }, models)).not.toBe(first)
  })
})

// What the harness is BUILT WITH — chiefd's tools and chiefd's identity.
//
// THE DEFECT THIS BLOCK PINS: both were dropped between the roster and the
// harness. The agent could talk and could do nothing, and it answered as a
// general-purpose assistant rather than as the person chiefd staffed. There is
// no symptom for either one until somebody asks the agent to do its job, which
// is why it survived every green suite in this repository.
describe('AgentHost tools and identity', () => {
  it('builds the harness with the tools chiefd granted, in chiefd’s order', async () => {
    // The harness used to be built with NO tools at all. Read back off the
    // harness rather than off the recording, so this asserts what the agent
    // can actually call.
    const { profile } = person()

    const harness = await hostAgent(
      'toolful',
      { ...profile, tools: ['bash', 'read', 'ls'] },
      await operatorRouteFixture()
    )

    expect(harness.getTools().map((tool) => tool.name)).toEqual(['bash', 'read', 'ls'])
  })

  it('builds the harness with a system prompt naming the person', async () => {
    // "I don't run any company — I'm Claude, an AI assistant created by
    // Anthropic" was a real answer from a real CEO of a real company, because
    // the harness was constructed with no system prompt at all.
    const { profile } = person()
    const before = built.length

    await hostAgent(
      'named',
      { ...profile, displayName: 'webproof-labs · CEO' },
      await operatorRouteFixture()
    )

    const record = built.at(-1)
    expect(built.length).toBe(before + 1)
    if (isNullish(record)) throw new Error('hosting a person must construct a harness')
    expect(record.systemPrompt).toContain('webproof-labs · CEO')
    // The same tools, seen at the constructor: the harness is built with them
    // rather than given them afterwards, so a first turn already has them.
    expect(record.toolNames).toEqual(['read', 'bash'])
  })

  it('treats a changed tool grant as a different agent', async () => {
    // A person whose grant changed and whose harness was reused keeps the tools
    // they used to have — either still holding one the company revoked, or
    // still missing one it just granted.
    const { profile } = person()
    const models = await operatorRouteFixture()

    const first = await hostAgent('regranted', { ...profile, tools: ['read'] }, models)

    expect(await hostAgent('regranted', { ...profile, tools: ['read', 'bash'] }, models)).not.toBe(
      first
    )
  })

  it('treats a REORDERED tool grant as a different agent', async () => {
    // Order is part of the grant: chiefd's list is ordered and the harness is
    // built from it in that order, so two orders are two different prompts.
    // A set comparison would call these the same agent.
    const { profile } = person()
    const models = await operatorRouteFixture()

    const first = await hostAgent('reordered', { ...profile, tools: ['read', 'bash'] }, models)

    expect(await hostAgent('reordered', { ...profile, tools: ['bash', 'read'] }, models)).not.toBe(
      first
    )
  })

  it('treats a changed displayName as a different agent', async () => {
    // A promoted person keeps answering under their old title otherwise — the
    // identity is baked into the system prompt at construction, so nothing
    // short of a rebuild changes who the agent says it is.
    const { profile } = person()
    const models = await operatorRouteFixture()

    const first = await hostAgent('retitled', { ...profile, displayName: 'Acme · CTO' }, models)

    expect(await hostAgent('retitled', { ...profile, displayName: 'Acme · CEO' }, models)).not.toBe(
      first
    )
  })

  it('binds the extension tools to the harness they were built into', async () => {
    // The extension tools act on the harness the selection was used to build:
    // an `org_send` circuit breaker aborts ITS turn, a card is delivered to
    // ITS queue. Unbound, both are silent no-ops — the tool appears to work
    // and its effect lands nowhere.
    const { profile } = person()
    const before = bound.length

    const harness = await hostAgent('bound', profile, await operatorRouteFixture())

    expect(bound.length).toBe(before + 1)
    expect(bound.at(-1)).toBe(harness)
  })

  it('starts the person’s lifecycle, and stops it when they are dropped', async () => {
    // The two Pi session events no harness event can supply. A person is only
    // alive between them: `session_start` is what opens their SSE
    // subscription, and `session_shutdown` is what closes it. A registry that
    // dropped a harness without stopping it would leave an offboarded person
    // draining a mailbox into an object nobody holds — invisible from every
    // surface, for the life of the server process.
    const { profile } = person()
    lifecycleCalls.length = 0

    await hostAgent('lifecycled', profile, await operatorRouteFixture())
    expect(lifecycleCalls).toEqual(['start:startup'])

    await retainDesired('lifecycled', [])
    expect(lifecycleCalls).toEqual(['start:startup', 'shutdown:quit'])
  })

  it('stops the outgoing lifecycle BEFORE the replacement starts', async () => {
    // A profile change rebuilds the harness. Two live subscriptions for one
    // person would each drain the same mailbox, and the loser's deliveries
    // would land in a harness nobody holds a reference to. The ORDER is the
    // assertion: shutdown, then start.
    const { profile } = person()
    const models = await operatorRouteFixture()
    await hostAgent('rebuilt', profile, models)
    lifecycleCalls.length = 0

    await hostAgent('rebuilt', { ...profile, displayName: 'Acme · CTO' }, models)

    expect(lifecycleCalls).toEqual(['shutdown:new', 'start:resume'])
  })

  it('reports the granted tools it could not supply, per person', async () => {
    // An id nobody registered is REPORTED rather than dropped. Saying nothing
    // is what left an operator looking at a CEO that could not hire, delegate
    // or read the roster — a failure with no symptom until somebody asked it to.
    const { profile } = person()

    const harness = await hostAgent(
      'degraded',
      { ...profile, tools: ['read', 'org_send', 'org_hire'] },
      await operatorRouteFixture()
    )

    expect(harness.getTools().map((tool) => tool.name)).toEqual(['read'])
    expect(unavailableTools('degraded', 'ada')).toEqual(['org_hire', 'org_send'])
  })

  it('reports nothing missing for a person whose whole grant was supplied', async () => {
    // Otherwise `degraded` on the roster means nothing: a list that always has
    // entries is a list nobody reads.
    const { profile } = person()

    await hostAgent('whole', profile, await operatorRouteFixture())

    expect(unavailableTools('whole', 'ada')).toEqual([])
  })

  it('reports nothing for somebody this server does not host', async () => {
    expect(unavailableTools('whole', 'nobody')).toEqual([])
    expect(unavailableTools('never-seen', 'ada')).toEqual([])
  })
})

// A hosted person asking for a fresh session gets one.
//
// THE DEFECT THIS BLOCK PINS: `requestSessionReplacement()` answered `false`,
// on the reading that this host has no native session replacement. It does —
// a replacement is a new transcript plus a rebind, and this module performs
// both. Answering `false` did not make the request go away: the durable
// fresh-session request that asked for it stayed `running` in chiefd's ledger,
// claimed by a person nobody was ever going to serve, waiting for a tmux client
// that may never attach. A durable request with no server is a leak.
describe('AgentHost session replacement', () => {
  /** Every property the intercom's `nativeResetProof` reads back before it will
   * complete the durable request. None of it is decoration: a replacement that
   * fails any one of these is reported as a failed reset against a session that
   * was in fact replaced. */
  it('writes a private new transcript carrying exactly one marker entry', async () => {
    const { profile } = person()
    const models = await operatorRouteFixture()
    await hostAgent('replaced', profile, models)
    const [first] = transcripts(profile)
    if (typeof first !== 'string') throw new Error('hosting must create a transcript')

    await replaceSession('replaced', 'ada', {
      customType: 'organization-company-native-reset',
      data: { requestId: 'req-7', sourceSessionId: 'source-session' }
    })

    const files = transcripts(profile)
    expect(files).toHaveLength(2)
    const created = files.find((name) => name !== first)
    if (typeof created !== 'string') throw new Error('a replacement must create a transcript')
    const path = join(sessionsDir(profile.cwd), created)
    // Private: the proof refuses a receipt any other account can read.
    expect(statSync(path).mode & 0o077).toBe(0)
    const lines = readFileSync(path, 'utf8').trim().split('\n')
    /* eslint-disable @typescript-eslint/consistent-type-assertions */
    // The receipt is Pi's own JSONL and `nativeResetProof` parses it exactly
    // this way, one `JSON.parse` per line into an untyped record. Reading it
    // through a helper would test the helper rather than the receipt.
    const entries = lines.map((line) => JSON.parse(line) as Record<string, unknown>)
    /* eslint-enable @typescript-eslint/consistent-type-assertions */
    // Its own session header, and a session id that is not the one replaced.
    expect(entries[0]?.type).toBe('session')
    expect(entries[0]?.id).toBe(created.replace('.jsonl', ''))
    const markers = entries.filter(
      (entry) => entry.type === 'custom' && entry.customType === 'organization-company-native-reset'
    )
    expect(markers).toHaveLength(1)
    expect(markers[0]?.data).toEqual({ requestId: 'req-7', sourceSessionId: 'source-session' })
  })

  it('shuts the old session down and starts the new one, in that order', async () => {
    const { profile } = person()
    await hostAgent('resessioned', profile, await operatorRouteFixture())
    lifecycleCalls.length = 0

    await replaceSession('resessioned', 'ada', { customType: 'marker' })

    // The outgoing subscription closes before the incoming one opens. Two live
    // subscriptions for one person would each drain the same mailbox, and the
    // loser's deliveries would land in a harness nobody holds. `new` is Pi's
    // own word for both halves of this boundary.
    expect(lifecycleCalls).toEqual(['shutdown:new', 'start:new'])
    expect(hostedPeople('resessioned')).toEqual(['ada'])
  })

  it('builds a NEW harness on the new transcript', async () => {
    const { profile } = person()
    const first = await hostAgent('reharnessed', profile, await operatorRouteFixture())

    await replaceSession('reharnessed', 'ada', { customType: 'marker' })

    // A replacement that reused the harness would leave the agent appending to
    // the transcript it was supposed to abandon — a fresh session in the ledger
    // and the same conversation in the file.
    const rebuilt = hostedSession('reharnessed', 'ada')
    expect(rebuilt).toBeDefined()
    expect(await rebuilt?.getEntries()).toHaveLength(1)
    expect(hostedSession('reharnessed', 'ada')).not.toBe(first)
  })

  it('refuses to replace a session for somebody it does not host', async () => {
    // Loud, because the caller reports the outcome to a durable request: a
    // silent no-op would mark a replacement completed against a person who was
    // never there.
    await expect(replaceSession('never-seen', 'ada', { customType: 'marker' })).rejects.toThrow(
      'no hosted person'
    )
  })
})
