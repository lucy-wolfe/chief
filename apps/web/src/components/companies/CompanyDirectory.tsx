'use client'

import type { ReactElement } from 'react'

import { CompanyRow } from '@/components/companies/CompanyRow'
import { ChiefApiError } from '@/types/ApiErrors'
import type { CompanySummary } from '@/types/ChiefApi'

interface CompanyDirectoryProps {
  readonly companies: readonly CompanySummary[]
  readonly loading: boolean
  readonly error: unknown
  readonly onBoot: (companyKey: string) => Promise<void>
  readonly onStop: (companyKey: string) => Promise<void>
  readonly onRetry: () => Promise<unknown>
}

function errorDetail(error: unknown): string {
  if (error instanceof ChiefApiError) {
    if (error.status === 503) return 'chiefd unreachable'
    return error.detail ?? error.message
  }
  return error instanceof Error ? error.message : String(error)
}

function isChiefdUnavailable(error: unknown): boolean {
  return error instanceof ChiefApiError && error.status === 503
}

export function CompanyDirectory({
  companies,
  loading,
  error,
  onBoot,
  onStop,
  onRetry
}: CompanyDirectoryProps): ReactElement {
  return (
    <section aria-label="Companies" data-company-directory="true">
      <h1>Companies</h1>
      {loading && companies.length === 0 ? <p className="chief-empty">Loading companies…</p> : null}
      {typeof error !== 'undefined' ? (
        <div role="alert" className="chief-error">
          <p>{errorDetail(error)}</p>
          {isChiefdUnavailable(error) ? (
            <button onClick={(): void => void onRetry()} type="button" className="chief-button">
              Retry
            </button>
          ) : null}
        </div>
      ) : null}
      {!loading && companies.length === 0 ? (
        <p className="chief-empty">Create a company to get started.</p>
      ) : null}
      <div className="chief-company-list">
        {/* Keyed by the company KEY: two directories may hold companies with
            the same slug, and React would then reconcile two rows as one. */}
        {companies.map((company) => (
          <CompanyRow company={company} key={company.key} onBoot={onBoot} onStop={onStop} />
        ))}
      </div>
    </section>
  )
}
