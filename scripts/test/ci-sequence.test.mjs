// #930: locks the exact sequence properties whose absence produced three
// live defects in one hour, all pre-existing for nine landings — see
// scripts/ci-sequence.mjs's header for the incident this guards against.
// Every assertion here is DERIVED from the real `.github/workflows/ci.yml`
// via `scripts/ci-sequence.mjs`, never a remembered order.

import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'
import assert from 'node:assert/strict'

import { deriveCargoTestTargets } from '../cargo-test-derive.mjs'
import { jobBlock, jobNeeds, jobSteps, matrixIncludes } from '../ci-sequence.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const workflowPath = join(repoRoot, '.github', 'workflows', 'ci.yml')

function readWorkflow() {
  return readFileSync(workflowPath, 'utf8')
}

test('the real ci.yml is non-vacuous: cargo-test-workspace-shard resolves to a real job with real steps', () => {
  const text = readWorkflow()
  const block = jobBlock(text, 'cargo-test-workspace-shard')
  assert.ok(block, 'cargo-test-workspace-shard job not found in ci.yml -- has it been renamed?')
  const steps = jobSteps(text, 'cargo-test-workspace-shard')
  assert.ok(steps.length >= 4, `expected at least 4 steps, got ${steps.length} -- the extractor may be broken`)
})

test('cargo-test-workspace-shard starts after guard and owns compile plus execution', () => {
  const text = readWorkflow()
  const block = jobBlock(text, 'cargo-test-workspace-shard')
  assert.match(block, /needs:\s*guard/)
  assert.match(block, /bash scripts\/cargo-test-workspace\.sh/)
  assert.match(block, /if:\s*matrix\.precompile == true[\s\S]*bash scripts\/cargo-test-workspace\.sh --no-run/)
  assert.doesNotMatch(block, /build-chiefd|actions\/download-artifact|setup-bun|bun install|turbo run build|chmod \+x|touch apps\/chiefd/)
  assert.equal(jobBlock(text, 'cargo-test-workspace-compile'), undefined)
})

test('the daemon shard precompiles beacond before chiefd-daemon integration tests can spawn it', () => {
  const text = readWorkflow()
  const entries = matrixIncludes(text, 'cargo-test-workspace-shard')
  const precompiled = entries.filter((entry) => entry.precompile === 'true')

  assert.deepEqual(
    precompiled.map((entry) => ({ shard: entry.shard, packages: entry.packages })),
    [{ shard: 'daemon', packages: 'chiefd-daemon beacond' }],
  )

  const block = jobBlock(text, 'cargo-test-workspace-shard')
  const compile = block.indexOf('bash scripts/cargo-test-workspace.sh --no-run')
  const execute = block.lastIndexOf('bash scripts/cargo-test-workspace.sh')
  assert.ok(compile >= 0, 'the daemon precompile command must exist')
  assert.ok(execute > compile, 'the package group must execute only after all cross-package binaries compile')
})

test('cargo-test-workspace-shard uses an isolated cache for its level-0 test profile', () => {
  const text = readWorkflow()
  const buildBlock = jobBlock(text, 'build-chiefd')
  const testBlock = jobBlock(text, 'cargo-test-workspace-shard')
  const buildKey = /shared-key:\s*(\S+)/.exec(buildBlock ?? '')
  const testKey = /shared-key:\s*(\S+)/.exec(testBlock ?? '')
  assert.ok(buildKey, 'build-chiefd has no shared cargo cache key')
  assert.ok(testKey, 'cargo-test-workspace-shard has no shared cargo cache key')
  assert.notEqual(
    testKey[1],
    buildKey[1],
    'test and binary profiles must not share a Cargo cache with different compiler flags',
  )
  assert.match(testKey[1], /^chiefd-test-opt0-v1-/)
})

test('build-chiefd reuses only an exact Rust-input binary cache', () => {
  const block = jobBlock(readWorkflow(), 'build-chiefd')
  assert.ok(block, 'build-chiefd job not found')
  assert.match(block, /id: chiefd-binary-cache/)
  // PINNED TO THE MAJOR ON PURPOSE, and moved deliberately when it is bumped
  // (v4 -> v6, 2026-08-24). The step this guard describes is a hand-keyed
  // binary cache whose correctness depends on `cache-hit` behaving exactly as
  // the pinned major does; a silent major bump is the one change that could
  // alter that without touching a line this file reads. Do not relax it to
  // `@v\d+` — the version being visible here is what forces the bump to be
  // looked at rather than absorbed.
  assert.match(block, /uses: actions\/cache@v6/)
  assert.match(block, /path:[\s\S]*apps\/chiefd\/target\/debug\/chiefd\n[\s\S]*chiefd\n[\s\S]*beacond/)
  assert.match(block, /key: chiefd-ci-binaries-v1-\$\{\{ runner\.os \}\}-rust-1\.97\.1-\$\{\{ hashFiles\(/)
  assert.match(block, /'apps\/chiefd\/\*\*'/)
  assert.match(block, /'!apps\/chiefd\/target\/\*\*'/)
  assert.match(block, /'\.cargo\/\*\*'/)
  assert.match(block, /'rust-toolchain\.toml'/)
  assert.match(block, /'rust-toolchain'/)
  assert.match(block, /Report exact chiefd CI binary cache status/)
  assert.match(block, /if: steps\.chiefd-binary-cache\.outputs\.cache-hit != 'true'/)
  assert.match(block, /Assert cached or built binaries are present/)
})

test('CI dependency installs skip lifecycle builds because each gate owns its explicit build', () => {
  const workflow = readWorkflow()
  const installs = workflow
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.startsWith('run: bun install --frozen-lockfile'))
  assert.ok(installs.length > 0, 'ci.yml has no locked Bun install step')
  assert.ok(
    installs.every((line) => line.endsWith('--ignore-scripts')),
    'every CI Bun install must skip the root postinstall build; the workflow runs its required build explicitly'
  )
})

test('repo guard shards let the shard runner own the workspace build once', () => {
  const block = jobBlock(readWorkflow(), 'repo-guards-shard')
  assert.ok(block, 'repo-guards-shard job not found')
  assert.doesNotMatch(
    block,
    /Run the workspace build required by package guards/,
    'the workflow must not run a build before ci-guard-shards.mjs runs its own build'
  )
  assert.match(block, /node scripts\/ci-guard-shards\.mjs --shards 4 --shard/)
})

test('the docs-only guard sends the complete pull-request endpoints to the fail-closed scope checker', () => {
  const block = jobBlock(readWorkflow(), 'guard')
  assert.ok(block, 'guard job not found')
  assert.match(block, /fetch-depth:\s*2/)
  assert.match(block, /CI_SCOPE_BASE_SHA:\s*\$\{\{ github\.event\.pull_request\.base\.sha \}\}/)
  assert.match(block, /CI_SCOPE_HEAD_SHA:\s*\$\{\{ github\.event\.pull_request\.head\.sha \}\}/)
  assert.match(block, /run:\s*node scripts\/ci-pr-scope\.mjs/)
  assert.doesNotMatch(block, /github\.event\.before/)
})

test('jobBlock returns undefined for a job that does not exist, rather than an empty string that silently passes downstream checks', () => {
  const text = readWorkflow()
  assert.equal(jobBlock(text, 'this-job-does-not-exist-9f3a'), undefined)
  assert.deepEqual(jobNeeds(text, 'this-job-does-not-exist-9f3a'), [])
  assert.deepEqual(jobSteps(text, 'this-job-does-not-exist-9f3a'), [])
})

test('DEFECT 2 (CI env): the test-unit job body does not unset or override CI, and GitHub Actions sets it unconditionally for every job', () => {
  // #930's second defect was specific to a driver running OUTSIDE GitHub
  // Actions (the merger's own reproduction), where CI is not automatically
  // set the way it is on a real runner. There is nothing to derive from
  // ci.yml for this half of the defect -- it is a property of the reproducing
  // driver, not of the workflow file, which is exactly why it went
  // unnoticed: nothing in this repo checks a driver AGAINST ci.yml for this.
  // Documented here as the one leg of DEFECT 2/3 this guard cannot check
  // structurally, so the gap is visible rather than silently assumed covered.
  const text = readWorkflow()
  const testUnitBlock = jobBlock(text, 'test-unit')
  assert.ok(testUnitBlock, 'test-unit job not found')
  assert.ok(
    !/\bCI:\s*(""|false|0)\s*$/m.test(testUnitBlock),
    'test-unit explicitly unsets/falsifies CI -- that would need its own guard line here'
  )
})

test('vitest lanes start at their true binary dependency and the stable collector covers all packages', () => {
  const text = readWorkflow()
  const base = jobBlock(text, 'test-unit-base')
  const chiefd = jobBlock(text, 'test-unit-chiefd')
  const piing = jobBlock(text, 'test-unit-piing')
  const contract = jobBlock(text, 'test-unit-piing-contract')
  const collector = jobBlock(text, 'test-unit')
  assert.ok(base, 'test-unit-base job not found')
  assert.ok(chiefd, 'test-unit-chiefd job not found')
  assert.ok(piing, 'test-unit-piing job not found')
  assert.ok(contract, 'test-unit-piing-contract job not found')
  assert.ok(collector, 'test-unit collector job not found')
  assert.match(base, /needs:\s*guard/)
  assert.match(base, /--filter=@chief\/web/)
  assert.match(base, /--filter=@chief\/eslinter/)
  assert.doesNotMatch(base, /build-chiefd|chiefd-ci-binary|download-artifact/)
  assert.deepEqual(jobNeeds(text, 'test-unit-chiefd'), ['guard', 'build-chiefd'])
  assert.match(chiefd, /--filter=@chief\/chiefing/)
  assert.match(chiefd, /--filter=@chief\/testing/)
  assert.match(chiefd, /chiefd-ci-binary/)
  assert.deepEqual(jobNeeds(text, 'test-unit-piing'), ['guard'])
  assert.match(piing, /filter=@chief\/piing/)
  assert.match(piing, /--shard=\$\{\{ matrix\.shard \}\}/)
  assert.doesNotMatch(piing, /build-chiefd|chiefd-ci-binary|download-artifact/)
  assert.match(contract, /--testNamePattern=/)
  assert.match(contract, /CI_TOOL_CONTRACT_LANE/)
  assert.deepEqual(jobNeeds(text, 'test-unit'), [
    'guard',
    'test-unit-base',
    'test-unit-chiefd',
    'test-unit-piing',
    'test-unit-piing-contract'
  ])
  assert.match(collector, /needs\.test-unit-base\.result/)
  assert.match(collector, /needs\.test-unit-chiefd\.result/)
  assert.match(collector, /needs\.test-unit-piing\.result/)
  assert.match(collector, /needs\.test-unit-piing-contract\.result/)
})

test('lint and knip run independently and keep one stable collector', () => {
  const text = readWorkflow()
  const eslint = jobBlock(text, 'lint-eslint')
  const knip = jobBlock(text, 'lint-knip')
  const collector = jobBlock(text, 'lint')
  assert.ok(eslint)
  assert.ok(knip)
  assert.ok(collector)
  assert.match(eslint, /needs:\s*guard/)
  assert.match(knip, /needs:\s*guard/)
  assert.match(eslint, /run: bun run lint/)
  assert.doesNotMatch(eslint, /run: bun run knip/)
  assert.match(knip, /run: bun run knip/)
  assert.doesNotMatch(knip, /run: bun run lint/)
  assert.deepEqual(jobNeeds(text, 'lint'), ['guard', 'lint-eslint', 'lint-knip'])
  assert.match(collector, /name: Lint and knip/)
  assert.match(collector, /needs\.lint-eslint\.result/)
  assert.match(collector, /needs\.lint-knip\.result/)
})

// ---------------------------------------------------------------------------
// The CLI shard's parallel Cargo targets.
//
// `scripts/cargo-test-workspace-shard.sh` runs ONLY the targets named in
// `CI_CARGO_PARALLEL_TARGETS` when its package is `chief-cli`, so that list
// IS the run set of the shard: a `tests/*.rs` file absent from it does not
// run in CI at all.
//
// This assertion used to read `/parallel_targets: lib bin:chiefd doc
// interpret_crash/` — an ADJACENCY of two target names, which is not a
// property of the workflow. #1049 added the `daemon_level_log` target
// between `doc` and `interpret_crash`, a correct change, and main went red;
// the pressure that produces is on the AUTHOR to weaken the guard. It was
// also blind to the defect that actually costs something: a target file on
// disk that the list does not name, which CI then silently never runs.
//
// Both are fixed by deriving the expected set from the crate and comparing
// SETS, so order carries no meaning and a legitimately added target needs no
// edit here — only the workflow entry it must have anyway.
// ---------------------------------------------------------------------------

const CHIEF_CLI_DIR = join(repoRoot, 'apps', 'chiefd', 'crates', 'chief-cli')

/**
 * Compare one matrix entry's `parallel_targets:` text against a crate's
 * derived target set. `declaredText` is `undefined` when the key is absent —
 * the "someone deleted it" case, which must fail rather than compare nothing
 * against nothing.
 *
 * `derivationOk` is the anti-vacuity floor: the derivation must have found a
 * lib, a bin, doctests and at least one integration target, which is the
 * shape `chief-cli` has. If the crate ever genuinely loses one of the four,
 * this fails loudly and gets revisited — it does not quietly become an
 * assertion about nothing.
 */
function checkParallelTargets(declaredText, expected) {
  const declared = (declaredText ?? '').trim().split(/\s+/).filter(Boolean)
  const missing = expected.filter((target) => !declared.includes(target))
  const extra = declared.filter((target) => !expected.includes(target))
  const isIntegration = (target) => target !== 'lib' && target !== 'doc' && !target.startsWith('bin:')
  const derivationOk =
    expected.includes('lib') &&
    expected.includes('doc') &&
    expected.some((target) => target.startsWith('bin:')) &&
    expected.some(isIntegration)
  return {
    declared,
    expected,
    missing,
    extra,
    derivationOk,
    ok: derivationOk && declared.length > 0 && missing.length === 0 && extra.length === 0,
  }
}

test("the Rust CLI shard runs every one of chief-cli's Cargo test targets in parallel", () => {
  const text = readWorkflow()
  const block = jobBlock(text, 'cargo-test-workspace-shard')
  assert.ok(block, 'cargo-test-workspace-shard job not found')

  const entries = matrixIncludes(text, 'cargo-test-workspace-shard')
  assert.ok(entries.length > 0, 'cargo-test-workspace-shard resolves ZERO matrix entries -- the extractor is broken')
  const cli = entries.find((entry) => entry.packages === 'chief-cli')
  assert.ok(cli, `no matrix entry runs chief-cli: ${JSON.stringify(entries.map((e) => e.packages))}`)

  const expected = deriveCargoTestTargets(CHIEF_CLI_DIR)
  const result = checkParallelTargets(cli.parallel_targets, expected)
  assert.ok(
    result.derivationOk,
    `the target derivation over ${CHIEF_CLI_DIR} produced ${JSON.stringify(expected)}, which is not the shape ` +
      'chief-cli has (a lib, a bin, doctests and integration tests). Fix the derivation or record the real change ' +
      'to the crate -- do not compare against it while it says this.',
  )
  assert.deepEqual(
    { missing: result.missing, extra: result.extra },
    { missing: [], extra: [] },
    `the cli shard's parallel_targets and chief-cli's real Cargo targets disagree.\n` +
      `  workflow: ${result.declared.join(' ') || '(the key is absent)'}\n` +
      `  crate:    ${expected.join(' ')}\n` +
      'A MISSING target does not run in CI at all -- the shard script runs only what this list names. An EXTRA ' +
      'target names a Cargo target that does not exist. Edit the ci.yml entry, never this assertion.',
  )
  assert.match(block, /CI_CARGO_PARALLEL_TARGETS:\s*\$\{\{ matrix\.parallel_targets \}\}/)

  const ignored = entries.filter((entry) => entry.packages !== 'chief-cli' && entry.parallel_targets !== undefined)
  assert.deepEqual(
    ignored.map((entry) => entry.shard),
    [],
    'the shard script honours CI_CARGO_PARALLEL_TARGETS only for chief-cli, so this list is a silent no-op here',
  )
})

// The three subtests below drive the same check with synthetic input, so
// "it still catches a broken workflow" is a checked fact rather than a
// belief about the assertion above.

function syntheticCrate(testFileNames) {
  const dir = mkdtempSync(join(tmpdir(), 'ci-parallel-targets-'))
  mkdirSync(join(dir, 'src'), { recursive: true })
  mkdirSync(join(dir, 'tests'), { recursive: true })
  writeFileSync(
    join(dir, 'Cargo.toml'),
    '[package]\nname = "widget-cli"\n\n[[bin]]\nname = "widget"\npath = "src/main.rs"\n',
  )
  writeFileSync(join(dir, 'src', 'lib.rs'), '')
  writeFileSync(join(dir, 'src', 'main.rs'), 'fn main() {}\n')
  for (const name of testFileNames) writeFileSync(join(dir, 'tests', `${name}.rs`), '')
  return dir
}

test('the parallel-target check fires when the workflow drops parallel_targets, and names every lost target', () => {
  const crate = syntheticCrate(['alpha', 'beta'])
  try {
    const workflow = [
      'jobs:',
      '  cargo-test-workspace-shard:',
      '    strategy:',
      '      matrix:',
      '        include:',
      '          - shard: cli',
      '            packages: chief-cli',
      '          - shard: daemon',
      '            packages: chiefd-daemon beacond',
      '',
    ].join('\n')
    const entries = matrixIncludes(workflow, 'cargo-test-workspace-shard')
    assert.deepEqual(
      entries.map((entry) => entry.shard),
      ['cli', 'daemon'],
      'the matrix extractor must still see both entries -- a check that reads nothing cannot fire on anything',
    )
    const cli = entries.find((entry) => entry.packages === 'chief-cli')
    const result = checkParallelTargets(cli.parallel_targets, deriveCargoTestTargets(crate))
    assert.equal(result.ok, false, 'a deleted parallel_targets must fail the check')
    assert.deepEqual(result.missing, ['lib', 'bin:widget', 'doc', 'alpha', 'beta'])
  } finally {
    rmSync(crate, { recursive: true, force: true })
  }
})

test('the parallel-target check fires on a single dropped target, in either direction', () => {
  const crate = syntheticCrate(['alpha', 'beta'])
  try {
    const expected = deriveCargoTestTargets(crate)
    assert.deepEqual(expected, ['lib', 'bin:widget', 'doc', 'alpha', 'beta'])

    const dropped = checkParallelTargets('lib bin:widget doc alpha', expected)
    assert.equal(dropped.ok, false)
    assert.deepEqual(dropped.missing, ['beta'], 'an integration test CI would never run must be named')

    const droppedLib = checkParallelTargets('bin:widget doc alpha beta', expected)
    assert.equal(droppedLib.ok, false)
    assert.deepEqual(droppedLib.missing, ['lib'])

    const invented = checkParallelTargets('lib bin:widget doc alpha beta gamma', expected)
    assert.equal(invented.ok, false)
    assert.deepEqual(invented.extra, ['gamma'], 'a target with no Cargo target behind it must be named')
  } finally {
    rmSync(crate, { recursive: true, force: true })
  }
})

test('the parallel-target check accepts a legitimately added target with no edit to this guard', () => {
  const crate = syntheticCrate(['alpha', 'beta'])
  try {
    assert.equal(checkParallelTargets('lib bin:widget doc alpha beta', deriveCargoTestTargets(crate)).ok, true)

    // The exact move #1049 made: a new integration test file, wired into the
    // workflow in the same commit. Written between the existing names, which
    // is what broke the adjacency this replaced.
    writeFileSync(join(crate, 'tests', 'daemon_level_log.rs'), '')
    const added = checkParallelTargets(
      'lib bin:widget doc daemon_level_log alpha beta',
      deriveCargoTestTargets(crate),
    )
    assert.equal(added.ok, true, `a legitimately added target must pass: ${JSON.stringify(added)}`)

    // The same file, NOT wired: the silent-skip defect the old literal could
    // not see at all.
    const unwired = checkParallelTargets('lib bin:widget doc alpha beta', deriveCargoTestTargets(crate))
    assert.equal(unwired.ok, false)
    assert.deepEqual(unwired.missing, ['daemon_level_log'])
  } finally {
    rmSync(crate, { recursive: true, force: true })
  }
})
