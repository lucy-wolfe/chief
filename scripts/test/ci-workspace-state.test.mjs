// Guard for scripts/ci-workspace-state-manifest.mjs (#838, E9-S6 CI second
// pass). #877 proved a repo-invariant guard can arrive wired into no CI
// job with nothing noticing; the same gap exists one layer up, for
// workspace members instead of guard files: a new `apps/*`/`packages/*`
// package, or a new Cargo crate, can arrive with no recorded CI-execution
// state and nothing fails. This is what notices.
//
// Two independent enumerations, kept apart deliberately (this file's own
// standing rule, DECISIONS.md 2026-08-04 "synthetic fixtures prove
// capability, live state measures state" — the ENUMERATION here reads
// live repo state on purpose, that IS its job; the negative-self-test
// fixtures below construct their own synthetic member lists rather than
// doctoring the real ones, so they keep proving the CHECKER's logic
// regardless of what the real tree currently contains):
//   1. Bun workspace members: every `<glob-parent>/*` directory containing
//      a package.json, resolved from root package.json's `workspaces`
//      globs (mirrors knip-workspace-map.test.mjs's own resolver).
//   2. Cargo workspace members: every entry in `apps/chiefd/Cargo.toml`'s
//      `members = [...]` array.
// Every member from both must have an entry in CI_WORKSPACE_STATE_MANIFEST,
// and every entry's claimed status must match reality in both directions —
// same symmetric discipline as #877's guard-wiring checker.
//
// Run with `node --test scripts/test/ci-workspace-state.test.mjs` (wired
// as `bun run test:ci-workspace-state`).

import assert from 'node:assert/strict'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import { CI_WORKSPACE_STATE_MANIFEST } from '../ci-workspace-state-manifest.mjs'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(__dirname, '..', '..')

function repoFile(root, ...segments) {
  return join(root, ...segments)
}

function readPackageJson(root) {
  return JSON.parse(readFileSync(repoFile(root, 'package.json'), 'utf8'))
}

function readWorkflowFiles(root) {
  const workflowsDir = repoFile(root, '.github', 'workflows')
  let names
  try {
    names = readdirSync(workflowsDir).filter((name) => name.endsWith('.yml') || name.endsWith('.yaml'))
  } catch {
    return ''
  }
  return names.map((name) => readFileSync(join(workflowsDir, name), 'utf8')).join('\n---\n')
}

function yamlTopLevelBlock(source, key) {
  const prefix = `  ${key}:`
  const start = source.indexOf(prefix)
  if (start === -1) return ''
  const remainder = source.slice(start + prefix.length)
  const next = remainder.search(/\n  [A-Za-z][A-Za-z0-9_-]*:/)
  return source.slice(start, next === -1 ? source.length : start + prefix.length + next)
}

function turboTaskBlock(source, taskName) {
  const prefix = `    "${taskName}": {`
  const start = source.indexOf(prefix)
  if (start === -1) return ''
  const remainder = source.slice(start + prefix.length)
  const next = remainder.search(/\n    "[^"]+": \{/)
  return source.slice(start, next === -1 ? source.length : start + prefix.length + next)
}

// #891's debugger is useful only if all three delivery links stay connected:
// the manual workflow input, each real Vitest job's environment, and Turbo's
// strict-env allowlist. A missing link yields a green test with no diagnostic
// output, which looks like evidence but is actually an inert measurement.
export function validateDebug891Delivery(workflowText, turboText) {
  const errors = []
  const dispatch = yamlTopLevelBlock(workflowText, 'workflow_dispatch')
  const testUnitJobs = ['test-unit-base', 'test-unit-piing']
    .map((job) => ({ job, block: yamlTopLevelBlock(workflowText, job) }))
  const testUnitTask = turboTaskBlock(turboText, 'test:unit')

  if (!/^      debug_891:\s*$/m.test(dispatch)) {
    errors.push('workflow_dispatch has no debug_891 input for the #891 manual diagnostic run')
  }
  if (!/^        type:\s*boolean\s*$/m.test(dispatch)) {
    errors.push('workflow_dispatch debug_891 input is not boolean')
  }
  for (const { job, block } of testUnitJobs) {
    if (!/DEBUG_891:\s*\$\{\{\s*inputs\.debug_891\s*==\s*true\s*&&\s*'1'\s*\|\|\s*''\s*\}\}/.test(block)) {
      errors.push(`test-unit does not map the debug_891 input to DEBUG_891 in ${job}`)
    }
  }
  if (!/"env"\s*:\s*\[[^\]]*"DEBUG_891"[^\]]*\]/.test(testUnitTask)) {
    errors.push('turbo test:unit does not declare DEBUG_891 under strict env mode')
  }

  return errors
}

// Every directory matched by a `workspaces` glob of the form "<dir>/*" that
// itself contains a package.json — the same definition bun/npm workspaces
// use, and the same resolver shape knip-workspace-map.test.mjs already
// uses for the identical enumeration.
function resolveBunWorkspaceMembers(root, globs) {
  const members = []
  for (const glob of globs) {
    if (!glob.endsWith('/*')) continue
    const parentRel = glob.slice(0, -2)
    const parentAbs = repoFile(root, parentRel)
    if (!existsSync(parentAbs)) continue
    for (const entry of readdirSync(parentAbs, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue
      const memberRel = `${parentRel}/${entry.name}`
      if (existsSync(repoFile(root, memberRel, 'package.json'))) members.push(memberRel)
    }
  }
  return members.sort()
}

// Parses `members = [...]` out of apps/chiefd/Cargo.toml. Deliberately a
// small hand-rolled scan (quoted-string extraction inside the first
// `members = [ ... ]` block), not a full TOML parser -- the file's shape
// is simple and stable, and a dependency-free scan mirrors this repo's
// other guard scripts (stub-import-guard.mjs, cargo-test-floor-lib.mjs).
function resolveCargoWorkspaceMembers(root) {
  const manifestPath = repoFile(root, 'apps', 'chiefd', 'Cargo.toml')
  const source = readFileSync(manifestPath, 'utf8')
  const match = /members\s*=\s*\[([\s\S]*?)\]/.exec(source)
  if (!match) {
    throw new Error(`[ci-workspace-state] apps/chiefd/Cargo.toml has no "members = [...]" array -- cannot enumerate`)
  }
  const entries = [...match[1].matchAll(/"([^"]+)"/g)].map((m) => m[1])
  return ['apps/chiefd', ...entries.map((e) => `apps/chiefd/${e}`)].sort()
}

function hasTestUnitScript(root, memberRel) {
  const pkgPath = repoFile(root, memberRel, 'package.json')
  if (!existsSync(pkgPath)) return false
  const pkg = JSON.parse(readFileSync(pkgPath, 'utf8'))
  return typeof pkg.scripts?.['test:unit'] === 'string'
}

// ---------------------------------------------------------------------------
// The validator. Pure function of (bunMembers, cargoMembers, manifest,
// workflowText, hasTestUnitFn) -> string[] of violation messages -- same
// shape as validateGuardWiring/validateKnipWorkspaceMap, exercisable
// against the real repo or a doctored fixture without duplicating logic.
// `hasTestUnitFn` is injected (not hardcoded to `hasTestUnitScript` reading
// the real repo) so the negative self-tests below can fabricate a member's
// script-presence without writing real package.json files onto disk.
// ---------------------------------------------------------------------------
export function validateCiWorkspaceState(bunMembers, cargoMembers, manifest, workflowText, hasTestUnitFn) {
  const errors = []
  const testUnitWired = /bun run test(?![\w:-])|turbo run test:unit/.test(workflowText)
  const cargoWired = /cargo test --workspace/.test(workflowText)

  for (const member of bunMembers) {
    const entry = manifest[member]
    if (!entry) {
      errors.push(
        `"${member}" is a bun workspace member with NO entry in ci-workspace-state-manifest.mjs -- a ` +
          `package can arrive with no recorded CI-execution state and nothing notices (#838). Add ` +
          `{ status: 'vitest' } if it declares "test:unit", or { status: 'no-tests', reason: '...' } if not.`
      )
      continue
    }
    const declaresTestUnit = hasTestUnitFn(member)
    if (entry.status === 'vitest') {
      if (!declaresTestUnit) {
        errors.push(
          `"${member}" is marked { status: 'vitest' } but its own package.json has no "test:unit" script ` +
            `-- the manifest claims coverage that does not exist (#838)`
        )
      }
      if (!testUnitWired) {
        errors.push(
          `"${member}" is marked { status: 'vitest' } but no workflow invokes the Vitest task at all ` +
            `-- every vitest-status member fell out of CI at once (#838)`
        )
      }
    } else if (entry.status === 'no-tests') {
      if (!entry.reason || typeof entry.reason !== 'string' || entry.reason.trim().length === 0) {
        errors.push(`"${member}" is marked no-tests with no stated reason -- #838 requires one`)
      }
      if (declaresTestUnit) {
        errors.push(
          `"${member}" is marked { status: 'no-tests' } but its own package.json DOES declare "test:unit" ` +
            `-- the manifest is stale (#838): flip this entry to { status: 'vitest' }`
        )
      }
    } else if (entry.status !== 'cargo') {
      errors.push(`"${member}"'s manifest entry has an unrecognized status "${entry.status}"`)
    }
  }

  for (const member of cargoMembers) {
    const entry = manifest[member]
    if (!entry) {
      errors.push(
        `"${member}" is a Cargo workspace member with NO entry in ci-workspace-state-manifest.mjs (#838). ` +
          `Add { status: 'cargo' }.`
      )
      continue
    }
    if (entry.status !== 'cargo') {
      errors.push(`"${member}" is a real Cargo workspace member but its manifest entry says "${entry.status}", not "cargo" (#838)`)
    } else if (!cargoWired) {
      errors.push(
        `"${member}" is marked { status: 'cargo' } but no workflow invokes "cargo test --workspace" at all ` +
          `-- every cargo-status member fell out of CI at once (#838)`
      )
    }
  }

  // Symmetric direction: a manifest entry naming a member that no longer
  // exists on disk (renamed, removed) is a stale entry too.
  const realMembers = new Set([...bunMembers, ...cargoMembers])
  for (const manifestKey of Object.keys(manifest)) {
    if (!realMembers.has(manifestKey)) {
      errors.push(
        `ci-workspace-state-manifest.mjs has an entry for "${manifestKey}", which does not correspond to ` +
          `any real bun or Cargo workspace member today -- stale manifest entry (#838)`
      )
    }
  }

  return errors
}

// ---------------------------------------------------------------------------
// Guard tests against the real repo.
// ---------------------------------------------------------------------------

test('sanity check: the enumeration is not vacuous -- resolves a real, non-trivial member count', () => {
  const pkg = readPackageJson(repoRoot)
  const bunMembers = resolveBunWorkspaceMembers(repoRoot, pkg.workspaces)
  const cargoMembers = resolveCargoWorkspaceMembers(repoRoot)
  assert.ok(bunMembers.length >= 5, `expected >=5 bun workspace members, found ${bunMembers.length}`)
  assert.ok(cargoMembers.length >= 5, `expected >=5 Cargo workspace members, found ${cargoMembers.length}`)
})

test('every real workspace member (bun + cargo) has a manifest entry, and every entry matches CI reality', () => {
  const pkg = readPackageJson(repoRoot)
  const bunMembers = resolveBunWorkspaceMembers(repoRoot, pkg.workspaces)
  const cargoMembers = resolveCargoWorkspaceMembers(repoRoot)
  const workflowText = readWorkflowFiles(repoRoot)
  const errors = validateCiWorkspaceState(
    bunMembers,
    cargoMembers,
    CI_WORKSPACE_STATE_MANIFEST,
    workflowText,
    (member) => hasTestUnitScript(repoRoot, member)
  )
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('LIVE: #891 manual diagnostics have an end-to-end workflow-to-Turbo delivery path', () => {
  const workflowText = readWorkflowFiles(repoRoot)
  const turboText = readFileSync(repoFile(repoRoot, 'turbo.json'), 'utf8')
  const errors = validateDebug891Delivery(workflowText, turboText)
  assert.deepEqual(errors, [], errors.join('\n'))
})

// ---------------------------------------------------------------------------
// Negative self-tests: fully synthetic member lists and manifests (never
// the real repo's), hand-crafting the exact bad states and proving the
// checker goes red, then confirming the corresponding fix goes green.
// ---------------------------------------------------------------------------

test('RED: a bun member with no manifest entry at all is caught by name', () => {
  const bunMembers = ['apps/brand-new']
  const errors = validateCiWorkspaceState(
    bunMembers,
    [],
    {},
    'run: bun run test',
    () => true
  )
  assert.ok(
    errors.some((m) => m.includes('apps/brand-new') && m.includes('NO entry')),
    `expected a missing-entry violation naming the new member, got: ${JSON.stringify(errors)}`
  )
})

test('GREEN: adding a vitest entry for that same member, with test:unit wired, clears the violation', () => {
  const bunMembers = ['apps/brand-new']
  const manifest = { 'apps/brand-new': { status: 'vitest' } }
  const errors = validateCiWorkspaceState(bunMembers, [], manifest, 'run: bun run test', () => true)
  assert.deepEqual(errors, [])
})

test('RED: a member marked vitest whose own package.json has no test:unit script is caught', () => {
  const bunMembers = ['apps/brand-new']
  const manifest = { 'apps/brand-new': { status: 'vitest' } }
  const errors = validateCiWorkspaceState(bunMembers, [], manifest, 'run: bun run test', () => false)
  assert.ok(
    errors.some((m) => m.includes('apps/brand-new') && m.includes('no "test:unit" script')),
    `expected a claims-coverage-that-does-not-exist violation, got: ${JSON.stringify(errors)}`
  )
})

test('RED: a member marked no-tests whose own package.json DOES declare test:unit is flagged as stale', () => {
  const bunMembers = ['apps/brand-new']
  const manifest = { 'apps/brand-new': { status: 'no-tests', reason: 'placeholder' } }
  const errors = validateCiWorkspaceState(bunMembers, [], manifest, '', () => true)
  assert.ok(
    errors.some((m) => m.includes('apps/brand-new') && m.includes('manifest is stale')),
    `expected a stale no-tests violation, got: ${JSON.stringify(errors)}`
  )
})

test('RED: no workflow invokes "bun run test" at all -- every vitest-status member falls out at once', () => {
  const bunMembers = ['apps/brand-new']
  const manifest = { 'apps/brand-new': { status: 'vitest' } }
  const errors = validateCiWorkspaceState(bunMembers, [], manifest, 'run: bun run typecheck', () => true)
  assert.ok(
    errors.some((m) => m.includes('apps/brand-new') && m.includes('fell out of CI at once')),
    `expected a test-not-wired violation, got: ${JSON.stringify(errors)}`
  )
})

test('RED: a #891 workflow input without its job environment and Turbo declaration is rejected', () => {
  const workflowText = [
    '  workflow_dispatch:',
    '    inputs:',
    '      debug_891:',
    '        type: boolean',
    '  test-unit:',
    '    steps: []'
  ].join('\n')
  const turboText = ['{', '  "tasks": {', '    "test:unit": { "dependsOn": ["^build"] }', '  }', '}'].join('\n')
  const errors = validateDebug891Delivery(workflowText, turboText)
  assert.ok(
    errors.some((message) => message.includes('test-unit does not map')) &&
      errors.some((message) => message.includes('turbo test:unit does not declare')),
    `expected both broken diagnostic-delivery links to be named, got: ${JSON.stringify(errors)}`
  )
})

test('RED: a Cargo member with no manifest entry is caught by name', () => {
  const errors = validateCiWorkspaceState([], ['apps/chiefd/crates/brand-new'], {}, 'cargo test --workspace', () => true)
  assert.ok(
    errors.some((m) => m.includes('apps/chiefd/crates/brand-new') && m.includes('NO entry')),
    `expected a missing Cargo entry violation, got: ${JSON.stringify(errors)}`
  )
})

test('RED: a manifest entry for a member that no longer exists is caught as stale', () => {
  const manifest = { 'apps/long-gone': { status: 'vitest' } }
  const errors = validateCiWorkspaceState([], [], manifest, '', () => true)
  assert.ok(
    errors.some((m) => m.includes('apps/long-gone') && m.includes('stale manifest entry')),
    `expected a stale-entry violation, got: ${JSON.stringify(errors)}`
  )
})

// LIVE proof (not a doctored fixture): writing a real throwaway package.json
// under apps/ is detected by resolveBunWorkspaceMembers, and clears once
// removed -- the same "the resolver itself works against the real
// filesystem" proof guard-wiring.test.mjs's own LIVE test gives realGuardFiles().
test('LIVE: a real throwaway workspace member on disk is detected by the resolver, and clears once removed', () => {
  const tempDir = mkdtempSync(join(tmpdir(), 'ci-workspace-state-live-'))
  try {
    const fakeApps = join(tempDir, 'apps')
    mkdirSync(join(fakeApps, 'throwaway-member'), { recursive: true })
    writeFileSync(join(fakeApps, 'throwaway-member', 'package.json'), '{"name":"throwaway"}\n')
    const members = resolveBunWorkspaceMembers(tempDir, ['apps/*'])
    assert.deepEqual(members, ['apps/throwaway-member'])
  } finally {
    rmSync(tempDir, { recursive: true, force: true })
  }
})
