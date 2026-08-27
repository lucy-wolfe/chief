// Guard + regression test for scripts/assert-typecheck-nonvacuous.mjs (#886).
//
// #886: apps/cli's non-legacy TypeScript was checked by NEITHER typecheck leg.
// Proven by injecting a real type error and watching `bash scripts/typecheck.sh`
// — the exact command CI's typecheck job runs — exit 0. Fixed by adding the
// package to the root tsconfig.json's `references`.
//
// RE-ANCHORED (P3) when apps/cli was deleted. The old assertion named
// `apps/cli` as a literal, so deleting the package would have left an
// instrument that could no longer see its subject — the single most expensive
// mistake this repo has made. It is replaced by
// the CLASS it was always standing in for: EVERY bun workspace member that has
// its own `tsconfig.json` is a reference in the root `tsconfig.json`, derived
// from `package.json`'s `workspaces` globs and checked in both directions. A
// new package cannot arrive unchecked, and no future deletion can quietly
// empty this test — a member that leaves takes its own row with it, and one
// that arrives fails here by name.
//
// Per #848, a fix that adds coverage without a non-vacuity assertion on that
// coverage can silently regress back to checking nothing. The root
// tsconfig.json is SOLUTION-STYLE (empty `include`/`files`, only
// `references`) — `assertMinimumRealFiles` (the #848 mechanism already used
// for tsconfig.extensions.json) walks a config's OWN `include` patterns on disk,
// which for a solution-style config is always empty, so it would refuse
// UNCONDITIONALLY if pointed at tsconfig.json. This file's first job is
// proving that distinction is handled: a floor on tsconfig.json's project
// graph must go through the aggregate `assertNonVacuous` path instead.
//
// Run with `node --test scripts/test/assert-typecheck-nonvacuous.test.mjs`.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { existsSync, mkdtempSync, mkdirSync, readdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { tmpdir } from 'node:os'

import {
  isSolutionStyle,
  readTsconfig,
  assertNonVacuous,
  assertMinimumRealFiles
} from '../assert-typecheck-nonvacuous.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')

/** Every bun workspace member that carries its own `tsconfig.json`. */
function typecheckedWorkspaceMembers() {
  const manifest = JSON.parse(readFileSync(join(repoRoot, 'package.json'), 'utf8'))
  const members = []
  for (const pattern of manifest.workspaces ?? []) {
    const [group, star] = pattern.split('/')
    assert.equal(star, '*', `only <group>/* workspace globs are modelled here, got '${pattern}'`)
    for (const entry of readdirSync(join(repoRoot, group), { withFileTypes: true })) {
      if (!entry.isDirectory()) continue
      const member = `${group}/${entry.name}`
      if (existsSync(join(repoRoot, member, 'tsconfig.json'))) members.push(member)
    }
  }
  return members.sort()
}

test('#886 regression: every workspace member with a tsconfig.json is a reference in the root tsconfig.json', () => {
  const config = readTsconfig(join(repoRoot, 'tsconfig.json'))
  const referenced = (config.references ?? []).map((r) => r.path).sort()
  const members = typecheckedWorkspaceMembers()
  // NON-VACUITY. An empty derivation would make the equality below pass while
  // proving nothing, which is the exact failure this whole file exists to
  // prevent one level down.
  assert.ok(
    members.length >= 4,
    `the workspace-member derivation went blind: found ${members.length} members with a tsconfig.json`
  )
  assert.deepEqual(
    referenced,
    members,
    `the root tsconfig.json's references must equal the workspace members that have a tsconfig.json — ` +
      `a member missing here is the whole class #886 found (a package with its own tsconfig.json/` +
      `vitest.config.ts/build script, checked by neither typecheck leg); an extra one is a reference ` +
      `to a package that no longer exists`
  )
})

test('the root tsconfig.json is solution-style (empty include/files, only references)', () => {
  const config = readTsconfig(join(repoRoot, 'tsconfig.json'))
  assert.equal(isSolutionStyle(config), true)
})

test('tsconfig.extensions.json is a PLAIN config (real include patterns, no references)', () => {
  const config = readTsconfig(join(repoRoot, 'tsconfig.extensions.json'))
  assert.equal(isSolutionStyle(config), false)
})

test('assertNonVacuous applies a floor to the aggregate project-graph count, not a per-file disk walk', () => {
  const configPath = join(repoRoot, 'tsconfig.json')
  // 15, matching the floor scripts/typecheck.sh wires. It was 22 until
  // #751/P0 removed apps/api from tsconfig.json's project graph, and the real
  // aggregate fell again (20 -> 17) when P3 removed apps/cli; the floor is
  // unchanged because it is a floor, not a census. A test that kept an old
  // exact number would fail on a correct tree, which is the same re-anchor the
  // script's own comment sanctions when a graph legitimately shrinks.
  const { inputs, problems } = assertNonVacuous(configPath, 15)
  assert.ok(inputs >= 15, `expected at least 15 aggregate input patterns, got ${inputs}`)
  assert.deepEqual(problems, [])
})

test('assertNonVacuous with a floor ABOVE the real count refuses and names the shortfall', () => {
  const configPath = join(repoRoot, 'tsconfig.json')
  const { problems } = assertNonVacuous(configPath, 1_000_000)
  assert.equal(problems.length, 1)
  assert.match(problems[0], /below the expected floor of 1000000/)
  assert.match(problems[0], /#886/)
})

test('assertNonVacuous with no floor keeps the original #3081 nonzero-only behavior', () => {
  const configPath = join(repoRoot, 'tsconfig.json')
  const { problems } = assertNonVacuous(configPath)
  assert.deepEqual(problems, [])
})

// ---------------------------------------------------------------------------
// TAMPER PROOF: a real isolated fixture solution-config, one reference
// dropped, proves the floor catches exactly #886's own regression shape.
// ---------------------------------------------------------------------------

function writeFixtureSolution(root, referencedPatternCounts) {
  const refs = []
  for (const [name, patternCount] of Object.entries(referencedPatternCounts)) {
    const dir = join(root, name)
    mkdirSync(dir, { recursive: true })
    const include = Array.from({ length: patternCount }, (_, i) => `src${i}/**/*.ts`)
    writeFileSync(join(dir, 'tsconfig.json'), JSON.stringify({ include }))
    refs.push({ path: name })
  }
  const rootConfigPath = join(root, 'tsconfig.json')
  writeFileSync(rootConfigPath, JSON.stringify({ files: [], references: refs }))
  return rootConfigPath
}

test('tamper proof: dropping one referenced project drops the aggregate below a floor that included it', () => {
  const root = mkdtempSync(join(tmpdir(), 'typecheck-nonvacuous-tamper-'))
  try {
    const withBoth = writeFixtureSolution(root, { pkgA: 2, pkgB: 3 })
    const before = assertNonVacuous(withBoth, 4)
    assert.deepEqual(before.problems, [])
    assert.equal(before.inputs, 5)

    // Simulate #886's exact regression: a reference silently dropped.
    const droppedRoot = mkdtempSync(join(tmpdir(), 'typecheck-nonvacuous-tamper-dropped-'))
    try {
      const onlyOne = writeFixtureSolution(droppedRoot, { pkgA: 2 })
      const after = assertNonVacuous(onlyOne, 4)
      assert.equal(after.inputs, 2)
      assert.equal(after.problems.length, 1)
      assert.match(after.problems[0], /below the expected floor of 4/)
    } finally {
      rmSync(droppedRoot, { recursive: true, force: true })
    }
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('assertMinimumRealFiles is unchanged: a solution-style config always resolves 0 real files of its own', () => {
  // This is exactly why #886 could not reuse assertMinimumRealFiles for
  // tsconfig.json directly — proving the boundary, not just asserting it.
  assert.throws(
    () => assertMinimumRealFiles(join(repoRoot, 'tsconfig.json'), 1),
    /resolve to only 0 real file\(s\)/
  )
})

// #751/E4: this used to hardcode `100` -- a second, independently-typed
// copy of a number scripts/typecheck.sh already states. The legacy tree was
// dissolved into Rust (apps/cli/src/legacy/** went ~108 files -> 10), so
// typecheck.sh re-anchored its own floor and this copy went stale on its
// own, failing while the leg it claims to describe was passing. Read the
// live wiring instead, exactly as scripts/test/typecheck-nonvacuous.test.mjs
// (the sibling guard that owns the ratchet on this same number) already
// does -- a floor cited in two places drifts, a floor derived from the one
// place it is configured cannot.
function wiredLegacyFloor() {
  const script = readFileSync(join(repoRoot, 'scripts', 'typecheck.sh'), 'utf8')
  const match = script.match(/assert-typecheck-nonvacuous\.mjs\s+tsconfig\.extensions\.json\s+(\d+)/)
  assert.ok(
    match,
    'scripts/typecheck.sh must wire assert-typecheck-nonvacuous.mjs to tsconfig.extensions.json with a floor ' +
      'argument (#848) -- not finding the invocation at all means the guard has been un-wired, not retuned'
  )
  return Number(match[1])
}

test('assertMinimumRealFiles still enforces tsconfig.extensions.json\'s existing #848 floor, as scripts/typecheck.sh actually wires it', () => {
  const floor = wiredLegacyFloor()
  assert.ok(floor > 0, 'the wired floor must be positive -- a floor of 0 would guard nothing')
  const total = assertMinimumRealFiles(join(repoRoot, 'tsconfig.extensions.json'), floor)
  assert.ok(total >= floor, `expected at least ${floor} real .ts files, got ${total}`)
})

// ---------------------------------------------------------------------------
// LIVE: end-to-end proof that scripts/typecheck.sh itself — not just the
// library functions above — fails on the exact #886 regression shape and
// passes once restored. Slower (spawns a real subprocess), kept as one test
// rather than duplicated per-scenario.
// ---------------------------------------------------------------------------

// The directory the live leg injects a throwaway file INTO, and the package
// whose membership of the checked graph is what makes the injection mean
// anything. Named rather than derived, and both asserted before anything is
// written: a live proof that silently stops injecting into the graph is a
// green test that proves nothing.
const LIVE_PROOF_PACKAGE = 'packages/testing'
const LIVE_PROOF_DIR = `${LIVE_PROOF_PACKAGE}/src`
// A file this test CREATES and DELETES. The name is deliberately unmistakable
// so a copy left behind by a killed run is obviously residue and not source.
const LIVE_PROOF_BASENAME = '__typecheck_liveness_probe.ts'

// TOMBSTONE: this used to APPEND a type error to `packages/testing/src/index.ts`
// and restore the bytes it had read in a `finally`.
//
// That was already the second iteration. The first restored `git show HEAD:…`,
// which is not the working tree's version, and so silently destroyed an
// uncommitted edit — observed live, with the next suite then failing against
// code its author had already fixed. Reading the disk bytes instead fixed the
// git-state half and left the real hazard untouched: the window between the
// read and the `finally` is a whole `scripts/typecheck.sh` run, minutes long,
// and anything that edits that file inside it is overwritten with no error and
// no diff to notice. In a repo where several agents work one tree at once,
// that is a class of silent data loss, not a corner case.
//
// Creating and deleting a file of this test's own removes the class rather
// than narrowing it: no tracked file is ever read or written, so there is
// nothing to lose the race over. The proof is unchanged in strength — both
// `tsconfig.build.json` and `tsconfig.vitest.json` include `src/**/*.ts`, so a
// new file there is in exactly the graph the old injection relied on.
test('live: scripts/typecheck.sh fails a real type error injected into the project graph, passes once removed', () => {
  const probePath = join(repoRoot, LIVE_PROOF_DIR, LIVE_PROOF_BASENAME)
  assert.ok(
    existsSync(join(repoRoot, LIVE_PROOF_DIR)),
    `${LIVE_PROOF_DIR} must exist for the live proof to inject anything`
  )
  assert.ok(
    typecheckedWorkspaceMembers().includes(LIVE_PROOF_PACKAGE),
    `${LIVE_PROOF_PACKAGE} must be in the checked project graph, or the live proof proves nothing`
  )
  // REFUSE rather than clobber. A probe already on disk is residue from a
  // killed run — or, worse, a real file somebody named this — and quietly
  // overwriting it would reintroduce the exact hazard this shape removes.
  assert.ok(
    !existsSync(probePath),
    `${LIVE_PROOF_DIR}/${LIVE_PROOF_BASENAME} already exists; delete it (it is residue from an interrupted run)`
  )

  writeFileSync(probePath, 'export const __e886LiveProof: number = "not a number"\n')
  try {
    let redFailed = false
    try {
      execFileSync('bash', ['scripts/typecheck.sh'], { cwd: repoRoot, stdio: 'pipe' })
    } catch {
      redFailed = true
    }
    assert.equal(
      redFailed,
      true,
      `a real type error in ${LIVE_PROOF_DIR}/${LIVE_PROOF_BASENAME} must fail scripts/typecheck.sh`
    )
  } finally {
    rmSync(probePath, { force: true })
    // The build leg runs before `tsc -b`, so a probe that reached it may have
    // left emitted artifacts behind. They would fail the repo-purity check as
    // an unexplained untracked file, which is a confusing way to learn this
    // test ran.
    for (const emitted of ['__typecheck_liveness_probe.js', '__typecheck_liveness_probe.d.ts']) {
      rmSync(join(repoRoot, LIVE_PROOF_PACKAGE, 'dist', emitted), { force: true })
    }
  }
  // Green: with the probe gone the tree passes. Allowed to be slow; this is
  // the one true end-to-end proof and is worth the real subprocess cost.
  execFileSync('bash', ['scripts/typecheck.sh'], { cwd: repoRoot, stdio: 'pipe' })
})
