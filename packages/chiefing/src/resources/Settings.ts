import { postOrgRoute, postOrgRouteVoid } from '@/resources/OrgRoutes'
import type { OrgSettings } from '@/types/Settings'
import type { HttpTransport } from '@/types/Transport'

interface OrgSettingsDto {
  launcherRoot?: string
  supervisionIntervalMs: number
  acknowledgementTimeoutMs: number
  acknowledgementRetryLimit: number
  replacementLimit: number
}

interface OrgSettingsReadResponse {
  found: boolean
  settings?: OrgSettingsDto
}

/** The `org_settings` singleton client (E7-S3, #818). Replaces
 * `state/launcher.json`: `launcherRoot` is a column on the same durable row
 * the four supervision policy ints already live on, published by chiefd's
 * `org_settings_publish_launcher_root` (the four ints stay owned by their own
 * genesis/policy paths -- `publishLauncherRoot` writes ONLY `launcherRoot`,
 * matching the Rust route it drives). */
export class SettingsClient {
  constructor(
    protected readonly transport: HttpTransport,
    protected readonly url: string = ''
  ) {}

  /** `undefined` when the company is unknown OR has no settings row yet
   * (genesis not run) -- both are `found:false` on the wire, and neither is
   * an error: a caller asking "what's the launcher root" for a company that
   * does not durably exist yet gets an honest absence, not a refusal. */
  async read(slug: string): Promise<OrgSettings | undefined> {
    const response = await postOrgRoute<OrgSettingsReadResponse>(
      this.transport,
      this.url,
      '/v1/org/settings/read',
      { slug }
    )
    if (!response.found || !response.settings) return undefined
    return {
      launcherRoot: response.settings.launcherRoot,
      supervisionIntervalMs: response.settings.supervisionIntervalMs,
      acknowledgementTimeoutMs: response.settings.acknowledgementTimeoutMs,
      acknowledgementRetryLimit: response.settings.acknowledgementRetryLimit,
      replacementLimit: response.settings.replacementLimit
    }
  }

  /** `POST /v1/org/settings/publish` -- an `unknown-company`/`UNKNOWN_COMPANY`
   * refusal (genesis has not run) surfaces as `OrgRowRefusalError`, the same
   * shape every other `/v1/org/*` route in this family uses. */
  async publishLauncherRoot(input: {
    slug: string
    at: string
    launcherRoot: string
  }): Promise<void> {
    await postOrgRouteVoid(this.transport, this.url, '/v1/org/settings/publish', {
      slug: input.slug,
      at: input.at,
      launcherRoot: input.launcherRoot
    })
  }
}
