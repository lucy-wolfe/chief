/**
 * The driver for a hosted person's reactive lifecycle.
 *
 * # The gap this closes
 *
 * `server/ExtensionTools` gave a web-hosted person every tool and drove none
 * of the lifecycle. The extensions' `on(...)` registrations went into an empty
 * function, and the intercom was installed with `pollIntervalMs: 0`.
 *
 * That option is the part worth reading twice. Since #827 it does NOT name a
 * poll cadence — the fallback floor is deleted and there is no configurable
 * poll-only mode. `0` is a test/fixture seam whose whole meaning is
 * "construct NO `SseWatcher`". So the web host was not avoiding a timer; it
 * had switched off the only reactive channel a person has. The intercom
 * subscribes to `mailbox/<personId>`, `session-maintenance` and `supervision`
 * on chiefd's `GET /v1/docs/watch` and drains on a `doc-change` frame — the
 * same wake the tmux pane rides, over one multiplexed streaming `fetch()` per
 * `url|slug` for the whole process.
 *
 * chiefd needed nothing new. `mailbox_store_name` publishes a `WatchEvent` for
 * `mailbox/<personId>` when a mailbox row write commits, and
 * `dispatch::recipients_for` routes a fired reminder to the person it NAMES.
 * The wake was published the whole time; nobody here was listening, and even
 * had they been, `drainWithheldReason` would have answered
 * `delivery_not_ready` forever — because `deliveryReady` becomes true in
 * exactly two handlers, and neither was ever called.
 *
 * # One event surface, and it is the harness's own
 *
 * `AgentHarness` publishes `subscribe(listener)` over
 * `AgentEvent | AgentHarnessOwnEvent` and `on(type, handler)` for the
 * result-bearing own-events. The `AgentEvent` family is field-identical to
 * Pi's `ExtensionEvent` for the whole agent loop, so this module forwards the
 * event object it was given rather than rebuilding one — a re-shaped event is
 * a second opinion about what happened.
 *
 * Two hooks have no harness event because they are not the harness's to know:
 * a SESSION starting and a session shutting down are decisions of whoever
 * hosts the agent. Here that is `server/AgentHost`, so this module exposes
 * them as `start`/`shutdown` and the host fires them at the two boundaries it
 * owns — building a harness, and dropping one.
 *
 * # What is refused, and why refusing beats accepting
 *
 * A callback accepted and never called is a dead mechanism. `REFUSED_HOOKS`
 * names each hook this host will not drive together with the reason, the
 * recorder never registers one, and the count reaches the roster beside the
 * tools a person could not be given. A hook that is neither driven nor refused
 * is recorded as refused as well — losing a company to a new upstream hook
 * would be a worse failure than running without it — and
 * `ExtensionLifecycle.test.ts` fails on it, which is where the loud failure
 * belongs.
 *
 * The map is EMPTY today, and that is a claim rather than an omission: every
 * hook these extensions register is driven. The one entry it used to hold is
 * retired above `DRIVEN_HOOKS`, with the source it was wrong about.
 *
 * # A hosted person has to survive its own length
 *
 * Two members of `ExtensionContext` decide whether a hosted person lives past
 * one context window, and both were absent. `getContextUsage()` is the reading
 * that says a compaction is due; `requestSessionReplacement()` is how an agent
 * asks for a clean transcript. This module answers both, from
 * `server/ContextUsage` and from the host's own `replaceSession` — and it runs
 * the threshold compaction itself, because `AgentHarness` publishes `compact()`
 * and never calls it. A pane gets that threshold from Pi for free; a hosted
 * person had nothing, and would have ended its life at the window.
 */
import { DEFAULT_COMPACTION_SETTINGS, shouldCompact } from '@earendil-works/pi-agent-core'
import type { Api, Model } from '@earendil-works/pi-ai'
import type { ExtensionContext } from '@earendil-works/pi-coding-agent'

import { contextUsage } from '@/server/ContextUsage'
import { sessionsDir } from '@/server/PiHome'
import type { HostedContextUsage } from '@/types/ContextUsage'
import type {
  CompactionReason,
  HostedLifecycle,
  LifecycleSubject,
  RecordedHandlers,
  SessionReplacementRequest,
  SessionReplacementResult
} from '@/types/ExtensionLifecycle'
import { isNullish } from '@/utils/Nullish'

/**
 * The hooks this host fires, and the harness event each one is fired from.
 *
 * A `session_*` entry names no harness event on purpose: those two are the
 * host's own boundaries, fired by `start`/`shutdown` below.
 *
 * # `session_compact`, and the refusal that was wrong on the facts
 *
 * This hook used to be refused, on the reading that `AgentHarness`'s `fromHook`
 * and Pi's `fromExtension` are different facts. They are the same fact under
 * two names, and the two implementations say so:
 *
 *  - `AgentHarness.compact()` takes `provided = hookResult?.compaction` from the
 *    `session_before_compact` hook, calls
 *    `session.appendCompaction(..., provided !== undefined)`, and emits
 *    `{ type: 'session_compact', compactionEntry, fromHook: provided !== undefined }`.
 *  - Pi's `AgentSession` sets `fromExtension = true` when the same
 *    `session_before_compact` emit returns a `compaction`, calls
 *    `sessionManager.appendCompaction(..., fromExtension)`, and emits
 *    `{ type: 'session_compact', compactionEntry, fromExtension, reason, willRetry }`.
 *  - Both `appendCompaction` implementations take that boolean as their
 *    parameter named `fromHook` and write it to the entry's `fromHook` FIELD.
 *
 * One predicate — "did a `session_before_compact` handler supply the summary?" —
 * one persisted field, two event field names. The intercom already requires the
 * two to agree: its handler tests `event.fromExtension !== true` and then
 * `entry.fromHook !== true` on the entry that same event carries. Carrying the
 * harness's boolean across is therefore a straight field carry, not an
 * assertion that one thing is another.
 *
 * `reason` and `willRetry` are Pi's and the harness has neither, so neither is
 * mapped from anything. They are supplied by the host as the host's own facts:
 * this module is what calls `harness.compact()`, so it knows whether it
 * compacted on its own threshold or because an extension asked, and it never
 * retries a turn across a compaction.
 */
export const DRIVEN_HOOKS: ReadonlyMap<string, string> = new Map([
  ['tool_call', 'tool_call'],
  ['tool_result', 'tool_result'],
  ['before_agent_start', 'before_agent_start'],
  ['agent_settled', 'settled'],
  ['model_select', 'model_update'],
  ['agent_start', 'agent_start'],
  ['agent_end', 'agent_end'],
  ['turn_start', 'turn_start'],
  ['turn_end', 'turn_end'],
  ['message_start', 'message_start'],
  ['message_update', 'message_update'],
  ['message_end', 'message_end'],
  ['tool_execution_start', 'tool_execution_start'],
  ['tool_execution_update', 'tool_execution_update'],
  ['tool_execution_end', 'tool_execution_end'],
  ['session_compact', 'session_compact'],
  ['session_start', 'the host builds a harness'],
  ['session_shutdown', 'the host drops a harness']
])

/**
 * The hooks this host will not drive, each with the reason a reader gets.
 *
 * The claim is that every hook these extensions register is CLASSIFIED —
 * driven, or refused by name with a reason a reader gets. It used to be the
 * stronger "every hook is driven", and that held while this map was empty; it
 * stopped being true the moment an extension registered a hook this host has
 * no event to drive it FROM. Weakening the sentence rather than the mechanism
 * is the honest move: a refusal here is a decision somebody made and wrote
 * down, and it reaches the roster by name where silence would not.
 *
 * A hook classified NEITHER way is still refused by `unclassifiedHookReason`
 * rather than accepted into a function that drops it — that is what keeps this
 * claim honest, and it is unchanged. So is the test's discipline: it asserts
 * the exact list rather than a count, so a second refusal cannot ride in
 * beside the first without somebody editing it deliberately, and the reverse
 * check refuses a refusal naming a hook nothing registers.
 */
export const REFUSED_HOOKS: ReadonlyMap<string, string> = new Map<string, string>([
  [
    'input',
    // #1208. The intercom registers `input` to rescue a submission that would
    // otherwise reach Pi's `prompt()` throw on a busy pane. That throw belongs
    // to Pi's INTERACTIVE TUI, which submits bare prompts; this host is not it
    // and has no user-input event to drive the hook FROM — there is no `input`
    // in the harness event union, in `SessionEvents`, or in anything this
    // module forwards. Mapping it to something would be inventing an event.
    //
    // Refused by name rather than accepted into a function that drops it,
    // which is the whole point of this map: a hosted person's submissions
    // arrive by this host's own path and never through the seam the rescue
    // guards, so there is nothing here to rescue and nothing silently lost.
    'this host has no user-input event: the hook guards Pi interactive-TUI submissions, which a hosted person never makes'
  ],
  [
    'session_before_compact',
    // 2026-08-24. The intercom registers `session_before_compact` so a
    // COMPACTION cancels the settle countdown: a compaction emits no turn
    // events while it runs, so a person doing the longest, quietest work a
    // pane does otherwise reads as idle and is parked mid-work.
    //
    // This host drives `session_compact` (the END) but has no event for the
    // START: nothing here decides to compact, so there is no moment to fire it
    // from. Mapping it to `session_compact` would be worse than refusing —
    // it would beat "working" at the instant the work FINISHED, which is the
    // opposite of the fact.
    //
    // Nothing is lost by the refusal: a hosted person's settle countdown is
    // this host's own concern, and the intercom's handler only sends a beat.
    'this host drives the END of a compaction but has no event for its START: it never decides to compact, so there is no moment to fire this from'
  ]
])

/** The reason recorded for a hook nobody has classified. */
export function unclassifiedHookReason(hook: string): string {
  return (
    `"${hook}" is neither in DRIVEN_HOOKS nor in REFUSED_HOOKS. An extension registered a ` +
    `lifecycle hook this host has never decided about, so it is refused rather than accepted ` +
    `and dropped. Classify it in server/ExtensionLifecycle.`
  )
}

function eventType(event: unknown): string | undefined {
  if (typeof event !== 'object' || isNullish(event)) return undefined
  const record: Record<string, unknown> = Object.fromEntries(Object.entries(event))
  return typeof record.type === 'string' ? record.type : undefined
}

/**
 * The agent-loop events this module forwards unchanged, by harness type.
 *
 * Every one of these is field-identical between `AgentEvent` and Pi's
 * `ExtensionEvent`, which is why forwarding is legitimate. The two Pi events
 * that carry extra fields — `turn_start`'s `turnIndex`/`timestamp` and
 * `turn_end`'s `turnIndex` — are read by no handler the extensions register.
 */
const SUBSCRIBED_EVENTS: ReadonlySet<string> = new Set([
  'agent_start',
  'agent_end',
  'turn_start',
  'turn_end',
  'message_start',
  'message_update',
  'message_end',
  'tool_execution_start',
  'tool_execution_update',
  'tool_execution_end'
])

/** Whether the harness refused a queue because no turn is running.
 *
 * `AgentHarness` raises `AgentHarnessError` with `code: 'invalid_state'` for
 * "Cannot steer while idle" and "Cannot follow up while idle". Matched on the
 * CODE rather than on the sentence, and never swallowed wholesale: a genuine
 * failure to deliver must still be reported. */
function isIdleRefusal(error: unknown): boolean {
  if (typeof error !== 'object' || isNullish(error)) return false
  const record: Record<string, unknown> = Object.fromEntries(Object.entries(error))
  return record.code === 'invalid_state'
}

/** A lifecycle handler that threw, reported rather than propagated.
 *
 * Pi's own `ExtensionRunner` routes a handler failure to its error listeners
 * and carries on; a throwing extension never takes the session down. This host
 * keeps that contract for the same reason: one handler's bug must not make a
 * person unroutable, and it must not be silent either. */
function reportHandlerFailure(personCwd: string, hook: string, error: unknown): void {
  /* eslint-disable lucy/no-console-usage */
  // apps/web has no shared logger package — the same exemption
  // `providers/OrgStoreProvider.tsx` records. A lifecycle handler that throws
  // has no other surface: it leaves the person running with one hook silently
  // dead, which is indistinguishable from a healthy agent until somebody
  // notices their mail stopped.
  console.error(`extension lifecycle handler "${hook}" failed for ${personCwd}`, error)
  /* eslint-enable lucy/no-console-usage */
}

/**
 * The live state a lifecycle handler asks this host about.
 *
 * Held rather than derived on demand because Pi's own `ExtensionContext` is a
 * facade of getters resolved at call time, and the intercom depends on that:
 * it reads `isIdle()` AFTER awaits, and an answer captured when the handler
 * started would be a fact about a moment that has passed.
 */
class LifecycleState {
  idle = true
  pending = false
  entries: readonly unknown[] = []
  leafId: string | null = null
  sessionId = ''
  /** How full the window is, as of the last boundary that read the transcript.
   *
   * `undefined` means the live model publishes no usable context window, which
   * is Pi's own answer and not a zero. */
  usage: HostedContextUsage | undefined = undefined
  /** When [`usage`] was computed, epoch ms, or `undefined` before the first
   * snapshot.
   *
   * The reading is deliberately taken at a boundary and served unchanged (see
   * `contextUsage()`), which means it stops advancing whenever boundaries stop
   * happening — and a person whose every turn is refused by the provider
   * produces no `settled`, no `start` and no `compact`, so it stops for good.
   * Observed live: a CEO reported 19,636 tokens for fourteen minutes while the
   * same session recomputed to 1,054,396, which is what the provider had just
   * rejected the request at.
   *
   * A number with no timestamp cannot tell a reader it is dead. That is the
   * whole reason this field exists: not to expire the reading — expiring it
   * would be this host inventing a staleness policy — but to let whoever reads
   * it work out the age for themselves. */
  usageAsOf: number | undefined = undefined
}

/**
 * The `ExtensionContext` this host can answer honestly, and nothing more.
 *
 * Every omission is deliberate and every one of them is a question this
 * process genuinely cannot answer:
 *
 *  - `ui` — there is no terminal. Every intercom call site already wraps
 *    `ui.notify` in a `try`, because a notification is decoration and the
 *    durable event log is the authority.
 *  - `mode`, `signal`, `shutdown`, `isProjectTrusted`, `getSystemPrompt` — no
 *    handler these extensions register reads any of them.
 *
 * The three members that used to head that list are answered now, and each of
 * them is what keeps a hosted person alive past its first context window:
 *
 *  - `getContextUsage` — the reading `server/ContextUsage` computes from the
 *    live model's window and the branch this harness is appending to. It is a
 *    SNAPSHOT because Pi's contract is synchronous and the transcript read is
 *    not; it is refreshed at session start, at settle and after a compaction,
 *    which is every boundary a reader stands on.
 *  - `compact` — `AgentHarness.compact()`, fire-and-forget with Pi's
 *    `onComplete`/`onError` callbacks, exactly as Pi's own binding does it.
 *  - `requestSessionReplacement` — a real replacement, served by the host that
 *    builds harnesses. It answers `true` when it has accepted the request and
 *    `false` when one is already in flight, which is the patched Pi runner's
 *    own contract, and it reports the outcome through `onResult`.
 */
function hostContext(
  subject: LifecycleSubject,
  state: LifecycleState,
  transcriptPath: string,
  actions: {
    compact: (options: unknown) => void
    requestSessionReplacement: (request: unknown) => boolean
  }
): ExtensionContext {
  const context = {
    cwd: subject.cwd,
    hasUI: false,
    get model(): Model<Api> {
      return subject.harness.getModel()
    },
    sessionManager: {
      getSessionId: (): string => state.sessionId,
      getSessionFile: (): string => transcriptPath,
      getSessionDir: (): string => sessionsDir(subject.cwd),
      getEntries: (): readonly unknown[] => state.entries,
      getLeafId: (): string | null => state.leafId
    },
    isIdle: (): boolean => state.idle,
    hasPendingMessages: (): boolean => state.pending,
    abort: (): void => void subject.harness.abort(),
    getContextUsage: (): HostedContextUsage | undefined => state.usage,
    compact: (options: unknown): void => actions.compact(options),
    requestSessionReplacement: (request: unknown): boolean =>
      actions.requestSessionReplacement(request)
  }
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Pi's `ExtensionContext` is a large concrete facade and the extensions
  // installed here read the members above. Structurally implementing the whole
  // interface would mean answering questions this host cannot answer, which is
  // the opposite of what the omissions above are for.
  return context as never
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

/** The driver for one person: the recorded handlers, wired to one harness. */
class LifecycleDriver implements HostedLifecycle {
  private readonly handlers: RecordedHandlers
  private readonly subject: LifecycleSubject
  private readonly state = new LifecycleState()
  /** The one context this person has — handlers AND tools, as under Pi. */
  readonly context: ExtensionContext
  private readonly detach: Array<() => void> = []
  /** One delivery at a time: `prompt()` refuses a busy harness. */
  private delivering: Promise<void> = Promise.resolve()
  private stopped = false
  /** A compaction is running. `AgentHarness.compact()` refuses a busy harness
   * and would report `busy` as a compaction failure, so a second request while
   * one is in flight is refused HERE, where the reason is known. */
  private compacting = false
  /** Why the compaction now in flight was started, for the event it emits. */
  private compactionReason: CompactionReason = 'manual'
  /** One session replacement at a time, which is the patched runner's own
   * single-flight rule: a second request while one is pending answers `false`
   * rather than racing two transcripts under one agent. */
  private replacing = false

  constructor(handlers: RecordedHandlers, subject: LifecycleSubject, transcriptPath: string) {
    this.handlers = handlers
    this.subject = subject
    this.context = hostContext(subject, this.state, transcriptPath, {
      compact: (options) => this.startCompaction('manual', asCompactOptions(options)),
      requestSessionReplacement: (request) => this.requestReplacement(request)
    })
    this.wire()
  }

  /**
   * Run every handler registered for one hook, in registration order.
   *
   * The LAST defined result wins, which is what Pi's runner does for the
   * single-result hooks these extensions use. A handler that throws is
   * reported and the next one still runs: Pi isolates handler failures, and a
   * host that did not would let one extension's bug silence another's.
   */
  private async fire(hook: string, event: unknown): Promise<unknown> {
    const registered = this.handlers.get(hook)
    if (isNullish(registered)) return undefined
    let result: unknown
    for (const handler of registered) {
      try {
        const answer: unknown = await handler(event, this.context)
        if (!isNullish(answer)) result = answer
      } catch (error) {
        reportHandlerFailure(this.subject.cwd, hook, error)
      }
    }
    return result
  }

  /** Re-read the transcript the harness is appending to.
   *
   * `Session.getEntries()` is asynchronous and Pi's `SessionManager` answers
   * synchronously, so the snapshot is refreshed at the two boundaries whose
   * handlers read it — session start and settle — and is therefore the
   * transcript as it stood when the handler was called, which is exactly what
   * Pi's own synchronous read gives. */
  private async snapshotSession(): Promise<void> {
    try {
      this.state.entries = await this.subject.session.getEntries()
      this.state.leafId = await this.subject.session.getLeafId()
      const metadata: { id?: unknown } = await this.subject.session.getMetadata()
      this.state.sessionId = typeof metadata.id === 'string' ? metadata.id : ''
      // The context reading rides the SAME snapshot, and must: a percentage
      // taken from a different read of the transcript than the entries a
      // handler is looking at is two answers to one question.
      this.state.usage = contextUsage(
        this.subject.harness.getModel().contextWindow,
        await this.subject.session.getBranch(),
        (await this.subject.session.buildContext()).messages
      )
      // Stamped only on the success path, and LAST. A snapshot that threw
      // leaves the previous reading in place — which is the right call, a
      // last-known-good beats nothing — but it must not also carry a fresh
      // timestamp, or the one signal that says "this is old" would be renewed
      // by the failure that made it older.
      this.state.usageAsOf = Date.now()
    } catch (error) {
      reportHandlerFailure(this.subject.cwd, 'session-snapshot', error)
    }
  }

  /**
   * Compact this person's context, and report the outcome Pi's way.
   *
   * Fire-and-forget by contract: `ExtensionContext.compact` returns `void` and
   * hands its result to `onComplete`/`onError`, which is what the intercom's
   * durable compaction receipt is written from. The host's own threshold
   * compaction goes through the same path with no callbacks, so there is one
   * compaction mechanism rather than two.
   */
  private startCompaction(reason: CompactionReason, options: CompactOptions): void {
    void this.runCompaction(reason, options)
  }

  private async runCompaction(reason: CompactionReason, options: CompactOptions): Promise<void> {
    if (this.compacting || this.stopped) {
      const refusal = new Error('a compaction is already running for this person')
      if (isNullish(options.onError)) reportHandlerFailure(this.subject.cwd, 'compact', refusal)
      else options.onError(refusal)
      return
    }
    this.compacting = true
    this.compactionReason = reason
    try {
      const result = await this.subject.harness.compact(options.customInstructions)
      // The window changed, so the reading has to. A stale snapshot here is
      // what turns one compaction into an endless run of them.
      await this.snapshotSession()
      options.onComplete?.(result)
    } catch (error) {
      const failure = error instanceof Error ? error : new Error(String(error))
      if (isNullish(options.onError)) reportHandlerFailure(this.subject.cwd, 'compact', failure)
      else options.onError(failure)
    } finally {
      this.compacting = false
    }
  }

  /** Compact when the window says it is time, at the one boundary where the
   * harness is idle and the reading is fresh.
   *
   * This is the threshold a tmux pane gets from Pi for nothing.
   * `AgentHarness` publishes `compact()` and never calls it, so without this a
   * hosted person grows until the provider refuses the request — and no
   * durable request, no operator and no ledger is involved in preventing it,
   * exactly as in a pane. */
  private async compactWhenDue(): Promise<void> {
    const usage = this.state.usage
    if (isNullish(usage) || isNullish(usage.tokens)) return
    if (!shouldCompact(usage.tokens, usage.contextWindow, DEFAULT_COMPACTION_SETTINGS)) return
    await this.runCompaction('threshold', {})
  }

  /**
   * Accept one session replacement, and serve it after the handler returns.
   *
   * The deferral is the patched runner's own shape and it is not cosmetic: a
   * replacement shuts this driver down, and doing that inside the handler that
   * asked for it would tear down the context the handler is still holding.
   *
   * `false` is the honest answer for a second request while one is pending —
   * the contract's own word for "the host did not accept it" — and it is what
   * the intercom's `scheduleLateNativeFreshSession` reads to decide whether to
   * mark its durable request as host-requested.
   */
  private requestReplacement(request: unknown): boolean {
    const parsed = asReplacementRequest(request)
    if (isNullish(parsed) || this.replacing || this.stopped) return false
    this.replacing = true
    setTimeout(() => void this.serveReplacement(parsed), 0)
    return true
  }

  private async serveReplacement(request: SessionReplacementRequest): Promise<void> {
    const report = async (result: SessionReplacementResult): Promise<void> => {
      this.replacing = false
      try {
        await request.onResult?.(result)
      } catch (error) {
        reportHandlerFailure(this.subject.cwd, 'session-replacement-result', error)
      }
    }
    // The patched runner's own gate, on this host's own two facts. A
    // replacement swaps the transcript under the agent, so a turn that started
    // between the request and here must keep its session.
    if (!this.state.idle || this.state.pending) {
      await report({
        status: 'failed',
        error: 'Native session replacement was skipped because the person left its idle boundary'
      })
      return
    }
    try {
      await this.subject.replaceSession(request)
      await report({ status: 'completed' })
    } catch (error) {
      await report({
        status: 'failed',
        error: error instanceof Error ? error.message : String(error)
      })
    }
  }

  /** Serve a replacement a handler asked for by RETURNING it.
   *
   * `session_start` and `agent_settled` may answer with a `newSession`, and
   * that return value is the intercom's PRIMARY way of asking: the direct
   * `ctx.requestSessionReplacement(...)` call is its late fallback. A host that
   * drove the hook and dropped the result would have exactly half the
   * mechanism, which is the shape of every defect this module was written
   * against. */
  private honourReplacementResult(result: unknown): boolean {
    if (typeof result !== 'object' || isNullish(result)) return false
    const record: Record<string, unknown> = Object.fromEntries(Object.entries(result))
    if (!('newSession' in record)) return false
    return this.requestReplacement(record.newSession)
  }

  /** Attach to the harness. Every listener is detached on shutdown, so a
   * person chiefd drops stops observing as well as stops being referenced. */
  private wire(): void {
    const harness = this.subject.harness
    this.detach.push(
      harness.subscribe((event): Promise<void> | void => {
        const type = eventType(event)
        if (isNullish(type) || !SUBSCRIBED_EVENTS.has(type)) return undefined
        if (type === 'agent_start' || type === 'turn_start') this.state.idle = false
        return void this.fire(type, event)
      })
    )
    this.detach.push(
      harness.on('before_agent_start', async () => {
        this.state.idle = false
        await this.fire('before_agent_start', { type: 'before_agent_start' })
        return undefined
      })
    )
    this.detach.push(
      harness.on('tool_call', async (event) => {
        const result = await this.fire('tool_call', event)
        return asToolCallResult(result)
      })
    )
    this.detach.push(
      harness.on('tool_result', async (event) => {
        const result = await this.fire('tool_result', event)
        return asToolResultPatch(result)
      })
    )
    this.detach.push(
      harness.on('model_update', async (event) => {
        // Pi's `model_select` and the harness's `model_update` carry the same
        // three fields under the same names. `source` is `"set" | "restore"`
        // here and `"set" | "cycle" | "restore"` in Pi — a subset, because
        // this host has no model cycler.
        await this.fire('model_select', { ...event, type: 'model_select' })
        return undefined
      })
    )
    this.detach.push(
      harness.on('queue_update', (event) => {
        this.state.pending =
          event.steer.length > 0 || event.followUp.length > 0 || event.nextTurn.length > 0
        return undefined
      })
    )
    this.detach.push(
      harness.on('session_compact', async (event) => {
        // A straight field carry plus two facts this host owns. `fromHook` and
        // `fromExtension` are one predicate under two names (see
        // `DRIVEN_HOOKS`), and `reason`/`willRetry` are the host's own: it
        // started this compaction and it never retries a turn across one.
        await this.fire('session_compact', {
          type: 'session_compact',
          compactionEntry: event.compactionEntry,
          fromExtension: event.fromHook,
          reason: this.compactionReason,
          willRetry: false
        })
        return undefined
      })
    )
    this.detach.push(
      harness.on('settled', async () => {
        // Idle FIRST: Pi 0.80.10 already reports idle at this boundary, and
        // the intercom's idle-resume path returns unless `isIdle()` is exactly
        // true. Snapshot second, so the settled handler's transcript read sees
        // the turn that just ended — and carries the context reading the
        // handlers are about to make a compaction decision from.
        this.state.idle = true
        await this.snapshotSession()
        const result = await this.fire('agent_settled', { type: 'agent_settled' })
        // A replacement takes precedence over the threshold: compacting a
        // transcript that is about to be abandoned spends a summary on
        // history nobody will read again.
        if (this.honourReplacementResult(result)) return undefined
        await this.compactWhenDue()
        return undefined
      })
    )
  }

  /**
   * Deliver one extension message, and WAKE the person when nothing is
   * running.
   *
   * Pi's `deliverAs` names one of the harness's three queues, and two of them
   * — `steer` and `followUp` — refuse outright while the harness is idle
   * (`AgentHarnessError` / `invalid_state`, "Cannot follow up while idle").
   * The third accepts anything and is worse: `AgentHarness` never drains
   * `nextTurnQueue` on its own. It reports the count at `settled` and leaves
   * running it to whoever hosts the agent, so a message queued there by this
   * host would look delivered and never be read.
   *
   * A mailbox drain fires precisely when a person is idle — that is what
   * `agent_settled` means, and what a `doc-change` finds a quiet person in —
   * so the idle case is the COMMON one, not the edge. It is why a reminder
   * that reached this far was still lost: the drain succeeded, the durable
   * envelope was archived, and `followUp` threw into a fire-and-forget void.
   *
   * Serialized on one chain because `prompt()` refuses a busy harness: two
   * envelopes arriving together get two turns in order rather than one turn
   * and one "AgentHarness is busy".
   */
  /** The reading the extensions are seeing, read out and never recomputed.
   *
   * `hostContext`'s `getContextUsage` returns this same field, so a reader over
   * HTTP and a lifecycle handler are looking at ONE snapshot. Recomputing here
   * would answer a question that already has an answer-holder, and the two
   * would part company in exactly the interval a reader cares about: between a
   * boundary and the next `snapshotSession`. */
  contextUsage(): HostedContextUsage | undefined {
    return this.state.usage
  }

  /** When [`contextUsage`] was taken, epoch ms, or `undefined` if never.
   *
   * Deliberately NOT folded into `HostedContextUsage`: that type is
   * field-identical to Pi's `ContextUsage` because it is what
   * `getContextUsage()` hands the extensions, and an extra member there would
   * make this host's answer a different shape from the one every extension was
   * written against. The age belongs to the READER of the snapshot, not to the
   * reading. */
  contextUsageAsOf(): number | undefined {
    return this.state.usageAsOf
  }

  deliver(text: string, mode: 'steer' | 'followUp' | 'nextTurn'): void {
    const trimmed = text.trim()
    if (trimmed === '') return
    this.delivering = this.delivering
      .then(() => this.deliverOne(text, mode))
      .catch((error: unknown) => reportHandlerFailure(this.subject.cwd, 'deliver', error))
  }

  private async deliverOne(text: string, mode: 'steer' | 'followUp' | 'nextTurn'): Promise<void> {
    const harness = this.subject.harness
    // A running turn is the only state in which `deliverAs` means anything, so
    // the extension's own choice is honoured exactly there. `state.idle` is
    // this host's own tracking and can be one event stale, so the refusal is
    // caught as well: an `invalid_state` here means the turn ended between the
    // check and the call, which is the same case as idle.
    if (!this.state.idle && mode !== 'nextTurn') {
      try {
        if (mode === 'steer') await harness.steer(text)
        else await harness.followUp(text)
        return
      } catch (error) {
        if (!isIdleRefusal(error)) throw error
      }
    }
    // Nothing is running, so nothing will ever read a queue. The wake IS the
    // turn. Not awaited for its ANSWER — `sendMessage` is fire-and-forget and
    // a reminder's whole product is that the agent starts thinking — but
    // awaited for its COMPLETION, so the next delivery does not collide.
    await harness.prompt(text)
  }

  async start(reason: 'startup' | 'resume' | 'new'): Promise<void> {
    await this.snapshotSession()
    const result = await this.fire('session_start', { type: 'session_start', reason })
    this.honourReplacementResult(result)
  }

  async shutdown(reason: 'quit' | 'new'): Promise<void> {
    if (this.stopped) return
    this.stopped = true
    // Fire BEFORE detaching: the intercom's `session_shutdown` closes its SSE
    // subscription and then awaits every bounded operation still in flight. A
    // detach first would leave that awaiting work with no way to report.
    await this.fire('session_shutdown', { type: 'session_shutdown', reason })
    for (const off of this.detach) off()
    this.detach.length = 0
  }
}

/** A `tool_call` handler's answer, narrowed to what the harness accepts.
 *
 * Pi's `ToolCallEventResult` and the harness's `ToolCallResult` are the same
 * two fields. Narrowing rather than asserting means a handler returning
 * something else is dropped here instead of reaching the agent loop as a
 * malformed block. */
function asToolCallResult(result: unknown): { block?: boolean; reason?: string } | undefined {
  if (typeof result !== 'object' || isNullish(result)) return undefined
  const record: Record<string, unknown> = Object.fromEntries(Object.entries(result))
  const block = typeof record.block === 'boolean' ? { block: record.block } : {}
  const reason = typeof record.reason === 'string' ? { reason: record.reason } : {}
  return { ...block, ...reason }
}

/** A `tool_result` handler's answer, narrowed the same way.
 *
 * The harness's `ToolResultPatch` carries a `terminate` the Pi event result
 * does not, so it is not read: an extension cannot ask for something Pi never
 * offered it. */
function asToolResultPatch(result: unknown): { details?: unknown; isError?: boolean } | undefined {
  if (typeof result !== 'object' || isNullish(result)) return undefined
  const record: Record<string, unknown> = Object.fromEntries(Object.entries(result))
  const details = 'details' in record ? { details: record.details } : {}
  const isError = typeof record.isError === 'boolean' ? { isError: record.isError } : {}
  return { ...details, ...isError }
}

/** Pi's `CompactOptions`, narrowed to what this host can act on. */
interface CompactOptions {
  customInstructions?: string
  onComplete?: (result: unknown) => void
  onError?: (error: unknown) => void
}

/** An extension's `ctx.compact(...)` argument, narrowed the same way a handler
 * result is: an option this host does not understand is dropped here rather
 * than reaching `AgentHarness.compact` as something it never accepted. */
function asCompactOptions(options: unknown): CompactOptions {
  if (typeof options !== 'object' || isNullish(options)) return {}
  const record: Record<string, unknown> = Object.fromEntries(Object.entries(options))
  const onComplete = asCallback(record.onComplete)
  const onError = asCallback(record.onError)
  return {
    ...(typeof record.customInstructions === 'string'
      ? { customInstructions: record.customInstructions }
      : {}),
    ...(isNullish(onComplete) ? {} : { onComplete }),
    ...(isNullish(onError) ? {} : { onError })
  }
}

/** One callback an extension handed this host, narrowed to "callable".
 *
 * `typeof value === 'function'` narrows to `Function`, which cannot be called
 * with an argument at all, so the narrowing has to name a signature. This one
 * is the widest that is still callable, and what is actually passed is stated
 * at each call site rather than promised here. */
function asCallback(value: unknown): ((argument: unknown) => void) | undefined {
  if (typeof value !== 'function') return undefined
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Pi declares these signatures on `CompactOptions` and
  // `NativeSessionReplacementRequest`; this host is the caller, so the
  // argument it passes is the one it constructs immediately above the call.
  return value as (argument: unknown) => void
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

/**
 * A native session replacement request, narrowed before anything acts on it.
 *
 * `customType` is the one required field, and it is required for a reason the
 * intercom states from the other side: the marker entry written into the new
 * transcript IS the receipt its `nativeResetProof` reads back. A request with
 * no marker would replace a session and leave nothing to prove it, so it is
 * refused here rather than served into a transcript nobody can attribute.
 */
function asReplacementRequest(request: unknown): SessionReplacementRequest | undefined {
  if (typeof request !== 'object' || isNullish(request)) return undefined
  const record: Record<string, unknown> = Object.fromEntries(Object.entries(request))
  if (typeof record.customType !== 'string' || record.customType === '') return undefined
  const onResult = asCallback(record.onResult)
  return {
    customType: record.customType,
    ...('data' in record ? { data: record.data } : {}),
    ...(isNullish(onResult) ? {} : { onResult })
  }
}

/**
 * Wire one person's recorded handlers to the harness they were installed for.
 *
 * `transcriptPath` is the file the harness is actually appending to, which is
 * not always the one chiefd named: a person who has never spoken has no
 * `sessionFile` and this host creates one. The lifecycle must report the file
 * in use, because the intercom's native session-replacement proof compares it
 * against the directory it scans.
 */
export function driveLifecycle(
  handlers: RecordedHandlers,
  subject: LifecycleSubject,
  transcriptPath: string
): HostedLifecycle {
  return new LifecycleDriver(handlers, subject, transcriptPath)
}
