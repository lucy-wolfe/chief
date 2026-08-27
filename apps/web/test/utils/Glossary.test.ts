/**
 * E6-S7's (#812) own copy fence: walks every user-facing string module this
 * story ships and mechanically checks the glossary rules from chief/CLAUDE.md
 * — "assign"/"assignment" appears only as a
 * verb, generic filler never labels the concept, and "owned task" is
 * banned entirely. Reads files from disk (not import) for the same reason
 * `MandateFence.test.ts` does: a banned string behind a type error must
 * still fail this test.
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, relative, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const here = dirname(fileURLToPath(import.meta.url))
// DERIVED, not listed. Every directory under `src/components` is a
// user-facing copy root, and a hand-written list is a coverage gap that opens
// silently the day somebody adds a directory: `shell/` had existed unscanned,
// and nothing went red. The rule is "all of them", which needs no maintenance
// and cannot go stale. `src/server` and `src/app` are deliberately NOT scanned:
// they carry the internal `assignments` KEYSPACE, which chief/CLAUDE.md's
// glossary exempts by name ("internal identifiers, param names, keyspaces and
// log sources are exempt — this glossary governs user-facing copy").
const componentsRoot = join(here, '..', '..', 'src', 'components')
const roots = readdirSync(componentsRoot, { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => join(componentsRoot, entry.name))

interface SourceFile {
  relativePath: string
  contents: string
}

function walk(dir: string): string[] {
  const stat = statSync(dir, { throwIfNoEntry: false })
  if (!stat || !stat.isDirectory()) return []
  const entries = readdirSync(dir, { withFileTypes: true })
  const files: string[] = []
  for (const entry of entries) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) files.push(...walk(full))
    else if (entry.isFile() && (full.endsWith('.ts') || full.endsWith('.tsx'))) files.push(full)
  }
  return files
}

function readSourceFiles(): SourceFile[] {
  return roots
    .flatMap((root) => walk(root))
    .map((path) => ({
      relativePath: relative(join(here, '..', '..'), path),
      contents: readFileSync(path, 'utf8')
    }))
}

// A banned noun use of "assignment"/"assign" — a following article/possessive
// or "the"/"an"/"your" makes it read as a noun naming a tracked
// thing, not a verb ("assigned to @val").
const NOUN_ASSIGNMENT_PATTERN = /\b(the|an|your|this|owned)\s+assignment\b/i
const BANNED_FILLER = /\bwork item\b|\bobjective\b|\bowned task\b/i
// "delegation" as a noun (not the verb "delegate"/"delegated").
const NOUN_DELEGATION_PATTERN = /\bdelegation\b/i

describe('Glossary (goal 🎯)', () => {
  const files = readSourceFiles()

  it('scanned at least one source file', () => {
    expect(files.length).toBeGreaterThan(0)
  })

  // The derivation itself, checked: a scan that silently covers three of four
  // directories reports the same clean result as one that covers all four.
  // This is the assertion that would have caught `shell/` going unscanned.
  it('scans EVERY directory under src/components, derived from the tree', () => {
    const scanned = new Set(
      files.map((file) => file.relativePath.split(sep).join('/').split('/')[2])
    )
    const onDisk = readdirSync(componentsRoot, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => entry.name)
    expect(onDisk.length).toBeGreaterThan(0)
    expect([...scanned].sort()).toEqual([...onDisk].sort())
  })

  it('never uses "assignment"/"assign" as a noun naming a tracked thing', () => {
    const offenders = files.filter((file) => NOUN_ASSIGNMENT_PATTERN.test(file.contents))
    expect(offenders.map((file) => file.relativePath)).toEqual([])
  })

  it('never uses "delegation" as a noun', () => {
    const offenders = files.filter((file) => NOUN_DELEGATION_PATTERN.test(file.contents))
    expect(offenders.map((file) => file.relativePath)).toEqual([])
  })

  it('never uses banned filler ("work item", "objective", "owned task") to label a goal', () => {
    const offenders = files.filter((file) => BANNED_FILLER.test(file.contents))
    expect(offenders.map((file) => file.relativePath)).toEqual([])
  })

  // A 2-line window (the emoji's own line plus its neighbors) tolerates
  // prose comments that wrap a sentence across lines, while still catching
  // a genuinely unlabeled or mislabeled use.
  function hasNearbyWord(lines: readonly string[], index: number, word: RegExp): boolean {
    const window = [lines[index - 1], lines[index], lines[index + 1]]
      .filter((line): line is string => typeof line === 'string')
      .join(' ')
    return word.test(window)
  }

  it('🎯 only appears in a goal-vocabulary context (goal/active/urgent/high/normal/low/manager/delegated)', () => {
    const goalWords = /goal|active|urgent|high|normal|low|manager|delegated|supervision/i
    for (const file of files) {
      if (!file.contents.includes('🎯')) continue
      const lines = file.contents.split('\n')
      lines.forEach((line, index) => {
        if (!line.includes('🎯')) return
        expect(
          hasNearbyWord(lines, index, goalWords),
          `${file.relativePath}: "${line.trim()}"`
        ).toBe(true)
      })
    }
  })
})
