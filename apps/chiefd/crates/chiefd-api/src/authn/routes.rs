//! The `/v1/auth/*` HTTP handlers (agent-auth P0). These two routes are the
//! ONLY ones the verify-middleware exempts, because they mint the token every
//! other route needs. They read the shared [`AuthRuntime`] via an `Extension`;
//! `None` means the surface is running without auth configured (standalone /
//! migration docstore), and both answer `501`.
//!
//! Bodies are camelCase — the client is JSON from TypeScript, like every other
//! docstore route.

use std::sync::Arc;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use chiefd_core::store::identities::IdentityKind;

use crate::docstore::route_error::RouteError;

use super::middleware::CallerIdentity;
use super::runtime::{AuthRuntime, ChallengeError, EnrollError, RedeemError};

/// `POST /v1/auth/challenge` body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeRequest {
    /// The identity the caller claims to be; the nonce is bound to it.
    pub identity_id: String,
}

/// `POST /v1/auth/challenge` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeResponse {
    /// Opaque handle returned with the signature.
    pub nonce_id: String,
    /// The nonce to sign (inside the domain-separated message).
    pub nonce: String,
}

/// `POST /v1/auth/token` body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRequest {
    /// The challenge handle.
    pub nonce_id: String,
    /// base64 (standard) of the IEEE-P1363 P-256 signature.
    pub signature: String,
}

/// `POST /v1/auth/token` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    /// The minted bearer token.
    pub token: String,
}

const NOT_CONFIGURED: &str = "auth not configured on this surface";

/// `POST /v1/auth/challenge` — issue an identity-bound nonce. An unknown or
/// inactive identity is a flat `401` (no existence oracle). An identity store
/// that could not be READ is a `503` instead: no verdict was reached, so
/// answering with one tells a client to stop asking about a fault that
/// resolves on its own (#1204).
pub async fn challenge(
    Extension(runtime): Extension<Option<Arc<AuthRuntime>>>,
    Json(request): Json<ChallengeRequest>,
) -> Response {
    let Some(runtime) = runtime else {
        return (StatusCode::NOT_IMPLEMENTED, NOT_CONFIGURED).into_response();
    };
    match runtime.challenge(&request.identity_id).await {
        Ok(issued) => (
            StatusCode::OK,
            Json(ChallengeResponse { nonce_id: issued.nonce_id, nonce: issued.nonce }),
        )
            .into_response(),
        Err(ChallengeError::UnknownIdentity | ChallengeError::Inactive) => {
            (StatusCode::UNAUTHORIZED, "unknown or inactive identity").into_response()
        }
        Err(ChallengeError::Unavailable { reason }) => {
            RouteError::unavailable("identity-store-unavailable", reason).into_response()
        }
        Err(ChallengeError::Entropy) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "entropy unavailable").into_response()
        }
    }
}

/// `POST /v1/auth/enroll` body. Enrols a keypair identity; the fingerprint is
/// derived server-side from `pubkey`, never supplied.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollRequest {
    /// The new identity id.
    pub identity_id: String,
    /// The principal it acts as.
    pub principal: String,
    /// `person` / `service` / `operator` (not `channel` — channels are enrolled
    /// at boot, not over HTTP).
    pub kind: String,
    /// Required for `person`, omitted otherwise (DDL coherence CHECK).
    #[serde(default)]
    pub company_slug: Option<String>,
    /// SPKI-DER public key, base64 (standard).
    pub pubkey: String,
}

/// `POST /v1/auth/enroll` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollResponse {
    /// `true` if a new row was inserted, `false` if the identity already existed
    /// (idempotent re-materialisation).
    pub enrolled: bool,
}

/// `POST /v1/auth/enroll` — a GATED route: only the operator principal may
/// enrol, and the resolved [`CallerIdentity`] the middleware attached is what
/// proves it. The enrolment is attributed to that identity.
///
/// It used to have a second arm, taken when no `CallerIdentity` was present,
/// which enrolled anybody under local trust and attributed the act to nobody.
/// That arm existed for the `enroll`-stage rollout, and since nothing in the
/// tree ever set `CHIEFD_AUTH_ENABLED` it was the ONLY arm any deployment took:
/// an unauthenticated caller could write an identity of its choosing into the
/// company's trust anchor. It is deleted with the stage (A6).
///
/// There is no bootstrap problem left to solve by keeping it. The operator's own
/// identity is enrolled from disk at boot by [`super::boot::build_auth_runtime`]
/// — never over HTTP — so the first credential exists before this route can be
/// called at all.
pub async fn enroll(
    Extension(runtime): Extension<Option<Arc<AuthRuntime>>>,
    caller: Option<Extension<CallerIdentity>>,
    Json(request): Json<EnrollRequest>,
) -> Response {
    let Some(runtime) = runtime else {
        return (StatusCode::NOT_IMPLEMENTED, NOT_CONFIGURED).into_response();
    };
    // The `Option` is the extractor's shape, not a policy: this route is
    // non-exempt, so on any surface that has a runtime the middleware has
    // already refused a bearer-less request. Absence here therefore cannot be
    // reached, and it REFUSES rather than falling back to local trust.
    let Some(Extension(CallerIdentity(identity))) = caller else {
        return (StatusCode::UNAUTHORIZED, "caller-unauthenticated").into_response();
    };
    if identity.principal != "operator" {
        return (StatusCode::FORBIDDEN, "enrolment requires operator privilege").into_response();
    }
    let enrolled_by: Option<String> = Some(identity.identity_id);
    let Some(kind) = IdentityKind::parse(&request.kind) else {
        return (StatusCode::BAD_REQUEST, "unknown identity kind").into_response();
    };
    if kind == IdentityKind::Channel {
        return (StatusCode::BAD_REQUEST, "channels are not enrolled over HTTP").into_response();
    }
    match runtime
        .enroll_identity(
            &request.identity_id,
            &request.principal,
            kind,
            request.company_slug.as_deref(),
            &request.pubkey,
            enrolled_by.as_deref(),
        )
        .await
    {
        Ok(enrolled) => (StatusCode::OK, Json(EnrollResponse { enrolled })).into_response(),
        Err(EnrollError::BadPubkey) => {
            (StatusCode::BAD_REQUEST, "pubkey is not a valid P-256 SPKI key").into_response()
        }
        // The id exists with a different key: enrolment never silently re-keys;
        // rotation is a separate deliberate act.
        Err(EnrollError::FingerprintConflict) => (
            StatusCode::CONFLICT,
            "identity already enrolled with a different key (rotation is explicit)",
        )
            .into_response(),
        // A coherence-CHECK violation (e.g. a person with no company slug) is the
        // caller's malformed request, not a server fault.
        Err(EnrollError::Db(_)) => {
            (StatusCode::CONFLICT, "enrolment rejected (constraint)").into_response()
        }
    }
}

/// `POST /v1/auth/token` — verify the signed challenge and mint a JWT.
pub async fn token(
    Extension(runtime): Extension<Option<Arc<AuthRuntime>>>,
    Json(request): Json<TokenRequest>,
) -> Response {
    let Some(runtime) = runtime else {
        return (StatusCode::NOT_IMPLEMENTED, NOT_CONFIGURED).into_response();
    };
    match runtime.redeem(&request.nonce_id, &request.signature).await {
        Ok(token) => (StatusCode::OK, Json(TokenResponse { token })).into_response(),
        // A bad/expired nonce or a rejected signature is an authentication
        // failure (401): the caller did not prove possession of the key.
        Err(RedeemError::BadNonce | RedeemError::BadSignature | RedeemError::NotAKeypair) => {
            (StatusCode::UNAUTHORIZED, "challenge not satisfied").into_response()
        }
        // The signature verified but the identity is not permitted a token
        // (revoked / issuance refused): 403.
        Err(RedeemError::Forbidden | RedeemError::Issue(_)) => {
            (StatusCode::FORBIDDEN, "identity not authorized").into_response()
        }
        // The identity store could not be read, so nothing was decided about
        // this caller. Same rule as the challenge route above and as the
        // middleware: a retryable status, never a verdict.
        Err(RedeemError::Unavailable { reason }) => {
            RouteError::unavailable("identity-store-unavailable", reason).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chiefd_core::actor::CompanyDb;
    use chiefd_core::store::identities::Identity;
    use chiefd_core::store::COMPANY_DB_FILENAME;
    use chiefd_core::test_support::ManualClock;

    fn runtime(dir: &std::path::Path) -> Arc<AuthRuntime> {
        let company = Arc::new(
            CompanyDb::open(
                "acme",
                &dir.join(COMPANY_DB_FILENAME),
                Arc::new(ManualClock::starting_at(0, 1_000)),
            )
            .expect("open company"),
        );
        Arc::new(AuthRuntime::new(
            company,
            Arc::new(b"enroll-unit-secret".to_vec()),
            30_000,
            8,
            Arc::new(|| 1_000),
        ))
    }

    /// A body whose key is deliberately malformed, so a caller that gets PAST
    /// both credential checks stops at the next one and proves the order.
    fn request() -> EnrollRequest {
        EnrollRequest {
            identity_id: "intruder".to_string(),
            principal: "intruder".to_string(),
            kind: "operator".to_string(),
            company_slug: None,
            pubkey: "not-a-key".to_string(),
        }
    }

    fn caller(principal: &str) -> Option<Extension<CallerIdentity>> {
        Some(Extension(CallerIdentity(Identity {
            identity_id: format!("id-{principal}"),
            principal: principal.to_string(),
            kind: IdentityKind::Operator,
            company_slug: None,
            pubkey: Some("spki".to_string()),
            fingerprint: "fp".to_string(),
            active: true,
            enrolled_at: 0,
            enrolled_by: None,
            revoked_at: None,
        })))
    }

    /// A6 — THE SHARPEST ROLLOUT ARM. `enroll` used to accept an absent caller
    /// as local trust and enrol whatever it was handed, attributed to nobody.
    /// Because nothing in the tree set `CHIEFD_AUTH_ENABLED`, that was the only
    /// arm any deployment ever took: WRITING THE COMPANY'S TRUST ANCHOR took no
    /// credential at all, and the record said who did it was nobody. The
    /// middleware refuses a bearer-less request before this handler runs now,
    /// and the handler refuses too, so neither one alone is load-bearing.
    #[tokio::test]
    async fn an_absent_caller_cannot_enrol_an_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response = enroll(Extension(Some(runtime(dir.path()))), None, Json(request())).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// The refusal above is about the CREDENTIAL, not about the principal — a
    /// person who authenticates perfectly still may not write the trust anchor,
    /// and that check has to survive the one above.
    #[tokio::test]
    async fn an_authenticated_non_operator_still_cannot_enrol() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response =
            enroll(Extension(Some(runtime(dir.path()))), caller("quant-head"), Json(request()))
                .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// And the operator gets past both gates, so the two refusals above are not
    /// satisfied by a handler that refuses everybody. It stops at the malformed
    /// key, which is the next check and therefore proves the credential checks
    /// sit in front of it.
    #[tokio::test]
    async fn the_operator_reaches_the_enrolment_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response =
            enroll(Extension(Some(runtime(dir.path()))), caller("operator"), Json(request())).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// A surface with no runtime answers 501 BEFORE any caller check, which is
    /// what keeps `chiefd docstore-only` — which holds no identities at all —
    /// answering exactly the way it always has.
    #[tokio::test]
    async fn a_surface_with_no_runtime_still_answers_not_implemented() {
        let response = enroll(Extension(None), None, Json(request())).await;
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    /// Break the identity store under a live actor — see the twin helper in
    /// `runtime.rs` for why a second connection dropping the table is the real
    /// failure path and a `shutdown` is not.
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

    /// The status and the decoded `{code, detail}` a route answered with.
    async fn decoded(response: Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await.expect("body");
        let parsed = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("a route-error body: {}", String::from_utf8_lossy(&bytes)));
        (status, parsed)
    }

    /// #1204, at the route. The mint path answered `401 unknown or inactive
    /// identity` while the store was merely unreadable — settled-looking, and
    /// false. `chief`'s bearer acquisition classifies a 401 from this route as
    /// permanent and a 5xx as transient, so which of the two it gets decides
    /// whether an actuator rides out a store stall or exits.
    #[tokio::test]
    async fn challenge_answers_503_when_the_identity_store_faults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rt = runtime(dir.path());
        break_the_identity_store(dir.path());

        let response = challenge(
            Extension(Some(rt)),
            Json(ChallengeRequest { identity_id: "person:a".to_owned() }),
        )
        .await;
        let (status, body) = decoded(response).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "identity-store-unavailable");
    }

    /// The token route's half. A caller that satisfied the challenge and hit a
    /// store fault used to be told `403 identity not authorized`.
    #[tokio::test]
    async fn token_answers_503_when_the_identity_store_faults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rt = runtime(dir.path());
        // Enrol and take a real nonce, so the redeem gets PAST the nonce check
        // and reaches the identity read — otherwise this would pin `BadNonce`.
        let key = p256::ecdsa::SigningKey::from_bytes(&[7u8; 32].into()).expect("key");
        let spki = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            p256::pkcs8::EncodePublicKey::to_public_key_der(&p256::ecdsa::VerifyingKey::from(&key))
                .expect("spki")
                .as_bytes(),
        );
        rt.enroll_identity("person:a", "person:a", IdentityKind::Person, Some("acme"), &spki, None)
            .await
            .expect("enrol");
        let issued = rt.challenge("person:a").await.expect("challenge while healthy");
        let signature: p256::ecdsa::Signature = p256::ecdsa::signature::Signer::sign(
            &key,
            &super::super::sig::challenge_message("person:a", &issued.nonce),
        );
        let signature = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature.to_bytes(),
        );

        break_the_identity_store(dir.path());

        let response =
            token(Extension(Some(rt)), Json(TokenRequest { nonce_id: issued.nonce_id, signature }))
                .await;
        let (status, body) = decoded(response).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "identity-store-unavailable");
    }

    /// THE CONTROL, and the reason the two above cannot be satisfied by a
    /// route that answers 503 to everybody: an identity the store WAS read for
    /// and does not hold is still the flat `401` with no existence oracle.
    #[tokio::test]
    async fn a_readable_store_that_holds_nobody_is_still_401() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response = challenge(
            Extension(Some(runtime(dir.path()))),
            Json(ChallengeRequest { identity_id: "ghost".to_owned() }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
