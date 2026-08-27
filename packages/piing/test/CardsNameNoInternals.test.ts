/**
 * NO CARD NARRATES CHIEFD'S INTERNAL MACHINERY TO AN AGENT.
 *
 * Operator ruling, 2026-08-13, on the sentence every department create carried
 * ("chiefd's reconciler has already been signalled; it owns the runtime and is
 * bringing it up now"): *"Why are we showing this? Do we have a reconciler? We
 * shouldn't have. We just have a CLI that projects whatever comes out of
 * chiefd's API."*
 *
 * An agent acts on people and departments. It cannot act on a convergence pass,
 * a single-flight window, or a reconcile loop, so a sentence about one changes
 * nothing it does next and is pure noise on a routine success. The honest facts
 * a caller CAN act on — the change is durable, the pane is not up yet, do not
 * retry the write — stay; the mechanism that delivers them does not.
 *
 * The ban is on the SHAPE, not on any one sentence, because the sentences get
 * edited and the rule does not. Comments are stripped first, so a hit here is
 * live copy an agent can read; the doc comments that explain WHY these words are
 * absent are deliberately still allowed to use them.
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { withoutComments } from '@test/support/TypeScriptSource'
import { describe, expect, test } from 'vitest'

const SOURCE = withoutComments(
  readFileSync(
    fileURLToPath(new URL('../extensions/organization-intercom.ts', import.meta.url)),
    'utf8'
  )
)

/**
 * Machinery an agent cannot act on. Each entry was live card copy at some
 * point: the reconciler on every create and every interrupted teardown, "on the
 * next convergence" on every hire and recall, "disk-authoritative" on the
 * roster (state is SQL in chiefd, not disk).
 */
const BANNED = [
  'reconciler',
  'next convergence',
  'reconcile loop',
  'single-flight',
  'convergence pass',
  'disk-authoritative'
] as const

describe('a card states what the caller can act on, never chiefd internals', () => {
  for (const phrase of BANNED) {
    test(`no live copy says "${phrase}"`, () => {
      expect(SOURCE).not.toContain(phrase)
    })
  }
})

/**
 * Prose, roughly: every double-quoted literal long enough to be a sentence
 * rather than an id, a route or a status code. Comments are already stripped,
 * so what is left is what an agent reads.
 */
const PROSE = [...SOURCE.matchAll(/"((?:[^"\\\n]|\\.){60,})"/g)].flatMap((match) =>
  typeof match[1] === 'string' ? [match[1]] : []
)

/**
 * Every tool name this file declares. A bare `"org_x"` literal — the `name:`
 * field, or one arm of the union a shared registrar takes (`org_stop_department
 * | org_remove_department | …`) — is a declaration; the long literals are prose
 * and are excluded above.
 */
const REGISTERED = new Set(
  [...SOURCE.matchAll(/"(org_[a-z_]+)"/g)].flatMap((match) =>
    typeof match[1] === 'string' ? [match[1]] : []
  )
)

describe('a card never sends the caller to something that does not exist', () => {
  test('the prose corpus is not empty', () => {
    // Non-vacuity: a regex that matched nothing would pass every assertion
    // below while checking no card at all.
    expect(PROSE.length).toBeGreaterThan(30)
    expect(REGISTERED.size).toBeGreaterThan(15)
  })

  test('every org_ verb a card names is a tool that is registered here', () => {
    // `org_start_person` advertised `org_new_session`, a tool that has never
    // existed, and `org_maintain_session` advertised `org_start_person with
    // newSession: true`, a parameter #751/P3 deleted — so the two cards for
    // "give somebody clean context" pointed at each other through two dead
    // names, and the verb that does the work was named by neither.
    const dangling = new Set<string>()
    for (const line of PROSE) {
      for (const match of line.matchAll(/\borg_[a-z_]+/g)) {
        if (!REGISTERED.has(match[0])) dangling.add(match[0])
      }
    }
    expect([...dangling], 'a card names a tool nobody registers').toEqual([])
  })

  test('no card asks the caller for a rationale', () => {
    // #1093 deleted the justification a selected Pi resource used to cost, and
    // `person_resources.rationale` is a retired column — yet the hire failure
    // card still told the model to send "exact catalog ids plus matching
    // rationales", which is a turn spent on a field no route accepts.
    for (const line of PROSE) expect(line).not.toMatch(/rationale|justification/i)
  })

  test('no card offers the deleted newSession parameter', () => {
    // Pi's own native session API still carries a `newSession` field, which is
    // why this reads the PROSE and not the whole file.
    for (const line of PROSE) expect(line).not.toContain('newSession')
  })
})
