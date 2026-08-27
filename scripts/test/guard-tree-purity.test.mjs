// THE GUARD SUITE MUST LEAVE THE WORKING TREE EXACTLY AS IT FOUND IT.
//
// The class this closes, from the incident that produced it:
// `scripts/test/stub-import-guard.test.mjs` wrote a live probe to
// `apps/cli/src/legacy/...` with `mkdirSync(dirname(...), { recursive: true })`
// and removed only the FILE. Every run therefore recreated three empty
// directories under a package #751/P3 deleted, in a repository where
// `scripts/test/no-ts-cli-stub.test.mjs` asserts that package does not exist.
// Each guard passed when run alone. The suite failed on its NEXT run, and the
// failure named an innocent test. That is the worst shape a flake can take:
// the guard that reports it is not the guard that caused it, so the
// investigation starts in the wrong file.
//
// WHY `git status --porcelain` IS NOT THE CHECK, and why reaching for it first
// is exactly how this stayed invisible: git tracks files, not directories. An
// empty untracked directory does not appear in `git status --porcelain`, in
// `git ls-files --others`, or in any porcelain-shaped cleanliness assertion.
// The residue that broke the suite produced a completely clean `git status`.
// The first test below proves that blindness live rather than asserting it, so
// nobody re-derives the weaker check later.
//
// WHY THE LIVE CHECK BELONGS TO THE SHARD RUNNER. The old check copied the
// tree and ran the full guard corpus a second time. CI had already run those
// guards once, so this added about 100 seconds to one shard. The real shard
// runner now snapshots its own isolated worktree once before its selected
// guards and once after them. The serial mutation path does the same around
// its one guard. This file keeps the small proofs for the snapshot instrument;
// `ci-guard-shard.test.mjs` proves the real runner rejects residue.
//
// Run with `node --test scripts/test/guard-tree-purity.test.mjs`.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync, spawnSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  describeDiff,
  diffSnapshots,
  isClean,
  snapshotTree,
} from '../guard-tree-purity.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const SELF = basename(fileURLToPath(import.meta.url))

// ---------------------------------------------------------------------------
// Fixtures: a tiny committed repository with one synthetic "guard" in it, so
// every claim below is demonstrated against a real `node --test` run rather
// than against a description of one.
// ---------------------------------------------------------------------------

/** A committed one-file repository containing `scripts/test/<name>` with the
 * given source. Committed with per-invocation identity flags so it needs no
 * host-level git config (a host that has one and a host that does not must
 * behave identically here). */
function withFixtureRepo(guardSource, body) {
  const root = mkdtempSync(join(tmpdir(), 'guard-tree-purity-fixture-'))
  try {
    mkdirSync(join(root, 'scripts', 'test'), { recursive: true })
    writeFileSync(join(root, 'scripts', 'test', 'synthetic.test.mjs'), guardSource)
    writeFileSync(join(root, '.gitignore'), 'build-output/\n*.log\n')
    writeFileSync(join(root, 'README.md'), '# fixture\n')
    const git = (...args) =>
      execFileSync('git', ['-c', 'user.email=guard@example.com', '-c', 'user.name=guard', ...args], {
        cwd: root,
        encoding: 'utf8',
      })
    git('init', '-q')
    git('add', '-A')
    git('commit', '-q', '-m', 'fixture')
    body({ root, git })
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}

/**
 * The environment a nested `node --test` needs.
 *
 * `NODE_TEST_CONTEXT` is set by the runner in every test process, and a child
 * that inherits it refuses to run any file at all -- "run() is being called
 * recursively within a test file. skipping running files" -- while still
 * EXITING ZERO. Every "nothing changed" assertion in this file would then pass
 * against a suite that never executed. Dropping the variable is what makes the
 * nested run real; the executed-test-count refusals below are what make sure
 * it stayed real.
 */
function nestedTestEnv() {
  const env = { ...process.env }
  delete env.NODE_TEST_CONTEXT
  return env
}

/**
 * #1035: pin the nested runner's reporter to TAP.
 *
 * Every executed-test-count refusal in this file reads `# tests N`, which only
 * the TAP reporter emits. The default reporter is a NODE VERSION FACT, not a
 * stable one: through Node 24 a non-TTY `node --test` defaulted to TAP, and
 * from Node 26 it defaults to `spec` unconditionally, whose tail reads
 * `ℹ tests N`. On a Node 26 host every count parsed to 0 and all five arms
 * refused with "it did not really run" -- the counts were RIGHT to refuse on
 * what they could see, and the tree was never actually dirty. Asking for the
 * format we parse makes the guard independent of the host's Node.
 */
const TAP_REPORTER = ['--test-reporter=tap']

/** Number of tests a `node --test` run reported executing, from its TAP tail. */
function executedTestCount(stdout) {
  return Number(/^# tests (\d+)$/m.exec(stdout ?? '')?.[1] ?? 0)
}

/** Run the fixture's synthetic guard exactly as the real suite runs a guard. */
function runFixtureGuard(root) {
  const result = spawnSync(process.execPath, ['--test', ...TAP_REPORTER, 'scripts/test/synthetic.test.mjs'], {
    cwd: root,
    encoding: 'utf8',
    env: nestedTestEnv(),
  })
  assert.equal(result.status, 0, `the fixture guard itself must pass:\n${result.stdout}\n${result.stderr}`)
  assert.equal(
    executedTestCount(result.stdout),
    1,
    `the fixture guard must actually have RUN -- a skipped nested run leaves no residue and would make every ` +
      `assertion here vacuously true:\n${result.stdout}\n${result.stderr}`
  )
}

/** The exact defect, as a guard file: a live probe under a deleted package
 * that removes its file and leaves the directories it created. */
const POLLUTING_GUARD = `import { test } from 'node:test'
import { mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
test('a live probe whose cleanup removes the file and not the directories', () => {
  const probe = join(repoRoot, 'apps', 'cli', 'src', 'legacy', 'Probe.ts')
  mkdirSync(dirname(probe), { recursive: true })
  writeFileSync(probe, 'export const probe = 1\\n')
  rmSync(probe, { force: true })
})
`

/** The same probe done correctly: everything under its own temp root. */
const CLEAN_GUARD = `import { test } from 'node:test'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
test('a probe that writes only under its own temp root', () => {
  const root = mkdtempSync(join(tmpdir(), 'synthetic-probe-'))
  try {
    mkdirSync(join(root, 'apps', 'cli', 'src', 'legacy'), { recursive: true })
    writeFileSync(join(root, 'apps', 'cli', 'src', 'legacy', 'Probe.ts'), 'export const probe = 1\\n')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
`

// ---------------------------------------------------------------------------
// DEMONSTRATED RED. Every assertion in the live leg is "nothing changed",
// which is indistinguishable from a broken detector until it has been watched
// to fire on the real shape.
// ---------------------------------------------------------------------------

test('DEMONSTRATED RED: empty directories left by a guard are residue, and `git status --porcelain` cannot see them', () => {
  withFixtureRepo(POLLUTING_GUARD, ({ root, git }) => {
    const before = snapshotTree(root)
    runFixtureGuard(root)
    const after = snapshotTree(root)
    const diff = diffSnapshots(before, after)

    // THE POINT OF THE WHOLE FILE. Git is blind to this, so a cleanliness
    // check built on `git status` would have passed the exact defect that
    // took a day to find.
    assert.equal(
      git('status', '--porcelain').trim(),
      '',
      'fixture precondition: git must report a CLEAN tree here -- if this ever fails, git learned to see empty ' +
        'untracked directories and the reasoning in this file needs revisiting'
    )

    assert.equal(isClean(diff), false, 'the snapshot check must NOT report clean')
    assert.deepEqual(
      diff.added,
      ['apps', 'apps/cli', 'apps/cli/src', 'apps/cli/src/legacy'],
      'every directory the probe created must be named, not just the topmost one'
    )
    assert.deepEqual(diff.removed, [])
    assert.deepEqual(diff.changed, [])
    assert.match(describeDiff(diff), /\+ apps\/cli\/src\/legacy/)
  })
})

test('DEMONSTRATED GREEN: the same probe written against its own temp root leaves nothing', () => {
  withFixtureRepo(CLEAN_GUARD, ({ root }) => {
    const before = snapshotTree(root)
    runFixtureGuard(root)
    const after = snapshotTree(root)
    assert.equal(
      isClean(diffSnapshots(before, after)),
      true,
      `a guard that writes only under a temp root must leave no trace:\n${describeDiff(diffSnapshots(before, after))}`
    )
  })
})

test('a leftover FILE and a restored-with-different-bytes file are both residue', () => {
  const guard = `import { test } from 'node:test'
import { writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
test('leaves a file behind and rewrites a tracked one', () => {
  writeFileSync(join(repoRoot, 'left-behind.txt'), 'residue\\n')
  writeFileSync(join(repoRoot, 'README.md'), '# fixture, but not the bytes it started with\\n')
})
`
  withFixtureRepo(guard, ({ root }) => {
    const before = snapshotTree(root)
    runFixtureGuard(root)
    const diff = diffSnapshots(before, snapshotTree(root))
    assert.deepEqual(diff.added, ['left-behind.txt'])
    assert.deepEqual(diff.changed, ['README.md'], 'content, not mere presence -- a probe that restores the wrong bytes is the same defect one layer in')
  })
})

test('build output the repository already ignores is not residue', () => {
  const guard = `import { test } from 'node:test'
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
test('writes only gitignored output', () => {
  mkdirSync(join(repoRoot, 'build-output', 'nested'), { recursive: true })
  writeFileSync(join(repoRoot, 'build-output', 'nested', 'artifact.js'), 'built\\n')
  writeFileSync(join(repoRoot, 'run.log'), 'noise\\n')
})
`
  withFixtureRepo(guard, ({ root }) => {
    const before = snapshotTree(root)
    runFixtureGuard(root)
    const diff = diffSnapshots(before, snapshotTree(root))
    assert.equal(
      isClean(diff),
      true,
      `.gitignore is the ignore policy, not a second skip list maintained here:\n${describeDiff(diff)}`
    )
  })
})

test('the snapshot walks a real tree rather than resolving to nothing', () => {
  // Non-vacuity for the mechanism itself: a `snapshotTree` that silently
  // returned an empty map would make every "clean" assertion above pass while
  // proving nothing at all.
  //
  // WHERE THE FLOOR COMES FROM, and why it is not a number.
  //
  // This assertion used to read `snapshot.size > 2000`, calibrated against a
  // tree that carried ~1,540 planning documents. The open-source release
  // stopped shipping those, the same healthy walk returned ~1,498, and the
  // floor failed a correct snapshot. Replacing it with a smaller literal was
  // rejected twice, and rightly: a better-documented magic number is still a
  // magic number, and the repair would have been to schedule the same
  // maintenance again for whoever next deletes a thousand files.
  //
  // So the floor is DERIVED, from the tree itself, at runtime. Every TRACKED
  // file must appear in a healthy walk; directories and non-ignored untracked
  // files only push the size higher. `snapshot.size >= trackedCount` is
  // therefore true of every correct walk at every repository size, and it
  // stays exactly as strong the day somebody deletes another thousand files.
  // There is no number to go stale, so there is no next reader to instruct.
  //
  // It is not weaker than the literal was. Every failure mode this assertion
  // exists for is catastrophic rather than marginal -- an empty map, a walk
  // that never descends, a root that resolved to one subdirectory -- and each
  // lands far below the tracked-file count. The subdirectory case is caught
  // specifically because `git ls-files` run with `cwd` at the repository root
  // returns the WHOLE list however little the walk managed to reach.
  //
  // If `git ls-files` cannot be read, this REFUSES in words rather than
  // falling back to a literal: an instrument that cannot look has not
  // reported, and silence is never the green.
  let trackedCount
  try {
    trackedCount = execFileSync('git', ['ls-files'], { cwd: repoRoot, encoding: 'utf8' })
      .split('\n')
      .filter(Boolean).length
  } catch (error) {
    assert.fail(
      `REFUSING TO REPORT SUCCESS: cannot enumerate tracked files to derive the floor (${error.message}). ` +
        'This check has not passed, it has not run -- do not substitute a literal.'
    )
  }
  assert.ok(
    trackedCount > 0,
    'REFUSING TO REPORT SUCCESS: git ls-files returned zero tracked files, so the derived floor would be ' +
      'vacuously satisfied by an empty snapshot -- the exact failure this assertion exists to catch.'
  )
  const snapshot = snapshotTree(repoRoot)
  assert.ok(
    snapshot.size >= trackedCount,
    `expected a real checkout: snapshotted ${snapshot.size} entries, but the repository tracks ` +
      `${trackedCount} files and every one of them must appear in a healthy walk (directories and ` +
      'non-ignored untracked files only push this higher). A shortfall means the walk did not reach ' +
      'the whole tree -- fix the walk, never the floor.'
  )
  assert.equal(snapshot.get('scripts/test'), 'dir', 'directories must be first-class entries, or the whole check is git status again')
  assert.ok(snapshot.get(`scripts/test/${SELF}`)?.startsWith('file:'), 'files must be content-hashed')
  assert.ok(![...snapshot.keys()].some((path) => path.startsWith('node_modules')), 'node_modules is never walked')
  assert.ok(![...snapshot.keys()].some((path) => path.endsWith('.tsbuildinfo')), 'gitignored build output is not tree content')
})
