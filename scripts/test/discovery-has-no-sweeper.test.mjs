// beacond never expires, sweeps or reaps a registration on a timer. Removal
// is an explicit verb, and liveness is judged by the READER.
//
// # Why this is an ABSENCE pin, and why an absence needs one
//
// beacond has tests for its explicit deletes. Nothing pinned the ABSENCE of
// an expiry sweeper, because the code that would violate the rule does not
// exist yet -- so there was nothing to write an assertion about, and the rule
// lived in a module doc comment, which is prose a future change reads past.
//
// The rule: a registration is removed by somebody CALLING a removal verb.
// Nothing removes one because time passed. `last_seen_at` is a REPORT a
// reader may use, never an input to a reaper, and the reader's real
// discriminator is pid plus process start time -- an entry for a dead process
// is judged dead when it is read, not deleted on a schedule by a third party.
//
// # Why it matters more than it looks
//
// A TTL is the reflex fix for the first stale row anybody sees, and it is
// wrong here in a way that only shows up under load: the row a sweeper
// deletes may belong to a company that is slow, not dead, and deleting it
// makes the daemon unreachable to every client while the daemon itself is
// perfectly healthy. Worse, a sweeper is a background loop, which the
// reactive mandate bans outright -- so the fix would breach two rules and
// look like housekeeping.
//
// # The instrument
//
// A grep over beacond's own sources for the constructs a time-based reaper
// needs. This is a SHAPE check, not a name check: a sweeper cannot be written
// without a periodic driver or a deadline comparison, whatever it is called.

import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { dirname, join } from 'node:path'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const root = join(__dirname, '..', '..')
const BEACOND = 'apps/chiefd/crates/beacond/src'

/**
 * The constructs a time-driven reaper cannot be written without.
 *
 * Named by SHAPE rather than by identifier: somebody adding one will not call
 * it `sweep`, they will call it `prune_stale` or `gc` or nothing at all. But
 * they cannot avoid a periodic driver or an elapsed-time comparison.
 *
 * @type {ReadonlyArray<{pattern: string, why: string}>}
 */
export const REAPER_CONSTRUCTS = [
  {
    pattern: 'tokio::time::interval',
    why: 'A periodic driver. Also banned workspace-wide by clippy.toml; named here too because this crate is where a registry sweeper would be written.',
  },
  {
    pattern: 'tokio::spawn',
    why: 'A detached background task. beacond answers requests; it runs nothing on its own behalf, so a spawned task here is a loop nobody asked for.',
  },
  {
    pattern: 'elapsed\\(\\)',
    why: 'An elapsed-time comparison, which is how a deadline is tested. Liveness here is pid plus process start time, judged by the reader, never a duration.',
  },
  {
    pattern: 'DEFAULT_TTL|_TTL|TTL_|MAX_AGE|STALE_AFTER|EXPIRY|EXPIRES_AT',
    why: 'A time-to-live constant. There is no expiry: removal is an explicit verb.',
  },
]

/** Every beacond source line matching `pattern`, excluding `#[cfg(test)]`
 *  bodies -- test code may drive a clock to prove something about a reader. */
export function reaperHits(pattern, cwd = root) {
  let files
  try {
    files = execFileSync('git', ['ls-files', '--', BEACOND], { cwd, encoding: 'utf8' })
      .split('\n')
      .filter((path) => path.endsWith('.rs'))
  } catch (error) {
    throw new Error(
      `CANNOT CHECK: cannot enumerate ${BEACOND} (${error.message}). This guard has not passed, it has ` +
        'not run -- silence is never the green.'
    )
  }
  if (files.length === 0) {
    throw new Error(
      `CANNOT CHECK: ${BEACOND} holds no Rust sources. The crate moved or the path is wrong; refusing to ` +
        'report a clean result over an empty search.'
    )
  }
  const regex = new RegExp(pattern)
  const hits = []
  for (const path of files) {
    const production = readFileSync(join(cwd, path), 'utf8').split('#[cfg(test)]')[0]
    for (const [index, line] of production.split('\n').entries()) {
      // A comment SAYING there is no sweeper is not a sweeper. This guard's
      // whole subject is prose-adjacent, so it must not fire on the prose.
      const code = line.split('//')[0]
      if (regex.test(code)) hits.push(`${path}:${index + 1}:${line.trim()}`)
    }
  }
  return hits
}

test('beacond carries no time-driven reaper', () => {
  const offences = []
  for (const { pattern, why } of REAPER_CONSTRUCTS) {
    const hits = reaperHits(pattern, root)
    if (hits.length > 0) offences.push(`${pattern}:\n  ${hits.join('\n  ')}\n  WHY BANNED: ${why}`)
  }
  assert.deepEqual(
    offences,
    [],
    `beacond has grown a time-driven reaper:\n\n${offences.join('\n\n')}\n\n` +
      'Removal is an explicit verb and liveness is judged by the READER from pid plus process start ' +
      'time. A row that looks stale may belong to a company that is slow, not dead, and deleting it ' +
      'makes a healthy daemon unreachable to every client.'
  )
})

test('every banned construct states WHY', () => {
  for (const row of REAPER_CONSTRUCTS) {
    assert.ok(row.why.length > 40, `${row.pattern} has no real reason recorded`)
  }
})

test('NON-VACUITY: the search reaches real sources, and a planted reaper WOULD be caught', () => {
  // The failure mode this guard is most exposed to: a path that resolves to
  // nothing reports a clean crate for ever. So prove the search sees code.
  const anyCode = reaperHits('fn ', root)
  assert.ok(
    anyCode.length > 10,
    `the search found ${anyCode.length} function definitions in ${BEACOND} -- too few to be this crate, ` +
      'so the search is not reading what it claims to read'
  )
  // And prove the patterns discriminate, on text rather than on the tree.
  const planted = 'let mut ticker = tokio::time::interval(Duration::from_secs(60));'
  assert.ok(
    REAPER_CONSTRUCTS.some(({ pattern }) => new RegExp(pattern).test(planted)),
    'a planted periodic driver must match at least one banned construct'
  )
  const prose = '//! **No background loop.** beacond never expires, sweeps or reaps a row on a timer.'
  assert.ok(
    !REAPER_CONSTRUCTS.some(({ pattern }) => new RegExp(pattern).test(prose.split('//')[0])),
    'the comment stating the rule must never be reported as breaking it'
  )
})
