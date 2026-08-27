import { identityAccentOrder, organizationPersonAccent } from '@chief/piing'
import { expect, test } from 'vitest'

/**
 * #485: a person's identity accent must be stable across roster GROWTH — adding
 * a person must not rotate anyone else's colour. The bug was that the accent was
 * allocated by POSITION in `manifest.peopleOrder`, which `refreshPeopleOrder`
 * re-sorts by department rank + head-first on every mutation, so a hire shifted
 * existing indices and repainted the company. The fix allocates by `createdAt`
 * (a new hire always sorts LAST), so growth never moves an existing accent.
 */
function accentsBy(order: readonly string[]): Record<string, string> {
  return Object.fromEntries(order.map((id) => [id, organizationPersonAccent(order, id)]))
}
function identityAccents(people: Record<string, { createdAt: string }>): Record<string, string> {
  return accentsBy(identityAccentOrder(people))
}

test("#485: hiring a new person does NOT rotate existing people's identity accents", () => {
  const people: Record<string, { createdAt: string }> = {
    ceo: { createdAt: '2026-07-01T00:00:00.000Z' },
    'chief-of-staff': { createdAt: '2026-07-01T00:01:00.000Z' },
    'market-intel-head': { createdAt: '2026-07-01T00:02:00.000Z' },
    'execution-head': { createdAt: '2026-07-01T00:03:00.000Z' }
  }
  const before = identityAccents(people)
  // A real hire gets a LATER createdAt than everyone already on the roster.
  const grown = { ...people, 'new-analyst': { createdAt: '2026-07-01T09:00:00.000Z' } }
  const after = identityAccents(grown)

  for (const id of Object.keys(people)) {
    expect(after[id]).toBe(before[id]) // identity colour survives the hire
  }
  expect(after['new-analyst']).toBeDefined()
  // Still all-distinct (the #111 no-duplicate guarantee holds).
  expect(new Set(Object.values(after)).size).toBe(Object.keys(grown).length)
})

test('#485: accents are keyed on identity (createdAt), not on peopleOrder position or map insertion order', () => {
  const people = {
    a: { createdAt: '2026-07-01T00:00:00.000Z' },
    b: { createdAt: '2026-07-01T00:01:00.000Z' },
    c: { createdAt: '2026-07-01T00:02:00.000Z' }
  }
  const stable = identityAccents(people)
  // The same people presented in a DIFFERENT map/insertion order (what a
  // department re-sort produces) must yield the identical accents.
  const reordered = { c: people.c, a: people.a, b: people.b }
  expect(identityAccents(reordered)).toEqual(stable)
})

test('#485 documents the bug it fixes: POSITION-based allocation DOES rotate when the order re-sorts', () => {
  // The pre-fix behavior: accent = index in the (mutable, re-sorted) order.
  const before = accentsBy(['ceo', 'chief-of-staff', 'execution-head'])
  // refreshPeopleOrder can insert a new hire (or a freshly-appointed head)
  // AHEAD of existing people, shifting their indices — the exact rotation #485
  // measured live.
  const afterResort = accentsBy(['ceo', 'new-head', 'chief-of-staff', 'execution-head'])
  expect(afterResort['chief-of-staff']).not.toBe(before['chief-of-staff'])
  expect(afterResort['execution-head']).not.toBe(before['execution-head'])
  // And the fix (createdAt order) is what avoids this — proven by the tests above.
})
