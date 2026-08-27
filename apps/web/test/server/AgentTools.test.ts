// Tool selection: which of Pi's and the extensions' tools a hosted person
// gets, and what this host admits it cannot give them.
//
// THE FIRST DEFECT THIS PINS: a hosted agent had NO TOOLS AT ALL.
// `ApiHostLaunchProfile.tools` carries chiefd's grant — 60 ids for a CEO, from
// `read` and `bash` through the whole `org_*` family — and the harness was
// constructed with none of them.
//
// THE SECOND: it then had SEVEN. The `org_*` family lives in Pi extensions
// that register into `ExtensionAPI`, this host builds a flat tool list, and no
// adapter joined the two — so 53 of a CEO's 60 came back "unavailable" and an
// agent that looked staffed could not message, hire, or read the roster.
//
// Three properties, and each one is a defect if it goes the other way:
//
//   - what IS supplied is supplied, by chiefd's id and in chiefd's order. The
//     selection decides nothing: it maps ids to tools somebody else built;
//   - the extension tools come from the REGISTRATION, never from a list in
//     this app. A second inventory of `org_*` names here is the same class of
//     bug as a second roster;
//   - what is NOT supplied is REPORTED. Dropping an id silently is exactly how
//     "the CEO has no tools" stayed invisible.
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import type { ExtensionAPI } from '@earendil-works/pi-coding-agent'
import { Type } from 'typebox'
import { describe, expect, it } from 'vitest'

import { selectTools } from '@/server/AgentTools'
import type { AgentProfile } from '@/types/AgentHost'
import type { ExtensionInstaller } from '@/types/ExtensionTools'

/** A real directory, because every coding tool is scoped to the person's own
 * workspace and a constructor is entitled to look at the path it is given. */
function profile(granted: readonly string[]): AgentProfile {
  const root = mkdtempSync(join(tmpdir(), 'agent-tools-'))
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

/** A stand-in for the organization extension: it registers `org_*` tools the
 * way the real one does, without needing that person's live daemon. */
function registering(...names: readonly string[]): readonly ExtensionInstaller[] {
  return [
    (pi: ExtensionAPI) => {
      for (const name of names) {
        pi.registerTool({
          name,
          label: name,
          description: `fixture tool ${name}`,
          parameters: Type.Object({}),
          execute: async () => ({ content: [{ type: 'text', text: name }], details: { ok: true } })
        })
      }
    }
  ]
}

/** The tools by the NAME the model sees, never by object identity: the
 * contract is that the id chiefd granted resolves to a tool of that name.
 * Identity would pass just as happily on a tool built for the wrong id. */
async function names(
  granted: readonly string[],
  installers: readonly ExtensionInstaller[] = []
): Promise<string[]> {
  const selection = await selectTools(profile(granted), installers)
  return selection.tools.map((tool) => tool.name)
}

describe('selectTools', () => {
  it('supplies Pi’s tool for every coding id this host builds itself', async () => {
    // The seven coding tools, by the ids chiefd uses for them. A missing entry
    // here is a capability the agent silently does not have.
    expect(await names(['read', 'bash', 'edit', 'write', 'grep', 'find', 'ls'])).toEqual([
      'read',
      'bash',
      'edit',
      'write',
      'grep',
      'find',
      'ls'
    ])
  })

  it('supplies the org_* family it used to report as unavailable', async () => {
    // THE HEART OF THE SECOND DEFECT. These are the ids that make a CEO a CEO
    // — hire, message, read the roster — and they came back unavailable for
    // every hosted person because nothing adapted the extension's
    // registration into the harness's tool list.
    const granted = ['read', 'org_send', 'org_roster', 'org_hire']
    const selection = await selectTools(
      profile(granted),
      registering('org_send', 'org_roster', 'org_hire')
    )

    expect(selection.tools.map((tool) => tool.name)).toEqual(granted)
    expect(selection.unavailable).toEqual([])
  })

  it('preserves chiefd’s order rather than imposing its own', async () => {
    // chiefd's list IS ordered, and the harness is built from it in that order,
    // so a selection that sorted or grouped would hand the model a different
    // tool list than the one the company granted. `AgentHost.sameProfile`
    // compares grants order-sensitively for the same reason.
    expect(await names(['ls', 'bash', 'read'])).toEqual(['ls', 'bash', 'read'])
    expect(await names(['org_hire', 'read'], registering('org_hire'))).toEqual(['org_hire', 'read'])
  })

  it('reports an id no extension registered instead of dropping it', async () => {
    // The installer list here is a stand-in that registers exactly
    // `org_send`, so `org_does_not_exist` is an id nothing in THIS
    // selection registered — and nothing anywhere in the tree registers it
    // either, which is the shape the assertion is about. (In production every id a
    // CEO is granted is now built; this is the behavior that keeps the next
    // gap visible.) An agent that says it cannot beats one that looks like it
    // can.
    const selection = await selectTools(
      profile(['read', 'org_send', 'org_does_not_exist']),
      registering('org_send')
    )

    expect(selection.tools.map((tool) => tool.name)).toEqual(['read', 'org_send'])
    expect(selection.unavailable).toEqual(['org_does_not_exist'])
  })

  it('sorts the unavailable ids so two identical grants read identically', async () => {
    // Reported per person and surfaced on the roster, so the order must come
    // from the ids and not from where they happened to sit in chiefd's list.
    const selection = await selectTools(profile(['org_hire', 'bash', 'org_transfer']), [])

    expect(selection.unavailable).toEqual(['org_hire', 'org_transfer'])
  })

  it('never invents a tool for an id nobody granted', async () => {
    // The selection grants only what chiefd named — including the extension
    // tools, which are installed in full and then filtered by the grant, the
    // same way Pi's own `--tools` allowlist filters a pane.
    expect(await names(['read'], registering('org_hire', 'org_send'))).toEqual(['read'])
  })

  it('gives an empty grant no tools and no complaint', async () => {
    // A person chiefd granted nothing is not degraded — they are a person with
    // no tools, on purpose. Reporting them as missing something would make the
    // roster's `degraded` list meaningless.
    const selection = await selectTools(profile([]), [])

    expect(selection.tools).toEqual([])
    expect(selection.unavailable).toEqual([])
  })

  it('refuses an extension tool that shadows a built-in one', async () => {
    // chiefd's materializer refuses this upstream (`PROTECTED_TOOL_SHADOWED`).
    // Refusing here too means this host can never be the place a person's
    // `bash` quietly becomes somebody else's `bash`.
    await expect(selectTools(profile(['bash']), registering('bash'))).rejects.toThrow(
      'extension tool "bash" shadows a built-in tool'
    )
  })

  it('hands back the binding the harness needs, on every selection', async () => {
    // The extension tools act on the harness they are used to build, so the
    // selection carries the late binding rather than leaving the caller to
    // find it. A selection without one is a tool set whose circuit breakers
    // cannot stop a turn.
    expect(typeof (await selectTools(profile([]), [])).bind).toBe('function')
  })
})
