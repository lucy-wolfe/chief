// #934: tests for the gate driver's own preconditions.
//
// Every arm here is a DEMONSTRATED RED: it reproduces one of the four defects
// that made the merger's matrix weaker than CI for nine landings, and asserts
// the preflight REFUSES. A guard that has only ever been observed passing is a
// claim, not a check — so each refusal arm is paired with a control proving
// the same preflight passes when the condition is satisfied.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, chmodSync, rmSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'

const SCRIPT = new URL('../gate-preflight.sh', import.meta.url).pathname
const REPO_ROOT = new URL('../..', import.meta.url).pathname

/**
 * #1041: the binary names this preflight must require, DERIVED from ci.yml's
 * own debug-binary chmod line rather than typed here. The preflight itself
 * checks the debug test directory used by the local gate and CI test path.
 * Deployment has its own release path.
 *
 * The list was `['chiefd', 'beacond']` in both the script and this fixture,
 * while ci.yml provisions three -- `chiefd` was missing, which is the
 * one the docstore suites actually boot. Two hand-maintained copies of the
 * same list are two lists that can disagree, and this pair disagreed with CI
 * silently for as long as they existed. Deriving here means a fourth binary
 * added to CI makes this fixture demand it too, with no edit.
 */
function ciProvisionedBinaries() {
  const workflow = readFileSync(join(REPO_ROOT, '.github/workflows/ci.yml'), 'utf8')
  const line = workflow
    .split('\n')
    .find((l) => l.includes('chmod +x') && l.includes('apps/chiefd/target/debug/'))
  assert.ok(line, 'ci.yml has no `chmod +x apps/chiefd/target/debug/...` line to derive from')
  const names = [...line.matchAll(/apps\/chiefd\/target\/debug\/([A-Za-z0-9_-]+)/g)].map((m) => m[1])
  assert.ok(names.length > 0, 'derived an EMPTY CI-binary set from ci.yml')
  return [...new Set(names)]
}

/** Run the preflight; return { code, out } instead of throwing. */
function run(root, env = {}) {
  return runPhase(root, undefined, env)
}

/** Same, but with an explicit phase arg (pre/post/all) — omit for the
 * default (bare repo-root invocation, "all"). */
function runPhase(root, phase, env = {}) {
  const args = phase === undefined ? [SCRIPT, root] : [SCRIPT, root, phase]
  try {
    const out = execFileSync('bash', args, {
      encoding: 'utf8',
      // The disk arm is ALWAYS exercised. A control that skips an arm is not a
      // control for that arm -- so the seam is the threshold (1G, which any host
      // running a gate satisfies), never a disable flag.
      env: { ...process.env, GATE_PREFLIGHT_MIN_FREE_GB: '1', ...env }
    })
    return { code: 0, out }
  } catch (error) {
    return { code: error.status ?? -1, out: `${error.stdout ?? ''}${error.stderr ?? ''}` }
  }
}

/** A fixture repo satisfying every precondition; callers break exactly one. */
function goodRepo() {
  const root = mkdtempSync(join(tmpdir(), 'gate-preflight-'))
  mkdirSync(join(root, 'apps/chiefd/target/debug'), { recursive: true })
  mkdirSync(join(root, 'scripts'), { recursive: true })
  for (const bin of ciProvisionedBinaries()) {
    const p = join(root, 'apps/chiefd/target/debug', bin)
    writeFileSync(p, '#!/bin/sh\nexit 0\n')
    chmodSync(p, 0o755)
  }
  writeFileSync(join(root, 'scripts/cargo-test-workspace.sh'), '#!/bin/sh\nexit 0\n')
  return root
}

test('control: a repo satisfying every precondition is allowed to gate', () => {
  const root = goodRepo()
  try {
    const { code, out } = run(root, { CI: '1' })
    assert.equal(code, 0, out)
    assert.match(out, /gate-preflight: OK/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('demonstrated red: CI unset REFUSES — the defect that hid 41 skipped tests', () => {
  const root = goodRepo()
  try {
    const { code, out } = run(root, { CI: '' })
    assert.equal(code, 1, 'an unset CI must refuse, not warn')
    assert.match(out, /REFUSING TO GATE: CI is unset/)
    assert.match(out, /its green would mean nothing/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('demonstrated red: a missing debug test binary REFUSES — EVERY one ci.yml provisions in-repo', () => {
  for (const missing of ciProvisionedBinaries()) {
    const root = goodRepo()
    try {
      rmSync(join(root, 'apps/chiefd/target/debug', missing))
      const { code, out } = run(root, { CI: '1' })
      assert.equal(code, 1, `a missing ${missing} must refuse`)
      assert.match(out, new RegExp(`debug/${missing} is missing or not executable`))
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  }
})

test('demonstrated red: a non-executable binary REFUSES — chmod +x is part of ci.yml, not decoration', () => {
  const root = goodRepo()
  try {
    chmodSync(join(root, 'apps/chiefd/target/debug/chiefd'), 0o644)
    const { code, out } = run(root, { CI: '1' })
    assert.equal(code, 1)
    assert.match(out, /is missing or not executable/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('demonstrated red: a missing cargo-test-workspace.sh REFUSES — no silent fail-fast fallback', () => {
  const root = goodRepo()
  try {
    rmSync(join(root, 'scripts/cargo-test-workspace.sh'))
    const { code, out } = run(root, { CI: '1' })
    assert.equal(code, 1)
    assert.match(out, /cargo-test-workspace\.sh is missing/)
    assert.match(out, /not about the tree/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('demonstrated red: insufficient disk REFUSES rather than starting a matrix it cannot fit', () => {
  const root = goodRepo()
  try {
    const { code, out } = run(root, { CI: '1', GATE_PREFLIGHT_MIN_FREE_GB: '999999' })
    assert.equal(code, 1)
    assert.match(out, /REFUSING TO GATE: only \d+G free/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// The portability regression. `df -BG --output=avail` is GNU coreutils only:
// macOS df rejects `-B`, FREE parsed to the empty string, and the disk arm
// refused with "only unknownG free" on a host with 70G available -- on EVERY
// branch, including untouched main. The arm above passes on either script,
// because "999999G needed" refuses whether or not a number was read; these
// three are the ones that could only pass with a portable read.

test('#1035 regression: the disk arm reads a REAL number on this host, not the empty string a GNU-only df left behind', () => {
  const root = goodRepo()
  try {
    const { code, out } = run(root, { CI: '1', GATE_PREFLIGHT_MIN_FREE_GB: '999999' })
    assert.equal(code, 1)
    // The load-bearing half: "unknownG" is what the GNU-only read produced on
    // macOS. A digit here proves df was actually parsed on THIS platform.
    assert.doesNotMatch(out, /only unknownG free/)
    const free = out.match(/only (\d+)G free/)
    assert.ok(free, `expected a numeric free-space reading, got: ${out}`)
    assert.ok(Number(free[1]) > 0, `expected a positive free-space reading, got ${free[1]}G`)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('#1035 regression: the script uses no GNU-only df flag -- the read must be POSIX on both platforms', () => {
  const text = readFileSync(SCRIPT, 'utf8')
  const dfCalls = text.split('\n').filter((l) => /^\s*[^#]*\bdf\b/.test(l))
  assert.ok(dfCalls.length > 0, 'no df invocation found in the preflight at all')
  for (const call of dfCalls) {
    assert.doesNotMatch(call, /--output/, `GNU-only df --output in: ${call.trim()}`)
    assert.doesNotMatch(call, /\s-B/, `GNU-only df -B in: ${call.trim()}`)
  }
})

test('#1035: still FAILS CLOSED -- a root df cannot report on refuses rather than skipping the disk check', () => {
  // The fix must not turn an unreadable disk into a pass. A nonexistent path
  // makes df report nothing, so FREE is empty and the -z test must refuse.
  // (Reached via the repo-root check, which refuses first for the same path --
  // so assert the disk read itself directly, in isolation.)
  const free = execFileSync(
    'bash',
    ['-c', `df -kP /nonexistent-gate-preflight-disk 2>/dev/null | awk 'NR==2 {print $4}' | tr -dc '0-9'`],
    { encoding: 'utf8' }
  )
  assert.equal(free, '', 'expected an unreadable path to yield no available field')
})

test('a missing repo root REFUSES rather than silently checking nothing', () => {
  const { code, out } = run('/nonexistent-gate-preflight-root', { CI: '1' })
  assert.equal(code, 1)
  assert.match(out, /is not a directory/)
})

// #941: guard-count's [shell-gate] section, when the tool is present.

function writeFakeGuardCount(root, { status = 0, out = 'DERIVED_GUARD_COUNT:1\n  [test.mjs] foo.test.mjs\n  [shell-gate] scripts/foo.sh (ci.yml:1)\n' } = {}) {
  mkdirSync(join(root, 'scripts'), { recursive: true })
  writeFileSync(
    join(root, 'scripts/guard-count.mjs'),
    `process.stdout.write(${JSON.stringify(out)});\nprocess.exit(${status});\n`
  )
}

test('control: guard-count.mjs present with a non-empty [shell-gate] section is allowed to gate', () => {
  const root = goodRepo()
  try {
    writeFakeGuardCount(root)
    const { code, out } = run(root, { CI: '1' })
    assert.equal(code, 0, out)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('demonstrated red: [#941] guard-count.mjs present but [shell-gate] section is EMPTY REFUSES', () => {
  const root = goodRepo()
  try {
    writeFakeGuardCount(root, { out: 'DERIVED_GUARD_COUNT:5\n  [test.mjs] foo.test.mjs\n' })
    const { code, out } = run(root, { CI: '1' })
    assert.equal(code, 1)
    assert.match(out, /EMPTY \[shell-gate\] section/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('demonstrated red: [#941] guard-count.mjs exiting non-zero REFUSES', () => {
  const root = goodRepo()
  try {
    writeFakeGuardCount(root, { status: 1, out: 'boom\n' })
    const { code, out } = run(root, { CI: '1' })
    assert.equal(code, 1)
    assert.match(out, /guard-count\.mjs exited 1/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('a synthetic fixture with no guard-count.mjs at all is unaffected by check 5 (absence is not this check\'s failure mode)', () => {
  const root = goodRepo()
  try {
    const { code, out } = run(root, { CI: '1' })
    assert.equal(code, 0, out)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// #941: cache-state freshness, only when CARGO_CACHE_STATE_SINCE_MS is set.

/** @param {string} root
 *  @param {{ stampedAtMs?: number, missing?: boolean }} options */
function writeFakeCacheStateTool(root, { stampedAtMs, missing = false } = {}) {
  mkdirSync(join(root, 'scripts'), { recursive: true })
  if (missing) return
  const body = `
const args = process.argv.slice(2)
if (args[0] !== 'assert') { process.exit(2) }
let since
for (let i = 1; i < args.length; i++) { if (args[i] === '--since') since = Number(args[i + 1]) }
const stampedAtMs = ${stampedAtMs}
if (stampedAtMs < since) {
  console.error('[cargo-cache-state] REFUSING TO GATE: stale stamp')
  process.exit(1)
}
console.log('[cargo-cache-state] OK')
process.exit(0)
`
  writeFileSync(join(root, 'scripts/cargo-cache-state.mjs'), body)
}

test('control: CARGO_CACHE_STATE_SINCE_MS unset means check 6 does not run at all', () => {
  const root = goodRepo()
  try {
    // No cargo-cache-state.mjs in this fixture at all -- if check 6 ran
    // unconditionally it would refuse on the missing tool. It must not run.
    const { code, out } = run(root, { CI: '1' })
    assert.equal(code, 0, out)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('demonstrated red: [#941] CARGO_CACHE_STATE_SINCE_MS set with a stale stamp REFUSES', () => {
  const root = goodRepo()
  try {
    writeFakeCacheStateTool(root, { stampedAtMs: 500 })
    const { code, out } = run(root, { CI: '1', CARGO_CACHE_STATE_SINCE_MS: '1000' })
    assert.equal(code, 1)
    assert.match(out, /REFUSING TO GATE: stale stamp/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('control: CARGO_CACHE_STATE_SINCE_MS set with a fresh stamp is allowed to gate', () => {
  const root = goodRepo()
  try {
    writeFakeCacheStateTool(root, { stampedAtMs: 1500 })
    const { code, out } = run(root, { CI: '1', CARGO_CACHE_STATE_SINCE_MS: '1000' })
    assert.equal(code, 0, out)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('demonstrated red: [#941] CARGO_CACHE_STATE_SINCE_MS set but cargo-cache-state.mjs missing REFUSES', () => {
  const root = goodRepo()
  try {
    writeFakeCacheStateTool(root, { missing: true })
    const { code, out } = run(root, { CI: '1', CARGO_CACHE_STATE_SINCE_MS: '1000' })
    assert.equal(code, 1)
    assert.match(out, /cargo-cache-state\.mjs is missing/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

// #941 rework: phase split (pre/post) — CI/disk/runner must be checkable
// BEFORE a multi-minute debug test build, not only after. A disk-floor check
// that only runs after the build it exists to protect cannot protect it.

test('phase "pre": passes on a repo satisfying only the pre-build preconditions (no binaries needed)', () => {
  const root = mkdtempSync(join(tmpdir(), 'gate-preflight-pre-'))
  try {
    mkdirSync(join(root, 'scripts'), { recursive: true })
    writeFileSync(join(root, 'scripts/cargo-test-workspace.sh'), '#!/bin/sh\nexit 0\n')
    // Deliberately NO apps/chiefd/target/debug binaries -- "pre" must not
    // require them; that's "post"'s job.
    const { code, out } = runPhase(root, 'pre', { CI: '1' })
    assert.equal(code, 0, out)
    assert.match(out, /gate-preflight \(pre\): OK/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('phase "pre": demonstrated red — CI unset refuses even with no binaries in play', () => {
  const root = mkdtempSync(join(tmpdir(), 'gate-preflight-pre-'))
  try {
    mkdirSync(join(root, 'scripts'), { recursive: true })
    writeFileSync(join(root, 'scripts/cargo-test-workspace.sh'), '#!/bin/sh\nexit 0\n')
    const { code, out } = runPhase(root, 'pre', { CI: '' })
    assert.equal(code, 1)
    assert.match(out, /REFUSING TO GATE: CI is unset/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('phase "post": REFUSES on a repo satisfying "pre" but with no binaries provisioned yet', () => {
  const root = mkdtempSync(join(tmpdir(), 'gate-preflight-post-'))
  try {
    mkdirSync(join(root, 'scripts'), { recursive: true })
    writeFileSync(join(root, 'scripts/cargo-test-workspace.sh'), '#!/bin/sh\nexit 0\n')
    const { code, out } = runPhase(root, 'post', { CI: '1' })
    assert.equal(code, 1)
    assert.match(out, /is missing or not executable/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('phase "post": passes once binaries are provisioned, independent of CI/disk (those are "pre"\'s job)', () => {
  const root = goodRepo()
  try {
    // Deliberately CI unset -- "post" must not re-check it; that's "pre"'s
    // job, and a driver calling both arms would otherwise double-refuse for
    // an already-covered reason.
    const { code, out } = runPhase(root, 'post', { CI: '' })
    assert.equal(code, 0, out)
    assert.match(out, /gate-preflight \(post\): OK/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('an unknown phase argument REFUSES rather than silently running something', () => {
  const root = goodRepo()
  try {
    const { code, out } = runPhase(root, 'sideways', { CI: '1' })
    assert.equal(code, 1)
    assert.match(out, /unknown phase 'sideways'/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('phase "all" (bare invocation, unchanged) still runs every check, both pre and post', () => {
  const root = mkdtempSync(join(tmpdir(), 'gate-preflight-all-'))
  try {
    mkdirSync(join(root, 'scripts'), { recursive: true })
    writeFileSync(join(root, 'scripts/cargo-test-workspace.sh'), '#!/bin/sh\nexit 0\n')
    // No binaries -- "all" must still hit the post-build binary check and
    // refuse, proving it did not silently degrade into "pre" only.
    const { code, out } = run(root, { CI: '1' })
    assert.equal(code, 1)
    assert.match(out, /is missing or not executable/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

