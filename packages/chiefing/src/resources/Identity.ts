// agent-auth (P0) — the AGENT side of the cryptographic-identity boundary.
// Every agent holds a P-256 keypair; the private key is its durable,
// revocable identity anchor and NEVER leaves the box. Pure crypto plus
// path-injected key-file I/O — a Pi-home artifact explicitly carved out by
// Mandate 2 / ruling D20. This is the ONLY module in the package that may
// import `node:fs`, and every path it touches is an argument, never resolved
// from env or cwd.
//
// ── WIRE CONTRACT WITH THE RUST VERIFIER (must match exactly) ─────────────
// Signature scheme: ECDSA P-256 (prime256v1) over SHA-256, IEEE-P1363
// encoding (raw 64-byte r||s) — NOT DER — so the Rust `p256` crate verifies
// it with `Signature::from_slice` (its default fixed-width form).
// Public key on the wire: SPKI DER, base64 (standard, not url).
// Signed message (domain-separated): the bytes
//     utf8("chiefd-auth-v1") || utf8(identityId) || utf8(nonce)
// concatenated with NO separator, unambiguous because the daemon issues a
// fixed-width nonce. `authChallengeMessage` returns those bytes as base64 (a
// stable, serializable form) rather than a raw Buffer, so `signAuthChallenge`/
// `verifyAuthChallenge` compose on a plain string; the byte sequence signed
// is identical to org-durable-store.ts's Buffer-returning predecessor.

import { createPrivateKey, createPublicKey, generateKeyPairSync, sign, verify } from 'node:crypto'
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  statSync,
  writeFileSync
} from 'node:fs'
import { dirname, join } from 'node:path'

// Relative, not `@/`: this module is in the pane-facing `extension-runtime`
// closure (#751/P7), which resolves without a paths mapping.
import { isNullish } from '../Nullish.js'
import type { AgentKeypair, AgentKeypairRead } from '../types/Auth.js'

/** The domain-separation tag mixed into every signed challenge. Rust
 * authority: apps/chiefd/crates/chiefd-api/src/authn/sig.rs. */
export const AUTH_DOMAIN_TAG = 'chiefd-auth-v1'

/** The named curve; `prime256v1` is OpenSSL's name for NIST P-256. */
const CURVE = 'prime256v1'

/** Filename of the agent's private key inside its pi-home. A distinct file
 * (NOT auth.json, which holds provider creds) so the identity key has its
 * own 0600 lifecycle and is never serialized beside model/provider material. */
export const IDENTITY_KEY_FILENAME = 'chiefd-identity.key.pem'

/** Generate a fresh P-256 keypair. */
export function generateAgentKeypair(): AgentKeypair {
  const { privateKey, publicKey } = generateKeyPairSync('ec', {
    namedCurve: CURVE,
    privateKeyEncoding: { type: 'pkcs8', format: 'pem' },
    publicKeyEncoding: { type: 'spki', format: 'der' }
  })
  return {
    privatePkcs8Pem: privateKey,
    publicSpkiBase64: publicKey.toString('base64')
  }
}

/** Derive the SPKI-DER-base64 public key from a private-key PEM. */
export function publicSpkiBase64FromPrivatePem(privatePkcs8Pem: string): string {
  const pub = createPublicKey({ key: privatePkcs8Pem, format: 'pem' })
  return pub.export({ type: 'spki', format: 'der' }).toString('base64')
}

/** Load the agent's keypair from its pi-home, generating and persisting one
 * 0600 on first use. `piHome` is always an argument — never resolved from
 * env or cwd (Mandate 2 / ruling D20). */
export async function loadOrCreateAgentKeypair(piHome: string): Promise<AgentKeypair> {
  const keyPath = join(piHome, IDENTITY_KEY_FILENAME)
  if (existsSync(keyPath)) {
    const privatePkcs8Pem = readFileSync(keyPath, 'utf8')
    return { privatePkcs8Pem, publicSpkiBase64: publicSpkiBase64FromPrivatePem(privatePkcs8Pem) }
  }
  const keypair = generateAgentKeypair()
  mkdirSync(dirname(keyPath), { recursive: true })
  // 0600 from the first byte: write with the restrictive mode, then chmod to
  // defend against a permissive umask having widened the create.
  writeFileSync(keyPath, keypair.privatePkcs8Pem, { mode: 0o600 })
  chmodSync(keyPath, 0o600)
  return keypair
}

/** Materialization helper: ensure a person's identity key exists at their
 * STAGE pi-home, swap-safely and create-ONCE — the key is the durable
 * identity anchor and must NEVER be regenerated (that would orphan the
 * enrolled public key). If the FINAL home already has a key, it is preserved
 * (copied into the stage so the atomic stage->destination swap keeps it);
 * otherwise a fresh one is generated into the stage. Returns the
 * SPKI-DER-base64 public key to enrol. */
export async function ensurePersonIdentityKey(
  stageKeyPath: string,
  finalKeyPath: string
): Promise<string> {
  if (existsSync(stageKeyPath)) {
    return publicSpkiBase64FromPrivatePem(readFileSync(stageKeyPath, 'utf8'))
  }
  if (existsSync(finalKeyPath)) {
    if (finalKeyPath !== stageKeyPath) {
      mkdirSync(dirname(stageKeyPath), { recursive: true })
      copyFileSync(finalKeyPath, stageKeyPath)
      chmodSync(stageKeyPath, 0o600)
    }
    return publicSpkiBase64FromPrivatePem(readFileSync(stageKeyPath, 'utf8'))
  }
  const keypair = generateAgentKeypair()
  mkdirSync(dirname(stageKeyPath), { recursive: true })
  writeFileSync(stageKeyPath, keypair.privatePkcs8Pem, { mode: 0o600 })
  chmodSync(stageKeyPath, 0o600)
  return keypair.publicSpkiBase64
}

/** The exact bytes an agent signs for a challenge, base64-encoded. Kept as
 * one function so every caller — sign, verify, and the test-suite — shares
 * ONE definition of the signed message. */
export function authChallengeMessage(identityId: string, nonce: string): string {
  return Buffer.concat([
    Buffer.from(AUTH_DOMAIN_TAG, 'utf8'),
    Buffer.from(identityId, 'utf8'),
    Buffer.from(nonce, 'utf8')
  ]).toString('base64')
}

/** Sign a daemon challenge message (as returned by `authChallengeMessage`).
 * Returns the base64 (standard) of the 64-byte IEEE-P1363 ECDSA/SHA-256
 * signature the Rust verifier expects. */
export function signAuthChallenge(message: string, privatePkcs8Pem: string): string {
  const key = createPrivateKey(privatePkcs8Pem)
  const signature = sign('sha256', Buffer.from(message, 'base64'), {
    key,
    dsaEncoding: 'ieee-p1363'
  })
  return signature.toString('base64')
}

/** Read an agent's keypair from its pi-home, or say why there is none.
 * Deliberately NOT `loadOrCreateAgentKeypair`: a pane authenticating with a
 * key it just invented would present an unenrolled identity and be refused,
 * and it would also write a second durable anchor beside the one
 * materialization owns. Absence is a fact to report, never one to repair.
 *
 * A refusal is a VALUE, never a throw. This is called per request through the
 * pane transport, and a throw there would turn a credential-hygiene problem
 * into an outage. The caller then authenticates token-less and chiefd answers
 * a legible refusal on the routes fenced to a person. */
export function readAgentKeypair(piHome: string): AgentKeypairRead {
  const keyPath = join(piHome, IDENTITY_KEY_FILENAME)
  if (!existsSync(keyPath)) return { refusal: { reason: 'absent', keyPath } }
  const mode = permissionBits(keyPath)
  if (!isNullish(mode) && (mode & 0o077) !== 0) {
    return { refusal: { reason: 'permissive-mode', keyPath, mode } }
  }
  try {
    const privatePkcs8Pem = readFileSync(keyPath, 'utf8')
    return {
      keypair: {
        privatePkcs8Pem,
        publicSpkiBase64: publicSpkiBase64FromPrivatePem(privatePkcs8Pem)
      }
    }
  } catch {
    return { refusal: { reason: 'unreadable', keyPath } }
  }
}

/**
 * The permission bits of a private key, or `undefined` when they cannot be
 * read at all — in which case the mode rule below abstains and the read
 * itself reports whatever it finds.
 *
 * The mode rule: both writers already create these keys 0600 —
 * `loadOrCreateAgentKeypair` above and `chiefd_host`'s materializer, each
 * pinned by its own test — and until #751 NEITHER reader looked, so a key
 * whose mode had widened after it was written loaded exactly as happily as
 * one that had not. The strict half was the one nobody could reach with a
 * stolen file.
 *
 * A too-permissive key is treated as NO usable key rather than thrown on. A
 * throw would land in the request path of every org tool call —
 * `readAgentKeypair` is called per request through the pane transport — and
 * would turn a credential-hygiene problem into an outage. Refusing instead
 * gives the caller the same shape an absent key already has: it authenticates
 * token-less and chiefd answers a legible refusal on the routes fenced to a
 * person.
 *
 * HOW THE REFUSAL IS REPORTED, since this module cannot report it itself
 * (A4). It is copied FLAT into every pi-home and imports nothing but `node:*`
 * and a type; the package has no logger, and `console` is banned by lint
 * precisely so a shared logger is used instead. So it does not log — it
 * RETURNS the reason, as an `AgentKeyRefusal` naming the file and the exact
 * mode, and `organization-intercom.ts` (which can log, owns the pane's token
 * manager, and already owns the durable `bus/events.jsonl` failure trail)
 * turns that value into a report. This used to be a silent `undefined`
 * indistinguishable from "no key was ever written", which — now that the
 * daemon refuses a bad mode outright (A1) — meant a pane simply stopped
 * working with nothing anywhere saying why.
 *
 * A key whose mode cannot be read at all yields `undefined` here; the mode
 * rule abstains and the read reports whatever the open finds.
 */
function permissionBits(keyPath: string): number | undefined {
  try {
    return statSync(keyPath).mode & 0o777
  } catch {
    return undefined
  }
}

/**
 * `<keysRoot>/keys/operator.key` — the operator's private key, where
 * `keysRoot` is a company's own `<dir>/.chief`.
 *
 * The Rust authority is `identity_keys::keys_dir`, which the daemon and the
 * `chief` CLI both link: one operator identity per COMPANY, minted 0600 at
 * daemon boot and never configured. This is the same derivation for the one
 * Node caller that needs it — apps/web's server, which is co-located with the
 * daemon and authenticates as the same principal.
 *
 * `.key`, never `.env`: a `.env` suffix invites somebody to `source` a private
 * key. The root is an ARGUMENT, like every other path in this module.
 *
 * This used to take a "data root" and its doc had to explain at length that
 * the data root (`~/.chiefd`) was not the orgs root (`~/.chiefd/orgs`) that
 * `CHIEFD_DATA_ROOT` confusingly held — two names one directory apart, a
 * collision `identity_keys`' module doc records the cost of. Neither root
 * exists now: a company's keys are inside the company.
 */
export function operatorKeyPath(keysRoot: string): string {
  return join(keysRoot, 'keys', 'operator.key')
}

/**
 * Read a non-person identity key (the operator's, the actuator's), refusing
 * one that is not owner-only.
 *
 * The same mode rule `readAgentKeypair` applies to a pane's key and
 * `identity_keys::load_private_key_pem` applies to the daemon's, through the
 * same `permissionBits` helper, so one file is never judged three ways.
 * `undefined` for absent, unreadable, or group/world-readable.
 *
 * WHY THIS RETURNS `undefined` RATHER THAN A4's `AgentKeyRefusal` VALUE, since
 * the difference is deliberate and the reasoning is the opposite of a
 * shortcut. A4 gave the PANE's read a refusal value because a pane's key has
 * exactly one reader: a silent `undefined` there meant a pane stopped working
 * with nothing anywhere saying why. The operator key has THREE readers on the
 * same box, and the other two already refuse it loudly by name — the daemon
 * refuses to serve (`identity_keys::load_private_key_pem`, naming the file and
 * the `chmod 600`), and the `chiefd` CLI refuses the request before dialling
 * (`BearerError::KeyTooPermissive`). A widened mode on this file is therefore
 * already reported, twice, by the two callers that CAN report. This one is a
 * web server whose honest answer to "is there a usable operator key" is yes or
 * no.
 */
export function readIdentityKeyPem(keyPath: string): string | undefined {
  if (!existsSync(keyPath)) return undefined
  const mode = permissionBits(keyPath)
  if (!isNullish(mode) && (mode & 0o077) !== 0) return undefined
  try {
    return readFileSync(keyPath, 'utf8')
  } catch {
    return undefined
  }
}

/** Verify a challenge signature against an SPKI-DER-base64 public key. This
 * mirrors what the daemon does; it exists so unit tests can prove the
 * sign/verify pair without a daemon, and it is NOT used in the request path. */
export function verifyAuthChallenge(
  message: string,
  signature: string,
  publicSpkiBase64: string
): boolean {
  return verify(
    'sha256',
    Buffer.from(message, 'base64'),
    {
      key: Buffer.from(publicSpkiBase64, 'base64'),
      format: 'der',
      type: 'spki',
      dsaEncoding: 'ieee-p1363'
    },
    Buffer.from(signature, 'base64')
  )
}
