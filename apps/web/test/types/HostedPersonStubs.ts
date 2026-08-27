/**
 * Types for `test/harness/HostedPersonStubs.ts`. Kept in a `/types/` directory
 * (matching `lucy/no-exported-type-outside-types-dir`, which applies to test/
 * the same as src/) — the harness itself stays focused on the recorders.
 */
import type { AgentHarness, Session } from '@earendil-works/pi-agent-core'

/** What a harness recorder is built to stand in for.
 *
 * Every option is a fact a real harness and its session already carry, and
 * each one exists because a hosted person's SURVIVAL depends on it: the model's
 * window, how much of it the conversation currently fills, and what is left
 * after a compaction. */
export interface HarnessStubOptions {
  /** The live model's context window. `0` — the default — is a model that
   * publishes none, which is the case Pi answers `undefined` for. */
  readonly contextWindow?: number
  /** Context tokens the transcript reports before anything compacts it. */
  readonly contextTokens?: number
  /** Context tokens left after a compaction. `0` is the honest default: right
   * after a compaction nothing has answered yet, so the count is unknown. */
  readonly tokensAfterCompaction?: number
}

/** One compaction the harness actually performed. */
export interface CompactionRecord {
  readonly customInstructions?: string
  readonly tokensBefore: number
}

/** How a message actually reached the person.
 *
 * `steer` and `followUp` are the harness's live queues and exist only while a
 * turn is running. `prompt` is a turn STARTED for the message — the only way
 * to reach an idle person, since `AgentHarness` never drains its own
 * `nextTurnQueue`. The distinction is the assertion: a reminder delivered to a
 * quiet agent must arrive as `prompt`, because everything else is a message
 * nobody will read. */
export type QueuedMode = 'steer' | 'followUp' | 'prompt'

/** One message the extensions pushed into the person's own queue. */
export interface QueuedMessage {
  readonly mode: QueuedMode
  readonly text: string
}

/** A harness recorder plus the handles a test drives it with. */
export interface HarnessStub {
  /** The object `LifecycleSubject.harness` is built from. */
  readonly harness: AgentHarness
  /** The transcript that harness is appending to — ONE conversation, as in
   * production, so a compaction the harness performs is visible to the
   * reading the driver takes from the session. */
  readonly session: Session
  /** Everything the extensions delivered into this person's queue, in order. */
  readonly delivered: QueuedMessage[]
  /** Every compaction the harness actually ran, in order. */
  readonly compactions: CompactionRecord[]
  /** The transcript's current size, as its usage reports it. */
  contextTokens(): number
  /** How many times a circuit breaker aborted this person's turn. */
  abortCount(): number
  /** Emit one `AgentEvent`, exactly as the agent loop does. */
  emit(event: { type: string } & Record<string, unknown>): Promise<void>
  /** Fire one of the harness's own events and collect what the driver
   * returned, which is what the real harness does with the result. */
  fire(type: string, event: Record<string, unknown>): Promise<unknown[]>
  /** How many listeners are still attached — zero after a shutdown. */
  listenerCount(): number
}
