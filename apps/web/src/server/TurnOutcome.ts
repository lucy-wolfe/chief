/**
 * Reading what a turn produced, and what went wrong when nothing did.
 *
 * # Why these two are shared and the MESSAGES are not
 *
 * Two callers run turns against a harness — `PersonTalk` for a company's
 * people and `FounderTalk` for the one agent that predates a company — and
 * both must answer the same two questions: which of the assistant's content is
 * readable text, and, when the turn failed, which KIND of failure it was.
 *
 * The classification is shared because it is a fact about the PROVIDER's
 * words, identical whoever asked, and because getting it wrong is expensive in
 * a specific way: a rejected credential once surfaced as
 * `502 turn-failed: "Connection error."`, which sent a reader to check the
 * network on a host whose egress was answering 200 in 100ms.
 *
 * The MESSAGES are deliberately not shared. A person's credential comes from
 * the pi-home chiefd materialized for them; the Founder's comes from the
 * operator's own registry. One sentence covering both would have to be vague
 * about the only thing an operator needs to know — which file to open — so
 * each caller writes its own and this module decides only which of the four
 * cases it is writing about.
 */
import type { TurnFailure } from '@/types/TurnOutcome'
import { isNullish } from '@/utils/Nullish'

/**
 * Classify a provider's own failure text.
 *
 * Matched on TEXT because that is all a failed `AssistantMessage` carries —
 * there is no status code on one. Each pattern is a phrase providers actually
 * send; anything unmatched falls through to `transport`, which is the safe
 * direction: it names a cause an operator can check rather than asserting one.
 */
export function turnFailure(reason: string): TurnFailure {
  const said = reason.toLowerCase()
  if (
    said.includes('401') ||
    said.includes('403') ||
    said.includes('unauthorized') ||
    said.includes('authentication') ||
    said.includes('invalid api key') ||
    said.includes('incorrect api key') ||
    said.includes('no auth credentials')
  ) {
    return 'credential-rejected'
  }
  if (said.includes('429') || said.includes('rate limit') || said.includes('quota')) {
    return 'rate-limited'
  }
  if (said.includes('400') || said.includes('invalid schema') || said.includes('invalid request')) {
    return 'rejected-request'
  }
  return 'transport'
}

/** One content part's readable text, or nothing.
 *
 * `Object.fromEntries(Object.entries(...))` narrows without an assertion:
 * `Object.entries` accepts an object and yields string keys, so the result is
 * a record by construction rather than by a claim about the input. */
function textOf(part: unknown): string | undefined {
  if (typeof part !== 'object' || isNullish(part)) return undefined
  const record: Record<string, unknown> = Object.fromEntries(Object.entries(part))
  return record.type === 'text' && typeof record.text === 'string' ? record.text : undefined
}

/** The assistant's words, without the parts a reader cannot read.
 *
 * Tool calls and thinking blocks are real content but they are not the reply;
 * concatenating them would put a tool's JSON in the transcript as if the agent
 * had said it. */
export function assistantText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return content
    .map(textOf)
    .filter((text): text is string => !isNullish(text))
    .join('')
}
