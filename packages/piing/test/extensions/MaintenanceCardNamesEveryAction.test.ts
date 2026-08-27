/**
 * NO SESSION-MAINTENANCE CARD MAY RENDER THE WORD `undefined`.
 *
 * The operator was sent this, from a live company:
 *
 *     🧠undefined · @gus
 *     undefined
 *     (Ctrl+O to expand)
 *     Pi cannot use openrouter/openrouter/deepseek/deepseek-v4-flash …
 *
 * The card's two lookup tables were keyed by `fresh_session` and `compact`
 * only, so a `set_model` request found no row in either — and the fallback
 * lookup carried a `!`, which asserted to the compiler that a missing row
 * could not be missing. Adding a third action without adding its rows
 * therefore type-checked cleanly and failed on the glass, in the one place
 * whose entire job is telling the operator what happened.
 *
 * The comment above that code said "#319: this card renders EVERY
 * session-maintenance action — fixing the pre-existing binary
 * fresh_session-vs-everything-else assumption". #319 fixed the ICON and left
 * both tables binary. A comment is not a test.
 *
 * So this drives the CROSS PRODUCT — every action × every phase — rather than
 * the actions that happen to exist today. A fourth action added without its
 * rows fails here, which is the only version of this test that would have
 * caught the third one.
 */
import { isNullish } from '@test/support/Nullish'
import { captureRegisteredTools } from '@test/support/ToolRegistrationHarness'
import type { CapturedRenderer, ToolRegistrationCapture } from '@test/types/ToolRegistrationHarness'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

/** Closed set, mirrored from `MaintenanceAction` in the Rust store and from
 *  `packages/chiefing/src/types/SessionLifecycle.ts`. */
// ONE ACTION. `fresh_session` and `set_model` are deleted with
// `org_maintain_session`. The no-`undefined` arm below is narrowed rather than
// deleted: it is the regression guard for a MISSING LOOKUP ROW rendering the
// word `undefined` onto an operator's card, and a one-row table can still lose
// its row.
const ACTIONS = ['compact'] as const
/** Closed set, mirrored from `MaintenanceStatus`. */
const PHASES = ['queued', 'running', 'applying', 'completed', 'failed', 'skipped'] as const

const CUSTOM_TYPE = 'organization-session-maintenance'

/** A `CardTheme` that colours nothing, so what comes back is the plain text
 *  the operator reads. `renderCard` needs `fg` and `bold`; `bg` is optional
 *  and omitted so a boxed card stays a flat `Text` node this test can read. */
const THEME = {
  fg: (_token: string, text: string) => text,
  bold: (text: string) => text
}

function request(action: string, phase: string): Record<string, unknown> {
  return {
    id: `session-maintenance:${action}`,
    action,
    personId: 'gus',
    requestedBy: 'ceo',
    reason: `${action} requested by ceo`,
    automatic: false,
    status: phase,
    requestedAt: '2026-08-19T00:00:00.000Z'
  }
}

/** Every string anywhere in a rendered card, however deeply nested. */
function renderedStrings(value: unknown): string[] {
  if (typeof value === 'string') return [value]
  if (Array.isArray(value)) return value.flatMap(renderedStrings)
  if (typeof value === 'object' && !isNullish(value))
    return Object.values(value).flatMap(renderedStrings)
  return []
}

describe('the session-maintenance card', () => {
  let capture: ToolRegistrationCapture
  let render: CapturedRenderer

  beforeAll(async () => {
    capture = await captureRegisteredTools()
    const found = capture.renderers.get(CUSTOM_TYPE)
    if (!found) throw new Error(`${CUSTOM_TYPE} registered no renderer`)
    render = found
  })

  afterAll(async () => {
    await capture.stop()
  })

  it('registers a renderer at all, so the assertions below are not vacuous', () => {
    expect(capture.renderers.has(CUSTOM_TYPE)).toBe(true)
  })

  it('renders no `undefined` for any action in any phase', () => {
    const offenders: string[] = []
    for (const action of ACTIONS) {
      for (const phase of PHASES) {
        const card = render({ details: { request: request(action, phase), phase } }, {}, THEME)
        const strings = renderedStrings(card)
        // `undefined` reaching a card is always a missing lookup row, never a
        // legitimate word — no copy in this product contains it.
        if (strings.some((text) => text.includes('undefined'))) offenders.push(`${phase}:${action}`)
        // An empty card is the same defect wearing a different coat.
        if (!strings.length) offenders.push(`${phase}:${action} rendered nothing`)
      }
    }
    expect(offenders).toEqual([])
  })

  // TOMBSTONES: `gives a model change its own title and names the model`,
  // `shows provider and model as separate fields, never joined by a slash`,
  // `keeps the surviving actions saying what they always said` and `does not
  // tell a skipped model change that its session is already small`.
  //
  // All four were about `set_model` and `fresh_session` copy. Both actions are
  // deleted; a card cannot render a title for an action that cannot exist.
  it('still says what a compaction says, in the phase an operator reads most', () => {
    const compacted = renderedStrings(
      render(
        { details: { request: request('compact', 'completed'), phase: 'completed' } },
        {},
        THEME
      )
    ).join('\n')
    expect(compacted).toContain('Context compacted')
  })
})
