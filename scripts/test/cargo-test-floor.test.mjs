// #857: the executed-test and suite-block floors for `cargo test --workspace`.
// Ported to the same `node --test` shape as sql-only-state.test.mjs and
// parked-suite-triage.test.mjs.
//
// Fixtures are the GENUINE historical pair from gating #814
// (scripts/test/fixtures/cargo-real-{clean-2360,truncated-1816}.txt),
// preserved by the merger at /workspace/evidence/857 on a build host and
// pulled in verbatim — not derived or invented. Same tree, one variable
// (the release binary present vs. missing), full raw `cargo test` output
// both ways.
//
// Five properties:
//   1. Parsing is correct against both real logs.
//   2. The floor fires on the truncated log and does NOT fire on the clean
//      log — as two SEPARATE, independently-named assertions (the merger's
//      point: a floor of 1 would also "pass" the clean log, so proving the
//      comparison works is not the same claim as proving it catches what
//      it was built for).
//   3. The block-count floor specifically — not just the executed-count
//      floor — fires on the truncated log even with every `FAILED` line
//      stripped out, so a naive "any FAILED line" detector wearing a
//      floor's clothing would fail this test even though it "looks"
//      correct against the unmodified fixture.
//   4. Both floors are RATCHETS, checked against this file's actual git
//      history, not just documented as one.
//   5. A skip marker refuses the run even when BOTH floors are cleared — the
//      merger's finding against #852's new e2e precondition: libtest has no
//      first-class "skipped" outcome, so an early return past a
//      precondition still prints `test result: ok. 1 passed; …`, identical
//      to a real pass in every signal the floors read. #862 replaced the
//      original bare `/\bSKIPPING\b/` word-scan with a namespaced,
//      structured `CHIEFD_E2E_SKIPPED suite="…" reason="…"` line — a common
//      English word risks an accidental match in unrelated log output, and
//      the structured fields let a refusal name exactly which suite(s)
//      skipped and why instead of just "something skipped, somewhere."
//
// #889: sections 1-4 above test parsing/skip-marker/vacuity-floor behavior
// that is UNCHANGED by this packet and stays exactly as it was. What #889
// added is sections 5-8 below: the derivation in
// scripts/cargo-test-derive.mjs (workspace-member discovery, structural
// block counting, test-attribute enumeration), the exact-match `checkExact`
// comparison, an end-to-end demonstration that the mechanism actually fires
// (on a synthetic tree this file fully controls, per §0.5 — "a guard nobody
// has seen fail is a decoration"), and a sanity check against the real live
// tree. The two historical constants this file used to ratchet-check
// (CARGO_TEST_EXECUTED_FLOOR/CARGO_TEST_BLOCK_FLOOR) are retired along with
// their transcription — see cargo-test-floor.mjs's header for the full
// before/after reasoning.

import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import {
  CARGO_TEST_BLOCK_VACUITY_FLOOR,
  CARGO_TEST_EXECUTED_VACUITY_FLOOR,
} from '../cargo-test-floor.mjs'
import {
  checkExceptionLiveness,
  deriveExpectedCounts,
  findHarnessDisabledTestPaths,
  findOrphanedIgnoreAttributes,
  KNOWN_TEST_COUNT_EXCEPTIONS,
  parseWorkspaceMembers,
  resolveTarget,
  scanFileForDocExamples,
  scanFileForTestAttributes,
} from '../cargo-test-derive.mjs'
import {
  checkExact,
  checkFloor,
  countResultLines,
  hasCompileFailure,
  hasSkippedTest,
  parseExecutedCount,
  parseSkippedTests,
} from '../cargo-test-floor-lib.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const floorFile = join(here, '..', 'cargo-test-floor.mjs')
const chiefdRoot = join(repoRoot, 'apps', 'chiefd')
const workspaceTestWrapper = join(repoRoot, 'scripts', 'cargo-test-workspace.sh')
const workspaceShardWrapper = join(repoRoot, 'scripts', 'cargo-test-workspace-shard.sh')

function fixture(name) {
  return readFileSync(join(here, 'fixtures', name), 'utf8')
}

const CLEAN = 'cargo-real-clean-2360.txt'
const TRUNCATED = 'cargo-real-truncated-1816.txt'

// The floors AS THEY WERE when the #814 fixtures were captured — used for
// every fixture-based `checkFloor` call below, deliberately NOT the LIVE
// vacuity floor (`CARGO_TEST_EXECUTED_VACUITY_FLOOR`/
// `CARGO_TEST_BLOCK_VACUITY_FLOOR`) imported above. Testing a frozen
// historical fixture against whatever the vacuity floor happens to be TODAY
// would make these tests fail as an unrelated side effect of a LATER,
// unrelated ratchet — exactly the kind of coupling a fixture is supposed to
// avoid. (#889: there is no longer a live EXACT floor to worry about
// coupling against here at all — that comparison is now derived fresh per
// run in section 6/7 below, against real or synthetic trees, never against
// a frozen fixture like this one.)
const HISTORICAL_EXECUTED_FLOOR = 2360
const HISTORICAL_BLOCK_FLOOR = 69

// ---- 1. Parsing against the real historical pair -----------------------

test('the real clean log (gating #814, after the release binary was rebuilt) parses to 2360 executed across 69 suites', () => {
  const output = fixture(CLEAN)
  assert.equal(parseExecutedCount(output), 2360)
  assert.equal(countResultLines(output), 69)
  assert.equal(hasCompileFailure(output), false)
})

test('the sanctioned Rust test wrapper forwards --no-run to its disk-bounded shard', () => {
  const wrapper = readFileSync(workspaceTestWrapper, 'utf8')
  const shard = readFileSync(workspaceShardWrapper, 'utf8')
  assert.match(wrapper, /cargo-test-workspace-shard\.sh" \"\$@\"/)
  assert.match(shard, /the only supported shard option is --no-run/)
  assert.match(shard, /cargo_args\+=\(--no-run\)/)
  assert.match(shard, /if \(\( run_only \)\); then/)
  assert.match(shard, /CI_CARGO_PARALLEL_TARGETS/)
  assert.match(shard, /target_args\+=\(--test/)
  assert.match(shard, /target_args\+=\(--lib\)/)
  assert.match(shard, /wait "\$\{pids\[\$index\]\}"/)
})

test('the real truncated log (the #814 incident itself) parses to 1817 executed across 52 suites', () => {
  const output = fixture(TRUNCATED)
  // 1816 passed + 1 failed = 1817 executed. The merger's count (1816) was
  // passed-only; this function sums passed+failed, which is the honest
  // "how many tests actually ran" number the floor compares against.
  assert.equal(parseExecutedCount(output), 1817)
  assert.equal(countResultLines(output), 52)
  assert.equal(hasCompileFailure(output), false, 'the #814 incident was pure fail-fast truncation, not a compile failure')
})

// ---- 2. Fires on truncated, does NOT fire on clean — as separate claims -

test('the floor does NOT fire on the real clean log', () => {
  const result = checkFloor(fixture(CLEAN), HISTORICAL_EXECUTED_FLOOR, HISTORICAL_BLOCK_FLOOR)
  assert.equal(result.ok, true, `expected the clean log to pass: ${result.message ?? ''}`)
})

test('the floor DOES fire on the real truncated log — the actual #814 incident, not a derived fixture', () => {
  const result = checkFloor(fixture(TRUNCATED), HISTORICAL_EXECUTED_FLOOR, HISTORICAL_BLOCK_FLOOR)
  assert.equal(result.ok, false, 'expected the truncated log to fail the floor')
  assert.match(result.message, /below the floor/)
})

test('a floor of 1 would ALSO "pass" the clean log — proving the comparison arithmetic works is not the same claim as proving it catches truncation, so both directions above are asserted independently', () => {
  const trivialFloor = checkFloor(fixture(CLEAN), 1, 1)
  assert.equal(trivialFloor.ok, true)
  // The real floors, checked separately above, are what actually catch
  // #814 — this test exists only to name the gap a single "comparison
  // works" assertion would leave unstated.
})

// ---- 3. The block floor specifically survives FAILED-line removal ------

test('the block-count floor fires on the truncated log EVEN WITH every FAILED line removed — not a "contains a failure" detector wearing a floor\'s clothing', () => {
  const withoutFailures = fixture(TRUNCATED)
    .split('\n')
    .filter((line) => !line.includes('FAILED'))
    .join('\n')
  assert.doesNotMatch(withoutFailures, /FAILED/, 'the fixture must genuinely have no FAILED text left')

  const result = checkFloor(withoutFailures, HISTORICAL_EXECUTED_FLOOR, HISTORICAL_BLOCK_FLOOR)
  assert.equal(result.ok, false, 'the floor must still fire with zero failure lines present — this is the compile-failure shape: no failures to not-fail-fast on, just missing suites')
  assert.match(result.message, /suite/i)
})

test('a compile failure that leaves a crate contributing zero test-result lines is caught, and named as the likely cause', () => {
  const output = [
    'test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s',
    'error[E0433]: failed to resolve: use of undeclared crate or module `chiefd_core`',
    ' --> crates/chiefd-api/src/lib.rs:12:5',
    'error: could not compile `chiefd-api` (lib) due to 1 previous error',
  ].join('\n')
  assert.equal(hasCompileFailure(output), true)
  const result = checkFloor(output, HISTORICAL_EXECUTED_FLOOR, HISTORICAL_BLOCK_FLOOR)
  assert.equal(result.ok, false)
  assert.match(result.message, /compile failure/)
  assert.match(result.message, /ZERO/)
})

test('a genuine test failure inside an otherwise-complete run does not itself trip the floor — a run can fail loudly on real content and still be complete', () => {
  const output =
    fixture(CLEAN) +
    '\ntest result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s\n'
  const result = checkFloor(output, HISTORICAL_EXECUTED_FLOOR, HISTORICAL_BLOCK_FLOOR)
  assert.equal(result.ok, true, `a complete run with one real failure still executed enough: ${result.message ?? ''}`)
  assert.equal(result.executed, 2364)
  assert.equal(result.blocks, 70)
})

// ---- 4b. the #862 skip marker refuses the run even when both floors are
//          cleared, and does so with a NAMED, structured suite+reason ----
//
// The real marker format `chiefd_e2e::skip_banner`
// (apps/chiefd/tests/e2e/src/lib.rs) emits — verified against that
// function's own unit tests, not hand-guessed here:
//   CHIEFD_E2E_SKIPPED suite="<label>" reason="<missing|stale|check_failed>"

test('the #862 skip marker refuses the run even though both floors are comfortably cleared — a skipped test still prints "test result: ok. 1 passed"', () => {
  const output =
    fixture(CLEAN) +
    '\nCHIEFD_E2E_SKIPPED suite="supervisor_handoff_byte_identity" reason="stale"\n' +
    '[chiefd-e2e] SKIPPING "supervisor_handoff_byte_identity": chiefd release binary not built locally.\n' +
    'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
  assert.equal(hasSkippedTest(output), true)
  // Both floors alone would pass this output — 2361 executed, 70 blocks,
  // both above the (historical, fixture-relative) floors — which is
  // exactly the hole this check exists to close.
  assert.ok(parseExecutedCount(output) >= HISTORICAL_EXECUTED_FLOOR)
  assert.ok(countResultLines(output) >= HISTORICAL_BLOCK_FLOOR)
  const result = checkFloor(output, HISTORICAL_EXECUTED_FLOOR, HISTORICAL_BLOCK_FLOOR)
  assert.equal(result.ok, false, 'a skip marker must refuse the run regardless of the counts')
  assert.match(result.message, /supervisor_handoff_byte_identity/)
  assert.match(result.message, /stale/)
})

test('parseSkippedTests extracts the suite and reason as structured fields, not just a boolean', () => {
  const output =
    'CHIEFD_E2E_SKIPPED suite="supervisor_handoff_byte_identity" reason="missing"\n' +
    'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
  const skipped = parseSkippedTests(output)
  assert.deepEqual(skipped, [{ suite: 'supervisor_handoff_byte_identity', reason: 'missing' }])
})

test('parseSkippedTests names EVERY skipped suite when more than one skips in the same run', () => {
  const output =
    'CHIEFD_E2E_SKIPPED suite="suite_a" reason="missing"\n' +
    'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n' +
    'CHIEFD_E2E_SKIPPED suite="suite_b" reason="stale"\n' +
    'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
  const skipped = parseSkippedTests(output)
  assert.deepEqual(skipped, [
    { suite: 'suite_a', reason: 'missing' },
    { suite: 'suite_b', reason: 'stale' }
  ])
  const result = checkFloor(output, 0, 0)
  assert.match(result.message, /suite_a/)
  assert.match(result.message, /suite_b/)
})

test('negative self-test: the bare English word "SKIPPING" alone (the pre-#862 shape) is no longer sufficient to trip the check — proves this is a real format match, not a leftover substring check', () => {
  const output = 'the word SKIPPING appears here but not as the real marker line\n'
  assert.equal(hasSkippedTest(output), false)
})

test('the real clean and truncated logs both have no skip marker today — a floor of "no skips" is not accidentally already tripped by history', () => {
  assert.equal(hasSkippedTest(fixture(CLEAN)), false)
  assert.equal(hasSkippedTest(fixture(TRUNCATED)), false)
})

// ---- 4. The VACUITY floor (only) is still a checked ratchet -------------
//
// #889 retired CARGO_TEST_EXECUTED_FLOOR/CARGO_TEST_BLOCK_FLOOR — the exact
// loss ratchet those used to be is now DERIVED (section 6 below), so there
// is nothing left transcribed to ratchet-check for it. What remains
// hand-maintained is the much smaller, deliberately wide vacuity floor
// (cargo-test-floor.mjs), and THAT is still worth pinning against its own
// git history for the same reason #857 originally did this: a floor that
// silently decreased would be indistinguishable from one that was always
// this low.

function assertNeverDecreased(exportName, currentValue) {
  let log
  try {
    log = execFileSync('git', ['log', '-p', '--follow', '--reverse', '--', floorFile], {
      cwd: repoRoot,
      encoding: 'utf8',
    })
  } catch {
    log = ''
  }
  const pattern = new RegExp(`^\\+export const ${exportName} = (\\d+)`, 'gm')
  const historical = [...log.matchAll(pattern)].map((m) => Number(m[1]))
  const sequence = [...historical, currentValue]
  for (let i = 1; i < sequence.length; i += 1) {
    assert.ok(
      sequence[i] >= sequence[i - 1],
      `${exportName} decreased from ${sequence[i - 1]} to ${sequence[i]} somewhere in its history ` +
        `(full sequence: ${sequence.join(' -> ')}) — a drop is the signal, not the noise (#857); ` +
        'raise it for legitimately added tests/crates, never lower it to make a run pass'
    )
  }
}

test('CARGO_TEST_EXECUTED_VACUITY_FLOOR has never decreased across its entire git history', () => {
  assertNeverDecreased('CARGO_TEST_EXECUTED_VACUITY_FLOOR', CARGO_TEST_EXECUTED_VACUITY_FLOOR)
})

test('CARGO_TEST_BLOCK_VACUITY_FLOOR has never decreased across its entire git history', () => {
  assertNeverDecreased('CARGO_TEST_BLOCK_VACUITY_FLOOR', CARGO_TEST_BLOCK_VACUITY_FLOOR)
})

test('the vacuity floor is wide relative to the real historical fixture — it must not fire on ordinary legitimate deletion the way the old exact floor did', () => {
  // #871's actual incident: the tree legitimately carried FEWER tests before
  // unit-d was wired in (2379 vs the 2390 that followed). A vacuity floor
  // that would have fired on that legitimate, smaller-but-real tree is not
  // "wide" — this pins that the chosen constants stay comfortably under any
  // real count this repo has ever measured.
  assert.ok(CARGO_TEST_EXECUTED_VACUITY_FLOOR < 2360, 'must clear even the oldest real captured fixture')
  assert.ok(CARGO_TEST_BLOCK_VACUITY_FLOOR < 69, 'must clear even the oldest real captured fixture')
})

// ---- 5. Derivation primitives — pure, small, and independently checkable

test('parseWorkspaceMembers reads a real members array with interleaved comment lines, matching apps/chiefd/Cargo.toml\'s own shape around tests/unit-d', () => {
  const cargoToml = `
[workspace]
resolver = "2"
members = [
    "crates/beacond",
    "crates/chiefd-core",
    # a comment line, exactly like the real file has around tests/unit-d
    "tests/unit-d",
]
`
  assert.deepEqual(parseWorkspaceMembers(cargoToml), ['crates/beacond', 'crates/chiefd-core', 'tests/unit-d'])
})

test('parseWorkspaceMembers returns an empty array (not a throw) for a Cargo.toml with no [workspace] table — the vacuity guard downstream is what turns that into a loud failure', () => {
  assert.deepEqual(parseWorkspaceMembers('[package]\nname = "x"\n'), [])
})

test('parseWorkspaceMembers extracts EVERY quoted member on a line, not just the first — architect2\'s own miscount (a character class silently narrowing a match set) is exactly the shape a first-match-per-line extraction would have if this array were ever reformatted onto fewer lines', () => {
  const cargoToml = '[workspace]\nmembers = [\n    "crates/a", "crates/b", "crates/c",\n]\n'
  assert.deepEqual(parseWorkspaceMembers(cargoToml), ['crates/a', 'crates/b', 'crates/c'])
})

test('scanFileForTestAttributes counts #[test] and #[tokio::test(...)] as declared, and only the ones directly followed by #[ignore...] as ignored', () => {
  const src = [
    '#[test]',
    'fn plain() {}',
    '',
    '#[tokio::test]',
    'async fn plain_async() {}',
    '',
    '#[tokio::test(flavor = "multi_thread")]',
    'async fn parametrized() {}',
    '',
    '#[test]',
    '#[ignore = "some reason"]',
    'fn ignored_one() {}',
  ].join('\n')
  assert.deepEqual(scanFileForTestAttributes(src), { declared: 4, ignored: 1 })
})

test('scanFileForTestAttributes does NOT count a mention of "#[test]" inside a // comment or doc comment as a declared test — the real trap: a broad regex over this codebase would overcount by treating prose about test attributes as test attributes', () => {
  const src = [
    '// `#[test]`-attributed functions get allow-expect-in-tests from clippy.toml',
    '/// Call at the top of a `#[test]` that depends on the release binary.',
    'fn helper() {}',
  ].join('\n')
  assert.deepEqual(scanFileForTestAttributes(src), { declared: 0, ignored: 0 })
})

test('findOrphanedIgnoreAttributes is empty for every real #[ignore] in this repo today — the adjacency assumption scanFileForTestAttributes relies on actually holds against the live tree, not just a synthetic fixture', () => {
  const derived = deriveExpectedCounts(chiefdRoot)
  assert.deepEqual(
    derived.orphanedIgnores,
    [],
    'an orphaned #[ignore] means a test attribute + #[ignore] pair no longer sits adjacent the way ' +
      'every instance did when this derivation was designed — investigate before trusting the count'
  )
})

// ---- 5b. Runnable doc examples — the second half of the executed count --
//
// #1051: this derivation counted test ATTRIBUTES only, and this file's own
// header asserted that was complete ("zero runnable doc examples today").
// #1049 added one ```no_run example to crates/chiefd-log/src/lib.rs, which
// compiles and prints a passing libtest line, so the support shard observed
// 69 against a derived 68 and CI went red on a correct change. The rule is
// derived now, and these are the cases it has to get right.

test('scanFileForDocExamples counts what rustdoc runs: ```no_run and ```compile_fail and a bare fence execute, ```text is not a test at all, and ```ignore is declared but reported ignored', () => {
  const src = [
    '//! ```no_run',
    '//! chiefd_log::install("chiefd");',
    '//! ```',
    '//!',
    '//! ```text',
    '//! company.launch  phase=staging',
    '//! ```',
    '//!',
    '//! ```',
    '//! let plain = 1;',
    '//! ```',
    '//!',
    '//! ```rust,compile_fail',
    '//! let refused: u8 = "not a number";',
    '//! ```',
    '//!',
    '//! ```ignore',
    '//! this_one_is_reported_but_never_run();',
    '//! ```',
  ].join('\n')
  // Four of the five fences are Rust to rustdoc (```text is not a test at
  // all), and one of those four is the ```ignore one — reported, never run,
  // so it nets out of executed exactly like an `#[ignore]`d function, which
  // is why it is declared AND ignored rather than skipped.
  assert.deepEqual(scanFileForDocExamples(src), { declared: 4, ignored: 1 })
})

test('scanFileForDocExamples treats an unknown info string as prose, not Rust — the ```yaml trap a "everything except text" rule would count as a test', () => {
  const src = ['//! ```yaml', '//! company: acme', '//! ```'].join('\n')
  assert.deepEqual(scanFileForDocExamples(src), { declared: 0, ignored: 0 })
})

test('scanFileForDocExamples counts a /// example on a pub item and NOT one on a private or pub(crate) item — rustdoc collects nothing from an item it does not document', () => {
  const onPublic = ['/// ```', '/// let one = 1;', '/// ```', 'pub fn install() {}'].join('\n')
  assert.deepEqual(scanFileForDocExamples(onPublic), { declared: 1, ignored: 0 })

  const onPrivate = ['/// ```', '/// let one = 1;', '/// ```', 'fn helper() {}'].join('\n')
  assert.deepEqual(scanFileForDocExamples(onPrivate), { declared: 0, ignored: 0 })

  const onCrateVisible = ['/// ```', '/// let one = 1;', '/// ```', 'pub(crate) fn helper() {}'].join('\n')
  assert.deepEqual(scanFileForDocExamples(onCrateVisible), { declared: 0, ignored: 0 })

  const throughAttributes = [
    '/// ```',
    '/// let one = 1;',
    '/// ```',
    '/// More prose after the example.',
    '#[inline]',
    'pub fn install() {}',
  ].join('\n')
  assert.deepEqual(scanFileForDocExamples(throughAttributes), { declared: 1, ignored: 0 })
})

test('scanFileForDocExamples does not read a ``` inside a fenced block as a new fence', () => {
  const src = ['//! ```text', '//! Write it as ```rust in the guide.', '//! ```'].join('\n')
  assert.deepEqual(scanFileForDocExamples(src), { declared: 0, ignored: 0 })
})

test('the live tree: chiefd-log derives its runnable doc example, and chief-cli — whose every doc fence is ```text — derives none', () => {
  const withExample = deriveExpectedCounts(chiefdRoot, { memberPaths: ['crates/chiefd-log'] })
  assert.ok(
    withExample.docExamples >= 1,
    'crates/chiefd-log/src/lib.rs carries a ```no_run example that really runs (the CI log names it: ' +
      '"test crates/chiefd-log/src/lib.rs - (line 33) - compile ... ok"). A zero here means the scan stopped ' +
      'seeing it and the executed count is short again.'
  )
  const withoutExamples = deriveExpectedCounts(chiefdRoot, { memberPaths: ['crates/chief-cli'] })
  assert.equal(
    withoutExamples.docExamples,
    0,
    'chief-cli documents tmux layouts and log shapes in ```text fences; counting those would invent tests ' +
      'no run produces'
  )
})

test('a doc example under tests/ or in a bin target is not counted — cargo test --doc collects the lib target only', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'cargo-doc-example-scope-'))
  try {
    writeFileSync(
      join(fixtureRoot, 'Cargo.toml'),
      '[workspace]\nresolver = "2"\nmembers = [\n    "crates/only",\n]\nexclude = []\n'
    )
    const crate = join(fixtureRoot, 'crates', 'only')
    mkdirSync(join(crate, 'src', 'bin'), { recursive: true })
    mkdirSync(join(crate, 'tests'), { recursive: true })
    writeFileSync(join(crate, 'Cargo.toml'), '[package]\nname = "only"\nversion = "0.0.0"\n')
    const example = ['//! ```no_run', '//! let one = 1;', '//! ```'].join('\n')
    writeFileSync(join(crate, 'src', 'lib.rs'), `${example}\n`)
    writeFileSync(join(crate, 'src', 'main.rs'), `${example}\nfn main() {}\n`)
    writeFileSync(join(crate, 'src', 'bin', 'extra.rs'), `${example}\nfn main() {}\n`)
    writeFileSync(join(crate, 'tests', 'it.rs'), `${example}\n`)

    const derived = deriveExpectedCounts(fixtureRoot, { exceptions: [] })
    assert.equal(derived.docExamples, 1, 'only the lib target contributes a doc example')
    assert.equal(derived.expectedExecuted, 1, 'and it is the whole executed count of a crate with no #[test]')
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

// ---- architect2's three corrections, each pinned as its own test --------

test('correction 1: a target-conditional exception is withheld on the target that compiles the test and applied on the one that does not — a flat delta would under-count; an unconditional exception applies on both', () => {
  // Injected, not live. The only target-conditional entry the checked-in list
  // ever had named `chiefd-host/src/auth/peercred.rs`, which #751/P7 deleted
  // along with the pane-ancestry authentication it served; its row went in the
  // same commit, because `checkExceptionLiveness` refuses a row whose subject
  // is gone. The CAPABILITY is what this test is for, and it must keep being
  // proven whether or not the tree currently happens to exercise it — a
  // capability tested only through its one live user disappears silently the
  // day that user does.
  const appleOnly = {
    file: 'crates/chiefd-core/tests/port_provenance.rs',
    appliesWhen: (target) => target.vendor !== 'apple',
    testDelta: -1,
    blockDelta: 0,
    matchContains: 'port_provenance',
    reason: 'injected fixture for the appliesWhen capability',
    issue: '#751',
  }
  const exceptions = [appleOnly, ...KNOWN_TEST_COUNT_EXCEPTIONS]
  const linux = deriveExpectedCounts(chiefdRoot, {
    exceptions,
    target: { vendor: 'unknown', os: 'linux', triple: 'x86_64-unknown-linux-gnu' },
  })
  const apple = deriveExpectedCounts(chiefdRoot, {
    exceptions,
    target: { vendor: 'apple', os: 'macos', triple: 'aarch64-apple-darwin' },
  })
  assert.equal(linux.appliedExceptions.length, 2, 'both the conditional AND the unconditional exception apply on Linux')
  assert.equal(linux.skippedExceptions.length, 0)
  assert.equal(
    apple.appliedExceptions.length,
    1,
    'on an apple target, only the unconditional exception applies — the conditional one must be WITHHELD'
  )
  assert.equal(apple.skippedExceptions.length, 1)
  assert.equal(
    apple.expectedExecuted,
    linux.expectedExecuted + 1,
    'withholding a -1 on apple must leave the apple expectation exactly one HIGHER'
  )
})

test('correction 1: the checked-in exception list is unconditional today, and that is a fact the derivation states rather than assumes', () => {
  const linux = deriveExpectedCounts(chiefdRoot, {
    target: { vendor: 'unknown', os: 'linux', triple: 'x86_64-unknown-linux-gnu' },
  })
  const apple = deriveExpectedCounts(chiefdRoot, {
    target: { vendor: 'apple', os: 'macos', triple: 'aarch64-apple-darwin' },
  })
  assert.equal(linux.skippedExceptions.length, 0)
  assert.equal(apple.skippedExceptions.length, 0)
  assert.equal(
    apple.expectedExecuted,
    linux.expectedExecuted,
    'with no target-conditional entry left, the two targets expect the same count'
  )
})

test('correction 1: resolveTarget reads process.platform, and its output shape is what appliesWhen expects', () => {
  const target = resolveTarget()
  assert.ok(['apple', 'unknown'].includes(target.vendor))
  assert.ok(typeof target.triple === 'string' && target.triple.length > 0)
})

test('correction 2: tests/seam-fixture is NOT among the derived workspace members, even though it has its own src/lib.rs on disk — proves members come from parsing Cargo.toml\'s `members` array, not from walking the filesystem for Cargo.toml files', () => {
  const derived = deriveExpectedCounts(chiefdRoot)
  assert.ok(
    !derived.members.includes('tests/seam-fixture'),
    'seam-fixture carries its own [workspace] table and is in the root Cargo.toml\'s `exclude` — ' +
      '`cargo test --workspace` never builds it, so counting it would over-derive'
  )
})

// `tests/e2e` was the other half of this case until the chiefd-e2e crate was
// deleted with the E2E corpus. The property under test is unchanged and still
// has a real subject: a `tests/`-nested member must derive its own blocks
// rather than be swallowed as some other crate's `tests/*.rs` files.
test('correction 2: tests/unit-d IS a derived member in its own right, not swallowed as tests/*.rs files of some other crate', () => {
  const derived = deriveExpectedCounts(chiefdRoot)
  assert.ok(derived.members.includes('tests/unit-d'))
  const unitD = derived.perMember.find((m) => m.member === 'tests/unit-d')
  assert.ok(unitD && unitD.blocks > 0, 'tests/unit-d must derive its OWN blocks (its own tests/*.rs files)')
})

test('partial derivation selects one workspace member for a disk-bounded cargo test shard and keeps its exact count', () => {
  const derived = deriveExpectedCounts(chiefdRoot, { memberPaths: ['crates/identity-keys'] })
  assert.deepEqual(derived.members, ['crates/identity-keys'])
  // 7, down from 8: `keys_dir_from_orgs_root` was deleted with the orgs root
  // itself, and the two tests that existed only to hold it in step with
  // `keys_dir` collapsed into one. This number is a PIN on a real count and is
  // meant to be edited by whoever changes that count — an approximate one here
  // would stop detecting the silently-dropped test block it exists for.
  assert.equal(derived.expectedExecuted, 7)
  assert.equal(derived.expectedBlocks, 2)
  assert.equal(derived.vacuity.ok, true)
  assert.throws(
    () => deriveExpectedCounts(chiefdRoot, { memberPaths: ['crates/not-a-member'] }),
    /unknown workspace member/,
  )
})

test('the sanctioned workspace wrapper dispatches CI package groups to the shard floor runner', () => {
  const wrapper = readFileSync(workspaceTestWrapper, 'utf8')
  assert.match(wrapper, /CI_CARGO_PACKAGES/)
  assert.match(wrapper, /CI_CARGO_MEMBERS/)
  assert.match(wrapper, /exec bash "\$ROOT\/scripts\/cargo-test-workspace-shard\.sh"/)
})

test('correction 3: findHarnessDisabledTestPaths reads [[test]] harness = false from a manifest rather than assuming every tests/*.rs file is libtest-harnessed', () => {
  const cargoToml = `
[package]
name = "x"

[[test]]
name = "raw_binary_test"
path = "tests/raw_binary_test.rs"
harness = false

[[test]]
name = "normal_test"
path = "tests/normal_test.rs"
`
  assert.deepEqual(findHarnessDisabledTestPaths(cargoToml), ['tests/raw_binary_test.rs'])
})

test('correction 3: the live apps/chiefd tree has zero [[test]] harness = false targets today — a checked fact, not an assumption', () => {
  const derived = deriveExpectedCounts(chiefdRoot)
  assert.deepEqual(
    derived.harnessDisabledTests,
    [],
    'a harness=false target would invalidate this derivation\'s block-counting rule for that file — ' +
      'must be accounted for explicitly (like the apple exception) before the vacuity guard can pass'
  )
})

// ---- team-lead/architect2's correction: the exception LIST is itself a
//      hand-maintained inventory and must be liveness-checked, following
//      #902's per-line-exemption pattern — a stale entry silently corrupts
//      the derived count in the direction that HIDES a loss, which is worse
//      than an ordinary stale guard. --------------------------------------

test('checkExceptionLiveness: both live checked-in exceptions still match their file verbatim today', () => {
  const stale = checkExceptionLiveness(chiefdRoot)
  assert.deepEqual(stale, [], 'a stale exception must be caught here, in the receipt, before it silently corrupts a count')
})

test('checkExceptionLiveness fires when an exception\'s matchContains text is no longer present — the file exists, but the shape it names is gone', () => {
  const root = mkdtempSync(join(tmpdir(), 'cargo-test-derive-liveness-'))
  try {
    mkdirSync(join(root, 'crate-a', 'src'), { recursive: true })
    writeFileSync(join(root, 'crate-a', 'src', 'lib.rs'), '// nothing matching here\n')
    const exceptions = [
      { file: 'crate-a/src/lib.rs', testDelta: -1, matchContains: 'fn this_no_longer_exists' },
    ]
    const stale = checkExceptionLiveness(root, exceptions)
    assert.equal(stale.length, 1)
    assert.equal(stale[0].file, 'crate-a/src/lib.rs')
    assert.match(stale[0].reason, /no longer contains/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('checkExceptionLiveness does NOT flag an exception whose file no longer exists — that is missingExceptions\' job, not double-reported here', () => {
  const stale = checkExceptionLiveness('/does/not/exist/anywhere', [
    { file: 'nope.rs', testDelta: -1, matchContains: 'fn x' },
  ])
  assert.deepEqual(stale, [])
})

test('demonstrated red: a stale exception fails deriveExpectedCounts\'s OWN vacuity guard, refusing to trust the derivation, exactly like a harness=false surprise or a near-empty scan', () => {
  const root = mkdtempSync(join(tmpdir(), 'cargo-test-derive-liveness-vacuity-'))
  try {
    writeFileSync(join(root, 'Cargo.toml'), '[workspace]\nmembers = [\n    "crate-a",\n]\n')
    mkdirSync(join(root, 'crate-a', 'src'), { recursive: true })
    writeFileSync(join(root, 'crate-a', 'Cargo.toml'), '[package]\nname = "crate-a"\n')
    writeFileSync(join(root, 'crate-a', 'src', 'lib.rs'), '#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() {}\n}\n')
    const staleException = [
      { file: 'crate-a/src/lib.rs', testDelta: -1, matchContains: 'fn this_text_is_not_in_the_file' },
    ]
    const derived = deriveExpectedCounts(root, { exceptions: staleException })
    assert.equal(derived.vacuity.ok, false, 'a stale exception must flip the vacuity guard, not silently apply its delta')
    assert.equal(derived.vacuity.staleExceptions.length, 1)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// ---- 6. checkExact: the loss ratchet fires on drift in EITHER direction -

test('checkExact passes when observed matches the derived expectation exactly', () => {
  const output = 'test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n'
  const result = checkExact(output, 5, 1)
  assert.equal(result.ok, true)
})

test('checkExact fires when observed is BELOW the derived expectation — the original #814/#857 shape: a suite silently did not run', () => {
  const output = 'test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n'
  const result = checkExact(output, 8, 2)
  assert.equal(result.ok, false)
  assert.match(result.message, /below the derived expectation/)
})

test('checkExact fires when observed is ABOVE the derived expectation — #889\'s new failure mode: the derivation itself missed a test, and that must be a loud failure, not a silently-accepted bonus', () => {
  const output = 'test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n'
  const result = checkExact(output, 5, 1)
  assert.equal(result.ok, false)
  assert.match(result.message, /ABOVE the derived expectation/)
  assert.match(result.message, /derivation did not enumerate/)
})

test('checkExact still refuses on a skip marker even when both counts match exactly — the #862 protection is not lost by the redesign', () => {
  const output =
    'CHIEFD_E2E_SKIPPED suite="x" reason="stale"\n' +
    'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
  const result = checkExact(output, 1, 1)
  assert.equal(result.ok, false)
  assert.match(result.message, /skipped/)
})

// ---- 7. End-to-end demonstration on a synthetic tree — a guard nobody has
//         seen fail is a decoration (#0.5); this proves it fires, on a tree
//         this test fully controls rather than the live repo (which cannot
//         safely be mutated by a test run). --------------------------------

function makeSyntheticWorkspace() {
  const root = mkdtempSync(join(tmpdir(), 'cargo-test-derive-demo-'))
  writeFileSync(
    join(root, 'Cargo.toml'),
    '[workspace]\nresolver = "2"\nmembers = [\n    "crate-a",\n]\n'
  )
  mkdirSync(join(root, 'crate-a', 'src'), { recursive: true })
  mkdirSync(join(root, 'crate-a', 'tests'), { recursive: true })
  writeFileSync(join(root, 'crate-a', 'Cargo.toml'), '[package]\nname = "crate-a"\nversion = "0.1.0"\n')
  writeFileSync(
    join(root, 'crate-a', 'src', 'lib.rs'),
    '#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() {}\n\n    #[test]\n    fn two() {}\n}\n'
  )
  writeFileSync(
    join(root, 'crate-a', 'tests', 'it.rs'),
    '#[test]\nfn integration_one() {}\n'
  )
  return root
}

test('demonstrated red: deriving against a synthetic 2-crate-file tree gives blocks=3 (lib unittests + doctest + one tests/ file) and executed=3 (two inline + one integration)', () => {
  const root = makeSyntheticWorkspace()
  try {
    const derived = deriveExpectedCounts(root, { exceptions: [] })
    assert.equal(derived.expectedBlocks, 3, 'lib unittests binary + its doctest phase + tests/it.rs')
    assert.equal(derived.expectedExecuted, 3, 'two inline #[test]s + one integration #[test]')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('demonstrated red: a captured run reporting only the lib block (crate-a\'s tests/it.rs silently dropped, the #814 shape) fails checkExact against that same derivation, then passes again once the run is complete — restore proves the guard is not just permanently red', () => {
  const root = makeSyntheticWorkspace()
  try {
    const derived = deriveExpectedCounts(root, { exceptions: [] })

    const truncatedOutput = [
      'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s',
      'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s',
    ].join('\n')
    const truncatedResult = checkExact(truncatedOutput, derived.expectedExecuted, derived.expectedBlocks)
    assert.equal(truncatedResult.ok, false, 'tests/it.rs never ran — this MUST fire')
    assert.match(truncatedResult.message, /below the derived expectation/)

    const completeOutput = [
      'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s',
      'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s',
      'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s',
    ].join('\n')
    const restoredResult = checkExact(completeOutput, derived.expectedExecuted, derived.expectedBlocks)
    assert.equal(restoredResult.ok, true, 'once tests/it.rs runs, the same derived expectation is met')
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('demonstrated red: deleting one inline #[test] from source changes what the NEXT derivation expects, so a stale captured run (from before the deletion) now reads as drift ABOVE the new expectation — this is the concrete shape of "the mechanism notices a source change", not just an arithmetic check', () => {
  const root = makeSyntheticWorkspace()
  try {
    const before = deriveExpectedCounts(root, { exceptions: [] })
    assert.equal(before.expectedExecuted, 3)

    // Simulate a source-level deletion of `two()`.
    writeFileSync(
      join(root, 'crate-a', 'src', 'lib.rs'),
      '#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() {}\n}\n'
    )
    const after = deriveExpectedCounts(root, { exceptions: [] })
    assert.equal(after.expectedExecuted, 2, 'the derivation tracks the deletion immediately, no re-baseline step')

    // A run captured against the OLD (pre-deletion) source would still show
    // 3 — comparing it against the NEW derived expectation of 2 must fire,
    // exactly the way a stale/mismatched capture should.
    const staleOutput = [
      'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s',
      'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s',
      'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s',
    ].join('\n')
    const result = checkExact(staleOutput, after.expectedExecuted, after.expectedBlocks)
    assert.equal(result.ok, false)
    assert.match(result.message, /ABOVE the derived expectation/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// ---- 8. The live tree, today — a scope statement, not a pinned number ---
//
// Deliberately does NOT assert an exact number the way the old floor tests
// did (#5.3: a number in a document is a snapshot, not a fact this test
// should own) — that pinning is exactly the mechanism #889 retires. This
// only asserts the derivation's OWN vacuity guard passes against the real
// tree, and that every checked-in exception resolves against a real file
// (never how many of them APPLY on this run's platform — one is
// target-conditional by design, section 6's tests already cover that
// dimension against synthetic targets).

test('deriving against the real, live apps/chiefd tree passes its own vacuity guard and every checked-in exception resolves against a real file', () => {
  const derived = deriveExpectedCounts(chiefdRoot)
  assert.equal(derived.vacuity.ok, true, JSON.stringify(derived.vacuity))
  // #831: chiefd-locktest removed from apps/chiefd/Cargo.toml's workspace
  // members (the whole crate deleted with the rest of the last file-lock
  // code) -- a deliberate, real reduction, not drift. Floor moved 8 -> 7 to
  // match, same as any other floor in this file that tracks a real removal.
  // Then 7 -> 6 for the same reason: tests/e2e went with the E2E corpus.
  assert.ok(derived.members.length >= 6, `expected at least 6 workspace members, found ${derived.members.length}`)
  assert.deepEqual(derived.missingExceptions, [], 'no exception should cite a file that no longer exists')
  assert.equal(
    derived.appliedExceptions.length + derived.skippedExceptions.length,
    KNOWN_TEST_COUNT_EXCEPTIONS.length,
    'every exception must be resolved one way or the other (applied or withheld for this target) — none silently dropped'
  )
})
