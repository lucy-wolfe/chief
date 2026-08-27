import { ChiefdUnavailableError, PersonContractsRefusalError } from '@/Errors'
import { isNullish } from '@/Nullish'
import { decodeRefusal, isRefusalStatus } from '@/resources/OrgRoutes'
import type { OrganizationPersonContractsDocument } from '@/types/PersonContracts'
import type { HttpTransport } from '@/types/Transport'

/** Company-keyed via client root (matches today's factory — the root-bound
 * rewrite — org-person-contracts-rows.ts:90-92). Uses its own refusal class
 * (`PersonContractsRefusalError`), never `OrgRowRefusalError` — the one
 * resource family in this story with a distinct error type per the Contract. */
export class PersonContractsClient {
  constructor(
    protected readonly transport: HttpTransport,
    protected readonly url: string = ''
  ) {}

  private async post<T>(path: string, body: unknown): Promise<T> {
    const response = await this.transport.post(path, body)
    if (response.status >= 200 && response.status < 300) {
      try {
        return JSON.parse(response.body)
      } catch (cause) {
        throw new ChiefdUnavailableError({
          kind: 'malformed-body',
          url: this.url,
          path,
          status: response.status,
          cause
        })
      }
    }
    if (isRefusalStatus(response.status)) {
      const { code, detail } = decodeRefusal(response.body)
      throw new PersonContractsRefusalError({ code, detail })
    }
    throw new ChiefdUnavailableError({
      kind: 'http-error',
      url: this.url,
      path,
      status: response.status
    })
  }

  async read(
    slug: string
  ): Promise<{ found: boolean; document?: OrganizationPersonContractsDocument }> {
    const wire = await this.post<{ found: boolean; contracts?: string }>(
      '/v1/org/person-contracts/read',
      { slug }
    )
    if (!wire.found || isNullish(wire.contracts)) return { found: false }
    return {
      found: true,
      document: JSON.parse(wire.contracts)
    }
  }

  /**
   * `POST /v1/org/person-contracts/projection-plan` (E7-S3, #818) -- the
   * MD5-vs-stored-contract comparison moved into Rust; TS sends what it
   * observed on disk (or `null` for missing/unreadable) per person and gets
   * back a per-person `write` (with the text to overwrite the file with) or
   * `keep`. Zero comparison logic on this side.
   */
  async projectionPlan(input: {
    slug: string
    observed: ReadonlyArray<{ personId: string; md5: string | null }>
  }): Promise<ReadonlyArray<{ personId: string; action: 'write' | 'keep'; text?: string }>> {
    const wire = await this.post<{
      actions: Array<{ personId: string; action: 'write' | 'keep'; text?: string }>
    }>('/v1/org/person-contracts/projection-plan', {
      slug: input.slug,
      observed: input.observed.map((o) => ({ personId: o.personId, md5: o.md5 }))
    })
    return wire.actions
  }
}
