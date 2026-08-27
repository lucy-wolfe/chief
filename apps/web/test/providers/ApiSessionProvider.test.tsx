// @vitest-environment jsdom
import { act, createElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { ApiSessionProvider, useAccessToken, useChiefApi } from '@/providers/ApiSessionProvider'
import { ChiefApiClientService } from '@/services/ChiefApiClientService'

interface ProviderBox {
  client: ChiefApiClientService | undefined
  token: string | null | undefined
}

function Harness({ box }: { box: ProviderBox }): null {
  box.client = useChiefApi()
  box.token = useAccessToken()()
  return null
}

describe('ApiSessionProvider', () => {
  let container: HTMLDivElement
  let root: Root

  beforeEach(() => {
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
  })

  afterEach(() => {
    act(() => root.unmount())
    container.remove()
  })

  it('hands consumers the exact client and token it was given', () => {
    // The provider's whole job: no construction of its own, no ambient token.
    // A provider that built its own client would give one consumer a
    // different one from another, each with its own auth state.
    const client = new ChiefApiClientService({
      baseUrl: 'http://fake-api.test',
      fetchImpl: async () => new Response('{}', { status: 200 })
    })
    const box: ProviderBox = { client: undefined, token: undefined }

    act(() => {
      root.render(
        createElement(ApiSessionProvider, {
          client,
          tokenGetter: () => 'injected-token',
          children: createElement(Harness, { box })
        })
      )
    })

    expect(box.client).toBe(client)
    expect(box.token).toBe('injected-token')
  })
})
