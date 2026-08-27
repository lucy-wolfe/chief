import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, test } from 'vitest'

// #827 (E8-S5): the last two synchronous agent-thread parks
// (`synchronousTransientBackoff` / `Atomics.wait`, and the sync
// `chiefdReadDocument` curl-blocking reader) are deleted, along with the
// inline retry micro-ladder. Every retry path is `withTransientReadRetryAsync`
// / `asynchronousTransientBackoff`.
//
// Most of this was already done by #794 (E4-S8)'s transport migration
// landing before #827's own edits — verified here as a standing regression
// so a future change cannot silently reintroduce a blocking wait on this
// process's own JS thread (which would freeze the pane's UI, SSE reader, and
// mailbox drain simultaneously).

const PACKAGE_ROOT = fileURLToPath(new URL('../..', import.meta.url))
const INTERCOM_SOURCE = readFileSync(
  join(PACKAGE_ROOT, 'extensions/organization-intercom.ts'),
  'utf8'
)
const TEAM_UI_SOURCE = readFileSync(join(PACKAGE_ROOT, 'extensions/team-ui.ts'), 'utf8')

// Absence assertions below must check real call/declaration shape, never
// bare text — doc comments legitimately name a deleted symbol in backticks
// as history (e.g. chiefdReadDocumentAsync's own comment names the old
// blocking `spawnSync("curl")` transport it replaced). Stripping comments
// before matching keeps the assertion honest against source, not prose.
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '')
}

describe('NoAgentThreadParks: no blocking wait on the agent process JS thread', () => {
  test('organization-intercom.ts has no Atomics.wait / synchronousTransientBackoff', () => {
    // Actual usage, not bare mention: the async-ladder doc comments
    // legitimately name both deleted symbols in backticks explaining why
    // the file no longer uses them, so these checks target a real call/
    // declaration shape rather than any string occurrence.
    expect(INTERCOM_SOURCE).not.toMatch(/\bAtomics\.wait\s*\(/)
    expect(INTERCOM_SOURCE).not.toMatch(/\bsynchronousTransientBackoff\s*\(/)
    expect(INTERCOM_SOURCE).not.toContain('function synchronousTransientBackoff')
    expect(INTERCOM_SOURCE).not.toContain('const synchronousTransientBackoff')
  })

  test('every retry path uses the async ladder', () => {
    expect(INTERCOM_SOURCE).toContain('asynchronousTransientBackoff')
    expect(INTERCOM_SOURCE).toContain('withTransientReadRetryAsync')
  })

  // TOMBSTONE: `retryCoalescedWake`. The sender-side bounded retry existed to
  // re-attempt a message wake that a concurrent reconcile had coalesced away.
  // The wake it retried was a COMPANY-WIDE `/v1/org/runtime/launch`, which only
  // the head of the root department may post, so for every non-executive sender
  // the retry re-ran a refusal three times. The delivery itself now nudges
  // chiefd's reconcile duty (`org_mailbox_delta` -> `wake_reconcile`), so there
  // is no sender-side wake left to coalesce, defer, or retry — and no async
  // `setTimeout` closure on this thread either, which is what this file guards.
  test('no sender-side wake retry ladder survives on the agent thread', () => {
    expect(INTERCOM_SOURCE).not.toContain('retryCoalescedWake')
    expect(INTERCOM_SOURCE).not.toContain('message-wake-coalesced')
  })

  test('team-ui.ts has no blocking chiefdReadDocument (spawnSync-curl); the sole reader is async', () => {
    // Actual usage, not bare mention: chiefdReadDocumentAsync's own doc
    // comment legitimately names the old blocking transport ("blocking
    // spawnSync(\"curl\")") as history explaining the replacement — matched
    // against comment-stripped source so that history doesn't trip the
    // absence check (see stripComments above).
    expect(stripComments(TEAM_UI_SOURCE)).not.toMatch(/\bspawnSync\s*\(/)
    expect(TEAM_UI_SOURCE).toMatch(/async function chiefdReadDocument\(/)
    expect(TEAM_UI_SOURCE).toContain('chiefdReadDocumentAsync')
  })
})
