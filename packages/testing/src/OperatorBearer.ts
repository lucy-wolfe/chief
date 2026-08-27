/**
 * The test harness's own credential (A7 — the design record
 * ruling 6): read the operator key `chiefd run` minted in this harness's temp
 * data root, sign a challenge, and hold the bearer chiefd minted for it.
 *
 * # Why this is a fourth copy of a three-copy algorithm
 *
 * It should not be. `@chief/chiefing` exports `authChallengeMessage` and
 * `signAuthChallenge`, `apps/web` composes them server-side, and
 * `chief_cli::bearer` is the Rust speaker. Importing the TypeScript one here is
 * IMPOSSIBLE rather than merely undesirable: `@chief/chiefing` devDepends on
 * `@chief/testing` — this package is the harness its contract tests boot — so
 * the reverse edge is a workspace build cycle.
 *
 * What stops the copy from drifting is not care, it is a test.
 * `packages/chiefing/test/resources/HarnessSignatureParity.test.ts` lives in the
 * one package able to import BOTH halves and asserts they produce identical
 * bytes for one fixed key, identity and nonce. A copy that cannot silently
 * disagree is one rule with two speakers, which is the shape `identity_keys`
 * already imposes across the Rust/TypeScript boundary.
 *
 * # It never MINTS a key
 *
 * `chiefd run` creates `<data-root>/keys/operator.key` 0600 at boot and
 * preserves it thereafter. A harness that generated its own would present an
 * unenrolled identity and be refused — and it would write a second durable
 * anchor beside the one the daemon owns.
 */
import { createPrivateKey, sign } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'

import { isNullish } from '@/Nullish'
import type { AuthorizedFetch, OperatorBearerOptions } from '@/types/OperatorBearer'

/**
 * The enrolled identity id of the operator. The Rust
 * `identity_keys::OPERATOR_IDENTITY_ID` and `apps/web`'s
 * `DEFAULT_OPERATOR_IDENTITY_ID` are the same literal.
 */
const OPERATOR_IDENTITY_ID = 'operator'

/**
 * The domain-separation tag mixed into every signed challenge. Rust authority:
 * `identity_keys::AUTH_DOMAIN_TAG`.
 */
const AUTH_DOMAIN_TAG = 'chiefd-auth-v1'

/** The two routes chiefd's verify-middleware exempts, because they mint the
 * token every other route needs. */
const CHALLENGE_PATH = '/v1/auth/challenge'
const TOKEN_PATH = '/v1/auth/token'

/** `<keys-root>/keys/operator.key` — `identity_keys::operator_key_path`. For a
 * company daemon the keys root is `<dir>/.chief`. */
export function operatorKeyPath(keysRoot: string): string {
  return join(keysRoot, 'keys', 'operator.key')
}

/**
 * The exact bytes a caller signs, base64-encoded:
 * `utf8(tag) || utf8(identityId) || utf8(nonce)`, concatenated with NO
 * separator. Unambiguous because the daemon issues a fixed-width nonce.
 */
export function authChallengeMessage(identityId: string, nonce: string): string {
  return Buffer.concat([
    Buffer.from(AUTH_DOMAIN_TAG, 'utf8'),
    Buffer.from(identityId, 'utf8'),
    Buffer.from(nonce, 'utf8')
  ]).toString('base64')
}

/**
 * Base64 of the 64-byte IEEE-P1363 ECDSA/SHA-256 signature the Rust verifier
 * expects — raw `r||s`, NOT DER, which is what the `p256` crate's
 * `Signature::from_slice` reads.
 */
export function signAuthChallenge(message: string, privatePkcs8Pem: string): string {
  return sign('sha256', Buffer.from(message, 'base64'), {
    key: createPrivateKey(privatePkcs8Pem),
    dsaEncoding: 'ieee-p1363'
  }).toString('base64')
}

/* eslint-disable lucy/no-json-stringify */
// Same reasoning as `CompanyDaemon.seedCompany`'s own disable:
// `toJsonTreeString` is not a dependency of this package, and these are the
// only outgoing bodies it serializes.
function jsonBody(value: unknown): string {
  return JSON.stringify(value)
}
/* eslint-enable lucy/no-json-stringify */

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && !isNullish(value)
}

function readString(body: unknown, field: string, route: string): string {
  const value = isRecord(body) ? body[field] : undefined
  if (typeof value !== 'string') {
    // A live endpoint answering a shape this client cannot read is BUILD SKEW,
    // not an authentication failure, and it is worth saying so differently.
    throw new Error(`${route} answered a body with no string \`${field}\``)
  }
  return value
}

/**
 * One full challenge → sign → token round trip. Returns the minted bearer.
 *
 * Throws on any failure, and that is right HERE where every other client
 * reports and carries on: a product client refuses on its own behalf only at
 * the cost of turning a credential problem into an outage, but a TEST harness
 * that silently proceeded token-less would turn an authentication break into a
 * `401` attributed to whatever route the suite happened to call first.
 */
export async function acquireOperatorBearer(options: OperatorBearerOptions): Promise<string> {
  const identityId = options.identityId ?? OPERATOR_IDENTITY_ID
  const keyPath = operatorKeyPath(options.keysRoot)
  let privatePkcs8Pem: string
  try {
    privatePkcs8Pem = await readFile(keyPath, 'utf8')
  } catch (error) {
    throw new Error(
      `no ${identityId} key at ${keyPath}: ${String(error)}. ` +
        `\`chiefd run\` mints it at boot, so an absent one means the daemon ` +
        `never reached its credential bootstrap.`,
      { cause: error }
    )
  }

  const challengeResponse = await fetch(`${options.url}${CHALLENGE_PATH}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: jsonBody({ identityId })
  })
  if (!challengeResponse.ok) {
    throw new Error(
      `${CHALLENGE_PATH} for ${identityId} refused: ` +
        `${challengeResponse.status} ${(await challengeResponse.text()).slice(0, 400)}`
    )
  }
  const challengeBody: unknown = await challengeResponse.json()
  const nonceId = readString(challengeBody, 'nonceId', CHALLENGE_PATH)
  const nonce = readString(challengeBody, 'nonce', CHALLENGE_PATH)

  const signature = signAuthChallenge(authChallengeMessage(identityId, nonce), privatePkcs8Pem)
  const tokenResponse = await fetch(`${options.url}${TOKEN_PATH}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: jsonBody({ nonceId, signature })
  })
  if (!tokenResponse.ok) {
    throw new Error(
      `${TOKEN_PATH} for ${identityId} refused: ` +
        `${tokenResponse.status} ${(await tokenResponse.text()).slice(0, 400)}`
    )
  }
  return readString(await tokenResponse.json(), 'token', TOKEN_PATH)
}

/**
 * A `fetch` bound to one daemon that carries the operator bearer, mints it
 * lazily on the first call, and re-acquires exactly ONCE on a `401`.
 *
 * The re-acquisition is not defensive padding. chiefd's HS256 secret is
 * ephemeral unless a secret file was provisioned, so a restart invalidates
 * every live token without re-execing any client — the exact gap
 * `paneChiefdTransport` closed with `authInvalidate` for panes.
 */
export function createOperatorFetch(options: OperatorBearerOptions): AuthorizedFetch {
  let cached: string | undefined

  const send = async (
    path: string,
    init: { method?: string; headers?: Record<string, string>; body?: string } | undefined,
    token: string
  ): Promise<Response> =>
    fetch(`${options.url}${path}`, {
      method: init?.method,
      headers: { ...init?.headers, authorization: `Bearer ${token}` },
      body: init?.body
    })

  return async (path, init) => {
    const token = cached ?? (await acquireOperatorBearer(options))
    cached = token
    const response = await send(path, init, token)
    if (response.status !== 401) return response
    const fresh = await acquireOperatorBearer(options)
    cached = fresh
    return send(path, init, fresh)
  }
}
