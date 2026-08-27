// THE "chiefd must not know about tmux" BOUNDARY, unconditional (#751/P10).
//
// The mandate is one sentence — chiefd is the backend, it is client-agnostic,
// and it does not know what a pane is — and a sentence loses to 105 files. P1
// turned it into a per-FILE violation REGISTER whose rows named the packet that
// would drain them; P8 and P9 drained the last of them; P10 deletes the
// register and the four rules that existed only to keep it honest.
//
// WHAT IS LEFT IS SIMPLER AND STRICTLY STRONGER. There is no work-list to add a
// row to, refile, or forget:
//
//   1. filesNamingTmux              — NO tracked `.rs` file under a scan root
//                                     matches /tmux/i. Comments included, on
//                                     purpose: a comment in `chiefd-core`
//                                     saying "the tmux pane this respawns" is
//                                     evidence the code still serves tmux, and
//                                     an exemption rule would be a second
//                                     classifier that itself rots.
//   2. backendCratesDependingOnCli  — the boundary crossed through Cargo
//                                     rather than through text.
//   3. clientCratesDependingOnBackend
//                                   — rule 2 read from the other side. A
//                                     boundary enforced in one direction is not
//                                     a boundary.
//   4. cratesOutOfScope             — every chiefd crate that carries tmux is a
//                                     DECLARED client crate. A new crate cannot
//                                     quietly become a place tmux is allowed.
//   5. blindScanRoots               — the guard can see its own subject.
//
// RULE 5 IS THE ONE THAT CHANGED SHAPE, and it is the interesting half of
// making this unconditional. While the register drained, non-vacuity was a
// floor on VIOLATIONS: 107 files matched, so a collapse to a handful meant the
// scan had gone blind rather than that the work was done. The passing state is
// now ZERO matches, so that floor would be either permanently red or set to
// zero — a floor that can never fail. It moves to the thing still supposed to
// be large: the count of tracked `.rs` files each root RESOLVES. #848's lesson
// is exactly this — a scan root that stopped existing returns an empty result
// that looks identical to verified-clean — so every root is asserted
// individually, since one dead root would otherwise hide behind three live ones.
//
// Run with `node --test scripts/test/backend-tmux-boundary.test.mjs`.

import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { execFileSync } from 'node:child_process'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import {
  backendCratesDependingOnCli,
  blindScanRoots,
  CLIENT_CRATES,
  clientCratesDependingOnBackend,
  cratesOutOfScope,
  deriveBoundaryReport,
  MINIMUM_TRACKED_FILES_PER_ROOT,
  parseTomlDependencyKeys,
  SCAN_ROOTS,
  scannedCountsByCrate,
  scanTmuxFiles,
  trackedRustFilesByRoot,
} from '../backend-tmux-boundary-lib.mjs'

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..')

test('chiefd must not know about tmux: no backend file names it, neither side of the client boundary depends on the other, and the guard can see its subject', () => {
  const report = deriveBoundaryReport(repoRoot)
  // One deepEqual over named arrays, so the failure message IS the report.
  assert.deepEqual(
    {
      filesNamingTmux: report.filesNamingTmux,
      backendCratesDependingOnCli: report.backendCratesDependingOnCli,
      clientCratesDependingOnBackend: report.clientCratesDependingOnBackend,
      cratesOutOfScope: report.cratesOutOfScope,
      blindScanRoots: report.blindScanRoots,
    },
    {
      filesNamingTmux: [],
      backendCratesDependingOnCli: [],
      clientCratesDependingOnBackend: [],
      cratesOutOfScope: [],
      blindScanRoots: [],
    },
  )
})

test('non-vacuity: every scan root resolves a real corpus, and the counts are reported per root', () => {
  const tracked = trackedRustFilesByRoot(repoRoot)
  assert.deepEqual(Object.keys(tracked).sort(), [...SCAN_ROOTS].sort())
  for (const [root, count] of Object.entries(tracked)) {
    assert.ok(
      count >= MINIMUM_TRACKED_FILES_PER_ROOT,
      `${root} resolves ${count} tracked .rs file(s); a scan that cannot see its subject must refuse, not report clean`,
    )
  }
  // And the violation scan agrees it looked at that corpus and found no CODE
  // naming tmux. The raw scan is deliberately NOT asserted empty: the backend
  // carries tombstones explaining why tmux left, and those are comment-only.
  // Asserting the raw count here would re-impose the rule this guard just
  // stopped enforcing, from the other side.
  assert.deepEqual(
    scanTmuxFiles(repoRoot).filter((entry) => !entry.commentOnly),
    [],
  )
  assert.deepEqual(
    Object.values(
      scannedCountsByCrate(scanTmuxFiles(repoRoot).filter((entry) => !entry.commentOnly)),
    ).reduce((a, b) => a + b, 0),
    0,
  )
})

test('non-vacuity: the CLIENT crate really does still carry tmux — the boundary has a live subject on the other side', () => {
  // If `chief-cli` were also clean, every assertion above would pass on a tree
  // where tmux had simply been deleted rather than MOVED, and the guard would
  // be proving nothing about a boundary.
  const clientFiles = scanTmuxFiles(repoRoot, CLIENT_CRATES)
  assert.ok(
    clientFiles.length > 0,
    'the operator client must still own tmux; a clean client means the concept was deleted, not moved',
  )
  // ...and it is not out of scope by accident: it is DECLARED.
  assert.deepEqual(cratesOutOfScope(repoRoot), [])
})

test('negative self-test: rules 1, 4 and 5 each fire on a doctored tree', () => {
  const root = mkdtempSync(join(tmpdir(), 'tmux-boundary-'))
  try {
    execFileSync('git', ['init', '-q'], { cwd: root })
    const backend = SCAN_ROOTS[0]
    const client = CLIENT_CRATES[0]
    for (const crate of [backend, client]) {
      mkdirSync(join(root, crate, 'src'), { recursive: true })
      writeFileSync(join(root, crate, 'Cargo.toml'), '[package]\nname = "x"\n\n[dependencies]\n')
    }
    // Enough tracked files that the blindness rule is satisfied to begin with,
    // so the other rules are observed in isolation.
    for (let n = 0; n < MINIMUM_TRACKED_FILES_PER_ROOT; n += 1) {
      writeFileSync(join(root, backend, 'src', `m${n}.rs`), 'pub fn f() {}\n')
      writeFileSync(join(root, client, 'src', `m${n}.rs`), 'pub fn f() {}\n')
    }
    assert.deepEqual(blindScanRoots(root, [backend]), [], 'the fixture starts sighted')
    assert.deepEqual(scanTmuxFiles(root, [backend]), [], 'and clean')

    // RULE 1, both directions. A comment-only mention is REPORTED but is not a
    // violation — the backend has to be able to write down why tmux left, and
    // one of those tombstones is the reason `TMUX_PANE` must never be renamed.
    writeFileSync(join(root, backend, 'src', 'note.rs'), '// the tmux pane this once respawned\n')
    const commented = scanTmuxFiles(root, [backend])
    assert.equal(commented.length, 1, 'a comment-only mention is still SEEN')
    assert.ok(commented[0].commentOnly, 'and is classified as comment-only')
    assert.equal(commented[0].codeHits, 0)

    // Real code naming tmux is the violation, and it must still be caught.
    writeFileSync(join(root, backend, 'src', 'bad.rs'), 'const S: &str = "tmux-socket";\n')
    const found = scanTmuxFiles(root, [backend]).filter((entry) => !entry.commentOnly)
    assert.equal(found.length, 1, 'code naming tmux is a violation')
    assert.equal(found[0].file, `${backend}/src/bad.rs`)
    assert.ok(found[0].codeHits >= 1)

    // RULE 4: a chiefd crate carrying tmux that is neither scanned nor declared.
    const stranger = 'apps/chiefd/crates/chiefd-stranger'
    mkdirSync(join(root, stranger, 'src'), { recursive: true })
    writeFileSync(join(root, stranger, 'Cargo.toml'), '[package]\nname = "s"\n')
    writeFileSync(join(root, stranger, 'src', 'lib.rs'), 'const S: &str = "tmux";\n')
    assert.deepEqual(cratesOutOfScope(root, CLIENT_CRATES), [
      `${stranger} carries tmux but is neither in SCAN_ROOTS nor a declared CLIENT crate`,
    ])

    // RULE 5: a root that resolves nothing is BLIND, not clean.
    assert.deepEqual(blindScanRoots(root, ['apps/chiefd/crates/gone']), [
      'apps/chiefd/crates/gone resolves only 0 tracked .rs file(s) — the scan is blind',
    ])
    assert.deepEqual(
      scanTmuxFiles(root, ['apps/chiefd/crates/gone']),
      [],
      'and its violation scan is empty — which is exactly why the blindness rule has to exist',
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('negative self-test: the dependency parse does not confuse a comment, a sibling backend crate, or a target-conditional table', () => {
  // `chiefd-core`/`chiefd-host`/`chiefd-api` all START with `chiefd`, so a
  // prefix match would flag every legitimate backend edge. A commented-out
  // dependency is not an edge. A target-conditional table IS one — a
  // boundary crossed only on Linux is still crossed.
  const manifest = [
    '[package]',
    'name = "chiefd-api"',
    '',
    '[dependencies]',
    'chiefd-core = { workspace = true }',
    'chiefd-host = { workspace = true }',
    '# chiefd = { path = "../chiefd" }  # deliberately not an edge',
    '',
    '[target.\'cfg(not(target_os = "macos"))\'.dependencies]',
    'rusqlite = { workspace = true }',
    '',
    '[package.metadata.whatever]',
    'chiefd = "not a dependency table"',
    '',
  ].join('\n')
  assert.deepEqual(
    parseTomlDependencyKeys(manifest).map((entry) => `${entry.table}:${entry.name}`),
    [
      'dependencies:chiefd-core',
      'dependencies:chiefd-host',
      'target.\'cfg(not(target_os = "macos"))\'.dependencies:rusqlite',
    ],
  )

  // And the live manifests, BOTH directions: today's real answer is "no
  // edge", which is only meaningful because the parse above demonstrably
  // finds real edges.
  assert.deepEqual(backendCratesDependingOnCli(repoRoot), [])
  assert.deepEqual(clientCratesDependingOnBackend(repoRoot), [])
})
