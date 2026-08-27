// POST /api/companies — creating a company from the browser.
//
// The gap this closes was named for weeks in `ClientPathsAreServed`'s
// `UNSERVED_METHODS`: the page's create form dialled `POST /companies`, the
// path existed for GET, so the method-level hole answered 405 while every
// path-level check stayed green.
//
// What is pinned here is the SHAPE of the request that leaves this server —
// two strings and nothing else. The old route body required a `bootstrap`: a
// model route plus a fresh provider observation, which is a provider
// credential. Repairing that by giving this server one would have spread the
// secret to a third place to produce a fact chiefd already holds, so chiefd
// resolves it itself and this route must not start sending one again.
import { describe, expect, it, vi } from 'vitest'

const created = vi.fn()

vi.mock('@/server/CompanyLifecycle', () => ({
  createCompany: (input: unknown) => {
    created(input)
    return new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('event: created\ndata: {"slug":"acme"}\n\n'))
        controller.close()
      }
    })
  }
}))

const { POST } = await import('@/app/api/companies/route')

function post(body: unknown): Promise<Response> {
  return POST(
    new Request('http://web.test/api/companies', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      /* eslint-disable lucy/no-json-stringify */
      // A raw HTTP request body for the route under test, not an app-API call
      // — the same exemption the SSE frame builders take.
      body: JSON.stringify(body)
      /* eslint-enable lucy/no-json-stringify */
    })
  )
}

describe('POST /api/companies', () => {
  it('forwards the name and the purpose, and nothing else', async () => {
    created.mockReset()

    const response = await post({ name: 'Acme Anvils', purpose: 'Sell anvils to coyotes.' })

    expect(response.status).toBe(200)
    expect(response.headers.get('content-type')).toBe('text/event-stream')
    expect(created).toHaveBeenCalledWith({
      name: 'Acme Anvils',
      purpose: 'Sell anvils to coyotes.'
    })
    expect(await response.text()).toContain('event: created')
  })

  it('never forwards a caller-supplied bootstrap', async () => {
    // A browser cannot honestly produce one, and a route that passed one
    // through would put the choice of a company's model route back in the
    // hands of whoever POSTs to it.
    created.mockReset()

    await post({
      name: 'Acme Anvils',
      purpose: 'Sell anvils to coyotes.',
      bootstrap: { provider: 'somebody-elses', model: 'x' }
    })

    expect(created).toHaveBeenCalledWith({
      name: 'Acme Anvils',
      purpose: 'Sell anvils to coyotes.'
    })
  })

  it('refuses a body the form itself would not have submitted', async () => {
    // Reported as a STATUS, not as a `failed` frame: nothing has been narrated
    // yet, and a malformed body is the caller's mistake rather than a
    // lifecycle refusal. Filing it under the lifecycle heading would tell an
    // operator a launch failed when none was ever started.
    created.mockReset()

    const response = await post({ name: 'A', purpose: 'x' })

    expect(response.status).toBe(422)
    expect(created).not.toHaveBeenCalled()
  })
})
