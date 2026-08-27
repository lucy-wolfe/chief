// THE FRONT DOOR IS `chief`. THE DAEMON IS `chiefd`. NEITHER IS THE OTHER.
//
// The two halves of the P6 split were installed as `chiefd` (the operator
// client) and `chiefd-daemon` (the backend) — a naming that read backwards,
// because the `d` suffix means daemon and it was attached to the program a
// person types. The names are now `chief` and `chiefd`.
//
// The rename's one real hazard is that for exactly one commit the token
// `chiefd` meant TWO programs: the old front door and the new daemon. A
// half-updated reference in that window does not fail — it reaches a real
// program with the wrong job, which is far worse than reaching nothing. The
// repo passed through that window atomically, and this guard is what keeps it
// closed: reintroducing a `chiefd` front door, or a `chiefd-daemon` binary,
// fails here rather than at the moment an operator's `chief attach` execs the
// wrong executable.
//
// Every assertion below reads a REAL file. Nothing is transcribed: the
// manifests, the client's `DAEMON_PROGRAM`, the release table, the prebuilt
// manifest default and the CI build flags are the six places a binary name is
// written down, and a name that disagrees between any two of them is exactly
// the defect this file exists to catch.
import { strict as assert } from 'node:assert'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

const repoRoot = fileURLToPath(new URL('../..', import.meta.url))
const read = (relative) => readFileSync(join(repoRoot, relative), 'utf8')

const CLIENT_MANIFEST = 'apps/chiefd/crates/chief-cli/Cargo.toml'
const DAEMON_MANIFEST = 'apps/chiefd/crates/chiefd-daemon/Cargo.toml'
const CLIENT_MAIN_RS = 'apps/chiefd/crates/chief-cli/src/main.rs'
const CLIENT_DAEMON_RS = 'apps/chiefd/crates/chief-cli/src/daemon.rs'
const CLIENT_PATHS_RS = 'apps/chiefd/crates/chief-cli/src/paths.rs'
const RELEASE_SCRIPT = 'scripts/release-chiefd.ts'
const PREBUILT_MANIFEST = 'scripts/prebuilt-binary-manifest.mjs'
const CI_WORKFLOW = '.github/workflows/ci.yml'

/** Every `[[bin]] name = "..."` a manifest declares, in order. */
function binTargets(manifest) {
  return [...manifest.matchAll(/\[\[bin\]\][\s\S]*?name = "([^"]+)"/g)].map((match) => match[1])
}

test('the operator client crate builds exactly one binary, named `chief`', () => {
  const manifest = read(CLIENT_MANIFEST)
  assert.deepEqual(
    binTargets(manifest),
    ['chief'],
    `${CLIENT_MANIFEST} must declare exactly one [[bin]], named chief — the program an operator types`,
  )
  // The PACKAGE keeps its own name. `chief-cli` already reads as "the chief
  // CLI crate", and a package rename would move the whole CI shard matrix for
  // nothing an operator can see.
  assert.match(manifest, /^name = "chief-cli"$/m)
})

test('the daemon crate builds exactly one binary, named `chiefd`', () => {
  const manifest = read(DAEMON_MANIFEST)
  assert.deepEqual(
    binTargets(manifest),
    ['chiefd'],
    `${DAEMON_MANIFEST} must declare exactly one [[bin]], named chiefd — the backend`,
  )
  // Same reasoning as above, in the other direction: `chiefd-daemon` is the
  // daemon crate OF chiefd, which is what the directory says.
  assert.match(manifest, /^name = "chiefd-daemon"$/m)
})

test('no crate in the workspace builds a binary named `chiefd-daemon`, and none but the daemon builds `chiefd`', () => {
  const crates = [
    'beacond',
    'chief-cli',
    'chiefd-api',
    'chiefd-core',
    'chiefd-daemon',
    'chiefd-host',
    'chiefd-log',
    'host-primitives',
    'identity-keys',
  ]
  const declared = new Map()
  for (const crate of crates) {
    for (const name of binTargets(read(`apps/chiefd/crates/${crate}/Cargo.toml`))) {
      declared.set(name, crate)
    }
  }
  assert.equal(
    declared.get('chiefd-daemon'),
    undefined,
    'the obsolete `chiefd-daemon` binary name must not come back: the daemon is `chiefd`',
  )
  assert.equal(declared.get('chief'), 'chief-cli')
  assert.equal(declared.get('chiefd'), 'chiefd-daemon')
  assert.equal(declared.get('beacond'), 'beacond')
  assert.equal(declared.size, 3, 'the workspace ships exactly three binaries')
})

test('the front door execs `chiefd` and never itself', () => {
  const source = read(CLIENT_MAIN_RS)
  const daemonSource = read(CLIENT_DAEMON_RS)
  const pathsSource = read(CLIENT_PATHS_RS)
  const match = source.match(/pub\(crate\) const DAEMON_PROGRAM: &str = "([^"]+)";/)
  assert.ok(match, `${CLIENT_MAIN_RS} must declare DAEMON_PROGRAM`)
  assert.equal(
    match[1],
    'chiefd',
    'the client forwards a daemon mode into `chiefd`; a client that named itself here would exec itself forever',
  )
  // The sibling resolution is the reason a stale `PATH` entry cannot serve a
  // company. Both forwarding and company startup must use the one helper, and
  // that helper must replace only the running client's file name.
  assert.match(source, /current_exe\(\)/)
  assert.match(source, /paths::chiefd_daemon_binary\(&executable\)/)
  assert.match(daemonSource, /paths::chiefd_daemon_binary\(&client_executable\)/)
  assert.match(pathsSource, /client_executable\.with_file_name\(super::DAEMON_PROGRAM\)/)
})

test('the front door never advertises itself as `chiefd`', () => {
  const source = read(CLIENT_MAIN_RS)
  // The usage text and every refusal this binary prints name the program an
  // operator actually types. `chiefd <verb>` in an operator-facing string is
  // the old front door leaking back.
  const leaks = [...source.matchAll(/"[^"\n]*\bchiefd (?:new|create|ls|attach|stop|rm|actuate|reset|topology|host|sidebar|help|--version)\b[^"\n]*"/g)]
  assert.deepEqual(leaks.map((leak) => leak[0]), [], 'an operator-facing string must say `chief <verb>`, not `chiefd <verb>`')
  assert.match(source, /"Usage: chief \[command\]"/)
  assert.doesNotMatch(source, /"Usage: chiefd \[command\]"/)
})

test('the release table, the prebuilt manifest and CI agree on the same three names', () => {
  assert.match(
    read(RELEASE_SCRIPT),
    /export const RELEASE_BINARIES = \["chief", "chiefd", "beacond"\] as const;/,
    'the install publishes chief, chiefd and beacond — a table that still said `chiefd` for the CLIENT would ship the daemon as the front door',
  )
  assert.match(
    read(RELEASE_SCRIPT),
    /export const OBSOLETE_RELEASE_BINARIES = \["chiefd-daemon"\] as const;/,
    'an install must REMOVE the pre-rename `chiefd-daemon`, not leave it beside the new pair',
  )
  assert.match(read(PREBUILT_MANIFEST), /export const DEFAULT_BINARIES = \["chief", "chiefd", "beacond"\];/)
  const workflow = read(CI_WORKFLOW)
  assert.match(workflow, /--bin chief --bin chiefd --bin beacond/)
  assert.doesNotMatch(workflow, /--bin chiefd-daemon/)
  assert.doesNotMatch(workflow, /target\/debug\/chiefd-daemon/)
})
