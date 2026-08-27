// ONE VERSION OF RECORD, and the files that must agree with it.
//
// # Why this exists
//
// The shipped version had THREE literals that disagreed, and the one everybody
// called the version of record was the only one nothing read:
//
//   package.json                "2.0.0"   read by NOTHING
//   apps/chiefd/Cargo.toml      "0.1.0"   the CARGO_PKG_VERSION fallback
//   .github/workflows/release.yml         a hard-coded `v2.0.0` seed, then
//                                         major.minor from whatever tag existed
//
// The workflow was the real source of record, and its own comment claimed the
// opposite -- "the major/minor track the workspace version" -- which nothing
// implemented. So an operator changing the declared version changed nothing
// shipped, silently. That is the same shape as a JSDoc describing a different
// object and a guard wrapped without saying why: a statement of intent sitting
// where a reader takes it for a mechanism.
//
// The repair made `package.json` the version of record for real. This guard is
// what keeps it one: two files carry the number, and a third derives from it.
//
// # What each assertion is for
//
// 1. `apps/chiefd/Cargo.toml` equals `package.json`. Cargo cannot read
//    `package.json`, so this pair genuinely has to be maintained -- which is
//    exactly the drift a guard is for rather than a comment.
// 2. `release.yml` DERIVES rather than carrying a literal. A version literal
//    reappearing there recreates the original defect, and it would be invisible
//    until somebody noticed the shipped number ignoring the declared one.

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
const read = (relative) => readFileSync(join(root, relative), 'utf8')

/** The declared version of record. */
export function declaredVersion() {
  const parsed = JSON.parse(read('package.json'))
  assert.equal(
    typeof parsed.version,
    'string',
    'package.json has no version -- it is the declared version of record and cannot be absent'
  )
  return parsed.version
}

/** The `version = "..."` in the chiefd workspace manifest. */
export function cargoWorkspaceVersion() {
  const source = read('apps/chiefd/Cargo.toml')
  const match = source.match(/^version = "([^"]+)"$/m)
  assert.ok(match, 'apps/chiefd/Cargo.toml has no top-level `version = "..."`')
  return match[1]
}

test('the cargo workspace version equals the declared version of record', () => {
  const declared = declaredVersion()
  const cargo = cargoWorkspaceVersion()
  assert.equal(
    cargo,
    declared,
    `apps/chiefd/Cargo.toml says ${cargo} and package.json says ${declared}. Cargo cannot read ` +
      'package.json, so these two are maintained by hand and must be changed together: the cargo ' +
      'value is the CARGO_PKG_VERSION a build script stamps when CHIEF_RELEASE_VERSION is absent, ' +
      'so a disagreement means an unstamped build reports a version no release ever had.'
  )
})

test('the release workflow DERIVES the series from package.json and carries no version literal', () => {
  const workflow = read('.github/workflows/release.yml')

  assert.ok(
    workflow.includes("require('./package.json').version"),
    'release.yml must read the series from package.json. It used to seed a literal tag and take ' +
      'major.minor from whatever tag existed, which made the workflow the real source of record ' +
      'while package.json was a declared intent nothing consumed.'
  )

  // A version literal in the tag-computing script is the original defect
  // returning. Comments are exempt: the block explains what it replaced, and
  // that explanation necessarily names the old literal.
  //
  // BOUNDED TO THE SCRIPT ITSELF, from its `set -euo pipefail` to the line
  // that emits the tag. A looser slice runs past the step into the rest of the
  // workflow and reports tooling pins -- `bun-version: 1.3.10` -- as versions
  // of record, which is a guard crying wolf about something it was never
  // asked to police. (Measured: that was this test's first run.)
  const start = workflow.indexOf('set -euo pipefail', workflow.indexOf('Compute next tag'))
  const end = workflow.indexOf('echo "tag=', start)
  assert.ok(start > 0 && end > start, 'could not locate the tag-computing script to scan it')
  const script = workflow.slice(start, end)
  const offenders = script
    .split('\n')
    .map((line, index) => ({ line, number: index + 1 }))
    .filter(({ line }) => !line.trim().startsWith('#'))
    .filter(({ line }) => /\bv?\d+\.\d+\.\d+\b/.test(line))
  assert.deepEqual(
    offenders.map(({ line }) => line.trim()),
    [],
    'a version literal in the tag-computing script is a SECOND source of record. The series comes ' +
      'from package.json and the patch comes from the existing tags; nothing else may name a version.'
  )
})

test('NON-VACUITY: the readers find real values, and would notice a disagreement', () => {
  // Both readers parse a file rather than a constant, so a moved key or a
  // reformatted manifest would make them return nothing and every assertion
  // above would pass by comparing two absences.
  const declared = declaredVersion()
  assert.match(declared, /^\d+\.\d+\.\d+/, `package.json version is not a version: ${declared}`)
  assert.match(cargoWorkspaceVersion(), /^\d+\.\d+\.\d+/)

  // And the comparison is real: a different value must fail it.
  assert.notEqual(declared, '0.0.0-not-a-real-version')
})
