// #984: pure parsing/enumeration logic for the "the chiefd workspace denies
// rustc warnings, in committed manifest state, and no crate opts out" guard.
// Factored out of `scripts/test/deny-warnings-lints.test.mjs` so it is
// unit-testable against private fixture directories, matching
// `scripts/chiefd-workspace-membership-lib.mjs`'s own split.
//
// WHY THIS GUARD EXISTS
// ---------------------
// The denial has to live in `apps/chiefd/Cargo.toml`'s `[workspace.lints.rust]`
// and be inherited by every member via `[lints] workspace = true`. The
// tempting alternatives — `RUSTFLAGS="-D warnings"` in a shell profile, a CI
// `env:` block, a build script — are all MACHINE-LOCAL: they hold for whoever
// set them and are invisible on a clean clone. That is the exact failure class
// this program has paid for repeatedly (a lockfile naming a deleted workspace;
// a guard demanding README text that no longer existed; a release script
// importing a deleted module — each green on a warm checkout, red on a fresh
// one). Manifest state is identical for every developer, for CI, and for both
// targets, so that is where the denial belongs and that is what this checks.
//
// Two ways the denial could be silently undone, both covered here:
//   1. The workspace table stops denying `warnings` (removed, or downgraded to
//      "warn"/"allow").
//   1b. A row in the SAME table sits at `"warn"`. This one is not obvious and
//      cost real time: `cargo check -v` shows cargo emitting
//      `--deny=warnings … --warn=missing_docs`, group flag FIRST, and rustc
//      lets the later, more specific flag win — so a `warn` row SURVIVES the
//      group deny and is enforced by nothing while reading as if it were
//      enforced. (`chiefd-host::launcher_assets` went undocumented on main
//      exactly this way: only `scripts/cargo-check-macos.sh` caught it,
//      because its `RUSTFLAGS="-D warnings"` is appended AFTER the manifest's
//      flags.) An explicit `"allow"` row is a different thing and is left
//      alone — that is an honest, committed exception; `"warn"` is a lint
//      level that does nothing.
//   2. A crate stops inheriting — a new member lands with no `[lints]` table,
//      or an existing one overrides `warnings` back down in its own
//      `[lints.rust]`, or a crate root plants `#![allow(warnings)]`/
//      `#![allow(unused)]`/`#![allow(dead_code)]`, which turns the whole crate
//      back into a warning-tolerant island while the workspace table still
//      reads as if it were denied.
//
// Filesystem-based enumeration, not a transcribed list of crates: a member
// added today is covered today, with no edit here.

import { existsSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

import { parseTomlStringArray } from './chiefd-workspace-membership-lib.mjs'

/**
 * The lint levels declared in a `[<section>]` table of a Cargo manifest's raw
 * text, as `{ <lint>: <level> }`.
 *
 * Hand-rolled rather than a real TOML parser, for the same reason
 * `parseTomlStringArray` is: this repo wires no TOML dependency at the root,
 * and lint tables are flat `name = "level"` lines (Cargo also accepts
 * `name = { level = "deny", priority = -1 }`, which is read here too).
 * Comments are stripped first so a commented-out row is never mistaken for a
 * live one.
 */
export function parseLintTable(manifestText, section) {
  const lines = manifestText.split('\n').map((line) => {
    const hashIndex = line.indexOf('#')
    return (hashIndex === -1 ? line : line.slice(0, hashIndex)).trim()
  })
  const levels = {}
  let inSection = false
  for (const line of lines) {
    if (line.startsWith('[')) {
      inSection = line === `[${section}]`
      continue
    }
    if (!inSection || line === '') continue
    const match = /^([A-Za-z0-9_:-]+)\s*=\s*(.+)$/.exec(line)
    if (!match) continue
    const [, name, rawValue] = match
    const stringValue = /^"([^"]*)"$/.exec(rawValue)
    if (stringValue) {
      levels[name] = stringValue[1]
      continue
    }
    const inlineLevel = /level\s*=\s*"([^"]*)"/.exec(rawValue)
    if (inlineLevel) levels[name] = inlineLevel[1]
  }
  return levels
}

/** True when a manifest's raw text carries a `[lints]` table with `workspace = true`. */
export function inheritsWorkspaceLints(manifestText) {
  return /(?:^|\n)\[lints\]\s*\n(?:[^[]*?\n)?\s*workspace\s*=\s*true\b/.test(manifestText)
}

/**
 * Crate-root inner attributes that would re-tolerate warnings for a whole
 * crate even while its manifest still inherits the workspace denial. Only the
 * BROAD groups are listed: a targeted `#![allow(clippy::some_specific_lint)]`
 * with a stated reason is a judgement call, but blanket `warnings`/`unused`/
 * `dead_code` at the crate root is the denial being switched off wholesale.
 */
const BLANKET_CRATE_ALLOWS = ['warnings', 'unused', 'dead_code']

/** Blanket `#![allow(...)]` groups found in a crate-root source file's text. */
export function blanketAllowsIn(sourceText) {
  const found = new Set()
  for (const match of sourceText.matchAll(/#!\s*\[\s*allow\s*\(([^)]*)\)\s*\]/g)) {
    for (const raw of match[1].split(',')) {
      const name = raw.trim()
      if (BLANKET_CRATE_ALLOWS.includes(name)) found.add(name)
    }
  }
  return [...found].sort()
}

const CRATE_ROOT_CANDIDATES = ['src/lib.rs', 'src/main.rs']

/**
 * The full check over a Cargo workspace root.
 *
 * Returns everything it enumerated rather than a bare boolean, per the
 * non-vacuity discipline the other guards here follow: a check that only says
 * pass/fail cannot be told apart from one that scanned nothing.
 *
 * - `workspaceRustLints` / `workspaceClippyLints` — the parsed workspace tables.
 * - `inertWarnRows` — `[workspace.lints.rust]` rows left at `"warn"`, which
 *   cargo's flag order lets survive the group deny (see the header).
 * - `members` — the `members = [...]` entries actually parsed.
 * - `inheriting` / `notInheriting` — members whose own manifest does (not)
 *   carry `[lints] workspace = true`.
 * - `overriding` — members that re-declare `warnings` in their own
 *   `[lints.rust]`, which would beat the inherited workspace level.
 * - `crateRootsScanned` — crate-root source files actually read.
 * - `blanketAllows` — `{ file, allows }` for any crate root switching the
 *   denial off wholesale.
 */
export function checkDenyWarnings(workspaceRoot) {
  const manifestText = readFileSync(join(workspaceRoot, 'Cargo.toml'), 'utf8')
  const members = parseTomlStringArray(manifestText, 'members')
  const workspaceRustLints = parseLintTable(manifestText, 'workspace.lints.rust')
  const workspaceClippyLints = parseLintTable(manifestText, 'workspace.lints.clippy')

  const inheriting = []
  const notInheriting = []
  const overriding = []
  const crateRootsScanned = []
  const blanketAllows = []

  for (const member of members) {
    const memberManifestPath = join(workspaceRoot, member, 'Cargo.toml')
    if (!existsSync(memberManifestPath)) {
      notInheriting.push(member)
      continue
    }
    const memberText = readFileSync(memberManifestPath, 'utf8')
    if (inheritsWorkspaceLints(memberText)) inheriting.push(member)
    else notInheriting.push(member)

    const ownRust = parseLintTable(memberText, 'lints.rust')
    if (Object.hasOwn(ownRust, 'warnings') && ownRust.warnings !== 'deny') {
      overriding.push(`${member} (warnings = "${ownRust.warnings}")`)
    }

    for (const candidate of CRATE_ROOT_CANDIDATES) {
      const rootPath = join(workspaceRoot, member, candidate)
      if (!existsSync(rootPath)) continue
      const relative = `${member}/${candidate}`
      crateRootsScanned.push(relative)
      const allows = blanketAllowsIn(readFileSync(rootPath, 'utf8'))
      if (allows.length > 0) blanketAllows.push({ file: relative, allows })
    }
  }

  const inertWarnRows = Object.entries(workspaceRustLints)
    .filter(([name, level]) => name !== 'warnings' && level === 'warn')
    .map(([name]) => name)
    .sort()

  return {
    workspaceRustLints,
    workspaceClippyLints,
    inertWarnRows,
    members,
    inheriting,
    notInheriting,
    overriding,
    crateRootsScanned,
    blanketAllows,
  }
}
