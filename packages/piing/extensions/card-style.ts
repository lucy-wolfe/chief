/**
 * Shared card presentation helpers — the house style every rendered card
 * converges on (see `docs/cards-style.md` for the full spec, established by
 * the #354 audit). This module is imported by both `organization-intercom.ts`
 * and `team-ui.ts` and must never depend on `../src`: every file under
 * `extensions/` is a copied deployment unit that has to load standalone (the
 * same rule `appendOrganizationLogLine` documents elsewhere).
 *
 * Anatomy every card renders to:
 *   <state-emoji> <Title, sentence case, ≤5 words> · <human target>
 *   <dim detail — what happened / what happens next>
 *   [(Ctrl+O to expand)]
 *
 * The unified entry point every card site converges on is {@link renderCard}
 * (see the "Unified card renderer" section at the bottom of this file): a call
 * site builds a declarative {@link CardSpec} and hands it to `renderCard`, which
 * owns the whole visual assembly — box-vs-plain, the title/call line, dim detail
 * lines, body collapse rules, and the single expand hint. Nothing below that
 * entry point re-implements card layout by string concatenation.
 */

import { Box, Text } from "@earendil-works/pi-tui";

/** The minimal theme shape every helper needs — matches both the real Pi
 * theme object and the `plainTheme()` fixture used across `tests/*.test.ts`. */
export interface CardTheme {
  fg(token: string, text: string): string;
  bold(text: string): string;
  bg?(token: string, line: string): string;
  getBgAnsi?(token: string): string;
}

/**
 * Fixed state-emoji vocabulary — exactly one glyph per state class.
 * Deliberately excludes "in-progress": a `renderCall` line uses the tool's
 * own domain emoji instead (see {@link domainIcon}), not a vocabulary entry.
 */
/**
 * EVERY GLYPH ANY CARD DRAWS, in one reviewed place.
 *
 * # Why a table and not a literal at each call site
 *
 * A card icon is chosen by whoever writes the card, on their machine, in their
 * font. It renders there, so it ships. The operator then sees tofu or a glyph
 * that overdraws the next column, and nothing anywhere failed.
 *
 * # The property that makes a glyph safe, which is not "it looked fine"
 *
 * A codepoint is admitted here when it is **Emoji_Presentation=Yes** and
 * **East_Asian_Width=Wide**. Those two together mean every wcwidth-based
 * terminal — tmux included, and tmux is always in this stack — allocates the
 * same TWO columns the font wants to draw into, so the glyph cannot overdraw
 * its neighbour.
 *
 * What is excluded, and why each broke:
 *
 * * **Text-default codepoints** (`Emoji_Presentation=No`) such as U+1F3D7
 *   BUILDING CONSTRUCTION, U+23F8, U+25B6, U+1F5D1, U+1F5D3, U+1F6E1. A
 *   wcwidth terminal gives them ONE column while an emoji font draws two, and
 *   many font chains carry no text-style glyph for them at all — they exist
 *   almost only in colour-emoji fonts. Tofu, or a stretched glyph.
 * * **U+FE0F (VS16)**, the "please draw the previous character as emoji"
 *   request. It is zero-width to wcwidth and honoured inconsistently by fonts,
 *   so it widens the disagreement rather than fixing it. No entry here carries
 *   one.
 * * **Codepoints newer than Unicode 11.0** (2018) — U+1FA91 CHAIR, U+1FA86
 *   NESTING DOLLS — which are simply absent from older font chains.
 *
 * We cannot see any operator's font, so this optimises for the strongest
 * property that is provable without one: width agreement and coverage breadth.
 */
export const CARD_GLYPHS = {
  // Card states.
  success: "✅",
  wait: "⏳",
  handoff: "🤝",
  inputRepair: "🧾",
  failure: "❗",
  circuit: "🛑",

  // Domains. One meaning per glyph — an icon that means two opposite things
  // teaches nothing, which is why hire and offboard no longer share one.
  send: "📤",
  inbox: "📥",
  roster: "📋",
  mailbox: "📬",
  message: "💬",
  report: "📊",
  alert: "🚨",
  energy: "⚡",
  goal: "🎯",
  person: "👤",
  think: "🧠",
  compass: "🧭",
  calendar: "📅",
  lock: "🔒",

  // Structure and lifecycle.
  department: "🏢",
  starting: "🚀",
  pausing: "💤",
  stopping: "🔻",
  resuming: "⏩",
  removing: "🧹",
  hire: "👋",
  offboard: "🚪",
  bench: "💺",
  recall: "🔔",
  appointHead: "👑",
  transfer: "🚚",
  moveDepartment: "🌳",
  startPerson: "🌱",
  stopPerson: "🍃",

  // Reminders.
  reminder: "⏰",
  reminderOff: "🔕",
} as const;

/**
 * Bare text symbols — NOT emoji, and they must never gain a VS16.
 *
 * These are single-column drawing characters with broad monospace coverage.
 * They are listed separately because the rule above is the wrong rule for
 * them: they are width-1 by design and asking a font to draw them as emoji is
 * what breaks them.
 */
export const CARD_TEXT_SYMBOLS = {
  arrow: "→",
  branch: "↳",
  notEqual: "≠",
  atMost: "≤",
  refresh: "⟳",
  gear: "\u2699",
} as const;

export type CardState = "success" | "wait" | "handoff" | "input-repair" | "failure" | "circuit";

export const CARD_STATE_EMOJI: Readonly<Record<CardState, string>> = {
  success: CARD_GLYPHS.success,
  wait: CARD_GLYPHS.wait,
  handoff: CARD_GLYPHS.handoff,
  "input-repair": CARD_GLYPHS.inputRepair,
  failure: CARD_GLYPHS.failure,
  circuit: CARD_GLYPHS.circuit,
};

/** Theme token each state's title renders in. */
export const CARD_STATE_TOKEN: Readonly<Record<CardState, string>> = {
  success: "success",
  wait: "warning",
  handoff: "warning",
  "input-repair": "warning",
  failure: "error",
  circuit: "warning",
};

/** A resolved `{emoji, token}` pair, the shape both fixed states and a
 * domain-emoji in-progress line reduce to before formatting. */
export interface CardIcon {
  emoji: string;
  token: string;
}

/** Looks up the fixed vocabulary entry for a concluded state. */
export function cardStateIcon(state: CardState): CardIcon {
  return { emoji: CARD_STATE_EMOJI[state], token: CARD_STATE_TOKEN[state] };
}

/** Wraps a tool's own domain emoji (🧭 📤 🧠 …) for a `renderCall` line. Not
 * part of the fixed vocabulary by design — every tool picks its own. */
export function domainIcon(emoji: string, token = "dim"): CardIcon {
  return { emoji, token };
}

function resolveCardIcon(icon: CardState | CardIcon): CardIcon {
  return typeof icon === "string" ? cardStateIcon(icon) : icon;
}

/**
 * The anatomy's first line for a concluded (non-in-progress) card:
 * `<emoji> <Title>` in the state's token color, plus ` · <target>` in dim
 * when a target is given. Accepts either a fixed {@link CardState} or a
 * custom `{emoji, token}` pair (e.g. a domain emoji kept in its success
 * color for a read-only op, per the vocabulary table's "may keep its domain
 * emoji" success carve-out).
 */
export function cardTitle(
  theme: CardTheme,
  icon: CardState | CardIcon,
  title: string,
  target?: string,
  mentions?: MentionColorizer,
): string {
  const resolved = resolveCardIcon(icon);
  const head = theme.fg(resolved.token, `${resolved.emoji} ${title}`);
  return target ? `${head} ${theme.fg("dim", `· ${colorizeTarget(target, mentions)}`)}` : head;
}

/**
 * A pure `@name → colored @name` string transform (see
 * `organization-intercom.ts`'s `colorizePersonMentions`). Passed into
 * {@link cardTitle}/{@link cardCallLine} so a card target's `@person` mentions
 * render in that person's identity accent (#433) without this shared module
 * having to know anything about the roster or the color math. `undefined`
 * leaves the target exactly as before.
 */
export type MentionColorizer = (target: string) => string;

function colorizeTarget(target: string, mentions?: MentionColorizer): string {
  return mentions ? mentions(target) : target;
}

/**
 * The boxed-message header variant: `<emoji> <bold title>` with an optional dim
 * ` · <target>` suffix. Distinct from {@link cardTitle} (which colors the whole
 * `<emoji> <title>` in a state token): launcher/intercom message cards title in
 * bold rather than a status hue, and the emoji stays unstyled. Used by
 * {@link renderCard} when a spec sets `titleStyle: "bold"`.
 */
export function cardBoldTitle(
  theme: CardTheme,
  emoji: string,
  title: string,
  target?: string,
  mentions?: MentionColorizer,
): string {
  const head = emoji ? `${emoji} ${theme.bold(title)}` : theme.bold(title);
  return target ? `${head} ${theme.fg("dim", `· ${colorizeTarget(target, mentions)}`)}` : head;
}

/**
 * The accent-colored bold header: `theme.fg(accent, theme.bold("<emoji> <title>"))`,
 * with the emoji INSIDE the span. Distinct from {@link cardBoldTitle} (plain
 * bold, emoji unstyled): the launcher system-notice and sender-labelled cards
 * title in a colored, emphatic hue. `accent` defaults to the theme's `"accent"`
 * token; pass another (e.g. an identity token) to color a sender label.
 */
export function cardAccentBoldTitle(theme: CardTheme, emoji: string, title: string, accent = "accent"): string {
  return theme.fg(accent, theme.bold(emoji ? `${emoji} ${title}` : title));
}

/**
 * The in-progress (`renderCall`) variant: per the vocabulary table, the
 * WHOLE line renders dim — title and target alike — since nothing has
 * happened yet. `emoji` is the tool's own domain glyph, not a vocabulary
 * lookup.
 */
export function cardCallLine(
  theme: CardTheme,
  options: { emoji: string; title: string; target?: string },
  mentions?: MentionColorizer,
): string {
  const head = theme.fg("dim", `${options.emoji} ${options.title}`);
  return options.target ? `${head} ${theme.fg("dim", `· ${colorizeTarget(options.target, mentions)}`)}` : head;
}

/** The anatomy's second line: dim detail text (what happened / what happens next). */
export function cardDetail(theme: CardTheme, text: string): string {
  return theme.fg("dim", text);
}

/**
 * Detect Pi's hard "provider is not configured" startup/turn failure and pull
 * the provider name out (#399 part 2). This is a CONFIGURATION failure — a pane
 * that cannot run at all because its provider has no credentials — not a
 * transient/retryable provider outage, so callers route it to a `failure` card
 * instead of the reliability-escalation path. Returns `undefined` for any other
 * message so an ordinary provider error is never mis-carded as a config problem.
 */
export function providerConfigurationError(message: unknown): { provider: string } | undefined {
  const match = /provider is not configured:?\s*([A-Za-z0-9._-]+)/i.exec(providerErrorText(message));
  return match ? { provider: match[1] } : undefined;
}

/** The provider error string, however the caller happens to be holding it. */
function providerErrorText(message: unknown): string {
  return typeof message === "string"
    ? message
    : message instanceof Error
      ? message.message
      : message && typeof message === "object" && typeof (message as { errorMessage?: unknown }).errorMessage === "string"
        ? (message as { errorMessage: string }).errorMessage
        : "";
}

/**
 * Detect the provider's PERMANENT "this request does not fit the context
 * window" rejection and pull both numbers out of it.
 *
 * Like {@link providerConfigurationError}, this exists to keep a failure that
 * CANNOT succeed out of the reliability-escalation path. A 400 of this shape is
 * a statement about the request we just built, not about the provider's health:
 * the same request will be rejected identically forever, so counting it toward
 * "N consecutive provider failures" accuses the provider of an outage it is not
 * having, and tells the reader to go check model health that is fine.
 *
 * Observed on the operator's live company (`Taperoom Inc`, 2026-08-18), eight
 * times across five people, all on `moonshotai/kimi-k2.6`:
 *
 *   "This endpoint's maximum context length is 262144 tokens. However, you
 *    requested about 262175 tokens (18355 of text input, 10003 of tool input,
 *    233817 in the output)."
 *
 * Note WHICH number is large: only ~28k of that is prompt, and 233817 is the
 * reservation for OUTPUT. That is why this is worth naming separately rather
 * than folding into the existing compaction escape hatch — compacting a 28k
 * prompt cannot clear an overflow that the output reservation causes, so a pane
 * in this state is not one compaction away from recovering.
 *
 * Returns `undefined` for every other message, so an ordinary transient
 * provider error is never mis-carded as a permanent one.
 */
export function providerRequestTooLargeError(message: unknown): { limit: number; requested: number } | undefined {
  const match = /maximum context length is (\d+) tokens[\s\S]{0,80}?you requested (?:about )?(\d+)/i
    .exec(providerErrorText(message));
  if (!match) return undefined;
  return { limit: Number(match[1]), requested: Number(match[2]) };
}

/**
 * The legible failure card body for a pane that cannot start because its
 * provider has no configured credentials (#399 part 2). Replaces the raw
 * `Error: Provider is not configured: <name>` dump with the house `failure`
 * vocabulary: a clear reason, an actionable remedy, and a pointer to the log
 * where the underlying error is persisted for debugging.
 */
export interface ProviderConfigurationFailureOptions {
  provider: string;
  personId?: string;
  logPath?: string;
}

/**
 * The {@link CardSpec} for the provider-not-configured failure card — the
 * single source both {@link providerConfigurationFailureCard} (string form) and
 * the `PANE_FAILURE_TYPE` renderer build from, so the two can never drift.
 */
export function providerConfigurationFailureSpec(options: ProviderConfigurationFailureOptions): CardSpec {
  const detail = [
    `Provider '${options.provider}' has no configured credentials, so this pane cannot start a turn.`,
    `Fix: configure credentials for '${options.provider}' or migrate this person's model.`,
  ];
  if (options.logPath) detail.push(`Log: ${options.logPath}`);
  return {
    kind: "pane-failure",
    icon: "failure",
    title: "Provider not configured",
    target: options.personId ? `@${options.personId}` : undefined,
    detail,
    body: { kind: "none" },
    boxed: true,
  };
}

export function providerConfigurationFailureCard(
  theme: CardTheme,
  options: ProviderConfigurationFailureOptions,
): string {
  return renderCardText(theme, providerConfigurationFailureSpec(options));
}

/**
 * The legible failure card for a turn the model's context window refused.
 *
 * The counterpart to {@link providerConfigurationFailureSpec}, and the reason
 * {@link providerRequestTooLargeError} exists: without it the pane shows
 * OpenRouter's raw 400, whose `previous_errors` array repeats the same sentence
 * about fifteen times before the transcript truncates it — two screens of JSON
 * where one sentence was wanted.
 *
 * Two things the reader needs and the raw dump does not say:
 *
 *   1. THE PROVIDER IS FINE. A 400 in a scrollback full of 5xx reads like an
 *      outage. This one is not one, and saying so stops the operator checking
 *      provider health that is healthy.
 *   2. NOTHING WILL RETRY. The same request is rejected identically for ever,
 *      so a reader waiting for the pane to recover on its own waits for ever.
 *
 * The remedy names the OUTPUT reservation rather than the prompt, because that
 * is what overflows here (233817 of 262175 tokens in the observed case) and it
 * is exactly the part a compaction cannot shrink.
 */
export interface ProviderRequestTooLargeOptions {
  requested: number;
  limit: number;
  personId?: string;
  logPath?: string;
}

/**
 * The {@link CardSpec} for the request-too-large failure card. Both sentences
 * the operator is looking for — "did not fit the model's context window" and
 * "will not be retried" — live here and nowhere else, so the pane and any
 * string rendering of the same card can never disagree.
 */
export function providerRequestTooLargeSpec(options: ProviderRequestTooLargeOptions): CardSpec {
  const detail = [
    `This turn's request did not fit the model's context window (${options.requested} tokens requested, ${options.limit} allowed).`,
    "The provider is reachable and this will not be retried.",
    "Fix: lower this model's output reservation, or move this person to a model with a larger window. Compaction cannot clear an output reservation.",
  ];
  if (options.logPath) detail.push(`Log: ${options.logPath}`);
  return {
    kind: "pane-failure",
    icon: "failure",
    title: "Request too large for the context window",
    target: options.personId ? `@${options.personId}` : undefined,
    detail,
    body: { kind: "none" },
    boxed: true,
  };
}

/**
 * The provider REFUSED the turn on content, and the turn's inbound mail was
 * destroyed by that refusal.
 *
 * Three things the reader needs, none of which Pi's raw
 * `Error: Provider finish_reason: content_filter` says — and it is usually sat
 * under the provider's own canned refusal in Chinese, which reads like the
 * agent said something rather than like the route refused it:
 *
 *   1. THE PROVIDER IS HEALTHY. Nothing is down. This request was declined on
 *      what it contained, so checking provider access finds nothing wrong.
 *   2. NOTHING WILL RETRY, and a replay would be refused identically — the
 *      filter is a function of the content, not of the moment.
 *   3. THE MESSAGE THAT STARTED THIS TURN WAS CONSUMED. It was receipted at
 *      turn start, so it is gone from the mailbox whether or not it was read.
 *      The sender is bounced separately; the person at the pane has to know it
 *      happened or they will wait for an answer to a message nobody read.
 *
 * The remedies are the two that actually exist. Retrying is not one of them.
 */
export interface ProviderContentFilterOptions {
  personId?: string;
  logPath?: string;
  /** How many inbound deliveries this refused turn consumed, if any. */
  consumedDeliveries?: number;
}

/** The {@link CardSpec} for the content-refusal failure card. */
export function providerContentFilterSpec(options: ProviderContentFilterOptions): CardSpec {
  const detail = [
    "The provider refused this turn on its CONTENT (finish_reason: content_filter).",
    "The provider is reachable and healthy; an identical retry is refused identically, so this will not be retried.",
  ];
  if (options.consumedDeliveries) {
    detail.push(
      options.consumedDeliveries === 1
        ? "The message that started this turn was receipted at turn start and is gone from the mailbox — it was NOT read. Its sender has been told."
        : `The ${options.consumedDeliveries} messages that started this turn were receipted at turn start and are gone from the mailbox — none was read. Their senders have been told.`,
    );
  }
  detail.push("Fix: re-scope or rephrase the work so it stops carrying what the filter refuses, or move this person to a model whose filter does not fire on it.");
  if (options.logPath) detail.push(`Log: ${options.logPath}`);
  return {
    kind: "pane-failure",
    icon: "failure",
    title: "The provider refused this turn on content",
    target: options.personId ? `@${options.personId}` : undefined,
    detail,
    body: { kind: "none" },
    boxed: true,
  };
}

/** A 402 from the route: the ACCOUNT is empty. Lives here beside its card and
 * beside {@link providerRequestTooLargeError}, which reads a provider error
 * string for exactly the same reason. */
export function providerInsufficientCreditsError(message: unknown): boolean {
  const text = typeof message === "string" ? message : "";
  return /\b402\b/.test(text) && /insufficient[_ -]?credits/i.test(text);
}

/**
 * The route answered 402: the ACCOUNT this pane runs on has no credits.
 *
 * The one failure on this card's list whose remedy belongs to a HUMAN and to
 * nobody else. Every other pane-failure card describes something an agent or a
 * config change can address; this one can only be cleared by whoever owns the
 * account. Filed as an ordinary provider error it did the opposite: it climbed
 * the reliability counter and mailed a manager AGENT asking them to check
 * provider health, which that agent cannot do and which was not the problem.
 */
export interface ProviderInsufficientCreditsOptions {
  personId?: string;
  logPath?: string;
}

/** The {@link CardSpec} for the out-of-credits failure card. */
export function providerInsufficientCreditsSpec(options: ProviderInsufficientCreditsOptions): CardSpec {
  const detail = [
    "The provider answered 402: this account is out of credits.",
    "Nothing is broken and nothing will retry — every turn is refused identically until the account is topped up.",
    "Fix: add credits to the account this Pi is configured against. No agent in this company can do it.",
  ];
  if (options.logPath) detail.push(`Log: ${options.logPath}`);
  return {
    kind: "pane-failure",
    icon: "failure",
    title: "The provider account is out of credits",
    target: options.personId ? `@${options.personId}` : undefined,
    detail,
    body: { kind: "none" },
    boxed: true,
  };
}

/**
 * The model keeps PRINTING its tool calls instead of making them.
 *
 * The turn completes, nothing runs, and every rule reads the person as having
 * finished their work — so this is invisible without being said out loud. Past
 * the corrective cap the model is the problem rather than the turn, and only
 * the operator can act on that.
 */
export interface ProviderPrintedToolCallOptions {
  personId?: string;
  logPath?: string;
  corrections: number;
}

/** The {@link CardSpec} for the printed-tool-call card. */
export function providerPrintedToolCallSpec(options: ProviderPrintedToolCallOptions): CardSpec {
  const detail = [
    "This model has written its tool calls as TEXT instead of executing them.",
    `Corrected ${options.corrections} times this session and it kept happening, so no further corrections will be sent — a fourth would be a loop against a model that has ignored three.`,
    "The turns LOOK finished: nothing runs, no error is raised, and the work simply does not happen.",
    "Fix: move this person to a different model, or update Pi — the behaviour is the model's, not this company's.",
  ];
  if (options.logPath) detail.push(`Log: ${options.logPath}`);
  return {
    kind: "pane-failure",
    icon: "failure",
    title: "The model is printing tool calls instead of making them",
    target: options.personId ? `@${options.personId}` : undefined,
    detail,
    body: { kind: "none" },
    boxed: true,
  };
}

/**
 * The one place that decides WHICH pane-failure card a recorded failure is.
 * Both producers append the same `PANE_FAILURE_TYPE` entry, so the renderer
 * must discriminate on the payload rather than assume — it assumed, and a
 * delivered overflow card would have rendered as "Provider not configured".
 */
export function paneFailureSpec(details: {
  provider?: unknown;
  personId?: unknown;
  logPath?: unknown;
  requested?: unknown;
  limit?: unknown;
  contentFiltered?: unknown;
  consumedDeliveries?: unknown;
  insufficientCredits?: unknown;
  printedToolCalls?: unknown;
}): CardSpec {
  const personId = typeof details.personId === "string" ? details.personId : undefined;
  const logPath = typeof details.logPath === "string" ? details.logPath : undefined;
  // Discriminated FIRST, and on its own explicit flag rather than on the
  // absence of the other two payloads: a content refusal carries no numbers,
  // so an "everything else is a configuration failure" tail would render it as
  // "Provider not configured" — which is the exact mis-render the comment
  // above this function was written about.
  if (typeof details.printedToolCalls === "number") {
    return providerPrintedToolCallSpec({
      personId,
      logPath,
      corrections: details.printedToolCalls,
    });
  }
  if (details.insufficientCredits === true) {
    return providerInsufficientCreditsSpec({ personId, logPath });
  }
  if (details.contentFiltered === true) {
    return providerContentFilterSpec({
      personId,
      logPath,
      consumedDeliveries: typeof details.consumedDeliveries === "number" ? details.consumedDeliveries : undefined,
    });
  }
  if (typeof details.requested === "number" && typeof details.limit === "number") {
    return providerRequestTooLargeSpec({ requested: details.requested, limit: details.limit, personId, logPath });
  }
  const provider = typeof details.provider === "string" && details.provider ? details.provider : "the configured provider";
  return providerConfigurationFailureSpec({ provider, personId, logPath });
}

/** The one expand-hint spelling the whole product uses. */
export const CARD_EXPAND_HINT_TEXT = "(Ctrl+O to expand)";

/** Themed expand hint, ready to append as the card's last line. */
export function cardHint(theme: CardTheme): string {
  return theme.fg("dim", CARD_EXPAND_HINT_TEXT);
}


// --- Identity accents: THE ORIGIN ------------------------------------------
//
// #150 batch D: this module is the ORIGIN of the roster accent palette, the
// hue-wrap allocator, and the operator exemption. `organization-intercom.ts`
// imports these directly (extension -> extension). `src/foundation/theme.ts`
// carries a copy under the "extensions cannot import ../src" deployment rule
// (and src does not import a copied deployment unit either) — that copy is
// pinned MECHANICALLY by tests/org-accent-uniqueness.test.ts (palette
// byte-equality + allocator behavioral equality across wrap sizes), replacing
// the old "if you edit one, edit both" comment contract.
//
// #588: fixed Google/Material-derived role hues, rebalanced to the same stable
// raw-identity luminance band as src/home/IdentityTheme.ts. This is copied
// because Pi extensions cannot import ../src; org-accent-uniqueness.test.ts
// pins it byte-for-byte to that source of truth.
export const ORGANIZATION_PERSON_ACCENTS = [
  // #603: orange/amber were `#ba6700`/`#a67200` — both rounded to the same
  // xterm-256 cube cell (`#af5f00`) and landed black ink at 4.4597:1, under
  // the 4.5 floor. Re-picked to `#c75e00`/`#a27400`; keep byte-identical to
  // `src/foundation/theme.ts`.
  "#e24033", "#c75e00", "#a27400", "#2c8e46", "#00899a",
  "#3c7adf", "#6977c5", "#a74ef5", "#d83d98", "#c05e68",
] as const;

/** #120: the reserved ids denoting the HUMAN operator rather than an agent. */
export const OPERATOR_IDENTITY_IDS = ["operator"] as const;

/** #120: is this id the human operator rather than a roster person? */
export function isOperatorIdentity(personId: string): boolean {
  return (OPERATOR_IDENTITY_IDS as readonly string[]).includes(personId.trim().toLowerCase());
}

// TOMBSTONE: `STANDARD_PI_IDENTITY_IDS` (`operator`/`ceo`) and
// `isStandardPiIdentity` stood here as string-id appearance authority. The
// allocator below still runs for every roster person, including the Chief,
// because the rail needs every identity color. The create-once home writer
// skips the Chief structurally, and intercom resolves the Chief from the root
// department, so this extension needs no standard-id list.
// `OPERATOR_IDENTITY_IDS`/`isOperatorIdentity` above are a DIFFERENT rule
// (#120, "the human operator rather than an agent") and are untouched.

/** Parse `#rrggbb` into `[r, g, b]` 0-255 channels. */
export function accentRgb(hexColor: string): number[] {
  return [1, 3, 5].map((index) => Number.parseInt(hexColor.slice(index, index + 2), 16));
}

function accentHex(values: number[]): string {
  return `#${values.map((value) => value.toString(16).padStart(2, "0")).join("")}`;
}

// #111: the wrap is on HUE. Each rotated result returns to the raw accent
// luminance band; 37 is not a divisor of 360, so repeated application walks
// the wheel instead of landing on the base hue.
const ACCENT_WRAP_HUE_STEP_DEGREES = 37;
const RAW_ACCENT_LUMINANCE = 0.202;
const LIGHT_IDENTITY_LUMINANCE = 0.080;
const DARK_IDENTITY_LUMINANCE = 0.400;
const ACCENT_WRAP_MAX_ATTEMPTS = 360;

function accentRgbToHsl(hexColor: string): [number, number, number] {
  const [r, g, b] = accentRgb(hexColor).map((channel) => channel / 255) as [number, number, number];
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const lightness = (max + min) / 2;
  if (max === min) return [0, 0, lightness];
  const delta = max - min;
  const saturation = lightness > 0.5 ? delta / (2 - max - min) : delta / (max + min);
  let hue: number;
  if (max === r) hue = ((g - b) / delta + (g < b ? 6 : 0)) / 6;
  else if (max === g) hue = ((b - r) / delta + 2) / 6;
  else hue = ((r - g) / delta + 4) / 6;
  return [hue * 360, saturation, lightness];
}

function accentHslToHex(hue: number, saturation: number, lightness: number): string {
  const h = (((hue % 360) + 360) % 360) / 360;
  if (saturation === 0) {
    const value = Math.round(lightness * 255);
    return accentHex([value, value, value]);
  }
  const q = lightness < 0.5 ? lightness * (1 + saturation) : lightness + saturation - lightness * saturation;
  const p = 2 * lightness - q;
  const channel = (offset: number): number => {
    let t = h + offset;
    if (t < 0) t += 1;
    if (t > 1) t -= 1;
    if (t < 1 / 6) return p + (q - p) * 6 * t;
    if (t < 1 / 2) return q;
    if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
    return p;
  };
  return accentHex([channel(1 / 3), channel(0), channel(-1 / 3)].map((value) => Math.round(value * 255)));
}

/** Rotate a color's hue before the allocator restores relative luminance. */
export function accentRotateHue(hexColor: string, degrees: number): string {
  const [hue, saturation, lightness] = accentRgbToHsl(hexColor);
  return accentHslToHex(hue + degrees, saturation, lightness);
}

/** WCAG relative luminance for one `#rrggbb` color. */
export function colorRelativeLuminance(hexColor: string): number {
  const [r = 0, g = 0, b = 0] = accentRgb(hexColor).map((channel) => {
    const value = channel / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function colorWithRelativeLuminance(hexColor: string, target: number): string {
  const [hue, saturation] = accentRgbToHsl(hexColor);
  let low = 0;
  let high = 1;
  for (let iteration = 0; iteration < 32; iteration += 1) {
    const middle = (low + high) / 2;
    if (colorRelativeLuminance(accentHslToHex(hue, saturation, middle)) < target) low = middle;
    else high = middle;
  }
  const darker = accentHslToHex(hue, saturation, low);
  const lighter = accentHslToHex(hue, saturation, high);
  return Math.abs(colorRelativeLuminance(darker) - target) <=
    Math.abs(colorRelativeLuminance(lighter) - target)
    ? darker
    : lighter;
}

export type IdentityForegroundMode = "light" | "dark";

/** Derive a readable display value from one stable raw identity accent. */
export function readableIdentityForeground(
  hexColor: string,
  mode: IdentityForegroundMode,
): string {
  return colorWithRelativeLuminance(
    hexColor,
    mode === "light" ? LIGHT_IDENTITY_LUMINANCE : DARK_IDENTITY_LUMINANCE,
  );
}

/** Resolve the current Pi Light/Dark member from its actual card background. */
export function identityForegroundMode(
  theme: Pick<CardTheme, "getBgAnsi">,
): IdentityForegroundMode {
  const ansi = typeof theme.getBgAnsi === "function"
    ? String(theme.getBgAnsi("customMessageBg"))
    : "";
  const match = /\x1b\[48;2;(\d+);(\d+);(\d+)m/.exec(ansi);
  if (!match) {
    throw new Error("Pi theme did not expose a truecolor custom-message background");
  }
  const hexColor = `#${match.slice(1, 4)
    .map((channel) => Number(channel).toString(16).padStart(2, "0"))
    .join("")}`;
  return colorRelativeLuminance(hexColor) >= 0.5 ? "light" : "dark";
}

/** Display one raw roster accent against the current Pi mode surfaces. */
export function organizationPersonDisplayAccent(
  theme: Pick<CardTheme, "getBgAnsi">,
  rawAccent: string,
): string {
  return readableIdentityForeground(rawAccent, identityForegroundMode(theme));
}

/**
 * Every person's identity accent in roster order — differentiates past the
 * palette by hue rotation, and THROWS rather than ever returning a duplicate
 * (#111: a silent wrap is a misattribution engine, not a cosmetic limit).
 */
/**
 * #485: the identity-stable ordering the accent allocator must be fed — the
 * intercom's copy of `src/foundation/theme.ts`'s `identityAccentOrder`. Order by
 * `createdAt` (persisted once at registration, never touched by
 * `refreshPeopleOrder`'s department re-sort), `id` tiebreak, so a person's
 * `@name` mention colour stays byte-identical to their pane border and does not
 * rotate when the roster grows. (Duplicated here for the same reason
 * `organizationPersonAccents` is — the extension boundary; the durable fix is a
 * single persisted per-identity accent that retires both copies.)
 */
export function identityAccentOrder(people: Record<string, { createdAt: string }>): string[] {
  return Object.keys(people).sort((left, right) => {
    const byCreated = String(people[left]?.createdAt ?? "").localeCompare(String(people[right]?.createdAt ?? ""));
    return byCreated !== 0 ? byCreated : left.localeCompare(right);
  });
}

export function organizationPersonAccents(peopleOrder: readonly string[]): string[] {
  const allocated: string[] = [];
  const taken = new Set<string>();
  for (const [index] of peopleOrder.entries()) {
    const base = ORGANIZATION_PERSON_ACCENTS[index % ORGANIZATION_PERSON_ACCENTS.length]!;
    const cycle = Math.floor(index / ORGANIZATION_PERSON_ACCENTS.length);
    let candidate = cycle === 0
      ? base
      : colorWithRelativeLuminance(
        accentRotateHue(base, cycle * ACCENT_WRAP_HUE_STEP_DEGREES),
        RAW_ACCENT_LUMINANCE,
      );
    for (let attempt = 1; taken.has(candidate) && attempt <= ACCENT_WRAP_MAX_ATTEMPTS; attempt += 1) {
      candidate = colorWithRelativeLuminance(
        accentRotateHue(base, cycle * ACCENT_WRAP_HUE_STEP_DEGREES + attempt),
        RAW_ACCENT_LUMINANCE,
      );
    }
    if (taken.has(candidate)) {
      throw new Error(
        `Cannot allocate a distinct organization accent for roster position ${index} ` +
          `('${peopleOrder[index]}'): the palette and its hue rotations are exhausted. ` +
          `Refusing to hand two people the same identity color.`,
      );
    }
    taken.add(candidate);
    allocated.push(candidate);
  }
  return allocated;
}

// --- Human-readable references -------------------------------------------

/** id-prefix families known to appear in launcher/tool text today. Callers
 * needing a different set (e.g. a card that only ever sees `task-` ids) may
 * override via `options.prefixes`. */
const DEFAULT_HUMAN_REF_PREFIXES = ["goal", "transition", "health", "task", "assignment"] as const;

export interface HumanRefOptions {
  /** Collapsed-line character budget. Defaults to 140, matching the
   * pre-existing `boundedSystemNoticeText` default. */
  maximum?: number;
  /** id-prefix families to scrub, e.g. `["goal", "transition"]`. */
  prefixes?: readonly string[];
  /** Text substituted for a matched raw id. Defaults to "the affected work". */
  replacement?: string;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Strips control characters, replaces `<prefix>-<opaque-id>` tokens with a
 * human-neutral phrase, and drops bare long hex hashes — without truncating
 * or checking for emptiness. Exposed separately from {@link humanRef} so a
 * caller that needs an extra prefix-strip in between (like
 * `boundedSystemNoticeText`'s assignment-header stripping in
 * `organization-intercom.ts`) can still reuse this half.
 */
export function scrubHumanRef(value: string, options: Pick<HumanRefOptions, "prefixes" | "replacement"> = {}): string {
  const prefixes = options.prefixes ?? DEFAULT_HUMAN_REF_PREFIXES;
  const replacement = options.replacement ?? "the affected work";
  const idPattern = new RegExp(`\\b(?:${prefixes.map(escapeRegExp).join("|")})-[a-z0-9][a-z0-9.:_-]{7,}\\b`, "gi");
  return value
    .replace(/[\u0000-\u001f\u007f]/g, " ")
    .replace(idPattern, replacement)
    .replace(/\b[a-f0-9]{20,}\b/gi, "")
    .replace(/\s+/g, " ")
    .trim();
}

/**
 * Finishes a scrubbed string: returns `undefined` when nothing legible
 * survived, otherwise truncates to `maximum` characters with an ellipsis.
 */
export function finalizeHumanRef(text: string, maximum = 140): string | undefined {
  if (!/[\p{L}\p{N}]/u.test(text)) return undefined;
  const truncated = text.length > maximum;
  return `${text.slice(0, maximum)}${truncated ? "…" : ""}`;
}

/**
 * The one-call id→human-name scrubber: raw ids collapse to a neutral phrase,
 * control chars and bare hashes are stripped, and the result is bounded to a
 * collapsed-line length. Generalizes `organization-intercom.ts`'s original
 * `boundedSystemNoticeText` (goal-/transition-/health- only) to any id-prefix
 * family a card might see (task, assignment, health, …).
 */
export function humanRef(value: unknown, options: HumanRefOptions = {}): string | undefined {
  if (typeof value !== "string") return undefined;
  return finalizeHumanRef(scrubHumanRef(value, options), options.maximum ?? 140);
}

// ===========================================================================
// Unified card renderer
// ===========================================================================
//
// Before this, every one of the product's ~76 card sites hand-rolled its own
// layout: each concatenated `theme.fg(...)` strings, invented its own body
// shape, and spelled the expand hint its own way. The #354 audit built the
// house-style *primitives* above but nothing made the shared path the *only*
// path, so each site kept re-implementing it. `renderCard` closes that gap: it
// is the single entry point that turns a declarative {@link CardSpec} into the
// `Box | Text` node Pi renders. "Render a card" and "render it correctly"
// become the same call.
//
// Two structural guarantees the type system enforces:
//   1. {@link CardBody} is a REQUIRED, discriminated field on {@link CardSpec}
//      — there is no "absent body" state to forget (the #103 regression, made
//      unrepeatable). A `"list"` body carries its items, so the renderer
//      collapses it correctly by construction: a list can never be truncated
//      to one item and an ellipsis, because the renderer never sees it as a
//      blob of prose.
//   2. {@link CardKind} is a CLOSED union and {@link CARD_KIND_INFO} is a TOTAL
//      `Record<CardKind, …>`. Adding a kind without registering it is a compile
//      error — there is no runtime default branch for a new card to fall into
//      silently (the silent body-drop that hid a working producer for weeks).

/**
 * The closed set of every distinct card the product renders. Kept as plain
 * string labels (no domain coupling) so this module stays a standalone
 * deployment unit. {@link CARD_KIND_INFO} is a total record over this union, so
 * a new member here is a compile error until it is registered — the mechanical
 * replacement for "remember to wire up the new card".
 */
export type CardKind =
  // Generic org-tool card families (organization-intercom.ts tool hooks).
  | "tool-call"
  | "tool-success"
  | "tool-failure"
  // Boxed intercom / launcher message cards (customMessageBg box).
  | "intercom-message"
  | "intercom-assignment"
  | "system-notice"
  | "session-maintenance"
  | "work-resumed"
  | "first-boot"
  | "pane-failure"
  // team-ui cards.
  | "live-schedules";

/** Static facts about a card kind, independent of any single render. */
export interface CardKindInfo {
  /** `true` = wrapped in a `customMessageBg` box; `false` = a plain `Text`. */
  readonly boxed: boolean;
  /** Human-readable one-liner for the kind, used by docs/telemetry/tests. */
  readonly label: string;
}

/**
 * The single source of truth for what card kinds exist. TOTAL by construction:
 * `Record<CardKind, …>` makes the compiler reject any {@link CardKind} added
 * without a matching entry here — so a card can never be introduced with no
 * registered home. Golden-test coverage keys off this record.
 */
export const CARD_KIND_INFO: Record<CardKind, CardKindInfo> = {
  "tool-call": { boxed: false, label: "org tool in-progress line" },
  "tool-success": { boxed: false, label: "org tool success card" },
  "tool-failure": { boxed: false, label: "org tool failure/retry card" },
  "intercom-message": { boxed: true, label: "peer intercom message" },
  "intercom-assignment": { boxed: true, label: "new-goal assignment envelope" },
  "system-notice": { boxed: true, label: "ChiefD-authored system notice" },
  "session-maintenance": { boxed: true, label: "session-maintenance card" },
  "work-resumed": { boxed: true, label: "work-resumed card" },
  "first-boot": { boxed: true, label: "first-boot welcome card" },
  "pane-failure": { boxed: true, label: "pane bootstrap failure card" },
  "live-schedules": { boxed: true, label: "live-schedules snapshot card" },
};

/** Every declared {@link CardKind}, in declaration order. */
export const CARD_KINDS = Object.keys(CARD_KIND_INFO) as CardKind[];

/**
 * The card body — a REQUIRED, discriminated field. There is deliberately no
 * "absent" variant: omitting it is a compile error, not an empty card.
 *
 * - `"none"`: the card is a headline (+ optional dim detail) with nothing
 *   expandable behind it — so it never shows an expand hint over empty space.
 * - `"prose"`: free text with a gist-carrying opening. Collapses per
 *   {@link ProseBody.collapse}: `"preview"` shows a bounded first slice + `…`;
 *   `"hidden"` shows only the expand hint (the full text appears only when
 *   expanded). Expands to the full text.
 * - `"list"`: an enumeration. ALWAYS renders in full, even collapsed — 96
 *   characters of a list is one item and an ellipsis, which delivers the
 *   notification while withholding the information (#103).
 */
export type CardBody =
  | { readonly kind: "none" }
  | ProseBody
  | { readonly kind: "list"; readonly items: readonly string[] }
  | { readonly kind: "lines"; readonly lines: readonly CardLine[] };

/**
 * One pre-structured body line for a `"lines"` body — the escape hatch for the
 * bespoke boxed cards (launcher system notices, the person-message 🎯/🗓 block)
 * whose middle content mixes tokens and whose collapse micro-rules the card
 * owns. The site describes each line declaratively — a `token` NAME plus an
 * optional uncolored `raw` suffix (e.g. a trailing `…` that must sit OUTSIDE
 * the color span) — so it never constructs a color itself, and {@link renderCard}
 * still owns the box, the title, and the hint spelling. An empty `text` with no
 * token is a blank spacer line.
 */
/** One inline title decoration — see {@link CardSpec.titleTags}. */
export interface CardTag {
  readonly text: string;
  readonly token: string;
  /** Separator emitted before the tag; defaults to a single space. */
  readonly sep?: string;
}

export interface CardLine {
  readonly text: string;
  /** `theme.fg` token; omit to emit `text` verbatim (no color). */
  readonly token?: string;
  /** Wrap `text` in `theme.bold` (before any `token`) — a bold section header
   * such as the work-resumed "🎯 3 open goals" line. */
  readonly bold?: boolean;
  /** Prepended BEFORE the colored span, uncolored — e.g. a leading `👤 ` glyph. */
  readonly prefix?: string;
  /** Appended AFTER the colored span, uncolored — e.g. a trailing `…`, or an
   * identity-colored sender segment. */
  readonly raw?: string;
  /** `"per-line"` re-wraps each newline in the token span; see {@link ProseBody.wrap}. */
  readonly wrap?: "whole" | "per-line";
}

export interface ProseBody {
  readonly kind: "prose";
  readonly text: string;
  /** Collapsed presentation. Defaults to `"preview"`. */
  readonly collapse?: "preview" | "hidden";
  /** Preview slice length when `collapse: "preview"`. Defaults to 96. */
  readonly previewChars?: number;
  /**
   * How a multi-line expanded body is colored:
   * - `"whole"` (default): the entire text is wrapped in one `bodyToken` span.
   * - `"per-line"`: each line is wrapped in its own span. Required when the
   *   body must stay colored across newlines (Pi's `Text` does not propagate an
   *   ANSI color past a line break), e.g. a multi-line dim reason block.
   * Only affects `collapse: "hidden"`/expanded rendering; a preview is a single
   * line either way.
   */
  readonly wrap?: "whole" | "per-line";
}

/**
 * The declarative description of one card. Every field a renderer needs to lay
 * the card out lives here; {@link renderCard} owns turning it into pixels.
 */
export interface CardSpec {
  /** Which card this is — closed union, drives coverage + telemetry. */
  readonly kind: CardKind;
  /** Fixed-state vocabulary entry OR an explicit `{emoji, token}` (domain glyph). */
  readonly icon: CardState | CardIcon;
  readonly title: string;
  /**
   * How the title line is styled:
   * - `"state"` (default): `theme.fg(<icon token>, "<emoji> <title>")` — the
   *   status-hued tool-card header ({@link cardTitle}).
   * - `"bold"`: `<emoji> <bold title>` — the boxed launcher/intercom message
   *   header ({@link cardBoldTitle}), where the emoji stays unstyled.
   * - `"accent-bold"`: `theme.fg(accent, theme.bold("<emoji> <title>"))` — the
   *   accent-colored bold header ({@link cardAccentBoldTitle}), emoji inside the
   *   span (launcher system notices, sender-labelled cards).
   * Ignored when `inProgress` is set (the whole line renders dim).
   */
  readonly titleStyle?: "state" | "bold" | "accent-bold";
  /** Theme token used by `titleStyle: "accent-bold"` and by {@link sender}.
   * Defaults to `"accent"`. */
  readonly accentToken?: string;
  /**
   * A sender label appended to the title line as ` <dim from> <name>`:
   * - `{ from, name }` colors the name as accent-bold ({@link accentToken}) —
   *   the sender-labelled idiom ("📥 Inbox from Ari").
   * - `{ from, rendered }` uses a pre-colored name segment verbatim — for the
   *   person-message header whose `@sender` carries the sender's own IDENTITY
   *   accent (a roster-hex color card-style cannot own).
   */
  readonly sender?:
    | { readonly from: string; readonly name: string }
    | { readonly from: string; readonly rendered: string };
  /**
   * Inline decorations appended to the title line after the target/sender, each
   * as `<sep><theme.fg(token, text)>` (sep defaults to a single space) — e.g. a
   * tool-failure card's `(system fault)` / `(ref …)` / `· <summary>` tags and its
   * inline expand hint, or a goal card's dim priority suffix. The site supplies a
   * token NAME, never a color, so AC1 holds.
   */
  readonly titleTags?: readonly CardTag[];
  /** ` · <target>` segment; rendered dim. */
  readonly target?: string;
  /** Dim detail line(s) below the title — what happened / what happens next. */
  readonly detail?: string | readonly string[];
  /** REQUIRED. No `?` — a card with no body is a compile error, not a blank. */
  readonly body: CardBody;
  /** Box (`customMessageBg`) vs plain `Text`. */
  readonly boxed: boolean;
  /** `@name` colorizer for the target segment (identity accents, #433). */
  readonly mentions?: MentionColorizer;
  /**
   * In-progress (`renderCall`) variant: the WHOLE title line renders dim, since
   * nothing has concluded yet. `icon` supplies the tool's own domain glyph.
   */
  readonly inProgress?: boolean;
  /**
   * Theme token the body text renders in. Boxed cards use `"customMessageText"`;
   * a tool card's expanded raw output uses `undefined` (emitted verbatim, no
   * color wrap). Ignored for a `"none"` body.
   */
  readonly bodyToken?: string;
  /** Token for a collapsed prose preview when expanded output intentionally
   * stays raw (diagnostics/record text): defaults to {@link bodyToken}. */
  readonly collapsedBodyToken?: string;
  /**
   * Trailing themed lines shown in BOTH collapsed and expanded states, after
   * the body — e.g. a failure card's error line or a success card's warning.
   * Each renders as `theme.fg(token, text)` on its own line.
   */
  readonly footer?: readonly { readonly text: string; readonly token: string }[];
  /**
   * Metadata that is deliberately hidden until Ctrl+O — e.g. a durable reply
   * handle. A collapsed card still gets exactly one canonical expand hint even
   * when its prose body was short enough to fit in full.
   */
  readonly expandedFooter?: readonly { readonly text: string; readonly token: string }[];
}

export interface RenderCardOptions {
  /** Whether the pane is showing this card expanded (Ctrl+O). */
  readonly expanded?: boolean;
}

const DEFAULT_PREVIEW_CHARS = 96;

/**
 * Collapse prose to a bounded single-line preview: whitespace normalized to
 * single spaces (so an enumeration or multi-line blob folds to one line), then
 * sliced to `maximum`. `truncated` says whether anything was dropped, so the
 * caller can decide to append `…` and an expand hint. Mirrors the long-standing
 * `compactPresentation` behaviour the card sites used before unification.
 */
export function previewText(text: string, maximum = DEFAULT_PREVIEW_CHARS): { text: string; truncated: boolean } {
  const normalized = text.replace(/\s+/g, " ").trim();
  return { text: normalized.slice(0, maximum), truncated: normalized.length > maximum };
}

/** Wrap body text in its token, or emit it verbatim when no token is given.
 * With `wrap: "per-line"` each line is wrapped in its own span so the color
 * survives newlines (Pi's `Text` does not propagate an ANSI color past a line
 * break). */
function paintBody(theme: CardTheme, token: string | undefined, text: string, wrap: "whole" | "per-line" = "whole"): string {
  if (!token) return text;
  if (wrap === "whole") return theme.fg(token, text);
  return text.split("\n").map((line) => theme.fg(token, line)).join("\n");
}

/**
 * The sanctioned public body-text emitter: `theme.fg(token, text)`, honoring
 * `wrap` (`"per-line"` re-wraps each line so the color survives newlines). This
 * is how a card site colors body text it hand-composes outside a full
 * {@link CardSpec} — e.g. when a card needs an `accent-bold` title with a dim
 * target segment that {@link renderCard} does not lay out for that style — so it
 * never calls `theme.fg` directly (AC1). Same coloring `renderCard` applies to a
 * body internally.
 */
export function cardBody(theme: CardTheme, token: string, text: string, wrap: "whole" | "per-line" = "whole"): string {
  return paintBody(theme, token, text, wrap);
}

/**
 * The uniform per-tool FAILURE result body (#150): `⚠️ <title>` in the failure
 * hue (dim `· <target>` when given), then the raw tool output — verbatim after a
 * blank line when expanded, else a dim `· <preview>…` line with the expand hint
 * when truncated. Byte-identical to the idiom every intercom tool hook used to
 * hand-roll; the ONE home for it, so no site carries its own `theme.fg`/hint.
 */
export function toolFailureText(
  theme: CardTheme,
  title: string,
  message: string,
  expanded: boolean,
  options: { previewChars?: number; target?: string } = {},
): string {
  let text = cardTitle(theme, "failure", title, options.target);
  if (expanded && message) return `${text}\n\n${message}`;
  const summary = previewText(message, options.previewChars ?? 120);
  if (summary.text) text += `\n${theme.fg("dim", `· ${summary.text}${summary.truncated ? "…" : ""}`)}${summary.truncated ? `  ${cardHint(theme)}` : ""}`;
  return text;
}

/** The card's text content as a single string — the box/plain wrapper is
 * applied by {@link renderCard}. Exposed so unit tests (and any caller that
 * only needs the string) can assert the exact assembly without a pi-tui node. */
export function renderCardText(theme: CardTheme, spec: CardSpec, options: RenderCardOptions = {}): string {
  const expanded = options.expanded === true;
  const lines: string[] = [];
  let hasExpandHint = false;

  const accentToken = spec.accentToken ?? "accent";
  const emoji = resolveCardIcon(spec.icon).emoji;
  let titleLine: string;
  if (spec.inProgress) {
    titleLine = cardCallLine(theme, { emoji, title: spec.title, target: spec.target }, spec.mentions);
  } else if (spec.titleStyle === "accent-bold") {
    titleLine = cardAccentBoldTitle(theme, emoji, spec.title, accentToken);
  } else if (spec.titleStyle === "bold") {
    titleLine = cardBoldTitle(theme, emoji, spec.title, spec.target, spec.mentions);
  } else {
    titleLine = cardTitle(theme, spec.icon, spec.title, spec.target, spec.mentions);
  }
  if (spec.sender) {
    // Evaluate the dim `from` span before the name so the observable
    // theme.fg call order stays left-to-right (matches the pre-migration
    // template-literal order).
    const from = theme.fg("dim", spec.sender.from);
    const name = "rendered" in spec.sender
      ? spec.sender.rendered
      : cardAccentBoldTitle(theme, "", spec.sender.name, accentToken);
    titleLine += ` ${from} ${name}`;
  }
  if (spec.titleTags) {
    for (const tag of spec.titleTags) titleLine += `${tag.sep ?? " "}${theme.fg(tag.token, tag.text)}`;
  }
  lines.push(titleLine);

  if (spec.detail !== undefined) {
    const details = Array.isArray(spec.detail) ? spec.detail : [spec.detail];
    for (const line of details) lines.push(cardDetail(theme, line));
  }

  const body = spec.body;
  if (body.kind === "lines") {
    // Fully site-structured mixed-token body: each line carries its own token
    // and optional uncolored suffix; the site owns any collapse logic.
    for (const line of body.lines) {
      const core = line.bold ? theme.bold(line.text) : line.text;
      lines.push(`${line.prefix ?? ""}${paintBody(theme, line.token, core, line.wrap ?? "whole")}${line.raw ?? ""}`);
    }
  } else if (body.kind === "list") {
    // A list ALWAYS renders every item in full — never collapsed to a preview.
    for (const item of body.items) lines.push(paintBody(theme, spec.bodyToken, item));
  } else if (body.kind === "prose") {
    if (expanded) {
      // Expanded body is set off by a blank line, matching the pre-unification
      // `\n\n<body>` idiom every boxed card used.
      lines.push("");
      lines.push(paintBody(theme, spec.bodyToken, body.text, body.wrap ?? "whole"));
    } else if ((body.collapse ?? "preview") === "hidden") {
      // Collapsed: reveal nothing but the affordance to expand.
      lines.push(cardHint(theme));
      hasExpandHint = true;
    } else {
      const preview = previewText(body.text, body.previewChars ?? DEFAULT_PREVIEW_CHARS);
      const line = `${paintBody(theme, spec.collapsedBodyToken ?? spec.bodyToken, preview.text)}${preview.truncated ? "…" : ""}`;
      lines.push(preview.truncated ? `${line}  ${cardHint(theme)}` : line);
      hasExpandHint = preview.truncated;
    }
  }

  if (spec.footer) {
    for (const line of spec.footer) lines.push(theme.fg(line.token, line.text));
  }
  if (spec.expandedFooter) {
    if (expanded) {
      for (const line of spec.expandedFooter) lines.push(theme.fg(line.token, line.text));
    } else if (!hasExpandHint) {
      lines.push(cardHint(theme));
    }
  }

  return lines.join("\n");
}

/**
 * THE single card entry point. Turns a declarative {@link CardSpec} into the
 * `Box | Text` node Pi renders — owning box-vs-plain, the title/call line, dim
 * detail, body collapse, and the one expand hint. Every card site calls this;
 * nothing beneath it re-implements card layout.
 *
 * A boxed card requires the theme to expose `bg` (the real Pi theme always
 * does); if it is absent the card degrades to a plain `Text` rather than
 * throwing, so a bare fixture theme still renders.
 */
export function renderCard(theme: CardTheme, spec: CardSpec, options: RenderCardOptions = {}): Box | Text {
  const node = new Text(renderCardText(theme, spec, options), 0, 0);
  if (!spec.boxed || typeof theme.bg !== "function") return node;
  const bg = theme.bg.bind(theme);
  const box = new Box(1, 1, (line: string) => bg("customMessageBg", line));
  box.addChild(node);
  return box;
}

/**
 * Render a homogeneous batch of declarative cards in one Pi node. Transport
 * batches remain a single custom message/acknowledgement unit, but
 * every visible block still travels through the exact same CardSpec renderer
 * as an individual card. A group must not mix boxed and plain cards: that
 * would conceal a layout decision inside the wrapper rather than its specs.
 */
export function renderCardGroup(theme: CardTheme, specs: readonly CardSpec[], options: RenderCardOptions = {}): Box | Text {
  if (!specs.length) return new Text("", 0, 0);
  const boxed = specs[0]!.boxed;
  if (specs.some((spec) => spec.boxed !== boxed)) throw new Error("A card group must not mix boxed and plain cards");
  const node = new Text(specs.map((spec) => renderCardText(theme, spec, options)).join("\n\n"), 0, 0);
  if (!boxed || typeof theme.bg !== "function") return node;
  const bg = theme.bg.bind(theme);
  const box = new Box(1, 1, (line: string) => bg("customMessageBg", line));
  box.addChild(node);
  return box;
}
