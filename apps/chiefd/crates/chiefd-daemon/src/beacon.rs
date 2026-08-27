//! chiefd's half of discovery (E10-S3, #764): it PUBLISHES where it is.
//! Nothing here READS the registry — a daemon does not need to discover
//! itself, and every consumer-side lookup lives in `packages/chiefing`.
//!
//! Three calls, three distinct jobs, no overlap:
//!   - [`Beacon::register`] — once, immediately after bind. The ONE
//!     conditional `UPDATE` of the company's location columns that is
//!     simultaneously this daemon's single-writer ADMISSION and its
//!     address publication. `Admitted` continues booting; `Occupied`/
//!     `UnknownCompany` are LOUD refusals (see `run.rs`).
//!   - [`Beacon::heartbeat`] — on the liveness tick chiefd ALREADY emits
//!     (`chiefd_log::heartbeat_interval()`); refreshes
//!     `lastSeenAt` only. Not fatal on failure.
//!   - [`Beacon::deregister`] — once, on graceful shutdown. Clears the
//!     location; does NOT delete the company.
//!
//! None of the three is a fallback or a retry for another, and none has a
//! conditional arm that only runs when something is broken: no re-register
//! (ruling D22/F13 — a 404 at heartbeat time means the company was
//! deleted, which is an operator's business, not something a daemon
//! repairs by recreating state), no claim/token/re-read-and-verify
//! protocol (that WAS the deleted owner marker's job; beacond's single
//! `UPDATE` either applies or it does not), no lock, no sweeper, no TTL.
//!
//! # What beacond is FOR now
//!
//! Not discovery. A command finds its own directory's daemon by reading
//! `<dir>/.chief/run/daemon.json` (`crate::rendezvous`), so no registry sits
//! between a command and its own company. What survives here is the
//! box-wide presence registry — "what is running anywhere on this machine",
//! for `chief ls` and the web app — and this daemon's single-writer
//! ADMISSION, which is the one conditional `UPDATE` that says nobody else
//! is already serving this directory.
//!
//! Every call is keyed by the canonical company DIRECTORY. It was the slug,
//! which was never unique: two data roots could each hold an `acme`, and
//! beacond had one row for both.
//!
//! # What this replaces, and the one thing it costs
//!
//! The per-company owner marker file (formerly
//! `chiefd_host::run_admission`) is DELETED by this
//! story, not left dormant — it was a lock file (ruling D19) and
//! unsanctioned disk state (ruling D20), and it held a second copy of the
//! `pid`/`host`/liveness fact beacond's `register` now owns as the single
//! authority.
//!
//! Cost, stated rather than hidden: the marker matched `/proc/<pid>/stat`
//! start times to defeat pid recycling. beacond has no start-time column
//! (ruling D23 fixes the columns beacond keeps), so a recycled pid on the
//! SAME host can make a dead location read live and refuse a legitimate
//! boot. The window is one boot wide, the failure mode is a LOUD refusal
//! (never a silent second writer — the same fail-safe direction the
//! marker's own design chose), and the remedy is one `deregister` call
//! against the stale row. This is not "fixed" by adding a file back.

use std::time::Duration;

use chiefd_api::docstore::LivenessSink;
use http_body_util::BodyExt as _;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;

/// Every request gets this budget. No retry ladder anywhere in this module
/// — one request, one answer, one caller-visible outcome.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// `deregister`'s own tighter budget: a wedged beacond must not hold up
/// graceful shutdown.
const DEREGISTER_TIMEOUT: Duration = Duration::from_millis(500);

type Client = HyperClient<HttpConnector, http_body_util::Full<hyper::body::Bytes>>;

/// chiefd's half of discovery. See the module doc for the three calls this
/// makes and why each is exactly one thing.
pub(crate) struct Beacon {
    base_url: String,
    /// The company DIRECTORY, canonical and absolute — beacond's key.
    ///
    /// It was the slug, which was never unique: one slug under two data roots
    /// was two companies, and beacond could not tell them apart. A directory is
    /// unique by construction, so the registry needs no composite and no
    /// second opinion about which company a row belongs to.
    dir: String,
    pid: i64,
    hostname: String,
    client: Client,
}

/// What beacond decided about this daemon's `register` claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Admission {
    /// The location columns now name us: we own this company.
    Admitted,
    /// beacond has no row for this directory. The company does not exist —
    /// `chiefd run` must not create one by binding.
    UnknownCompany,
    /// A verifiably live pid already serves this company. The fields are
    /// the incumbent's, so the refusal can be LOUD.
    Occupied { pid: i64, hostname: Option<String>, last_seen_at: Option<String> },
}

/// Why a beacon call failed. Every variant is a transport/protocol failure;
/// beacond's own DOMAIN answers (unknown company, occupied, pid mismatch)
/// are decoded into [`Admission`] / a bare `bool`, never this type — this
/// is only "the call itself did not complete as expected."
#[derive(Debug, thiserror::Error)]
pub(crate) enum BeaconError {
    /// The request could not be sent, timed out, or the connection failed.
    #[error("could not reach beacond at {url}: {source}")]
    Transport {
        url: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// beacond answered, but not with a status this call recognizes.
    #[error("beacond at {url} answered {status} for {path}: {body}")]
    UnexpectedStatus { url: String, path: &'static str, status: u16, body: String },
    /// beacond's 2xx body could not be parsed as the expected shape.
    #[error("beacond at {url} sent an unparseable body for {path}: {source}")]
    MalformedBody {
        url: String,
        path: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Deserialize)]
struct CreateCompanyBody {
    created: bool,
}

#[derive(Debug, Deserialize)]
struct RegisterOccupiedBody {
    pid: i64,
    hostname: Option<String>,
    #[serde(rename = "lastSeenAt")]
    last_seen_at: Option<String>,
}

impl Beacon {
    /// `BEACOND_URL` or beacond's own compiled-in default.
    ///
    /// Read from [`beacond::config`] rather than restated here. This module
    /// and `lifecycle::discovery` each used to carry a private
    /// `DEFAULT_BEACOND_URL` literal — two copies of the discovery port in one
    /// crate, plus beacond's own, and the message in
    /// `chief-cli/src/discovery.rs`'s `unreachable_beacond_detail` is what a
    /// copy that predates a port move actually costs an operator. That module
    /// is flat in `chief-cli/src/`; the `lifecycle::discovery` path this
    /// comment used to name has not existed since the module was flattened.
    pub(crate) fn from_env(dir: &std::path::Path) -> Self {
        let base_url =
            std::env::var("BEACOND_URL").unwrap_or_else(|_| beacond::config::default_url());
        Self {
            base_url,
            dir: dir.to_string_lossy().into_owned(),
            pid: std::process::id() as i64,
            hostname: hostname(),
            client: HyperClient::builder(TokioExecutor::new()).build_http(),
        }
    }

    /// The configured beacond base URL, for complete refusal diagnostics.
    #[must_use]
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn post(
        &self,
        path: &'static str,
        body: serde_json::Value,
    ) -> Result<(u16, String), BeaconError> {
        let started = std::time::Instant::now();
        let outcome = self.post_once(path, body).await;
        // Register, self-registration, heartbeat and deregister are the only
        // calls this daemon makes to beacond, and the first of them is what a
        // launching client is blocked waiting for. Timing each one is what
        // makes "the wait was on beacond" provable rather than suspected.
        match &outcome {
            Ok((status, _body)) => tracing::debug!(
                event = "beacond.call",
                dir = %self.dir,
                path,
                status = *status,
                elapsed_ms = chiefd_log::elapsed_ms(started),
                "a beacond call answered"
            ),
            Err(error) => tracing::warn!(
                event = "beacond.call.failed",
                dir = %self.dir,
                path,
                elapsed_ms = chiefd_log::elapsed_ms(started),
                reason = %error,
                "a beacond call did not answer"
            ),
        }
        outcome
    }

    async fn post_once(
        &self,
        path: &'static str,
        body: serde_json::Value,
    ) -> Result<(u16, String), BeaconError> {
        let url = format!("{}{path}", self.base_url);
        let request = hyper::Request::builder()
            .method("POST")
            .uri(&url)
            .header("connection", "close")
            .header("content-type", "application/json")
            .body(http_body_util::Full::new(hyper::body::Bytes::from(body.to_string())))
            .map_err(|source| BeaconError::Transport {
                url: url.clone(),
                source: Box::new(source),
            })?;
        let response = tokio::time::timeout(REQUEST_TIMEOUT, self.client.request(request))
            .await
            .map_err(|_elapsed| BeaconError::Transport {
                url: url.clone(),
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("no response within {REQUEST_TIMEOUT:?}"),
                )),
            })?
            .map_err(|source| BeaconError::Transport {
                url: url.clone(),
                source: Box::new(source),
            })?;
        let status = response.status().as_u16();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|source| BeaconError::Transport {
                url: url.clone(),
                source: Box::new(source),
            })?
            .to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok((status, text))
    }

    /// `POST /v1/register` — the ONE claim-and-publish call, issued exactly
    /// once per daemon, immediately after bind and before any write to the
    /// company database (see `run.rs`'s ordering comment for why register
    /// precedes `ensure_schema`).
    ///
    /// beacond applies this as a single conditional `UPDATE` of an
    /// EXISTING company row's location columns, inside one `BEGIN
    /// IMMEDIATE` transaction. That one write IS this daemon's admission:
    /// winning it means we own the company. It never inserts — a daemon
    /// may not bring a company into existence by binding to a port.
    ///
    /// The body names the canonical `dir`. See this module's doc: beacond's
    /// rows are keyed by directory now, so a same-named company in another
    /// directory is a different row rather than a collision.
    pub(crate) async fn register(&self, url: &str, port: u16) -> Result<Admission, BeaconError> {
        let (status, body) = self
            .post(
                "/v1/register",
                serde_json::json!({
                    "dir": self.dir,
                    "url": url,
                    "port": port,
                    "pid": self.pid,
                    "hostname": self.hostname,
                }),
            )
            .await?;
        match status {
            200 => Ok(Admission::Admitted),
            404 => Ok(Admission::UnknownCompany),
            409 => {
                let occupied: RegisterOccupiedBody =
                    serde_json::from_str(&body).map_err(|source| BeaconError::MalformedBody {
                        url: self.base_url.clone(),
                        path: "/v1/register",
                        source,
                    })?;
                Ok(Admission::Occupied {
                    pid: occupied.pid,
                    hostname: occupied.hostname,
                    last_seen_at: occupied.last_seen_at,
                })
            }
            other => Err(BeaconError::UnexpectedStatus {
                url: self.base_url.clone(),
                path: "/v1/register",
                status: other,
                body,
            }),
        }
    }

    /// `POST /v1/company/create` — self-registration, at BOOT only.
    ///
    /// Operator ruling, 2026-08-26: *"chiefd should always try to register to
    /// beacond when it starts. if it exists, no-op. if it doesn't, register"*
    /// — given after a company's registry row was lost to an external deletion
    /// of `~/.chief` and the daemon's admission refusal left the company
    /// unstartable. beacond's create is an upsert (`ON CONFLICT(dir) DO
    /// UPDATE`), so an existing row is the no-op half and the returned
    /// `created` flag is how the caller knows which half happened.
    ///
    /// THIS DOES NOT REPEAL D22/F13 (see [`Self::heartbeat`], below): a 404
    /// MID-RUN still means the operator deleted the company while we ran, and
    /// the daemon still never repairs that by recreating state. The line
    /// between the two cases is WHO HOLDS PROOF: at boot the caller has just
    /// proved `<dir>/.chief/db/chief.db` exists on disk — the directory is the
    /// company and the database is the proof — while `chief rm` deletes
    /// `<dir>/.chief/` BEFORE the registry row (`remove.rs`), so a removed
    /// company can never pass that proof again. Boot self-registration
    /// restores a registry that lost a row it should have; mid-run
    /// re-registration would resurrect a company the operator removed.
    ///
    /// # Errors
    /// Transport, unexpected status, or an unparseable 200 body.
    pub(crate) async fn create_company(&self, key: &str, slug: &str) -> Result<bool, BeaconError> {
        let (status, body) = self
            .post(
                "/v1/company/create",
                serde_json::json!({ "dir": self.dir, "key": key, "slug": slug }),
            )
            .await?;
        match status {
            200 => {
                let answer: CreateCompanyBody =
                    serde_json::from_str(&body).map_err(|source| BeaconError::MalformedBody {
                        url: self.base_url.clone(),
                        path: "/v1/company/create",
                        source,
                    })?;
                Ok(answer.created)
            }
            other => Err(BeaconError::UnexpectedStatus {
                url: self.base_url.clone(),
                path: "/v1/company/create",
                status: other,
                body,
            }),
        }
    }

    /// `POST /v1/heartbeat` — refresh `lastSeenAt`, pid-fenced, one
    /// beacond transaction. Issued from the liveness tick chiefd already
    /// emits (this type's [`LivenessSink`] impl, below).
    ///
    /// There is deliberately NO "on 404, register again" arm (ruling
    /// D22/F13): a 404 means the company was deleted while we ran, which
    /// is a LOUD warn and an operator's business, not something a daemon
    /// repairs by recreating state.
    pub(crate) async fn heartbeat(&self) -> Result<(), BeaconError> {
        let (status, body) = self
            .post("/v1/heartbeat", serde_json::json!({ "dir": self.dir, "pid": self.pid }))
            .await?;
        match status {
            200 => Ok(()),
            other => Err(BeaconError::UnexpectedStatus {
                url: self.base_url.clone(),
                path: "/v1/heartbeat",
                status: other,
                body,
            }),
        }
    }

    /// `POST /v1/deregister` — one pid-fenced `UPDATE` that CLEARS the
    /// location columns, one beacond transaction. Does NOT delete the
    /// company; stopping a daemon is not deleting a company. Bounded by a
    /// hard 500ms budget so a wedged beacond cannot hold shutdown.
    pub(crate) async fn deregister(&self) -> Result<(), BeaconError> {
        let (status, body) = tokio::time::timeout(
            DEREGISTER_TIMEOUT,
            self.post("/v1/deregister", serde_json::json!({ "dir": self.dir, "pid": self.pid })),
        )
        .await
        .map_err(|_elapsed| BeaconError::Transport {
            url: self.base_url.clone(),
            source: Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("no response within {DEREGISTER_TIMEOUT:?}"),
            )),
        })??;
        match status {
            200 => Ok(()),
            other => Err(BeaconError::UnexpectedStatus {
                url: self.base_url.clone(),
                path: "/v1/deregister",
                status: other,
                body,
            }),
        }
    }
}

/// This machine's name, as this daemon REPORTS it in a registration.
///
/// One line, delegating to [`beacond::liveness::hostname`], and that is the
/// whole point: the operator client compares a registration against the same
/// function, and the two programs no longer share a crate to keep them
/// honest. A local re-implementation here — even a correct-looking one — is
/// how every registration a Mac wrote came to read as foreign.
pub(crate) fn hostname() -> String {
    beacond::liveness::hostname()
}

/// The [`LivenessSink`] chiefd run attaches to the docstore mount's
/// existing heartbeat tick (E10-S3, #764) — no new timer.
///
/// `on_liveness_tick` must not block (the tick loop it is called from must
/// keep ticking on schedule), so this fires the heartbeat request onto the
/// current runtime and does not await it. A failed heartbeat is `tracing
/// ::warn!` (`beacond_heartbeat_failed`), never fatal: nothing about
/// ownership changed, only `lastSeenAt` is stale, and the next tick
/// refreshes it. No retry, no backoff, no state remembering a failed tick.
pub(crate) struct HeartbeatSink {
    beacon: std::sync::Arc<Beacon>,
}

impl HeartbeatSink {
    pub(crate) fn new(beacon: std::sync::Arc<Beacon>) -> Self {
        Self { beacon }
    }
}

impl LivenessSink for HeartbeatSink {
    fn on_liveness_tick(&self) {
        let beacon = std::sync::Arc::clone(&self.beacon);
        tokio::spawn(async move {
            if let Err(error) = beacon.heartbeat().await {
                tracing::warn!(
                    dir = %beacon.dir,
                    %error,
                    "chiefd run: beacond_heartbeat_failed — lastSeenAt is stale until the next tick"
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use axum::extract::{Json, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::Router;
    use tokio::sync::Mutex;

    use super::*;

    /// One captured request: method-implicit (every route here is POST),
    /// path, and the decoded JSON body.
    #[derive(Debug, Clone)]
    struct Captured {
        path: &'static str,
        body: serde_json::Value,
    }

    #[derive(Clone, Default)]
    struct FakeBeacond {
        captured: std::sync::Arc<Mutex<Vec<Captured>>>,
        /// What `/v1/register` answers next: `None` = 200 admitted.
        register_answer: std::sync::Arc<Mutex<Option<(StatusCode, serde_json::Value)>>>,
        /// What `/v1/heartbeat` answers next: `None` = 200 ok.
        heartbeat_answer: std::sync::Arc<Mutex<Option<StatusCode>>>,
    }

    async fn fake_register(
        State(state): State<FakeBeacond>,
        Json(body): Json<serde_json::Value>,
    ) -> Response {
        state.captured.lock().await.push(Captured { path: "/v1/register", body });
        match state.register_answer.lock().await.take() {
            Some((status, body)) => (status, Json(body)).into_response(),
            None => (StatusCode::OK, Json(serde_json::json!({"registered": true}))).into_response(),
        }
    }

    async fn fake_heartbeat(
        State(state): State<FakeBeacond>,
        Json(body): Json<serde_json::Value>,
    ) -> Response {
        state.captured.lock().await.push(Captured { path: "/v1/heartbeat", body });
        match state.heartbeat_answer.lock().await.take() {
            Some(status) => (status, Json(serde_json::json!({}))).into_response(),
            None => (StatusCode::OK, Json(serde_json::json!({"ok": true, "lastSeenAt": "now"})))
                .into_response(),
        }
    }

    async fn fake_deregister(
        State(state): State<FakeBeacond>,
        Json(body): Json<serde_json::Value>,
    ) -> Response {
        state.captured.lock().await.push(Captured { path: "/v1/deregister", body });
        (StatusCode::OK, Json(serde_json::json!({"deregistered": true}))).into_response()
    }

    /// Spin up a real fake beacond on `127.0.0.1:0` and return a [`Beacon`]
    /// pointed at it, plus the shared capture buffer. A real socket, a real
    /// hyper client — the proof is the request the client actually sends,
    /// not a mock of the client's internals.
    async fn fake_beacond() -> (Beacon, FakeBeacond, tokio::task::JoinHandle<()>) {
        let state = FakeBeacond::default();
        let app = Router::new()
            .route("/v1/register", post(fake_register))
            .route("/v1/heartbeat", post(fake_heartbeat))
            .route("/v1/deregister", post(fake_deregister))
            .with_state(state.clone());
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind fake beacond");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fake beacond");
        });
        let beacon = Beacon {
            base_url: format!("http://{addr}"),
            dir: "/work/acme".to_string(),
            pid: 4242,
            hostname: "test-host".to_string(),
            client: HyperClient::builder(TokioExecutor::new()).build_http(),
        };
        (beacon, state, handle)
    }

    // ---- Test 6: register/heartbeat/deregister build the documented method, path, body ----

    #[tokio::test]
    async fn register_posts_the_documented_body_to_the_documented_path() {
        let (beacon, state, _server) = fake_beacond().await;
        let admission = beacon.register("http://127.0.0.1:8793", 8793).await.expect("register");
        assert_eq!(admission, Admission::Admitted);
        let captured = state.captured.lock().await;
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].path, "/v1/register");
        assert_eq!(
            captured[0].body,
            serde_json::json!({
                "dir": "/work/acme",
                "url": "http://127.0.0.1:8793",
                "port": 8793,
                "pid": 4242,
                "hostname": "test-host",
            })
        );
    }

    #[tokio::test]
    async fn heartbeat_posts_the_documented_body_to_the_documented_path() {
        let (beacon, state, _server) = fake_beacond().await;
        beacon.heartbeat().await.expect("heartbeat");
        let captured = state.captured.lock().await;
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].path, "/v1/heartbeat");
        assert_eq!(captured[0].body, serde_json::json!({"dir": "/work/acme", "pid": 4242}));
    }

    #[tokio::test]
    async fn deregister_posts_the_documented_body_to_the_documented_path() {
        let (beacon, state, _server) = fake_beacond().await;
        beacon.deregister().await.expect("deregister");
        let captured = state.captured.lock().await;
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].path, "/v1/deregister");
        assert_eq!(captured[0].body, serde_json::json!({"dir": "/work/acme", "pid": 4242}));
    }

    #[tokio::test]
    async fn the_beacon_requests_exactly_these_four_paths_and_no_others() {
        // A structural assertion, not just four individual ones: grep the
        // PRODUCTION half of this file (everything before `#[cfg(test)]`,
        // so this test's own source -- whose assertion strings themselves
        // contain the search pattern `"/v1/` -- cannot self-match) for
        // every string literal that looks like a route, and assert the set
        // is exactly {company/create, deregister, heartbeat, register}.
        //
        // 3 -> 4, deliberately: `company/create` is boot-only
        // self-registration (the operator's 2026-08-26 repeal -- see
        // `create_company`'s own doc, which also draws the D22/F13 line
        // that keeps it out of mid-run paths). This pin exists so a fifth
        // path is a decision somebody makes here, in review, rather than a
        // call site nobody counted -- it caught the fourth one's author on
        // its first run against the change.
        let source = include_str!("beacon.rs");
        let production =
            source.split("#[cfg(test)]").next().expect("split always yields at least one part");
        let mut paths: Vec<&str> =
            production.split("\"/v1/").skip(1).filter_map(|s| s.split('"').next()).collect();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths, vec!["company/create", "deregister", "heartbeat", "register"]);
    }

    /// THE CASE THE SLUG KEY COULD NOT REPRESENT.
    ///
    /// Two directories may hold companies with the same name. Keyed by slug,
    /// beacond had ONE row for both — so the second daemon read the first as a
    /// live incumbent and refused, or replaced its location and stranded it.
    /// Keyed by directory they are two rows, and this asserts the daemon sends
    /// what distinguishes them.
    #[test]
    fn two_directories_holding_a_company_of_the_same_name_are_two_different_registrations() {
        let here = Beacon::from_env(std::path::Path::new("/work/acme"));
        let there = Beacon::from_env(std::path::Path::new("/elsewhere/acme"));
        assert_eq!(here.dir, "/work/acme");
        assert_ne!(
            here.dir, there.dir,
            "two directories are two companies, whatever they are called"
        );
    }

    // ---- Test 7: register maps beacond's three answers correctly ----

    #[tokio::test]
    async fn register_maps_404_to_unknown_company() {
        let (beacon, state, _server) = fake_beacond().await;
        *state.register_answer.lock().await =
            Some((StatusCode::NOT_FOUND, serde_json::json!({"code": "unknown-company"})));
        let admission = beacon.register("http://x", 1).await.expect("register");
        assert_eq!(admission, Admission::UnknownCompany);
    }

    #[tokio::test]
    async fn register_maps_409_to_occupied_carrying_the_incumbents_identity_for_a_loud_refusal() {
        let (beacon, state, _server) = fake_beacond().await;
        *state.register_answer.lock().await = Some((
            StatusCode::CONFLICT,
            serde_json::json!({
                "code": "company-live-elsewhere",
                "pid": 9999,
                "hostname": "other-host",
                "lastSeenAt": "2026-08-03T00:00:00.000Z",
            }),
        ));
        let admission = beacon.register("http://x", 1).await.expect("register");
        assert_eq!(
            admission,
            Admission::Occupied {
                pid: 9999,
                hostname: Some("other-host".to_string()),
                last_seen_at: Some("2026-08-03T00:00:00.000Z".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn register_maps_200_to_admitted() {
        let (beacon, _state, _server) = fake_beacond().await;
        let admission = beacon.register("http://x", 1).await.expect("register");
        assert_eq!(admission, Admission::Admitted);
    }

    // ---- Test 8: unreachable beacond at register time -> Err, no panic, no block ----

    #[tokio::test]
    async fn an_unreachable_beacond_makes_register_return_err_promptly_never_panic_or_block() {
        let beacon = Beacon {
            base_url: "http://127.0.0.1:1".to_string(), // refused immediately on loopback
            dir: "/work/acme".to_string(),
            pid: 1,
            hostname: "h".to_string(),
            client: HyperClient::builder(TokioExecutor::new()).build_http(),
        };
        let result = tokio::time::timeout(Duration::from_secs(5), beacon.register("http://x", 1))
            .await
            .expect("must not block past the request's own timeout");
        assert!(result.is_err());
    }

    // ---- Test 9: unreachable beacond at heartbeat time -> warn, not fatal ----

    #[tokio::test]
    async fn heartbeat_against_an_unreachable_beacond_returns_err_but_the_sink_never_panics() {
        let beacon = std::sync::Arc::new(Beacon {
            base_url: "http://127.0.0.1:1".to_string(),
            dir: "/work/acme".to_string(),
            pid: 1,
            hostname: "h".to_string(),
            client: HyperClient::builder(TokioExecutor::new()).build_http(),
        });
        // Directly exercising the fallible call `HeartbeatSink` wraps: the
        // sink's own contract (never fatal) is that this Err is downgraded
        // to a warn and swallowed, not propagated -- checked structurally
        // by `on_liveness_tick`'s signature returning `()`, not `Result`.
        assert!(beacon.heartbeat().await.is_err());
        let sink = HeartbeatSink::new(beacon);
        sink.on_liveness_tick(); // must not panic even though it will fail
    }

    #[tokio::test]
    async fn heartbeat_pid_mismatch_is_reported_as_an_unexpected_status_not_a_panic() {
        let (beacon, state, _server) = fake_beacond().await;
        *state.heartbeat_answer.lock().await = Some(StatusCode::CONFLICT);
        let result = beacon.heartbeat().await;
        assert!(matches!(result, Err(BeaconError::UnexpectedStatus { status: 409, .. })));
    }
}
