// Vitest globalSetup: refuse to run a package's tests until the workspace
// packages it imports have been built.
//
// Why this exists (#2982). `turbo.json` declares `test:unit` -> dependsOn
// ["^build"], and that IS honoured — `bun run test` at the root builds
// dependencies first. But each package's own script is a bare `vitest run`, so
// running tests the way you actually iterate on them —
// `cd apps/zipbox && vitest run test/providers/Foo.test.ts` — bypasses turbo
// entirely and skips the build. The tests then fail with 100+
// "Failed to resolve import" errors that read exactly like a broken checkout.
//
// That misreading cost five agents time in one night, four of whom had been
// warned about it in their brief. A warning that fails five times needs to
// become a mechanism, so this turns the whole class into one actionable line.
//
// The required package list is DERIVED, never hardcoded: it is every dependency
// declared with the `workspace:` protocol. A hardcoded list goes stale exactly
// the way this problem grows — `apps/zipbox` gained `voicing` and `billing`
// after the first agents hit it, and a stale list would have silently passed.
//
// Fail-visible by construction: every step that cannot determine an answer
// THROWS rather than returning "fine". A precheck that silently passes when it
// cannot tell reproduces the original problem one layer up.

import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join, parse } from 'node:path'

const WORKSPACE_PROTOCOL = 'workspace:'

function readJson(path, what) {
  let raw
  try {
    raw = readFileSync(path, 'utf8')
  } catch (cause) {
    throw new Error(`[workspace-build-guard] cannot read ${what} at ${path}: ${cause.message}`)
  }
  try {
    return JSON.parse(raw)
  } catch (cause) {
    throw new Error(`[workspace-build-guard] cannot parse ${what} at ${path}: ${cause.message}`)
  }
}

// Every file an `exports` map (or a bare `main`) points at. A package whose
// entry files are committed (build-config) passes for free; one that emits to
// dist fails until it is built — no build/no-build special-casing needed.
function entryPointsOf(manifest) {
  const entries = []
  const walk = (node) => {
    if (typeof node === 'string') {
      if (node.startsWith('./')) {
        entries.push(node)
      }
      return
    }
    if (node !== null && typeof node === 'object') {
      for (const value of Object.values(node)) {
        walk(value)
      }
    }
  }
  walk(manifest.exports ?? null)
  if (typeof manifest.main === 'string') {
    entries.push(manifest.main)
  }
  return [...new Set(entries)]
}

// An entry may be a subpath PATTERN (`"./ui/*"` -> `./dist/ui/*.js`), which names
// no single file — existsSync() on a path containing `*` is false unconditionally,
// so checking it literally reports every such package unbuilt forever (#3144:
// packages/voicing was the first to use one). For a pattern, check the static
// directory prefix before the `*` instead: absent before the build, present after.
// `join` normalizes the trailing slash, so `./dist/ui/*.js` checks `<dep>/dist/ui`.
function entryExists(depDir, entry) {
  const star = entry.indexOf('*')
  const target = star === -1 ? entry : entry.slice(0, entry.lastIndexOf('/', star) + 1)
  return existsSync(join(depDir, target))
}

// The repo root is the nearest ancestor whose package.json declares
// `workspaces`. Resolving packages from there — rather than from node_modules —
// keeps the guard independent of hoisting layout and of `exports` maps that
// refuse a `./package.json` subpath.
function findRepoRoot(startDir) {
  let dir = startDir
  for (;;) {
    const candidate = join(dir, 'package.json')
    if (existsSync(candidate)) {
      const manifest = readJson(candidate, 'package.json')
      if (Array.isArray(manifest.workspaces)) {
        return dir
      }
    }
    const parent = dirname(dir)
    if (parent === dir || parent === parse(dir).root) {
      throw new Error(
        `[workspace-build-guard] no package.json with a "workspaces" field above ${startDir}; ` +
          `cannot locate the repo root. Refusing to run rather than passing blind.`
      )
    }
    dir = parent
  }
}

// name -> directory, built by reading every workspace package's own manifest.
function workspacePackageDirs(repoRoot, globs) {
  const dirs = new Map()
  for (const glob of globs) {
    if (!glob.endsWith('/*')) {
      continue
    }
    const parentDir = join(repoRoot, glob.slice(0, -2))
    if (!existsSync(parentDir)) {
      continue
    }
    for (const entry of readdirSync(parentDir, { withFileTypes: true })) {
      if (!entry.isDirectory()) {
        continue
      }
      const dir = join(parentDir, entry.name)
      const manifestPath = join(dir, 'package.json')
      if (!existsSync(manifestPath)) {
        continue
      }
      const manifest = readJson(manifestPath, `${entry.name} package.json`)
      if (typeof manifest.name === 'string') {
        dirs.set(manifest.name, dir)
      }
    }
  }
  return dirs
}

export default function assertWorkspaceBuilt() {
  const packageDir = process.cwd()
  const manifestPath = join(packageDir, 'package.json')
  const manifest = readJson(manifestPath, 'package.json')

  const declared = { ...(manifest.dependencies ?? {}), ...(manifest.devDependencies ?? {}) }
  const workspaceDeps = Object.entries(declared)
    .filter(([, version]) => String(version).startsWith(WORKSPACE_PROTOCOL))
    .map(([name]) => name)

  if (workspaceDeps.length === 0) {
    return
  }

  const repoRoot = findRepoRoot(packageDir)
  const rootManifest = readJson(join(repoRoot, 'package.json'), 'root package.json')
  const packageDirs = workspacePackageDirs(repoRoot, rootManifest.workspaces)
  const unbuilt = []

  for (const name of workspaceDeps) {
    // Failing to locate a declared workspace dependency is itself a finding: a
    // guard that shrugged here would pass blind on exactly the packages it
    // exists to check.
    const depDir = packageDirs.get(name)
    if (depDir === undefined) {
      throw new Error(
        `[workspace-build-guard] ${name} is declared with the workspace: protocol but no package ` +
          `under ${rootManifest.workspaces.join(', ')} declares that name. ` +
          `Refusing to run rather than passing blind.`
      )
    }

    const depManifest = readJson(join(depDir, 'package.json'), `${name} package.json`)
    const entries = entryPointsOf(depManifest)

    if (entries.length === 0) {
      throw new Error(
        `[workspace-build-guard] ${name} declares neither "exports" nor "main", so this guard ` +
          `cannot tell whether it is built. Refusing to run rather than passing blind.`
      )
    }

    const missing = entries.filter((entry) => !entryExists(depDir, entry))
    if (missing.length > 0) {
      unbuilt.push(name)
    }
  }

  if (unbuilt.length > 0) {
    const filters = unbuilt.map((name) => `--filter=${name}`).join(' ')
    throw new Error(
      `[workspace-build-guard] workspace packages are not built: ${unbuilt.join(', ')}. ` +
        `Their tests would fail with misleading "Failed to resolve import" errors. ` +
        `Build them first: \`bun x turbo run build ${filters}\` (or run tests via \`bun run test\` ` +
        `from the repo root, which builds dependencies for you).`
    )
  }
}
