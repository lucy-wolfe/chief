/**
 * Wire types for the runtime / materialization / model-command route family
 * (`chiefd-api`'s `docstore/runtime_routes.rs`).
 *
 * These replace the exported interfaces of seven deleted TypeScript modules —
 * `org-runtime.ts`, `org-runtime-contracts.ts`, `org-materialize.ts`,
 * `org-model-command.ts`, `org-company-session-actions.ts`,
 * `org-extension-runtime-drift.ts` and `org-monitor-reader.ts`. Nothing here
 * carries behaviour: every decision they used to make now happens in Rust, and
 * these are the shapes that come back over the wire.
 */

/* TOMBSTONE (chief-home-is-cwd §4d): `PersonMaterializationFailure`,
 * `PersonFailurePolicy`, `MaterializeResult` and `PersonExtensionDrift` — the
 * wire shapes of the deleted `POST /v1/org/materialize/*` family. No home is
 * projected from the manifest, so a person cannot fail to materialize, there
 * is no blast radius to choose between refusing and containing, and a home
 * that is written once and never re-projected cannot drift from a source. */

/* TOMBSTONE (chief-home-is-cwd §3/§4e): `InstalledResourceRef` and
 * `InstalledResourceCatalog` — the wire shape of
 * `/v1/org/resource-catalog/read`, which listed the skill, extension and
 * package ids a person could be hired with. Nobody is hired with one: an
 * agent's skills are the files in `<dir>/.pi/skills`, which Pi discovers and
 * loads through one symlink, so there is no catalog to choose from and no
 * route left to serve it. */

/** A running agent that cannot have loaded the extensions on its own disk. */
export interface PersonRuntimeExtensionDrift {
  readonly personId: string
  readonly panePid: number
  readonly loadedStale: readonly string[]
  readonly processStartedAt: string
}

// TOMBSTONE: `ActuatorPresence`, `Unobserved`, `RuntimeDriftScan` and
// `PersonRuntimeExtensionDrift`'s scan wrapper.
//
// `ActuatorPresence` mirrored chiefd's answer to "who is actuating this
// company, and does their lease still hold". The lease was renewed by the
// actuator POSTING its observation, and there is no observation, so there is no
// lease and no presence. chiefd cannot know whether anybody is attached and
// deliberately does not try -- a NAMED, ACCEPTED loss.
//
// `Unobserved` existed so an empty drift list could be told apart from "nobody
// looked". That distinction was load-bearing and correct while the answer
// depended on somebody looking; it has no referent once nobody does.

// TOMBSTONE: `DeployDriftVerdict` and `DeployDriftReport`.
//
// The mirror of chiefd's RUNTIME drift scan: which running people could not
// have loaded the extensions currently on their disk. Its whole vocabulary --
// `observer: ActuatorPresence`, `unobserved`, and an `exitCode` of 4 meaning
// "nobody looked" -- existed only because the answer depended on somebody
// having observed the host, and chiefd no longer observes anything.
//
// The concern is preserved by construction rather than dropped: the extension
// source digest is an input to a person's derived launch hash, so a deploy
// moves the hash, the running pane's tag no longer matches, and the actuator
// replaces it. A stale pane cannot survive a converge pass, so there is nothing
// left to scan for and no verdict to report.

// TOMBSTONE: `RuntimeObservationResult`. It was the shape of
// `POST /v1/org/runtime/observe` — desired versus observed person ids and an
// `exact` verdict over the two. chiefd holds the desired half and no longer has
// the observed half, so there is no comparison left to describe.
//
// `unexpectedObservedPersonIds` is NOT deleted by name-similarity elsewhere: it
// is a separate desired-side projection with a separate meaning
// (`runtime_projection.rs`), and only the feed FROM the observation dies. It
// dies HERE because this type's other three fields cannot be filled at all.

/**
 * Everything a launch decides that the committed rows cannot supply.
 *
 * The two id lists are NOT interchangeable. `requestedPersonIds` becomes
 * durable launch intent — only those nodes may run, and the CEO is always
 * implicitly intended. `executionLeasePersonIds` is an in-memory projection
 * input only: persisting an execution lease would leave a manager resident
 * after one completed public tool call, which is exactly the minimum-fleet
 * violation the split exists to prevent.
 */
export interface LaunchInput {
  readonly slug: string
  /** Who is asking, for the durable audit trail. */
  readonly actor: string
  readonly requestedPersonIds?: readonly string[]
  readonly executionLeasePersonIds?: readonly string[]
  // `materializationReady` is DELETED (A6). It asked chiefd to skip the
  // materialization freshness repair "because the caller has just
  // materialized", and nothing in the tree ever set it. That repair is the ONLY
  // thing that enrols a company's people into the trust table genesis mints
  // their keys for, so once the auth gate became unconditional a client
  // asserting readiness would have launched panes whose people could not
  // authenticate at all. A client-supplied switch that produces an
  // unauthenticable company is not configuration.
}

export interface RuntimeLaunchResult {
  readonly slug: string
  readonly sessionName: string
  readonly socketName: string
  /** How many people chiefd DESIRES running. Rust authority:
   * `ReconcileReport::desired_people`. It was `plannedSteps`, a count of the
   * per-person actions chiefd emitted; chiefd emits none, and the two differ in
   * every case that matters — a steady company of four plans zero and desires
   * four. */
  readonly desiredPeople: number
  // TOMBSTONE: `actuatedSteps`. Always 0 — chiefd emits no actions and applies
  // none, so the only number it could honestly report was zero, and it was
  // logged on every pass as `actuated=0`. How many actions were APPLIED is the
  // client's count and arrives on its own observed-runtime POST. Same ruling as
  // `deferredStarts` in `organization-intercom.ts`: a field permanently zero is
  // worse than no field, because a reader branches on it.
  readonly notes: readonly string[]
}

export interface RuntimeStopResult {
  readonly slug: string
  readonly sessionName: string
  readonly stopped: boolean
  readonly notes: readonly string[]
}

/**
 * Who owns a company's runtime, as chiefd validated and derived it.
 *
 * Absence of the row is the DECIDED initial state — a company that has never
 * claimed a runtime is `released` — not a refusal. The server derives that,
 * so no client carries the rule.
 */
export interface RuntimeOwnership {
  readonly version: number
  readonly organization: string
  readonly status: 'active' | 'released'
  /** The owning actuator's own opaque handle. chiefd stores it and compares
   * it for equality; it never parses it, and only the operator client reads it
   * as a tmux socket. AC6 removed the `sessionName` that stood beside it — it
   * was `org-<slug>` for this record's own slug. */
  readonly socketName?: string
  readonly claimedAt?: string
  readonly validatedAt?: string
  readonly releasedAt?: string
}

export interface RuntimeOwnershipResult {
  readonly organization: string
  readonly status: 'active' | 'released'
  readonly socketName?: string
  readonly takeover: boolean
  readonly previousSocketName?: string
}

/** Whether the company's runtime is up, as the progress view reports it. */
export type CompanyActionRuntime = 'running' | 'stopped'

// TOMBSTONE: `SessionMaintenanceAction` and `CompanySessionActionProgress`,
// the company-wide fanout's wire types. The feature is deleted whole and no
// caller could ever create one.
// TOMBSTONE: `SkippedParkedMaintenanceTarget`, the row
// `skipParkedCompanySessionTargets` returned. Deleted with the company-action
// family it belonged to.
