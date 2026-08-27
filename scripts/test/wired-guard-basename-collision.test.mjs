// #976: a basename under `tests/` colliding with a wired guard under
// `scripts/test/` must fail the build. This is the CLASS fix for the
// incident that motivated #976: `scripts/test/sql-only-state.test.mjs`
// (wired, runs under `bun run test:sql-only-state`) had an unwired twin,
// `tests/sql-only-state.test.ts`, that ran under no configuration at all
// (typecheck.sh excludes `tests/**`, no vitest config includes root
// `tests/`). An engineer editing the twin (#899, an hour before this fix
// was dispatched) believed a guard was fixed; the wired file was untouched
// and the gate went red on the row the engineer thought they had cleared.
// A same-basename file in two different runners is invisible confusion by
// construction -- there is no legitimate reason for it, so this guard
// treats every collision as a hard failure rather than a triage table.
//
// Source of truth for "wired guard": every `scripts/test/*.test.mjs` file
// on disk, same directory-listing approach guard-wiring.test.mjs uses (not
// a name-pattern guess against package.json).
//
// Run with `node --test scripts/test/wired-guard-basename-collision.test.mjs`.

import assert from 'node:assert/strict'
import { readdirSync, writeFileSync, rmSync, mkdirSync } from 'node:fs'
import { dirname, join, basename } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const guardTestDir = join(repoRoot, 'scripts', 'test')
const testsDir = join(repoRoot, 'tests')

function guardStems(dir) {
  return readdirSync(dir)
    .filter((name) => name.endsWith('.test.mjs'))
    .map((name) => name.slice(0, -'.test.mjs'.length))
}

// Every `*.test.ts` basename anywhere under tests/, however deeply nested
// -- a collision is a collision regardless of subdirectory, since it is
// the FILENAME an engineer types/searches for that causes the confusion.
function testsTsStems(dir) {
  const stems = []
  for (const entry of readdirSync(dir, { recursive: true })) {
    if (typeof entry !== 'string') continue
    if (!entry.endsWith('.test.ts')) continue
    stems.push(basename(entry).slice(0, -'.test.ts'.length))
  }
  return stems
}

// ---------------------------------------------------------------------------
// The validator. Pure function of (guardStemList, testsTsStemList) ->
// string[] of violation messages -- same shape as sql-only-state's and
// guard-wiring's own validators, exercisable against real and doctored
// inputs alike.
// ---------------------------------------------------------------------------
export function validateNoBasenameCollision(guardStemList, testsTsStemList) {
  const errors = []
  const guardSet = new Set(guardStemList)
  const seen = new Set()
  for (const stem of testsTsStemList) {
    if (seen.has(stem)) continue
    if (guardSet.has(stem)) {
      errors.push(
        `tests/**/${stem}.test.ts collides with the wired guard scripts/test/${stem}.test.mjs -- ` +
          `a same-basename twin in a different test runner executes under no shared configuration ` +
          `and is the exact confusion class #976 fixed (an engineer can edit the unwired twin believing ` +
          `they fixed the guard). Delete the tests/ twin, or rename one side so the basenames never match.`
      )
    }
    seen.add(stem)
  }
  return errors
}

// ---------------------------------------------------------------------------
// Guard test against the real repo.
// ---------------------------------------------------------------------------

test('no tests/**/*.test.ts basename collides with a wired scripts/test/*.test.mjs guard (control: repo as-is passes)', () => {
  const errors = validateNoBasenameCollision(guardStems(guardTestDir), testsTsStems(testsDir))
  assert.deepEqual(errors, [], errors.join('\n'))
})

// ---------------------------------------------------------------------------
// Negative self-test (pure fixture): a fabricated collision is caught by
// name.
// ---------------------------------------------------------------------------

test('RED (fixture): a colliding stem is caught by name', () => {
  const errors = validateNoBasenameCollision(['sql-only-state', 'guard-wiring'], ['sql-only-state', 'some-other-file'])
  assert.ok(
    errors.some((m) => m.includes('sql-only-state.test.ts') && m.includes('sql-only-state.test.mjs')),
    `expected a collision violation naming sql-only-state, got: ${JSON.stringify(errors)}`
  )
  assert.ok(
    !errors.some((m) => m.includes('some-other-file')),
    'a non-colliding stem must not be reported'
  )
})

// ---------------------------------------------------------------------------
// LIVE demonstration (arm): write a real throwaway file under tests/ whose
// basename collides with a real, already-wired scripts/test/ guard, prove
// it is caught by the real directory scan, then remove it and prove the
// repo is clean again (control).
// ---------------------------------------------------------------------------

test('LIVE: a real throwaway tests/ file colliding with a real wired guard is caught, and clears once removed', () => {
  const throwawayDir = join(testsDir, '.976-collision-fixture')
  const throwawayPath = join(throwawayDir, 'guard-wiring.test.ts')
  assert.ok(guardStems(guardTestDir).includes('guard-wiring'), 'guard-wiring.test.mjs must exist for this demonstration to be meaningful')
  mkdirSync(throwawayDir, { recursive: true })
  writeFileSync(throwawayPath, "// throwaway collision fixture for #976's live demonstration\n")
  try {
    const errors = validateNoBasenameCollision(guardStems(guardTestDir), testsTsStems(testsDir))
    assert.ok(
      errors.some((m) => m.includes('guard-wiring.test.ts') && m.includes('guard-wiring.test.mjs')),
      `expected a live collision violation for the throwaway file, got: ${JSON.stringify(errors)}`
    )
  } finally {
    rmSync(throwawayPath, { force: true })
    rmSync(throwawayDir, { recursive: true, force: true })
  }
  const errorsAfterCleanup = validateNoBasenameCollision(guardStems(guardTestDir), testsTsStems(testsDir))
  assert.deepEqual(errorsAfterCleanup, [], 'the repo must be clean again once the throwaway twin is removed')
})
