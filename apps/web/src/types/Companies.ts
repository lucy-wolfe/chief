/** View-model vocabulary for the companies directory.  The lifecycle wire
 * types remain owned by the SSE client; this module only gives the web UI
 * stable input and presentation shapes. */
import { z } from 'zod'

import { ChiefApiError } from '@/types/ApiErrors'
import type { CompanySummary } from '@/types/ChiefApi'
import type { LifecyclePhaseFrame, LifecycleTerminal } from '@/types/Sse'

export const COMPANY_NAME_MIN_LENGTH = 2
export const COMPANY_NAME_MAX_LENGTH = 120
export const COMPANY_PURPOSE_MIN_LENGTH = 4
export const COMPANY_PURPOSE_MAX_LENGTH = 2_000

/** `POST /api/companies`'s body, checked with the SAME bounds Founder's launch
 * tool declares.
 *
 * One set of numbers, read by both halves. The form these bounds were written
 * for is gone; the numbers are not, because the tool that replaced it declares
 * the identical minima and maxima to the model (`server/FounderAgent`'s
 * `LAUNCH_PARAMETERS`), and a route that invented different limits would refuse
 * a name the model was told it could send. */
export const CompanyCreateBodySchema = z.object({
  name: z.string().trim().min(COMPANY_NAME_MIN_LENGTH).max(COMPANY_NAME_MAX_LENGTH),
  purpose: z.string().trim().min(COMPANY_PURPOSE_MIN_LENGTH).max(COMPANY_PURPOSE_MAX_LENGTH)
})

export interface CompanyCreateInput {
  readonly name: string
  readonly purpose: string
}

export interface CompanyLifecycleFailure {
  readonly code: string
  readonly detail: string
}

export type CompanyLifecycleSuccess = Exclude<LifecycleTerminal, { kind: 'failed' }>

/** One lifecycle run the DIRECTORY page started, as its console renders it.
 *
 * `action` and `input` are gone. The page started two kinds of run while it
 * carried the create form; it starts exactly one now — a boot — because
 * creating a company is Founder's, on Founder's own page, with its own
 * reporting. A union member with no producer and a field nothing sets are not
 * free: they read as capability, and the next reader has to prove they are not
 * before touching anything near them. */
export interface CompanyLifecycleView {
  readonly runId: number
  readonly phases: readonly LifecyclePhaseFrame[]
  readonly terminal?: CompanyLifecycleSuccess
  readonly failure?: CompanyLifecycleFailure
  /** The company the run was started against, so Retry can start the same one.
   * A key, never the slug on `terminal`: that one is chiefd's lifecycle wire
   * answering in its own vocabulary and no route here accepts it. */
  readonly companyKey?: string
}

export interface CompanyDirectoryState {
  readonly companies: readonly CompanySummary[]
  readonly loading: boolean
  readonly error: unknown
  readonly refresh: () => Promise<readonly CompanySummary[]>
  readonly create: (
    input: CompanyCreateInput,
    onPhase: (frame: LifecyclePhaseFrame) => void
  ) => Promise<LifecycleTerminal>
  readonly boot: (
    companyKey: string,
    onPhase: (frame: LifecyclePhaseFrame) => void
  ) => Promise<LifecycleTerminal>
  readonly stop: (companyKey: string) => Promise<void>
}

/** The browser submits the minimal company spec; apps/api supplies the live
 * founder observation and determines the resulting slug. */
export function companyCreateRequest(input: CompanyCreateInput): {
  name: string
  purpose: string
} {
  // `{name, purpose}` and nothing else. This used to send a `{spec:{…}}`
  // envelope with no `founder` at all, which matched NO version of the api's
  // schema — every create from the browser failed validation before any
  // lifecycle code ran, which is why the web could never launch a company.
  // apps/api observes the founder route (it has the provider credential; the
  // browser does not) and chiefd derives the slug.
  return { name: input.name, purpose: input.purpose }
}

/** Preserve server-provided lifecycle failure text.  The fallback is only
 * for a malformed transport failure that supplied no typed envelope. */
export function lifecycleFailureFrom(error: unknown): CompanyLifecycleFailure {
  if (error instanceof ChiefApiError) {
    return {
      code: error.code ?? error.kind,
      detail: error.detail ?? error.message
    }
  }
  return {
    code: 'unexpected-error',
    detail: error instanceof Error ? error.message : String(error)
  }
}
