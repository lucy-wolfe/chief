//! The verify-middleware (agent-auth P0, R4): ONE layer over the whole docstore
//! router that resolves the caller's cryptographic identity BEFORE any handler
//! runs.
//!
//! Decision (never localhost — R5): a request is authorized iff it carries a
//! `Authorization: Bearer <jwt>` whose HMAC verifies, whose `sub` is an enrolled
//! ACTIVE identity, and whose `kid` equals that identity's CURRENT fingerprint.
//! Nothing about the connection's origin (127.0.0.1 or a future remote listener)
//! enters the decision.
//!
//! * missing / malformed bearer, or a bad-signature / malformed token -> **401**
//! * good token but the identity is revoked or its key rotated (kid
//!   mismatch) -> **403**
//! * good token and the identity store could NOT BE READ -> **503**
//!   `identity-store-unavailable`
//!
//! The last row is a status and not a verdict, and the difference is the whole
//! of #1204. A 403 says the trust decision was MADE and went against you; a
//! client that believes it is right to stop asking. A store fault means the
//! decision was never made at all, and answering it with a 403 told every
//! caller a permanent-sounding lie: a seven-second stall in one company's
//! identity store killed its resident actuator, and with it the sidebar brain
//! that hosts every rail, for two hours. Fail-closed is unchanged — no handler
//! runs either way — but the sentence the caller reads is now "ask again",
//! which is the one that is true.
//!
//! Authentication is PER-AGENT. A token verifies on identity-active plus
//! key-fingerprint match and NOTHING else: no incarnation binding, no hash
//! binding. That is what makes a live pane un-brickable — a running agent can
//! never be locked out of its own company by state moving underneath it.
//! Revocation is deactivate-identity or rotate-key.
//!
//! Exactly four routes are exempt — the two halves of the pre-auth liveness
//! probe and the two auth endpoints (which mint the very token everything else
//! needs). See [`EXEMPT_PATHS`]. The identity read is one indexed lookup per
//! request (PK), performed through the [`IdentityLookup`] seam so the trust
//! decision never reads through the docstore surface it gates.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header::AUTHORIZATION, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures_util::future::BoxFuture;

use chiefd_core::error::ChiefdError;
use chiefd_core::store::identities::Identity;

use super::jwt;

/// The exact set of routes that run WITHOUT a bearer. Anything not in this set
/// is gated. Adding a row here makes a route serve a caller who has proved
/// nothing, so each one carries its reason:
///
/// * `/v1/docs/health` — pre-auth liveness. `chiefctl daemon ensure` hits it
///   before any token exists.
/// * `/v1/docs/runtime` — the OTHER half of that same probe, and it answers
///   WHAT this listener is (`{mode, company}`) where health answers only
///   whether it is up. `chief-cli`'s `probe_health` asks both, in that order,
///   and treats a listener whose mode it cannot read as unhealthy — so gating
///   this one would make a first boot race its own operator key, which the
///   daemon mints only once boot reaches the auth runtime. Its body is a
///   non-secret process identity: the slug it names is already published,
///   unauthenticated, by beacond's registry.
/// * `/v1/auth/challenge`, `/v1/auth/token` — they mint the token every other
///   route requires, so they cannot require one.
///
/// It is a LIST and not a prefix rule on purpose: `/v1/docs/queue` and
/// `/v1/admin/shutdown` sit beside two of these and are gated, and a prefix
/// would have quietly swept them in.
pub const EXEMPT_PATHS: &[&str] =
    &["/v1/docs/health", "/v1/docs/runtime", "/v1/auth/challenge", "/v1/auth/token"];

/// Whether a path bypasses identity verification. Public so the router-walk
/// drift-guard test can assert the exempt set matches the registered routes.
#[must_use]
pub fn is_exempt(path: &str) -> bool {
    EXEMPT_PATHS.contains(&path)
}

/// Resolve an enrolled identity by id — the seam the middleware reads through so
/// it never touches the docstore. The company-actor implementation performs
/// one indexed `identities` PK lookup; tests use in-memory fakes.
pub trait IdentityLookup: Send + Sync {
    /// The identity for `identity_id`.
    ///
    /// `Ok(None)` means NEVER ENROLLED — the store was read and it holds
    /// nobody by that id. `Err` means the store COULD NOT BE LOOKED IN: a
    /// SQLite fault on the pooled read connection, or a reader pool that is
    /// closed, exhausted or cancelled.
    ///
    /// Both fail closed — neither authorizes anybody and no handler runs on
    /// either — but they are not the same answer, and collapsing them was the
    /// bug this seam now forbids at the type level. The caller renders the
    /// first as a verdict (`403`) and the second as a retryable status
    /// (`503`), because a client cannot distinguish "you are not welcome here"
    /// from "ask me again in a moment" unless the server does it first.
    fn get<'a>(
        &'a self,
        identity_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Identity>, ChiefdError>>;
}

/// State threaded into the middleware: the HMAC secret and the two read seams.
///
/// It used to carry a third member, `gate: bool` — whether EVERY non-exempt
/// route required a bearer — fed from `CHIEFD_AUTH_ENABLED`. It is deleted
/// (A6). A surface that has an `AuthState` requires a bearer on every
/// non-exempt path, and there is no field left that could say otherwise.
#[derive(Clone)]
pub struct AuthState {
    /// The daemon-local HS256 secret (never logged).
    pub secret: Arc<Vec<u8>>,
    /// The identity resolver (company-actor-backed in production).
    pub lookup: Arc<dyn IdentityLookup>,
}

impl AuthState {
    /// Construct from a secret and the identity lookup.
    #[must_use]
    pub fn new(secret: Arc<Vec<u8>>, lookup: Arc<dyn IdentityLookup>) -> Self {
        Self { secret, lookup }
    }
}

fn bearer(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// The `axum::middleware::from_fn_with_state` entry point.
///
/// The state is OPTIONAL, and the two arms are two SURFACES rather than two
/// settings of one surface.
///
/// `Some` is the company surface: `chiefd run` supplies it unconditionally
/// (#751/P7), and every non-exempt path requires a bearer that verifies. There
/// is no 127.0.0.1 bypass (R5) and, since A6, no environment variable, no
/// parameter and no field that softens it.
///
/// `None` is `chiefd docstore-only`, which has no company actor at mount and
/// therefore no identities to resolve at all. It is not enforcement turned off:
/// that mode refuses to start unless its database is inside the OS temporary
/// directory, refuses any bind that is not loopback, and says in its startup
/// log that it is unauthenticated and test-only (always-on-auth ruling 5,
/// #1097). There is no real surface it can be switched onto, which is the whole
/// test an off switch fails. `chiefd`'s `unauthenticated_mounts.rs` pins
/// which mounts reach this arm, by name.
pub async fn require_identity(
    State(state): State<Option<AuthState>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(state) = state else {
        return next.run(request).await;
    };
    if is_exempt(request.uri().path()) {
        return next.run(request).await;
    }

    let Some(token) = bearer(&request) else {
        // ALWAYS. There used to be a second arm here, taken whenever the
        // universal gate was off, that ran the handler with no identity at all
        // — and since nothing in the tree ever set `CHIEFD_AUTH_ENABLED`, that
        // arm was the one every company daemon took. It is deleted (A6).
        return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
    };
    let Ok(claims) = jwt::verify(&state.secret, token) else {
        return (StatusCode::UNAUTHORIZED, "invalid bearer token").into_response();
    };
    // THE FAULT ARM COMES FIRST, and it is not a verdict. The store could not
    // be read, so nothing was decided — answering `403 unknown identity` here
    // told a client with a correct retry ladder that its identity was gone,
    // and one seven-second stall cost a company its actuator for two hours.
    // Still fail-closed: `next` is not called.
    let identity = match state.lookup.get(&claims.sub).await {
        Ok(found) => found,
        Err(fault) => {
            return crate::docstore::route_error::RouteError::unavailable(
                "identity-store-unavailable",
                format!("the identity store could not be read: {fault}"),
            )
            .into_response();
        }
    };
    let Some(identity) = identity else {
        return (StatusCode::FORBIDDEN, "unknown identity").into_response();
    };
    if !identity.active {
        return (StatusCode::FORBIDDEN, "revoked identity").into_response();
    }
    if identity.fingerprint != claims.kid {
        // The key was rotated (or the channel epoch bumped) after this token was
        // minted: the anchor moved, so the token is dead even though its HMAC is
        // intact.
        return (StatusCode::FORBIDDEN, "stale credential (key rotated)").into_response();
    }
    // Hand the resolved identity to downstream handlers (e.g. the enroll route's
    // operator-privilege check). Present whenever verification ran and passed,
    // which on this surface is every request that got this far.
    let mut request = request;
    request.extensions_mut().insert(CallerIdentity(identity));
    next.run(request).await
}

/// The cryptographically-resolved caller, inserted into request extensions by
/// [`require_identity`] after a token verifies.
///
/// On the company surface it is ALWAYS present on a non-exempt route: the
/// request that carries no credential is refused before any handler runs
/// (A6 — the rollout stage that made absence mean "local trust" is deleted).
/// It is absent only on `chiefd docstore-only`, the temp-path, loopback-only,
/// identity-less test surface described on [`require_identity`].
#[derive(Clone)]
pub struct CallerIdentity(pub Identity);

/// The verified caller. **A non-exempt route cannot run without one**, and this
/// extractor is how a handler says so in its own signature rather than in a
/// comment.
///
/// # Why an extractor and not `Extension<CallerIdentity>`
///
/// The built-in answers **500** when the extension is missing, which turns an
/// authentication failure into a fault: the caller is told chiefd broke when in
/// fact chiefd declined. This refuses `401 caller-unauthenticated` — the same
/// status, code shape and body `require_self_identity` and `reminder_actor`
/// already return — so no client learns a new refusal from this change.
///
/// # When it can actually fire
///
/// On the company surface, never: [`require_identity`] refuses a bearer-less
/// request before any handler runs (A6). It fires only on `chiefd
/// docstore-only`, which mounts with no auth runtime at all by ruling (A5,
/// #1097) and is fenced to a temp-path database and a loopback bind. Since
/// #1122 the org contract suites no longer drive `/v1/org/*` there, so a 401
/// on that surface is the correct answer for a surface that can authenticate
/// nobody.
///
/// The deliverable is the TYPE, not the runtime check: with this in the
/// signature no handler can spell the absent case, so no future handler can
/// quietly decide that absence means local trust.
pub struct Caller(pub Identity);

impl<S> axum::extract::FromRequestParts<S> for Caller
where
    S: Send + Sync,
{
    type Rejection = crate::docstore::route_error::RouteError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<CallerIdentity>().map(|c| Self(c.0.clone())).ok_or_else(|| {
            crate::docstore::route_error::RouteError::unauthenticated(
                "caller-unauthenticated",
                "this route needs a caller authenticated by its enrolled identity key; present \
                 a bearer token from POST /v1/auth/token",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use chiefd_core::store::identities::IdentityKind;
    use tower::ServiceExt;

    use crate::authn::{issue_token_for, jwt::Claims};

    struct FakeLookup(HashMap<String, Identity>);
    impl IdentityLookup for FakeLookup {
        fn get<'a>(
            &'a self,
            identity_id: &'a str,
        ) -> BoxFuture<'a, Result<Option<Identity>, ChiefdError>> {
            Box::pin(async move { Ok(self.0.get(identity_id).cloned()) })
        }
    }

    /// A store that cannot be read at all. It carries the fault it would
    /// produce, because the two shapes the REAL read can return —
    /// `StoreFailure` from the pooled SQLite connection and `Unavailable` from
    /// the reader pool — must both come out as one retryable status rather
    /// than as a verdict about the caller.
    /// It holds a CONSTRUCTOR rather than a value because `ChiefdError` is
    /// deliberately not `Clone` — the taxonomy is a thing you produce once, at
    /// the site that failed.
    struct FailingLookup(fn() -> ChiefdError);
    impl IdentityLookup for FailingLookup {
        fn get<'a>(
            &'a self,
            _identity_id: &'a str,
        ) -> BoxFuture<'a, Result<Option<Identity>, ChiefdError>> {
            let fault = (self.0)();
            Box::pin(async move { Err(fault) })
        }
    }

    /// The status and the decoded `{code, detail}` of one call.
    async fn answer(
        app: Router,
        method: &str,
        path: &str,
        bearer: Option<&str>,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = builder.body(Body::empty()).expect("request");
        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await.expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn identity(id: &str, active: bool, fingerprint: &str) -> Identity {
        Identity {
            identity_id: id.to_string(),
            principal: id.to_string(),
            kind: IdentityKind::Operator,
            company_slug: None,
            pubkey: Some("spki".to_string()),
            fingerprint: fingerprint.to_string(),
            active,
            enrolled_at: 0,
            enrolled_by: None,
            revoked_at: None,
        }
    }

    fn app(state: AuthState) -> Router {
        Router::new()
            .route("/v1/docs/health", get(|| async { "ok" }))
            .route("/v1/auth/challenge", axum::routing::post(|| async { "challenge" }))
            .route("/v1/docs/read", axum::routing::post(|| async { "secret-data" }))
            .layer(axum::middleware::from_fn_with_state(Some(state.clone()), require_identity))
            .with_state(())
    }

    fn state_with(identities: Vec<Identity>) -> (AuthState, Vec<u8>) {
        let secret = b"unit-test-daemon-secret".to_vec();
        let map = identities.into_iter().map(|i| (i.identity_id.clone(), i)).collect();
        (AuthState::new(Arc::new(secret.clone()), Arc::new(FakeLookup(map))), secret)
    }

    fn person(id: &str, fingerprint: &str) -> Identity {
        Identity {
            kind: IdentityKind::Person,
            company_slug: Some("cobalt".to_string()),
            ..identity(id, true, fingerprint)
        }
    }

    async fn status(app: Router, method: &str, path: &str, bearer: Option<&str>) -> StatusCode {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = builder.body(Body::empty()).expect("request");
        app.oneshot(request).await.expect("response").status()
    }

    #[tokio::test]
    async fn a_valid_token_reaches_the_handler() {
        let (state, secret) = state_with(vec![identity("op", true, "fp-1")]);
        let token = issue_token_for(&secret, &identity("op", true, "fp-1"), 0).expect("mint");
        assert_eq!(status(app(state), "POST", "/v1/docs/read", Some(&token)).await, StatusCode::OK,);
    }

    #[tokio::test]
    async fn a_missing_bearer_is_401_on_a_gated_route() {
        let (state, _) = state_with(vec![identity("op", true, "fp-1")]);
        assert_eq!(
            status(app(state), "POST", "/v1/docs/read", None).await,
            StatusCode::UNAUTHORIZED,
        );
    }

    #[tokio::test]
    async fn a_forged_token_is_401() {
        let (state, _) = state_with(vec![identity("op", true, "fp-1")]);
        let forged =
            issue_token_for(b"WRONG-secret", &identity("op", true, "fp-1"), 0).expect("mint");
        assert_eq!(
            status(app(state), "POST", "/v1/docs/read", Some(&forged)).await,
            StatusCode::UNAUTHORIZED,
        );
    }

    #[tokio::test]
    async fn a_revoked_identity_is_403() {
        // Token minted while active; identity now revoked in the lookup.
        let (state, secret) = state_with(vec![identity("op", false, "fp-1")]);
        let token = issue_token_for(&secret, &identity("op", true, "fp-1"), 0).expect("mint");
        assert_eq!(
            status(app(state), "POST", "/v1/docs/read", Some(&token)).await,
            StatusCode::FORBIDDEN,
        );
    }

    #[tokio::test]
    async fn a_rotated_key_makes_the_old_token_403() {
        // Lookup now has fp-2; the token carries kid fp-1.
        let (state, secret) = state_with(vec![identity("op", true, "fp-2")]);
        let old_token = issue_token_for(&secret, &identity("op", true, "fp-1"), 0).expect("mint");
        assert_eq!(
            status(app(state), "POST", "/v1/docs/read", Some(&old_token)).await,
            StatusCode::FORBIDDEN,
        );
    }

    #[tokio::test]
    async fn an_unknown_identity_is_403() {
        let (state, secret) = state_with(vec![]);
        let token = issue_token_for(&secret, &identity("ghost", true, "fp-1"), 0).expect("mint");
        assert_eq!(
            status(app(state), "POST", "/v1/docs/read", Some(&token)).await,
            StatusCode::FORBIDDEN,
        );
    }

    /// #1204 — THE TEST THAT USED TO PIN THE BUG. Its previous name was
    /// `a_failed_identity_lookup_is_fail_closed_with_403`, and both halves of
    /// that name were doing work: fail-closed was right and is unchanged, and
    /// the `403` was the defect, asserted as though it were the contract.
    ///
    /// A 403 is a VERDICT — the store was read, and the answer is no. A store
    /// that could not be read has produced no verdict at all, and saying
    /// otherwise is a lie a correct client acts on: `chief actuate` treats a
    /// 4xx as terminal (rightly), so a seven-second stall in one company's
    /// identity store exited its actuator, and with it the sidebar brain, and
    /// the company sat un-converged for two hours.
    ///
    /// So: still no handler, still no authorization — and a status that says
    /// "ask again", which is the true one.
    #[tokio::test]
    async fn a_failed_identity_lookup_is_fail_closed_with_503_not_a_verdict() {
        let secret = b"unit-test-daemon-secret".to_vec();
        let token = issue_token_for(&secret, &identity("op", true, "fp-1"), 0).expect("mint");
        let state = AuthState::new(
            Arc::new(secret),
            Arc::new(FailingLookup(|| ChiefdError::StoreFailure {
                store: "auth-identities",
                cause: "database is locked (SQLITE_BUSY)".to_owned(),
            })),
        );
        let (status, body) = answer(app(state), "POST", "/v1/docs/read", Some(&token)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("a route-error body");
        assert_eq!(parsed["code"], "identity-store-unavailable");
        let detail = parsed["detail"].as_str().expect("detail");
        assert!(detail.contains("auth-identities"), "the cause names the store: {detail}");
        assert!(
            !body.contains("secret-data"),
            "FAIL CLOSED: the handler must not have run, whatever the status says: {body}"
        );
    }

    /// The other half of the split, and the reason both tests exist side by
    /// side: somebody "simplifying" these two arms into one status — in EITHER
    /// direction — breaks one of them. A store that was read and holds nobody
    /// is a settled answer, and a client that retried it would ask the
    /// identical question forever.
    #[tokio::test]
    async fn a_never_enrolled_identity_is_still_a_403_beside_the_503() {
        let (state, secret) = state_with(vec![]);
        let token = issue_token_for(&secret, &identity("ghost", true, "fp-1"), 0).expect("mint");
        let (status, body) = answer(app(state), "POST", "/v1/docs/read", Some(&token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body, "unknown identity");
    }

    /// The reader pool is the OTHER shape `identity_read` can fault with —
    /// closed, exhausted or cancelled — and it reaches the middleware by the
    /// same path as a SQLite error. Both are transient by nature and both must
    /// answer the retryable status; covering only one would leave half the
    /// real failure mode pinned by nothing.
    #[tokio::test]
    async fn a_pool_closed_fault_is_also_503() {
        let secret = b"unit-test-daemon-secret".to_vec();
        let token = issue_token_for(&secret, &identity("op", true, "fp-1"), 0).expect("mint");
        let state = AuthState::new(
            Arc::new(secret),
            Arc::new(FailingLookup(|| ChiefdError::Unavailable { reason: "reader-pool-closed" })),
        );
        let (status, body) = answer(app(state), "POST", "/v1/docs/read", Some(&token)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("a route-error body");
        assert_eq!(parsed["code"], "identity-store-unavailable");
        assert!(!body.contains("secret-data"), "fail closed on this shape too");
    }

    #[tokio::test]
    async fn exempt_routes_bypass_verification() {
        let (state, _) = state_with(vec![]);
        assert_eq!(
            status(app(state.clone()), "GET", "/v1/docs/health", None).await,
            StatusCode::OK
        );
        assert_eq!(status(app(state), "POST", "/v1/auth/challenge", None).await, StatusCode::OK,);
    }

    /// Auth is PER-AGENT: a person's token keeps working for as long as the
    /// identity is active and its key unrotated. Nothing about a respawn, a
    /// fresh session, or any other state moving underneath a live pane may
    /// retire its credential — that is exactly the brick this deletion removes.
    #[tokio::test]
    async fn a_persons_token_survives_anything_but_deactivation_and_rotation() {
        let (state, secret) = state_with(vec![person("quant-head", "fp-1")]);
        let token = issue_token_for(&secret, &person("quant-head", "fp-1"), 0).expect("mint");
        assert_eq!(status(app(state), "POST", "/v1/docs/read", Some(&token)).await, StatusCode::OK,);

        // Rotating the key IS revocation, and still is.
        let (rotated, _) = state_with(vec![person("quant-head", "fp-2")]);
        assert_eq!(
            status(app(rotated), "POST", "/v1/docs/read", Some(&token)).await,
            StatusCode::FORBIDDEN,
        );

        // Deactivating the identity IS revocation, and still is.
        let mut inactive = person("quant-head", "fp-1");
        inactive.active = false;
        let (deactivated, _) = state_with(vec![inactive]);
        assert_eq!(
            status(app(deactivated), "POST", "/v1/docs/read", Some(&token)).await,
            StatusCode::FORBIDDEN,
        );
    }

    /// A3. The resident actuator authenticates as a SERVICE, and every route it
    /// calls is a read. The rule those reads follow is "is there a valid
    /// credential", full stop — and specifically NOT "which person is this".
    ///
    /// The trap this pins is one step past the middleware and is the reason the
    /// test lives here: a person-deriving helper answers `None` for a Service,
    /// so a read armed that way would authenticate the actuator perfectly and
    /// then refuse it. The middleware's own decision reads `active` and the
    /// fingerprint and asks nothing about kind, and that is what a service
    /// credential depends on.
    #[tokio::test]
    async fn a_service_credential_is_as_good_as_any_other_on_a_read() {
        let service = Identity {
            kind: IdentityKind::Service,
            // Daemon-scoped: no company, and no person behind it to resolve.
            company_slug: None,
            ..identity("service", true, "fp-svc")
        };
        let (state, secret) = state_with(vec![service.clone()]);
        let token = issue_token_for(&secret, &service, 0).expect("mint");
        assert_eq!(
            status(app(state), "POST", "/v1/docs/read", Some(&token)).await,
            StatusCode::OK,
            "a Service bearer must reach the handler"
        );

        // And it is revoked on the same anchor as every other identity — the
        // separate principal is separate for the audit trail, never a weaker
        // credential.
        let mut revoked = service.clone();
        revoked.active = false;
        let (state, secret) = state_with(vec![revoked]);
        let token = issue_token_for(&secret, &service, 0).expect("mint");
        assert_eq!(
            status(app(state), "POST", "/v1/docs/read", Some(&token)).await,
            StatusCode::FORBIDDEN,
        );
    }

    /// A6 — THE RULE THIS PACKET EXISTS FOR, and it is the exact inverse of the
    /// test it replaces. That one asserted a bearer-less request was SERVED
    /// when the universal gate was off, and because nothing in the tree ever
    /// set `CHIEFD_AUTH_ENABLED` it was describing what every real company
    /// daemon did.
    ///
    /// It is written against the CONSTRUCTOR rather than against a value:
    /// `AuthState::new` takes a secret and a lookup and nothing else, so there
    /// is no argument a caller could pass that would make the first assertion
    /// below answer anything but 401.
    #[tokio::test]
    async fn a_bearer_less_request_is_refused_and_no_state_can_say_otherwise() {
        let (state, secret) = state_with(vec![identity("op", true, "fp-1")]);

        assert_eq!(
            status(app(state.clone()), "POST", "/v1/docs/read", None).await,
            StatusCode::UNAUTHORIZED,
            "no credential is 401 on every non-exempt path, with no stage left to change it"
        );

        let forged =
            issue_token_for(b"WRONG-secret", &identity("op", true, "fp-1"), 0).expect("mint");
        assert_eq!(
            status(app(state.clone()), "POST", "/v1/docs/read", Some(&forged)).await,
            StatusCode::UNAUTHORIZED,
            "a presented-but-forged credential is refused"
        );

        let good = issue_token_for(&secret, &identity("op", true, "fp-1"), 0).expect("mint");
        assert_eq!(status(app(state), "POST", "/v1/docs/read", Some(&good)).await, StatusCode::OK,);
    }

    /// A6, acceptance criterion 2 as AMENDED (#1115: three → four). This is
    /// the textual half and it is deliberately not the whole guard: a constant
    /// can agree with itself while a route registered after the `.layer()` call
    /// is effectively exempt, which is exactly what three routes were until
    /// #1115. `serve_bound_composes_every_route_inside_the_gate` in
    /// `docstore::mod` is the behavioural half — it walks the composed router
    /// and asserts the set that answers WITHOUT a credential is exactly this
    /// constant. Both must be edited together, on purpose.
    #[test]
    fn the_exempt_set_is_exactly_four_and_a_fifth_is_a_deliberate_act() {
        assert_eq!(
            EXEMPT_PATHS,
            ["/v1/docs/health", "/v1/docs/runtime", "/v1/auth/challenge", "/v1/auth/token"],
            "a fifth exempt path serves a request with no verified identity. If that is \
             intended, change this assertion in the same commit, give the entry its reason on \
             the constant, and record the amendment in DECISIONS.md",
        );
        for path in EXEMPT_PATHS {
            assert!(is_exempt(path));
        }
        assert!(!is_exempt("/v1/docs/read"));
        assert!(!is_exempt("/v1/docs/queue"), "a sibling of two exempt paths, and gated");
        assert!(!is_exempt("/v1/admin/shutdown"), "shuts the daemon down; never exempt");
        assert!(!is_exempt("/v1/org/person/offboard"));
    }

    #[test]
    fn a_manually_forged_claims_still_needs_the_secret() {
        // Sanity: a hand-built Claims cannot be turned into a token without the
        // secret, so there is no bypass by constructing claims directly.
        let claims = Claims { sub: "op".into(), iat: 0, kid: "fp-1".into(), scope: "all".into() };
        assert!(jwt::mint(b"attacker-guess", &claims).is_ok());
        // ...but it verifies only under the same secret (covered by jwt tests).
    }
}
