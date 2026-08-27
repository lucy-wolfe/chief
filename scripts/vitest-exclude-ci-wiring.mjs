// #923: `apps/cli/test/SupervisionOwner.test.ts` (#825) was excluded from vitest, given its
// own `test:supervision-owner` script, and wired into `ci.yml` nowhere -- verbatim #888's
// acceptance criterion, recurring five months of program-time later because the convention
// #888 established (exclude -> named script -> CI `run:` line) was never derived, only
// remembered. `test:unit` cannot see this class of gap by construction (that is what the
// exclude means), and a human running the script by hand -- which is exactly what happened
// here, 3/3, at the architect's request -- makes an unwired script look covered, not less
// unwired. This file is the derivation: for every vitest config's `exclude` list, resolve
// each entry to the `package.json` script that runs it, and assert that script is invoked in
// a workflow `run:` line.
//
// Deliberately narrow, matching the merger's pre-push `wired-check` reasoning (#923's issue
// comment): a script with no resolving vitest-exclude entry is not this guard's subject at
// all, and a resolved-but-exempted script is a named decision, not a hiding place. The three
// exemptions below are carried verbatim from the merger's own derivation (25 `test:*`
// scripts on canonical, 21 CI-invoked, 4 not -- `test:supervision-owner` was the real hole,
// now closed by this same packet's `ci.yml` change; the other three are legitimately never
// resolved by ANY vitest exclude entry today, but are kept here so a future exclude that
// happens to reuse one of those scripts does not need a fresh argument re-litigated).
//
// Fails closed: a missing/unreadable `package.json`, a missing `.github/workflows/`
// directory, or a vitest config whose `exclude` array cannot be parsed all abort rather than
// silently reporting an empty (and therefore "clean") result -- the same posture
// `cargo-test-derive.mjs`'s vacuity floor and `stub-import-guard.mjs`'s `deriveStubInventory`
// already take on this repo. Run with `node --test
// scripts/test/vitest-exclude-ci-wiring.test.mjs`.

import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join } from 'node:path'

// A vitest-exclude-ci-wiring scan finding fewer than this many vitest.config.ts files is a
// vacuity failure in the instrument (a wrong workspace root, a glob that silently matched
// nothing) -- never reported as "0 configs, 0 violations, clean". This repo has 5 vitest
// configs today (apps/web, packages/chiefing, packages/piing, packages/testing);
// kept far below that so it only fires on genuine collapse.
const MIN_PLAUSIBLE_VITEST_CONFIGS = 2

// UPDATED when the root script table was cut to an operator-facing set. The three rows this
// map used to carry all named scripts that do not exist: `test:ci:e2e` never existed on this
// tree at all (it was carried over from a derivation of a different branch), and
// `test:clean-env`/`test:unit:coverage` were deleted as unreferenced. A row excusing a script
// nobody can invoke excuses nothing, which is a stale row (Mandate 0), so they are removed
// rather than kept "just in case a future exclude reuses the name" — a future exclude gets a
// fresh, real argument, which is cheaper than reading a lie today.
//
// EMPTY is the correct state right now, and the MECHANISM stays: every vitest config's
// `exclude` list is `[]` on this tree, so the guard resolves nothing and has nothing to
// exempt. The next genuinely-exempt script gets one row here plus its reason, instead of
// being silently unwired.
export const EXEMPT_SCRIPTS = {}

function exists(path) {
  try {
    statSync(path)
    return true
  } catch {
    return false
  }
}

function readJson(path) {
  return JSON.parse(readFileSync(path, 'utf8'))
}

/** Workspace member directories from `package.json`'s `workspaces` globs (`apps/*`,
 * `packages/*` in this repo) -- a directory listing filtered to real dirs, not a glob
 * library, matching this repo's other derivations' preference for a targeted scan over a
 * dependency. Throws if `workspaces` is missing or resolves to zero directories: a
 * derivation that silently found nothing is not a derivation that found nothing to report. */
export function resolveWorkspaceMemberDirs(repoRoot, pkg) {
  const patterns = Array.isArray(pkg.workspaces) ? pkg.workspaces : []
  if (patterns.length === 0) {
    throw new Error(
      `vitest-exclude-ci-wiring: package.json has no "workspaces" array -- cannot derive ` +
        `member directories at all (refusing rather than scanning nothing and reporting clean)`
    )
  }
  const dirs = []
  for (const pattern of patterns) {
    const match = /^([^*]+)\*$/.exec(pattern)
    if (!match) {
      throw new Error(
        `vitest-exclude-ci-wiring: unrecognized workspaces pattern "${pattern}" -- expected ` +
          `a "<dir>/*" glob (this repo's only shape today); refusing to guess`
      )
    }
    const parent = join(repoRoot, match[1])
    let entries
    try {
      entries = readdirSync(parent, { withFileTypes: true })
    } catch {
      continue
    }
    for (const entry of entries) {
      if (entry.isDirectory()) dirs.push(join(parent, entry.name))
    }
  }
  return dirs.sort()
}

/** Strip a `//` comment from one line, wherever it starts -- full-line ("// ...") or trailing
 * after code ("'x.test.ts', // ..."). Tracks single/double-quote state left to right so a
 * `//` inside an actual string literal (never happens in this array today, but the scan does
 * not assume it) is not mistaken for a comment start; the first UNQUOTED `//` ends the line. */
function stripTrailingComment(line) {
  let inSingle = false;
  let inDouble = false;
  for (let i = 0; i < line.length; i += 1) {
    const ch = line[i];
    if (ch === "'" && !inDouble) inSingle = !inSingle;
    else if (ch === '"' && !inSingle) inDouble = !inDouble;
    else if (!inSingle && !inDouble && ch === '/' && line[i + 1] === '/') return line.slice(0, i);
  }
  return line;
}

/** Strip every `//` comment -- full-line AND trailing -- before any bracket/quote scanning, so
 * a comment's own punctuation (an apostrophe, a stray `]`) can never be mistaken for a real
 * string-literal delimiter or the array's own close bracket. An apostrophe in a comment like
 * "this file's `mutate()`" previously made the quote-literal regex below start a fake string
 * at that apostrophe and run to the next real `'`, silently swallowing every entry after it
 * into an unresolvable needle (#950/#954 caught this: a poisoned parse that still happens to
 * report SOME violations reads as a real failure, but a poisoned parse that happens to match
 * its expected count would report a clean corpus it never actually scanned -- worse than a
 * red). Handling TRAILING comments too (not just full-line ones) matters because nothing in
 * this repo enforces "every comment is its own line" -- a fix that only covered the
 * convention, not the syntax, would have left the identical defect one `// trailing note`
 * away. */
function stripLineComments(text) {
  return text.split('\n').map(stripTrailingComment).join('\n');
}

/** Extract the quoted string literals inside a vitest config's `exclude: [...]` array via a
 * targeted regex, matching `cargo-test-derive.mjs`'s `parseWorkspaceMembers` precedent (a
 * `git grep`-tier convention array, not a shape that warrants a full TS/AST parser). Returns
 * `null` (not `[]`) when the config has no `exclude` key at all -- distinct from "exclude
 * present but empty" -- so callers do not conflate "nothing excluded" with "couldn't find the
 * key". Throws if an `exclude:` key is present but the array cannot be closed (malformed
 * config) rather than silently returning a partial list. */
export function parseViteExcludeList(configText) {
  const stripped = stripLineComments(configText)
  const keyMatch = /exclude\s*:\s*\[/.exec(stripped)
  if (!keyMatch) return null
  const start = keyMatch.index + keyMatch[0].length
  const close = stripped.indexOf(']', start)
  if (close === -1) {
    throw new Error(
      `vitest-exclude-ci-wiring: found "exclude: [" with no closing "]" -- malformed vitest ` +
        `config, refusing to parse a partial exclude list`
    )
  }
  const body = stripped.slice(start, close)
  const entries = []
  for (const literal of body.matchAll(/'([^']+)'|"([^"]+)"/g)) {
    entries.push(literal[1] ?? literal[2])
  }
  return entries
}

/** The `package.json` script name whose command string invokes `<memberRelPath>/<entry>`
 * exactly (e.g. "apps/cli/test/SupervisionOwner.test.ts"), or `undefined` if no script wraps
 * it at all -- mirrors `guard-wiring.test.mjs`'s `scriptNameForGuardFile`. */
function scriptNameForExcludedFile(scripts, needle) {
  for (const [name, command] of Object.entries(scripts)) {
    if (typeof command === 'string' && command.includes(needle)) return name
  }
  return undefined
}

/** Whole-script-name match against `bun run <scriptName>` in workflow text, same word-boundary
 * guard as `guard-wiring.test.mjs`'s `isInvokedInWorkflows` (so `test:ci` does not
 * accidentally match a workflow line invoking `test:ci:e2e`). */
function isInvokedInWorkflows(workflowText, scriptName) {
  const pattern = new RegExp(`bun run ${scriptName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?![\\w:-])`)
  return pattern.test(workflowText)
}

/**
 * Derive every vitest-exclude entry across every workspace member's `vitest.config.ts`,
 * resolve each to a `package.json` script, and validate CI wiring. Pure function of
 * (memberDirs, repoRoot, scripts, workflowText, exemptScripts, minPlausibleConfigs) -> a full
 * scope statement (what was checked) plus `violations` -- the shape `deriveExpectedCounts`
 * and `validateGuardWiring` both already use on this repo, so the transcript proves the
 * subject rather than asking a reader to trust it.
 *
 * `minPlausibleConfigs` defaults to `MIN_PLAUSIBLE_VITEST_CONFIGS` (the real repo's vacuity
 * floor) but is overridable so a deliberately narrow test fixture -- one throwaway member
 * directory, not the whole workspace -- can assert its own behavior without tripping the
 * real-repo floor; the floor's job is to catch a scan that silently found nothing where it
 * expected the real workspace, not to forbid a unit test from constructing a smaller world.
 */
export function deriveVitestExcludeWiring(
  memberDirs,
  repoRoot,
  scripts,
  workflowText,
  exemptScripts = EXEMPT_SCRIPTS,
  minPlausibleConfigs = MIN_PLAUSIBLE_VITEST_CONFIGS
) {
  const checkedConfigs = []
  const resolved = []
  const violations = []

  for (const memberDir of memberDirs) {
    const configPath = join(memberDir, 'vitest.config.ts')
    if (!exists(configPath)) continue
    const memberRel = memberDir.slice(repoRoot.length + 1)
    checkedConfigs.push(memberRel)

    const configText = readFileSync(configPath, 'utf8')
    const excludeList = parseViteExcludeList(configText)
    if (excludeList === null) continue

    for (const entry of excludeList) {
      const needle = `${memberRel}/${entry}`
      const scriptName = scriptNameForExcludedFile(scripts, needle)
      if (!scriptName) {
        violations.push(
          `${needle} is excluded from vitest (${memberRel}/vitest.config.ts) but no ` +
            `package.json script invokes it -- it cannot be run by name, by a human or by ` +
            `CI (#923)`
        )
        continue
      }

      const exemptReason = exemptScripts[scriptName]
      if (exemptReason) {
        resolved.push({ path: needle, scriptName, status: 'exempt', reason: exemptReason })
        continue
      }

      const wired = isInvokedInWorkflows(workflowText, scriptName)
      if (!wired) {
        violations.push(
          `${needle} is excluded from vitest (${memberRel}/vitest.config.ts), scripted as ` +
            `"${scriptName}", but no .github/workflows/*.yml file invokes "bun run ` +
            `${scriptName}" -- it runs nowhere in CI (#923, the exact #888 defect recurring)`
        )
        resolved.push({ path: needle, scriptName, status: 'unwired' })
        continue
      }

      resolved.push({ path: needle, scriptName, status: 'wired' })
    }
  }

  if (checkedConfigs.length < minPlausibleConfigs) {
    throw new Error(
      `vitest-exclude-ci-wiring: found only ${checkedConfigs.length} vitest.config.ts file(s) ` +
        `under workspace members -- expected at least ${minPlausibleConfigs}. This is ` +
        `a vacuity failure in the scan (wrong root, workspaces glob matched nothing), not a ` +
        `fact about the tree; refusing to report a result. Checked: ${JSON.stringify(checkedConfigs)}`
    )
  }

  return { checkedConfigs, resolved, violations }
}

/** Read the concatenation of every `.github/workflows/*.yml`/`.yaml` file, matching
 * `guard-wiring.test.mjs`'s `readWorkflowFiles`. Throws (does not return `''`) when the
 * directory itself is missing -- an empty workflows dir would make every script look unwired,
 * which is a fact about the read failing, not about the tree. */
export function readWorkflowFiles(repoRoot) {
  const workflowsDir = join(repoRoot, '.github', 'workflows')
  let names
  try {
    names = readdirSync(workflowsDir).filter((name) => name.endsWith('.yml') || name.endsWith('.yaml'))
  } catch (err) {
    throw new Error(
      `vitest-exclude-ci-wiring: cannot read .github/workflows/ (${err.message}) -- refusing ` +
        `to treat that as "nothing is wired"`
    )
  }
  if (names.length === 0) {
    throw new Error('vitest-exclude-ci-wiring: .github/workflows/ contains zero workflow files')
  }
  return names.map((name) => readFileSync(join(workflowsDir, name), 'utf8')).join('\n---\n')
}

// ---------------------------------------------------------------------------
// CLI entry point -- prints what it operated on before the verdict, then the
// verdict, and exits non-zero on any violation. Never `echo`s a fact next to
// an unasserted decision (#0.6): the exit code is the only output that
// governs anything, but the printed subject lets a reader confirm it against
// the transcript rather than take the exit code's word for it.
// ---------------------------------------------------------------------------
function main() {
  const repoRoot = join(import.meta.dirname, '..')
  const pkg = readJson(join(repoRoot, 'package.json'))
  const memberDirs = resolveWorkspaceMemberDirs(repoRoot, pkg)
  const workflowText = readWorkflowFiles(repoRoot)

  const result = deriveVitestExcludeWiring(memberDirs, repoRoot, pkg.scripts, workflowText)

  console.log(`vitest-exclude-ci-wiring: checked ${result.checkedConfigs.length} vitest config(s): ${result.checkedConfigs.join(', ')}`)
  console.log(`vitest-exclude-ci-wiring: resolved ${result.resolved.length} excluded test(s):`)
  for (const r of result.resolved) {
    console.log(`  ${r.path} -> ${r.scriptName} [${r.status}${r.reason ? `: ${r.reason}` : ''}]`)
  }

  if (result.violations.length > 0) {
    console.error(`vitest-exclude-ci-wiring: ${result.violations.length} violation(s):`)
    for (const v of result.violations) console.error(`  - ${v}`)
    process.exit(1)
  }

  console.log('vitest-exclude-ci-wiring: clean -- every vitest-excluded test resolves to a CI-wired or exempted script')
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main()
}
