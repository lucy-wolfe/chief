// @vitest-environment jsdom
import { act, createElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('@/components/companies/CompaniesHome', () => ({
  CompaniesHome: () => createElement('div', { 'data-companies-home': 'true' }, 'Companies home')
}))

import CompaniesPage from '@/app/page'

describe('CompaniesPage', () => {
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

  it('mounts the companies directory surface with no console errors', () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {})

    act(() => {
      root.render(<CompaniesPage />)
    })

    expect(container.querySelector('[data-companies-home="true"]')?.textContent).toBe(
      'Companies home'
    )
    expect(errorSpy).not.toHaveBeenCalled()

    errorSpy.mockRestore()
  })
})
