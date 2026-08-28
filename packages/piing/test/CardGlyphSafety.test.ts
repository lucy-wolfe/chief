/**
 * CARD GLYPHS ARE SAFE BY CODEPOINT PROPERTY, NOT BY LOOKING FINE.
 *
 * An operator reported one card icon rendering as tofu, or as a glyph
 * overdrawing its neighbour, while the tick beside it was perfect. The
 * mechanism is not the font being wrong — it is the CODEPOINT being the wrong
 * kind:
 *
 *  - A text-default codepoint (`Emoji_Presentation=No`) such as U+1F3D7
 *    BUILDING CONSTRUCTION is width-1 to every wcwidth-based terminal — tmux
 *    included, and tmux is always in this stack — while an emoji font wants to
 *    draw two columns. Many font chains carry no text-style glyph for those
 *    codepoints at all: they live almost only in colour-emoji fonts.
 *  - U+FE0F (VS16) asks for emoji presentation. It is zero-width to wcwidth
 *    and honoured inconsistently, so it widens the disagreement rather than
 *    settling it.
 *  - A codepoint newer than the reader's fonts is simply absent.
 *
 * A glyph chosen by rendering it on the author's machine tests the author's
 * font. These rules test the property instead, which is the strongest thing
 * provable without seeing any operator's terminal.
 */
import { readdirSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { CARD_GLYPHS, CARD_TEXT_SYMBOLS } from '@test-assets/card-style'
import { describe, expect, test } from 'vitest'

/**
 * Every codepoint a card may draw, written out hex-by-hex ON PURPOSE.
 *
 * The list is the review. Each entry is `Emoji_Presentation=Yes` and
 * `East_Asian_Width=Wide`, so every terminal allocates the two columns the
 * font draws into; and each is Unicode 11.0 (2018) or older, so it is present
 * in font chains that are years out of date. Adding a glyph means adding a row
 * here and stating those facts, which is the point — the cost of the row is
 * the review it forces.
 */
const SAFE_CODEPOINTS = new Map<number, string>([
  [0x2705, 'WHITE HEAVY CHECK MARK (Emoji 1.0)'],
  [0x23f3, 'HOURGLASS WITH FLOWING SAND (1.0)'],
  [0x23f0, 'ALARM CLOCK (1.0)'],
  [0x23e9, 'BLACK RIGHT-POINTING DOUBLE TRIANGLE (1.0)'],
  [0x2757, 'HEAVY EXCLAMATION MARK SYMBOL (1.0)'],
  [0x26a1, 'HIGH VOLTAGE SIGN (1.0)'],
  [0x1f91d, 'HANDSHAKE (Emoji 3.0)'],
  [0x1f9fe, 'RECEIPT (11.0)'],
  [0x1f6d1, 'OCTAGONAL SIGN (Emoji 3.0)'],
  [0x1f4e4, 'OUTBOX TRAY (1.0)'],
  [0x1f4e5, 'INBOX TRAY (1.0)'],
  [0x1f4cb, 'CLIPBOARD (1.0)'],
  [0x1f4ec, 'OPEN MAILBOX WITH RAISED FLAG (1.0)'],
  [0x1f4ac, 'SPEECH BALLOON (1.0)'],
  [0x1f4ca, 'BAR CHART (1.0)'],
  [0x1f6a8, 'POLICE CARS REVOLVING LIGHT (1.0)'],
  [0x1f3af, 'DIRECT HIT (1.0)'],
  [0x1f464, 'BUST IN SILHOUETTE (1.0)'],
  [0x1f9e0, 'BRAIN (Emoji 5.0)'],
  [0x1f9ed, 'COMPASS (11.0)'],
  [0x1f4c5, 'CALENDAR (1.0)'],
  [0x1f512, 'LOCK (1.0)'],
  [0x1f3e2, 'OFFICE BUILDING (1.0)'],
  [0x1f680, 'ROCKET (1.0)'],
  [0x1f4a4, 'SLEEPING SYMBOL (1.0)'],
  [0x1f53b, 'DOWN-POINTING RED TRIANGLE (1.0)'],
  [0x1f9f9, 'BROOM (11.0)'],
  [0x1f44b, 'WAVING HAND SIGN (1.0)'],
  [0x1f6aa, 'DOOR (1.0)'],
  [0x1f4ba, 'SEAT (1.0)'],
  [0x1f514, 'BELL (1.0)'],
  [0x1f515, 'BELL WITH CANCELLATION STROKE (1.0)'],
  [0x1f451, 'CROWN (1.0)'],
  [0x1f69a, 'DELIVERY TRUCK (1.0)'],
  [0x1f333, 'DECIDUOUS TREE (1.0)'],
  [0x1f331, 'SEEDLING (1.0)'],
  [0x1f343, 'LEAF FLUTTERING IN WIND (1.0)'],
  // Emoji_Presentation=Yes and East_Asian_Width=Wide — a two-column emoji, so
  // it belongs HERE and not in the text-symbol list, whose contract is the
  // opposite. It sat there until review caught it: a row contradicting the
  // rule its own table teaches is worse than a missing row, because the next
  // person adding a wide glyph to the text list would cite it as precedent.
  [0x274c, 'CROSS MARK (Emoji 1.0)']
])

/**
 * Bare drawing characters. Width-1 by design and broadly present in monospace
 * fonts; the emoji rule is the WRONG rule for them, and a VS16 on any of these
 * is what would break them.
 */
const SAFE_TEXT_SYMBOLS = new Map<number, string>([
  [0x2192, 'RIGHTWARDS ARROW'],
  [0x21b3, 'DOWNWARDS ARROW WITH TIP RIGHTWARDS'],
  [0x2260, 'NOT EQUAL TO'],
  [0x2264, 'LESS-THAN OR EQUAL TO'],
  [0x27f3, 'CLOCKWISE GAPPED CIRCLE ARROW'],
  [0x2699, 'GEAR (bare: no VS16, drawn as a text symbol)'],
  [0x2839, 'BRAILLE PATTERN DOTS-1456 (spinner frame)']
])

const VS16 = 0xfe0f
/**
 * ZERO WIDTH JOINER. Never sanctioned, and scanned for explicitly.
 *
 * A ZWJ composes two individually-safe codepoints into ONE glyph whose width
 * and font coverage are properties of the composition, not of its parts — and
 * because each part is sanctioned and the joiner itself is invisible, a sweep
 * that did not look for it would pass the composed sequence in silence. That
 * is the one way an unsafe glyph could still reach a card.
 */
const ZWJ = 0x200d
const extensionsDir = fileURLToPath(new URL('../extensions', import.meta.url))

/** Codepoints in the ranges a card glyph could plausibly come from. */
function interestingCodepoints(text: string): number[] {
  return [...text]
    .map((character) => character.codePointAt(0) ?? 0)
    .filter(
      (codepoint) =>
        (codepoint >= 0x2190 && codepoint <= 0x2bff) ||
        (codepoint >= 0x1f000 && codepoint <= 0x1faff) ||
        codepoint === VS16 ||
        codepoint === ZWJ
    )
}

/**
 * Strip comments before sweeping.
 *
 * This has to be real rather than approximate: two legitimate comments in the
 * intercom quote a retired card's glyph while explaining why it is retired,
 * and a sweep that tripped on those would be teaching people to delete their
 * own history to keep a test quiet.
 *
 * Only FULL-LINE `//` comments are stripped, so a trailing `code // 🪞` would
 * still be refused. That direction is deliberate — it refuses more than it
 * must, never less — but it means the first such false positive is a comment
 * to move, not a bug to hunt.
 */
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '')
}

describe('every glyph in the table is a safe single codepoint', () => {
  test('no VS16, no ZWJ, no modifiers — one codepoint per entry', () => {
    for (const [name, glyph] of Object.entries(CARD_GLYPHS)) {
      const codepoints = [...glyph].map((character) => character.codePointAt(0) ?? 0)
      expect(codepoints, `${name} must be exactly one codepoint`).toHaveLength(1)
      expect(codepoints[0], `${name} must not be a VS16 sequence`).not.toBe(VS16)
    }
  })

  test('every entry is in the reviewed allowlist', () => {
    for (const [name, glyph] of Object.entries(CARD_GLYPHS)) {
      const codepoint = glyph.codePointAt(0) ?? 0
      expect(
        SAFE_CODEPOINTS.has(codepoint),
        `${name} (${glyph}, U+${codepoint.toString(16).toUpperCase()}) is not in ` +
          'SAFE_CODEPOINTS. ' +
          'Add a row stating its Emoji_Presentation, East_Asian_Width and Unicode version, ' +
          'or choose a glyph that already has one.'
      ).toBe(true)
    }
  })

  test('text symbols are bare and single-codepoint', () => {
    for (const [name, glyph] of Object.entries(CARD_TEXT_SYMBOLS)) {
      const codepoints = [...glyph].map((character) => character.codePointAt(0) ?? 0)
      expect(codepoints, `${name} must be one bare codepoint`).toHaveLength(1)
      expect(
        SAFE_TEXT_SYMBOLS.has(codepoints[0] ?? 0),
        `${name} must be an allowed text symbol`
      ).toBe(true)
    }
  })
})

/**
 * NOTE ON SCOPE: this sweep enforces membership of the sanctioned SET, not
 * provenance from the table. An inline literal that happens to be a sanctioned
 * codepoint passes. That is deliberate for now — the harm class this exists to
 * stop is an UNSAFE glyph reaching a card, and that is caught completely.
 * Requiring every glyph to be imported from `CARD_GLYPHS` is a style rule on
 * top, and a separate change.
 */
describe('no extension draws a glyph from outside the table', () => {
  test('every card glyph in every extension is one the table sanctions', () => {
    const offenders: string[] = []
    const sanctioned = new Set<number>([...SAFE_CODEPOINTS.keys(), ...SAFE_TEXT_SYMBOLS.keys()])

    for (const file of readdirSync(extensionsDir).filter((name) => name.endsWith('.ts'))) {
      const lines = stripComments(readFileSync(`${extensionsDir}/${file}`, 'utf8')).split('\n')
      lines.forEach((line, index) => {
        for (const codepoint of interestingCodepoints(line)) {
          if (sanctioned.has(codepoint)) continue
          offenders.push(
            `${file}:${index + 1} U+${codepoint.toString(16).toUpperCase().padStart(4, '0')}` +
              (codepoint === VS16 ? ' (VS16 — request emoji presentation; never safe)' : '') +
              (codepoint === ZWJ
                ? ' (ZWJ — composes a glyph whose width is not its parts’; never safe)'
                : '')
          )
        }
      })
    }

    expect(
      offenders,
      'These characters are drawn by an extension but are not sanctioned. A card icon must ' +
        'come from CARD_GLYPHS or CARD_TEXT_SYMBOLS in card-style.ts, so it is reviewed for ' +
        'terminal width agreement and font coverage rather than for rendering on the author’s ' +
        `machine:\n  ${offenders.join('\n  ')}`
    ).toEqual([])
  })
})

describe('the sweep can fail', () => {
  // Both fixtures are written with explicit escapes rather than literal
  // characters. That is not fussiness: the first draft of this file was
  // written through a shell heredoc which mangled the literals, leaving one
  // fixture containing NO glyph at all — so the test asserted emptiness over
  // emptiness and passed while proving nothing. Escapes cannot be mangled by
  // an encoding, and they say which codepoint is meant.
  const VS16_SEQUENCE = '\u{1F3D7}\u{FE0F}'
  const RETIRED_GLYPH = '\u{1FA9E}'

  test('a VS16 sequence is refused', () => {
    // A sweep that cannot fail proves nothing. This asserts the check rejects
    // the exact thing it exists to reject — the discriminating-fixture rule.
    const found = interestingCodepoints(stripComments(`const icon = "${VS16_SEQUENCE}";`))
    expect(found).toContain(VS16)
    expect(found).toContain(0x1f3d7)
  })

  test('a ZWJ sequence is refused even though both halves are sanctioned', () => {
    // The dangerous case: every component is on the allowlist, so only the
    // joiner distinguishes a safe pair of glyphs from one composed glyph whose
    // width and coverage are nobody's guarantee.
    const composed = `const icon = "\u{1F468}\u{200D}\u{1F4BB}";`
    expect(interestingCodepoints(stripComments(composed))).toContain(ZWJ)
  })

  test('a comment quoting a retired glyph is NOT refused', () => {
    // Two real comments explain why a card was retired and quote its glyph to
    // do so. Tripping on those would teach people to delete the explanation to
    // keep a test quiet. The fixture carries a REAL glyph, so the comment
    // stripping is what makes it pass — not the absence of anything to find.
    const commented = `// the retired "${RETIRED_GLYPH} Reflection" card\nconst safe = 1;`
    expect(interestingCodepoints(commented)).toContain(0x1fa9e)
    expect(interestingCodepoints(stripComments(commented))).toEqual([])
  })
})
