import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'

import { expect, it } from 'vitest'

import { piingPackageRoot } from '@/runtime/PiPaths'

function* walk(directory: string): Generator<string> {
  for (const entry of readdirSync(directory)) {
    const path = join(directory, entry)
    if (statSync(path).isDirectory()) yield* walk(path)
    else if (/\.ts$/.test(entry)) yield path
  }
}

/** The trees that materialize and run a person's Pi home, and therefore the
 * trees that could touch Pi's own files.
 *
 * `apps/cli/src/legacy` until ca2da9b57 deleted it, then `apps/cli/src` until
 * P3 deleted the TypeScript CLI itself. Each time the walk threw ENOENT on a
 * clean clone and passed on every warm one, where the deleted directory still
 * sat on disk — the same invisible-dangling-reference shape that broke
 * `bun run release`. Repointed at the launcher sources that exist rather than
 * softened: the invariant (no launcher code touches Pi's own home files) is
 * unchanged and still has a real subject. */
function launcherRoots(): string[] {
  return [join(piingPackageRoot(), 'src'), join(piingPackageRoot(), 'extensions')]
}

function launcherSources(): string[] {
  return launcherRoots().flatMap((root) => [...walk(root)])
}

it('the launcher-source scan below is not vacuous — it reads real files from both trees', () => {
  // Without this, the three invariants below are satisfiable by finding
  // nothing: a walk over a tree that has moved or emptied reports zero
  // offenders and reads as a pass. The previous walk target was deleted
  // outright and this file threw instead of lying, which was luck — an
  // emptied directory would have gone silently green.
  for (const root of launcherRoots()) {
    expect([...walk(root)].length, `${root} contributed no files to the scan`).toBeGreaterThan(0)
  }
  expect(launcherSources().length).toBeGreaterThan(1)
})

it('no launcher code ever reads or parses auth.json — Pi owns credentials end to end', () => {
  const offenders: string[] = []
  for (const path of launcherSources()) {
    const lines = readFileSync(path, 'utf8').split('\n')
    for (const [index, line] of lines.entries()) {
      if (!line.includes('auth.json')) continue
      if (/readFileSync|readFile|createReadStream|JSON\.parse|await import|require\(/.test(line)) {
        offenders.push(`${path}:${index + 1}: ${line.trim()}`)
      }
    }
  }
  expect(offenders).toEqual([])
})

it('no launcher code writes, reads, or validates keybindings.json — Pi uses its own defaults', () => {
  const offenders: string[] = []
  for (const path of launcherSources()) {
    const lines = readFileSync(path, 'utf8').split('\n')
    for (const [index, line] of lines.entries()) {
      if (!line.includes('keybindings')) continue
      if (
        /writeFileSync|writeJson|readFileSync|readFile|JSON\.parse|cpSync|symlinkSync/.test(line)
      ) {
        offenders.push(`${path}:${index + 1}: ${line.trim()}`)
      }
    }
  }
  expect(offenders).toEqual([])
})

it('no launcher code generates a per-agent or operator models.json catalog', () => {
  const offenders: string[] = []
  for (const path of launcherSources()) {
    const source = readFileSync(path, 'utf8')
    if (source.includes('writeManagedModelCatalog')) {
      offenders.push(`${path}: writeManagedModelCatalog reference`)
    }
    for (const [index, line] of source.split('\n').entries()) {
      if (!line.includes('models.json')) continue
      if (/writeFileSync|writeJson|renameSync|cpSync|symlinkSync/.test(line)) {
        offenders.push(`${path}:${index + 1}: ${line.trim()}`)
      }
    }
  }
  expect(offenders).toEqual([])
})
