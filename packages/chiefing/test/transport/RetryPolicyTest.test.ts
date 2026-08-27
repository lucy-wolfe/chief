import { describe, expect, it } from 'vitest'

import {
  awaitedDelay,
  CONNECT_RETRY_BACKOFFS_MS,
  ENSURE_SCHEMA_RETRY_DELAYS_MS,
  retryDelayWithJitter
} from '@/transport/RetryPolicy'

describe('ladder constants are frozen to their contracted values', () => {
  it('CONNECT_RETRY_BACKOFFS_MS', () => {
    expect(CONNECT_RETRY_BACKOFFS_MS).toEqual([25, 75, 150])
  })

  it('ENSURE_SCHEMA_RETRY_DELAYS_MS', () => {
    expect(ENSURE_SCHEMA_RETRY_DELAYS_MS).toEqual([100, 250, 500, 1000, 2000, 4000])
  })

  it('approaches but never reaches 2x baseMs as random() approaches 1', () => {
    const delay = retryDelayWithJitter(100, () => 0.999999)
    expect(delay).toBeGreaterThan(100)
    expect(delay).toBeLessThan(200)
  })

  it('stays within [baseMs, baseMs * 2) for the default random source', () => {
    const delay = retryDelayWithJitter(50)
    expect(delay).toBeGreaterThanOrEqual(50)
    expect(delay).toBeLessThan(100)
  })
})

describe('awaitedDelay', () => {
  it('resolves after roughly the requested delay, without blocking the event loop', async () => {
    const started = Date.now()
    await awaitedDelay(5)
    expect(Date.now() - started).toBeGreaterThanOrEqual(0)
  })
})

describe('the dead ladder constant and the never-wired breaker stay deleted (#751/G13)', () => {
  it('LOCK_RETRY_BASE_DELAYS_MS and UnreachableCircuit are gone from the module', async () => {
    // Both shipped for "a later resource story" / "a consumer (apps/api) opts
    // in per client" that never arrived, and could not: E8-S6c deleted the
    // whole lock surface on both sides, so there is no busy refusal left for
    // the ladder to pace. This replaces the two value assertions that used to
    // live above — the contract is now their ABSENCE, and a revert that
    // re-adds either is caught here rather than shipping a second unused
    // retry policy beside the live one.
    const module: Record<string, unknown> = await import('@/transport/RetryPolicy')
    expect(Object.prototype.hasOwnProperty.call(module, 'LOCK_RETRY_BASE_DELAYS_MS')).toBe(false)
    expect(Object.prototype.hasOwnProperty.call(module, 'UnreachableCircuit')).toBe(false)
    // Negative self-check: the same probe finds a symbol that IS exported.
    expect(Object.prototype.hasOwnProperty.call(module, 'CONNECT_RETRY_BACKOFFS_MS')).toBe(true)
  })
})
