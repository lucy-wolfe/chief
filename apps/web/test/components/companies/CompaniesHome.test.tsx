// @vitest-environment jsdom
//
// # Why these two tests moved from CREATE to BOOT
//
// They were written against the name/purpose form, because that form was how
// this page started a lifecycle. The form is gone — a company is created by
// talking to Founder now — so the page starts exactly one lifecycle, and that
// is BOOT.
//
// What each test protects is unchanged and deliberately not weakened: an
// opaque phase the browser has no vocabulary for is still rendered verbatim,
// navigation still leaves the destination to one value rather than composing
// it out of what the page happens to be rendering, and a lifecycle
// 503 still reaches the retryable directory banner. Those are properties of
// the lifecycle console and the directory, and they were only ever reached
// through create by accident of which button existed.
//
// The third test is the front door itself.
import { act, type ReactNode } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

interface HoistedMocks {
  push: ReturnType<typeof vi.fn>
  directory: unknown
}

const mocks = vi.hoisted((): HoistedMocks => ({
  push: vi.fn(),
  directory: undefined
}))

vi.mock('next/navigation', () => ({
  useRouter: () => ({ push: mocks.push })
}))

vi.mock('@/providers/CompanyEventsProvider', () => ({
  CompanyEventsProvider: ({ children }: { children: ReactNode }) => children
}))

vi.mock('@/hooks/UseCompanyDirectory', () => ({
  useCompanyDirectory: () => mocks.directory
}))

import { CompaniesHome } from '@/components/companies/CompaniesHome'
import { ChiefApiError } from '@/types/ApiErrors'

async function flushWork(): Promise<void> {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

/** A directory with one stopped company, so the row renders a Boot button. */
function directoryWith(boot: unknown): unknown {
  return {
    companies: [
      {
        key: '0123456789ab',
        dir: '/work/acme',
        slug: 'acme',
        status: 'stopped',
        chiefd: { healthy: true, url: 'http://127.0.0.1:9001' }
      }
    ],
    loading: false,
    error: undefined,
    refresh: async () => [],
    create: async () => ({ kind: 'created' as const, slug: 'unused' }),
    boot,
    stop: async () => undefined
  }
}

function bootButton(container: HTMLElement): HTMLButtonElement {
  const button = [...container.querySelectorAll('button')].find(
    (candidate) => candidate.textContent === 'Boot'
  )
  if (!button) throw new Error('the boot button did not render')
  return button
}

describe('CompaniesHome', () => {
  let container: HTMLDivElement
  let root: Root

  beforeEach(() => {
    mocks.push.mockReset()
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
  })

  afterEach(() => {
    act(() => {
      root.unmount()
    })
    container.remove()
  })

  it('opens on Founder Mode, not on a form', () => {
    mocks.directory = directoryWith(async () => ({ kind: 'booted' as const, slug: 'unused' }))

    act(() => {
      root.render(<CompaniesHome />)
    })

    const founder = [...container.querySelectorAll('a')].find(
      (link) => link.textContent === 'Founder Mode'
    )
    expect(founder?.getAttribute('href')).toBe('/founder')
    // The form is GONE, not hidden. Its marker attribute and both of its
    // fields are the evidence: a page that still rendered them behind a flag
    // would pass a text assertion and still be a form.
    expect(container.querySelector('[data-company-create-form]')).toBeNull()
    expect(container.querySelector('input')).toBeNull()
    expect(container.querySelector('textarea')).toBeNull()
  })

  it('renders an opaque lifecycle phase and opens the booted company by KEY', async () => {
    const boot = vi.fn(
      async (_companyKey: string, onPhase: (frame: { phase: string; detail: string }) => void) => {
        onPhase({ phase: 'future-api-phase', detail: 'unmapped by the browser' })
        return { kind: 'booted' as const, slug: 'server-chosen-slug' }
      }
    )
    mocks.directory = directoryWith(boot)

    act(() => {
      root.render(<CompaniesHome />)
    })
    await act(async () => {
      bootButton(container).dispatchEvent(new Event('click', { bubbles: true }))
      await flushWork()
    })

    // Booted by KEY, not by the display word: two directories may hold
    // companies called `acme`, and the button has to name exactly one of them.
    expect(boot).toHaveBeenCalledWith('0123456789ab', expect.any(Function))
    expect(container.textContent).toContain('future-api-phase — unmapped by the browser')
    // The terminal frame's `slug` is chiefd's OWN answer off its slug-keyed
    // lifecycle wire, and `/c/:companyKey` accepts no such thing. The fixture
    // makes the two differ on purpose: the console shows the readable word and
    // the destination is the key the operator clicked.
    expect(mocks.push).toHaveBeenCalledWith('/c/0123456789ab')
    expect(container.textContent).toContain('Finished: server-chosen-slug')
  })

  it('surfaces a lifecycle transport 503 through the retryable directory banner', async () => {
    const boot = vi.fn(async (): Promise<never> => {
      throw new ChiefApiError({
        kind: 'upstream',
        status: 503,
        code: 'upstream-unreachable',
        detail: 'the company daemon could not be reached'
      })
    })
    mocks.directory = directoryWith(boot)

    act(() => {
      root.render(<CompaniesHome />)
    })
    await act(async () => {
      bootButton(container).dispatchEvent(new Event('click', { bubbles: true }))
      await flushWork()
    })

    expect(container.textContent).toContain('chiefd unreachable')
    expect(container.textContent).toContain(
      'upstream-unreachable: the company daemon could not be reached'
    )
    const retry = [...container.querySelectorAll('button')].find(
      (candidate) => candidate.textContent === 'Retry'
    )
    expect(retry).toBeDefined()
  })
})
