/** Public types for the in-process Pi agent registry.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */
/** What chiefd's launch profile tells this server about one person. */
export interface AgentProfile {
  readonly personId: string
  readonly cwd: string
  readonly env: Readonly<Record<string, string>>
  /** The person's transcript, absent for somebody who has never spoken. */
  readonly sessionFile?: string
  /** Tool ids chiefd granted this person. Carried, not decided: the manifest
   * says what somebody may call and chiefd has already resolved it. */
  readonly tools: readonly string[]
  /** How the person is titled to itself: "<company> · <title>". */
  readonly displayName: string
}
