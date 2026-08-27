/**
 * SERVER-ONLY: the operator P-256 challenge→sign→token flow. Imported by
 * `app/api/session/route.ts` and nowhere else — the browser never sees the
 * operator private key.
 *
 * Signing goes through `@chief/chiefing`'s exported primitives
 * (`authChallengeMessage`/`signAuthChallenge`) — the ONE implementation of
 * this crypto in the monorepo. No domain tag is restated here, no signature
 * math is copied.
 *
 * A note for anyone diffing this against #807's story text: the story
 * describes `signAuthChallenge(privatePkcs8Pem, identityId, nonce): string`
 * (a single 3-arg call). What `@chief/chiefing` actually ships (E2-S3, #772)
 * is a 2-function composition — `authChallengeMessage(identityId, nonce):
 * string` builds the base64 domain-separated message, then
 * `signAuthChallenge(message, privatePkcs8Pem): string` signs it — verified
 * byte-identical to the original algorithm by a frozen fixture in
 * `packages/chiefing/test/resources/IdentityTest.test.ts`. This file calls
 * the real, merged export shape rather than the story's now-stale
 * description of it.
 */
import { authChallengeMessage, signAuthChallenge } from '@chief/chiefing'

import type { FetchImpl } from '@/types/Fetch'

/**
 * The two routes chiefd's verify-middleware exempts, because they mint the
 * token every other route needs.
 *
 * The `/v1` prefix is load-bearing and was MISSING until A2. These paths were
 * written when `apiUrl` meant the deleted apps/api and the version prefix was
 * already in the base; `apiUrl` is now a company daemon's bare origin from
 * beacond, where the routes are `/v1/auth/*` (`docstore/router.rs:684-685`).
 * Every call was a 404 that surfaced as `operator challenge failed: status 404`.
 */
const CHALLENGE_PATH = '/v1/auth/challenge'
const TOKEN_PATH = '/v1/auth/token'

interface ChallengeResponseBody {
  nonceId: string
  nonce: string
}

interface TokenResponseBody {
  token: string
}

function isChallengeResponseBody(value: unknown): value is ChallengeResponseBody {
  if (!value || typeof value !== 'object') return false
  return (
    'nonceId' in value &&
    typeof value.nonceId === 'string' &&
    'nonce' in value &&
    typeof value.nonce === 'string'
  )
}

function isTokenResponseBody(value: unknown): value is TokenResponseBody {
  if (!value || typeof value !== 'object') return false
  return 'token' in value && typeof value.token === 'string'
}

/* eslint-disable lucy/no-json-stringify */
// @tribes-terminal/foundation's toJsonTreeString is not a dependency
// anywhere in this workspace (same reasoning as @chief/chiefing's
// FetchTransport.ts, E2-S1).
function jsonBody(value: unknown): string {
  return JSON.stringify(value)
}
/* eslint-enable lucy/no-json-stringify */

/** Run the full challenge→sign→token round trip against apps/api and
 * return the minted JWT. Throws on any upstream failure — the route handler
 * turns that into the 502 `auth-upstream` envelope. */
export async function acquireOperatorToken(options: {
  apiUrl: string
  identityId: string
  privateKeyPem: string
  fetchImpl?: FetchImpl
}): Promise<string> {
  const fetchImpl = options.fetchImpl ?? fetch

  const challengeResponse = await fetchImpl(`${options.apiUrl}${CHALLENGE_PATH}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: jsonBody({ identityId: options.identityId })
  })
  if (challengeResponse.status !== 200) {
    throw new Error(`operator challenge failed: status ${challengeResponse.status}`)
  }
  const challengeBody: unknown = await challengeResponse.json()
  if (!isChallengeResponseBody(challengeBody)) {
    throw new Error('operator challenge response missing nonceId/nonce')
  }

  const message = authChallengeMessage(options.identityId, challengeBody.nonce)
  const signature = signAuthChallenge(message, options.privateKeyPem)

  const tokenResponse = await fetchImpl(`${options.apiUrl}${TOKEN_PATH}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: jsonBody({ nonceId: challengeBody.nonceId, signature })
  })
  if (tokenResponse.status !== 200) {
    throw new Error(`operator token mint failed: status ${tokenResponse.status}`)
  }
  const tokenBody: unknown = await tokenResponse.json()
  if (!isTokenResponseBody(tokenBody)) {
    throw new Error('operator token response missing token')
  }
  return tokenBody.token
}
