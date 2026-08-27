/**
 * Render what a `fetch()` rejection already knows, so nobody has to guess it.
 *
 * # The defect this exists to close
 *
 * Node's undici rejects with a `TypeError` whose message is the two words
 * `fetch failed`. The actual fact — `ECONNREFUSED`, and which address and port
 * were refused — is not lost: it is sitting on `error.cause`, populated, the
 * whole time. Every surface in this repo that reported a connect failure read
 * `error.message` and threw the cause away, so an operator whose company
 * daemon had simply never been started read `fetch failed` and had no way to
 * learn which port nobody was listening on. That cost real time: the answer
 * was that nothing had launched `chief host` on :8789, and the message that
 * would have said so was one property away at every one of those surfaces.
 *
 * # This never diagnoses
 *
 * It reads `code`, `address` and `port` off the cause chain and renders them.
 * It does not map codes to advice, does not guess at causes it cannot see, and
 * returns `undefined` when the rejection carries nothing its own message did
 * not already say — a caller that appends nothing is strictly better than one
 * that appends a confident invention.
 *
 * Every reader below narrows with `in` rather than asserting a shape, the same
 * idiom `FetchTransport.errorCode` uses on the same objects: these values come
 * from a runtime, and a cast would be this module claiming to know what it was
 * handed.
 */

/** One link's `code`, when it has one. */
function codeOf(error: object): string | undefined {
  if (!('code' in error)) return undefined
  const code = error.code
  return typeof code === 'string' && code.trim() !== '' ? code.trim() : undefined
}

/** One link's dialled address, when the runtime supplied it. */
function addressOf(error: object): string | undefined {
  if (!('address' in error)) return undefined
  const address = error.address
  return typeof address === 'string' && address.trim() !== '' ? address.trim() : undefined
}

/** One link's dialled port, when the runtime supplied it. */
function portOf(error: object): string | undefined {
  if (!('port' in error)) return undefined
  const port = error.port
  return typeof port === 'number' && Number.isFinite(port) ? String(port) : undefined
}

/**
 * The first link in the chain that names a `code`.
 *
 * Depth-first through `cause` and through `AggregateError.errors`, because a
 * host that resolves to both IPv4 and IPv6 rejects with an `AggregateError`
 * whose children carry the codes while the outer error carries none. Bounded
 * by `depth`, because a runtime is free to hand back a self-referential
 * `cause` and a diagnostic must never be the reason a request hangs.
 */
function firstCoded(error: unknown, depth = 0): object | undefined {
  if (depth > 8) return undefined
  if (!error || typeof error !== 'object') return undefined
  if (codeOf(error)) return error
  if ('cause' in error) {
    const nested = firstCoded(error.cause, depth + 1)
    if (nested) return nested
  }
  if ('errors' in error && Array.isArray(error.errors)) {
    for (const child of error.errors) {
      const found = firstCoded(child, depth + 1)
      if (found) return found
    }
  }
  return undefined
}

/**
 * What the rejection knows beyond its own message — `ECONNREFUSED
 * 127.0.0.1:8789` — or `undefined` when it knows nothing more.
 *
 * The address appears only when the runtime supplied one. undici sets
 * `address`/`port` on a connect refusal and omits them on, say, a DNS failure,
 * where the code alone is the whole fact.
 */
export function fetchFailureDetail(error: unknown): string | undefined {
  const coded = firstCoded(error)
  if (!coded) return undefined
  const code = codeOf(coded)
  if (!code) return undefined
  const address = addressOf(coded)
  const port = portOf(coded)
  if (address && port) return `${code} ${address}:${port}`
  if (address) return `${code} ${address}`
  if (port) return `${code} port ${port}`
  return code
}

/**
 * A fetch rejection's message with its own cause appended once.
 *
 * `fetch failed` becomes `fetch failed: ECONNREFUSED 127.0.0.1:8789`. A
 * message that ALREADY names the code is returned untouched — Bun sets `code`
 * on the error itself and writes a fuller sentence than undici does, and
 * appending there would produce `... ConnectionRefused: ConnectionRefused`.
 */
export function describeFetchFailure(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error)
  const detail = fetchFailureDetail(error)
  if (!detail) return message
  if (message.includes(detail)) return message
  const [code] = detail.split(' ')
  if (code && message.includes(code)) return message
  return `${message}: ${detail}`
}
