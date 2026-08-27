/**
 * THE RENDEZVOUS' READER INVENTORY IS COMPLETE, OR THIS FAILS.
 *
 * # The defect this exists for
 *
 * `host_primitives::rendezvous`'s module doc used to say the daemon writes the
 * rendezvous "and `chief-cli` READS it" — a two-name inventory of a file with
 * three readers. The third, `packages/chiefing/src/discovery/Rendezvous.ts`,
 * announced itself only inside its own file, in another language, in a package
 * no Rust change touches.
 *
 * On 2026-08-26 a field was added to that record: additive, compatible,
 * reviewed. The unnamed reader refused the record, and every pane in a live
 * company exited 1 at start-up. Nobody was careless — the change touched every
 * reader anyone knew about, and the one that was not named in the surface's own
 * doc was not looked for, because a surface's doc is where an engineer goes to
 * learn who consumes it.
 *
 * **An incomplete reader inventory on a surface's own doc is how a
 * cross-language contract loses a reader.** This guard makes that inventory
 * enforceable instead of aspirational: it finds the files that actually read
 * the record and fails if the doc does not name each one.
 *
 * # Why it greps for CALLS and not imports
 *
 * A barrel that re-exports `readDaemonRendezvous` is not a reader — it hands
 * the function on. A file that CALLS it consumes the record and must be in the
 * inventory. Counting re-exports would fill the list with modules that would
 * not notice this file changing, which is the opposite of the point.
 */
import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test from 'node:test'

const repo = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
const RENDEZVOUS_RS = 'apps/chiefd/crates/host-primitives/src/rendezvous.rs'

/**
 * Every tracked file that CALLS one of these, in code rather than in prose.
 *
 * The comment filter is not fussiness: `packages/chiefing/src/types/Transport.ts`
 * mentions `readDaemonRendezvous(dir).url` inside a doc comment to explain what
 * a field holds. It reads nothing. Reporting it would be a false positive on
 * the guard's first run, and a guard that cries wolf gets deleted — which
 * would cost exactly the inventory this exists to keep.
 */
function callers(patterns) {
  const matcher = new RegExp(patterns.join('|'))
  const out = execFileSync(
    'git',
    ['grep', '-l', '-E', patterns.join('|'), '--', '*.rs', '*.ts'],
    { cwd: repo, encoding: 'utf8' }
  )
  return out
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .filter((path) => !/\.test\.ts$|\/tests?\//.test(path))
    .filter((path) => path !== RENDEZVOUS_RS)
    // The parser file is NOT excluded: it defines `parseDaemonRendezvous`, so
    // it IS the third reader — the one whose absence from the inventory cost a
    // company. Excluding it was this guard's own first version, and a control
    // run proved that version blind to the exact defect it exists for.
    .filter((path) => !/\/index\.ts$/.test(path))
    .filter((path) => {
      const code = readFileSync(join(repo, path), 'utf8')
        .split('\n')
        .filter((line) => {
          const trimmed = line.trimStart()
          return !(
            trimmed.startsWith('//') ||
            trimmed.startsWith('*') ||
            trimmed.startsWith('/*')
          )
        })
        .join('\n')
      return matcher.test(code)
    })
}

test('every reader of the daemon rendezvous is named in its own module doc', () => {
  const doc = readFileSync(join(repo, RENDEZVOUS_RS), 'utf8')
  // The doc's inventory section, not the whole file: a path mentioned in a
  // tombstone or an example would otherwise satisfy this by accident.
  const inventory = /WHO WRITES IT AND WHO READS IT([\s\S]*?)\n\/\/! #/.exec(doc)?.[1] ?? ''
  assert.notEqual(inventory.length, 0, 'the inventory section must exist to be checked')

  const readers = callers([
    // TypeScript: the two exported entry points, called rather than re-exported.
    'readDaemonRendezvous\\(',
    'parseDaemonRendezvous\\(',
    // Rust: the client's own reader.
    'read_rendezvous\\('
  ])
  // THE BLINDNESS FLOOR IS PER LANGUAGE, NOT AGGREGATE.
  //
  // It was one `readers.length >= 3` check, and that check could not fail for
  // the case it existed to catch: the three TypeScript callers satisfy a total
  // of three on their own, so renaming `read_rendezvous` on the Rust side
  // would drop the Rust arm out of this guard SILENTLY — the floor still
  // passes, `missing` only ever inspects files that were FOUND, and the
  // instrument quietly becomes TypeScript-only while reporting green. Delete
  // the Rust pattern above and the aggregate version of this file stays green,
  // which is the definition of a guard whose control run passes.
  //
  // One floor per language, each with its own sentence, so a rename on either
  // side is a red that names the side.
  const rustReaders = readers.filter((path) => path.endsWith('.rs'))
  const typescriptReaders = readers.filter((path) => path.endsWith('.ts'))
  assert.ok(
    rustReaders.length >= 1,
    `no RUST reader of the rendezvous was found — \`read_rendezvous\` was probably renamed, so this guard has gone blind on the Rust side rather than the readers having gone away. Found: ${readers.join(', ')}`
  )
  assert.ok(
    typescriptReaders.length >= 2,
    `fewer than two TYPESCRIPT readers were found — \`readDaemonRendezvous\`/\`parseDaemonRendezvous\` were probably renamed, so this guard has gone blind on the TypeScript side rather than the readers having gone away. Found: ${readers.join(', ')}`
  )

  const missing = readers.filter((path) => !inventory.includes(path))
  assert.deepEqual(
    missing,
    [],
    `these files read the daemon rendezvous and are NOT named in its inventory (${RENDEZVOUS_RS}):\n` +
      `${missing.join('\n')}\n\n` +
      'Add them there in this commit. A reader that announces itself only inside its own file ' +
      'announces itself to nobody — that is how an additive field killed a live company.'
  )
})

test('the inventory names the reader whose absence caused the outage', () => {
  const doc = readFileSync(join(repo, RENDEZVOUS_RS), 'utf8')
  assert.match(
    doc,
    /packages\/chiefing\/src\/discovery\/Rendezvous\.ts/,
    'the TypeScript reader must stay named: it is the one that was missing'
  )
  assert.match(
    doc,
    /packages\/piing\/extensions\/organization-intercom\.ts/,
    'and the extension that reaches it, because that is what an engineer greps for'
  )
})
