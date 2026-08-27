// #887: every directory under `apps/chiefd/crates/` and `apps/chiefd/tests/`
// carrying its own `Cargo.toml` must be a real workspace member OR
// explicitly excluded in `apps/chiefd/Cargo.toml` — a positive statement,
// never an omission. Run with `node --test
// scripts/test/chiefd-workspace-membership.test.mjs`.
//
// #871's own shape, stated so the next reader doesn't have to reconstruct
// why this exists: `apps/chiefd/tests/unit-d` sat on disk with twenty test
// functions, absent from `members`, absent from `exclude`, never compiled
// by any Rust command — found only by a count disagreeing with a count (11
// `#[ignore]` attributes in the tree vs. 3 a workspace run reported), which
// is luck dressed as method. This guard makes the same class fail loudly
// instead: a new crate or test directory added without a `members` entry
// is caught the moment this runs, not discovered by chance months later.
//
// Two tests, matching #873's zero-false-positive standard and #848's
// non-vacuity lesson:
//   1. The REAL tree passes today, and the check states what it actually
//      enumerated (not just true/false) — proof the scan isn't vacuous.
//   2. A SYNTHETIC fixture (never the real tree) demonstrates red then
//      green: an unlisted `Cargo.toml` directory fails, naming itself;
//      removing it (or listing it) passes.

import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import { checkWorkspaceMembership, parseTomlStringArray } from '../chiefd-workspace-membership-lib.mjs'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(__dirname, '..', '..')
const chiefdRoot = join(repoRoot, 'apps', 'chiefd')

// ---- 1. The real tree, today ------------------------------------------

test('every real apps/chiefd crate/test directory is a listed member or explicitly excluded', () => {
  const result = checkWorkspaceMembership(chiefdRoot)
  assert.deepEqual(
    result.unlisted,
    [],
    `unlisted (neither in members nor exclude): ${result.unlisted.join(', ')}`
  )
})

test('the real-tree scan is not vacuous — it actually enumerates directories, members, and excludes', () => {
  const result = checkWorkspaceMembership(chiefdRoot)
  assert.ok(result.found.length > 0, 'found zero Cargo.toml-bearing directories — the scan itself is broken')
  assert.ok(result.members.length > 0, 'found zero members — the members array parse is broken')
  assert.ok(result.excluded.length > 0, 'found zero excludes — the exclude array parse is broken (tests/seam-fixture is expected)')
  // Every crate this repo is known to carry today, named explicitly rather
  // than just asserting a count — a reviewer can see at a glance that the
  // scan reached real, specific packages, not an empty or wrong directory.
  // `tests/e2e` was in this list until the chiefd-e2e crate was deleted with
  // the rest of the E2E corpus — a deliberate removal, so the name comes out
  // rather than the assertion being softened. `crates/beacond` and
  // `crates/chiefd-api` are named in its place: the point of listing crates
  // explicitly is that a reviewer can see the scan reached real, specific
  // packages, and that only holds if the list tracks the real membership.
  for (const expected of [
    'crates/beacond',
    'crates/chiefd-core',
    'crates/chiefd-host',
    'crates/chiefd-api',
    'crates/chiefd-daemon',
    'crates/chief-cli',
    // The leaf both sides of the client boundary link: the named pause points
    // and the diagnostic redactor both actuators run, where neither may depend
    // on the other.
    'crates/host-primitives',
    'tests/unit-d',
  ]) {
    assert.ok(result.found.includes(expected), `expected to find ${expected} on disk`)
    assert.ok(result.members.includes(expected), `expected ${expected} to be a listed member`)
  }
  assert.ok(result.excluded.includes('tests/seam-fixture'), 'expected tests/seam-fixture in exclude')
})

// ---- 2. Synthetic fixture: demonstrated red, then green -----------------

function writeFixtureManifest(fixtureRoot, { members, exclude }) {
  const membersBlock = members.map((entry) => `    "${entry}",`).join('\n')
  const excludeList = exclude.map((entry) => `"${entry}"`).join(', ')
  writeFileSync(
    join(fixtureRoot, 'Cargo.toml'),
    `[workspace]\nresolver = "2"\nmembers = [\n${membersBlock}\n]\nexclude = [${excludeList}]\n`
  )
}

function plantPackage(fixtureRoot, relativePath) {
  const dir = join(fixtureRoot, relativePath)
  mkdirSync(dir, { recursive: true })
  writeFileSync(join(dir, 'Cargo.toml'), '[package]\nname = "fixture"\nversion = "0.0.0"\n')
}

test('a fixture directory with a Cargo.toml but no members/exclude entry fails, naming itself', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'chiefd-workspace-membership-fixture-'))
  try {
    plantPackage(fixtureRoot, 'crates/known-crate')
    plantPackage(fixtureRoot, 'crates/rogue-crate') // the unlisted one
    writeFixtureManifest(fixtureRoot, { members: ['crates/known-crate'], exclude: [] })

    const result = checkWorkspaceMembership(fixtureRoot)
    assert.deepEqual(result.unlisted, ['crates/rogue-crate'], 'the rogue crate must be named, and be the ONLY one named')
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

test('the same fixture passes once the rogue directory is listed in members', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'chiefd-workspace-membership-fixture-'))
  try {
    plantPackage(fixtureRoot, 'crates/known-crate')
    plantPackage(fixtureRoot, 'crates/rogue-crate')
    writeFixtureManifest(fixtureRoot, { members: ['crates/known-crate', 'crates/rogue-crate'], exclude: [] })

    const result = checkWorkspaceMembership(fixtureRoot)
    assert.deepEqual(result.unlisted, [])
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

test('the same fixture also passes if the rogue directory is explicitly excluded instead', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'chiefd-workspace-membership-fixture-'))
  try {
    plantPackage(fixtureRoot, 'crates/known-crate')
    plantPackage(fixtureRoot, 'crates/rogue-crate')
    writeFixtureManifest(fixtureRoot, {
      members: ['crates/known-crate'],
      exclude: ['crates/rogue-crate'],
    })

    const result = checkWorkspaceMembership(fixtureRoot)
    assert.deepEqual(result.unlisted, [])
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

test('a directory with no Cargo.toml of its own is invisible to the scan — not every subdirectory is a package', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'chiefd-workspace-membership-fixture-'))
  try {
    plantPackage(fixtureRoot, 'crates/known-crate')
    // A plain directory, no Cargo.toml — e.g. a fixtures/ or docs/ dir some
    // crate keeps alongside its own sources. Must not be flagged.
    mkdirSync(join(fixtureRoot, 'crates', 'not-a-package'), { recursive: true })
    writeFixtureManifest(fixtureRoot, { members: ['crates/known-crate'], exclude: [] })

    const result = checkWorkspaceMembership(fixtureRoot)
    assert.deepEqual(result.found, ['crates/known-crate'])
    assert.deepEqual(result.unlisted, [])
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

// ---- 3. The parser itself, against the two hazards its own doc names ----

test('the array parser skips a comment line naming a quoted path that is not actually a member', () => {
  const text = `[workspace]\nmembers = [\n    "crates/real",\n    # "crates/commented-out" is NOT live, only mentioned\n]\nexclude = []\n`
  assert.deepEqual(parseTomlStringArray(text, 'members'), ['crates/real'])
})

test('the array parser skips an inline trailing comment on a real entry without losing the entry', () => {
  const text = `[workspace]\nmembers = [\n    "crates/real", # kept for now, see #123\n]\nexclude = []\n`
  assert.deepEqual(parseTomlStringArray(text, 'members'), ['crates/real'])
})
