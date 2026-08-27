// E2-S5 — no staffing method retries anything, on transport failure or on
// 503: exactly one request is recorded in every case (a refused verb
// re-sent is a different op). Also guards ruling D24/F25: StaffingClient
// exposes no companyRemove and the package exports no CompanyRemove* type.

import {
  fixedResponseTransport,
  jsonResponse,
  RecordingTransport,
  textResponse
} from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import * as chiefing from '@/index'
import { StaffingClient } from '@/resources/Staffing'

describe('StaffingClient — never retries', () => {
  it('a 503 is surfaced as ChiefdUnavailableError after exactly one request', async () => {
    const transport = new RecordingTransport(() => textResponse(503, 'unavailable'))
    const staffing = new StaffingClient(transport)
    await expect(staffing.benchPerson('acme', 'p1')).rejects.toBeInstanceOf(
      chiefing.ChiefdUnavailableError
    )
    expect(transport.calls).toHaveLength(1)
  })

  it('a transport rejection propagates without a retry attempt', async () => {
    // The responder throws unconditionally, simulating a transport-level
    // failure. If StaffingClient retried, `calls` would grow past 1 on a
    // second attempt — asserted below regardless of how many times the
    // transport was actually invoked.
    const transport = new RecordingTransport(() => {
      throw new Error('simulated transport failure')
    })
    const staffing = new StaffingClient(transport)
    await expect(staffing.recallPerson('acme', 'p1')).rejects.toThrow()
    expect(transport.calls).toHaveLength(1)
  })

  it('a 422 refusal on a refusal-as-value verb is not retried', async () => {
    const transport = new RecordingTransport(() => jsonResponse(422, { code: 'x', detail: 'y' }))
    const staffing = new StaffingClient(transport)
    await staffing.offboardPerson('acme', 'p1')
    expect(transport.calls).toHaveLength(1)
  })

  it('rg-equivalent: no retry/re-POST/attempt wording anywhere in Staffing.ts', async () => {
    const { readFileSync } = await import('node:fs')
    const { dirname, join } = await import('node:path')
    const { fileURLToPath } = await import('node:url')
    const here = dirname(fileURLToPath(import.meta.url))
    const staffingSrc = readFileSync(
      join(here, '..', '..', 'src', 'resources', 'Staffing.ts'),
      'utf8'
    )
    expect(/retry|re-?POST|attempt/i.test(staffingSrc)).toBe(false)
  })
})

describe('ruling D24/F25 — companyRemove is not on this surface', () => {
  it('StaffingClient exposes no companyRemove method', () => {
    // No call is ever made — a static shape check — so the transport's own
    // responder is never invoked.
    const staffing = new StaffingClient(fixedResponseTransport(200, {}))
    expect('companyRemove' in staffing).toBe(false)
  })

  it('the package barrel exports no CompanyRemove* type or value', () => {
    const exported = Object.keys(chiefing)
    expect(exported.some((name) => name.startsWith('CompanyRemove'))).toBe(false)
  })

  it("rg-equivalent: companyRemove/CompanyRemove never appear in this story's own files", async () => {
    const { readFileSync } = await import('node:fs')
    const { dirname, join } = await import('node:path')
    const { fileURLToPath } = await import('node:url')
    const here = dirname(fileURLToPath(import.meta.url))
    const src = join(here, '..', '..', 'src')

    // Scoped to this story's own resource clients — exactly the
    // `.../src/resources/Staffing.ts` scope the Contract's own grep names —
    // not the whole package: both RowStores.ts (S4, out of this story's
    // isolation) and this package's own types/Staffing.ts carry a legitimate
    // doc comment NAMING the excluded D24/F25 family to explain its absence,
    // which is documentation, not a reintroduction of the protocol.
    const ownFiles = [join(src, 'resources', 'Staffing.ts'), join(src, 'resources', 'Reminders.ts')]
    const hits = ownFiles.filter((file) =>
      /companyRemove|CompanyRemove/.test(readFileSync(file, 'utf8'))
    )
    expect(hits).toEqual([])
  })
})
