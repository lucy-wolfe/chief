/**
 * Types for `test/harness/FakeChiefApi.ts`. Kept in a `/types/` directory
 * (matching `lucy/no-exported-type-outside-types-dir`, which applies to
 * test/ the same as src/) — the harness itself stays focused on the router.
 */
import type {
  CompaniesResponse,
  CompanySummary,
  CompanyTree,
  MailboxResponse,
  PeopleResponse,
  TranscriptResponse
} from '@/types/ChiefApi'
import type { LifecyclePhaseFrame } from '@/types/Sse'

export interface RecordedRequest {
  method: string
  path: string
  search: string
  headers: Record<string, string>
  body?: unknown
}

export interface FakeLifecycleScript {
  readonly phases: readonly LifecyclePhaseFrame[]
  readonly terminal:
    | { readonly event: 'created' | 'booted'; readonly slug: string }
    | {
        readonly event: 'failed'
        readonly error: { readonly code: string; readonly detail: string }
      }
}

export interface FakeChiefApiFixtures {
  /** `GET /companies` — apps/api serves a BARE ARRAY, so this fixture is one.
   * `GET /companies/:companyKey` serves the same `CompanySummary` shape
   * (apps/api's own `CompanyStatus = CompanySummary`), minus `url`.
   *
   * Every `Record` below is keyed by the company KEY — the handle the routes
   * resolve by — and never by the display slug. */
  companies: CompaniesResponse
  companyDetails: Record<string, CompanySummary>
  trees: Record<string, CompanyTree>
  people: Record<string, PeopleResponse>
  transcripts: Record<string, Record<string, TranscriptResponse>>
  mailboxes: Record<string, Record<string, MailboxResponse>>
  lifecycle?: {
    readonly create?: FakeLifecycleScript
    readonly boot?: Record<string, FakeLifecycleScript>
  }
}
