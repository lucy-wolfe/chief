/**
 * How full a hosted person's context window is, answered from facts this host
 * already holds.
 *
 * # Why this number has to exist
 *
 * A tmux pane compacts itself. Pi runs a context threshold over its own message
 * list and, when the conversation outgrows the window, summarises the history
 * and carries on — which is why a CEO in a pane can stay up for weeks.
 * `AgentHarness` has no such threshold: it exposes `compact()` and never calls
 * it. So a web-hosted person had no ceiling and no relief, and the first long
 * life was going to end with a provider refusing the request.
 *
 * `ExtensionContext.getContextUsage()` is also the reading the intercom uses to
 * decide whether a park is worth a compaction, and its own guard is
 * `typeof ... !== "function"`. A host that omits the member is a host where that
 * decision is never made; a host that invents a number is worse.
 *
 * # Why it is not a second opinion
 *
 * Pi's `AgentSession.getContextUsage()` is three facts and no judgement: the
 * live model's `contextWindow`, the branch's newest compaction, and
 * `estimateContextTokens` over the message list. Every one of those is
 * reachable here — the harness knows its model, and this host holds the very
 * `Session` the harness appends to — and the arithmetic is `pi-agent-core`'s
 * own exported functions rather than a copy of them. What is transcribed is the
 * RULE, in the same order and with the same outcomes, because the rule is the
 * part that must not drift.
 *
 * The post-compaction rule is the half that looks optional and is not. The
 * newest assistant `usage` in a freshly compacted branch describes the context
 * BEFORE the compaction, so trusting it would report a full window one entry
 * after a successful compaction — and a host compacting on that reading would
 * compact again immediately, and again, forever. `tokens: null` is the honest
 * answer until the next provider response, and every reader already handles it.
 */
import type { AgentMessage, SessionTreeEntry } from '@earendil-works/pi-agent-core'
import { calculateContextTokens, estimateContextTokens } from '@earendil-works/pi-agent-core'

import type { HostedContextUsage } from '@/types/ContextUsage'
import { isNullish } from '@/utils/Nullish'

/** The newest compaction on this branch, or `undefined` when there is none.
 *
 * The same backwards scan Pi's `getLatestCompactionEntry` performs: the LAST
 * compaction is the only one that bounds what the usage numbers describe. */
function latestCompactionIndex(entries: readonly SessionTreeEntry[]): number {
  for (let index = entries.length - 1; index >= 0; index -= 1) {
    if (entries[index]?.type === 'compaction') return index
  }
  return -1
}

/** Whether an assistant answered AFTER the compaction and reported usage.
 *
 * An aborted or errored assistant message is skipped because its usage
 * describes a request that did not finish, and a zero total means the provider
 * reported nothing at all. Either one would be a token count about a turn that
 * never happened. */
function hasUsageAfter(entries: readonly SessionTreeEntry[], compactionIndex: number): boolean {
  for (let index = entries.length - 1; index > compactionIndex; index -= 1) {
    const entry = entries[index]
    if (entry?.type !== 'message') continue
    const message: AgentMessage = entry.message
    if (message.role !== 'assistant') continue
    if (message.stopReason === 'aborted' || message.stopReason === 'error') continue
    if (calculateContextTokens(message.usage) > 0) return true
  }
  return false
}

/**
 * The reading, or `undefined` when the model publishes no usable window.
 *
 * `undefined` is Pi's own answer for a model with no `contextWindow`, and it is
 * the honest one: a percentage of an unknown window is not a smaller fact, it
 * is a made-up one.
 */
export function contextUsage(
  contextWindow: number | undefined,
  branch: readonly SessionTreeEntry[],
  messages: readonly AgentMessage[]
): HostedContextUsage | undefined {
  if (isNullish(contextWindow) || contextWindow <= 0) return undefined
  const compactionIndex = latestCompactionIndex(branch)
  if (compactionIndex >= 0 && !hasUsageAfter(branch, compactionIndex)) {
    return { tokens: null, contextWindow, percent: null }
  }
  const estimate = estimateContextTokens([...messages])
  return {
    tokens: estimate.tokens,
    contextWindow,
    percent: (estimate.tokens / contextWindow) * 100
  }
}
