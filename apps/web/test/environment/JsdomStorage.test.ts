// @vitest-environment jsdom
// Guards the environment every other jsdom test in this package stands on.
// Node 26 keeps `localStorage`/`sessionStorage` on the global and both read as
// `undefined` without `--localstorage-file`; Vitest's jsdom environment then
// refuses to copy jsdom's real `Storage` over a name the global already holds,
// so `localStorage` is `undefined` inside every jsdom test. That made
// test/services/SessionClientService.test.ts fail 3 of 6 on Node 26 and pass in
// CI, whose Node is older — a red that hid for an unknown time. The cure is
// `execArgv: ['--no-experimental-webstorage']` in vitest.config.ts. Remove it
// and this file fails first, and says why.
import { afterEach, describe, expect, it } from 'vitest'

describe('the jsdom environment owns the Web Storage globals', () => {
  afterEach(() => {
    localStorage.clear()
    sessionStorage.clear()
  })

  it("exposes a live localStorage, not Node's dead one", () => {
    expect(localStorage).toBeDefined()
    expect(localStorage.length).toBe(0)
    localStorage.setItem('probe', 'value')
    expect(localStorage.getItem('probe')).toBe('value')
    expect(localStorage.length).toBe(1)
    localStorage.clear()
    expect(localStorage.length).toBe(0)
  })

  it('exposes a live sessionStorage that is a different store', () => {
    expect(sessionStorage).toBeDefined()
    sessionStorage.setItem('probe', 'session-value')
    expect(sessionStorage.getItem('probe')).toBe('session-value')
    expect(localStorage.getItem('probe')).toBeNull()
  })

  it('gives window and globalThis the same Storage instances', () => {
    expect(window.localStorage).toBe(globalThis.localStorage)
    expect(window.sessionStorage).toBe(globalThis.sessionStorage)
  })
})
