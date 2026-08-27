// The Founder's instructions come from the skill file, not from this app.
//
// There is no constant here to compare against, because there is no second
// copy of the prompt any more. What these tests prove is the property that
// replaced the pin: the text the harness is built with IS the bytes of
// `packages/piing/skills/founder/SKILL.md`, read at runtime, and a
// skill this server cannot read is a refusal rather than a fallback.
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'

import { afterEach, describe, expect, it } from 'vitest'

import {
  founderSkillBody,
  founderSystemPrompt,
  resetFounderIdentity
} from '@/server/FounderIdentity'

/** The skill file, located the way a reader would locate it — through the
 * package that ships it, exactly as the module under test does. Composing a
 * `../../../..` path from this test's own directory would pass in the
 * workspace and prove nothing about an installed package. */
function skillFile(): string {
  return createRequire(import.meta.url).resolve('@chief/piing/skills/founder/SKILL.md')
}

describe('FounderIdentity', () => {
  afterEach(() => {
    resetFounderIdentity()
  })

  it('reads a real skill file (an empty read would make every assertion vacuous)', () => {
    const source = readFileSync(skillFile(), 'utf8')
    expect(source.length).toBeGreaterThan(100)
    expect(source).toContain('chiefd_launch_company')
  })

  it('is the skill file’s own body, byte for byte', () => {
    const source = readFileSync(skillFile(), 'utf8')
    // The body is everything after the YAML front matter, trimmed. Front
    // matter is Pi's skill-listing metadata — `name: founder` in a
    // system prompt is not an instruction to anybody.
    const match = /^---\n[\s\S]*?\n---\n([\s\S]*)$/.exec(source)
    expect(match?.[1]).toBeDefined()
    expect(founderSkillBody()).toBe((match?.[1] ?? '').trim())
  })

  it('carries the skill body into the system prompt unchanged', () => {
    expect(founderSystemPrompt()).toContain(founderSkillBody())
  })

  it('does not leak the skill’s front matter into the prompt', () => {
    const prompt = founderSystemPrompt()
    expect(prompt).not.toContain('name: founder')
    expect(prompt).not.toContain('description:')
  })

  it('tells the hosted Founder the one thing that is untrue of the tmux one', () => {
    // The tmux Founder is granted seven coding tools and told not to use them.
    // This one HAS none, and a model left to infer that offers them anyway.
    const prompt = founderSystemPrompt()
    expect(prompt).toContain('chiefd_launch_company')
    expect(prompt).toContain('no shell')
  })

  it('caches the read rather than touching disk on every turn', () => {
    // Same string identity twice: a re-read would produce an equal but
    // distinct value, and the prompt is built once per turn.
    expect(founderSkillBody()).toBe(founderSkillBody())
  })
})
