// THE VERSION ENSURE MAY NEVER STOP A PERSON.
//
// #1281 restarts a component that is not the installed build. The plan's first
// draft said the chiefd arm should use "the graceful shutdown path `chief stop`
// already drives". It must not, and the reason is one file away: `chief stop`
// is `stop_runtime(..., false)`, whose supervised order is pinned as data in
// `stop.rs` as
//
//   clear-launch-intent, clear-runtime, reap-actuator-processes, kill-actuator,
//   reap-session-processes, kill-session, release-ownership, stop-daemon
//
// `stop-daemon` is the LAST of eight steps and two of the seven ahead of it
// kill the actuator and the session. A version check that reached for that
// path would stop every person in the company to update a binary — and would
// override an operator's own wake click, which the wake lease exists to
// protect.
//
// The correct verb already existed: `daemon::stop`, whose own contract says
// "nothing durable is touched — stopping a daemon is not removing a company".
//
// A unit test can assert that panes survive a call. It cannot assert that the
// NEXT author does not reach one module over for the teardown, which is the
// actual risk: both verbs are spelled `stop`, they live in sibling files, and
// the wrong one looks more thorough. So the pane's survival is pinned as a
// property of the VERB the ensure is allowed to name.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const repo = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
const daemonRs = join(repo, 'apps/chiefd/crates/chief-cli/src/daemon.rs')

/** The ensure's own call site, from the log event that names it to the end of its branch. */
function ensureBranch(source) {
  const start = source.indexOf('if let Some(restart) = stale_daemon_restart(published)')
  assert.notEqual(start, -1, 'the version ensure must still be reachable from the adopt path')
  const end = source.indexOf('} else {', start)
  assert.notEqual(end, -1, 'the ensure branch must still be a branch')
  return source.slice(start, end)
}

test('the version ensure stops the daemon with the daemon-only verb', () => {
  const source = readFileSync(daemonRs, 'utf8')
  const branch = ensureBranch(source)
  assert.match(
    branch,
    /\bstop\(client, dir\)\.await/,
    'the ensure must stop the daemon through `daemon::stop`, the verb that touches no pane',
  )
})

test('the version ensure never reaches for the company teardown', () => {
  const source = readFileSync(daemonRs, 'utf8')
  // Comments are allowed to NAME the forbidden path — the one in daemon.rs
  // explains why it is forbidden, and deleting that explanation would be a
  // loss. Code is what is checked.
  const code = source
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('//'))
    .join('\n')
  for (const forbidden of [
    'stop_runtime',
    'kill_runtime_sessions',
    'preserve_daemon',
    'kill-session',
    'kill-actuator',
  ]) {
    assert.equal(
      code.includes(forbidden),
      false,
      `daemon.rs must never call ${forbidden}: it stops people, and a version check may not`,
    )
  }
})

// NON-VACUITY. Every assertion above passes trivially if the anchors are
// renamed or the file moves, so the anchors are asserted to exist. A guard
// that cannot fail is the defect it is meant to catch.
test('the anchors this guard reads still exist', () => {
  const source = readFileSync(daemonRs, 'utf8')
  assert.match(source, /fn stale_daemon_restart\(/, 'the check helper')
  assert.match(source, /fn refuse_if_still_stale\(/, 'the loop floor')
  assert.match(source, /event = "daemon\.build\.stale"/, 'the operator-facing event')
  const stopRs = readFileSync(join(repo, 'apps/chiefd/crates/chief-cli/src/stop.rs'), 'utf8')
  assert.match(
    stopRs,
    /"kill-session"/,
    'the teardown this guard exists to keep away from the ensure must still be the teardown',
  )
})
