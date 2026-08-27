//! Cryptographic caller authentication (agent-auth P0, the design record).
//!
//! The daemon half of the keypair -> JWT boundary Fable ratified (R2/R3):
//!
//! * [`sig`] — P-256 verification of the agent's domain-separated challenge
//!   signature, byte-matching the agent side in `agent-identity.ts`.
//! * [`jwt`] — a hand-rolled, no-expiry HS256 token whose `kid` anchors on the
//!   identity's fingerprint.
//! * [`nonce`] — the bounded, single-use, identity-bound challenge store.
//!
//! [`issue_token_for`] is the ONE issuance path both the keypair flow
//! (`/v1/auth/token`, after signature verify) and the server-side channel mint
//! (operator-pane / operator-remote, after attestation) go through, so every
//! token — however the caller proved itself — is minted and revoked the same
//! way (Fable's "SAME issuance fn + SAME revocation anchor").

pub mod boot;
pub mod jwt;
pub mod middleware;
pub mod nonce;
pub mod routes;
pub mod runtime;
pub mod sig;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use chiefd_core::store::identities::Identity;

/// Bytes in a random token: 32 -> a fixed 43-char base64url string, the
/// fixed-width nonce the domain-separation boundary relies on. Reused for
/// nonce ids, channel epochs, and (raw, un-encoded) the HS256 secret.
pub const TOKEN_BYTES: usize = 32;

/// A url-safe, fixed-width random token (nonce, nonce id, or channel epoch).
///
/// # Errors
/// Propagates a `getrandom` failure (no OS entropy) — the caller turns it into
/// a 500, never a weak token.
pub fn random_token() -> Result<String, getrandom::Error> {
    let mut buffer = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut buffer)?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

/// The canonical fingerprint of a public key: base64url(SHA-256(SPKI DER)). Used
/// as the `identities.fingerprint` for a keypair identity and as the JWT `kid`,
/// so a key uniquely and stably identifies itself. Channels do NOT use this —
/// their fingerprint is a random epoch ([`random_token`]).
#[must_use]
pub fn fingerprint_of_spki(spki_der: &[u8]) -> String {
    let digest = Sha256::digest(spki_der);
    URL_SAFE_NO_PAD.encode(digest)
}

/// 32 raw random bytes for an ephemeral HS256 daemon secret (kept raw, never
/// base64, and never logged).
///
/// # Errors
/// Propagates a `getrandom` failure.
pub fn random_secret() -> Result<[u8; TOKEN_BYTES], getrandom::Error> {
    let mut buffer = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut buffer)?;
    Ok(buffer)
}

/// Why a token could not be issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueError {
    /// The identity is revoked (`active = 0`); it gets no token.
    Inactive,
    /// The claims could not be minted.
    Jwt(jwt::JwtError),
}

/// Mint a token for an already-authenticated identity. This is the SINGLE
/// issuance path (keypair and channel alike): it refuses an inactive identity,
/// and anchors the token's `kid` on the identity's CURRENT fingerprint so a
/// later rotation invalidates it. The CALLER is responsible for having proven
/// the identity (signature verify, or channel attestation) before calling.
///
/// Authentication is PER-AGENT: the token verifies for as long as the identity
/// is active and its key unrotated. There is no incarnation binding — revocation
/// is deactivate-identity or rotate-key, and nothing else.
///
/// # Errors
/// [`IssueError::Inactive`] for a revoked identity; [`IssueError::Jwt`] if the
/// claims cannot be serialized.
pub fn issue_token_for(
    secret: &[u8],
    identity: &Identity,
    now_ms: i64,
) -> Result<String, IssueError> {
    if !identity.active {
        return Err(IssueError::Inactive);
    }
    jwt::mint(
        secret,
        &jwt::Claims {
            sub: identity.identity_id.clone(),
            iat: now_ms,
            kid: identity.fingerprint.clone(),
            scope: "all".to_string(),
        },
    )
    .map_err(IssueError::Jwt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chiefd_core::store::identities::IdentityKind;

    fn identity(active: bool, fingerprint: &str) -> Identity {
        Identity {
            identity_id: "person:a".to_string(),
            principal: "person:a".to_string(),
            kind: IdentityKind::Person,
            company_slug: Some("acme".to_string()),
            pubkey: Some("spki".to_string()),
            fingerprint: fingerprint.to_string(),
            active,
            enrolled_at: 0,
            enrolled_by: None,
            revoked_at: None,
        }
    }

    #[test]
    fn random_token_is_fixed_width_43_chars() {
        let a = random_token().expect("entropy");
        let b = random_token().expect("entropy");
        assert_eq!(a.len(), 43, "32 bytes base64url-no-pad == 43 chars");
        assert_ne!(a, b, "tokens are random");
    }

    #[test]
    fn issue_token_anchors_kid_on_the_current_fingerprint() {
        let secret = b"daemon-secret";
        let token = issue_token_for(secret, &identity(true, "fp-current"), 100).expect("issue");
        let claims = jwt::verify(secret, &token).expect("verify");
        assert_eq!(claims.sub, "person:a");
        assert_eq!(claims.kid, "fp-current");
        assert_eq!(claims.scope, "all");
        assert_eq!(claims.iat, 100);
    }

    #[test]
    fn an_inactive_identity_gets_no_token() {
        let secret = b"daemon-secret";
        assert_eq!(issue_token_for(secret, &identity(false, "fp"), 100), Err(IssueError::Inactive),);
    }
}
