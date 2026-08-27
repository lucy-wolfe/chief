// The Founder host: what it refuses, what it builds, and what its one tool does.
//
// Everything here runs against a REAL `AgentHarness` on the REAL route Pi's
// own `ModelRuntime` builds from a fixture agent directory. Nothing calls a
// provider — constructing a harness does not — so the assertions are about the
// agent this server would actually put in front of an operator, not about a
// stand-in for it.
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
  launchCompany: vi.fn()
}))

vi.mock('@/server/CompanyLifecycle', () => ({
  launchCompany: mocks.launchCompany
}))

import {
  founderAgent,
  FounderUnavailableError,
  hostedFounder,
  resetFounder
} from '@/server/FounderAgent'
import { founderSkillBody } from '@/server/FounderIdentity'

/** An operator Pi agent directory carrying one credentialed provider, and the
 * settings file naming the route that Pi — and therefore Founder — is on.
 *
 * `zip` is CUSTOM: it is not a provider Pi knows natively, which is the
 * ordinary shape of a box and the case a route chosen anywhere else gets
 * wrong. */
function agentDir(): string {
  const root = mkdtempSync(join(tmpdir(), 'founder-agent-'))
  /* eslint-disable lucy/no-json-stringify */
  // The real registry file shape, read off disk by the module under test.
  writeFileSync(
    join(root, 'models.json'),
    JSON.stringify({
      providers: {
        zip: {
          baseUrl: 'https://example.invalid/v1',
          api: 'openai-completions',
          apiKey: 'sk-fixture',
          models: [{ id: 'fast-1', contextWindow: 8192 }]
        }
      }
    })
  )
  /* eslint-enable lucy/no-json-stringify */
  writeSettings(root, { defaultProvider: 'zip', defaultModel: 'fast-1' })
  return root
}

/** The operator's root Pi settings.json — the file Pi resolves a session's
 * own model from, and therefore the file this box's Founder route comes from. */
function writeSettings(root: string, settings: Record<string, unknown>): void {
  /* eslint-disable lucy/no-json-stringify */
  // The real settings file shape, read off disk by the module under test.
  writeFileSync(join(root, 'settings.json'), JSON.stringify(settings))
  /* eslint-enable lucy/no-json-stringify */
}

/** Run a founder tool by name, the way the model would. */
async function callLaunch(args: {
  name: string
  purpose: string
}): Promise<{ text: string; isError: boolean }> {
  const founder = await founderAgent()
  const tool = founder.tools.find((entry) => entry.name === 'chiefd_launch_company')
  if (!tool) throw new Error('the launch tool is not on the harness')
  const result = await tool.execute('call-1', args)
  const first = result.content[0]
  const text = first?.type === 'text' ? first.text : ''
  const details: Record<string, unknown> = Object.fromEntries(Object.entries(result.details ?? {}))
  return { text, isError: details.ok !== true }
}

describe('FounderAgent', () => {
  // `vi.stubEnv` rather than assigning `process.env` directly: apps/web's
  // `lucy/no-process-env` keeps every environment read in `common/Env`, and a
  // test that reached around it would be the first exception in the app.
  beforeEach(() => {
    resetFounder()
    mocks.launchCompany.mockReset()
    vi.stubEnv('PI_SOURCE_AGENT_DIR', agentDir())
  })

  afterEach(() => {
    resetFounder()
    vi.unstubAllEnvs()
  })

  it('refuses, by name, a box whose Pi has not been pointed at a model', async () => {
    const root = agentDir()
    writeSettings(root, { theme: 'light/dark' })
    vi.stubEnv('PI_SOURCE_AGENT_DIR', root)
    await expect(founderAgent()).rejects.toThrowError(FounderUnavailableError)
    // The refusal names the FILES to fix, because that is the operator's next
    // move. It used to name two environment variables, which sent them to a
    // shell profile instead of to the model picker.
    await expect(founderAgent()).rejects.toThrowError(/settings\.json/)
    await expect(founderAgent()).rejects.toThrowError(/defaultProvider/)
    // Refused BEFORE anything was built: nothing is hosted, so no later
    // request can find a half-started Founder.
    expect(hostedFounder()).toBeUndefined()
  })

  it('refuses a provider the registry names but holds no key for', async () => {
    const root = mkdtempSync(join(tmpdir(), 'founder-agent-nokey-'))
    /* eslint-disable lucy/no-json-stringify */
    // The real registry shape again: a provider the box describes perfectly
    // and holds no credential for.
    writeFileSync(
      join(root, 'models.json'),
      JSON.stringify({
        providers: { zip: { baseUrl: 'https://example.invalid/v1', models: [{ id: 'fast-1' }] } }
      })
    )
    /* eslint-enable lucy/no-json-stringify */
    writeSettings(root, { defaultProvider: 'zip', defaultModel: 'fast-1' })
    vi.stubEnv('PI_SOURCE_AGENT_DIR', root)
    // PI'S OWN ANSWER, not a second one. `ModelRuntime.getModel` returns
    // nothing for a provider it holds no credential for, so a box that
    // describes `zip` perfectly and holds no key for it has no route — and
    // this server reports that rather than building an agent that would fail
    // at its first turn.
    await expect(founderAgent()).rejects.toThrowError(FounderUnavailableError)
    // The refusal names every file Pi actually consults. It once named only
    // `models.json` while the routing code read that file alone, so a box
    // whose key was in `auth.json` and whose models were in the store — the
    // ORDINARY Pi box, which writes no `models.json` — was told its registry
    // did not describe a provider it described in full.
    await expect(founderAgent()).rejects.toThrowError(/models-store\.json/)
    await expect(founderAgent()).rejects.toThrowError(/auth\.json/)
  })

  it('refuses a model no source describes rather than substituting one', async () => {
    const root = agentDir()
    writeSettings(root, { defaultProvider: 'zip', defaultModel: 'a-model-nobody-registered' })
    vi.stubEnv('PI_SOURCE_AGENT_DIR', root)
    await expect(founderAgent()).rejects.toThrowError(FounderUnavailableError)
    expect(hostedFounder()).toBeUndefined()
  })

  it('builds a Founder with the founder prompt and exactly one tool', async () => {
    const founder = await founderAgent()
    expect(founder.route.provider).toBe('zip')
    expect(founder.route.model.id).toBe('fast-1')
    expect(founder.tools.map((tool) => tool.name)).toEqual(['chiefd_launch_company'])
    expect(founder.systemPrompt).toContain(founderSkillBody())
  })

  it("runs on the route the operator's own Pi is on, with nothing exported", async () => {
    // THE RULE. Founder mode has no route of its own to be configured with:
    // the Founder PANE takes the model its live Pi session is running, and Pi
    // resolves that from its own agent directory. This door has no live
    // session, so it asks Pi's own `ModelRuntime` for the same answer. A box
    // whose only provider is custom — the ordinary zbox — must therefore get a
    // Founder on that provider without being told twice.
    const root = agentDir()
    writeSettings(root, { defaultProvider: 'zip', defaultModel: 'fast-1' })
    vi.stubEnv('PI_SOURCE_AGENT_DIR', root)
    const founder = await founderAgent()
    expect(founder.route.provider).toBe('zip')
    expect(founder.route.model.id).toBe('fast-1')
  })

  it('is a singleton: the same conversation answers the next request', async () => {
    const first = await founderAgent()
    expect(await founderAgent()).toBe(first)
  })

  it('rebuilds when the operator changes the route in Pi', async () => {
    const first = await founderAgent()
    expect(await founderAgent()).toBe(first)
    // A different route is a different Founder. Answering from the old one
    // would leave an operator who switched models in Pi still talking to the
    // one they were trying to move off — and creating companies on it.
    const root = agentDir()
    writeSettings(root, { defaultProvider: 'zip', defaultModel: 'a-model-nobody-registered' })
    vi.stubEnv('PI_SOURCE_AGENT_DIR', root)
    await expect(founderAgent()).rejects.toThrowError(FounderUnavailableError)
  })

  it('launches a company through the one lifecycle client and records the slug', async () => {
    mocks.launchCompany.mockResolvedValue({ slug: 'acme-inc' })
    const result = await callLaunch({ name: '  Acme Inc  ', purpose: '  Build things  ' })

    // Trimmed, because the model sends what the operator typed.
    expect(mocks.launchCompany).toHaveBeenCalledWith({
      name: 'Acme Inc',
      purpose: 'Build things'
    })
    expect(result.isError).toBe(false)
    expect(result.text).toContain('Company launched')
    // The slug is recorded as DATA, so the route can report it without
    // parsing the model's prose.
    expect(hostedFounder()?.launch.launched).toEqual({ slug: 'acme-inc', name: 'Acme Inc' })
  })

  it('reports a refused genesis as a failure the model must not read as success', async () => {
    mocks.launchCompany.mockRejectedValue(new Error('beacond already claims "acme-inc"'))
    const result = await callLaunch({ name: 'Acme Inc', purpose: 'Build things' })

    expect(result.isError).toBe(true)
    expect(result.text).toContain('No company was created')
    expect(result.text).toContain('beacond already claims')
    // Nothing recorded: a UI that linked to a company chiefd refused to create
    // would send the operator to a 404 with a success banner above it.
    expect(hostedFounder()?.launch.launched).toBeUndefined()
  })

  it('tells the model it MAY retry once the cause is fixed, and not before', async () => {
    // The success path ends in a prohibition ("do not create another"). A
    // failure path carrying only a second prohibition is what produces an
    // agent that reports the error and stops — a dead-ended conversation,
    // where this tool replaced a Retry button.
    mocks.launchCompany.mockRejectedValue(
      new Error('/root/.pi/agent/settings.json does not name a usable route')
    )
    const { text } = await callLaunch({ name: 'Acme Inc', purpose: 'Build things' })

    expect(text).toContain('MAY call this tool again')
    // Bounded, not open: the causes we actually hit are operator-fixable
    // BETWEEN attempts, so an immediate retry is a loop against a refusal
    // that cannot change yet.
    expect(text).toContain('Do not retry immediately')
  })

  it('RECOVERS: a failed launch and then a successful one, in ONE conversation', async () => {
    // One harness, one `launch` record, two turns. Two independent calls with
    // two mocks would pass against a broken implementation because neither
    // would have observed the other; the subject here is the state carried
    // ACROSS the failed turn — that nothing latched, and that the second
    // attempt is not poisoned by the first.
    //
    // This is the capability that replaced the form's Retry button, and the
    // operator has already hit both of the causes it recovers from
    // (a Pi that named no route, and `launcher-root-unusable`).
    mocks.launchCompany.mockRejectedValueOnce(
      new Error('launcher-root-unusable: no packages/piing/extensions at /gone')
    )
    const founderBefore = await founderAgent()

    const failed = await callLaunch({ name: 'Acme Inc', purpose: 'Build things' })
    expect(failed.isError).toBe(true)
    expect(hostedFounder()?.launch.launched).toBeUndefined()

    // The operator fixes the launcher root. Same conversation, same harness.
    mocks.launchCompany.mockResolvedValueOnce({ slug: 'acme-inc' })
    const succeeded = await callLaunch({ name: 'Acme Inc', purpose: 'Build things' })

    expect(succeeded.isError).toBe(false)
    expect(succeeded.text).toContain('Company launched')
    expect(hostedFounder()?.launch.launched).toEqual({ slug: 'acme-inc', name: 'Acme Inc' })
    // The SAME Founder answered both turns. A rebuild between them would have
    // discarded the conversation the operator was having, and would also mean
    // this test never exercised the carried state it exists for.
    expect(hostedFounder()).toBe(founderBefore)
    expect(mocks.launchCompany).toHaveBeenCalledTimes(2)
  })
})
