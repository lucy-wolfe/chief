//! A person's (and the daemon's) durable cryptographic identity key.
//!
//! # Why it sits beside `ensure_agent_home` rather than inside a materializer
//!
//! It used to live in `materialize::settings`, the write-once block of a
//! per-pass projection, and it was the ONE thing in that file that was never a
//! projection: the key is minted once and preserved for ever, because
//! regenerating it orphans the public half already enrolled in the trust table
//! and the returning principal is a stranger. Every neighbour it had was
//! rewritten on every pass; it was the odd one out. With the materializer gone
//! it moves next to [`crate::agent_home`], whose create-if-absent rule is the
//! same rule this file has always had.
//!
//! # One function, not two
//!
//! There were two create-once helpers — one for a person's key inside a
//! pi-home, one for the daemon's own keys under `<data-root>/keys` — and the
//! person half carried a stage/destination pair so a staged materialization
//! could carry an existing key across the promote. There is no stage any more,
//! so the pair collapsed and the two helpers became the same three lines:
//! [`ensure_identity_key`].
//!
//! # The mint is a seam, and it is bound
//!
//! [`IdentityKeyMint`] is a trait so a caller can mint without a source of
//! randomness under test. A trait with no implementation would be a person with
//! no identity, so [`SystemIdentityKeyMint`] is the one binding and it uses the
//! same curve, crate and PKCS#8 PEM encoding `chiefd-api`'s challenge verifier
//! reads — a key minted here and a signature verified there cannot disagree
//! about the format.

use std::path::Path;

use p256::ecdsa::SigningKey;
use p256::pkcs8::{EncodePrivateKey, LineEnding};

use crate::materialize::{MaterializeError, MATERIALIZE_FILESYSTEM};

/// Filename of an agent's private key inside its own home.
///
/// A distinct file (NOT `auth.json`, which is a symlink to the operator's own
/// credentials) so the identity key has its own 0600 lifecycle and is never
/// serialized beside anything the operator owns.
pub const IDENTITY_KEY_FILENAME: &str = "chiefd-identity.key.pem";

/// Mints a durable cryptographic identity.
///
/// Called at most ONCE per subject, ever.
pub trait IdentityKeyMint: Send + Sync {
    /// A fresh P-256 private key in PKCS#8 PEM form.
    ///
    /// # Errors
    /// Whatever the caller's crypto provider reports; the caller propagates it
    /// rather than proceeding without an identity.
    fn generate_pkcs8_pem(&self) -> Result<String, MaterializeError>;
}

/// Mints P-256 signing keys from the operating system's entropy.
///
/// Stateless, so one value can serve a whole roster.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemIdentityKeyMint;

impl SystemIdentityKeyMint {
    /// A mint drawing on the OS entropy source.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl IdentityKeyMint for SystemIdentityKeyMint {
    /// A fresh P-256 private key as PKCS#8 PEM.
    ///
    /// Entropy failure PROPAGATES. A mint that fell back to a weaker source, or
    /// to a fixed seed, would hand every person in the fleet the same
    /// "identity" — which is worse than refusing, because it looks like it
    /// worked.
    fn generate_pkcs8_pem(&self) -> Result<String, MaterializeError> {
        let mut seed = [0_u8; 32];
        getrandom::fill(&mut seed).map_err(|error| {
            MaterializeError::refuse(
                MATERIALIZE_FILESYSTEM,
                format!("Could not read entropy for an identity key: {error}"),
            )
        })?;
        let key = SigningKey::from_slice(&seed).map_err(|error| {
            MaterializeError::refuse(
                MATERIALIZE_FILESYSTEM,
                format!("Could not derive a P-256 identity key: {error}"),
            )
        })?;
        key.to_pkcs8_pem(LineEnding::LF).map(|pem| pem.to_string()).map_err(|error| {
            MaterializeError::refuse(
                MATERIALIZE_FILESYSTEM,
                format!("Could not encode an identity key: {error}"),
            )
        })
    }
}

/// The daemon's identity-key mint.
///
/// Not a stub, and the distinction matters: a mint that returned a fixed key
/// would hand a whole fleet one identity while looking like it worked.
#[must_use]
pub const fn host_identity_key_mint() -> SystemIdentityKeyMint {
    SystemIdentityKeyMint::new()
}

/// Create `path` as a fresh 0600 P-256 identity key, once, and never again.
///
/// Returns whether it minted, so a caller can distinguish a first provision
/// from the idempotent steady state without stat-ing the file itself.
///
/// Serves a person's key inside their agent home and the daemon-scoped
/// operator/service keys under `<dir>/.chief/keys` alike — the rule is
/// identical for both, and stating it twice is how the two drift.
///
/// # "Once" is enforced by the WRITE, not by the check
///
/// The `exists` test below is a fast path and nothing more. It cannot be the
/// rule, because between it and the write another pass can mint: this module
/// has always said "whichever writer runs first owns the anchor", and for as
/// long as the write was a `rename(2)` it was the LAST writer that owned it.
/// Measured on a real company (`4cc439341aa9`, 2026-08-20T00:23:11Z), where
/// four provisioning passes ran at once inside one daemon: one pass enrolled
/// its key, a second published a different key over the file 3 ms later, and
/// six of twenty-one people were withheld from launch for ever after with "a
/// different identity key is already enrolled for this person". The trust
/// table cannot be re-pointed — rotation is explicit — so the orphaned person
/// never recovers on their own.
///
/// [`crate::materialize::create_text_once`] therefore refuses to replace, in
/// one syscall, and a losing minter reports `false` and keeps the winner's
/// key. That holds against two tasks in one process and against two daemons on
/// one directory alike, which is why it is the write and not a lock.
///
/// # Errors
/// A directory or file that cannot be created, or an entropy failure — which
/// PROPAGATES rather than falling back to a weaker source, for the reason
/// [`SystemIdentityKeyMint`] records.
pub fn ensure_identity_key(
    path: &Path,
    mint: &dyn IdentityKeyMint,
) -> Result<bool, MaterializeError> {
    if path.exists() {
        return Ok(false);
    }
    crate::materialize::create_text_once(path, &mint.generate_pkcs8_pem()?, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real P-256 key, deterministic from a seed — the same shape the
    /// enrolment tests use, so a fixture key is one the verifier would accept.
    fn seeded_pem(seed: u8) -> String {
        SigningKey::from_slice(&[seed; 32])
            .expect("key")
            .to_pkcs8_pem(LineEnding::LF)
            .expect("pem")
            .to_string()
    }

    /// Created once at 0600, then PRESERVED — regenerating would orphan the
    /// public half already enrolled in the trust table.
    #[test]
    fn a_key_is_created_once_at_0600_and_never_regenerated() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        // A nested path on purpose: neither the keys directory on a fresh
        // company nor an agent home's parent chain is guaranteed to exist, and
        // the caller must not have to create it first.
        let path = dir.path().join("keys").join("operator.key");
        let mint = SystemIdentityKeyMint::new();

        assert!(ensure_identity_key(&path, &mint).expect("create"), "created on first call");
        let minted = std::fs::read_to_string(&path).expect("read");
        assert!(minted.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert_eq!(
            std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777,
            0o600,
            "a private key is owner-only from the first byte"
        );

        assert!(
            !ensure_identity_key(&path, &mint).expect("second call"),
            "an existing key is preserved, never re-minted"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("read again"), minted);
    }

    #[test]
    fn a_minted_key_is_pkcs8_pem_with_lf_line_endings() {
        let pem = SystemIdentityKeyMint::new().generate_pkcs8_pem().expect("mint a key");
        assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----\n"), "unexpected header: {pem}");
        assert!(pem.trim_end().ends_with("-----END PRIVATE KEY-----"));
        assert!(!pem.contains('\r'), "CRLF would not round-trip through the verifier");
    }

    /// The property that matters is DISTINCTNESS. A mint that returned the same
    /// bytes twice would give every person in a fleet one identity, and the
    /// fleet would look healthy while being trivially impersonable.
    #[test]
    fn two_mints_never_produce_the_same_key() {
        let mint = SystemIdentityKeyMint::new();
        let first = mint.generate_pkcs8_pem().expect("first key");
        let second = mint.generate_pkcs8_pem().expect("second key");
        assert_ne!(first, second);
    }

    /// TWO MINTERS, ONE ANCHOR. Both pass the existence check before either
    /// writes — the interleaving that reached a real company, where four
    /// provisioning passes ran at once inside one daemon. Whichever wins,
    /// exactly ONE call may report that it created the key, and the file must
    /// hold that call's key: a second writer that replaced the first orphans
    /// the public half already enrolled in the trust table, and the person is
    /// locked out of their own company for ever.
    #[test]
    fn two_minters_that_race_leave_one_anchor_and_one_creator() {
        use std::sync::{Arc, Barrier};

        /// A mint held at a barrier, so both callers are PAST `path.exists()`
        /// before either publishes. A fixed seed each, so the winner is
        /// identifiable from the bytes on disk.
        struct BarrierMint {
            seed: u8,
            barrier: Arc<Barrier>,
        }

        impl IdentityKeyMint for BarrierMint {
            fn generate_pkcs8_pem(&self) -> Result<String, MaterializeError> {
                self.barrier.wait();
                Ok(seeded_pem(self.seed))
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        // Nested, and absent: this is a first provision, so neither caller can
        // see the other's file when it decides to mint.
        let path = dir.path().join("agent").join("quant-head").join("chiefd-identity.key.pem");
        let barrier = Arc::new(Barrier::new(2));

        let handles: Vec<_> = [3_u8, 9_u8]
            .into_iter()
            .map(|seed| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mint = BarrierMint { seed, barrier };
                    (seed, ensure_identity_key(&path, &mint).expect("ensure"))
                })
            })
            .collect();
        let outcomes: Vec<(u8, bool)> =
            handles.into_iter().map(|handle| handle.join().expect("join")).collect();

        let creators: Vec<u8> =
            outcomes.iter().filter(|(_, created)| *created).map(|(seed, _)| *seed).collect();
        assert_eq!(creators.len(), 1, "exactly one minter may own the anchor; got {outcomes:?}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            seeded_pem(creators[0]),
            "the key on disk is the one the winning call reported creating"
        );
    }

    #[test]
    fn a_minted_key_parses_back_as_a_p256_signing_key() {
        use p256::pkcs8::DecodePrivateKey;
        let pem = SystemIdentityKeyMint::new().generate_pkcs8_pem().expect("mint a key");
        SigningKey::from_pkcs8_pem(&pem).expect("the mint's own output must round-trip");
    }
}
