//! Where a non-person identity key lives on disk, and the mode it must have.
//!
//! # One operator per COMPANY, because the company is the directory
//!
//! The operator is one principal (agent-auth P0), and the boundary is now the
//! company directory: `<dir>/.chief` holds everything chief owns for the
//! company standing in `<dir>`. So the key is `<dir>/.chief/keys/operator.key`
//! and the actuator's is `<dir>/.chief/keys/service.key`, beside the HS256
//! signing secret, so ONE directory is the whole trust root — and it moves,
//! backs up and is destroyed with the company it belongs to.
//!
//! It is DERIVED, never configured. `CHIEFD_OPERATOR_KEY_PATH` used to name
//! it, and an unset trust anchor is an off switch: the daemon enrolled no
//! operator, warned, and served anyway. Nothing here reads the environment.
//!
//! # TOMBSTONE: "the two roots have the same name and are different directories"
//!
//! This crate used to carry a whole section on that hazard, and a second entry
//! point to survive it. `chiefd run --data-root` and `CHIEFD_DATA_ROOT` did
//! NOT mean the data root — they meant the ORGS root, `<data-root>/orgs`,
//! where a company's `.<slug>.chief.db` sat. The collision cost a full day
//! (#13): a control-plane write passed `~/.chiefd`, a database was created
//! there, the write succeeded against it, and the daemon one directory below
//! never saw it. So [`keys_dir`] took the data root, `keys_dir_from_orgs_root`
//! took the orgs root, and each was named for what its caller actually held.
//!
//! Both roots are gone and so is the second entry point. There is one root per
//! company, it is inside the company, and nothing above it is chief's — which
//! removes the hazard rather than documenting it. The flags that named the old
//! roots are deleted too; `chiefd run` takes `--dir`.
//!
//! # The permission rule
//!
//! A private key readable by anyone but its owner is a key you must assume is
//! copied. Both existing writers already create person keys `0600`
//! (`chiefd_host`'s materializer and `@chief/chiefing`'s `writeAgentKeypair`),
//! but NEITHER reader checked the mode before loading — so the strict half was
//! the one nobody could reach with a stolen file. [`assert_owner_only`] is the
//! missing half, and it refuses rather than warns.

use std::io;
use std::path::{Path, PathBuf};

/// The enrolled identity id of the operator. `apps/web` already defaults to
/// this exact literal (`DEFAULT_OPERATOR_IDENTITY_ID`), and the daemon's
/// bootstrap enrolment already hardcodes it; this is that one name, written
/// once.
pub const OPERATOR_IDENTITY_ID: &str = "operator";

/// The enrolled identity id of the resident actuator.
///
/// A SEPARATE principal from the operator on purpose. The actuator's actions
/// are automatic and the operator's are deliberate, and an audit trail that
/// cannot tell them apart is worth much less than one that can — which is the
/// whole reason the staffing routes are losing `String::new()` as their actor.
pub const SERVICE_IDENTITY_ID: &str = "service";

/// The domain-separation tag mixed into every signed challenge.
///
/// It lives here because BOTH halves must say it the same way and neither may
/// link the other: the daemon verifies with it (`chiefd_api::authn::sig`, which
/// re-exports this) and the operator client signs with it
/// (`chief_cli::bearer`). It used to be a literal in the daemon alone, which
/// was safe only while nothing else spelled it; A2 gives it a second speaker,
/// and two literals that agree today are the drift this crate exists to
/// prevent. `@chief/chiefing`'s `AUTH_DOMAIN_TAG` is the third speaker and is
/// pinned by a frozen signature fixture.
pub const AUTH_DOMAIN_TAG: &str = "chiefd-auth-v1";

/// The exact bytes a caller signs for a challenge: `tag || identityId ||
/// nonce`, concatenated with no separator.
///
/// The daemon issues a FIXED-WIDTH nonce, which is what makes the
/// `identityId`/`nonce` boundary unambiguous without a separator. ONE
/// definition, linked by the verifier and by every signer.
#[must_use]
pub fn challenge_message(identity_id: &str, nonce: &str) -> Vec<u8> {
    let mut message = Vec::with_capacity(AUTH_DOMAIN_TAG.len() + identity_id.len() + nonce.len());
    message.extend_from_slice(AUTH_DOMAIN_TAG.as_bytes());
    message.extend_from_slice(identity_id.as_bytes());
    message.extend_from_slice(nonce.as_bytes());
    message
}

/// `<data-root>/keys`, the directory holding every non-person credential.
const KEYS_DIRNAME: &str = "keys";

/// The operator's P-256 private key, PKCS#8 PEM.
///
/// `.key`, never `.env`: a `.env` suffix invites somebody to `source` it, and
/// this is a private key rather than a variable assignment.
const OPERATOR_KEY_FILENAME: &str = "operator.key";

/// The resident actuator's P-256 private key, PKCS#8 PEM.
const SERVICE_KEY_FILENAME: &str = "service.key";

/// The daemon's HS256 token-signing secret.
///
/// It lives BESIDE the keys, not in the launcher checkout where it used to.
/// Keyed to the checkout, two data roots sharing one install shared a signing
/// secret and one data root across two installs got two — neither of which is
/// what "a different data root is a different fleet" means.
const HS256_SECRET_FILENAME: &str = "auth-hs256.secret";

/// `<chief-dir>/keys`, where `<chief-dir>` is a company's own `<dir>/.chief`.
///
/// # TOMBSTONE: `keys_dir_from_orgs_root`
///
/// A second entry point sat here — `orgs_root.parent().map(keys_dir)` — and
/// its own doc explained why: "the two functions exist separately because the
/// two roots have the same name in different files". One caller held
/// `~/.chiefd` and the other held `~/.chiefd/orgs`, so one of them had to
/// climb a directory to reach the keys, and getting that wrong wrote an
/// operator's private key one level from where the reader looked.
///
/// There is now ONE root and nothing above it to climb to: a company's keys
/// are inside the company's own directory. So the parent walk is deleted
/// rather than renamed — an `Option` return whose `None` arm no real
/// deployment could produce was the shape of the confusion, not a safety net.
#[must_use]
pub fn keys_dir(chief_dir: &Path) -> PathBuf {
    chief_dir.join(KEYS_DIRNAME)
}

/// The operator key inside a [`keys_dir`].
#[must_use]
pub fn operator_key_path(keys_dir: &Path) -> PathBuf {
    keys_dir.join(OPERATOR_KEY_FILENAME)
}

/// The actuator's service key inside a [`keys_dir`].
#[must_use]
pub fn service_key_path(keys_dir: &Path) -> PathBuf {
    keys_dir.join(SERVICE_KEY_FILENAME)
}

/// The HS256 signing secret inside a [`keys_dir`].
#[must_use]
pub fn hs256_secret_path(keys_dir: &Path) -> PathBuf {
    keys_dir.join(HS256_SECRET_FILENAME)
}

/// Why a credential file could not be accepted.
#[derive(Debug)]
pub enum KeyError {
    /// The file could not be read at all.
    Unreadable {
        /// The path that failed.
        path: PathBuf,
        /// What the filesystem reported.
        source: io::Error,
    },
    /// The file is readable by its group or by the world.
    TooPermissive {
        /// The path that failed.
        path: PathBuf,
        /// The permission bits found, masked to `0o777`.
        mode: u32,
    },
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::TooPermissive { path, mode } => write!(
                formatter,
                "{} is mode {mode:04o}; a private key must be readable by its owner alone \
                 (chmod 600 {})",
                path.display(),
                path.display()
            ),
        }
    }
}

impl std::error::Error for KeyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable { source, .. } => Some(source),
            Self::TooPermissive { .. } => None,
        }
    }
}

/// Refuse a credential file that anyone but its owner can read.
///
/// This is a REFUSAL and not a warning. A key whose mode widened after it was
/// written must be treated as copied, and the only useful moment to say so is
/// before it is used to prove an identity.
///
/// # Errors
/// [`KeyError::Unreadable`] when the file has no metadata, and
/// [`KeyError::TooPermissive`] when any group or world bit is set.
pub fn assert_owner_only(path: &Path) -> Result<(), KeyError> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = std::fs::metadata(path)
        .map_err(|source| KeyError::Unreadable { path: path.to_path_buf(), source })?;
    // `mode()` is `u32` on every unix target through `std`, so this arithmetic
    // is identical on macOS and Linux. Reaching for `libc::mode_t` instead is
    // what makes a file like this compile on one of the two.
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        return Ok(());
    }
    Err(KeyError::TooPermissive { path: path.to_path_buf(), mode })
}

/// Read a private key PEM, refusing one that is not owner-only.
///
/// # Errors
/// Whatever [`assert_owner_only`] refuses, plus [`KeyError::Unreadable`] when
/// the contents cannot be read.
pub fn load_private_key_pem(path: &Path) -> Result<String, KeyError> {
    assert_owner_only(path)?;
    std::fs::read_to_string(path)
        .map_err(|source| KeyError::Unreadable { path: path.to_path_buf(), source })
}

#[cfg(test)]
mod tests {
    // Staging a fixture in a tempdir is the sanctioned use of the
    // seam-disallowed writer: production filesystem effects belong to
    // `chiefd_host`, and this crate has no production writer at all. Same
    // allow every sibling test that stages a fixture carries.
    #![allow(clippy::disallowed_methods)]
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn write_key(path: &Path, mode: u32) {
        std::fs::write(path, "-----BEGIN PRIVATE KEY-----\n").expect("stage key");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    /// The keys are INSIDE the company, one join from the folder chief owns.
    ///
    /// Two tests stood here — `the_data_root_and_the_orgs_root_resolve_to_one
    /// _keys_directory` and `a_parentless_orgs_root_has_no_keys_directory` —
    /// and both existed only to hold two entry points in step, because the
    /// data root and the orgs root had the same name one directory apart (the
    /// confusion that cost #13 a day). There is one root now and no parent
    /// walk, so there is nothing left for them to keep in step; what survives
    /// is the assertion that the derivation is a plain join, which is what
    /// makes the second entry point unnecessary.
    #[test]
    fn the_keys_live_one_join_inside_the_folder_chief_owns() {
        let chief_dir = Path::new("/work/anvils/.chief");
        assert_eq!(keys_dir(chief_dir), Path::new("/work/anvils/.chief/keys"));
        // Inside the company directory, never beside it: a company's trust
        // root moves, backs up and is destroyed with the company.
        assert!(keys_dir(chief_dir).starts_with("/work/anvils"));
    }

    /// Every credential is in ONE directory, so the fleet boundary is one
    /// thing to move, back up, or destroy.
    #[test]
    fn every_credential_shares_the_one_directory() {
        let keys = keys_dir(Path::new("/work/anvils/.chief"));
        for path in [operator_key_path(&keys), service_key_path(&keys), hs256_secret_path(&keys)] {
            assert_eq!(path.parent().expect("a parent"), keys);
        }
        // `.key`, never `.env`: nobody may be tempted to `source` a private key.
        assert!(operator_key_path(&keys).to_string_lossy().ends_with(".key"));
        assert!(service_key_path(&keys).to_string_lossy().ends_with(".key"));
    }

    /// THE WIRE FACT. `tag || identityId || nonce`, no separator, in that
    /// order. The daemon verifies these exact bytes and every signer produces
    /// them; a reordering here is a fleet-wide authentication outage that no
    /// type would catch.
    #[test]
    fn the_signed_message_is_the_tag_then_the_identity_then_the_nonce() {
        assert_eq!(AUTH_DOMAIN_TAG, "chiefd-auth-v1");
        assert_eq!(challenge_message("id-X", "nonce-Y"), b"chiefd-auth-v1id-Xnonce-Y".to_vec());
        // The nonce is fixed-width at the issuer, which is the ONLY reason a
        // separator is unnecessary. Written down as an example rather than
        // stated, so nobody "simplifies" the layout on the assumption that the
        // boundary is already unambiguous for any input.
        assert_eq!(
            challenge_message("ab", "cd"),
            challenge_message("a", "bcd"),
            "equal-length concatenations collide; only a fixed-width nonce separates them"
        );
    }

    #[test]
    fn an_owner_only_key_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("operator.key");
        write_key(&path, 0o600);
        assert!(load_private_key_pem(&path).expect("load").starts_with("-----BEGIN"));
        // 0400 is stricter, and stricter is never a refusal.
        write_key(&path, 0o400);
        assert!(assert_owner_only(&path).is_ok());
    }

    /// THE RULE THIS CRATE ADDS. Both writers already create 0600; neither
    /// reader checked, so a key that widened after it was written was loaded
    /// exactly as happily as one that had not.
    #[test]
    fn a_group_or_world_readable_key_is_refused_not_warned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("operator.key");
        for mode in [0o640, 0o604, 0o644, 0o660, 0o666, 0o777] {
            write_key(&path, mode);
            let refused = load_private_key_pem(&path).expect_err("a loose key must be refused");
            match refused {
                KeyError::TooPermissive { mode: found, .. } => assert_eq!(found, mode),
                other => panic!("expected a permission refusal, got {other:?}"),
            }
        }
    }

    /// The refusal names the file and the exact command that fixes it — the
    /// house rule that a refusal states the way through.
    #[test]
    fn the_refusal_names_the_path_and_the_way_through() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("operator.key");
        write_key(&path, 0o644);
        let message = load_private_key_pem(&path).expect_err("refused").to_string();
        assert!(message.contains("operator.key"), "{message}");
        assert!(message.contains("chmod 600"), "{message}");
    }

    #[test]
    fn a_missing_key_is_unreadable_rather_than_permissive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("absent.key");
        assert!(matches!(load_private_key_pem(&missing), Err(KeyError::Unreadable { .. })));
    }
}
