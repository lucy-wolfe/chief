/**
 * THE CARD, DELIVERED — not the card, constructed.
 *
 * `ProviderFailureCounterIgnoresAPermanentRefusal.test.ts` proves a context
 * overflow is classified `request_too_large`, never counted, never escalated.
 * All of that worked in production and none of it reached the person watching
 * the pane. Measured live on 2026-08-18, across a whole pane scrollback with
 * three overflows in it:
 *
 *     occurrences of "did not fit the model" : 0
 *     occurrences of "will not be retried"   : 0
 *     occurrences of the raw provider dump   : 36
 *
 * Two screens of raw OpenRouter JSON — the same sentence repeated about fifteen
 * times inside its nested `previous_errors` array, then cut with
 * `[truncated 1289 chars]` — which is exactly what the card was written to
 * replace.
 *
 * # Why the obvious test would have passed over it
 *
 * The content was never wrong. `pi.sendMessage` was called, with both sentences
 * and both numbers in it, on every overflow. A test that asserts `sendMessage`
 * received the right string is green against the defect. What was wrong was
 * `{ deliverAs: "nextTurn" }`, which parks the card in Pi's
 * `_pendingNextTurnMessages` until the NEXT prompt is submitted — and a person
 * in this state overflows every next turn, so the card was queued behind a turn
 * that could never run. Guarded by a one-shot flag, so there was exactly one
 * attempt and it was spent on a delivery that could not land.
 *
 * So this file asserts the OPERATOR-VISIBLE STRING: it takes the entry the
 * install actually appended, runs it through the renderer the install actually
 * registered, flattens the result to the lines a terminal would print, and
 * counts words in it. Nothing here can be satisfied by a well-formed call to a
 * delivery that goes nowhere — with `deliverAs: "nextTurn"` restored there is
 * no appended entry to render at all, and every test below fails at the first
 * read rather than at an assertion.
 *
 * The renderer matters as much as the delivery. It used to build
 * `providerConfigurationFailureSpec` unconditionally, ignoring the payload — so
 * a DELIVERED overflow card would have drawn "Provider not configured", which
 * is legible and wrong, and worse than the raw dump. That is why the assertions
 * below are about rendered text and not about the appended data.
 */
import {
  CONTEXT_OVERFLOW,
  installedPane,
  stopInstalledPanes,
  TRANSIENT
} from '@test/support/InstalledPane'
import { isNullish } from '@test/support/Nullish'
import type { PaneEntry } from '@test/types/InstalledPane'
import { afterEach, describe, expect, test } from 'vitest'

const PANE_FAILURE_TYPE = 'organization-pane-failure'

/** A second overflow against a DIFFERENT window — what a person moved to
 *  another model produces. The card names a limit, so a card naming the old
 *  limit must never suppress it. */
const NARROWER_OVERFLOW =
  '400: {"message":"This endpoint\'s maximum context length is 131072 tokens. ' +
  'However, you requested about 140000 tokens (18355 of text input, 10003 of ' +
  'tool input, 111642 in the output)."}'

/** The measurement that discriminated: the explanation, versus the dump it was
 *  written to replace. */
function occurrences(haystack: string, needle: string): number {
  return haystack.split(needle).length - 1
}

function failureCards(entries: readonly PaneEntry[]): readonly PaneEntry[] {
  return entries.filter((entry) => entry.customType === PANE_FAILURE_TYPE)
}

/** The nth pane-failure card, or a readable throw. A missing card is the defect
 *  itself, so it must never read as an assertion about content. */
function failureCard(entries: readonly PaneEntry[], index: number): PaneEntry {
  const card = failureCards(entries)[index]
  if (isNullish(card)) {
    throw new Error(`no pane-failure card at index ${index}; the pane got none`)
  }
  return card
}

afterEach(stopInstalledPanes)

describe('the context-overflow failure card', () => {
  test('is delivered to the pane on the turn that failed, and reads as the explanation', async () => {
    const pane = await installedPane()

    await pane.endTurn(CONTEXT_OVERFLOW)

    // DELIVERED. Not queued behind a turn that cannot run: the append happens
    // inside the same `agent_end` that saw the failure, and Pi paints it with
    // no turn of any kind. This length is the assertion the old code cannot
    // satisfy — `deliverAs: "nextTurn"` appends nothing.
    const cards = failureCards(pane.entries())
    expect(cards).toHaveLength(1)

    // RENDERED, through the renderer the install registered. A card no renderer
    // can draw is not delivered either; `render` throws rather than pass.
    const shown = pane.render(failureCard(pane.entries(), 0))

    // The two sentences the operator was measured never to have seen.
    expect(occurrences(shown, 'did not fit the model')).toBe(1)
    expect(occurrences(shown, 'will not be retried')).toBe(1)
    // Both numbers, so the reader can tell by how much and stop guessing.
    expect(shown).toContain('262175')
    expect(shown).toContain('262144')
    // And the diagnosis, which is the reason this is not the outage card.
    expect(shown).toContain('The provider is reachable')

    // NOT the wrong card. The renderer used to draw this one for every
    // pane-failure payload, whatever it said.
    expect(shown).not.toContain('Provider not configured')
    expect(shown).not.toContain('no configured credentials')

    // AND NOT THE DUMP. Zero occurrences of the provider's own words, in the
    // direction that was 36 live.
    expect(occurrences(shown, "This endpoint's maximum context length")).toBe(0)
    expect(occurrences(shown, 'previous_errors')).toBe(0)
    expect(occurrences(shown, '400: {')).toBe(0)
  })

  test('is said once across an unbroken run of identical rejections', async () => {
    const pane = await installedPane()

    // The live shape: three overflows in a row, each rejected identically.
    await pane.endTurn(CONTEXT_OVERFLOW)
    await pane.endTurn(CONTEXT_OVERFLOW)
    await pane.endTurn(CONTEXT_OVERFLOW)

    // Once. The anti-churn intent of the original one-shot guard, preserved:
    // a card per failed turn would bury the intercom traffic being read.
    expect(failureCards(pane.entries())).toHaveLength(1)
  })

  test('is said again after a turn completes, because the next overflow is new', async () => {
    const pane = await installedPane()

    await pane.endTurn(CONTEXT_OVERFLOW)
    expect(failureCards(pane.entries())).toHaveLength(1)

    // The person trims their context and works. A completed turn is the proof
    // they left the state the card described.
    await pane.completeTurn()
    await pane.endTurn(CONTEXT_OVERFLOW)

    // So the second overflow is genuinely new information and is said again.
    // The process-lifetime one-shot went permanently silent here.
    expect(failureCards(pane.entries())).toHaveLength(2)
  })

  test('is said again when the window changes, because the card names a limit', async () => {
    const pane = await installedPane()

    await pane.endTurn(CONTEXT_OVERFLOW)
    // No successful turn in between — the person was moved to a model with a
    // smaller window and overflowed it immediately.
    await pane.endTurn(NARROWER_OVERFLOW)

    const cards = failureCards(pane.entries())
    expect(cards).toHaveLength(2)
    // The second card must name the NEW window; a guard keyed on a bare boolean
    // would have left the operator reading a limit that is no longer theirs.
    expect(pane.render(failureCard(pane.entries(), 1))).toContain('131072')
  })

  test('a transient provider error draws no card at all', async () => {
    const pane = await installedPane()

    await pane.endTurn(TRANSIENT)
    await pane.endTurn(TRANSIENT)
    await pane.endTurn(TRANSIENT)

    // The pane-failure card is for the two PERMANENT states. A transient outage
    // is the escalation path's business, and a fix that carded everything would
    // pass every test above and fail here.
    expect(failureCards(pane.entries())).toHaveLength(0)
  })
})
