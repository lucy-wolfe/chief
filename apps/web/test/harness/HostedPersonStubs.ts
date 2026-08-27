/**
 * The two halves of a hosted person a lifecycle test has to stand in for: the
 * `AgentHarness` whose events drive the hooks, and the `Session` the handlers
 * read the transcript from.
 *
 * Both are Pi's concrete classes and neither can be constructed without a
 * provider route, so these are recorders rather than instances. They implement
 * exactly the members `server/ExtensionLifecycle` reaches — which is the point:
 * a stub that grew members the driver does not use would stop being evidence
 * about the driver.
 */
import type { AgentHarness, Session } from '@earendil-works/pi-agent-core'
import type {
  CompactionRecord,
  HarnessStub,
  HarnessStubOptions,
  QueuedMessage
} from '@test/types/HostedPersonStubs'

import type { LifecycleSubject, SessionReplacer } from '@/types/ExtensionLifecycle'

type OwnHandler = (event: unknown) => unknown
type Subscriber = (event: unknown) => unknown

/**
 * A harness that records instead of running a model.
 *
 * `subscribe` and `on` are the only two ways the driver reaches a harness, and
 * both hand back an unsubscribe function — held here so a test can assert that
 * shutdown really detaches rather than merely stopping to care.
 */
export function harnessStub(options: HarnessStubOptions = {}): HarnessStub {
  const delivered: QueuedMessage[] = []
  const subscribers = new Set<Subscriber>()
  const own = new Map<string, Set<OwnHandler>>()
  const compactions: CompactionRecord[] = []
  let aborted = 0
  let running = false
  let branchTokens = options.contextTokens ?? 0
  let compactionEntryId = 0

  // `AgentHarness`'s OWN contract, not a convenience: `steer` and `followUp`
  // reject with `AgentHarnessError` / `invalid_state` when the harness is
  // idle, and `prompt` refuses a harness that is busy. A stub that accepted
  // everything is exactly what let a reminder look delivered while the real
  // harness threw "Cannot follow up while idle" into a fire-and-forget void —
  // caught only by a live run. It is modelled here so it cannot be again.
  const liveQueue =
    (mode: 'steer' | 'followUp') =>
    (text: string): Promise<void> => {
      if (!running) {
        const refusal: Error & { code?: string } = new Error(
          `Cannot ${mode === 'steer' ? 'steer' : 'follow up'} while idle`
        )
        refusal.code = 'invalid_state'
        return Promise.reject(refusal)
      }
      delivered.push({ mode, text })
      return Promise.resolve()
    }

  const recorder = {
    subscribe(listener: Subscriber): () => void {
      subscribers.add(listener)
      return () => void subscribers.delete(listener)
    },
    on(type: string, handler: OwnHandler): () => void {
      const registered = own.get(type) ?? new Set<OwnHandler>()
      registered.add(handler)
      own.set(type, registered)
      return () => void registered.delete(handler)
    },
    steer: liveQueue('steer'),
    followUp: liveQueue('followUp'),
    prompt: (text: string): Promise<unknown> => {
      if (running) {
        const busy: Error & { code?: string } = new Error('AgentHarness is busy')
        busy.code = 'busy'
        return Promise.reject(busy)
      }
      running = true
      delivered.push({ mode: 'prompt', text })
      // A turn that starts and never settles would deadlock the driver's
      // delivery chain, so it settles: the assertion is that the wake ran a
      // turn, not what the model said.
      running = false
      return Promise.resolve({ role: 'assistant', content: [] })
    },
    abort: (): Promise<void> => {
      aborted += 1
      return Promise.resolve()
    },
    // `AgentHarness.compact()` refuses a harness that is not idle and refuses a
    // session with nothing in it, both by throwing. A stub that compacted
    // unconditionally would let a driver that compacts mid-turn look correct —
    // the same permissiveness that hid the idle-queue defect.
    compact: async (customInstructions?: string): Promise<unknown> => {
      if (running) {
        const busy: Error & { code?: string } = new Error('compact() requires idle harness')
        busy.code = 'busy'
        throw busy
      }
      if (branchTokens <= 0) {
        const empty: Error & { code?: string } = new Error('Nothing to compact')
        empty.code = 'compaction'
        throw empty
      }
      const tokensBefore = branchTokens
      // A real compaction is what makes the NEXT reading smaller. Modelled,
      // because a stub whose usage never falls would make an endless
      // compaction loop look like a passing test.
      branchTokens = options.tokensAfterCompaction ?? 0
      compactionEntryId += 1
      const entry = {
        type: 'compaction',
        id: `compaction-${compactionEntryId}`,
        parentId: null,
        summary: 'stub summary',
        firstKeptEntryId: 'kept',
        tokensBefore,
        // The harness's own field, written from `provided !== undefined` — no
        // hook supplied this one, so it is false, exactly as in production.
        fromHook: false
      }
      compactions.push({ customInstructions, tokensBefore })
      for (const handler of [...(own.get('session_compact') ?? [])]) {
        await handler({ type: 'session_compact', compactionEntry: entry, fromHook: false })
      }
      return { summary: entry.summary, firstKeptEntryId: 'kept', tokensBefore }
    },
    getModel: (): unknown => ({
      id: 'stub-model',
      provider: 'stub',
      contextWindow: options.contextWindow ?? 0
    }),
    setModel: (): Promise<void> => Promise.resolve(),
    setThinkingLevel: (): Promise<void> => Promise.resolve()
  }

  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // `AgentHarness` is a concrete class whose constructor needs a live provider
  // route. The driver reaches the members above; a structural implementation
  // of the whole class would be a second, always-stale copy of it.
  const harness = recorder as never as AgentHarness
  /* eslint-enable @typescript-eslint/consistent-type-assertions */

  return {
    harness,
    // The SAME conversation the harness is compacting, because in production
    // they are one object: `AgentHarness` appends to the `Session` this host
    // holds. A stub whose transcript did not shrink when its harness compacted
    // would make an endless compaction loop look like a passing test.
    session: liveSessionStub(
      () => branchTokens,
      () => compactionEntryId > 0
    ),
    delivered,
    compactions,
    contextTokens: () => branchTokens,
    abortCount: () => aborted,
    emit: async (event) => {
      if (event.type === 'agent_start' || event.type === 'turn_start') running = true
      if (event.type === 'agent_end' || event.type === 'turn_end') running = false
      for (const listener of [...subscribers]) await listener(event)
    },
    fire: async (type, event) => {
      const results: unknown[] = []
      for (const handler of [...(own.get(type) ?? [])]) {
        results.push(await handler({ ...event, type }))
      }
      return results
    },
    listenerCount: () => subscribers.size + [...own.values()].reduce((n, set) => n + set.size, 0)
  }
}

/** One assistant turn whose provider usage says the context is this big.
 *
 * A REAL `AssistantMessage`, not a number: `server/ContextUsage` runs
 * `pi-agent-core`'s own `estimateContextTokens` over it, so a fabricated total
 * would test the fabrication instead of the reading. */
function assistantTurn(tokens: number): Record<string, unknown> {
  return {
    role: 'assistant',
    content: [],
    api: 'anthropic-messages',
    provider: 'stub',
    model: 'stub-model',
    usage: { input: tokens, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: tokens },
    stopReason: 'stop',
    timestamp: 0
  }
}

/** A transcript that answers from live suppliers rather than a fixed array. */
function liveSessionStub(tokens: () => number, compacted: () => boolean): Session {
  const branch = (): readonly unknown[] => {
    const size = tokens()
    // A compaction entry with NO assistant turn behind it is the case Pi
    // answers `tokens: null` for, and the case a host that ignored it would
    // compact forever over.
    if (size <= 0) return compacted() ? [{ type: 'compaction', id: 'c', parentId: null }] : []
    return [
      ...(compacted() ? [{ type: 'compaction', id: 'c', parentId: null }] : []),
      { type: 'message', id: 'm', parentId: null, message: assistantTurn(size) }
    ]
  }
  const recorder = {
    getEntries: (): Promise<readonly unknown[]> => Promise.resolve(branch()),
    getBranch: (): Promise<readonly unknown[]> => Promise.resolve(branch()),
    buildContext: (): Promise<{ messages: readonly unknown[] }> =>
      Promise.resolve({ messages: tokens() <= 0 ? [] : [assistantTurn(tokens())] }),
    getLeafId: (): Promise<string | null> => Promise.resolve(null),
    getMetadata: (): Promise<{ id: string }> => Promise.resolve({ id: 'stub-session' })
  }
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // `Session` needs a `SessionStorage` and a real file; the driver reads the
  // five members above to answer Pi's synchronous `SessionManager` and its
  // context reading.
  return recorder as never as Session
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

/** A transcript the lifecycle can read without a file behind it. */
export function sessionStub(entries: readonly unknown[] = []): Session {
  const recorder = {
    getEntries: (): Promise<readonly unknown[]> => Promise.resolve(entries),
    getBranch: (): Promise<readonly unknown[]> => Promise.resolve(entries),
    buildContext: (): Promise<{ messages: readonly unknown[] }> =>
      Promise.resolve({ messages: [] }),
    getLeafId: (): Promise<string | null> => Promise.resolve(null),
    getMetadata: (): Promise<{ id: string }> => Promise.resolve({ id: 'stub-session' })
  }
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // `Session` needs a `SessionStorage` and a real file; the driver reads the
  // five members above to answer Pi's synchronous `SessionManager`.
  return recorder as never as Session
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

/** The whole subject `ExtensionToolSet.bind` takes.
 *
 * `replaceSession` REFUSES by default. A permissive replacement is the same
 * mistake as a permissive queue: it would let a driver that never actually
 * asks the host for anything pass. A test that wants one supplies it. */
export function subjectFor(
  stub: HarnessStub,
  cwd: string,
  replaceSession: SessionReplacer = () =>
    Promise.reject(new Error('this subject serves no session replacement'))
): LifecycleSubject {
  return { harness: stub.harness, session: stub.session, cwd, replaceSession }
}
