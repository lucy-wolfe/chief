/**
 * A fetch rejection must reach an operator carrying what it already knew.
 *
 * # The defect
 *
 * Node's undici rejects with a `TypeError` whose message is the two words
 * `fetch failed`, and puts the real fact — `ECONNREFUSED`, the address, the
 * port — on `error.cause`. Every surface that reported a connect failure read
 * `.message`, so an operator whose daemon had never been started read two
 * words and could not learn which port nobody was listening on.
 *
 * The shapes below are the real ones, not invented: undici's nested
 * `cause` with `code`/`address`/`port`, Bun's own `ConnectionRefused` set on
 * the error itself, and the `AggregateError` a dual-stack host produces.
 */
import { describe, expect, it } from 'vitest'

import { describeFetchFailure, fetchFailureDetail } from '@/transport/FetchFailure'

/** undici: `TypeError: fetch failed` with the facts one level down. */
function undiciRefusal(port = 8789): Error {
  const cause = Object.assign(new Error('connect ECONNREFUSED 127.0.0.1:' + port), {
    code: 'ECONNREFUSED',
    syscall: 'connect',
    address: '127.0.0.1',
    port
  })
  return Object.assign(new TypeError('fetch failed'), { cause })
}

describe('a fetch rejection names the cause it is already carrying', () => {
  it('turns undici`s two words into the code and the port', () => {
    // THE DEFECT, in one assertion: this is what the operator reads.
    expect(describeFetchFailure(undiciRefusal())).toBe('fetch failed: ECONNREFUSED 127.0.0.1:8789')
    expect(fetchFailureDetail(undiciRefusal())).toBe('ECONNREFUSED 127.0.0.1:8789')
  })

  it('reads through an AggregateError, which is how a dual-stack host refuses', () => {
    // A name resolving to both IPv4 and IPv6 rejects with an AggregateError
    // whose CHILDREN carry the codes; the outer error carries none. A walker
    // that only followed `cause` would report nothing here.
    const aggregate = Object.assign(new AggregateError([], 'all attempts failed'), {
      errors: [
        Object.assign(new Error('connect ECONNREFUSED ::1:6969'), {
          code: 'ECONNREFUSED',
          address: '::1',
          port: 6969
        })
      ]
    })
    const error = Object.assign(new TypeError('fetch failed'), { cause: aggregate })

    expect(fetchFailureDetail(error)).toBe('ECONNREFUSED ::1:6969')
  })

  it('does not repeat a code the message already names', () => {
    // Bun sets `code` on the error itself and writes a fuller sentence than
    // undici does. Appending there produces "... ConnectionRefused:
    // ConnectionRefused", which is noise pretending to be detail.
    const bun = Object.assign(new Error('Unable to connect. ConnectionRefused'), {
      code: 'ConnectionRefused'
    })

    expect(describeFetchFailure(bun)).toBe('Unable to connect. ConnectionRefused')
  })

  it('renders a code that carries no address, rather than inventing one', () => {
    const dns = Object.assign(new TypeError('fetch failed'), {
      cause: Object.assign(new Error('getaddrinfo ENOTFOUND nowhere.invalid'), {
        code: 'ENOTFOUND'
      })
    })

    expect(fetchFailureDetail(dns)).toBe('ENOTFOUND')
    expect(describeFetchFailure(dns)).toBe('fetch failed: ENOTFOUND')
  })

  it('adds NOTHING when the rejection knows nothing more', () => {
    // The honest half. A helper that always appends something would append an
    // invention here, and an invented diagnosis is worse than two honest
    // words — it sends the reader somewhere specific and wrong.
    const bare = new TypeError('fetch failed')

    expect(fetchFailureDetail(bare)).toBeUndefined()
    expect(describeFetchFailure(bare)).toBe('fetch failed')
  })

  it('survives a self-referential cause chain instead of spinning', () => {
    // A runtime is free to hand us a cycle. A diagnostic must never be the
    // reason a request hangs.
    const looping: { message: string; cause?: unknown } = { message: 'looping' }
    looping.cause = looping

    expect(fetchFailureDetail(looping)).toBeUndefined()
  })

  it('describes a non-Error rejection without throwing', () => {
    expect(describeFetchFailure('just a string')).toBe('just a string')
    expect(fetchFailureDetail(undefined)).toBeUndefined()
  })
})
