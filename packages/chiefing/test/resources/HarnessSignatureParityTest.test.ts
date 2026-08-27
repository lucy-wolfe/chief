/**
 * A7. `@chief/testing` carries its own challenge signer, and it is not allowed
 * to disagree with this package's.
 *
 * WHY THE COPY EXISTS. The harness cannot import `@chief/chiefing`: this
 * package **devDepends on `@chief/testing`** — the harness is what its own
 * contract tests boot — so the reverse edge is a workspace build cycle. The
 * copy is forced, not chosen.
 *
 * WHY THIS FILE LIVES HERE. `packages/chiefing` is the only package that can
 * import BOTH halves, so it is the only place the two can be compared. A copy
 * that cannot silently disagree is one rule with two speakers, which is the
 * shape `identity_keys` already imposes on the Rust and TypeScript sides of
 * this same message.
 *
 * The Rust verifier is the third speaker and is pinned separately by
 * `IdentityTest.test.ts`'s frozen fixture; this test does not re-derive that,
 * it ties the harness to the implementation that fixture already covers.
 */
import {
  authChallengeMessage as harnessMessage,
  signAuthChallenge as harnessSign
} from '@chief/testing'
import { describe, expect, it } from 'vitest'

import {
  authChallengeMessage,
  generateAgentKeypair,
  signAuthChallenge,
  verifyAuthChallenge
} from '@/resources/Identity'

// A fixed identity and a fixed nonce: the two inputs whose concatenation is
// the whole domain-separation rule. A random pair would still pass while the
// tag drifted, because both sides would drift together only if they shared an
// implementation — which is exactly what this test denies.
const IDENTITY_ID = 'operator'
const NONCE = 'c2FtcGxlLWZpeGVkLXdpZHRoLW5vbmNl'

describe('the @chief/testing harness signs exactly what @chief/chiefing signs', () => {
  it('builds the identical domain-separated message', () => {
    expect(harnessMessage(IDENTITY_ID, NONCE)).toBe(authChallengeMessage(IDENTITY_ID, NONCE))
  })

  it('produces a signature this package verifies, and vice versa', () => {
    const { privatePkcs8Pem, publicSpkiBase64 } = generateAgentKeypair()
    const message = authChallengeMessage(IDENTITY_ID, NONCE)

    // ECDSA is randomized, so the two signatures are NOT byte-equal and
    // asserting that they were would be a test of the RNG. What must hold is
    // that each side's signature verifies under the other's rule.
    expect(
      verifyAuthChallenge(message, harnessSign(message, privatePkcs8Pem), publicSpkiBase64)
    ).toBe(true)
    expect(
      verifyAuthChallenge(
        harnessMessage(IDENTITY_ID, NONCE),
        signAuthChallenge(message, privatePkcs8Pem),
        publicSpkiBase64
      )
    ).toBe(true)
  })

  it('refuses a signature over a different nonce, so the message is load-bearing', () => {
    const { privatePkcs8Pem, publicSpkiBase64 } = generateAgentKeypair()
    const signature = harnessSign(harnessMessage(IDENTITY_ID, NONCE), privatePkcs8Pem)

    expect(
      verifyAuthChallenge(
        authChallengeMessage(IDENTITY_ID, `${NONCE}x`),
        signature,
        publicSpkiBase64
      )
    ).toBe(false)
  })
})
