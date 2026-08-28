/**
 * A REFUSAL MUST NOT LIE ABOUT WHOSE FAULT IT IS.
 *
 * Card rendering decides between a named business-rule refusal and a raw
 * caught exception by one thing: whether the result carries a `status`. With
 * one, the card states the refusal; without one, it is tagged `(system
 * fault)`.
 *
 * That distinction was defeated by the throw path. Validation refusals were
 * raised as plain `Error`s, and every adapter that catches a throw flattened
 * it into a status-less result — so a deliberate, carefully worded caller
 * refusal reached the renderer indistinguishable from a crash and was labelled
 * as one.
 *
 * The cost is the wrong recovery, not merely a wrong word: **a system fault
 * invites the same call again; a caller error invites a corrected one.** An
 * agent told "(system fault)" for naming a company where a department belongs
 * will retry the identical call, because retrying is what that label means.
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import {
  callerRefusalForTest,
  isCallerRefusalCardForTest,
  refusalResultForTest,
  showsSystemFaultTagForTest
} from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

describe('a decided refusal keeps its classification through the adapters', () => {
  test('a caller refusal carries a status out of the adapter', () => {
    const result = refusalResultForTest(callerRefusalForTest("'acme-capital' names the company"))

    expect(result.details?.status).toBe('refused')
  })

  test('a caller refusal may name its own status', () => {
    const result = refusalResultForTest(callerRefusalForTest('nope', 'recipient_lookup'))

    expect(result.details?.status).toBe('recipient_lookup')
  })

  /**
   * THE DISCRIMINATING HALF. Without this, the test above could be satisfied by
   * giving EVERY failure a status — which would delete the distinction rather
   * than fix it, and would relabel real crashes as business rules. An
   * unexpected exception must still be a system fault, because that is true and
   * because retrying it is the right recovery.
   */
  test('an unexpected exception still has no status, so it still reads as a system fault', () => {
    const result = refusalResultForTest(new Error('chiefd returned an invalid outcome'))

    expect(result.details?.status).toBeUndefined()
  })

  test('a non-Error throw is also treated as a system fault', () => {
    expect(refusalResultForTest('a bare string').details?.status).toBeUndefined()
  })
})

/**
 * THE WIRING, NOT ONLY THE TRANSFORMATION.
 *
 * The tests above pin what `refusalResult` DOES. They cannot notice a catch
 * path that stops calling it: revert any single adapter to flattening the error
 * by hand and every one of them still passes, because the helper is still
 * correct — it is simply no longer on that route.
 *
 * So the rule is enforced mechanically, over the source. That covers the eight
 * catch paths here today AND the ninth somebody adds next month, which is the
 * thing a set of per-adapter functional tests could never do: a new adapter
 * arrives already unpinned.
 *
 * The detectable signature is the flatten idiom appearing as an ARGUMENT to
 * `toolResult` — that is precisely "turn this caught error into a result
 * without asking whether it was a decided refusal". The same idiom is fine
 * elsewhere: building a message for a log line, or inside `refusalResult`
 * itself, which is its sanctioned home.
 */
describe('every catch path funnels through refusalResult', () => {
  const FLATTEN = 'error instanceof Error ? error.message : String(error)'
  const source = readFileSync(
    fileURLToPath(new URL('../extensions/organization-intercom.ts', import.meta.url)),
    'utf8'
  )

  /** Lines where the flatten idiom sits inside a `toolResult(` call. */
  function handFlattenedResults(text: string): string[] {
    return text
      .split('\n')
      .map((line, index) => ({ line, number: index + 1 }))
      .filter(({ line }) => line.includes('toolResult(') && line.includes(FLATTEN))
      .map(({ number, line }) => `${number}: ${line.trim()}`)
  }

  test('no catch path flattens a caught error into a result by hand', () => {
    expect(
      handFlattenedResults(source),
      'These build a result straight from a caught error, so a decided refusal loses its ' +
        'status there and the card calls it a system fault. Return refusalResult(error) instead.'
    ).toEqual([])
  })

  test('the sweep can fail', () => {
    // The discriminating fixture: a synthetic catch path that flattens by hand
    // must be caught. Without this, the rule above would also pass if the
    // detector simply never matched anything.
    const offending = `} catch (error) { return toolResult(false, ${FLATTEN}); }`
    expect(handFlattenedResults(offending)).toHaveLength(1)
  })
})

/**
 * THE VERB FOLLOWS THE CLASSIFICATION.
 *
 * "refused" invites a corrected call; "failed" invites a retry. Which word a
 * card uses is therefore a claim about whose fault the failure was, and the
 * two must not be interchangeable — a crash called "refused" tells a reader to
 * fix a call that was never wrong, which is the #11 defect pointed the other
 * way and worse for it.
 */
describe('a card says refused only when the tool decided it', () => {
  test('a classified refusal is refused', () => {
    expect(isCallerRefusalCardForTest({ status: 'refused' })).toBe(true)
    expect(isCallerRefusalCardForTest({ status: 'incumbent_disposition_required' })).toBe(true)
  })

  /**
   * THE DISCRIMINATING HALF. Without it the rule above passes by returning
   * true for everything — which would relabel every crash a refusal and delete
   * the distinction rather than using it.
   */
  test('an unclassified failure is NOT refused', () => {
    expect(isCallerRefusalCardForTest({})).toBe(false)
    expect(isCallerRefusalCardForTest(undefined)).toBe(false)
  })

  /**
   * A status carried for CONTEXT is not a classification. The partial-hire card
   * names what already landed so a retry does not double-hire; the error it
   * wraps may be a genuine crash, and only the error's own type knows.
   */
  test('a status carried as context with fault:true is NOT refused', () => {
    expect(isCallerRefusalCardForTest({ status: 'hire_partial', fault: true })).toBe(false)
    expect(isCallerRefusalCardForTest({ status: 'hire_partial' })).toBe(true)
  })
})

/**
 * THE VERB AND THE TAG READ THE SAME MARKER.
 *
 * They diverged: the verb moved to the fault marker while the tag still
 * measured only the absence of a status. A partial batch wrapping a real crash
 * therefore said "failed" — correctly — with no crash marker beside the list of
 * people already hired, which reads as bad input to anyone debugging it.
 *
 * One classification, two surfaces, and a test that fails if either moves
 * without the other.
 */
describe('the system-fault tag reads the same marker as the verb', () => {
  test('an unclassified failure carries the tag', () => {
    expect(showsSystemFaultTagForTest({})).toBe(true)
    expect(showsSystemFaultTagForTest(undefined)).toBe(true)
  })

  test('a decided refusal does NOT carry the tag', () => {
    expect(showsSystemFaultTagForTest({ status: 'refused' })).toBe(false)
    expect(showsSystemFaultTagForTest({ status: 'hire_partial' })).toBe(false)
  })

  test('a context-carried status wrapping a crash DOES carry the tag', () => {
    expect(showsSystemFaultTagForTest({ status: 'hire_partial', fault: true })).toBe(true)
  })

  test('verb and tag never disagree about the same detail', () => {
    // The property that matters is not either rule alone, it is that they are
    // complementary: anything called "refused" must not be tagged a fault, and
    // anything tagged a fault must not be called "refused".
    const cases: Array<{ label: string; detail: Record<string, unknown> }> = [
      { label: 'no status', detail: {} },
      { label: 'a decided refusal', detail: { status: 'refused' } },
      { label: 'a context status', detail: { status: 'hire_partial' } },
      {
        label: 'a context status wrapping a crash',
        detail: { status: 'hire_partial', fault: true }
      }
    ]
    for (const { label, detail } of cases) {
      expect(
        isCallerRefusalCardForTest(detail) && showsSystemFaultTagForTest(detail),
        `${label} must not be both a refusal and a system fault`
      ).toBe(false)
    }
  })
})
