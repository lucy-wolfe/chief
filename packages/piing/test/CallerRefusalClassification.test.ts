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
import { callerRefusalForTest, refusalResultForTest } from '@test-assets/organization-intercom'
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
