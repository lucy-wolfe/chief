/**
 * THE NUMBER THAT DECIDES WHETHER A HOSTED PERSON LIVES PAST ONE WINDOW.
 *
 * THE DEFECT THIS PINS: the web host published no `getContextUsage()`, so
 * nothing ever asked whether a hosted person's context was full. A tmux pane
 * runs Pi's own threshold and compacts itself; `AgentHarness` exposes
 * `compact()` and never calls it. The hosted CEO therefore had no ceiling and
 * no relief, and its first long life was going to end with a provider refusing
 * the request.
 *
 * Everything here is real: real `AssistantMessage` usage blocks, real branch
 * entries, and `pi-agent-core`'s own `estimateContextTokens` doing the
 * arithmetic inside the function under test. The only thing constructed is the
 * transcript, which is INPUT.
 */
import type { AgentMessage, SessionTreeEntry } from '@earendil-works/pi-agent-core'
import { describe, expect, it } from 'vitest'

import { contextUsage } from '@/server/ContextUsage'

/** One assistant turn whose provider usage reports this much context. */
function assistant(tokens: number, stopReason = 'stop'): AgentMessage {
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // `AssistantMessage` carries provider fields no reading here touches; what
  // matters is `role`, `usage` and `stopReason`, which is what is set.
  return {
    role: 'assistant',
    content: [],
    api: 'anthropic-messages',
    provider: 'stub',
    model: 'stub-model',
    usage: { input: tokens, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: tokens },
    stopReason,
    timestamp: 0
  } as unknown as AgentMessage
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

function messageEntry(id: string, message: AgentMessage): SessionTreeEntry {
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // The branch is Pi's own entry shape; the reading tests `type`, `message`
  // and position, which is what is set.
  return { type: 'message', id, parentId: null, timestamp: '', message } as SessionTreeEntry
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

function compactionEntry(id: string): SessionTreeEntry {
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // The same reason the two builders above give: `CompactionEntry` carries
  // summary fields the reading never looks at, and only `type` and position
  // decide anything here.
  return {
    type: 'compaction',
    id,
    parentId: null,
    timestamp: '',
    summary: '',
    firstKeptEntryId: '',
    tokensBefore: 0
  } as SessionTreeEntry
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

describe('how full a hosted person’s context window is', () => {
  it('reports tokens, the window and the percent', () => {
    const message = assistant(50_000)

    expect(contextUsage(200_000, [messageEntry('a', message)], [message])).toEqual({
      tokens: 50_000,
      contextWindow: 200_000,
      percent: 25
    })
  })

  it('answers nothing for a model that publishes no window', () => {
    const message = assistant(50_000)

    // Pi's own answer. A percentage of an unknown window is not a smaller
    // fact, it is an invented one — and a park compaction queued on an
    // invented number spends a summary nobody asked for.
    expect(contextUsage(undefined, [messageEntry('a', message)], [message])).toBeUndefined()
    expect(contextUsage(0, [messageEntry('a', message)], [message])).toBeUndefined()
  })

  it('answers UNKNOWN right after a compaction, never the pre-compaction size', () => {
    // The newest assistant usage on a freshly compacted branch describes the
    // context BEFORE the compaction. Reading it as the current size would
    // report a full window one entry after a successful compaction — and a
    // host compacting on that reading would compact again, and again, forever.
    const before = assistant(190_000)
    const branch = [messageEntry('a', before), compactionEntry('c')]

    expect(contextUsage(200_000, branch, [])).toEqual({
      tokens: null,
      contextWindow: 200_000,
      percent: null
    })
  })

  it('trusts the first assistant that answers after a compaction', () => {
    const before = assistant(190_000)
    const after = assistant(12_000)
    const branch = [messageEntry('a', before), compactionEntry('c'), messageEntry('b', after)]

    expect(contextUsage(200_000, branch, [after])).toEqual({
      tokens: 12_000,
      contextWindow: 200_000,
      percent: 6
    })
  })

  it('does not count an aborted or errored turn as an answer', () => {
    // Their usage describes a request that did not finish. Counting one would
    // unblock the reading with a number about a turn that never happened.
    const branch = (stopReason: string): SessionTreeEntry[] => [
      compactionEntry('c'),
      messageEntry('b', assistant(12_000, stopReason))
    ]

    expect(contextUsage(200_000, branch('aborted'), [])?.tokens).toBeNull()
    expect(contextUsage(200_000, branch('error'), [])?.tokens).toBeNull()
  })
})
