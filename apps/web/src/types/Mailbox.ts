/** Public types for the mailbox read.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */

/** One person's mailbox as this server answers it.
 *
 * `pendingCount` is counted on the server against chiefd's own bucket
 * vocabulary, so the browser never parses chiefd's storage format and the
 * count has exactly one implementation. */
export interface MailboxRead {
  readonly personId: string
  readonly pendingCount: number
  /** chiefd's entries, forwarded opaque. */
  readonly envelopes: readonly unknown[]
}
