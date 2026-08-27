// #887: pure enumeration/parsing logic for the "every chiefd crate/test
// directory is a real workspace member or explicitly excluded" guard,
// factored out of `scripts/test/chiefd-workspace-membership.test.mjs` so it
// is unit-testable directly against a private fixture directory — matching
// `scripts/cargo-test-floor-lib.mjs`'s own split of parsing logic from the
// test file that exercises it.
//
// #871's own shape: `apps/chiefd/tests/unit-d` sat on disk with a real
// `Cargo.toml`-less package (twenty test functions, no `Cargo.toml` of its
// own at the time), absent from `apps/chiefd/Cargo.toml`'s `members`, absent
// from `exclude`, and no Rust command ever compiled it — found only by a
// count disagreeing with a count. This module answers the question directly:
// walk the filesystem for every directory that carries its own `Cargo.toml`
// under `crates/`/`tests/`, and name any that `apps/chiefd/Cargo.toml`
// itself does not account for, one way or the other.
//
// Filesystem-based enumeration, not `git ls-files`: a freshly-created,
// not-yet-committed directory is exactly the state a real instance of this
// class starts in, and `cargo build --workspace` does not care whether a
// directory is tracked — it resolves whatever is on disk. A git-based
// enumeration would silently miss the exact case this guard exists to catch
// before it is ever committed.

import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

/**
 * Every directory directly under `chiefdRoot/subdir` that contains its own
 * `Cargo.toml` — a real Cargo package the Rust toolchain would try to
 * resolve, regardless of whether `apps/chiefd/Cargo.toml` lists it. Returns
 * paths in the SAME `"<subdir>/<name>"` shape `members`/`exclude` entries
 * use, sorted for a stable, readable diff.
 */
export function cargoPackageDirs(chiefdRoot, subdir) {
  const base = join(chiefdRoot, subdir)
  if (!existsSync(base)) return []
  return readdirSync(base, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => `${subdir}/${entry.name}`)
    .filter((relative) => existsSync(join(chiefdRoot, relative, 'Cargo.toml')))
    .sort()
}

/**
 * The quoted string entries of a top-level `key = [...]` array inside a
 * Cargo.toml's raw text.
 *
 * Hand-rolled rather than a full TOML parser — this workspace has no TOML
 * dependency wired at the root today, and `apps/chiefd/Cargo.toml`'s
 * `members`/`exclude` arrays are simple, single-level, one-string-per-entry
 * lists with only line comments interspersed (see the file itself); a
 * general parser would be genuine new dependency weight for a shape this
 * narrow. Matches `scripts/test/chiefd-workspace-location.test.mjs`'s own
 * precedent of plain-text checks over this exact file rather than a real
 * TOML AST. `#`-to-end-of-line comments are stripped BEFORE extracting
 * quoted strings, so a comment mentioning a quoted path (several exist in
 * this file today) is never mistaken for a live entry.
 */
export function parseTomlStringArray(manifestText, key) {
  const withoutComments = manifestText
    .split('\n')
    .map((line) => {
      const hashIndex = line.indexOf('#')
      return hashIndex === -1 ? line : line.slice(0, hashIndex)
    })
    .join('\n')
  const arrayMatch = new RegExp(`(?:^|\\n)\\s*${key}\\s*=\\s*\\[([\\s\\S]*?)\\]`).exec(withoutComments)
  if (!arrayMatch) return []
  return [...arrayMatch[1].matchAll(/"([^"]+)"/g)].map((entryMatch) => entryMatch[1])
}

/**
 * The full check: every `Cargo.toml`-bearing directory under
 * `<chiefdRoot>/crates` and `<chiefdRoot>/tests`, cross-checked against
 * `<chiefdRoot>/Cargo.toml`'s `members`/`exclude` arrays.
 *
 * Returns `{ found, members, excluded, unlisted }` — `found` and the two
 * parsed arrays are always returned (not just a boolean) so a caller can
 * state what was actually enumerated, per #848's non-vacuity lesson: a
 * check that only reports pass/fail cannot be told apart from one that
 * silently enumerated nothing.
 */
export function checkWorkspaceMembership(chiefdRoot) {
  const manifestPath = join(chiefdRoot, 'Cargo.toml')
  const manifestText = readFileSync(manifestPath, 'utf8')
  const members = parseTomlStringArray(manifestText, 'members')
  const excluded = parseTomlStringArray(manifestText, 'exclude')
  const accounted = new Set([...members, ...excluded])

  const found = [...cargoPackageDirs(chiefdRoot, 'crates'), ...cargoPackageDirs(chiefdRoot, 'tests')]
  const unlisted = found.filter((dir) => !accounted.has(dir))

  return { found, members, excluded, unlisted }
}
