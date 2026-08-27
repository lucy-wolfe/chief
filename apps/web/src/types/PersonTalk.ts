/** Public types for the talk verbs.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */

/** One readable turn of a person's conversation.
 *
 * Only what a reader can read: `user` and `assistant` text. A session tree also
 * carries model changes, tool activity, compactions and labels, and rendering
 * those as conversation would put a tool's JSON on screen as if the agent had
 * said it. */
export interface TranscriptEntry {
  /** Always `message`. The browser's fold switches on it, and an entry without
   * it is silently skipped — which is how a full transcript rendered empty. */
  readonly type: 'message'
  readonly id: string
  /** The message, reduced to its readable text parts. */
  readonly message: {
    readonly role: 'user' | 'assistant'
    readonly content: readonly { readonly type: 'text'; readonly text: string }[]
  }
}

/** How a message reaches an agent.
 *
 * `prompt` starts a turn and is answered. `steer` and `followUp` hand a message
 * to a turn that is already running — the harness's own queues — and are
 * answered only by the turn they joined. */
export type SayMode = 'prompt' | 'steer' | 'followUp'

/** What one message did.
 *
 * `reply` is present only for `prompt`, because only `prompt` waits for an
 * answer. A queued message reporting `reply: ''` would be indistinguishable
 * from an agent that said nothing. */
export interface SayOutcome {
  readonly personId: string
  readonly mode: SayMode
  readonly reply?: string
}
