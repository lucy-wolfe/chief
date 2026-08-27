// #907: proves scripts/guard-count.mjs's derivation actually MOVES when the
// tree changes, rather than merely existing. A count that has never been
// observed changing is a claim, not a check (§0.6's self-test discipline,
// applied to a count instead of a numeric bound).

import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'

import {
  deriveAllGuards,
  deriveGuardFiles,
  deriveShellGateFiles,
  findWorkflowRunLines,
} from '../guard-count.mjs'

test('deriveGuardFiles enumerates every scripts/test/*.test.mjs on disk, not a remembered subset', () => {
  const files = deriveGuardFiles()
  // Not asserting an exact number here -- that would be exactly the
  // transcribed-count trap this packet exists to close. Asserting the
  // shape (real guard files this repo is known to have) is the check that
  // survives a legitimate future addition without an edit.
  assert.ok(files.length >= 14, `expected at least 14 real guard files, got ${files.length}`)
  assert.ok(files.includes('guard-wiring.test.mjs'))
  assert.ok(files.includes('sql-only-state.test.mjs'))
  assert.ok(files.every((f) => f.endsWith('.test.mjs')))
})

test('demonstrated: adding a guard file changes the derived count with ZERO edits to guard-count.mjs', () => {
  const dir = mkdtempSync(join(tmpdir(), 'guard-count-demo-'))
  try {
    writeFileSync(join(dir, 'a.test.mjs'), '')
    writeFileSync(join(dir, 'b.test.mjs'), '')
    const before = deriveGuardFiles(dir)
    assert.equal(before.length, 2)

    writeFileSync(join(dir, 'c.test.mjs'), '')
    const after = deriveGuardFiles(dir)
    assert.equal(after.length, 3, 'adding a third guard file must be picked up with no code change')
    assert.ok(after.includes('c.test.mjs'))
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('demonstrated: removing a guard file changes the derived count with ZERO edits to guard-count.mjs', () => {
  const dir = mkdtempSync(join(tmpdir(), 'guard-count-demo-'))
  try {
    writeFileSync(join(dir, 'a.test.mjs'), '')
    writeFileSync(join(dir, 'b.test.mjs'), '')
    assert.equal(deriveGuardFiles(dir).length, 2)

    rmSync(join(dir, 'b.test.mjs'))
    assert.equal(deriveGuardFiles(dir).length, 1, 'removing a guard file must be picked up with no code change')
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a directory with non-.test.mjs files is not miscounted -- only real guard files count', () => {
  const dir = mkdtempSync(join(tmpdir(), 'guard-count-demo-'))
  try {
    writeFileSync(join(dir, 'a.test.mjs'), '')
    writeFileSync(join(dir, 'README.md'), '')
    writeFileSync(join(dir, 'fixture.txt'), '')
    writeFileSync(join(dir, 'helper.mjs'), '') // not .test.mjs -- a library file, not a guard
    assert.equal(deriveGuardFiles(dir).length, 1)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('demonstrated red: an empty directory refuses rather than silently reporting zero guards as a pass', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'guard-count-empty-'))
  try {
    assert.equal(deriveGuardFiles(dir).length, 0)
    // The CLI entrypoint (not exercised by unit import) is what turns this
    // into a refusal -- see guard-count.mjs's `if (files.length === 0)`
    // branch, exercised via the CLI in ci.yml. Asserted at the unit level
    // here as the precondition that branch depends on.
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

// #916: the `scripts/test/*.test.mjs` enumeration above is blind to a real
// second category of CI-wired gate -- a plain shell script invoked from a
// `.github/workflows/*.yml` `run:` line, either directly or one hop through
// a package.json script name. This section proves the SECOND derivation
// moves the same way the first one does (§0.6's self-test discipline,
// applied to the new category rather than assumed to inherit it).

test('deriveShellGateFiles finds the three real CI-wired shell gates against the real tree, naming workflow and line', () => {
  const found = deriveShellGateFiles()
  const names = found.map((f) => f.file)
  // Not asserting the count is exactly 3 -- a legitimate future shell gate
  // must not require an edit here, mirroring deriveGuardFiles' own
  // "shape, not a transcribed number" posture above.
  assert.ok(names.includes('scripts/typecheck.sh'), 'typecheck.sh is reached one hop through `bun run typecheck`')
  assert.ok(names.includes('scripts/cargo-check-macos.sh'))
  assert.ok(names.includes('scripts/cargo-test-workspace.sh'))
  for (const entry of found) {
    assert.ok(entry.workflow.length > 0, 'every entry must name the workflow file it was found in')
    assert.ok(entry.line > 0, 'every entry must name the line it was found on')
    assert.ok(entry.via.length > 0, 'every entry must quote the literal command CI runs')
  }
})

test('demonstrated: a new shell gate wired directly into a workflow is picked up with ZERO edits to guard-count.mjs', () => {
  const workflowsDir = mkdtempSync(join(tmpdir(), 'guard-count-workflows-'))
  try {
    const workflowPath = join(workflowsDir, 'ci.yml')
    writeFileSync(
      workflowPath,
      'jobs:\n  example:\n    steps:\n      - name: a throwaway shell gate\n        run: bash scripts/injected-demo-gate.sh\n',
    )
    // Assert the write actually landed before trusting anything derived
    // from it (ENGINEER-BRIEF §0.6: a setup step that silently fails to
    // apply must never be allowed to fall through into a result that looks
    // like a real pass).
    const written = readFileSync(workflowPath, 'utf8')
    assert.ok(written.includes('scripts/injected-demo-gate.sh'), 'the injected workflow line did not land -- aborting rather than trusting a stale directory')

    const runLines = findWorkflowRunLines(workflowsDir)
    assert.equal(runLines.length, 1)
    const found = deriveShellGateFiles({ runLines, scripts: {} })
    assert.equal(found.length, 1)
    assert.equal(found[0].file, 'scripts/injected-demo-gate.sh')
    assert.equal(found[0].workflow, 'ci.yml')
    assert.equal(found[0].line, 5)
  } finally {
    rmSync(workflowsDir, { recursive: true, force: true })
  }
})

test('demonstrated: a shell gate reached one hop through a package.json script name is found, and a name with no script entry is not', () => {
  const workflowsDir = mkdtempSync(join(tmpdir(), 'guard-count-workflows-'))
  try {
    writeFileSync(
      join(workflowsDir, 'ci.yml'),
      'jobs:\n  example:\n    steps:\n      - run: bun run injected-demo-typecheck\n      - run: bun run injected-demo-nonexistent\n',
    )
    const runLines = findWorkflowRunLines(workflowsDir)
    assert.equal(runLines.length, 2)
    const found = deriveShellGateFiles({
      runLines,
      scripts: { 'injected-demo-typecheck': 'bash scripts/injected-demo-gate.sh' },
    })
    assert.equal(found.length, 1, 'a `bun run` name with no matching package.json script must not be miscounted as a shell gate')
    assert.equal(found[0].file, 'scripts/injected-demo-gate.sh')
    assert.match(found[0].via, /injected-demo-typecheck.*injected-demo-gate\.sh/s)
  } finally {
    rmSync(workflowsDir, { recursive: true, force: true })
  }
})

test('demonstrated: removing the workflow line drops the shell gate with ZERO edits to guard-count.mjs', () => {
  const workflowsDir = mkdtempSync(join(tmpdir(), 'guard-count-workflows-'))
  try {
    const workflowPath = join(workflowsDir, 'ci.yml')
    writeFileSync(workflowPath, 'jobs:\n  example:\n    steps:\n      - run: bash scripts/injected-demo-gate.sh\n')
    assert.equal(deriveShellGateFiles({ runLines: findWorkflowRunLines(workflowsDir), scripts: {} }).length, 1)

    writeFileSync(workflowPath, 'jobs:\n  example:\n    steps:\n      - run: echo nothing here\n')
    const after = readFileSync(workflowPath, 'utf8')
    assert.ok(!after.includes('injected-demo-gate.sh'), 'the rewrite did not land -- aborting rather than trusting a stale file')
    assert.equal(deriveShellGateFiles({ runLines: findWorkflowRunLines(workflowsDir), scripts: {} }).length, 0)
  } finally {
    rmSync(workflowsDir, { recursive: true, force: true })
  }
})

test('a run: line that is not a shell-script invocation is not miscounted -- only real scripts/*.sh gates count', () => {
  const workflowsDir = mkdtempSync(join(tmpdir(), 'guard-count-workflows-'))
  try {
    writeFileSync(
      join(workflowsDir, 'ci.yml'),
      [
        'jobs:',
        '  example:',
        '    steps:',
        '      - run: bun run test:unit',
        '      - run: cargo build --release',
        '      - run: node scripts/some-tool.mjs',
        '      - run: bash scripts/real-gate.sh',
        '',
      ].join('\n'),
    )
    const found = deriveShellGateFiles({ runLines: findWorkflowRunLines(workflowsDir), scripts: {} })
    assert.equal(found.length, 1)
    assert.equal(found[0].file, 'scripts/real-gate.sh')
  } finally {
    rmSync(workflowsDir, { recursive: true, force: true })
  }
})

test('demonstrated red: a workflow directory that resolves to zero run: lines is a vacuity failure the CLI refuses, not a silent pass', () => {
  const workflowsDir = mkdtempSync(join(tmpdir(), 'guard-count-empty-workflows-'))
  try {
    writeFileSync(join(workflowsDir, 'ci.yml'), 'jobs:\n  example:\n    steps: []\n')
    assert.equal(findWorkflowRunLines(workflowsDir).length, 0)
    // The CLI entrypoint (not exercised by unit import) is what turns this
    // into a refusal -- see guard-count.mjs's MIN_PLAUSIBLE_RUN_LINES
    // branch, exercised via the CLI in ci.yml. Asserted at the unit level
    // here as the precondition that branch depends on.
  } finally {
    rmSync(workflowsDir, { recursive: true, force: true })
  }
})

test('deriveAllGuards tags every entry by category and never merges the three lists', () => {
  const all = deriveAllGuards()
  const testEntries = all.filter((g) => g.category === 'test.mjs')
  const shellEntries = all.filter((g) => g.category === 'shell-gate')
  const bunTestEntries = all.filter((g) => g.category === 'bun-test-suite')
  assert.equal(testEntries.length + shellEntries.length + bunTestEntries.length, all.length, 'every entry must carry exactly one of the three known categories')
  assert.ok(testEntries.length >= 14)
  assert.ok(shellEntries.length >= 3)
  // #977 set this floor at 11, the number of CI-wired apps/cli bun:test
  // suites at the time. #751/E4 deleted ten of them with the TypeScript
  // modules they tested (and their package.json scripts and ci.yml steps
  // with them), leaving `LauncherRunnerStdin.test.ts` as the only one. The
  // floor is re-anchored to 1 rather than deleted, because what it protects
  // is unchanged and still live: this category must never collapse to ZERO
  // silently, which is what would happen if the derivation's script->suite
  // resolution broke. The EXACT membership is asserted by
  // scripts/test/gate-matrix-legs.test.mjs's own set-equality control, so a
  // suite appearing or disappearing is still caught by name — this line is
  // the vacuity floor, not the inventory.
  // ZERO is now a legal answer, and the floor asks the question that actually
  // matters instead of asserting a count. #751's `apps/api`/`src/legacy`
  // deletion took `LauncherRunnerStdin.test.ts` — the last bun:test suite —
  // with the runner it tested, so this category genuinely derives empty. A
  // hardcoded `>= 1` would have failed on a correct tree, which is the same
  // "guard blocks the check it exists to protect" shape the typecheck floor
  // above already had to be re-anchored out of.
  //
  // What it protects is unchanged: zero must mean "nothing is wired", never
  // "the script->suite resolution broke". So it is measured against an
  // INDEPENDENT source of truth — root package.json's own scripts — rather
  // than against a number somebody has to remember to update.
  const rootScripts = Object.values(
    JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), '..', '..', 'package.json'), 'utf8')).scripts ?? {}
  )
  // A single test FILE, which is what this category derives. `bun test <dir>`
  // (the parked corpus) is not a wired suite and must not make this floor
  // demand an entry for it.
  const bunTestScripts = rootScripts.filter((command) => /^bun test\s+\S+\.test\.ts\b/.test(command))
  if (bunTestScripts.length === 0) {
    assert.equal(
      bunTestEntries.length,
      0,
      'no root package.json script runs `bun test <file>.test.ts`, so this category must derive EMPTY — a non-empty derivation here means the resolver invented a suite'
    )
  } else {
    assert.ok(
      bunTestEntries.length >= 1,
      `root package.json wires ${bunTestScripts.length} \`bun test\` script(s) but the category derived to zero — that is a broken resolution, not an empty tree`
    )
  }
  assert.ok(shellEntries.every((g) => g.invokedFrom && g.via), 'every shell-gate entry must name where it was found and the literal command')
  assert.ok(bunTestEntries.every((g) => g.invokedFrom && g.via && g.scriptName), 'every bun-test-suite entry must name where it was found, the literal command, and the package.json script name')
  assert.ok(testEntries.every((g) => g.invokedFrom === undefined), 'a test.mjs entry must never carry shell-gate/bun-test-suite-only fields')
})

test('CLI entrypoint prints DERIVED_GUARD_COUNT and exits 0 against the real tree', async () => {
  const { execFileSync } = await import('node:child_process')
  const { dirname, join: pathJoin } = await import('node:path')
  const { fileURLToPath } = await import('node:url')
  const here = dirname(fileURLToPath(import.meta.url))
  const cliPath = pathJoin(here, '..', 'guard-count.mjs')
  const output = execFileSync('node', [cliPath], { encoding: 'utf8' })
  assert.match(output, /^DERIVED_GUARD_COUNT:\d+$/m)
  const match = /^DERIVED_GUARD_COUNT:(\d+)$/m.exec(output)
  const printedCount = Number(match[1])
  const lines = output.trim().split('\n').slice(1)
  assert.equal(lines.length, printedCount, 'the printed count must match the number of listed files')
})
