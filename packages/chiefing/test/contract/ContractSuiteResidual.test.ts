/**
 * #751/G6 — a real-binary contract suite that silently skips is a guard that
 * cannot see.
 *
 * Every file in `test/contract/**` gates itself on `chiefdBinaryTestGate()`
 * and, when the debug test binary is absent, becomes `describe.skip`. #846 chose
 * that deliberately: skip visibly locally, fail loudly in CI. The "fail loudly
 * in CI" half works — `chiefdBinaryTestGate` throws when `CI` is set. The
 * "visibly" half does not survive contact with a real run: the skip title is
 * carried inside a `describe` name, so what a developer actually sees at the
 * end of `bun run test:unit` is
 *
 *     Test Files  46 passed | 7 skipped (53)
 *          Tests  700 passed | 25 skipped (725)
 *
 * and nothing anywhere says WHICH capability was not exercised. That number
 * looks like a pass. This packet found out the hard way: the contract suite
 * had been skipping on this workstation for its entire existence, and the
 * moment a binary was built, eight tests failed immediately. Green meant "not
 * run", which is this audit's defining defect one level up from the code.
 *
 * So this file does not gate itself. It always runs, derives the list of
 * suites that WOULD be skipped, and turns their absence into a red with the
 * exact build command attached. It supersedes #846's local-skip half for this
 * package: a developer who has not built chiefd now learns that from a
 * failing test naming the seven files, instead of from a passing summary.
 *
 * Derived, not listed: the file set comes from scanning `test/contract/` for
 * the gate call, so a new contract suite is covered the day it is written and
 * a deleted one cannot leave a stale row behind.
 */
import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { chiefdBinaryTestGate, chiefdBuildCommand } from '@chief/testing'
import { describe, expect, it } from 'vitest'

const CONTRACT_DIR = dirname(fileURLToPath(import.meta.url))
const SELF = 'ContractSuiteResidual.test.ts'

/** Contract suites that disable themselves when the binary is missing. */
function gatedSuites(): string[] {
  return readdirSync(CONTRACT_DIR)
    .filter((name) => name.endsWith('.test.ts') && name !== SELF)
    .filter((name) =>
      readFileSync(join(CONTRACT_DIR, name), 'utf8').includes('chiefdBinaryTestGate(')
    )
    .sort()
}

const GATED = gatedSuites()
const gate = chiefdBinaryTestGate()

describe('the real-binary contract suite reports what it did not run (#751/G6)', () => {
  it('the scan finds the gated suites (a vacuous scan would hide the whole residual)', () => {
    // If this ever reads zero files the check below is meaningless — it would
    // pass by having nothing to report, which is precisely the failure shape
    // this file exists to eliminate.
    expect(GATED.length).toBeGreaterThan(0)
    expect(GATED).toContain('RowsContract.test.ts')
  })

  it('every gated suite actually ran — the chiefd debug test binary is present', () => {
    expect(
      gate.present,
      `RESIDUAL: ${GATED.length} real-binary contract suite(s) DID NOT RUN because the ` +
        `chiefd debug test binary is missing at ${gate.binaryPath}.\n\n` +
        `NOT EXERCISED:\n${GATED.map((name) => `  - test/contract/${name}`).join('\n')}\n\n` +
        `These suites are the only place this package is checked against the real ` +
        `router instead of a fake transport that answers anything. Skipping them ` +
        `turns a green summary into a statement about nothing.\n\n` +
        `Build it:\n\n    ${chiefdBuildCommand()}\n`
    ).toBe(true)
  })

  it('every gated suite also names its own skip visibly (#846 title convention)', () => {
    // The describe-title banner is still the right thing for reporters that
    // only print names; this keeps it from rotting now that the summary check
    // above carries the load.
    const missing = GATED.filter(
      (name) => !readFileSync(join(CONTRACT_DIR, name), 'utf8').includes('chiefdBinarySkipTitle')
    )
    expect(missing, 'a gated contract suite skips without naming itself').toEqual([])
  })
})
