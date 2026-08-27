// The pane's own credential, and the ONE place any in-pane caller acquires it
// (A4 — the design record ruling 4).
//
// It used to live inside `organization-intercom.ts`, which meant exactly one
// extension in a pane could authenticate: `team-ui` and every SSE reader ran
// in the SAME pane, over the SAME key, and reached chiefd with no credential
// at all — not because they had none, but because the acquirer was not
// reachable from where they were. Moving it here rather than copying it is the
// whole point: a second acquirer would be a second token cache, and the two
// would disagree the moment either re-acquired.
//
// Relative specifiers and `node:path` only — this module is inside the
// `extension-runtime` closure, which is copied FLAT into every pi-home with no
// `node_modules` and no tsconfig paths mapping.
import { FetchTransport } from '../transport/FetchTransport.js'
import type { PaneIdentity, PaneKeyRefusalReporter } from '../types/Auth.js'
import { AgentTokenManager } from './AgentToken.js'
import { readAgentKeypair } from './Identity.js'

/**
 * One {@link AgentTokenManager} per {@link PaneIdentity} for the life of this
 * process.
 *
 * A token is acquired once (challenge -> sign -> mint) and reused; the manager
 * caches it. Keyed by (url, person, PUBLIC KEY) rather than held in a single
 * slot: a token minted for one company must never be presented to another, a
 * token minted for one person is not another person's, and — the one that is
 * easy to miss — a token minted under a PREVIOUS key must not survive the key
 * being replaced. Caching on the identity's name alone would keep presenting a
 * credential the daemon has already stopped trusting. In a real pane none of
 * the three ever changes, which is exactly why it costs nothing to be precise.
 */
const paneTokenManagers = new Map<string, AgentTokenManager>()

/** Joins the three parts of a cache key. A NUL cannot appear in a URL, in a
 * person id, or in base64, so no two distinct triples can collide into one
 * string. */
const PANE_TOKEN_KEY_SEPARATOR = '\u0000'

/** The exact directory whose identity key this person signs with. It is
 * carried by the launch catalog because the Chief has no agent home. */
function paneHomeDirectory(identity: PaneIdentity): string {
  return identity.identityDir
}

/**
 * The caller's own credential (#751/P7) for one (daemon, person, key) triple,
 * or `undefined` when this pane has no usable key.
 *
 * `undefined` is a real state, not a bug to repair here: a home written before
 * P7, a person whose materialization was contained, or a key whose mode has
 * widened past 0600 and is therefore refused. The caller then calls token-less
 * and chiefd refuses it on the routes fenced to a person, with a message
 * saying so. Inventing a key would only turn a legible refusal into an
 * illegible one.
 *
 * `onRefusal` is how the last of those three stops being silent. It fires with
 * the reason and the mode; the extension decides where that goes.
 */
export function paneTokenManager(
  identity: PaneIdentity,
  onRefusal?: PaneKeyRefusalReporter
): AgentTokenManager | undefined {
  const personId = identity.personId.trim()
  const organizationDir = identity.organizationDir.trim()
  const identityDir = identity.identityDir.trim()
  const url = identity.url.trim()
  if (!personId || !organizationDir || !identityDir || !url) return undefined
  const normalized: PaneIdentity = { url, personId, organizationDir, identityDir }
  const read = readAgentKeypair(paneHomeDirectory(normalized))
  if (!read.keypair) {
    onRefusal?.(read.refusal, normalized)
    return undefined
  }
  const key = [url, personId, read.keypair.publicSpkiBase64].join(PANE_TOKEN_KEY_SEPARATOR)
  const existing = paneTokenManagers.get(key)
  if (existing) return existing
  const manager = new AgentTokenManager(
    new FetchTransport(url),
    personId,
    read.keypair.privatePkcs8Pem
  )
  paneTokenManagers.set(key, manager)
  return manager
}

/**
 * The transport every in-pane chiefd call travels on.
 *
 * `authHeader` never throws into the request path: a failed acquisition
 * returns no header and the request proceeds token-less, to be refused by
 * chiefd. The daemon, not this pane, is the authority on that.
 *
 * This used to cache the token for the life of the process and never
 * re-acquire, on the reasoning that the only thing able to retire a live token
 * — the identity key rotating — re-execs the pane, so no process could outlive
 * its own token. That reasoning missed **chiefd restarting.** The daemon's
 * HS256 signing secret rotates on restart unless a secret file was provisioned
 * (`chiefd-api/src/authn/boot.rs`), invalidating every token in flight at once
 * without re-execing anybody. Every org tool call from every surviving pane
 * then 401ed until that pane was respawned — with the daemon's own comment
 * saying clients "simply re-acquire on the resulting 401", and the client's
 * `invalidate()` having no production caller. Each side assumed the other
 * handled recovery, so neither did.
 *
 * `authInvalidate` closes it: the transport drops the cached bearer and
 * retries exactly once on a 401. That also covers a legitimate key rotation
 * and a token expiring mid-life, which a persisted secret would not.
 */
export function paneChiefdTransport(
  identity: PaneIdentity,
  onRefusal?: PaneKeyRefusalReporter
): FetchTransport {
  const manager = paneTokenManager(identity, onRefusal)
  return manager
    ? new FetchTransport(
        identity.url,
        undefined,
        () => manager.authHeader(),
        () => {
          manager.invalidate()
        }
      )
    : new FetchTransport(identity.url)
}
