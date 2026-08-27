// @vitest-environment jsdom
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { CompanyDirectory } from '@/components/companies/CompanyDirectory'
import { ChiefApiError } from '@/types/ApiErrors'

describe('CompanyDirectory', () => {
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
  })

  it('shows the empty-state invitation and preserves refusal detail verbatim', () => {
    act(() => {
      root.render(
        <CompanyDirectory
          companies={[]}
          error={
            new ChiefApiError({
              kind: 'conflict',
              status: 409,
              code: 'company-not-api-hosted',
              detail: 'company is hosted by tmux'
            })
          }
          loading={false}
          onBoot={async (): Promise<void> => undefined}
          onRetry={async (): Promise<unknown> => undefined}
          onStop={async (): Promise<void> => undefined}
        />
      )
    })

    expect(container.textContent).toContain('Create a company to get started.')
    expect(container.textContent).toContain('company is hosted by tmux')
  })

  it('renders a 503 as the chiefd-unreachable retryable banner', () => {
    let retries = 0
    act(() => {
      root.render(
        <CompanyDirectory
          companies={[]}
          error={
            new ChiefApiError({
              kind: 'upstream',
              status: 503,
              code: 'upstream-unreachable',
              detail: 'the daemon connection failed'
            })
          }
          loading={false}
          onBoot={async (): Promise<void> => undefined}
          onRetry={async (): Promise<unknown> => void (retries += 1)}
          onStop={async (): Promise<void> => undefined}
        />
      )
    })

    expect(container.textContent).toContain('chiefd unreachable')
    act(() => {
      container.querySelector('button')?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    expect(retries).toBe(1)
  })
})
