// GUARD: `.claude/statusline.sh` survives fractional percentages.
//
// #922: Claude Code reports `used_percentage` as a FLOAT. The status line fed
// those floats to integer-only `[ ]` and `$(( ))`, so every comparison they
// feed silently stopped participating — no error a human saw, because the
// harness discards the script's stderr, and no missing output either, because a
// failed comparison just takes the false branch. Segments vanished and nobody
// could say why.
//
// WHY THIS IS A `.test.mjs` AND NOT THE `.sh` IT REPLACES
// ------------------------------------------------------
// This proof existed as `scripts/test/statusline-fractional-percentages.sh`,
// sitting inside the guard directory, and NOTHING RAN IT. Its own header said
// so, in as many words — "NOT WIRED TO ANY GATE, DELIBERATELY UNTESTABLE BY
// THIS REPO'S TOOLING" — and listed the six instruments that cannot see
// `.claude/`. That was a true observation about six scanners and a false
// conclusion about this repo: the corpus that runs every gate is
// `scripts/test/*.test.mjs`, derived from a directory listing, and it can spawn
// anything. A `.sh` file in that same directory is invisible to that derivation
// and to `guard-wiring.test.mjs`, which is how a guard came to be parked in the
// guard directory.
//
// The subject is live: `.claude/settings.json` runs `.claude/statusline.sh` on
// every render, in every session anyone opens in this checkout.
//
// Run with `node --test scripts/test/statusline-fractional-percentages.test.mjs`.

import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const REPO_ROOT = fileURLToPath(new URL('../..', import.meta.url))
const STATUSLINE = fileURLToPath(new URL('../../.claude/statusline.sh', import.meta.url))

/** The only input shape that reproduces #922: every percentage fractional. */
const FRACTIONAL = {
  cwd: '/tmp/repo',
  model: { display_name: 'Opus 5' },
  context_window: { total_input_tokens: 612000, context_window_size: 1000000, used_percentage: 61.2 },
  cost: { total_duration_ms: 920000, total_lines_added: 12, total_lines_removed: 3 },
  effort: { level: 'medium' },
  rate_limits: { five_hour: { used_percentage: 11.5 }, seven_day: { used_percentage: 3.25 } },
  session_id: 'sess-fractional',
}

/** The same shape with integers, to prove the working path is unchanged. */
const INTEGER = {
  ...FRACTIONAL,
  context_window: { total_input_tokens: 612000, context_window_size: 1000000, used_percentage: 61 },
  rate_limits: { five_hour: { used_percentage: 9 }, seven_day: { used_percentage: 88 } },
  session_id: 'sess-integer',
}

function render(fixture) {
  const result = spawnSync('sh', [STATUSLINE], {
    input: JSON.stringify(fixture),
    encoding: 'utf8',
    cwd: REPO_ROOT,
  })
  return { stdout: result.stdout ?? '', stderr: result.stderr ?? '', status: result.status }
}

test('the subject exists and jq is installed — this proof is vacuous without either', () => {
  assert.ok(existsSync(STATUSLINE), '.claude/statusline.sh is the subject; without it this guard tests nothing')
  // Deliberately a FAILURE, not a skip. `.claude/statusline.sh` has its own
  // no-jq fallback that prints a bare path, and under that fallback every
  // fixture below renders identically and passes -- a green from a run that
  // exercised none of the code this guard is about.
  const jq = spawnSync('sh', ['-c', 'command -v jq'], { encoding: 'utf8' })
  assert.equal(jq.status, 0, 'jq is not installed: the status line would take its bare-path fallback and every assertion below would pass vacuously')
})

test('a fractional fixture renders every segment and writes nothing to stderr', () => {
  const { stdout, stderr } = render(FRACTIONAL)
  assert.equal(stderr, '', `fractional fixture wrote to stderr, which the harness discards: ${stderr}`)
  assert.match(stdout, /session/, "the 'session' segment is missing — its comparison silently did not participate")
  assert.match(stdout, /week/, "the 'week' segment is missing — its comparison silently did not participate")
  assert.match(stdout, /61%/, 'the context percentage was not truncated to an integer')
})

test('an all-integer fixture is unchanged: the fix did not move the working path', () => {
  const { stdout, stderr } = render(INTEGER)
  assert.equal(stderr, '', `integer fixture wrote to stderr: ${stderr}`)
  assert.match(stdout, /91% session/, `expected '91% session' (100 - 9), got: ${stdout}`)
  assert.match(stdout, /12% week/, `expected '12% week' (100 - 88), got: ${stdout}`)
})
