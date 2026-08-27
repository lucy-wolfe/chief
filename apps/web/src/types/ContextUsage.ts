/** Public types for the hosted context reading.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */

/** How full a hosted person's context window is.
 *
 * Field-identical to Pi's `ContextUsage`, which is what the extensions read,
 * and it keeps Pi's `null` vocabulary rather than dropping the member: right
 * after a compaction the token count is genuinely UNKNOWN until the next
 * provider response, and "unknown" and "zero" are different answers to a
 * question that decides whether to spend a compaction. */
export interface HostedContextUsage {
  /** Estimated context tokens, or `null` when no assistant has answered since
   * the newest compaction. */
  readonly tokens: number | null
  /** The live model's window. Always a positive number — a model without one
   * produces no reading at all. */
  readonly contextWindow: number
  /** `tokens` as a percentage of `contextWindow`, or `null` with `tokens`. */
  readonly percent: number | null
}
