//! Lower-case hex for a hash digest, in one place.
//!
//! # Why this exists
//!
//! Under sha2 0.10 a digest was a `GenericArray`, which implements
//! `LowerHex`, so every call site wrote `format!("{:x}", digest)`. sha2 0.11
//! (digest 0.11) returns a `hybrid_array::Array` instead, and that type does
//! NOT implement `LowerHex` — the same expression no longer compiles.
//!
//! The BYTES did not change; only the printing did. That distinction is the
//! whole risk of the upgrade: these digests are durable keys — the launch
//! fence, the event-journal idempotency key, the session-maintenance id — so a
//! hex encoding that differed by a character would silently re-key every one of
//! them and every existing row would stop matching. One function, pinned by a
//! known-answer test against the published SHA-256 vector for `"abc"`, is what
//! keeps that from being possible to get wrong twice.
use core::fmt::Write as _;

/// The lower-case, zero-padded hex of a digest — byte for byte what
/// `format!("{:x}", …)` produced under sha2 0.10.
#[must_use]
pub fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let bytes = digest.as_ref();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::hex_digest;
    use sha2::{Digest as _, Sha256};

    /// THE KNOWN ANSWER. `SHA-256("abc")` is published in FIPS 180-4 and is not
    /// derived from this code, so it catches the one failure the upgrade could
    /// otherwise hide: a hex encoding that is self-consistent (every test that
    /// computes its own expectation the same way still passes) and different
    /// from what shipped.
    #[test]
    fn the_hex_of_a_digest_matches_the_published_sha256_vector() {
        assert_eq!(
            hex_digest(Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "the durable keys built from this encoding must not move"
        );
    }

    /// Zero padding is not cosmetic: a byte below 0x10 printed as one character
    /// would shorten the string and change every id built from it.
    #[test]
    fn every_byte_takes_two_characters() {
        assert_eq!(hex_digest([0x00_u8, 0x0f, 0xff]), "000fff");
        assert_eq!(hex_digest(Sha256::digest(b"abc")).len(), 64);
    }
}
