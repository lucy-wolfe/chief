/** A fully resolved, non-secret input for one hosted agent. ChiefD decides
 * every field; the host adds nothing.
 *
 * Every field is a FACT about the person, not an invocation of a program. This
 * used to carry `rpcArgs` — `--tools`/`--session` flags for a Pi
 * CLI subprocess. The harness is now an in-process library, so argv has no
 * consumer, and the flag encoding lost information: a reader had to scan for
 * `--session` and take the next element to learn the transcript path. */
export interface ApiHostLaunchProfile {
  personId: string
  cwd: string
  env: Readonly<Record<string, string>>
  /** The person's own transcript, absent for somebody who has never spoken. */
  sessionFile?: string
  /** Tool ids this person may call. */
  tools: readonly string[]
  /** How the person is titled to itself: "<company> · <title>". */
  displayName: string
}

/** Who is actuating a company. chiefd publishes this as a FACT and no longer
 * refuses the read outside shadow mode.
 *
 * The read used to answer `422 company-not-api-hosted` whenever the company
 * was actuating runtime panes, so that no caller could stand up a second actuator
 * beside live convergence. The rule is still right; enforcing it on a READ was
 * not. Reading a fact is not actuating on it, and only the reader knows whether
 * it is about to — while the gate made this contract unreadable by the client
 * that needs it most, since a operator client runs against `apply` by definition.
 *
 * All three values the refusal carried are here, so a caller that must not
 * double-actuate raises its own refusal from them. */
export interface ApiHostActuation {
  /** What the gate reads. A tripped breaker forces `shadow`, so this can differ
   * from `configuredMode`. */
  effectiveMode: string
  /** What the durable document says, before the breaker is applied. */
  configuredMode: string
  /** Whether the converge breaker is tripped. */
  breakerTripped: boolean
}

/** One read of the API-host launch profile: who is actuating, and the plans. */
export interface ApiHostLaunchProfileRead {
  actuation: ApiHostActuation
  plans: readonly ApiHostLaunchProfile[]
}
