//! A minimal HS256 JWT (agent-auth P0, R3), hand-rolled over `hmac` + `sha2`.
//!
//! Why hand-rolled: the claim set is deliberately unusual — NO `exp` (tokens
//! never expire; the revocation anchor is the KEY, checked per request against
//! the live `identities` row), plus a `kid` that must equal the identity's
//! CURRENT fingerprint. A general JWT crate's expiry/validation machinery is
//! exactly the behaviour we do NOT want, and `sha2` is already vendored.
//!
//! This module only mints and integrity-checks tokens. It does NOT decide
//! whether a `sub` is enrolled/active or whether `kid` still matches — that is
//! the middleware's live DB check (R3/R4), because a token that is
//! cryptographically intact must STILL be rejected once its key is revoked or
//! rotated.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The fixed header for every token: `{"alg":"HS256","typ":"JWT"}`.
const HEADER_JSON: &str = r#"{"alg":"HS256","typ":"JWT"}"#;

/// The claim set. No `exp` by design (R3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The identity id (`identities.identity_id`).
    pub sub: String,
    /// Issued-at, ms since epoch.
    pub iat: i64,
    /// The identity's fingerprint AT MINT TIME. The middleware rejects the token
    /// once the row's fingerprint differs (key rotation / epoch bump).
    pub kid: String,
    /// Coarse scope; `"all"` in v1.
    pub scope: String,
}

/// Why a token could not be decoded/verified. Never distinguishes "bad
/// signature" from "malformed" to a caller beyond the log — both are a 401.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtError {
    /// The compact form was not three base64url segments.
    Malformed,
    /// The HMAC did not match (forged or wrong secret).
    BadSignature,
    /// The payload segment was not valid `Claims` JSON.
    BadClaims,
    /// Claims could not be serialized at mint time (should never happen).
    Encode,
}

fn mac(secret: &[u8], signing_input: &[u8]) -> Result<HmacSha256, JwtError> {
    // HMAC accepts a key of any length, so this never errors in practice; we
    // still thread the Result rather than `unwrap`/`expect` (denied outside
    // tests) so a zero-length secret degrades to a clean error, not a panic.
    // hmac 0.13 moved `new_from_slice` off `Mac` and onto `KeyInit` (digest
    // 0.11 split construction from the MAC operations). Same call, same
    // fallible signature, different trait.
    let mut m = <HmacSha256 as KeyInit>::new_from_slice(secret).map_err(|_| JwtError::Encode)?;
    m.update(signing_input);
    Ok(m)
}

/// Mint a signed token for `claims` under `secret`.
///
/// # Errors
/// [`JwtError::Encode`] if the claims cannot be serialized (unreachable for the
/// fixed `Claims` shape, but never `unwrap`ed on a request path).
pub fn mint(secret: &[u8], claims: &Claims) -> Result<String, JwtError> {
    let payload_json = serde_json::to_vec(claims).map_err(|_| JwtError::Encode)?;
    let header_b64 = URL_SAFE_NO_PAD.encode(HEADER_JSON.as_bytes());
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json);
    let signing_input = format!("{header_b64}.{payload_b64}");
    let signature = mac(secret, signing_input.as_bytes())?.finalize().into_bytes();
    let signature_b64 = URL_SAFE_NO_PAD.encode(signature);
    Ok(format!("{signing_input}.{signature_b64}"))
}

/// Verify a token's HMAC and decode its claims. Does NOT consult the DB — the
/// caller (middleware) must still check `sub` is enrolled + active and `kid`
/// matches the current fingerprint.
///
/// # Errors
/// [`JwtError`] variants for a malformed token, a bad signature, or bad claims.
pub fn verify(secret: &[u8], token: &str) -> Result<Claims, JwtError> {
    let mut parts = token.split('.');
    let (Some(header_b64), Some(payload_b64), Some(signature_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Malformed);
    };
    let signing_input = format!("{header_b64}.{payload_b64}");
    let signature = URL_SAFE_NO_PAD.decode(signature_b64).map_err(|_| JwtError::Malformed)?;
    // Constant-time comparison via the MAC's own verifier.
    mac(secret, signing_input.as_bytes())?
        .verify_slice(&signature)
        .map_err(|_| JwtError::BadSignature)?;
    let payload = URL_SAFE_NO_PAD.decode(payload_b64).map_err(|_| JwtError::BadClaims)?;
    serde_json::from_slice::<Claims>(&payload).map_err(|_| JwtError::BadClaims)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims() -> Claims {
        Claims { sub: "person:a".into(), iat: 1234, kid: "fp-1".into(), scope: "all".into() }
    }

    #[test]
    fn mint_then_verify_roundtrips_the_claims() {
        let secret = b"a-32-byte-ish-daemon-secret-value";
        let token = mint(secret, &claims()).expect("mint");
        assert_eq!(verify(secret, &token).expect("verify"), claims());
    }

    #[test]
    fn a_token_has_no_exp_claim() {
        let secret = b"secret";
        let token = mint(secret, &claims()).expect("mint");
        let payload_b64 = token.split('.').nth(1).expect("payload");
        let payload = URL_SAFE_NO_PAD.decode(payload_b64).expect("decode");
        let json = String::from_utf8(payload).expect("utf8");
        assert!(!json.contains("exp"), "tokens never expire (R3): {json}");
        assert!(json.contains("\"kid\":\"fp-1\""));
        assert!(!json.contains("gen"), "a token carries no incarnation binding: {json}");
    }

    #[test]
    fn a_different_secret_does_not_verify() {
        let token = mint(b"secret-one", &claims()).expect("mint");
        assert_eq!(verify(b"secret-two", &token), Err(JwtError::BadSignature));
    }

    #[test]
    fn a_tampered_payload_does_not_verify() {
        let token = mint(b"secret", &claims()).expect("mint");
        let mut parts: Vec<&str> = token.split('.').collect();
        // Swap in a forged payload (different sub) but keep the old signature.
        let forged = Claims { sub: "operator".into(), ..claims() };
        let forged_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
        parts[1] = &forged_b64;
        let tampered = parts.join(".");
        assert_eq!(verify(b"secret", &tampered), Err(JwtError::BadSignature));
    }

    #[test]
    fn a_non_three_segment_token_is_malformed() {
        assert_eq!(verify(b"secret", "only.two"), Err(JwtError::Malformed));
        assert_eq!(verify(b"secret", "a.b.c.d"), Err(JwtError::Malformed));
        assert_eq!(verify(b"secret", "nodots"), Err(JwtError::Malformed));
    }
}
