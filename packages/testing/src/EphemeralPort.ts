/**
 * Allocates an ephemeral TCP port by binding `:0` on loopback, reading the
 * OS-assigned port, and closing immediately.
 *
 * Port TOCTOU, stated honestly: between this function closing its probe
 * socket and the daemon binding the same port, another process could grab
 * it — a real, if narrow, race. This module does not retry around it; the
 * a bind conflict makes the child exit immediately, and boot fails fast with
 * the child's log tail rather than hanging (see that module's readiness
 * rules). A retry loop here would paper over a failure the caller is already
 * required to handle correctly.
 */
import { createServer } from 'node:net'

export function allocateEphemeralPort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer()
    server.once('error', (error) => {
      reject(error)
    })
    server.listen(0, '127.0.0.1', () => {
      const address = server.address()
      if (!address || typeof address === 'string') {
        server.close()
        reject(new Error('allocateEphemeralPort: OS did not return a bound port'))
        return
      }
      const { port } = address
      server.close((closeError) => {
        if (closeError) {
          reject(closeError)
          return
        }
        resolve(port)
      })
    })
  })
}
