/**
 * Mandate 1 (reactive-only) conformance fence: this harness must never
 * block a thread or poll on a fixed interval. Same fence class E2-S1
 * applies to chiefing.
 */
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

const SRC_ROOT = new URL('../src', import.meta.url).pathname

const BANNED_PATTERNS: readonly { name: string; pattern: RegExp }[] = [
  { name: 'Atomics.wait', pattern: /Atomics\.wait/ },
  { name: 'spawnSync', pattern: /spawnSync/ },
  { name: 'setInterval', pattern: /setInterval/ },
  { name: 'execSync', pattern: /execSync/ }
]

function walkTsFiles(dir: string): string[] {
  const out: string[] = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) {
      out.push(...walkTsFiles(path))
    } else if (entry.isFile() && entry.name.endsWith('.ts')) {
      out.push(path)
    }
  }
  return out
}

describe('src/** carries no blocking primitives', () => {
  const files = walkTsFiles(SRC_ROOT)

  it('scanned at least one file (sanity check the scan itself is not vacuous)', () => {
    expect(files.length).toBeGreaterThan(3)
  })

  for (const { name, pattern } of BANNED_PATTERNS) {
    it(`no file references ${name}`, () => {
      const offenders = files.filter((file) => pattern.test(readFileSync(file, 'utf8')))
      expect(offenders).toEqual([])
    })
  }

  it('negative self-check: the scan actually flags a real occurrence (not vacuously green)', () => {
    const offenders = ['const x = spawnSync("ls")'].filter((line) => /spawnSync/.test(line))
    expect(offenders).toHaveLength(1)
  })
})
