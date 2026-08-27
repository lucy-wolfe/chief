// #923: unit tests for scripts/vitest-exclude-ci-wiring.mjs -- RED/GREEN fixtures for the
// pure `deriveVitestExcludeWiring` validator, fail-closed vacuity/parse-error assertions, and
// the real-repo assertion. Run with `node --test
// scripts/test/vitest-exclude-ci-wiring.test.mjs`.

import assert from 'node:assert/strict'
import { existsSync, mkdtempSync, mkdirSync, rmSync, writeFileSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import {
  EXEMPT_SCRIPTS,
  deriveVitestExcludeWiring,
  parseViteExcludeList,
  readWorkflowFiles,
  resolveWorkspaceMemberDirs,
} from '../vitest-exclude-ci-wiring.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')

function readPackageJson() {
  return JSON.parse(readFileSync(join(repoRoot, 'package.json'), 'utf8'))
}

// ---------------------------------------------------------------------------
// Pure-function fixtures -- fabricated members/scripts/workflow text, so these
// stay true regardless of what canonical's real vitest configs currently
// contain (same discipline guard-wiring.test.mjs's own RED/GREEN fixtures
// use a synthetic 'brand-new-guard' name for).
// ---------------------------------------------------------------------------

test('parseViteExcludeList: returns null when no exclude key is present', () => {
  assert.equal(parseViteExcludeList('export default { test: { include: ["test/**"] } }'), null)
})

test('parseViteExcludeList: extracts every quoted entry, single or double quoted', () => {
  const text = `exclude: [\n  'test/A.test.ts',\n  "test/B.test.ts"\n]`
  assert.deepEqual(parseViteExcludeList(text), ['test/A.test.ts', 'test/B.test.ts'])
})

test('parseViteExcludeList: throws (fails closed) on an unclosed exclude array', () => {
  assert.throws(() => parseViteExcludeList("exclude: [\n  'test/A.test.ts',\n"), /malformed vitest config/)
})

// #950/#954: a real apostrophe in a comment line ("this file's `mutate()`") inside an
// EXISTING exclude array made this exact regex start a fake string literal at that
// apostrophe and silently swallow every real entry after it -- caught only because the
// poisoned parse happened to still report a nonzero violation count, not because anything
// here would have caught a poisoned parse that happened to match cleanly. Comment lines
// (this file's own convention: always their own line, prefixed `//`) must never affect what
// gets extracted, no matter what punctuation they contain.
test('parseViteExcludeList: an apostrophe in a full-line comment does not poison the real entries', () => {
  const text = [
    'exclude: [',
    "  'test/A.test.ts',",
    "  // this file's own `mutate()` doesn't get confused by an apostrophe",
    "  'test/B.test.ts'",
    ']'
  ].join('\n')
  assert.deepEqual(parseViteExcludeList(text), ['test/A.test.ts', 'test/B.test.ts'])
})

// Nothing in this repo enforces "every comment is on its own line" -- a fix that only
// stripped full-line comments would still be poisoned by a trailing one.
test('parseViteExcludeList: an apostrophe in a TRAILING comment does not poison the real entries', () => {
  const text = [
    'exclude: [',
    "  'test/A.test.ts', // it's the ported one",
    "  'test/B.test.ts'",
    ']'
  ].join('\n')
  assert.deepEqual(parseViteExcludeList(text), ['test/A.test.ts', 'test/B.test.ts'])
})

test('resolveWorkspaceMemberDirs: throws when package.json has no workspaces array', () => {
  assert.throws(() => resolveWorkspaceMemberDirs('/repo', {}), /no "workspaces" array/)
})

test('resolveWorkspaceMemberDirs: throws on an unrecognized workspaces pattern', () => {
  assert.throws(
    () => resolveWorkspaceMemberDirs('/repo', { workspaces: ['apps/*/nested'] }),
    /unrecognized workspaces pattern/
  )
})

// ---------------------------------------------------------------------------
// LIVE fixtures: real throwaway directories on disk, exercised through the
// actual `deriveVitestExcludeWiring` entry point -- same discipline
// guard-wiring.test.mjs's own LIVE test uses for realGuardFiles().
// ---------------------------------------------------------------------------

function makeFixtureRepo() {
  const root = mkdtempSync(join(tmpdir(), '923-vitest-exclude-'))
  mkdirSync(join(root, 'apps', 'demo', 'test'), { recursive: true })
  writeFileSync(
    join(root, 'apps', 'demo', 'vitest.config.ts'),
    "import { defineConfig } from 'vitest/config'\nexport default defineConfig({ test: { exclude: ['test/Excluded.test.ts'] } })\n"
  )
  return root
}

test('LIVE RED: an excluded test whose script is not invoked anywhere in workflow text fails, naming the test', () => {
  const root = makeFixtureRepo()
  try {
    const memberDirs = [join(root, 'apps', 'demo')]
    const scripts = { 'test:excluded': 'bun test apps/demo/test/Excluded.test.ts' }
    const workflowText = 'run: bun run test:unit'
    const result = deriveVitestExcludeWiring(memberDirs, root, scripts, workflowText, {}, 1)
    assert.equal(result.violations.length, 1)
    assert.ok(result.violations[0].includes('apps/demo/test/Excluded.test.ts'))
    assert.ok(result.violations[0].includes('test:excluded'))
    assert.ok(result.violations[0].includes('runs nowhere in CI'))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('LIVE RED: an excluded test with a vitest exclude entry but no resolving script fails, naming the missing script', () => {
  const root = makeFixtureRepo()
  try {
    const memberDirs = [join(root, 'apps', 'demo')]
    const result = deriveVitestExcludeWiring(memberDirs, root, {}, 'run: bun run test:unit', {}, 1)
    assert.equal(result.violations.length, 1)
    assert.ok(result.violations[0].includes('no package.json script invokes it'))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('LIVE GREEN: an excluded test whose script IS invoked in a workflow run: line passes clean', () => {
  const root = makeFixtureRepo()
  try {
    const memberDirs = [join(root, 'apps', 'demo')]
    const scripts = { 'test:excluded': 'bun test apps/demo/test/Excluded.test.ts' }
    const workflowText = 'run: bun run test:excluded'
    const result = deriveVitestExcludeWiring(memberDirs, root, scripts, workflowText, {}, 1)
    assert.deepEqual(result.violations, [])
    assert.equal(result.resolved[0].status, 'wired')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('LIVE GREEN: an excluded test whose script is a named exemption passes clean even when unwired', () => {
  const root = makeFixtureRepo()
  try {
    const memberDirs = [join(root, 'apps', 'demo')]
    const scripts = { 'test:excluded': 'bun test apps/demo/test/Excluded.test.ts' }
    const workflowText = 'run: bun run test:unit'
    const result = deriveVitestExcludeWiring(
      memberDirs,
      root,
      scripts,
      workflowText,
      { 'test:excluded': 'demonstration exemption' },
      1
    )
    assert.deepEqual(result.violations, [])
    assert.equal(result.resolved[0].status, 'exempt')
    assert.equal(result.resolved[0].reason, 'demonstration exemption')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('LIVE: fails closed (throws, no violations array at all) on a malformed vitest config rather than reporting clean', () => {
  const root = mkdtempSync(join(tmpdir(), '923-vitest-exclude-malformed-'))
  try {
    mkdirSync(join(root, 'apps', 'demo', 'test'), { recursive: true })
    writeFileSync(
      join(root, 'apps', 'demo', 'vitest.config.ts'),
      "export default { test: { exclude: ['test/Unclosed.test.ts' } }"
    )
    const memberDirs = [join(root, 'apps', 'demo')]
    assert.throws(
      () => deriveVitestExcludeWiring(memberDirs, root, {}, 'run: bun run test:unit', {}),
      /malformed vitest config/
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('vacuity: deriveVitestExcludeWiring throws when fewer than 2 vitest configs are found under member dirs', () => {
  const root = mkdtempSync(join(tmpdir(), '923-vitest-exclude-vacuity-'))
  try {
    mkdirSync(join(root, 'apps', 'demo'), { recursive: true })
    // No vitest.config.ts anywhere under this member -- checkedConfigs stays empty.
    assert.throws(
      () => deriveVitestExcludeWiring([join(root, 'apps', 'demo')], root, {}, 'run: bun run test:unit', {}),
      /vacuity failure in the scan/
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('readWorkflowFiles: throws when .github/workflows is missing rather than treating it as "nothing wired"', () => {
  const root = mkdtempSync(join(tmpdir(), '923-vitest-exclude-noworkflows-'))
  try {
    assert.throws(() => readWorkflowFiles(root), /cannot read .github\/workflows/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// ---------------------------------------------------------------------------
// The real-repo assertion: today's actual tree must be fully clean once
// test:supervision-owner is wired into ci.yml by this same packet.
// ---------------------------------------------------------------------------

test('REAL REPO: every vitest-excluded test in this repo resolves to a CI-wired or exempted script', () => {
  const pkg = readPackageJson()
  const memberDirs = resolveWorkspaceMemberDirs(repoRoot, pkg)
  const workflowText = readWorkflowFiles(repoRoot)
  const result = deriveVitestExcludeWiring(memberDirs, repoRoot, pkg.scripts, workflowText)
  assert.deepEqual(result.violations, [], result.violations.join('\n'))
  // Sanity: this must have actually READ the real vitest configs, not silently
  // matched zero entries and passed vacuously for the wrong reason.
  //
  // #751/E4 then #751/P0 re-pointed a named anchor file twice; P3 deleted the
  // package that held it. A named-file anchor was always the wrong shape — it
  // had to be re-pointed every time a list changed, it cannot express "empty",
  // and it dies outright when its package does. What the sanity check actually
  // needs to prove is that the derivation read the real configs, so it compares
  // against an INDEPENDENT parse of EVERY member's config. Empty == empty is a
  // verified pass; empty when a config lists entries is still a failure, and a
  // derivation that stops reading a whole member fails the same way.
  const declaredExcludes = []
  for (const memberDir of memberDirs) {
    const configPath = join(repoRoot, memberDir, 'vitest.config.ts')
    if (!existsSync(configPath)) continue
    // Concrete files only: a glob elsewhere in the same config (coverage
    // patterns, `include`) is not an exclude entry this derivation resolves.
    for (const match of readFileSync(configPath, 'utf8').matchAll(/'(test\/[^']+\.test\.ts)'/g)) {
      if (!match[1].includes('*')) declaredExcludes.push(`${memberDir}/${match[1]}`)
    }
  }
  assert.deepEqual(
    result.resolved.map((entry) => entry.path).sort(),
    declaredExcludes.sort(),
    'the derivation must resolve exactly the excludes the members\' vitest configs declare — a mismatch means it stopped reading one of them, which is the vacuous pass this check exists to catch'
  )
})

test('EXEMPT_SCRIPTS: every entry has a non-empty reason string', () => {
  for (const [name, reason] of Object.entries(EXEMPT_SCRIPTS)) {
    assert.ok(typeof reason === 'string' && reason.trim().length > 0, `"${name}" has no reason`)
  }
})
