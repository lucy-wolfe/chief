import { describe, expect, it } from 'vitest'

import { identityAccentOrder, organizationPersonAccent } from '@/home/IdentityTheme'

describe('identityAccentOrder', () => {
  it('orders by createdAt, ascending', () => {
    const order = identityAccentOrder({
      bob: { createdAt: '2026-01-02T00:00:00Z' },
      alice: { createdAt: '2026-01-01T00:00:00Z' }
    })
    expect(order).toEqual(['alice', 'bob'])
  })

  it('breaks a createdAt tie by id, deterministically', () => {
    const order = identityAccentOrder({
      zed: { createdAt: '2026-01-01T00:00:00Z' },
      amy: { createdAt: '2026-01-01T00:00:00Z' }
    })
    expect(order).toEqual(['amy', 'zed'])
  })

  it('a new hire (later createdAt) always sorts last, never displacing an earlier one', () => {
    const before = identityAccentOrder({
      alice: { createdAt: '2026-01-01T00:00:00Z' },
      bob: { createdAt: '2026-01-02T00:00:00Z' }
    })
    const after = identityAccentOrder({
      alice: { createdAt: '2026-01-01T00:00:00Z' },
      bob: { createdAt: '2026-01-02T00:00:00Z' },
      carol: { createdAt: '2026-01-03T00:00:00Z' }
    })
    expect(after.slice(0, before.length)).toEqual(before)
    expect(after.at(-1)).toBe('carol')
  })
})

describe('organizationPersonAccent', () => {
  it('is stable w.r.t. the given order: same position always gets the same accent', () => {
    const order = ['alice', 'bob', 'carol']
    const first = organizationPersonAccent(order, 'bob')
    const second = organizationPersonAccent(order, 'bob')
    expect(first).toBe(second)
  })

  it('assigns a distinct accent to every person in a roster larger than the palette', () => {
    const order = Array.from({ length: 30 }, (_, index) => `person-${index}`)
    const accents = order.map((id) => organizationPersonAccent(order, id))
    expect(new Set(accents).size).toBe(order.length)
  })

  it('every accent is a valid #rrggbb hex color', () => {
    const order = ['alice']
    expect(organizationPersonAccent(order, 'alice')).toMatch(/^#[0-9a-f]{6}$/)
  })

  it('throws for a person not present in the given order', () => {
    expect(() => organizationPersonAccent(['alice'], 'unknown')).toThrow(/unknown person/)
  })

  it('adding a new person to the end of the order does not change an existing accent', () => {
    const before = ['alice', 'bob']
    const after = ['alice', 'bob', 'carol']
    expect(organizationPersonAccent(before, 'alice')).toBe(organizationPersonAccent(after, 'alice'))
    expect(organizationPersonAccent(before, 'bob')).toBe(organizationPersonAccent(after, 'bob'))
  })
})
