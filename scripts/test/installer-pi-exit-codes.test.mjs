import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { chmodSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'

// WHAT THIS EXISTS FOR.
//
// install.sh refuses to report success when a prerequisite is missing, and that
// rule had no instrument at all: every Pi branch in the script was reachable
// only by running the real installer against the real network, so nothing
// checked the EXIT CODE any of them produced. The rule shipped as a comment,
// and a comment is exactly what the two `|| true` sites contradicted — the
// script printed its happy outro and exited zero with the agent runtime absent.
//
// So this runs the script's OWN BYTES. It does not re-implement the decision
// block or assert over its text; it extracts the block by marker, supplies the
// handful of names the block reads from earlier in the file, and executes it
// against stub `pi` and `npm` on a private PATH.

const installer = fileURLToPath(new URL('../../install.sh', import.meta.url))
const START = /^version_below\(\) \{$/m

/** The block under test, cut from the real file at a marker rather than at a
 *  line number, plus the definitions it inherits from earlier in the script.
 *
 *  It REFUSES rather than passing when the marker is gone: a guard that cannot
 *  find its subject has not verified the subject, and reporting that as green
 *  is the failure this whole file is written against. */
function decisionBlock() {
  const src = readFileSync(installer, 'utf8')
  const at = src.search(START)
  assert.notEqual(
    at, -1,
    'CANNOT CHECK: install.sh no longer contains a `version_below() {` line, so the Pi ' +
    'decision block cannot be located. Re-derive the marker and update this guard; do not ' +
    'delete it, and do not let it pass on a file it could not read.',
  )
  const tail = src.slice(at)
  for (const needed of ['install_pi()', 'confirm_default_yes()', 'if ! have pi; then']) {
    assert.ok(
      tail.includes(needed),
      `CANNOT CHECK: the extracted block is missing ${needed}; the marker no longer bounds the ` +
      'Pi section. Re-derive it rather than trusting this run.',
    )
  }
  return [
    'set -eu',
    'CHIEF_HOME="$HOME/.chief"',
    'PI_INSTALL="npm install -g --ignore-scripts @earendil-works/pi-coding-agent"',
    "say() { printf '%s\\n' \"$*\"; }",
    "die() { printf 'chief install: %s\\n' \"$*\" >&2; exit 1; }",
    'have() { command -v "$1" >/dev/null 2>&1; }',
    'pi_floor="${TEST_PI_FLOOR:-0.80.10}"',
    tail,
  ].join('\n')
}

/** Run the block with stub binaries. `pi` absent when version is null. */
function run({ piVersion, npmSucceeds, npmInstallsVersion = '0.90.0', source = undefined }) {
  const dir = mkdtempSync(join(tmpdir(), 'chief-installer-guard-'))
  const bin = join(dir, 'bin')
  execFileSync('mkdir', ['-p', bin])
  const piPath = join(bin, 'pi')

  const writeStub = (p, body) => { writeFileSync(p, `#!/bin/sh\n${body}\n`); chmodSync(p, 0o755) }

  if (piVersion !== null) writeStub(piPath, `echo "pi ${piVersion}"`)
  // npm "installing" means it drops a pi stub at the requested version.
  writeStub(
    join(bin, 'npm'),
    npmSucceeds
      ? `cat > '${piPath}' <<'S'\n#!/bin/sh\necho "pi ${npmInstallsVersion}"\nS\nchmod 755 '${piPath}'\nexit 0`
      : 'echo "npm: failed" >&2; exit 1',
  )

  const script = join(dir, 'block.sh')
  writeFileSync(script, source ?? decisionBlock())

  try {
    const stdout = execFileSync('sh', [script], {
      encoding: 'utf8',
      // No controlling terminal: the no-tty default path, which is the one CI takes.
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { PATH: `${bin}:/usr/bin:/bin`, HOME: dir, TEST_PI_FLOOR: '0.80.10' },
    })
    return { status: 0, output: stdout }
  } catch (error) {
    return { status: error.status ?? -1, output: `${error.stdout ?? ''}${error.stderr ?? ''}` }
  }
}

test('an absent Pi that fails to install does not report success', () => {
  const { status, output } = run({ piVersion: null, npmSucceeds: false })
  assert.notEqual(
    status, 0,
    'install.sh exited ZERO with the agent runtime absent. A caller — a CI job, a provisioning ' +
    'script — is told chief is ready when nothing can run a person. The declined-upgrade path ' +
    'already refuses to call a too-old Pi success; an absent one is strictly worse.',
  )
  assert.match(output, /Pi did not install/, 'the failure must say what did not happen')
  assert.match(output, /npm install -g/, 'and must name the command that fixes it')
})

test('an accepted upgrade that fails to install does not report success', () => {
  const { status, output } = run({ piVersion: '0.70.0', npmSucceeds: false })
  assert.notEqual(
    status, 0,
    'agreeing to upgrade did not make the upgrade happen: the too-old Pi the decline path ' +
    'refuses to call success is exactly what is left behind, so this cannot exit zero either.',
  )
  assert.match(output, /Pi did not upgrade/)
})

test('an absent Pi that installs cleanly reports success', () => {
  const { status, output } = run({ piVersion: null, npmSucceeds: true, npmInstallsVersion: '0.90.0' })
  assert.equal(status, 0, `the happy path must still be green:\n${output}`)
  assert.match(output, /is ready/)
})

test('a Pi already at or above the floor is left alone, and nothing is asked', () => {
  const { status, output } = run({ piVersion: '0.90.0', npmSucceeds: false })
  assert.equal(status, 0, `a satisfactory Pi must not be touched:\n${output}`)
  assert.doesNotMatch(output, /\[Y\/n\]/, 'nothing should be asked about a Pi that already qualifies')
})

test('with no terminal the upgrade prompt takes its default and SAYS so', () => {
  // The default is yes, so a working npm upgrades without a tty and without hanging.
  const { status, output } = run({ piVersion: '0.70.0', npmSucceeds: true, npmInstallsVersion: '0.90.0' })
  assert.equal(status, 0, `the no-tty default must proceed, not hang or abort:\n${output}`)
  assert.match(
    output, /no terminal to ask on/,
    'a choice made on somebody\'s behalf must be stated, not silent',
  )
})

test('the exit-code assertions BITE — proven against the defect they were written for', () => {
  // A pin nobody flipped is a claim, not evidence. This reverts the fix inside
  // the extracted copy only (the file on disk is untouched) and asserts the
  // guard above would have caught the original bug.
  const reverted = decisionBlock().replace(
    /\|\| die "chief itself is installed under \$CHIEF_HOME; Pi did not install[^"]*"/,
    '|| true',
  )
  assert.ok(reverted.includes('|| true'), 'the control could not construct the defect; re-derive it')
  const { status } = run({ piVersion: null, npmSucceeds: false, source: reverted })
  assert.equal(
    status, 0,
    'CONTROL FAILED: with `|| true` restored the block should exit zero, which is the bug. It did ' +
    'not, so the passing assertions above are not measuring what they claim to measure.',
  )
})
