import { isNullish } from '@/Nullish'
import { nowIso, postOrgRoute } from '@/resources/OrgRoutes'
import type { OrganizationManifest, OrganizationSpec } from '@/types/Organization'
import type { HttpTransport } from '@/types/Transport'

/** Rust authority: apps/chiefd/crates/chiefd-api/src/docstore/router.rs
 * `org_manifest_read`/`org_manifest_genesis`. Company-keyed via
 * `root` exactly like every other resource in this story (Contract's
 * "Company keying" rule). */
export class ManifestClient {
  constructor(
    protected readonly transport: HttpTransport,
    protected readonly url: string = ''
  ) {}

  async read(slug: string): Promise<{ manifest: string } | undefined> {
    const wire = await postOrgRoute<{ found: boolean; manifest?: string }>(
      this.transport,
      this.url,
      '/v1/org/manifest/read',
      { slug }
    )
    if (!wire.found || isNullish(wire.manifest)) return undefined
    return { manifest: wire.manifest }
  }

  /**
   * The manifest, decoded.
   *
   * `read` returns the serialized document because most callers forward it
   * verbatim. The launcher's callers want the object, and used to get it from
   * `loadOrganization` (`org-store.ts`, deleted), which also *validated* the
   * manifest in TypeScript. That validation is chiefd's now
   * (`validate_organization_manifest`), and chiefd will not serve a manifest
   * that fails it — so this only decodes. Re-checking here would be a second
   * implementation of a rule that already has one.
   */
  async readManifest(slug: string): Promise<OrganizationManifest | undefined> {
    const wire = await this.read(slug)
    if (isNullish(wire)) return undefined
    return JSON.parse(wire.manifest)
  }

  /**
   * Create a company from its SPEC.
   *
   * The wire used to carry a pre-normalized `OrganizationManifest` string plus
   * a person-contracts document — artifacts this package's caller had to
   * build, which meant TypeScript
   * decided every id, default tool grant, employment state and unit
   * relationship and chiefd merely stored the answer. #751 moved the builders
   * into Rust, so the request now carries the question: the spec and event
   * clock.
   *
   * `at` defaults to the current wall clock when the caller has no more
   * precise event stamp.
   */
  async genesis(
    slug: string,
    spec: OrganizationSpec,
    opts: { at?: string } = {}
  ): Promise<boolean> {
    const wire = await postOrgRoute<{ created: boolean }>(
      this.transport,
      this.url,
      '/v1/org/manifest/genesis',
      {
        slug,
        spec,
        at: opts.at ?? nowIso()
      }
    )
    return wire.created
  }
}
