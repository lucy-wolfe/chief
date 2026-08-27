// #858: a test that searches for its own spawned children by process name
// (`pgrep`/`pkill` matched against a literal script/binary name) must NOT
// use an identifier another test file in the same vitest package also uses
// as a spawn target — that collision is a real, live defect class, distinct
// from #855's missing-deadline class and deliberately checked separately
// (see the ruling below).
//
// WHY THIS EXISTS: `apps/api/test/services/AgentHostServiceReal.test.ts`'s
// orphan-child check (#799) used to run `pgrep -af FakeRpcChild.mjs` after
// `stopAll()` and assert nothing survives. `FakeRpcChild.mjs` is ALSO the
// script `apps/api/test/harness/FakeRpcChild.test.ts` and
// `apps/api/test/services/RpcClientProcessAdapter.test.ts` spawn, in the
// SAME vitest package, which vitest runs as separate, concurrent test FILES
// by default (no `fileParallelism: false`, no pool isolation). Under real
// load a sibling file's still-alive (not orphaned) child matched the same
// `pgrep -af` pattern and false-positived the orphan check — caught by
// eng-e0-s1 via a forced, cache-busted `turbo run test:unit` run, not
// assumed. The fix (`revamp/api/agent-host-core`@30e62216) is the reference
// this check's PASSING fixture is modeled on: generate a marker unique to
// THIS test's own run (`` `no-orphan-test-${Math.random().toString(36)...}`
// ``), pass it as an inert CLI arg to the spawned children (`pgrep -af`
// shows full argv), and scope the search to that marker instead of the
// shared script name.
//
// #855 VS #858 — TWO DEFECTS, ONE FILE, DELIBERATELY TWO CHECKS: the SAME
// motivating instance also had a genuine missing-deadline defect (a
// different test in the same file, "prompt() then waitForIdle()", needed a
// longer per-test budget for a real `node` cold start) — that's #855's
// check, not this one. Folding both into one guard would produce a
// violation message that can't say WHICH defect it found; #855's own commit
// records the reasoning for keeping them separate. This check does ONLY the
// process-identity collision.
//
// WHY THE DETECTOR IS SHAPED THIS WAY (what's cleanly checkable and what
// isn't): "does this test search for its children by an identifier unique
// enough to survive concurrent sibling tests" is NOT decidable from one
// file's text alone — whether a string is "unique enough" is a fact about
// the WHOLE package's spawns, not this file. The naive version of this
// check ("flag any `pgrep`/`pkill` matched by a plain name instead of a
// marker") was considered and rejected for exactly that reason: it would be
// guessing. What IS mechanically checkable, with no guessing: does the
// LITERAL STRING a `pgrep`/`pkill` call searches for ALSO appear, verbatim,
// as a script/binary reference some OTHER file in the same package spawns?
// If yes, that's the concrete, real condition that caused this bug — not an
// inference about uniqueness, a literal string collision. A dynamically
// generated marker (`` `prefix-${Math.random()...}` ``, a template literal
// with interpolation) is never a plain string literal, so it can never
// collide with anything by construction — which is exactly why it's the
// right fix and why this detector naturally accepts it without special-
// casing "this looks like a marker."
//
// SCOPE: every `apps/*` and `packages/*` package with its own
// `vitest.config.ts` (same scope as #855's own guard) — the unit
// `bun run test:unit` actually fans out to. Matching is PER PACKAGE: files
// in different packages run in different vitest processes and cannot
// collide this way.
//
// Run with `node --test scripts/test/process-search-namespace.test.mjs`.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { existsSync, readFileSync, mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { tmpdir } from 'node:os'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(__dirname, '..', '..')

function repoFile(...segments) {
  return join(repoRoot, ...segments)
}

// `cwd` is a parameter (not a module-level constant) so the tamper proof can
// point this SAME real pipeline at an isolated fixture package instead of a
// hand-rolled stand-in for it.
function gitLsFiles(cwd, ...pathspecs) {
  return execFileSync('git', ['ls-files', '-z', '--', ...pathspecs], { cwd, encoding: 'utf8' })
    .split('\0')
    .filter(Boolean)
}

/** Every `vitest.config.ts` under `apps/*` or `packages/*` — one per
 * `bun run test:unit`-driven package, found from source (git-tracked), never
 * a hardcoded list. Matches #855's own `vitestConfigs`. */
function vitestConfigs(cwd) {
  return gitLsFiles(cwd, 'apps', 'packages').filter((f) => /^(apps|packages)\/[^/]+\/vitest\.config\.ts$/.test(f))
}

/** Every `.test.ts`/`.test.tsx` file under `<packageDir>/test/`. */
function packageTestFiles(cwd, packageDir) {
  return (
    gitLsFiles(cwd, join(packageDir, 'test'))
      .filter((f) => /\.test\.tsx?$/.test(f))
      // #751/P4: `git ls-files --cached` reports the INDEX, so a test file
      // deleted in the working tree but not yet staged is still listed — and
      // this scan then died on a bare `ENOENT: ... open '<path>'` out of
      // readFileSync, taking the real collision check down with it and
      // reporting something that reads as a broken checkout rather than as
      // anything about the code. Hit for real while #751/P4 deleted seven
      // dead apps/cli test files. A path with no bytes on disk contains no
      // search literals and no spawn literals; skipping it gives the same
      // answer the scan would give once the deletion is staged, reached
      // without a crash in between. sql-only-state.test.mjs carries the same
      // filter for the same reason.
      .filter((f) => existsSync(join(cwd, f)))
  )
}

// A `pgrep`/`pkill` call via the node:child_process family. Deliberately
// narrow to the mechanism actually observed (`execFileSync('pgrep', [...])`)
// plus its sync siblings — a process-search call, not a spawn call.
const PROCESS_SEARCH_CALL = /\b(?:execFileSync|execSync|spawnSync)\s*\(\s*['"](pgrep|pkill)['"]/g

/** Every PLAIN string-literal argument in a `pgrep`/`pkill` call's argv
 * array, excluding flags (`-af`, `-x`, ...). A backtick template literal
 * with `${` interpolation — the marker idiom — is NEVER matched here: it is
 * not a plain quoted string, which is exactly why a dynamically generated
 * marker can never trip this detector by construction. */
function processSearchLiterals(text) {
  const literals = []
  let match
  PROCESS_SEARCH_CALL.lastIndex = 0
  while ((match = PROCESS_SEARCH_CALL.exec(text))) {
    // The argv array is the call's second argument; scan a bounded window
    // after the match for the array literal's own quoted-string elements —
    // a full-parenthesis-balance parse is unnecessary here since we only
    // need the FIRST bracketed array right after the call, and pgrep/pkill
    // argv arrays in every real call site observed are short and single-line
    // or short multi-line.
    const windowEnd = text.indexOf(')', match.index)
    const window = windowEnd === -1 ? text.slice(match.index, match.index + 400) : text.slice(match.index, windowEnd + 1)
    const stringLiteralRe = /'([^'\\]*(?:\\.[^'\\]*)*)'|"([^"\\]*(?:\\.[^"\\]*)*)"/g
    let literalMatch
    while ((literalMatch = stringLiteralRe.exec(window))) {
      const value = literalMatch[1] ?? literalMatch[2]
      if (value.startsWith('-')) continue // a flag, not a search identifier
      if (value === 'pgrep' || value === 'pkill') continue // the program name itself
      literals.push(value)
    }
  }
  return literals
}

/** Every literal string in `text` that looks like a script/binary reference
 * — ends in a common executable-script extension. Deliberately broad (any
 * quoted string with that suffix, wherever it appears: a `new URL(...)`
 * argument, a `cliPath` assignment, an `execFileSync` argv element) rather
 * than trying to resolve exactly which construct spawns it — the collision
 * this check exists to catch is a literal TEXT match a real `pgrep -af`
 * would also make, so matching the same way `pgrep -af` does (substring
 * against argv/whatever text is present) is the right fidelity, not a
 * narrower one requiring full spawn-site resolution. */
const SCRIPT_REFERENCE_LITERAL = /'([^'\\]*\.(?:mjs|cjs|js|sh|py))'|"([^"\\]*\.(?:mjs|cjs|js|sh|py))"/g

function scriptReferenceLiterals(text) {
  const literals = []
  let match
  SCRIPT_REFERENCE_LITERAL.lastIndex = 0
  while ((match = SCRIPT_REFERENCE_LITERAL.exec(text))) {
    literals.push(match[1] ?? match[2])
  }
  return literals
}

/**
 * Scan one vitest-driven package for the real collision: a `pgrep`/`pkill`
 * search literal in one file that is also a substring of a script-reference
 * literal in a DIFFERENT file in the same package. Reports THE PAIR (per
 * team-lead's ruling) — the fix may belong to either side: the searcher
 * scoping its marker, or the spawner namespacing what it runs.
 */
function scanPackageForCollisions(cwd, configPath) {
  const packageDir = dirname(configPath)
  const files = packageTestFiles(cwd, packageDir)
  const fileTexts = new Map(files.map((file) => [file, readFileSync(join(cwd, file), 'utf8')]))

  // Every script-reference literal, per file — used as the "what does
  // someone else in this package spawn" side of the comparison.
  const referencesByFile = new Map(files.map((file) => [file, scriptReferenceLiterals(fileTexts.get(file))]))

  const collisions = []
  for (const searchFile of files) {
    const searchLiterals = processSearchLiterals(fileTexts.get(searchFile))
    for (const searchLiteral of searchLiterals) {
      for (const spawnFile of files) {
        if (spawnFile === searchFile) continue
        const hit = referencesByFile.get(spawnFile).find((reference) => reference.includes(searchLiteral))
        if (hit !== undefined) {
          collisions.push({ searchFile, searchLiteral, spawnFile, spawnReference: hit })
        }
      }
    }
  }
  return { packageDir, collisions }
}

// #848: a scan root that no longer exists (or a glob that stops matching)
// must refuse to trust its own empty result rather than pass vacuously.
// Matches #855's own floor and reasoning.
const MINIMUM_VITEST_PACKAGES = 3

test(`at least ${MINIMUM_VITEST_PACKAGES} vitest.config.ts packages are found — a near-zero count refuses to run rather than passing quietly`, () => {
  const configs = vitestConfigs(repoRoot)
  console.log(`[process-search-namespace] found ${configs.length} vitest.config.ts package(s): ${configs.join(', ')}`)
  if (configs.length < MINIMUM_VITEST_PACKAGES) {
    throw new Error(
      `only ${configs.length} vitest.config.ts package(s) found under apps/*, packages/* — below the expected ` +
        `floor of ${MINIMUM_VITEST_PACKAGES}. A glob or directory move probably broke this scan (#848). ` +
        'REFUSING TO TRUST THIS RESULT.'
    )
  }
})

test('no test file searches for processes by an identifier another file in the same package also spawns', () => {
  const configs = vitestConfigs(repoRoot)
  const results = configs.map((configPath) => scanPackageForCollisions(repoRoot, configPath))
  const violations = results.filter((r) => r.collisions.length > 0)

  assert.deepEqual(
    violations.map((v) => ({
      package: v.packageDir,
      collisions: v.collisions.map(
        (c) =>
          `${c.searchFile} searches for processes matching '${c.searchLiteral}', which ${c.spawnFile} also spawns ` +
          `(via '${c.spawnReference}') — under real vitest file-level parallelism, ${c.spawnFile}'s live child can ` +
          `be misread as ${c.searchFile}'s orphan, or vice versa. Fix either side: scope ${c.searchFile}'s search ` +
          `to a marker it owns, or namespace what ${c.spawnFile} spawns.`
      )
    })),
    [],
    'a process-search literal must never collide with a spawn-target literal in a sibling test file (#858)'
  )
})

// ---------------------------------------------------------------------------
// TAMPER PROOF: builds REAL isolated fixture packages on disk (not a
// hand-rolled in-memory stand-in) and runs the actual
// scanPackageForCollisions pipeline against them — proving end to end that
// a real literal collision is caught, reports BOTH files, and that the real
// fix (a dynamically generated marker) clears it without needing a special
// case.
// ---------------------------------------------------------------------------

function writeFixturePackage(root, testFiles) {
  const packageDir = join(root, 'fixture-pkg')
  const testDir = join(packageDir, 'test')
  mkdirSync(testDir, { recursive: true })
  writeFileSync(
    join(packageDir, 'vitest.config.ts'),
    "import { defineConfig } from 'vitest/config'\nexport default defineConfig({ test: { include: ['test/**/*.test.ts'] } })\n"
  )
  for (const [name, body] of Object.entries(testFiles)) {
    writeFileSync(join(testDir, name), body)
  }
  execFileSync('git', ['init', '-q'], { cwd: root })
  execFileSync('git', ['add', '-A'], { cwd: root })
  return 'fixture-pkg/vitest.config.ts'
}

test('tamper proof: a real literal collision between a spawner and a searcher, in separate files, is caught and names both', () => {
  const spawnerBody = "const CLI_PATH = new URL('./FakeChild.mjs', import.meta.url)\nit('spawns', () => { void CLI_PATH })\n"
  const searcherBody =
    "import { execFileSync } from 'node:child_process'\n" +
    "it('checks for orphans', () => { execFileSync('pgrep', ['-af', 'FakeChild.mjs']) })\n"

  const root = mkdtempSync(join(tmpdir(), 'process-search-namespace-tamper-'))
  try {
    const configPath = writeFixturePackage(root, { 'Spawner.test.ts': spawnerBody, 'Searcher.test.ts': searcherBody })
    const result = scanPackageForCollisions(root, configPath)
    assert.equal(result.collisions.length, 1, `expected exactly one collision: ${JSON.stringify(result.collisions)}`)
    assert.equal(result.collisions[0].searchFile, 'fixture-pkg/test/Searcher.test.ts')
    assert.equal(result.collisions[0].spawnFile, 'fixture-pkg/test/Spawner.test.ts')
    assert.equal(result.collisions[0].searchLiteral, 'FakeChild.mjs')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('tamper proof: the real fix — a dynamically generated marker — clears the collision without a special case', () => {
  const spawnerBody = "const CLI_PATH = new URL('./FakeChild.mjs', import.meta.url)\nit('spawns', () => { void CLI_PATH })\n"
  const searcherBody =
    "import { execFileSync } from 'node:child_process'\n" +
    "it('checks for orphans', () => {\n" +
    "  const marker = `no-orphan-test-${Math.random().toString(36).slice(2)}`\n" +
    "  execFileSync('pgrep', ['-af', marker])\n" +
    '})\n'

  const root = mkdtempSync(join(tmpdir(), 'process-search-namespace-tamper-'))
  try {
    const configPath = writeFixturePackage(root, { 'Spawner.test.ts': spawnerBody, 'Searcher.test.ts': searcherBody })
    const result = scanPackageForCollisions(root, configPath)
    assert.equal(
      result.collisions.length,
      0,
      `a template-literal marker must never collide — nothing to compare against a plain string: ${JSON.stringify(result.collisions)}`
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('tamper proof: a pgrep search literal that matches nothing anywhere else in the package is never flagged', () => {
  const searcherBody =
    "import { execFileSync } from 'node:child_process'\n" +
    "it('checks for orphans', () => { execFileSync('pgrep', ['-af', 'NothingSpawnsThis.mjs']) })\n"
  const plainBody = "it('adds', () => { void (1 + 1) })\n"

  const root = mkdtempSync(join(tmpdir(), 'process-search-namespace-tamper-'))
  try {
    const configPath = writeFixturePackage(root, { 'Searcher.test.ts': searcherBody, 'Plain.test.ts': plainBody })
    const result = scanPackageForCollisions(root, configPath)
    assert.equal(result.collisions.length, 0, `a search literal with no real spawn-target match must never be flagged: ${JSON.stringify(result.collisions)}`)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('tamper proof: a spawn/search pair inside the SAME file is not flagged — the collision this check targets is cross-FILE', () => {
  const sameFileBody =
    "import { execFileSync } from 'node:child_process'\n" +
    "const CLI_PATH = new URL('./FakeChild.mjs', import.meta.url)\n" +
    "it('spawns then checks', () => {\n" +
    '  void CLI_PATH\n' +
    "  execFileSync('pgrep', ['-af', 'FakeChild.mjs'])\n" +
    '})\n'

  const root = mkdtempSync(join(tmpdir(), 'process-search-namespace-tamper-'))
  try {
    const configPath = writeFixturePackage(root, { 'SameFile.test.ts': sameFileBody })
    const result = scanPackageForCollisions(root, configPath)
    assert.equal(
      result.collisions.length,
      0,
      `same-file spawn+search must not be flagged — this check targets cross-file collisions under vitest's file-level parallelism: ${JSON.stringify(result.collisions)}`
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('negative self-test: pgrep flags (like -af) and the program name itself are never treated as search identifiers', () => {
  const text = "execFileSync('pgrep', ['-af', '-x', 'real-target.mjs'])\n"
  const literals = processSearchLiterals(text)
  assert.deepEqual(literals, ['real-target.mjs'])
})