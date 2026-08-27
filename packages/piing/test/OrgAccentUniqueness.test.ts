import { isNullish } from '@test/support/Nullish'
import {
  colorRelativeLuminance,
  identityForegroundMode,
  ORGANIZATION_PERSON_ACCENTS as CARD_STYLE_ACCENTS,
  organizationPersonAccents as cardStyleAccents,
  organizationPersonDisplayAccent,
  readableIdentityForeground
} from '@test-assets/card-style'
import {
  colorizePersonMentions,
  type IntercomOrganizationManifest,
  organizationPersonAccentHex
} from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

import { identityAccentOrder, organizationPersonAccent } from '@/home/IdentityTheme'

type IntercomPerson = IntercomOrganizationManifest extends { people: Record<string, infer Person> }
  ? Person
  : never
type IntercomDepartment = IntercomOrganizationManifest extends {
  departments: Record<string, infer Department>
}
  ? Department
  : never

const roster = (count: number): string[] =>
  Array.from({ length: count }, (_, index) => `person-${index}`)

function sourceAccents(peopleOrder: readonly string[]): string[] {
  return peopleOrder.map((personId) => organizationPersonAccent(peopleOrder, personId))
}

function contrast(left: string, right: string): number {
  const leftLuminance = colorRelativeLuminance(left)
  const rightLuminance = colorRelativeLuminance(right)
  return (
    (Math.max(leftLuminance, rightLuminance) + 0.05) /
    (Math.min(leftLuminance, rightLuminance) + 0.05)
  )
}

function backgroundTheme(background: string): { getBgAnsi(token: string): string } {
  const [r = 0, g = 0, b = 0] = [1, 3, 5].map((index) =>
    Number.parseInt(background.slice(index, index + 2), 16)
  )
  return { getBgAnsi: () => `\x1b[48;2;${r};${g};${b}m` }
}

function intercomPerson(id: string, createdAt: string): IntercomPerson {
  return {
    id,
    name: id,
    title: 'test role',
    kind: 'worker',
    departmentId: 'root',
    employmentState: 'active',
    createdAt
  }
}

function rootDepartment(headPersonId: string): IntercomDepartment {
  return {
    id: 'root',
    name: 'Root',
    purpose: 'test fixture',
    headPersonId,
    state: 'active'
  }
}

function intercomManifest(peopleOrder: string[]): IntercomOrganizationManifest {
  const people = Object.fromEntries(
    peopleOrder.map((id, index) => [
      id,
      intercomPerson(id, `2026-07-01T00:${String(index).padStart(2, '0')}:00.000Z`)
    ])
  )
  const firstPerson = peopleOrder.at(0)
  if (typeof firstPerson !== 'string') throw new Error('accent fixture needs a person')
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: 'tribes-capital',
    name: 'Tribes Capital',
    rootDepartmentId: 'root',
    departmentOrder: ['root'],
    peopleOrder,
    departments: { root: rootDepartment(firstPerson) },
    people
  }
}

describe('#111 — identity is the color, so no two people may share one', () => {
  test('a roster inside the palette gets the curated hues, unchanged', () => {
    const order = roster(CARD_STYLE_ACCENTS.length)
    expect(sourceAccents(order)).toEqual([...CARD_STYLE_ACCENTS])
  })

  test('a roster larger than the palette still gives everyone a distinct accent', () => {
    const order = roster(CARD_STYLE_ACCENTS.length * 3 + 7)
    const accents = sourceAccents(order)

    expect(accents).toHaveLength(order.length)
    expect(new Set(accents).size).toBe(order.length)
  })

  test('every curated and hue-wrapped identity has an AA Light and Dark foreground', () => {
    const rawAccents = cardStyleAccents(roster(200))
    const lightBackgrounds = [
      '#f8f8f8',
      '#d0d0e0',
      '#e8e8e8',
      '#ede7f6',
      '#e8e8f0',
      '#e8f0e8',
      '#f0e8e8'
    ]
    const darkBackgrounds = [
      '#18181e',
      '#3a3a4a',
      '#343541',
      '#2d2838',
      '#282832',
      '#283228',
      '#3c2828'
    ]
    for (const raw of rawAccents) {
      for (const [mode, backgrounds] of [
        ['light', lightBackgrounds],
        ['dark', darkBackgrounds]
      ] as const) {
        const foreground = readableIdentityForeground(raw, mode)
        for (const background of backgrounds) {
          expect(
            contrast(foreground, background),
            `${raw} -> ${foreground} on ${background}`
          ).toBeGreaterThanOrEqual(4.5)
        }
      }
    }
  })

  test('Automatic mention color follows the live Pi card background', () => {
    const raw = CARD_STYLE_ACCENTS[0]
    const lightTheme = backgroundTheme('#ede7f6')
    const darkTheme = backgroundTheme('#2d2838')
    expect(identityForegroundMode(lightTheme)).toBe('light')
    expect(identityForegroundMode(darkTheme)).toBe('dark')
    expect(organizationPersonDisplayAccent(lightTheme, raw)).toBe(
      readableIdentityForeground(raw, 'light')
    )
    expect(organizationPersonDisplayAccent(darkTheme, raw)).toBe(
      readableIdentityForeground(raw, 'dark')
    )
  })

  test('raw person mentions use the readable display color while the Chief stays neutral', () => {
    const manifest = intercomManifest(['chief', 'worker'])
    const theme = {
      ...backgroundTheme('#ede7f6'),
      getFgAnsi: () => '\x1b[38;2;108;108;108m'
    }
    const workerRaw = organizationPersonAccentHex(manifest, 'worker')
    expect(workerRaw).toBeDefined()
    if (isNullish(workerRaw)) throw new Error('worker needs an accent')
    const display = readableIdentityForeground(workerRaw, 'light')
    const [r, g, b] = [1, 3, 5].map((index) => Number.parseInt(display.slice(index, index + 2), 16))
    const rendered = colorizePersonMentions(theme, manifest, '@chief asked @worker')
    expect(rendered).toContain('@chief')
    expect(rendered).toContain(`\x1b[38;2;${r};${g};${b}m@worker\x1b[39m`)
    expect(rendered).not.toMatch(/38;2;[^m]+m@chief/)
  })

  test('the live 49-person roster does not collide across palette wraps', () => {
    const accents = sourceAccents(roster(49))
    const firstWrap = CARD_STYLE_ACCENTS.length
    const secondWrap = firstWrap * 2

    expect(new Set(accents).size).toBe(49)
    expect(accents[firstWrap]).not.toBe(accents[0])
    expect(accents[secondWrap]).not.toBe(accents[0])
    expect(accents[secondWrap]).not.toBe(accents[firstWrap])
  })

  test('every wrapped raw accent returns to the curated relative-luminance band', () => {
    const accents = cardStyleAccents(roster(200)).slice(CARD_STYLE_ACCENTS.length)
    expect(accents.slice(0, 10)).toEqual([
      '#9f7517',
      '#788300',
      '#5c8900',
      '#2b8a7e',
      '#3c72ff',
      '#8566e6',
      '#9468c5',
      '#e10dc2',
      '#da4a45',
      '#a37240'
    ])
    for (const accent of accents) {
      expect(Math.abs(colorRelativeLuminance(accent) - 0.202), accent).toBeLessThanOrEqual(0.003)
    }
  })

  test("nobody shares the CEO's accent", () => {
    const order = ['ceo', ...roster(80)]
    const accents = sourceAccents(order)
    const ceo = accents.at(0)

    expect(ceo).toBeDefined()
    expect(accents.slice(1).filter((accent) => accent === ceo)).toEqual([])
  })

  test('a person outside the roster is a loud error, not a color', () => {
    expect(() => organizationPersonAccent(['ceo'], 'nobody')).toThrow(/unknown person/)
  })

  test("the extension's duplicated allocator agrees with the source of truth", () => {
    const peopleOrder = roster(49)
    const people = Object.fromEntries(
      peopleOrder.map((id, index) => [
        id,
        { createdAt: `2026-07-01T00:${String(index).padStart(2, '0')}:00.000Z` }
      ])
    )
    const identityOrder = identityAccentOrder(people)
    const expected = sourceAccents(identityOrder)
    const manifest = intercomManifest(peopleOrder)

    const chiefPersonId = manifest.departments[manifest.rootDepartmentId]?.headPersonId
    expect(chiefPersonId).toBeDefined()
    if (isNullish(chiefPersonId)) throw new Error('fixture root needs a Chief')
    expect(organizationPersonAccentHex(manifest, chiefPersonId)).toBeUndefined()
    for (const [index, personId] of identityOrder.entries()) {
      if (personId === chiefPersonId) continue
      expect(organizationPersonAccentHex(manifest, personId)).toBe(expected[index])
    }
  })
})

describe('#150 — card-style is the accent origin; the Pi-home copy is pinned mechanically', () => {
  test('the palette arrays agree entry for entry', () => {
    expect(sourceAccents(roster(CARD_STYLE_ACCENTS.length))).toEqual([...CARD_STYLE_ACCENTS])
  })

  test('the allocators agree behaviorally across wrap boundaries', () => {
    const base = CARD_STYLE_ACCENTS.length

    for (const count of [1, base - 1, base, base + 1, base * 2, base * 2 + 1, 49, 80]) {
      const order = roster(count)
      expect(cardStyleAccents(order)).toEqual(sourceAccents(order))
    }
  })

  // The palette and allocator are the only duplicated mechanism. Chief-neutral
  // Pi appearance is resolved structurally by the home writer and intercom;
  // it is not a second allocator branch to pin here.
})
