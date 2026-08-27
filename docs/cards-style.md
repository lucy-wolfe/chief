# Card house style

One style for every rendered card surface (boxed message cards, tool call
cards, tool result cards, chrome). Established by the #354 card audit;
codified here and in `extensions/card-style.ts` so every card ticket (#355
foundation, #357-#362 cleanup) converges on the same rules instead of
re-deriving them per file.

## Anatomy

One line of identity, one line of substance, optional expand:

```
<state-emoji> <Title, sentence case, ≤5 words> · <human target>
<dim detail — what happened / what happens next>
[(Ctrl+O to expand)]
```

## State-emoji vocabulary (exactly one per state class)

| State | Emoji | Theme token | Meaning |
|---|---|---|---|
| in-progress (`renderCall`) | the tool's own *domain* emoji (🧭 📤 📌 …) | `dim` | the whole call line renders dim — title is a verb-ing phrase, nothing has happened yet |
| success | ✅ | `success` | past-tense verified claim (`✅ Task created · Fix login bug`); a read-only op with no mutation may keep its domain emoji instead (`📋 Roster updated`) |
| retryable wait | ⏳ | `warning` | the caller can retry once conditions change (busy, rate limited) |
| blocked on a person | 🤝 | `warning` | progress needs another human/agent's action (handoff pending) |
| input repair | 🧾 | `warning` | the caller's own arguments need fixing before a retry can succeed |
| hard failure | ⚠️ | `error` | not retryable as-is; something is actually broken |
| circuit stopped | 🛑 | `warning` | a bounded retry loop hit its cap and stopped itself |

Titles are never lowercase sentence fragments (`choose a teammate`), never
ALL-CAPS, never a raw internal verb interpolated into `"${action} failed"`.

Use `extensions/card-style.ts`'s `CARD_STATE_EMOJI` / `CARD_STATE_TOKEN` (or
the `cardTitle`/`cardCallLine` helpers, which apply them for you) instead of
hand-picking an emoji or color per card.

## Color: theme tokens only

Zero raw ANSI escapes, zero hex constants, in any card path. Every color comes
from `theme.fg(token, text)` / `theme.bg(token, text)` using one of:
`success`, `warning`, `error`, `dim`, `accent`, `customMessageText`,
`customMessageBg`. Sender/footer accents must be theme-derived, not a
hardcoded palette — they must read correctly in light, dark, and system theme
modes.

## Human-readable references

Cards show the human short name: `@person`, or a goal/assignment/task's short
*title*. Raw ids (`goal-…`, `task-…`, `transition-…`,
generations) appear only in the expanded view, never in the collapsed line.
Use `extensions/card-style.ts`'s `humanRef` (a generalization of the existing
`boundedSystemNoticeText` scrubber) instead of re-deriving an id-stripping
regex per card.

## Boxes

`customMessageBg` box = inbound/ambient events only (mail, wake-ups,
recovery). Tool call/result cards are plain `Text` — never boxed. This split
is already correct in the codebase; keep it that way.

## Expand hint

Exactly one spelling, dim, last line: `(Ctrl+O to expand)`. Use
`card-style.ts`'s `CARD_EXPAND_HINT_TEXT` constant or `cardHint(theme)` helper
— never `(Ctrl+O for details)`, `(Ctrl+O for recovery steps)`, a bare
`Ctrl+O to expand`, or a custom "More context: N items" variant.

## Update behavior

One card per logical operation. A multi-phase operation (e.g. session
maintenance's queued → running → completed) updates the SAME card in place or
replaces it — it never appends a new card per phase, and no card is ever
written on a timer.

## Verification bar (every card-touching PR)

Chief's shared tmux server grants the browser `xterm*` terminal family separate
`RGB` and `extkeys` features before a new operator client attaches. Do not
replace exact RGB card or sidebar colors with 256-color approximations; a real
tmux capture must retain their `38;2` and `48;2` sequences.

"Tests pass" is not acceptance. Every PR that touches a card must:
1. Render the touched card(s) in a real tmux pane.
2. `tmux capture-pane -e` (or the wrapper the repo already uses).
3. Decode the emitted fg/bg escape codes to RGB and confirm they read
   correctly in **light**, **dark**, and **system** terminal theme modes.
4. Attach the captures (or their decoded RGB values) to the PR/handoff.

## Shared helpers (`extensions/card-style.ts`)

This is a standalone module with zero `../src` imports (every extension under
`extensions/` is a copied deployment unit and must load without the parent
source tree — see `appendOrganizationLogLine`'s doc comment for the same
rule applied elsewhere). It exports:

- `CARD_STATE_EMOJI`, `CARD_STATE_TOKEN`, `cardStateIcon(state)` — the fixed
  vocabulary table above, as data.
- `domainIcon(emoji, token?)` — wraps a tool's own domain emoji into the same
  `{emoji, token}` shape the fixed states use, for `renderCall` lines.
- `cardTitle(theme, icon, title, target?)` — the anatomy's first line:
  `<emoji> <Title>` in the state's token color, ` · <target>` in dim.
- `cardCallLine(theme, { emoji, title, target? })` — the in-progress variant:
  the whole line renders in `dim`, per the vocabulary table.
- `cardDetail(theme, text)` — the anatomy's second (dim) line.
- `CARD_EXPAND_HINT_TEXT`, `cardHint(theme)` — the one expand-hint spelling.
- `scrubHumanRef(text, opts)`, `finalizeHumanRef(text, max)`, `humanRef(value,
  opts)` — the id-stripping/truncation pipeline; `humanRef` is the one-call
  version, the other two exist so a caller needing a custom prefix-strip
  in between (like `boundedSystemNoticeText`'s assignment-header stripping)
  can still reuse the shared scrub + truncate halves.

Every helper takes a minimal `CardTheme` shape (`fg`/`bold`/optional `bg`) so
it works against both the real Pi theme object and the `plainTheme()` test
fixture already used across `tests/*.test.ts`.
