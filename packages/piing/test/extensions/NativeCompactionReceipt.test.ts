/**
 * THE RECEIPT THAT ASKED WHO WROTE THE SUMMARY INSTEAD OF WHETHER THE WORK
 * HAPPENED.
 *
 * An operator or a manager issues `org_maintain_session action=compact`. The
 * tool answers "Compaction queued for @ceo.", Pi really compacts the session —
 * and the supervision ledger terminalises the request `failed` at the next
 * session start. The product did the work and then told its own operator the
 * work failed.
 *
 * # The witness that was wrong, established from both implementations
 *
 * Both receipt paths demanded the persisted compaction entry's
 * `fromHook === true`. That field does not say "a compaction happened". Pi's
 * `AgentSession` sets `fromExtension = true` ONLY when a
 * `session_before_compact` emit returns a `compaction`
 * (`core/agent-session.js`, both compaction sites), passes it to
 * `sessionManager.appendCompaction(...)`, and `core/session-manager.js:783`
 * persists that same boolean under the entry's `fromHook` field.
 * `AgentHarness` does the identical thing under the identical two names —
 * `9361d097d` established that they are ONE predicate. It claims *"an
 * extension supplied the summary"*.
 *
 * NOTHING in this repository registers `session_before_compact`. The intercom's
 * own compact call hands `customInstructions` to PI's summarizer, so the
 * receipt was demanding a fact that contradicted the call it was receipting.
 * The witness was unsatisfiable on every host, tmux included.
 *
 * # The question the receipt actually asks
 *
 * "Did the compaction we asked for happen — in the session we claimed, at the
 * anchor we recorded, exactly once?" `appendCompaction` sets
 * `parentId: this.leafId`, and the intercom records that same leaf as
 * `compactAnchorEntryId` when it claims the request. So the entry's PARENT is
 * the witness, it always was, and nothing is added to hold the answer.
 *
 * # What this file is
 *
 * `nativeCompactionProof` is the whole decision. All three receipt sites read
 * it and map it straight through — the pre-invoke check (skip a compaction that
 * already happened), the `onComplete` receipt, and the session-start
 * crash-recovery receipt that is what actually wrote `failed`. The
 * `session_compact` in-flight receipt shares its entry predicate. So the cases
 * below are the terminal states, one row per thing that can be true of a
 * session, and the source fence at the bottom pins the mapping and the absence
 * of the retired witness at every site.
 */
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { withoutComments } from '@test/support/TypeScriptSource'
import { nativeCompactionProof } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

const PACKAGE_ROOT = fileURLToPath(new URL('../..', import.meta.url))
const SOURCE = readFileSync(join(PACKAGE_ROOT, 'extensions/organization-intercom.ts'), 'utf8')
const SOURCE_CODE = withoutComments(SOURCE)

const SESSION = 'session-under-compaction'
const ANCHOR = 'entry-anchor'
/** The durable sentinel a request carries when it claimed an EMPTY session, so
 *  the compaction Pi appends is parented at the root (`parentId: null`). */
const EMPTY_SESSION_ANCHOR = '<session-root>'

interface Entry {
  readonly type: string
  readonly id: string
  readonly parentId: string | null
  /** Present only when a `session_before_compact` handler supplied the summary.
   *  Nothing in this product registers that hook, so on every real run this is
   *  absent — which is the entire defect. */
  readonly fromHook?: boolean
}

/** A durable `compact` request as chiefd's ledger holds it, claimed against
 *  `SESSION` at leaf `ANCHOR`. */
function compactRequest(overrides: Record<string, unknown> = {}): never {
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // `SessionMaintenanceRequest` is a wide chiefd-owned shape; this fixture
  // supplies the four fields `nativeCompactionProof` reads and nothing else,
  // exactly as `OrgSessionMaintenanceReceipt.test.ts` builds its records.
  return {
    id: 'session-maintenance:7:ceo:compact',
    action: 'compact',
    personId: 'ceo',
    status: 'running',
    compactSessionId: SESSION,
    compactAnchorEntryId: ANCHOR,
    ...overrides
  } as never
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

/** An `ExtensionContext` carrying the one thing the proof reads: Pi's live
 *  session manager, answering with the session id and the append-ordered
 *  entries `getEntries()` returns. */
function context(session: string, entries: readonly Entry[]): never {
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Pi's `ExtensionContext` is a large concrete surface; the function under
  // test reaches exactly `sessionManager.getSessionId()` and
  // `sessionManager.getEntries()`, which is what Pi's loader hands it.
  return {
    sessionManager: {
      getSessionId: () => session,
      getEntries: () => [...entries]
    }
  } as never
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

/** The transcript at the moment the request is claimed: work, then the leaf
 *  that becomes the anchor. */
const BEFORE_COMPACTION: readonly Entry[] = [
  { type: 'message', id: 'entry-1', parentId: null },
  { type: 'message', id: ANCHOR, parentId: 'entry-1' }
]

/** The compaction Pi writes with its OWN summarizer: parented at the anchor,
 *  and carrying NO `fromHook`, because no extension supplied the summary. */
const PI_SUMMARIZED: Entry = { type: 'compaction', id: 'entry-compaction', parentId: ANCHOR }

describe('a durable compact request terminalises on what happened', () => {
  test('THE DEFECT: Pi compacted with its own summarizer, and that is a PROVEN receipt', () => {
    // Seen red against `9361d097d`: this exact case answered `ambiguous`,
    // which the session-start receipt maps to `finish status=failed` with
    // "Native compaction receipt diverged from the persisted Pi session
    // anchor" — for a compaction that succeeded.
    expect(
      nativeCompactionProof(
        context(SESSION, [...BEFORE_COMPACTION, PI_SUMMARIZED]),
        compactRequest()
      )
    ).toEqual({ state: 'proven', entryId: 'entry-compaction' })
  })

  test('an EMPTY session compacts at the root anchor and is proven the same way', () => {
    expect(
      nativeCompactionProof(
        context(SESSION, [{ type: 'compaction', id: 'entry-root-compaction', parentId: null }]),
        compactRequest({ compactAnchorEntryId: EMPTY_SESSION_ANCHOR })
      )
    ).toEqual({ state: 'proven', entryId: 'entry-root-compaction' })
  })

  test('an extension-supplied summary proves it too — the predicate is indifferent to WHO wrote it', () => {
    // The honest negative, inverted. `fromHook` is not read at all now, so the
    // one case it used to admit still passes: if this repository ever does
    // register `session_before_compact`, its compaction is receipted by the
    // same anchor as Pi's own, and neither is privileged over the other.
    expect(
      nativeCompactionProof(
        context(SESSION, [...BEFORE_COMPACTION, { ...PI_SUMMARIZED, fromHook: true }]),
        compactRequest()
      )
    ).toEqual({ state: 'proven', entryId: 'entry-compaction' })
  })
})

describe('the honest negatives: a compaction that did not happen is never receipted as one', () => {
  test('nothing appended after the anchor is ABSENT — the caller invokes compact, it does not complete', () => {
    expect(nativeCompactionProof(context(SESSION, BEFORE_COMPACTION), compactRequest())).toEqual({
      state: 'absent'
    })
  })

  test('a compaction parented somewhere else is AMBIGUOUS, never proof of ours', () => {
    expect(
      nativeCompactionProof(
        context(SESSION, [
          ...BEFORE_COMPACTION,
          { type: 'compaction', id: 'entry-other', parentId: 'entry-1' }
        ]),
        compactRequest()
      )
    ).toEqual({ state: 'ambiguous' })
  })

  test('TWO compactions at the anchor are AMBIGUOUS — the refusal to compact twice is preserved', () => {
    expect(
      nativeCompactionProof(
        context(SESSION, [
          ...BEFORE_COMPACTION,
          PI_SUMMARIZED,
          { type: 'compaction', id: 'entry-compaction-2', parentId: ANCHOR }
        ]),
        compactRequest()
      )
    ).toEqual({ state: 'ambiguous' })
  })

  test('a session that is no longer the claimed one is AMBIGUOUS whatever it contains', () => {
    expect(
      nativeCompactionProof(
        context('some-other-session', [...BEFORE_COMPACTION, PI_SUMMARIZED]),
        compactRequest()
      )
    ).toEqual({ state: 'ambiguous' })
  })

  test('ordinary work appended after the anchor, with no compaction, is AMBIGUOUS', () => {
    expect(
      nativeCompactionProof(
        context(SESSION, [
          ...BEFORE_COMPACTION,
          { type: 'message', id: 'entry-later', parentId: ANCHOR }
        ]),
        compactRequest()
      )
    ).toEqual({ state: 'ambiguous' })
  })

  test('an anchor that is not in the transcript at all is AMBIGUOUS', () => {
    expect(
      nativeCompactionProof(
        context(SESSION, [{ type: 'message', id: 'entry-1', parentId: null }]),
        compactRequest()
      )
    ).toEqual({ state: 'ambiguous' })
  })

  test('a request with no claimed session or anchor is ABSENT, not a receipt', () => {
    expect(
      nativeCompactionProof(
        context(SESSION, [...BEFORE_COMPACTION, PI_SUMMARIZED]),
        compactRequest({ compactSessionId: undefined })
      )
    ).toEqual({ state: 'absent' })
    expect(
      nativeCompactionProof(
        context(SESSION, [...BEFORE_COMPACTION, PI_SUMMARIZED]),
        compactRequest({ compactAnchorEntryId: undefined })
      )
    ).toEqual({ state: 'absent' })
  })

  test('a request that is not a compaction is ABSENT', () => {
    expect(
      nativeCompactionProof(
        context(SESSION, [...BEFORE_COMPACTION, PI_SUMMARIZED]),
        compactRequest({ action: 'fresh_session' })
      )
    ).toEqual({ state: 'absent' })
  })

  test('no live session manager at all is AMBIGUOUS, never proof', () => {
    expect(nativeCompactionProof(undefined, compactRequest())).toEqual({ state: 'ambiguous' })
  })
})

describe('the retired witness is gone from every receipt site, not just the one that was read', () => {
  test('`fromHook` and `fromExtension` appear in no CODE in the intercom', () => {
    // Comments are stripped first, deliberately: the tombstone above
    // `isAnchoredNativeCompactionEntry` quotes both names, and the account of
    // how a mechanism failed is the most valuable documentation in the file
    // that carries it. A guard that forced its own explanation to be deleted
    // would be traded away the first time somebody needed it.
    //
    // Counted rather than `not.toContain`ed: a containment failure prints the
    // whole 14k-line file as its diff, which is a red nobody can read.
    const mentions = (needle: string): number => SOURCE_CODE.split(needle).length - 1
    expect(
      mentions('fromHook'),
      'the retired witness is back in CODE. `fromHook` means "an extension supplied the ' +
        'summary", never "a compaction happened", and nothing registers session_before_compact.'
    ).toBe(0)
    expect(mentions('fromExtension'), "same field, Pi's name for it").toBe(0)
    // …and the fence is not vacuous: these are the two shapes it catches.
    expect(withoutComments('  // entry.fromHook !== true\n  x.fromHook !== true\n')).toContain(
      'fromHook'
    )
    expect(
      withoutComments('/* event.fromExtension */\nif (event.fromExtension !== true) return;\n')
    ).toContain('fromExtension')
  })

  test('the in-flight `session_compact` receipt decides with the same anchored-entry predicate', () => {
    // The one receipt path this file's cases cannot reach directly: it lives
    // inside `installOrganizationIntercom`'s closure and only fires while a
    // native compaction lease is held. It is pinned to the tested predicate
    // rather than re-deriving the rule, so there is one answer-holder.
    expect(SOURCE_CODE).toContain('pi.on("session_compact"')
    expect(SOURCE_CODE.split('pi.on("session_compact"')[1]?.slice(0, 1200) ?? '').toContain(
      '!isAnchoredNativeCompactionEntry(entry, expectedParent)'
    )
  })

  test('a proven receipt maps to `completed` and every other state to a non-success', () => {
    // The startup crash-recovery site: this is the mapping that wrote `failed`
    // for a compaction that had succeeded.
    const startup = SOURCE_CODE.split('const runningCompact =')[1]?.slice(0, 900) ?? ''
    expect(startup).toContain('const proof = nativeCompactionProof(ctx, runningCompact)')
    expect(startup).toContain('proof.state === "proven" ? {')
    expect(startup).toContain('status: "completed"')
    expect(startup).toContain('status: "failed"')
  })
})
