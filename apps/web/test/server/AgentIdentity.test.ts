// Who a hosted agent thinks it is.
//
// THE DEFECT THIS PINS: a hosted agent had NO IDENTITY.
// `ApiHostLaunchProfile.displayName` carries "<company> · <title>", and chiefd
// materializes an `AGENTS.md` into the person's own workspace with their
// mandate and how their company works. The harness was built with neither, so
// it ran on Pi's default system prompt. Asked what company it ran, the CEO of
// `webproof-labs` answered: "I don't run any company — I'm Claude, an AI
// assistant created by Anthropic, to be helpful, harmless, and honest."
//
// That is not cosmetic. `AGENTS.md` is where the company's own instructions
// live; an agent that never reads it is not the person chiefd staffed, and
// nothing on any surface says so — the pane is up, the roster says hosted, and
// only a conversation reveals that nobody is home.
//
// Real files in a real temp directory rather than a mocked `readFile`: the
// second property below is that a MISSING file is ordinary, and only a real
// filesystem distinguishes "absent" from "the mock was not configured".
import { mkdtemp, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

import { systemPromptFor } from '@/server/AgentIdentity'

/** A person's workspace, optionally carrying the context file chiefd writes. */
async function workspace(context?: string): Promise<string> {
  const cwd = await mkdtemp(join(tmpdir(), 'agent-identity-'))
  if (typeof context === 'string') await writeFile(join(cwd, 'AGENTS.md'), context, 'utf8')
  return cwd
}

describe('systemPromptFor', () => {
  it('names the person chiefd titled, verbatim', async () => {
    // VERBATIM is the assertion. `displayName` is chiefd's own spelling of the
    // company and the title; a prompt that paraphrased it would introduce a
    // second name for one person, and the answer to "who are you" is the whole
    // product here.
    const prompt = await systemPromptFor({
      cwd: await workspace(),
      displayName: 'webproof-labs · CEO'
    })

    expect(prompt).toContain('webproof-labs · CEO')
  })

  it('tells the agent which directory it is working in', async () => {
    // The same cwd every file and shell tool is scoped to. An agent that does
    // not know where it stands answers about paths it cannot reach.
    const cwd = await workspace()

    const prompt = await systemPromptFor({ cwd, displayName: 'webproof-labs · CEO' })

    expect(prompt).toContain(cwd)
  })

  it('carries the workspace’s AGENTS.md into the prompt', async () => {
    // The company's own instructions. This is the half of the defect that made
    // the agent not merely anonymous but UNBRIEFED: chiefd writes the mandate
    // into the workspace and the harness never opened it.
    const cwd = await workspace('# Mandate\n\nShip the proof-of-work pipeline.\n')

    const prompt = await systemPromptFor({ cwd, displayName: 'webproof-labs · CEO' })

    expect(prompt).toContain('Ship the proof-of-work pipeline.')
    // Named, so the model knows the section is its own workspace document
    // rather than something the operator pasted.
    expect(prompt).toContain('AGENTS.md')
    // The identity survives the file: both facts, not one replacing the other.
    expect(prompt).toContain('webproof-labs · CEO')
  })

  it('still returns a usable identity when there is no AGENTS.md', async () => {
    // MUST NOT THROW. A person materialized by an older build has no
    // `AGENTS.md`, and refusing to host them over a missing document would take
    // a working agent off the air — one person's missing file becoming one
    // person's whole failure, the shape this program keeps re-learning.
    const cwd = await workspace()

    const prompt = await systemPromptFor({ cwd, displayName: 'webproof-labs · CEO' })

    expect(prompt).toContain('webproof-labs · CEO')
    expect(prompt).not.toContain('AGENTS.md')
    expect(prompt.length).toBeGreaterThan(0)
  })

  it('does not swallow the context file when it is empty', async () => {
    // An empty `AGENTS.md` is a company that wrote nothing, not a company
    // without a workspace. The identity line is still the answer to "who are
    // you", so the prompt must remain usable either way.
    const cwd = await workspace('')

    const prompt = await systemPromptFor({ cwd, displayName: 'webproof-labs · CEO' })

    expect(prompt).toContain('webproof-labs · CEO')
  })
})
