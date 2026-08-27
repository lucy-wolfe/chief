/** Public types for the company-lifecycle routes.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */

/** What stopping a company actually did.
 *
 * Both booleans are reported because they are different facts and an operator
 * acts on them differently: a company whose session was killed but whose
 * daemon survived is not stopped, and a single `stopped: true` would hide
 * exactly that. */
export interface CompanyStopped {
  readonly slug: string
  /** Which branch chiefd ran: `supervised`, `orphan-session-stopped`, or
   * `already-stopped`. Carried verbatim — this layer does not re-word it. */
  readonly mode: string
  readonly sessionStopped: boolean
  readonly daemonStopped: boolean
}
