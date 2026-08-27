import { describe, expect, it } from 'vitest'

import {
  BEACOND_URL_ENV,
  beacondUrlFromEnvironment,
  DEFAULT_BEACOND_URL
} from '@/discovery/Company'

describe('beacondUrlFromEnvironment', () => {
  it('falls back to DEFAULT_BEACOND_URL when unset', () => {
    expect(beacondUrlFromEnvironment({})).toBe(DEFAULT_BEACOND_URL)
  })

  it('falls back to DEFAULT_BEACOND_URL when blank/whitespace', () => {
    expect(beacondUrlFromEnvironment({ [BEACOND_URL_ENV]: '' })).toBe(DEFAULT_BEACOND_URL)
    expect(beacondUrlFromEnvironment({ [BEACOND_URL_ENV]: '   ' })).toBe(DEFAULT_BEACOND_URL)
  })

  it('falls back to DEFAULT_BEACOND_URL for a non-http scheme', () => {
    expect(beacondUrlFromEnvironment({ [BEACOND_URL_ENV]: 'file:///etc/passwd' })).toBe(
      DEFAULT_BEACOND_URL
    )
    expect(beacondUrlFromEnvironment({ [BEACOND_URL_ENV]: 'ftp://example.com' })).toBe(
      DEFAULT_BEACOND_URL
    )
  })

  it('falls back to DEFAULT_BEACOND_URL for an unparseable value', () => {
    expect(beacondUrlFromEnvironment({ [BEACOND_URL_ENV]: 'not a url' })).toBe(DEFAULT_BEACOND_URL)
  })

  it('returns a valid http(s) URL trimmed', () => {
    expect(beacondUrlFromEnvironment({ [BEACOND_URL_ENV]: '  http://127.0.0.1:9999  ' })).toBe(
      'http://127.0.0.1:9999'
    )
    expect(beacondUrlFromEnvironment({ [BEACOND_URL_ENV]: 'https://beacond.internal' })).toBe(
      'https://beacond.internal'
    )
  })

  it('takes an explicit record — a same-named ambient variable is irrelevant', () => {
    // This is a pure function over its argument; PublicSurface.test.ts's
    // forbidden-primitives scan separately proves src/ never references the
    // ambient environment at all, so there is nothing to prove by mutating
    // it here too.
    expect(beacondUrlFromEnvironment({ [BEACOND_URL_ENV]: 'http://explicit:2' })).toBe(
      'http://explicit:2'
    )
  })
})
