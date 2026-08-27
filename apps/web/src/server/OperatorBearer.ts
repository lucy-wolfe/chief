/**
 * SERVER-ONLY: this Next server's operator bearer, one token per company
 * daemon.
 *
 * # This is a CACHE, not a second acquirer
 *
 * `helpers/OperatorChallenge.ts` does the challenge → sign → token round trip
 * through `@chief/chiefing`'s shared primitives, and it stays the only place
 * that crypto happens. What was missing was everything AROUND it: the token was
 * acquired once, handed to the browser by `POST /api/session`, and never
 * attached to anything this server sent itself. So the server-side
 * `ChiefdClient` (`CompanyChiefd.ts`) and the SSE proxy (`CompanyFeed.ts`)
 * reached chiefd with no credential at all.
 *
 * # Why the cache is keyed by chiefd URL
 *
 * apps/api is deleted and identities live in each company's own database, so a
 * token is minted BY a company's daemon and is only good there. One cached
 * token replayed at the next company 401s every call and looks like a
 * credential problem. Keying by daemon is the same rule `chief_cli::bearer`
 * applies on the Rust side.
 *
 * The KEY it signs with is that company's own, too: the daemon mints
 * `<dir>/.chief/keys/operator.key` inside the directory it serves. There is no
 * box-wide operator key left to reach for, which is why every entry point here
 * carries the company's directory beside its URL.
 *
 * # Failure is silent here and loud at the daemon
 *
 * No key on this box means no header, and chiefd answers. That is deliberate:
 * the daemon is the authority on whether a call may proceed, and this server
 * refusing on its own behalf would turn a missing file into a 500 on every
 * route. A key that exists but is group-readable reads as ABSENT, because
 * chiefing's `readIdentityKeyPem` refuses it on the same rule the daemon and
 * the CLI apply to the same file.
 */
import { operatorIdentityId, operatorPrivateKeyPem } from '@/common/Env'
import { acquireOperatorToken } from '@/helpers/OperatorChallenge'

/** One bearer per company daemon, for the life of this server process. */
const tokens = new Map<string, string>()

/** The two hooks `ChiefdClient` takes: a per-request header and a way to drop
 * a bearer chiefd has just refused. Not exported — it is `operatorAuth`'s
 * return type and nothing else names it, so exporting it would be an
 * unconsumed surface. */
interface OperatorAuth {
  authHeaderProvider: () => Promise<Record<string, string> | undefined>
  authInvalidate: () => void
}

/** The cached bearer for one daemon, acquiring one on first use. `undefined`
 * when this box has no usable operator key, or when the daemon refused to mint
 * — never a throw into the request path. */
async function bearerFor(chiefdUrl: string, companyDir: string): Promise<string | undefined> {
  const cached = tokens.get(chiefdUrl)
  if (typeof cached === 'string') return cached

  const privateKeyPem = operatorPrivateKeyPem(companyDir)
  if (!privateKeyPem) return undefined

  try {
    const token = await acquireOperatorToken({
      apiUrl: chiefdUrl,
      identityId: operatorIdentityId(),
      privateKeyPem
    })
    tokens.set(chiefdUrl, token)
    return token
  } catch {
    // The daemon is the authority. A failed acquisition sends the request
    // token-less and lets chiefd answer, rather than converting a credential
    // problem into an outage of every route on this server.
    return undefined
  }
}

/**
 * The `Authorization` header for one company's chiefd, or nothing.
 *
 * For callers that hold a raw `fetch` rather than a `ChiefdClient` — the SSE
 * proxy is the only one, because an event stream must be forwarded as bytes.
 */
export async function operatorAuthorizationHeaders(
  chiefdUrl: string,
  companyDir: string
): Promise<Record<string, string>> {
  const token = await bearerFor(chiefdUrl, companyDir)
  if (!token) return {}
  return { Authorization: `Bearer ${token}` }
}

/**
 * The auth hooks for one company's `ChiefdClient`.
 *
 * `authInvalidate` is what makes a chiefd restart recoverable: the daemon's
 * HS256 signing secret is ephemeral unless a secret file was provisioned, so a
 * restart rotates it and every cached bearer becomes garbage at the same
 * instant. `FetchTransport` drops the token and retries exactly once on a 401.
 */
export function operatorAuth(chiefdUrl: string, companyDir: string): OperatorAuth {
  return {
    authHeaderProvider: async () => {
      const token = await bearerFor(chiefdUrl, companyDir)
      return token ? { Authorization: `Bearer ${token}` } : undefined
    },
    authInvalidate: () => {
      tokens.delete(chiefdUrl)
    }
  }
}
