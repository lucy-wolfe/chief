import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import type { ChallengeResponse } from '@/types/Auth'
import type { DocsRuntime } from '@/types/Health'
import type { OrganizationPersonContractsDocument } from '@/types/PersonContracts'
import type { Reminder } from '@/types/Reminders'
import type { SessionEpochDoc } from '@/types/RowDocs'
import type { SseDocChangeEvent } from '@/types/Watch'

const here = dirname(fileURLToPath(import.meta.url))
const fixturesDir = join(here, '..', 'fixtures')

function loadFixture<T>(name: string): T {
  return JSON.parse(readFileSync(join(fixturesDir, name), 'utf8'))
}

function hasKeys(value: unknown, keys: readonly string[]): boolean {
  if (!value || typeof value !== 'object') return false
  return keys.every((key) => key in value)
}

/** Compile-time-only helper: fails to typecheck if `K` is not a key of `T`.
 * Never called — its existence in the file is the assertion. */
function assertHasKey<T>(): <K extends keyof T>(key: K) => void {
  return () => undefined
}

describe('wire casing — type-level assertions', () => {
  it('SseDocChangeEvent keeps updated_at snake_case (never updatedAt)', () => {
    assertHasKey<SseDocChangeEvent>()('updated_at')
    // @ts-expect-error — camelCase must not exist on this type.
    assertHasKey<SseDocChangeEvent>()('updatedAt')
    expect(true).toBe(true)
  })
})

describe('wire casing — runtime fixture round-trips satisfy required keys', () => {
  it('SseDocChangeEvent fixture has every required key, snake_case included', () => {
    const event = loadFixture<SseDocChangeEvent>('watch-event.json')
    expect(hasKeys(event, ['seq', 'slug', 'store', 'updated_at', 'removed'])).toBe(true)
    expect(event.updated_at).toBeTypeOf('string')
  })

  it('Reminder fixture satisfies Reminder', () => {
    const reminder = loadFixture<Reminder>('reminder.json')
    expect(
      hasKeys(reminder, [
        'id',
        'personId',
        'createdByPersonId',
        'prompt',
        'intervalMs',
        'nextDueAt',
        'status',
        'recurring',
        'createdAt'
      ])
    ).toBe(true)
  })

  it('OrganizationPersonContractsDocument fixture satisfies the type', () => {
    const document = loadFixture<OrganizationPersonContractsDocument>(
      'person-contracts-document.json'
    )
    expect(hasKeys(document, ['version', 'organization', 'contracts'])).toBe(true)
    expect(hasKeys(document.contracts['person-1'], ['text', 'md5'])).toBe(true)
  })

  it('SessionEpochDoc fixture satisfies the type (RowDocs family)', () => {
    const doc = loadFixture<SessionEpochDoc>('session-epoch-doc.json')
    expect(hasKeys(doc, ['version', 'organization', 'epochAt', 'reason'])).toBe(true)
  })

  it('ChallengeResponse fixture satisfies the type (Auth family)', () => {
    const challenge = loadFixture<ChallengeResponse>('challenge-response.json')
    expect(hasKeys(challenge, ['nonceId', 'nonce'])).toBe(true)
  })

  it('DocsRuntime fixture satisfies the type (Health family)', () => {
    const runtime = loadFixture<DocsRuntime>('docs-runtime.json')
    expect(hasKeys(runtime, ['mode'])).toBe(true)
  })
})
