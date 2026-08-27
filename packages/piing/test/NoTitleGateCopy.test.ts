/**
 * NO REFUSAL MAY TELL A CALLER THAT A TOOL IS "CEO-LEVEL".
 *
 * A live CEO told its operator that its Chief of Staff "doesn't hold the
 * org-management tools needed to create a department or hire a department head
 * — those are CEO/head-level functions". It had learned that model somewhere,
 * and the product taught it: three handlers refused with "Only the CEO or a
 * department head may …", which reads as a general rule about who may do what.
 *
 * `AGENTS.md` (operator ruling, 2026-08-13) bans the phrasing outright:
 * "Authority is the subtree you head, never the job title … Never tell a caller
 * that a tool is 'CEO-level' or 'head-level' — no such gate exists."
 *
 * The GATE on those three tools is GONE as of the B1 gate removal — their
 * routes are fenced server-side, so the tools moved to the subtree catalog and
 * the kind checks came out. What survives here is the phrasing ban, which was
 * never about those three: no refusal anywhere in this file may assert a job
 * title, whatever the caller lacks.
 *
 * This suite fences the phrasing rather than the wording of any one sentence,
 * because the sentence will be edited again and the ban is on the SHAPE.
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

/** Phrases that assert a job-title gate. Commentary is stripped above, so a
 *  hit here is live copy an agent can read. */
const BANNED = ['Only the CEO or a department head', 'CEO-level', 'head-level'] as const

describe('a refusal names a missing subtree, never a job title', () => {
  for (const phrase of BANNED) {
    test(`no live copy says "${phrase}"`, () => {
      expect(SOURCE).not.toContain(phrase)
    })
  }

  test('there is no kind-gated refusal left to word badly', () => {
    // This pinned THREE such refusals and required each to name the way
    // through. All three are gone: their routes are fenced server-side, so the
    // tools moved to the subtree catalog and the checks came out with them.
    //
    // The assertion INVERTS rather than being deleted, and that is deliberate.
    // The banned phrasing above can be satisfied by deleting a sentence; this
    // is what stops the GATE coming back with a politer sentence attached.
    const refusals = SOURCE.split('\n').filter(
      (line) => line.includes('if (!manager(') && line.includes('throw new Error(')
    )
    expect(refusals, 'a handler that refuses by KIND is a role gate returning').toHaveLength(0)
  })
})
