// The `ExtensionAPI` → `AgentTool` adapter: what the extensions registered is
// what the harness gets, whole.
//
// THE DEFECT THIS PINS: the web host built 7 of a CEO's 60 tools. The other 53
// live in `packages/piing/extensions/*`, which install into Pi's
// `ExtensionAPI` and call `registerTool`, while the harness takes a flat
// `AgentTool[]`. No adapter stood between the two shapes, so an agent that
// looked perfectly staffed could not message, hire, or open a
// task.
//
// The property under test is NOT "these tool names are present". A test naming
// a handful of tools does not stop the next one from being dropped, and the
// names are not this module's to know in the first place — the extensions own
// them. What is pinned is that the adapter FILTERS NOTHING: whatever an
// extension registers appears, by count and by name, with its schema, its
// handler, and its result reaching the caller untouched.
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import type { ExtensionAPI, ToolDefinition } from '@earendil-works/pi-coding-agent'
import { harnessStub, subjectFor } from '@test/harness/HostedPersonStubs'
import { Type } from 'typebox'
import { describe, expect, it, vi } from 'vitest'

import { HOSTED_EXTENSIONS, installExtensions } from '@/server/ExtensionTools'
import type { AgentProfile } from '@/types/AgentHost'
import type { ExtensionInstaller } from '@/types/ExtensionTools'

/** A person as chiefd's launch profile describes one.
 *
 * The environment carries the real keys chiefd puts on an API-host profile,
 * because the extensions read their company out of it — an install with a
 * different set is an install of somebody else. */
function profileFor(granted: readonly string[] = []): AgentProfile {
  const root = mkdtempSync(join(tmpdir(), 'extension-tools-'))
  return {
    personId: 'ceo',
    cwd: root,
    env: {
      ORG_LAUNCHER_ORG_DIR: join(root, 'acme'),
      ORG_LAUNCHER_ORGANIZATION: 'acme',
      ORG_LAUNCHER_PERSON: 'ceo',
      ORG_LAUNCHER_ROOT: root,
      ORG_CHIEFD_URL: 'http://127.0.0.1:1'
    },
    tools: granted,
    displayName: 'Acme · CEO'
  }
}

/**
 * What a set of installers registers, observed WITHOUT the adapter.
 *
 * A second, independent recorder is the whole point: comparing the adapter's
 * output against the adapter's own bookkeeping would pass however much it
 * dropped. This is Pi's side of the contract, written down once, in six lines.
 */
async function registeredNames(
  profile: AgentProfile,
  installers: readonly ExtensionInstaller[]
): Promise<string[]> {
  const names: string[] = []
  const recorder = {
    registerTool: (definition: ToolDefinition): void => void names.push(definition.name),
    registerMessageRenderer: (): void => {},
    registerEntryRenderer: (): void => {},
    appendEntry: (): void => {},
    on: (): void => {},
    sendMessage: (): void => {},
    setThinkingLevel: (): void => {},
    setModel: (): Promise<boolean> => Promise.resolve(true)
  }
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Same reasoning as the adapter's own recorder: Pi's `ExtensionAPI` is a
  // large concrete surface and an extension calls six methods of it.
  const pi = recorder as never
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
  for (const install of installers) await install(pi, profile.env)
  return names.sort()
}

/** The schema object the decorated fixture registers, held so a test can
 * assert the adapter carried THAT object rather than a copy of it. */
const decoratedSchema = Type.Object({ note: Type.String() })

/** An extension that registers one tool with every optional field set.
 *
 * Every field the adapter carries is exercised by this one definition, so a
 * carry that is quietly dropped fails a test rather than a hosted agent. */
const decorated: ExtensionInstaller = (pi: ExtensionAPI) => {
  pi.registerTool({
    name: 'fixture_decorated',
    label: 'Decorated fixture',
    description: 'Every optional field a tool definition may carry.',
    parameters: decoratedSchema,
    executionMode: 'sequential',
    prepareArguments: (args: unknown) =>
      typeof args === 'string' ? { note: args } : { note: 'unreadable' },
    renderCall: () => {
      throw new Error('renderCall builds a terminal component and must never be called here')
    },
    renderResult: () => {
      throw new Error('renderResult builds a terminal component and must never be called here')
    },
    execute: async (toolCallId, params, _signal, _onUpdate, context) => ({
      content: [{ type: 'text', text: `${toolCallId}:${params.note}` }],
      details: { ok: true, sawContext: typeof context?.abort === 'function' }
    })
  })
}

/** A second offline fixture, registering TWO tools from one install.
 *
 * `organization-intercom` — the whole hosted set — is deliberately absent from
 * these fixtures: its install reads the company manifest from that person's
 * live daemon, which is a running company, not a unit test. It is proven end
 * to end instead, through a real browser against a real company
 * (`scripts/browser-org-tools-check.mjs`). So the adapter's own property is
 * proven against fixtures, and the count assertion below stays non-vacuous
 * only while more than one installer registers more than one tool between
 * them — which is what this fixture is for.
 */
const pair: ExtensionInstaller = (pi: ExtensionAPI) => {
  for (const name of ['fixture_first', 'fixture_second']) {
    pi.registerTool({
      name,
      label: name,
      description: `Fixture tool ${name}.`,
      parameters: Type.Object({}),
      execute: async () => ({ content: [{ type: 'text', text: name }], details: { ok: true } })
    })
  }
}

describe('installExtensions', () => {
  it('adapts every tool the extensions registered, by count and by name', async () => {
    const profile = profileFor()
    const installers = [pair, decorated]

    const adapted = await installExtensions(profile, installers)
    const expected = await registeredNames(profile, installers)

    // BY COUNT: a filter, a `continue`, or a map that overwrote an entry all
    // show up here before anybody has to guess which tool went missing.
    expect(adapted.tools.size).toBe(expected.length)
    // BY NAME: derived from the registration on both sides, so a tool added to
    // an extension tomorrow is covered by this assertion without editing it.
    expect([...adapted.tools.keys()].sort()).toEqual(expected)
  })

  it('adapts every tool of a single install that registers more than one', async () => {
    // The one place tool names are written down, and it is a REGRESSION
    // anchor, not the inventory: the assertion above owns completeness. This
    // says one install contributing several tools loses none of them.
    const adapted = await installExtensions(profileFor(), [pair])

    expect([...adapted.tools.keys()].sort()).toEqual(['fixture_first', 'fixture_second'])
  })

  it('carries a tool’s schema across unchanged, by identity', async () => {
    const adapted = await installExtensions(profileFor(), [decorated])
    const tool = adapted.tools.get('fixture_decorated')

    // The SAME schema object the extension built, not a copy and not a
    // re-derivation. A provider is handed this object; a reshaped one is a
    // different contract with the model.
    expect(tool?.parameters).toBe(decoratedSchema)
    expect(tool?.name).toBe('fixture_decorated')
    expect(tool?.label).toBe('Decorated fixture')
    expect(tool?.description).toBe('Every optional field a tool definition may carry.')
    expect(tool?.executionMode).toBe('sequential')
  })

  it('carries a tool’s handler across unchanged, result path and all', async () => {
    const adapted = await installExtensions(profileFor(), [decorated])
    const tool = adapted.tools.get('fixture_decorated')

    const result = await tool?.execute('call-7', { note: 'hello' })

    // The extension's own result object, reaching the caller as the extension
    // built it: same content, same details. A wrapper that re-encoded either
    // is how a tool comes to "work" and answer the model something else. The
    // `sawContext` flag is the fifth argument Pi passes and the harness does
    // not — the one thing the adapter has to supply itself.
    expect(result?.content).toEqual([{ type: 'text', text: 'call-7:hello' }])
    expect(result?.details).toEqual({ ok: true, sawContext: true })
  })

  it('carries prepareArguments, which runs before schema validation', async () => {
    const adapted = await installExtensions(profileFor(), [decorated])

    // Dropped, this is silent: a historical transcript's argument alias stops
    // being normalized and the tool refuses arguments it used to accept.
    expect(adapted.tools.get('fixture_decorated')?.prepareArguments?.('hello')).toEqual({
      note: 'hello'
    })
  })

  it('does not carry the terminal renderers', async () => {
    const adapted = await installExtensions(profileFor(), [decorated])
    const tool = adapted.tools.get('fixture_decorated')

    // `renderCall`/`renderResult` build `pi-tui` components for a pane. This
    // host has no pane, and the fixture's throw proves nothing here calls them.
    expect('renderCall' in Object(tool)).toBe(false)
    expect('renderResult' in Object(tool)).toBe(false)
  })

  it('refuses two extensions registering one tool name', async () => {
    // A collision is a real defect that chiefd's materializer refuses upstream.
    // Silently letting one win here would make this host the place a person
    // acquires a tool nobody granted.
    await expect(installExtensions(profileFor(), [decorated, decorated])).rejects.toThrow(
      'two extensions registered a tool named "fixture_decorated"'
    )
  })

  it('gives a tool the abort its circuit breakers call, once bound', async () => {
    const breaker: ExtensionInstaller = (pi: ExtensionAPI) => {
      pi.registerTool({
        name: 'fixture_breaker',
        label: 'Breaker',
        description: 'Calls the execution context’s abort, as org_send does.',
        parameters: Type.Object({}),
        execute: async (_id, _params, _signal, _onUpdate, context) => {
          context?.abort()
          return { content: [{ type: 'text', text: 'stopped' }], details: { ok: false } }
        }
      })
    }
    const adapted = await installExtensions(profileFor(), [breaker])
    const stub = harnessStub()
    adapted.bind(subjectFor(stub, '/tmp/extension-tools-breaker'), '/tmp/session.jsonl')

    await adapted.tools.get('fixture_breaker')?.execute('call-1', {})

    // The breaker that stops a wedged turn reaches the harness this tool set
    // built. Unbound it would be a no-op, and a model stuck in an empty-send
    // loop would stay stuck.
    expect(stub.abortCount()).toBe(1)
  })

  it('delivers an extension message through the person’s lifecycle', async () => {
    let held: ExtensionAPI | undefined
    // Exactly how the real extensions reach `sendMessage`: they keep `pi` in a
    // closure and call it later, from a handler, long after install returned.
    const adapted = await installExtensions(profileFor(), [(pi) => void (held = pi)])
    const stub = harnessStub()
    adapted.bind(subjectFor(stub, '/tmp/extension-tools-queue'), '/tmp/session.jsonl')

    held?.sendMessage({ customType: 'card', content: 'wake up', display: true })
    held?.sendMessage(
      { customType: 'card', content: [{ type: 'text', text: 'urgent' }], display: true },
      { deliverAs: 'steer' }
    )
    await vi.waitFor(() => expect(stub.delivered.length).toBe(2))

    // BOTH arrive as a started turn, and that is the product rather than a
    // detail of the stub: this agent is idle, `steer` and `followUp` refuse an
    // idle harness outright, and `nextTurnQueue` is never drained by
    // `AgentHarness` itself. `server/ExtensionLifecycle` owns that decision —
    // this module only has to hand it the message instead of guessing a queue,
    // which is what it used to do and why a real reminder was lost.
    expect(stub.delivered).toEqual([
      { mode: 'prompt', text: 'wake up' },
      { mode: 'prompt', text: 'urgent' }
    ])
  })

  it('installs a real, non-empty extension list in production', () => {
    // The `installers` argument is a seam for tests; the DEFAULT is the
    // product. A default this file could quietly diverge from is the second
    // source of truth the whole adapter exists to avoid.
    expect(HOSTED_EXTENSIONS.length).toBeGreaterThan(0)
    expect(HOSTED_EXTENSIONS.every((install) => typeof install === 'function')).toBe(true)
  })
})
