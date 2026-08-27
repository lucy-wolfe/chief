// GUARD: no file under `scripts/` is a file nothing invokes.
//
// The derivation, the closure and the register live in
// `scripts/script-invocation-lib.mjs`; this file is the assertion half.
//
// WHY THIS EXISTS
// ---------------
// `scripts/orphanable-spawner-scan.ts` was a correct spawn scanner with a
// correct allowlist that no `package.json` script, no workflow and no gate
// driver invoked. This repo already had the sentence for it — "a correct,
// CI-wired guard nobody runs before pushing produces exactly the same outcome
// as a broken guard" — and that one was not even wired. The sweep that found it
// found twenty-six more files under `scripts/` in the same state, and deleted
// them; what could not be deleted was the ability to make another.
//
// `guard-wiring.test.mjs` polices `scripts/test/*.test.mjs` and nothing else,
// which is exactly why a scanner written as a `.ts` file one directory up was
// invisible to it. This guard is that guard's other half: the domain is every
// runnable file under `scripts/`, and the verdict is invoked, registered, or
// failing.
//
// Arms:
//   1. THE REAL TREE — nothing unrun; no register row stale; no register row
//      retired by CI having quietly picked the script up.
//   2. ROW SHAPE — a row that cannot be checked is not a fact.
//   3. NON-VACUITY — a clean answer from a closure that reached nothing is not
//      evidence. Floors on both the inventory and the invoked set, and the four
//      root kinds asserted to be really contributing.
//   4. DEMONSTRATED RED — an unwired script, a stale row, a retired row, and
//      each shape of reference, all against fixture trees.
//
// Fixtures are written under `mkdtemp`, never into the checkout: this suite has
// to leave the working tree byte-identical (`ci-guard-shard`).
//
// Run with `node --test scripts/test/script-invocation.test.mjs`.

import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import {
  MIN_PLAUSIBLE_INVOKED,
  MIN_PLAUSIBLE_SCRIPT_FILES,
  OPERATOR_ENTRYPOINTS,
  auditScriptInvocation,
  deriveInvokedScripts,
  invocationsIn,
  registerShapeViolations,
  scriptInventory,
  stripComments,
} from '../script-invocation-lib.mjs'

const REPO_ROOT = fileURLToPath(new URL('../..', import.meta.url))

/** Audit the real checkout once; every arm below reads this. */
const AUDIT = auditScriptInvocation(REPO_ROOT)

/** A throwaway repo-shaped tree, outside the checkout. */
function fixture(files) {
  const root = mkdtempSync(join(tmpdir(), 'script-invocation-'))
  mkdirSync(join(root, '.github', 'workflows'), { recursive: true })
  mkdirSync(join(root, 'scripts', 'test'), { recursive: true })
  writeFileSync(join(root, 'package.json'), JSON.stringify({ scripts: {} }))
  writeFileSync(join(root, 'turbo.json'), '{}')
  for (const [path, contents] of Object.entries(files)) {
    const absolute = join(root, path)
    mkdirSync(dirname(absolute), { recursive: true })
    writeFileSync(absolute, contents)
  }
  return root
}

// --- 1. the real tree --------------------------------------------------------

test('every file under scripts/ is invoked by something, or registered with a reason', () => {
  assert.deepEqual(
    AUDIT.unrun,
    [],
    'these files under scripts/ are invoked by no package.json script, no workflow, no vitest config, ' +
      'no turbo task, no other script and no guard. Wire each one or delete it — a checker nothing runs ' +
      'produces exactly the outcome of a deleted one, at the cost of every reader who has to work out ' +
      'whether it matters. If a human really runs it by name, add a row to OPERATOR_ENTRYPOINTS saying who ' +
      'and when.',
  )
})

test('no register row names a file this tree does not have', () => {
  assert.deepEqual(AUDIT.staleRows, [])
})

test('no register row survives its script becoming wired', () => {
  assert.deepEqual(AUDIT.wiredRows, [])
})

// --- 2. row shape ------------------------------------------------------------

test('every register row is a checkable fact: a scripts/ path, a date, a written reason', () => {
  assert.deepEqual(registerShapeViolations(), [])
})

// --- 3. non-vacuity ----------------------------------------------------------

test('the audit is not vacuous: it walked a real tree and the closure really reached it', () => {
  assert.ok(
    AUDIT.inventory.length >= MIN_PLAUSIBLE_SCRIPT_FILES,
    `only ${AUDIT.inventory.length} files under scripts/ — the walk is broken, not the tree`,
  )
  assert.ok(
    AUDIT.invokedBy.size >= MIN_PLAUSIBLE_INVOKED,
    `the closure reached only ${AUDIT.invokedBy.size} scripts — the roots stopped resolving, which would ` +
      'report the whole tree as unrun rather than report a defect',
  )
  // Every kind of root must be contributing something. A root that silently
  // stops matching is the failure mode this whole file is about, one level up:
  // it would not error, it would just quietly enlarge the unrun list.
  const sources = [...AUDIT.invokedBy.values()].flat().map((entry) => entry.source)
  for (const kind of [
    { what: 'a package.json script', matcher: (source) => source.includes('package.json script') },
    { what: 'a workflow', matcher: (source) => source.startsWith('.github/workflows/') },
    { what: 'a vitest config', matcher: (source) => source.endsWith('vitest.config.ts') },
    { what: 'the derived guard corpus', matcher: (source) => source.includes('guard-count.mjs') },
    { what: 'another script', matcher: (source) => source.startsWith('scripts/') },
  ]) {
    assert.ok(
      sources.some(kind.matcher),
      `no script is reached through ${kind.what} — that root kind has stopped resolving`,
    )
  }
})

test('the register is residue, not a parking lot', () => {
  // A register that grows without bound is the third option this rule says does
  // not exist, arriving slowly. This is a ceiling on the EXEMPTIONS, not on the
  // tree: it fails when someone registers their way out of wiring or deleting.
  assert.ok(
    OPERATOR_ENTRYPOINTS.length <= 20,
    `${OPERATOR_ENTRYPOINTS.length} operator entrypoints registered — a register this size has stopped ` +
      'being the residue of a sweep and become the place unrun scripts go',
  )
})

// --- 4. demonstrated red -----------------------------------------------------

test('DEMONSTRATED RED: an unwired script is named, and wiring it clears the violation', () => {
  const root = fixture({
    'scripts/a-scanner-nobody-runs.mjs': 'console.log("hello")\n',
    'scripts/wired.sh': 'echo hi\n',
    '.github/workflows/ci.yml': 'jobs:\n  a:\n    steps:\n      - run: bash scripts/wired.sh\n',
  })
  try {
    const before = auditScriptInvocation(root, { register: [] })
    assert.deepEqual(before.unrun, ['scripts/a-scanner-nobody-runs.mjs'])

    // Wiring it into the SAME workflow clears it, with no edit to any register.
    writeFileSync(
      join(root, '.github/workflows/ci.yml'),
      'jobs:\n  a:\n    steps:\n      - run: bash scripts/wired.sh\n      - run: node scripts/a-scanner-nobody-runs.mjs\n',
    )
    assert.deepEqual(auditScriptInvocation(root, { register: [] }).unrun, [])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('DEMONSTRATED RED: a register row naming a deleted file says to delete itself, by name', () => {
  const root = fixture({ 'scripts/real.sh': 'echo hi\n' })
  try {
    const { staleRows } = auditScriptInvocation(root, {
      register: [
        { file: 'scripts/gone.sh', registeredOn: '2026-08-10', reason: 'x'.repeat(50) },
        { file: 'scripts/real.sh', registeredOn: '2026-08-10', reason: 'y'.repeat(50) },
      ],
    })
    assert.deepEqual(staleRows, ['scripts/gone.sh — no such file under scripts/ today; delete this row'])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('DEMONSTRATED RED: a register row whose script CI now runs says to delete itself, by name', () => {
  const root = fixture({
    'scripts/promoted.sh': 'echo hi\n',
    '.github/workflows/ci.yml': 'jobs:\n  a:\n    steps:\n      - run: bash scripts/promoted.sh\n',
  })
  const register = [{ file: 'scripts/promoted.sh', registeredOn: '2026-08-10', reason: 'z'.repeat(50) }]
  try {
    const { wiredRows } = auditScriptInvocation(root, { register })
    assert.equal(wiredRows.length, 1)
    assert.match(wiredRows[0], /scripts\/promoted\.sh — is now invoked by \.github\/workflows\/ci\.yml/)
    assert.match(wiredRows[0], /delete this row, the script is wired/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('DEMONSTRATED RED: a reference from ANOTHER SCRIPT never retires a row', () => {
  // The direction that has to stay closed. `gate-matrix-sequence.test.mjs` reads
  // `scripts/gate-matrix.sh` line by line to lock its stage order and executes
  // none of it; retiring the row on that reference would delete the only record
  // of who actually runs the merger's gate driver.
  const root = fixture({
    'scripts/operator-only.sh': 'echo hi\n',
    'scripts/test/reads-it.test.mjs': "import { readFileSync } from 'node:fs'\nreadFileSync('scripts/operator-only.sh')\n",
  })
  const register = [{ file: 'scripts/operator-only.sh', registeredOn: '2026-08-10', reason: 'q'.repeat(50) }]
  try {
    const audit = auditScriptInvocation(root, { register })
    assert.deepEqual(audit.wiredRows, [])
    assert.deepEqual(audit.unrun, [])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('DEMONSTRATED RED: a row with no date or no written reason fails', () => {
  const violations = registerShapeViolations([
    { file: 'scripts/a.sh', registeredOn: '2026-08-10', reason: 'short' },
    { file: 'scripts/b.sh', registeredOn: '', reason: 'x'.repeat(50) },
    { file: 'apps/c.sh', registeredOn: '2026-08-10', reason: 'y'.repeat(50) },
    { file: 'scripts/a.sh', registeredOn: '2026-08-10', reason: 'z'.repeat(50) },
  ])
  assert.ok(violations.some((v) => v.includes('no written reason')))
  assert.ok(violations.some((v) => v.includes('no registration date')))
  assert.ok(violations.some((v) => v.includes('must name a file under scripts/')))
  assert.ok(violations.some((v) => v.includes('registered twice')))
})

// --- the shapes the closure has to be able to see ---------------------------

test('every reference shape this repo actually uses is recognised', () => {
  const inventory = [
    'scripts/runner.sh',
    'scripts/lib/common.sh',
    'scripts/tool.mjs',
    'scripts/probe.ts',
    'scripts/test/setup.mjs',
  ]
  const seen = (text, file) => invocationsIn(text, { file, repoRoot: '/repo', inventory })

  assert.deepEqual(seen('run: bash scripts/runner.sh --flag', '.github/workflows/ci.yml'), ['scripts/runner.sh'])
  assert.deepEqual(seen('node --test scripts/test/setup.mjs', '.github/workflows/ci.yml'), ['scripts/test/setup.mjs'])
  assert.deepEqual(seen('bun run scripts/probe.ts', 'package.json'), ['scripts/probe.ts'])
  assert.deepEqual(seen('. "$HERE/lib/common.sh"', 'scripts/runner.sh'), ['scripts/lib/common.sh'])
  assert.deepEqual(seen('"$here/runner.sh" "$log"', 'scripts/tool.mjs'), ['scripts/runner.sh'])
  assert.deepEqual(seen('PY="$HERE/tool.mjs"', 'scripts/runner.sh'), ['scripts/tool.mjs'])
  assert.deepEqual(seen("import { x } from '../tool.mjs'", 'scripts/test/setup.mjs'), ['scripts/tool.mjs'])
  assert.deepEqual(
    seen("globalSetup: ['../../scripts/test/setup.mjs'],", 'packages/p/vitest.config.ts'),
    ['scripts/test/setup.mjs'],
  )
  assert.deepEqual(seen('const G = join(dir, "..", "runner.sh")', 'scripts/test/setup.mjs'), ['scripts/runner.sh'])
})

test('a MENTION is not an invocation: prose and data rows do not count as callers', () => {
  const inventory = ['scripts/scanner.ts', 'scripts/runner.sh']
  const seen = (text, file) => invocationsIn(text, { file, repoRoot: '/repo', inventory })

  // The exact defect an earlier draft had: a known-coverage-gap row naming the
  // scanner made the scanner look invoked, and the file this whole packet is
  // about escaped its own guard.
  assert.deepEqual(seen('const KNOWN_GAP = [\n  "scripts/scanner.ts",\n]\n', 'scripts/test/gap.test.mjs'), [])
  // A comment naming a sibling is documentation.
  assert.deepEqual(seen('// see scripts/runner.sh for the sequence\nexport const x = 1\n', 'scripts/tool.mjs'), [])
  assert.deepEqual(seen('# scripts/runner.sh does the same thing\necho hi\n', 'scripts/other.sh'), [])
})

test('comment stripping does not eat the file: a block-comment marker inside a line comment is inert', () => {
  // The measured regression this scanner had: stripping `/* */` before `//` let
  // a `/*` inside a line comment open a phantom block that ran to the next `*/`
  // anywhere in the file, swallowing 14,000 of 17,478 characters including the
  // import that named the library under test.
  const text = ['// a line comment containing /* an opener', "import { x } from '../tool.mjs'", '/* real */ const y = 1'].join(
    '\n',
  )
  const stripped = stripComments(text, 'scripts/test/a.test.mjs')
  assert.match(stripped, /tool\.mjs/, 'the import survived the strip')
  assert.doesNotMatch(stripped, /an opener/, 'the line comment was removed')
  assert.doesNotMatch(stripped, /real/, 'the block comment was removed')
})

test('the closure is transitive: a script a registered entrypoint calls is invoked', () => {
  const root = fixture({
    'scripts/entry.sh': 'bash "$HERE/helper.sh"\n',
    'scripts/helper.sh': 'echo hi\n',
  })
  try {
    const register = [{ file: 'scripts/entry.sh', registeredOn: '2026-08-10', reason: 'r'.repeat(50) }]
    const invoked = deriveInvokedScripts(root, { inventory: scriptInventory(root), register })
    assert.ok(invoked.has('scripts/helper.sh'), 'a callee of a registered entrypoint is invoked, not unrun')
    assert.deepEqual(auditScriptInvocation(root, { register }).unrun, [])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
