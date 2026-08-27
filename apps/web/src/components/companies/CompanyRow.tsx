'use client'

import type { ReactElement } from 'react'

import type { CompanySummary } from '@/types/ChiefApi'

interface CompanyRowProps {
  readonly company: CompanySummary
  readonly onBoot: (companyKey: string) => Promise<void>
  readonly onStop: (companyKey: string) => Promise<void>
}

/** One row of `GET /companies`, rendering exactly the four facts that route
 * serves: the slug, the running/stopped status, and chiefd's probed health.
 *
 * FOUR THINGS THIS ROW USED TO RENDER ARE GONE, because apps/api's
 * `CompanySummary` has never carried them and no other route serves them
 * per-company either (mandate 0 — drop the feature rather than invent the
 * field or fall back to a placeholder):
 *
 * - `company.name` — a company's only served identity in the directory is
 *   its KEY, shown as its SLUG. The human name lives on the root department
 *   and comes from `GET /companies/:companyKey/tree`, which the directory does
 *   not fetch.
 * - `company.hosting` — the api/tmux/none badge, and with it the
 *   "tmux-hosted rows show disabled Boot/Stop buttons with a
 *   company-not-api-hosted tooltip" behavior. `CompanyHosting` exists as an
 *   apps/api type and `CompanyDirectoryService.hosting()` computes it, but
 *   NO route exposes it. Boot and Stop are now driven by `status` alone,
 *   which is the fact the route actually reports.
 * - `company.peopleCount` / `company.departmentCount` — the "×N people · ×N
 *   departments" line. Both are only derivable from the per-company tree. */
export function CompanyRow({ company, onBoot, onStop }: CompanyRowProps): ReactElement {
  const bootVisible = company.status === 'stopped'
  const stopVisible = company.status === 'running'
  const requestStop = (): void => {
    if (!window.confirm(`Stop ${company.slug}?`)) return
    void onStop(company.key)
  }

  return (
    <article data-company-row={company.slug} className="chief-company-row">
      <h3 className="chief-company-row__slug">{company.slug}</h3>
      <span
        data-company-status={company.status}
        className={`chief-pill chief-pill--${company.status}`}
      >
        {company.status}
      </span>
      <span
        aria-label={company.chiefd.healthy ? 'chiefd healthy' : 'chiefd unhealthy'}
        className={`chief-dot chief-dot--${company.chiefd.healthy ? 'healthy' : 'unhealthy'}`}
      >
        {company.chiefd.healthy ? '\u25cf' : '\u25cb'}
      </span>
      <div className="chief-row-actions">
        <a
          href={`/c/${encodeURIComponent(company.key)}`}
          className="chief-link-button chief-link-button--primary"
        >
          Open
        </a>
        {bootVisible ? (
          <button
            onClick={(): void => void onBoot(company.key)}
            type="button"
            className="chief-button"
          >
            Boot
          </button>
        ) : null}
        {stopVisible ? (
          <button onClick={requestStop} type="button" className="chief-button chief-button--danger">
            Stop
          </button>
        ) : null}
      </div>
    </article>
  )
}
