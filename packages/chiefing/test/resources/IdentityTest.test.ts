import { chmodSync, mkdtempSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

import {
  authChallengeMessage,
  ensurePersonIdentityKey,
  generateAgentKeypair,
  IDENTITY_KEY_FILENAME,
  loadOrCreateAgentKeypair,
  operatorKeyPath,
  publicSpkiBase64FromPrivatePem,
  readAgentKeypair,
  readIdentityKeyPem,
  signAuthChallenge,
  verifyAuthChallenge
} from '@/resources/Identity'

function tempDir(): string {
  return mkdtempSync(join(tmpdir(), 'chiefing-identity-test-'))
}

/** Every regular file under `root`, recursively — used to prove a run
 * touches nothing outside the pi-home it was handed. */
function listFilesRecursive(root: string): string[] {
  const out: string[] = []
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const full = join(root, entry.name)
    if (entry.isDirectory()) out.push(...listFilesRecursive(full))
    else out.push(full)
  }
  return out
}

// #835 (ported from tests/agent-jwt.test.ts's "the identity key filename is
// stable (contract with materialization)"): every test above imports
// IDENTITY_KEY_FILENAME symbolically and none pins its actual string value —
// apps/cli's materialization writes/reads this exact filename independently,
// so a rename here silently breaks that contract with nothing catching it.
describe('IDENTITY_KEY_FILENAME', () => {
  it('is a stable literal — a rename here is a breaking change for materialization', () => {
    expect(IDENTITY_KEY_FILENAME).toBe('chiefd-identity.key.pem')
  })
})

describe('generateAgentKeypair / publicSpkiBase64FromPrivatePem', () => {
  it('derives the same public key from the private key it generated', () => {
    const keypair = generateAgentKeypair()
    expect(publicSpkiBase64FromPrivatePem(keypair.privatePkcs8Pem)).toBe(keypair.publicSpkiBase64)
  })
})

describe('loadOrCreateAgentKeypair', () => {
  it('generates and persists on first use, then loads the same key on re-entry', async () => {
    const piHome = tempDir()
    const created = await loadOrCreateAgentKeypair(piHome)
    const loaded = await loadOrCreateAgentKeypair(piHome)
    expect(loaded).toEqual(created)
  })

  it('writes only the identity key file, nowhere outside the pi-home it was handed', async () => {
    const piHome = tempDir()
    await loadOrCreateAgentKeypair(piHome)
    const files = listFilesRecursive(piHome)
    expect(files).toEqual([join(piHome, IDENTITY_KEY_FILENAME)])
  })

  it('persists the key 0600', async () => {
    const piHome = tempDir()
    await loadOrCreateAgentKeypair(piHome)
    const mode = statSync(join(piHome, IDENTITY_KEY_FILENAME)).mode & 0o777
    expect(mode).toBe(0o600)
  })

  // #835 (ported from tests/agent-identity.test.ts): distinct from the
  // round-trip test above — this seeds the key file directly (bypassing
  // loadOrCreateAgentKeypair entirely) to prove an EXTERNALLY-written key is
  // adopted byte-for-byte, not merely that the function agrees with itself
  // across two calls.
  it('adopts an existing key file written by something other than itself', async () => {
    const piHome = tempDir()
    const seeded = generateAgentKeypair()
    writeFileSync(join(piHome, IDENTITY_KEY_FILENAME), seeded.privatePkcs8Pem, { mode: 0o600 })
    const loaded = await loadOrCreateAgentKeypair(piHome)
    expect(loaded.privatePkcs8Pem).toBe(seeded.privatePkcs8Pem)
    expect(loaded.publicSpkiBase64).toBe(seeded.publicSpkiBase64)
    expect(readFileSync(join(piHome, IDENTITY_KEY_FILENAME), 'utf8')).toBe(seeded.privatePkcs8Pem)
  })
})

describe('ensurePersonIdentityKey', () => {
  it('generates into the stage when neither stage nor final has a key', async () => {
    const stageRoot = tempDir()
    const finalRoot = tempDir()
    const stageKeyPath = join(stageRoot, IDENTITY_KEY_FILENAME)
    const finalKeyPath = join(finalRoot, IDENTITY_KEY_FILENAME)

    const pubkey = await ensurePersonIdentityKey(stageKeyPath, finalKeyPath)

    expect(listFilesRecursive(stageRoot)).toEqual([stageKeyPath])
    expect(listFilesRecursive(finalRoot)).toEqual([])
    expect(pubkey).toBe(publicSpkiBase64FromPrivatePem(readFileSync(stageKeyPath, 'utf8')))
  })

  it('preserves the FINAL key by copying it into the stage, never regenerating', async () => {
    const stageRoot = tempDir()
    const finalRoot = tempDir()
    const stageKeyPath = join(stageRoot, IDENTITY_KEY_FILENAME)
    const finalKeyPath = join(finalRoot, IDENTITY_KEY_FILENAME)
    const existing = await loadOrCreateAgentKeypair(finalRoot)

    const pubkey = await ensurePersonIdentityKey(stageKeyPath, finalKeyPath)

    expect(pubkey).toBe(existing.publicSpkiBase64)
    expect(readFileSync(stageKeyPath, 'utf8')).toBe(existing.privatePkcs8Pem)
  })

  it('is a no-op when the stage already has a key', async () => {
    const stageRoot = tempDir()
    const finalRoot = tempDir()
    const stageKeyPath = join(stageRoot, IDENTITY_KEY_FILENAME)
    const finalKeyPath = join(finalRoot, IDENTITY_KEY_FILENAME)
    const existing = await loadOrCreateAgentKeypair(stageRoot)

    const pubkey = await ensurePersonIdentityKey(stageKeyPath, finalKeyPath)

    expect(pubkey).toBe(existing.publicSpkiBase64)
    expect(listFilesRecursive(finalRoot)).toEqual([])
  })
})

describe('readAgentKeypair', () => {
  it('reads an owner-only key', async () => {
    const piHome = tempDir()
    const written = await loadOrCreateAgentKeypair(piHome)
    const read = readAgentKeypair(piHome)
    expect(read.keypair?.publicSpkiBase64).toBe(written.publicSpkiBase64)
    expect(read.refusal).toBeUndefined()
  })

  /** A4: absence is still NO KEY, and is now labelled as such. It is a real,
   * benign, documented state — a home written before #751/P7 — and the
   * reporting path deliberately stays quiet about it, so it must be
   * distinguishable from the refusals below rather than folded in with them. */
  it('reports absence as a refusal named `absent`, naming the file it looked for', () => {
    const piHome = tempDir()
    const read = readAgentKeypair(piHome)
    expect(read.keypair).toBeUndefined()
    expect(read.refusal?.reason).toBe('absent')
    expect(read.refusal?.keyPath).toBe(join(piHome, IDENTITY_KEY_FILENAME))
  })

  /** THE READER HALF THAT WAS MISSING. Both writers create these keys 0600
   * and neither reader looked, so a key whose mode had widened after it was
   * written loaded exactly as happily as one that had not. */
  it('refuses a group- or world-readable key rather than authenticating with it', async () => {
    for (const mode of [0o640, 0o604, 0o644, 0o666]) {
      const piHome = tempDir()
      await loadOrCreateAgentKeypair(piHome)
      const keyPath = join(piHome, IDENTITY_KEY_FILENAME)
      chmodSync(keyPath, mode)
      const read = readAgentKeypair(piHome)
      expect(read.keypair, `mode ${mode.toString(8)}`).toBeUndefined()
      // A4: the refusal now SAYS which rule fired and on which bits. A caller
      // that only learns "no key" cannot tell a `chmod` away from a
      // materialization that never ran, and the pane-side report exists
      // precisely to tell an operator which one it is.
      expect(read.refusal?.reason, `mode ${mode.toString(8)}`).toBe('permissive-mode')
      expect(read.refusal?.keyPath).toBe(keyPath)
      expect(read.refusal?.mode).toBe(mode)
      // The key is still THERE and still readable by this process — the
      // refusal is the mode rule, not an I/O failure.
      expect(readFileSync(keyPath, 'utf8')).toContain('PRIVATE KEY')
    }
  })

  it('accepts a stricter mode — 0400 is owner-only too', async () => {
    const piHome = tempDir()
    await loadOrCreateAgentKeypair(piHome)
    chmodSync(join(piHome, IDENTITY_KEY_FILENAME), 0o400)
    expect(readAgentKeypair(piHome).keypair).toBeDefined()
  })
})

describe('sign/verify round trip', () => {
  it('a signature signAuthChallenge produces verifies with verifyAuthChallenge', () => {
    const keypair = generateAgentKeypair()
    const message = authChallengeMessage('person-1', 'a-nonce-value')
    const signature = signAuthChallenge(message, keypair.privatePkcs8Pem)
    expect(verifyAuthChallenge(message, signature, keypair.publicSpkiBase64)).toBe(true)
  })

  it('rejects a signature over a different message', () => {
    const keypair = generateAgentKeypair()
    const message = authChallengeMessage('person-1', 'a-nonce-value')
    const signature = signAuthChallenge(message, keypair.privatePkcs8Pem)
    const otherMessage = authChallengeMessage('person-1', 'a-different-nonce')
    expect(verifyAuthChallenge(otherMessage, signature, keypair.publicSpkiBase64)).toBe(false)
  })

  // #835 (ported from tests/agent-identity.test.ts): the prior test only
  // varies the nonce half of the message; these two are the identityId half
  // (domain binding) and the keypair half (key binding) — distinct failure
  // modes a single "rejects a different message" case doesn't distinguish.
  it('a signature for one identityId does not verify for another (domain binding)', () => {
    const keypair = generateAgentKeypair()
    const message = authChallengeMessage('person-alice', 'shared-nonce')
    const signature = signAuthChallenge(message, keypair.privatePkcs8Pem)
    const otherIdentityMessage = authChallengeMessage('person-mallory', 'shared-nonce')
    expect(verifyAuthChallenge(otherIdentityMessage, signature, keypair.publicSpkiBase64)).toBe(
      false
    )
  })

  it("another keypair's public key does not verify the signature", () => {
    const signer = generateAgentKeypair()
    const other = generateAgentKeypair()
    const message = authChallengeMessage('person-1', 'a-nonce-value')
    const signature = signAuthChallenge(message, signer.privatePkcs8Pem)
    expect(verifyAuthChallenge(message, signature, other.publicSpkiBase64)).toBe(false)
  })

  it('authChallengeMessage is exactly utf8(AUTH_DOMAIN_TAG) || utf8(identityId) || utf8(nonce), no separator', () => {
    // Golden value captured, at story time, from the CURRENT src
    // implementation (org-durable-store.ts / agent-identity.ts's
    // authChallengeMessage), so drift in the domain-separation formula is
    // caught by byte comparison, not merely "it still verifies".
    expect(authChallengeMessage('person-golden', 'golden-nonce-000000000000000000')).toBe(
      'Y2hpZWZkLWF1dGgtdjFwZXJzb24tZ29sZGVuZ29sZGVuLW5vbmNlLTAwMDAwMDAwMDAwMDAwMDAwMA=='
    )
  })

  it('accepts a signature frozen from the current implementation — guards the raw IEEE-P1363 wire format', () => {
    // Captured once (bun -e against this file's own implementation) and
    // hardcoded: proves the VERIFY path still accepts a real
    // base64/raw-P1363/no-separator signature. ECDSA signing itself is
    // non-deterministic (a fresh k per signature), so this pins the decode
    // format rather than a reproducible sign() output.
    const pubSpkiBase64 =
      'MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAECvDfe/THg19CEn50h/Ccc+tuia3K+pXz4lsvO5CWq0jbpCxVdoU99xijNqkIIOQYxkrzXWbEaQIxm42ZZpSZwA=='
    const message = authChallengeMessage('person-golden', 'golden-nonce-000000000000000000')
    const signature =
      'Ald/1F5RJB+858iina24c99Ck3b0uhqbOMH8SomYTTHR1f23STsQMP5yphtYQrN2dc6wlrenEG468NGCd1/RSQ=='
    expect(verifyAuthChallenge(message, signature, pubSpkiBase64)).toBe(true)
  })
})

// A2: the NON-person credentials — the operator's key and the actuator's.
// The Rust authority is `identity_keys`, which the daemon and the `chiefd` CLI
// both link; this is the same derivation and the same permission rule for the
// one Node caller that needs them, apps/web's server.
describe('non-person identity keys', () => {
  it('derives <dir>/.chief/keys/operator.key, and never a .env', () => {
    // One operator per COMPANY, and the company is the directory. `.key` and
    // never `.env` — a `.env` suffix invites somebody to `source` a private
    // key.
    expect(operatorKeyPath('/work/anvils/.chief')).toBe('/work/anvils/.chief/keys/operator.key')
    expect(operatorKeyPath('/work/anvils/.chief')).not.toContain('.env')
    // A different directory is a different company, even under the same name.
    expect(operatorKeyPath('/elsewhere/anvils/.chief')).not.toBe(
      operatorKeyPath('/work/anvils/.chief')
    )
    // And the key stays INSIDE the company it belongs to, so it moves, backs
    // up and is destroyed with it.
    expect(operatorKeyPath('/work/anvils/.chief').startsWith('/work/anvils/')).toBe(true)
  })

  it('reads an owner-only key and refuses one anybody else can read', () => {
    // THE RULE. Both writers already create these keys 0600; the readers are
    // where it is enforced, and the daemon
    // (`identity_keys::load_private_key_pem`) refuses the same file on the
    // same test. A key whose mode widened after it was written must not load
    // just because it once was correct.
    const dir = tempDir()
    const keyPath = join(dir, 'operator.key')
    const keypair = generateAgentKeypair()
    writeFileSync(keyPath, keypair.privatePkcs8Pem, { mode: 0o600 })
    chmodSync(keyPath, 0o600)
    expect(readIdentityKeyPem(keyPath)).toBe(keypair.privatePkcs8Pem)

    // 0400 is STRICTER, and stricter is never a refusal.
    chmodSync(keyPath, 0o400)
    expect(readIdentityKeyPem(keyPath)).toBe(keypair.privatePkcs8Pem)

    for (const mode of [0o640, 0o604, 0o644, 0o660, 0o666]) {
      chmodSync(keyPath, mode)
      expect(readIdentityKeyPem(keyPath)).toBeUndefined()
    }
  })

  it('an absent key is undefined rather than a throw', () => {
    // The daemon MINTS this file at boot, so a box that has never run one
    // legitimately has none. A throw here would land in a request path.
    expect(readIdentityKeyPem(join(tempDir(), 'never-minted.key'))).toBeUndefined()
  })
})
