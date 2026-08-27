// @vitest-environment jsdom
import { ACME_DAEMON_URL, GLOBEX_DAEMON_URL } from '@test/harness/FakeChiefApi'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { CompanyRow } from '@/components/companies/CompanyRow'
import type { CompanySummary } from '@/types/ChiefApi'

function buttonsFor(container: HTMLDivElement): HTMLButtonElement[] {
  const buttons: HTMLButtonElement[] = []
  for (const candidate of container.querySelectorAll('button')) {
    if (candidate instanceof HTMLButtonElement) buttons.push(candidate)
  }
  return buttons
}

function labelledText(container: HTMLDivElement, label: string): string | null {
  const element = container.querySelector(`[aria-label="${label}"]`)
  return element instanceof HTMLElement ? element.textContent : null
}

/** apps/api's `CompanySummary`, field for field. This fixture used to carry
 * `name`, `hosting`, `peopleCount`, `departmentCount` and a `chiefd.url` —
 * five fields `GET /companies` has never served — which is why every
 * assertion below had to be rewritten rather than adjusted. */
function company(overrides: Partial<CompanySummary> = {}): CompanySummary {
  return {
    key: '0123456789ab',
    dir: '/work/acme',
    slug: 'acme',
    status: 'running',
    url: ACME_DAEMON_URL,
    chiefd: { healthy: true, httpStatus: 200, reason: 'ok', runtimeMode: 'company' },
    ...overrides
  }
}

describe('CompanyRow', () => {
  let container: HTMLDivElement
  let root: Root

  beforeEach(() => {
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
  })

  afterEach(() => {
    act(() => {
      root.unmount()
    })
    container.remove()
    vi.unstubAllGlobals()
  })

  it('identifies a running company by slug, shows health, and never prints its daemon address', () => {
    act(() => {
      root.render(
        <CompanyRow
          company={company()}
          onBoot={async (): Promise<void> => undefined}
          onStop={async (): Promise<void> => undefined}
        />
      )
    })

    expect(buttonsFor(container).map((button) => button.textContent)).toEqual(['Stop'])
    expect(labelledText(container, 'chiefd healthy')).toBe('●')
    // The slug is the whole HEADING: apps/api serves no company name here, so
    // the readable word is all a person gets. (What the row ACTS by is the
    // key — see the Open/Boot/Stop assertions.) This replaces
    // `toContain('×7 people · ×3 departments')`, a line built from
    // `peopleCount`/`departmentCount` that no route serves.
    expect(container.querySelector('h3')?.textContent).toBe('acme')
    // Ruling D1/D2 — the walked url is decoded and never rendered or dialed.
    expect(container.textContent).not.toContain(ACME_DAEMON_URL)
  })

  it('shows boot only for a stopped company', () => {
    const boot = vi.fn(async (): Promise<void> => undefined)
    act(() => {
      root.render(
        <CompanyRow
          company={company({ status: 'stopped', chiefd: { healthy: false } })}
          onBoot={boot}
          onStop={async (): Promise<void> => undefined}
        />
      )
    })

    const buttons = buttonsFor(container)
    expect(buttons.map((button) => button.textContent)).toEqual(['Boot'])
    const [button] = buttons
    if (!button) throw new Error('Boot button was not rendered')
    act(() => {
      button.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    // By KEY, not by the display word this row SHOWS.
    expect(boot).toHaveBeenCalledWith('0123456789ab')
  })

  // REWRITTEN CONTRACT. This case used to be "keeps tmux-hosted rows visible
  // but disables both lifecycle controls with the refusal reason": it rendered
  // `hosting: 'tmux'` and asserted both buttons were disabled with the title
  // `company-not-api-hosted: company is hosted by tmux`. `GET /companies` does
  // not serve `hosting`, so the row can never know, and `company-not-api-hosted`
  // is not a lifecycle-route refusal at all — it is the 409 apps/api's LIVE
  // verbs raise. The behavior that survives is the one the served fields can
  // actually support: a registered company whose chiefd did not answer reads
  // as stopped, shows an unhealthy marker, and offers Boot.
  it('renders a registered-but-unreachable company as stopped and offers Boot', () => {
    act(() => {
      root.render(
        <CompanyRow
          company={company({
            slug: 'globex',
            status: 'stopped',
            url: GLOBEX_DAEMON_URL,
            chiefd: { healthy: false, httpStatus: 503, reason: 'probe failed' }
          })}
          onBoot={async (): Promise<void> => undefined}
          onStop={async (): Promise<void> => undefined}
        />
      )
    })

    const buttons = buttonsFor(container)
    expect(buttons.map((button) => button.textContent)).toEqual(['Boot'])
    for (const button of buttons) expect(button.disabled).toBe(false)
    expect(labelledText(container, 'chiefd unhealthy')).toBe('○')
    expect(container.textContent).not.toContain(GLOBEX_DAEMON_URL)
  })

  it('asks for confirmation before it stops a running company', () => {
    const stop = vi.fn(async (): Promise<void> => undefined)
    const confirm = vi.fn(() => true)
    vi.stubGlobal('confirm', confirm)
    act(() => {
      root.render(
        <CompanyRow
          company={company()}
          onBoot={async (): Promise<void> => undefined}
          onStop={stop}
        />
      )
    })

    const [button] = buttonsFor(container)
    if (!button) throw new Error('Stop button was not rendered')
    act(() => {
      button.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    // Two DIFFERENT handles, and the split is the point: the prompt names the
    // SLUG because that is the word a person recognises (it used to read
    // `Stop Acme?` from a `name` field), and `onStop` is given the KEY because
    // that is what addresses one company rather than every company sharing a
    // display word.
    expect(confirm).toHaveBeenCalledWith('Stop acme?')
    expect(stop).toHaveBeenCalledWith('0123456789ab')
  })
})
