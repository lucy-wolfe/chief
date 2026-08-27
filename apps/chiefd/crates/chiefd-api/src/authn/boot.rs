//! Daemon-init wiring for agent-auth (P0, R1): use the owning company actor,
//! resolve the HS256 secret, and self-enrol the bootstrap operator from a disk
//! key — all BEFORE any request is served, never over HTTP.
//!
//! **The auth runtime is not optional (#751/P7).** It used to be: with
//! `CHIEFD_AUTH_ENABLED` unset a company came up with no runtime, no
//! `/v1/auth/*` endpoints and no way for an agent to prove anything, because
//! agents were authenticated by the terminal pane they descended from instead.
//! That authentication is deleted, so a daemon with no issuer is a daemon whose
//! agents have no identity at all. Every `chiefd run` now builds the runtime and
//! serves the endpoints, and an init failure REFUSES to serve.
//!
//! **And there is no stage (A6).** `CHIEFD_AUTH_ENABLED` used to select a
//! UNIVERSAL gate — whether the verify-middleware required a bearer on EVERY
//! non-exempt route — as agent-auth's own staged flip: enrol the fleet, then
//! enforce. Nothing in the tree ever set it, so the whole fleet ran in the
//! enrol stage, which is how a company came to serve every route to a caller
//! that presented nothing at all. The variable, the `AuthMode` enum it parsed
//! into, and the `enforce` boolean it fed through four constructors are
//! deleted. A bearer is required, always, and there is no value to set and no
//! parameter to pass that says otherwise.

use std::path::Path;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use p256::pkcs8::{DecodePrivateKey, EncodePublicKey};
use p256::SecretKey;

use chiefd_core::actor::CompanyDb;

use super::runtime::{AuthRuntime, Clock};
use super::{fingerprint_of_spki, random_secret, TOKEN_BYTES};

// TOMBSTONE: `CHIEFD_AUTH_ENABLED` (A6).
//
// It selected the UNIVERSAL gate: unset meant the "enrol" stage, in which
// every non-exempt route served a request that carried no bearer at all. It
// was set by NOTHING in this repository that starts a daemon — no launcher, no
// service unit, no shell script, no `.env` — so the enrol stage was not a
// stage, it was the fleet. Its enum (`AuthMode`), its parser (`mode()`), and
// the `enforce: bool` it fed through four constructors and one struct field
// are deleted with it. Authentication is not a deployment choice, so there is
// deliberately no replacement: no config file, no per-deployment policy, no
// `#[cfg]`, and no "development mode".

// TOMBSTONE: `CHIEFD_OPERATOR_KEY_PATH`.
//
// It named the operator's key file, and it was set by NOTHING in the tree — so
// every daemon booted with no operator identity at all, logged "operator key
// absent", and served anyway. A trust anchor that is unset by default is an
// off switch wearing a configuration flag's clothes, and it is the fourth one
// this workstream deletes.
//
// The path is DERIVED now: `<data-root>/keys/operator.key`, from
// `identity_keys`, which both this daemon and the operator client resolve
// without a handshake and without an inherited environment variable acting as
// a second control plane. There is deliberately no override — one operator per
// data root, and a different data root is a different fleet.

const NONCE_TTL_MS: i64 = 30_000;
const NONCE_MAX_PER_IDENTITY: usize = 8;

/// Build the daemon auth runtime from its owning company actor, resolve the
/// HS256 secret (read a provisioned secret, or an ephemeral one), and
/// bootstrap-enrol the two daemon-scoped principals from `<keys_dir>`: the
/// operator (`operator.key`, deliberate action) and the resident actuator
/// (`service.key`, automatic action). They are separate so an audit record can
/// say which of the two acted.
///
/// `keys_dir` is `<data-root>/keys` — [`identity_keys`] owns that derivation,
/// and the caller resolves it from whichever root it holds. Both credentials
/// live in that one directory, so the fleet's trust root is one thing.
///
/// This function READS. It never writes a key or a secret: filesystem effects
/// belong to `chiefd_host::executor`, and re-creating a writer here is exactly
/// the seam this crate must not cross. The caller ensures both files exist
/// before calling.
///
/// # Errors
/// A company-actor or secret-init failure — the caller REFUSES to serve rather
/// than run unauthenticated.
pub async fn build_auth_runtime(
    company: Arc<CompanyDb>,
    keys_dir: &Path,
    clock: Clock,
) -> Result<Arc<AuthRuntime>, String> {
    let operator_key_path = identity_keys::operator_key_path(keys_dir);
    let secret = load_secret(&identity_keys::hs256_secret_path(keys_dir))?;

    let runtime = Arc::new(AuthRuntime::new(
        company,
        Arc::new(secret.to_vec()),
        NONCE_TTL_MS,
        NONCE_MAX_PER_IDENTITY,
        clock,
    ));

    // A failure here REFUSES rather than warns, and that is the behaviour
    // change this packet carries. It used to log "operator key absent" and
    // serve on, which was survivable only while nothing needed an operator
    // identity — the state every daemon in the fleet was actually in, because
    // the env var that named this file was set by nothing. A company whose
    // operator cannot prove who it is has no control plane, and saying so at
    // boot is cheaper than every later call failing separately.
    let inserted = enroll_operator(&runtime, &operator_key_path).await.map_err(|error| {
        format!("operator identity at {}: {error}", operator_key_path.display())
    })?;
    tracing::info!(
        inserted,
        key = %operator_key_path.display(),
        "agent-auth: bootstrap operator identity enrolled from disk"
    );

    // The resident actuator's principal, on the same terms and refusing on the
    // same failures. A SEPARATE principal from the operator, and the deciding
    // reason is the AUDIT TRAIL rather than least privilege: the staffing
    // routes are losing `String::new()` as their actor, and a record that could
    // not tell an automatic actuation from a deliberate operator action would
    // waste that fix.
    //
    // What it is FOR is narrower than it looks. The actuator mutates NOTHING
    // over HTTP: `chief-cli/src/actuate/client.rs` makes exactly four calls and
    // every one is a read. So this credential authenticates READS, and without
    // it those four calls fail closed the moment the universal gate is on and
    // the company stops converging — which is why a company that cannot enrol
    // it is as unserveable as one that cannot enrol its operator.
    let service_key_path = identity_keys::service_key_path(keys_dir);
    let inserted = enroll_service(&runtime, &service_key_path)
        .await
        .map_err(|error| format!("service identity at {}: {error}", service_key_path.display()))?;
    tracing::info!(
        inserted,
        key = %service_key_path.display(),
        "agent-auth: actuator service identity enrolled from disk"
    );

    Ok(runtime)
}

/// Resolve the 32-byte HS256 secret. The daemon READS a provisioned secret file
/// (like the operator key) but never WRITES one — filesystem effects belong to
/// `chiefd_host::executor`, and re-creating a writer here is exactly the seam
/// this crate must not cross. The deployer provisions the secret file 0600
/// alongside the operator key; if it is absent, the daemon falls back to an
/// EPHEMERAL in-process secret (random, never persisted, never logged): tokens
/// stay valid for this daemon's lifetime and a restart rotates the secret, on
/// which clients simply re-acquire on the resulting 401. A provisioned file
/// keeps the secret stable across restarts.
fn load_secret(path: &Path) -> Result<[u8; TOKEN_BYTES], String> {
    if path.exists() {
        let bytes = std::fs::read(path).map_err(|error| format!("read secret: {error}"))?;
        <[u8; TOKEN_BYTES]>::try_from(bytes.as_slice())
            .map_err(|_| format!("hs256 secret at {} must be {TOKEN_BYTES} bytes", path.display()))
    } else {
        random_secret().map_err(|error| format!("entropy: {error}"))
    }
}

/// Parse the operator's PKCS#8 PEM private key, derive its SPKI public key +
/// fingerprint, and idempotently enrol it as the daemon-scoped operator identity.
async fn enroll_operator(runtime: &AuthRuntime, key_path: &Path) -> Result<bool, String> {
    let (spki_b64, fingerprint) = read_daemon_key(key_path, "operator")?;
    runtime
        .enroll_bootstrap_operator(identity_keys::OPERATOR_IDENTITY_ID, &spki_b64, &fingerprint)
        .await
        .map_err(|error| format!("enrol operator: {error}"))
}

/// Idempotently enrol the resident actuator's key as the daemon-scoped
/// `service` identity.
///
/// Daemon-scoped exactly like the operator — one key at the data root, enrolled
/// into every company on it, with no `company_slug`. A company-scoped service
/// identity would need a key per company and would still need one enrolment
/// each: more moving parts for the same end state.
///
/// A service principal acts as ITSELF and never names a requester, so there is
/// deliberately no service twin of `bind_requester_to_caller` to write. The rule
/// for a route a service drives is that a valid credential is present, and
/// specifically NOT to derive a person from it: a person-deriving helper answers
/// `None` for a `Service`, which a handler can easily mistake for
/// "unauthenticated" and refuse — authenticating the actuator perfectly and then
/// turning it away.
///
/// A fingerprint conflict is tolerated for the reason
/// [`AuthRuntime::enroll_bootstrap_operator`] tolerates the operator's: an
/// enrolled key that differs from the one on disk is a rotation, and boot must
/// not overwrite a trust anchor. It IS logged at warn, because unlike a
/// deliberate operator rotation the usual cause here is a deleted-and-re-minted
/// `service.key`, whose only other symptom is an actuator that quietly
/// authenticates nowhere.
async fn enroll_service(runtime: &AuthRuntime, key_path: &Path) -> Result<bool, String> {
    use chiefd_core::store::identities::IdentityKind;

    let (spki_b64, _fingerprint) = read_daemon_key(key_path, "service")?;
    match runtime
        .enroll_identity(
            identity_keys::SERVICE_IDENTITY_ID,
            identity_keys::SERVICE_IDENTITY_ID,
            IdentityKind::Service,
            None,
            &spki_b64,
            None,
        )
        .await
    {
        Ok(inserted) => Ok(inserted),
        Err(super::runtime::EnrollError::FingerprintConflict) => {
            tracing::warn!(
                key = %key_path.display(),
                "agent-auth: the enrolled service identity carries a different key than this \
                 file; leaving the enrolled key untouched. If the actuator cannot authenticate, \
                 the key on disk was re-minted and the enrolled half must be rotated deliberately"
            );
            Ok(false)
        }
        Err(error) => Err(format!("enrol service: {error:?}")),
    }
}

/// Read a daemon-scoped private key and derive its SPKI public half plus
/// fingerprint.
///
/// Mode-checked: a private key readable by anyone but its owner is a key to
/// assume is copied, and the only useful moment to say so is before it proves an
/// identity. Both writers already create 0600; until #1092 neither reader
/// looked.
fn read_daemon_key(key_path: &Path, label: &str) -> Result<(String, String), String> {
    let pem = identity_keys::load_private_key_pem(key_path).map_err(|error| error.to_string())?;
    let secret_key =
        SecretKey::from_pkcs8_pem(&pem).map_err(|error| format!("parse {label} key: {error}"))?;
    let spki_der = secret_key
        .public_key()
        .to_public_key_der()
        .map_err(|error| format!("encode {label} spki: {error}"))?;
    Ok((BASE64_STANDARD.encode(spki_der.as_bytes()), fingerprint_of_spki(spki_der.as_bytes())))
}

#[cfg(test)]
mod tests {
    // These tests stage fixture files (a provisioned secret, an operator key
    // PEM) in a tempdir to drive the real read/enrol paths. `std::fs::write` is
    // the seam-disallowed method in PRODUCTION (file effects belong to
    // chiefd_host); staging a tempdir fixture in a test is the sanctioned use,
    // same allow every sibling test that writes a fixture carries.
    #![allow(clippy::disallowed_methods)]
    use super::*;

    #[test]
    fn provisioned_secret_is_read_stably_and_absent_falls_back_to_ephemeral() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth-hs256.secret");
        // Absent: an ephemeral in-process secret, and the daemon writes NOTHING
        // (filesystem effects belong to chiefd_host — the seam).
        let ephemeral = load_secret(&path).expect("ephemeral");
        assert!(!path.exists(), "the daemon must never write the secret file");
        assert_eq!(ephemeral.len(), TOKEN_BYTES);
        // Provisioned: the exact bytes on disk are read back, stable across boots.
        let provisioned = [7u8; TOKEN_BYTES];
        std::fs::write(&path, provisioned).expect("provision the secret file");
        assert_eq!(load_secret(&path).expect("read"), provisioned);
        assert_eq!(load_secret(&path).expect("read again"), provisioned);
    }

    /// Stage a real key at `path` with `mode`, from a deterministic scalar so
    /// no RNG feature is needed.
    fn stage_key(path: &Path, scalar: u8, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;

        use p256::pkcs8::{EncodePrivateKey, LineEnding};
        let key = SecretKey::from_slice(&[scalar; 32]).expect("scalar");
        let pem = key.to_pkcs8_pem(LineEnding::LF).expect("pem");
        std::fs::write(path, pem.as_bytes()).expect("write key");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    /// Stage BOTH daemon keys at `<keys_dir>`, with `mode` on the operator's,
    /// and open a company beside them. The two scalars differ on purpose: the
    /// operator and the actuator are two principals, so a test that staged one
    /// key twice would pass while proving nothing about telling them apart.
    fn staged_keys_dir(dir: &Path, mode: u32) -> std::path::PathBuf {
        let keys = identity_keys::keys_dir(dir);
        std::fs::create_dir_all(&keys).expect("keys dir");
        stage_key(&identity_keys::operator_key_path(&keys), 7, mode);
        stage_key(&identity_keys::service_key_path(&keys), 9, 0o600);
        keys
    }

    fn company_at(dir: &Path) -> Arc<CompanyDb> {
        use chiefd_core::store::COMPANY_DB_FILENAME;
        use chiefd_core::test_support::ManualClock;
        Arc::new(
            CompanyDb::open(
                "acme",
                &dir.join(COMPANY_DB_FILENAME),
                Arc::new(ManualClock::starting_at(0, 1_000)),
            )
            .expect("open company"),
        )
    }

    #[tokio::test]
    async fn build_runtime_uses_the_company_actor_and_enrols_operator_from_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keys = staged_keys_dir(dir.path(), 0o600);
        let runtime = build_auth_runtime(company_at(dir.path()), &keys, Arc::new(|| 1_000))
            .await
            .expect("build");
        // The operator identity now resolves for the middleware — the enrolled
        // anchor is present after boot.
        use super::super::middleware::IdentityLookup;
        assert!(
            runtime.get(identity_keys::OPERATOR_IDENTITY_ID).await.expect("readable").is_some(),
            "operator enrolled at boot"
        );
    }

    /// A3. TWO daemon-scoped principals come up at boot, not one, and they are
    /// distinct keys. The actuator's actions are automatic and the operator's
    /// are deliberate, and an audit record that cannot tell them apart is worth
    /// much less than one that can — which is the whole reason this identity
    /// exists rather than the actuator borrowing the operator's.
    #[tokio::test]
    async fn the_actuator_is_enrolled_as_its_own_service_principal() {
        use chiefd_core::store::identities::IdentityKind;

        use super::super::middleware::IdentityLookup;
        let dir = tempfile::tempdir().expect("tempdir");
        let keys = staged_keys_dir(dir.path(), 0o600);
        let runtime = build_auth_runtime(company_at(dir.path()), &keys, Arc::new(|| 1_000))
            .await
            .expect("build");

        let service_id = identity_keys::SERVICE_IDENTITY_ID;
        let service =
            runtime.get(service_id).await.expect("readable").expect("service enrolled at boot");
        assert_eq!(service.kind, IdentityKind::Service, "never an Operator, never a Person");
        assert!(service.active);
        assert_eq!(
            service.company_slug, None,
            "daemon-scoped: one key at the data root, enrolled into every company on it"
        );

        let operator = runtime
            .get(identity_keys::OPERATOR_IDENTITY_ID)
            .await
            .expect("readable")
            .expect("operator enrolled");
        assert_ne!(
            operator.fingerprint, service.fingerprint,
            "two principals means two keys; sharing one would erase the distinction this \
             identity exists to make"
        );
    }

    /// Enrolment is idempotent: a daemon restart re-reads the same file and
    /// must not fail, re-key, or double-insert.
    #[tokio::test]
    async fn a_restart_re_enrols_both_principals_without_inserting_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keys = staged_keys_dir(dir.path(), 0o600);
        let company = company_at(dir.path());
        let first = build_auth_runtime(Arc::clone(&company), &keys, Arc::new(|| 1_000))
            .await
            .expect("first boot");
        drop(first);
        // The same company actor, the same two keys on disk: the second boot is
        // a no-op rather than a refusal.
        build_auth_runtime(company, &keys, Arc::new(|| 2_000)).await.expect("second boot");
    }

    /// The service key is refused on exactly the terms the operator key is: an
    /// absent one refuses to serve, because a company whose actuator cannot
    /// authenticate stops converging and says nothing about why.
    #[tokio::test]
    async fn an_absent_service_key_refuses_to_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keys = identity_keys::keys_dir(dir.path());
        std::fs::create_dir_all(&keys).expect("keys dir");
        stage_key(&identity_keys::operator_key_path(&keys), 7, 0o600);
        let Err(error) =
            build_auth_runtime(company_at(dir.path()), &keys, Arc::new(|| 1_000)).await
        else {
            panic!("a company with no service key must refuse to serve")
        };
        assert!(error.contains("service.key"), "{error}");
    }

    /// And on the mode rule too. A private key anyone can read is a key to
    /// assume is copied, whichever principal it belongs to.
    #[tokio::test]
    async fn a_world_readable_service_key_refuses_to_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keys = identity_keys::keys_dir(dir.path());
        std::fs::create_dir_all(&keys).expect("keys dir");
        stage_key(&identity_keys::operator_key_path(&keys), 7, 0o600);
        stage_key(&identity_keys::service_key_path(&keys), 9, 0o644);
        let Err(error) =
            build_auth_runtime(company_at(dir.path()), &keys, Arc::new(|| 1_000)).await
        else {
            panic!("a world-readable service key must refuse to serve")
        };
        assert!(error.contains("service.key"), "{error}");
        assert!(error.contains("chmod 600"), "{error}");
    }

    /// THE BEHAVIOUR CHANGE. An absent operator key used to log a warning and
    /// serve on, which is how the whole fleet ran with no operator identity at
    /// all: the env var that named the file was set by nothing. A company
    /// whose operator cannot prove who it is has no control plane.
    #[tokio::test]
    async fn an_absent_operator_key_refuses_to_build_rather_than_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keys = identity_keys::keys_dir(dir.path());
        std::fs::create_dir_all(&keys).expect("keys dir");
        let Err(error) =
            build_auth_runtime(company_at(dir.path()), &keys, Arc::new(|| 1_000)).await
        else {
            panic!("a company with no operator key must refuse to serve")
        };
        assert!(error.contains("operator.key"), "{error}");
    }

    /// A key anyone can read is a key to assume is copied. Both writers create
    /// 0600 already; this is the reader half that was missing, and it refuses
    /// at boot rather than at the first call that needs the identity.
    #[tokio::test]
    async fn a_group_readable_operator_key_refuses_to_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keys = staged_keys_dir(dir.path(), 0o640);
        let Err(error) =
            build_auth_runtime(company_at(dir.path()), &keys, Arc::new(|| 1_000)).await
        else {
            panic!("a group-readable operator key must refuse to serve")
        };
        assert!(error.contains("chmod 600"), "{error}");
    }
}
