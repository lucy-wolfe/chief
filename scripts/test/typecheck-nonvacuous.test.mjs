// #848: `bun run typecheck`'s legacy leg (`tsconfig.extensions.json`) silently
// checked ~0 files after E4-S1/#787 moved `src/` to
// `apps/cli/src/legacy/**` — the include patterns still "resolved" (2
// non-empty strings), so the EXISTING non-vacuity guard
// (`assertNonVacuous`, built for #3081's different failure shape: a
// solution-style config checked with `-p` instead of `-b`) stayed green
// throughout, because it counts SYNTACTIC include patterns, not real files
// on disk. #785 then moved the extension root into the Piing package, where a
// total-only floor would let the 108-file legacy tree mask a zero-file package
// extension root. The per-include floor below closes that second failure mode.
//
// `assertMinimumRealFiles` (in `../assert-typecheck-nonvacuous.mjs`) is the
// fix: it walks each `include` pattern's root directory and counts real
// `.ts` files, so a scan root that still "resolves" as a non-empty pattern
// but points at a deleted or near-empty directory is caught. This file
// proves five things, matching the shape `cargo-test-floor.test.mjs`
// established for the analogous `cargo test` executed-count floor:
//
//   1. The counting logic itself is correct against controlled fixture
//      trees (nested dirs, non-`.ts` files, `node_modules`/`.git`/`target`
//      excluded).
//   2. `assertMinimumRealFiles` throws, naming the shortfall and citing
//      #848, when the real count is below the floor.
//   3. It does NOT throw when the real count clears the floor.
//   4. THE ACTUAL #848 REGRESSION, reproduced structurally rather than as a
//      synthetic minimal case: a fixture config shaped exactly like
//      `tsconfig.extensions.json` was during the incident (`include` naming a
//      deleted legacy root alongside a real package-extension root) fails the
//      guard at the real historical floor.
//   5. The guard is actually WIRED into `scripts/typecheck.sh` for the
//      legacy leg, and the real `tsconfig.extensions.json` clears the real
//      floor today — a guard proven correct in isolation but never wired
//      to the thing it exists to protect is the #852 lesson repeating
//      itself one file over.
//
// Per team-lead's explicit ask on #848: prove the guard FIRES, not just
// that it is present — a guard never seen to fail is indistinguishable
// from one that cannot fail.
//
// Follow-up, same session: "are there other legs in that script whose file
// count nobody has ever looked at?" There was one —
// `tsconfig.capabilities.json`'s package-skills leg had NO non-vacuity
// protection at all, not even the syntactic `assertNonVacuous` check,
// because it's a plain (non-solution-style) config with a single include
// pattern. That leg no longer exists: the package skills stopped shipping
// TypeScript, so the config and the leg were DELETED rather than floored to
// zero (see the note at the end of `scripts/typecheck.sh`). Its own section
// went with it; section 6 below keeps the part that outlived it — the
// permanent sweep, so a future leg cannot go unguarded silently either.

import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import { assertMinimumFilesForInclude, assertMinimumRealFiles } from '../assert-typecheck-nonvacuous.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const typecheckScript = join(repoRoot, 'scripts', 'typecheck.sh')
const realExtensionsConfig = join(repoRoot, 'tsconfig.extensions.json')

function readTypecheckScript() {
  return execFileSync('cat', [typecheckScript], { encoding: 'utf8' })
}

// The floor `scripts/typecheck.sh` currently wires a given plain config's
// leg to. Read from the live invocation below rather than hardcoded twice,
// so this file cannot silently drift from what actually runs.
function wiredFloorFor(configFileName) {
  const script = readTypecheckScript()
  const pattern = new RegExp(
    `assert-typecheck-nonvacuous\\.mjs\\s+${configFileName.replace('.', '\\.')}\\s+(\\d+)`
  )
  const match = script.match(pattern)
  assert.ok(
    match,
    `scripts/typecheck.sh must wire assert-typecheck-nonvacuous.mjs to ${configFileName} with a floor ` +
      'argument (#848) -- this file could not find that invocation at all, meaning the guard has been ' +
      'un-wired, not just tightened or loosened'
  )
  return Number(match[1])
}

function wiredExtensionsFloor() {
  return wiredFloorFor('tsconfig.extensions.json')
}

const EXTENSIONS_INCLUDE = 'packages/piing/extensions/**/*.ts'

function escapeRegExp(value) {
  return [...value].map((character) => '\\.^$*+?()[]{}|/'.includes(character) ? `\\${character}` : character).join('')
}

function wiredExtensionsIncludeFloor() {
  const script = readTypecheckScript()
  const pattern = new RegExp(
    `assert-typecheck-nonvacuous\\.mjs\\s+tsconfig\\.extensions\\.json\\s+\\d+\\s+--include-floor\\s+['\"]?${escapeRegExp(EXTENSIONS_INCLUDE)}['\"]?\\s+(\\d+)`
  )
  const match = script.match(pattern)
  assert.ok(
    match,
    'scripts/typecheck.sh must wire tsconfig.extensions.json to a package-extension include floor (#785), not only an aggregate floor'
  )
  return Number(match[1])
}

// Every `bun x tsc --noEmit -p <config>` invocation in scripts/typecheck.sh
// (the plain, non-solution-style legs -- the workspace half uses `tsc -b`
// against a solution config and is guarded separately by the existing
// `assertNonVacuous`/pattern-count check) must have a preceding
// `assert-typecheck-nonvacuous.mjs <same config>` call earlier in the file.
// This is the permanent form of team-lead's #848 follow-up question --
// "are there other legs in that script whose file count nobody has ever
// looked at" -- so a FOURTH leg added later without a guard fails this
// test immediately rather than sitting unguarded until it, too, goes
// vacuous and nobody notices for a story's entire lifetime.
function findUnguardedNoEmitLegs(scriptText = readTypecheckScript()) {
  const lines = scriptText.split('\n')
  const unguarded = []
  for (let i = 0; i < lines.length; i += 1) {
    // Only real invocation lines -- NOT the `echo "[typecheck] ... tsc
    // --noEmit -p tsconfig.foo.json"` log lines just above each one, which
    // mention the identical text for a human reading the output and would
    // otherwise false-positive as an unguarded "invocation" appearing
    // before its own guard.
    const isInvocation = /^\s*(?:bun\s+x\s+)?tsc\s+--noEmit\s+-p\s+(\S+)\s*$/.test(lines[i])
    if (!isInvocation) continue
    const match = lines[i].match(/-p\s+(\S+?)\s*$/)
    const config = match[1]
    const guardPattern = new RegExp(`assert-typecheck-nonvacuous\\.mjs\\s+${config.replace('.', '\\.')}\\b`)
    const guardedEarlier = lines.slice(0, i).some((earlier) => guardPattern.test(earlier))
    if (!guardedEarlier) unguarded.push({ config, line: i + 1 })
  }
  return unguarded
}

const dirs = []
function tempDir() {
  const dir = mkdtempSync(join(tmpdir(), 'typecheck-nonvacuous-'))
  dirs.push(dir)
  return dir
}
test.after(() => {
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true })
})

function writeFile(path, contents = '// fixture\n') {
  mkdirSync(dirname(path), { recursive: true })
  writeFileSync(path, contents)
}

function writeTsconfig(dir, include) {
  const path = join(dir, 'tsconfig.json')
  writeFileSync(path, JSON.stringify({ include }, null, 2))
  return path
}

// ---- 1. The counting logic itself, against controlled fixture trees ----

test('counts real .ts files under a nested tree, ignoring non-.ts files and node_modules/.git/target', () => {
  const root = tempDir()
  writeFile(join(root, 'src', 'a.ts'))
  writeFile(join(root, 'src', 'b.ts'))
  writeFile(join(root, 'src', 'nested', 'c.ts'))
  writeFile(join(root, 'src', 'README.md')) // not .ts -- must not count
  writeFile(join(root, 'src', 'node_modules', 'dep', 'index.ts')) // excluded dir
  writeFile(join(root, 'src', '.git', 'hooks', 'pre-commit.ts')) // excluded dir
  writeFile(join(root, 'src', 'target', 'debug', 'x.ts')) // excluded dir

  const config = writeTsconfig(root, ['src/**/*.ts'])
  const total = assertMinimumRealFiles(config, 3)
  assert.equal(total, 3, 'must count exactly the 3 real .ts files, not the excluded/non-.ts ones')
})

test('sums real files across multiple include patterns, matching tsconfig.extensions.json\'s own shape (two roots)', () => {
  const root = tempDir()
  writeFile(join(root, 'legacy', 'one.ts'))
  writeFile(join(root, 'legacy', 'two.ts'))
  writeFile(join(root, 'packages', 'piing', 'extensions', 'three.ts'))

  const config = writeTsconfig(root, ['legacy/**/*.ts', 'packages/piing/extensions/**/*.ts'])
  const total = assertMinimumRealFiles(config, 3)
  assert.equal(total, 3)
})

// ---- 2/3. Fires below the floor, does not fire at/above it --------------

test('throws, naming the shortfall and #848, when the real count is below the floor', () => {
  const root = tempDir()
  writeFile(join(root, 'src', 'only-one.ts'))
  const config = writeTsconfig(root, ['src/**/*.ts'])

  assert.throws(
    () => assertMinimumRealFiles(config, 5),
    (/** @type {Error} */ error) => {
      assert.match(error.message, /only 1 real file\(s\)/)
      assert.match(error.message, /below the expected floor of 5/)
      assert.match(error.message, /#848/)
      return true
    }
  )
})

test('does not throw when the real count clears the floor', () => {
  const root = tempDir()
  writeFile(join(root, 'src', 'a.ts'))
  writeFile(join(root, 'src', 'b.ts'))
  const config = writeTsconfig(root, ['src/**/*.ts'])
  assert.equal(assertMinimumRealFiles(config, 2), 2)
})

test('throws at exactly one file short of the floor, and does not throw exactly at the floor -- an off-by-one on the comparison itself would hide behind a coarser fixture', () => {
  const root = tempDir()
  writeFile(join(root, 'src', 'a.ts'))
  writeFile(join(root, 'src', 'b.ts'))
  const config = writeTsconfig(root, ['src/**/*.ts'])
  assert.throws(() => assertMinimumRealFiles(config, 3))
  assert.doesNotThrow(() => assertMinimumRealFiles(config, 2))
})

// ---- 4. THE ACTUAL #848 REGRESSION, reproduced structurally -------------

test('the actual #848 shape: a config whose include still names a deleted root alongside a real one fails the guard at the historical floor', () => {
  const root = tempDir()
  // Mirrors the incident exactly: the package extension root is real (scaled
  // down here), while the pre-move root the include still names no longer
  // exists on disk at all.
  for (let i = 0; i < 6; i += 1) writeFile(join(root, 'packages', 'piing', 'extensions', `ext-${i}.ts`))
  // Deliberately NOT creating `root/src` at all.
  const config = writeTsconfig(root, ['src/**/*.ts', 'packages/piing/extensions/**/*.ts'])

  // The OLD guard (`assertNonVacuous`, unaffected by this fixture since it
  // only counts syntactic patterns) would have reported this config fine --
  // two non-empty include strings. `assertMinimumRealFiles` is what
  // actually walks the disk and catches the real shortfall (6 files here,
  // matching the incident's 26-vs-136 shape at a smaller scale).
  assert.throws(
    () => assertMinimumRealFiles(config, 20),
    (/** @type {Error} */ error) => {
      assert.match(error.message, /only 6 real file\(s\)/)
      assert.match(error.message, /#848/)
      return true
    },
    'a config citing a deleted scan root alongside a real one must still be caught by the real-file count'
  )
})

test('restoring the deleted root (the actual repair #848 made) clears the same fixture', () => {
  const root = tempDir()
  for (let i = 0; i < 6; i += 1) writeFile(join(root, 'packages', 'piing', 'extensions', `ext-${i}.ts`))
  for (let i = 0; i < 20; i += 1) writeFile(join(root, 'src', `legacy-${i}.ts`))
  const config = writeTsconfig(root, ['src/**/*.ts', 'packages/piing/extensions/**/*.ts'])

  const total = assertMinimumRealFiles(config, 20)
  assert.equal(total, 26, 'repointing (here: creating) the real root must clear the floor again')
})

// ---- 5. Wired into scripts/typecheck.sh, and the REAL config clears it --

test('scripts/typecheck.sh actually wires assert-typecheck-nonvacuous.mjs to tsconfig.extensions.json with a floor argument', () => {
  // Fails loudly (via wiredExtensionsFloor's own assertion) if the invocation is
  // missing entirely -- a guard that is correct in isolation but never
  // called protects nothing, which is exactly how #848 itself went
  // unnoticed: the guard IT displaced (#3081's assertNonVacuous) was real
  // and correct for its own failure shape, just never extended to this one.
  const floor = wiredExtensionsFloor()
  assert.ok(floor > 0, 'the wired floor must be a positive number, not e.g. 0 (which would guard nothing)')
})

test('scripts/typecheck.sh also wires a positive package-extension include floor for tsconfig.extensions.json', () => {
  const floor = wiredExtensionsIncludeFloor()
  assert.ok(floor > 0, 'the package-extension include floor must be positive, not zero (#785)')
})

test('the floor wired into scripts/typecheck.sh has never decreased across its git history -- a ratchet, not a convention', () => {
  let log
  try {
    log = execFileSync('git', ['log', '-p', '--follow', '--reverse', '--', typecheckScript], {
      cwd: repoRoot,
      encoding: 'utf8',
    })
  } catch {
    log = ''
  }
  const pattern = /\+node scripts\/assert-typecheck-nonvacuous\.mjs tsconfig\.legacy\.json (\d+)/g
  const historical = [...log.matchAll(pattern)].map((m) => Number(m[1]))
  // #751/E4: this ratchet was written when the legacy tree only ever grew, so
  // "never lower it" and "the real tree must clear it" could never conflict.
  // E4 dissolves that tree on purpose — the port into Rust took
  // apps/cli/src/legacy/** from ~108 files to 10 — and at that point the two
  // rules DO conflict: a floor the tree can no longer reach makes the leg
  // refuse to run, which is strictly worse than no floor.
  //
  // So the ratchet is applied only over floors the tree can still clear. A
  // floor recorded when the tree was larger than it is now is dropped from
  // the sequence, and re-anchoring below it is permitted; lowering a floor
  // the CURRENT tree could have cleared is still the signal it always was,
  // and still fails here. The companion assertion below (real count >= wired
  // floor) keeps the other half honest, so a floor cannot be re-anchored to
  // something the tree does not actually justify.
  const realFileCount = assertMinimumRealFiles(realExtensionsConfig, 0)
  const clearable = historical.filter((floor) => floor <= realFileCount)
  const sequence = [...clearable, wiredExtensionsFloor()]
  for (let i = 1; i < sequence.length; i += 1) {
    assert.ok(
      sequence[i] >= sequence[i - 1],
      `the legacy typecheck floor decreased from ${sequence[i - 1]} to ${sequence[i]} somewhere in its ` +
        `history (full sequence: ${sequence.join(' -> ')}; real files today: ${realFileCount}) -- a drop ` +
        'is the signal, not the noise (#848); never lower it to make a run pass. Re-anchoring is allowed ' +
        'ONLY when the legacy tree itself shrank past the old floor, which is not the case here.'
    )
  }
  assert.ok(
    wiredExtensionsFloor() > 0,
    'the legacy typecheck floor must stay above zero -- a leg that runs while observing nothing reports ' +
      'success it did not earn (#848)'
  )
})

test('the REAL tsconfig.extensions.json clears the REAL wired floor today, measured against the actual repo tree', () => {
  const floor = wiredExtensionsFloor()
  const total = assertMinimumRealFiles(realExtensionsConfig, floor)
  assert.ok(
    total >= floor,
    `tsconfig.extensions.json resolves ${total} real .ts file(s), which must be at or above the wired floor ` +
      `of ${floor} -- if this assertion is what fails, the floor itself is stale, not this test`
  )
})

test('the REAL tsconfig.extensions.json clears its independent package-extension floor today', () => {
  const floor = wiredExtensionsIncludeFloor()
  const total = assertMinimumFilesForInclude(realExtensionsConfig, EXTENSIONS_INCLUDE, floor)
  assert.ok(
    total >= floor,
    `tsconfig.extensions.json's ${EXTENSIONS_INCLUDE} root resolves ${total} real .ts file(s), which must be at or above ${floor}`
  )
})

test('the package-extension floor fires when that root disappears even though the legacy tree still clears the aggregate floor', () => {
  const tamperedConfig = join(repoRoot, '.tsconfig.legacy.package-root.tampered.785.json')
  writeFileSync(
    tamperedConfig,
    JSON.stringify(
      {
        // A LARGE surviving root plus a missing one. `apps/cli/src/legacy`
        // used to play the large part; #751/P0 deleted it, so the aggregate
        // floor started refusing the fixture before the per-include assertion
        // could demonstrate the hole. `packages/piing/src` is the same shape
        // and still exists.
        include: ['packages/piing/src/**/*.ts', 'packages/piing/extensions-does-not-exist-785/**/*.ts'],
      },
      null,
      2
    )
  )
  try {
    // This is the #785 hole: a large surviving root keeps the aggregate floor
    // green, so only the per-include assertion can refuse the scan.
    assert.doesNotThrow(() => assertMinimumRealFiles(tamperedConfig, wiredExtensionsFloor()))
    const extensionFloor = wiredExtensionsIncludeFloor()
    assert.throws(
      () => assertMinimumFilesForInclude(tamperedConfig, EXTENSIONS_INCLUDE, extensionFloor),
      (/** @type {Error} */ error) => {
        assert.match(error.message, /#848\/#785/)
        assert.match(error.message, new RegExp(`below the expected floor of ${extensionFloor}`))
        return true
      },
      'the package-extension root must be independently required; a large sibling tree must never mask its disappearance'
    )
  } finally {
    rmSync(tamperedConfig, { force: true })
  }
})

// ---- 6. The sweep itself: no NEW leg can go unguarded silently ---------

test('every `tsc --noEmit -p <config>` invocation in scripts/typecheck.sh has a preceding non-vacuity guard for that same config', () => {
  const unguarded = findUnguardedNoEmitLegs()
  assert.deepEqual(
    unguarded,
    [],
    `scripts/typecheck.sh has ${unguarded.length} unguarded tsc --noEmit leg(s), each one a scan that can ` +
      `silently resolve to zero files and still exit 0: ${JSON.stringify(unguarded)} -- add an ` +
      'assert-typecheck-nonvacuous.mjs <config> <floor> call before it, per #848'
  )
})

test('the sweep itself actually detects an unguarded leg -- the SAME detector function, against a fixture script, not a copy-pasted check', () => {
  const fixtureScript = [
    '#!/usr/bin/env bash',
    'node scripts/assert-typecheck-nonvacuous.mjs tsconfig.extensions.json 100',
    'bun x tsc --noEmit -p tsconfig.extensions.json',
    'bun x tsc --noEmit -p tsconfig.brand-new-leg.json', // deliberately unguarded
  ].join('\n')

  const unguarded = findUnguardedNoEmitLegs(fixtureScript)
  assert.deepEqual(unguarded, [{ config: 'tsconfig.brand-new-leg.json', line: 4 }])
})

test('the sweep detector reports nothing unguarded for a fixture where every leg has a preceding guard', () => {
  const fixtureScript = [
    '#!/usr/bin/env bash',
    'node scripts/assert-typecheck-nonvacuous.mjs tsconfig.extensions.json 100',
    'bun x tsc --noEmit -p tsconfig.extensions.json',
    'node scripts/assert-typecheck-nonvacuous.mjs tsconfig.brand-new-leg.json 10',
    'bun x tsc --noEmit -p tsconfig.brand-new-leg.json',
  ].join('\n')

  assert.deepEqual(findUnguardedNoEmitLegs(fixtureScript), [])
})
