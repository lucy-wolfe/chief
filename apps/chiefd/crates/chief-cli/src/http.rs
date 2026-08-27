//! One bounded JSON transport for every lifecycle HTTP call.
//!
//! Ported from the transport half of `chiefd-process.ts` (its `fetch`
//! health/shutdown probes) and `foundation/beacond-ensure.ts`'s probe.
//!
//! Deliberately hyper, not `reqwest` and never a shelled-out `curl` — the same
//! reasoning `beacon.rs` records: the crate is already in chiefd's production
//! graph, and a `curl` subprocess is a thread that cannot be cancelled.
//!
//! Every request carries an explicit budget and there is no retry ladder
//! anywhere in this module: one request, one answer, one caller-visible
//! outcome. Retrying is the caller's decision, made against its own deadline.
//!
//! # The one exception to "no retry", and why it is not a ladder
//!
//! A client built by [`Client::operator`] retries EXACTLY ONCE, on EXACTLY
//! `401`, after dropping its cached bearer. That is not patience about a slow
//! daemon — it is the recovery for a fact the daemon publishes about itself:
//! its HS256 signing secret is ephemeral unless a secret file was provisioned,
//! so a restart rotates it and invalidates every token in flight at once. The
//! pane side settled on the same single retry (`FetchTransport.send`) for the
//! same reason, and it is bounded to one so a genuinely unauthorized identity
//! fails fast instead of looping against the challenge endpoint.
//!
//! # Which clients carry a credential
//!
//! [`Client::operator`] for a COMPANY DAEMON. [`Client::new`] for beacond and
//! for `chief host` — two HTTP surfaces with no auth runtime at all, where a
//! challenge would 404 on every call.

use std::sync::Arc;
use std::time::Duration;

use chief_cli::bearer::{Bearer, JsonPost};
use chiefd_log::{duration_ms as budget_ms, elapsed_ms};
use http_body_util::BodyExt as _;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;

/// The shared hyper client type: HTTP/1.1 over loopback TCP with a full body.
type Inner = HyperClient<HttpConnector, http_body_util::Full<hyper::body::Bytes>>;

/// One answer: the status and the raw body text.
///
/// The body is text, not a parsed value, because several callers need to quote
/// it verbatim into an operator-facing refusal.
#[derive(Debug, Clone)]
pub(crate) struct Answer {
    /// The HTTP status.
    pub(crate) status: u16,
    /// The response body as text.
    pub(crate) body: String,
}

impl Answer {
    /// Parse the body as JSON, or `None` when it is absent or malformed.
    ///
    /// Malformed is deliberately not an error: a live endpoint that answered
    /// with nonsense is still a live endpoint, and that distinction is the
    /// whole point of the health probe's three-way result.
    #[must_use]
    pub(crate) fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.body).ok()
    }
}

/// Why a request did not produce an [`Answer`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum HttpError {
    /// Nothing answered: connection refused, aborted, or the budget elapsed.
    #[error("could not reach {url}: {reason}")]
    Transport {
        /// The URL that did not answer.
        url: String,
        /// What the transport reported.
        reason: String,
    },
    /// The request was NOT sent, because the credential it would have carried
    /// must not be used.
    ///
    /// Distinct from [`HttpError::Transport`] on purpose: nothing was dialled,
    /// so "could not reach" would send the reader to the network. The reason
    /// carries the key path, the mode found, and the `chmod` that fixes it.
    #[error("{url} was not called: {reason}")]
    Credential {
        /// The daemon that was about to be called.
        url: String,
        /// The credential refusal, stating the way through.
        reason: String,
    },
}

/// How long the two token-minting round trips may take.
///
/// Deliberately its own number and deliberately SHORTER than a route budget:
/// acquisition is two small loopback POSTs against a daemon that has already
/// answered, and it sits IN FRONT of the caller's own budget. A generous value
/// here would spend the caller's patience before its request was ever sent.
const AUTH_BUDGET: Duration = Duration::from_secs(10);

/// A bounded JSON client for one base URL.
#[derive(Clone)]
pub(crate) struct Client {
    inner: Inner,
    /// The operator credential, when this client speaks to a company daemon.
    ///
    /// `None` for beacond and for `chief host`: neither surface has an auth
    /// runtime, so a challenge there is a 404 on every single call.
    bearer: Option<Arc<Bearer>>,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// Build a client that presents NO credential. Cheap: hyper pools
    /// connections internally.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self { inner: HyperClient::builder(TokioExecutor::new()).build_http(), bearer: None }
    }

    /// Build a client that authenticates as the operator of the company in
    /// `dir`.
    ///
    /// # There is no "no company yet" case, and that is the whole change
    ///
    /// This used to resolve `$HOME`, derive a fleet-wide data root from it, and
    /// fall back to an UNAUTHENTICATED client with a warning when `HOME` was
    /// unset — an off switch reachable by an environment nobody set. The key is
    /// `<dir>/.chief/keys/operator.key` now, and `dir` is the directory the
    /// command was run in, so every caller has one: `chief` and genesis
    /// included, which run before the company they are creating exists.
    ///
    /// The KEY is still not read here. The daemon mints it at boot, so a
    /// pre-genesis client legitimately holds a path to a file that is not there
    /// yet; the file is opened on the first request that needs a header, and an
    /// absent one sends the request out token-less to be judged by the daemon
    /// that owns the question (see [`Bearer::authorization`]'s three outcomes).
    #[must_use]
    pub(crate) fn operator(dir: &std::path::Path) -> Self {
        Self {
            inner: HyperClient::builder(TokioExecutor::new()).build_http(),
            bearer: Some(Arc::new(Bearer::operator(&super::paths::keys_dir(dir)))),
        }
    }

    /// `GET url` within `budget`.
    ///
    /// # Errors
    /// [`HttpError::Transport`] when nothing answers inside the budget, and
    /// [`HttpError::Credential`] when this client holds a private key others
    /// can read — that request is never sent.
    pub(crate) async fn get(&self, url: &str, budget: Duration) -> Result<Answer, HttpError> {
        self.send("GET", url, None, budget).await
    }

    /// `POST url` with a JSON body within `budget`.
    ///
    /// # Errors
    /// [`HttpError::Transport`] when nothing answers inside the budget, and
    /// [`HttpError::Credential`] when this client holds a private key others
    /// can read — that request is never sent.
    pub(crate) async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
        budget: Duration,
    ) -> Result<Answer, HttpError> {
        self.send("POST", url, Some(body.to_string()), budget).await
    }

    /// Attach this client's credential, and re-acquire ONCE on a `401`.
    ///
    /// A client with no credential, or a URL with no resolvable origin, sends
    /// exactly what it always sent. The second attempt happens only when the
    /// first actually CARRIED a header: a 401 answered to a token-less request
    /// is the daemon's correct verdict, not a stale token, and re-acquiring
    /// against it would spend a round trip to be refused identically.
    async fn send(
        &self,
        method: &str,
        url: &str,
        body: Option<String>,
        budget: Duration,
    ) -> Result<Answer, HttpError> {
        let (Some(bearer), Some(origin)) = (self.bearer.as_ref(), origin_of(url)) else {
            return self.send_logged(method, url, body, budget, None).await;
        };
        let header = self.authorization(bearer, &origin).await?;
        let answer = self.send_logged(method, url, body.clone(), budget, header.clone()).await?;
        if answer.status != 401 || header.is_none() {
            return Ok(answer);
        }
        // The daemon restarted and rotated its ephemeral HS256 secret, the key
        // rotated, or the token simply expired. All three look identical from
        // here and all three are fixed the same way, so no attempt is made to
        // tell them apart.
        tracing::info!(
            event = "http.bearer.reacquire",
            url,
            identity = bearer.identity_id(),
            "chiefd refused the cached bearer; acquiring a fresh one and retrying once"
        );
        bearer.invalidate(&origin);
        let renewed = self.authorization(bearer, &origin).await?;
        self.send_logged(method, url, body, budget, renewed).await
    }

    /// This client's `Authorization` value for one daemon.
    ///
    /// Three outcomes, and the split is the packet's one deliberate policy:
    ///
    /// * `Ok(Some(header))` — acquired.
    /// * `Err` — the key exists and MUST NOT be used, because anyone but its
    ///   owner can read it. That is a local, precise, actionable refusal, and
    ///   it is the same rule A1 gave the daemon (ruling 1: "a key that became
    ///   `0644` after the fact must stop the daemon, not warn it"). Letting the
    ///   request go out anyway would make `chmod g+r` a silent downgrade to
    ///   anonymous — a fifth off switch, in the packet that deletes the other
    ///   four — and once the universal gate lands, the only thing the operator
    ///   would see is a `401 missing bearer token` that names neither the file
    ///   nor the mode.
    /// * `Ok(None)` — everything else: no key on this box yet (the daemon mints
    ///   it at boot, so `chief` legitimately runs before one exists), a
    ///   challenge the daemon refused, an unreachable auth route. Each of those
    ///   is a condition the DAEMON is the authority on and answers precisely,
    ///   so the request goes out token-less and is judged there.
    async fn authorization(
        &self,
        bearer: &Bearer,
        origin: &str,
    ) -> Result<Option<String>, HttpError> {
        match bearer.authorization(self, origin).await {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.is_key_hygiene_refusal() => {
                tracing::error!(
                    event = "http.bearer.refused",
                    origin,
                    identity = bearer.identity_id(),
                    key = %bearer.key_path().display(),
                    reason = %error,
                    "chief refuses to use a private key others can read"
                );
                Err(HttpError::Credential { url: origin.to_string(), reason: error.to_string() })
            }
            Err(error) => {
                tracing::warn!(
                    event = "http.bearer.unavailable",
                    origin,
                    identity = bearer.identity_id(),
                    key = %bearer.key_path().display(),
                    reason = %error,
                    "chief could not present an operator identity; the request goes out unauthenticated"
                );
                Ok(None)
            }
        }
    }

    /// Every lifecycle HTTP call this binary makes passes through here, so this
    /// is the one place that has to be instrumented for all of them.
    ///
    /// A launch is mostly HTTP: beacond lookups, the company daemon's health
    /// probe, genesis. Timing each request individually is
    /// what turns "the launch took 4½ minutes" into "the wait was N calls to
    /// one endpoint, each answering in M ms" — a distinction that used to
    /// require `ss` on a live box.
    async fn send_logged(
        &self,
        method: &str,
        url: &str,
        body: Option<String>,
        budget: Duration,
        authorization: Option<String>,
    ) -> Result<Answer, HttpError> {
        let started = std::time::Instant::now();
        let outcome = self.send_once(method, url, body, budget, authorization).await;
        // The URL and never the BODY: a request body carries a company spec, a
        // provider name and — on the genesis route — the operator's own words.
        // The path is what an incident needs; the payload is not.
        match &outcome {
            Ok(answer) => tracing::info!(
                event = "http.request",
                method,
                url,
                status = answer.status,
                elapsed_ms = elapsed_ms(started),
                "chief HTTP request answered"
            ),
            Err(error) => tracing::warn!(
                event = "http.request.failed",
                method,
                url,
                elapsed_ms = elapsed_ms(started),
                budget_ms = budget_ms(budget),
                reason = %error,
                "chief HTTP request did not answer"
            ),
        }
        outcome
    }

    async fn send_once(
        &self,
        method: &str,
        url: &str,
        body: Option<String>,
        budget: Duration,
        authorization: Option<String>,
    ) -> Result<Answer, HttpError> {
        let transport = |reason: String| HttpError::Transport { url: url.to_string(), reason };
        let mut builder = hyper::Request::builder()
            .method(method)
            .uri(url)
            // One request per connection: an operator command is short-lived
            // and must never leave a half-open socket against a daemon it is
            // about to ask to exit.
            .header("connection", "close");
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        // The header VALUE is never logged anywhere in this module: a bearer is
        // a credential, and `send_logged` deliberately records only the method,
        // the URL and the status.
        if let Some(value) = authorization {
            builder = builder.header("authorization", value);
        }
        let payload = http_body_util::Full::new(hyper::body::Bytes::from(body.unwrap_or_default()));
        let request = builder.body(payload).map_err(|error| transport(error.to_string()))?;
        let response = tokio::time::timeout(budget, self.inner.request(request))
            .await
            .map_err(|_elapsed| transport(format!("no response within {budget:?}")))?
            .map_err(|error| transport(error.to_string()))?;
        let status = response.status().as_u16();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|error| transport(error.to_string()))?
            .to_bytes();
        Ok(Answer { status, body: String::from_utf8_lossy(&bytes).into_owned() })
    }
}

/// The two token-minting round trips, which must carry NO credential.
///
/// They go through [`Client::send_logged`] and never [`Client::send`]: sending
/// them through the authenticated path would call back into acquisition to get
/// a header, which recurses forever. `/v1/auth/challenge` and `/v1/auth/token`
/// are two of the three paths the verify-middleware exempts, precisely so this
/// is possible.
impl JsonPost for Client {
    async fn post_json_unauthenticated(
        &self,
        url: String,
        body: serde_json::Value,
    ) -> Result<(u16, String), String> {
        match self.send_logged("POST", &url, Some(body.to_string()), AUTH_BUDGET, None).await {
            Ok(answer) => Ok((answer.status, answer.body)),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// `scheme://authority` of a request URL — the key a bearer is cached under,
/// and the base the two auth routes hang off.
///
/// A token is minted BY one company's daemon and is only good there, so the
/// cache is keyed by the daemon and never by the process. `None` for anything
/// that is not an absolute URL, which makes the caller send the request
/// unauthenticated rather than guess at an origin.
fn origin_of(url: &str) -> Option<String> {
    let uri: hyper::Uri = url.parse().ok()?;
    let scheme = uri.scheme_str()?;
    let authority = uri.authority()?;
    Some(format!("{scheme}://{authority}"))
}

/// Strip any trailing slashes so `{base}{path}` is always well formed.
#[must_use]
pub(crate) fn base(url: &str) -> &str {
    url.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    // The fixtures stage a real operator key in a tempdir. Production
    // filesystem effects belong to `chiefd_host`; staging a tempdir fixture is
    // the sanctioned test use, same allow every sibling test carries.
    #![allow(clippy::disallowed_methods)]
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{base, origin_of, Answer, Client};

    #[test]
    fn an_origin_is_the_scheme_and_authority_of_any_route_url() {
        // The bearer cache is keyed by DAEMON, and every request URL carries a
        // path. A key that included the path would mint one token per route.
        assert_eq!(
            origin_of("http://127.0.0.1:8791/v1/org/roster/desired").as_deref(),
            Some("http://127.0.0.1:8791")
        );
        assert_eq!(
            origin_of("http://127.0.0.1:8791/v1/auth/token").as_deref(),
            Some("http://127.0.0.1:8791")
        );
        // Two daemons are two origins and therefore two tokens: a token is
        // minted by one company's daemon and is only good there.
        assert_ne!(
            origin_of("http://127.0.0.1:8791/v1/docs/health"),
            origin_of("http://127.0.0.1:8792/v1/docs/health")
        );
        // Not an absolute URL: send it unauthenticated rather than guess.
        assert_eq!(origin_of("/v1/docs/health"), None);
    }

    /// A staged 0600 operator key in `<dir>/.chief/keys`, the way this
    /// directory's own daemon mints it at boot.
    fn staged_operator_key(dir: &Path) {
        use p256::pkcs8::{EncodePrivateKey as _, LineEnding};

        let keys = super::super::paths::keys_dir(dir);
        std::fs::create_dir_all(&keys).expect("keys dir");
        let secret = p256::SecretKey::from_slice(&[7u8; 32]).expect("scalar");
        let path = identity_keys::operator_key_path(&keys);
        std::fs::write(&path, secret.to_pkcs8_pem(LineEnding::LF).expect("pem").as_bytes())
            .expect("write key");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    /// What one stub daemon observed, so a test can assert on the WIRE rather
    /// than on this client's own belief about what it sent.
    #[derive(Default)]
    struct Observed {
        /// The `authorization` header on each non-auth request, in order.
        headers: std::sync::Mutex<Vec<Option<String>>>,
        /// How many tokens were minted.
        minted: AtomicUsize,
        /// How many times the protected route must answer 401 before it stops.
        refusals: AtomicUsize,
    }

    /// A daemon that mints a fresh token per acquisition and records the
    /// `authorization` header of every protected request.
    ///
    /// The SIGNATURE is not verified here — `bearer.rs` pins that against a
    /// real P-256 verifier. What this stub is for is the header: whether it
    /// arrives, on which requests, and how many times a 401 is retried.
    async fn stub_daemon(observed: std::sync::Arc<Observed>) -> String {
        use axum::extract::State;
        use axum::http::HeaderMap;
        use axum::routing::post;

        let app = axum::Router::new()
            .route(
                "/v1/auth/challenge",
                post(|| async { axum::Json(serde_json::json!({"nonceId":"n","nonce":"abc"})) }),
            )
            .route(
                "/v1/auth/token",
                post(|State(state): State<std::sync::Arc<Observed>>| async move {
                    let minted = state.minted.fetch_add(1, Ordering::SeqCst) + 1;
                    axum::Json(serde_json::json!({ "token": format!("jwt-{minted}") }))
                }),
            )
            .route(
                "/v1/org/roster/desired",
                post(
                    |State(state): State<std::sync::Arc<Observed>>, headers: HeaderMap| async move {
                        let seen = headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        if let Ok(mut recorded) = state.headers.lock() {
                            recorded.push(seen);
                        }
                        if state.refusals.load(Ordering::SeqCst) > 0 {
                            state.refusals.fetch_sub(1, Ordering::SeqCst);
                            return (axum::http::StatusCode::UNAUTHORIZED, "missing bearer token");
                        }
                        (axum::http::StatusCode::OK, "{}")
                    },
                ),
            )
            .with_state(observed);
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind the stub daemon");
        let url = format!("http://{}", listener.local_addr().expect("stub address"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        url
    }

    /// THE PACKET'S WHOLE POINT, asserted on the WIRE. Before this,
    /// `grep -rn "Bearer\|Authorization" chief-cli/src` returned nothing and
    /// every operator command reached chiefd anonymously.
    #[tokio::test]
    async fn an_operator_client_presents_a_bearer_to_a_company_daemon() {
        let root = tempfile::tempdir().expect("tempdir");
        staged_operator_key(root.path());
        let observed = std::sync::Arc::new(Observed::default());
        let url = stub_daemon(std::sync::Arc::clone(&observed)).await;

        let client = Client::operator(root.path());
        let answer = client
            .post_json(
                &format!("{url}/v1/org/roster/desired"),
                &serde_json::json!({}),
                std::time::Duration::from_secs(5),
            )
            .await
            .expect("the stub answers");

        assert_eq!(answer.status, 200);
        let headers = observed.headers.lock().expect("headers").clone();
        assert_eq!(headers, vec![Some("Bearer jwt-1".to_string())]);
    }

    /// beacond and `chief host` have no auth runtime at all, so a challenge
    /// against either is a 404 on every single call. Their client must stay
    /// bare — a credential attached there is pure cost and a misleading log.
    #[tokio::test]
    async fn an_unauthenticated_client_sends_no_authorization_header() {
        let observed = std::sync::Arc::new(Observed::default());
        let url = stub_daemon(std::sync::Arc::clone(&observed)).await;

        Client::new()
            .post_json(
                &format!("{url}/v1/org/roster/desired"),
                &serde_json::json!({}),
                std::time::Duration::from_secs(5),
            )
            .await
            .expect("the stub answers");

        assert_eq!(observed.headers.lock().expect("headers").clone(), vec![None]);
        assert_eq!(observed.minted.load(Ordering::SeqCst), 0, "no token was ever asked for");
    }

    /// A 401 re-acquires ONCE. The daemon's HS256 secret is ephemeral unless a
    /// secret file was provisioned, so a restart invalidates every cached
    /// bearer at once — and before this the client cached its token for the
    /// life of the process and every later call was refused.
    #[tokio::test]
    async fn a_refused_bearer_is_re_acquired_once_and_the_request_retried() {
        let root = tempfile::tempdir().expect("tempdir");
        staged_operator_key(root.path());
        let observed = std::sync::Arc::new(Observed::default());
        observed.refusals.store(1, Ordering::SeqCst);
        let url = stub_daemon(std::sync::Arc::clone(&observed)).await;

        let answer = Client::operator(root.path())
            .post_json(
                &format!("{url}/v1/org/roster/desired"),
                &serde_json::json!({}),
                std::time::Duration::from_secs(5),
            )
            .await
            .expect("the stub answers");

        assert_eq!(answer.status, 200, "the retry carried a fresh token and was accepted");
        assert_eq!(
            observed.headers.lock().expect("headers").clone(),
            vec![Some("Bearer jwt-1".to_string()), Some("Bearer jwt-2".to_string())],
            "the second attempt must carry a DIFFERENT token, or the retry is pointless"
        );
    }

    /// ONE retry, never a ladder. A genuinely unauthorized identity must fail
    /// fast rather than loop against the challenge endpoint.
    #[tokio::test]
    async fn a_persistent_401_is_retried_exactly_once() {
        let root = tempfile::tempdir().expect("tempdir");
        staged_operator_key(root.path());
        let observed = std::sync::Arc::new(Observed::default());
        observed.refusals.store(99, Ordering::SeqCst);
        let url = stub_daemon(std::sync::Arc::clone(&observed)).await;

        let answer = Client::operator(root.path())
            .post_json(
                &format!("{url}/v1/org/roster/desired"),
                &serde_json::json!({}),
                std::time::Duration::from_secs(5),
            )
            .await
            .expect("the stub answers");

        assert_eq!(answer.status, 401, "the second refusal is returned, not retried again");
        assert_eq!(observed.headers.lock().expect("headers").len(), 2);
        assert_eq!(observed.minted.load(Ordering::SeqCst), 2);
    }

    /// An ABSENT key still lets the request go out. The daemon mints
    /// `<data-root>/keys/operator.key` at boot, so a box whose daemon has never
    /// run legitimately has none — and `chief` reaches beacond and its own
    /// loopback listener before any daemon exists. Refusing here would turn a
    /// state the product passes through into an outage of every command.
    #[tokio::test]
    async fn an_absent_key_sends_the_request_unauthenticated_rather_than_refusing() {
        let root = tempfile::tempdir().expect("tempdir");
        // Deliberately NOT staged: this is a box whose daemon has never booted.
        let observed = std::sync::Arc::new(Observed::default());
        let url = stub_daemon(std::sync::Arc::clone(&observed)).await;

        let answer = Client::operator(root.path())
            .post_json(
                &format!("{url}/v1/org/roster/desired"),
                &serde_json::json!({}),
                std::time::Duration::from_secs(5),
            )
            .await
            .expect("the request still goes out");

        assert_eq!(answer.status, 200);
        assert_eq!(observed.headers.lock().expect("headers").clone(), vec![None]);
    }

    /// A GROUP-READABLE key is the other case, and it is a HARD refusal: the
    /// request is never dialled.
    ///
    /// Ruling 1 says a key that widened after it was written must stop the
    /// caller rather than warn it, and A1 gave the daemon exactly that rule
    /// over exactly this file. A client that sent the request anyway would make
    /// `chmod g+r` a silent downgrade to anonymous — a fifth off switch in the
    /// packet that deletes the other four — and once the universal gate lands
    /// the only thing an operator would see is `401 missing bearer token`,
    /// which names neither the file nor the mode.
    #[tokio::test]
    async fn a_group_readable_key_stops_the_request_before_it_is_dialled() {
        let root = tempfile::tempdir().expect("tempdir");
        staged_operator_key(root.path());
        let key = identity_keys::operator_key_path(&super::super::paths::keys_dir(root.path()));
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let observed = std::sync::Arc::new(Observed::default());
        let url = stub_daemon(std::sync::Arc::clone(&observed)).await;

        let refusal = Client::operator(root.path())
            .post_json(
                &format!("{url}/v1/org/roster/desired"),
                &serde_json::json!({}),
                std::time::Duration::from_secs(5),
            )
            .await
            .expect_err("a key others can read must stop the call");

        let message = refusal.to_string();
        assert!(message.contains("operator.key"), "{message}");
        assert!(message.contains("0644"), "{message}");
        assert!(message.contains("chmod 600"), "{message}");
        assert!(
            observed.headers.lock().expect("headers").is_empty(),
            "the daemon was never called at all"
        );
        assert_eq!(observed.minted.load(Ordering::SeqCst), 0, "no nonce was spent");
    }

    #[test]
    fn a_base_url_never_doubles_its_separator() {
        assert_eq!(base("http://127.0.0.1:8791"), "http://127.0.0.1:8791");
        assert_eq!(base("http://127.0.0.1:8791/"), "http://127.0.0.1:8791");
        assert_eq!(base("http://127.0.0.1:8791///"), "http://127.0.0.1:8791");
    }

    #[test]
    fn a_malformed_body_is_absent_json_rather_than_an_error() {
        // Load-bearing: an endpoint that answered garbage is still an endpoint
        // that answered, and the health probe's `reachable` verdict depends on
        // that staying distinguishable from `unreachable`.
        let answer = Answer { status: 200, body: "not json".to_string() };
        assert!(answer.json().is_none());
        let good = Answer { status: 200, body: r#"{"status":"ok"}"#.to_string() };
        assert_eq!(
            good.json().and_then(|v| v["status"].as_str().map(str::to_string)),
            Some("ok".to_string())
        );
    }
}
