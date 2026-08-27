// Roster convergence: whose decision it is, and what one person's failure costs
// everybody else.
//
// The two properties under test are the two ways this has already gone wrong
// in this program:
//
//   - chiefd decides who runs. apps/api reimplemented that rule in TypeScript,
//     read the wrong field name, concluded NOBODY was desired, and answered
//     every talk verb 409 while its suites stayed green — because they tested
//     the reimplementation against itself.
//   - the ROUTE is the box's, not a person's. Every hosted person and the
//     Founder run on the operator's own Pi defaults, resolved once per
//     convergence; a box whose Pi names no model refuses the whole read by
//     name rather than marking each person unroutable in turn.
//
// A THIRD property joins them, and it is the defect this module shipped: the
// profile's `tools` and `displayName` were dropped on the way to the host, so
// every hosted agent ran with no tools and no identity. That has no symptom —
// the person is hosted, the roster says so — until somebody asks the agent to
// do its job. `degraded` exists so the roster says out loud when a person is
// running WITHOUT tools their company granted them.
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import type { ApiHostLaunchProfile } from '@chief/chiefing'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { selectTools } from '@/server/AgentTools'
import type { AgentProfile } from '@/types/AgentHost'

// The extension install is the one seam stubbed here. It reads the company
// manifest from that person's live daemon, which is a running company; the
// selection this file exercises — chiefd's grant, filtered, with what was not
// supplied reported — is the real one either way.
vi.mock('@/server/ExtensionTools', () => ({
  HOSTED_EXTENSIONS: [],
  installExtensions: () =>
    Promise.resolve({
      tools: new Map(),
      refusedHandlers: [],
      bind: () => ({
        context: {},
        start: () => Promise.resolve(),
        shutdown: () => Promise.resolve()
      })
    })
}))

const read = vi.fn()
const hostAgent = vi.fn()
const retainDesired = vi.fn()
const hostedAgent = vi.fn()
const unavailableTools = vi.fn()
/** Lifecycle hooks the host refused to drive for a person.
 *
 * Empty in every test here except the one about it: a refused hook degrades a
 * person the same way a missing tool does, and the roster is where an operator
 * learns about either. */
const refusedHandlers = vi.fn()

/** What the host could not supply, per person, from the LAST launch.
 *
 * Filled by the real `selectTools` running over the profile this module built,
 * rather than by a hand-fed list: the assertion worth making is that chiefd's
 * grant survives the trip onto the profile and that the `org_*` family is what
 * comes back unavailable. A stub returning `['org_send']` would pass just
 * as well against a module that dropped `tools` entirely — which is the exact
 * defect. */
const missing = new Map<string, readonly string[]>()

/** chiefd's profile read, as it now answers: who is actuating, plus the plans.
 *
 * `shadow` by default because that is the case every test below is about. The
 * tmux case has its own test and says so explicitly — that reversal used to be
 * a REFUSAL from chiefd; it is now a fact this server decides from. */
function resolveRead(
  plans: readonly unknown[],
  actuation: { effectiveMode: string; configuredMode: string; breakerTripped: boolean } = {
    effectiveMode: 'shadow',
    configuredMode: 'shadow',
    breakerTripped: false
  }
): void {
  read.mockResolvedValue({ actuation, plans })
}

vi.mock('@/server/CompanyChiefd', () => ({
  companyChiefd: async () => ({ apiHostLaunchProfile: { read } }),
  // The real class, because the refusal's STATUS and CODE are what the route
  // layer branches on — a stub would let a mislabelled refusal pass.
  CompanyUnavailableError: class extends Error {
    readonly status: number
    readonly code: string
    constructor(options: { status: number; code: string; message: string }) {
      super(options.message)
      this.status = options.status
      this.code = options.code
    }
  }
}))
/** The one route this box runs on.
 *
 * Stubbed at the module seam rather than reproduced per person, because that
 * IS the rule: `convergeRoster` resolves it ONCE and hands the same object to
 * everybody. `operatorRoute` answering `undefined` is a whole-company refusal,
 * which the test below drives.
 */
const operatorRoute = vi.fn()

vi.mock('@/server/OperatorPi', () => ({
  operatorRoute: (...args: unknown[]) => operatorRoute(...args)
}))
vi.mock('@/server/AgentHost', () => ({
  hostAgent: (...args: unknown[]) => hostAgent(...args),
  hostedAgent: (...args: unknown[]) => hostedAgent(...args),
  retainDesired: (...args: unknown[]) => retainDesired(...args),
  unavailableTools: (...args: unknown[]) => unavailableTools(...args),
  refusedHandlers: (...args: unknown[]) => refusedHandlers(...args)
}))

const { agentFor, convergeRoster } = await import('@/server/HostedRoster')

/** A person root, laid out the way chiefd materializes one. */
function world(): { cwd: string } {
  const root = mkdtempSync(join(tmpdir(), 'hosted-roster-'))
  mkdirSync(join(root, 'pi-home'), { recursive: true })
  mkdirSync(join(root, 'workspace'), { recursive: true })
  writeFileSync(join(root, 'workspace', '.keep'), '')
  return { cwd: join(root, 'workspace') }
}

/** chiefd's wire profile for one person, typed as chiefd's OWN type.
 *
 * `ApiHostLaunchProfile` rather than a loose record, so a field this module is
 * required to carry cannot quietly go missing from the fixture the way `tools`
 * and `displayName` went missing from the profile. */
function plan(personId: string, tools: readonly string[] = ['read']): ApiHostLaunchProfile {
  const { cwd } = world()
  return {
    personId,
    cwd,
    env: {},
    tools,
    displayName: 'Acme · Engineer'
  }
}

beforeEach(() => {
  read.mockReset()
  missing.clear()
  // The host's own answer, computed by the REAL tool selection over the
  // profile this module hands it. That makes `degraded` below a fact about
  // what chiefd granted, not about what a stub was told to say.
  hostAgent.mockReset().mockImplementation(async (_companyKey: unknown, profile: AgentProfile) => {
    missing.set(profile.personId, (await selectTools(profile)).unavailable)
    return {}
  })
  unavailableTools
    .mockReset()
    .mockImplementation((_companyKey: unknown, personId: string) => missing.get(personId) ?? [])
  refusedHandlers.mockReset().mockReturnValue([])
  retainDesired.mockReset().mockResolvedValue(undefined)
  hostedAgent.mockReset()
  operatorRoute.mockReset().mockResolvedValue({
    models: { getModel: () => undefined },
    model: { id: 'fixture-model-a', provider: 'openrouter' },
    provider: 'openrouter'
  })
})

afterEach(() => {
  vi.clearAllMocks()
})

describe('convergeRoster', () => {
  it('hosts exactly the people chiefd’s profile names', async () => {
    resolveRead([plan('ceo'), plan('ada')])

    const result = await convergeRoster('acme')

    expect(result.hosted).toEqual(['ceo', 'ada'])
    // The roster is chiefd's answer, passed through. This module computes no
    // membership of its own — that reimplementation is the defect.
    expect(retainDesired).toHaveBeenCalledWith('acme', ['ceo', 'ada'])
  })

  it('drops the undesired BEFORE launching anybody', async () => {
    // Ordering is the property: a person chiefd no longer wants running must
    // stop being hosted even if every launch afterwards fails. Deferring the
    // drop to the end would keep an offboarded person alive on any error path.
    const order: string[] = []
    retainDesired.mockImplementation(() => order.push('retain'))
    hostAgent.mockImplementation(async () => {
      order.push('host')
      throw new Error('launch exploded')
    })
    resolveRead([plan('ada')])

    await expect(convergeRoster('acme')).rejects.toThrow('launch exploded')

    expect(order).toEqual(['retain', 'host'])
  })

  it('reports a tmux-actuating company as a refusal, not an upstream fault', async () => {
    // A company actuating tmux panes is not hosted here, and a host launching
    // agents beside that would be the second roster this layer exists to
    // prevent. It is a 409 naming the state, never a 502: reported as an
    // upstream fault it reads as "the daemon is broken" about a daemon that is
    // working correctly and reporting its own mode.
    //
    // chiefd used to REFUSE the read with `company-not-api-hosted` and this
    // module recognised the refusal by its code. chiefd now publishes the three
    // facts that refusal carried and leaves the decision here — reading a fact
    // is not actuating on it, and the same read is the launch half of the
    // contract for a tmux client, which runs under `apply` by definition.
    // The refusal an operator sees is unchanged in status and code.
    resolveRead([plan('ceo')], {
      effectiveMode: 'apply',
      configuredMode: 'apply',
      breakerTripped: false
    })

    await expect(convergeRoster('acme')).rejects.toMatchObject({
      status: 409,
      code: 'company-not-api-hosted'
    })
    // The facts are chiefd's and they are all named. The phrasing is this
    // server's: chiefd used to supply the whole sentence, including a
    // `chiefd set-actuation-config` command line — a backend describing a CLI
    // to a browser.
    await expect(convergeRoster('acme')).rejects.toThrow('effective actuation mode is apply')
    // And nothing is hosted: the refusal must come BEFORE any launch.
    expect(hostAgent).not.toHaveBeenCalled()
  })

  it('hosts a company whose breaker forced it into shadow, and says so', async () => {
    // The EFFECTIVE mode decides. A tripped breaker forces shadow, and under a
    // tripped breaker chiefd is not actuating anything either — so there is no
    // second roster to collide with.
    resolveRead([plan('ceo')], {
      effectiveMode: 'shadow',
      configuredMode: 'apply',
      breakerTripped: true
    })

    const result = await convergeRoster('acme')

    expect(result.hosted).toEqual(['ceo'])
  })

  it('lets an unrecognized failure through rather than mislabelling it', async () => {
    read.mockRejectedValue(new Error('connection reset'))

    await expect(convergeRoster('acme')).rejects.toThrow('connection reset')
  })

  it('lets a transport error through even when its text names the refusal code', async () => {
    // The read no longer refuses at all, so a failure carrying those words is
    // unambiguously a transport failure. Kept as an inverted assertion rather
    // than deleted: message text was never part of the contract, and treating
    // it as one would answer 409 "this company runs in tmux" for a daemon that
    // never said so.
    read.mockRejectedValue(new Error('proxy log: company-not-api-hosted was seen upstream'))

    const failure: unknown = await convergeRoster('acme').catch((error: unknown) => error)

    expect(failure).toBeInstanceOf(Error)
    expect(failure).not.toMatchObject({ code: 'company-not-api-hosted' })
  })

  it('hosts nobody, and refuses nobody, for a company chiefd wants empty', async () => {
    resolveRead([])

    const result = await convergeRoster('acme')

    expect(result).toEqual({ hosted: [], degraded: [] })
    expect(retainDesired).toHaveBeenCalledWith('acme', [])
  })

  it('carries chiefd’s tools and displayName onto the profile it launches', async () => {
    // THE DEFECT, AT ITS SOURCE. Both fields were dropped right here, between
    // chiefd's wire profile and the host's, so the harness was built with
    // neither: the agent could talk and could do nothing, and answered "I'm
    // Claude, an AI assistant created by Anthropic" when asked what company it
    // ran. A straight field carry — anything computed here would be a second
    // opinion about a decision chiefd already made.
    resolveRead([plan('ceo', ['read', 'bash'])])

    await convergeRoster('acme')

    expect(hostAgent).toHaveBeenCalledWith(
      'acme',
      expect.objectContaining({
        personId: 'ceo',
        tools: ['read', 'bash'],
        displayName: 'Acme · Engineer'
      }),
      expect.anything()
    )
  })

  it('reports a person running without the org_* tools their company granted', async () => {
    // A hosted person with no `org_*` tools looks perfectly staffed and cannot
    // delegate, hire, or create a department. There is no symptom until
    // somebody asks the agent to do its job — so the roster says it out loud
    // rather than swallowing it.
    resolveRead([plan('ceo', ['read', 'org_send', 'org_hire'])])

    const result = await convergeRoster('acme')

    expect(result.hosted).toEqual(['ceo'])
    expect(result.degraded).toEqual([
      { personId: 'ceo', missingTools: ['org_hire', 'org_send'], refusedHandlers: [] }
    ])
  })

  it('leaves a fully equipped person out of degraded entirely', async () => {
    // Otherwise the list means nothing: a `degraded` array that always has
    // entries is one nobody reads, and the one person who really cannot
    // delegate goes back to being invisible.
    resolveRead([plan('ceo', ['read', 'bash', 'ls'])])

    const result = await convergeRoster('acme')

    expect(result.hosted).toEqual(['ceo'])
    expect(result.degraded).toEqual([])
  })

  it('reports one person degraded without implicating anybody else', async () => {
    // One person's shortfall is one person's, and a company-wide warning about
    // a healthy CEO is a warning the operator learns to ignore.
    resolveRead([plan('ceo'), plan('ada', ['org_roster'])])

    const result = await convergeRoster('acme')

    expect(result.hosted).toEqual(['ceo', 'ada'])
    expect(result.degraded).toEqual([
      { personId: 'ada', missingTools: ['org_roster'], refusedHandlers: [] }
    ])
  })

  // THE RULE: chief chooses no route, so there is exactly ONE, and it is the
  // operator's own Pi. A box whose Pi names no default model cannot host
  // anybody, and it says so by name — hosting agents that would each fail at
  // their first turn is the failure this program keeps reproducing.
  it('refuses the whole convergence when this box’s own Pi names no route', async () => {
    operatorRoute.mockResolvedValue(undefined)
    resolveRead([plan('ceo'), plan('ada')])

    await expect(convergeRoster('acme')).rejects.toMatchObject({
      status: 409,
      code: 'operator-route-unset'
    })
    expect(hostAgent).not.toHaveBeenCalled()
  })

  it('resolves the route ONCE and hands the same one to everybody', async () => {
    // Per-person resolution is what the deleted `PersonModels` did, and its
    // whole point was scoping one person's credential away from another's.
    // There is one operator on this server, so there is one route: resolving
    // it per person would be a decision this layer no longer makes.
    resolveRead([plan('ceo'), plan('ada')])

    await convergeRoster('acme')

    expect(operatorRoute).toHaveBeenCalledTimes(1)
    const [first, second] = hostAgent.mock.calls
    expect(first?.[2]).toBe(second?.[2])
  })
})

describe('agentFor', () => {
  it('returns a live agent without re-reading chiefd', async () => {
    hostedAgent.mockReturnValue({ live: true })

    expect(await agentFor('acme', 'ceo')).toEqual({ live: true })
    expect(read).not.toHaveBeenCalled()
  })

  it('converges first when the registry is cold, rather than answering “not running”', async () => {
    // The first request after a restart finds an empty registry. Answering
    // 409 there would be this process disagreeing with a roster it does not
    // own — which is precisely how every talk verb failed under apps/api.
    hostedAgent.mockReturnValueOnce(undefined).mockReturnValueOnce({ live: true })
    resolveRead([plan('ceo')])

    expect(await agentFor('acme', 'ceo')).toEqual({ live: true })
    expect(read).toHaveBeenCalledWith('acme')
  })

  it('stays undefined for somebody chiefd does not want running', async () => {
    hostedAgent.mockReturnValue(undefined)
    resolveRead([])

    expect(await agentFor('acme', 'ghost')).toBeUndefined()
  })
})
