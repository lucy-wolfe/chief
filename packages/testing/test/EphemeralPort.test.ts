import { createServer } from 'node:net'

import { describe, expect, it } from 'vitest'

import { allocateEphemeralPort } from '@/EphemeralPort'

describe('allocateEphemeralPort', () => {
  it('returns a distinct port on two consecutive allocations', async () => {
    const first = await allocateEphemeralPort()
    const second = await allocateEphemeralPort()
    expect(first).not.toBe(second)
  })

  it('returns a port that is bindable immediately after allocation', async () => {
    const port = await allocateEphemeralPort()
    await new Promise<void>((resolve, reject) => {
      const server = createServer()
      server.once('error', reject)
      server.listen(port, '127.0.0.1', () => {
        server.close(() => resolve())
      })
    })
  })

  it('returns a port in the valid TCP range', async () => {
    const port = await allocateEphemeralPort()
    expect(port).toBeGreaterThan(0)
    expect(port).toBeLessThan(65536)
  })
})
