/** Public types for the hosted lifecycle driver.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */
import type { AgentHarness, Session } from '@earendil-works/pi-agent-core'
import type { ExtensionContext } from '@earendil-works/pi-coding-agent'

import type { HostedContextUsage } from '@/types/ContextUsage'

/** One `ExtensionAPI.on(...)` registration, as this host records it.
 *
 * Pi's own `Extension.handlers` is a `Map<string, HandlerFn[]>` of exactly
 * this shape: a hook name and the functions registered under it, in
 * registration order. Nothing here narrows the event or the result — the
 * bridge hands each handler the event Pi would hand it and reads back the
 * result Pi would read. */
export type RecordedHandler = (event: unknown, context: unknown) => unknown

/** The handlers one person's extensions registered, by hook name. */
export type RecordedHandlers = ReadonlyMap<string, readonly RecordedHandler[]>

/** What this host needs in order to answer a lifecycle handler honestly.
 *
 * Every member is a fact the host already holds. Nothing is derived, defaulted
 * or invented: a lifecycle handler asking a question this host cannot answer
 * must get no answer rather than a plausible one. */
export interface LifecycleSubject {
  /** The harness whose events drive the lifecycle. */
  readonly harness: AgentHarness
  /** The transcript that harness reads and appends to. */
  readonly session: Session
  /** The person's own workspace, which is also the agent's cwd. */
  readonly cwd: string
  /** Replace this person's session with a fresh one carrying the request's
   * marker entry, and rebuild everything that was bound to the old one.
   *
   * Supplied by the host rather than performed here because building and
   * dropping a harness are the host's two boundaries, and this is both of them
   * in one operation. It THROWS on failure: a replacement that silently did
   * not happen would leave a durable fresh-session request looking served. */
  readonly replaceSession: SessionReplacer
}

/** The host operation that performs one session replacement.
 *
 * Named rather than written inline because both the subject and every test
 * double refer to it, and a shape restated in four places is a shape that
 * drifts in three of them. */
export type SessionReplacer = (request: SessionReplacementRequest) => Promise<void>

/** How a replacement's outcome is reported back to whoever asked for it. */
export type SessionReplacementReport = (result: SessionReplacementResult) => void | Promise<void>

/** Why a compaction ran, in Pi's own vocabulary.
 *
 * `overflow` is Pi's third word and this host never uses it: it names a
 * compaction that recovers an aborted turn so the turn can be retried, and
 * nothing here retries a turn. */
export type CompactionReason = 'manual' | 'threshold'

/** One native session replacement, as an extension asks for it.
 *
 * Field-identical to the `NativeSessionReplacementRequest` the Pi patch adds,
 * because it is the same request: the extensions hand this shape to
 * `ctx.requestSessionReplacement(...)`, and return it as `newSession` from
 * `session_start` and `agent_settled`. */
export interface SessionReplacementRequest {
  /** The marker entry written into the new transcript. It is the RECEIPT: the
   * intercom reads it back out of the file to prove the replacement happened. */
  readonly customType: string
  readonly data?: unknown
  readonly onResult?: SessionReplacementReport
}

/** What became of a replacement, reported to whoever asked for it. */
export interface SessionReplacementResult {
  readonly status: 'completed' | 'cancelled' | 'failed'
  readonly error?: string
}

/** A running person's lifecycle, as the host starts and stops it. */
export interface HostedLifecycle {
  /** The ONE `ExtensionContext` this person has.
   *
   * Pi hands a tool's `execute` and a lifecycle handler the same object, and
   * this host must too. A tool holding a thinner context than the handlers
   * would be a second answer to "which session is this", and the intercom
   * would resolve a model change against one and its restore correction
   * against the other. */
  readonly context: ExtensionContext
  /** How full this person's context window is, as this host last read it.
   *
   * The SAME snapshot `context.getContextUsage()` hands the extensions, read
   * out rather than recomputed. That is the whole value of it: the arithmetic
   * in `server/ContextUsage` is already covered, and what no reader outside
   * this process could see was the SNAPSHOT — whether it exists yet, and which
   * boundary it was taken at. A caller that recomputed would be a second
   * answer to one question, and the two would disagree exactly between a
   * boundary and the next refresh, which is the interval that matters.
   *
   * `undefined` means the live model publishes no usable context window. It is
   * never a zero. */
  contextUsage(): HostedContextUsage | undefined

  /** When [`contextUsage`] was taken, epoch ms, or `undefined` if never.
   *
   * The reading above is a snapshot and says so; this is the fact that makes
   * that claim checkable. A boundary-taken number stops advancing when
   * boundaries stop, and a person whose every turn the provider refuses
   * produces no further boundary — so the reading can be arbitrarily old while
   * looking exactly like a fresh one. The age is deliberately left to the
   * caller: a staleness verdict here would be this host inventing a policy. */
  contextUsageAsOf(): number | undefined
  /** Deliver one message an extension pushed at its own agent.
   *
   * `mode` is Pi's `deliverAs`, and it names a QUEUE — which only exists
   * while a turn is running. A message pushed at an IDLE person cannot use
   * one, and queueing it would be worse than dropping it: `AgentHarness`
   * never drains `nextTurnQueue` itself, so the message would sit there
   * looking delivered forever. Waking an idle person is therefore running a
   * turn, and that is what this does.
   *
   * Fire-and-forget, because `ExtensionAPI.sendMessage` is: it returns void
   * and its callers treat delivery as best-effort. */
  deliver(text: string, mode: 'steer' | 'followUp' | 'nextTurn'): void
  /** Fire `session_start` and let the extensions open their own channels.
   *
   * `reason` is Pi's own vocabulary. The host has exactly three: `startup` for
   * a person hosted for the first time in this process, `resume` for one whose
   * harness was rebuilt because chiefd's profile changed, and `new` for the
   * session a replacement just created — which is the word Pi uses for the
   * same boundary, and the one the intercom's startup path reads a fresh
   * transcript from. */
  start(reason: 'startup' | 'resume' | 'new'): Promise<void>
  /** Fire `session_shutdown`, which is what closes the SSE subscription.
   *
   * `quit` is a person chiefd no longer wants running; `new` is a harness
   * being replaced by a rebuild of the same person. */
  shutdown(reason: 'quit' | 'new'): Promise<void>
}
