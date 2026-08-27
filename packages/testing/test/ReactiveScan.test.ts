import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { describe, expect, test } from 'vitest'

import { REACTIVE_ALLOWLIST } from '../../../scripts/reactive-allowlist'
import { scan } from '../../../scripts/reactive-scan'

// #827 (E8-S5): reactive-scan is the standing gate for Mandate 1
// (reactive-only) — this test makes a scan regression a TEST failure, not
// just a CLI exit code a CI shard could silently skip past.

// #827/E8-S5's own scope explicitly excludes `org-runtime.ts` (owned by
// #826/E8-S4's tmux/pane convergence-wait loops) — see plan Step 8's "known
// gap, flagged not hidden". Triaging those sites here would mean judging
// another story's code, which is out of bounds; leaving the wrapper test
// asserting a hard zero would mean it can never pass until #826 lands, even
// though this story's own register (20 entries, zero stale rows) is
// complete and correct. So the three specific, known, out-of-scope sites are
// named and the assertion is a SUBSET check, not equality and not zero: it
// passes now with exactly these three outstanding, keeps passing (vacuously)
// once #826 lands and triages them away, and still fails the instant any
// *other*, unexpected untriaged site appears anywhere in the tree.
const KNOWN_UNTRIAGED_PENDING_826 = new Set<string>([])

describe('reactive-scan', () => {
  test('passes on the real tree, modulo #826/E8-S4-owned org-runtime.ts sites', () => {
    const result = scan()
    const unexpectedUntriaged = result.untriaged.filter(
      (s) => !KNOWN_UNTRIAGED_PENDING_826.has(`${s.file}:${s.line}:${s.primitive}`)
    )
    if (
      unexpectedUntriaged.length > 0 ||
      result.staleAllowlist.length > 0 ||
      result.unclassified.length > 0
    ) {
      const detail = [
        ...unexpectedUntriaged.map(
          (s) => `untriaged: ${s.file}:${s.line} [${s.primitive}] ${s.text}`
        ),
        ...result.staleAllowlist.map((e) => `stale: ${e.file} [${e.primitive}]`),
        ...result.unclassified.map((e) => `unclassified: ${e.file} [${e.primitive}]`)
      ].join('\n')
      throw new Error(`reactive-scan failed:\n${detail}`)
    }
    expect(unexpectedUntriaged).toEqual([])
    expect(result.staleAllowlist).toEqual([])
    expect(result.unclassified).toEqual([])
  })

  test('the allowlist is non-empty and every entry names one of the five allowed classes', () => {
    expect(REACTIVE_ALLOWLIST.length).toBeGreaterThan(0)
    const classes = [
      'deadline',
      'render-clock',
      'external-protocol',
      'os-liveness',
      'bounded-retry'
    ]
    for (const entry of REACTIVE_ALLOWLIST) {
      const namesAClass = classes.some((c) => entry.reason.toLowerCase().includes(c))
      expect(
        namesAClass,
        `${entry.file} [${entry.primitive}] reason must name a class: ${entry.reason}`
      ).toBe(true)
    }
  })

  test('fails on an injected unlisted setInterval (untriaged)', () => {
    const dir = mkdtempSync(join(tmpdir(), 'reactive-scan-test-'))
    try {
      // Fixture must live where the scanner looks: apps/<name>/src/** or
      // packages/piing/extensions/** or packages/<name>/src/**.
      const appsDir = join(dir, 'apps', 'fixture-app', 'src')
      mkdirSync(appsDir, { recursive: true })
      writeFileSync(
        join(appsDir, 'Unlisted.ts'),
        'export function poll() { setInterval(() => { readSomething() }, 1000) }\n'
      )
      const result = scan(dir, { allowlist: [] })
      expect(result.ok).toBe(false)
      expect(
        result.untriaged.some(
          (s) => s.file === 'apps/fixture-app/src/Unlisted.ts' && s.primitive === 'setInterval'
        )
      ).toBe(true)
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test('a stale allowlist entry (site removed but entry kept) fails the gate', () => {
    const dir = mkdtempSync(join(tmpdir(), 'reactive-scan-test-'))
    try {
      const appsDir = join(dir, 'apps', 'fixture-app', 'src')
      mkdirSync(appsDir, { recursive: true })
      writeFileSync(join(appsDir, 'Empty.ts'), 'export const nothing = 1\n')
      const result = scan(dir, {
        allowlist: [
          {
            file: 'apps/fixture-app/src/Empty.ts',
            primitive: 'setInterval',
            match: 'setInterval(poll, 1000)',
            reason: 'deadline: does not exist'
          }
        ]
      })
      expect(result.ok).toBe(false)
      expect(result.staleAllowlist).toHaveLength(1)
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  // #966/#967: the three regression tests the (file, primitive, match) key
  // exists to make possible -- none of these were expressible under the old
  // (file, primitive) key, because that key could not tell "the blessed line
  // moved within its file" apart from "the blessed line moved to a DIFFERENT
  // file" apart from "a second, different, unrelated site now shares this
  // file+primitive with the blessed one".

  test('a blessed site that merely shifted lines within the same file still matches -- text is the anchor, not position', () => {
    const dir = mkdtempSync(join(tmpdir(), 'reactive-scan-test-'))
    try {
      const appsDir = join(dir, 'apps', 'fixture-app', 'src')
      mkdirSync(appsDir, { recursive: true })
      // Padding above the real site shifts its line number relative to
      // wherever the allowlist entry might have been authored -- irrelevant
      // to a text-keyed match.
      writeFileSync(
        join(appsDir, 'Shifted.ts'),
        '// padding\n'.repeat(20) + 'export function poll() { setInterval(tick, 1000) }\n'
      )
      const result = scan(dir, {
        allowlist: [
          {
            file: 'apps/fixture-app/src/Shifted.ts',
            primitive: 'setInterval',
            match: 'export function poll() { setInterval(tick, 1000) }',
            reason: 'deadline: fixture'
          }
        ]
      })
      expect(result.ok).toBe(true)
      expect(result.untriaged).toEqual([])
      expect(result.staleAllowlist).toEqual([])
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test('a site moved to a DIFFERENT file fails loudly on BOTH sides -- the exact #963 shape', () => {
    const dir = mkdtempSync(join(tmpdir(), 'reactive-scan-test-'))
    try {
      const appsDir = join(dir, 'apps', 'fixture-app', 'src')
      mkdirSync(appsDir, { recursive: true })
      // The real site now lives in NewHome.ts; the allowlist entry still
      // names OldHome.ts (which no longer contains it) -- a pure file move,
      // zero code change, exactly what #963 did to two Atomics.wait sites.
      writeFileSync(
        join(appsDir, 'NewHome.ts'),
        'export function poll() { setInterval(tick, 1000) }\n'
      )
      const result = scan(dir, {
        allowlist: [
          {
            file: 'apps/fixture-app/src/OldHome.ts',
            primitive: 'setInterval',
            match: 'export function poll() { setInterval(tick, 1000) }',
            reason: 'deadline: fixture'
          }
        ]
      })
      expect(result.ok).toBe(false)
      // The moved-to file's site has no covering entry (unregistered).
      expect(
        result.untriaged.some(
          (s) => s.file === 'apps/fixture-app/src/NewHome.ts' && s.primitive === 'setInterval'
        )
      ).toBe(true)
      // The moved-from file's entry no longer matches anything (orphaned).
      expect(result.staleAllowlist.some((e) => e.file === 'apps/fixture-app/src/OldHome.ts')).toBe(
        true
      )
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test('a second, different site sharing (file, primitive) with a blessed one is untriaged on its own -- the exact LOCK_SLEEPER (#967) shape', () => {
    const dir = mkdtempSync(join(tmpdir(), 'reactive-scan-test-'))
    try {
      const appsDir = join(dir, 'apps', 'fixture-app', 'src')
      mkdirSync(appsDir, { recursive: true })
      writeFileSync(
        join(appsDir, 'TwoSites.ts'),
        'export function pollA() { setInterval(tickA, 1000) }\n' +
          'export function pollB() { setInterval(tickB, 2000) }\n'
      )
      const result = scan(dir, {
        allowlist: [
          {
            file: 'apps/fixture-app/src/TwoSites.ts',
            primitive: 'setInterval',
            match: 'export function pollA() { setInterval(tickA, 1000) }',
            reason: 'deadline: fixture -- only pollA was ever reviewed'
          }
        ]
      })
      expect(result.ok).toBe(false)
      // pollA matches its entry and is NOT untriaged; pollB shares the same
      // (file, primitive) under the old key but has distinct text, so under
      // the new key it correctly stands alone, unreviewed.
      expect(
        result.untriaged.some(
          (s) => s.text === 'export function pollB() { setInterval(tickB, 2000) }'
        )
      ).toBe(true)
      expect(
        result.untriaged.some(
          (s) => s.text === 'export function pollA() { setInterval(tickA, 1000) }'
        )
      ).toBe(false)
      expect(result.staleAllowlist).toEqual([])
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test('two real sites sharing byte-identical text in the same file each require their own allowlist entry -- bag, not set, semantics', () => {
    const dir = mkdtempSync(join(tmpdir(), 'reactive-scan-test-'))
    try {
      const appsDir = join(dir, 'apps', 'fixture-app', 'src')
      mkdirSync(appsDir, { recursive: true })
      const identicalLine = 'setInterval(tick, 1000)'
      // Two distinct call sites (different enclosing branches) whose trimmed
      // source text is byte-identical -- the same shape as this story's own
      // org-fresh-session-transaction.ts entries (two functions, one
      // identical retry-sleep line each).
      writeFileSync(
        join(appsDir, 'Duplicate.ts'),
        `if (a) {\n  ${identicalLine}\n} else {\n  ${identicalLine}\n}\n`
      )

      // Deliberately wrong: byte-identical text appears twice in the real
      // tree, but only one entry blesses it. A set-based check would let
      // this pass (the text "has been blessed somewhere"); the bag check
      // must not -- the second occurrence must still be untriaged.
      const singleResult = scan(dir, {
        allowlist: [
          {
            file: 'apps/fixture-app/src/Duplicate.ts',
            primitive: 'setInterval',
            match: identicalLine,
            reason: 'deadline: fixture'
          }
        ]
      })
      expect(singleResult.ok).toBe(false)
      expect(singleResult.untriaged).toHaveLength(1)
      expect(singleResult.staleAllowlist).toEqual([])

      // Correct: one entry per real occurrence, even though the text is
      // identical -- both are reviewed on their own terms, matching this
      // story's own org-fresh-session-transaction.ts/chiefd-process.ts rows.
      const pairResult = scan(dir, {
        allowlist: [
          {
            file: 'apps/fixture-app/src/Duplicate.ts',
            primitive: 'setInterval',
            match: identicalLine,
            reason: 'deadline: fixture, occurrence 1 of 2'
          },
          {
            file: 'apps/fixture-app/src/Duplicate.ts',
            primitive: 'setInterval',
            match: identicalLine,
            reason: 'deadline: fixture, occurrence 2 of 2'
          }
        ]
      })
      expect(pairResult.ok).toBe(true)
      expect(pairResult.untriaged).toEqual([])
      expect(pairResult.staleAllowlist).toEqual([])
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })
})
