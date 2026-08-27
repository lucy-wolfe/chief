import { Theme, type ThemeColor } from '@earendil-works/pi-coding-agent'
import type { Box, Text } from '@earendil-works/pi-tui'
import {
  CARD_EXPAND_HINT_TEXT,
  CARD_KIND_INFO,
  CARD_KINDS,
  CARD_STATE_EMOJI,
  CARD_STATE_TOKEN,
  cardAccentBoldTitle,
  cardBody,
  cardBoldTitle,
  cardCallLine,
  cardDetail,
  cardHint,
  type CardKind,
  type CardSpec,
  type CardState,
  cardStateIcon,
  cardTitle,
  domainIcon,
  finalizeHumanRef,
  humanRef,
  paneFailureSpec,
  previewText,
  providerRequestTooLargeError,
  providerRequestTooLargeSpec,
  readableIdentityForeground,
  renderCard,
  renderCardGroup,
  renderCardText,
  scrubHumanRef,
  toolFailureText
} from '@test-assets/card-style'
import { describe, expect, test } from 'vitest'

interface TrackingTheme {
  calls: Array<{ token: string; text: string }>
  bgCalls: Array<{ token: string; text: string }>
  bold(text: string): string
  fg(token: string, text: string): string
  bg(token: string, text: string): string
}

/** Mirrors the tone-tracking `plainTheme()` fixture used across
 * `tests/org-intercom.test.ts` / `tests/team-ui.test.ts`, except it also
 * records which token each call used so assertions can check color routing,
 * not just the rendered text. */
function trackingTheme(): TrackingTheme {
  const calls: Array<{ token: string; text: string }> = []
  const bgCalls: Array<{ token: string; text: string }> = []
  return {
    calls,
    bgCalls,
    bold: (text: string) => text,
    fg: (token: string, text: string) => {
      calls.push({ token, text })
      return text
    },
    bg: (token: string, text: string) => {
      bgCalls.push({ token, text })
      return text
    }
  }
}

function renderedCard(node: Box | Text, width: number): string {
  return node.render(width).join('\n')
}

describe('card-style: state-emoji vocabulary', () => {
  test('exactly one emoji and token per fixed state class', () => {
    const states: CardState[] = ['success', 'wait', 'handoff', 'input-repair', 'failure', 'circuit']
    for (const state of states) {
      expect(CARD_STATE_EMOJI[state]).toBeTruthy()
      expect(CARD_STATE_TOKEN[state]).toBeTruthy()
    }
    // Locked exactly to the #354 house-style spec -- a regression here is a
    // silent vocabulary drift, not a stylistic nuance.
    expect(CARD_STATE_EMOJI).toEqual({
      success: '✅',
      wait: '⏳',
      handoff: '🤝',
      'input-repair': '🧾',
      failure: '⚠️',
      circuit: '🛑'
    })
    expect(CARD_STATE_TOKEN.success).toBe('success')
    expect(CARD_STATE_TOKEN.wait).toBe('warning')
    expect(CARD_STATE_TOKEN.failure).toBe('error')
  })

  test("cardStateIcon resolves the fixed pair; domainIcon wraps a tool's own emoji", () => {
    expect(cardStateIcon('success')).toEqual({ emoji: '✅', token: 'success' })
    expect(domainIcon('🧭')).toEqual({ emoji: '🧭', token: 'dim' })
    expect(domainIcon('📋', 'success')).toEqual({ emoji: '📋', token: 'success' })
  })
})

describe('card-style: cardTitle', () => {
  test("formats <emoji> <title> in the state's token, · target in dim", () => {
    const theme = trackingTheme()
    const line = cardTitle(theme, 'success', 'Task created', 'Fix login bug')
    expect(line).toBe('✅ Task created · Fix login bug')
    expect(theme.calls).toEqual([
      { token: 'success', text: '✅ Task created' },
      { token: 'dim', text: '· Fix login bug' }
    ])
  })

  test('omits the target segment entirely when no target is given', () => {
    const theme = trackingTheme()
    expect(cardTitle(theme, 'failure', 'Assignment failed')).toBe('⚠️ Assignment failed')
    expect(theme.calls).toEqual([{ token: 'error', text: '⚠️ Assignment failed' }])
  })

  test('accepts a domain icon carve-out for a read-only success (📋 Roster updated)', () => {
    const theme = trackingTheme()
    expect(cardTitle(theme, domainIcon('📋', 'success'), 'Roster updated')).toBe(
      '📋 Roster updated'
    )
  })

  test('covers every non-in-progress state class end to end', () => {
    const theme = trackingTheme()
    expect(cardTitle(theme, 'wait', 'Company updating')).toBe('⏳ Company updating')
    expect(cardTitle(theme, 'handoff', 'Waiting for handoff')).toBe('🤝 Waiting for handoff')
    expect(cardTitle(theme, 'input-repair', 'Check completion fields')).toBe(
      '🧾 Check completion fields'
    )
    expect(cardTitle(theme, 'circuit', 'Message retry loop stopped')).toBe(
      '🛑 Message retry loop stopped'
    )
  })
})

describe('card-style: cardBody (public body-text emitter)', () => {
  test('colors text in a token; per-line wrap re-colors across newlines', () => {
    const theme = trackingTheme()
    expect(cardBody(theme, 'customMessageText', 'hello')).toBe('hello')
    expect(theme.calls).toEqual([{ token: 'customMessageText', text: 'hello' }])
    const t2 = trackingTheme()
    cardBody(t2, 'dim', 'a\nb', 'per-line')
    expect(t2.calls).toEqual([
      { token: 'dim', text: 'a' },
      { token: 'dim', text: 'b' }
    ])
  })
})

describe('card-style: titleTags (inline title decorations)', () => {
  test('appends each tag after the title with its own token and separator', () => {
    const theme = trackingTheme()
    const text = renderCardText(theme, {
      kind: 'tool-failure',
      icon: 'failure',
      title: 'org_send failed',
      target: '@a',
      titleTags: [
        { text: '(system fault)', token: 'dim' },
        { text: '· timed out…', token: 'dim' },
        { text: '(Ctrl+O to expand)', token: 'dim', sep: '  ' }
      ],
      body: { kind: 'none' },
      boxed: false
    })
    // Default sep is a single space; the hint tag uses a double space.
    expect(text).toBe('⚠️ org_send failed · @a (system fault) · timed out…  (Ctrl+O to expand)')
    expect(theme.calls).toEqual([
      { token: 'error', text: '⚠️ org_send failed' },
      { token: 'dim', text: '· @a' },
      { token: 'dim', text: '(system fault)' },
      { token: 'dim', text: '· timed out…' },
      { token: 'dim', text: '(Ctrl+O to expand)' }
    ])
  })
})

describe('card-style: cardBoldTitle (boxed-message header)', () => {
  test('renders <emoji> <bold title> with the emoji unstyled and a dim · target', () => {
    const theme = trackingTheme()
    const line = cardBoldTitle(theme, '🧠', 'Session notice', '@ceo')
    expect(line).toBe('🧠 Session notice · @ceo')
    // The emoji is NOT wrapped in any fg token (unlike cardTitle); only the
    // dim target segment routes through fg.
    expect(theme.calls).toEqual([{ token: 'dim', text: '· @ceo' }])
  })

  test('omits the target and tolerates an empty emoji (bold-only header)', () => {
    const theme = trackingTheme()
    expect(cardBoldTitle(theme, '⚡', 'Work resumed')).toBe('⚡ Work resumed')
    expect(cardBoldTitle(theme, '', 'Work resumed')).toBe('Work resumed')
  })

  test("renderCardText honors titleStyle:'bold'", () => {
    const theme = trackingTheme()
    const text = renderCardText(theme, {
      kind: 'system-notice',
      icon: domainIcon('🧠'),
      titleStyle: 'bold',
      title: 'Session notice',
      body: { kind: 'none' },
      boxed: true
    })
    expect(text).toBe('🧠 Session notice')
    // No state-token fg on the title (bold, not colored).
    expect(theme.calls).toEqual([])
  })
})

describe('card-style: cardAccentBoldTitle + accent-bold header / sender', () => {
  test('wraps <emoji> <title> in one bold+accent span, emoji inside', () => {
    const theme = trackingTheme()
    expect(cardAccentBoldTitle(theme, '📬', 'Intercom')).toBe('📬 Intercom')
    // Exactly one fg call, token = accent, over the whole "<emoji> <title>".
    expect(theme.calls).toEqual([{ token: 'accent', text: '📬 Intercom' }])
  })

  test('honors a custom accent token and an empty emoji (colored sender name)', () => {
    const theme = trackingTheme()
    expect(cardAccentBoldTitle(theme, '', 'Ari', 'customMessageLabel')).toBe('Ari')
    expect(theme.calls).toEqual([{ token: 'customMessageLabel', text: 'Ari' }])
  })

  test('renderCardText accent-bold + sender reproduces a sender-labelled header byte-for-byte', () => {
    // The accent-bold + sender idiom, as the boxed intercom message card
    // renders it:
    //   fg(accent, bold(label)) ` ` fg(dim,"from") ` ` fg(accent, bold(name))
    const theme = trackingTheme()
    const text = renderCardText(theme, {
      kind: 'intercom-message',
      icon: domainIcon('📬'),
      titleStyle: 'accent-bold',
      title: 'Intercom',
      sender: { from: 'from', name: 'Ari' },
      body: { kind: 'none' },
      boxed: true
    })
    expect(text).toBe('📬 Intercom from Ari')
    expect(theme.calls).toEqual([
      { token: 'accent', text: '📬 Intercom' },
      { token: 'dim', text: 'from' },
      { token: 'accent', text: 'Ari' }
    ])
  })
})

describe('card-style: cardCallLine (in-progress)', () => {
  test('renders the whole line dim, including the target', () => {
    const theme = trackingTheme()
    const line = cardCallLine(theme, {
      emoji: '🧭',
      title: 'Assigning work',
      target: '3 direct reports'
    })
    expect(line).toBe('🧭 Assigning work · 3 direct reports')
    expect(theme.calls).toEqual([
      { token: 'dim', text: '🧭 Assigning work' },
      { token: 'dim', text: '· 3 direct reports' }
    ])
  })

  test('omits the target segment when absent', () => {
    const theme = trackingTheme()
    expect(cardCallLine(theme, { emoji: '📤', title: 'Sending message' })).toBe(
      '📤 Sending message'
    )
  })
})

describe('card-style: cardDetail + expand hint', () => {
  test('cardDetail dims arbitrary detail text', () => {
    const theme = trackingTheme()
    expect(cardDetail(theme, 'No message was queued.')).toBe('No message was queued.')
    expect(theme.calls).toEqual([{ token: 'dim', text: 'No message was queued.' }])
  })

  test('cardHint renders the single canonical spelling, dim', () => {
    const theme = trackingTheme()
    expect(cardHint(theme)).toBe(CARD_EXPAND_HINT_TEXT)
    expect(CARD_EXPAND_HINT_TEXT).toBe('(Ctrl+O to expand)')
    expect(theme.calls).toEqual([{ token: 'dim', text: '(Ctrl+O to expand)' }])
  })
})

describe('card-style: humanRef / scrubHumanRef / finalizeHumanRef', () => {
  test('replaces known id-prefix families with a neutral phrase', () => {
    expect(humanRef('Created goal-abc123def456 for the sprint')).toBe(
      'Created the affected work for the sprint'
    )
    expect(humanRef('See task-9f8e7d6c5b4a for details')).toBe('See the affected work for details')
    expect(humanRef('assignment-554433221100 replied')).toBe('the affected work replied')
  })

  test('only scrubs the requested prefix families when overridden', () => {
    expect(humanRef('task-9f8e7d6c5b4a stays raw here', { prefixes: ['goal'] })).toBe(
      'task-9f8e7d6c5b4a stays raw here'
    )
  })

  test('strips control characters and bare long hex hashes', () => {
    const esc = String.fromCharCode(27)
    const nul = String.fromCharCode(0)
    const longHex = 'a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4' // 32 hex chars, over the 20-char threshold
    const withControlChars = `${esc}[31mMessage from @launcher ${longHex}${nul}`
    const scrubbed = humanRef(withControlChars)
    expect(scrubbed).toBeTruthy()
    if (!scrubbed) throw new Error('control-character fixture must remain legible')
    expect(scrubbed).not.toContain(esc)
    expect(scrubbed).not.toContain(nul)
    expect(scrubbed).not.toContain(longHex)
    expect(scrubbed).toContain('Message from @launcher')
  })

  test('returns undefined for non-string input and for input with no legible characters', () => {
    expect(humanRef(undefined)).toBeUndefined()
    expect(humanRef(42)).toBeUndefined()
    expect(humanRef(' '.repeat(8))).toBeUndefined()
  })

  test('truncates to the maximum with an ellipsis', () => {
    // "x" (not a hex digit) keeps this out of the bare-long-hex-hash strip below.
    const long = 'x'.repeat(200)
    expect(humanRef(long, { maximum: 10 })).toBe(`${'x'.repeat(10)}…`)
    expect(humanRef('short', { maximum: 10 })).toBe('short')
  })

  test('custom replacement text is honored', () => {
    expect(humanRef('goal-abc12345 needs review', { replacement: 'a goal' })).toBe(
      'a goal needs review'
    )
  })

  test('scrubHumanRef + finalizeHumanRef compose to the same result as humanRef', () => {
    const value = 'Reassigned task-1122334455 to @signal-analyst'
    expect(finalizeHumanRef(scrubHumanRef(value))).toBe(humanRef(value))
  })
})

// ===========================================================================
// Unified renderer: CardKind registry + exhaustiveness (#150 foundation)
// ===========================================================================
describe('card-style: CardKind registry is total by construction', () => {
  test('CARD_KIND_INFO has exactly one entry per declared CardKind', () => {
    // Every enumerated kind resolves to registry info, and there are no stray
    // registry entries. `CARD_KIND_INFO: Record<CardKind, CardKindInfo>` makes
    // the compiler reject a kind added to the union without an entry here — this
    // asserts the runtime shape agrees with that guarantee.
    expect(CARD_KINDS.length).toBe(Object.keys(CARD_KIND_INFO).length)
    for (const kind of CARD_KINDS) {
      const info = CARD_KIND_INFO[kind]
      expect(info, `no CARD_KIND_INFO entry for '${kind}'`).toBeDefined()
      expect(typeof info.boxed).toBe('boolean')
      expect(info.label).toBeTruthy()
    }
    expect(new Set(CARD_KINDS).size).toBe(CARD_KINDS.length)
  })

  test('a total Record<CardKind, X> is a compile-time obligation — a dispatch that omits a kind fails typecheck', () => {
    // This is the exhaustiveness contract in code: any consumer that builds a
    // `Record<CardKind, T>` (e.g. a presenter map) must cover every kind or the
    // type does not check. Adding a member to `CardKind` without extending this
    // object is a `bun run typecheck` error — the mechanical replacement for the
    // silent unknown-kind fall-through this system deletes.
    const covered: Record<CardKind, true> = {
      'tool-call': true,
      'tool-success': true,
      'tool-failure': true,
      'intercom-message': true,
      'intercom-assignment': true,
      'system-notice': true,
      'session-maintenance': true,
      'work-resumed': true,
      'first-boot': true,
      'pane-failure': true,
      'live-schedules': true
    }
    expect(Object.keys(covered).sort()).toEqual([...CARD_KINDS].sort())
  })
})

describe('card-style: CardSpec makes an absent body a compile error (#103 lock)', () => {
  test('a spec without `body` does not typecheck', () => {
    // @ts-expect-error — `body` is required; omitting it is a compile error, not
    // an empty card. `bun run typecheck` fails if this line ever compiles.
    const missingBody: CardSpec = {
      kind: 'system-notice',
      icon: 'success',
      title: 'x',
      boxed: true
    }
    void missingBody
    // A well-formed spec (body present) of course compiles.
    const ok: CardSpec = {
      kind: 'system-notice',
      icon: 'success',
      title: 'x',
      boxed: true,
      body: { kind: 'none' }
    }
    expect(ok.body.kind).toBe('none')
  })
})

// ===========================================================================
// Unified renderer: renderCardText / renderCard (#150 foundation)
// ===========================================================================
describe('card-style: previewText', () => {
  test('normalizes whitespace to single spaces and folds newlines to one line', () => {
    expect(previewText('a\n\n  b   c', 40)).toEqual({ text: 'a b c', truncated: false })
  })
  test('slices to the maximum and reports truncation', () => {
    expect(previewText('x'.repeat(50), 10)).toEqual({ text: 'x'.repeat(10), truncated: true })
  })
})

describe('card-style: renderCardText assembly', () => {
  test('title line + dim detail lines, no body', () => {
    const theme = trackingTheme()
    const spec: CardSpec = {
      kind: 'tool-success',
      icon: 'success',
      title: 'Task created',
      target: 'Fix login',
      detail: ['No message was queued.', 'Retry once next turn.'],
      body: { kind: 'none' },
      boxed: false
    }
    expect(renderCardText(theme, spec)).toBe(
      '✅ Task created · Fix login\nNo message was queued.\nRetry once next turn.'
    )
    expect(theme.calls).toEqual([
      { token: 'success', text: '✅ Task created' },
      { token: 'dim', text: '· Fix login' },
      { token: 'dim', text: 'No message was queued.' },
      { token: 'dim', text: 'Retry once next turn.' }
    ])
  })

  test('in-progress spec renders the whole title line dim (renderCall variant)', () => {
    const theme = trackingTheme()
    const spec: CardSpec = {
      kind: 'tool-call',
      icon: domainIcon('🧭'),
      title: 'Assigning goal',
      target: '3 direct reports',
      inProgress: true,
      body: { kind: 'none' },
      boxed: false
    }
    expect(renderCardText(theme, spec)).toBe('🧭 Assigning goal · 3 direct reports')
    expect(theme.calls.every((c) => c.token === 'dim')).toBe(true)
  })

  test('prose body: collapsed shows a bounded preview + hint, expanded shows the full text', () => {
    const long = 'This is a long body '.repeat(20).trim()
    const collapsed = renderCardText(trackingTheme(), {
      kind: 'intercom-message',
      icon: 'success',
      title: 'Message',
      body: { kind: 'prose', text: long },
      boxed: true,
      bodyToken: 'customMessageText'
    })
    expect(collapsed).toContain('…')
    expect(collapsed).toContain(CARD_EXPAND_HINT_TEXT)

    const expanded = renderCardText(
      trackingTheme(),
      {
        kind: 'intercom-message',
        icon: 'success',
        title: 'Message',
        body: { kind: 'prose', text: long },
        boxed: true,
        bodyToken: 'customMessageText'
      },
      { expanded: true }
    )
    // Expanded shows the full text set off by a blank line, and no hint.
    expect(expanded).toContain(`\n\n${long}`)
    expect(expanded).not.toContain(CARD_EXPAND_HINT_TEXT)
  })

  test('a collapsed prose preview may be dim while its expanded diagnostic stays raw', () => {
    const long = 'diagnostic '.repeat(20).trim()
    const collapsedTheme = trackingTheme()
    renderCardText(collapsedTheme, {
      kind: 'tool-success',
      icon: 'success',
      title: '2 lessons recalled',
      body: { kind: 'prose', text: long, previewChars: 20 },
      bodyToken: undefined,
      collapsedBodyToken: 'dim',
      boxed: false
    })
    expect(collapsedTheme.calls.some((call) => call.token === 'dim')).toBe(true)
    const expandedTheme = trackingTheme()
    renderCardText(
      expandedTheme,
      {
        kind: 'tool-success',
        icon: 'success',
        title: '2 lessons recalled',
        body: { kind: 'prose', text: long, previewChars: 20 },
        bodyToken: undefined,
        collapsedBodyToken: 'dim',
        boxed: false
      },
      { expanded: true }
    )
    expect(expandedTheme.calls.some((call) => call.text === long)).toBe(false)
  })

  test("prose body with collapse:'hidden' reveals only the expand hint when collapsed", () => {
    const spec: CardSpec = {
      kind: 'system-notice',
      icon: 'success',
      title: 'Session notice',
      detail: 'Remember one useful lesson.',
      body: { kind: 'prose', text: 'full instructions', collapse: 'hidden' },
      boxed: true,
      bodyToken: 'customMessageText'
    }
    const collapsed = renderCardText(trackingTheme(), spec)
    expect(collapsed).toBe('✅ Session notice\nRemember one useful lesson.\n(Ctrl+O to expand)')
    expect(collapsed).not.toContain('full instructions')

    const expanded = renderCardText(trackingTheme(), spec, { expanded: true })
    expect(expanded).toContain('\n\nfull instructions')
    expect(expanded).not.toContain(CARD_EXPAND_HINT_TEXT)
  })

  test("prose wrap:'per-line' colors every line of a multi-line expanded body (color survives newlines)", () => {
    const theme = trackingTheme()
    const text = renderCardText(
      theme,
      {
        kind: 'session-maintenance',
        icon: domainIcon('🧠'),
        titleStyle: 'bold',
        title: 'Compacting context',
        body: {
          kind: 'prose',
          text: 'Reason: growing\nRequested by you',
          collapse: 'hidden',
          wrap: 'per-line'
        },
        bodyToken: 'dim',
        boxed: true
      },
      { expanded: true }
    )
    expect(text).toBe('🧠 Compacting context\n\nReason: growing\nRequested by you')
    // Each body line was wrapped in its own dim span (two separate fg calls),
    // not one span spanning the newline.
    expect(theme.calls).toEqual([
      { token: 'dim', text: 'Reason: growing' },
      { token: 'dim', text: 'Requested by you' }
    ])
  })

  test('footer lines render in their own token in both collapsed and expanded states', () => {
    const spec: CardSpec = {
      kind: 'session-maintenance',
      icon: domainIcon('🧠'),
      titleStyle: 'bold',
      title: 'Compaction paused',
      detail: 'Pi is focusing this session.',
      body: { kind: 'prose', text: 'Reason: x', collapse: 'hidden' },
      bodyToken: 'dim',
      boxed: true,
      footer: [{ text: 'boom', token: 'error' }]
    }
    const collapsed = renderCardText(trackingTheme(), spec)
    expect(collapsed).toBe(
      '🧠 Compaction paused\nPi is focusing this session.\n(Ctrl+O to expand)\nboom'
    )
    const expanded = renderCardText(trackingTheme(), spec, { expanded: true })
    expect(expanded.endsWith('\nboom')).toBe(true)
  })

  test('lines body renders each pre-structured line in its own token, with an uncolored raw suffix', () => {
    const theme = trackingTheme()
    const text = renderCardText(theme, {
      kind: 'system-notice',
      icon: domainIcon(''),
      titleStyle: 'accent-bold',
      title: '🎯 Goal review',
      body: {
        kind: 'lines',
        lines: [
          { text: 'An open goal needs review.', token: 'customMessageText' },
          { text: 'Affected person: @a', token: 'dim' },
          { text: '' },
          { text: 'For @a: validate the Q3 set', token: 'customMessageText', raw: '…' },
          { text: 'Next: continue the next step', token: 'dim' },
          { text: 'Blocked', token: 'warning' }
        ]
      },
      boxed: true
    })
    expect(text).toBe(
      [
        '🎯 Goal review',
        'An open goal needs review.',
        'Affected person: @a',
        '',
        'For @a: validate the Q3 set…',
        'Next: continue the next step',
        'Blocked'
      ].join('\n')
    )
    // The trailing "…" is OUTSIDE the customMessageText span (raw), and the
    // blank line carries no token.
    expect(theme.calls).toEqual([
      { token: 'accent', text: '🎯 Goal review' },
      { token: 'customMessageText', text: 'An open goal needs review.' },
      { token: 'dim', text: 'Affected person: @a' },
      { token: 'customMessageText', text: 'For @a: validate the Q3 set' },
      { token: 'dim', text: 'Next: continue the next step' },
      { token: 'warning', text: 'Blocked' }
    ])
  })

  test('list body renders every item in full even when collapsed (the #103 regression lock)', () => {
    const items = [
      'For @a: validate the Q3 set',
      'For @b: review the ledger',
      'For @c: settle the outcome'
    ]
    const spec: CardSpec = {
      kind: 'system-notice',
      icon: 'success',
      title: 'Goal review',
      body: { kind: 'list', items },
      boxed: true,
      bodyToken: 'customMessageText'
    }
    const collapsed = renderCardText(trackingTheme(), spec)
    for (const item of items) expect(collapsed).toContain(item)
    // A list is never truncated to a preview: no ellipsis, no expand hint.
    expect(collapsed).not.toContain('…')
    expect(collapsed).not.toContain(CARD_EXPAND_HINT_TEXT)
  })
})

describe('card-style: renderCard node wrapping', () => {
  test('all four custom card types use the mode-correct identity foreground and background', () => {
    const actualTheme = (identity: string, background: string): Theme => {
      const foregrounds: Record<ThemeColor, string> = {
        accent: identity,
        bashMode: identity,
        border: identity,
        borderAccent: identity,
        borderMuted: identity,
        customMessageLabel: identity,
        success: identity,
        error: identity,
        warning: identity,
        muted: identity,
        dim: identity,
        text: identity,
        thinkingText: identity,
        userMessageText: identity,
        customMessageText: identity,
        toolTitle: identity,
        toolOutput: identity,
        mdHeading: identity,
        mdLink: identity,
        mdLinkUrl: identity,
        mdCode: identity,
        mdCodeBlock: identity,
        mdCodeBlockBorder: identity,
        mdQuote: identity,
        mdQuoteBorder: identity,
        mdHr: identity,
        mdListBullet: identity,
        toolDiffAdded: identity,
        toolDiffRemoved: identity,
        toolDiffContext: identity,
        syntaxComment: identity,
        syntaxKeyword: identity,
        syntaxFunction: identity,
        syntaxVariable: identity,
        syntaxString: identity,
        syntaxNumber: identity,
        syntaxType: identity,
        syntaxOperator: identity,
        syntaxPunctuation: identity,
        thinkingOff: identity,
        thinkingMinimal: identity,
        thinkingLow: identity,
        thinkingMedium: identity,
        thinkingHigh: identity,
        thinkingXhigh: identity,
        thinkingMax: identity
      }
      const backgrounds = {
        selectedBg: background,
        userMessageBg: background,
        customMessageBg: background,
        toolPendingBg: background,
        toolSuccessBg: background,
        toolErrorBg: background
      }
      return new Theme(foregrounds, backgrounds, 'truecolor')
    }
    const raw = '#e24033'
    const light = actualTheme(readableIdentityForeground(raw, 'light'), '#ede7f6')
    const dark = actualTheme(readableIdentityForeground(raw, 'dark'), '#2d2838')
    for (const kind of [
      'intercom-message',
      'session-maintenance',
      'work-resumed',
      'pane-failure'
    ] as const satisfies readonly CardKind[]) {
      const spec: CardSpec = {
        kind,
        icon: 'success',
        title: 'Message ready',
        body: { kind: 'prose', text: 'identity body' },
        boxed: true,
        bodyToken: 'customMessageText'
      }
      const lightFrame = renderedCard(renderCard(light, spec), 80)
      const darkFrame = renderedCard(renderCard(dark, spec), 80)
      expect(lightFrame).toContain(`${light.getFgAnsi('customMessageText')}identity body`)
      expect(darkFrame).toContain(`${dark.getFgAnsi('customMessageText')}identity body`)
      expect(lightFrame).toContain(light.getBgAnsi('customMessageBg'))
      expect(darkFrame).toContain(dark.getBgAnsi('customMessageBg'))
    }

    expect(light.getFgAnsi('customMessageText')).not.toBe(dark.getFgAnsi('customMessageText'))
    expect(light.getBgAnsi('customMessageBg')).not.toBe(dark.getBgAnsi('customMessageBg'))
  })

  test('a non-boxed spec renders a plain Text node with no background box', () => {
    const theme = trackingTheme()
    const node = renderCard(theme, {
      kind: 'tool-success',
      icon: 'success',
      title: 'Done',
      body: { kind: 'none' },
      boxed: false
    })
    const rendered = renderedCard(node, 80)
    expect(rendered).toContain('Done')
    // No customMessageBg box was applied.
    expect(theme.bgCalls).toEqual([])
  })

  test('a boxed spec wraps the content in a customMessageBg box', () => {
    const theme = trackingTheme()
    const node = renderCard(theme, {
      kind: 'system-notice',
      icon: 'success',
      title: 'Session notice',
      body: { kind: 'none' },
      boxed: true
    })
    node.render(80)
    // The box's per-line background function ran against customMessageBg only.
    expect(theme.bgCalls.length).toBeGreaterThan(0)
    expect(new Set(theme.bgCalls.map((c) => c.token))).toEqual(new Set(['customMessageBg']))
  })

  test('a boxed spec degrades to a plain Text when the theme exposes no bg', () => {
    const node = renderCard(
      { bold: (t: string) => t, fg: (_t: string, text: string) => text },
      {
        kind: 'pane-failure',
        icon: 'failure',
        title: 'Provider not configured',
        body: { kind: 'none' },
        boxed: true
      }
    )
    expect(renderedCard(node, 80)).toContain('Provider not configured')
  })

  test('a boxed group gives every declarative card its own expansion boundary', () => {
    const theme = trackingTheme()
    const node = renderCardGroup(theme, [
      {
        kind: 'intercom-message',
        icon: domainIcon('📬'),
        titleStyle: 'accent-bold',
        title: 'Intercom',
        sender: { from: 'from', name: 'Ari' },
        body: { kind: 'prose', text: 'short body' },
        expandedFooter: [{ token: 'dim', text: 'replyTo message-1' }],
        boxed: true
      },
      {
        kind: 'intercom-message',
        icon: domainIcon('⚡'),
        titleStyle: 'accent-bold',
        title: 'Intercom priority',
        sender: { from: 'from', name: 'Bea' },
        body: { kind: 'prose', text: 'another body' },
        expandedFooter: [{ token: 'dim', text: 'replyTo message-2' }],
        boxed: true
      }
    ])
    const collapsed = renderedCard(node, 160)
    expect(collapsed).toContain('Intercom')
    expect(collapsed).toContain(CARD_EXPAND_HINT_TEXT)
    expect(collapsed).not.toContain('replyTo message-1')
    const expanded = renderedCard(
      renderCardGroup(
        theme,
        [
          {
            kind: 'intercom-message',
            icon: domainIcon('📬'),
            titleStyle: 'accent-bold',
            title: 'Intercom',
            body: { kind: 'prose', text: 'short body' },
            expandedFooter: [{ token: 'dim', text: 'replyTo message-1' }],
            boxed: true
          },
          {
            kind: 'intercom-message',
            icon: domainIcon('⚡'),
            titleStyle: 'accent-bold',
            title: 'Intercom priority',
            body: { kind: 'prose', text: 'another body' },
            expandedFooter: [{ token: 'dim', text: 'replyTo message-2' }],
            boxed: true
          }
        ],
        { expanded: true }
      ),
      160
    )
    expect(expanded).toContain('replyTo message-1')
    expect(expanded).toContain('replyTo message-2')
  })
})

describe('card-style: toolFailureText (#150 shared per-tool failure idiom)', () => {
  test('collapsed: failure title, dim preview with ellipsis and expand hint when truncated', () => {
    const theme = trackingTheme()
    const long = 'x'.repeat(150)
    const text = toolFailureText(theme, 'Goal not recorded', long, false)
    expect(text.split('\n')[0]).toContain('⚠️ Goal not recorded')
    expect(theme.calls[0]).toEqual({ token: 'error', text: '⚠️ Goal not recorded' })
    // Preview is dim, ellipsized INSIDE the dim span, hint dim.
    const dim = theme.calls.filter((c) => c.token === 'dim')
    const [preview, hint] = dim
    expect(preview?.text).toBe(`· ${'x'.repeat(120)}…`)
    expect(hint?.text).toBe('(Ctrl+O to expand)')
  })

  test('collapsed short message: no ellipsis, no hint', () => {
    const theme = trackingTheme()
    const text = toolFailureText(theme, 'Escalation not recorded', 'boom', false)
    expect(text).toBe('⚠️ Escalation not recorded\n· boom')
    expect(theme.calls.some((c) => c.text.includes('Ctrl+O'))).toBe(false)
  })

  test('expanded: raw message verbatim after a blank line, uncolored', () => {
    const theme = trackingTheme()
    const text = toolFailureText(theme, 'Department not created', 'raw output', true)
    expect(text).toBe('⚠️ Department not created\n\nraw output')
    expect(theme.calls.length).toBe(1) // only the title span
  })

  test('target renders as a dim `· target` OUTSIDE the failure span (color-pass vocabulary)', () => {
    const theme = trackingTheme()
    toolFailureText(theme, 'Goal not settled', '', false, { target: '@quant-head' })
    expect(theme.calls[0]).toEqual({ token: 'error', text: '⚠️ Goal not settled' })
    expect(theme.calls[1]).toEqual({ token: 'dim', text: '· @quant-head' })
  })

  test('expanded with empty message degrades to the bare title', () => {
    const theme = trackingTheme()
    expect(toolFailureText(theme, 'Control board unavailable', '', true)).toBe(
      '⚠️ Control board unavailable'
    )
  })
})

/**
 * The permanent-failure detector (`Taperoom Inc`, 2026-08-18). The point of
 * these is the NEGATIVE cases as much as the positive one: a detector that is
 * eager here would route a genuine transient outage — which the same company
 * was having that day, on a different model — onto the permanent path and stop
 * counting real provider failures.
 */
describe('providerRequestTooLargeError', () => {
  const OBSERVED =
    '400: {"message":"This endpoint\'s maximum context length is 262144 tokens. ' +
    'However, you requested about 262175 tokens (18355 of text input, 10003 of ' +
    'tool input, 233817 in the output). Please reduce the length of either one."}'

  test('reads both numbers out of the rejection the operator actually saw', () => {
    expect(providerRequestTooLargeError(OBSERVED)).toEqual({ limit: 262144, requested: 262175 })
  })

  test('reads the earlier wording, which omits "about"', () => {
    expect(
      providerRequestTooLargeError(
        '400 ... maximum context length is 1048576 tokens. However, you requested 1053371'
      )
    ).toEqual({ limit: 1048576, requested: 1053371 })
  })

  test('accepts an Error and an { errorMessage } holder, like its sibling detector', () => {
    expect(providerRequestTooLargeError(new Error(OBSERVED))).toEqual({
      limit: 262144,
      requested: 262175
    })
    expect(providerRequestTooLargeError({ errorMessage: OBSERVED })).toEqual({
      limit: 262144,
      requested: 262175
    })
  })

  test('a transient provider failure is NOT permanent', () => {
    expect(providerRequestTooLargeError('Connection error.')).toBeUndefined()
    expect(providerRequestTooLargeError('503 status code (no body)')).toBeUndefined()
    expect(providerRequestTooLargeError('terminated')).toBeUndefined()
    expect(providerRequestTooLargeError('Provider is not configured: openrouter')).toBeUndefined()
  })

  test('prose that merely mentions a context window does not match', () => {
    expect(
      providerRequestTooLargeError('the maximum context length is 262144 tokens for this model')
    ).toBeUndefined()
  })

  test('a nullish or non-string message is undefined, never a throw', () => {
    expect(providerRequestTooLargeError(undefined)).toBeUndefined()
    expect(providerRequestTooLargeError(null)).toBeUndefined()
    expect(providerRequestTooLargeError(42)).toBeUndefined()
  })
})

/**
 * The two pane-failure cards, and the one function that decides which is which.
 *
 * `paneFailureSpec` exists because both producers append the SAME entry type,
 * so the renderer has to read the payload. It did not: it built the
 * configuration card unconditionally, and a delivered overflow card would have
 * told the operator their provider had no credentials. Legible and wrong.
 */
describe('the pane-failure cards', () => {
  test('the overflow card carries both sentences and both numbers', () => {
    const spec = providerRequestTooLargeSpec({ requested: 262175, limit: 262144 })
    const detail = Array.isArray(spec.detail) ? spec.detail.join('\n') : String(spec.detail)
    expect(detail).toContain("did not fit the model's context window")
    expect(detail).toContain('will not be retried')
    expect(detail).toContain('262175')
    expect(detail).toContain('262144')
    // The remedy names the OUTPUT reservation, because that is what overflows
    // and it is the part a compaction cannot shrink.
    expect(detail).toContain('output reservation')
    expect(spec.kind).toBe('pane-failure')
  })

  test('the overflow card carries the person and the log only when it has them', () => {
    const bare = providerRequestTooLargeSpec({ requested: 10, limit: 5 })
    expect(bare.target).toBeUndefined()
    expect(Array.isArray(bare.detail) ? bare.detail.join('\n') : '').not.toContain('Log:')
    const full = providerRequestTooLargeSpec({
      requested: 10,
      limit: 5,
      personId: 'head-of-engineering',
      logPath: '/c/.chief/logs/exceptions.jsonl'
    })
    expect(full.target).toBe('@head-of-engineering')
    expect(Array.isArray(full.detail) ? full.detail.join('\n') : '').toContain(
      'Log: /c/.chief/logs/exceptions.jsonl'
    )
  })

  test('a payload with both numbers is the overflow card', () => {
    const spec = paneFailureSpec({ requested: 262175, limit: 262144, personId: 'enzo' })
    expect(spec.title).toBe('Request too large for the context window')
    expect(spec.target).toBe('@enzo')
  })

  test('a payload with a provider is the configuration card', () => {
    const spec = paneFailureSpec({ provider: 'openrouter', personId: 'enzo' })
    expect(spec.title).toBe('Provider not configured')
    expect(Array.isArray(spec.detail) ? spec.detail.join('\n') : '').toContain("'openrouter'")
  })

  test('a payload with neither still names the configured provider, never throws', () => {
    const spec = paneFailureSpec({})
    expect(spec.title).toBe('Provider not configured')
    expect(spec.target).toBeUndefined()
  })

  test('a half-payload is never mistaken for an overflow', () => {
    // A number without its partner cannot state the comparison the card is
    // about, so it must not select the card that promises both.
    expect(paneFailureSpec({ requested: 262175 }).title).toBe('Provider not configured')
    expect(paneFailureSpec({ limit: 262144 }).title).toBe('Provider not configured')
  })
})
