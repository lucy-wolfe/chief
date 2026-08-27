import { describe, expect, it } from 'vitest'

import { parseCompanyRow } from '@/discovery/Company'

function fullLocationRow(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    dir: '/work/acme',
    key: '0123456789ab',
    slug: 'acme',
    registeredAt: '2026-08-03T12:00:00.000Z',
    url: 'http://127.0.0.1:8794',
    port: 8794,
    pid: 4242,
    hostname: 'box',
    lastSeenAt: '2026-08-04T00:00:00.000Z',
    ...overrides
  }
}

describe('parseCompanyRow', () => {
  it('accepts a row with a full location', () => {
    const row = parseCompanyRow(fullLocationRow())
    expect(row).toEqual({
      dir: '/work/acme',
      key: '0123456789ab',
      slug: 'acme',
      registeredAt: '2026-08-03T12:00:00.000Z',
      url: 'http://127.0.0.1:8794',
      port: 8794,
      pid: 4242,
      hostname: 'box',
      lastSeenAt: '2026-08-04T00:00:00.000Z'
    })
  })

  it('accepts a row with no location fields at all — the never-booted company', () => {
    const row = parseCompanyRow({
      dir: '/work/acme',
      key: '0123456789ab',
      slug: 'acme',
      registeredAt: '2026-08-03T12:00:00.000Z'
    })
    expect(row.url).toBeUndefined()
    expect(row.port).toBeUndefined()
    expect(row.pid).toBeUndefined()
    expect(row.hostname).toBeUndefined()
    expect(row.lastSeenAt).toBeUndefined()
  })

  it('treats explicit nulls as absent, same as an absent field', () => {
    const row = parseCompanyRow({
      dir: '/work/acme',
      key: '0123456789ab',
      slug: 'acme',
      registeredAt: '2026-08-03T12:00:00.000Z',
      url: null,
      port: null,
      pid: null,
      hostname: null,
      lastSeenAt: null
    })
    expect(row.url).toBeUndefined()
    expect(row.port).toBeUndefined()
    expect(row.pid).toBeUndefined()
    expect(row.hostname).toBeUndefined()
    expect(row.lastSeenAt).toBeUndefined()
  })

  it('rejects a missing slug', () => {
    const { slug: _slug, ...rest } = fullLocationRow()
    expect(() => parseCompanyRow(rest)).toThrow()
  })

  it('rejects a missing dir — the identity', () => {
    const { dir: _dir, ...rest } = fullLocationRow()
    expect(() => parseCompanyRow(rest)).toThrow()
  })

  it('rejects a missing key', () => {
    const { key: _key, ...rest } = fullLocationRow()
    expect(() => parseCompanyRow(rest)).toThrow()
  })

  /** THE KEY IS READ, NEVER DERIVED — so its SHAPE is checked at the wire.
   *
   * A slug, a path, or a truncated digest in the `key` field is a producer
   * that filled the wrong one, and the mistake the composite `slug@hash` made
   * plausible. Nothing downstream re-hashes `dir` to notice. */
  it('rejects a key that is not twelve lowercase hex characters', () => {
    for (const bad of ['', 'acme', '/work/acme', '0123456789a', '0123456789abc', '0123456789AB']) {
      expect(() => parseCompanyRow(fullLocationRow({ key: bad })), bad).toThrow()
    }
  })

  /** TWO DIRECTORIES MAY HOLD COMPANIES WITH THE SAME DISPLAY WORD.
   *
   * The slug carries no uniqueness at all any more, so a pair that differs
   * only by directory must parse as two legitimate rows — not as a duplicate
   * this boundary has any opinion about. */
  it('accepts two rows that share a slug and differ only by directory', () => {
    const first = parseCompanyRow(fullLocationRow())
    const second = parseCompanyRow(fullLocationRow({ dir: '/elsewhere/acme', key: 'cafebabe0011' }))
    expect(first.slug).toBe(second.slug)
    expect(first.dir).not.toBe(second.dir)
    expect(first.key).not.toBe(second.key)
  })

  it('rejects a missing registeredAt', () => {
    const { registeredAt: _registeredAt, ...rest } = fullLocationRow()
    expect(() => parseCompanyRow(rest)).toThrow()
  })

  it('rejects a string port', () => {
    expect(() => parseCompanyRow(fullLocationRow({ port: '8794' }))).toThrow()
  })

  it('rejects a pid of 0', () => {
    expect(() => parseCompanyRow(fullLocationRow({ pid: 0 }))).toThrow()
  })

  it('rejects a partial location — url present, pid absent', () => {
    const row = fullLocationRow()
    delete row.pid
    expect(() => parseCompanyRow(row)).toThrow()
  })

  it('rejects a partial location — only hostname present', () => {
    expect(() =>
      parseCompanyRow({
        dir: '/work/acme',
        key: '0123456789ab',
        slug: 'acme',
        registeredAt: '2026-08-03T12:00:00.000Z',
        hostname: 'box'
      })
    ).toThrow()
  })

  it('rejects null', () => {
    expect(() => parseCompanyRow(null)).toThrow()
  })

  it('rejects a non-object', () => {
    expect(() => parseCompanyRow('acme')).toThrow()
    expect(() => parseCompanyRow(42)).toThrow()
    expect(() => parseCompanyRow(['acme'])).toThrow()
  })

  it('the rejection is a thrown error, not a silent partial object', () => {
    let threw = false
    try {
      parseCompanyRow({ slug: 'acme' })
    } catch (error) {
      threw = true
      expect(error).toBeInstanceOf(Error)
    }
    expect(threw).toBe(true)
  })
})
