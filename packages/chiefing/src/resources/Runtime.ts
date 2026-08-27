import { isNullish } from '@/Nullish'
import { postOrgRoute } from '@/resources/OrgRoutes'
import type {
  LaunchInput,
  RuntimeLaunchResult,
  RuntimeOwnership,
  RuntimeOwnershipResult,
  RuntimeStopResult
} from '@/types/Runtime'
import type { HttpTransport } from '@/types/Transport'

/**
 * The runtime / runtime-placement / materialization client.
 *
 * Every method here is ONE POST and nothing else. It exists because the
 * seventeen-module TypeScript cluster it replaced
 * (`org-runtime.ts`, `org-tmux.ts`, `org-materialize.ts`,
 * `org-model-command.ts`, `org-company-session-actions.ts`,
 * `org-loop-control.ts`, `org-monitor-reader.ts`,
 * `org-extension-runtime-drift.ts`, `org-runtime-ownership.ts` and their
 * siblings) has been deleted: runtime observation and actuation, pi-home
 * materialization, runtime placement, model and thinking commands and the
 * company-session-action verbs are all chiefd's business now (mandate 3).
 *
 * There is deliberately no decision in this file — no retry ladder, no
 * fallback, no derived state, no local validation that duplicates a refusal
 * chiefd already owns. If a method here ever needs a branch, the branch
 * belongs in Rust.
 */
export class RuntimeClient {
  constructor(
    protected readonly transport: HttpTransport,
    protected readonly url: string = ''
  ) {}

  private slug(slug: string): string {
    return slug
  }

  // ---- runtime lifecycle -------------------------------------------------

  // TOMBSTONE: `observe` and `POST /v1/org/runtime/observe`.
  //
  // It was one fail-closed runtime read compared against the desired-active
  // set: chiefd's answer to "what is actually running". chiefd holds the
  // DESIRED state now and has no view of a host, so the route is deleted rather
  // than left answering an empty or unknown shape.
  //
  // That distinction is the whole reason this is a deletion. The Rust handler
  // REFUSED an unproven observation rather than folding it into an empty one,
  // because an empty `processHandles` would have published a recovery
  // fingerprint accusing every live person of being missing. A TypeScript
  // client that kept calling a route which could only ever answer "nobody" would
  // hand that same conflation to every caller. There is nothing to ask, so
  // there is nothing to call.
  //
  // NAMED, ACCEPTED LOSS: no client can ask chiefd what is running. The
  // actuator owns the operator's screen and is the only process that can see a
  // pane.

  /**
   * `POST /v1/org/runtime/launch` — claim the writer lease and runtime
   * ownership, ensure materialization is current, then converge once.
   *
   * `requestedPersonIds` is durable launch intent: only these nodes may run
   * (the CEO is always implicitly intended). `executionLeasePersonIds` is
   * deliberately NOT durable — persisting an execution lease would leave a
   * manager resident after one completed tool call and break the
   * minimum-fleet rule — so it travels separately and is never recorded.
   */
  async launch(input: LaunchInput): Promise<RuntimeLaunchResult> {
    return postOrgRoute<RuntimeLaunchResult>(
      this.transport,
      this.url,
      '/v1/org/runtime/launch',
      this.launchBody(input)
    )
  }

  // TOMBSTONE (chief-home-is-cwd §4c): `launchCeoOnly` (POST
  // /v1/org/runtime/launch-ceo-only) stood here — "probe the CEO's provider,
  // take the boot lease, bring up exactly the CEO pane, and wait reactively for
  // it to be live". The operator client owns every pane, so there is no first
  // pane for the daemon to bring up.

  /** `POST /v1/org/runtime/resume` — resume a supervised runtime. A resume
   * never opens the launch fence, so requested ids are dropped server-side
   * rather than silently granting intent. */
  async resume(input: LaunchInput): Promise<RuntimeLaunchResult> {
    return postOrgRoute<RuntimeLaunchResult>(
      this.transport,
      this.url,
      '/v1/org/runtime/resume',
      this.launchBody(input)
    )
  }

  /**
   * `POST /v1/org/runtime/stop` — converge to an empty projection, wait
   * reactively for provable session absence, release ownership.
   *
   * `localTeardownReason` is set ONLY by a caller holding the CEO-boot
   * suppression lease. Every other stop is daemon-converged, and setting it
   * where a daemon runs races the duty that owns teardown.
   */
  async stop(input: {
    readonly slug: string
    readonly localTeardownReason?: string
  }): Promise<RuntimeStopResult> {
    return postOrgRoute<RuntimeStopResult>(this.transport, this.url, '/v1/org/runtime/stop', {
      slug: this.slug(input.slug),
      ...(isNullish(input.localTeardownReason)
        ? {}
        : { localTeardownReason: input.localTeardownReason })
    })
  }

  private launchBody(input: LaunchInput): Record<string, unknown> {
    return {
      slug: this.slug(input.slug),
      actor: input.actor,
      requestedPersonIds: [...(input.requestedPersonIds ?? [])],
      executionLeasePersonIds: [...(input.executionLeasePersonIds ?? [])]
    }
  }

  /**
   * `POST /v1/org/runtime/ownership/read` — who owns this company's runtime.
   *
   * Deliberately a route rather than a raw `rows.readRuntimeOwner` read: the
   * server applies the ownership validator and derives the documented initial
   * state ("released") for a company that has never claimed one. A client
   * reading the raw row would have to re-implement both, and a validator that
   * disagreed with the daemon's is how a CLI and its daemon end up with
   * different views of who holds the session.
   */
  async readOwnership(slug: string): Promise<RuntimeOwnership> {
    return postOrgRoute<RuntimeOwnership>(
      this.transport,
      this.url,
      '/v1/org/runtime/ownership/read',
      { slug: this.slug(slug) }
    )
  }

  /** `POST /v1/org/runtime/ownership/claim` — prove no prior socket owns a
   * live projection, then record the DAEMON's own.
   *
   * AC6: this took a `socketName`. It does not any more, and a caller cannot
   * name one: the owner recorded is the daemon's `ActuatorConfig::socket`,
   * which the operator client supplied once at daemon start. A per-request
   * name was unverifiable, could only ever produce a spurious refusal, and
   * over a released company could write an owner the daemon does not hold —
   * locking the daemon out of its own company. */
  async claimOwnership(input: { readonly slug: string }): Promise<RuntimeOwnershipResult> {
    return postOrgRoute<RuntimeOwnershipResult>(
      this.transport,
      this.url,
      '/v1/org/runtime/ownership/claim',
      { slug: this.slug(input.slug) }
    )
  }

  /** `POST /v1/org/runtime/ownership/release`. Same rule as the claim above. */
  async releaseOwnership(input: { readonly slug: string }): Promise<RuntimeOwnershipResult> {
    return postOrgRoute<RuntimeOwnershipResult>(
      this.transport,
      this.url,
      '/v1/org/runtime/ownership/release',
      { slug: this.slug(input.slug) }
    )
  }

  // TOMBSTONE (#751/P10): `closeTemporaryPane` dialed
  // `POST /v1/org/runtime/close-temporary-pane`, which no longer exists in any
  // Rust router. The capability was not moved — it DISSOLVED. It existed so
  // chiefd could move viewers off a temporary Founder/launcher pane and close
  // it, proving each of the caller's claims against tmux first. After the
  // actuation split chiefd cannot see a pane at all, and the operator client
  // owns both the session and the pane it would have been handing over, so the
  // whole handshake is a local operation with no second party.
  //
  // Deleted rather than left dangling: `RouteTableDerivation` caught this as
  // "a chiefing client dials a route no crate registers", and its own remedy is
  // the right one — delete the client method, or add the route in Rust. There
  // is nothing to add.

  // TOMBSTONE (chief-home-is-cwd §4d/§4e): the whole materialization client —
  // `materialize`, `ensureMaterializationCurrent`, `materializationIsStale` and
  // `extensionDrift`, dialling `POST /v1/org/materialize/{run,ensure-current,
  // stale,extension-drift}`, plus `installedResourceCatalog` on
  // `POST /v1/org/resource-catalog/read`.
  //
  // No home is projected from the manifest any more: `ensure_agent_home` writes
  // an agent home once, at hire, and a home that is never re-projected cannot
  // be stale and cannot drift. The resource catalog listed the skills a person
  // could be hired with, and nobody is hired with one — an agent's skills are
  // whatever is in `<dir>/.pi/skills` when Pi looks. Every one of those five
  // routes is deleted in chiefd, so these methods leave the same way
  // `closeTemporaryPane` did above: `RouteTableDerivation` says "delete the
  // client method, or add the route in Rust", and there is nothing to add.

  // TOMBSTONE: `runtimeExtensionDrift` and `deployDrift`, with their two
  // routes (`/v1/org/runtime/extension-drift`, `/v1/org/runtime/deploy-drift`).
  //
  // Both asked which RUNNING processes could not have loaded their current
  // extensions, and both answered from the actuator's observation. Their own
  // doc named the subtlety exactly: an empty `drift` list meant BOTH "nobody is
  // stale" and "nothing ever looked", and only `unobserved` told them apart.
  //
  // That distinction no longer has a referent, because the staleness is now
  // corrected rather than detected: the extension source digest is an input to
  // each person's derived launch hash, so a deploy moves the hash, the running
  // pane's tag stops matching, and the actuator replaces it on the next pass. A
  // stale pane cannot survive convergence, so there is no scan to run.

  // TOMBSTONE: the five company-session-action methods —
  // `queueCompanySessionAction`, `companySessionActionProgress`,
  // `unresolvedCompanySessionAction`, `skipParkedCompanySessionTargets` and
  // `reconcileCompanySessionActionClaims`.
  //
  // The chiefd half of #54's company-wide native reset and compact actions,
  // deleted whole with the feature. This client was the ONLY caller of the
  // queue verb anywhere in the tree, and its own callers were contract tests:
  // the historical queuer was the legacy CLI deleted in `ca2da9b57` and no
  // replacement ever arrived. A control plane for an action nothing could
  // create.
}
