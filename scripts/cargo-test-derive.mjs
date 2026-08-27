// #889: derive the cargo-test loss-ratchet from the tree instead of
// transcribing it by hand. This file answers exactly one question —
// "how many test(s) across how many suite(s) does a Linux `cargo test
// --workspace` run over apps/chiefd's CURRENT source expect to produce?" —
// as a pure, testable computation over the checked-in `.rs` files, so the
// answer can never drift from the tree the way a hand-written number did
// twice in one day (#871, #879; full history in cargo-test-floor.mjs).
//
// This is deliberately a DIFFERENT instrument from cargo-test-floor.mjs's
// vacuity floor. That table, restated because it is the whole point of the
// split:
//
//                    | vacuity floor              | loss ratchet (THIS FILE)
//   question          | "did this leg check ANYTHING?" | "did we silently LOSE tests?"
//   catches            | a glob stops matching; N -> 0  | 2 tests vanish from 2379
//   correct setting    | low and wide                   | exact
//   survives deletion? | yes                             | only if derived
//
// A transcribed number tried to do both jobs and failed at both: wide enough
// that #871's two-test loss hid inside the slack, yet tight enough that
// ordinary landings made it stale again one cycle later (#879). Exactness
// has to come from derivation, not diligence — this file is that derivation;
// cargo-test-floor.mjs keeps only the (much smaller, hand-maintained-by-
// design) vacuity floor.
//
// ---------------------------------------------------------------------------
// architect2's review caught the one place a filesystem scan would have
// answered a DIFFERENT question than "what does `cargo test --workspace`
// run": `apps/chiefd/tests/seam-fixture` has its own `src/lib.rs`, so a
// scan for Cargo.toml files would count it — but it carries its own
// `[workspace]` table and is explicitly listed in the root Cargo.toml's
// `exclude = ["tests/seam-fixture"]`; `cargo test --workspace` never builds
// or runs it (it is a deliberately-failing clippy fixture, per
// TESTING.md §3.1). This is why `deriveExpectedCounts` below derives its
// crate set from parsing the root `Cargo.toml`'s `members` array
// (`parseWorkspaceMembers`) rather than walking the filesystem for
// `Cargo.toml` files — the members list IS what `--workspace` means, and a
// filesystem walk answering "what crates exist on disk" is §0.5's exact
// trap: a check that could not have distinguished the right answer from
// the wrong one. Two of those members — `tests/e2e` and `tests/unit-d` —
// are themselves full crates that happen to live under a directory named
// `tests/`; each is processed as its own top-level member with its own
// src/tests substructure, never mistaken for a `tests/*.rs` file of some
// other crate.
// ---------------------------------------------------------------------------
// Ground truth for the shape below is the REAL captured
// scripts/test/fixtures/cargo-real-clean-2360.txt, not a guess at cargo's
// behavior. Reading it end to end shows exactly three kinds of block:
//
//   "     Running unittests src/lib.rs (target/debug/deps/…)"   — one per
//     lib target, ALWAYS present if src/lib.rs exists, even with zero
//     `#[test]`s inside (confirmed: tests/e2e's own crate prints
//     "test result: ok. 0 passed" for its unittests binary).
//   "     Running unittests src/main.rs (…)" / "…src/bin/<name>.rs (…)" —
//     same, one per bin target (src/main.rs, plus one per src/bin/*.rs).
//   "     Running tests/<file>.rs (…)"                           — one per
//     `.rs` file DIRECTLY under a member's tests/ directory (not
//     recursive — a shared helper module like
//     crates/chiefd-core/tests/conformance_common/mod.rs, pulled in via
//     `mod conformance_common;` from a sibling test file, does NOT get its
//     own binary; confirmed against the real fixture, which has no
//     "Running tests/conformance_common/mod.rs" line even though both
//     conformance_activity.rs and conformance_assignment.rs `mod` it).
//   "   Doc-tests <crate>"                                       — one per
//     crate with a lib target, ALWAYS present (even with zero runnable doc
//     examples: confirmed 5 "Doc-tests <crate>: test result: ok. 0 passed"
//     blocks in the fixture, one for every lib-target crate in the tree).
//     This block always fires, so it is derived rather than assumed absent.
//
// So BLOCK COUNT is fully structural — no test-attribute enumeration is
// needed for it at all, and no macro or #[ignore] ambiguity touches it.
//
// EXECUTED COUNT is not structural. It is the sum of two enumerations:
//
//   * `#[test]`/`#[tokio::test(...)]`-attributed functions, minus the ones
//     `#[ignore]`d — scanned from source lines rather than parsed (this repo
//     uses exactly two spellings; confirmed by grepping every
//     `#[...test...]`-shaped attribute in the tree — no rstest, proptest, or
//     test_case macro dependency exists in any Cargo.toml here, so there is
//     no macro-generated test whose function body never appears as source
//     text).
//   * RUNNABLE DOC EXAMPLES — `scanFileForDocExamples` below.
//
// The second bullet was missing until #1051, and this header used to assert
// its absence: "this codebase has zero runnable doc examples today (every
// ```fenced block under a doc comment is tagged ```text or ```ignore)". That
// was true when written and #1049 made it false, adding one ```no_run
// example to `crates/chiefd-log/src/lib.rs`. A `no_run` example still
// COMPILES and still prints a passing libtest line, so the support shard
// observed 69 against a derived 68 and CI went red on a correct change —
// the same defect this file exists to prevent, in this file: an assumption
// hardcoded where a derivation belonged. The comment is fixed with the code
// because an invariant claim the code no longer holds is how the next
// reader is misled.
//
// If either enumeration ever meets something it cannot count,
// KNOWN_TEST_COUNT_EXCEPTIONS below is where the gap gets named, not a fudge
// factor here.

import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join, sep } from 'node:path'

// A workspace this small compiling to fewer than this many test attributes,
// or discovering fewer than this many members, means the DERIVATION ITSELF
// found (near) nothing — a wrong root path, an empty workspace-members
// parse, a walk that silently matched zero files. That is a vacuity failure
// in the instrument, not a fact about the tree, and must never be reported
// as "0 expected, 0 observed, exact match" (which would look like a green).
// Kept far below any plausible real count so it only fires on genuine
// collapse, mirroring cargo-test-floor.mjs's own vacuity floor.
const MIN_PLAUSIBLE_MEMBERS = 4
const MIN_PLAUSIBLE_FILES_SCANNED = 50
const MIN_PLAUSIBLE_DECLARED_TESTS = 500

/**
 * The derivation target — Cargo's `target_vendor`/`target_os` are what
 * actually gate a `#[cfg(...)]`-conditional test in or out at compile time,
 * so an exception keyed to those (not to "always subtract 1") stays correct
 * on any host. Resolved from `process.platform` rather than shelling out to
 * `rustc -vV` (this file never invokes cargo/rustc — see the header), which
 * is exact for the two vendors this program's cfg-gated code distinguishes
 * (`apple` vs everything else); a target explicitly passed in always wins.
 */
export function resolveTarget() {
  return process.platform === 'darwin'
    ? { vendor: 'apple', os: 'macos', triple: 'x86_64-apple-darwin (assumed)' }
    : { vendor: 'unknown', os: 'linux', triple: 'x86_64-unknown-linux-gnu (assumed)' }
}

// ---------------------------------------------------------------------------
// The two enumerated exceptions this repo currently needs. A named exception
// is a decision, not a hiding place — see the file:line evidence in `reason`
// before touching this array. Add an entry ONLY when static enumeration is
// provably impossible or provably wrong for a specific, named site; never to
// silence a mismatch you have not root-caused. Both entries below were found
// the same way: derive, run the real `cargo test --workspace` on a Linux
// gate host, and root-cause every delta against the actual log rather than
// accepting or rounding past it — see the receipt for the crate-by-crate
// reconciliation that isolated each one.
//
// `matchContains` — team-lead/architect2's correction: this exception LIST
// is itself exactly the hand-maintained-inventory class #889 exists to
// abolish, and for a loss ratchet a stale entry is worse than an ordinary
// stale guard, because it rots in the direction that HIDES a loss (an entry
// subtracting for a test that no longer exists silently corrupts the
// expected count downward, and a corrupted-downward expectation cannot
// distinguish itself from a healthy tree). `checkExceptionLiveness` below
// asserts every entry's premise still holds — the named file exists AND the
// exact text the exception exists for is still present verbatim — following
// #902's `GlossaryLint.test.ts` per-line-exemption pattern (content-keyed,
// asserted live, never assumed). A stale entry must fail LOUDLY, naming
// itself, not silently keep adjusting a number for a shape that is gone.
export const KNOWN_TEST_COUNT_EXCEPTIONS = [
  // The `crates/chiefd-host/src/auth/peercred.rs` entry lived here until
  // #751/P7. It subtracted one Darwin-only-cfg test, and it was the only live
  // instance of a target-conditional `appliesWhen`. P7 deleted the module it
  // named — chiefd no longer authenticates a caller by peer credentials on a
  // socket it walked to a terminal pane — so the entry went with it in the same
  // commit, which is exactly what `checkExceptionLiveness` demands of a row
  // whose subject is gone. The CAPABILITY it demonstrated is unchanged and is
  // still proven, now against an injected exception rather than a live one, by
  // `cargo-test-floor.test.mjs`'s "correction 1" pair.
  {
    file: 'crates/chiefd-core/tests/port_provenance.rs',
    // Unconditional — this is a false positive in the line-scan ITSELF, not
    // a platform-conditional compile exclusion, so it applies on every
    // target.
    testDelta: -1,
    blockDelta: 0,
    matchContains: 'fn old_path_fixture',
    reason:
      'Found empirically: a real `cargo test --workspace` on a Linux gate host measured ' +
      'exactly 2536 executed against a pre-fix derived expectation of 2537 — a crate-by-crate ' +
      'reconciliation against the real log (7 of 8 members matched exactly) isolated the gap to ' +
      'chiefd-core alone, and a per-file breakdown isolated it to this one file. Root cause: ' +
      '`retired_shape_inside_a_cfg_test_module_is_not_flagged` (mod `retired_root_fixture_tests`, ' +
      'around line 385) builds a `r#"..."#` raw-string FIXTURE containing example Rust source text ' +
      'fed to `find_retired_root_constructions` as DATA — and that fixture text happens to contain ' +
      'a `#[test]\\n    fn old_path_fixture()` sequence (line ~392-393) formatted exactly like a ' +
      'real attribute+fn pair. The line-scan below cannot distinguish "this text is Rust source ' +
      'code" from "this text is a string literal containing Rust-shaped text" without a real ' +
      'tokenizer (which was tried as a diagnostic and had its own bugs — see the receipt; a ' +
      'quick raw-string tracker is not trustworthy enough to make load-bearing, so a named, ' +
      'evidence-backed exception is the honest fix here, not a fragile tokenizer). This is exactly ' +
      'the "static enumeration is not exact" class #889 was warned to expect, just from an ' +
      'unanticipated direction (a test-scanning test\'s own fixture data) rather than a macro or a ' +
      'cfg. A scoped pathspec (excluding a directory, the way tests/seam-fixture is excluded ' +
      'structurally) was considered and rejected here: the false positive is ONE function\'s fixture ' +
      'inside a file with nine other REAL tests, so excluding the whole file would under-count those ' +
      'nine; excluding only the fixture\'s raw-string byte range would require a real Rust-string ' +
      'tokenizer, and the one built as a diagnostic for this packet already proved unreliable (false ' +
      'positives against real tests using char/escape literals elsewhere in the tree) — building an ' +
      'unreliable tokenizer INTO the derivation would be strictly worse than a named, liveness-checked ' +
      'delta. A content-keyed exception (`matchContains`, asserted live below) is the honest choice ' +
      'for this specific shape.',
    issue: '#889',
  },
]

/**
 * @typedef {{ file: string, matchContains: string, testDelta?: number, blockDelta?: number,
 *             reason?: string, issue?: string }} TestCountException
 */

/**
 * For every exception whose named `file` exists, assert its `matchContains`
 * text is still present verbatim — the bidirectional half of the exception
 * mechanism (#902's `GlossaryLint.test.ts` precedent): an exception that no
 * longer matches anything real must surface for review, not silently keep
 * adjusting a number for a shape that is gone. Returns the list of stale
 * exceptions (empty when every entry's premise still holds).
 *
 * The parameter names only the two fields this check READS. Inferring it
 * from the default argument instead would demand every field a real
 * `KNOWN_TEST_COUNT_EXCEPTIONS` row carries (`blockDelta`, `reason`,
 * `issue`) from a caller that is testing the liveness check itself.
 * @param {string} chiefdRoot
 * @param {TestCountException[]} exceptions
 */
export function checkExceptionLiveness(chiefdRoot, exceptions = KNOWN_TEST_COUNT_EXCEPTIONS) {
  const stale = []
  for (const exception of exceptions) {
    const absolutePath = join(chiefdRoot, exception.file)
    if (!exists(absolutePath)) {
      // Already reported separately as `missingExceptions` by
      // `deriveExpectedCounts` — not double-counted as "stale" here, but
      // still excluded from "live".
      continue
    }
    const text = readFileSync(absolutePath, 'utf8')
    if (!exception.matchContains || !text.includes(exception.matchContains)) {
      stale.push({
        file: exception.file,
        matchContains: exception.matchContains,
        reason: exception.matchContains
          ? `the file exists but no longer contains "${exception.matchContains}" — the shape this exception exists for is gone`
          : 'this exception has no matchContains to verify — every entry must be liveness-checkable',
      })
    }
  }
  return stale
}

const TEST_ATTR = /^\s*#\[(test|tokio::test(\([^)]*\))?)\]\s*$/
const IGNORE_ATTR = /^\s*#\[ignore(\s*=.*)?\]\s*$/
// Any line that could legitimately sit between a test attribute and the `fn`
// it decorates without ending the search: another attribute, a doc comment,
// a plain comment, or blank space. Anything else (the function signature
// itself, or unrelated code) ends the lookahead.
const ATTR_OR_COMMENT_OR_BLANK = /^\s*(#\[|\/\/|$)/
const FN_LINE = /^\s*(pub(\([^\s)]*\))?\s+)?(async\s+)?fn\s+\w/

/**
 * Scan one file's text for `#[test]`/`#[tokio::test(...)]` attribute lines
 * and, for each, whether an `#[ignore...]` attribute sits between it and the
 * `fn` it decorates. Confirmed against every real instance in this repo
 * (6 `#[ignore]`s, all immediately after their test attribute — #test then
 * #ignore then fn is the only order found; #889's plan documents the sweep).
 * Returns `{ declared, ignored }` for this one file.
 */
export function scanFileForTestAttributes(text) {
  const lines = text.split('\n')
  let declared = 0
  let ignored = 0
  for (let i = 0; i < lines.length; i += 1) {
    if (!TEST_ATTR.test(lines[i])) continue
    declared += 1
    for (let j = i + 1; j < lines.length; j += 1) {
      if (IGNORE_ATTR.test(lines[j])) {
        ignored += 1
        break
      }
      if (FN_LINE.test(lines[j])) break
      if (!ATTR_OR_COMMENT_OR_BLANK.test(lines[j])) break
    }
  }
  return { declared, ignored }
}

// The code-block attributes rustdoc understands. A fenced block whose info
// string is empty, or made only of these, is RUST and becomes a doctest;
// one carrying any other token (```text, ```json, ```console) is not Rust to
// rustdoc and produces no test at all. Listed rather than negated, because
// "everything except text" is the assumption that would silently start
// counting a ```yaml block as a test.
const RUSTDOC_CODE_ATTRS = new Set([
  'rust',
  'ignore',
  'should_panic',
  'no_run',
  'compile_fail',
  'standalone_crate',
  'edition2015',
  'edition2018',
  'edition2021',
  'edition2024',
])

const DOC_FENCE = /^\s*(\/\/\/|\/\/!)\s*```(.*)$/
const DOC_LINE = /^\s*(\/\/\/|\/\/!)/
// A line that may sit between a doc comment and the item it documents
// without ending the search: another attribute, a plain comment, or blank.
const ITEM_LEAD = /^\s*(#\[|#!\[|\/\/|$)/
// Public FROM THE CRATE ROOT. `pub(crate)`, `pub(super)` and `pub(in …)` are
// deliberately excluded: rustdoc documents only what is publicly reachable
// unless `--document-private-items` is passed, and `cargo test --doc` does
// not pass it, so a restricted item's example never runs.
const PUBLIC_ITEM = /^\s*pub(\s|\((?!crate|super|in[\s)]))/

/**
 * Count the RUNNABLE doc examples in one file's text — the second half of the
 * executed count, and the half #1049 proved was missing.
 *
 * Returns `{ declared, ignored }` with the same meaning the attribute scan
 * gives those words, so a caller adds them to the same running totals:
 * `declared` counts every fence rustdoc turns into a test, `ignored` the
 * subset tagged ```ignore, which rustdoc REPORTS but never runs (so it lands
 * outside passed+failed exactly like an `#[ignore]`d function). ```no_run and
 * ```compile_fail both count as executed: each one compiles and each one
 * prints a passing line.
 *
 * `///` fences count only on a `pub` item, because rustdoc collects nothing
 * from a private one; `//!` fences always count, being the module's own docs.
 * The limit worth stating: a `pub` item inside a private module is judged
 * public here and rustdoc does not document it, so a runnable example there
 * would be over-counted. That direction fails LOUD — the ratchet reports
 * observed BELOW expected and its message sends the reader to this file —
 * rather than hiding a lost test, and this tree has no such example today.
 */
export function scanFileForDocExamples(text) {
  const lines = text.split('\n')
  let declared = 0
  let ignored = 0
  for (let i = 0; i < lines.length; i += 1) {
    const opening = DOC_FENCE.exec(lines[i])
    if (!opening) continue
    const [, marker, info] = opening
    // Walk to the closing fence first, so a fence inside this block cannot be
    // read as a new opening and the item search starts after the whole block.
    let end = i + 1
    while (end < lines.length && !DOC_FENCE.test(lines[end])) end += 1
    const tokens = info
      .split(/[\s,]+/)
      .map((token) => token.trim().toLowerCase())
      .filter(Boolean)
    i = end
    if (!tokens.every((token) => RUSTDOC_CODE_ATTRS.has(token))) continue
    if (marker === '///' && !documentsPublicItem(lines, end + 1)) continue
    declared += 1
    if (tokens.includes('ignore')) ignored += 1
  }
  return { declared, ignored }
}

/** Does the doc comment continuing at `start` document a publicly reachable
 * item? Reads forward past the rest of the doc comment and any attributes to
 * the item's own line. */
function documentsPublicItem(lines, start) {
  for (let i = start; i < lines.length; i += 1) {
    if (DOC_LINE.test(lines[i]) || ITEM_LEAD.test(lines[i])) continue
    return PUBLIC_ITEM.test(lines[i])
  }
  return false
}

/**
 * `true` for `#[ignore...]` attribute lines that are NOT immediately
 * associated with a preceding test attribute within a few lines — a sanity
 * check that the adjacency assumption `scanFileForTestAttributes` relies on
 * still holds. An orphaned `#[ignore]` (on some other item entirely, or
 * separated from its test attribute by something unexpected) means the
 * assumption has broken and the derivation must say so rather than silently
 * under- or over-count.
 */
export function findOrphanedIgnoreAttributes(text) {
  const lines = text.split('\n')
  const orphans = []
  for (let i = 0; i < lines.length; i += 1) {
    if (!IGNORE_ATTR.test(lines[i])) continue
    let associated = false
    for (let j = i - 1; j >= Math.max(0, i - 4); j -= 1) {
      if (TEST_ATTR.test(lines[j])) {
        associated = true
        break
      }
      if (!ATTR_OR_COMMENT_OR_BLANK.test(lines[j])) break
    }
    if (!associated) orphans.push({ line: i + 1, text: lines[i].trim() })
  }
  return orphans
}

/** Recursively list every `.rs` file under `dir` (source only — there is no
 * `target/` in a fresh checkout, but it is skipped defensively in case one
 * is ever present alongside a source tree during local iteration). */
function walkRustFiles(dir) {
  const out = []
  let entries
  try {
    entries = readdirSync(dir, { withFileTypes: true })
  } catch {
    return out
  }
  for (const entry of entries) {
    if (entry.name === 'target') continue
    const full = join(dir, entry.name)
    if (entry.isDirectory()) {
      out.push(...walkRustFiles(full))
    } else if (entry.isFile() && entry.name.endsWith('.rs')) {
      out.push(full)
    }
  }
  return out
}

function exists(path) {
  try {
    statSync(path)
    return true
  } catch {
    return false
  }
}

/** `.rs` files directly (non-recursively) inside `dir`, sorted for stable
 * output — used for both the tests/ directory (one block per file) and
 * src/bin/ (one bin target per file). */
function directRustFiles(dir) {
  let entries
  try {
    entries = readdirSync(dir, { withFileTypes: true })
  } catch {
    return []
  }
  return entries
    .filter((e) => e.isFile() && e.name.endsWith('.rs'))
    .map((e) => e.name)
    .sort()
}

/** Parse the `[workspace] members = [...]` array out of the root
 * `Cargo.toml` text with a targeted regex rather than a full TOML parser —
 * apps/chiefd's own array is a flat list of quoted string literals, one per
 * line, with `#`-prefixed comment lines interleaved (around `tests/unit-d`).
 * A full TOML dependency is unwarranted for one array; this is a
 * `git grep`-tier convention, not the kind of structure that hides a false
 * positive for THIS file. Extracts every `"..."` literal per line (not just
 * the first) specifically because architect2's own miscount — a character
 * class that silently narrowed a `grep` pattern's match set — is the same
 * failure shape a "first quoted string per line" extraction would have if
 * this array were ever reformatted onto fewer lines (e.g. two members on
 * one line): a narrower pattern doesn't error, it silently returns fewer
 * members than exist. Still not a general TOML parser — a `members = [...]`
 * spanning a nested table or containing an escaped quote would defeat it —
 * but validated against the real file's actual text, not merely inspected. */
export function parseWorkspaceMembers(cargoTomlText) {
  const match = /\[workspace\][\s\S]*?members\s*=\s*\[([\s\S]*?)\]/.exec(cargoTomlText)
  if (!match) return []
  const members = []
  for (const line of match[1].split('\n')) {
    const trimmed = line.trim()
    if (!trimmed || trimmed.startsWith('#')) continue
    for (const literal of trimmed.matchAll(/"([^"]+)"/g)) {
      members.push(literal[1])
    }
  }
  return members
}

/** `true` when a crate's own `Cargo.toml` disables doctests
 * (`[lib] doctest = false`) — read from the manifest, not assumed. None in
 * this repo today (verified: `grep -rn doctest` over every nested
 * Cargo.toml under apps/chiefd has zero hits), but the derivation must not
 * assume that stays true. */
function doctestDisabled(cargoTomlText) {
  return /\[lib\][\s\S]*?doctest\s*=\s*false/.test(cargoTomlText)
}

/**
 * `[[test]] harness = false` targets are raw binaries: they never print a
 * libtest `test result: …` line at all, so a file counted by this
 * derivation's "one block per tests/*.rs file" rule would be WRONG for one
 * — over-counting a block that cargo's real output never produces (and, if
 * such a file also contains `#[test]`-shaped lines for some other reason,
 * potentially over-counting executed too, since a raw-harness binary's own
 * `main()` decides what "passed" even means). Architect2's correction:
 * verify this from the manifest rather than assume every `tests/*.rs` file
 * is libtest-harnessed. None exist in this repo today (checked directly:
 * `grep -rn harness` over every nested Cargo.toml under apps/chiefd has
 * zero real hits, only a doc-comment word) — this function is what keeps
 * that a checked fact rather than an assumption that silently rots.
 */
export function findHarnessDisabledTestPaths(cargoTomlText) {
  const disabled = []
  const testBlockRe = /\[\[test\]\]([\s\S]*?)(?=\n\[\[|\n\[[a-zA-Z]|\s*$)/g
  let match
  while ((match = testBlockRe.exec(cargoTomlText))) {
    const block = match[1]
    if (!/harness\s*=\s*false/.test(block)) continue
    const pathMatch = /path\s*=\s*"([^"]+)"/.exec(block)
    disabled.push(pathMatch ? pathMatch[1] : '(unnamed [[test]] block)')
  }
  return disabled
}

/**
 * The bin target NAMES of one member: every `[[bin]] name = "…"` in its
 * manifest, plus the auto-discovered targets cargo adds — the package name
 * for a `src/main.rs` that no `[[bin]]` claims, and one per `src/bin/*.rs`.
 * The installed name of a crate is frequently NOT its package name
 * (`chief-cli` builds `chiefd`), so this reads the table rather than
 * assuming the two agree.
 */
function binTargetNames(memberDir, cargoTomlText) {
  const names = new Set()
  const claimedPaths = new Set()
  const binBlockRe = /\[\[bin\]\]([\s\S]*?)(?=\n\[\[|\n\[[a-zA-Z]|\s*$)/g
  let match
  while ((match = binBlockRe.exec(cargoTomlText))) {
    const block = match[1]
    const name = /name\s*=\s*"([^"]+)"/.exec(block)?.[1]
    if (name) names.add(name)
    const path = /path\s*=\s*"([^"]+)"/.exec(block)?.[1]
    if (path) claimedPaths.add(path)
  }
  if (exists(join(memberDir, 'src', 'main.rs')) && !claimedPaths.has('src/main.rs')) {
    const packageName = /\[package\][\s\S]*?\bname\s*=\s*"([^"]+)"/.exec(cargoTomlText)?.[1]
    if (packageName) names.add(packageName)
  }
  for (const file of directRustFiles(join(memberDir, 'src', 'bin'))) {
    names.add(file.replace(/\.rs$/, ''))
  }
  return [...names].sort()
}

/**
 * Every Cargo test target of one workspace member, spelled the way
 * `scripts/cargo-test-workspace-shard.sh`'s `case` statement spells them:
 * `lib` (`--lib`), `bin:<name>` (`--bin <name>`), `doc` (`--doc`) and a bare
 * file stem per `tests/*.rs` (`--test <stem>`).
 *
 * This exists because that script runs ONLY the targets named in
 * `CI_CARGO_PARALLEL_TARGETS` when it shards `chief-cli`, so the workflow's
 * list IS the run set: a `tests/*.rs` file missing from it does not run in
 * CI at all. A guard comparing the workflow against a transcribed list
 * cannot see that, and needs a hand edit every time a legitimate target is
 * added (it went red on #1049's `daemon_level_log` for exactly that reason).
 * The same structural facts `deriveMember` below already reads answer the
 * question directly, so the guard reads them instead.
 */
export function deriveCargoTestTargets(memberDir) {
  const cargoTomlPath = join(memberDir, 'Cargo.toml')
  const cargoToml = exists(cargoTomlPath) ? readFileSync(cargoTomlPath, 'utf8') : ''
  const targets = []
  const hasLib = exists(join(memberDir, 'src', 'lib.rs'))
  if (hasLib) targets.push('lib')
  for (const bin of binTargetNames(memberDir, cargoToml)) targets.push(`bin:${bin}`)
  if (hasLib && !doctestDisabled(cargoToml)) targets.push('doc')
  for (const file of directRustFiles(join(memberDir, 'tests'))) {
    targets.push(file.replace(/\.rs$/, ''))
  }
  return targets
}

/**
 * Derive the expected block count and test-attribute inventory for one
 * workspace member directory. Pure function of the filesystem; no cargo
 * invocation.
 */
function deriveMember(memberDir) {
  const cargoTomlPath = join(memberDir, 'Cargo.toml')
  const cargoToml = exists(cargoTomlPath) ? readFileSync(cargoTomlPath, 'utf8') : ''

  const hasLib = exists(join(memberDir, 'src', 'lib.rs'))
  const hasMainBin = exists(join(memberDir, 'src', 'main.rs'))
  const extraBins = directRustFiles(join(memberDir, 'src', 'bin'))
  const testFiles = directRustFiles(join(memberDir, 'tests'))
  const hasDoctest = hasLib && !doctestDisabled(cargoToml)
  const harnessDisabledTests = findHarnessDisabledTestPaths(cargoToml)

  const blocks =
    (hasLib ? 1 : 0) + (hasMainBin ? 1 : 0) + extraBins.length + testFiles.length + (hasDoctest ? 1 : 0)

  // `cargo test --doc` runs the LIB target's examples only: a doc example in
  // a bin target or under tests/ is never collected, so scanning those files
  // for examples would count tests no run produces.
  const srcDir = join(memberDir, 'src')
  const binDir = join(srcDir, 'bin')
  const isLibTargetFile = (file) =>
    hasDoctest &&
    file.startsWith(`${srcDir}${sep}`) &&
    file !== join(srcDir, 'main.rs') &&
    !file.startsWith(`${binDir}${sep}`)

  const rustFiles = walkRustFiles(memberDir)
  let declared = 0
  let ignored = 0
  let docExamples = 0
  const orphanedIgnores = []
  for (const file of rustFiles) {
    const text = readFileSync(file, 'utf8')
    const scanned = scanFileForTestAttributes(text)
    declared += scanned.declared
    ignored += scanned.ignored
    if (isLibTargetFile(file)) {
      const examples = scanFileForDocExamples(text)
      declared += examples.declared
      ignored += examples.ignored
      docExamples += examples.declared
    }
    for (const orphan of findOrphanedIgnoreAttributes(text)) {
      orphanedIgnores.push({ file, ...orphan })
    }
  }

  return {
    dir: memberDir,
    hasLib,
    hasMainBin,
    extraBins: extraBins.length,
    testFiles: testFiles.length,
    hasDoctest,
    blocks,
    filesScanned: rustFiles.length,
    declared,
    ignored,
    docExamples,
    orphanedIgnores,
    harnessDisabledTests,
  }
}

/**
 * Derive the full expected `{ executed, blocks }` for a Linux `cargo test
 * --workspace` run over `chiefdRoot` (the `apps/chiefd` directory), applying
 * `exceptions` (default `KNOWN_TEST_COUNT_EXCEPTIONS`) on top of the raw
 * structural + attribute-scan derivation.
 *
 * Returns a full scope statement, not just the two numbers — #889's
 * acceptance criterion is that the check states what it enumerated and what
 * it compared, not merely that it compared correctly.
 */
export function deriveExpectedCounts(chiefdRoot, options = {}) {
  const exceptions = options.exceptions ?? KNOWN_TEST_COUNT_EXCEPTIONS
  const target = options.target ?? resolveTarget()
  const cargoTomlPath = join(chiefdRoot, 'Cargo.toml')
  const cargoToml = exists(cargoTomlPath) ? readFileSync(cargoTomlPath, 'utf8') : ''
  const workspaceMembers = parseWorkspaceMembers(cargoToml)
  const requestedMembers = options.memberPaths
  const members = requestedMembers
    ? workspaceMembers.filter((member) => requestedMembers.includes(member))
    : workspaceMembers
  if (requestedMembers) {
    const unknownMembers = requestedMembers.filter((member) => !workspaceMembers.includes(member))
    if (unknownMembers.length > 0) {
      throw new Error(`unknown workspace member(s): ${unknownMembers.join(', ')}`)
    }
    if (members.length === 0) throw new Error('memberPaths selected no workspace members')
  }

  const perMember = members.map((relPath) => ({
    member: relPath,
    ...deriveMember(join(chiefdRoot, relPath)),
  }))

  const rawBlocks = perMember.reduce((sum, m) => sum + m.blocks, 0)
  const rawDeclared = perMember.reduce((sum, m) => sum + m.declared, 0)
  const rawIgnored = perMember.reduce((sum, m) => sum + m.ignored, 0)
  const docExamples = perMember.reduce((sum, m) => sum + m.docExamples, 0)
  const filesScanned = perMember.reduce((sum, m) => sum + m.filesScanned, 0)
  const orphanedIgnores = perMember.flatMap((m) =>
    m.orphanedIgnores.map((o) => ({ member: m.member, ...o }))
  )
  const harnessDisabledTests = perMember.flatMap((m) =>
    m.harnessDisabledTests.map((path) => ({ member: m.member, path }))
  )

  const rawExecuted = rawDeclared - rawIgnored

  let executedDelta = 0
  let blocksDelta = 0
  const appliedExceptions = []
  const missingExceptions = []
  const skippedExceptions = []
  for (const exception of exceptions) {
    // `exception.file` is chiefdRoot-relative (e.g.
    // "crates/chiefd-core/tests/port_provenance.rs") regardless of which
    // workspace member it resolves under.
    if (!exists(join(chiefdRoot, exception.file))) {
      // A named exception citing a file that no longer exists is exactly
      // the erosion §2.5 warns about, aimed the other direction: an
      // exception nobody re-checks after the file it names moves or is
      // deleted. Surfaced rather than silently dropped or silently kept.
      missingExceptions.push(exception)
      continue
    }
    if (requestedMembers && !members.some((member) => exception.file === member || exception.file.startsWith(`${member}/`))) {
      continue
    }
    // `appliesWhen` defaults to "always" for an exception with no target
    // dependence. A target-conditional entry must set it: a flat delta for a
    // cfg-gated test is only correct on the targets that exclude the item, and
    // on the others it would make the derivation UNDER-count — the one
    // direction a loss ratchet must never be wrong in.
    const applies = exception.appliesWhen ? exception.appliesWhen(target) : true
    if (applies) {
      executedDelta += exception.testDelta ?? 0
      blocksDelta += exception.blockDelta ?? 0
      appliedExceptions.push(exception)
    } else {
      skippedExceptions.push(exception)
    }
  }

  const expectedExecuted = rawExecuted + executedDelta
  const expectedBlocks = rawBlocks + blocksDelta

  const staleExceptions = checkExceptionLiveness(chiefdRoot, exceptions)

  const vacuity = {
    // `harnessDisabledTests.length === 0` belongs here, not only in
    // `orphanedIgnores`-style reporting: a `[[test]] harness = false` target
    // this derivation has not seen means the block-count rule ("one block
    // per tests/*.rs file") is WRONG for that file, so the whole derivation
    // is untrustworthy until it is accounted for — the same "refuse rather
    // than silently miscount" posture as the members/files/declared floors.
    // `staleExceptions.length === 0` is the same posture applied to the
    // exception list itself: a stale entry corrupts the expected count in
    // the direction that HIDES a real loss, so it is refused exactly like a
    // vacuous scan rather than left to silently miscount.
    ok:
      (requestedMembers
        ? members.length > 0 && filesScanned > 0 && rawDeclared > 0
        : members.length >= MIN_PLAUSIBLE_MEMBERS &&
          filesScanned >= MIN_PLAUSIBLE_FILES_SCANNED &&
          rawDeclared >= MIN_PLAUSIBLE_DECLARED_TESTS) &&
      harnessDisabledTests.length === 0 &&
      staleExceptions.length === 0,
    members: members.length,
    filesScanned,
    declared: rawDeclared,
    harnessDisabledTests,
    staleExceptions,
  }

  return {
    target,
    members,
    perMember,
    filesScanned,
    declaredTests: rawDeclared,
    ignoredTests: rawIgnored,
    docExamples,
    orphanedIgnores,
    harnessDisabledTests,
    appliedExceptions,
    missingExceptions,
    skippedExceptions,
    staleExceptions,
    expectedExecuted,
    expectedBlocks,
    vacuity,
  }
}
