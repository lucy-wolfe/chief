//! `AuthRuntime` — the daemon-side auth state and operations (agent-auth P0).
//!
//! Holds the HS256 secret, bounded nonce store, and the owning company's
//! [`CompanyDb`](chiefd_core::actor::CompanyDb). It is the single object the
//! `/v1/auth/*` handlers and verify middleware share:
//!
//! * [`AuthRuntime::challenge`] issues an identity-bound nonce.
//! * [`AuthRuntime::redeem`] verifies the signed challenge and mints a token.
//! * [`AuthRuntime::mint_channel`] is the SERVER-SIDE channel mint (pi-pane,
//!   operator-remote) — the same issuance path, gated by the caller's
//!   attestation, never reachable across a process boundary.
//! * [`AuthRuntime::enroll_bootstrap_operator`] self-enrols the boot operator.
//! * It implements [`IdentityLookup`] for the middleware's per-request read.
//!
//! Every token — keypair or channel — flows through [`super::issue_token_for`]
//! and revokes on the SAME anchor (the identity's `active`/`fingerprint`).

use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use futures_util::future::BoxFuture;
use p256::ecdsa::VerifyingKey;
use p256::pkcs8::DecodePublicKey;

use chiefd_core::actor::CompanyDb;
use chiefd_core::error::ChiefdError;
use chiefd_core::store::identities::{Identity, IdentityKind, NewIdentity};

use super::middleware::IdentityLookup;
use super::nonce::{Issued, NonceStore};
use super::{fingerprint_of_spki, issue_token_for, random_token, sig, IssueError};

/// A monotonic-ish wall clock returning ms since epoch. Injected so the runtime
/// never reaches for a forbidden `Date::now`-style call directly and tests are
/// deterministic.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Why a challenge could not be issued.
///
/// Not `Copy`: [`Self::Unavailable`] carries the fault's own words, and the
/// caller renders them. `PartialEq` stays, so the existing `assert_eq!` tests
/// read exactly as they did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChallengeError {
    /// The identity store was read and holds nobody by that id.
    UnknownIdentity,
    /// The identity exists but is revoked.
    Inactive,
    /// The identity store COULD NOT BE READ. Not a verdict about the caller:
    /// the trust decision was never made, and asking again in a moment is the
    /// right thing to do. Kept apart from [`Self::UnknownIdentity`] because a
    /// mint that answers "you are not enrolled" during a seven-second store
    /// stall costs a company its actuator (#1204).
    Unavailable {
        /// The `ChiefdError` this read produced, rendered.
        reason: String,
    },
    /// Out of OS entropy.
    Entropy,
}

/// Why a token could not be minted from a redeemed challenge.
///
/// Not `Copy`, for the same reason [`ChallengeError`] is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemError {
    /// The nonce was unknown, already used, or expired.
    BadNonce,
    /// The identity is unknown or revoked.
    Forbidden,
    /// The identity store could not be read — see
    /// [`ChallengeError::Unavailable`].
    Unavailable {
        /// The `ChiefdError` this read produced, rendered.
        reason: String,
    },
    /// The identity is a channel (no pubkey): channels never sign a challenge.
    NotAKeypair,
    /// The signature did not verify against the enrolled key.
    BadSignature,
    /// The token could not be issued (inactive / encode).
    Issue(IssueError),
}

/// Why an enrolment was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollError {
    /// The public key was not valid base64 / not a valid P-256 SPKI key.
    BadPubkey,
    /// The `identity_id` already exists with a DIFFERENT key fingerprint.
    /// Enrolment is idempotent on (id, fingerprint); presenting a new key for an
    /// existing id is an EXPLICIT rotation act, never a silent re-key here (that
    /// would be revocation's blind spot — Fable). The caller must rotate
    /// deliberately.
    FingerprintConflict,
    /// A person was enrolled without a company slug, a channel/daemon-scoped
    /// kind was given one, or the company actor could not make the change.
    Db(String),
}

/// Why a channel token could not be minted.
///
/// Not `Copy`, for the same reason [`ChallengeError`] is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelMintError {
    /// No such channel identity, or it is not `kind='channel'`.
    UnknownChannel,
    /// The identity store could not be read — see
    /// [`ChallengeError::Unavailable`].
    Unavailable {
        /// The `ChiefdError` this read produced, rendered.
        reason: String,
    },
    /// The channel is revoked.
    Inactive,
    /// The token could not be issued.
    Issue(IssueError),
}

/// The shared daemon auth state.
pub struct AuthRuntime {
    secret: Arc<Vec<u8>>,
    company: Arc<CompanyDb>,
    nonces: Mutex<NonceStore>,
    clock: Clock,
}

impl AuthRuntime {
    /// Build from the owning company actor, the HS256 secret, nonce policy, and
    /// a clock.
    #[must_use]
    pub fn new(
        company: Arc<CompanyDb>,
        secret: Arc<Vec<u8>>,
        nonce_ttl_ms: i64,
        nonce_max_per_identity: usize,
        clock: Clock,
    ) -> Self {
        Self {
            secret,
            company,
            nonces: Mutex::new(NonceStore::new(nonce_ttl_ms, nonce_max_per_identity)),
            clock,
        }
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    /// Read an identity by id through the owning actor.
    ///
    /// `Ok(None)` is NEVER ENROLLED; `Err` is COULD NOT LOOK — a SQLite fault
    /// on the pooled read connection, or a reader pool that is closed,
    /// exhausted or cancelled. Every caller here still fails closed on both,
    /// and every caller reports them differently: the doc on this function
    /// used to say the two were "deliberately indistinguishable", and the
    /// `.ok()` that made them so is what turned a seven-second store stall
    /// into `403 unknown identity` for every caller of every route (#1204).
    async fn read_identity(&self, identity_id: &str) -> Result<Option<Identity>, ChiefdError> {
        self.company.identity_read(identity_id.to_owned()).await
    }

    /// Issue an identity-bound challenge nonce. Only enrolled, active identities
    /// get one — an unknown id is refused rather than seeded into the nonce
    /// store, so a stranger cannot pump the map.
    ///
    /// # Errors
    /// [`ChallengeError`] for an unknown/inactive identity, an unreadable
    /// identity store, or missing entropy.
    pub async fn challenge(&self, identity_id: &str) -> Result<Issued, ChallengeError> {
        let identity = self
            .read_identity(identity_id)
            .await
            .map_err(|fault| ChallengeError::Unavailable { reason: fault.to_string() })?
            .ok_or(ChallengeError::UnknownIdentity)?;
        if !identity.active {
            return Err(ChallengeError::Inactive);
        }
        let nonce = random_token().map_err(|_| ChallengeError::Entropy)?;
        let nonce_id = random_token().map_err(|_| ChallengeError::Entropy)?;
        let now = self.now();
        if let Ok(mut store) = self.nonces.lock() {
            store.issue(identity_id, &nonce_id, &nonce, now);
        }
        Ok(Issued { nonce_id, nonce })
    }

    /// Redeem a signed challenge for a token. Consumes the nonce (single-use),
    /// verifies the domain-separated P-256 signature against the identity's
    /// enrolled key, then mints via the ONE issuance path.
    ///
    /// # Errors
    /// [`RedeemError`] for a bad/expired nonce, an unknown/revoked identity, an
    /// unreadable identity store, a non-keypair identity, a bad signature, or
    /// an issuance failure.
    pub async fn redeem(&self, nonce_id: &str, signature_b64: &str) -> Result<String, RedeemError> {
        let now = self.now();
        let (identity_id, nonce) = {
            let mut store = self.nonces.lock().map_err(|_| RedeemError::BadNonce)?;
            store.consume(nonce_id, now).ok_or(RedeemError::BadNonce)?
        };
        let identity = self
            .read_identity(&identity_id)
            .await
            .map_err(|fault| RedeemError::Unavailable { reason: fault.to_string() })?
            .ok_or(RedeemError::Forbidden)?;
        if !identity.active {
            return Err(RedeemError::Forbidden);
        }
        let pubkey_b64 = identity.pubkey.as_deref().ok_or(RedeemError::NotAKeypair)?;
        let spki_der = BASE64_STANDARD.decode(pubkey_b64).map_err(|_| RedeemError::BadSignature)?;
        let signature =
            BASE64_STANDARD.decode(signature_b64).map_err(|_| RedeemError::BadSignature)?;
        if !sig::verify_challenge(&spki_der, &identity_id, &nonce, &signature) {
            return Err(RedeemError::BadSignature);
        }
        issue_token_for(&self.secret, &identity, now).map_err(RedeemError::Issue)
    }

    /// SERVER-SIDE channel mint: issue a token for an attested channel identity
    /// (`operator-pane`, `operator-remote`). The CALLER must have performed the
    /// channel's attestation (allowlist / pane check) BEFORE calling —
    /// this only enforces enrolment + active. Boundary rule: this is an
    /// in-process API, never an HTTP route, so it can only be reached by a
    /// channel whose transport the daemon itself terminates.
    ///
    /// # Errors
    /// [`ChannelMintError`] for an unknown/non-channel/revoked identity, an
    /// unreadable identity store, or an issuance failure.
    pub async fn mint_channel(
        &self,
        channel_identity_id: &str,
    ) -> Result<String, ChannelMintError> {
        let identity = self
            .read_identity(channel_identity_id)
            .await
            .map_err(|fault| ChannelMintError::Unavailable { reason: fault.to_string() })?
            .ok_or(ChannelMintError::UnknownChannel)?;
        if identity.kind != IdentityKind::Channel {
            return Err(ChannelMintError::UnknownChannel);
        }
        if !identity.active {
            return Err(ChannelMintError::Inactive);
        }
        issue_token_for(&self.secret, &identity, self.now()).map_err(ChannelMintError::Issue)
    }

    /// Enrol a keypair identity (person / service / operator) from its SPKI
    /// public key. Idempotent by `identity_id` — re-materialising a person is a
    /// no-op, never a silent re-key. The fingerprint is DERIVED from the key
    /// (never caller-supplied), and `active` is set explicitly to 1 (fail-closed,
    /// no DEFAULT). Returns whether a row was inserted.
    ///
    /// # Errors
    /// [`EnrollError::BadPubkey`] if the key is not a valid P-256 SPKI value;
    /// [`EnrollError::Db`] on a coherence-CHECK or other company-store failure.
    pub async fn enroll_identity(
        &self,
        identity_id: &str,
        principal: &str,
        kind: IdentityKind,
        company_slug: Option<&str>,
        pubkey_spki_b64: &str,
        enrolled_by: Option<&str>,
    ) -> Result<bool, EnrollError> {
        let spki = BASE64_STANDARD.decode(pubkey_spki_b64).map_err(|_| EnrollError::BadPubkey)?;
        // Reject anything that is not a real P-256 public key before it reaches
        // the trust table — the fingerprint (and thus every future kid match)
        // depends on a well-formed key.
        VerifyingKey::from_public_key_der(&spki).map_err(|_| EnrollError::BadPubkey)?;
        let fingerprint = fingerprint_of_spki(&spki);
        // The actor owns the check-and-insert in one `BEGIN IMMEDIATE`; no
        // runtime-side connection or mutex can reintroduce a TOCTOU. Same id +
        // same key is a no-op; a new key is an explicit conflict, never a
        // silent re-key.
        self.company
            .identity_enroll(NewIdentity {
                identity_id,
                principal,
                kind,
                company_slug,
                pubkey: Some(pubkey_spki_b64),
                fingerprint: &fingerprint,
                enrolled_by,
            })
            .await
            .map_err(|error| match error {
                ChiefdError::Refused(refusal)
                    if refusal.code == "auth-identity-fingerprint-conflict" =>
                {
                    EnrollError::FingerprintConflict
                }
                other => EnrollError::Db(other.to_string()),
            })
    }

    /// Idempotently self-enrol the bootstrap operator identity at daemon init.
    /// Runs through the owning company's actor with the operator's PUBLIC key
    /// (SPKI-DER-base64) read from disk — never over HTTP. A no-op once
    /// enrolled, and it never overwrites a rotated key. Returns whether a row
    /// was inserted.
    ///
    /// # Errors
    /// Propagates a company-store write failure.
    pub async fn enroll_bootstrap_operator(
        &self,
        identity_id: &str,
        pubkey_spki_b64: &str,
        fingerprint: &str,
    ) -> Result<bool, ChiefdError> {
        match self
            .company
            .identity_enroll(NewIdentity {
                identity_id,
                principal: "operator",
                kind: IdentityKind::Operator,
                company_slug: None,
                pubkey: Some(pubkey_spki_b64),
                fingerprint,
                enrolled_by: None,
            })
            .await
        {
            // A rotated operator key is already an enrolled trust anchor. Boot
            // must leave it untouched and continue rather than treating that
            // deliberate rotation as a failed bootstrap.
            Err(ChiefdError::Refused(refusal))
                if refusal.code == "auth-identity-fingerprint-conflict" =>
            {
                Ok(false)
            }
            other => other,
        }
    }

    /// The HS256 secret, for constructing the middleware [`AuthState`].
    #[must_use]
    pub fn secret(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.secret)
    }
}

impl IdentityLookup for AuthRuntime {
    fn get<'a>(
        &'a self,
        identity_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Identity>, ChiefdError>> {
        Box::pin(async move { self.read_identity(identity_id).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chiefd_core::store::COMPANY_DB_FILENAME;
    use chiefd_core::test_support::ManualClock;

    // Reuse the agent-side crypto shape via p256 directly to sign challenges.
    use p256::ecdsa::signature::Signer;
    use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
    use p256::pkcs8::EncodePublicKey;

    struct RuntimeFixture {
        runtime: AuthRuntime,
        // Keep the company file alive until the actor inside `runtime` has
        // shut down and checkpointed its connection. Also the directory the
        // store-fault tests reach into, to break the database under the actor.
        _dir: tempfile::TempDir,
    }

    fn runtime_with(now: i64) -> RuntimeFixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(COMPANY_DB_FILENAME);
        let company = Arc::new(
            CompanyDb::open("acme", &path, Arc::new(ManualClock::starting_at(0, now)))
                .expect("open company actor"),
        );
        let runtime =
            AuthRuntime::new(company, Arc::new(b"secret".to_vec()), 1000, 8, Arc::new(move || now));
        RuntimeFixture { runtime, _dir: dir }
    }

    async fn enroll_person(rt: &AuthRuntime, id: &str, key: &SigningKey) -> String {
        let spki = VerifyingKey::from(key).to_public_key_der().expect("spki").as_bytes().to_vec();
        let spki_b64 = BASE64_STANDARD.encode(&spki);
        let fingerprint = format!("fp-{id}");
        rt.company
            .identity_enroll(NewIdentity {
                identity_id: id,
                principal: id,
                kind: IdentityKind::Person,
                company_slug: Some("acme"),
                pubkey: Some(&spki_b64),
                fingerprint: &fingerprint,
                enrolled_by: None,
            })
            .await
            .expect("enrol through actor");
        spki_b64
    }

    fn sign(key: &SigningKey, identity_id: &str, nonce: &str) -> String {
        let sig: Signature = key.sign(&sig::challenge_message(identity_id, nonce));
        BASE64_STANDARD.encode(sig.to_bytes())
    }

    #[tokio::test]
    async fn full_challenge_redeem_mints_a_verifiable_token() {
        let fixture = runtime_with(100);
        let rt = &fixture.runtime;
        let key = SigningKey::from_bytes(&[7u8; 32].into()).expect("key");
        enroll_person(rt, "person:a", &key).await;
        let issued = rt.challenge("person:a").await.expect("challenge");
        let signature = sign(&key, "person:a", &issued.nonce);
        let token = rt.redeem(&issued.nonce_id, &signature).await.expect("redeem");
        let claims = super::super::jwt::verify(b"secret", &token).expect("verify");
        assert_eq!(claims.sub, "person:a");
        assert_eq!(claims.kid, "fp-person:a");
    }

    #[tokio::test]
    async fn challenge_for_unknown_identity_is_refused() {
        let fixture = runtime_with(1);
        assert_eq!(fixture.runtime.challenge("ghost").await, Err(ChallengeError::UnknownIdentity));
    }

    #[tokio::test]
    async fn a_replayed_nonce_is_rejected() {
        let fixture = runtime_with(1);
        let rt = &fixture.runtime;
        let key = SigningKey::from_bytes(&[7u8; 32].into()).expect("key");
        enroll_person(rt, "person:a", &key).await;
        let issued = rt.challenge("person:a").await.expect("challenge");
        let signature = sign(&key, "person:a", &issued.nonce);
        assert!(rt.redeem(&issued.nonce_id, &signature).await.is_ok());
        assert_eq!(rt.redeem(&issued.nonce_id, &signature).await, Err(RedeemError::BadNonce));
    }

    #[tokio::test]
    async fn a_wrong_key_signature_is_rejected() {
        let fixture = runtime_with(1);
        let rt = &fixture.runtime;
        let real = SigningKey::from_bytes(&[7u8; 32].into()).expect("key");
        let wrong = SigningKey::from_bytes(&[9u8; 32].into()).expect("key");
        enroll_person(rt, "person:a", &real).await;
        let issued = rt.challenge("person:a").await.expect("challenge");
        let signature = sign(&wrong, "person:a", &issued.nonce);
        assert_eq!(rt.redeem(&issued.nonce_id, &signature).await, Err(RedeemError::BadSignature));
    }

    #[tokio::test]
    async fn revoked_identity_cannot_challenge_or_redeem() {
        let fixture = runtime_with(1);
        let rt = &fixture.runtime;
        let key = SigningKey::from_bytes(&[7u8; 32].into()).expect("key");
        enroll_person(rt, "person:a", &key).await;
        let issued = rt.challenge("person:a").await.expect("challenge before revoke");
        let signature = sign(&key, "person:a", &issued.nonce);
        rt.company.identity_revoke("person:a".to_owned(), 5).await.expect("revoke through actor");
        // A fresh challenge is refused, and the pre-revoke nonce won't redeem.
        assert_eq!(rt.challenge("person:a").await, Err(ChallengeError::Inactive));
        assert_eq!(rt.redeem(&issued.nonce_id, &signature).await, Err(RedeemError::Forbidden));
    }

    #[tokio::test]
    async fn bootstrap_operator_self_enrol_is_idempotent_then_resolves() {
        let fixture = runtime_with(1);
        let rt = &fixture.runtime;
        assert!(rt
            .enroll_bootstrap_operator("operator", "c3BraQ==", "fp-op")
            .await
            .expect("enrol"));
        assert!(!rt
            .enroll_bootstrap_operator("operator", "c3BraQ==", "fp-op")
            .await
            .expect("again"));
        assert!(!rt
            .enroll_bootstrap_operator("operator", "different-key", "fp-rotated")
            .await
            .expect("rotated operator remains enrolled"));
        // The operator identity now resolves for the middleware.
        assert_eq!(
            rt.get("operator").await.expect("the store is readable").expect("operator").fingerprint,
            "fp-op"
        );
    }

    #[tokio::test]
    async fn enroll_identity_is_idempotent_on_key_and_conflicts_on_rekey() {
        let fixture = runtime_with(1);
        let rt = &fixture.runtime;
        let key = SigningKey::from_bytes(&[7u8; 32].into()).expect("key");
        let spki = spki_b64(&key);
        // First enrol inserts; the same key again is an idempotent no-op success.
        assert_eq!(
            rt.enroll_identity(
                "person:a",
                "person:a",
                IdentityKind::Person,
                Some("acme"),
                &spki,
                None
            )
            .await,
            Ok(true),
        );
        assert_eq!(
            rt.enroll_identity(
                "person:a",
                "person:a",
                IdentityKind::Person,
                Some("acme"),
                &spki,
                None
            )
            .await,
            Ok(false),
        );
        // A DIFFERENT key for the same id is an explicit conflict, never a re-key.
        let key2 = SigningKey::from_bytes(&[9u8; 32].into()).expect("key2");
        assert_eq!(
            rt.enroll_identity(
                "person:a",
                "person:a",
                IdentityKind::Person,
                Some("acme"),
                &spki_b64(&key2),
                None
            )
            .await,
            Err(EnrollError::FingerprintConflict),
        );
        // The original key still stands (no silent overwrite).
        let expected = fingerprint_of_spki(&BASE64_STANDARD.decode(&spki).expect("decode"));
        assert_eq!(
            rt.get("person:a").await.expect("readable").expect("present").fingerprint,
            expected
        );
        // A malformed pubkey is rejected outright.
        assert_eq!(
            rt.enroll_identity(
                "person:b",
                "person:b",
                IdentityKind::Person,
                Some("acme"),
                "!!not-b64",
                None
            )
            .await,
            Err(EnrollError::BadPubkey),
        );
    }

    fn spki_b64(key: &SigningKey) -> String {
        BASE64_STANDARD
            .encode(VerifyingKey::from(key).to_public_key_der().expect("spki").as_bytes())
    }

    #[tokio::test]
    async fn channel_mint_uses_the_same_issuance_and_respects_active() {
        let fixture = runtime_with(50);
        let rt = &fixture.runtime;
        assert!(rt
            .company
            .identity_enroll(NewIdentity {
                identity_id: "operator-remote",
                principal: "operator",
                kind: IdentityKind::Channel,
                company_slug: None,
                pubkey: None,
                fingerprint: "epoch-1",
                enrolled_by: None,
            })
            .await
            .expect("channel enrol"));
        let token = rt.mint_channel("operator-remote").await.expect("mint");
        let claims = super::super::jwt::verify(b"secret", &token).expect("verify");
        assert_eq!(claims.sub, "operator-remote");
        assert_eq!(claims.kid, "epoch-1");
        // A person id is never mintable as a channel.
        assert_eq!(rt.mint_channel("person:nope").await, Err(ChannelMintError::UnknownChannel));
    }

    /// Break the identity store UNDER a live actor, the cheapest way that is
    /// still the real failure path.
    ///
    /// A second `rusqlite` connection to the same WAL database drops the
    /// `identities` table. The pooled reader the auth runtime uses then fails
    /// inside `store::identities::get`, which maps every rusqlite error through
    /// `store_failure(AUTH_IDENTITIES_STORE, ..)` — so what reaches
    /// `read_identity` is the exact `ChiefdError::StoreFailure` a `SQLITE_BUSY`
    /// on a stalled store produces, arriving by the exact same code path.
    ///
    /// The alternative was shutting the actor down, and it does not work:
    /// `CompanyDb::shutdown` closes the WRITER thread and leaves the reader
    /// pool serving, so `identity_read` keeps answering happily.
    /// The `disallowed_methods` allow is the point of the fixture, not a way
    /// around it: the rule says only `chiefd_core::store` may open a company
    /// connection, and this test opens one PRECISELY to violate the invariant
    /// the rule protects, so that the pooled reader beside it faults for real.
    #[allow(clippy::disallowed_methods)]
    fn break_the_identity_store(dir: &std::path::Path) {
        let conn = rusqlite::Connection::open(dir.join(COMPANY_DB_FILENAME))
            .expect("open a second connection to the company database");
        conn.execute_batch("DROP TABLE identities;").expect("drop the identities table");
    }

    /// #1204 — A MINT DURING A STORE FAULT IS NOT A VERDICT ABOUT THE CALLER.
    ///
    /// `challenge` answered `UnknownIdentity` for a store it could not read,
    /// which the route renders as `401 unknown or inactive identity`: a
    /// settled-looking answer that says the identity is not enrolled. It is
    /// the same lie the middleware told with its 403, on the path that MINTS
    /// the token — so a client whose cached token has just been invalidated by
    /// a daemon restart, which is exactly when the store is most likely to be
    /// slow, is told to give up.
    #[tokio::test]
    async fn a_store_fault_on_challenge_is_unavailable_not_unknown() {
        let fixture = runtime_with(1);
        let rt = &fixture.runtime;
        let key = SigningKey::from_bytes(&[7u8; 32].into()).expect("key");
        enroll_person(rt, "person:a", &key).await;
        // Healthy first, so the fault below is the only difference.
        assert!(rt.challenge("person:a").await.is_ok());

        break_the_identity_store(fixture._dir.path());

        match rt.challenge("person:a").await {
            Err(ChallengeError::Unavailable { reason }) => {
                assert!(reason.contains("auth-identities"), "the fault names the store: {reason}");
            }
            other => panic!("a store that cannot be read is not a verdict: {other:?}"),
        }
    }

    /// The redeem half of the same rule. `Forbidden` renders as `403 identity
    /// not authorized`, which tells a caller that satisfied the challenge
    /// correctly that its identity is refused.
    #[tokio::test]
    async fn a_store_fault_on_redeem_is_unavailable_not_forbidden() {
        let fixture = runtime_with(1);
        let rt = &fixture.runtime;
        let key = SigningKey::from_bytes(&[7u8; 32].into()).expect("key");
        enroll_person(rt, "person:a", &key).await;
        let issued = rt.challenge("person:a").await.expect("challenge while healthy");
        let signature = sign(&key, "person:a", &issued.nonce);

        break_the_identity_store(fixture._dir.path());

        match rt.redeem(&issued.nonce_id, &signature).await {
            Err(RedeemError::Unavailable { reason }) => {
                assert!(reason.contains("auth-identities"), "the fault names the store: {reason}");
            }
            other => panic!("a store that cannot be read is not a verdict: {other:?}"),
        }
    }

    /// The middleware seam carries the same split, and it is the one every
    /// authenticated request goes through. `Ok(None)` is never-enrolled;
    /// `Err` is could-not-look; the `.ok()` that used to flatten the second
    /// into the first is gone.
    #[tokio::test]
    async fn the_middleware_lookup_tells_never_enrolled_apart_from_could_not_look() {
        let fixture = runtime_with(1);
        let rt = &fixture.runtime;
        let key = SigningKey::from_bytes(&[7u8; 32].into()).expect("key");
        enroll_person(rt, "person:a", &key).await;
        assert!(rt.get("person:a").await.expect("readable").is_some());
        assert!(rt.get("ghost").await.expect("readable").is_none(), "read, and holds nobody");

        break_the_identity_store(fixture._dir.path());

        assert!(rt.get("person:a").await.is_err(), "not read at all, and it says so");
    }
}
