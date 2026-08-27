//! The operator client's half of agent-auth: read a derived key, sign a
//! challenge, and hold the bearer chiefd minted for it.
//!
//! # Why this is in the LIBRARY half
//!
//! `http.rs` — the binary half's transport — is the obvious home, and it is the
//! wrong one. The resident actuator (`actuate::client`) needs the same
//! acquisition for its own `service` identity and its changefeed, and it cannot
//! name a module `main.rs` declares. One acquirer in `lib.rs` is what stops the
//! second copy: the crypto, the wire shape, the cache and the failure policy
//! are stated once and both halves link them.
//!
//! # What it does NOT hold
//!
//! The key MINT. `chiefd_host::materialize::ensure_daemon_identity_key` creates
//! `<dir>/.chief/keys/*.key` 0600 at daemon boot and preserves it thereafter,
//! and this client only ever READS. That is also why the key is loaded LAZILY,
//! on the first request that needs a header rather than when a client is built:
//! `chief` constructs its client before the company's daemon exists, and
//! the daemon is what mints the file.
//!
//! Where the key lives, the permission rule, and the exact bytes a signature
//! covers all come from [`identity_keys`], which the daemon links too. Neither
//! half may depend on the other, so anything the two must SAY THE SAME WAY
//! lives there rather than as a matching literal in each.
//!
//! # The failure policy, stated once
//!
//! Acquisition failure is REPORTED to the caller and never decided here: this
//! module names the failure precisely and each caller classifies it.
//!
//! What a caller must NOT do is send the request without a header and let
//! chiefd answer. That was this module's advice, and A6 retired it: every
//! non-exempt route now answers a bare call `401 missing bearer token`, so the
//! bare call is not a degrade but a guaranteed refusal wearing a status that
//! names the wrong thing — the read, rather than the mint that failed. Ask
//! [`BearerError::is_transient`] instead: a 5xx from an auth route is a daemon
//! that cannot decide yet, and a 401 from one is an identity it does not
//! accept.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey as _;
use p256::SecretKey;

// The wire facts the daemon also links. Re-exported so a reader of this module
// does not have to know which crate holds them, and so there is exactly one
// definition to change.
pub use identity_keys::{challenge_message, AUTH_DOMAIN_TAG};

/// The two routes the verify-middleware exempts, because they mint the token
/// every other route needs.
///
/// Public because a caller that classifies a mint failure has to say WHICH
/// route refused it — `chiefd refused /v1/auth/challenge with 401` names the
/// cause, where the same failure reported against the read that needed the
/// token names only a symptom.
pub const CHALLENGE_PATH: &str = "/v1/auth/challenge";
/// The token route. See [`CHALLENGE_PATH`].
pub const TOKEN_PATH: &str = "/v1/auth/token";

/// Why a bearer could not be produced.
///
/// Every variant names the thing an operator would have to change. A refusal
/// that says only "auth failed" costs a support round trip that the daemon's
/// own status code and body could have saved.
#[derive(Debug, thiserror::Error)]
pub enum BearerError {
    /// The key file is readable by its group or by the world.
    ///
    /// A HARD refusal, and the only one this module produces — see
    /// [`BearerError::is_key_hygiene_refusal`]. Ruling 1 says a key that became
    /// `0644` after it was written must STOP the daemon rather than warn it,
    /// and A1 implemented exactly that on the daemon side. A client that read
    /// the same file, saw the same mode and carried on would make the two
    /// halves of one rule disagree about one file — and it would be a fifth
    /// off switch, in the packet whose purpose is deleting the other four:
    /// `chmod g+r` would silently downgrade every command to anonymous.
    #[error("{identity_id} key: {reason}")]
    KeyTooPermissive {
        /// The identity whose key must not be used.
        identity_id: String,
        /// The refusal, naming the path, the mode found, and the `chmod 600`
        /// that fixes it.
        reason: String,
    },
    /// The key file is not there, or cannot be read at all.
    ///
    /// NOT a refusal, and the distinction is deliberate. The daemon mints
    /// `<dir>/.chief/keys/operator.key` at boot, so in a directory that has never run
    /// one there is legitimately no key — and `chief` reaches beacond and
    /// its own loopback listener BEFORE any daemon exists. Absence is a state
    /// the product passes through; a widened mode is not.
    #[error("{identity_id} key: {reason}")]
    KeyAbsent {
        /// The identity whose key was not found.
        identity_id: String,
        /// What the read reported, naming the path it wanted.
        reason: String,
    },
    /// `POST /v1/auth/challenge` did not answer `200`.
    #[error("challenge for {identity_id} refused: HTTP {status} {body}")]
    Challenge {
        /// The identity the nonce was asked for.
        identity_id: String,
        /// The status the daemon answered.
        status: u16,
        /// The daemon's own body, quoted verbatim.
        body: String,
    },
    /// `POST /v1/auth/token` did not answer `200`.
    #[error("token for {identity_id} refused: HTTP {status} {body}")]
    Token {
        /// The identity the token was asked for.
        identity_id: String,
        /// The status the daemon answered.
        status: u16,
        /// The daemon's own body, quoted verbatim.
        body: String,
    },
    /// A `200` whose body this client cannot read. A live endpoint answering a
    /// shape we do not understand is a BUILD SKEW, not an authentication
    /// failure, and it is worth saying so differently.
    #[error("{route} answered a body this client cannot read: {reason}")]
    Malformed {
        /// Which of the two routes answered it.
        route: &'static str,
        /// What the decode refused with.
        reason: String,
    },
    /// The key parsed as no P-256 private key, or signing failed.
    #[error("cannot sign with the {identity_id} key: {reason}")]
    Sign {
        /// The identity whose key would not sign.
        identity_id: String,
        /// What the crypto refused with.
        reason: String,
    },
    /// Nothing answered at all.
    #[error("could not reach {route} at {base_url}: {reason}")]
    Transport {
        /// Which of the two routes was being called.
        route: &'static str,
        /// The daemon this client was talking to.
        base_url: String,
        /// What the transport reported.
        reason: String,
    },
}

impl BearerError {
    /// Whether this refusal must stop the request rather than let it go out
    /// unauthenticated.
    ///
    /// Exactly one thing does: a key anyone but its owner can read.
    ///
    /// WHY THE TWO KEY CASES DIFFER, since they are one file away from each
    /// other and the split is the whole point:
    ///
    /// * A WIDENED MODE is a fact only this side can see. The daemon will never
    ///   mention it — after the gate is deleted its answer is a flat
    ///   `401 missing bearer token`, which names neither the file nor the mode
    ///   and sends the reader looking at enrolment. Ruling 1's words are that
    ///   such a key must STOP the caller rather than warn it, and A1 already
    ///   made the daemon refuse on it; a client that carried on would make the
    ///   two halves of one rule disagree about one file, and would turn
    ///   `chmod g+r` into a silent downgrade to anonymous.
    /// * An ABSENT key is a state the product legitimately passes through. The
    ///   daemon MINTS this file at boot, so a box that has never run one has
    ///   none, and `chief` reaches beacond and its own loopback listener
    ///   before any daemon exists. Refusing there would make a normal moment in
    ///   a company's life an outage of every command.
    ///
    /// Everything else — a challenge the daemon refused, an unreachable auth
    /// route, a body this build cannot read — is a condition the DAEMON is the
    /// authority on and answers precisely, so it is not this side's to refuse.
    #[must_use]
    pub fn is_key_hygiene_refusal(&self) -> bool {
        matches!(self, Self::KeyTooPermissive { .. })
    }

    /// Whether asking again, later, could plausibly answer differently.
    ///
    /// Nothing answered, or the daemon answered a 5xx — a company that is
    /// starting, restarting, or briefly wedged. Everything else is a settled
    /// verdict: a 401 means this identity is not enrolled, a bad key stays bad,
    /// and a body this build cannot read will not become readable. A caller
    /// with a retry ladder consults this so it does not hammer a refusal.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::Challenge { status, .. } | Self::Token { status, .. } => *status >= 500,
            Self::KeyTooPermissive { .. }
            | Self::KeyAbsent { .. }
            | Self::Malformed { .. }
            | Self::Sign { .. } => false,
        }
    }
}

/// A POST of one JSON body that carries NO credential.
///
/// The seam exists so [`Bearer`] can perform its two round trips without
/// owning a transport — the binary half has one and the resident actuator has
/// another, and neither may become the other's dependency.
///
/// **It must be implemented over an UNAUTHENTICATED send.** An implementation
/// that attached a bearer would call back into acquisition to get one, which
/// recurses forever. The two routes are exempt from the middleware precisely
/// so this is possible.
pub trait JsonPost {
    /// POST `body` to `url` and answer the status and the raw body text.
    ///
    /// `Err` is reserved for "nothing answered"; every status the daemon
    /// actually produced — 401 and 501 included — is an `Ok`.
    fn post_json_unauthenticated(
        &self,
        url: String,
        body: serde_json::Value,
    ) -> impl std::future::Future<Output = Result<(u16, String), String>> + Send;
}

/// One non-person principal: its enrolled identity id and the key file that
/// proves it.
#[derive(Debug, Clone)]
pub struct IdentityCredential {
    identity_id: String,
    key_path: PathBuf,
}

impl IdentityCredential {
    /// The operator, `<keys>/operator.key`.
    ///
    /// # It takes the KEYS DIRECTORY, and that is what deleted the confusion
    ///
    /// Its ancestors took "a root" and derived the rest, and there were two
    /// roots with the same name one directory apart — `~/.chiefd` and
    /// `~/.chiefd/orgs`, both spelled `--data-root` somewhere — so this type
    /// needed FOUR constructors, two of which existed only to say which of the
    /// two a caller happened to be holding, and one of which could answer
    /// `None`. #13 cost a full day to that collision.
    ///
    /// A company is a directory now and its keys are `<dir>/.chief/keys`, named
    /// once by `paths::keys_dir`. Taking the directory the keys are IN removes
    /// the derivation and therefore removes the question: there is nothing left
    /// to get wrong, so there is one constructor per principal.
    #[must_use]
    pub fn operator(keys_dir: &Path) -> Self {
        Self {
            identity_id: identity_keys::OPERATOR_IDENTITY_ID.to_string(),
            key_path: identity_keys::operator_key_path(keys_dir),
        }
    }

    /// The resident actuator, `<keys>/service.key`.
    ///
    /// A SEPARATE principal from the operator on purpose: an audit trail that
    /// cannot tell an automatic action from a deliberate one is worth much less
    /// than one that can.
    #[must_use]
    pub fn service(keys_dir: &Path) -> Self {
        Self {
            identity_id: identity_keys::SERVICE_IDENTITY_ID.to_string(),
            key_path: identity_keys::service_key_path(keys_dir),
        }
    }
}

/// Sign a challenge nonce with a PKCS#8 PEM P-256 private key.
///
/// Answers the base64 (standard alphabet) of the 64-byte IEEE-P1363 signature
/// `chiefd_api::authn::sig::verify_challenge` expects. Fixed-width `r||s`, never
/// DER — the daemon rejects a DER signature by length.
///
/// # Errors
/// [`BearerError::Sign`] when the PEM is not a P-256 private key.
pub fn sign_challenge(
    private_pem: &str,
    identity_id: &str,
    nonce: &str,
) -> Result<String, BearerError> {
    let secret = SecretKey::from_pkcs8_pem(private_pem).map_err(|error| BearerError::Sign {
        identity_id: identity_id.to_string(),
        reason: format!("not a PKCS#8 P-256 private key: {error}"),
    })?;
    let signing_key = SigningKey::from(&secret);
    let signature: Signature = signing_key.sign(&challenge_message(identity_id, nonce));
    Ok(BASE64_STANDARD.encode(signature.to_bytes()))
}

/// One principal's bearer tokens, one per daemon it has spoken to.
///
/// Keyed by base URL because a token is minted BY a company's daemon and is
/// only good there: identities live in each company's own database, and the
/// HS256 secret is that daemon's. A single cached token replayed at the next
/// company would 401 every call and look like a credential problem.
#[derive(Debug)]
pub struct Bearer {
    credential: IdentityCredential,
    tokens: Mutex<HashMap<String, String>>,
}

impl Bearer {
    /// Hold tokens for one principal.
    #[must_use]
    pub fn new(credential: IdentityCredential) -> Self {
        Self { credential, tokens: Mutex::new(HashMap::new()) }
    }

    /// The operator's bearer for the company whose keys are in `keys_dir`.
    #[must_use]
    pub fn operator(keys_dir: &Path) -> Self {
        Self::new(IdentityCredential::operator(keys_dir))
    }

    /// The resident actuator's bearer for the company whose keys are in
    /// `keys_dir`.
    #[must_use]
    pub fn service(keys_dir: &Path) -> Self {
        Self::new(IdentityCredential::service(keys_dir))
    }

    /// The enrolled identity this bearer proves.
    #[must_use]
    pub fn identity_id(&self) -> &str {
        &self.credential.identity_id
    }

    /// The key file this bearer signs with. Named in diagnostics so an operator
    /// is told WHICH file to look at.
    #[must_use]
    pub fn key_path(&self) -> &Path {
        &self.credential.key_path
    }

    /// The `Authorization` header value for `base_url`, acquiring one on first
    /// use.
    ///
    /// # Errors
    /// [`BearerError`] naming the step that refused. The caller decides what to
    /// do with it; this module never turns a missing credential into a refusal
    /// of the request itself.
    pub async fn authorization<T>(
        &self,
        transport: &T,
        base_url: &str,
    ) -> Result<String, BearerError>
    where
        T: JsonPost + Sync,
    {
        let key = normalized(base_url);
        if let Some(token) = self.cached(&key) {
            return Ok(format!("Bearer {token}"));
        }
        let token = self.acquire(transport, &key).await?;
        self.remember(&key, &token);
        Ok(format!("Bearer {token}"))
    }

    /// Drop the cached token for one daemon, so the next
    /// [`Bearer::authorization`] performs a fresh round trip.
    ///
    /// The daemon's HS256 secret is ephemeral unless a secret file was
    /// provisioned, so a chiefd restart rotates it and every cached bearer
    /// becomes garbage at the same instant. This is the half that recovers.
    pub fn invalidate(&self, base_url: &str) {
        let key = normalized(base_url);
        guarded(&self.tokens).remove(&key);
    }

    fn cached(&self, base_url: &str) -> Option<String> {
        guarded(&self.tokens).get(base_url).cloned()
    }

    fn remember(&self, base_url: &str, token: &str) {
        guarded(&self.tokens).insert(base_url.to_string(), token.to_string());
    }

    async fn acquire<T>(&self, transport: &T, base_url: &str) -> Result<String, BearerError>
    where
        T: JsonPost + Sync,
    {
        let identity_id = self.credential.identity_id.as_str();
        // Read the key FIRST. A key that is absent or too permissive is worth
        // discovering before a nonce is minted and left to expire.
        let pem =
            identity_keys::load_private_key_pem(&self.credential.key_path).map_err(|error| {
                let reason = error.to_string();
                let identity_id = identity_id.to_string();
                match error {
                    identity_keys::KeyError::TooPermissive { .. } => {
                        BearerError::KeyTooPermissive { identity_id, reason }
                    }
                    identity_keys::KeyError::Unreadable { .. } => {
                        BearerError::KeyAbsent { identity_id, reason }
                    }
                }
            })?;

        let (status, body) = transport
            .post_json_unauthenticated(
                format!("{base_url}{CHALLENGE_PATH}"),
                serde_json::json!({ "identityId": identity_id }),
            )
            .await
            .map_err(|reason| BearerError::Transport {
                route: CHALLENGE_PATH,
                base_url: base_url.to_string(),
                reason,
            })?;
        if status != 200 {
            return Err(BearerError::Challenge {
                identity_id: identity_id.to_string(),
                status,
                body,
            });
        }
        let challenge: ChallengeAnswer = serde_json::from_str(&body).map_err(|error| {
            BearerError::Malformed { route: CHALLENGE_PATH, reason: error.to_string() }
        })?;

        let signature = sign_challenge(&pem, identity_id, &challenge.nonce)?;
        let (status, body) = transport
            .post_json_unauthenticated(
                format!("{base_url}{TOKEN_PATH}"),
                serde_json::json!({ "nonceId": challenge.nonce_id, "signature": signature }),
            )
            .await
            .map_err(|reason| BearerError::Transport {
                route: TOKEN_PATH,
                base_url: base_url.to_string(),
                reason,
            })?;
        if status != 200 {
            return Err(BearerError::Token { identity_id: identity_id.to_string(), status, body });
        }
        let minted: TokenAnswer = serde_json::from_str(&body).map_err(|error| {
            BearerError::Malformed { route: TOKEN_PATH, reason: error.to_string() }
        })?;
        Ok(minted.token)
    }
}

/// `POST /v1/auth/challenge`'s answer. camelCase on the wire, like every other
/// docstore route.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeAnswer {
    nonce_id: String,
    nonce: String,
}

/// `POST /v1/auth/token`'s answer.
#[derive(serde::Deserialize)]
struct TokenAnswer {
    token: String,
}

/// One base URL, spelled one way, so `http://host:1/` and `http://host:1` do
/// not occupy two cache slots and mint two tokens.
fn normalized(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

/// The cache guard, taking a poisoned lock's contents rather than panicking.
///
/// A panic while a token map was borrowed must not make every later request
/// unauthenticated for the life of the process. Nothing in the map is an
/// invariant a panic could have half-written — it is a `String` keyed by a
/// `String` — so the contents are safe to keep.
fn guarded(
    tokens: &Mutex<HashMap<String, String>>,
) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
    match tokens.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    // The fixtures stage a real key file in a tempdir and chmod it, which is
    // the sanctioned test use of the seam-disallowed writer: production
    // filesystem effects belong to `chiefd_host`, and this module has no
    // production writer at all.
    #![allow(clippy::disallowed_methods)]
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use p256::ecdsa::signature::Verifier as _;
    use p256::ecdsa::VerifyingKey;
    use p256::pkcs8::{EncodePrivateKey as _, LineEnding};

    use super::*;

    /// The keys directory a company's fixture stages into, `<dir>/.chief/keys`
    /// — the layout `paths::keys_dir` names in production, spelled here
    /// because the test half may not reach the binary's module.
    fn keys_of(dir: &Path) -> PathBuf {
        identity_keys::keys_dir(&dir.join(".chief"))
    }

    /// A key on disk at `mode`, plus the public half a verifier would hold.
    /// Deterministic scalar so no RNG feature is needed.
    fn staged_key(dir: &Path, filename: &str, mode: u32) -> (PathBuf, VerifyingKey) {
        let keys = keys_of(dir);
        std::fs::create_dir_all(&keys).expect("keys dir");
        let secret = SecretKey::from_slice(&[7u8; 32]).expect("scalar");
        let path = keys.join(filename);
        std::fs::write(&path, secret.to_pkcs8_pem(LineEnding::LF).expect("pem").as_bytes())
            .expect("write key");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        (path, VerifyingKey::from(SigningKey::from(&secret)))
    }

    /// A stand-in daemon: it issues a nonce, VERIFIES the signature the way
    /// `chiefd_api::authn::sig::verify_challenge` does, and mints a token only
    /// when it holds. The verification is the point — a stub that accepted any
    /// signature would prove nothing about the bytes this module produces.
    struct StubDaemon {
        verifying_key: VerifyingKey,
        nonce: String,
        expected_identity: String,
        challenges: AtomicUsize,
        tokens: AtomicUsize,
    }

    impl StubDaemon {
        fn new(verifying_key: VerifyingKey, expected_identity: &str) -> Self {
            Self {
                verifying_key,
                nonce: "0123456789abcdef0123456789abcdef".to_string(),
                expected_identity: expected_identity.to_string(),
                challenges: AtomicUsize::new(0),
                tokens: AtomicUsize::new(0),
            }
        }
    }

    impl JsonPost for StubDaemon {
        async fn post_json_unauthenticated(
            &self,
            url: String,
            body: serde_json::Value,
        ) -> Result<(u16, String), String> {
            if url.ends_with(CHALLENGE_PATH) {
                self.challenges.fetch_add(1, Ordering::SeqCst);
                if body["identityId"].as_str() != Some(self.expected_identity.as_str()) {
                    return Ok((401, "unknown or inactive identity".to_string()));
                }
                return Ok((200, format!(r#"{{"nonceId":"n-1","nonce":"{}"}}"#, self.nonce)));
            }
            if url.ends_with(TOKEN_PATH) {
                self.tokens.fetch_add(1, Ordering::SeqCst);
                let encoded = body["signature"].as_str().unwrap_or_default();
                let Ok(raw) = BASE64_STANDARD.decode(encoded) else {
                    return Ok((401, "challenge not satisfied".to_string()));
                };
                let Ok(signature) = Signature::from_slice(&raw) else {
                    return Ok((401, "challenge not satisfied".to_string()));
                };
                let message = challenge_message(&self.expected_identity, &self.nonce);
                if self.verifying_key.verify(&message, &signature).is_err() {
                    return Ok((401, "challenge not satisfied".to_string()));
                }
                return Ok((200, r#"{"token":"minted-jwt"}"#.to_string()));
            }
            Err(format!("no route {url}"))
        }
    }

    /// THE RULE THE WHOLE MODULE EXISTS FOR. The signature this client produces
    /// is accepted by the daemon's own verification — same message layout, same
    /// fixed-width encoding, same base64 alphabet. Two literals that agreed by
    /// inspection is exactly what this replaces.
    #[tokio::test]
    async fn the_signature_this_client_produces_is_one_the_daemon_accepts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_path, verifying_key) = staged_key(dir.path(), "operator.key", 0o600);
        let daemon = StubDaemon::new(verifying_key, identity_keys::OPERATOR_IDENTITY_ID);

        let bearer = Bearer::operator(&keys_of(dir.path()));
        let header = bearer
            .authorization(&daemon, "http://127.0.0.1:8791")
            .await
            .expect("the daemon accepts what this client signed");

        assert_eq!(header, "Bearer minted-jwt");
        assert_eq!(daemon.tokens.load(Ordering::SeqCst), 1);
    }

    /// A signature is 64 bytes of `r||s`, never DER. The daemon rejects a DER
    /// signature by LENGTH, so a wrong encoding here fails as "challenge not
    /// satisfied" and looks like a key problem.
    #[test]
    fn the_signature_is_fixed_width_p1363_base64() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, _) = staged_key(dir.path(), "operator.key", 0o600);
        let pem = std::fs::read_to_string(&path).expect("read");
        let encoded = sign_challenge(&pem, "operator", "n").expect("sign");
        assert_eq!(BASE64_STANDARD.decode(&encoded).expect("base64").len(), 64);
    }

    /// The token is cached per daemon: a second call performs NO round trip.
    /// Without this every request would mint a token, which is a challenge and
    /// a signature per call.
    #[tokio::test]
    async fn a_token_is_acquired_once_per_daemon_and_reused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_path, verifying_key) = staged_key(dir.path(), "operator.key", 0o600);
        let daemon = StubDaemon::new(verifying_key, identity_keys::OPERATOR_IDENTITY_ID);
        let bearer = Bearer::operator(&keys_of(dir.path()));

        for _ in 0..3 {
            bearer.authorization(&daemon, "http://127.0.0.1:8791").await.expect("header");
        }
        assert_eq!(daemon.challenges.load(Ordering::SeqCst), 1);

        // A trailing slash is the SAME daemon, not a second one.
        bearer.authorization(&daemon, "http://127.0.0.1:8791/").await.expect("header");
        assert_eq!(daemon.challenges.load(Ordering::SeqCst), 1);

        // And invalidation is what makes a restarted daemon recoverable.
        bearer.invalidate("http://127.0.0.1:8791");
        bearer.authorization(&daemon, "http://127.0.0.1:8791").await.expect("header");
        assert_eq!(daemon.challenges.load(Ordering::SeqCst), 2);
    }

    /// THE HYGIENE RULE, and the one refusal this module makes HARD. Ruling 1:
    /// a key that widened after it was written must STOP the caller, not warn
    /// it. A1 made the daemon refuse on exactly this; a client that read the
    /// same file, saw the same mode and carried on would make the two halves
    /// of one rule disagree, and `chmod g+r` would become a silent fifth off
    /// switch in the packet that deletes the other four.
    #[tokio::test]
    async fn a_group_readable_key_is_a_hard_refusal_and_an_absent_one_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_path, verifying_key) = staged_key(dir.path(), "operator.key", 0o640);
        let daemon = StubDaemon::new(verifying_key, identity_keys::OPERATOR_IDENTITY_ID);

        let loose = Bearer::operator(&keys_of(dir.path()))
            .authorization(&daemon, "http://127.0.0.1:8791")
            .await
            .expect_err("a loose key must not authenticate");
        assert!(loose.is_key_hygiene_refusal(), "{loose}");

        // Absence is the other case, and it must NOT be a hard refusal: the
        // daemon mints the key at boot, so `chief` legitimately runs
        // before one exists.
        let absent = Bearer::service(&keys_of(dir.path()))
            .authorization(&daemon, "http://127.0.0.1:8791")
            .await
            .expect_err("an absent key cannot authenticate either");
        assert!(!absent.is_key_hygiene_refusal(), "{absent}");
    }

    /// Nothing else is a hard refusal. A daemon that refuses the challenge is
    /// the DAEMON's verdict and it answers precisely; only a widened mode is a
    /// fact this side alone can see.
    #[tokio::test]
    async fn a_refused_challenge_is_not_a_key_hygiene_refusal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_path, verifying_key) = staged_key(dir.path(), "operator.key", 0o600);
        let daemon = StubDaemon::new(verifying_key, identity_keys::SERVICE_IDENTITY_ID);

        let refusal = Bearer::operator(&keys_of(dir.path()))
            .authorization(&daemon, "http://127.0.0.1:8791")
            .await
            .expect_err("an unenrolled identity has no nonce");

        assert!(!refusal.is_key_hygiene_refusal(), "{refusal}");
    }

    /// A key readable by anyone else is refused BEFORE a nonce is spent, and
    /// the refusal states the command that fixes it.
    #[tokio::test]
    async fn a_group_readable_key_refuses_and_names_the_way_through() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_path, verifying_key) = staged_key(dir.path(), "operator.key", 0o640);
        let daemon = StubDaemon::new(verifying_key, identity_keys::OPERATOR_IDENTITY_ID);

        let refusal = Bearer::operator(&keys_of(dir.path()))
            .authorization(&daemon, "http://127.0.0.1:8791")
            .await
            .expect_err("a loose key must not authenticate");

        let message = refusal.to_string();
        assert!(message.contains("chmod 600"), "{message}");
        assert!(message.contains("operator"), "{message}");
        assert_eq!(daemon.challenges.load(Ordering::SeqCst), 0, "no nonce is spent");
    }

    /// An absent key names the FILE. On a box whose daemon has never booted
    /// there is no key yet, and "unauthorized" would send the reader looking at
    /// the wrong thing.
    #[tokio::test]
    async fn an_absent_key_names_the_file_it_wanted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_path, verifying_key) = staged_key(dir.path(), "operator.key", 0o600);
        let daemon = StubDaemon::new(verifying_key, identity_keys::SERVICE_IDENTITY_ID);

        // The service key was never minted; only the operator's was.
        let refusal = Bearer::service(&keys_of(dir.path()))
            .authorization(&daemon, "http://127.0.0.1:8791")
            .await
            .expect_err("an absent key cannot authenticate");

        assert!(refusal.to_string().contains("service.key"), "{refusal}");
    }

    /// TWO COMPANIES ARE TWO TRUST ROOTS, because each one's daemon mints its
    /// own keys inside its own directory.
    ///
    /// The retired shape had ONE `<data-root>/keys` for every company on the
    /// box — one operator per fleet — so a client that resolved the wrong root
    /// signed with a key from another fleet, which is what #13 cost a day to.
    /// There is no root to resolve now: the keys are `<dir>/.chief/keys`, and
    /// two directories cannot share them.
    #[test]
    fn each_company_directory_carries_its_own_operator_key() {
        let here = Bearer::operator(Path::new("/work/acme/.chief/keys"));
        let elsewhere = Bearer::operator(Path::new("/elsewhere/acme/.chief/keys"));
        assert_ne!(here.key_path(), elsewhere.key_path());
        assert_eq!(here.key_path(), Path::new("/work/acme/.chief/keys/operator.key"));
        assert_eq!(here.identity_id(), elsewhere.identity_id(), "one operator, per company");
    }

    /// Retrying is worth it only when asking again could answer differently.
    /// A 401 is a settled verdict and a bad key stays bad; a 5xx or an
    /// unreachable daemon is a company that may simply be starting.
    #[test]
    fn only_an_unreachable_daemon_or_a_5xx_is_worth_asking_again() {
        let transport = BearerError::Transport {
            route: "/v1/auth/token",
            base_url: "http://127.0.0.1:1".to_string(),
            reason: "connection refused".to_string(),
        };
        assert!(transport.is_transient());
        let busy = BearerError::Challenge {
            identity_id: "operator".to_string(),
            status: 503,
            body: String::new(),
        };
        assert!(busy.is_transient());
        let unenrolled = BearerError::Challenge {
            identity_id: "operator".to_string(),
            status: 401,
            body: String::new(),
        };
        assert!(!unenrolled.is_transient());
        let loose = BearerError::KeyTooPermissive {
            identity_id: "operator".to_string(),
            reason: String::new(),
        };
        assert!(!loose.is_transient());
    }

    /// The two principals are separate files and separate identities. A
    /// service that authenticated as the operator would make the audit trail
    /// this workstream is building worthless.
    #[test]
    fn the_operator_and_the_service_are_different_principals() {
        let keys = Path::new("/work/acme/.chief/keys");
        let operator = Bearer::operator(keys);
        let service = Bearer::service(keys);
        assert_eq!(operator.identity_id(), "operator");
        assert_eq!(service.identity_id(), "service");
        assert_ne!(operator.key_path(), service.key_path());
        assert_eq!(operator.key_path(), Path::new("/work/acme/.chief/keys/operator.key"));
        assert_eq!(service.key_path(), Path::new("/work/acme/.chief/keys/service.key"));
    }

    /// A refusal from the challenge route is reported as the DAEMON's answer,
    /// quoted, rather than flattened into "auth failed".
    #[tokio::test]
    async fn a_refused_challenge_quotes_the_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_path, verifying_key) = staged_key(dir.path(), "operator.key", 0o600);
        // The stub expects `service`, so the operator's challenge is refused.
        let daemon = StubDaemon::new(verifying_key, identity_keys::SERVICE_IDENTITY_ID);

        let refusal = Bearer::operator(&keys_of(dir.path()))
            .authorization(&daemon, "http://127.0.0.1:8791")
            .await
            .expect_err("an unenrolled identity has no nonce");

        let message = refusal.to_string();
        assert!(message.contains("HTTP 401"), "{message}");
        assert!(message.contains("unknown or inactive identity"), "{message}");
    }
}
