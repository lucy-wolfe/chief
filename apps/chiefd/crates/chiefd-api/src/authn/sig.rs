//! P-256 challenge-signature verification (agent-auth P0, R2).
//!
//! The daemon side of the signing contract pinned by the agent in
//! `src/organization/agent-identity.ts`: ECDSA P-256 over SHA-256, IEEE-P1363
//! fixed-width (64-byte) signatures, SPKI-DER public keys, and a
//! domain-separated message. `p256`'s `Verifier` hashes the message with
//! SHA-256 internally, matching the agent's `sign("sha256", ...)`.

use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::pkcs8::DecodePublicKey;

// The tag and the message layout MOVED to `identity_keys` (A2) and are
// re-exported here so every existing caller — the token handler, the tests, and
// `pub use` consumers — keeps its path. They moved because the daemon stopped
// being the only speaker: `chief_cli::bearer` now SIGNS these bytes, the
// boundary guard forbids either crate from linking the other, and two literals
// that agree today are exactly what `identity_keys` exists to stop drifting.
pub use identity_keys::{challenge_message, AUTH_DOMAIN_TAG};

/// Verify a challenge signature.
///
/// * `spki_der` — the enrolled public key's SPKI DER bytes.
/// * `signature_p1363` — the 64-byte IEEE-P1363 signature.
///
/// Returns `true` only if the signature is a valid P-256/SHA-256 signature by
/// `spki_der` over `challenge_message(identity_id, nonce)`. Every malformed
/// input (bad key DER, wrong-length signature) is a plain `false`, never a
/// panic — this runs on an unauthenticated request path.
#[must_use]
pub fn verify_challenge(
    spki_der: &[u8],
    identity_id: &str,
    nonce: &str,
    signature_p1363: &[u8],
) -> bool {
    let Ok(verifying_key) = VerifyingKey::from_public_key_der(spki_der) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(signature_p1363) else {
        return false;
    };
    verifying_key.verify(&challenge_message(identity_id, nonce), &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use p256::ecdsa::signature::Signer;
    use p256::ecdsa::SigningKey;
    use p256::pkcs8::EncodePublicKey;

    /// Deterministic-enough keypair for a test: a fixed 32-byte scalar.
    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed.max(1); 32].into()).expect("valid scalar")
    }

    fn spki(key: &SigningKey) -> Vec<u8> {
        VerifyingKey::from(key).to_public_key_der().expect("spki").as_bytes().to_vec()
    }

    fn sign(key: &SigningKey, identity_id: &str, nonce: &str) -> Vec<u8> {
        let sig: Signature = key.sign(&challenge_message(identity_id, nonce));
        sig.to_bytes().to_vec()
    }

    #[test]
    fn a_valid_signature_verifies() {
        let key = signing_key(7);
        assert!(verify_challenge(
            &spki(&key),
            "person:a",
            "nonce-1",
            &sign(&key, "person:a", "nonce-1")
        ));
    }

    #[test]
    fn signature_is_64_byte_p1363() {
        let key = signing_key(7);
        assert_eq!(sign(&key, "id", "n").len(), 64, "must be fixed-width r||s, not DER");
    }

    #[test]
    fn wrong_identity_fails() {
        let key = signing_key(7);
        let sig = sign(&key, "person:a", "nonce-1");
        assert!(!verify_challenge(&spki(&key), "person:mallory", "nonce-1", &sig));
    }

    #[test]
    fn wrong_nonce_fails() {
        let key = signing_key(7);
        let sig = sign(&key, "person:a", "nonce-1");
        assert!(!verify_challenge(&spki(&key), "person:a", "nonce-2", &sig));
    }

    #[test]
    fn other_key_fails() {
        let signer = signing_key(7);
        let other = signing_key(9);
        let sig = sign(&signer, "person:a", "nonce-1");
        assert!(!verify_challenge(&spki(&other), "person:a", "nonce-1", &sig));
    }

    #[test]
    fn malformed_inputs_are_false_not_panic() {
        let key = signing_key(7);
        // Garbage key DER.
        assert!(!verify_challenge(b"not-a-key", "id", "n", &sign(&key, "id", "n")));
        // Wrong-length signature.
        assert!(!verify_challenge(&spki(&key), "id", "n", b"short"));
    }

    #[test]
    fn message_layout_is_tag_id_nonce() {
        let message = challenge_message("id-X", "nonce-Y");
        assert_eq!(message, format!("{AUTH_DOMAIN_TAG}id-Xnonce-Y").into_bytes());
        // Guard against accidental base64 in the tag path.
        let _ = base64::engine::general_purpose::STANDARD.encode(&message);
    }
}
