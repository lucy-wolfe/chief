// #857: pure parsing/comparison logic for the executed-test and
// suite-count floors, factored out of scripts/cargo-test-workspace.sh so it
// is unit-testable directly against real captured `cargo test` output
// (scripts/test/cargo-test-floor.test.mjs) rather than only exercised
// end-to-end. Mirrors scripts/assert-typecheck-nonvacuous.mjs's shape:
// exported pure functions, a thin CLI entrypoint at the bottom.

// Every crate/test-binary `cargo test` runs prints its OWN summary line,
// exactly once, in the shape:
//   test result: ok. 1208 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 9.36s
//   test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
// "executed" is passed + failed for that binary — ignored/measured/filtered
// tests never ran and must not count toward the floor.
const RESULT_LINE = /^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed;/

/**
 * Sum passed+failed across every `test result: …` line in raw `cargo test`
 * output. Returns 0 for output with no such line at all (a workspace whose
 * every crate failed to compile before any test ran) — the floor check
 * downstream is what turns that into a loud failure, not this function.
 */
export function parseExecutedCount(output) {
  let total = 0
  for (const line of output.split('\n')) {
    const match = RESULT_LINE.exec(line)
    if (match) {
      total += Number(match[1]) + Number(match[2])
    }
  }
  return total
}

/** How many `test result:` lines a run's output contains — one per
 * crate/binary test SUITE that actually ran. The merger's replay of the
 * real #814 incident found this the sharper signal: 69 vs 52 shows
 * SEVENTEEN WHOLE SUITES never ran, where the test-count delta alone reads
 * as a diffuse, harder-to-place shortfall. */
export function countResultLines(output) {
  return output.split('\n').filter((line) => RESULT_LINE.test(line)).length
}

/** `true` when `output` shows a crate that never reached `cargo test` at
 * all — a compile failure. `--no-fail-fast` does not help this case: a
 * crate that fails to build contributes no `test result:` line whether or
 * not sibling crates keep running, so the floor is the only thing that
 * catches it. */
export function hasCompileFailure(output) {
  // Deliberately NOT a bare `/^error:/` — cargo prints "error: test failed,
  // to rerun pass `-p …`" after ANY test failure, compile-related or not
  // (found the hard way against the real #814 incident log, which is pure
  // fail-fast truncation with no compile error at all: a too-permissive
  // optional-group version of this regex matched that trailer line and
  // reported a compile failure that never happened). Only a numbered rustc
  // diagnostic (`error[E0433]:`) or cargo's specific "could not compile"
  // summary line count.
  return /^error\[E\d+\]:/m.test(output) || /^error: could not compile/m.test(output)
}

// #862: the marker line this constant matches, `SKIP_BANNER_MARKER
// suite="<label>" reason="<token>"`, was originally emitted by
// `chiefd_e2e::skip_banner` in the now-deleted `apps/chiefd/tests/e2e/`
// crate (the Rust-side half of this wire contract, including its own
// `the_marker_literal_appears_in_exactly_one_place_in_production_source`
// drift test, is gone with it). No current producer emits this marker.
// Left in place as inert parsing logic (harmless: it simply never matches
// now) rather than removed outright -- `parseSkippedTests` below is
// general-purpose floor-checking machinery this file shares with every
// other Rust crate's test output, not e2e-specific in its own right, and
// removing the export blind risks breaking a consumer this pass cannot
// verify under the no-builds directive. #970-flag: unexercised call sites
// of this specific marker are now a known dead path, not silently retested.
const SKIP_BANNER_MARKER = 'CHIEFD_E2E_SKIPPED'

// Namespaced and grep-safe on purpose (#862's replacement for the original
// #857-follow-up's bare `/\bSKIPPING\b/`): a common English word risks an
// unrelated, accidental match in some other crate's own log output, which
// this specific, unlikely-to-collide token does not. Captures the two
// structured fields (`suite`, `reason`) rather than just detecting
// presence, so a refusal can name exactly which suite(s) skipped and why —
// the same "structural fact, not a boolean guess" upgrade #853/#858/#863
// all made today. Built from the SKIP_BANNER_MARKER constant above (not a
// second copy of the literal) so the one place this file names the marker
// text is the one place it could ever drift from the Rust side.
const SKIP_BANNER_LINE = new RegExp(`^${SKIP_BANNER_MARKER} suite="([^"]*)" reason="([^"]*)"$`, 'm')

/**
 * Every `{ suite, reason }` pair a `SKIP_BANNER_MARKER` line in `output`
 * names — libtest has no first-class "skipped" outcome, so a test that
 * early-returns past a precondition (e.g. #852's e2e binary-currency check)
 * still prints its own `test result: ok. 1 passed; …` line: identical to a
 * genuine pass in every signal the floors above read. Neither the executed
 * count nor the block count can tell "ran and passed" from "skipped and
 * reported passed" — this is the only thing that can, and it exists
 * specifically because a suite that skips every test still clears both
 * floors. Empty array when `output` contains no marker line at all.
 */
export function parseSkippedTests(output) {
  const skipped = []
  let match
  const re = new RegExp(SKIP_BANNER_LINE, 'gm')
  while ((match = re.exec(output))) {
    skipped.push({ suite: match[1], reason: match[2] })
  }
  return skipped
}

/** `true` when `output` names at least one skipped test — the boolean shape
 * `checkFloor` gates on; `parseSkippedTests` is what supplies the actionable
 * detail once this fires. */
export function hasSkippedTest(output) {
  return parseSkippedTests(output).length > 0
}

/**
 * Compare a run's executed count AND suite-block count against their
 * floors. Returns `{ ok: true, executed, blocks }` or
 * `{ ok: false, executed, blocks, message }` — the message names the
 * likely cause per #857's acceptance criteria (fail-fast truncation vs. a
 * compile failure), and which floor(s) were missed.
 *
 * Deliberately does NOT key on the presence of a `FAILED` result line
 * anywhere in the output: a run can fail loudly on real, fully-executed
 * content and still be complete (a genuine test failure), and a run with
 * ZERO failure lines can still be truncated (every remaining crate simply
 * never got to run). The floors are the primary signal this function
 * trusts — plus one orthogonal check neither floor can cover: a
 * `SKIP_BANNER_MARKER` line refuses the run outright, on its own,
 * independent of both counts, because a skipped-but-reported-passed test
 * clears them exactly like a real one would (#857 follow-up, the merger's
 * finding against #852; #862 made the marker structured and namespaced).
 *
 * #889: this is now used for TWO different comparisons with different
 * floor sources — see `checkExact` below for the derived, exact one. This
 * function's own `>=` semantics are what makes it the right shape for the
 * wide, hand-maintained VACUITY floor (`cargo-test-floor.mjs`'s
 * `CARGO_TEST_EXECUTED_VACUITY_FLOOR`/`CARGO_TEST_BLOCK_VACUITY_FLOOR`),
 * where "at or above" is exactly the question. It is deliberately the
 * WRONG shape for the loss ratchet, which needs exactness in both
 * directions — that is `checkExact`'s job, not this function's.
 */
export function checkFloor(output, executedFloor, blockFloor) {
  const executed = parseExecutedCount(output)
  const blocks = countResultLines(output)
  const executedShort = executed < executedFloor
  const blocksShort = blocks < blockFloor
  const skippedTests = parseSkippedTests(output)

  if (!executedShort && !blocksShort && skippedTests.length === 0) {
    return { ok: true, executed, blocks }
  }

  const lines = []
  if (skippedTests.length > 0) {
    const named = skippedTests.map((s) => `"${s.suite}" (${s.reason})`).join(', ')
    lines.push(
      `${skippedTests.length} test(s) skipped: ${named} — a precondition-gated test printed its own ` +
        '`test result: ok. 1 passed; …` line without actually running its body. libtest has no ' +
        'first-class "skipped" outcome, so this reads identically to a real pass in the executed and ' +
        'block counts above; it is refused on its own, independent of either floor. Find out why the ' +
        "precondition wasn't met (e.g. a release binary that needed rebuilding) and re-run."
    )
  }
  if (blocksShort) {
    lines.push(
      `only ${blocks} test SUITE(s) ("test result:" lines) ran, below the floor of ${blockFloor} — ` +
        'whole crates/binaries never executed at all, not a partial shortfall within one suite.'
    )
  }
  if (executedShort) {
    lines.push(`only ${executed} test(s) executed in total, below the floor of ${executedFloor}.`)
  }
  if (hasCompileFailure(output)) {
    lines.push(
      'A compile failure was found in the output (`error[E…`/`error: could not compile`) — that ' +
        'crate contributed ZERO to both counts regardless of --no-fail-fast, because its tests never ' +
        'ran at all. Fix the compile error, then re-run.'
    )
  } else {
    lines.push(
      'No compile-failure marker was found, so this is most likely fail-fast truncation from an ' +
        'earlier run WITHOUT --no-fail-fast, or a crate/binary silently dropped from the workspace ' +
        '(a members list edit, a moved Cargo.toml). Re-run with `scripts/cargo-test-workspace.sh`, ' +
        'which always passes --no-fail-fast, and read the full output for the real failure(s).'
    )
  }
  return { ok: false, executed, blocks, message: lines.join(' ') }
}

/**
 * #889: the loss-ratchet comparison — a run's executed/block counts must
 * match `expectedExecuted`/`expectedBlocks` EXACTLY, not merely clear a
 * floor. This is deliberately stricter than `checkFloor` in the upward
 * direction too: with an exact derivation, `observed > expected` is not
 * "extra tests, fine" — it means the derivation under-counted (a new test
 * spelling, an uncaught macro, a missed cfg) and must be fixed or given a
 * named exception, per #889's "fail on drift in either direction, on top
 * of the derived value" design. Silently accepting `observed > expected`
 * would let exactly this class of gap re-accumulate invisibly, which is
 * the failure this whole packet exists to close.
 *
 * Reuses every truncation/compile-failure/skip-marker signal `checkFloor`
 * already has — those causes do not change just because the target became
 * exact instead of a floor.
 */
export function checkExact(output, expectedExecuted, expectedBlocks) {
  const executed = parseExecutedCount(output)
  const blocks = countResultLines(output)
  const skippedTests = parseSkippedTests(output)
  const executedMatches = executed === expectedExecuted
  const blocksMatches = blocks === expectedBlocks

  if (executedMatches && blocksMatches && skippedTests.length === 0) {
    return { ok: true, executed, blocks, expectedExecuted, expectedBlocks }
  }

  const lines = []
  if (skippedTests.length > 0) {
    const named = skippedTests.map((s) => `"${s.suite}" (${s.reason})`).join(', ')
    lines.push(
      `${skippedTests.length} test(s) skipped: ${named} — refused independent of both counts, same as ` +
        'the vacuity check.'
    )
  }
  if (!blocksMatches) {
    const direction = blocks < expectedBlocks ? 'below' : 'ABOVE'
    lines.push(
      `${blocks} test SUITE(s) ran, ${direction} the derived expectation of ${expectedBlocks} — ` +
        (blocks < expectedBlocks
          ? 'a whole crate/binary/tests-file never executed (truncation, a compile failure, or a ' +
            'workspace-members edit the derivation has not seen).'
          : 'the tree has more compiled test targets than the derivation enumerated — a new crate, ' +
            'bin, or tests/*.rs file exists that scripts/cargo-test-derive.mjs did not count. Fix the ' +
            'derivation or the workspace-members list; do not raise a number here.')
    )
  }
  if (!executedMatches) {
    const direction = executed < expectedExecuted ? 'below' : 'ABOVE'
    lines.push(
      `${executed} test(s) executed, ${direction} the derived expectation of ${expectedExecuted} — ` +
        (executed < expectedExecuted
          ? 'tests were lost (truncation, a compile failure, or a real regression) — see the block-count ' +
            'line above and the compile-failure check below for the likely cause.'
          : 'a test attribute the derivation did not enumerate actually ran — a new test-attribute ' +
            'spelling, a macro-generated test, or a KNOWN_TEST_COUNT_EXCEPTIONS entry that no longer ' +
            'applies. Name the gap in scripts/cargo-test-derive.mjs; do not raise a number here.')
    )
  }
  if (hasCompileFailure(output)) {
    lines.push(
      'A compile failure was found in the output (`error[E…`/`error: could not compile`) — that crate ' +
        'contributed ZERO to both counts regardless of --no-fail-fast.'
    )
  } else if (blocks < expectedBlocks || executed < expectedExecuted) {
    lines.push(
      'No compile-failure marker was found, so a shortfall is most likely fail-fast truncation from a ' +
        'run WITHOUT --no-fail-fast. Re-run with scripts/cargo-test-workspace.sh.'
    )
  }
  return { ok: false, executed, blocks, expectedExecuted, expectedBlocks, message: lines.join(' ') }
}

// Executed directly by scripts/cargo-test-workspace.sh: reads a captured
// `cargo test` output file (path in argv[2]), runs BOTH checks — #889 split
// this into two independently-sourced comparisons rather than one:
//   1. the wide, hand-maintained VACUITY floor (cargo-test-floor.mjs) —
//      protects against the derivation itself (below) being fooled by a
//      wrong root or an empty walk, since that would make step 2 compare
//      two near-zero numbers and call it "exact".
//   2. the EXACT loss ratchet, derived fresh from the source tree on every
//      run (cargo-test-derive.mjs) — this is #889's actual deliverable.
// Both must pass. Exits 1 with a loud message naming which one failed and
// why, per #889's acceptance criterion that the check states what it
// enumerated and what it compared.
if (import.meta.url === `file://${process.argv[1]}`) {
  const { readFileSync } = await import('node:fs')
  const { dirname, join } = await import('node:path')
  const { fileURLToPath } = await import('node:url')
  const { CARGO_TEST_EXECUTED_VACUITY_FLOOR, CARGO_TEST_BLOCK_VACUITY_FLOOR } = await import(
    './cargo-test-floor.mjs'
  )
  const { deriveExpectedCounts } = await import('./cargo-test-derive.mjs')

  const logPath = process.argv[2]
  if (!logPath) {
    console.error('[cargo-test-floor] usage: node cargo-test-floor-lib.mjs <captured-output-file>')
    process.exit(2)
  }
  const output = readFileSync(logPath, 'utf8')

  const here = dirname(fileURLToPath(import.meta.url))
  const chiefdRoot = join(here, '..', 'apps', 'chiefd')
  const derived = deriveExpectedCounts(chiefdRoot)

  if (!derived.vacuity.ok) {
    console.error('[cargo-test-floor] REFUSING TO REPORT SUCCESS:')
    console.error(
      `  the derivation itself found implausibly little — ${derived.vacuity.members} workspace ` +
        `member(s), ${derived.vacuity.filesScanned} .rs file(s) scanned, ${derived.vacuity.declared} ` +
        'test attribute(s) declared. That is a vacuity failure in cargo-test-derive.mjs (wrong root, an ' +
        'empty workspace-members parse, a walk that silently matched nothing) — not evidence about the ' +
        'tree. Fix the derivation before trusting any comparison against it.'
    )
    if (derived.vacuity.harnessDisabledTests.length > 0) {
      console.error(
        '  Additionally, a `[[test]] harness = false` target was found that this derivation does not ' +
          `account for: ${derived.vacuity.harnessDisabledTests.map((h) => `${h.member}/${h.path}`).join(', ')}. ` +
          'That target never prints a libtest `test result:` line, so the "one block per tests/*.rs file" ' +
          'rule is wrong for it — add explicit handling in cargo-test-derive.mjs before trusting this run.'
      )
    }
    if (derived.vacuity.staleExceptions.length > 0) {
      console.error(
        '  Additionally, a KNOWN_TEST_COUNT_EXCEPTIONS entry is STALE (its named file exists but the ' +
          'text it exists for is gone) — the derivation is silently corrupted in the direction that ' +
          'HIDES a real loss until this is resolved: ' +
          derived.vacuity.staleExceptions.map((s) => `${s.file}: ${s.reason}`).join('; ') +
          '. Remove or update the exception in cargo-test-derive.mjs.'
      )
    }
    process.exit(2)
  }

  const vacuityResult = checkFloor(output, CARGO_TEST_EXECUTED_VACUITY_FLOOR, CARGO_TEST_BLOCK_VACUITY_FLOOR)
  if (!vacuityResult.ok) {
    console.error('[cargo-test-floor] REFUSING TO REPORT SUCCESS (vacuity floor):')
    console.error(`  ${vacuityResult.message}`)
    process.exit(1)
  }

  const exactResult = checkExact(output, derived.expectedExecuted, derived.expectedBlocks)
  if (!exactResult.ok) {
    console.error('[cargo-test-floor] REFUSING TO REPORT SUCCESS (derived loss ratchet):')
    console.error(`  ${exactResult.message}`)
    if (derived.appliedExceptions.length > 0) {
      console.error(
        `  Exceptions already applied: ${derived.appliedExceptions.map((e) => e.file).join(', ')}`
      )
    }
    process.exit(1)
  }

  console.log(
    `[cargo-test-floor] target ${derived.target.triple} (vendor=${derived.target.vendor}); ` +
      `enumerated ${derived.members.length} workspace member(s), ${derived.filesScanned} .rs file(s); ` +
      `${derived.declaredTests} test attribute(s) declared, ${derived.ignoredTests} #[ignore]d, ` +
      `${derived.appliedExceptions.length} named exception(s) applied` +
      (derived.skippedExceptions.length > 0
        ? ` (${derived.skippedExceptions.length} withheld for this target)`
        : '') +
      ` -> expected ${derived.expectedExecuted} executed / ${derived.expectedBlocks} suites. ` +
      `Observed ${exactResult.executed} executed / ${exactResult.blocks} suites — EXACT match. ` +
      `Vacuity floor (${CARGO_TEST_EXECUTED_VACUITY_FLOOR}/${CARGO_TEST_BLOCK_VACUITY_FLOOR}) also cleared.`
  )
}
