#!/usr/bin/env node
// #907: "how many root guards exist" has been a REMEMBERED number in this
// program (a "standing four" — triage-map, sql-only-state, guard-wiring,
// doc-append-only — instead of an enumeration of what actually exists), and
// that is exactly how `test:e2e-park` stayed red on canonical for six
// landings unnoticed: the guard was correct and firing, it was simply
// missing from the list of guards someone actually ran.
//
// This file is not a new check — `scripts/test/guard-wiring.test.mjs`
// already enumerates `scripts/test/*.test.mjs` from disk as its source of
// truth for "what is a guard" and asserts every one is wired-or-reasoned.
// This is that same enumeration, factored into a standalone CLI so any
// driver (an engineer's remote gate script, the merger's pre-push check, a
// receipt line) can print `DERIVED_GUARD_COUNT:<n>` and the real list
// instead of hand-typing a number into a brief or a prompt — the artifact
// this file exists to remove is a REMEMBERED count, not an unenforced one.
//
// #916: the `scripts/test/*.test.mjs` enumeration is blind to a SECOND real
// category of CI-wired gate — a plain shell script invoked directly from a
// `.github/workflows/*.yml` `run:` line (`scripts/cargo-check-macos.sh`,
// `scripts/cargo-test-workspace.sh`), or reached one hop through a
// package.json script name (`bun run typecheck` -> `bash
// scripts/typecheck.sh`). Neither lives under `scripts/test/`, so neither
// was ever visible to `deriveGuardFiles` — the merger had to hand-add
// `bash scripts/cargo-check-macos.sh` as a 25th gate when gating #884,
// because the standard matrix derived from this file would otherwise have
// gated everything except the thing that packet actually shipped. Both
// categories are now derived and returned TAGGED (`test.mjs` vs
// `shell-gate`) rather than merged into one list — #873 already ruled that
// `test:unit`/`test:ci`/`test:clean-env` are real `test:*` scripts that are
// NOT guards, so an untagged merge here would make the same mistake in the
// other direction: a reader could no longer tell "deliberately excluded"
// from "the derivation missed it".
//
// Usage: node scripts/guard-count.mjs
//   Prints DERIVED_GUARD_COUNT:<n> (test.mjs + shell-gate combined) and one
//   tagged guard per line.
//   Exits 1 if either category's enumeration looks vacuous — the same "an
//   instrument that cannot fail is worse than no instrument" posture every
//   other guard in this program takes.

import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const guardTestDir = join(here, 'test')
const repoRoot = join(here, '..')
const workflowsDir = join(repoRoot, '.github', 'workflows')
const packageJsonPath = join(repoRoot, 'package.json')

// A workspace this small carrying fewer than this many workflow files, or a
// workflow corpus with fewer than this many `run:` lines, means the SCAN
// found (near) nothing — a wrong root path, a moved `.github` directory, a
// YAML shape this scanner's line-based parse does not recognize. That is a
// vacuity failure in the instrument, not a fact about the tree, and must
// never be reported as "0 shell gates found, vacuously clean".
const MIN_PLAUSIBLE_RUN_LINES = 10

/** Every real `scripts/test/*.test.mjs` file on disk, sorted — the same
 * enumeration `scripts/test/guard-wiring.test.mjs`'s `realGuardFiles()`
 * uses, so the two can never silently disagree about what counts as a
 * guard. Kept in this file (not imported from the test file) so this CLI
 * has no dependency on `node:test`. */
export function deriveGuardFiles(dir = guardTestDir) {
  return readdirSync(dir)
    .filter((name) => name.endsWith('.test.mjs'))
    .sort()
}

/** Every `run: <command>` line across every `.github/workflows/*.yml` file,
 * as `{ workflow, line, command }`. Deliberately a line-based scan, not a
 * YAML parser: every `run:` step in this repo's workflows today is a
 * single-line scalar (`run: <command>`), never a `run: |` block scalar, so
 * a line regex is exact for the shape that exists rather than a general
 * parser for a shape that does not. If a block-scalar `run:` is ever added,
 * this function will not see the lines inside it — `vacuity` below is the
 * backstop that surfaces a collapse in what this scan can find, not a
 * silent under-count. */
export function findWorkflowRunLines(dir = workflowsDir) {
  let names
  try {
    names = readdirSync(dir).filter((name) => name.endsWith('.yml') || name.endsWith('.yaml'))
  } catch {
    return []
  }
  const runLines = []
  for (const name of names.sort()) {
    const text = readFileSync(join(dir, name), 'utf8')
    const lines = text.split('\n')
    for (let i = 0; i < lines.length; i += 1) {
      // Optional leading `- ` because a `run:` step is legal YAML either as
      // its own line under a preceding `- name: ...` sibling (this repo's
      // ci.yml style throughout) or as the list item's own first key
      // (`- run: <cmd>`, used by several of this file's own test fixtures) —
      // both compile to the identical step and must be found identically.
      const match = /^\s*-?\s*run:\s*(.+?)\s*$/.exec(lines[i])
      if (match) runLines.push({ workflow: name, line: i + 1, command: match[1] })
    }
  }
  return runLines
}

/** Parse the flat `"name": "command"` pairs out of package.json's
 * top-level `scripts` object with a targeted regex rather than a full JSON
 * parse of the whole manifest — mirrors #907's own
 * `parseWorkspaceMembers`'s "a git-grep-tier convention, not a structure
 * that hides a false positive for THIS file" reasoning. Used only to
 * resolve one hop of indirection (`bun run typecheck` -> its script body),
 * never to enumerate scripts generally. */
export function readPackageJsonScripts(path = packageJsonPath) {
  const json = JSON.parse(readFileSync(path, 'utf8'))
  return json.scripts ?? {}
}

const SHELL_INVOKE = /(?:^|\s)(?:bash|sh)\s+(scripts\/[\w./-]+\.sh)\b/
const SHELL_INVOKE_RELATIVE = /(?:^|\s)\.\/(scripts\/[\w./-]+\.sh)\b/
const RUN_SCRIPT_NAME = /^(?:bun|npm)\s+run\s+([\w:-]+)/
const BUN_TEST_MEMBER_SUITE = /^bun test ((?:apps|packages)\/[\w.-]+\/test\/[\w./-]+\.test\.ts)$/

/** Does this command text directly invoke a `scripts/*.sh` file (`bash
 * scripts/x.sh`, `sh scripts/x.sh`, `./scripts/x.sh`)? Returns the
 * repo-relative path or `undefined`. */
function directShellInvocation(command) {
  const direct = SHELL_INVOKE.exec(command) ?? SHELL_INVOKE_RELATIVE.exec(command)
  return direct ? direct[1] : undefined
}

/** Every `scripts/*.sh` file actually reachable from a CI workflow's
 * `run:` line, either directly (`run: bash scripts/x.sh`) or one hop
 * through a `package.json` script name (`run: bun run typecheck` where
 * `package.json`'s `"typecheck"` script is itself `bash scripts/x.sh`).
 * Returns `{ file, workflow, line, via }[]`, sorted by file then workflow
 * then line — `via` is the literal command CI actually runs, for a receipt
 * to quote directly rather than re-derive. */
export function deriveShellGateFiles(options = {}) {
  const runLines = options.runLines ?? findWorkflowRunLines(options.workflowsDir)
  const scripts = options.scripts ?? readPackageJsonScripts(options.packageJsonPath)
  const found = []
  for (const { workflow, line, command } of runLines) {
    const direct = directShellInvocation(command)
    if (direct) {
      found.push({ file: direct, workflow, line, via: command })
      continue
    }
    const scriptName = RUN_SCRIPT_NAME.exec(command)?.[1]
    if (scriptName === undefined) continue
    const resolved = scripts[scriptName]
    if (resolved === undefined) continue
    const indirect = directShellInvocation(resolved)
    if (indirect) found.push({ file: indirect, workflow, line, via: `${command}\` -> \`${resolved}` })
  }
  return found.sort((a, b) => a.file.localeCompare(b.file) || a.workflow.localeCompare(b.workflow) || a.line - b.line)
}

/** #977: every `bun run test:<name>` step in a workflow whose package.json
 * script is `bun test <member>/test/*.test.ts` — the class of CI-wired leg
 * `deriveGuardFiles` cannot see (it only enumerates `scripts/test/*.test.mjs`)
 * and that #977's own measurement found genuinely uncovered by the gate:
 * 11 of ci.yml's 47 `test:*` steps, none in the [test.mjs] corpus and none
 * `test:unit` itself. Excludes `test:unit` explicitly — #873 already ruled
 * it a real script that is not a guard, same reasoning as the shell-gate
 * category's own exclusion. Derived, not maintained as a name list, for the
 * same reason every other category here is: a maintained leg list is how
 * three shell gates ended up facing forty-seven CI steps unnoticed.
 *
 * The path pattern was `apps/cli/test/**` until P3 deleted that package. It is
 * now any workspace member's `test/` tree, which is what the vacuity refusal
 * below has always measured this derivation against — a narrower pattern than
 * that refusal would report "the scan broke" the first time a surviving member
 * wired such a step. */
export function deriveBunTestSuiteFiles(options = {}) {
  const runLines = options.runLines ?? findWorkflowRunLines(options.workflowsDir)
  const scripts = options.scripts ?? readPackageJsonScripts(options.packageJsonPath)
  const found = []
  for (const { workflow, line, command } of runLines) {
    const scriptName = RUN_SCRIPT_NAME.exec(command)?.[1]
    if (scriptName === undefined || scriptName === 'test:unit') continue
    const resolved = scripts[scriptName]
    if (resolved === undefined) continue
    const match = BUN_TEST_MEMBER_SUITE.exec(resolved)
    if (match) found.push({ file: match[1], scriptName, workflow, line, via: `${command}\` -> \`${resolved}` })
  }
  return found.sort((a, b) => a.file.localeCompare(b.file) || a.workflow.localeCompare(b.workflow) || a.line - b.line)
}

/** All three guard categories, tagged, sorted by category then name — the
 * combined view any driver should print or count. Never silently merges
 * the lists: #873 ruled that not every `test:*` package.json script is
 * a guard, and the same discipline applies here in both other directions —
 * a shell gate and a bun-test suite are each their own category, distinct
 * from a `scripts/test/*.test.mjs` file, not members of the same list
 * wearing a different extension.
 *
 * The return type is stated rather than inferred: inference over three
 * differently-shaped literals widens the array element to their COMMON
 * fields, so a consumer reading `invokedFrom`/`via`/`scriptName` — which
 * `gate-matrix-legs.mjs` and this file's own guard test both do — was
 * reading a property the inferred type does not have.
 * @returns {{ category: string, name: string, invokedFrom?: string, via?: string, scriptName?: string }[]}
 */
export function deriveAllGuards(options = {}) {
  const testFiles = deriveGuardFiles(options.guardTestDir).map((name) => ({
    category: 'test.mjs',
    name,
  }))
  const shellGates = deriveShellGateFiles(options).map((entry) => ({
    category: 'shell-gate',
    name: entry.file,
    invokedFrom: `${entry.workflow}:${entry.line}`,
    via: entry.via,
  }))
  const bunTestSuites = deriveBunTestSuiteFiles(options).map((entry) => ({
    category: 'bun-test-suite',
    name: entry.file,
    scriptName: entry.scriptName,
    invokedFrom: `${entry.workflow}:${entry.line}`,
    via: entry.via,
  }))
  return [...testFiles, ...shellGates, ...bunTestSuites]
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const testFiles = deriveGuardFiles()
  if (testFiles.length === 0) {
    console.error('[guard-count] REFUSING TO REPORT SUCCESS: scripts/test/ enumerated zero guard files — this is a vacuity failure in the enumeration (wrong path, an empty/moved directory), not evidence about the tree.')
    process.exit(1)
  }

  const runLines = findWorkflowRunLines()
  if (runLines.length < MIN_PLAUSIBLE_RUN_LINES) {
    console.error(`[guard-count] REFUSING TO REPORT SUCCESS: only ${runLines.length} \`run:\` lines found across .github/workflows/*.yml (expected ${MIN_PLAUSIBLE_RUN_LINES}+) — this is a vacuity failure in the workflow scan (wrong path, moved .github, or a YAML shape this line-scan cannot see), not evidence about CI.`)
    process.exit(1)
  }

  const shellGates = deriveShellGateFiles({ runLines })
  if (shellGates.length === 0) {
    console.error('[guard-count] REFUSING TO REPORT SUCCESS: zero CI-wired shell gates found under scripts/*.sh — this repo is known to wire at least 3 (typecheck.sh, cargo-check-macos.sh, cargo-test-workspace.sh); a count of zero means the scan broke, not that they were removed.')
    process.exit(1)
  }

  const bunTestSuites = deriveBunTestSuiteFiles({ runLines })
  // ZERO is a legal answer here, but only when nothing is wired.
  //
  // This used to refuse outright, citing "known to wire at least 11 (#977)".
  // That was true when written; #751's deletion of `apps/api` and
  // `apps/cli/src/legacy` took the last of them — `LauncherRunnerStdin.test.ts`
  // — along with the runner it tested, so the honest count is now 0 and a flat
  // refusal would block the check it exists to protect.
  //
  // The protection it actually provides is unchanged: zero must never mean
  // "the scan broke". So it is measured against an INDEPENDENT source of
  // truth — root package.json's own scripts — instead of a remembered number.
  const wiredBunTestScripts = Object.values(
    JSON.parse(readFileSync(packageJsonPath, 'utf8')).scripts ?? {}
  ).filter((command) => /^bun test\s+\S+\.test\.ts\b/.test(command))
  if (bunTestSuites.length === 0 && wiredBunTestScripts.length > 0) {
    console.error(`[guard-count] REFUSING TO REPORT SUCCESS: root package.json wires ${wiredBunTestScripts.length} \`bun test <file>.test.ts\` script(s) but zero CI-wired bun:test suites were derived — the scan broke, rather than the suites being removed.`)
    process.exit(1)
  }

  const total = testFiles.length + shellGates.length + bunTestSuites.length
  console.log(`DERIVED_GUARD_COUNT:${total}`)
  for (const file of testFiles) console.log(`  [test.mjs] ${file}`)
  for (const gate of shellGates) console.log(`  [shell-gate] ${gate.file} (${gate.workflow}:${gate.line} via \`${gate.via}\`)`)
  for (const suite of bunTestSuites) console.log(`  [bun-test-suite] ${suite.file} (${suite.workflow}:${suite.line} via \`${suite.via}\`)`)
}
