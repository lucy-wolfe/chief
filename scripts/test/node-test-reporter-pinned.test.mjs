import assert from 'node:assert/strict'
import { readFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'

import { NODE_TEST_REPORTER_ARGS } from '../gate-matrix-legs.mjs'

// EVERY `node --test` THIS REPO SPAWNS ASKS FOR ITS REPORTER.
//
// The default is a Node VERSION fact, not a stable one: through Node 24 a
// non-TTY `node --test` defaulted to TAP; from Node 26 it defaults to `spec`,
// whose failures read `✖ name` and whose tail reads `ℹ tests N`.
//
// #1035 paid for this once already, inside `guard-tree-purity`: its
// executed-count arms parse `# tests N`, read 0 on a Node 26 host, and refused
// five ways about a tree that was never dirty. That fix pinned the reporter for
// ONE nested runner. The sites that run the real suite did not, and
// `ci-guard-shard.mjs` parses the format — it lifts a failing shard's failing
// subtests with `startsWith("not ok ")`, which under `spec` matches nothing. A
// red shard would print the right verdict and an empty diagnosis.
//
// Nothing in CI pins the Node version, so this is not a change anybody here
// would make. It is a change a runner-image bump makes FOR us, with no commit
// in this repo — which is exactly why it needs a guard rather than a habit.

const SCRIPTS = fileURLToPath(new URL('..', import.meta.url))

/** Every `.mjs` under `scripts/`, excluding the guard suite itself. */
function productionScripts() {
  return readdirSync(SCRIPTS, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith('.mjs'))
    .map((entry) => entry.name)
}

/** Lines that spawn `node --test`, with the file they came from. */
function spawnSites() {
  const sites = []
  for (const name of productionScripts()) {
    const text = readFileSync(join(SCRIPTS, name), 'utf8')
    for (const [index, line] of text.split('\n').entries()) {
      if (line.trimStart().startsWith('//') || line.trimStart().startsWith('*')) continue
      if (/["']--test["']/.test(line)) sites.push({ name, line: index + 1, text: line.trim() })
    }
  }
  return sites
}

test('every production `node --test` spawn passes the reporter instead of inheriting it', () => {
  const sites = spawnSites()
  // NON-VACUITY FIRST. A scan that found nothing would pass this file forever,
  // and the whole point is that the sites exist and are easy to add to.
  assert.ok(
    sites.length >= 3,
    `expected the known spawn sites; found ${sites.length}: ${JSON.stringify(sites)}`,
  )
  const unpinned = sites.filter((site) => !site.text.includes('NODE_TEST_REPORTER_ARGS'))
  assert.deepEqual(
    unpinned,
    [],
    'these spawn `node --test` without asking for a reporter, so they parse whatever the ' +
      'host Node happens to emit — spread NODE_TEST_REPORTER_ARGS into the argv',
  )
})

test('the reporter is defined ONCE, and it is the format the shard parser reads', () => {
  // One definition, because the format asked for and the format parsed must be
  // the same fact. A second constant is a second answer.
  const declarations = productionScripts().filter((name) =>
    readFileSync(join(SCRIPTS, name), 'utf8').includes('export const NODE_TEST_REPORTER_ARGS'),
  )
  assert.deepEqual(declarations, ['gate-matrix-legs.mjs'])
  assert.deepEqual(NODE_TEST_REPORTER_ARGS, ['--test-reporter=tap'])

  // AND THE PARSER STILL READS TAP. If a future change moves the reporter, this
  // fails and names the line that has to move with it — rather than leaving a
  // shard silently unable to report its own failures.
  const shard = readFileSync(join(SCRIPTS, 'ci-guard-shard.mjs'), 'utf8')
  assert.ok(
    shard.includes('startsWith("not ok ")'),
    'the shard parser reads TAP; if that changed, the pinned reporter must change with it',
  )
})
