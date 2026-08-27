import { fixedResponseTransport } from '@test/resources/RecordingTransport'
import { describe, expect, it } from 'vitest'

import { RowStoresClient } from '@/resources/RowStores'

describe('RowStoresClient.rowMutate', () => {
  it('async read-compute-publish: reads once, computes once, publishes once', async () => {
    const transport = fixedResponseTransport(200, {})
    const client = new RowStoresClient(transport)
    let reads = 0
    let publishes = 0
    const value = await client.rowMutate<{ count: number }>(
      async () => {
        reads += 1
        return { found: true, doc: { count: 1 } }
      },
      async () => {
        publishes += 1
      },
      (current) => ({ count: (current?.count ?? 0) + 1 })
    )
    expect(value).toEqual({ count: 2 })
    expect(reads).toBe(1)
    expect(publishes).toBe(1)
  })

  it('not found: mutator sees undefined and still publishes the computed value', async () => {
    const client = new RowStoresClient(fixedResponseTransport(200, {}))
    let published: { count: number } | undefined
    const value = await client.rowMutate<{ count: number }>(
      async () => ({ found: false }),
      async (doc) => {
        published = doc
      },
      (current) => ({ count: (current?.count ?? 0) + 1 })
    )
    expect(value).toEqual({ count: 1 })
    expect(published).toEqual({ count: 1 })
  })

  it('unchanged (mutator hands back the same reference): publishes nothing', async () => {
    const client = new RowStoresClient(fixedResponseTransport(200, {}))
    const existing = { count: 5 }
    let publishes = 0
    const value = await client.rowMutate<{ count: number }>(
      async () => ({ found: true, doc: existing }),
      async () => {
        publishes += 1
      },
      (current) => current ?? { count: 0 }
    )
    expect(value).toBe(existing)
    expect(publishes).toBe(0)
  })
})
