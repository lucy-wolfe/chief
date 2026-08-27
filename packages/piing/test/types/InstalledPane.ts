/**
 * Public types for the installed-pane harness.
 * Housed here per `lucy/no-exported-type-outside-types-dir`.
 */

/** One durable organization event, reduced to the fields tests read back. */
export interface OrganizationEvent {
  event?: string
  kind?: string
  consecutiveFailures?: number
  /** Every failure-path event carries it, and it must stay false. */
  automaticRetry?: boolean
  /** `message-queued` / `message-bounced`: who it went to. */
  to?: string
  recipientPersonId?: string
  /** `message-queued`: the envelope, flattened onto the event. */
  id?: string
  fromPersonId?: string
  body?: string
  /** `message-bounced`: the envelope ids this refusal destroyed. */
  messageIds?: readonly string[]
}

/** One inbound delivery a test hands to the pane, reduced to the fields the
 *  acceptance path reads. */
export interface Delivery {
  readonly id: string
  readonly fromPersonId: string
  readonly body: string
}

/** One custom entry exactly as `pi.appendEntry` received it. */
export interface PaneEntry {
  readonly customType: string
  readonly data: Record<string, unknown>
}

/**
 * One installed intercom, plus everything a test needs to drive it and to read
 * what the OPERATOR would have seen.
 */
export interface Pane {
  /** Fire one FAILED turn through the production `agent_end` handler. */
  endTurn(errorMessage: string): Promise<void>
  /**
   * Fire Pi's `session_before_compact` — the signal a compaction has STARTED.
   * Throws when the install did not register it, because a silently
   * unregistered hook is the feature quietly not existing.
   */
  beginCompaction(): Promise<void>
  /** The same failed turn, except that it had already begun a tool call. */
  endTurnWithToolCall(errorMessage: string): Promise<void>
  /** Fire one turn that COMPLETED — the event that re-arms the failure cards. */
  completeTurn(): Promise<void>
  /**
   * Put `deliveries` in this person's mailbox as PENDING and then drive the
   * production `message_start` handler over them, which is what receipts them
   * `accepted`. One delivery arrives as a single envelope, several as the batch
   * card — both are shapes Pi really produces.
   */
  deliver(...deliveries: readonly Delivery[]): Promise<void>
  /** Every durable organization event written so far, in order. */
  events(): readonly OrganizationEvent[]
  /** Every custom entry the install appended to the transcript, in order. */
  entries(): readonly PaneEntry[]
  /**
   * Every agent-activity beat this pane posted, in order.
   *
   * This is the settle countdown's ONE input: chiefd knows nothing else about
   * whether a person is working, so "did the countdown keep running" is exactly
   * "what did this pane report".
   */
  beats(): ReadonlyArray<{ person: string; working: boolean }>
  /**
   * The entry, rendered through the renderer the install actually registered,
   * flattened to the lines a terminal would print. Throws when no renderer is
   * registered for the entry's type — a card nothing can draw is not delivered.
   */
  render(entry: PaneEntry): string
}

/** The theme the harness renders with: it records nothing and paints nothing,
 *  because the assertions are about the words the operator reads and color
 *  would only hide them. */
export interface PlainCardTheme {
  bold(text: string): string
  fg(token: string, text: string): string
  bg(token: string, text: string): string
}
