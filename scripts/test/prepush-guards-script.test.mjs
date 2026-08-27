// #973: a correct, CI-wired guard nobody runs before pushing produces the
// same outcome as a broken guard -- a red discovered at assembly, by
// someone else, attributed by guesswork (#963's sql-only-state row was
// exactly this). `bun run test:pre-push-guards` is the cheap, discoverable pre-push
// entrypoint into the 37 repo-invariant `scripts/test/*.test.mjs` guards
// (via `scripts/gate-matrix-legs.mjs`, no cargo build) that closes that gap.
//
// This test locks the WIRING, not the guards' own correctness (each has its
// own test; `gate-matrix-legs.test.mjs` already proves the derivation
// itself). If `package.json`'s "test:pre-push-guards" script is deleted, renamed, or
// drifts from the exact three `--explicit-shell-gate` flags
// `scripts/gate-matrix.sh` passes (the flags `gate-matrix-legs.mjs` needs to
// avoid refusing on shell-gate reconciliation when run standalone), this
// fails loudly rather than the drift going unnoticed until someone tries to
// run it and it silently does the wrong thing.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(__dirname, '..', '..')

function readPackageJsonScripts() {
  const parsed = JSON.parse(readFileSync(join(repoRoot, 'package.json'), 'utf8'))
  if (!parsed.scripts || typeof parsed.scripts !== 'object') {
    throw new Error('root package.json has no "scripts" table')
  }
  return parsed.scripts
}

function shellGateFlagsFromMatrix() {
  const matrix = readFileSync(join(repoRoot, 'scripts', 'gate-matrix.sh'), 'utf8')
  const flags = [...matrix.matchAll(/--explicit-shell-gate\s+(\S+)/g)].map((m) => m[1].replace(/[;\\]+$/, ''))
  if (flags.length === 0) {
    throw new Error('scripts/gate-matrix.sh no longer passes any --explicit-shell-gate flag -- derive the new source of truth rather than trusting this test\'s old assumption')
  }
  return flags
}

test('#973: "bun run test:pre-push-guards" exists and runs gate-matrix-legs.mjs with no cargo build', () => {
  const scripts = readPackageJsonScripts()
  assert.ok('test:pre-push-guards' in scripts, 'package.json is missing the "test:pre-push-guards" script #973 introduced')
  assert.match(scripts["test:pre-push-guards"], /node scripts\/gate-matrix-legs\.mjs/, 'the "test:pre-push-guards" script must invoke gate-matrix-legs.mjs directly (no cargo build, no turbo) -- that is the entire point of the cheap pre-push entrypoint')
})

test('#973: "test:pre-push-guards" passes the SAME --explicit-shell-gate set gate-matrix.sh does -- neither more nor fewer', () => {
  const scripts = readPackageJsonScripts()
  const matrixFlags = shellGateFlagsFromMatrix()
  for (const flag of matrixFlags) {
    assert.ok(
      scripts["test:pre-push-guards"].includes(`--explicit-shell-gate ${flag}`),
      `"test:pre-push-guards" is missing --explicit-shell-gate ${flag}, which scripts/gate-matrix.sh passes -- without it, gate-matrix-legs.mjs refuses to run at all (shell-gate reconciliation failure) rather than silently skipping`
    )
  }
  const guardsFlagCount = (scripts["test:pre-push-guards"].match(/--explicit-shell-gate/g) || []).length
  assert.equal(
    guardsFlagCount,
    matrixFlags.length,
    'the "test:pre-push-guards" script has a different number of --explicit-shell-gate flags than scripts/gate-matrix.sh -- an extra or missing one silently changes which shell gates are treated as covered elsewhere'
  )
})
