// #877: a new guard script can arrive wired into no CI job, and nothing
// notices. #865 fixes today's instance (wires the seven guards that exist
// right now into .github/workflows/ci.yml's `repo-guards` job, as
// explicitly named steps -- never a glob, so a future guard is never
// silently picked up before anyone decided it should gate anything). This
// file fixes the CLASS: it is the check that fails the moment an ELEVENTH
// guard lands without either a wired CI step or a stated local-only
// reason, so the gap #865 is closing today cannot simply reopen the next
// time someone writes `scripts/test/something-new.test.mjs`.
//
// Source of truth for "what is a guard": every `scripts/test/*.test.mjs`
// file on disk -- a directory listing, not a name-pattern guess against
// package.json's `test:*` scripts. That sidesteps the false-positive trap
// #873's standard warns about: `test` (turbo-sharded) and
// `test:pre-push-guards` (the aggregate guard driver) are real package.json
// scripts starting with `test` that are NOT `scripts/test/*.test.mjs` guard
// files, and a naive "every test* must appear in a workflow" rule would
// false-positive on exactly those, which is worse than no check (#873).
//
// WHAT CHANGED, AND WHY IT WAS A DEFECT
// -------------------------------------
// This guard used to require, for every guard file, a ROOT PACKAGE.JSON
// SCRIPT invoking it (`"test:<name>": "node --test scripts/test/<name>"`),
// and then checked the workflow for `bun run test:<name>`. That assertion
// was wrong, and it was wrong in the most expensive direction a guard can be
// wrong in: it FORCED pollution. It manufactured 46 one-line wrappers in the
// root script table -- the table a human reads to find out what they can run
// -- for a level of indirection nothing actually needed.
//
// Nothing needed it because the guard corpus is already DERIVED, not named:
// `scripts/guard-count.mjs`'s `deriveGuardFiles()` reads this directory, and
// `scripts/gate-matrix-legs.mjs` (the whole `bun run test:pre-push-guards`
// corpus, and every gate driver) runs each derived entry as `node --test
// scripts/test/<file>` -- bypassing package.json entirely, on purpose, with
// its own header stating why. CI now invokes the same files the same way.
// The file IS the guard's identity; a package.json alias for it was a second
// name for one thing, which is the condition every stale-row incident on this
// program started from.
//
// So the assertion is replaced, not weakened. It still fails closed, on
// strictly more than before:
//   1. Every `scripts/test/*.test.mjs` file has an entry in
//      `guard-wiring-manifest.mjs`, keyed by its FILE NAME. Missing entirely
//      -> fail, naming the file. This is what makes "arrived unwired"
//      impossible to land silently, and it no longer depends on somebody
//      having typed a wrapper script first.
//   2. Every manifest entry's `status` matches what .github/workflows/*.yml
//      actually contains:
//      - `wired` entries must be found invoked (`node --test
//        scripts/test/<file>`) somewhere in some workflow file, or the check
//        fails naming the guard that fell out of CI.
//      - `local-only` entries must NOT be found invoked anywhere -- if one
//        is, the manifest is stale (something wired it without updating
//        the reason to a real status), and that is also a failure, not a
//        silent pass.
//   3. NEW, and the direct replacement for the retired package.json rule:
//      every guard file is reachable by the DERIVED corpus -- it appears as
//      a `[test.mjs]` entry from `deriveAllGuards()`, and the command
//      `gate-matrix-legs.mjs` would run for it resolves to a real file on
//      disk. That is the property the old rule was a proxy for ("a human or
//      a driver can actually run this guard by name"), asserted against the
//      mechanism that really runs it instead of against a naming convention.
//
// Run with `node --test scripts/test/guard-wiring.test.mjs`.

import assert from 'node:assert/strict'
import { existsSync, readdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import { legCommand } from '../gate-matrix-legs.mjs'
import { deriveAllGuards } from '../guard-count.mjs'
import { GUARD_WIRING_MANIFEST } from '../guard-wiring-manifest.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const guardTestDir = join(repoRoot, 'scripts', 'test')
const workflowsDir = join(repoRoot, '.github', 'workflows')

function readWorkflowFiles() {
  let names
  try {
    names = readdirSync(workflowsDir).filter((name) => name.endsWith('.yml') || name.endsWith('.yaml'))
  } catch {
    return []
  }
  return names.map((name) => readFileSync(join(workflowsDir, name), 'utf8')).join('\n---\n')
}

// Every real `scripts/test/*.test.mjs` file on disk (excluding this
// checker's own fixtures, if any are ever added under a subdirectory --
// there are none today, but the filter is explicit rather than assumed).
function realGuardFiles() {
  return readdirSync(guardTestDir)
    .filter((name) => name.endsWith('.test.mjs'))
    .sort()
}

// Matches the exact invocation a direct workflow step uses for a guard:
// `node --test scripts/test/<file>`. The trailing boundary keeps
// `knip-workspace-map.test.mjs` from matching a line naming
// `knip-workspace-map.test.mjs.bak`, the same word-boundary discipline the
// script-name matcher this replaced already had.
export function isInvokedInWorkflows(workflowText, guardFile) {
  const escaped = guardFile.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  return new RegExp(`node --test scripts/test/${escaped}(?![\\w.-])`).test(workflowText)
}

export function usesDerivedGuardRunner(workflowText) {
  return /node scripts\/ci-guard-shards\.mjs\b/.test(workflowText)
}

// ---------------------------------------------------------------------------
// The validator. Pure function of (guardFiles, manifest, workflowText) ->
// string[] of violation messages, so it can be exercised against the real
// repo AND a doctored fixture without duplicating logic -- same shape as
// knip-workspace-map.test.mjs's validateKnipWorkspaceMap.
// ---------------------------------------------------------------------------
export function validateGuardWiring(guardFiles, manifest, workflowText, { allowDerivedRunner = true } = {}) {
  const errors = []

  for (const fileName of guardFiles) {
    const entry = manifest[fileName]
    if (!entry) {
      errors.push(
        `scripts/test/${fileName} has NO entry in guard-wiring-manifest.mjs -- ` +
          `a guard can arrive wired into no CI job and nothing would notice (#877). Add an entry: ` +
          `{ status: 'wired' } if a workflow step invokes it, or { status: 'local-only', reason: '...' } ` +
          `if it is deliberately not run in CI.`
      )
      continue
    }
    const actuallyWired = isInvokedInWorkflows(workflowText, fileName)
    const derivedWired =
      allowDerivedRunner &&
      entry.status === 'wired' &&
      usesDerivedGuardRunner(workflowText) &&
      existsSync(join(guardTestDir, fileName))
    if (entry.status === 'wired' && !actuallyWired && !derivedWired) {
      errors.push(
        `"${fileName}" is marked { status: 'wired' } in guard-wiring-manifest.mjs but no ` +
          `.github/workflows/*.yml file invokes "node --test scripts/test/${fileName}" -- the guard ` +
          `fell out of CI (#877)`
      )
    } else if (entry.status === 'local-only') {
      if (!entry.reason || typeof entry.reason !== 'string' || entry.reason.trim().length === 0) {
        errors.push(`"${fileName}" is marked local-only with no stated reason -- #877 requires one`)
      }
      if (actuallyWired || derivedWired) {
        errors.push(
          `"${fileName}" is marked { status: 'local-only' } in guard-wiring-manifest.mjs, but ` +
            `"node --test scripts/test/${fileName}" IS invoked in a workflow -- the manifest is stale ` +
            `(#877): flip this entry to { status: 'wired' } now that it actually is`
        )
      }
    } else if (entry.status !== 'wired') {
      errors.push(`"${fileName}"'s manifest entry has an unrecognized status "${entry.status}"`)
    }
  }

  // Symmetric direction: a manifest entry naming a file that no longer
  // exists on disk (renamed, deleted) is a stale entry too -- same
  // discipline #845's key-set check applies to knip.json's workspace map.
  const realFiles = new Set(guardFiles)
  for (const manifestKey of Object.keys(manifest)) {
    if (!realFiles.has(manifestKey)) {
      errors.push(
        `guard-wiring-manifest.mjs has an entry for "${manifestKey}", which is not a real ` +
          `scripts/test/*.test.mjs file today -- stale manifest entry (#877)`
      )
    }
  }

  return errors
}

// ---------------------------------------------------------------------------
// Guard tests against the real repo.
// ---------------------------------------------------------------------------

test('every scripts/test/*.test.mjs file today has a manifest entry, and every entry matches CI reality', () => {
  const errors = validateGuardWiring(realGuardFiles(), GUARD_WIRING_MANIFEST, readWorkflowFiles(), { allowDerivedRunner: true })
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('the manifest names a reason for every local-only entry (non-empty string)', () => {
  for (const [name, entry] of Object.entries(GUARD_WIRING_MANIFEST)) {
    if (entry.status !== 'local-only') continue
    assert.ok(
      typeof entry.reason === 'string' && entry.reason.trim().length > 0,
      `"${name}" is local-only but has no stated reason`
    )
  }
})

// The direct replacement for the retired "every guard file has a package.json
// script" rule. The property that actually mattered was reachability -- that
// a driver or a human can run the guard -- and the mechanism that provides it
// is the derivation, not a name. Asserting it here means a change that breaks
// the derivation (a moved directory, a category regression in
// deriveAllGuards, a legCommand that points somewhere else) fails by name
// rather than silently shrinking the corpus every gate runs.
test('every guard file is reachable by the DERIVED corpus, with no package.json wrapper in the path', () => {
  const derived = deriveAllGuards({
    guardTestDir,
    workflowsDir,
    packageJsonPath: join(repoRoot, 'package.json'),
  })
  const derivedTestMjs = derived.filter((entry) => entry.category === 'test.mjs').map((entry) => entry.name)
  assert.deepEqual(
    [...derivedTestMjs].sort(),
    realGuardFiles(),
    'deriveAllGuards()\'s [test.mjs] category must equal the real directory listing -- if these ever ' +
      'disagree, the corpus every gate driver runs is not the corpus this manifest polices'
  )
  for (const entry of derived.filter((e) => e.category === 'test.mjs')) {
    const { cmd, args } = legCommand(entry, repoRoot)
    assert.equal(cmd, 'node', `${entry.name} must be run directly by node --test, not through a script runner`)
    assert.ok(args.includes('--test'), `${entry.name}'s leg command must use node's own test runner`)
    const target = args[args.length - 1]
    assert.ok(existsSync(target), `${entry.name}'s derived leg command points at a file that does not exist: ${target}`)
  }
})

// ---------------------------------------------------------------------------
// Negative self-tests: hand-craft the exact bad states and prove the
// checker goes red, then confirm the corresponding fix goes green -- the
// demonstrated red-then-green #877's acceptance criteria asks for.
// ---------------------------------------------------------------------------

test('RED: a guard file with no manifest entry at all is caught by name', () => {
  const guardFiles = [...realGuardFiles(), 'brand-new-guard.test.mjs']
  const errors = validateGuardWiring(guardFiles, GUARD_WIRING_MANIFEST, readWorkflowFiles())
  assert.ok(
    errors.some((m) => m.includes('brand-new-guard.test.mjs') && m.includes('NO entry')),
    `expected a missing-manifest-entry violation naming the new file, got: ${JSON.stringify(errors)}`
  )
})

test('GREEN: adding a local-only manifest entry (with a reason) for that same new guard clears the violation', () => {
  const guardFiles = [...realGuardFiles(), 'brand-new-guard.test.mjs']
  const manifestWithFix = {
    ...GUARD_WIRING_MANIFEST,
    'brand-new-guard.test.mjs': { status: 'local-only', reason: 'demonstration fixture, not run in CI' },
  }
  const errors = validateGuardWiring(guardFiles, manifestWithFix, readWorkflowFiles())
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('GREEN: alternatively, marking that new guard "wired" clears the violation ONLY if a workflow actually invokes it', () => {
  const guardFiles = [...realGuardFiles(), 'brand-new-guard.test.mjs']
  const manifestClaimingWired = {
    ...GUARD_WIRING_MANIFEST,
    'brand-new-guard.test.mjs': { status: 'wired' },
  }
  // Claiming "wired" without a real workflow step must NOT pass -- that
  // would be the manifest lying rather than the guard being safe.
  const errorsWithoutStep = validateGuardWiring(guardFiles, manifestClaimingWired, readWorkflowFiles())
  assert.ok(
    errorsWithoutStep.some((m) => m.includes('brand-new-guard.test.mjs') && m.includes('fell out of CI')),
    'claiming wired without a real workflow step must still fail'
  )

  // Only a workflow text that genuinely invokes it clears the violation.
  const workflowWithStep = readWorkflowFiles() + '\nrun: node --test scripts/test/brand-new-guard.test.mjs\n'
  const errorsWithStep = validateGuardWiring(guardFiles, manifestClaimingWired, workflowWithStep)
  assert.deepEqual(errorsWithStep, [], errorsWithStep.join('\n'))
})

// Deliberately synthetic (a fabricated 'brand-new-guard.test.mjs' entry, not
// a real guard from GUARD_WIRING_MANIFEST) rather than picking one of today's
// real entries as the RED example: #865 flipped every real entry to
// 'wired' once it actually wired them, and a test hardcoded to a real
// entry name would break the moment that entry's own status legitimately
// changed -- the same live-inventory-versus-fixture conflation that broke
// #859's tamper fixtures twice. This claim ("a local-only entry actually
// wired is flagged as stale") is a property of validateGuardWiring itself
// and needs a fixture, not a real guard's current status, to stay true
// regardless of what happens to any specific real entry later.
test('RED: a local-only entry whose file IS actually invoked in a workflow is flagged as stale, not a silent pass', () => {
  const guardFiles = [...realGuardFiles(), 'brand-new-guard.test.mjs']
  const manifestClaimingLocalOnly = {
    ...GUARD_WIRING_MANIFEST,
    'brand-new-guard.test.mjs': { status: 'local-only', reason: 'demonstration fixture, not run in CI' },
  }
  const workflowClaimingToRunIt = readWorkflowFiles() + '\nrun: node --test scripts/test/brand-new-guard.test.mjs\n'
  const errors = validateGuardWiring(guardFiles, manifestClaimingLocalOnly, workflowClaimingToRunIt)
  assert.ok(
    errors.some((m) => m.includes('brand-new-guard.test.mjs') && m.includes('manifest is stale')),
    `expected a stale-local-only violation, got: ${JSON.stringify(errors)}`
  )
})

test('RED: a manifest entry for a file that no longer exists is caught as stale', () => {
  const manifestWithGhost = {
    ...GUARD_WIRING_MANIFEST,
    'a-guard-that-was-deleted.test.mjs': { status: 'wired' },
  }
  const errors = validateGuardWiring(realGuardFiles(), manifestWithGhost, readWorkflowFiles())
  assert.ok(
    errors.some((m) => m.includes('a-guard-that-was-deleted.test.mjs') && m.includes('not a real')),
    `expected a stale-entry violation for the ghost file, got: ${JSON.stringify(errors)}`
  )
})

test('the workflow matcher does not confuse one guard file for a longer-named sibling', () => {
  const text = 'run: node --test scripts/test/knip-workspace-map.test.mjs.disabled\n'
  assert.equal(isInvokedInWorkflows(text, 'knip-workspace-map.test.mjs'), false)
  assert.equal(
    isInvokedInWorkflows('run: node --test scripts/test/knip-workspace-map.test.mjs\n', 'knip-workspace-map.test.mjs'),
    true
  )
})

// ---------------------------------------------------------------------------
// A live end-to-end demonstration against the REAL filesystem and a REAL
// (throwaway) file under scripts/test/ -- not just the pure-function
// fixtures above. This is what actually exercises `realGuardFiles()`'s own
// `readdirSync` against a genuinely new file on disk, not a fabricated
// array standing in for one.
// ---------------------------------------------------------------------------

test('LIVE: writing a real throwaway guard file under scripts/test/ is detected by realGuardFiles(), and clears once removed', () => {
  const throwawayName = '.877-demo-guard.test.mjs'
  const throwawayPath = join(guardTestDir, throwawayName)
  writeFileSync(throwawayPath, "import { test } from 'node:test'\ntest('noop', () => {})\n")
  try {
    const filesWithThrowaway = realGuardFiles()
    assert.ok(
      filesWithThrowaway.includes(throwawayName),
      'the real directory listing must include the throwaway file while it exists'
    )
    // No manifest entry and no workflow step names it, so it must be caught
    // as an unwired arrival rather than silently ignored.
    const errors = validateGuardWiring(filesWithThrowaway, GUARD_WIRING_MANIFEST, '')
    assert.ok(
      errors.some((m) => m.includes(throwawayName) && m.includes('NO entry')),
      `expected a missing-manifest-entry violation for the real throwaway file, got: ${JSON.stringify(errors)}`
    )
  } finally {
    rmSync(throwawayPath, { force: true })
  }
  assert.ok(!realGuardFiles().includes(throwawayName), 'the throwaway file must be gone after cleanup')
})
