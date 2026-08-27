/** Public types for roster convergence.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */

/** What one convergence did. */
export interface RosterConvergence {
  readonly hosted: readonly string[]
  /** People running WITHOUT some of the tools chiefd granted them, or with a
   * lifecycle hook this host refuses to drive.
   *
   * A hosted person with no `org_*` tools looks perfectly staffed and cannot
   * delegate, hire or create a department — a failure with no symptom until
   * somebody asks the agent to do its job. A refused hook is the same failure
   * from the other side: the agent looks alive and something that should
   * happen to it never does. Reported rather than swallowed: the alternative
   * is silence about a degraded agent. */
  readonly degraded: readonly DegradedPerson[]
}

/** A hosted person running with less than their extensions asked for. */
export interface DegradedPerson {
  readonly personId: string
  /** The tool ids this host could not supply, sorted. */
  readonly missingTools: readonly string[]
  /** Lifecycle hooks the extensions registered that this host refuses to
   * drive, sorted.
   *
   * A separate list from `missingTools` because they degrade a person
   * differently: a missing tool is something the agent cannot DO, and a
   * refused hook is something that will not HAPPEN to the agent. Folding them
   * into one list would tell an operator neither. */
  readonly refusedHandlers: readonly string[]
}
