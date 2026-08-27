import { readdirSync, readFileSync } from 'node:fs'
import { join, relative } from 'node:path'

import { describe, expect, it } from 'vitest'

import { piingSkillsRoot } from '@/runtime/PiPaths'

const skillsRoot = piingSkillsRoot()

/** Code-bearing subtrees shipped inside a skill — real code, not agent instructions. */
const CODE_SUBTREES = new Set(['scripts', 'runtime', 'agents', 'node_modules'])

/** Absolute paths of every instructional markdown file the agent reads. */
function skillInstructionFiles(): string[] {
  const files: string[] = []
  const walk = (directory: string): void => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const full = join(directory, entry.name)
      if (entry.isDirectory()) {
        if (!CODE_SUBTREES.has(entry.name)) walk(full)
        continue
      }
      if (
        entry.name === 'SKILL.md' ||
        (entry.name.endsWith('.md') && directory.endsWith('references'))
      ) {
        files.push(full)
      }
    }
  }
  walk(skillsRoot)
  return files
}

const FORBIDDEN_PATTERNS: ReadonlyArray<{ label: string; regex: RegExp; fix: string }> = [
  {
    label: 'bun src/cli.ts',
    regex: /\bbun\s+src\/cli\.ts\b/,
    // The remedy must name a verb the binary actually owns. It used to say
    // `chiefd catalog --json`, which `chiefd` refuses — a lint that fixed one
    // dead command by teaching another.
    fix: 'use a real `chiefd` verb (`chiefd`, `chief`, `chief attach <company>`)'
  },
  {
    label: 'bun run <script>',
    regex: /\bbun\s+run\s+(company|department|contract|org|cli|start|help)\b/,
    fix: 'use a real `chiefd` verb (`chief attach <company>`, `chief stop <company>`)'
  },
  {
    label: '$ORG_LAUNCHER_ROOT/src/cli.ts',
    regex: /\$(?:\{)?ORG_LAUNCHER_ROOT(?:\})?\/src\/cli\.ts/,
    fix: 'use the installed `chiefd` binary — it resolves the configured launcher root itself'
  },
  {
    label: 'hardcoded dev-checkout path',
    regex: /\/Developer\/team-launcher(?:-2\.0)?\b|team-launcher-2\.0\/src/,
    fix: "never hardcode the source checkout; the agent's cwd is not the checkout on a real install"
  },
  {
    label: 'retired triber command',
    regex: /\btriber\b/,
    fix: 'use the sole installed `chiefd` command'
  }
]

describe('skill CLI-invocation hygiene (#145, #518)', () => {
  const files = skillInstructionFiles()

  it('has a non-empty corpus and a detector that actually fires', () => {
    expect(files.length).toBeGreaterThan(0)
    const sample = 'Run `bun src/cli.ts catalog --json` and `bun run company -- tree acme`.'
    const hits = FORBIDDEN_PATTERNS.filter((pattern) => pattern.regex.test(sample))
    expect(hits.map((hit) => hit.label)).toContain('bun src/cli.ts')
    expect(hits.map((hit) => hit.label)).toContain('bun run <script>')
    expect(
      FORBIDDEN_PATTERNS.some((pattern) => pattern.regex.test('Run `triber catalog --json`.'))
    ).toBe(true)
  })

  it('contains no dev-source-checkout invocation in an instruction', () => {
    const violations: string[] = []
    for (const file of files) {
      const lines = readFileSync(file, 'utf8').split('\n')
      lines.forEach((line, index) => {
        for (const pattern of FORBIDDEN_PATTERNS) {
          if (pattern.regex.test(line)) {
            violations.push(
              `${relative(skillsRoot, file)}:${index + 1} [${pattern.label}] — ${pattern.fix}\n` +
                `    ${line.trim()}`
            )
          }
        }
      })
    }
    expect(
      violations,
      `dev-checkout invocations found in skill instructions:\n${violations.join('\n')}`
    ).toEqual([])
  })
})
