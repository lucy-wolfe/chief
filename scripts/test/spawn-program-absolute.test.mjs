// GUARD: no process this product spawns is named by a bare name.
//
// The generalisation of the cold-attach defect fixed in 99e0a3e69, where three
// processes each answered "where is Pi?" in their own environment and nothing
// made them agree. The rule, the derivation and the register live in
// `scripts/spawn-program-absolute-lib.mjs`; this file is the assertion half.
//
// Two properties, because the defect has a loud form and a quiet one:
//
//   * NO BARE NAME — a program literal a shipped process hands to a spawn must
//     be absolute, or carry a register row with a written reason.
//   * ONE RESOLVER — no two production files may answer one product question
//     ("where is Pi?"). 99e0a3e69 collapsed three answers on the spawn path and
//     left a fourth on the preflight path, which is how `chief attach` came to
//     refuse on a host where Founder started.
//
// Arms, in the order this repo has learned to need them:
//
//   1. THE REAL TREE — one `deepEqual` per property, carrying both directions,
//      so the diff is the report.
//   2. ROW SHAPE — a register row that cannot be checked is not a fact.
//   3. NON-VACUITY — a clean answer from a scan that read nothing is not
//      evidence. Floors on files read, detectors armed, and findings seen.
//   4. DEMONSTRATED RED — every detector, both register arms and the second
//      resolver, fired against a fixture tree. A guard that has never failed has
//      never been tested.
//
// Fixtures are written under `mkdtemp`, never into the checkout: this suite has
// to leave the working tree byte-identical (`ci-guard-shard`).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  DETECTORS,
  REGISTERED_BARE_NAMES,
  compareFindingsToRegister,
  productSourceFiles,
  productionRust,
  registerRowsNamingMissingFiles,
  registerShapeViolations,
  resolverSites,
  rowKey,
  scanForBareNamePrograms,
  secondResolverViolations,
  SINGLE_SOURCE_FACTS,
} from '../spawn-program-absolute-lib.mjs'

const REPO_ROOT = fileURLToPath(new URL('../..', import.meta.url))

/** Scan the real checkout once; every arm below reads this. */
const FILES = productSourceFiles(REPO_ROOT)
const FINDINGS = scanForBareNamePrograms(REPO_ROOT, FILES)

// --- 1. the real tree --------------------------------------------------------

test('every program a shipped process names by a literal is absolute, or registered', () => {
  assert.deepEqual(compareFindingsToRegister(FINDINGS, REGISTERED_BARE_NAMES), {
    unregistered: [],
    stale: [],
  })
})

test('one product question, one resolver: nothing answers "where is Pi?" twice', () => {
  // The QUIET form of the same defect. 99e0a3e69 collapsed three answers on the
  // spawn path and left a fourth on the preflight path. The ladder is one rung
  // now — `PATH` — which removes two of the three ways this could regrow and
  // leaves the rule and its scan exactly as they were.
  assert.deepEqual(secondResolverViolations(REPO_ROOT, FILES), [])
})

test('DEMONSTRATED RED: a second file answering the same question is named, with both files', () => {
  const root = mkdtempSync(join(tmpdir(), 'spawn-program-absolute-resolver-'))
  try {
    mkdirSync(join(root, 'apps/crate/src'), { recursive: true })
    writeFileSync(
      join(root, 'apps/crate/src/preflight.rs'),
      'fn a() { candidates_on_path("pi", path_var, &is_executable) }\n',
    )
    const clean = secondResolverViolations(root)
    assert.deepEqual(clean, [], `one file stating the fact is the healthy state: ${clean}`)

    writeFileSync(
      join(root, 'apps/crate/src/founder_pi.rs'),
      'fn b() { candidates_on_path("pi", other, &also) }\n',
    )
    const violations = secondResolverViolations(root)
    assert.equal(violations.length, 1)
    assert.match(violations[0], /2 production files answer/)
    assert.match(violations[0], /preflight\.rs/)
    assert.match(violations[0], /founder_pi\.rs/)

    // And the vacuity arm: a tree stating the fact NOWHERE fails too, because
    // "no violations" from a scan that read nothing is not evidence.
    const empty = mkdtempSync(join(tmpdir(), 'spawn-program-absolute-empty-'))
    try {
      assert.equal(secondResolverViolations(empty).length, SINGLE_SOURCE_FACTS.length)
    } finally {
      rmSync(empty, { recursive: true, force: true })
    }
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('the resolver scan actually sees the real resolver, by file', () => {
  const sites = resolverSites(REPO_ROOT, FILES)
  // ONE FACT NOW, in one file. `pi-pin-env` (preflight.rs) and
  // `pi-checkout-path` (founder_pi.rs) were the two this asserted; both are
  // deleted from the product with the Pi pin, and the `PATH` walk is the only
  // remaining statement of where Pi is.
  assert.deepEqual(sites.get('pi-path-lookup'), [
    'apps/chiefd/crates/chief-cli/src/preflight.rs',
  ])
})

// --- 2. row shape ------------------------------------------------------------

test('every register row is a checkable fact: reason, date, real detector, real file', () => {
  assert.deepEqual(registerShapeViolations(REGISTERED_BARE_NAMES), [])
  assert.deepEqual(registerRowsNamingMissingFiles(REPO_ROOT, REGISTERED_BARE_NAMES), [])
})

// --- 3. non-vacuity ----------------------------------------------------------

test('the scan is not vacuous: it read the product, and it can still see something', () => {
  // Floors, not inventories. Well under the real numbers so a legitimate
  // deletion never fails here, and far enough above zero that a scan which
  // silently read nothing does.
  assert.ok(
    FILES.length >= 200,
    `only ${FILES.length} product source files were read; the walk is broken, not the tree`,
  )
  assert.ok(
    FILES.some((file) => file.startsWith('apps/chiefd/crates/') && file.endsWith('.rs')),
    'the chiefd crates must be in scope',
  )
  assert.ok(
    FILES.some((file) => file.startsWith('packages/') && file.endsWith('.ts')),
    'the TypeScript packages must be in scope',
  )
  assert.ok(DETECTORS.length >= 4, 'the detector set collapsed')
  assert.ok(
    FINDINGS.length >= REGISTERED_BARE_NAMES.length,
    `${FINDINGS.length} findings against ${REGISTERED_BARE_NAMES.length} rows — ` +
      'a register cannot outnumber what the scan can see',
  )
})

test('test bodies are out of scope, and the cut is the one the crates use', () => {
  const source = 'fn real() {}\n#[cfg(test)]\nmod tests { Command::new("throwaway"); }\n'
  assert.equal(productionRust(source), 'fn real() {}\n')
  assert.ok(!FILES.includes('apps/chiefd/crates/chiefd-daemon/build.rs'))
  assert.ok(!FILES.some((file) => file.endsWith('/tests.rs')))
  assert.ok(!FILES.some((file) => file.includes('/tests/')))
})

// --- 4. demonstrated red -----------------------------------------------------

/** A throwaway tree with the given files, outside the checkout. */
function fixture(files) {
  const root = mkdtempSync(join(tmpdir(), 'spawn-program-absolute-'))
  for (const [path, contents] of Object.entries(files)) {
    const absolute = join(root, path)
    mkdirSync(dirname(absolute), { recursive: true })
    writeFileSync(absolute, contents)
  }
  return root
}

test('DEMONSTRATED RED: every detector names the file, the line and the program', () => {
  const root = fixture({
    // The literal spawn.
    'apps/crate/src/lifecycle.rs': 'fn go() {\n    Command::new("tmux").arg("-L");\n}\n',
    // The shape `SystemTmuxRunner::default` takes.
    'apps/crate/src/runner.rs': 'fn d() -> Self {\n    Self { binary: PathBuf::from("pi") }\n}\n',
    // THE DEFECT, verbatim: `chiefd` before 99e0a3e69, and the dead
    // `PiPaths` it left behind in `main.rs`.
    'apps/crate/src/daemon.rs':
      'fn go() {\n' +
      '    let pi_binary = std::env::var(PI_BINARY_ENV)\n' +
      '        .map(PathBuf::from)\n' +
      '        .unwrap_or_else(|_| PathBuf::from("pi"));\n' +
      '}\n',
    // The same default reached through a binding named nothing like a program.
    'apps/crate/src/other.rs':
      'fn go() {\n' +
      '    let chosen = std::env::var("CHIEFD_PI_BINARY").unwrap_or_else(|_| "pi".to_string());\n' +
      '}\n',
    // The TypeScript half.
    'packages/pkg/src/Harness.ts': "const child = spawn('tmux', ['-L', socket])\n",
  })
  try {
    const findings = scanForBareNamePrograms(root)
    const fired = new Set(findings.map((finding) => finding.detector))
    for (const detector of DETECTORS) {
      assert.ok(
        fired.has(detector.id),
        `detector "${detector.id}" (${detector.what}) never fired: ${JSON.stringify(findings)}`,
      )
    }

    const defect = findings.find((finding) => finding.file === 'apps/crate/src/daemon.rs')
    assert.ok(defect, 'the defect that shipped must be found')
    assert.equal(defect.program, 'pi')
    // The line of the BINDING, not of the fallback literal three lines further
    // down: the binding is what the reader has to change.
    assert.equal(defect.line, 2, 'the line must be the line, so a reader can go there')

    // And every one is reported as unregistered, because none is in the
    // register — this is exactly what a new bare-name spawn looks like.
    const { unregistered } = compareFindingsToRegister(findings, [])
    assert.equal(unregistered.length, findings.length)
    assert.ok(unregistered.every((row) => /:\d+ \[[a-z-]+\] "/.test(row)))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('DEMONSTRATED RED: an absolute program, a test body, and a comment are all silent', () => {
  const root = fixture({
    'apps/crate/src/ok.rs':
      'fn go() {\n' +
      '    Command::new("/usr/bin/env").arg("pi");\n' +
      '    let binary = PathBuf::from("/opt/pi/bin/pi");\n' +
      '}\n' +
      '#[cfg(test)]\n' +
      'mod tests {\n' +
      '    fn t() { Command::new("tmux"); }\n' +
      '}\n',
    'apps/crate/src/prose.rs':
      '//! `Command::new("pi")` is what the defect looked like.\n' +
      '// let binary = PathBuf::from("pi");\n' +
      'fn go() {}\n',
    'packages/pkg/src/Ok.ts': "const child = spawn(binaryPath, ['run'])\n",
  })
  try {
    assert.deepEqual(scanForBareNamePrograms(root), [])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('DEMONSTRATED RED: a register row that matches nothing fails and says to delete it', () => {
  const stale = {
    file: 'apps/chiefd/crates/chief-cli/src/tmux.rs',
    detector: 'rust-command-new',
    program: 'screen',
    registeredOn: '2026-08-10',
    reason: 'a multiplexer this product has never used, registered here purely to be stale',
  }
  const { stale: reported } = compareFindingsToRegister(FINDINGS, [
    ...REGISTERED_BARE_NAMES,
    stale,
  ])
  assert.deepEqual(reported, [`${rowKey(stale)} — matches nothing today; delete this row`])
})

test('DEMONSTRATED RED: a row with no reason, no date, or a bogus detector fails', () => {
  const violations = registerShapeViolations([
    { file: 'a.rs', detector: 'rust-command-new', program: 'pi', registeredOn: '2026-08-10', reason: 'short' },
    { file: 'b.rs', detector: 'rust-command-new', program: 'pi', reason: 'x'.repeat(50) },
    { file: 'c.rs', detector: 'not-a-detector', program: 'pi', registeredOn: '2026-08-10', reason: 'y'.repeat(50) },
    { file: 'd.rs', detector: 'rust-command-new', program: '/usr/bin/pi', registeredOn: '2026-08-10', reason: 'z'.repeat(50) },
  ])
  assert.ok(violations.some((v) => v.includes('written reason')))
  assert.ok(violations.some((v) => v.includes('registration date')))
  assert.ok(violations.some((v) => v.includes('detector that does not exist')))
  assert.ok(violations.some((v) => v.includes('needs no registration')))

  assert.deepEqual(
    registerRowsNamingMissingFiles(REPO_ROOT, [
      {
        file: 'apps/chiefd/crates/chief-cli/src/does-not-exist.rs',
        detector: 'rust-command-new',
        program: 'pi',
        registeredOn: '2026-08-10',
        reason: 'a path this repo does not have, registered here purely to prove the arm fires',
      },
    ]).length,
    1,
  )
})
