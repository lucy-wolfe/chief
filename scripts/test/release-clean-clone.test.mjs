// `bun run release` is the ONE documented way to install chiefd and beacond,
// and it is the one command no contributor ever runs, because everyone who
// could run it already has a working tree. Commit ca2da9b57 deleted
// `apps/api` and `apps/cli/src/legacy` and left three references behind; each
// one, on its own, was fatal to `bun run release` from a clean clone, and all
// three were invisible on every machine that already had the repo — the
// deleted files still sat in those working trees, unstaged and untracked, so
// the imports resolved and the workspace entries pointed at real directories.
//
// That is the class this guard closes, and the class is NOT "these three
// names". It is: A DELETION LEFT A DANGLING REFERENCE THAT ONLY A CLEAN CLONE
// CAN SEE. So nothing below is written against `apps/api`, `src/legacy` or
// `chiefd-e2e` — every check derives its subject from the tree and then asks
// whether the reference still points at something that exists.
//
// Three references, three checks:
//
//   1. `scripts/release-chiefd.ts` (and everything it transitively pulls in
//      from `scripts/`) must import nothing out of `apps/`, `packages/` or
//      `@chief/*`. `bun install --frozen-lockfile` links the workspace but
//      does NOT build it, so a `@chief/*` package resolves to a directory
//      whose `dist/` does not exist yet; and a relative reach into `apps/`
//      is a build script depending on the application it builds. Either way
//      the script cannot LOAD, failing at module resolution before it does
//      any work at all.
//   2. Every workspace path `bun.lock` names must exist on disk. A stale
//      entry makes the release entry's forced frozen install fail outright.
//   3. Every package in `apps/chiefd/Cargo.lock` with no `source =` line is
//      a PATH dependency, i.e. a workspace member of `apps/chiefd`. Each one
//      must resolve to a real member directory declaring that package name,
//      or the cargo build the release script drives cannot start.
//
// Every check carries a non-vacuity floor: a scan that finds nothing must
// FAIL, never pass. That matters more here than usual, because all three
// subjects are parsed out of files with regexes — a parser that quietly
// stops matching would otherwise turn this guard green forever.
//
// Run with `node --test scripts/test/release-clean-clone.test.mjs`.

import assert from 'node:assert/strict'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')

const RELEASE_ENTRY = 'scripts/release-chiefd.ts'
const BUN_LOCK = 'bun.lock'
const CARGO_LOCK = join('apps', 'chiefd', 'Cargo.lock')
const CHIEFD_MANIFEST = join('apps', 'chiefd', 'Cargo.toml')

// ---------------------------------------------------------------------------
// 1. The pre-install module graph
// ---------------------------------------------------------------------------

// Blank out comments and string bodies so a module specifier is only ever
// read from real code. Written as a state walk rather than a regex because
// the alternative fails in both directions on this exact file: a `//` inside
// a string literal ends the "line comment" a regex thinks it found, and
// `release-chiefd.ts`'s own doc comment NAMES the deleted module it used to
// import — a comment-blind scan would report the defect against the fix.
export function stripCommentsAndStrings(source) {
  const out = []
  let i = 0
  while (i < source.length) {
    const two = source.slice(i, i + 2)
    if (two === '//') {
      while (i < source.length && source[i] !== '\n') { out.push(' '); i += 1 }
      continue
    }
    if (two === '/*') {
      while (i < source.length && source.slice(i, i + 2) !== '*/') { out.push(source[i] === '\n' ? '\n' : ' '); i += 1 }
      out.push('  ')
      i += 2
      continue
    }
    const ch = source[i]
    if (ch === '"' || ch === "'" || ch === '`') {
      // Keep the quotes and the body: the specifier extractor below reads
      // the body of exactly these literals. Only NESTED comment-like text is
      // protected here, by virtue of being consumed as string content.
      out.push(ch)
      i += 1
      while (i < source.length && source[i] !== ch) {
        if (source[i] === '\\') { out.push(source[i], source[i + 1] ?? ''); i += 2; continue }
        out.push(source[i])
        i += 1
      }
      out.push(ch)
      i += 1
      continue
    }
    out.push(ch)
    i += 1
  }
  return out.join('')
}

// Every module specifier the file names: static `import`/`export ... from`,
// bare side-effect `import "x"`, dynamic `import("x")`, and `require("x")`.
export function collectModuleSpecifiers(source) {
  const code = stripCommentsAndStrings(source)
  const found = []
  const patterns = [
    /\bimport\s+(?:type\s+)?(?:[^'"`;()]*?\s+from\s+)?['"]([^'"]+)['"]/g,
    /\bexport\s+(?:type\s+)?[^'"`;()]*?\s+from\s+['"]([^'"]+)['"]/g,
    /\bimport\s*\(\s*['"]([^'"]+)['"]\s*\)/g,
    /\brequire\s*\(\s*['"]([^'"]+)['"]\s*\)/g,
  ]
  for (const pattern of patterns) {
    for (const match of code.matchAll(pattern)) found.push(match[1])
  }
  return [...new Set(found)]
}

const RESOLUTION_CANDIDATES = ['', '.ts', '.mts', '.cts', '.tsx', '.js', '.mjs', '.cjs', '/index.ts', '/index.js']

function resolveRelative(root, fromFileRel, specifier) {
  const base = resolve(root, dirname(fromFileRel), specifier)
  for (const suffix of RESOLUTION_CANDIDATES) {
    const candidate = base + suffix
    if (!existsSync(candidate)) continue
    if (suffix === '' && statSync(candidate).isDirectory()) continue
    return relative(root, candidate).split(sep).join('/')
  }
  return undefined
}

/**
 * Walk the module graph rooted at `entryRel`, following only relative
 * specifiers, and report every reference that a clean clone could not
 * satisfy before `bun install` has linked AND built the workspace.
 *
 * `workspacePackageNames` is derived from bun.lock by the caller rather than
 * hardcoded, so a newly added workspace package is covered the day it is
 * added and a removed one stops being asserted about.
 *
 * Returns { visited, specifiers, violations } — the first two exist so the
 * caller can prove the walk was not vacuous, which is the whole difference
 * between this passing and this being green.
 */
export function auditPreInstallGraph(root, entryRel, workspacePackageNames) {
  const violations = []
  const specifiers = []
  const visited = []
  const queue = [entryRel]
  const seen = new Set(queue)

  while (queue.length > 0) {
    const fileRel = queue.shift()
    const absolute = join(root, fileRel)
    if (!existsSync(absolute)) {
      violations.push(`${fileRel} does not exist on disk`)
      continue
    }
    visited.push(fileRel)
    for (const specifier of collectModuleSpecifiers(readFileSync(absolute, 'utf8'))) {
      specifiers.push({ file: fileRel, specifier })
      if (specifier.startsWith('node:')) continue

      if (!specifier.startsWith('.')) {
        if (workspacePackageNames.includes(specifier) || specifier.split('/')[0] === '@chief') {
          violations.push(
            `${fileRel} imports the workspace package "${specifier}". ${RELEASE_ENTRY} runs before the ` +
              `workspace is BUILT (\`bun install --frozen-lockfile\` links it, it does not compile it), so ` +
              `this cannot resolve on a clean clone.`
          )
        }
        continue
      }

      const resolved = resolveRelative(root, fileRel, specifier)
      if (resolved === undefined) {
        violations.push(
          `${fileRel} imports "${specifier}", which resolves to nothing on disk — a deletion left a dangling ` +
            `reference that only a clean clone can see.`
        )
        continue
      }
      if (resolved.startsWith('apps/') || resolved.startsWith('packages/')) {
        violations.push(
          `${fileRel} imports "${specifier}" -> ${resolved}. A release script must not reach into apps/ or ` +
            `packages/: it runs before the workspace is built, and it is the thing that builds it.`
        )
        continue
      }
      if (!seen.has(resolved)) {
        seen.add(resolved)
        queue.push(resolved)
      }
    }
  }

  return { visited, specifiers, violations }
}

// ---------------------------------------------------------------------------
// 2. bun.lock's workspace paths
// ---------------------------------------------------------------------------

// The `"workspaces"` block, keyed by path.
export function workspacePathsFromBunLock(text) {
  return [...text.matchAll(/^\s{4}"((?:apps|packages)\/[^"]+)":/gm)].map((m) => m[1]).sort()
}

// The resolution block's `"@chief/x": ["@chief/x@workspace:apps/y"]` entries —
// an INDEPENDENT statement of the same fact, in a different part of the same
// file. Cross-checking the two is what makes a half-updated lock (one block
// edited, the other not) fail rather than pass.
export function workspacePackagesFromBunLock(text) {
  return [...text.matchAll(/"([^"]+)":\s*\["[^"]+@workspace:([^"]+)"\]/g)]
    .map((m) => ({ name: m[1], path: m[2] }))
    .sort((a, b) => a.name.localeCompare(b.name))
}

// ---------------------------------------------------------------------------
// 3. apps/chiefd/Cargo.lock's path dependencies
// ---------------------------------------------------------------------------

// A `[[package]]` block with no `source =` line is a path dependency: cargo
// expects to find it in the workspace, on disk.
export function pathPackagesFromCargoLock(text) {
  const names = []
  for (const block of text.split('[[package]]').slice(1)) {
    if (/^\s*source\s*=/m.test(block)) continue
    const name = /^\s*name\s*=\s*"([^"]+)"/m.exec(block)
    if (name) names.push(name[1])
  }
  return names.sort()
}

// Declared members of the apps/chiefd workspace, each mapped to the package
// name its own Cargo.toml declares. Deliberately NOT a `crates/*` glob: today
// `chiefd-unit-d` is a member living under `tests/unit-d`, and a guard that
// assumed the directory layout would fail on a correct tree.
export function workspaceMemberPackages(root) {
  const manifest = readFileSync(join(root, CHIEFD_MANIFEST), 'utf8')
  const membersBlock = /members\s*=\s*\[([\s\S]*?)\]/.exec(manifest)
  if (!membersBlock) return []
  const packages = []
  for (const match of membersBlock[1].matchAll(/"([^"]+)"/g)) {
    const memberDir = match[1]
    const memberManifest = join(root, 'apps', 'chiefd', memberDir, 'Cargo.toml')
    if (!existsSync(memberManifest)) {
      packages.push({ dir: memberDir, name: undefined })
      continue
    }
    const name = /^\s*name\s*=\s*"([^"]+)"/m.exec(readFileSync(memberManifest, 'utf8'))
    packages.push({ dir: memberDir, name: name ? name[1] : undefined })
  }
  return packages
}

// ===========================================================================
// The real tree, today
// ===========================================================================

test('the release script\'s module graph imports nothing from apps/, packages/ or @chief/*', () => {
  const lock = readFileSync(join(repoRoot, BUN_LOCK), 'utf8')
  const names = workspacePackagesFromBunLock(lock).map((p) => p.name)
  const { violations } = auditPreInstallGraph(repoRoot, RELEASE_ENTRY, names)
  assert.deepEqual(violations, [], `\n  - ${violations.join('\n  - ')}`)
})

test('the release-script scan is not vacuous — it read the real file and found real specifiers', () => {
  const lock = readFileSync(join(repoRoot, BUN_LOCK), 'utf8')
  const names = workspacePackagesFromBunLock(lock).map((p) => p.name)
  const { visited, specifiers } = auditPreInstallGraph(repoRoot, RELEASE_ENTRY, names)
  assert.ok(visited.includes(RELEASE_ENTRY), `the walk never visited ${RELEASE_ENTRY} — the scan is broken, not clean`)
  assert.ok(
    specifiers.length > 3,
    `found only ${specifiers.length} module specifiers in the release graph — the extractor has stopped matching, ` +
      `and an extractor that finds nothing reports every tree as clean`
  )
  assert.ok(
    names.length > 3,
    `derived only ${names.length} workspace package names from bun.lock — the bare-specifier half of this check ` +
      `has nothing to compare against`
  )
})

test('every workspace path bun.lock names exists on disk', () => {
  const lock = readFileSync(join(repoRoot, BUN_LOCK), 'utf8')
  const declared = workspacePathsFromBunLock(lock)
  assert.ok(
    declared.length > 3,
    `bun.lock declared only ${declared.length} workspace paths — the parse is broken, not the lock`
  )
  const missing = declared.filter((p) => !existsSync(join(repoRoot, p)))
  assert.deepEqual(
    missing,
    [],
    `bun.lock names workspace path(s) that do not exist: ${missing.join(', ')}. \`bun run release\` ` +
      `fails during its forced frozen install on a clean clone.`
  )
})

test('bun.lock states its workspace set twice, and the two statements agree', () => {
  const lock = readFileSync(join(repoRoot, BUN_LOCK), 'utf8')
  const fromWorkspaces = workspacePathsFromBunLock(lock)
  const fromResolutions = workspacePackagesFromBunLock(lock)
  assert.ok(fromResolutions.length > 3, `derived only ${fromResolutions.length} @workspace: resolutions — the parse is broken`)
  assert.deepEqual(
    fromResolutions.map((p) => p.path).sort(),
    fromWorkspaces,
    'the "workspaces" block and the "@workspace:" resolution entries disagree — the lock is half-updated'
  )
  for (const { name, path } of fromResolutions) {
    assert.ok(existsSync(join(repoRoot, path)), `bun.lock resolves ${name} to ${path}, which does not exist`)
  }
})

test('every path dependency in apps/chiefd/Cargo.lock is a real, declared workspace member', () => {
  const lock = readFileSync(join(repoRoot, CARGO_LOCK), 'utf8')
  const pathPackages = pathPackagesFromCargoLock(lock)
  assert.ok(
    pathPackages.length > 3,
    `apps/chiefd/Cargo.lock declared only ${pathPackages.length} source-less packages — the block parse is broken`
  )
  const members = workspaceMemberPackages(repoRoot)
  assert.ok(members.length > 3, `apps/chiefd/Cargo.toml declared only ${members.length} members — the members parse is broken`)

  const unbackedMembers = members.filter((m) => m.name === undefined)
  assert.deepEqual(
    unbackedMembers.map((m) => m.dir),
    [],
    `apps/chiefd/Cargo.toml lists member(s) with no readable Cargo.toml on disk: ${unbackedMembers.map((m) => m.dir).join(', ')}`
  )

  const memberNames = members.map((m) => m.name)
  const orphans = pathPackages.filter((name) => !memberNames.includes(name))
  assert.deepEqual(
    orphans,
    [],
    `apps/chiefd/Cargo.lock names path package(s) with no workspace member on disk: ${orphans.join(', ')}. ` +
      `Cargo refuses to build against a lock naming a crate that is not there, so the release build cannot start ` +
      `on a clean clone — while a working tree that still carries the deleted crate builds fine.`
  )
})

// ===========================================================================
// Demonstrated red: each check fails on a doctored fixture
// ===========================================================================

// Every relative specifier written into a fixture below uses a deliberately
// synthetic last path segment (`fixture-cli`, `fixture-entry`, `gone-paths`,
// ...) and never a real one such as an "index" or "paths" segment. That is
// not style. `scripts/coverage-scope-gap.mjs` decides whether a file is
// "referenced by something in the tree" with a TEXT scan over every relative
// import specifier under `scripts/` and `tests/`, matched on the LAST PATH
// SEGMENT alone — it does not care whether the specifier sits in real code, a
// fixture string, or a comment. So a fixture whose specifier ends in an
// "index" segment silently makes every uncovered `index.ts` in the repo look
// referenced, and `test:coverage-scope-gap` then fails naming a file this
// guard never touched. Observed twice on this packet against
// `conformance/scenarios/index.ts`: once from a fixture, and once from the
// PROSE of this very comment, which originally quoted the offending
// specifier verbatim and re-created the trap it was describing.
function withFixture(run) {
  const dir = mkdtempSync(join(tmpdir(), 'release-clean-clone-'))
  try {
    return run(dir)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

test('demonstrated red: a relative import reaching into apps/ fails, naming the file and the target', () => {
  withFixture((dir) => {
    mkdirSync(join(dir, 'scripts'), { recursive: true })
    mkdirSync(join(dir, 'apps', 'fixture-cli', 'src'), { recursive: true })
    writeFileSync(join(dir, 'apps', 'fixture-cli', 'src', 'fixture-paths.ts'), 'export const launcherRoot = "/x"\n')
    writeFileSync(join(dir, RELEASE_ENTRY), 'import { launcherRoot } from "../apps/fixture-cli/src/fixture-paths";\n')
    const { violations } = auditPreInstallGraph(dir, RELEASE_ENTRY, [])
    assert.equal(violations.length, 1)
    assert.match(violations[0], /apps\/fixture-cli\/src\/fixture-paths/)

    // Green half: the same file established from its own location instead.
    writeFileSync(
      join(dir, RELEASE_ENTRY),
      'import { dirname, resolve } from "node:path";\nimport { fileURLToPath } from "node:url";\nconst launcherRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");\n'
    )
    assert.deepEqual(auditPreInstallGraph(dir, RELEASE_ENTRY, []).violations, [])
  })
})

test('demonstrated red: an import of a DELETED module is caught even though nothing named apps/ or packages/', () => {
  withFixture((dir) => {
    mkdirSync(join(dir, 'scripts'), { recursive: true })
    writeFileSync(join(dir, RELEASE_ENTRY), 'import { helper } from "./gone-with-the-deletion";\n')
    const { violations } = auditPreInstallGraph(dir, RELEASE_ENTRY, [])
    assert.equal(violations.length, 1)
    assert.match(violations[0], /resolves to nothing on disk/)
  })
})

test('demonstrated red: a @chief/* workspace package import fails, and a plain npm dependency does not', () => {
  withFixture((dir) => {
    mkdirSync(join(dir, 'scripts'), { recursive: true })
    writeFileSync(join(dir, RELEASE_ENTRY), 'import { z } from "@chief/piing";\nimport { x } from "zod";\n')
    const { violations } = auditPreInstallGraph(dir, RELEASE_ENTRY, ['@chief/piing'])
    assert.equal(violations.length, 1, 'a plain npm dependency must not be flagged — only the workspace')
    assert.match(violations[0], /@chief\/piing/)
  })
})

test('demonstrated red: the violation is caught one hop away, through another scripts/ file', () => {
  withFixture((dir) => {
    mkdirSync(join(dir, 'scripts'), { recursive: true })
    mkdirSync(join(dir, 'packages', 'fixture-piing', 'src'), { recursive: true })
    writeFileSync(join(dir, 'packages', 'fixture-piing', 'src', 'fixture-entry.ts'), 'export const x = 1\n')
    writeFileSync(join(dir, 'scripts', 'fixture-release-helper.ts'), 'export { x } from "../packages/fixture-piing/src/fixture-entry";\n')
    writeFileSync(join(dir, RELEASE_ENTRY), 'import { x } from "./fixture-release-helper";\n')
    const { visited, violations } = auditPreInstallGraph(dir, RELEASE_ENTRY, [])
    assert.ok(visited.includes('scripts/fixture-release-helper.ts'), 'the walk must follow relative imports inside scripts/')
    assert.equal(violations.length, 1)
    assert.match(violations[0], /fixture-release-helper\.ts/)
  })
})

test('a doc comment NAMING a deleted module is prose, not an import — the fix must not report itself', () => {
  const source = [
    '/**',
    ' * This was previously imported from `apps/cli/src/legacy/foundation/paths`.',
    ' * import { launcherRoot } from "../apps/cli/src/legacy/foundation/gone-paths";',
    ' */',
    '// import { gone } from "../packages/gone-piing";',
    'import { resolve } from "node:path";',
    'const url = "https://example.com/apps/cli"; // a // inside a string',
  ].join('\n')
  assert.deepEqual(collectModuleSpecifiers(source), ['node:path'])
})

test('demonstrated red: a bun.lock workspace path that does not exist is caught, and the two blocks must agree', () => {
  const real = readFileSync(join(repoRoot, BUN_LOCK), 'utf8')
  const declared = workspacePathsFromBunLock(real)
  // The anchor is a workspace that exists TODAY; it was `apps/cli` until P3
  // deleted that package, which is the same rot this whole guard exists to
  // catch, one level up.
  assert.ok(declared.includes('apps/web'), 'sanity: the real lock names apps/web')

  const doctored = real.replace('    "apps/web": {', '    "apps/deleted-by-a-past-commit": {')
  assert.notEqual(doctored, real, 'the doctoring did not land — refusing to trust a fixture that never changed')
  const doctoredPaths = workspacePathsFromBunLock(doctored)
  assert.ok(doctoredPaths.includes('apps/deleted-by-a-past-commit'))
  assert.ok(!existsSync(join(repoRoot, 'apps/deleted-by-a-past-commit')))
  // ...and the cross-check fires too: the resolution block still says apps/web.
  assert.notDeepEqual(
    workspacePackagesFromBunLock(doctored).map((p) => p.path).sort(),
    doctoredPaths
  )
})

test('demonstrated red: a Cargo.lock path package whose crate was deleted is caught by name', () => {
  const real = readFileSync(join(repoRoot, CARGO_LOCK), 'utf8')
  const doctored = `${real}\n[[package]]\nname = "chiefd-deleted-crate"\nversion = "0.1.0"\ndependencies = [\n "chiefd-core",\n]\n`
  const pathPackages = pathPackagesFromCargoLock(doctored)
  assert.ok(pathPackages.includes('chiefd-deleted-crate'))
  const memberNames = workspaceMemberPackages(repoRoot).map((m) => m.name)
  assert.ok(!memberNames.includes('chiefd-deleted-crate'))
  assert.deepEqual(pathPackages.filter((n) => !memberNames.includes(n)), ['chiefd-deleted-crate'])
})

test('a Cargo.lock package WITH a source line is a registry crate, never asserted against the crates tree', () => {
  const withSource = [
    '[[package]]',
    'name = "serde"',
    'version = "1.0.0"',
    'source = "registry+https://github.com/rust-lang/crates.io-index"',
    '',
    '[[package]]',
    'name = "chiefd-core"',
    'version = "0.1.0"',
    '',
  ].join('\n')
  assert.deepEqual(pathPackagesFromCargoLock(withSource), ['chiefd-core'])
})

test('demonstrated red: an empty scan is a failure, not a pass, for all three subjects', () => {
  assert.deepEqual(workspacePathsFromBunLock(''), [])
  assert.deepEqual(workspacePackagesFromBunLock(''), [])
  assert.deepEqual(pathPackagesFromCargoLock(''), [])
  assert.deepEqual(collectModuleSpecifiers(''), [])
  // Each real-tree test above asserts `> 3` against these same derivations,
  // so an extractor that regressed to the empty results asserted here takes
  // this guard red rather than silently green.
})
