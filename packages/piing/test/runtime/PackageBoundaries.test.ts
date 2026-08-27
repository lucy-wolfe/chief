/**
 * The epic's boundary fence (grows with every later E3 story): `src/**` never
 * reads `process.env`/`process.cwd` directly, never calls `fetch(`, and never
 * imports the legacy tree. Scoped to `src/` only — the asset trees under
 * `packages/piing/extensions`/`skills` run inside Pi agent processes and
 * legitimately read their pane env once E3-S7 fills them.
 */
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const SRC_ROOT = fileURLToPath(new URL('../../src', import.meta.url))

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

// Strips comments so matches below check actual code, not provenance
// comments that legitimately mention these forbidden patterns while
// explaining that the module never uses them.
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '')
}

describe('src/** stays inside the piing boundary fence', () => {
  const files = walkTsFiles(SRC_ROOT)

  it('scanned at least one file (sanity check the scan itself is not vacuous)', () => {
    expect(files.length).toBeGreaterThan(10)
  })

  it('no file references process.env outside a comment', () => {
    const offenders = files.filter((file) =>
      stripComments(readFileSync(file, 'utf8')).includes('process.env')
    )
    expect(offenders).toEqual([])
  })

  it('no file references process.cwd outside a comment', () => {
    const offenders = files.filter((file) =>
      stripComments(readFileSync(file, 'utf8')).includes('process.cwd')
    )
    expect(offenders).toEqual([])
  })

  it('no file calls fetch( outside a comment', () => {
    const offenders = files.filter((file) =>
      stripComments(readFileSync(file, 'utf8')).includes('fetch(')
    )
    expect(offenders).toEqual([])
  })

  it('no file imports the legacy tree (../src/ or apps/cli outside this package)', () => {
    const legacyImportPattern = /from\s+['"](\.\.\/)+(src|extensions|apps\/cli)\//
    const offenders = files.filter((file) => legacyImportPattern.test(readFileSync(file, 'utf8')))
    expect(offenders).toEqual([])
  })

  it('negative self-check: the scan actually flags real forbidden usage (not vacuously green)', () => {
    const offenders = ['const x = process.env.FOO', 'await fetch(url)'].filter(
      (line) =>
        stripComments(line).includes('process.env') || stripComments(line).includes('fetch(')
    )
    expect(offenders).toHaveLength(2)
  })
})
