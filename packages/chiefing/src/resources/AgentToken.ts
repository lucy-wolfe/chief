// The PANE side of agent-auth: acquire and cache one chiefd JWT for one
// identity, using that identity's own private key.
//
// Split out of `resources/Auth.ts` by #751/P7. Since P7 a Pi pane authenticates
// with the P-256 key materialization put in its pi-home — chiefd cannot see the
// terminal pane a caller descends from any more, so possession of the key is
// the proof. That put this class inside the `extension-runtime` closure, which
// is COPIED FLAT into every pi-home, and a flat copy cannot hold two files
// whose basenames collide. `resources/Auth.ts` alongside `types/Auth.ts` is
// exactly such a pair, and the collision would be silent — one file
// overwriting the other in every agent's home. Hence its own module, with only
// the imports a pane genuinely needs; enrolment (an operator concern) stays
// behind in `resources/Auth.ts`.
//
// Relative specifiers, not `@/` aliases: the copied deployment has no tsconfig
// paths mapping to resolve an alias with.
import { AuthAcquisitionError } from '../Errors.js'
import type { HttpTransport } from '../types/Transport.js'
import { authChallengeMessage, signAuthChallenge } from './Identity.js'

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object'
}

function parseJsonBody(body: string): unknown {
  try {
    return JSON.parse(body)
  } catch {
    return undefined
  }
}

function parseChallengeResponse(value: unknown): { nonceId: string; nonce: string } | undefined {
  if (!isRecord(value)) return undefined
  if (typeof value.nonceId !== 'string' || !value.nonceId) return undefined
  if (typeof value.nonce !== 'string' || !value.nonce) return undefined
  return { nonceId: value.nonceId, nonce: value.nonce }
}

function parseTokenResponse(value: unknown): { token: string } | undefined {
  if (!isRecord(value)) return undefined
  if (typeof value.token !== 'string' || !value.token) return undefined
  return { token: value.token }
}

/** Async twin of agent-jwt.ts:54. `authHeader()` never throws into the
 * request path; feeds `authHeaderProvider`. Acquires and caches a chiefd JWT
 * for one identity, using that identity's private key. One instance per
 * identity per process. */
export class AgentTokenManager {
  private cachedToken: string | undefined

  constructor(
    protected readonly transport: HttpTransport,
    protected readonly identityId: string,
    protected readonly privatePkcs8Pem: string
  ) {}

  /** The header provider wired into `FetchTransport`. Returns the cached
   * Bearer header, acquiring a token on first use. Never throws into the
   * request path: if acquisition fails here, it returns `undefined` and the
   * request proceeds token-less (and is correctly rejected 401) — the
   * daemon, not the client, is the authority. */
  async authHeader(): Promise<Record<string, string> | undefined> {
    if (!this.cachedToken) {
      try {
        this.cachedToken = await this.acquire()
      } catch {
        return undefined
      }
    }
    return { Authorization: `Bearer ${this.cachedToken}` }
  }

  /** Drop the cached token so the next `authHeader()` re-acquires (after a
   * 401). */
  invalidate(): void {
    this.cachedToken = undefined
  }

  /** Force a full challenge -> sign -> token round-trip and cache the
   * result. Exposed for the re-acquire-on-401 path and tests.
   * @throws AuthAcquisitionError on an unexpected status or malformed body. */
  async acquire(): Promise<string> {
    const challengeResponse = await this.transport.post('/v1/auth/challenge', {
      identityId: this.identityId
    })
    if (challengeResponse.status !== 200) {
      throw new AuthAcquisitionError(
        `challenge failed: status ${challengeResponse.status} body ${challengeResponse.body}`
      )
    }
    const challenge = parseChallengeResponse(parseJsonBody(challengeResponse.body))
    if (!challenge) {
      throw new AuthAcquisitionError('challenge response missing nonceId/nonce')
    }

    const message = authChallengeMessage(this.identityId, challenge.nonce)
    const signature = signAuthChallenge(message, this.privatePkcs8Pem)
    const tokenResponse = await this.transport.post('/v1/auth/token', {
      nonceId: challenge.nonceId,
      signature
    })
    if (tokenResponse.status !== 200) {
      throw new AuthAcquisitionError(
        `token mint failed: status ${tokenResponse.status} body ${tokenResponse.body}`
      )
    }
    const token = parseTokenResponse(parseJsonBody(tokenResponse.body))
    if (!token) {
      throw new AuthAcquisitionError('token response missing token')
    }
    this.cachedToken = token.token
    return token.token
  }

  /** The current cached token, if any (tests / diagnostics). */
  currentToken(): string | undefined {
    return this.cachedToken
  }
}
