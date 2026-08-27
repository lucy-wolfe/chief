// GUARD: every detached spawn in this tree is triaged, and the triage is true.
//
// The rule, the detectors and the register live in
// `scripts/orphanable-spawner-lib.mjs`; this file is the assertion half.
//
// WHY IT IS HERE AND NOT WHERE IT WAS
// -----------------------------------
// This check existed for months as `scripts/orphanable-spawner-scan.ts` plus an
// allowlist, and NOTHING INVOKED IT. Its only test sat in the parked `tests/`
// corpus, which runs in no lane. The first run of the ported scan against the
// real tree found THIRTEEN untriaged detached spawn sites against a register of
// five rows — including two test-owned `beacond` daemons with no child-side
// self-kill at all, which is precisely the shape #987 measured as an
// eight-to-twelve-hour orphan on a shared build host. None of that was a new
// defect. All of it had been true, and unreportable, for as long as the scanner
// was dark.
//
// Arms, in the order this repo has learned to need them:
//
//   1. THE REAL TREE — one `deepEqual` per direction, so the diff is the report.
//   2. ROW SHAPE — a row that cannot be checked is not a fact, and neither is a
//      watchdog vocabulary entry no row claims.
//   3. NON-VACUITY — a clean answer from a scan that read nothing is not
//      evidence. Floors on files read, roots that still match, and sites seen.
//   4. DEMONSTRATED RED — every detector, both register directions and the
//      watchdog-claim arm, fired against a fixture tree.
//
// Fixtures are written under `mkdtemp`, never into the checkout: this suite has
// to leave the working tree byte-identical (`ci-guard-shard`).
//
// Run with `node --test scripts/test/orphanable-spawner.test.mjs`.

import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import {
  DETACH_MARKERS,
  ORPHAN_SPAWNER_ALLOWLIST,
  WATCHDOG_TOKENS,
  allowlistRowsNamingMissingFiles,
  allowlistShapeViolations,
  compareSitesToAllowlist,
  findSpawnSites,
  rowKey,
  scanVacuity,
  scannedFiles,
  unbackedWatchdogClaims,
} from '../orphanable-spawner-lib.mjs'

const REPO_ROOT = fileURLToPath(new URL('../..', import.meta.url))

/** Scan the real checkout once; every arm below reads this. */
const FILES = scannedFiles(REPO_ROOT)
const SITES = findSpawnSites(REPO_ROOT, FILES)

/** A throwaway tree with the given files, outside the checkout. */
function fixture(files) {
  const root = mkdtempSync(join(tmpdir(), 'orphanable-spawner-'))
  for (const [path, contents] of Object.entries(files)) {
    const absolute = join(root, path)
    mkdirSync(dirname(absolute), { recursive: true })
    writeFileSync(absolute, contents)
  }
  return root
}

// --- 1. the real tree --------------------------------------------------------

test('every detached spawn site is triaged, and every row still matches a site', () => {
  assert.deepEqual(compareSitesToAllowlist(SITES), { untriaged: [], stale: [] })
})

test('no row claims a watchdog its file does not contain', () => {
  assert.deepEqual(unbackedWatchdogClaims(REPO_ROOT), [])
})

test('no row names a path this tree does not have', () => {
  assert.deepEqual(allowlistRowsNamingMissingFiles(REPO_ROOT), [])
})

// --- 2. row shape ------------------------------------------------------------

test('every row is a checkable fact, and every watchdog token is claimed', () => {
  assert.deepEqual(allowlistShapeViolations(), [])
})

// --- 3. non-vacuity ----------------------------------------------------------

test('the scan is not vacuous: it read the tree, every root still matches, and it can still see', () => {
  assert.deepEqual(scanVacuity(REPO_ROOT, FILES, SITES), [])
  assert.ok(DETACH_MARKERS.length >= 7, 'the detector set collapsed')
  assert.ok(
    SITES.some((site) => site.file.endsWith('.rs')),
    'the Rust detector must be seeing the crates',
  )
  assert.ok(
    SITES.some((site) => site.file.endsWith('.sh')),
    'the shell detectors must be seeing the scripts',
  )
})

// --- 4. demonstrated red -----------------------------------------------------

test('DEMONSTRATED RED: every detector fires against a fixture tree, naming file, line and marker', () => {
  const root = fixture({
    'packages/pkg/src/Daemon.ts': "const child = spawn(bin, [], {\n  detached: true,\n})\n",
    'apps/crate/src/spawn.rs': 'fn go() {\n    command.process_group(0);\n}\n',
    'scripts/a-nohup.sh': 'nohup "$START" >/dev/null 2>&1\n',
    'scripts/b-setsid.sh': 'setsid env FOO=1 chiefd run\n',
    'scripts/c-disown.sh': 'long_running_thing\ndisown\n',
    'scripts/d-background.sh': '( while :; do sleep 1; done ) &\n',
    'scripts/e-tmux.sh': 'tmux new-session -d -s company\n',
  })
  try {
    const sites = findSpawnSites(root)
    const fired = new Set(sites.map((site) => site.marker))
    for (const marker of DETACH_MARKERS) {
      assert.ok(fired.has(marker.id), `detector "${marker.id}" (${marker.what}) never fired: ${JSON.stringify(sites)}`)
    }
    const detachedTs = sites.find((site) => site.file === 'packages/pkg/src/Daemon.ts')
    assert.equal(detachedTs.line, 2, 'the line must be the line, so a reader can go there')

    // With an empty register, every one of them is reported as untriaged --
    // which is exactly what a new detached spawn looks like.
    const { untriaged, stale } = compareSitesToAllowlist(sites, [])
    assert.equal(untriaged.length, sites.length)
    assert.deepEqual(stale, [])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('DEMONSTRATED RED: a foreground spawn, a comment and a quoted assertion are all silent', () => {
  const root = fixture({
    // Foreground: the parent blocks, so the child cannot outlive it.
    'packages/pkg/src/Sync.ts': "const result = spawnSync(bin, ['--version'])\n",
    // Prose describing the rule is not the rule being broken.
    'packages/pkg/src/Prose.ts': '// A spawn with detached: true is severed from its parent.\n/* detached: true */\nexport const x = 1\n',
    // A test pinning the production source by string is not a spawn site.
    'packages/pkg/src/Pin.ts': 'expect(source).toContain("detached: true")\n',
    'apps/crate/src/ok.rs': 'fn go() {\n    command.output();\n}\n',
    // A shell comment, and a `&&` that is not a background `&`.
    'scripts/quiet.sh': '# nohup is what we do NOT do here\nfoo && bar\n',
  })
  try {
    assert.deepEqual(findSpawnSites(root), [])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('DEMONSTRATED RED: a row that matches nothing fails and says to delete it', () => {
  const stale = {
    file: 'packages/pkg/src/AServiceThisRepoNeverHad.ts',
    marker: 'js-detached',
    classification: 'orphanable',
    registeredOn: '2026-08-10',
    reason: 'a file this repo has never had, registered here purely so this arm has a subject to report',
  }
  const { stale: reported } = compareSitesToAllowlist(SITES, [...ORPHAN_SPAWNER_ALLOWLIST, stale])
  assert.deepEqual(reported, [`${rowKey(stale)} — matches nothing today; delete this row`])
  assert.equal(allowlistRowsNamingMissingFiles(REPO_ROOT, [stale]).length, 1)
})

test('DEMONSTRATED RED: a row claiming a watchdog its file lacks is named', () => {
  const root = fixture({
    'packages/pkg/src/Bare.ts': "const child = spawn(bin, [], { detached: true })\n",
  })
  try {
    const lying = [
      {
        file: 'packages/pkg/src/Bare.ts',
        marker: 'js-detached',
        classification: 'robust-watchdog',
        watchdog: 'chiefd-store-exit-with-parent',
        registeredOn: '2026-08-10',
        reason: 'claims the child-side self-kill without arming it, which is the one thing an allowlist may never do',
      },
    ]
    const violations = unbackedWatchdogClaims(root, lying)
    assert.equal(violations.length, 1)
    assert.match(violations[0], /does not contain/)

    // And the honest version of the same row, with the token really present,
    // passes -- so the arm is testing the token, not the classification.
    writeFileSync(
      join(root, 'packages/pkg/src/Bare.ts'),
      "const child = spawn(bin, [], { detached: true, env: { CHIEFD_STORE_EXIT_WITH_PARENT: '1' } })\n",
    )
    assert.deepEqual(unbackedWatchdogClaims(root, lying), [])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('DEMONSTRATED RED: a row with no reason, no date, a bogus marker or an unclaimed token fails', () => {
  const violations = allowlistShapeViolations([
    { file: 'a.ts', marker: 'js-detached', classification: 'orphanable', registeredOn: '2026-08-10', reason: 'short' },
    { file: 'b.ts', marker: 'js-detached', classification: 'orphanable', registeredOn: '', reason: 'x'.repeat(80) },
    { file: 'c.ts', marker: 'not-a-marker', classification: 'orphanable', registeredOn: '2026-08-10', reason: 'y'.repeat(80) },
    { file: 'd.ts', marker: 'js-detached', classification: 'invented', registeredOn: '2026-08-10', reason: 'z'.repeat(80) },
    {
      file: 'e.ts',
      marker: 'js-detached',
      classification: 'robust-watchdog',
      registeredOn: '2026-08-10',
      reason: 'w'.repeat(80),
    },
  ])
  assert.ok(violations.some((v) => v.includes('no written reason')))
  assert.ok(violations.some((v) => v.includes('no registration date')))
  assert.ok(violations.some((v) => v.includes('marker that does not exist')))
  assert.ok(violations.some((v) => v.includes('outside the closed set')))
  assert.ok(violations.some((v) => v.includes('names no token')))
  // No row above claims a watchdog token, so every entry in the vocabulary is
  // reported -- the arm that keeps a token naming a deleted mechanism from
  // reading as an available one.
  for (const token of Object.keys(WATCHDOG_TOKENS)) {
    assert.ok(violations.some((v) => v.includes(`"${token}"`) && v.includes('no row claims')))
  }
})

test('DEMONSTRATED RED: a scan root that stops matching files is named, not silently skipped', () => {
  const root = fixture({ 'apps/crate/src/ok.rs': 'fn go() {}\n' })
  try {
    const violations = scanVacuity(root)
    assert.ok(violations.some((v) => v.includes('the walk is broken')))
    for (const dir of ['packages', 'scripts', 'tests']) {
      assert.ok(
        violations.some((v) => v.includes(`scan root "${dir}"`)),
        `a root matching nothing must be named: ${dir}`,
      )
    }
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
