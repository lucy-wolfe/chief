//! A3, acceptance criterion 5: the resident actuator reaches chiefd WITH A
//! BEARER, proved by a test rather than by inspection.
//!
//! Everything here is real except the daemon's product logic: a real TCP
//! listener, real HTTP over hyper, a real P-256 key read off disk at 0600, a
//! real ECDSA signature over `identity_keys::challenge_message`, and a real
//! `Authorization` header on the reads. The server is a stub of chiefd's three
//! relevant routes, and it VERIFIES the signature with the enrolled public half
//! exactly as `chiefd_api::authn::sig::verify_challenge` does — so a client that
//! signed the wrong bytes would fail here rather than pass a test that only
//! checked a header was present.
//!
//! It also pins the two rules that are easy to get wrong:
//!
//! * the two auth routes are middleware-EXEMPT and must be called with no
//!   credential, or acquiring a token would require a token;
//! * a `401` re-acquires ONCE and retries, and the second token is then cached
//!   rather than re-minted per call.

// Staging a key fixture in a tempdir is the sanctioned use of the
// seam-disallowed writer: production filesystem effects belong to
// `chiefd_host`, and nothing in this crate writes a key.
#![allow(clippy::disallowed_methods)]
// The stub's route handlers are not `#[test]` functions, so clippy's
// allow-*-in-tests switches do not reach them. Same allow every other
// integration test in this crate carries, for the same reason.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::pkcs8::{EncodePrivateKey, LineEnding};
use p256::SecretKey;

use chief_cli::actuate::client::{ActuationClient, Wake};
use chief_cli::bearer::Bearer;

/// The only token this stub's read routes accept. The first token it mints is
/// deliberately NOT this one, which is what makes the re-acquire path real: a
/// cached token that outlived the daemon's signing secret looks exactly like
/// this from the client side.
const ACCEPTED_TOKEN: &str = "token-2";

struct Stub {
    verifying_key: VerifyingKey,
    nonces: Mutex<HashMap<String, String>>,
    minted: AtomicUsize,
    /// Every `Authorization` header the AUTH routes saw. Must stay empty.
    auth_route_credentials: Mutex<Vec<String>>,
    /// Every `Authorization` header the READ routes saw, in order.
    read_credentials: Mutex<Vec<String>>,
}

async fn challenge(
    State(stub): State<Arc<Stub>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    stub.auth_route_credentials.lock().expect("lock").extend(presented(&headers));
    assert_eq!(body["identityId"], "service", "the actuator claims its own principal");
    let nonce_id = format!("nonce-id-{}", stub.nonces.lock().expect("lock").len() + 1);
    let nonce = format!("nonce-value-{nonce_id}");
    stub.nonces.lock().expect("lock").insert(nonce_id.clone(), nonce.clone());
    Json(serde_json::json!({ "nonceId": nonce_id, "nonce": nonce })).into_response()
}

async fn token(
    State(stub): State<Arc<Stub>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    stub.auth_route_credentials.lock().expect("lock").extend(presented(&headers));
    let nonce_id = body["nonceId"].as_str().expect("a nonce id");
    let nonce = stub.nonces.lock().expect("lock").get(nonce_id).cloned().expect("an issued nonce");
    let signature_bytes = BASE64_STANDARD
        .decode(body["signature"].as_str().expect("a signature"))
        .expect("standard base64");
    assert_eq!(signature_bytes.len(), 64, "IEEE-P1363 fixed-width r||s, never DER");
    let signature = Signature::from_slice(&signature_bytes).expect("a P-256 signature");

    // THE REAL CHECK. The client signed `tag || identityId || nonce` with the
    // key on disk, and this is the daemon's half of that contract.
    stub.verifying_key
        .verify(&identity_keys::challenge_message("service", &nonce), &signature)
        .expect("the actuator's signature must verify against its enrolled key");

    let minted = stub.minted.fetch_add(1, Ordering::SeqCst) + 1;
    Json(serde_json::json!({ "token": format!("token-{minted}") })).into_response()
}

async fn desired(State(stub): State<Arc<Stub>>, headers: HeaderMap) -> Response {
    if let Some(refusal) = stub.refuse_without_the_accepted_token(&headers) {
        return refusal;
    }
    Json(serde_json::json!({
        "company": "acme",
        "actuationMode": "apply",
        "people": [{ "personId": "vera", "launchHash": "aaa" }],
    }))
    .into_response()
}

async fn watch(State(stub): State<Arc<Stub>>, headers: HeaderMap) -> Response {
    if let Some(refusal) = stub.refuse_without_the_accepted_token(&headers) {
        return refusal;
    }
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        "event: doc-change\nid: 7\ndata: {\"seq\":7,\"store\":\"activity\"}\n\n",
    )
        .into_response()
}

impl Stub {
    fn refuse_without_the_accepted_token(&self, headers: &HeaderMap) -> Option<Response> {
        let presented = presented(headers);
        self.read_credentials.lock().expect("lock").extend(presented.clone());
        let accepted = format!("Bearer {ACCEPTED_TOKEN}");
        if presented.first() == Some(&accepted) {
            return None;
        }
        Some(
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "code": "unauthenticated", "detail": "stale token" })),
            )
                .into_response(),
        )
    }
}

fn presented(headers: &HeaderMap) -> Vec<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| vec![value.to_owned()])
        .unwrap_or_default()
}

/// A company's keys directory, `<dir>/.chief/keys`.
fn keys_of(dir: &std::path::Path) -> std::path::PathBuf {
    identity_keys::keys_dir(&dir.join(".chief"))
}

/// Write the actuator's key where the client reads it from, at 0600, and
/// answer the public half the stub enrols.
fn stage_service_key(dir: &std::path::Path) -> VerifyingKey {
    use std::os::unix::fs::PermissionsExt as _;

    let keys = keys_of(dir);
    std::fs::create_dir_all(&keys).expect("keys dir");
    let secret = SecretKey::from_slice(&[9u8; 32]).expect("scalar");
    let path = identity_keys::service_key_path(&keys);
    std::fs::write(&path, secret.to_pkcs8_pem(LineEnding::LF).expect("pem").as_bytes())
        .expect("stage the service key");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    VerifyingKey::from(secret.public_key())
}

#[tokio::test]
async fn the_actuator_signs_for_its_own_identity_and_presents_the_token_on_every_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = Arc::new(Stub {
        verifying_key: stage_service_key(dir.path()),
        nonces: Mutex::new(HashMap::new()),
        minted: AtomicUsize::new(0),
        auth_route_credentials: Mutex::new(Vec::new()),
        read_credentials: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route("/v1/auth/challenge", post(challenge))
        .route("/v1/auth/token", post(token))
        .route("/v1/org/runtime/desired", post(desired))
        .route("/v1/docs/watch", get(watch))
        .with_state(Arc::clone(&stub));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind the stub");
    let url = format!("http://{}", listener.local_addr().expect("stub address"));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let client =
        ActuationClient::new(&url, "0123456789ab", Arc::new(Bearer::service(&keys_of(dir.path()))));

    // The first read: acquire `token-1`, be refused, re-acquire `token-2`, and
    // succeed. One retry, never a loop.
    let desired = client.desired().await.expect("the desired set");
    assert_eq!(desired.people.len(), 1);
    assert_eq!(desired.company, "acme");

    // The second read costs no acquisition at all: the token is cached.
    client.desired().await.expect("the desired set again");

    // And the changefeed carries the same credential — it is not a route that
    // gets to be anonymous because it is long-lived.
    let wake = client.wait(Some(3), Duration::from_secs(5)).await.expect("a wake");
    assert_eq!(wake, Wake::Change { seq: 7 });

    let accepted = format!("Bearer {ACCEPTED_TOKEN}");
    assert_eq!(
        *stub.read_credentials.lock().expect("lock"),
        vec!["Bearer token-1".to_owned(), accepted.clone(), accepted.clone(), accepted],
        "every read carries a bearer; the refused one is re-acquired exactly once and then cached"
    );
    assert_eq!(stub.minted.load(Ordering::SeqCst), 2, "one re-acquisition, not one per call");
    assert!(
        stub.auth_route_credentials.lock().expect("lock").is_empty(),
        "the two auth routes are middleware-exempt: a transport that authenticated them would \
         need a token in order to get a token"
    );
}
