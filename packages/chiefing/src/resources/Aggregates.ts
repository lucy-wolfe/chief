import { normalizeRowRead, postOrgRoute, withReadOpts } from '@/resources/OrgRoutes'
import type { ReadOpts, RowReadResult, WireRowRead } from '@/types/OrgDocs'
import type { HttpTransport } from '@/types/Transport'

/** Serialized-ledger strings; parsing stays with callers. Rust authority:
 * apps/chiefd/crates/chiefd-api/src/docstore/router.rs `org_activity_read`,
 * `org_supervision_read`, `session_maintenance_read`. Company-keyed via
 * `root`.
 *
 * READ-ONLY since the publisher-route sweep. The publish, publish-cas,
 * reconcile-structural and clear methods this class used to carry all dialled
 * routes that no caller anywhere in the tree ever posted, and both sides are
 * deleted together. */
export class AggregatesClient {
  constructor(
    protected readonly transport: HttpTransport,
    protected readonly url: string = ''
  ) {}

  async activityRead(slug: string, opts?: ReadOpts): Promise<RowReadResult> {
    const wire = await postOrgRoute<WireRowRead>(
      this.transport,
      this.url,
      '/v1/org/activity/read',
      withReadOpts({ slug }, opts)
    )
    return normalizeRowRead(wire)
  }

  async supervisionRead(slug: string, opts?: ReadOpts): Promise<RowReadResult> {
    const wire = await postOrgRoute<WireRowRead>(
      this.transport,
      this.url,
      '/v1/org/supervision/read',
      withReadOpts({ slug }, opts)
    )
    return normalizeRowRead(wire)
  }

  async sessionMaintenanceRead(slug: string, opts?: ReadOpts): Promise<RowReadResult> {
    const wire = await postOrgRoute<WireRowRead>(
      this.transport,
      this.url,
      '/v1/org/session-maintenance/read',
      withReadOpts({ slug }, opts)
    )
    return normalizeRowRead(wire)
  }
}
