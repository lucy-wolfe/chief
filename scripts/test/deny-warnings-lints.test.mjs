// #984: the chiefd workspace denies every rustc warning, in COMMITTED
// MANIFEST STATE, and no crate quietly opts back out.
//
// The user's requirement was "warnings must be impossible to ignore". The
// mechanism is `[workspace.lints.rust] warnings = "deny"` in
// `apps/chiefd/Cargo.toml`, inherited by every member through
// `[lints] workspace = true`. The mechanism that was explicitly REJECTED is
// `RUSTFLAGS="-D warnings"` in a shell profile / CI env var / build script,
// because that is machine-local: green for whoever set it, absent on a clean
// clone. This repo has already paid for that class more than once (a lockfile
// naming a deleted workspace; a guard demanding README text that no longer
// existed; a release script importing a deleted module — every one fine on a
// warm checkout and broken on a fresh one).
//
// So the property under test is not "warnings are denied somewhere", it is
// "warnings are denied in a file every clone and every CI run reads
// identically, and every crate is actually covered by it". A future crate that
// lands without `[lints] workspace = true`, or that overrides `warnings` back
// down in its own table, or that plants `#![allow(warnings)]` at its crate
// root, is a silent opt-out that no build failure would report — this fails
// instead.
//
// Shape follows the other guards here (#873's zero-false-positive standard,
// #848's non-vacuity lesson):
//   1. The REAL tree passes today, and the check states what it enumerated.
//   2. A non-vacuity floor: an empty scan FAILS rather than passes.
//   3. SYNTHETIC fixtures (never the real tree) demonstrate red, then green,
//      for each way the denial can be undone.
//
// Run with `node --test scripts/test/deny-warnings-lints.test.mjs`.

import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import { blanketAllowsIn, checkDenyWarnings, inheritsWorkspaceLints, parseLintTable } from '../deny-warnings-lints-lib.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const chiefdRoot = join(repoRoot, 'apps', 'chiefd')

// ---- 1. The real tree, today -------------------------------------------

test('the chiefd workspace denies rustc warnings in committed manifest state', () => {
  const result = checkDenyWarnings(chiefdRoot)
  assert.equal(
    result.workspaceRustLints.warnings,
    'deny',
    'apps/chiefd/Cargo.toml [workspace.lints.rust] must carry `warnings = "deny"` — ' +
      'the denial lives in the manifest on purpose, never in RUSTFLAGS or a CI env var'
  )
  // The pre-existing denies this packet must not have displaced.
  assert.equal(result.workspaceRustLints.unsafe_code, 'deny')
  // `missing_docs` must be spelled out, not left to the group — see below.
  assert.equal(result.workspaceRustLints.missing_docs, 'deny')
  assert.equal(result.workspaceClippyLints.unwrap_used, 'deny')
  assert.equal(result.workspaceClippyLints.expect_used, 'deny')
  assert.equal(result.workspaceClippyLints.panic, 'deny')
})

// The subtle one. `cargo check -v` emits `--deny=warnings … --warn=missing_docs`
// — the GROUP flag first — and rustc lets the later, more specific flag win.
// So a row left at `"warn"` beside `warnings = "deny"` is enforced by nothing
// while reading as though it were enforced, which is strictly worse than not
// listing the lint at all. That is precisely how `launcher_assets` went
// undocumented on main under a manifest that appeared to deny missing docs.
test('no [workspace.lints.rust] row is left at "warn", where the group deny cannot reach it', () => {
  const result = checkDenyWarnings(chiefdRoot)
  assert.deepEqual(
    result.inertWarnRows,
    [],
    `lint rows at "warn" survive \`warnings = "deny"\` and enforce nothing: ${result.inertWarnRows.join(', ')} — ` +
      'spell the level "deny" (or "allow" if the exception is deliberate)'
  )
})

test('every workspace member inherits the workspace lint table', () => {
  const result = checkDenyWarnings(chiefdRoot)
  assert.deepEqual(
    result.notInheriting,
    [],
    `members missing \`[lints] workspace = true\`: ${result.notInheriting.join(', ')}`
  )
  assert.deepEqual(
    result.overriding,
    [],
    `members overriding the inherited warnings level: ${result.overriding.join(', ')}`
  )
})

test('no chiefd crate root switches the denial back off wholesale', () => {
  const result = checkDenyWarnings(chiefdRoot)
  assert.deepEqual(
    result.blanketAllows,
    [],
    `crate roots with a blanket #![allow(...)]: ${result.blanketAllows
      .map((entry) => `${entry.file} -> ${entry.allows.join('/')}`)
      .join(', ')}`
  )
})

test('the real-tree scan is not vacuous — an empty enumeration fails rather than passes', () => {
  const result = checkDenyWarnings(chiefdRoot)
  assert.ok(result.members.length >= 6, `expected at least 6 workspace members, got ${result.members.length}`)
  assert.equal(
    result.inheriting.length,
    result.members.length,
    'every parsed member must have been resolved to a real manifest that inherits'
  )
  assert.ok(
    result.crateRootsScanned.length >= 5,
    `expected to have read at least 5 crate-root sources, got ${result.crateRootsScanned.length}`
  )
  // Named explicitly, not merely counted: a reviewer can see the scan reached
  // real, specific packages rather than an empty or wrong directory.
  for (const expected of [
    'crates/beacond',
    'crates/chiefd-core',
    'crates/chiefd-host',
    'crates/chiefd-api',
    'crates/chiefd-daemon',
    'crates/chief-cli',
    'tests/unit-d',
  ]) {
    assert.ok(result.inheriting.includes(expected), `expected ${expected} to inherit the workspace lints`)
  }
})

// ---- 2. Synthetic fixtures: demonstrated red, then green -----------------

function plantWorkspace(root, { workspaceRustBody, members }) {
  writeFileSync(
    join(root, 'Cargo.toml'),
    `[workspace]\nresolver = "2"\nmembers = [\n${members
      .map((member) => `    "${member.path}",`)
      .join('\n')}\n]\n\n[workspace.lints.rust]\n${workspaceRustBody}\n`
  )
  for (const member of members) {
    const dir = join(root, member.path)
    mkdirSync(join(dir, 'src'), { recursive: true })
    writeFileSync(join(dir, 'Cargo.toml'), `[package]\nname = "fixture"\nversion = "0.0.0"\n${member.manifestTail ?? '\n[lints]\nworkspace = true\n'}`)
    writeFileSync(join(dir, 'src', 'lib.rs'), member.libSource ?? '')
  }
}

function fixtureRoot() {
  return mkdtempSync(join(tmpdir(), 'deny-warnings-lints-fixture-'))
}

test('a fixture workspace that does not deny warnings is caught', () => {
  const root = fixtureRoot()
  try {
    plantWorkspace(root, {
      workspaceRustBody: 'unsafe_code = "deny"\n',
      members: [{ path: 'crates/a' }],
    })
    const result = checkDenyWarnings(root)
    assert.equal(result.workspaceRustLints.warnings, undefined, 'no warnings deny must be visible')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('the same fixture passes once the workspace denies warnings', () => {
  const root = fixtureRoot()
  try {
    plantWorkspace(root, {
      workspaceRustBody: 'warnings = "deny"\nunsafe_code = "deny"\n',
      members: [{ path: 'crates/a' }],
    })
    const result = checkDenyWarnings(root)
    assert.equal(result.workspaceRustLints.warnings, 'deny')
    assert.deepEqual(result.notInheriting, [])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('a fixture workspace that leaves a lint at "warn" beside the group deny is caught, and "allow" is not', () => {
  const root = fixtureRoot()
  try {
    plantWorkspace(root, {
      workspaceRustBody: 'warnings = "deny"\nmissing_docs = "warn"\nunsafe_code = "deny"\nsome_deliberate_exception = "allow"\n',
      members: [{ path: 'crates/a' }],
    })
    const result = checkDenyWarnings(root)
    assert.deepEqual(result.inertWarnRows, ['missing_docs'])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('the same fixture passes once that row is spelled "deny"', () => {
  const root = fixtureRoot()
  try {
    plantWorkspace(root, {
      workspaceRustBody: 'warnings = "deny"\nmissing_docs = "deny"\nunsafe_code = "deny"\n',
      members: [{ path: 'crates/a' }],
    })
    assert.deepEqual(checkDenyWarnings(root).inertWarnRows, [])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('a fixture member with no [lints] table is named as not inheriting', () => {
  const root = fixtureRoot()
  try {
    plantWorkspace(root, {
      workspaceRustBody: 'warnings = "deny"\n',
      members: [{ path: 'crates/a' }, { path: 'crates/opted-out', manifestTail: '\n' }],
    })
    const result = checkDenyWarnings(root)
    assert.deepEqual(result.notInheriting, ['crates/opted-out'])
    assert.deepEqual(result.inheriting, ['crates/a'])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('a fixture member that downgrades warnings in its own [lints.rust] is named as overriding', () => {
  const root = fixtureRoot()
  try {
    plantWorkspace(root, {
      workspaceRustBody: 'warnings = "deny"\n',
      members: [
        { path: 'crates/a' },
        {
          path: 'crates/sneaky',
          manifestTail: '\n[lints]\nworkspace = true\n\n[lints.rust]\nwarnings = "allow"\n',
        },
      ],
    })
    const result = checkDenyWarnings(root)
    assert.deepEqual(result.overriding, ['crates/sneaky (warnings = "allow")'])
    // …and it still counts as inheriting, so the two failures are reported
    // independently rather than one masking the other.
    assert.ok(result.inheriting.includes('crates/sneaky'))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('a fixture crate root with #![allow(warnings)] is named, and a targeted allow is not', () => {
  const root = fixtureRoot()
  try {
    plantWorkspace(root, {
      workspaceRustBody: 'warnings = "deny"\n',
      members: [
        { path: 'crates/a', libSource: '#![allow(clippy::too_many_lines)] // targeted, with a reason\n' },
        { path: 'crates/blanket', libSource: '#![allow(warnings)]\n' },
      ],
    })
    const result = checkDenyWarnings(root)
    assert.deepEqual(result.blanketAllows, [{ file: 'crates/blanket/src/lib.rs', allows: ['warnings'] }])
    assert.deepEqual(result.crateRootsScanned, ['crates/a/src/lib.rs', 'crates/blanket/src/lib.rs'])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// ---- 3. The parsers themselves, against their known hazards -------------

test('parseLintTable reads plain and table-valued lint levels, and stops at the next section', () => {
  const text =
    '[workspace.lints.rust]\nwarnings = "deny"\nmissing_docs = "warn"\nrust_2018_idioms = { level = "deny", priority = -1 }\n\n[profile.release]\nopt-level = 3\n'
  assert.deepEqual(parseLintTable(text, 'workspace.lints.rust'), {
    warnings: 'deny',
    missing_docs: 'warn',
    rust_2018_idioms: 'deny',
  })
  assert.deepEqual(parseLintTable(text, 'workspace.lints.clippy'), {})
})

test('parseLintTable ignores a commented-out lint row', () => {
  const text = '[workspace.lints.rust]\n# warnings = "allow"  <- deliberately not live\nwarnings = "deny"\n'
  assert.deepEqual(parseLintTable(text, 'workspace.lints.rust'), { warnings: 'deny' })
})

test('inheritsWorkspaceLints requires the [lints] table, not a mention of the words', () => {
  assert.equal(inheritsWorkspaceLints('[lints]\nworkspace = true\n'), true)
  assert.equal(inheritsWorkspaceLints('[package]\nname = "x"\n# lints workspace = true one day\n'), false)
  assert.equal(inheritsWorkspaceLints('[lints]\nworkspace = false\n'), false)
})

test('blanketAllowsIn only reports the broad groups, and only as an inner attribute', () => {
  assert.deepEqual(blanketAllowsIn('#![allow(dead_code, unused)]\n'), ['dead_code', 'unused'])
  assert.deepEqual(blanketAllowsIn('#![allow(clippy::pedantic)]\n'), [])
  // An ITEM-level allow is a local judgement call, not the crate switching the
  // denial off — this guard is deliberately not in that business.
  assert.deepEqual(blanketAllowsIn('#[allow(dead_code)]\nfn helper() {}\n'), [])
})
