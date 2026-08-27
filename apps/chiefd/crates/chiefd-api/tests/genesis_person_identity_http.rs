//! Genesis provisions every person's identity, so a person can authenticate
//! with NOTHING run in between.
//!
//! The defect this pins: a person's key was minted by the materializer and
//! enrolled by `refresh_materialization`, its only caller. Both need a runtime
//! host, so a company that had not converged held people who genuinely exist
//! and can prove nothing — `/v1/auth/challenge` answered 401 for the CEO of a
//! company created one call earlier. Every wait in the older suites sat AFTER a
//! tool call whose reconcile enrolled as a side effect, which is why a tool call
//! was the hidden precondition of the credential that tool call needed.
//!
//! This suite deliberately makes NO reconcile, materialize or roster call
//! between genesis and the challenge. The only mount capability it wires is the
//! company's org directory, which `chiefd run --serve-only` has too — so a
//! surface with no runtime host mints a person bearer here exactly as this test
//! does.

// `std::fs::write` is the seam-disallowed method in PRODUCTION (file effects
// belong to chiefd_host); staging a tempdir fixture in a test is the sanctioned
// use, same allow `authn::boot`'s own key-staging tests carry.
#![allow(clippy::expect_used, clippy::panic, clippy::disallowed_methods)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chiefd_api::docstore::{
    router_with_live_resolver, ChangeFeed, DocStore, LiveResolutionMode, SupervisionLiveResolver,
    SupervisionLiveSource,
};
use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SystemClock;
use http_body_util::BodyExt;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use p256::SecretKey;
use serde_json::{json, Value};
use tower::ServiceExt;

/// `slugify("Identity Genesis")`, which the route derives for itself.
const SLUG: &str = "identity-genesis";
const SPEC_NAME: &str = "Identity Genesis";
const GENESIS_AT: &str = "2026-08-13T00:00:00.000Z";
struct Fixture {
    app: axum::Router,
    company: Arc<CompanyDb>,
    /// The COMPANY DIRECTORY. Agent homes hang off `<dir>/.chief/agent/<id>/`.
    company_dir: std::path::PathBuf,
    operator_bearer: String,
    _dir: tempfile::TempDir,
}

/// Stage a daemon key exactly as `ensure_identity_key` writes it.
fn stage_daemon_key(path: &Path, scalar: u8) -> SigningKey {
    use std::os::unix::fs::PermissionsExt as _;

    let key = SecretKey::from_slice(&[scalar; 32]).expect("scalar");
    let pem = key.to_pkcs8_pem(LineEnding::LF).expect("pem");
    std::fs::write(path, pem.as_bytes()).expect("write key");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    SigningKey::from(key)
}

fn sign_challenge(key: &SigningKey, identity_id: &str, nonce: &str) -> String {
    let signature: Signature = key.sign(&identity_keys::challenge_message(identity_id, nonce));
    BASE64_STANDARD.encode(signature.to_bytes())
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let company_dir = dir.path().join(SLUG);
    std::fs::create_dir_all(&company_dir).expect("company dir");
    let company = Arc::new(
        CompanyDb::open(
            SLUG,
            &company_dir.join("company.sqlite"),
            Arc::new(SystemClock::default()),
        )
        .expect("company"),
    );

    // The daemon's own two principals, minted and enrolled before anything is
    // served — the precedent this change extends to people.
    // A company's keys live inside the company's own directory, `<dir>/.chief/keys`.
    let keys_dir = identity_keys::keys_dir(&company_dir.join(".chief"));
    std::fs::create_dir_all(&keys_dir).expect("mkdir keys");
    let operator_key = stage_daemon_key(&identity_keys::operator_key_path(&keys_dir), 7);
    stage_daemon_key(&identity_keys::service_key_path(&keys_dir), 9);
    let auth = chiefd_api::authn::boot::build_auth_runtime(
        Arc::clone(&company),
        &keys_dir,
        Arc::new(|| 1_700_000_000_000),
    )
    .await
    .expect("auth runtime");

    // The operator bearer genesis itself needs. Minted in-process, exactly as
    // the CLI mints it over the two exempt routes.
    let issued =
        auth.challenge(identity_keys::OPERATOR_IDENTITY_ID).await.expect("operator challenge");
    let operator_bearer = auth
        .redeem(
            &issued.nonce_id,
            &sign_challenge(&operator_key, identity_keys::OPERATOR_IDENTITY_ID, &issued.nonce),
        )
        .await
        .expect("operator token");

    let shipped_skills_root = dir.path().join("shipped-skills");
    std::fs::create_dir_all(shipped_skills_root.join("manager")).expect("shipped skill dir");
    std::fs::write(
        shipped_skills_root.join("manager/SKILL.md"),
        "---\nname: manager\ndescription: Manage this company.\n---\n",
    )
    .expect("shipped skill");
    std::fs::create_dir_all(shipped_skills_root.join("founder")).expect("Founder skill dir");
    std::fs::write(
        shipped_skills_root.join("founder/SKILL.md"),
        "---\nname: founder\ndescription: Start a company.\n---\n",
    )
    .expect("Founder skill");

    let source =
        SupervisionLiveSource::new(Arc::clone(&company), SLUG.to_owned()).with_agent_home_root(
            chiefd_api::docstore::AgentHomeRoot { dir: company_dir.clone(), shipped_skills_root },
        );
    let resolver: SupervisionLiveResolver = Arc::new(move |slug, mode| {
        (slug == SLUG && mode == LiveResolutionMode::Genesis).then(|| source.clone())
    });

    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&dir.path().join("control.sqlite").display().to_string(), 2, feed)
            .expect("control docstore"),
    );
    store.ensure_schema().await.expect("control schema");
    let app = router_with_live_resolver(
        store,
        1024 * 1024,
        Duration::from_secs(15),
        None,
        Some(resolver),
        Some(auth),
    );
    Fixture { app, company, company_dir, operator_bearer, _dir: dir }
}

impl Fixture {
    async fn post(&self, path: &str, body: &Value, bearer: Option<&str>) -> (StatusCode, Value) {
        let mut builder =
            Request::builder().method("POST").uri(path).header("content-type", "application/json");
        if let Some(token) = bearer {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let request = builder.body(Body::from(body.to_string())).expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response.into_body().collect().await.expect("body").to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    /// Genesis and nothing else. No reconcile, no materialize, no roster call.
    async fn genesis(&self) {
        let (status, body) = self
            .post("/v1/org/manifest/genesis", &genesis_request(), Some(&self.operator_bearer))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["created"], json!(true));
    }
}

fn genesis_request() -> Value {
    json!({
        "slug": SLUG,
        "spec": {
            "name": SPEC_NAME,
            "purpose": "Prove a person can authenticate the moment they exist.",
            "chief": { "id": "chief", "name": "Avery" },
            "departments": [{
                "name": "Quant",
                "purpose": "Own systematic research.",
                "head": { "id": "quant-head", "name": "Quinn" },
                "staff": [{ "id": "signal-researcher", "name": "Signal Researcher" }]
            }]
        },
        "at": GENESIS_AT,
    })
}

/// Genesis owns the first project-skill seed. The Founder-only skill remains
/// outside the company root.
#[tokio::test]
async fn genesis_seeds_the_shipped_company_skills() {
    let f = fixture().await;
    f.genesis().await;

    let skills = f.company_dir.join(".pi/skills");
    assert!(skills.join("manager/SKILL.md").is_file());
    assert!(!skills.join("founder").exists());
}

/// THE REGRESSION. Genesis, then authenticate — with nothing in between.
#[tokio::test]
async fn a_person_can_authenticate_immediately_after_genesis() {
    let f = fixture().await;
    f.genesis().await;

    // 1. The Chief key exists directly under `.chief`, minted by the act that
    //    created the person rather than by a later convergence pass. The Chief
    //    is the operator Pi and has no managed agent home.
    let key_path = chiefd_host::agent_home::chief_identity_key_path(&f.company_dir);
    let pem = std::fs::read_to_string(&key_path)
        .unwrap_or_else(|error| panic!("no identity key at {}: {error}", key_path.display()));

    // 2. The challenge is issued — the identity row exists.
    let (status, challenge) =
        f.post("/v1/auth/challenge", &json!({ "identityId": "chief" }), None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the CEO of a company created one call ago must be able to prove who they are"
    );

    // 3. And the key on disk redeems it, so the enrolled half is the public half
    //    of the file the person actually signs with.
    let key = SigningKey::from(SecretKey::from_pkcs8_pem(&pem).expect("p-256 pkcs#8"));
    let (status, token) = f
        .post(
            "/v1/auth/token",
            &json!({
                "nonceId": challenge["nonceId"],
                "signature": sign_challenge(
                    &key,
                    "chief",
                    challenge["nonce"].as_str().expect("nonce"),
                ),
            }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{token}");
    assert!(token["token"].as_str().is_some_and(|t| !t.is_empty()), "{token}");
}

/// Every person of the roster, not only the CEO.
#[tokio::test]
async fn the_whole_genesis_roster_is_provisioned() {
    let f = fixture().await;
    f.genesis().await;
    for person_id in ["chief", "quant-head", "signal-researcher"] {
        let (status, _) =
            f.post("/v1/auth/challenge", &json!({ "identityId": person_id }), None).await;
        assert_eq!(status, StatusCode::OK, "{person_id} must be able to authenticate");
    }
}

/// The negative, and the whole point of keeping it: provisioning enrols exactly
/// the people the company committed, and nobody else. "Everyone is enrolled" and
/// "the right people are enrolled" look identical from the positive direction.
#[tokio::test]
async fn a_person_who_was_never_provisioned_still_cannot_authenticate() {
    let f = fixture().await;
    f.genesis().await;
    let (status, _) = f.post("/v1/auth/challenge", &json!({ "identityId": "ghost" }), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "an unenrolled id is refused, not invented");
}

/// A revoked person is refused even though their key is still on disk and their
/// row is still there. Enrolment did not become permission.
#[tokio::test]
async fn a_revoked_person_still_cannot_authenticate() {
    let f = fixture().await;
    f.genesis().await;
    let (status, _) = f.post("/v1/auth/challenge", &json!({ "identityId": "chief" }), None).await;
    assert_eq!(status, StatusCode::OK, "enrolled before revocation");

    assert!(
        f.company.identity_revoke("chief".to_owned(), 1_700_000_001_000).await.expect("revoke"),
        "the row exists to revoke"
    );

    let (status, _) = f.post("/v1/auth/challenge", &json!({ "identityId": "chief" }), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a revoked identity is inactive, not re-enrolled");
    assert!(
        chiefd_host::agent_home::chief_identity_key_path(&f.company_dir).exists(),
        "revocation is a trust-table act; provisioning never deletes or re-mints the key"
    );
}
