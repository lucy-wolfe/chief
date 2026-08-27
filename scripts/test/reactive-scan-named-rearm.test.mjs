// R11: the reactive scan follows a NAMED callback one hop.
//
// `setTimeout(rearm, delay)` where `rearm` itself calls `setTimeout` is the
// same poll-in-disguise as an inline nested one — a one-shot timer that
// re-arms itself, functionally a `setInterval` wearing a disguise. The
// detector only inspected the argument list, so moving the re-arm into a named
// function made it invisible. A scan with a blind spot is worse than no scan:
// it reports clean and is believed.
import { spawnSync } from 'node:child_process'
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'
import assert from 'node:assert/strict'

// Run the REAL scanner through bun, not an import: `node --test` cannot
// resolve the scanner's extensionless TypeScript imports, and a test that
// reimplemented the scan would prove nothing about the scan.
function scanSites(root) {
  const result = spawnSync(
    'bun',
    [
      '-e',
      "import { findReactiveSites } from './scripts/reactive-scan.ts';" +
        'console.log(JSON.stringify(findReactiveSites(process.argv[1])))',
      root
    ],
    { cwd: join(import.meta.dirname, '..', '..'), encoding: 'utf8' }
  )
  if (result.status !== 0) throw new Error(`scan failed: ${result.stderr}`)
  return JSON.parse(result.stdout.trim().split('\n').pop())
}

/** A scratch repo holding one file under a scanned directory. */
function repoWith(source) {
  const root = mkdtempSync(join(tmpdir(), 'reactive-scan-'))
  mkdirSync(join(root, 'packages', 'piing', 'extensions'), { recursive: true })
  writeFileSync(join(root, 'packages', 'piing', 'extensions', 'sample.ts'), source)
  return root
}

test('a named callback that re-arms is flagged', () => {
  // The fixture that fails the pre-R11 scan: nothing nested in the argument
  // list, and a re-arm one identifier away.
  const root = repoWith(`
function rearm() {
  doWork()
  setTimeout(rearm, 1000)
}
export function start() {
  setTimeout(rearm, 1000)
}
`)
  try {
    const sites = scanSites(root)
    assert.ok(
      sites.some((site) => site.primitive === 'setTimeout-self-rescheduling'),
      `expected the named re-arm to be flagged, got: ${JSON.stringify(sites)}`
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('a named callback that does NOT re-arm stays clean', () => {
  // The other half, or the rule would just flag every named callback: a
  // one-shot timer calling a named function is not a poll, and reporting it
  // as one trains everybody to ignore the scan.
  const root = repoWith(`
function finish() {
  doWork()
}
export function start() {
  setTimeout(finish, 1000)
}
`)
  try {
    const sites = scanSites(root)
    assert.equal(
      sites.filter((site) => site.primitive === 'setTimeout-self-rescheduling').length,
      0,
      `a one-shot named callback is not a poll: ${JSON.stringify(sites)}`
    )
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('an inline nested setTimeout is still flagged', () => {
  // The original shape must not regress while adding the new one.
  const root = repoWith(`
export function start() {
  setTimeout(() => {
    setTimeout(() => doWork(), 10)
  }, 1000)
}
`)
  try {
    const sites = scanSites(root)
    assert.ok(sites.some((site) => site.primitive === 'setTimeout-self-rescheduling'))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('CI actually runs the scan this file fixtures', () => {
  // THE GAP THIS CLOSES, AND WHY IT LIVES HERE.
  //
  // These fixtures prove the DETECTOR is not blind. They cannot prove the scan
  // ever runs. `lint:reactive` has been on this repo's standing pre-push list
  // since #827 and was wired into no CI job at all, so the only thing that ever
  // executed it was somebody remembering — which is the `test:pre-push-guards`
  // gap in different clothes, and the reason CLAUDE.md says a correct, CI-wired
  // guard nobody runs produces the same outcome as a broken one.
  //
  // Asserted in the reactive scan's OWN guard rather than a new general one:
  // "every standing check is CI-wired" is a real and larger property, and the
  // general form is not a grep — `test` and `test:pre-push-guards` are executed
  // by CI under different spellings (vitest shards, `node scripts/ci-guard-shards.mjs`),
  // so a naive scan for `bun run <script>` would refuse on two checks that do
  // run. That guard is worth writing; writing it as a side effect of this one
  // is how a wiring fix becomes an unbounded cleanup.
  const workflow = readFileSync(
    fileURLToPath(new URL('../../.github/workflows/ci.yml', import.meta.url)),
    'utf8',
  )
  assert.match(
    workflow,
    /^\s+run: bun run lint:reactive$/m,
    'ci.yml must invoke `bun run lint:reactive`; the scan is only a gate if something runs it',
  )
})
