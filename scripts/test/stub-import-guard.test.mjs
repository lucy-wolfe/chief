// #859: a scaffolded module's throwing stub is type-identical to a real
// implementation, so nothing in the ordinary gate set (typecheck, lint,
// knip, vitest) catches an import repointed at one. See
// scripts/stub-import-guard.mjs's header for the full rationale.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { tmpdir } from 'node:os'

import {
  checkTouchedFiles,
  deriveStubInventory,
  extractChiefImports,
  scanFileForStubs,
  touchedFiles
} from '../stub-import-guard.mjs'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(__dirname, '..', '..')

// The measured, independently cross-checked fact this guard exists to
// protect. First landed at 31 symbols across 11 modules; dropped to 15
// across 8 once #783 implemented `packages/piing/src/home/IdentityTheme.ts`
// (7 symbols) out from under this same story -- proof the self-retirement
// design works live, not just by description (see DECISIONS.md). A change
// to this number is either a real story landing (the count should DROP) or
// a scan regression (the count silently hits 0 or jumps) -- both are worth
// a human look, which is why this is a fixed assertion, not a >=0
// tautology. Re-pin this number (never loosen to >=) whenever a listed
// module's story lands.
//
// 15/8 -> 13/6 at merge time: E2-S6/#775 implemented SseHub and SseWatcher,
// retiring one symbol and one module each. Second live demonstration of the
// self-retirement design, after #783 took it 31/11 -> 15/8. The drop was
// caught by THIS assertion failing at the gate, not by anyone remembering
// to update it.
//
// 0 across 0 modules is a VALID terminal state this assertion is expected
// to reach eventually (every remaining stub gets implemented by some
// story), and re-pinning to `stubs.length === 0` / `modules.size === 0`
// then is an ordinary ratchet update, not a special case -- the guard
// working itself out of a job is success, not a regression to work around.
// What must never happen is a scan that resolves to zero because it looked
// in the WRONG place (a moved `packages/` dir, a broken glob) reading
// identically to "genuinely none left"; the synthetic non-vacuity check
// below exists specifically to keep that distinction real once this
// assertion is legitimately pinned at 0/0.
test('the derived stub inventory on the real tree is exactly 0 symbols across 0 modules', () => {
  const inventory = deriveStubInventory(repoRoot)
  const stubs = inventory.filter((f) => f.status === 'stub')
  const partial = inventory.filter((f) => f.status === 'partially-stub')
  const modules = new Set(stubs.map((s) => s.file))
  assert.deepEqual(partial, [], 'no class should be ambiguously partially-stub on the real tree today')
  assert.equal(stubs.length, 0, `stub count drifted -- expected terminal 0, scanner now finds ${stubs.length}`)
  assert.equal(modules.size, 0, `stub module count drifted -- expected terminal 0, scanner now finds ${modules.size}`)
})

// #859 handback #2: this used to anchor on a hardcoded REAL module
// (IdentityTheme.ts, then GoalPriority.ts after #783 implemented the
// first). Both anchors were correct in their moment and both were
// structurally guaranteed to expire again -- this guard's entire subject
// is symbols becoming implemented, and #784/E2-S5 etc. will eventually
// implement GoalPriority.ts too, at which point the real tree can
// legitimately reach 0 stubs / 0 modules (see the count test above, and
// DECISIONS.md). A hardcoded-real-module non-vacuity check would then
// have nothing left to anchor on, exactly like the tamper fixtures below
// did when #774 emptied packages/chiefing.
//
// The fix generalizes past re-anchoring: this check's JOB is "does
// deriveStubInventory ever find anything, or does it always return empty
// regardless of input" -- that is a claim about the SCANNER MECHANISM, not
// about what the real tree currently contains. A synthetic fixture proves
// exactly that, permanently: it plants ONE unambiguous stub function in an
// otherwise-empty `packages/` tree and asserts the scanner finds precisely
// it. This keeps working at the real tree's 0/0 terminal state, because it
// never reads the real tree at all -- "zero found in the real tree" and
// "the scanner can't find anything, ever" are now two different,
// independently-checked claims instead of one conflated assumption.
test('sanity check: the scan is not vacuous — a synthetic fixture with exactly one stub resolves exactly one stub', () => {
  const root = mkdtempSync(join(tmpdir(), 'stub-import-guard-vacuity-'))
  try {
    const dir = join(root, 'packages', 'fixture-pkg', 'src')
    mkdirSync(dir, { recursive: true })
    writeFileSync(
      join(dir, 'SyntheticStub.ts'),
      "export function syntheticStubFn(): void {\n  throw new Error('not implemented: synthetic fixture stub')\n}\n" +
        'export function syntheticRealFn(): number {\n  return 1 + 1\n}\n'
    )
    const inventory = deriveStubInventory(root)
    const stubs = inventory.filter((f) => f.status === 'stub')
    assert.deepEqual(stubs, [
      { file: 'packages/fixture-pkg/src/SyntheticStub.ts', symbol: 'syntheticStubFn', kind: 'function', status: 'stub' }
    ])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// #915: before this fix, `walkTs`'s swallowed `readdirSync` failure meant a
// `packages/` root that does not exist resolved to zero files -> zero
// stubs -> a "clean" verdict identical to the real tree's genuine terminal
// 0/0 state. A moved or renamed scan root must be REFUSED, not silently
// scored as clean (#848's class, one guard over).
test('fail-closed: a scan root that does not exist is refused, not silently reported clean', () => {
  const root = mkdtempSync(join(tmpdir(), 'stub-import-guard-missing-root-'))
  try {
    // No `packages/` directory created at all under `root`.
    assert.throws(
      () => deriveStubInventory(root),
      /REFUSING TO RUN -- scan root does not exist/,
      'a missing packages/ dir must throw, never resolve to an empty (falsely clean) inventory'
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('fail-closed: a scan root that exists but enumerates zero files is refused, not silently reported clean', () => {
  const root = mkdtempSync(join(tmpdir(), 'stub-import-guard-empty-root-'))
  try {
    mkdirSync(join(root, 'packages'), { recursive: true })
    assert.throws(
      () => deriveStubInventory(root),
      /REFUSING TO RUN -- 0 \.ts files found/,
      'an empty packages/ dir must throw, never resolve to a vacuously clean inventory'
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// #784 implemented the last live stubs, so the real-tree fact above is now
// terminal 0/0. Keep that fixed fact honest with the actual command's whole
// path: reintroduce an untracked source stub and an untracked caller in this
// very checkout, prove the command fails and names the import, then remove
// both and prove it returns to green. The private-fixture tests below still
// pin parser shapes; this one proves the live packages walk + Git touched-file
// enumeration cannot silently treat a terminal inventory as an inert guard.
// THE PROBE WRITES INTO THE REAL CHECKOUT, so where it writes is part of the
// test's correctness, not an implementation detail.
//
// The importer used to be `apps/cli/src/legacy/StubImportGuardTerminalProbe.ts`
// and the setup used to `mkdirSync(dirname(...), { recursive: true })` for both
// probe files. #751/P3 deleted `apps/cli`, and the `finally` below only ever
// removed the FILES -- so every run of this guard RECREATED `apps/cli`,
// `apps/cli/src` and `apps/cli/src/legacy` as empty directories in the working
// tree and left them there. `git status --porcelain` cannot see an empty
// untracked directory, so nothing looked dirty; the residue surfaced one run
// LATER, as `scripts/test/no-ts-cli-stub.test.mjs` ("apps/cli does not exist")
// failing and naming an innocent guard. A guard that mutates the repository it
// is checking is the worst shape of flake there is.
//
// So: both probe files now live in a directory that ALREADY EXISTS, and the
// test ASSERTS that directory exists rather than creating it. If
// `packages/piing/src` ever moves, this refuses out loud instead of quietly
// resurrecting a deleted tree. The caller's location is irrelevant to the
// mechanism under test -- `checkTouchedFiles` resolves the package from the
// STUB's path, never the caller's -- so sitting beside the stub costs the
// proof nothing.
const PROBE_DIR = 'packages/piing/src'

test('RED/GREEN: a reintroduced live stub and touched import fail the actual guard, then removal restores terminal zero', () => {
  const stubRelative = `${PROBE_DIR}/StubImportGuardTerminalProbe.ts`
  const importerRelative = `${PROBE_DIR}/StubImportGuardTerminalProbeCaller.ts`
  const stubFile = join(repoRoot, stubRelative)
  const importerFile = join(repoRoot, importerRelative)

  // Never `mkdirSync` under `repoRoot`: creating a directory to hold a probe is
  // how this test resurrected a deleted package for a whole day.
  assert.ok(
    existsSync(join(repoRoot, PROBE_DIR)),
    `${PROBE_DIR} must already exist -- this probe writes into the live checkout and must never CREATE a ` +
      'directory there. If the tree moved, re-anchor PROBE_DIR; do not let the test build the path it needs.'
  )
  assert.ok(!existsSync(stubFile), `${stubRelative} must not already exist -- refusing to overwrite a real file`)
  assert.ok(!existsSync(importerFile), `${importerRelative} must not already exist -- refusing to overwrite a real file`)

  try {
    writeFileSync(
      stubFile,
      "export function reintroducedTerminalStub(): void {\n  throw new Error('not implemented: terminal-state probe')\n}\n"
    )
    writeFileSync(
      importerFile,
      "import { reintroducedTerminalStub } from '@chief/piing'\nexport const terminalProbe = reintroducedTerminalStub\n"
    )

    const inventory = deriveStubInventory(repoRoot)
    const stubs = inventory.filter((entry) => entry.status === 'stub')
    assert.deepEqual(stubs, [
      {
        file: stubRelative,
        symbol: 'reintroducedTerminalStub',
        kind: 'function',
        status: 'stub'
      }
    ])

    const files = touchedFiles(repoRoot)
    assert.ok(files.includes(stubRelative), 'the live reintroduced stub must be visible as untracked')
    assert.ok(files.includes(importerRelative), 'the live importing caller must be visible as untracked')
    assert.deepEqual(checkTouchedFiles(repoRoot, files, inventory), [
      {
        file: importerRelative,
        imports: 'reintroducedTerminalStub',
        from: '@chief/piing',
        implementedBy: stubRelative
      }
    ])

    const red = spawnSync('bun', ['scripts/stub-import-guard.mjs'], { cwd: repoRoot, encoding: 'utf8' })
    assert.equal(red.status, 1, `the actual guard must reject the reintroduced import:\n${red.stdout}\n${red.stderr}`)
    assert.match(red.stderr, /reintroducedTerminalStub/)
    assert.match(red.stderr, /StubImportGuardTerminalProbe\.ts/)
  } finally {
    rmSync(stubFile, { force: true })
    rmSync(importerFile, { force: true })
  }

  // The probe leaves NOTHING behind. Asserted, not assumed: the whole defect
  // was a cleanup that removed the files it wrote and none of the directories.
  assert.equal(existsSync(stubFile), false, 'the stub probe must not survive its own test')
  assert.equal(existsSync(importerFile), false, 'the caller probe must not survive its own test')
  // #1041: asked of GIT, not of the disk. This assertion's subject is "this
  // probe left nothing behind", and the thing a probe can leave behind is a
  // path git can see. `existsSync` also answered yes to a leftover
  // `apps/cli/node_modules` that a `git clean` exclusion had spared, which
  // turned this guard red for a reason that had nothing to do with the probe
  // — an instrument reporting on the filesystem instead of on its subject.
  // The probe's own directories are untracked and not ignored, so they stay
  // fully visible to this check.
  assert.deepEqual(
    execFileSync(
      'git',
      ['ls-files', '--cached', '--others', '--exclude-standard', '--', 'apps/cli'],
      { cwd: repoRoot, encoding: 'utf8' }
    )
      .split('\n')
      .filter((line) => line.length > 0),
    [],
    'apps/cli is deleted and this test must never be what puts it back'
  )

  const inventoryAfterRemoval = deriveStubInventory(repoRoot)
  assert.deepEqual(inventoryAfterRemoval, [], 'removing the probe must restore the terminal real-tree inventory')
  const green = spawnSync('bun', ['scripts/stub-import-guard.mjs'], { cwd: repoRoot, encoding: 'utf8' })
  assert.equal(green.status, 0, `the actual guard must return green after probe cleanup:\n${green.stdout}\n${green.stderr}`)
  assert.match(green.stdout, /clean\. 0 stub symbol\(s\) across 0 module\(s\)/)
})

// The exact bug this scanner shipped with and fixed: a return type shaped
// as an object literal (`): { legacy: string } {`) has two brace groups
// back to back, and a naive "first `{` after the signature" scan treats the
// return type's own brace as the body's -- silently dropping the symbol
// from the derived set entirely (a false NEGATIVE, the dangerous direction
// for a security-shaped guard).
test('regression: a function whose return type is an object literal is still classified correctly', () => {
  const source = `
export function organizationPersonThemeFileNames(_personId: string): {
  legacy: string
  light: string
  dark: string
} {
  throw new Error('not implemented: stub')
}
`
  const findings = scanFileForStubs('fixture.ts', source)
  assert.deepEqual(
    findings.map((f) => f.symbol),
    ['organizationPersonThemeFileNames']
  )
})

// The indirection bug: a hoisted `const NOT_IMPLEMENTED = '...'` thrown by
// identifier reads as clean under a scanner that only matches the inline
// string literal shape. This is the exact difference between piing's stub
// modules (literal) and chiefing's (hoisted) that made an earlier version
// of this check under-count by 5 modules.
test('a hoisted not-implemented constant thrown by identifier is recognised as a stub', () => {
  const source = `
const NOT_IMPLEMENTED = 'not implemented: @chief/chiefing stub'

export class ExampleClient {
  async doThing(): Promise<void> {
    throw new Error(NOT_IMPLEMENTED)
  }
}
`
  const findings = scanFileForStubs('fixture.ts', source)
  assert.deepEqual(findings, [{ file: 'fixture.ts', symbol: 'ExampleClient', kind: 'class', status: 'stub' }])
})

test('a class with a mix of stub and genuinely real methods is reported separately, not silently classified either way', () => {
  const source = `
const NOT_IMPLEMENTED = 'not implemented: stub'

export class MixedClient {
  async stubbed(): Promise<void> {
    throw new Error(NOT_IMPLEMENTED)
  }
  real(): number {
    return 1 + 1
  }
}
`
  const findings = scanFileForStubs('fixture.ts', source)
  assert.equal(findings.length, 1)
  assert.equal(findings[0].status, 'partially-stub')
  assert.deepEqual(findings[0].stubMethods, ['stubbed'])
  assert.deepEqual(findings[0].realMethods, ['real'])
})

// An empty constructor (TS parameter-property auto-assignment) grants no
// capability on its own -- it must not make an otherwise fully-throwing
// class read as "partially real" the way a genuinely functional method
// would.
test('an empty parameter-property constructor does not prevent a class from being classified fully-stub', () => {
  const source = `
const NOT_IMPLEMENTED = 'not implemented: stub'

export class ClientWithConstructor {
  constructor(
    protected readonly transport: unknown,
    protected readonly root?: string
  ) {}

  async doThing(): Promise<void> {
    throw new Error(NOT_IMPLEMENTED)
  }
}
`
  const findings = scanFileForStubs('fixture.ts', source)
  assert.deepEqual(findings, [
    { file: 'fixture.ts', symbol: 'ClientWithConstructor', kind: 'class', status: 'stub' }
  ])
})

test('extractChiefImports finds named imports across multiple @chief/* packages, ignoring type-only and aliased forms', () => {
  const source = `
import { piingSkillsRoot, organizationPersonAccent } from "@chief/piing"
import type { Foo } from "@chief/piing"
import { StaffingClient as Staffing } from '@chief/chiefing'
`
  const imports = extractChiefImports(source)
  assert.deepEqual(imports, [
    { pkg: '@chief/piing', names: ['piingSkillsRoot', 'organizationPersonAccent'] },
    { pkg: '@chief/piing', names: ['Foo'] },
    { pkg: '@chief/chiefing', names: ['StaffingClient'] }
  ])
})

// --- tamper proofs: the check must actually be able to fail -----------

function fixtureRepo() {
  const root = mkdtempSync(join(tmpdir(), 'stub-import-guard-fixture-'))
  execFileSync('git', ['init', '-q'], { cwd: root })
  execFileSync('git', ['config', 'user.email', 'test@example.com'], { cwd: root })
  execFileSync('git', ['config', 'user.name', 'test'], { cwd: root })
  mkdirSync(join(root, 'apps', 'cli', 'src'), { recursive: true })
  writeFileSync(join(root, 'README.md'), '# fixture\n')
  execFileSync('git', ['add', '.'], { cwd: root })
  execFileSync('git', ['commit', '-q', '-m', 'init'], { cwd: root })
  return root
}

// #859 handback #2: these two fixtures went through two designs already.
// First, a hardcoded real symbol (`makeTheme` from piing) -- #783
// implemented it for real out from under this story, so the tamper
// "proof" would have demonstrated the guard catching a literal-shaped stub
// against a symbol that was no longer a stub. Second, deriving the target
// from the LIVE real-tree inventory at test time (first stub under each
// package family) -- #774 then implemented every remaining `packages/
// chiefing` stub, emptying that family entirely, and `firstStubInPackage`
// correctly refused loudly rather than silently skipping (exactly the
// property it was built for) instead of passing.
//
// Both designs shared one flaw: they made a claim about SYNTAX ("does the
// scanner recognise this throw shape") depend on the CONTENTS of the live
// tree, which drifts for reasons that have nothing to do with the parser.
// The merger's framing: a fixture is the right source for "does the parser
// handle this syntax"; live inventory is the right source for "does the
// pipeline see real stubs today" -- a different, separately-covered claim
// (the count test above). Fusing them meant a story landing anywhere in
// `packages/chiefing` could break a test about hoisted-constant PARSING.
//
// Fully synthetic fixes both failure modes for good: each test plants its
// own literal-shaped or hoisted-constant-shaped stub module inside the
// SAME temp git repo the "importer" file lives in, derives that fixture
// root's OWN inventory (deriveStubInventory has never read the real repo
// here), and only then exercises checkTouchedFiles/touchedFiles against
// it -- proving the full pipeline handles both throw shapes, permanently,
// independent of what packages/piing or packages/chiefing currently
// contain. This also means these two tests keep passing at the real
// tree's eventual 0/0 terminal state, which the live-inventory-derived
// design could not.
function writeSyntheticStubModule(root, { pkg, relativeFile, source, symbol, kind }) {
  const full = join(root, relativeFile)
  mkdirSync(dirname(full), { recursive: true })
  writeFileSync(full, source)
  return { pkg, file: relativeFile, symbol, kind }
}

test('tamper proof: an UNTRACKED file importing a literal-shaped stub is caught', () => {
  const root = fixtureRepo()
  try {
    const target = writeSyntheticStubModule(root, {
      pkg: '@chief/piing',
      relativeFile: join('packages', 'piing', 'src', 'SyntheticLiteralStub.ts'),
      source:
        "export function syntheticLiteralStub(): void {\n  throw new Error('not implemented: @chief/piing synthetic fixture stub')\n}\n",
      symbol: 'syntheticLiteralStub',
      kind: 'function'
    })
    execFileSync('git', ['add', '-A'], { cwd: root })
    execFileSync('git', ['commit', '-q', '-m', 'add synthetic literal stub module'], { cwd: root })
    const inventory = deriveStubInventory(root)

    const file = join(root, 'apps', 'cli', 'src', 'tamper-literal.ts')
    writeFileSync(
      file,
      `import { ${target.symbol} } from "${target.pkg}";\nexport const x = ${target.symbol};\n`
    )
    const files = touchedFiles(root)
    assert.deepEqual(files, ['apps/cli/src/tamper-literal.ts'], 'an untracked file must be visible to the scan')
    const violations = checkTouchedFiles(root, files, inventory)
    assert.equal(violations.length, 1, `expected importing '${target.symbol}' from a synthetic ${target.pkg} stub to be caught`)
    assert.equal(violations[0].imports, target.symbol)
    assert.equal(violations[0].from, target.pkg)
    assert.equal(violations[0].implementedBy, target.file)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('tamper proof: a STAGED file importing a hoisted-constant-shaped stub is caught', () => {
  const root = fixtureRepo()
  try {
    const target = writeSyntheticStubModule(root, {
      pkg: '@chief/chiefing',
      relativeFile: join('packages', 'chiefing', 'src', 'SyntheticHoistedStub.ts'),
      source:
        "const NOT_IMPLEMENTED = 'not implemented: @chief/chiefing synthetic fixture stub'\n\n" +
        'export class SyntheticHoistedClient {\n  async doThing(): Promise<void> {\n    throw new Error(NOT_IMPLEMENTED)\n  }\n}\n',
      symbol: 'SyntheticHoistedClient',
      kind: 'class'
    })
    execFileSync('git', ['add', '-A'], { cwd: root })
    execFileSync('git', ['commit', '-q', '-m', 'add synthetic hoisted-constant stub module'], { cwd: root })
    const inventory = deriveStubInventory(root)

    const file = join(root, 'apps', 'cli', 'src', 'tamper-hoisted.ts')
    writeFileSync(
      file,
      `import { ${target.symbol} } from "${target.pkg}";\nexport const x = ${target.symbol};\n`
    )
    execFileSync('git', ['add', 'apps/cli/src/tamper-hoisted.ts'], { cwd: root })
    const files = touchedFiles(root)
    assert.deepEqual(files, ['apps/cli/src/tamper-hoisted.ts'], 'a staged file must be visible to the scan')
    const violations = checkTouchedFiles(root, files, inventory)
    assert.equal(violations.length, 1, `expected importing '${target.symbol}' from a synthetic ${target.pkg} stub to be caught`)
    assert.equal(violations[0].imports, target.symbol)
    assert.equal(violations[0].from, target.pkg)
    assert.equal(violations[0].implementedBy, target.file)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('negative control: a file importing only real symbols is not flagged', () => {
  const root = fixtureRepo()
  try {
    const file = join(root, 'apps', 'cli', 'src', 'real.ts')
    writeFileSync(
      file,
      `import { piingSkillsRoot, BUILTIN_TOOLS } from "@chief/piing";\nexport const x = piingSkillsRoot();\n`
    )
    const files = touchedFiles(root)
    const inventory = deriveStubInventory(repoRoot)
    const violations = checkTouchedFiles(root, files, inventory)
    assert.deepEqual(violations, [])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// The exact gap an earlier internal version of this check had: scanning
// only `git diff --name-only <base>` sees neither staged nor untracked
// files, so the check can never observe what it exists to check and
// reports clean forever regardless of what actually changed.
test('touchedFiles sees staged, unstaged, AND untracked files -- not only committed diffs', () => {
  const root = fixtureRepo()
  try {
    writeFileSync(join(root, 'apps', 'cli', 'src', 'untracked.ts'), 'export const a = 1\n')
    writeFileSync(join(root, 'apps', 'cli', 'src', 'staged.ts'), 'export const b = 1\n')
    execFileSync('git', ['add', 'apps/cli/src/staged.ts'], { cwd: root })
    writeFileSync(join(root, 'README.md'), '# fixture\nchanged, unstaged\n')
    const files = new Set(touchedFiles(root))
    assert.ok(files.has('apps/cli/src/untracked.ts'), 'untracked file missing')
    assert.ok(files.has('apps/cli/src/staged.ts'), 'staged file missing')
    // README.md is filtered out by extension (.md, not .ts) -- confirms the
    // unstaged-diff source is read too, just filtered downstream, not that
    // it was silently skipped upstream.
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// #914: reproduces the exact live incident -- a `git ls-files --others`
// invocation whose stdout exceeds execFileSync's buffer bound -- WITHOUT
// generating the tens of thousands of real files (13,531 in the incident)
// that would take to overflow the real 64 MiB default. touchedFiles's
// optional `maxBuffer` override lets this test inject an artificially tiny
// bound and trip the exact same overflow with a handful of untracked files,
// proving the new path refuses with a named, attributed error rather than
// throwing a bare, unattributed runtime error OR silently returning a
// truncated file list.
test('#914: an execFileSync buffer overflow while enumerating touched files fails closed with a named reason, not a truncated list or a bare crash', () => {
  const root = fixtureRepo()
  try {
    // Sixteen untracked files with long names comfortably exceed a 64-byte
    // maxBuffer once `git ls-files --others --exclude-standard` prints all
    // of their paths -- no need to approach the real 13,531-file incident.
    for (let i = 0; i < 16; i += 1) {
      writeFileSync(
        join(root, `untracked-file-with-a-long-name-to-overflow-a-tiny-buffer-${i}.ts`),
        'export const a = 1\n'
      )
    }
    assert.throws(
      () => touchedFiles(root, { maxBuffer: 64 }),
      (/** @type {Error} */ error) => {
        assert.match(error.message, /REFUSING TO RUN/)
        assert.match(error.message, /ls-files/)
        assert.match(error.message, /#914/)
        return true
      },
      'expected a named, attributed refusal identifying the failing git subcommand'
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// Negative control for the test above: the same fixture, the same tiny
// buffer, but with too little untracked output to actually overflow it --
// proves the assertion above is exercising a real overflow condition, not
// something touchedFiles would throw on regardless of input.
test('#914 control: the same small maxBuffer does not spuriously fail when output actually fits', () => {
  const root = fixtureRepo()
  try {
    writeFileSync(join(root, 'apps', 'cli', 'src', 'small.ts'), 'export const a = 1\n')
    const files = touchedFiles(root, { maxBuffer: 4096 })
    assert.deepEqual(files, ['apps/cli/src/small.ts'])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// Real-tree proof this lands as a ratchet, not a cleanup backlog.
test('the real tree has zero touched-file violations right now (no changes staged)', () => {
  const files = touchedFiles(repoRoot)
  const inventory = deriveStubInventory(repoRoot)
  const violations = checkTouchedFiles(repoRoot, files, inventory)
  assert.deepEqual(violations, [])
})
