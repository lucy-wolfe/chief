import { postOrgRoute } from '@/resources/OrgRoutes'
import type {
  ActivityCommandStatus,
  BuildPersonContractsResult,
  CompanyTreeResult,
  InScopeResult,
  OrganizationLifecycleStatus,
  StaffingLifecycleResult,
  TreeLinesResult,
  UnitRemovalImpact,
  UnitRemovalPreview,
  UnitSubtreeResult
} from '@/types/OrgSlice'
import type { HttpTransport } from '@/types/Transport'

/** Rust authority: apps/chiefd/crates/chiefd-api/src/docstore/org_slice.rs.
 * The activity / staffing-lifecycle / units / cold-start /
 * caller-authorization / control-authority / person-contracts route family —
 * every handler here replaces a TypeScript
 * function that used to make the same decision in
 * `apps/cli/src/legacy/organization/` (now deleted). Company-keyed via
 * `root` exactly like every other resource in this story (Contract's
 * "Company keying" rule). Every route's request struct is
 * `#[serde(deny_unknown_fields)]`, so the request bodies below name fields
 * precisely — an extra or misspelled field is a hard 400. Every refusal
 * decodes through the shared `postOrgRoute` dispatch (`resources/OrgRoutes`):
 * a 400/404/422 throws `OrgRowRefusalError`, anything else throws
 * `ChiefdUnavailableError`. */
export class OrgSliceClient {
  constructor(
    protected readonly transport: HttpTransport,
    protected readonly url: string = ''
  ) {}

  /** `POST /v1/org/lifecycle-status/read` — the read-only up/down control
   * board. Every durable source but the manifest degrades into a warning
   * rather than an error. */
  async lifecycleStatus(
    slug: string,
    opts: { scopeDepartmentId?: string; maxPeople?: number } = {}
  ): Promise<OrganizationLifecycleStatus> {
    return postOrgRoute(this.transport, this.url, '/v1/org/lifecycle-status/read', {
      slug,
      scopeDepartmentId: opts.scopeDepartmentId,
      maxPeople: opts.maxPeople
    })
  }

  /** `POST /v1/org/tree/read` — the operator's ASCII organization tree, one
   * line per unit. */
  async treeLines(slug: string): Promise<TreeLinesResult> {
    return postOrgRoute(this.transport, this.url, '/v1/org/tree/read', { slug })
  }

  /** `POST /v1/org/unit/subtree` — the unit plus every descendant, in
   * canonical `departmentOrder`. */
  /**
   * `POST /v1/org/tree/structured` — the company as a forest: departments
   * nested by parent, each carrying the people assigned to it.
   *
   * The sibling of [`treeLines`], which answers the same question for a
   * terminal by returning ASCII lines. A browser needs the STRUCTURE, and
   * building it client-side from a manifest is the projection-in-a-client
   * mandate 3 forbids.
   */
  async treeStructured(slug: string): Promise<CompanyTreeResult> {
    return postOrgRoute(this.transport, this.url, '/v1/org/tree/structured', {
      slug
    })
  }

  async unitSubtree(slug: string, unitId: string): Promise<UnitSubtreeResult> {
    return postOrgRoute(this.transport, this.url, '/v1/org/unit/subtree', {
      slug,
      unitId
    })
  }

  /** `POST /v1/org/unit/removal-impact` — exactly who a unit removal would
   * fire, without touching the database. */
  async unitRemovalImpact(slug: string, unitId: string): Promise<UnitRemovalImpact> {
    return postOrgRoute(this.transport, this.url, '/v1/org/unit/removal-impact', {
      slug,
      unitId
    })
  }

  /** `POST /v1/org/unit/removal-preview` — build and validate the exact
   * recursive-removal result without writing. */
  async unitRemovalPreview(slug: string, unitId: string, at?: string): Promise<UnitRemovalPreview> {
    return postOrgRoute(this.transport, this.url, '/v1/org/unit/removal-preview', {
      slug,
      unitId,
      at
    })
  }

  /** `POST /v1/org/activity/command-status` — every handoff the
   * authenticated caller still owes, plus the exact pending authority. */
  async activityCommandStatus(
    slug: string,
    callerPersonId: string
  ): Promise<ActivityCommandStatus> {
    return postOrgRoute(this.transport, this.url, '/v1/org/activity/command-status', {
      slug,
      callerPersonId
    })
  }

  /** `POST /v1/org/staffing/lifecycle` — run one staffing lifecycle action
   * end to end. The runtime is NOT converged inline: the mutation lands and
   * chiefd's reconcile loop is woken, which is what moves or tears down the
   * pane. */
  async staffingLifecycle(
    slug: string,
    action: 'bench' | 'transfer' | 'offboard',
    personId: string,
    opts: { toDepartmentId?: string; reason?: string } = {}
  ): Promise<StaffingLifecycleResult> {
    return postOrgRoute(this.transport, this.url, '/v1/org/staffing/lifecycle', {
      slug,
      action,
      personId,
      toDepartmentId: opts.toDepartmentId,
      reason: opts.reason
    })
  }

  /** `POST /v1/org/control-authority/person-in-scope` — whether the actor
   * may act on the target person. An absent `actorPersonId` is the human
   * operator: full scope by construction, earned from pane ownership before
   * the request was ever made. */
  async personInScope(
    slug: string,
    targetPersonId: string,
    actorPersonId?: string
  ): Promise<InScopeResult> {
    return postOrgRoute(this.transport, this.url, '/v1/org/control-authority/person-in-scope', {
      slug,
      actorPersonId,
      targetPersonId
    })
  }

  /** `POST /v1/org/control-authority/department-in-scope` — whether the
   * actor manages the unit. */
  async departmentInScope(
    slug: string,
    actorPersonId: string,
    departmentId: string
  ): Promise<InScopeResult> {
    return postOrgRoute(this.transport, this.url, '/v1/org/control-authority/department-in-scope', {
      slug,
      actorPersonId,
      departmentId
    })
  }

  /** `POST /v1/org/person-contracts/build` — rebuild and publish every
   * person's operating contract. `published: false` means nothing changed,
   * and nothing was written. */
  async buildPersonContracts(slug: string): Promise<BuildPersonContractsResult> {
    return postOrgRoute(this.transport, this.url, '/v1/org/person-contracts/build', {
      slug
    })
  }
}
