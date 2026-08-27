// Resolving a company's chiefd, and the two failures that must stay distinct.
//
// "beacond has never heard of this company" and "beacond knows it but nothing
// is running" call for different actions from whoever hit them — the first is a
// stale link or a company that was never created, the second is a company to
// start. Collapsing them into one error is how an operator ends up trying to
// restart something that never existed, so each carries its own status and
// code.
//
// The company is addressed by its KEY, `sha256(dir)[..12]`, and resolved on the
// registry LIST. Not by slug — two directories may hold companies with the same
// display word, and a slug-keyed resolver answers with whichever the registry
// listed first. Not by `GET /v1/lookup?dir=` either: that takes the company's
// directory, which only a process standing IN it knows, and a server rendering
// companies for an operator does not stand anywhere.
import { afterEach, describe, expect, it, vi } from 'vitest'

import { companyChiefd, CompanyUnavailableError } from '@/server/CompanyChiefd'
import type { FetchImpl } from '@/types/Fetch'

/** A beacond that answers `GET /v1/list` with whatever body is given. */
function fakeBeacond(body: string): FetchImpl {
  const impl: FetchImpl = async (input) => {
    const url = new URL(input instanceof Request ? input.url : String(input))
    if (url.pathname === '/v1/list') {
      return new Response(body, {
        status: 200,
        headers: { 'content-type': 'application/json' }
      })
    }
    return new Response('unexpected', { status: 500 })
  }
  return impl
}

const KEY = '0123456789ab'

// Written as the WIRE, not built from an object: this fixture's whole job is
// to assert a shape beacond actually sends, and a serialized object would let
// a field rename pass here while breaking the real client.
const RUNNING = [
  '{"companies":[{',
  `"dir":"/work/cobalt","key":"${KEY}","slug":"cobalt",`,
  '"registeredAt":"2026-08-09T00:00:00.000Z",',
  '"url":"http://127.0.0.1:8792","port":8792,"pid":11,',
  '"hostname":"fixture","lastSeenAt":"2026-08-09T00:00:00.000Z"}]}'
].join('')

/** Registered, never started: beacond keeps the row, with no location. */
const REGISTERED_NOT_RUNNING = [
  '{"companies":[{',
  `"dir":"/work/cobalt","key":"${KEY}","slug":"cobalt",`,
  '"registeredAt":"2026-08-09T00:00:00.000Z"}]}'
].join('')

/** TWO DIRECTORIES, ONE DISPLAY WORD — the pair a slug-keyed resolver could
 * not tell apart, listed side by side as beacond now lists them. */
const TWO_ACMES = [
  '{"companies":[{',
  '"dir":"/elsewhere/acme","key":"cafebabe0011","slug":"acme",',
  '"registeredAt":"2026-08-09T00:00:00.000Z",',
  '"url":"http://127.0.0.1:9001","port":9001,"pid":21,',
  '"hostname":"fixture","lastSeenAt":"2026-08-09T00:00:00.000Z"},{',
  '"dir":"/work/acme","key":"00ff00ff00ff","slug":"acme",',
  '"registeredAt":"2026-08-09T00:00:00.000Z",',
  '"url":"http://127.0.0.1:9002","port":9002,"pid":22,',
  '"hostname":"fixture","lastSeenAt":"2026-08-09T00:00:00.000Z"}]}'
].join('')

describe('companyChiefd', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('returns a client pointed at the running company’s own daemon', async () => {
    vi.stubGlobal('fetch', fakeBeacond(RUNNING))

    const client = await companyChiefd(KEY)

    // The address came from beacond, not from configuration — that is the
    // whole point of resolving per request.
    expect(client.url).toBe('http://127.0.0.1:8792')
  })

  /**
   * THE PAIR THE PREDECESSOR COULD NOT ADDRESS.
   *
   * Both rows say `acme`. A resolver that matched the display word would have
   * one answer for two companies and would hand the operator whichever came
   * first — silently, because the wrong daemon answers 200.
   */
  it('addresses two same-named companies separately, by key', async () => {
    vi.stubGlobal('fetch', fakeBeacond(TWO_ACMES))

    expect((await companyChiefd('cafebabe0011')).url).toBe('http://127.0.0.1:9001')
    expect((await companyChiefd('00ff00ff00ff')).url).toBe('http://127.0.0.1:9002')
  })

  it('refuses an unknown key as 404 unknown-company', async () => {
    vi.stubGlobal('fetch', fakeBeacond(`{"companies":[]}`))

    const error = await companyChiefd('deadbeef0000').then(
      () => undefined,
      (caught: unknown) => caught
    )

    expect(error).toBeInstanceOf(CompanyUnavailableError)
    if (!(error instanceof CompanyUnavailableError)) throw new Error('narrowing')
    expect(error.status).toBe(404)
    expect(error.code).toBe('unknown-company')
  })

  it('refuses a registered-but-stopped company as 409, naming how to start it', async () => {
    vi.stubGlobal('fetch', fakeBeacond(REGISTERED_NOT_RUNNING))

    const error = await companyChiefd(KEY).then(
      () => undefined,
      (caught: unknown) => caught
    )

    expect(error).toBeInstanceOf(CompanyUnavailableError)
    if (!(error instanceof CompanyUnavailableError)) throw new Error('narrowing')
    expect(error.status).toBe(409)
    expect(error.code).toBe('company-not-running')
    // The message has to be actionable: an operator reading it should not have
    // to go looking for the command — and the command is now "run chief in that
    // directory", so the message must name the directory.
    expect(error.message).toContain('/work/cobalt')
    expect(error.message).toContain('running chief in that directory')
  })
})
